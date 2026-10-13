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
        .alpns(vec![ALPN.to_vec()]);
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
    let addr = if matches!(profile, NetworkProfile::Local { relay: None }) {
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
    } else {
        addr
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
}
