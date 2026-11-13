//! Chrome DevTools Protocol subset over HTTP/WebSocket.
//!
//! Discovery: `/json/version`, `/json/list`, `/json/new`.
//! Endpoints: `/devtools/page/{id}` and `/devtools/browser`.
//! Domains: Browser, Target, Page, Runtime (core commands).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::service::{
    ApiError, ApiService, AuthContext, ClickOptions, CreateOptions, EvaluateOptions, HoverOptions,
    PressOptions, ScreenshotOptions, TypeOptions,
};

const AGENT: &str = "cdp";

static TARGETS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn host_of(headers: &HeaderMap) -> String {
    headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1:9222")
        .to_owned()
}

fn target_json(host: &str, id: &str, url: &str) -> Value {
    json!({
        "id": id,
        "type": "page",
        "title": "",
        "url": url,
        "description": "",
        "webSocketDebuggerUrl": format!("ws://{host}/devtools/page/{id}"),
        "devtoolsFrontendUrl": "",
    })
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

fn auth(
    service: &ApiService,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<AuthContext, Response> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .or(query_token);
    service
        .authenticate(token, Some(AGENT))
        .map_err(|_| unauthorized())
}

pub(crate) fn cdp_error(error: ApiError) -> (i64, String) {
    let code = match error.status {
        400 => -32602,
        404 => -32601,
        _ => -32603,
    };
    (code, error.message)
}

pub(crate) async fn version(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
) -> Json<Value> {
    let host = host_of(&headers);
    Json(json!({
        "Browser": format!("NexiumX/{}", crate::VERSION),
        "Protocol-Version": "1.3",
        "User-Agent": service.config().user_agent,
        "V8-Version": "Boa",
        "WebKit-Version": "",
        "webSocketDebuggerUrl": format!("ws://{host}/devtools/browser"),
    }))
}

pub(crate) async fn protocol() -> Json<Value> {
    Json(json!({
        "version": { "major": "1", "minor": "3" },
        "domains": [
            { "domain": "Browser", "commands": [
                {"name": "getVersion"}, {"name": "getBrowserCommandLine"},
                {"name": "close"}
            ]},
            { "domain": "Target", "commands": [
                {"name": "getTargets"}, {"name": "getTargetInfo"},
                {"name": "createTarget"}, {"name": "closeTarget"},
                {"name": "attachToTarget"}, {"name": "activateTarget"},
                {"name": "setDiscoverTargets"}, {"name": "setAutoAttach"},
                {"name": "detachFromTarget"}
            ]},
            { "domain": "Page", "commands": [
                {"name": "enable"}, {"name": "navigate"}, {"name": "reload"},
                {"name": "captureScreenshot"}, {"name": "getLayoutMetrics"},
                {"name": "getFrameTree"}, {"name": "getNavigationHistory"},
                {"name": "getCookies"}, {"name": "setCookie"},
                {"name": "deleteCookie"}, {"name": "bringToFront"}, {"name": "close"}
            ]},
            { "domain": "Runtime", "commands": [
                {"name": "enable"}, {"name": "evaluate"},
                {"name": "callFunctionOn"}, {"name": "getProperties"},
                {"name": "releaseObject"}
            ]},
            { "domain": "DOM", "commands": [
                {"name": "enable"}, {"name": "getDocument"},
                {"name": "describeNode"}, {"name": "querySelector"},
                {"name": "querySelectorAll"}, {"name": "getOuterHTML"},
                {"name": "getAttributes"}, {"name": "getBoxModel"},
                {"name": "getContentQuads"}, {"name": "getNodeForLocation"},
                {"name": "resolveNode"}, {"name": "focus"},
                {"name": "scrollIntoViewIfNeeded"}
            ]},
            { "domain": "Input", "commands": [
                {"name": "dispatchMouseEvent"}, {"name": "dispatchKeyEvent"},
                {"name": "insertText"}
            ]},
            { "domain": "Network", "commands": [
                {"name": "enable"}, {"name": "setCacheDisabled"},
                {"name": "clearBrowserCache"}
            ]},
            { "domain": "Storage", "commands": [
                {"name": "getCookies"}, {"name": "setCookies"},
                {"name": "clearDataForOrigin"}, {"name": "getUsageAndQuota"}
            ]},
            { "domain": "Log", "commands": [ {"name": "enable"} ] },
            { "domain": "Console", "commands": [ {"name": "enable"} ] },
            { "domain": "Performance", "commands": [
                {"name": "enable"}, {"name": "disable"}, {"name": "getMetrics"}
            ]},
            { "domain": "Security", "commands": [ {"name": "enable"} ] },
            { "domain": "Emulation", "commands": [
                {"name": "setDeviceMetricsOverride"},
                {"name": "clearDeviceMetricsOverride"}
            ]},
            { "domain": "Schema", "commands": [ {"name": "getDomains"} ] }
        ]
    }))
}

pub(crate) async fn close_target(
    State(service): State<Arc<ApiService>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Ok(auth) = auth(&service, &headers, query.get("token").map(String::as_str)) else {
        return unauthorized();
    };
    let _ = service.close(&auth, &id).await;
    if let Ok(mut targets) = TARGETS.lock() {
        targets.remove(&id);
    }
    (StatusCode::OK, "Target is closed").into_response()
}

pub(crate) async fn activate_target(
    State(service): State<Arc<ApiService>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if auth(&service, &headers, query.get("token").map(String::as_str)).is_err() {
        return unauthorized();
    }
    let known = TARGETS
        .lock()
        .map(|targets| targets.contains_key(&id))
        .unwrap_or(false);
    if !known {
        return (StatusCode::NOT_FOUND, "No such target").into_response();
    }
    (StatusCode::OK, "Target activated").into_response()
}

pub(crate) async fn list(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
) -> Json<Value> {
    let _ = service;
    let host = host_of(&headers);
    let targets = TARGETS
        .lock()
        .map(|targets| {
            targets
                .iter()
                .map(|(id, url)| target_json(&host, id, url))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Json(Value::Array(targets))
}

pub(crate) async fn new_target(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Ok(auth) = auth(&service, &headers, query.get("token").map(String::as_str)) else {
        return unauthorized();
    };
    let url = query.get("url").cloned().unwrap_or_default();
    let options = CreateOptions {
        url: (!url.is_empty()).then(|| url.clone()),
        ..Default::default()
    };
    match service.create(&auth, options).await {
        Ok(value) => {
            let id = value
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Ok(mut targets) = TARGETS.lock() {
                targets.insert(id.clone(), url);
            }
            Json(target_json(&host_of(&headers), &id, "")).into_response()
        }
        Err(error) => {
            let (code, message) = cdp_error(error);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": { "code": code, "message": message } })),
            )
                .into_response()
        }
    }
}

pub(crate) async fn page_ws(
    State(service): State<Arc<ApiService>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Ok(auth) = auth(&service, &headers, query.get("token").map(String::as_str)) else {
        return unauthorized();
    };
    let known = TARGETS
        .lock()
        .map(|targets| targets.contains_key(&id))
        .unwrap_or(false);
    if !known {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown target" })),
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(service, auth, id, socket))
}

pub(crate) async fn browser_ws(
    State(service): State<Arc<ApiService>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Ok(auth) = auth(&service, &headers, query.get("token").map(String::as_str)) else {
        return unauthorized();
    };
    let existing = TARGETS
        .lock()
        .ok()
        .and_then(|targets| targets.keys().next().cloned());
    let target = match existing {
        Some(id) => id,
        None => {
            let options = CreateOptions::default();
            match service.create(&auth, options).await {
                Ok(value) => {
                    let id = value
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if let Ok(mut targets) = TARGETS.lock() {
                        targets.insert(id.clone(), String::new());
                    }
                    id
                }
                Err(_) => return unauthorized(),
            }
        }
    };
    ws.on_upgrade(move |socket| handle_socket(service, auth, target, socket))
}

async fn handle_socket(
    service: Arc<ApiService>,
    auth: AuthContext,
    target: String,
    mut socket: WebSocket,
) {
    let mut seen_network = 0usize;
    let mut seen_console = 0usize;
    let mut seen_errors = 0usize;
    while let Some(message) = socket.recv().await {
        match message {
            Ok(Message::Text(text)) => {
                let request: Value = match serde_json::from_str(&text) {
                    Ok(request) => request,
                    Err(error) => {
                        let response = json!({
                            "id": Value::Null,
                            "error": { "code": -32700, "message": error.to_string() }
                        });
                        if socket
                            .send(Message::Text(response.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                };
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let session_target = request
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or(&target);
                let result = dispatch(&service, &auth, session_target, &method, &params).await;
                let response = match result {
                    Ok(value) => json!({ "id": id, "result": value }),
                    Err((code, message)) => {
                        json!({ "id": id, "error": { "code": code, "message": message } })
                    }
                };
                if socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
                if method == "Page.navigate" {
                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|duration| duration.as_secs_f64())
                        .unwrap_or(0.0);
                    let mut events = vec![json!({
                        "method": "Page.domContentEventFired",
                        "params": { "timestamp": timestamp }
                    })];
                    if let Ok(network) = service.network(&auth, Some(session_target)).await
                        && let Some(entries) = network.get("events").and_then(Value::as_array)
                    {
                        for (index, entry) in entries.iter().enumerate().skip(seen_network) {
                            let request_id = format!("req-{index}");
                            let url = entry.get("url").cloned().unwrap_or(Value::Null);
                            let request_method =
                                entry.get("method").cloned().unwrap_or_else(|| json!("GET"));
                            let status = entry.get("status").cloned().unwrap_or(json!(200));
                            events.push(json!({
                                "method": "Network.requestWillBeSent",
                                "params": {
                                    "requestId": request_id,
                                    "type": "Document",
                                    "request": { "url": url, "method": request_method },
                                }
                            }));
                            events.push(json!({
                                "method": "Network.responseReceived",
                                "params": {
                                    "requestId": request_id,
                                    "type": "Document",
                                    "response": { "url": url, "status": status },
                                }
                            }));
                            events.push(json!({
                                "method": "Network.loadingFinished",
                                "params": { "requestId": request_id, "encodedDataLength": 0 }
                            }));
                        }
                        seen_network = entries.len();
                    }
                    if let Ok(logs) = service.console(&auth, Some(session_target)).await {
                        if let Some(messages) = logs.get("messages").and_then(Value::as_array) {
                            for entry in messages.iter().skip(seen_console) {
                                let text = entry.get("text").cloned().unwrap_or_else(|| json!(""));
                                events.push(json!({
                                    "method": "Runtime.consoleAPICalled",
                                    "params": {
                                        "type": "log",
                                        "args": [{ "type": "string", "value": text }],
                                        "timestamp": timestamp,
                                    }
                                }));
                                events.push(json!({
                                    "method": "Log.entryAdded",
                                    "params": {
                                        "entry": {
                                            "level": "info",
                                            "text": text,
                                            "source": "javascript",
                                            "timestamp": timestamp,
                                        }
                                    }
                                }));
                            }
                            seen_console = messages.len();
                        }
                        if let Some(errors) = logs.get("errors").and_then(Value::as_array) {
                            for entry in errors.iter().skip(seen_errors) {
                                let text = entry.clone();
                                events.push(json!({
                                    "method": "Log.entryAdded",
                                    "params": {
                                        "entry": {
                                            "level": "error",
                                            "text": text,
                                            "source": "javascript",
                                            "timestamp": timestamp,
                                        }
                                    }
                                }));
                            }
                            seen_errors = errors.len();
                        }
                    }
                    events.push(json!({
                        "method": "Page.loadEventFired",
                        "params": { "timestamp": timestamp }
                    }));
                    for event in events {
                        if socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            Ok(Message::Ping(payload)) => {
                if socket.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

fn param_node(params: &Value) -> Result<usize, (i64, String)> {
    params
        .get("nodeId")
        .and_then(Value::as_u64)
        .map(|node| node as usize)
        .ok_or((-32602, "nodeId is required".to_owned()))
}

fn find_dom_node(tree: &Value, node: u64) -> Option<Value> {
    if tree.get("nodeId").and_then(Value::as_u64) == Some(node) {
        return Some(tree.clone());
    }
    if let Some(children) = tree.get("children").and_then(Value::as_array) {
        for child in children {
            if let Some(found) = find_dom_node(child, node) {
                return Some(found);
            }
        }
    }
    if let Some(content) = tree.get("templateContent")
        && let Some(found) = find_dom_node(content, node)
    {
        return Some(found);
    }
    None
}

fn remote_object(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "type": "object", "subtype": "null", "value": null }),
        Value::Bool(_) => json!({ "type": "boolean", "value": value }),
        Value::Number(_) => json!({
            "type": "number",
            "value": value,
            "description": value.to_string(),
        }),
        Value::String(_) => json!({ "type": "string", "value": value }),
        Value::Array(items) => json!({
            "type": "object",
            "subtype": "array",
            "value": value,
            "description": format!("Array({})", items.len()),
        }),
        Value::Object(_) => json!({
            "type": "object",
            "value": value,
            "description": "Object",
        }),
    }
}

async fn dispatch(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    target: &str,
    method: &str,
    params: &Value,
) -> Result<Value, (i64, String)> {
    match method {
        "Browser.getVersion" => Ok(json!({
            "protocolVersion": "1.3",
            "product": format!("NexiumX/{}", crate::VERSION),
            "revision": "",
            "userAgent": service.config().user_agent,
            "jsVersion": "Boa",
        })),
        "Browser.close" => Ok(json!({})),
        "Target.getTargets" => {
            let infos = TARGETS
                .lock()
                .map(|targets| {
                    targets
                        .iter()
                        .map(|(id, url)| {
                            json!({
                                "targetId": id,
                                "type": "page",
                                "title": "",
                                "url": url,
                                "attached": id == target,
                                "canAccessOpener": false,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(json!({ "targetInfos": infos }))
        }
        "Target.attachToTarget" => Ok(json!({ "sessionId": target })),
        "Target.createTarget" => {
            let url = params.get("url").and_then(Value::as_str).unwrap_or("");
            let options = CreateOptions {
                url: (!url.is_empty()).then(|| url.to_owned()),
                ..Default::default()
            };
            let value = service.create(auth, options).await.map_err(cdp_error)?;
            let id = value
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Ok(mut targets) = TARGETS.lock() {
                targets.insert(id.clone(), url.to_owned());
            }
            Ok(json!({ "targetId": id }))
        }
        "Target.closeTarget" | "Page.close" => {
            let id = params
                .get("targetId")
                .and_then(Value::as_str)
                .unwrap_or(target);
            service.close(auth, id).await.map_err(cdp_error)?;
            if let Ok(mut targets) = TARGETS.lock() {
                targets.remove(id);
            }
            Ok(json!({ "success": true }))
        }
        "Page.enable"
        | "Page.disable"
        | "Runtime.enable"
        | "Runtime.disable"
        | "Network.enable"
        | "Log.enable"
        | "DOM.enable"
        | "DOM.disable"
        | "Target.setDiscoverTargets"
        | "Target.setAutoAttach"
        | "Target.detachFromTarget"
        | "Runtime.releaseObject"
        | "Page.setLifecycleEventsEnabled" => Ok(json!({})),
        "Browser.getBrowserCommandLine" => Ok(json!({ "arguments": [] })),
        "Target.getTargetInfo" => {
            let target_id = params
                .get("targetId")
                .and_then(Value::as_str)
                .unwrap_or(target);
            let url = TARGETS
                .lock()
                .ok()
                .and_then(|targets| targets.get(target_id).cloned())
                .unwrap_or_default();
            Ok(json!({
                "targetInfo": {
                    "targetId": target_id,
                    "type": "page",
                    "title": "",
                    "url": url,
                    "attached": target_id == target,
                    "canAccessOpener": false,
                }
            }))
        }
        "Target.activateTarget" => Ok(json!({})),
        "Page.getCookies" => {
            let cookies = service
                .cookies(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            Ok(json!({ "cookies": cookies }))
        }
        "Page.setCookie" | "Storage.setCookies" => {
            if let Some(cookies) = params.get("cookies").and_then(Value::as_array) {
                for cookie in cookies {
                    let name = cookie.get("name").and_then(Value::as_str).unwrap_or("");
                    let value = cookie.get("value").and_then(Value::as_str).unwrap_or("");
                    if !name.is_empty() {
                        service
                            .add_cookie(auth, Some(target), name, value)
                            .await
                            .map_err(cdp_error)?;
                    }
                }
            }
            Ok(json!({}))
        }
        "Page.deleteCookie" | "Storage.clearDataForOrigin" => {
            service
                .delete_cookies(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            if method == "Storage.clearDataForOrigin" {
                service.clear_cache(auth, Some(target)).await.ok();
            }
            Ok(json!({}))
        }
        "Storage.getCookies" => {
            let cookies = service
                .cookies(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            Ok(json!({ "cookies": cookies }))
        }
        "Storage.getUsageAndQuota" => Ok(json!({
            "usage": 0,
            "quota": 0,
            "overrideActive": false,
            "usageBreakdown": []
        })),
        "Network.setCacheDisabled" => {
            if params
                .get("cacheDisabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                service.clear_cache(auth, Some(target)).await.ok();
            }
            Ok(json!({}))
        }
        "Network.clearBrowserCache" => {
            service
                .clear_cache(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            Ok(json!({}))
        }
        "Performance.enable" | "Performance.disable" | "Console.enable" => Ok(json!({})),
        "Performance.getMetrics" => Ok(json!({ "metrics": [] })),
        "Security.enable" | "Security.disable" => Ok(json!({})),
        "Security.setIgnoreCertificateErrors"
        | "Fetch.enable"
        | "Fetch.disable"
        | "Network.setExtraHTTPHeaders"
        | "Network.emulateNetworkConditions" => {
            Err((-32601, format!("{method} is not supported by NexiumX")))
        }
        "Emulation.clearDeviceMetricsOverride" => {
            service
                .set_window_rect(auth, Some(target), 1280.0, 720.0)
                .await
                .map_err(cdp_error)?;
            Ok(json!({}))
        }
        "DOM.getNodeForLocation" => {
            let x = params.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let y = params.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let element = service
                .element_at_point(auth, Some(target), x, y)
                .await
                .map_err(cdp_error)?;
            let node = element.get("node").and_then(Value::as_u64).unwrap_or(0);
            Ok(json!({ "nodeId": node, "backendNodeId": node, "frameId": "main" }))
        }
        "DOM.getContentQuads" => {
            let node = param_node(params)?;
            let rect = service
                .dom_box(auth, Some(target), node)
                .await
                .map_err(cdp_error)?;
            let x = rect.get("x").and_then(Value::as_f64).unwrap_or(0.0);
            let y = rect.get("y").and_then(Value::as_f64).unwrap_or(0.0);
            let width = rect.get("width").and_then(Value::as_f64).unwrap_or(0.0);
            let height = rect.get("height").and_then(Value::as_f64).unwrap_or(0.0);
            Ok(json!({
                "quads": [[x, y, x + width, y, x + width, y + height, x, y + height]]
            }))
        }
        "DOM.scrollIntoViewIfNeeded" => {
            let node = param_node(params)?;
            service
                .scroll_node(auth, Some(target), node)
                .await
                .map_err(cdp_error)?;
            Ok(json!({}))
        }
        "Schema.getDomains" => Ok(json!({
            "domains": [
                { "name": "Browser" },
                { "name": "Target" },
                { "name": "Page" },
                { "name": "Runtime" },
                { "name": "DOM" },
                { "name": "Input" },
                { "name": "Emulation" }
            ]
        })),
        "DOM.getDocument" => {
            let tree = service
                .dom_tree(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            Ok(json!({ "root": tree }))
        }
        "DOM.describeNode" => {
            let node = param_node(params)?;
            let tree = service
                .dom_tree(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            let found =
                find_dom_node(&tree, node as u64).ok_or((-32602, "node not found".to_owned()))?;
            Ok(json!({ "node": found }))
        }
        "DOM.querySelector" => {
            let root = param_node(params)?;
            let selector = params
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let nodes = service
                .dom_query(auth, Some(target), root, &selector, false)
                .await
                .map_err(cdp_error)?;
            let node = nodes
                .as_array()
                .and_then(|nodes| nodes.first())
                .and_then(Value::as_u64)
                .unwrap_or(0);
            Ok(json!({ "nodeId": node }))
        }
        "DOM.querySelectorAll" => {
            let root = param_node(params)?;
            let selector = params
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let nodes = service
                .dom_query(auth, Some(target), root, &selector, true)
                .await
                .map_err(cdp_error)?;
            Ok(json!({ "nodeIds": nodes }))
        }
        "DOM.getOuterHTML" => {
            let node = param_node(params)?;
            let value = service
                .dom_outer(auth, Some(target), node)
                .await
                .map_err(cdp_error)?;
            Ok(json!({
                "outerHTML": value.get("html").cloned().unwrap_or(Value::Null)
            }))
        }
        "DOM.getAttributes" => {
            let node = param_node(params)?;
            let attributes = service
                .dom_attributes(auth, Some(target), node)
                .await
                .map_err(cdp_error)?;
            let flat: Vec<Value> = attributes
                .as_array()
                .map(|attributes| {
                    attributes
                        .iter()
                        .flat_map(|attribute| {
                            [
                                attribute.get("name").cloned().unwrap_or(Value::Null),
                                attribute.get("value").cloned().unwrap_or(Value::Null),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!({ "attributes": flat }))
        }
        "DOM.getBoxModel" => {
            let node = param_node(params)?;
            let rect = service
                .dom_box(auth, Some(target), node)
                .await
                .map_err(cdp_error)?;
            let x = rect.get("x").and_then(Value::as_f64).unwrap_or(0.0);
            let y = rect.get("y").and_then(Value::as_f64).unwrap_or(0.0);
            let width = rect.get("width").and_then(Value::as_f64).unwrap_or(0.0);
            let height = rect.get("height").and_then(Value::as_f64).unwrap_or(0.0);
            let quad = json!([x, y, x + width, y, x + width, y + height, x, y + height]);
            Ok(json!({
                "model": {
                    "content": quad,
                    "padding": quad,
                    "border": quad,
                    "margin": quad,
                    "width": width,
                    "height": height,
                }
            }))
        }
        "DOM.resolveNode" => {
            let node = param_node(params)?;
            Ok(json!({
                "object": {
                    "type": "object",
                    "objectId": format!("node-{node}"),
                    "description": "Element",
                }
            }))
        }
        "DOM.focus" => {
            let node = param_node(params)?;
            service
                .focus_node(auth, Some(target), node)
                .await
                .map_err(cdp_error)?;
            Ok(json!({}))
        }
        "Input.dispatchMouseEvent" => {
            let event_type = params
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("mouseReleased");
            let x = params.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let y = params.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let button = match params.get("button").and_then(Value::as_str) {
                Some("right") => "right",
                Some("middle") => "middle",
                _ => "left",
            };
            match event_type {
                "mouseReleased" => {
                    let options = ClickOptions {
                        x: Some(x),
                        y: Some(y),
                        selector: None,
                        button: Some(button.to_owned()),
                    };
                    service
                        .click(auth, Some(target), options)
                        .await
                        .map_err(cdp_error)?;
                }
                "mouseMoved" => {
                    let options = HoverOptions {
                        x: Some(x),
                        y: Some(y),
                        selector: None,
                    };
                    service
                        .hover(auth, Some(target), options)
                        .await
                        .map_err(cdp_error)?;
                }
                _ => {}
            }
            Ok(json!({}))
        }
        "Input.dispatchKeyEvent" => {
            let event_type = params
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("keyDown");
            if event_type == "keyDown" || event_type == "char" {
                if let Some(text) = params.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    let options = TypeOptions {
                        text: text.to_owned(),
                        selector: None,
                    };
                    service
                        .type_text(auth, Some(target), options)
                        .await
                        .map_err(cdp_error)?;
                } else if let Some(key) = params
                    .get("key")
                    .or_else(|| params.get("code"))
                    .and_then(Value::as_str)
                {
                    let options = PressOptions {
                        key: key.to_owned(),
                    };
                    service
                        .press(auth, Some(target), options)
                        .await
                        .map_err(cdp_error)?;
                }
            }
            Ok(json!({}))
        }
        "Input.insertText" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let options = TypeOptions {
                text,
                selector: None,
            };
            service
                .type_text(auth, Some(target), options)
                .await
                .map_err(cdp_error)?;
            Ok(json!({}))
        }
        "Emulation.setDeviceMetricsOverride" => {
            let width = params
                .get("width")
                .and_then(Value::as_f64)
                .unwrap_or(1280.0);
            let height = params
                .get("height")
                .and_then(Value::as_f64)
                .unwrap_or(720.0);
            service
                .set_window_rect(auth, Some(target), width as f32, height as f32)
                .await
                .map_err(cdp_error)?;
            Ok(json!({}))
        }
        "Runtime.getProperties" => Ok(json!({ "result": [], "internalProperties": [] })),
        "Page.navigate" => {
            let url = params.get("url").and_then(Value::as_str).unwrap_or("");
            match service.navigate(auth, Some(target), url).await {
                Ok(data) => {
                    if let Ok(mut targets) = TARGETS.lock() {
                        targets.insert(target.to_owned(), url.to_owned());
                    }
                    Ok(json!({
                        "frameId": "main",
                        "loaderId": "",
                        "url": data.get("url").cloned().unwrap_or(Value::Null),
                    }))
                }
                Err(error) => {
                    let (_, message) = cdp_error(error);
                    Ok(json!({ "frameId": "main", "errorText": message }))
                }
            }
        }
        "Page.reload" => {
            service
                .reload(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            Ok(json!({}))
        }
        "Page.captureScreenshot" => {
            let format = params
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("png")
                .to_owned();
            let quality = params
                .get("quality")
                .and_then(Value::as_u64)
                .map(|quality| quality.min(100) as u8);
            let options = ScreenshotOptions {
                format: Some(format),
                quality,
                full_page: params.get("captureBeyondViewport").and_then(Value::as_bool),
                element: None,
            };
            let data = service
                .screenshot(auth, Some(target), options)
                .await
                .map_err(cdp_error)?;
            Ok(json!({
                "data": data.get("data").cloned().unwrap_or(Value::Null)
            }))
        }
        "Page.getLayoutMetrics" => {
            let status = service
                .status(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            let width = status
                .pointer("/viewport/width")
                .and_then(Value::as_f64)
                .unwrap_or(1280.0);
            let height = status
                .pointer("/viewport/height")
                .and_then(Value::as_f64)
                .unwrap_or(720.0);
            let script = "Math.max(document.documentElement.scrollHeight, document.body ? document.body.scrollHeight : 0, innerHeight)".to_owned();
            let content_height = match service
                .evaluate(auth, Some(target), EvaluateOptions { script })
                .await
            {
                Ok(value) => value
                    .get("result")
                    .and_then(Value::as_f64)
                    .unwrap_or(height),
                Err(_) => height,
            };
            Ok(json!({
                "contentSize": { "x": 0.0, "y": 0.0, "width": width, "height": content_height },
                "layoutViewport": { "pageX": 0.0, "pageY": 0.0, "clientWidth": width, "clientHeight": height },
                "visualViewport": { "offsetX": 0.0, "offsetY": 0.0, "pageX": 0.0, "pageY": 0.0, "clientWidth": width, "clientHeight": height, "scale": 1.0 },
            }))
        }
        "Page.getFrameTree" => {
            let status = service
                .status(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            let url = status.pointer("/page/url").cloned().unwrap_or(Value::Null);
            Ok(json!({
                "frameTree": {
                    "frame": {
                        "id": "main",
                        "loaderId": "",
                        "url": url,
                        "domainAndRegistry": "",
                        "mimeType": "text/html",
                        "securityOrigin": "",
                    }
                }
            }))
        }
        "Page.getNavigationHistory" => {
            let status = service
                .status(auth, Some(target))
                .await
                .map_err(cdp_error)?;
            let url = status.pointer("/page/url").cloned().unwrap_or(Value::Null);
            let title = status
                .pointer("/page/title")
                .cloned()
                .unwrap_or(Value::Null);
            Ok(json!({
                "currentIndex": 0,
                "entries": [{ "id": 0, "url": url, "userTypedURL": url, "title": title, "transitionType": "typed" }]
            }))
        }
        "Page.bringToFront" => Ok(json!({})),
        "Runtime.evaluate" => {
            let expression = params
                .get("expression")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if expression.trim().is_empty() {
                return Err((-32602, "expression must not be empty".to_owned()));
            }
            let value = service
                .evaluate(auth, Some(target), EvaluateOptions { script: expression })
                .await
                .map_err(cdp_error)?;
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            Ok(json!({ "result": remote_object(&result) }))
        }
        "Runtime.callFunctionOn" => {
            let declaration = params
                .get("functionDeclaration")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if declaration.trim().is_empty() {
                return Err((-32602, "functionDeclaration must not be empty".to_owned()));
            }
            let object_id = params.get("objectId").and_then(Value::as_str);
            let script = if let Some(node) = object_id
                .and_then(|object_id| object_id.strip_prefix("node-"))
                .and_then(|node| node.parse::<usize>().ok())
            {
                let selector = service
                    .element_selector(auth, Some(target), node)
                    .await
                    .map_err(cdp_error)?;
                let selector = selector.as_str().unwrap_or_default();
                format!(
                    "(()=>{{const el=document.querySelector({});return ({declaration}).call(el);}})()",
                    serde_json::to_string(selector).unwrap_or_default()
                )
            } else {
                format!("({declaration})()")
            };
            let value = service
                .evaluate(auth, Some(target), EvaluateOptions { script })
                .await
                .map_err(cdp_error)?;
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            Ok(json!({ "result": remote_object(&result) }))
        }
        other => Err((-32601, format!("method not found: {other}"))),
    }
}
