use super::{Generation, InstallationCert, PeerId, verify_cert, wire};
use crate::CoreError;
use minicbor::{Decoder, Encoder};

pub(crate) struct AuthHello {
    pub(crate) peer: PeerId,
    pub(crate) generation: Generation,
    pub(crate) cert: InstallationCert,
    pub(crate) nonce: [u8; 32],
}
impl AuthHello {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(5)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.peer.0)
            .unwrap()
            .bytes(&self.generation.0)
            .unwrap();
        let mut bytes = e.into_writer();
        bytes.extend(self.cert.to_bytes());
        Encoder::new(&mut bytes).bytes(&self.nonce).unwrap();
        bytes
    }
    pub(crate) fn decode(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() > 16384 {
            return Err(wire::invalid());
        }
        let mut d = Decoder::new(input);
        wire::array(&mut d, 5)?;
        if wire::uint(&mut d)? != 1 {
            return Err(wire::invalid());
        }
        let peer = PeerId(wire::bytes(&mut d)?);
        let generation = Generation(wire::bytes(&mut d)?);
        let start = d.position();
        d.skip().map_err(|_| wire::invalid())?;
        let cert = InstallationCert::from_bytes(&input[start..d.position()])?;
        let result = Self {
            peer,
            generation,
            cert,
            nonce: wire::bytes(&mut d)?,
        };
        wire::canonical(input, &result.encode())?;
        verify_cert(&result.cert, peer)?;
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{DeviceKind, Secret32, derive_account, derive_device, sign_cert};

    #[test]
    fn hello_rejects_noncanonical_trailing_and_wrong_peer_certificate() {
        let keys = derive_account(&Secret32::from([7; 32])).unwrap();
        let device = derive_device(Secret32::from([8; 32])).unwrap();
        let hello = AuthHello {
            peer: keys.peer_id(),
            generation: Generation([9; 16]),
            cert: sign_cert(&keys, &device, DeviceKind::Desktop, 1000).unwrap(),
            nonce: [10; 32],
        };
        let bytes = hello.encode();
        assert_eq!(AuthHello::decode(&bytes).unwrap().peer, keys.peer_id());
        let mut bad = bytes.clone();
        bad.push(0);
        assert!(AuthHello::decode(&bad).is_err());
        let mut bad = bytes;
        bad.splice(1..2, [0x18, 1]);
        assert!(AuthHello::decode(&bad).is_err());
        let mut bad = hello;
        bad.peer = PeerId([0; 32]);
        assert!(AuthHello::decode(&bad.encode()).is_err());
    }

    /// Ruling R35: a renewal of my own record (same generation and devices, another seq) is the
    /// same authority, on the newer packet; an equal seq with another packet, another
    /// generation, other devices, and another account's differing record are refused.
    #[test]
    fn grant_record_takes_only_a_renewal_of_my_own_record() {
        use crate::account::{
            IdentityRecord, PublishIntent, RecordDevice, publication::SignedRecord, record_packet,
            recovery,
        };
        let local =
            recovery::local_account(derive_account(&Secret32::from([1; 32])).unwrap(), PublishIntent::LocalCreate, 1)
                .unwrap();
        let other = derive_account(&Secret32::from([2; 32])).unwrap();
        let device = |seed: u8| {
            let d = derive_device(Secret32::from([seed; 32])).unwrap();
            RecordDevice {
                installation_id: d.installation_id(),
                transport_public_key: d.transport_public_key(),
                kind: DeviceKind::Desktop,
            }
        };
        let record = |generation: u8, seeds: &[u8], issued: u64| IdentityRecord {
            generation: Generation([generation; 16]),
            issued_at: issued,
            expires_at: issued + 600,
            devices: seeds.iter().map(|s| device(*s)).collect(),
        };
        let sign = |keys: &crate::account::AccountKeys, r: &IdentityRecord, seq: u64| {
            SignedRecord::new(record_packet(keys, r, seq).unwrap(), keys.peer_id()).unwrap()
        };
        let me = local.keys.peer_id();
        let held = sign(&local.keys, &record(1, &[7, 8], 1000), 1_000_000);
        let newer = sign(&local.keys, &record(1, &[7, 8], 1001), 1_000_001);
        let older = sign(&local.keys, &record(1, &[7, 8], 999), 999_999);
        let pick = |exchanged: &SignedRecord| grant_record(&local, me, &held, exchanged).map(|r| r.hash);
        assert_eq!(pick(&held).unwrap(), held.hash);
        assert_eq!(pick(&newer).unwrap(), newer.hash);
        assert_eq!(pick(&older).unwrap(), held.hash);
        let refused = |result: Result<[u8; 32], CoreError>| {
            result.is_err_and(|e| e.code == crate::ErrorCode::AuthFailed && e.detail == AuthCheck::Record.detail())
        };
        for (name, exchanged) in [
            ("equal seq", sign(&local.keys, &record(1, &[7, 8], 1002), 1_000_000)),
            ("generation", sign(&local.keys, &record(2, &[7, 8], 1001), 1_000_001)),
            ("devices", sign(&local.keys, &record(1, &[7], 1001), 1_000_001)),
        ] {
            assert!(refused(pick(&exchanged)), "{name}");
        }
        let theirs = sign(&other, &record(1, &[7, 8], 1000), 1_000_000);
        let theirs_newer = sign(&other, &record(1, &[7, 8], 1001), 1_000_001);
        assert!(refused(
            grant_record(&local, other.peer_id(), &theirs, &theirs_newer).map(|r| r.hash)
        ));
    }
}

use super::{
    InstallationId, RevokeIntent, clock::Clock, publication::SignedRecord,
    storage::schema::LocalAccount,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use iroh::endpoint::{Connection, RecvStream, SendStream, Side};
pub(crate) const ALPN: &[u8] = b"chatproject/account/1";
pub(crate) type LocalIdentity = LocalAccount;

/// Which check refused an account authentication, as the `AuthFailed` detail (fixed texts:
/// nothing of the peer, its record or keys). e2e m7 notes it (`check_of`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthCheck {
    /// ALPN, the expected account, or the certificate's transport key against the connection.
    Identity,
    /// The installation certificate's signature.
    Certificate,
    /// The record the peer showed is not the one this device holds for its account.
    Record,
    /// The peer's installation is not in that record.
    Device,
    /// The installation proof's signature.
    Proof,
    /// The grant is not valid (expired record, removed device).
    Grant,
}
impl AuthCheck {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    const ALL: [Self; 6] = [
        Self::Identity,
        Self::Certificate,
        Self::Record,
        Self::Device,
        Self::Proof,
        Self::Grant,
    ];
    fn detail(self) -> &'static str {
        match self {
            Self::Identity => "account auth refused: identity",
            Self::Certificate => "account auth refused: certificate",
            Self::Record => "account auth refused: record",
            Self::Device => "account auth refused: device",
            Self::Proof => "account auth refused: proof",
            Self::Grant => "account auth refused: grant",
        }
    }
    fn refuse(self) -> CoreError {
        CoreError::new(crate::ErrorCode::AuthFailed, self.detail())
    }
    /// A fixed short name (e2e facts).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Certificate => "certificate",
            Self::Record => "record",
            Self::Device => "device",
            Self::Proof => "proof",
            Self::Grant => "grant",
        }
    }
    /// The check an `AuthFailed` error of `finish` names; `None` for any other error, including
    /// the unnamed `AuthFailed` ones (an undecodable or future record, I/O): e2e notes `none`.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn of(error: &CoreError) -> Option<Self> {
        Self::ALL.into_iter().find(|c| error.detail == c.detail())
    }
}
#[derive(Clone)]
pub(crate) struct AuthGrant {
    pub(crate) peer: PeerId,
    pub(crate) installation: InstallationId,
    pub(crate) transport: [u8; 32],
    pub(crate) record_hash: [u8; 32],
    pub(crate) generation: Generation,
    pub(crate) expires_at: u64,
}
impl AuthGrant {
    pub(crate) fn valid(
        &self,
        record: &SignedRecord,
        tombstones: &[RevokeIntent],
        now: u64,
    ) -> bool {
        self.record_hash == record.hash
            && self.generation == record.record.generation
            && now < self.expires_at
            && record.current(now)
            && !record.record.is_expired(now)
            && record.record.devices.iter().any(|d| {
                d.installation_id == self.installation && d.transport_public_key == self.transport
            })
            && !tombstones.iter().any(|t| {
                t.target_installation == self.installation || t.target_transport == self.transport
            })
    }
}
pub(crate) fn envelope(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.array(3).unwrap().u8(1).unwrap().u8(kind).unwrap();
    if kind == 0 {
        let mut bytes = e.into_writer();
        bytes.extend_from_slice(payload);
        bytes
    } else {
        e.bytes(payload).unwrap();
        e.into_writer()
    }
}
pub(crate) fn payload(input: &[u8], kind: u8) -> Result<Vec<u8>, CoreError> {
    let mut d = Decoder::new(input);
    wire::array(&mut d, 3)?;
    if wire::uint(&mut d)? != 1 || wire::uint(&mut d)? != u64::from(kind) {
        return Err(wire::invalid());
    }
    let value = if kind == 0 {
        let start = d.position();
        d.skip().map_err(|_| wire::invalid())?;
        input[start..d.position()].to_vec()
    } else {
        d.bytes().map_err(|_| wire::invalid())?.to_vec()
    };
    wire::canonical(input, &envelope(kind, &value))?;
    Ok(value)
}
pub(crate) async fn read_frame(recv: &mut RecvStream, limit: usize) -> Result<Vec<u8>, CoreError> {
    read_auth_frame(recv, limit, false).await
}
fn io_error(acquisition: bool) -> CoreError {
    if acquisition {
        CoreError::new(
            crate::ErrorCode::TransportFailed,
            "account_transport_failed",
        )
    } else {
        wire::invalid()
    }
}
async fn read_auth_frame(
    recv: &mut RecvStream,
    limit: usize,
    acquisition: bool,
) -> Result<Vec<u8>, CoreError> {
    let mut length = [0; 4];
    recv.read_exact(&mut length)
        .await
        .map_err(|_| io_error(acquisition))?;
    let length = u32::from_be_bytes(length) as usize;
    if length > limit || length > 65536 {
        return Err(wire::invalid());
    }
    let mut bytes = vec![0; length];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|_| io_error(acquisition))?;
    Ok(bytes)
}
pub(crate) async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<(), CoreError> {
    if bytes.len() > 65536 {
        return Err(wire::invalid());
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(|_| wire::invalid())?;
    send.write_all(bytes).await.map_err(|_| wire::invalid())
}
pub(crate) struct Preface {
    pub(crate) send: SendStream,
    pub(crate) recv: RecvStream,
    pub(crate) hello: AuthHello,
}
pub(crate) async fn receive_preface(conn: &Connection) -> Result<Preface, CoreError> {
    conn.set_max_concurrent_uni_streams(0u32.into());
    conn.set_max_concurrent_bi_streams(4u32.into());
    let (send, mut recv) = conn.accept_bi().await.map_err(|_| wire::invalid())?;
    let hello = AuthHello::decode(&payload(&read_frame(&mut recv, 16384).await?, 0)?)?;
    Ok(Preface { send, recv, hello })
}
fn hello(local: &LocalIdentity) -> Result<AuthHello, CoreError> {
    let record = local.records.accepted.as_ref().ok_or_else(wire::invalid)?;
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|_| wire::invalid())?;
    Ok(AuthHello {
        peer: local.keys.peer_id(),
        generation: record.record.generation,
        cert: local.cert.clone(),
        nonce,
    })
}
fn context(dialer: &AuthHello, acceptor: &AuthHello) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.array(3).unwrap().u8(1).unwrap();
    for hello in [dialer, acceptor] {
        e.array(6)
            .unwrap()
            .bytes(&hello.peer.0)
            .unwrap()
            .bytes(&hello.cert.transport_public_key)
            .unwrap()
            .bytes(&hello.cert.installation_id.0)
            .unwrap()
            .bytes(&hello.cert.hash())
            .unwrap()
            .bytes(&hello.generation.0)
            .unwrap()
            .bytes(&hello.nonce)
            .unwrap();
    }
    e.into_writer()
}
fn proof_message(
    conn: &Connection,
    context: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, CoreError> {
    let mut exporter = zeroize::Zeroizing::new([0; 32]);
    conn.export_keying_material(&mut *exporter, b"ChatProject/account-exporter/v1", context)
        .map_err(|_| wire::invalid())?;
    let mut message = zeroize::Zeroizing::new(b"ChatProject/account-auth/v1".to_vec());
    message.extend_from_slice(context);
    message.extend_from_slice(&*exporter);
    Ok(message)
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn authenticate(
    conn: Connection,
    expected: PeerId,
    local: &LocalIdentity,
    record: &SignedRecord,
    clock: &dyn Clock,
    evidence: &mut Option<SignedRecord>,
    gate: Option<&mut super::manager::messages::ProofGate>,
) -> Result<AuthGrant, CoreError> {
    check_gate(gate.as_deref())?;
    let own = hello(local)?;
    conn.set_max_concurrent_uni_streams(0u32.into());
    conn.set_max_concurrent_bi_streams(4u32.into());
    let (mut send, mut recv) = conn.open_bi().await.map_err(|_| io_error(gate.is_some()))?;
    check_gate(gate.as_deref())?;
    write_auth_frame(&mut send, &envelope(0, &own.encode()), gate.as_deref()).await?;
    let remote = AuthHello::decode(&payload(
        &read_auth_frame(&mut recv, 16384, gate.is_some()).await?,
        0,
    )?)?;
    check_gate(gate.as_deref())?;
    finish(
        &conn,
        expected,
        local,
        record,
        clock,
        evidence,
        own,
        Preface {
            send,
            recv,
            hello: remote,
        },
        gate,
    )
    .await
}
pub(crate) async fn authenticate_incoming(
    conn: &Connection,
    local: &LocalIdentity,
    record: &SignedRecord,
    clock: &dyn Clock,
    mut preface: Preface,
    evidence: &mut Option<SignedRecord>,
) -> Result<AuthGrant, CoreError> {
    let own = hello(local)?;
    write_frame(&mut preface.send, &envelope(0, &own.encode())).await?;
    finish(
        conn,
        preface.hello.peer,
        local,
        record,
        clock,
        evidence,
        own,
        preface,
        None,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn finish(
    conn: &Connection,
    expected: PeerId,
    local: &LocalIdentity,
    record: &SignedRecord,
    clock: &dyn Clock,
    evidence: &mut Option<SignedRecord>,
    own: AuthHello,
    preface: Preface,
    mut gate: Option<&mut super::manager::messages::ProofGate>,
) -> Result<AuthGrant, CoreError> {
    let Preface {
        mut send,
        mut recv,
        hello: remote,
    } = preface;
    if conn.alpn() != ALPN
        || remote.peer != expected
        || remote.cert.transport_public_key != *conn.remote_id().as_bytes()
    {
        return Err(AuthCheck::Identity.refuse());
    }
    verify_cert(&remote.cert, expected).map_err(|_| AuthCheck::Certificate.refuse())?;
    let own_record = local.records.accepted.as_ref().ok_or_else(wire::invalid)?;
    write_auth_frame(
        &mut send,
        &envelope(2, own_record.packet.as_bytes()),
        gate.as_deref(),
    )
    .await?;
    let packet = payload(&read_auth_frame(&mut recv, 1100, gate.is_some()).await?, 2)?;
    check_gate(gate.as_deref())?;
    let exchanged = SignedRecord::decode(&packet, expected)?;
    // Identity-signature/canonical evidence survives a denied installation proof.
    *evidence = Some(exchanged.clone());
    super::parse_record(&exchanged.packet, expected, clock.unix_seconds())?;
    let record = grant_record(local, expected, record, &exchanged)?;
    if remote.generation != record.record.generation {
        return Err(AuthCheck::Record.refuse());
    }
    if !record.record.devices.iter().any(|d| {
        d.installation_id == remote.cert.installation_id
            && d.transport_public_key == remote.cert.transport_public_key
            && d.kind == remote.cert.kind
    }) {
        return Err(AuthCheck::Device.refuse());
    }
    let grant = AuthGrant {
        peer: expected,
        installation: remote.cert.installation_id,
        transport: remote.cert.transport_public_key,
        record_hash: record.hash,
        generation: record.record.generation,
        expires_at: record.record.expires_at,
    };
    if !grant.valid(record, &[], clock.unix_seconds()) {
        return Err(AuthCheck::Grant.refuse());
    }
    let context = if conn.side() == Side::Client {
        context(&own, &remote)
    } else {
        context(&remote, &own)
    };
    let message = proof_message(conn, &context)?;
    if let Some(gate) = gate.as_mut() {
        gate.before_signature().await;
    }
    check_gate(gate.as_deref())?;
    let sig = SigningKey::from_bytes(local.device.installation().as_bytes())
        .sign(&message)
        .to_bytes();
    check_gate(gate.as_deref())?;
    if let Some(gate) = gate.as_ref() {
        gate.signature_write();
    }
    write_auth_frame(&mut send, &envelope(1, &sig), gate.as_deref()).await?;
    let sig: [u8; 64] = payload(&read_auth_frame(&mut recv, 80, gate.is_some()).await?, 1)?
        .try_into()
        .map_err(|_| wire::invalid())?;
    check_gate(gate.as_deref())?;
    VerifyingKey::from_bytes(&remote.cert.installation_id.0)
        .map_err(|_| wire::invalid())?
        .verify_strict(&message, &Signature::from_bytes(&sig))
        .map_err(|_| AuthCheck::Proof.refuse())?;
    if !grant.valid(record, &[], clock.unix_seconds()) {
        return Err(AuthCheck::Grant.refuse());
    }
    send.finish().map_err(|_| io_error(gate.is_some()))?;
    Ok(grant)
}

/// The record a session is granted on: the one this device holds for the account, which the
/// peer showed too. Ruling R35 (e2e m7 run 43728038578): my other device may hold a renewal of
/// my record this one has not taken yet, or the reverse (each renews on its own, R28/R30). Of
/// my own account, a record of the same shape (`RecordState::same_shape`) at another seq is the
/// same authority: the newer of the two, which the caller takes from the evidence (or keeps)
/// right after. An equal seq with another packet is a same-seq conflict (M3 5.2), refused; so is
/// any other difference, and any difference for another account.
fn grant_record<'a>(
    local: &LocalIdentity,
    expected: PeerId,
    record: &'a SignedRecord,
    exchanged: &'a SignedRecord,
) -> Result<&'a SignedRecord, CoreError> {
    if exchanged.hash == record.hash {
        return Ok(record);
    }
    if expected != local.keys.peer_id() || !local.records.same_shape(exchanged, record) {
        return Err(AuthCheck::Record.refuse());
    }
    match exchanged.seq.cmp(&record.seq) {
        std::cmp::Ordering::Greater => Ok(exchanged),
        std::cmp::Ordering::Less => Ok(record),
        std::cmp::Ordering::Equal => Err(AuthCheck::Record.refuse()),
    }
}

fn check_gate(gate: Option<&super::manager::messages::ProofGate>) -> Result<(), CoreError> {
    if let Some(gate) = gate {
        gate.check()?;
    }
    Ok(())
}
// Manual/incoming auth keeps its original IO; only an off-actor acquisition needs
// a current proof gate at both the length and payload writes of each frame.
async fn write_auth_frame(
    send: &mut SendStream,
    bytes: &[u8],
    gate: Option<&super::manager::messages::ProofGate>,
) -> Result<(), CoreError> {
    let Some(gate) = gate else {
        return write_frame(send, bytes).await;
    };
    if bytes.len() > 65536 {
        return Err(wire::invalid());
    }
    gate.check()?;
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(|_| io_error(true))?;
    gate.check()?;
    send.write_all(bytes).await.map_err(|_| io_error(true))?;
    gate.check()
}
