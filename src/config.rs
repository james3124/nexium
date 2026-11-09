use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::utils::{NxError, Result};

/// Engine configuration (TOML-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub user_agent: String,
    pub timeout_ms: u64,
    pub max_redirects: usize,
    pub max_response_bytes: usize,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub data_dir: PathBuf,
    pub storage_enabled: bool,
    pub cookies_enabled: bool,
    pub cache_enabled: bool,
    pub cache_max_entries: usize,
    /// HTTP proxy URL, e.g. `http://127.0.0.1:8080` (CONNECT tunneling).
    pub proxy: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            user_agent: concat!(
                "Nexium/",
                env!("CARGO_PKG_VERSION"),
                " (independent Rust headless browser engine)"
            )
            .to_owned(),
            timeout_ms: 30_000,
            max_redirects: 10,
            max_response_bytes: 32 * 1024 * 1024,
            viewport_width: 1280,
            viewport_height: 720,
            data_dir: PathBuf::from("nexium-data"),
            storage_enabled: true,
            cookies_enabled: true,
            cache_enabled: true,
            cache_max_entries: 128,
            proxy: None,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| NxError::Config(format!("cannot read config {}: {e}", path.display())))?;
        toml::from_str(&text)
            .map_err(|e| NxError::Config(format!("invalid config {}: {e}", path.display())))
    }

    /// Load `path` when it exists, otherwise return defaults.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}
