# Spec — Task 16: disk-backed AST cache (memory-bound core)

Repo: C:\workspaces\vscode\delphi-devkit, branch `lsp`. The LSP OOMs because the
AST cache is RAM-resident: moka holds full ASTs, its weigher UNDERCOUNTS real
heap so the 512MB cap doesn't actually bound RAM, and on eviction an AST is LOST
(a re-access re-parses from source). Implement the user's design: parse → persist
the AST to disk → drop from RAM → reload the AST from disk (fast, no re-parse)
when needed again, unless the source hash changed. Commit per green step, both
suites green before each, preserve ALL invariants (parser/SESSION.md).

## Grounding (what exists)
- `unit_cache.rs`: `UnitCache` = `moka::sync::Cache<Identifier, CacheEntry>`,
  `max_capacity(DEFAULT_CAPACITY_BYTES=512MiB)`, weigher = `meta.estimated_bytes()`
  (shallow — undercounts). `insert(key, meta)`, `get(key)`, `insert_count`.
- `cache_store.rs`: `CacheStore` (snapshot dir per project identity),
  `save(cache)` = BULK snapshot of all Done entries, `load_into(cache)` = bulk
  load on open. Uses the transparent `Identifier`/`FileId` serde + per-unit hash
  validation (`SourceStamp` xxh3 of own source + includes + dependency sources).
- `unit_loader.rs`: `interface_of(key)` → cache hit? else resolve name → file →
  `parse_and_cache` (RE-PARSES from source on every miss). This is the reload
  hook.
- Invariants: virtual buffers NEVER persist (#21/#25); transparent serde
  re-interns/re-registers on load; hash-gated validity; dual-track; never-wrong.

## Deliverable A — per-unit AST persistence
Add to `cache_store.rs`:
- `save_unit(&self, meta: &UnitMeta) -> Result<(), CachePersistError>` — write ONE
  unit's `UnitMeta` to a per-unit snapshot file in the store's directory (filename
  = a hash of the unit key, like the bulk snapshot names). Skip virtual/tainted
  units (they must never persist — reuse the same gate as `save`:
  `cycle_tainted`/`recovered` skipped; a unit whose source FileId can't serialize
  to a real path → skip, not error).
- `load_unit(&self, unit_key, ...) -> Result<Option<Arc<UnitMeta>>, _>` — read the
  per-unit file, deserialize (transparent serde re-registers FileIds / re-interns
  Spurs), and HASH-VALIDATE it (own source hash + include stamps + dependency
  stamps) exactly like `load_into` does per entry. Return `Some` only if valid;
  `None` (and ideally delete/ignore the stale file) on hash mismatch / missing /
  corrupt (corrupt ≠ crash — mirror the panic-free decode discipline).
- Atomic writes (temp + rename), corrupt ≠ missing — reuse the existing patterns.
- Keep the bulk `save`/`load_into` working (shutdown/didSave still bulk-save; or
  make bulk-save iterate `save_unit` — your call, but don't regress persistence).

## Deliverable B — persist-on-insert (disk units only) + evict-to-disk
- When a DISK unit is parsed and inserted into the cache, persist it via
  `save_unit` so it's on disk BEFORE it can be evicted. VIRTUAL units are never
  persisted (invariant) — gate on the same virtual/tainted check. Do this where
  the pipeline inserts a freshly-parsed on-disk unit (driver/pipeline/loader),
  NOT for the active editor buffer.
  - IO note: a disk unit is parsed ONCE then cache-hit thereafter, so persist-on-
    insert is one write per unit, not per keystroke. Acceptable. (The active
    buffer re-parses per edit but is virtual → never written.)
- Add a moka `eviction_listener` on the cache that, if a Done entry is evicted
  and somehow not yet persisted, persists it (best-effort, log-not-panic) — so
  eviction is always a safe drop (reloadable from disk), never data loss. With
  persist-on-insert this is belt-and-suspenders, but wire it so eviction can
  never strand an unpersisted AST. (Give `UnitCache` access to the `CacheStore`
  or a persist callback — thread it through cleanly.)

## Deliverable C — lazy reload-from-disk on cache miss
- In `interface_of` (unit_loader) and any cache-miss path (`meta_of`/query
  cache-miss where appropriate), BEFORE re-parsing from source: try
  `store.load_unit(key)` (hash-validated). If it returns `Some`, insert into the
  moka cache and use it — NO re-parse, NO source read. Only if `None` (no valid
  snapshot / hash changed) fall through to `parse_and_cache` from source.
- Effect: an evicted unit reloads from disk (cheap); source is re-read/re-parsed
  ONLY when its hash changed. This is the user's "source not needed until file
  hash changes."
- Thread the `CacheStore` (or a loader-visible handle to it) into the loader so
  it can `load_unit`. Keep cycle-safety and dependency recording intact.

## Deliverable D — fix the weigher so RAM is actually bounded
- `estimated_bytes` undercounts (it uses shallow member counts; `UnitMeta` holds
  the whole AST: nested `TypeExpression`s, member vecs, usages, etc.). Make it a
  MUCH closer proxy for real heap — e.g. a deep walk that sums per-node costs, OR
  a cheap robust proxy proportional to real size (the unit's SOURCE byte length
  is a good cheap proxy for AST size; combine with member/usage counts). The goal
  is that the 512MB (consider lowering the default to ~128–256MB for an editor)
  cap keeps ACTUAL process RAM for the cache bounded near the cap. Document the
  chosen estimate + capacity.
- Strengthen the M1 weigher test intent: assert a large unit weighs
  substantially more than a tiny one (proportional-ish), so an undercount
  regression is caught.

## Deliverable E — prove it's bounded
- Test: parse N distinct large units (or synthesize metas) into a cache with a
  SMALL capacity; assert moka evicts (entry_count stays bounded, not N) and that
  each evicted unit is reloadable via `load_unit` (round-trips, hash-valid).
- Test: reload-on-miss path — evict a unit, then `interface_of`/`load_unit`
  returns it from DISK without re-parsing (assert no source read / a parse-count
  probe), and after changing the source's bytes the reload is rejected (hash
  mismatch) and a re-parse occurs.
- Test: virtual units are NEVER written by `save_unit` / persist-on-insert /
  eviction (invariant #21/#25 intact).
- All existing parser + server tests green.

## Definition of done (adversarial-review gate)
- RAM for the AST cache is bounded near the moka cap under many-unit workloads
  (evict-to-disk + a non-undercounting weigher) — proven by a test.
- A cache miss reloads the AST from disk (hash-validated) instead of re-parsing;
  source is re-read only on hash change — proven.
- Per-unit persistence + reload round-trips correctly (FileIds re-registered,
  Spurs re-interned, dual-track intact, spans resolve); corrupt/stale files are
  ignored, never crash.
- Virtual/tainted/recovered units never persist. Bulk save/load still work.
- All invariants intact; both suites green; workspace builds.

Report: file-by-file, commits, exact test counts, the persistence/reload wiring,
the weigher estimate + capacity chosen (and the bound-holds test result), the
lazy-reload proof (no re-parse on hash match; re-parse on mismatch), and anything
unverified (flag it — esp. that live-editor RAM wasn't measured, only the
bounded-eviction + reload tests).
