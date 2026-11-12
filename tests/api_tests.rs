mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use common::spawn_test_server;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use nexium::ApiService;
use nexium::Config;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-api-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn spawn_api(token: Option<&str>) -> (SocketAddr, Arc<ApiService>, PathBuf) {
    let data_dir = temp_dir("server");
    let mut config = Config {
        data_dir: data_dir.clone(),
        ..Config::default()
    };
    config.api_token = token.map(str::to_owned);
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
    body: Value,
}

async fn http(
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
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    HttpResponse { status, body }
}

#[tokio::test]
async fn rest_controls_a_live_browser_session() {
    let web = spawn_test_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;

    let invalid = http(
        addr,
        "POST",
        "/browser/create",
        None,
        Some("agent-a"),
        None,
        Some(json!({ "viewport": { "width": "wide", "height": 600 } })),
    )
    .await;
    assert_eq!(invalid.status, 400, "{}", invalid.body);
    assert_eq!(invalid.body["ok"], false);
    assert_eq!(invalid.body["error"]["code"], "bad_request");

    let created = http(
        addr,
        "POST",
        "/browser/create",
        None,
        Some("agent-a"),
        None,
        Some(json!({
            "url": format!("http://{web}/interact"),
            "viewport": { "width": 800, "height": 600 }
        })),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);
    assert_eq!(created.body["ok"], true);
    let session = created.body["data"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let status = http(
        addr,
        "GET",
        "/browser/status",
        None,
        Some("agent-a"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status.status, 200);
    assert_eq!(status.body["data"]["page"]["title"], "Interact");
    assert_eq!(status.body["data"]["tabs"].as_array().unwrap().len(), 1);

    let second_tab = http(
        addr,
        "POST",
        "/browser/tabs/create",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "url": format!("http://{web}/two") })),
    )
    .await;
    assert_eq!(second_tab.status, 200, "{}", second_tab.body);
    let tab_two = second_tab.body["data"]["tab_id"].as_u64().unwrap();
    assert_eq!(second_tab.body["data"]["tabs"].as_array().unwrap().len(), 2);

    let tabs = http(
        addr,
        "GET",
        "/browser/tabs",
        None,
        Some("agent-a"),
        Some(&session),
        None,
    )
    .await;
    let tab_one = tabs.body["data"]["tabs"][0]["tab_id"].as_u64().unwrap();
    let switched = http(
        addr,
        "POST",
        "/browser/tabs/switch",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "tab_id": tab_one })),
    )
    .await;
    assert_eq!(switched.status, 200);
    assert_eq!(switched.body["data"]["page"]["title"], "Interact");

    let closed_tab = http(
        addr,
        "POST",
        "/browser/tabs/close",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "tab_id": tab_two })),
    )
    .await;
    assert_eq!(closed_tab.status, 200);
    assert_eq!(closed_tab.body["data"]["closed"], true);
    assert_eq!(closed_tab.body["data"]["tabs"].as_array().unwrap().len(), 1);

    let snapshot = http(
        addr,
        "POST",
        "/browser/snapshot",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({})),
    )
    .await;
    assert_eq!(snapshot.status, 200, "{}", snapshot.body);
    assert!(
        snapshot.body["data"]["dom"]["html"]
            .as_str()
            .unwrap()
            .contains("id=\"btn\"")
    );
    assert_eq!(snapshot.body["data"]["accessibility"]["role"], "document");

    let extract = http(
        addr,
        "POST",
        "/browser/extract",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({})),
    )
    .await;
    assert_eq!(extract.status, 200);
    assert_eq!(extract.body["data"]["title"], "Interact");
    assert!(
        extract.body["data"]["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["text"] == "Go")
    );

    let hover = http(
        addr,
        "POST",
        "/browser/hover",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "selector": "#btn" })),
    )
    .await;
    assert_eq!(hover.status, 200, "{}", hover.body);
    let hovered = http(
        addr,
        "POST",
        "/browser/evaluate",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({
            "script": "document.getElementById('out').getAttribute('data-hover')"
        })),
    )
    .await;
    assert_eq!(hovered.body["data"]["result"], "yes");

    let click = http(
        addr,
        "POST",
        "/browser/click",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "selector": "#btn" })),
    )
    .await;
    assert_eq!(click.status, 200, "{}", click.body);

    let evaluated = http(
        addr,
        "POST",
        "/browser/evaluate",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({
            "script": "document.getElementById('out').getAttribute('data-count')"
        })),
    )
    .await;
    assert_eq!(evaluated.status, 200, "{}", evaluated.body);
    assert_eq!(evaluated.body["data"]["result"], "1");

    let typed = http(
        addr,
        "POST",
        "/browser/type",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "selector": "#name", "text": "hello" })),
    )
    .await;
    assert_eq!(typed.status, 200, "{}", typed.body);
    let value = http(
        addr,
        "POST",
        "/browser/evaluate",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "script": "document.getElementById('name').value" })),
    )
    .await;
    assert_eq!(value.body["data"]["result"], "hello");

    let pressed = http(
        addr,
        "POST",
        "/browser/press",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "key": "Enter" })),
    )
    .await;
    assert_eq!(pressed.status, 200, "{}", pressed.body);
    let entered = http(
        addr,
        "POST",
        "/browser/evaluate",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({
            "script": "document.getElementById('out').getAttribute('data-enter')"
        })),
    )
    .await;
    assert_eq!(entered.body["data"]["result"], "yes");

    let screenshot = http(
        addr,
        "POST",
        "/browser/screenshot",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "format": "png" })),
    )
    .await;
    assert_eq!(screenshot.status, 200, "{}", screenshot.body);
    assert!(screenshot.body["data"]["data"].as_str().unwrap().len() > 100);
    assert_eq!(screenshot.body["data"]["width"].as_u64(), Some(800));

    let network = http(
        addr,
        "GET",
        "/browser/network",
        None,
        Some("agent-a"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(network.status, 200);
    assert!(
        network.body["data"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["url"].as_str().unwrap_or("").ends_with("/interact"))
    );

    let saved = http(
        addr,
        "POST",
        "/browser/session/save",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "name": "snap" })),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);

    let navigated = http(
        addr,
        "POST",
        "/browser/navigate",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "url": format!("http://{web}/two") })),
    )
    .await;
    assert_eq!(navigated.status, 200, "{}", navigated.body);
    assert_eq!(navigated.body["data"]["title"], "Two");

    let back = http(
        addr,
        "POST",
        "/browser/back",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({})),
    )
    .await;
    assert_eq!(back.status, 200);
    assert_eq!(back.body["data"]["changed"], true);
    assert_eq!(back.body["data"]["page"]["title"], "Interact");

    let restored = http(
        addr,
        "POST",
        "/browser/session/restore",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "name": "snap" })),
    )
    .await;
    assert_eq!(restored.status, 200, "{}", restored.body);
    assert_eq!(restored.body["data"]["restored"], true);
    assert_eq!(restored.body["data"]["tabs"].as_array().unwrap().len(), 1);

    let scrolled_page = http(
        addr,
        "POST",
        "/browser/navigate",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "url": format!("http://{web}/scroll-page") })),
    )
    .await;
    assert_eq!(scrolled_page.status, 200, "{}", scrolled_page.body);
    let scrolled = http(
        addr,
        "POST",
        "/browser/scroll",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "dy": 400 })),
    )
    .await;
    assert_eq!(scrolled.status, 200, "{}", scrolled.body);
    assert!(
        scrolled.body["data"]["scroll_y"].as_f64().unwrap() > 0.0,
        "{}",
        scrolled.body
    );

    let closed = http(
        addr,
        "POST",
        "/browser/close",
        None,
        Some("agent-a"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(closed.status, 200);
    assert_eq!(closed.body["data"]["closed"], true);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn api_requires_authentication_and_isolates_agents() {
    let web = spawn_test_server().await;
    let (addr, _service, data_dir) = spawn_api(Some("secret")).await;

    let unauthorized = http(
        addr,
        "POST",
        "/browser/create",
        None,
        Some("agent-a"),
        None,
        Some(json!({ "url": format!("http://{web}/one") })),
    )
    .await;
    assert_eq!(unauthorized.status, 401);
    assert_eq!(unauthorized.body["ok"], false);
    assert_eq!(unauthorized.body["error"]["code"], "unauthorized");

    let wrong = http(
        addr,
        "POST",
        "/browser/create",
        Some("nope"),
        Some("agent-a"),
        None,
        Some(json!({ "url": format!("http://{web}/one") })),
    )
    .await;
    assert_eq!(wrong.status, 401);

    let created = http(
        addr,
        "POST",
        "/browser/create",
        Some("secret"),
        Some("agent-a"),
        None,
        Some(json!({ "url": format!("http://{web}/one") })),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);
    let session = created.body["data"]["session_id"].as_str().unwrap();

    let foreign = http(
        addr,
        "GET",
        "/browser/status",
        Some("secret"),
        Some("agent-b"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(foreign.status, 403);
    assert_eq!(foreign.body["error"]["code"], "forbidden");

    let other_create = http(
        addr,
        "POST",
        "/browser/create",
        Some("secret"),
        Some("agent-b"),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(other_create.status, 200);
    let other_session = other_create.body["data"]["session_id"].as_str().unwrap();
    assert_ne!(session, other_session);

    let blank_eval = http(
        addr,
        "POST",
        "/browser/evaluate",
        Some("secret"),
        Some("agent-b"),
        Some(other_session),
        Some(json!({ "script": "1 + 1" })),
    )
    .await;
    assert_eq!(blank_eval.status, 200, "{}", blank_eval.body);
    assert_eq!(blank_eval.body["data"]["result"].as_f64(), Some(2.0));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn mcp_lists_tools_and_executes_browser_calls() {
    let web = spawn_test_server().await;
    let (_addr, service, data_dir) = spawn_api(None).await;
    let auth = service.local_auth("mcp-test");

    let initialize = nexium::mcp::handle_request(
        &service,
        &auth,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" }
        }),
    )
    .await
    .unwrap();
    assert_eq!(initialize["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(initialize["result"]["serverInfo"]["name"], "nexium");

    let listed = nexium::mcp::handle_request(
        &service,
        &auth,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    )
    .await
    .unwrap();
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for expected in [
        "browser_create",
        "browser_close",
        "browser_navigate",
        "browser_click",
        "browser_type",
        "browser_press",
        "browser_scroll",
        "browser_hover",
        "browser_screenshot",
        "browser_snapshot",
        "browser_extract",
        "browser_wait",
        "browser_tabs",
        "browser_switch_tab",
        "browser_session_save",
        "browser_session_restore",
        "browser_evaluate",
        "browser_network",
        "browser_status",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }

    let created = nexium::mcp::handle_request(&service, &auth, &json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "browser_create",
            "arguments": { "url": format!("http://{web}/interact"), "viewport": { "width": 800, "height": 600 } }
        }
    }))
    .await
    .unwrap();
    assert_eq!(created["result"]["isError"], false, "{created}");
    let session = created["result"]["structuredContent"]["session_id"]
        .as_str()
        .unwrap();
    assert!(!session.is_empty());

    let click = nexium::mcp::handle_request(
        &service,
        &auth,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "browser_click",
                "arguments": { "session_id": session, "selector": "#btn" }
            }
        }),
    )
    .await
    .unwrap();
    assert_eq!(click["result"]["isError"], false, "{click}");

    let evaluated = nexium::mcp::handle_request(
        &service,
        &auth,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "browser_evaluate",
                "arguments": {
                    "session_id": session,
                    "script": "document.getElementById('out').getAttribute('data-count')"
                }
            }
        }),
    )
    .await
    .unwrap();
    assert_eq!(evaluated["result"]["structuredContent"]["result"], "1");

    let missing_tool = nexium::mcp::handle_request(
        &service,
        &auth,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": { "name": "browser_unknown", "arguments": {} }
        }),
    )
    .await
    .unwrap();
    assert_eq!(missing_tool["result"]["isError"], true);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn websocket_accepts_commands_and_streams_events() {
    let web = spawn_test_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws?agent=ws-agent"))
            .await
            .unwrap();

    let connected = socket.next().await.unwrap().unwrap();
    let connected: Value = serde_json::from_str(connected.to_text().unwrap()).unwrap();
    assert_eq!(connected["event"], "connected");

    socket
        .send(Message::Text(
            json!({
                "id": 1,
                "command": "create",
                "params": {
                    "url": format!("http://{web}/interact"),
                    "viewport": { "width": 800, "height": 600 }
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let mut events = Vec::new();
    let mut response = None;
    for _ in 0..30 {
        let message = socket.next().await.unwrap().unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value.get("ok").is_some() {
            response = Some(value);
            break;
        }
        if let Some(event) = value.get("event").and_then(Value::as_str) {
            events.push(event.to_owned());
        }
    }
    let response = response.expect("create response");
    assert_eq!(response["ok"], true, "{response}");
    let session = response["data"]["session_id"].as_str().unwrap().to_owned();
    for _ in 0..20 {
        let Ok(Some(Ok(message))) =
            tokio::time::timeout(std::time::Duration::from_millis(200), socket.next()).await
        else {
            break;
        };
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if let Some(event) = value.get("event").and_then(Value::as_str) {
            events.push(event.to_owned());
            if event == "session_created" {
                break;
            }
        }
    }
    assert!(events.contains(&"session_created".to_owned()), "{events:?}");

    socket
        .send(Message::Text(
            json!({
                "id": 2,
                "command": "navigate",
                "session_id": session,
                "params": { "url": format!("http://{web}/two") }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let mut events = Vec::new();
    let mut response = None;
    for _ in 0..50 {
        let message = socket.next().await.unwrap().unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value.get("id").and_then(Value::as_u64) == Some(2) && value.get("ok").is_some() {
            response = Some(value);
            break;
        }
        if let Some(event) = value.get("event").and_then(Value::as_str) {
            events.push(event.to_owned());
        }
    }
    let response = response.expect("navigate response");
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["data"]["title"], "Two");
    for _ in 0..20 {
        if events.contains(&"navigation_started".to_owned())
            && events.contains(&"navigation_completed".to_owned())
        {
            break;
        }
        let Ok(Some(Ok(message))) =
            tokio::time::timeout(std::time::Duration::from_millis(200), socket.next()).await
        else {
            break;
        };
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if let Some(event) = value.get("event").and_then(Value::as_str) {
            events.push(event.to_owned());
        }
    }
    assert!(
        events.contains(&"navigation_started".to_owned()),
        "{events:?}"
    );
    assert!(
        events.contains(&"navigation_completed".to_owned()),
        "{events:?}"
    );

    socket.close(None).await.unwrap();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn event_stream_reports_navigation_console_and_network() {
    let web = spawn_test_server().await;
    let (_addr, service, data_dir) = spawn_api(None).await;
    let auth = service.local_auth("event-agent");
    let mut events = service.subscribe();

    let created = service.create(&auth, Default::default()).await.unwrap();
    let session = created["session_id"].as_str().unwrap().to_owned();

    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event.event);
    }
    assert!(seen.contains(&"session_created".to_owned()));

    service
        .navigate(&auth, Some(&session), &format!("http://{web}/interact"))
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut network = false;
    while let Ok(event) = events.try_recv() {
        if event.event == "network" {
            network = true;
        }
        seen.push(event.event);
    }
    assert!(seen.contains(&"navigation_started".to_owned()), "{seen:?}");
    assert!(
        seen.contains(&"navigation_completed".to_owned()),
        "{seen:?}"
    );
    assert!(network, "{seen:?}");

    let _ = std::fs::remove_dir_all(data_dir);
}
