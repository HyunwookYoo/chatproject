use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use minicbor::{Decoder, Encoder};
use sha2::{Digest, Sha256};

use super::{AccountKeys, DeviceKeys, DeviceKind, InstallationId, PeerId, wire};
use crate::CoreError;

const DOMAIN: &[u8] = b"ChatProject/cert/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationCert {
    pub peer_id: PeerId,
    pub installation_id: InstallationId,
    pub transport_public_key: [u8; 32],
    pub kind: DeviceKind,
    pub created_at: u64,
    pub sig: [u8; 64],
}

impl InstallationCert {
    fn encode(&self, signed: bool) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(if signed { 7 } else { 6 })
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.peer_id.0)
            .unwrap()
            .bytes(&self.installation_id.0)
            .unwrap()
            .bytes(&self.transport_public_key)
            .unwrap();
        wire::put_kind(&mut e, self.kind);
        e.u64(self.created_at).unwrap();
        if signed {
            e.bytes(&self.sig).unwrap();
        }
        e.into_writer()
    }

    fn message(&self) -> Vec<u8> {
        let mut message = DOMAIN.to_vec();
        message.push(1);
        message.extend(self.encode(false));
        message
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode(true)
    }

    /// Decode structure only; callers must also verify the expected account.
    pub fn from_bytes(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() > 256 {
            return Err(wire::invalid());
        }
        let mut d = Decoder::new(input);
        wire::array(&mut d, 7)?;
        if wire::uint(&mut d)? != 1 {
            return Err(wire::invalid());
        }
        let cert = Self {
            peer_id: PeerId(wire::bytes(&mut d)?),
            installation_id: InstallationId(wire::bytes(&mut d)?),
            transport_public_key: wire::bytes(&mut d)?,
            kind: wire::kind(&mut d)?,
            created_at: wire::uint(&mut d)?,
            sig: wire::bytes(&mut d)?,
        };
        wire::canonical(input, &cert.to_bytes())?;
        Ok(cert)
    }

    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.to_bytes()).into()
    }
}

pub fn sign_cert(
    keys: &AccountKeys,
    device: &DeviceKeys,
    kind: DeviceKind,
    at: u64,
) -> Result<InstallationCert, CoreError> {
    sign_cert_public(
        keys,
        device.installation_id(),
        device.transport_public_key(),
        kind,
        at,
    )
}

pub(crate) fn sign_cert_public(
    keys: &AccountKeys,
    installation_id: InstallationId,
    transport_public_key: [u8; 32],
    kind: DeviceKind,
    at: u64,
) -> Result<InstallationCert, CoreError> {
    let mut cert = InstallationCert {
        peer_id: keys.peer_id(),
        installation_id,
        transport_public_key,
        kind,
        created_at: at,
        sig: [0; 64],
    };
    cert.sig = SigningKey::from_bytes(keys.identity().as_bytes())
        .sign(&cert.message())
        .to_bytes();
    Ok(cert)
}

pub fn verify_cert(cert: &InstallationCert, expected: PeerId) -> Result<(), CoreError> {
    if cert.peer_id != expected {
        return Err(wire::invalid());
    }
    VerifyingKey::from_bytes(&expected.0)
        .map_err(|_| wire::invalid())?
        .verify_strict(&cert.message(), &Signature::from_bytes(&cert.sig))
        .map_err(|_| wire::invalid())
}
