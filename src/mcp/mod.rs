//! Native MCP (Model Context Protocol) server over stdio or HTTP JSON-RPC.

mod tools;

pub use tools::{call as call_tool, tool_definitions};

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::api::service::{ApiService, AuthContext};

pub async fn handle_request(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    request: &Value,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str)?;
    match method {
        "initialize" => {
            let version = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            Some(success(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "nexium",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            ))
        }
        "notifications/initialized" | "notifications/cancelled" => None,
        "ping" => Some(success(id, json!({}))),
        "tools/list" => Some(success(id, json!({ "tools": tool_definitions() }))),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let started = Instant::now();
            let result = tools::call(service, auth, name, arguments).await;
            let status = match &result {
                Ok(_) => 200,
                Err(error) => error.status,
            };
            service.record_audit(
                &auth.agent,
                "MCP",
                &format!("tools/call {name}"),
                status,
                started.elapsed().as_millis() as u64,
            );
            match result {
                Ok(data) => {
                    let text =
                        serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string());
                    Some(success(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": text }],
                            "structuredContent": data,
                            "isError": false,
                        }),
                    ))
                }
                Err(error) => Some(success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("{}: {}", error.code, error.message),
                        }],
                        "isError": true,
                    }),
                )),
            }
        }
        other => Some(error(id, -32601, format!("method not found: {other}"))),
    }
}

fn success(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
}

fn error(id: Option<Value>, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message },
    })
}

pub fn parse_error(message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": -32700, "message": message },
    })
}

/// Serve MCP JSON-RPC over stdin/stdout (one JSON object per line).
pub async fn run_stdio(service: Arc<ApiService>) -> std::io::Result<()> {
    let auth = service.local_auth("mcp-stdio");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                write_response(&mut stdout, &parse_error(error.to_string())).await?;
                continue;
            }
        };
        if let Some(response) = handle_request(&service, &auth, &request).await {
            write_response(&mut stdout, &response).await?;
        }
    }
    Ok(())
}

async fn write_response(stdout: &mut tokio::io::Stdout, response: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    stdout.write_all(&bytes).await?;
    stdout.flush().await
}
