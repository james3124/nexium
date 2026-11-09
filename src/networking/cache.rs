//! Small in-memory HTTP cache for explicitly fresh GET responses.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use hyper::HeaderMap;
use url::Url;

use super::client::Response;

struct CachedResponse {
    url: Url,
    status: hyper::StatusCode,
    headers: HeaderMap,
    body: bytes::Bytes,
    expires: Instant,
}

pub struct HttpCache {
    entries: Mutex<HashMap<String, CachedResponse>>,
    enabled: bool,
    max_entries: usize,
}

impl HttpCache {
    pub fn new(enabled: bool, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            enabled,
            max_entries: max_entries.max(1),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }

    pub fn get(&self, url: &Url) -> Option<Response> {
        if !self.enabled {
            return None;
        }
        let key = url.as_str();
        let Ok(mut entries) = self.entries.lock() else {
            return None;
        };
        let cached = entries.get(key)?;
        if cached.expires <= Instant::now() {
            entries.remove(key);
            return None;
        }
        Some(Response {
            url: cached.url.clone(),
            status: cached.status,
            headers: cached.headers.clone(),
            body: cached.body.clone(),
            redirects: Vec::new(),
        })
    }

    pub fn put(
        &self,
        url: &Url,
        status: hyper::StatusCode,
        headers: &HeaderMap,
        body: &bytes::Bytes,
    ) {
        if !self.enabled || !status.is_success() {
            return;
        }
        if headers.contains_key("set-cookie") || headers.contains_key("vary") {
            return;
        }
        let Some(freshness) = freshness(headers) else {
            return;
        };
        if freshness.is_zero() {
            return;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.len() >= self.max_entries
            && !entries.contains_key(url.as_str())
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, cached)| cached.expires)
                .map(|(key, _)| key.clone())
        {
            entries.remove(&oldest);
        }
        entries.insert(
            url.as_str().to_owned(),
            CachedResponse {
                url: url.clone(),
                status,
                headers: headers.clone(),
                body: body.clone(),
                expires: Instant::now() + freshness,
            },
        );
    }
}

fn freshness(headers: &HeaderMap) -> Option<Duration> {
    let cache_control = headers
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let mut max_age = None;
    for directive in cache_control.split(',') {
        let directive = directive.trim();
        let (name, value) = directive
            .split_once('=')
            .map(|(name, value)| (name.trim(), value.trim().trim_matches('"')))
            .unwrap_or((directive, ""));
        match name.to_ascii_lowercase().as_str() {
            "no-store" | "no-cache" => return None,
            "max-age" => {
                if let Ok(seconds) = value.parse::<u64>() {
                    max_age = Some(Duration::from_secs(seconds));
                }
            }
            _ => {}
        }
    }
    if let Some(max_age) = max_age {
        return Some(max_age);
    }
    let expires = headers
        .get("expires")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())?;
    expires.duration_since(SystemTime::now()).ok()
}
