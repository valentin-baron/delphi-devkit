use serde::{Serialize, Deserialize};
use anyhow::Result;
use std::path::PathBuf;
use crate::projects::*;
use crate::files::dproj::{find_dproj_file, get_main_source};
use crate::utils::normalize_path;

/// Synthesised for a bare-source project: there is no `.dproj` to enumerate.
pub const BARE_CONFIGURATIONS: [&str; 2] = ["Debug", "Release"];
/// The two platforms the command-line compiler produces directly: `Win32` →
/// dcc32, `Win64` → dcc64.
pub const BARE_PLATFORMS: [&str; 2] = ["Win32", "Win64"];
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
    /// `None` means the `.dproj` file default.
    pub active_configuration: Option<String>,
    /// `None` means the `.dproj` file default.
    pub active_platform: Option<String>,
    pub start_parameters: Option<String>,
    /// The dproj's `Debugger_RunParams` (Project > Options > Run in the IDE),
    /// refreshed on [`Self::discover_paths`]. `None` for bare projects.
    pub dproj_run_params: Option<String>,
    /// The dproj's `Debugger_HostApplication` (Project > Options > Debugger),
    /// macros expanded and relative paths resolved against the project
    /// directory. Refreshed on [`Self::discover_paths`].
    pub dproj_host_application: Option<String>,
    /// DevKit-side override, taking precedence over
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
    /// The project-level override, else the dproj's defaults.
    pub fn effective_config_platform(&self, dproj: &dproj_rs::Dproj) -> (String, String) {
        let config = self.active_configuration.clone()
            .or_else(|| dproj.active_configuration().ok())
            .unwrap_or_else(|| "Debug".to_string());
        let platform = self.active_platform.clone()
            .or_else(|| dproj.active_platform().ok())
            .unwrap_or_else(|| "Win32".to_string());
        (config, platform)
    }

    /// The DevKit override wins over the dproj's `Debugger_HostApplication`.
    /// A blank value counts as absent, and so does one still holding a `$(…)`
    /// macro: it is no launchable path and must not shadow the project's exe.
    pub fn effective_host_application(&self) -> Option<String> {
        let usable = |s: &String| !s.trim().is_empty() && !s.contains("$(");
        self.host_application.clone()
            .filter(usable)
            .or_else(|| self.dproj_host_application.clone().filter(usable))
    }

    /// `ide_env` holds the IDE environment-variable overrides of this
    /// project's compiler — see
    /// [`CompilerConfiguration::ide_environment_overrides`]; without a
    /// compiler context, [`crate::utils::ide_environment_overrides`].
    pub fn discover_paths(&mut self, ide_env: &[(String, String)]) -> Result<()> {
        let config = self.active_configuration.clone();
        let platform = self.active_platform.clone();
        self.discover_paths_inner(config.as_deref(), platform.as_deref(), ide_env)
    }

    pub fn discover_paths_for(&mut self, config: &str, platform: &str, ide_env: &[(String, String)]) -> Result<()> {
        self.discover_paths_inner(Some(config), Some(platform), ide_env)
    }

    fn discover_paths_inner(&mut self, config: Option<&str>, platform: Option<&str>, ide_env: &[(String, String)]) -> Result<()> {
        if self.dproj.is_none() {
            // Adopt a sibling `.dproj` if there is one; its absence is no
            // error, a bare `.dpr`/`.dpk` being a valid project.
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
            // A `.dpr` yields an exe (and matching `.ini`) beside the source;
            // a `.dpk` has no standalone executable. The dproj-derived fields
            // are cleared so values of a dproj since removed cannot linger.
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
                match Self::discover_exe(&dproj_path, config, platform, &self.directory, ide_env)? {
                    Some(exe_path) => {
                        self.exe = Some(exe_path.to_string_lossy().to_string());
                        self.ini = Some(exe_path.with_extension("ini").to_string_lossy().to_string());
                    }
                    _ => {
                        self.exe = None;
                        self.ini = None;
                    }
                }
                (self.dproj_run_params, self.dproj_host_application) =
                    Self::discover_debugger_settings(&dproj_path, config, platform, &self.directory, ide_env);
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
                    Self::discover_debugger_settings(&dproj_path, config, platform, &self.directory, ide_env);
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
        ide_env: &[(String, String)],
    ) -> (Option<String>, Option<String>) {
        let Some(dproj) = Self::load_dproj_with_ide_environment(dproj_path, project_directory, ide_env) else {
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

    /// The executable the dproj builds for the given config/platform override
    /// (or its own defaults), evaluated with the same environment as the
    /// debugger settings, so an output directory under `$(VEGADIR)` resolves
    /// like the host application does. `None` when the dproj names no
    /// output, or when its path depends on a `$(NAME)` nothing defines: that
    /// is not a path, and must not pass for one.
    fn discover_exe(
        dproj_path: &PathBuf,
        config: Option<&str>,
        platform: Option<&str>,
        project_directory: &str,
        ide_env: &[(String, String)],
    ) -> Result<Option<PathBuf>> {
        let explicit = config.is_some() || platform.is_some();
        let dproj = match Self::load_dproj_with_ide_environment(dproj_path, project_directory, ide_env) {
            Some(dproj) => dproj,
            _ if explicit => anyhow::bail!("Failed to parse dproj: {}", dproj_path.display()),
            _ => return Ok(None),
        };
        let (cfg, plat) = Self::effective_cfg_plat(&dproj, config, platform);
        let exe = dproj
            .get_exe_path_for(&cfg, &plat)
            .ok()
            .map(normalize_path)
            // A collapsed path is deliberately not filtered here: `Project`
            // has no report to say so with, and dropping the executable
            // silently leaves the debug target refusing with "compile it
            // first" for a project that is compiled. The describe holds the
            // raw value and warns there.
            .filter(|exe| !crate::files::dproj::has_unresolved_macro(&exe.to_string_lossy()));
        Ok(exe)
    }

    /// Parse a `.dproj` seeding the `$(NAME)` expansion map with everything
    /// the IDE-launched MSBuild would see: the process environment first,
    /// overridden by the Delphi IDE's own environment-variable overrides
    /// (`ide_env` — they exist only inside the IDE's process, so they are
    /// read back from the registry of the relevant compiler configuration),
    /// plus the project-context properties (`ProjectDir`, `ProjectName`)
    /// that dproj-rs cannot derive on its own. Names are matched whatever
    /// their casing, and one that resolves to nothing stays in the value as
    /// `$(NAME)` rather than vanishing from it — see
    /// [`crate::files::dproj::seed_environment`].
    fn load_dproj_with_ide_environment(
        dproj_path: &PathBuf,
        project_directory: &str,
        ide_env: &[(String, String)],
    ) -> Option<dproj_rs::Dproj> {
        let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
        for (name, value) in ide_env {
            env.insert(name.clone(), value.clone());
        }
        let project_name = dproj_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        env.insert("ProjectDir".to_string(), project_directory.to_string());
        env.insert("ProjectName".to_string(), project_name);
        crate::files::dproj::load_with_environment(dproj_path, env).ok()
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

