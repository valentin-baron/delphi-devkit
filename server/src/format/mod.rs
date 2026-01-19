use std::path::PathBuf;
use anyhow::{Result, Context};
use scopeguard::defer;
use tower_lsp::lsp_types::{Range, Url};

use crate::{projects::CompilerConfigurations, utils::Document};

const DEFAULT_FORMATTER_CONFIG: &str = include_str!("presets/ddk_formatter.config");

pub struct Formatter {
    config_path: PathBuf,
    file_path: PathBuf,
}

impl Formatter {
    pub fn new(url: Url) -> Result<Self> {
        let config_path = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("Failed to get config dir"))?
            .join("ddk")
            .join("ddk_formatter.config");
        if !config_path.exists() {
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&config_path, DEFAULT_FORMATTER_CONFIG).ok();
        }
        let file_path = url.to_file_path().map_err(|_| anyhow::anyhow!("Invalid file URL"))?;
        if !file_path.exists() {
            anyhow::bail!("File does not exist: {}", file_path.display());
        }

        Ok(Formatter { config_path, file_path })
    }

    pub fn execute(&self, range: Option<Range>) -> Result<String> {
        let mut code = std::fs::read_to_string(&self.file_path)
            .context("Failed to read file content")?;
        if let Some(range) = range {
            let document = Document::new(&code);
            code = document.range(range).to_string();
        }
        let temp_file = tempfile::NamedTempFile::new()?;
        std::fs::write(temp_file.path(), code)?;
        let temp_file_path = temp_file.path();
        defer! {
            std::fs::remove_file(temp_file_path).ok();
        }
        let formatter = CompilerConfigurations::first_available_formatter().context("No formatter configured")?;
        let status = std::process::Command::new(&formatter)
            .arg("-config")
            .arg(&self.config_path)
            .arg(temp_file_path)
            .status()
            .context("Failed to execute formatter")?;
        if !status.success() {
            anyhow::bail!("Formatter failed with exit code: {}", status);
        }
        let content = std::fs::read_to_string(temp_file_path)
            .context("Failed to read formatted code")?;
        return Ok(content);
    }
}