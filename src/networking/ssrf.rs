//! SSRF (server-side request forgery) policy helpers.
//!
//! When [`crate::config::Config::block_private_networks`] is enabled the
//! [`crate::HttpClient`] resolves every request host (on every redirect hop)
//! and rejects the request when *any* resolved address is loopback, private,
//! link-local, unspecified or multicast. Checking every resolved address
//! defeats the simplest DNS-rebinding trick, where a hostname carries both a
//! literal IP that passes a textual check and a private address that is used
//! for the actual connection.
//!
//! ## Rebinding limitation
//!
//! A pre-resolve check cannot fully prevent rebinding: DNS may return a
//! different answer between validation and connection. To narrow that window
//! the client pins the validated addresses and the direct connector dials the
//! pinned [`std::net::SocketAddr`]s instead of resolving again. Pinning is per
//! client and keyed by `host:port`, so later requests to the same authority
//! reuse the first validated answer.
//!
//! Pinning cannot be applied when the connection is made by a proxy: an HTTP
//! CONNECT proxy and `socks5h://` resolve the destination themselves, and even
//! `socks5://` (local DNS) resolves again inside the handshake. In those modes
//! the pre-resolve check still runs, but a proxy-side rebinding window remains.
//! Use bypass/`no_proxy` rules to keep sensitive hosts off third-party proxies.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Failure returned by [`resolve_and_validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsrfError {
    /// The host resolved to (at least) one address that is not globally
    /// routable. The message contains `private-network` for compatibility
    /// with existing callers and logs.
    Blocked { host: String, address: IpAddr },
    /// The host could not be resolved. SSRF mode fails closed.
    Resolution { host: String, message: String },
}

impl fmt::Display for SsrfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SsrfError::Blocked { host, address } => write!(
                formatter,
                "private-network requests are blocked by configuration: {host} resolves to {address}"
            ),
            SsrfError::Resolution { host, message } => {
                write!(formatter, "cannot resolve {host}: {message}")
            }
        }
    }
}

impl std::error::Error for SsrfError {}

/// True when `address` must not be reached by a request protected by SSRF
/// blocking. Covers loopback, RFC 1918 private, link-local, unspecified,
/// multicast and other reserved/non-globally-routable ranges.
pub fn is_blocked_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_blocked_ipv4(address),
        IpAddr::V6(address) => is_blocked_ipv6(address),
    }
}

fn is_blocked_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
        // 0.0.0.0/8 "this network".
        || octets[0] == 0
        // 100.64.0.0/10 carrier-grade NAT / shared address space.
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        // 192.0.0.0/24 IETF protocol assignments.
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        // 198.18.0.0/15 benchmarking.
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        // 240.0.0.0/4 reserved (255.255.255.255 is already broadcast).
        || octets[0] >= 240
}

fn is_blocked_ipv6(address: Ipv6Addr) -> bool {
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        || address.to_ipv4_mapped().is_some_and(is_blocked_ipv4)
}

/// True when `host` is a `localhost`-style name or a blocked IP literal.
pub fn is_blocked_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(is_blocked_address)
}

/// Resolve `host:port` and reject the target when any resolved address is
/// blocked. Returns all resolved addresses so the caller can pin them for the
/// connection, closing the rebinding window on the direct (non-proxy) path.
pub async fn resolve_and_validate(
    host: &str,
    port: u16,
) -> std::result::Result<Vec<SocketAddr>, SsrfError> {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(SsrfError::Resolution {
            host: host.to_owned(),
            message: "empty host".to_owned(),
        });
    }

    let addresses: Vec<SocketAddr> = if let Ok(address) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(address, port)]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| SsrfError::Resolution {
                host: host.to_owned(),
                message: error.to_string(),
            })?
            .collect()
    };

    if addresses.is_empty() {
        return Err(SsrfError::Resolution {
            host: host.to_owned(),
            message: "no addresses returned".to_owned(),
        });
    }

    if let Some(blocked) = addresses
        .iter()
        .map(SocketAddr::ip)
        .find(|address| is_blocked_address(*address))
    {
        return Err(SsrfError::Blocked {
            host: host.to_owned(),
            address: blocked,
        });
    }

    Ok(addresses)
}
