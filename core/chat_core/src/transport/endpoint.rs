use super::frame::ALPN;
use crate::{CoreError, ErrorCode};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;

#[derive(Debug, Clone)]
pub enum NetworkProfile {
    Public,
    #[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
    Local {
        relay: Option<iroh::RelayUrl>,
    },
}

pub(crate) async fn bind_endpoint(
    profile: &NetworkProfile,
    key: SecretKey,
) -> Result<Endpoint, CoreError> {
    let builder = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .alpns(vec![
            ALPN.to_vec(),
            crate::account::auth::ALPN.to_vec(),
            crate::account::pair::ALPN.to_vec(),
        ]);
    let builder = match profile {
        NetworkProfile::Public => builder.relay_mode(RelayMode::Default).address_lookup(
            iroh_mainline_address_lookup::DhtAddressLookup::builder()
                .addr_filter(iroh::address_lookup::AddrFilter::relay_only()),
        ),
        #[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
        NetworkProfile::Local { relay } => builder.relay_mode(match relay {
            Some(url) => RelayMode::Custom(iroh::RelayMap::from_iter([url.clone()])),
            None => RelayMode::Disabled,
        }),
    };
    builder
        .bind()
        .await
        .map_err(|_| CoreError::new(ErrorCode::TransportFailed, "endpoint bind failed"))
}

pub(crate) fn endpoint_ticket(endpoint: &Endpoint, profile: &NetworkProfile) -> String {
    let addr = endpoint.addr();
    #[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
    let addr = match profile {
        NetworkProfile::Local { relay: None } => {
            let sockets = endpoint.bound_sockets().into_iter().map(|mut socket| {
                if socket.ip().is_unspecified() {
                    socket.set_ip(if socket.is_ipv4() {
                        std::net::Ipv4Addr::LOCALHOST.into()
                    } else {
                        std::net::Ipv6Addr::LOCALHOST.into()
                    });
                }
                iroh::TransportAddr::Ip(socket)
            });
            EndpointAddr::from_parts(endpoint.id(), sockets)
        }
        // The relay only, as public records carry (relay-only address lookup): a dial sends
        // its handshake to every direct address of a ticket at once, and an Android
        // emulator's user-mode NAT stalled those handshakes until the dial deadline. Direct
        // paths are still found after the handshake.
        NetworkProfile::Local { relay: Some(url) } => {
            EndpointAddr::from_parts(endpoint.id(), [iroh::TransportAddr::Relay(url.clone())])
        }
        NetworkProfile::Public => addr,
    };
    let _ = profile;
    EndpointTicket::new(addr).to_string()
}

pub(crate) fn parse_target(target: &str) -> Result<EndpointAddr, CoreError> {
    target
        .parse::<EndpointTicket>()
        .map(|ticket| ticket.endpoint_addr().clone())
        .or_else(|_| target.parse::<EndpointId>().map(EndpointAddr::from))
        .map_err(|_| CoreError::new(ErrorCode::InvalidPeerAddress, "invalid peer address"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_and_id_parser_preserve_identity_and_reject_junk() {
        let key = SecretKey::generate();
        let addr = EndpointAddr::from_parts(
            key.public(),
            [iroh::TransportAddr::Ip("127.0.0.1:42000".parse().unwrap())],
        );
        let ticket = EndpointTicket::new(addr.clone()).to_string();
        assert_eq!(parse_target(&ticket).unwrap(), addr);
        assert_eq!(
            parse_target(&key.public().to_string()).unwrap().id,
            key.public()
        );
        for invalid in ["", "not-an-address", "http://127.0.0.1:42"] {
            assert_eq!(
                parse_target(invalid).unwrap_err().code,
                ErrorCode::InvalidPeerAddress
            );
        }
    }
    /// Task 18: with a relay configured, a local ticket names the relay only (as public
    /// records do), never the endpoint's direct addresses.
    #[tokio::test]
    async fn local_relay_ticket_names_only_the_relay() {
        let endpoint = bind_endpoint(&NetworkProfile::Local { relay: None }, SecretKey::generate())
            .await
            .unwrap();
        assert!(endpoint.addr().ip_addrs().count() > 0);
        let relay: iroh::RelayUrl = "http://127.0.0.1:3340".parse().unwrap();
        let profile = NetworkProfile::Local { relay: Some(relay.clone()) };
        let addr = parse_target(&endpoint_ticket(&endpoint, &profile)).unwrap();
        assert_eq!(addr.id, endpoint.id());
        assert_eq!(addr.relay_urls().collect::<Vec<_>>(), [&relay]);
        assert_eq!(addr.ip_addrs().count(), 0);
        endpoint.close().await;
    }
}
