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

use crate::unit_cache::{CachePersistError, LoadReport, SaveReport, UnitCache, hash_bytes};

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
        Ok(Self {
            snapshot_path: base.join(identity.snapshot_file_name()?),
        })
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

    /// Delete the snapshot (corruption recovery, explicit cache reset).
    pub fn discard(&self) -> Result<(), CachePersistError> {
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
