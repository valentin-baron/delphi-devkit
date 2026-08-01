# Spec — Task 14 (parts 3 & 4): ObjectPascal TextMate grammar + LSP cache-save

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Parts 1 & 2 (language
registration `objectpascal` for .pas/.dpr/.dpk/.inc + a language-configuration +
the client `documentSelector`) are ALREADY DONE and committed-pending. Do NOT
redo them. This spec covers the two remaining pieces. Commit per green step
(`feat(lsp): …` / `feat(extension): …`), verify builds, no happy-path shrugs.

## Part 3 — ObjectPascal TextMate grammar (base coloring)
The extension registers the `objectpascal` language but ships NO source grammar,
so base coloring relies entirely on the sparse semantic-token layer. Add a real
TextMate grammar so operators, local variables, and everything the semantic
layer omits get colored; semantic tokens then REFINE on top.

- Create `vscode_extension/config/languages/grammar/objectpascal.tmLanguage.json`
  — a TextMate grammar (`scopeName` e.g. `source.objectpascal`) covering:
  - **Comments:** `{ … }`, `(* … *)` (block), `// …` (line), and — IMPORTANT —
    do NOT swallow compiler directives `{$…}` / `(*$…*)` as comments; scope them
    distinctly (e.g. `meta.preprocessor` / `keyword.control.directive`).
  - **Strings:** single-quoted `'…'` with `''` escape; char codes `#39` / `#$41`
    (scope as `constant.character`); adjacency `'a'#13'b'`.
  - **Numbers:** decimal, `$hex`, `%binary`, floats with exponent
    (`constant.numeric`).
  - **Keywords:** reserved words (control: `if/then/else/case/while/for/repeat/
    until/try/except/finally/begin/end/…`; declaration: `unit/interface/
    implementation/uses/type/const/var/procedure/function/class/record/…`;
    operators-as-words: `and/or/not/xor/div/mod/in/is/as/shl/shr`). Case-
    INSENSITIVE (Delphi is case-insensitive) — use `(?i)`.
  - **Types:** the common built-in type names (Integer/String/Boolean/…) as
    `support.type` (optional but nice).
  - **Operators/punctuation:** `:=`, `+ - * / < > = <> <= >= @ ^ .. :` etc.
  - **Contextual keywords** (`name/index/read/write/message/default/…`): these
    are ALSO valid identifiers; a TextMate grammar can't disambiguate perfectly
    — prefer NOT force-coloring them as keywords in identifier position (keep the
    never-wrong spirit; the semantic layer refines identifiers anyway).
- Register it in `package.json` `contributes.grammars`:
  `{ "language": "objectpascal", "scopeName": "source.objectpascal",
     "path": "./config/languages/grammar/objectpascal.tmLanguage.json" }`.
- Validate: the grammar JSON parses; `npm run check-types` + `node esbuild.js`
  still succeed; VS Code can load it (no scope/regex errors — test the regexes
  are valid). If you can't fully verify in-editor, at minimum assert the JSON is
  well-formed and the patterns compile as regexes (a small node script).
- Keep it MIT-compatible / original — write the patterns yourself; do not paste a
  license-encumbered grammar.

## Part 4 — LSP cache persistence (server)
Today the session NEVER persists: `SessionOptions.watch:false`, no autosave
`tick`, and the LSP `shutdown` handler (main.rs:873) does not save. So the
LocalAppData snapshot is never written. (Note: the ACTIVELY-EDITED buffer is a
VIRTUAL buffer and never persists BY DESIGN — invariant #21/#25 — so only
on-disk units, e.g. a saved file and its disk-parsed imports, are cacheable.)

Wire persistence via the SessionManager / DelphiLsp (server/src/{session.rs,
main.rs}), keeping the task-8 async/lock discipline (session work on
spawn_blocking, no lock across `.await`):
1. **On `did_save`** (`textDocument/didSave`): the file is now on disk — parse it
   from DISK via `ProjectSession::parse_source_file` (so it AND its imports enter
   the cache as persistable on-disk units, not virtual), then call
   `session.save_now()` to write the snapshot. (didSave currently isn't handled;
   add the handler.)
2. **On LSP `shutdown`**: call `session.save_now()` (best-effort; log on error,
   never panic) before returning — so edits made in the session persist.
3. **Optional periodic autosave**: if cheap, drive `session.tick(now)` on a
   timer (the session already has `save_interval` + dirty tracking) so long
   sessions persist without waiting for shutdown. If it complicates the async
   model, skip it and rely on didSave + shutdown, and note it.
- `save_now`/`tick` return a `SaveReport{written, skipped}` — LOG skipped units
  (do not swallow), consistent with the never-silent-swallow rule.
- Expose whatever SessionManager method is needed (e.g. `save_now()`,
  `parse_disk_and_save(path)`), behind the same lock model.
- Tests (server): a didSave path parses the file from disk and calls save_now
  (assert a snapshot is written to a temp cache dir — reuse the fallback-session
  test harness with an explicit snapshot base); shutdown triggers a save. Do NOT
  assert the virtual open-buffer persists (it must not).

## Execution order (commit per green step)
1. Part 4 cache-save (server) + tests. Commit (`feat(lsp): persist cache on didSave/shutdown`).
2. Part 3 grammar + package.json registration + JSON/regex validation. Commit (`feat(extension): ObjectPascal TextMate grammar`).
3. `cargo build` + `cargo test -p ddk-server` + `cargo test -p delphi-parser` green; `npm run check-types` + esbuild clean; README/CHANGELOG note the new language + grammar + cache persistence.

## Definition of done (adversarial-review gate)
- didSave parses the saved file from disk and persists the snapshot; shutdown
  saves; virtual open buffers still never persist; skipped units logged.
- The grammar colors comments/strings/numbers/keywords/operators; `{$…}`
  directives are NOT mis-scoped as plain comments; JSON + regexes valid; the
  extension still type-checks and bundles.
- Async/lock discipline intact; parser + server tests green; workspace builds.

Report: file-by-file, commits, exact test counts, how you wired persistence
(didSave/shutdown/tick), a note on the grammar's directive handling, whether you
verified the grammar in-editor or only by JSON/regex validation, and anything
unverified (flag it). Output goes to an orchestrator.
