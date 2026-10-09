use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::state::{COMPILER_CONFIGURATIONS, COMPILER_CONFIGURATIONS_CHANGED, Stateful};
use crate::utils::{FilePath, Load};

pub(crate) const DEFAULT_COMPILERS: &str = include_str!("presets/default_compilers.ron");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialCompilerConfiguration {
    pub condition: Option<String>,
    pub product_name: Option<String>,
    pub product_version: Option<usize>,
    pub package_version: Option<usize>,
    pub compiler_version: Option<usize>,
    pub installation_path: Option<String>,
    pub build_arguments: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerConfiguration {
    pub condition: String,
    pub product_name: String,
    pub product_version: usize,
    pub package_version: usize,
    pub compiler_version: usize,
    pub installation_path: String,
    pub build_arguments: Vec<String>,
}

impl CompilerConfiguration {
    /// The Delphi IDE environment-variable overrides (Tools > Options > IDE >
    /// Environment Variables) belonging to THIS installation, read at runtime
    /// from its own BDS registry hive — `product_version` is the BDS major
    /// version (`23` for Delphi 12 Athens). Never persisted: the registry is
    /// the source of truth and the IDE can change it at any time.
    pub fn ide_environment_overrides(&self) -> Vec<(String, String)> {
        crate::utils::bds_environment_overrides(self.product_version)
    }

    pub fn update(&mut self, partial: &PartialCompilerConfiguration) {
        if let Some(condition) = &partial.condition {
            self.condition = condition.clone();
        }
        if let Some(product_name) = &partial.product_name {
            self.product_name = product_name.clone();
        }
        if let Some(product_version) = partial.product_version {
            self.product_version = product_version;
        }
        if let Some(package_version) = partial.package_version {
            self.package_version = package_version;
        }
        if let Some(compiler_version) = partial.compiler_version {
            self.compiler_version = compiler_version;
        }
        if let Some(installation_path) = &partial.installation_path {
            self.installation_path = installation_path.clone();
        }
        if let Some(build_arguments) = &partial.build_arguments {
            self.build_arguments = build_arguments.clone();
        }
    }
}

type CompilerMap = HashMap<String, CompilerConfiguration>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CompilerConfigurations {
    _compilers: CompilerMap,
}

impl Stateful for CompilerConfigurations {
    fn internal_change_flag() -> &'static AtomicBool {
        &COMPILER_CONFIGURATIONS_CHANGED
    }
    fn get_state() -> &'static Arc<RwLock<Self>> {
        &COMPILER_CONFIGURATIONS
    }
}

impl Serialize for CompilerConfigurations {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self._compilers.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CompilerConfigurations {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut compilers = CompilerMap::deserialize(deserializer)?;
        // Strip any leftover /p:Configuration=… or /p:Platform=… arguments
        // that were baked into user-saved configs from older versions.
        for config in compilers.values_mut() {
            sanitize_build_arguments(&mut config.build_arguments);
            correct_the_delphi_2007_version_symbol(config);
        }
        Ok(CompilerConfigurations {
            _compilers: compilers,
        })
    }
}

/// Corrects a saved Delphi 2007 configuration that still carries `VER190`.
/// That is the symbol of the 2007 compiler **for .NET**; the Win32 compiler
/// declares `VER185`, and nothing a Win32 build produces ever defines
/// `VER190`. The debug target reads this symbol to evaluate a package's
/// `{$LIBSUFFIX}` conditions, so leaving a stored one wrong keeps answering
/// `{$IFDEF VER185}` with the wrong branch. Shipping a corrected preset
/// does not reach anyone: a saved `compilers.ron` is loaded whole and never
/// merged with the defaults.
///
/// Keyed on the pair, so a configuration a user deliberately pointed
/// elsewhere is left alone: BDS 5.0 is Delphi 2007 and nothing else.
fn correct_the_delphi_2007_version_symbol(config: &mut CompilerConfiguration) {
    if config.product_version == 5 && config.condition.eq_ignore_ascii_case("VER190") {
        config.condition = "VER185".to_string();
    }
}

/// Remove `/p:Configuration=…` and `/p:Platform=…` arguments from the
/// build arguments list.  These are now injected dynamically at build time
/// based on per-project / per-workspace overrides.
pub fn sanitize_build_arguments(args: &mut Vec<String>) {
    args.retain(|arg| {
        let lower = arg.to_lowercase();
        !lower.starts_with("/p:configuration=") && !lower.starts_with("/p:platform=")
    });
}

impl CompilerConfigurations {
    pub fn new() -> Self {
        Self::load_from_file(&Self::get_file_path())
    }

    pub async fn first_available_formatter() -> Option<PathBuf> {
        let guard = Self::get_state().read().await;
        for compiler in guard._compilers.values() {
            let path = PathBuf::from(&compiler.installation_path)
                .join("bin")
                .join("Formatter.exe");
            if path.exists() {
                return Some(path);
            }
        }
        None
    }

    pub fn overwrite(&mut self, other: CompilerConfigurations) {
        self._compilers = other._compilers;
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self._compilers.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&CompilerConfiguration> {
        self._compilers.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut CompilerConfiguration> {
        self._compilers.get_mut(key)
    }

    pub fn remove(&mut self, key: &str) -> Option<CompilerConfiguration> {
        self._compilers.remove(key)
    }

    pub fn insert(&mut self, key: String, compiler: CompilerConfiguration) {
        self._compilers.insert(key, compiler);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &CompilerConfiguration)> {
        self._compilers.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self._compilers.keys()
    }

    pub fn validate(&self) -> Result<()> {
        for (key, compiler) in &self._compilers {
            if key.trim().is_empty() {
                anyhow::bail!("Compiler key cannot be empty.");
            }
            if compiler.condition.trim().is_empty() {
                anyhow::bail!("Compiler condition cannot be empty for key: {}", key);
            }
            if compiler.product_name.trim().is_empty() {
                anyhow::bail!("Compiler product name cannot be empty for key: {}", key);
            }
            if compiler.installation_path.trim().is_empty() {
                anyhow::bail!("Compiler installation path cannot be empty for key: {}", key);
            }
            let path = PathBuf::from(&compiler.installation_path);
            if !path.exists() {
                anyhow::bail!("Compiler installation path does not exist for key: {}: {}", key, compiler.installation_path);
            }
            if !path.is_dir() {
                anyhow::bail!("Compiler installation path is not a directory for key: {}: {}", key, compiler.installation_path);
            }
            let rsvars_path = path.join("bin").join("rsvars.bat");
            if !rsvars_path.exists() {
                anyhow::bail!("rsvars.bat not found in compiler installation path for key: {}: {}", key, rsvars_path.display());
            }
        }
        Ok(())
    }
}

impl Default for CompilerConfigurations {
    fn default() -> Self {
        lazy_static::lazy_static!(
            static ref DEFAULT_COMPILERS_MAP: CompilerConfigurations = {
                CompilerConfigurations {
                    _compilers: ron::from_str(DEFAULT_COMPILERS).unwrap_or_else(|_| HashMap::new())
                }
            };
        );
        DEFAULT_COMPILERS_MAP.clone()
    }
}

impl Load for CompilerConfigurations {}

impl FilePath for CompilerConfigurations {
    fn get_file_path() -> &'static PathBuf {
        lazy_static::lazy_static! {
            static ref PATH: PathBuf = {
                dirs::config_dir()
                    .expect("Could not determine config directory")
                .join("ddk")
                .join("compilers.ron")
            };
        }
        return &PATH;
    }
}

pub async fn compiler_exists(key: &str) -> bool {
    CompilerConfigurations::get_state().read().await._compilers.contains_key(key)
}
#[cfg(test)]
mod preset_tests {
    use super::*;

    /// Every preset's `condition` is the `VERxxx` of the compiler that
    /// builds for Windows, which the debug target reads to evaluate a
    /// package's `{$LIBSUFFIX}` conditions. Delphi 2007 is the one whose
    /// Win32 symbol (`VER185`) differs from the .NET compiler's (`VER190`)
    /// and whose `CompilerVersion` (18.5) no whole number can record.
    #[test]
    fn the_delphi_2007_preset_names_the_win32_compiler() {
        let presets: HashMap<String, CompilerConfiguration> = ron::from_str(DEFAULT_COMPILERS).expect("presets parse");
        let delphi_2007 = presets.get("2007").expect("a 2007 preset");

        assert_eq!(delphi_2007.condition, "VER185");
    }

    /// A saved `compilers.ron` is loaded whole, never merged with the
    /// presets, so the corrected preset reaches nobody who has already run
    /// DevKit unless the stored value is migrated on the way in.
    #[test]
    fn a_stored_delphi_2007_keeps_no_dotnet_version_symbol() {
        let stored = r#"{
            "2007": (
                condition: "VER190",
                product_name: "Delphi 2007",
                product_version: 5,
                package_version: 110,
                compiler_version: 19,
                installation_path: r"C:\CodeGear\RAD Studio\5.0",
                build_arguments: [],
            ),
        }"#;

        let loaded: CompilerConfigurations = ron::from_str(stored).expect("stored config parses");

        assert_eq!(loaded._compilers["2007"].condition, "VER185");
    }

    /// Only that pair is corrected: a configuration pointed somewhere of
    /// the user's own choosing is not second-guessed.
    #[test]
    fn a_version_symbol_of_another_product_is_left_alone() {
        let stored = r#"{
            "12.0": (
                condition: "VER190",
                product_name: "Delphi 12",
                product_version: 23,
                package_version: 290,
                compiler_version: 36,
                installation_path: r"C:\Embarcadero\Studio\23.0",
                build_arguments: [],
            ),
        }"#;

        let loaded: CompilerConfigurations = ron::from_str(stored).expect("stored config parses");

        assert_eq!(loaded._compilers["12.0"].condition, "VER190");
    }
}
