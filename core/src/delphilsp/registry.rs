//! Read the RAD Studio IDE's per-user settings that a `.dproj` alone cannot
//! provide: the **global Library Path** (with the Browsing Path and the
//! default package output directories) and the user-defined **environment
//! variable** overrides (`$(VEGADIR)`, `$(DXVCL)`, …).
//!
//! Both live under `HKCU\SOFTWARE\<vendor>\BDS\<version>`, whose vendor segment
//! changed with the product's owner — hence [`IdeRegistryRoot`]. A missing key
//! yields empty data rather than an error; non-Windows builds are stubs.

/// Where one Delphi installation keeps its IDE settings in `HKCU`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeRegistryRoot {
    /// `Borland` up to BDS 5.0 (Delphi 2007), `CodeGear` for 6.0/7.0
    /// (2009/2010), `Embarcadero` from 8.0 (XE) onwards.
    pub vendor: &'static str,
    /// The BDS version segment, e.g. `23.0` for Delphi 12 Athens.
    pub version: String,
}

impl IdeRegistryRoot {
    /// The default root of a BDS major version (`23` for Delphi 12 Athens —
    /// the same number as `CompilerConfiguration::product_version`).
    pub fn for_bds_version(major: usize) -> Self {
        let vendor = match major {
            0..=5 => "Borland",
            6..=7 => "CodeGear",
            _ => "Embarcadero",
        };
        IdeRegistryRoot {
            vendor,
            version: format!("{major}.0"),
        }
    }

    /// The default root for a `"<major>.0"` version string.
    pub fn for_version_string(bds_version: &str) -> Self {
        Self::for_bds_version(bds_major(bds_version))
    }

    /// The path below `HKEY_CURRENT_USER`, e.g. `SOFTWARE\Embarcadero\BDS\23.0`.
    pub fn key_path(&self) -> String {
        format!(r"SOFTWARE\{}\BDS\{}", self.vendor, self.version)
    }

    /// The `Library\<platform>` settings of this installation.
    pub fn library_settings(&self, platform: &str) -> IdeLibrarySettings {
        imp::read_library_settings(self, platform)
    }
}

/// The major number of a `"23.0"` BDS version string (`0` when unparsable).
pub fn bds_major(bds_version: &str) -> usize {
    bds_version.split('.').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// The IDE's library settings for one target platform, `;`-separated and
/// still containing `$(NAME)` macros exactly as the registry holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdeLibrarySettings {
    /// `Search Path` — the global Library Path (compiled units, mostly).
    pub search_path: Option<String>,
    /// `Browsing Path` — where the IDE looks for the sources behind the
    /// library path; what DelphiLSP and the debugger navigate into.
    pub browsing_path: Option<String>,
    /// `Debug DCU Path` — prepended to `-I`/`-U` for debug configurations.
    pub debug_dcu_path: Option<String>,
    /// `Package DPL Output` — the default `-LE` target: where packages are
    /// written when the project does not say otherwise.
    pub package_dpl_output: Option<String>,
    /// `Package DCP Output` — the default `-LN` target.
    pub package_dcp_output: Option<String>,
}

#[cfg(windows)]
mod imp {
    use super::{IdeLibrarySettings, IdeRegistryRoot};
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    fn string_value(key: &RegKey, name: &str) -> Option<String> {
        key.get_value::<String, _>(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    pub fn read_library_settings(root: &IdeRegistryRoot, platform: &str) -> IdeLibrarySettings {
        let path = format!(r"{}\Library\{platform}", root.key_path());
        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(path, KEY_READ) else {
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
}

#[cfg(not(windows))]
mod imp {
    use super::{IdeLibrarySettings, IdeRegistryRoot};

    pub fn read_library_settings(_root: &IdeRegistryRoot, _platform: &str) -> IdeLibrarySettings {
        IdeLibrarySettings::default()
    }
}

/// The IDE library settings of a BDS version (`"23.0"`) for one platform,
/// read from the version's default registry root.
pub fn read_ide_library_settings(bds_version: &str, platform: &str) -> IdeLibrarySettings {
    IdeRegistryRoot::for_version_string(bds_version).library_settings(platform)
}

/// The user-defined environment-variable overrides of one BDS version
/// (`"23.0"`).
pub fn read_ide_environment_variables(bds_version: &str) -> Vec<(String, String)> {
    crate::utils::bds_environment_overrides(bds_major(bds_version))
}

#[cfg(test)]
mod tests {
    use super::IdeRegistryRoot;

    #[test]
    fn root_follows_the_vendor_history() {
        assert_eq!(IdeRegistryRoot::for_bds_version(5).key_path(), r"SOFTWARE\Borland\BDS\5.0");
        assert_eq!(IdeRegistryRoot::for_bds_version(7).key_path(), r"SOFTWARE\CodeGear\BDS\7.0");
        assert_eq!(IdeRegistryRoot::for_version_string("23.0").key_path(), r"SOFTWARE\Embarcadero\BDS\23.0");
    }
}
