pub mod range;

use std::path::PathBuf;
use anyhow::{Result, Context};
use scopeguard::defer;

use crate::{projects::CompilerConfigurations};

const DEFAULT_FORMATTER_CONFIG: &str = include_str!("presets/ddk_formatter.config");

pub struct Formatter {
    config_path: PathBuf,
    content: String,
}

impl Formatter {
    pub fn new(content: String) -> Result<Self> {
        let config_path = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("Failed to get config dir"))?
            .join("ddk")
            .join("ddk_formatter.config");
        if !config_path.exists() {
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&config_path, DEFAULT_FORMATTER_CONFIG).ok();
        }

        Ok(Formatter { config_path, content })
    }

    pub async fn execute(self) -> Result<String> {
        let temp_file = tempfile::NamedTempFile::with_suffix(".pas")?;
        std::fs::write(temp_file.path(), &self.content)?;
        let temp_file_path = temp_file.into_temp_path();
        defer! {
            std::fs::remove_file(&temp_file_path).ok();
        }
        let formatter = CompilerConfigurations::first_available_formatter()
            .await
            .context("No formatters found (all compiler paths have been searched)")?;
        let status = std::process::Command::new(&formatter)
            .args(&["-e", "utf-8", "-config"])
            .arg(&self.config_path)
            .arg(&temp_file_path)
            .status()
            .context("Failed to execute formatter")?;
        if !status.success() {
            anyhow::bail!("Formatter failed with exit code: {}", status);
        }
        let content = std::fs::read_to_string(&temp_file_path)
            .context("Failed to read formatted code")?;
        return Ok(content.strip_prefix('\u{feff}').unwrap_or(&content).to_string());
    }
}