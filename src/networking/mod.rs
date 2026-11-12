//! Networking subsystem.

pub mod bypass;
pub mod cache;
pub mod client;
pub mod compression;
pub mod cookies;
pub mod ssrf;

pub use bypass::ProxyBypass;
pub use client::{HttpClient, NetworkEvent, Request, Response};
pub use cookies::{CookieRecord, SameSite};

use url::Url;

use crate::utils::{NxError, Result};

/// Parse user supplied URL text. Bare hosts default to `https://`.
pub fn parse_url(input: &str) -> Result<Url> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(NxError::InvalidUrl("empty URL".to_owned()));
    }

    if let Ok(url) = Url::parse(trimmed) {
        if matches!(url.scheme(), "http" | "https") {
            return Ok(url);
        }
        if trimmed.contains("://") {
            return Err(NxError::UnsupportedScheme(url.scheme().to_owned()));
        }
    }

    let candidate = format!("https://{trimmed}");
    Url::parse(&candidate).map_err(|e| NxError::InvalidUrl(format!("{trimmed}: {e}")))
}
