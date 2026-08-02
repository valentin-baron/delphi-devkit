# Implementation-section semantic AST — design spec

Goal: replace the flat usage scan + `impl_scopes` side-table with a REAL
implementation-section AST covering ALL scopes (free routines, methods of
class/record/interface/helpers, nested routines, anonymous methods/closures,
`with`-blocks, `initialization`/`finalization`), carrying typed local symbols and
expression trees — enough to power completion, go-to/hover with type info on
locals, and `inherited` navigation. NOT a control-flow compiler.

## Three hard constraints (shape every decision)

1. **Low memory.** Body ASTs are needed ONLY for the unit being edited/queried;
   cross-unit resolution uses INTERFACES only (never another unit's body). So
   resident body memory ≈ the few open units. The rest is bounded by the existing
   machinery: bodies live on `UnitMeta`, serialize into the compressed `.unit`
   file, are weighed by `estimated_bytes`, evicted under the moka RAM cap, and
   reload from disk WITHOUT reparse. The weigher MUST count body-AST bytes so
   eviction stays honest.
2. **Fast parse.** Single-pass recursive descent, no backtracking, error-tolerant
   (on any unexpected token resync to the next `;` / `end` / decl boundary and
   keep going). Bodies parse once per unit then cache; only the OPEN unit reparses
   on edit (one unit).
3. **Few reparses.** Per-unit hash-cached exactly like the interface. Editing unit
   A reparses only A. Cross-unit edits never cascade a body reparse — all
   cross-unit type/inheritance resolution is LAZY at query time (uses the import's
   interface index, which is separately cached). No whole-project reparse ever.

## Never-wrong (unconditional)

A body region the parser cannot model confidently degrades LOCALLY (that
statement/expression drops to an opaque span; its enclosing scope survives) and
sets no wrong binding. A scope whose extent cannot be tracked reliably marks its
subtree unreliable and queries fall back to "no result" rather than a wrong one.
Unknown over a guess, everywhere.

## AST shape (pragmatic — scopes + expressions, shallow statements)

New module `ast_impl.rs` (kept separate from the interface `ast.rs`). All types
`Serialize + Deserialize + Clone + Debug`; identifiers dual-track interned; every
literal/opaque region a `CodeLocation` span (the "AST carries no strings /
expressions are spans" invariant continues to hold for leaves).

```
ImplementationBody {
    routines: Vec<RoutineImplementation>,   // top-level impl routines, source order
    initialization: Option<StatementList>,  // shallow
    finalization: Option<StatementList>,
    reliable: bool,                          // whole-section clean-parse flag (gate)
}

RoutineImplementation {
    name: QualifiedName,                     // the header name occurrence
    owner_type_key: Option<Identifier>,      // Some(TFoo) for `procedure TFoo.Bar`
    kind: RoutineKind,                       // procedure/function/ctor/dtor/operator
    scope: Scope,
}

Scope {                                      // one lexical scope
    kind: ScopeKind,                         // Routine | Nested | Anonymous | Method | With
    span: Span,                              // whole scope extent (enclosing test by offset)
    self_type_key: Option<Identifier>,       // Some(TFoo) inside a method body → `Self`/bare members
    declarations: Vec<LocalSymbol>,          // params + var/const/type/label + inline vars
    statements: StatementList,
}

LocalSymbol {
    name: QualifiedName,                      // decl key + its own span
    kind: LocalKind,                          // Var|Const|Type|Param|Label|InlineVar
    type_key: Option<Identifier>,            // simple reference type (for typing/completion); else None
    // full TypeExpression optional-boxed only if cheaply available; else None (never block on it)
}

StatementList = Vec<Statement>

Statement {                                  // SHALLOW — we model scope + expressions, not control flow
    Expression(Expression),                  // a bare expression / call statement
    Assignment { target: Expression, value: Expression },
    LocalVar(LocalSymbol, Option<Expression>),// inline `var X := expr` (adds to enclosing scope)
    With { items: Vec<Expression>, body: StatementList },  // opens member scope over items' types
    ChildScope(Box<Scope>),                  // nested routine / anonymous method
    Group(StatementList),                    // begin/end, if/for/while/case/try bodies flattened:
                                             //   condition/selector expressions are pulled into the
                                             //   Group as Expression statements; branch bodies recurse.
                                             //   We do NOT tag which control-flow it was.
    Opaque(CodeLocation),                    // a region we chose not to model; still scanned for
                                             //   identifier occurrences so references stay complete
}

Expression {                                 // receiver-shaped, enough to type a chain for completion
    Identifier(QualifiedName),
    Member { receiver: Box<Expression>, member: QualifiedName },   // a.b  (closes ledger #41)
    Call   { callee: Box<Expression>, arguments: Vec<Expression>, arguments_span: CodeLocation },
    Index  { base: Box<Expression>, indices: Vec<Expression> },
    Cast   { type_name: QualifiedName, operand: Box<Expression> }, // `x as T` and `T(x)`
    Inherited { method: Option<QualifiedName> },                   // `inherited` / `inherited Foo`
    Unary  { operator: UnaryOperator, operand: Box<Expression> },  // @ ^ not - +
    Binary { operator: BinaryOperator, left: Box<Expression>, right: Box<Expression> },
    AnonymousMethod(Box<Scope>),             // closure body is a child scope
    SetOrArrayLiteral(CodeLocation),         // `[a, b]` — inner elements scanned for occurrences
    Literal(CodeLocation),                   // number/string/nil/true/false — opaque leaf
    Parenthesized(Box<Expression>),
}
```

Notes:
- `Group` + `Opaque` keep statements shallow while still descending for every
  identifier occurrence, so `references`/`symbol_at` see the full occurrence set
  (no regression vs today's flat scan). The flat `usages` vec is DERIVED from a
  walk of the body AST (kept for the reference index) — one source of truth.
- Occurrences now carry binding context (their scope + whether member-qualified),
  which closes #41 (owner-qualified member usages) and is the substrate for exact
  references/rename (#42) later.

## Integration

- `UnitMeta` gains `body: ImplementationBody` (replacing `impl_scopes` +
  `impl_scopes_reliable`). `usages` is derived from the body walk. Format bump.
- `collect_implementation_usages` is replaced by `parse_implementation_body`,
  reusing the interface parser's type/parameter parsing where useful.
- Weigher: add body-AST cost (per expression/statement/scope node) to
  `estimated_bytes` so the RAM cap holds.
- Serialization: rides the existing compressed `[magic|version]` segment path.
- Queries: `symbol_at`/`definition_at`/`hover`/`member_*` consult the scope tree +
  occurrences; the it.15 gate becomes `body.reliable`. Type resolution + the
  cross-unit inheritance walk stay LAZY at query time (constraint #3).

## Build stages (each: implement → adversarial review → real-code probe → commit)

- **S1** Expression AST + expression parser (standalone, unit-tested).
- **S2** Statement/scope parser → full `ImplementationBody`; all scope kinds;
  typed locals; init/final; derive `usages`; replace the flat scan. Real-code
  probe: parse all 820 units, 0 failures, report scopes/exprs captured + parse
  time + weighed bytes.
- **S3** `UnitMeta` field + serialization (format bump) + weigher + cache; RSS/
  memory check under the cap; round-trip test.
- **S4** Rewire queries onto the scope tree + occurrences; keep never-wrong gate;
  live smoke (locals/params/nested/anon/method scopes jump + hover).
- (Follow-on, separate: completion via receiver typing + inheritance walk; then
  `inherited` nav + interface↔impl sync. Builds on S1–S4.)
```
