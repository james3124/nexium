//! In-memory RFC 6265-style cookie jar (persistence arrives in Phase 4).

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use hyper::HeaderMap;
use url::Url;

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
}

pub struct CookieJar {
    cookies: HashMap<(String, String, String), Cookie>,
    enabled: bool,
}

impl CookieJar {
    pub fn new(enabled: bool) -> Self {
        Self {
            cookies: HashMap::new(),
            enabled,
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

    /// Build the `Cookie` request header value for a URL.
    pub fn cookie_header(&self, url: &Url) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let path = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        let secure = url.scheme() == "https";
        let now = SystemTime::now();

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
    let host = url.host_str()?.to_ascii_lowercase();
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

    for attribute in parts {
        let (key, attribute_value) = attribute
            .split_once('=')
            .map(|(key, value)| (key.trim(), value.trim()))
            .unwrap_or((attribute.trim(), ""));
        match key.to_ascii_lowercase().as_str() {
            "domain" => {
                let domain_value = attribute_value.trim_start_matches('.').to_ascii_lowercase();
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

    let (domain, host_only) = match domain {
        Some(domain) => {
            if host == domain || host.ends_with(&format!(".{domain}")) {
                (domain, false)
            } else {
                return None;
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    #[test]
    fn domain_path_and_secure_matching() {
        let jar = CookieJar::new(true);
        let mut jar = jar;
        jar.set_from_headers(&url("https://www.example.com/app/login"), &{
            let mut headers = HeaderMap::new();
            headers.insert(
                "set-cookie",
                "session=abc; Domain=example.com; Path=/app; Secure"
                    .parse()
                    .unwrap(),
            );
            headers
        });
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
        jar.set_from_headers(&url("https://example.com/a/b/page"), &{
            let mut headers = HeaderMap::new();
            headers.insert("set-cookie", "x=1".parse().unwrap());
            headers
        });
        assert_eq!(default_path("/a/b/page"), "/a/b");
        assert_eq!(
            jar.cookie_header(&url("https://example.com/a/b/other")),
            Some("x=1".to_owned())
        );
        assert_eq!(jar.cookie_header(&url("https://example.com/a")), None);

        let mut headers = HeaderMap::new();
        headers.insert("set-cookie", "x=; Max-Age=0; Path=/a/b".parse().unwrap());
        jar.set_from_headers(&url("https://example.com/"), &headers);
        assert_eq!(jar.len(), 0);
    }

    #[test]
    fn rejects_foreign_domains() {
        let mut jar = CookieJar::new(true);
        let mut headers = HeaderMap::new();
        headers.insert("set-cookie", "a=b; Domain=other.com".parse().unwrap());
        jar.set_from_headers(&url("https://example.com/"), &headers);
        assert!(jar.is_empty());
    }

    #[test]
    fn disabled_jar_does_nothing() {
        let mut jar = CookieJar::new(false);
        let mut headers = HeaderMap::new();
        headers.insert("set-cookie", "a=b".parse().unwrap());
        jar.set_from_headers(&url("https://example.com/"), &headers);
        assert!(jar.cookie_header(&url("https://example.com/")).is_none());
        assert!(jar.is_empty());
    }
}
