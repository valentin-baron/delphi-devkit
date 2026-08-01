# Spec — Task 10: find-references + rename

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. Build on task-8/9 infra
(positions, DocumentStore, SessionManager+spawn_blocking, `locations::
code_location_to_lsp`, publish version guard). Commit per green step
(`feat(lsp): …`), `cargo build` + both test suites green before each. Governing
rule (carried, and it BINDS HARDEST here because rename is destructive): NEVER a
wrong answer. A read query returns empty over a guess; a destructive edit must
NEVER touch an identifier that isn't the renamed symbol, and must never leave a
rename incomplete (dangling old references).

## Query API you consume (parser, already built)
- `session.references(symbol_key: Identifier) -> Vec<Occurrence>`;
  `Occurrence { unit: Identifier, location: CodeLocation }`. This is an
  OVER-APPROXIMATING candidate set: occurrences are matched by folded key from
  the (scope-unresolved) usage index — it may include unrelated identifiers that
  merely share the name (a local var, a different symbol with the same name),
  and it never misses a real occurrence in a cached unit.
- `session.symbol_at(unit_key, position) -> Option<QueryTarget>` (key, display,
  kind Declaration|Member|Usage, location, owner_type).
- `session.definition(unit_key, key, owner) -> Vec<CodeLocation>`.
- `locations::code_location_to_lsp(session, location) -> Option<Location>` (task 9).

## Deliverable A — textDocument/references (SHIP; read-only, honest)
- Advertise `references_provider`.
- Handler: (Url, Position) → (unit_key, offset) → `symbol_at` → if a target,
  `session.references(target.key)`; map each `Occurrence.location` via
  `code_location_to_lsp` → `Vec<Location>`. Honor `context.include_declaration`
  (drop the declaration site if false). Empty when no target.
- This is a CANDIDATE set (document it in code + README): for read-only "find all
  references" that the user visually reviews, over-approximation is acceptable
  and matches how the parser documents the index. Do NOT claim precision it
  doesn't have.
- Run on spawn_blocking behind the session lock (task-8 discipline).
- Tests: references across two units incl./excl. the declaration; a symbol with
  no refs → empty; ranges correct in each occurrence's own file.

## Deliverable B — rename: CORRECTNESS-GATED (this is the hard part)
A rename must be BOTH complete (rewrite every real reference) AND correct
(rewrite nothing else). The occurrence set from `references` is over-
approximating (scope-unresolved), so:
- Renaming the whole candidate set → may rewrite an UNRELATED same-named
  identifier (a local var `Result`, a different unit's `Name`) — a destructive
  WRONG edit.
- Renaming only the provably-bound subset (declaration + resolved interface
  refs) → LEAVES impl-section uses un-renamed → dangling/broken code — also wrong.
Neither is acceptable under the never-wrong rule. Therefore:

1. **Assess honestly.** With the current unscoped usage index, determine whether
   ANY provably-safe rename subset exists that is both complete and correct. It
   likely does NOT for the general case (local-variable name collisions can't be
   detected without scope resolution).
2. **Preferred outcome: DEFER rename** — do NOT advertise `rename_provider`; add
   a numbered `parser/SESSION.md` ledger entry stating that a correct+complete
   rename requires scope-resolved bindings (over-approximation over-renames;
   declaration-only under-renames), with a plan (scope resolution / symbol table
   is the prerequisite; it also sharpens references and member-usage owners —
   #41). This is the honest, safe call and is ACCEPTABLE as the task outcome.
3. **OR, only if you can PROVE it safe:** implement a strictly-limited rename that
   refuses (prepareRename → null with a message; or a JSON-RPC error) unless the
   operation is provably safe, and prove the safety gate leaves NO path to
   renaming an unrelated identifier or to an incomplete edit. If you cannot prove
   both, do (2). Do NOT ship a rename that "usually works" — rarity is not a
   justification (working rule).

Whichever you choose, `prepareRename` (if advertised) must reject positions where
identity isn't established (bare usages), and the decision + reasoning go in the
report and README.

## Execution order (commit per green step)
1. references handler + capability + tests. Commit.
2. Rename assessment: implement the safe-subset-or-defer decision; if defer,
   ledger entry + README note (no capability advertised); if implement, the
   gated handler + prepareRename + a WorkspaceEdit builder (TextEdits grouped by
   Url, each range via the occurrence's own-file mapping) + tests proving the
   gate blocks every unsafe case. Commit.
3. `cargo build` + both suites green; README updated (references shipped; rename
   status). Commit.

## Definition of done (adversarial-review gate)
- references: cross-unit, ranges correct per file, include-declaration honored,
  empty on no-target; documented as a candidate set.
- rename: EITHER deferred with a sound ledger entry and no advertised capability,
  OR a correctness-gated implementation with a proof (and tests) that it can
  neither rewrite an unrelated identifier nor produce an incomplete edit.
- All queries off the async executor behind the session lock; no deadlock; no
  lock across await; capabilities match what's implemented.
- Parser invariants intact; parser tests green; workspace builds.

Report: file-by-file, commits, exact test counts, references proof points, the
rename decision WITH its correctness argument (why the chosen path can't produce
a wrong or incomplete edit), and anything unverified (flag it). Output goes to an
orchestrator.
