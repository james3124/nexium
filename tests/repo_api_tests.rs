//! REST/MCP coverage for the repository navigation tools against a local
//! mock GitHub REST + raw-content server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use nexium::{ApiService, Config};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "nexium-repo-{tag}-{}-{nanos}-{count}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

/// A minimal HTTP/1.1 server that answers the GitHub endpoints used by the
/// `GitHubApiBackend` fixture: metadata, contents, recursive tree and raw files.
async fn spawn_mock_github() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move { serve_mock(stream).await });
        }
    });
    addr
}

async fn serve_mock(mut stream: TcpStream) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if buffer.len() > 64 * 1024 {
                    return;
                }
            }
        }
    }
    let request = String::from_utf8_lossy(&buffer);
    let request_line = request.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = target.split_once('?').map_or(target, |(path, _)| path);

    let (status, content_type, body): (u16, &str, Vec<u8>) = if method != "GET" {
        (
            405,
            "application/json",
            br#"{"message":"method not allowed"}"#.to_vec(),
        )
    } else {
        mock_response(path)
    };
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(header.as_bytes()).await.is_err() {
        return;
    }
    let _ = stream.write_all(&body).await;
    let _ = stream.flush().await;
}

fn mock_response(path: &str) -> (u16, &'static str, Vec<u8>) {
    const JSON: &str = "application/json";
    const TEXT: &str = "text/plain";
    const HTML: &str = "text/html; charset=utf-8";
    const BINARY: &str = "application/octet-stream";

    match path {
        // Minimal GitHub-like HTML pages for the browser backend fixture.
        "/acme/demo" => (
            200,
            HTML,
            br#"<html><head><meta name="description" content="Browser fixture"></head><body><a href="/acme/demo/tree/main">main</a></body></html>"#.to_vec(),
        ),
        "/acme/demo/tree/main" => (
            200,
            HTML,
            br#"<html><body><a href="/acme/demo/tree/main/src">src</a><a href="/acme/demo/blob/main/README.md">README.md</a></body></html>"#.to_vec(),
        ),
        "/acme/demo/tree/main/src" => (
            200,
            HTML,
            br#"<html><body><a href="/acme/demo/blob/main/src/lib.rs">lib.rs</a></body></html>"#.to_vec(),
        ),
        // A file path has no directory anchors, so browser list yields no entries.
        "/acme/demo/tree/main/README.md" => (
            200,
            HTML,
            b"<html><body>not a directory</body></html>".to_vec(),
        ),
        "/acme/demo/blob/main/README.md" => (
            200,
            HTML,
            br#"<html><body><table><tr><td class="blob-code">browser line one</td></tr><tr><td class="blob-code">browser line two</td></tr></table></body></html>"#.to_vec(),
        ),
        "/repos/acme/demo" => (
            200,
            JSON,
            json!({
                "default_branch": "main",
                "description": "Demo repository",
                "visibility": "public",
                "size": 42,
            })
            .to_string()
            .into_bytes(),
        ),
        "/repos/acme/limited" => (
            403,
            JSON,
            br#"{"message":"API rate limit exceeded"}"#.to_vec(),
        ),
        "/repos/acme/missing" => (404, JSON, br#"{"message":"Not Found"}"#.to_vec()),
        "/repos/acme/broken" => (500, JSON, br#"{"message":"upstream exploded"}"#.to_vec()),
        "/repos/acme/demo/git/trees/main" => (
            200,
            JSON,
            json!({
                "truncated": false,
                "tree": [
                    { "path": "README.md", "type": "blob", "size": 40 },
                    { "path": "src", "type": "tree" },
                    { "path": "src/lib.rs", "type": "blob", "size": 18 },
                    { "path": "src/nested", "type": "tree" },
                    { "path": "src/nested/util.py", "type": "blob", "size": 12 },
                    { "path": "assets", "type": "tree" },
                    { "path": "assets/logo.png", "type": "blob", "size": 12 },
                ],
            })
            .to_string()
            .into_bytes(),
        ),
        _ if path.starts_with("/repos/acme/demo/contents") => {
            let suffix = path
                .strip_prefix("/repos/acme/demo/contents")
                .unwrap_or("")
                .trim_start_matches('/');
            let entries = match suffix {
                "" => json!([
                    { "name": "README.md", "path": "README.md", "type": "file", "size": 40, "html_url": "https://github.com/acme/demo/blob/main/README.md" },
                    { "name": "src", "path": "src", "type": "dir", "html_url": "https://github.com/acme/demo/tree/main/src" },
                    { "name": "assets", "path": "assets", "type": "dir", "html_url": "https://github.com/acme/demo/tree/main/assets" },
                ]),
                "src" => json!([
                    { "name": "lib.rs", "path": "src/lib.rs", "type": "file", "size": 18 },
                    { "name": "nested", "path": "src/nested", "type": "dir" },
                ]),
                "src/nested" => json!([
                    { "name": "util.py", "path": "src/nested/util.py", "type": "file", "size": 12 },
                ]),
                "assets" => json!([
                    { "name": "logo.png", "path": "assets/logo.png", "type": "file", "size": 12 },
                ]),
                // GitHub returns a single object for file content paths; the
                // backend treats that as an empty directory listing.
                _ => json!({ "type": "file", "path": suffix }),
            };
            (200, JSON, entries.to_string().into_bytes())
        }
        _ if path.starts_with("/acme/demo/") => {
            let rest = path.strip_prefix("/acme/demo/").unwrap_or("");
            let file = rest.split_once('/').map_or("", |(_, file)| file);
            match file {
                "README.md" => (200, TEXT, b"line1\nline2\nline3\nline4\nline5\n".to_vec()),
                "src/lib.rs" => (200, TEXT, b"pub fn hello() {}\n".to_vec()),
                "assets/logo.png" => (200, BINARY, b"\x89PNG\x00\x01\x02binary".to_vec()),
                _ => (404, JSON, br#"{"message":"Not Found"}"#.to_vec()),
            }
        }
        _ => (404, JSON, br#"{"message":"Not Found"}"#.to_vec()),
    }
}

async fn spawn_api(addr: SocketAddr) -> (SocketAddr, Arc<ApiService>, TempDir) {
    let data_dir = temp_dir("server");
    let config = Config {
        data_dir: data_dir.0.clone(),
        repo_api_base: Some(format!("http://{addr}")),
        repo_raw_base: Some(format!("http://{addr}")),
        repo_web_base: Some(format!("http://{addr}")),
        github_token: None,
        ..Config::default()
    };
    let service = ApiService::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_addr = listener.local_addr().unwrap();
    let server = Arc::clone(&service);
    tokio::spawn(async move {
        nexium::api::serve_on(listener, server).await.unwrap();
    });
    (api_addr, service, data_dir)
}

struct HttpResponse {
    status: u16,
    body: Value,
}

async fn http(
    addr: SocketAddr,
    path: &str,
    agent: &str,
    session: Option<&str>,
    body: Value,
) -> HttpResponse {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", addr.to_string())
        .header("content-type", "application/json")
        .header("x-nexium-agent", agent);
    if let Some(session) = session {
        builder = builder.header("x-nexium-session", session);
    }
    let request = builder
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    HttpResponse { status, body }
}

async fn mcp(
    service: &Arc<ApiService>,
    agent: &str,
    id: u64,
    name: &str,
    arguments: Value,
) -> Value {
    let auth = service.local_auth(agent);
    nexium::mcp::handle_request(
        service,
        &auth,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn open_then_list_root_and_nested_directory() {
    let github = spawn_mock_github().await;
    let (addr, _service, _dir) = spawn_api(github).await;

    let opened = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s1"),
        json!({ "repo": "acme/demo@main" }),
    )
    .await;
    assert_eq!(opened.status, 200, "{}", opened.body);
    let data = &opened.body["data"];
    assert_eq!(data["repo"], "acme/demo");
    assert_eq!(data["ref"], "main");
    assert_eq!(data["path"], "");
    assert_eq!(data["kind"], "dir");
    assert_eq!(data["source"], "github");
    assert_eq!(data["metadata"]["description"], "Demo repository");
    assert_eq!(data["metadata"]["default_branch"], "main");
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert!(
        entries
            .iter()
            .any(|entry| entry["name"] == "README.md" && entry["kind"] == "file")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["name"] == "src" && entry["kind"] == "dir")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["name"] == "assets" && entry["kind"] == "dir")
    );

    let listed = http(
        addr,
        "/browser/repo/list",
        "agent-a",
        Some("s1"),
        json!({ "path": "src" }),
    )
    .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let data = &listed.body["data"];
    assert_eq!(data["path"], "src");
    assert_eq!(data["kind"], "dir");
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry["path"] == "src/lib.rs"));
    assert!(entries.iter().any(|entry| entry["path"] == "src/nested"));
    assert_eq!(data["truncated"], false);

    let again = http(addr, "/browser/repo/list", "agent-a", Some("s1"), json!({})).await;
    assert_eq!(again.body["data"]["path"], "src");
}

#[tokio::test]
async fn tree_filters_by_prefix() {
    let github = spawn_mock_github().await;
    let (addr, _service, _dir) = spawn_api(github).await;
    http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s1"),
        json!({ "repo": "acme/demo@main" }),
    )
    .await;

    let all = http(addr, "/browser/repo/tree", "agent-a", Some("s1"), json!({})).await;
    assert_eq!(all.status, 200, "{}", all.body);
    assert_eq!(all.body["data"]["kind"], "tree");
    assert_eq!(all.body["data"]["truncated"], false);
    assert_eq!(all.body["data"]["path"], "");
    assert_eq!(all.body["data"]["entries"].as_array().unwrap().len(), 7);

    let filtered = http(
        addr,
        "/browser/repo/tree",
        "agent-a",
        Some("s1"),
        json!({ "prefix": "src/" }),
    )
    .await;
    let data = &filtered.body["data"];
    assert_eq!(data["path"], "src/");
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert!(
        entries
            .iter()
            .all(|entry| entry["path"].as_str().unwrap().starts_with("src/"))
    );
}

#[tokio::test]
async fn read_supports_line_ranges_truncation_and_binary_metadata() {
    let github = spawn_mock_github().await;
    let (addr, _service, _dir) = spawn_api(github).await;
    http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s1"),
        json!({ "repo": "acme/demo@main" }),
    )
    .await;

    let ranged = http(
        addr,
        "/browser/repo/read",
        "agent-a",
        Some("s1"),
        json!({ "path": "README.md", "start_line": 2, "max_lines": 2 }),
    )
    .await;
    assert_eq!(ranged.status, 200, "{}", ranged.body);
    let data = &ranged.body["data"];
    assert_eq!(data["kind"], "file");
    assert_eq!(data["path"], "README.md");
    assert_eq!(data["text"], "line2\nline3");
    assert_eq!(data["total_lines"], 5);
    assert_eq!(data["truncated"], true);
    assert_eq!(data["binary"], false);
    assert_eq!(data["language_hint"], "markdown");
    let lines = data["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["n"], 2);
    assert_eq!(lines[1]["n"], 3);
    assert_eq!(lines[0]["text"], "line2");

    let whole = http(
        addr,
        "/browser/repo/read",
        "agent-a",
        Some("s1"),
        json!({ "path": "README.md", "max_lines": 100 }),
    )
    .await;
    assert_eq!(whole.body["data"]["truncated"], false);
    assert_eq!(whole.body["data"]["lines"].as_array().unwrap().len(), 5);

    let binary = http(
        addr,
        "/browser/repo/read",
        "agent-a",
        Some("s1"),
        json!({ "path": "assets/logo.png" }),
    )
    .await;
    assert_eq!(binary.status, 200, "{}", binary.body);
    let data = &binary.body["data"];
    assert_eq!(data["binary"], true);
    assert_eq!(data["text"], "");
    assert_eq!(data["lines"].as_array().unwrap().len(), 0);
    assert_eq!(data["total_lines"], 0);
    assert_eq!(data["truncated"], true);
    assert_eq!(data["language_hint"], Value::Null);

    let directory = http(
        addr,
        "/browser/repo/read",
        "agent-a",
        Some("s1"),
        json!({ "path": "src" }),
    )
    .await;
    assert_eq!(directory.status, 404, "{}", directory.body);
    assert_eq!(directory.body["error"]["code"], "not_found");

    let missing_path = http(
        addr,
        "/browser/repo/read",
        "agent-a",
        Some("s1"),
        json!({ "path": "" }),
    )
    .await;
    assert_eq!(missing_path.status, 400, "{}", missing_path.body);
    assert_eq!(missing_path.body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn search_matches_names_and_paths() {
    let github = spawn_mock_github().await;
    let (addr, _service, _dir) = spawn_api(github).await;
    http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s1"),
        json!({ "repo": "acme/demo@main" }),
    )
    .await;

    let found = http(
        addr,
        "/browser/repo/search",
        "agent-a",
        Some("s1"),
        json!({ "query": "LIB" }),
    )
    .await;
    assert_eq!(found.status, 200, "{}", found.body);
    let data = &found.body["data"];
    assert_eq!(data["query"], "LIB");
    assert_eq!(data["repo"], "acme/demo");
    assert_eq!(data["ref"], "main");
    assert_eq!(data["truncated"], false);
    let matches = data["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], "src/lib.rs");

    let dirs_only = http(
        addr,
        "/browser/repo/search",
        "agent-a",
        Some("s1"),
        json!({ "query": "src", "paths_only": true }),
    )
    .await;
    let matches = dirs_only.body["data"]["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert!(matches.iter().all(|entry| entry["kind"] == json!("dir")));

    let empty = http(
        addr,
        "/browser/repo/search",
        "agent-a",
        Some("s1"),
        json!({ "query": "  " }),
    )
    .await;
    assert_eq!(empty.status, 400, "{}", empty.body);
}

#[tokio::test]
async fn navigate_supports_relative_paths_history_and_urls() {
    let github = spawn_mock_github().await;
    let (addr, _service, _dir) = spawn_api(github).await;
    http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s1"),
        json!({ "repo": "acme/demo@main" }),
    )
    .await;
    http(
        addr,
        "/browser/repo/list",
        "agent-a",
        Some("s1"),
        json!({ "path": "src" }),
    )
    .await;

    let file = http(
        addr,
        "/browser/repo/navigate",
        "agent-a",
        Some("s1"),
        json!({ "target": "../README.md" }),
    )
    .await;
    assert_eq!(file.status, 200, "{}", file.body);
    assert_eq!(file.body["data"]["kind"], "file");
    assert_eq!(file.body["data"]["path"], "README.md");
    assert_eq!(
        file.body["data"]["text"],
        "line1\nline2\nline3\nline4\nline5"
    );

    let back = http(
        addr,
        "/browser/repo/navigate",
        "agent-a",
        Some("s1"),
        json!({ "target": "", "direction": "back" }),
    )
    .await;
    assert_eq!(back.status, 200, "{}", back.body);
    assert_eq!(back.body["data"]["kind"], "dir");
    assert_eq!(back.body["data"]["path"], "src");

    let forward = http(
        addr,
        "/browser/repo/navigate",
        "agent-a",
        Some("s1"),
        json!({ "target": "", "direction": "forward" }),
    )
    .await;
    assert_eq!(forward.body["data"]["kind"], "file");
    assert_eq!(forward.body["data"]["path"], "README.md");

    let url = http(
        addr,
        "/browser/repo/navigate",
        "agent-a",
        Some("s1"),
        json!({ "target": "https://github.com/acme/demo/tree/main/src/nested" }),
    )
    .await;
    assert_eq!(url.status, 200, "{}", url.body);
    assert_eq!(url.body["data"]["repo"], "acme/demo");
    assert_eq!(url.body["data"]["ref"], "main");
    assert_eq!(url.body["data"]["path"], "src/nested");
    assert_eq!(url.body["data"]["kind"], "dir");

    let invalid = http(
        addr,
        "/browser/repo/navigate",
        "agent-a",
        Some("s1"),
        json!({ "target": "", "direction": "sideways" }),
    )
    .await;
    assert_eq!(invalid.status, 400, "{}", invalid.body);
    assert_eq!(invalid.body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn backend_and_repo_errors_are_mapped_to_api_errors() {
    let github = spawn_mock_github().await;
    let (addr, _service, _dir) = spawn_api(github).await;

    let unknown_backend = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s1"),
        json!({ "repo": "acme/demo@main", "backend": "gitlab" }),
    )
    .await;
    assert_eq!(unknown_backend.status, 400, "{}", unknown_backend.body);
    assert_eq!(unknown_backend.body["error"]["code"], "bad_request");
    assert!(
        unknown_backend.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unsupported repository backend")
    );

    let not_found = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s2"),
        json!({ "repo": "acme/missing" }),
    )
    .await;
    assert_eq!(not_found.status, 404, "{}", not_found.body);
    assert_eq!(not_found.body["error"]["code"], "not_found");

    let rate_limited = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s3"),
        json!({ "repo": "acme/limited" }),
    )
    .await;
    assert_eq!(rate_limited.status, 429, "{}", rate_limited.body);
    assert_eq!(rate_limited.body["error"]["code"], "upstream");
    assert_eq!(rate_limited.body["error"]["retry_after_ms"], 60_000);

    let upstream = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s4"),
        json!({ "repo": "acme/broken" }),
    )
    .await;
    assert_eq!(upstream.status, 502, "{}", upstream.body);
    assert_eq!(upstream.body["error"]["code"], "upstream");

    let not_opened = http(addr, "/browser/repo/list", "agent-a", Some("s5"), json!({})).await;
    assert_eq!(not_opened.status, 404, "{}", not_opened.body);
    assert_eq!(not_opened.body["error"]["code"], "not_found");

    let bad_ref = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("s6"),
        json!({ "repo": "../evil/repo" }),
    )
    .await;
    assert_eq!(bad_ref.status, 400, "{}", bad_ref.body);
    assert_eq!(bad_ref.body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn rest_and_mcp_share_the_same_results() {
    let github = spawn_mock_github().await;
    let (addr, service, _dir) = spawn_api(github).await;
    let agent = "agent-parity";
    let session = "shared";

    let rest_open = http(
        addr,
        "/browser/repo/open",
        agent,
        Some(session),
        json!({ "repo": "acme/demo@main" }),
    )
    .await;
    assert_eq!(rest_open.status, 200, "{}", rest_open.body);
    let mcp_open = mcp(
        &service,
        agent,
        1,
        "repo_open",
        json!({ "repo": "acme/demo@main", "session_id": session }),
    )
    .await;
    assert_eq!(mcp_open["result"]["isError"], false, "{mcp_open}");
    assert_eq!(
        rest_open.body["data"],
        mcp_open["result"]["structuredContent"]
    );

    let arguments =
        json!({ "path": "README.md", "start_line": 3, "max_lines": 1, "session_id": session });
    let rest_read = http(
        addr,
        "/browser/repo/read",
        agent,
        Some(session),
        arguments.clone(),
    )
    .await;
    let mcp_read = mcp(&service, agent, 2, "repo_read", arguments).await;
    assert_eq!(rest_read.status, 200, "{}", rest_read.body);
    assert_eq!(mcp_read["result"]["isError"], false, "{mcp_read}");
    assert_eq!(
        rest_read.body["data"],
        mcp_read["result"]["structuredContent"]
    );
    assert_eq!(rest_read.body["data"]["text"], "line3");

    let rest_tree = http(
        addr,
        "/browser/repo/tree",
        agent,
        Some(session),
        json!({ "prefix": "assets" }),
    )
    .await;
    let mcp_tree = mcp(
        &service,
        agent,
        3,
        "repo_tree",
        json!({ "prefix": "assets", "session_id": session }),
    )
    .await;
    assert_eq!(
        rest_tree.body["data"],
        mcp_tree["result"]["structuredContent"]
    );

    let mcp_navigate = mcp(
        &service,
        agent,
        4,
        "repo_navigate",
        json!({ "target": "../src/lib.rs", "session_id": session }),
    )
    .await;
    assert_eq!(mcp_navigate["result"]["structuredContent"]["kind"], "file");
    assert_eq!(
        mcp_navigate["result"]["structuredContent"]["path"],
        "src/lib.rs"
    );
}

#[tokio::test]
async fn navigation_state_is_isolated_per_session_and_agent() {
    let github = spawn_mock_github().await;
    let (addr, _service, _dir) = spawn_api(github).await;

    let opened_a = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("session-a"),
        json!({ "repo": "acme/demo@main:src" }),
    )
    .await;
    assert_eq!(opened_a.status, 200, "{}", opened_a.body);
    assert_eq!(opened_a.body["data"]["path"], "src");

    let opened_b = http(
        addr,
        "/browser/repo/open",
        "agent-a",
        Some("session-b"),
        json!({ "repo": "acme/demo@main" }),
    )
    .await;
    assert_eq!(opened_b.status, 200, "{}", opened_b.body);
    assert_eq!(opened_b.body["data"]["path"], "");

    let listed_a = http(
        addr,
        "/browser/repo/list",
        "agent-a",
        Some("session-a"),
        json!({}),
    )
    .await;
    assert_eq!(listed_a.body["data"]["path"], "src");
    assert!(
        listed_a.body["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "src/lib.rs")
    );

    let listed_b = http(
        addr,
        "/browser/repo/list",
        "agent-a",
        Some("session-b"),
        json!({}),
    )
    .await;
    assert_eq!(listed_b.body["data"]["path"], "");
    assert!(
        listed_b.body["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "README.md")
    );

    // The same session id used by a different agent must not share history.
    let opened_other = http(
        addr,
        "/browser/repo/open",
        "agent-b",
        Some("session-a"),
        json!({ "repo": "acme/demo@main:assets" }),
    )
    .await;
    assert_eq!(opened_other.status, 200, "{}", opened_other.body);
    assert_eq!(opened_other.body["data"]["path"], "assets");

    let listed_a_again = http(
        addr,
        "/browser/repo/list",
        "agent-a",
        Some("session-a"),
        json!({}),
    )
    .await;
    assert_eq!(listed_a_again.body["data"]["path"], "src");

    let listed_other = http(
        addr,
        "/browser/repo/list",
        "agent-b",
        Some("session-a"),
        json!({}),
    )
    .await;
    assert_eq!(listed_other.body["data"]["path"], "assets");
}

#[tokio::test]
async fn browser_backend_selected_at_open_is_reused_by_every_operation() {
    let github = spawn_mock_github().await;
    let (addr, service, _dir) = spawn_api(github).await;
    let agent = "agent-browser";
    let session = "browser-session";

    let opened = http(
        addr,
        "/browser/repo/open",
        agent,
        Some(session),
        json!({ "repo": "acme/demo@main", "backend": "browser" }),
    )
    .await;
    assert_eq!(opened.status, 200, "{}", opened.body);
    let data = &opened.body["data"];
    assert_eq!(data["source"], "browser");
    assert_eq!(data["metadata"]["description"], "Browser fixture");
    let entries = data["entries"].as_array().unwrap();
    assert!(entries.iter().any(|entry| entry["path"] == "src"));
    assert!(entries.iter().any(|entry| entry["path"] == "README.md"));

    let listed = http(
        addr,
        "/browser/repo/list",
        agent,
        Some(session),
        json!({ "path": "src" }),
    )
    .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(listed.body["data"]["source"], "browser");
    assert_eq!(listed.body["data"]["path"], "src");
    assert!(
        listed.body["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "src/lib.rs")
    );

    let read = http(
        addr,
        "/browser/repo/read",
        agent,
        Some(session),
        json!({ "path": "README.md" }),
    )
    .await;
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(read.body["data"]["source"], "browser");
    assert_eq!(read.body["data"]["kind"], "file");
    assert_eq!(
        read.body["data"]["text"],
        "browser line one\nbrowser line two"
    );

    let tree = http(
        addr,
        "/browser/repo/tree",
        agent,
        Some(session),
        json!({ "prefix": "src/" }),
    )
    .await;
    assert_eq!(tree.status, 200, "{}", tree.body);
    assert_eq!(tree.body["data"]["source"], "browser");
    assert!(
        tree.body["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "src/lib.rs")
    );

    let search = http(
        addr,
        "/browser/repo/search",
        agent,
        Some(session),
        json!({ "query": "lib" }),
    )
    .await;
    assert_eq!(search.status, 200, "{}", search.body);
    assert_eq!(search.body["data"]["source"], "browser");
    assert!(
        search.body["data"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "src/lib.rs")
    );

    let back = http(
        addr,
        "/browser/repo/navigate",
        agent,
        Some(session),
        json!({ "target": "", "direction": "back" }),
    )
    .await;
    assert_eq!(back.status, 200, "{}", back.body);
    assert_eq!(back.body["data"]["source"], "browser");
    assert_eq!(back.body["data"]["path"], "");

    // MCP dispatch shares the same per-session backend choice.
    let mcp_listed = mcp(
        &service,
        agent,
        1,
        "repo_list",
        json!({ "session_id": session }),
    )
    .await;
    assert_eq!(mcp_listed["result"]["isError"], false, "{mcp_listed}");
    assert_eq!(
        mcp_listed["result"]["structuredContent"]["source"],
        "browser"
    );
    assert_eq!(mcp_listed["result"]["structuredContent"]["path"], "");
}
