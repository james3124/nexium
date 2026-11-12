//! REST, WebSocket and MCP control plane.

mod hardening;
pub mod routes;
pub mod service;
mod websocket;

pub use service::{ApiError, ApiService, AuthContext, ServerEvent};

use std::path::Path;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::{self, Next};
use base64::Engine as _;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
};
use tokio_rustls::TlsAcceptor;

use crate::config::Config;
use crate::utils::{NxError, Result};

pub async fn serve(config: Config) -> Result<()> {
    let tls = match (&config.api_tls_cert, &config.api_tls_key) {
        (Some(cert), Some(key)) => Some(tls_acceptor(cert, key)?),
        (None, None) => None,
        _ => {
            return Err(NxError::Config(
                "api_tls_cert and api_tls_key must be configured together".to_owned(),
            ));
        }
    };
    if config.api_tls_bind.is_some() && tls.is_none() {
        return Err(NxError::Config(
            "api_tls_bind requires api_tls_cert and api_tls_key".to_owned(),
        ));
    }
    if config.api_token.is_none() && config.api_tokens.is_empty() {
        for bind in std::iter::once(&config.api_bind).chain(config.api_tls_bind.iter()) {
            if let Ok(address) = bind.parse::<std::net::SocketAddr>()
                && !address.ip().is_loopback()
            {
                return Err(NxError::Config(
                    "refusing to expose an unauthenticated control server beyond localhost; set api_token"
                        .to_owned(),
                ));
            }
        }
    }
    let service = ApiService::new(config.clone());
    match (tls, config.api_tls_bind.as_deref()) {
        (Some(acceptor), Some(tls_bind)) => {
            let http_listener = tokio::net::TcpListener::bind(&config.api_bind)
                .await
                .map_err(NxError::Io)?;
            let tls_listener = tokio::net::TcpListener::bind(tls_bind)
                .await
                .map_err(NxError::Io)?;
            tracing::info!(
                "NEXIUM control server listening on http://{}",
                config.api_bind
            );
            tracing::info!("NEXIUM control server listening on https://{tls_bind}");
            tokio::try_join!(
                serve_dual_on(http_listener, Arc::clone(&service), acceptor.clone()),
                serve_tls_on(tls_listener, service, acceptor)
            )?;
            Ok(())
        }
        (Some(acceptor), None) => {
            let listener = tokio::net::TcpListener::bind(&config.api_bind)
                .await
                .map_err(NxError::Io)?;
            tracing::info!(
                "NEXIUM control server listening on https://{}",
                config.api_bind
            );
            serve_tls_on(listener, service, acceptor).await
        }
        (None, _) => {
            let listener = tokio::net::TcpListener::bind(&config.api_bind)
                .await
                .map_err(NxError::Io)?;
            tracing::info!(
                "NEXIUM control server listening on http://{}",
                config.api_bind
            );
            serve_on(listener, service).await
        }
    }
}

pub async fn serve_on(listener: tokio::net::TcpListener, service: Arc<ApiService>) -> Result<()> {
    let router = routes::router(service);
    axum::serve(listener, router)
        .await
        .map_err(|error| NxError::Browser(format!("control server: {error}")))
}

/// Serve the control plane over TLS. HTTP/1.1 with upgrades covers REST,
/// WebSocket and MCP SSE.
pub async fn serve_tls_on(
    listener: tokio::net::TcpListener,
    service: Arc<ApiService>,
    acceptor: TlsAcceptor,
) -> Result<()> {
    let router = tls_router(service);
    loop {
        let (stream, _peer) = listener.accept().await.map_err(NxError::Io)?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(stream).await else {
                return;
            };
            let io = hyper_util::rt::TokioIo::new(stream);
            let service = hyper_util::service::TowerToHyperService::new(router);
            let connection = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades();
            if let Err(error) = connection.await {
                tracing::debug!("TLS connection error: {error}");
            }
        });
    }
}

/// Serve plain HTTP and TLS on the same port, deciding per connection from the
/// first byte (TLS handshake records start with `0x16`). This keeps browsers
/// that upgrade `http://` to `https://` working on the API port.
pub async fn serve_dual_on(
    listener: tokio::net::TcpListener,
    service: Arc<ApiService>,
    acceptor: TlsAcceptor,
) -> Result<()> {
    let plain_router = routes::router(Arc::clone(&service));
    let tls_router = tls_router(service);
    loop {
        let (stream, _peer) = listener.accept().await.map_err(NxError::Io)?;
        let acceptor = acceptor.clone();
        let plain_router = plain_router.clone();
        let tls_router = tls_router.clone();
        tokio::spawn(async move {
            let mut first = [0u8; 1];
            let is_tls = matches!(stream.peek(&mut first).await, Ok(1) if first[0] == 0x16);
            if is_tls {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let service = hyper_util::service::TowerToHyperService::new(tls_router);
                let io = hyper_util::rt::TokioIo::new(stream);
                if let Err(error) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await
                {
                    tracing::debug!("TLS connection error: {error}");
                }
            } else {
                let service = hyper_util::service::TowerToHyperService::new(plain_router);
                let io = hyper_util::rt::TokioIo::new(stream);
                if let Err(error) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await
                {
                    tracing::debug!("connection error: {error}");
                }
            }
        });
    }
}

pub fn router(service: Arc<ApiService>) -> axum::Router {
    routes::router(service)
}

/// TLS requests get `x-forwarded-proto: https` so OAuth metadata reports the
/// scheme the client actually used.
fn tls_router(service: Arc<ApiService>) -> axum::Router {
    routes::router(service).layer(middleware::from_fn(
        |mut request: Request, next: Next| async move {
            request
                .headers_mut()
                .insert("x-forwarded-proto", HeaderValue::from_static("https"));
            next.run(request).await
        },
    ))
}

fn tls_acceptor(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor> {
    let certs = load_pem_blocks(cert_path, "CERTIFICATE")?
        .into_iter()
        .map(CertificateDer::from)
        .collect::<Vec<_>>();
    if certs.is_empty() {
        return Err(NxError::Config(format!(
            "no CERTIFICATE blocks in {}",
            cert_path.display()
        )));
    }
    let key = load_private_key(key_path)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| NxError::Config(format!("TLS configuration: {error}")))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| NxError::Config(format!("TLS certificate/key: {error}")))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    for (label, kind) in [
        ("PRIVATE KEY", KeyKind::Pkcs8),
        ("EC PRIVATE KEY", KeyKind::Sec1),
        ("RSA PRIVATE KEY", KeyKind::Pkcs1),
    ] {
        if let Some(bytes) = load_pem_blocks(path, label)?.into_iter().next() {
            return Ok(match kind {
                KeyKind::Pkcs8 => PrivatePkcs8KeyDer::from(bytes).into(),
                KeyKind::Sec1 => PrivateSec1KeyDer::from(bytes).into(),
                KeyKind::Pkcs1 => PrivatePkcs1KeyDer::from(bytes).into(),
            });
        }
    }
    Err(NxError::Config(format!(
        "no private key PEM blocks in {}",
        path.display()
    )))
}

enum KeyKind {
    Pkcs8,
    Sec1,
    Pkcs1,
}

fn load_pem_blocks(path: &Path, label: &str) -> Result<Vec<Vec<u8>>> {
    let text = std::fs::read_to_string(path).map_err(NxError::Io)?;
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut blocks = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find(&begin) {
        let after_begin = &rest[start + begin.len()..];
        let Some(finish) = after_begin.find(&end) else {
            break;
        };
        let body: String = after_begin[..finish]
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body.as_bytes())
            .map_err(|error| {
                NxError::Config(format!(
                    "invalid {label} PEM in {}: {error}",
                    path.display()
                ))
            })?;
        blocks.push(bytes);
        rest = &after_begin[finish + end.len()..];
    }
    Ok(blocks)
}
