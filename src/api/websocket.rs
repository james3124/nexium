//! Real-time WebSocket command and event transport.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::routes::respond_error;
use super::service::{ApiError, ApiService, AuthContext, TabsCreateOptions, TabsTargetOptions};
use crate::mcp;

#[derive(Default, Deserialize)]
pub(crate) struct WsQuery {
    token: Option<String>,
    agent: Option<String>,
}

#[derive(Deserialize)]
struct WsCommand {
    #[serde(default)]
    id: Value,
    command: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

pub(crate) async fn ws_handler(
    State(service): State<Arc<ApiService>>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .or(query.token.as_deref());
    let agent = headers
        .get("x-nexium-agent")
        .and_then(|value| value.to_str().ok())
        .or(query.agent.as_deref());
    let auth = match service.authenticate(token, agent) {
        Ok(auth) => auth,
        Err(error) => return respond_error(error).into_response(),
    };
    ws.on_upgrade(move |socket| handle_socket(service, auth, socket))
}

async fn handle_socket(service: Arc<ApiService>, auth: AuthContext, mut socket: WebSocket) {
    let mut events = service.subscribe();
    let connected = json!({
        "event": "connected",
        "data": {
            "agent": auth.agent,
            "server": "NexiumX",
            "version": crate::VERSION,
        }
    });
    if socket
        .send(Message::Text(connected.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) if event.owner == auth.agent => {
                        if let Ok(text) = serde_json::to_string(&event)
                            && socket.send(Message::Text(text.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let response = handle_text(&service, &auth, &text).await;
                        if socket.send(Message::Text(response.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn handle_text(service: &Arc<ApiService>, auth: &AuthContext, text: &str) -> Value {
    let command: WsCommand = match serde_json::from_str(text) {
        Ok(command) => command,
        Err(error) => {
            return json!({
                "id": null,
                "ok": false,
                "error": { "code": "bad_request", "message": error.to_string() },
            });
        }
    };
    let mut args = command.params.unwrap_or_else(|| json!({}));
    if !args.is_object() {
        args = json!({});
    }
    let key = match command.session_id.as_deref() {
        Some(session) => format!("{}/{}", auth.agent, session),
        None => auth.agent.clone(),
    };
    if let Err(error) = service.check_rate_limit(&key, true) {
        let mut body = json!({ "code": error.code, "message": error.message });
        if let Some(retry_after_ms) = error.retry_after_ms {
            body["retry_after_ms"] = json!(retry_after_ms);
        }
        return json!({ "id": command.id, "ok": false, "error": body });
    }
    if let Some(session_id) = command.session_id.as_deref() {
        args["session_id"] = json!(session_id);
    }
    let result = match command.command.as_str() {
        "tabs_create" => match serde_json::from_value::<TabsCreateOptions>(args.clone()) {
            Ok(options) => {
                let session = command.session_id.as_deref();
                service.tabs_create(auth, session, options).await
            }
            Err(error) => Err(ApiError::bad_request(error.to_string())),
        },
        "tabs_close" => match serde_json::from_value::<TabsTargetOptions>(args.clone()) {
            Ok(options) => {
                let session = command.session_id.as_deref();
                service.tabs_close(auth, session, options).await
            }
            Err(error) => Err(ApiError::bad_request(error.to_string())),
        },
        other => {
            let tool = if other.starts_with("browser_") {
                other.to_owned()
            } else {
                format!("browser_{other}")
            };
            mcp::call_tool(service, auth, &tool, args).await
        }
    };
    match result {
        Ok(data) => json!({ "id": command.id, "ok": true, "data": data }),
        Err(error) => json!({
            "id": command.id,
            "ok": false,
            "error": { "code": error.code, "message": error.message },
        }),
    }
}
