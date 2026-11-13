//! Phase 1 networking gap coverage: proxy bypass (`no_proxy`) rules, SSRF
//! hardening and per-hop re-checks on redirects.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::spawn_test_server;
use nexium::networking::ProxyBypass;
use nexium::networking::ssrf;
use nexium::{Config, HttpClient, Request, parse_url};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Server that answers every request with `302 Location: <location>`.
async fn spawn_redirect_server(location: String) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let location = location.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

/// Proxy that counts connections and immediately drops them. Any request that
/// reaches it fails, so a successful fetch proves the proxy was bypassed.
async fn spawn_dead_proxy() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (addr, connections)
}

/// Public IP literal that the SSRF policy accepts. The proxy answers its
/// CONNECT without touching the network, so tests never leave the process.
const PUBLIC_HOST: &str = "93.184.216.34";

/// HTTP CONNECT proxy used by the SSRF redirect test.
///
/// CONNECTs to [`PUBLIC_HOST`] are answered with `200` and the tunneled
/// request receives a `302` to `location`. Every other authority is tunnelled
/// to the real destination so the same harness works as an SSRF-disabled
/// control run.
async fn spawn_redirecting_proxy(location: String) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                break;
            };
            let counter = Arc::clone(&counter);
            let location = location.clone();
            tokio::spawn(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.ends_with(b"\r\n\r\n") {
                    let Ok(read) = client.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    head.extend_from_slice(&buffer[..read]);
                    if head.len() > 16 * 1024 {
                        return;
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let Some(target) = text
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .map(str::to_owned)
                else {
                    return;
                };
                if target.starts_with(&format!("{PUBLIC_HOST}:")) {
                    let _ = client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await;
                    // Drain the tunneled request head, then answer with the
                    // redirect. No upstream connection is made.
                    let mut inner = Vec::new();
                    let mut byte = [0u8; 1];
                    while !inner.ends_with(b"\r\n\r\n") {
                        let Ok(read) = client.read(&mut byte).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        inner.push(byte[0]);
                    }
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = client.write_all(response.as_bytes()).await;
                    let _ = client.shutdown().await;
                } else {
                    let Ok(mut upstream) = TcpStream::connect(&target).await else {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                        return;
                    };
                    let _ = client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                }
            });
        }
    });
    (addr, connections)
}

// ---------------------------------------------------------------------------
// Bypass list matching
// ---------------------------------------------------------------------------

#[test]
fn bypass_matches_hosts_domains_cidr_and_ports() {
    let bypass = ProxyBypass::from_entries([
        "localhost",
        ".example.com",
        "*.example.org",
        "10.0.0.0/8",
        "fd00::/8",
        "127.0.0.1:8080",
        "203.0.113.9",
        "[2001:db8::1]",
    ]);

    // Exact hostnames, case-insensitive.
    assert!(bypass.matches("localhost", None));
    assert!(bypass.matches("LOCALHOST", Some(80)));
    assert!(!bypass.matches("localhost.example.net", None));

    // `.suffix` and `*.wildcard` cover the apex and subdomains, not lookalikes.
    assert!(bypass.matches("example.com", Some(443)));
    assert!(bypass.matches("api.example.com", Some(443)));
    assert!(!bypass.matches("badexample.com", None));
    assert!(bypass.matches("example.org", None));
    assert!(bypass.matches("cdn.example.org", None));
    assert!(!bypass.matches("notexample.org", None));

    // CIDR, IPv4 and IPv6.
    assert!(bypass.matches("10.0.0.0", None));
    assert!(bypass.matches("10.200.3.4", Some(443)));
    assert!(!bypass.matches("11.0.0.1", None));
    assert!(bypass.matches("fd12:3456::1", None));
    assert!(!bypass.matches("fe80::1", None));

    // Exact IPs (any port) and port-qualified hosts.
    assert!(bypass.matches("203.0.113.9", Some(80)));
    assert!(bypass.matches("2001:db8::1", Some(443)));
    assert!(bypass.matches("127.0.0.1", Some(8080)));
    assert!(!bypass.matches("127.0.0.1", Some(8081)));
    assert!(!bypass.matches("127.0.0.1", None));

    // `*` bypasses everything.
    assert!(ProxyBypass::from_entries(["*"]).matches("anything.example", None));

    // Mixed separators are accepted (environment-variable style).
    assert_eq!(
        ProxyBypass::from_entries(["a.example, b.example; c.example"]).len(),
        3
    );

    // URLs resolve their effective port.
    let bypass = ProxyBypass::from_entries(["localhost:8080"]);
    assert!(bypass.matches_url(&parse_url("http://localhost:8080/x").unwrap()));
    assert!(!bypass.matches_url(&parse_url("https://localhost/x").unwrap()));
}

#[test]
fn bypass_uses_no_proxy_environment_as_fallback() {
    // Configuration entries win over the environment.
    let configured = Config {
        no_proxy: vec!["configured.example".to_owned()],
        ..Config::default()
    };
    let bypass = ProxyBypass::from_config_with_env(&configured, Some("env.example"));
    assert!(bypass.matches("configured.example", None));
    assert!(!bypass.matches("env.example", None));

    // Empty configuration falls back to NO_PROXY / no_proxy text.
    let bypass =
        ProxyBypass::from_config_with_env(&Config::default(), Some("localhost, .corp.example"));
    assert!(bypass.matches("localhost", Some(80)));
    assert!(bypass.matches("intranet.corp.example", None));
    assert!(!bypass.matches("corp.example.net", None));

    // No configuration and no environment means no bypass and no proxy use
    // is suppressed.
    assert!(ProxyBypass::from_config_with_env(&Config::default(), None).is_empty());
}

#[test]
fn config_accepts_no_proxy_as_string_or_list() {
    let dir = std::env::temp_dir().join(format!(
        "nexium-no-proxy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let string_form = dir.join("string.toml");
    std::fs::write(&string_form, "no_proxy = \"localhost, 127.0.0.1\"\n").unwrap();
    let config = Config::load(&string_form).unwrap();
    assert_eq!(config.no_proxy, vec!["localhost, 127.0.0.1".to_owned()]);
    let bypass = ProxyBypass::from_entries(&config.no_proxy);
    assert!(bypass.matches("127.0.0.1", Some(80)));

    let list_form = dir.join("list.toml");
    std::fs::write(&list_form, "no_proxy = [\"localhost\", \"10.0.0.0/8\"]\n").unwrap();
    let config = Config::load(&list_form).unwrap();
    assert_eq!(config.no_proxy.len(), 2);

    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// Direct connections for bypassed hosts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bypassed_host_skips_the_http_connect_proxy() {
    let target = spawn_test_server().await;
    let (proxy, connections) = spawn_dead_proxy().await;
    let config = Config {
        proxy: Some(format!("http://{proxy}")),
        no_proxy: vec![format!("127.0.0.1:{}", target.port())],
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
    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "bypassed host must not reach the CONNECT proxy"
    );
}

#[tokio::test]
async fn bypassed_host_skips_the_socks5_proxy() {
    let target = spawn_test_server().await;
    let (proxy, connections) = spawn_dead_proxy().await;
    let config = Config {
        proxy: Some(format!("socks5h://{proxy}")),
        no_proxy: vec!["127.0.0.1".to_owned()],
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
    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "bypassed host must not reach the SOCKS5 proxy"
    );
}

#[tokio::test]
async fn bypass_is_rechecked_on_redirect_hops() {
    // The first hop is bypassed, the redirect target is not: only the second
    // hop may use the proxy.
    let final_server = spawn_test_server().await;
    let redirect =
        spawn_redirect_server(format!("http://127.0.0.1:{}/", final_server.port())).await;
    let (proxy, connections) = common::spawn_connect_proxy().await;
    let config = Config {
        proxy: Some(format!("http://{proxy}")),
        no_proxy: vec![format!("127.0.0.1:{}", redirect.port())],
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let response = client
        .fetch(Request::get(
            parse_url(&format!("http://{redirect}/")).unwrap(),
        ))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&response.body).contains("Hello"));
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "only the non-bypassed redirect hop may use the proxy"
    );

    // Mirror case: the redirect target is bypassed, the first hop is not.
    let final_server = spawn_test_server().await;
    let redirect =
        spawn_redirect_server(format!("http://127.0.0.1:{}/", final_server.port())).await;
    let (proxy, connections) = common::spawn_connect_proxy().await;
    let config = Config {
        proxy: Some(format!("http://{proxy}")),
        no_proxy: vec![format!("127.0.0.1:{}", final_server.port())],
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let response = client
        .fetch(Request::get(
            parse_url(&format!("http://{redirect}/")).unwrap(),
        ))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&response.body).contains("Hello"));
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "the first hop must use the proxy, the bypassed redirect must not"
    );
}

// ---------------------------------------------------------------------------
// SSRF hardening
// ---------------------------------------------------------------------------

#[test]
fn ssrf_classifies_reserved_addresses() {
    for address in [
        "127.0.0.1",
        "127.255.255.254",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "0.0.0.0",
        "224.0.0.1",
        "100.64.0.1",
        "240.0.0.1",
        "::1",
        "::",
        "fe80::1",
        "fc00::1",
        "ff02::1",
        "::ffff:127.0.0.1",
    ] {
        assert!(
            ssrf::is_blocked_address(address.parse().unwrap()),
            "{address} must be blocked"
        );
    }
    for address in [
        "8.8.8.8",
        "1.1.1.1",
        "93.184.216.34",
        "2606:4700:4700::1111",
        "2001:4860:4860::8888",
    ] {
        assert!(
            !ssrf::is_blocked_address(address.parse().unwrap()),
            "{address} must be allowed"
        );
    }
    assert!(ssrf::is_blocked_host("localhost"));
    assert!(ssrf::is_blocked_host("app.localhost"));
    assert!(ssrf::is_blocked_host("127.0.0.1"));
    assert!(!ssrf::is_blocked_host("93.184.216.34"));
}

#[tokio::test]
async fn ssrf_resolution_rejects_any_private_address() {
    // `localhost` resolves to loopback (and often both loopback families);
    // any blocked answer rejects the whole name.
    let error = ssrf::resolve_and_validate("localhost", 80)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ssrf::SsrfError::Blocked { .. }),
        "{error:?}"
    );

    // Public literals resolve without DNS and are accepted.
    let addresses = ssrf::resolve_and_validate("93.184.216.34", 80)
        .await
        .unwrap();
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].ip().to_string(), "93.184.216.34");
}

#[tokio::test]
async fn ssrf_blocks_literals_and_hostnames_that_resolve_to_loopback() {
    let target = spawn_test_server().await;
    let config = Config {
        block_private_networks: true,
        ..Config::default()
    };
    let client = HttpClient::new(&config);

    for host in ["127.0.0.1", "localhost"] {
        let url = parse_url(&format!("http://{host}:{}/", target.port())).unwrap();
        let error = client.fetch(Request::get(url)).await.unwrap_err();
        assert!(
            error.to_string().contains("private-network"),
            "{host}: {error}"
        );
    }
}

#[tokio::test]
async fn ssrf_is_rechecked_on_redirect_hops() {
    let target = spawn_test_server().await;
    let location = format!("http://localhost:{}/", target.port());
    let (proxy, connections) = spawn_redirecting_proxy(location).await;

    // Control: with SSRF disabled the redirect is followed through the proxy.
    let config = Config {
        proxy: Some(format!("http://{proxy}")),
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let response = client
        .fetch(Request::get(
            parse_url(&format!("http://{PUBLIC_HOST}/redirect")).unwrap(),
        ))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&response.body).contains("Hello"));
    assert_eq!(connections.load(Ordering::SeqCst), 2);

    // SSRF enabled: the first (public) hop is allowed, the redirect to a
    // hostname that resolves to loopback is rejected before it is dialed.
    let (proxy, connections) =
        spawn_redirecting_proxy(format!("http://localhost:{}/", target.port())).await;
    let config = Config {
        block_private_networks: true,
        proxy: Some(format!("http://{proxy}")),
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let error = client
        .fetch(Request::get(
            parse_url(&format!("http://{PUBLIC_HOST}/redirect")).unwrap(),
        ))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("private-network"),
        "redirect target was not re-checked: {error}"
    );
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "the blocked redirect hop must not reach the proxy"
    );
}

fn pac_path(tag: &str, script: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "nexium-pac-{tag}-{}-{nanos}.pac",
        std::process::id()
    ));
    std::fs::write(&path, script).unwrap();
    path
}

#[tokio::test]
async fn pac_direct_decision_bypasses_the_static_proxy() {
    let page = spawn_test_server().await;
    let (proxy, connections) = spawn_dead_proxy().await;
    let pac = pac_path(
        "direct",
        &format!(
            "function FindProxyForURL(url, host) {{\n\
             if (host == \"127.0.0.1\") return \"DIRECT\";\n\
             return \"PROXY 127.0.0.1:{}\";\n\
             }}",
            proxy.port()
        ),
    );
    let config = Config {
        proxy: Some(format!("http://{proxy}")),
        pac_file: Some(pac.clone()),
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let request = Request::get(parse_url(&format!("http://{page}/")).unwrap());
    let response = client.fetch(request).await.unwrap();
    assert_eq!(response.status.as_u16(), 200);
    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "PAC DIRECT must not dial the proxy"
    );
    let _ = std::fs::remove_file(pac);
}

#[tokio::test]
async fn pac_proxy_decision_routes_through_the_proxy() {
    let page = spawn_test_server().await;
    let (proxy, connections) = spawn_dead_proxy().await;
    let pac = pac_path(
        "proxy",
        &format!(
            "function FindProxyForURL(url, host) {{\n\
             if (shExpMatch(host, \"*.public\")) return \"DIRECT\";\n\
             return \"PROXY 127.0.0.1:{}\";\n\
             }}",
            proxy.port()
        ),
    );
    let config = Config {
        pac_file: Some(pac.clone()),
        ..Config::default()
    };
    let client = HttpClient::new(&config);
    let request = Request::get(parse_url(&format!("http://{page}/")).unwrap());
    assert!(client.fetch(request).await.is_err());
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "PAC PROXY decision must dial the proxy"
    );
    let _ = std::fs::remove_file(pac);
}
