//! Encrypted envelope and canonical private account payloads.
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{AeadInPlace, Key},
};
use zeroize::Zeroizing;

use super::{CoreError, ErrorCode, Secret32, StoreId, StoredPlaintext};

pub(super) const FILE_LIMIT: usize = 4 * 1024 * 1024;
const DOMAIN: &[u8] = b"ChatProject/store/v1";

fn corrupt() -> CoreError {
    CoreError::new(ErrorCode::StorageCorrupt, "Invalid account snapshot")
}

fn aad(id: StoreId, installation: &[u8; 32]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(DOMAIN.len() + 49);
    aad.extend_from_slice(DOMAIN);
    aad.push(1);
    aad.extend_from_slice(&id.0);
    aad.extend_from_slice(installation);
    aad
}

pub(super) fn seal(
    id: StoreId,
    key: &Secret32,
    state: &StoredPlaintext,
) -> Result<Vec<u8>, CoreError> {
    // Even before allocating ciphertext, the payload cannot fit beyond this bound.
    if state.payload.len() > FILE_LIMIT - 87 {
        return Err(CoreError::new(
            ErrorCode::StorageFull,
            "Account snapshot exceeds limit",
        ));
    }
    let mut nonce = [0; 12];
    getrandom::fill(&mut nonce)
        .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "Secure randomness unavailable"))?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_bytes()));
    // Reserve the authentication tag before copying plaintext, avoiding a growth
    // reallocation that could leave an unerased plaintext allocation behind.
    let mut ciphertext = Zeroizing::new(Vec::with_capacity(state.payload.len() + 16));
    ciphertext.extend_from_slice(&state.payload);
    cipher
        .encrypt_in_place(
            Nonce::from_slice(&nonce),
            &aad(id, &state.installation),
            &mut *ciphertext,
        )
        .map_err(|_| corrupt())?;
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(5)
        .and_then(|e| e.u8(1))
        .and_then(|e| e.bytes(&id.0))
        .and_then(|e| e.bytes(&state.installation))
        .and_then(|e| e.bytes(&nonce))
        .and_then(|e| e.bytes(&ciphertext))
        .map_err(|_| corrupt())?;
    let encoded = e.into_writer();
    if encoded.len() > FILE_LIMIT {
        return Err(CoreError::new(
            ErrorCode::StorageFull,
            "Account snapshot exceeds limit",
        ));
    }
    Ok(encoded)
}

pub(super) fn unseal(
    id: StoreId,
    key: &Secret32,
    encoded: &[u8],
) -> Result<StoredPlaintext, CoreError> {
    let mut d = minicbor::Decoder::new(encoded);
    if d.array().map_err(|_| corrupt())? != Some(5) || d.u8().map_err(|_| corrupt())? != 1 {
        return Err(corrupt());
    }
    if d.bytes().map_err(|_| corrupt())? != id.0 {
        return Err(corrupt());
    }
    let installation: [u8; 32] = d
        .bytes()
        .map_err(|_| corrupt())?
        .try_into()
        .map_err(|_| corrupt())?;
    let nonce: [u8; 12] = d
        .bytes()
        .map_err(|_| corrupt())?
        .try_into()
        .map_err(|_| corrupt())?;
    let ciphertext = d.bytes().map_err(|_| corrupt())?;
    if ciphertext.len() < 16 || d.position() != encoded.len() {
        return Err(corrupt());
    }
    let mut payload = Zeroizing::new(ciphertext.to_vec());
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_bytes()));
    cipher
        .decrypt_in_place(
            Nonce::from_slice(&nonce),
            &aad(id, &installation),
            &mut *payload,
        )
        .map_err(|_| corrupt())?;
    Ok(StoredPlaintext {
        installation,
        payload,
    })
}

/// Private seed state never implements Debug, Clone, or serde.
pub(crate) enum AccountState {
    Candidate(crate::account::DeviceKeys),
    /// Locally established keys; the publication intent is not public membership.
    Active(Box<LocalAccount>),
}
pub(crate) struct LocalAccount {
    pub(crate) keys: crate::account::AccountKeys,
    pub(crate) device: crate::account::DeviceKeys,
    pub(crate) cert: crate::account::InstallationCert,
    pub(crate) revision: u64,
    pub(crate) intent: crate::account::PublishIntent,
    pub(crate) records: crate::account::publication::RecordState,
    pub(crate) remote: Vec<crate::account::connection::RemoteRecord>,
    pub(crate) contacts: Vec<crate::account::contacts::Contact>,
}
impl AccountState {
    pub(crate) fn decode(stored: &StoredPlaintext) -> Result<Self, CoreError> {
        if stored.payload.first() == Some(&0x8a) {
            use crate::account::{
                AccountKeys, InstallationCert, PublishIntent, derive_device, verify_cert,
            };
            let mut d = minicbor::Decoder::new(&stored.payload);
            d.array().map_err(|_| corrupt())?;
            if d.u8().map_err(|_| corrupt())? != 1 || d.u8().map_err(|_| corrupt())? != 1 {
                return Err(corrupt());
            }
            let mut identity = Secret32::from([0; 32]);
            let bytes = d.bytes().map_err(|_| corrupt())?;
            if bytes.len() != 32 {
                return Err(corrupt());
            }
            identity.as_mut_bytes().copy_from_slice(bytes);
            let keys = AccountKeys::from_identity(identity);
            let mut seed = Secret32::from([0; 32]);
            let bytes = d.bytes().map_err(|_| corrupt())?;
            if bytes.len() != 32 {
                return Err(corrupt());
            }
            seed.as_mut_bytes().copy_from_slice(bytes);
            let device = derive_device(seed)?;
            let cert = InstallationCert::from_bytes(d.bytes().map_err(|_| corrupt())?)
                .map_err(|_| corrupt())?;
            verify_cert(&cert, keys.peer_id()).map_err(|_| corrupt())?;
            if cert.installation_id != device.installation_id()
                || cert.transport_public_key != device.transport_public_key()
            {
                return Err(corrupt());
            }
            let revision = d.u64().map_err(|_| corrupt())?;
            let intent = match d.u8().map_err(|_| corrupt())? {
                0 => PublishIntent::LocalCreate,
                1 => PublishIntent::Replacement,
                _ => return Err(corrupt()),
            };
            if revision == 0 {
                return Err(corrupt());
            }
            let records = crate::account::publication::RecordState::decode(
                d.bytes().map_err(|_| corrupt())?,
                keys.peer_id(),
            )
            .map_err(|_| corrupt())?;
            let remote = crate::account::connection::decode_cache(d.bytes().map_err(|_| corrupt())?, keys.peer_id()).map_err(|_| corrupt())?;
            let contacts = crate::account::contacts::decode(d.bytes().map_err(|_| corrupt())?, keys.peer_id()).map_err(|_| corrupt())?;
            let state = Self::Active(Box::new(LocalAccount {
                keys,
                device,
                cert,
                revision,
                intent,
                records,
                remote,
                contacts,
            }));
            if state.encode().payload != stored.payload
                || state.device().installation_id().0 != stored.installation
            {
                return Err(corrupt());
            }
            return Ok(state);
        }
        // Canonical definite CBOR [1, 0, bytes32]. Reject nonminimal integers,
        // indefinite arrays, extra fields, and trailing data.
        if stored.payload.len() != 37 || stored.payload[..5] != [0x83, 1, 0, 0x58, 32] {
            return Err(corrupt());
        }
        let mut seed = Secret32::from([0; 32]);
        seed.as_mut_bytes().copy_from_slice(&stored.payload[5..]);
        let keys = crate::account::derive_device(seed)?;
        if keys.installation_id().0 != stored.installation {
            return Err(corrupt());
        }
        Ok(Self::Candidate(keys))
    }
    pub(crate) fn encode(&self) -> StoredPlaintext {
        if let Self::Active(local) = self {
            let mut payload = Zeroizing::new(Vec::with_capacity(384));
            let mut e = minicbor::Encoder::new(&mut *payload);
            e.array(10)
                .unwrap()
                .u8(1)
                .unwrap()
                .u8(1)
                .unwrap()
                .bytes(local.keys.identity().as_bytes())
                .unwrap()
                .bytes(local.device.seed().as_bytes())
                .unwrap()
                .bytes(&local.cert.to_bytes())
                .unwrap()
                .u64(local.revision)
                .unwrap()
                .u8(match local.intent {
                    crate::account::PublishIntent::LocalCreate => 0,
                    crate::account::PublishIntent::Replacement => 1,
                })
                .unwrap();
            e.bytes(&local.records.encode()).unwrap();
            e.bytes(&crate::account::connection::encode_cache(&local.remote)).unwrap();
            e.bytes(&crate::account::contacts::encode(&local.contacts)).unwrap();
            return StoredPlaintext {
                installation: local.device.installation_id().0,
                payload,
            };
        }
        let Self::Candidate(keys) = self else {
            unreachable!()
        };
        let mut payload = Zeroizing::new(Vec::with_capacity(37));
        payload.extend_from_slice(&[0x83, 1, 0, 0x58, 32]);
        payload.extend_from_slice(keys.seed().as_bytes());
        StoredPlaintext {
            installation: keys.installation_id().0,
            payload,
        }
    }
    pub(crate) fn device(&self) -> &crate::account::DeviceKeys {
        match self {
            Self::Candidate(keys) => keys,
            Self::Active(local) => &local.device,
        }
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;
    use crate::account::{PublishIntent, derive_account, encode_recovery, recovery::local_account};

    #[test]
    fn established_codec_stores_signing_seed_not_recovery_entropy() {
        let entropy = Secret32::from([0; 32]);
        let keys = derive_account(&entropy).unwrap();
        let peer = keys.peer_id();
        let state = AccountState::Active(Box::new(
            local_account(keys, PublishIntent::Replacement, 7).unwrap(),
        ));
        let stored = state.encode();
        let mut d = minicbor::Decoder::new(&stored.payload);
        assert_eq!(d.array().unwrap(), Some(10));
        assert_eq!(d.u8().unwrap(), 1);
        assert_eq!(d.u8().unwrap(), 1);
        let AccountState::Active(local) = &state else {
            panic!()
        };
        assert_eq!(d.bytes().unwrap(), local.keys.identity().as_bytes());
        assert_eq!(d.bytes().unwrap(), local.device.seed().as_bytes());
        assert!(!stored.payload.windows(32).any(|w| w == entropy.as_bytes()));
        let phrase = Zeroizing::new(encode_recovery(&entropy).unwrap());
        assert!(
            !stored
                .payload
                .windows(phrase.len())
                .any(|w| w == phrase.as_bytes())
        );
        let AccountState::Active(restored) = AccountState::decode(&stored).unwrap() else {
            panic!()
        };
        assert_eq!(restored.keys.peer_id(), peer);
        assert_eq!(restored.revision, 7);
        assert_eq!(restored.intent, PublishIntent::Replacement);
    }

    #[test]
    fn established_codec_rejects_mismatch_noncanonical_and_unknown_state() {
        let state = AccountState::Active(Box::new(
            local_account(
                derive_account(&Secret32::from([0; 32])).unwrap(),
                PublishIntent::LocalCreate,
                1,
            )
            .unwrap(),
        ));
        let mut stored = state.encode();
        stored.installation[0] ^= 1;
        assert!(AccountState::decode(&stored).is_err());
        let mut stored = state.encode();
        // Change the signing seed while leaving its signed cert unchanged.
        stored.payload[5] ^= 1;
        assert!(AccountState::decode(&stored).is_err());
        let mut stored = state.encode();
        stored.payload.push(0);
        assert!(AccountState::decode(&stored).is_err());
        let mut stored = state.encode();
        stored.payload[2] = 2; // PendingImport is not a successful empty payload.
        assert!(AccountState::decode(&stored).is_err());
    }
}
