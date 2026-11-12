//! Phase 6/8 API hardening: rate limiting, token registry/rotation, audit log
//! and the MCP HTTP SSE transport.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use nexium::ApiService;
use nexium::Config;
use nexium::config::ApiTokenConfig;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-gap6-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn spawn_api(mutate: impl FnOnce(&mut Config)) -> (SocketAddr, Arc<ApiService>, PathBuf) {
    let data_dir = temp_dir("server");
    let mut config = Config {
        data_dir: data_dir.clone(),
        ..Config::default()
    };
    mutate(&mut config);
    let service = ApiService::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = Arc::clone(&service);
    tokio::spawn(async move {
        nexium::api::serve_on(listener, server).await.unwrap();
    });
    (addr, service, data_dir)
}

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Value,
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    agent: Option<&str>,
    body: Option<Value>,
) -> HttpResponse {
    http_with_session(addr, method, path, token, agent, None, body).await
}

#[allow(clippy::too_many_arguments)]
async fn http_with_session(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    agent: Option<&str>,
    session: Option<&str>,
    body: Option<Value>,
) -> HttpResponse {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let payload = body.map(|body| body.to_string()).unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", addr.to_string())
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(agent) = agent {
        builder = builder.header("x-nexium-agent", agent);
    }
    if let Some(session) = session {
        builder = builder.header("x-nexium-session", session);
    }
    let request = builder.body(Full::new(Bytes::from(payload))).unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    HttpResponse {
        status,
        headers,
        body,
    }
}

#[tokio::test]
async fn rate_limit_returns_structured_429_per_agent() {
    let (addr, _service, data_dir) = spawn_api(|config| {
        config.api_rate_limit_per_minute = 3;
    })
    .await;

    for _ in 0..3 {
        let ok = http(addr, "GET", "/health", None, Some("agent-a"), None).await;
        assert_eq!(ok.status, 200, "{}", ok.body);
    }
    let limited = http(addr, "GET", "/health", None, Some("agent-a"), None).await;
    assert_eq!(limited.status, 429, "{}", limited.body);
    assert_eq!(limited.body["ok"], false);
    assert_eq!(limited.body["error"]["code"], "rate_limited");
    assert!(
        limited.body["error"]["retry_after_ms"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "{}",
        limited.body
    );
    assert!(limited.headers.contains_key("retry-after"));

    // Another agent keeps its own budget.
    let other = http(addr, "GET", "/health", None, Some("agent-b"), None).await;
    assert_eq!(other.status, 200, "{}", other.body);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn multiple_tokens_map_to_agents_and_legacy_token_still_works() {
    let (addr, _service, data_dir) = spawn_api(|config| {
        config.api_token = Some("legacy-secret".to_owned());
        config.api_tokens = vec![
            ApiTokenConfig {
                token: "alpha-token".to_owned(),
                agent: "agent-a".to_owned(),
            },
            ApiTokenConfig {
                token: "beta-token".to_owned(),
                agent: "agent-b".to_owned(),
            },
        ];
    })
    .await;

    let missing = http(
        addr,
        "POST",
        "/browser/create",
        None,
        Some("agent-a"),
        Some(json!({})),
    )
    .await;
    assert_eq!(missing.status, 401);

    // The token's mapped agent wins over a spoofed agent header.
    let created = http(
        addr,
        "POST",
        "/browser/create",
        Some("alpha-token"),
        Some("agent-b"),
        Some(json!({})),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);
    let session = created.body["data"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let foreign = http_with_session(
        addr,
        "GET",
        "/browser/status",
        Some("beta-token"),
        Some("agent-b"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(foreign.status, 403, "{}", foreign.body);

    let owner = http_with_session(
        addr,
        "GET",
        "/browser/status",
        Some("alpha-token"),
        Some("agent-a"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(owner.status, 200, "{}", owner.body);
    assert_eq!(owner.body["data"]["session_id"], session);

    // Backward compatibility: the legacy single token still authenticates and
    // honours the agent header.
    let legacy = http(
        addr,
        "POST",
        "/browser/create",
        Some("legacy-secret"),
        Some("agent-legacy"),
        Some(json!({})),
    )
    .await;
    assert_eq!(legacy.status, 200, "{}", legacy.body);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn token_rotation_and_revocation_take_effect_immediately() {
    let (addr, _service, data_dir) = spawn_api(|config| {
        config.api_tokens = vec![ApiTokenConfig {
            token: "rotate-me".to_owned(),
            agent: "agent-r".to_owned(),
        }];
    })
    .await;

    let listed = http(
        addr,
        "GET",
        "/v1/admin/tokens",
        Some("rotate-me"),
        None,
        None,
    )
    .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let tokens = listed.body["data"]["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0]["agent"], "agent-r");
    assert!(tokens[0].get("token").is_none(), "{}", tokens[0]);
    let token_id = tokens[0]["id"].as_str().unwrap().to_owned();

    let rotated = http(
        addr,
        "POST",
        "/v1/admin/tokens/rotate",
        Some("rotate-me"),
        None,
        Some(json!({ "token_id": token_id })),
    )
    .await;
    assert_eq!(rotated.status, 200, "{}", rotated.body);
    let fresh = rotated.body["data"]["token"].as_str().unwrap().to_owned();
    assert_ne!(fresh, "rotate-me");
    assert_eq!(rotated.body["data"]["token_id"], token_id);

    let old = http(
        addr,
        "GET",
        "/v1/admin/tokens",
        Some("rotate-me"),
        None,
        None,
    )
    .await;
    assert_eq!(old.status, 401, "{}", old.body);

    let usable = http(addr, "GET", "/v1/admin/tokens", Some(&fresh), None, None).await;
    assert_eq!(usable.status, 200, "{}", usable.body);

    let revoked = http(
        addr,
        "DELETE",
        &format!("/v1/admin/tokens/{token_id}"),
        Some(&fresh),
        None,
        None,
    )
    .await;
    assert_eq!(revoked.status, 200, "{}", revoked.body);
    assert_eq!(revoked.body["data"]["revoked"], true);

    let after = http(addr, "GET", "/v1/admin/tokens", Some(&fresh), None, None).await;
    assert_eq!(after.status, 401, "{}", after.body);

    let missing = http(
        addr,
        "DELETE",
        &format!("/v1/admin/tokens/{token_id}"),
        Some("whatever"),
        None,
        None,
    )
    .await;
    assert_eq!(missing.status, 401);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn audit_log_records_requests_and_caps_at_bound() {
    let (addr, service, data_dir) = spawn_api(|config| {
        config.api_audit_capacity = 5;
    })
    .await;

    for _ in 0..8 {
        let response = http(addr, "GET", "/health", None, Some("audit-agent"), None).await;
        assert_eq!(response.status, 200);
    }

    // MCP tools/call entries are audited too.
    let auth = service.local_auth("mcp-audit");
    nexium::mcp::handle_request(
        &service,
        &auth,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "browser_status", "arguments": {} }
        }),
    )
    .await
    .unwrap();

    let audit = http(
        addr,
        "GET",
        "/v1/audit?limit=50",
        None,
        Some("audit-agent"),
        None,
    )
    .await;
    assert_eq!(audit.status, 200, "{}", audit.body);
    let entries = audit.body["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 5, "ring buffer must cap at its bound");
    assert_eq!(audit.body["data"]["capacity"], 5);
    assert!(audit.body["data"]["total_recorded"].as_u64().unwrap() >= 8);
    assert!(
        entries[0]["path"]
            .as_str()
            .unwrap_or("")
            .contains("tools/call"),
        "newest-first ordering, got {entries:?}"
    );
    for entry in entries {
        assert!(entry["timestamp_ms"].as_u64().is_some());
        assert!(entry["agent"].as_str().is_some());
        assert!(entry["method"].as_str().is_some());
        assert!(entry["path"].as_str().is_some());
        assert!(entry["status"].as_u64().is_some());
        assert!(entry["duration_ms"].as_u64().is_some());
        assert!(entry.get("token").is_none());
        assert!(entry.get("body").is_none());
    }
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"].as_str().unwrap_or("").contains("tools/call")),
        "{entries:?}"
    );

    let limited = http(
        addr,
        "GET",
        "/v1/audit?limit=2",
        None,
        Some("audit-agent"),
        None,
    )
    .await;
    assert_eq!(limited.body["data"]["entries"].as_array().unwrap().len(), 2);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn websocket_commands_are_rate_limited_without_disconnect() {
    let (addr, _service, data_dir) = spawn_api(|config| {
        config.api_ws_rate_limit_per_minute = 2;
    })
    .await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws?agent=ws-agent"))
            .await
            .unwrap();
    let connected = socket.next().await.unwrap().unwrap();
    let connected: Value = serde_json::from_str(connected.to_text().unwrap()).unwrap();
    assert_eq!(connected["event"], "connected");

    let mut codes = Vec::new();
    for id in 0..3u64 {
        socket
            .send(Message::Text(
                json!({ "id": id, "command": "definitely_unknown" })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let message = socket.next().await.unwrap().unwrap();
            let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            if value.get("id").and_then(Value::as_u64) == Some(id)
                && let Some(code) = value.pointer("/error/code").and_then(Value::as_str)
            {
                codes.push(code.to_owned());
                break;
            }
        }
    }
    assert_eq!(codes[2], "rate_limited", "{codes:?}");
    socket.close(None).await.unwrap();

    let _ = std::fs::remove_dir_all(data_dir);
}

struct SseClient {
    stream: tokio::net::TcpStream,
    buffer: Vec<u8>,
}

impl SseClient {
    async fn connect(addr: SocketAddr) -> Self {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request =
            format!("GET /mcp/sse HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        Self {
            stream,
            buffer: Vec::new(),
        }
    }

    async fn next_event(&mut self, name: &str) -> String {
        let needle = format!("event: {name}\ndata: ");
        loop {
            {
                let text = String::from_utf8_lossy(&self.buffer).replace("\r\n", "\n");
                if let Some(start) = text.find(&needle) {
                    let data_start = start + needle.len();
                    if let Some(end) = text[data_start..].find('\n') {
                        let data = text[data_start..data_start + end].trim().to_owned();
                        // Earlier frames are no longer needed; dropping the
                        // whole buffer keeps chunked-framing offsets irrelevant.
                        self.buffer.clear();
                        return data;
                    }
                }
            }
            let mut chunk = [0u8; 4096];
            let read = tokio::time::timeout(Duration::from_secs(5), self.stream.read(&mut chunk))
                .await
                .expect("timed out waiting for SSE event")
                .unwrap();
            assert!(read > 0, "SSE stream closed early");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

#[tokio::test]
async fn mcp_sse_endpoint_streams_endpoint_and_responses() {
    let (addr, _service, data_dir) = spawn_api(|_| {}).await;
    let mut sse = SseClient::connect(addr).await;

    let endpoint = sse.next_event("endpoint").await;
    assert!(
        endpoint.starts_with("/mcp/message?sessionId="),
        "{endpoint}"
    );
    let session_id = endpoint.split("sessionId=").nth(1).unwrap().to_owned();

    let posted = http(
        addr,
        "POST",
        &format!("/mcp/message?sessionId={session_id}"),
        None,
        None,
        Some(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" }
        })),
    )
    .await;
    assert_eq!(posted.status, 202, "{}", posted.body);

    let message = sse.next_event("message").await;
    let response: Value = serde_json::from_str(&message).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(response["result"]["serverInfo"]["name"], "nexium");

    // The plain POST-only JSON-RPC endpoint keeps working.
    let direct = http(
        addr,
        "POST",
        "/mcp",
        None,
        None,
        Some(json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" })),
    )
    .await;
    assert_eq!(direct.status, 200, "{}", direct.body);
    assert_eq!(direct.body["id"], 9);
    assert_eq!(direct.body["result"], json!({}));

    let unknown = http(
        addr,
        "POST",
        "/mcp/message?sessionId=missing",
        None,
        None,
        Some(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" })),
    )
    .await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn mcp_streamable_http_get_stream_delete_and_version_echo() {
    let (addr, _service, data_dir) = spawn_api(|_| {}).await;

    let init = http(
        addr,
        "POST",
        "/mcp",
        None,
        Some("streamable"),
        Some(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "1" }
            }
        })),
    )
    .await;
    assert_eq!(init.status, 200, "{}", init.body);
    assert_eq!(init.body["result"]["protocolVersion"], "2025-11-25");

    let deleted = http(addr, "DELETE", "/mcp", None, Some("streamable"), None).await;
    assert_eq!(deleted.status, 204);

    // Streamable HTTP opens a long-lived SSE response; read only the head.
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"GET /mcp HTTP/1.1\r\nHost: nexium\r\nAccept: text/event-stream\r\n\
              MCP-Protocol-Version: 2025-11-25\r\n\r\n",
        )
        .await
        .unwrap();
    let mut buffer = vec![0u8; 2048];
    let read = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buffer))
        .await
        .expect("SSE response head")
        .unwrap();
    let head = String::from_utf8_lossy(&buffer[..read]).to_ascii_lowercase();
    assert!(head.contains("200 ok"), "{head}");
    assert!(head.contains("text/event-stream"), "{head}");
    assert!(head.contains("mcp-session-id:"), "{head}");

    let _ = std::fs::remove_dir_all(data_dir);
}
