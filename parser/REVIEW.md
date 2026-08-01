# Deep Review Backlog (2026-07-17)

Five-agent deep review of the whole crate + empirical stress harness against
`C:\Delphi\VSS\Intern\src` (468 .pas/.dpr/.dpk, 462 OK, 0 panics before fixes).
Items ranked; the writer works top-down, reviewer gates each commit, tester
adds a regression test per fix. Mandate: every item is FIXED or ledgered with a
plan — no "works 98%".

Status key: ` ` open · `WIP` in progress · `x` fixed+reviewed+tested · `LEDGER` deferred with plan.

## Writer round notes (PARSER-GRAMMAR cluster, parser.rs / ast.rs)

- H5: bound is `MAX_PARSE_DEPTH = 64`, not ~256. Empirically pure `^^^…T`
  overflows a 2 MiB stack (cargo-test worker default) at ~125 debug frames;
  64 keeps ~2x margin and still dwarfs any real Delphi nesting. Verified both
  the caret and nested-record paths degrade to `Err(RecursionLimit)`.
- H3: `external`-clause (lib + name/index/delayed/dependency) handled in
  `consume_method_directives`; `forward` added to the routine-directive set.
  `name` is recognized ONLY inside the external clause — it was deliberately
  NOT added to the general `is_trailing_directive` set, because `Name` is a
  very common const/field identifier and the shared set also drives
  `consume_trailing_directives` for types/vars. `dependency` (plain `Ident`,
  no token) matched by text; if it appears without a preceding name/index the
  clause is still fully consumed (absorbed into the library span) — no abort.
- DISCOVERED + FIXED (not in the original backlog): `class const` / `class type`
  members aborted the unit ("member kind after 'class'"). This codebase does
  not use them so the stress harness missed it, but it is valid Delphi. Routed
  both into the shared nested-const/nested-type parsers. Their class-level flag
  is not modeled (Member::NestedConst/NestedType have no is_class) — parsing
  correctness only; flag modeling can follow if needed.
- Empirical stress (`stress_full_src_tree`, C:\Delphi\VSS\Intern\src): 462→464
  units OK. The single remaining failure is `be.core.gui.dpk` whose `.inc`
  `contains` list holds bracketed German prose ("Dieses Projekt ist noch nicht
  kompilierbar…") — genuinely non-compilable source, not a grammar bug.

## HIGH — correctness on real source / silent corruption / crash

- [x] H1 `&`-escaped identifiers interned WITH the `&` (`token.rs:263`, all
  `intern`/`intern_key` call sites, `if_eval.rs:355`). `&Type` ≠ `Type` →
  broken find-references / `Declared`. Also `{$IF Declared(&X)}` aborts unit.
  Fix: strip one leading `&` in an `identifier_text()` helper before interning;
  accept leading `&` in if_eval tokenizer.
- [x] H2 `{$I}` probes unit search path, not include path (`token_cursor.rs:447`;
  `DCC_IncludePath` never read in `context.rs`). Dedicated `.inc` dirs → unit
  abort. Fix: read `DCC_IncludePath` into `context.include_paths`, probe it.
  Sub: unquoted-name whitespace truncation, single-quote-pair stripping.
- [x] H3 `external`/`name`/`index` routine directives unhandled
  (`parser.rs:1799`, `is_trailing_directive:2064`). Interface external decls
  (RTL/Winapi/`external 'kernel32' name '...'`) abort the unit. Fix: parse an
  `external`-clause (lib expr + name/index/delayed/dependency args) in
  `consume_method_directives`; add `Forward`/`Name` to the recognized set.
- [x] H4 Generic ancestor + class-helper target not consuming `<...>`
  (`parser.rs:1074` interface ancestors, `:997/:1047` helper `for`). `IList<T> =
  interface(IEnumerable<T>)` and `class helper for TList<Integer>` → parse error.
  Fix: call `parse_type_argument_list()` after every ancestor / for-target.
- [x] H5 No recursion-depth guard anywhere in the grammar parser → native stack
  overflow (process abort, not `Err`) on `^^^^…Integer` / deep nested generics /
  nested variant parts. Fix: thread a depth counter through
  `parse_type_expression`/`parse_member_sections`/`parse_variant_part`; error
  past ~256 (mirror `MAX_INCLUDE_DEPTH`).
- [x] H6 Procedural-type field drops trailing calling convention:
  `X: function(...): BOOL stdcall;` → `stdcall` misread as next field
  (`beHook.pas`). Also cdecl/register/safecall/`of object`. Fix: consume
  directive chain after a procedural field/var type.
- [x] H7 Method resolution clause `procedure IFoo.Method = Impl;` in class body
  unhandled (`parser.rs:1364`; `beBarSelectBox.pas`). Fix: after the member
  name, if next is `Dot`, parse qualified name + optional `= <ident>` target.
- [x] H8 `const_value` under-records dependencies (`if_eval.rs:137`) — records
  only the unit where found, unlike `is_declared` (`:92`) which records all
  walked. Editing a shadowing const in a later-used unit never invalidates the
  importer → stale cache. Fix: record dependency for every `Loaded` artifact
  walked.
- [x] H9 Dependency stamps omit the dependency's own `.inc` includes
  (`unit_cache.rs:149`, `parse_state.rs:186`). Editing a dep's include never
  invalidates the importer (watcher + load paths). Fix: carry the dep's include
  stamps in `Dependency`/`SavedDependency`, validate + add to `watched_files`;
  or reverse-transitively invalidate importers of an invalidated dep.
- [x] H10 Windows file DELETE misses invalidation (`watcher.rs:47`): `path_key`
  canonicalize fails on delete → no `\\?\` prefix → key mismatch vs index →
  stale cache served (single delete never triggers FullSweep). Fix: lexical
  normalization in `path_key` (absolutize + strip `\\?\` + fold), no canonicalize.
- [x] H11 `Failed` units absent from reverse index (`driver.rs:279`,
  `watcher.rs:145`). Fixing a broken `.inc` never clears the dependent's stale
  failure until a burst/restart. Fix: record reverse edges for `Failed` entries
  (source + consulted includes/imports), or drop `Failed` on unmatched PerFile.

## MEDIUM

- [x] M1 moka weigher ignores `interface.symbols`/`members` (`unit_cache.rs:199`)
  — dominant memory since v5. 512MB cap enforced against fiction → no eviction →
  OOM at scale. Fix: add symbols + per-symbol members to `estimated_bytes`.
- [x] M2 `into_artifact` indexes `file_ids[file_index]` unchecked
  (`unit_cache.rs:596/619/642`) → panic on corrupt snapshot (violates
  corrupt≠missing). Fix: `.get(idx).ok_or(FileReadError)?` at all three sites.
- [x] M3 `begin_unit` without matching `end_unit` on parse failure
  (`parser.rs:125`, `pipeline.rs:197`) → leaked chain stack → false `Cycle` for
  namespace-resolved units. Fix: `end_unit` in an RAII guard / the `Err` arm.
- [x] M4 UTF-16BE BOM (`FE FF`) silently ANSI-mojibake'd (`source.rs:168`). Fix:
  BE decode branch or distinct `FileReadError`.
- [x] M5 if_eval rejects `%bin`/`&oct`/`#char` literals (`if_eval.rs:209`) → unit
  abort on valid `{$IF}`. Fix: add those literal arms to `tokenize`.
- [x] M6 Unary `negate` overflow on `i64::MIN` (`if_eval.rs:719`) → panic. Fix:
  `checked_neg`.
- [x] M7 `arena.text`/`location_text` panic on register-only (persisted) files
  (`source.rs:112`). Fix: route through `content()`, return `Result`.
- [x] M8 `{$IFOPT}` ignores project switch options (`context.rs:201`). Fix: map
  `DCC_*` switch options into `default_switches`.
- [x] M9 Generic type params + constraints discarded (`parser.rs:551`
  `skip_generic_parameters`) → no hover/rename on `T`. Fix: capture
  `Vec<GenericParameter{name, constraints_span}>` on declarations.
- [x] M10 ddk stdout UTF-8 BOM breaks all JSON (`ddk.rs:107`). Fix: strip BOM /
  `from_utf8_lossy`.
- [x] M11 Legacy/fractional CompilerVersion aborts whole compiler list
  (`ddk.rs:30`, `u32` can't hold `18.5`). Fix: `Option<f64>` + skip bad rows.
- [x] M12 DFM surrogate-pair `#$` codes fail whole parse (`dfm.rs:335`). Fix:
  combine high+low surrogate `#$` pairs before `char::from_u32`.
- [x] M13 FullSweep rebuilds index without flushing moka pending tasks
  (`driver.rs:246`). Fix: `run_pending_tasks()` before `rebuild_index`.

## LOW / LEDGER (verify vs dcc, then fix-or-ledger)

- [LEDGER] L1 Digit separators. VERIFIED: Delphi (incl. 12) has NO digit
  separator (that is a Rust/C# feature) — so the lexer correctly excludes `_`
  from numeric literals, and the parser's `text.replace('_', "")` never sees a
  `_` inside a number: dead but harmless. `1_000` lexes as `1` + ident `_000`,
  which dcc also rejects. PLAN: drop the misleading `replace('_')` +comment in a
  cleanup pass; no behavioral change. No correctness risk — left as-is for now.
- [x] L2 `{$IFDEF FOO BAR}` interns whole remainder (`token_cursor.rs:238`) →
  never matches `FOO`. Fix: first whitespace token.
- [x] L3 `>=` fusion not handled in type-argument position (`parser.rs:1690`) —
  `TArray<Byte>=(...)`. Fix: mirror `skip_generic_parameters`.
- [x] L4 `class property`/`class const` class-level flag lost (`parser.rs:1228`).
- [x] L5 Generic args on ancestor parsed then discarded (`parser.rs:1005`).
- [x] L6 Large unsigned const `$FFFFFFFFFFFFFFFF` (it. 14). Added
  `ConstantValue::UInt(u64)` (transparent serde picks it up — no
  `SavedConstantValue` mirror since it. 9); `parse_integer_literal` now returns
  `ConstantValue`, trying `u64` on `i64` overflow (still-too-big → None, never a
  bit-cast). `if_eval` gained `Value::UInt`/`Token::UInt` and does EXACT
  mixed-width comparison/arithmetic via `i128` (narrowed back to the tightest
  Int/UInt; a result fitting neither → Unknown). The if_eval tokenizer's
  hex/decimal/binary/octal paths retry as u64 on overflow. Format bumped
  **v10 → v11**; old-version-reject test updated (rejects v10 cleanly). Bonus:
  `1 shl 63` no longer wraps to a negative i64 (now exact UInt 2^63). Tests:
  `large_unsigned_constants_evaluate`, `large_unsigned_constant_captured_and_
  round_trips`, `cross_unit_large_unsigned_constant_evaluates`.
- [LEDGER] L7 `$CODEPAGE`/`-cp` ignored; `CP_ACP` always; no
  `MB_ERR_INVALID_CHARS` (`source.rs`). DEFERRED: real-world impact is small —
  BOM'd files (UTF-8/UTF-16) already decode correctly, and no-BOM source on a
  German dev box matches `CP_ACP`. Chicken-and-egg: the `{$CODEPAGE n}` directive
  lives in the bytes being decoded. PLAN: pre-scan the ASCII-safe head for
  `{$CODEPAGE}` / honor the integrator's `-cp` (a `CompilerProfile.code_page`),
  pass that code page to `MultiByteToWideChar`, and set `MB_ERR_INVALID_CHARS`
  to surface undecodable bytes instead of silently substituting.
- [x] L8 UTF-16LE odd trailing byte silently dropped (`source.rs:172`).
- [x] L9 `ß`/non-ASCII fold divergence (it. 14). Introduced ONE
  `globals::fold_identifier()` — ordinal ASCII fold (`a`–`z` → `A`–`Z`),
  non-ASCII bytes BYTE-IDENTICAL (validity-preserving: only ASCII bytes are
  rewritten). Every folded-key producer routes through it: `intern_key`,
  `context::is_defined`, `unit_resolution::resolve_unit`, `if_eval::size_of`
  builtin-type fold, `if_eval::const_value` builtin-name fold, `layout` builtin
  lookup. Behaviour-PRESERVING for ASCII (identical bytes to the old
  `to_uppercase`; existing dual-track tests stay green); for non-ASCII it never
  produces a WRONG match (`ß`≠`SS`, `ö`≠`Ö`) and the two tracks can no longer
  diverge. JUSTIFIED exceptions that stay: directive/switch KEYWORD dispatch
  (`token_cursor` IFDEF/ALIGN/letter — fixed reserved ASCII, not identifiers);
  `%VAR%` pseudo-include name (env-var/build-metadata, not a Delphi identifier);
  `eq_ignore_ascii_case` unit-name compares in `if_eval` (already exactly the
  ordinal ASCII fold — behaviourally identical to `fold_identifier` equality).
  NOTE re-ledgered: verifying dcc's ACTUAL non-ASCII fold is unresolved; the
  SAFE ordinal-ASCII + byte-identical choice (never a wrong match) is chosen and
  documented on `fold_identifier`. Tests: `fold_identifier_is_ordinal_ascii`,
  `fold_identifier_leaves_non_ascii_byte_identical`, `non_ascii_tracks_agree_
  through_one_fold`, `non_ascii_define_ifdef_round_trip_is_consistent`.
- [x] L10 `RTLVersion` aliased to `CompilerVersion` (it. 14). Added
  `CompilerProfile.rtl_version: Option<f64>` (`None` = same as
  `compiler_version`, the modern-Delphi default) and `ProjectContext.rtl_version:
  f64` (resolved at `from_dproj`). `const_value` returns `rtl_version` for
  `RTLVERSION` and `compiler_version` for `COMPILERVERSION` — evaluated
  independently, no longer hard-aliased (still both 36 for Delphi 12). Test:
  `rtl_version_and_compiler_version_evaluate_independently` (divergent profile).
- [LEDGER] L11 `$ELSEIF` accepted after `$IFDEF`/`$IFOPT` (leniency). DEFERRED:
  this only accepts input dcc would already reject — the safe direction for a
  tolerant analysis parser. PLAN (if strict diagnostics are wanted): tag each
  conditional frame with its opener kind and reject `$ELSEIF` unless the frame
  is an `$IF` chain; surface as a diagnostic, not a hard parse abort.
- [x] L12 invalidations don't bump `insert_count` — VERIFIED NON-ISSUE at the
  integrated level: `Session::apply_plan` sets `dirty = true` directly from the
  invalidation report count (`revalidate()` counts dropped entries), NOT from
  `insert_count`, so a pure-invalidation tick still autosaves. `insert_count` is
  only used to detect nested inserts during a parse. No change needed.
- [LEDGER] L13 alias target interned case-folded, effective name uppercased
  (`context.rs:246`, `unit_resolution.rs:33`). DEFERRED: harmless on Windows
  (case-insensitive FS) and the effective name is only re-folded into a cache
  key, so display casing loss doesn't matter. Only bites a case-sensitive mount
  (network share / WSL). PLAN: store the alias TARGET on the display track and
  fold only for the map key, so `resolve_unit` probes the real-cased filename.
- [x] L14 snapshot temp file uniqueness is pid-only (`cache_store.rs:106`).
- [x] L15 TOCTOU + redundant re-read in `stamp_file` (it. 14). `SourceArena`
  now RETAINS the raw on-disk bytes it read per file (`SourceEntry.raw:
  OnceLock<Vec<u8>>`, filled alongside the decoded string in `content()`) and
  exposes `raw_bytes(FileId) -> Option<&[u8]>`. `stamp_file` hashes those — one
  read, no TOCTOU window. The hash INPUT is byte-identical to `hash_file(path)`
  (same raw bytes, same `hash_bytes`), so existing snapshots still validate.
  Virtual buffers have no raw bytes → `raw_bytes` None → stamp falls back to
  hashing decoded content (never matches a disk read, dropped stale on load —
  the intended #21/#25 behaviour, preserved). A not-yet-materialized disk file
  falls back to `hash_file` (same bytes). Stable-`&str` arena guarantee intact
  (raw is a separate `OnceLock`, never handed out as `&str`). Tests:
  `source_stamp_hashes_raw_bytes_for_ansi_and_utf16`, `raw_bytes_retained_after_
  disk_read`, `virtual_files_and_spans` (raw None).
- [x] L16 DFM property named `Object`/`Inline`/`Inherited` misparsed. Fixed via
  `starts_child_object` (`=`-lookahead disambiguates property from header).
  Note: `Item` was not a false trigger here (not in the structural keyword set).
- [x] L17 unbounded watcher quiescence defer. Fixed: `max_defer` ceiling (default
  5s) forces a flush under continuous churn.
- [LEDGER] L18 open window between snapshot load and watcher start
  (`driver.rs`). DEFERRED: tiny window; load-time hash validation covers changes
  BEFORE open, and the first FullSweep covers anything after. PLAN: start the
  watcher BEFORE `load_into` and buffer events, or run one reconciliation sweep
  immediately after the watcher starts. Low likelihood, safe direction.
- [LEDGER] L19 `ddk` `.cmd`/`.bat` PATHEXT resolution (`ddk.rs`). DEFERRED:
  speculative — depends on how ddk ships. Rust's `Command` finds `ddk.exe` but
  not a `ddk.cmd` shim. PLAN: if ddk ships as a shim, resolve via `PATHEXT` /
  invoke through `cmd /c`; verify against the real install first.
- [x] CONFIG define-coverage (it. 14). The stress harness now parses each source
  under the dproj that GOVERNS it: a package/program (`.dpk`/`.dpr`) uses its
  sibling `<stem>.dproj` (e.g. `be.core.gui.dproj` next to `be.core.gui.dpk`,
  which defines `BE_CORE_D11_USES`), unit `.pas` files keep the top-level
  be.dproj — all via `from_dproj`, so real active-config defines flow through.
  The last failure (`be.core.gui.dpk`'s intentionally-noncompilable German-prose
  dead branch) is resolved: the tree now parses cleanly (464 units + 4
  program/pkg, **0 failures**). The regression guard asserts `total_failures ==
  0` so a genuine grammar failure can no longer hide behind a dead-branch
  excuse. `main.rs` already builds its context via `from_dproj` (its
  hand-written list is the compiler's auto-defines, not project defines).

## Verified correct (no action)
- `layout.rs` size table vs LLP64 (field-by-field).
- Dual-track interning discipline across cache/resolver/loader key paths
  (except the corrupt-before-fold H1 and low L13).
- Serialization re-intern/re-register on load (no raw Spur/FileId persisted).
- Loop progress in every parse loop (no infinite loops); `>>` fusion; Kleene
  short-circuit; Delphi operator precedence; arena `&str` stability; DFM string
  continuation (#24) and binary-DFM detection (#23).
