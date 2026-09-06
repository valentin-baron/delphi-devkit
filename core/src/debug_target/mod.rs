//! The **debug target** of a Delphi project: a debugger-agnostic description
//! of what debugging that project means — which executable to launch or
//! attach to, the symbol files next to it, the project's own module when the
//! project is a package or a DLL, where the sources are, the arguments, and
//! what is missing or stale. It is the same information the IDE gathers for
//! *Run with debugger*, computed from DevKit's project state, the dproj, the
//! compiler's `rsvars.bat` and the IDE's per-user registry settings.
//!
//! Nothing here belongs to a particular debugger. A debug adapter's VS Code
//! extension (or its MCP server) asks DevKit for the target and maps it onto
//! its own launch attributes; hand-written configurations shrink to a
//! project reference. The callers — CLI `ddk debug-target`, the MCP
//! `delphi_get_debug_target` tool, the LSP `debug/target` method — are thin
//! wrappers around [`crate::commands::cmd_debug_target`].

mod ide_registry;

pub use ide_registry::{IdeLibrarySettings, IdeRegistryRoot};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::projects::{CompilerConfiguration, Project};
use crate::utils::normalize_path;

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

/// The symbol files a debugger reads next to the launched executable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolFiles {
    /// The linker map (`DCC_MapFile=3`): source lines and public names.
    pub map: String,
    /// The remote-debug symbols (`DCC_RemoteDebug`): locals, types, expressions.
    pub rsm: String,
}

/// A module the target process loads at run time whose debug information the
/// debugger should bind up front — the project's own package or DLL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugModule {
    /// The module's file name (`libAbout.bpl`), how a loaded module is matched.
    pub name: String,
    /// The built module on disk, when found.
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
    /// Host Application that loads a package or a DLL.
    pub executable: String,
    /// The Host Application when the executable is one.
    pub host_application: Option<String>,
    /// Product name of the compiler the project builds with.
    pub compiler: String,
    pub config: String,
    pub platform: String,
    /// `32` or `64` for a Windows platform; `None` (with a warning) otherwise.
    pub bitness: Option<u8>,
    /// Symbol files expected next to `executable`.
    pub symbols: SymbolFiles,
    /// The project directory.
    pub source_root: String,
    /// Existing directories to resolve unit sources from, most specific
    /// first: the project directory, the dproj's unit search and include
    /// paths, the IDE's Library Path and Browsing Path for the platform, and
    /// the compiler's `source` tree.
    pub source_search_paths: Vec<String>,
    /// The project's own package/DLL for a non-program target.
    pub modules: Vec<DebugModule>,
    /// Command-line arguments: the dproj's `Debugger_RunParams` fused with the
    /// saved Start Parameters, exactly as `Run` passes them.
    pub args: Vec<String>,
    /// Human-readable problems that will degrade or break a session:
    /// missing executable or module, missing or stale symbols, a non-Windows
    /// platform.
    pub warnings: Vec<String>,
}

impl std::fmt::Display for DebugTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            DebugTargetKind::Program => "program",
            DebugTargetKind::Package => "package",
            DebugTargetKind::Library => "library",
        };
        writeln!(f, "Debug target for \"{}\" ({kind}, {} {}, {}):", self.project, self.config, self.platform, self.compiler)?;
        writeln!(f, "  project file: {}", self.project_file)?;
        writeln!(f, "  executable:   {}", self.executable)?;
        if let Some(host) = &self.host_application {
            writeln!(f, "  host app:     {host}")?;
        }
        writeln!(f, "  map / rsm:    {} / {}", self.symbols.map, self.symbols.rsm)?;
        for module in &self.modules {
            writeln!(f, "  module:       {} -> {}", module.name, module.binary.as_deref().unwrap_or("(not built)"))?;
        }
        if !self.args.is_empty() {
            writeln!(f, "  args:         {}", self.args.join(" "))?;
        }
        writeln!(f, "  source root:  {}", self.source_root)?;
        writeln!(f, "  source paths: {} directories", self.source_search_paths.len())?;
        if !self.warnings.is_empty() {
            writeln!(f, "Warnings:")?;
            for warning in &self.warnings {
                writeln!(f, "- {warning}")?;
            }
        }
        Ok(())
    }
}

// ─── Building ────────────────────────────────────────────────────────────────

/// Everything the builder needs, resolved once: the effective configuration
/// and platform, the dproj evaluated for them, the macro map that expands
/// `$(NAME)` the way the IDE would, and the IDE's registry settings.
struct TargetContext<'a> {
    project: &'a Project,
    config: String,
    platform: String,
    /// The dproj's merged property group for config/platform, `$(…)` expanded.
    group: Option<dproj_rs::dproj::PropertyGroup>,
    macros: MacroMap,
    library: IdeLibrarySettings,
}

/// Describes the debug target of a project that builds with `compiler`.
/// Pure with respect to global state: everything needed is passed in.
pub fn build_debug_target(project: &Project, compiler: &CompilerConfiguration) -> Result<DebugTarget> {
    let context = TargetContext::new(project, compiler);
    let mut warnings = Vec::new();

    let kind = context.kind();
    let host_application = project.effective_host_application().or_else(|| context.dproj_host_application());
    let executable = match (kind, &host_application, &project.exe) {
        (DebugTargetKind::Program, Some(host), _) => host.clone(),
        (DebugTargetKind::Program, None, Some(exe)) => exe.clone(),
        (DebugTargetKind::Program, None, None) => bail!(
            "Project \"{}\" has no executable to debug. Compile it first.",
            project.name
        ),
        (_, Some(host), _) => host.clone(),
        (_, None, _) => bail!(
            "{} \"{}\" has no Host Application to debug through. Set one via Project > Options > Debugger \
             in the Delphi IDE, or DevKit's \"Set Host Application\".",
            if kind == DebugTargetKind::Package { "Package" } else { "Library" },
            project.name
        ),
    };
    check_executable_artefacts(&executable, kind == DebugTargetKind::Program, &mut warnings);

    let bitness = match context.platform.to_lowercase().as_str() {
        "win32" => Some(32),
        "win64" | "win64x" => Some(64),
        other => {
            warnings.push(format!("Platform {other} is not a Windows target; a Windows debugger cannot debug it."));
            None
        }
    };

    let modules = match kind {
        DebugTargetKind::Program => Vec::new(),
        DebugTargetKind::Package => context.package_module(&executable, &mut warnings).into_iter().collect(),
        DebugTargetKind::Library => context.library_module(&mut warnings).into_iter().collect(),
    };

    let source_search_paths = context.source_search_paths();

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
        symbols: SymbolFiles {
            map: json_path(&sibling(&executable, "map")),
            rsm: json_path(&sibling(&executable, "rsm")),
        },
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
        warnings,
    })
}

impl<'a> TargetContext<'a> {
    fn new(project: &'a Project, compiler: &'a CompilerConfiguration) -> Self {
        let rsvars = rsvars_of(compiler);
        let ide_env = compiler.ide_environment_overrides();
        let dproj = project.dproj.as_ref().and_then(|path| load_dproj(path, project, &rsvars, &ide_env));
        let (config, platform) = effective_config_platform(project, dproj.as_ref());
        let group = dproj
            .as_ref()
            .and_then(|dproj| dproj.active_property_group_for(&config, &platform).ok());
        let macros = MacroMap::new(project, compiler, &rsvars, &ide_env, &config, &platform);
        let library = IdeRegistryRoot::for_compiler(compiler).library_settings(&platform);
        TargetContext { project, config, platform, group, macros, library }
    }

    fn kind(&self) -> DebugTargetKind {
        if self.project.dpk.is_some() {
            return DebugTargetKind::Package;
        }
        let generates_dll = self
            .group
            .as_ref()
            .and_then(|group| group.project_properties.gen_dll.as_deref())
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if generates_dll {
            return DebugTargetKind::Library;
        }
        DebugTargetKind::Program
    }

    /// The dproj's own `Debugger_HostApplication` for the effective
    /// configuration/platform, read live and macro-expanded: the fallback
    /// when the persisted project state predates host discovery or is stale.
    fn dproj_host_application(&self) -> Option<String> {
        let raw = self.group.as_ref()?.other.get("Debugger_HostApplication")?;
        let host = self.macros.expand(raw.trim());
        if host.is_empty() || host.contains("$(") {
            return None;
        }
        Some(absolutize(&host, &self.project.directory).to_string_lossy().to_string())
    }

    /// The stem of the built package/DLL: the project name plus the
    /// `LIBSUFFIX` the dproj declares (`DllSuffix`), when it declares one.
    fn binary_stem(&self) -> String {
        let suffix = self
            .group
            .as_ref()
            .and_then(|group| group.other.get("DllSuffix"))
            .map(|suffix| suffix.trim())
            .filter(|suffix| !suffix.is_empty() && !suffix.contains("$("))
            .unwrap_or("");
        format!("{}{suffix}", self.project.name)
    }

    // ─── Modules ─────────────────────────────────────────────────────────

    /// The package's own `.bpl`, searched where the IDE puts one — the
    /// dproj's `DCC_BplOutput`, the IDE's default package output, the
    /// hosting executable's directory, `.\<Platform>\<Config>` — and its
    /// `.dcp`. Without a built `.bpl` the debugger treats the package as a
    /// black box, so that is a warning, not an error.
    fn package_module(&self, host: &str, warnings: &mut Vec<String>) -> Option<DebugModule> {
        let stem = self.binary_stem();
        let mut directories = Vec::new();
        push_dir(&mut directories, self.dproj_output(|options| options.bpl_output.clone()));
        push_dir(&mut directories, self.expanded_dir(self.library.package_dpl_output.as_deref()));
        directories.extend(self.common_output_dirs("Bpl"));
        push_dir(&mut directories, Path::new(host).parent().map(Path::to_path_buf));
        directories.push(PathBuf::from(&self.project.directory).join(&self.platform).join(&self.config));

        let binary = find_built_file(&directories, &self.project.name, &stem, "bpl");
        let Some(binary) = binary else {
            let searched: Vec<String> = directories.iter().map(|d| d.to_string_lossy().to_string()).collect();
            warnings.push(format!(
                "No built .bpl found for package \"{}\" (searched: {}). Compile it first, or the debugger will treat it as a black box.",
                self.project.name,
                searched.join("; ")
            ));
            return Some(DebugModule {
                name: format!("{stem}.bpl"),
                binary: None,
                map: None,
                rsm: None,
                dcp: None,
            });
        };
        check_module_artefacts(&binary, warnings);

        let mut dcp_directories = Vec::new();
        push_dir(&mut dcp_directories, self.dproj_output(|options| options.dcp_output.clone()));
        push_dir(&mut dcp_directories, self.expanded_dir(self.library.package_dcp_output.as_deref()));
        dcp_directories.extend(self.common_output_dirs("Dcp"));
        push_dir(&mut dcp_directories, Path::new(&binary).parent().map(Path::to_path_buf));
        let dcp = find_built_file(&dcp_directories, &self.project.name, &self.project.name, "dcp");
        if dcp.is_none() {
            warnings.push(format!(
                "No .dcp found for package \"{}\": the debugger will lack the package's rich debug information.",
                self.project.name
            ));
        }

        Some(DebugModule {
            name: file_name(&binary),
            map: Some(json_path(&sibling(&binary, "map"))),
            rsm: Some(json_path(&sibling(&binary, "rsm"))),
            dcp: dcp.as_deref().map(json_path),
            binary: Some(json_path(&binary)),
        })
    }

    /// The DLL a library project builds: DevKit records its output as the
    /// program-style `<stem>.exe`; the DLL sits in the same directory.
    fn library_module(&self, warnings: &mut Vec<String>) -> Option<DebugModule> {
        let stem = self.binary_stem();
        let output_dir = self
            .project
            .exe
            .as_deref()
            .and_then(|exe| Path::new(exe).parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(&self.project.directory).join(&self.platform).join(&self.config));
        let binary = find_built_file(&[output_dir.clone()], &self.project.name, &stem, "dll");
        let Some(binary) = binary else {
            warnings.push(format!(
                "No built .dll found for library \"{}\" in {}. Compile it first, or the debugger will treat it as a black box.",
                self.project.name,
                output_dir.to_string_lossy()
            ));
            return Some(DebugModule { name: format!("{stem}.dll"), binary: None, map: None, rsm: None, dcp: None });
        };
        check_module_artefacts(&binary, warnings);
        Some(DebugModule {
            name: file_name(&binary),
            map: Some(json_path(&sibling(&binary, "map"))),
            rsm: Some(json_path(&sibling(&binary, "rsm"))),
            dcp: None,
            binary: Some(json_path(&binary)),
        })
    }

    /// One expanded, absolutized output directory read from the dproj's
    /// merged property group (dproj-rs has already expanded `$(…)` there).
    fn dproj_output(&self, select: impl Fn(&dproj_rs::dproj::DccOptions) -> Option<String>) -> Option<PathBuf> {
        let raw = self.group.as_ref().and_then(|group| select(&group.dcc_options))?;
        self.expanded_dir(Some(&raw))
    }

    /// Expands and absolutizes a directory value; `None` when a macro stays
    /// unresolved (not a usable directory).
    fn expanded_dir(&self, raw: Option<&str>) -> Option<PathBuf> {
        let raw = raw?.trim();
        if raw.is_empty() {
            return None;
        }
        let expanded = self.macros.expand(raw);
        if expanded.contains("$(") {
            return None;
        }
        Some(absolutize(&expanded, &self.project.directory))
    }

    /// `$(BDSCOMMONDIR)\<kind>\<platform>` then `$(BDSCOMMONDIR)\<kind>`: the
    /// IDE's default package output, platform subdirectory first (Win32
    /// builds land in the root).
    fn common_output_dirs(&self, kind: &str) -> Vec<PathBuf> {
        let Some(root) = self.expanded_dir(Some(&format!("$(BDSCOMMONDIR)\\{kind}"))) else {
            return Vec::new();
        };
        vec![root.join(&self.platform), root]
    }

    // ─── Sources ─────────────────────────────────────────────────────────

    /// The directories a debugger should scan for unit sources, most
    /// specific first and without duplicates: the project directory, the
    /// dproj's unit search path and include path (a `{$I}` line is
    /// attributed to the `.inc` file, so the debugger must find it too), the
    /// IDE's Library Path and Browsing Path for the platform — the browsing
    /// path is where the sources behind third-party components live — and
    /// the compiler's own `source` tree. Only existing directories are kept.
    fn source_search_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        push_unique(&mut paths, PathBuf::from(&self.project.directory));
        let dproj_paths = [
            self.group.as_ref().and_then(|group| group.dcc_options.unit_search_path.clone()),
            self.group.as_ref().and_then(|group| group.dcc_options.include_path.clone()),
        ];
        let ide_paths = [self.library.search_path.clone(), self.library.browsing_path.clone()];
        for list in dproj_paths.into_iter().chain(ide_paths).flatten() {
            for entry in list.split(';') {
                if let Some(dir) = self.expanded_dir(Some(entry)) {
                    push_unique(&mut paths, dir);
                }
            }
        }
        if let Some(dir) = self.expanded_dir(Some("$(BDS)\\source")) {
            push_unique(&mut paths, dir);
        }
        paths
            .into_iter()
            .filter(|dir| dir.is_dir())
            .map(|dir| json_path(&dir.to_string_lossy()))
            .collect()
    }
}

// ─── Macro expansion ─────────────────────────────────────────────────────────

/// Expands `$(NAME)` references the way the IDE effectively does, from the
/// process environment, the compiler's `rsvars.bat`, the IDE's environment
/// variable overrides and the project context. Names are matched
/// case-insensitively, as on Windows; unknown names stay verbatim so the
/// caller can recognise an unusable value.
struct MacroMap {
    values: HashMap<String, String>,
}

impl MacroMap {
    fn new(
        project: &Project,
        compiler: &CompilerConfiguration,
        rsvars: &HashMap<String, String>,
        ide_env: &[(String, String)],
        config: &str,
        platform: &str,
    ) -> Self {
        let mut map = MacroMap { values: HashMap::new() };
        for (name, value) in std::env::vars() {
            map.set(&name, &value);
        }
        for (name, value) in rsvars {
            map.set(name, value);
        }
        for (name, value) in ide_env {
            map.set(name, value);
        }
        map.set_default("BDS", &compiler.installation_path);
        let bds = map.get("BDS").unwrap_or_default();
        map.set_default("BDSLIB", &format!("{bds}\\lib"));
        map.set_default("BDSINCLUDE", &format!("{bds}\\include"));
        map.set_default("BDSBIN", &format!("{bds}\\bin"));
        if let Some(documents) = dirs::document_dir() {
            let user_dir = documents.join("Embarcadero").join("Studio").join(format!("{}.0", compiler.product_version));
            map.set_default("BDSUSERDIR", &user_dir.to_string_lossy());
        }
        map.set("Config", config);
        map.set("Platform", platform);
        map.set("ProjectDir", &project.directory);
        map.set("ProjectName", &project.name);
        map
    }

    fn set(&mut self, name: &str, value: &str) {
        self.values.insert(name.to_lowercase(), value.to_string());
    }

    fn set_default(&mut self, name: &str, value: &str) {
        self.values.entry(name.to_lowercase()).or_insert_with(|| value.to_string());
    }

    fn get(&self, name: &str) -> Option<String> {
        self.values.get(&name.to_lowercase()).cloned()
    }

    /// Replaces every resolvable `$(NAME)`, repeatedly, since a value may
    /// itself reference other macros (`$(BDSLIB)\$(Platform)\release`).
    fn expand(&self, value: &str) -> String {
        let mut current = value.to_string();
        for _ in 0..8 {
            let expanded = MACRO_REGEX
                .replace_all(&current, |captures: &regex::Captures| {
                    self.get(&captures["name"]).unwrap_or_else(|| captures[0].to_string())
                })
                .to_string();
            if expanded == current {
                break;
            }
            current = expanded;
        }
        current
    }
}

lazy_static::lazy_static! {
    static ref MACRO_REGEX: regex::Regex = regex::Regex::new(r"\$\((?P<name>[A-Za-z_][A-Za-z0-9_]*)\)").unwrap();
}

// ─── Project evaluation helpers ──────────────────────────────────────────────

fn rsvars_of(compiler: &CompilerConfiguration) -> HashMap<String, String> {
    let path = PathBuf::from(&compiler.installation_path).join("bin").join("rsvars.bat");
    dproj_rs::rsvars::parse_rsvars_file(&path).unwrap_or_default()
}

/// Parses the dproj seeding `$(NAME)` expansion with what an IDE-launched
/// MSBuild sees: the process environment, `rsvars.bat`, the IDE's overrides
/// and the project context.
fn load_dproj(
    dproj_path: &str,
    project: &Project,
    rsvars: &HashMap<String, String>,
    ide_env: &[(String, String)],
) -> Option<dproj_rs::Dproj> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.extend(rsvars.clone());
    for (name, value) in ide_env {
        env.insert(name.clone(), value.clone());
    }
    env.insert("ProjectDir".to_string(), project.directory.clone());
    env.insert("ProjectName".to_string(), project.name.clone());
    dproj_rs::DprojBuilder::new().env(env).from_file(dproj_path).ok()
}

fn effective_config_platform(project: &Project, dproj: Option<&dproj_rs::Dproj>) -> (String, String) {
    match dproj {
        Some(dproj) => project.effective_config_platform(dproj),
        _ => (
            project.active_configuration.clone().unwrap_or_else(|| "Debug".to_string()),
            project.active_platform.clone().unwrap_or_else(|| "Win32".to_string()),
        ),
    }
}

// ─── Artefact checks ─────────────────────────────────────────────────────────

/// Flags a missing executable and, when it is the project's own program,
/// missing or stale `.map`/`.rsm` next to it. A host application's symbols
/// are optional: it is the loaded module that matters then.
fn check_executable_artefacts(executable: &str, own_program: bool, warnings: &mut Vec<String>) {
    if !Path::new(executable).exists() {
        warnings.push(format!("Executable not found: {executable}. Compile the project first."));
        return;
    }
    if own_program {
        check_symbols_next_to(executable, "the executable", warnings);
    }
}

fn check_module_artefacts(binary: &str, warnings: &mut Vec<String>) {
    check_symbols_next_to(binary, &file_name(binary), warnings);
}

/// How much older than its binary a symbol file may be before it counts as
/// stale. Within one build the linker writes the `.map` and `.rsm` *before*
/// it finishes the executable (measured: 0.3–0.6 s earlier on an MSBuild
/// build of a mid-sized program), and a large link takes longer than that, so
/// only a gap that cannot belong to the same build is reported.
const STALE_SYMBOLS_TOLERANCE: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Missing symbols cost features; symbols left over from an earlier build are
/// worse — breakpoints land on wrong lines and locals read as garbage.
fn check_symbols_next_to(binary: &str, what: &str, warnings: &mut Vec<String>) {
    let binary_time = modified_time(binary).map(|time| time - STALE_SYMBOLS_TOLERANCE);
    for (extension, effect) in [
        ("map", "no source lines: breakpoints and stepping will not work"),
        ("rsm", "variable inspection and expression evaluation will be severely limited"),
    ] {
        let symbol_file = sibling(binary, extension);
        if !Path::new(&symbol_file).exists() {
            warnings.push(format!(
                "Missing .{extension} next to {what} ({effect}). Compile with debug info (Compile for Debugging)."
            ));
            continue;
        }
        if let (Some(binary_time), Some(symbol_time)) = (binary_time, modified_time(&symbol_file)) {
            if symbol_time < binary_time {
                warnings.push(format!(
                    "The .{extension} next to {what} is older than the binary: stale symbols make breakpoints land on \
                     wrong lines. Recompile with debug info (Compile for Debugging)."
                ));
            }
        }
    }
}

fn modified_time(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
}

// ─── File helpers ────────────────────────────────────────────────────────────

/// The first `<stem>.<ext>` across the directories, else the newest
/// `<name>*.<ext>` — packages carry a `LIBSUFFIX` the dproj may not declare
/// literally (`$(Auto)`), so a prefix match catches `libAbout290.bpl`.
fn find_built_file(directories: &[PathBuf], name: &str, stem: &str, extension: &str) -> Option<String> {
    for dir in directories {
        let exact = dir.join(format!("{stem}.{extension}"));
        if exact.is_file() {
            return Some(normalize_path(exact).to_string_lossy().to_string());
        }
    }
    let prefix = name.to_lowercase();
    let suffix = format!(".{extension}");
    for dir in directories {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let newest = entries
            .flatten()
            .filter(|entry| {
                let file_name = entry.file_name().to_string_lossy().to_lowercase();
                file_name.starts_with(&prefix) && file_name.ends_with(&suffix)
            })
            .max_by_key(|entry| entry.metadata().and_then(|metadata| metadata.modified()).ok());
        if let Some(entry) = newest {
            return Some(normalize_path(entry.path()).to_string_lossy().to_string());
        }
    }
    None
}

fn absolutize(dir: &str, base: &str) -> PathBuf {
    let path = PathBuf::from(dir);
    let absolute = if path.is_relative() { PathBuf::from(base).join(path) } else { path };
    normalize_path(absolute)
}

fn push_dir(directories: &mut Vec<PathBuf>, dir: Option<PathBuf>) {
    if let Some(dir) = dir {
        directories.push(dir);
    }
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
mod tests {
    use super::*;

    fn compiler() -> CompilerConfiguration {
        CompilerConfiguration {
            condition: "VER360".into(),
            product_name: "Delphi 12.0 Athens".into(),
            product_version: 23,
            package_version: 290,
            compiler_version: 36,
            installation_path: r"C:\Delphi\23.0".into(),
            build_arguments: Vec::new(),
        }
    }

    fn project(dir: &str) -> Project {
        Project { id: 7, name: "Demo".into(), directory: dir.into(), ..Default::default() }
    }

    fn macros(rsvars: &[(&str, &str)]) -> MacroMap {
        let rsvars: HashMap<String, String> = rsvars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        MacroMap::new(&project(r"C:\src\demo"), &compiler(), &rsvars, &[], "Debug", "Win64")
    }

    #[test]
    fn expands_nested_and_case_insensitive_macros() {
        let map = macros(&[("BDSCOMMONDIR", r"C:\Users\Public\Documents\Embarcadero\Studio\23.0")]);
        assert_eq!(map.expand(r"$(bdslib)\$(PLATFORM)\release"), r"C:\Delphi\23.0\lib\Win64\release");
        assert_eq!(map.expand(r"$(BDSCOMMONDIR)\Bpl"), r"C:\Users\Public\Documents\Embarcadero\Studio\23.0\Bpl");
        assert_eq!(map.expand(r"$(ProjectDir)\..\shared"), r"C:\src\demo\..\shared");
    }

    #[test]
    fn unknown_macros_stay_verbatim() {
        let map = macros(&[]);
        assert_eq!(map.expand(r"$(NOSUCHVAR_DDK)\x"), r"$(NOSUCHVAR_DDK)\x");
    }

    #[test]
    fn rsvars_win_over_derived_defaults_and_project_context_wins_over_all() {
        let map = macros(&[("BDSLIB", r"D:\custom\lib"), ("Platform", "Android")]);
        assert_eq!(map.expand("$(BDSLIB)"), r"D:\custom\lib");
        assert_eq!(map.expand("$(Platform)"), "Win64");
    }

    #[test]
    fn finds_exact_stem_first_then_newest_prefixed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("libDemo290.bpl"), b"x").unwrap();
        let found = find_built_file(&[dir.to_path_buf()], "libDemo", "libDemoD29", "bpl").unwrap();
        assert!(found.ends_with("libDemo290.bpl"), "prefix fallback expected, got {found}");
        std::fs::write(dir.join("libDemoD29.bpl"), b"x").unwrap();
        let found = find_built_file(&[dir.to_path_buf()], "libDemo", "libDemoD29", "bpl").unwrap();
        assert!(found.ends_with("libDemoD29.bpl"), "exact stem expected, got {found}");
        assert!(find_built_file(&[dir.to_path_buf()], "other", "other", "bpl").is_none());
    }

    #[test]
    fn symbol_checks_report_missing_and_stale_files() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("Demo.exe");
        let map = tmp.path().join("Demo.map");
        std::fs::write(&map, b"map").unwrap();
        std::fs::write(&exe, b"exe").unwrap();
        // A map from an earlier build: well outside the same-build tolerance.
        let an_hour_ago = SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&map).unwrap().set_modified(an_hour_ago).unwrap();
        let mut warnings = Vec::new();
        check_executable_artefacts(&exe.to_string_lossy(), true, &mut warnings);
        assert!(warnings.iter().any(|w| w.contains(".map") && w.contains("older")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("Missing .rsm")), "{warnings:?}");
    }

    #[test]
    fn a_program_target_lists_its_exe_and_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_string_lossy().to_string();
        let exe = tmp.path().join("Demo.exe");
        std::fs::write(&exe, b"exe").unwrap();
        let mut project = project(&dir);
        project.dpr = Some(tmp.path().join("Demo.dpr").to_string_lossy().to_string());
        project.exe = Some(exe.to_string_lossy().to_string());
        project.dproj_run_params = Some("-a".into());
        project.start_parameters = Some("\"b c\"".into());

        let target = build_debug_target(&project, &compiler()).unwrap();
        assert_eq!(target.kind, DebugTargetKind::Program);
        assert_eq!(target.bitness, Some(32));
        assert!(target.executable.ends_with("/Demo.exe"));
        assert!(target.symbols.rsm.ends_with("/Demo.rsm"));
        assert_eq!(target.args, vec!["-a", "b c"]);
        assert!(target.modules.is_empty());
        assert_eq!(target.source_search_paths[0], json_path(&normalize_path(&dir).to_string_lossy()));
        assert!(target.warnings.iter().any(|w| w.contains("Missing .map")));
    }

    #[test]
    fn a_package_target_launches_the_host_and_binds_its_bpl() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_string_lossy().to_string();
        let host = tmp.path().join("Host.exe");
        std::fs::write(&host, b"exe").unwrap();
        let out = tmp.path().join("Win32").join("Debug");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("Demo290.bpl"), b"bpl").unwrap();
        std::fs::write(out.join("Demo.dcp"), b"dcp").unwrap();
        let mut project = project(&dir);
        project.dpk = Some(tmp.path().join("Demo.dpk").to_string_lossy().to_string());
        project.host_application = Some(host.to_string_lossy().to_string());

        let target = build_debug_target(&project, &compiler()).unwrap();
        assert_eq!(target.kind, DebugTargetKind::Package);
        assert!(target.executable.ends_with("/Host.exe"));
        assert_eq!(target.host_application, Some(target.executable.clone()));
        let module = &target.modules[0];
        assert_eq!(module.name, "Demo290.bpl");
        assert!(module.dcp.as_deref().unwrap().ends_with("/Demo.dcp"));
        // A host's own symbols are optional; the module's missing ones are reported.
        assert!(!target.warnings.iter().any(|w| w.contains("the executable")));
        assert!(target.warnings.iter().any(|w| w.contains("Demo290.bpl") && w.contains("Missing .map")));
    }

    #[test]
    fn a_package_without_host_is_an_error_not_a_target() {
        let tmp = tempfile::tempdir().unwrap();
        let mut project = project(&tmp.path().to_string_lossy());
        project.dpk = Some(tmp.path().join("Demo.dpk").to_string_lossy().to_string());
        let error = build_debug_target(&project, &compiler()).unwrap_err().to_string();
        assert!(error.contains("Host Application"), "{error}");
    }
}
