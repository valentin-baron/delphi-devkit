//! Integration with the `ddk` CLI (delphi-devkit): which compilers are
//! installed (→ standard-unit sources) and which projects/workspaces exist
//! (→ what must be parseable).
//!
//! JSON shapes observed from `ddk compiler list --json` and
//! `ddk project list --json` (ddk 2026-07). Parsing is separated from
//! process execution so the schema mapping is testable without the CLI;
//! live-CLI tests live behind the `local-tests` feature.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Debug)]
pub struct DdkError {
    pub message: String,
}

impl DdkError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// One row of `ddk compiler list --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct CompilerInstallation {
    /// ddk key, e.g. "12.0", "XE4".
    pub key: String,
    pub product_name: String,
    /// Delphi `ProductVersion` — a float (e.g. `23.0`; Delphi 2007 = `18.5`).
    pub product_version: f64,
    /// `CompilerVersion` constant, a float (Delphi 12 = `36.0`, 2007 = `18.5`).
    pub compiler_version: f64,
    pub installation_path: PathBuf,
}

impl CompilerInstallation {
    /// The VERxxx conditional symbol this compiler defines. Formula, not
    /// per-compiler data: VER<compiler_version * 10>.
    pub fn ver_define(&self) -> String {
        format!("VER{}", (self.compiler_version * 10.0).round() as i64)
    }

    /// Root of the shipped RTL/VCL/... sources.
    pub fn source_root(&self) -> PathBuf {
        self.installation_path.join("source")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectListing {
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub id: u64,
    pub name: String,
    /// Matches [`CompilerInstallation::key`].
    pub compiler_id: String,
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub id: u64,
    pub name: String,
    pub directory: PathBuf,
    /// `null` for legacy registrations without a project file (observed in
    /// live data: exe-only entries). No dproj → no ProjectContext; callers
    /// must skip such projects explicitly.
    pub dproj: Option<PathBuf>,
    /// `null` for library/package projects that produce no executable.
    pub exe: Option<PathBuf>,
    pub active: bool,
}

// ─── Parsing (pure, testable) ────────────────────────────────────────────

pub fn parse_compiler_list(json: &str) -> Result<Vec<CompilerInstallation>, DdkError> {
    // Parse row-by-row and SKIP rows that don't deserialize, rather than
    // failing the whole list. A single legacy/malformed registration (missing
    // or non-numeric version) must not make the target compiler undiscoverable.
    let rows: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|error| DdkError::new(format!("compiler list JSON: {error}")))?;
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_value(row).ok())
        .collect())
}

pub fn parse_project_listing(json: &str) -> Result<ProjectListing, DdkError> {
    serde_json::from_str(json)
        .map_err(|error| DdkError::new(format!("project list JSON: {error}")))
}

// ─── CLI execution ───────────────────────────────────────────────────────

fn run_ddk(arguments: &[&str]) -> Result<String, DdkError> {
    let output = Command::new("ddk")
        .args(arguments)
        .output()
        .map_err(|error| DdkError::new(format!("cannot run ddk {arguments:?}: {error}")))?;
    if !output.status.success() {
        return Err(DdkError::new(format!(
            "ddk {arguments:?} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut text = String::from_utf8(output.stdout)
        .map_err(|error| DdkError::new(format!("ddk output not UTF-8: {error}")))?;
    // Windows CLI tools often prepend a UTF-8 BOM; it would make serde_json
    // fail at "line 1 column 1". Strip it.
    if let Some(without_bom) = text.strip_prefix('\u{FEFF}') {
        text = without_bom.to_string();
    }
    Ok(text)
}

pub fn installed_compilers() -> Result<Vec<CompilerInstallation>, DdkError> {
    parse_compiler_list(&run_ddk(&["compiler", "list", "--json"])?)
}

pub fn project_listing() -> Result<ProjectListing, DdkError> {
    parse_project_listing(&run_ddk(&["project", "list", "--json"])?)
}

/// Find a compiler by ddk key (`Workspace::compiler_id`).
pub fn compiler_by_key<'a>(
    compilers: &'a [CompilerInstallation],
    key: &str,
) -> Option<&'a CompilerInstallation> {
    compilers.iter().find(|compiler| compiler.key == key)
}

// ─── Standard-unit source discovery ──────────────────────────────────────

/// All directories under the installation's `source` tree that directly
/// contain `.pas` files — the search-path extension for standard units
/// (System.SysUtils, Vcl.Forms, ...). Walks recursively; explicit error when
/// the source root itself is missing (broken installation), silently skips
/// unreadable subdirectories only after having found the root.
pub fn standard_source_directories(
    installation: &CompilerInstallation,
) -> Result<Vec<PathBuf>, DdkError> {
    let root = installation.source_root();
    if !root.is_dir() {
        return Err(DdkError::new(format!(
            "source root missing: {} (installation {})",
            root.display(),
            installation.product_name
        )));
    }
    let mut directories = Vec::new();
    collect_pas_directories(&root, &mut directories);
    directories.sort();
    Ok(directories)
}

fn collect_pas_directories(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return; // unreadable subdirectory below an existing root
    };
    let mut contains_pas = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pas_directories(&path, found);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pas"))
        {
            contains_pas = true;
        }
    }
    if contains_pas {
        found.push(directory.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPILER_JSON: &str = r#"[
        {
            "key": "12.0",
            "product_name": "Delphi 12.0 Athens",
            "product_version": 23,
            "compiler_version": 36,
            "installation_path": "C:\\Program Files (x86)\\Embarcadero\\Studio\\23.0"
        },
        {
            "key": "XE4",
            "product_name": "Delphi XE4",
            "product_version": 11,
            "compiler_version": 25,
            "installation_path": "C:\\Program Files (x86)\\Embarcadero\\Studio\\11.0"
        }
    ]"#;

    const PROJECT_JSON: &str = r#"{
        "workspaces": [
            {
                "id": 240,
                "name": "be.D12",
                "compiler_id": "12.0",
                "projects": [
                    {
                        "id": 214,
                        "name": "be",
                        "directory": "c:\\Delphi\\VSS\\Intern\\BE\\D12",
                        "dproj": "c:\\Delphi\\VSS\\Intern\\BE\\D12\\be.dproj",
                        "exe": "C:\\Delphi\\VSS\\Intern\\BE\\build\\bin\\D12\\be.exe",
                        "active": true
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn parses_compiler_list() {
        let compilers = parse_compiler_list(COMPILER_JSON).unwrap();
        assert_eq!(compilers.len(), 2);
        let delphi12 = compiler_by_key(&compilers, "12.0").unwrap();
        assert_eq!(delphi12.compiler_version, 36.0);
        assert_eq!(delphi12.ver_define(), "VER360");
        assert!(delphi12.source_root().ends_with("source"));
        assert!(compiler_by_key(&compilers, "99").is_none());
    }

    #[test]
    fn compiler_list_tolerates_fractional_and_bad_rows() {
        // M11: a fractional CompilerVersion (Delphi 2007 = 18.5) must parse,
        // and one malformed legacy row must NOT make the good ones vanish.
        let json = r#"[
            { "key": "18.0", "product_name": "Delphi 2007", "product_version": 11.0,
              "compiler_version": 18.5, "installation_path": "C:/D2007" },
            { "key": "broken", "product_name": "Legacy", "compiler_version": null,
              "installation_path": "C:/Legacy" },
            { "key": "12.0", "product_name": "Delphi 12", "product_version": 23.0,
              "compiler_version": 36.0, "installation_path": "C:/D12" }
        ]"#;
        let compilers = parse_compiler_list(json).unwrap();
        assert_eq!(compilers.len(), 2); // the null-version row is skipped
        assert_eq!(compiler_by_key(&compilers, "18.0").unwrap().ver_define(), "VER185");
        assert_eq!(compiler_by_key(&compilers, "12.0").unwrap().ver_define(), "VER360");
    }

    #[test]
    fn parses_project_listing() {
        let listing = parse_project_listing(PROJECT_JSON).unwrap();
        assert_eq!(listing.workspaces.len(), 1);
        let workspace = &listing.workspaces[0];
        assert_eq!(workspace.compiler_id, "12.0");
        assert!(workspace.projects[0].active);
        assert!(
            workspace.projects[0]
                .dproj
                .as_ref()
                .unwrap()
                .ends_with("be.dproj")
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_compiler_list("{not json").is_err());
        assert!(parse_project_listing("[]").is_err()); // wrong shape
    }

    #[test]
    fn source_directory_walk() {
        let root = std::env::temp_dir().join("delphi_parser_ddk_walk");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("source/rtl/common")).unwrap();
        std::fs::create_dir_all(root.join("source/rtl/empty")).unwrap();
        std::fs::write(root.join("source/rtl/common/System.SysUtils.pas"), "x").unwrap();
        std::fs::write(root.join("source/rtl/empty/readme.txt"), "x").unwrap();

        let installation = CompilerInstallation {
            key: "12.0".into(),
            product_name: "Delphi 12".into(),
            product_version: 23.0,
            compiler_version: 36.0,
            installation_path: root.clone(),
        };
        let directories = standard_source_directories(&installation).unwrap();
        assert_eq!(directories, vec![root.join("source/rtl/common")]);

        // missing source root is an explicit error, not an empty list
        let broken = CompilerInstallation {
            installation_path: root.join("nowhere"),
            ..installation
        };
        assert!(standard_source_directories(&broken).is_err());
    }
}

/// Live-CLI tests — require ddk on PATH and this machine's registrations.
/// Excluded unless built with `--features local-tests`.
#[cfg(all(test, feature = "local-tests"))]
mod local_tests {
    use super::*;

    #[test]
    fn live_compiler_list_contains_delphi12() {
        let compilers = installed_compilers().unwrap();
        let delphi12 = compiler_by_key(&compilers, "12.0").expect("Delphi 12 registered");
        assert_eq!(delphi12.compiler_version, 36.0);
        assert!(delphi12.installation_path.is_dir());
        let sources = standard_source_directories(delphi12).unwrap();
        assert!(!sources.is_empty(), "Delphi 12 ships RTL/VCL sources");
    }

    #[test]
    fn live_project_listing_has_workspaces() {
        let listing = project_listing().unwrap();
        assert!(!listing.workspaces.is_empty());
    }
}
