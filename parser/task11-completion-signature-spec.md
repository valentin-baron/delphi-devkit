# Spec — Task 11: completion (Ctrl+Space) + signature help (Ctrl+Shift+Space)

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Build on task-8/9/10
infra (positions, DocumentStore, SessionManager+spawn_blocking, locations, the
publish version guard). Commit per green step (`feat(lsp): …`), `cargo build` +
both suites green before each. Governing rule (carried): never a WRONG answer —
completion after `.` must not suggest a wrong member set; signature help must
never show a fabricated signature. Insufficient info → empty/None, never a guess.

## Query API you consume (parser)
- `session.completions(unit_key, position: u32) -> Vec<Completion>` (already
  built): after `.` → members of the receiver type; else top-level (builtins +
  own + imports). `Completion { display, key, kind: CompletionKind
  (Symbol(SymbolKind)|Member(MemberKind)|Builtin), type_key, directives,
  visibility }`.
- AST for signatures: `UnitMeta.ast` (via the cache) holds routine signatures.
  Top-level routines are `interface_declarations` of kind Procedure/Function;
  methods are `Member::Method(MethodDeclaration{ name, routine: RoutineType })`.
  `RoutineType { kind, parameters: Vec<Parameter>, return_type }`;
  `Parameter { modifier: ParameterModifier, names: Vec<QualifiedName>,
  parameter_type: Option<TypeExpression>, default_value }`. Expressions are
  spans; resolve type/name display via `globals::resolve` on the folded/display
  identifiers. (The derived interface index does NOT carry params — you add a
  parser query that reads the AST.)

## Deliverable A — textDocument/completion (SHIP; ready)
- Advertise `completion_provider` with `trigger_characters: ["."]`, `resolve_
  provider: false`.
- Handler: (Url, Position) → (unit_key, offset) → `session.completions(unit_key,
  offset)` → map each `Completion` to a `CompletionItem`:
  - `label` = display; `kind` = map CompletionKind → `CompletionItemKind`
    (Symbol/Member kinds → Class/Interface/Field/Method/Property/Enum/Constant/
    Variable/Function/etc.; Builtin → Keyword or Struct); `detail` = a short type/
    kind string (e.g. the resolved `type_key`, or the member kind).
  - De-dup already handled by the parser; do not re-suggest a wrong member set —
    a member completion (after `.`) must contain ONLY the receiver type's members
    (the parser guarantees this), never top-level leakage.
- Run on spawn_blocking behind the session lock (task-8 discipline).
- Tests: member completion after `.` lists the type's members with correct kinds;
  top-level completion includes an imported symbol + a builtin; empty context →
  the top-level set (never a wrong member list); positions mid-identifier resolve
  the enclosing context sensibly.

## Deliverable B — parser query: routine signature
Add to `parser/src/driver.rs` (+ a `SignatureInfo` type in query.rs):
- `signature_help(unit_key, callee_key, owner: Option<Identifier>) ->
  Option<SignatureInfo>` where `SignatureInfo { label: String /*full signature*/,
  parameters: Vec<ParameterInfo{ label: String, /* "const Name: Type" */ }>,
  return_type: Option<String> }`.
- Resolution: a member routine (owner = the type) → find the owner type (own then
  imports, SAME cross-unit loader as `definition`), then its `Member::Method`
  whose folded name == callee_key; a top-level routine → the interface
  declaration of kind Procedure/Function with that key (own then imports).
  Read `RoutineType.parameters` and `return_type` from the AST; build the labels
  via `globals::resolve`. Unresolvable / not-a-routine → `None` (never a
  fabricated signature). Overloads: if multiple routines share the key, return
  all as separate signatures (SignatureHelp supports a signature list) — or, if
  the index can't distinguish overloads, return what you can prove and note it.
- Parser tests: signature of an own method (params + return), of an imported
  routine (cross-unit), a procedure (no return), untyped param (`var Buffer`),
  a defaulted param; unknown callee → None.

## Deliverable C — textDocument/signatureHelp (server)
- Advertise `signature_help_provider` with `trigger_characters: ["(", ","]`,
  `retrigger_characters: [","]`.
- **Call-context detection (the fiddly, get-it-right part).** From the cursor,
  scan the document text BACKWARDS to find the enclosing unclosed `(` at the
  current call depth, skipping balanced `()[]` and string/char/comment content
  (`'…'`, `{…}`, `(*…*)`, `//…`). The identifier (possibly dotted `Obj.Method`)
  immediately before that `(` is the callee. `active_parameter` = count of
  top-level commas between that `(` and the cursor (commas inside nested
  parens/brackets/strings don't count). If no enclosing call → None.
  Put this in a tested helper (`server/src/call_context.rs`): it operates on text
  + byte offset, returns `Option<{ callee_offset, active_parameter }>`. Test
  nested calls, commas in string literals, comments, no-call, multi-line calls.
- Handler: detect context → resolve the callee via `symbol_at(unit_key,
  callee_offset)` → `signature_help(unit_key, target.key, target.owner_type)` →
  build `SignatureHelp { signatures: [SignatureInformation{ label, parameters }],
  active_signature: 0, active_parameter }`. No callee / unresolved → None.
- Never fabricate: if the callee doesn't resolve to a routine, return None (the
  editor shows nothing), never a made-up `(...)`.

## Execution order (commit per green step)
1. completion handler + capability + CompletionKind→LSP mapping + tests. Commit.
2. parser `signature_help` query + SignatureInfo + parser tests. Commit.
3. `call_context.rs` detection helper + exhaustive tests. Commit.
4. signatureHelp handler + capability + tests. Commit.
5. `cargo build` + both suites green; README updated (completion + signatureHelp
   capabilities, trigger chars); note any overload limitation. Commit.

## Definition of done (adversarial-review gate)
- completion: member-after-`.` lists only the receiver's members (no top-level
  leak); top-level includes imports+builtins; correct CompletionItemKinds;
  empty context → top-level, never a wrong member list.
- signature help: correct params/return for own + cross-unit routines/methods;
  active_parameter correct across nested calls / commas-in-strings / comments;
  unresolved callee → None (never fabricated); overloads handled or limitation
  noted.
- call-context detection is exhaustively tested (nesting, strings, comments,
  multi-line, no-call).
- Queries off the async executor behind the session lock; no deadlock; caps
  match implemented features. Parser invariants intact; parser + server tests
  green; workspace builds.

Report: file-by-file, commits, exact test counts, completion + signature proof
points, the call-context test matrix, overload handling, and anything unverified
(flag it). Output goes to an orchestrator.
