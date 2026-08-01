# ddk-server — Delphi Language Server (LSP)

tower-lsp server wiring the [`delphi-parser`](../parser) analysis engine to an
editor. This document describes the **document-lifecycle foundation** (Task 8):
the plumbing every language feature builds on. Features themselves
(definition / references / completion / hover / rename / signatureHelp /
semanticTokens) are **separate later tasks** and are intentionally NOT advertised
yet.

## Modules

| Module | Role |
|---|---|
| `positions.rs` | `LineIndex`: exact **UTF-16 code-unit ↔ byte-offset** mapping for one document, both directions. Handles LF/CRLF, multibyte UTF-8, astral/surrogate-pair characters, EOL/EOF. Clamps out-of-range positions (never panics). The #1 LSP defect surface — tested exhaustively. |
| `documents.rs` | `DocumentStore`: `Url → { version, LineIndex }` for open editor buffers (the authoritative unsaved text). Applies incremental **and** full `didChange` edits through the position mapper; ignores stale (older-version) changes. |
| `session.rs` | Bridges ddk-core project/compiler config → parser `CompilerProfile`; owns the parser `ProjectSession` behind an async lock; opens per active project with a graceful **no-dproj fallback** context. |
| `diagnostics.rs` | Maps the parser's `UnifiedDiagnostic`s to LSP `Diagnostic`s. Only a location **in the analyzed buffer** gets an exact byte-mapped range; a DFM-only offset (or a location in another file) is anchored at the top of the document — **never a fabricated pas range**. |
| `main.rs` | tower-lsp handlers: `initialize` capabilities, `didOpen`/`didChange`/`didClose` → `analyze` → `publishDiagnostics`. |

## Capabilities advertised

- `textDocumentSync = INCREMENTAL` — the editor streams open/change/close.
- Pushed diagnostics via `textDocument/publishDiagnostics` (needs no capability
  flag).

Everything else stays **off**. No definition/completion/references/hover/
rename/signatureHelp/semanticTokens provider is claimed — a capability is only
advertised once it is actually backed.

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

- Language-feature providers (definition/references/completion/hover/rename/
  signatureHelp/semanticTokens) — the parser query API
  (`ProjectSession::{symbol_at, definition, references, completions}`) already
  exists; only the LSP request handlers + capabilities remain.
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
