//! Generation of `.delphilsp.json` settings files.
//!
//! Embarcadero's **DelphiLSP** VS Code extension (code insight / completion)
//! reads a `<project>.delphilsp.json` file sitting next to the project's
//! `.dpr`/`.dpk`. That file is normally produced by the RAD Studio IDE when a
//! project is opened, which means code insight is dead for anyone who never
//! opens the IDE. This module reconstructs the file from the same sources the
//! IDE uses:
//!
//! * the project's `.dproj` (evaluated for the effective config/platform),
//! * the compiler installation's `bin\rsvars.bat` (`$(BDS)`, `$(BDSCOMMONDIR)`…),
//! * the IDE's **global Library Path** and user-defined environment variable
//!   overrides, both read from `HKCU\SOFTWARE\Embarcadero\BDS\<ver>`.
//!
//! The emitted `dccOptions` string mirrors what the IDE writes. It only feeds
//! code insight — it never drives a real build — so switches that cannot be
//! derived faithfully fall back to sane defaults.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;

mod registry;
pub use registry::IdeLibrarySettings;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Value of the top-level `generatedBy` key DDK stamps on every file it
/// writes, as a sibling of `settings`.
///
/// The RAD Studio IDE never writes this key, which makes it a reliable
/// ownership marker: a `.delphilsp.json` carrying it is DDK's to refresh
/// whenever the project's search paths, defines, configuration or platform go
/// stale, while a file without it was hand-made or produced by the IDE and
/// must never be overwritten automatically.
pub const GENERATED_BY_MARKER: &str = "delphi-devkit";

/// Unit aliases the RAD Studio IDE emits when a project defines none of its
/// own. Observed verbatim in IDE-generated `.delphilsp.json` files.
pub const DEFAULT_UNIT_ALIASES: &str = "Generics.Collections=System.Generics.Collections;\
Generics.Defaults=System.Generics.Defaults;\
WinTypes=Winapi.Windows;\
WinProcs=Winapi.Windows;\
DbiTypes=BDE;\
DbiProcs=BDE;\
DbiErrs=BDE";

// ---------------------------------------------------------------------------
// Macro expansion
// ---------------------------------------------------------------------------

/// A case-insensitive `$(NAME)` variable map (Windows environment semantics).
///
/// Names are also kept in their original spelling because `dproj-rs` resolves
/// `$(NAME)` through a **case-sensitive** map: seeding it needs the exact
/// casing the `.dproj` files use (`DCC_UnitSearchPath`, not `DCC_UNITSEARCHPATH`).
#[derive(Debug, Clone, Default)]
pub struct MacroMap {
    /// Upper-cased keys — the lookup used by [`MacroMap::expand`].
    vars: HashMap<String, String>,
    /// The same entries under their original spelling.
    original_case: HashMap<String, String>,
}

impl MacroMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a variable, overwriting any previous value.
    pub fn set(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        let key = key.as_ref();
        let value = value.into();
        self.vars.insert(key.to_ascii_uppercase(), value.clone());
        self.original_case.insert(key.to_string(), value);
    }

    /// Insert a variable only when that name is not already defined.
    pub fn set_default(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        if self.get(key.as_ref()).is_none() {
            self.set(key, value);
        }
    }

    /// Forget a variable, whatever casing it was defined with.
    pub fn remove(&mut self, key: &str) {
        let upper = key.to_ascii_uppercase();
        self.vars.remove(&upper);
        self.original_case.retain(|k, _| k.to_ascii_uppercase() != upper);
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.vars.get(&key.to_ascii_uppercase())
    }

    pub fn extend<I, K, V>(&mut self, entries: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        for (k, v) in entries {
            self.set(k, v);
        }
    }

    /// Expand every `$(NAME)` reference. Unknown names are left **verbatim**
    /// so callers can detect (and report) unresolved macros — this is the one
    /// behavioural difference from MSBuild, which expands them to nothing.
    ///
    /// Expansion is iterative (a resolved value may itself contain macros) and
    /// bounded so a self-referential definition cannot loop forever.
    pub fn expand(&self, value: &str) -> String {
        const MAX_PASSES: usize = 8;
        let mut current = value.to_string();
        for _ in 0..MAX_PASSES {
            if !current.contains("$(") {
                break;
            }
            let next = self.expand_once(&current);
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    fn expand_once(&self, value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        let bytes: Vec<char> = value.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '$' && i + 1 < bytes.len() && bytes[i + 1] == '(' {
                if let Some(close) = (i + 2..bytes.len()).find(|&j| bytes[j] == ')') {
                    let name: String = bytes[i + 2..close].iter().collect();
                    match self.get(&name) {
                        Some(resolved) => out.push_str(resolved),
                        // Unknown: keep the token so the caller can warn.
                        _ => out.push_str(&format!("$({name})")),
                    }
                    i = close + 1;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    /// Seed environment for `dproj-rs` property-group evaluation. Every entry
    /// appears both upper-cased and in its original spelling, because that
    /// lookup is case-sensitive.
    pub fn as_env(&self) -> HashMap<String, String> {
        let mut env = self.vars.clone();
        env.extend(self.original_case.iter().map(|(k, v)| (k.clone(), v.clone())));
        env
    }
}

// ---------------------------------------------------------------------------
// Path list helpers
// ---------------------------------------------------------------------------

/// Strip trailing path separators (`c:\foo\` → `c:\foo`) without eating a
/// drive root (`c:\` stays `c:\`).
fn trim_trailing_separator(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        path
    } else {
        trimmed
    }
}

/// Split a `;`-separated path list, expand its macros, and drop entries that
/// still contain an unresolved `$(NAME)` (collecting a warning for each).
pub fn expand_path_list(raw: &str, macros: &MacroMap, warnings: &mut Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for entry in raw.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let expanded = macros.expand(entry);
        if expanded.contains("$(") {
            warnings.push(format!(
                "Dropped search-path entry with unresolved macro: {expanded}"
            ));
            continue;
        }
        let cleaned = trim_trailing_separator(expanded.trim());
        if !cleaned.is_empty() {
            out.push(cleaned.to_string());
        }
    }
    out
}

/// Quote a dcc option value when it contains a space, the way the IDE does.
fn quote_if_needed(value: &str) -> String {
    if value.contains(' ') {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

/// Join a path list into a dcc option payload: `;`-separated, entries with
/// spaces double-quoted individually.
fn join_paths(paths: &[String]) -> String {
    paths.iter().map(|p| quote_if_needed(p)).collect::<Vec<_>>().join(";")
}

// ---------------------------------------------------------------------------
// File URI
// ---------------------------------------------------------------------------

/// Percent-encode a Windows path into the `file:///C%3A/dir/file.dpk` form the
/// IDE writes: backslashes become forward slashes and everything outside the
/// unreserved URI set is percent-encoded — including `:`, spaces, `(`, `)` and
/// `+`, all observed encoded in IDE-generated files.
pub fn path_to_file_uri(path: &Path) -> String {
    const SAFE: &str = "-._~/";
    let normalized = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(normalized.len() + 8);
    for byte in normalized.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || SAFE.contains(ch) {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("file:///{}", encoded.trim_start_matches('/'))
}

/// Like [`path_to_file_uri`] but for a directory: the IDE always terminates
/// those URIs with a slash.
pub fn dir_to_file_uri(path: &Path) -> String {
    let uri = path_to_file_uri(path);
    if uri.ends_with('/') { uri } else { format!("{uri}/") }
}

// ---------------------------------------------------------------------------
// dccOptions assembly
// ---------------------------------------------------------------------------

/// Fully resolved inputs for [`build_dcc_options`]. Everything here is already
/// expanded — the builder does no macro resolution of its own, which keeps it
/// trivially unit-testable.
#[derive(Debug, Clone, Default)]
pub struct DccOptionsInput {
    /// `true` for a `.dpk` (emits `-TX.bpl`).
    pub is_package: bool,
    /// `true` for a `.dpr` that builds a DLL (emits `-TX.dll`).
    pub is_library: bool,
    pub optimize: bool,
    pub stack_frames: bool,
    pub inlining_off: bool,
    pub range_checking: Option<bool>,
    pub overflow_checking: Option<bool>,
    /// `-A` payload (already `;`-joined).
    pub unit_aliases: String,
    /// `-D` payload (already `;`-joined, inheritance token removed).
    pub defines: String,
    /// `-NS` payload (already `;`-joined, inheritance token removed).
    pub namespaces: String,
    /// `-E` payload.
    pub exe_output: String,
    /// `-NU` payload.
    pub dcu_output: String,
    /// `-LE` payload.
    pub bpl_output: String,
    /// `-LN` payload.
    pub dcp_output: String,
    /// Prepended to `-I` and `-U` only (the IDE's "Debug DCU path"); `None`
    /// for non-debug configurations.
    pub debug_dcu_path: Option<String>,
    /// The full library/unit search path: the project's own entries followed
    /// by the IDE's global Library Path.
    pub search_paths: Vec<String>,
    /// `.dcp` names from `<DCCReference>` (emitted as `-LU`).
    pub required_packages: Vec<String>,
    /// `DCC_Description` (emitted as `--description:"…"`).
    pub description: Option<String>,
}

impl DccOptionsInput {
    /// Target extension for `-TX`.
    fn target_extension(&self) -> &'static str {
        if self.is_package {
            ".bpl"
        } else if self.is_library {
            ".dll"
        } else {
            ".exe"
        }
    }
}

/// Append `<prefix><value>` (quoted when it contains a space), skipping empty
/// values so the IDE's "absent option" behaviour is preserved.
fn push_value(parts: &mut Vec<String>, prefix: &str, value: &str) {
    if !value.is_empty() {
        parts.push(format!("{prefix}{}", quote_if_needed(value)));
    }
}

/// Assemble the single-line `dccOptions` string, in the order the RAD Studio
/// IDE emits it.
pub fn build_dcc_options(input: &DccOptionsInput) -> String {
    let mut parts: Vec<String> = Vec::new();

    let flag = |on: bool| if on { '+' } else { '-' };
    parts.push(format!("-$O{}", flag(input.optimize)));
    parts.push(format!("-$W{}", flag(input.stack_frames)));
    if input.inlining_off {
        parts.push("--inline:off".to_string());
    }
    if let Some(on) = input.range_checking {
        parts.push(format!("-$R{}", flag(on)));
    }
    if let Some(on) = input.overflow_checking {
        parts.push(format!("-$Q{}", flag(on)));
    }
    // Always present in IDE output: no dcc32.cfg, quiet, emit "never build" dcps.
    parts.push("--no-config".to_string());
    parts.push("-Q".to_string());
    parts.push("-Z".to_string());
    parts.push(format!("-TX{}", input.target_extension()));

    push_value(&mut parts, "-A", &input.unit_aliases);
    push_value(&mut parts, "-D", &input.defines);
    push_value(&mut parts, "-E", &input.exe_output);

    // -I / -U additionally see the IDE's debug DCU directory; -O / -R do not.
    let with_debug_dcus: Vec<String> = match &input.debug_dcu_path {
        Some(debug) => std::iter::once(debug.clone())
            .chain(input.search_paths.iter().cloned())
            .collect(),
        _ => input.search_paths.clone(),
    };
    let include_and_unit = join_paths(&with_debug_dcus);
    let object_and_resource = join_paths(&input.search_paths);

    if !include_and_unit.is_empty() {
        parts.push(format!("-I{include_and_unit}"));
    }
    push_value(&mut parts, "-LE", &input.bpl_output);
    push_value(&mut parts, "-LN", &input.dcp_output);
    push_value(&mut parts, "-NU", &input.dcu_output);
    push_value(&mut parts, "-NS", &input.namespaces);
    if !object_and_resource.is_empty() {
        parts.push(format!("-O{object_and_resource}"));
        parts.push(format!("-R{object_and_resource}"));
    }
    if !include_and_unit.is_empty() {
        parts.push(format!("-U{include_and_unit}"));
    }
    if let Some(description) = input.description.as_ref().filter(|d| !d.trim().is_empty()) {
        parts.push(format!("--description:\"{description}\""));
    }
    if !input.required_packages.is_empty() {
        parts.push(format!("-LU{};", input.required_packages.join(";")));
    }

    parts.join(" ")
}

// ---------------------------------------------------------------------------
// The generated file
// ---------------------------------------------------------------------------

/// The `settings` object of a `.delphilsp.json`, field-for-field as the RAD
/// Studio IDE writes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DelphiLspSettings {
    project: String,
    dllname: String,
    #[serde(rename = "dccOptions")]
    dcc_options: String,
    /// Extra files to treat as part of the project. The IDE leaves this empty;
    /// the unit list is derived from the search paths instead.
    #[serde(rename = "projectFiles")]
    project_files: Vec<String>,
    #[serde(rename = "includeDCUsInUsesCompletion")]
    include_dcus_in_uses_completion: bool,
    #[serde(rename = "enableKeyWordCompletion")]
    enable_keyword_completion: bool,
    /// Directories DelphiLSP may navigate into (the IDE's Browsing Path).
    #[serde(rename = "browsingPaths")]
    browsing_paths: Vec<String>,
    /// `%APPDATA%\Embarcadero\BDS\<version>\`.
    #[serde(rename = "CommonAppData")]
    common_app_data: String,
    /// `<installation>\ObjRepos\`.
    #[serde(rename = "Templates")]
    templates: String,
}

/// A whole `.delphilsp.json`. The RAD Studio IDE writes only `settings`; DDK
/// adds the [`GENERATED_BY_MARKER`] alongside it so a file it owns can be told
/// apart from one the IDE produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DelphiLspFile {
    settings: DelphiLspSettings,
    /// Absent on IDE-generated files — see [`GENERATED_BY_MARKER`].
    #[serde(rename = "generatedBy", skip_serializing_if = "Option::is_none")]
    generated_by: Option<String>,
}

/// Outcome of generating a `.delphilsp.json` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelphiLspConfigResult {
    /// Absolute path of the file that was written.
    pub file_path: String,
    /// Project this configuration describes (`.dproj` when there is one).
    pub project_file: String,
    /// `file:///…` URI of the `.dpr`/`.dpk` main source.
    pub project_uri: String,
    /// Compiler DLL DelphiLSP should load, e.g. `dcc64290.dll`.
    pub dllname: String,
    pub configuration: String,
    pub platform: String,
    /// Compiler installation the settings were derived from.
    pub compiler: String,
    pub search_path_count: usize,
    pub browsing_path_count: usize,
    pub define_count: usize,
    /// Non-fatal problems (unresolved macros, missing registry data, …).
    pub warnings: Vec<String>,
}

impl fmt::Display for DelphiLspConfigResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Wrote {}", self.file_path)?;
        writeln!(f, "  Project:     {}", self.project_file)?;
        writeln!(f, "  Target:      {} / {}", self.configuration, self.platform)?;
        writeln!(f, "  Compiler:    {} ({})", self.compiler, self.dllname)?;
        write!(
            f,
            "  Search path: {} entries, {} browsing paths, {} defines",
            self.search_path_count, self.browsing_path_count, self.define_count
        )?;
        for warning in &self.warnings {
            write!(f, "\n  ! {warning}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Everything the generator needs about the target, resolved by the caller.
#[derive(Debug, Clone)]
pub struct GenerationRequest {
    /// The project's `.dproj`. `None` for a bare `.dpr`/`.dpk` without one.
    pub dproj_path: Option<PathBuf>,
    /// The `.dpr`/`.dpk` main source (used for the `project` URI).
    pub main_source: PathBuf,
    /// Build configuration override; `None` uses the `.dproj` default.
    pub configuration: Option<String>,
    /// Target platform override; `None` uses the `.dproj` default.
    pub platform: Option<String>,
    /// Root of the Delphi installation (the folder containing `bin`).
    pub installation_path: PathBuf,
    /// Human-readable compiler name, echoed back in the result.
    pub compiler_name: String,
    /// Where to write the file; `None` writes `<main source>.delphilsp.json`
    /// next to the project.
    pub out_path: Option<PathBuf>,
}

/// Generate (and write) the `.delphilsp.json` file for `request`.
pub fn generate(request: &GenerationRequest) -> Result<DelphiLspConfigResult> {
    let mut warnings: Vec<String> = Vec::new();

    let installation = &request.installation_path;
    let rsvars_path = installation.join("bin").join("rsvars.bat");
    if !rsvars_path.exists() {
        bail!("rsvars.bat not found in compiler installation: {}", rsvars_path.display());
    }
    let rsvars = dproj_rs::rsvars::parse_rsvars_file(&rsvars_path)
        .with_context(|| format!("Failed to parse {}", rsvars_path.display()))?;

    let bds_version = bds_version_from_installation(installation);

    // ── Build the macro map ────────────────────────────────────────────────
    let mut macros = MacroMap::new();
    macros.extend(rsvars);
    // `rsvars.bat` deliberately blanks PLATFORM; the IDE's library paths use it.
    macros.remove("PLATFORM");
    let bds = macros
        .get("BDS")
        .cloned()
        .unwrap_or_else(|| installation.to_string_lossy().to_string());
    macros.set_default("BDS", bds.clone());
    macros.set_default("BDSLIB", format!("{bds}\\lib"));
    macros.set_default("BDSINCLUDE", format!("{bds}\\include"));
    if let Some(user_dir) = bds_user_dir(&bds_version) {
        macros.set_default("BDSUSERDIR", user_dir);
    }
    // The IDE's own "Environment Variables" overrides win over rsvars/process env.
    macros.extend(registry::read_ide_environment_variables(&bds_version));

    // ── Resolve configuration / platform ───────────────────────────────────
    let dproj = match &request.dproj_path {
        Some(path) => Some(
            dproj_rs::DprojBuilder::new()
                .env(macros.as_env())
                .from_file(path)
                .map_err(|e| anyhow::anyhow!("Failed to parse {}: {e}", path.display()))?,
        ),
        _ => None,
    };
    let configuration = request
        .configuration
        .clone()
        .or_else(|| dproj.as_ref().and_then(|d| d.active_configuration().ok()))
        .unwrap_or_else(|| "Debug".to_string());
    let platform = request
        .platform
        .clone()
        .or_else(|| dproj.as_ref().and_then(|d| d.active_platform().ok()))
        .unwrap_or_else(|| "Win32".to_string());
    macros.set("Platform", platform.clone());
    macros.set("Config", configuration.clone());
    macros.set("Configuration", configuration.clone());

    // The IDE library settings are per-platform, so they can only be read once
    // the effective platform is known.
    let ide = registry::read_ide_library_settings(&bds_version, &platform);
    if ide.search_path.is_none() {
        warnings.push(format!(
            "No IDE Library Path found in HKCU\\SOFTWARE\\Embarcadero\\BDS\\{bds_version}\\Library\\{platform}; \
             only the project's own search path will be used."
        ));
    }

    let global_search_paths = ide
        .search_path
        .as_deref()
        .map(|raw| expand_path_list(raw, &macros, &mut warnings))
        .unwrap_or_default();

    // ── Evaluate the effective property group ──────────────────────────────
    // `DCC_*` names are deliberately left out of the seed environment: the
    // project's list properties end in an inheritance token (`;$(DCC_Define)`)
    // which must chain across property groups. `dproj-rs` re-asserts every
    // seeded variable after each group, so seeding those names would reset the
    // chain. Left unseeded they expand to nothing, yielding the project's own
    // values — the IDE's global counterparts are appended below.
    let property_group = match &request.dproj_path {
        Some(path) => Some(
            dproj_rs::DprojBuilder::new()
                .env(macros.as_env())
                .from_file(path)
                .and_then(|d| d.active_property_group_for(&configuration, &platform))
                .map_err(|e| anyhow::anyhow!("Failed to evaluate {}: {e}", path.display()))?,
        ),
        _ => None,
    };
    let dcc = property_group.as_ref().map(|pg| &pg.dcc_options);

    // ── Assemble the option payloads ───────────────────────────────────────
    let take = |value: Option<&String>| value.filter(|v| !v.trim().is_empty()).cloned();
    let flag_of = |value: Option<&String>| value.map(|v| v.eq_ignore_ascii_case("true"));

    // The IDE's global Library Path always follows the project's own entries —
    // exactly where the `;$(DCC_UnitSearchPath)` inheritance token sat.
    let mut search_paths = match dcc.and_then(|d| take(d.unit_search_path.as_ref())) {
        Some(raw) => expand_path_list(&raw, &macros, &mut warnings),
        _ => Vec::new(),
    };
    search_paths.extend(global_search_paths.iter().cloned());

    let default_output = format!(".\\{platform}\\{configuration}");
    let exe_output = dcc
        .and_then(|d| take(d.exe_output.as_ref()))
        .unwrap_or_else(|| default_output.clone());
    let dcu_output = dcc
        .and_then(|d| take(d.dcu_output.as_ref()))
        .unwrap_or_else(|| default_output.clone());
    let bpl_output = dcc
        .and_then(|d| take(d.bpl_output.as_ref()))
        .or_else(|| ide.package_dpl_output.as_deref().map(|p| macros.expand(p)))
        .unwrap_or_default();
    let dcp_output = dcc
        .and_then(|d| take(d.dcp_output.as_ref()))
        .or_else(|| ide.package_dcp_output.as_deref().map(|p| macros.expand(p)))
        .unwrap_or_default();

    let defines = strip_trailing_separators(
        &dcc.and_then(|d| take(d.define.as_ref())).unwrap_or_default(),
    );
    // Namespaces keep the IDE's trailing `;`.
    let namespaces = dcc.and_then(|d| take(d.namespace.as_ref())).unwrap_or_default();
    // Project-defined aliases sit in front of the IDE's built-in ones, again
    // where the `;$(DCC_UnitAlias)` inheritance token was.
    let unit_aliases = match dcc.and_then(|d| take(d.unit_alias.as_ref())) {
        Some(own) => format!("{};{DEFAULT_UNIT_ALIASES}", strip_trailing_separators(&own)),
        _ => DEFAULT_UNIT_ALIASES.to_string(),
    };

    let is_package = request
        .main_source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("dpk"))
        .unwrap_or(false);
    let is_library = !is_package
        && property_group
            .as_ref()
            .and_then(|pg| pg.project_properties.app_type.as_deref())
            .map(|t| t.eq_ignore_ascii_case("Library"))
            .unwrap_or(false);

    // Debug DCUs are only meaningful when the configuration asks for them.
    let wants_debug_dcus = dcc
        .and_then(|d| flag_of(d.debug_dcus.as_ref()))
        .unwrap_or_else(|| configuration.eq_ignore_ascii_case("Debug"));
    let debug_dcu_path = match (wants_debug_dcus, ide.debug_dcu_path.as_deref()) {
        (true, Some(raw)) => {
            let expanded = macros.expand(raw);
            if expanded.contains("$(") {
                warnings.push(format!("Unresolved macro in IDE Debug DCU path: {expanded}"));
                None
            } else {
                Some(trim_trailing_separator(&expanded).to_string())
            }
        }
        _ => None,
    };

    let required_packages = dproj
        .as_ref()
        .map(collect_required_packages)
        .unwrap_or_default();

    let options_input = DccOptionsInput {
        is_package,
        is_library,
        optimize: dcc.and_then(|d| flag_of(d.optimize.as_ref())).unwrap_or(false),
        stack_frames: dcc
            .and_then(|d| flag_of(d.generate_stack_frames.as_ref()))
            .unwrap_or(true),
        inlining_off: dcc
            .and_then(|d| d.inlining.as_deref())
            .map(|v| v.eq_ignore_ascii_case("off"))
            .unwrap_or(false),
        range_checking: dcc.and_then(|d| flag_of(d.range_checking.as_ref())),
        overflow_checking: dcc.and_then(|d| flag_of(d.integer_overflow_check.as_ref())),
        unit_aliases,
        defines: defines.clone(),
        namespaces,
        exe_output,
        dcu_output,
        bpl_output,
        dcp_output,
        debug_dcu_path,
        search_paths: search_paths.clone(),
        required_packages,
        description: dcc.and_then(|d| take(d.description.as_ref())),
    };
    let dcc_options = build_dcc_options(&options_input);

    // ── dllname ────────────────────────────────────────────────────────────
    let dllname = match find_compiler_dll(installation, &platform) {
        Some(name) => name,
        _ => {
            warnings.push(format!(
                "No compiler DLL found for platform {platform} in {}\\bin; DelphiLSP may refuse to start.",
                installation.display()
            ));
            String::new()
        }
    };

    // ── Browsing paths and IDE data directories ────────────────────────────
    let browsing_paths: Vec<String> = ide
        .browsing_path
        .as_deref()
        .map(|raw| expand_path_list(raw, &macros, &mut warnings))
        .unwrap_or_default()
        .iter()
        .map(|p| path_to_file_uri(Path::new(p)))
        .collect();
    let common_app_data = dirs::config_dir()
        .map(|dir| dir_to_file_uri(&dir.join("Embarcadero").join("BDS").join(&bds_version)))
        .unwrap_or_default();
    let templates = dir_to_file_uri(&installation.join("ObjRepos"));

    // ── Write ──────────────────────────────────────────────────────────────
    let project_uri = path_to_file_uri(&request.main_source);
    let file = DelphiLspFile {
        settings: DelphiLspSettings {
            project: project_uri.clone(),
            dllname: dllname.clone(),
            dcc_options: dcc_options.clone(),
            project_files: Vec::new(),
            include_dcus_in_uses_completion: dcc
                .and_then(|d| flag_of(d.include_dcus_in_uses_completion.as_ref()))
                .unwrap_or(true),
            enable_keyword_completion: true,
            browsing_paths: browsing_paths.clone(),
            common_app_data,
            templates,
        },
        generated_by: Some(GENERATED_BY_MARKER.to_string()),
    };
    let out_path = match &request.out_path {
        Some(path) => path.clone(),
        _ => default_out_path(&request.main_source),
    };
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&file)?;
    std::fs::write(&out_path, json)
        .with_context(|| format!("Failed to write {}", out_path.display()))?;

    let define_count = defines.split(';').filter(|d| !d.trim().is_empty()).count();
    Ok(DelphiLspConfigResult {
        file_path: out_path.to_string_lossy().to_string(),
        project_file: request
            .dproj_path
            .as_ref()
            .unwrap_or(&request.main_source)
            .to_string_lossy()
            .to_string(),
        project_uri,
        dllname,
        configuration,
        platform,
        compiler: request.compiler_name.clone(),
        search_path_count: search_paths.len(),
        browsing_path_count: browsing_paths.len(),
        define_count,
        warnings,
    })
}

/// `<dir>\<stem>.delphilsp.json` next to the main source — the location the
/// DelphiLSP extension looks in.
pub fn default_out_path(main_source: &Path) -> PathBuf {
    let stem = main_source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());
    let dir = main_source.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{stem}.delphilsp.json"))
}

/// Trim trailing `;` separators left over after an inheritance token was
/// replaced by nothing (`DEBUG;QBF_ODAC;` → `DEBUG;QBF_ODAC`).
fn strip_trailing_separators(value: &str) -> String {
    value.trim_end_matches(';').to_string()
}

/// `.dcp` names referenced by the project — emitted as `-LU`.
fn collect_required_packages(dproj: &dproj_rs::Dproj) -> Vec<String> {
    dproj
        .project
        .item_groups
        .iter()
        .flat_map(|ig| &ig.dcc_references)
        .filter_map(|r| {
            let path = Path::new(&r.include);
            let is_dcp = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("dcp"))
                .unwrap_or(false);
            is_dcp
                .then(|| path.file_stem().map(|s| s.to_string_lossy().to_string()))
                .flatten()
        })
        .collect()
}

/// `C:\…\Studio\23.0` → `23.0`. Falls back to the folder name as-is.
pub fn bds_version_from_installation(installation: &Path) -> String {
    installation
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `<Documents>\Embarcadero\Studio\<version>` — the IDE's `$(BDSUSERDIR)`.
fn bds_user_dir(bds_version: &str) -> Option<String> {
    let documents = dirs::document_dir()?;
    Some(
        documents
            .join("Embarcadero")
            .join("Studio")
            .join(bds_version)
            .to_string_lossy()
            .to_string(),
    )
}

/// dcc DLL file-name prefix (and optional suffix) for a target platform.
fn compiler_dll_prefix(platform: &str) -> (&'static str, &'static str) {
    match platform.to_ascii_lowercase().as_str() {
        "win32" => ("dcc32", ""),
        "win64" => ("dcc64", ""),
        "win64x" => ("dcc64", "N"),
        "android" => ("dccaarm", ""),
        "android64" => ("dccaarm64", ""),
        "iosdevice64" => ("dcciosarm64", ""),
        "iossimarm64" => ("dcciossimarm64", ""),
        "linux64" => ("dcclinux64", ""),
        "osx64" => ("dccosx64", ""),
        "osxarm64" => ("dccosxarm64", ""),
        _ => ("dcc32", ""),
    }
}

/// Locate the compiler DLL DelphiLSP must load, e.g. `dcc64290.dll`, by
/// scanning `<installation>\bin` for `<prefix><version><suffix>.dll`.
pub fn find_compiler_dll(installation: &Path, platform: &str) -> Option<String> {
    let (prefix, suffix) = compiler_dll_prefix(platform);
    let entries = std::fs::read_dir(installation.join("bin")).ok()?;
    let mut matches: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(|s| s.to_string()))
        .filter(|name| matches_compiler_dll(name, prefix, suffix))
        .collect();
    matches.sort();
    matches.pop()
}

/// `dcc64290.dll` matches (`dcc64`, ``) but `dcc64290N.dll` does not — the
/// middle section must be digits only.
fn matches_compiler_dll(name: &str, prefix: &str, suffix: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".dll") else {
        return false;
    };
    let Some(rest) = stem.strip_prefix(&prefix.to_ascii_lowercase()) else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(&suffix.to_ascii_lowercase()) else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_macros() -> MacroMap {
        let mut macros = MacroMap::new();
        macros.set("BDS", r"c:\delphi\23.0");
        macros.set("BDSLIB", r"c:\delphi\23.0\lib");
        macros.set("BDSCOMMONDIR", r"c:\public\23.0");
        macros.set("Platform", "Win64");
        macros.set("Config", "Debug");
        macros
    }

    // ── Macro expansion ───────────────────────────────────────────────────

    #[test]
    fn expands_case_insensitively() {
        let macros = sample_macros();
        assert_eq!(
            macros.expand(r"$(BDSLIB)\$(PLATFORM)\release"),
            r"c:\delphi\23.0\lib\Win64\release"
        );
    }

    #[test]
    fn expands_nested_values() {
        let mut macros = MacroMap::new();
        macros.set("ROOT", r"c:\root");
        macros.set("SUB", r"$(ROOT)\sub");
        assert_eq!(macros.expand(r"$(SUB)\leaf"), r"c:\root\sub\leaf");
    }

    #[test]
    fn leaves_unknown_macros_verbatim() {
        let macros = sample_macros();
        assert_eq!(macros.expand(r"$(NOPE)\x"), r"$(NOPE)\x");
    }

    #[test]
    fn self_referential_macro_terminates() {
        let mut macros = MacroMap::new();
        macros.set("LOOP", "$(LOOP)");
        assert_eq!(macros.expand("$(LOOP)"), "$(LOOP)");
    }

    // ── Path list expansion ───────────────────────────────────────────────

    #[test]
    fn drops_entries_with_unresolved_macros_and_warns() {
        let macros = sample_macros();
        let mut warnings = Vec::new();
        let paths = expand_path_list(
            r"$(BDS)\include\;$(VEGADIR)\src; ;c:\lib",
            &macros,
            &mut warnings,
        );
        assert_eq!(paths, vec![r"c:\delphi\23.0\include", r"c:\lib"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("$(VEGADIR)"), "{}", warnings[0]);
    }

    #[test]
    fn keeps_drive_root_intact() {
        assert_eq!(trim_trailing_separator(r"c:\"), r"c:\");
        assert_eq!(trim_trailing_separator(r"c:\foo\"), r"c:\foo");
    }

    // ── File URI ──────────────────────────────────────────────────────────

    #[test]
    fn encodes_windows_path_as_file_uri() {
        assert_eq!(
            path_to_file_uri(Path::new(r"C:\Athens\hydra_2\About\libAboutD29.dpk")),
            "file:///C%3A/Athens/hydra_2/About/libAboutD29.dpk"
        );
    }

    #[test]
    fn encodes_spaces_parens_and_plus_in_file_uri() {
        assert_eq!(
            path_to_file_uri(Path::new(r"C:\Program Files (x86)\GDI+\a.dpr")),
            "file:///C%3A/Program%20Files%20%28x86%29/GDI%2B/a.dpr"
        );
    }

    #[test]
    fn directory_uris_end_with_a_slash() {
        assert_eq!(dir_to_file_uri(Path::new(r"C:\a\ObjRepos")), "file:///C%3A/a/ObjRepos/");
        assert_eq!(dir_to_file_uri(Path::new(r"C:\a\ObjRepos\")), "file:///C%3A/a/ObjRepos/");
    }

    // ── dccOptions assembly ───────────────────────────────────────────────

    fn package_input() -> DccOptionsInput {
        DccOptionsInput {
            is_package: true,
            optimize: false,
            stack_frames: true,
            inlining_off: true,
            range_checking: Some(true),
            unit_aliases: DEFAULT_UNIT_ALIASES.to_string(),
            defines: "DEBUG;QBF_ODAC".to_string(),
            namespaces: "System;Vcl;".to_string(),
            exe_output: r".\Win64\Debug".to_string(),
            dcu_output: r".\Win64\Debug".to_string(),
            bpl_output: r"c:\public\23.0\Bpl\Win64".to_string(),
            dcp_output: r"c:\public\23.0\Dcp\Win64".to_string(),
            debug_dcu_path: Some(r"c:\delphi\23.0\lib\Win64\debug".to_string()),
            search_paths: vec![
                r".\Win64\Debug".to_string(),
                r"c:\delphi\23.0\lib\Win64\release".to_string(),
                r"c:\libs\Common Library\Sources".to_string(),
            ],
            required_packages: vec!["libStdFormsD29".to_string()],
            description: Some("Hydra About Menu".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn assembles_switches_in_ide_order() {
        let options = build_dcc_options(&package_input());
        assert!(
            options.starts_with("-$O- -$W+ --inline:off -$R+ --no-config -Q -Z -TX.bpl -A"),
            "{options}"
        );
    }

    #[test]
    fn emits_defines_namespaces_and_outputs() {
        let options = build_dcc_options(&package_input());
        assert!(options.contains(" -DDEBUG;QBF_ODAC "), "{options}");
        assert!(options.contains(" -NSSystem;Vcl; "), "{options}");
        assert!(options.contains(r" -E.\Win64\Debug "), "{options}");
        assert!(options.contains(r" -NU.\Win64\Debug "), "{options}");
        assert!(options.contains(r" -LEc:\public\23.0\Bpl\Win64 "), "{options}");
        assert!(options.contains(r" -LNc:\public\23.0\Dcp\Win64 "), "{options}");
    }

    #[test]
    fn include_and_unit_paths_lead_with_debug_dcus() {
        let options = build_dcc_options(&package_input());
        let unit = options
            .split(" -U")
            .nth(1)
            .expect("missing -U")
            .to_string();
        assert!(unit.starts_with(r"c:\delphi\23.0\lib\Win64\debug;.\Win64\Debug;"), "{unit}");
        let object = options.split(" -O").nth(1).expect("missing -O");
        assert!(object.starts_with(r".\Win64\Debug;"), "{object}");
    }

    #[test]
    fn quotes_only_entries_containing_spaces() {
        let options = build_dcc_options(&package_input());
        assert!(options.contains(r#";"c:\libs\Common Library\Sources""#), "{options}");
        assert!(!options.contains(r#""c:\libs\Common""#), "{options}");
    }

    #[test]
    fn emits_description_and_required_packages_last() {
        let options = build_dcc_options(&package_input());
        assert!(options.ends_with(r#"--description:"Hydra About Menu" -LUlibStdFormsD29;"#), "{options}");
    }

    #[test]
    fn program_target_uses_exe_extension() {
        let input = DccOptionsInput { is_package: false, ..package_input() };
        assert!(build_dcc_options(&input).contains(" -TX.exe "));
        let library = DccOptionsInput { is_package: false, is_library: true, ..package_input() };
        assert!(build_dcc_options(&library).contains(" -TX.dll "));
    }

    #[test]
    fn omits_optional_switches_when_unknown() {
        let input = DccOptionsInput {
            range_checking: None,
            overflow_checking: None,
            inlining_off: false,
            ..package_input()
        };
        let options = build_dcc_options(&input);
        assert!(!options.contains("-$R"), "{options}");
        assert!(!options.contains("-$Q"), "{options}");
        assert!(!options.contains("--inline:off"), "{options}");
    }

    // ── Misc helpers ──────────────────────────────────────────────────────

    #[test]
    fn compiler_dll_name_must_end_in_digits() {
        assert!(matches_compiler_dll("dcc64290.dll", "dcc64", ""));
        assert!(!matches_compiler_dll("dcc64290N.dll", "dcc64", ""));
        assert!(matches_compiler_dll("dcc64290N.dll", "dcc64", "N"));
        assert!(!matches_compiler_dll("dcc64.dll", "dcc64", ""));
        assert!(!matches_compiler_dll("dcc32290.dll", "dcc64", ""));
    }

    #[test]
    fn default_out_path_sits_next_to_the_main_source() {
        assert_eq!(
            default_out_path(Path::new(r"C:\a\b\libAboutD29.dpk")),
            PathBuf::from(r"C:\a\b\libAboutD29.delphilsp.json")
        );
    }

    #[test]
    fn generated_by_marker_sits_beside_settings_not_inside_it() {
        let file = DelphiLspFile {
            settings: DelphiLspSettings {
                project: "file:///C%3A/a/App.dpr".into(),
                dllname: "dcc32290.dll".into(),
                dcc_options: "--no-config".into(),
                project_files: Vec::new(),
                include_dcus_in_uses_completion: true,
                enable_keyword_completion: true,
                browsing_paths: Vec::new(),
                common_app_data: String::new(),
                templates: String::new(),
            },
            generated_by: Some(GENERATED_BY_MARKER.to_string()),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&file).unwrap()).unwrap();
        assert_eq!(json["generatedBy"], "delphi-devkit");
        assert!(json["settings"].get("generatedBy").is_none(), "marker must not be nested");
    }

    #[test]
    fn ide_files_without_the_marker_still_parse() {
        // The IDE writes `settings` only; reading such a file must not fail and
        // must leave the ownership marker unset.
        let raw = r#"{"settings":{"project":"file:///C%3A/a/App.dpr","dllname":"dcc32290.dll",
            "dccOptions":"--no-config","projectFiles":[],"includeDCUsInUsesCompletion":true,
            "enableKeyWordCompletion":true,"browsingPaths":[],"CommonAppData":"","Templates":""}}"#;
        let file: DelphiLspFile = serde_json::from_str(raw).unwrap();
        assert_eq!(file.generated_by, None);
    }

    #[test]
    fn strips_trailing_define_separators() {
        assert_eq!(strip_trailing_separators("DEBUG;QBF_ODAC;"), "DEBUG;QBF_ODAC");
        assert_eq!(strip_trailing_separators("DEBUG"), "DEBUG");
    }
}
