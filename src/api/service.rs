//! Shared control-plane service used by REST, WebSocket and MCP transports.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc};

use super::hardening::{
    AuditEntry, AuditLog, RateLimiter, TokenInfo, TokenRegistry, constant_time_eq, normalize_agent,
};
use crate::browser::{Browser, TabId};
use crate::interaction::MouseButton;
use crate::networking::NetworkEvent;
use crate::rendering::RenderOptions;
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

#[derive(Clone, Debug, Default, Deserialize)]
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
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ClickOptions {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub selector: Option<String>,
    pub button: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TypeOptions {
    pub text: String,
    pub selector: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PressOptions {
    pub key: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ScrollOptions {
    pub dx: Option<f32>,
    pub dy: Option<f32>,
    pub selector: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HoverOptions {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub selector: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ScreenshotOptions {
    pub format: Option<String>,
    pub quality: Option<u8>,
    pub full_page: Option<bool>,
    pub element: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SnapshotOptions {
    pub include_accessibility: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct EvaluateOptions {
    pub script: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TabsCreateOptions {
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TabsTargetOptions {
    pub tab_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SessionOptions {
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct WaitOptions {
    pub ms: Option<u64>,
    pub selector: Option<String>,
    pub timeout_ms: Option<u64>,
}

pub struct SessionEntry {
    pub id: String,
    pub owner: String,
    pub profile: Option<String>,
    pub created_ms: u64,
    pub last_used_ms: AtomicU64,
    pub browser: tokio::sync::Mutex<Browser>,
    seen_network: Mutex<u64>,
    seen_console: Mutex<usize>,
    seen_errors: Mutex<usize>,
}

impl SessionEntry {
    fn touch(&self) {
        self.last_used_ms.store(now_ms(), Ordering::SeqCst);
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
}

impl ApiService {
    pub fn new(config: crate::config::Config) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let auth_required = config.api_token.is_some() || !config.api_tokens.is_empty();
        let tokens = TokenRegistry::from_config(&config);
        let audit = AuditLog::new(config.api_audit_capacity);
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
        let mut config = self.config.clone();
        if let Some(user_agent) = options.user_agent {
            config.user_agent = user_agent;
        }
        let mut browser = match options.profile.as_deref() {
            Some(profile) => Browser::with_profile(config, profile)?,
            None => Browser::new(config),
        };
        if let Some(viewport) = options.viewport
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
        removed
    }

    pub async fn close(&self, auth: &AuthContext, id: &str) -> ApiResult<Value> {
        let entry = self.resolve(auth, Some(id)).await?;
        self.emit("session_closed", Some(&entry.id), json!({}));
        self.sessions.write().await.remove(&entry.id);
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
        let rendered = browser
            .render(
                &page,
                RenderOptions {
                    viewport,
                    max_height: 20_000,
                    load_images: true,
                },
            )
            .await?;
        let image = if let Some(selector) = options.element.as_deref() {
            let node = page
                .select(selector)
                .map_err(|error| ApiError::bad_request(error.to_string()))?
                .ok_or_else(|| ApiError::not_found(format!("no element matches {selector:?}")))?;
            rendered
                .element_image(node)
                .ok_or_else(|| ApiError::not_found("element has no rendered box".to_owned()))?
        } else if options.full_page.unwrap_or(false) {
            rendered.image.clone()
        } else {
            let (scroll_x, scroll_y) = browser
                .active_tab()
                .map(|tab| (tab.scroll_x, tab.scroll_y))
                .unwrap_or((0.0, 0.0));
            rendered.viewport_image_at(scroll_x, scroll_y)
        };
        let format = options
            .format
            .as_deref()
            .unwrap_or("png")
            .to_ascii_lowercase();
        let (format, bytes) = if format == "jpeg" || format == "jpg" {
            (
                "jpeg".to_owned(),
                rendered
                    .jpeg_bytes(options.quality.unwrap_or(85))
                    .map_err(ApiError::from)?,
            )
        } else {
            (
                "png".to_owned(),
                rendered.png_bytes().map_err(ApiError::from)?,
            )
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data = json!({
            "format": format,
            "width": image.width(),
            "height": image.height(),
            "bytes": bytes.len(),
            "data": encoded,
        });
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

    pub async fn session_save(
        &self,
        auth: &AuthContext,
        id: Option<&str>,
        options: SessionOptions,
    ) -> ApiResult<Value> {
        let entry = self.resolve(auth, id).await?;
        let browser = entry.browser.lock().await;
        if entry.profile.is_some() {
            browser.save_profile_session()?;
            return Ok(json!({ "saved": true, "profile": entry.profile }));
        }
        let path = session_path(&self.config, &entry.id, options.name.as_deref())?;
        browser.save_session(&path)?;
        Ok(json!({ "saved": true, "path": path.display().to_string() }))
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
