//! JWT signing for the FCM OAuth assertion (RS256) and the APNs provider token
//! (ES256). `jsonwebtoken` builds the token; the signature comes from `ring`,
//! which TLS already uses, through a [`CryptoProvider`] of our own. The crate's
//! built-in backends would add aws-lc or a second RustCrypto stack.

use jsonwebtoken::crypto::{CryptoProvider, JwtSigner, JwtVerifier, KeyUtils};
use jsonwebtoken::errors::{ErrorKind, Result as JwtResult};
use jsonwebtoken::signature::{Error as SignatureError, Signer};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header};
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, RSA_PKCS1_SHA256, RsaKeyPair,
};
use serde_json::Value;

/// Signs only; the proxy never verifies a JWT. JWK helpers are unused.
static RING: CryptoProvider = CryptoProvider {
    signer_factory,
    verifier_factory,
    key_utils: KeyUtils::new_unimplemented(),
};

/// A private key checked at start-up to sign with its algorithm.
pub(crate) struct JwtKey {
    alg: Algorithm,
    key: EncodingKey,
}

impl JwtKey {
    /// An RSA key in PEM, PKCS#1 or PKCS#8 (a Google service account's `private_key`).
    pub(crate) fn rsa_pem(pem: &str) -> Option<Self> {
        Self::checked(
            Algorithm::RS256,
            EncodingKey::from_rsa_pem(pem.as_bytes()).ok()?,
        )
    }

    /// A P-256 key in PKCS#8 PEM (an APNs `.p8`).
    pub(crate) fn p256_pem(pem: &str) -> Option<Self> {
        Self::checked(
            Algorithm::ES256,
            EncodingKey::from_ec_pem(pem.as_bytes()).ok()?,
        )
    }

    fn checked(alg: Algorithm, key: EncodingKey) -> Option<Self> {
        signer_factory(&alg, &key).ok()?;
        Some(JwtKey { alg, key })
    }

    /// A compact JWT. `header.alg` is set to this key's algorithm.
    pub(crate) fn sign(&self, mut header: Header, claims: &Value) -> Option<String> {
        header.alg = self.alg;
        // Fails only when already installed; nothing else installs one, as
        // this build enables no jsonwebtoken crypto feature.
        let _ = RING.install_default();
        jsonwebtoken::encode(&header, claims, &self.key).ok()
    }
}

struct Rs256(RsaKeyPair);
struct Es256(EcdsaKeyPair);

impl Signer<Vec<u8>> for Rs256 {
    fn try_sign(&self, msg: &[u8]) -> Result<Vec<u8>, SignatureError> {
        let mut sig = vec![0; self.0.public().modulus_len()];
        self.0
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), msg, &mut sig)
            .map_err(|_| SignatureError::new())?;
        Ok(sig)
    }
}

impl JwtSigner for Rs256 {
    fn algorithm(&self) -> Algorithm {
        Algorithm::RS256
    }
}

impl Signer<Vec<u8>> for Es256 {
    fn try_sign(&self, msg: &[u8]) -> Result<Vec<u8>, SignatureError> {
        self.0
            .sign(&SystemRandom::new(), msg)
            .map(|sig| sig.as_ref().to_vec())
            .map_err(|_| SignatureError::new())
    }
}

impl JwtSigner for Es256 {
    fn algorithm(&self) -> Algorithm {
        Algorithm::ES256
    }
}

/// `jsonwebtoken` hands RSA keys over as PKCS#1 DER and EC keys as PKCS#8 DER.
fn signer_factory(alg: &Algorithm, key: &EncodingKey) -> JwtResult<Box<dyn JwtSigner>> {
    match alg {
        Algorithm::RS256 => match RsaKeyPair::from_der(key.as_bytes()) {
            Ok(pair) => Ok(Box::new(Rs256(pair))),
            Err(_) => Err(ErrorKind::InvalidRsaKey(String::new()).into()),
        },
        Algorithm::ES256 => match EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            key.as_bytes(),
            &SystemRandom::new(),
        ) {
            Ok(pair) => Ok(Box::new(Es256(pair))),
            Err(_) => Err(ErrorKind::InvalidEcdsaKey.into()),
        },
        _ => Err(ErrorKind::UnsupportedAlgorithm.into()),
    }
}

fn verifier_factory(_: &Algorithm, _: &DecodingKey) -> JwtResult<Box<dyn JwtVerifier>> {
    Err(ErrorKind::UnsupportedAlgorithm.into())
}
