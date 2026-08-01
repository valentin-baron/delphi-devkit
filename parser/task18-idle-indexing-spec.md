# Spec — Task 18: idle background project indexing

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Now that every parse is
memory-bounded (resident-only editor / budgeted Full / disk-backed cache / arena
trim), it is SAFE to warm the cache in the background. This restores the cross-
unit precision the budget/resident-only modes trade away: as units get indexed
(parsed + persisted + resident), `{$IF Declared/SizeOf}`, go-to-def, references,
completion resolve against the warm cache instead of degrading to Unknown.

Commit per green step, both suites green before each. Preserve foreground
responsiveness above all — indexing must NEVER stall the editor.

## Design
A background indexer owned by the server (`DelphiLsp`/`SessionManager`):
1. **Trigger:** start (or resume) an indexing pass when the editor goes IDLE — no
   didOpen/didChange/feature request for a short debounce (e.g. 1.5–2s). Cancel/
   pause the pass the instant any didChange or feature request arrives (foreground
   wins); resume after the next idle.
2. **Work list:** enumerate the project's own `.pas` units — from the session's
   search paths / the dproj, EXCLUDING the standard RTL/VCL source dirs (those are
   task 22's one-time bootstrap; don't re-index them every idle). Skip units
   already cached-and-fresh (hash-valid on disk). Deterministic order.
3. **Process one unit at a time** on a blocking task under the session lock, but
   RELEASE the lock between units so a foreground request can interleave: parse
   the unit (`parse_source_file`, budgeted Full — bounded), which persists its AST
   (task 16) and populates the reference index; then `trim_arena()` (task 19) so
   the arena stays at ~1–2 units across the whole pass. Yield/await between units
   (check the cancel flag each time).
4. **Cancelation:** an atomic "cancel/generation" token — a foreground event bumps
   the generation; the indexer checks it between units and stops. No half-written
   state (each unit parse is atomic; persistence is per-unit).
5. **Progress:** report via task-17 `begin_progress`/`report`/`end`
   ("Delphi: indexing", `report(Some(pct), Some("N/M — <unit>"))`, `end`). Best-
   effort; a progress failure never affects indexing.
6. **Bounded + non-interfering:** never hold the session lock across the whole
   pass (only per-unit); the memory bounds (moka evict, arena trim, disk persist)
   keep RAM flat as thousands of units are processed. Optionally throttle (a tiny
   sleep between units) so indexing doesn't peg a core.

## Server wiring
- A `spawn`ed background task (not `spawn_blocking` for the loop; use
  `spawn_blocking` for each unit's parse). Store its cancel token + a "last
  activity" instant on `DelphiLsp`.
- didOpen/didChange/feature handlers bump last-activity + set the cancel flag.
- A lightweight ticker/idle-check (or trigger indexing from the debounce after
  the last activity) starts the pass when idle.
- On session close/shutdown, stop the indexer.

## Constraints / never-wrong
- Indexing only WARMS the cache (parse + persist) — it never changes a query's
  correctness, only its completeness/precision (more resident → fewer Unknowns).
  A partially-indexed project is always correct, just less complete — never a
  wrong answer.
- Respect the memory bounds: per-unit parse is budgeted; `trim_arena` between
  units; rely on moka eviction + disk persistence. Prove RAM stays bounded across
  a large indexing pass (an RSS-style or entry-count/arena-resident bound test).
- Foreground latency: a test/argument that a foreground request during indexing is
  not blocked for more than one unit's parse (the lock is released between units).

## Tests
- Indexing a temp project of N units caches/persists them (each becomes hash-valid
  on disk + resident or reloadable); progress reported; a cancel token bumped
  mid-pass stops it promptly (processed < N).
- After indexing, a cross-unit `Declared`/definition that was Unknown/empty on a
  cold cache now resolves (precision restored) — proving the warm-up works.
- Arena/entry bound holds across the pass (resident set stays bounded, not N).
- Foreground non-interference: a simulated didChange during a pass cancels it and
  the buffer analyze proceeds.

## Definition of done (adversarial-review gate)
- Idle-triggered, foreground-preemptible indexing that warms the project cache one
  bounded unit at a time, persisting ASTs, trimming the arena between units, with
  progress in the status bar.
- RAM stays bounded across a large pass; foreground requests are never stalled
  more than one unit; cancellation is prompt and leaves no half-state.
- Never a wrong answer (warming only improves completeness); memory + task-15/16/
  19/21/25 invariants intact; both suites green.

Report: file-by-file, commits, exact test counts, the idle-trigger + cancelation
design, the bounded-RAM-across-a-pass proof, the foreground-non-interference
argument/test, the precision-restored test, and anything unverified (flag it —
esp. live idle behavior + real-project latency not measured).
