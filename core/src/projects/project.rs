use serde::{Serialize, Deserialize};
use anyhow::Result;
use std::path::PathBuf;
use crate::projects::*;
use crate::files::dproj::{find_dproj_file, get_main_source, get_exe_path, get_exe_path_for};
use crate::utils::normalize_path;

/// Build configurations offered for a bare-source project (no `.dproj`).
/// DevKit synthesises these because there is no project file to enumerate, and
/// they map onto the dcc switches produced by the compiler for such projects.
pub const BARE_CONFIGURATIONS: [&str; 2] = ["Debug", "Release"];
/// Target platforms offered for a bare-source project. Limited to the two the
/// command-line compiler can produce directly: `Win32` → dcc32, `Win64` → dcc64.
pub const BARE_PLATFORMS: [&str; 2] = ["Win32", "Win64"];
/// Default configuration/platform when a bare project has no override set.
pub const BARE_DEFAULT_CONFIGURATION: &str = "Debug";
pub const BARE_DEFAULT_PLATFORM: &str = "Win32";

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct ProjectLink {
    pub id: usize,
    pub project_id: usize,
}

impl ProjectLink {
    pub fn get_project<'a>(&self, projects_data: &'a ProjectsData) -> Option<&'a Project> {
        return projects_data.projects.iter().find(|proj| proj.id == self.project_id);
    }
    pub fn get_project_mut<'a>(&self, projects_data: &'a mut ProjectsData) -> Option<&'a mut Project> {
        return projects_data.projects.iter_mut().find(|proj| proj.id == self.project_id);
    }
    pub fn get_workspace<'a>(&self, projects_data: &'a ProjectsData) -> Option<&'a Workspace> {
        for workspace in &projects_data.workspaces {
            if workspace.project_links.iter().any(|link| link.id == self.id) {
                return Some(workspace);
            }
        }
        return None;
    }
    pub fn get_workspace_mut<'a>(&self, projects_data: &'a mut ProjectsData) -> Option<&'a mut Workspace> {
        for workspace in &mut projects_data.workspaces {
            if workspace.project_links.iter().any(|link| link.id == self.id) {
                return Some(workspace);
            }
        }
        return None;
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub id: usize,
    pub name: String,
    pub directory: String,
    pub dproj: Option<String>,
    pub dpr: Option<String>,
    pub dpk: Option<String>,
    pub exe: Option<String>,
    pub ini: Option<String>,
    /// Per-project build configuration override (e.g. "Debug", "Release").
    /// `None` means use the `.dproj` file default.
    pub active_configuration: Option<String>,
    /// Per-project build platform override (e.g. "Win32", "Win64").
    /// `None` means use the `.dproj` file default.
    pub active_platform: Option<String>,
    /// Command-line arguments passed to the executable when run via RunProgram.
    pub start_parameters: Option<String>,
    /// `Debugger_RunParams` read from the dproj's active property group (the
    /// "Run Parameters" set via Project > Options > Run in the Delphi IDE).
    /// Refreshed on [`Self::discover_paths`]; used as the fallback when
    /// `start_parameters` is unset. `None` for bare (dproj-less) projects.
    pub dproj_run_params: Option<String>,
}

impl Default for Project {
    fn default() -> Self {
        Project {
            id: 0,
            name: String::new(),
            directory: String::new(),
            dproj: None,
            dpr: None,
            dpk: None,
            exe: None,
            ini: None,
            active_configuration: None,
            active_platform: None,
            start_parameters: None,
            dproj_run_params: None,
        }
    }
}

impl Project {
    /// Resolve the effective (configuration, platform) for this project.
    /// Falls back to the dproj file's defaults when the project-level
    /// override is `None`.
    pub fn effective_config_platform(&self, dproj: &dproj_rs::Dproj) -> (String, String) {
        let config = self.active_configuration.clone()
            .or_else(|| dproj.active_configuration().ok())
            .unwrap_or_else(|| "Debug".to_string());
        let platform = self.active_platform.clone()
            .or_else(|| dproj.active_platform().ok())
            .unwrap_or_else(|| "Win32".to_string());
        (config, platform)
    }

    pub fn discover_paths(&mut self) -> Result<()> {
        let config = self.active_configuration.clone();
        let platform = self.active_platform.clone();
        self.discover_paths_inner(config.as_deref(), platform.as_deref())
    }

    /// Discover paths using an explicit config/platform override.
    pub fn discover_paths_for(&mut self, config: &str, platform: &str) -> Result<()> {
        self.discover_paths_inner(Some(config), Some(platform))
    }

    fn discover_paths_inner(&mut self, config: Option<&str>, platform: Option<&str>) -> Result<()> {
        if self.dproj.is_none() {
            // A sibling `.dproj` may exist next to the main source; adopt it if so.
            // Its absence is not an error: a bare `.dpr`/`.dpk` is a valid project.
            if let Some(dpr_path) = &self.dpr {
                if let Ok(dproj_path) = find_dproj_file(&PathBuf::from(dpr_path)) {
                    self.dproj = Some(normalize_path(&dproj_path).to_string_lossy().to_string());
                }
            } else if let Some(dpk_path) = &self.dpk {
                if let Ok(dproj_path) = find_dproj_file(&PathBuf::from(dpk_path)) {
                    self.dproj = Some(normalize_path(&dproj_path).to_string_lossy().to_string());
                }
            }
        }
        if self.dproj.is_none() {
            // No `.dproj`: resolve paths straight from the bare source. A `.dpr`
            // yields an executable (and matching `.ini`) alongside the source;
            // a `.dpk` produces a package with no standalone executable.
            if let Some(dpr_path) = &self.dpr {
                let exe = PathBuf::from(dpr_path).with_extension("exe");
                self.ini = Some(exe.with_extension("ini").to_string_lossy().to_string());
                self.exe = Some(exe.to_string_lossy().to_string());
                return Ok(());
            } else if self.dpk.is_some() {
                self.exe = None;
                self.ini = None;
                return Ok(());
            }
            anyhow::bail!("Cannot discover paths - no dproj, dpr or dpk available for project id: {}", self.id);
        }
        let dproj_path = PathBuf::from(self.dproj.as_ref().unwrap());

        let main_source = get_main_source(&dproj_path)?;
        match main_source.extension().and_then(|ext| ext.to_str()).map(|s| s.to_lowercase()) {
            Some(ext) if ext == "dpr" => {
                self.dpr = Some(main_source.to_string_lossy().to_string());
                self.dpk = None;
                // Resolve the exe path, respecting any config/platform overrides.
                // When only one is provided, fill the other from the dproj defaults.
                let exe_result = if config.is_some() || platform.is_some() {
                    let dproj = dproj_rs::Dproj::from_file(&dproj_path)
                        .map_err(|e| anyhow::anyhow!("Failed to parse dproj: {}", e))?;
                    let cfg = config
                        .map(|s| s.to_string())
                        .or_else(|| dproj.active_configuration().ok())
                        .unwrap_or_else(|| "Debug".to_string());
                    let plat = platform
                        .map(|s| s.to_string())
                        .or_else(|| dproj.active_platform().ok())
                        .unwrap_or_else(|| "Win32".to_string());
                    get_exe_path_for(&dproj_path, &cfg, &plat)
                } else {
                    get_exe_path(&dproj_path)
                };
                if let Ok(exe_path) = exe_result {
                    let exe_file_name = exe_path;
                    self.exe = Some(exe_file_name.to_string_lossy().to_string());
                    self.ini = Some(exe_file_name.with_extension("ini").to_string_lossy().to_string());
                } else {
                    self.exe = None;
                    self.ini = None;
                }
                self.dproj_run_params = Self::discover_run_params(&dproj_path, config, platform);
            },
            Some(ext) if ext == "dpk" => {
                self.dpk = Some(main_source.to_string_lossy().to_string());
                self.dpr = None;
                self.exe = None;
                self.ini = None;
                self.dproj_run_params = None;
            },
            _ => {
                anyhow::bail!("Cannot discover paths - main source file is not a DPR or DPK for project id: {}", self.id);
            }
        }

        return Ok(());
    }

    /// Reads `Debugger_RunParams` from the dproj's active property group for
    /// the given config/platform override (or the dproj's own defaults when
    /// both are `None`) — the same "Run Parameters" a developer sets via
    /// Project > Options > Run in the Delphi IDE. Returns `None` on any parse
    /// failure or when the value is absent/blank.
    fn discover_run_params(dproj_path: &PathBuf, config: Option<&str>, platform: Option<&str>) -> Option<String> {
        let dproj = dproj_rs::Dproj::from_file(dproj_path).ok()?;
        let cfg = config
            .map(|s| s.to_string())
            .or_else(|| dproj.active_configuration().ok())
            .unwrap_or_else(|| "Debug".to_string());
        let plat = platform
            .map(|s| s.to_string())
            .or_else(|| dproj.active_platform().ok())
            .unwrap_or_else(|| "Win32".to_string());
        dproj
            .active_property_group_for(&cfg, &plat)
            .ok()?
            .debugger_options
            .run_params
            .filter(|s| !s.trim().is_empty())
    }

    pub fn get_project_file(&self) -> Result<PathBuf> {
        if let Some(dproj_path) = &self.dproj {
            let path = PathBuf::from(dproj_path);
            if path.exists() {
                return Ok(path);
            }
        }
        if let Some(dpr_path) = &self.dpr {
            let path = PathBuf::from(dpr_path);
            if path.exists() {
                return Ok(path);
            }
        }
        if let Some(dpk_path) = &self.dpk {
            let path = PathBuf::from(dpk_path);
            if path.exists() {
                return Ok(path);
            }
        }
        anyhow::bail!("Cannot get project file - no dproj, dpr or dpk available for project id: {}", self.id);
    }
}