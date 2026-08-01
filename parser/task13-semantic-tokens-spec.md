# Spec — Task 13: semantic tokens (syntax highlighting)

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Build on task-8..12
(positions LineIndex, session+spawn_blocking, capability advertising). Commit per
green step (`feat(lsp): …`), `cargo build` + both suites green before each.
Governing rule (carried, natural fit here): LSP semantic tokens are ADDITIVE over
the editor's TextMate grammar, so EMIT A TOKEN ONLY WHEN THE CLASSIFICATION IS
CERTAIN. An unresolved/ambiguous identifier → OMIT it (the editor falls back to
its TextMate color — never a wrong semantic color). Never a wrong-confident token.

## Grounding
- Lexer (`parser/src/token.rs`): `Token` enum — trivia (`Whitespace`,`Newline`),
  comments (`BlockComment`,`BlockCommentParen`,`LineComment`), `IntLiteral`,
  `StringLiteral`, reserved words (keywords), `Identifier`, operators,
  directives (`{$…}`). `Token::is_trivia()`. Lexing a buffer yields tokens with
  spans (byte offsets).
- AST + interface index: declaration names carry kinds (`SymbolKind`), members
  carry `MemberKind` (Field/Method/Property/…), parameters exist in `RoutineType`.
  `UnitMeta.usages` = identifier occurrences (over-approximating, scope-unresolved).
- Reuse `symbol_at`/interface index to classify an identifier by what it declares
  or resolves to.

## Deliverable A — parser query: classified tokens
Add `ProjectSession::semantic_tokens(unit_key) -> Vec<SemanticToken>` (+ types in
query.rs): `SemanticToken { location: CodeLocation, token_type: SemanticKind,
modifiers: <bitflags or Vec> }`. `SemanticKind` enum covering the classes you
emit (e.g. Keyword, Comment, String, Number, Operator, Type, Class, Interface,
Enum, EnumMember, Function/Method, Property, Field, Parameter, Variable,
Constant, Namespace/Unit, Directive/Macro).

Classification, each emitted ONLY when certain:
1. **Lexical (precise, from the lexer):** comments → Comment; `StringLiteral` →
   String (incl. `#`-char codes → String or a suitable class); `IntLiteral` →
   Number; reserved words → Keyword; `{$…}` directives → Macro; operators →
   Operator (optional — omit if noisy). Trivia (whitespace/newline) → no token.
2. **Identifier roles (precise for declarations/members):** a declaration NAME
   span → its `SymbolKind` (Type→Type/Class/Interface/Enum by the type kind,
   Procedure/Function→Function, Const→Constant, Var→Variable); a member NAME span
   → its `MemberKind` (Field→Field/Property→Property/Method→Method); a parameter
   name → Parameter; a unit name in a `uses`/header → Namespace. These are
   structurally known — precise.
3. **Identifier USAGES (best-effort, OMIT when unsure):** for an identifier
   occurrence that is not a declaration, resolve it (own interface / imports, the
   same machinery as `symbol_at`/`definition`); if it resolves UNAMBIGUOUSLY to a
   known kind → emit that kind; if unresolved or ambiguous → OMIT (no token; the
   editor's TextMate handles it). Do NOT guess a class for an unknown identifier.
   Add a `declaration`/`definition`/`readonly` modifier where structurally known
   (a declaration site → `declaration` modifier).

Build the token list by lexing the buffer once (spans) and consulting the AST/
interface classification per identifier. Return tokens in SOURCE ORDER (or let
the server sort). Parser tests: a small unit → keywords/comments/strings/numbers
classified; a `type TFoo = class` name → Class with `declaration`; a method name
→ Method; a parameter → Parameter; an UNKNOWN identifier usage → no token
(omitted); a known cross-unit type usage → Type.

## Deliverable B — server: legend + encoding + capability
- Advertise `semantic_tokens_provider` (Full; Range optional) with a LEGEND —
  the ordered list of `SemanticTokenType`s and modifiers your `SemanticKind`
  maps to (standard LSP token types: namespace, type, class, enum, interface,
  struct, typeParameter, parameter, variable, property, enumMember, function,
  method, keyword, comment, string, number, operator, macro). Keep the legend and
  the `SemanticKind → (typeIndex, modifierBitset)` mapping in ONE place.
- `textDocument/semanticTokens/full`: get the unit's `semantic_tokens`, map each
  to LSP's DELTA encoding. CRITICAL correctness (the fiddly part):
  - **Single-line only:** an LSP semantic token CANNOT span lines. A multi-line
    token (a block comment `{ … }` / `(* … *)` across lines, a multi-line string)
    MUST be split into one token PER LINE it covers. Get this right — an
    unsplit multi-line token corrupts the whole delta stream after it.
  - **UTF-16:** `length` and `deltaStartChar` are in UTF-16 code units (use the
    LineIndex / position mapping — multibyte and astral chars count correctly).
  - **Sorted + relative:** sort tokens by (line, startChar); encode
    (deltaLine, deltaStartChar-relative-to-prev-on-same-line, length, typeIndex,
    modifierBitset); no overlaps (if two tokens overlap, keep the more specific
    one — but by construction lexer+decl spans shouldn't overlap; assert/dedupe).
  - Clamp/skip any token whose span can't map (never panic, never a bad delta).
- Run on spawn_blocking behind the session lock (task-8 discipline).
- Server tests (encoding is where bugs live): delta encoding of several tokens on
  multiple lines; a MULTI-LINE block comment split into per-line tokens; UTF-16
  length for a token after a multibyte/emoji char; ordering; empty doc → empty.

## Execution order (commit per green step)
1. Parser `semantic_tokens` query + SemanticKind/SemanticToken + classification
   (lexical + declaration/member + usage-or-omit) + parser tests. Commit.
2. Server legend + SemanticKind→LSP mapping + the delta encoder (with multi-line
   split + UTF-16) as a tested helper (`server/src/semantic.rs`). Commit.
3. `textDocument/semanticTokens/full` handler + capability + server tests. Commit.
4. `cargo build` + both suites green; README updated (semanticTokens capability +
   legend + the omit-when-unsure policy). Commit.

## Definition of done (adversarial-review gate)
- Classification: keywords/comments/strings/numbers precise; declaration/member/
  parameter names get their correct kind + `declaration` modifier; an UNKNOWN
  identifier usage is OMITTED (never a wrong color); known usages classified.
- Encoding: multi-line tokens split per line; UTF-16 lengths/deltas correct
  (multibyte/astral); sorted, relative, no overlaps, no panic; empty → empty.
- Queries off the async executor behind the session lock; capability + legend
  match what's emitted. Parser invariants intact; both suites green; builds.

Report: file-by-file, commits, exact test counts, the SemanticKind→LSP legend
mapping, classification proof points (incl. the omit-when-unsure case), the
encoding test matrix (esp. multi-line split + UTF-16), and anything unverified
(flag it). Output goes to an orchestrator.
