//! Wire formats shared by the push proxy and its clients (spec 5.1, 9).
//!
//! A PushHandle is an AEAD envelope only the proxy can open. Request and response
//! bodies are JSON; every byte value in them is base64url without padding
//! ([`BASE64URL_NOPAD`]). Pure types and crypto: no I/O, no async.
//!
//! `Debug` output of every type here leaves out tokens, tags, handles and
//! ciphertexts, so a stray `{:?}` cannot put them in a log.

use std::fmt;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

/// The one encoding for byte values in push JSON.
pub use data_encoding::BASE64URL_NOPAD;

pub const HANDLE_LIFETIME_MS: u64 = 2_592_000_000;
pub const RENEW_AFTER_MS: u64 = 604_800_000;
pub const PUSH_CIPHER_MAX: usize = 2_900;
pub const MAX_PAYLOAD: usize = 4_096;
pub const MAX_ITEMS: usize = 100;
pub const MAX_BODY: usize = 65_536;
pub const MAX_TOKEN: usize = 512;
pub type Tag = [u8; 16];

const FORMAT_VERSION: u8 = 1;
const NONCE_LEN: usize = 24;
/// version | key_id | nonce. The first two bytes are the AAD.
const HEADER_LEN: usize = 2 + NONCE_LEN;
const AEAD_TAG_LEN: usize = 16;
/// platform | issued | expires | tag | token_len
const PLAIN_FIXED_LEN: usize = 1 + 8 + 8 + 16 + 2;
const MIN_HANDLE_LEN: usize = HEADER_LEN + PLAIN_FIXED_LEN + 1 + AEAD_TAG_LEN;
const MAX_HANDLE_LEN: usize = HEADER_LEN + PLAIN_FIXED_LEN + MAX_TOKEN + AEAD_TAG_LEN;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Fcm,
    Apns,
}

impl Platform {
    fn to_byte(self) -> u8 {
        match self {
            Platform::Fcm => 1,
            Platform::Apns => 2,
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Platform::Fcm),
            2 => Some(Platform::Apns),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HandlePlain {
    pub platform: Platform,
    pub token: String,
    pub tag: Tag,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

impl fmt::Debug for HandlePlain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandlePlain")
            .field("platform", &self.platform)
            .field("token", &Redacted)
            .field("tag", &Redacted)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandleError {
    /// Not a handle of a format this build knows.
    Malformed,
    /// The key id is neither the current nor the previous key.
    UnknownKey,
    /// Authentication failed: wrong key or altered bytes.
    Forged,
    /// The token is empty or longer than [`MAX_TOKEN`] bytes.
    TokenTooLong,
    /// The OS random source failed while sealing.
    Random,
}

impl fmt::Display for HandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HandleError::Malformed => "malformed handle",
            HandleError::UnknownKey => "unknown handle key",
            HandleError::Forged => "handle failed authentication",
            HandleError::TokenTooLong => "token empty or too long",
            HandleError::Random => "random source unavailable",
        })
    }
}

impl std::error::Error for HandleError {}

/// Seals `plain` under `key`. Handle bytes =
/// 1 (format version) | key_id u8 | nonce 24 | XChaCha20-Poly1305(plaintext),
/// AAD = the first two bytes. Plaintext =
/// platform u8 (1 fcm, 2 apns) | issued u64be | expires u64be | tag 16 | token_len u16be | token UTF-8.
pub fn seal_handle(
    key: &[u8; 32],
    key_id: u8,
    plain: &HandlePlain,
) -> Result<Vec<u8>, HandleError> {
    if plain.token.is_empty() || plain.token.len() > MAX_TOKEN {
        return Err(HandleError::TokenTooLong);
    }
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| HandleError::Random)?;
    seal_raw(key, key_id, &nonce, &encode_plain(plain))
}

/// Opens a handle with whichever of `keys` (`(key_id, key)`) its header names.
/// Does not check expiry; that is the caller's decision.
pub fn open_handle(keys: &[(u8, [u8; 32])], handle: &[u8]) -> Result<HandlePlain, HandleError> {
    if !(MIN_HANDLE_LEN..=MAX_HANDLE_LEN).contains(&handle.len()) || handle[0] != FORMAT_VERSION {
        return Err(HandleError::Malformed);
    }
    let (aad, rest) = handle.split_at(2);
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let key = keys
        .iter()
        .find(|(id, _)| *id == aad[1])
        .map(|(_, key)| key)
        .ok_or(HandleError::UnknownKey)?;
    let plaintext = XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| HandleError::Forged)?;
    decode_plain(&plaintext).ok_or(HandleError::Malformed)
}

fn seal_raw(
    key: &[u8; 32],
    key_id: u8,
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, HandleError> {
    let aad = [FORMAT_VERSION, key_id];
    // The cipher only refuses plaintexts near 256 GiB; a handle body is at most
    // PLAIN_FIXED_LEN + MAX_TOKEN bytes, so this error is not reachable.
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| HandleError::Malformed)?;
    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.extend_from_slice(&aad);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// The caller has checked `1 <= token.len() <= MAX_TOKEN`, so the length fits in u16.
fn encode_plain(plain: &HandlePlain) -> Vec<u8> {
    let token = plain.token.as_bytes();
    let mut out = Vec::with_capacity(PLAIN_FIXED_LEN + token.len());
    out.push(plain.platform.to_byte());
    out.extend_from_slice(&plain.issued_at_ms.to_be_bytes());
    out.extend_from_slice(&plain.expires_at_ms.to_be_bytes());
    out.extend_from_slice(&plain.tag);
    out.extend_from_slice(&(token.len() as u16).to_be_bytes());
    out.extend_from_slice(token);
    out
}

fn decode_plain(b: &[u8]) -> Option<HandlePlain> {
    let (fixed, token) = b.split_at_checked(PLAIN_FIXED_LEN)?;
    let platform = Platform::from_byte(fixed[0])?;
    let issued_at_ms = u64::from_be_bytes(fixed[1..9].try_into().ok()?);
    let expires_at_ms = u64::from_be_bytes(fixed[9..17].try_into().ok()?);
    let tag: Tag = fixed[17..33].try_into().ok()?;
    let token_len = usize::from(u16::from_be_bytes(fixed[33..35].try_into().ok()?));
    if token_len == 0 || token_len > MAX_TOKEN || token.len() != token_len {
        return None;
    }
    let token = std::str::from_utf8(token).ok()?.to_owned();
    Some(HandlePlain {
        platform,
        token,
        tag,
        issued_at_ms,
        expires_at_ms,
    })
}

/// `POST /handle` body. `tag` is base64url of the 16-byte [`Tag`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleRequest {
    pub platform: Platform,
    pub token: String,
    pub tag: String,
}

impl fmt::Debug for HandleRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandleRequest")
            .field("platform", &self.platform)
            .field("token", &Redacted)
            .field("tag", &Redacted)
            .finish()
    }
}

/// `POST /handle` reply. `handle` is base64url of the sealed bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleResponse {
    pub handle: String,
    pub expires_at_ms: u64,
}

impl fmt::Debug for HandleResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandleResponse")
            .field("handle", &Redacted)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// What one push asks for. `e` is base64url of the stripped MLS ciphertext.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PushItemKind {
    Cipher { e: String },
    Wake,
    Read,
}

impl fmt::Debug for PushItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushItemKind::Cipher { .. } => f.debug_struct("Cipher").field("e", &Redacted).finish(),
            PushItemKind::Wake => f.write_str("Wake"),
            PushItemKind::Read => f.write_str("Read"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushItem {
    pub handle: String,
    #[serde(flatten)]
    pub kind: PushItemKind,
}

impl fmt::Debug for PushItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushItem")
            .field("handle", &Redacted)
            .field("kind", &self.kind)
            .finish()
    }
}

/// `POST /push` body.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PushRequest {
    pub items: Vec<PushItem>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushResult {
    Ok,
    Gone,
    Limited,
    Bad,
}

/// `POST /push` reply: one result per item, same order and length as `items`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PushResponse {
    pub results: Vec<PushResult>,
}

struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_OLD: [u8; 32] = [0x11; 32];
    const KEY_NEW: [u8; 32] = [0x22; 32];

    fn plain() -> HandlePlain {
        HandlePlain {
            platform: Platform::Apns,
            token: "device-token-0123".to_owned(),
            tag: [7; 16],
            issued_at_ms: 1_700_000_000_000,
            expires_at_ms: 1_700_000_000_000 + HANDLE_LIFETIME_MS,
        }
    }

    #[test]
    fn handle_round_trips_with_current_key() {
        for platform in [Platform::Fcm, Platform::Apns] {
            let p = HandlePlain {
                platform,
                ..plain()
            };
            let handle = seal_handle(&KEY_NEW, 2, &p).unwrap();
            assert_eq!(handle[0], 1, "format version");
            assert_eq!(handle[1], 2, "key id");
            // 1 + 1 + 24 nonce + (1 + 8 + 8 + 16 + 2 + token) + 16 poly1305 tag
            assert_eq!(handle.len(), 26 + 35 + p.token.len() + 16);
            assert_eq!(open_handle(&[(2, KEY_NEW)], &handle).unwrap(), p);
        }
    }

    #[test]
    fn previous_key_still_opens() {
        let handle = seal_handle(&KEY_OLD, 1, &plain()).unwrap();
        let keys = [(2, KEY_NEW), (1, KEY_OLD)];
        assert_eq!(open_handle(&keys, &handle).unwrap(), plain());
    }

    #[test]
    fn wrong_key_tampered_and_truncated_are_refused() {
        let handle = seal_handle(&KEY_NEW, 2, &plain()).unwrap();

        assert_eq!(
            open_handle(&[(2, KEY_OLD)], &handle),
            Err(HandleError::Forged)
        );

        for i in 2..handle.len() {
            let mut bad = handle.clone();
            bad[i] ^= 0x01;
            assert_eq!(
                open_handle(&[(2, KEY_NEW)], &bad),
                Err(HandleError::Forged),
                "byte {i}"
            );
        }

        // The header is authenticated: the same key under another id does not open it.
        let mut relabeled = handle.clone();
        relabeled[1] = 3;
        assert_eq!(
            open_handle(&[(2, KEY_NEW), (3, KEY_NEW)], &relabeled),
            Err(HandleError::Forged)
        );

        for len in 0..26 {
            assert_eq!(
                open_handle(&[(2, KEY_NEW)], &handle[..len]),
                Err(HandleError::Malformed),
                "len {len}"
            );
        }
        let mut v2 = handle.clone();
        v2[0] = 2;
        assert_eq!(
            open_handle(&[(2, KEY_NEW)], &v2),
            Err(HandleError::Malformed)
        );

        assert_eq!(
            open_handle(&[(1, KEY_OLD)], &handle),
            Err(HandleError::UnknownKey)
        );
        assert_eq!(open_handle(&[], &handle), Err(HandleError::UnknownKey));
    }

    #[test]
    fn token_over_512_bytes_is_refused_on_seal() {
        let max = HandlePlain {
            token: "a".repeat(MAX_TOKEN),
            ..plain()
        };
        let handle = seal_handle(&KEY_NEW, 2, &max).unwrap();
        assert_eq!(open_handle(&[(2, KEY_NEW)], &handle).unwrap(), max);

        let over = HandlePlain {
            token: "a".repeat(MAX_TOKEN + 1),
            ..plain()
        };
        assert_eq!(
            seal_handle(&KEY_NEW, 2, &over),
            Err(HandleError::TokenTooLong)
        );

        // 171 three-byte chars = 513 bytes: the limit is in bytes, not chars.
        let wide = HandlePlain {
            token: "\u{d55c}".repeat(171),
            ..plain()
        };
        assert_eq!(
            seal_handle(&KEY_NEW, 2, &wide),
            Err(HandleError::TokenTooLong)
        );

        let empty = HandlePlain {
            token: String::new(),
            ..plain()
        };
        assert_eq!(
            seal_handle(&KEY_NEW, 2, &empty),
            Err(HandleError::TokenTooLong)
        );
    }

    #[test]
    fn push_request_json_shape() {
        let json = r#"{"items":[{"handle":"aGFuZGxl","kind":"cipher","e":"Y2lwaGVy"},{"handle":"aDI","kind":"wake"},{"handle":"aDM","kind":"read"}]}"#;
        let req: PushRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            PushRequest {
                items: vec![
                    PushItem {
                        handle: BASE64URL_NOPAD.encode(b"handle"),
                        kind: PushItemKind::Cipher {
                            e: BASE64URL_NOPAD.encode(b"cipher")
                        },
                    },
                    PushItem {
                        handle: "aDI".to_owned(),
                        kind: PushItemKind::Wake
                    },
                    PushItem {
                        handle: "aDM".to_owned(),
                        kind: PushItemKind::Read
                    },
                ],
            }
        );
        assert_eq!(serde_json::to_string(&req).unwrap(), json);

        let resp = PushResponse {
            results: vec![
                PushResult::Ok,
                PushResult::Gone,
                PushResult::Limited,
                PushResult::Bad,
            ],
        };
        let resp_json = r#"{"results":["ok","gone","limited","bad"]}"#;
        assert_eq!(serde_json::to_string(&resp).unwrap(), resp_json);
        assert_eq!(
            serde_json::from_str::<PushResponse>(resp_json).unwrap(),
            resp
        );

        let hreq = HandleRequest {
            platform: Platform::Fcm,
            token: "tok".to_owned(),
            tag: BASE64URL_NOPAD.encode(&[7; 16]),
        };
        let hreq_json = r#"{"platform":"fcm","token":"tok","tag":"BwcHBwcHBwcHBwcHBwcHBw"}"#;
        assert_eq!(serde_json::to_string(&hreq).unwrap(), hreq_json);
        assert_eq!(
            serde_json::from_str::<HandleRequest>(hreq_json).unwrap(),
            hreq
        );

        let hresp = HandleResponse {
            handle: "aGFuZGxl".to_owned(),
            expires_at_ms: 5,
        };
        let hresp_json = r#"{"handle":"aGFuZGxl","expires_at_ms":5}"#;
        assert_eq!(serde_json::to_string(&hresp).unwrap(), hresp_json);
        assert_eq!(
            serde_json::from_str::<HandleResponse>(hresp_json).unwrap(),
            hresp
        );

        assert_eq!(serde_json::to_string(&Platform::Apns).unwrap(), r#""apns""#);
        assert!(
            serde_json::from_str::<PushRequest>(r#"{"items":[{"handle":"a","kind":"x"}]}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<PushRequest>(r#"{"items":[{"handle":"a","kind":"cipher"}]}"#)
                .is_err()
        );
    }

    #[test]
    fn two_seals_of_same_plain_differ() {
        let a = seal_handle(&KEY_NEW, 2, &plain()).unwrap();
        let b = seal_handle(&KEY_NEW, 2, &plain()).unwrap();
        assert_ne!(a[2..26], b[2..26], "nonce");
        assert_ne!(a, b);
    }

    #[test]
    fn malformed_plaintext_under_a_valid_seal_is_refused() {
        // Only the proxy can seal, so this guards against a buggy or future sealer.
        let nonce = [9; NONCE_LEN];
        let good = encode_plain(&plain());
        let cases: [Vec<u8>; 5] = [
            {
                let mut p = good.clone();
                p[0] = 3; // unknown platform
                p
            },
            good[..good.len() - 1].to_vec(),  // token cut short
            [good.as_slice(), b"x"].concat(), // trailing byte
            {
                let mut p = good.clone();
                let n = p.len();
                p[n - 1] = 0xff; // not UTF-8
                p
            },
            {
                let mut p = good[..PLAIN_FIXED_LEN - 2].to_vec();
                p.extend_from_slice(&0u16.to_be_bytes()); // empty token
                p
            },
        ];
        for (i, body) in cases.iter().enumerate() {
            let handle = seal_raw(&KEY_NEW, 2, &nonce, body).unwrap();
            assert_eq!(
                open_handle(&[(2, KEY_NEW)], &handle),
                Err(HandleError::Malformed),
                "case {i}"
            );
        }
        let handle = seal_raw(&KEY_NEW, 2, &nonce, &good).unwrap();
        assert_eq!(open_handle(&[(2, KEY_NEW)], &handle).unwrap(), plain());
    }

    #[test]
    fn debug_output_hides_secrets() {
        let p = plain();
        let shown = format!(
            "{p:?} {:?} {:?} {:?}",
            HandleRequest {
                platform: Platform::Fcm,
                token: p.token.clone(),
                tag: "c2VjcmV0dGFn".to_owned()
            },
            HandleResponse {
                handle: "c2VjcmV0aGFuZGxl".to_owned(),
                expires_at_ms: 1
            },
            PushRequest {
                items: vec![PushItem {
                    handle: "c2VjcmV0aGFuZGxl".to_owned(),
                    kind: PushItemKind::Cipher {
                        e: "c2VjcmV0Y2lwaGVy".to_owned()
                    },
                }],
            },
        );
        for secret in [
            p.token.as_str(),
            "c2VjcmV0dGFn",
            "c2VjcmV0aGFuZGxl",
            "c2VjcmV0Y2lwaGVy",
            "7, 7",
        ] {
            assert!(!shown.contains(secret), "{secret} leaked");
        }
    }
}
