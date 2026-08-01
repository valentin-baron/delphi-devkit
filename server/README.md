# ddk-server — Delphi Language Server (LSP)

tower-lsp server wiring the [`delphi-parser`](../parser) analysis engine to an
editor. This document describes the **document-lifecycle foundation** (Task 8)
plus the language features layered on it: **definition + hover** (Task 9) and
**find-references** (Task 10). The remaining features (completion / rename /
signatureHelp / semanticTokens) are **separate later tasks**; a capability is
advertised only once it is actually backed. **rename** in particular is
*deliberately deferred* (not just unimplemented) — see "Why rename is deferred".

## Modules

| Module | Role |
|---|---|
| `positions.rs` | `LineIndex`: exact **UTF-16 code-unit ↔ byte-offset** mapping for one document, both directions. Handles LF/CRLF, multibyte UTF-8, astral/surrogate-pair characters, EOL/EOF. Clamps out-of-range positions (never panics). The #1 LSP defect surface — tested exhaustively. |
| `documents.rs` | `DocumentStore`: `Url → { version, LineIndex }` for open editor buffers (the authoritative unsaved text). Applies incremental **and** full `didChange` edits through the position mapper; ignores stale (older-version) changes. |
| `session.rs` | Bridges ddk-core project/compiler config → parser `CompilerProfile`; owns the parser `ProjectSession` behind an async lock; opens per active project with a graceful **no-dproj fallback** context. |
| `diagnostics.rs` | Maps the parser's `UnifiedDiagnostic`s to LSP `Diagnostic`s. Only a location **in the analyzed buffer** gets an exact byte-mapped range; a DFM-only offset (or a location in another file) is anchored at the top of the document — **never a fabricated pas range**. |
| `locations.rs` | `code_location_to_lsp`: maps a parser `CodeLocation` (a byte span into *some* parsed file) to an LSP `Location`, computing the `Range` from the **TARGET file's own text** — the open-document `LineIndex` when that file is a buffer, else a `LineIndex` built from `arena.content(file)`. Returns `None` (never a fabricated `Location`) for a virtual/non-file target or unreadable content. Shared navigation primitive for definition/hover/**references**. Also `resolve_references`: the folded key under the cursor → every recorded occurrence across cached units, each mapped through its OWN file's text, honoring `include_declaration` — an **over-approximating candidate set** (see below). |
| `hover.rs` | Formats the parser's `HoverInfo` into a fenced `delphi` hover signature. Renders only facts the parser captured — a field/property's known type, a method's directives, the owning type — and shows **kind only** when the declared type is anonymous (`type_key` None): **never a fabricated type/return type**. |
| `main.rs` | tower-lsp handlers: `initialize` capabilities, `didOpen`/`didChange`/`didClose` → `analyze` → `publishDiagnostics`, plus `textDocument/definition`, `textDocument/hover`, and `textDocument/references`. |

## Capabilities advertised

- `textDocumentSync = INCREMENTAL` — the editor streams open/change/close.
- Pushed diagnostics via `textDocument/publishDiagnostics` (needs no capability
  flag).
- `definitionProvider` — `textDocument/definition`. The identifier under the
  cursor → its declaration site(s), own-unit or **cross-file**, each mapped to a
  `Location` from the **target file's own text**. An unresolved target → **no
  jump** (empty), never a wrong one.
- `hoverProvider` — `textDocument/hover`. The symbol under the cursor → its
  declared facts (kind, declared type, method directives, visibility, owning
  type), resolved **cross-unit** through the same machinery as definition, and
  rendered as a fenced `delphi` signature. No honest facts → **None**, never a
  fabricated type.
- `referencesProvider` — `textDocument/references`. The identifier under the
  cursor → every recorded occurrence of its folded key across **cached** units,
  each mapped to a `Location` from its **own file's text**, honoring
  `context.includeDeclaration`. No symbol under the cursor → **None**.
  **This is a READ-ONLY, OVER-APPROXIMATING candidate set** the user visually
  reviews: the parser's usage index is scope-unresolved, so it never misses a
  real occurrence in a cached unit but *may* include an unrelated same-named
  identifier (a local `Result`, a same-named symbol in another unit). This is
  documented honestly (matching how the parser documents the index) — it does
  **not** claim precision it lacks. Only units that have been parsed/cached
  contribute occurrences.

The remaining feature providers — completion / **rename** / signatureHelp /
semanticTokens — stay **off**; a capability is only advertised once it is
actually backed. See below for why **rename is deliberately deferred**.

### Why rename is deferred (not advertised)

A rename must be **both complete** (rewrite *every* real reference) **and
correct** (rewrite *nothing else*) — and because it is **destructive**, the
never-a-wrong-answer rule binds hardest here. The only occurrence set available
is the *same* over-approximating candidate set `references` serves (the
scope-unresolved usage index). That leaves no correct option:

- Renaming the **whole** candidate set would rewrite an unrelated same-named
  identifier (a local `Result`, a different unit's `Name`) → a **destructive
  wrong edit**.
- Renaming only the **provably-bound subset** (the declaration + resolved
  interface references) would leave the implementation-section uses — recorded
  only as flat, owner-less, scope-unresolved usages — un-renamed → **dangling /
  broken code**, an incomplete edit.

No provable safety gate bridges this without scope resolution (proving an
occurrence binds to *this* symbol, not a shadowing local, needs the very scope
resolution that is missing; the usage index does not even record whether an
occurrence is a local binding). So `rename_provider` is **not advertised** and
no `rename`/`prepareRename` handler ships — the editor offers no rename rather
than a sometimes-wrong one. This is the honest, safe outcome, ledgered as parser
`SESSION.md` **#42** with the prerequisite plan (a scope-resolved symbol table,
which also sharpens `references` to an exact set and closes #40/#41). `references`
is acceptable to ship over the same set precisely because it is non-destructive
and the user reviews it.

## Async / lock model (why it can't deadlock or block the executor)

`ProjectSession` is **synchronous** and does blocking file IO + parsing, so it
must never run on a tokio worker directly.

- The session lives in `Arc<tokio::sync::Mutex<Option<ProjectSession>>>`
  (`Option` = "no project resolvable yet" → graceful degradation, never a panic).
- Every parse runs inside `tokio::task::spawn_blocking`; inside that blocking
  thread the lock is taken with `blocking_lock()`. **The lock is a critical
  section around a synchronous parse and is never held across an `.await`.** The
  async caller awaits the `JoinHandle`, not the lock — so no task holds the mutex
  while suspended. An async-mutex-across-await deadlock is structurally
  impossible.
- The document store lock is likewise held only for the short section that
  copies text out; the parse then runs on the copied text.
- One session per process (the parser's interner/arena are process globals — one
  project per process, exactly the LSP model).

## ddk-core config → CompilerProfile bridge

`session::resolve_active_project_inputs` reads `PROJECTS_DATA` (active project +
its workspace's `compiler_id`) and `COMPILER_CONFIGURATIONS`
(`CompilerConfiguration { compiler_version, condition, installation_path }`),
then `compiler_profile` builds:

- `compiler_version: f64` from the config's `compiler_version`;
- `rtl_version: None` (⇒ equals `compiler_version` — correct for every modern
  Delphi);
- `defines`: the `VERxxx` condition + compiler-family symbols
  (`CONDITIONALEXPRESSIONS`, `UNICODE`, `ASSEMBLER`) + target-platform symbols
  (`MSWINDOWS` + `WIN32`/`CPUX86`/… or `WIN64`/`CPUX64`/…).

`ProjectContext::from_dproj` adds the dproj's own `DCC_Define`s on top. The
compiler installation's `source` tree provides the standard-unit search paths
(`delphi_parser::ddk::standard_source_directories`). No dproj / no compiler →
`fallback_inputs` (Delphi 12 / Win32 defaults) so buffers still parse.

## File watching

The parser's own OS watcher is **off** (`SessionOptions.watch = false`) to avoid
double-watching: ddk-core already runs file watchers, and buffer invalidation is
driven from the editor lifecycle (`didChange`). Re-parse on save/change comes
from the LSP notifications, not a second OS watcher.

## Preserved parser invariants

- **Virtual buffers never persist.** `parse_buffer` seeds the arena with
  `insert_virtual`; the unsaved unit's `FileId` path does not canonicalize, so it
  is dropped as `unreadable` on any snapshot load — unsaved state never
  masquerades as on-disk state (parser ledger #21/#25). Proven by
  `delphi_parser::driver::tests::parse_buffer_virtual_unit_is_not_persisted`.
- **Panic-free / never-a-wrong-answer.** Position mapping clamps rather than
  panicking; a diagnostic without an honest in-buffer range is anchored at the
  document top, never given a fabricated pas span.

## Deferred to later feature tasks

- **definition, hover, and references are now wired** (Tasks 9–10). The
  remaining language-feature providers (completion / signatureHelp /
  semanticTokens) are still deferred — the parser query API
  (`ProjectSession::{symbol_at, definition, hover_info, references, completions}`)
  already exists; only the LSP request handlers + capabilities remain.
- **rename is deferred, not merely unimplemented** (Task 10 Deliverable B): a
  correct+complete rename requires scope-resolved bindings the parser does not
  yet have (over-approximation over-renames; declaration-only under-renames).
  Ledgered as parser SESSION.md **#42**; `rename_provider` is intentionally not
  advertised. See "Why rename is deferred" above.
- **Interface ↔ implementation method jump** (Task 9 Deliverable D) is deferred:
  the parser does not yet structurally capture implementation-section method
  headers (`procedure TFoo.Bar; begin … end;`), so the jump has no data and was
  NOT faked. Ledgered as parser SESSION.md **#40** with a capture-and-map plan;
  `definition`/`hover` resolve the interface declaration today.
- **Per-document project resolution.** The foundation uses the *active* project
  for the session; matching an arbitrary opened file to the project that owns it
  (by search-path membership) is a refinement.
- **DFM diagnostics served into the dfm document.** A DFM-only finding is
  currently surfaced on the pas unit (top-of-document anchor with the offset
  noted). Mapping it into the `.dfm` document's own range (and go-to across the
  pas↔dfm boundary) is deferred (parser ledger #34).
- **Sharper severities.** All parser findings currently map to WARNING; a
  per-finding severity table is a later refinement.
- **`didSave`-driven persistence / autosave cadence.** `tick` is not yet driven
  from the LSP; snapshot autosave of on-disk units is a follow-up.
