//! MCP tool coverage for the vision subsystem.
//!
//! Mirrors `tests/api_tests.rs`: build an `ApiService` with an isolated data
//! directory, authenticate locally, and drive `mcp::handle_request` against a
//! local HTML fixture page.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use nexium::api::service::{CreateOptions, ViewportOptions};
use nexium::{ApiService, AuthContext, Config};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static COUNTER: AtomicU64 = AtomicU64::new(0);

const VISION_HTML: &str = r#"<!doctype html><html><head><title>Vision MCP</title><style>
 body { margin: 0; font-family: sans-serif; }
 #primary { position: absolute; left: 20px; top: 40px; width: 160px; height: 44px; background: #2ea043; color: #fff; border: 0; }
 #profile { position: absolute; left: 20px; top: 110px; }
 #out { position: absolute; left: 20px; top: 160px; }
</style></head><body>
 <h1>Vision MCP test page</h1>
 <button id="primary">Submit <span>order</span></button>
 <a id="profile" href="/destination">Open profile</a>
 <div id="out" data-activated="no">idle</div>
 <script>
 document.getElementById('primary').addEventListener('click', function () {
   var out = document.getElementById('out');
   out.setAttribute('data-activated', 'yes');
   out.textContent = 'order submitted';
 });
 </script>
</body></html>"#;

const DESTINATION_HTML: &str =
    "<html><head><title>Destination</title></head><body><h1>Destination reached</h1></body></html>";

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-vision-mcp-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn service_with_data_dir(data_dir: PathBuf) -> Arc<ApiService> {
    ApiService::new(Config {
        data_dir,
        ..Config::default()
    })
}

/// Small HTML server so the visual index sees real, styled elements.
async fn spawn_vision_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(serve_vision_page(stream));
        }
    });
    addr
}

async fn serve_vision_page(mut stream: tokio::net::TcpStream) {
    let mut buffer = vec![0u8; 16 * 1024];
    let Ok(size) = stream.read(&mut buffer).await else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..size]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/");
    let body = if path == "/destination" {
        DESTINATION_HTML
    } else {
        VISION_HTML
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn session_on(service: &Arc<ApiService>, auth: &AuthContext, url: String) -> String {
    let created = service
        .create(
            auth,
            CreateOptions {
                url: Some(url),
                viewport: Some(ViewportOptions {
                    width: 800,
                    height: 600,
                }),
                ..CreateOptions::default()
            },
        )
        .await
        .unwrap();
    created["session_id"].as_str().unwrap().to_owned()
}

async fn mcp(service: &Arc<ApiService>, auth: &AuthContext, request: Value) -> Value {
    nexium::mcp::handle_request(service, auth, &request)
        .await
        .expect("JSON-RPC response")
}

async fn call(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    id: u64,
    name: &str,
    arguments: Value,
) -> Value {
    mcp(
        service,
        auth,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }),
    )
    .await
}

fn success(response: &Value) -> &Value {
    assert!(
        !response["result"]["isError"].as_bool().unwrap_or(true),
        "tool call failed: {response}"
    );
    &response["result"]["structuredContent"]
}

fn tool_named<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("missing tool {name}"))
}

fn decode_base64(value: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value.as_str().expect("base64 payload"))
        .expect("decodable base64")
}

#[tokio::test]
async fn tools_list_advertises_vision_tools_with_schemas() {
    let data_dir = temp_dir("list");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");

    let listed = mcp(
        &service,
        &auth,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .await;
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let expected = [
        "vision_snapshot",
        "vision_analyze",
        "vision_extract_text",
        "vision_find_element",
        "vision_click_target",
        "vision_compare",
    ];
    for name in expected {
        assert!(names.contains(&name), "missing tool {name}");
    }
    // The `snapshot` alias is callable but must not be advertised twice.
    assert!(
        !names.contains(&"snapshot"),
        "alias listed as a separate tool"
    );

    for name in expected {
        let schema = &tool_named(tools, name)["inputSchema"];
        assert_eq!(schema["type"], "object", "schema for {name}");
        assert_eq!(schema["additionalProperties"], false, "schema for {name}");
        let properties = schema["properties"].as_object().expect("properties object");
        assert!(
            properties.contains_key("session_id"),
            "{name} lacks session_id"
        );
        assert_eq!(properties["session_id"]["type"], "string");
    }

    let snapshot = &tool_named(tools, "vision_snapshot")["inputSchema"]["properties"];
    for property in ["full_page", "include_visual"] {
        assert_eq!(snapshot[property]["type"], "boolean");
    }
    assert_eq!(snapshot["quality"]["type"], "integer");
    assert_eq!(snapshot["format"]["type"], "string");

    let analyze = &tool_named(tools, "vision_analyze")["inputSchema"]["properties"];
    assert_eq!(analyze["region"]["type"], "object");
    assert_eq!(analyze["full_page"]["type"], "boolean");

    let extract = &tool_named(tools, "vision_extract_text")["inputSchema"]["properties"];
    assert_eq!(extract["query"]["type"], "string");
    assert_eq!(extract["region"]["type"], "object");

    for name in ["vision_find_element", "vision_click_target"] {
        let schema = &tool_named(tools, name)["inputSchema"];
        assert_eq!(schema["required"], json!(["description"]));
        assert_eq!(schema["properties"]["description"]["type"], "string");
    }
    let find = &tool_named(tools, "vision_find_element")["inputSchema"]["properties"];
    assert_eq!(find["top_k"]["type"], "integer");
    assert_eq!(find["region"]["type"], "object");
    let click = &tool_named(tools, "vision_click_target")["inputSchema"]["properties"];
    for property in ["execute", "verify"] {
        assert_eq!(click[property]["type"], "boolean");
    }
    assert_eq!(click["button"]["type"], "string");

    let compare = &tool_named(tools, "vision_compare")["inputSchema"]["properties"];
    assert_eq!(compare["threshold"]["type"], "integer");
    assert_eq!(compare["min_pixels"]["type"], "integer");
    assert_eq!(compare["save_artifacts"]["type"], "boolean");

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_snapshot_and_snapshot_alias_return_image_and_visual_index() {
    let web = spawn_vision_server().await;
    let data_dir = temp_dir("snapshot");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");
    let session = session_on(&service, &auth, format!("http://{web}/")).await;

    let response = call(
        &service,
        &auth,
        1,
        "vision_snapshot",
        json!({ "session_id": session, "format": "jpeg", "quality": 80 }),
    )
    .await;
    let data = success(&response);
    assert_eq!(data["format"], "jpeg");
    assert_eq!(data["width"], 800.0);
    assert_eq!(data["height"], 600.0);
    assert!(data["bytes"].as_u64().unwrap() > 0);
    let jpeg = decode_base64(&data["data"]);
    assert_eq!(&jpeg[..2], &[0xff, 0xd8], "JPEG magic bytes");
    assert!(!data["elements"].as_array().unwrap().is_empty());
    assert!(!data["text_runs"].as_array().unwrap().is_empty());
    assert_eq!(data["viewport"]["width"], 800.0);
    assert_eq!(data["scroll_x"], 0.0);
    assert_eq!(data["scroll_y"], 0.0);

    let alias = call(
        &service,
        &auth,
        2,
        "snapshot",
        json!({ "session_id": session, "full_page": true, "include_visual": false }),
    )
    .await;
    let data = success(&alias);
    assert_eq!(data["format"], "png");
    assert!(
        data.get("elements").is_none(),
        "include_visual=false omits elements"
    );
    assert!(data.get("text_runs").is_none());
    assert!(data["height"].as_u64().unwrap() >= 600);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_analyze_returns_elements_text_clickable_regions_and_png() {
    let web = spawn_vision_server().await;
    let data_dir = temp_dir("analyze");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");
    let session = session_on(&service, &auth, format!("http://{web}/")).await;

    let response = call(
        &service,
        &auth,
        1,
        "vision_analyze",
        json!({
            "session_id": session,
            "region": { "x": 0, "y": 0, "width": 400, "height": 200 }
        }),
    )
    .await;
    let data = success(&response);
    let elements = data["elements"].as_array().expect("elements array");
    assert!(!elements.is_empty());
    assert!(elements.iter().any(|element| {
        element["label"] == "Submit order" && element["clickable"].as_bool() == Some(true)
    }));
    assert!(!data["text_runs"].as_array().unwrap().is_empty());
    let clickable = data["clickable_regions"]
        .as_array()
        .expect("clickable regions");
    assert!(clickable.iter().any(|item| item["label"] == "Submit order"));
    assert_eq!(data["viewport"]["width"], 800.0);
    assert_eq!(data["scroll"]["y"], 0.0);
    let png = decode_base64(&data["image"]);
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_extract_text_reports_runs_and_phrase_matches() {
    let web = spawn_vision_server().await;
    let data_dir = temp_dir("extract");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");
    let session = session_on(&service, &auth, format!("http://{web}/")).await;

    let response = call(
        &service,
        &auth,
        1,
        "vision_extract_text",
        json!({ "session_id": session, "query": "Submit order" }),
    )
    .await;
    let data = success(&response);
    let text = data["text"].as_str().unwrap();
    assert!(text.contains("Submit"), "unexpected text: {text}");
    assert!(text.contains("order"), "unexpected text: {text}");
    assert!(!data["runs"].as_array().unwrap().is_empty());
    let matches = data["matches"].as_array().expect("matches array");
    assert!(!matches.is_empty(), "cross-fragment phrase should match");
    assert_eq!(matches[0]["text"], "Submit order");
    assert_eq!(matches[0]["confidence"], 1.0);
    assert!(matches[0]["rect"]["width"].as_f64().unwrap() > 0.0);

    let no_query = call(
        &service,
        &auth,
        2,
        "vision_extract_text",
        json!({ "session_id": session }),
    )
    .await;
    let data = success(&no_query);
    assert_eq!(data["matches"], json!([]));
    assert!(!data["runs"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_find_element_ranks_the_described_button_and_rejects_empty_input() {
    let web = spawn_vision_server().await;
    let data_dir = temp_dir("find");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");
    let session = session_on(&service, &auth, format!("http://{web}/")).await;

    let response = call(
        &service,
        &auth,
        1,
        "vision_find_element",
        json!({
            "session_id": session,
            "description": "green Submit order button",
            "top_k": 3
        }),
    )
    .await;
    let data = success(&response);
    let matches = data["matches"].as_array().expect("matches array");
    assert!(!matches.is_empty());
    assert_eq!(matches[0]["element"]["tag"], "button");
    assert_eq!(matches[0]["element"]["label"], "Submit order");
    assert!(matches[0]["score"].as_f64().unwrap() > 0.5);
    assert!(!matches[0]["signals"].as_array().unwrap().is_empty());

    let invalid = call(
        &service,
        &auth,
        2,
        "vision_find_element",
        json!({ "session_id": session, "description": "   " }),
    )
    .await;
    assert!(
        invalid["result"]["isError"].as_bool() == Some(true),
        "empty description should fail: {invalid}"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_click_target_locates_clicks_and_verifies_a_change() {
    let web = spawn_vision_server().await;
    let data_dir = temp_dir("click");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");
    let session = session_on(&service, &auth, format!("http://{web}/")).await;

    let located = call(
        &service,
        &auth,
        1,
        "vision_click_target",
        json!({ "session_id": session, "description": "Open profile link" }),
    )
    .await;
    let data = success(&located);
    assert_eq!(data["label"], "Open profile");
    assert!(data["x"].as_f64().unwrap() > 0.0);
    assert!(data["y"].as_f64().unwrap() > 0.0);
    assert!(
        data.get("click_result").is_none(),
        "execute defaults to false"
    );

    let clicked = call(
        &service,
        &auth,
        2,
        "vision_click_target",
        json!({
            "session_id": session,
            "description": "Submit order button",
            "execute": true,
            "verify": true
        }),
    )
    .await;
    let data = success(&clicked);
    assert_eq!(data["label"], "Submit order");
    assert!(data["node_id"].as_u64().unwrap() > 0);
    assert!(data["confidence"].as_f64().unwrap() > 0.0);
    assert!(data["score"].as_f64().unwrap() > 0.0);
    assert!(!data["signals"].as_array().unwrap().is_empty());
    assert_eq!(data["click_result"]["element"]["tag"], "button");
    let verification = &data["verification"];
    assert!(verification["ratio"].as_f64().unwrap() > 0.0);
    assert!(
        !verification["changed_regions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        verification["text_added"]
            .as_array()
            .unwrap()
            .iter()
            .any(|text| text.as_str().unwrap_or("").contains("submitted")),
        "unexpected text diff: {verification}"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_compare_reports_shape_and_can_save_artifacts() {
    let web = spawn_vision_server().await;
    let data_dir = temp_dir("compare");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");
    let session = session_on(&service, &auth, format!("http://{web}/")).await;

    let response = call(
        &service,
        &auth,
        1,
        "vision_compare",
        json!({
            "session_id": session,
            "threshold": 8,
            "min_pixels": 40
        }),
    )
    .await;
    let data = success(&response);
    assert_eq!(data["ratio"], 0.0, "an unchanged page should not differ");
    assert_eq!(data["changed_regions"], json!([]));
    assert_eq!(data["text_added"], json!([]));
    assert_eq!(data["text_removed"], json!([]));

    let with_artifacts = call(
        &service,
        &auth,
        2,
        "vision_compare",
        json!({ "session_id": session, "save_artifacts": true }),
    )
    .await;
    let data = success(&with_artifacts);
    assert!(data["ratio"].is_number());
    assert!(data["changed_regions"].is_array());
    let artifacts = &data["artifacts"];
    assert!(
        artifacts["before"]["path"]
            .as_str()
            .unwrap()
            .ends_with(".png")
    );
    assert!(
        artifacts["after"]["url"]
            .as_str()
            .unwrap()
            .starts_with("/artifacts/")
    );
    assert!(std::path::Path::new(artifacts["before"]["path"].as_str().unwrap()).exists());

    let _ = std::fs::remove_dir_all(&data_dir);
}
