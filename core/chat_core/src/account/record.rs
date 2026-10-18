use data_encoding::BASE64URL_NOPAD;
use ed25519_dalek::{Signature, VerifyingKey};
use minicbor::{Decoder, Encoder};
use pkarr::{
    Keypair, SignedPacket, Timestamp,
    dns::{
        CLASS, Packet,
        rdata::{RData, TXT},
    },
};
use sha2::{Digest, Sha256};

use super::{AccountKeys, DeviceKind, Generation, InstallationId, PeerId, wire};
use crate::CoreError;

pub const RECORD_TTL_SECONDS: u32 = 86400;
pub const RECORD_LIFETIME_SECONDS: u64 = 604800;
pub const MAX_ACCOUNT_PACKET_BYTES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDevice {
    pub installation_id: InstallationId,
    pub transport_public_key: [u8; 32],
    pub kind: DeviceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRecord {
    pub generation: Generation,
    pub issued_at: u64,
    pub expires_at: u64,
    /// Strictly sorted by raw installation bytes, with unique transport keys.
    pub devices: Vec<RecordDevice>,
}

impl IdentityRecord {
    fn validate(&self) -> Result<(), CoreError> {
        if self.devices.len() > 5
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at > RECORD_LIFETIME_SECONDS
        {
            return Err(wire::invalid());
        }
        for (i, device) in self.devices.iter().enumerate() {
            if i > 0 && self.devices[i - 1].installation_id >= device.installation_id {
                return Err(wire::invalid());
            }
            if self.devices[..i]
                .iter()
                .any(|d| d.transport_public_key == device.transport_public_key)
            {
                return Err(wire::invalid());
            }
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, CoreError> {
        self.validate()?;
        let mut e = Encoder::new(Vec::new());
        e.array(5)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.generation.0)
            .unwrap()
            .u64(self.issued_at)
            .unwrap()
            .u64(self.expires_at)
            .unwrap()
            .array(self.devices.len() as u64)
            .unwrap();
        for device in &self.devices {
            e.array(3)
                .unwrap()
                .bytes(&device.installation_id.0)
                .unwrap()
                .bytes(&device.transport_public_key)
                .unwrap();
            wire::put_kind(&mut e, device.kind);
        }
        Ok(e.into_writer())
    }

    pub fn from_bytes(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() > MAX_ACCOUNT_PACKET_BYTES {
            return Err(wire::invalid());
        }
        let mut d = Decoder::new(input);
        wire::array(&mut d, 5)?;
        if wire::uint(&mut d)? != 1 {
            return Err(wire::invalid());
        }
        let generation = Generation(wire::bytes(&mut d)?);
        let issued_at = wire::uint(&mut d)?;
        let expires_at = wire::uint(&mut d)?;
        let count = d
            .array()
            .map_err(|_| wire::invalid())?
            .ok_or_else(wire::invalid)?;
        if count > 5 {
            return Err(wire::invalid());
        }
        let mut devices = Vec::with_capacity(count as usize);
        for _ in 0..count {
            wire::array(&mut d, 3)?;
            devices.push(RecordDevice {
                installation_id: InstallationId(wire::bytes(&mut d)?),
                transport_public_key: wire::bytes(&mut d)?,
                kind: wire::kind(&mut d)?,
            });
        }
        let record = Self {
            generation,
            issued_at,
            expires_at,
            devices,
        };
        wire::canonical(input, &record.to_bytes()?)?;
        Ok(record)
    }

    /// Expired signed records remain valid renewal bases, never authorization.
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }
}

/// Hash the full public-key/signature/timestamp/DNS wrapper, not its DNS value.
pub fn record_hash(packet: &SignedPacket) -> [u8; 32] {
    Sha256::digest(packet.as_bytes()).into()
}

pub fn record_packet(
    keys: &AccountKeys,
    record: &IdentityRecord,
    seq_us: u64,
) -> Result<SignedPacket, CoreError> {
    if seq_us > i64::MAX as u64 {
        return Err(wire::invalid());
    }
    let text = BASE64URL_NOPAD.encode(&record.to_bytes()?);
    let packet = SignedPacket::builder()
        .txt(
            "_account".try_into().unwrap(),
            TXT::try_from(text.as_str()).map_err(|_| wire::invalid())?,
            RECORD_TTL_SECONDS,
        )
        .timestamp(Timestamp::from(seq_us))
        .sign(&Keypair::from_secret_key(keys.identity().as_bytes()))
        .map_err(|_| wire::invalid())?;
    check_packet_size(&packet)?;
    Ok(packet)
}

pub(crate) fn check_packet_size(packet: &SignedPacket) -> Result<(), CoreError> {
    // The application wrapper limit is stricter than BEP44's DNS value limit.
    if packet.as_bytes().len() < 104
        || packet.as_bytes().len() > MAX_ACCOUNT_PACKET_BYTES
        || packet.encoded_packet().len() > 1000
    {
        return Err(wire::invalid());
    }
    Ok(())
}

pub fn parse_record(
    packet: &SignedPacket,
    expected: PeerId,
    now: u64,
) -> Result<IdentityRecord, CoreError> {
    let record = parse_stored_record(packet, expected)?;
    if record.issued_at > now.saturating_add(300) {
        return Err(wire::invalid());
    }
    Ok(record)
}

// An encrypted, accepted snapshot remains valid when the wall clock rolls back.
// This verifies bytes/signature/shape only; it never grants current authorization.
pub(crate) fn parse_stored_record(
    packet: &SignedPacket,
    expected: PeerId,
) -> Result<IdentityRecord, CoreError> {
    check_packet_size(packet)?;
    let bytes = packet.as_bytes();
    if bytes[..32] != expected.0 {
        return Err(wire::invalid());
    }
    let seq = u64::from_be_bytes(bytes[96..104].try_into().unwrap());
    if seq > i64::MAX as u64 {
        return Err(wire::invalid());
    }
    let dns = &bytes[104..];
    let mut signable = format!("3:seqi{seq}e1:v{}:", dns.len()).into_bytes();
    signable.extend_from_slice(dns);
    VerifyingKey::from_bytes(&expected.0)
        .map_err(|_| wire::invalid())?
        .verify_strict(
            &signable,
            &Signature::from_bytes(bytes[32..96].try_into().unwrap()),
        )
        .map_err(|_| wire::invalid())?;
    // Require one IN TXT answer, with no other DNS sections or header options.
    if dns.get(..12) != Some(&[0, 0, 128, 0, 0, 0, 0, 1, 0, 0, 0, 0]) {
        return Err(wire::invalid());
    }
    let parsed = Packet::parse(dns).map_err(|_| wire::invalid())?;
    let rr = &parsed.answers[0];
    let origin = pkarr::PublicKey::try_from(&expected.0)
        .map_err(|_| wire::invalid())?
        .to_z32();
    if rr.name.to_string() != format!("_account.{origin}")
        || rr.class != CLASS::IN
        || rr.ttl != RECORD_TTL_SECONDS
        || rr.cache_flush
    {
        return Err(wire::invalid());
    }
    // Packet::parse accepts a trailing suffix; independently bound this sole RR.
    let mut end = 12;
    loop {
        let label = *dns.get(end).ok_or_else(wire::invalid)?;
        if label & 0xc0 == 0xc0 {
            end += 2;
            break;
        }
        end += 1;
        if label == 0 {
            break;
        }
        end += label as usize;
    }
    let length = dns.get(end + 8..end + 10).ok_or_else(wire::invalid)?;
    let length = u16::from_be_bytes(length.try_into().unwrap()) as usize;
    if end + 10 + length != dns.len() {
        return Err(wire::invalid());
    }
    let RData::TXT(txt) = &rr.rdata else {
        return Err(wire::invalid());
    };
    let text = String::try_from(txt.clone()).map_err(|_| wire::invalid())?;
    let payload = BASE64URL_NOPAD
        .decode(text.as_bytes())
        .map_err(|_| wire::invalid())?;
    if BASE64URL_NOPAD.encode(&payload) != text {
        return Err(wire::invalid());
    }
    IdentityRecord::from_bytes(&payload)
}
