//! Persistence and screenshot-artifact hardening for the control API.
//!
//! Covers the two Phase 6 gaps that remained in-memory / base64-only:
//!   * session registry + browser state persisted to disk and rebuilt after a
//!     process restart (simulated by constructing a second ApiService),
//!   * opt-in screenshot artifacts written under a configurable directory with
//!     a validated name and a path/URL returned alongside the base64 payload.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use common::spawn_test_server;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use nexium::ApiService;
use nexium::Config;
use serde_json::{Value, json};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-gap-hardening-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn spawn_api_on(
    data_dir: PathBuf,
    mutate: impl FnOnce(&mut Config),
) -> (SocketAddr, Arc<ApiService>) {
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
    (addr, service)
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
    let (status, headers, bytes) = http_raw(addr, method, path, agent, session, body).await;
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let _ = headers;
    HttpResponse {
        status,
        body: parsed,
    }
}

#[allow(clippy::too_many_arguments)]
async fn http_raw(
    addr: SocketAddr,
    method: &str,
    path: &str,
    agent: Option<&str>,
    session: Option<&str>,
    body: Option<Value>,
) -> (u16, HashMap<String, String>, Vec<u8>) {
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
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn session_survives_a_restart_like_reload() {
    let web = spawn_test_server().await;
    let data_dir = temp_dir("persist");
    let (addr, _first) = spawn_api_on(data_dir.clone(), |_| {}).await;

    // Create and save a live session with two tabs.
    let created = http(
        addr,
        "POST",
        "/browser/create",
        Some("agent-a"),
        None,
        Some(json!({ "url": format!("http://{web}/interact") })),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);
    let session = created.body["data"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let second_tab = http(
        addr,
        "POST",
        "/browser/tabs/create",
        Some("agent-a"),
        Some(&session),
        Some(json!({ "url": format!("http://{web}/two") })),
    )
    .await;
    assert_eq!(second_tab.status, 200, "{}", second_tab.body);

    let saved = http(
        addr,
        "POST",
        "/browser/session/save",
        Some("agent-a"),
        Some(&session),
        Some(json!({})),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.body["data"]["saved"], true);
    assert_eq!(saved.body["data"]["persisted"], true);
    assert!(
        saved.body["data"]["path"]
            .as_str()
            .unwrap()
            .contains("api-sessions"),
        "{}",
        saved.body
    );

    // Simulate a process restart: a brand-new service over the same data dir
    // has no in-memory sessions of its own.
    let (addr2, _second) = spawn_api_on(data_dir.clone(), |_| {}).await;

    let listed = http(
        addr2,
        "GET",
        "/browser/session/persisted",
        Some("agent-a"),
        None,
        None,
    )
    .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let records = listed.body["data"]["sessions"].as_array().unwrap();
    assert!(
        records
            .iter()
            .any(|record| record["session_id"] == session && record["owner"] == "agent-a"),
        "{}",
        listed.body
    );

    let restored = http(
        addr2,
        "POST",
        "/browser/session/restore-persisted",
        Some("agent-a"),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(restored.status, 200, "{}", restored.body);
    let ids = restored.body["data"]["restored"].as_array().unwrap();
    assert!(
        ids.iter().any(|id| id.as_str() == Some(session.as_str())),
        "{}",
        restored.body
    );

    // The restored session keeps its id, owner, viewport and both tabs.
    let status = http(
        addr2,
        "GET",
        "/browser/status",
        Some("agent-a"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status.status, 200, "{}", status.body);
    assert_eq!(status.body["data"]["session_id"], session);
    assert_eq!(
        status.body["data"]["tabs"].as_array().unwrap().len(),
        2,
        "{}",
        status.body
    );

    // Restoring is idempotent and ownership is still enforced.
    let again = http(
        addr2,
        "POST",
        "/browser/session/restore-persisted",
        Some("agent-a"),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(again.body["data"]["restored"].as_array().unwrap().len(), 0);
    let foreign = http(
        addr2,
        "GET",
        "/browser/status",
        Some("agent-b"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(foreign.status, 403, "{}", foreign.body);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn screenshot_artifact_is_written_validated_and_served() {
    let web = spawn_test_server().await;
    let artifacts = temp_dir("artifacts");
    let data_dir = temp_dir("shot");
    let (addr, _service) = spawn_api_on(data_dir.clone(), |config| {
        config.api_artifacts_dir = Some(artifacts.clone());
        config.api_artifact_max_files = 3;
    })
    .await;

    let created = http(
        addr,
        "POST",
        "/browser/create",
        Some("agent-a"),
        None,
        Some(json!({ "url": format!("http://{web}/interact") })),
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.body);
    let session = created.body["data"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Default behaviour: base64 only, no artifact fields.
    let plain = http(
        addr,
        "POST",
        "/browser/screenshot",
        Some("agent-a"),
        Some(&session),
        Some(json!({})),
    )
    .await;
    assert_eq!(plain.status, 200, "{}", plain.body);
    assert!(plain.body["data"]["data"].as_str().unwrap().len() > 100);
    assert!(plain.body["data"]["path"].is_null());
    assert!(plain.body["data"]["url"].is_null());

    // Opt in with a named artifact.
    let saved = http(
        addr,
        "POST",
        "/browser/screenshot",
        Some("agent-a"),
        Some(&session),
        Some(json!({ "format": "png", "save": true, "artifact_name": "proof-shot" })),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert!(saved.body["data"]["data"].as_str().unwrap().len() > 100);
    assert_eq!(saved.body["data"]["url"], "/artifacts/proof-shot.png");
    let path = PathBuf::from(saved.body["data"]["path"].as_str().unwrap());
    assert!(path.exists(), "artifact not found at {}", path.display());
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "invalid PNG header"
    );

    // The returned URL actually serves the same bytes.
    let (status, headers, served) = http_raw(
        addr,
        "GET",
        "/artifacts/proof-shot.png",
        Some("agent-a"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        headers.get("content-type").map(String::as_str),
        Some("image/png")
    );
    assert_eq!(served, bytes);

    // Path traversal and hidden names are rejected.
    for bad in ["../evil", "a/b", ".hidden", ".."] {
        let rejected = http(
            addr,
            "POST",
            "/browser/screenshot",
            Some("agent-a"),
            Some(&session),
            Some(json!({ "save": true, "artifact_name": bad })),
        )
        .await;
        assert_eq!(rejected.status, 400, "name {bad:?}: {}", rejected.body);
        assert_eq!(rejected.body["error"]["code"], "bad_request");
    }

    // Auto-generated names also work and the store stays bounded.
    for _ in 0..5 {
        let auto = http(
            addr,
            "POST",
            "/browser/screenshot",
            Some("agent-a"),
            Some(&session),
            Some(json!({ "save": true })),
        )
        .await;
        assert_eq!(auto.status, 200, "{}", auto.body);
        assert!(
            auto.body["data"]["path"]
                .as_str()
                .unwrap()
                .contains("artifacts")
        );
    }
    let kept = std::fs::read_dir(&artifacts)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension == "png")
        })
        .count();
    assert!(kept <= 3, "expected at most 3 artifacts, found {kept}");

    let _ = std::fs::remove_dir_all(&artifacts);
    let _ = std::fs::remove_dir_all(&data_dir);
}
