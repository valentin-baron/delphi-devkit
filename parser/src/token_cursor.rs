//! Directive-aware token cursor: the single pass over a unit's source.
//!
//! The grammar parser sees only significant tokens via [`TokenCursor::peek`] /
//! [`TokenCursor::advance`]. Everything directive-shaped is handled as a side
//! effect of cursor movement — conditional compilation ([`UnitParseState`] +
//! [`crate::if_eval`]), `{$DEFINE}`/`{$UNDEF}`, switch directives, and `{$I}`
//! include splicing (a stack of lexers over [`SourceArena`] buffers). Tokens
//! inside dead conditional branches are swallowed. Nothing is ever re-lexed.

use std::path::PathBuf;
use std::sync::Arc;

use logos::Logos;

use crate::context::{ProjectContext, SwitchFlags};
use crate::if_eval::{self, Condition, EvalError, StateResolver};
use crate::meta::{CodeLocation, FileId, Span};
use crate::parse_state::{ConditionalKind, DirectiveError, UnitParseState};
use crate::source::{FileReadError, SourceArena};
use crate::token::{Token, directive_inner_text};

const MAX_INCLUDE_DEPTH: usize = 32;

/// A significant token together with where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lexeme {
    pub token: Token,
    pub location: CodeLocation,
}

#[derive(Debug)]
pub enum CursorError {
    /// Unrecognised input bytes.
    Lex(CodeLocation),
    /// Conditional-directive structure error (dangling $ELSE, unterminated…).
    Directive(DirectiveError),
    /// `{$IF}`/`{$ELSEIF}` expression failed to parse or type-check.
    Condition {
        location: CodeLocation,
        error: EvalError,
    },
    /// `{$I file}` could not be resolved or read.
    Include {
        location: CodeLocation,
        error: FileReadError,
    },
    IncludeDepthExceeded(CodeLocation),
    UnexpectedToken {
        expected: Token,
        found: Option<Lexeme>,
    },
}

impl From<DirectiveError> for CursorError {
    fn from(error: DirectiveError) -> Self {
        CursorError::Directive(error)
    }
}

/// What to do when a `{$IF}` condition evaluates to [`Condition::Unknown`].
/// Either way a [`Diagnostic`] is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnknownConditionPolicy {
    #[default]
    AssumeFalse,
    AssumeTrue,
}

/// Non-fatal finding collected during cursor movement.
#[derive(Debug)]
pub struct Diagnostic {
    pub location: CodeLocation,
    pub message: String,
}

pub struct TokenCursor<'arena> {
    arena: &'arena SourceArena,
    state: UnitParseState,
    /// Lexer stack; last = innermost include. Kept in sync with the state's
    /// include stack (which only records provenance for diagnostics).
    lexers: Vec<(FileId, logos::Lexer<'arena, Token>)>,
    /// Lookahead buffer (at most 2). Peeking runs directive side effects
    /// early relative to grammar consumption — harmless, because directive
    /// state is driven by lexical order either way.
    peeked: std::collections::VecDeque<Lexeme>,
    unknown_policy: UnknownConditionPolicy,
    pub diagnostics: Vec<Diagnostic>,
    /// Location of the most recently produced significant token — a best-effort
    /// anchor for a diagnostic when an error carries no `Lexeme` of its own
    /// (used by error-tolerant recovery in the parser).
    last_location: CodeLocation,
}

impl<'arena> TokenCursor<'arena> {
    /// `file` must have been materialized in the arena (`load`/`insert_virtual`
    /// — not merely `register`ed); panics otherwise.
    pub fn new(arena: &'arena SourceArena, context: Arc<ProjectContext>, file: FileId) -> Self {
        let lexer = Token::lexer(arena.loaded_content(file));
        Self {
            arena,
            state: UnitParseState::new(context),
            lexers: vec![(file, lexer)],
            peeked: std::collections::VecDeque::new(),
            unknown_policy: UnknownConditionPolicy::default(),
            diagnostics: Vec::new(),
            last_location: CodeLocation {
                file,
                span: Span::new(0, 0),
            },
        }
    }

    /// Location of the most recently produced significant token (or the file
    /// start before any token). A best-effort diagnostic anchor for recovery.
    pub fn last_location(&self) -> CodeLocation {
        self.last_location
    }

    pub fn with_unknown_policy(mut self, policy: UnknownConditionPolicy) -> Self {
        self.unknown_policy = policy;
        self
    }

    pub fn state(&self) -> &UnitParseState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut UnitParseState {
        &mut self.state
    }

    /// Record a non-fatal parser finding. Lets the grammar layer surface a
    /// deliberate-but-lossy situation (e.g. an attribute discarded at a section
    /// boundary) instead of swallowing it silently.
    pub fn push_diagnostic(&mut self, location: CodeLocation, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            location,
            message: message.into(),
        });
    }

    /// Tear down into the final parse state + collected diagnostics
    /// (artifact building needs include stamps and dependencies).
    pub fn into_parts(self) -> (UnitParseState, Vec<Diagnostic>) {
        (self.state, self.diagnostics)
    }

    /// Source text of a lexeme, valid for the arena's lifetime.
    pub fn text(&self, lexeme: Lexeme) -> &'arena str {
        self.arena.text(lexeme.location.file, lexeme.location.span)
    }

    fn fill_lookahead(&mut self, count: usize) -> Result<(), CursorError> {
        while self.peeked.len() < count {
            match self.next_significant()? {
                Some(lexeme) => self.peeked.push_back(lexeme),
                None => break,
            }
        }
        Ok(())
    }

    /// Next significant token without consuming it. `None` = clean EOF.
    pub fn peek(&mut self) -> Result<Option<Lexeme>, CursorError> {
        self.fill_lookahead(1)?;
        Ok(self.peeked.front().copied())
    }

    /// The token AFTER the next one. Needed to disambiguate trailing routine
    /// directives from declarations whose name is a context keyword
    /// (`stdcall;` vs. `const Platform = 2;`).
    pub fn peek_second(&mut self) -> Result<Option<Lexeme>, CursorError> {
        self.fill_lookahead(2)?;
        Ok(self.peeked.get(1).copied())
    }

    /// Consume and return the next significant token. `None` = clean EOF.
    pub fn advance(&mut self) -> Result<Option<Lexeme>, CursorError> {
        let result = match self.peeked.pop_front() {
            Some(lexeme) => Ok(Some(lexeme)),
            None => self.next_significant(),
        };
        if let Ok(Some(lexeme)) = &result {
            self.last_location = lexeme.location;
        }
        result
    }

    /// Consume the next token, requiring an exact kind.
    pub fn expect(&mut self, token: Token) -> Result<Lexeme, CursorError> {
        match self.advance()? {
            Some(lexeme) if lexeme.token == token => Ok(lexeme),
            found => Err(CursorError::UnexpectedToken {
                expected: token,
                found,
            }),
        }
    }

    // ─── Core loop ───────────────────────────────────────────────────────

    fn next_significant(&mut self) -> Result<Option<Lexeme>, CursorError> {
        loop {
            let Some(raw) = self.pull_raw()? else {
                // main-file EOF: open conditional frames are an error
                self.state.finish()?;
                return Ok(None);
            };
            if raw.token.is_trivia() {
                continue;
            }
            if raw.token.is_directive() {
                self.handle_directive(raw)?;
                continue;
            }
            if !self.state.is_active() {
                // dead conditional branch: EVERYTHING is swallowed, including
                // unlexable bytes — `{$IFDEF X} Error: do not use! {$ENDIF}`
                // compile-breaker prose is a common Delphi idiom
                continue;
            }
            if raw.token == Token::Error {
                return Err(CursorError::Lex(raw.location));
            }
            return Ok(Some(raw));
        }
    }

    /// Next raw token from the innermost lexer, unwinding finished includes.
    fn pull_raw(&mut self) -> Result<Option<Lexeme>, CursorError> {
        loop {
            let Some((file, lexer)) = self.lexers.last_mut() else {
                return Ok(None);
            };
            let file = *file;
            match lexer.next() {
                Some(result) => {
                    let location = CodeLocation {
                        file,
                        span: Span::from(lexer.span()),
                    };
                    // lex failures become spanned Error tokens so the dead-
                    // branch filter in next_significant() can swallow them
                    let token = result.unwrap_or(Token::Error);
                    return Ok(Some(Lexeme { token, location }));
                }
                None => {
                    self.lexers.pop();
                    if self.lexers.is_empty() {
                        return Ok(None); // main file done
                    }
                    self.state.pop_include();
                }
            }
        }
    }

    // ─── Directive dispatch ──────────────────────────────────────────────

    fn handle_directive(&mut self, lexeme: Lexeme) -> Result<(), CursorError> {
        let inner = directive_inner_text(self.text(lexeme));
        let name_end = inner
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(inner.len());
        let name = &inner[..name_end];
        let argument = inner[name_end..].trim();
        let location = lexeme.location;

        match name.to_ascii_uppercase().as_str() {
            "IFDEF" => {
                let condition =
                    self.state.needs_condition() && self.lookup_define(argument);
                self.state
                    .push_conditional(ConditionalKind::IfDef, condition, location);
            }
            "IFNDEF" => {
                let condition =
                    self.state.needs_condition() && !self.lookup_define(argument);
                self.state
                    .push_conditional(ConditionalKind::IfNDef, condition, location);
            }
            "IF" => {
                let condition = if self.state.needs_condition() {
                    self.evaluate(argument, location)?
                } else {
                    false // dead branch: expression must not be evaluated
                };
                self.state
                    .push_conditional(ConditionalKind::If, condition, location);
            }
            "IFOPT" => {
                let condition = self.state.needs_condition() && self.check_ifopt(argument);
                self.state
                    .push_conditional(ConditionalKind::IfOpt, condition, location);
            }
            "ELSEIF" => {
                let condition = if self.state.elseif_needs_condition() {
                    self.evaluate(argument, location)?
                } else {
                    false
                };
                self.state.elseif_branch(condition, location)?;
            }
            "ELSE" => self.state.else_branch(location)?,
            "ENDIF" | "IFEND" => {
                self.state.pop_conditional(location)?;
            }
            "DEFINE" if self.state.is_active() => {
                let symbol = self.state.context.intern_key(first_identifier(argument));
                self.state.apply_define(symbol);
            }
            "UNDEF" if self.state.is_active() => {
                let symbol = self.state.context.intern_key(first_identifier(argument));
                self.state.apply_undef(symbol);
            }
            "I" | "INCLUDE" if self.state.is_active() => {
                // {$I+} / {$I-} is the IO-checks switch, not an include
                if let Some(enabled) = parse_plus_minus(argument) {
                    self.state
                        .switches
                        .flags
                        .set(SwitchFlags::IO_CHECKS, enabled);
                } else {
                    self.push_include(argument, location)?;
                }
            }
            _ if self.state.is_active() => self.apply_switch_directive(name, argument),
            _ => {} // dead branch: non-conditional directives are inert
        }
        Ok(())
    }

    fn lookup_define(&self, symbol: &str) -> bool {
        // `{$IFDEF FOO BAR}` tests FOO — dcc uses the first identifier only.
        let symbol = self.state.context.intern_key(first_identifier(symbol));
        self.state.is_defined(symbol)
    }

    fn evaluate(&mut self, expression: &str, location: CodeLocation) -> Result<bool, CursorError> {
        let mut resolver = StateResolver {
            state: &mut self.state,
            layout_depth: 0,
        };
        match if_eval::evaluate_condition(expression, &mut resolver) {
            Ok(Condition::True) => Ok(true),
            Ok(Condition::False) => Ok(false),
            Ok(Condition::Unknown) => {
                let assumed = matches!(self.unknown_policy, UnknownConditionPolicy::AssumeTrue);
                self.diagnostics.push(Diagnostic {
                    location,
                    message: format!(
                        "condition '{expression}' is not evaluable; assuming {assumed}"
                    ),
                });
                Ok(assumed)
            }
            Err(error) => Err(CursorError::Condition { location, error }),
        }
    }

    fn check_ifopt(&self, argument: &str) -> bool {
        let mut characters = argument.trim().chars();
        let (Some(letter), Some(sign)) = (characters.next(), characters.next()) else {
            return false;
        };
        let Some(flag) = switch_flag_for_letter(letter) else {
            return false;
        };
        self.state.switches.flags.contains(flag) == (sign == '+')
    }

    fn push_include(&mut self, argument: &str, location: CodeLocation) -> Result<(), CursorError> {
        if self.lexers.len() >= MAX_INCLUDE_DEPTH {
            return Err(CursorError::IncludeDepthExceeded(location));
        }

        let name = normalize_include_name(argument);
        // `{$I %VARIABLE%}` pseudo-include: splices a string literal, not a
        // file (compiler feature: %DATE%, %TIME%, environment variables)
        if let Some(inner) = name
            .strip_prefix('%')
            .and_then(|rest| rest.strip_suffix('%'))
        {
            return self.push_pseudo_include(inner, location);
        }

        let file = match self.resolve_include_path(name, location.file) {
            Ok(path) => self
                .arena
                .load(&path)
                .map_err(|error| CursorError::Include { location, error })?,
            Err(probed) => {
                return Err(CursorError::Include {
                    location,
                    error: crate::source::FileReadError {
                        path: PathBuf::from(name),
                        message: format!(
                            "include not found; probed: {}",
                            probed
                                .iter()
                                .map(|path| path.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    },
                });
            }
        };
        self.state.push_include(file, location);
        self.lexers
            .push((file, Token::lexer(self.arena.loaded_content(file))));
        Ok(())
    }

    /// `{$I %VAR%}`: environment variable → its value as a string literal;
    /// %DATE%/%TIME% → placeholder + diagnostic (build metadata, irrelevant
    /// for analysis); unknown → empty literal + diagnostic. The literal is
    /// spliced via a virtual one-token buffer.
    fn push_pseudo_include(
        &mut self,
        variable: &str,
        location: CodeLocation,
    ) -> Result<(), CursorError> {
        let upper = variable.to_uppercase();
        let value = match upper.as_str() {
            "DATE" | "TIME" | "DATETIME" => {
                self.diagnostics.push(Diagnostic {
                    location,
                    message: format!("{{$I %{variable}%}}: build timestamp replaced by placeholder"),
                });
                format!("<{upper}>")
            }
            _ => match std::env::var(variable) {
                Ok(value) => value,
                Err(_) => {
                    self.diagnostics.push(Diagnostic {
                        location,
                        message: format!(
                            "{{$I %{variable}%}}: variable not set, spliced empty string"
                        ),
                    });
                    String::new()
                }
            },
        };
        let literal = format!("'{}'", value.replace('\'', "''"));
        let file = self
            .arena
            .insert_virtual(format!("<pseudo-include %{variable}%>"), literal);
        self.state.push_include(file, location);
        self.lexers
            .push((file, Token::lexer(self.arena.loaded_content(file))));
        Ok(())
    }

    /// `{$I name}` resolution: quoted names allowed, `.pas` appended when no
    /// extension given (compiler behavior), relative paths tried against the
    /// including file's directory first, then the project search paths.
    /// `Err` carries every probed path for the error message.
    fn resolve_include_path(&self, name: &str, from: FileId) -> Result<PathBuf, Vec<PathBuf>> {
        let mut candidate = PathBuf::from(name);
        if candidate.extension().is_none() {
            candidate.set_extension("pas");
        }
        if candidate.is_absolute() {
            return Ok(candidate);
        }

        let mut probed = Vec::new();
        if let Some(directory) = self.arena.path(from).parent() {
            let local = directory.join(&candidate);
            if local.exists() {
                return Ok(local);
            }
            probed.push(local);
        }
        // Delphi resolves `{$I}` against the include path (`DCC_IncludePath`);
        // the unit search path is probed too as a lenient fallback (finding a
        // file there is never wrong — only more permissive than dcc).
        for search_path in self
            .state
            .context
            .include_paths
            .iter()
            .chain(self.state.context.search_paths.iter())
        {
            let joined = search_path.join(&candidate);
            if joined.exists() {
                return Ok(joined);
            }
            probed.push(joined);
        }
        Err(probed)
    }

    /// Switch directives: `{$H+}`, `{$A8}`, `{$Z4}`, `{$RANGECHECKS ON}`, ...
    /// Unknown directives are deliberately ignored ($R resources, $REGION,
    /// $WARN, linker options — irrelevant to parsing).
    fn apply_switch_directive(&mut self, name: &str, argument: &str) {
        let switches = &mut self.state.switches;
        let upper = name.to_ascii_uppercase();

        // Single letters with +/- or value
        if upper.len() == 1 {
            let letter = upper.chars().next().unwrap();
            match letter {
                'A' => {
                    switches.align = match parse_plus_minus(argument) {
                        Some(true) => 8,
                        Some(false) => 1,
                        None => argument.parse().unwrap_or(switches.align),
                    };
                }
                'Z' => {
                    switches.min_enum_size = match parse_plus_minus(argument) {
                        Some(true) => 4,
                        Some(false) => 1,
                        None => argument.parse().unwrap_or(switches.min_enum_size),
                    };
                }
                _ => {
                    if let (Some(flag), Some(enabled)) =
                        (switch_flag_for_letter(letter), parse_plus_minus(argument))
                    {
                        switches.flags.set(flag, enabled);
                    }
                }
            }
            return;
        }

        // Long forms
        let long_flag = match upper.as_str() {
            "ALIGN" => {
                switches.align = match parse_on_off(argument) {
                    Some(true) => 8,
                    Some(false) => 1,
                    None => argument.parse().unwrap_or(switches.align),
                };
                return;
            }
            "MINENUMSIZE" => {
                switches.min_enum_size = argument.parse().unwrap_or(switches.min_enum_size);
                return;
            }
            "BOOLEVAL" => SwitchFlags::BOOL_EVAL,
            "ASSERTIONS" => SwitchFlags::ASSERTIONS,
            "DEBUGINFO" => SwitchFlags::DEBUG_INFO,
            "LONGSTRINGS" => SwitchFlags::LONG_STRINGS,
            "IOCHECKS" => SwitchFlags::IO_CHECKS,
            "WRITEABLECONST" => SwitchFlags::WRITEABLE_CONSTS,
            "LOCALSYMBOLS" => SwitchFlags::LOCAL_SYMBOLS,
            "TYPEINFO" => SwitchFlags::TYPE_INFO,
            "OPTIMIZATION" => SwitchFlags::OPTIMIZATION,
            "OPENSTRINGS" => SwitchFlags::OPEN_STRINGS,
            "OVERFLOWCHECKS" => SwitchFlags::OVERFLOW_CHECKS,
            "RANGECHECKS" => SwitchFlags::RANGE_CHECKS,
            "TYPEDADDRESS" => SwitchFlags::TYPED_ADDRESS,
            "SAFEDIVIDE" => SwitchFlags::SAFE_DIVIDE,
            "VARSTRINGCHECKS" => SwitchFlags::VAR_STRING_CHECKS,
            "STACKFRAMES" => SwitchFlags::STACK_FRAMES,
            "EXTENDEDSYNTAX" => SwitchFlags::EXTENDED_SYNTAX,
            "REFERENCEINFO" => SwitchFlags::REFERENCE_INFO,
            _ => return,
        };
        if let Some(enabled) = parse_on_off(argument) {
            switches.flags.set(long_flag, enabled);
        }
    }
}

fn parse_plus_minus(argument: &str) -> Option<bool> {
    match argument.trim() {
        "+" => Some(true),
        "-" => Some(false),
        _ => None,
    }
}

/// First identifier in a directive argument: the leading run of identifier
/// characters, ignoring any trailing junk. `{$DEFINE FOO BAR}` defines `FOO`.
fn first_identifier(argument: &str) -> &str {
    let trimmed = argument.trim_start();
    let end = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(trimmed.len());
    &trimmed[..end]
}

/// `{$I name}` file name: a quoted name yields its content up to the closing
/// quote (stripping exactly one surrounding pair, not every quote); an
/// unquoted name ends at the first whitespace, matching `dcc`.
fn normalize_include_name(argument: &str) -> &str {
    let trimmed = argument.trim();
    match trimmed.strip_prefix('\'') {
        Some(inner) => match inner.split_once('\'') {
            Some((quoted, _rest)) => quoted,
            None => inner,
        },
        None => trimmed.split_whitespace().next().unwrap_or(trimmed),
    }
}

fn parse_on_off(argument: &str) -> Option<bool> {
    let argument = argument.trim();
    if argument.eq_ignore_ascii_case("on") {
        Some(true)
    } else if argument.eq_ignore_ascii_case("off") {
        Some(false)
    } else {
        None
    }
}

fn switch_flag_for_letter(letter: char) -> Option<SwitchFlags> {
    Some(match letter.to_ascii_uppercase() {
        'B' => SwitchFlags::BOOL_EVAL,
        'C' => SwitchFlags::ASSERTIONS,
        'D' => SwitchFlags::DEBUG_INFO,
        'H' => SwitchFlags::LONG_STRINGS,
        'I' => SwitchFlags::IO_CHECKS,
        'J' => SwitchFlags::WRITEABLE_CONSTS,
        'L' => SwitchFlags::LOCAL_SYMBOLS,
        'M' => SwitchFlags::TYPE_INFO,
        'O' => SwitchFlags::OPTIMIZATION,
        'P' => SwitchFlags::OPEN_STRINGS,
        'Q' => SwitchFlags::OVERFLOW_CHECKS,
        'R' => SwitchFlags::RANGE_CHECKS,
        'T' => SwitchFlags::TYPED_ADDRESS,
        'U' => SwitchFlags::SAFE_DIVIDE,
        'V' => SwitchFlags::VAR_STRING_CHECKS,
        'W' => SwitchFlags::STACK_FRAMES,
        'X' => SwitchFlags::EXTENDED_SYNTAX,
        'Y' => SwitchFlags::REFERENCE_INFO,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DefineSet, Interner, SwitchState, TargetPlatform};
    use crate::unit_cache::UnitCache;
    use std::collections::HashMap;

    fn test_context(defines: &[&str]) -> Arc<ProjectContext> {
        let mut context = ProjectContext {
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
        };
        for define in defines {
            let symbol = context.intern(define);
            context.base_defines.define(symbol);
        }
        Arc::new(context)
    }

    /// Collect the identifier/keyword texts the cursor lets through.
    fn surviving_text(source: &str, defines: &[&str]) -> Vec<String> {
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", source);
        let mut cursor = TokenCursor::new(&arena, test_context(defines), file);
        let mut texts = Vec::new();
        while let Some(lexeme) = cursor.advance().unwrap() {
            texts.push(cursor.text(lexeme).to_string());
        }
        texts
    }

    /// L10: `RTLVersion` and `CompilerVersion` are now DISTINCT constants, not
    /// a hard alias. A profile whose two versions diverge must evaluate each
    /// `{$IF}` independently — the branch kept for `RTLVersion = 30` differs
    /// from the branch kept for `CompilerVersion = 36`.
    #[test]
    fn rtl_version_and_compiler_version_evaluate_independently() {
        let context = {
            let mut context = ProjectContext {
                configuration: "Debug".to_string(),
                platform_name: "Win32".to_string(),
                platform: TargetPlatform::Win32,
                compiler_version: 36.0,
                rtl_version: 30.0, // deliberately divergent (old-release shape)
                base_defines: DefineSet::default(),
                search_paths: Vec::new(),
                include_paths: Vec::new(),
                namespaces: Vec::new(),
                unit_aliases: HashMap::new(),
                default_switches: SwitchState::default(),
                unit_cache: UnitCache::default(),
            };
            let _ = &mut context;
            Arc::new(context)
        };

        let evaluate = |source: &str| -> Vec<String> {
            let arena = SourceArena::new();
            let file = arena.insert_virtual("test.pas", source);
            let mut cursor = TokenCursor::new(&arena, context.clone(), file);
            let mut texts = Vec::new();
            while let Some(lexeme) = cursor.advance().unwrap() {
                texts.push(cursor.text(lexeme).to_string());
            }
            texts
        };

        // CompilerVersion tracks compiler_version (36), NOT rtl_version.
        assert_eq!(
            evaluate("{$IF CompilerVersion = 36} a {$ELSE} b {$IFEND}"),
            ["a"]
        );
        // RTLVersion tracks rtl_version (30) independently.
        assert_eq!(
            evaluate("{$IF RTLVersion = 30} a {$ELSE} b {$IFEND}"),
            ["a"]
        );
        // The old hard-alias would have made RTLVersion = 36 true; it is not.
        assert_eq!(
            evaluate("{$IF RTLVersion = 36} a {$ELSE} b {$IFEND}"),
            ["b"]
        );
    }

    #[test]
    fn ifdef_filters_dead_branch() {
        let source = "a {$IFDEF FOO} b {$ELSE} c {$ENDIF} d";
        assert_eq!(surviving_text(source, &["FOO"]), ["a", "b", "d"]);
        assert_eq!(surviving_text(source, &[]), ["a", "c", "d"]);
    }

    #[test]
    fn ifdef_uses_first_identifier_only() {
        // L2: `{$IFDEF FOO BAR}` tests FOO (trailing junk ignored), not "FOO BAR".
        let source = "a {$IFDEF FOO BAR} b {$ELSE} c {$ENDIF} d";
        assert_eq!(surviving_text(source, &["FOO"]), ["a", "b", "d"]);
    }

    #[test]
    fn define_uses_first_identifier_only() {
        // L2: `{$DEFINE FOO BAR}` defines FOO; a later `{$IFDEF FOO}` sees it.
        let source = "{$DEFINE FOO BAR} {$IFDEF FOO} yes {$ELSE} no {$ENDIF}";
        assert_eq!(surviving_text(source, &[]), ["yes"]);
    }

    /// L9: a `{$DEFINE}` / `{$IFDEF}` round-trip with a NON-ASCII define name is
    /// consistent — both the define dispatch and the ifdef lookup fold through
    /// the SAME `fold_identifier` (ordinal ASCII), so the ASCII portion is
    /// case-insensitive while the non-ASCII byte must match exactly. The old
    /// split (`intern_key` Unicode-fold vs directive ASCII-fold) could disagree.
    #[test]
    fn non_ascii_define_ifdef_round_trip_is_consistent() {
        // ASCII part case-insensitive, non-ASCII `ß` byte-identical → matches.
        let source = "{$DEFINE ßFoo} {$IFDEF ßFOO} yes {$ELSE} no {$ENDIF}";
        assert_eq!(surviving_text(source, &[]), ["yes"]);

        // A DIFFERENT non-ASCII byte (`ö` vs `ß`) must NOT match — ordinal fold
        // never collapses distinct non-ASCII identifiers (no wrong match).
        let source = "{$DEFINE ßBar} {$IFDEF öBar} wrong {$ELSE} ok {$ENDIF}";
        assert_eq!(surviving_text(source, &[]), ["ok"]);
    }

    #[test]
    fn elseif_chain_takes_first_true_branch() {
        let source = "{$IF Defined(A)} a {$ELSEIF Defined(B)} b {$ELSE} c {$IFEND}";
        assert_eq!(surviving_text(source, &["A", "B"]), ["a"]);
        assert_eq!(surviving_text(source, &["B"]), ["b"]);
        assert_eq!(surviving_text(source, &[]), ["c"]);
    }

    #[test]
    fn dead_branch_swallows_unlexable_compile_breaker_text() {
        // common idiom: prose as intentional compile error in a guard branch
        let source = "{$IFDEF NOPE} Error: do not use this unit! {$ENDIF} ok";
        assert_eq!(surviving_text(source, &[]), ["ok"]);
        // in an ACTIVE branch the same bytes are a hard error
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", "active ! breaker");
        let mut cursor = TokenCursor::new(&arena, test_context(&[]), file);
        cursor.advance().unwrap();
        assert!(matches!(cursor.advance(), Err(CursorError::Lex(_))));
    }

    #[test]
    fn dead_branch_condition_not_evaluated() {
        // garbage expression inside a dead branch must not error
        let source = "{$IFDEF NOPE} {$IF ??? garbage !!!} x {$ENDIF} {$ENDIF} ok";
        assert_eq!(surviving_text(source, &[]), ["ok"]);
    }

    #[test]
    fn define_and_undef_mid_file() {
        let source = "{$DEFINE LATER} {$IFDEF LATER} a {$ENDIF} {$UNDEF LATER} {$IFDEF LATER} b {$ENDIF}";
        assert_eq!(surviving_text(source, &[]), ["a"]);
    }

    #[test]
    fn defines_are_case_insensitive() {
        // Delphi identifiers are case-insensitive — dual-track lookup keys
        let source = "{$DEFINE UseFoo} {$IFDEF USEFOO} a {$ENDIF} {$ifdef usefoo} b {$endif}";
        assert_eq!(surviving_text(source, &[]), ["a", "b"]);
    }

    #[test]
    fn define_in_dead_branch_ignored() {
        let source = "{$IFDEF NOPE} {$DEFINE X} {$ENDIF} {$IFDEF X} bad {$ENDIF} ok";
        assert_eq!(surviving_text(source, &[]), ["ok"]);
    }

    #[test]
    fn compiler_version_condition() {
        let source = "{$IF CompilerVersion >= 35.0} modern {$ELSE} old {$IFEND}";
        assert_eq!(surviving_text(source, &[]), ["modern"]);
    }

    #[test]
    fn ifopt_reads_switch_state() {
        // $H+ is the modern default; flip it and observe
        let source = "{$IFOPT H+} on {$ENDIF} {$H-} {$IFOPT H+} bad {$ENDIF} {$IFOPT H-} off {$ENDIF}";
        assert_eq!(surviving_text(source, &[]), ["on", "off"]);
    }

    #[test]
    fn sizeof_builtins_evaluate_from_layout_table() {
        // Win32 context: Pointer = 4, Extended = 10
        let source = "{$IF SizeOf(Pointer) = 4} p4 {$ELSE} p8 {$IFEND}\
                      {$IF SizeOf(Extended) = 10} x87 {$IFEND}\
                      {$IF SizeOf(Int64) = 8} i8 {$IFEND}";
        assert_eq!(surviving_text(source, &[]), ["p4", "x87", "i8"]);
    }

    #[test]
    fn unknown_condition_uses_policy_and_diagnoses() {
        let arena = SourceArena::new();
        let file = arena.insert_virtual(
            "test.pas",
            "{$IF SizeOf(TSomewhereElse) > 4} yes {$ELSE} no {$IFEND}",
        );
        let mut cursor = TokenCursor::new(&arena, test_context(&[]), file);
        let mut texts = Vec::new();
        while let Some(lexeme) = cursor.advance().unwrap() {
            texts.push(cursor.text(lexeme).to_string());
        }
        assert_eq!(texts, ["no"]); // AssumeFalse default
        assert_eq!(cursor.diagnostics.len(), 1);
    }

    #[test]
    fn unterminated_conditional_errors_at_eof() {
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", "{$IFDEF FOO} a");
        let mut cursor = TokenCursor::new(&arena, test_context(&["FOO"]), file);
        assert!(cursor.advance().is_ok()); // 'a'
        assert!(matches!(
            cursor.advance(),
            Err(CursorError::Directive(
                DirectiveError::UnterminatedConditional { .. }
            ))
        ));
    }

    #[test]
    fn include_splices_and_conditional_spans_include() {
        let directory = std::env::temp_dir().join("delphi_parser_cursor_test");
        std::fs::create_dir_all(&directory).unwrap();
        // include opens a conditional that the main file closes
        std::fs::write(directory.join("defs.inc"), "inc_token {$IFDEF FOO}").unwrap();
        std::fs::write(
            directory.join("main.pas"),
            "start {$I defs.inc} guarded {$ENDIF} finish",
        )
        .unwrap();

        let arena = SourceArena::new();
        let file = arena.load(directory.join("main.pas")).unwrap();
        let mut cursor = TokenCursor::new(&arena, test_context(&["FOO"]), file);
        let mut texts = Vec::new();
        while let Some(lexeme) = cursor.advance().unwrap() {
            texts.push(cursor.text(lexeme).to_string());
        }
        assert_eq!(texts, ["start", "inc_token", "guarded", "finish"]);
    }

    #[test]
    fn pseudo_include_splices_env_variable_as_string_literal() {
        // SAFETY: test-local variable name, no reader outside this test
        unsafe { std::env::set_var("DELPHI_PARSER_TEST_PSEUDO", "wert") };
        let arena = SourceArena::new();
        let file = arena.insert_virtual(
            "test.pas",
            "a {$I %DELPHI_PARSER_TEST_PSEUDO%} b {$I %DOES_NOT_EXIST_XYZ%} c",
        );
        let mut cursor = TokenCursor::new(&arena, test_context(&[]), file);
        let mut tokens = Vec::new();
        while let Some(lexeme) = cursor.advance().unwrap() {
            tokens.push((lexeme.token, cursor.text(lexeme).to_string()));
        }
        assert_eq!(
            tokens,
            [
                (Token::Ident, "a".to_string()),
                (Token::StringLiteral, "'wert'".to_string()),
                (Token::Ident, "b".to_string()),
                (Token::StringLiteral, "''".to_string()),
                (Token::Ident, "c".to_string()),
            ]
        );
        // the unset variable left a diagnostic, the set one did not
        assert_eq!(cursor.diagnostics.len(), 1);
    }

    #[test]
    fn missing_include_error_lists_probed_paths() {
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", "{$I nowhere.inc} x");
        let mut cursor = TokenCursor::new(&arena, test_context(&[]), file);
        let Err(CursorError::Include { error, .. }) = cursor.advance() else {
            panic!("expected include error");
        };
        assert!(error.message.contains("probed:"), "{}", error.message);
    }

    #[test]
    fn switch_sign_with_leading_space() {
        // ledger #11: `{$H +}` — argument is trimmed before the sign check
        let source = "{$H -} {$IFOPT H-} off {$ENDIF} {$H +} {$IFOPT H+} on {$ENDIF}";
        assert_eq!(surviving_text(source, &[]), ["off", "on"]);
    }

    #[test]
    fn io_switch_not_confused_with_include() {
        let source = "{$I-} {$IFOPT I-} a {$ENDIF} {$I+} {$IFOPT I+} b {$ENDIF}";
        assert_eq!(surviving_text(source, &[]), ["a", "b"]);
    }

    #[test]
    fn align_switch_value() {
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", "{$A1} x {$ALIGN 16} y {$A+}");
        let mut cursor = TokenCursor::new(&arena, test_context(&[]), file);
        cursor.advance().unwrap(); // x — after {$A1}
        assert_eq!(cursor.state().switches.align, 1);
        cursor.advance().unwrap(); // y — after {$ALIGN 16}
        assert_eq!(cursor.state().switches.align, 16);
        assert!(cursor.advance().unwrap().is_none());
        assert_eq!(cursor.state().switches.align, 8);
    }

    #[test]
    fn peek_and_expect() {
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", "unit Foo;");
        let mut cursor = TokenCursor::new(&arena, test_context(&[]), file);
        assert_eq!(cursor.peek().unwrap().unwrap().token, Token::Unit);
        let unit = cursor.expect(Token::Unit).unwrap();
        assert_eq!(cursor.text(unit), "unit");
        assert_eq!(cursor.expect(Token::Ident).map(|l| cursor.text(l)).unwrap(), "Foo");
        assert!(matches!(
            cursor.expect(Token::Ident),
            Err(CursorError::UnexpectedToken { .. })
        ));
    }
}
