//! Core repository navigation tests: reference parsing and the GitHub REST/raw
//! backend against a local mock server (no external network).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use nexium::Config;
use nexium::intelligence::{GitHubApiBackend, RepoBackend, RepoError, RepoKind, RepoRef};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Default)]
struct SeenHeaders {
    authorization: Vec<String>,
    if_none_match: Vec<String>,
}

#[derive(Default)]
struct MockState {
    seen: Mutex<SeenHeaders>,
    repo_requests: AtomicUsize,
}

async fn spawn_github_mock() -> (SocketAddr, Arc<MockState>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(MockState::default());
    let server_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let state = Arc::clone(&server_state);
            tokio::spawn(async move {
                handle(stream, state).await;
            });
        }
    });
    (addr, state)
}

async fn handle(mut stream: TcpStream, state: Arc<MockState>) {
    let mut buffer = vec![0u8; 16 * 1024];
    let Ok(size) = stream.read(&mut buffer).await else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..size]).to_string();
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or("");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_owned();
    let header = |name: &str| {
        lines.clone().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    if let Some(auth) = header("authorization")
        && !auth.is_empty()
    {
        state.seen.lock().unwrap().authorization.push(auth);
    }
    if let Some(etag) = header("if-none-match")
        && !etag.is_empty()
    {
        state.seen.lock().unwrap().if_none_match.push(etag);
    }

    let (status, reason, headers, body): (u16, &str, &str, Vec<u8>) = match path.as_str() {
        "/repos/acme/widget" => {
            state.repo_requests.fetch_add(1, Ordering::SeqCst);
            if header("if-none-match").as_deref() == Some("\"v1\"") {
                (304, "Not Modified", "ETag: \"v1\"\r\n", Vec::new())
            } else {
                (
                    200,
                    "OK",
                    "Content-Type: application/json\r\nETag: \"v1\"\r\n",
                    br#"{"default_branch":"main","description":"demo repo","visibility":"public","size":42}"#.to_vec(),
                )
            }
        }
        "/repos/acme/widget/contents" => (
            200,
            "OK",
            "Content-Type: application/json\r\n",
            br#"[{"name":"src","path":"src","type":"dir"},{"name":"README.md","path":"README.md","type":"file","size":12}]"#.to_vec(),
        ),
        "/repos/acme/widget/contents/src" => (
            200,
            "OK",
            "Content-Type: application/json\r\n",
            br#"[{"name":"lib.rs","path":"src/lib.rs","type":"file","size":60}]"#.to_vec(),
        ),
        "/repos/acme/widget/git/trees/main" => (
            200,
            "OK",
            "Content-Type: application/json\r\n",
            br#"{"truncated":false,"tree":[{"path":"README.md","type":"blob","size":12},{"path":"src","type":"tree"},{"path":"src/lib.rs","type":"blob","size":60},{"path":"src/nested/mod.rs","type":"blob","size":20}]}"#.to_vec(),
        ),
        "/repos/acme/forbidden" => (403, "Forbidden", "Content-Type: text/plain\r\n", b"no".to_vec()),
        "/repos/acme/limited" => (429, "Too Many Requests", "Content-Type: text/plain\r\n", b"slow down".to_vec()),
        "/repos/acme/missing" => (404, "Not Found", "Content-Type: text/plain\r\n", b"missing".to_vec()),
        "/acme/widget/main/src/lib.rs" => (
            200,
            "OK",
            "Content-Type: text/plain\r\n",
            b"line one\nline two\nline three\nline four\nline five\n".to_vec(),
        ),
        "/acme/widget/main/big.bin" => (
            200,
            "OK",
            "Content-Type: application/octet-stream\r\n",
            vec![0u8; 64],
        ),
        _ => (404, "Not Found", "Content-Type: text/plain\r\n", b"nope".to_vec()),
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&body).await;
    let _ = stream.shutdown().await;
}

fn api_config(addr: SocketAddr) -> Config {
    let mut config = Config::default();
    // Isolate the backend's own ETag cache from the HTTP client's disk/memory cache.
    config.cache_enabled = false;
    config.repo_api_base = Some(format!("http://{addr}"));
    config.repo_raw_base = Some(format!("http://{addr}"));
    config
}

#[test]
fn parses_all_documented_url_and_shorthand_forms() {
    let cases = [
        ("https://github.com/octo/demo", "github.com", None, ""),
        (
            "https://github.com/octo/demo/tree/main/src",
            "github.com",
            Some("main"),
            "src",
        ),
        (
            "https://github.com/octo/demo/blob/main/src/lib.rs",
            "github.com",
            Some("main"),
            "src/lib.rs",
        ),
        (
            "https://raw.githubusercontent.com/octo/demo/main/src/lib.rs",
            "raw.githubusercontent.com",
            Some("main"),
            "src/lib.rs",
        ),
        ("octo/demo", "github.com", None, ""),
        ("octo/demo@main", "github.com", Some("main"), ""),
        (
            "octo/demo@main:src/lib.rs",
            "github.com",
            Some("main"),
            "src/lib.rs",
        ),
        ("https://github.com/octo/demo/", "github.com", None, ""),
        (
            "https://github.com/octo/demo/blob/main/src/my%20file.rs",
            "github.com",
            Some("main"),
            "src/my file.rs",
        ),
    ];
    for (input, host, rf, path) in cases {
        let parsed = RepoRef::parse(input).unwrap_or_else(|error| panic!("{input}: {error}"));
        assert_eq!(parsed.host, host, "{input}");
        assert_eq!(parsed.owner, "octo", "{input}");
        assert_eq!(parsed.repo, "demo", "{input}");
        assert_eq!(parsed.rf.as_deref(), rf, "{input}");
        assert_eq!(parsed.path, path, "{input}");
    }
}

#[test]
fn rejects_invalid_traversal_and_unsupported_hosts() {
    for input in [
        "",
        "octo",
        "octo/demo/extra",
        "octo/../demo",
        "octo/demo:../secret",
        "octo/demo@../x",
        "https://gitlab.com/octo/demo",
        "https://github.com/octo/demo/blob/main/%2e%2e/secret",
        "https://github.com/octo/demo/blob/main/%ZZ",
    ] {
        let error = RepoRef::parse(input).expect_err(input);
        assert_eq!(error.code(), "bad_request", "{input}: {error}");
    }
}

#[test]
fn displays_canonical_reference() {
    assert_eq!(
        RepoRef::parse("octo/demo@main:src/lib.rs")
            .unwrap()
            .to_string(),
        "octo/demo@main:src/lib.rs"
    );
    assert_eq!(
        RepoRef::parse("https://github.com/octo/demo/")
            .unwrap()
            .to_string(),
        "octo/demo"
    );
    assert_eq!(
        RepoRef::parse("octo/demo@MAIN").unwrap().to_string(),
        "octo/demo@main"
    );
}

#[test]
fn at_path_and_relative_handle_dotdot() {
    let rf = RepoRef::parse("octo/demo@main:docs").unwrap();
    assert_eq!(rf.at_path("src/lib.rs").unwrap().path, "src/lib.rs");
    assert_eq!(rf.at_path("").unwrap().path, "");
    assert!(rf.at_path("../secret").is_err());
    assert_eq!(rf.relative("guide.md").unwrap().path, "docs/guide.md");
    assert_eq!(rf.relative("../src/lib.rs").unwrap().path, "src/lib.rs");
    assert_eq!(rf.relative("/src/main.rs").unwrap().path, "src/main.rs");
    assert_eq!(rf.relative("../..").unwrap().path, "");
    assert_eq!(
        rf.relative("https://github.com/other/repo").unwrap().repo,
        "repo"
    );
}

#[tokio::test]
async fn api_backend_lists_root_and_filters_tree_prefix() {
    let (addr, _) = spawn_github_mock().await;
    let rf = RepoRef::parse("acme/widget@main").unwrap();
    let backend = GitHubApiBackend::new(&api_config(addr));

    let listing = backend.list(&rf, "").await.unwrap();
    assert!(!listing.truncated);
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| entry.path == "src" && entry.kind == RepoKind::Dir)
    );
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| entry.path == "README.md" && entry.kind == RepoKind::File)
    );

    let mut capped = api_config(addr);
    capped.repo_max_tree_entries = 2;
    let capped_backend = GitHubApiBackend::new(&capped);
    let tree = capped_backend.tree(&rf, "src").await.unwrap();
    assert_eq!(tree.len(), 2, "tree result is explicitly capped");
    assert!(tree.iter().all(|entry| entry.path.starts_with("src")));
    assert_eq!(tree[0].path, "src");
    assert_eq!(tree[1].path, "src/lib.rs");
}

#[tokio::test]
async fn api_backend_reads_ranges_and_detects_truncation_and_binary() {
    let (addr, _) = spawn_github_mock().await;
    let rf = RepoRef::parse("acme/widget@main").unwrap();
    let backend = GitHubApiBackend::new(&api_config(addr));

    let ranged = backend
        .read(&rf, "src/lib.rs", Some(2), Some(2))
        .await
        .unwrap();
    assert_eq!(
        ranged.lines.iter().map(|line| line.n).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(ranged.text, "line two\nline three");
    assert_eq!(ranged.total_lines, 5);
    assert!(ranged.truncated);
    assert!(!ranged.binary);
    assert_eq!(ranged.language_hint.as_deref(), Some("rust"));

    let full = backend
        .read(&rf, "src/lib.rs", None, Some(100))
        .await
        .unwrap();
    assert_eq!(full.lines.len(), 5);
    assert_eq!(full.lines[0].n, 1);
    assert!(!full.truncated);

    let mut small = api_config(addr);
    small.repo_max_file_bytes = 8;
    let small_backend = GitHubApiBackend::new(&small);
    let binary = small_backend
        .read(&rf, "big.bin", None, None)
        .await
        .unwrap();
    assert!(binary.binary);
    assert!(binary.truncated);
    assert!(binary.text.is_empty());
    assert!(binary.lines.is_empty());
}

#[tokio::test]
async fn api_backend_sends_token_only_as_a_header() {
    let (addr, state) = spawn_github_mock().await;
    let rf = RepoRef::parse("acme/widget@main").unwrap();
    let mut config = api_config(addr);
    config.github_token = Some("test-token".to_owned());
    let backend = GitHubApiBackend::new(&config);

    let metadata = backend.metadata(&rf).await.unwrap();
    assert_eq!(metadata.default_branch, "main");
    assert_eq!(metadata.size_kb, Some(42));
    let seen = state.seen.lock().unwrap();
    assert!(
        seen.authorization
            .iter()
            .any(|value| value == "Bearer test-token"),
        "token is sent as a header, never in a URL or response"
    );
    drop(seen);
    // The configured token must not leak into error text either.
    let error = backend
        .metadata(&RepoRef::parse("acme/missing").unwrap())
        .await
        .expect_err("404 should fail");
    assert!(!error.to_string().contains("test-token"));
}

#[tokio::test]
async fn api_backend_reuses_etag_on_second_call() {
    let (addr, state) = spawn_github_mock().await;
    let rf = RepoRef::parse("acme/widget@main").unwrap();
    let backend = GitHubApiBackend::new(&api_config(addr));

    let first = backend.metadata(&rf).await.unwrap();
    let second = backend.metadata(&rf).await.unwrap();
    assert_eq!(first.default_branch, second.default_branch);
    assert_eq!(state.repo_requests.load(Ordering::SeqCst), 2);
    let seen = state.seen.lock().unwrap();
    assert!(seen.if_none_match.iter().any(|value| value == "\"v1\""));
}

#[tokio::test]
async fn api_backend_maps_upstream_error_statuses() {
    let (addr, _) = spawn_github_mock().await;
    let backend = GitHubApiBackend::new(&api_config(addr));

    assert!(matches!(
        backend
            .metadata(&RepoRef::parse("acme/missing").unwrap())
            .await,
        Err(RepoError::NotFound(_))
    ));
    assert!(matches!(
        backend
            .metadata(&RepoRef::parse("acme/forbidden").unwrap())
            .await,
        Err(RepoError::RateLimited(_))
    ));
    assert!(matches!(
        backend
            .metadata(&RepoRef::parse("acme/limited").unwrap())
            .await,
        Err(RepoError::RateLimited(_))
    ));
    assert_eq!(
        backend
            .metadata(&RepoRef::parse("acme/limited").unwrap())
            .await
            .expect_err("rate limits carry a retry hint")
            .retry_after_ms(),
        Some(60_000)
    );
}
