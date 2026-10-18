//! Only the encrypted envelope lives here. A5 owns the typed payload codec.
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
