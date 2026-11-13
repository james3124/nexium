//! WebDriver and CDP protocol adapter tests (in-process).

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use nexium::ApiService;
use nexium::Config;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

fn temp_dir() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("nexium-wd-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn spawn_api() -> (SocketAddr, Arc<ApiService>, std::path::PathBuf) {
    let data_dir = temp_dir();
    let config = Config {
        data_dir: data_dir.clone(),
        ..Config::default()
    };
    let service = ApiService::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = Arc::clone(&service);
    tokio::spawn(async move {
        nexium::api::serve_on(listener, server).await.unwrap();
    });
    (addr, service, data_dir)
}

async fn raw_call(addr: SocketAddr, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    raw_call_session(addr, method, path, None, body).await
}

async fn raw_call_session(
    addr: SocketAddr,
    method: &str,
    path: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let session_header = session
        .map(|session| format!("X-Nexium-Session: {session}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nX-Nexium-Agent: webdriver\r\nContent-Type: application/json\r\n{session_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let json = serde_json::from_str(body).unwrap_or(Value::Null);
    (status, json)
}

async fn read_response(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
) -> Value {
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let value: Value = serde_json::from_str(&text).unwrap();
        if value["id"] == id {
            return value;
        }
    }
    panic!("no response for id {id}");
}

#[tokio::test]
async fn webdriver_subset_controls_a_page() {
    let page_addr = common::spawn_test_server().await;
    let (addr, _service, data_dir) = spawn_api().await;

    let (status, body) = raw_call(addr, "GET", "/status", None).await;
    assert_eq!(status, 200);
    assert_eq!(body["value"]["ready"], true);

    let (_, created) = raw_call(addr, "POST", "/session", Some(json!({}))).await;
    let session = created["value"]["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(created["value"]["capabilities"]["browserName"], "nexiumx");

    let url = format!("http://{page_addr}/");
    let (status, _) = raw_call(
        addr,
        "POST",
        &format!("/session/{session}/url"),
        Some(json!({ "url": url })),
    )
    .await;
    assert_eq!(status, 200);

    let (_, current) = raw_call(addr, "GET", &format!("/session/{session}/url"), None).await;
    assert_eq!(current["value"], url);
    let (_, title) = raw_call(addr, "GET", &format!("/session/{session}/title"), None).await;
    assert_eq!(title["value"], "Test Page");
    let (_, source) = raw_call(addr, "GET", &format!("/session/{session}/source"), None).await;
    assert!(
        source["value"]
            .as_str()
            .unwrap_or_default()
            .contains("<h1>Hello</h1>")
    );
    let (_, shot) = raw_call(addr, "GET", &format!("/session/{session}/screenshot"), None).await;
    assert!(shot["value"].as_str().unwrap_or_default().len() > 500);

    let (_, element) = raw_call(
        addr,
        "POST",
        &format!("/session/{session}/element"),
        Some(json!({ "using": "css selector", "value": "h1" })),
    )
    .await;
    let element = element["value"].as_str().unwrap().to_owned();
    assert!(element.starts_with('e'));

    let (_, text) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/element/{element}/text"),
        None,
    )
    .await;
    assert_eq!(text["value"], "Hello");

    let (_, executed) = raw_call(
        addr,
        "POST",
        &format!("/session/{session}/execute/sync"),
        Some(json!({ "script": "return 2 + 2;", "args": [] })),
    )
    .await;
    assert_eq!(executed["value"], 4.0);

    // DOM inspection over REST.
    let (_, dom) = raw_call_session(addr, "GET", "/browser/dom", Some(&session), None).await;
    assert_eq!(dom["data"]["nodeName"], "#document");
    assert!(
        dom["data"]["children"]
            .as_array()
            .is_some_and(|children| !children.is_empty())
    );

    // Selenium element state commands.
    let (_, intro) = raw_call(
        addr,
        "POST",
        &format!("/session/{session}/element"),
        Some(json!({ "using": "css selector", "value": ".intro" })),
    )
    .await;
    let intro = intro["value"].as_str().unwrap().to_owned();
    let (_, class) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/element/{intro}/attribute/class"),
        None,
    )
    .await;
    assert_eq!(class["value"], "intro");
    let (_, displayed) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/element/{intro}/displayed"),
        None,
    )
    .await;
    assert_eq!(displayed["value"], true);
    let (_, enabled) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/element/{intro}/enabled"),
        None,
    )
    .await;
    assert_eq!(enabled["value"], true);
    let (_, name) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/element/{intro}/name"),
        None,
    )
    .await;
    assert_eq!(name["value"], "p");
    let (_, rect) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/element/{intro}/rect"),
        None,
    )
    .await;
    assert!(rect["value"]["width"].as_f64().unwrap_or(0.0) > 0.0);

    // Window rect and cookies.
    let (_, window) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/window/rect"),
        None,
    )
    .await;
    assert!(window["value"]["width"].as_f64().unwrap_or(0.0) >= 1.0);
    raw_call(
        addr,
        "POST",
        &format!("/session/{session}/window/rect"),
        Some(json!({ "width": 1024, "height": 768 })),
    )
    .await;
    let (_, resized) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/window/rect"),
        None,
    )
    .await;
    assert_eq!(resized["value"]["width"], 1024.0);

    raw_call(
        addr,
        "POST",
        &format!("/session/{session}/cookie"),
        Some(json!({ "cookie": { "name": "wd", "value": "1" } })),
    )
    .await;
    let (_, cookies) = raw_call(addr, "GET", &format!("/session/{session}/cookie"), None).await;
    assert!(
        cookies["value"]
            .as_array()
            .is_some_and(|cookies| cookies.iter().any(|cookie| cookie["name"] == "wd"))
    );
    raw_call(addr, "DELETE", &format!("/session/{session}/cookie"), None).await;

    let (_, timeouts) = raw_call(addr, "GET", &format!("/session/{session}/timeouts"), None).await;
    assert_eq!(timeouts["value"]["pageLoad"], 30000);

    // Window handles and new tab.
    let (_, handles) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/window/handles"),
        None,
    )
    .await;
    let handles = handles["value"].as_array().unwrap().clone();
    assert!(!handles.is_empty());
    let (_, created) = raw_call(
        addr,
        "POST",
        &format!("/session/{session}/window/new"),
        Some(json!({ "type": "tab" })),
    )
    .await;
    assert!(created["value"]["handle"].as_u64().is_some());
    let (_, handles) = raw_call(
        addr,
        "GET",
        &format!("/session/{session}/window/handles"),
        None,
    )
    .await;
    assert_eq!(handles["value"].as_array().unwrap().len(), 2);
    let (_, switched) = raw_call(
        addr,
        "POST",
        &format!("/session/{session}/window"),
        Some(json!({ "handle": handles["value"][0] })),
    )
    .await;
    assert_eq!(switched["value"], Value::Null);

    let (status, _) = raw_call(addr, "DELETE", &format!("/session/{session}"), None).await;
    assert_eq!(status, 200);
    let (status, gone) = raw_call(addr, "GET", &format!("/session/{session}/title"), None).await;
    assert_eq!(status, 404);
    assert_eq!(gone["value"]["error"], "invalid session id");

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn cdp_discovery_endpoints_and_target_close() {
    let page_addr = common::spawn_test_server().await;
    let (addr, _service, data_dir) = spawn_api().await;

    let (_, list) = raw_call(addr, "GET", "/json", None).await;
    assert!(list.is_array());

    let (_, protocol) = raw_call(addr, "GET", "/json/protocol", None).await;
    let domains = protocol["domains"].as_array().unwrap();
    assert!(domains.len() >= 10);
    assert!(domains.iter().any(|domain| domain["domain"] == "DOM"));
    assert!(domains.iter().any(|domain| domain["domain"] == "Input"));

    let (_, created) = raw_call(
        addr,
        "PUT",
        &format!("/json/new?url=http://{page_addr}/"),
        None,
    )
    .await;
    let target = created["id"].as_str().unwrap().to_owned();

    let (status, _) = raw_call(addr, "GET", &format!("/json/activate/{target}"), None).await;
    assert_eq!(status, 200);

    let (status, _) = raw_call(addr, "GET", &format!("/json/close/{target}"), None).await;
    assert_eq!(status, 200);
    let (_, list) = raw_call(addr, "GET", "/json/list", None).await;
    assert!(
        !list
            .as_array()
            .is_some_and(|targets| targets.iter().any(|item| item["id"] == target))
    );

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn bidi_session_controls_page_and_emits_events() {
    let page_addr = common::spawn_test_server().await;
    let (addr, _service, data_dir) = spawn_api().await;

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/session"))
        .await
        .unwrap();

    socket
        .send(Message::Text(
            json!({ "id": 1, "method": "session.new", "params": { "capabilities": {} } })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let created = read_response(&mut socket, 1).await;
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert!(session.starts_with("sess-"));

    socket
        .send(Message::Text(
            json!({ "id": 2, "method": "browsingContext.getTree", "params": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let tree = read_response(&mut socket, 2).await;
    let contexts = tree["result"]["contexts"].as_array().unwrap();
    assert!(!contexts.is_empty());
    let context = contexts[0]["context"].as_str().unwrap().to_owned();

    socket
        .send(Message::Text(
            json!({
                "id": 3,
                "method": "browsingContext.navigate",
                "params": { "context": context, "url": format!("http://{page_addr}/one"), "wait": "complete" }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let navigated = read_response(&mut socket, 3).await;
    assert!(navigated["result"]["navigation"].is_string());

    let mut saw_load = false;
    for _ in 0..20 {
        let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout(Duration::from_secs(2), socket.next()).await
        else {
            break;
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        if event["method"] == "browsingContext.load" {
            saw_load = true;
        }
        if saw_load {
            break;
        }
    }
    assert!(saw_load, "expected browsingContext.load event");

    socket
        .send(Message::Text(
            json!({
                "id": 4,
                "method": "script.evaluate",
                "params": { "expression": "6 * 7", "target": { "context": context }, "awaitPromise": false }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let evaluated = read_response(&mut socket, 4).await;
    assert_eq!(evaluated["result"]["result"]["value"].as_f64(), Some(42.0));

    socket
        .send(Message::Text(
            json!({
                "id": 5,
                "method": "script.callFunction",
                "params": { "functionDeclaration": "() => 40 + 2", "target": { "context": context } }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let called = read_response(&mut socket, 5).await;
    assert_eq!(called["result"]["result"]["value"].as_f64(), Some(42.0));

    socket
        .send(Message::Text(
            json!({ "id": 6, "method": "session.end", "params": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let ended = read_response(&mut socket, 6).await;
    assert_eq!(ended["type"], "success");

    let _ = socket.close(None).await;
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn cdp_discovery_and_runtime_evaluate() {
    let page_addr = common::spawn_test_server().await;
    let (addr, _service, data_dir) = spawn_api().await;

    let (_, version) = raw_call(addr, "GET", "/json/version", None).await;
    assert!(
        version["Browser"]
            .as_str()
            .unwrap_or_default()
            .starts_with("NexiumX/")
    );

    let (_, created) = raw_call(
        addr,
        "PUT",
        &format!("/json/new?url=http://{page_addr}/"),
        None,
    )
    .await;
    let target = created["id"].as_str().unwrap().to_owned();
    assert!(target.starts_with("sess-"));

    let (_, list) = raw_call(addr, "GET", "/json/list", None).await;
    assert!(
        list.as_array()
            .is_some_and(|targets| targets.iter().any(|item| item["id"] == target))
    );

    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/{target}"))
            .await
            .unwrap();

    socket
        .send(Message::Text(
            json!({
                "id": 1,
                "method": "Runtime.evaluate",
                "params": { "expression": "6 * 7", "returnByValue": true }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(response["result"]["result"]["value"], 42.0);

    // DOM inspection domain.
    socket
        .send(Message::Text(
            json!({ "id": 10, "method": "DOM.getDocument" })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let document = read_response(&mut socket, 10).await;
    let node_type = document["result"]["root"]["nodeType"].as_u64().unwrap();
    let root = document["result"]["root"]["nodeId"].as_u64().unwrap();
    assert_eq!(node_type, 9);

    socket
        .send(Message::Text(
            json!({
                "id": 11,
                "method": "DOM.querySelector",
                "params": { "nodeId": root, "selector": "p.intro" }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let query = read_response(&mut socket, 11).await;
    let node = query["result"]["nodeId"].as_u64().unwrap();
    assert!(node > 0);

    socket
        .send(Message::Text(
            json!({
                "id": 12,
                "method": "DOM.getAttributes",
                "params": { "nodeId": node }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let attributes = read_response(&mut socket, 12).await;
    let flat = attributes["result"]["attributes"].as_array().unwrap();
    assert!(flat.iter().any(|value| value == "class"));
    assert!(flat.iter().any(|value| value == "intro"));

    socket
        .send(Message::Text(
            json!({
                "id": 13,
                "method": "DOM.getOuterHTML",
                "params": { "nodeId": node }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let outer = read_response(&mut socket, 13).await;
    assert!(String::from(outer["result"]["outerHTML"].as_str().unwrap()).contains("intro"));

    socket
        .send(Message::Text(
            json!({
                "id": 14,
                "method": "DOM.getBoxModel",
                "params": { "nodeId": node }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let box_model = read_response(&mut socket, 14).await;
    assert!(box_model["result"]["model"]["width"].as_f64().unwrap() > 0.0);

    socket
        .send(Message::Text(
            json!({
                "id": 15,
                "method": "DOM.resolveNode",
                "params": { "nodeId": node }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let resolved = read_response(&mut socket, 15).await;
    let object_id = resolved["result"]["object"]["objectId"]
        .as_str()
        .unwrap()
        .to_owned();
    socket
        .send(Message::Text(
            json!({
                "id": 16,
                "method": "Runtime.callFunctionOn",
                "params": {
                    "objectId": object_id,
                    "functionDeclaration": "function(){ return this.tagName; }",
                    "returnByValue": true
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let tag = read_response(&mut socket, 16).await;
    assert_eq!(tag["result"]["result"]["value"], "P");

    socket
        .send(Message::Text(
            json!({
                "id": 17,
                "method": "Input.dispatchKeyEvent",
                "params": { "type": "keyDown", "key": "Escape" }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let input = read_response(&mut socket, 17).await;
    assert!(input["result"].is_object());

    socket
        .send(Message::Text(
            json!({
                "id": 18,
                "method": "Emulation.setDeviceMetricsOverride",
                "params": { "width": 1024, "height": 768, "deviceScaleFactor": 1, "mobile": false }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    read_response(&mut socket, 18).await;
    socket
        .send(Message::Text(
            json!({ "id": 19, "method": "Page.getLayoutMetrics" })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let metrics = read_response(&mut socket, 19).await;
    assert_eq!(metrics["result"]["contentSize"]["width"], 1024.0);

    socket
        .send(Message::Text(
            json!({
                "id": 2,
                "method": "Page.navigate",
                "params": { "url": format!("http://{page_addr}/one") }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let mut navigated = Value::Null;
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let message: Value = serde_json::from_str(&text).unwrap();
        if message["id"] == 2 {
            navigated = message;
            break;
        }
    }
    assert!(navigated["result"]["frameId"].is_string());

    socket.close(None).await.unwrap();
    let _ = raw_call(addr, "DELETE", &format!("/session/{target}"), None).await;
    let _ = std::fs::remove_dir_all(data_dir);
}
