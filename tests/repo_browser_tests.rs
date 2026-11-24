//! Browser UI backend tests against a local GitHub-like HTML fixture server.

use std::net::SocketAddr;

use nexium::Config;
use nexium::engine::css::Viewport;
use nexium::intelligence::{BrowserRepoBackend, RepoBackend, RepoKind, RepoRef};
use nexium::{Browser, PageSession};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ROOT_PAGE: &str = r#"<!DOCTYPE html><html><head><title>widget</title>
<meta name="description" content="demo repository"></head><body>
<h1>widget</h1>
<a href="/acme/widget/tree/main">main</a>
<a href="/acme/widget/tree/main/src">src</a>
<a href="/acme/widget/blob/main/src/lib.rs">lib.rs</a>
<a href="/acme/widget/blob/main/README.md">README.md</a>
<a href="https://example.com/external">external</a>
</body></html>"#;

const SRC_PAGE: &str = r#"<!DOCTYPE html><html><body>
<a href="/acme/widget/tree/main">main</a>
<a href="/acme/widget/tree/main/src">src</a>
<a href="/acme/widget/blob/main/src/lib.rs">lib.rs</a>
<a href="/acme/widget/blob/main/src/main.rs">main.rs</a>
<a href="/acme/widget/tree/main/src/deep">deep</a>
</body></html>"#;

const DEEP_PAGE: &str = r#"<!DOCTYPE html><html><body>
<a href="/acme/widget/tree/main/src/deep">deep</a>
<a href="/acme/widget/blob/main/src/deep/mod.rs">mod.rs</a>
</body></html>"#;

const LIB_PAGE: &str = r#"<!DOCTYPE html><html><body><table>
<tr><td class="blob-code blob-code-inner">pub fn first() {}</td></tr>
<tr><td class="blob-code blob-code-inner"></td></tr>
<tr><td class="blob-code blob-code-inner">pub fn second() {}</td></tr>
<tr><td class="blob-code blob-code-inner">pub fn third() {}</td></tr>
<tr><td class="blob-code blob-code-inner">}</td></tr>
</table></body></html>"#;

const PLAIN_PAGE: &str = r#"<!DOCTYPE html><html><body>
<p>plain fallback text</p>
</body></html>"#;

async fn spawn_fixture() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8 * 1024];
                let Ok(size) = stream.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..size]);
                let path = request
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");
                let body = match path {
                    "/acme/widget" | "/acme/widget/tree/main" => ROOT_PAGE,
                    "/acme/widget/tree/main/src" => SRC_PAGE,
                    "/acme/widget/tree/main/src/deep" => DEEP_PAGE,
                    "/acme/widget/blob/main/src/lib.rs" => LIB_PAGE,
                    "/acme/widget/blob/main/plain.txt" => PLAIN_PAGE,
                    "/acme/widget/blob/main/src/main.rs"
                    | "/acme/widget/blob/main/src/deep/mod.rs" => {
                        "<html><body><td class=\"blob-code\">fn nested() {}</td></body></html>"
                    }
                    _ => "<html><body><h1>fixture page</h1></body></html>",
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

fn browser_config(addr: SocketAddr) -> Config {
    let mut config = Config::default();
    config.cache_enabled = false;
    config.javascript_enabled = false;
    config.repo_web_base = Some(format!("http://{addr}"));
    config
}

#[tokio::test]
async fn browser_backend_lists_directories_and_files() {
    let addr = spawn_fixture().await;
    let backend = BrowserRepoBackend::new(&browser_config(addr));
    assert_eq!(backend.name(), "browser");
    let rf = RepoRef::parse("acme/widget@main").unwrap();

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
            .any(|entry| entry.path == "src/lib.rs" && entry.kind == RepoKind::File)
    );
    assert!(
        listing.entries.iter().all(|entry| !entry
            .url
            .as_deref()
            .unwrap_or("")
            .contains("example.com"))
    );

    let nested = backend.list(&rf, "src").await.unwrap();
    assert!(
        nested
            .entries
            .iter()
            .all(|entry| entry.path.starts_with("src/") || entry.path == "src")
    );
    assert!(
        nested
            .entries
            .iter()
            .any(|entry| entry.path == "src/lib.rs")
    );
}

#[tokio::test]
async fn browser_backend_reads_numbered_code_lines() {
    let addr = spawn_fixture().await;
    let backend = BrowserRepoBackend::new(&browser_config(addr));
    let rf = RepoRef::parse("acme/widget@main").unwrap();

    let ranged = backend
        .read(&rf, "src/lib.rs", Some(2), Some(2))
        .await
        .unwrap();
    assert_eq!(
        ranged.lines.iter().map(|line| line.n).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(ranged.lines[0].text, "");
    assert_eq!(ranged.lines[1].text, "pub fn second() {}");
    assert_eq!(ranged.text, "\npub fn second() {}");
    assert_eq!(ranged.total_lines, 5);
    assert!(ranged.truncated);
    assert!(!ranged.binary);
    assert_eq!(ranged.language_hint.as_deref(), Some("rust"));

    let full = backend.read(&rf, "src/lib.rs", None, None).await.unwrap();
    assert_eq!(full.lines.len(), 5);
    assert!(!full.truncated);

    let fallback = backend.read(&rf, "plain.txt", None, None).await.unwrap();
    assert_eq!(fallback.text, "plain fallback text");
}

#[tokio::test]
async fn browser_backend_tree_finds_nested_entries_and_caps_length() {
    let addr = spawn_fixture().await;
    let backend = BrowserRepoBackend::new(&browser_config(addr));
    let rf = RepoRef::parse("acme/widget@main").unwrap();

    let entries = backend.tree(&rf, "").await.unwrap();
    let paths = entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"src"), "{paths:?}");
    assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
    assert!(paths.contains(&"src/main.rs"), "{paths:?}");
    assert!(paths.contains(&"src/deep"), "{paths:?}");
    assert!(paths.contains(&"src/deep/mod.rs"), "{paths:?}");
    assert!(paths.contains(&"README.md"), "{paths:?}");
    assert!(paths.windows(2).all(|pair| pair[0] <= pair[1]));

    let mut capped_config = browser_config(addr);
    capped_config.repo_max_tree_entries = 3;
    let capped = BrowserRepoBackend::new(&capped_config)
        .tree(&rf, "")
        .await
        .unwrap();
    assert_eq!(
        capped.len(),
        3,
        "truncation is expressed by a capped length"
    );
}

#[tokio::test]
async fn browser_backend_reads_metadata_from_ui_links() {
    let addr = spawn_fixture().await;
    let backend = BrowserRepoBackend::new(&browser_config(addr));
    let rf = RepoRef::parse("acme/widget").unwrap();

    let metadata = backend.metadata(&rf).await.unwrap();
    assert_eq!(metadata.default_branch, "main");
    assert_eq!(metadata.description.as_deref(), Some("demo repository"));
    assert_eq!(metadata.visibility, "public");
    assert_eq!(metadata.size_kb, None);
}

#[tokio::test]
async fn clicking_a_folder_link_navigates_to_the_fixture_url() {
    let addr = spawn_fixture().await;
    let config = browser_config(addr);
    let browser = Browser::new(config);
    let url = format!("http://{addr}/acme/widget");
    let page = browser.open(&url).await.unwrap();
    let mut session = PageSession::new(
        page,
        Viewport {
            width: 1280.0,
            height: 720.0,
        },
    );

    let link = session
        .elements()
        .into_iter()
        .find(|element| {
            element
                .href
                .as_deref()
                .is_some_and(|href| href.ends_with("/acme/widget/tree/main/src"))
        })
        .expect("the src folder anchor should be present");
    let click = session.click(
        link.rect.x + link.rect.width / 2.0,
        link.rect.y + link.rect.height / 2.0,
    );
    assert!(
        click
            .href
            .as_deref()
            .is_some_and(|href| href.ends_with("/acme/widget/tree/main/src"))
    );
    let navigation = session
        .take_navigation()
        .expect("link click records navigation");
    assert_eq!(
        navigation.url,
        format!("http://{addr}/acme/widget/tree/main/src")
    );

    let loaded = session
        .follow_link(browser.client(), &navigation.url)
        .await
        .unwrap();
    assert_eq!(loaded.url.path(), "/acme/widget/tree/main/src");
    assert!(loaded.document.serialize().contains("main.rs"));
}
