use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{
    ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE,
    HOST, LOCATION, PROXY_AUTHORIZATION, USER_AGENT,
};
use hyper::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower_service::Service;
use tracing::warn;
use url::Url;

use super::cache::HttpCache;
use super::compression::decode_body;
use super::cookies::CookieJar;
use crate::config::Config;
use crate::utils::{NxError, Result};

type HyperClient = Client<hyper_rustls::HttpsConnector<Connector>, Full<Bytes>>;
type BoxError = Box<dyn StdError + Send + Sync>;
type ConnectFuture =
    Pin<Box<dyn Future<Output = std::result::Result<TokioIo<TcpStream>, BoxError>> + Send>>;

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
    interceptor: Mutex<Option<Arc<dyn RequestInterceptor>>>,
}

impl HttpClient {
    pub fn new(config: &Config) -> Self {
        let connector = build_connector(config);

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
            interceptor: Mutex::new(None),
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

    pub fn cookie_count(&self) -> usize {
        self.cookies.lock().map(|jar| jar.len()).unwrap_or(0)
    }

    pub fn clear_cookies(&self) {
        if let Ok(mut jar) = self.cookies.lock() {
            jar.clear();
        }
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Perform a request, following redirects and buffering the body.
    pub async fn fetch(&self, mut request: Request) -> Result<Response> {
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

        let timeout = request.timeout.unwrap_or(self.default_timeout);
        let max_redirects = request.max_redirects.unwrap_or(self.max_redirects);

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
                .and_then(|jar| jar.cookie_header(&request.url))
                .is_none();

        if cache_allowed && let Some(cached) = self.cache.get(&request.url) {
            return Ok(cached);
        }

        let mut url = request.url;
        let mut method = request.method;
        let mut headers = request.headers;
        let mut body = request.body;
        let mut redirects: Vec<Url> = Vec::new();

        for _ in 0..=max_redirects {
            let mut builder = hyper::Request::builder()
                .method(method.clone())
                .uri(url.as_str());

            let cookie_header = self
                .cookies
                .lock()
                .ok()
                .and_then(|jar| jar.cookie_header(&url));

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

#[derive(Clone)]
enum Connector {
    Direct(HttpConnector),
    Proxy(ProxyConnector),
}

impl Service<hyper::Uri> for Connector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = ConnectFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, destination: hyper::Uri) -> Self::Future {
        match self {
            Connector::Direct(connector) => {
                let future = connector.call(destination);
                Box::pin(async move { future.await.map_err(|error| Box::new(error) as BoxError) })
            }
            Connector::Proxy(connector) => connector.call(destination),
        }
    }
}

#[derive(Clone)]
struct ProxyConfig {
    host: String,
    port: u16,
    authorization: Option<HeaderValue>,
}

#[derive(Clone)]
struct ProxyConnector {
    inner: HttpConnector,
    proxy: ProxyConfig,
}

impl ProxyConnector {
    fn new(proxy: ProxyConfig) -> Self {
        Self {
            inner: HttpConnector::new(),
            proxy,
        }
    }
}

impl Service<hyper::Uri> for ProxyConnector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = ConnectFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, destination: hyper::Uri) -> Self::Future {
        let proxy = self.proxy.clone();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let proxy_uri: hyper::Uri = format!("http://{}:{}", proxy.host, proxy.port)
                .parse()
                .map_err(|_| connector_error("invalid proxy address"))?;
            let stream = inner
                .call(proxy_uri)
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            let mut tcp = stream.into_inner();

            let host = destination
                .host()
                .ok_or_else(|| connector_error("missing proxy destination host"))?;
            let port = destination.port_u16().unwrap_or_else(|| {
                if destination.scheme_str() == Some("https") {
                    443
                } else {
                    80
                }
            });
            let authority = format!("{host}:{port}");
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

            // Read the response head byte-by-byte so we never consume TLS
            // bytes that follow the CONNECT response.
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
            Ok(TokioIo::new(tcp))
        })
    }
}

fn connector_error(message: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(message.into()))
}

fn build_connector(config: &Config) -> hyper_rustls::HttpsConnector<Connector> {
    let connector = match config.proxy.as_deref().and_then(parse_proxy) {
        Some(proxy) => Connector::Proxy(ProxyConnector::new(proxy)),
        None => {
            let mut http = HttpConnector::new();
            http.enforce_http(false);
            Connector::Direct(http)
        }
    };
    HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(connector)
}

fn parse_proxy(raw: &str) -> Option<ProxyConfig> {
    let url = match Url::parse(raw) {
        Ok(url) => url,
        Err(error) => {
            warn!("invalid proxy URL {raw}: {error}");
            return None;
        }
    };
    if url.scheme() != "http" {
        warn!(
            "unsupported proxy scheme {}: only http proxies are supported",
            url.scheme()
        );
        return None;
    }
    let host = url.host_str()?.to_owned();
    let port = url.port_or_known_default().unwrap_or(8080);
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
        authorization,
    })
}
