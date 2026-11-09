mod common;

use std::time::Duration;

use common::spawn_test_server;
use nexium::browser::Page;
use nexium::networking::parse_url;
use nexium::{Config, HttpClient, NxError, Request};

fn client() -> HttpClient {
    HttpClient::new(&Config::default())
}

fn url(addr: &std::net::SocketAddr, path: &str) -> url::Url {
    parse_url(&format!("http://{addr}{path}")).unwrap()
}

#[tokio::test]
async fn fetches_html_with_headers() {
    let addr = spawn_test_server().await;
    let response = client().fetch(Request::get(url(&addr, "/"))).await.unwrap();
    assert_eq!(response.status.as_u16(), 200);
    assert!(String::from_utf8_lossy(&response.body).contains("Hello"));
    assert!(response.is_html());
    assert_eq!(response.charset().as_deref(), Some("utf-8"));
    assert!(response.header("content-length").is_some());
}

#[tokio::test]
async fn propagates_error_status() {
    let addr = spawn_test_server().await;
    let response = client()
        .fetch(Request::get(url(&addr, "/missing")))
        .await
        .unwrap();
    assert_eq!(response.status.as_u16(), 404);
}

#[tokio::test]
async fn follows_relative_redirects() {
    let addr = spawn_test_server().await;
    let response = client()
        .fetch(Request::get(url(&addr, "/redirect")))
        .await
        .unwrap();
    assert_eq!(response.status.as_u16(), 200);
    assert_eq!(response.url.path(), "/final");
    assert_eq!(response.redirects.len(), 1);
    assert!(String::from_utf8_lossy(&response.body).contains("final"));
}

#[tokio::test]
async fn detects_redirect_loops() {
    let addr = spawn_test_server().await;
    let error = client()
        .fetch(Request::get(url(&addr, "/loop")).max_redirects(3))
        .await
        .unwrap_err();
    assert!(matches!(error, NxError::TooManyRedirects(3)));
}

#[tokio::test]
async fn enforces_response_size_limit() {
    let client = HttpClient::new(&Config {
        max_response_bytes: 4096,
        ..Config::default()
    });
    let addr = spawn_test_server().await;
    let error = client
        .fetch(Request::get(url(&addr, "/big")))
        .await
        .unwrap_err();
    assert!(matches!(error, NxError::ResponseTooLarge(4096)));
}

#[tokio::test]
async fn times_out_slow_responses() {
    let addr = spawn_test_server().await;
    let result = client()
        .fetch(Request::get(url(&addr, "/slow")).timeout(Duration::from_millis(200)))
        .await;
    assert!(matches!(result, Err(NxError::Timeout(_))));
}

#[tokio::test]
async fn decodes_latin1_pages() {
    let addr = spawn_test_server().await;
    let response = client()
        .fetch(Request::get(url(&addr, "/latin1")))
        .await
        .unwrap();
    let page = Page::from_response(response);
    assert!(
        page.document
            .text_content(page.document.document_node())
            .contains("café")
    );
    assert_eq!(page.charset, "windows-1252");
}

#[test]
fn parse_url_accepts_bare_hosts() {
    assert_eq!(
        parse_url("example.com").unwrap().as_str(),
        "https://example.com/"
    );
    assert_eq!(
        parse_url("http://example.com/a?b=1").unwrap().as_str(),
        "http://example.com/a?b=1"
    );
    assert_eq!(
        parse_url("  localhost:8080/x  ").unwrap().as_str(),
        "https://localhost:8080/x"
    );
}

#[test]
fn parse_url_rejects_empty_and_unsupported_schemes() {
    assert!(matches!(parse_url(""), Err(NxError::InvalidUrl(_))));
    assert!(matches!(
        parse_url("ftp://example.com/file"),
        Err(NxError::UnsupportedScheme(_))
    ));
}

#[tokio::test]
async fn decompresses_gzip_deflate_and_brotli() {
    let addr = spawn_test_server().await;
    for (path, expected) in [
        ("/gzip", "gzip body"),
        ("/deflate", "deflate body"),
        ("/brotli", "brotli body"),
    ] {
        let response = client()
            .fetch(Request::get(url(&addr, path)))
            .await
            .unwrap();
        assert_eq!(response.status.as_u16(), 200);
        let body = String::from_utf8_lossy(&response.body);
        assert!(body.contains(expected), "{path}: {body}");
        assert!(response.header("content-encoding").is_none());
    }
}

#[tokio::test]
async fn advertises_supported_encodings() {
    let addr = spawn_test_server().await;
    let response = client()
        .fetch(Request::get(url(&addr, "/encoding")))
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&response.body);
    assert!(body.contains("gzip"));
    assert!(body.contains("br"));
    assert!(body.contains("deflate"));
}

#[tokio::test]
async fn invalid_compressed_bodies_are_reported() {
    let addr = spawn_test_server().await;
    let error = client()
        .fetch(Request::get(url(&addr, "/bad-gzip")))
        .await
        .unwrap_err();
    assert!(matches!(error, NxError::Network(_)), "{error:?}");
}

#[tokio::test]
async fn cookies_are_stored_and_sent_back() {
    let addr = spawn_test_server().await;
    let client = client();
    client
        .fetch(Request::get(url(&addr, "/set-cookie")))
        .await
        .unwrap();
    assert_eq!(client.cookie_count(), 1);
    let response = client
        .fetch(Request::get(url(&addr, "/echo-cookie")))
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&response.body);
    assert!(body.contains("session=abc123"), "{body}");
}

#[tokio::test]
async fn cookie_paths_and_secure_flag_are_respected() {
    let addr = spawn_test_server().await;
    let client = client();
    client
        .fetch(Request::get(url(&addr, "/set-cookie-scoped")))
        .await
        .unwrap();
    let root = client
        .fetch(Request::get(url(&addr, "/echo-cookie")))
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&root.body).contains("scoped=1"));
    let scoped = client
        .fetch(Request::get(url(&addr, "/private/echo")))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&scoped.body).contains("scoped=1"));

    client
        .fetch(Request::get(url(&addr, "/set-cookie-secure")))
        .await
        .unwrap();
    let echo = client
        .fetch(Request::get(url(&addr, "/echo-cookie")))
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&echo.body).contains("secure=1"));
}

#[tokio::test]
async fn cookies_can_be_deleted_and_cleared() {
    let addr = spawn_test_server().await;
    let client = client();
    client
        .fetch(Request::get(url(&addr, "/set-cookie")))
        .await
        .unwrap();
    client
        .fetch(Request::get(url(&addr, "/delete-cookie")))
        .await
        .unwrap();
    assert_eq!(client.cookie_count(), 0);
    client
        .fetch(Request::get(url(&addr, "/set-cookie")))
        .await
        .unwrap();
    client.clear_cookies();
    assert_eq!(client.cookie_count(), 0);
}

#[tokio::test]
async fn cookies_can_be_disabled() {
    let config = Config {
        cookies_enabled: false,
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let addr = spawn_test_server().await;
    client
        .fetch(Request::get(url(&addr, "/set-cookie")))
        .await
        .unwrap();
    assert_eq!(client.cookie_count(), 0);
    let response = client
        .fetch(Request::get(url(&addr, "/echo-cookie")))
        .await
        .unwrap();
    assert!(response.body.is_empty());
}

#[tokio::test]
async fn fresh_get_responses_are_cacheable() {
    use std::sync::atomic::Ordering;

    let (addr, count) = common::spawn_counting_server("max-age=60").await;
    let client = client();
    let first = client
        .fetch(Request::get(
            parse_url(&format!("http://{addr}/page")).unwrap(),
        ))
        .await
        .unwrap();
    let second = client
        .fetch(Request::get(
            parse_url(&format!("http://{addr}/page")).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(first.body, second.body);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(client.cache_len(), 1);

    client.clear_cache();
    client
        .fetch(Request::get(
            parse_url(&format!("http://{addr}/page")).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn no_store_and_disabled_cache_bypass_storage() {
    use std::sync::atomic::Ordering;

    let (addr, count) = common::spawn_counting_server("no-store").await;
    let client = client();
    let url = parse_url(&format!("http://{addr}/page")).unwrap();
    client.fetch(Request::get(url.clone())).await.unwrap();
    client.fetch(Request::get(url)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(client.cache_len(), 0);

    let (addr, count) = common::spawn_counting_server("max-age=60").await;
    let config = Config {
        cache_enabled: false,
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let url = parse_url(&format!("http://{addr}/page")).unwrap();
    client.fetch(Request::get(url.clone())).await.unwrap();
    client.fetch(Request::get(url)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_keys_include_the_query_string() {
    use std::sync::atomic::Ordering;

    let (addr, count) = common::spawn_counting_server("max-age=60").await;
    let client = client();
    for path in ["/page", "/page?x=1"] {
        client
            .fetch(Request::get(
                parse_url(&format!("http://{addr}{path}")).unwrap(),
            ))
            .await
            .unwrap();
    }
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(client.cache_len(), 2);
}

#[tokio::test]
async fn expired_cache_entries_are_refetched() {
    use std::sync::atomic::Ordering;

    let (addr, count) = common::spawn_counting_server("max-age=1").await;
    let client = client();
    let url = parse_url(&format!("http://{addr}/page")).unwrap();
    client.fetch(Request::get(url.clone())).await.unwrap();
    client.fetch(Request::get(url.clone())).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    client.fetch(Request::get(url)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn http_requests_can_go_through_a_connect_proxy() {
    use std::sync::atomic::Ordering;

    let target = spawn_test_server().await;
    let (proxy, connections) = common::spawn_connect_proxy().await;
    let config = Config {
        proxy: Some(format!("http://{proxy}")),
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

struct BlockInterceptor {
    responses: std::sync::atomic::AtomicUsize,
}

impl nexium::networking::client::RequestInterceptor for BlockInterceptor {
    fn intercept_request(
        &self,
        request: &mut nexium::Request,
    ) -> nexium::networking::client::InterceptDecision {
        if request.url.path() == "/blocked" {
            return nexium::networking::client::InterceptDecision::Respond(Box::new(
                nexium::Response {
                    url: request.url.clone(),
                    status: hyper::StatusCode::OK,
                    headers: hyper::HeaderMap::new(),
                    body: bytes::Bytes::from_static(b"intercepted"),
                    redirects: Vec::new(),
                },
            ));
        }
        request.headers.insert(
            "x-intercepted",
            hyper::header::HeaderValue::from_static("1"),
        );
        nexium::networking::client::InterceptDecision::Continue
    }

    fn intercept_response(&self, _request: &nexium::Request, response: &mut nexium::Response) {
        self.responses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        response
            .headers
            .insert("x-seen", hyper::header::HeaderValue::from_static("yes"));
    }
}

#[tokio::test]
async fn request_interception_can_block_modify_and_inspect() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let addr = spawn_test_server().await;
    let client = client();
    let interceptor = Arc::new(BlockInterceptor {
        responses: AtomicUsize::new(0),
    });
    client.set_interceptor(Some(interceptor.clone()));

    let blocked = client
        .fetch(Request::get(url(&addr, "/blocked")))
        .await
        .unwrap();
    assert_eq!(&blocked.body[..], b"intercepted");
    assert_eq!(interceptor.responses.load(Ordering::SeqCst), 0);

    let response = client.fetch(Request::get(url(&addr, "/"))).await.unwrap();
    assert!(String::from_utf8_lossy(&response.body).contains("Hello"));
    assert_eq!(response.header("x-seen"), Some("yes"));
    assert_eq!(interceptor.responses.load(Ordering::SeqCst), 1);
}
