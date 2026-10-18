//! Exact secret payload, channel-bound receipt and bounded ordered transfer.
use crate::{
    CoreError,
    account::{
        AccountKeys, DeviceKeys, FlowId, Generation, InstallationCert, InstallationId, PeerId,
        Secret32, TxId,
        contacts::Contact,
        wire::{self, invalid},
    },
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use minicbor::{Decoder, Encoder, data::Type};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub(crate) const LIMIT: usize = 4 * 1024 * 1024;
pub(crate) const CHUNK: usize = 16 * 1024 - 64;
pub(crate) fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
pub(crate) struct Payload {
    pub tx: TxId,
    pub keys: AccountKeys,
    pub cert: InstallationCert,
    pub generation: Generation,
    pub base: [u8; 32],
    pub contacts: Vec<Contact>,
}
impl Payload {
    pub(crate) fn encode_parts(
        tx: TxId,
        keys: &AccountKeys,
        cert: &InstallationCert,
        generation: Generation,
        base: [u8; 32],
        contacts: &[Contact],
    ) -> Zeroizing<Vec<u8>> {
        // Reserve the bounded maximum once so seed-containing allocations never grow.
        let mut bytes = Zeroizing::new(Vec::with_capacity(LIMIT));
        let mut e = Encoder::new(&mut *bytes);
        e.array(7)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&tx.0)
            .unwrap()
            .bytes(keys.identity().as_bytes())
            .unwrap();
        e.writer_mut().extend_from_slice(&cert.to_bytes());
        e.bytes(&generation.0)
            .unwrap()
            .bytes(&base)
            .unwrap()
            .array(contacts.len() as u64)
            .unwrap();
        for c in contacts {
            e.array(4)
                .unwrap()
                .bytes(&c.peer.0)
                .unwrap()
                .str(&c.alias)
                .unwrap()
                .u8(u8::from(c.verified_digest.is_some()))
                .unwrap();
            if let Some(digest) = c.verified_digest {
                e.bytes(&digest).unwrap();
            } else {
                e.null().unwrap();
            }
        }
        bytes
    }
    pub(crate) fn decode(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() > LIMIT {
            return Err(invalid());
        }
        let mut d = Decoder::new(input);
        wire::array(&mut d, 7)?;
        if wire::uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let tx = TxId(wire::bytes(&mut d)?);
        let mut seed = Secret32::from([0; 32]);
        let raw = d.bytes().map_err(|_| invalid())?;
        if raw.len() != 32 {
            return Err(invalid());
        }
        seed.as_mut_bytes().copy_from_slice(raw);
        let keys = AccountKeys::from_identity(seed);
        let start = d.position();
        d.skip().map_err(|_| invalid())?;
        let cert = InstallationCert::from_bytes(&input[start..d.position()])?;
        crate::account::verify_cert(&cert, keys.peer_id())?;
        let generation = Generation(wire::bytes(&mut d)?);
        let base = wire::bytes(&mut d)?;
        let n = d.array().map_err(|_| invalid())?.ok_or_else(invalid)?;
        if n > 1000 {
            return Err(invalid());
        }
        let mut payload = Self {
            tx,
            keys,
            cert,
            generation,
            base,
            contacts: Vec::with_capacity(n as usize),
        };
        for _ in 0..n {
            wire::array(&mut d, 4)?;
            let peer = PeerId(wire::bytes(&mut d)?);
            let alias = d.str().map_err(|_| invalid())?;
            crate::account::invite::validate_name(alias)?;
            let verified = wire::uint(&mut d)?;
            let digest = optional(&mut d)?;
            if peer == payload.keys.peer_id()
                || payload.contacts.last().is_some_and(|c| c.peer >= peer)
                || verified != u64::from(digest.is_some())
                || digest.is_some_and(|v| {
                    v != crate::account::safety_digest(payload.keys.peer_id(), peer)
                })
            {
                return Err(invalid());
            }
            payload.contacts.push(Contact {
                peer,
                alias: alias.into(),
                verified_digest: digest,
                known_installation: None,
                new_device: false,
            });
        }
        wire::canonical(
            input,
            &Self::encode_parts(
                tx,
                &payload.keys,
                &payload.cert,
                generation,
                base,
                &payload.contacts,
            ),
        )?;
        Ok(payload)
    }
    pub(crate) fn matches_device(&self, device: &DeviceKeys) -> bool {
        self.cert.installation_id == device.installation_id()
            && self.cert.transport_public_key == device.transport_public_key()
    }
}
pub(crate) fn optional<const N: usize>(d: &mut Decoder<'_>) -> Result<Option<[u8; N]>, CoreError> {
    if d.datatype().map_err(|_| invalid())? == Type::Null {
        d.null().map_err(|_| invalid())?;
        Ok(None)
    } else {
        Ok(Some(wire::bytes(d)?))
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub tx: TxId,
    pub payload_hash: [u8; 32],
    pub installation: InstallationId,
    pub flow: FlowId,
    pub handshake: [u8; 32],
    pub sig: [u8; 64],
}
impl Receipt {
    fn encode(&self, signed: bool) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(if signed { 7 } else { 6 })
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.tx.0)
            .unwrap()
            .bytes(&self.payload_hash)
            .unwrap()
            .bytes(&self.installation.0)
            .unwrap()
            .bytes(&self.flow.0)
            .unwrap()
            .bytes(&self.handshake)
            .unwrap();
        if signed {
            e.bytes(&self.sig).unwrap();
        }
        e.into_writer()
    }
    fn message(&self) -> Vec<u8> {
        let mut v = b"ChatProject/pair-receipt/v1".to_vec();
        v.push(1);
        v.extend(self.encode(false));
        v
    }
    pub(crate) fn sign(
        tx: TxId,
        payload_hash: [u8; 32],
        device: &DeviceKeys,
        flow: FlowId,
        handshake: [u8; 32],
    ) -> Self {
        let mut r = Self {
            tx,
            payload_hash,
            installation: device.installation_id(),
            flow,
            handshake,
            sig: [0; 64],
        };
        r.sig = SigningKey::from_bytes(device.installation().as_bytes())
            .sign(&r.message())
            .to_bytes();
        r
    }
    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.encode(true)
    }
    pub(crate) fn decode(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() > 256 {
            return Err(invalid());
        }
        let mut d = Decoder::new(input);
        wire::array(&mut d, 7)?;
        if wire::uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let r = Self {
            tx: TxId(wire::bytes(&mut d)?),
            payload_hash: wire::bytes(&mut d)?,
            installation: InstallationId(wire::bytes(&mut d)?),
            flow: FlowId(wire::bytes(&mut d)?),
            handshake: wire::bytes(&mut d)?,
            sig: wire::bytes(&mut d)?,
        };
        wire::canonical(input, &r.bytes())?;
        VerifyingKey::from_bytes(&r.installation.0)
            .map_err(|_| invalid())?
            .verify_strict(&r.message(), &Signature::from_bytes(&r.sig))
            .map_err(|_| invalid())?;
        Ok(r)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Envelope {
    pub tx: TxId,
    pub total: usize,
    pub hash: [u8; 32],
}
impl Envelope {
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(4)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.tx.0)
            .unwrap()
            .u64(self.total as u64)
            .unwrap()
            .bytes(&self.hash)
            .unwrap();
        e.into_writer()
    }
    pub(crate) fn decode(input: &[u8]) -> Result<Self, CoreError> {
        let mut d = Decoder::new(input);
        wire::array(&mut d, 4)?;
        if wire::uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let tx = TxId(wire::bytes(&mut d)?);
        let total = wire::uint(&mut d)?;
        if total == 0 || total > LIMIT as u64 {
            return Err(invalid());
        }
        let e = Self {
            tx,
            total: total as usize,
            hash: wire::bytes(&mut d)?,
        };
        wire::canonical(input, &e.bytes())?;
        Ok(e)
    }
}
pub(crate) fn chunk(tx: TxId, index: u64, bytes: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(bytes.len() + 64));
    let mut e = Encoder::new(&mut *out);
    e.array(4)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&tx.0)
        .unwrap()
        .u64(index)
        .unwrap()
        .bytes(bytes)
        .unwrap();
    out
}
pub(crate) struct Receiving {
    pub envelope: Envelope,
    index: u64,
    bytes: Zeroizing<Vec<u8>>,
}
impl Receiving {
    pub(crate) fn new(envelope: Envelope) -> Self {
        Self {
            envelope,
            index: 0,
            bytes: Zeroizing::new(Vec::with_capacity(envelope.total)),
        }
    }
    pub(crate) fn push(&mut self, input: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, CoreError> {
        let mut d = Decoder::new(input);
        wire::array(&mut d, 4)?;
        if wire::uint(&mut d)? != 1
            || TxId(wire::bytes(&mut d)?) != self.envelope.tx
            || wire::uint(&mut d)? != self.index
        {
            return Err(invalid());
        }
        let bytes = d.bytes().map_err(|_| invalid())?;
        if bytes.is_empty()
            || bytes.len() > CHUNK
            || self.bytes.len() + bytes.len() > self.envelope.total
        {
            return Err(invalid());
        }
        wire::canonical(input, &chunk(self.envelope.tx, self.index, bytes))?;
        self.bytes.extend_from_slice(bytes);
        self.index += 1;
        if self.bytes.len() == self.envelope.total {
            if hash(&self.bytes) != self.envelope.hash {
                return Err(invalid());
            }
            Ok(Some(std::mem::replace(
                &mut self.bytes,
                Zeroizing::new(Vec::new()),
            )))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{DeviceKind, derive_account, derive_device, sign_cert};
    fn payload() -> (DeviceKeys, Zeroizing<Vec<u8>>) {
        let keys = derive_account(&Secret32::from([3; 32])).unwrap();
        let device = derive_device(Secret32::from([4; 32])).unwrap();
        let cert = sign_cert(&keys, &device, DeviceKind::Mobile, 1000).unwrap();
        let contacts = vec![Contact {
            peer: PeerId([9; 32]),
            alias: "alias".into(),
            verified_digest: Some(crate::account::safety_digest(
                keys.peer_id(),
                PeerId([9; 32]),
            )),
            known_installation: None,
            new_device: false,
        }];
        (
            device,
            Payload::encode_parts(
                TxId([1; 16]),
                &keys,
                &cert,
                Generation([2; 16]),
                [8; 32],
                &contacts,
            ),
        )
    }
    #[test]
    fn payload_exact_four_contact_fields_and_seed_certificate_validation() {
        let (device, bytes) = payload();
        let p = Payload::decode(&bytes).unwrap();
        assert!(p.matches_device(&device));
        assert!(p.contacts[0].known_installation.is_none());
        assert!(!p.contacts[0].new_device);
        let mut d = Decoder::new(&bytes);
        assert_eq!(d.array().unwrap(), Some(7));
        for _ in 0..6 {
            d.skip().unwrap();
        }
        assert_eq!(d.array().unwrap(), Some(1));
        assert_eq!(d.array().unwrap(), Some(4));
        let mut bad = bytes.to_vec();
        bad[22] ^= 1;
        assert!(Payload::decode(&bad).is_err());
        let mut bad = bytes.to_vec();
        bad.push(0);
        assert!(Payload::decode(&bad).is_err());
        let mut bad = bytes.to_vec();
        bad.splice(0..1, [0x98, 7]);
        assert!(Payload::decode(&bad).is_err());
    }
    #[test]
    fn ordered_chunks_reject_skip_duplicate_excess_hash_and_trailing() {
        let (_, bytes) = payload();
        let e = Envelope {
            tx: TxId([1; 16]),
            total: bytes.len(),
            hash: hash(&bytes),
        };
        for (index, payload) in [(1, bytes.as_slice()), (0, &bytes[..0])] {
            assert!(
                Receiving::new(e)
                    .push(&chunk(e.tx, index, payload))
                    .is_err()
            );
        }
        let mut r = Receiving::new(e);
        assert!(r.push(&chunk(e.tx, 0, &bytes[..10])).unwrap().is_none());
        assert!(r.push(&chunk(e.tx, 0, &bytes[10..])).is_err());
        let mut extra = bytes.to_vec();
        extra.push(0);
        assert!(Receiving::new(e).push(&chunk(e.tx, 0, &extra)).is_err());
        let mut bad = bytes.to_vec();
        bad[20] ^= 1;
        assert!(Receiving::new(e).push(&chunk(e.tx, 0, &bad)).is_err());
        let mut noncanonical = chunk(e.tx, 0, &bytes);
        noncanonical.push(0);
        assert!(Receiving::new(e).push(&noncanonical).is_err());
        let result = Receiving::new(e)
            .push(&chunk(e.tx, 0, &bytes))
            .unwrap()
            .unwrap();
        assert_eq!(result, bytes);
        let too_big = Envelope {
            total: LIMIT + 1,
            ..e
        };
        assert!(Envelope::decode(&too_big.bytes()).is_err());
    }
    #[test]
    fn receipt_is_exact_signed_transaction_hash_installation_session_and_noise() {
        let (device, bytes) = payload();
        let r = Receipt::sign(
            TxId([1; 16]),
            hash(&bytes),
            &device,
            FlowId([7; 16]),
            [8; 32],
        );
        assert!(Receipt::decode(&r.bytes()).is_ok());
        let mut changed = r.clone();
        changed.flow.0[0] ^= 1;
        assert!(Receipt::decode(&changed.bytes()).is_err());
        let mut changed = r.clone();
        changed.handshake[0] ^= 1;
        assert!(Receipt::decode(&changed.bytes()).is_err());
        let mut changed = r.clone();
        changed.payload_hash[0] ^= 1;
        assert!(Receipt::decode(&changed.bytes()).is_err());
        let mut trailing = r.bytes();
        trailing.push(0);
        assert!(Receipt::decode(&trailing).is_err());
    }
}
