# Spec — Task 8: LSP document lifecycle + ProjectSession wiring (foundation)

Repo: C:\workspaces\vscode\delphi-devkit (Rust workspace), branch `lsp`.
The parser is vendored at `parser/` (crate `delphi-parser`, lib `delphi_parser`).
The LSP server is `server/` (crate `ddk-server`, tower-lsp 0.20, tokio). Core is
`ddk-core`. Commit per green step (conventional commits: `feat(lsp): …`), minimal
messages, `cargo build` + relevant tests green before each commit. This is the
FOUNDATION every language feature builds on — get the plumbing exactly right;
features (definition/references/completion/hover/rename/signature/semantic
tokens) are SEPARATE later tasks, NOT this one.

## Governing discipline
Carried from the parser run: never surface a WRONG answer. A position that maps
imprecisely, a stale buffer, or a diagnostic on the wrong range is a defect.
Preserve ALL parser invariants (SESSION.md/REVIEW.md in `parser/`): virtual
buffers never persist; panic-free; dual-track; process-global interner/arena =
one project per process (fine — the LSP is one process).

## Part A — parser addition: parse an in-memory buffer

`ProjectSession::parse_source_file` (driver.rs:213) reads from DISK. The LSP
holds unsaved editor buffers, so add:

- `ProjectSession::parse_buffer(&mut self, path: &Path, content: &str) ->
  Result<(ParseOutcome, Option<Arc<UnitMeta>>), SessionError>` — mirror
  `parse_source_file` but seed the arena via `arena.insert_virtual(path,
  content)` instead of `arena.load(path)`, then run the SAME pipeline (loader,
  index, reference index, parse diagnostics, dfm link, dirty tracking). Virtual
  buffers already never persist (#21/#25) — do not regress that.
- A URI/path → `unit_key` (Identifier) handle so the LSP can call the query
  methods after parsing: return the parsed `meta.name()` to the caller (it
  already returns `Option<Arc<UnitMeta>>` → `meta.name()`), and/or add a small
  `unit_key_for_path`/`intern_unit_name` helper if needed. The LSP keeps its own
  `Url → unit_key` map from parse results.
- Tests (in parser): `parse_buffer` parses unsaved content, produces the same
  interface surface as the on-disk parse, and the virtual unit is NOT persisted
  by `save_now`.
- Keep the change minimal and additive; do not alter `parse_source_file`.

## Part B — ddk-server wiring

Explore `ddk-core` first (state, projects, compiler config, encoding) and
`server/src/main.rs` (existing tower-lsp handlers/style). You must discover:
- how the active project/workspace + its `.dproj` + active config/platform are
  known (there is `PROJECTS_DATA`, `dproj` cache, `effective_config_platform`);
- how to obtain the data for a parser `CompilerProfile` (context.rs:121 —
  `compiler_version: f64`, `rtl_version: Option<f64>`, platform, etc.) and the
  standard-source dirs (`delphi_parser::ddk::standard_source_directories`, and
  the compiler install/version from ddk-core's compiler configuration).

Build the foundation:

1. **Capabilities.** In `initialize`, advertise real `ServerCapabilities`:
   `text_document_sync` = INCREMENTAL (preferred) or FULL to start, and a
   `diagnostic`/publish capability. Leave feature providers (definition,
   completion, references, hover, rename, signatureHelp, semanticTokens) OFF for
   now — they are later tasks; do NOT claim a capability you don't implement.

2. **Document store.** A `Url → { text: String, version, line_index }` map
   (behind the server's lock). `line_index` supports UTF-16 ↔ byte-offset
   mapping (see 4). Update it on didOpen/didChange (apply incremental changes
   correctly if you advertise INCREMENTAL — the classic bug is UTF-16 vs byte
   ranges; get it right and test it) and drop on didClose.

3. **Session management.** Hold `ProjectSession` (from `delphi_parser::driver`)
   behind an async-safe lock (`tokio::sync::Mutex`, or `std::sync::Mutex` inside
   `spawn_blocking` — `ProjectSession` is sync and does blocking IO/parse; do NOT
   block the async executor: run parses via `tokio::task::spawn_blocking` or a
   dedicated thread). One session per opened workspace/project (open via
   `ProjectSession::open(dproj, config, platform, &CompilerProfile, options)`).
   If no dproj is resolvable yet, degrade gracefully (parse buffers with a
   minimal/default context) — never panic. Wire `SessionOptions.watch` sensibly
   (the LSP already has ddk-core file watchers; avoid double-watching — prefer
   `watch:false` and drive invalidation from LSP didChange/didSave, OR document
   the choice).

4. **Position mapping (CRITICAL — the #1 LSP bug source).** LSP `Position` is
   (line: u32, character: u32) where `character` counts UTF-16 code units. The
   parser uses byte offsets into the file content. Implement a tested,
   correct-by-construction mapping BOTH ways for a given document text:
   `(line, utf16_char) → byte_offset` and `byte_offset → (line, utf16_char)`.
   Handle: CRLF vs LF, multibyte UTF-8, astral/surrogate-pair characters (emoji
   in comments/strings), position at end-of-line and end-of-file. This is shared
   infra used by every future feature — put it in a `server/src/positions.rs`
   (or ddk-core) with thorough unit tests (ASCII, CRLF, multibyte, surrogate
   pair, EOL/EOF).

5. **Lifecycle → parse → diagnostics (the one end-to-end feature to prove it).**
   On didOpen/didChange: update the store, parse the buffer through the session
   (`parse_buffer`, on a blocking task), then publish `textDocument/
   publishDiagnostics` built from `session.diagnostics(unit_key)` (parse + dfm
   diagnostics), mapping each parser `CodeLocation`/span → an LSP `Range` via the
   position mapper. Honest severities; a dfm diagnostic that only has a dfm
   offset (no pas span) maps into the dfm document's range or is attached to the
   unit with a best-effort range — do NOT fabricate a wrong pas range (mirror
   the parser's honesty). Clear diagnostics on didClose.

## Execution order (commit per green step)
1. Parser `parse_buffer` + tests (`cargo test -p delphi-parser`). Commit.
2. `positions.rs` UTF-16↔byte mapper + exhaustive unit tests. Commit.
3. Document store + didOpen/didChange/didClose + capabilities (no parse yet —
   just the store, verified by a store unit test). Commit.
4. Session management (open from dproj + CompilerProfile, behind lock,
   spawn_blocking parse) + graceful no-dproj fallback. Commit.
5. Parse-on-change → publishDiagnostics end-to-end (range-mapped, honest). Commit.
6. `cargo build` (workspace) + `cargo test` green; a short `server/README` or
   module docs on the wiring; note what's deferred to feature tasks. Commit.

## Definition of done (adversarial-review gate)
- `parse_buffer` parses unsaved content, same interface as on-disk, never
  persists the virtual unit (tested).
- Position mapping is correct for ASCII/CRLF/multibyte/surrogate/EOL/EOF (tested)
  — no off-by-one, both directions round-trip.
- Document store tracks open docs, applies incremental changes correctly, drops
  on close (tested).
- `ProjectSession` lives behind a lock; parses run OFF the async executor
  (spawn_blocking); no `.await` holds a lock across a blocking parse; no panic on
  missing dproj.
- didOpen/didChange → parse → publishDiagnostics works end-to-end with correct
  ranges; didClose clears; feature-provider capabilities remain OFF (honest).
- Parser invariants intact (virtual-never-persist, panic-free, dual-track);
  parser's 213 tests still green; workspace builds.
- Anything deferred is ledgered/noted, not silently skipped.

Report: file-by-file, commits, exact test counts, the position-mapping test
matrix, how you bridged ddk-core's project/compiler config into a
CompilerProfile, the async/lock model you chose, and anything you couldn't fully
verify (flag it). Output goes to an orchestrator.
