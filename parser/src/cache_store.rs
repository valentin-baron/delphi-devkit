//! Snapshot location + atomic save/load for the [`UnitCache`], keyed by
//! project identity (dproj path, configuration, platform, compiler version).
//!
//! Default location: `%LOCALAPPDATA%\delphi-devkit\parser-cache\<hash>.bin`.
//! One snapshot per identity — switching configuration or platform is a
//! different `ProjectContext` AND a different snapshot.
//!
//! Edge cases handled explicitly:
//! - `%LOCALAPPDATA%` unset → hard error, no silent fallback location.
//! - Concurrent writers (two LSP/ddk processes, same project): each writes a
//!   process-unique temp file, then renames. `std::fs::rename` on Windows
//!   replaces the destination atomically (MOVEFILE_REPLACE_EXISTING) — the
//!   last writer wins, a reader never sees a torn file.
//! - Corrupt/truncated snapshot → explicit error from `load_into`; caller
//!   decides (typically [`CacheStore::discard`] + full reparse). Never a
//!   panic, never silently treated as "no cache".
//! - Missing snapshot (first run) → `Ok(None)`, distinct from corruption.
//! - Project path is canonicalized and case-folded before hashing — the same
//!   project opened via `c:\FOO` and `C:\foo` maps to one snapshot.
//!
//! "Save regularly" is the driver's trigger (periodic timer + on shutdown +
//! after watcher-driven reindexing); this module provides the primitive.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use crate::unit_cache::{
    CachePersistError, LoadReport, SaveReport, UnitCache, hash_bytes, is_persistable,
    load_valid_meta, serialize_meta,
};
use crate::unit_meta::UnitMeta;

/// What makes a snapshot unique. Two identities that differ in ANY field use
/// separate snapshot files.
#[derive(Debug)]
pub struct CacheIdentity<'a> {
    pub project_path: &'a Path,
    pub configuration: &'a str,
    pub platform: &'a str,
    pub compiler_version: f64,
}

impl CacheIdentity<'_> {
    fn snapshot_file_name(&self) -> Result<String, CachePersistError> {
        let canonical = self
            .project_path
            .canonicalize()
            .map_err(|error| CachePersistError {
                message: format!(
                    "cannot canonicalize project path {}: {error}",
                    self.project_path.display()
                ),
            })?;
        // Windows paths are case-insensitive; fold so spelling variants of
        // the same project share one snapshot.
        let folded_path = canonical.to_string_lossy().to_lowercase();
        let mut identity_bytes = Vec::new();
        identity_bytes.extend_from_slice(folded_path.as_bytes());
        identity_bytes.push(0);
        identity_bytes.extend_from_slice(self.configuration.as_bytes());
        identity_bytes.push(0);
        identity_bytes.extend_from_slice(self.platform.as_bytes());
        identity_bytes.push(0);
        identity_bytes.extend_from_slice(&self.compiler_version.to_bits().to_le_bytes());
        Ok(format!("{:016x}.bin", hash_bytes(&identity_bytes)))
    }
}

pub struct CacheStore {
    pub snapshot_path: PathBuf,
    /// Directory holding the PER-UNIT snapshot files (`<key-hash>.unit`), one
    /// per persisted disk unit. Derived from the same project-identity as
    /// `snapshot_path` (sibling `<identity>.units/` directory), so switching
    /// configuration/platform uses a different per-unit directory exactly like
    /// it uses a different bulk snapshot. Per-unit files are the memory-bound
    /// core (Task 16): a unit is written here on insert and reloaded from here
    /// after a moka eviction, so RAM holds only the working set.
    pub units_dir: PathBuf,
}

impl CacheStore {
    /// Store under `%LOCALAPPDATA%\delphi-devkit\parser-cache`. Errors when
    /// the environment variable is unset — deliberately no fallback, a cache
    /// silently landing in an unexpected directory is worse than an error.
    pub fn for_project(identity: &CacheIdentity) -> Result<Self, CachePersistError> {
        let local_app_data =
            std::env::var_os("LOCALAPPDATA").ok_or_else(|| CachePersistError {
                message: "LOCALAPPDATA is not set".to_string(),
            })?;
        let base = PathBuf::from(local_app_data)
            .join("delphi-devkit")
            .join("parser-cache");
        Self::in_directory(base, identity)
    }

    /// Store under an explicit base directory (tests, custom setups).
    pub fn in_directory(
        base: impl Into<PathBuf>,
        identity: &CacheIdentity,
    ) -> Result<Self, CachePersistError> {
        let base = base.into();
        std::fs::create_dir_all(&base).map_err(|error| CachePersistError {
            message: format!("cannot create cache directory {}: {error}", base.display()),
        })?;
        let snapshot_path = base.join(identity.snapshot_file_name()?);
        // Per-unit files live in a sibling `<identity>.units/` directory. It is
        // created lazily on the first `save_unit` (a read-only session that only
        // ever `load_unit`s never needs it), so `in_directory` does not create
        // it — only compute the path.
        let units_dir = snapshot_path.with_extension("units");
        Ok(Self {
            snapshot_path,
            units_dir,
        })
    }

    /// Absolute path of the per-unit snapshot file for a unit identified by its
    /// name STRING. The `Identifier`/`Spur` is process-local and unstable across
    /// sessions, so the filename hashes the stable, CASE-FOLDED name instead —
    /// the same discipline the bulk snapshot uses for its identity hash. Folding
    /// here makes save/load agree on the file regardless of the caller's casing
    /// (`resolve(unit_key)` yields the folded key, but an external caller may
    /// pass the as-written spelling). Distinct units cannot collide (64-bit xxh3
    /// of the folded name); the `.unit` extension keeps them apart from `.bin`.
    pub fn unit_file_path(&self, unit_name: &str) -> PathBuf {
        let folded = crate::globals::fold_identifier(unit_name);
        let hash = hash_bytes(folded.as_bytes());
        self.units_dir.join(format!("{hash:016x}.unit"))
    }

    /// Atomic snapshot write: temp file (process-unique) + rename. Interned
    /// identifiers and file ids serialize transparently through the process
    /// globals, so no arena/interner needs to be threaded here.
    pub fn save(&self, cache: &UnitCache) -> Result<SaveReport, CachePersistError> {
        // pid + a process-local counter: two concurrent saves in the SAME
        // process (autosave timer racing a shutdown save) must not share one
        // temp path and clobber each other before the rename (L14).
        static SAVE_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = SAVE_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = self
            .snapshot_path
            .with_extension(format!("tmp-{}-{}", std::process::id(), nonce));
        let report = cache.save(&temporary)?;
        std::fs::rename(&temporary, &self.snapshot_path).map_err(|error| {
            // leave no orphaned temp file behind on failure
            let _ = std::fs::remove_file(&temporary);
            CachePersistError {
                message: format!(
                    "cannot move snapshot into place at {}: {error}",
                    self.snapshot_path.display()
                ),
            }
        })?;
        Ok(report)
    }

    /// Load the snapshot if one exists. `Ok(None)` = no snapshot (first run).
    /// `Err` = snapshot exists but is unreadable/corrupt — caller decides,
    /// typically [`Self::discard`] followed by a cold start.
    pub fn load_into(&self, cache: &UnitCache) -> Result<Option<LoadReport>, CachePersistError> {
        if !self.snapshot_path.exists() {
            return Ok(None);
        }
        cache.load(&self.snapshot_path).map(Some)
    }

    // ─── Per-unit persistence (Task 16: memory-bound core) ───────────────
    //
    // A DISK unit is written to its own `<key-hash>.unit` file on insert so it
    // is on disk BEFORE moka can evict it; after an eviction a cache miss
    // reloads it from that file (hash-validated) instead of re-parsing from
    // source. RAM therefore holds only the working set. Virtual/tainted/
    // recovered units are NEVER written (the never-persist invariant #21/#25),
    // gated by `is_persistable`.

    /// The stable per-unit filename for a meta, or `None` if the meta's own name
    /// `Identifier` is foreign (a `Spur` the current interner cannot resolve) —
    /// such a meta cannot be addressed on disk and is simply not persisted
    /// (never a panic). Uses `try_resolve` for the same reason bulk `save` does.
    fn unit_path_for(&self, meta: &UnitMeta) -> Option<PathBuf> {
        let folded = crate::globals::interner().try_resolve(&meta.name().spur())?;
        Some(self.unit_file_path(folded))
    }

    /// Persist ONE unit's [`UnitMeta`] to its per-unit snapshot file, atomically
    /// (temp + rename). Returns `Ok(false)` — WITHOUT writing — when the unit
    /// must not persist (virtual/tainted/recovered, per [`is_persistable`], or a
    /// meta with a foreign name/`FileId` that cannot be addressed/serialized);
    /// `Ok(true)` when a file was written. Never a panic on a foreign
    /// FileId/Spur: serialization degrades to a clean skip (M2).
    ///
    /// The write is atomic like the bulk `save`: a process-unique temp file is
    /// written then renamed into place, so a concurrent `load_unit` never sees a
    /// torn file and two writers of the same unit resolve last-writer-wins.
    pub fn save_unit(&self, meta: &UnitMeta) -> Result<bool, CachePersistError> {
        // Never-persist gate (virtual/tainted/recovered). A virtual buffer's own
        // source hash does not validate against a disk read → skipped (#21/#25).
        if !is_persistable(meta) {
            return Ok(false);
        }
        let Some(path) = self.unit_path_for(meta) else {
            // Foreign name Identifier — cannot be addressed on disk. Skip, never
            // panic (mirrors bulk save's `try_resolve` discipline).
            return Ok(false);
        };
        // Transparent-serde serialize; a foreign FileId yields a serde error →
        // clean skip, never a panic (M2). Only real failures to reach disk are
        // surfaced as errors below.
        let Ok(segment) = serialize_meta(meta) else {
            return Ok(false);
        };

        std::fs::create_dir_all(&self.units_dir).map_err(|error| CachePersistError {
            message: format!(
                "cannot create per-unit cache directory {}: {error}",
                self.units_dir.display()
            ),
        })?;
        // pid + counter: two saves of the SAME unit in one process must not
        // share a temp path and clobber each other before the rename (L14).
        static UNIT_SAVE_NONCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let nonce = UNIT_SAVE_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), nonce));
        std::fs::write(&temporary, &segment).map_err(|error| CachePersistError {
            message: format!(
                "cannot write per-unit snapshot {}: {error}",
                temporary.display()
            ),
        })?;
        std::fs::rename(&temporary, &path).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            CachePersistError {
                message: format!(
                    "cannot move per-unit snapshot into place at {}: {error}",
                    path.display()
                ),
            }
        })?;
        Ok(true)
    }

    /// Reload ONE unit from its per-unit snapshot file, hash-validated. Returns:
    /// - `Some(meta)` when the file exists, decodes (transparent serde
    ///   re-registers FileIds / re-interns Spurs) AND is hash-fresh (own source
    ///   + dfm + includes + dependencies + their includes all match disk);
    /// - `None` when there is no file, it is corrupt, or it is STALE (any hash
    ///   changed) — the caller then re-parses from source. A corrupt/stale file
    ///   is deleted (best-effort) so it does not linger; the delete failing is
    ///   itself non-fatal.
    ///
    /// Panic-free: a corrupt file / unregisterable path degrades to `None`,
    /// never a crash (M2). Takes the folded unit-name STRING (stable), matching
    /// how `save_unit` names the file.
    pub fn load_unit(&self, folded_unit_name: &str) -> Option<Arc<UnitMeta>> {
        let path = self.unit_file_path(folded_unit_name);
        let bytes = std::fs::read(&path).ok()?;
        match load_valid_meta(&bytes) {
            Some(meta) => Some(Arc::new(meta)),
            None => {
                // Corrupt or stale (source/dep hash changed): drop the file so a
                // later `load_unit` does not keep re-reading a dead snapshot. The
                // authoritative copy will be rewritten on the next parse+insert.
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Delete the snapshot (corruption recovery, explicit cache reset).
    pub fn discard(&self) -> Result<(), CachePersistError> {
        // Reset the per-unit files too: a corruption recovery / explicit cache
        // reset must wipe the whole durable cache, not just the bulk snapshot.
        // Best-effort — a missing directory is fine, a real failure surfaces.
        match std::fs::remove_dir_all(&self.units_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CachePersistError {
                    message: format!(
                        "cannot delete per-unit cache directory {}: {error}",
                        self.units_dir.display()
                    ),
                });
            }
        }
        match std::fs::remove_file(&self.snapshot_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CachePersistError {
                message: format!(
                    "cannot delete snapshot {}: {error}",
                    self.snapshot_path.display()
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{QualifiedName, Unit};
    use crate::meta::{CodeLocation, Span};
    use crate::unit_cache::{CacheEntry, Usage, hash_file};
    use crate::unit_meta::UnitMeta;
    use std::sync::Arc;

    fn test_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join("delphi_parser_cache_store").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn write_file(directory: &Path, name: &str, content: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn meta_for(unit_path: &Path) -> UnitMeta {
        let arena = crate::globals::arena();
        let file = arena.load(unit_path).unwrap();
        let name = QualifiedName {
            name: crate::globals::intern("UnitA"),
            key: crate::globals::intern_key("UnitA"),
            location: CodeLocation { file, span: Span::new(0, 4) },
        };
        let ast = Unit {
            name,
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
            Vec::new(),
            vec![Usage {
                symbol: crate::globals::intern_key("TFoo"),
                location: CodeLocation { file, span: Span::new(0, 4) },
            }],
        )
    }

    /// A meta whose unit name is `unit_name` (distinct per test so per-unit
    /// files do not collide), sourced from a real on-disk `<unit_name>.pas`.
    fn named_meta(directory: &Path, unit_name: &str) -> UnitMeta {
        let unit_path = write_file(directory, &format!("{unit_name}.pas"), &format!("unit {unit_name};"));
        let arena = crate::globals::arena();
        let file = arena.load(&unit_path).unwrap();
        let name = QualifiedName {
            name: crate::globals::intern(unit_name),
            key: crate::globals::intern_key(unit_name),
            location: CodeLocation { file, span: Span::new(0, unit_name.len()) },
        };
        UnitMeta::new(
            Unit {
                name,
                interface_uses: None,
                interface_declarations: Vec::new(),
                implementation_uses: None,
            },
            false,
            unit_path.to_path_buf(),
            hash_file(&unit_path).unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    fn identity_for<'a>(project: &'a Path) -> CacheIdentity<'a> {
        CacheIdentity {
            project_path: project,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        }
    }

    #[test]
    fn save_unit_then_load_unit_round_trips() {
        // Deliverable A: one unit written to its per-unit file reloads
        // hash-valid, without touching the bulk snapshot.
        let directory = test_directory("save_unit_roundtrip");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let store = CacheStore::in_directory(&directory, &identity_for(&project)).unwrap();

        let meta = named_meta(&directory, "PerUnitA");
        assert!(store.save_unit(&meta).unwrap(), "a disk unit must persist");
        // the per-unit file exists; the bulk snapshot does NOT
        assert!(store.unit_file_path("perunita").exists());
        assert!(!store.snapshot_path.exists());

        let loaded = store.load_unit("perunita").expect("reloads from disk");
        assert_eq!(crate::globals::resolve(loaded.name()), "PERUNITA");
        assert_eq!(loaded.source_hash, meta.source_hash);
    }

    #[test]
    fn load_unit_missing_is_none() {
        let directory = test_directory("load_unit_missing");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let store = CacheStore::in_directory(&directory, &identity_for(&project)).unwrap();
        assert!(store.load_unit("neversaved").is_none());
    }

    #[test]
    fn load_unit_stale_after_source_change_is_none_and_deletes() {
        // A per-unit file whose source bytes changed must reload as None
        // (hash-mismatch) and be deleted, so it never serves stale state.
        let directory = test_directory("load_unit_stale");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let store = CacheStore::in_directory(&directory, &identity_for(&project)).unwrap();

        let meta = named_meta(&directory, "StaleUnit");
        assert!(store.save_unit(&meta).unwrap());
        let unit_file = store.unit_file_path("staleunit");
        assert!(unit_file.exists());

        // change the source bytes → the recorded stamp no longer matches
        std::fs::write(directory.join("StaleUnit.pas"), "unit StaleUnit; // edited").unwrap();
        assert!(store.load_unit("staleunit").is_none(), "stale reload is rejected");
        assert!(!unit_file.exists(), "a stale per-unit file is deleted");
    }

    #[test]
    fn load_unit_corrupt_is_none_not_panic() {
        // M2: a corrupt per-unit file degrades to None (dropped), never a panic.
        let directory = test_directory("load_unit_corrupt");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let store = CacheStore::in_directory(&directory, &identity_for(&project)).unwrap();

        let meta = named_meta(&directory, "CorruptUnit");
        assert!(store.save_unit(&meta).unwrap());
        let unit_file = store.unit_file_path("corruptunit");
        std::fs::write(&unit_file, [0xFFu8; 16]).unwrap();

        assert!(store.load_unit("corruptunit").is_none());
        assert!(!unit_file.exists(), "a corrupt per-unit file is deleted");
    }

    #[test]
    fn save_unit_skips_tainted_and_recovered() {
        // The never-persist gate: cycle-tainted and recovered metas are never
        // written to a per-unit file (invariant #21/#25 discipline).
        let directory = test_directory("save_unit_tainted");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let store = CacheStore::in_directory(&directory, &identity_for(&project)).unwrap();

        // cycle_tainted = true
        let mut tainted = named_meta(&directory, "TaintedUnit");
        tainted.cycle_tainted = true;
        assert!(!store.save_unit(&tainted).unwrap(), "tainted must not persist");
        assert!(!store.unit_file_path("taintedunit").exists());

        // recovered = true
        let recovered = named_meta(&directory, "RecoveredUnit").with_recovered(true);
        assert!(!store.save_unit(&recovered).unwrap(), "recovered must not persist");
        assert!(!store.unit_file_path("recoveredunit").exists());
    }

    #[test]
    fn save_unit_skips_virtual_source() {
        // #21/#25: a meta whose source path is not a real on-disk file whose
        // bytes hash to its stamp (a virtual/unsaved buffer) is never written.
        let directory = test_directory("save_unit_virtual");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let store = CacheStore::in_directory(&directory, &identity_for(&project)).unwrap();

        // real meta, then corrupt the recorded source_hash so it no longer
        // validates against the disk file — the same shape a virtual buffer has
        // (decoded-content hash ≠ disk read).
        let mut meta = named_meta(&directory, "VirtualLike");
        meta.source_hash ^= 0xDEAD_BEEF;
        assert!(!store.save_unit(&meta).unwrap(), "non-validating source must not persist");
        assert!(!store.unit_file_path("virtuallike").exists());
    }

    #[test]
    fn discard_removes_per_unit_files() {
        let directory = test_directory("discard_units");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let store = CacheStore::in_directory(&directory, &identity_for(&project)).unwrap();
        let meta = named_meta(&directory, "DiscardMe");
        assert!(store.save_unit(&meta).unwrap());
        assert!(store.units_dir.exists());
        store.discard().unwrap();
        assert!(!store.units_dir.exists(), "discard wipes the per-unit directory");
        store.discard().unwrap(); // idempotent
    }

    #[test]
    fn roundtrip_through_store() {
        let directory = test_directory("roundtrip");
        let unit_path = write_file(&directory, "UnitA.pas", "unit UnitA;");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let identity = CacheIdentity {
            project_path: &project,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };

        {
            let cache = UnitCache::default();
            let built = meta_for(&unit_path);
            cache.insert(built.name(), Arc::new(built));
            let store = CacheStore::in_directory(&directory, &identity).unwrap();
            assert_eq!(store.save(&cache).unwrap().written, 1);
        }

        let cache = UnitCache::default();
        let store = CacheStore::in_directory(&directory, &identity).unwrap();
        let report = store.load_into(&cache).unwrap().unwrap();
        assert_eq!(report.loaded, 1);
        assert!(matches!(
            cache.get(crate::globals::intern_key("UnitA")),
            Some(CacheEntry::Done(_))
        ));
    }

    #[test]
    fn missing_snapshot_is_none_not_error() {
        let directory = test_directory("missing");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let identity = CacheIdentity {
            project_path: &project,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };
        let store = CacheStore::in_directory(&directory, &identity).unwrap();
        let cache = UnitCache::default();
        assert!(store.load_into(&cache).unwrap().is_none());
    }

    #[test]
    fn corrupt_snapshot_is_error_then_discardable() {
        let directory = test_directory("corrupt");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let identity = CacheIdentity {
            project_path: &project,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };
        let store = CacheStore::in_directory(&directory, &identity).unwrap();
        std::fs::write(&store.snapshot_path, b"not a snapshot").unwrap();

        let cache = UnitCache::default();
        assert!(store.load_into(&cache).is_err());
        store.discard().unwrap();
        assert!(store.load_into(&cache).unwrap().is_none());
        store.discard().unwrap(); // idempotent on missing file
    }

    #[test]
    fn identity_fields_separate_snapshots() {
        let directory = test_directory("identity");
        let project = write_file(&directory, "P.dproj", "<x/>");
        let debug = CacheIdentity {
            project_path: &project,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };
        let release = CacheIdentity {
            configuration: "Release",
            ..debug
        };
        let win64 = CacheIdentity {
            platform: "Win64",
            ..debug
        };
        let name_debug = debug.snapshot_file_name().unwrap();
        assert_ne!(name_debug, release.snapshot_file_name().unwrap());
        assert_ne!(name_debug, win64.snapshot_file_name().unwrap());
        // same project via different path spelling → same snapshot
        let uppercase_spelling = directory.join("P.DPROJ");
        let respelled = CacheIdentity {
            project_path: &uppercase_spelling,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };
        assert_eq!(name_debug, respelled.snapshot_file_name().unwrap());
    }
}
