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

// v17: `UnitMeta::implementation_body` became `#[serde(skip)]` (the memory fix —
// bodies are working-set state for the active editor unit only, never persisted).
// A v16 snapshot's segment layout still carried the body bytes, so it must be
// rejected rather than mis-decoded.
const CACHE_FORMAT_VERSION: u32 = 17;
/// Default RAM cap for the in-memory AST cache. Lowered from 512MiB to 256MiB
/// for an EDITOR workload (Task 16 D): the disk-backed cache means an evicted
/// unit reloads cheaply from its per-unit file instead of re-parsing, so a
/// tighter working set trades a little reload latency for a much smaller
/// resident footprint. Paired with a NON-undercounting weigher
/// ([`UnitMeta::estimated_bytes`]) so this cap actually bounds process RAM near
/// its value rather than being blown past by an undercount.
pub const DEFAULT_CAPACITY_BYTES: u64 = 256 * 1024 * 1024;

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
    /// For a class/interface type: the FOLDED type KEYS of its declared
    /// ancestors (`class(TBase, IIntf)` → `[TBase, IIntf]`), in source order.
    /// Empty for every other shape. A DERIVED index field extracted from the
    /// AST's `ClassType.ancestors` / `InterfaceType.ancestors`; it drives the
    /// query-time inheritance-flattened member surface (completion / go-to /
    /// hover on an inherited member). Only the LAST segment of a dotted ancestor
    /// name is retained via its folded `key` — the interface index is
    /// name-keyed, so the simple type key is what a cross-unit
    /// `interface().find` consults.
    pub ancestors: Vec<Identifier>,
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

// ─── Persistence hook (Task 16: evict-to-disk) ─────────────────────────────

/// A sink that can write a single unit's [`UnitMeta`] to durable storage. The
/// [`crate::cache_store::CacheStore`] implements it. Attached to a [`UnitCache`]
/// via [`UnitCache::attach_persister`] so that:
///   1. a freshly-parsed DISK unit is written on insert (on disk BEFORE it can
///      be evicted), and
///   2. moka's eviction listener persists an evicted `Done` entry that was
///      somehow not yet persisted — making eviction always a safe, reloadable
///      drop, never data loss.
///
/// The never-persist gate (virtual/tainted/recovered) lives INSIDE the
/// implementation (`CacheStore::save_unit` returns `Ok(false)` for those), so
/// the cache calls `persist` unconditionally for `Done` entries and the sink
/// decides. `persist` is best-effort and MUST NOT panic: an IO failure is
/// logged by the implementation, never propagated (an eviction cannot fail).
pub trait UnitPersister: Send + Sync {
    /// Persist this meta if it is persistable. Best-effort, log-not-panic.
    fn persist(&self, meta: &UnitMeta);
}

/// Shared, write-once slot for the persister. `Arc`-cloned into the moka
/// eviction closure (set at build time, BEFORE the store exists) so the closure
/// reads whatever persister is later attached at session open. `OnceLock`
/// because a cache is bound to exactly one store for its whole lifetime.
type PersisterSlot = Arc<std::sync::OnceLock<Arc<dyn UnitPersister>>>;

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
    /// The durable per-unit sink (attached at session open). Shared with the
    /// eviction listener so an evicted `Done` entry is persisted before it is
    /// dropped from RAM. `None` (unset slot) → no persistence (batch parses,
    /// tests that never attach a store) — eviction is then a plain drop.
    persister: PersisterSlot,
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
        let persister: PersisterSlot = Arc::new(std::sync::OnceLock::new());
        // The eviction listener captures a clone of the persister slot. moka
        // sets the listener at BUILD time (before a store exists); the slot is
        // filled later at session open, and the closure reads it then. An
        // evicted `Done` entry is persisted here (belt-and-suspenders with
        // persist-on-insert) so eviction can NEVER strand an unpersisted AST —
        // it is always a safe drop, reloadable from disk. Best-effort and
        // panic-free: `persist` (→ `save_unit`) logs, never panics, so an
        // eviction cannot fail. `Failed` entries and evictions with no attached
        // persister are ignored (nothing durable to write).
        let eviction_slot = persister.clone();
        let entries = moka::sync::Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|_, entry: &CacheEntry| match entry {
                CacheEntry::Done(meta) => meta.estimated_bytes(),
                CacheEntry::Failed(_) => 64,
            })
            .eviction_listener(move |_key, entry, _cause| {
                if let CacheEntry::Done(meta) = entry {
                    if let Some(sink) = eviction_slot.get() {
                        sink.persist(&meta);
                    }
                }
            })
            .build();
        Self {
            entries,
            inserts: std::sync::atomic::AtomicU64::new(0),
            persister,
        }
    }

    /// Attach the durable per-unit sink. Called once at session open, after the
    /// [`crate::cache_store::CacheStore`] exists. Idempotent-safe: a second
    /// attach is ignored (the slot is write-once) — the cache is bound to one
    /// store for its lifetime. After this, `insert` persists disk units eagerly
    /// and eviction persists any not-yet-written `Done` entry.
    pub fn attach_persister(&self, sink: Arc<dyn UnitPersister>) {
        let _ = self.persister.set(sink);
    }

    /// Persist a meta through the attached sink, if any. Best-effort (the sink
    /// logs, never panics) and a no-op when no sink is attached. The sink itself
    /// applies the never-persist gate (virtual/tainted/recovered skipped).
    fn persist_now(&self, meta: &Arc<UnitMeta>) {
        if let Some(sink) = self.persister.get() {
            sink.persist(meta);
        }
    }

    pub fn get(&self, unit: Identifier) -> Option<CacheEntry> {
        self.entries.get(&unit)
    }

    pub fn insert(&self, unit: Identifier, meta: Arc<UnitMeta>) {
        // Persist-on-insert (Task 16): write a DISK unit to its per-unit file
        // BEFORE it can be evicted, so an eviction is always a safe drop. The
        // sink's never-persist gate skips virtual/tainted/recovered metas — the
        // active editor buffer (virtual) is therefore never written here. A
        // disk unit is parsed ONCE then cache-hit, so this is one write per
        // unit, not per keystroke. No-op when no store is attached.
        self.persist_now(&meta);
        self.entries.insert(unit, CacheEntry::Done(meta));
        self.inserts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Insert a meta that is ALREADY DURABLE on disk — WITHOUT re-persisting it
    /// (Task 16, write-amplification fix). The reload-on-miss path
    /// ([`crate::unit_loader`]) obtains a meta from
    /// [`crate::cache_store::CacheStore::load_unit`], which only returns a
    /// hash-VALID meta read from its per-unit file: the file therefore already
    /// exists and re-validates, so calling the full `save_unit` (serialize +
    /// temp-write + rename) on reinsert would be a redundant write of bytes
    /// identical to what is on disk. This variant skips [`Self::persist_now`] and
    /// only populates the RAM cache. Eviction still persists via the listener if
    /// somehow needed, and persist-on-insert stays the rule for freshly-PARSED
    /// disk units (which reach [`Self::insert`]).
    pub fn insert_durable(&self, unit: Identifier, meta: Arc<UnitMeta>) {
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
            // Same compressed `[magic | version]` segment format as the per-unit
            // files (via `serialize_meta`), so bulk and per-unit stay identical.
            match serialize_meta(meta.as_ref()) {
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
            let meta: UnitMeta = match decode_segment(&segment) {
                Some(meta) => meta,
                None => {
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

// ─── Per-unit persistence primitives (shared by bulk + per-unit paths) ─────
//
// These encapsulate the three invariants every persistence path must obey, so
// the bulk `save`/`load_into` and the per-unit `save_unit`/`load_unit`
// (cache_store.rs) share ONE implementation and cannot drift:
//   1. never-persist gate (virtual/tainted/recovered) — `is_persistable`;
//   2. transparent-serde serialize (FileId→path, Spur→string) — `serialize_meta`;
//   3. transparent-serde deserialize + hash-validation — `load_valid_meta`.

/// May this meta be written to disk at all? A `cycle_tainted` or `recovered`
/// parse is INCOMPLETE and must never persist as a clean interface (a fresh
/// full parse may differ) — the never-wrong discipline for the durable cache.
/// A VIRTUAL (unsaved editor) buffer must also never persist (#21/#25): its
/// source stamp hashed DECODED content, so it does not match its own on-disk
/// bytes — detected here as "own source hash does not validate against the file
/// on disk". A unit whose own source file is missing/unreadable is likewise not
/// persistable (nothing to validate against on reload). Dependencies/includes
/// are NOT re-validated here — those staleness checks happen on LOAD; this gate
/// only rejects the three never-persist categories.
pub fn is_persistable(meta: &UnitMeta) -> bool {
    if meta.cycle_tainted || meta.recovered {
        return false;
    }
    // Own source must be a real on-disk file whose raw bytes still hash to the
    // recorded stamp. A virtual buffer fails this (decoded-content hash ≠ disk
    // read, or the display path is not a real file) → never persisted (#21/#25).
    matches!(hash_file(&meta.source_path), Ok(hash) if hash == meta.source_hash)
}

/// Magic prefix stamped on every meta segment so a foreign/older byte layout is
/// rejected DETERMINISTICALLY (per-unit `.unit` files have no outer version
/// guard, unlike the bulk snapshot's `SavedCacheDisk.version`). The 4-byte magic
/// plus the `CACHE_FORMAT_VERSION` word means a stale segment decodes to `None`
/// (→ delete + re-parse) rather than being fed to an incompatible bincode.
const SEGMENT_MAGIC: [u8; 4] = *b"DUC1";
/// Header length: 4-byte magic + 4-byte little-endian format version.
const SEGMENT_HEADER_LEN: usize = 8;

/// Transparent-serde serialize of a single meta into its own byte segment,
/// DEFLATE-compressed behind a `[magic | version]` header. `Identifier`s and
/// `FileId`s inside serialize as strings/paths through the process globals; a
/// foreign `FileId`/`Spur` yields a serde error (never a panic — M2), returned
/// to the caller to record as a skip.
///
/// Compression matters: a meta is the full INTERFACE AST, and a large RTL/VCL
/// unit's AST is highly repetitive (thousands of similar decls) — DEFLATE cuts
/// the on-disk `.unit`/snapshot size several-fold. Writes are infrequent
/// (per unit on parse), so the default compression level is the right trade.
pub fn serialize_meta(meta: &UnitMeta) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let raw = bincode::serialize(meta).map_err(|error| error.to_string())?;
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&raw).map_err(|error| error.to_string())?;
    let compressed = encoder.finish().map_err(|error| error.to_string())?;
    let mut segment = Vec::with_capacity(SEGMENT_HEADER_LEN + compressed.len());
    segment.extend_from_slice(&SEGMENT_MAGIC);
    segment.extend_from_slice(&CACHE_FORMAT_VERSION.to_le_bytes());
    segment.extend_from_slice(&compressed);
    Ok(segment)
}

/// Decode one meta segment written by [`serialize_meta`]: check the
/// `[magic | version]` header, DEFLATE-decompress, then transparent-serde
/// deserialize (re-registering FileIds / re-interning Spurs). Panic-free: a
/// missing/foreign magic, a version mismatch, a truncated stream, or a corrupt
/// payload all degrade to `None` (never a crash — M2). Does NOT hash-validate;
/// callers that require freshness use [`load_valid_meta`].
fn decode_segment(segment: &[u8]) -> Option<UnitMeta> {
    use std::io::Read;
    if segment.len() < SEGMENT_HEADER_LEN || segment[0..4] != SEGMENT_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(segment[4..SEGMENT_HEADER_LEN].try_into().ok()?);
    if version != CACHE_FORMAT_VERSION {
        return None;
    }
    let mut decoder = flate2::read::DeflateDecoder::new(&segment[SEGMENT_HEADER_LEN..]);
    let mut raw = Vec::new();
    decoder.read_to_end(&mut raw).ok()?;
    bincode::deserialize(&raw).ok()
}

/// Test-only wrapper over the private [`decode_segment`] so cross-module tests
/// (e.g. the S3 body round-trip in `unit_meta`) can exercise the real
/// `serialize_meta` → `[magic|version]` decode path.
#[cfg(test)]
pub fn decode_segment_for_test(segment: &[u8]) -> Option<UnitMeta> {
    decode_segment(segment)
}

/// Deserialize one meta segment (transparent serde re-registers FileIds /
/// re-interns Spurs) and hash-validate it against current file contents. Panic
/// free: a corrupt segment or unregisterable path is a clean `None` (never a
/// crash — M2). Returns `Some(meta)` ONLY when the meta decodes AND is
/// hash-fresh (own source + dfm + includes + dependencies + their includes).
pub fn load_valid_meta(segment: &[u8]) -> Option<UnitMeta> {
    let meta: UnitMeta = decode_segment(segment)?;
    match validate_meta(&meta) {
        Validity::Fresh => Some(meta),
        Validity::Stale | Validity::Unreadable => None,
    }
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
    fn segment_is_compressed_versioned_and_roundtrips() {
        // A meta segment is DEFLATE-compressed behind a `[magic | version]`
        // header: it must shrink a repetitive payload, round-trip through the
        // validating loader, and reject the OLD headerless (raw-bincode) format.
        let directory = std::env::temp_dir().join("delphi_parser_unit_cache_compress");
        let unit_path = write_temp(&directory, "UnitC.pas", "unit UnitC;");
        let dependency_path = write_temp(&directory, "UnitD.pas", "unit UnitD;");
        let mut meta = build_meta(&unit_path, &dependency_path);
        // Many repeated usages → a highly compressible payload, mirroring the
        // repetition of a real interface AST.
        let file = meta.usages[0].location.file;
        meta.usages = (0..2000)
            .map(|_| Usage {
                symbol: crate::globals::intern_key("TFoo"),
                location: CodeLocation { file, span: Span::new(0, 4) },
            })
            .collect();

        let raw = bincode::serialize(&meta).unwrap();
        let segment = serialize_meta(&meta).expect("serializes");

        // Header: magic + current version.
        assert_eq!(&segment[0..4], &SEGMENT_MAGIC);
        assert_eq!(
            u32::from_le_bytes(segment[4..8].try_into().unwrap()),
            CACHE_FORMAT_VERSION
        );
        // Compression actually shrinks a repetitive payload (header included).
        assert!(
            segment.len() < raw.len(),
            "compressed {} must be smaller than raw {}",
            segment.len(),
            raw.len()
        );

        // Round-trips through the validating loader (source unchanged → Fresh).
        let loaded = load_valid_meta(&segment).expect("decodes + fresh");
        assert_eq!(loaded.usages.len(), 2000);

        // A raw (headerless, uncompressed) segment — the OLD format — is
        // rejected cleanly (→ None → delete + re-parse), never mis-decoded.
        assert!(
            decode_segment(&raw).is_none(),
            "an old headerless segment must not decode"
        );
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
        // A snapshot written by a PRIOR format version (here v16, one behind the
        // current v17) must be refused with a clean version-mismatch error — not
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
        assert_eq!(CACHE_FORMAT_VERSION, 17, "update this test on a format bump");
        let stale = SavedCacheDisk {
            version: 16,
            units: vec![vec![0xDE, 0xAD, 0xBE, 0xEF]],
        };
        std::fs::write(&snapshot, bincode::serialize(&stale).unwrap()).unwrap();

        let cache = UnitCache::default();
        let result = cache.load(&snapshot);
        let error = result.expect_err("an old-version snapshot must be rejected");
        // the message names both the found and expected versions
        assert!(
            error.message.contains("16") && error.message.contains("17"),
            "version-mismatch message must name found (16) and expected (17): {}",
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

    /// Write-amplification fix (Task 16): `insert` persists (a freshly-parsed
    /// disk unit must reach disk before it can be evicted), but `insert_durable`
    /// does NOT (its meta already came from disk, hash-valid — re-persisting
    /// would rewrite identical bytes). A counting persister proves each path's
    /// persist behaviour exactly.
    #[test]
    fn insert_durable_skips_persist_but_insert_persists() {
        use std::sync::atomic::{AtomicU64, Ordering};

        struct CountingPersister {
            calls: Arc<AtomicU64>,
        }
        impl UnitPersister for CountingPersister {
            fn persist(&self, _meta: &UnitMeta) {
                self.calls.fetch_add(1, Ordering::Relaxed);
            }
        }

        let directory = std::env::temp_dir().join("delphi_parser_insert_durable");
        let unit_path = write_temp(&directory, "UnitA.pas", "unit UnitA;");
        let dependency_path = write_temp(&directory, "UnitB.pas", "unit UnitB;");

        let calls = Arc::new(AtomicU64::new(0));
        // A large cap so nothing is EVICTED — the eviction listener also calls
        // `persist` (belt-and-suspenders), and re-inserting the SAME key triggers
        // a replace-eviction; using distinct keys under a roomy cap isolates the
        // insert-path `persist_now` (the write-amp under test) from that.
        let cache = UnitCache::with_capacity(256 * 1024 * 1024);
        cache.attach_persister(Arc::new(CountingPersister {
            calls: calls.clone(),
        }));

        let meta = Arc::new(build_meta(&unit_path, &dependency_path));

        // insert → one persist (persist-on-insert for a freshly-parsed unit).
        cache.insert(crate::globals::intern_key("FreshUnit"), meta.clone());
        assert_eq!(calls.load(Ordering::Relaxed), 1, "insert must persist once");

        // insert_durable (distinct key, no eviction) → NO persist: the reload
        // path's meta is already on disk, hash-valid; re-persisting would rewrite
        // identical bytes (the write-amplification this fix removes).
        cache.insert_durable(crate::globals::intern_key("ReloadedUnit"), meta.clone());
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "insert_durable must NOT persist — the meta is already on disk"
        );

        // a second insert_durable under an ALIAS key must ALSO not persist:
        // proves the reload alias path is free of the double-write.
        cache.insert_durable(crate::globals::intern_key("AliasUnit"), meta);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "aliasing via insert_durable must not write a second time"
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
