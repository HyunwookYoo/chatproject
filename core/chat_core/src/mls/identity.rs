//! MLS identity: the per-installation signature key and the credential that binds it to the
//! account's device list.
//!
//! The MLS signature key is derived from the device seed (stable across recovery). The
//! credential carries `peer`, `installation`, the MLS public key and a `binding` signature made
//! with the installation's transport key, so a peer can check a leaf against the account's
//! published `IdentityRecord` without trusting anything else.
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use minicbor::{Decoder, Encoder};
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::types::SignatureScheme;

use crate::account::{DeviceKeys, IdentityRecord, InstallationId, PeerId};
use crate::{CoreError, ErrorCode};

type R<T> = Result<T, CoreError>;

const BINDING_DOMAIN: &[u8] = b"ChatProject/mls-credential/v1";
/// Encoded credentials are about 170 bytes; anything above this is rejected before parsing.
pub(crate) const MAX_CREDENTIAL_BYTES: usize = 200;

fn invalid_credential() -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, "invalid mls credential")
}

fn auth_failed() -> CoreError {
    CoreError::new(ErrorCode::AuthFailed, "mls credential verification failed")
}

/// The installation's MLS signature key pair, derived deterministically from the device seed.
pub(crate) fn mls_signer(device: &DeviceKeys) -> SignatureKeyPair {
    let seed = device.mls_signature_seed();
    let public = SigningKey::from_bytes(seed.as_bytes()).verifying_key();
    SignatureKeyPair::from_raw(
        SignatureScheme::ED25519,
        seed.as_bytes().to_vec(),
        public.to_bytes().to_vec(),
    )
}

fn binding_message(peer: &PeerId, installation: &InstallationId, mls_public: &[u8; 32]) -> Vec<u8> {
    let mut message = BINDING_DOMAIN.to_vec();
    message.extend_from_slice(&peer.0);
    message.extend_from_slice(&installation.0);
    message.extend_from_slice(mls_public);
    message
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MlsCredential {
    pub peer: PeerId,
    pub installation: InstallationId,
    pub mls_public: [u8; 32],
    pub binding: [u8; 64],
}

impl MlsCredential {
    /// Bind this installation's MLS key to `peer` with a signature by the transport key.
    pub(crate) fn issue(peer: PeerId, device: &DeviceKeys) -> MlsCredential {
        let installation = device.installation_id();
        let mut mls_public = [0u8; 32];
        mls_public.copy_from_slice(mls_signer(device).public());
        let binding = SigningKey::from_bytes(device.transport().as_bytes())
            .sign(&binding_message(&peer, &installation, &mls_public))
            .to_bytes();
        MlsCredential {
            peer,
            installation,
            mls_public,
            binding,
        }
    }

    /// Canonical CBOR `[1, peer, installation, mls_public, binding]`.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(5)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.peer.0)
            .unwrap()
            .bytes(&self.installation.0)
            .unwrap()
            .bytes(&self.mls_public)
            .unwrap()
            .bytes(&self.binding)
            .unwrap();
        e.into_writer()
    }

    pub(crate) fn decode(input: &[u8]) -> R<Self> {
        if input.len() > MAX_CREDENTIAL_BYTES {
            return Err(invalid_credential());
        }
        fn bytes<const N: usize>(d: &mut Decoder<'_>) -> R<[u8; N]> {
            d.bytes()
                .map_err(|_| invalid_credential())?
                .try_into()
                .map_err(|_| invalid_credential())
        }
        let mut d = Decoder::new(input);
        if d.array().map_err(|_| invalid_credential())? != Some(5)
            || d.u64().map_err(|_| invalid_credential())? != 1
        {
            return Err(invalid_credential());
        }
        let credential = Self {
            peer: PeerId(bytes(&mut d)?),
            installation: InstallationId(bytes(&mut d)?),
            mls_public: bytes(&mut d)?,
            binding: bytes(&mut d)?,
        };
        // Trailing bytes and non-canonical encodings both differ from the re-encoding.
        if credential.encode() != input {
            return Err(invalid_credential());
        }
        Ok(credential)
    }
}

/// Check an MLS leaf against the peer's account record and return the installation it belongs
/// to. A malformed or mismatching credential is `AuthFailed`; an installation missing from
/// `record` is `RecordUnavailable` (the record may simply be older than the leaf).
pub(crate) fn verify_leaf(
    credential: &[u8],
    leaf_signature_key: &[u8],
    peer: PeerId,
    record: &IdentityRecord,
) -> R<InstallationId> {
    let credential = MlsCredential::decode(credential).map_err(|_| auth_failed())?;
    if credential.peer != peer {
        return Err(auth_failed());
    }
    let device = record
        .devices
        .iter()
        .find(|device| device.installation_id == credential.installation)
        .ok_or_else(|| {
            CoreError::new(
                ErrorCode::RecordUnavailable,
                "mls installation is not in the account record",
            )
        })?;
    if credential.mls_public.as_slice() != leaf_signature_key {
        return Err(auth_failed());
    }
    VerifyingKey::from_bytes(&device.transport_public_key)
        .map_err(|_| auth_failed())?
        .verify_strict(
            &binding_message(
                &credential.peer,
                &credential.installation,
                &credential.mls_public,
            ),
            &Signature::from_bytes(&credential.binding),
        )
        .map_err(|_| auth_failed())?;
    Ok(credential.installation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{DeviceKind, Generation, RecordDevice, Secret32, derive_device};
    use openmls_traits::signatures::Signer as _;

    const PEER: PeerId = PeerId([7; 32]);

    fn device(seed: u8) -> DeviceKeys {
        derive_device(Secret32::from([seed; 32])).unwrap()
    }

    fn record(devices: &[&DeviceKeys]) -> IdentityRecord {
        let mut devices: Vec<RecordDevice> = devices
            .iter()
            .map(|d| RecordDevice {
                installation_id: d.installation_id(),
                transport_public_key: d.transport_public_key(),
                kind: DeviceKind::Mobile,
            })
            .collect();
        devices.sort_by_key(|d| d.installation_id);
        IdentityRecord {
            generation: Generation([1; 16]),
            issued_at: 10,
            expires_at: 20,
            devices,
        }
    }

    fn leaf(device: &DeviceKeys) -> Vec<u8> {
        mls_signer(device).public().to_vec()
    }

    fn code(r: R<InstallationId>) -> ErrorCode {
        r.unwrap_err().code
    }

    #[test]
    fn signer_is_stable_and_distinct_from_transport() {
        let a = mls_signer(&device(1));
        let b = mls_signer(&device(1));
        assert_eq!(a.public(), b.public());
        assert_eq!(a.signature_scheme(), SignatureScheme::ED25519);
        assert_eq!(a.public().len(), 32);
        assert_ne!(a.public(), device(1).transport_public_key());
        assert_ne!(a.public(), device(1).installation_id().0);
        assert_ne!(a.public(), mls_signer(&device(2)).public());
        // The pair is a usable Ed25519 signer.
        let sig = a.sign(b"payload").unwrap();
        VerifyingKey::from_bytes(a.public().try_into().unwrap())
            .unwrap()
            .verify_strict(b"payload", &Signature::from_slice(&sig).unwrap())
            .unwrap();
    }

    #[test]
    fn credential_round_trip_and_verify() {
        let d = device(1);
        let cred = MlsCredential::issue(PEER, &d);
        let bytes = cred.encode();
        assert!(bytes.len() <= MAX_CREDENTIAL_BYTES);
        assert_eq!(MlsCredential::decode(&bytes).unwrap(), cred);
        let other = device(2);
        let record = record(&[&d, &other]);
        assert_eq!(
            verify_leaf(&bytes, &leaf(&d), PEER, &record).unwrap(),
            d.installation_id()
        );
    }

    #[test]
    fn credential_rejects_wrong_peer() {
        let d = device(1);
        let bytes = MlsCredential::issue(PEER, &d).encode();
        assert_eq!(
            code(verify_leaf(
                &bytes,
                &leaf(&d),
                PeerId([8; 32]),
                &record(&[&d])
            )),
            ErrorCode::AuthFailed
        );
    }

    #[test]
    fn credential_rejects_unlisted_installation() {
        let (d, other) = (device(1), device(2));
        let bytes = MlsCredential::issue(PEER, &d).encode();
        assert_eq!(
            code(verify_leaf(&bytes, &leaf(&d), PEER, &record(&[&other]))),
            ErrorCode::RecordUnavailable
        );
    }

    #[test]
    fn credential_rejects_swapped_mls_key() {
        let (d, other) = (device(1), device(2));
        let bytes = MlsCredential::issue(PEER, &d).encode();
        // Leaf key differs from the credential's.
        assert_eq!(
            code(verify_leaf(&bytes, &leaf(&other), PEER, &record(&[&d]))),
            ErrorCode::AuthFailed
        );
        // Credential's mls_public replaced: binding no longer covers it.
        let mut forged = MlsCredential::issue(PEER, &d);
        forged.mls_public = mls_signer(&other).public().try_into().unwrap();
        assert_eq!(
            code(verify_leaf(
                &forged.encode(),
                &leaf(&other),
                PEER,
                &record(&[&d])
            )),
            ErrorCode::AuthFailed
        );
    }

    #[test]
    fn credential_rejects_transport_signature_of_other_device() {
        let (d, other) = (device(1), device(2));
        // `other` signs a credential that names `d`'s installation.
        let mut cred = MlsCredential::issue(PEER, &other);
        cred.installation = d.installation_id();
        assert_eq!(
            code(verify_leaf(
                &cred.encode(),
                &leaf(&other),
                PEER,
                &record(&[&d, &other])
            )),
            ErrorCode::AuthFailed
        );
    }

    #[test]
    fn credential_decode_rejects_oversize_and_trailing() {
        let bytes = MlsCredential::issue(PEER, &device(1)).encode();
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(MlsCredential::decode(&trailing).is_err());
        let oversize = vec![0u8; MAX_CREDENTIAL_BYTES + 1];
        assert!(MlsCredential::decode(&oversize).is_err());
        assert!(MlsCredential::decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(MlsCredential::decode(&[]).is_err());
        // verify_leaf reports every malformed credential as AuthFailed.
        assert_eq!(
            code(verify_leaf(
                &trailing,
                &[0; 32],
                PEER,
                &record(&[&device(1)])
            )),
            ErrorCode::AuthFailed
        );
    }
}
