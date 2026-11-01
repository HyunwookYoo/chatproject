//! MLS (OpenMLS 0.9) state for 1:1 conversations.
//!
//! Items here are consumed by the later M5 tasks (seal/open, membership, wire), hence the
//! blanket `dead_code` allowance until they land.
#![allow(dead_code)]

pub(crate) mod group;
pub(crate) mod identity;
pub(crate) mod kv;
pub(crate) mod membership;
pub(crate) mod payload;
pub(crate) mod wire;

use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;

/// Past epochs whose secrets a group keeps so late application messages still open.
pub(crate) const MAX_PAST_EPOCHS: usize = 5;
/// `SenderRatchetConfiguration::out_of_order_tolerance`.
pub(crate) const OUT_OF_ORDER: u32 = 200;
/// `SenderRatchetConfiguration::maximum_forward_distance`.
pub(crate) const MAX_FORWARD: u32 = 1000;

/// OpenMLS provider: RustCrypto for crypto and randomness, `KvStore` (binary, SQL-backed) for
/// state. One provider serves one unit of work and is dropped with its SQL transaction.
pub(crate) struct MlsProvider {
    crypto: RustCrypto,
    storage: kv::KvStore,
}

impl MlsProvider {
    pub(crate) fn new(storage: kv::KvStore) -> Self {
        Self {
            crypto: RustCrypto::default(),
            storage,
        }
    }
}

impl OpenMlsProvider for MlsProvider {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = kv::KvStore;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }
    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }
    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}
