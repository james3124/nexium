#![allow(dead_code)]

use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Spawn a small HTTP/1.1 server for tests and return its address.
pub async fn spawn_test_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(handle(stream));
        }
    });
    addr
}

async fn handle(mut stream: TcpStream) {
    let mut buf = vec![0u8; 16 * 1024];
    let Ok(n) = stream.read(&mut buf).await else {
        return;
    };
    if n == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");
    let header_value = |name: &str| {
        lines.clone().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim().eq_ignore_ascii_case(name) {
                Some(value.trim().to_owned())
            } else {
                None
            }
        })
    };
    let accept_encoding = header_value("accept-encoding").unwrap_or_default();
    let cookie = header_value("cookie").unwrap_or_default();

    let response = match path {
        "/" => response(
            200,
            "OK",
            "Content-Type: text/html; charset=utf-8\r\n",
            b"<html><head><title>Test Page</title></head><body><h1>Hello</h1><p class=\"intro\">World</p></body></html>".to_vec(),
        ),
        "/plain" => response(
            200,
            "OK",
            "Content-Type: text/plain\r\n",
            b"just text".to_vec(),
        ),
        "/json" => response(
            200,
            "OK",
            "Content-Type: application/json\r\n",
            br#"{"ok":true,"items":[1,2,3]}"#.to_vec(),
        ),
        "/binary" => response(
            200,
            "OK",
            "Content-Type: application/octet-stream\r\n",
            b"\x89PNG\r\n\x1a\nnot-really".to_vec(),
        ),
        "/import.css" => response(
            200,
            "OK",
            "Content-Type: text/css\r\n",
            b"p { color: #ff0000; font-size: 40px; margin: 0; }".to_vec(),
        ),
        "/import-page" => response(
            200,
            "OK",
            "Content-Type: text/html\r\n",
            b"<html><head><link rel=stylesheet href=/import.css></head><body><p id=x>x</p></body></html>".to_vec(),
        ),
        "/import-style" => response(
            200,
            "OK",
            "Content-Type: text/html\r\n",
            b"<html><head><style>@import \"/import.css\";</style></head><body><p id=x>x</p></body></html>".to_vec(),
        ),
        "/import-media" => response(
            200,
            "OK",
            "Content-Type: text/html\r\n",
            b"<html><head><style>@import \"/import.css\" (min-width: 100px);</style></head><body><p id=x>x</p></body></html>".to_vec(),
        ),
        "/cycle-page" => response(
            200,
            "OK",
            "Content-Type: text/html\r\n",
            b"<html><head><link rel=stylesheet href=/cycle.css></head><body><p id=x>x</p></body></html>".to_vec(),
        ),
        "/cycle.css" => response(
            200,
            "OK",
            "Content-Type: text/css\r\n",
            b"@import \"/cycle.css\"; p { color: #00ff00; }".to_vec(),
        ),
        "/latin1" => response(
            200,
            "OK",
            "Content-Type: text/html; charset=iso-8859-1\r\n",
            b"<html><body><p>caf\xe9</p></body></html>".to_vec(),
        ),
        "/redirect" => response(302, "Found", "Location: /final\r\n", Vec::new()),
        "/redirect-absolute" => response(
            302,
            "Found",
            "Location: http://127.0.0.1:1/elsewhere\r\n",
            Vec::new(),
        ),
        "/final" => response(
            200,
            "OK",
            "Content-Type: text/html\r\n",
            b"<html><body><p>final</p></body></html>".to_vec(),
        ),
        "/loop" => response(302, "Found", "Location: /loop\r\n", Vec::new()),
        "/big" => response(
            200,
            "OK",
            "Content-Type: text/html\r\n",
            vec![b'x'; 1024 * 1024],
        ),
        "/gzip" => {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(b"<html><body><p>gzip body</p></body></html>")
                .unwrap();
            response(
                200,
                "OK",
                "Content-Type: text/html\r\nContent-Encoding: gzip\r\n",
                encoder.finish().unwrap(),
            )
        }
        "/deflate" => {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(b"<html><body><p>deflate body</p></body></html>")
                .unwrap();
            response(
                200,
                "OK",
                "Content-Type: text/html\r\nContent-Encoding: deflate\r\n",
                encoder.finish().unwrap(),
            )
        }
        "/brotli" => {
            let mut encoder = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
            encoder
                .write_all(b"<html><body><p>brotli body</p></body></html>")
                .unwrap();
            encoder.flush().unwrap();
            response(
                200,
                "OK",
                "Content-Type: text/html\r\nContent-Encoding: br\r\n",
                encoder.into_inner(),
            )
        }
        "/encoding" => response(
            200,
            "OK",
            "Content-Type: text/plain\r\n",
            accept_encoding.into_bytes(),
        ),
        "/set-cookie" => response(
            200,
            "OK",
            "Content-Type: text/html\r\nSet-Cookie: session=abc123; Path=/; HttpOnly\r\n",
            b"<html><body>set</body></html>".to_vec(),
        ),
        "/set-cookie-scoped" => response(
            200,
            "OK",
            "Content-Type: text/html\r\nSet-Cookie: scoped=1; Path=/private\r\n",
            b"<html><body>set</body></html>".to_vec(),
        ),
        "/set-cookie-secure" => response(
            200,
            "OK",
            "Content-Type: text/html\r\nSet-Cookie: secure=1; Path=/; Secure\r\n",
            b"<html><body>set</body></html>".to_vec(),
        ),
        "/delete-cookie" => response(
            200,
            "OK",
            "Content-Type: text/html\r\nSet-Cookie: session=; Path=/; Max-Age=0\r\n",
            b"<html><body>deleted</body></html>".to_vec(),
        ),
        "/echo-cookie" => response(
            200,
            "OK",
            "Content-Type: text/plain\r\n",
            cookie.into_bytes(),
        ),
        "/private/echo" => response(
            200,
            "OK",
            "Content-Type: text/plain\r\n",
            cookie.into_bytes(),
        ),
        "/bad-gzip" => response(
            200,
            "OK",
            "Content-Type: text/html\r\nContent-Encoding: gzip\r\n",
            b"not gzip at all".to_vec(),
        ),
        "/slow" => {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            response(200, "OK", "Content-Type: text/html\r\n", b"slow".to_vec())
        }
        _ => response(404, "Not Found", "Content-Type: text/plain\r\n", b"nope".to_vec()),
    };

    let _ = stream.write_all(&response).await;
    let _ = stream.shutdown().await;
}

fn response(status: u16, reason: &str, headers: &str, body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "HTTP/1.1 {status} {reason}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&body);
    out
}

/// HTTP server that counts requests and answers with a configurable
/// Cache-Control header.
pub async fn spawn_counting_server(cache_control: &'static str) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&count);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = stream.read(&mut buf).await;
                counter.fetch_add(1, Ordering::SeqCst);
                let body = b"cached response";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: {cache_control}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (addr, count)
}

/// Minimal HTTP CONNECT proxy that tunnels to the requested authority.
pub async fn spawn_connect_proxy() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&count);
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                break;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut client = client;
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
                let text = String::from_utf8_lossy(&head);
                let Some(target) = text
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                else {
                    return;
                };
                let Ok(mut upstream) = TcpStream::connect(target).await else {
                    let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                    return;
                };
                let _ = client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    (addr, count)
}
