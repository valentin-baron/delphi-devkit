use anyhow::Result;
use dproj_rs::Dproj;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::utils::normalize_path;

lazy_static::lazy_static! {
    /// Matches the plain dproj option-group condition form `'$(Ident)'!=''`
    /// (the groups that carry DCC_/Debugger_ option values). The compound
    /// flag-defining conditions (`'$(Config)'=='Debug' or '$(Cfg_1)'!=''`)
    /// deliberately do not match: they only set the Base/Cfg_N flags.
    static ref CONDITION_IDENT_REGEX: regex::Regex =
        regex::Regex::new(r"^\s*'\$\((?P<ident>[A-Za-z0-9_]+)\)'\s*!=\s*''\s*$").unwrap();
}

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

// ═══════════════════════════════════════════════════════════════════════════════
//  Raw property access
// ═══════════════════════════════════════════════════════════════════════════════

/// Reads the raw text of a single property (e.g. `Debugger_HostApplication`)
/// from a `.dproj`, resolved for an effective (configuration, platform) pair.
///
/// This complements `dproj-rs`, which only surfaces a fixed set of known
/// properties: the dproj's `PropertyGroup` convention is re-applied here for
/// one arbitrary tag. A group applies when it has no `Condition` (project
/// defaults) or when its condition is of the plain option-group form
/// `'$(Ident)'!=''` with `Ident` one of `Base`, `Base_<platform>`,
/// `<CfgKey>`, `<CfgKey>_<platform>` — where `<CfgKey>` is the
/// `<BuildConfiguration Include="<config>"><Key>Cfg_N</Key>` mapping declared
/// in the dproj itself. Applying groups are evaluated in document order
/// (later wins), mirroring MSBuild. Returns the raw, unexpanded value; blank
/// values count as absent.
pub fn read_raw_property_for(dproj_path: &PathBuf, config: &str, platform: &str, tag: &str) -> Option<String> {
    let xml = std::fs::read_to_string(dproj_path).ok()?;
    let doc = roxmltree::Document::parse(&xml).ok()?;

    let config_key = find_build_configuration_key(&doc, config);
    let accepted = accepted_condition_idents(&config_key, platform);

    let mut value: Option<String> = None;
    for group in doc.descendants().filter(|n| n.has_tag_name("PropertyGroup")) {
        if !property_group_applies(&group, &accepted) {
            continue;
        }
        for child in group.children().filter(|n| n.is_element()) {
            if child.tag_name().name() == tag {
                // Trimmed on capture: surrounding whitespace would otherwise
                // flow into macro expansion and path resolution.
                value = Some(child.text().unwrap_or_default().trim().to_string());
            }
        }
    }
    value.filter(|v| !v.is_empty())
}

/// The `Cfg_N` key the dproj assigns to a configuration name, from its
/// `<BuildConfiguration Include="..."><Key>...</Key>` item group.
fn find_build_configuration_key(doc: &roxmltree::Document, config: &str) -> Option<String> {
    for build_configuration in doc.descendants().filter(|n| n.has_tag_name("BuildConfiguration")) {
        let include = build_configuration.attribute("Include").unwrap_or_default();
        if !include.eq_ignore_ascii_case(config) {
            continue;
        }
        for key in build_configuration.children().filter(|n| n.has_tag_name("Key")) {
            return key.text().map(|s| s.trim().to_string());
        }
    }
    None
}

/// The condition identifiers that select property groups applying to the
/// given (configuration key, platform) pair, from least to most specific.
fn accepted_condition_idents(config_key: &Option<String>, platform: &str) -> Vec<String> {
    let mut idents = vec!["Base".to_string(), format!("Base_{platform}")];
    if let Some(key) = config_key {
        idents.push(key.clone());
        idents.push(format!("{key}_{platform}"));
    }
    idents
}

fn property_group_applies(group: &roxmltree::Node, accepted: &[String]) -> bool {
    let Some(condition) = group.attribute("Condition") else {
        // Unconditional group: project-level defaults.
        return true;
    };
    let Some(caps) = CONDITION_IDENT_REGEX.captures(condition) else {
        // A condition form this reader does not understand (e.g. the
        // compound flag-defining conditions): not an option group.
        return false;
    };
    let ident = &caps["ident"];
    accepted.iter().any(|accepted_ident| accepted_ident.eq_ignore_ascii_case(ident))
}