use std::{future::Future, io::Write, pin::Pin};

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
            let _ = writeln!(std::io::stderr().lock(), "account_dht lookup started");
            resolve_result(self.client.resolve(&key, ResolvePolicy::NetworkOnly).await)
        })
    }

    fn publish<'a>(&'a self, packet: &'a SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            check_packet_size(packet)?;
            let _ = writeln!(std::io::stderr().lock(), "account_dht publish started");
            let result = self.client.publish(packet).await;
            log_publish(&result, &mut std::io::stderr().lock());
            result.map(|_| ()).map_err(publish_error)
        })
    }
}

fn resolve_result(result: Result<SignedPacket, ResolveError>) -> Result<Lookup, CoreError> {
    log_lookup(&result, &mut std::io::stderr().lock());
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

fn log_publish(result: &Result<pkarr::StoredNodeCount, PublishError>, output: &mut impl Write) {
    let outcome = match result {
        Ok(count) => {
            let _ = writeln!(
                output,
                "account_dht publish outcome=stored stored_nodes={count}"
            );
            return;
        }
        Err(PublishError::NoDhtNodesQueried) => "no_nodes_queried",
        Err(PublishError::NoResponses) => "no_responses",
        Err(PublishError::UnexpectedResponses) => "unexpected_responses",
        Err(PublishError::NotMostRecent) => "not_most_recent",
        Err(PublishError::Rejected { .. }) => "rejected",
    };
    let _ = writeln!(output, "account_dht publish outcome={outcome}");
}

fn log_lookup(result: &Result<SignedPacket, ResolveError>, output: &mut impl Write) {
    let outcome = match result {
        Ok(_) => "found",
        Err(ResolveError::NotFound) => "not_found",
        Err(ResolveError::NoDhtNodesQueried) => "no_nodes_queried",
        Err(ResolveError::NoResponses) => "no_responses",
        Err(ResolveError::NoUsableResponses) => "no_usable_responses",
        Err(ResolveError::UnexpectedResponses) => "unexpected_responses",
        Err(ResolveError::InvalidSignedPacket { .. }) => "invalid_signed_packet",
    };
    let _ = writeln!(output, "account_dht lookup outcome={outcome}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkarr::errors::{PublishError, ResolveError};

    #[test]
    fn backend_diagnostics_preserve_store_count_and_redact_publish_errors() {
        for count in [0, 1, 20] {
            let mut output = Vec::new();
            log_publish(&Ok(count), &mut output);
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("account_dht publish outcome=stored stored_nodes={count}\n")
            );
        }
        for (error, expected) in [
            (PublishError::NoDhtNodesQueried, "no_nodes_queried"),
            (PublishError::NoResponses, "no_responses"),
            (PublishError::UnexpectedResponses, "unexpected_responses"),
            (PublishError::NotMostRecent, "not_most_recent"),
            (
                PublishError::Rejected {
                    code: 7634521,
                    description: "private-rejection-marker".into(),
                },
                "rejected",
            ),
        ] {
            let mut output = Vec::new();
            log_publish(&Err(error), &mut output);
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("account_dht publish outcome={expected}\n")
            );
        }
    }

    #[test]
    fn backend_diagnostics_redact_lookup_packets_and_error_payloads() {
        let packet = SignedPacket::builder()
            .txt(
                "_account".try_into().unwrap(),
                "private-packet-marker".try_into().unwrap(),
                30,
            )
            .sign(&pkarr::Keypair::from_secret_key(&[7; 32]))
            .unwrap();
        let mut output = Vec::new();
        log_lookup(&Ok(packet), &mut output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "account_dht lookup outcome=found\n"
        );
        for (error, expected) in [
            (ResolveError::NotFound, "not_found"),
            (ResolveError::NoDhtNodesQueried, "no_nodes_queried"),
            (ResolveError::NoResponses, "no_responses"),
            (ResolveError::NoUsableResponses, "no_usable_responses"),
            (ResolveError::UnexpectedResponses, "unexpected_responses"),
            (
                ResolveError::InvalidSignedPacket { seq: 7634521 },
                "invalid_signed_packet",
            ),
        ] {
            let mut output = Vec::new();
            log_lookup(&Err(error), &mut output);
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("account_dht lookup outcome={expected}\n")
            );
        }
    }

    #[test]
    fn backend_diagnostics_ignore_output_failure() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        log_publish(&Ok(1), &mut Closed);
        log_publish(&Err(PublishError::NoResponses), &mut Closed);
        log_lookup(&Err(ResolveError::NoResponses), &mut Closed);
    }

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
