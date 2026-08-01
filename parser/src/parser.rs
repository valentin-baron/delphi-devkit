//! Grammar parser over [`TokenCursor`]. Slice 1: source headers
//! (unit/program/library/package) and uses/requires/contains clauses.
//! Directive handling, conditional filtering and includes already happened
//! underneath the cursor — this layer sees only significant tokens.

use std::sync::Arc;

use crate::ast::{
    Ancestor, Attribute, ClassType, DeclarationKind, EnumerationMember, GenericParameter, InClause,
    InterfaceDeclaration, InterfaceType, Library, Member, MethodDeclaration, Package, Parameter,
    ParameterModifier, Program, PropertyDeclaration, QualifiedName, RoutineKind, RoutineType,
    Source, StructuredKind, StructuredType, TypeExpression, Unit, UsedUnit, UsesDeclarations,
    VariantArm, VariantPart, Visibility, VisibilitySection,
};
use crate::context::{Identifier, ProjectContext};
use crate::meta::{CodeLocation, FileId, Span};
use crate::source::{FileReadError, SourceArena};
use crate::token::Token;
use crate::token_cursor::{CursorError, Lexeme, TokenCursor};

#[derive(Debug)]
pub enum ParseError {
    FileReadError(FileReadError),
    Cursor(CursorError),
    Unexpected {
        expected: &'static str,
        found: Option<Lexeme>,
    },
    /// Grammar nesting exceeded [`MAX_PARSE_DEPTH`] — pathological input
    /// (`^^^^…T`, deeply nested generics/variant parts). Degrades to an
    /// error instead of overflowing the native stack.
    RecursionLimit,
}

/// Maximum grammar nesting depth for the mutually-recursive type/member/
/// variant descent. Past this the parser returns [`ParseError::RecursionLimit`]
/// rather than risk a native stack overflow (process abort).
///
/// Empirically, pure `^^^…T` recursion overflows a 2 MiB stack (the default
/// for `cargo test` worker threads) at ~125 debug frames; the member/variant
/// paths grow the stack per depth-unit comparably. 64 keeps a ~2x margin
/// under that worst case while dwarfing any real Delphi type nesting (rarely
/// past a dozen), so legitimate source is never rejected.
const MAX_PARSE_DEPTH: usize = 64;

/// Whether the interface loop should keep going or has reached
/// `implementation`. Return value of [`UnitParser::parse_one_interface_item`].
enum InterfaceStep {
    Continue,
    Done,
}

/// Is a parse error UNRECOVERABLE at declaration granularity? A directive-
/// structure error means the conditional-compilation skeleton the cursor relies
/// on is broken (unterminated `{$IFDEF}`, dangling `{$ELSE}`, uses-cycle) — the
/// whole-file token stream is then untrustworthy, so error-tolerant recovery
/// must NOT swallow it; the unit fails as before. A lexer error, an unexpected
/// token, an include failure, or the recursion limit are all local and
/// recoverable (drop the region + resync).
fn is_unrecoverable(error: &ParseError) -> bool {
    matches!(error, ParseError::Cursor(CursorError::Directive(_)))
}

/// The source location an error points at, if it carries one — the diagnostic
/// anchor for recovery. `None` when the error has no intrinsic location (the
/// caller falls back to the cursor's last position).
///
/// Public so the session/LSP layer can anchor a hard-parse-failure squiggle at
/// the actual error site (a precise `Error` diagnostic) instead of always
/// falling back to the top of the document.
pub fn error_location(error: &ParseError) -> Option<CodeLocation> {
    match error {
        ParseError::Cursor(CursorError::Lex(location))
        | ParseError::Cursor(CursorError::Condition { location, .. })
        | ParseError::Cursor(CursorError::Include { location, .. })
        | ParseError::Cursor(CursorError::IncludeDepthExceeded(location)) => Some(*location),
        ParseError::Cursor(CursorError::UnexpectedToken {
            found: Some(lexeme),
            ..
        }) => Some(lexeme.location),
        ParseError::Unexpected {
            found: Some(lexeme),
            ..
        } => Some(lexeme.location),
        _ => None,
    }
}

impl From<FileReadError> for ParseError {
    fn from(value: FileReadError) -> Self {
        ParseError::FileReadError(value)
    }
}

impl From<CursorError> for ParseError {
    fn from(value: CursorError) -> Self {
        ParseError::Cursor(value)
    }
}

/// The parsed AST slot of a [`ParseOutcome`]. For a **unit**, the caching
/// pipeline ([`crate::pipeline::parse_and_cache`]) MOVES the `Unit` out into a
/// durable [`crate::unit_meta::UnitMeta`] and leaves this `Moved` — a unit's
/// authoritative AST then lives in the meta (`meta.ast`), never here. Making
/// the moved-out state a distinct variant (rather than a synthetic placeholder
/// `Source`) means an external caller physically cannot mistake it for real
/// data: it must match `Present(..)` to reach a `Source` at all.
#[derive(Debug)]
pub enum ParsedSource {
    /// The real parsed AST. `parse_file_full` always produces this; the caching
    /// pipeline keeps it for non-unit sources (program/library/package).
    Present(Source),
    /// A unit whose `Unit` AST was moved into its `UnitMeta` by
    /// `parse_and_cache`. Read the AST from the returned meta, not from here.
    Moved,
}

impl ParsedSource {
    /// The parsed `Source`, or `None` if it was moved into a `UnitMeta`.
    pub fn present(&self) -> Option<&Source> {
        match self {
            ParsedSource::Present(source) => Some(source),
            ParsedSource::Moved => None,
        }
    }

    /// Take the `Source` out, leaving `Moved`. Used by the pipeline to move a
    /// `Unit` into its meta.
    pub fn take(&mut self) -> Option<Source> {
        match std::mem::replace(self, ParsedSource::Moved) {
            ParsedSource::Present(source) => Some(source),
            ParsedSource::Moved => None,
        }
    }
}

/// Full result of parsing one file: the AST plus everything the artifact
/// builder needs from the cursor state.
pub struct ParseOutcome {
    /// The parsed AST. For a unit run through `parse_and_cache` this becomes
    /// [`ParsedSource::Moved`] (the `Unit` lives in the returned meta); for
    /// non-unit sources and for a bare `parse_file_full` it is `Present`.
    pub source: ParsedSource,
    pub seen_includes: Vec<FileId>,
    pub dependencies: Vec<crate::unit_cache::Dependency>,
    /// Unresolved identifier occurrences from the implementation section.
    pub usages: Vec<crate::unit_cache::Usage>,
    pub diagnostics: Vec<crate::token_cursor::Diagnostic>,
    /// An interface uses-cycle was hit (invalid Delphi, F2047) — result is
    /// best-effort and must not be persisted as trustworthy.
    pub cycle_tainted: bool,
    /// Error-tolerant recovery dropped at least one broken interface
    /// declaration (a diagnostic marks each). The parse is best-effort: the
    /// surviving declarations are real, but the unit is INCOMPLETE, so — like
    /// `cycle_tainted` — it must not be persisted as a clean interface (the
    /// save path skips it). See [`crate::pipeline::build_unit_meta`].
    pub recovered: bool,
}

/// Parse one source file that is already materialized in the arena.
pub fn parse_file(
    arena: &SourceArena,
    context: Arc<ProjectContext>,
    file: FileId,
) -> Result<Source, ParseError> {
    parse_file_full(arena, context, file, None).map(|outcome| match outcome.source {
        // `parse_file_full` never moves the source, so this is always `Present`.
        ParsedSource::Present(source) => source,
        ParsedSource::Moved => unreachable!("parse_file_full never moves the source"),
    })
}

/// Like [`parse_file`], with cursor-state byproducts, optionally wired to an
/// [`InterfaceLoader`] so `{$IF Declared(...)}` can consult imports.
pub fn parse_file_full(
    arena: &SourceArena,
    context: Arc<ProjectContext>,
    file: FileId,
    loader: Option<std::rc::Rc<dyn crate::parse_state::InterfaceLoader>>,
) -> Result<ParseOutcome, ParseError> {
    let mut cursor = TokenCursor::new(arena, context, file);
    cursor.state_mut().loader = loader;
    let mut parser = UnitParser {
        cursor,
        depth: 0,
        pending_type_eq: false,
        pending_attributes: Vec::new(),
        recovered: false,
        block_nesting: 0,
    };
    let source = parser.parse_source()?;
    let recovered = parser.recovered;
    let (mut state, diagnostics) = parser.cursor.into_parts();
    Ok(ParseOutcome {
        source: ParsedSource::Present(source),
        seen_includes: state.seen_includes().to_vec(),
        dependencies: state.take_dependencies(),
        usages: state.take_usages(),
        diagnostics,
        cycle_tainted: state.is_cycle_tainted(),
        recovered,
    })
}

/// Convenience: load from disk and parse.
pub fn parse_path(
    arena: &SourceArena,
    context: Arc<ProjectContext>,
    path: &str,
) -> Result<Source, ParseError> {
    let file = arena.load(path)?;
    parse_file(arena, context, file)
}

struct UnitParser<'arena> {
    cursor: TokenCursor<'arena>,
    /// Current grammar nesting depth (see [`MAX_PARSE_DEPTH`]).
    depth: usize,
    /// Set when a type-argument list closed on a `>=`-fused token
    /// (`TArray<Byte>=…`): the `=` belongs to the enclosing declaration and
    /// is delivered through [`consume_type_terminating_eq`]. Consumed
    /// immediately by the surrounding declaration, so it never lingers across
    /// unrelated declarations.
    pending_type_eq: bool,
    /// Attributes captured at the interface-section top level (`[Foo] type …`)
    /// before the section keyword was known. The section parser drains this as
    /// the leading attributes of its FIRST declaration (merged ahead of any
    /// inline `[...]`), then it is empty again. Never lingers across sections:
    /// each section takes it immediately.
    pending_attributes: Vec<Attribute>,
    /// Set when error-tolerant recovery dropped a broken interface declaration.
    /// Threaded into [`ParseOutcome::recovered`] — a recovered unit is flagged
    /// and never persisted as a clean interface.
    recovered: bool,
    /// Structural block-body nesting: how many class/record/object/interface
    /// bodies are currently open (their opener consumed, their `end` not yet).
    /// Incremented on entry to [`Self::parse_member_sections`] and decremented
    /// only on its Ok return, so a member-level failure leaves it inflated by
    /// the count of still-open bodies. [`Self::recover_from_interface_error`]
    /// uses it to seed the nesting-aware resync and then resets it to 0.
    block_nesting: usize,
}

impl UnitParser<'_> {
    /// Enter one recursion level, erroring past [`MAX_PARSE_DEPTH`]. Paired
    /// with [`exit_depth`] by the recursive wrappers below so the counter is
    /// balanced on every path, Ok or Err.
    fn enter_depth(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            Err(ParseError::RecursionLimit)
        } else {
            Ok(())
        }
    }

    fn exit_depth(&mut self) {
        self.depth -= 1;
    }

    fn parse_source(&mut self) -> Result<Source, ParseError> {
        let first = self.cursor.advance()?;
        match first.map(|lexeme| lexeme.token) {
            Some(Token::Unit) => self.parse_unit().map(Source::Unit),
            Some(Token::Program) => self.parse_program().map(Source::Program),
            Some(Token::Library) => self.parse_library().map(Source::Library),
            Some(Token::Package) => self.parse_package().map(Source::Package),
            _ => Err(ParseError::Unexpected {
                expected: "'unit', 'program', 'library' or 'package'",
                found: first,
            }),
        }
    }

    // unit Name.Dotted; [deprecated/platform...] interface [uses ...] ...
    // implementation [uses ...] ... end.
    fn parse_unit(&mut self) -> Result<Unit, ParseError> {
        let name = self.qualified_name()?;
        // register on the parse chain's cycle stack — the top-level unit
        // never passes through InterfaceLoader::interface_of
        let loader = self.cursor.state().loader.clone();
        if let Some(loader) = &loader {
            loader.begin_unit(name.key);
        }
        let unit_key = name.key;
        let result = self.parse_unit_body(name);
        // Balance begin_unit on BOTH success and failure. A parse error after
        // the header must not leave the unit key on the chain's active-unit
        // stack — a later request for the same key (e.g. a namespace-resolved
        // name) would otherwise be reported as a false uses-cycle.
        if let Some(loader) = &loader {
            loader.end_unit(unit_key);
        }
        result
    }

    fn parse_unit_body(&mut self, name: QualifiedName) -> Result<Unit, ParseError> {
        self.skip_until_semicolon()?; // portability directives after the name
        self.expect_keyword(Token::Interface, "'interface'")?;

        let interface_uses = self.optional_uses()?;
        let interface_declarations = self.parse_interface_declarations()?;
        let implementation_uses = self.optional_uses()?;
        self.collect_implementation_usages()?;

        Ok(Unit {
            name,
            interface_uses,
            interface_declarations,
            implementation_uses,
        })
    }

    /// Usage-index skeleton: every identifier-like occurrence in the
    /// implementation section, recorded unresolved (folded key + location).
    /// Scope-aware resolution refines this later; until then the index
    /// over-approximates ("candidate usages"), which is the safe direction
    /// for find-references.
    fn collect_implementation_usages(&mut self) -> Result<(), ParseError> {
        while let Some(lexeme) = self.cursor.advance()? {
            if lexeme.token.can_be_identifier() {
                let key = {
                    let text = self.identifier_text(lexeme);
                    self.cursor.state().context.intern_key(text)
                };
                self.cursor.state_mut().record_usage(crate::unit_cache::Usage {
                    symbol: key,
                    location: lexeme.location,
                });
            }
        }
        Ok(())
    }

    // program Name; [uses ...] <block> end.
    fn parse_program(&mut self) -> Result<Program, ParseError> {
        let name = self.qualified_name()?;
        self.skip_until_semicolon()?; // legacy `program P(Input, Output);`
        let uses = self.optional_uses()?;
        self.skip_rest()?;
        Ok(Program { name, uses })
    }

    fn parse_library(&mut self) -> Result<Library, ParseError> {
        let name = self.qualified_name()?;
        self.skip_until_semicolon()?;
        let uses = self.optional_uses()?;
        self.skip_rest()?;
        Ok(Library { name, uses })
    }

    // package Name; [requires a, b;] [contains c in 'c.pas', d;] end.
    fn parse_package(&mut self) -> Result<Package, ParseError> {
        let name = self.qualified_name()?;
        self.expect_keyword(Token::Semicolon, "';'")?;

        let mut requires = Vec::new();
        if self.peek_token()? == Some(Token::Requires) {
            self.cursor.advance()?;
            loop {
                requires.push(self.qualified_name()?);
                let found = self.cursor.advance()?;
                match found.map(|lexeme| lexeme.token) {
                    Some(Token::Comma) => continue,
                    Some(Token::Semicolon) => break,
                    _ => {
                        return Err(ParseError::Unexpected {
                            expected: "',' or ';' in requires clause",
                            found,
                        });
                    }
                }
            }
        }

        let contains = if self.peek_token()? == Some(Token::Contains) {
            Some(self.parse_uses_list(false)?)
        } else {
            None
        };

        self.skip_rest()?;
        Ok(Package {
            name,
            requires,
            contains,
        })
    }

    // ─── Interface section (slice 2: shallow — names, kinds, locations) ──

    /// Consumes everything up to and including `implementation`.
    ///
    /// ERROR-TOLERANT (ledger #10): a `ParseError` raised while parsing ONE
    /// interface item (a malformed declaration, a lexer `Token::Error` in an
    /// active region, an unexpected token) does NOT abort the whole unit. The
    /// error is recorded as a diagnostic, the parser resyncs to the next
    /// declaration boundary (the next top-level section keyword /
    /// `implementation` / `end` / EOF at the interface nesting level), and the
    /// loop continues — the already-parsed declarations still populate the
    /// interface, the broken region contributes a diagnostic and NEVER a bogus
    /// symbol. A recovered parse sets [`Self::recovered`], so the unit is
    /// flagged and never persisted as a clean interface.
    ///
    /// A DIRECTIVE-structure error (unterminated `{$IFDEF}`, dangling `{$ELSE}`)
    /// is NOT recovered from here: it means the conditional skeleton the cursor
    /// relies on is broken, so the whole-file result is untrustworthy — it
    /// propagates as before (the unit fails, as it must).
    fn parse_interface_declarations(
        &mut self,
    ) -> Result<Vec<InterfaceDeclaration>, ParseError> {
        let mut declarations = Vec::new();
        loop {
            match self.parse_one_interface_item(&mut declarations) {
                Ok(InterfaceStep::Continue) => {}
                Ok(InterfaceStep::Done) => return Ok(declarations),
                Err(error) => {
                    // A directive-structure error is unrecoverable (the
                    // conditional skeleton is broken) — propagate. Everything
                    // else (malformed decl, active-region lexer error,
                    // unexpected token, recursion limit) is recovered: drop the
                    // broken region with a diagnostic and resync.
                    if is_unrecoverable(&error) {
                        return Err(error);
                    }
                    self.recover_from_interface_error(error)?;
                }
            }
        }
    }

    /// Parse ONE interface item, mutating `declarations`. Returns whether the
    /// interface loop should continue or is done (`implementation` reached).
    /// Isolated so [`Self::parse_interface_declarations`] can catch a per-item
    /// error and resync without unwinding the whole unit.
    fn parse_one_interface_item(
        &mut self,
        declarations: &mut Vec<InterfaceDeclaration>,
    ) -> Result<InterfaceStep, ParseError> {
        match self.peek_token()? {
            Some(Token::Implementation) => {
                self.cursor.advance()?;
                self.report_dropped_pending_attributes();
                Ok(InterfaceStep::Done)
            }
            Some(Token::Type) => {
                self.cursor.advance()?;
                self.parse_type_section(declarations)?;
                Ok(InterfaceStep::Continue)
            }
            Some(Token::Const) => {
                self.cursor.advance()?;
                self.parse_constant_section(DeclarationKind::Const, declarations)?;
                Ok(InterfaceStep::Continue)
            }
            Some(Token::ResourceString) => {
                self.cursor.advance()?;
                self.parse_constant_section(DeclarationKind::ResourceString, declarations)?;
                Ok(InterfaceStep::Continue)
            }
            Some(Token::Var) => {
                self.cursor.advance()?;
                self.parse_variable_section(DeclarationKind::Var, declarations)?;
                Ok(InterfaceStep::Continue)
            }
            Some(Token::ThreadVar) => {
                self.cursor.advance()?;
                self.parse_variable_section(DeclarationKind::ThreadVar, declarations)?;
                Ok(InterfaceStep::Continue)
            }
            Some(Token::Procedure) => {
                self.cursor.advance()?;
                declarations.push(self.parse_routine_header(DeclarationKind::Procedure)?);
                Ok(InterfaceStep::Continue)
            }
            Some(Token::Function) => {
                self.cursor.advance()?;
                declarations.push(self.parse_routine_header(DeclarationKind::Function)?);
                Ok(InterfaceStep::Continue)
            }
            // attribute(s) preceding the following section's first declaration
            // (`[Foo] type TBar = …`). Captured here, drained by the section
            // parser as leading attributes (ledger #16).
            Some(Token::LBracket) => {
                let mut captured = self.parse_attributes()?;
                self.pending_attributes.append(&mut captured);
                Ok(InterfaceStep::Continue)
            }
            Some(Token::Exports) => {
                self.cursor.advance()?;
                self.skip_declaration_tail(false)?;
                Ok(InterfaceStep::Continue)
            }
            _ => {
                let found = self.cursor.peek()?;
                Err(ParseError::Unexpected {
                    expected: "interface declaration or 'implementation'",
                    found,
                })
            }
        }
    }

    /// Recover from a per-item interface error: record a diagnostic at the
    /// error site, mark the parse recovered, and resync to the next declaration
    /// boundary. GUARANTEES TERMINATION: [`Self::resync_to_declaration_boundary`]
    /// advances at least one token (or reaches EOF), so a pathological input
    /// cannot loop the interface parser forever. Any leftover pending
    /// attributes from the broken region are cleared (they attached to nothing).
    fn recover_from_interface_error(&mut self, error: ParseError) -> Result<(), ParseError> {
        self.recovered = true;
        self.pending_attributes.clear();
        // Reset the grammar-depth counter to the interface baseline (0). A
        // `RecursionLimit` error unwinds through the `parse_type_expression`/
        // `parse_member_sections` wrappers WITHOUT running their `exit_depth`
        // (the `?` short-circuits past it), so `self.depth` is left inflated.
        // Without this reset the very next declaration's `enter_depth` would
        // instantly re-trip the limit and recover everything away. The
        // interface loop always runs at depth 0.
        self.depth = 0;
        self.pending_type_eq = false;
        // How many class/record/object/interface bodies the failure left open.
        // The resync must climb OUT of these before a section keyword can count
        // as a top-level boundary (else a member starter mints a phantom).
        let open_blocks = self.block_nesting;
        self.block_nesting = 0;
        let location = error_location(&error).unwrap_or_else(|| self.cursor.last_location());
        self.cursor.push_diagnostic(
            location,
            // Error-tolerant recovery DROPPED a declaration — the interface is
            // incomplete (missing symbol feeds wrong go-to-def/completion), a
            // real Warning the user should see, not a hint.
            crate::token_cursor::Severity::Warning,
            format!("interface declaration dropped by error recovery: {error:?}"),
        );
        self.resync_to_declaration_boundary(open_blocks)
    }

    /// Skip tokens until the next interface-level declaration boundary — a
    /// top-level section keyword, `implementation`, `end`, or EOF — leaving that
    /// boundary UNCONSUMED so the interface loop re-dispatches on it. Advances
    /// AT LEAST ONE token before checking (so an error at the current token
    /// cannot spin in place), and tolerates active-region lexer errors by
    /// skipping the offending token (the lexer has already moved past it). This
    /// is the termination guard for declaration-level recovery.
    ///
    /// NESTING-AWARE (the North Star guard). When the failure happened INSIDE a
    /// type body — a broken class/record MEMBER (`Field Integer` with no `:`) —
    /// the error unwinds to the interface loop with the cursor still inside the
    /// (already-opened, still-unbalanced) class/record. A section keyword is
    /// also a MEMBER-starter (`procedure`/`function`/`type`/`const`/`var`/…), so
    /// a naive scan would stop at the FIRST such keyword and let the interface
    /// loop re-dispatch it as a bogus TOP-LEVEL declaration — minting a phantom
    /// symbol that feeds wrong go-to-def/completion. To prevent that, we balance
    /// `class`/`record`/`object`/`interface`/`dispinterface` blocks and
    /// `(`/`[` groups as we scan, and only treat a section keyword (or
    /// `implementation`) as a top-level boundary when ALL of those depths are 0
    /// — i.e. AFTER the broken type's closing `end;`. Everything encountered
    /// while still inside an unbalanced block is part of the broken region and
    /// is discarded, never re-dispatched.
    fn resync_to_declaration_boundary(&mut self, open_blocks: usize) -> Result<(), ParseError> {
        // Block/group nesting of the broken region, relative to the interface
        // top level. `open_blocks` seeds how many class/record/object/interface
        // bodies were still open when the failure unwound to the interface loop
        // (their openers were consumed before the error, so the scan cannot
        // re-derive them). Each balancing `end` we meet closes one; only once
        // ALL block/paren/bracket depths reach 0 does a section keyword become a
        // real top-level boundary. Additional openers we encounter DURING the
        // scan (nested types in the broken region) are counted on top.
        //
        // Note: a failure BEFORE `parse_member_sections` opens the body (e.g. a
        // malformed top-level header) unwinds with `open_blocks == 0` even though
        // some tokens — `(`/`[` or even a `class`/`record` word — may sit BEHIND
        // the cursor. That is benign: openers behind the cursor cannot desync a
        // forward scan (we only balance what we meet from here on), so seeding
        // `block_depth = 0` is correct and the scan still terminates at the next
        // genuine top-level boundary or EOF.
        let mut block_depth = open_blocks;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        // `previous` seeds the class/record block-open disambiguation the same
        // way `skip_declaration_tail` does. We start mid-body, so seed it as if
        // just after a `;` — NOT a type header (`None`/`=`): that keeps a bare
        // member marker such as `class procedure` from being miscounted as a
        // fresh block open (the header path only fires on `None`/`=`).
        let mut previous: Option<Token> = Some(Token::Semicolon);

        // Advance past the token the error was raised on (progress guarantee).
        // A lexer error here is swallowed — the lexer already consumed the bad
        // token, and we are discarding this region anyway.
        match self.cursor.advance() {
            Ok(_) => {}
            Err(CursorError::Lex(_)) => {}
            Err(other) => return Err(other.into()),
        }
        loop {
            let peeked = match self.cursor.peek() {
                Ok(peeked) => peeked,
                // A lexer error at the boundary scan: skip the bad token and
                // keep scanning (the lexer advanced past it on the failed pull).
                Err(CursorError::Lex(_)) => {
                    match self.cursor.advance() {
                        Ok(_) => continue,
                        Err(CursorError::Lex(_)) => continue,
                        Err(other) => return Err(other.into()),
                    }
                }
                // A directive-structure error mid-resync is unrecoverable.
                Err(other) => return Err(other.into()),
            };
            let at_top_level = block_depth == 0 && paren_depth == 0 && bracket_depth == 0;
            match peeked.map(|lexeme| lexeme.token) {
                // Boundaries — but ONLY at interface nesting depth 0. Inside an
                // unbalanced block these are member starters that belong to the
                // broken region; fall through to the discard arm below.
                Some(
                    Token::Type
                    | Token::Const
                    | Token::ResourceString
                    | Token::Var
                    | Token::ThreadVar
                    | Token::Procedure
                    | Token::Function
                    | Token::Exports
                    | Token::Implementation,
                ) if at_top_level => return Ok(()),
                // EOF always stops (no more tokens to balance against).
                None => return Ok(()),
                // A stray `end` at interface depth closes nothing we track; stop
                // there so the caller surfaces it (or reaches EOF). Leaving it
                // unconsumed avoids eating a real `implementation`/EOF past it.
                Some(Token::End) if at_top_level => return Ok(()),
                // A balancing `end` closes the enclosing broken block: consume it
                // and an optional trailing `;`, dropping back toward top level.
                // ONLY when a tracked block is actually open (`block_depth > 0`):
                // an `end` reached with `block_depth == 0` but `!at_top_level`
                // sits inside a resync-tracked `(...)`/`[...]` group of the broken
                // region — it closes no block and must fall through to the discard
                // arm, never decrement (an unguarded `0 - 1` underflows: panic in
                // debug/tests, wrap-to-usize::MAX in release running resync to EOF
                // and dropping every following top-level declaration, ledger #10).
                Some(Token::End) if block_depth > 0 => {
                    block_depth -= 1;
                    match self.cursor.advance() {
                        Ok(_) => {}
                        Err(CursorError::Lex(_)) => {}
                        Err(other) => return Err(other.into()),
                    }
                    if block_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                        // consume the `;` that terminates the type declaration
                        if self.cursor.peek().ok().flatten().map(|lexeme| lexeme.token)
                            == Some(Token::Semicolon)
                        {
                            let _ = self.cursor.advance();
                        }
                    }
                    previous = Some(Token::End);
                    continue;
                }
                // Block/group openers: track them so a member starter inside the
                // broken region is not mistaken for a top-level boundary. The
                // block-open disambiguation helpers inspect the token AFTER the
                // opener, so — like `skip_declaration_tail` — we CONSUME the
                // opener first and only then decide whether it opened a block.
                Some(Token::Class) => {
                    self.discard_current_token()?;
                    if self.class_opens_block(previous).unwrap_or(false) {
                        block_depth += 1;
                    }
                    previous = Some(Token::Class);
                    continue;
                }
                Some(Token::Record) => {
                    self.discard_current_token()?;
                    if self.record_opens_block().unwrap_or(false) {
                        block_depth += 1;
                    }
                    previous = Some(Token::Record);
                    continue;
                }
                Some(Token::Object) => {
                    // `procedure(...) of object` is a type suffix, not a block.
                    if previous != Some(Token::Of) {
                        block_depth += 1;
                    }
                    self.discard_current_token()?;
                    previous = Some(Token::Object);
                    continue;
                }
                Some(token @ (Token::Interface | Token::DispInterface)) => {
                    self.discard_current_token()?;
                    // forward declaration `IFoo = interface;` opens nothing
                    if self.peek_token().ok().flatten() != Some(Token::Semicolon) {
                        block_depth += 1;
                    }
                    previous = Some(token);
                    continue;
                }
                Some(Token::LParen) => {
                    paren_depth += 1;
                    self.discard_current_token()?;
                    previous = Some(Token::LParen);
                    continue;
                }
                Some(Token::RParen) => {
                    paren_depth = paren_depth.saturating_sub(1);
                    self.discard_current_token()?;
                    previous = Some(Token::RParen);
                    continue;
                }
                Some(Token::LBracket) => {
                    bracket_depth += 1;
                    self.discard_current_token()?;
                    previous = Some(Token::LBracket);
                    continue;
                }
                Some(Token::RBracket) => {
                    bracket_depth = bracket_depth.saturating_sub(1);
                    self.discard_current_token()?;
                    previous = Some(Token::RBracket);
                    continue;
                }
                // Anything else (including section keywords while still nested)
                // is part of the broken region — discard it.
                Some(other) => {
                    self.discard_current_token()?;
                    previous = Some(other);
                }
            }
        }
    }

    /// Consume the current token during error recovery, swallowing an
    /// active-region lexer error (the region is being dropped anyway).
    fn discard_current_token(&mut self) -> Result<(), ParseError> {
        match self.cursor.advance() {
            Ok(_) => Ok(()),
            Err(CursorError::Lex(_)) => Ok(()),
            Err(other) => Err(other.into()),
        }
    }

    // type: Name<generics?> = <body>; — repeated while an identifier follows
    fn parse_type_section(
        &mut self,
        declarations: &mut Vec<InterfaceDeclaration>,
    ) -> Result<(), ParseError> {
        loop {
            let attributes = self.take_leading_attributes()?;
            match self.peek_token()? {
                Some(token) if token.can_be_identifier() => {}
                // Next token starts a new section or `implementation`: any
                // attributes we just took belong to whatever follows, not to a
                // declaration here. Return them to the pending buffer so the
                // interface loop attaches them to the next section's first
                // declaration, or reports them dropped at `implementation`
                // (ledger #32) — never a silent loss.
                _ => return self.restore_pending_attributes(attributes),
            }
            let name = self.simple_name()?;
            let (generic_parameters, equal_already_consumed) =
                self.parse_generic_parameters()?;
            if !equal_already_consumed {
                self.expect_keyword(Token::Eq, "'='")?;
            }
            let type_expression = self.parse_type_expression()?;
            self.expect_keyword(Token::Semicolon, "';'")?;
            // Record own symbols BEFORE consuming trailing directives: the
            // cursor evaluates a following `{$IF Declared(TFoo.Bar)}` on the
            // NEXT peek, so the members must already be visible to it.
            self.cursor.state_mut().declare_interface_key(name.key);
            self.cursor.state_mut().record_own_type_members(
                name.key,
                type_can_inherit(&type_expression),
                type_member_entries(&type_expression),
            );
            // Record the full structure too, so a following `{$IF SizeOf(TFoo)}`
            // can lay out this own type mid-parse (needs packed flag / nested
            // inline types the flattened member map does not carry). Clone once
            // into an `Rc`; the owned value still moves into the declaration.
            self.cursor.state_mut().record_own_type_expression(
                name.key,
                std::rc::Rc::new(type_expression.clone()),
            );
            self.consume_trailing_directives()?;
            declarations.push(InterfaceDeclaration {
                kind: DeclarationKind::Type,
                name,
                constant_value: None,
                type_expression: Some(type_expression),
                generic_parameters,
                attributes,
            });
        }
    }

    // const/resourcestring: Name = value; | Name: Type = value;
    fn parse_constant_section(
        &mut self,
        kind: DeclarationKind,
        declarations: &mut Vec<InterfaceDeclaration>,
    ) -> Result<(), ParseError> {
        loop {
            let attributes = self.take_leading_attributes()?;
            match self.peek_token()? {
                Some(token) if token.can_be_identifier() => {}
                _ => return self.restore_pending_attributes(attributes),
            }
            let name = self.simple_name()?;
            let mut type_expression = None;
            let (constant_value, declaration_finished) =
                if self.peek_token()? == Some(Token::Colon) {
                    // typed constant: `Origin: TPoint = (X: 0; Y: 0);`
                    self.cursor.advance()?;
                    type_expression = Some(self.parse_type_expression()?);
                    if !self.consume_type_terminating_eq()? {
                        return Err(ParseError::Unexpected {
                            expected: "'=' after typed-constant type",
                            found: self.cursor.peek()?,
                        });
                    }
                    self.expression_span(&[Token::Semicolon])?;
                    self.expect_keyword(Token::Semicolon, "';'")?;
                    self.consume_trailing_directives()?;
                    (None, true)
                } else if kind == DeclarationKind::Const {
                    self.try_capture_literal_constant()?
                } else {
                    (None, false)
                };
            if !declaration_finished {
                self.skip_declaration_tail(true)?;
            }
            self.cursor.state_mut().declare_interface_key(name.key);
            if let Some(value) = constant_value {
                self.cursor.state_mut().record_own_constant(name.key, value);
            }
            declarations.push(InterfaceDeclaration {
                kind,
                name,
                constant_value,
                type_expression,
                generic_parameters: Vec::new(),
                attributes,
            });
        }
    }

    /// Captures `= <literal>;` / `= -<number>;` constant initializers.
    /// Returns (value, whole-declaration-consumed). On a partial match the
    /// consumed tokens are simply part of what `skip_declaration_tail` would
    /// have skipped — the caller continues with the tail skipper.
    fn try_capture_literal_constant(
        &mut self,
    ) -> Result<(Option<crate::unit_cache::ConstantValue>, bool), ParseError> {
        if self.peek_token()? != Some(Token::Eq) {
            return Ok((None, false)); // typed const `X: T = ...` or malformed
        }
        let second = self.cursor.peek_second()?.map(|lexeme| lexeme.token);
        let negated = second == Some(Token::Minus);
        if !negated
            && !matches!(
                second,
                Some(
                    Token::IntLiteral
                        | Token::FloatLiteral
                        | Token::StringLiteral
                        | Token::CharLiteral
                        | Token::True
                        | Token::False
                )
            )
        {
            return Ok((None, false));
        }

        self.cursor.advance()?; // '='
        if negated {
            self.cursor.advance()?; // '-'
        }
        let Some(literal) = self.cursor.advance()? else {
            return Err(ParseError::Unexpected {
                expected: "constant value",
                found: None,
            });
        };
        let value = self.literal_value(literal, negated);
        // only the exact single-literal form ends here; anything longer
        // (`= 1 + 2;`) falls back to the tail skipper
        if value.is_some() && self.peek_token()? == Some(Token::Semicolon) {
            self.cursor.advance()?;
            self.consume_trailing_directives()?;
            return Ok((value, true));
        }
        Ok((None, false))
    }

    fn literal_value(
        &self,
        literal: Lexeme,
        negated: bool,
    ) -> Option<crate::unit_cache::ConstantValue> {
        use crate::unit_cache::ConstantValue;
        let text = self.cursor.text(literal);
        let value = match literal.token {
            Token::IntLiteral => parse_integer_literal(text)?,
            Token::FloatLiteral => {
                ConstantValue::Float(text.replace('_', "").parse::<f64>().ok()?)
            }
            Token::StringLiteral if !negated => {
                let unquoted = unquote_string_literal(text);
                ConstantValue::Str(self.cursor.state().context.intern(&unquoted))
            }
            Token::CharLiteral if !negated => {
                let code = parse_character_code(text)?;
                let character = char::from_u32(code)?;
                ConstantValue::Str(
                    self.cursor
                        .state()
                        .context
                        .intern(&character.to_string()),
                )
            }
            Token::True if !negated => ConstantValue::Bool(true),
            Token::False if !negated => ConstantValue::Bool(false),
            _ => return None,
        };
        Some(match (negated, value) {
            (false, value) => value,
            // checked: `-i64::MIN` overflows → None (Unknown), never a wrong wrap.
            (true, ConstantValue::Int(v)) => ConstantValue::Int(v.checked_neg()?),
            (true, ConstantValue::Float(v)) => ConstantValue::Float(-v),
            // `-<UInt>` (a value above i64::MAX) has no exact i64/u64 form → None.
            (true, ConstantValue::UInt(_)) => return None,
            (true, _) => return None, // `-True` / `-'x'` is not a literal
        })
    }

    // var/threadvar: A, B: Type; — one declaration per name
    fn parse_variable_section(
        &mut self,
        kind: DeclarationKind,
        declarations: &mut Vec<InterfaceDeclaration>,
    ) -> Result<(), ParseError> {
        loop {
            let mut attributes = self.take_leading_attributes()?;
            match self.peek_token()? {
                Some(token) if token.can_be_identifier() => {}
                _ => return self.restore_pending_attributes(std::mem::take(&mut attributes)),
            }
            let mut group = Vec::new();
            loop {
                let name = self.simple_name()?;
                self.cursor.state_mut().declare_interface_key(name.key);
                group.push(name);
                if self.peek_token()? == Some(Token::Comma) {
                    self.cursor.advance()?;
                    continue;
                }
                break;
            }
            // structured type annotation; initializer/absolute as spans
            self.expect_keyword(Token::Colon, "':'")?;
            let variable_type = self.parse_type_expression()?;
            if std::mem::take(&mut self.pending_type_eq) {
                // `X: TArray<Byte>=(...)` — `>=` already delivered the `=`
                self.expression_span(&[Token::Semicolon])?;
            } else {
                self.consume_hint_directives()?;
                match self.peek_token()? {
                    Some(Token::Eq) => {
                        self.cursor.advance()?;
                        self.expression_span(&[Token::Semicolon])?;
                    }
                    Some(Token::Absolute) => {
                        self.cursor.advance()?;
                        self.expression_span(&[Token::Semicolon])?;
                    }
                    _ => {}
                }
            }
            self.expect_keyword(Token::Semicolon, "';'")?;
            self.consume_trailing_directives()?;
            // one declaration per name; the parsed type has single ownership
            // in the AST and is attached to the LAST declaration of the
            // group (`GCount, GTotal: Integer` — semantic layer duplicates)
            let group_length = group.len();
            for (index, name) in group.into_iter().enumerate() {
                // Attributes bind the whole group; move them onto the first
                // declaration (avoids cloning the span vec per name).
                let declaration_attributes = if index == 0 {
                    std::mem::take(&mut attributes)
                } else {
                    Vec::new()
                };
                declarations.push(InterfaceDeclaration {
                    kind,
                    name,
                    constant_value: None,
                    type_expression: None,
                    generic_parameters: Vec::new(),
                    attributes: declaration_attributes,
                });
            }
            if group_length > 0 {
                let last = declarations.len() - 1;
                declarations[last].type_expression = Some(variable_type);
            }
        }
    }

    fn parse_routine_header(
        &mut self,
        kind: DeclarationKind,
    ) -> Result<InterfaceDeclaration, ParseError> {
        // `[Foo] procedure Bar;` — the attribute was captured at the section
        // top level before `procedure` dispatched here; drain it.
        let attributes = std::mem::take(&mut self.pending_attributes);
        let name = self.simple_name()?;
        let (generic_parameters, equal_consumed) = self.parse_generic_parameters()?;
        if equal_consumed {
            // `>=` fusion means `=` followed the generic list — never valid
            // on a routine header
            return Err(ParseError::Unexpected {
                expected: "'(' or ';' after routine name",
                found: self.cursor.peek()?,
            });
        }
        let routine_kind = if kind == DeclarationKind::Function {
            RoutineKind::Function
        } else {
            RoutineKind::Procedure
        };
        let routine = self.parse_routine_type(routine_kind)?;
        self.expect_keyword(Token::Semicolon, "';'")?;
        self.consume_method_directives()?;
        self.cursor.state_mut().declare_interface_key(name.key);
        Ok(InterfaceDeclaration {
            kind,
            name,
            constant_value: None,
            type_expression: Some(TypeExpression::Routine(Box::new(routine))),
            generic_parameters,
            attributes,
        })
    }

    /// Single (undotted) declaration name, dual-track interned.
    fn simple_name(&mut self) -> Result<QualifiedName, ParseError> {
        let lexeme = self.expect_identifier_like()?;
        let text = self.identifier_text(lexeme);
        Ok(QualifiedName {
            name: self.cursor.state().context.intern(text),
            key: self.cursor.state().context.intern_key(text),
            location: lexeme.location,
        })
    }

    /// Identifier text with the reserved-word escape stripped: `&begin` denotes
    /// the identifier `begin` (the `&` only suppresses keyword interpretation,
    /// so `&Type` and `Type` are the SAME symbol). Only `Ident` tokens carry
    /// the escape — keyword-like lexemes that double as names never do.
    fn identifier_text(&self, lexeme: Lexeme) -> &str {
        let raw = self.cursor.text(lexeme);
        if lexeme.token == Token::Ident {
            raw.strip_prefix('&').unwrap_or(raw)
        } else {
            raw
        }
    }

    /// Parses `<...>` after a declaration name, CAPTURING the declared type
    /// parameters and their constraint clauses. Returns the parameters plus a
    /// flag that is true when the closing `>` fused with a following `=` into
    /// `>=` (`TBox<T>=class`) — the caller's `=` is then already consumed.
    ///
    /// Grammar (Embarcadero): a `<...>` list holds parameter groups separated
    /// by `;`; each group is a comma-separated identifier list sharing one
    /// optional `: constraint-clause` (so `<T, U: IFoo>` constrains BOTH).
    fn parse_generic_parameters(
        &mut self,
    ) -> Result<(Vec<GenericParameter>, bool), ParseError> {
        if self.peek_token()? != Some(Token::Lt) {
            return Ok((Vec::new(), false));
        }
        self.cursor.advance()?; // '<'
        let mut parameters = Vec::new();
        loop {
            // comma-separated names sharing one constraint clause
            let mut names = Vec::new();
            loop {
                names.push(self.simple_name()?);
                if self.peek_token()? == Some(Token::Comma) {
                    self.cursor.advance()?;
                    continue;
                }
                break;
            }
            let constraints = if self.peek_token()? == Some(Token::Colon) {
                self.cursor.advance()?;
                Some(self.capture_constraint_span()?)
            } else {
                None
            };
            for name in names {
                parameters.push(GenericParameter { name, constraints });
            }
            match self.peek_token()? {
                Some(Token::Semicolon) => {
                    self.cursor.advance()?;
                }
                Some(Token::Gt) => {
                    self.cursor.advance()?;
                    return Ok((parameters, false));
                }
                Some(Token::GtEq) => {
                    self.cursor.advance()?;
                    return Ok((parameters, true)); // '>' closed the list, '=' consumed
                }
                found => {
                    return Err(ParseError::Unexpected {
                        expected: "';', '>' or ':' in type parameter list",
                        found: found.and(self.cursor.peek()?),
                    });
                }
            }
        }
    }

    /// Captures a generic constraint clause (`class, constructor`,
    /// `IComparable<T>`, …) as one span, stopping (without consuming) at the
    /// enclosing `;`, `>` or `>=` at angle depth 0. Nested `<...>` in
    /// constraint type references is tracked so their `>` is not mistaken for
    /// the parameter-list close.
    fn capture_constraint_span(&mut self) -> Result<CodeLocation, ParseError> {
        let mut angle_depth = 0usize;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut first: Option<Lexeme> = None;
        let mut last: Option<Lexeme> = None;
        loop {
            let Some(peeked) = self.cursor.peek()? else {
                return Err(ParseError::Unexpected {
                    expected: "generic constraint",
                    found: None,
                });
            };
            let token = peeked.token;
            let at_top = angle_depth == 0 && paren_depth == 0 && bracket_depth == 0;
            if at_top && matches!(token, Token::Semicolon | Token::Gt | Token::GtEq) {
                break;
            }
            match token {
                Token::Lt => angle_depth += 1,
                // only reached at angle_depth > 0 (top-level Gt broke above):
                // closes a nested generic in the constraint type reference
                Token::Gt | Token::GtEq => angle_depth = angle_depth.saturating_sub(1),
                Token::LParen => paren_depth += 1,
                Token::RParen => paren_depth = paren_depth.saturating_sub(1),
                Token::LBracket => bracket_depth += 1,
                Token::RBracket => bracket_depth = bracket_depth.saturating_sub(1),
                _ => {}
            }
            self.cursor.advance()?;
            if token.can_be_identifier() {
                let key = {
                    let text = self.identifier_text(peeked);
                    self.cursor.state().context.intern_key(text)
                };
                self.cursor
                    .state_mut()
                    .record_usage(crate::unit_cache::Usage {
                        symbol: key,
                        location: peeked.location,
                    });
            }
            if first.is_none() {
                first = Some(peeked);
            }
            last = Some(peeked);
        }
        let (Some(first), Some(last)) = (first, last) else {
            return Err(ParseError::Unexpected {
                expected: "non-empty generic constraint",
                found: self.cursor.peek()?,
            });
        };
        Ok(join_locations(first.location, last.location))
    }

    /// Skip one declaration body up to its terminating `;` at depth 0.
    /// Tracks three depths: `record`/`class`/`interface`/`object`/
    /// `dispinterface`…`end` blocks, parentheses (params, record constant
    /// values — their inner `;` must not terminate) and brackets (GUIDs,
    /// array bounds, attributes inside class bodies).
    fn skip_declaration_tail(&mut self, trailing_directives: bool) -> Result<(), ParseError> {
        let mut block_depth = 0usize;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut previous: Option<Token> = None;

        loop {
            let found = self.cursor.advance()?;
            let Some(lexeme) = found else {
                return Err(ParseError::Unexpected {
                    expected: "';' ending the declaration",
                    found: None,
                });
            };
            match lexeme.token {
                Token::LParen => paren_depth += 1,
                Token::RParen => {
                    paren_depth = paren_depth.checked_sub(1).ok_or(ParseError::Unexpected {
                        expected: "balanced parentheses",
                        found,
                    })?;
                }
                Token::LBracket => bracket_depth += 1,
                Token::RBracket => {
                    bracket_depth =
                        bracket_depth.checked_sub(1).ok_or(ParseError::Unexpected {
                            expected: "balanced brackets",
                            found,
                        })?;
                }
                Token::Class => {
                    if self.class_opens_block(previous)? {
                        block_depth += 1;
                    }
                }
                Token::Record => {
                    if self.record_opens_block()? {
                        block_depth += 1;
                    }
                }
                Token::Object => {
                    // `procedure(...) of object` is a calling-kind suffix,
                    // `= object ... end` a (legacy) block
                    if previous != Some(Token::Of) {
                        block_depth += 1;
                    }
                }
                Token::Interface | Token::DispInterface => {
                    // forward declaration `IFoo = interface;` opens nothing
                    if self.peek_token()? != Some(Token::Semicolon) {
                        block_depth += 1;
                    }
                }
                Token::End => {
                    block_depth = block_depth.checked_sub(1).ok_or(ParseError::Unexpected {
                        expected: "';' ending the declaration (unbalanced 'end')",
                        found,
                    })?;
                }
                Token::Semicolon
                    if block_depth == 0 && paren_depth == 0 && bracket_depth == 0 =>
                {
                    break;
                }
                _ => {}
            }
            // interface-side usage index: every identifier inside declaration
            // bodies (field types, base classes, parameter types, ...)
            if lexeme.token.can_be_identifier() {
                let key = {
                    let text = self.identifier_text(lexeme);
                    self.cursor.state().context.intern_key(text)
                };
                self.cursor
                    .state_mut()
                    .record_usage(crate::unit_cache::Usage {
                        symbol: key,
                        location: lexeme.location,
                    });
            }
            previous = Some(lexeme.token);
        }

        if trailing_directives {
            self.consume_trailing_directives()?;
        }
        Ok(())
    }

    /// `; stdcall; deprecated 'msg';` after routine headers, procedure-type
    /// declarations and variables of procedure type. Two-token lookahead
    /// guards against declarations whose NAME is a context keyword
    /// (`const Platform = 2;` — `platform` here starts a new entry, it is
    /// not a portability directive).
    fn consume_trailing_directives(&mut self) -> Result<(), ParseError> {
        loop {
            let Some(peeked) = self.cursor.peek()? else {
                return Ok(());
            };
            let candidate = peeked.token;
            // `register` has no keyword token (plain Ident) — text check
            let register_convention = candidate == Token::Ident
                && self.cursor.text(peeked).eq_ignore_ascii_case("register");
            if !is_trailing_directive(candidate) && !register_convention {
                return Ok(());
            }
            let second = self.cursor.peek_second()?.map(|lexeme| lexeme.token);
            match second {
                // `stdcall;` — plain directive
                Some(Token::Semicolon) => {
                    self.cursor.advance()?;
                    self.cursor.advance()?;
                }
                // `deprecated 'message';`
                Some(Token::StringLiteral) if candidate == Token::Deprecated => {
                    self.cursor.advance()?;
                    self.cursor.advance()?;
                    self.expect_keyword(Token::Semicolon, "';'")?;
                }
                // anything else: the keyword is a declaration name
                _ => return Ok(()),
            }
        }
    }

    fn class_opens_block(&mut self, previous: Option<Token>) -> Result<bool, ParseError> {
        // forward `= class;` | `class of T` | generic constraint closers
        // (`<T: class>`, `<T: class; U>`, `<T: class, constructor>`)
        if matches!(
            self.peek_token()?,
            Some(Token::Semicolon | Token::Of | Token::Comma | Token::Gt | Token::GtEq) | None
        ) {
            return Ok(false);
        }
        // type header always follows `=`. The tail skipper starts right
        // AFTER that `=`, so header-`class` is either the very first tail
        // token (previous == None) or follows an inner `=` (nested
        // `type TInner = class`). Only there `class procedure` means "class
        // body whose first member is a procedure", not the member marker.
        if matches!(previous, None | Some(Token::Eq)) {
            return Ok(true);
        }
        // inside a body: `class function/var/property/...` member markers
        if matches!(
            self.peek_token()?,
            Some(
                Token::Function
                    | Token::Procedure
                    | Token::Constructor
                    | Token::Destructor
                    | Token::Var
                    | Token::ThreadVar
                    | Token::Property
                    | Token::Operator
            )
        ) {
            return Ok(false);
        }
        // remaining forms open a block (`class abstract`, `class sealed`,
        // `class helper`, `class(`). A wrong `true` here surfaces loudly as
        // an unbalanced-'end' error rather than silently misparsing.
        Ok(true)
    }

    fn record_opens_block(&mut self) -> Result<bool, ParseError> {
        Ok(!matches!(
            self.peek_token()?,
            // generic constraint positions only: `<T: record>` / `, U` / `; U`
            Some(Token::Gt | Token::GtEq | Token::Comma | Token::Semicolon) | None
        ))
    }

    /// Parse every consecutive `[...]` attribute group at the current position,
    /// returning the captured attributes in source order. Handles stacked
    /// groups (`[A][B]`), comma-grouped attributes (`[A, B(1)]`), qualified
    /// names (`[Xml.Serializable]`) and argument lists as raw spans (contents
    /// never parsed). Balanced-bracket tolerant: a nested `[]` or `(...)` inside
    /// an argument list does not close the group prematurely.
    ///
    /// Replaces the former `skip_attribute` (SESSION.md ledger #16): the same
    /// balanced scan, but capturing rather than discarding.
    /// Leading attributes for the next declaration: any `pending_attributes`
    /// captured at the section top level (`[Foo] type …`) FIRST, then any
    /// attributes written inline right here (`type [Foo] TBar = …`). Drains the
    /// pending buffer so it never leaks to a later declaration.
    fn take_leading_attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attributes = std::mem::take(&mut self.pending_attributes);
        attributes.append(&mut self.parse_attributes()?);
        Ok(attributes)
    }

    /// Return leading attributes that a section parser took but found no
    /// declaration for (the next token starts a new section or `implementation`)
    /// to the pending buffer, so the interface loop re-decides where they
    /// belong. On section exit the pending buffer is empty, so these become the
    /// whole pending set (order preserved). Always `Ok(())` — a convenience so
    /// the call sites can `return self.restore_pending_attributes(..)`.
    fn restore_pending_attributes(&mut self, attributes: Vec<Attribute>) -> Result<(), ParseError> {
        // Prepend: any (currently none) already-pending attribute was captured
        // AFTER these, so source order is these-first.
        let mut restored = attributes;
        restored.append(&mut self.pending_attributes);
        self.pending_attributes = restored;
        Ok(())
    }

    /// Attributes left in `pending_attributes` when the interface section ends
    /// (`[Foo] implementation`) have no declaration to attach to. That is
    /// invalid Delphi, so discarding them is correct — but the discard must be
    /// LOUD, not silent (no-silent-swallow rule): emit a diagnostic per dropped
    /// group. Drains the buffer.
    fn report_dropped_pending_attributes(&mut self) {
        for attribute in std::mem::take(&mut self.pending_attributes) {
            self.cursor.push_diagnostic(
                attribute.location,
                // A dropped attribute at a section boundary is benign for
                // analysis (it attached to nothing) — surface it as a Hint, not
                // a warning about broken code.
                crate::token_cursor::Severity::Hint,
                "attribute before `implementation` has no declaration to attach \
                 to; ignored (invalid Delphi)",
            );
        }
    }

    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attributes = Vec::new();
        while self.peek_token()? == Some(Token::LBracket) {
            let open = self.cursor.advance()?.expect("LBracket peeked");
            // An empty `[]` is not valid Delphi, but tolerate it (no attrs).
            if self.peek_token()? == Some(Token::RBracket) {
                self.cursor.advance()?;
                continue;
            }
            loop {
                let name = self.type_name_reference()?; // records the usage too
                let mut end = name.location;
                let arguments = if self.peek_token()? == Some(Token::LParen) {
                    let span = self.balanced_group_span(Token::LParen, Token::RParen)?;
                    end = span;
                    Some(span)
                } else {
                    None
                };
                attributes.push(Attribute {
                    name,
                    arguments,
                    location: join_locations(open.location, end),
                });
                match self.peek_token()? {
                    Some(Token::Comma) => {
                        self.cursor.advance()?;
                        continue;
                    }
                    Some(Token::RBracket) => {
                        self.cursor.advance()?;
                        break;
                    }
                    _ => {
                        return Err(ParseError::Unexpected {
                            expected: "',' or ']' in attribute list",
                            found: self.cursor.peek()?,
                        });
                    }
                }
            }
        }
        Ok(attributes)
    }

    /// Capture a balanced `(...)` (or `[...]`) group as one span, INCLUDING the
    /// opening and closing delimiters, tolerating nested brackets/parens of
    /// either kind inside. Consumes through the closing delimiter. Identifier
    /// usages inside are recorded (find-references over attribute arguments).
    fn balanced_group_span(
        &mut self,
        open_token: Token,
        close_token: Token,
    ) -> Result<CodeLocation, ParseError> {
        let open = self.cursor.advance()?.ok_or(ParseError::Unexpected {
            expected: "'(' or '[' opening a group",
            found: None,
        })?;
        debug_assert_eq!(open.token, open_token);
        let mut paren_depth = usize::from(open_token == Token::LParen);
        let mut bracket_depth = usize::from(open_token == Token::LBracket);
        let last = loop {
            let Some(lexeme) = self.cursor.advance()? else {
                return Err(ParseError::Unexpected {
                    expected: "closing ')' or ']' in attribute arguments",
                    found: None,
                });
            };
            match lexeme.token {
                Token::LParen => paren_depth += 1,
                Token::RParen => paren_depth = paren_depth.saturating_sub(1),
                Token::LBracket => bracket_depth += 1,
                Token::RBracket => bracket_depth = bracket_depth.saturating_sub(1),
                _ if lexeme.token.can_be_identifier() => {
                    let key = {
                        let text = self.identifier_text(lexeme);
                        self.cursor.state().context.intern_key(text)
                    };
                    self.cursor
                        .state_mut()
                        .record_usage(crate::unit_cache::Usage {
                            symbol: key,
                            location: lexeme.location,
                        });
                }
                _ => {}
            }
            if paren_depth == 0 && bracket_depth == 0 && lexeme.token == close_token {
                break lexeme;
            }
        };
        Ok(join_locations(open.location, last.location))
    }

    // ─── Type expressions (deep type parse) ──────────────────────────────

    fn parse_type_expression(&mut self) -> Result<TypeExpression, ParseError> {
        self.enter_depth()?;
        let result = self.parse_type_expression_inner();
        self.exit_depth();
        result
    }

    fn parse_type_expression_inner(&mut self) -> Result<TypeExpression, ParseError> {
        match self.peek_token()? {
            Some(Token::Caret) => {
                self.cursor.advance()?;
                Ok(TypeExpression::Pointer(Box::new(
                    self.parse_type_expression()?,
                )))
            }
            Some(Token::Packed) => {
                self.cursor.advance()?;
                match self.parse_type_expression()? {
                    TypeExpression::Record(mut structured) => {
                        structured.is_packed = true;
                        Ok(TypeExpression::Record(structured))
                    }
                    TypeExpression::Class(mut class_type) => {
                        class_type.is_packed = true;
                        Ok(TypeExpression::Class(class_type))
                    }
                    other => Ok(other), // `packed array` — packedness affects layout later
                }
            }
            Some(Token::Class) => self.parse_class_flavor(),
            Some(Token::Record) => self
                .parse_structured_type(StructuredKind::Record)
                .map(|structured| TypeExpression::Record(Box::new(structured))),
            Some(Token::Object) => self
                .parse_structured_type(StructuredKind::Object)
                .map(|structured| TypeExpression::Record(Box::new(structured))),
            Some(Token::Interface) => self.parse_interface_type(false),
            Some(Token::DispInterface) => self.parse_interface_type(true),
            Some(Token::Array) => {
                self.cursor.advance()?;
                let bounds = if self.peek_token()? == Some(Token::LBracket) {
                    self.cursor.advance()?;
                    let span = self.expression_span(&[Token::RBracket])?;
                    self.expect_keyword(Token::RBracket, "']'")?;
                    Some(span)
                } else {
                    None
                };
                self.expect_keyword(Token::Of, "'of'")?;
                if self.peek_token()? == Some(Token::Const) {
                    // `array of const` — open varargs parameter
                    self.cursor.advance()?;
                    return Ok(TypeExpression::ArrayOfConst);
                }
                Ok(TypeExpression::Array {
                    bounds,
                    element: Box::new(self.parse_type_expression()?),
                })
            }
            Some(Token::Set) => {
                self.cursor.advance()?;
                self.expect_keyword(Token::Of, "'of'")?;
                Ok(TypeExpression::SetOf(Box::new(
                    self.parse_type_expression()?,
                )))
            }
            Some(Token::File) => {
                self.cursor.advance()?;
                if self.peek_token()? == Some(Token::Of) {
                    self.cursor.advance()?;
                    Ok(TypeExpression::File(Some(Box::new(
                        self.parse_type_expression()?,
                    ))))
                } else {
                    Ok(TypeExpression::File(None))
                }
            }
            Some(Token::String) => {
                let string_lexeme = self.cursor.advance()?.expect("peeked");
                if self.peek_token()? == Some(Token::LBracket) {
                    self.cursor.advance()?;
                    let length = self.expression_span(&[Token::RBracket])?;
                    self.expect_keyword(Token::RBracket, "']'")?;
                    Ok(TypeExpression::SizedString(length))
                } else {
                    let text = self.cursor.text(string_lexeme);
                    Ok(TypeExpression::Reference {
                        name: QualifiedName {
                            name: self.cursor.state().context.intern(text),
                            key: self.cursor.state().context.intern_key(text),
                            location: string_lexeme.location,
                        },
                        type_arguments: Vec::new(),
                    })
                }
            }
            Some(Token::Procedure) => {
                self.cursor.advance()?;
                self.parse_routine_type(RoutineKind::Procedure)
                    .map(|routine| TypeExpression::Routine(Box::new(routine)))
            }
            Some(Token::Function) => {
                self.cursor.advance()?;
                self.parse_routine_type(RoutineKind::Function)
                    .map(|routine| TypeExpression::Routine(Box::new(routine)))
            }
            Some(Token::Reference) => {
                // `reference to procedure/function(...)`
                self.cursor.advance()?;
                self.expect_keyword(Token::To, "'to'")?;
                let kind = match self.advance_token()? {
                    Some(Token::Procedure) => RoutineKind::Procedure,
                    Some(Token::Function) => RoutineKind::Function,
                    _ => {
                        return Err(ParseError::Unexpected {
                            expected: "'procedure' or 'function' after 'reference to'",
                            found: self.cursor.peek()?,
                        });
                    }
                };
                self.parse_routine_type(kind)
                    .map(|routine| TypeExpression::AnonymousMethod(Box::new(routine)))
            }
            Some(Token::Type) => {
                // distinct type: `TMyInt = type Integer`
                self.cursor.advance()?;
                Ok(TypeExpression::Distinct(Box::new(
                    self.parse_type_expression()?,
                )))
            }
            Some(Token::LParen) => self.parse_enumeration(),
            Some(token) if token.can_be_identifier() => {
                let start = self.cursor.peek()?.expect("peeked").location;
                let name = self.type_name_reference()?;
                let type_arguments = self.parse_type_argument_list()?;
                if self.peek_token()? == Some(Token::DotDot) {
                    self.cursor.advance()?;
                    let end = self.expression_span(&[
                        Token::Semicolon,
                        Token::RParen,
                        Token::RBracket,
                        Token::Comma,
                        Token::Eq,
                        Token::End,
                    ])?;
                    return Ok(TypeExpression::Subrange(join_locations(start, end)));
                }
                Ok(TypeExpression::Reference {
                    name,
                    type_arguments,
                })
            }
            // literal-headed subrange: `0..5`, `-1..1`, `'a'..'z'`
            Some(
                Token::IntLiteral
                | Token::FloatLiteral
                | Token::CharLiteral
                | Token::StringLiteral
                | Token::Minus
                | Token::Plus,
            ) => {
                let span = self.expression_span(&[
                    Token::Semicolon,
                    Token::RParen,
                    Token::RBracket,
                    Token::Comma,
                    Token::Eq,
                    Token::End,
                ])?;
                Ok(TypeExpression::Subrange(span))
            }
            _ => Err(ParseError::Unexpected {
                expected: "type expression",
                found: self.cursor.peek()?,
            }),
        }
    }

    /// `class` already peeked: forward, class-of, or full class type.
    fn parse_class_flavor(&mut self) -> Result<TypeExpression, ParseError> {
        self.cursor.advance()?; // 'class'
        match self.peek_token()? {
            Some(Token::Semicolon) => Ok(TypeExpression::ForwardClass), // caller eats ';'
            Some(Token::Of) => {
                self.cursor.advance()?;
                Ok(TypeExpression::ClassReference(self.type_name_reference()?))
            }
            _ => {
                let mut class_type = ClassType {
                    is_packed: false,
                    is_sealed: false,
                    is_abstract: false,
                    ancestors: Vec::new(),
                    helper_for: None,
                    sections: Vec::new(),
                };
                loop {
                    match self.peek_token()? {
                        Some(Token::Sealed) => {
                            self.cursor.advance()?;
                            class_type.is_sealed = true;
                        }
                        Some(Token::Abstract) => {
                            self.cursor.advance()?;
                            class_type.is_abstract = true;
                        }
                        Some(Token::Helper) => {
                            self.cursor.advance()?;
                            // `class helper [(TBaseHelper)] for TFoo`
                            if self.peek_token()? == Some(Token::LParen) {
                                self.cursor.advance()?;
                                let name = self.type_name_reference()?;
                                let type_arguments = self.parse_type_argument_list()?;
                                class_type.ancestors.push(Ancestor { name, type_arguments });
                                self.expect_keyword(Token::RParen, "')'")?;
                            }
                            self.expect_keyword(Token::For, "'for'")?;
                            class_type.helper_for = Some(self.type_name_reference()?);
                            // helper target may be generic: `for TList<Integer>`
                            self.parse_type_argument_list()?;
                        }
                        _ => break,
                    }
                }
                if self.peek_token()? == Some(Token::LParen) {
                    self.cursor.advance()?;
                    loop {
                        // ancestor may be generic: `class(TList<T>)`
                        let name = self.type_name_reference()?;
                        let type_arguments = self.parse_type_argument_list()?;
                        class_type.ancestors.push(Ancestor { name, type_arguments });
                        match self.advance_token()? {
                            Some(Token::Comma) => continue,
                            Some(Token::RParen) => break,
                            _ => {
                                return Err(ParseError::Unexpected {
                                    expected: "',' or ')' in ancestor list",
                                    found: self.cursor.peek()?,
                                });
                            }
                        }
                    }
                    // `TFoo = class(TBase);` — ancestor-only shorthand
                    if self.peek_token()? == Some(Token::Semicolon) {
                        return Ok(TypeExpression::Class(Box::new(class_type)));
                    }
                }
                let (sections, _) = self.parse_member_sections(true)?;
                class_type.sections = sections;
                self.expect_keyword(Token::End, "'end'")?;
                Ok(TypeExpression::Class(Box::new(class_type)))
            }
        }
    }

    fn parse_structured_type(
        &mut self,
        kind: StructuredKind,
    ) -> Result<StructuredType, ParseError> {
        self.cursor.advance()?; // 'record' | 'object'
        let mut structured = StructuredType {
            kind,
            is_packed: false,
            sections: Vec::new(),
            variant_part: None,
            helper_for: None,
        };
        if self.peek_token()? == Some(Token::Helper) {
            self.cursor.advance()?;
            self.expect_keyword(Token::For, "'for'")?;
            structured.helper_for = Some(self.type_name_reference()?);
            // helper target may be generic: `record helper for TArray<Byte>`
            self.parse_type_argument_list()?;
        }
        let (sections, variant_part) = self.parse_member_sections(true)?;
        structured.sections = sections;
        structured.variant_part = variant_part;
        self.expect_keyword(Token::End, "'end'")?;
        Ok(structured)
    }

    fn parse_interface_type(
        &mut self,
        is_dispinterface: bool,
    ) -> Result<TypeExpression, ParseError> {
        self.cursor.advance()?; // 'interface' | 'dispinterface'
        if self.peek_token()? == Some(Token::Semicolon) {
            return Ok(if is_dispinterface {
                TypeExpression::ForwardDispInterface
            } else {
                TypeExpression::ForwardInterface
            });
        }
        let mut interface_type = InterfaceType {
            is_dispinterface,
            ancestors: Vec::new(),
            guid: None,
            members: Vec::new(),
        };
        if self.peek_token()? == Some(Token::LParen) {
            self.cursor.advance()?;
            loop {
                // ancestor may be generic: `interface(IEnumerable<T>)`
                let name = self.type_name_reference()?;
                let type_arguments = self.parse_type_argument_list()?;
                interface_type.ancestors.push(Ancestor { name, type_arguments });
                match self.advance_token()? {
                    Some(Token::Comma) => continue,
                    Some(Token::RParen) => break,
                    _ => {
                        return Err(ParseError::Unexpected {
                            expected: "',' or ')' in ancestor list",
                            found: self.cursor.peek()?,
                        });
                    }
                }
            }
        }
        if self.peek_token()? == Some(Token::LBracket) {
            self.cursor.advance()?;
            interface_type.guid = Some(self.expression_span(&[Token::RBracket])?);
            self.expect_keyword(Token::RBracket, "']'")?;
        }
        let (mut sections, _) = self.parse_member_sections(false)?;
        if let Some(section) = sections.pop() {
            interface_type.members = section.members;
        }
        self.expect_keyword(Token::End, "'end'")?;
        Ok(TypeExpression::Interface(Box::new(interface_type)))
    }

    /// Shared member parsing for classes, records, objects and interfaces.
    /// Stops at `end` (not consumed). `allow_visibility` is false for
    /// interfaces (single flat section).
    fn parse_member_sections(
        &mut self,
        allow_visibility: bool,
    ) -> Result<(Vec<VisibilitySection>, Option<VariantPart>), ParseError> {
        self.enter_depth()?;
        // Track structural block nesting so declaration-level error recovery can
        // tell it is INSIDE a class/record/object/interface body. The opener was
        // already consumed by the caller, so a member failure here would leave
        // the interface-loop resync unable to see that it must first climb OUT
        // of the broken block (else a member starter — `procedure`/`type`/… —
        // gets re-dispatched as a phantom TOP-LEVEL symbol). We increment on the
        // way in and decrement ONLY on the Ok path: on error the counter stays
        // inflated by exactly the number of still-open bodies, which
        // `recover_from_interface_error` reads to seed (then resets).
        self.block_nesting += 1;
        let result = self.parse_member_sections_inner(allow_visibility);
        if result.is_ok() {
            self.block_nesting -= 1;
        }
        self.exit_depth();
        result
    }

    fn parse_member_sections_inner(
        &mut self,
        allow_visibility: bool,
    ) -> Result<(Vec<VisibilitySection>, Option<VariantPart>), ParseError> {
        let mut sections = vec![VisibilitySection {
            visibility: Visibility::Unspecified,
            strict: false,
            members: Vec::new(),
        }];
        let mut variant_part = None;

        loop {
            match self.peek_token()? {
                Some(Token::End) | None => break,
                Some(Token::Strict) if allow_visibility => {
                    self.cursor.advance()?;
                    let visibility = match self.advance_token()? {
                        Some(Token::Private) => Visibility::Private,
                        Some(Token::Protected) => Visibility::Protected,
                        _ => {
                            return Err(ParseError::Unexpected {
                                expected: "'private' or 'protected' after 'strict'",
                                found: self.cursor.peek()?,
                            });
                        }
                    };
                    sections.push(VisibilitySection {
                        visibility,
                        strict: true,
                        members: Vec::new(),
                    });
                }
                Some(
                    Token::Private
                    | Token::Protected
                    | Token::Public
                    | Token::Published
                    | Token::Automated,
                ) if allow_visibility => {
                    let visibility = match self.advance_token()?.expect("peeked") {
                        Token::Private => Visibility::Private,
                        Token::Protected => Visibility::Protected,
                        Token::Public => Visibility::Public,
                        Token::Published => Visibility::Published,
                        _ => Visibility::Automated,
                    };
                    sections.push(VisibilitySection {
                        visibility,
                        strict: false,
                        members: Vec::new(),
                    });
                }
                Some(Token::Case) => {
                    variant_part = Some(self.parse_variant_part()?);
                    break; // variant part is always last before `end`
                }
                _ => {
                    let members = &mut sections.last_mut().expect("non-empty").members;
                    if !self.parse_one_member(members)? {
                        break;
                    }
                }
            }
        }
        Ok((sections, variant_part))
    }

    /// One member (field group / method / property / nested type / nested
    /// const / class-var block). Returns false when nothing member-like
    /// follows (caller then expects `end`).
    fn parse_one_member(&mut self, members: &mut Vec<Member>) -> Result<bool, ParseError> {
        // Member attributes (`[Weak] FField: T`, `[Foo] procedure Bar;`) —
        // captured here and attached to the first member this call produces
        // (ledger #16). A member producer may emit several members (a field
        // group, a `var` block, a nested type/const section); the attributes
        // bind the leading one, matching Delphi's per-declaration semantics.
        let attributes = self.parse_attributes()?;
        let first_new = members.len();
        let produced = self.parse_one_member_body(members)?;
        if produced && !attributes.is_empty() {
            if let Some(member) = members.get_mut(first_new) {
                set_member_attributes(member, attributes);
            }
        }
        Ok(produced)
    }

    fn parse_one_member_body(&mut self, members: &mut Vec<Member>) -> Result<bool, ParseError> {
        match self.peek_token()? {
            Some(Token::Procedure) => {
                self.cursor.advance()?;
                members.push(self.parse_method(RoutineKind::Procedure, false)?);
            }
            Some(Token::Function) => {
                self.cursor.advance()?;
                members.push(self.parse_method(RoutineKind::Function, false)?);
            }
            Some(Token::Constructor) => {
                self.cursor.advance()?;
                members.push(self.parse_method(RoutineKind::Constructor, false)?);
            }
            Some(Token::Destructor) => {
                self.cursor.advance()?;
                members.push(self.parse_method(RoutineKind::Destructor, false)?);
            }
            Some(Token::Property) => {
                self.cursor.advance()?;
                members.push(self.parse_property(false)?);
            }
            Some(Token::Class) => {
                self.cursor.advance()?;
                match self.peek_token()? {
                    Some(Token::Procedure) => {
                        self.cursor.advance()?;
                        members.push(self.parse_method(RoutineKind::Procedure, true)?);
                    }
                    Some(Token::Function) => {
                        self.cursor.advance()?;
                        members.push(self.parse_method(RoutineKind::Function, true)?);
                    }
                    Some(Token::Constructor) => {
                        self.cursor.advance()?;
                        members.push(self.parse_method(RoutineKind::Constructor, true)?);
                    }
                    Some(Token::Destructor) => {
                        self.cursor.advance()?;
                        members.push(self.parse_method(RoutineKind::Destructor, true)?);
                    }
                    Some(Token::Operator) => {
                        self.cursor.advance()?;
                        members.push(self.parse_method(RoutineKind::Operator, true)?);
                    }
                    Some(Token::Var | Token::ThreadVar) => {
                        self.cursor.advance()?;
                        self.parse_field_group_block(members, true)?;
                    }
                    Some(Token::Property) => {
                        self.cursor.advance()?;
                        members.push(self.parse_property(true)?);
                    }
                    // `class const` / `class type` sections
                    Some(Token::Const) => {
                        self.cursor.advance()?;
                        self.parse_nested_const_members(members)?;
                    }
                    Some(Token::Type) => {
                        self.cursor.advance()?;
                        self.parse_nested_type_members(members)?;
                    }
                    _ => {
                        return Err(ParseError::Unexpected {
                            expected: "member kind after 'class'",
                            found: self.cursor.peek()?,
                        });
                    }
                }
            }
            Some(Token::Var | Token::ThreadVar) => {
                self.cursor.advance()?;
                self.parse_field_group_block(members, false)?;
            }
            Some(Token::Type) => {
                self.cursor.advance()?;
                self.parse_nested_type_members(members)?;
            }
            Some(Token::Const) => {
                self.cursor.advance()?;
                self.parse_nested_const_members(members)?;
            }
            Some(token) if token.can_be_identifier() => {
                self.parse_field_group(members, false)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Nested `type` section inside a class/record body: entries as long as
    /// `Name =` / `Name<...> =` follows. Shared by plain and `class type`.
    fn parse_nested_type_members(
        &mut self,
        members: &mut Vec<Member>,
    ) -> Result<(), ParseError> {
        loop {
            let Some(token) = self.peek_token()? else { break };
            if !token.can_be_identifier() {
                break;
            }
            if !matches!(
                self.cursor.peek_second()?.map(|lexeme| lexeme.token),
                Some(Token::Eq | Token::Lt)
            ) {
                break;
            }
            let name = self.simple_name()?;
            let (generic_parameters, equal_consumed) = self.parse_generic_parameters()?;
            if !equal_consumed {
                self.expect_keyword(Token::Eq, "'='")?;
            }
            let type_expression = self.parse_type_expression()?;
            self.expect_keyword(Token::Semicolon, "';'")?;
            self.consume_trailing_directives()?;
            members.push(Member::NestedType(Box::new(InterfaceDeclaration {
                kind: DeclarationKind::Type,
                name,
                constant_value: None,
                type_expression: Some(type_expression),
                generic_parameters,
                attributes: Vec::new(),
            })));
        }
        Ok(())
    }

    /// Nested `const` section inside a class/record body, until the next
    /// keyword-led member. Shared by plain and `class const`.
    fn parse_nested_const_members(
        &mut self,
        members: &mut Vec<Member>,
    ) -> Result<(), ParseError> {
        loop {
            let Some(token) = self.peek_token()? else { break };
            if !token.can_be_identifier() {
                break;
            }
            if !matches!(
                self.cursor.peek_second()?.map(|lexeme| lexeme.token),
                Some(Token::Eq | Token::Colon)
            ) {
                break;
            }
            let name = self.simple_name()?;
            let (constant_value, finished) = self.try_capture_literal_constant()?;
            if !finished {
                self.skip_declaration_tail(true)?;
            }
            members.push(Member::NestedConst {
                name,
                constant_value,
                attributes: Vec::new(),
            });
        }
        Ok(())
    }

    /// `var`/`class var` block: field groups until a keyword-led member.
    fn parse_field_group_block(
        &mut self,
        members: &mut Vec<Member>,
        is_class_var: bool,
    ) -> Result<(), ParseError> {
        loop {
            match self.peek_token()? {
                Some(token)
                    if token.can_be_identifier()
                        && matches!(
                            self.cursor.peek_second()?.map(|lexeme| lexeme.token),
                            Some(Token::Comma | Token::Colon)
                        ) =>
                {
                    self.parse_field_group(members, is_class_var)?;
                }
                _ => return Ok(()),
            }
        }
    }

    /// `A, B: TFoo;` — trailing `;` optional directly before `end`
    /// (`record A: Integer end` is legal).
    fn parse_field_group(
        &mut self,
        members: &mut Vec<Member>,
        is_class_var: bool,
    ) -> Result<(), ParseError> {
        let mut names = Vec::new();
        loop {
            names.push(self.simple_name()?);
            if self.peek_token()? == Some(Token::Comma) {
                self.cursor.advance()?;
                continue;
            }
            break;
        }
        self.expect_keyword(Token::Colon, "':'")?;
        let field_type = self.parse_type_expression()?;
        // field initializer (`class var X: Integer = 1;`,
        // `X: TArray<Byte>=(...)` where `>=` already delivered the `=`)
        if std::mem::take(&mut self.pending_type_eq) {
            self.expression_span(&[Token::Semicolon, Token::End])?;
        } else {
            self.consume_hint_directives()?;
            if self.peek_token()? == Some(Token::Eq) {
                self.cursor.advance()?;
                self.expression_span(&[Token::Semicolon, Token::End])?;
            }
        }
        if self.peek_token()? == Some(Token::Semicolon) {
            self.cursor.advance()?;
        }
        members.push(Member::Field {
            names,
            field_type,
            is_class_var,
            attributes: Vec::new(),
        });
        Ok(())
    }

    fn parse_method(
        &mut self,
        kind: RoutineKind,
        is_class_method: bool,
    ) -> Result<Member, ParseError> {
        let name = self.simple_name()?;
        // Method resolution clause: `procedure IFoo.Method = Impl;` maps an
        // inherited interface method onto an implementing routine. The `Dot`
        // right after the member name (never present on a real declaration)
        // is the trigger.
        if self.peek_token()? == Some(Token::Dot) {
            return self.parse_method_resolution(name, kind, is_class_method);
        }
        let (generic_parameters, equal_consumed) = self.parse_generic_parameters()?;
        if equal_consumed {
            return Err(ParseError::Unexpected {
                expected: "'(' or ';' after method name",
                found: self.cursor.peek()?,
            });
        }
        let routine = self.parse_routine_type(kind)?;
        self.expect_keyword(Token::Semicolon, "';'")?;
        let directives = self.consume_method_directives()?;
        Ok(Member::Method(Box::new(MethodDeclaration {
            name,
            routine,
            is_class_method,
            directives,
            generic_parameters,
            resolution_target: None,
            attributes: Vec::new(),
        })))
    }

    /// `procedure IFoo.Method = Impl;` — the leading identifier
    /// (`interface_name`) was already read by [`parse_method`]. Builds the
    /// qualified `IFoo.Method` name, an optional `= Impl` resolution target,
    /// and consumes the terminating `;`. No signature is present.
    fn parse_method_resolution(
        &mut self,
        interface_name: QualifiedName,
        kind: RoutineKind,
        is_class_method: bool,
    ) -> Result<Member, ParseError> {
        let mut display = crate::globals::resolve(interface_name.name).to_string();
        let mut last_location = interface_name.location;
        while self.peek_token()? == Some(Token::Dot) {
            self.cursor.advance()?;
            let part = self.expect_identifier_like()?;
            display.push('.');
            display.push_str(self.cursor.text(part));
            last_location = part.location;
        }
        let name = QualifiedName {
            name: self.cursor.state().context.intern(&display),
            key: self.cursor.state().context.intern_key(&display),
            location: join_locations(interface_name.location, last_location),
        };
        let resolution_target = if self.peek_token()? == Some(Token::Eq) {
            self.cursor.advance()?;
            Some(self.type_name_reference()?)
        } else {
            None
        };
        self.expect_keyword(Token::Semicolon, "';'")?;
        Ok(Member::Method(Box::new(MethodDeclaration {
            name,
            routine: RoutineType {
                kind,
                parameters: Vec::new(),
                return_type: None,
                of_object: false,
            },
            is_class_method,
            directives: Vec::new(),
            generic_parameters: Vec::new(),
            resolution_target,
            attributes: Vec::new(),
        })))
    }

    fn parse_property(&mut self, is_class: bool) -> Result<Member, ParseError> {
        let name = self.simple_name()?;
        let index_parameters = if self.peek_token()? == Some(Token::LBracket) {
            self.cursor.advance()?;
            let parameters = self.parse_parameter_list(Token::RBracket)?;
            self.expect_keyword(Token::RBracket, "']'")?;
            parameters
        } else {
            Vec::new()
        };
        let property_type = if self.peek_token()? == Some(Token::Colon) {
            self.cursor.advance()?;
            Some(self.parse_type_expression()?)
        } else {
            None // visibility hoisting: `property Color;`
        };

        let mut read_target = None;
        let mut write_target = None;
        const SPECIFIER_STOPS: &[Token] = &[
            Token::Semicolon,
            Token::Read,
            Token::Write,
            Token::Stored,
            Token::Default,
            Token::NoDefault,
            Token::Index,
            Token::Implements,
            Token::ReadOnly,
            Token::WriteOnly,
            Token::DispId,
        ];
        loop {
            match self.peek_token()? {
                Some(Token::Semicolon) | None => break,
                Some(Token::Read) => {
                    self.cursor.advance()?;
                    read_target = Some(self.type_name_reference()?);
                }
                Some(Token::Write) => {
                    self.cursor.advance()?;
                    write_target = Some(self.type_name_reference()?);
                }
                Some(
                    Token::Stored
                    | Token::Default
                    | Token::NoDefault
                    | Token::Index
                    | Token::Implements
                    | Token::ReadOnly
                    | Token::WriteOnly
                    | Token::DispId,
                ) => {
                    self.cursor.advance()?;
                    // value expression (may be empty for nodefault/readonly)
                    if !matches!(self.peek_token()?, Some(Token::Semicolon) | None)
                        && !SPECIFIER_STOPS.contains(&self.peek_token()?.expect("checked"))
                    {
                        self.expression_span(SPECIFIER_STOPS)?;
                    }
                }
                _ => {
                    return Err(ParseError::Unexpected {
                        expected: "property specifier or ';'",
                        found: self.cursor.peek()?,
                    });
                }
            }
        }
        self.expect_keyword(Token::Semicolon, "';'")?;

        // `; default;` array-property marker
        let mut is_default = false;
        if self.peek_token()? == Some(Token::Default)
            && self.cursor.peek_second()?.map(|lexeme| lexeme.token) == Some(Token::Semicolon)
        {
            self.cursor.advance()?;
            self.cursor.advance()?;
            is_default = true;
        }

        Ok(Member::Property(Box::new(PropertyDeclaration {
            name,
            index_parameters,
            property_type,
            read_target,
            write_target,
            is_default,
            is_class,
            attributes: Vec::new(),
        })))
    }

    /// Signature after the routine keyword (and name, for methods):
    /// `[(params)] [: ReturnType] [of object]`. Does NOT consume the `;`.
    fn parse_routine_type(&mut self, kind: RoutineKind) -> Result<RoutineType, ParseError> {
        let parameters = if self.peek_token()? == Some(Token::LParen) {
            self.cursor.advance()?;
            let parameters = self.parse_parameter_list(Token::RParen)?;
            self.expect_keyword(Token::RParen, "')'")?;
            parameters
        } else {
            Vec::new()
        };
        let return_type = if matches!(kind, RoutineKind::Function | RoutineKind::Operator)
            || self.peek_token()? == Some(Token::Colon)
        {
            self.expect_keyword(Token::Colon, "':' before return type")?;
            Some(self.parse_type_expression()?)
        } else {
            None
        };
        // Trailing procedural-type directives attach directly to the type,
        // with NO separating ';' (`function(...): BOOL stdcall`). `of object`
        // and the calling conventions may appear in either order. Routine
        // HEADERS always have their `;` before any directive, so the loop
        // simply breaks there and `consume_method_directives` handles them.
        let mut of_object = false;
        loop {
            let Some(peeked) = self.cursor.peek()? else { break };
            match peeked.token {
                Token::Of => {
                    self.cursor.advance()?;
                    self.expect_keyword(Token::Object, "'object'")?;
                    of_object = true;
                }
                Token::StdCall
                | Token::CDecl
                | Token::SafeCall
                | Token::Pascal
                | Token::WinApi
                | Token::Near
                | Token::Far
                | Token::VarArgs => {
                    self.cursor.advance()?;
                }
                // `register` has no dedicated token (plain Ident)
                Token::Ident
                    if self.cursor.text(peeked).eq_ignore_ascii_case("register") =>
                {
                    self.cursor.advance()?;
                }
                _ => break,
            }
        }
        Ok(RoutineType {
            kind,
            parameters,
            return_type,
            of_object,
        })
    }

    /// Parameters up to (not including) `terminator`:
    /// `[var|const|out] A, B [: T [= default]] ; ...`
    fn parse_parameter_list(&mut self, terminator: Token) -> Result<Vec<Parameter>, ParseError> {
        let mut parameters = Vec::new();
        if self.peek_token()? == Some(terminator) {
            return Ok(parameters);
        }
        loop {
            // Parameter attributes may sit before OR after the modifier:
            // `[ref] const A: T` and `const [ref] A: T` are both legal.
            let mut attributes = self.parse_attributes()?;
            let modifier = match self.peek_token()? {
                Some(Token::Var) => {
                    self.cursor.advance()?;
                    ParameterModifier::Var
                }
                Some(Token::Const) => {
                    self.cursor.advance()?;
                    ParameterModifier::Const
                }
                Some(Token::Out) => {
                    self.cursor.advance()?;
                    ParameterModifier::Out
                }
                _ => ParameterModifier::None,
            };
            // attributes written after the modifier (`const [ref] A: T`)
            attributes.append(&mut self.parse_attributes()?);
            let mut names = Vec::new();
            loop {
                names.push(self.simple_name()?);
                if self.peek_token()? == Some(Token::Comma) {
                    self.cursor.advance()?;
                    continue;
                }
                break;
            }
            let parameter_type = if self.peek_token()? == Some(Token::Colon) {
                self.cursor.advance()?;
                Some(self.parse_type_expression()?)
            } else {
                None // untyped: `var Buffer`
            };
            let default = if self.consume_type_terminating_eq()? {
                Some(self.expression_span(&[Token::Semicolon, terminator])?)
            } else {
                None
            };
            parameters.push(Parameter {
                modifier,
                names,
                parameter_type,
                default,
                attributes,
            });
            match self.peek_token()? {
                Some(Token::Semicolon) => {
                    self.cursor.advance()?;
                }
                Some(token) if token == terminator => break,
                _ => {
                    return Err(ParseError::Unexpected {
                        expected: "';' or closing bracket in parameter list",
                        found: self.cursor.peek()?,
                    });
                }
            }
        }
        Ok(parameters)
    }

    fn parse_enumeration(&mut self) -> Result<TypeExpression, ParseError> {
        self.expect_keyword(Token::LParen, "'('")?;
        let mut members = Vec::new();
        loop {
            let name = self.simple_name()?;
            let explicit_value = if self.peek_token()? == Some(Token::Eq) {
                self.cursor.advance()?;
                Some(self.expression_span(&[Token::Comma, Token::RParen])?)
            } else {
                None
            };
            members.push(EnumerationMember {
                name,
                explicit_value,
            });
            match self.advance_token()? {
                Some(Token::Comma) => continue,
                Some(Token::RParen) => break,
                _ => {
                    return Err(ParseError::Unexpected {
                        expected: "',' or ')' in enumeration",
                        found: self.cursor.peek()?,
                    });
                }
            }
        }
        Ok(TypeExpression::Enumeration(members))
    }

    /// `case [Tag:] TypeRef of  0: (…); 1: (…; case … of …);`
    fn parse_variant_part(&mut self) -> Result<VariantPart, ParseError> {
        self.enter_depth()?;
        let result = self.parse_variant_part_inner();
        self.exit_depth();
        result
    }

    fn parse_variant_part_inner(&mut self) -> Result<VariantPart, ParseError> {
        self.expect_keyword(Token::Case, "'case'")?;
        let first = self.type_name_reference()?;
        let (selector_name, selector_type) = if self.peek_token()? == Some(Token::Colon) {
            self.cursor.advance()?;
            (Some(first), self.type_name_reference()?)
        } else {
            (None, first)
        };
        self.expect_keyword(Token::Of, "'of'")?;

        let mut arms = Vec::new();
        loop {
            // variant part ends at the enclosing `end` or `)`
            if matches!(self.peek_token()?, Some(Token::End | Token::RParen) | None) {
                break;
            }
            let labels = self.expression_span(&[Token::Colon])?;
            self.expect_keyword(Token::Colon, "':'")?;
            self.expect_keyword(Token::LParen, "'('")?;

            let mut fields = Vec::new();
            let mut nested = None;
            loop {
                match self.peek_token()? {
                    Some(Token::RParen) | None => break,
                    Some(Token::Case) => {
                        nested = Some(Box::new(self.parse_variant_part()?));
                        break; // nested variant is the arm's tail
                    }
                    Some(token) if token.can_be_identifier() => {
                        self.parse_field_group(&mut fields, false)?;
                    }
                    _ => {
                        return Err(ParseError::Unexpected {
                            expected: "field, nested 'case' or ')' in variant arm",
                            found: self.cursor.peek()?,
                        });
                    }
                }
            }
            self.expect_keyword(Token::RParen, "')'")?;
            if self.peek_token()? == Some(Token::Semicolon) {
                self.cursor.advance()?;
            }
            arms.push(VariantArm {
                labels,
                fields,
                nested,
            });
        }
        Ok(VariantPart {
            selector_name,
            selector_type,
            arms,
        })
    }

    /// Type-position qualified name: records a usage for every reference.
    fn type_name_reference(&mut self) -> Result<QualifiedName, ParseError> {
        let name = self.qualified_name()?;
        self.cursor
            .state_mut()
            .record_usage(crate::unit_cache::Usage {
                symbol: name.key,
                location: name.location,
            });
        Ok(name)
    }

    /// `<Integer, TFoo<Byte>>` in type USAGE position (arguments, not
    /// parameter declarations). Handles the `>=`/`>>`… lexing: `>` closes.
    fn parse_type_argument_list(&mut self) -> Result<Vec<TypeExpression>, ParseError> {
        if self.peek_token()? != Some(Token::Lt) {
            return Ok(Vec::new());
        }
        self.cursor.advance()?;
        let mut arguments = Vec::new();
        loop {
            arguments.push(self.parse_type_expression()?);
            match self.advance_token()? {
                Some(Token::Comma) => continue,
                Some(Token::Gt) => break,
                // `TArray<Byte>=(...)` — the closing `>` fused with a
                // following `=` into `>=`. Close the list and hand the `=`
                // to the enclosing declaration via `pending_type_eq`.
                Some(Token::GtEq) => {
                    self.pending_type_eq = true;
                    break;
                }
                _ => {
                    return Err(ParseError::Unexpected {
                        expected: "',' or '>' in type arguments",
                        found: self.cursor.peek()?,
                    });
                }
            }
        }
        Ok(arguments)
    }

    /// Consumes a type-terminating `=` (typed-constant / field / variable
    /// initializer, parameter default). Honors a `pending_type_eq` produced
    /// by a `>=`-fused type-argument close before looking at the token stream.
    /// Returns true when an `=` was consumed.
    fn consume_type_terminating_eq(&mut self) -> Result<bool, ParseError> {
        if std::mem::take(&mut self.pending_type_eq) {
            return Ok(true);
        }
        if self.peek_token()? == Some(Token::Eq) {
            self.cursor.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Expression captured as a span: consume tokens (recording identifier
    /// usages) until one of `stops` appears at bracket depth 0. Stop token
    /// is NOT consumed. Depth-underflowing `)`/`]` also stop.
    fn expression_span(&mut self, stops: &[Token]) -> Result<CodeLocation, ParseError> {
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut first: Option<Lexeme> = None;
        let mut last: Option<Lexeme> = None;
        loop {
            let Some(peeked) = self.cursor.peek()? else {
                return Err(ParseError::Unexpected {
                    expected: "expression",
                    found: None,
                });
            };
            let token = peeked.token;
            if paren_depth == 0 && bracket_depth == 0 && stops.contains(&token) {
                break;
            }
            match token {
                Token::LParen => paren_depth += 1,
                Token::RParen => {
                    if paren_depth == 0 {
                        break;
                    }
                    paren_depth -= 1;
                }
                Token::LBracket => bracket_depth += 1,
                Token::RBracket => {
                    if bracket_depth == 0 {
                        break;
                    }
                    bracket_depth -= 1;
                }
                _ => {}
            }
            self.cursor.advance()?;
            if token.can_be_identifier() {
                let key = {
                    let text = self.identifier_text(peeked);
                    self.cursor.state().context.intern_key(text)
                };
                self.cursor
                    .state_mut()
                    .record_usage(crate::unit_cache::Usage {
                        symbol: key,
                        location: peeked.location,
                    });
            }
            if first.is_none() {
                first = Some(peeked);
            }
            last = Some(peeked);
        }
        let (Some(first), Some(last)) = (first, last) else {
            return Err(ParseError::Unexpected {
                expected: "non-empty expression",
                found: self.cursor.peek()?,
            });
        };
        Ok(join_locations(first.location, last.location))
    }

    /// `deprecated ['msg'] | platform | library | experimental` after fields
    /// and type usages, before the `;`.
    fn consume_hint_directives(&mut self) -> Result<(), ParseError> {
        loop {
            match self.peek_token()? {
                Some(Token::Deprecated) => {
                    self.cursor.advance()?;
                    if self.peek_token()? == Some(Token::StringLiteral) {
                        self.cursor.advance()?;
                    }
                }
                Some(Token::Platform | Token::Library | Token::Experimental) => {
                    // only when actually followed by `;`/`end` — `Platform`
                    // is a legal identifier
                    if matches!(
                        self.cursor.peek_second()?.map(|lexeme| lexeme.token),
                        Some(Token::Semicolon | Token::End)
                    ) {
                        self.cursor.advance()?;
                    } else {
                        return Ok(());
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Method directives after the signature's `;`: `virtual; override;
    /// message WM_X; stdcall; deprecated 'x';` … Every directive keyword is
    /// also a legal member name — a directive is only consumed when its
    /// following token confirms it (never `:` — that starts a field).
    fn consume_method_directives(&mut self) -> Result<Vec<Identifier>, ParseError> {
        let mut directives = Vec::new();
        loop {
            let Some(peeked) = self.cursor.peek()? else {
                return Ok(directives);
            };
            let token = peeked.token;
            // `external <lib> [name ..] [index ..] [delayed] [dependency ..];`
            // — a multi-part clause (RTL/Winapi imports), handled on its own.
            if token == Token::External {
                self.consume_external_clause(&mut directives)?;
                continue;
            }
            let takes_argument = matches!(token, Token::Message | Token::DispId);
            let is_marker = is_trailing_directive(token)
                || matches!(
                    token,
                    Token::Virtual
                        | Token::Dynamic
                        | Token::Override
                        | Token::Reintroduce
                        | Token::Abstract
                        | Token::Final
                        | Token::Static
                        // `procedure Foo; forward;` — forward declaration
                        | Token::Forward
                );
            let register_convention = token == Token::Ident
                && self.cursor.text(peeked).eq_ignore_ascii_case("register");
            if !takes_argument && !is_marker && !register_convention {
                return Ok(directives);
            }
            let second = self.cursor.peek_second()?.map(|lexeme| lexeme.token);
            if takes_argument {
                // `message WM_USER;` — but `Message: string;` is a FIELD
                if second == Some(Token::Colon) {
                    return Ok(directives);
                }
                let key = {
                    let text = self.cursor.text(peeked);
                    self.cursor.state().context.intern_key(text)
                };
                self.cursor.advance()?;
                self.expression_span(&[Token::Semicolon])?;
                self.expect_keyword(Token::Semicolon, "';'")?;
                directives.push(key);
                continue;
            }
            match second {
                Some(Token::Semicolon) => {
                    let key = {
                        let text = self.cursor.text(peeked);
                        self.cursor.state().context.intern_key(text)
                    };
                    self.cursor.advance()?;
                    self.cursor.advance()?;
                    directives.push(key);
                }
                Some(Token::StringLiteral) if token == Token::Deprecated => {
                    let key = {
                        let text = self.cursor.text(peeked);
                        self.cursor.state().context.intern_key(text)
                    };
                    self.cursor.advance()?;
                    self.cursor.advance()?;
                    self.expect_keyword(Token::Semicolon, "';'")?;
                    directives.push(key);
                }
                _ => return Ok(directives),
            }
        }
    }

    /// `external ['libname'] [name <expr>] [index <expr>] [delayed]
    /// [dependency <expr>];` — the import directive on RTL/Winapi routines.
    /// The library name and sub-clause arguments are constant expressions,
    /// captured as spans; the clause is consumed through its terminating `;`.
    /// `external` may also appear bare (`external;`).
    fn consume_external_clause(
        &mut self,
        directives: &mut Vec<Identifier>,
    ) -> Result<(), ParseError> {
        // sub-keyword tokens that end the library-name expression
        const CLAUSE_STOPS: &[Token] =
            &[Token::Semicolon, Token::Name, Token::Index, Token::Delayed];

        let external_lexeme = self.cursor.advance()?.expect("caller peeked 'external'");
        directives.push({
            let text = self.cursor.text(external_lexeme);
            self.cursor.state().context.intern_key(text)
        });

        // optional library-name expression
        if !matches!(self.peek_token()?, Some(Token::Semicolon) | None)
            && !CLAUSE_STOPS.contains(&self.peek_token()?.expect("checked"))
            && !self.peek_is_dependency()?
        {
            self.expression_span(CLAUSE_STOPS)?;
        }

        loop {
            match self.peek_token()? {
                Some(Token::Name) | Some(Token::Index) => {
                    let key = {
                        let lexeme = self.cursor.peek()?.expect("peeked");
                        let text = self.cursor.text(lexeme);
                        self.cursor.state().context.intern_key(text)
                    };
                    self.cursor.advance()?;
                    self.expression_span(CLAUSE_STOPS)?;
                    directives.push(key);
                }
                Some(Token::Delayed) => {
                    let key = {
                        let lexeme = self.cursor.peek()?.expect("peeked");
                        let text = self.cursor.text(lexeme);
                        self.cursor.state().context.intern_key(text)
                    };
                    self.cursor.advance()?;
                    directives.push(key);
                }
                _ if self.peek_is_dependency()? => {
                    self.cursor.advance()?; // 'dependency'
                    self.expression_span(&[Token::Semicolon])?;
                }
                _ => break,
            }
        }
        self.expect_keyword(Token::Semicolon, "';' ending external clause")?;
        Ok(())
    }

    /// `dependency` has no dedicated token (plain `Ident`) — text match.
    fn peek_is_dependency(&mut self) -> Result<bool, ParseError> {
        match self.cursor.peek()? {
            Some(lexeme) => Ok(lexeme.token == Token::Ident
                && self.cursor.text(lexeme).eq_ignore_ascii_case("dependency")),
            None => Ok(false),
        }
    }

    // ─── Uses clauses ────────────────────────────────────────────────────

    fn optional_uses(&mut self) -> Result<Option<UsesDeclarations>, ParseError> {
        if self.peek_token()? == Some(Token::Uses) {
            Ok(Some(self.parse_uses_list(true)?))
        } else {
            Ok(None)
        }
    }

    /// Parses `uses`/`contains` followed by `name [in 'path'] (, ...)* ;`.
    /// `record_imports`: uses entries feed the state's import list (package
    /// `contains` members are project parts, not imports — false there).
    fn parse_uses_list(&mut self, record_imports: bool) -> Result<UsesDeclarations, ParseError> {
        let keyword = self
            .cursor
            .advance()?
            .expect("caller peeked the keyword");
        let mut uses = Vec::new();
        loop {
            let name = self.qualified_name()?;
            let source_file = if self.peek_token()? == Some(Token::In) {
                self.cursor.advance()?;
                let literal = self.cursor.expect(Token::StringLiteral)?;
                let unquoted = unquote_string_literal(self.cursor.text(literal));
                Some(InClause {
                    path: self.cursor.state().context.intern(&unquoted),
                    location: literal.location,
                })
            } else {
                None
            };
            if record_imports {
                self.cursor.state_mut().record_import(name.key);
            }
            uses.push(UsedUnit { name, source_file });

            let found = self.cursor.advance()?;
            match found.map(|lexeme| lexeme.token) {
                Some(Token::Comma) => continue,
                Some(Token::Semicolon) => break,
                _ => {
                    return Err(ParseError::Unexpected {
                        expected: "',' or ';' in uses clause",
                        found,
                    });
                }
            }
        }
        Ok(UsesDeclarations {
            uses,
            location: keyword.location,
        })
    }

    /// `Foo` or `Winapi.Windows` — parts may be context-sensitive keywords
    /// (`Winapi` lexes as a calling-convention keyword). Interned twice:
    /// display track as written, lookup track case-folded.
    fn qualified_name(&mut self) -> Result<QualifiedName, ParseError> {
        let first = self.expect_identifier_like()?;
        let mut dotted = self.identifier_text(first).to_string();
        let mut last = first;

        while self.peek_token()? == Some(Token::Dot) {
            self.cursor.advance()?;
            let part = self.expect_identifier_like()?;
            dotted.push('.');
            dotted.push_str(self.identifier_text(part));
            last = part;
        }

        let name = self.cursor.state().context.intern(&dotted);
        let key = self.cursor.state().context.intern_key(&dotted);
        let location = if first.location.file == last.location.file {
            CodeLocation {
                file: first.location.file,
                span: Span {
                    start: first.location.span.start,
                    end: last.location.span.end,
                },
            }
        } else {
            first.location // name split across an include boundary — first part wins
        };
        Ok(QualifiedName {
            name,
            key,
            location,
        })
    }

    fn expect_identifier_like(&mut self) -> Result<Lexeme, ParseError> {
        match self.cursor.advance()? {
            Some(lexeme) if lexeme.token.can_be_identifier() => Ok(lexeme),
            found => Err(ParseError::Unexpected {
                expected: "identifier",
                found,
            }),
        }
    }

    // ─── Skipping (structure not parsed yet in this slice) ───────────────

    fn skip_until_semicolon(&mut self) -> Result<(), ParseError> {
        loop {
            match self.advance_token()? {
                Some(Token::Semicolon) => return Ok(()),
                Some(_) => continue,
                None => {
                    return Err(ParseError::Unexpected {
                        expected: "';'",
                        found: None,
                    });
                }
            }
        }
    }

    /// Consume everything up to and including `target` (top-level scan;
    /// `implementation` is a reserved word, so a plain token scan is safe).
    /// Currently unused — a coarse resync helper kept for the error-tolerant
    /// recovery path (ledger #10/#39), which resyncs at declaration boundaries;
    /// a future coarser fallback (skip to `implementation`/`end`) would use this.
    #[allow(dead_code)]
    fn skip_to_token(&mut self, target: Token) -> Result<(), ParseError> {
        loop {
            match self.cursor.advance()? {
                Some(lexeme) if lexeme.token == target => return Ok(()),
                Some(_) => continue,
                None => {
                    return Err(ParseError::Unexpected {
                        expected: "'implementation'",
                        found: None,
                    });
                }
            }
        }
    }

    /// Drain to EOF so directive-structure errors (unterminated `{$IFDEF}`)
    /// still surface for the not-yet-parsed remainder of the file.
    fn skip_rest(&mut self) -> Result<(), ParseError> {
        while self.cursor.advance()?.is_some() {}
        Ok(())
    }

    fn peek_token(&mut self) -> Result<Option<Token>, ParseError> {
        Ok(self.cursor.peek()?.map(|lexeme| lexeme.token))
    }

    fn advance_token(&mut self) -> Result<Option<Token>, ParseError> {
        Ok(self.cursor.advance()?.map(|lexeme| lexeme.token))
    }

    fn expect_keyword(&mut self, token: Token, expected: &'static str) -> Result<(), ParseError> {
        match self.cursor.advance()? {
            Some(lexeme) if lexeme.token == token => Ok(()),
            found => Err(ParseError::Unexpected { expected, found }),
        }
    }
}

/// Merge two locations into one span. Locations in different files (include
/// boundary) keep the first — same convention as `qualified_name`.
/// Attach captured attributes to a freshly-produced member. `NestedType`
/// carries its attributes on the inner `InterfaceDeclaration`.
fn set_member_attributes(member: &mut Member, attributes: Vec<Attribute>) {
    match member {
        Member::Field {
            attributes: slot, ..
        } => *slot = attributes,
        Member::Method(method) => method.attributes = attributes,
        Member::Property(property) => property.attributes = attributes,
        Member::NestedType(declaration) => declaration.attributes = attributes,
        Member::NestedConst {
            attributes: slot, ..
        } => *slot = attributes,
    }
}

/// Collect a type's flattened members as (member key → simple member-type key)
/// pairs, for own-unit scoped `Declared(TFoo.Bar[.Sub])` (SESSION.md ledger
/// #19). The member-type key (`None` for complex/anonymous types) enables
/// nested own-type walks. Nested types are listed as members but not recursed
/// into (they own their own member space). Mirrors the interface index's
/// member flattening and its `simple_type_key` rule (bare `Reference` only).
/// May a type of this shape inherit members from a base — OR otherwise carry a
/// member surface we do not flatten here? `true` for every class and interface
/// (a class implicitly descends from `TObject`, an interface from `IInterface`,
/// and either may name explicit possibly-cross-unit ancestors), AND `true` for
/// alias/distinct/class-reference shapes that redirect to another type whose
/// member surface lives elsewhere: a bare `Reference` alias (`TFoo = TBar`)
/// inherits the aliased type's ENTIRE member surface (including its direct
/// members), a `Distinct` type (`T = type Integer`) likewise, and a
/// `ClassReference` (`class of T`) exposes `T`'s class-level members. For all of
/// these the member set we can see here is not authoritative, so a member absent
/// from the direct declarations must degrade to Unknown, never a confident
/// false. Only genuinely ancestor-less, self-contained shapes (records, enums,
/// sets, subranges, pointers, routine types, …) carry no unseen member space and
/// keep the confident `false`. Mirrors `unit_meta::type_can_inherit`; drives the
/// "member absent from direct members → Unknown, not false" rule for own-unit
/// scoped `Declared` (SESSION.md ledger #19).
fn type_can_inherit(type_expression: &TypeExpression) -> bool {
    matches!(
        type_expression,
        TypeExpression::Class(_)
            | TypeExpression::Interface(_)
            | TypeExpression::Reference { .. }
            | TypeExpression::Distinct(_)
            | TypeExpression::ClassReference(_)
            // Forward declarations: the member surface is completed elsewhere
            // and is not knowable from the forward alone → a missing member
            // must degrade to Unknown, never a confident false (#19).
            | TypeExpression::ForwardClass
            | TypeExpression::ForwardInterface
            | TypeExpression::ForwardDispInterface
    )
}

fn type_member_entries(
    type_expression: &TypeExpression,
) -> Vec<(Identifier, Option<Identifier>)> {
    fn simple_type_key(type_expression: &TypeExpression) -> Option<Identifier> {
        match type_expression {
            TypeExpression::Reference { name, .. } => Some(name.key),
            _ => None,
        }
    }
    fn from_members(source: &[Member], out: &mut Vec<(Identifier, Option<Identifier>)>) {
        for member in source {
            match member {
                Member::Field {
                    names, field_type, ..
                } => {
                    let type_key = simple_type_key(field_type);
                    out.extend(names.iter().map(|name| (name.key, type_key)));
                }
                Member::Method(method) => out.push((
                    method.name.key,
                    method.routine.return_type.as_ref().and_then(simple_type_key),
                )),
                Member::Property(property) => out.push((
                    property.name.key,
                    property.property_type.as_ref().and_then(simple_type_key),
                )),
                Member::NestedType(declaration) => out.push((declaration.name.key, None)),
                Member::NestedConst { name, .. } => out.push((name.key, None)),
            }
        }
    }
    fn from_variant(
        variant_part: &VariantPart,
        out: &mut Vec<(Identifier, Option<Identifier>)>,
    ) {
        for arm in &variant_part.arms {
            from_members(&arm.fields, out);
            if let Some(nested) = &arm.nested {
                from_variant(nested, out);
            }
        }
    }
    let mut out = Vec::new();
    match type_expression {
        TypeExpression::Class(class_type) => {
            for section in &class_type.sections {
                from_members(&section.members, &mut out);
            }
        }
        TypeExpression::Record(structured) => {
            for section in &structured.sections {
                from_members(&section.members, &mut out);
            }
            if let Some(variant_part) = &structured.variant_part {
                from_variant(variant_part, &mut out);
            }
        }
        TypeExpression::Interface(interface_type) => {
            from_members(&interface_type.members, &mut out);
        }
        _ => {}
    }
    out
}

fn join_locations(first: CodeLocation, last: CodeLocation) -> CodeLocation {
    if first.file == last.file {
        CodeLocation {
            file: first.file,
            span: Span {
                start: first.span.start,
                end: last.span.end,
            },
        }
    } else {
        first
    }
}

/// `$FF` hex, `%101` binary, `&17` octal, decimal; `_` separators allowed.
/// Returns [`ConstantValue::Int`] when the value fits `i64`, else
/// [`ConstantValue::UInt`] when it fits `u64` (`$FFFFFFFFFFFFFFFF`, values in
/// the `i64::MAX+1 ..= u64::MAX` range — Delphi's `UInt64`). A value that fits
/// NEITHER (or malformed digits) yields `None` (Unknown) — never a bit-cast to
/// a wrong negative `i64`, which would be silent corruption (L6).
fn parse_integer_literal(text: &str) -> Option<crate::unit_cache::ConstantValue> {
    use crate::unit_cache::ConstantValue;
    let text = text.replace('_', "");
    let (radix, digits) = match text.as_bytes().first()? {
        b'$' => (16, &text[1..]),
        b'%' => (2, &text[1..]),
        b'&' => (8, &text[1..]),
        _ => (10, text.as_str()),
    };
    if let Ok(value) = i64::from_str_radix(digits, radix) {
        return Some(ConstantValue::Int(value));
    }
    // Overflowed i64 — retry as u64 (unsigned literals like $FFFFFFFFFFFFFFFF).
    // Still-too-big → None (Unknown), never a wrong number.
    u64::from_str_radix(digits, radix).ok().map(ConstantValue::UInt)
}

/// `#13` decimal or `#$0D` hex character code.
fn parse_character_code(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('#')?;
    match digits.strip_prefix('$') {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => digits.parse().ok(),
    }
}

/// Tokens that may follow a declaration's `;` as standalone directives
/// (`; stdcall; deprecated;`). Each is only treated as a directive when the
/// token after it confirms it (see `consume_trailing_directives`).
fn is_trailing_directive(token: Token) -> bool {
    matches!(
        token,
        Token::StdCall
            | Token::SafeCall
            | Token::CDecl
            | Token::Pascal
            | Token::WinApi
            | Token::VarArgs
            | Token::Near
            | Token::Far
            | Token::Local
            | Token::Overload
            | Token::Inline
            | Token::Assembler
            | Token::Export
            | Token::Deprecated
            | Token::Platform
            | Token::Experimental
            | Token::Library
            | Token::Delayed
            | Token::Unsafe
            | Token::Static
    )
}

/// `'Main.pas'` → `Main.pas`; embedded `''` unescaped.
fn unquote_string_literal(literal: &str) -> String {
    literal
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .unwrap_or(literal)
        .replace("''", "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DefineSet, Interner, SwitchState, TargetPlatform};
    use crate::unit_cache::UnitCache;
    use std::collections::HashMap;

    fn test_context() -> Arc<ProjectContext> {
        Arc::new(ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths: Vec::new(),
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        })
    }

    fn parse(source: &str) -> Source {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual("test.pas", source);
        parse_file(&arena, context, file).unwrap()
    }

    /// Parse a virtual source and return the FULL outcome (diagnostics,
    /// recovered flag, …) — for the error-tolerant-recovery tests.
    fn parse_outcome(source: &str) -> ParseOutcome {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual("test.pas", source);
        parse_file_full(&arena, context, file, None).unwrap()
    }

    /// The interface declaration display names of a parsed outcome's unit.
    fn outcome_declaration_names(outcome: &ParseOutcome) -> Vec<String> {
        let Some(Source::Unit(unit)) = outcome.source.present() else {
            panic!("expected a unit");
        };
        unit.interface_declarations
            .iter()
            .map(|declaration| crate::globals::resolve(declaration.name.name).to_string())
            .collect()
    }

    fn resolve(_context: &ProjectContext, name: crate::context::Identifier) -> String {
        crate::globals::resolve(name).to_string()
    }

    fn uses_names(context: &ProjectContext, uses: &Option<UsesDeclarations>) -> Vec<String> {
        uses.as_ref()
            .map(|list| {
                list.uses
                    .iter()
                    .map(|used| resolve(context, used.name.name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The whole AST tree serializes (bincode) and deserializes back through
    /// the global interner + arena, with names re-resolving and NO raw
    /// Spur/FileId integers on the wire. Uses a real disk file so the FileId
    /// paths re-`register` on load.
    #[test]
    fn full_ast_serde_round_trip_holds_no_raw_integers() {
        let directory = std::env::temp_dir().join("delphi_parser_ast_serde");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Roundtrip.pas");
        std::fs::write(
            &path,
            "unit Roundtrip;\ninterface\n\
             type TWidget = class\n  FCount: Integer;\n  procedure Draw;\nend;\n\
             const MaxWidgets = 7;\n\
             implementation\nend.",
        )
        .unwrap();

        // parse through the GLOBAL arena so serialized FileIds resolve back
        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.load(&path).unwrap();
        let source = parse_file(arena, context.clone(), file).unwrap();

        let bytes = bincode::serialize(&source).unwrap();

        // No raw identifier/file integers: the interned strings and the path
        // are present as text instead.
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Roundtrip"));
        assert!(text.contains("TWidget"));
        assert!(text.contains("MaxWidgets"));
        assert!(text.contains("Roundtrip.pas"));

        let restored: Source = bincode::deserialize(&bytes).unwrap();
        let Source::Unit(unit) = restored else {
            panic!("expected unit");
        };
        assert_eq!(crate::globals::resolve(unit.name.name), "Roundtrip");
        let names: Vec<_> = unit
            .interface_declarations
            .iter()
            .map(|declaration| crate::globals::resolve(declaration.name.name))
            .collect();
        assert!(names.contains(&"TWidget"));
        assert!(names.contains(&"MaxWidgets"));
        // a location survived: its span text is recoverable from the arena
        let widget = &unit.interface_declarations[0];
        assert_eq!(
            crate::globals::arena()
                .try_location_text(widget.name.location)
                .unwrap(),
            "TWidget"
        );
    }

    #[test]
    fn unit_header_and_uses() {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual(
            "test.pas",
            "unit Foo.Bar deprecated;\n\
             interface\n\
             uses System.SysUtils, Winapi.Windows;\n\
             implementation\n\
             uses System.Classes;\n\
             end.",
        );
        let Source::Unit(unit) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected unit");
        };
        assert_eq!(resolve(&context, unit.name.name), "Foo.Bar");
        assert_eq!(
            uses_names(&context, &unit.interface_uses),
            ["System.SysUtils", "Winapi.Windows"]
        );
        assert_eq!(
            uses_names(&context, &unit.implementation_uses),
            ["System.Classes"]
        );
    }

    #[test]
    fn unit_without_uses() {
        let Source::Unit(unit) = parse("unit Plain; interface implementation end.") else {
            panic!("expected unit");
        };
        assert!(unit.interface_uses.is_none());
        assert!(unit.implementation_uses.is_none());
    }

    /// Parse through the GLOBAL arena so location spans resolve back to text
    /// via `crate::globals::arena()` in the assertions below.
    fn expect_unit(source: &str) -> Unit {
        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.insert_virtual("attr_test.pas", source);
        match parse_file(arena, context, file).unwrap() {
            Source::Unit(unit) => unit,
            _ => panic!("expected unit"),
        }
    }

    fn attribute_names(attributes: &[Attribute]) -> Vec<String> {
        attributes
            .iter()
            .map(|attribute| crate::globals::resolve(attribute.name.name).to_string())
            .collect()
    }

    #[test]
    fn attributes_captured_at_declaration_member_parameter() {
        // Declaration-level (`[Foo] type`), member-level (field / method /
        // property), parameter-level (`[ref]`), stacked (`[A][B]`) and
        // comma-grouped (`[A, B]`) — all must be CAPTURED, not skipped (#16).
        let unit = expect_unit(
            "unit Attr;\ninterface\n\
             [Entity('t')] [Table]\n\
             type TFoo = class\n\
               [Weak] FValue: Integer;\n\
               [Test, Ignore] procedure Run(const [ref] Item: TBar);\n\
               [Column('id')] property Id: Integer read FValue;\n\
             end;\n\
             implementation\nend.",
        );
        let declaration = &unit.interface_declarations[0];
        // stacked [Entity('t')][Table] on the type declaration
        assert_eq!(attribute_names(&declaration.attributes), ["Entity", "Table"]);
        // argument list is a SPAN, present for Entity, absent for Table
        assert!(declaration.attributes[0].arguments.is_some());
        assert!(declaration.attributes[1].arguments.is_none());
        assert_eq!(
            crate::globals::arena()
                .try_location_text(declaration.attributes[0].arguments.unwrap())
                .unwrap(),
            "('t')"
        );

        let Some(TypeExpression::Class(class_type)) = declaration.type_expression.as_ref() else {
            panic!("expected class");
        };
        let members = &class_type.sections[0].members;
        let Member::Field { attributes, .. } = &members[0] else {
            panic!("expected field");
        };
        assert_eq!(attribute_names(attributes), ["Weak"]);

        let Member::Method(method) = &members[1] else {
            panic!("expected method");
        };
        // comma-grouped [Test, Ignore]
        assert_eq!(attribute_names(&method.attributes), ["Test", "Ignore"]);
        // parameter attribute [ref]
        let parameter = &method.routine.parameters[0];
        assert_eq!(attribute_names(&parameter.attributes), ["ref"]);

        let Member::Property(property) = &members[2] else {
            panic!("expected property");
        };
        assert_eq!(attribute_names(&property.attributes), ["Column"]);
        // read target still parsed (existing behaviour preserved)
        assert_eq!(
            crate::globals::resolve(property.read_target.as_ref().unwrap().name),
            "FValue"
        );
    }

    #[test]
    fn attribute_name_preserves_case_and_dotted_form() {
        // Name captured AS WRITTEN, dual-track: display keeps case, dotted
        // names stay whole; no `Attribute`-suffix normalization (#16 note).
        let unit = expect_unit(
            "unit A;\ninterface\n\
             [Xml.Serializable] type TFoo = class end;\n\
             implementation\nend.",
        );
        let attribute = &unit.interface_declarations[0].attributes[0];
        assert_eq!(crate::globals::resolve(attribute.name.name), "Xml.Serializable");
        assert_eq!(
            crate::globals::resolve(attribute.name.key),
            "XML.SERIALIZABLE"
        );
    }

    #[test]
    fn attribute_before_implementation_is_dropped_with_diagnostic() {
        // `[Foo]` right before `implementation` has no declaration to attach
        // to (invalid Delphi). Dropping it is correct, but the drop must be
        // surfaced as a diagnostic, never silently swallowed (ledger #32).
        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.insert_virtual(
            "dangling_attr.pas",
            "unit A;\ninterface\n\
             type TFoo = class end;\n\
             [Dangling]\n\
             implementation\nend.",
        );
        let outcome = parse_file_full(arena, context, file, None).unwrap();
        let Some(Source::Unit(unit)) = outcome.source.present() else {
            panic!("expected unit");
        };
        // the valid declaration still parsed, WITHOUT the dangling attribute
        assert_eq!(unit.interface_declarations.len(), 1);
        assert!(unit.interface_declarations[0].attributes.is_empty());
        // the discard is reported, not silent
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("no declaration to attach")),
            "dropped dangling attribute must leave a diagnostic: {:?}",
            outcome.diagnostics
        );
    }

    #[test]
    fn nested_brackets_in_attribute_arguments_do_not_close_early() {
        // Balanced tolerance: `[...]` and `(...)` inside the argument list must
        // not terminate the attribute group prematurely.
        let unit = expect_unit(
            "unit A;\ninterface\n\
             [Values([1, 2], (3))] type TFoo = class end;\n\
             implementation\nend.",
        );
        let attribute = &unit.interface_declarations[0].attributes[0];
        assert_eq!(crate::globals::resolve(attribute.name.name), "Values");
        assert_eq!(
            crate::globals::arena()
                .try_location_text(attribute.arguments.unwrap())
                .unwrap(),
            "([1, 2], (3))"
        );
    }

    #[test]
    fn conditional_uses_entry() {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual(
            "test.pas",
            "unit U; interface uses A{$IFDEF NEVER}, B{$ENDIF}, C; implementation end.",
        );
        let Source::Unit(unit) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected unit");
        };
        assert_eq!(uses_names(&context, &unit.interface_uses), ["A", "C"]);
    }

    #[test]
    fn program_with_in_clauses() {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual(
            "test.dpr",
            "program Demo;\nuses Vcl.Forms, Main in 'Main.pas' {MainForm}, Utils in '..\\shared\\Utils.pas';\nbegin\nend.",
        );
        let Source::Program(program) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected program");
        };
        assert_eq!(resolve(&context, program.name.name), "Demo");
        let uses = program.uses.as_ref().unwrap();
        assert_eq!(uses.uses.len(), 3);
        assert_eq!(
            resolve(&context, uses.uses[1].source_file.as_ref().unwrap().path),
            "Main.pas"
        );
        assert_eq!(
            resolve(&context, uses.uses[2].source_file.as_ref().unwrap().path),
            r"..\shared\Utils.pas"
        );
    }

    #[test]
    fn package_requires_contains() {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual(
            "test.dpk",
            "package MyPack;\nrequires rtl, vcl;\ncontains PackUnit in 'PackUnit.pas';\nend.",
        );
        let Source::Package(package) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected package");
        };
        assert_eq!(resolve(&context, package.name.name), "MyPack");
        let requires: Vec<String> = package
            .requires
            .iter()
            .map(|name| resolve(&context, name.name))
            .collect();
        assert_eq!(requires, ["rtl", "vcl"]);
        assert_eq!(package.contains.as_ref().unwrap().uses.len(), 1);
    }

    fn declaration_names(source: &str) -> Vec<(DeclarationKind, String)> {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual("test.pas", source);
        let Source::Unit(unit) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected unit");
        };
        unit.interface_declarations
            .iter()
            .map(|declaration| {
                (
                    declaration.kind,
                    resolve(&context, declaration.name.name),
                )
            })
            .collect()
    }

    fn unit_source(interface_body: &str) -> String {
        format!("unit U;\ninterface\n{interface_body}\nimplementation\nend.")
    }

    #[test]
    fn escaped_identifier_declares_unescaped_symbol() {
        // H1: `&Type` is the reserved-word escape for identifier `Type`; the
        // declared symbol must be `Type`, not `&Type`, so references match.
        let source = unit_source("type &Type = Integer;\nconst &begin = 1;");
        assert_eq!(
            declaration_names(&source),
            [
                (DeclarationKind::Type, "Type".to_string()),
                (DeclarationKind::Const, "begin".to_string()),
            ]
        );
    }

    #[test]
    fn shallow_declarations_all_kinds() {
        use DeclarationKind::*;
        let source = unit_source(
            "type TAlias = System.Classes.TStringList;\n\
             const MaxThings = 100;\n\
             resourcestring SHello = 'hi';\n\
             var GCount, GTotal: Integer;\n\
             threadvar TlsSlot: Pointer;\n\
             procedure DoThing(Value: Integer = 5);\n\
             function GetThing: Integer;",
        );
        assert_eq!(
            declaration_names(&source),
            [
                (Type, "TAlias".to_string()),
                (Const, "MaxThings".to_string()),
                (ResourceString, "SHello".to_string()),
                (Var, "GCount".to_string()),
                (Var, "GTotal".to_string()),
                (ThreadVar, "TlsSlot".to_string()),
                (Procedure, "DoThing".to_string()),
                (Function, "GetThing".to_string()),
            ]
        );
    }

    #[test]
    fn nested_and_variant_records() {
        let source = unit_source(
            "type\n\
             TOuter = record\n\
               Inner: record A: Integer; end;\n\
               case Tag: Byte of\n\
                 0: (X: Integer);\n\
                 1: (Y: Double; Z: record Q: Byte; end);\n\
             end;\n\
             TAfter = Integer;",
        );
        let names = declaration_names(&source);
        assert_eq!(names[0].1, "TOuter");
        assert_eq!(names[1].1, "TAfter");
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn classes_forward_classref_helper_interface_guid() {
        let source = unit_source(
            "type\n\
             TThing = class;\n\
             TThingClass = class of TThing;\n\
             TThing = class(TObject)\n\
               strict private\n\
                 FValue: Integer;\n\
                 class var FShared: Integer;\n\
               public\n\
                 type TInner = record I: Integer; end;\n\
                 const InnerMax = 5;\n\
                 class function Make: TThing; static;\n\
                 property Value: Integer read FValue write FValue;\n\
             end;\n\
             TThingHelper = class helper for TThing\n\
               function Doubled: Integer;\n\
             end;\n\
             IThing = interface;\n\
             IThing = interface(IInterface)\n\
               ['{D3AF6B0E-1111-2222-3333-444455556666}']\n\
               function Get: Integer;\n\
             end;",
        );
        let names: Vec<String> = declaration_names(&source)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(
            names,
            ["TThing", "TThingClass", "TThing", "TThingHelper", "IThing", "IThing"]
        );
    }

    #[test]
    fn generics_with_constraints_and_fused_ge() {
        let source = unit_source(
            "type\n\
             TList<T: class, constructor> = class\n\
               procedure Add(Item: T);\n\
             end;\n\
             TPair<K; V: record> = record Key: K; Value: V; end;\n\
             TBox<T>= class Value: T; end;\n\
             TNested = Integer;",
        );
        let names: Vec<String> = declaration_names(&source)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["TList", "TPair", "TBox", "TNested"]);
    }

    #[test]
    fn procedure_types_and_trailing_conventions() {
        let source = unit_source(
            "type\n\
             TNotify = procedure(Sender: TObject) of object;\n\
             TCallback = function(X: Integer): Integer; stdcall;\n\
             var Hook: procedure; stdcall;\n\
             function Load(const Path: string): Boolean; overload; stdcall; deprecated 'use LoadEx';",
        );
        let names: Vec<String> = declaration_names(&source)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["TNotify", "TCallback", "Hook", "Load"]);
    }

    #[test]
    fn typed_constants_with_inner_semicolons() {
        let source = unit_source(
            "const\n\
             Origin: TPoint = (X: 0; Y: 0);\n\
             Table: array[0..1] of TPoint = ((X: 1; Y: 2), (X: 3; Y: 4));\n\
             Letters = ['a'..'z'];\n\
             Greeting = 'it''s';",
        );
        let names: Vec<String> = declaration_names(&source)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["Origin", "Table", "Letters", "Greeting"]);
    }

    #[test]
    fn context_keyword_names_not_eaten_as_directives() {
        // `platform`/`name` are context keywords AND portability directives;
        // as entry names they must survive
        let source = unit_source(
            "const\n\
             Name = 'x';\n\
             Platform = 2;\n\
             var Index: Integer;",
        );
        let names: Vec<String> = declaration_names(&source)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["Name", "Platform", "Index"]);
    }

    #[test]
    fn attributes_and_inline_records_in_vars() {
        let source = unit_source(
            "type\n\
             [Weak] TRef = class end;\n\
             var\n\
             [Volatile] Counter: Integer;\n\
             Anon: record A: Integer; end;\n\
             List: array of record B: Byte; end;",
        );
        let names: Vec<String> = declaration_names(&source)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["TRef", "Counter", "Anon", "List"]);
    }

    fn parse_first_type(source: &str) -> (Arc<ProjectContext>, TypeExpression) {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual("test.pas", source);
        let Source::Unit(unit) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected unit");
        };
        let declaration = unit
            .interface_declarations
            .into_iter()
            .find(|declaration| declaration.kind == DeclarationKind::Type)
            .expect("type declaration");
        (context, declaration.type_expression.expect("type expression"))
    }

    #[test]
    fn deep_class_structure() {
        let (context, type_expression) = parse_first_type(&unit_source(
            "type TThing = class(TBase, IThing)\n\
               strict private\n\
                 FValue, FOther: Integer;\n\
                 class var FShared: TList<Byte>;\n\
               public\n\
                 constructor Create(AOwner: TObject); overload;\n\
                 procedure Notify(Sender: TObject); virtual; message WM_USER;\n\
                 class function Make(const Name: string = 'x'): TThing; static;\n\
                 property Value: Integer read FValue write SetValue; \n\
                 property Items[Index: Integer]: Byte read GetItem; default;\n\
             end;",
        ));
        let TypeExpression::Class(class_type) = type_expression else {
            panic!("expected class");
        };
        assert_eq!(class_type.ancestors.len(), 2);
        assert_eq!(class_type.sections.len(), 3); // unspecified + strict private + public
        assert!(class_type.sections[1].strict);
        assert_eq!(class_type.sections[1].visibility, Visibility::Private);

        let private_members = &class_type.sections[1].members;
        let Member::Field { names, is_class_var, .. } = &private_members[0] else {
            panic!("field");
        };
        assert_eq!(names.len(), 2);
        assert!(!is_class_var);
        let Member::Field { is_class_var, field_type, .. } = &private_members[1] else {
            panic!("class var field");
        };
        assert!(is_class_var);
        let TypeExpression::Reference { type_arguments, .. } = field_type else {
            panic!("generic reference");
        };
        assert_eq!(type_arguments.len(), 1);

        let public_members = &class_type.sections[2].members;
        let Member::Method(create) = &public_members[0] else {
            panic!("constructor");
        };
        assert_eq!(create.routine.kind, RoutineKind::Constructor);
        assert_eq!(create.routine.parameters.len(), 1);
        assert!(create.directives.contains(&context.intern_key("overload")));

        let Member::Method(notify) = &public_members[1] else {
            panic!("method");
        };
        assert!(notify.directives.contains(&context.intern_key("virtual")));
        assert!(notify.directives.contains(&context.intern_key("message")));

        let Member::Method(make) = &public_members[2] else {
            panic!("class function");
        };
        assert!(make.is_class_method);
        assert_eq!(make.routine.parameters[0].modifier, ParameterModifier::Const);
        assert!(make.routine.parameters[0].default.is_some());
        assert!(make.routine.return_type.is_some());

        let Member::Property(value_property) = &public_members[3] else {
            panic!("property");
        };
        assert_eq!(
            value_property.read_target.as_ref().unwrap().key,
            context.intern_key("FVALUE")
        );
        assert_eq!(
            value_property.write_target.as_ref().unwrap().key,
            context.intern_key("SetValue")
        );
        assert!(!value_property.is_default);

        let Member::Property(items_property) = &public_members[4] else {
            panic!("indexed property");
        };
        assert_eq!(items_property.index_parameters.len(), 1);
        assert!(items_property.is_default);
    }

    #[test]
    fn deep_record_with_variant_part_and_enum() {
        let (_, type_expression) = parse_first_type(&unit_source(
            "type TShape = record\n\
               Kind: (skDot, skLine = 4);\n\
               case Tag: Byte of\n\
                 0: (X: Integer);\n\
                 1: (P: array[0..3] of Byte;\n\
                     case Inner: Word of\n\
                       2: (Q: Double));\n\
             end;",
        ));
        let TypeExpression::Record(record_type) = type_expression else {
            panic!("expected record");
        };
        let Member::Field { field_type, .. } = &record_type.sections[0].members[0] else {
            panic!("enum field");
        };
        let TypeExpression::Enumeration(members) = field_type else {
            panic!("enumeration");
        };
        assert_eq!(members.len(), 2);
        assert!(members[1].explicit_value.is_some());

        let variant_part = record_type.variant_part.as_ref().expect("variant part");
        assert!(variant_part.selector_name.is_some());
        assert_eq!(variant_part.arms.len(), 2);
        let Member::Field { field_type, .. } = &variant_part.arms[1].fields[0] else {
            panic!("array field");
        };
        let TypeExpression::Array { bounds, .. } = field_type else {
            panic!("array");
        };
        assert!(bounds.is_some());
        assert!(variant_part.arms[1].nested.is_some());
    }

    #[test]
    fn deep_interface_and_type_forms() {
        let (context, type_expression) = parse_first_type(&unit_source(
            "type IThing = interface(IInterface)\n\
               ['{D3AF6B0E-1111-2222-3333-444455556666}']\n\
               function Count: Integer;\n\
               property Value: Integer read Count;\n\
             end;\n\
             type PInt = ^Integer;\n\
             TRef = class of TThing;\n\
             TCallback = reference to function(X: Integer): Boolean;\n\
             TDist = type string;\n\
             TShort = string[40];",
        ));
        let TypeExpression::Interface(interface_type) = type_expression else {
            panic!("expected interface");
        };
        assert!(interface_type.guid.is_some());
        assert_eq!(interface_type.ancestors.len(), 1);
        assert_eq!(interface_type.members.len(), 2);
        let _ = context;
    }

    #[test]
    fn variable_types_are_structured() {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual(
            "test.pas",
            &unit_source("var Buffers: array of TThing;\nHook: procedure of object; stdcall;"),
        );
        let Source::Unit(unit) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected unit");
        };
        let buffers = &unit.interface_declarations[0];
        let Some(TypeExpression::Array { bounds: None, .. }) = &buffers.type_expression else {
            panic!("dynamic array");
        };
        let hook = &unit.interface_declarations[1];
        let Some(TypeExpression::Routine(routine)) = &hook.type_expression else {
            panic!("procedure type");
        };
        assert!(routine.of_object);
    }

    #[test]
    fn qualified_name_lookup_keys_fold_case() {
        let arena = SourceArena::new();
        let context = test_context();
        let file = arena.insert_virtual(
            "test.pas",
            "unit U; interface uses SysUtils; implementation uses SYSUTILS; end.",
        );
        let Source::Unit(unit) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected unit");
        };
        let interface_entry = &unit.interface_uses.as_ref().unwrap().uses[0].name;
        let implementation_entry = &unit.implementation_uses.as_ref().unwrap().uses[0].name;
        // display spellings differ, lookup keys are identical
        assert_ne!(interface_entry.name, implementation_entry.name);
        assert_eq!(interface_entry.key, implementation_entry.key);
        assert_eq!(interface_entry.key, context.intern_key("sysutils"));
    }

    // ─── Regression tests for the parser-grammar review cluster ──────────
    // Each proves one landed fix (H3-H7, M9, L3-L5, class const/type) parses
    // and captures what the artifact layer relies on, so a silent regression
    // fails a portable `cargo test`.

    /// H6: a procedural-type field carries its trailing calling convention
    /// with NO separating `;` (`Hook: function(...): BOOL stdcall;`). The bug
    /// misread the convention as the next field's name and lost the real next
    /// member. Assert every field parses in order — including the one AFTER a
    /// procedural field — across stdcall/cdecl/register/`of object`.
    #[test]
    fn h6_procedural_field_convention_preserves_next_member() {
        let (context, type_expression) = parse_first_type(&unit_source(
            "type THooks = class(TObject)\n\
               StdHook: function(var P: TPoint): BOOL stdcall;\n\
               CdeclHook: function(X: Integer): Integer cdecl;\n\
               RegHook: procedure(Sender: TObject) register;\n\
               EventHook: procedure(Sender: TObject) of object;\n\
               After: Integer;\n\
             end;",
        ));
        let TypeExpression::Class(class_type) = type_expression else {
            panic!("expected class");
        };
        let members = &class_type.sections[0].members;
        let mut field_names = Vec::new();
        for member in members {
            if let Member::Field { names, .. } = member {
                for name in names {
                    field_names.push(resolve(&context, name.name));
                }
            }
        }
        // `After` present ⇒ no convention was swallowed as a field name
        assert_eq!(
            field_names,
            ["StdHook", "CdeclHook", "RegHook", "EventHook", "After"]
        );
        let Member::Field { field_type, .. } = &members[3] else {
            panic!("EventHook field");
        };
        let TypeExpression::Routine(routine) = field_type else {
            panic!("procedure type");
        };
        assert!(routine.of_object);
    }

    /// H7: method resolution clause `procedure IFoo.Execute = DoExecute;`
    /// inside a class body — the qualified interface method and the
    /// implementing target are both captured, and the ordinary method after
    /// it still parses.
    #[test]
    fn h7_method_resolution_clause_captured() {
        let (context, type_expression) = parse_first_type(&unit_source(
            "type TFoo = class(TInterfacedObject, IFoo)\n\
               procedure IFoo.Execute = DoExecute;\n\
               procedure DoExecute;\n\
             end;",
        ));
        let TypeExpression::Class(class_type) = type_expression else {
            panic!("expected class");
        };
        let members = &class_type.sections[0].members;
        let Member::Method(resolution) = &members[0] else {
            panic!("resolution clause method");
        };
        assert_eq!(resolve(&context, resolution.name.name), "IFoo.Execute");
        let target = resolution
            .resolution_target
            .as_ref()
            .expect("resolution target");
        assert_eq!(resolve(&context, target.name), "DoExecute");
        assert!(matches!(&members[1], Member::Method(_)));
    }

    /// H3: interface-section external/forward routine directives —
    /// `external 'lib' name '...'`, `external <expr> index N`, and `forward`.
    #[test]
    fn h3_external_and_forward_routine_directives() {
        let names: Vec<String> = declaration_names(&unit_source(
            "function GetLastError: DWORD; stdcall; external 'kernel32.dll' name 'GetLastError';\n\
             procedure Foo; external SomeLib index 5;\n\
             procedure Bar; forward;\n\
             function Baz: Integer;",
        ))
        .into_iter()
        .map(|(_, name)| name)
        .collect();
        assert_eq!(names, ["GetLastError", "Foo", "Bar", "Baz"]);
    }

    /// H4: generic ancestor on an interface (`interface(IEnumerable<T>)`) and
    /// a class-helper `for` target carrying generic args
    /// (`class helper for TList<Integer>`).
    #[test]
    fn h4_generic_ancestor_and_helper_target() {
        let names: Vec<String> = declaration_names(&unit_source(
            "type\n\
             IList<T> = interface(IEnumerable<T>)\n\
               function GetCount: Integer;\n\
             end;\n\
             TFoo = class helper for TList<Integer>\n\
               procedure Extra;\n\
             end;",
        ))
        .into_iter()
        .map(|(_, name)| name)
        .collect();
        assert_eq!(names, ["IList", "TFoo"]);
    }

    /// H5: pathological nesting must degrade to `Err(RecursionLimit)`, never
    /// overflow the native stack. Runs on a freshly spawned thread with the
    /// DEFAULT stack size (same class as a cargo-test worker) so the point —
    /// graceful degradation on an ordinary stack — is actually exercised. A
    /// shallow-but-nontrivial nesting on the same path must still parse.
    #[test]
    fn h5_recursion_limit_degrades_gracefully_on_default_stack() {
        fn parse_owned(source: String) -> Result<ParseOutcome, ParseError> {
            let arena = SourceArena::new();
            let context = test_context();
            let file = arena.insert_virtual("test.pas", &source);
            parse_file_full(&arena, context, file, None)
        }

        // N well past MAX_PARSE_DEPTH; the guard aborts the DESCENT at the limit
        // (`ParseError::RecursionLimit`), so the native stack only ever grows to
        // ~depth-limit frames regardless of how large N is. With declaration-
        // level recovery (task 5, #10) that per-declaration error no longer
        // aborts the whole unit: the broken deep declaration is DROPPED with a
        // diagnostic and the parse is flagged `recovered`. The stack-safety
        // guarantee (no overflow) is unchanged — the recursion guard still fires;
        // recovery merely resyncs past it instead of failing the unit.
        let deep_pointer = unit_source(&format!("type TDeep = {}Integer;", "^".repeat(400)));
        let deep_generic = unit_source(&format!(
            "type TDeep = {}Integer{};",
            "TA<".repeat(400),
            ">".repeat(400)
        ));
        let shallow_pointer = unit_source(&format!("type TShallow = {}Integer;", "^".repeat(8)));
        let shallow_generic = unit_source(&format!(
            "type TShallow = {}Integer{};",
            "TA<".repeat(8),
            ">".repeat(8)
        ));

        let handle = std::thread::Builder::new()
            .name("recursion-guard".to_string())
            .spawn(move || {
                // Deep nesting: the unit still parses (no stack overflow, no
                // panic), the deep declaration is recovered-away (flagged), and
                // it emits NO clean symbol — never a bogus TDeep.
                let deep_pointer_outcome =
                    parse_owned(deep_pointer).expect("deep pointer must not overflow the stack");
                assert!(deep_pointer_outcome.recovered, "deep pointer chain must recover");
                assert!(outcome_declaration_names(&deep_pointer_outcome).is_empty());

                let deep_generic_outcome =
                    parse_owned(deep_generic).expect("deep generic must not overflow the stack");
                assert!(deep_generic_outcome.recovered, "deep generic nesting must recover");
                assert!(outcome_declaration_names(&deep_generic_outcome).is_empty());

                // Shallow nesting: well under the limit, parses cleanly (NOT
                // flagged recovered), yielding the real symbol.
                let shallow_pointer_outcome =
                    parse_owned(shallow_pointer).expect("shallow pointer must parse");
                assert!(!shallow_pointer_outcome.recovered);
                assert!(
                    outcome_declaration_names(&shallow_pointer_outcome)
                        .contains(&"TShallow".to_string())
                );

                let shallow_generic_outcome =
                    parse_owned(shallow_generic).expect("shallow generic must parse");
                assert!(!shallow_generic_outcome.recovered);
                assert!(
                    outcome_declaration_names(&shallow_generic_outcome)
                        .contains(&"TShallow".to_string())
                );
            })
            .expect("spawn recursion-guard thread");
        handle
            .join()
            .expect("recursion-guard thread must not overflow the stack");
    }

    /// M9: generic type parameters + their constraint clauses are captured on
    /// the declaration (`TFoo<T: class, constructor; U>`), with the
    /// constraint kept as a source span.
    #[test]
    fn m9_generic_parameters_and_constraint_spans_captured() {
        let arena = SourceArena::new();
        let context = test_context();
        let source = unit_source(
            "type TFoo<T: class, constructor; U> = class\n\
               procedure Use(Value: T);\n\
             end;",
        );
        let file = arena.insert_virtual("test.pas", &source);
        let Source::Unit(unit) = parse_file(&arena, context.clone(), file).unwrap() else {
            panic!("expected unit");
        };
        let declaration = &unit.interface_declarations[0];
        assert_eq!(resolve(&context, declaration.name.name), "TFoo");
        assert_eq!(declaration.generic_parameters.len(), 2);
        let t_parameter = &declaration.generic_parameters[0];
        let u_parameter = &declaration.generic_parameters[1];
        assert_eq!(resolve(&context, t_parameter.name.name), "T");
        assert_eq!(resolve(&context, u_parameter.name.name), "U");
        let constraint_span = t_parameter.constraints.expect("T constraint span");
        let constraint_text = arena.location_text(constraint_span).to_lowercase();
        assert!(constraint_text.contains("class"), "constraint span text: {constraint_text}");
        assert!(constraint_text.contains("constructor"), "constraint span text: {constraint_text}");
        assert!(u_parameter.constraints.is_none(), "U is unconstrained");
    }

    /// L3: fused `>=` in type-argument position on a generic typed const
    /// (`TArray<Byte>=(1,2,3)`) and a nested generic instantiation
    /// (`TDictionary<string,TArray<Byte>>`).
    #[test]
    fn l3_generic_typed_const_and_nested_type_arguments() {
        let names: Vec<String> = declaration_names(&unit_source(
            "const Data: TArray<Byte>=(1, 2, 3);\n\
             var Lookup: TDictionary<string,TArray<Byte>>;",
        ))
        .into_iter()
        .map(|(_, name)| name)
        .collect();
        assert_eq!(names, ["Data", "Lookup"]);
    }

    /// L4: `class property` sets the `is_class` flag; a plain property does
    /// not.
    #[test]
    fn l4_class_property_is_class_flag() {
        let (_, type_expression) = parse_first_type(&unit_source(
            "type TFoo = class\n\
               class property Shared: Integer read FShared;\n\
               property Instance: Integer read FInstance;\n\
             end;",
        ));
        let TypeExpression::Class(class_type) = type_expression else {
            panic!("expected class");
        };
        let members = &class_type.sections[0].members;
        let Member::Property(shared) = &members[0] else {
            panic!("class property");
        };
        assert!(shared.is_class, "class property must set is_class");
        let Member::Property(instance) = &members[1] else {
            panic!("instance property");
        };
        assert!(!instance.is_class, "plain property is not class-level");
    }

    /// L5: a generic instantiation on a class ancestor is retained
    /// (`class(TList<Integer>)`), not discarded.
    #[test]
    fn l5_ancestor_retains_generic_arguments() {
        let (context, type_expression) =
            parse_first_type(&unit_source("type TFoo = class(TList<Integer>)\nend;"));
        let TypeExpression::Class(class_type) = type_expression else {
            panic!("expected class");
        };
        assert_eq!(class_type.ancestors.len(), 1);
        let ancestor = &class_type.ancestors[0];
        assert_eq!(resolve(&context, ancestor.name.name), "TList");
        assert_eq!(
            ancestor.type_arguments.len(),
            1,
            "generic instantiation argument on the ancestor is retained"
        );
    }

    /// Discovered fix: `class const` and `class type` members inside a class
    /// body (valid Delphi; previously aborted the unit).
    #[test]
    fn class_const_and_class_type_members_parse() {
        let (_, type_expression) = parse_first_type(&unit_source(
            "type TFoo = class\n\
               class const MaxItems = 10;\n\
               class type TInner = Integer;\n\
               procedure Use;\n\
             end;",
        ));
        let TypeExpression::Class(class_type) = type_expression else {
            panic!("expected class");
        };
        let members = &class_type.sections[0].members;
        assert!(
            members.iter().any(|member| matches!(member, Member::NestedConst { .. })),
            "class const member"
        );
        assert!(
            members.iter().any(|member| matches!(member, Member::NestedType(_))),
            "class type member"
        );
        assert!(
            members.iter().any(|member| matches!(member, Member::Method(_))),
            "trailing ordinary method still parses"
        );
    }

    /// Machine-specific stress test: parse EVERY .pas under src\core with
    /// the real be.dproj context; report failures with location. Run:
    ///   cargo test --features local-tests core_tree -- --nocapture
    #[cfg(feature = "local-tests")]
    #[test]
    fn core_tree_parses() {
        let profile = crate::context::CompilerProfile {
            compiler_version: 36.0,
            rtl_version: None,
            defines: [
                "VER360", "MSWINDOWS", "WIN32", "CPU386", "CPUX86", "CPU32BITS",
                "UNICODE", "CONDITIONALEXPRESSIONS", "ASSEMBLER",
            ]
            .map(String::from)
            .to_vec(),
        };
        let context = Arc::new(
            ProjectContext::from_dproj(
                r"C:\Delphi\VSS\Intern\be\D12\be.dproj",
                None,
                None,
                &profile,
            )
            .unwrap(),
        );

        fn collect_pas(directory: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(directory) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_pas(&path, found);
                } else if path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pas"))
                {
                    found.push(path);
                }
            }
        }
        let mut files = Vec::new();
        collect_pas(std::path::Path::new(r"C:\Delphi\VSS\Intern\src\core"), &mut files);
        assert!(!files.is_empty(), "no sources found");

        let arena = SourceArena::new();
        let mut failures = Vec::new();
        for path in &files {
            let file = match arena.load(path) {
                Ok(file) => file,
                Err(error) => {
                    failures.push(format!("{}: read: {}", path.display(), error.message));
                    continue;
                }
            };
            if let Err(error) = parse_file(&arena, context.clone(), file) {
                let detail = match &error {
                    ParseError::Unexpected { expected, found } => match found {
                        Some(lexeme) => format!(
                            "expected {expected}, found {:?} '{}' at offset {} in {}",
                            lexeme.token,
                            arena.location_text(lexeme.location),
                            lexeme.location.span.start,
                            arena.path(lexeme.location.file).display()
                        ),
                        None => format!("expected {expected}, found EOF"),
                    },
                    ParseError::Cursor(crate::token_cursor::CursorError::Lex(location)) => {
                        let content = arena.loaded_content(location.file);
                        let start = location.span.start as usize;
                        format!(
                            "unlexable input {:?} at offset {start} in {} (context: {:?})",
                            &content[start..(start + 1).min(content.len())],
                            arena.path(location.file).display(),
                            &content[start.saturating_sub(20)..(start + 20).min(content.len())]
                        )
                    }
                    other => format!("{other:?}"),
                };
                failures.push(format!("{}: {detail}", path.display()));
            }
        }
        println!(
            "parsed {} files, {} failures",
            files.len(),
            failures.len()
        );
        for failure in failures.iter().take(30) {
            println!("  {failure}");
        }
        assert!(
            failures.is_empty(),
            "{} of {} files failed",
            failures.len(),
            files.len()
        );
    }

    /// Categorize a [`ParseError`] into a stable, low-cardinality bucket key
    /// plus a detailed one-line example (with file path + offset). Used by the
    /// stress harness to group failures by PATTERN.
    #[cfg(feature = "local-tests")]
    fn classify_parse_error(arena: &SourceArena, error: &ParseError) -> (String, String) {
        use crate::token_cursor::CursorError;
        match error {
            ParseError::Unexpected { expected, found } => match found {
                Some(lexeme) => (
                    format!("Unexpected: expected {expected}, found {:?}", lexeme.token),
                    format!(
                        "expected {expected}, found {:?} '{}' at offset {} in {}",
                        lexeme.token,
                        arena.location_text(lexeme.location),
                        lexeme.location.span.start,
                        arena.path(lexeme.location.file).display()
                    ),
                ),
                None => (
                    format!("Unexpected: expected {expected}, found EOF"),
                    format!("expected {expected}, found EOF"),
                ),
            },
            ParseError::Cursor(CursorError::Lex(location)) => {
                let content = arena.loaded_content(location.file);
                let start = location.span.start as usize;
                (
                    "Lex: unrecognized input".to_string(),
                    format!(
                        "unlexable input {:?} at offset {start} in {} (context: {:?})",
                        &content[start..(start + 1).min(content.len())],
                        arena.path(location.file).display(),
                        &content[start.saturating_sub(30)..(start + 30).min(content.len())]
                    ),
                )
            }
            ParseError::Cursor(CursorError::Directive(directive_error)) => (
                "Cursor: directive structure error".to_string(),
                format!("directive error: {directive_error:?}"),
            ),
            ParseError::Cursor(CursorError::Condition { location, error }) => (
                "Cursor: {$IF} condition eval".to_string(),
                format!(
                    "condition error {error:?} at offset {} in {}",
                    location.span.start,
                    arena.path(location.file).display()
                ),
            ),
            ParseError::Cursor(CursorError::Include { location, error }) => (
                "Cursor: {$I} include not resolved".to_string(),
                format!(
                    "include error '{}' at offset {} in {}",
                    error.message,
                    location.span.start,
                    arena.path(location.file).display()
                ),
            ),
            ParseError::Cursor(CursorError::IncludeDepthExceeded(location)) => (
                "Cursor: include depth exceeded".to_string(),
                format!(
                    "include depth exceeded at offset {} in {}",
                    location.span.start,
                    arena.path(location.file).display()
                ),
            ),
            ParseError::Cursor(CursorError::UnexpectedToken { expected, found }) => match found {
                Some(lexeme) => (
                    format!("CursorUnexpectedToken: expected {expected:?}, found {:?}", lexeme.token),
                    format!(
                        "cursor expected {expected:?}, found {:?} '{}' at offset {} in {}",
                        lexeme.token,
                        arena.location_text(lexeme.location),
                        lexeme.location.span.start,
                        arena.path(lexeme.location.file).display()
                    ),
                ),
                None => (
                    format!("CursorUnexpectedToken: expected {expected:?}, found EOF"),
                    format!("cursor expected {expected:?}, found EOF"),
                ),
            },
            ParseError::FileReadError(read_error) => (
                "FileReadError (parse)".to_string(),
                format!("file read error: {}", read_error.message),
            ),
            ParseError::RecursionLimit => (
                "RecursionLimit: grammar nesting too deep".to_string(),
                "grammar nesting exceeded MAX_PARSE_DEPTH".to_string(),
            ),
        }
    }

    /// Machine-specific EMPIRICAL STRESS HARNESS: walk the ENTIRE
    /// `C:\Delphi\VSS\Intern\src` tree (recursively) for .pas/.dpr/.dpk and
    /// parse each through the FULL pipeline (parse + artifact production via
    /// the interface loader / lazy imports). Panics are caught per file so one
    /// bad unit never aborts the run. This is a DISCOVERY harness: it prints a
    /// ranked, categorized failure report. It ALSO carries a companion
    /// regression guard at the very end (after the full report prints) that
    /// fails if the units-OK count drops below the real-source baseline, so a
    /// parse-count regression surfaces automatically in CI. Run:
    ///   cargo test --features local-tests stress_full_src_tree -- --nocapture --test-threads=1
    #[cfg(feature = "local-tests")]
    #[test]
    fn stress_full_src_tree() {
        use std::collections::BTreeMap;
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::sync::{Arc as StdArc, Mutex};

        let profile = crate::context::CompilerProfile {
            compiler_version: 36.0,
            rtl_version: None,
            defines: [
                "VER360", "MSWINDOWS", "WIN32", "CPU386", "CPUX86", "CPU32BITS",
                "UNICODE", "CONDITIONALEXPRESSIONS", "ASSEMBLER",
            ]
            .map(String::from)
            .to_vec(),
        };
        // CONFIG (REVIEW.md): the harness context must come from a dproj ACTIVE
        // config, not a hand-written define list — otherwise a project-specific
        // define (`BE_CORE_D11_USES`) is missing and an intentionally-
        // noncompilable dead branch (German prose in `be.core.gui.dpk`'s `.inc`)
        // gets reached. A package/program (`.dpk`/`.dpr`) has its OWN sibling
        // `<stem>.dproj` (e.g. `be.core.gui.dproj` next to `be.core.gui.dpk`)
        // whose active-config defines flow through `from_dproj`; unit `.pas`
        // files have no sibling dproj and correctly use the top-level be.dproj
        // context. So each file is parsed under the dproj that governs it.
        let default_dproj = std::path::PathBuf::from(r"C:\Delphi\VSS\Intern\be\D12\be.dproj");
        let build_context = |dproj: &std::path::Path| {
            Arc::new(
                ProjectContext::from_dproj(dproj, None, None, &profile)
                    .unwrap_or_else(|error| panic!("from_dproj({}) failed: {error:?}", dproj.display())),
            )
        };
        // Per-dproj context cache (built lazily as governing dprojs are seen).
        let mut contexts: std::collections::HashMap<std::path::PathBuf, Arc<ProjectContext>> =
            std::collections::HashMap::new();
        contexts.insert(default_dproj.clone(), build_context(&default_dproj));

        // The dproj that governs a source file: its sibling `<stem>.dproj` when
        // one exists on disk (packages/programs), else the top-level be.dproj.
        let governing_dproj = |path: &std::path::Path| -> std::path::PathBuf {
            let sibling = path.with_extension("dproj");
            if sibling.is_file() {
                sibling
            } else {
                default_dproj.clone()
            }
        };

        fn collect_sources(directory: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(directory) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_sources(&path, found);
                } else if path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("pas")
                            || extension.eq_ignore_ascii_case("dpr")
                            || extension.eq_ignore_ascii_case("dpk")
                    })
                {
                    found.push(path);
                }
            }
        }
        let mut files = Vec::new();
        collect_sources(std::path::Path::new(r"C:\Delphi\VSS\Intern\src"), &mut files);
        files.sort();
        assert!(!files.is_empty(), "no sources found");

        // Capture panic location+message from the hook (single-threaded run).
        let panic_slot: StdArc<Mutex<Option<String>>> = StdArc::new(Mutex::new(None));
        let hook_slot = panic_slot.clone();
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "<unknown location>".to_string());
            *hook_slot.lock().unwrap() = Some(format!("{location} — {payload}"));
        }));

        // (category key, one example line) — first example per category wins.
        let mut categories: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let mut all_failures: Vec<String> = Vec::new();
        let mut record = |category: String, example: String| {
            let entry = categories
                .entry(category)
                .or_insert_with(|| (0, example.clone()));
            entry.0 += 1;
            all_failures.push(example);
        };

        let arena = crate::globals::arena();
        let index = Arc::new(crate::watcher::ReverseDependencyIndex::default());
        let mut parsed_ok = 0usize;
        let mut non_unit_ok = 0usize;

        for path in &files {
            let display = path.display().to_string();
            *panic_slot.lock().unwrap() = None;

            let file = match arena.load(path) {
                Ok(file) => file,
                Err(error) => {
                    record(
                        "FileReadError (arena load)".to_string(),
                        format!("{display}: read: {}", error.message),
                    );
                    continue;
                }
            };

            // Resolve (build once, then reuse) the context of the dproj that
            // governs this file, so package/program dead branches see their own
            // project defines (CONFIG).
            let dproj = governing_dproj(path);
            let context_ref = contexts
                .entry(dproj.clone())
                .or_insert_with(|| build_context(&dproj))
                .clone();
            let context_ref = &context_ref;
            let index_ref = &index;
            let result = catch_unwind(AssertUnwindSafe(|| {
                let loader = crate::unit_loader::UnitLoader::new(
                    arena,
                    context_ref.clone(),
                    Some(index_ref.clone()),
                );
                crate::pipeline::parse_and_cache(arena, context_ref, file, Some(loader))
            }));

            match result {
                Ok(Ok((outcome, artifact))) => {
                    if artifact.is_some() {
                        parsed_ok += 1;
                    } else {
                        // program/library/package: parses fine, no interface
                        non_unit_ok += 1;
                        let _ = &outcome;
                    }
                }
                Ok(Err(error)) => {
                    let (category, detail) = classify_parse_error(arena, &error);
                    record(category, format!("{display}: {detail}"));
                }
                Err(_) => {
                    let message = panic_slot
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| "<panic, no hook message>".to_string());
                    // key the category by the panic site (file:line), not the
                    // per-file message, so the same crash site groups together
                    let site = message
                        .split(" — ")
                        .next()
                        .unwrap_or("<unknown>")
                        .to_string();
                    record(
                        format!("PANIC @ {site}"),
                        format!("{display}: PANIC {message}"),
                    );
                }
            }
        }

        std::panic::set_hook(previous_hook);

        let total_failures: usize = categories.values().map(|(count, _)| count).sum();
        println!("\n======== stress_full_src_tree ========");
        println!("root: C:\\Delphi\\VSS\\Intern\\src");
        println!("total source files (.pas/.dpr/.dpk): {}", files.len());
        println!("parsed OK (unit + artifact):          {parsed_ok}");
        println!("parsed OK (program/lib/pkg, no artifact): {non_unit_ok}");
        println!("failures (parse errors + panics):     {total_failures}");
        println!("distinct failure categories:          {}", categories.len());

        // Ranked categories by frequency (descending).
        let mut ranked: Vec<(&String, &(usize, String))> = categories.iter().collect();
        ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        println!("\n---- categories ranked by frequency ----");
        for (category, (count, example)) in &ranked {
            println!("[{count:>4}] {category}");
            println!("        e.g. {example}");
        }

        println!("\n---- first 40 distinct failures ----");
        for failure in all_failures.iter().take(40) {
            println!("  {failure}");
        }
        println!("======== end stress_full_src_tree ========\n");

        // Companion REGRESSION GUARD (NIT from review): the discovery report
        // above always prints first, so this stays a discovery harness — but a
        // drop below the established real-source baseline now FAILS CI instead
        // of silently regressing the parse count. 464 = current units-OK
        // baseline (SESSION.md it. work log). Raise this number when the
        // baseline genuinely improves; never lower it to paper over a
        // regression.
        const PARSE_OK_BASELINE: usize = 464;
        assert!(
            parsed_ok >= PARSE_OK_BASELINE,
            "parse-count regression: parsed_ok = {parsed_ok}, expected >= \
             {PARSE_OK_BASELINE} (real-source baseline). A unit that used to \
             parse now fails — inspect the ranked categories above."
        );
        // CONFIG: with each file parsed under its GOVERNING dproj's active
        // config (packages/programs via their sibling `<stem>.dproj`), the whole
        // tree is now clean — the last failure (`be.core.gui.dpk`, an
        // intentionally-noncompilable dead branch reached only for lack of the
        // project define `BE_CORE_D11_USES`) is resolved by using the real
        // defines. Assert ZERO failures so a genuine grammar regression can no
        // longer hide behind a "known dead-branch" excuse. If a real
        // intentionally-noncompilable case ever appears, exclude it EXPLICITLY
        // here with a comment and adjust this count — never silently.
        assert_eq!(
            total_failures, 0,
            "the src tree must parse cleanly under each file's governing dproj \
             config; {total_failures} failure(s) — inspect the categories above."
        );
    }

    /// Machine-specific: parse a real production unit against the real
    /// be.dproj context. Run: cargo test --features local-tests real_unit
    #[cfg(feature = "local-tests")]
    #[test]
    fn real_unit_shallow_parse() {
        let profile = crate::context::CompilerProfile {
            compiler_version: 36.0,
            rtl_version: None,
            defines: [
                "VER360", "MSWINDOWS", "WIN32", "CPU386", "CPUX86", "CPU32BITS",
                "UNICODE", "CONDITIONALEXPRESSIONS", "ASSEMBLER",
            ]
            .map(String::from)
            .to_vec(),
        };
        let context = Arc::new(
            ProjectContext::from_dproj(
                r"C:\Delphi\VSS\Intern\be\D12\be.dproj",
                None,
                None,
                &profile,
            )
            .unwrap(),
        );
        let arena = SourceArena::new();
        let source = parse_path(
            &arena,
            context.clone(),
            r"C:\Delphi\VSS\Intern\src\core\system\beDBVersion.pas",
        )
        .unwrap();
        let Source::Unit(unit) = source else {
            panic!("expected unit");
        };
        println!(
            "unit {} — {} interface declarations:",
            crate::globals::resolve(unit.name.name),
            unit.interface_declarations.len()
        );
        for declaration in &unit.interface_declarations {
            println!(
                "  {:?} {}",
                declaration.kind,
                crate::globals::resolve(declaration.name.name)
            );
        }
        assert!(!unit.interface_declarations.is_empty());
    }

    #[test]
    fn garbage_start_is_an_error() {
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", "begin end.");
        assert!(matches!(
            parse_file(&arena, test_context(), file),
            Err(ParseError::Unexpected { .. })
        ));
    }

    // ─── Deliverable B: error-tolerant declaration-level recovery (#10) ───

    #[test]
    fn broken_middle_declaration_still_yields_the_others_with_a_diagnostic() {
        // The 2nd declaration is malformed (`= = = ;` is not a type body). The
        // 1st and 3rd must still be extracted, a diagnostic recorded for the
        // broken region, and the parse flagged recovered (never persisted clean).
        let outcome = parse_outcome(
            "unit U;\ninterface\n\
             type TGood = class end;\n\
             type TBroken = = = = ;\n\
             const AfterBroken = 5;\n\
             implementation\nend.",
        );
        let names = outcome_declaration_names(&outcome);
        assert!(names.contains(&"TGood".to_string()), "1st decl survives: {names:?}");
        assert!(
            names.contains(&"AfterBroken".to_string()),
            "3rd decl survives after resync: {names:?}"
        );
        // the broken 2nd contributed a diagnostic, not a bogus symbol
        assert!(!outcome.diagnostics.is_empty(), "the broken decl leaves a diagnostic");
        assert!(outcome.recovered, "the parse is flagged recovered");
        // NO bogus symbol for the broken region: TBroken must not appear as a
        // clean declaration (its body never parsed).
        assert!(
            !names.contains(&"TBroken".to_string()),
            "a broken declaration must NOT emit a symbol: {names:?}"
        );
    }

    #[test]
    fn broken_class_member_does_not_mint_a_phantom_top_level_symbol() {
        // THE North Star regression. A class MEMBER is malformed (`Field Integer`
        // — the `:` is missing), so parsing fails INSIDE the class body. The
        // error unwinds to the interface loop with the cursor still inside the
        // unbalanced class. A naive resync would stop at the FIRST section-like
        // keyword — the following genuine top-level `procedure Beta;` — and the
        // interface loop would then re-dispatch it… but WORSE, member starters
        // inside the broken body (`function`/`procedure`/`type`…) would be minted
        // as bogus top-level symbols. The nesting-aware resync must consume the
        // ENTIRE broken class (through its closing `end;`) before any keyword is
        // treated as a top-level boundary.
        let outcome = parse_outcome(
            "unit U;\ninterface\n\
             type TBad = class\n\
               Field Integer;\n\
               procedure Member;\n\
             end;\n\
             procedure Beta;\n\
             implementation\nend.",
        );
        let names = outcome_declaration_names(&outcome);
        // (a) NO phantom. Neither the broken class's member (`Member`) nor the
        // broken type name leaks as a top-level symbol. Beta IS genuinely a
        // top-level declaration and legitimately survives (see (b) below), so
        // the phantom assertion targets the member name that must never escape.
        assert!(
            !names.contains(&"Member".to_string()),
            "a class member must NEVER become a top-level symbol: {names:?}"
        );
        // The surviving top-level set is a SUBSET of the truly-declared names.
        let truly_declared = ["TBad", "Beta"];
        for name in &names {
            assert!(
                truly_declared.contains(&name.as_str()),
                "surviving symbol {name:?} is not a truly-declared top-level name: {names:?}"
            );
        }
        // (b) the genuine top-level declaration AFTER the broken type still
        // recovers — resync stopped exactly at the class's closing `end;`.
        assert!(
            names.contains(&"Beta".to_string()),
            "the genuine top-level decl after the broken type must recover: {names:?}"
        );
        // (c) the unit is flagged recovered (never persisted as clean).
        assert!(outcome.recovered, "the parse is flagged recovered");
        assert!(!outcome.diagnostics.is_empty(), "the broken member leaves a diagnostic");
    }

    #[test]
    fn broken_record_member_does_not_mint_a_phantom_top_level_symbol() {
        // Same trap, `record` flavour, and the broken member sits BEFORE a
        // section keyword inside the body (`function`), which must be swallowed
        // as part of the broken region, not re-dispatched at top level.
        let outcome = parse_outcome(
            "unit U;\ninterface\n\
             type TRec = record\n\
               Value Integer;\n\
               function Inner: Integer;\n\
             end;\n\
             const AfterRec = 7;\n\
             implementation\nend.",
        );
        let names = outcome_declaration_names(&outcome);
        assert!(
            !names.contains(&"Inner".to_string()),
            "a record member must NEVER become a top-level symbol: {names:?}"
        );
        assert!(
            names.contains(&"AfterRec".to_string()),
            "the genuine top-level decl after the broken record must recover: {names:?}"
        );
        for name in &names {
            assert!(
                ["TRec", "AfterRec"].contains(&name.as_str()),
                "surviving symbol {name:?} is not truly-declared: {names:?}"
            );
        }
        assert!(outcome.recovered);
    }

    #[test]
    fn resync_end_inside_paren_at_block_depth_zero_does_not_underflow() {
        // Ledger #10 regression: a malformed top-level header fails BEFORE any
        // class/record body is opened, so the resync starts with block_depth==0.
        // The broken tail then places an `end` INSIDE a resync-tracked `(...)`
        // group: at that point block_depth==0 but !at_top_level (paren_depth>0).
        // The balancing-`end` arm must NOT fire here — an unguarded `block_depth
        // -= 1` would underflow: panic ('attempt to subtract with overflow') in
        // debug/tests, or wrap to usize::MAX in release (resync runs to EOF and
        // silently drops every following top-level declaration). The depth-0
        // `end` is junk inside the broken region and must be discarded instead.
        //
        // A parse error must NEVER abort the unit or panic in recovery (#10):
        // the unit parses with recovery, is flagged `recovered`, and a genuine
        // following top-level declaration still appears with no phantom minted.
        let outcome = parse_outcome(
            "unit U;\ninterface\n\
             procedure ;x( a end );\n\
             procedure Genuine;\n\
             implementation\nend.",
        );
        let names = outcome_declaration_names(&outcome);
        // No panic reaching here already proves the underflow is closed.
        assert!(outcome.recovered, "the malformed header is recovered, not fatal");
        assert!(!outcome.diagnostics.is_empty(), "the broken header leaves a diagnostic");
        // The genuine following top-level declaration survives the resync.
        assert!(
            names.contains(&"Genuine".to_string()),
            "the genuine decl after the broken region must recover: {names:?}"
        );
        // No phantom: the junk tokens inside the broken region (`x`, `a`) must
        // never leak as top-level symbols.
        assert!(
            !names.contains(&"x".to_string()) && !names.contains(&"a".to_string()),
            "broken-region junk must never become a top-level symbol: {names:?}"
        );
    }

    #[test]
    fn lexer_error_in_active_declaration_recovers() {
        // A stray unlexable byte sequence inside an active interface region:
        // recovery skips to the next boundary and keeps the later declaration.
        let outcome = parse_outcome(
            "unit U;\ninterface\n\
             type TFirst = class end;\n\
             const Bad = @!#~^^^ ;\n\
             const Good = 1;\n\
             implementation\nend.",
        );
        let names = outcome_declaration_names(&outcome);
        assert!(names.contains(&"TFirst".to_string()));
        assert!(names.contains(&"Good".to_string()), "recovers to the later const: {names:?}");
        assert!(outcome.recovered);
        assert!(!outcome.diagnostics.is_empty());
    }

    #[test]
    fn recovery_terminates_on_pathological_input() {
        // Pathological: a long run of tokens that never forms a valid
        // declaration and never hits a boundary until EOF. Recovery must
        // TERMINATE (advance ≥1 token per resync), not loop. A wall-clock guard
        // proves no infinite loop.
        let mut pathological = String::from("unit U;\ninterface\ntype ");
        for _ in 0..5000 {
            pathological.push_str("^ ");
        }
        pathological.push_str("\nimplementation\nend.");

        let start = std::time::Instant::now();
        let outcome = parse_outcome(&pathological);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "recovery must terminate quickly, took {:?}",
            start.elapsed()
        );
        // it recovered (or cleanly reached implementation); either way no hang
        // and no panic. The `^^^…` never forms a symbol.
        assert!(outcome_declaration_names(&outcome).is_empty());
    }

    #[test]
    fn recursion_limit_recovery_resets_depth_for_later_declarations() {
        // A declaration that trips the recursion guard must NOT poison the
        // declarations after it: recovery resets the depth counter, so a good
        // declaration following the deep one still parses cleanly. (Regression
        // for the depth-counter leak on the `?`-unwound RecursionLimit path.)
        let outcome = parse_outcome(&format!(
            "unit U;\ninterface\n\
             type TDeep = {}Integer;\n\
             const AfterDeep = 42;\n\
             implementation\nend.",
            "^".repeat(400)
        ));
        let names = outcome_declaration_names(&outcome);
        assert!(outcome.recovered);
        assert!(
            names.contains(&"AfterDeep".to_string()),
            "the declaration after a recursion-limit recovery must still parse: {names:?}"
        );
        assert!(!names.contains(&"TDeep".to_string()));
    }

    #[test]
    fn clean_parse_is_not_flagged_recovered() {
        // A well-formed unit must NOT be flagged recovered (so it persists).
        let outcome = parse_outcome(
            "unit U;\ninterface\n\
             type TOk = class end;\n\
             const N = 1;\n\
             implementation\nend.",
        );
        assert!(!outcome.recovered);
        assert!(outcome.diagnostics.is_empty());
    }

    #[test]
    fn unterminated_conditional_is_not_swallowed_by_recovery() {
        // A directive-structure error (unterminated `{$IFDEF}`) is UNRECOVERABLE
        // — the conditional skeleton is broken, so the unit must still fail
        // rather than be silently "recovered". Guards against recovery masking a
        // corrupt token stream.
        let arena = SourceArena::new();
        let file = arena.insert_virtual(
            "test.pas",
            "unit U;\ninterface\n{$IFDEF FOO}\ntype TX = class end;\n\
             implementation\nend.",
        );
        let error = parse_file_full(&arena, test_context(), file, None)
            .err()
            .expect("unterminated conditional must propagate, not be recovered");
        assert!(
            matches!(error, ParseError::Cursor(CursorError::Directive(_))),
            "expected a directive-structure error, got: {error:?}"
        );
    }
}
