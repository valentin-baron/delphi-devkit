use anyhow::Result;
use dproj_rs::Dproj;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::utils::normalize_path;

// ═══════════════════════════════════════════════════════════════════════════════
//  Dproj Cache
// ═══════════════════════════════════════════════════════════════════════════════

/// Cached entry holding the parsed [`Dproj`] and the path it was loaded from.
struct CacheEntry {
    dproj: Dproj,
    path: PathBuf,
}

lazy_static::lazy_static! {
    /// Global runtime-only cache of parsed `.dproj` files, keyed by project id.
    static ref DPROJ_CACHE: Mutex<HashMap<usize, CacheEntry>> = Mutex::new(HashMap::new());
}

/// Return a clone of the cached [`Dproj`] for `project_id`, parsing from
/// `dproj_path` on a cache miss.  The cache is invalidated automatically
/// when the path changes between calls.
pub fn get_or_load(project_id: usize, dproj_path: &PathBuf) -> Result<Dproj> {
    let mut cache = DPROJ_CACHE.lock().unwrap();
    if let Some(entry) = cache.get(&project_id) {
        if entry.path == *dproj_path {
            return Ok(entry.dproj.clone());
        }
    }
    let dproj = Dproj::from_file(dproj_path)
        .map_err(|e| anyhow::anyhow!("Failed to parse dproj: {}", e))?;
    cache.insert(project_id, CacheEntry {
        dproj: dproj.clone(),
        path: dproj_path.clone(),
    });
    Ok(dproj)
}

/// Remove the cached entry for a single project.
pub fn invalidate(project_id: usize) {
    let mut cache = DPROJ_CACHE.lock().unwrap();
    cache.remove(&project_id);
}

/// Clear the entire cache (e.g. on bulk reload).
pub fn invalidate_all() {
    let mut cache = DPROJ_CACHE.lock().unwrap();
    cache.clear();
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Public helpers
// ═══════════════════════════════════════════════════════════════════════════════

pub fn get_main_source(dproj_path: &PathBuf) -> Result<PathBuf> {
    let dproj = Dproj::from_file(dproj_path)
        .map_err(|e| anyhow::anyhow!("Failed to parse dproj: {}", e))?;
    dproj.get_main_source()
        .map(normalize_path)
        .map_err(|e| anyhow::anyhow!("Main source not found in dproj: {}", e))
}

pub fn get_exe_path(dproj_path: &PathBuf) -> Result<PathBuf> {
    let dproj = Dproj::from_file(dproj_path)
        .map_err(|e| anyhow::anyhow!("Failed to parse dproj: {}", e))?;
    dproj.get_exe_path()
        .map(normalize_path)
        .map_err(|e| anyhow::anyhow!("Exe path not found in dproj: {}", e))
}

pub fn get_exe_path_for(dproj_path: &PathBuf, config: &str, platform: &str) -> Result<PathBuf> {
    let dproj = Dproj::from_file(dproj_path)
        .map_err(|e| anyhow::anyhow!("Failed to parse dproj: {}", e))?;
    dproj.get_exe_path_for(config, platform)
        .map(normalize_path)
        .map_err(|e| anyhow::anyhow!("Exe path not found in dproj for {}/{}: {}", config, platform, e))
}

lazy_static::lazy_static! {
    /// A `$(Name)` reference.
    static ref MACRO_REFERENCE: regex::Regex = regex::Regex::new(r"\$\(([^()\s]+)\)").unwrap();
    /// The expression of a `Condition="…"` attribute.
    static ref CONDITION: regex::Regex = regex::Regex::new(r#"Condition\s*=\s*"([^"]*)""#).unwrap();
    /// The name of an element the dproj opens, i.e. a property it declares.
    static ref PROPERTY_ELEMENT: regex::Regex = regex::Regex::new(r"<([A-Za-z_][\w.-]*)[\s>]").unwrap();
}

/// Whether `value` still holds a `$(Name)` that nothing resolved — see
/// [`seed_environment`]. Such a value is not a usable path.
pub fn has_unresolved_macro(value: &str) -> bool {
    value.contains("$(")
}

/// Whether `value` is a path a variable collapsed out of. The names a
/// `Condition` tests cannot be seeded with their own reference — see
/// [`seed_environment`] — so for those, dproj-rs still expands `$(VEGADIR)`
/// to nothing and `$(VEGADIR)\bpl` arrives as `\bpl`: rooted, with no drive
/// and no host, which is a directory that exists on whatever drive the
/// server happens to run from. The collapse is visible in the shape of the
/// result whichever variable caused it, and a dproj has no reason to name a
/// driveless rooted path otherwise. A UNC path (`\\host\share`) is not one.
pub fn looks_collapsed(value: &str) -> bool {
    let path = value.trim();
    let mut characters = path.chars();
    match (characters.next(), characters.next()) {
        (Some('\\'), Some('\\')) | (Some('/'), Some('/')) => false,
        (Some('\\'), _) | (Some('/'), _) => true,
        _ => false,
    }
}

/// The names of the `$(Name)` references left in `value`.
pub fn unresolved_macros(value: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for reference in MACRO_REFERENCE.captures_iter(value) {
        let name = reference[1].to_string();
        if !names.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
            names.push(name);
        }
    }
    names
}

/// Completes `environment`, the variables a dproj is evaluated with, for
/// the dproj whose text is `dproj_source`, in the two ways dproj-rs cannot:
///
/// * **Casing.** dproj-rs looks names up case-sensitively, MSBuild does not:
///   a dproj writing `$(VegaDir)` for a variable defined as `VEGADIR` gets
///   the value under the spelling it uses.
/// * **Undefined names.** dproj-rs expands an unknown `$(Name)` to nothing,
///   so `$(VEGADIR)\bpl` silently becomes `\bpl` — a different, existing
///   directory at worst. A name that neither the environment nor the dproj
///   itself defines is therefore seeded with its own reference, which
///   leaves `$(Name)` in the evaluated value for the reader to recognise
///   ([`has_unresolved_macro`]) instead of a path that merely looks valid.
///
/// Names the dproj defines as properties (`$(DCC_UnitSearchPath)` inside its
/// own definition, `$(Base)`, `$(Cfg_1)`) keep MSBuild's semantics — empty
/// until defined — since the dproj fills them itself as it is evaluated.
/// That covers the machinery the build configurations are selected with,
/// whose truth must not change.
///
/// So do the names a `Condition` tests, and that carve-out is not optional:
/// the IDE guards its own property groups with flags it tests before it
/// declares them (`'$(Cfg_2_Win64)'!=''`), and a seeded `$(Cfg_2_Win64)` is
/// a non-empty string, which makes every such group merge and the wrong
/// build configuration win. The cost is that a value under a variable that
/// a condition also tests still collapses — `$(VEGADIR)\bpl` to `\bpl` —
/// where dproj-rs offers one environment for conditions and values alike.
pub fn seed_environment(mut environment: HashMap<String, String>, dproj_source: &str) -> HashMap<String, String> {
    let declared = declared_properties(dproj_source);
    let tested_by_a_condition: Vec<String> = CONDITION
        .captures_iter(dproj_source)
        .flat_map(|condition| unresolved_macros(&condition[1]))
        .collect();
    for name in unresolved_macros(dproj_source) {
        if environment.contains_key(&name) {
            continue;
        }
        let defined_elsewhere = environment
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(&name))
            .map(|(_, value)| value.clone());
        if let Some(value) = defined_elsewhere {
            environment.insert(name, value);
            continue;
        }
        let is_declared = declared.iter().any(|property| property.eq_ignore_ascii_case(&name));
        let is_tested = tested_by_a_condition.iter().any(|tested| tested.eq_ignore_ascii_case(&name));
        if is_declared || is_tested || name.starts_with("MSBuild") {
            continue;
        }
        environment.insert(name.clone(), format!("$({name})"));
    }
    environment
}

/// The property names the dproj declares as elements of its own. Matched
/// whatever their casing, as MSBuild resolves them: a dproj that writes
/// `$(DCC_UNITSEARCHPATH)` for the `<DCC_UnitSearchPath>` it declares two
/// lines above means the same property.
fn declared_properties(dproj_source: &str) -> Vec<String> {
    PROPERTY_ELEMENT.captures_iter(dproj_source).map(|element| element[1].to_string()).collect()
}

/// Parses the dproj at `dproj_path`, evaluating it with `environment`
/// completed by [`seed_environment`].
pub fn load_with_environment(dproj_path: &PathBuf, environment: HashMap<String, String>) -> Result<Dproj> {
    let source = std::fs::read(dproj_path)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", dproj_path.display(), e))?;
    dproj_rs::DprojBuilder::new()
        .env(seed_environment(environment, &source))
        .from_file(dproj_path)
        .map_err(|e| anyhow::anyhow!("Failed to parse dproj: {}", e))
}

#[cfg(test)]
mod environment_tests {
    use super::*;

    fn environment(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const DPROJ: &str = r#"<Project>
        <PropertyGroup Condition="'$(Base)'!='' or '$(OnlyTested)'!=''">
            <Base>true</Base>
            <DCC_UnitSearchPath>$(VegaDir)\src;$(NOWHERE)\lib;$(DCC_UnitSearchPath)</DCC_UnitSearchPath>
            <DCC_BplOutput>$(BDSCOMMONDIR)\Bpl</DCC_BplOutput>
        </PropertyGroup>
        <Import Project="$(MSBuildProjectName).deployproj"/>
    </Project>"#;

    #[test]
    fn a_defined_variable_is_offered_under_the_spelling_the_dproj_uses() {
        let seeded = seed_environment(environment(&[("VEGADIR", r"C:\vega")]), DPROJ);
        assert_eq!(seeded.get("VegaDir").map(String::as_str), Some(r"C:\vega"));
    }

    #[test]
    fn an_undefined_variable_survives_as_its_own_reference() {
        let seeded = seed_environment(environment(&[]), DPROJ);
        assert_eq!(seeded.get("NOWHERE").map(String::as_str), Some("$(NOWHERE)"));
        assert_eq!(seeded.get("BDSCOMMONDIR").map(String::as_str), Some("$(BDSCOMMONDIR)"));
    }

    #[test]
    fn properties_the_dproj_declares_keep_msbuild_semantics() {
        let seeded = seed_environment(environment(&[]), DPROJ);
        for untouched in ["DCC_UnitSearchPath", "Base", "MSBuildProjectName"] {
            assert!(!seeded.contains_key(untouched), "{untouched} must stay undefined");
        }
    }

    /// A property is the dproj's own whatever casing the reference uses;
    /// otherwise the dproj's own value for it is dropped as unresolved.
    #[test]
    fn a_property_is_recognised_whatever_casing_the_reference_uses() {
        const SHOUTED: &str = r#"<Project>
            <PropertyGroup>
                <DCC_UnitSearchPath>$(DCC_UNITSEARCHPATH);src</DCC_UnitSearchPath>
            </PropertyGroup>
        </Project>"#;
        let seeded = seed_environment(environment(&[]), SHOUTED);

        assert!(!seeded.contains_key("DCC_UNITSEARCHPATH"), "the dproj declares it, whatever the casing");
    }

    /// The IDE guards its property groups with flags it tests before it
    /// declares them; a seeded one is a non-empty string and would make
    /// every such group merge.
    #[test]
    fn a_name_a_condition_tests_keeps_msbuild_semantics() {
        let seeded = seed_environment(environment(&[]), DPROJ);

        assert!(!seeded.contains_key("OnlyTested"), "a tested name must stay undefined");
    }

    #[test]
    fn a_path_a_variable_collapsed_out_of_is_recognised() {
        assert!(looks_collapsed(r"\bpl"));
        assert!(looks_collapsed(r"\Host.exe"));
        assert!(looks_collapsed("/usr/lib"), "a forward-slash root collapses the same way");
        assert!(!looks_collapsed(r"\\server\share\bpl"), "a UNC path is a real one");
        assert!(!looks_collapsed(r"C:\bpl"));
        assert!(!looks_collapsed(r".\Win64\Debug"));
        assert!(!looks_collapsed(""));
    }

    #[test]
    fn unresolved_references_are_found_by_name() {
        assert!(has_unresolved_macro(r"$(NOWHERE)\lib"));
        assert!(!has_unresolved_macro(r"C:\lib"));
        assert_eq!(unresolved_macros(r"$(A)\x;$(b)\y;$(a)\z"), vec!["A", "b"]);
    }
}

pub fn find_dproj_file(main_file_path: &PathBuf) -> Result<PathBuf> {
    let dproj_path = main_file_path.with_extension("dproj");
    if dproj_path.exists() {
        return Ok(dproj_path);
    } else {
        anyhow::bail!("DPROJ file not found for main file: {}", main_file_path.display());
    }
}

/// Return the available configurations from a `.dproj` file.
pub fn get_configurations(dproj: &Dproj) -> Vec<String> {
    dproj.configurations().iter().map(|s| s.to_string()).collect()
}

/// Return the available platforms from a `.dproj` file (name + active flag).
pub fn get_platforms(dproj: &Dproj) -> Vec<(String, bool)> {
    dproj.platforms().iter().map(|(s, active)| (s.to_string(), *active)).collect()
}

/// Return the dproj's default active configuration.
pub fn get_active_configuration(dproj: &Dproj) -> Option<String> {
    dproj.active_configuration().ok()
}

/// Return the dproj's default active platform.
pub fn get_active_platform(dproj: &Dproj) -> Option<String> {
    dproj.active_platform().ok()
}

