//! Task 18 — idle background project indexing.
//!
//! Warms the project cache one bounded unit at a time so cross-unit features
//! (`{$IF Declared/SizeOf}`, go-to-definition, references, completion) resolve
//! against a full cache instead of degrading to Unknown under the resident-only
//! editor budget (Task-15) or the per-chain load budget (Task-22). Indexing only
//! WARMS the cache (parse + persist): it changes a query's COMPLETENESS, never
//! its correctness. A partially-indexed project is always correct, just less
//! complete.
//!
//! ## Foreground responsiveness above all
//!
//! The pass NEVER holds the session lock across the whole enumeration. Each unit
//! is parsed under a fresh `blocking_lock()` acquisition that is released before
//! the next unit, so a foreground `analyze`/read that arrives mid-pass waits at
//! most ONE unit's parse for the lock. Between units the loop also checks the
//! cancel token: a foreground event bumps it and the loop stops promptly, having
//! left NO half-state (each unit's parse+persist is atomic under the parser's own
//! per-unit discipline).
//!
//! ## Bounded RAM across a whole pass
//!
//! Each unit is parsed by `ProjectSession::parse_source_file` (already
//! memory-budgeted per Task-22), then `trim_arena()` runs between units (Task-19)
//! so the arena stays at ~one unit's disk content across the pass. The moka AST
//! cache evicts to disk (Task-16). RAM therefore stays flat as thousands of units
//! are processed — the resident set is bounded by the caps, not by the unit
//! count.
//!
//! ## The cancel/generation token
//!
//! [`IndexGeneration`] is an `AtomicU64`. `did_open`/`did_change` and every
//! feature handler call [`IndexGeneration::bump`] on entry (foreground activity).
//! The idle ticker snapshots the generation, waits for the debounce, and only
//! starts a pass if the generation is unchanged (still idle). The pass captures
//! the generation it started under and, between every unit, checks
//! [`IndexGeneration::changed_since`]; the first bump stops it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The monotonic activity/cancel token for background indexing. A foreground
/// event bumps it; the indexer compares against a snapshot taken when it began.
#[derive(Debug, Default)]
pub struct IndexGeneration {
    value: AtomicU64,
}

impl IndexGeneration {
    pub fn new() -> Self {
        IndexGeneration {
            value: AtomicU64::new(0),
        }
    }

    /// Record foreground activity: bumps the generation so any in-flight pass
    /// that snapshotted an earlier value cancels at its next between-units check,
    /// and a pending idle-debounce that snapshotted an earlier value declines to
    /// start. `Relaxed` is sufficient — the only requirement is that a later read
    /// eventually observes a value different from an earlier snapshot; there is no
    /// other memory ordered against this counter (the session lock, not this
    /// token, orders the actual parse state).
    pub fn bump(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// The current generation — snapshotted by the idle ticker before the
    /// debounce and by a pass when it starts.
    pub fn current(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    /// Whether foreground activity has occurred since `snapshot` was taken. The
    /// idle ticker uses it to decline starting a pass; the pass loop uses it to
    /// cancel between units.
    pub fn changed_since(&self, snapshot: u64) -> bool {
        self.current() != snapshot
    }
}

/// Enumerate the project's OWN `.pas` units to index: every `.pas` under
/// `project_directories` (deduplicated, deterministically sorted), EXCLUDING any
/// file under a `standard_source_directory` (the RTL/VCL tree — Task 22 handles
/// those separately, and re-indexing them every idle would be wasteful).
///
/// Determinism: the result is a `BTreeSet`-ordered `Vec<PathBuf>`, so a pass
/// processes units in a stable order across runs (the spec's "deterministic
/// order"). Only the immediate directory contents are listed (no recursion): a
/// Delphi `DCC_UnitSearchPath` entry names a directory whose `.pas` files are the
/// searchable units; recursion would sweep unrelated sibling trees.
///
/// Pure and side-effect-free apart from reading directory entries, so it is
/// unit-testable against a temp directory tree.
pub fn project_unit_paths(
    project_directories: &[PathBuf],
    standard_source_directories: &[PathBuf],
) -> Vec<PathBuf> {
    // Canonicalize the excluded standard dirs once so a path comparison is not
    // fooled by spelling (relative vs absolute, `.` segments). A dir that cannot
    // canonicalize (missing) simply never matches — safe: it excludes nothing it
    // should not.
    let excluded: BTreeSet<PathBuf> = standard_source_directories
        .iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .collect();

    let mut units: BTreeSet<PathBuf> = BTreeSet::new();
    for directory in project_directories {
        // Skip a project directory that IS (canonically) a standard source dir —
        // e.g. a dproj that redundantly lists an RTL path in its search path.
        if let Ok(canonical_dir) = directory.canonicalize() {
            if excluded.contains(&canonical_dir) {
                continue;
            }
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_pas_file(&path) {
                continue;
            }
            // Exclude a `.pas` whose (canonical) parent is a standard source dir,
            // even if it was reached through a non-standard-looking spelling.
            if let Ok(canonical) = path.canonicalize() {
                if let Some(parent) = canonical.parent() {
                    if excluded.contains(parent) {
                        continue;
                    }
                }
                units.insert(canonical);
            } else {
                units.insert(path);
            }
        }
    }
    units.into_iter().collect()
}

/// Whether `path` is a Delphi unit source file (`.pas`, case-insensitive). Only
/// `.pas` is a unit; `.dpr`/`.dpk` are program/package sources (not importable
/// units) and are excluded from the indexing work list.
fn is_pas_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.eq_ignore_ascii_case("pas"))
            .unwrap_or(false)
}

/// The Delphi unit name a source file declares, by convention its file stem
/// (`System.SysUtils.pas` → `System.SysUtils`). Used to key the freshness check:
/// a unit already resident in the cache under this (folded) key is skipped. This
/// is a convention, not a parse — an ill-named file simply fails the skip and is
/// (harmlessly) parsed, never wrongly skipped.
pub fn unit_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
}

/// The outcome of attempting to index one unit — surfaced so the ticker can
/// report progress and a test can assert what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitIndexOutcome {
    /// The unit was parsed and its AST persisted into the cache (warming work
    /// done).
    Indexed,
    /// The unit was already cached-and-fresh; skipped without re-parsing.
    AlreadyFresh,
    /// The unit failed to parse (best-effort: logged, never fatal; the pass
    /// continues to the next unit). Correctness is unaffected — a failed warm-up
    /// simply leaves that unit as un-indexed as before.
    Failed,
    /// No session was open, so nothing could be indexed (the pass ends).
    NoSession,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_bump_is_observed_by_changed_since() {
        let generation = IndexGeneration::new();
        let snapshot = generation.current();
        assert!(!generation.changed_since(snapshot), "no activity yet");
        generation.bump();
        assert!(
            generation.changed_since(snapshot),
            "a bump cancels a pass that snapshotted the earlier generation"
        );
        // A fresh snapshot after the bump is again 'unchanged' until the next bump.
        let snapshot2 = generation.current();
        assert!(!generation.changed_since(snapshot2));
    }

    #[test]
    fn enumerates_project_pas_excluding_standard_and_non_pas() {
        let root = std::env::temp_dir()
            .join("ddk-index-enum")
            .join(format!("{}", std::process::id()));
        let project = root.join("project");
        let rtl = root.join("rtl");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&rtl).unwrap();

        std::fs::write(project.join("Alpha.pas"), "unit Alpha; end.").unwrap();
        std::fs::write(project.join("Beta.PAS"), "unit Beta; end.").unwrap(); // case-insensitive
        std::fs::write(project.join("Main.dpr"), "program Main; end.").unwrap(); // not a unit
        std::fs::write(project.join("readme.txt"), "notes").unwrap(); // not a unit
        std::fs::write(rtl.join("System.SysUtils.pas"), "unit X; end.").unwrap(); // excluded

        let units = project_unit_paths(&[project.clone(), rtl.clone()], &[rtl.clone()]);

        // Alpha + Beta only — RTL unit excluded, .dpr and .txt excluded.
        assert_eq!(units.len(), 2, "only the two project .pas units: {units:?}");
        let names: BTreeSet<String> = units
            .iter()
            .map(|path| unit_stem(path).unwrap())
            .collect();
        assert!(names.contains("Alpha"));
        assert!(names.contains("Beta"));
        assert!(!names.iter().any(|name| name == "System.SysUtils"));
        assert!(!names.iter().any(|name| name == "Main"));
    }

    #[test]
    fn enumeration_is_deterministic_and_deduplicated() {
        let root = std::env::temp_dir()
            .join("ddk-index-determ")
            .join(format!("{}", std::process::id()));
        let project = root.join("p");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&project).unwrap();
        for name in ["C.pas", "A.pas", "B.pas"] {
            std::fs::write(project.join(name), "unit U; end.").unwrap();
        }

        // The SAME directory listed twice must not double-count (dedup by
        // canonical path), and the order must be stable/sorted across calls.
        let first = project_unit_paths(&[project.clone(), project.clone()], &[]);
        let second = project_unit_paths(&[project.clone()], &[]);
        assert_eq!(first, second, "deterministic + deduplicated across calls");
        assert_eq!(first.len(), 3, "three distinct units, no duplicate: {first:?}");
        // Sorted: stems come out in ascending order.
        let stems: Vec<String> = first.iter().map(|p| unit_stem(p).unwrap()).collect();
        let mut sorted = stems.clone();
        sorted.sort();
        assert_eq!(stems, sorted, "deterministic sorted order");
    }

    #[test]
    fn unit_stem_handles_dotted_unit_names() {
        assert_eq!(
            unit_stem(Path::new("/x/System.SysUtils.pas")).as_deref(),
            Some("System.SysUtils")
        );
        assert_eq!(unit_stem(Path::new("/x/Alpha.pas")).as_deref(), Some("Alpha"));
    }
}
