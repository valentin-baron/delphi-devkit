//! The RAD Studio IDE's per-user, per-platform library settings, read from
//! the registry: the global **Library Path**, the **Browsing Path** and the
//! default package output directories. A `.dproj` never carries these, yet a
//! debugger needs the browsing path above all — it is where the IDE finds the
//! *sources* of the units the library path only provides compiled.
//!
//! The registry root is modelled explicitly ([`IdeRegistryRoot`]) because it
//! is not a constant: the vendor segment changed with the product's owner,
//! and RAD Studio can run against an alternative key (`bds.exe -r<Key>`)
//! so that one installation serves several component sets. Everything here
//! degrades gracefully — a missing key yields empty settings, never an error.

use crate::projects::CompilerConfiguration;

/// Where one Delphi installation keeps its IDE settings in `HKCU`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeRegistryRoot {
    /// `Borland` up to BDS 5.0 (Delphi 2007), `CodeGear` for 6.0/7.0
    /// (2009/2010), `Embarcadero` from 8.0 (XE) onwards.
    pub vendor: &'static str,
    /// The key under the vendor: `BDS` by default. This is the segment an
    /// IDE started with `-r<Key>` replaces.
    pub key: String,
    /// The BDS version segment, e.g. `23.0` for Delphi 12 Athens.
    pub version: String,
}

impl IdeRegistryRoot {
    /// The default root of the installation a compiler configuration
    /// describes — `product_version` is the BDS major version.
    pub fn for_compiler(compiler: &CompilerConfiguration) -> Self {
        let vendor = match compiler.product_version {
            0..=5 => "Borland",
            6..=7 => "CodeGear",
            _ => "Embarcadero",
        };
        IdeRegistryRoot {
            vendor,
            key: "BDS".to_string(),
            version: format!("{}.0", compiler.product_version),
        }
    }

    /// The path below `HKEY_CURRENT_USER`, e.g. `SOFTWARE\Embarcadero\BDS\23.0`.
    pub fn key_path(&self) -> String {
        format!(r"SOFTWARE\{}\{}\{}", self.vendor, self.key, self.version)
    }

    /// The `Library\<platform>` settings of this installation.
    pub fn library_settings(&self, platform: &str) -> IdeLibrarySettings {
        imp::read_library_settings(self, platform)
    }
}

/// The IDE's library settings for one target platform, `;`-separated and
/// still containing `$(NAME)` macros exactly as the registry holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdeLibrarySettings {
    /// `Search Path` — the global Library Path (compiled units, mostly).
    pub search_path: Option<String>,
    /// `Browsing Path` — where the IDE looks for the sources behind the
    /// library path; the debugger's most valuable source roots.
    pub browsing_path: Option<String>,
    /// `Package DPL Output` — where packages are written when the project
    /// does not say otherwise.
    pub package_dpl_output: Option<String>,
    /// `Package DCP Output` — the matching default for `.dcp` files.
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
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    pub fn read_library_settings(root: &IdeRegistryRoot, platform: &str) -> IdeLibrarySettings {
        let path = format!(r"{}\Library\{platform}", root.key_path());
        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(path, KEY_READ) else {
            return IdeLibrarySettings::default();
        };
        IdeLibrarySettings {
            search_path: string_value(&key, "Search Path"),
            browsing_path: string_value(&key, "Browsing Path"),
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

#[cfg(test)]
mod tests {
    use super::IdeRegistryRoot;
    use crate::projects::CompilerConfiguration;

    fn compiler(product_version: usize) -> CompilerConfiguration {
        CompilerConfiguration {
            condition: String::new(),
            product_name: String::new(),
            product_version,
            package_version: 0,
            compiler_version: 0,
            installation_path: String::new(),
            build_arguments: Vec::new(),
        }
    }

    #[test]
    fn root_follows_the_vendor_history() {
        assert_eq!(IdeRegistryRoot::for_compiler(&compiler(5)).key_path(), r"SOFTWARE\Borland\BDS\5.0");
        assert_eq!(IdeRegistryRoot::for_compiler(&compiler(7)).key_path(), r"SOFTWARE\CodeGear\BDS\7.0");
        assert_eq!(IdeRegistryRoot::for_compiler(&compiler(23)).key_path(), r"SOFTWARE\Embarcadero\BDS\23.0");
    }

    #[test]
    fn a_custom_key_replaces_the_bds_segment() {
        let mut root = IdeRegistryRoot::for_compiler(&compiler(23));
        root.key = "VegaBranchX".to_string();
        assert_eq!(root.key_path(), r"SOFTWARE\Embarcadero\VegaBranchX\23.0");
    }
}
