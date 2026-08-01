# Spec — Task 24: organize the frontend status view

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Task 17 wired transient
`workDoneProgress` (VS Code shows a spinner during an operation). This task adds a
PERSISTENT, at-a-glance status bar item reflecting the language server's overall
state, so the user always sees what ddk-server is doing (Ready / Analyzing /
Indexing N/M / Bootstrapping). Commit per green step, `cargo build` +
`cargo test -p ddk-server` + `npm run check-types` (extension) green.

## Server side (ddk-server)
- Define a custom notification the server pushes to the client, e.g.
  `ddk/serverStatus` with a small owned payload:
  `{ state: "Initializing"|"Ready"|"Analyzing"|"Indexing"|"Bootstrapping",
     detail: Option<String>,   // e.g. the unit/file name
     current: Option<u32>, total: Option<u32> }`  // for Indexing/Bootstrapping
  (a serde struct in the server; send via the tower-lsp `Client::send_notification`).
- Emit transitions (best-effort, never block/fail the operation):
  - `Ready` after `initialized` (and when analyze/indexing finish and nothing else
    is running).
  - `Analyzing { detail: filename }` at analyze begin; back to `Ready`/prior at end.
  - `Indexing { current, total, detail: unit }` from the task-18 indexing pass
    (per unit), `Ready` when the pass completes or is canceled.
  - (`Bootstrapping` is emitted by task 22 later — define the state now so the
    view is ready for it.)
- Keep it lightweight: a helper `set_status(state, detail, current, total)` on
  `DelphiLsp` that fire-and-forgets the notification. Debounce/coalesce is optional
  (per-unit Indexing updates are fine; the client can throttle rendering).
- This is SEPARATE from and complementary to task-17 `workDoneProgress` (keep 17 —
  it drives VS Code's built-in progress; this drives a persistent status item).

## Extension side (vscode_extension)
- Add a persistent `StatusBarItem` (left or right, low priority) shown while the
  language client is running: e.g.
  - Ready → `$(check) DDK` (or `$(database) DDK: Ready`)
  - Analyzing → `$(sync~spin) DDK: <file>`
  - Indexing → `$(sync~spin) DDK: Indexing 340/1200` (+ tooltip with the unit)
  - Bootstrapping → `$(sync~spin) DDK: Bootstrapping RTL 120/900`
  - Initializing → `$(loading~spin) DDK: starting…`
- Register a handler for the `ddk/serverStatus` notification (via the existing
  `LanguageClient.onNotification`, mirroring the existing
  `notifications/projects/update` handlers in client.ts) that updates the item.
- Click action: reveal the "DDK Server" output channel (or a no-op command) —
  wire a command if trivial, else omit the command.
- Dispose the status item on client stop/deactivate. Don't collide with the
  existing compiler status bar item (separate item, clear label).

## Constraints
- Best-effort telemetry: a status send that fails must never affect analyze/
  indexing. No lock held across the notification await (task-8 discipline).
- Never mislead: only show Indexing/Bootstrapping while actually running; return to
  Ready promptly on completion/cancel (tie into the task-18 cancel path so a
  preempted pass flips back to Ready/Analyzing, not a stuck "Indexing").
- TypeScript must `check-types` clean; keep the house style of the existing
  extension status/notification code.

## Tests
- Server: a unit test that `set_status` produces the expected notification payload
  shape for each state (like the task-17 progress payload test); and that the
  indexing pass emits Indexing→Ready around a pass (can assert the state helper is
  called with the right args, since the live transport isn't unit-testable).
- Extension: at minimum it type-checks + bundles; note that live rendering isn't
  unit-testable (same limitation as task 17).

## Definition of done (adversarial-review gate)
- A persistent status bar item shows Ready / Analyzing <file> / Indexing N/M /
  Bootstrapping N/M, driven by a best-effort `ddk/serverStatus` notification.
- State returns to Ready promptly on completion/cancel (no stuck spinner); the
  indexing/analyze paths emit transitions; task-17 progress still works.
- No lock/async regression; server tests + extension check-types green; disposed
  on stop.

Report: file-by-file, commits, exact test counts + extension check-types result,
the notification schema + where each transition is emitted, how the extension
renders + disposes it, and anything unverified (flag it — esp. live status-bar
rendering not visually confirmed).
