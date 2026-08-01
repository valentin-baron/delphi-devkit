# Spec — Task 19: bound the arena for disk-file text (the last unbounded term)

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. After task 16 the moka
AST cache is empirically bounded (~188MiB at the 256MiB cap). The REMAINING
unbounded term is the `SourceArena`: disk-file decoded text (`content:
OnceLock<String>`) + raw bytes (`raw: OnceLock<Vec<u8>>`) are written ONCE and
NEVER freed. As a session parses/reloads more units, arena RAM grows monotonically
regardless of AST eviction → process RAM still unbounded. This is also the user's
explicit design: "source file not necessary anymore until file hash changed" —
free a file's text after parse, re-read from disk on demand.

Commit per green step, both suites green before each. This touches the SAME unsafe
lifetime model as task 15 — soundness is the crux; get it right or it's a UAF.

## Grounding
- `parser/src/source.rs`: `SourceEntry { path, content: OnceLock<String>, raw:
  OnceLock<Vec<u8>>, is_virtual, virtual_content: Mutex<Option<Box<str>>> }`.
  `SourceArena.files: elsa::sync::FrozenVec<Box<SourceEntry>>` (append-only —
  the ENTRY boxes are stable, never moved). `content()`/`loaded_content()`/`text()`
  return `&str` into the entry; virtual entries already use a clearable
  `Mutex<Option<Box<str>>>` + `virtual_content_ref` (the task-15 lifetime-extend
  transmute, proven sound because access is serialized by the session lock and a
  borrow is released before the lock is). `free_virtual`/`set_virtual` exist.
- Task-15 soundness invariant (MUST be preserved and EXTENDED): a `&str` handed
  out of the arena is only borrowed DURING a synchronous parse/query, all
  serialized by the LSP session `blocking_lock()`; content for a FileId is only
  replaced/cleared BETWEEN parses/queries, when no borrow into it is live.

## The design — clearable, re-readable disk content + LRU trim at safe checkpoints
1. **Make disk-file content clearable + re-readable.** Change disk entries so
   `content` (and `raw`) can be dropped and re-read from disk on demand. Reuse the
   virtual mechanism's shape: an interior-mutable cell holding `Option<Box<str>>`
   (+ raw `Option<Box<[u8]>>`) that `content()` fills by reading the file if empty,
   and that a trim can clear. `content()` for a disk file: if present → return the
   (lifetime-extended) `&str`; if cleared/never-read → read from disk (the entry
   has the real path), store, return. Track a per-entry `last_access` (a cheap
   monotonic counter or `AtomicU64` tick bumped on each `content()`), for LRU.
   - Virtual entries keep their existing behavior (their "path" is display-only,
     cannot be re-read — a virtual file's content must NEVER be trimmed/cleared,
     or it's lost; only DISK entries with a real, re-readable path are trimmable).
   - `raw` bytes: same treatment; re-read for hashing on demand. (Stamp hashing
     must still hash the exact on-disk bytes — re-reading gives the same bytes
     unless the file changed, which is fine; hash-validation already handles
     change.)
2. **`trim_disk_content(cap_bytes)`** on `SourceArena`: while total resident disk
   content (sum of materialized `content`+`raw` lengths) exceeds `cap_bytes`,
   clear the LEAST-recently-accessed DISK entry's content+raw. Never clear virtual
   entries. Never clear an entry accessed in the "current" window (see soundness).
   Returns bytes freed / entries cleared (for a test/metrics).
3. **Call `trim` only at SAFE CHECKPOINTS (soundness-critical).** The session must
   call `trim_disk_content(cap)` ONLY when NO `&str` borrow into the arena is live
   — i.e. AFTER a parse/query has completed and its owned results are built, still
   under the session `blocking_lock()`, before returning. Add a
   `ProjectSession::trim_arena()` (or fold into an existing post-parse hook) and
   call it at the end of the blocking parse/query sections in the server
   (`analyze` and the read handlers, after building owned LSP results, inside the
   spawn_blocking, before the lock releases). Do NOT trim reactively inside
   `content()` (a borrow from an earlier file in the same parse may be live — that
   would UAF). Document this precisely, mirroring the task-15 SAFETY note.
   - Transient peak: a single parse chain (a unit + its directive-forced
     includes/imports) may materialize several files at once and can briefly
     exceed `cap` DURING that chain; trim afterwards brings it back. Since the
     eager-load fix limits one analyze to ~one unit's chain, the transient peak is
     bounded and small. Note this.

## Soundness (the adversarial-review target — argue it explicitly in code)
- Clearing a disk entry's content drops its `Box<str>`; any outstanding
  lifetime-extended `&str` into it would dangle. This is safe ONLY because trim
  runs at a checkpoint with no live arena borrows (all parse/query borrows dropped;
  the returned data is owned). Prove: every `content()`/`text()`/`try_text()`
  caller consumes the `&str` (to owned, or within the same synchronous
  parse/query) before the enclosing blocking section returns; trim is called after
  that. No `&str` from the arena escapes a blocking section (the task-15 review
  established this for query results — re-confirm it still holds and now also
  covers the parse path).
- Concurrency: the arena is process-global + Sync; trim must not run concurrently
  with a parse/query that holds a borrow. The single session `blocking_lock()`
  serializes them — trim is called while holding it, and parses/queries hold it
  too, so they're mutually exclusive. Re-reading in `content()` under a concurrent
  reader of a DIFFERENT file is fine (per-entry cells). Confirm no other thread
  (moka eviction listener, import loader) clears or reads disk content outside the
  session lock in a way that races trim.

## Tests
- Parse many distinct large DISK units, then `trim_disk_content(small_cap)`;
  assert resident disk content drops to ≤ cap and entries were cleared; then
  `content()` on a cleared entry RE-READS from disk and returns the SAME text
  (spans still resolve). Virtual entries are never cleared by trim.
- A cleared-then-re-read disk file: its `content()`/`text(span)` matches the
  original; a re-read after the on-disk bytes CHANGED returns the new bytes (and
  hash-validation would reject a stale cached AST — that's task-16's job, just
  confirm no crash).
- Soundness smoke: a parse that materializes several files, then trim, then a
  query on the parsed unit — no panic, correct results (exercises the checkpoint
  ordering).
- Preserve: virtual-never-persist, task-15 virtual bound (`didchange_and_reads_do_
  not_grow_the_arena`), reload-from-disk (task 16). Both suites green.

## Definition of done (adversarial-review gate)
- Disk-file arena text is bounded by a cap via LRU trim at safe checkpoints;
  cleared files re-read from disk on demand (source re-read only when needed).
- Soundness: no `&str` from the arena outlives its blocking section; trim never
  clears a live-borrowed entry; virtual content never trimmed. The unsafe model is
  argued explicitly and is as sound as task 15 (the review WILL hunt a UAF).
- A test proves resident disk content stays ≤ cap after trim and re-read is
  correct. Task-15/16 invariants intact; both suites green; workspace builds.

Report: file-by-file, commits, exact test counts, the clearable-disk-content +
LRU + checkpoint-trim design, the soundness argument (why trimming can't UAF), the
cap chosen + where trim is called, the bound test result, and anything unverified
(flag it — esp. that live-editor RAM wasn't measured, only the trim/re-read tests).
