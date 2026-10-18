//! Exact canonical QR, preface, prologue and proof encodings.
use super::PairDirection;
use crate::CoreError;
use crate::account::{
    DeviceKeys, FlowId, InstallationId, PeerId, Secret32, TxId,
    wire::{self, array, bytes, canonical, invalid, uint},
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use minicbor::{Decoder, Encoder, data::Type};
use zeroize::Zeroizing;
pub(crate) const PREFIX: &str = "chatproject:pair/1/";
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Existing,
    New,
}
impl Role {
    pub(crate) fn number(self) -> u8 {
        if self == Self::Existing { 0 } else { 1 }
    }
}
pub(crate) struct Qr {
    pub direction: PairDirection,
    pub flow: FlowId,
    pub transport: [u8; 32],
    pub installation: InstallationId,
    pub existing: Option<PeerId>,
    pub resume: Option<TxId>,
    pub relay: Option<String>,
    pub expires: u64,
    pub psk: Secret32,
}
fn put_optional(e: &mut Encoder<Vec<u8>>, value: Option<&[u8]>) {
    if let Some(v) = value {
        e.bytes(v).unwrap();
    } else {
        e.null().unwrap();
    }
}
fn optional<const N: usize>(d: &mut Decoder<'_>) -> Result<Option<[u8; N]>, CoreError> {
    if d.datatype().map_err(|_| invalid())? == Type::Null {
        d.null().map_err(|_| invalid())?;
        Ok(None)
    } else {
        Ok(Some(bytes(d)?))
    }
}
impl Qr {
    pub(crate) fn owner(&self) -> Role {
        match self.direction {
            PairDirection::NewDisplays => Role::New,
            PairDirection::ExistingDisplays => Role::Existing,
        }
    }
    fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut e = Encoder::new(Vec::new());
        e.array(10)
            .unwrap()
            .u8(1)
            .unwrap()
            .u8(match self.direction {
                PairDirection::NewDisplays => 0,
                PairDirection::ExistingDisplays => 1,
            })
            .unwrap()
            .bytes(&self.flow.0)
            .unwrap()
            .bytes(&self.transport)
            .unwrap()
            .bytes(&self.installation.0)
            .unwrap();
        put_optional(&mut e, self.existing.as_ref().map(|v| v.0.as_slice()));
        put_optional(&mut e, self.resume.as_ref().map(|v| v.0.as_slice()));
        if let Some(r) = &self.relay {
            e.str(r).unwrap();
        } else {
            e.null().unwrap();
        }
        e.u64(self.expires)
            .unwrap()
            .bytes(self.psk.as_bytes())
            .unwrap();
        Zeroizing::new(e.into_writer())
    }
    pub(crate) fn display(&self) -> String {
        format!(
            "{}{}",
            PREFIX,
            data_encoding::BASE64URL_NOPAD.encode(&self.encode())
        )
    }
    pub(crate) fn parse(text: &str, now: u64) -> Result<Self, CoreError> {
        let encoded = text.strip_prefix(PREFIX).ok_or_else(invalid)?;
        if encoded.len() > 2731 {
            return Err(invalid());
        } // Reject before decoding/allocation.
        let input = Zeroizing::new(
            data_encoding::BASE64URL_NOPAD
                .decode(encoded.as_bytes())
                .map_err(|_| invalid())?,
        );
        if input.len() > 2048 {
            return Err(invalid());
        }
        let mut d = Decoder::new(&input);
        array(&mut d, 10)?;
        if uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let direction = match uint(&mut d)? {
            0 => PairDirection::NewDisplays,
            1 => PairDirection::ExistingDisplays,
            _ => return Err(invalid()),
        };
        let flow = FlowId(bytes(&mut d)?);
        let transport = bytes(&mut d)?;
        let installation = InstallationId(bytes(&mut d)?);
        let existing = optional(&mut d)?.map(PeerId);
        let resume = optional(&mut d)?.map(TxId);
        let relay = if d.datatype().map_err(|_| invalid())? == Type::Null {
            d.null().map_err(|_| invalid())?;
            None
        } else {
            let r = d.str().map_err(|_| invalid())?;
            if r.len() > 256 {
                return Err(invalid());
            }
            let u: iroh::RelayUrl = r.parse().map_err(|_| invalid())?;
            if u.scheme() != "https"
                || !u.username().is_empty()
                || u.password().is_some()
                || u.fragment().is_some()
            {
                return Err(invalid());
            }
            Some(r.to_owned())
        };
        let expires = uint(&mut d)?;
        let psk = Secret32::from(bytes(&mut d)?);
        let qr = Self {
            direction,
            flow,
            transport,
            installation,
            existing,
            resume,
            relay,
            expires,
            psk,
        };
        if (qr.owner() == Role::Existing) != qr.existing.is_some()
            || expires <= now
            || expires > now.saturating_add(300)
        {
            return Err(invalid());
        }
        iroh::EndpointId::from_bytes(&transport).map_err(|_| invalid())?;
        VerifyingKey::from_bytes(&installation.0).map_err(|_| invalid())?;
        canonical(&input, &qr.encode())?;
        Ok(qr)
    }
}
#[derive(Clone)]
pub(crate) struct Preface {
    pub flow: FlowId,
    pub role: Role,
    pub transport: [u8; 32],
    pub installation: InstallationId,
    pub existing: Option<PeerId>,
    pub resume: Option<TxId>,
}
impl Preface {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(7)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.flow.0)
            .unwrap()
            .u8(self.role.number())
            .unwrap()
            .bytes(&self.transport)
            .unwrap()
            .bytes(&self.installation.0)
            .unwrap();
        put_optional(&mut e, self.existing.as_ref().map(|v| v.0.as_slice()));
        put_optional(&mut e, self.resume.as_ref().map(|v| v.0.as_slice()));
        e.into_writer()
    }
    pub(crate) fn parse(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() > 16384 {
            return Err(invalid());
        }
        let mut d = Decoder::new(input);
        array(&mut d, 7)?;
        if uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let flow = FlowId(bytes(&mut d)?);
        let role = match uint(&mut d)? {
            0 => Role::Existing,
            1 => Role::New,
            _ => return Err(invalid()),
        };
        let value = Self {
            flow,
            role,
            transport: bytes(&mut d)?,
            installation: InstallationId(bytes(&mut d)?),
            existing: optional(&mut d)?.map(PeerId),
            resume: optional(&mut d)?.map(TxId),
        };
        if (role == Role::Existing) != value.existing.is_some() {
            return Err(invalid());
        }
        canonical(input, &value.encode())?;
        Ok(value)
    }
}
pub(crate) fn prologue(
    qr: &Qr,
    local: &Preface,
    remote: &Preface,
    actual: [u8; 32],
    initiator: bool,
) -> Result<Vec<u8>, CoreError> {
    if local.role == remote.role
        || local.flow != qr.flow
        || remote.flow != qr.flow
        || local.resume != qr.resume
        || remote.resume != qr.resume
        || remote.transport != actual
    {
        return Err(invalid());
    }
    let owner = if initiator { remote } else { local };
    if owner.role != qr.owner()
        || owner.transport != qr.transport
        || owner.installation != qr.installation
        || owner.existing != qr.existing
    {
        return Err(invalid());
    }
    let (e, n) = if local.role == Role::Existing {
        (local, remote)
    } else {
        (remote, local)
    };
    let mut enc = Encoder::new(Vec::new());
    enc.array(10)
        .unwrap()
        .u8(1)
        .unwrap()
        .u8(match qr.direction {
            PairDirection::NewDisplays => 0,
            PairDirection::ExistingDisplays => 1,
        })
        .unwrap()
        .bytes(&qr.flow.0)
        .unwrap()
        .bytes(&e.existing.ok_or_else(invalid)?.0)
        .unwrap()
        .bytes(&e.transport)
        .unwrap()
        .bytes(&e.installation.0)
        .unwrap()
        .bytes(&n.transport)
        .unwrap()
        .bytes(&n.installation.0)
        .unwrap();
    put_optional(&mut enc, qr.resume.as_ref().map(|v| v.0.as_slice()));
    enc.u64(qr.expires).unwrap();
    Ok(enc.into_writer())
}
fn proof_body(flow: FlowId, role: Role, hash: &[u8; 32], prologue: &[u8; 32]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.array(5)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&flow.0)
        .unwrap()
        .u8(role.number())
        .unwrap()
        .bytes(hash)
        .unwrap()
        .bytes(prologue)
        .unwrap();
    e.into_writer()
}
fn proof_message(body: &[u8]) -> Vec<u8> {
    let mut m = b"ChatProject/pair-proof/v1".to_vec();
    m.push(1);
    m.extend(body);
    m
}
pub(crate) fn proof(
    device: &DeviceKeys,
    p: &Preface,
    hash: &[u8; 32],
    prologue: &[u8; 32],
) -> Vec<u8> {
    let mut body = proof_body(p.flow, p.role, hash, prologue);
    let sig = SigningKey::from_bytes(device.installation().as_bytes()).sign(&proof_message(&body));
    body[0] = 0x86;
    Encoder::new(&mut body).bytes(&sig.to_bytes()).unwrap();
    body
}
pub(crate) fn verify_proof(
    input: &[u8],
    p: &Preface,
    hash: &[u8; 32],
    prologue: &[u8; 32],
) -> Result<(), CoreError> {
    let mut d = Decoder::new(input);
    array(&mut d, 6)?;
    if uint(&mut d)? != 1
        || bytes::<16>(&mut d)? != p.flow.0
        || uint(&mut d)? != u64::from(p.role.number())
        || bytes::<32>(&mut d)? != *hash
        || bytes::<32>(&mut d)? != *prologue
    {
        return Err(invalid());
    }
    let sig = bytes::<64>(&mut d)?;
    let mut body = proof_body(p.flow, p.role, hash, prologue);
    VerifyingKey::from_bytes(&p.installation.0)
        .map_err(|_| invalid())?
        .verify_strict(&proof_message(&body), &Signature::from_bytes(&sig))
        .map_err(|_| invalid())?;
    body[0] = 0x86;
    Encoder::new(&mut body).bytes(&sig).unwrap();
    wire::canonical(input, &body)
}
