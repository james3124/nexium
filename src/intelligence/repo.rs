//! Repository navigation primitives: parse a GitHub reference, then list and
//! read its contents through a pluggable backend.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Instant;

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::browser::{Browser, Page};
use crate::config::Config;
use crate::engine::dom::{Document, NodeId};
use crate::intelligence::visible_text;
use crate::networking::{HttpClient, Request, parse_url};

/// A validated repository reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RepoRef {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub rf: Option<String>,
    pub path: String,
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)?;
        if let Some(rf) = &self.rf {
            write!(f, "@{rf}")?;
        }
        if !self.path.is_empty() {
            write!(f, ":{}", self.path)?;
        }
        Ok(())
    }
}

impl RepoRef {
    /// Parse a GitHub URL or `owner/repo@ref:path` shorthand.
    pub fn parse(input: &str) -> Result<Self, RepoError> {
        let input = input.trim().trim_end_matches('/');
        if input.is_empty() {
            return Err(RepoError::bad_request("repository reference is empty"));
        }
        if input.starts_with("http://") || input.starts_with("https://") {
            return Self::from_url(input);
        }
        let (repo_part, path) = match input.split_once(':') {
            Some((left, right)) => (left, validate_path(right)?),
            None => (input, String::new()),
        };
        let (repo_part, rf) = match repo_part.split_once('@') {
            Some((left, right)) => (
                left,
                Some(validate_segment(right, "ref")?.to_ascii_lowercase()),
            ),
            None => (repo_part, None),
        };
        let mut parts = repo_part.split('/').filter(|part| !part.is_empty());
        let owner = validate_segment(parts.next().unwrap_or(""), "owner")?;
        let repo = validate_segment(parts.next().unwrap_or(""), "repo")?;
        if parts.next().is_some() {
            return Err(RepoError::bad_request("expected 'owner/repo'"));
        }
        Ok(Self {
            host: "github.com".to_owned(),
            owner,
            repo,
            rf,
            path,
        })
    }

    fn from_url(input: &str) -> Result<Self, RepoError> {
        // The URL parser collapses dot segments, so inspect the raw path before
        // parsing to keep encoded traversal (`%2e%2e`) from being normalized away.
        if raw_path_has_traversal(input) {
            return Err(RepoError::bad_request("path traversal is not allowed"));
        }
        let url = parse_url(input).map_err(|_| RepoError::bad_request("invalid URL"))?;
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        if host != "github.com" && host != "raw.githubusercontent.com" {
            return Err(RepoError::bad_request("only github.com URLs are supported"));
        }
        let segments: Vec<String> = url
            .path_segments()
            .map(|parts| {
                parts
                    .filter(|part| !part.is_empty())
                    .map(percent_decode)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        if segments.len() < 2 {
            return Err(RepoError::bad_request("URL has no owner/repo pair"));
        }
        let owner = validate_segment(&segments[0], "owner")?;
        let repo = validate_segment(&segments[1], "repo")?;
        let (rf, path) = if host == "raw.githubusercontent.com" {
            if segments.len() < 4 {
                return Err(RepoError::bad_request("raw URL has no ref/path"));
            }
            (
                Some(validate_segment(&segments[2], "ref")?.to_ascii_lowercase()),
                validate_path(&segments[3..].join("/"))?,
            )
        } else if segments.len() >= 4 && (segments[2] == "tree" || segments[2] == "blob") {
            (
                Some(validate_segment(&segments[3], "ref")?.to_ascii_lowercase()),
                validate_path(&segments[4..].join("/"))?,
            )
        } else {
            (None, String::new())
        };
        Ok(Self {
            host,
            owner,
            repo,
            rf,
            path,
        })
    }

    /// A copy with a different validated path.
    pub fn at_path(&self, path: &str) -> Result<Self, RepoError> {
        let mut copy = self.clone();
        copy.path = validate_path(path)?;
        Ok(copy)
    }

    /// Resolve `..` segments against the current path.
    pub fn relative(&self, target: &str) -> Result<Self, RepoError> {
        if target.starts_with("http://") || target.starts_with("https://") {
            return Self::parse(target);
        }
        let mut segments: Vec<&str> = if target.starts_with('/') {
            Vec::new()
        } else {
            self.path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect()
        };
        for part in target.split('/') {
            match part {
                "" | "." => continue,
                ".." => {
                    segments.pop();
                }
                _ => segments.push(part),
            }
        }
        self.at_path(&segments.join("/"))
    }

    pub fn api_repo_path(&self) -> String {
        format!("repos/{}/{}", self.owner, self.repo)
    }

    fn api_url(&self, base: &str, suffix: &str, path: Option<&str>) -> Result<Url, RepoError> {
        let mut url = Url::parse(&format!("{}/{}/{suffix}", base, self.api_repo_path()))
            .map_err(|_| RepoError::upstream("could not build GitHub API URL"))?;
        if let Some(rf) = &self.rf {
            url.query_pairs_mut().append_pair("ref", rf);
        }
        if let Some(path) = path
            && !path.is_empty()
        {
            url.path_segments_mut()
                .map_err(|_| RepoError::upstream("opaque GitHub API URL"))?
                .extend(path.split('/'));
        }
        Ok(url)
    }
}

fn validate_segment(value: &str, label: &str) -> Result<String, RepoError> {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || value.contains(['/', '\\', '\0'])
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(RepoError::bad_request(format!("invalid {label}")));
    }
    Ok(value.to_owned())
}

fn validate_path(path: &str) -> Result<String, RepoError> {
    if path.len() > 4096 || path.contains('\\') || path.contains('\0') {
        return Err(RepoError::bad_request("invalid path"));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(RepoError::bad_request("path traversal is not allowed")),
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

/// Check the unparsed URL path for `..`, including percent-escaped dot
/// segments. Runs before the URL parser normalizes them away.
fn raw_path_has_traversal(input: &str) -> bool {
    let rest = input.split_once("://").map_or(input, |(_, rest)| rest);
    let path = rest.split_once('?').map_or(rest, |(path, _)| path);
    let path = path.split_once('#').map_or(path, |(path, _)| path);
    let after_host = path.split_once('/').map_or("", |(_, path)| path);
    after_host.split('/').any(|segment| {
        percent_decode(segment)
            .unwrap_or_else(|_| segment.to_owned())
            .split('/')
            .any(|part| part == "..")
    })
}

/// Percent-decode a URL path segment. Rejects malformed escapes and bytes that
/// are not valid UTF-8 so a decoded `..` cannot slip through validation.
fn percent_decode(value: &str) -> Result<String, RepoError> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let Some(&high) = bytes.get(index + 1) else {
                return Err(RepoError::bad_request("invalid percent escape in URL"));
            };
            let Some(&low) = bytes.get(index + 2) else {
                return Err(RepoError::bad_request("invalid percent escape in URL"));
            };
            let hex = |byte: u8| -> Option<u8> {
                match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    b'A'..=b'F' => Some(byte - b'A' + 10),
                    _ => None,
                }
            };
            let (Some(high), Some(low)) = (hex(high), hex(low)) else {
                return Err(RepoError::bad_request("invalid percent escape in URL"));
            };
            out.push(high * 16 + low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| RepoError::bad_request("invalid UTF-8 in URL path"))
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepoKind {
    Dir,
    File,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoEntry {
    pub name: String,
    pub path: String,
    pub kind: RepoKind,
    pub size: Option<u64>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoListing {
    pub entries: Vec<RepoEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoFile {
    pub text: String,
    pub lines: Vec<RepoLine>,
    pub total_lines: usize,
    pub truncated: bool,
    pub binary: bool,
    pub language_hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoLine {
    pub n: usize,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoMetadata {
    pub default_branch: String,
    pub description: Option<String>,
    pub visibility: String,
    pub size_kb: Option<u64>,
}

#[derive(Debug)]
pub enum RepoError {
    BadRequest(String),
    NotFound(String),
    RateLimited(String),
    Upstream(String),
}

impl RepoError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::RateLimited(message.into())
    }
    pub fn upstream(message: impl Into<String>) -> Self {
        Self::Upstream(message.into())
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::NotFound(_) => "not_found",
            Self::RateLimited(_) | Self::Upstream(_) => "upstream",
        }
    }

    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::RateLimited(_) => Some(60_000),
            _ => None,
        }
    }
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest(message)
            | Self::NotFound(message)
            | Self::RateLimited(message)
            | Self::Upstream(message) => write!(f, "{message}"),
        }
    }
}

/// Backend for repository structure and file reads.
///
/// Methods return boxed futures so the trait is object-safe and can be held as
/// `Arc<dyn RepoBackend>` by the API service. Callers still use plain `.await`.
pub trait RepoBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn metadata<'a>(
        &'a self,
        rf: &'a RepoRef,
    ) -> Pin<Box<dyn Future<Output = Result<RepoMetadata, RepoError>> + Send + 'a>>;
    fn list<'a>(
        &'a self,
        rf: &'a RepoRef,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RepoListing, RepoError>> + Send + 'a>>;
    fn tree<'a>(
        &'a self,
        rf: &'a RepoRef,
        prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RepoEntry>, RepoError>> + Send + 'a>>;
    fn read<'a>(
        &'a self,
        rf: &'a RepoRef,
        path: &'a str,
        start_line: Option<usize>,
        max_lines: Option<usize>,
    ) -> Pin<Box<dyn Future<Output = Result<RepoFile, RepoError>> + Send + 'a>>;
}

#[derive(Clone)]
struct CachedResponse {
    body: Bytes,
    etag: Option<String>,
    fetched: Instant,
}

/// GitHub REST/raw-content backend with ETag revalidation and a bounded cache.
pub struct GitHubApiBackend {
    client: HttpClient,
    config: Config,
    cache: Mutex<HashMap<String, CachedResponse>>,
}

impl GitHubApiBackend {
    pub fn new(config: &Config) -> Self {
        Self {
            client: HttpClient::new(config),
            config: config.clone(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn api_base(&self) -> &str {
        self.config
            .repo_api_base
            .as_deref()
            .unwrap_or("https://api.github.com")
    }

    fn raw_base(&self) -> &str {
        self.config
            .repo_raw_base
            .as_deref()
            .unwrap_or("https://raw.githubusercontent.com")
    }

    fn token(&self) -> Option<String> {
        self.config.github_token.clone().or_else(|| {
            std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|value| !value.is_empty())
        })
    }

    async fn get(&self, url: Url, accept: &str) -> Result<(Bytes, Option<String>), RepoError> {
        let key = url.to_string();
        let etag = self.cache.lock().ok().and_then(|cache| {
            cache.get(&key).and_then(|entry| {
                (entry.fetched.elapsed().as_secs() < self.config.repo_cache_ttl_secs)
                    .then(|| entry.etag.clone())
                    .flatten()
            })
        });
        let mut request = Request::get(url.clone());
        request = request
            .header("Accept", accept)
            .map_err(|_| RepoError::bad_request("invalid Accept header"))?
            .header("User-Agent", &self.config.user_agent)
            .map_err(|_| RepoError::bad_request("invalid User-Agent"))?
            .header("X-GitHub-Api-Version", "2022-11-28")
            .map_err(|_| RepoError::bad_request("invalid API version header"))?;
        if let Some(token) = self.token() {
            request = request
                .header("Authorization", &format!("Bearer {token}"))
                .map_err(|_| RepoError::bad_request("invalid token header"))?;
        }
        if let Some(etag) = etag {
            request = request
                .header("If-None-Match", &etag)
                .map_err(|_| RepoError::bad_request("invalid cache header"))?;
        }
        let response = self
            .client
            .fetch(request)
            .await
            .map_err(|error| RepoError::upstream(format!("GitHub request failed: {error}")))?;
        let status = response.status.as_u16();
        if status == 304 {
            if let Some(entry) = self
                .cache
                .lock()
                .ok()
                .and_then(|cache| cache.get(&key).cloned())
            {
                return Ok((entry.body, entry.etag));
            }
            return Err(RepoError::upstream(
                "cache entry expired during revalidation",
            ));
        }
        if status == 404 {
            return Err(RepoError::not_found(format!("not found: {}", url.path())));
        }
        if status == 403 || status == 429 {
            return Err(RepoError::rate_limited("GitHub rate limit reached"));
        }
        if status >= 400 {
            return Err(RepoError::upstream(format!(
                "GitHub returned HTTP {status}"
            )));
        }
        let body = response.body.clone();
        let response_etag = response.header("etag").map(str::to_owned);
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= self.config.repo_cache_max_entries {
                if let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, entry)| entry.fetched)
                    .map(|(key, _)| key.clone())
                {
                    cache.remove(&oldest);
                }
            }
            cache.insert(
                key,
                CachedResponse {
                    body: body.clone(),
                    etag: response_etag.clone(),
                    fetched: Instant::now(),
                },
            );
        }
        Ok((body, response_etag))
    }

    async fn json(&self, url: Url) -> Result<Value, RepoError> {
        let (body, _) = self.get(url, "application/vnd.github+json").await?;
        serde_json::from_slice(&body)
            .map_err(|error| RepoError::upstream(format!("invalid GitHub JSON: {error}")))
    }
}

impl RepoBackend for GitHubApiBackend {
    fn name(&self) -> &'static str {
        "github"
    }

    fn metadata<'a>(
        &'a self,
        rf: &'a RepoRef,
    ) -> Pin<Box<dyn Future<Output = Result<RepoMetadata, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let url = Url::parse(&format!("{}/{}", self.api_base(), rf.api_repo_path()))
                .map_err(|_| RepoError::upstream("invalid repository URL"))?;
            let value = self.json(url).await?;
            Ok(RepoMetadata {
                default_branch: value
                    .get("default_branch")
                    .and_then(Value::as_str)
                    .unwrap_or("main")
                    .to_owned(),
                description: value
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                visibility: value
                    .get("visibility")
                    .and_then(Value::as_str)
                    .unwrap_or("public")
                    .to_owned(),
                size_kb: value.get("size").and_then(Value::as_u64),
            })
        })
    }

    fn list<'a>(
        &'a self,
        rf: &'a RepoRef,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RepoListing, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let path = validate_path(path)?;
            let url = rf.api_url(self.api_base(), "contents", Some(&path))?;
            let value = self.json(url).await?;
            let mut entries = Vec::new();
            if let Some(items) = value.as_array() {
                for item in items {
                    let Some(name) = item.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let kind = match item.get("type").and_then(Value::as_str).unwrap_or("file") {
                        "dir" => RepoKind::Dir,
                        _ => RepoKind::File,
                    };
                    let entry_path = item
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or(name)
                        .to_owned();
                    entries.push(RepoEntry {
                        name: name.to_owned(),
                        path: entry_path,
                        kind,
                        size: item.get("size").and_then(Value::as_u64),
                        url: item
                            .get("html_url")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    });
                }
            }
            let truncated = entries.len() >= self.config.repo_max_tree_entries;
            entries.truncate(self.config.repo_max_tree_entries);
            entries.sort_by(|a, b| {
                (a.kind == RepoKind::File)
                    .cmp(&(b.kind == RepoKind::File))
                    .then(a.name.cmp(&b.name))
            });
            Ok(RepoListing { entries, truncated })
        })
    }

    fn tree<'a>(
        &'a self,
        rf: &'a RepoRef,
        prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RepoEntry>, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let rf = rf.clone();
            let ref_name = match rf.rf.clone() {
                Some(rf) => rf,
                None => self.metadata(&rf).await?.default_branch,
            };
            let url = Url::parse(&format!(
                "{}/{}/git/trees/{ref_name}?recursive=1",
                self.api_base(),
                rf.api_repo_path()
            ))
            .map_err(|_| RepoError::upstream("invalid tree URL"))?;
            let value = self.json(url).await?;
            let truncated = value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut entries = Vec::new();
            if let Some(items) = value.get("tree").and_then(Value::as_array) {
                for item in items {
                    let Some(path) = item.get("path").and_then(Value::as_str) else {
                        continue;
                    };
                    if !prefix.is_empty() && !path.starts_with(prefix) {
                        continue;
                    }
                    let kind = match item.get("type").and_then(Value::as_str).unwrap_or("blob") {
                        "tree" => RepoKind::Dir,
                        _ => RepoKind::File,
                    };
                    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                    entries.push(RepoEntry {
                        name,
                        path: path.to_owned(),
                        kind,
                        size: item.get("size").and_then(Value::as_u64),
                        url: item.get("url").and_then(Value::as_str).map(str::to_owned),
                    });
                }
            }
            let capped = entries.len() >= self.config.repo_max_tree_entries || truncated;
            entries.truncate(self.config.repo_max_tree_entries);
            if capped {
                // Keep the listing deterministic even when truncated.
                entries.sort_by(|a, b| a.path.cmp(&b.path));
            }
            Ok(entries)
        })
    }

    fn read<'a>(
        &'a self,
        rf: &'a RepoRef,
        path: &'a str,
        start_line: Option<usize>,
        max_lines: Option<usize>,
    ) -> Pin<Box<dyn Future<Output = Result<RepoFile, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let path = validate_path(path)?;
            if path.is_empty() {
                return Err(RepoError::bad_request("a file path is required"));
            }
            let rf = rf.clone();
            let ref_name = match rf.rf.clone() {
                Some(rf) => rf,
                None => self.metadata(&rf).await?.default_branch,
            };
            let mut url = Url::parse(&format!(
                "{}/{}/{}/{ref_name}",
                self.raw_base(),
                rf.owner,
                rf.repo
            ))
            .map_err(|_| RepoError::upstream("invalid raw URL"))?;
            url.path_segments_mut()
                .map_err(|_| RepoError::upstream("opaque raw URL"))?
                .extend(path.split('/'));
            let (body, _) = self.get(url, "text/plain").await?;
            Ok(file_from_bytes(
                &body,
                &path,
                start_line,
                max_lines,
                self.config.repo_max_file_bytes,
            ))
        })
    }
}

/// Shared binary detection, line numbering and truncation for both backends.
fn file_from_bytes(
    bytes: &[u8],
    path: &str,
    start_line: Option<usize>,
    max_lines: Option<usize>,
    max_bytes: usize,
) -> RepoFile {
    let binary =
        bytes.contains(&0) || std::str::from_utf8(bytes).is_err() || bytes.len() > max_bytes;
    if binary {
        return RepoFile {
            text: String::new(),
            lines: Vec::new(),
            total_lines: 0,
            truncated: true,
            binary: true,
            language_hint: language_hint(path),
        };
    }
    let text = String::from_utf8_lossy(bytes).into_owned();
    let all_lines: Vec<&str> = text.lines().collect();
    let start = start_line.unwrap_or(1).saturating_sub(1);
    let start = start.min(all_lines.len());
    let limit = max_lines.unwrap_or(400).clamp(1, 2000);
    let end = (start + limit).min(all_lines.len());
    let lines = all_lines[start..end]
        .iter()
        .enumerate()
        .map(|(index, line)| RepoLine {
            n: start + index + 1,
            text: (*line).to_owned(),
        })
        .collect();
    RepoFile {
        text: all_lines[start..end].join("\n"),
        lines,
        total_lines: all_lines.len(),
        truncated: end < all_lines.len(),
        binary: false,
        language_hint: language_hint(path),
    }
}

/// Best-effort repository backend that reads the GitHub web UI through the
/// same browser used for every other page in Nexium. GitHub markup is not a
/// stable API: missing anchors simply yield fewer entries, never a panic.
pub struct BrowserRepoBackend {
    config: Config,
}

impl BrowserRepoBackend {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
        }
    }

    fn web_base(&self) -> &str {
        self.config
            .repo_web_base
            .as_deref()
            .unwrap_or("https://github.com")
            .trim_end_matches('/')
    }

    fn root_url(&self, rf: &RepoRef) -> String {
        format!("{}/{}/{}", self.web_base(), rf.owner, rf.repo)
    }

    /// GitHub accepts `HEAD` as a ref alias; use it when the caller did not
    /// pin a ref (the API backend resolves the default branch instead).
    fn ref_name(rf: &RepoRef) -> String {
        rf.rf.clone().unwrap_or_else(|| "HEAD".to_owned())
    }

    fn tree_url(&self, rf: &RepoRef, path: &str) -> String {
        let root = self.root_url(rf);
        let rf_name = Self::ref_name(rf);
        if path.is_empty() {
            format!("{root}/tree/{rf_name}")
        } else {
            format!("{root}/tree/{rf_name}/{path}")
        }
    }

    fn blob_url(&self, rf: &RepoRef, path: &str) -> String {
        format!("{}/blob/{}/{}", self.root_url(rf), Self::ref_name(rf), path)
    }

    async fn open(&self, url: &str) -> Result<Page, RepoError> {
        // Repository trees and blob pages are server-rendered; executing the
        // host page's scripts is unnecessary and can overflow the JS runtime
        // on large code-hosting pages.
        let mut config = self.config.clone();
        config.javascript_enabled = false;
        let browser = Browser::new(config);
        let page = browser
            .open(url)
            .await
            .map_err(|error| RepoError::upstream(format!("browser navigation failed: {error}")))?;
        if page.status.as_u16() == 404 {
            return Err(RepoError::not_found(format!("not found: {url}")));
        }
        Ok(page)
    }

    /// Split a same-host anchor into `(kind, repo path, ref, absolute URL)`.
    /// Both GitHub-style absolute URLs and fixture-relative hrefs are accepted.
    fn href_parts(
        &self,
        page: &Page,
        href: &str,
        rf: &RepoRef,
    ) -> Option<(RepoKind, String, String, String)> {
        let url = Url::parse(href).ok().or_else(|| page.url.join(href).ok())?;
        if url.host_str()? != page.url.host_str()? {
            return None;
        }
        let segments: Vec<String> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .map(percent_decode)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if segments.len() < 4 || segments[0] != rf.owner || segments[1] != rf.repo {
            return None;
        }
        let kind = match segments[2].as_str() {
            "tree" => RepoKind::Dir,
            "blob" => RepoKind::File,
            _ => return None,
        };
        let reference = segments[3].clone();
        let path = segments[4..].join("/");
        Some((kind, path, reference, url.to_string()))
    }

    fn matches_href(
        &self,
        page: &Page,
        href: &str,
        rf: &RepoRef,
        expected_ref: Option<&str>,
    ) -> Option<(RepoKind, String, String)> {
        let (kind, path, reference, url) = self.href_parts(page, href, rf)?;
        if path.is_empty() || expected_ref.is_some_and(|expected| expected != reference) {
            return None;
        }
        Some((kind, path, url))
    }

    fn entries_from_page(&self, page: &Page, rf: &RepoRef, prefix: &str) -> Vec<RepoEntry> {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let expected_ref = rf.rf.as_deref();
        for id in page.document.descendants(page.document.document_node()) {
            let Some(element) = page.document.element(id) else {
                continue;
            };
            if !element.name.eq_ignore_ascii_case("a") {
                continue;
            }
            let Some(href) = element.get_attribute("href") else {
                continue;
            };
            let Some((kind, path, url)) = self.matches_href(page, href, rf, expected_ref) else {
                continue;
            };
            if !prefix.is_empty() && path != prefix && !path.starts_with(&format!("{prefix}/")) {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            entries.push(RepoEntry {
                name,
                path,
                kind,
                size: None,
                url: Some(url),
            });
        }
        entries.sort_by(|a, b| {
            (a.kind == RepoKind::File)
                .cmp(&(b.kind == RepoKind::File))
                .then(a.path.cmp(&b.path))
        });
        entries
    }
}

impl RepoBackend for BrowserRepoBackend {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn metadata<'a>(
        &'a self,
        rf: &'a RepoRef,
    ) -> Pin<Box<dyn Future<Output = Result<RepoMetadata, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let page = self.open(&self.root_url(rf)).await?;
            let mut branch_link = None;
            let mut any_branch = None;
            let mut description = None;
            for id in page.document.descendants(page.document.document_node()) {
                let Some(element) = page.document.element(id) else {
                    continue;
                };
                if element.name.eq_ignore_ascii_case("meta") {
                    let is_description = element
                        .get_attribute("name")
                        .is_some_and(|value| value.eq_ignore_ascii_case("description"))
                        || element
                            .get_attribute("property")
                            .is_some_and(|value| value.eq_ignore_ascii_case("og:description"));
                    if is_description && description.is_none() {
                        description = element.get_attribute("content").map(str::to_owned);
                    }
                }
                if !element.name.eq_ignore_ascii_case("a") {
                    continue;
                }
                let Some(href) = element.get_attribute("href") else {
                    continue;
                };
                let Some((kind, path, reference, _)) = self.href_parts(&page, href, rf) else {
                    continue;
                };
                if kind == RepoKind::Dir && path.is_empty() && branch_link.is_none() {
                    branch_link = Some(reference.clone());
                }
                if any_branch.is_none() {
                    any_branch = Some(reference);
                }
            }
            Ok(RepoMetadata {
                default_branch: branch_link
                    .or(any_branch)
                    .unwrap_or_else(|| "main".to_owned()),
                description,
                visibility: "public".to_owned(),
                size_kb: None,
            })
        })
    }

    fn list<'a>(
        &'a self,
        rf: &'a RepoRef,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RepoListing, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let path = validate_path(path)?;
            let page = self.open(&self.tree_url(rf, &path)).await?;
            let mut entries = self.entries_from_page(&page, rf, &path);
            let cap = self.config.repo_max_tree_entries;
            let truncated = entries.len() > cap;
            entries.truncate(cap);
            Ok(RepoListing { entries, truncated })
        })
    }

    fn tree<'a>(
        &'a self,
        rf: &'a RepoRef,
        prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RepoEntry>, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let prefix = validate_path(prefix)?;
            let cap = self.config.repo_max_tree_entries;
            let mut queue = VecDeque::from([(prefix, 0usize)]);
            let mut visited_dirs = HashSet::new();
            let mut seen_paths = HashSet::new();
            let mut entries = Vec::new();
            while let Some((dir, depth)) = queue.pop_front() {
                if depth > 3 || entries.len() >= cap {
                    break;
                }
                let listing = self.list(rf, &dir).await?;
                for entry in listing.entries {
                    if !seen_paths.insert(entry.path.clone()) || entries.len() >= cap {
                        continue;
                    }
                    if entry.kind == RepoKind::Dir
                        && depth < 3
                        && visited_dirs.insert(entry.path.clone())
                    {
                        queue.push_back((entry.path.clone(), depth + 1));
                    }
                    entries.push(entry);
                }
            }
            entries.truncate(cap);
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(entries)
        })
    }

    fn read<'a>(
        &'a self,
        rf: &'a RepoRef,
        path: &'a str,
        start_line: Option<usize>,
        max_lines: Option<usize>,
    ) -> Pin<Box<dyn Future<Output = Result<RepoFile, RepoError>> + Send + 'a>> {
        Box::pin(async move {
            let path = validate_path(path)?;
            if path.is_empty() {
                return Err(RepoError::bad_request("a file path is required"));
            }
            let page = self.open(&self.blob_url(rf, &path)).await?;
            let text = code_text(&page.document).unwrap_or_else(|| visible_text(&page.document));
            Ok(file_from_bytes(
                text.as_bytes(),
                &path,
                start_line,
                max_lines,
                self.config.repo_max_file_bytes,
            ))
        })
    }
}

/// Pull numbered code rows out of a GitHub blob page. Nested matches are
/// skipped so a highlighted token inside a row does not duplicate the line.
fn code_text(doc: &Document) -> Option<String> {
    let ids = doc.descendants(doc.document_node());
    let is_code = |id: NodeId| {
        doc.element(id).is_some_and(|element| {
            if element.get_attribute("data-testid") == Some("code-cell") {
                return true;
            }
            element.get_attribute("class").is_some_and(|class| {
                class.split_ascii_whitespace().any(|token| {
                    token == "blob-code"
                        || token == "blob-code-inner"
                        || token.starts_with("blob-code-inner")
                        || token == "react-file-line"
                })
            })
        })
    };
    let matched: HashSet<NodeId> = ids.iter().copied().filter(|&id| is_code(id)).collect();
    if matched.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for id in ids {
        if !matched.contains(&id) {
            continue;
        }
        let mut ancestor = doc.parent(id);
        let mut nested = false;
        while let Some(parent) = ancestor {
            if matched.contains(&parent) {
                nested = true;
                break;
            }
            ancestor = doc.parent(parent);
        }
        if nested {
            continue;
        }
        lines.push(
            doc.text_content(id)
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
        );
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn language_hint(path: &str) -> Option<String> {
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let language = match extension.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "rb" => "ruby",
        "sh" => "shell",
        "toml" => "toml",
        "json" => "json",
        "md" => "markdown",
        "html" => "html",
        "css" => "css",
        "yaml" | "yml" => "yaml",
        _ => return None,
    };
    Some(language.to_owned())
}
