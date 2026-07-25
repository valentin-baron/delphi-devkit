//! Read the RAD Studio IDE's per-user settings that a `.dproj` alone cannot
//! provide: the **global Library Path** and the user-defined **environment
//! variable** overrides (`$(VEGADIR)`, `$(DXVCL)`, …).
//!
//! Both live under `HKCU\SOFTWARE\Embarcadero\BDS\<version>`. Everything here
//! degrades gracefully: a missing key yields empty data plus a warning from
//! the caller rather than an error, and non-Windows builds compile to stubs.

/// The IDE's library settings for one target platform.
#[derive(Debug, Clone, Default)]
pub struct IdeLibrarySettings {
    /// `Search Path` — the global Library Path, `;`-separated and still
    /// containing `$(NAME)` macros.
    pub search_path: Option<String>,
    /// `Browsing Path` — the directories DelphiLSP may navigate into
    /// (RTL/VCL sources), `;`-separated and still containing `$(NAME)` macros.
    pub browsing_path: Option<String>,
    /// `Debug DCU Path` — prepended to `-I`/`-U` for debug configurations.
    pub debug_dcu_path: Option<String>,
    /// `Package DPL Output` — the default `-LE` target.
    pub package_dpl_output: Option<String>,
    /// `Package DCP Output` — the default `-LN` target.
    pub package_dcp_output: Option<String>,
}

#[cfg(windows)]
mod imp {
    use super::IdeLibrarySettings;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, REG_EXPAND_SZ, REG_SZ};

    fn bds_key(bds_version: &str, sub_key: &str) -> Option<RegKey> {
        let path = format!("SOFTWARE\\Embarcadero\\BDS\\{bds_version}\\{sub_key}");
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(path, KEY_READ)
            .ok()
    }

    fn string_value(key: &RegKey, name: &str) -> Option<String> {
        key.get_value::<String, _>(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    pub fn read_ide_library_settings(bds_version: &str, platform: &str) -> IdeLibrarySettings {
        let Some(key) = bds_key(bds_version, &format!("Library\\{platform}")) else {
            return IdeLibrarySettings::default();
        };
        IdeLibrarySettings {
            search_path: string_value(&key, "Search Path"),
            browsing_path: string_value(&key, "Browsing Path"),
            debug_dcu_path: string_value(&key, "Debug DCU Path"),
            package_dpl_output: string_value(&key, "Package DPL Output"),
            package_dcp_output: string_value(&key, "Package DCP Output"),
        }
    }

    pub fn read_ide_environment_variables(bds_version: &str) -> Vec<(String, String)> {
        let Some(key) = bds_key(bds_version, "Environment Variables") else {
            return Vec::new();
        };
        key.enum_values()
            .filter_map(|entry| entry.ok())
            // Only string values define a usable `$(NAME)` macro.
            .filter(|(_, value)| matches!(value.vtype, REG_SZ | REG_EXPAND_SZ))
            .filter_map(|(name, value)| {
                let text = value.to_string().trim().to_string();
                (!name.trim().is_empty() && !text.is_empty()).then_some((name, text))
            })
            .collect()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::IdeLibrarySettings;

    pub fn read_ide_library_settings(_bds_version: &str, _platform: &str) -> IdeLibrarySettings {
        IdeLibrarySettings::default()
    }

    pub fn read_ide_environment_variables(_bds_version: &str) -> Vec<(String, String)> {
        Vec::new()
    }
}

pub use imp::{read_ide_environment_variables, read_ide_library_settings};
