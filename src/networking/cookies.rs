//! In-memory RFC 6265-style cookie jar with import/export for profiles.
//!
//! ## SameSite approximation
//!
//! The engine does not yet give the network layer an explicit "top-level
//! navigation" flag, so two pragmatic approximations are used:
//!
//! * A request is treated as a top-level navigation when it is a `GET` whose
//!   `Accept` header asks for an HTML document (`text/html...`). The browser
//!   uses exactly that header for page loads, while sub-resources, `fetch()`
//!   and `XMLHttpRequest` use `*/*`, `text/css`, ... and stay non-navigations.
//! * "Same-site" is computed from the scheme-independent registrable domain
//!   (eTLD+1) of the top-level document and of the requested host. The
//!   top-level document site is recorded by [`CookieJar::set_top_level_site`]
//!   after a navigation completes. Without a recorded top-level document
//!   (for example when [`super::client::HttpClient`] is used directly) every
//!   request is treated as first-party.
//!
//! The registrable domain comes from a small built-in public-suffix table
//! covering common multi-label suffixes plus a few private suffixes. It is a
//! pragmatic subset of the Public Suffix List, not the full list.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hyper::HeaderMap;
use serde::{Deserialize, Serialize};
use url::{Host, Url};

/// `SameSite` cookie attribute (RFC 6265bis).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SameSite {
    /// Sent on same-site requests and on top-level safe `GET` navigations.
    #[default]
    Lax,
    /// Sent only on same-site requests.
    Strict,
    /// Sent on all requests; requires the `Secure` attribute.
    None,
}

#[derive(Clone, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub expires: Option<SystemTime>,
    pub host_only: bool,
    pub same_site: SameSite,
}

/// Serializable cookie used for profile persistence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CookieRecord {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub host_only: bool,
    pub expires: Option<u64>,
    /// Older profiles stored before SameSite support deserialize as `Lax`,
    /// matching the RFC 6265bis default for cookies without the attribute.
    #[serde(default)]
    pub same_site: SameSite,
}

impl Cookie {
    pub fn to_record(&self) -> CookieRecord {
        CookieRecord {
            name: self.name.clone(),
            value: self.value.clone(),
            domain: self.domain.clone(),
            path: self.path.clone(),
            secure: self.secure,
            http_only: self.http_only,
            host_only: self.host_only,
            expires: self
                .expires
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs()),
            same_site: self.same_site,
        }
    }

    pub fn from_record(record: &CookieRecord) -> Self {
        Self {
            name: record.name.clone(),
            value: record.value.clone(),
            domain: record.domain.clone(),
            path: record.path.clone(),
            secure: record.secure,
            http_only: record.http_only,
            host_only: record.host_only,
            expires: record
                .expires
                .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds))),
            same_site: record.same_site,
        }
    }
}

pub struct CookieJar {
    cookies: HashMap<(String, String, String), Cookie>,
    enabled: bool,
    /// Registrable domain of the current top-level document, if known.
    top_level_site: Option<String>,
}

impl CookieJar {
    pub fn new(enabled: bool) -> Self {
        Self {
            cookies: HashMap::new(),
            enabled,
            top_level_site: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn len(&self) -> usize {
        self.cookies
            .values()
            .filter(|cookie| !is_expired(cookie))
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    /// Record the site of the document that is now on top. Called by the
    /// client after a navigation, see the module-level approximation notes.
    pub fn set_top_level_site(&mut self, url: &Url) {
        self.top_level_site = request_host(url).map(|host| registrable_domain(&host));
    }

    /// Site of the current top-level document, if a navigation was observed.
    pub fn top_level_site(&self) -> Option<&str> {
        self.top_level_site.as_deref()
    }

    pub fn records(&self) -> Vec<CookieRecord> {
        let now = SystemTime::now();
        self.cookies
            .values()
            .filter(|cookie| !cookie.expires.is_some_and(|expires| expires <= now))
            .map(Cookie::to_record)
            .collect()
    }

    pub fn restore(&mut self, records: &[CookieRecord]) {
        for record in records {
            let cookie = Cookie::from_record(record);
            let key = (
                cookie.domain.clone(),
                cookie.path.clone(),
                cookie.name.clone(),
            );
            self.cookies.insert(key, cookie);
        }
    }

    /// Apply a `document.cookie = "name=value; ..."` assignment.
    pub fn set_document_cookie(&mut self, url: &Url, value: &str) {
        if !self.enabled {
            return;
        }
        let header = if value.trim_end().ends_with(';') {
            format!("{value} path=/")
        } else {
            format!("{value}; path=/")
        };
        if let Some(cookie) = parse_set_cookie(url, &header) {
            self.store(cookie);
        }
    }

    pub fn set_from_headers(&mut self, url: &Url, headers: &HeaderMap) {
        if !self.enabled {
            return;
        }
        for value in headers.get_all("set-cookie") {
            let Ok(value) = value.to_str() else {
                continue;
            };
            if let Some(cookie) = parse_set_cookie(url, value) {
                self.store(cookie);
            }
        }
    }

    fn store(&mut self, cookie: Cookie) {
        let key = (
            cookie.domain.clone(),
            cookie.path.clone(),
            cookie.name.clone(),
        );
        if cookie
            .expires
            .is_some_and(|expires| expires <= SystemTime::now())
        {
            self.cookies.remove(&key);
        } else {
            self.cookies.insert(key, cookie);
        }
    }

    /// Build the `Cookie` request header value for a URL, treating the
    /// request as a non-navigation (sub-resource / script) request.
    pub fn cookie_header(&self, url: &Url) -> Option<String> {
        self.cookie_header_for(url, false)
    }

    /// Build the `Cookie` request header value for a URL.
    ///
    /// `top_level_navigation` marks the request as a top-level safe `GET`
    /// navigation, which is what allows `SameSite=Lax` cookies to be sent
    /// cross-site. It is ignored for `SameSite=Strict`/`SameSite=None`.
    pub fn cookie_header_for(&self, url: &Url, top_level_navigation: bool) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let host = request_host(url)?;
        let path = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        let secure = url.scheme() == "https";
        let now = SystemTime::now();

        let request_site = registrable_domain(&host);
        // No observed navigation (e.g. direct API use): keep the historic
        // behaviour of treating every request as first-party.
        let same_site = self
            .top_level_site
            .as_deref()
            .is_none_or(|site| site == request_site);

        let mut matching: Vec<&Cookie> = self
            .cookies
            .values()
            .filter(|cookie| {
                if cookie.expires.is_some_and(|expires| expires <= now) {
                    return false;
                }
                if cookie.secure && !secure {
                    return false;
                }
                if !same_site_allows(cookie.same_site, same_site, top_level_navigation) {
                    return false;
                }
                let domain_ok = if cookie.host_only {
                    host == cookie.domain
                } else {
                    host == cookie.domain || host.ends_with(&format!(".{}", cookie.domain))
                };
                domain_ok && path_matches(path, &cookie.path)
            })
            .collect();
        matching.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
        if matching.is_empty() {
            return None;
        }
        Some(
            matching
                .iter()
                .map(|cookie| format!("{}={}", cookie.name, cookie.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

fn same_site_allows(
    same_site: SameSite,
    request_is_same_site: bool,
    top_level_navigation: bool,
) -> bool {
    match same_site {
        SameSite::None => true,
        SameSite::Strict => request_is_same_site,
        SameSite::Lax => request_is_same_site || top_level_navigation,
    }
}

/// Canonical request host for cookie matching: lowercased domain without a
/// trailing dot, or the text form of an IP address.
fn request_host(url: &Url) -> Option<String> {
    let host = match url.host()? {
        Host::Domain(domain) => domain.trim_end_matches('.').to_ascii_lowercase(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => address.to_string(),
    };
    if host.is_empty() { None } else { Some(host) }
}

fn is_expired(cookie: &Cookie) -> bool {
    cookie
        .expires
        .is_some_and(|expires| expires <= SystemTime::now())
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

fn default_path(request_path: &str) -> String {
    if !request_path.starts_with('/') {
        return "/".to_owned();
    }
    match request_path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(index) => request_path[..index].to_owned(),
    }
}

fn parse_set_cookie(url: &Url, header: &str) -> Option<Cookie> {
    let host = request_host(url)?;
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let value = value.trim().to_owned();

    let mut domain: Option<String> = None;
    let mut path: Option<String> = None;
    let mut secure = false;
    let mut http_only = false;
    let mut expires: Option<SystemTime> = None;
    let mut max_age: Option<i64> = None;
    let mut same_site: Option<SameSite> = None;

    for attribute in parts {
        let (key, attribute_value) = attribute
            .split_once('=')
            .map(|(key, value)| (key.trim(), value.trim()))
            .unwrap_or((attribute.trim(), ""));
        match key.to_ascii_lowercase().as_str() {
            "domain" => {
                let domain_value = attribute_value
                    .trim_start_matches('.')
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if domain_value.is_empty() {
                    return None;
                }
                domain = Some(domain_value);
            }
            "path" => {
                if attribute_value.starts_with('/') {
                    path = Some(attribute_value.to_owned());
                }
            }
            "secure" => secure = true,
            "httponly" => http_only = true,
            "max-age" => {
                if let Ok(seconds) = attribute_value.parse::<i64>() {
                    max_age = Some(seconds);
                }
            }
            "expires" => {
                if let Ok(time) = httpdate::parse_http_date(attribute_value) {
                    expires = Some(time);
                }
            }
            "samesite" => {
                // Unknown values fall back to the modern default (`Lax`).
                same_site = Some(match attribute_value.to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "none" => SameSite::None,
                    _ => SameSite::Lax,
                });
            }
            _ => {}
        }
    }

    if let Some(seconds) = max_age {
        expires = if seconds <= 0 {
            Some(SystemTime::UNIX_EPOCH)
        } else {
            Some(SystemTime::now() + Duration::from_secs(seconds as u64))
        };
    }

    let same_site = same_site.unwrap_or_default();
    // SameSite=None only exists for secure contexts.
    if same_site == SameSite::None && !secure {
        return None;
    }

    let (domain, host_only) = match domain {
        Some(domain) => {
            if domain == host {
                // A Domain attribute naming the request host itself is a
                // host-only cookie (RFC 6265 section 5.3 step 6).
                (domain, true)
            } else if host.parse::<IpAddr>().is_ok() {
                // Domain cookies cannot widen an IP-address host.
                return None;
            } else if !host.ends_with(&format!(".{domain}")) {
                // Domain must domain-match the request host.
                return None;
            } else if is_public_suffix(&domain) {
                // Refuse cookies scoped to a public suffix (`com`, `co.uk`).
                return None;
            } else {
                (domain, false)
            }
        }
        None => (host, true),
    };

    Some(Cookie {
        name: name.to_owned(),
        value,
        domain,
        path: path.unwrap_or_else(|| default_path(url.path())),
        secure,
        http_only,
        expires,
        host_only,
        same_site,
    })
}

/// True when `domain` is (or is treated as) a public suffix and must not be
/// accepted as a cookie Domain attribute.
///
/// The table is a pragmatic subset of the Public Suffix List: single-label
/// domains are always suffixes, plus common multi-label ccTLD suffixes and a
/// few widely used private suffixes.
pub fn is_public_suffix(domain: &str) -> bool {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return true;
    }
    if domain.parse::<IpAddr>().is_ok() {
        return false;
    }
    if !domain.contains('.') {
        return true;
    }
    PUBLIC_SUFFIXES.iter().any(|suffix| *suffix == domain)
}

/// Scheme-independent registrable domain (eTLD+1) used for same-site checks.
///
/// IP addresses and single-label hosts (e.g. `localhost`) are returned as-is.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.parse::<IpAddr>().is_ok() || !host.contains('.') {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    // Find the longest known public suffix; the label before it (if any)
    // completes the registrable domain.
    for suffix_len in (2..labels.len()).rev() {
        let suffix = labels[labels.len() - suffix_len..].join(".");
        if is_public_suffix(&suffix) {
            return labels[labels.len() - suffix_len - 1..].join(".");
        }
    }
    if labels.len() >= 2 {
        labels[labels.len() - 2..].join(".")
    } else {
        host
    }
}

/// Multi-label public suffixes recognised by the built-in table.
const PUBLIC_SUFFIXES: &[&str] = &[
    "ac.at",
    "ac.il",
    "ac.in",
    "ac.jp",
    "ac.ke",
    "ac.kr",
    "ac.nz",
    "ac.th",
    "ac.uk",
    "appspot.com",
    "asn.au",
    "blogspot.com",
    "cloudfront.net",
    "co.at",
    "co.il",
    "co.in",
    "co.jp",
    "co.ke",
    "co.kr",
    "co.nz",
    "co.th",
    "co.uk",
    "com.ar",
    "com.au",
    "com.bd",
    "com.br",
    "com.cn",
    "com.co",
    "com.ec",
    "com.eg",
    "com.es",
    "com.hk",
    "com.mx",
    "com.my",
    "com.ng",
    "com.pe",
    "com.ph",
    "com.pk",
    "com.pl",
    "com.pt",
    "com.ru",
    "com.sa",
    "com.sg",
    "com.tr",
    "com.tw",
    "com.ua",
    "com.uy",
    "com.ve",
    "com.vn",
    "edu.au",
    "edu.br",
    "edu.cn",
    "edu.co",
    "edu.ec",
    "edu.eg",
    "edu.es",
    "edu.hk",
    "edu.in",
    "edu.mx",
    "edu.my",
    "edu.ng",
    "edu.pe",
    "edu.ph",
    "edu.pk",
    "edu.sa",
    "edu.sg",
    "edu.tr",
    "edu.tw",
    "edu.ua",
    "edu.uy",
    "edu.vn",
    "firm.in",
    "gen.in",
    "geek.nz",
    "go.jp",
    "go.kr",
    "go.th",
    "gob.ar",
    "gob.co",
    "gob.ec",
    "gob.es",
    "gob.mx",
    "gob.pe",
    "gob.uy",
    "gov.au",
    "gov.br",
    "gov.cn",
    "gov.co",
    "gov.eg",
    "gov.hk",
    "gov.il",
    "gov.in",
    "gov.my",
    "gov.ng",
    "gov.ph",
    "gov.pk",
    "gov.sa",
    "gov.sg",
    "gov.tr",
    "gov.tw",
    "gov.uk",
    "gov.ve",
    "gov.vn",
    "govt.nz",
    "gr.jp",
    "gv.at",
    "herokuapp.com",
    "id.au",
    "id.vn",
    "ind.in",
    "in.th",
    "ltd.uk",
    "me.uk",
    "msk.ru",
    "ne.jp",
    "ne.kr",
    "net.au",
    "net.br",
    "net.cn",
    "net.co",
    "net.eg",
    "net.hk",
    "net.in",
    "net.my",
    "net.ng",
    "net.nz",
    "net.pe",
    "net.ph",
    "net.pk",
    "net.pl",
    "net.sa",
    "net.sg",
    "net.tr",
    "net.tw",
    "net.ua",
    "net.uy",
    "net.ve",
    "net.vn",
    "netlify.app",
    "nom.es",
    "or.at",
    "or.id",
    "or.jp",
    "or.kr",
    "or.th",
    "org.ar",
    "org.au",
    "org.br",
    "org.cn",
    "org.co",
    "org.ec",
    "org.eg",
    "org.es",
    "org.hk",
    "org.il",
    "org.in",
    "org.mx",
    "org.my",
    "org.ng",
    "org.nz",
    "org.pe",
    "org.ph",
    "org.pk",
    "org.pl",
    "org.pt",
    "org.ru",
    "org.sa",
    "org.sg",
    "org.tr",
    "org.tw",
    "org.ua",
    "org.uk",
    "org.uy",
    "org.ve",
    "org.vn",
    "pages.dev",
    "plc.uk",
    "re.kr",
    "s3.amazonaws.com",
    "sch.uk",
    "school.nz",
    "spb.ru",
    "vercel.app",
    "web.id",
    "workers.dev",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("set-cookie", value.parse().unwrap());
        headers
    }

    #[test]
    fn domain_path_and_secure_matching() {
        let mut jar = CookieJar::new(true);
        jar.set_from_headers(
            &url("https://www.example.com/app/login"),
            &headers("session=abc; Domain=example.com; Path=/app; Secure"),
        );
        assert_eq!(
            jar.cookie_header(&url("https://www.example.com/app/page")),
            Some("session=abc".to_owned())
        );
        assert_eq!(
            jar.cookie_header(&url("http://www.example.com/app/page")),
            None
        );
        assert_eq!(
            jar.cookie_header(&url("https://www.example.com/other")),
            None
        );
        assert_eq!(
            jar.cookie_header(&url("https://evil-example.com/app")),
            None
        );
    }

    #[test]
    fn default_path_and_expiry() {
        let mut jar = CookieJar::new(true);
        jar.set_from_headers(&url("https://example.com/a/b/page"), &headers("x=1"));
        assert_eq!(default_path("/a/b/page"), "/a/b");
        assert_eq!(
            jar.cookie_header(&url("https://example.com/a/b/other")),
            Some("x=1".to_owned())
        );
        assert_eq!(jar.cookie_header(&url("https://example.com/a")), None);

        jar.set_from_headers(
            &url("https://example.com/"),
            &headers("x=; Max-Age=0; Path=/a/b"),
        );
        assert_eq!(jar.len(), 0);
    }

    #[test]
    fn rejects_foreign_domains() {
        let mut jar = CookieJar::new(true);
        jar.set_from_headers(
            &url("https://example.com/"),
            &headers("a=b; Domain=other.com"),
        );
        assert!(jar.is_empty());
    }

    #[test]
    fn disabled_jar_does_nothing() {
        let mut jar = CookieJar::new(false);
        jar.set_from_headers(&url("https://example.com/"), &headers("a=b"));
        assert!(jar.cookie_header(&url("https://example.com/")).is_none());
        assert!(jar.is_empty());
    }

    #[test]
    fn parses_same_site_and_requires_secure_for_none() {
        let mut jar = CookieJar::new(true);
        jar.set_from_headers(
            &url("https://example.com/"),
            &headers("strict=1; SameSite=Strict"),
        );
        jar.set_from_headers(
            &url("https://example.com/"),
            &headers("lax=1; SameSite=Lax"),
        );
        jar.set_from_headers(
            &url("https://example.com/"),
            &headers("none=1; SameSite=None"),
        );
        jar.set_from_headers(
            &url("https://example.com/"),
            &headers("secure-none=1; SameSite=None; Secure"),
        );
        jar.set_from_headers(&url("https://example.com/"), &headers("default=1"));

        let mut records = jar.records();
        records.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<(&str, SameSite)> = records
            .iter()
            .map(|record| (record.name.as_str(), record.same_site))
            .collect();
        assert_eq!(
            names,
            vec![
                ("default", SameSite::Lax),
                ("lax", SameSite::Lax),
                ("secure-none", SameSite::None),
                ("strict", SameSite::Strict),
            ]
        );
    }

    #[test]
    fn rejects_public_suffix_and_non_matching_domains() {
        let mut jar = CookieJar::new(true);
        jar.set_from_headers(
            &url("https://www.example.com/"),
            &headers("a=b; Domain=com"),
        );
        assert!(jar.is_empty());
        jar.set_from_headers(
            &url("https://www.example.co.uk/"),
            &headers("a=b; Domain=co.uk"),
        );
        assert!(jar.is_empty());
        jar.set_from_headers(
            &url("https://www.example.co.uk/"),
            &headers("a=b; Domain=example.co.uk"),
        );
        assert_eq!(jar.len(), 1);
        jar.set_from_headers(
            &url("https://www.example.com/"),
            &headers("c=d; Domain=example.com"),
        );
        assert_eq!(jar.len(), 2);
    }

    #[test]
    fn same_site_filtering_uses_top_level_site() {
        let mut jar = CookieJar::new(true);
        jar.set_from_headers(
            &url("https://api.tracker.example/"),
            &headers("strict=1; Domain=tracker.example; SameSite=Strict"),
        );
        jar.set_from_headers(
            &url("https://api.tracker.example/"),
            &headers("lax=1; Domain=tracker.example; SameSite=Lax"),
        );
        jar.set_from_headers(
            &url("https://api.tracker.example/"),
            &headers("none=1; Domain=tracker.example; SameSite=None; Secure"),
        );

        // Same-site sub-resource: everything except Secure over http.
        jar.set_top_level_site(&url("https://www.tracker.example/page"));
        assert_eq!(
            sorted_cookies(jar.cookie_header_for(&url("https://api.tracker.example/x"), false)),
            vec!["lax=1", "none=1", "strict=1"]
        );

        // Cross-site sub-resource: only SameSite=None survives.
        jar.set_top_level_site(&url("https://other.example/"));
        assert_eq!(
            sorted_cookies(jar.cookie_header_for(&url("https://api.tracker.example/x"), false)),
            vec!["none=1"]
        );

        // Cross-site top-level navigation: Lax and None survive.
        assert_eq!(
            sorted_cookies(jar.cookie_header_for(&url("https://api.tracker.example/x"), true)),
            vec!["lax=1", "none=1"]
        );
    }

    fn sorted_cookies(header: Option<String>) -> Vec<String> {
        let mut cookies: Vec<String> = header
            .unwrap_or_default()
            .split("; ")
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
        cookies.sort();
        cookies
    }

    #[test]
    fn record_json_without_same_site_defaults_to_lax() {
        let record: CookieRecord = serde_json::from_str(
            r#"{"name":"a","value":"b","domain":"example.com","path":"/","secure":false,"http_only":false,"host_only":true,"expires":null}"#,
        )
        .unwrap();
        assert_eq!(record.same_site, SameSite::Lax);
    }

    #[test]
    fn registrable_domains_use_public_suffix_table() {
        assert_eq!(registrable_domain("www.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("a.b.example.com"), "example.com");
        assert_eq!(registrable_domain("example.com"), "example.com");
        assert_eq!(registrable_domain("co.uk"), "co.uk");
        assert_eq!(registrable_domain("localhost"), "localhost");
        assert_eq!(registrable_domain("127.0.0.1"), "127.0.0.1");
        assert!(is_public_suffix("com"));
        assert!(is_public_suffix("CO.UK"));
        assert!(!is_public_suffix("example.com"));
        assert!(!is_public_suffix("127.0.0.1"));
    }
}
