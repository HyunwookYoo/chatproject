//! Shared signed-record fixture. Only compiled with test-hooks AND debug assertions.
use crate::{
    CoreError, ErrorCode,
    account::{
        AccountServices, Lookup, PeerId, RecordBackend, RecordFuture,
        clock::{Clock, SystemClock},
        parse_record,
    },
};
use futures_util::{SinkExt, StreamExt};
use pkarr::{PublicKey, SignedPacket};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

pub fn validate(url: &str) -> Result<(), CoreError> {
    if url == "ws://127.0.0.1:47000/records" || url == "ws://[::1]:47000/records" {
        Ok(())
    } else {
        Err(super::protocol::invalid())
    }
}
pub fn services(url: String, clock: Option<Arc<dyn Clock>>) -> Result<AccountServices, CoreError> {
    validate(&url)?;
    let clock = clock.unwrap_or_else(|| Arc::new(SystemClock::default()));
    Ok(AccountServices::new(
        Arc::new(Backend {
            url,
            clock: clock.clone(),
        }),
        clock,
    ))
}
struct Backend {
    url: String,
    clock: Arc<dyn Clock>,
}
fn unavailable() -> CoreError {
    CoreError::new(
        ErrorCode::RecordUnavailable,
        "Local record fixture unavailable",
    )
}
impl Backend {
    async fn request(&self, request: Value) -> Result<Value, CoreError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let config = WebSocketConfig::default()
                .max_message_size(Some(8192))
                .max_frame_size(Some(8192));
            let (mut socket, _) =
                tokio_tungstenite::connect_async_with_config(&self.url, Some(config), false)
                    .await
                    .map_err(|_| unavailable())?;
            socket
                .send(Message::Text(request.to_string().into()))
                .await
                .map_err(|_| unavailable())?;
            let message = socket
                .next()
                .await
                .ok_or_else(unavailable)?
                .map_err(|_| unavailable())?;
            let Message::Text(text) = message else {
                return Err(unavailable());
            };
            let reply = serde_json::from_str(&text).map_err(|_| unavailable())?;
            let _ = socket.close(None).await;
            Ok(reply)
        })
        .await
        .map_err(|_| unavailable())?
    }
}
impl RecordBackend for Backend {
    fn lookup(&self, peer: PeerId) -> RecordFuture<'_, Lookup> {
        Box::pin(async move {
            let value = self.request(json!({"op":"lookup", "peer":peer.0})).await?;
            if value["absent"] == true {
                return Ok(Lookup::CompletedAbsent);
            }
            let bytes: Vec<u8> =
                serde_json::from_value(value["packet"].clone()).map_err(|_| unavailable())?;
            if !(104..=1000).contains(&bytes.len()) || bytes[..32] != peer.0 {
                return Err(unavailable());
            }
            let key = PublicKey::try_from(&peer.0).map_err(|_| unavailable())?;
            let packet = SignedPacket::from_relay_payload(&key, &bytes[32..].to_vec().into())
                .map_err(|_| unavailable())?;
            parse_record(&packet, peer, self.clock.unix_seconds())?;
            Ok(Lookup::Found(packet))
        })
    }
    fn publish<'a>(&'a self, packet: &'a SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            if !(104..=1000).contains(&packet.as_bytes().len()) {
                return Err(unavailable());
            }
            let value = self
                .request(json!({"op":"publish", "packet":packet.as_bytes().to_vec()}))
                .await?;
            if value["ok"] == true {
                Ok(())
            } else if value["conflict"] == true {
                Err(CoreError::new(
                    ErrorCode::RecordConflict,
                    "Local record conflict",
                ))
            } else {
                Err(unavailable())
            }
        })
    }
}
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn lookup_validates_against_shared_clock_after_await() {
        use super::*;
        use crate::account::{
            DeviceKind, Generation, IdentityRecord, RecordDevice, Secret32, clock::FakeClock,
            derive_account, derive_device, record_packet,
        };
        let now = 1_800_000_000;
        let clock = Arc::new(FakeClock::new(now, 0));
        let account = derive_account(&Secret32::from([17; 32])).unwrap();
        let device = derive_device(Secret32::from([18; 32])).unwrap();
        let packet = record_packet(
            &account,
            &IdentityRecord {
                generation: Generation([1; 16]),
                issued_at: now + 600,
                expires_at: now + 1200,
                devices: vec![RecordDevice {
                    installation_id: device.installation_id(),
                    transport_public_key: device.transport_public_key(),
                    kind: DeviceKind::Desktop,
                }],
            },
            now * 1_000_000,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:47000")
            .await
            .unwrap();
        let services =
            services("ws://127.0.0.1:47000/records".into(), Some(clock.clone())).unwrap();
        let lookup = services.backend.as_ref().unwrap().lookup(account.peer_id());
        let reply = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            clock.advance_seconds(601).unwrap();
            socket
                .send(Message::Text(
                    json!({"packet":packet.as_bytes().to_vec()})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let _ = socket.next().await;
        };
        let (result, ()) = tokio::join!(lookup, reply);
        assert!(matches!(result, Ok(Lookup::Found(_))));
        assert_eq!(services.clock.unix_seconds(), now + 601);
    }
    #[test]
    fn configured_clock_is_shared_by_services_and_record_validation() {
        use super::*;
        use crate::account::clock::FakeClock;
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_800_000_000, 0));
        let services =
            services("ws://127.0.0.1:47000/records".into(), Some(clock.clone())).unwrap();
        assert!(Arc::ptr_eq(&services.clock, &clock));
        let normal = super::services("ws://127.0.0.1:47000/records".into(), None).unwrap();
        assert!(normal.clock.wake().is_none());
    }
    #[test]
    fn rejects_public_query_and_wrong_port() {
        for url in [
            "ws://example.com:47000/records",
            "ws://127.0.0.1:47001/records",
            "ws://127.0.0.1:47000/records?x",
            "ws://127.0.0.1:47000/",
        ] {
            assert!(super::services(url.into(), None).is_err());
        }
    }
}
