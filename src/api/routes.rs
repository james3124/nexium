//! Axum REST routes for browser control.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::{FromRequest, Path, Query, Request, State};
use axum::http::header::{ACCEPT, AUTHORIZATION, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use futures_util::StreamExt;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::service::{
    ApiError, ApiResult, ApiService, AuthContext, ClickOptions, CreateOptions, EvaluateOptions,
    HoverOptions, PressOptions, ScreenshotOptions, ScrollOptions, SessionOptions, SnapshotOptions,
    TabsCreateOptions, TabsTargetOptions, TypeOptions,
};
use super::websocket;
use super::{bidi, cdp, webdriver};
use crate::mcp;

#[derive(Deserialize)]
struct NavigateBody {
    url: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AuditQuery {
    limit: Option<usize>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RotateBody {
    #[serde(alias = "id")]
    token_id: Option<String>,
    agent: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct McpMessageQuery {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

pub(crate) struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> std::result::Result<Self, Response> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(respond_error(ApiError::bad_request(rejection.body_text()))),
        }
    }
}

pub fn router(service: Arc<ApiService>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/memory", get(memory))
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/{*path}",
            get(oauth_protected_resource),
        )
        .route("/browser/create", post(create))
        .route("/browser/close", post(close))
        .route("/browser/status", get(status))
        .route("/browser/navigate", post(navigate))
        .route("/browser/back", post(back))
        .route("/browser/forward", post(forward))
        .route("/browser/reload", post(reload))
        .route("/browser/click", post(click))
        .route("/browser/type", post(type_text))
        .route("/browser/press", post(press))
        .route("/browser/scroll", post(scroll))
        .route("/browser/hover", post(hover))
        .route("/browser/screenshot", post(screenshot))
        .route("/browser/snapshot", post(snapshot))
        .route("/browser/extract", post(extract))
        .route("/browser/evaluate", post(evaluate))
        .route("/browser/tabs", get(tabs))
        .route("/browser/tabs/create", post(tabs_create))
        .route("/browser/tabs/close", post(tabs_close))
        .route("/browser/tabs/switch", post(tabs_switch))
        .route("/browser/session/save", post(session_save))
        .route("/browser/session/restore", post(session_restore))
        .route("/browser/dom", get(dom))
        .route("/browser/network", get(network))
        .route("/browser/console", get(console))
        .route("/v1/audit", get(audit))
        .route("/v1/admin/tokens", get(tokens_list))
        .route("/v1/admin/tokens/rotate", post(tokens_rotate))
        .route("/v1/admin/tokens/{id}", delete(tokens_revoke))
        .route("/ws", get(websocket::ws_handler))
        .route("/status", get(webdriver::status))
        .route(
            "/session",
            get(bidi::session_ws).post(webdriver::new_session),
        )
        .route("/session/{session_id}", delete(webdriver::delete_session))
        .route(
            "/session/{session_id}/url",
            get(webdriver::current_url).post(webdriver::navigate),
        )
        .route("/session/{session_id}/title", get(webdriver::title))
        .route("/session/{session_id}/source", get(webdriver::source))
        .route(
            "/session/{session_id}/screenshot",
            get(webdriver::screenshot),
        )
        .route(
            "/session/{session_id}/element",
            post(webdriver::find_element),
        )
        .route(
            "/session/{session_id}/elements",
            post(webdriver::find_elements),
        )
        .route(
            "/session/{session_id}/element/{element_id}/click",
            post(webdriver::element_click),
        )
        .route(
            "/session/{session_id}/element/{element_id}/text",
            get(webdriver::element_text),
        )
        .route(
            "/session/{session_id}/element/{element_id}/value",
            post(webdriver::element_send_keys),
        )
        .route(
            "/session/{session_id}/execute/sync",
            post(webdriver::execute_sync),
        )
        .route("/session/{session_id}/back", post(webdriver::back))
        .route("/session/{session_id}/forward", post(webdriver::forward))
        .route("/session/{session_id}/refresh", post(webdriver::refresh))
        .route(
            "/session/{session_id}/window",
            get(webdriver::window_handle),
        )
        .route(
            "/session/{session_id}/window/handles",
            get(webdriver::window_handles),
        )
        .route(
            "/session/{session_id}/timeouts",
            get(webdriver::get_timeouts).post(webdriver::accept_timeouts),
        )
        .route(
            "/session/{session_id}/element/{element_id}/attribute/{name}",
            get(webdriver::element_attribute),
        )
        .route(
            "/session/{session_id}/element/{element_id}/property/{name}",
            get(webdriver::element_attribute),
        )
        .route(
            "/session/{session_id}/element/{element_id}/displayed",
            get(webdriver::element_displayed),
        )
        .route(
            "/session/{session_id}/element/{element_id}/enabled",
            get(webdriver::element_enabled),
        )
        .route(
            "/session/{session_id}/element/{element_id}/selected",
            get(webdriver::element_selected),
        )
        .route(
            "/session/{session_id}/element/{element_id}/rect",
            get(webdriver::element_rect),
        )
        .route(
            "/session/{session_id}/element/{element_id}/size",
            get(webdriver::element_size),
        )
        .route(
            "/session/{session_id}/element/{element_id}/name",
            get(webdriver::element_name),
        )
        .route(
            "/session/{session_id}/element/{element_id}/clear",
            post(webdriver::element_clear),
        )
        .route(
            "/session/{session_id}/element/{element_id}/submit",
            post(webdriver::element_submit),
        )
        .route(
            "/session/{session_id}/window/rect",
            get(webdriver::window_rect).post(webdriver::set_window_rect),
        )
        .route(
            "/session/{session_id}/window",
            post(webdriver::switch_window),
        )
        .route(
            "/session/{session_id}/window/new",
            post(webdriver::new_window),
        )
        .route(
            "/session/{session_id}/cookie",
            get(webdriver::get_cookies)
                .post(webdriver::add_cookie)
                .delete(webdriver::delete_cookies),
        )
        .route(
            "/session/{session_id}/cookie/{name}",
            delete(webdriver::delete_cookies),
        )
        .route("/json/version", get(cdp::version))
        .route("/json/list", get(cdp::list))
        .route("/json", get(cdp::list))
        .route("/json/new", get(cdp::new_target).put(cdp::new_target))
        .route("/json/close/{id}", get(cdp::close_target))
        .route("/json/activate/{id}", get(cdp::activate_target))
        .route("/json/protocol", get(cdp::protocol))
        .route("/devtools/page/{id}", get(cdp::page_ws))
        .route("/devtools/browser", get(cdp::browser_ws))
        .route(
            "/mcp",
            get(mcp_stream).post(mcp_endpoint).delete(mcp_delete),
        )
        .route("/mcp/sse", get(mcp_sse))
        .route("/mcp/message", post(mcp_message))
        .layer(middleware::from_fn_with_state(service.clone(), guard))
        .layer(middleware::from_fn(cors))
        .with_state(service)
}

/// CORS for loopback origins only, so browser-based MCP inspectors on
/// `localhost`/`127.0.0.1` can call the control plane.
async fn cors(request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let allowed = origin.as_deref().is_some_and(is_loopback_origin);
    let private_network = request
        .headers()
        .get("access-control-request-private-network")
        .and_then(|value| value.to_str().ok())
        == Some("true");
    if allowed && request.method() == axum::http::Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        add_cors(
            response.headers_mut(),
            origin.as_deref().unwrap_or("*"),
            private_network,
        );
        return response;
    }
    let mut response = next.run(request).await;
    if allowed {
        add_cors(
            response.headers_mut(),
            origin.as_deref().unwrap_or("*"),
            private_network,
        );
    }
    response
}

fn is_loopback_origin(origin: &str) -> bool {
    url::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
}

fn add_cors(headers: &mut HeaderMap, origin: &str, private_network: bool) {
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert("access-control-allow-origin", value);
        headers.insert("vary", HeaderValue::from_static("Origin"));
    }
    headers.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
    );
    headers.insert(
        "access-control-allow-headers",
        HeaderValue::from_static(
            "authorization, content-type, mcp-protocol-version, mcp-session-id, \
             last-event-id, x-nexium-agent, x-nexium-session",
        ),
    );
    headers.insert(
        "access-control-expose-headers",
        HeaderValue::from_static("mcp-session-id"),
    );
    headers.insert("access-control-max-age", HeaderValue::from_static("600"));
    if private_network {
        headers.insert(
            "access-control-allow-private-network",
            HeaderValue::from_static("true"),
        );
    }
}

async fn oauth_protected_resource(headers: HeaderMap) -> Response {
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1:9222");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| {
            if host.ends_with(":9443") {
                "https"
            } else {
                "http"
            }
        });
    Json(json!({
        "resource": format!("{scheme}://{host}"),
        "bearer_methods_supported": ["header"],
        "scopes_supported": [],
    }))
    .into_response()
}

/// Global guard: sliding-window rate limiting plus one audit entry per REST
/// request. Authentication itself stays in the handlers so `/health` remains
/// reachable; the guard only attributes traffic for limiting/auditing.
async fn guard(State(service): State<Arc<ApiService>>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let (token, agent, session) = credentials(request.headers());
    let agent = service.identify_agent(token, agent);
    let key = match session {
        Some(session) => format!("{agent}/{session}"),
        None => agent.clone(),
    };
    let response = match service.check_rate_limit(&key, false) {
        Ok(()) => next.run(request).await,
        Err(error) => respond(Err(error)),
    };
    let status = response.status().as_u16();
    service.record_audit(
        &agent,
        &method,
        &path,
        status,
        started.elapsed().as_millis() as u64,
    );
    response
}

async fn root() -> Response {
    Json(json!({
        "ok": true,
        "data": {
            "name": "nexium",
            "version": crate::VERSION,
            "health": "/health",
            "rest": "/browser/*",
            "mcp": "POST /mcp (JSON-RPC)",
            "mcp_stream": "GET /mcp with Accept: text/event-stream",
            "mcp_sse": "GET /mcp/sse + POST /mcp/message?sessionId=...",
            "websocket": "/ws",
        }
    }))
    .into_response()
}

async fn health(State(service): State<Arc<ApiService>>) -> Response {
    Json(json!({
        "ok": true,
        "data": {
            "name": "nexium",
            "version": crate::VERSION,
            "api": true,
            "auth_required": service.auth_required(),
            "memory": crate::utils::memory::usage_json(),
        }
    }))
    .into_response()
}

async fn memory() -> Response {
    Json(json!({
        "ok": true,
        "data": crate::utils::memory::usage_json(),
    }))
    .into_response()
}

async fn create(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<CreateOptions>,
) -> Response {
    let (auth, _) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.create(&auth, options).await)
}

async fn close(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    let id = match session {
        Some(id) => id,
        None => match service.resolve(&auth, None).await {
            Ok(entry) => entry.id.clone(),
            Err(error) => return respond(Err(error)),
        },
    };
    respond(service.close(&auth, &id).await)
}

async fn status(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.status(&auth, session.as_deref()).await)
}

async fn navigate(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<NavigateBody>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.navigate(&auth, session.as_deref(), &body.url).await)
}

async fn back(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.back(&auth, session.as_deref()).await)
}

async fn forward(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.forward(&auth, session.as_deref()).await)
}

async fn reload(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.reload(&auth, session.as_deref()).await)
}

async fn click(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<ClickOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.click(&auth, session.as_deref(), options).await)
}

async fn type_text(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<TypeOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.type_text(&auth, session.as_deref(), options).await)
}

async fn press(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<PressOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.press(&auth, session.as_deref(), options).await)
}

async fn scroll(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<ScrollOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.scroll(&auth, session.as_deref(), options).await)
}

async fn hover(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<HoverOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.hover(&auth, session.as_deref(), options).await)
}

async fn screenshot(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<ScreenshotOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.screenshot(&auth, session.as_deref(), options).await)
}

async fn snapshot(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<SnapshotOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.snapshot(&auth, session.as_deref(), options).await)
}

async fn extract(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.extract(&auth, session.as_deref()).await)
}

async fn evaluate(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<EvaluateOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.evaluate(&auth, session.as_deref(), options).await)
}

async fn tabs(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.tabs(&auth, session.as_deref()).await)
}

async fn tabs_create(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<TabsCreateOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(
        service
            .tabs_create(&auth, session.as_deref(), options)
            .await,
    )
}

async fn tabs_close(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<TabsTargetOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.tabs_close(&auth, session.as_deref(), options).await)
}

async fn tabs_switch(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<TabsTargetOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(
        service
            .tabs_switch(&auth, session.as_deref(), options)
            .await,
    )
}

async fn session_save(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<SessionOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(
        service
            .session_save(&auth, session.as_deref(), options)
            .await,
    )
}

async fn session_restore(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(options): ApiJson<SessionOptions>,
) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(
        service
            .session_restore(&auth, session.as_deref(), options)
            .await,
    )
}

async fn dom(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.dom_tree(&auth, session.as_deref()).await)
}

async fn network(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.network(&auth, session.as_deref()).await)
}

async fn console(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let (auth, session) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    respond(service.console(&auth, session.as_deref()).await)
}

const MCP_SESSION_HEADER: &str = "mcp-session-id";

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

async fn mcp_endpoint(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let (auth, _) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    let session = header_str(&headers, MCP_SESSION_HEADER).map(str::to_owned);
    let mut response = match mcp::handle_request(&service, &auth, &body).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    };
    if let Some(session) = session
        && let Ok(value) = HeaderValue::from_str(&session)
    {
        response.headers_mut().insert(MCP_SESSION_HEADER, value);
    }
    response
}

/// Streamable HTTP transport: the client opens `GET /mcp` with
/// `Accept: text/event-stream` for server-to-client messages.
async fn mcp_stream(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let accept = headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !accept.contains("text/event-stream") {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({
                "ok": false,
                "error": {
                    "code": "method_not_allowed",
                    "message": "MCP Streamable HTTP stream requires Accept: text/event-stream; send JSON-RPC with POST /mcp"
                }
            })),
        )
            .into_response();
    }
    if let Err(error) = auth_context(&service, &headers) {
        return respond(Err(error));
    }
    let (session_id, receiver) = service.open_mcp_session();
    let guard = McpSessionGuard {
        service: Arc::clone(&service),
        id: session_id.clone(),
    };
    let messages =
        futures_util::stream::unfold((receiver, guard), |(mut receiver, guard)| async move {
            let value = receiver.recv().await?;
            let data = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
            Some((
                Ok::<Event, Infallible>(Event::default().event("message").data(data)),
                (receiver, guard),
            ))
        });
    let mut response = Sse::new(messages)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&session_id) {
        response.headers_mut().insert(MCP_SESSION_HEADER, value);
    }
    response
}

async fn mcp_delete(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    if let Err(error) = auth_context(&service, &headers) {
        return respond(Err(error));
    }
    if let Some(session) = header_str(&headers, MCP_SESSION_HEADER) {
        service.close_mcp_session(session);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn audit(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    if let Err(error) = auth_context(&service, &headers) {
        return respond(Err(error));
    }
    let limit = query.limit.unwrap_or(100).min(1000);
    let (entries, capacity, total_recorded) = service.audit_snapshot(limit);
    respond(Ok(json!({
        "entries": entries,
        "capacity": capacity,
        "total_recorded": total_recorded,
        "limit": limit,
    })))
}

async fn tokens_list(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    if let Err(error) = auth_context(&service, &headers) {
        return respond(Err(error));
    }
    respond(Ok(json!({ "tokens": service.token_list() })))
}

async fn tokens_rotate(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<RotateBody>,
) -> Response {
    if let Err(error) = auth_context(&service, &headers) {
        return respond(Err(error));
    }
    respond(service.rotate_token(body.token_id.as_deref(), body.agent.as_deref()))
}

async fn tokens_revoke(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(error) = auth_context(&service, &headers) {
        return respond(Err(error));
    }
    respond(service.revoke_token(&id))
}

/// Removes the MCP SSE session registration when the stream is dropped.
struct McpSessionGuard {
    service: Arc<ApiService>,
    id: String,
}

impl Drop for McpSessionGuard {
    fn drop(&mut self) {
        self.service.close_mcp_session(&self.id);
    }
}

async fn mcp_sse(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    if let Err(error) = auth_context(&service, &headers) {
        return respond(Err(error));
    }
    let (session_id, receiver) = service.open_mcp_session();
    let endpoint = format!("/mcp/message?sessionId={session_id}");
    let guard = McpSessionGuard {
        service: Arc::clone(&service),
        id: session_id,
    };
    let first = futures_util::stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("endpoint").data(endpoint))
    });
    let messages =
        futures_util::stream::unfold((receiver, guard), |(mut receiver, guard)| async move {
            let value = receiver.recv().await?;
            let data = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
            Some((
                Ok::<Event, Infallible>(Event::default().event("message").data(data)),
                (receiver, guard),
            ))
        });
    Sse::new(first.chain(messages))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

async fn mcp_message(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Query(query): Query<McpMessageQuery>,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let (auth, _) = match auth_context(&service, &headers) {
        Ok(value) => value,
        Err(error) => return respond(Err(error)),
    };
    let Some(session_id) = query.session_id.filter(|id| !id.is_empty()) else {
        return respond(Err(ApiError::bad_request("sessionId is required")));
    };
    let Some(sender) = service.mcp_session_sender(&session_id) else {
        return respond(Err(ApiError::not_found(format!(
            "unknown MCP session: {session_id}"
        ))));
    };
    // Process off the request path: the JSON-RPC response is delivered on the
    // SSE stream, while this endpoint answers 202 Accepted immediately.
    tokio::spawn(async move {
        if let Some(response) = mcp::handle_request(&service, &auth, &body).await {
            let _ = sender.send(response);
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({ "ok": true, "session_id": session_id })),
    )
        .into_response()
}

pub(crate) fn auth_context(
    service: &ApiService,
    headers: &HeaderMap,
) -> ApiResult<(AuthContext, Option<String>)> {
    let (token, agent, session) = credentials(headers);
    Ok((service.authenticate(token, agent)?, session))
}

fn credentials(headers: &HeaderMap) -> (Option<&str>, Option<&str>, Option<String>) {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .or_else(|| {
            headers
                .get("x-nexium-token")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
        });
    let agent = headers
        .get("x-nexium-agent")
        .and_then(|value| value.to_str().ok());
    let session = headers
        .get("x-nexium-session")
        .and_then(|value| value.to_str().ok())
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    (token, agent, session)
}

pub(crate) fn respond(result: ApiResult<Value>) -> Response {
    match result {
        Ok(data) => (StatusCode::OK, Json(json!({ "ok": true, "data": data }))).into_response(),
        Err(error) => {
            let status =
                StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut error_body = json!({
                "code": error.code,
                "message": error.message,
            });
            if let Some(retry_after_ms) = error.retry_after_ms {
                error_body["retry_after_ms"] = json!(retry_after_ms);
            }
            let mut response = (
                status,
                Json(json!({
                    "ok": false,
                    "error": error_body,
                })),
            )
                .into_response();
            if let Some(retry_after_ms) = error.retry_after_ms {
                let seconds = retry_after_ms.div_ceil(1000).max(1);
                if let Ok(value) = seconds.to_string().parse() {
                    response.headers_mut().insert(RETRY_AFTER, value);
                }
            }
            response
        }
    }
}

pub(crate) fn respond_error(error: super::service::ApiError) -> Response {
    respond(Err(error))
}
