# Spec — Task 9: go-to-definition + hover (+ interface/impl jump stretch)

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Parser at `parser/`
(delphi_parser), server at `server/` (ddk-server). Build on the task-8
foundation (positions.rs LineIndex, DocumentStore, SessionManager +
spawn_blocking, publish-version guard). Commit per green step (`feat(lsp): …`),
`cargo build` + `cargo test -p ddk-server` + `cargo test -p delphi-parser` green
before each. Governing rule (carried): a query must NEVER return a WRONG answer —
an unresolved target yields NO result, never a guessed location/type.

## Query API you consume (already built, parser/src/driver.rs + query.rs)
- `symbol_at(unit_key, position: u32) -> Option<QueryTarget>` — QueryTarget{key,
  display, kind (Declaration|Member|Usage), location: CodeLocation, owner_type}.
- `definition(unit_key, symbol_key, member_owner: Option<Identifier>) ->
  Vec<CodeLocation>` — decl site(s), cross-unit, cycle-safe, empty if unresolved.
- `CodeLocation { file: FileId, span: Span{start,end} }` (byte offsets).
- Arena: `session.arena().path(file) -> &Path`, `arena().content(file) ->
  Result<&str>` (decoded text of any parsed file — the source of truth for
  mapping a target span to a Range even when the file isn't open in the editor).
- `session.context()` for interning the unit key of the active document.

## Deliverable A — CodeLocation → LSP Location mapping (shared infra)
Every navigation feature needs this. Add (server side, e.g. positions.rs or a
new `locations.rs`): `code_location_to_lsp(session, location) -> Option<Location>`
that (1) resolves `arena().path(file)` → `Url::from_file_path`, (2) gets
`arena().content(file)` for the TARGET file, builds a `LineIndex`, maps the byte
span → `Range`. Returns None (never a wrong Range) if the path can't be a URL or
content is unreadable (e.g. a virtual/unsaved target). Prefer the DocumentStore's
LineIndex when the target file IS open (avoid re-reading), else build from arena
content. Tests: same-file target, cross-file target, virtual-target → None.

## Deliverable B — textDocument/definition
- Advertise `definition_provider`.
- Handler: map request (Url, Position) → (unit_key, byte offset) via the open
  document's LineIndex + the Url→unit_key map; `symbol_at` → if a QueryTarget,
  call `definition(unit_key, target.key, target.owner_type)`; map each
  CodeLocation via Deliverable A → `GotoDefinitionResponse::Array`. Empty result
  (no symbol / unresolved) → `None`, never a wrong jump.
- Run the query on spawn_blocking behind the session lock (task-8 pattern);
  no lock across `.await`.
- Tests: definition on an own-unit type jumps to its decl; on an imported symbol
  jumps cross-file (dependency recorded); on a member `Owner.Member`; on
  whitespace/unknown → empty.

## Deliverable C — textDocument/hover
Hover needs the symbol's TYPE/KIND/signature, which `symbol_at` alone doesn't
format. Add a parser query (parser/src/driver.rs) — keep the never-wrong rule:
- `hover_info(unit_key, position) -> Option<HoverInfo>` where `HoverInfo` (in
  query.rs) carries the display name, a SymbolKind/MemberKind, the declared
  type key (if any), method directives, visibility, and the owning type (for a
  member) — resolved through the SAME cross-unit machinery as `definition` so a
  hover over an imported symbol shows the imported declaration's facts. Unknown/
  unresolved → None.
- Server `hover_provider`; handler formats HoverInfo into markdown (e.g. a
  ```delphi fenced signature line: `type TFoo = class(...)`, `FBar: Integer`,
  `procedure Baz(...): T; virtual;`), returns `Hover{contents, range}` with the
  range = the occurrence span (Deliverable A within the open doc). No facts →
  None.
- Resolve strings via the interner (globals::resolve) for display. Do NOT invent
  a signature you can't derive from the AST/interface index — if the type is
  anonymous/complex (type_key None), show the kind only, not a fabricated type.
- Tests: hover over a field shows its type; over a method shows directives; over
  an imported type resolves cross-unit; over unknown → None.

## Deliverable D — interface ↔ implementation jump (STRETCH; implement-or-ledger)
Delphi methods declare in the interface (`procedure TFoo.Bar;` in the class) and
implement in the implementation section (`procedure TFoo.Bar; begin … end;`).
- FIRST verify whether the parser captures implementation-section method
  headers as locatable definitions (check parse_state usages / pipeline — the
  impl section is currently only token-scanned for the usage index). If a
  method-implementation location is available or cheaply derivable, wire a
  jump (a custom request or fold into `definition` returning BOTH the interface
  decl and the impl site). 
- If the impl-section method headers are NOT structurally captured (likely),
  DO NOT fake it. Ledger a numbered SESSION.md entry (parser/SESSION.md) with a
  plan (capture `procedure QualifiedName;` implementation headers → map to the
  interface method by qualified key) and leave interface/impl jump for a
  follow-up task. Be explicit in the report.

## Execution order (commit per green step)
1. Deliverable A (CodeLocation→Location) + tests. Commit.
2. Deliverable B (definition) + capability + tests. Commit.
3. Deliverable C (hover_info parser query + tests) then the hover handler +
   capability + tests. Commit.
4. Deliverable D: implement if data exists, else ledger. Commit (docs/ledger).
5. `cargo build` + both test suites green; server README updated with the new
   capabilities; note deferrals. Commit.

## Definition of done (adversarial-review gate)
- definition: own + cross-unit + member targets jump correctly; unresolved →
  empty (never a wrong location); cross-file Range mapping correct (uses the
  target file's own LineIndex, not the source file's).
- hover: shows correct type/kind/directives incl. cross-unit; anonymous type →
  kind-only, no fabricated type; unknown → None.
- All queries run off the async executor behind the session lock; no deadlock;
  no lock across await.
- Capabilities advertised match what's implemented (definition, hover); D either
  works or is honestly ledgered.
- Parser invariants intact; parser tests green; workspace builds.

Report: file-by-file, commits, exact test counts, proof points per deliverable
(esp. cross-file definition Range correctness and cross-unit hover), the D
decision (implemented or ledgered + why), and anything unverified (flag it).
