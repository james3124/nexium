//! Phase 1 networking coverage: SameSite/public-suffix cookies, conditional
//! cache revalidation and SOCKS5 proxying.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::spawn_test_server;
use nexium::networking::SameSite;
use nexium::networking::cookies::{CookieJar, is_public_suffix, registrable_domain};
use nexium::networking::parse_url;
use nexium::{Config, HttpClient, NxError, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// SameSite / public-suffix helpers and tests
// ---------------------------------------------------------------------------

/// Cookie test server that can be reached over several loopback addresses.
async fn spawn_cookie_server() -> SocketAddr {
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(read) = stream.read(&mut buf).await else {
                    return;
                };
                if read == 0 {
                    return;
                }
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let mut lines = request.lines();
                let path = lines
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_owned();
                let cookie = lines
                    .clone()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.trim()
                            .eq_ignore_ascii_case("cookie")
                            .then(|| value.trim().to_owned())
                    })
                    .unwrap_or_default();
                let (extra_headers, body) = match path.as_str() {
                    "/" => ("", "<html><body>document</body></html>".to_owned()),
                    "/set-strict" => (
                        "Set-Cookie: strict=1; Path=/; SameSite=Strict\r\n",
                        "ok".to_owned(),
                    ),
                    "/set-lax" => (
                        "Set-Cookie: lax=1; Path=/; SameSite=Lax\r\n",
                        "ok".to_owned(),
                    ),
                    "/set-none" => (
                        "Set-Cookie: none=1; Path=/; SameSite=None\r\n",
                        "ok".to_owned(),
                    ),
                    "/set-none-secure" => (
                        "Set-Cookie: secure-none=1; Path=/; SameSite=None; Secure\r\n",
                        "ok".to_owned(),
                    ),
                    "/set-default" => ("Set-Cookie: default=1; Path=/\r\n", "ok".to_owned()),
                    "/echo" => ("", cookie),
                    _ => ("", "missing".to_owned()),
                };
                let status = if path == "/" || path.starts_with("/set-") || path == "/echo" {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

async fn fetch_text(client: &HttpClient, url: &str, accept: Option<&str>) -> String {
    let mut request = Request::get(parse_url(url).unwrap());
    if let Some(accept) = accept {
        request = request.header("Accept", accept).unwrap();
    }
    let response = client.fetch(request).await.unwrap();
    String::from_utf8_lossy(&response.body).into_owned()
}

#[tokio::test]
async fn samesite_none_requires_secure() {
    let addr = spawn_cookie_server().await;
    let client = HttpClient::new(&Config::default());
    let base = format!("http://127.0.0.1:{}", addr.port());

    client
        .fetch(Request::get(
            parse_url(&format!("{base}/set-none")).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(
        client.cookie_count(),
        0,
        "SameSite=None without Secure must be dropped"
    );

    client
        .fetch(Request::get(
            parse_url(&format!("{base}/set-none-secure")).unwrap(),
        ))
        .await
        .unwrap();
    let records = client.cookie_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].same_site, SameSite::None);
    assert!(records[0].secure);
    // Secure cookies are not sent over plain http.
    let echo = fetch_text(&client, &format!("{base}/echo"), None).await;
    assert!(!echo.contains("secure-none=1"), "{echo}");
}

#[tokio::test]
async fn samesite_strict_and_lax_follow_navigation_context() {
    let addr = spawn_cookie_server().await;
    let client = HttpClient::new(&Config::default());
    let first = format!("http://127.0.0.1:{}", addr.port());
    let second = format!("http://127.0.0.2:{}", addr.port());

    // Establish a top-level document at the first site.
    let document = fetch_text(&client, &first, Some("text/html,application/xhtml+xml")).await;
    assert!(document.contains("document"), "{document}");

    for path in ["/set-strict", "/set-lax", "/set-default"] {
        for base in [&first, &second] {
            client
                .fetch(Request::get(parse_url(&format!("{base}{path}")).unwrap()))
                .await
                .unwrap();
        }
    }

    // Same-site sub-resource: Strict, Lax and default-Lax cookies are sent.
    let same_site = fetch_text(&client, &format!("{first}/echo"), None).await;
    assert!(same_site.contains("strict=1"), "{same_site}");
    assert!(same_site.contains("lax=1"), "{same_site}");
    assert!(same_site.contains("default=1"), "{same_site}");

    // Cross-site sub-resource: only SameSite=None would be sent; here none.
    let cross_site = fetch_text(&client, &format!("{second}/echo"), None).await;
    assert!(!cross_site.contains("strict=1"), "{cross_site}");
    assert!(!cross_site.contains("lax=1"), "{cross_site}");
    assert!(!cross_site.contains("default=1"), "{cross_site}");

    // Cross-site top-level GET navigation: Lax (including the default) is
    // allowed, Strict is not.
    let cross_navigation = fetch_text(
        &client,
        &format!("{second}/echo"),
        Some("text/html,application/xhtml+xml"),
    )
    .await;
    assert!(!cross_navigation.contains("strict=1"), "{cross_navigation}");
    assert!(cross_navigation.contains("lax=1"), "{cross_navigation}");
    assert!(cross_navigation.contains("default=1"), "{cross_navigation}");
}

fn set_cookie(jar: &mut CookieJar, origin: &str, cookie: &str) {
    let mut headers = hyper::HeaderMap::new();
    headers.insert("set-cookie", cookie.parse().unwrap());
    jar.set_from_headers(&url::Url::parse(origin).unwrap(), &headers);
}

#[test]
fn public_suffix_domains_are_rejected_or_normalized() {
    let mut jar = CookieJar::new(true);

    set_cookie(&mut jar, "https://www.example.com/", "a=b; Domain=com");
    assert!(jar.is_empty(), "bare TLD Domain must be rejected");
    set_cookie(&mut jar, "https://www.example.co.uk/", "a=b; Domain=co.uk");
    assert!(jar.is_empty(), "multi-label public suffix must be rejected");
    set_cookie(
        &mut jar,
        "https://www.example.com/",
        "a=b; Domain=other.com",
    );
    assert!(jar.is_empty(), "non-matching Domain must be rejected");
    set_cookie(
        &mut jar,
        "https://www.example.co.uk/",
        "a=b; Domain=.example.co.uk",
    );
    assert_eq!(jar.len(), 1, "leading dot is normalized");
    assert_eq!(
        jar.cookie_header(&url::Url::parse("https://shop.example.co.uk/").unwrap()),
        Some("a=b".to_owned())
    );
    set_cookie(
        &mut jar,
        "https://www.example.com/",
        "c=d; Domain=www.example.com",
    );
    assert_eq!(jar.len(), 2);
    let records = jar.records();
    let www = records
        .iter()
        .find(|record| record.name == "c")
        .expect("cookie c");
    assert!(www.host_only, "Domain equal to the host is host-only");
}

#[test]
fn registrable_domain_handles_multi_label_suffixes() {
    assert_eq!(registrable_domain("www.example.co.uk"), "example.co.uk");
    assert_eq!(registrable_domain("a.b.example.com"), "example.com");
    assert_eq!(registrable_domain("shop.example.org.au"), "example.org.au");
    assert_eq!(registrable_domain("www.example.co.jp"), "example.co.jp");
    assert_eq!(registrable_domain("localhost"), "localhost");
    assert_eq!(registrable_domain("127.0.0.1"), "127.0.0.1");
    assert!(is_public_suffix("com"));
    assert!(is_public_suffix("co.uk"));
    assert!(!is_public_suffix("example.co.uk"));
}

// ---------------------------------------------------------------------------
// Conditional cache revalidation
// ---------------------------------------------------------------------------

async fn spawn_revalidation_server() -> (SocketAddr, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let conditionals = Arc::new(AtomicUsize::new(0));
    let request_counter = Arc::clone(&requests);
    let conditional_counter = Arc::clone(&conditionals);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let request_counter = Arc::clone(&request_counter);
            let conditional_counter = Arc::clone(&conditional_counter);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(read) = stream.read(&mut buf).await else {
                    return;
                };
                if read == 0 {
                    return;
                }
                request_counter.fetch_add(1, Ordering::SeqCst);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let mut lines = request.lines();
                let path = lines
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
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
                let if_none_match = header("if-none-match");
                let if_modified_since = header("if-modified-since");
                if if_none_match.is_some() || if_modified_since.is_some() {
                    conditional_counter.fetch_add(1, Ordering::SeqCst);
                }

                let (status, headers, body) = match path.as_str() {
                    "/etag" => {
                        if if_none_match
                            .as_deref()
                            .is_some_and(|value| value.contains("\"v1\""))
                        {
                            (
                                "304 Not Modified",
                                "ETag: \"v1\"\r\nCache-Control: max-age=60\r\n".to_owned(),
                                Vec::new(),
                            )
                        } else {
                            (
                                "200 OK",
                                "ETag: \"v1\"\r\nCache-Control: max-age=1\r\nContent-Type: text/plain\r\n"
                                    .to_owned(),
                                b"etag-body".to_vec(),
                            )
                        }
                    }
                    "/last-modified" => {
                        let modified = "Wed, 21 Oct 2015 07:28:00 GMT";
                        if if_modified_since.is_some() {
                            (
                                "304 Not Modified",
                                format!(
                                    "Last-Modified: {modified}\r\nCache-Control: max-age=60\r\n"
                                ),
                                Vec::new(),
                            )
                        } else {
                            (
                                "200 OK",
                                format!(
                                    "Last-Modified: {modified}\r\nCache-Control: max-age=1\r\nContent-Type: text/plain\r\n"
                                ),
                                b"modified-body".to_vec(),
                            )
                        }
                    }
                    "/etag-changed" => {
                        if if_none_match
                            .as_deref()
                            .is_some_and(|value| value.contains("\"v1\""))
                        {
                            (
                                "200 OK",
                                "ETag: \"v2\"\r\nCache-Control: max-age=60\r\nContent-Type: text/plain\r\n"
                                    .to_owned(),
                                b"new-body".to_vec(),
                            )
                        } else {
                            (
                                "200 OK",
                                "ETag: \"v1\"\r\nCache-Control: max-age=1\r\nContent-Type: text/plain\r\n"
                                    .to_owned(),
                                b"old-body".to_vec(),
                            )
                        }
                    }
                    _ => ("404 Not Found", String::new(), b"missing".to_vec()),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut out = response.into_bytes();
                out.extend_from_slice(&body);
                let _ = stream.write_all(&out).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (addr, requests, conditionals)
}

#[tokio::test]
async fn etag_stale_entry_is_revalidated_with_304() {
    let (addr, requests, conditionals) = spawn_revalidation_server().await;
    let client = HttpClient::new(&Config::default());
    let url = parse_url(&format!("http://{addr}/etag")).unwrap();

    let first = client.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(first.status.as_u16(), 200);
    assert_eq!(first.body.as_ref(), b"etag-body");

    tokio::time::sleep(Duration::from_millis(1100)).await;
    let revalidated = client.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(revalidated.status.as_u16(), 200, "304 must be transparent");
    assert_eq!(revalidated.body.as_ref(), b"etag-body");
    assert_eq!(conditionals.load(Ordering::SeqCst), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 2);

    // The 304 refreshed freshness, so the next request is served from cache.
    let cached = client.fetch(Request::get(url)).await.unwrap();
    assert_eq!(cached.body.as_ref(), b"etag-body");
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(client.cache_len(), 1);
}

#[tokio::test]
async fn last_modified_stale_entry_is_revalidated_with_304() {
    let (addr, requests, conditionals) = spawn_revalidation_server().await;
    let client = HttpClient::new(&Config::default());
    let url = parse_url(&format!("http://{addr}/last-modified")).unwrap();

    let first = client.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(first.body.as_ref(), b"modified-body");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let revalidated = client.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(revalidated.status.as_u16(), 200);
    assert_eq!(revalidated.body.as_ref(), b"modified-body");
    assert_eq!(conditionals.load(Ordering::SeqCst), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    client.fetch(Request::get(url)).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn conditional_request_with_changed_validator_replaces_entry() {
    let (addr, requests, conditionals) = spawn_revalidation_server().await;
    let client = HttpClient::new(&Config::default());
    let url = parse_url(&format!("http://{addr}/etag-changed")).unwrap();

    let first = client.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(first.body.as_ref(), b"old-body");
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let replaced = client.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(replaced.status.as_u16(), 200);
    assert_eq!(replaced.body.as_ref(), b"new-body");
    assert_eq!(replaced.header("etag"), Some("\"v2\""));
    assert_eq!(conditionals.load(Ordering::SeqCst), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 2);

    let cached = client.fetch(Request::get(url)).await.unwrap();
    assert_eq!(cached.body.as_ref(), b"new-body");
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn validators_are_persisted_on_disk() {
    let (addr, requests, conditionals) = spawn_revalidation_server().await;
    let dir = std::env::temp_dir().join(format!(
        "nexium-revalidation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let url = parse_url(&format!("http://{addr}/etag")).unwrap();

    let first = HttpClient::new(&Config::default());
    first.set_cache_dir(&dir).unwrap();
    let response = first.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(response.body.as_ref(), b"etag-body");
    drop(first);
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let second = HttpClient::new(&Config::default());
    second.set_cache_dir(&dir).unwrap();
    let revalidated = second.fetch(Request::get(url)).await.unwrap();
    assert_eq!(revalidated.body.as_ref(), b"etag-body");
    assert_eq!(
        conditionals.load(Ordering::SeqCst),
        1,
        "disk validators were not used"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// SOCKS5 proxy
// ---------------------------------------------------------------------------

/// Minimal SOCKS5 server (RFC 1928 + RFC 1929) that tunnels to the requested
/// authority. `auth` enables username/password authentication.
async fn spawn_fake_socks5(
    auth: Option<(&'static str, &'static str)>,
) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let _ = handle_socks5(&mut stream, auth).await;
            });
        }
    });
    (addr, connections)
}

async fn handle_socks5(
    stream: &mut TcpStream,
    auth: Option<(&'static str, &'static str)>,
) -> std::io::Result<()> {
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting[0] != 0x05 {
        return Ok(());
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    stream.read_exact(&mut methods).await?;
    let method = if auth.is_some() {
        if methods.contains(&0x02) { 0x02 } else { 0xff }
    } else if methods.contains(&0x00) {
        0x00
    } else {
        0xff
    };
    stream.write_all(&[0x05, method]).await?;
    if method == 0xff {
        return Ok(());
    }
    if method == 0x02 {
        let mut version = [0u8; 1];
        stream.read_exact(&mut version).await?;
        let mut user_len = [0u8; 1];
        stream.read_exact(&mut user_len).await?;
        let mut username = vec![0u8; user_len[0] as usize];
        stream.read_exact(&mut username).await?;
        let mut pass_len = [0u8; 1];
        stream.read_exact(&mut pass_len).await?;
        let mut password = vec![0u8; pass_len[0] as usize];
        stream.read_exact(&mut password).await?;
        let (expected_user, expected_pass) = auth.unwrap_or(("", ""));
        if username == expected_user.as_bytes() && password == expected_pass.as_bytes() {
            stream.write_all(&[0x01, 0x00]).await?;
        } else {
            stream.write_all(&[0x01, 0x01]).await?;
            return Ok(());
        }
    }

    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await?;
    if request[0] != 0x05 || request[1] != 0x01 {
        return Ok(());
    }
    let target = match request[3] {
        0x01 => {
            let mut address = [0u8; 4];
            stream.read_exact(&mut address).await?;
            let mut port = [0u8; 2];
            stream.read_exact(&mut port).await?;
            format!(
                "{}.{}.{}.{}:{}",
                address[0],
                address[1],
                address[2],
                address[3],
                u16::from_be_bytes(port)
            )
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut host = vec![0u8; len[0] as usize];
            stream.read_exact(&mut host).await?;
            let mut port = [0u8; 2];
            stream.read_exact(&mut port).await?;
            format!(
                "{}:{}",
                String::from_utf8_lossy(&host),
                u16::from_be_bytes(port)
            )
        }
        0x04 => {
            let mut address = [0u8; 16];
            stream.read_exact(&mut address).await?;
            let mut port = [0u8; 2];
            stream.read_exact(&mut port).await?;
            format!(
                "[{}]:{}",
                std::net::Ipv6Addr::from(address),
                u16::from_be_bytes(port)
            )
        }
        _ => return Ok(()),
    };
    let mut upstream = TcpStream::connect(&target).await?;
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .await?;
    tokio::io::copy_bidirectional(stream, &mut upstream).await?;
    Ok(())
}

#[tokio::test]
async fn socks5h_proxy_tunnels_http_requests() {
    let target = spawn_test_server().await;
    let (proxy, connections) = spawn_fake_socks5(None).await;
    let config = Config {
        proxy: Some(format!("socks5h://{proxy}")),
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let response = client
        .fetch(Request::get(
            parse_url(&format!("http://{target}/")).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status.as_u16(), 200);
    assert!(String::from_utf8_lossy(&response.body).contains("Hello"));
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn socks5_proxy_supports_username_password_auth() {
    let target = spawn_test_server().await;
    let (proxy, connections) = spawn_fake_socks5(Some(("user", "pass"))).await;
    let config = Config {
        proxy: Some(format!("socks5://user:pass@{proxy}")),
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let response = client
        .fetch(Request::get(
            parse_url(&format!("http://{target}/plain")).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status.as_u16(), 200);
    assert_eq!(response.body.as_ref(), b"just text");
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn socks5_proxy_reports_authentication_failure() {
    let target = spawn_test_server().await;
    let (proxy, _) = spawn_fake_socks5(Some(("user", "pass"))).await;
    let config = Config {
        proxy: Some(format!("socks5://user:wrong@{proxy}")),
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let error = client
        .fetch(Request::get(
            parse_url(&format!("http://{target}/plain")).unwrap(),
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, NxError::Network(_)), "{error:?}");
}
