//! File watching → invalidation planning.
//!
//! Split in two layers so the interesting logic is deterministic and fully
//! testable without a filesystem:
//!
//! 1. [`ChangeCollector`] — pure state machine fed with (path, Instant).
//!    Debounces to quiescence and detects **bursts** (git checkout, branch
//!    switch): when more distinct files changed than `burst_threshold`, the
//!    plan flips from per-file invalidation to one deferred full
//!    hash-revalidation sweep. While events keep arriving, flushing is
//!    postponed — a checkout never causes invalidation thrash mid-flight.
//! 2. [`FileWatcher`] — thin `notify` glue feeding the collector; the driver
//!    polls it on its own cadence.
//!
//! Path identity: keys are canonicalized when the file still exists and
//! case-folded (Windows). For deleted files canonicalization is impossible —
//! the folded raw path is used. If that spelling differs from the indexed one
//! (8.3 names, symlinks), the per-file lookup can miss; the full-sweep path
//! and load-time hash validation still catch it. Recorded in SESSION.md
//! ledger (#13).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

use crate::context::Identifier;
use crate::unit_cache::{CacheEntry, UnitCache, hash_file};
use crate::unit_meta::UnitMeta;

pub const WATCHED_EXTENSIONS: &[&str] = &["pas", "inc", "dpr", "dpk", "dproj", "dfm"];

pub fn is_watched_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            WATCHED_EXTENSIONS
                .iter()
                .any(|watched| extension.eq_ignore_ascii_case(watched))
        })
}

/// Stable key for path-identity maps, always case-folded.
///
/// Canonicalizes when possible, then STRIPS the Windows verbatim/extended-
/// length prefix (`\\?\`, `\\?\UNC\`). This is what makes DELETE invalidation
/// work: an existing file canonicalizes to `\\?\C:\..`, but a delete event's
/// path cannot be canonicalized (the file is gone) and stays raw `C:\..`. Left
/// unstripped, the two never compare equal, so every delete silently misses
/// per-file invalidation and the cache serves stale results (ledger #13/H10).
/// Folding away the prefix reconciles the canonical and raw spellings.
pub fn path_key(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.to_string_lossy();
    let normalized = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.into_owned()
    };
    normalized.to_lowercase()
}

#[derive(Debug, PartialEq, Eq)]
pub enum InvalidationPlan {
    /// Few files changed: invalidate exactly the units depending on them.
    PerFile(Vec<PathBuf>),
    /// Burst (e.g. git checkout): one full hash-revalidation sweep instead of
    /// thrashing per file.
    FullSweep { changed_files: usize },
}

#[derive(Debug, Clone, Copy)]
pub struct ChangeCollectorConfig {
    /// No new event for this long → pending changes flush.
    pub quiescence: Duration,
    /// More distinct files than this in one pending batch → burst.
    pub burst_threshold: usize,
    /// Hard ceiling on how long a batch may keep deferring. Continuous churn
    /// (a formatter/build touching watched files faster than `quiescence`
    /// forever) would otherwise never reach quiescence, starving invalidation
    /// and serving stale results indefinitely. Past this age since the batch's
    /// FIRST event, flush regardless (L17).
    pub max_defer: Duration,
}

impl Default for ChangeCollectorConfig {
    fn default() -> Self {
        Self {
            quiescence: Duration::from_millis(500),
            burst_threshold: 64,
            max_defer: Duration::from_secs(5),
        }
    }
}

/// Deterministic debounce/burst state machine. Time is always passed in —
/// never sampled — so every edge is unit-testable.
pub struct ChangeCollector {
    config: ChangeCollectorConfig,
    pending: HashMap<String, PathBuf>, // key → original path, deduplicated
    last_event: Option<Instant>,
    first_event: Option<Instant>, // start of the current batch (for max_defer)
}

impl ChangeCollector {
    pub fn new(config: ChangeCollectorConfig) -> Self {
        Self {
            config,
            pending: HashMap::new(),
            last_event: None,
            first_event: None,
        }
    }

    /// Feed one filesystem event. Non-watched extensions are ignored.
    pub fn record(&mut self, path: PathBuf, now: Instant) {
        if !is_watched_path(&path) {
            return;
        }
        if self.first_event.is_none() {
            self.first_event = Some(now);
        }
        self.pending.insert(path_key(&path), path);
        self.last_event = Some(now);
    }

    /// Flush if quiescence has been reached. `None` = nothing to do yet
    /// (empty, or events still arriving — a running checkout keeps pushing
    /// the flush out).
    pub fn poll(&mut self, now: Instant) -> Option<InvalidationPlan> {
        let last_event = self.last_event?;
        if self.pending.is_empty() {
            return None;
        }
        let quiet = now.duration_since(last_event) >= self.config.quiescence;
        let deferred_too_long = self
            .first_event
            .is_some_and(|first| now.duration_since(first) >= self.config.max_defer);
        if !quiet && !deferred_too_long {
            return None;
        }

        let changed: Vec<PathBuf> = self.pending.drain().map(|(_, path)| path).collect();
        self.last_event = None;
        self.first_event = None;
        if changed.len() > self.config.burst_threshold {
            Some(InvalidationPlan::FullSweep {
                changed_files: changed.len(),
            })
        } else {
            Some(InvalidationPlan::PerFile(changed))
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

// ─── Reverse dependencies ────────────────────────────────────────────────

/// path → units whose artifact depends on that file (own source, includes,
/// consulted dependency sources).
///
/// Entries are additive: when a unit is re-parsed with a smaller file set,
/// old mappings linger until [`Self::rebuild`] — they can only cause
/// *over*-invalidation (safe direction), never missed invalidation.
#[derive(Default)]
pub struct ReverseDependencyIndex {
    map: Mutex<HashMap<String, HashSet<Identifier>>>,
}

impl ReverseDependencyIndex {
    pub fn index_artifact(&self, unit_key: Identifier, meta: &UnitMeta) {
        let mut map = self.map.lock().unwrap();
        for path in meta.watched_files() {
            map.entry(path_key(path)).or_default().insert(unit_key);
        }
    }

    pub fn units_for(&self, path: &Path) -> Vec<Identifier> {
        self.map
            .lock()
            .unwrap()
            .get(&path_key(path))
            .map(|units| units.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Drop everything (before re-indexing from current cache contents).
    pub fn rebuild_from<'a>(
        &self,
        metas: impl Iterator<Item = (Identifier, &'a UnitMeta)>,
    ) {
        let mut map = self.map.lock().unwrap();
        map.clear();
        for (unit_key, meta) in metas {
            for path in meta.watched_files() {
                map.entry(path_key(path)).or_default().insert(unit_key);
            }
        }
    }
}

// ─── Applying a plan ─────────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InvalidationReport {
    pub invalidated_units: usize,
    pub checked_units: usize,
    /// The exact unit keys that were evicted from the cache. The driver uses
    /// this to purge derived side-tables (e.g. `dfm_links`) so a query after an
    /// edit never returns pre-edit links/diagnostics for an evicted unit.
    pub invalidated_keys: Vec<Identifier>,
}

/// Execute an [`InvalidationPlan`] against the cache.
pub fn apply_invalidation(
    plan: &InvalidationPlan,
    cache: &UnitCache,
    index: &ReverseDependencyIndex,
) -> InvalidationReport {
    match plan {
        InvalidationPlan::PerFile(paths) => {
            let mut report = InvalidationReport::default();
            let mut seen: HashSet<Identifier> = HashSet::new();
            for path in paths {
                for unit in index.units_for(path) {
                    if seen.insert(unit) {
                        cache.invalidate(unit);
                        report.invalidated_units += 1;
                        report.invalidated_keys.push(unit);
                    }
                }
            }
            // `Failed` entries are NOT in the reverse index (only successful
            // artifacts are), so a per-file edit can never target them — a
            // broken unit whose include the user just FIXED would keep serving
            // the stale failure until a burst/restart (H11). Drop failures so
            // they re-parse; a failure only ever re-derives (safe direction).
            // Their keys join `invalidated_keys` too, so the driver's purge loop
            // covers any derived side-table keyed off a dropped failed unit
            // (symmetry/insurance — Failed entries carry no index today).
            let dropped_failed = cache.invalidate_failed();
            report.invalidated_units += dropped_failed.len();
            report.invalidated_keys.extend(dropped_failed);
            report
        }
        InvalidationPlan::FullSweep { .. } => cache.revalidate(),
    }
}

impl UnitCache {
    /// Hash-check every cached entry against the current files; drop the
    /// stale/unreadable ones. `Failed` entries carry no file stamps — they
    /// are always dropped (conservative: the error may have been fixed).
    pub fn revalidate(&self) -> InvalidationReport {
        let mut report = InvalidationReport::default();
        for (unit_key, entry) in self.iter_entries() {
            report.checked_units += 1;
            let artifact = match &entry {
                CacheEntry::Done(artifact) => artifact,
                CacheEntry::Failed(_) => {
                    self.invalidate(unit_key);
                    report.invalidated_units += 1;
                    report.invalidated_keys.push(unit_key);
                    continue;
                }
            };
            let still_valid = std::iter::once((&artifact.source_path, artifact.source_hash))
                .chain(
                    artifact
                        .dfm
                        .iter()
                        .map(|stamp| (&stamp.path, stamp.hash)),
                )
                .chain(
                    artifact
                        .includes
                        .iter()
                        .map(|stamp| (&stamp.path, stamp.hash)),
                )
                .chain(
                    artifact
                        .dependencies
                        .iter()
                        .map(|dependency| (&dependency.source_path, dependency.source_hash)),
                )
                .all(|(path, expected)| hash_file(path).is_ok_and(|hash| hash == expected));
            if !still_valid {
                self.invalidate(unit_key);
                report.invalidated_units += 1;
                report.invalidated_keys.push(unit_key);
            }
        }
        report
    }
}

// ─── notify glue ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct WatchError {
    pub message: String,
}

/// Watches directories recursively and feeds the collector. The driver calls
/// [`Self::poll`] on its own cadence (e.g. every few hundred ms) and applies
/// the returned plan.
pub struct FileWatcher {
    // field order = drop order: stop the OS watcher before the collector goes
    _watcher: notify::RecommendedWatcher,
    collector: Arc<Mutex<ChangeCollector>>,
}

impl FileWatcher {
    pub fn start(
        directories: &[PathBuf],
        config: ChangeCollectorConfig,
    ) -> Result<Self, WatchError> {
        let collector = Arc::new(Mutex::new(ChangeCollector::new(config)));
        let handler_collector = Arc::clone(&collector);

        let mut watcher = notify::recommended_watcher(
            move |event: Result<notify::Event, notify::Error>| {
                let Ok(event) = event else {
                    return; // watch errors surface via the OS watcher dying, not per-event
                };
                if !matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Modify(_)
                        | notify::EventKind::Remove(_)
                ) {
                    return;
                }
                let now = Instant::now();
                let mut collector = handler_collector.lock().unwrap();
                for path in event.paths {
                    collector.record(path, now);
                }
            },
        )
        .map_err(|error| WatchError {
            message: error.to_string(),
        })?;

        for directory in directories {
            watcher
                .watch(directory, RecursiveMode::Recursive)
                .map_err(|error| WatchError {
                    message: format!("cannot watch {}: {error}", directory.display()),
                })?;
        }

        Ok(Self {
            _watcher: watcher,
            collector,
        })
    }

    pub fn poll(&self, now: Instant) -> Option<InvalidationPlan> {
        self.collector.lock().unwrap().poll(now)
    }

    pub fn pending_count(&self) -> usize {
        self.collector.lock().unwrap().pending_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{QualifiedName, Unit};
    use crate::meta::{CodeLocation, Span};
    use crate::unit_cache::{SourceStamp, hash_file};
    use crate::unit_meta::UnitMeta;
    use std::sync::Arc as StdArc;

    fn config(quiescence_ms: u64, burst_threshold: usize) -> ChangeCollectorConfig {
        ChangeCollectorConfig {
            quiescence: Duration::from_millis(quiescence_ms),
            burst_threshold,
            // large so the existing quiescence/burst tests are unaffected by
            // the max-defer ceiling; the L17 test sets its own small value
            max_defer: Duration::from_secs(3600),
        }
    }

    #[test]
    fn extension_filter() {
        assert!(is_watched_path(Path::new(r"c:\x\a.pas")));
        assert!(is_watched_path(Path::new(r"c:\x\A.PAS")));
        assert!(is_watched_path(Path::new(r"c:\x\a.Inc")));
        assert!(is_watched_path(Path::new(r"c:\x\a.dproj")));
        assert!(is_watched_path(Path::new(r"c:\x\a.dfm")));
        assert!(!is_watched_path(Path::new(r"c:\x\a.dcu")));
        assert!(!is_watched_path(Path::new(r"c:\x\a.txt")));
        assert!(!is_watched_path(Path::new(r"c:\x\pas"))); // no extension
    }

    #[test]
    fn small_change_flushes_per_file_after_quiescence() {
        let mut collector = ChangeCollector::new(config(500, 64));
        let start = Instant::now();
        collector.record(PathBuf::from(r"c:\x\a.pas"), start);
        collector.record(PathBuf::from(r"c:\x\b.inc"), start);
        // not quiet yet
        assert_eq!(collector.poll(start + Duration::from_millis(499)), None);
        // quiet → per-file plan with both files, deduplicated
        let Some(InvalidationPlan::PerFile(files)) =
            collector.poll(start + Duration::from_millis(500))
        else {
            panic!("expected per-file plan");
        };
        assert_eq!(files.len(), 2);
        // drained
        assert_eq!(collector.poll(start + Duration::from_secs(10)), None);
    }

    #[test]
    fn max_defer_forces_flush_under_continuous_churn() {
        // L17: events arrive faster than quiescence forever; the max_defer
        // ceiling must force a flush instead of starving invalidation.
        let mut collector = ChangeCollector::new(ChangeCollectorConfig {
            quiescence: Duration::from_millis(500),
            burst_threshold: 64,
            max_defer: Duration::from_secs(2),
        });
        let start = Instant::now();
        // an event every 100ms (< quiescence) for 2.5s: never quiet
        let mut flushed_at = None;
        for step in 0..25 {
            let now = start + Duration::from_millis(step * 100);
            collector.record(PathBuf::from(format!(r"c:\x\u{step}.pas")), now);
            if collector.poll(now).is_some() {
                flushed_at = Some(now.duration_since(start));
                break;
            }
        }
        // forced flush at/after max_defer despite never reaching quiescence
        assert!(flushed_at.is_some_and(|elapsed| elapsed >= Duration::from_secs(2)));
    }

    #[test]
    fn duplicate_events_deduplicate() {
        let mut collector = ChangeCollector::new(config(500, 64));
        let start = Instant::now();
        for _ in 0..10 {
            collector.record(PathBuf::from(r"c:\x\a.pas"), start);
            collector.record(PathBuf::from(r"c:\x\A.PAS"), start); // case variant
        }
        assert_eq!(collector.pending_count(), 1);
    }

    #[test]
    fn burst_becomes_full_sweep() {
        let mut collector = ChangeCollector::new(config(500, 10));
        let start = Instant::now();
        for index in 0..25 {
            collector.record(PathBuf::from(format!(r"c:\x\u{index}.pas")), start);
        }
        let plan = collector.poll(start + Duration::from_millis(500)).unwrap();
        assert_eq!(plan, InvalidationPlan::FullSweep { changed_files: 25 });
    }

    #[test]
    fn ongoing_burst_defers_flush() {
        // git checkout: events keep arriving → flush keeps moving out
        let mut collector = ChangeCollector::new(config(500, 10));
        let start = Instant::now();
        for second in 0..5 {
            collector.record(
                PathBuf::from(format!(r"c:\x\u{second}.pas")),
                start + Duration::from_millis(second * 400),
            );
            // 450ms after the LATEST event: never quiet, never flushes
            assert_eq!(
                collector.poll(start + Duration::from_millis(second * 400 + 450)),
                None
            );
        }
        // checkout done, quiescence reached → one flush
        assert!(
            collector
                .poll(start + Duration::from_millis(4 * 400 + 501))
                .is_some()
        );
    }

    #[test]
    fn non_watched_files_ignored() {
        let mut collector = ChangeCollector::new(config(500, 64));
        let start = Instant::now();
        collector.record(PathBuf::from(r"c:\x\a.dcu"), start);
        collector.record(PathBuf::from(r"c:\x\.git\index"), start);
        assert_eq!(collector.pending_count(), 0);
        assert_eq!(collector.poll(start + Duration::from_secs(1)), None);
    }

    // ─── reverse index + sweep against real temp files ───────────────────

    fn temp_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join("delphi_parser_watcher").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn artifact_for(unit: &str, source: &Path, include: Option<&Path>) -> UnitMeta {
        let key = crate::globals::intern_key(unit);
        let name = QualifiedName {
            name: crate::globals::intern(unit),
            key,
            location: CodeLocation {
                file: crate::meta::FileId(0),
                span: Span::new(0, 0),
            },
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
            source.to_path_buf(),
            hash_file(source).unwrap(),
            include
                .map(|path| {
                    vec![SourceStamp {
                        path: path.to_path_buf(),
                        hash: hash_file(path).unwrap(),
                    }]
                })
                .unwrap_or_default(),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn reverse_index_and_per_file_invalidation() {
        let directory = temp_directory("per_file");
        let unit_a = directory.join("UnitA.pas");
        let shared_include = directory.join("shared.inc");
        std::fs::write(&unit_a, "unit UnitA;").unwrap();
        std::fs::write(&shared_include, "{$DEFINE X}").unwrap();

                let cache = UnitCache::default();
        let index = ReverseDependencyIndex::default();

        let artifact = artifact_for("UNITA", &unit_a, Some(&shared_include));
        let unit_key = artifact.name();
        index.index_artifact(unit_key, &artifact);
        cache.insert(unit_key, StdArc::new(artifact));

        // change to the INCLUDE (different spelling) invalidates the unit
        let plan = InvalidationPlan::PerFile(vec![directory.join("SHARED.INC")]);
        let report = apply_invalidation(&plan, &cache, &index);
        assert_eq!(report.invalidated_units, 1);
        assert!(cache.get(unit_key).is_none());

        // unrelated file invalidates nothing
        let unrelated = InvalidationPlan::PerFile(vec![directory.join("other.pas")]);
        assert_eq!(
            apply_invalidation(&unrelated, &cache, &index).invalidated_units,
            0
        );
    }

    #[test]
    fn path_key_folds_verbatim_prefix() {
        // H10: an existing file canonicalizes to `\\?\C:\..`; a delete event's
        // path stays raw `C:\..`. Both must produce the same key or deletes
        // miss invalidation. (Neither path exists here, so canonicalize is a
        // no-op and only the prefix-strip runs — exactly the delete case.)
        assert_eq!(
            path_key(Path::new(r"\\?\C:\Foo\Bar.pas")),
            path_key(Path::new(r"C:\Foo\Bar.pas"))
        );
        assert_eq!(
            path_key(Path::new(r"\\?\UNC\server\share\U.pas")),
            path_key(Path::new(r"\\server\share\U.pas"))
        );
    }

    #[test]
    fn per_file_tick_drops_failed_entries() {
        // H11: a Failed unit is not reverse-indexed, so a per-file edit (e.g.
        // the user fixing its include) can't target it — the tick must drop
        // failures so they re-parse.
                let cache = UnitCache::default();
        let index = ReverseDependencyIndex::default();
        let failed_key = crate::globals::intern_key("BROKEN");
        cache.insert_failed(
            failed_key,
            StdArc::new(crate::parser::ParseError::Unexpected {
                expected: "something",
                found: None,
            }),
        );

        let plan = InvalidationPlan::PerFile(vec![PathBuf::from("anything.inc")]);
        let report = apply_invalidation(&plan, &cache, &index);
        assert_eq!(report.invalidated_units, 1);
        assert!(cache.get(failed_key).is_none());
        // L3: the dropped failed key is reported in `invalidated_keys` too, so
        // the driver's purge loop covers it (symmetry/insurance).
        assert!(
            report.invalidated_keys.contains(&failed_key),
            "dropped failed key must be reported for purge: {:?}",
            report.invalidated_keys
        );
    }

    #[test]
    fn full_sweep_revalidates_by_hash() {
        let directory = temp_directory("sweep");
        let unchanged_path = directory.join("Stable.pas");
        let edited_path = directory.join("Edited.pas");
        std::fs::write(&unchanged_path, "unit Stable;").unwrap();
        std::fs::write(&edited_path, "unit Edited;").unwrap();

                let cache = UnitCache::default();
        let index = ReverseDependencyIndex::default();

        let stable = artifact_for("STABLE", &unchanged_path, None);
        let edited = artifact_for("EDITED", &edited_path, None);
        let stable_key = stable.name();
        let edited_key = edited.name();
        cache.insert(stable_key, StdArc::new(stable));
        cache.insert(edited_key, StdArc::new(edited));

        std::fs::write(&edited_path, "unit Edited; // changed").unwrap();

        let report = apply_invalidation(
            &InvalidationPlan::FullSweep { changed_files: 999 },
            &cache,
            &index,
        );
        assert_eq!(report.checked_units, 2);
        assert_eq!(report.invalidated_units, 1);
        assert!(cache.get(stable_key).is_some());
        assert!(cache.get(edited_key).is_none());
    }
}
