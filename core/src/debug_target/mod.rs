//! The **debug target** of a Delphi project: a debugger-agnostic description
//! of what debugging that project means — which executable to launch or
//! attach to, the symbol files next to it, the project's own module when it
//! is not that executable, where the sources are, the arguments, and what is
//! missing or stale. It is the same information the IDE gathers for *Run
//! with debugger*, computed from DevKit's project state, the dproj, the
//! compiler's `rsvars.bat` and the IDE's per-user registry settings.
//!
//! Nothing here belongs to a particular debugger. A debug adapter's VS Code
//! extension (or its MCP server) asks DevKit for the target and maps it onto
//! its own launch attributes; hand-written configurations shrink to a
//! project reference. The callers — CLI `ddk debug-target`, the MCP
//! `delphi_get_debug_target` tool, the LSP `debug/target` method — are thin
//! wrappers around [`crate::commands::cmd_debug_target`].
//!
//! Two rules hold throughout:
//!
//! * **A path in the target is a file that was found.** Symbol files, module
//!   binaries and `.dcp`s are `None` when they are not on disk; what is
//!   missing is said in `warnings`, never implied by a path that leads
//!   nowhere.
//! * **`warnings` are problems, `notes` are information.** A warning means
//!   the session will be degraded or will not work; a note explains a choice
//!   DevKit made. An empty `warnings` list therefore means ready to debug.
//!
//! Everything read from outside the project state — `rsvars.bat`, the IDE's
//! registry settings — comes in through [`IdeSettings`], so the builder is a
//! pure function of its inputs and its tests need neither a Delphi
//! installation nor `HKCU`.

mod lib_suffix;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::delphilsp::{IdeLibrarySettings, IdeRegistryRoot};
use crate::files::dproj::{has_unresolved_macro, load_with_environment, unresolved_macros};
use crate::projects::{CompilerConfiguration, IdeEnvironment, MacroMap, Project};
use crate::utils::normalize_path;
use lib_suffix::{BuildSymbols, DeclaredSuffix};

/// What kind of binary the project produces, which decides how it is
/// debugged: a program is launched itself, a package or a DLL is loaded by
/// its Host Application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DebugTargetKind {
    Program,
    Package,
    Library,
}

/// The symbol files found next to a binary; `None` for one that is not there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolFiles {
    /// The linker map (`DCC_MapFile=3`): source lines and public names.
    pub map: Option<String>,
    /// The remote-debug symbols (`DCC_RemoteDebug`): locals, types, expressions.
    pub rsm: Option<String>,
}

/// A module the target process loads at run time whose debug information the
/// debugger should bind up front: the project's own package or DLL, or the
/// project's own program when a Host Application launches it. Every path is
/// a file that exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugModule {
    /// The module's file name (`libAbout290.bpl`), how a loaded module is
    /// matched; known even when the module was never built.
    pub name: String,
    pub binary: Option<String>,
    pub map: Option<String>,
    pub rsm: Option<String>,
    /// The compiled package (`.dcp`), the rich debug information of a BPL.
    pub dcp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugTarget {
    /// The managed project's id; `None` for an ad-hoc project file.
    pub project_id: Option<usize>,
    pub project: String,
    /// The `.dproj` when there is one, else the `.dpr`/`.dpk`.
    pub project_file: String,
    /// The `.dpr`/`.dpk` main source, when known.
    pub main_source: Option<String>,
    pub kind: DebugTargetKind,
    /// The process to launch or attach to: the program's own exe, or the
    /// Host Application that loads a package, a DLL — or the program itself,
    /// when one is configured for it. The one path that may not exist yet
    /// (a program never built); `warnings` says so.
    pub executable: String,
    /// The Host Application when the executable is one.
    pub host_application: Option<String>,
    /// Product name of the compiler the project builds with.
    pub compiler: String,
    pub config: String,
    pub platform: String,
    /// `32` or `64` for a Windows platform; `None` (with a warning) otherwise.
    pub bitness: Option<u8>,
    /// The symbol files found next to `executable`.
    pub symbols: SymbolFiles,
    /// The project directory.
    pub source_root: String,
    /// Existing directories to resolve unit sources from, most specific
    /// first: the project directory, the dproj's unit search and include
    /// paths, the IDE's Library Path and Browsing Path for the platform, and
    /// the compiler's `source` tree.
    pub source_search_paths: Vec<String>,
    /// The project's own binary whenever `executable` is not it: the
    /// package or DLL a host loads, or the program a Host Application starts.
    pub modules: Vec<DebugModule>,
    /// Command-line arguments: the dproj's `Debugger_RunParams` fused with the
    /// saved Start Parameters, exactly as `Run` passes them.
    pub args: Vec<String>,
    /// What will degrade or break a session: a missing executable or module,
    /// missing, empty or stale symbols, a platform the project does not
    /// build or Windows cannot debug, a value depending on an undefined
    /// `$(NAME)`, an input that could not be read. Empty means ready.
    pub warnings: Vec<String>,
    /// What DevKit decided or left out without harm to the session: the
    /// compiler an unlinked project is described with, an IDE path that is
    /// not configured, search path entries that do not exist.
    pub notes: Vec<String>,
}

impl std::fmt::Display for DebugTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            DebugTargetKind::Program => "program",
            DebugTargetKind::Package => "package",
            DebugTargetKind::Library => "library",
        };
        let found = |file: &Option<String>| file.clone().unwrap_or_else(|| "(none)".to_string());
        writeln!(f, "Debug target for \"{}\" ({kind}, {} {}, {}):", self.project, self.config, self.platform, self.compiler)?;
        writeln!(f, "  project file: {}", self.project_file)?;
        writeln!(f, "  executable:   {}", self.executable)?;
        if let Some(host) = &self.host_application {
            writeln!(f, "  host app:     {host}")?;
        }
        writeln!(f, "  map / rsm:    {} / {}", found(&self.symbols.map), found(&self.symbols.rsm))?;
        for module in &self.modules {
            writeln!(f, "  module:       {} -> {}", module.name, module.binary.as_deref().unwrap_or("(not built)"))?;
        }
        if !self.args.is_empty() {
            writeln!(f, "  args:         {}", self.args.join(" "))?;
        }
        writeln!(f, "  source root:  {}", self.source_root)?;
        writeln!(f, "  source paths: {} directories", self.source_search_paths.len())?;
        for (title, lines) in [("Warnings", &self.warnings), ("Notes", &self.notes)] {
            if lines.is_empty() {
                continue;
            }
            writeln!(f, "{title}:")?;
            for line in lines {
                writeln!(f, "- {line}")?;
            }
        }
        Ok(())
    }
}

/// What is wrong with the target, and what is merely worth knowing.
#[derive(Debug, Default)]
struct Report {
    warnings: Vec<String>,
    notes: Vec<String>,
}

impl Report {
    fn warn(&mut self, message: impl Into<String>) {
        push_once(&mut self.warnings, message.into());
    }

    fn note(&mut self, message: impl Into<String>) {
        push_once(&mut self.notes, message.into());
    }

    /// A value that cannot be used because a `$(NAME)` in it has no definition.
    fn warn_unresolved(&mut self, what: &str, value: &str) {
        let names: Vec<String> = unresolved_macros(value).iter().map(|name| format!("$({name})")).collect();
        self.warn(format!(
            "{what} depends on {}, which nothing defines (not rsvars.bat, not the IDE's environment variables, \
             not the project): \"{value}\" was ignored.",
            names.join(", ")
        ));
    }
}

fn push_once(lines: &mut Vec<String>, line: String) {
    if !lines.contains(&line) {
        lines.push(line);
    }
}

// ─── The project to describe ─────────────────────────────────────────────────

/// The project as it has to be described: `project` itself, or — when
/// `config`/`platform` ask for another build than the active one — a copy
/// whose executable, Host Application and run parameters were discovered
/// anew **for that build**. The persisted ones belong to the active
/// configuration and platform; describing a Release/Win64 build with the
/// Debug/Win32 executable would hand a debugger the wrong binary. When the
/// discovery fails, those values are dropped rather than kept, and the
/// reason comes back as a warning.
pub fn project_to_describe(
    project: &Project,
    config: Option<String>,
    platform: Option<String>,
    ide_env: &[(String, String)],
) -> (Project, Vec<String>) {
    let mut described = project.clone();
    if config.is_none() && platform.is_none() {
        return (described, Vec::new());
    }
    if config.is_some() {
        described.active_configuration = config;
    }
    if platform.is_some() {
        described.active_platform = platform;
    }
    let mut warnings = Vec::new();
    if let Err(error) = described.discover_paths(ide_env) {
        described.exe = None;
        described.dproj_run_params = None;
        described.dproj_host_application = None;
        warnings.push(format!(
            "The project's paths could not be discovered for the requested build ({error}); \
             the executable and the Host Application of the active build were not used in their place."
        ));
    }
    (described, warnings)
}

// ─── IDE inputs ──────────────────────────────────────────────────────────────

/// The inputs that come from the Delphi installation rather than from the
/// project: its macro environment and the IDE's per-platform library
/// settings. [`InstalledIde`] reads the real ones; a test supplies values.
pub trait IdeSettings {
    /// The installation's macro environment (`rsvars.bat`, the IDE's
    /// environment-variable overrides, derived data directories).
    fn environment(&self) -> Result<IdeEnvironment>;
    /// The IDE's Library Path, Browsing Path and package output defaults
    /// for `platform`.
    fn library_settings(&self, platform: &str) -> IdeLibrarySettings;
}

/// The installation a compiler configuration describes: `rsvars.bat` on
/// disk, the IDE settings under the version's registry root.
pub struct InstalledIde<'a> {
    pub compiler: &'a CompilerConfiguration,
}

impl IdeSettings for InstalledIde<'_> {
    fn environment(&self) -> Result<IdeEnvironment> {
        IdeEnvironment::read(Path::new(&self.compiler.installation_path), &bds_version(self.compiler))
    }

    fn library_settings(&self, platform: &str) -> IdeLibrarySettings {
        IdeRegistryRoot::for_bds_version(self.compiler.product_version).library_settings(platform)
    }
}

/// `"23.0"` for Delphi 12: the registry and Documents segment of the version.
fn bds_version(compiler: &CompilerConfiguration) -> String {
    format!("{}.0", compiler.product_version)
}

// ─── Building ────────────────────────────────────────────────────────────────

/// Describes the debug target of a project that builds with `compiler`,
/// reading the installation's `rsvars.bat` and the IDE's registry settings.
/// The project's active configuration and platform are the ones described;
/// see [`project_to_describe`] for another build.
pub fn build_debug_target(project: &Project, compiler: &CompilerConfiguration) -> Result<DebugTarget> {
    build_debug_target_with(project, compiler, &InstalledIde { compiler })
}

/// [`build_debug_target`] with the IDE inputs supplied by `ide`. Pure with
/// respect to global state: everything needed is passed in.
pub fn build_debug_target_with(
    project: &Project,
    compiler: &CompilerConfiguration,
    ide: &dyn IdeSettings,
) -> Result<DebugTarget> {
    let mut report = Report::default();
    let context = TargetContext::new(project, compiler, ide, &mut report);

    let kind = context.kind();
    let host_application = context.host_application(&mut report);
    let executable = match (kind, &host_application, &project.exe) {
        (_, Some(host), _) => host.clone(),
        (DebugTargetKind::Program, None, Some(exe)) => exe.clone(),
        (DebugTargetKind::Program, None, None) => bail!(
            "Project \"{}\" has no executable to debug for {} {}. Compile it first.",
            project.name,
            context.config,
            context.platform
        ),
        (_, None, _) => bail!(
            "{} \"{}\" has no Host Application to debug through for {} {}. Set one via Project > Options > \
             Debugger in the Delphi IDE, or DevKit's \"Set Host Application\".",
            if kind == DebugTargetKind::Package { "Package" } else { "Library" },
            project.name,
            context.config,
            context.platform
        ),
    };
    // The launched executable's own symbols are required only when it is
    // the project's program; a host's are a bonus, the module's count.
    let launches_own_program = kind == DebugTargetKind::Program && host_application.is_none();
    let symbols = if Path::new(&executable).exists() {
        symbols_next_to(&executable, "the executable", launches_own_program, &mut report)
    } else {
        report.warn(format!("Executable not found: {executable}. Compile the project first."));
        SymbolFiles::default()
    };

    let bitness = match context.platform.to_lowercase().as_str() {
        "win32" => Some(32),
        "win64" | "win64x" => Some(64),
        other => {
            report.warn(format!("Platform {other} is not a Windows target; a Windows debugger cannot debug it."));
            None
        }
    };

    let modules: Vec<DebugModule> = match kind {
        DebugTargetKind::Program if launches_own_program => Vec::new(),
        DebugTargetKind::Program => vec![context.hosted_program_module(&mut report)],
        DebugTargetKind::Package => vec![context.package_module(&executable, &mut report)],
        DebugTargetKind::Library => vec![context.library_module(&mut report)],
    };

    let source_search_paths = context.source_search_paths(&mut report);

    let args = crate::commands::fuse_run_params(project.dproj_run_params.clone(), project.start_parameters.clone())
        .map(|joined| crate::commands::split_run_args(&joined))
        .unwrap_or_default();

    let main_source = project.dpr.clone().or_else(|| project.dpk.clone());
    let project_file = project
        .dproj
        .clone()
        .or_else(|| main_source.clone())
        .unwrap_or_else(|| project.directory.clone());

    Ok(DebugTarget {
        project_id: Some(project.id),
        project: project.name.clone(),
        project_file: json_path(&project_file),
        main_source: main_source.as_deref().map(json_path),
        kind,
        symbols,
        executable: json_path(&executable),
        host_application: host_application.as_deref().map(json_path),
        compiler: compiler.product_name.clone(),
        config: context.config.clone(),
        platform: context.platform.clone(),
        bitness,
        source_root: json_path(&normalize_path(&project.directory).to_string_lossy()),
        source_search_paths,
        modules,
        args,
        warnings: report.warnings,
        notes: report.notes,
    })
}

/// Everything the builder needs, resolved once: the effective configuration
/// and platform, the dproj evaluated for them, the macro map that expands
/// `$(NAME)` the way the IDE would, and the IDE's library settings. Every
/// input that fails to load is reported and replaced by the best available
/// fallback, never silently.
struct TargetContext<'a> {
    project: &'a Project,
    compiler: &'a CompilerConfiguration,
    config: String,
    platform: String,
    /// The dproj's merged property group for config/platform, `$(…)` expanded.
    group: Option<dproj_rs::dproj::PropertyGroup>,
    macros: MacroMap,
    library: IdeLibrarySettings,
}

/// A directory value once its macros are expanded.
enum Directory {
    /// Nothing was configured.
    Blank,
    Found(PathBuf),
    /// A `$(NAME)` in it has no definition; the expanded text is kept for
    /// the report.
    Unresolved(String),
}

impl<'a> TargetContext<'a> {
    fn new(
        project: &'a Project,
        compiler: &'a CompilerConfiguration,
        ide: &dyn IdeSettings,
        report: &mut Report,
    ) -> Self {
        let environment = ide.environment().unwrap_or_else(|error| {
            report.warn(format!(
                "The IDE environment of {} could not be read ({error}); $(BDS)-relative paths will stay unresolved.",
                compiler.product_name
            ));
            IdeEnvironment::default()
        });
        let mut macros = environment.macros(Path::new(&compiler.installation_path));
        macros.set("ProjectDir", project.directory.clone());
        macros.set("ProjectName", project_file_stem(project));
        // `<DllSuffix>$(Auto)</DllSuffix>`: the IDE's automatic LIBSUFFIX is
        // the package version (`290` for Delphi 12).
        macros.set("Auto", compiler.package_version.to_string());

        let dproj = project.dproj.as_deref().and_then(|path| {
            load_with_environment(&PathBuf::from(path), macros.as_env())
                .map_err(|error| {
                    report.warn(format!(
                        "Could not evaluate {path} ({error}); the target is described from DevKit's recorded \
                         project state alone, so config/platform, host application and search paths may be incomplete."
                    ));
                })
                .ok()
        });
        let (config, platform) = effective_config_platform(project, dproj.as_ref());
        if let Some(dproj) = &dproj {
            warn_about_an_unsupported_platform(dproj, &platform, report);
        }
        let group = dproj.as_ref().and_then(|dproj| {
            dproj
                .active_property_group_for(&config, &platform)
                .map_err(|error| {
                    report.warn(format!(
                        "The dproj defines no property group for {config}/{platform} ({error}); output directories, \
                         host application and search paths from the dproj are unavailable."
                    ));
                })
                .ok()
        });
        macros.set("Config", config.clone());
        macros.set("Configuration", config.clone());
        macros.set("Platform", platform.clone());

        let library = ide.library_settings(&platform);
        let registry_key = IdeRegistryRoot::for_bds_version(compiler.product_version).key_path();
        if library.search_path.is_none() {
            report.note(format!(
                "No IDE Library Path is configured for {platform} ({registry_key}); sources are looked for in the \
                 project's own paths, the Browsing Path and the compiler's source tree."
            ));
        }
        if library.browsing_path.is_none() {
            report.note(format!(
                "No IDE Browsing Path is configured for {platform} ({registry_key}); the sources of third-party \
                 libraries will not be found through it."
            ));
        }

        TargetContext { project, compiler, config, platform, group, macros, library }
    }

    /// A package by its main source or its `AppType`; a library by `AppType`
    /// or `GenDll`. Both properties sit in the dproj's unconditional group,
    /// so the kind does not depend on the platform being one the project
    /// defines settings for.
    fn kind(&self) -> DebugTargetKind {
        let properties = self.group.as_ref().map(|group| &group.project_properties);
        let app_type = properties.and_then(|properties| properties.app_type.as_deref()).unwrap_or("");
        if self.project.dpk.is_some() || app_type.eq_ignore_ascii_case("Package") {
            return DebugTargetKind::Package;
        }
        let generates_dll = properties
            .and_then(|properties| properties.gen_dll.as_deref())
            .is_some_and(|value| value.eq_ignore_ascii_case("true"));
        if app_type.eq_ignore_ascii_case("Library") || generates_dll {
            return DebugTargetKind::Library;
        }
        DebugTargetKind::Program
    }

    /// The Host Application, by the rule `Run` follows — DevKit's override,
    /// then the dproj's value DevKit recorded — and, where the record has
    /// none (it may predate host discovery), the dproj read now. A relative
    /// path is resolved against the project directory. A host depending on
    /// an undefined `$(NAME)` is no host, and is reported.
    fn host_application(&self, report: &mut Report) -> Option<String> {
        let recorded = [
            ("DevKit's Host Application override", &self.project.host_application),
            ("The dproj's Host Application", &self.project.dproj_host_application),
        ];
        for (what, host) in recorded {
            let Some(host) = host.as_deref().map(str::trim).filter(|host| !host.is_empty()) else {
                continue;
            };
            if has_unresolved_macro(host) {
                report.warn_unresolved(what, host);
                continue;
            }
            return Some(absolutize(host, &self.project.directory).to_string_lossy().to_string());
        }
        let live = self.group.as_ref()?.other.get("Debugger_HostApplication")?.trim();
        if live.is_empty() {
            return None;
        }
        if has_unresolved_macro(live) {
            report.warn_unresolved("The dproj's Host Application", live);
            return None;
        }
        Some(absolutize(live, &self.project.directory).to_string_lossy().to_string())
    }

    /// The file name stem of what the project builds: that of its main
    /// source, as the compiler names the output — not the project's display
    /// name, which can be anything.
    fn output_stem(&self) -> String {
        self.project
            .dpk
            .as_deref()
            .or(self.project.dpr.as_deref())
            .and_then(|main_source| Path::new(main_source).file_stem())
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| self.project.name.clone())
    }

    /// The stem of the built package/DLL: the output stem plus its `LIBSUFFIX`.
    fn binary_stem(&self, report: &mut Report) -> String {
        format!("{}{}", self.output_stem(), self.lib_suffix(report))
    }

    /// The `LIBSUFFIX` of the package/DLL: the dproj's `DllSuffix` (macros
    /// expanded, `$(Auto)` included), else what the main source declares
    /// with `{$LIBSUFFIX}` for this compiler and platform.
    fn lib_suffix(&self, report: &mut Report) -> String {
        let from_dproj = self
            .group
            .as_ref()
            .and_then(|group| group.other.get("DllSuffix"))
            .map(|suffix| suffix.trim())
            .filter(|suffix| !suffix.is_empty());
        if let Some(suffix) = from_dproj {
            if has_unresolved_macro(suffix) {
                report.warn_unresolved("The dproj's DllSuffix", suffix);
                return String::new();
            }
            return suffix.to_string();
        }
        let Some(main_source) = self.project.dpk.as_deref().or(self.project.dpr.as_deref()) else {
            return String::new();
        };
        let source = match std::fs::read(main_source) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
            Err(error) => {
                report.warn(format!(
                    "Could not read {main_source} ({error}) to look for a {{$LIBSUFFIX}}; the module name may lack its suffix."
                ));
                return String::new();
            }
        };
        match lib_suffix::declared_suffix(&source, &self.build_symbols()) {
            DeclaredSuffix::None => String::new(),
            DeclaredSuffix::Literal(suffix) => suffix,
            DeclaredSuffix::Auto => self.compiler.package_version.to_string(),
            DeclaredSuffix::Ambiguous(candidates) => {
                report.warn(format!(
                    "{main_source} declares several {{$LIBSUFFIX}} values ({}) under conditions DevKit cannot \
                     evaluate; the module name is given without a suffix and may be wrong.",
                    candidates.join(", ")
                ));
                String::new()
            }
        }
    }

    /// The conditional symbols in effect for this build: the compiler's
    /// version symbol, the platform's, and the project's own defines.
    fn build_symbols(&self) -> BuildSymbols {
        let mut defined = vec![self.compiler.condition.clone(), "MSWINDOWS".to_string(), "CONDITIONALEXPRESSIONS".to_string()];
        let platform_symbols: &[&str] = match self.platform.to_lowercase().as_str() {
            "win32" => &["WIN32", "CPUX86", "CPU386", "CPU32BITS"],
            "win64" | "win64x" => &["WIN64", "CPUX64", "CPU64BITS"],
            _ => &[],
        };
        defined.extend(platform_symbols.iter().map(|symbol| symbol.to_string()));
        let project_defines = self.group.as_ref().and_then(|group| group.dcc_options.define.clone()).unwrap_or_default();
        defined.extend(project_defines.split(';').map(str::trim).filter(|symbol| !symbol.is_empty()).map(str::to_string));
        BuildSymbols { defined, compiler_version: self.compiler.compiler_version as f64 }
    }

    // ─── Modules ─────────────────────────────────────────────────────────

    /// A program started by a Host Application is still the module whose
    /// symbols matter; the host launches it, DevKit knows where it is.
    fn hosted_program_module(&self, report: &mut Report) -> DebugModule {
        let Some(exe) = self.project.exe.as_deref() else {
            report.warn(format!(
                "Program \"{}\" is started by a Host Application but has no executable of its own recorded. Compile it first.",
                self.project.name
            ));
            return unbuilt_module(format!("{}.exe", self.output_stem()));
        };
        if !Path::new(exe).exists() {
            report.warn(format!("Executable not found: {exe}. Compile the project first."));
            return unbuilt_module(file_name(exe));
        }
        built_module(exe, None, report)
    }

    /// The package's own `.bpl`, searched where a build puts one — the
    /// dproj's `DCC_BplOutput`, the IDE's default package output,
    /// `.\<Platform>\<Config>` — and, last, in the hosting executable's
    /// directory; with its `.dcp`. Without a built `.bpl` the debugger
    /// treats the package as a black box, so that is a warning, not an error.
    fn package_module(&self, host: &str, report: &mut Report) -> DebugModule {
        let bpl_name = format!("{}.bpl", self.binary_stem(report));
        let host_directory = Path::new(host).parent().map(Path::to_path_buf);
        let mut directories = Vec::new();
        self.push_dproj_output(&mut directories, "DCC_BplOutput", |options| options.bpl_output.clone(), report);
        self.push_ide_output(&mut directories, "Package DPL Output", self.library.package_dpl_output.as_deref(), report);
        directories.extend(self.common_output_dirs("Bpl"));
        directories.push(PathBuf::from(&self.project.directory).join(&self.platform).join(&self.config));
        directories.extend(host_directory.clone());

        let Some(binary) = find_built_file(&directories, &bpl_name) else {
            let searched: Vec<String> = directories.iter().map(|d| d.to_string_lossy().to_string()).collect();
            report.warn(format!(
                "No built {bpl_name} found for package \"{}\" (searched: {}). Compile it first, or the debugger will treat it as a black box.",
                self.project.name,
                searched.join("; ")
            ));
            return unbuilt_module(bpl_name);
        };
        if let Some(host_directory) = host_directory {
            warn_about_a_different_copy(&binary, &host_directory.join(&bpl_name), report);
        }

        let dcp_name = format!("{}.dcp", self.output_stem());
        let mut dcp_directories = Vec::new();
        self.push_dproj_output(&mut dcp_directories, "DCC_DcpOutput", |options| options.dcp_output.clone(), report);
        self.push_ide_output(&mut dcp_directories, "Package DCP Output", self.library.package_dcp_output.as_deref(), report);
        dcp_directories.extend(self.common_output_dirs("Dcp"));
        dcp_directories.extend(Path::new(&binary).parent().map(Path::to_path_buf));
        let dcp = find_built_file(&dcp_directories, &dcp_name);
        if dcp.is_none() {
            report.warn(format!(
                "No {dcp_name} found for package \"{}\": the debugger will lack the package's rich debug information.",
                self.project.name
            ));
        }
        built_module(&binary, dcp, report)
    }

    /// The DLL a library project builds: DevKit records its output as the
    /// program-style `<stem>.exe`; the DLL sits in the same directory.
    fn library_module(&self, report: &mut Report) -> DebugModule {
        let dll_name = format!("{}.dll", self.binary_stem(report));
        let output_dir = self
            .project
            .exe
            .as_deref()
            .and_then(|exe| Path::new(exe).parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(&self.project.directory).join(&self.platform).join(&self.config));
        let Some(binary) = find_built_file(&[output_dir.clone()], &dll_name) else {
            report.warn(format!(
                "No built {dll_name} found for library \"{}\" in {}. Compile it first, or the debugger will treat it as a black box.",
                self.project.name,
                output_dir.to_string_lossy()
            ));
            return unbuilt_module(dll_name);
        };
        built_module(&binary, None, report)
    }

    /// Adds an output directory the dproj's merged property group names.
    fn push_dproj_output(
        &self,
        directories: &mut Vec<PathBuf>,
        property: &str,
        select: impl Fn(&dproj_rs::dproj::DccOptions) -> Option<String>,
        report: &mut Report,
    ) {
        let raw = self.group.as_ref().and_then(|group| select(&group.dcc_options));
        self.push_output(directories, &format!("The dproj's {property}"), raw.as_deref(), report);
    }

    /// Adds an output directory the IDE's library settings name.
    fn push_ide_output(&self, directories: &mut Vec<PathBuf>, setting: &str, raw: Option<&str>, report: &mut Report) {
        self.push_output(directories, &format!("The IDE's {setting}"), raw, report);
    }

    fn push_output(&self, directories: &mut Vec<PathBuf>, what: &str, raw: Option<&str>, report: &mut Report) {
        match self.directory(raw) {
            Directory::Found(dir) => directories.push(dir),
            Directory::Unresolved(value) => report.warn_unresolved(what, &value),
            Directory::Blank => {}
        }
    }

    /// Expands and absolutizes a directory value. A relative one is taken
    /// from the project directory: that is the compiler's working directory,
    /// for the dproj's paths and for the IDE's alike.
    fn directory(&self, raw: Option<&str>) -> Directory {
        let raw = raw.map(str::trim).unwrap_or("");
        if raw.is_empty() {
            return Directory::Blank;
        }
        let expanded = self.macros.expand(raw);
        if has_unresolved_macro(&expanded) {
            return Directory::Unresolved(expanded);
        }
        Directory::Found(absolutize(&expanded, &self.project.directory))
    }

    /// The IDE's default package output: `$(BDSCOMMONDIR)\<kind>\<platform>`,
    /// and the root `$(BDSCOMMONDIR)\<kind>` for Win32 only — that is where
    /// Win32 builds land, so for any other platform a file there is another
    /// platform's build.
    fn common_output_dirs(&self, kind: &str) -> Vec<PathBuf> {
        let Directory::Found(root) = self.directory(Some(&format!("$(BDSCOMMONDIR)\\{kind}"))) else {
            return Vec::new();
        };
        let mut directories = vec![root.join(&self.platform)];
        if self.platform.eq_ignore_ascii_case("Win32") {
            directories.push(root);
        }
        directories
    }

    // ─── Sources ─────────────────────────────────────────────────────────

    /// The directories a debugger should scan for unit sources, most
    /// specific first and without duplicates: the project directory, the
    /// dproj's unit search path and include path (a `{$I}` line is
    /// attributed to the `.inc` file, so the debugger must find it too), the
    /// IDE's Library Path and Browsing Path for the platform — the browsing
    /// path is where the sources behind third-party components live — and
    /// the compiler's own `source` tree. Only existing directories are
    /// kept; what was left out is reported.
    fn source_search_paths(&self, report: &mut Report) -> Vec<String> {
        let dcc_options = self.group.as_ref().map(|group| &group.dcc_options);
        let lists = [
            ("the dproj's unit search path", dcc_options.and_then(|options| options.unit_search_path.clone())),
            ("the dproj's include path", dcc_options.and_then(|options| options.include_path.clone())),
            ("the IDE's Library Path", self.library.search_path.clone()),
            ("the IDE's Browsing Path", self.library.browsing_path.clone()),
            ("the compiler's source tree", Some("$(BDS)\\source".to_string())),
        ];
        let mut candidates = vec![normalize_path(&self.project.directory)];
        let mut unresolved: Vec<String> = Vec::new();
        for (origin, list) in lists {
            for entry in list.unwrap_or_default().split(';') {
                match self.directory(Some(entry)) {
                    Directory::Found(dir) => push_unique(&mut candidates, dir),
                    Directory::Unresolved(value) => push_once(&mut unresolved, format!("{value} (in {origin})")),
                    Directory::Blank => {}
                }
            }
        }
        if !unresolved.is_empty() {
            report.warn(format!(
                "{} source search path entries depend on a $(NAME) nothing defines and were left out: {}.",
                unresolved.len(),
                summary(&unresolved)
            ));
        }
        let (existing, missing): (Vec<PathBuf>, Vec<PathBuf>) = candidates.into_iter().partition(|dir| dir.is_dir());
        if !missing.is_empty() {
            let missing: Vec<String> = missing.iter().map(|dir| dir.to_string_lossy().to_string()).collect();
            report.note(format!(
                "{} source search path entries do not exist and were left out: {}.",
                missing.len(),
                summary(&missing)
            ));
        }
        if existing.is_empty() {
            report.warn("No source directory exists for this project: a debugger will show no source.");
        }
        existing.iter().map(|dir| json_path(&dir.to_string_lossy())).collect()
    }
}

/// The first few of `items`, and how many more there are.
fn summary(items: &[String]) -> String {
    const SHOWN: usize = 5;
    let shown = items.iter().take(SHOWN).cloned().collect::<Vec<_>>().join("; ");
    if items.len() <= SHOWN {
        return shown;
    }
    format!("{shown}; and {} more", items.len() - SHOWN)
}

// ─── Project evaluation helpers ──────────────────────────────────────────────

fn effective_config_platform(project: &Project, dproj: Option<&dproj_rs::Dproj>) -> (String, String) {
    match dproj {
        Some(dproj) => project.effective_config_platform(dproj),
        _ => (
            project.active_configuration.clone().unwrap_or_else(|| "Debug".to_string()),
            project.active_platform.clone().unwrap_or_else(|| "Win32".to_string()),
        ),
    }
}

/// `$(ProjectName)` is the project file's name, whatever the project is
/// called in DevKit.
fn project_file_stem(project: &Project) -> String {
    [&project.dproj, &project.dpr, &project.dpk]
        .into_iter()
        .flatten()
        .find_map(|file| Path::new(file).file_stem())
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| project.name.clone())
}

/// A platform the dproj does not enable is one the project does not build:
/// its settings come from no property group of its own, and whatever sits
/// in its output directory was not made by this project's options.
fn warn_about_an_unsupported_platform(dproj: &dproj_rs::Dproj, platform: &str, report: &mut Report) {
    let platforms = dproj.platforms();
    let enabled: Vec<&str> = platforms.iter().filter(|(_, enabled)| *enabled).map(|(name, _)| *name).collect();
    if platforms.is_empty() || enabled.iter().any(|name| name.eq_ignore_ascii_case(platform)) {
        return;
    }
    report.warn(format!(
        "Platform {platform} is not enabled for this project (the dproj enables: {}); what is described is a \
         build the project does not produce.",
        if enabled.is_empty() { "none".to_string() } else { enabled.join(", ") }
    ));
}

// ─── Artefact checks ─────────────────────────────────────────────────────────

/// The module at `binary`, with the symbol files found next to it.
fn built_module(binary: &str, dcp: Option<String>, report: &mut Report) -> DebugModule {
    let symbols = symbols_next_to(binary, &file_name(binary), true, report);
    DebugModule {
        name: file_name(binary),
        binary: Some(json_path(binary)),
        map: symbols.map,
        rsm: symbols.rsm,
        dcp: dcp.as_deref().map(json_path),
    }
}

fn unbuilt_module(name: String) -> DebugModule {
    DebugModule { name, binary: None, map: None, rsm: None, dcp: None }
}

/// How far apart a binary and its symbol file may be written before they
/// cannot belong to the same build. Within one build the linker writes the
/// `.map` and `.rsm` *before* it finishes the executable (measured: 0.3–0.6 s
/// earlier on an MSBuild build of a mid-sized program), and a large link
/// takes longer than that, so only a wider gap is reported.
const SAME_BUILD_TOLERANCE: Duration = Duration::from_secs(5 * 60);

/// The `.map` and `.rsm` found next to `binary` (called `what` in the
/// messages). A file that exists but cannot belong to the binary — empty,
/// or written by another build — is reported and not returned: symbols left
/// over from an earlier build are worse than none, breakpoints land on wrong
/// lines and locals read as garbage. A missing file is a warning only when
/// the symbols are `required`.
fn symbols_next_to(binary: &str, what: &str, required: bool, report: &mut Report) -> SymbolFiles {
    let binary_time = std::fs::metadata(binary).and_then(|metadata| metadata.modified()).ok();
    let mut find = |extension: &str, effect: &str| -> Option<String> {
        let path = sibling(binary, extension);
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if required {
                    report.warn(format!(
                        "Missing .{extension} next to {what} ({effect}). Compile with debug info (Compile for Debugging)."
                    ));
                }
                return None;
            }
            Err(error) => {
                report.warn(format!("Could not inspect {path} ({error}); it was not used."));
                return None;
            }
        };
        if metadata.len() == 0 {
            report.warn(format!(
                "The .{extension} next to {what} is empty, as a build that failed leaves it. Recompile with debug info (Compile for Debugging)."
            ));
            return None;
        }
        if let Some(problem) = written_by_another_build(binary_time, metadata.modified().ok()) {
            report.warn(format!(
                "The .{extension} next to {what} is {problem}: symbols of another build make breakpoints land on \
                 wrong lines. Recompile with debug info (Compile for Debugging)."
            ));
            return None;
        }
        Some(json_path(&path))
    };
    SymbolFiles {
        map: find("map", "no source lines: breakpoints and stepping will not work"),
        rsm: find("rsm", "variable inspection and expression evaluation will be severely limited"),
    }
}

/// Says how a symbol file's time rules out the binary's build, if it does.
/// Times that cannot be read or compared rule nothing out.
fn written_by_another_build(binary: Option<SystemTime>, symbols: Option<SystemTime>) -> Option<&'static str> {
    let (binary, symbols) = (binary?, symbols?);
    match binary.duration_since(symbols) {
        Ok(older_by) if older_by > SAME_BUILD_TOLERANCE => Some("older than the binary"),
        Ok(_) => None,
        Err(newer) if newer.duration() > SAME_BUILD_TOLERANCE => {
            Some("newer than the binary, as a build that did not finish linking leaves it")
        }
        Err(_) => None,
    }
}

/// The host loads the copy in its own directory before any other: when that
/// copy is not the file described, the symbols described are not the ones
/// of the code that will run.
fn warn_about_a_different_copy(described: &str, in_host_directory: &Path, report: &mut Report) {
    let same_file = normalize_path(in_host_directory)
        .to_string_lossy()
        .eq_ignore_ascii_case(&normalize_path(described).to_string_lossy());
    if same_file || !in_host_directory.is_file() {
        return;
    }
    let fingerprint = |path: &Path| std::fs::metadata(path).ok().map(|metadata| (metadata.len(), metadata.modified().ok()));
    if fingerprint(in_host_directory) == fingerprint(Path::new(described)) {
        return;
    }
    report.warn(format!(
        "{} is a different file from the one described ({described}), and it is the one the host loads: the \
         symbols described will not match the running code. Remove or update that copy.",
        in_host_directory.to_string_lossy()
    ));
}

// ─── File helpers ────────────────────────────────────────────────────────────

/// The first directory holding exactly `file_name`. An exact name only: the
/// stem already carries the `LIBSUFFIX`, and a looser match in a shared
/// output directory would pick up another package's binary.
fn find_built_file(directories: &[PathBuf], file_name: &str) -> Option<String> {
    directories
        .iter()
        .map(|dir| dir.join(file_name))
        .find(|candidate| candidate.is_file())
        .map(|found| normalize_path(found).to_string_lossy().to_string())
}

fn absolutize(dir: &str, base: &str) -> PathBuf {
    let path = PathBuf::from(dir);
    let absolute = if path.is_relative() { PathBuf::from(base).join(path) } else { path };
    normalize_path(absolute)
}

fn push_unique(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    let candidate = normalize_path(candidate);
    let exists = paths
        .iter()
        .any(|p| p.to_string_lossy().eq_ignore_ascii_case(&candidate.to_string_lossy()));
    if !exists {
        paths.push(candidate);
    }
}

fn sibling(path: &str, extension: &str) -> String {
    PathBuf::from(path).with_extension(extension).to_string_lossy().to_string()
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Forward slashes: valid on Windows, and readable inside JSON.
fn json_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests;
