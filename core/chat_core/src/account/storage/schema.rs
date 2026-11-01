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
    CandidatePinned(Box<crate::account::pair::transaction::CandidatePinned>),
    PendingImport(Box<crate::account::pair::transaction::PendingImport>),
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
    pub(crate) pairs: crate::account::pair::transaction::PairState,
}
impl AccountState {
    /// Copy already-validated actor state for a proposed update. Durable input
    /// still uses the full decoder; keys and zeroizing payloads remain owned.
    pub(crate) fn copy_for_update(&self) -> Self {
        use crate::account::pair::transaction::{CandidatePinned, PendingImport};
        match self {
            Self::Candidate(device) => Self::Candidate(device.copy_for_update()),
            Self::CandidatePinned(p) => Self::CandidatePinned(Box::new(CandidatePinned {
                device: p.device.copy_for_update(),
                revision: p.revision,
                pin: p.pin,
                envelope: p.envelope,
            })),
            Self::PendingImport(p) => Self::PendingImport(Box::new(PendingImport {
                device: p.device.copy_for_update(),
                revision: p.revision,
                pin: p.pin,
                payload: p.payload.clone(),
                receipt: p.receipt.clone(),
                records: p.records.clone(),
            })),
            Self::Active(local) => Self::Active(Box::new(LocalAccount {
                keys: local.keys.copy_for_update(),
                device: local.device.copy_for_update(),
                cert: local.cert.clone(),
                revision: local.revision,
                intent: local.intent,
                records: local.records.clone(),
                remote: local.remote.clone(),
                contacts: local.contacts.clone(),
                pairs: local.pairs.copy_for_update(),
            })),
        }
    }
    fn decode_pending(stored: &StoredPlaintext) -> Result<Self, CoreError> {
        use crate::account::{
            derive_device,
            pair::{
                transaction::{CandidatePinned, ExistingPin, PendingImport},
                transfer::{Envelope, Payload, Receipt, hash},
            },
            publication::RecordState,
        };
        let mut d = minicbor::Decoder::new(&stored.payload);
        crate::account::wire::array(&mut d, 8)?;
        if d.u8().map_err(|_| corrupt())? != 1 {
            return Err(corrupt());
        }
        let tag = d.u8().map_err(|_| corrupt())?;
        let mut seed = Secret32::from([0; 32]);
        let raw = d.bytes().map_err(|_| corrupt())?;
        if raw.len() != 32 {
            return Err(corrupt());
        }
        seed.as_mut_bytes().copy_from_slice(raw);
        let device = derive_device(seed)?;
        let revision = d.u64().map_err(|_| corrupt())?;
        if revision == 0 {
            return Err(corrupt());
        }
        let pin = ExistingPin::decode(&mut d)?;
        let state = match tag {
            2 => {
                let tx = crate::account::TxId(crate::account::wire::bytes(&mut d)?);
                let hash = crate::account::wire::bytes(&mut d)?;
                let total = d.u64().map_err(|_| corrupt())?;
                if total == 0 || total > FILE_LIMIT as u64 {
                    return Err(corrupt());
                }
                Self::CandidatePinned(Box::new(CandidatePinned {
                    device,
                    revision,
                    pin,
                    envelope: Envelope {
                        tx,
                        total: total as usize,
                        hash,
                    },
                }))
            }
            3 => {
                let payload = Zeroizing::new(d.bytes().map_err(|_| corrupt())?.to_vec());
                let p = Payload::decode(&payload)?;
                let receipt = Receipt::decode(d.bytes().map_err(|_| corrupt())?)?;
                let records = RecordState::decode(d.bytes().map_err(|_| corrupt())?, pin.peer)?;
                if p.keys.peer_id() != pin.peer
                    || !p.matches_device(&device)
                    || receipt.tx != p.tx
                    || receipt.payload_hash != hash(&payload)
                    || receipt.installation != device.installation_id()
                    || records.generation != p.generation
                    || !records.pending.is_empty()
                {
                    return Err(corrupt());
                }
                Self::PendingImport(Box::new(PendingImport {
                    device,
                    revision,
                    pin,
                    payload,
                    receipt,
                    records,
                }))
            }
            _ => return Err(corrupt()),
        };
        if state.encode().payload != stored.payload
            || state.device().installation_id().0 != stored.installation
        {
            return Err(corrupt());
        }
        Ok(state)
    }
    fn encode_pending(&self) -> StoredPlaintext {
        // Reserve before copying seeds, including oversized snapshots that Store
        // will reject. A fixed FILE_LIMIT reserve could reallocate secret bytes.
        let records = match self {
            Self::PendingImport(p) => p.records.encode(),
            _ => Vec::new(),
        };
        let capacity = match self {
            Self::PendingImport(p) => p.payload.len() + records.len() + 512,
            _ => 512,
        };
        let mut e = minicbor::Encoder::new(Vec::with_capacity(capacity));
        e.array(8).unwrap().u8(1).unwrap();
        match self {
            Self::CandidatePinned(p) => {
                e.u8(2)
                    .unwrap()
                    .bytes(p.device.seed().as_bytes())
                    .unwrap()
                    .u64(p.revision)
                    .unwrap();
                p.pin.encode(&mut e);
                e.bytes(&p.envelope.tx.0)
                    .unwrap()
                    .bytes(&p.envelope.hash)
                    .unwrap()
                    .u64(p.envelope.total as u64)
                    .unwrap();
            }
            Self::PendingImport(p) => {
                e.u8(3)
                    .unwrap()
                    .bytes(p.device.seed().as_bytes())
                    .unwrap()
                    .u64(p.revision)
                    .unwrap();
                p.pin.encode(&mut e);
                e.bytes(&p.payload)
                    .unwrap()
                    .bytes(&p.receipt.bytes())
                    .unwrap()
                    .bytes(&records)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        StoredPlaintext {
            installation: self.device().installation_id().0,
            payload: Zeroizing::new(e.into_writer()),
        }
    }
    pub(crate) fn decode(stored: &StoredPlaintext) -> Result<Self, CoreError> {
        if stored.payload.first() == Some(&0x88) {
            return Self::decode_pending(stored).map_err(|_| corrupt());
        }
        if stored.payload.first() == Some(&0x8b) {
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
            let pairs = crate::account::pair::transaction::PairState::decode(d.bytes().map_err(|_| corrupt())?, keys.peer_id(), &device, records.pending.len()).map_err(|_| corrupt())?;
            let state = Self::Active(Box::new(LocalAccount {
                keys,
                device,
                cert,
                revision,
                intent,
                records,
                remote,
                contacts,
                pairs,
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
        if matches!(self, Self::CandidatePinned(_) | Self::PendingImport(_)) {
            return self.encode_pending();
        }
        if let Self::Active(local) = self {
            let records = local.records.encode();
            let remote = crate::account::connection::encode_cache(&local.remote);
            let contacts = crate::account::contacts::encode(&local.contacts);
            let pairs = local.pairs.encode();
            let capacity = records.len() + remote.len() + contacts.len() + pairs.len() + 512;
            let mut payload = Zeroizing::new(Vec::with_capacity(capacity));
            let mut e = minicbor::Encoder::new(&mut *payload);
            e.array(11)
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
            e.bytes(&records).unwrap();
            e.bytes(&remote).unwrap();
            e.bytes(&contacts).unwrap();
            e.bytes(&pairs).unwrap();
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
            Self::CandidatePinned(p) => &p.device,
            Self::PendingImport(p) => &p.device,
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
        assert_eq!(d.array().unwrap(), Some(11));
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
    fn valid_contacts_then_invalid_pair_state_rejects_with_owned_contact_drop() {
        let mut local = local_account(
            derive_account(&Secret32::from([0; 32])).unwrap(),
            PublishIntent::LocalCreate,
            1,
        )
        .unwrap();
        local.contacts.push(crate::account::contacts::Contact {
            peer: crate::account::PeerId([5; 32]),
            alias: "private alias".into(),
            verified_digest: None,
            known_installation: None,
            new_device: false,
        });
        let state = AccountState::Active(Box::new(local));
        let mut stored = state.encode();
        assert!(AccountState::decode(&stored).is_ok());
        assert!(std::mem::needs_drop::<crate::account::contacts::Contact>());
        // Valid contacts are decoded before this last pair-state version fails.
        let len = stored.payload.len();
        assert_eq!(&stored.payload[len - 4..], &[0x83, 1, 0x80, 0xf6]);
        stored.payload[len - 3] = 2;
        assert!(AccountState::decode(&stored).is_err());
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

#[cfg(all(test, feature = "test-hooks", debug_assertions))]
mod update_copy_tests {
    use super::*;
    use subtle::ConstantTimeEq;
    #[test]
    fn every_phase_copies_exact_bytes_without_aliasing_rich_state() {
        for stored in crate::account::storage::migration_fixtures::migration_states()
            .into_iter()
            .flatten()
        {
            let original = AccountState::decode(&stored).unwrap();
            let mut copied = original.copy_for_update();
            let encoded = copied.encode();
            assert_eq!(encoded.installation, stored.installation);
            assert!(bool::from(
                encoded.payload.as_slice().ct_eq(stored.payload.as_slice())
            ));
            match &mut copied {
                AccountState::Candidate(device) => {
                    *device = crate::account::derive_device(Secret32::from([99; 32])).unwrap()
                }
                AccountState::CandidatePinned(p) => p.envelope.hash[0] ^= 1,
                AccountState::PendingImport(p) => {
                    p.payload[0] ^= 1;
                    p.records.observed_seq += 1;
                }
                AccountState::Active(local) => {
                    local.contacts[0].alias.push('x');
                    local.pairs.existing[0].payload[0] ^= 1;
                    local.records.observed_seq += 1;
                    local.keys =
                        crate::account::AccountKeys::from_identity(Secret32::from([99; 32]));
                }
            }
            assert!(bool::from(
                original
                    .encode()
                    .payload
                    .as_slice()
                    .ct_eq(stored.payload.as_slice())
            ));
            assert!(!bool::from(
                copied
                    .encode()
                    .payload
                    .as_slice()
                    .ct_eq(stored.payload.as_slice())
            ));
        }
    }
}
