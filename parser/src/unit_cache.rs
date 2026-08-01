//! Unit-level cache of parse artifacts, moka-backed.
//!
//! Cached per unit: the interface (what importers see), an implementation
//! index (positions, usages — "where is this symbol used"), the source hash
//! and the hashes of every dependency consulted during the parse. Eviction is
//! size-aware (moka TinyLFU with a byte weigher) — at 8M LOC nothing here may
//! assume "everything stays resident".
//!
//! Persistence: [`UnitCache::save`] / [`UnitCache::load`] write a bincode
//! snapshot. Interned [`Identifier`]s and session-local [`FileId`]s are not
//! stable across processes, so the saved form stores resolved strings and
//! paths; loading re-interns and re-registers (lazily — no source is read).
//! Every unit is hash-validated against the current file contents on load;
//! stale entries are silently dropped and will re-parse on demand.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::context::Identifier;
use crate::meta::CodeLocation;
use crate::parser::ParseError;
use crate::unit_meta::UnitMeta;

const CACHE_FORMAT_VERSION: u32 = 11;
const DEFAULT_CAPACITY_BYTES: u64 = 512 * 1024 * 1024;

pub fn hash_bytes(bytes: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(bytes)
}

/// Hash of a file's raw on-disk bytes (before any decoding), the validity
/// stamp for cached artifacts.
pub fn hash_file(path: impl AsRef<Path>) -> std::io::Result<u64> {
    Ok(hash_bytes(&std::fs::read(path)?))
}

// ─── Cached artifact ─────────────────────────────────────────────────────

/// What kind of thing an interface symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Type,
    Const,
    ResourceString,
    Var,
    ThreadVar,
    Procedure,
    Function,
}

/// Compile-time value of a simple constant (`const MaxThings = 3;`) —
/// captured when the initializer is a single literal, `None` for anything
/// computed. Feeds `{$IF SomeConst > 2}` across units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ConstantValue {
    Int(i64),
    /// A constant whose value exceeds `i64::MAX` but fits `u64`
    /// (`$FFFFFFFFFFFFFFFF`, `18446744073709551615`). Delphi's `UInt64`/`NativeUInt`
    /// range. Captured as `UInt` rather than bit-cast to a negative `i64` (which
    /// would be silent corruption) — a value that fits neither stays `None`
    /// (Unknown). Feeds mixed-width `{$IF}` comparisons via the evaluator.
    UInt(u64),
    Float(f64),
    Bool(bool),
    /// Display-interned literal content (unquoted). Serializes as its string
    /// through the global interner (transparent `Identifier` serde).
    Str(Identifier),
}

/// What kind of thing a type member is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberKind {
    Field,
    Method,
    Property,
    NestedType,
    NestedConst,
}

/// One member of a structured type (class/record/interface), flattened for
/// completion ("ctrl+space after `.`"), scoped `Declared(T.M)` and dfm
/// linking. Type structure itself lives in the AST; the artifact carries
/// the queryable surface.
#[derive(Debug, Clone)]
pub struct MemberSymbol {
    pub name: Identifier,
    pub key: Identifier,
    pub kind: MemberKind,
    pub location: CodeLocation,
    /// Property `read` target key — dfm handler/field linking.
    pub read_target: Option<Identifier>,
    /// Property `write` target key.
    pub write_target: Option<Identifier>,
    /// The member's declared type as a SIMPLE type reference key (field type,
    /// property type, method return type). `None` for anonymous/complex types
    /// (inline records, arrays, procedural types); the full `TypeExpression`
    /// stays in the AST for callers that need the structure. Feeds scoped
    /// `Declared(A.B.C)`: `C`'s type is found via `B`'s `type_key`.
    pub type_key: Option<Identifier>,
    /// Method directive keys (`virtual`/`override`/`abstract`/`stdcall`/
    /// `message`/…), folded, in source order. Empty for non-methods.
    pub directives: Vec<Identifier>,
    /// Visibility of the enclosing section (`Unspecified` = the type's default,
    /// resolved by a later semantic stage). Records/interfaces carry
    /// `Unspecified` for their single flat section.
    pub visibility: crate::ast::Visibility,
    /// `strict` modifier of the enclosing section: `strict private` /
    /// `strict protected` set this. Only meaningful with `Private`/`Protected`
    /// visibility; `false` everywhere else. Kept distinct from `visibility` so
    /// a later semantic stage can enforce strict scoping (unit-visible vs.
    /// type-only).
    pub strict: bool,
    /// Attribute name keys (`[Weak]` → `WEAK`) on this member, source order.
    pub attributes: Vec<Identifier>,
}

/// One name an importing unit can see.
#[derive(Debug, Clone)]
pub struct InterfaceSymbol {
    /// Display track (as written).
    pub name: Identifier,
    /// Lookup track (case-folded) — all `Declared()`/completion lookups.
    pub key: Identifier,
    pub kind: SymbolKind,
    pub location: CodeLocation,
    /// Only for [`SymbolKind::Const`] with a single-literal initializer.
    pub constant_value: Option<ConstantValue>,
    /// For type symbols: their members (methods, fields, properties, …).
    pub members: Vec<MemberSymbol>,
    /// Attribute name keys on the declaration (`[Entity]` → `ENTITY`), source
    /// order. Empty for declarations without attributes.
    pub attributes: Vec<Identifier>,
    /// May this type inherit members from a base? `true` for every class and
    /// interface — a class implicitly descends from `TObject` and an interface
    /// from `IInterface` even with an empty ancestor list, and either may name
    /// explicit (possibly cross-unit) ancestors. `false` only for types that
    /// cannot inherit members at all (records, enums, aliases, …). Scoped
    /// `Declared(Type.Member)` must degrade a not-directly-found member to
    /// Unknown — never a confident `false` — whenever this is `true`, because
    /// the member could be inherited from an ancestor we do not flatten here
    /// (ledger #19).
    pub has_ancestors: bool,
}

impl InterfaceSymbol {
    pub fn find_member(&self, key: Identifier) -> Option<&MemberSymbol> {
        self.members.iter().find(|member| member.key == key)
    }
}

/// Parsed interface section of a unit: everything an importing unit can see.
/// Shared via `Arc` — many importers across tasks, one instance.
#[derive(Debug)]
pub struct UnitInterface {
    pub name: Identifier,
    /// Declaration order preserved (matters for completion ranking later).
    pub symbols: Vec<InterfaceSymbol>,
}

impl UnitInterface {
    pub fn contains_key(&self, key: Identifier) -> bool {
        self.symbols.iter().any(|symbol| symbol.key == key)
    }

    pub fn find(&self, key: Identifier) -> Option<&InterfaceSymbol> {
        self.symbols.iter().find(|symbol| symbol.key == key)
    }
}

/// One recorded occurrence of a symbol, for usage queries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Usage {
    pub symbol: Identifier,
    pub location: CodeLocation,
}

/// A unit whose interface was consulted while parsing this one (its symbols
/// may have decided an `{$IF}` or a layout). If its source changed, this
/// artifact is stale even though our own file did not change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub unit: Identifier,
    pub source_path: PathBuf,
    pub source_hash: u64,
    /// The dependency's own `{$I}` include stamps. Its interface can be shaped
    /// by a conditionally-guarded const/symbol in one of these, so editing a
    /// dependency's include must invalidate THIS unit too — not just the
    /// dependency. Without this, a `.inc` edit staled the dependency but the
    /// importer validated only against the dependency's `.pas` hash (unchanged)
    /// and served a stale interface.
    pub includes: Vec<SourceStamp>,
}

/// A file that contributed bytes to a parse: `{$I}` includes. If any of them
/// changed, the artifact is stale — same rule as the main source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStamp {
    pub path: PathBuf,
    pub hash: u64,
}

#[derive(Debug, Clone)]
pub enum CacheEntry {
    Done(Arc<UnitMeta>),
    /// Failures are cached too: a broken unit is reported once per importer
    /// but never re-parsed (until invalidated).
    Failed(Arc<ParseError>),
}

// ─── Cache ───────────────────────────────────────────────────────────────

/// Size-aware cache of unit artifacts, keyed by case-folded unit name.
///
/// No `InProgress` state: cross-task duplicate parses are a benign race
/// (last writer wins, results equivalent) and uses-cycle detection lives on
/// each parse's own DFS stack, not here.
pub struct UnitCache {
    entries: moka::sync::Cache<Identifier, CacheEntry>,
    /// Exact insert counter. moka's `entry_count` is eventually consistent —
    /// unusable for "did this parse add anything" decisions (dirty tracking).
    inserts: std::sync::atomic::AtomicU64,
}

impl Default for UnitCache {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY_BYTES)
    }
}

impl std::fmt::Debug for UnitCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnitCache")
            .field("entries", &self.entries.entry_count())
            .finish()
    }
}

impl UnitCache {
    pub fn with_capacity(max_bytes: u64) -> Self {
        let entries = moka::sync::Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|_, entry: &CacheEntry| match entry {
                CacheEntry::Done(meta) => meta.estimated_bytes(),
                CacheEntry::Failed(_) => 64,
            })
            .build();
        Self {
            entries,
            inserts: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn get(&self, unit: Identifier) -> Option<CacheEntry> {
        self.entries.get(&unit)
    }

    pub fn insert(&self, unit: Identifier, meta: Arc<UnitMeta>) {
        self.entries.insert(unit, CacheEntry::Done(meta));
        self.inserts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn insert_failed(&self, unit: Identifier, error: Arc<ParseError>) {
        self.entries.insert(unit, CacheEntry::Failed(error));
        self.inserts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Monotonic count of inserts ever made — exact, unlike `entry_count`.
    pub fn insert_count(&self) -> u64 {
        self.inserts.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn invalidate(&self, unit: Identifier) {
        self.entries.invalidate(&unit);
    }

    pub fn invalidate_all(&self) {
        self.entries.invalidate_all();
    }

    /// Drop every `Failed` entry, returning the count. Failures carry no file
    /// stamps and are not reverse-indexed, so per-file invalidation cannot
    /// reach them; callers drop them to force a re-parse after any edit that
    /// might be the fix (safe direction — a failure only re-derives).
    /// Drop every `Failed` entry, returning the keys that were dropped. The
    /// caller appends these to `invalidated_keys` so the driver's purge loop
    /// covers them for symmetry/insurance — even though `Failed` entries are
    /// never indexed today, a later derived side-table keyed off a failed unit
    /// must not be missed.
    pub fn invalidate_failed(&self) -> Vec<Identifier> {
        self.entries.run_pending_tasks();
        let mut dropped = Vec::new();
        for (unit_key, entry) in self.iter_entries() {
            if matches!(entry, CacheEntry::Failed(_)) {
                self.invalidate(unit_key);
                dropped.push(unit_key);
            }
        }
        dropped
    }

    pub fn entry_count(&self) -> u64 {
        self.entries.entry_count()
    }

    /// Force moka to apply pending inserts/invalidations so `iter_entries`
    /// reflects them (moka is otherwise eventually consistent — ledger #22).
    /// Call before any decision that reads the entry set, e.g. rebuilding the
    /// reverse index after a sweep.
    pub fn run_pending_tasks(&self) {
        self.entries.run_pending_tasks();
    }

    /// Snapshot iteration over current entries (moka iterator is weakly
    /// consistent — fine for revalidation sweeps and persistence).
    pub fn iter_entries(&self) -> impl Iterator<Item = (Identifier, CacheEntry)> + '_ {
        self.entries.iter().map(|(unit, entry)| (*unit, entry))
    }

    // ─── Persistence ─────────────────────────────────────────────────────

    /// Write all successful entries to `path`. The whole [`UnitMeta`] (AST +
    /// stamps + deps + usages) is bincoded; interned [`Identifier`]s and
    /// [`FileId`]s inside it serialize transparently as strings/paths through
    /// the process-global interner and arena, so the snapshot is process
    /// independent with no hand-written mirror. Cycle-tainted metas are never
    /// persisted (best-effort in-session only).
    ///
    /// Panic-free per unit (M2): a meta that cannot be serialized — e.g. it
    /// holds a `FileId` the global arena never issued, or an `Identifier` from a
    /// foreign interner generation — is skipped, not fatal. The returned
    /// [`SaveReport`] mirrors the load side's [`LoadReport`]: it reports both the
    /// number of segments actually written AND every skipped meta (by unit name
    /// and serde error), so a driver can surface the drop instead of it being
    /// swallowed silently.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<SaveReport, CachePersistError> {
        // moka is eventually consistent (ledger #22): without this, entries
        // inserted moments ago may be invisible to iter() and silently
        // missing from the snapshot
        self.entries.run_pending_tasks();
        let mut metas: Vec<Arc<UnitMeta>> = Vec::new();
        for (_, entry) in self.entries.iter() {
            let CacheEntry::Done(meta) = entry else {
                continue; // failures are not worth persisting
            };
            if meta.cycle_tainted || meta.recovered {
                // best-effort in-session only, never durable: a cycle-tainted OR
                // error-recovered parse is INCOMPLETE and must not persist as a
                // clean interface (a fresh full parse may differ). Same gate for
                // both — the never-wrong discipline for the durable cache.
                continue;
            }
            metas.push(meta);
        }
        // Each meta is bincoded into its OWN byte segment. This is what makes
        // the load panic-free per unit (M2): a corrupt/unregisterable segment
        // is dropped in isolation, the rest still load. The SAVE side mirrors
        // it: a meta that fails to serialize — e.g. it carries a `FileId` the
        // global arena never issued (foreign arena reaching this `pub` path) —
        // is skipped in isolation, never aborting the whole snapshot and never
        // panicking. Such an entry simply re-parses on demand next session.
        let mut segments: Vec<Vec<u8>> = Vec::with_capacity(metas.len());
        let mut skipped: Vec<SkippedUnit> = Vec::new();
        for meta in &metas {
            match bincode::serialize(meta.as_ref()) {
                Ok(segment) => segments.push(segment),
                Err(error) => {
                    // Dropping the meta is the correct recovery (re-parses on
                    // demand next session), but the drop must be VISIBLE — record
                    // it so the driver can log/inspect it, symmetric with the
                    // load side's `LoadReport`. Resolve the unit name through
                    // `try_resolve`: the very reason a meta fails here can be a
                    // foreign `Identifier`, so a plain `resolve` would itself
                    // panic — fall back to a placeholder.
                    let name = crate::globals::interner()
                        .try_resolve(&meta.name().spur())
                        .unwrap_or("<unresolved-identifier>")
                        .to_string();
                    skipped.push(SkippedUnit {
                        name,
                        error: error.to_string(),
                    });
                }
            }
        }
        let written = segments.len();
        let saved = SavedCacheDisk {
            version: CACHE_FORMAT_VERSION,
            units: segments,
        };
        let bytes = bincode::serialize(&saved).map_err(|error| CachePersistError {
            message: error.to_string(),
        })?;
        std::fs::write(path.as_ref(), bytes).map_err(|error| CachePersistError {
            message: error.to_string(),
        })?;
        Ok(SaveReport { written, skipped })
    }

    /// Load a snapshot, hash-validating every unit (own source AND all recorded
    /// dependencies, incl. their includes) against current file contents. Stale
    /// or unreadable entries are dropped — they re-parse on demand. Referenced
    /// files are only *registered* in the arena (lazily, via transparent
    /// `FileId` deserialization), never read.
    ///
    /// Panic-free: a corrupt snapshot (unregisterable path, malformed bytes)
    /// surfaces as a per-unit deserialize error counted `unreadable`, never a
    /// panic (M2). The whole-file bincode is decoded to a header + per-unit
    /// byte segments so one corrupt unit does not poison the rest.
    pub fn load(&self, path: impl AsRef<Path>) -> Result<LoadReport, CachePersistError> {
        let bytes = std::fs::read(path.as_ref()).map_err(|error| CachePersistError {
            message: error.to_string(),
        })?;
        // Decode into an owned, per-unit-segmented header first, so a single
        // corrupt unit degrades to `unreadable` rather than failing the whole
        // load. `FileId`/`Identifier` inside a segment re-register/re-intern on
        // demand when that segment is decoded.
        let saved: SavedCacheDisk =
            bincode::deserialize(&bytes).map_err(|error| CachePersistError {
                message: error.to_string(),
            })?;
        if saved.version != CACHE_FORMAT_VERSION {
            return Err(CachePersistError {
                message: format!(
                    "cache format version {} (expected {CACHE_FORMAT_VERSION})",
                    saved.version
                ),
            });
        }

        let mut report = LoadReport::default();
        for segment in saved.units {
            // A unit's `FileId`s deserialize by re-registering their paths; an
            // unregisterable path (deleted / virtual buffer) is a clean serde
            // error → count unreadable, never panic (M2, #21, #25).
            let meta: UnitMeta = match bincode::deserialize(&segment) {
                Ok(meta) => meta,
                Err(_) => {
                    report.unreadable += 1;
                    continue;
                }
            };
            match validate_meta(&meta) {
                Validity::Fresh => {
                    self.insert(meta.name(), Arc::new(meta));
                    report.loaded += 1;
                }
                Validity::Stale => report.stale += 1,
                Validity::Unreadable => report.unreadable += 1,
            }
        }
        Ok(report)
    }
}

enum Validity {
    Fresh,
    Stale,
    Unreadable,
}

/// Hash-validate a loaded meta against current file contents: own source,
/// includes, dependency sources and their includes. Any mismatch → `Stale`;
/// any missing/unreadable file → `Unreadable`.
fn validate_meta(meta: &UnitMeta) -> Validity {
    match hash_file(&meta.source_path) {
        Ok(hash) if hash == meta.source_hash => {}
        Ok(_) => return Validity::Stale,
        Err(_) => return Validity::Unreadable,
    }
    // sibling dfm: a form edit must stale the unit exactly like an include edit
    if let Some(dfm) = &meta.dfm {
        match hash_file(&dfm.path) {
            Ok(hash) if hash == dfm.hash => {}
            Ok(_) => return Validity::Stale,
            Err(_) => return Validity::Unreadable,
        }
    }
    for include in &meta.includes {
        match hash_file(&include.path) {
            Ok(hash) if hash == include.hash => {}
            Ok(_) => return Validity::Stale,
            Err(_) => return Validity::Unreadable,
        }
    }
    for dependency in &meta.dependencies {
        match hash_file(&dependency.source_path) {
            Ok(hash) if hash == dependency.source_hash => {}
            Ok(_) => return Validity::Stale,
            Err(_) => return Validity::Unreadable,
        }
        // a dependency's include shapes its interface → validate them too (H9)
        for include in &dependency.includes {
            match hash_file(&include.path) {
                Ok(hash) if hash == include.hash => {}
                Ok(_) => return Validity::Stale,
                Err(_) => return Validity::Unreadable,
            }
        }
    }
    Validity::Fresh
}

#[derive(Debug)]
pub struct CachePersistError {
    pub message: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LoadReport {
    pub loaded: usize,
    /// Source or dependency hash mismatch — file changed since the snapshot.
    pub stale: usize,
    /// Source or dependency file missing/unreadable.
    pub unreadable: usize,
}

/// One meta that `save` could not serialize and therefore dropped. Recorded so
/// the drop is visible (symmetric with `LoadReport`), never silently swallowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedUnit {
    /// Case-folded unit key, or `<unresolved-identifier>` if the unit's own name
    /// Identifier is itself foreign (the failure that caused the skip).
    pub name: String,
    /// The serde error that caused the skip (e.g. foreign FileId/Identifier).
    pub error: String,
}

/// Outcome of [`UnitCache::save`]. Mirrors [`LoadReport`]: `written` is the
/// number of metas persisted, `skipped` names every meta dropped because it
/// could not be serialized. A non-empty `skipped` is not fatal (the units
/// re-parse on demand next session) but MUST be surfaced, never swallowed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SaveReport {
    pub written: usize,
    pub skipped: Vec<SkippedUnit>,
}

// ─── Saved (process-independent) form ────────────────────────────────────
//
// The persisted form is simply a version tag plus one bincode-serialized
// `UnitMeta` byte segment per unit. `Identifier`s and `FileId`s inside a
// segment serialize transparently as strings/paths through the global interner
// and arena (see `crate::context::Identifier` and `crate::meta::FileId`), so
// there is NO hand-written mirror struct: the old `SavedSymbol`/`SavedMember`/
// `SavedUnit` machinery is gone. Per-segment bytes keep the load panic-free
// (a single corrupt/unregisterable unit is dropped in isolation — M2).

#[derive(Serialize, Deserialize)]
struct SavedCacheDisk {
    version: u32,
    /// One bincoded `UnitMeta` per unit.
    units: Vec<Vec<u8>>,
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{QualifiedName, Unit};
    use crate::meta::Span;

    fn write_temp(directory: &Path, name: &str, content: &str) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Minimal `UnitMeta` (through the GLOBAL interner/arena) with one
    /// dependency and one implementation usage — the same shape the old
    /// `build_artifact` produced, now built as a `UnitMeta`.
    fn build_meta(unit_path: &Path, dependency_path: &Path) -> UnitMeta {
        let arena = crate::globals::arena();
        let file = arena.load(unit_path).unwrap();
        let unit_name = QualifiedName {
            name: crate::globals::intern("UnitA"),
            key: crate::globals::intern_key("UnitA"),
            location: CodeLocation { file, span: Span::new(0, 4) },
        };
        let ast = Unit {
            name: unit_name,
            interface_uses: None,
            interface_declarations: Vec::new(),
            implementation_uses: None,
        };
        UnitMeta::new(
            ast,
            false,
            unit_path.to_path_buf(),
            hash_file(unit_path).unwrap(),
            Vec::new(),
            vec![Dependency {
                unit: crate::globals::intern_key("UnitB"),
                source_path: dependency_path.to_path_buf(),
                source_hash: hash_file(dependency_path).unwrap(),
                includes: Vec::new(),
            }],
            vec![Usage {
                symbol: crate::globals::intern_key("TFoo"),
                location: CodeLocation { file, span: Span::new(0, 4) },
            }],
        )
    }

    #[test]
    fn save_load_roundtrip_with_hash_validation() {
        let directory = std::env::temp_dir().join("delphi_parser_unit_cache_roundtrip");
        let unit_path = write_temp(&directory, "UnitA.pas", "unit UnitA;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");
        let snapshot = directory.join("cache.bin");

        // session 1: fill + save
        {
            let cache = UnitCache::default();
            let meta = build_meta(&unit_path, &dependency_path);
            cache.insert(meta.name(), Arc::new(meta));
            assert_eq!(cache.save(&snapshot).unwrap().written, 1);
        }

        // session 2: fresh cache, load (globals persist — that IS the design)
        let cache = UnitCache::default();
        let report = cache.load(&snapshot).unwrap();
        assert_eq!(
            report,
            LoadReport {
                loaded: 1,
                stale: 0,
                unreadable: 0
            }
        );

        let unit = crate::globals::intern_key("UnitA");
        let Some(CacheEntry::Done(meta)) = cache.get(unit) else {
            panic!("expected cached meta");
        };
        assert_eq!(meta.usages.len(), 1);
        // location points at a lazily-registered file: content readable on demand
        let location = meta.usages[0].location;
        assert_eq!(crate::globals::arena().content(location.file).unwrap(), "unit UnitA;");
    }

    #[test]
    fn changed_source_is_stale_on_load() {
        let directory = std::env::temp_dir().join("delphi_parser_unit_cache_stale");
        let unit_path = write_temp(&directory, "UnitA.pas", "unit UnitA;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");
        let snapshot = directory.join("cache.bin");

        {
            let cache = UnitCache::default();
            let meta = build_meta(&unit_path, &dependency_path);
            cache.insert(meta.name(), Arc::new(meta));
            cache.save(&snapshot).unwrap();
        }

        std::fs::write(&unit_path, "unit UnitA; // edited").unwrap();

        let cache = UnitCache::default();
        let report = cache.load(&snapshot).unwrap();
        assert_eq!(report.stale, 1);
        assert_eq!(report.loaded, 0);
        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn changed_dependency_is_stale_on_load() {
        let directory = std::env::temp_dir().join("delphi_parser_unit_cache_dep_stale");
        let unit_path = write_temp(&directory, "UnitA.pas", "unit UnitA;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");
        let snapshot = directory.join("cache.bin");

        {
            let cache = UnitCache::default();
            let meta = build_meta(&unit_path, &dependency_path);
            cache.insert(meta.name(), Arc::new(meta));
            cache.save(&snapshot).unwrap();
        }

        std::fs::write(&dependency_path, "unit UnitB; // interface changed").unwrap();

        let cache = UnitCache::default();
        let report = cache.load(&snapshot).unwrap();
        assert_eq!(report.stale, 1);
        assert_eq!(report.loaded, 0);
    }

    #[test]
    fn changed_dependency_include_is_stale_on_load() {
        // H9: the dependency's `.pas` is untouched, but its `{$I}` include
        // changed — the importer must still be dropped as stale.
        let directory = std::env::temp_dir().join("delphi_parser_dep_include_stale");
        let unit_path = write_temp(&directory, "UnitA.pas", "unit UnitA;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");
        let dependency_include = write_temp(&directory, "shared.inc", "const K = 1;");
        let snapshot = directory.join("cache.bin");

        {
            let cache = UnitCache::default();
            let mut meta = build_meta(&unit_path, &dependency_path);
            meta.dependencies[0].includes.push(SourceStamp {
                path: dependency_include.clone(),
                hash: hash_file(&dependency_include).unwrap(),
            });
            cache.insert(meta.name(), Arc::new(meta));
            cache.save(&snapshot).unwrap();
        }

        // only the dependency's include changes
        std::fs::write(&dependency_include, "const K = 2;").unwrap();

        let cache = UnitCache::default();
        let report = cache.load(&snapshot).unwrap();
        assert_eq!(report.stale, 1);
        assert_eq!(report.loaded, 0);
    }

    #[test]
    fn changed_dfm_is_stale_on_load() {
        // Deliverable B: a unit's sibling `.dfm` changed while its `.pas` is
        // untouched — the unit must be dropped as stale, exactly like an
        // include edit. Proves the dfm stamp participates in load validation.
        let directory = std::env::temp_dir().join("delphi_parser_dfm_stale");
        let unit_path = write_temp(&directory, "Form1.pas", "unit Form1;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");
        let dfm_path = write_temp(&directory, "Form1.dfm", "object Form1: TForm1\nend\n");
        let snapshot = directory.join("cache.bin");

        {
            let cache = UnitCache::default();
            let meta = build_meta(&unit_path, &dependency_path).with_dfm(Some(SourceStamp {
                path: dfm_path.clone(),
                hash: hash_file(&dfm_path).unwrap(),
            }));
            cache.insert(meta.name(), Arc::new(meta));
            assert_eq!(cache.save(&snapshot).unwrap().written, 1);
        }

        // roundtrip first: unchanged dfm still loads fresh
        {
            let cache = UnitCache::default();
            assert_eq!(cache.load(&snapshot).unwrap().loaded, 1);
        }

        // only the dfm changes → stale
        std::fs::write(&dfm_path, "object Form1: TForm1\n  Left = 5\nend\n").unwrap();
        let cache = UnitCache::default();
        let report = cache.load(&snapshot).unwrap();
        assert_eq!(report.stale, 1);
        assert_eq!(report.loaded, 0);
    }

    #[test]
    fn corrupt_segment_is_unreadable_not_panic() {
        // M2: a snapshot whose per-unit segment is corrupt must degrade to
        // "unreadable" (dropped), never panic. We corrupt one unit's bytes and
        // confirm the loader reports it unreadable without crashing.
        let directory = std::env::temp_dir().join("delphi_parser_corrupt_segment");
        let unit_path = write_temp(&directory, "UnitA.pas", "unit UnitA;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");
        let snapshot = directory.join("cache.bin");

        {
            let cache = UnitCache::default();
            let meta = build_meta(&unit_path, &dependency_path);
            cache.insert(meta.name(), Arc::new(meta));
            cache.save(&snapshot).unwrap();
        }

        // corrupt the single unit segment to unparseable bytes
        let mut saved: SavedCacheDisk = {
            let bytes = std::fs::read(&snapshot).unwrap();
            bincode::deserialize(&bytes).unwrap()
        };
        saved.units[0] = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        std::fs::write(&snapshot, bincode::serialize(&saved).unwrap()).unwrap();

        let cache = UnitCache::default();
        // must return a report (no panic); the corrupt unit counts as unreadable
        let report = cache.load(&snapshot).unwrap();
        assert_eq!(report.unreadable, 1);
        assert_eq!(report.loaded, 0);
    }

    #[test]
    fn old_version_snapshot_is_cleanly_rejected() {
        // A snapshot written by a PRIOR format version (here v10, one behind the
        // current v11) must be refused with a clean version-mismatch error — not
        // a panic, not a partial/garbage load. Bincode is not self-describing,
        // so an old snapshot's unit bytes may not even match the current
        // `UnitMeta` layout; the version guard must reject BEFORE any unit
        // segment is decoded.
        let directory = std::env::temp_dir().join("delphi_parser_old_version");
        std::fs::create_dir_all(&directory).unwrap();
        let snapshot = directory.join("cache.bin");

        // Write a snapshot stamped with the previous version, carrying a unit
        // segment of bytes that would NOT decode under the current `UnitMeta`
        // layout. The version guard must reject before any segment is touched,
        // so these bytes are never even reached.
        assert_eq!(CACHE_FORMAT_VERSION, 11, "update this test on a format bump");
        let stale = SavedCacheDisk {
            version: 10,
            units: vec![vec![0xDE, 0xAD, 0xBE, 0xEF]],
        };
        std::fs::write(&snapshot, bincode::serialize(&stale).unwrap()).unwrap();

        let cache = UnitCache::default();
        let result = cache.load(&snapshot);
        let error = result.expect_err("an old-version snapshot must be rejected");
        // the message names both the found and expected versions
        assert!(
            error.message.contains("10") && error.message.contains("11"),
            "version-mismatch message must name found (10) and expected (11): {}",
            error.message
        );
    }

    #[test]
    fn unregisterable_file_path_is_unreadable_not_panic() {
        // #21/#25/M2: a meta whose FileId path cannot be re-registered on load
        // (deleted between save and load) must degrade to unreadable, no panic.
        let directory = std::env::temp_dir().join("delphi_parser_unregisterable");
        let unit_path = write_temp(&directory, "UnitA.pas", "unit UnitA;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");
        let snapshot = directory.join("cache.bin");

        {
            let cache = UnitCache::default();
            let meta = build_meta(&unit_path, &dependency_path);
            cache.insert(meta.name(), Arc::new(meta));
            cache.save(&snapshot).unwrap();
        }
        // delete the file the usage's FileId points at — register() will fail
        std::fs::remove_file(&unit_path).unwrap();

        let cache = UnitCache::default();
        let report = cache.load(&snapshot).unwrap();
        // hash validation of the missing own source flags it unreadable and
        // drops it without panic — nothing loads.
        assert_eq!(report.loaded, 0);
        assert_eq!(report.unreadable, 1);
    }

    #[test]
    fn foreign_fileid_does_not_panic_on_save() {
        // HIGH/M2: a meta carrying a `FileId` the process-global arena never
        // issued (out-of-range index, e.g. built against a foreign arena and
        // reaching this `pub` save path) must NOT panic during serialization.
        // The whole `FileId::serialize` -> arena lookup used to `.expect(...)`,
        // aborting the entire `UnitCache::save`. Now it degrades: the bad meta
        // is skipped, the snapshot is written, no panic.
        let directory = std::env::temp_dir().join("delphi_parser_foreign_fileid");
        let good_unit_path = write_temp(&directory, "Good.pas", "unit Good;");
        let good_dependency = write_temp(&directory, "GoodDep.pas", "unit GoodDep;");
        let snapshot = directory.join("cache.bin");

        let cache = UnitCache::default();

        // a valid meta (all FileIds registered in the global arena)
        let good = {
            let arena = crate::globals::arena();
            let file = arena.load(&good_unit_path).unwrap();
            let name = QualifiedName {
                name: crate::globals::intern("Good"),
                key: crate::globals::intern_key("Good"),
                location: CodeLocation { file, span: Span::new(0, 4) },
            };
            UnitMeta::new(
                Unit {
                    name,
                    interface_uses: None,
                    interface_declarations: Vec::new(),
                    implementation_uses: None,
                },
                false,
                good_unit_path.clone(),
                hash_file(&good_unit_path).unwrap(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        };
        cache.insert(good.name(), Arc::new(good));

        // a meta whose usage location points at a FileId far past the end of
        // the arena — `try_path` returns None, serialize yields a serde error.
        let foreign = {
            let foreign_file = crate::meta::FileId(u32::MAX);
            let name = QualifiedName {
                name: crate::globals::intern("Foreign"),
                key: crate::globals::intern_key("Foreign"),
                location: CodeLocation {
                    file: foreign_file,
                    span: Span::new(0, 7),
                },
            };
            UnitMeta::new(
                Unit {
                    name,
                    interface_uses: None,
                    interface_declarations: Vec::new(),
                    implementation_uses: None,
                },
                false,
                good_dependency.clone(),
                hash_file(&good_dependency).unwrap(),
                Vec::new(),
                Vec::new(),
                vec![Usage {
                    symbol: crate::globals::intern_key("TFoo"),
                    location: CodeLocation {
                        file: foreign_file,
                        span: Span::new(0, 4),
                    },
                }],
            )
        };
        cache.insert(foreign.name(), Arc::new(foreign));

        // must NOT panic; the foreign-FileId meta is skipped, the good one saved
        let report = cache.save(&snapshot).unwrap();
        assert_eq!(report.written, 1, "only the well-formed meta is persisted");
        // the skip is VISIBLE in the report (M2 symmetry with LoadReport), not
        // silently swallowed: exactly one unit dropped, named, with its error.
        assert_eq!(report.skipped.len(), 1, "the foreign-FileId meta is reported skipped");
        assert_eq!(report.skipped[0].name, "FOREIGN");
        assert!(
            report.skipped[0].error.contains("FileId"),
            "skip carries the serde error, got: {}",
            report.skipped[0].error
        );
    }

    /// M2 (mirror of `foreign_fileid_does_not_panic_on_save`, for `Identifier`):
    /// a `UnitMeta` whose name `Identifier` is a `Spur` the CURRENT interner
    /// never issued (a stale Spur from a previous interner generation, or a
    /// foreign interner) must NOT panic on save. `Identifier::serialize` used to
    /// call lasso's `resolve`, which panics on a foreign Spur; it now uses
    /// `try_resolve` and yields a serde error, so the bad meta is skipped and
    /// reported, the snapshot still written.
    ///
    /// This test SWAPS the process-global interner via `reset_for_tests`, which
    /// invalidates every `Spur` interned in the previous generation — so it
    /// cannot run alongside other tests that hold live `Spur`s. It is therefore
    /// `#[ignore]`d out of the default (parallel) run and executed explicitly,
    /// serially:
    ///
    /// ```text
    /// cargo test --  --ignored --test-threads=1 foreign_identifier_does_not_panic_on_save
    /// ```
    ///
    /// It is the only in-tree test that calls `reset_for_tests`. Ignoring it
    /// keeps the default suite deterministically green while still proving the
    /// panic-free `Identifier::serialize` contract when run as documented.
    #[test]
    #[ignore = "swaps the global interner; run serially with --ignored --test-threads=1"]
    fn foreign_identifier_does_not_panic_on_save() {
        // Pad the CURRENT generation so the captured Spurs get large, high
        // indices. lasso hands out dense indices; after the reset below the
        // fresh interner only interns a handful of strings, so a high index
        // stays past the end of the new interner and `try_resolve` returns None
        // (a genuinely foreign Spur) instead of coincidentally resolving to a
        // re-issued low index. This makes the test robust whether run in
        // isolation or after the whole suite.
        for pad in 0..1000 {
            let _ = crate::globals::intern(&format!("__foreign_ident_pad_{pad}__"));
        }
        // Capture identifiers from the CURRENT interner generation. After the
        // reset below these become foreign Spurs the new interner cannot
        // resolve — exactly the condition `Identifier::serialize` must survive.
        let foreign_display = crate::globals::intern("ForeignUnit_Distinctive");
        let foreign_key = crate::globals::intern_key("ForeignUnit_Distinctive");

        // Fresh interner AND arena. `foreign_display`/`foreign_key` are now
        // stale (foreign) Spurs; anything interned/registered after this is
        // valid in the new generation.
        crate::globals::reset_for_tests();

        let directory = std::env::temp_dir().join("delphi_parser_foreign_identifier");
        let good_unit_path = write_temp(&directory, "GoodIdent.pas", "unit GoodIdent;");
        let snapshot = directory.join("cache.bin");
        let cache = UnitCache::default();

        // A valid meta built entirely in the NEW generation — proves the good
        // one still persists alongside the skipped foreign one.
        let good = build_meta(
            &good_unit_path,
            &write_temp(&directory, "GoodIdentDep.pas", "unit GoodIdentDep;"),
        );
        cache.insert(good.name(), Arc::new(good));

        // A meta whose name Identifier is foreign but whose FileId is valid in
        // the new arena — isolates the Identifier failure path. `register` the
        // file in the new arena so the FileId serializes fine; only the name
        // Spur is foreign.
        let file = crate::globals::arena().load(&good_unit_path).unwrap();
        let foreign = UnitMeta::new(
            Unit {
                name: QualifiedName {
                    name: foreign_display,
                    key: foreign_key,
                    location: CodeLocation { file, span: Span::new(0, 11) },
                },
                interface_uses: None,
                interface_declarations: Vec::new(),
                implementation_uses: None,
            },
            false,
            good_unit_path.clone(),
            hash_file(&good_unit_path).unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        cache.insert(foreign.name(), Arc::new(foreign));

        // Must NOT panic. The foreign-Identifier meta is skipped and reported;
        // the good meta is written.
        let report = cache.save(&snapshot).unwrap();
        assert_eq!(report.written, 1, "only the well-formed meta is persisted");
        assert_eq!(report.skipped.len(), 1, "the foreign-Identifier meta is reported skipped");
        // its own name Spur is foreign, so it cannot be resolved for the report
        assert_eq!(report.skipped[0].name, "<unresolved-identifier>");
        assert!(
            report.skipped[0].error.contains("identifier not in current interner"),
            "skip carries the identifier serde error, got: {}",
            report.skipped[0].error
        );
    }

    #[test]
    fn invalidation() {
        let cache = UnitCache::default();
        let unit = crate::globals::intern_key("X");
        cache.insert_failed(
            unit,
            Arc::new(ParseError::Unexpected {
                expected: "anything",
                found: None,
            }),
        );
        assert!(matches!(cache.get(unit), Some(CacheEntry::Failed(_))));
        cache.invalidate(unit);
        assert!(cache.get(unit).is_none());
    }
}
