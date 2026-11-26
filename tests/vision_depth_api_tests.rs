//! End-to-end coverage for the vision depth wiring: selectable OCR backends,
//! pluggable element locators and the describe-act-verify `vision_act` surface
//! over REST and MCP.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use nexium::api::service::{CreateOptions, ViewportOptions};
use nexium::{ApiService, AuthContext, Config};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Canvas-only page: its text lives in pixels, not in the DOM layout index.
const CANVAS_HTML: &str = r#"<!doctype html><html><head><title>Canvas OCR</title><style>
body { margin: 0; background: #fff; font-family: sans-serif; }
</style></head><body>
<canvas id="paint" width="600" height="200" style="background:#fff"></canvas>
<script>
var ctx = document.getElementById('paint').getContext('2d');
ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, 600, 200);
ctx.fillStyle = '#000'; ctx.font = '48px sans-serif';
ctx.fillText('STOP', 20, 75);
// Draw the second word with overlapping glyph bitmaps: at 48px the normal
// advance is ~37px, so painting O at x=52 (85% of that advance) makes the
// glyphs touch while leaving a waist, reproducing the "UU" canvas blob.
ctx.fillText('G', 20, 150);
ctx.fillText('O', 52, 150);
</script>
</body></html>"#;

const ACTION_HTML: &str = r#"<!doctype html><html><head><title>Vision act</title><style>
body { margin: 0; font-family: sans-serif; }
#go { position: absolute; left: 20px; top: 40px; width: 160px; height: 44px; background: #2ea043; color: #fff; border: 0; }
#out { position: absolute; left: 20px; top: 120px; }
</style></head><body>
<h1>Act page</h1>
<button id="go">Submit order</button>
<div id="out">idle</div>
<script>
document.getElementById('go').addEventListener('click', function () {
  document.getElementById('out').textContent = 'order submitted';
});
</script>
</body></html>"#;

/// Two equally labelled buttons force a tied top-two score.
const AMBIGUOUS_HTML: &str = r#"<!doctype html><html><head><title>Ambiguous</title><style>
body { margin: 0; font-family: sans-serif; }
button { position: absolute; width: 160px; height: 44px; border: 0; }
#one { left: 20px; top: 40px; }
#two { left: 200px; top: 40px; }
#out { position: absolute; left: 20px; top: 120px; }
</style></head><body>
<h1>Ambiguous page</h1>
<button id="one">Submit order</button>
<button id="two">Submit order</button>
<div id="out">idle</div>
<script>
document.getElementById('one').addEventListener('click', function () {
  document.getElementById('out').textContent = 'order submitted';
});
document.getElementById('two').addEventListener('click', function () {
  document.getElementById('out').textContent = 'order submitted';
});
</script>
</body></html>"#;

/// The target starts well below a 400x200 viewport.
const OFFSCREEN_HTML: &str = r#"<!doctype html><html><head><title>Offscreen</title><style>
body { margin: 0; font-family: sans-serif; }
#spacer { height: 800px; }
#go { position: absolute; left: 20px; top: 520px; width: 160px; height: 44px; background: #2ea043; border: 0; }
#out { position: absolute; left: 20px; top: 600px; }
</style></head><body>
<div id="spacer"></div>
<button id="go">Submit order</button>
<div id="out">idle</div>
<script>
document.getElementById('go').addEventListener('click', function () {
  document.getElementById('out').textContent = 'order submitted';
});
</script>
</body></html>"#;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-vision-depth-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn spawn_api() -> (SocketAddr, Arc<ApiService>, PathBuf) {
    let data_dir = temp_dir("server");
    let service = ApiService::new(Config {
        data_dir: data_dir.clone(),
        ..Config::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = Arc::clone(&service);
    tokio::spawn(async move {
        nexium::api::serve_on(listener, server).await.unwrap();
    });
    (addr, service, data_dir)
}

fn service_with_data_dir(data_dir: PathBuf) -> Arc<ApiService> {
    ApiService::new(Config {
        data_dir,
        ..Config::default()
    })
}

async fn spawn_html_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(serve_page(stream));
        }
    });
    addr
}

async fn serve_page(mut stream: tokio::net::TcpStream) {
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
    let body = match path {
        "/canvas" => CANVAS_HTML,
        "/ambiguous" => AMBIGUOUS_HTML,
        "/offscreen" => OFFSCREEN_HTML,
        _ => ACTION_HTML,
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

struct HttpResponse {
    status: u16,
    body: Value,
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
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
    if let Some(agent) = agent {
        builder = builder.header("x-nexium-agent", agent);
    }
    if let Some(session) = session {
        builder = builder.header("x-nexium-session", session);
    }
    let request = builder.body(Full::new(Bytes::from(payload))).unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    HttpResponse { status, body }
}

async fn create_session(
    addr: SocketAddr,
    agent: &str,
    url: &str,
    viewport: Option<(u32, u32)>,
) -> String {
    let mut body = json!({ "url": url });
    if let Some((width, height)) = viewport {
        body["viewport"] = json!({ "width": width, "height": height });
    }
    let created = http(
        addr,
        "POST",
        "/browser/create",
        Some(agent),
        None,
        Some(body),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);
    created.body["data"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn post(
    addr: SocketAddr,
    agent: &str,
    session: &str,
    path: &str,
    body: Value,
) -> HttpResponse {
    http(addr, "POST", path, Some(agent), Some(session), Some(body)).await
}

async fn mcp(
    service: &Arc<ApiService>,
    auth: &AuthContext,
    id: u64,
    name: &str,
    arguments: Value,
) -> Value {
    nexium::mcp::handle_request(
        service,
        auth,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }),
    )
    .await
    .expect("JSON-RPC response")
}

#[tokio::test]
async fn glyph_ocr_adds_ocr_results_and_none_adds_none() {
    let web = spawn_html_server().await;
    let (addr, _service, data_dir) = spawn_api().await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/canvas"), None).await;

    let glyph = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/extract-text",
        json!({ "ocr_backend": "glyph", "session_id": session }),
    )
    .await;
    assert_eq!(glyph.status, 200, "{}", glyph.body);
    let data = &glyph.body["data"];
    let ocr = data["ocr"].as_array().expect("ocr array");
    assert!(!ocr.is_empty(), "glyph OCR found no painted text: {data}");
    assert!(ocr.iter().all(|item| item["source"] == "ocr"));
    assert!(ocr.iter().any(|item| {
        item["text"]
            .as_str()
            .unwrap_or("")
            .chars()
            .any(|ch| ch.is_ascii_alphabetic())
    }));
    let joined: String = ocr
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<String>()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    assert!(
        joined.contains("GO"),
        "tightly spaced canvas 'GO' was not read as G followed by O: {joined:?} (ocr: {data})"
    );
    assert!(ocr[0]["rect"]["width"].as_f64().unwrap() > 0.0);
    // The layout-grounded fields remain unchanged.
    assert!(data["text"].as_str().unwrap().trim().is_empty());
    assert_eq!(data["matches"], json!([]));

    for path in ["/browser/vision/analyze", "/browser/vision/snapshot"] {
        let response = post(
            addr,
            "agent-a",
            &session,
            path,
            json!({ "ocr_backend": "glyph", "session_id": session }),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        assert!(
            !response.body["data"]["ocr"].as_array().unwrap().is_empty(),
            "{path} did not include OCR results"
        );
    }

    for backend in [None, Some("none")] {
        let mut body = json!({ "session_id": session });
        if let Some(backend) = backend {
            body["ocr_backend"] = json!(backend);
        }
        let response = post(
            addr,
            "agent-a",
            &session,
            "/browser/vision/extract-text",
            body,
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        assert_eq!(response.body["data"]["ocr"], json!([]));
    }

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn unknown_ocr_backend_and_locator_are_bad_requests() {
    let web = spawn_html_server().await;
    let (addr, _service, data_dir) = spawn_api().await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/action"), None).await;

    let ocr = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/analyze",
        json!({ "ocr_backend": "mystery", "session_id": session }),
    )
    .await;
    assert_eq!(ocr.status, 400, "{}", ocr.body);
    assert_eq!(ocr.body["error"]["code"], "bad_request");
    assert!(
        ocr.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("none, glyph")
    );

    let locator = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/find-element",
        json!({
            "description": "Submit order button",
            "locator": "no-such-locator",
            "session_id": session,
        }),
    )
    .await;
    assert_eq!(locator.status, 400, "{}", locator.body);
    assert_eq!(locator.body["error"]["code"], "bad_request");
    assert!(
        locator.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("heuristic")
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_act_clicks_a_described_button_and_reports_verification() {
    let web = spawn_html_server().await;
    let (addr, _service, data_dir) = spawn_api().await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/action"), None).await;

    let response = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/act",
        json!({ "description": "Submit order button", "session_id": session }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let data = &response.body["data"];
    assert_eq!(data["action"], "click");
    assert_eq!(data["ambiguous"], false);
    assert_eq!(data["executed"], true);
    assert_eq!(data["target"]["label"], "Submit order");
    assert!(data["target"]["x"].as_f64().unwrap() > 0.0);
    assert!(data["target"]["y"].as_f64().unwrap() > 0.0);
    assert!(data["target"]["score"].as_f64().unwrap() > 0.5);
    assert_eq!(data["click_result"]["x"], data["target"]["x"]);

    let verification = &data["verification"];
    assert_eq!(verification["success_hint"], true, "{verification}");
    assert!(verification["changed_regions"].as_array().unwrap().len() > 0);
    assert!(
        verification["text_added"]
            .as_array()
            .unwrap()
            .iter()
            .any(|text| text.as_str().unwrap_or("").contains("submitted"))
    );

    let text = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/extract-text",
        json!({ "session_id": session }),
    )
    .await;
    assert!(
        text.body["data"]["text"]
            .as_str()
            .unwrap()
            .contains("order submitted")
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_act_ambiguous_case_does_not_click() {
    let web = spawn_html_server().await;
    let (addr, _service, data_dir) = spawn_api().await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/ambiguous"), None).await;

    let response = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/act",
        json!({ "description": "Submit order button", "session_id": session }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let data = &response.body["data"];
    assert_eq!(data["ambiguous"], true);
    assert_eq!(data["executed"], false);
    assert!(data.get("click_result").is_none());
    let candidates = data["candidates"].as_array().expect("candidates array");
    assert!(candidates.len() >= 2, "{data}");
    assert_eq!(candidates[0]["element"]["label"], "Submit order");
    assert_eq!(candidates[1]["element"]["label"], "Submit order");

    let text = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/extract-text",
        json!({ "session_id": session }),
    )
    .await;
    assert!(text.body["data"]["text"].as_str().unwrap().contains("idle"));
    assert!(
        !text.body["data"]["text"]
            .as_str()
            .unwrap()
            .contains("order submitted")
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_act_auto_scrolls_to_an_offscreen_target_then_clicks() {
    let web = spawn_html_server().await;
    let (addr, _service, data_dir) = spawn_api().await;
    let session = create_session(
        addr,
        "agent-a",
        &format!("http://{web}/offscreen"),
        Some((400, 200)),
    )
    .await;

    let response = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/act",
        json!({
            "description": "Submit order button",
            "max_attempts": 2,
            "session_id": session,
        }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let data = &response.body["data"];
    assert_eq!(data["ambiguous"], false, "{data}");
    assert_eq!(data["executed"], true);
    let y = data["target"]["y"].as_f64().unwrap();
    assert!(
        (0.0..=200.0).contains(&y),
        "target not scrolled into view: {data}"
    );
    assert_eq!(data["verification"]["success_hint"], true, "{data}");

    let text = post(
        addr,
        "agent-a",
        &session,
        "/browser/vision/extract-text",
        json!({ "session_id": session }),
    )
    .await;
    assert!(
        text.body["data"]["text"]
            .as_str()
            .unwrap()
            .contains("order submitted")
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn mcp_vision_act_returns_structured_content() {
    let web = spawn_html_server().await;
    let data_dir = temp_dir("mcp");
    let service = service_with_data_dir(data_dir.clone());
    let auth = service.local_auth("agent-a");
    let created = service
        .create(
            &auth,
            CreateOptions {
                url: Some(format!("http://{web}/action")),
                viewport: Some(ViewportOptions {
                    width: 800,
                    height: 600,
                }),
                ..CreateOptions::default()
            },
        )
        .await
        .unwrap();
    let session = created["session_id"].as_str().unwrap().to_owned();

    let response = mcp(
        &service,
        &auth,
        1,
        "vision_act",
        json!({
            "description": "Submit order button",
            "session_id": session,
            "verify": true,
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"], false, "{response}");
    let data = &response["result"]["structuredContent"];
    assert_eq!(data["action"], "click");
    assert_eq!(data["ambiguous"], false);
    assert_eq!(data["executed"], true);
    assert_eq!(data["target"]["label"], "Submit order");
    assert_eq!(data["verification"]["success_hint"], true, "{data}");

    let listed = nexium::mcp::handle_request(
        &service,
        &auth,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await
    .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "vision_act")
        .expect("vision_act tool is advertised");
    assert_eq!(tool["inputSchema"]["required"], json!(["description"]));
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(
        properties["action"]["enum"],
        json!(["click", "hover", "scroll_into_view"])
    );
    for property in [
        "verify",
        "threshold",
        "min_pixels",
        "locator",
        "max_attempts",
        "session_id",
    ] {
        assert!(properties.contains_key(property), "missing {property}");
    }
    assert!(!properties.contains_key("ocr_backend"));

    let _ = std::fs::remove_dir_all(&data_dir);
}
