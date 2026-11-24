//! End-to-end tests for the vision API service and REST surface.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use nexium::api::service::{
    VisionAnalyzeOptions, VisionCompareOptions, VisionFindOptions, VisionOptions,
};
use nexium::{ApiService, Config};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static COUNTER: AtomicU64 = AtomicU64::new(0);

const VISION_HTML: &str = r#"<!doctype html><html><head><title>Vision</title><style>
body { margin: 0; font-family: sans-serif; }
#primary { position: absolute; left: 20px; top: 40px; width: 160px; height: 44px; background: #2ea043; color: #fff; border: 0; }
#profile { position: absolute; left: 20px; top: 110px; }
#out { position: absolute; left: 20px; top: 160px; }
</style></head><body>
<h1>Vision test page</h1>
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

const OTHER_HTML: &str = "<html><head><title>Other</title></head><body><h1>Different document</h1><button>Something else</button></body></html>";

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-vision-api-{tag}-{}-{nanos}-{count}",
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
    let body = match path {
        "/destination" => DESTINATION_HTML,
        "/other" => OTHER_HTML,
        _ => VISION_HTML,
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    HttpResponse { status, body }
}

async fn create_session(addr: SocketAddr, agent: &str, url: &str) -> String {
    let created = http(
        addr,
        "POST",
        "/browser/create",
        None,
        Some(agent),
        None,
        Some(json!({ "url": url })),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);
    created.body["data"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn labels(elements: &Value) -> Vec<String> {
    elements
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["label"].as_str().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn run_texts(runs: &Value) -> Vec<String> {
    runs.as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["text"].as_str().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// JSON round-trips widen `f32` to `f64`, so compare with a tolerance.
fn assert_close(left: f64, right: f64) {
    assert!(
        (left - right).abs() < 0.001,
        "expected {left} to be close to {right}"
    );
}

fn decode_base64(value: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value.as_str().expect("base64 payload"))
        .expect("decodable base64")
}

#[tokio::test]
async fn vision_analyze_returns_elements_text_and_png() {
    let web = spawn_vision_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/")).await;

    let response = http(
        addr,
        "POST",
        "/browser/vision/analyze",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "session_id": session })),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let data = &response.body["data"];
    let elements = data["elements"].as_array().expect("elements array");
    let button = elements
        .iter()
        .find(|element| element["label"] == "Submit order")
        .expect("button described in the loaded HTML");
    assert_eq!(button["tag"], "button");
    assert_eq!(button["clickable"], true);
    assert!(button["viewport_rect"]["width"].as_f64().unwrap() > 0.0);
    assert!(data["text_runs"].as_array().unwrap().len() >= 3);
    let clickable = labels(&data["clickable_regions"]);
    assert!(clickable.iter().any(|label| label == "Submit order"));
    assert_eq!(data["viewport"]["width"], 1280.0);
    assert_eq!(data["viewport"]["height"], 720.0);
    assert_eq!(data["scroll"]["x"], 0.0);
    assert_eq!(data["scroll"]["y"], 0.0);

    let encoded = data["image"].as_str().expect("base64 PNG");
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("decodable base64");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_extract_text_finds_a_phrase_and_handles_missing_query() {
    let web = spawn_vision_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/")).await;

    let found = http(
        addr,
        "POST",
        "/browser/vision/extract-text",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "query": "Submit", "session_id": session })),
    )
    .await;
    assert_eq!(found.status, 200, "{}", found.body);
    let data = &found.body["data"];
    assert!(data["text"].as_str().unwrap().contains("Submit order"));
    assert!(!data["runs"].as_array().unwrap().is_empty());
    let matches = data["matches"].as_array().expect("matches array");
    assert!(!matches.is_empty());
    assert_eq!(matches[0]["confidence"], 1.0);
    assert!(matches[0]["rect"]["width"].as_f64().unwrap() > 0.0);

    let all = http(
        addr,
        "POST",
        "/browser/vision/extract-text",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "session_id": session })),
    )
    .await;
    assert_eq!(all.status, 200, "{}", all.body);
    assert_eq!(all.body["data"]["matches"], json!([]));
    assert_eq!(all.body["data"]["text"], data["text"]);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_find_element_ranks_the_expected_button_first() {
    let web = spawn_vision_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/")).await;

    let response = http(
        addr,
        "POST",
        "/browser/vision/find-element",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({
            "description": "green Submit order button",
            "top_k": 3,
            "session_id": session,
        })),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let matches = response.body["data"]["matches"]
        .as_array()
        .expect("matches array");
    assert!(!matches.is_empty());
    assert_eq!(matches[0]["element"]["tag"], "button");
    assert_eq!(matches[0]["element"]["label"], "Submit order");
    assert!(matches[0]["score"].as_f64().unwrap() > 0.5);
    assert!(matches[0]["signals"].as_array().unwrap().len() > 0);
    assert!(
        matches[0]["element"]["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|signal| signal.as_str().unwrap_or("").starts_with("bg:"))
    );

    let empty = http(
        addr,
        "POST",
        "/browser/vision/find-element",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "description": "  ", "session_id": session })),
    )
    .await;
    assert_eq!(empty.status, 400);
    assert_eq!(empty.body["error"]["code"], "bad_request");

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_click_target_with_execute_follows_a_link() {
    let web = spawn_vision_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/")).await;

    let response = http(
        addr,
        "POST",
        "/browser/vision/click-target",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({
            "description": "Open profile link",
            "execute": true,
            "session_id": session,
        })),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let data = &response.body["data"];
    assert_eq!(data["label"], "Open profile");
    assert!(data["x"].as_f64().unwrap() > 0.0);
    assert!(data["y"].as_f64().unwrap() > 0.0);
    assert!(data["score"].as_f64().unwrap() > 0.0);
    assert_eq!(data["click_result"]["element"]["tag"], "a");

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
    assert_eq!(status.status, 200, "{}", status.body);
    assert_eq!(status.body["data"]["page"]["title"], "Destination");

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_click_target_verify_reports_visual_and_text_diff() {
    let web = spawn_vision_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/")).await;

    let response = http(
        addr,
        "POST",
        "/browser/vision/click-target",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({
            "description": "Submit order button",
            "execute": true,
            "verify": true,
            "session_id": session,
        })),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let verification = &response.body["data"]["verification"];
    assert!(verification["ratio"].as_f64().unwrap() > 0.0);
    assert!(verification["changed_regions"].as_array().unwrap().len() > 0);
    let added = verification["text_added"]
        .as_array()
        .expect("text_added array");
    assert!(
        added
            .iter()
            .any(|text| text.as_str().unwrap_or("").contains("submitted")),
        "unexpected text diff: {verification}"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_snapshot_respects_full_page_format_and_include_visual() {
    let web = spawn_vision_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/")).await;

    let normal = http(
        addr,
        "POST",
        "/browser/vision/snapshot",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "session_id": session })),
    )
    .await;
    assert_eq!(normal.status, 200, "{}", normal.body);
    let data = &normal.body["data"];
    assert_eq!(data["format"], "png");
    assert_eq!(data["width"], 1280.0);
    assert_eq!(data["height"], 720.0);
    assert!(data["bytes"].as_u64().unwrap() > 0);
    assert!(data["data"].as_str().unwrap().len() > 100);
    assert!(data["elements"].as_array().unwrap().len() > 0);
    assert!(data["text_runs"].as_array().unwrap().len() > 0);
    assert_eq!(data["scroll_x"], 0.0);
    assert_eq!(data["scroll_y"], 0.0);
    assert_eq!(data["viewport"]["width"], 1280.0);

    let full = http(
        addr,
        "POST",
        "/browser/vision/snapshot",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({
            "full_page": true,
            "format": "jpeg",
            "quality": 50,
            "include_visual": false,
            "session_id": session,
        })),
    )
    .await;
    assert_eq!(full.status, 200, "{}", full.body);
    let data = &full.body["data"];
    assert_eq!(data["format"], "jpeg");
    assert!(data["height"].as_u64().unwrap() >= 720);
    assert!(data.get("elements").is_none());
    assert!(data.get("text_runs").is_none());
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(data["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&jpeg[..2], &[0xff, 0xd8]);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn rest_responses_match_direct_service_values() {
    let web = spawn_vision_server().await;
    let (addr, service, data_dir) = spawn_api(None).await;
    let session = create_session(addr, "agent-a", &format!("http://{web}/")).await;
    let auth = service.local_auth("agent-a");

    let rest = http(
        addr,
        "POST",
        "/browser/vision/analyze",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "session_id": session })),
    )
    .await;
    assert_eq!(rest.status, 200, "{}", rest.body);
    assert_eq!(rest.body["ok"], true);
    let direct = service
        .vision_analyze(
            &auth,
            Some(&session),
            VisionAnalyzeOptions {
                session_id: Some(session.clone()),
                ..VisionAnalyzeOptions::default()
            },
        )
        .await
        .expect("direct analyze");
    let rest_data = &rest.body["data"];
    assert_eq!(
        rest_data["elements"].as_array().unwrap().len(),
        direct["elements"].as_array().unwrap().len()
    );
    assert_eq!(labels(&rest_data["elements"]), labels(&direct["elements"]));
    assert_eq!(
        run_texts(&rest_data["text_runs"]),
        run_texts(&direct["text_runs"])
    );
    assert_eq!(
        labels(&rest_data["clickable_regions"]),
        labels(&direct["clickable_regions"])
    );
    assert_close(
        rest_data["viewport"]["width"].as_f64().unwrap(),
        direct["viewport"]["width"].as_f64().unwrap(),
    );
    assert_close(
        rest_data["viewport"]["height"].as_f64().unwrap(),
        direct["viewport"]["height"].as_f64().unwrap(),
    );
    assert_close(
        rest_data["scroll"]["x"].as_f64().unwrap(),
        direct["scroll"]["x"].as_f64().unwrap(),
    );
    assert_close(
        rest_data["scroll"]["y"].as_f64().unwrap(),
        direct["scroll"]["y"].as_f64().unwrap(),
    );
    assert_eq!(
        decode_base64(&rest_data["image"]).len(),
        decode_base64(&direct["image"]).len()
    );

    let rest_snapshot = http(
        addr,
        "POST",
        "/browser/vision/snapshot",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "session_id": session })),
    )
    .await;
    let direct_snapshot = service
        .vision_snapshot(
            &auth,
            Some(&session),
            VisionOptions {
                session_id: Some(session.clone()),
                ..VisionOptions::default()
            },
        )
        .await
        .expect("direct snapshot");
    let rest_data = &rest_snapshot.body["data"];
    assert_eq!(rest_data["format"], direct_snapshot["format"]);
    for field in ["width", "height", "bytes", "scroll_x", "scroll_y"] {
        assert_eq!(rest_data[field], direct_snapshot[field], "field {field}");
    }
    for field in ["width", "height"] {
        assert_close(
            rest_data["viewport"][field].as_f64().unwrap(),
            direct_snapshot["viewport"][field].as_f64().unwrap(),
        );
    }
    assert_eq!(
        run_texts(&rest_data["text_runs"]),
        run_texts(&direct_snapshot["text_runs"])
    );
    assert_eq!(
        labels(&rest_data["elements"]),
        labels(&direct_snapshot["elements"])
    );
    assert_eq!(
        decode_base64(&rest_data["data"]).len(),
        decode_base64(&direct_snapshot["data"]).len()
    );

    let rest_compare = http(
        addr,
        "POST",
        "/browser/vision/compare",
        None,
        Some("agent-a"),
        Some(&session),
        Some(json!({ "session_id": session })),
    )
    .await;
    let direct_compare = service
        .vision_compare(
            &auth,
            Some(&session),
            VisionCompareOptions {
                session_id: Some(session.clone()),
                ..VisionCompareOptions::default()
            },
        )
        .await
        .expect("direct compare");
    let rest_compare_data = &rest_compare.body["data"];
    assert_close(
        rest_compare_data["ratio"].as_f64().unwrap(),
        direct_compare["ratio"].as_f64().unwrap(),
    );
    assert_eq!(
        rest_compare_data["changed_regions"]
            .as_array()
            .unwrap()
            .len(),
        direct_compare["changed_regions"].as_array().unwrap().len()
    );
    assert_eq!(
        rest_compare_data["text_added"],
        direct_compare["text_added"]
    );
    assert_eq!(
        rest_compare_data["text_removed"],
        direct_compare["text_removed"]
    );
    let direct_find = service
        .vision_find_element(
            &auth,
            Some(&session),
            VisionFindOptions {
                description: "Submit order button".to_owned(),
                session_id: Some(session.clone()),
                ..VisionFindOptions::default()
            },
        )
        .await
        .expect("direct find");
    assert_eq!(
        direct_find["matches"][0]["element"]["label"],
        "Submit order"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn vision_requests_are_isolated_between_agents() {
    let web = spawn_vision_server().await;
    let (addr, _service, data_dir) = spawn_api(None).await;
    let first = create_session(addr, "agent-a", &format!("http://{web}/")).await;
    let second = create_session(addr, "agent-b", &format!("http://{web}/other")).await;

    let first_response = http(
        addr,
        "POST",
        "/browser/vision/analyze",
        None,
        Some("agent-a"),
        Some(&first),
        Some(json!({ "session_id": first })),
    )
    .await;
    assert_eq!(first_response.status, 200, "{}", first_response.body);
    let first_labels = labels(&first_response.body["data"]["elements"]);
    assert!(first_labels.iter().any(|label| label == "Submit order"));

    let second_response = http(
        addr,
        "POST",
        "/browser/vision/analyze",
        None,
        Some("agent-b"),
        Some(&second),
        Some(json!({ "session_id": second })),
    )
    .await;
    assert_eq!(second_response.status, 200, "{}", second_response.body);
    let second_labels = labels(&second_response.body["data"]["elements"]);
    assert!(second_labels.iter().any(|label| label == "Something else"));
    assert!(!second_labels.iter().any(|label| label == "Submit order"));

    let forbidden = http(
        addr,
        "POST",
        "/browser/vision/analyze",
        None,
        Some("agent-b"),
        Some(&first),
        Some(json!({ "session_id": first })),
    )
    .await;
    assert_eq!(forbidden.status, 403, "{}", forbidden.body);
    assert_eq!(forbidden.body["error"]["code"], "forbidden");

    let _ = std::fs::remove_dir_all(&data_dir);
}
