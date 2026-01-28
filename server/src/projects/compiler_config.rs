use anyhow::Result;
use ron::ser::PrettyConfig;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::utils::{FileLock, FilePath, Load};

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
        let compilers = CompilerMap::deserialize(deserializer)?;
        Ok(CompilerConfigurations {
            _compilers: compilers,
        })
    }
}

impl CompilerConfigurations {
    pub fn new() -> Self {
        Self::load_from_file(&Self::get_file_path())
    }

    pub fn initialize() -> Result<()> {
        if !Self::get_file_path().exists() {
            let file_lock: FileLock<Self> = FileLock::new()?;
            let data = &file_lock.file;
            data.save()?;
        }
        Ok(())
    }

    pub fn first_available_formatter() -> Option<PathBuf> {
        let file_lock: FileLock<CompilerConfigurations> = FileLock::new().ok()?;
        for compiler in file_lock.file._compilers.values() {
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

    pub fn save(&self) -> Result<()> {
        let path = Self::get_file_path();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("Failed to create config directory: {}", e))?;
        }

        let content = ron::ser::to_string_pretty(&self, PrettyConfig::default()
            .struct_names(true)
            .escape_strings(false))
            .map_err(|e| anyhow::anyhow!("Failed to serialize compilers: {}", e))?;
        std::fs::write(&path, content)
            .map_err(|e| anyhow::anyhow!("Failed to write compilers file: {}", e))?;
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
    fn get_file_path() -> PathBuf {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ddk")
            .join("compilers.ron");
        return path;
    }
}

pub fn compiler_exists(key: &str) -> bool {
    CompilerConfigurations::new().contains_key(key)
}