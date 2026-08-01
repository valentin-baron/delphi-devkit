# Spec — Task 15 (CRITICAL): fix the LSP OOM

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. The LSP crashes with OOM
during normal editing. Commit per green step, both suites green before each,
preserve ALL parser invariants (parser/SESSION.md). This is a correctness +
resource fix — get it right, but land the fast relief first.

## Diagnosis (confirmed)
`ProjectSession::parse_buffer` (driver.rs:293) calls `arena.insert_virtual(path,
content)` on EVERY call. `SourceArena.files` is an append-only `elsa::sync::
FrozenVec` and the arena is PROCESS-GLOBAL (`globals::arena()`), so every virtual
entry (full decoded String + retained raw bytes) lives forever. Two multipliers:
1. Every LSP READ handler re-parses: `analyze`, `resolve_definition`,
   `resolve_hover`, `resolve_references`, `resolve_completion`,
   `resolve_signature_help`, `resolve_semantic_tokens` each call `parse_buffer`
   (server/src/main.rs — ~7 sites). A single hover/completion/scroll appends a
   full-file copy.
2. Every `didChange` also re-parses.
→ The global arena grows without bound → OOM. Also every parse re-runs the full
pipeline (AST build + import touches) — CPU waste on top.

## Part 1 (FAST RELIEF — do first, commit alone) — read handlers reuse the meta
The meta from the last `analyze` is already in the cache (keyed by unit name).
Read handlers must NOT re-parse; they must reuse it.
- In `analyze`, after a successful `parse_buffer`, record the mapping the read
  handlers need: `Url → (unit_key, version)` (a map on `DelphiLsp`, behind a
  lock). 
- Change `resolve_definition`/`resolve_hover`/`resolve_references`/
  `resolve_completion`/`resolve_signature_help`/`resolve_semantic_tokens` to:
  look up the `unit_key` for the request's `Url`, fetch the CACHED meta from the
  session (a `ProjectSession` accessor like `meta_for(unit_key)` — add one if
  needed; the query methods already take `unit_key`), and run the query — with
  NO `parse_buffer` call. If no meta is cached for the Url yet (never analyzed),
  either trigger a single analyze or return empty — do not loop.
- This removes 6× the parse rate immediately and stops read-driven arena growth.
- Keep correctness: the position→offset mapping still uses the request document's
  LineIndex (current version). The cached meta's spans match that version because
  analyze parsed that version. Guard the version if needed (if the doc changed
  since analyze, a fresh analyze runs on the newer didChange anyway).
- Commit: `fix(lsp): read handlers reuse the analyzed meta, stop re-parsing`.

## Part 2 (THE MEMORY BOUND) — stop the arena growing per edit
Even parsing once per keystroke, `insert_virtual` still appends forever. Bound it:
the arena must hold at most ONE virtual entry per open-document PATH, reusing a
stable `FileId` and REPLACING (freeing) its content on re-parse.

Design latitude — pick the approach that is correct AND preserves the parser's
contracts. The hard constraints:
- **Stable `&str` DURING a parse:** the lexer/parser borrow `arena.content(file)
  -> &str` for the duration of one parse. A parse is synchronous and completes
  before the next; content for a given FileId may only be replaced BETWEEN
  parses, never during one. 
- **Span-provenance:** after a re-parse, `arena.content(file)` for that FileId
  MUST return the exact text the new meta's spans index (task-9 fix relies on
  this for ranges/hover). So content replacement and re-parse are atomic per
  document version.
- **Virtual-never-persist (#21/#25):** the reused virtual FileId must still carry
  a display-only path that fails `register` on load, so its meta is never
  persisted as on-disk state. Do not regress `virtual_open_buffer_does_not_
  survive_a_save_load_roundtrip`.
- **Disk files unchanged:** `load`/`register` for real on-disk files keep their
  current dedup-by-canonical-path behavior; this change is for VIRTUAL buffers.
- Suggested approach (evaluate against the constraints): give `SourceArena` a
  `set_virtual(path, content) -> FileId` that dedups virtual buffers by path
  (like disk files dedup by canonical path), reusing the FileId and REPLACING the
  stored content (dropping the prior String + raw bytes so memory is bounded).
  If replacing an entry's content while satisfying the stable-`&str` contract
  needs a storage change (e.g. content behind an interior-mutable cell that hands
  out a ref valid until the next replace, with replace only happening between
  parses under the session's serialization), implement it carefully and document
  why it's sound. `parse_buffer` calls `set_virtual` instead of `insert_virtual`.
- On `didClose` (server): free the document's virtual buffer (drop its content;
  the FileId may remain but its content should be releasable) and drop the
  Url→unit_key entry and any per-doc caches. Add a server `did_close` cleanup.
- Also consider: the L15 raw-byte retention doubles virtual-buffer memory — for
  VIRTUAL buffers the raw bytes are just the utf8 of the content; keep only what
  is needed (a virtual buffer's stamp can hash the decoded content directly, as
  it already does — avoid retaining a separate raw copy for virtual entries).

## Part 3 — prove it's bounded
- Parser test: call `parse_buffer` (or `set_virtual`) N=1000 times for the SAME
  path with changing content; assert the arena's VIRTUAL entry count stays ==1
  (or bounded), not N, and that memory for virtual content does not grow with N.
  Assert the latest content is what `content(file)` returns and its spans resolve.
- Parser test: virtual-never-persist still holds after the change.
- Server test: a sequence of didChange + several read requests creates NO new
  arena entries beyond the one per open document (or assert the arena virtual
  count is bounded across the sequence).
- Keep all existing parser + server tests green.

## Definition of done (adversarial-review gate)
- Read handlers do not re-parse; they reuse the analyzed meta (correct answers,
  same as before).
- The arena holds a bounded number of virtual entries under continuous editing
  (one per open document); superseded virtual content is freed — proven by a
  stress test that would OOM/grow under the old code.
- Stable-`&str`-during-parse, span-provenance, virtual-never-persist, dual-track,
  panic-free — all intact and tested. No wrong query answers.
- didClose frees the document's resources.
- Parser + server suites green; workspace builds.

Report: file-by-file, commits, exact test counts, the arena mechanism you chose
and WHY it's sound (stable-&str + span-provenance argument), the memory-bound
test result (entry count stays bounded across N edits), and anything unverified
(flag it). Output goes to an orchestrator.
