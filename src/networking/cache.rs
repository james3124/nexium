//! HTTP cache for explicitly fresh GET responses with conditional
//! revalidation, memory-first with an optional filesystem-backed store.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hyper::HeaderMap;
use hyper::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;

use super::client::Response;
use crate::storage::{read_locked, write_atomic};
use crate::utils::Result;

struct CachedResponse {
    url: Url,
    status: hyper::StatusCode,
    headers: HeaderMap,
    body: bytes::Bytes,
    expires: Instant,
    /// Validator headers captured with the stored response, used to build
    /// `If-None-Match` / `If-Modified-Since` once the entry goes stale.
    etag: Option<String>,
    last_modified: Option<String>,
}

impl CachedResponse {
    fn to_response(&self) -> Response {
        Response {
            url: self.url.clone(),
            status: self.status,
            headers: self.headers.clone(),
            body: self.body.clone(),
            redirects: Vec::new(),
        }
    }

    fn has_validator(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

/// Result of a cache probe.
pub enum CacheLookup {
    /// A stored entry that is still fresh; it can be used directly.
    Fresh(Response),
    /// A stored entry that expired but carries a validator and can be
    /// revalidated with a conditional request.
    Stale(Response),
    /// Nothing usable is stored.
    Miss,
}

#[derive(Serialize, Deserialize)]
struct DiskMetadata {
    url: String,
    status: u16,
    headers: Vec<(String, String)>,
    expires_secs: u64,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
}

pub struct HttpCache {
    entries: Mutex<HashMap<String, CachedResponse>>,
    enabled: bool,
    max_entries: usize,
    disk_dir: Mutex<Option<PathBuf>>,
}

impl HttpCache {
    pub fn new(enabled: bool, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            enabled,
            max_entries: max_entries.max(1),
            disk_dir: Mutex::new(None),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_disk_dir(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        if let Ok(mut slot) = self.disk_dir.lock() {
            *slot = Some(dir.to_path_buf());
        }
        Ok(())
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
        let disk_dir = self.disk_dir.lock().ok().and_then(|dir| dir.clone());
        if let Some(dir) = disk_dir
            && let Ok(entries) = std::fs::read_dir(&dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_cache_file = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| matches!(ext, "meta" | "body" | "lock"));
                if is_cache_file {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    /// Probe the cache, returning fresh entries as well as stale entries that
    /// can be revalidated with their stored validator.
    pub fn lookup(&self, url: &Url) -> CacheLookup {
        if !self.enabled {
            return CacheLookup::Miss;
        }
        let key = url.as_str();
        let Ok(mut entries) = self.entries.lock() else {
            return CacheLookup::Miss;
        };
        if let Some(cached) = entries.get(key) {
            if cached.expires > Instant::now() {
                return CacheLookup::Fresh(cached.to_response());
            }
            if cached.has_validator() {
                return CacheLookup::Stale(cached.to_response());
            }
            entries.remove(key);
            return CacheLookup::Miss;
        }
        drop(entries);
        match self.load_disk(url) {
            Some((response, true)) => CacheLookup::Fresh(response),
            Some((response, false)) => CacheLookup::Stale(response),
            None => CacheLookup::Miss,
        }
    }

    /// Return only a fresh cached response (historic API).
    pub fn get(&self, url: &Url) -> Option<Response> {
        match self.lookup(url) {
            CacheLookup::Fresh(response) => Some(response),
            _ => None,
        }
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
        self.insert(url, status, headers, body, freshness);
        self.store_disk(url, status, headers, body, freshness);
    }

    /// Refresh a stored entry after a successful `304 Not Modified`
    /// revalidation: keep the cached body, merge the new headers and reset
    /// freshness from the 304 response.
    pub fn refresh(
        &self,
        url: &Url,
        status: hyper::StatusCode,
        headers: &HeaderMap,
        body: &bytes::Bytes,
    ) {
        if !self.enabled {
            return;
        }
        // A 304 without fresh lifetime information stays cacheable but stale,
        // so the next request revalidates again.
        let freshness = freshness(headers).unwrap_or(Duration::ZERO);
        self.insert(url, status, headers, body, freshness);
        self.store_disk(url, status, headers, body, freshness);
    }

    fn insert(
        &self,
        url: &Url,
        status: hyper::StatusCode,
        headers: &HeaderMap,
        body: &bytes::Bytes,
        freshness: Duration,
    ) {
        let expires = Instant::now() + freshness;
        if let Ok(mut entries) = self.entries.lock() {
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
                    expires,
                    etag: validator(headers, "etag"),
                    last_modified: validator(headers, "last-modified"),
                },
            );
        }
    }

    fn store_disk(
        &self,
        url: &Url,
        status: hyper::StatusCode,
        headers: &HeaderMap,
        body: &bytes::Bytes,
        freshness: Duration,
    ) {
        let Some(dir) = self.disk_dir.lock().ok().and_then(|dir| dir.clone()) else {
            return;
        };
        let (meta_path, body_path) = cache_paths(&dir, url);
        let expires_secs = SystemTime::now()
            .checked_add(freshness)
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let metadata = DiskMetadata {
            url: url.to_string(),
            status: status.as_u16(),
            headers: headers
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.as_str().to_owned(), value.to_owned()))
                })
                .collect(),
            expires_secs,
            etag: validator(headers, "etag"),
            last_modified: validator(headers, "last-modified"),
        };
        let Ok(meta_bytes) = serde_json::to_vec(&metadata) else {
            return;
        };
        let _ = write_atomic(&body_path, body);
        let _ = write_atomic(&meta_path, &meta_bytes);
    }

    /// Load an entry from disk. The boolean is `true` for a fresh entry and
    /// `false` for a stale entry that still has a validator.
    fn load_disk(&self, url: &Url) -> Option<(Response, bool)> {
        let dir = self.disk_dir.lock().ok().and_then(|dir| dir.clone())?;
        let (meta_path, body_path) = cache_paths(&dir, url);
        let meta_bytes = read_locked(&meta_path).ok()??;
        let metadata: DiskMetadata = serde_json::from_slice(&meta_bytes).ok()?;
        let mut headers = HeaderMap::new();
        for (name, value) in &metadata.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.append(name, value);
            }
        }
        let etag = metadata
            .etag
            .clone()
            .or_else(|| validator(&headers, "etag"));
        let last_modified = metadata
            .last_modified
            .clone()
            .or_else(|| validator(&headers, "last-modified"));
        let has_validator = etag.is_some() || last_modified.is_some();

        let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        let fresh = metadata.expires_secs > now_secs;
        if !fresh && !has_validator {
            let _ = std::fs::remove_file(meta_path);
            let _ = std::fs::remove_file(body_path);
            return None;
        }
        let body = bytes::Bytes::from(read_locked(&body_path).ok()??);
        let url_final = Url::parse(&metadata.url).ok()?;
        let status = hyper::StatusCode::from_u16(metadata.status).ok()?;
        let expires = if fresh {
            Instant::now() + Duration::from_secs(metadata.expires_secs.saturating_sub(now_secs))
        } else {
            // Keep the entry available for revalidation only.
            Instant::now()
        };
        let cached = CachedResponse {
            url: url_final.clone(),
            status,
            headers,
            body,
            expires,
            etag,
            last_modified,
        };
        let response = cached.to_response();
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(url_final.as_str().to_owned(), cached);
        }
        Some((response, fresh))
    }
}

fn validator(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn cache_paths(dir: &Path, url: &Url) -> (PathBuf, PathBuf) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.as_str().hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());
    (
        dir.join(format!("{key}.meta")),
        dir.join(format!("{key}.body")),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    fn headers(max_age: &str, etag: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("cache-control", HeaderValue::from_str(max_age).unwrap());
        if let Some(etag) = etag {
            headers.insert("etag", HeaderValue::from_str(etag).unwrap());
        }
        headers
    }

    #[test]
    fn stale_entries_with_validators_are_revalidatable() {
        let cache = HttpCache::new(true, 8);
        let url = Url::parse("http://example.com/a").unwrap();
        cache.put(
            &url,
            hyper::StatusCode::OK,
            &headers("max-age=1", Some("\"v1\"")),
            &bytes::Bytes::from_static(b"body"),
        );
        // Force the entry to look expired.
        if let Ok(mut entries) = cache.entries.lock()
            && let Some(cached) = entries.get_mut(url.as_str())
        {
            cached.expires = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
        }
        match cache.lookup(&url) {
            CacheLookup::Stale(response) => {
                assert_eq!(response.body, bytes::Bytes::from_static(b"body"));
                assert_eq!(response.header("etag"), Some("\"v1\""));
            }
            _ => panic!("expected a stale, revalidatable entry"),
        }
    }

    #[test]
    fn stale_entries_without_validators_are_dropped() {
        let cache = HttpCache::new(true, 8);
        let url = Url::parse("http://example.com/b").unwrap();
        cache.put(
            &url,
            hyper::StatusCode::OK,
            &headers("max-age=1", None),
            &bytes::Bytes::from_static(b"body"),
        );
        if let Ok(mut entries) = cache.entries.lock()
            && let Some(cached) = entries.get_mut(url.as_str())
        {
            cached.expires = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
        }
        assert!(matches!(cache.lookup(&url), CacheLookup::Miss));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn refresh_keeps_body_and_resets_freshness() {
        let cache = HttpCache::new(true, 8);
        let url = Url::parse("http://example.com/c").unwrap();
        cache.put(
            &url,
            hyper::StatusCode::OK,
            &headers("max-age=1", Some("\"v1\"")),
            &bytes::Bytes::from_static(b"body"),
        );
        cache.refresh(
            &url,
            hyper::StatusCode::OK,
            &headers("max-age=60", Some("\"v1\"")),
            &bytes::Bytes::from_static(b"body"),
        );
        assert!(matches!(cache.lookup(&url), CacheLookup::Fresh(_)));
        assert_eq!(cache.len(), 1);
    }
}
