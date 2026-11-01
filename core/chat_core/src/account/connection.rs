//! Public record acceptance and authenticated connection IO; storage remains in Manager.
use super::{
    PeerId, RecordFuture, RevocationNotifier, RevokeIntent,
    auth::{self, AuthGrant},
    clock::Clock,
    publication::{Recipient, SignedRecord},
    wire,
};
use crate::{CoreError, ErrorCode};
use iroh::endpoint::Connection;
use minicbor::{Decoder, Encoder, data::Type};
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteRecordView {
    pub peer: PeerId,
    pub accepted_generation: super::Generation,
    pub conflict_hash: Option<[u8; 32]>,
    pub conflict: Option<super::IdentityRecord>,
    /// Provenance of a current live proof, never authorization by itself.
    pub cached: bool,
}
#[derive(Clone)]
pub(crate) struct RemoteRecord {
    pub(crate) peer: PeerId,
    pub(crate) accepted: SignedRecord,
    pub(crate) floor: u64,
    pub(crate) conflict: Option<SignedRecord>,
}
struct RemoteRecordComparison {
    retained_before: bool,
    hash_equal: bool,
    seq_vs_accepted: Ordering,
    below_prior_floor: bool,
    generation_equal: bool,
    devices_equal: bool,
    time_valid: bool,
}
impl RemoteRecord {
    pub(crate) fn observe(&mut self, found: SignedRecord, now: u64) -> Result<(), CoreError> {
        let comparison = RemoteRecordComparison {
            retained_before: self.conflict.is_some(),
            hash_equal: found.hash == self.accepted.hash,
            seq_vs_accepted: found.seq.cmp(&self.accepted.seq),
            below_prior_floor: found.seq < self.floor,
            generation_equal: found.record.generation == self.accepted.record.generation,
            devices_equal: found.record.devices == self.accepted.record.devices,
            time_valid: found.current(now),
        };
        self.floor = self.floor.max(found.seq);
        if found.hash == self.accepted.hash {
            return if self.conflict.is_none() {
                Ok(())
            } else {
                self.conflict = Some(found);
                Err(conflict(comparison))
            };
        }
        if self.conflict.is_none()
            && found.current(now)
            && found.seq >= self.floor
            && found.seq > self.accepted.seq
            && found.record.generation == self.accepted.record.generation
            && found.record.devices == self.accepted.record.devices
        {
            self.accepted = found;
            Ok(())
        } else {
            self.conflict = Some(found);
            Err(conflict(comparison))
        }
    }
}
fn conflict(comparison: RemoteRecordComparison) -> CoreError {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "record_conflict remote_observation RemoteRecordComparison {{ retained_before: {}, hash_equal: {}, seq_vs_accepted: {:?}, below_prior_floor: {}, generation_equal: {}, devices_equal: {}, time_valid: {} }}",
        comparison.retained_before, comparison.hash_equal, comparison.seq_vs_accepted,
        comparison.below_prior_floor, comparison.generation_equal, comparison.devices_equal,
        comparison.time_valid,
    );
    CoreError::new(ErrorCode::RecordConflict, "Account record conflict")
}
pub(crate) fn encode_cache(cache: &[RemoteRecord]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.array(cache.len() as u64).unwrap();
    for entry in cache {
        e.array(4)
            .unwrap()
            .bytes(&entry.peer.0)
            .unwrap()
            .bytes(entry.accepted.packet.as_bytes())
            .unwrap()
            .u64(entry.floor)
            .unwrap();
        if let Some(conflict) = &entry.conflict {
            e.bytes(conflict.packet.as_bytes()).unwrap();
        } else {
            e.null().unwrap();
        }
    }
    e.into_writer()
}
pub(crate) fn decode_cache(input: &[u8], own: PeerId) -> Result<Vec<RemoteRecord>, CoreError> {
    if input.len() > 2_100_000 {
        return Err(wire::invalid());
    }
    let mut d = Decoder::new(input);
    let count = d
        .array()
        .map_err(|_| wire::invalid())?
        .ok_or_else(wire::invalid)?;
    if count > 1000 {
        return Err(wire::invalid());
    }
    let mut cache: Vec<RemoteRecord> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        wire::array(&mut d, 4)?;
        let peer = PeerId(wire::bytes(&mut d)?);
        if peer == own || cache.last().is_some_and(|last| last.peer >= peer) {
            return Err(wire::invalid());
        }
        let accepted = SignedRecord::decode(d.bytes().map_err(|_| wire::invalid())?, peer)?;
        let floor = wire::uint(&mut d)?;
        let conflict = if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
            d.null().map_err(|_| wire::invalid())?;
            None
        } else {
            Some(SignedRecord::decode(
                d.bytes().map_err(|_| wire::invalid())?,
                peer,
            )?)
        };
        if floor > i64::MAX as u64
            || accepted.seq > floor
            || conflict.as_ref().is_some_and(|r| r.seq > floor)
        {
            return Err(wire::invalid());
        }
        cache.push(RemoteRecord {
            peer,
            accepted,
            floor,
            conflict,
        });
    }
    wire::canonical(input, &encode_cache(&cache))?;
    Ok(cache)
}
pub(crate) struct Response {
    pub(crate) body:Vec<u8>,
    pub(crate) gate:Option<super::manager::messages::ResponseGate>,
}
pub(crate) enum Input {
    MessageAttempt(super::manager::messages::AttemptResult),
    MessageLookup(crate::message::types::MessageKey,u64,Result<super::Lookup,CoreError>),
    MessageProof(super::manager::messages::ProofResult),
    Incoming(Connection),
    Control {
        connection: Connection,
        body: Vec<u8>,
        reply: oneshot::Sender<Result<Response, CoreError>>,
    },
    Closed(usize),
}
#[derive(Clone)]
pub(crate) struct Session {
    pub(crate) conn: Connection,
    pub(crate) grant: AuthGrant,
    pub(crate) ready: bool,
    pub(crate) cached: bool,
}
#[derive(Clone, Default)]
pub(crate) struct Registry(
    pub(crate) Arc<Mutex<Vec<Session>>>,
    // Actor supplies only the durable expected set immediately before delivery.
    pub(crate) Arc<Mutex<BTreeMap<[u8; 16], Vec<Recipient>>>>,
);
impl Registry {
    pub(crate) fn capture(
        &self,
        intent: &RevokeIntent,
        now: u64,
    ) -> Result<Vec<Recipient>, CoreError> {
        let mut recipients: Vec<_> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| eligible(s, intent, now))
            .map(|s| (s.grant.installation, s.grant.transport))
            .collect();
        recipients.sort();
        recipients.dedup();
        if recipients.len() > 4 {
            return Err(wire::invalid());
        }
        Ok(recipients)
    }
    pub(crate) fn apply(
        &self,
        mut policy: impl FnMut(&AuthGrant) -> (bool, bool),
    ) -> Vec<AuthGrant> {
        let mut removed = vec![];
        self.0.lock().unwrap().retain_mut(|session| {
            let (ready, close) = policy(&session.grant);
            if session.ready && (!ready || close || session.conn.close_reason().is_some()) {
                removed.push(session.grant.clone());
            }
            // Restriction is sticky: only a fresh proof may grant again.
            session.ready &= ready;
            if close || session.conn.close_reason().is_some() {
                session.conn.close(0u32.into(), b"account restricted");
                false
            } else {
                true
            }
        });
        removed
    }
    pub(crate) fn sync_record(&self, record: &SignedRecord, cancel: CancellationToken) {
        let sessions: Vec<_> = self.0.lock().unwrap().iter().cloned().collect();
        let body = auth::envelope(2, record.packet.as_bytes());
        for session in sessions {
            let body = body.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let work = async {
                    let (mut send, mut recv) =
                        session.conn.open_bi().await.map_err(|_| wire::invalid())?;
                    auth::write_frame(&mut send, &body).await?;
                    send.finish().map_err(|_| wire::invalid())?;
                    let _ = auth::read_frame(&mut recv, 1100).await?;
                    Ok::<(), CoreError>(())
                };
                tokio::select! {biased;_=cancel.cancelled()=>{},_=tokio::time::timeout(Duration::from_secs(5),work)=>{}}
            });
        }
    }
}
fn eligible(s: &Session, intent: &RevokeIntent, now: u64) -> bool {
    s.ready
        && s.grant.peer == intent.peer_id
        && s.grant.installation != intent.target_installation
        && s.grant.transport != intent.target_transport
        && s.grant.generation == intent.generation
        && s.grant.record_hash == intent.base_hash
        && now < s.grant.expires_at
        && s.conn.close_reason().is_none()
}
pub(crate) struct Notifier {
    pub(crate) registry: Registry,
    pub(crate) clock: Arc<dyn Clock>,
}
impl RevocationNotifier for Notifier {
    fn notify<'a>(&'a self, intent: &'a RevokeIntent) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let expected = self
                .registry
                .1
                .lock()
                .unwrap()
                .get(&intent.nonce)
                .cloned()
                .ok_or_else(|| {
                    CoreError::new(
                        ErrorCode::RecordUnavailable,
                        "Recipients were not durably captured",
                    )
                })?;
            let recipients = {
                let sessions = self.registry.0.lock().unwrap();
                expected
                    .iter()
                    .map(|recipient| {
                        sessions
                            .iter()
                            .find(|s| {
                                (s.grant.installation, s.grant.transport) == *recipient
                                    && eligible(s, intent, self.clock.unix_seconds())
                            })
                            .cloned()
                            .ok_or_else(|| {
                                CoreError::new(
                                    ErrorCode::RecordUnavailable,
                                    "Expected account receipt unavailable",
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            for session in recipients {
                let expected = intent.to_frame();
                let work = async {
                    let (mut send, mut recv) =
                        session.conn.open_bi().await.map_err(|_| wire::invalid())?;
                    send.write_all(&expected)
                        .await
                        .map_err(|_| wire::invalid())?;
                    let response = auth::read_frame(&mut recv, 65536).await?;
                    if response != expected[4..]
                        || self.clock.unix_seconds() >= session.grant.expires_at
                        || session.conn.close_reason().is_some()
                        || !self.registry.0.lock().unwrap().iter().any(|s| {
                            s.conn.stable_id() == session.conn.stable_id()
                                && eligible(s, intent, self.clock.unix_seconds())
                        })
                    {
                        return Err(wire::invalid());
                    }
                    send.finish().map_err(|_| wire::invalid())?;
                    Ok(())
                };
                tokio::time::timeout(Duration::from_secs(5), work)
                    .await
                    .map_err(|_| {
                        CoreError::new(ErrorCode::NetworkTimeout, "Account receipt timeout")
                    })??;
            }
            Ok(())
        })
    }
}
pub(crate) fn reader(conn: Connection, input: mpsc::Sender<Input>, cancel: CancellationToken) {
    tokio::spawn(async move {
        loop {
            let accepted =
                tokio::select! {biased;_=cancel.cancelled()=>break,result=conn.accept_bi()=>result};
            let Ok((mut send, mut recv)) = accepted else {
                break;
            };
            let work = async {
                let body = auth::read_frame(&mut recv, 65536).await?;
                let (reply, result) = oneshot::channel();
                input
                    .try_send(Input::Control {
                        connection: conn.clone(),
                        body,
                        reply,
                    })
                    .map_err(|_| wire::invalid())?;
                let response = result.await.map_err(|_| wire::invalid())??;
                #[cfg(all(feature="test-hooks",debug_assertions))]
                let mut response=response;
                #[cfg(all(feature="test-hooks",debug_assertions))]
                if let Some(gate)=&mut response.gate {gate.checkpoint().await;}
                if response.gate.as_ref().is_some_and(|gate|!gate.permits()) {return Err(wire::invalid());}
                #[cfg(all(feature="test-hooks",debug_assertions))]
                if let Some(gate)=&response.gate {gate.write_started();}
                auth::write_frame(&mut send, &response.body).await?;
                send.finish().map_err(|_| wire::invalid())?;
                Ok::<(), CoreError>(())
            };
            let result = tokio::select! {biased;_=cancel.cancelled()=>break,result=tokio::time::timeout(Duration::from_secs(10),work)=>result};
            if !matches!(result, Ok(Ok(()))) {
                break;
            }
        }
        conn.close(0u32.into(), b"account stream ended");
        let _ = input.try_send(Input::Closed(conn.stable_id()));
    });
}
pub(crate) struct Runtime {
    pub(crate) cached_lookups: std::collections::BTreeSet<PeerId>,
    pub(crate) registry: Registry,
    pub(crate) input: mpsc::Receiver<Input>,
    pub(crate) send: mpsc::Sender<Input>,
    pub(crate) cancel: CancellationToken,
}
impl Default for Runtime {
    fn default() -> Self {
        let (send, input) = mpsc::channel(32);
        Self {
            cached_lookups: Default::default(),
            registry: Registry::default(),
            input,
            send,
            cancel: CancellationToken::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        DeviceKind, Generation, IdentityRecord, RecordDevice, Secret32, derive_account,
        derive_device, record_packet,
    };

    const NOW: u64 = 1_800_000_000;
    const SEQ: u64 = NOW * 1_000_000;
    const CHILD: &str = "CHAT_CORE_RECORD_CONFLICT_FIXTURE";

    fn signed(seq: u64, generation: u8, device: u8, issued: u64, expires: u64) -> SignedRecord {
        let keys = derive_account(&Secret32::from(*b"record-conflict-private-sentinel")).unwrap();
        let device = derive_device(Secret32::from([device; 32])).unwrap();
        let packet = record_packet(
            &keys,
            &IdentityRecord {
                generation: Generation([generation; 16]),
                issued_at: issued,
                expires_at: expires,
                devices: vec![RecordDevice {
                    installation_id: device.installation_id(),
                    transport_public_key: device.transport_public_key(),
                    kind: DeviceKind::Desktop,
                }],
            },
            seq,
        ).unwrap();
        SignedRecord::new(packet, keys.peer_id()).unwrap()
    }

    // Removing failure diagnostics, capturing retained state after mutation, or
    // accepting a rollback must fail this real-observer stderr/state regression.
    #[test]
    fn remote_record_failure_diagnostic_preserves_observation_policy() {
        if let Ok(case) = std::env::var(CHILD) {
            let retained = std::env::var("CHAT_CORE_RECORD_CONFLICT_RETAINED").unwrap() == "true";
            let accepted = signed(SEQ, 1, 7, NOW, NOW + 600);
            let mut remote = RemoteRecord {
                peer: PeerId(accepted.packet.as_bytes()[..32].try_into().unwrap()),
                accepted: accepted.clone(),
                floor: if case == "prior_floor" { SEQ + 2 } else { SEQ },
                conflict: retained.then(|| signed(SEQ, 2, 7, NOW, NOW + 600)),
            };
            let found = match case.as_str() {
                "exact" => accepted.clone(),
                "renewal" | "prior_floor" => signed(SEQ + 1, 1, 7, NOW, NOW + 601),
                "lower" => signed(SEQ - 1, 1, 7, NOW, NOW + 600),
                "equal" => signed(SEQ, 1, 7, NOW, NOW + 601),
                "generation" => signed(SEQ + 1, 2, 7, NOW, NOW + 600),
                "devices" => signed(SEQ + 1, 1, 8, NOW, NOW + 600),
                "future" => signed(SEQ + 600_000_000, 1, 7, NOW + 600, NOW + 1200),
                _ => panic!("unknown child fixture"),
            };
            if std::env::var_os("CHAT_CORE_RECORD_CONFLICT_CLOSED_STDERR").is_some() {
                use std::io::Read;
                // Parent closes the pipe reader before releasing observation.
                std::io::stdin().read_exact(&mut [0]).unwrap();
            }
            let result = remote.observe(found.clone(), NOW);
            let succeeds = !retained && matches!(case.as_str(), "exact" | "renewal");
            if succeeds {
                result.unwrap();
                assert!(remote.conflict.is_none());
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, ErrorCode::RecordConflict);
                assert_eq!(error.detail, "Account record conflict");
                assert_eq!(remote.conflict.as_ref().unwrap().packet.as_bytes(), found.packet.as_bytes());
            }
            let expected_accepted = if succeeds && case == "renewal" { &found } else { &accepted };
            assert_eq!(remote.accepted.packet.as_bytes(), expected_accepted.packet.as_bytes());
            let expected_floor = match case.as_str() {
                "prior_floor" => SEQ + 2,
                "renewal" | "generation" | "devices" => SEQ + 1,
                "future" => SEQ + 600_000_000,
                _ => SEQ,
            };
            assert_eq!(remote.floor, expected_floor);
            // A retained conflict must also survive durable cache encoding.
            let restored = decode_cache(&encode_cache(&[remote.clone()]), PeerId([0; 32])).unwrap();
            assert_eq!(encode_cache(&restored), encode_cache(&[remote]));
            return;
        }

        // These literal facts are hand-derived from the signed fixtures, before
        // observation mutates the floor or replaces retained conflict evidence.
        let cases = [
            ("exact", "true", "Equal", "false", "true", "true", "true"),
            ("renewal", "false", "Greater", "false", "true", "true", "true"),
            ("lower", "false", "Less", "true", "true", "true", "true"),
            ("equal", "false", "Equal", "false", "true", "true", "true"),
            ("generation", "false", "Greater", "false", "false", "true", "true"),
            ("devices", "false", "Greater", "false", "true", "false", "true"),
            ("future", "false", "Greater", "false", "true", "true", "false"),
            ("prior_floor", "false", "Greater", "true", "true", "true", "true"),
        ];
        for retained in [false, true] {
            for (case, hash, seq, below, generation, devices, time) in cases {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "account::connection::tests::remote_record_failure_diagnostic_preserves_observation_policy", "--nocapture"])
                    .env(CHILD, case)
                    .env("CHAT_CORE_RECORD_CONFLICT_RETAINED", retained.to_string())
                    .output().unwrap();
                assert!(output.status.success(), "child {case}/{retained} failed: {}", String::from_utf8_lossy(&output.stderr));
                let stderr = String::from_utf8(output.stderr).unwrap().replace("\r\n", "\n");
                let expected = if !retained && matches!(case, "exact" | "renewal") {
                    String::new()
                } else {
                    format!("record_conflict remote_observation RemoteRecordComparison {{ retained_before: {retained}, hash_equal: {hash}, seq_vs_accepted: {seq}, below_prior_floor: {below}, generation_equal: {generation}, devices_equal: {devices}, time_valid: {time} }}\n")
                };
                assert!(!stderr.contains("record-conflict-private-sentinel"));
                // Exact stderr equality permits no packet/key/hash/number or
                // other fixture material alongside the approved comparison facts.
                assert_eq!(stderr, expected, "diagnostic for {case}/{retained}");
            }
        }
    }

    #[test]
    fn remote_record_diagnostic_ignores_closed_stderr() {
        use std::io::Write;
        use std::process::Stdio;

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "account::connection::tests::remote_record_failure_diagnostic_preserves_observation_policy", "--nocapture"])
            .env(CHILD, "lower")
            .env("CHAT_CORE_RECORD_CONFLICT_RETAINED", "false")
            .env("CHAT_CORE_RECORD_CONFLICT_CLOSED_STDERR", "true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn().unwrap();
        drop(child.stderr.take().unwrap());
        child.stdin.take().unwrap().write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "closed stderr changed the observer outcome: {}", String::from_utf8_lossy(&output.stdout));
    }
}
