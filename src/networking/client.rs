use std::collections::{HashMap, VecDeque};
use std::error::Error as StdError;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{
    ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE,
    HOST, IF_MODIFIED_SINCE, IF_NONE_MATCH, LOCATION, PROXY_AUTHORIZATION, USER_AGENT,
};
use hyper::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower_service::Service;
use tracing::warn;
use url::Url;

use super::bypass::ProxyBypass;
use super::cache::{CacheLookup, HttpCache};
use super::compression::decode_body;
use super::cookies::CookieJar;
use super::ssrf;
use crate::config::Config;
use crate::utils::{NxError, Result};

type HyperClient = Client<hyper_rustls::HttpsConnector<Connector>, Full<Bytes>>;
type BoxError = Box<dyn StdError + Send + Sync>;
type ConnectFuture =
    Pin<Box<dyn Future<Output = std::result::Result<TokioIo<TcpStream>, BoxError>> + Send>>;
type PinnedAddresses = Arc<Mutex<HashMap<String, Vec<SocketAddr>>>>;

/// An outbound HTTP request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: Url,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub timeout: Option<Duration>,
    pub max_redirects: Option<usize>,
}

impl Request {
    pub fn new(method: Method, url: Url) -> Self {
        Self {
            method,
            url,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            timeout: None,
            max_redirects: None,
        }
    }

    pub fn get(url: Url) -> Self {
        Self::new(Method::GET, url)
    }

    pub fn header(mut self, name: &str, value: &str) -> Result<Self> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| NxError::InvalidHeader(name.to_owned()))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| NxError::InvalidHeader(format!("{name}: {value}")))?;
        self.headers.insert(name, value);
        Ok(self)
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn max_redirects(mut self, limit: usize) -> Self {
        self.max_redirects = Some(limit);
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct NetworkEvent {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub resource_type: String,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// A fully buffered HTTP response.
#[derive(Debug)]
pub struct Response {
    pub url: Url,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub redirects: Vec<Url>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    pub fn mime_type(&self) -> Option<String> {
        let value = self.content_type()?;
        let mime = value.split(';').next()?.trim();
        if mime.is_empty() {
            None
        } else {
            Some(mime.to_ascii_lowercase())
        }
    }

    pub fn charset(&self) -> Option<String> {
        let value = self.content_type()?;
        for param in value.split(';').skip(1) {
            let param = param.trim();
            let Some((key, val)) = param.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("charset") {
                let val = val.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return Some(val.to_owned());
                }
            }
        }
        None
    }

    pub fn is_html(&self) -> bool {
        matches!(
            self.mime_type().as_deref(),
            Some("text/html" | "application/xhtml+xml")
        )
    }
}

/// Decision returned by a [`RequestInterceptor`].
pub enum InterceptDecision {
    Continue,
    Respond(Box<Response>),
}

/// Hook for request interception, blocking and response inspection.
pub trait RequestInterceptor: Send + Sync {
    fn intercept_request(&self, _request: &mut Request) -> InterceptDecision {
        InterceptDecision::Continue
    }

    fn intercept_response(&self, _request: &Request, _response: &mut Response) {}
}

/// Asynchronous HTTP/1.1 + HTTP/2 client with TLS and connection pooling.
pub struct HttpClient {
    inner: HyperClient,
    default_timeout: Duration,
    max_redirects: usize,
    user_agent: String,
    max_response_bytes: usize,
    cookies: Mutex<CookieJar>,
    cache: HttpCache,
    block_private_networks: bool,
    /// Addresses validated by the SSRF policy, keyed by `host:port`. The
    /// direct connector dials these instead of resolving again (rebinding
    /// mitigation on the non-proxy path).
    pinned: PinnedAddresses,
    interceptor: Mutex<Option<Arc<dyn RequestInterceptor>>>,
    network: Mutex<VecDeque<NetworkEvent>>,
    network_sequence: AtomicU64,
}

impl HttpClient {
    pub fn new(config: &Config) -> Self {
        let pinned: PinnedAddresses = Arc::new(Mutex::new(HashMap::new()));
        let connector = build_connector(config, Arc::clone(&pinned));

        let inner = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build(connector);

        Self {
            inner,
            default_timeout: config.timeout(),
            max_redirects: config.max_redirects,
            user_agent: config.user_agent.clone(),
            max_response_bytes: config.max_response_bytes,
            cookies: Mutex::new(CookieJar::new(config.cookies_enabled)),
            cache: HttpCache::new(config.cache_enabled, config.cache_max_entries),
            block_private_networks: config.block_private_networks,
            pinned,
            interceptor: Mutex::new(None),
            network: Mutex::new(VecDeque::new()),
            network_sequence: AtomicU64::new(1),
        }
    }

    pub fn network_events(&self) -> Vec<NetworkEvent> {
        self.network
            .lock()
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear_network_events(&self) {
        if let Ok(mut events) = self.network.lock() {
            events.clear();
        }
    }

    pub fn set_interceptor(&self, interceptor: Option<Arc<dyn RequestInterceptor>>) {
        if let Ok(mut slot) = self.interceptor.lock() {
            *slot = interceptor;
        }
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    pub fn set_cache_dir(&self, dir: &Path) -> Result<()> {
        self.cache.set_disk_dir(dir)
    }

    /// `Cookie` header value for a URL, for `document.cookie` reads.
    pub fn cookie_header_for_url(&self, url: &Url) -> Option<String> {
        self.cookies
            .lock()
            .ok()
            .and_then(|jar| jar.cookie_header(url))
    }

    /// Apply a script-visible `document.cookie` assignment.
    pub fn set_document_cookie(&self, url: &Url, value: &str) {
        if let Ok(mut jar) = self.cookies.lock() {
            jar.set_document_cookie(url, value);
        }
    }

    pub fn cookie_count(&self) -> usize {
        self.cookies.lock().map(|jar| jar.len()).unwrap_or(0)
    }

    pub fn clear_cookies(&self) {
        if let Ok(mut jar) = self.cookies.lock() {
            jar.clear();
        }
    }

    pub fn cookie_records(&self) -> Vec<super::cookies::CookieRecord> {
        self.cookies
            .lock()
            .map(|jar| jar.records())
            .unwrap_or_default()
    }

    pub fn restore_cookies(&self, records: &[super::cookies::CookieRecord]) {
        if let Ok(mut jar) = self.cookies.lock() {
            jar.restore(records);
        }
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Perform a request, following redirects and buffering the body.
    pub async fn fetch(&self, request: Request) -> Result<Response> {
        let method = request.method.to_string();
        let url = request.url.to_string();
        let started = Instant::now();
        let result = self.fetch_inner(request).await;
        let (status, resource_type, error) = match &result {
            Ok(response) => (
                Some(response.status.as_u16()),
                resource_type(response.mime_type().as_deref(), &url),
                None,
            ),
            Err(error) => (None, resource_type(None, &url), Some(error.to_string())),
        };
        self.record_network(NetworkEvent {
            sequence: 0,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            method,
            url,
            status,
            resource_type,
            duration_ms: started.elapsed().as_millis() as u64,
            error,
        });
        result
    }

    fn record_network(&self, mut event: NetworkEvent) {
        event.sequence = self.network_sequence.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut events) = self.network.lock() {
            if events.len() >= 500 {
                events.pop_front();
            }
            events.push_back(event);
        }
    }

    async fn fetch_inner(&self, mut request: Request) -> Result<Response> {
        let original_request = request.clone();
        let interceptor = self.interceptor.lock().ok().and_then(|slot| slot.clone());
        if let Some(interceptor) = &interceptor
            && let InterceptDecision::Respond(response) =
                interceptor.intercept_request(&mut request)
        {
            return Ok(*response);
        }
        if !matches!(request.url.scheme(), "http" | "https") {
            return Err(NxError::UnsupportedScheme(request.url.scheme().to_owned()));
        }
        // SSRF policy is enforced below for every redirect hop, and again
        // before a cached body is returned.

        let timeout = request.timeout.unwrap_or(self.default_timeout);
        let max_redirects = request.max_redirects.unwrap_or(self.max_redirects);

        // The network layer has no explicit "top-level navigation" flag yet,
        // so a navigation is approximated as a GET asking for an HTML
        // document (what `Page::load` sends). Sub-resources, fetch() and XHR
        // use `*/*` or a resource-specific Accept and stay non-navigations.
        let top_level_navigation = is_navigation_request(&request);

        let has_cookie = request.headers.contains_key(COOKIE);
        let request_no_cache = request
            .headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("no-cache") || value.contains("no-store")
            });
        let cache_allowed = request.method == Method::GET
            && !request.headers.contains_key(AUTHORIZATION)
            && !has_cookie
            && !request_no_cache
            && self
                .cookies
                .lock()
                .ok()
                .and_then(|jar| jar.cookie_header_for(&request.url, top_level_navigation))
                .is_none();

        // A stale cache entry with an ETag/Last-Modified is revalidated with a
        // conditional request instead of being discarded.
        let mut revalidation: Option<Response> = None;
        if cache_allowed {
            match self.cache.lookup(&request.url) {
                CacheLookup::Fresh(cached) => {
                    self.enforce_ssrf(&cached.url).await?;
                    if top_level_navigation {
                        self.update_top_level_site(&cached.url);
                    }
                    return Ok(cached);
                }
                CacheLookup::Stale(stale) => revalidation = Some(stale),
                CacheLookup::Miss => {}
            }
        }

        let mut url = request.url;
        let mut method = request.method;
        let mut headers = request.headers;
        let mut body = request.body;
        let mut redirects: Vec<Url> = Vec::new();

        for _ in 0..=max_redirects {
            // Re-checked on every hop so a redirect cannot smuggle a request
            // to a private address past the original validation.
            self.enforce_ssrf(&url).await?;

            let mut builder = hyper::Request::builder()
                .method(method.clone())
                .uri(url.as_str());

            let cookie_header = self
                .cookies
                .lock()
                .ok()
                .and_then(|jar| jar.cookie_header_for(&url, top_level_navigation));

            let header_map = builder
                .headers_mut()
                .expect("hyper request builder is valid");
            *header_map = headers.clone();
            header_map
                .entry(USER_AGENT)
                .or_insert_with(|| HeaderValue::from_str(&self.user_agent).unwrap());
            header_map
                .entry(ACCEPT)
                .or_insert_with(|| HeaderValue::from_static("*/*"));
            header_map
                .entry(ACCEPT_ENCODING)
                .or_insert_with(|| HeaderValue::from_static("gzip, deflate, br"));
            // Let hyper derive Host from the URL instead of sending a stale one.
            header_map.remove(HOST);
            if !header_map.contains_key(COOKIE)
                && let Some(cookie_header) = cookie_header
                && let Ok(value) = HeaderValue::from_str(&cookie_header)
            {
                header_map.insert(COOKIE, value);
            }
            if let Some(stale) = revalidation.as_ref() {
                if let Some(etag) = stale.header("etag")
                    && let Ok(value) = HeaderValue::from_str(etag)
                {
                    header_map.entry(IF_NONE_MATCH).or_insert(value);
                }
                if let Some(modified) = stale.header("last-modified")
                    && let Ok(value) = HeaderValue::from_str(modified)
                {
                    header_map.entry(IF_MODIFIED_SINCE).or_insert(value);
                }
            }

            let request = builder
                .body(Full::new(body.clone()))
                .map_err(|e| NxError::Network(e.to_string()))?;

            let response = tokio::time::timeout(timeout, self.inner.request(request))
                .await
                .map_err(|_| NxError::Timeout(timeout))?
                .map_err(format_client_error)?;

            let status = response.status();
            let response_headers = response.headers().clone();
            if let Ok(mut jar) = self.cookies.lock() {
                jar.set_from_headers(&url, &response_headers);
            }

            // 304: the cached representation is still valid. Merge the new
            // headers, refresh freshness and reuse the stored body.
            if status == StatusCode::NOT_MODIFIED
                && let Some(stale) = revalidation.take()
            {
                let merged = merge_revalidated_headers(&stale.headers, &response_headers);
                self.cache
                    .refresh(&stale.url, stale.status, &merged, &stale.body);
                let mut response = Response {
                    url: stale.url,
                    status: stale.status,
                    headers: merged,
                    body: stale.body,
                    redirects,
                };
                if let Some(interceptor) = &interceptor {
                    interceptor.intercept_response(&original_request, &mut response);
                }
                return Ok(response);
            }

            if is_followable_redirect(status)
                && let Some(location) = response_headers.get(LOCATION).and_then(|v| v.to_str().ok())
            {
                let next = url
                    .join(location)
                    .map_err(|e| NxError::InvalidUrl(format!("{location}: {e}")))?;
                if !matches!(next.scheme(), "http" | "https") {
                    return Err(NxError::UnsupportedScheme(next.scheme().to_owned()));
                }

                redirects.push(url.clone());

                let same_origin = same_origin(&url, &next);
                let switch_to_get = match status.as_u16() {
                    303 => method != Method::HEAD,
                    301 | 302 => method == Method::POST,
                    _ => false,
                };
                if switch_to_get {
                    method = Method::GET;
                    body = Bytes::new();
                    headers.remove(CONTENT_TYPE);
                    headers.remove(CONTENT_LENGTH);
                    headers.remove(CONTENT_ENCODING);
                }
                if !same_origin {
                    headers.remove(COOKIE);
                    headers.remove(AUTHORIZATION);
                    headers.remove(PROXY_AUTHORIZATION);
                }
                headers.remove(CONTENT_LENGTH);
                headers.remove(HOST);

                // Conditional headers only ever apply to the original URL.
                revalidation = None;
                url = next;
                continue;
            }

            let (mut parts, incoming) = response.into_parts();
            let content_encoding = parts
                .headers
                .get(CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = tokio::time::timeout(timeout, self.read_body(incoming))
                .await
                .map_err(|_| NxError::Timeout(timeout))??;
            let body = if content_encoding.is_some() {
                let decoded =
                    decode_body(body, content_encoding.as_deref(), self.max_response_bytes)?;
                parts.headers.remove(CONTENT_ENCODING);
                parts.headers.remove(CONTENT_LENGTH);
                decoded
            } else {
                body
            };

            let mut response = Response {
                url,
                status,
                headers: parts.headers,
                body,
                redirects,
            };
            if let Some(interceptor) = &interceptor {
                interceptor.intercept_response(&original_request, &mut response);
            }
            if top_level_navigation {
                self.update_top_level_site(&response.url);
            }
            if cache_allowed && method == Method::GET {
                self.cache.put(
                    &response.url,
                    response.status,
                    &response.headers,
                    &response.body,
                );
            }
            return Ok(response);
        }

        Err(NxError::TooManyRedirects(max_redirects))
    }

    async fn read_body(&self, incoming: hyper::body::Incoming) -> Result<Bytes> {
        let mut incoming = incoming;
        let mut buf: Vec<u8> = Vec::new();
        while let Some(frame) = incoming.frame().await {
            let frame = frame.map_err(|e| NxError::Network(e.to_string()))?;
            if let Some(data) = frame.data_ref() {
                if buf.len() + data.len() > self.max_response_bytes {
                    return Err(NxError::ResponseTooLarge(self.max_response_bytes));
                }
                buf.extend_from_slice(data);
            }
        }
        Ok(Bytes::from(buf))
    }

    /// Remember the site of the document that is now on top, so later
    /// sub-resource requests can apply SameSite rules.
    fn update_top_level_site(&self, url: &Url) {
        if let Ok(mut jar) = self.cookies.lock() {
            jar.set_top_level_site(url);
        }
    }

    /// Resolve the destination and reject it when any address is private,
    /// loopback, link-local, unspecified or multicast. Resolved addresses are
    /// pinned for the direct connector. See [`super::ssrf`] for the DNS
    /// rebinding caveat.
    async fn enforce_ssrf(&self, url: &Url) -> Result<()> {
        if !self.block_private_networks {
            return Ok(());
        }
        let Some(host) = url.host_str() else {
            return Err(NxError::InvalidUrl("request URL has no host".to_owned()));
        };
        let port = url.port_or_known_default().unwrap_or(443);
        match ssrf::resolve_and_validate(host, port).await {
            Ok(addresses) => {
                if let Ok(mut pinned) = self.pinned.lock() {
                    pinned.insert(pin_key(host, port), addresses);
                }
                Ok(())
            }
            Err(error) => Err(NxError::Network(error.to_string())),
        }
    }
}

/// Approximation used for `SameSite=Lax`: a request counts as a top-level
/// navigation when it is a safe `GET` asking for an HTML document.
fn is_navigation_request(request: &Request) -> bool {
    request.method == Method::GET
        && request
            .headers
            .get(ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
}

/// Merge `304 Not Modified` headers into the stored representation's headers.
fn merge_revalidated_headers(cached: &HeaderMap, fresh: &HeaderMap) -> HeaderMap {
    let mut merged = cached.clone();
    for (name, value) in fresh.iter() {
        // The cached body is authoritative; keep its framing headers.
        if name == CONTENT_LENGTH
            || name == CONTENT_ENCODING
            || name.as_str().eq_ignore_ascii_case("transfer-encoding")
        {
            continue;
        }
        merged.insert(name.clone(), value.clone());
    }
    merged
}

fn is_followable_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Flatten the hyper client error chain into a readable message.
fn format_client_error(error: hyper_util::client::legacy::Error) -> NxError {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    NxError::Network(message)
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Connector that either dials the destination directly (preferring the
/// addresses pinned by the SSRF policy) or tunnels through the configured
/// proxy. The `no_proxy` bypass list is evaluated per destination, i.e. once
/// per redirect hop.
#[derive(Clone)]
struct Connector {
    direct: HttpConnector,
    proxy: Option<ProxyConfig>,
    bypass: Arc<ProxyBypass>,
    pinned: PinnedAddresses,
}

impl Connector {
    fn new(config: &Config, pinned: PinnedAddresses) -> Self {
        let mut direct = HttpConnector::new();
        // The HTTPS layer wraps this connector, so https URIs must be accepted.
        direct.enforce_http(false);
        Self {
            direct,
            proxy: config.proxy.as_deref().and_then(parse_proxy),
            bypass: Arc::new(ProxyBypass::from_config(config)),
            pinned,
        }
    }
}

impl Service<hyper::Uri> for Connector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = ConnectFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, destination: hyper::Uri) -> Self::Future {
        let proxy = self.proxy.clone();
        let bypass = Arc::clone(&self.bypass);
        let pinned = Arc::clone(&self.pinned);
        let mut direct = self.direct.clone();
        Box::pin(async move {
            let (host, port) = destination_host_port(&destination)
                .ok_or_else(|| connector_error("missing destination host"))?;

            if let Some(proxy) = proxy
                && !bypass.matches(&host, Some(port))
            {
                return proxy_tunnel(&mut direct, &proxy, &host, port).await;
            }

            // Direct connection: use the addresses validated by the SSRF
            // policy so DNS cannot change between check and connect.
            let addresses = pinned
                .lock()
                .ok()
                .and_then(|map| map.get(&pin_key(&host, port)).cloned());
            if let Some(addresses) = addresses {
                let mut last_error = None;
                for address in addresses {
                    match TcpStream::connect(address).await {
                        Ok(stream) => return Ok(TokioIo::new(stream)),
                        Err(error) => last_error = Some(error),
                    }
                }
                if let Some(error) = last_error {
                    return Err(Box::new(error));
                }
            }

            direct
                .call(destination)
                .await
                .map_err(|error| Box::new(error) as BoxError)
        })
    }
}

#[derive(Clone, Debug)]
enum ProxyKind {
    /// HTTP CONNECT tunnel.
    HttpConnect,
    /// SOCKS5 tunnel; `remote_dns` selects `socks5h://` semantics.
    Socks5 { remote_dns: bool },
}

#[derive(Clone)]
struct ProxyConfig {
    host: String,
    port: u16,
    kind: ProxyKind,
    authorization: Option<HeaderValue>,
    username: Option<String>,
    password: Option<String>,
}

/// Lower-cased destination host and effective port for a request URI.
fn destination_host_port(uri: &hyper::Uri) -> Option<(String, u16)> {
    let host = uri
        .host()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let port = uri.port_u16().unwrap_or_else(|| {
        if uri.scheme_str() == Some("https") {
            443
        } else {
            80
        }
    });
    Some((host, port))
}

/// Canonical `host:port` key used for pinned SSRF addresses.
fn pin_key(host: &str, port: u16) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let canonical = host
        .parse::<IpAddr>()
        .map_or_else(|_| host.to_ascii_lowercase(), |address| address.to_string());
    format!("{canonical}:{port}")
}

/// `host:port`, bracketing IPv6 literals for `CONNECT` authorities.
fn authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Establish a tunnel to `host:port` through the configured proxy.
async fn proxy_tunnel(
    connector: &mut HttpConnector,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> std::result::Result<TokioIo<TcpStream>, BoxError> {
    let mut tcp = dial_proxy(connector, proxy).await?;

    match proxy.kind {
        ProxyKind::HttpConnect => {
            let authority = authority(host, port);
            let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
            if let Some(authorization) = &proxy.authorization
                && let Ok(value) = authorization.to_str()
            {
                request.push_str(&format!("Proxy-Authorization: {value}\r\n"));
            }
            request.push_str("\r\n");
            tcp.write_all(request.as_bytes())
                .await
                .map_err(|error| Box::new(error) as BoxError)?;

            // Read the response head byte-by-byte so we never consume
            // TLS bytes that follow the CONNECT response.
            let mut response = Vec::new();
            let mut byte = [0u8; 1];
            while !response.ends_with(b"\r\n\r\n") {
                if response.len() > 16 * 1024 {
                    return Err(connector_error("proxy response too large"));
                }
                let read = tcp
                    .read(&mut byte)
                    .await
                    .map_err(|error| Box::new(error) as BoxError)?;
                if read == 0 {
                    return Err(connector_error("proxy closed the connection"));
                }
                response.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&response);
            let status_line = head.lines().next().unwrap_or("");
            if !status_line.contains(" 200") {
                return Err(connector_error(format!(
                    "proxy CONNECT failed: {status_line}"
                )));
            }
        }
        ProxyKind::Socks5 { remote_dns } => {
            socks5_handshake(&mut tcp, proxy, host, port, remote_dns).await?;
        }
    }
    Ok(TokioIo::new(tcp))
}

/// Open a TCP connection to the configured proxy server itself.
async fn dial_proxy(
    inner: &mut HttpConnector,
    proxy: &ProxyConfig,
) -> std::result::Result<TcpStream, BoxError> {
    let proxy_uri: hyper::Uri = format!("http://{}:{}", proxy.host, proxy.port)
        .parse()
        .map_err(|_| connector_error("invalid proxy address"))?;
    let stream = inner
        .call(proxy_uri)
        .await
        .map_err(|error| Box::new(error) as BoxError)?;
    Ok(stream.into_inner())
}

/// Complete a SOCKS5 handshake (RFC 1928 + RFC 1929 username/password auth).
///
/// `remote_dns` is true for `socks5h://`: the destination host name is sent to
/// the proxy (`ATYP=0x03`) instead of being resolved locally.
async fn socks5_handshake(
    tcp: &mut TcpStream,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
    remote_dns: bool,
) -> std::result::Result<(), BoxError> {
    // Greeting: offer no-auth and, when credentials exist, username/password.
    let mut methods = vec![0x00u8];
    if proxy.username.is_some() {
        methods.push(0x02);
    }
    let mut greeting = vec![0x05u8, methods.len() as u8];
    greeting.extend_from_slice(&methods);
    tcp.write_all(&greeting)
        .await
        .map_err(|error| Box::new(error) as BoxError)?;

    let mut selection = [0u8; 2];
    tcp.read_exact(&mut selection)
        .await
        .map_err(|error| Box::new(error) as BoxError)?;
    if selection[0] != 0x05 {
        return Err(connector_error("unexpected SOCKS5 version in method reply"));
    }
    match selection[1] {
        0x00 => {}
        0x02 => {
            let username = proxy.username.as_deref().unwrap_or("");
            let password = proxy.password.as_deref().unwrap_or("");
            if username.len() > 255 || password.len() > 255 {
                return Err(connector_error("SOCKS5 credentials too long"));
            }
            let mut auth = vec![0x01u8, username.len() as u8];
            auth.extend_from_slice(username.as_bytes());
            auth.push(password.len() as u8);
            auth.extend_from_slice(password.as_bytes());
            tcp.write_all(&auth)
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            let mut reply = [0u8; 2];
            tcp.read_exact(&mut reply)
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            if reply[1] != 0x00 {
                return Err(connector_error("SOCKS5 authentication failed"));
            }
        }
        0xff => {
            return Err(connector_error(
                "SOCKS5 proxy rejected all offered authentication methods",
            ));
        }
        method => {
            return Err(connector_error(format!(
                "unsupported SOCKS5 authentication method {method:#04x}"
            )));
        }
    }

    // CONNECT request.
    let mut request = vec![0x05u8, 0x01, 0x00];
    if remote_dns && host.parse::<std::net::IpAddr>().is_err() {
        let host_bytes = host.as_bytes();
        if host_bytes.len() > 255 {
            return Err(connector_error("SOCKS5 destination host name too long"));
        }
        request.push(0x03);
        request.push(host_bytes.len() as u8);
        request.extend_from_slice(host_bytes);
    } else {
        let address = resolve_locally(host, port).await?;
        match address {
            std::net::SocketAddr::V4(address) => {
                request.push(0x01);
                request.extend_from_slice(&address.ip().octets());
            }
            std::net::SocketAddr::V6(address) => {
                request.push(0x04);
                request.extend_from_slice(&address.ip().octets());
            }
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    tcp.write_all(&request)
        .await
        .map_err(|error| Box::new(error) as BoxError)?;

    let mut reply = [0u8; 4];
    tcp.read_exact(&mut reply)
        .await
        .map_err(|error| Box::new(error) as BoxError)?;
    if reply[0] != 0x05 {
        return Err(connector_error(
            "unexpected SOCKS5 version in CONNECT reply",
        ));
    }
    if reply[1] != 0x00 {
        let reason = match reply[1] {
            0x01 => "general SOCKS server failure",
            0x02 => "connection not allowed by ruleset",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "TTL expired",
            0x07 => "command not supported",
            0x08 => "address type not supported",
            _ => "unknown error",
        };
        return Err(connector_error(format!("SOCKS5 CONNECT failed: {reason}")));
    }
    // Drain BND.ADDR/BND.PORT so no handshake bytes leak into the tunnel.
    let bound_len = match reply[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut length = [0u8; 1];
            tcp.read_exact(&mut length)
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            length[0] as usize
        }
        other => {
            return Err(connector_error(format!(
                "SOCKS5 reply used unsupported address type {other:#04x}"
            )));
        }
    };
    let mut bound = vec![0u8; bound_len + 2];
    tcp.read_exact(&mut bound)
        .await
        .map_err(|error| Box::new(error) as BoxError)?;
    Ok(())
}

/// Resolve a destination for `socks5://` (local DNS) proxies.
async fn resolve_locally(
    host: &str,
    port: u16,
) -> std::result::Result<std::net::SocketAddr, BoxError> {
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return Ok(std::net::SocketAddr::new(address, port));
    }
    tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| Box::new(error) as BoxError)?
        .next()
        .ok_or_else(|| connector_error(format!("cannot resolve SOCKS5 destination {host}")))
}

fn resource_type(mime: Option<&str>, url: &str) -> String {
    let mime = mime.unwrap_or("").to_ascii_lowercase();
    if mime.contains("text/html") || mime.contains("application/xhtml") {
        return "document".to_owned();
    }
    if mime.contains("text/css") {
        return "stylesheet".to_owned();
    }
    if mime.contains("javascript") || mime.contains("ecmascript") {
        return "script".to_owned();
    }
    if mime.starts_with("image/") {
        return "image".to_owned();
    }
    if mime.starts_with("font/") || mime.contains("woff") {
        return "font".to_owned();
    }
    if mime.contains("json") || mime.contains("xml") {
        return "fetch".to_owned();
    }
    let extension = url
        .split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('.').next())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "css" => "stylesheet",
        "js" | "mjs" => "script",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
        "woff" | "woff2" | "ttf" | "otf" => "font",
        "json" => "fetch",
        _ => "other",
    }
    .to_owned()
}

fn connector_error(message: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(message.into()))
}

fn build_connector(
    config: &Config,
    pinned: PinnedAddresses,
) -> hyper_rustls::HttpsConnector<Connector> {
    HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(Connector::new(config, pinned))
}

fn parse_proxy(raw: &str) -> Option<ProxyConfig> {
    let url = match Url::parse(raw) {
        Ok(url) => url,
        Err(error) => {
            warn!("invalid proxy URL {raw}: {error}");
            return None;
        }
    };
    let kind = match url.scheme() {
        "http" => ProxyKind::HttpConnect,
        "socks5" => ProxyKind::Socks5 { remote_dns: false },
        "socks5h" => ProxyKind::Socks5 { remote_dns: true },
        other => {
            warn!(
                "unsupported proxy scheme {other}: supported schemes are http (CONNECT), socks5 and socks5h"
            );
            return None;
        }
    };
    let host = url.host_str()?.to_owned();
    let default_port = match kind {
        ProxyKind::HttpConnect => 8080,
        ProxyKind::Socks5 { .. } => 1080,
    };
    let port = url.port_or_known_default().unwrap_or(default_port);
    let username = if url.username().is_empty() {
        None
    } else {
        Some(percent_decode(url.username()))
    };
    let password = url.password().map(percent_decode);
    let authorization = if url.username().is_empty() {
        None
    } else {
        let credentials = format!("{}:{}", url.username(), url.password().unwrap_or(""));
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        HeaderValue::from_str(&format!("Basic {encoded}")).ok()
    };
    Some(ProxyConfig {
        host,
        port,
        kind,
        authorization,
        username,
        password,
    })
}

/// Decode `%XX` escapes in proxy credentials.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
