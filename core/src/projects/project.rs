use serde::{Serialize, Deserialize};
use anyhow::Result;
use std::path::PathBuf;
use crate::lexorank::{LexoRank, HasLexoRank};
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
    pub sort_rank: LexoRank,
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

impl HasLexoRank for ProjectLink {
    fn get_lexorank(&self) -> &LexoRank {
        &self.sort_rank
    }
    fn set_lexorank(&mut self, lexorank: LexoRank) {
        self.sort_rank = lexorank;
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
    /// `Debugger_HostApplication` read from the dproj's active property group
    /// (the "Host application" set via Project > Options > Debugger in the
    /// Delphi IDE). Common project macros are expanded and a relative path is
    /// resolved against the project directory. Refreshed on
    /// [`Self::discover_paths`]; `None` for bare (dproj-less) projects.
    pub dproj_host_application: Option<String>,
    /// DevKit-side Host Application override: the executable RunProgram
    /// launches to host this project (e.g. the application loading a `.dpk`
    /// package or a DLL). Takes precedence over
    /// [`Self::dproj_host_application`].
    pub host_application: Option<String>,
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
            dproj_host_application: None,
            host_application: None,
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

    /// The executable that hosts this project at run time, when one is
    /// configured: the DevKit override wins over the dproj's own
    /// `Debugger_HostApplication`. Blank values count as absent, and so does
    /// a value still containing an unresolved `$(...)` macro — it is not a
    /// launchable path, and must never shadow the project's own exe.
    pub fn effective_host_application(&self) -> Option<String> {
        let usable = |s: &String| !s.trim().is_empty() && !s.contains("$(");
        self.host_application.clone()
            .filter(usable)
            .or_else(|| self.dproj_host_application.clone().filter(usable))
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
            // Without a dproj there is nothing to source the dproj-derived
            // fields from: clear them so values from a previously-present
            // dproj cannot linger and affect run-target resolution.
            if let Some(dpr_path) = &self.dpr {
                let exe = PathBuf::from(dpr_path).with_extension("exe");
                self.ini = Some(exe.with_extension("ini").to_string_lossy().to_string());
                self.exe = Some(exe.to_string_lossy().to_string());
                self.dproj_run_params = None;
                self.dproj_host_application = None;
                return Ok(());
            } else if self.dpk.is_some() {
                self.exe = None;
                self.ini = None;
                self.dproj_run_params = None;
                self.dproj_host_application = None;
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
                (self.dproj_run_params, self.dproj_host_application) =
                    Self::discover_debugger_settings(&dproj_path, config, platform, &self.directory);
            },
            Some(ext) if ext == "dpk" => {
                self.dpk = Some(main_source.to_string_lossy().to_string());
                self.dpr = None;
                self.exe = None;
                self.ini = None;
                // A package has no standalone executable, but its Run
                // Parameters and Host Application (Project > Options in the
                // Delphi IDE) drive how RunProgram launches the hosting exe.
                (self.dproj_run_params, self.dproj_host_application) =
                    Self::discover_debugger_settings(&dproj_path, config, platform, &self.directory);
            },
            _ => {
                anyhow::bail!("Cannot discover paths - main source file is not a DPR or DPK for project id: {}", self.id);
            }
        }

        return Ok(());
    }

    /// Reads the debugger-related settings from the dproj's active property
    /// group for the given config/platform override (or the dproj's own
    /// defaults when both are `None`): `Debugger_RunParams` (Project >
    /// Options > Run in the Delphi IDE) and `Debugger_HostApplication`
    /// (Project > Options > Debugger). `$(NAME)` references are expanded by
    /// dproj-rs the way the IDE-launched MSBuild would resolve them — see
    /// [`Self::load_dproj_with_ide_environment`] — and a relative host path
    /// is resolved against the project directory. Blank values count as
    /// absent; both values are `None` on any parse failure.
    fn discover_debugger_settings(
        dproj_path: &PathBuf,
        config: Option<&str>,
        platform: Option<&str>,
        project_directory: &str,
    ) -> (Option<String>, Option<String>) {
        let Some(dproj) = Self::load_dproj_with_ide_environment(dproj_path, project_directory) else {
            return (None, None);
        };
        let (cfg, plat) = Self::effective_cfg_plat(&dproj, config, platform);
        let Ok(group) = dproj.active_property_group_for(&cfg, &plat) else {
            return (None, None);
        };
        let run_params = group.debugger_options.run_params.clone().filter(|s| !s.trim().is_empty());
        let host_application = group
            .other
            .get("Debugger_HostApplication")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|host| Self::absolutize_host_application(host, project_directory));
        (run_params, host_application)
    }

    /// Parse a `.dproj` seeding the `$(NAME)` expansion map with everything
    /// the IDE-launched MSBuild would see: the process environment first,
    /// overridden by the Delphi IDE's own environment-variable overrides
    /// (Tools > Options > IDE > Environment Variables — they exist only
    /// inside the IDE's process, so they are read back from the registry),
    /// plus the project-context properties (`ProjectDir`, `ProjectName`)
    /// that dproj-rs cannot derive on its own. Names that resolve to nothing
    /// expand to an empty string, matching MSBuild semantics.
    fn load_dproj_with_ide_environment(dproj_path: &PathBuf, project_directory: &str) -> Option<dproj_rs::Dproj> {
        let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
        for (name, value) in IDE_ENV_OVERRIDES.iter() {
            env.insert(name.clone(), value.clone());
        }
        let project_name = dproj_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        env.insert("ProjectDir".to_string(), project_directory.to_string());
        env.insert("ProjectName".to_string(), project_name);
        dproj_rs::DprojBuilder::new().env(env).from_file(dproj_path).ok()
    }

    /// Resolve a discovered host-application path against the project
    /// directory when relative, then normalise it.
    fn absolutize_host_application(host: &str, project_directory: &str) -> String {
        let path = PathBuf::from(host);
        let absolute = if path.is_relative() {
            PathBuf::from(project_directory).join(path)
        } else {
            path
        };
        normalize_path(&absolute).to_string_lossy().to_string()
    }

    /// Resolve the effective (configuration, platform) from explicit overrides
    /// falling back to the dproj's own defaults — shared by the dproj property
    /// discovery helpers.
    fn effective_cfg_plat(dproj: &dproj_rs::Dproj, config: Option<&str>, platform: Option<&str>) -> (String, String) {
        let cfg = config
            .map(|s| s.to_string())
            .or_else(|| dproj.active_configuration().ok())
            .unwrap_or_else(|| "Debug".to_string());
        let plat = platform
            .map(|s| s.to_string())
            .or_else(|| dproj.active_platform().ok())
            .unwrap_or_else(|| "Win32".to_string());
        (cfg, plat)
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

lazy_static::lazy_static! {
    /// The Delphi IDE's own environment-variable overrides (Tools > Options >
    /// IDE > Environment Variables), read once per process: dproj values
    /// reference them (e.g. `$(VEGADIR)`) although they exist in no real
    /// environment outside the IDE.
    static ref IDE_ENV_OVERRIDES: Vec<(String, String)> = crate::utils::ide_environment_overrides();
}