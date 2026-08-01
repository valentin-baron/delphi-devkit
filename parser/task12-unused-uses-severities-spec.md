# Spec — Task 12: unused-uses diagnostics + per-finding severities

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Build on task-8..11.
Commit per green step (`feat(lsp): …`), `cargo build` + both suites green before
each. Governing rule (carried, and it BINDS HARD on unused-uses): never a WRONG
answer. A FALSE "unit unused" hint makes a user delete a needed unit and break
the build — so only flag a unit when we are CONFIDENT nothing references it, and
even then as a HINT with the caveat, never an error/removal claim.

## Part A — per-finding severities (replace all-WARNING)
Today every diagnostic maps to LSP WARNING. Make severity meaningful:
- Add a `Severity` enum (`Error | Warning | Information | Hint`) to the parser's
  diagnostics. `token_cursor::Diagnostic` is `{ location, message }`;
  `query::UnifiedDiagnostic` is `{ source, location, dfm_offset, message }`. Add
  a `severity: Severity` to BOTH, set at each CREATION SITE (the code that emits
  the finding knows its kind):
  - lexer error in an ACTIVE region / unrecoverable parse error → `Error`
    (a real syntax error in code the user must fix).
  - error-tolerant recovery resync (a declaration was dropped) → `Warning`.
  - unknown `{$IF}` expression (→ AssumeFalse) → `Warning`.
  - dropped/undecidable attribute, dropped-pending-attribute-at-section-end →
    `Information` (or `Hint`).
  - DFM hard finding (dangling component, missing handler, type mismatch) →
    `Warning`; DFM "possibly inherited" note / "form class not found" note →
    `Hint`/`Information`.
  - unused-uses (Part B) → `Hint`.
  Grep every `Diagnostic { … }` / `push_diagnostic` / `UnifiedDiagnostic { … }`
  construction and set an accurate severity — do NOT default-fill `Warning`
  blindly; each site chooses.
- Server `diagnostics.rs`: map `Severity` → `DiagnosticSeverity`
  (Error/Warning/Information/Hint) instead of the hardcoded WARNING. Update the
  existing diagnostic tests to assert the mapped severity for a couple of kinds.

## Part B — unused-uses analysis (conservative HINT)
A `uses` entry whose unit contributes NO referenced symbol is a candidate
"unused". This is inherently heuristic (a unit can be needed for initialization
SIDE EFFECTS, a re-exported type, an ancestor, operator overloads) — so it must
be CONSERVATIVE and honest.

- Parser query (parser/src/driver.rs): `unused_units(unit_key) ->
  Vec<UnusedUnit>` where `UnusedUnit { unit: Identifier, location: CodeLocation
  /* the uses-clause entry span */ }`. Algorithm, per uses entry U of the
  target unit (interface AND implementation uses; the AST carries the uses
  entries with their spans):
  1. Load U's interface via the SAME loader as `definition` (cycle-safe,
     dependency-recorded). If U can't be loaded (missing source / DCU-only) →
     DO NOT flag it (we can't prove it unused) — skip.
  2. Collect U's exported symbol keys (its interface symbols' folded keys).
  3. If NONE of U's exported keys appears anywhere in the target unit's usage set
     (`meta.usages` + interface-body references) → U is a candidate unused.
  4. If ANY exported key appears → U is "possibly used" → DO NOT flag
     (conservative; the over-approximating usage index means a name-match is
     enough to spare it — a false "used" is safe, a false "unused" is not).
  - NEVER flag a unit whose interface could not be fully resolved, or that is
    consulted as a dependency for `{$IF Declared/SizeOf}` (those are real uses),
    or reached via a cycle (taint → skip).
- Surface each `UnusedUnit` as a `UnifiedDiagnostic` (new `DiagnosticSource`
  variant, e.g. `Analysis`, or reuse Parse with a clear message), severity
  `Hint`, message like: `unit 'Foo' is in the uses clause but none of its
  symbols are referenced (it may still be needed for initialization side
  effects)` — an honest hint, not a removal instruction.
- Wire it into `session.diagnostics(unit_key)` (or a dedicated call the server
  merges) so it publishes alongside parse+dfm diagnostics on didOpen/didChange.
- Tests (parser): a unit that imports Used + Unused, references only Used →
  exactly Unused flagged; a unit whose only reference to Foo is by a name that
  ALSO exists elsewhere → still not flagged if a Foo export key matches (spare
  it); an unloadable import → not flagged; a `{$IF Declared(Foo.X)}`-only use →
  not flagged (Foo consulted as a dependency = used).

## Execution order (commit per green step)
1. `Severity` on parser diagnostics + set at every creation site; server maps it;
   update diagnostic-severity tests. Commit.
2. Parser `unused_units` query + UnusedUnit type + conservative algorithm +
   parser tests. Commit.
3. Merge unused-uses into the published diagnostics (Hint severity) + server
   test that an unused import surfaces as a Hint and a used one does not. Commit.
4. `cargo build` + both suites green; README updated (severities now meaningful;
   unused-uses as a conservative hint with its limitations). Commit.

## Definition of done (adversarial-review gate)
- Severities are per-finding and accurate (an unknown-{$IF} is a Warning, a
  syntax error an Error, a dropped attribute Info/Hint, unused-uses a Hint) — not
  all-WARNING; server maps them 1:1.
- unused-uses NEVER flags a unit that is referenced, unloadable, consulted as a
  dependency, or reached via a cycle (conservative — a false "unused" is a HIGH
  defect because it invites breaking the build); it's a Hint with the
  side-effect caveat, never an error.
- Wired into the published diagnostics; correct ranges (the uses entry span).
- Queries off the async executor behind the session lock; caps unchanged (this
  is diagnostics, already published). Parser invariants intact; both suites
  green; workspace builds.

Report: file-by-file, commits, exact test counts, the severity mapping table,
unused-uses proof points (esp. the conservative never-false-flag cases), and
anything unverified (flag it). Output goes to an orchestrator.
