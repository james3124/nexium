//! W3C WebDriver subset over HTTP.
//!
//! Supported commands: status, session create/delete, navigate, url, title,
//! source, screenshot, find element(s), element click/text/send keys, execute
//! sync, back/forward/refresh, window handle and timeouts (accepted, ignored).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::service::{
    ApiError, ApiService, AuthContext, CreateOptions, EvaluateOptions, ScreenshotOptions,
    TabsCreateOptions, TabsTargetOptions,
};

const AGENT: &str = "webdriver";

fn auth(service: &ApiService, headers: &HeaderMap) -> Result<AuthContext, Response> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    service
        .authenticate(token, Some(AGENT))
        .map_err(api_error_response)
}

fn ok(value: Value) -> Response {
    (StatusCode::OK, Json(json!({ "value": value }))).into_response()
}

fn err(status: StatusCode, error: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "value": { "error": error, "message": message.into(), "stacktrace": "" }
        })),
    )
        .into_response()
}

fn api_error_response(error: ApiError) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let code = match error.status {
        400 => "invalid argument",
        401 => "invalid session id",
        404 if error.message.contains("session") => "invalid session id",
        404 => "no such element",
        409 => "session not created",
        _ => "unknown error",
    };
    err(status, code, error.message)
}

fn map_value(result: Result<Value, ApiError>, extract: impl FnOnce(&Value) -> Value) -> Response {
    match result {
        Ok(value) => ok(extract(&value)),
        Err(error) => api_error_response(error),
    }
}

#[derive(Deserialize)]
pub(crate) struct UrlBody {
    url: String,
}

#[derive(Deserialize)]
pub(crate) struct FindBody {
    using: String,
    value: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct KeysBody {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    value: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(crate) struct ExecuteBody {
    script: String,
    #[serde(default)]
    #[allow(dead_code)]
    args: Vec<Value>,
}

pub(crate) async fn status() -> Response {
    ok(json!({
        "ready": true,
        "message": format!("NexiumX {}", crate::VERSION),
        "build": { "version": crate::VERSION }
    }))
}

pub(crate) async fn new_session(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let options = CreateOptions {
        url: None,
        ..Default::default()
    };
    match service.create(&auth, options).await {
        Ok(value) => {
            let id = value
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ok(json!({
                "sessionId": id,
                "capabilities": {
                    "browserName": "nexiumx",
                    "browserVersion": crate::VERSION,
                    "platformName": std::env::consts::OS,
                    "acceptInsecureCerts": true,
                    "setWindowRect": false,
                }
            }))
        }
        Err(error) => api_error_response(error),
    }
}

pub(crate) async fn delete_session(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    match service.close(&auth, &session).await {
        Ok(_) => ok(Value::Null),
        Err(error) => api_error_response(error),
    }
}

pub(crate) async fn navigate(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Json(body): Json<UrlBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(
        service.navigate(&auth, Some(&session), &body.url).await,
        |_| Value::Null,
    )
}

pub(crate) async fn current_url(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.status(&auth, Some(&session)).await, |value| {
        value.pointer("/page/url").cloned().unwrap_or(Value::Null)
    })
}

pub(crate) async fn title(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.status(&auth, Some(&session)).await, |value| {
        value.pointer("/page/title").cloned().unwrap_or(Value::Null)
    })
}

pub(crate) async fn source(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.page_source(&auth, Some(&session)).await, |value| {
        value.clone()
    })
}

pub(crate) async fn screenshot(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let options = ScreenshotOptions {
        format: Some("png".to_owned()),
        quality: None,
        full_page: Some(false),
        element: None,
    };
    map_value(
        service.screenshot(&auth, Some(&session), options).await,
        |value| value.get("data").cloned().unwrap_or(Value::Null),
    )
}

fn require_css(using: &str) -> Result<(), Response> {
    if using == "css selector" {
        Ok(())
    } else {
        Err(err(
            StatusCode::BAD_REQUEST,
            "invalid argument",
            format!("unsupported locator strategy: {using}"),
        ))
    }
}

fn element_id(value: &Value) -> Value {
    value
        .get("node")
        .and_then(Value::as_u64)
        .map(|node| Value::String(format!("e{node}")))
        .unwrap_or(Value::Null)
}

pub(crate) async fn find_element(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Json(body): Json<FindBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    if let Err(response) = require_css(&body.using) {
        return response;
    }
    map_value(
        service
            .find_element(&auth, Some(&session), &body.value)
            .await,
        element_id,
    )
}

pub(crate) async fn find_elements(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Json(body): Json<FindBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    if let Err(response) = require_css(&body.using) {
        return response;
    }
    map_value(
        service
            .find_elements(&auth, Some(&session), &body.value)
            .await,
        |value| {
            value
                .as_array()
                .map(|elements| elements.iter().map(element_id).collect())
                .unwrap_or_default()
        },
    )
}

fn parse_node(element: &str) -> Result<usize, Response> {
    element
        .strip_prefix('e')
        .unwrap_or(element)
        .parse::<usize>()
        .map_err(|_| {
            err(
                StatusCode::NOT_FOUND,
                "no such element",
                format!("invalid element id: {element}"),
            )
        })
}

pub(crate) async fn element_click(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    map_value(
        service.click_node(&auth, Some(&session), node).await,
        |_| Value::Null,
    )
}

pub(crate) async fn element_text(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    map_value(
        service.element_text(&auth, Some(&session), node).await,
        |value| value.get("text").cloned().unwrap_or(Value::Null),
    )
}

pub(crate) async fn element_send_keys(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
    Json(body): Json<KeysBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    let text = body
        .text
        .or_else(|| body.value.map(|parts| parts.concat()))
        .unwrap_or_default();
    map_value(
        service.type_node(&auth, Some(&session), node, &text).await,
        |_| Value::Null,
    )
}

fn element_field(
    result: Result<Value, ApiError>,
    field: &'static str,
    fallback: Value,
) -> Response {
    map_value(result, |value| {
        value.get(field).cloned().unwrap_or(fallback.clone())
    })
}

pub(crate) async fn element_attribute(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element, name)): Path<(String, String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    map_value(
        service
            .element_attribute(&auth, Some(&session), node, &name)
            .await,
        |value| value.clone(),
    )
}

pub(crate) async fn element_displayed(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    element_field(
        service.element_state(&auth, Some(&session), node).await,
        "displayed",
        Value::Bool(false),
    )
}

pub(crate) async fn element_enabled(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    element_field(
        service.element_state(&auth, Some(&session), node).await,
        "enabled",
        Value::Bool(false),
    )
}

pub(crate) async fn element_selected(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    element_field(
        service.element_state(&auth, Some(&session), node).await,
        "selected",
        Value::Bool(false),
    )
}

pub(crate) async fn element_rect(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    element_field(
        service.element_state(&auth, Some(&session), node).await,
        "rect",
        Value::Null,
    )
}

pub(crate) async fn element_size(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    map_value(
        service.element_state(&auth, Some(&session), node).await,
        |value| {
            let rect = value.get("rect").cloned().unwrap_or(Value::Null);
            json!({
                "width": rect.get("width").cloned().unwrap_or(Value::Null),
                "height": rect.get("height").cloned().unwrap_or(Value::Null),
            })
        },
    )
}

pub(crate) async fn element_name(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    element_field(
        service.element_state(&auth, Some(&session), node).await,
        "tag",
        Value::Null,
    )
}

pub(crate) async fn element_clear(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    map_value(
        service.clear_element(&auth, Some(&session), node).await,
        |_| Value::Null,
    )
}

pub(crate) async fn element_submit(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((session, element)): Path<(String, String)>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let node = match parse_node(&element) {
        Ok(node) => node,
        Err(response) => return response,
    };
    map_value(
        service.submit_element(&auth, Some(&session), node).await,
        |_| Value::Null,
    )
}

#[derive(Deserialize)]
pub(crate) struct WindowSwitchBody {
    handle: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct WindowNewBody {
    #[serde(default, rename = "type")]
    #[allow(dead_code)]
    window_type: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct WindowRectBody {
    width: f64,
    height: f64,
    #[serde(default)]
    #[allow(dead_code)]
    x: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    y: Option<f64>,
}

#[derive(Deserialize)]
pub(crate) struct CookieBody {
    #[serde(default)]
    cookie: Option<CookieInner>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CookieInner {
    name: String,
    value: String,
}

pub(crate) async fn window_rect(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.window_rect(&auth, Some(&session)).await, |value| {
        value.clone()
    })
}

pub(crate) async fn set_window_rect(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Json(body): Json<WindowRectBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(
        service
            .set_window_rect(&auth, Some(&session), body.width as f32, body.height as f32)
            .await,
        |_| Value::Null,
    )
}

pub(crate) async fn switch_window(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Json(body): Json<WindowSwitchBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let tab_id = body.handle.parse::<u64>().ok();
    let options = TabsTargetOptions { tab_id };
    map_value(
        service.tabs_switch(&auth, Some(&session), options).await,
        |_| Value::Null,
    )
}

pub(crate) async fn new_window(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    _body: Option<Json<WindowNewBody>>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let options = TabsCreateOptions { url: None };
    map_value(
        service.tabs_create(&auth, Some(&session), options).await,
        |value| {
            json!({
                "handle": value.get("tab_id").cloned().unwrap_or(Value::Null),
                "type": "tab",
            })
        },
    )
}

fn tab_ids(value: &Value) -> Vec<String> {
    value
        .get("tabs")
        .and_then(Value::as_array)
        .map(|tabs| {
            tabs.iter()
                .filter_map(|tab| tab.get("tab_id").and_then(Value::as_u64))
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) async fn get_cookies(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.cookies(&auth, Some(&session)).await, |value| {
        value.clone()
    })
}

pub(crate) async fn add_cookie(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Json(body): Json<CookieBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let (name, value) = match (body.cookie, body.name, body.value) {
        (Some(cookie), _, _) => (cookie.name, cookie.value),
        (None, Some(name), Some(value)) => (name, value),
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "invalid argument",
                "cookie name/value required",
            );
        }
    };
    map_value(
        service
            .add_cookie(&auth, Some(&session), &name, &value)
            .await,
        |_| Value::Null,
    )
}

pub(crate) async fn delete_cookies(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.delete_cookies(&auth, Some(&session)).await, |_| {
        Value::Null
    })
}

pub(crate) async fn get_timeouts(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(_session): Path<String>,
) -> Response {
    let Ok(_auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    ok(json!({ "implicit": 0, "pageLoad": 30000, "script": 30000 }))
}

pub(crate) async fn execute_sync(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Json(body): Json<ExecuteBody>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    let script = format!("(function(){{{}}})()", body.script);
    let options = EvaluateOptions { script };
    map_value(
        service.evaluate(&auth, Some(&session), options).await,
        |value| value.get("result").cloned().unwrap_or(Value::Null),
    )
}

pub(crate) async fn back(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.back(&auth, Some(&session)).await, |_| Value::Null)
}

pub(crate) async fn forward(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.forward(&auth, Some(&session)).await, |_| {
        Value::Null
    })
}

pub(crate) async fn refresh(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.reload(&auth, Some(&session)).await, |_| Value::Null)
}

pub(crate) async fn window_handle(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.tabs(&auth, Some(&session)).await, |value| {
        value
            .get("tabs")
            .and_then(Value::as_array)
            .and_then(|tabs| {
                tabs.iter()
                    .find(|tab| tab.get("active").and_then(Value::as_bool) == Some(true))
            })
            .and_then(|tab| tab.get("tab_id").and_then(Value::as_u64))
            .map(|id| Value::String(id.to_string()))
            .unwrap_or(Value::Null)
    })
}

pub(crate) async fn window_handles(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(session): Path<String>,
) -> Response {
    let Ok(auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    map_value(service.tabs(&auth, Some(&session)).await, |value| {
        json!(tab_ids(value))
    })
}

pub(crate) async fn accept_timeouts(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(_session): Path<String>,
) -> Response {
    let Ok(_auth) = auth(&service, &headers) else {
        return api_error_response(ApiError::unauthorized("missing bearer token"));
    };
    ok(Value::Null)
}
