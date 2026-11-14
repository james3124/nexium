//! Minimal PAC (Proxy Auto-Config) support.
//!
//! Handles the common `FindProxyForURL` shape: ordered `if (...) return "..."`
//! rules using `shExpMatch`, `dnsDomainIs`, `isPlainHostName`, host equality,
//! `!`, `&&` and `||`, plus a fallback return. Directives inside one return
//! value are tried in order (`PROXY`, `HTTP`, `SOCKS5`, `SOCKS5H`, `DIRECT`).
//!
//! Arbitrary JavaScript in PAC files is not executed; unsupported conditions
//! simply never match and fall through.

use std::fs;
use std::net::ToSocketAddrs;
use std::path::Path;
use std::str::FromStr;

use boa_engine::{Context, JsResult, JsString, JsValue, NativeFunction, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    HttpConnect,
    Socks5,
    Socks5h,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum PacRoute {
    /// No PAC decision; use the statically configured proxy, if any.
    #[default]
    Default,
    Direct,
    Proxy {
        kind: RouteKind,
        host: String,
        port: u16,
    },
}

#[derive(Clone, Debug)]
enum Condition {
    Any,
    Not(Box<Condition>),
    All(Vec<Condition>),
    Or(Vec<Condition>),
    HostEquals(String),
    HostSuffix(String),
    ShExpMatch(String),
    IsPlainHostName,
}

impl Condition {
    fn matches(&self, host: &str) -> bool {
        match self {
            Condition::Any => true,
            Condition::Not(inner) => !inner.matches(host),
            Condition::All(parts) => parts.iter().all(|part| part.matches(host)),
            Condition::Or(parts) => parts.iter().any(|part| part.matches(host)),
            Condition::HostEquals(name) => host.eq_ignore_ascii_case(name),
            Condition::HostSuffix(suffix) => {
                let suffix = suffix.trim_start_matches('.');
                host.eq_ignore_ascii_case(suffix)
                    || host
                        .to_ascii_lowercase()
                        .ends_with(&format!(".{}", suffix.to_ascii_lowercase()))
            }
            Condition::ShExpMatch(pattern) => glob_match(&pattern.to_ascii_lowercase(), host),
            Condition::IsPlainHostName => !host.contains('.'),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PacScript {
    source: Option<String>,
    rules: Vec<(Condition, Vec<PacRoute>)>,
    fallback: Vec<PacRoute>,
}

impl PacScript {
    pub fn load(path: &Path) -> Option<Self> {
        let source = fs::read_to_string(path).ok()?;
        Self::parse(&source)
    }

    pub fn parse(source: &str) -> Option<Self> {
        let mut rules = Vec::new();
        let mut fallback = Vec::new();
        let mut pending: Option<String> = None;
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            if let Some(index) = trimmed.find("if") {
                let rest = &trimmed[index..];
                let condition = match rest.find("return") {
                    Some(end) => rest[..end].to_owned(),
                    None => rest.to_owned(),
                };
                pending = Some(condition);
            }
            let Some(return_index) = trimmed.find("return") else {
                continue;
            };
            let quoted = extract_quoted(&trimmed[return_index + "return".len()..]);
            let routes = quoted.map(parse_directives).unwrap_or_default();
            if routes.is_empty() {
                continue;
            }
            match pending.take() {
                Some(expression) => {
                    if let Some(condition) = parse_condition(&expression) {
                        rules.push((condition, routes));
                    }
                }
                None => fallback = routes,
            }
        }
        if source.trim().is_empty() || (rules.is_empty() && fallback.is_empty()) {
            return None;
        }
        Some(Self {
            source: Some(source.to_owned()),
            rules,
            fallback,
        })
    }

    /// Evaluate the script for a destination host (pattern rules only).
    pub fn route(&self, host: &str) -> PacRoute {
        let host = host.to_ascii_lowercase();
        for (condition, routes) in &self.rules {
            if condition.matches(&host) {
                return routes.first().cloned().unwrap_or_default();
            }
        }
        self.fallback.first().cloned().unwrap_or_default()
    }

    /// Execute `FindProxyForURL` with Boa; falls back to pattern rules when
    /// the script cannot be evaluated.
    pub fn route_with_url(&self, url: &str, host: &str) -> PacRoute {
        if let Some(routes) = self.evaluate(url, host)
            && let Some(route) = routes.first()
        {
            return route.clone();
        }
        self.route(host)
    }

    fn evaluate(&self, url: &str, host: &str) -> Option<Vec<PacRoute>> {
        let source = self.source.as_deref()?;
        let mut context = Context::default();
        register_pac_globals(&mut context)?;
        context.eval(Source::from_bytes(source)).ok()?;
        let function = context
            .global_object()
            .get(JsString::from("FindProxyForURL"), &mut context)
            .ok()?;
        let function = function.as_function()?;
        let result = function
            .call(
                &JsValue::undefined(),
                &[JsString::from(url).into(), JsString::from(host).into()],
                &mut context,
            )
            .ok()?;
        let directive = result.as_string()?.to_std_string_escaped();
        let routes = parse_directives(&directive);
        (!routes.is_empty()).then_some(routes)
    }
}

fn register_pac_globals(context: &mut Context) -> Option<()> {
    for (name, arity, function) in [
        (
            "shExpMatch",
            2,
            pac_sh_exp_match as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
        ),
        ("dnsDomainIs", 2, pac_dns_domain_is),
        ("isPlainHostName", 1, pac_is_plain_host_name),
        ("isInNet", 3, pac_is_in_net),
        ("dnsResolve", 1, pac_dns_resolve),
        ("dnsDomainLevels", 1, pac_dns_domain_levels),
        ("myIpAddress", 0, pac_my_ip_address),
        ("alert", 1, pac_alert),
    ] {
        context
            .register_global_callable(
                JsString::from(name),
                arity,
                NativeFunction::from_fn_ptr(function),
            )
            .ok()?;
    }
    Some(())
}

fn string_arg(args: &[JsValue], index: usize) -> Option<String> {
    args.get(index)
        .and_then(JsValue::as_string)
        .map(|value| value.to_std_string_escaped())
}

fn pac_sh_exp_match(
    _this: &JsValue,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let value = string_arg(args, 0).unwrap_or_default().to_ascii_lowercase();
    let pattern = string_arg(args, 1).unwrap_or_default().to_ascii_lowercase();
    Ok(JsValue::from(glob_match(&pattern, &value)))
}

fn pac_dns_domain_is(
    _this: &JsValue,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let host = string_arg(args, 0).unwrap_or_default().to_ascii_lowercase();
    let domain = string_arg(args, 1)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .trim_start_matches('.')
        .to_owned();
    Ok(JsValue::from(
        host.eq_ignore_ascii_case(&domain) || host.ends_with(&format!(".{domain}")),
    ))
}

fn pac_is_plain_host_name(
    _this: &JsValue,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let host = string_arg(args, 0).unwrap_or_default();
    Ok(JsValue::from(!host.contains('.')))
}

fn ipv4_to_u32(ip: std::net::Ipv4Addr) -> u32 {
    u32::from(ip)
}

fn resolve_ipv4(host: &str) -> Option<std::net::Ipv4Addr> {
    (host, 0u16)
        .to_socket_addrs()
        .ok()?
        .find_map(|address| match address.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            std::net::IpAddr::V6(_) => None,
        })
}

fn pac_is_in_net(_this: &JsValue, args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let host = string_arg(args, 0).unwrap_or_default();
    let pattern = string_arg(args, 1).unwrap_or_default();
    let mask = string_arg(args, 2).unwrap_or_default();
    let ip = std::net::Ipv4Addr::from_str(&host)
        .ok()
        .or_else(|| resolve_ipv4(&host));
    let (Some(ip), Ok(pattern), Ok(mask)) = (
        ip,
        std::net::Ipv4Addr::from_str(&pattern),
        std::net::Ipv4Addr::from_str(&mask),
    ) else {
        return Ok(JsValue::from(false));
    };
    Ok(JsValue::from(
        ipv4_to_u32(ip) & ipv4_to_u32(mask) == ipv4_to_u32(pattern) & ipv4_to_u32(mask),
    ))
}

fn pac_dns_resolve(_this: &JsValue, args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let host = string_arg(args, 0).unwrap_or_default();
    match resolve_ipv4(&host) {
        Some(ip) => Ok(JsValue::from(JsString::from(ip.to_string()))),
        None => Ok(JsValue::null()),
    }
}

fn pac_dns_domain_levels(
    _this: &JsValue,
    args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    let host = string_arg(args, 0).unwrap_or_default();
    Ok(JsValue::from(host.matches('.').count() as f64))
}

fn pac_my_ip_address(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from("127.0.0.1")))
}

fn pac_alert(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn extract_quoted(text: &str) -> Option<&str> {
    let start = text.find('"')?;
    let rest = &text[start + 1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn parse_directives(value: &str) -> Vec<PacRoute> {
    value.split(';').filter_map(parse_directive).collect()
}

fn parse_directive(value: &str) -> Option<PacRoute> {
    let value = value.trim();
    let mut parts = value.split_whitespace();
    let kind = parts.next()?.to_ascii_uppercase();
    match kind.as_str() {
        "DIRECT" => Some(PacRoute::Direct),
        "PROXY" | "HTTP" | "HTTPS" => {
            let (host, port) = proxy_address(parts.next()?, 8080);
            Some(PacRoute::Proxy {
                kind: RouteKind::HttpConnect,
                host,
                port,
            })
        }
        "SOCKS" | "SOCKS5" => {
            let (host, port) = proxy_address(parts.next()?, 1080);
            Some(PacRoute::Proxy {
                kind: RouteKind::Socks5,
                host,
                port,
            })
        }
        "SOCKS5H" => {
            let (host, port) = proxy_address(parts.next()?, 1080);
            Some(PacRoute::Proxy {
                kind: RouteKind::Socks5h,
                host,
                port,
            })
        }
        _ => None,
    }
}

fn proxy_address(value: &str, default_port: u16) -> (String, u16) {
    if let Some((host, port)) = value.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (host.to_owned(), port);
    }
    (value.to_owned(), default_port)
}

fn parse_condition(expression: &str) -> Option<Condition> {
    let expression = expression
        .trim()
        .trim_start_matches("if")
        .trim()
        .trim_start_matches('(')
        .trim()
        .trim_end_matches('{')
        .trim()
        .trim_end_matches(')')
        .trim();
    if expression.is_empty() {
        return None;
    }
    if let Some(parts) = split_top_level(expression, "||") {
        let parsed: Vec<Condition> = parts
            .iter()
            .filter_map(|part| parse_condition(part))
            .collect();
        if !parsed.is_empty() {
            return Some(Condition::Or(parsed));
        }
    }
    if let Some(parts) = split_top_level(expression, "&&") {
        let parsed: Vec<Condition> = parts
            .iter()
            .filter_map(|part| parse_condition(part))
            .collect();
        if !parsed.is_empty() {
            return Some(Condition::All(parsed));
        }
    }
    if let Some(rest) = expression.strip_prefix('!') {
        return Some(Condition::Not(Box::new(parse_condition(rest)?)));
    }
    if let Some(rest) = expression.strip_prefix("shExpMatch(") {
        let args = rest.trim_end_matches(')');
        let pattern = args.split(',').nth(1).map(str::trim).unwrap_or("");
        return Some(Condition::ShExpMatch(
            pattern.trim_matches('"').trim_matches('\'').to_owned(),
        ));
    }
    if let Some(rest) = expression.strip_prefix("dnsDomainIs(") {
        let args = rest.trim_end_matches(')');
        let domain = args.split(',').nth(1).map(str::trim).unwrap_or("");
        return Some(Condition::HostSuffix(
            domain.trim_matches('"').trim_matches('\'').to_owned(),
        ));
    }
    if expression.starts_with("isPlainHostName(") {
        return Some(Condition::IsPlainHostName);
    }
    if let Some((left, right)) = expression.split_once("==")
        && left.trim().eq_ignore_ascii_case("host")
    {
        return Some(Condition::HostEquals(
            right.trim().trim_matches('"').trim_matches('\'').to_owned(),
        ));
    }
    if expression == "true" {
        return Some(Condition::Any);
    }
    None
}

fn split_top_level(expression: &str, operator: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut start = 0usize;
    let bytes = expression.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => quoted = !quoted,
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if !quoted && depth == 0 && expression[index..].starts_with(operator) {
            parts.push(expression[start..index].to_owned());
            index += operator.len();
            start = index;
            continue;
        }
        index += 1;
    }
    if parts.is_empty() {
        None
    } else {
        parts.push(expression[start..].to_owned());
        Some(parts)
    }
}

/// Glob match supporting `*` and `?` (PAC `shExpMatch` semantics).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None;
    let mut mark = 0usize;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            mark = t;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_directives_and_conditions() {
        let script = PacScript::parse(
            "function FindProxyForURL(url, host) {\n\
             if (isPlainHostName(host)) return \"DIRECT\";\n\
             if (shExpMatch(host, \"*.internal.example\")) return \"PROXY proxy.local:3128; DIRECT\";\n\
             if (dnsDomainIs(host, \".corp\")) return \"SOCKS5 socks.local:1080\";\n\
             return \"PROXY fallback:8080\";\n\
             }",
        )
        .unwrap();
        assert_eq!(script.route("localhost"), PacRoute::Direct);
        assert_eq!(
            script.route("api.internal.example"),
            PacRoute::Proxy {
                kind: RouteKind::HttpConnect,
                host: "proxy.local".to_owned(),
                port: 3128
            }
        );
        assert_eq!(
            script.route("db.corp"),
            PacRoute::Proxy {
                kind: RouteKind::Socks5,
                host: "socks.local".to_owned(),
                port: 1080
            }
        );
        assert_eq!(
            script.route("public.example"),
            PacRoute::Proxy {
                kind: RouteKind::HttpConnect,
                host: "fallback".to_owned(),
                port: 8080
            }
        );
    }

    #[test]
    fn negated_and_compound_conditions() {
        let script = PacScript::parse(
            "function FindProxyForURL(url, host) {\n\
             if (!shExpMatch(host, \"*.public\") && host == \"api.corp\") return \"PROXY p:3128\";\n\
             return \"DIRECT\";\n\
             }",
        )
        .unwrap();
        assert_eq!(
            script.route("api.corp"),
            PacRoute::Proxy {
                kind: RouteKind::HttpConnect,
                host: "p".to_owned(),
                port: 3128
            }
        );
        assert_eq!(script.route("api.public"), PacRoute::Direct);
    }

    #[test]
    fn executes_find_proxy_for_url_javascript() {
        let script = PacScript::parse(
            "function FindProxyForURL(url, host) {\n\
             if (dnsDomainLevels(host) > 1) { return \"DIRECT\"; }\n\
             if (isInNet(host, \"127.0.0.0\", \"255.0.0.0\")) return \"DIRECT\";\n\
             return \"PROXY p:3128\";\n\
             }",
        )
        .unwrap();
        assert_eq!(
            script.route_with_url("http://a.b.example/", "a.b.example"),
            PacRoute::Direct
        );
        assert_eq!(
            script.route_with_url("http://127.0.0.1/", "127.0.0.1"),
            PacRoute::Direct
        );
        assert_eq!(
            script.route_with_url("http://plain/", "plain"),
            PacRoute::Proxy {
                kind: RouteKind::HttpConnect,
                host: "p".to_owned(),
                port: 3128
            }
        );
    }

    #[test]
    fn glob_supports_star_and_question() {
        assert!(glob_match("*.example.com", "a.example.com"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "abbc"));
    }
}
