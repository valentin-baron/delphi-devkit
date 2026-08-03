//! Implementation-section semantic AST — Stage S1: the **expression layer**.
//!
//! This module defines the [`Expression`] tree the implementation-section parser
//! builds over routine bodies (see `impl-ast-spec.md`), plus a precedence-climbing
//! (Pratt) expression parser implemented as `impl UnitParser` methods in
//! `parser.rs` (kept there because the parser needs the crate-private cursor
//! helpers; the grammar is nonetheless cohesive — every expression production
//! lives in the `// ─── Expression parser (Stage S1) ───` block of that file).
//!
//! Design invariants inherited from the interface AST (`ast.rs`):
//! * Identifiers are [`QualifiedName`] — dual-track interned (display + folded
//!   key) and carrying their own source span.
//! * Every literal / opaque region is a [`CodeLocation`] span; the AST carries no
//!   strings and never eagerly evaluates an expression leaf.
//! * Recursive positions are `Box`ed.
//!
//! S1 is standalone: nothing here is wired into `UnitMeta`, serialization or the
//! query layer yet (that is S2–S4). It exists to be unit-tested in isolation.

use serde::{Deserialize, Serialize};

use crate::ast::{LocalKind, QualifiedName, RoutineKind};
use crate::context::Identifier;
use crate::meta::{CodeLocation, Span};

/// A prefix / postfix unary operator over a single operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOperator {
    /// `@x` — address-of.
    AddressOf,
    /// `not x` — logical / bitwise negation.
    Not,
    /// unary `-x` — arithmetic negation.
    Negate,
    /// unary `+x` — arithmetic identity (accepted for symmetry; a no-op).
    Plus,
    /// `p^` — pointer dereference. This is a POSTFIX operator in Delphi
    /// (`pointer^`), unlike the prefix operators above; the parser applies it in
    /// the postfix chain but models it as a `Unary` so the receiver shape stays
    /// uniform.
    Dereference,
}

/// A binary operator. Membership (`in`), the type test (`is`) and the comparison
/// operators are all modelled here; the value/type-producing `as` cast is NOT a
/// `Binary` — it becomes an [`Expression::Cast`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOperator {
    // multiplicative level
    Multiply,
    Divide,
    IntegerDivide, // div
    Modulo,        // mod
    And,           // and
    ShiftLeft,     // shl
    ShiftRight,    // shr
    // additive level
    Add,
    Subtract,
    Or,  // or
    Xor, // xor
    // relational level
    Equal,        // =
    NotEqual,     // <>
    Less,         // <
    Greater,      // >
    LessEqual,    // <=
    GreaterEqual, // >=
    In,           // in
    Is,           // is (type test)
}

/// Index of an `Expression` in its owning [`ImplementationBody::expression_arena`].
///
/// The whole implementation-body expression tree is flattened into ONE arena
/// `Vec<Expression>` per unit: every interior child position that used to be a
/// `Box<Expression>` / `Vec<Expression>` is now an `ExprId` / `Vec<ExprId>` that
/// indexes that single arena. This trades N per-node `Box` allocations for one
/// `Vec` allocation over the whole body (the body is the code-heavy per-unit
/// parse). `ExprId` is a plain `Copy` index — it carries no lifetime and never
/// dangles as long as it is resolved against the SAME body's arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExprId(pub u32);

/// A receiver-shaped expression — enough structure to type a member/call chain
/// for completion and go-to, NOT enough to evaluate. Leaves that carry no
/// navigational structure (literals, set/array bodies, anonymous-method bodies)
/// are kept as opaque [`CodeLocation`] spans.
///
/// Recursive child positions are stored as [`ExprId`] indices into the owning
/// [`ImplementationBody::expression_arena`], NOT `Box`es — see [`ExprId`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expression {
    /// A bare (possibly dotted) name: `Foo`, `System.Classes`.
    Identifier(QualifiedName),
    /// `receiver.member` — a member access.
    Member {
        receiver: ExprId,
        member: QualifiedName,
    },
    /// `callee(arguments)` — a call / value-cast `T(x)`. The `arguments_span`
    /// covers the whole `(...)` group (for hover / signature-help ranges).
    Call {
        callee: ExprId,
        arguments: Vec<ExprId>,
        arguments_span: CodeLocation,
    },
    /// `base[indices]` — an array / default-property index (`a[i, j]`).
    Index {
        base: ExprId,
        indices: Vec<ExprId>,
    },
    /// `operand as type_name` — the class-reference `as` cast. (`type_name(x)`
    /// value casts stay a [`Expression::Call`]; disambiguating type-vs-value is a
    /// semantic concern, deliberately not attempted here.)
    Cast {
        type_name: QualifiedName,
        operand: ExprId,
    },
    /// `inherited` (bare) or `inherited Method[(args)]`. When a method name and/or
    /// arguments follow, the whole `inherited Foo(...)` postfix chain is built
    /// with this node as its innermost receiver; `method` holds the immediate
    /// method name that followed `inherited`, when present.
    ///
    /// `keyword_location` is the span of the `inherited` keyword itself. It is
    /// what makes a BARE `inherited` (no method name) navigable — a cursor on the
    /// keyword has no `method` span to land on, so the locator matches this span.
    Inherited {
        method: Option<QualifiedName>,
        keyword_location: CodeLocation,
    },
    /// A prefix (`@ not - +`) or postfix (`^`) unary application.
    Unary {
        operator: UnaryOperator,
        operand: ExprId,
    },
    /// A binary application (see [`BinaryOperator`]).
    Binary {
        operator: BinaryOperator,
        left: ExprId,
        right: ExprId,
    },
    /// An anonymous-method opener in expression position — `procedure … end`,
    /// `function … end` or `reference to …`. The closure body is a real child
    /// [`Scope`] of kind [`ScopeKind::Anonymous`]: its params, inline vars and
    /// statements belong to it, so a cursor inside the closure resolves against
    /// the closure's own symbols rather than the enclosing routine's.
    AnonymousMethod(Box<Scope>),
    /// `[a, b, c]` — a set or array-constructor literal. The bracket group span
    /// is captured; identifier occurrences INSIDE are still recorded (so the
    /// reference index stays complete) even though the elements are not retained
    /// as sub-expressions.
    SetOrArrayLiteral(CodeLocation),
    /// A number / string / `nil` / `True` / `False` literal — an opaque leaf.
    Literal(CodeLocation),
    /// `(inner)` — an explicitly parenthesized sub-expression, retained so a
    /// later walk can distinguish grouping from precedence.
    Parenthesized(ExprId),
}

// ─── Statement / scope layer (Stage S2) ──────────────────────────────────────
//
// The scope tree is the real model of the implementation section: every lexical
// scope (free routine, method, nested routine, anonymous method, `with`-block)
// is a [`Scope`] carrying its own local symbols and a shallow statement list.
// Statements are shallow on purpose — we model scope structure and expression
// occurrences, NOT control-flow semantics: a control-flow construct's condition
// and bound expressions are pulled into the list as plain [`Statement::Expression`]s
// and its branch bodies recurse as [`Statement::Group`]s, but the parser never
// tags which control-flow form produced them.
//
// Every leaf is a span or a declaration key (the "AST carries no strings /
// expressions are spans" invariant continues to hold). All types derive
// `Serialize + Deserialize + Clone + Debug`.

/// One body-local declaration — a parameter, a declaration-part `var`/`const`/
/// `type`/`label`, or an inline `var`/`const` introduced mid-body. `name` gives
/// both the folded lookup key and the exact source span of the declaring
/// occurrence (`QualifiedName` carries `key` + `location`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSymbol {
    /// The declared name — its folded lookup key and its own declaration span.
    pub name: QualifiedName,
    pub kind: LocalKind,
    /// The declared type as a SIMPLE reference key (`Local: TThing` → `TThing`),
    /// else `None` for an anonymous/complex/absent type. Captured only when
    /// trivial; a later stage refines it.
    pub type_key: Option<Identifier>,
}

/// The kind of a lexical [`Scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeKind {
    /// A free routine body (`procedure Foo; … begin … end`).
    Routine,
    /// A routine nested in another routine's declaration part.
    Nested,
    /// An anonymous method / closure body.
    Anonymous,
    /// A method body (`procedure TFoo.Bar`): `self_type_key` is `Some(TFoo)`, so
    /// bare member names / `Self` resolve against the owner type.
    Method,
    /// A `with E do …` block — its receivers' types open a member scope.
    With,
}

/// One lexical scope: its kind, whole-extent span, the type its `Self` binds to
/// (for methods), its local declarations and its shallow statement list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub kind: ScopeKind,
    /// The whole scope extent — a body position is enclosed when its offset falls
    /// inside this span. For nested scopes the tightest-covering span wins.
    pub span: Span,
    /// `Some(TFoo)` inside a method body (bare members / `Self` resolve against
    /// it); `None` for a free routine / closure / nested routine / with-block.
    pub self_type_key: Option<Identifier>,
    /// Params + declaration-part `var`/`const`/`type`/`label` + inline vars.
    pub declarations: Vec<LocalSymbol>,
    pub statements: StatementList,
}

/// A shallow list of statements (see the module note above).
pub type StatementList = Vec<Statement>;

/// One shallow statement. Control-flow forms are NOT modelled as typed nodes;
/// their condition/selector/bound expressions become [`Statement::Expression`]s
/// and their sub-statement bodies recurse as [`Statement::Group`]s, so every
/// identifier occurrence and nested scope is still captured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Statement {
    /// A bare expression / call statement. The [`ExprId`] indexes the owning
    /// [`ImplementationBody::expression_arena`].
    Expression(ExprId),
    /// `target := value`.
    Assignment {
        target: ExprId,
        value: ExprId,
    },
    /// An inline `var X [: T] [:= expr];` / `const X = expr;` (Delphi 10.3+). The
    /// symbol is ALSO appended to the enclosing scope's `declarations` so it
    /// resolves; this node keeps its optional initializer expression.
    LocalVar(LocalSymbol, Option<ExprId>),
    /// `with E1, E2 do body` — `items` are the receiver expressions (their types
    /// open a member scope later); `body` is the following statement(s).
    With {
        items: Vec<ExprId>,
        body: StatementList,
    },
    /// A nested routine / anonymous-method scope reachable in the tree.
    ChildScope(Box<Scope>),
    /// A `begin`/`case`/`try`/branch body flattened: condition/selector
    /// expressions are pulled in as `Expression` statements and branch bodies
    /// recurse. We do NOT tag which control-flow form it was.
    Group(StatementList),
    /// A region we chose not to model, still scanned for identifier occurrences
    /// so references stay complete.
    Opaque(CodeLocation),
}

/// One implementation-section routine definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineImplementation {
    /// The header name occurrence (`procedure TThing.Run` → the `Run` occurrence).
    pub name: QualifiedName,
    /// `Some(TThing)` for a qualified `procedure TThing.Run`; `None` for a free
    /// routine.
    pub owner_type_key: Option<Identifier>,
    /// procedure / function / constructor / destructor / operator.
    pub kind: RoutineKind,
    /// The routine's own scope (kind `Method` when `owner_type_key.is_some()`,
    /// else `Routine`).
    pub scope: Scope,
}

/// The whole implementation section as a scope tree plus the unit's
/// initialization / finalization statement lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementationBody {
    /// The single per-unit expression arena. Every [`ExprId`] appearing anywhere
    /// in this body (in a [`Statement`], or as a child of another `Expression`)
    /// indexes THIS vector. It is the one owner of the whole body's expression
    /// tree — one `Vec` allocation instead of one `Box` per interior node. Roots
    /// are referenced from the `Statement`s; there is no separate root list.
    pub expression_arena: Vec<Expression>,
    /// Top-level impl routines, in source order.
    pub routines: Vec<RoutineImplementation>,
    /// `initialization … end.` block (or the unit's `begin … end.` init block).
    pub initialization: Option<StatementList>,
    /// `finalization … end.` block.
    pub finalization: Option<StatementList>,
    /// Whole-section clean-parse flag: `true` only when nothing degraded. The
    /// it.15 query gate reads this (mirrors the derived `impl_scopes_reliable`).
    pub reliable: bool,
}

impl Default for ImplementationBody {
    fn default() -> Self {
        Self {
            expression_arena: Vec::new(),
            routines: Vec::new(),
            initialization: None,
            finalization: None,
            reliable: true,
        }
    }
}

impl ImplementationBody {
    /// Resolve an [`ExprId`] to its `Expression` in this body's arena. Panics on
    /// an out-of-range id (a well-formed body only ever holds ids it allocated).
    pub fn expression(&self, id: ExprId) -> &Expression {
        &self.expression_arena[id.0 as usize]
    }

    /// Rebuild the flat [`crate::ast::ImplRoutine`] table (params/locals per
    /// routine body, plus the whole-body span) DERIVED from this scope tree — the
    /// single-source-of-truth replacement for the separately-stored `impl_scopes`
    /// vector. Emits ONE `ImplRoutine` per routine-defining scope
    /// ([`ScopeKind::Routine`], [`ScopeKind::Method`], [`ScopeKind::Nested`]),
    /// exactly the scopes the parser recorded into the old flat table; anonymous
    /// methods and `with`-blocks carry no `ImplRoutine` (they never did). Byte-
    /// identical to what `record_impl_routine_from_declarations` produced: params
    /// are the leading [`LocalKind::Param`] declarations, locals are the
    /// declaration-part `var`/`const`/`type`/`label` entries (inline vars, which
    /// the flat table never held, are excluded). Same-unit local resolution
    /// ([`crate::driver`] `local_at`/hover) walks the result unchanged.
    pub fn flatten_impl_routines(&self) -> Vec<crate::ast::ImplRoutine> {
        let mut routines = Vec::new();
        for routine in &self.routines {
            emit_routine_scope(
                &routine.scope,
                routine.name,
                routine.owner_type_key,
                &self.expression_arena,
                &mut routines,
            );
        }
        routines
    }

    /// Structural node count over the whole body — a cheap, allocation-free walk
    /// the moka weigher charges per node (statements + expressions + scopes +
    /// declarations). Never touches the derived flat table (weigher runs under
    /// moka's insert lock, ledger #29).
    pub fn node_count(&self) -> usize {
        let mut nodes = 0usize;
        for routine in &self.routines {
            nodes += 1; // the RoutineImplementation header
            count_scope_nodes(&routine.scope, &self.expression_arena, &mut nodes);
        }
        if let Some(initialization) = &self.initialization {
            count_statements_nodes(initialization, &self.expression_arena, &mut nodes);
        }
        if let Some(finalization) = &self.finalization {
            count_statements_nodes(finalization, &self.expression_arena, &mut nodes);
        }
        nodes
    }
}

/// Emit one routine-defining scope's `ImplRoutine`, then descend into every
/// nested routine (a `ChildScope`, and closures' nested routines) so the flat
/// table matches the parser's recording shape. Anonymous methods and `with`
/// scopes carry no entry (they never did). `enclosing_name` supplies the `name`
/// field for nested `ChildScope` entries — a `Scope` stores no header name, and
/// the flat-table consumer keys on `body_span`, never on a nested entry's
/// `name`, so threading the enclosing routine's (already-interned) name keeps the
/// field well-formed without inventing a wrong lookup key.
fn emit_routine_scope(
    scope: &Scope,
    enclosing_name: QualifiedName,
    owner_type_key: Option<Identifier>,
    arena: &[Expression],
    out: &mut Vec<crate::ast::ImplRoutine>,
) {
    use crate::ast::{ImplRoutine, LocalDeclaration};
    if matches!(scope.kind, ScopeKind::Routine | ScopeKind::Method | ScopeKind::Nested) {
        let mut params = Vec::new();
        let mut locals = Vec::new();
        for symbol in &scope.declarations {
            let declaration = LocalDeclaration {
                name: symbol.name,
                decl_kind: symbol.kind,
                type_key: symbol.type_key,
            };
            match symbol.kind {
                LocalKind::Param => params.push(declaration),
                // Inline vars were never part of the flat table (the S2 builder
                // appends them to the scope's declarations); exclude them so the
                // derived table is byte-identical to the parser's recording.
                LocalKind::InlineVar => {}
                _ => locals.push(declaration),
            }
        }
        out.push(ImplRoutine {
            name: enclosing_name,
            owner_type_key,
            body_span: scope.span,
            params,
            locals,
        });
    }
    collect_nested_routines(&scope.statements, enclosing_name, arena, out);
}

/// Find every nested routine scope reachable from a statement list and emit its
/// `ImplRoutine` (via [`emit_routine_scope`]).
fn collect_nested_routines(
    statements: &[Statement],
    enclosing_name: QualifiedName,
    arena: &[Expression],
    out: &mut Vec<crate::ast::ImplRoutine>,
) {
    for statement in statements {
        match statement {
            Statement::ChildScope(scope) => {
                emit_routine_scope(scope, enclosing_name, scope.self_type_key, arena, out)
            }
            Statement::Group(inner) => collect_nested_routines(inner, enclosing_name, arena, out),
            Statement::With { items, body } => {
                for item in items {
                    collect_nested_routines_in_expression(*item, enclosing_name, arena, out);
                }
                collect_nested_routines(body, enclosing_name, arena, out);
            }
            Statement::Expression(expression) => {
                collect_nested_routines_in_expression(*expression, enclosing_name, arena, out)
            }
            Statement::Assignment { target, value } => {
                collect_nested_routines_in_expression(*target, enclosing_name, arena, out);
                collect_nested_routines_in_expression(*value, enclosing_name, arena, out);
            }
            Statement::LocalVar(_, Some(expression)) => {
                collect_nested_routines_in_expression(*expression, enclosing_name, arena, out)
            }
            Statement::LocalVar(_, None) | Statement::Opaque(_) => {}
        }
    }
}

/// Descend into an expression for anonymous-method bodies, whose nested routines
/// (but not the anonymous scope itself) belong in the flat table.
fn collect_nested_routines_in_expression(
    expression: ExprId,
    enclosing_name: QualifiedName,
    arena: &[Expression],
    out: &mut Vec<crate::ast::ImplRoutine>,
) {
    match &arena[expression.0 as usize] {
        Expression::AnonymousMethod(scope) => {
            collect_nested_routines(&scope.statements, enclosing_name, arena, out)
        }
        Expression::Member { receiver, .. } => {
            collect_nested_routines_in_expression(*receiver, enclosing_name, arena, out)
        }
        Expression::Call { callee, arguments, .. } => {
            collect_nested_routines_in_expression(*callee, enclosing_name, arena, out);
            for argument in arguments {
                collect_nested_routines_in_expression(*argument, enclosing_name, arena, out);
            }
        }
        Expression::Index { base, indices } => {
            collect_nested_routines_in_expression(*base, enclosing_name, arena, out);
            for index in indices {
                collect_nested_routines_in_expression(*index, enclosing_name, arena, out);
            }
        }
        Expression::Cast { operand, .. } => {
            collect_nested_routines_in_expression(*operand, enclosing_name, arena, out)
        }
        Expression::Unary { operand, .. } => {
            collect_nested_routines_in_expression(*operand, enclosing_name, arena, out)
        }
        Expression::Binary { left, right, .. } => {
            collect_nested_routines_in_expression(*left, enclosing_name, arena, out);
            collect_nested_routines_in_expression(*right, enclosing_name, arena, out);
        }
        Expression::Parenthesized(inner) => {
            collect_nested_routines_in_expression(*inner, enclosing_name, arena, out)
        }
        _ => {}
    }
}

fn count_scope_nodes(scope: &Scope, arena: &[Expression], nodes: &mut usize) {
    *nodes += 1 + scope.declarations.len();
    count_statements_nodes(&scope.statements, arena, nodes);
}

fn count_statements_nodes(statements: &[Statement], arena: &[Expression], nodes: &mut usize) {
    for statement in statements {
        *nodes += 1;
        match statement {
            Statement::Expression(expression) => count_expression_nodes(*expression, arena, nodes),
            Statement::Assignment { target, value } => {
                count_expression_nodes(*target, arena, nodes);
                count_expression_nodes(*value, arena, nodes);
            }
            Statement::LocalVar(_, Some(expression)) => {
                count_expression_nodes(*expression, arena, nodes)
            }
            Statement::LocalVar(_, None) => {}
            Statement::With { items, body } => {
                for item in items {
                    count_expression_nodes(*item, arena, nodes);
                }
                count_statements_nodes(body, arena, nodes);
            }
            Statement::ChildScope(scope) => count_scope_nodes(scope, arena, nodes),
            Statement::Group(inner) => count_statements_nodes(inner, arena, nodes),
            Statement::Opaque(_) => {}
        }
    }
}

fn count_expression_nodes(expression: ExprId, arena: &[Expression], nodes: &mut usize) {
    *nodes += 1;
    match &arena[expression.0 as usize] {
        Expression::Member { receiver, .. } => count_expression_nodes(*receiver, arena, nodes),
        Expression::Call { callee, arguments, .. } => {
            count_expression_nodes(*callee, arena, nodes);
            for argument in arguments {
                count_expression_nodes(*argument, arena, nodes);
            }
        }
        Expression::Index { base, indices } => {
            count_expression_nodes(*base, arena, nodes);
            for index in indices {
                count_expression_nodes(*index, arena, nodes);
            }
        }
        Expression::Cast { operand, .. } => count_expression_nodes(*operand, arena, nodes),
        Expression::Unary { operand, .. } => count_expression_nodes(*operand, arena, nodes),
        Expression::Binary { left, right, .. } => {
            count_expression_nodes(*left, arena, nodes);
            count_expression_nodes(*right, arena, nodes);
        }
        Expression::Parenthesized(inner) => count_expression_nodes(*inner, arena, nodes),
        Expression::AnonymousMethod(scope) => count_scope_nodes(scope, arena, nodes),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::UnitParser;

    /// Parse a bare expression snippet and return the flat expression arena plus
    /// the root [`ExprId`]. Mirrors the cursor-construction the other parser tests
    /// use (a virtual source over a fresh arena), then drives the S1 expression
    /// entry point. Children are resolved via `arena[id.0]` — the flat-slab shape.
    fn parse_expression(source: &str) -> (Vec<Expression>, ExprId) {
        UnitParser::parse_expression_for_test(source)
    }

    /// The display spelling of a `QualifiedName`, via the global interner.
    fn name_of(name: &QualifiedName) -> String {
        crate::globals::resolve(name.name).to_string()
    }

    /// Resolve an `ExprId` against a parsed arena. `arena[id]` shorthand.
    fn at(arena: &[Expression], id: ExprId) -> &Expression {
        &arena[id.0 as usize]
    }

    /// Report the flattened node sizes. The win of the arena refactor is NOT a
    /// smaller `Expression` — it is ONE `Vec` allocation for the whole body
    /// instead of ~one `Box` per interior node. `ExprId` is a 4-byte index.
    #[test]
    fn arena_node_sizes_are_reported() {
        use std::mem::size_of;
        // A 4-byte index — the whole point (a `Box<Expression>` was 8 bytes/ptr
        // AND a separate heap allocation; an `ExprId` is neither).
        assert_eq!(size_of::<ExprId>(), 4, "ExprId is a u32 index");
        // Print for the record (run with `-- --nocapture`).
        println!(
            "size_of::<Expression>() = {}, size_of::<ExprId>() = {}, size_of::<Statement>() = {}",
            size_of::<Expression>(),
            size_of::<ExprId>(),
            size_of::<Statement>(),
        );
        // The enum size is set by its LARGEST variant. The largest variants carry
        // no `Box` (`Call`/`Inherited` — a `CodeLocation` payload), so flattening
        // the `Box`-bearing variants (`Member`/`Cast`/`Unary`/`Binary`/
        // `Parenthesized`) to a 4-byte `ExprId` cannot GROW the enum. Prove the
        // dominating payloads to make the "did not grow" claim concrete.
        println!(
            "size_of::<QualifiedName>() = {}, size_of::<CodeLocation>() = {}, \
             size_of::<Vec<ExprId>>() = {}, size_of::<Option<QualifiedName>>() = {}",
            size_of::<QualifiedName>(),
            size_of::<CodeLocation>(),
            size_of::<Vec<ExprId>>(),
            size_of::<Option<QualifiedName>>(),
        );
    }

    #[test]
    fn dotted_member_chain_nests_left() {
        // a.b.c  →  Member{ Member{ Identifier(a), b }, c }
        let (arena, root) = parse_expression("a.b.c");
        let Expression::Member { receiver, member } = at(&arena, root) else {
            panic!("expected outer Member, got {:?}", at(&arena, root));
        };
        assert_eq!(name_of(member), "c");
        let Expression::Member { receiver, member } = at(&arena, *receiver) else {
            panic!("expected inner Member");
        };
        assert_eq!(name_of(member), "b");
        let Expression::Identifier(base) = at(&arena, *receiver) else {
            panic!("expected Identifier base");
        };
        assert_eq!(name_of(base), "a");
    }

    #[test]
    fn call_with_two_arguments() {
        let (arena, root) = parse_expression("Foo(x, y)");
        let Expression::Call {
            callee, arguments, ..
        } = at(&arena, root)
        else {
            panic!("expected Call, got {:?}", at(&arena, root));
        };
        let Expression::Identifier(callee_name) = at(&arena, *callee) else {
            panic!("expected Identifier callee");
        };
        assert_eq!(name_of(callee_name), "Foo");
        assert_eq!(arguments.len(), 2);
    }

    #[test]
    fn mixed_postfix_chain_nests() {
        // Obj.Method(a)[0].Field
        //   → Member{ Index{ Call{ Member{ Ident(Obj), Method }, [a] }, [0] }, Field }
        let (arena, root) = parse_expression("Obj.Method(a)[0].Field");
        let Expression::Member { receiver, member } = at(&arena, root) else {
            panic!("expected outer Member, got {:?}", at(&arena, root));
        };
        assert_eq!(name_of(member), "Field");
        let Expression::Index { base, indices } = at(&arena, *receiver) else {
            panic!("expected Index");
        };
        assert_eq!(indices.len(), 1);
        let Expression::Call { callee, .. } = at(&arena, *base) else {
            panic!("expected Call");
        };
        let Expression::Member { receiver, member } = at(&arena, *callee) else {
            panic!("expected Member callee");
        };
        assert_eq!(name_of(member), "Method");
        let Expression::Identifier(base_name) = at(&arena, *receiver) else {
            panic!("expected Identifier Obj");
        };
        assert_eq!(name_of(base_name), "Obj");
    }

    #[test]
    fn pointer_deref_then_member() {
        // p^.Field  →  Member{ Unary{ Dereference, Ident(p) }, Field }
        let (arena, root) = parse_expression("p^.Field");
        let Expression::Member { receiver, member } = at(&arena, root) else {
            panic!("expected Member, got {:?}", at(&arena, root));
        };
        assert_eq!(name_of(member), "Field");
        let Expression::Unary { operator, operand } = at(&arena, *receiver) else {
            panic!("expected Unary deref");
        };
        assert_eq!(*operator, UnaryOperator::Dereference);
        let Expression::Identifier(base) = at(&arena, *operand) else {
            panic!("expected Identifier p");
        };
        assert_eq!(name_of(base), "p");
    }

    #[test]
    fn as_cast_produces_cast() {
        let (arena, root) = parse_expression("x as TFoo");
        let Expression::Cast { type_name, operand } = at(&arena, root) else {
            panic!("expected Cast, got {:?}", at(&arena, root));
        };
        assert_eq!(name_of(type_name), "TFoo");
        let Expression::Identifier(base) = at(&arena, *operand) else {
            panic!("expected Identifier x");
        };
        assert_eq!(name_of(base), "x");
    }

    #[test]
    fn is_test_produces_binary() {
        let (arena, root) = parse_expression("x is TBar");
        let Expression::Binary {
            operator,
            left,
            right,
        } = at(&arena, root)
        else {
            panic!("expected Binary, got {:?}", at(&arena, root));
        };
        assert_eq!(*operator, BinaryOperator::Is);
        let Expression::Identifier(left_name) = at(&arena, *left) else {
            panic!("expected Identifier x");
        };
        assert_eq!(name_of(left_name), "x");
        // The right side of `is` is a type name, modelled as an Identifier.
        let Expression::Identifier(right_name) = at(&arena, *right) else {
            panic!("expected Identifier TBar");
        };
        assert_eq!(name_of(right_name), "TBar");
    }

    #[test]
    fn bare_inherited() {
        let (arena, root) = parse_expression("inherited");
        let Expression::Inherited { method, .. } = at(&arena, root) else {
            panic!("expected Inherited, got {:?}", at(&arena, root));
        };
        assert!(method.is_none());
    }

    #[test]
    fn inherited_with_method_and_arguments() {
        // inherited Create(x)
        //   → Call{ callee: Inherited{ method: Some(Create) }, [x] }
        let (arena, root) = parse_expression("inherited Create(x)");
        let Expression::Call {
            callee, arguments, ..
        } = at(&arena, root)
        else {
            panic!("expected Call over Inherited, got {:?}", at(&arena, root));
        };
        assert_eq!(arguments.len(), 1);
        let Expression::Inherited { method, .. } = at(&arena, *callee) else {
            panic!("expected Inherited callee");
        };
        let method = method.expect("method name after inherited");
        assert_eq!(name_of(&method), "Create");
    }

    #[test]
    fn address_of_prefix() {
        let (arena, root) = parse_expression("@Proc");
        let Expression::Unary { operator, operand } = at(&arena, root) else {
            panic!("expected Unary, got {:?}", at(&arena, root));
        };
        assert_eq!(*operator, UnaryOperator::AddressOf);
        let Expression::Identifier(name) = at(&arena, *operand) else {
            panic!("expected Identifier Proc");
        };
        assert_eq!(name_of(name), "Proc");
    }

    #[test]
    fn precedence_multiply_binds_tighter_than_add() {
        // 1 + 2 * 3  →  Binary{ Add, 1, Binary{ Multiply, 2, 3 } }
        let (arena, root) = parse_expression("1 + 2 * 3");
        let Expression::Binary {
            operator,
            left,
            right,
        } = at(&arena, root)
        else {
            panic!("expected Binary, got {:?}", at(&arena, root));
        };
        assert_eq!(*operator, BinaryOperator::Add);
        assert!(matches!(at(&arena, *left), Expression::Literal(_)));
        let Expression::Binary { operator, .. } = at(&arena, *right) else {
            panic!("expected nested multiply");
        };
        assert_eq!(*operator, BinaryOperator::Multiply);
    }

    #[test]
    fn set_or_array_literal() {
        let (arena, root) = parse_expression("[1, 2, 3]");
        assert!(
            matches!(at(&arena, root), Expression::SetOrArrayLiteral(_)),
            "got {:?}",
            at(&arena, root)
        );
    }

    #[test]
    fn malformed_trailing_dot_degrades_without_panic() {
        // `a .`  — a member access whose member is missing. Must not panic nor
        // loop; degrades to a best-effort partial (the receiver survives) and the
        // cursor is left advanced past the offending region.
        let (arena, root) = parse_expression("a . ");
        // Either the bare Identifier(a) (dot dropped) or a Member with a synthetic
        // partial member is acceptable — the invariant under test is "no panic,
        // returns a partial". Assert it is at least an expression rooted at `a`.
        match at(&arena, root) {
            Expression::Identifier(name) => assert_eq!(name_of(name), "a"),
            Expression::Member { receiver, .. } => {
                assert!(matches!(at(&arena, *receiver), Expression::Identifier(_)));
            }
            other => panic!("unexpected degraded shape: {other:?}"),
        }
    }
}
