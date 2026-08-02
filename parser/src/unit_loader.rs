//! [`InterfaceLoader`] implementation: serves "give me the parsed version of
//! unit X" requests. Fulfillment order: cache hit → cycle check → resolve
//! source → parse (recursively wired to itself, so nested `Declared()`
//! queries force further units on demand).
//!
//! One instance per top-level parse chain (`Rc`): the cycle stack is
//! chain-local. Concurrent chains share only the process-wide cache, where
//! a duplicate parse is the accepted benign race.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::context::{Identifier, ProjectContext};
use crate::parse_state::{InterfaceLoader, LoadOutcome};
use crate::pipeline::parse_and_cache;
use crate::source::SourceArena;
use crate::unit_cache::CacheEntry;
use crate::unit_resolution::resolve_unit;

/// The default per-chain transitive-load budget for a [`LoadMode::Full`] parse
/// ([`UnitLoader::with_store`] / [`UnitLoader::new`]) — the cap that keeps a
/// `didSave`, a navigation/query cross-unit resolution and a batch/indexing
/// parse from cascading into the WHOLE `{$IF Declared/SizeOf}` dependency
/// closure (be.core + the RTL/VCL source tree → the ~14GB Task-15/21 OOM).
///
/// A Full parse may force-load AT MOST this many units transitively across the
/// entire nested chain (a shared counter — see [`UnitLoader::loads_done`]);
/// beyond it a cache MISS that would parse degrades to [`LoadOutcome::NotFound`]
/// (→ Unknown → the safe AssumeFalse tri-state), NEVER a fabricated answer. This
/// lets go-to-def load the target plus a bounded few, and `didSave` parse the
/// saved unit plus a bounded closure, at a peak RAM of ~N × per-unit instead of
/// the whole closure. Cross-unit precision beyond N degrades to Unknown and
/// recovers as the cache warms (future batch indexing).
///
/// Chosen at 32: comfortably above a typical single unit's directly-forced
/// `{$IF Declared/SizeOf}` set (which is a handful), so real navigation and save
/// stay precise, while orders of magnitude below the thousands of units in the
/// full RTL/VCL/be.core closure that caused the OOM.
pub const MAX_TRANSITIVE_LOADS: usize = 32;

/// How the loader answers a cache MISS in [`UnitLoader::interface_of`] —
/// generalized from the Task-21 `Full`/`ResidentOnly` two-state into a per-chain
/// transitive-load BUDGET.
///
/// - [`LoadMode::Budgeted(n)`]: on a miss, reload the unit's AST from its
///   per-unit snapshot (hash-validated) or, failing that, resolve the source,
///   read it and PARSE it (which recursively force-loads that unit's own `{$IF
///   Declared/SizeOf}` dependency closure) — BUT only while fewer than `n` such
///   force-loads have happened on THIS chain (the shared [`UnitLoader::loads_done`]
///   counter). Once the budget is exhausted a miss-that-would-parse degrades to
///   [`LoadOutcome::NotFound`] (→ Unknown → safe AssumeFalse), NEVER a fabricated
///   result. A CACHE HIT is always served for free and never counts against the
///   budget. This bounds every `didSave` / navigation / batch parse's transitive
///   force-load closure (the Task-15/21 OOM fix); the default budget is
///   [`MAX_TRANSITIVE_LOADS`].
///
/// [`LoadMode::ResidentOnly`] (the editor buffer parse,
/// [`crate::driver::ProjectSession::parse_buffer`]) is exactly `Budgeted(0)`:
/// a miss NEVER parses, answering ONLY from what is already resident in the moka
/// RAM cache — so opening a unit never cascades into force-loading its whole
/// cross-unit closure. For clarity it stays a named constructor
/// ([`UnitLoader::with_store_resident_only`]) but shares the single budget path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// A cache miss may reload/parse until `loads_done` reaches this budget,
    /// then degrades to [`LoadOutcome::NotFound`]. `Budgeted(0)` is the
    /// resident-only editor behavior (never parses on a miss).
    Budgeted(usize),
}

impl LoadMode {
    /// The full miss-resolution budget for `didSave` / navigation / batch parses.
    pub const FULL: LoadMode = LoadMode::Budgeted(MAX_TRANSITIVE_LOADS);
    /// Resident-only: never parse on a miss (editor buffer parse, Task-15/21).
    pub const RESIDENT_ONLY: LoadMode = LoadMode::Budgeted(0);

    /// The transitive-load budget this mode permits.
    fn budget(self) -> usize {
        match self {
            LoadMode::Budgeted(budget) => budget,
        }
    }
}

pub struct UnitLoader {
    /// The process-global arena (`&'static`). Nested parses register their
    /// files here so serialized `FileId`s resolve consistently on save.
    arena: &'static SourceArena,
    context: Arc<ProjectContext>,
    /// Unit keys on THIS chain's parse stack (declared names, registered by
    /// the parser via `begin_unit` — includes the top-level unit).
    active_units: RefCell<Vec<Identifier>>,
    /// Files currently being parsed on this chain. Guards recursion that
    /// name-based checks cannot see, e.g. a `{$IF Declared(...)}` BEFORE the
    /// `unit` header (no name registered yet).
    active_files: RefCell<Vec<crate::meta::FileId>>,
    /// Weak self-reference so nested parses receive the same loader.
    self_reference: std::rc::Weak<UnitLoader>,
    /// When present, nested artifacts are registered here — otherwise units
    /// parsed as import side effects would be invisible to file-change
    /// invalidation (a MISSED invalidation, the unsafe direction).
    reverse_index: Option<Arc<crate::watcher::ReverseDependencyIndex>>,
    /// The durable per-unit store (Task 16). On a cache MISS the loader tries to
    /// RELOAD the unit's AST from its per-unit file (hash-validated) BEFORE
    /// re-parsing from source: an evicted unit reloads cheaply, and source is
    /// re-read/re-parsed only when the file hash changed. `None` for batch
    /// parses / tests with no durable store — those always re-parse on a miss.
    store: Option<Arc<crate::cache_store::CacheStore>>,
    /// Miss-resolution mode (see [`LoadMode`]): the per-chain transitive-load
    /// budget. [`LoadMode::FULL`] (budget [`MAX_TRANSITIVE_LOADS`]) for every
    /// constructor except [`Self::with_store_resident_only`], which the editor
    /// buffer parse ([`crate::driver::ProjectSession::parse_buffer`]) uses with
    /// [`LoadMode::RESIDENT_ONLY`] (budget 0 — never parses on a miss).
    mode: LoadMode,
    /// Transitive force-loads spent on THIS parse chain so far. Shared across the
    /// WHOLE nested chain (the self-referential `Rc` hands the SAME `UnitLoader`
    /// to every nested parse), so a `{$IF Declared}` five levels deep counts
    /// against the same budget as the top-level parse — the cascade is bounded
    /// end-to-end, not per-level. A CACHE HIT never touches this; only a miss
    /// that actually reloads-from-disk or parses increments it. `Cell` because
    /// the loader is shared behind `&self` on a single (non-`Send`) chain.
    loads_done: Cell<usize>,
}

impl UnitLoader {
    pub fn new(
        arena: &'static SourceArena,
        context: Arc<ProjectContext>,
        reverse_index: Option<Arc<crate::watcher::ReverseDependencyIndex>>,
    ) -> Rc<Self> {
        Self::with_store(arena, context, reverse_index, None)
    }

    /// Like [`Self::new`], but also threads the durable [`CacheStore`] so a
    /// cache miss reloads from the per-unit file before re-parsing (Task 16 C).
    /// [`LoadMode::FULL`] — a bounded transitive-load budget of
    /// [`MAX_TRANSITIVE_LOADS`] (didSave, navigation/query, batch parses).
    pub fn with_store(
        arena: &'static SourceArena,
        context: Arc<ProjectContext>,
        reverse_index: Option<Arc<crate::watcher::ReverseDependencyIndex>>,
        store: Option<Arc<crate::cache_store::CacheStore>>,
    ) -> Rc<Self> {
        Self::with_store_and_mode(arena, context, reverse_index, store, LoadMode::FULL)
    }

    /// Like [`Self::with_store`], but in [`LoadMode::ResidentOnly`]: a cache miss
    /// answers [`LoadOutcome::NotFound`] WITHOUT reloading-from-disk, resolving,
    /// arena-loading or parsing. This is the EDITOR buffer parse loader
    /// ([`crate::driver::ProjectSession::parse_buffer`]): opening a unit parses
    /// ONLY that unit — its cross-unit `{$IF Declared/SizeOf}` that are not
    /// already resident answer Unknown (safe AssumeFalse), never force-parsing
    /// the closure (Task-15 OOM). A resident hit is served exactly as in Full
    /// mode (including recording the dependency).
    pub fn with_store_resident_only(
        arena: &'static SourceArena,
        context: Arc<ProjectContext>,
        reverse_index: Option<Arc<crate::watcher::ReverseDependencyIndex>>,
        store: Option<Arc<crate::cache_store::CacheStore>>,
    ) -> Rc<Self> {
        Self::with_store_and_mode(arena, context, reverse_index, store, LoadMode::RESIDENT_ONLY)
    }

    fn with_store_and_mode(
        arena: &'static SourceArena,
        context: Arc<ProjectContext>,
        reverse_index: Option<Arc<crate::watcher::ReverseDependencyIndex>>,
        store: Option<Arc<crate::cache_store::CacheStore>>,
        mode: LoadMode,
    ) -> Rc<Self> {
        Rc::new_cyclic(|weak| Self {
            arena,
            context,
            active_units: RefCell::new(Vec::new()),
            active_files: RefCell::new(Vec::new()),
            self_reference: weak.clone(),
            reverse_index,
            store,
            mode,
            loads_done: Cell::new(0),
        })
    }

    /// Transitive force-loads spent on this chain so far (test/diagnostic
    /// accessor). A CACHE HIT never increments this; only a miss that actually
    /// reloads-from-disk or parses does — so it is the exact count the budget
    /// caps, and the load-cap bound-proof test asserts against it.
    pub fn loads_done(&self) -> usize {
        self.loads_done.get()
    }
}

impl InterfaceLoader for UnitLoader {
    fn interface_of(&self, unit_key: Identifier) -> LoadOutcome {
        if let Some(entry) = self.context.unit_cache.get(unit_key) {
            return match entry {
                CacheEntry::Done(artifact) => LoadOutcome::Loaded(artifact),
                CacheEntry::Failed(_) => LoadOutcome::Failed,
            };
        }

        if self.active_units.borrow().contains(&unit_key) {
            return LoadOutcome::Cycle;
        }

        // Per-chain transitive-load BUDGET (Task-15/21/22). The cache HIT above
        // was served free; reaching here means this miss would reload-from-disk
        // or parse — a transitive force-load that materializes another unit's
        // closure. Cap it: once this chain has spent its budget, degrade to
        // NotFound (→ Unknown → safe AssumeFalse) WITHOUT reload/resolve/arena/
        // parse, so a `didSave`/navigation/batch parse can never cascade into the
        // whole `{$IF Declared/SizeOf}` closure (be.core + RTL/VCL → the 14GB
        // OOM). `Budgeted(0)` (RESIDENT_ONLY, editor buffer parse) is the special
        // case where the FIRST miss is already over budget, so nothing is ever
        // force-loaded — exactly the Task-21 resident-only behavior. The cycle
        // check ABOVE runs first, so cycle-safety is unaffected by exhaustion; a
        // resident import is still served (and recorded) regardless of budget.
        // A budget-exhausted miss is NEVER a wrong answer, only a less-precise
        // Unknown that recovers as the cache warms.
        if self.loads_done.get() >= self.mode.budget() {
            return LoadOutcome::NotFound;
        }
        // Charge this force-load against the chain budget BEFORE performing it, so
        // the reload-from-disk path below (which also materializes a unit) counts
        // exactly like a re-parse. A cache hit never reaches here, so a hit is
        // never charged (cache-hit-is-free).
        self.loads_done.set(self.loads_done.get() + 1);

        // Lazy reload-from-disk (Task 16 C): BEFORE resolving the name and
        // re-parsing from source, try the per-unit snapshot. `load_unit`
        // hash-validates (own source + dfm + includes + dependencies); a
        // Some means the AST is on disk AND still fresh — reload it, NO
        // re-parse, NO source read. A None (no file / corrupt / hash changed)
        // falls through to the parse path below, so source is read only when
        // the file actually changed. Keyed by the requested unit's folded name;
        // an alias whose declared name differs simply misses here and re-parses
        // (never a wrong answer).
        if let Some(store) = &self.store {
            let requested_key_name = crate::globals::resolve(unit_key).to_string();
            if let Some(meta) = store.load_unit(&requested_key_name) {
                // Reinsert into the RAM cache under both the declared key and the
                // requested key (they usually coincide) so subsequent hits are
                // in-memory. Use `insert_durable`, NOT `insert`: this meta came
                // straight from `load_unit`, which only returns a hash-VALID meta
                // read from its per-unit file — the file already exists and
                // re-validates, so re-persisting it (full serialize + temp-write +
                // rename) would be a redundant write of identical bytes. The alias
                // path (name != key) must not write twice either; both inserts are
                // durable-skip (write-amplification fix, Task 16).
                self.context.unit_cache.insert_durable(meta.name(), meta.clone());
                if meta.name() != unit_key {
                    self.context.unit_cache.insert_durable(unit_key, meta.clone());
                }
                if let Some(index) = &self.reverse_index {
                    index.index_artifact(meta.name(), &meta);
                }
                return LoadOutcome::Loaded(meta);
            }
        }

        let requested_name = crate::globals::resolve(unit_key).to_string();
        let Some(resolved) = resolve_unit(&self.context, &requested_name) else {
            return LoadOutcome::NotFound;
        };
        // `SysUtils` may resolve to `System.SysUtils` — check the effective
        // name against the chain too
        let effective_key = self.context.intern_key(&resolved.effective_name);
        if effective_key != unit_key && self.active_units.borrow().contains(&effective_key) {
            return LoadOutcome::Cycle;
        }

        let file = match self.arena.load(&resolved.path) {
            Ok(file) => file,
            Err(_) => return LoadOutcome::NotFound,
        };
        if self.active_files.borrow().contains(&file) {
            return LoadOutcome::Cycle;
        }

        let loader: Rc<dyn InterfaceLoader> = self
            .self_reference
            .upgrade()
            .expect("loader is alive while serving a request");

        self.active_files.borrow_mut().push(file);
        // Cross-unit import: loaded for its INTERFACE only. Do NOT retain the
        // body — an imported unit is never the active editor unit, so its body is
        // dead weight; retaining it for every import load (RTL/VCL closure) is the
        // 20 GB OOM this fixes. Interface + flat usages (cross-unit references)
        // are kept regardless.
        let result = parse_and_cache(&self.arena, &self.context, file, Some(loader), false);
        self.active_files.borrow_mut().pop();

        match result {
            Ok((_, Some(meta))) => {
                // requested `SysUtils`, unit declares `System.SysUtils`:
                // alias the requested key to the same meta. `parse_and_cache`
                // ALREADY inserted (and persisted) this meta under its declared
                // name `meta.name()`; the per-unit file is addressed by that
                // declared name, so aliasing must use `insert_durable` — a plain
                // `insert` here would re-run `save_unit` and write the IDENTICAL
                // file a second time (the alias double-write, Task 16).
                if meta.name() != unit_key {
                    self.context.unit_cache.insert_durable(unit_key, meta.clone());
                }
                if let Some(index) = &self.reverse_index {
                    index.index_artifact(meta.name(), &meta);
                }
                LoadOutcome::Loaded(meta)
            }
            // resolved file was not a unit (program/library) — nothing
            // importable lives there
            Ok((_, None)) => LoadOutcome::NotFound,
            Err(error) => {
                self.context
                    .unit_cache
                    .insert_failed(unit_key, Arc::new(error));
                LoadOutcome::Failed
            }
        }
    }

    fn begin_unit(&self, unit_key: Identifier) {
        self.active_units.borrow_mut().push(unit_key);
    }

    fn end_unit(&self, unit_key: Identifier) {
        let mut active = self.active_units.borrow_mut();
        if let Some(position) = active.iter().rposition(|&key| key == unit_key) {
            active.remove(position);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Source;
    use crate::context::{DefineSet, Interner, SwitchState, TargetPlatform};
    use crate::pipeline;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_context(search_paths: Vec<PathBuf>) -> Arc<ProjectContext> {
        Arc::new(ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths,
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: crate::unit_cache::UnitCache::default(),
        })
    }

    fn temp_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir()
            .join("delphi_parser_unit_loader")
            .join(name);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn declaration_names(meta: &crate::unit_meta::UnitMeta) -> Vec<String> {
        meta.ast
            .interface_declarations
            .iter()
            .map(|declaration| crate::globals::resolve(declaration.name.name).to_string())
            .collect()
    }

    /// The full lazy-import loop: B's `{$IF Declared(Alpha)}` forces A's
    /// interface parse mid-directive and takes the branch.
    #[test]
    fn declared_forces_lazy_import_parse() {
        let directory = temp_directory("lazy");
        std::fs::write(
            directory.join("UnitA.pas"),
            "unit UnitA; interface const Alpha = 1; implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UnitB.pas"),
            "unit UnitB;\ninterface\nuses UnitA;\n\
             {$IF Declared(Alpha)} const HasAlpha = True; {$IFEND}\n\
             {$IF Declared(Missing)} const Wrong = True; {$IFEND}\n\
             implementation\nend.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);

        let file = arena.load(directory.join("UnitB.pas")).unwrap();
        let (outcome, artifact) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();

        // branch taken: Alpha found in lazily-parsed UnitA; Missing → false
        // (all imports resolvable → confident false, no diagnostic policy hit)
        let artifact = artifact.unwrap();
        assert_eq!(declaration_names(&artifact), ["HasAlpha"]);
        let _ = &outcome; // diagnostics/byproducts still available
        // UnitA landed in the cache as a side effect
        assert!(context.unit_cache.get(context.intern_key("UNITA")).is_some());
        // and is recorded as a dependency of B's artifact
        assert_eq!(artifact.dependencies.len(), 1);
        assert_eq!(
            artifact.dependencies[0].unit,
            context.intern_key("UNITA")
        );
    }

    /// Interface uses-cycle: Declared over the cycle answers Unknown
    /// (policy AssumeFalse) instead of hanging or crashing.
    #[test]
    fn interface_cycle_degrades_to_unknown() {
        let directory = temp_directory("cycle");
        std::fs::write(
            directory.join("UnitA.pas"),
            "unit UnitA; interface uses UnitB;\n\
             {$IF Declared(FromB)} const X = 1; {$IFEND}\n\
             const FromA = 1; implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UnitB.pas"),
            "unit UnitB; interface uses UnitA;\n\
             {$IF Declared(FromA)} const FromB = 1; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);

        let file = arena.load(directory.join("UnitA.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        // parse completes; the cycle-dependent branch fell to AssumeFalse
        // and left a diagnostic trail
        let names = declaration_names(&meta.unwrap());
        assert!(names.contains(&"FromA".to_string()));
        assert!(!names.contains(&"X".to_string()));
    }

    #[test]
    fn missing_import_makes_declared_unknown_not_false_positive() {
        let directory = temp_directory("missing_import");
        std::fs::write(
            directory.join("UnitB.pas"),
            "unit UnitB; interface uses NowhereUnit;\n\
             {$IF Declared(Ghost)} const Wrong = 1; {$ELSE} const Safe = 1; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);

        let file = arena.load(directory.join("UnitB.pas")).unwrap();
        let (outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        assert_eq!(declaration_names(&meta.unwrap()), ["Safe"]);
        assert!(!outcome.diagnostics.is_empty(), "Unknown must leave a diagnostic");
    }

    /// Cross-unit constant values in `{$IF}`: captured literals evaluate,
    /// computed constants stay Unknown (policy), shadowing respected.
    #[test]
    fn cross_unit_constant_values() {
        let directory = temp_directory("const_values");
        std::fs::write(
            directory.join("Config.pas"),
            "unit Config; interface\n\
             const ApiVersion = 3;\n\
             const NegOffset = -2;\n\
             const Greeting = 'hello';\n\
             const HexMask = $FF;\n\
             const Computed = 1 + 2;\n\
             implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Consumer.pas"),
            "unit Consumer; interface uses Config;\n\
             {$IF ApiVersion >= 3} const V3 = True; {$IFEND}\n\
             {$IF NegOffset < 0} const Neg = True; {$IFEND}\n\
             {$IF Greeting = 'hello'} const Greeted = True; {$IFEND}\n\
             {$IF HexMask = 255} const Masked = True; {$IFEND}\n\
             {$IF Computed = 3} const Wrong = True; {$ELSE} const NotCaptured = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);

        let file = arena.load(directory.join("Consumer.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        // Computed = 1 + 2 is not a single literal → Unknown → AssumeFalse
        // (honest: the value EXISTS but is not capturable yet — diagnostic)
        assert_eq!(
            declaration_names(&meta.unwrap()),
            ["V3", "Neg", "Greeted", "Masked", "NotCaptured"]
        );
    }

    /// resolve_qualified_type consistency (task-4 review): a qualified
    /// `SizeOf(Unit.TRec)` must still resolve when an UNRELATED, earlier-scanned
    /// import is missing. `uses RealUnit, NowhereUnit;` scans in reverse uses
    /// order, so `NowhereUnit` (missing) is visited BEFORE `RealUnit`; the old
    /// code aborted to Unknown on that first unresolvable import. Now the scan
    /// skips it and finds the named unit — never wrong (still only ever a size
    /// from the name-matching unit), just no longer needlessly Unknown.
    #[test]
    fn qualified_sizeof_resolves_past_unrelated_missing_import() {
        let directory = temp_directory("qualified_sizeof_missing");
        std::fs::write(
            directory.join("RealUnit.pas"),
            "unit RealUnit; interface\n\
             type TRec = record A: Integer; B: Integer; end;\n\
             implementation end.",
        )
        .unwrap();
        // NowhereUnit is deliberately absent from disk. It is LAST in `uses`, so
        // FIRST in the reversed scan — the exact position that used to abort.
        std::fs::write(
            directory.join("Consumer.pas"),
            "unit Consumer; interface uses RealUnit, NowhereUnit;\n\
             {$IF SizeOf(RealUnit.TRec) = 8} const Sized = True; {$ELSE} const Unsized = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("Consumer.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        // The qualified SizeOf resolved to 8 despite the missing NowhereUnit
        // being scanned first → `Sized` kept, `Unsized` not.
        assert_eq!(declaration_names(&meta.unwrap()), ["Sized"]);
    }

    /// L6: a captured large-unsigned constant (`$FFFFFFFFFFFFFFFF`) evaluates in
    /// a cross-unit `{$IF K = …}` — the UInt survives capture, cache and the
    /// mixed-width evaluator, and a value ONE bit smaller compares not-equal
    /// (a float round-trip would have collapsed them).
    #[test]
    fn cross_unit_large_unsigned_constant_evaluates() {
        let directory = temp_directory("uint_const");
        std::fs::write(
            directory.join("Bits.pas"),
            "unit Bits; interface\n\
             const AllBits = $FFFFFFFFFFFFFFFF;\n\
             implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UseBits.pas"),
            "unit UseBits; interface uses Bits;\n\
             {$IF AllBits = $FFFFFFFFFFFFFFFF} const Matched = True; {$IFEND}\n\
             {$IF AllBits = $FFFFFFFFFFFFFFFE} const Wrong = True; {$ELSE} const NotEqual = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("UseBits.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        assert_eq!(declaration_names(&meta.unwrap()), ["Matched", "NotEqual"]);
    }

    /// Scoped `Declared(OwnType.Member)`: the type is declared earlier in the
    /// SAME unit; a later `{$IF}` resolves the member against it (#19).
    #[test]
    fn scoped_declared_own_type_member() {
        let directory = temp_directory("scoped_own");
        std::fs::write(
            directory.join("UnitO.pas"),
            "unit UnitO;\ninterface\n\
             type TFoo = class FBar: Integer; procedure Go; end;\n\
             {$IF Declared(TFoo.FBar)} const HasBar = True; {$IFEND}\n\
             {$IF Declared(TFoo.Go)} const HasGo = True; {$IFEND}\n\
             {$IF Declared(TFoo.Nope)} const Wrong = True; {$ELSE} const NoNope = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("UnitO.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        let names = declaration_names(&meta.unwrap());
        // present direct members → true. `TFoo.Nope`: TFoo is a class, so it
        // can inherit (implicit TObject) → the missing member degrades to
        // Unknown, NOT a confident false (#19). Under the AssumeFalse policy
        // Unknown still takes the `{$ELSE}` branch, so `NoNope` appears and
        // `Wrong` does not — same observable outcome, honest reasoning.
        assert!(names.contains(&"HasBar".to_string()));
        assert!(names.contains(&"HasGo".to_string()));
        assert!(names.contains(&"NoNope".to_string()));
        assert!(!names.contains(&"Wrong".to_string()));
    }

    /// #19 blocker: `Declared(TChild.BaseMember)` where `BaseMember` is
    /// inherited from `TBase` (not a DIRECT member of `TChild`). A class can
    /// inherit, so the missing-from-direct-members lookup MUST degrade to
    /// Unknown — never a confident false that would silently flip which branch
    /// compiles. An ancestor-less record keeps the confident false.
    #[test]
    fn scoped_declared_inherited_member_is_not_false() {
        let directory = temp_directory("scoped_inherited");
        std::fs::write(
            directory.join("Inherit.pas"),
            "unit Inherit;\ninterface\n\
             type TBase = class BaseField: Integer; end;\n\
             type TChild = class(TBase) ChildField: Integer; end;\n\
             type TRec = record RecField: Integer; end;\n\
             {$IF Declared(TChild.ChildField)} const HasChild = True; {$IFEND}\n\
             {$IF Declared(TChild.BaseField)} const InheritedTrue = True; {$ELSE} const InheritedElse = True; {$IFEND}\n\
             {$IF Declared(TRec.Nope)} const RecWrong = True; {$ELSE} const RecFalse = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("Inherit.pas")).unwrap();
        let (outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        let names = declaration_names(&meta.unwrap());
        // direct member of the child → true
        assert!(names.contains(&"HasChild".to_string()));
        // The inherited member MUST NOT resolve to a confident false. Either
        // the base is fully resolved in-unit (→ True, `InheritedTrue`) or the
        // walk degrades to Unknown (→ `{$ELSE}` under AssumeFalse,
        // `InheritedElse`). Both are acceptable; a confident-false would be the
        // bug — that path is proven closed because a false would ALSO take the
        // else branch, so we additionally require a diagnostic on the Unknown
        // path OR the True-branch const, and forbid neither-branch states.
        let inherited_true = names.contains(&"InheritedTrue".to_string());
        let inherited_else = names.contains(&"InheritedElse".to_string());
        assert!(
            inherited_true || inherited_else,
            "inherited member must land on one branch"
        );
        // The load-bearing assertion: if it took the else branch it was Unknown
        // (a diagnostic is emitted), never a silent confident false.
        if inherited_else {
            assert!(
                !inherited_true,
                "inherited member cannot be both true and else"
            );
            assert!(
                !outcome.diagnostics.is_empty(),
                "an Unknown scoped Declared must leave a diagnostic — proves it \
                 was Unknown, not a silent confident false"
            );
        }
        // ancestor-less record: genuine absence stays a confident false
        assert!(names.contains(&"RecFalse".to_string()));
        assert!(!names.contains(&"RecWrong".to_string()));
    }

    /// Scoped `Declared(ImportedType.Member)`: the type lives in an imported
    /// unit; resolution forces the lazy parse, walks the member, AND records
    /// the import as a dependency (#19 — same staleness discipline as flat).
    #[test]
    fn scoped_declared_imported_type_member_records_dependency() {
        let directory = temp_directory("scoped_imported");
        std::fs::write(
            directory.join("Models.pas"),
            "unit Models;\ninterface\n\
             type TUser = class Name: string; Age: Integer; end;\n\
             implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Client.pas"),
            "unit Client;\ninterface\nuses Models;\n\
             {$IF Declared(TUser.Name)} const HasName = True; {$IFEND}\n\
             {$IF Declared(TUser.Missing)} const Wrong = True; {$ELSE} const NoMissing = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("Client.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        let meta = meta.unwrap();
        let names = declaration_names(&meta);
        assert!(names.contains(&"HasName".to_string()));
        // type resolved, member absent → confident false → else branch taken
        assert!(names.contains(&"NoMissing".to_string()));
        assert!(!names.contains(&"Wrong".to_string()));
        // the consulted import is recorded as a dependency
        assert!(
            meta.dependencies
                .iter()
                .any(|dependency| dependency.unit == context.intern_key("MODELS")),
            "Models must be recorded as a dependency of Client"
        );
    }

    /// #19: the IMPORTED-type walk must also degrade an inherited member to
    /// Unknown. `TChild` lives in an imported unit and descends from `TBase`;
    /// `Declared(TChild.BaseField)` must not be a confident false. A record in
    /// the same imported unit keeps its confident false for a genuine miss.
    #[test]
    fn scoped_declared_imported_inherited_member_is_not_false() {
        let directory = temp_directory("scoped_imported_inherited");
        std::fs::write(
            directory.join("Base.pas"),
            "unit Base;\ninterface\n\
             type TBase = class BaseField: Integer; end;\n\
             type TChild = class(TBase) ChildField: Integer; end;\n\
             type TRec = record RecField: Integer; end;\n\
             implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("User.pas"),
            "unit User;\ninterface\nuses Base;\n\
             {$IF Declared(TChild.ChildField)} const HasChild = True; {$IFEND}\n\
             {$IF Declared(TChild.BaseField)} const InheritedTrue = True; {$ELSE} const InheritedElse = True; {$IFEND}\n\
             {$IF Declared(TRec.Nope)} const RecWrong = True; {$ELSE} const RecFalse = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("User.pas")).unwrap();
        let (outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        let names = declaration_names(&meta.unwrap());
        assert!(names.contains(&"HasChild".to_string()));
        let inherited_true = names.contains(&"InheritedTrue".to_string());
        let inherited_else = names.contains(&"InheritedElse".to_string());
        assert!(inherited_true || inherited_else);
        if inherited_else {
            assert!(!inherited_true);
            assert!(
                !outcome.diagnostics.is_empty(),
                "imported inherited member Unknown must leave a diagnostic"
            );
        }
        // ancestor-less imported record: genuine miss stays confident false
        assert!(names.contains(&"RecFalse".to_string()));
        assert!(!names.contains(&"RecWrong".to_string()));
    }

    /// #19 alias gap (own unit): a type alias `TAlias = TBase` inherits TBase's
    /// ENTIRE member surface — both TBase's DIRECT members and anything TBase
    /// itself inherits. Because we do not (yet, #33) resolve the alias's
    /// Reference target to walk that surface, a member absent from the alias's
    /// (empty) direct declarations MUST degrade to Unknown, never a silent
    /// confident false. A `Distinct` type (`type Integer`) is the same shape.
    /// The ancestor-less record keeps its confident false to prove the
    /// distinction is preserved.
    #[test]
    fn scoped_declared_alias_member_is_not_false() {
        let directory = temp_directory("scoped_alias");
        std::fs::write(
            directory.join("Alias.pas"),
            "unit Alias;\ninterface\n\
             type TBase = class BaseField: Integer; end;\n\
             type TAlias = TBase;\n\
             type TDistinct = type Integer;\n\
             type TRec = record RecField: Integer; end;\n\
             {$IF Declared(TAlias.BaseField)} const DirectTrue = True; {$ELSE} const DirectElse = True; {$IFEND}\n\
             {$IF Declared(TAlias.Inherited)} const InhTrue = True; {$ELSE} const InhElse = True; {$IFEND}\n\
             {$IF Declared(TDistinct.Anything)} const DistTrue = True; {$ELSE} const DistElse = True; {$IFEND}\n\
             {$IF Declared(TRec.Nope)} const RecWrong = True; {$ELSE} const RecFalse = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("Alias.pas")).unwrap();
        let (outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        let names = declaration_names(&meta.unwrap());
        // A member of the aliased target — direct OR inherited — must NOT be a
        // confident false. Until #33 resolves the alias target we cannot return
        // a confident True either, so the honest result is Unknown → `{$ELSE}`
        // under AssumeFalse. The load-bearing signal that it was Unknown (not a
        // silent confident false) is the diagnostic trail.
        assert!(
            names.contains(&"DirectElse".to_string()) && !names.contains(&"DirectTrue".to_string()),
            "alias direct-target member must be Unknown (else branch), not confident true here"
        );
        assert!(
            names.contains(&"InhElse".to_string()) && !names.contains(&"InhTrue".to_string()),
            "alias inherited member must be Unknown (else branch)"
        );
        assert!(
            names.contains(&"DistElse".to_string()) && !names.contains(&"DistTrue".to_string()),
            "distinct-type member must be Unknown (else branch)"
        );
        // The invariant: an Unknown scoped Declared always leaves a diagnostic,
        // proving these else-branches came from Unknown and NOT a silent
        // confident false (the #19 corruption this fix kills for aliases).
        assert!(
            !outcome.diagnostics.is_empty(),
            "an Unknown alias/distinct scoped Declared must leave a diagnostic — \
             proves Unknown, not a silent confident false"
        );
        // ancestor-less record: genuine absence stays a confident false
        assert!(names.contains(&"RecFalse".to_string()));
        assert!(!names.contains(&"RecWrong".to_string()));
    }

    /// #19 alias gap (imported unit): same guarantee across the interface-index
    /// (`has_ancestors`) walk. `TAlias = TBase` in an imported unit must make
    /// both a direct-target and an inherited member of the alias degrade to
    /// Unknown, never a confident false. The imported record keeps its
    /// confident false.
    #[test]
    fn scoped_declared_imported_alias_member_is_not_false() {
        let directory = temp_directory("scoped_imported_alias");
        std::fs::write(
            directory.join("AliasLib.pas"),
            "unit AliasLib;\ninterface\n\
             type TBase = class BaseField: Integer; end;\n\
             type TAlias = TBase;\n\
             type TRec = record RecField: Integer; end;\n\
             implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("AliasUser.pas"),
            "unit AliasUser;\ninterface\nuses AliasLib;\n\
             {$IF Declared(TAlias.BaseField)} const DirectTrue = True; {$ELSE} const DirectElse = True; {$IFEND}\n\
             {$IF Declared(TAlias.Inherited)} const InhTrue = True; {$ELSE} const InhElse = True; {$IFEND}\n\
             {$IF Declared(TRec.Nope)} const RecWrong = True; {$ELSE} const RecFalse = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("AliasUser.pas")).unwrap();
        let (outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        let names = declaration_names(&meta.unwrap());
        assert!(
            names.contains(&"DirectElse".to_string()) && !names.contains(&"DirectTrue".to_string()),
            "imported alias direct-target member must be Unknown (else branch)"
        );
        assert!(
            names.contains(&"InhElse".to_string()) && !names.contains(&"InhTrue".to_string()),
            "imported alias inherited member must be Unknown (else branch)"
        );
        assert!(
            !outcome.diagnostics.is_empty(),
            "imported alias Unknown scoped Declared must leave a diagnostic"
        );
        assert!(names.contains(&"RecFalse".to_string()));
        assert!(!names.contains(&"RecWrong".to_string()));
    }

    /// Unresolvable first segment (unknown type / missing import) → Unknown,
    /// never a confident false (#19).
    #[test]
    fn scoped_declared_unresolved_first_segment_is_unknown() {
        let directory = temp_directory("scoped_unknown");
        std::fs::write(
            directory.join("UnitU.pas"),
            "unit UnitU; interface uses NowhereUnit;\n\
             {$IF Declared(TGhost.Field)} const Wrong = 1; {$ELSE} const Safe = 1; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("UnitU.pas")).unwrap();
        let (outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        // Unknown → AssumeFalse policy → else branch, plus a diagnostic
        assert_eq!(declaration_names(&meta.unwrap()), ["Safe"]);
        assert!(!outcome.diagnostics.is_empty(), "Unknown must leave a diagnostic");
    }

    /// Nested `Declared(A.B.C)`: walk a member's type to its nested member.
    /// One positive (`TOuter.Inner.Leaf`) and one negative (`.Nope`).
    #[test]
    fn scoped_declared_nested_three_segments() {
        let directory = temp_directory("scoped_nested");
        std::fs::write(
            directory.join("Nested.pas"),
            "unit Nested;\ninterface\n\
             type TInner = class Leaf: Integer; end;\n\
             type TOuter = class Inner: TInner; end;\n\
             {$IF Declared(TOuter.Inner.Leaf)} const HasLeaf = True; {$IFEND}\n\
             {$IF Declared(TOuter.Inner.Nope)} const Wrong = True; {$ELSE} const NoNope = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("Nested.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader), true).unwrap();
        let names = declaration_names(&meta.unwrap());
        assert!(names.contains(&"HasLeaf".to_string()));
        assert!(names.contains(&"NoNope".to_string()));
        assert!(!names.contains(&"Wrong".to_string()));
    }

    /// Second parse of the same importer hits the cache — no re-parse of A.
    #[test]
    fn cache_hit_on_second_request() {
        let directory = temp_directory("cache_hit");
        std::fs::write(
            directory.join("UnitA.pas"),
            "unit UnitA; interface const Alpha = 1; implementation end.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);

        let key = context.intern_key("UNITA");
        let LoadOutcome::Loaded(first) = loader.interface_of(key) else {
            panic!("expected load");
        };
        let LoadOutcome::Loaded(second) = loader.interface_of(key) else {
            panic!("expected cache hit");
        };
        assert!(Arc::ptr_eq(&first, &second));
    }

    /// Write a straight chain of `count` units `Chain0..Chain{count-1}` where each
    /// `ChainI` (except the last) `uses ChainI+1` and, via a
    /// `{$IF Declared(Marker{I+1})}` directive, force-parses `ChainI+1`
    /// mid-interface. So a Full parse of `Chain0` transitively force-loads the
    /// ENTIRE tail — one per link — an unbounded cascade unless the budget caps
    /// it. `MarkerI` is declared in `ChainI`, so the directive in `ChainI-1`
    /// resolves only after `ChainI` is parsed.
    fn write_force_load_chain(directory: &std::path::Path, count: usize) {
        for index in 0..count {
            let marker = format!("const Marker{index} = True;");
            let source = if index + 1 < count {
                format!(
                    "unit Chain{index};\ninterface\nuses Chain{next};\n\
                     {marker}\n\
                     {{$IF Declared(Marker{next})}} const Reached{next} = True; {{$IFEND}}\n\
                     implementation\nend.",
                    next = index + 1
                )
            } else {
                format!("unit Chain{index};\ninterface\n{marker}\nimplementation\nend.")
            };
            std::fs::write(directory.join(format!("Chain{index}.pas")), source).unwrap();
        }
    }

    /// BOUND PROOF (Task-22): a Full parse whose transitive `{$IF Declared}`
    /// closure is LARGER than the budget force-loads AT MOST `budget` units — the
    /// didSave / navigation RAM bound. Uses an explicit small budget so the test
    /// is fast and the cap is unambiguous; the far tail of the chain is proven
    /// ABSENT from the cache (never force-loaded), and `loads_done` is capped.
    #[test]
    fn full_parse_transitive_load_is_capped_at_budget() {
        let directory = temp_directory("budget_cap");
        // A 40-link chain: strictly longer than the 8-unit budget below, so the
        // cascade would run 39 transitive loads without the cap.
        let chain_length = 40;
        write_force_load_chain(&directory, chain_length);

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let budget = 8usize;
        let loader = UnitLoader::with_store_and_mode(
            arena,
            context.clone(),
            None,
            None,
            LoadMode::Budgeted(budget),
        );

        let file = arena.load(directory.join("Chain0.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader.clone()), true).unwrap();
        // Chain0 itself parsed successfully.
        assert!(meta.is_some());

        // The transitive force-loads are capped at the budget — never the whole
        // 39-unit tail. This is the load-cap that bounds peak RAM.
        assert!(
            loader.loads_done() <= budget,
            "transitive loads {} exceeded budget {budget}",
            loader.loads_done()
        );

        // The NEAR closure (within budget) landed in the cache; the FAR tail is
        // absent — proving the cascade was truncated, not merely counted. Chain1
        // (the first forced load) is present; a deep link past the budget is not.
        assert!(
            context.unit_cache.get(context.intern_key("CHAIN1")).is_some(),
            "the first forced import must load (under budget)"
        );
        let far = format!("CHAIN{}", chain_length - 1);
        assert!(
            context.unit_cache.get(context.intern_key(&far)).is_none(),
            "the far tail {far} must be absent — the cascade was capped, not run to the end"
        );

        // The count of distinct forced ChainI units in the cache never exceeds the
        // budget (Chain0 is the explicitly-parsed root, not a transitive load).
        let cached_chain_units = (1..chain_length)
            .filter(|index| {
                context
                    .unit_cache
                    .get(context.intern_key(&format!("CHAIN{index}")))
                    .is_some()
            })
            .count();
        assert!(
            cached_chain_units <= budget,
            "{cached_chain_units} forced units cached, budget was {budget}"
        );
    }

    /// The existing small force-load chain (well under `MAX_TRANSITIVE_LOADS`)
    /// still fully resolves under the DEFAULT Full budget — the didSave /
    /// navigation precision that must survive the cap. A 5-link chain (4
    /// transitive loads ≪ 32) reaches its end: every `Reached{I}` marker appears.
    #[test]
    fn small_chain_fully_resolves_under_default_budget() {
        let directory = temp_directory("budget_small");
        let chain_length = 5;
        write_force_load_chain(&directory, chain_length);

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        // Default Full budget (MAX_TRANSITIVE_LOADS) — the didSave/navigation path.
        let loader = UnitLoader::new(arena, context.clone(), None);

        let file = arena.load(directory.join("Chain0.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader.clone()), true).unwrap();
        assert!(meta.is_some());

        // Under budget, the whole tail resolved: the last link is in the cache and
        // the load count equals the chain's transitive length (4), well under 32.
        let far = format!("CHAIN{}", chain_length - 1);
        assert!(
            context.unit_cache.get(context.intern_key(&far)).is_some(),
            "small chain must resolve to its end under the default budget"
        );
        assert!(loader.loads_done() <= MAX_TRANSITIVE_LOADS);
        assert_eq!(loader.loads_done(), chain_length - 1);
    }

    /// RESIDENT-ONLY (budget 0, the editor buffer parse) force-loads NOTHING on a
    /// miss: parsing `Chain0` leaves its imported `Chain1` ABSENT from the cache
    /// (the Task-21 behavior, now expressed as `Budgeted(0)`). `loads_done`
    /// stays 0.
    #[test]
    fn resident_only_loads_nothing_on_a_miss() {
        let directory = temp_directory("budget_resident");
        write_force_load_chain(&directory, 3);

        let context = test_context(vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::with_store_resident_only(arena, context.clone(), None, None);

        let file = arena.load(directory.join("Chain0.pas")).unwrap();
        let (_outcome, meta) =
            pipeline::parse_and_cache(&arena, &context, file, Some(loader.clone()), true).unwrap();
        // Chain0 parses (its own buffer), but its `{$IF Declared(Marker1)}` could
        // not force-load Chain1 — that import stays out of the cache.
        assert!(meta.is_some());
        assert_eq!(loader.loads_done(), 0, "resident-only must force-load nothing");
        assert!(
            context.unit_cache.get(context.intern_key("CHAIN1")).is_none(),
            "resident-only must not pull Chain1 into the cache on a miss"
        );
    }
}
