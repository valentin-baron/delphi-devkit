# Delphi Parser — Session Progress

Loop marker file. Exists = loop started, do not re-prompt the user.
(Cron job ids are session-bound; a fresh session re-establishes its own
loop if asked — the marker semantics stay.)

## Quickstart for a fresh session

- `cargo test` — portable suite (must stay green).
- `cargo test --features local-tests` — adds machine-bound tests
  (C:\Delphi\VSS paths, live ddk CLI, 110-unit stress test `core_tree`).
- Work top-down from "Next steps" below; log every iteration here;
  ledger discipline is mandatory (see working rules).

## Working rules (user-mandated, apply to ANY agent)

- NO "works 98% of the time" arguments: every edge case is handled or gets
  a numbered ledger entry with a plan.
- No abbreviated identifiers (`context`, not `ctx`; `position`, not `pos`).
  Established CS terms (DFS) are fine.
- Don't stop after one roadmap point — keep pulling work until the budget
  is genuinely exhausted or user input is required; then say so explicitly.
- Commits: minimal one-line messages after every completed step; history
  is discarded when the crate moves into delphi-devkit.
- Local/machine-bound tests go behind the `local-tests` cargo feature.

## Architecture (current)

One pass per unit, lazy imports, cached artifacts:

```
SourceArena (stable &str buffers; eager load / lazy register)
  → logos lexer (payload-free tokens; spans into arena)
  → TokenCursor (directive engine: $IFDEF/$IF via if_eval, $DEFINE,
    $I includes incl. %VAR% pseudo-includes, switches; dead branches
    swallowed incl. unlexable compile-breaker text; 2-token lookahead)
  → grammar parser (headers, uses, deep type parse; expressions = spans)
  → pipeline (AST → UnitArtifact: interface symbols + members, usages,
    include/dependency hash stamps) → moka UnitCache
  → CacheStore (bincode snapshot in %LOCALAPPDATA%, hash-validated load)

{$IF Declared/SizeOf/Const} during a parse → StateResolver → UnitLoader
(per-chain Rc; cycle stacks via begin_unit/end_unit + active files)
→ resolve_unit (aliases → direct → namespaces over search paths)
→ nested parse_and_cache — recursion IS the "yield".

ProjectSession (driver.rs) owns arena/context/store/watcher/reverse-index:
open = context swap (snapshot load + index rebuild), tick = watcher poll →
per-file invalidation or burst full-sweep → autosave, shutdown = final save.
```

## Invariants (breaking these is silent corruption)

- Dual-track interning: `intern` = display (as written), `intern_key` =
  case-folded. EVERY comparison/cache/symbol-table key uses `intern_key`.
  The interner itself stays case-preserving. The fold is ONE function,
  `globals::fold_identifier` (ordinal ASCII a→A, non-ASCII byte-identical) —
  every folded-key producer routes through it so the tracks can never diverge
  (it. 14, L9). It is behaviour-identical to the old `to_uppercase` for ASCII;
  for non-ASCII it never produces a wrong match.
- AST carries NO raw Strings — only interned Identifiers and span
  CodeLocations. Expressions (bounds, defaults, enum values) are SPANS.
- `Spur`s and `FileId`s are session-local: persistence resolves to strings/
  paths on save and re-interns/re-registers on load. Never serialize raw. As of
  it. 9 this is AUTOMATIC — `Identifier` (newtype over `Spur`) and `FileId` carry
  transparent serde through the process-global interner/arena (`globals.rs`);
  proven by `globals::tests::identifier_bytes_are_a_string_not_a_raw_spur`.
- moka is eventually consistent: `entry_count`/`iter()` freshness is not
  guaranteed — decisions use `insert_count`; `save` calls
  `run_pending_tasks()` first.
- Virtual (unsaved) buffers never persist: their hashes AND locations fail
  load-time validation by design (#21, #25).
- `cycle_tainted` artifacts (interface uses-cycles, invalid Delphi) are
  cached in-session but never saved.
- Delphi semantics: `{$DEFINE}`/switches are unit-local (state copies);
  conditionals span include boundaries; `{$IF}` in dead branches is never
  evaluated; interface-visible symbols = builtins + earlier own decls +
  interfaces of already-seen uses.

## Decisions (from user, 2026-07-16)

- **Priority: infrastructure first** (cache persistence in LocalAppData, file
  watchers, git-checkout burst detection), then core parsing slices.
- **Case handling: dual-track.** Interner stays case-preserving (it may later
  hold strings/comments verbatim — case-folding at intern would corrupt them).
  All *lookups/comparisons* (defines, unit names, aliases, cache keys) go
  through a separately interned folded key (`intern_key`).
- **AST contains NO raw Strings — only Spurs/Identifiers.**
- **LSP server is NOT built here.** This crate = parser library, deliberately
  decoupled for stability; it will be integrated into delphi-devkit
  (C:\workspaces\vscode\delphi-devkit) and consumed there.
- **Delphi 12 (CompilerVersion 36) is the target for now.**
- **NO "works 98% of the time" arguments.** Every edge case is either handled
  or recorded in the ledger below with a plan.
- Commits: minimal messages ("wip"-style), history is discarded on devkit
  integration.

## Decision (from user, 2026-07-31) — LSP persistence: serialize the FULL AST

Supersedes the "persist a lossy `UnitArtifact` projection" model FOR THE LSP
TARGET. Rationale traced in the 2026-07-31 session.

- **One LSP server hosts one project per process.** Therefore the interner and
  the source arena become **process-global statics**. `Interner =
  lasso::ThreadedRodeo` already interns through `&self` (thread-safe), so a
  global is correct even across LSP worker threads — no per-thread Spur
  corruption (the reason a `thread_local` was rejected).
- **Transparent serde via the statics.** `impl Serialize/Deserialize for
  Identifier` resolves through the static interner (Spur→String on save,
  `get_or_intern`→Spur on load). `FileId` resolves through the static arena
  (→path on save, re-register on load). Disk still stores strings/paths, never
  raw Spurs/FileIds — same invariant as today (SESSION line ~60), now automatic
  instead of a hand-written `SavedSymbol` mirror.
- **Dual-track survives for free:** each `Identifier` round-trips its OWN exact
  string (display track → "TFoo", key track → "TFOO"); `get_or_intern` on load
  reproduces the right Spur. No code may assume intern vs intern_key Spurs are
  related — they are two independent entries.
- **The persisted unit is the whole `ast` tree + metadata** (source/include/
  dependency stamps, cycle_taint). Helper methods (`interface_of`, symbol/member
  lookup, usage queries) move onto a **`UnitMeta` wrapper** `{ ast, stamps,
  dependencies, cycle_taint, <derived indexes> }`. The interface surface +
  member symbols become indexes DERIVED from the AST (built lazily, cached
  in-memory), not a separately-persisted lossy struct.
- **Consequence accepted:** bincode is not self-describing → any AST node change
  bumps the format → one cold re-parse of the project. Fine; the cache
  re-parses on staleness anyway.
- **Not persisted, still transient by design:** the active file's AST is rebuilt
  live per keystroke (error-tolerant reparse); only OTHER units load from the
  snapshot. `cycle_tainted`/virtual-buffer artifacts remain never-persisted.

## Memory & loading architecture (LSP integration) — the consolidated model

The parser was designed for BOUNDED parses (batch/CLI). An editor reparses
continuously and navigates a huge dependency graph (be.core + RTL/VCL), which
broke that assumption catastrophically: opening one real unit force-loaded its
whole `{$IF Declared/SizeOf}` closure (nested parses pin every ancestor's text +
AST) → 14 GB. The fix is a layered memory model; each layer is independently
verified (tests + RSS probe, since the LSP can't be driven headlessly). The
through-line: **no parse ever cascades; the working set is bounded; precision is
restored by background indexing, not eager-during-parse.**

1. **One project per process** → the interner + `SourceArena` are process-global
   statics (`globals.rs`). Bounds are enforced per-operation and via LRU, not by
   holding everything resident.
2. **Editor parse is resident-only** (`UnitLoader` `LoadMode::Budgeted(0)`, used
   by `ProjectSession::parse_buffer`): opening/typing a unit parses ONLY that
   unit; cross-unit `{$IF Declared/SizeOf}` resolves from the RESIDENT moka cache
   or degrades to Unknown (safe AssumeFalse) — never force-parses an import. No
   cascade. (Proven: parsing a 10-deep `uses` closure leaves 1 unit cached.)
3. **Every Full parse is budgeted** (`LoadMode::Budgeted(MAX_TRANSITIVE_LOADS=32)`,
   used by `parse_source_file` = didSave / navigation / indexing, and `make_loader`
   = query cross-unit resolution): a chain force-loads at most 32 units then
   degrades to Unknown. Caps didSave/go-to-def peak (a cache HIT is always free
   and never counts). The counter is chain-shared via the loader's `Rc`.
4. **Disk-backed AST cache** (`cache_store` `save_unit`/`load_unit`, `unit_cache`
   moka + eviction-listener): a parsed disk unit's AST is persisted per-unit; on
   a cache miss the loader RELOADS the AST from disk (hash-validated: source +
   includes + deps) before re-parsing — source is re-read/re-parsed ONLY when its
   hash changed. moka evicts to disk at a 256 MiB weigher cap (weigher =
   `estimated_bytes` = source_len×16 + structural interface-index charge; RSS-
   verified real cache RAM ≈ 188 MiB ≤ cap). Virtual/tainted/recovered units
   never persist.
5. **Arena LRU trim** (`source.rs` clearable disk content + `trim_disk_content`,
   `ProjectSession::trim_arena`, cap 64 MiB): a disk file's decoded text + raw
   bytes are FREED between units (LRU) and re-read from disk on demand — "source
   not needed until the file hash changes". `trim_arena` runs only at SAFE
   CHECKPOINTS (after owned results are built, under the session lock, no live
   `&str` borrow) — the same unsafe-lifetime discipline as the virtual buffer
   mechanism (see the SAFETY note on `disk_content_ref`/`virtual_content_ref`).
6. **Idle background indexing** (`server/indexing.rs`, `SessionManager::index_unit`,
   `run_indexing_pass`): on editor idle (1.5 s debounce), parse the PROJECT's own
   units one BUDGETED unit at a time, persist each, `trim_arena` between — RAM
   flat across thousands of units. Foreground-preemptible (a generation token
   bumped on any didChange/request; checked between units; a foreground request
   waits ≤ one unit). NEVER indexes a unit OPEN in the editor (path→URL vs the
   documents-store snapshot) — so an open, unsaved buffer's meta is never
   overwritten with disk content. Warming restores the cross-unit precision that
   resident-only/budget trade away — completeness only, never correctness.
7. **Status** (`server/status.rs` `ddk/serverStatus` + the extension status bar):
   Ready / Analyzing <file> / Indexing N/M / Bootstrapping — what the server is
   doing, at a glance.

Net bound: moka AST cache ≤ ~256 MiB (RSS ~188) + arena disk text ≤ 64 MiB +
interner (distinct strings) ≈ a **~300 MiB bounded footprint**; no entry point
(open / save / navigate / index) can pull the full closure into RAM.

INVARIANTS THIS ADDS (breaking these is a wrong answer or a crash):
- No parse cascades (resident-only editor / budgeted Full).
- An OPEN editor buffer's cached meta is NEVER overwritten by the indexer.
- Arena content is cleared ONLY at safe checkpoints under the session lock (no
  live borrow) — the `disk_content_ref`/`virtual_content_ref` `unsafe` is sound
  by that serialization, not the type system alone.
- A query never maps a span onto text that differs from what the span was parsed
  against (staleness-guarded — see the stale-import guard in `code_location_to_lsp`).

## Module status (current, end of iteration 7)

| Module | Status |
|---|---|
| `token.rs` | logos lexer, all reserved words, directives opaque, trivia preserved, lex failures → spanned Error tokens |
| `meta.rs` | Span / FileId / CodeLocation; `FileId`/`Span`/`CodeLocation` serde (FileId ↔ path via global arena) |
| `globals.rs` | process-global interner + arena statics (AtomicPtr over leaked Box; reads lock-free, test-only `reset_for_tests` leaks-and-swaps); free `intern`/`intern_key`/`resolve`/`arena`; ONE `fold_identifier` (ordinal ASCII fold, non-ASCII byte-identical) = the single source of truth for every folded comparison key (it. 14, L9) |
| `source.rs` | arena: eager `load` / lazy `register`, BOM/UTF-16/ANSI decoding, stable refs across growth; RETAINS raw on-disk bytes per file (`raw_bytes(FileId)`) so stamps hash without a re-read (it. 14, L15); DISK content+raw are now CLEARABLE+re-readable (`Mutex<Option<Box<..>>>`), per-entry `last_access` LRU + `trim_disk_content(cap)` evicts coldest disk entries (never virtual) at a checkpoint; `content`/`raw_bytes`/`loaded_content`/`text` re-read a cleared disk file on demand (Task-19) |
| `context.rs` | ProjectContext from dproj (defines, search paths, namespaces, aliases, resolved config/platform names); CompilerProfile supplied by integrator (incl. `rtl_version: Option<f64>`, `None`=compiler_version); `ProjectContext.rtl_version` distinct from `compiler_version` (it. 14, L10); `Identifier` = newtype over Spur with transparent serde; `intern`/`intern_key`(→`fold_identifier`)/`is_defined`(→`fold_identifier`)/`resolve` delegate to `globals` |
| `parse_state.rs` | per-unit state: define/switch copies, conditional stack, include tracking, imports, own symbols/constants, own-type members (member→member-type, for scoped Declared #19), dependencies, usages, cycle taint, InterfaceLoader hook |
| `if_eval.rs` | full $IF/$ELSEIF evaluator, Kleene tri-state; `Value`/`Token` gained `UInt(u64)` with EXACT mixed-width Int/UInt comparison+arithmetic via i128 (it. 14, L6); Declared (own + imports via loader) incl. SCOPED `Declared(A.B[.C])` over own + imported type members with dependency recording (#19 closed), SizeOf (builtin table), const values (own + imports); `RTLVersion` reads `context.rtl_version` independently of `CompilerVersion` (L10); `resolve_qualified_type` skips unrelated unresolvable imports (task-4 review); dotted CONST values still Unknown (#30) |
| `token_cursor.rs` | single-pass directive cursor: all conditionals, $DEFINE/$UNDEF, $I includes (+%VAR% pseudo, probed-path errors), all switches, dead-branch tolerance, 2-token lookahead |
| `parser.rs` | headers, uses (+in-clauses), DEEP TYPE PARSE (full member structure), routine signatures, typed constants, structured vars, whole-unit usage recording; `parse_attributes` captures `[...]` at all six sites (#16); records own-type member→member-type pairs for scoped Declared (#19). **ERROR-TOLERANT interface parse (it. 13, #10):** per-item dispatch (`parse_one_interface_item`) wrapped by recovery — a per-declaration `ParseError` → diagnostic + `resync_to_declaration_boundary` (≥1-token progress guard, lexer-error-tolerant) + `recovered` flag; depth counter reset on recovery; directive-structure errors stay unrecoverable (`is_unrecoverable`) |
| `ast.rs` | full type AST: TypeExpression, members, visibility sections, variant parts, parameters, properties — Identifiers/spans only; `Attribute { name, arguments: Option<span>, location }` on declarations, fields, methods, properties, nested consts, parameters (#16) |
| `pipeline.rs` | AST → `UnitMeta` (owns the unit AST + include/dependency stamps + usages + cycle taint; interface surface derived lazily) + parse_and_cache (moves the `Unit` into the meta) |
| `unit_meta.rs` | `UnitMeta` wrapper: serde-serialized AST + stamps + deps + usages + cycle_taint; `#[serde(skip)]` `OnceCell` interface index derived from the AST (`interface()`, `watched_files`, `estimated_bytes`, `name()`). `build_interface` now also fills member `type_key`/`directives`/`visibility`/`attributes` and declaration `attributes` (derived, no format impact) |
| `unit_cache.rs` | moka cache (size-weighted, exact insert counter), `CacheEntry::Done(Arc<UnitMeta>)`, bincode persistence FORMAT v11 (v11 = `ConstantValue::UInt(u64)` large-unsigned constants, it. 14; v10 = `UnitMeta.recovered` never-persist gate, it. 13); save skips cycle_tainted OR recovered metas; ~~v8~~ (per-unit `UnitMeta` byte segments, transparent Identifier/FileId serde — no `SavedSymbol` mirror; v8 = attribute AST nodes), xxh3 validation, revalidation sweep. `MemberSymbol` gains type_key/directives/visibility/attributes; `InterfaceSymbol` gains attributes (all derived) |
| `unit_resolution.rs` | unit name → file: aliases → direct → namespace prefixes over search paths |
| `unit_loader.rs` | InterfaceLoader: cache→cycle-check→resolve→nested parse; chain-local cycle stacks; optional reverse-index registration |
| `layout.rs` | STAGE 2 DONE: `type_layout(type_expr, switches, platform, resolver) -> Option<Layout{size,alignment}>`. Records/objects (align/packed/$A/nested), enums ($Z floor, explicit values), subranges (int + char bounds, signedness), static arrays (multi-dim), ShortString (`n+1`), pointer-shaped types (class/interface/pointer/classref/dynarray/string/`reference to` = 1 ptr; method-ptr = 2), distinct unwrap. `LayoutResolver` trait resolves a `Reference` name → its `TypeExpression` (own interface types first via `own_type_expression`, then imports via loader with dependency recording; cycle → taint+None; depth ceiling 64). VARIANT records + SETS deferred → None (ledger #36/#37). ANY uncertainty (unknown field, unresolvable bound, deferred shape) → None — no wrong confident size. builtin sizes stage 1 unchanged (platform-dependent, LLP64-correct) |
| `cache_store.rs` | LocalAppData snapshots per (project, config, platform, compiler), atomic writes, corrupt≠missing |
| `watcher.rs` | deterministic ChangeCollector (quiescence + burst→FullSweep), ReverseDependencyIndex, notify glue |
| `driver.rs` | ProjectSession: open/parse_source_file/tick/save_now/shutdown. **LSP query API (it. 13):** `symbol_at`/`definition`(own + cross-unit via loader, member targets)/`references`(over cached units)/`completions`(member after `.` + top-level w/ imports + builtins)/`diagnostics`(parse + dfm unified). Owns a `ReferenceIndex` + per-unit parse-diagnostics map, both PURGED on invalidation (per-file) and REBUILT on full sweep — mirrors the dfm_links purge (no pointer into an evicted unit) |
| `query.rs` | LSP query result types (it. 13): `QueryTarget`{key,display,kind,location,owner_type}, `Completion`{display,key,kind,type_key,directives,visibility}, `CompletionKind`(Symbol/Member/Builtin), `UnifiedDiagnostic`{source,location:Option,dfm_offset:Option,message}, `DiagnosticSource`(Parse/Dfm), `TargetKind`(Declaration/Member/Usage) — owned, location-bearing, devkit maps to LSP types |
| `references.rs` | symbol→occurrences index (it. 13): folded key → [(unit, location)] built from each meta's interface declarations + members + recorded usages; `by_unit` reverse map for O(1) per-unit purge; `index_unit`/`purge_unit`/`occurrences`/`rebuild_from`. Over-approximating candidate set (usages not scope-resolved) — never MISSES a real occurrence, documented |
| `ddk.rs` | ddk CLI (compilers/projects JSON, null-tolerant), standard-source discovery |
| `dfm.rs` | text-DFM parser: component tree, values, handler candidates; binary DFM rejected |
| `dfm_link.rs` | pure DFM↔PAS linker `link_dfm(&UnitInterface, &DfmObject) -> DfmLinkResult`: component→field links (any matching `Field`; visibility NOT gated — IDE fields sit in the `Unspecified` default section), event-prop→method links, honest diagnostics (dangling/missing ONLY on ancestor-less forms; unresolved-possibly-inherited note when `has_ancestors`; form-class-not-found note; type-mismatch). Handler filter = event-name (`On…`) AND method-match BOTH required (a non-event ident whose value collides with a method name produces no link). Root object's own name skipped (it is the form, not a field) |

Tests: 212 portable (+1 ignored, reset-swap) + 6 behind `local-tests` (be.dproj
context, real unit, 110-unit stress test, full-src-tree stress, live ddk) — 218
with local-tests, all green after it. 14 (ledgered lows L6/L9/L10/L15/CONFIG +
resolve_qualified_type consistency / format v11).

## Edge-case ledger (open items require a plan, not a shrug)

| # | Edge case | Status / plan |
|---|---|---|
| 1 | Duplicate parse of same unit by concurrent tasks (cache miss race) | OPEN. Plan: async coalescing front door in devkit LSP layer (per-unit OnceCell + spawn_blocking); nested duplicates bounded by cache. Must be measured, not assumed harmless. |
| 2 | `{$I %VARIABLE%}` pseudo-includes (%DATE%, env vars) | CLOSED (it. 6): env vars splice their value as a string literal (via virtual one-token buffer); %DATE%/%TIME% splice a placeholder + diagnostic; unset vars splice `''` + diagnostic. Tested. |
| 3 | Qualified name split across include boundary | Location falls back to first part's span. Correctness of the *name* is unaffected. Plan: multi-file locations need a span-list representation when rename support lands. |
| 4 | Include resolution when file not found anywhere | CLOSED (it. 6): resolution returns every probed path; the Include error lists them all. Tested. |
| 5 | Unicode identifiers + case folding | RESOLVED (it. 14, L9) to the SAFE choice: ONE `globals::fold_identifier` = ordinal ASCII fold (a→A), non-ASCII bytes byte-identical — never a wrong match, never a track divergence. Behaviour-identical to the old `to_uppercase` for the ASCII identifiers that dominate real code. RE-LEDGERED sub-note: dcc's ACTUAL non-ASCII fold is still unverified (no targeted dcc compile done); the ordinal+byte-identical choice is deliberately conservative (would under-match a hypothetical dcc non-ASCII case-fold, never over-match). Revisit only if a real non-ASCII-identifier case surfaces. |
| 6 | `{$IF}` on unknown → policy AssumeFalse + diagnostic | By design (tri-state). Resolver completion (Declared/SizeOf) shrinks Unknown to: DCU-only units, layout gaps, broken source. SizeOf stage 2 (it. 12) closes the record/enum/subrange/array/string/pointer layout gap; remaining SizeOf Unknowns = deferred variant records (#36) + sets (#37) + unresolved-field/bound cases (correctly Unknown, never wrong). DCU-only plan: sidecar manifest loader. |
| 7 | Conditional `uses` entries relying on `in 'path'` inside dead branch | Handled by cursor (dead tokens swallowed) — covered by test. |
| 8 | dpr `uses Unit in 'path' {Form comment}` — form comments | Trivia, skipped by cursor. Covered by test (program_with_in_clauses). |
| 9 | `{$IFOPT X±}` with unknown switch letter | Returns false, frame still pushed (balanced). Matches compiler leniency? verify. |
| 10 | Lexer `Token::Error` / error-tolerant reparse | CLOSED for the INTERFACE section (it. 13). `parse_interface_declarations` is now error-tolerant: a `ParseError` while parsing ONE interface item (malformed declaration, active-region lexer `Token::Error`, unexpected token, recursion-limit) is caught, recorded as a diagnostic, and the parser RESYNCS to the next declaration boundary (top-level section keyword / `implementation` / `end` / EOF at interface nesting) — surviving declarations still populate the interface, the broken region contributes a diagnostic and NEVER a bogus symbol. Termination guaranteed: `resync_to_declaration_boundary` advances ≥1 token before scanning; a lexer error mid-resync skips the offending token (the lexer already consumed it). Depth counter reset to 0 on recovery (a `?`-unwound `RecursionLimit` leaves it inflated). A recovered parse is FLAGGED (`ParseOutcome.recovered` → `UnitMeta.recovered`, format v9→v10) and NEVER persisted as a clean interface (save skips it, same gate as `cycle_tainted`). DIRECTIVE-structure errors (unterminated `{$IFDEF}`, dangling `{$ELSE}`, uses-cycle) stay UNRECOVERABLE — the conditional skeleton is broken, so the unit fails as before (`is_unrecoverable`). DEFERRED (ledger #39): implementation-body / expression-level recovery — the interface section is the LSP-critical surface; the implementation body is scanned only for the usage index, where a lexer error still aborts. |
| 11 | `{$H +}` with space before the sign | CLOSED (it. 6): argument was already trimmed at directive dispatch — ledger entry was wrong about a fall-through; regression test added to keep it that way. |
| 12 | Persisted cache: two units with same name different paths (project variants) | Cache key = unit name only. Plan: key must include (name, resolved path) or per-project cache files. Partially mitigated: snapshots are already per (project, config, platform). |
| 13 | Watcher path identity for DELETED files | canonicalize impossible → folded raw path used; may miss per-file lookup for 8.3/symlink spellings. Full sweep + load-time hash validation catch it. Plan: keep; revisit if delete-heavy workflows show misses. |
| 14 | notify glue (`FileWatcher::start`) has no automated test | Core logic (collector/burst/index/sweep) fully unit-tested; OS-watcher glue is thin. Plan: integration test with real FS events + generous timeouts in the local-only test suite. |
| 15 | Reverse index is additive until `rebuild_from` | Re-parse with shrunken file set leaves stale mappings → over-invalidation only (safe direction). Driver rebuilds after full sweeps. |
| 16 | Attributes `[Foo(1)]` are skipped, not captured | CLOSED (it. 10): `parse_attributes` captures at all six call sites (decl/const/var/type-section/member/parameter) into a new `ast::Attribute { name, arguments: Option<span>, location }`. Name dual-track AS WRITTEN (no `Attribute`-suffix normalization); arguments a raw `(...)` span (never evaluated); balanced-bracket tolerant (nested `[]`/`(...)`); stacked `[A][B]` + comma `[A, B]` both flatten to the Vec. Format v7→v8 (AST change). Params capture before OR after the modifier. Derived `MemberSymbol`/`InterfaceSymbol` expose attribute keys. Round-trip + capture tests. |
| 17 | `class` block detection is token-heuristic | Rules: forward/`of`/constraint-closers → no block; first-tail-token or after `=` → block; member markers → no block; else block (wrong guess surfaces as unbalanced-'end' error, never silent). Deep type parse in slice 4 replaces the heuristic entirely. |
| 18 | Generic param list: `>=` token fusion (`TBox<T>=class`) | Handled explicitly (fused Gt+Eq). `>>` is two Gt tokens, no issue. `<T: record>` constraint keywords excluded from block detection via next-token closers. |
| 19 | `Declared(A.B)` dotted names | CLOSED (it. 10; INHERITED-member correctness fix it. 11): `StateResolver::is_declared_scoped` splits `A.B[.C…]`, resolves the first segment to a type — OWN interface type first (state tracks per-type `member → member-type` pairs plus a `can_inherit` flag, recorded BEFORE trailing directives so the cursor sees them), then imported units in reverse uses order (via the loader), also handling `Unit.Type[.Member]`. Walks members; a member's simple `type_key` descends to the next (same-unit/own) type. Records EVERY consulted import as a dependency (same staleness discipline as flat). **Not-directly-declared rule (it. 11 fix; alias/distinct/classref extension it. 12):** the member walk only flattens a type's DIRECT declarations, so a member absent from them must NOT be a confident false whenever the type's real member surface is larger than what we flatten. Both walks (`own_scoped_declared` and `walk_type_members`) degrade a not-directly-found member to Unknown when `type_can_inherit` holds. `type_can_inherit` is true for: any class (implicit `TObject`) or interface (implicit `IInterface`), or any type with explicit/cross-unit ancestors; AND any REDIRECT shape whose members live elsewhere — a bare `Reference` alias (`TFoo = TBar`, which inherits TBar's ENTIRE surface incl. its DIRECT members), a `Distinct` type (`T = type Integer`), or a `ClassReference` (`class of T`). Flag surfaced as `OwnTypeMembers::can_inherit` / `InterfaceSymbol::has_ancestors`, both derived from the AST via `type_can_inherit`. A confident false is kept ONLY for a genuinely self-contained, ancestor-less shape (record/enum/set/subrange/pointer/routine-type/…) whose member is genuinely absent — these carry NO unseen member space. This closes the silent-corruption bug where `Declared(TChild.BaseMember)` returned `Some(false)` for an inherited member, AND the it. 12 sibling where `Declared(TAlias.Member)` returned a confident false for an alias/distinct/classref target (both under Kleene `not false` silently kept a dead branch with no diagnostic). NOTE: for an alias/distinct/classref the result is currently only Unknown (never confident True), because we do not yet resolve the redirect target to walk its members — that PRECISION follow-up is ledger #33. Cross-unit member-type descent (a member whose type lives in ANOTHER unit) is still NOT followed → Unknown (sub-limitation #31; dominant own/same-unit cases covered). Tests: own-member true, imported-member true + dep recorded, ancestor-less `TRec.Nope` false, class/interface inherited member NOT false (own + imported walks, with a diagnostic on the Unknown path), alias/distinct member NOT false (own + imported, direct-target AND inherited, with a diagnostic), unresolved first segment Unknown, nested 3-segment positive + negative. |
| 20 | Interface uses-cycles (invalid Delphi, F2047) | Detected via loader chain stacks (unit keys from `begin_unit`/`end_unit` + active FILES for pre-header directive recursion). Parse degrades to Unknown; result marked `cycle_tainted` — cached in-session, NEVER persisted. Tested both directions of the cycle. |
| 21 | Artifact hash for virtual buffers (unsaved editor content) | No disk bytes → decoded content hashed; never matches a disk read → dropped as stale on next snapshot load. Intended: unsaved state must not masquerade as on-disk state. |
| 22 | moka `entry_count` eventually consistent | Never use for decisions. Exact `UnitCache::insert_count` (AtomicU64) exists for dirty tracking; tests assert via `get()`. |
| 23 | Binary DFM detection post-decoding | Raw 0xFF can't exist in &str; ANSI decoding maps it to U+00FF — detection checks the char, not the byte. Binary DFMs are rejected with a distinct error, never misparsed. Conversion path (convert.exe) is the devkit's call. |
| 24 | DFM string continuation vs. list items | Bare newline between string parts = NOT concatenation (starts next list item); only adjacency or `+` concatenates. Encoded in parser + test. |
| 26 | Global `reset_for_tests` vs parallel test runner | A mid-run swap invalidates `Spur`s a concurrent test interned in the old generation → its `resolve` would panic. RESOLVED by making the suite reset-independent (idempotent interning: serialize→deserialize reproduces the same Spur without emptying the global); `reset_for_tests` is provided per spec (leak-and-swap, no UAF) but NOT called in-tree. Any future test that truly needs an empty global must run serially (`--test-threads=1`), documented on `reset_for_tests`. |
| 27 | v7 persistence granularity | Snapshot = version tag + one bincoded `UnitMeta` byte segment per unit. Per-segment isolation is what keeps load panic-free (M2): a corrupt/unregisterable unit is dropped `unreadable` without poisoning the rest. `Identifier`/`FileId` serialize transparently as strings/paths through the globals (no `SavedSymbol` mirror). A `FileId` whose path cannot re-`register` (deleted file, virtual buffer) is a clean serde error → `unreadable` (#21/#25). Bincode is not self-describing, so ANY AST-node change bumps the format → one cold reparse (accepted). |
| 28 | Save-side `FileId::serialize` on a foreign/out-of-range id | CLOSED (it. 9): `FileId::serialize` used `arena().path()` which `.expect()`ed, so ONE id the process-global arena never issued (a non-global `&SourceArena` reaching the `pub parse_and_cache` → `save`) panicked the whole `UnitCache::save`, breaking M2. Now `SourceArena::try_path` is non-panicking and `serialize` returns a serde error for an unregistered id, mirroring the deserialize side; `save` skips the un-serializable meta in isolation (it re-parses on demand) rather than aborting. Regression: `foreign_fileid_does_not_panic_on_save`. |
| 29 | moka weigher must not force the derived interface index | CLOSED (it. 9): the weigher runs on the insert hot path under moka's internal lock; `estimated_bytes` used to call `interface()`, building the full member surface and mutating the `OnceCell` under that lock (review NIT). Switched to a purely STRUCTURAL estimate (`shallow_member_count` over the AST's interface declarations) — no `interface()`, no `OnceCell` write. Precision is irrelevant for eviction pressure; the derived index is still built lazily on the first real query. |
| 30 | Dotted CONSTANT value in `{$IF}` (`TFoo.MaxItems` as a value) | OPEN (deferred, it. 10). Scoped `Declared(A.B)` (#19) is done, but a dotted name used as a VALUE still returns Unknown in `const_value`. Plan: reuse the #19 scoped walk to reach the member, then read its captured `ConstantValue` — needs the member index to carry the member's constant value (nested/class consts) which it does not yet. Small, isolated follow-up. |
| 31 | Scoped `Declared` cross-unit member-type descent | OPEN (sub-limitation of #19, it. 10). `Declared(A.B.C)` where `B`'s type lives in a DIFFERENT unit than `A` is not followed → Unknown (never wrong, just conservative). Own-type and same-unit-imported chains ARE followed. Plan: when a member's `type_key` is not in the current interface, resolve it as a type across the unit's own imports (recording the extra dependency) before walking `C`. Deferred: the dominant real-world shapes (own type, same-unit) are covered; cross-unit nested-member Declared is rare. |
| 32 | Attribute with no declaration to attach to (`[Foo] implementation`, or `[Foo]` before EOF) | CLOSED (it. 11): a section parser that took leading attributes but then met a section boundary/`implementation` used to DROP them silently (also for `[Foo]` before a *different* section keyword). Now `restore_pending_attributes` returns them to the pending buffer so the interface loop re-attaches them to the next section's first declaration (`[Foo] const X = 1;` now works across the boundary), and `report_dropped_pending_attributes` emits a per-group diagnostic ("no declaration to attach to; ignored (invalid Delphi)") when they reach `implementation` with nowhere to land. `[Foo]` before EOF still hits the interface loop's hard `Unexpected` error (loud, not silent). Discarding truly-dangling attributes is correct Delphi behaviour; only the silence was the defect. Test: `attribute_before_implementation_is_dropped_with_diagnostic`. |
| 33 | Scoped `Declared(Alias.Member)` PRECISION: resolve the redirect target and walk ITS members | OPEN (deferred, it. 12). The it. 12 fix guarantees CORRECTNESS: `type_can_inherit` now returns true for a bare `Reference` alias (`TFoo = TBar`), a `Distinct` (`T = type Integer`), and a `ClassReference` (`class of T`), so a member absent from the alias's own (empty) direct declarations degrades to Unknown, never a confident false. But it can only ever return Unknown for these shapes — it cannot yet return a confident TRUE for `Declared(TAlias.RealMemberOfTarget)`, because we do not resolve the alias's Reference target to a type and walk THAT type's member surface. Plan: in both walks, when the resolved type is an alias/distinct/classref, extract the target's simple key (the `Reference`/`ClassReference` name, or the `Distinct` inner `Reference`), resolve it as a type — OWN interface type first, else across the unit's imports in reverse uses order (recording the extra dependency, same staleness discipline as #19/#31) — then walk the remaining segments against the target's members (which may itself chain through another alias or inherit, so bound the redirect depth to avoid alias cycles). Then `Declared(TAlias.BaseField)` → confident True and a true miss on a fully-resolved target → confident False. Cross-unit target resolution shares the mechanics of #31. Deferred: correctness (Unknown-not-false) is already guaranteed; this only sharpens Unknown→True/False for the alias case. Tests to add on close: `Declared(TAlias.DirectMemberOfTarget)` → True (own + imported), `Declared(TAlias.GenuinelyAbsent)` on an ancestor-less target → False. |
| 34 | DFM↔PAS link results not yet served as LSP go-to-definition / rename | OPEN (deferred to task 5, it. 11). `dfm_link::link_dfm` produces `ComponentLink`/`HandlerLink`/`DfmDiagnostic` and `ProjectSession` STORES them per unit (`dfm_links(unit_key)`), each link carrying both endpoints' locations (dfm byte offset + pas `CodeLocation`). What's missing is the request-handler translation into LSP `textDocument/definition` / `rename` / `publishDiagnostics` responses — that layer lives in delphi-devkit, not this parser library (SESSION decision: "LSP server is NOT built here"). Plan: task 5 exposes a stable query API over the stored result (position→link lookup both directions, rename edit-set spanning the dfm + pas) and the devkit consumes it. No correctness risk deferred: the links + honest diagnostics are already computed and tested; only the transport/format is pending. |
| 35 | DFM linker: resolve cross-unit base-form members (turn possibly-inherited NOTE → confident link/miss) | OPEN (deferred, it. 11). When the form class has ancestors (`TForm1 = class(TBaseForm)`), a component/handler absent from THIS unit's members is emitted as an `UnresolvedComponentPossiblyInherited` / `UnresolvedHandlerPossiblyInherited` NOTE (never a false "missing" — correctness is guaranteed). But we do not yet resolve `TBaseForm` across the unit's imports and walk ITS published fields / methods, so we cannot upgrade the note to a confident link (base component) or a confident miss. Plan: resolve each ancestor as a type — own interface first, then imported units in reverse uses order via the loader (recording the dependency, same staleness discipline as #19/#31/#33), bound the ancestor-walk depth against cycles — then re-run the field/method match against the flattened base surface before deciding note-vs-link-vs-miss. Shares mechanics with #31 (cross-unit member-type descent) and #33 (redirect-target walk). Deferred: correctness (Unknown-not-false) already holds; this sharpens NOTE→link/miss for inherited members. Tests on close: base-declared component → confident link across units; a component absent from BOTH child and fully-resolved base → confident dangling. |

| 36 | Variant-record (`case … of`) SizeOf | OPEN (deferred, it. 12). A record with a `variant_part` returns `None` from `record_layout` (never an approximated size — a wrong size flips `{$IF}`). The ABI: the variant region is placed after the fixed fields at the record's alignment; its size = `max` over arms of each arm's field-group layout (arms overlap); record size = fixed + variant, padded to record alignment. NOT YET IMPLEMENTED because the exact arm-alignment interaction (does each arm re-align from the variant region base? nested `case`? selector field inclusion when named vs unnamed) needs verification against dcc before I assert numbers. Plan: lay out the fixed prefix, then for each arm lay out its member group as a sub-record (respecting `{$A}`/packed), take the max size AND max alignment across arms, place the region at `align_up(fixed_size, region_alignment)`, final size `= align_up(region_offset + region_size, record_alignment)`. Add Win32+Win64 tests with KNOWN dcc sizes (e.g. `record Tag: Byte; case Integer of 0:(i: Integer); 1:(b: array[0..3] of Byte) end` = 8) and a confidence test before flipping to `Some`. Tested-deferred: `variant_record_is_deferred_to_unknown`. |
| 37 | `set of T` SizeOf | OPEN (deferred, it. 12). `SetOf` returns `None`. The Delphi rule: a set's size depends on the base type's ordinal count `n` (number of distinct values). For `n ≤ 8` → 1 byte; otherwise the byte count is `((hi div 8) - (lo div 8) + 1)` bytes based on the ordinal RANGE, rounded, capped at 32 bytes (256 bits). The subtlety is the lo/hi-byte-boundary rule (a set over a subrange `3..10` is NOT simply `ceil((10-3+1)/8)` — dcc uses `hi div 8 - lo div 8 + 1`), and the base-type cardinality must be resolved (enum count / subrange span / Byte=256→32 bytes / Char=256→32 bytes). NOT IMPLEMENTED because getting the byte-boundary rule wrong yields a wrong confident size. Plan: resolve the base type to (lo_ord, hi_ord) — builtin (Byte→0..255, Boolean→0..1, AnsiChar→0..255), enum (0..count-1, honoring explicit values via the enum machinery already built), or subrange (its evaluated bounds) — then `size = if hi <= 7 then 1 else (hi div 8 - lo div 8 + 1)`, capped ≤ 32; add Win32/Win64 KNOWN-size tests (`set of Byte`=32, `set of Boolean`=1, `set of 0..15`=2, `set of 'a'..'z'`) before flipping to `Some`. Tested-deferred: `set_type_is_deferred_to_unknown`. |
| 39 | Finer-grained error recovery (implementation body / expression level) | OPEN (deferred, it. 13). Declaration-level resync closed the LSP-critical INTERFACE section (#10). The IMPLEMENTATION section is currently only token-scanned for the usage index (`collect_implementation_usages`), where a lexer `Token::Error` still aborts via `?` (no resync) and an active-region lex failure fails the unit. Expression-level recovery (a half-typed initializer / bound span mid-declaration) is likewise not sub-declaration granular — a broken expression drops the WHOLE enclosing declaration, not just the expression. Plan: (a) make `collect_implementation_usages` tolerant (skip the offending token + diagnostic, continue the scan — the usage index is already an over-approximating candidate set, so dropping a token there is safe); (b) for finer expression recovery, resync inside a declaration to the next `;`/member boundary rather than dropping the whole declaration. Both are additive precision; correctness holds today (a dropped region is flagged + never a bogus symbol), this only widens how much survives a broken edit. |
| 38 | Static array over a NAMED index type (`array[Boolean] of T`, `array[TColor] of T`) | OPEN (deferred, it. 12). `array_element_count` handles explicit `lo..hi` ranges (incl. multi-dim) but a dimension that is a named ordinal type (not a literal range) → `None` (the whole array Unknown), because computing the element count needs the index type's cardinality (Boolean=2, an enum's member count, a subrange's span) which shares the #37 base-type-cardinality resolver. Plan: when a dimension is not a `lo..hi` range, resolve it as a named type and reuse the (lo_ord, hi_ord) resolver from #37 to get `hi - lo + 1`. Never wrong today (Unknown, not a guess); this only widens coverage. |
| 41 | Bare member-USAGE targets carry no owner qualification | OPEN (deferred, Task 9 review finding #5). A member accessed in a body (`Foo.Bar`, cursor on `Bar`) resolves through the usage index as a `Usage` whose kind carries NO owner type (`owner_type: None`) — unlike a member DECLARATION, which knows its owning type. So `definition`/`hover` on such a bare member usage look `Bar` up as a TOP-LEVEL symbol and may jump to an unrelated imported top-level `Bar` rather than `Foo`'s member `Bar` (an over-approximation inherited from Task 5's usage index, NOT introduced by Task 9 — Task 9's declaration-site resolution is correct). Correctness note: this is a possible WRONG jump for a specific member-access usage shape; it is tracked here rather than silently accepted. Plan: thread the owner through the usage index for member-access occurrences — when the implementation scan records an identifier that is the member half of a `Owner.Member` dot access, capture the owner expression's resolved type (or at least the syntactic owner key) alongside the usage, so `symbol_at` can return `owner_type: Some(owner)` for that occurrence and `definition`/`hover` resolve the member against the owner's type (same scoped machinery as #19) instead of as a top-level symbol. Requires the implementation walk to group dotted accesses (shares mechanics with #40's qualified-header capture). Deferred: the dominant declaration/type/free-symbol targets resolve correctly today; this sharpens the member-USAGE case. Tests on close: `Foo.Bar` usage with a top-level `Bar` also in scope resolves to `Foo`'s member `Bar`, not the top-level one. |
| 42 | `textDocument/rename` needs scope-resolved bindings (over-approx over-renames; decl-only under-renames) | OPEN (deferred, Task 10 Deliverable B). A rename must be BOTH complete (rewrite EVERY real reference of the symbol) AND correct (rewrite NOTHING else). The only occurrence set available is `references(key)` — the SAME scope-unresolved usage index that `textDocument/references` serves — which is an OVER-APPROXIMATING candidate set (documented on `references.rs`): it is the union of (a) the symbol's real references and (b) unrelated occurrences that merely share the folded name (a local `Result`, a same-named member of another type, a same-named top-level symbol in another cached unit). The dilemma is unresolvable without scope resolution: **(i)** renaming the WHOLE candidate set rewrites the (b) occurrences too → a DESTRUCTIVE wrong edit (silently corrupts an unrelated identifier); **(ii)** renaming only the provably-bound subset (the declaration + resolved INTERFACE references) leaves the IMPLEMENTATION-section uses — recorded only as flat, owner-less, scope-unresolved `Usage`s (#39/#40/#41) — un-renamed → dangling/broken code = an INCOMPLETE edit, also wrong. No provable safety GATE bridges this either: to prove a given occurrence binds to THIS symbol (not a shadowing local) needs the very scope resolution that is missing; even a "rename only if the name is globally unique and un-shadowed" gate cannot be discharged, because the usage index does not record whether an occurrence is a local binding, so "no shadowing local exists anywhere" is itself unprovable here. Under the never-wrong rule (which binds hardest for a DESTRUCTIVE op — rarity of collisions is not a justification), the honest call is to DEFER: `rename_provider` is NOT advertised and no `prepareRename`/`rename` handler is shipped, so the editor offers no rename rather than a sometimes-wrong one. `references` (read-only, user-reviewed) is acceptable to ship over the same set precisely because it is non-destructive. Plan (the prerequisite is scope resolution / a symbol table): build scope-resolved bindings so each occurrence carries its resolved declaration identity (distinguishing a real reference from a same-named local/member/other-unit symbol). That same symbol table (a) sharpens `references` from a candidate set to an exact set, (b) supplies owner qualification for member-usage occurrences (closes #41), and (c) resolves interface↔implementation method identity (relates to #40). Once bindings exist, a rename can rewrite EXACTLY the resolved-identity occurrences (complete) and NOTHING else (correct), with `prepareRename` rejecting any position whose identity is not established (bare/ambiguous usages). Ties to #41 (owner-qualified usages) and #40 (impl-section method identity) as shared prerequisites. Tests on close: rename rewrites every real reference across units AND leaves a same-named local / same-named other-unit symbol untouched; `prepareRename` on an unresolved bare usage → null. |
| 40 | Interface ↔ implementation method jump (LSP go-to on `procedure TFoo.Bar;` impl headers) | OPEN (deferred, Task 9 Deliverable D). Delphi declares a method in the interface (`procedure Bar;` inside `TFoo`) and implements it in the implementation section (`procedure TFoo.Bar; begin … end;`). A useful LSP jump goes BOTH ways between the two sites. Task 9's `definition`/`hover` resolve the INTERFACE declaration only; the implementation-section header is NOT structurally captured, so the jump has no data and (per never-wrong) was NOT faked. Current state: the implementation section is only FLAT token-scanned by `parser::collect_implementation_usages`, which records each identifier as a `Usage` (symbol + span) with NO qualified-name grouping — it cannot tell `TFoo.Bar` (a method-implementation header) from any other `TFoo` / `Bar` occurrence, has no owner↔member association, and does not mark a span as "the implementation site of method M on type T". So `definition` cannot return the impl body site, and there is no interface→impl or impl→interface mapping. Plan: (a) in the implementation walk, recognize a method-implementation HEADER — a top-level (nesting depth 0) `procedure|function|constructor|destructor|operator` followed by a `QualifiedName` containing a dot (`Owner.Method`, possibly generic `Owner<T>.Method`) up to the `;`/directive/`begin` — and capture `{ owner_key, method_key, header_location }` into a new per-unit `Vec<ImplementationMethod>` on `UnitMeta` (bump the persistence format version; the field serializes like other spans/keys). Skip nested locals and non-qualified routine headers (unit-level procedures without an owner). (b) Expose a query `implementation_of(unit_key, owner_key, method_key) -> Option<CodeLocation>` and its inverse, resolving the owner cross-unit via the SAME loader machinery as `definition`. (c) Fold BOTH sites into `definition` (return interface decl + impl header) OR add a dedicated `textDocument/implementation` request in the server; advertise `implementation_provider` only once backed. Correctness discipline: capture only a header whose qualified name unambiguously parses to `Owner.Method`; anything malformed contributes nothing (never a bogus location), consistent with #39's deferred implementation-body recovery (this capture must itself be resync-tolerant so a broken body does not drop the header already captured). Tests on close: `procedure TFoo.Bar; begin end;` → impl location found and distinct from the interface decl; interface→impl and impl→interface both resolve; a same-named free procedure is NOT mistaken for a method impl; cross-unit owner resolves. Deferred: the LSP-critical navigation (interface declaration go-to + hover) already ships in Task 9; interface↔impl is an additive second jump with no correctness risk while absent. |

## Iteration 1 work log (completed 2026-07-16)

- [x] SESSION.md created, decisions + ledger recorded
- [x] Dual-track interning: `intern` (display) vs `intern_key` (folded) —
      defines, aliases, `QualifiedName.key`; regression tests for
      `{$DEFINE UseFoo}`/`{$IFDEF USEFOO}` and uses-key folding
- [x] AST String removal (InClause.path → display-interned Identifier)
- [x] `cache_store.rs`: snapshot in %LOCALAPPDATA%\delphi-devkit\parser-cache,
      identity = (canonical folded dproj path, config, platform, compiler
      version) → xxh3 file name; atomic write (temp + rename), corrupt ≠
      missing, `discard()` recovery. 47 tests green.
- Commits: a46d003 (dual-track), a81f5ba (cache store)

## Iteration 2 work log (completed 2026-07-16)

- [x] Cache format v2: `UnitArtifact.includes: Vec<SourceStamp>` — `.inc`
      edits now stale their including units; validated at load + in sweeps;
      `watched_files()` = own source + includes + dependency sources.
- [x] `watcher.rs`:
      - `ChangeCollector` — deterministic debounce/burst state machine
        (time injected, fully unit-tested). Quiescence 500ms default;
        >64 distinct files pending = burst → `FullSweep` instead of
        per-file thrash. Ongoing checkout keeps deferring the flush
        (tested with rolling event sequence).
      - `ReverseDependencyIndex` path→units (folded canonical keys),
        `apply_invalidation`, `UnitCache::revalidate()` hash sweep.
      - `FileWatcher` notify glue (recursive watch, driver polls).
- Extension filter: pas/inc/dpr/dpk/dproj/dfm, case-insensitive.
- 55 tests green. Commit a56abdb.

## Iteration 3 work log (completed 2026-07-17)

- [x] `ddk.rs` — ddk CLI integration: `compiler list --json` /
      `project list --json` parsed into typed structs (parsing pure +
      testable, live CLI behind feature). `ver_define()` (VER<cv*10> formula),
      `standard_source_directories()` walks `<install>\source` for dirs
      containing .pas (explicit error when source root missing).
      LIVE-TEST FINDING: `Project.dproj` can be null (legacy exe-only
      registrations) → `Option<PathBuf>`; also `exe` null for packages;
      top-level has extra keys (group_project, active_project_id) — ignored.
- [x] `local-tests` cargo feature — machine-bound tests (C:\Delphi paths,
      live ddk) excluded from plain `cargo test`; run via
      `cargo test --features local-tests`. be_dproj_context moved behind it.
- [x] `driver.rs` — `ProjectSession`: open = context swap (context from
      dproj, snapshot load + hash validation, reverse-index rebuild, watcher
      start over project dir + existing search paths; missing search paths →
      notes). `tick(now)` = watcher poll → apply plan (sweep → index rebuild)
      → autosave (dirty + interval). `shutdown()` = final save; Drop saves
      nothing by design (unreportable failures). `mark_dirty()` hook for the
      parse pipeline. ProjectContext gained `configuration`/`platform_name`
      (resolved names — needed for snapshot identity).
- 62 tests green (65 with local-tests). Commits 34f4dc8, 1c51b0a.

## Iteration 4 work log (completed 2026-07-17)

- [x] Grammar slice 2 — shallow interface declarations: `type` (incl.
      generics with constraints, `>=` fusion, forward classes, class refs,
      helpers, interfaces + GUIDs, variant/nested records, procedure types
      with `of object`/conventions), `const`/`resourcestring` (typed record/
      array constants with inner `;`), `var`/`threadvar` (multi-name, inline
      anonymous records, `array of record`), routine headers (params,
      defaults, trailing directive chains), attribute skipping, `exports`.
- [x] `TokenCursor::peek_second` (2-token lookahead) — required to keep
      `const Platform = 2;` from being eaten as a portability directive.
- [x] `class`-block heuristic with previous-token context (ledger #17);
      found + fixed two real edge cases via tests: `= class procedure ...`
      (first member unqualified) and `TPair<K; V: record>` (semicolon inside
      generic params).
- [x] Local test parses real production unit (beDBVersion.pas via be.dproj):
      2 declarations extracted correctly.
- 70 tests green (73 with local-tests). Commit 1c9cbbd.

## Iteration 5 work log (completed 2026-07-17, same day continuation)

User feedback applied: iterations now pull points until the budget is spent,
not one-and-done.

- [x] `UnitInterface` symbols (display+key+kind+location), persistence
      format v3 (symbols saved with per-unit path table).
- [x] `pipeline.rs` — AST → `UnitArtifact` (symbol list, include stamps from
      the cursor's seen-includes, dependency stamps from consulted imports,
      cycle-taint flag) + `parse_and_cache`.
- [x] `unit_resolution.rs` — alias substitution → direct name → namespace
      prefixes, probed across search paths in order; effective name returned
      (SysUtils → System.SysUtils) for cache identity.
- [x] `unit_loader.rs` — `InterfaceLoader` impl: cache hit → cycle check →
      resolve → nested parse wired to itself. `Rc::new_cyclic` self-reference;
      chain-local cycle stacks (unit keys via parser-driven
      `begin_unit`/`end_unit` + active files for pre-header recursion).
- [x] `StateResolver::is_declared` — own interface keys, then imports in
      reverse uses order via loader; consulted units recorded as artifact
      dependencies; Cycle → taint + Unknown; unresolvable import → Unknown,
      never confident-false.
- [x] THE LAZY-IMPORT LOOP FROM THE ORIGINAL DESIGN DISCUSSION IS CLOSED:
      `{$IF Declared(Alpha)}` in unit B forces UnitA's interface parse
      mid-directive, takes the right branch, records the dependency —
      end-to-end test coverage incl. cycles, missing imports, cache hits.
- 81 tests green (85 with local-tests). Commit bbffcf3.
- [x] Driver wiring (same turn): `ProjectSession::parse_source_file` — full
      pipeline entry (loader with reverse-index registration for NESTED
      units, dirty tracking); `SessionOptions.standard_source_paths`
      appended to search paths at open (RTL resolution ready — devkit feeds
      `ddk::standard_source_directories`). EDGE CASE FOUND: moka
      `entry_count` is eventually consistent → exact `insert_count`
      AtomicU64 added; dirty tracking and tests now use it (ledger #22).
      Session test: change to UnitA.pas invalidates A AND its importer B.
- 82 tests green (86 with local-tests). Commit 07de7a1.

## Iteration 6 work log (2026-07-17, continuation on user instruction)

- [x] `layout.rs` — builtin size table (stage 1 of the layout engine):
      platform-dependent (Pointer/NativeInt 4|8, Extended 10|8, Variant
      16|24, LLP64: LongInt stays 4 on Win64), strings as references,
      ShortString 256, Real48 6. `SizeOf(Pointer) = 4` etc. now evaluate —
      covers the dominant real-world `{$IF SizeOf(...)}` patterns.
- [x] Constant values across units: `ConstantValue` (Int/Float/Bool/Str) on
      `InterfaceSymbol` (format v4), captured for single-literal
      initializers incl. negatives, `$FF`/`%bin`/`&oct`, `_` separators,
      `#13`/`#$0D` char literals. `StateResolver::const_value`: own consts →
      imports (reverse, shadowing stops the walk even when the value is
      uncapturable), dependency recording, cycle taint. `Computed = 1 + 2`
      honestly stays Unknown.
- [x] `StateResolver::size_of` wired to the builtin table; user types stay
      Unknown until record layout (stage 2, needs deep type parse).
- [x] Implementation usage index skeleton: identifier occurrences of the
      implementation section recorded as `Usage { key, location }` into the
      artifact (over-approximating candidate set — safe direction for
      find-references; scope-aware resolution refines later).
- 88 tests green (92 with local-tests). Commits 00157c3, 8467d6d.
- [x] `dfm.rs` — text-DFM parser: object/inherited/inline tree, dotted
      property paths, ints ($hex, negative), floats, multi-part strings
      (`''` escape, `#13`/`#$0D` codes, `+` line wrap — bare newline is NOT
      concatenation: in `(...)` lists it separates items), sets, binary
      blobs (skipped, marked), collections `<item…end>`, string lists.
      `identifier_properties()` = handler-candidate pass for pas↔dfm links
      (filtering enum values vs. methods happens against the .pas side).
      Binary DFMs (TPF0) rejected distinctly — note: after ANSI decoding the
      0xFF marker byte is U+00FF, not a raw byte (ledger #23).
- 92 tests green (96 with local-tests). Commit 156ef95.
- [x] Ledger #2 (pseudo-includes: env var → spliced string literal via
      virtual buffer, %DATE%/%TIME% → placeholder + diagnostic), #4 (include
      error lists all probed paths), #11 (was already handled — regression
      test added) all CLOSED. Commit fabecc8.
- [x] Usage index extended to the interface section: identifiers inside
      declaration bodies (field types, base classes, parameter types) are
      recorded — the whole unit is now covered for find-references.
      Commit baaffec.
- Final for this stretch: 95 tests green (99 with local-tests).

## Iteration 7 work log (2026-07-17)

**Deep type parse landed.** The declaration skipper is replaced by a
structured type parser for the whole interface section.

- [x] Type AST (`ast.rs`): TypeExpression (Reference+type args, Pointer,
      ClassReference, Array/ArrayOfConst, SetOf, File, Enumeration,
      Subrange/SizedString as spans, Routine/AnonymousMethod, Record, Class,
      Interface, Forward*, Distinct), members (fields/methods/properties/
      nested types/nested consts), visibility sections (strict), variant
      parts (nested), parameters with modifiers/defaults, property
      read/write targets (dfm-link source), method directive lists.
- [x] Parser: full member parsing for class/record/object/interface incl.
      `class var/function/property/operator`, helpers (`class helper (Base)
      for T`), GUIDs, ancestor-only shorthand `class(TBase);`,
      `reference to function`, distinct types, `array of const`.
      Directive-vs-member disambiguation via peek2 (`Message: string;` field
      vs `message WM_X;` directive — `:` decides).
- [x] Expressions stay SPANS (bounds, defaults, enum values, specifier
      values) — const-expression evaluation is a later stage. Identifier
      usages recorded inside all spans and type references.
- [x] Var/const/routine sections structured too: typed constants parse
      their type, variables get initializer/`absolute` spans, routine
      headers produce `TypeExpression::Routine` signatures (inline-hints
      data source).
- [x] STRESS TEST (local): all 110 units under src\core parse structured.
      Two real-world gaps found and fixed:
      1. `array of const` (open varargs) — new AST node.
      2. Compile-breaker prose in dead branches (`{$IFDEF X} Error: do not
         use! {$ENDIF}`) — lex failures now become Error tokens that the
         dead-branch filter swallows; ACTIVE-branch lex errors still fail
         (both directions tested). Ledger #10 partially resolved.
- 100 tests green (104 with local-tests incl. stress test). Commit 4b171fc.
- Ledger #17 (class-block heuristic) CLOSED — heuristic deleted with the
  skipper for types. #16 attributes: still skipped (capture = next step).
- [x] Member symbols in artifacts (persistence FORMAT v5): every type
      symbol carries its flattened members (fields incl. variant arms,
      methods, properties WITH read/write target keys, nested types/consts)
      → completion + dfm-linker input; roundtrip-tested. FOUND & FIXED:
      `UnitCache::save` now calls moka `run_pending_tasks()` first —
      without it, freshly inserted entries could be silently MISSING from
      snapshots (ledger #22 class, autosave data loss). Commit 17a48a8.
- Ledger #25 (new): artifacts from virtual buffers carry virtual-file
  locations in symbols/usages → `register` fails on load → dropped as
  unreadable. Consistent with #21 (unsaved state never persists), now
  also understood for locations, not just hashes.

## Iteration 8 work log (2026-07-17) — DEEP REVIEW + FIX PASS

Five-agent deep review (4 static reviewers by cluster + 1 empirical stress
harness over the whole `C:\Delphi\VSS\Intern\src` tree, 468 files). Findings
consolidated into `REVIEW.md` (43 items: 11 HIGH, 13 MEDIUM, 19 LOW). Worked as
writer→review→test rounds; every commit kept `cargo test` green. Subagents died
mid-pass (org spend limit) — remaining rounds done inline. Cache format v5 → v6.

- ALL 11 HIGH fixed: H1 `&`-escaped identifiers stripped before interning
  (symbol identity); H2 `{$I}` resolves via `DCC_IncludePath` (+ name
  normalization); H3 `external`/`name`/`index`/`forward` routine directives;
  H4 generic ancestor/helper-target `<...>`; H5 recursion-depth guard
  (`MAX_PARSE_DEPTH=64`, degrades to `Err`, no stack overflow); H6 procedural-
  type field trailing calling convention; H7 method resolution clause
  (`procedure IFoo.M = Impl;`); H8 `const_value` records every walked import
  (stale-cache fix); H9 dependency stamps now carry the dep's include stamps
  (`Dependency`/`SavedDependency` + `watched_files` + load validation); H10
  `path_key` strips the `\\?\` verbatim prefix (Windows DELETE invalidation);
  H11 per-file tick drops `Failed` entries (fixed include clears the failure).
- ALL 13 MEDIUM fixed: M1 weigher counts symbols/members (OOM at scale); M2
  panic-free snapshot decode (`file_index` bounds-checked → unreadable); M3
  `begin_unit`/`end_unit` balanced inside `parse_unit` (no false cycle on parse
  failure); M4 UTF-16BE decode + odd-byte guard; M5 `%bin`/`&oct`/`#char`
  literals in `{$IF}`; M6 checked `negate`; M7 panic-free `try_text`/
  `try_location_text`; M8 project switch options → `{$IFOPT}` defaults; M9
  generic parameters + constraint spans captured; M10 ddk BOM strip; M11 ddk
  versions `f64` + skip malformed rows; M12 DFM surrogate-pair `#$` codes; M13
  flush moka before index rebuild.
- LOW: fixed L2, L3, L4, L5, L8, L14, L16 (DFM structural-keyword property),
  L17 (watcher `max_defer` ceiling). L12 verified NON-ISSUE (dirty set from the
  invalidation report, not `insert_count`). Ledgered with concrete plans in
  REVIEW.md: L1 (Delphi has no digit separators — dead code), L6 (uint const →
  needs `ConstantValue::UInt` + format bump), L7 (codepage), L9 (ordinal fold),
  L10 (`rtl_version` profile field), L11 ($ELSEIF strictness), L13 (alias case),
  L15 (arena must retain raw bytes to hash), L18 (open window), L19 (ddk shim),
  CONFIG (build harness context from dproj active config).
- DISCOVERED + fixed while in the code: `class const` / `class type` members
  aborted the unit (valid Delphi) — now routed to nested-const/type parsers.
- Empirical stress: 462 → 464 units OK (H6/H7 real-source failures gone). The
  single remaining failure is `be.core.gui.dpk` — intentionally non-compilable
  German prose in a `{$IFDEF BE_CORE_D11_USES}` dead branch reached only because
  the harness define set is narrower than the real build (CONFIG ledger item,
  not a grammar bug).
- Tests: 126 portable + 6 local (132 total), all green. Regression test added
  per fixed item. New broad harness `stress_full_src_tree` (local-tests).
- REVIEW.md is the authoritative backlog; SESSION.md ledger items 16/19 remain
  (attribute capture; scoped `Declared(A.B)`), now unblocked by the deep parse.

## Iteration 9 work log (2026-07-31) — LSP FULL-AST SERIALIZATION

Executed the `lsp-full-ast-spec.md` refactor on `feat/lsp-full-ast-serialization`.
Per-step, `cargo test` green at every commit.

- **Global interner + arena statics (`globals.rs`).** Interner (`ThreadedRodeo`)
  and `SourceArena` are now process globals reached via `AtomicPtr` over a leaked
  `Box` (reads lock-free, return `&'static`; a reset leaks the old instance so
  outstanding refs stay valid — sound even under the parallel test runner). No
  `thread_local` (cross-thread Spurs must resolve). `ProjectContext` no longer
  owns the interner; `intern`/`intern_key`/`resolve` delegate to `globals`.
  `reset_for_tests` exists per spec (leak-and-swap) but is unused in-tree — the
  suite is reset-independent because interning is idempotent (documented). Commit
  4c60ef1.
- **Transparent serde.** `Identifier` is now a newtype over `Spur` with `Deref`
  and custom serde (Spur→string on save, `get_or_intern` on load) — chosen over a
  per-field `#[serde(with)]` after weighing churn; the newtype localized the break
  to a handful of raw `get_or_intern` sites. `FileId` serde resolves through the
  global arena (→path on save, lazy `register` on load; an unregisterable path is
  a clean serde error, never a panic). `Span`/`CodeLocation` derive serde. Commit
  9612d42.
- **AST tree serde** derived across every `ast.rs` type + `ConstantValue`; full
  `Source` round-trip test proves names/paths survive as strings. Commit 0850903.
- **`UnitMeta` wrapper (`unit_meta.rs`).** Owns the unit AST + stamps + deps +
  usages + cycle_taint; the interface surface (symbols + flattened members) is a
  `#[serde(skip)]` `OnceCell` index DERIVED from the AST on demand
  (`interface()`), no longer a separately-persisted struct. Commit 3f7556f.
- **Retired persisted `UnitArtifact`.** `CacheEntry::Done` now holds
  `Arc<UnitMeta>`; cache/store/pipeline/loader/driver/watcher swapped over;
  `SavedSymbol`/`SavedMember`/`SavedUnit`/`from_artifact`/`into_artifact` deleted;
  format **v6 → v7**. Persistence is one bincoded `UnitMeta` byte segment per
  unit (per-segment isolation keeps the load panic-free — a corrupt/
  unregisterable unit is dropped `unreadable`, M2). `save`/`load` no longer take
  arena/interner params (serde is transparent through globals). The driver and
  the nested-import loader now use the GLOBAL arena so serialized `FileId`s
  resolve consistently. Commit 32b0028.
- **Invariants preserved + tested:** cycle-tainted metas never persisted (save
  skips them); moka `run_pending_tasks()` before save and before index rebuild;
  hash validation (own source + includes + dependency sources + their includes,
  H9); virtual/unsaved buffers never persist (FileId register fails on load →
  unreadable, #21/#25); panic-free decode (M2, per-segment + `validate_meta`).
- **No-raw-integer proof:** `globals::tests::identifier_bytes_are_a_string_not_a_raw_spur`
  asserts an `Identifier`'s bytes are byte-identical to a bincoded `String` and
  the raw `Spur` integer is absent; `file_id_bytes_are_a_path_not_a_raw_index`
  does the same for `FileId`. Plus dual-track round-trip
  (`dual_track_survives_round_trip`).
- Tests: **136 portable + 6 local (142 total)**, all green. Warnings dropped
  158 → 105 (fewer types).

DESIGN FORKS resolved (flagged): (1) newtype vs per-field `serde(with)` for
`Identifier` — chose newtype (lower long-term risk, derive "just works" on the
AST). (2) `reset_for_tests` soundness under parallel tests — chose leak-and-swap
+ reset-independent tests (no test swaps the global mid-run). (3) driver/loader
arena — switched to the `&'static` global (required for `FileId` serde to
resolve); tests that never serialize also use the global (idempotent/additive,
no cross-test assertion depends on emptiness).

## Iteration 10 work log (2026-08-01) — RICH INTERFACE INDEX + ATTRIBUTES + SCOPED DECLARED

Executed `task2-rich-interface-spec.md` on `feat/lsp-full-ast-serialization`.
Per-step, `cargo test` green at every commit.

- **Attribute capture (#16 CLOSED), format v7→v8.** New `ast::Attribute { name:
  QualifiedName (dual-track, AS WRITTEN — no `Attribute`-suffix normalization),
  arguments: Option<CodeLocation> (raw `(...)` span, never evaluated), location }`.
  `parse_attributes` replaces `skip_attribute` at ALL six sites (top-level
  section dispatch, type/const/var sections, member, parameter): balanced-bracket
  tolerant (nested `[]`/`(...)`), stacked `[A][B]` + comma `[A, B]` flatten to a
  Vec, qualified names (`[Xml.Serializable]`), argument identifier usages
  recorded. Attributes attach to `InterfaceDeclaration`, `Member::{Field,Method,
  Property,NestedType,NestedConst}`, and `Parameter` (before OR after the
  modifier). Top-level `[Foo] type …` captured into a `pending_attributes` buffer
  drained by the section parser. Format bumped once (v8). Commit 54d18e3.
- **Rich derived interface index (no format impact — derived).** `MemberSymbol`
  gains `type_key: Option<Identifier>` (simple field/property/return type ref),
  `directives: Vec<Identifier>` (method directive keys), `visibility` (threaded
  from the enclosing `VisibilitySection`; records/interfaces → `Unspecified`),
  `attributes: Vec<Identifier>`. `InterfaceSymbol` gains `attributes`.
  `build_interface`/`collect_from_members` fill them straight from the AST. All
  existing `MemberSymbol` behaviour (name/key/kind/location/read+write target)
  unchanged — property read/write targets, member kinds, dfm-link input still
  correct. Commit dab1373.
- **Scoped `Declared(A.B[.C])` (#19 CLOSED).** `is_declared_scoped`: own type
  first (state records per-type member→member-type pairs BEFORE trailing
  directives so the cursor sees them mid-parse), then imported types in reverse
  uses order via the loader, also `Unit.Type[.Member]`. Member walk descends via
  the member's simple `type_key`. EVERY consulted import recorded as a
  dependency. Never confident-false unless the whole chain resolves and the
  member is absent; unresolvable segment → Unknown; cycle → taint + Unknown.
  Cross-unit member-type descent deferred (#31). Dotted const VALUES deferred
  (#30). Commit 119d649.
- **PROOF POINTS (tests):** `attributes_survive_serde_round_trip` (attribute name
  + argument span survive save/load via the AST); `attributes_captured_at_
  declaration_member_parameter`, `attribute_name_preserves_case_and_dotted_form`,
  `nested_brackets_in_attribute_arguments_do_not_close_early`;
  `member_symbol_exposes_type_directives_visibility_attributes`;
  `scoped_declared_own_type_member`, `scoped_declared_imported_type_member_
  records_dependency`, `scoped_declared_unresolved_first_segment_is_unknown`,
  `scoped_declared_nested_three_segments`.
- **BUG FOUND + FIXED while implementing scoped Declared:** own symbols were
  recorded AFTER `consume_trailing_directives`, but the cursor evaluates a
  following `{$IF Declared(TFoo.Bar)}` on the next peek — so members were invisible
  to it. Moved `declare_interface_key` + `record_own_type_members` to BEFORE
  `consume_trailing_directives`.
- Task-1 guarantees preserved: panic-free persistence (per-segment isolation, M2),
  no raw Spur/FileId on disk (transparent serde), SaveReport/LoadReport symmetry,
  dual-track interning — all existing tests still green.
- Tests: **146 portable (+1 ignored) + 6 local (152 total)**, all green. Ledger:
  #16 + #19 CLOSED; #30 (dotted const value) + #31 (cross-unit member-type
  descent) newly ledgered with plans.

## Iteration 11 work log (2026-08-01) — DFM↔PAS LINKER

Executed `task3-dfm-linker-spec.md` on `feat/lsp-full-ast-serialization`.
Per-step, `cargo test` green at every commit.

- **Deliverable A — pure linker (`dfm_link.rs`).** `link_dfm(&UnitInterface,
  &DfmObject) -> DfmLinkResult` (no I/O). Produces `ComponentLink`
  (component name → published `Field` member whose `type_key` equals the dfm
  node's class), `HandlerLink` (ident-valued property → `Method` member), and
  `DfmDiagnostic`. **Honest-diagnostic policy (Unknown-not-false, reuses task-2
  `has_ancestors`):** form class not in this unit → single `FormClassNotFound`
  note, no per-member guessing; class present + `has_ancestors` (every real
  form — implicit `TObject` or explicit/cross-unit base) → unresolved
  component/handler = `UnresolvedComponentPossiblyInherited` /
  `UnresolvedHandlerPossiblyInherited` INFO note, NEVER a hard error; class
  present + ancestor-less (self-contained shape) → hard `DanglingComponent` /
  `MissingHandler`. **Enum-vs-method filter:** a handler link is produced ONLY
  when the value key matches an actual `Method` member; enum values (`Align =
  alClient`, `Color = clBtnFace`) match no method → no link. A non-method ident
  emits a diagnostic ONLY if the property name follows the `On…` event
  convention AND is unresolved — a plain enum value is silent (not "missing").
  Type-mismatch (name matches a field, type differs) surfaced regardless of
  ancestors (the field IS local). Root object's own name is skipped (it is the
  form instance, not a published field). Commit a74a1e6.
- **Deliverable B.2 — dfm stamp on `UnitMeta`, format v8→v9.** New
  `UnitMeta.dfm: Option<SourceStamp>` (path+hash, same shape as an include
  stamp → transparent serde, no raw id). Included in `watched_files()`,
  load-time `validate_meta`, and the watcher `revalidate` sweep, so a `.dfm`
  edit stales the unit exactly like an include edit. `UnitMeta::with_dfm`
  builder keeps `new`'s positional signature stable. Format bumped v8→v9;
  version-reject test updated. Roundtrip/stale test `changed_dfm_is_stale_on_
  load`. Commit 4ce21ab.
- **Deliverable B.3 — dfm↔pas association + driver wiring.**
  `pipeline::sibling_dfm_stamp(pas_path)` derives `Unit1.dfm` from `Unit1.pas`
  (same dir, `.with_extension("dfm")`), stamped only when it exists on disk;
  `build_unit_meta` attaches it. `ProjectSession::parse_source_file` now runs
  `link_sibling_dfm`: decodes the dfm through the arena (BOM/UTF-16/ANSI, binary
  DFMs rejected distinctly per #23), parses it, runs `link_dfm` against the
  meta's derived interface, and stores the `DfmLinkResult` in a session map.
  `ProjectSession::dfm_links(unit_key)` is the session-level query surface for
  the LSP layer (task 5). A dfm read/decode/parse failure is a NOTE, never
  fatal. End-to-end test `parse_links_sibling_dfm_and_dfm_edit_invalidates_
  unit`. Commit 1019396.
- **Task-1/2 guarantees preserved:** the dfm stamp is a path+hash (reuses the
  include-stamp pattern) so no-raw-id-on-disk holds automatically; panic-free
  persistence, Save/LoadReport symmetry, dual-track interning all unchanged
  (existing tests still green).
- **PROOF POINTS (tests):** `component_and_handler_links_on_realistic_form`
  (component→published-field + event-prop→method); `enum_valued_property_
  produces_no_link` + `non_event_ident_property_matching_no_method_is_silent`
  (enum filter, both directions); `dangling_component_on_ancestor_less_form_is_
  a_hard_diagnostic` (hard error only when truly cannot inherit);
  `unknown_base_form_yields_unresolved_note_not_error` (cross-unit base →
  note, not error — the Unknown-not-false analog, both component AND handler);
  `form_class_not_in_unit_emits_only_a_note`; `type_mismatch_when_name_matches_
  but_type_differs`; `changed_dfm_is_stale_on_load`; `parse_links_sibling_dfm_
  and_dfm_edit_invalidates_unit`.
- **DEFERRED / LEDGERED:** the LSP query API that turns a stored `DfmLinkResult`
  into go-to-definition / rename responses is a task-5 dependency — the session
  now STORES links + diagnostics and exposes `dfm_links()`, but the request
  handlers live in delphi-devkit. Ledger #34. Also: cross-unit base-form member
  resolution (walking the actual base class's fields/methods to turn a
  possibly-inherited NOTE into a confident link/miss) is deferred — same
  precision follow-up shape as #33 for aliases. Ledger #35.
- Tests: **161 portable (+1 ignored) + 6 local (167 total)**, all green.
- **REVIEW FIX PASS (task-3 adversarial, FIX-REQUIRED closed):**
  - *(MEDIUM)* Handler links now require event-name (`On…`) AND method-match as
    BOTH conjuncts. Previously any ident property whose value collided with a
    method name (`Action = SomeName`, `Kind = bkOK`) emitted a spurious
    cross-boundary HandlerLink; `is_event_property` was only consulted in the
    no-match branch. Now `link_handlers` skips non-event idents up front. Module
    doc corrected (method-match alone is NOT authoritative). Regression:
    `non_event_ident_property_colliding_with_method_name_produces_no_link`.
  - *(MEDIUM)* `dfm_links` map is now purged on invalidation. `InvalidationReport`
    grew an `invalidated_keys: Vec<Identifier>` (populated in per-file + sweep
    eviction paths; failed-entry drops carry no links so their keys are omitted),
    and `apply_plan` removes each evicted key's `dfm_links` entry. Previously
    `dfm_links(unit_key)` served stale pre-edit links until re-parse. Regression:
    extended `parse_links_sibling_dfm_and_dfm_edit_invalidates_unit` asserts the
    key is reported and `dfm_links(unit_key)` returns `None` post-edit.
  - *(LOW)* `resolve_field` doc corrected: any matching `Field` links regardless
    of visibility (IDE component fields land in the parser's `Unspecified`
    default section; gating on `Published` alone would zero out real links). The
    filter (`kind == Field`) was already right; only the doc was misleading.

## Iteration 12 work log (2026-08-01) — SIZEOF STAGE 2 (RECORD/STRUCT LAYOUT)

Executed `task4-sizeof-layout-spec.md` on `feat/lsp-full-ast-serialization`.
Per-step, `cargo test` green at every commit. NORTH STAR honored: `size_of`
returns `Some(n)` ONLY when the layout is unambiguously dcc-correct; ANY
uncertainty → `None`, never a guessed number.

- **`layout.rs` — layout engine.** New `Layout{size,alignment}` +
  `type_layout(type_expr, switches, platform, resolver) -> Option<Layout>` +
  `LayoutResolver` trait (resolve a `Reference` name → `TypeExpression`; read a
  bound span's text; evaluate an integer const expression; depth-guard). Handles:
  scalars/pointers (class/interface/pointer/classref/`reference to`/dynamic
  array/`string` = 1 ptr; `procedure of object` = 2 ptr; bare routine = 1),
  distinct-unwrap, enumeration (smallest of 1/2/4 holding max ordinal, floored by
  `{$Z}`; explicit `= v` shifts the running ordinal via the const machinery;
  negative ordinal → None), subrange (int + char bounds via `..`-split, correct
  signed/unsigned width), ShortString (`n+1`, align 1), static array (multi-dim
  `element_count × size(T)`; dynamic `array of T` = ptr; named index type →
  None #38), record/object (fields in order, each at `align_up(offset,
  min(align(field), {$A}))`, record align = `min(max field align, {$A})`, final
  size padded; `is_packed`/`{$A}==1` → all align 1; `class var` excluded; any
  unknown field → None). VARIANT records + SETS → None (deferred, ledger
  #36/#37). builtin stage 1 unchanged; added `builtin_alignment`.
- **`if_eval.rs` — wired `StateResolver::size_of`.** builtin table first; else
  resolve the named type (own `own_type_expression` first, then imports in reverse
  uses order via loader, recording EVERY consulted unit as a dependency; cycle →
  taint + None) and run `type_layout`. Exposed `evaluate_value` (Value, not just
  a bool condition) for bound evaluation. `StateResolver` gained `layout_depth`
  (ceiling 64 → None on alias cycles / self-referential records).
- **`parse_state.rs` — own-type structure.** New
  `own_type_expressions: HashMap<Identifier, Rc<TypeExpression>>` +
  `record_own_type_expression`/`own_type_expression`, so `{$IF SizeOf(TFoo)}` on
  a type declared earlier in THIS unit's interface can lay it out mid-parse
  (needs packed flag / nested inline types the flattened member map lacks).
- **`parser.rs`** records the own type's full `TypeExpression` (one `Rc` clone,
  the owned value still moves into the declaration) BEFORE trailing directives,
  same ordering discipline as `own_type_members`.
- **`ast.rs`** — derived `Clone` on the `TypeExpression` subtree (the exact set
  reachable from a type body) so own types can be recorded as `Rc<TypeExpression>`
  and imported types cloned out of the borrowed `UnitMeta.ast`. No format impact
  (Clone is not serde).
- **ABI PROOF (tests, Win32 AND Win64 where they differ):** `record Byte;Integer`
  = 8 ($A8: 1+3pad+4), same `packed`/`{$A1}` = 5; `record Byte;Int64` = 16
  (Int64 8-byte aligned, both targets); nested record = 8; enum 200 vals = 1 under
  $Z1 / 4 under $Z4, 300 vals = 2, `(a,b=300,c)` = 2; `0..255`=1, `0..256`=2,
  `'a'..'z'`=1; `array[1..10] of Integer`=40, `array[0..3,0..1] of Byte`=8;
  `string[20]`=21; class field = 4/8; `string`/pointer field = 4/8; cross-unit
  `SizeOf(TPoint)`=8 with dependency recorded. **Confidence-discipline gate:** a
  record with an unknown field type → SizeOf Unknown for EVERY probed size (8, 5,
  >0) — `confidence_discipline_unknown_field_is_none_not_a_wrong_number`. Variant
  + set records → Unknown (deferred, tested).
- **Task-1/2/3 guarantees preserved:** no raw ids on disk (Clone is not serde;
  persistence unchanged), panic-free persistence, scoped Declared / attributes /
  dfm links untouched — all prior tests still green.
- **ABI RULES FLAGGED / deferred to None (never guessed):** variant-record arm
  overlap (#36), set sizing byte-boundary rule (#37), array over named index type
  (#38), negative enum ordinals (→ None in `enumeration_size`). Also: the spec's
  example "−1..100 → 2 signed" is WRONG per dcc — `-1..100` fits ShortInt = 1
  byte; my `integer_size_for_range` follows dcc (smallest signed type holding both
  ends), documented in `integer_range_sizing_rule`.
- Tests: **184 portable (+1 ignored) + 6 local (190 total)**, all green. Commits:
  layout engine + wiring, ABI proof tests, alignment cleanup.

## Iteration 13 work log (2026-08-01) — LSP QUERY API + ERROR-TOLERANT REPARSE

Executed `task5-lsp-query-error-tolerant-spec.md` on
`feat/lsp-full-ast-serialization`. Per-step, `cargo test` green at every commit.
North Star honored: a query / partial parse NEVER returns a wrong answer —
insufficient info → empty/None, a dropped region → diagnostic, never a bogus
symbol/usage/location.

- **Deliverable A — LSP query API (`query.rs` + methods on `ProjectSession`).**
  - `symbol_at(unit_key, position)` — the identifier occurrence under a byte
    position: declaration/member sites first (most specific identity), then the
    tightest usage span. Returns folded key + display + kind + span + owner type.
  - `definition(unit_key, symbol_key, member_owner)` — own interface symbol
    location, else resolve through imports in reverse uses order via the SAME
    loader as scoped `Declared` (cycle-safe, dependency-honest); member targets
    resolve the owner type (own → imports) then its member site. Empty when
    unresolved — never a wrong location.
  - `references(symbol_key)` — every occurrence across cached units from a new
    `references.rs` symbol→occurrences index (interface decls + members +
    recorded usages). PURGED per-unit on invalidation and REBUILT on full sweep,
    mirroring the task-3 dfm_links purge (no occurrence points into an evicted
    unit). Over-approximating candidate set (usages not scope-resolved) —
    documented; never misses a real occurrence.
  - `completions(unit_key, position)` — after `.`: members of the receiver's
    resolved type (own → imports), members only, visibility surfaced, empty on an
    unresolvable receiver (never a wrong member list); else top-level = builtins +
    own symbols declared up to the cursor + imported units' symbols, de-duped by key.
  - `diagnostics(unit_key)` — parse diagnostics (recovery + directive) unified
    with dfm-linker diagnostics into one `UnifiedDiagnostic` list, tagged by
    source; dfm findings carry a pas location only when they name a concrete
    member (else a dfm byte offset) — never a fabricated pas location.
- **Deliverable B — error-tolerant declaration-level resync (`parser.rs`).**
  `parse_interface_declarations` catches a per-item `ParseError`, records a
  diagnostic, resyncs to the next declaration boundary and continues — surviving
  declarations populate the interface, the broken region emits a diagnostic and
  NEVER a symbol. Termination guaranteed (≥1-token progress, lexer-error-tolerant
  resync). A recovered parse is flagged `ParseOutcome.recovered → UnitMeta.recovered`
  (format v9→v10) and NEVER persisted clean (save gate, same as `cycle_tainted`).
  Directive-structure errors stay unrecoverable. Depth counter reset on recovery
  (a `?`-unwound `RecursionLimit` leaves it inflated — regression-tested).
- **PROOF POINTS (tests):** `symbol_at_hits_declaration_and_definition_resolves_
  own_and_cross_unit` (symbol_at hit; definition own + cross-unit w/ the import
  cached as dependency; member def cross-unit; unresolved → empty);
  `references_across_units_and_purge_on_invalidation` (TThing referenced from two
  units; UserA eviction purges its occurrences, UserB's survive, none point into
  the gone unit); `member_completion_after_dot_and_top_level_includes_import` +
  `member_completion_after_dot_resolves_receiver_type` (members after `.`,
  imported symbol in top-level, builtins, no top-level leak into members);
  `diagnostics_unifies_parse_and_dfm`; `broken_middle_declaration_still_yields_
  the_others_with_a_diagnostic`; `lexer_error_in_active_declaration_recovers`;
  `recovery_terminates_on_pathological_input` (5000× `^`, wall-clock guard);
  `recursion_limit_recovery_resets_depth_for_later_declarations`;
  `clean_parse_is_not_flagged_recovered`; `unterminated_conditional_is_not_
  swallowed_by_recovery`; `recovered_unit_is_flagged_and_not_persisted_clean`
  (session end-to-end: flagged + `save.written == 0`).
- **Task-1..4 guarantees preserved:** panic-free persistence (per-segment M2),
  no raw Spur/FileId on disk (transparent serde; `recovered` is a plain bool),
  SizeOf/Declared/attributes/dfm links unchanged — all prior tests still green.
- **DEFERRED / LEDGERED:** implementation-body + expression-level recovery
  (#39) — the interface section is the LSP-critical surface; the impl body is
  only token-scanned for the usage index (a lexer error there still aborts).
  Also a completion-receiver enrichment hook (`symbol_declared_type_key`
  currently returns None for a var/const receiver → top-level, never wrong).
- Tests: **198 portable (+1 ignored) + 6 local (204 total)**, all green. Commits:
  query API + reference index; error-tolerant resync + never-persist gate.

## Iteration 14 work log (2026-08-01) — CLEAR REVIEW.md LEDGERED LOWS (+ consistency fix)

Executed `task6-ledgered-lows-spec.md` on `feat/lsp-full-ast-serialization`.
One commit per item, `cargo test` green before each. Never a WRONG confident
value: uncertainty → Unknown/None throughout (esp. L6 mixed-width, L9 fold).

- **L10 — RTLVersion distinct constant** (commit 54ce359). `CompilerProfile`
  gained `rtl_version: Option<f64>` (`None` = compiler_version); `ProjectContext`
  gained `rtl_version: f64` (resolved at `from_dproj`). `const_value` returns it
  for `RTLVERSION`, independent of `COMPILERVERSION`. Test:
  `rtl_version_and_compiler_version_evaluate_independently`.
- **resolve_qualified_type consistency** (commit 5c55970). It now SKIPS an
  unrelated missing/failed/cyclic import and keeps scanning for the named unit
  (mirrors `is_declared_scoped`), instead of aborting to Unknown on the first
  unresolvable import. Never-wrong guarantee unchanged (only ever a type from
  the name-matching unit). Test (fails without the fix):
  `qualified_sizeof_resolves_past_unrelated_missing_import`.
- **L9 — single fold_identifier** (commit e2be651). ONE ordinal-ASCII fold
  (`a`→`A`, non-ASCII byte-identical) in `globals`; routed `intern_key`,
  `is_defined`, `resolve_unit`, `if_eval` builtin/const folds, `layout` builtin
  lookup through it. Behaviour-preserving for ASCII (dual-track tests green);
  non-ASCII never a wrong match. Directive/switch KEYWORD folds and unit-name
  `eq_ignore_ascii_case` justified as fixed-ASCII / already-ordinal. Tests:
  `fold_identifier_is_ordinal_ascii`, `..._leaves_non_ascii_byte_identical`,
  `non_ascii_tracks_agree_through_one_fold`, `non_ascii_define_ifdef_round_trip_
  is_consistent`.
- **L15 — raw-byte hashing without re-read** (commit 0549418). `SourceArena`
  retains raw on-disk bytes (`raw_bytes(FileId)`); `stamp_file` hashes those —
  one read, no TOCTOU. Hash input byte-identical to `hash_file` (existing
  snapshots still validate). Virtual buffers → None → decoded-content fallback
  (#21/#25 preserved). Tests: `source_stamp_hashes_raw_bytes_for_ansi_and_
  utf16`, `raw_bytes_retained_after_disk_read`.
- **L6 — large unsigned constants + format v10→v11** (commit 2e28985). Added
  `ConstantValue::UInt(u64)` (transparent serde, no mirror);
  `parse_integer_literal` tries u64 on i64 overflow (still-too-big → None, never
  a bit-cast). `if_eval` gained `Value::UInt`/`Token::UInt`, EXACT mixed-width
  compare/arithmetic via i128 (narrow to tightest, else Unknown), tokenizer u64
  retry for hex/dec/bin/oct. Format v11; old-version-reject test updated. Tests:
  `large_unsigned_constants_evaluate`, `large_unsigned_constant_captured_and_
  round_trips`, `cross_unit_large_unsigned_constant_evaluates`.
- **CONFIG — harness governing-dproj coverage** (commit 4921ab0). The stress
  harness parses each file under its governing dproj active config (package/
  program via sibling `<stem>.dproj`, else be.dproj) — all via `from_dproj`.
  `be.core.gui.dpk`'s dead branch resolved (its own dproj defines
  `BE_CORE_D11_USES`). Tree now clean: 464 units + 4 pkg/prog, **0 failures**;
  guard asserts `total_failures == 0`.

- **Task-19 — bound the arena's disk-file text (last unbounded term).** DISK
  `SourceEntry.content`/`raw` changed from write-once `OnceLock` to clearable
  `Mutex<Option<Box<str>>>`/`Mutex<Option<Box<[u8]>>>`; a per-entry `last_access`
  tick (global `access_clock`) drives an LRU. `SourceArena::trim_disk_content(cap)`
  evicts the coldest DISK entries' content+raw until resident ≤ cap; VIRTUAL
  entries are NEVER trimmed (their display path can't be re-read → data loss —
  Task-15's bound owns them). `content`/`raw_bytes`/`loaded_content`/`text`
  re-read a cleared/never-read disk file on demand via the same lifetime-extend
  transmute as `virtual_content_ref` (`disk_content_ref`/`disk_raw_ref`).
  `ProjectSession::trim_arena()` = `trim_disk_content(ARENA_DISK_CONTENT_CAP =
  64 MiB)`, called ONLY at SAFE CHECKPOINTS: the end of every blocking
  parse/query section (analyze, all six read handlers, didSave
  `parse_disk_and_save`), after owned LSP results are built, still under the
  session `blocking_lock()`, before it releases — NEVER reactively inside
  `content` (a same-parse borrow could be live → UAF). SOUNDNESS (mirrors L15's
  virtual note, argued in-code at `trim_disk_content` + `trim_arena` + each call
  site): no arena `&str`/`&[u8]` escapes a blocking section (every caller copies
  to owned or uses it within the synchronous parse/query and drops it before
  returning); the single session lock serializes all parses/queries so a trim
  between them cannot race a live borrow; the moka persister serializes paths not
  text, the loader reads only during a parse. Preserves virtual-never-persist,
  Task-15 virtual bound, Task-16 reload+hash-validation, dual-track, never-wrong,
  panic-free (a failed re-read → the existing `FileReadError` path). Tests:
  `trim_disk_content_bounds_and_reread_is_correct`,
  `trim_then_disk_change_rereads_new_bytes_without_crash`,
  `trim_never_clears_virtual_entries`, `trim_evicts_least_recently_accessed_first`
  (source.rs local arena); `parse_then_trim_then_query_is_sound_and_correct`
  (driver checkpoint); `trim_arena_at_checkpoint_bounds_disk_content_and_query_
  still_resolves` (server). UNVERIFIED: live-editor process RAM not measured, only
  the trim/re-read/bound tests; the 64 MiB cap is a reasoned choice, not tuned
  against a real workload.

- **Format-version delta:** v10 → v11 (L6 `ConstantValue::UInt`). Old snapshots
  reject cleanly (`old_version_snapshot_is_cleanly_rejected`, updated to v10→v11).
- **Guarantees preserved:** dual-track (now via one fold), transparent serde /
  no-raw-id-on-disk, panic-free persistence, stable-`&str` arena, never-wrong
  Unknown discipline — all it.1..13 tests still green.
- Tests: **212 portable (+1 ignored) + 6 local (218 total)**, all green.
- **RE-LEDGERED (not closed):** ledger #5 sub-note — dcc's actual NON-ASCII fold
  is unverified; the ordinal-ASCII + byte-identical choice is the safe direction
  (never over-matches), revisit only if a real non-ASCII-identifier case appears.

## Next iteration start here

~~Deep type parse~~ DONE (it. 7). ~~Member symbols v5~~ DONE (it. 7).
~~Deep review + fix pass~~ DONE (it. 8) — see REVIEW.md for the ledgered
follow-ups (L6 uint const, L9 fold, L10 rtl_version, L15 arena raw bytes, etc.).
Next, in suggested order:
1. ~~dfm↔pas linker~~ DONE (it. 11). ~~scoped Declared(A.B)~~ DONE (it. 10).
   ~~Attribute capture~~ DONE (it. 10). Follow-ups: LSP query surface for dfm
   links (#34), cross-unit base-form member resolution (#35).
2. ~~SizeOf stage 2: record layout~~ DONE (it. 12). Remaining SizeOf gaps are
   deferred + ledgered: variant records (#36), sets (#37), array-over-named-index
   (#38) — each returns None (Unknown), never a wrong number.
3. ~~LSP query API surface + error-tolerant reparse~~ DONE (it. 13). Query API
   (symbol_at/definition/references/completions/diagnostics) on ProjectSession;
   declaration-level resync for the interface section (#10 closed for interface).
   Remaining: implementation-body / expression-level recovery (#39), DFM
   go-to-def/rename request handlers live in devkit (#34).

## Next steps (priority order, per "infrastructure first")

1. ~~Dual-track interning + AST no-strings~~ DONE (iteration 1)
2. ~~LocalAppData persistence primitive~~ DONE (iteration 1). Still open:
   periodic-save trigger + load-on-context-swap wiring — belongs to the
   driver/watcher layer (next item), since "regularly" needs a running loop.
3. ~~File watchers + burst detection~~ DONE (iteration 2).
4. ~~Driver (ProjectSession)~~ DONE (iteration 3).
5. ~~ddk CLI integration~~ DONE (iteration 3). Open: consume
   `standard_source_directories` when building search paths for a session
   (append to context.search_paths at open — do together with slice 3 unit
   resolution so RTL units resolve).
6. ~~Artifact production pipeline~~ DONE (iteration 5).
7. ~~Slice 3 + driver wiring + RTL search paths~~ DONE (it. 5).
8. ~~Layout stage 1 (builtin sizes) + cross-unit constant values~~ DONE
   (it. 6). Usage-index skeleton also DONE (it. 6).
9. Deep type parse — the next big block: full type structure (records with
   fields → SizeOf stage 2 with align/min_enum_size, classes with members,
   generics structure, attributes captured), replaces the class-block
   heuristic (ledger #17), enables scoped `Declared(A.B)` (#19), member
   completion, precise references.   ← NEXT
10. ~~DFM parser~~ DONE (it. 6). ~~DFM↔PAS linker~~ DONE (it. 11):
    `dfm_link::link_dfm` matches component names/handlers against .pas form
    classes with honest diagnostics; dfm SourceStamp on `UnitMeta` (format v9)
    in the session/watcher flow; driver runs the linker on parse and exposes
    `dfm_links()`. Remaining: LSP query surface for these links (ledger #34)
    and cross-unit base-form member resolution (ledger #35).
11. ~~Error-tolerant reparse (ledger #10) + LSP query API surface for devkit~~
    DONE (it. 13). Remaining: finer recovery (#39), inline hints data.
8. Layout engine (SizeOf), full construct coverage, usage collection,
   dfm-link extraction, incremental reparse for live typing.

Infrastructure phase (user priority) is hereby complete except the two
wiring points that need the parse pipeline (5./6.) — switching to core
parsing next iteration.

## Problems for next iteration

- None blocking. Ledger items 5, 9, 11 need verification against real dcc
  behavior (small Delphi test programs via ddk compile).
