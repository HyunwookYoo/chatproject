//! Who a request comes from, for per-client limits.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::http::HeaderMap;

/// The client's address. Behind a trusted reverse proxy that appends the peer
/// it saw, the last `X-Forwarded-For` entry; else the TCP peer.
pub fn client_ip(peer: Option<SocketAddr>, headers: &HeaderMap, trust_proxy: bool) -> IpAddr {
    let forwarded = || {
        let value = headers.get_all("x-forwarded-for").iter().next_back()?;
        value.to_str().ok()?.rsplit(',').next()?.trim().parse().ok()
    };
    if trust_proxy && let Some(ip) = forwarded() {
        return ip;
    }
    // No peer only happens in-process (tests, fakes): they share one bucket.
    peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |addr| addr.ip())
}

/// One client for a per-client limit: an IPv4 address, or an IPv6 /64 (the
/// smallest block a provider hands out, and every address in it is the
/// holder's). IPv4-mapped IPv6 is the IPv4 address.
pub fn client_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !u128::from(u64::MAX))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn client_key_keeps_ipv4() {
        assert_eq!(client_key(ip("203.0.113.7")), ip("203.0.113.7"));
    }

    #[test]
    fn client_key_masks_ipv6_to_64() {
        let key = ip("2001:db8:1:2::");
        assert_eq!(client_key(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd")), key);
        assert_eq!(client_key(ip("2001:db8:1:2::1")), key);
        assert_ne!(client_key(ip("2001:db8:1:3::1")), key);
    }

    #[test]
    fn client_key_unmaps_ipv4_mapped_ipv6() {
        assert_eq!(client_key(ip("::ffff:203.0.113.7")), ip("203.0.113.7"));
    }

    #[test]
    fn client_ip_takes_last_forwarded_entry_only_when_trusted() {
        let peer = Some(SocketAddr::from(([127, 0, 0, 1], 40_000)));
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", "198.51.100.1".parse().unwrap());
        headers.append(
            "x-forwarded-for",
            "198.51.100.2, 203.0.113.9".parse().unwrap(),
        );
        assert_eq!(client_ip(peer, &headers, true), ip("203.0.113.9"));
        assert_eq!(client_ip(peer, &headers, false), ip("127.0.0.1"));

        let mut garbage = HeaderMap::new();
        garbage.append("x-forwarded-for", "not an ip".parse().unwrap());
        assert_eq!(client_ip(peer, &garbage, true), ip("127.0.0.1"));
        assert_eq!(client_ip(None, &HeaderMap::new(), true), ip("0.0.0.0"));
    }
}
