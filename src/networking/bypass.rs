//! Proxy bypass (`no_proxy`) rule parsing and matching.
//!
//! Supported entry forms:
//!
//! | Entry | Matches |
//! | --- | --- |
//! | `*` | every host |
//! | `example.com` | exactly `example.com` |
//! | `.example.com`, `*.example.com` | `example.com` and any `*.example.com` subdomain |
//! | `127.0.0.1`, `::1`, `[::1]` | that IP literal |
//! | `10.0.0.0/8`, `fd00::/8` | any host that is an IP inside the range |
//! | `example.com:8080`, `[::1]:8080` | that host, only on that port |
//!
//! Entries are case-insensitive and may be separated by commas, semicolons or
//! whitespace (the `NO_PROXY` environment convention). When the configuration
//! list is empty the client falls back to `NO_PROXY` and then `no_proxy`.
//!
//! Plain hostnames match exactly; use the `.suffix`/`*.suffix` forms for a
//! domain and its subdomains. CIDR entries are matched against IP literals in
//! the request URL only: bypass matching never performs DNS resolution, so a
//! hostname that resolves into a bypassed range is still proxied.

use std::net::IpAddr;

use url::Url;

use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Rule {
    All,
    Host {
        host: String,
        port: Option<u16>,
        address: Option<IpAddr>,
    },
    Domain {
        suffix: String,
    },
    Cidr {
        network: IpAddr,
        prefix: u8,
    },
}

/// A parsed proxy bypass list.
#[derive(Debug, Clone, Default)]
pub struct ProxyBypass {
    rules: Vec<Rule>,
}

impl ProxyBypass {
    /// Build the bypass list from configuration, falling back to the
    /// `NO_PROXY` / `no_proxy` environment variables when the configuration
    /// list contains no usable entries.
    pub fn from_config(config: &Config) -> Self {
        let environment = std::env::var("NO_PROXY")
            .ok()
            .or_else(|| std::env::var("no_proxy").ok());
        Self::from_config_with_env(config, environment.as_deref())
    }

    /// Testable form of [`ProxyBypass::from_config`]: `environment` stands in
    /// for the `NO_PROXY` value.
    pub fn from_config_with_env(config: &Config, environment: Option<&str>) -> Self {
        let mut bypass = Self::from_entries(&config.no_proxy);
        if bypass.is_empty()
            && let Some(environment) = environment
        {
            bypass = Self::from_entries([environment]);
        }
        bypass
    }

    /// Parse raw entries (each may itself contain several separators).
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut rules = Vec::new();
        for entry in entries {
            for token in entry
                .as_ref()
                .split([',', ';', ' ', '\t', '\r', '\n'])
                .filter(|token| !token.is_empty())
            {
                if let Some(rule) = parse_rule(token) {
                    rules.push(rule);
                }
            }
        }
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// True when `host` (and `port`, when known) must bypass the proxy.
    pub fn matches(&self, host: &str, port: Option<u16>) -> bool {
        let host = normalize_host(host);
        if host.is_empty() {
            return false;
        }
        let address = host.parse::<IpAddr>().ok();

        for rule in &self.rules {
            match rule {
                Rule::All => return true,
                Rule::Host {
                    host: rule_host,
                    port: rule_port,
                    address: rule_address,
                } => {
                    if rule_port.is_some_and(|rule_port| Some(rule_port) != port) {
                        continue;
                    }
                    let same_host = match (rule_address, address) {
                        (Some(rule_address), Some(address)) => *rule_address == address,
                        _ => *rule_host == host,
                    };
                    if same_host {
                        return true;
                    }
                }
                Rule::Domain { suffix } => {
                    // Domain rules never apply to IP literals.
                    if address.is_some() {
                        continue;
                    }
                    if host == *suffix || host.ends_with(&format!(".{suffix}")) {
                        return true;
                    }
                }
                Rule::Cidr { network, prefix } => {
                    if let Some(address) = address
                        && cidr_contains(*network, *prefix, address)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// True when the URL's host must bypass the proxy.
    pub fn matches_url(&self, url: &Url) -> bool {
        url.host_str()
            .is_some_and(|host| self.matches(host, url.port_or_known_default()))
    }
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn parse_rule(token: &str) -> Option<Rule> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if token == "*" {
        return Some(Rule::All);
    }

    let (raw_host, port) = split_host_port(token)?;
    let host = normalize_host(raw_host);
    if host.is_empty() {
        return None;
    }
    if host == "*" {
        // `*:port` is not offered by browsers; treat a bare wildcard only.
        return (port.is_none()).then_some(Rule::All);
    }

    if let Some((network, prefix)) = host.split_once('/') {
        if port.is_some() {
            return None;
        }
        let network = network.parse::<IpAddr>().ok()?;
        let prefix = prefix.parse::<u8>().ok()?;
        let max = match network {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        return (prefix <= max).then_some(Rule::Cidr { network, prefix });
    }

    if let Some(suffix) = host.strip_prefix("*.").or_else(|| host.strip_prefix('.')) {
        if suffix.is_empty() || suffix.contains('*') {
            return None;
        }
        // Domain suffixes apply to every port; a port-qualified suffix is
        // ambiguous, so fail safe by treating the entry as unusable.
        if port.is_some() {
            return None;
        }
        return Some(Rule::Domain {
            suffix: suffix.to_owned(),
        });
    }
    if host.contains('*') {
        return None;
    }

    let address = host.parse::<IpAddr>().ok();
    Some(Rule::Host {
        host,
        port,
        address,
    })
}

/// Split an entry into host and optional port. Bracketed IPv6 (`[::1]:8080`)
/// carries a port after the closing bracket; a bare IPv6 literal never does.
fn split_host_port(token: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = token.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        let tail = &rest[close + 1..];
        if tail.is_empty() {
            return Some((host, None));
        }
        let port = tail.strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host, Some(port)));
    }

    let colon_count = token.bytes().filter(|byte| *byte == b':').count();
    match colon_count {
        0 => Some((token, None)),
        1 => {
            let (host, port) = token.rsplit_once(':')?;
            let port = port.parse::<u16>().ok()?;
            Some((host, Some(port)))
        }
        // Bare IPv6 literal: no port can be expressed without brackets.
        _ => Some((token, None)),
    }
}

fn cidr_contains(network: IpAddr, prefix: u8, address: IpAddr) -> bool {
    match (network, address) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            if prefix > 32 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            (u32::from(network) & mask) == (u32::from(address) & mask)
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => {
            if prefix > 128 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            (u128::from(network) & mask) == (u128::from(address) & mask)
        }
        _ => false,
    }
}
