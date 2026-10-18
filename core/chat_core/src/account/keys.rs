use std::borrow::Cow;

use bip39::{Language, Mnemonic};
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use super::{InstallationId, PeerId, Secret32};
use crate::{CoreError, ErrorCode};

const ACCOUNT_SALT: &[u8] = b"ChatProject/account-keys/v1";
const DEVICE_SALT: &[u8] = b"ChatProject/device-keys/v1";
const MAX_RECOVERY_INPUT_BYTES: usize = 4096;

pub struct AccountKeys {
    identity: Secret32,
    peer: PeerId,
}

impl AccountKeys {
    pub fn peer_id(&self) -> PeerId {
        self.peer
    }

    // Consumed by the later account signing/storage tasks, never exported to FFI.
    #[allow(dead_code)]
    pub(crate) fn identity(&self) -> &Secret32 {
        &self.identity
    }
}

pub struct DeviceKeys {
    seed: Secret32,
    transport: Secret32,
    installation: Secret32,
    installation_id: InstallationId,
}

impl DeviceKeys {
    pub fn installation_id(&self) -> InstallationId {
        self.installation_id
    }

    pub fn transport_public_key(&self) -> [u8; 32] {
        public_key(&self.transport)
    }

    // Consumed by the later account signing/storage tasks, never exported to FFI.
    #[allow(dead_code)]
    pub(crate) fn seed(&self) -> &Secret32 {
        &self.seed
    }

    #[allow(dead_code)]
    pub(crate) fn transport(&self) -> &Secret32 {
        &self.transport
    }

    #[allow(dead_code)]
    pub(crate) fn installation(&self) -> &Secret32 {
        &self.installation
    }
}

fn invalid_recovery() -> CoreError {
    CoreError::new(ErrorCode::InvalidRecovery, "invalid recovery phrase")
}

pub fn encode_recovery(entropy: &Secret32) -> Result<String, CoreError> {
    let mnemonic = Mnemonic::from_entropy_in(Language::English, entropy.as_bytes())
        .map_err(|_| invalid_recovery())?;
    Ok(mnemonic.to_string())
}

pub fn decode_recovery(input: &str) -> Result<Secret32, CoreError> {
    if input.len() > MAX_RECOVERY_INPUT_BYTES {
        return Err(invalid_recovery());
    }
    let mut normalized = Cow::Borrowed(input);
    Mnemonic::normalize_utf8_cow(&mut normalized);
    let mut normalized = Zeroizing::new(normalized.into_owned());
    normalized.make_ascii_lowercase();
    if normalized.split_whitespace().count() != 24 {
        return Err(invalid_recovery());
    }
    let mnemonic = Mnemonic::parse_in(Language::English, normalized.as_str())
        .map_err(|_| invalid_recovery())?;
    let entropy = Zeroizing::new(mnemonic.to_entropy());
    let mut secret = Secret32::from([0; 32]);
    secret.as_mut_bytes().copy_from_slice(&entropy);
    Ok(secret)
}

fn derive(seed: &Secret32, salt: &[u8], info: &[u8]) -> Secret32 {
    let mut output = Secret32::from([0; 32]);
    Hkdf::<Sha256>::new(Some(salt), seed.as_bytes())
        .expand(info, output.as_mut_bytes())
        .expect("32-byte output is valid for HKDF-SHA256");
    output
}

fn public_key(seed: &Secret32) -> [u8; 32] {
    SigningKey::from_bytes(seed.as_bytes())
        .verifying_key()
        .to_bytes()
}

pub fn derive_account(entropy: &Secret32) -> Result<AccountKeys, CoreError> {
    let identity = derive(entropy, ACCOUNT_SALT, b"identity-ed25519");
    let peer = PeerId(public_key(&identity));
    Ok(AccountKeys { identity, peer })
}

pub fn derive_device(seed: Secret32) -> Result<DeviceKeys, CoreError> {
    let transport = derive(&seed, DEVICE_SALT, b"transport-ed25519");
    let installation = derive(&seed, DEVICE_SALT, b"installation-ed25519");
    let installation_id = InstallationId(public_key(&installation));
    Ok(DeviceKeys {
        seed,
        transport,
        installation,
        installation_id,
    })
}

pub fn random_secret() -> Result<Secret32, CoreError> {
    let mut secret = Secret32::from([0; 32]);
    getrandom::fill(secret.as_mut_bytes())
        .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "OS randomness unavailable"))?;
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn fixed_hkdf_seed_outputs() {
        let account = derive_account(&Secret32::from([0; 32])).unwrap();
        assert_eq!(
            hex(account.identity().as_bytes()),
            "3c5a13c879ac3255f7e9259772c0f6f0e8d8cac255d81a9ee81078da74fd6a01"
        );
        let device = derive_device(Secret32::from([0xff; 32])).unwrap();
        assert_eq!(device.seed().as_bytes(), &[0xff; 32]);
        assert_eq!(
            hex(device.transport().as_bytes()),
            "5a57439114b6680ab0d408ea9a8c121f44b9808dfdbfd0038a305d91ef2e7d1c"
        );
        assert_eq!(
            hex(device.installation().as_bytes()),
            "074f0dc1fc5fe8dea217a5a8016f56d861d96d6e283ee85b30aa407ce10587c1"
        );
    }
}
