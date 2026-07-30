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

