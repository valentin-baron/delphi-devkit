//! Evaluator for `{$IF expr}` / `{$ELSEIF expr}` directive expressions.
//!
//! Input is the expression text AFTER the directive name (`SizeOf(TFoo) >= 4`,
//! not `$IF SizeOf(TFoo) >= 4`). Result is three-valued: symbol-dependent
//! terms the resolver can't answer yet (DCU-only unit, no symbol table, layout
//! gap) yield [`Condition::Unknown`], which propagates by Kleene logic —
//! `Unknown AND False = False`, so mixed expressions often still resolve.

use crate::parse_state::UnitParseState;

/// Compile-time constant value inside a directive expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    /// A value above `i64::MAX` that fits `u64` (`$FFFFFFFFFFFFFFFF`, Delphi
    /// `UInt64`). Kept distinct from `Int` so mixed-width comparisons/arithmetic
    /// stay exact (via `i128`) rather than bit-casting to a wrong negative.
    UInt(u64),
    Float(f64),
    Str(String),
    Bool(bool),
}

/// Three-valued outcome of a directive condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    True,
    False,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    /// Malformed expression — byte offset into the expression text.
    Syntax { position: usize, message: String },
    /// Well-formed but ill-typed ('a' + True, non-boolean top level, ...).
    Type { message: String },
}

/// Answers the symbol-dependent parts of a directive expression. `&mut`
/// because answering may lazily parse used-unit interfaces.
///
/// `None` = cannot answer (becomes `Unknown`), never "definitely no":
/// a resolver that has all sources answers `Declared` of a missing symbol
/// with `Some(false)`.
pub trait SymbolResolver {
    /// `Defined(X)` — define set is always fully known.
    fn is_defined(&mut self, symbol: &str) -> bool;
    /// `Declared(X)` — `X` may be dotted (`System.TObject`).
    fn is_declared(&mut self, identifier: &str) -> Option<bool>;
    /// `SizeOf(X)` in bytes.
    fn size_of(&mut self, type_name: &str) -> Option<u64>;
    /// Bare identifier: `CompilerVersion`, `RTLVersion`, visible constants.
    fn const_value(&mut self, identifier: &str) -> Option<Value>;
}

/// Resolver over a [`UnitParseState`]. `Declared`/`SizeOf`/consts answer
/// `Unknown` until the symbol table lands; `Defined` and the two compiler
/// constants already work, which covers the bulk of real-world directives.
pub struct StateResolver<'a> {
    pub state: &'a mut UnitParseState,
    /// Named-type descent depth for the layout engine — guards against alias
    /// cycles / self-referential records (`TFoo = record x: TFoo; end;`, invalid
    /// but must not overflow the stack). Reaching the ceiling returns `None`
    /// (Unknown), never a wrong size. Zero on construction.
    pub layout_depth: u32,
}

/// Ceiling for nested named-type resolution while sizing (alias chains, nested
/// records). Well past any realistic type nesting; a chain longer than this is
/// almost certainly a cycle and degrades to Unknown.
const MAX_LAYOUT_DEPTH: u32 = 64;

impl StateResolver<'_> {
    fn convert_constant(&self, value: crate::unit_cache::ConstantValue) -> Value {
        match value {
            crate::unit_cache::ConstantValue::Int(v) => Value::Int(v),
            crate::unit_cache::ConstantValue::UInt(v) => Value::UInt(v),
            crate::unit_cache::ConstantValue::Float(v) => Value::Float(v),
            crate::unit_cache::ConstantValue::Bool(v) => Value::Bool(v),
            crate::unit_cache::ConstantValue::Str(v) => {
                Value::Str(crate::globals::resolve(v).to_string())
            }
        }
    }
}

/// Outcome of resolving one step of a scoped `Declared(A.B.C)` walk.
enum ScopeStep {
    /// Chain fully resolved; `true` = member present, `false` = genuinely
    /// absent (compiler would reject). Only a fully-resolved chain may say
    /// `false` — everything unprovable stays `Unknown` (`None` at the caller).
    Resolved(bool),
    /// A segment could not be resolved (unknown type/unit, DCU-only, member
    /// whose type is cross-unit and not followed) → `Unknown`.
    Unknown,
    /// A uses-cycle was hit while consulting an import → taint + `Unknown`.
    /// Not yet constructed: the only cycle-producing path is cross-unit member
    /// descent (`Declared(A.B.C)` where `B`'s type lives in another unit), which
    /// is currently conservative-Unknown and not followed (ledger #31). Retained
    /// (with its `mark_cycle_tainted` handling at the call sites) so #31 lands
    /// cycle-safe by construction rather than needing a new variant then.
    #[allow(dead_code)]
    Cycle,
}

impl StateResolver<'_> {
    /// Scoped `Declared(A.B[.C…])`. Resolves the first segment to a type
    /// (own interface first, then imported units in reverse uses order), then
    /// walks members. Records every consulted import as a dependency — the same
    /// staleness discipline as the flat path, so a shadowing/growing import
    /// invalidates this unit's cache. Never a confident `false` unless the whole
    /// chain resolves and the final member is genuinely absent; an unresolvable
    /// segment → Unknown; a cycle → taint + Unknown.
    fn is_declared_scoped(&mut self, identifier: &str) -> Option<bool> {
        let segments: Vec<&str> = identifier.split('.').collect();
        // A trailing/leading empty segment (`TFoo.`) is malformed → Unknown.
        if segments.iter().any(|segment| segment.is_empty()) {
            return None;
        }
        let first_key = self.state.context.intern_key(segments[0]);

        // (1) First segment as an OWN interface type: `Declared(TFoo.Bar[.Sub])`
        // where TFoo is declared earlier in this very unit. Own types track
        // (member → member-type) pairs, so nested chains resolve too; a member
        // whose type is complex/foreign stops the walk at Unknown.
        if self.state.own_type_members_known(first_key) {
            let member_keys: Vec<_> = segments[1..]
                .iter()
                .map(|segment| self.state.context.intern_key(segment))
                .collect();
            return self.state.own_scoped_declared(first_key, &member_keys);
        }

        // (2) First segment as a type declared in an imported unit's interface,
        // OR as an imported UNIT name (then the second segment is the type).
        let loader = self.state.loader.clone()?;
        for import in self.state.imports_reversed() {
            match loader.interface_of(import) {
                crate::parse_state::LoadOutcome::Loaded(meta) => {
                    self.state.record_dependency(&meta);
                    let interface = meta.interface();
                    // (2a) first segment is a TYPE in this imported unit
                    if interface.find(first_key).is_some() {
                        match self.walk_type_members(interface, first_key, &segments[1..]) {
                            ScopeStep::Resolved(result) => return Some(result),
                            ScopeStep::Cycle => self.state.mark_cycle_tainted(),
                            ScopeStep::Unknown => {}
                        }
                    } else if crate::globals::resolve(meta.name()).eq_ignore_ascii_case(segments[0])
                        && segments.len() >= 2
                    {
                        // (2b) first segment IS this imported unit's name:
                        // `Unit.TType[.Member]` — resolve TType inside it.
                        let type_key = self.state.context.intern_key(segments[1]);
                        if interface.find(type_key).is_some() {
                            match self.walk_type_members(interface, type_key, &segments[2..]) {
                                ScopeStep::Resolved(result) => return Some(result),
                                ScopeStep::Cycle => self.state.mark_cycle_tainted(),
                                ScopeStep::Unknown => {}
                            }
                        }
                        // unit matched but the type is absent — a genuine
                        // negative for THIS unit, but a later import could still
                        // provide the whole chain; keep scanning.
                    }
                }
                crate::parse_state::LoadOutcome::Cycle => self.state.mark_cycle_tainted(),
                _ => {}
            }
        }
        // Nothing resolved the first segment to a type/unit anywhere. Every
        // import that resolved cleanly and did not match leaves the first
        // segment an unknown type/unit → still Unknown (we cannot prove absence
        // of a symbol we never located), never a confident false.
        None
    }

    /// Walk `remaining` member segments starting inside `type_key`'s members in
    /// `interface`. `remaining` empty means the type itself is the target (it
    /// exists → declared). Each further segment must be a member of the current
    /// type; to descend past it we need the member's simple `type_key` and that
    /// type must live in the SAME interface (nested/same-unit) — a member whose
    /// type is in another unit is not followed (→ Unknown; ledger #19 note).
    fn walk_type_members(
        &self,
        interface: &crate::unit_cache::UnitInterface,
        type_key: crate::context::Identifier,
        remaining: &[&str],
    ) -> ScopeStep {
        let Some(mut current_type) = interface.find(type_key) else {
            return ScopeStep::Unknown;
        };
        for (index, segment) in remaining.iter().enumerate() {
            let member_key = self.state.context.intern_key(segment);
            let Some(member) = current_type.find_member(member_key) else {
                // Type resolved, member absent from its DIRECT declarations. A
                // class/interface (or any type with ancestors) could still
                // inherit it from a base we do not flatten here → Unknown, never
                // a confident false (ledger #19). Only an ancestor-less type
                // (record/enum/…) can prove genuine absence.
                if current_type.has_ancestors {
                    return ScopeStep::Unknown;
                }
                return ScopeStep::Resolved(false);
            };
            let is_last = index + 1 == remaining.len();
            if is_last {
                return ScopeStep::Resolved(true);
            }
            // descend: the member's simple type must be another type in this
            // same interface, else we cannot follow the chain → Unknown.
            let Some(next_type_key) = member.type_key else {
                return ScopeStep::Unknown;
            };
            let Some(next_type) = interface.find(next_type_key) else {
                return ScopeStep::Unknown; // type lives in another unit
            };
            current_type = next_type;
        }
        // no member segments — the type itself is the (declared) target
        ScopeStep::Resolved(true)
    }

    /// Resolve a qualified `SizeOf(Unit.TType)` argument to the type's
    /// `TypeExpression`. Splits on the LAST `.` (the unit part may itself be
    /// dotted, e.g. `System.Types.TRect`), locates the imported unit by name
    /// among the current imports, and finds the simple type in its interface
    /// AST — reusing the same loader/dependency discipline as the flat path.
    /// Records the exporting unit as a dependency so a layout-affecting edit to
    /// it invalidates this unit's cache. A cycle taints + None; anything not
    /// resolved cleanly → None (Unknown), never a guessed size.
    fn resolve_qualified_type(
        &mut self,
        qualified: &str,
    ) -> Option<crate::ast::TypeExpression> {
        let (unit_part, type_part) = qualified.rsplit_once('.')?;
        // Malformed (`Unit.`, `.TType`, `Unit..TType`) → Unknown, never a guess.
        if unit_part.is_empty() || type_part.contains('.') || type_part.is_empty() {
            return None;
        }
        let type_key = self.state.context.intern_key(type_part);
        let loader = self.state.loader.clone()?;
        // Scan imports (reverse uses order) for the unit whose name matches the
        // qualifier; only that unit may satisfy an explicitly-qualified name.
        // An UNRELATED import that is missing/failed/cyclic does NOT decide the
        // result — we keep scanning for the named unit, mirroring the parallel
        // `is_declared_scoped`/`resolve_named_type` walks (previously this
        // aborted to Unknown on the FIRST unresolvable import even when a later
        // import was the named unit). A cycle taints the parse (invalid Delphi)
        // but still doesn't stop the scan; only the named unit's outcome
        // decides. The never-wrong guarantee is unchanged: we only ever return
        // a type found in the name-matching unit, else None (Unknown).
        for import in self.state.imports_reversed() {
            match loader.interface_of(import) {
                crate::parse_state::LoadOutcome::Loaded(meta) => {
                    if crate::globals::resolve(meta.name()).eq_ignore_ascii_case(unit_part) {
                        // Matching unit found — record it as a dependency whether
                        // or not it carries the type, then resolve the type there.
                        self.state.record_dependency(&meta);
                        return find_type_declaration(&meta.ast, type_key).cloned();
                    }
                    // a different unit — skip, keep scanning for the named one
                }
                crate::parse_state::LoadOutcome::Cycle => self.state.mark_cycle_tainted(),
                _ => {} // an unrelated unresolvable import → skip, keep scanning
            }
        }
        None // the named unit is not among the imports → Unknown
    }
}

impl SymbolResolver for StateResolver<'_> {
    fn is_defined(&mut self, symbol: &str) -> bool {
        let symbol = self.state.context.intern_key(symbol);
        self.state.is_defined(symbol)
    }

    fn is_declared(&mut self, identifier: &str) -> Option<bool> {
        // Dotted names (`TFoo.Bar`, `System.TObject`, `TFoo.TInner.X`) need
        // scoped resolution against the symbol table (ledger #19).
        if identifier.contains('.') {
            return self.is_declared_scoped(identifier);
        }

        let key = self.state.context.intern_key(identifier);
        if self.state.own_interface_declared(key) {
            return Some(true);
        }
        let loader = self.state.loader.clone()?; // no loader → Unknown

        let mut incomplete = false;
        for import in self.state.imports_reversed() {
            match loader.interface_of(import) {
                crate::parse_state::LoadOutcome::Loaded(meta) => {
                    self.state.record_dependency(&meta);
                    if meta.interface().contains_key(key) {
                        return Some(true);
                    }
                }
                // uses-cycle: invalid Delphi, degrade to Unknown AND taint
                // the parse result (must not be persisted as trustworthy)
                crate::parse_state::LoadOutcome::Cycle => {
                    self.state.mark_cycle_tainted();
                    incomplete = true;
                }
                // any unresolvable import makes "not declared anywhere"
                // unprovable — Unknown, never a confident false
                _ => incomplete = true,
            }
        }
        if incomplete { None } else { Some(false) }
    }

    fn size_of(&mut self, type_name: &str) -> Option<u64> {
        // ONE identifier fold (matches `intern_key` / the builtin table keys).
        let folded = crate::globals::fold_identifier(type_name);
        // Built-ins answer straight from the table (no symbol table needed).
        if let Some(size) = crate::layout::builtin_size(&folded, self.state.context.platform) {
            return Some(size);
        }
        // A qualified `SizeOf(Unit.TType)` names the exporting unit explicitly;
        // resolve the unit segment via the loader, then the simple type inside
        // it (ledger #19's scoped `Unit.Type` mechanics). Any failure → None.
        let type_expression = if type_name.contains('.') {
            self.resolve_qualified_type(type_name)?
        } else {
            // A user type: resolve its declaration (own first, then imports) and
            // run the layout engine. `size_of` sees ONLY the size; the engine
            // tracks alignment internally to lay out enclosing records. Any
            // uncertainty (unknown field, unresolvable bound, deferred
            // variant/set) → None, which the resolver degrades to Unknown —
            // never a guessed number that would silently flip a `{$IF}` branch
            // (the North Star).
            let key = self.state.context.intern_key(type_name);
            crate::layout::LayoutResolver::resolve_named_type(self, key)?
        };
        let switches = self.state.switches;
        let platform = self.state.context.platform;
        crate::layout::type_layout(&type_expression, switches, platform, self)
            .map(|layout| layout.size)
    }

    fn const_value(&mut self, identifier: &str) -> Option<Value> {
        match crate::globals::fold_identifier(identifier).as_str() {
            "COMPILERVERSION" => {
                return Some(Value::Float(self.state.context.compiler_version));
            }
            // RTLVersion is a DISTINCT constant, not an alias — it equals
            // compiler_version for every modern Delphi but the profile may
            // supply a divergent value (very old releases), which we honor here.
            "RTLVERSION" => {
                return Some(Value::Float(self.state.context.rtl_version));
            }
            _ => {}
        }

        let key = self.state.context.intern_key(identifier);
        if let Some(value) = self.state.own_constant(key) {
            return Some(self.convert_constant(value));
        }
        if identifier.contains('.') {
            // Dotted CONSTANT values (`TFoo.MaxItems` used as a value, not in
            // Declared) stay Unknown for now — deferred, ledger #30. Scoped
            // Declared (#19) is implemented; a dotted const value additionally
            // needs the member's captured `ConstantValue`, which the flat
            // member index does not yet carry across units.
            return None;
        }
        let loader = self.state.loader.clone()?;
        for import in self.state.imports_reversed() {
            match loader.interface_of(import) {
                crate::parse_state::LoadOutcome::Loaded(meta) => {
                    // Record EVERY consulted import as a dependency (like
                    // `is_declared`), not only the one that yields the value.
                    // A later-`uses`d unit that grows this constant would
                    // shadow an earlier one and change the result; if it were
                    // not a recorded dependency, editing it would never
                    // invalidate this unit → silently stale cache.
                    self.state.record_dependency(&meta);
                    if let Some(symbol) = meta.interface().find(key) {
                        // symbol exists here — shadowing STOPS the walk even
                        // when its value was not capturable (→ Unknown)
                        return symbol.constant_value.map(|value| {
                            self.convert_constant(value)
                        });
                    }
                }
                crate::parse_state::LoadOutcome::Cycle => {
                    self.state.mark_cycle_tainted();
                    return None;
                }
                _ => return None, // unresolvable import → Unknown
            }
        }
        None // not found anywhere (compiler would reject) → Unknown
    }
}

/// Evaluate a constant expression to a [`Value`], not a boolean condition.
/// Used by the layout engine for array/subrange bounds, enum ordinals and
/// string lengths (`array[0..Count-1]`, `0..255`, `string[MaxLen]`). Returns
/// `Ok(None)` when the expression is well-formed but an operand is Unknown
/// (an unresolved constant) — the layout engine treats that as "cannot size"
/// and returns `None`, never a guessed number. `Err` is a malformed/ill-typed
/// expression.
pub fn evaluate_value(
    expression: &str,
    resolver: &mut dyn SymbolResolver,
) -> Result<Evaluated, EvalError> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser {
        tokens,
        position: 0,
        resolver,
    };
    let value = parser.expression()?;
    parser.expect_eof()?;
    Ok(value)
}

impl crate::layout::LayoutResolver for StateResolver<'_> {
    fn resolve_named_type(
        &mut self,
        name_key: crate::context::Identifier,
    ) -> Option<crate::ast::TypeExpression> {
        // (1) Own interface type declared earlier in THIS unit.
        if let Some(type_expression) = self.state.own_type_expression(name_key) {
            return Some((*type_expression).clone());
        }
        // (2) A type declared in an imported unit's interface. Walk imports in
        // reverse uses order (later units shadow earlier ones); record EVERY
        // consulted import as a dependency so a layout-affecting edit to it
        // invalidates this unit's cache. A cycle taints and yields None.
        let loader = self.state.loader.clone()?;
        for import in self.state.imports_reversed() {
            match loader.interface_of(import) {
                crate::parse_state::LoadOutcome::Loaded(meta) => {
                    self.state.record_dependency(&meta);
                    if let Some(type_expression) =
                        find_type_declaration(&meta.ast, name_key)
                    {
                        return Some(type_expression.clone());
                    }
                }
                crate::parse_state::LoadOutcome::Cycle => {
                    self.state.mark_cycle_tainted();
                    return None;
                }
                _ => return None, // unresolvable import → Unknown, never a guess
            }
        }
        None
    }

    fn span_text(&mut self, location: crate::meta::CodeLocation) -> Option<String> {
        crate::globals::arena()
            .try_location_text(location)
            .ok()
            .map(str::to_string)
    }

    fn evaluate_integer(&mut self, expression: &str) -> Option<i64> {
        match evaluate_value(expression, self) {
            Ok(Some(Value::Int(value))) => Some(value),
            // A single char literal survives as a one-char string; the caller
            // (`bound_ordinal`) handles that form BEFORE calling us, so a string
            // here is not an integer bound → None (Unknown, never a guess).
            _ => None,
        }
    }

    fn enter_type(&mut self) -> bool {
        if self.layout_depth >= MAX_LAYOUT_DEPTH {
            return false;
        }
        self.layout_depth += 1;
        true
    }

    fn leave_type(&mut self) {
        self.layout_depth = self.layout_depth.saturating_sub(1);
    }
}

/// Find a top-level interface type declaration by lookup key in a unit AST and
/// return its `TypeExpression`. Only `Type` declarations with a body qualify.
fn find_type_declaration(
    unit: &crate::ast::Unit,
    name_key: crate::context::Identifier,
) -> Option<&crate::ast::TypeExpression> {
    unit.interface_declarations
        .iter()
        .find(|declaration| {
            declaration.kind == crate::ast::DeclarationKind::Type
                && declaration.name.key == name_key
        })
        .and_then(|declaration| declaration.type_expression.as_ref())
}

pub fn evaluate_condition(
    expression: &str,
    resolver: &mut dyn SymbolResolver,
) -> Result<Condition, EvalError> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser {
        tokens,
        position: 0,
        resolver,
    };
    let value = parser.expression()?;
    parser.expect_eof()?;
    match value {
        None => Ok(Condition::Unknown),
        Some(Value::Bool(true)) => Ok(Condition::True),
        Some(Value::Bool(false)) => Ok(Condition::False),
        Some(other) => Err(EvalError::Type {
            message: format!("directive condition must be Boolean, got {other:?}"),
        }),
    }
}

// ─── Lexer ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String), // includes word operators; classified by the parser
    Int(i64),
    /// A literal above `i64::MAX` that fits `u64` (`$FFFFFFFFFFFFFFFF`).
    UInt(u64),
    Float(f64),
    Str(String),
    LParen,
    RParen,
    Comma,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
}

/// Parse an integer literal body in `radix`, producing an `Int` when it fits
/// `i64` else a `UInt` when it fits `u64` (`$FFFFFFFFFFFFFFFF`). A value that
/// fits neither is a `Syntax` error (the caller surfaces it) — never a
/// bit-cast to a wrong number. `label` names the literal kind for the message.
fn parse_radix_token(
    digits: &str,
    radix: u32,
    start: usize,
    label: &str,
) -> Result<Token, EvalError> {
    if let Ok(value) = i64::from_str_radix(digits, radix) {
        return Ok(Token::Int(value));
    }
    if let Ok(value) = u64::from_str_radix(digits, radix) {
        return Ok(Token::UInt(value));
    }
    Err(EvalError::Syntax {
        position: start,
        message: format!("invalid {label} literal: out of range"),
    })
}

fn tokenize(source: &str) -> Result<Vec<(Token, usize)>, EvalError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let start = i;
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'(' => {
                tokens.push((Token::LParen, start));
                i += 1;
            }
            b')' => {
                tokens.push((Token::RParen, start));
                i += 1;
            }
            b',' => {
                // Only meaningful inside a function-call argument list
                // (`Foo(a, b)`); the parser consumes it there and rejects a
                // stray comma elsewhere as a syntax error.
                tokens.push((Token::Comma, start));
                i += 1;
            }
            b'+' => {
                tokens.push((Token::Plus, start));
                i += 1;
            }
            b'-' => {
                tokens.push((Token::Minus, start));
                i += 1;
            }
            b'*' => {
                tokens.push((Token::Star, start));
                i += 1;
            }
            b'/' => {
                tokens.push((Token::Slash, start));
                i += 1;
            }
            b'=' => {
                tokens.push((Token::Eq, start));
                i += 1;
            }
            b'<' => {
                i += 1;
                match bytes.get(i) {
                    Some(b'>') => {
                        tokens.push((Token::Ne, start));
                        i += 1;
                    }
                    Some(b'=') => {
                        tokens.push((Token::Le, start));
                        i += 1;
                    }
                    _ => tokens.push((Token::Lt, start)),
                }
            }
            b'>' => {
                i += 1;
                if bytes.get(i) == Some(&b'=') {
                    tokens.push((Token::Ge, start));
                    i += 1;
                } else {
                    tokens.push((Token::Gt, start));
                }
            }
            b'$' => {
                i += 1;
                let hex_start = i;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if hex_start == i {
                    return Err(EvalError::Syntax {
                        position: start,
                        message: "empty hex literal".into(),
                    });
                }
                let token = parse_radix_token(&source[hex_start..i], 16, start, "hex")?;
                tokens.push((token, start));
            }
            b'0'..=b'9' => {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let mut is_float = false;
                // '.' only counts as decimal point when a digit follows
                // (guards against future range syntax; harmless here).
                if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                    is_float = true;
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i < bytes.len() && (bytes[i] | 0x20) == b'e' {
                    let mut j = i + 1;
                    if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j].is_ascii_digit() {
                        is_float = true;
                        i = j;
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let text = &source[start..i];
                let tok = if is_float {
                    Token::Float(text.parse().map_err(|e| EvalError::Syntax {
                        position: start,
                        message: format!("invalid number: {e}"),
                    })?)
                } else {
                    // decimal: Int if it fits i64, else UInt (u64), else error
                    parse_radix_token(text, 10, start, "integer")?
                };
                tokens.push((tok, start));
            }
            b'\'' => {
                i += 1;
                let mut value = String::new();
                loop {
                    match bytes.get(i) {
                        None => {
                            return Err(EvalError::Syntax {
                                position: start,
                                message: "unterminated string literal".into(),
                            });
                        }
                        Some(b'\'') if bytes.get(i + 1) == Some(&b'\'') => {
                            value.push('\'');
                            i += 2;
                        }
                        Some(b'\'') => {
                            i += 1;
                            break;
                        }
                        Some(_) => {
                            // multi-byte UTF-8 safe: advance by char
                            let ch = source[i..].chars().next().unwrap();
                            value.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                tokens.push((Token::Str(value), start));
            }
            b'%' => {
                i += 1;
                let binary_start = i;
                while i < bytes.len() && (bytes[i] == b'0' || bytes[i] == b'1') {
                    i += 1;
                }
                if binary_start == i {
                    return Err(EvalError::Syntax {
                        position: start,
                        message: "empty binary literal".into(),
                    });
                }
                let token = parse_radix_token(&source[binary_start..i], 2, start, "binary")?;
                tokens.push((token, start));
            }
            b'&' => {
                i += 1;
                match bytes.get(i) {
                    // `&Type` — reserved-word escape: denotes identifier `Type`.
                    Some(&c) if c.is_ascii_alphabetic() || c == b'_' => {
                        let identifier_start = i;
                        while i < bytes.len()
                            && (bytes[i].is_ascii_alphanumeric()
                                || bytes[i] == b'_'
                                || bytes[i] == b'.')
                        {
                            i += 1;
                        }
                        while bytes[i - 1] == b'.' {
                            i -= 1;
                        }
                        tokens
                            .push((Token::Ident(source[identifier_start..i].to_string()), start));
                    }
                    // `&777` — octal literal.
                    Some(&c) if (b'0'..=b'7').contains(&c) => {
                        let octal_start = i;
                        while i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                            i += 1;
                        }
                        let token =
                            parse_radix_token(&source[octal_start..i], 8, start, "octal")?;
                        tokens.push((token, start));
                    }
                    _ => {
                        return Err(EvalError::Syntax {
                            position: start,
                            message: "expected identifier or octal digit after '&'".into(),
                        });
                    }
                }
            }
            b'#' => {
                i += 1;
                let code = if bytes.get(i) == Some(&b'$') {
                    i += 1;
                    let hex_start = i;
                    while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                    if hex_start == i {
                        return Err(EvalError::Syntax {
                            position: start,
                            message: "empty character code".into(),
                        });
                    }
                    u32::from_str_radix(&source[hex_start..i], 16).map_err(|error| {
                        EvalError::Syntax {
                            position: start,
                            message: format!("invalid character code: {error}"),
                        }
                    })?
                } else {
                    let decimal_start = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    if decimal_start == i {
                        return Err(EvalError::Syntax {
                            position: start,
                            message: "empty character code".into(),
                        });
                    }
                    source[decimal_start..i]
                        .parse::<u32>()
                        .map_err(|error| EvalError::Syntax {
                            position: start,
                            message: format!("invalid character code: {error}"),
                        })?
                };
                let character = char::from_u32(code).ok_or_else(|| EvalError::Syntax {
                    position: start,
                    message: "character code is not a valid code point".into(),
                })?;
                tokens.push((Token::Str(character.to_string()), start));
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
                {
                    i += 1;
                }
                // trailing '.' never belongs to the identifier
                while bytes[i - 1] == b'.' {
                    i -= 1;
                }
                tokens.push((Token::Ident(source[start..i].to_string()), start));
            }
            other => {
                return Err(EvalError::Syntax {
                    position: start,
                    message: format!("unexpected character '{}'", other as char),
                });
            }
        }
    }
    Ok(tokens)
}

// ─── Parser / evaluator ──────────────────────────────────────────────────

/// `None` = Unknown, propagated through arithmetic and rescued by Kleene
/// logic in `and`/`or`.
type Evaluated = Option<Value>;

struct Parser<'r> {
    tokens: Vec<(Token, usize)>,
    position: usize,
    resolver: &'r mut dyn SymbolResolver,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position).map(|(t, _)| t)
    }

    fn advance(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.position).map(|(t, _)| t.clone());
        self.position += 1;
        tok
    }

    fn current_position(&self) -> usize {
        self.tokens
            .get(self.position)
            .or_else(|| self.tokens.last())
            .map_or(0, |(_, p)| *p)
    }

    fn syntax(&self, message: impl Into<String>) -> EvalError {
        EvalError::Syntax {
            position: self.current_position(),
            message: message.into(),
        }
    }

    fn expect_eof(&self) -> Result<(), EvalError> {
        if self.position < self.tokens.len() {
            return Err(self.syntax("unexpected trailing tokens"));
        }
        Ok(())
    }

    /// Word-operator check on the upcoming token, case-insensitive.
    fn at_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(identifier)) if identifier.eq_ignore_ascii_case(word))
    }

    // expression := simple (relop simple)*
    fn expression(&mut self) -> Result<Evaluated, EvalError> {
        let mut left = self.simple()?;
        loop {
            let op = match self.peek() {
                Some(Token::Eq) => Comparison::Eq,
                Some(Token::Ne) => Comparison::Ne,
                Some(Token::Lt) => Comparison::Lt,
                Some(Token::Le) => Comparison::Le,
                Some(Token::Gt) => Comparison::Gt,
                Some(Token::Ge) => Comparison::Ge,
                _ => break,
            };
            self.advance();
            let right = self.simple()?;
            left = compare(op, left, right)?;
        }
        Ok(left)
    }

    // simple := term ((+ | - | or | xor) term)*    — Delphi precedence
    fn simple(&mut self) -> Result<Evaluated, EvalError> {
        let mut left = self.term()?;
        loop {
            if matches!(self.peek(), Some(Token::Plus)) {
                self.advance();
                let right = self.term()?;
                left = arithmetic(Arithmetic::Add, left, right)?;
            } else if matches!(self.peek(), Some(Token::Minus)) {
                self.advance();
                let right = self.term()?;
                left = arithmetic(Arithmetic::Sub, left, right)?;
            } else if self.at_word("or") {
                self.advance();
                let right = self.term()?;
                left = logic_or(left, right)?;
            } else if self.at_word("xor") {
                self.advance();
                let right = self.term()?;
                left = logic_xor(left, right)?;
            } else {
                break;
            }
        }
        Ok(left)
    }

    // term := factor ((* | / | div | mod | and | shl | shr) factor)*
    fn term(&mut self) -> Result<Evaluated, EvalError> {
        let mut left = self.factor()?;
        loop {
            if matches!(self.peek(), Some(Token::Star)) {
                self.advance();
                let right = self.factor()?;
                left = arithmetic(Arithmetic::Mul, left, right)?;
            } else if matches!(self.peek(), Some(Token::Slash)) {
                self.advance();
                let right = self.factor()?;
                left = arithmetic(Arithmetic::FloatDiv, left, right)?;
            } else if self.at_word("div") {
                self.advance();
                let right = self.factor()?;
                left = arithmetic(Arithmetic::IntDiv, left, right)?;
            } else if self.at_word("mod") {
                self.advance();
                let right = self.factor()?;
                left = arithmetic(Arithmetic::Mod, left, right)?;
            } else if self.at_word("and") {
                self.advance();
                let right = self.factor()?;
                left = logic_and(left, right)?;
            } else if self.at_word("shl") {
                self.advance();
                let right = self.factor()?;
                left = arithmetic(Arithmetic::Shl, left, right)?;
            } else if self.at_word("shr") {
                self.advance();
                let right = self.factor()?;
                left = arithmetic(Arithmetic::Shr, left, right)?;
            } else {
                break;
            }
        }
        Ok(left)
    }

    // factor := not factor | (+|-) factor | literal | '(' expr ')'
    //         | Defined(id) | Declared(id) | SizeOf(id) | ident
    fn factor(&mut self) -> Result<Evaluated, EvalError> {
        if self.at_word("not") {
            self.advance();
            return logic_not(self.factor()?);
        }
        if matches!(self.peek(), Some(Token::Minus)) {
            self.advance();
            return negate(self.factor()?);
        }
        if matches!(self.peek(), Some(Token::Plus)) {
            self.advance();
            return self.factor();
        }

        match self.advance() {
            Some(Token::Int(v)) => Ok(Some(Value::Int(v))),
            Some(Token::UInt(v)) => Ok(Some(Value::UInt(v))),
            Some(Token::Float(v)) => Ok(Some(Value::Float(v))),
            Some(Token::Str(v)) => Ok(Some(Value::Str(v))),
            Some(Token::LParen) => {
                let value = self.expression()?;
                match self.advance() {
                    Some(Token::RParen) => Ok(value),
                    _ => Err(self.syntax("expected ')'")),
                }
            }
            Some(Token::Ident(identifier)) => self.ident_factor(&identifier),
            _ => Err(self.syntax("expected operand")),
        }
    }

    fn ident_factor(&mut self, identifier: &str) -> Result<Evaluated, EvalError> {
        if identifier.eq_ignore_ascii_case("true") {
            return Ok(Some(Value::Bool(true)));
        }
        if identifier.eq_ignore_ascii_case("false") {
            return Ok(Some(Value::Bool(false)));
        }

        let function = ["defined", "declared", "sizeof"]
            .iter()
            .find(|f| identifier.eq_ignore_ascii_case(f))
            .copied();
        if let Some(function) = function {
            let argument = self.function_argument()?;
            return Ok(match function {
                "defined" => Some(Value::Bool(self.resolver.is_defined(&argument))),
                "declared" => self.resolver.is_declared(&argument).map(Value::Bool),
                _ => self
                    .resolver
                    .size_of(&argument)
                    .map(|size| Value::Int(size as i64)),
            });
        }

        // A general function call in a condition that is NOT one of the three
        // special forms above — `Length(X)`, `Ord(c)`, `High(T)`, `Low(T)`,
        // `Assigned(P)`, etc. dcc can evaluate several of these at compile time
        // (System.pas guards a body with `{$IF Length(RegisteredTypeInfoTable)
        // = 1}`), but doing so needs semantic information this evaluator does
        // not have. The correct degradation is Unknown (never a wrong guess) —
        // but we MUST still consume the `(...)` call syntax so the directive
        // stays well-formed. Leaving it unconsumed turned the whole unit into a
        // HARD parse failure (`CursorError::Condition` Syntax "unexpected
        // trailing tokens"), which skipped the unit entirely; Unknown instead
        // lets the cursor pick a branch under its AssumeFalse/True policy with a
        // diagnostic. See SESSION.md ledger #43.
        if matches!(self.peek(), Some(Token::LParen)) {
            self.consume_call_arguments()?;
            return Ok(None);
        }

        // bare (possibly dotted) identifier: constant lookup
        Ok(self.resolver.const_value(identifier))
    }

    /// Consume a balanced, comma-separated `(...)` argument list, evaluating and
    /// discarding each argument (its value may be Unknown — irrelevant, the
    /// enclosing call is Unknown regardless). Reuses [`Self::expression`] so
    /// nested calls, parentheses, and operators inside arguments are handled by
    /// the same grammar. The opening `(` is the current token on entry.
    fn consume_call_arguments(&mut self) -> Result<(), EvalError> {
        match self.advance() {
            Some(Token::LParen) => {}
            _ => return Err(self.syntax("expected '('")),
        }
        // Empty argument list: `Foo()`.
        if matches!(self.peek(), Some(Token::RParen)) {
            self.advance();
            return Ok(());
        }
        loop {
            let _ = self.expression()?;
            match self.advance() {
                Some(Token::RParen) => break,
                Some(Token::Comma) => continue,
                _ => return Err(self.syntax("expected ',' or ')' in call arguments")),
            }
        }
        Ok(())
    }

    fn function_argument(&mut self) -> Result<String, EvalError> {
        match self.advance() {
            Some(Token::LParen) => {}
            _ => return Err(self.syntax("expected '('")),
        }
        let argument = match self.advance() {
            Some(Token::Ident(identifier)) => identifier,
            _ => return Err(self.syntax("expected identifier argument")),
        };
        match self.advance() {
            Some(Token::RParen) => Ok(argument),
            _ => Err(self.syntax("expected ')'")),
        }
    }
}

// ─── Operations on values ────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Comparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy)]
enum Arithmetic {
    Add,
    Sub,
    Mul,
    FloatDiv,
    IntDiv,
    Mod,
    Shl,
    Shr,
}

fn type_error(message: impl Into<String>) -> EvalError {
    EvalError::Type {
        message: message.into(),
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(v) => Some(*v as f64),
        Value::UInt(v) => Some(*v as f64),
        Value::Float(v) => Some(*v),
        _ => None,
    }
}

/// Exact `i128` view of an integer value (`Int` or `UInt`). `i128` holds every
/// `i64` AND every `u64` without loss, so mixed-width comparisons/arithmetic
/// are exact — no float rounding, no sign-cast corruption. `None` for
/// non-integer values.
fn as_i128(value: &Value) -> Option<i128> {
    match value {
        Value::Int(v) => Some(i128::from(*v)),
        Value::UInt(v) => Some(i128::from(*v)),
        _ => None,
    }
}

/// Narrow an exact `i128` integer result back to the tightest [`Value`]: `Int`
/// if it fits `i64`, else `UInt` if it fits `u64`, else `None` (Unknown) — a
/// value that fits neither is never guessed (L6 discipline).
fn narrow_i128(value: i128) -> Option<Value> {
    if let Ok(v) = i64::try_from(value) {
        Some(Value::Int(v))
    } else if let Ok(v) = u64::try_from(value) {
        Some(Value::UInt(v))
    } else {
        None
    }
}

/// The two's-complement `u64` bit pattern of an integer value — the operand
/// model for bitwise `and`/`or`/`xor`/`not` (Delphi bit-ops act on bit
/// patterns, so `Int(-1)` is `0xFFFF_FFFF_FFFF_FFFF`).
fn as_u64_bits(value: &Value) -> Option<u64> {
    match value {
        Value::Int(v) => Some(*v as u64),
        Value::UInt(v) => Some(*v),
        _ => None,
    }
}

/// Re-tag a bitwise `u64` result as the tightest integer [`Value`].
fn bits_to_value(bits: u64) -> Value {
    if let Ok(v) = i64::try_from(bits) {
        Value::Int(v)
    } else {
        Value::UInt(bits)
    }
}

fn compare(op: Comparison, left: Evaluated, right: Evaluated) -> Result<Evaluated, EvalError> {
    let (Some(left), Some(right)) = (left, right) else {
        return Ok(None); // Unknown operand → Unknown comparison
    };

    let ordering = match (&left, &right) {
        // Both integers (any Int/UInt mix) → EXACT via i128, no float rounding
        // and no sign-cast: `Int(-1) < UInt(x)` and `UInt(huge) > Int(max)` are
        // decided correctly (L6 mixed-width discipline).
        (a, b) if as_i128(a).is_some() && as_i128(b).is_some() => {
            as_i128(a).unwrap().partial_cmp(&as_i128(b).unwrap())
        }
        (Value::Str(a), Value::Str(b)) => a.partial_cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
        _ => match (as_f64(&left), as_f64(&right)) {
            (Some(a), Some(b)) => a.partial_cmp(&b),
            _ => {
                return Err(type_error(format!(
                    "cannot compare {left:?} with {right:?}"
                )));
            }
        },
    };
    let Some(ordering) = ordering else {
        return Ok(None); // NaN involved
    };

    let result = match op {
        Comparison::Eq => ordering.is_eq(),
        Comparison::Ne => ordering.is_ne(),
        Comparison::Lt => ordering.is_lt(),
        Comparison::Le => ordering.is_le(),
        Comparison::Gt => ordering.is_gt(),
        Comparison::Ge => ordering.is_ge(),
    };
    Ok(Some(Value::Bool(result)))
}

fn arithmetic(op: Arithmetic, left: Evaluated, right: Evaluated) -> Result<Evaluated, EvalError> {
    let (Some(left), Some(right)) = (left, right) else {
        return Ok(None);
    };

    // Both operands integral (any Int/UInt mix): compute EXACTLY in i128, then
    // narrow to the tightest Int/UInt — a result that fits neither → Unknown,
    // never a wrong wrap (L6). Shifts operate on the u64 bit pattern (Delphi
    // bit semantics), which is exact for both signed and unsigned operands.
    if let (Some(a), Some(b)) = (as_i128(&left), as_i128(&right)) {
        match op {
            // Operands range over [i64::MIN, u64::MAX]; their sum/difference/
            // product can exceed i128 range (e.g. UInt(u64::MAX) squared ≈ 2^128
            // > i128::MAX). Use checked i128 ops so an intermediate overflow maps
            // to Unknown (Ok(None)) instead of panicking (debug) or wrapping to a
            // wrong number (release). `narrow_i128` still guards the final fit.
            Arithmetic::Add => return Ok(a.checked_add(b).and_then(narrow_i128)),
            Arithmetic::Sub => return Ok(a.checked_sub(b).and_then(narrow_i128)),
            Arithmetic::Mul => return Ok(a.checked_mul(b).and_then(narrow_i128)),
            Arithmetic::FloatDiv => {
                if b == 0 {
                    return Err(type_error("division by zero"));
                }
                return Ok(Some(Value::Float(a as f64 / b as f64)));
            }
            Arithmetic::IntDiv => {
                if b == 0 {
                    return Err(type_error("division by zero"));
                }
                // i128 division of two values each within i64/u64 range always
                // fits i64/u64 again (magnitude cannot grow), so narrow is exact.
                return Ok(narrow_i128(a / b));
            }
            Arithmetic::Mod => {
                if b == 0 {
                    return Err(type_error("division by zero"));
                }
                return Ok(narrow_i128(a % b));
            }
            Arithmetic::Shl | Arithmetic::Shr => {
                // Bit-pattern domain: exact for signed and unsigned alike.
                let bits = as_u64_bits(&left).unwrap();
                let shift = (as_u64_bits(&right).unwrap() & 63) as u32;
                let result = match op {
                    Arithmetic::Shl => bits << shift,
                    _ => bits >> shift,
                };
                return Ok(Some(bits_to_value(result)));
            }
        }
    }

    let (Some(a), Some(b)) = (as_f64(&left), as_f64(&right)) else {
        return Err(type_error(format!(
            "invalid operands {left:?} and {right:?}"
        )));
    };
    match op {
        Arithmetic::Add => Ok(Some(Value::Float(a + b))),
        Arithmetic::Sub => Ok(Some(Value::Float(a - b))),
        Arithmetic::Mul => Ok(Some(Value::Float(a * b))),
        Arithmetic::FloatDiv => {
            if b == 0.0 {
                return Err(type_error("division by zero"));
            }
            Ok(Some(Value::Float(a / b)))
        }
        _ => Err(type_error("integer operator on floating-point operand")),
    }
}

fn negate(value: Evaluated) -> Result<Evaluated, EvalError> {
    match value {
        None => Ok(None),
        // Negate exactly in i128, then narrow. `-i64::MIN` and `-(2^63)` both
        // land exactly (i64::MIN); a magnitude too large for i64 and negative
        // (so not u64 either) → Unknown, never a wrong wrap.
        Some(Value::Int(v)) => Ok(narrow_i128(-i128::from(v))),
        Some(Value::UInt(v)) => Ok(narrow_i128(-i128::from(v))),
        Some(Value::Float(v)) => Ok(Some(Value::Float(-v))),
        Some(other) => Err(type_error(format!("cannot negate {other:?}"))),
    }
}

/// Kleene three-valued AND; bitwise on integers. `Unknown AND False = False`.
fn logic_and(left: Evaluated, right: Evaluated) -> Result<Evaluated, EvalError> {
    match (left, right) {
        (Some(Value::Bool(false)), _) | (_, Some(Value::Bool(false))) => {
            Ok(Some(Value::Bool(false)))
        }
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => Ok(Some(Value::Bool(a && b))),
        (Some(a), Some(b)) if as_u64_bits(&a).is_some() && as_u64_bits(&b).is_some() => {
            Ok(Some(bits_to_value(as_u64_bits(&a).unwrap() & as_u64_bits(&b).unwrap())))
        }
        (None, _) | (_, None) => Ok(None),
        (Some(a), Some(b)) => Err(type_error(format!("invalid operands for and: {a:?}, {b:?}"))),
    }
}

/// Kleene three-valued OR; bitwise on integers. `Unknown OR True = True`.
fn logic_or(left: Evaluated, right: Evaluated) -> Result<Evaluated, EvalError> {
    match (left, right) {
        (Some(Value::Bool(true)), _) | (_, Some(Value::Bool(true))) => Ok(Some(Value::Bool(true))),
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => Ok(Some(Value::Bool(a || b))),
        (Some(a), Some(b)) if as_u64_bits(&a).is_some() && as_u64_bits(&b).is_some() => {
            Ok(Some(bits_to_value(as_u64_bits(&a).unwrap() | as_u64_bits(&b).unwrap())))
        }
        (None, _) | (_, None) => Ok(None),
        (Some(a), Some(b)) => Err(type_error(format!("invalid operands for or: {a:?}, {b:?}"))),
    }
}

fn logic_xor(left: Evaluated, right: Evaluated) -> Result<Evaluated, EvalError> {
    match (left, right) {
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => Ok(Some(Value::Bool(a ^ b))),
        (Some(a), Some(b)) if as_u64_bits(&a).is_some() && as_u64_bits(&b).is_some() => {
            Ok(Some(bits_to_value(as_u64_bits(&a).unwrap() ^ as_u64_bits(&b).unwrap())))
        }
        (None, _) | (_, None) => Ok(None), // xor never rescues Unknown
        (Some(a), Some(b)) => Err(type_error(format!("invalid operands for xor: {a:?}, {b:?}"))),
    }
}

fn logic_not(value: Evaluated) -> Result<Evaluated, EvalError> {
    match value {
        None => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(Value::Bool(!v))),
        Some(Value::Int(v)) => Ok(Some(Value::Int(!v))),
        // bitwise complement of the u64 bit pattern, re-tagged to tightest type
        Some(Value::UInt(v)) => Ok(Some(bits_to_value(!v))),
        Some(other) => Err(type_error(format!("invalid operand for not: {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delphi-12-ish mock: FOO defined, TBar 8 bytes declared, size of
    /// TUnknownRec not answerable.
    struct Mock;

    impl SymbolResolver for Mock {
        fn is_defined(&mut self, symbol: &str) -> bool {
            symbol.eq_ignore_ascii_case("FOO")
        }
        fn is_declared(&mut self, identifier: &str) -> Option<bool> {
            match identifier.to_uppercase().as_str() {
                "TBAR" => Some(true),
                "TUNKNOWNREC" => None,
                _ => Some(false),
            }
        }
        fn size_of(&mut self, type_name: &str) -> Option<u64> {
            match type_name.to_uppercase().as_str() {
                "TBAR" => Some(8),
                "POINTER" => Some(4),
                _ => None,
            }
        }
        fn const_value(&mut self, identifier: &str) -> Option<Value> {
            match identifier.to_uppercase().as_str() {
                "COMPILERVERSION" | "RTLVERSION" => Some(Value::Float(36.0)),
                _ => None,
            }
        }
    }

    fn eval(source: &str) -> Result<Condition, EvalError> {
        evaluate_condition(source, &mut Mock)
    }

    #[test]
    fn defined_and_version() {
        assert_eq!(eval("Defined(FOO)"), Ok(Condition::True));
        assert_eq!(eval("Defined(bar)"), Ok(Condition::False));
        assert_eq!(eval("CompilerVersion >= 35.0"), Ok(Condition::True));
        assert_eq!(eval("CompilerVersion > 36"), Ok(Condition::False));
        assert_eq!(eval("RTLVersion >= 20"), Ok(Condition::True));
    }

    #[test]
    fn sizeof_and_declared() {
        assert_eq!(eval("SizeOf(TBar) >= 4"), Ok(Condition::True));
        assert_eq!(eval("SizeOf(Pointer) = 8"), Ok(Condition::False));
        assert_eq!(eval("Declared(TBar)"), Ok(Condition::True));
        assert_eq!(eval("Declared(Nowhere)"), Ok(Condition::False));
        assert_eq!(eval("Declared(TUnknownRec)"), Ok(Condition::Unknown));
        assert_eq!(eval("SizeOf(TUnknownRec) > 4"), Ok(Condition::Unknown));
    }

    #[test]
    fn kleene_rescue() {
        // Unknown AND False = False — definite despite unevaluable SizeOf
        assert_eq!(
            eval("Defined(MISSING) and (SizeOf(TUnknownRec) > 4)"),
            Ok(Condition::False)
        );
        // Unknown OR True = True
        assert_eq!(
            eval("(SizeOf(TUnknownRec) > 4) or Defined(FOO)"),
            Ok(Condition::True)
        );
        // Unknown AND True = Unknown
        assert_eq!(
            eval("Defined(FOO) and (SizeOf(TUnknownRec) > 4)"),
            Ok(Condition::Unknown)
        );
        assert_eq!(eval("not (SizeOf(TUnknownRec) > 4)"), Ok(Condition::Unknown));
    }

    #[test]
    fn precedence_delphi_style() {
        // 'and' binds at multiplication level, 'or' at addition level
        assert_eq!(eval("True or False and False"), Ok(Condition::True));
        assert_eq!(eval("(True or False) and False"), Ok(Condition::False));
        assert_eq!(eval("2 + 3 * 4 = 14"), Ok(Condition::True));
        assert_eq!(eval("10 div 3 = 3"), Ok(Condition::True));
        assert_eq!(eval("10 mod 3 = 1"), Ok(Condition::True));
        assert_eq!(eval("1 shl 4 = $10"), Ok(Condition::True));
        assert_eq!(eval("-5 + 5 = 0"), Ok(Condition::True));
        assert_eq!(eval("10 / 4 = 2.5"), Ok(Condition::True));
    }

    #[test]
    fn strings_and_case() {
        assert_eq!(eval("'abc' = 'abc'"), Ok(Condition::True));
        assert_eq!(eval("'it''s' = 'it''s'"), Ok(Condition::True));
        assert_eq!(eval("dEfInEd(foo) AND TRUE"), Ok(Condition::True));
    }

    #[test]
    fn large_unsigned_constants_evaluate() {
        // L6: $FFFFFFFFFFFFFFFF (u64::MAX, above i64::MAX) is captured as a UInt
        // and compares exactly — the old i64 tokenizer errored on it.
        assert_eq!(eval("$FFFFFFFFFFFFFFFF = $FFFFFFFFFFFFFFFF"), Ok(Condition::True));
        assert_eq!(eval("18446744073709551615 = $FFFFFFFFFFFFFFFF"), Ok(Condition::True));
        // mixed Int/UInt comparison is EXACT (i128), no float rounding: a huge
        // UInt is greater than any i64, and a negative Int is less than any UInt.
        assert_eq!(eval("$FFFFFFFFFFFFFFFF > 9223372036854775807"), Ok(Condition::True));
        assert_eq!(eval("-1 < $8000000000000000"), Ok(Condition::True));
        // exact equality that a float round-trip would BREAK (both differ by 1
        // in the last bit, indistinguishable as f64) is still decided correctly
        assert_eq!(eval("$FFFFFFFFFFFFFFFF = $FFFFFFFFFFFFFFFE"), Ok(Condition::False));
        // a value beyond u64 fits NEITHER → Syntax error (never a wrong number)
        assert!(matches!(
            eval("$1FFFFFFFFFFFFFFFF = 0"),
            Err(EvalError::Syntax { .. })
        ));
    }

    #[test]
    fn mixed_width_arithmetic_overflow_is_unknown() {
        // L6: an intermediate product/sum/difference can exceed i128 range
        // (UInt(u64::MAX)^2 ≈ 2^128 > i128::MAX). Checked i128 arithmetic maps
        // that to Unknown — NEVER a panic (debug 'attempt to multiply with
        // overflow') and NEVER a wrong wrapped number (release).
        assert_eq!(
            eval("$FFFFFFFFFFFFFFFF * $FFFFFFFFFFFFFFFF = 0"),
            Ok(Condition::Unknown)
        );
        // Add just below the u64 ceiling would exceed u64 but still fits i128,
        // so it narrows cleanly to Unknown only when it leaves the u64 range;
        // the sum here is 2^64 which fits neither Int nor UInt → Unknown.
        assert_eq!(
            eval("$FFFFFFFFFFFFFFFF + 1 = 0"),
            Ok(Condition::Unknown)
        );
        // Subtraction underflow: a large negative magnitude that fits neither
        // i64 (too negative? no) — here i64::MIN - u64::MAX is far below i64::MIN
        // and negative so not u64 → Unknown, not a wrong wrap.
        assert_eq!(
            eval("-9223372036854775808 - $FFFFFFFFFFFFFFFF = 0"),
            Ok(Condition::Unknown)
        );
    }

    #[test]
    fn binary_octal_char_literals() {
        // M5: previously these aborted the whole unit with a Syntax error.
        assert_eq!(eval("%1010 = 10"), Ok(Condition::True));
        assert_eq!(eval("&777 = 511"), Ok(Condition::True));
        assert_eq!(eval("#65 = 'A'"), Ok(Condition::True));
        assert_eq!(eval("#$41 = 'A'"), Ok(Condition::True));
        assert!(matches!(eval("%"), Err(EvalError::Syntax { .. })));
    }

    #[test]
    fn general_function_call_is_unknown_not_error() {
        // Ledger #43: a directive that calls a function OTHER than
        // Defined/Declared/SizeOf — System.pas guards a body with
        // `{$IF Length(RegisteredTypeInfoTable) = 1}`. We cannot evaluate it,
        // but it must degrade to Unknown, NOT a hard Syntax error that fails
        // the whole unit (which was skipping System.pas from the RTL bootstrap).
        assert_eq!(eval("Length(RegisteredTypeInfoTable) = 1"), Ok(Condition::Unknown));
        // bare call in boolean position
        assert_eq!(eval("Assigned(SomePointer)"), Ok(Condition::Unknown));
        // Kleene rescue still works around an unknown call
        assert_eq!(eval("Defined(MISSING) and (Length(X) = 1)"), Ok(Condition::False));
        assert_eq!(eval("Defined(FOO) or (Length(X) = 1)"), Ok(Condition::True));
        // empty and multi-argument calls (the tokenizer now knows ',')
        assert_eq!(eval("Foo() = 1"), Ok(Condition::Unknown));
        assert_eq!(eval("Max(A, B) > 0"), Ok(Condition::Unknown));
        // nested calls / parenthesised arguments consume correctly
        assert_eq!(eval("Length(Copy(S, 1, 2)) = 0"), Ok(Condition::Unknown));
        // an unterminated call is still a Syntax error (never silently accepted)
        assert!(matches!(eval("Length(X = 1"), Err(EvalError::Syntax { .. })));
        // a stray comma OUTSIDE a call is a Syntax error, not a swallowed token
        assert!(matches!(eval("1, 2"), Err(EvalError::Syntax { .. })));
    }

    #[test]
    fn escaped_identifier_in_declared() {
        // H1: `&Type` denotes identifier `Type`; `&TBar` must resolve to TBar,
        // and must not raise a Syntax error on the leading `&`.
        assert_eq!(eval("Declared(&TBar)"), Ok(Condition::True));
        assert_eq!(eval("Declared(&Nowhere)"), Ok(Condition::False));
    }

    #[test]
    fn negate_min_int_does_not_panic() {
        // M6 + L6: `1 shl 63` no longer wraps to a negative i64 — it is now the
        // exact unsigned value 2^63 (UInt), and `-(2^63)` narrows to i64::MIN
        // (representable), so `-(1 shl 63) < 0` is a well-formed True — no panic,
        // no bogus overflow error. The mixed-width machinery makes it exact.
        assert_eq!(eval("-(1 shl 63) < 0"), Ok(Condition::True));
    }

    #[test]
    fn errors() {
        assert!(matches!(eval("1 +"), Err(EvalError::Syntax { .. })));
        assert!(matches!(eval("(True"), Err(EvalError::Syntax { .. })));
        assert!(matches!(eval("1 + 'x'"), Err(EvalError::Type { .. })));
        assert!(matches!(eval("1 + 2"), Err(EvalError::Type { .. }))); // non-bool top level
        assert!(matches!(eval("1 div 0"), Err(EvalError::Type { .. })));
        assert!(matches!(eval("True False"), Err(EvalError::Syntax { .. })));
    }
}
