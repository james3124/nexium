use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::utils::{NxError, Result};

/// A bearer token mapped to an agent identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTokenConfig {
    pub token: String,
    pub agent: String,
}

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
    pub max_tabs: usize,
    /// Reject requests to localhost and private IP ranges (SSRF hardening).
    pub block_private_networks: bool,
    pub javascript_enabled: bool,
    pub javascript_max_runtime_ms: u64,
    /// HTTP proxy URL, e.g. `http://127.0.0.1:8080` (CONNECT tunneling).
    pub proxy: Option<String>,
    /// Hosts that connect directly even when `proxy` is set (`no_proxy`).
    ///
    /// Entries may be hostnames, `.suffix` / `*.wildcard` domains, exact IPs,
    /// CIDR ranges (IPv4 or IPv6), `host:port`, or `*` for everything. When
    /// empty the `NO_PROXY` / `no_proxy` environment variables are used.
    #[serde(default, deserialize_with = "deserialize_no_proxy")]
    pub no_proxy: Vec<String>,
    /// Address for the REST/WebSocket/MCP control server.
    pub api_bind: String,
    /// Bearer token required by the control server. `None` disables auth.
    /// Kept for backward compatibility; prefer `api_tokens`.
    pub api_token: Option<String>,
    /// Bearer tokens mapped to agent identities (`[{ token, agent }]`).
    pub api_tokens: Vec<ApiTokenConfig>,
    /// PEM certificate chain enabling native TLS termination for the control
    /// server (requires `api_tls_key`).
    pub api_tls_cert: Option<PathBuf>,
    /// PEM private key (PKCS#8, SEC1 or PKCS#1) for `api_tls_cert`.
    pub api_tls_key: Option<PathBuf>,
    /// Optional HTTPS bind address. When set with `api_tls_cert`/`api_tls_key`,
    /// the server listens on both `api_bind` (plain HTTP) and this address
    /// (TLS). When unset, TLS replaces HTTP on `api_bind`.
    pub api_tls_bind: Option<String>,
    /// Maximum concurrent isolated browser sessions.
    pub api_max_sessions: usize,
    /// Idle sessions older than this are removed when new sessions are created.
    pub api_session_idle_timeout_ms: u64,
    /// Sliding-window REST request budget per agent/session per minute; 0 disables.
    pub api_rate_limit_per_minute: u32,
    /// Sliding-window WebSocket command budget per agent/session per minute.
    pub api_ws_rate_limit_per_minute: u32,
    /// Number of API operations kept in the in-memory audit ring buffer.
    pub api_audit_capacity: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            user_agent: concat!(
                "NexiumX/",
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
            max_tabs: 64,
            block_private_networks: false,
            javascript_enabled: true,
            javascript_max_runtime_ms: 2000,
            proxy: None,
            no_proxy: Vec::new(),
            api_bind: "127.0.0.1:9222".to_owned(),
            api_token: None,
            api_tokens: Vec::new(),
            api_tls_cert: None,
            api_tls_key: None,
            api_tls_bind: None,
            api_max_sessions: 16,
            api_session_idle_timeout_ms: 30 * 60 * 1000,
            api_rate_limit_per_minute: 600,
            api_ws_rate_limit_per_minute: 1200,
            api_audit_capacity: 256,
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

/// Accept `no_proxy` as either a TOML array or a single (comma-separated)
/// string, mirroring the environment variable convention.
fn deserialize_no_proxy<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ListOrString {
        List(Vec<String>),
        One(String),
    }

    Ok(match ListOrString::deserialize(deserializer)? {
        ListOrString::List(values) => values,
        ListOrString::One(value) => vec![value],
    })
}
