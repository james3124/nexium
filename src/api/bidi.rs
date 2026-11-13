//! WebDriver BiDi subset over WebSocket (`GET /session`).
//!
//! Modules: session, browsingContext, script, network events, log events.
//! Commands are dispatched to the shared browser engine service; nothing is
//! duplicated from CDP/WebDriver.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::service::{
    ApiError, ApiService, AuthContext, CreateOptions, EvaluateOptions, ScreenshotOptions,
    TabsCreateOptions, TabsTargetOptions,
};

const AGENT: &str = "bidi";

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
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized").into_response())
}

pub(crate) async fn session_ws(
    State(service): State<Arc<ApiService>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Ok(auth) = auth(&service, &headers, query.get("token").map(String::as_str)) else {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    };
    ws.on_upgrade(move |socket| handle_socket(service, auth, socket))
}

fn timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

async fn send_value(socket: &mut WebSocket, value: Value) {
    let _ = socket.send(Message::Text(value.to_string().into())).await;
}

async fn send_event(socket: &mut WebSocket, method: &str, params: Value) {
    let _ = socket
        .send(Message::Text(
            json!({ "type": "event", "method": method, "params": params })
                .to_string()
                .into(),
        ))
        .await;
}

fn bidi_value(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "type": "null" }),
        Value::Bool(_) => json!({ "type": "boolean", "value": value }),
        Value::Number(_) => json!({ "type": "number", "value": value }),
        Value::String(_) => json!({ "type": "string", "value": value }),
        Value::Array(items) => json!({
            "type": "array",
            "value": items.iter().map(bidi_value).collect::<Vec<_>>()
        }),
        Value::Object(map) => json!({
            "type": "object",
            "value": map
                .iter()
                .map(|(key, value)| json!([key, bidi_value(value)]))
                .collect::<Vec<_>>()
        }),
    }
}

fn contexts_from_tabs(value: &Value) -> Vec<Value> {
    value
        .get("tabs")
        .and_then(Value::as_array)
        .map(|tabs| {
            tabs.iter()
                .filter_map(|tab| {
                    let id = tab.get("tab_id").and_then(Value::as_u64)?;
                    Some(json!({
                        "context": id.to_string(),
                        "url": tab.get("url").cloned().unwrap_or(Value::Null),
                        "userContext": "default",
                        "originalOpener": Value::Null,
                        "children": [],
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn switch_tab(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    session: &str,
    context: &str,
) -> Result<(), (String, String)> {
    let tab_id = context.parse::<u64>().map_err(|_| {
        (
            "invalid argument".to_owned(),
            format!("invalid context: {context}"),
        )
    })?;
    service
        .tabs_switch(
            auth,
            Some(session),
            TabsTargetOptions {
                tab_id: Some(tab_id),
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| (error.code, error.message))
}

fn error_reply(id: Value, code: impl Into<String>, message: impl Into<String>) -> Value {
    json!({
        "type": "error",
        "id": id,
        "error": code.into(),
        "message": message.into(),
        "stacktrace": ""
    })
}

fn ok_reply(id: Value, result: Value) -> Value {
    json!({ "type": "success", "id": id, "result": result })
}

async fn handle_socket(service: Arc<ApiService>, auth: AuthContext, mut socket: WebSocket) {
    let mut session: Option<String> = None;
    let mut seen = (0usize, 0usize, 0usize);
    while let Some(message) = socket.recv().await {
        match message {
            Ok(Message::Text(text)) => {
                let request: Value = match serde_json::from_str(&text) {
                    Ok(request) => request,
                    Err(error) => {
                        send_value(
                            &mut socket,
                            error_reply(Value::Null, "invalid argument", error.to_string()),
                        )
                        .await;
                        continue;
                    }
                };
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                let mut events: Vec<(String, Value)> = Vec::new();
                let response = dispatch(
                    &service,
                    &auth,
                    &mut session,
                    &mut events,
                    &mut seen,
                    &method,
                    &params,
                    id.clone(),
                )
                .await;
                send_value(&mut socket, response).await;
                for (method, params) in events {
                    send_event(&mut socket, &method, params).await;
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
    if let Some(id) = session {
        let _ = service.close(&auth, &id).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    session: &mut Option<String>,
    events: &mut Vec<(String, Value)>,
    seen: &mut (usize, usize, usize),
    method: &str,
    params: &Value,
    id: Value,
) -> Value {
    match method {
        "session.status" => ok_reply(
            id,
            json!({ "ready": true, "message": format!("NexiumX {}", crate::VERSION) }),
        ),
        "session.new" => {
            if let Some(existing) = session.clone() {
                let _ = service.close(auth, &existing).await;
            }
            let options = CreateOptions::default();
            match service.create(auth, options).await {
                Ok(value) => {
                    let session_id = value
                        .get("session_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    *session = Some(session_id.clone());
                    ok_reply(
                        id,
                        json!({
                            "sessionId": session_id,
                            "capabilities": {
                                "browserName": "nexiumx",
                                "browserVersion": crate::VERSION,
                                "platformName": std::env::consts::OS,
                                "acceptInsecureCerts": true,
                                "setWindowRect": false,
                            }
                        }),
                    )
                }
                Err(error) => error_reply(id, error.code, error.message),
            }
        }
        "session.end" | "browser.close" => {
            if let Some(existing) = session.take() {
                let _ = service.close(auth, &existing).await;
            }
            ok_reply(id, json!({}))
        }
        _ => {
            let Some(session_id) = session.clone() else {
                return error_reply(id, "invalid session id", "no BiDi session");
            };
            match method {
                "browsingContext.getTree" => match service.tabs(auth, Some(&session_id)).await {
                    Ok(value) => ok_reply(id, json!({ "contexts": contexts_from_tabs(&value) })),
                    Err(error) => error_reply(id, error.code, error.message),
                },
                "browsingContext.create" => {
                    let url = params.get("url").and_then(Value::as_str).map(str::to_owned);
                    let options = TabsCreateOptions { url };
                    match service.tabs_create(auth, Some(&session_id), options).await {
                        Ok(value) => {
                            let context = value
                                .get("tab_id")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                .to_string();
                            events.push((
                                "browsingContext.contextCreated".to_owned(),
                                json!({
                                    "context": context,
                                    "url": "",
                                    "userContext": "default",
                                    "originalOpener": Value::Null,
                                    "children": [],
                                }),
                            ));
                            ok_reply(id, json!({ "context": context }))
                        }
                        Err(error) => error_reply(id, error.code, error.message),
                    }
                }
                "browsingContext.navigate" => {
                    let context = params
                        .get("context")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let url = params
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if let Err((code, message)) =
                        switch_tab(service, auth, &session_id, &context).await
                    {
                        return error_reply(id, code, message);
                    }
                    let navigation = format!("nav-{}", seen.0 + 1);
                    match service.navigate(auth, Some(&session_id), &url).await {
                        Ok(value) => {
                            let url = value.get("url").cloned().unwrap_or_else(|| json!(url));
                            events.push((
                                "browsingContext.navigationStarted".to_owned(),
                                json!({
                                    "context": context,
                                    "navigation": navigation,
                                    "url": url,
                                    "timestamp": timestamp(),
                                }),
                            ));
                            emit_log_events(service, auth, &session_id, events, seen).await;
                            events.push((
                                "browsingContext.load".to_owned(),
                                json!({
                                    "context": context,
                                    "navigation": navigation,
                                    "url": url,
                                    "timestamp": timestamp(),
                                }),
                            ));
                            ok_reply(id, json!({ "navigation": navigation, "url": url }))
                        }
                        Err(error) => error_reply(id, error.code, error.message),
                    }
                }
                "browsingContext.activate" => {
                    let context = params
                        .get("context")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match switch_tab(service, auth, &session_id, context).await {
                        Ok(()) => ok_reply(id, json!({})),
                        Err((code, message)) => error_reply(id, code, message),
                    }
                }
                "browsingContext.close" => {
                    let context = params
                        .get("context")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let tab_id = context.parse::<u64>().ok();
                    match service
                        .tabs_close(auth, Some(&session_id), TabsTargetOptions { tab_id })
                        .await
                    {
                        Ok(_) => {
                            events.push((
                                "browsingContext.contextDestroyed".to_owned(),
                                json!({ "context": context }),
                            ));
                            ok_reply(id, json!({}))
                        }
                        Err(error) => error_reply(id, error.code, error.message),
                    }
                }
                "browsingContext.captureScreenshot" => {
                    let context = params
                        .get("context")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if let Err((code, message)) =
                        switch_tab(service, auth, &session_id, context).await
                    {
                        return error_reply(id, code, message);
                    }
                    let options = ScreenshotOptions {
                        format: Some("png".to_owned()),
                        quality: None,
                        full_page: Some(false),
                        element: None,
                    };
                    match service.screenshot(auth, Some(&session_id), options).await {
                        Ok(value) => ok_reply(
                            id,
                            json!({
                                "data": value.get("data").cloned().unwrap_or(Value::Null)
                            }),
                        ),
                        Err(error) => error_reply(id, error.code, error.message),
                    }
                }
                "script.evaluate" => {
                    let expression = params
                        .get("expression")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    if let Some(context) = params
                        .get("target")
                        .and_then(|target| target.get("context"))
                        .and_then(Value::as_str)
                        && let Err((code, message)) =
                            switch_tab(service, auth, &session_id, context).await
                    {
                        return error_reply(id, code, message);
                    }
                    let options = EvaluateOptions { script: expression };
                    match service.evaluate(auth, Some(&session_id), options).await {
                        Ok(value) => {
                            let result = value.get("result").cloned().unwrap_or(Value::Null);
                            ok_reply(
                                id,
                                json!({ "type": "success", "result": bidi_value(&result) }),
                            )
                        }
                        Err(error) => error_reply(id, "unknown error", error.message),
                    }
                }
                "script.callFunction" => {
                    let declaration = params
                        .get("functionDeclaration")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let arguments: Vec<Value> = params
                        .get("arguments")
                        .and_then(Value::as_array)
                        .map(|arguments| {
                            arguments
                                .iter()
                                .map(|argument| {
                                    argument.get("value").cloned().unwrap_or(Value::Null)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let args =
                        serde_json::to_string(&arguments).unwrap_or_else(|_| "[]".to_owned());
                    let script = format!("({declaration})(...{args})");
                    let options = EvaluateOptions { script };
                    match service.evaluate(auth, Some(&session_id), options).await {
                        Ok(value) => {
                            let result = value.get("result").cloned().unwrap_or(Value::Null);
                            ok_reply(
                                id,
                                json!({ "type": "success", "result": bidi_value(&result) }),
                            )
                        }
                        Err(error) => error_reply(id, "unknown error", error.message),
                    }
                }
                "script.getRealms" => ok_reply(
                    id,
                    json!({
                        "realms": [{
                            "realm": format!("realm-{session_id}"),
                            "origin": "null",
                            "type": "window",
                        }]
                    }),
                ),
                "network.getCookies" => match service.cookies(auth, Some(&session_id)).await {
                    Ok(cookies) => ok_reply(id, json!({ "cookies": cookies })),
                    Err(error) => error_reply(id, error.code, error.message),
                },
                other => error_reply(id, "unknown command", format!("method not found: {other}")),
            }
        }
    }
}

async fn emit_log_events(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    session_id: &str,
    events: &mut Vec<(String, Value)>,
    seen: &mut (usize, usize, usize),
) {
    let context = service
        .tabs(auth, Some(session_id))
        .await
        .ok()
        .and_then(|value| {
            value
                .get("tabs")
                .and_then(Value::as_array)
                .and_then(|tabs| {
                    tabs.iter()
                        .find(|tab| tab.get("active").and_then(Value::as_bool) == Some(true))
                })
                .and_then(|tab| tab.get("tab_id").and_then(Value::as_u64))
        })
        .unwrap_or(0)
        .to_string();
    if let Ok(network) = service.network(auth, Some(session_id)).await
        && let Some(entries) = network.get("events").and_then(Value::as_array)
    {
        for (index, entry) in entries.iter().enumerate().skip(seen.0) {
            let request_id = format!("req-{index}");
            let url = entry.get("url").cloned().unwrap_or(Value::Null);
            let request_method = entry.get("method").cloned().unwrap_or_else(|| json!("GET"));
            let status = entry.get("status").cloned().unwrap_or(json!(200));
            events.push((
                "network.beforeRequestSent".to_owned(),
                json!({
                    "context": context,
                    "requestId": request_id,
                    "request": { "url": url, "method": request_method },
                    "initiator": { "type": "other" },
                    "timestamp": timestamp(),
                }),
            ));
            events.push((
                "network.responseCompleted".to_owned(),
                json!({
                    "context": context,
                    "requestId": request_id,
                    "response": { "url": url, "status": status },
                    "timestamp": timestamp(),
                }),
            ));
        }
        seen.0 = entries.len();
    }
    if let Ok(logs) = service.console(auth, Some(session_id)).await {
        if let Some(messages) = logs.get("messages").and_then(Value::as_array) {
            for entry in messages.iter().skip(seen.1) {
                let text = entry.get("text").cloned().unwrap_or_else(|| json!(""));
                events.push((
                    "log.entryAdded".to_owned(),
                    json!({
                        "type": "console",
                        "level": "info",
                        "text": text,
                        "timestamp": timestamp(),
                        "realm": format!("realm-{session_id}"),
                    }),
                ));
            }
            seen.1 = messages.len();
        }
        if let Some(errors) = logs.get("errors").and_then(Value::as_array) {
            for entry in errors.iter().skip(seen.2) {
                events.push((
                    "log.entryAdded".to_owned(),
                    json!({
                        "type": "javascript",
                        "level": "error",
                        "text": entry.clone(),
                        "timestamp": timestamp(),
                        "realm": format!("realm-{session_id}"),
                    }),
                ));
            }
            seen.2 = errors.len();
        }
    }
}

#[allow(dead_code)]
fn map_api_error(error: ApiError) -> Value {
    json!({ "error": error.code, "message": error.message })
}
