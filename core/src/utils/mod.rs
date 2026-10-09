use serde::{Serialize, Deserialize};
use std::path::{Path, PathBuf, Component};

mod document;
pub use document::*;

/// The custom environment-variable overrides configured in the Delphi IDE
/// (Tools > Options > IDE > Environment Variables), stored per BDS version at
/// `HKCU\SOFTWARE\<vendor>\BDS\<ver>\Environment Variables`. The IDE injects
/// these into its own process and thus into IDE-run MSBuild, so dproj values
/// reference them (a site-specific `$(VEGADIR)`) though they exist in no real
/// environment. `bds_major_version` is `23` for Delphi 12 Athens — the same
/// number as `CompilerConfiguration::product_version`. Empty when the key does
/// not exist, or off Windows.
#[cfg(windows)]
pub fn bds_environment_overrides(bds_major_version: usize) -> Vec<(String, String)> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let root = crate::delphilsp::IdeRegistryRoot::for_bds_version(bds_major_version);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key_path = format!(r"{}\Environment Variables", root.key_path());
    let Ok(env_key) = hkcu.open_subkey(key_path) else {
        return Vec::new();
    };
    read_environment_values(&env_key)
}

/// The IDE's environment-variable overrides under `env_key`. Only string
/// values (`REG_SZ`/`REG_EXPAND_SZ`) define a usable `$(NAME)`; names and
/// values are trimmed and empty ones dropped.
#[cfg(windows)]
fn read_environment_values(env_key: &winreg::RegKey) -> Vec<(String, String)> {
    use winreg::enums::{REG_EXPAND_SZ, REG_SZ};
    use winreg::types::FromRegValue;

    env_key
        .enum_values()
        .flatten()
        .filter(|(_, value)| matches!(value.vtype, REG_SZ | REG_EXPAND_SZ))
        .filter_map(|(name, value)| {
            // Proper registry decoding — `RegValue`'s Display is debug formatting.
            let text = String::from_reg_value(&value).ok()?.trim().to_string();
            (!name.trim().is_empty() && !text.is_empty()).then_some((name, text))
        })
        .collect()
}

#[cfg(not(windows))]
pub fn bds_environment_overrides(_bds_major_version: usize) -> Vec<(String, String)> {
    Vec::new()
}

/// Fallback for projects with no owning workspace or group project to pick a
/// compiler configuration from: the highest installed BDS version's non-empty
/// set.
#[cfg(windows)]
pub fn ide_environment_overrides() -> Vec<(String, String)> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(bds) = hkcu.open_subkey(r"SOFTWARE\Embarcadero\BDS") else {
        return Vec::new();
    };
    let mut versions: Vec<String> = bds.enum_keys().flatten().collect();
    // Version keys are "20.0", "22.0", "23.0", ... — lexicographic order is
    // wrong across the 9.x/10.x boundary, so compare numerically.
    versions.sort_by(|a, b| {
        let parse = |v: &str| v.split('.').next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
        parse(a).cmp(&parse(b))
    });
    for version in versions.iter().rev() {
        let Ok(env_key) = bds.open_subkey(format!(r"{version}\Environment Variables")) else {
            continue;
        };
        let overrides = read_environment_values(&env_key);
        if !overrides.is_empty() {
            return overrides;
        }
    }
    Vec::new()
}

#[cfg(not(windows))]
pub fn ide_environment_overrides() -> Vec<(String, String)> {
    Vec::new()
}

/// Strip trailing path separators (`c:\foo\` → `c:\foo`) without eating a
/// drive root (`c:\` stays `c:\`).
pub fn trim_trailing_separator(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        path
    } else {
        trimmed
    }
}

/// Percent-encode a Windows path into the `file:///C%3A/dir/file.dpk` form the
/// RAD Studio IDE writes: everything outside the unreserved URI set is encoded,
/// `:`, spaces, `(`, `)` and `+` included — all observed encoded in
/// IDE-generated files.
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
    if normalized.starts_with("//") {
        // UNC path: \\server\share\file → file://server/share/file (the
        // server is the URI authority, so exactly two slashes after `file:`).
        return format!("file:{encoded}");
    }
    format!("file:///{}", encoded.trim_start_matches('/'))
}

/// Like [`path_to_file_uri`] but for a directory: the IDE always terminates
/// those URIs with a slash.
pub fn dir_to_file_uri(path: &Path) -> String {
    let uri = path_to_file_uri(path);
    if uri.ends_with('/') { uri } else { format!("{uri}/") }
}

/// Normalise a path by:
///   1. Resolving `.` and `..` segments purely, without touching the filesystem.
///   2. Stripping the Windows extended-length prefix (`\\?\`).
///   3. Converting Delphi-style bare UNC paths (`UNC\server\share\...`, as
///      Delphi project files sometimes write them) to `\\server\share\...`;
///      the standard library would otherwise read them as relative.
///   4. On Windows, remapping `\\server\share\...` to the local drive letter
///      mapped to that share.
///
/// Unlike `std::fs::canonicalize()`, works on any path string whether or not
/// the target exists, and returns no `\\?\` prefix.
pub fn normalize_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();

    let path = {
        let s = path.to_string_lossy();
        if s.starts_with(r"\\?\") {
            let stripped = &s[4..];
            if stripped.len() >= 4 && stripped[..4].eq_ignore_ascii_case("UNC\\") {
                PathBuf::from(format!(r"\\{}", &stripped[4..]))
            } else {
                PathBuf::from(stripped)
            }
        } else if s.len() >= 4 && s[..4].eq_ignore_ascii_case("UNC\\") {
            PathBuf::from(format!(r"\\{}", &s[4..]))
        } else {
            path.to_path_buf()
        }
    };

    let mut components: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => { /* skip `.` */ }
            Component::ParentDir => {
                match components.last() {
                    Some(Component::Normal(_)) => { components.pop(); }
                    Some(Component::RootDir) | Some(Component::Prefix(_)) => { /* can't go above root */ }
                    _ => { components.push(component); }
                }
            }
            _ => { components.push(component); }
        }
    }

    let result: PathBuf = if components.is_empty() {
        PathBuf::from(".")
    } else {
        components.iter().collect()
    };

    #[cfg(windows)]
    {
        use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;

        let s = result.to_string_lossy();
        if s.starts_with(r"\\") && !s.starts_with(r"\\?\") {
            let mut comps = result.components();
            if let Some(Component::Prefix(p)) = comps.next() {
                let unc_prefix = p.as_os_str().to_string_lossy().into_owned();

                for drive in b'A'..=b'Z' {
                    let local_name: Vec<u16> = format!("{}:", drive as char)
                        .encode_utf16()
                        .chain(std::iter::once(0u16))
                        .collect();
                    let mut buf_len = 512u32;
                    let mut buf = vec![0u16; buf_len as usize];
                    let ret = unsafe {
                        WNetGetConnectionW(local_name.as_ptr(), buf.as_mut_ptr(), &mut buf_len)
                    };
                    if ret == 0 {
                        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                        let mapped_unc = String::from_utf16_lossy(&buf[..end]);
                        if mapped_unc.eq_ignore_ascii_case(&unc_prefix) {
                            let rest: PathBuf = comps
                                .filter(|c| !matches!(c, Component::RootDir))
                                .collect();
                            return PathBuf::from(format!("{}:\\", drive as char)).join(rest);
                        }
                    }
                }
            }
        }
    }

    result
}

pub trait FilePath {
    fn get_file_path() -> &'static PathBuf;
}

pub trait Load {
    fn load_from_file(path: &PathBuf) -> Self
    where
        Self: Serialize + Default + for<'de> Deserialize<'de>,
    {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(obj) = ron::from_str(&data) {
                return obj;
            }
        }
        return Self::default();
    }
}
