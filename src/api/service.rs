//! Shared control-plane service used by REST, WebSocket and MCP transports.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use image::codecs::png::{CompressionType, FilterType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc};

use super::hardening::{
    AuditEntry, AuditLog, RateLimiter, TokenInfo, TokenRegistry, constant_time_eq, normalize_agent,
};
use crate::browser::{Browser, Page, TabId};
use crate::engine::dom::{Document, NodeData, NodeId};
use crate::engine::layout::Rect;
use crate::intelligence::visual::compare_images;
use crate::intelligence::{
    BrowserRepoBackend, ElementLocator, ElementMatch, GitHubApiBackend, GlyphOcr, NoOcr,
    OcrBackend, RepoBackend, RepoEntry, RepoError, RepoFile, RepoKind, RepoRef, VisualIndex,
};
use crate::interaction::MouseButton;
use crate::networking::NetworkEvent;
use crate::rendering::{RenderOptions, RenderedPage, screenshots};
use crate::storage::{read_locked, write_atomic};
use crate::utils::NxError;

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub agent: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    /// Milliseconds until the client may retry (rate-limited responses only).
    pub retry_after_ms: Option<u64>,
}

impl ApiError {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            retry_after_ms: None,
        }
    }

    pub fn rate_limited(retry_after_ms: u64) -> Self {
        let mut error = Self::new(
            429,
            "rate_limited",
            "request rate limit exceeded; retry later",
        );
        error.retry_after_ms = Some(retry_after_ms.max(1));
        error
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, "bad_request", message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(401, "unauthorized", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, "forbidden", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, "not_found", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, "conflict", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "internal_error", message)
    }
}

impl From<NxError> for ApiError {
    fn from(error: NxError) -> Self {
        let status = match &error {
            NxError::InvalidUrl(_) | NxError::UnsupportedScheme(_) | NxError::InvalidHeader(_) => {
                400
            }
            NxError::Timeout(_) => 504,
            NxError::Network(_) => 502,
            NxError::Browser(_) | NxError::Render(_) | NxError::Storage(_) => 400,
            _ => 500,
        };
        Self::new(status, "browser_error", error.to_string())
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Clone, Debug, Serialize)]
pub struct ServerEvent {
    pub event: String,
    pub timestamp_ms: u64,
    pub session_id: Option<String>,
    #[serde(skip)]
    pub owner: String,
    pub data: Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ViewportOptions {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreateOptions {
    pub url: Option<String>,
    pub profile: Option<String>,
    pub viewport: Option<ViewportOptions>,
    pub user_agent: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ClickOptions {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub selector: Option<String>,
    pub button: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TypeOptions {
    pub text: String,
    pub selector: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PressOptions {
    pub key: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ScrollOptions {
    pub dx: Option<f32>,
    pub dy: Option<f32>,
    pub selector: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HoverOptions {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub selector: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ScreenshotClip {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: Option<f32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ScreenshotOptions {
    pub format: Option<String>,
    pub quality: Option<u8>,
    pub full_page: Option<bool>,
    pub element: Option<String>,
    pub clip: Option<ScreenshotClip>,
    /// `"document"` (default) or `"viewport"`; controls whether clip
    /// coordinates include the current scroll offset.
    pub clip_origin: Option<String>,
    pub png_compression: Option<String>,
    pub png_filter: Option<String>,
    /// Persist the encoded image to the configured artifacts directory and
    /// return its path/URL in the response. `artifact_name` implies `save`.
    pub save: Option<bool>,
    /// Optional safe base name (no extension, no path separators) for the
    /// persisted artifact. An auto-generated name is used when omitted.
    #[serde(alias = "artifact")]
    pub artifact_name: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SnapshotOptions {
    pub include_accessibility: Option<bool>,
    pub session_id: Option<String>,
}

/// Options for a visual screenshot plus layout-grounded index.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VisionOptions {
    /// Render the whole laid-out document instead of the current viewport.
    pub full_page: Option<bool>,
    /// `png` (default), `jpeg`/`jpg` or `webp`.
    pub format: Option<String>,
    /// JPEG quality (ignored for PNG/WebP).
    pub quality: Option<u8>,
    /// Include `elements` and `text_runs`. Defaults to `true`.
    pub include_visual: Option<bool>,
    /// Raster OCR backend used for painted (non-DOM) text: `none` (default) or `glyph`.
    pub ocr_backend: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `vision_analyze`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VisionAnalyzeOptions {
    pub full_page: Option<bool>,
    /// Optional viewport-space region; only elements intersecting it are
    /// returned in `clickable_regions`.
    pub region: Option<ScreenshotClip>,
    /// Raster OCR backend: `none` (default, no `ocr` field) or `glyph`.
    pub ocr_backend: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `vision_extract_text`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VisionTextOptions {
    /// Optional case-insensitive phrase to locate on screen.
    pub query: Option<String>,
    /// Optional viewport-space region used to filter matches and OCR text.
    pub region: Option<ScreenshotClip>,
    /// Raster OCR backend: `none` (default) or `glyph`.
    pub ocr_backend: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `vision_find_element`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VisionFindOptions {
    pub description: String,
    pub top_k: Option<usize>,
    /// Optional viewport-space region used to constrain candidates.
    pub region: Option<ScreenshotClip>,
    /// Element locator: `heuristic`/`default` (default) or another registered name.
    pub locator: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `vision_click_target`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VisionClickOptions {
    pub description: String,
    /// Actually dispatch the click at the matched viewport coordinates.
    pub execute: Option<bool>,
    /// Capture before/after and report a visual diff.
    pub verify: Option<bool>,
    pub button: Option<String>,
    /// Element locator: `heuristic`/`default` (default) or another registered name.
    pub locator: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `vision_act`: describe an element once, then locate, disambiguate,
/// scroll into view if needed, act and verify without extra round trips.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VisionActOptions {
    pub description: String,
    /// `click` (default), `hover` or `scroll_into_view`.
    pub action: Option<String>,
    /// Capture before/after and report a visual diff. Defaults to `true`.
    pub verify: Option<bool>,
    /// Per-channel pixel delta considered a change.
    pub threshold: Option<u8>,
    /// Minimum changed pixels before a region is reported.
    pub min_pixels: Option<u64>,
    /// Element locator: `heuristic`/`default` (default) or another registered name.
    pub locator: Option<String>,
    /// Maximum locate/auto-scroll attempts. Defaults to `2`.
    pub max_attempts: Option<usize>,
    pub session_id: Option<String>,
}

/// Options for `vision_compare`. Actions are performed by the caller (for
/// example the MCP tool) between two calls; this endpoint only captures.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VisionCompareOptions {
    pub threshold: Option<u8>,
    pub min_pixels: Option<u64>,
    pub save_artifacts: Option<bool>,
    pub session_id: Option<String>,
}

/// Options for `repo_open`: a GitHub URL or `owner/repo@ref:path` shorthand.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoOpenOptions {
    pub url: Option<String>,
    pub repo: Option<String>,
    /// Backend name: `github` (default) or `browser`.
    pub backend: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `repo_list`: list one directory, defaulting to the current path.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoPathOptions {
    pub path: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `repo_tree`: a capped flat tree, optionally filtered by prefix.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoTreeOptions {
    pub prefix: Option<String>,
    pub session_id: Option<String>,
}

/// Options for `repo_read`: bounded, line-numbered file content.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoReadOptions {
    pub path: Option<String>,
    pub start_line: Option<usize>,
    pub max_lines: Option<usize>,
    pub session_id: Option<String>,
}

/// Options for `repo_search`: local substring search over tree entries.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoSearchOptions {
    pub query: String,
    /// When true, return directory paths only (file entries are filtered out).
    pub paths_only: Option<bool>,
    pub session_id: Option<String>,
}

/// Options for `repo_navigate`: a relative/absolute path or URL, or a direction.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoNavigateOptions {
    pub target: String,
    /// `back` or `forward` walks the session's navigation history.
    pub direction: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct EvaluateOptions {
    pub script: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TabsCreateOptions {
    pub url: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TabsTargetOptions {
    pub tab_id: Option<u64>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SessionOptions {
    pub name: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct WaitOptions {
    pub ms: Option<u64>,
    pub selector: Option<String>,
    pub timeout_ms: Option<u64>,
    pub session_id: Option<String>,
}

pub struct SessionEntry {
    pub id: String,
    pub owner: String,
    pub profile: Option<String>,
    pub created_ms: u64,
    pub last_used_ms: AtomicU64,
    pub browser: tokio::sync::Mutex<Browser>,
    /// Creation-time settings, retained so a persisted session can be rebuilt
    /// with the same viewport and user agent.
    pub user_agent: Option<String>,
    pub viewport: Option<ViewportOptions>,
    seen_network: Mutex<u64>,
    seen_console: Mutex<usize>,
    seen_errors: Mutex<usize>,
}

impl SessionEntry {
    fn touch(&self) {
        self.last_used_ms.store(now_ms(), Ordering::SeqCst);
    }
}

/// Repository browsing position and history for one API session.
#[derive(Clone, Debug)]
struct RepoNavState {
    rf: RepoRef,
    path: String,
    history: Vec<(RepoRef, String)>,
    index: usize,
    /// Backend chosen at open time, reused by every later operation in this
    /// session so a browser-backed browse never silently switches to GitHub.
    backend: &'static str,
}

impl RepoNavState {
    fn new(rf: RepoRef, backend: &'static str) -> Self {
        let path = rf.path.clone();
        Self {
            history: vec![(rf.clone(), path.clone())],
            rf,
            path,
            index: 0,
            backend,
        }
    }

    /// Record a new location, discarding any forward history.
    fn push(&mut self, rf: RepoRef) {
        self.path = rf.path.clone();
        self.rf = rf.clone();
        self.history.truncate(self.index + 1);
        self.history.push((rf, self.path.clone()));
        self.index = self.history.len() - 1;
    }

    fn restore(&mut self) {
        let (rf, path) = self.history[self.index].clone();
        self.rf = rf;
        self.path = path;
    }

    fn undo(&mut self) -> bool {
        if self.index == 0 {
            return false;
        }
        self.index -= 1;
        self.restore();
        true
    }

    fn redo(&mut self) -> bool {
        if self.index + 1 >= self.history.len() {
            return false;
        }
        self.index += 1;
        self.restore();
        true
    }
}

pub struct ApiService {
    config: crate::config::Config,
    sessions: RwLock<HashMap<String, Arc<SessionEntry>>>,
    defaults: RwLock<HashMap<String, String>>,
    owners: Mutex<HashMap<String, String>>,
    profile_owners: Mutex<HashMap<String, String>>,
    events: broadcast::Sender<ServerEvent>,
    counter: AtomicU64,
    auth_required: bool,
    tokens: Mutex<TokenRegistry>,
    rest_limiter: RateLimiter,
    ws_limiter: RateLimiter,
    audit: AuditLog,
    mcp_sessions: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    /// Default repository content backend shared by REST, WebSocket and MCP callers.
    repo_backend: Arc<dyn RepoBackend>,
    /// Browser-UI fallback backend, selected with `backend: "browser"`.
    browser_repo_backend: Arc<dyn RepoBackend>,
    /// Per-session repository browsing history, keyed by agent and session id.
    repo_nav: Mutex<HashMap<String, RepoNavState>>,
}

/// One vision capture: the page, its rendered document, and the scroll offset
/// that produced the viewport view. Kept together so every vision endpoint
/// builds its `VisualIndex` with identical geometry.
struct VisionCapture {
    page: Page,
    rendered: RenderedPage,
    scroll: (f32, f32),
    viewport: crate::engine::css::Viewport,
    full_page: bool,
}

impl VisionCapture {
    fn index(&self) -> VisualIndex<'_> {
        VisualIndex::new(
            &self.page.document,
            &self.rendered.layout,
            &self.rendered.image,
            self.viewport,
            self.scroll,
        )
    }

    /// The image represented by this capture (viewport crop unless `full_page`).
    fn image(&self) -> image::RgbaImage {
        if self.full_page {
            self.rendered.image.clone()
        } else {
            self.rendered
                .viewport_image_at(self.scroll.0, self.scroll.1)
        }
    }
}

impl ApiService {
    pub fn new(config: crate::config::Config) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let auth_required = config.api_token.is_some() || !config.api_tokens.is_empty();
        let tokens = TokenRegistry::from_config(&config);
        let audit = AuditLog::new(config.api_audit_capacity);
        let repo_backend: Arc<dyn RepoBackend> = Arc::new(GitHubApiBackend::new(&config));
        let browser_repo_backend: Arc<dyn RepoBackend> = Arc::new(BrowserRepoBackend::new(&config));
        Arc::new(Self {
            config,
            sessions: RwLock::new(HashMap::new()),
            defaults: RwLock::new(HashMap::new()),
            owners: Mutex::new(HashMap::new()),
            profile_owners: Mutex::new(HashMap::new()),
            events,
            counter: AtomicU64::new(1),
            auth_required,
            tokens: Mutex::new(tokens),
            rest_limiter: RateLimiter::default(),
            ws_limiter: RateLimiter::default(),
            audit,
            mcp_sessions: Mutex::new(HashMap::new()),
            repo_backend,
            browser_repo_backend,
            repo_nav: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &crate::config::Config {
        &self.config
    }

    /// Whether bearer authentication is enforced (even after all tokens are
    /// revoked, a previously configured server keeps failing closed).
    pub fn auth_required(&self) -> bool {
        self.auth_required
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    pub fn authenticate(&self, token: Option<&str>, agent: Option<&str>) -> ApiResult<AuthContext> {
        let header_agent = normalize_agent(agent);
        let provided = token.map(str::trim).filter(|token| !token.is_empty());
        if let Some(token) = provided {
            if let Ok(tokens) = self.tokens.lock()
                && let Some(mapped) = tokens.lookup(token)
            {
                return Ok(AuthContext { agent: mapped });
            }
            if let Some(expected) = self.config.api_token.as_deref()
                && constant_time_eq(token.as_bytes(), expected.as_bytes())
            {
                // Legacy single-token mode keeps honouring the agent header.
                return Ok(AuthContext {
                    agent: header_agent,
                });
            }
        }
        if !self.auth_required {
            return Ok(AuthContext {
                agent: header_agent,
            });
        }
        Err(ApiError::unauthorized("missing or invalid bearer token"))
    }

    /// Best-effort agent identity for rate limiting and audit entries. Never
    /// fails so unauthenticated traffic is still attributed and throttled.
    pub fn identify_agent(&self, token: Option<&str>, agent: Option<&str>) -> String {
        if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty())
            && let Ok(tokens) = self.tokens.lock()
            && let Some(mapped) = tokens.lookup(token)
        {
            return mapped;
        }
        normalize_agent(agent)
    }

    /// Sliding-window request budget check (REST or WebSocket commands).
    pub fn check_rate_limit(&self, key: &str, websocket: bool) -> ApiResult<()> {
        let (limit, limiter) = if websocket {
            (self.config.api_ws_rate_limit_per_minute, &self.ws_limiter)
        } else {
            (self.config.api_rate_limit_per_minute, &self.rest_limiter)
        };
        limiter
            .check(key, limit, Duration::from_secs(60))
            .map_err(ApiError::rate_limited)
    }

    pub fn record_audit(
        &self,
        agent: &str,
        method: &str,
        path: &str,
        status: u16,
        duration_ms: u64,
    ) {
        self.audit.record(agent, method, path, status, duration_ms);
    }

    pub fn audit_snapshot(&self, limit: usize) -> (Vec<AuditEntry>, usize, u64) {
        (
            self.audit.snapshot(limit),
            self.audit.capacity(),
            self.audit.total(),
        )
    }

    pub fn token_list(&self) -> Vec<TokenInfo> {
        self.tokens
            .lock()
            .map(|tokens| tokens.list())
            .unwrap_or_default()
    }

    pub fn rotate_token(&self, token_id: Option<&str>, agent: Option<&str>) -> ApiResult<Value> {
        let mut tokens = self
            .tokens
            .lock()
            .map_err(|_| ApiError::internal("token registry unavailable"))?;
        let issued = tokens.rotate(token_id, agent)?;
        // The raw secret is returned exactly once, to the caller, and never
        // recorded in logs or the audit buffer.
        Ok(json!({
            "token_id": issued.id,
            "agent": issued.agent,
            "token": issued.token,
            "created_ms": issued.created_ms,
        }))
    }

    pub fn revoke_token(&self, id: &str) -> ApiResult<Value> {
        let mut tokens = self
            .tokens
            .lock()
            .map_err(|_| ApiError::internal("token registry unavailable"))?;
        if !tokens.revoke(id) {
            return Err(ApiError::not_found(format!("token not found: {id}")));
        }
        Ok(json!({ "token_id": id, "revoked": true }))
    }

    /// Register a new SSE message channel for an MCP connection.
    pub fn open_mcp_session(&self) -> (String, mpsc::UnboundedReceiver<Value>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let id = format!(
            "mcp-{}-{}",
            now_ms(),
            self.counter.fetch_add(1, Ordering::SeqCst)
        );
        if let Ok(mut sessions) = self.mcp_sessions.lock() {
            sessions.insert(id.clone(), sender);
        }
        (id, receiver)
    }

    pub fn mcp_session_sender(&self, id: &str) -> Option<mpsc::UnboundedSender<Value>> {
        self.mcp_sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(id).cloned())
    }

    pub fn close_mcp_session(&self, id: &str) {
        if let Ok(mut sessions) = self.mcp_sessions.lock() {
            sessions.remove(id);
        }
    }

    pub fn local_auth(&self, agent: &str) -> AuthContext {
        AuthContext {
            agent: agent.to_owned(),
        }
    }

    pub fn emit(&self, event: impl Into<String>, session_id: Option<&str>, data: Value) {
        let owner = session_id
            .and_then(|id| {
                self.owners
                    .lock()
                    .ok()
                    .and_then(|owners| owners.get(id).cloned())
            })
            .unwrap_or_default();
        let _ = self.events.send(ServerEvent {
            event: event.into(),
            timestamp_ms: now_ms(),
            session_id: session_id.map(str::to_owned),
            owner,
            data,
        });
    }

    pub async fn create(&self, auth: &AuthContext, options: CreateOptions) -> ApiResult<Value> {
        let id = self.create_entry(auth, options).await?;
        Ok(json!({ "session_id": id }))
    }

    async fn create_entry(&self, auth: &AuthContext, options: CreateOptions) -> ApiResult<String> {
        self.sweep_idle().await;
        {
            let sessions = self.sessions.read().await;
            if sessions.len() >= self.config.api_max_sessions {
                return Err(ApiError::conflict(format!(
                    "session limit reached ({})",
                    self.config.api_max_sessions
                )));
            }
        }
        if let Some(profile) = options.profile.as_deref()
            && let Ok(owners) = self.profile_owners.lock()
            && let Some(owner) = owners.get(profile)
            && owner != &auth.agent
        {
            return Err(ApiError::forbidden(format!(
                "profile {profile:?} belongs to a different agent"
            )));
        }
        let user_agent = options.user_agent.clone();
        let viewport = options.viewport.clone();
        let mut config = self.config.clone();
        if let Some(user_agent) = options.user_agent {
            config.user_agent = user_agent;
        }
        let mut browser = match options.profile.as_deref() {
            Some(profile) => Browser::with_profile(config, profile)?,
            None => Browser::new(config),
        };
        if let Some(viewport) = &viewport
            && viewport.width > 0
            && viewport.height > 0
        {
            browser.set_viewport(viewport.width, viewport.height)?;
        }
        if let Some(url) = options.url.as_deref() {
            browser.open_tab(url).await?;
        } else {
            browser.new_blank_tab()?;
        }
        let id = format!(
            "sess-{}-{}",
            now_ms(),
            self.counter.fetch_add(1, Ordering::SeqCst)
        );
        let entry = Arc::new(SessionEntry {
            id: id.clone(),
            owner: auth.agent.clone(),
            profile: options.profile,
            created_ms: now_ms(),
            last_used_ms: AtomicU64::new(now_ms()),
            browser: tokio::sync::Mutex::new(browser),
            user_agent,
            viewport,
            seen_network: Mutex::new(0),
            seen_console: Mutex::new(0),
            seen_errors: Mutex::new(0),
        });
        self.sessions
            .write()
            .await
            .insert(id.clone(), entry.clone());
        if let Ok(mut owners) = self.owners.lock() {
            owners.insert(id.clone(), auth.agent.clone());
        }
        if let Some(profile) = entry.profile.as_deref()
            && let Ok(mut owners) = self.profile_owners.lock()
        {
            owners.insert(profile.to_owned(), auth.agent.clone());
        }
        self.emit("session_created", Some(&id), json!({ "agent": auth.agent }));
        {
            let browser = entry.browser.lock().await;
            self.broadcast_browser_events(&entry, &browser);
        }
        if self.config.api_session_persistence_enabled
            && let Err(error) = self.persist_session(&entry, "default").await
        {
            tracing::warn!("failed to auto-persist session {id}: {error}");
        }
        Ok(id)
    }

    pub async fn sweep_idle(&self) -> usize {
        let timeout = self.config.api_session_idle_timeout_ms;
        if timeout == 0 {
            return 0;
        }
        let cutoff = now_ms().saturating_sub(timeout);
        let idle: Vec<Arc<SessionEntry>> = self
            .sessions
            .read()
            .await
            .values()
            .filter(|entry| entry.last_used_ms.load(Ordering::SeqCst) < cutoff)
            .cloned()
            .collect();
        let mut removed = 0usize;
        for entry in idle {
            let Ok(guard) = entry.browser.try_lock() else {
                continue;
            };
            drop(guard);
            if self.sessions.write().await.remove(&entry.id).is_none() {
                continue;
            }
            self.emit(
                "session_closed",
                Some(&entry.id),
                json!({ "reason": "idle" }),
            );
            if let Ok(mut owners) = self.owners.lock() {
                owners.remove(&entry.id);
            }
            {
                let mut defaults = self.defaults.write().await;
                defaults.retain(|_, session| session != &entry.id);
            }
            if let Some(profile) = entry.profile.as_deref() {
                let still_used = self
                    .sessions
                    .read()
                    .await
                    .values()
                    .any(|session| session.profile.as_deref() == Some(profile));
                if !still_used && let Ok(mut owners) = self.profile_owners.lock() {
                    owners.remove(profile);
                }
            }
            removed += 1;
        }
        if removed > 0 {
            release_heap();
        }
        removed
    }

    pub async fn close(&self, auth: &AuthContext, id: &str) -> ApiResult<Value> {
        let entry = self.resolve(auth, Some(id)).await?;
        self.emit("session_closed", Some(&entry.id), json!({}));
        self.sessions.write().await.remove(&entry.id);
        release_heap();
        if let Ok(mut owners) = self.owners.lock() {
            owners.remove(&entry.id);
        }
        {
            let mut defaults = self.defaults.write().await;
            defaults.retain(|_, session| session != &entry.id);
        }
        if let Some(profile) = entry.profile.as_deref() {
            let still_used = self
                .sessions
                .read()
                .await
                .values()
                .any(|session| session.profile.as_deref() == Some(profile));
            if !still_used && let Ok(mut owners) = self.profile_owners.lock() {
                owners.remove(profile);
            }
        }
        // A closed session must not be resurrected on the next restart.
        self.remove_persisted_session(&entry.id);
        Ok(json!({ "session_id": entry.id, "closed": true }))
    }

    pub async fn resolve(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
    ) -> ApiResult<Arc<SessionEntry>> {
        let id = match id {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => self.ensure_default(auth).await?,
        };
        let entry = self
            .sessions
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("session not found: {id}")))?;
        if entry.owner != auth.agent {
            return Err(ApiError::forbidden("session belongs to a different agent"));
        }
        entry.touch();
        Ok(entry)
    }

    async fn ensure_default(&self, auth: &AuthContext) -> ApiResult<String> {
        if let Some(id) = self.defaults.read().await.get(&auth.agent).cloned()
            && self.sessions.read().await.contains_key(&id)
        {
            return Ok(id);
        }
        let id = self.create_entry(auth, CreateOptions::default()).await?;
        self.defaults
            .write()
            .await
            .insert(auth.agent.clone(), id.clone());
        Ok(id)
    }

    pub async fn status(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        Ok(session_status(&entry, &browser))
    }

    pub async fn navigate(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        url: &str,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        self.emit("navigation_started", Some(&entry.id), json!({ "url": url }));
        let result = if browser.tabs().is_empty() {
            browser.open_tab(url).await.map(|_| ())
        } else {
            browser.navigate(url).await.map(|_| ())
        };
        match result {
            Ok(()) => {
                let data = page_summary(&browser);
                self.emit("navigation_completed", Some(&entry.id), data.clone());
                self.emit("loading_finished", Some(&entry.id), data.clone());
                self.broadcast_browser_events(&entry, &browser);
                Ok(data)
            }
            Err(error) => {
                self.emit(
                    "navigation_failed",
                    Some(&entry.id),
                    json!({ "url": url, "error": error.to_string() }),
                );
                self.broadcast_browser_events(&entry, &browser);
                Err(error.into())
            }
        }
    }

    pub async fn back(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        self.history(auth, id, "back").await
    }

    pub async fn forward(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        self.history(auth, id, "forward").await
    }

    pub async fn reload(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        self.history(auth, id, "reload").await
    }

    async fn history(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        action: &str,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        self.emit(action, Some(&entry.id), json!({ "session_id": entry.id }));
        let result = match action {
            "back" => browser.back().await.map(|page| page.is_some()),
            "forward" => browser.forward().await.map(|page| page.is_some()),
            _ => browser.reload().await.map(|_| true),
        };
        match result {
            Ok(changed) => {
                let data = json!({
                    "changed": changed,
                    "page": page_summary(&browser),
                });
                self.emit("navigation_completed", Some(&entry.id), data.clone());
                self.broadcast_browser_events(&entry, &browser);
                Ok(data)
            }
            Err(error) => {
                self.broadcast_browser_events(&entry, &browser);
                Err(error.into())
            }
        }
    }

    pub async fn click(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: ClickOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let (x, y) = resolve_point(
            &mut browser,
            options.selector.as_deref(),
            options.x,
            options.y,
        )?;
        let button = parse_button(options.button.as_deref())?;
        let (result, navigation) = browser.click_with_navigation(x, y, button)?;
        let element = browser.element_at(x, y)?;
        let mut data = json!({
            "x": x, "y": y,
            "result": result,
            "element": element,
        });
        if let Some(navigation) = navigation {
            let target = navigation.url.clone();
            let outcome = browser.navigate_navigation(navigation).await;
            data["navigation"] = json!({
                "url": target,
                "ok": outcome.is_ok(),
                "page": outcome.as_ref().ok().map(|_| page_summary(&browser)),
                "error": outcome.as_ref().err().map(|error| error.to_string()),
            });
        }
        self.emit(
            "interaction",
            Some(&entry.id),
            json!({
                "kind": "click", "x": x, "y": y, "data": data,
            }),
        );
        self.broadcast_browser_events(&entry, &browser);
        Ok(data)
    }

    /// Resolve a CSS selector to element info (WebDriver/CDP adapters).
    pub async fn find_element(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        selector: &str,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let element = browser
            .element(selector)?
            .ok_or_else(|| ApiError::not_found(format!("no element matches {selector:?}")))?;
        serde_json::to_value(element).map_err(|error| ApiError::internal(error.to_string()))
    }

    pub async fn find_elements(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        selector: &str,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let elements = browser.elements(selector)?;
        serde_json::to_value(elements).map_err(|error| ApiError::internal(error.to_string()))
    }

    pub async fn element_text(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let text = browser
            .element_info_node(node)?
            .and_then(|element| element.text)
            .unwrap_or_default();
        Ok(json!({ "text": text }))
    }

    /// Click a node by its DOM index, following any navigation it triggers.
    pub async fn click_node(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let rect = browser
            .element_info_node(node)?
            .ok_or_else(|| ApiError::not_found(format!("no element with node id {node}")))?
            .rect;
        let (x, y) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        let (result, navigation) =
            browser.click_with_navigation(x, y, crate::interaction::MouseButton::Left)?;
        if let Some(navigation) = navigation {
            let _ = browser.navigate_navigation(navigation).await;
        }
        Ok(json!({ "clicked": result.target }))
    }

    pub async fn type_node(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
        text: &str,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let rect = browser
            .element_info_node(node)?
            .ok_or_else(|| ApiError::not_found(format!("no element with node id {node}")))?
            .rect;
        let (x, y) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        browser.click_with_button(x, y, crate::interaction::MouseButton::Left)?;
        browser.type_text(text)?;
        Ok(json!({ "typed": text.chars().count() }))
    }

    pub async fn page_source(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let snapshot = self.snapshot(auth, id, SnapshotOptions::default()).await?;
        Ok(snapshot
            .get("dom")
            .and_then(|dom| dom.get("html"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Full DOM tree as JSON (node ids, types, names, attributes, children).
    pub async fn dom_tree(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        let page = browser
            .active_page()
            .ok_or_else(|| ApiError::not_found("no active page"))?;
        Ok(dom_node_json(&page.document, page.document.document_node()))
    }

    pub async fn dom_query(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        root: usize,
        selector: &str,
        all: bool,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let nodes = browser.node_query_in(root, selector, all)?;
        Ok(json!(nodes))
    }

    pub async fn dom_outer(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let html = browser.node_outer_html(node)?;
        Ok(json!({ "html": html }))
    }

    pub async fn dom_attributes(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let attributes: Vec<Value> = browser
            .node_attributes(node)?
            .into_iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
        Ok(json!(attributes))
    }

    pub async fn dom_box(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let rect = browser
            .element_info_node(node)?
            .ok_or_else(|| ApiError::not_found(format!("no layout box for node {node}")))?
            .rect;
        Ok(json!(rect))
    }

    pub async fn element_attribute(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
        name: &str,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let value = browser
            .node_attributes(node)?
            .into_iter()
            .find(|(attribute, _)| attribute.eq_ignore_ascii_case(name))
            .map(|(_, value)| Value::String(value))
            .unwrap_or(Value::Null);
        Ok(value)
    }

    pub async fn element_state(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let info = browser
            .element_info_node(node)?
            .ok_or_else(|| ApiError::not_found(format!("no element with node id {node}")))?;
        let selected = browser
            .node_attributes(node)?
            .iter()
            .any(|(name, _)| name == "checked" || name == "selected");
        Ok(json!({
            "displayed": info.visible,
            "enabled": info.enabled,
            "selected": selected,
            "rect": info.rect,
            "tag": info.tag,
            "text": info.text,
        }))
    }

    async fn node_selector(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<String> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        browser
            .node_selector(node)?
            .ok_or_else(|| ApiError::not_found(format!("no selector path for node {node}")))
    }

    pub async fn clear_element(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let selector = self.node_selector(auth, id, node).await?;
        let selector = serde_json::to_string(&selector).unwrap_or_default();
        let script = format!(
            "(()=>{{const el=document.querySelector({selector});\
             if(!el)return false;el.value='';\
             el.dispatchEvent(new Event('input',{{bubbles:true}}));\
             el.dispatchEvent(new Event('change',{{bubbles:true}}));return true;}})()"
        );
        self.evaluate(
            auth,
            id,
            EvaluateOptions {
                script,
                session_id: None,
            },
        )
        .await
    }

    pub async fn submit_element(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let selector = self.node_selector(auth, id, node).await?;
        let selector = serde_json::to_string(&selector).unwrap_or_default();
        let script = format!(
            "(()=>{{const el=document.querySelector({selector});if(!el)return false;\
             const form=el.tagName==='FORM'?el:el.form;\
             if(!form)return false;\
             if(form.requestSubmit){{form.requestSubmit();}}else{{form.submit();}}return true;}})()"
        );
        self.evaluate(
            auth,
            id,
            EvaluateOptions {
                script,
                session_id: None,
            },
        )
        .await
    }

    /// Clear the HTTP fresh-GET cache (CDP `Network.clearBrowserCache`).
    pub async fn clear_cache(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        browser.client().clear_cache();
        Ok(Value::Null)
    }

    pub async fn element_at_point(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        x: f32,
        y: f32,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let element = browser.element_at(x, y)?;
        Ok(element.map_or(Value::Null, |element| {
            serde_json::to_value(element).unwrap_or(Value::Null)
        }))
    }

    pub async fn scroll_node(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let selector = self.node_selector(auth, id, node).await?;
        let selector = serde_json::to_string(&selector).unwrap_or_default();
        let script = format!(
            "(()=>{{const el=document.querySelector({selector});\
             if(!el)return false;el.scrollIntoView({{block:'center',inline:'center'}});return true;}})()"
        );
        self.evaluate(
            auth,
            id,
            EvaluateOptions {
                script,
                session_id: None,
            },
        )
        .await
    }

    pub async fn element_selector(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        Ok(json!(self.node_selector(auth, id, node).await?))
    }

    pub async fn focus_node(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        node: usize,
    ) -> ApiResult<Value> {
        let selector = self.node_selector(auth, id, node).await?;
        let selector = serde_json::to_string(&selector).unwrap_or_default();
        let script = format!(
            "(()=>{{const el=document.querySelector({selector});if(!el)return false;el.focus();return true;}})()"
        );
        self.evaluate(
            auth,
            id,
            EvaluateOptions {
                script,
                session_id: None,
            },
        )
        .await
    }

    pub async fn window_rect(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        let viewport = browser.viewport();
        Ok(json!({ "x": 0, "y": 0, "width": viewport.width, "height": viewport.height }))
    }

    pub async fn set_window_rect(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        width: f32,
        height: f32,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        browser.set_viewport(width.max(1.0) as u32, height.max(1.0) as u32)?;
        Ok(Value::Null)
    }

    pub async fn cookies(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let value = self
            .evaluate(
                auth,
                id,
                EvaluateOptions {
                    script: "document.cookie".to_owned(),
                    session_id: None,
                },
            )
            .await?;
        let raw = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let cookies: Vec<Value> = raw
            .split(';')
            .filter_map(|pair| pair.trim().split_once('='))
            .map(|(name, value)| {
                json!({
                    "name": name.trim(),
                    "value": value.trim(),
                    "path": "/",
                    "domain": "",
                    "secure": false,
                    "httpOnly": false,
                    "expiry": Value::Null,
                })
            })
            .collect();
        Ok(json!(cookies))
    }

    pub async fn add_cookie(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        name: &str,
        value: &str,
    ) -> ApiResult<Value> {
        let script = format!(
            "document.cookie={};",
            serde_json::to_string(&format!("{name}={value}; path=/")).unwrap_or_default()
        );
        self.evaluate(
            auth,
            id,
            EvaluateOptions {
                script,
                session_id: None,
            },
        )
        .await?;
        Ok(Value::Null)
    }

    pub async fn delete_cookies(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let script = "document.cookie.split(';').forEach(c=>{\
            const name=c.split('=')[0].trim();\
            document.cookie=name+'=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';});";
        self.evaluate(
            auth,
            id,
            EvaluateOptions {
                script: script.to_owned(),
                session_id: None,
            },
        )
        .await?;
        Ok(Value::Null)
    }

    pub async fn type_text(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: TypeOptions,
    ) -> ApiResult<Value> {
        if options.text.is_empty() {
            return Err(ApiError::bad_request("text must not be empty"));
        }
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        if let Some(selector) = options.selector.as_deref() {
            let (x, y) = resolve_point(&mut browser, Some(selector), None, None)?;
            browser.click(x, y)?;
        }
        let result = browser.type_text(&options.text)?;
        let data = json!({ "text": options.text, "result": result });
        self.emit(
            "interaction",
            Some(&entry.id),
            json!({
                "kind": "type", "data": data,
            }),
        );
        self.broadcast_browser_events(&entry, &browser);
        Ok(data)
    }

    pub async fn press(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: PressOptions,
    ) -> ApiResult<Value> {
        if options.key.is_empty() {
            return Err(ApiError::bad_request("key must not be empty"));
        }
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let result = browser.press_key(&options.key)?;
        let data = json!({ "key": options.key, "result": result });
        self.emit(
            "interaction",
            Some(&entry.id),
            json!({
                "kind": "press", "data": data,
            }),
        );
        self.broadcast_browser_events(&entry, &browser);
        Ok(data)
    }

    pub async fn scroll(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: ScrollOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let (x, y) = if let Some(selector) = options.selector.as_deref() {
            browser.scroll_element_into_view(selector)?;
            browser
                .active_tab()
                .map(|tab| (tab.scroll_x, tab.scroll_y))
                .unwrap_or((0.0, 0.0))
        } else {
            browser.scroll_by(options.dx.unwrap_or(0.0), options.dy.unwrap_or(0.0))?
        };
        let data = json!({ "scroll_x": x, "scroll_y": y });
        self.emit(
            "interaction",
            Some(&entry.id),
            json!({
                "kind": "scroll", "data": data,
            }),
        );
        Ok(data)
    }

    pub async fn hover(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: HoverOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let (x, y) = resolve_point(
            &mut browser,
            options.selector.as_deref(),
            options.x,
            options.y,
        )?;
        let result = browser.hover(x, y)?;
        let data = json!({ "x": x, "y": y, "result": result });
        self.emit(
            "interaction",
            Some(&entry.id),
            json!({
                "kind": "hover", "data": data,
            }),
        );
        self.broadcast_browser_events(&entry, &browser);
        Ok(data)
    }

    fn artifacts_dir(&self) -> PathBuf {
        self.config
            .api_artifacts_dir
            .clone()
            .unwrap_or_else(|| self.config.data_dir.join("artifacts"))
    }

    /// Persist an encoded screenshot under the artifacts directory and return
    /// its absolute path and logical URL. Path traversal is rejected and the
    /// artifact store is bounded by both per-file size and file count.
    fn save_screenshot_artifact(
        &self,
        bytes: &[u8],
        extension: &str,
        requested_name: Option<&str>,
        session_id: &str,
    ) -> ApiResult<(PathBuf, String)> {
        if bytes.len() > self.config.api_artifact_max_bytes {
            return Err(ApiError::bad_request(format!(
                "screenshot payload ({} bytes) exceeds artifact size limit ({} bytes)",
                bytes.len(),
                self.config.api_artifact_max_bytes
            )));
        }
        let dir = self.artifacts_dir();
        let mut filename = match requested_name {
            Some(name) => validate_artifact_name(name)?,
            None => format!(
                "screenshot-{}-{}-{}",
                sanitize_path_component(session_id),
                now_ms(),
                self.counter.fetch_add(1, Ordering::SeqCst)
            ),
        };
        let suffix = format!(".{extension}");
        if !filename.ends_with(&suffix) {
            filename.push_str(&suffix);
        }
        let path = dir.join(&filename);
        // Defence in depth: the joined path must be a direct child of the
        // configured artifacts directory.
        if path.parent() != Some(dir.as_path()) {
            return Err(ApiError::bad_request("invalid artifact path"));
        }
        write_atomic(&path, bytes)
            .map_err(|error| ApiError::internal(format!("write screenshot artifact: {error}")))?;
        prune_artifacts(&dir, self.config.api_artifact_max_files);
        let url = format!("/artifacts/{filename}");
        Ok((path, url))
    }

    /// Read a persisted artifact by its (validated) file name.
    pub fn read_artifact(&self, name: &str) -> ApiResult<Vec<u8>> {
        let filename = validate_artifact_name(name)?;
        let path = self.artifacts_dir().join(filename);
        std::fs::read(&path).map_err(|_| ApiError::not_found(format!("artifact not found: {name}")))
    }

    pub async fn screenshot(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: ScreenshotOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        let page = browser
            .active_page()
            .cloned()
            .ok_or_else(|| ApiError::bad_request("active tab has no page"))?;
        let viewport = browser.viewport();
        let (tab_scroll_x, tab_scroll_y) = browser
            .active_tab()
            .map(|tab| (tab.scroll_x, tab.scroll_y))
            .unwrap_or((0.0, 0.0));
        let full_page = options.full_page.unwrap_or(false);
        let clip_origin = options
            .clip_origin
            .as_deref()
            .unwrap_or("document")
            .to_ascii_lowercase();
        // Render at the scroll offset for viewport-relative captures so
        // fixed/sticky content lands where the viewport shows it.
        let render_at_origin = options.element.is_some()
            || full_page
            || (options.clip.is_some() && clip_origin != "viewport");
        let (render_scroll_x, mut render_scroll_y) = if render_at_origin {
            (0.0, 0.0)
        } else {
            (tab_scroll_x, tab_scroll_y)
        };
        let render_options = |scroll_x: f32, scroll_y: f32| RenderOptions {
            viewport,
            max_height: 20_000,
            load_images: true,
            scroll_x,
            scroll_y,
        };
        let mut rendered = browser
            .render(&page, render_options(render_scroll_x, render_scroll_y))
            .await?;
        // Clamp stale scroll positions to the scrollable range before
        // positioning fixed content or cropping.
        let max_scroll_y = (rendered.layout.document_height - viewport.height).max(0.0);
        if render_scroll_y > max_scroll_y {
            render_scroll_y = max_scroll_y;
            rendered = browser
                .render(&page, render_options(render_scroll_x, render_scroll_y))
                .await?;
        }
        let image = if let Some(selector) = options.element.as_deref() {
            let node = page
                .select(selector)
                .map_err(|error| ApiError::bad_request(error.to_string()))?
                .ok_or_else(|| ApiError::not_found(format!("no element matches {selector:?}")))?;
            rendered
                .element_image(node)
                .ok_or_else(|| ApiError::not_found("element has no rendered box".to_owned()))?
        } else if let Some(clip) = options.clip {
            let (offset_x, offset_y) = if clip_origin == "viewport" {
                (render_scroll_x, render_scroll_y)
            } else {
                (0.0, 0.0)
            };
            let rect = crate::engine::layout::Rect {
                x: clip.x + offset_x,
                y: clip.y + offset_y,
                width: clip.width,
                height: clip.height,
            };
            let cropped = screenshots::crop(&rendered.image, rect).ok_or_else(|| {
                ApiError::bad_request("clip region lies outside the rendered page")
            })?;
            match clip.scale {
                Some(scale) if scale > 0.0 && (scale - 1.0).abs() > f32::EPSILON => {
                    screenshots::resize(
                        &cropped,
                        (cropped.width() as f32 * scale).round().max(1.0) as u32,
                        (cropped.height() as f32 * scale).round().max(1.0) as u32,
                    )
                }
                _ => cropped,
            }
        } else if full_page {
            rendered.image.clone()
        } else {
            rendered.viewport_image_at(render_scroll_x, render_scroll_y)
        };
        let format = options
            .format
            .as_deref()
            .unwrap_or("png")
            .to_ascii_lowercase();
        let (format, bytes) = match format.as_str() {
            "jpeg" | "jpg" => (
                "jpeg".to_owned(),
                screenshots::jpeg_bytes(&image, options.quality.unwrap_or(90))
                    .map_err(ApiError::from)?,
            ),
            "webp" => (
                "webp".to_owned(),
                screenshots::webp_bytes(&image).map_err(ApiError::from)?,
            ),
            _ => (
                "png".to_owned(),
                screenshots::png_bytes_with(
                    &image,
                    png_compression(options.png_compression.as_deref()),
                    png_filter(options.png_filter.as_deref()),
                )
                .map_err(ApiError::from)?,
            ),
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mut data = json!({
            "format": format,
            "width": image.width(),
            "height": image.height(),
            "bytes": bytes.len(),
            "data": encoded,
        });
        // Base64 output is unchanged by default. Persisting is opt-in via
        // `save: true` or by supplying an `artifact_name`.
        if options.save.unwrap_or(false) || options.artifact_name.is_some() {
            let (path, url) = self.save_screenshot_artifact(
                &bytes,
                &format,
                options.artifact_name.as_deref(),
                &entry.id,
            )?;
            if let Some(map) = data.as_object_mut() {
                map.insert("path".to_owned(), json!(path.display().to_string()));
                map.insert("url".to_owned(), json!(url.clone()));
                map.insert("artifact".to_owned(), json!(url));
            }
        }
        self.emit(
            "screenshot",
            Some(&entry.id),
            json!({
                "format": data["format"],
                "width": data["width"],
                "height": data["height"],
            }),
        );
        Ok(data)
    }

    fn vision_enabled(&self) -> ApiResult<()> {
        if self.config.vision_enabled {
            Ok(())
        } else {
            Err(ApiError::bad_request("vision subsystem is disabled"))
        }
    }

    /// Resolve an OCR backend name to a static instance. Unknown names are a
    /// client error rather than a silent fallback to the no-op backend.
    fn vision_ocr_backend(name: Option<&str>) -> ApiResult<&'static dyn OcrBackend> {
        static NONE: NoOcr = NoOcr;
        static GLYPH: std::sync::LazyLock<GlyphOcr> = std::sync::LazyLock::new(GlyphOcr::default);
        let normalized = name
            .map(|name| name.trim().to_ascii_lowercase())
            .filter(|name| !name.is_empty());
        match normalized.as_deref().unwrap_or("none") {
            "none" => Ok(&NONE),
            "glyph" => Ok(&*GLYPH),
            other => Err(ApiError::bad_request(format!(
                "invalid ocr_backend {other:?}; valid values: none, glyph"
            ))),
        }
    }

    /// Resolve an element locator name against the intelligence registry.
    fn vision_locator(name: Option<&str>) -> ApiResult<&'static dyn ElementLocator> {
        let normalized = name
            .map(|name| name.trim().to_ascii_lowercase())
            .filter(|name| !name.is_empty());
        crate::intelligence::locator(normalized.as_deref()).ok_or_else(|| {
            ApiError::bad_request(format!(
                "invalid locator {:?}; valid values: heuristic, default",
                normalized.as_deref().unwrap_or("")
            ))
        })
    }

    /// Resolve a session and capture the current viewport (or full document)
    /// plus its layout. All vision endpoints use this so image, layout and
    /// scroll offset always describe the same frame.
    async fn vision_capture(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        full_page: bool,
    ) -> ApiResult<(Arc<SessionEntry>, VisionCapture)> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        let page = browser
            .active_page()
            .cloned()
            .ok_or_else(|| ApiError::bad_request("active tab has no page"))?;
        let viewport = browser.viewport();
        let scroll = browser
            .active_tab()
            .map(|tab| (tab.scroll_x, tab.scroll_y))
            .unwrap_or((0.0, 0.0));
        // Full-page captures render from the origin; viewport captures render
        // at the current scroll offset so fixed/sticky boxes land correctly.
        let (render_x, render_y) = if full_page { (0.0, 0.0) } else { scroll };
        let rendered = browser
            .render(
                &page,
                RenderOptions {
                    viewport,
                    scroll_x: render_x,
                    scroll_y: render_y,
                    ..RenderOptions::default()
                },
            )
            .await?;
        let capture = VisionCapture {
            page,
            rendered,
            scroll: if full_page { (0.0, 0.0) } else { scroll },
            viewport,
            full_page,
        };
        drop(browser);
        Ok((entry, capture))
    }

    /// Screenshot fields plus the layout-grounded visual index.
    pub async fn vision_snapshot(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: VisionOptions,
    ) -> ApiResult<Value> {
        self.vision_enabled()?;
        let (entry, capture) = self
            .vision_capture(auth, id, options.full_page.unwrap_or(false))
            .await?;
        let image = capture.image();
        let (format, bytes) =
            encode_vision_image(&image, options.format.as_deref(), options.quality)?;
        let mut data = json!({
            "format": format,
            "width": image.width(),
            "height": image.height(),
            "bytes": bytes.len(),
            "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
            "scroll_x": capture.scroll.0,
            "scroll_y": capture.scroll.1,
            "viewport": json!({
                "width": capture.viewport.width,
                "height": capture.viewport.height,
            }),
        });
        if options.include_visual.unwrap_or(true) {
            let index = capture.index();
            data["elements"] = vision_json(index.elements())?;
            data["text_runs"] = vision_json(index.text_runs())?;
        }
        // OCR is opt-in: the default backend adds no work and keeps existing
        // responses byte-for-byte compatible.
        let backend = Self::vision_ocr_backend(options.ocr_backend.as_deref())?;
        if backend.name() != "none" {
            let index = capture.index();
            let region = if capture.full_page {
                Some(Rect {
                    x: 0.0,
                    y: 0.0,
                    width: capture.rendered.image.width() as f32,
                    height: capture.rendered.image.height() as f32,
                })
            } else {
                None
            };
            data["ocr"] = vision_json(index.ocr_text(backend, region))?;
        }
        self.emit(
            "screenshot",
            Some(&entry.id),
            json!({
                "format": data["format"],
                "width": data["width"],
                "height": data["height"],
            }),
        );
        Ok(data)
    }

    /// Elements, text runs, clickable regions and a PNG of the current frame.
    pub async fn vision_analyze(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: VisionAnalyzeOptions,
    ) -> ApiResult<Value> {
        self.vision_enabled()?;
        let (_, capture) = self
            .vision_capture(auth, id, options.full_page.unwrap_or(false))
            .await?;
        let index = capture.index();
        let region = vision_region(options.region, capture.scroll);
        let clickable: Vec<&crate::intelligence::VisualElement> = index
            .elements()
            .iter()
            .filter(|element| {
                element.clickable
                    && region
                        .as_ref()
                        .map_or(true, |rect| vision_intersects(rect, &element.rect))
            })
            .collect();
        let mut data = json!({
            "elements": vision_json(index.elements())?,
            "text_runs": vision_json(index.text_runs())?,
            "clickable_regions": vision_json(clickable)?,
            "viewport": json!({
                "width": capture.viewport.width,
                "height": capture.viewport.height,
            }),
            "scroll": json!({ "x": capture.scroll.0, "y": capture.scroll.1 }),
            "image": encode_png_base64(&capture.image())?,
        });
        let backend = Self::vision_ocr_backend(options.ocr_backend.as_deref())?;
        if backend.name() != "none" {
            let ocr_region = if capture.full_page && region.is_none() {
                Some(Rect {
                    x: 0.0,
                    y: 0.0,
                    width: capture.rendered.image.width() as f32,
                    height: capture.rendered.image.height() as f32,
                })
            } else {
                region
            };
            data["ocr"] = vision_json(index.ocr_text(backend, ocr_region))?;
        }
        Ok(data)
    }

    /// All painted text in reading order, plus optional phrase matches.
    pub async fn vision_extract_text(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: VisionTextOptions,
    ) -> ApiResult<Value> {
        self.vision_enabled()?;
        let (_, capture) = self.vision_capture(auth, id, false).await?;
        let index = capture.index();
        let runs = index.text_runs();
        // Text runs already include preserved space fragments, so concatenate
        // first and then normalise whitespace instead of inserting separators.
        let joined = runs.iter().map(|run| run.text.as_str()).collect::<String>();
        let text = crate::intelligence::collapse_whitespace(&joined);
        let region = vision_region(options.region, capture.scroll);
        let matches = match options
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
        {
            Some(query) => index
                .find_text(query)
                .into_iter()
                .filter(|found| {
                    region
                        .as_ref()
                        .map_or(true, |rect| vision_intersects(rect, &found.rect))
                })
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        let backend = Self::vision_ocr_backend(options.ocr_backend.as_deref())?;
        let ocr = index.ocr_text(backend, region);
        Ok(json!({
            "text": text,
            "runs": vision_json(runs)?,
            "matches": vision_json(matches)?,
            "ocr": vision_json(ocr)?,
        }))
    }

    /// Rank elements by a natural-language description.
    pub async fn vision_find_element(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: VisionFindOptions,
    ) -> ApiResult<Value> {
        self.vision_enabled()?;
        let description = options.description.trim();
        if description.is_empty() {
            return Err(ApiError::bad_request("description must not be empty"));
        }
        let locator = Self::vision_locator(options.locator.as_deref())?;
        let (_, capture) = self.vision_capture(auth, id, false).await?;
        let index = capture.index();
        let top_k = options.top_k.unwrap_or(self.config.vision_max_candidates);
        let matches = index.find_with(
            locator,
            description,
            top_k,
            vision_region(options.region, capture.scroll),
        );
        Ok(json!({ "matches": vision_json(matches)? }))
    }

    /// Locate the best element for a description and optionally click it.
    pub async fn vision_click_target(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: VisionClickOptions,
    ) -> ApiResult<Value> {
        self.vision_enabled()?;
        let description = options.description.trim();
        if description.is_empty() {
            return Err(ApiError::bad_request("description must not be empty"));
        }
        let locator = Self::vision_locator(options.locator.as_deref())?;
        let (entry, capture) = self.vision_capture(auth, id, false).await?;
        let index = capture.index();
        let best = index
            .find_with(
                locator,
                description,
                self.config.vision_max_candidates,
                None,
            )
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::not_found("no visual element matched the description"))?;
        let (x, y) = best.element.center;
        let mut data = json!({
            "x": x,
            "y": y,
            "node_id": best.element.node_id,
            "label": best.element.label,
            "confidence": best.element.confidence,
            "score": best.score,
            "signals": vision_json(best.signals)?,
        });
        if options.execute.unwrap_or(false) {
            let click_result = self
                .click(
                    auth,
                    Some(&entry.id),
                    ClickOptions {
                        x: Some(x),
                        y: Some(y),
                        button: options.button.clone(),
                        ..ClickOptions::default()
                    },
                )
                .await?;
            data["click_result"] = click_result;
            self.emit(
                "interaction",
                Some(&entry.id),
                json!({ "kind": "vision_click", "x": x, "y": y }),
            );
        }
        if options.verify.unwrap_or(false) {
            let (_, after) = self.vision_capture(auth, Some(&entry.id), false).await?;
            let (text_added, text_removed) = index.text_diff(&after.index());
            let mut comparison = compare_images(
                &capture.image(),
                &after.image(),
                self.config.vision_diff_threshold,
                self.config.vision_min_diff_pixels,
            );
            comparison.text_added = text_added;
            comparison.text_removed = text_removed;
            data["verification"] = vision_json(comparison)?;
        }
        Ok(data)
    }

    /// Describe an element once: locate it, refuse ambiguous matches, scroll it
    /// into view if needed, act, and optionally verify the visual change.
    pub async fn vision_act(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: VisionActOptions,
    ) -> ApiResult<Value> {
        self.vision_enabled()?;
        let description = options.description.trim();
        if description.is_empty() {
            return Err(ApiError::bad_request("description must not be empty"));
        }
        let locator = Self::vision_locator(options.locator.as_deref())?;
        let action = options
            .action
            .as_deref()
            .map(str::trim)
            .filter(|action| !action.is_empty())
            .unwrap_or("click")
            .to_ascii_lowercase();
        if !matches!(action.as_str(), "click" | "hover" | "scroll_into_view") {
            return Err(ApiError::bad_request(format!(
                "invalid action {action:?}; valid values: click, hover, scroll_into_view"
            )));
        }
        let verify = options.verify.unwrap_or(true);
        let threshold = options
            .threshold
            .unwrap_or(self.config.vision_diff_threshold);
        let min_pixels = options
            .min_pixels
            .unwrap_or(self.config.vision_min_diff_pixels);
        let max_attempts = options.max_attempts.unwrap_or(2).max(1);
        let top_k = self.config.vision_max_candidates;
        let (entry, mut capture) = self.vision_capture(auth, id, false).await?;
        let mut best: Option<ElementMatch> = None;

        for attempt in 0..max_attempts {
            let chosen = {
                let index = capture.index();
                let candidates = index.find_with(locator, description, top_k, None);
                if let [first, second, ..] = candidates.as_slice()
                    && (first.score - second.score).abs() <= 0.05
                    && first.score > 0.5
                {
                    return Ok(json!({
                        "action": action,
                        "ambiguous": true,
                        "candidates": vision_json(&candidates)?,
                        "executed": false,
                    }));
                }
                let chosen = candidates.first().cloned().ok_or_else(|| {
                    ApiError::not_found("no visual element matched the description")
                })?;
                let (x, y) = chosen.element.center;
                if let Some(hit) = index.element_at_point(x, y)
                    && !vision_same_target(
                        &capture.page.document,
                        hit.node_id,
                        chosen.element.node_id,
                    )
                {
                    return Ok(json!({
                        "action": action,
                        "ambiguous": true,
                        "reason": "target moved",
                        "executed": false,
                    }));
                }
                chosen
            };
            let (x, y) = chosen.element.center;
            let viewport = capture.viewport;
            let offscreen = x < 0.0 || y < 0.0 || x > viewport.width || y > viewport.height;
            if offscreen && attempt + 1 < max_attempts {
                self.scroll(
                    auth,
                    Some(&entry.id),
                    ScrollOptions {
                        dx: Some(x - viewport.width / 2.0),
                        dy: Some(y - viewport.height / 2.0),
                        ..ScrollOptions::default()
                    },
                )
                .await?;
                capture = self.vision_capture(auth, Some(&entry.id), false).await?.1;
                continue;
            }
            best = Some(chosen);
            break;
        }

        let best =
            best.ok_or_else(|| ApiError::not_found("no visual element matched the description"))?;
        let target = best.element;
        let (x, y) = target.center;
        let mut data = json!({
            "action": action,
            "target": json!({
                "x": x,
                "y": y,
                "node_id": target.node_id,
                "label": target.label,
                "confidence": target.confidence,
                "score": best.score,
            }),
            "ambiguous": false,
            "executed": true,
        });
        match action.as_str() {
            "click" => {
                let result = self
                    .click(
                        auth,
                        Some(&entry.id),
                        ClickOptions {
                            x: Some(x),
                            y: Some(y),
                            ..ClickOptions::default()
                        },
                    )
                    .await?;
                data["click_result"] = result;
            }
            "hover" => {
                let result = self
                    .hover(
                        auth,
                        Some(&entry.id),
                        HoverOptions {
                            x: Some(x),
                            y: Some(y),
                            ..HoverOptions::default()
                        },
                    )
                    .await?;
                data["hover_result"] = result;
            }
            "scroll_into_view" => {
                let result = self
                    .scroll(
                        auth,
                        Some(&entry.id),
                        ScrollOptions {
                            dx: Some(x - capture.viewport.width / 2.0),
                            dy: Some(y - capture.viewport.height / 2.0),
                            ..ScrollOptions::default()
                        },
                    )
                    .await?;
                data["scroll_result"] = result;
            }
            _ => unreachable!("action was validated above"),
        }
        self.emit(
            "interaction",
            Some(&entry.id),
            json!({ "kind": "vision_act", "action": action, "x": x, "y": y }),
        );

        if verify {
            let (_, after) = self.vision_capture(auth, Some(&entry.id), false).await?;
            let mut comparison =
                compare_images(&capture.image(), &after.image(), threshold, min_pixels);
            let (text_added, text_removed) = capture.index().text_diff(&after.index());
            comparison.text_added = text_added;
            comparison.text_removed = text_removed;
            let success_hint = comparison.ratio > threshold as f32
                || !comparison.text_added.is_empty()
                || !comparison.text_removed.is_empty();
            let mut verification = vision_json(comparison)?;
            verification["success_hint"] = json!(success_hint);
            data["verification"] = verification;
        }
        Ok(data)
    }

    /// Compare two captures taken around a caller-driven action.
    pub async fn vision_compare(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: VisionCompareOptions,
    ) -> ApiResult<Value> {
        self.vision_enabled()?;
        let (entry, before) = self.vision_capture(auth, id, false).await?;
        // The action (if any) is driven by the caller between captures; yield
        // once so queued DOM work from the previous command can settle.
        tokio::task::yield_now().await;
        let (_, after) = self.vision_capture(auth, Some(&entry.id), false).await?;
        let mut comparison = compare_images(
            &before.image(),
            &after.image(),
            options
                .threshold
                .unwrap_or(self.config.vision_diff_threshold),
            options
                .min_pixels
                .unwrap_or(self.config.vision_min_diff_pixels),
        );
        let (text_added, text_removed) = before.index().text_diff(&after.index());
        comparison.text_added = text_added;
        comparison.text_removed = text_removed;
        let mut data = vision_json(comparison)?;
        if options
            .save_artifacts
            .unwrap_or(self.config.vision_save_artifacts)
        {
            let before_bytes = screenshots::png_bytes(&before.image()).map_err(ApiError::from)?;
            let after_bytes = screenshots::png_bytes(&after.image()).map_err(ApiError::from)?;
            let safe_id = sanitize_path_component(&entry.id);
            let (before_path, before_url) = self.save_screenshot_artifact(
                &before_bytes,
                "png",
                Some(&format!("vision-before-{safe_id}")),
                &entry.id,
            )?;
            let (after_path, after_url) = self.save_screenshot_artifact(
                &after_bytes,
                "png",
                Some(&format!("vision-after-{safe_id}")),
                &entry.id,
            )?;
            data["artifacts"] = json!({
                "before": { "path": before_path.display().to_string(), "url": before_url },
                "after": { "path": after_path.display().to_string(), "url": after_url },
            });
        }
        Ok(data)
    }

    // ---- Repository navigation (REST /browser/repo/*, MCP repo_*) ----------

    /// Select the backend named by the caller: `github` (default) or `browser`.
    fn repo_backend(&self, name: Option<&str>) -> ApiResult<&dyn RepoBackend> {
        match name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("github")
        {
            "github" => Ok(self.repo_backend.as_ref()),
            "browser" => Ok(self.browser_repo_backend.as_ref()),
            other => Err(ApiError::bad_request(format!(
                "unsupported repository backend {other:?}; supported: github, browser"
            ))),
        }
    }

    fn repo_error(error: RepoError) -> ApiError {
        let status = match &error {
            RepoError::BadRequest(_) => 400,
            RepoError::NotFound(_) => 404,
            RepoError::RateLimited(_) => 429,
            RepoError::Upstream(_) => 502,
        };
        let code = error.code();
        let retry_after_ms = error.retry_after_ms();
        let mut api_error = ApiError::new(status, code, error.to_string());
        api_error.retry_after_ms = retry_after_ms;
        api_error
    }

    fn repo_envelope(rf: &RepoRef, kind: &str, source: &str) -> Value {
        json!({
            "repo": format!("{}/{}", rf.owner, rf.repo),
            "ref": rf.rf,
            "path": rf.path,
            "kind": kind,
            "source": source,
        })
    }

    fn repo_merge(data: &mut Value, fields: Value) {
        if let (Some(target), Some(fields)) = (data.as_object_mut(), fields.as_object()) {
            for (key, value) in fields {
                target.insert(key.clone(), value.clone());
            }
        }
    }

    fn repo_json<T: Serialize>(value: &T) -> ApiResult<Value> {
        serde_json::to_value(value).map_err(|error| ApiError::internal(error.to_string()))
    }

    fn repo_nav_key(&self, auth: &AuthContext, id: Option<&str>) -> String {
        format!(
            "{}:{}",
            auth.agent,
            id.filter(|id| !id.is_empty()).unwrap_or("default")
        )
    }

    fn repo_nav_get(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<RepoNavState> {
        let key = self.repo_nav_key(auth, id);
        self.repo_nav
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                ApiError::not_found("no repository is open for this session; call repo_open first")
            })
    }

    fn repo_nav_set(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        state: RepoNavState,
    ) -> ApiResult<()> {
        let key = self.repo_nav_key(auth, id);
        self.repo_nav
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, state);
        Ok(())
    }

    /// Resolve a repository reference, load its metadata and show the root.
    pub async fn repo_open(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: RepoOpenOptions,
    ) -> ApiResult<Value> {
        let backend = self.repo_backend(options.backend.as_deref())?;
        let input = options
            .url
            .or(options.repo)
            .ok_or_else(|| ApiError::bad_request("url or repo is required"))?;
        let rf = RepoRef::parse(&input).map_err(Self::repo_error)?;
        let metadata = backend.metadata(&rf).await.map_err(Self::repo_error)?;
        let listing = backend
            .list(&rf, &rf.path)
            .await
            .map_err(Self::repo_error)?;
        let mut data = Self::repo_envelope(&rf, "dir", backend.name());
        Self::repo_merge(
            &mut data,
            json!({
                "metadata": Self::repo_json(&metadata)?,
                "entries": listing.entries,
            }),
        );
        self.repo_nav_set(auth, id, RepoNavState::new(rf, backend.name()))?;
        Ok(data)
    }

    /// A bounded, flat view of the repository tree, optionally filtered.
    pub async fn repo_tree(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: RepoTreeOptions,
    ) -> ApiResult<Value> {
        let nav = self.repo_nav_get(auth, id)?;
        let backend = self.repo_backend(Some(nav.backend))?;
        let prefix = options.prefix.unwrap_or_default();
        let entries = backend
            .tree(&nav.rf, &prefix)
            .await
            .map_err(Self::repo_error)?;
        let mut data = Self::repo_envelope(&nav.rf, "tree", backend.name());
        data["path"] = json!(prefix);
        Self::repo_merge(&mut data, json!({ "entries": entries, "truncated": false }));
        Ok(data)
    }

    /// List one directory (defaults to the current path) and update history.
    pub async fn repo_list(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: RepoPathOptions,
    ) -> ApiResult<Value> {
        let mut nav = self.repo_nav_get(auth, id)?;
        let backend = self.repo_backend(Some(nav.backend))?;
        let rf = match options.path.as_deref() {
            Some(path) if !path.is_empty() => nav.rf.at_path(path).map_err(Self::repo_error)?,
            _ => nav.rf.clone(),
        };
        let listing = backend
            .list(&rf, &rf.path)
            .await
            .map_err(Self::repo_error)?;
        let mut data = Self::repo_envelope(&rf, "dir", backend.name());
        Self::repo_merge(
            &mut data,
            json!({
                "entries": listing.entries,
                "truncated": listing.truncated,
            }),
        );
        nav.push(rf);
        self.repo_nav_set(auth, id, nav)?;
        Ok(data)
    }

    /// Read a bounded, line-numbered slice of a file. Directories are rejected.
    pub async fn repo_read(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: RepoReadOptions,
    ) -> ApiResult<Value> {
        let nav = self.repo_nav_get(auth, id)?;
        let backend = self.repo_backend(Some(nav.backend))?;
        let path = options.path.as_deref().unwrap_or(&nav.path);
        if path.is_empty() {
            return Err(ApiError::bad_request("a file path is required"));
        }
        let rf = nav.rf.at_path(path).map_err(Self::repo_error)?;
        // A successful non-empty listing means the caller pointed at a folder.
        if let Ok(listing) = backend.list(&rf, &rf.path).await
            && !listing.entries.is_empty()
        {
            return Err(ApiError::not_found(format!(
                "path is a directory, not a file: {}",
                rf.path
            )));
        }
        let file: RepoFile = backend
            .read(&rf, &rf.path, options.start_line, options.max_lines)
            .await
            .map_err(Self::repo_error)?;
        let mut data = Self::repo_envelope(&rf, "file", backend.name());
        Self::repo_merge(&mut data, Self::repo_json(&file)?);
        Ok(data)
    }

    /// Substring search over the repository's flat tree (names and paths).
    pub async fn repo_search(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: RepoSearchOptions,
    ) -> ApiResult<Value> {
        let nav = self.repo_nav_get(auth, id)?;
        let backend = self.repo_backend(Some(nav.backend))?;
        let query = options.query.trim();
        if query.is_empty() {
            return Err(ApiError::bad_request("query must not be empty"));
        }
        let entries = backend.tree(&nav.rf, "").await.map_err(Self::repo_error)?;
        let needle = query.to_ascii_lowercase();
        let paths_only = options.paths_only.unwrap_or(false);
        let mut matches: Vec<RepoEntry> = entries
            .into_iter()
            .filter(|entry| {
                let haystack = format!("{} {}", entry.name, entry.path).to_ascii_lowercase();
                haystack.contains(&needle) && (!paths_only || matches!(&entry.kind, RepoKind::Dir))
            })
            .collect();
        let cap = self.config.repo_max_tree_entries;
        let truncated = matches.len() >= cap;
        matches.truncate(cap);
        Ok(json!({
            "repo": format!("{}/{}", nav.rf.owner, nav.rf.repo),
            "ref": nav.rf.rf,
            "query": options.query,
            "matches": matches,
            "truncated": truncated,
            "source": backend.name(),
        }))
    }

    /// Render the current navigation position as either a directory or a file.
    async fn repo_render(&self, rf: &RepoRef, backend: &dyn RepoBackend) -> ApiResult<Value> {
        let listing = backend.list(rf, &rf.path).await.map_err(Self::repo_error)?;
        if rf.path.is_empty() || !listing.entries.is_empty() {
            let mut data = Self::repo_envelope(rf, "dir", backend.name());
            Self::repo_merge(
                &mut data,
                json!({
                    "entries": listing.entries,
                    "truncated": listing.truncated,
                }),
            );
            return Ok(data);
        }
        let file = backend
            .read(rf, &rf.path, None, None)
            .await
            .map_err(Self::repo_error)?;
        let mut data = Self::repo_envelope(rf, "file", backend.name());
        Self::repo_merge(&mut data, Self::repo_json(&file)?);
        Ok(data)
    }

    /// Follow a relative path, absolute path or URL, or walk back/forward.
    pub async fn repo_navigate(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: RepoNavigateOptions,
    ) -> ApiResult<Value> {
        let mut nav = self.repo_nav_get(auth, id)?;
        let backend = self.repo_backend(Some(nav.backend))?;
        match options
            .direction
            .as_deref()
            .map(str::trim)
            .filter(|direction| !direction.is_empty())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("back") => {
                nav.undo();
            }
            Some("forward") => {
                nav.redo();
            }
            Some(other) => {
                return Err(ApiError::bad_request(format!(
                    "invalid navigation direction {other:?}; valid values: back, forward"
                )));
            }
            None => {
                let target = options.target.trim();
                if target.is_empty() {
                    return Err(ApiError::bad_request("target must not be empty"));
                }
                let next = if target.starts_with("http://") || target.starts_with("https://") {
                    RepoRef::parse(target).map_err(Self::repo_error)?
                } else {
                    nav.rf.relative(target).map_err(Self::repo_error)?
                };
                nav.push(next);
            }
        }
        let data = self.repo_render(&nav.rf, backend).await?;
        self.repo_nav_set(auth, id, nav)?;
        Ok(data)
    }

    pub async fn snapshot(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: SnapshotOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let dom = browser.dom_snapshot()?;
        let accessibility = if options.include_accessibility.unwrap_or(true) {
            Some(browser.accessibility_tree()?)
        } else {
            None
        };
        Ok(json!({ "dom": dom, "accessibility": accessibility }))
    }

    pub async fn extract(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        serde_json::to_value(browser.extract()?)
            .map_err(|error| ApiError::internal(error.to_string()))
    }

    pub async fn evaluate(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: EvaluateOptions,
    ) -> ApiResult<Value> {
        if options.script.trim().is_empty() {
            return Err(ApiError::bad_request("script must not be empty"));
        }
        if options.script.len() > 100_000 {
            return Err(ApiError::bad_request("script exceeds 100000 characters"));
        }
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let result = browser.evaluate(&options.script)?;
        self.broadcast_browser_events(&entry, &browser);
        Ok(json!({ "result": result }))
    }

    pub async fn tabs(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        Ok(json!({ "tabs": tabs_data(&browser) }))
    }

    pub async fn tabs_create(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: TabsCreateOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let tab_id = if let Some(url) = options.url.as_deref() {
            browser.open_tab(url).await?.0
        } else {
            browser.new_blank_tab()?.0
        };
        self.emit("tab_created", Some(&entry.id), json!({ "tab_id": tab_id }));
        self.broadcast_browser_events(&entry, &browser);
        Ok(json!({ "tab_id": tab_id, "tabs": tabs_data(&browser) }))
    }

    pub async fn tabs_close(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: TabsTargetOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let tab_id = options
            .tab_id
            .ok_or_else(|| ApiError::bad_request("tab_id is required"))?;
        let closed = browser.close_tab(TabId(tab_id));
        self.emit(
            "tab_closed",
            Some(&entry.id),
            json!({ "tab_id": tab_id, "closed": closed }),
        );
        Ok(json!({ "tab_id": tab_id, "closed": closed, "tabs": tabs_data(&browser) }))
    }

    pub async fn tabs_switch(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: TabsTargetOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        let tab_id = options
            .tab_id
            .ok_or_else(|| ApiError::bad_request("tab_id is required"))?;
        let switched = browser.switch_tab(TabId(tab_id));
        self.emit(
            "tab_switched",
            Some(&entry.id),
            json!({ "tab_id": tab_id, "switched": switched }),
        );
        self.broadcast_browser_events(&entry, &browser);
        Ok(json!({
            "tab_id": tab_id,
            "switched": switched,
            "page": page_summary(&browser),
            "tabs": tabs_data(&browser),
        }))
    }

    /// Save a session's browser state and record its API identity so it can be
    /// rebuilt after a process restart.
    async fn persist_session(&self, entry: &SessionEntry, name: &str) -> ApiResult<()> {
        self.write_session_state(entry, name).await?;
        self.record_persisted_session(entry, name)
    }

    /// Write the tabs/history/storage snapshot to disk. Profile sessions rely
    /// on the profile's own session file and return `None` for the path.
    async fn write_session_state(
        &self,
        entry: &SessionEntry,
        name: &str,
    ) -> ApiResult<Option<PathBuf>> {
        let browser = entry.browser.lock().await;
        if entry.profile.is_some() {
            browser.save_profile_session()?;
            Ok(None)
        } else {
            let path = session_path(&self.config, &entry.id, Some(name))?;
            browser.save_session(&path)?;
            Ok(Some(path))
        }
    }

    /// Upsert a session's metadata in the on-disk manifest.
    fn record_persisted_session(&self, entry: &SessionEntry, name: &str) -> ApiResult<()> {
        let default_for: Vec<String> = self
            .defaults
            .try_read()
            .map(|defaults| {
                defaults
                    .iter()
                    .filter(|(_, session)| session.as_str() == entry.id)
                    .map(|(agent, _)| agent.clone())
                    .collect()
            })
            .unwrap_or_default();
        let meta = PersistedSessionMeta {
            session_id: entry.id.clone(),
            owner: entry.owner.clone(),
            profile: entry.profile.clone(),
            name: name.to_owned(),
            created_ms: entry.created_ms,
            user_agent: entry.user_agent.clone(),
            viewport: entry.viewport.clone(),
            default_for,
            saved_ms: now_ms(),
        };
        let mut manifest = read_manifest(&self.config);
        manifest
            .sessions
            .retain(|other| other.session_id != entry.id);
        manifest.sessions.push(meta);
        write_manifest(&self.config, &manifest)
    }

    fn remove_persisted_session(&self, session_id: &str) {
        let mut manifest = read_manifest(&self.config);
        let before = manifest.sessions.len();
        manifest
            .sessions
            .retain(|other| other.session_id != session_id);
        if manifest.sessions.len() != before
            && let Err(error) = write_manifest(&self.config, &manifest)
        {
            tracing::warn!("failed to update API session manifest: {error}");
        }
    }

    /// Sessions recorded on disk, for discovery after a restart.
    pub fn persisted_sessions(&self) -> ApiResult<Value> {
        let manifest = read_manifest(&self.config);
        let sessions: Vec<Value> = manifest
            .sessions
            .iter()
            .map(|meta| {
                json!({
                    "session_id": meta.session_id,
                    "owner": meta.owner,
                    "profile": meta.profile,
                    "name": meta.name,
                    "created_ms": meta.created_ms,
                    "saved_ms": meta.saved_ms,
                })
            })
            .collect();
        Ok(json!({ "sessions": sessions, "count": sessions.len() }))
    }

    /// Rebuild persisted sessions into the in-memory registry. This is the
    /// explicit load-on-restart path; `serve()` also calls it automatically
    /// when `api_session_persistence_enabled` is set.
    pub async fn restore_persisted_sessions(&self, agent: Option<&str>) -> ApiResult<Value> {
        let manifest = read_manifest(&self.config);
        let mut restored = Vec::new();
        let mut skipped = Vec::new();
        let mut failed = Vec::new();
        for meta in &manifest.sessions {
            if agent.is_some_and(|agent| meta.owner != agent) {
                continue;
            }
            if self.sessions.read().await.contains_key(&meta.session_id) {
                skipped.push(json!({ "session_id": meta.session_id, "reason": "already_active" }));
                continue;
            }
            if self.sessions.read().await.len() >= self.config.api_max_sessions {
                skipped.push(json!({ "session_id": meta.session_id, "reason": "session_limit" }));
                continue;
            }
            match self.rebuild_persisted_session(meta).await {
                Ok(entry) => {
                    let id = entry.id.clone();
                    if let Ok(mut owners) = self.owners.lock() {
                        owners.insert(id.clone(), meta.owner.clone());
                    }
                    if let Some(profile) = entry.profile.as_deref()
                        && let Ok(mut owners) = self.profile_owners.lock()
                    {
                        owners.insert(profile.to_owned(), meta.owner.clone());
                    }
                    {
                        let mut defaults = self.defaults.write().await;
                        for default_agent in &meta.default_for {
                            defaults.insert(default_agent.clone(), id.clone());
                        }
                    }
                    self.sessions.write().await.insert(id.clone(), entry);
                    self.emit(
                        "session_restored",
                        Some(&id),
                        json!({ "session_id": id, "restored": true }),
                    );
                    restored.push(id);
                }
                Err(error) => {
                    tracing::warn!(
                        "skipping unrestorable API session {}: {error}",
                        meta.session_id
                    );
                    failed.push(json!({ "session_id": meta.session_id, "error": error.message }));
                }
            }
        }
        Ok(json!({
            "restored": restored,
            "count": restored.len(),
            "skipped": skipped,
            "failed": failed,
        }))
    }

    async fn rebuild_persisted_session(
        &self,
        meta: &PersistedSessionMeta,
    ) -> ApiResult<Arc<SessionEntry>> {
        let mut config = self.config.clone();
        if let Some(user_agent) = meta.user_agent.as_deref() {
            config.user_agent = user_agent.to_owned();
        }
        let mut browser = match meta.profile.as_deref() {
            Some(profile) => Browser::with_profile(config, profile)?,
            None => Browser::new(config),
        };
        if let Some(viewport) = &meta.viewport
            && viewport.width > 0
            && viewport.height > 0
        {
            browser.set_viewport(viewport.width, viewport.height)?;
        }
        if meta.profile.is_some() {
            browser.restore_profile_session().await?;
        } else {
            let path = session_path(&self.config, &meta.session_id, Some(&meta.name))?;
            browser.restore_session(&path).await?;
        }
        let now = now_ms();
        Ok(Arc::new(SessionEntry {
            id: meta.session_id.clone(),
            owner: meta.owner.clone(),
            profile: meta.profile.clone(),
            created_ms: meta.created_ms,
            last_used_ms: AtomicU64::new(now),
            browser: tokio::sync::Mutex::new(browser),
            user_agent: meta.user_agent.clone(),
            viewport: meta.viewport.clone(),
            seen_network: Mutex::new(0),
            seen_console: Mutex::new(0),
            seen_errors: Mutex::new(0),
        }))
    }

    pub async fn session_save(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: SessionOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let name = options.name.as_deref().unwrap_or("default");
        let state_path = self.write_session_state(&entry, name).await?;
        self.record_persisted_session(&entry, name)?;
        if entry.profile.is_some() {
            return Ok(json!({
                "saved": true,
                "profile": entry.profile,
                "persisted": true,
            }));
        }
        Ok(json!({
            "saved": true,
            "path": state_path.map(|path| path.display().to_string()).unwrap_or_default(),
            "persisted": true,
        }))
    }

    pub async fn session_restore(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: SessionOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let mut browser = entry.browser.lock().await;
        if entry.profile.is_some() {
            browser.restore_profile_session().await?;
        } else {
            let path = session_path(&self.config, &entry.id, options.name.as_deref())?;
            browser.restore_session(&path).await?;
        }
        let data = json!({ "restored": true, "tabs": tabs_data(&browser) });
        self.emit("session_restored", Some(&entry.id), data.clone());
        self.broadcast_browser_events(&entry, &browser);
        Ok(data)
    }

    pub async fn network(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        let events: Vec<NetworkEvent> = browser.client().network_events();
        Ok(json!({ "events": events }))
    }

    pub async fn console(&self, auth: &AuthContext, id: Option<&str>) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        let (messages, errors) = browser
            .active_page()
            .map(|page| (page.console.clone(), page.script_errors.clone()))
            .unwrap_or_default();
        Ok(json!({ "messages": messages, "errors": errors }))
    }

    pub async fn wait(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: WaitOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        if let Some(selector) = options.selector.as_deref() {
            let timeout = options.timeout_ms.unwrap_or(5000).min(30_000);
            let deadline = std::time::Instant::now() + Duration::from_millis(timeout);
            loop {
                let found = {
                    let mut browser = entry.browser.lock().await;
                    browser.element(selector)?.is_some()
                };
                if found {
                    return Ok(json!({ "found": true, "selector": selector }));
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(json!({ "found": false, "selector": selector }));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let ms = options.ms.unwrap_or(0).min(30_000);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        Ok(json!({ "waited_ms": ms }))
    }

    fn broadcast_browser_events(&self, entry: &Arc<SessionEntry>, browser: &Browser) {
        let events = browser.client().network_events();
        let mut last_network = entry.seen_network.lock().map(|value| *value).unwrap_or(0);
        for event in &events {
            if event.sequence <= last_network {
                continue;
            }
            last_network = last_network.max(event.sequence);
            self.emit(
                "network",
                Some(&entry.id),
                serde_json::to_value(event).unwrap_or(Value::Null),
            );
        }
        if let Ok(mut seen) = entry.seen_network.lock() {
            *seen = last_network;
        }
        let (messages, errors) = browser
            .active_page()
            .map(|page| (page.console.clone(), page.script_errors.clone()))
            .unwrap_or_default();
        let mut seen = entry.seen_console.lock().map(|value| *value).unwrap_or(0);
        if seen > messages.len() {
            seen = 0;
        }
        for message in messages.iter().skip(seen) {
            self.emit(
                "console",
                Some(&entry.id),
                json!({ "level": message.level, "text": message.text }),
            );
        }
        if let Ok(mut stored) = entry.seen_console.lock() {
            *stored = messages.len();
        }
        let mut seen_errors = entry.seen_errors.lock().map(|value| *value).unwrap_or(0);
        if seen_errors > errors.len() {
            seen_errors = 0;
        }
        for error in errors.iter().skip(seen_errors) {
            self.emit(
                "console",
                Some(&entry.id),
                json!({ "level": "error", "text": error }),
            );
        }
        if let Ok(mut stored) = entry.seen_errors.lock() {
            *stored = errors.len();
        }
    }
}

fn dom_node_json(doc: &Document, id: NodeId) -> Value {
    let node = doc.node(id);
    let (node_type, node_name, node_value, attributes) = match &node.data {
        NodeData::Document => (9, "#document".to_owned(), Value::Null, Vec::new()),
        NodeData::DocumentFragment => {
            (11, "#document-fragment".to_owned(), Value::Null, Vec::new())
        }
        NodeData::Doctype(doctype) => (10, doctype.name.clone(), Value::Null, Vec::new()),
        NodeData::Text(text) => (
            3,
            "#text".to_owned(),
            Value::String(text.clone()),
            Vec::new(),
        ),
        NodeData::Comment(text) => (
            8,
            "#comment".to_owned(),
            Value::String(text.clone()),
            Vec::new(),
        ),
        NodeData::Element(element) => {
            let attributes = element
                .attributes
                .iter()
                .flat_map(|attribute| {
                    [
                        Value::String(attribute.name.clone()),
                        Value::String(attribute.value.clone()),
                    ]
                })
                .collect::<Vec<_>>();
            (1, element.name.to_uppercase(), Value::Null, attributes)
        }
    };
    let mut object = serde_json::Map::new();
    object.insert("nodeId".to_owned(), json!(id.index()));
    object.insert("nodeType".to_owned(), json!(node_type));
    object.insert("nodeName".to_owned(), Value::String(node_name.clone()));
    if !node_value.is_null() {
        object.insert("nodeValue".to_owned(), node_value);
    }
    if node_type == 1 {
        object.insert(
            "localName".to_owned(),
            json!(match &node.data {
                NodeData::Element(element) => element.name.clone(),
                _ => String::new(),
            }),
        );
        object.insert("attributes".to_owned(), Value::Array(attributes));
    }
    let children: Vec<Value> = doc
        .children(id)
        .iter()
        .map(|&child| dom_node_json(doc, child))
        .collect();
    if !children.is_empty() {
        object.insert("children".to_owned(), Value::Array(children));
    }
    if let NodeData::Element(element) = &node.data
        && let Some(content) = element.template_contents
    {
        object.insert("templateContent".to_owned(), dom_node_json(doc, content));
    }
    Value::Object(object)
}

/// Return freed allocator arenas to the OS after sessions are dropped.
fn release_heap() {
    #[cfg(target_env = "gnu")]
    unsafe {
        let _ = libc::malloc_trim(0);
    }
}

pub fn session_status(entry: &SessionEntry, browser: &Browser) -> Value {
    let active = browser.active_tab();
    json!({
        "session_id": entry.id,
        "profile": entry.profile,
        "created_ms": entry.created_ms,
        "last_used_ms": entry.last_used_ms.load(Ordering::SeqCst),
        "viewport": {
            "width": browser.viewport().width,
            "height": browser.viewport().height,
        },
        "scroll": active.map(|tab| json!({
            "x": tab.scroll_x, "y": tab.scroll_y,
        })),
        "active_tab": active.map(|tab| tab.id.0),
        "tabs": tabs_data(browser),
        "page": page_summary(browser),
        "console_count": active.map(|tab| tab.page.as_ref().map(|page| page.console.len()).unwrap_or(0)),
        "error_count": active.map(|tab| tab.page.as_ref().map(|page| page.script_errors.len()).unwrap_or(0)),
        "memory": crate::utils::memory::usage_json(),
    })
}

pub fn page_summary(browser: &Browser) -> Value {
    let Some(page) = browser.active_page() else {
        return json!({ "url": null, "title": null });
    };
    json!({
        "url": page.url.to_string(),
        "title": page.title(),
        "status": page.status.as_u16(),
        "content_type": page.content_type,
        "body_len": page.body_len,
        "console_count": page.console.len(),
        "error_count": page.script_errors.len(),
    })
}

pub fn tabs_data(browser: &Browser) -> Vec<Value> {
    let active = browser.active_tab().map(|tab| tab.id);
    browser
        .tabs()
        .iter()
        .map(|tab| {
            json!({
                "tab_id": tab.id.0,
                "url": tab.url().map(|url| url.to_string()),
                "title": tab.title,
                "active": active == Some(tab.id),
                "can_go_back": tab.can_go_back(),
                "can_go_forward": tab.can_go_forward(),
                "scroll_x": tab.scroll_x,
                "scroll_y": tab.scroll_y,
            })
        })
        .collect()
}

fn resolve_point(
    browser: &mut Browser,
    selector: Option<&str>,
    x: Option<f32>,
    y: Option<f32>,
) -> ApiResult<(f32, f32)> {
    if let Some(selector) = selector {
        let mut element = browser
            .element(selector)?
            .ok_or_else(|| ApiError::not_found(format!("no element matches {selector:?}")))?;
        if !element.in_viewport {
            browser.scroll_element_into_view(selector)?;
            element = browser
                .element(selector)?
                .ok_or_else(|| ApiError::not_found(format!("no element matches {selector:?}")))?;
        }
        let (scroll_x, scroll_y) = browser
            .active_tab()
            .map(|tab| (tab.scroll_x, tab.scroll_y))
            .unwrap_or((0.0, 0.0));
        return Ok((
            element.rect.x + element.rect.width / 2.0 - scroll_x,
            element.rect.y + element.rect.height / 2.0 - scroll_y,
        ));
    }
    let x = x.ok_or_else(|| ApiError::bad_request("x is required without a selector"))?;
    let y = y.ok_or_else(|| ApiError::bad_request("y is required without a selector"))?;
    Ok((x, y))
}

fn parse_button(button: Option<&str>) -> ApiResult<MouseButton> {
    match button.unwrap_or("left").to_ascii_lowercase().as_str() {
        "left" => Ok(MouseButton::Left),
        "middle" => Ok(MouseButton::Middle),
        "right" => Ok(MouseButton::Right),
        other => Err(ApiError::bad_request(format!(
            "invalid mouse button: {other}"
        ))),
    }
}

/// Metadata for an API session that has been persisted to disk. Keeping this
/// registry outside the in-memory session map is what lets sessions survive a
/// process restart: the browser state file holds tabs/history/storage while
/// this record holds the API-level identity (owner, profile, defaults, ...).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedSessionMeta {
    session_id: String,
    owner: String,
    profile: Option<String>,
    name: String,
    created_ms: u64,
    user_agent: Option<String>,
    viewport: Option<ViewportOptions>,
    /// Agents for which this session was the default at save time.
    default_for: Vec<String>,
    saved_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionManifest {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    sessions: Vec<PersistedSessionMeta>,
}

fn sessions_dir(config: &crate::config::Config) -> PathBuf {
    config.data_dir.join("api-sessions")
}

fn manifest_path(config: &crate::config::Config) -> PathBuf {
    sessions_dir(config).join("manifest.json")
}

/// Read the session manifest. Missing or corrupt files are treated as an empty
/// registry so a damaged disk state never prevents the control plane starting.
fn read_manifest(config: &crate::config::Config) -> SessionManifest {
    let path = manifest_path(config);
    let bytes = match read_locked(&path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return SessionManifest::default(),
        Err(error) => {
            tracing::warn!(
                "cannot read API session manifest {}: {error}",
                path.display()
            );
            return SessionManifest::default();
        }
    };
    match serde_json::from_slice::<SessionManifest>(&bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                "ignoring corrupt API session manifest {}: {error}",
                path.display()
            );
            SessionManifest::default()
        }
    }
}

fn write_manifest(config: &crate::config::Config, manifest: &SessionManifest) -> ApiResult<()> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| ApiError::internal(format!("serialize session manifest: {error}")))?;
    write_atomic(&manifest_path(config), &bytes)
        .map_err(|error| ApiError::internal(format!("write session manifest: {error}")))
}

fn session_path(
    config: &crate::config::Config,
    session_id: &str,
    name: Option<&str>,
) -> ApiResult<std::path::PathBuf> {
    let name = name.unwrap_or("default");
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ApiError::bad_request(
            "session name must be 1-64 alphanumeric, '-' or '_' characters",
        ));
    }
    Ok(config
        .data_dir
        .join("api-sessions")
        .join(format!("{session_id}-{name}.json")))
}

/// Validate a caller-supplied artifact base name. Only a single path-free
/// component made of ASCII alphanumerics, `-`, `_` and `.` is accepted, which
/// prevents directory traversal and hidden-file creation.
pub(crate) fn validate_artifact_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 120
        || name.starts_with('.')
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ApiError::bad_request(
            "artifact name must be 1-120 alphanumeric, '-', '_' or '.' characters without path separators",
        ));
    }
    Ok(name.to_owned())
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('_');
    if sanitized.is_empty() {
        "session".to_owned()
    } else {
        sanitized.chars().take(64).collect()
    }
}

/// Keep the artifact store bounded by removing the oldest image files when the
/// configured limit is exceeded. Lock/temp files from atomic writes and
/// unrelated files are ignored.
fn prune_artifacts(dir: &Path, max_files: usize) {
    if max_files == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            let is_image = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "png" | "jpg" | "jpeg" | "webp"
                    )
                });
            if !is_image {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    if files.len() <= max_files {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    let excess = files.len() - max_files;
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn png_compression(value: Option<&str>) -> CompressionType {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("default") => CompressionType::Default,
        Some("best") => CompressionType::Best,
        Some("uncompressed") => CompressionType::Uncompressed,
        _ => CompressionType::Fast,
    }
}

fn png_filter(value: Option<&str>) -> FilterType {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("none") => FilterType::NoFilter,
        Some("up") => FilterType::Up,
        Some("avg") => FilterType::Avg,
        Some("paeth") => FilterType::Paeth,
        Some("adaptive") => FilterType::Adaptive,
        _ => FilterType::Sub,
    }
}

fn vision_json<T: Serialize>(value: T) -> ApiResult<Value> {
    serde_json::to_value(value).map_err(|error| ApiError::internal(error.to_string()))
}

/// Vision regions are viewport-relative; `VisualIndex` boxes are document-space.
fn vision_region(clip: Option<ScreenshotClip>, scroll: (f32, f32)) -> Option<Rect> {
    clip.map(|clip| Rect {
        x: clip.x + scroll.0,
        y: clip.y + scroll.1,
        width: clip.width,
        height: clip.height,
    })
}

fn vision_intersects(clip: &Rect, rect: &crate::intelligence::VisualRect) -> bool {
    clip.x < rect.x + rect.width
        && rect.x < clip.x + clip.width
        && clip.y < rect.y + rect.height
        && rect.y < clip.y + clip.height
}

/// Hit-testing returns the outermost element under a point. Wrapper elements
/// (html/body) are DOM relatives of the intended target, so only an unrelated
/// hit counts as "the target moved" before an action is dispatched.
fn vision_same_target(document: &Document, hit: usize, target: usize) -> bool {
    if hit == target {
        return true;
    }
    let hit_node = NodeId::from_index(hit);
    let target_node = NodeId::from_index(target);
    document
        .ancestors(hit_node)
        .iter()
        .any(|node| node.index() == target)
        || document
            .ancestors(target_node)
            .iter()
            .any(|node| node.index() == hit)
}

fn encode_vision_image(
    image: &image::RgbaImage,
    format: Option<&str>,
    quality: Option<u8>,
) -> ApiResult<(String, Vec<u8>)> {
    let format = format.unwrap_or("png").to_ascii_lowercase();
    match format.as_str() {
        "jpeg" | "jpg" => Ok((
            "jpeg".to_owned(),
            screenshots::jpeg_bytes(image, quality.unwrap_or(90)).map_err(ApiError::from)?,
        )),
        "webp" => Ok((
            "webp".to_owned(),
            screenshots::webp_bytes(image).map_err(ApiError::from)?,
        )),
        _ => Ok((
            "png".to_owned(),
            screenshots::png_bytes_with(image, png_compression(None), png_filter(None))
                .map_err(ApiError::from)?,
        )),
    }
}

fn encode_png_base64(image: &image::RgbaImage) -> ApiResult<String> {
    let bytes = screenshots::png_bytes(image).map_err(ApiError::from)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}
