use std::{future::Future, pin::Pin};

use pkarr::{
    Client, PublicKey, ResolvePolicy, SignedPacket,
    errors::{PublishError, ResolveError},
};

use super::{PeerId, record::check_packet_size};
use crate::{CoreError, ErrorCode};

pub type RecordFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CoreError>> + Send + 'a>>;

#[derive(Debug)]
pub enum Lookup {
    Found(SignedPacket),
    /// Best-effort completed lookup, never proof of global absence.
    CompletedAbsent,
}

pub trait RecordBackend: Send + Sync {
    fn lookup(&self, peer: PeerId) -> RecordFuture<'_, Lookup>;
    fn publish<'a>(&'a self, packet: &'a SignedPacket) -> RecordFuture<'a, ()>;
}

pub struct PkarrBackend {
    client: Client,
}

impl PkarrBackend {
    /// Starts a DHT-only client. Construction may open a UDP socket.
    pub fn new() -> Result<Self, CoreError> {
        let client = Client::builder()
            .no_relays()
            .cache_size(0)
            .build()
            .map_err(|_| CoreError::new(ErrorCode::RecordUnavailable, "account DHT unavailable"))?;
        Ok(Self { client })
    }
}

impl RecordBackend for PkarrBackend {
    fn lookup(&self, peer: PeerId) -> RecordFuture<'_, Lookup> {
        Box::pin(async move {
            let key = PublicKey::try_from(&peer.0)
                .map_err(|_| CoreError::new(ErrorCode::AuthFailed, "invalid account public key"))?;
            resolve_result(self.client.resolve(&key, ResolvePolicy::NetworkOnly).await)
        })
    }

    fn publish<'a>(&'a self, packet: &'a SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            check_packet_size(packet)?;
            self.client
                .publish(packet)
                .await
                .map(|_| ())
                .map_err(publish_error)
        })
    }
}

fn resolve_result(result: Result<SignedPacket, ResolveError>) -> Result<Lookup, CoreError> {
    match result {
        Ok(packet) => Ok(Lookup::Found(packet)),
        Err(ResolveError::NotFound) => Ok(Lookup::CompletedAbsent),
        Err(_) => Err(CoreError::new(
            ErrorCode::RecordUnavailable,
            "account DHT lookup failed",
        )),
    }
}

fn publish_error(error: PublishError) -> CoreError {
    let code = match error {
        PublishError::NotMostRecent => ErrorCode::RecordConflict,
        _ => ErrorCode::RecordUnavailable,
    };
    CoreError::new(code, "account DHT publication failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkarr::errors::{PublishError, ResolveError};

    struct FakeBackend(std::sync::Mutex<Option<Result<SignedPacket, ResolveError>>>);

    impl RecordBackend for FakeBackend {
        fn lookup(&self, _peer: PeerId) -> RecordFuture<'_, Lookup> {
            let result = self.0.lock().unwrap().take().unwrap();
            Box::pin(async move { resolve_result(result) })
        }
        fn publish<'a>(&'a self, _packet: &'a SignedPacket) -> RecordFuture<'a, ()> {
            Box::pin(async {
                Err(CoreError::new(
                    ErrorCode::RecordUnavailable,
                    "fixture publication unavailable",
                ))
            })
        }
    }

    async fn inject(result: Result<SignedPacket, ResolveError>) -> Result<Lookup, CoreError> {
        let backend: Box<dyn RecordBackend> =
            Box::new(FakeBackend(std::sync::Mutex::new(Some(result))));
        backend.lookup(PeerId([0; 32])).await
    }

    #[tokio::test]
    async fn only_not_found_means_completed_absent() {
        assert!(matches!(
            inject(Err(ResolveError::NotFound)).await,
            Ok(Lookup::CompletedAbsent)
        ));
        for error in [
            ResolveError::NoDhtNodesQueried,
            ResolveError::NoResponses,
            ResolveError::NoUsableResponses,
            ResolveError::InvalidSignedPacket { seq: 1 },
            ResolveError::UnexpectedResponses,
        ] {
            assert_eq!(
                inject(Err(error)).await.unwrap_err().code,
                ErrorCode::RecordUnavailable
            );
        }
        let packet = SignedPacket::builder()
            .sign(&pkarr::Keypair::from_secret_key(&[7; 32]))
            .unwrap();
        let expected = packet.as_bytes().clone();
        let Lookup::Found(found) = inject(Ok(packet)).await.unwrap() else {
            panic!("found record lost");
        };
        assert_eq!(found.as_bytes(), &expected);
    }

    #[test]
    fn publish_failures_are_never_success() {
        assert_eq!(
            publish_error(PublishError::NotMostRecent).code,
            ErrorCode::RecordConflict
        );
        for error in [
            PublishError::NoDhtNodesQueried,
            PublishError::NoResponses,
            PublishError::UnexpectedResponses,
            PublishError::Rejected {
                code: 1,
                description: "fixture".into(),
            },
        ] {
            assert_eq!(publish_error(error).code, ErrorCode::RecordUnavailable);
        }
    }

    #[tokio::test]
    async fn network_only_never_falls_back_to_fresh_or_expired_cache() {
        use pkarr::{Cache, Timestamp};
        let kp = pkarr::Keypair::from_secret_key(&[7; 32]);
        for last_seen in [Timestamp::now(), Timestamp::from(0)] {
            let mut packet = SignedPacket::builder()
                .txt(
                    "_account".try_into().unwrap(),
                    "cached".try_into().unwrap(),
                    86400,
                )
                .timestamp(Timestamp::from(900))
                .sign(&kp)
                .unwrap();
            packet.set_last_seen(&last_seen);
            let cache = std::sync::Arc::new(pkarr::InMemoryCache::new(
                std::num::NonZeroUsize::new(10).unwrap(),
            ));
            cache.put(&pkarr::CacheKey::from(&kp.public_key()), &packet);
            let client = Client::builder()
                .no_default_network()
                .dht(|config| {
                    config.bootstrap = Some(vec![]);
                    config.bind_address = Some(std::net::Ipv4Addr::LOCALHOST);
                    config.port = Some(0);
                    config
                })
                .cache(cache)
                .build()
                .unwrap();
            let backend = PkarrBackend { client };
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                backend.lookup(PeerId(kp.public_key().to_bytes())),
            )
            .await
            .unwrap();
            assert_eq!(result.unwrap_err().code, ErrorCode::RecordUnavailable);
        }
    }
}
