//! Wire formats shared by the queue server and its clients (M8 spec 9.2).
//!
//! Every command is about one queue and carries its own Ed25519 signature over
//! [`signing_bytes`]. Request and response bodies are JSON; every byte value in
//! them is base64url without padding ([`BASE64URL_NOPAD`]). A request is
//! `{"items": [...]}` and its reply is `{"results": [...]}`, one result per item
//! in the same order. Pure types and crypto: no I/O, no async.
//!
//! `Debug` output of every type here leaves out queue ids, keys, signatures and
//! ciphertexts, so a stray `{:?}` cannot put them in a log.

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The one encoding for byte values in queue JSON.
pub use data_encoding::BASE64URL_NOPAD;

pub const MAX_CT: usize = 65_536;
pub const MAX_UNACKED: usize = 256;
pub const MAX_ITEMS: usize = 100;
/// A `SEND` batch: five devices' 64 KiB ciphertexts in base64.
pub const MAX_BODY: usize = 1_048_576;
pub const ENTRY_TTL_MS: u64 = 86_400_000;
pub const RETIRE_MS: u64 = 172_800_000;
pub const IDLE_DELETE_MS: u64 = 2_592_000_000;
pub const ROTATE_MS: u64 = 604_800_000;
pub const TS_SKEW_MS: u64 = 600_000;
/// Chosen by the receiving device: 32 random bytes.
pub type QueueId = [u8; 32];

const DOMAIN: &[u8] = b"chatproject-queue-v1";

/// The five commands. Each names the `body` it signs, so the client and the
/// server hash the same bytes. A body is the command's own raw field bytes,
/// decoded from base64 where the JSON carries base64.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandName {
    /// Signed by the recipient key. Body = `sender_pub` (32 bytes). The
    /// signature is checked under `recipient_pub`, and Ed25519 hashes the
    /// verifying key into every signature, so both keys are bound.
    Create,
    /// Signed by the sender key. Body = `ct`.
    Send,
    /// Signed by the recipient key. Body = `since_seq` u64be.
    Recv,
    /// Signed by the recipient key. Body = `upto_seq` u64be.
    Ack,
    /// Signed by the recipient key. Body = empty.
    Delete,
}

impl CommandName {
    /// The `POST` path for a batch of this command.
    pub fn path(self) -> &'static str {
        match self {
            CommandName::Create => "/q/create",
            CommandName::Send => "/q/send",
            CommandName::Recv => "/q/recv",
            CommandName::Ack => "/q/ack",
            CommandName::Delete => "/q/delete",
        }
    }

    fn name(self) -> &'static [u8] {
        match self {
            CommandName::Create => b"CREATE",
            CommandName::Send => b"SEND",
            CommandName::Recv => b"RECV",
            CommandName::Ack => b"ACK",
            CommandName::Delete => b"DELETE",
        }
    }
}

/// The bytes a command's signature covers =
/// "chatproject-queue-v1" 0x00 | command name | 0x00 | queue_id 32 | ts_ms u64be | SHA-256(body) 32.
/// See [`CommandName`] for each command's `body`.
pub fn signing_bytes(cmd: CommandName, queue_id: &QueueId, ts_ms: u64, body: &[u8]) -> Vec<u8> {
    let name = cmd.name();
    let mut out = Vec::with_capacity(DOMAIN.len() + 1 + name.len() + 1 + 32 + 8 + 32);
    out.extend_from_slice(DOMAIN);
    out.push(0);
    out.extend_from_slice(name);
    out.push(0);
    out.extend_from_slice(queue_id);
    out.extend_from_slice(&ts_ms.to_be_bytes());
    out.extend_from_slice(&Sha256::digest(body));
    out
}

/// The Ed25519 public key of `secret`, as `CREATE` sends it.
pub fn public_key(secret: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(secret).verifying_key().to_bytes()
}

pub fn sign(secret: &[u8; 32], bytes: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(secret).sign(bytes).to_bytes()
}

/// Strict Ed25519 verification: a key that is not a point, a weak (small-order)
/// key, or a non-canonical signature is `false`, never a panic.
pub fn verify(public: &[u8; 32], bytes: &[u8], sig: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(public)
        .and_then(|key| key.verify_strict(bytes, &Signature::from_bytes(sig)))
        .is_ok()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// Not base64url without padding.
    Base64,
    /// A key, id or signature of the wrong length, or a ciphertext over [`MAX_CT`].
    Length,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DecodeError::Base64 => "not base64url",
            DecodeError::Length => "wrong length",
        })
    }
}

impl std::error::Error for DecodeError {}

/// Decodes a fixed-size value: a [`QueueId`] or key (`N = 32`) or a signature (`N = 64`).
/// The length is checked before any byte is decoded.
pub fn decode_array<const N: usize>(s: &str) -> Result<[u8; N], DecodeError> {
    let len = BASE64URL_NOPAD
        .decode_len(s.len())
        .map_err(|_| DecodeError::Base64)?;
    if len != N {
        return Err(DecodeError::Length);
    }
    let mut out = [0u8; N];
    BASE64URL_NOPAD
        .decode_mut(s.as_bytes(), &mut out)
        .map_err(|_| DecodeError::Base64)?;
    Ok(out)
}

/// Decodes a ciphertext of at most [`MAX_CT`] bytes. The length is checked before
/// anything is allocated.
pub fn decode_ct(s: &str) -> Result<Vec<u8>, DecodeError> {
    let len = BASE64URL_NOPAD
        .decode_len(s.len())
        .map_err(|_| DecodeError::Base64)?;
    if len > MAX_CT {
        return Err(DecodeError::Length);
    }
    BASE64URL_NOPAD
        .decode(s.as_bytes())
        .map_err(|_| DecodeError::Base64)
}

/// `POST /q/create` item. `recipient_pub` and `sender_pub` are base64url of 32-byte
/// Ed25519 public keys.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateItem {
    pub queue_id: String,
    pub recipient_pub: String,
    pub sender_pub: String,
    pub ts: u64,
    pub sig: String,
}

impl fmt::Debug for CreateItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateItem")
            .field("queue_id", &Redacted)
            .field("recipient_pub", &Redacted)
            .field("sender_pub", &Redacted)
            .field("ts", &self.ts)
            .field("sig", &Redacted)
            .finish()
    }
}

/// `POST /q/send` item. `ct` is base64url of at most [`MAX_CT`] bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendItem {
    pub queue_id: String,
    pub ct: String,
    pub ts: u64,
    pub sig: String,
}

impl fmt::Debug for SendItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendItem")
            .field("queue_id", &Redacted)
            .field("ct", &Redacted)
            .field("ts", &self.ts)
            .field("sig", &Redacted)
            .finish()
    }
}

/// `POST /q/recv` item: entries after `since_seq`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecvItem {
    pub queue_id: String,
    pub since_seq: u64,
    pub ts: u64,
    pub sig: String,
}

impl fmt::Debug for RecvItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvItem")
            .field("queue_id", &Redacted)
            .field("since_seq", &self.since_seq)
            .field("ts", &self.ts)
            .field("sig", &Redacted)
            .finish()
    }
}

/// `POST /q/ack` item: drops entries up to and including `upto_seq`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckItem {
    pub queue_id: String,
    pub upto_seq: u64,
    pub ts: u64,
    pub sig: String,
}

impl fmt::Debug for AckItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AckItem")
            .field("queue_id", &Redacted)
            .field("upto_seq", &self.upto_seq)
            .field("ts", &self.ts)
            .field("sig", &Redacted)
            .finish()
    }
}

/// `POST /q/delete` item.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteItem {
    pub queue_id: String,
    pub ts: u64,
    pub sig: String,
}

impl fmt::Debug for DeleteItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeleteItem")
            .field("queue_id", &Redacted)
            .field("ts", &self.ts)
            .field("sig", &Redacted)
            .finish()
    }
}

/// Body of every `/q/*` request, at most [`MAX_ITEMS`] items.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QueueRequest<T> {
    pub items: Vec<T>,
}

/// What one item got. A bad signature or a `ts` outside [`TS_SKEW_MS`] makes
/// only that item `bad`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum ItemResult {
    Ok,
    Exists,
    Gone,
    Full,
    Limited,
    Bad,
    /// A `RECV` answer.
    Entries {
        entries: Vec<Entry>,
    },
}

/// One stored ciphertext. `ts` is when the server received it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub ct: String,
    pub ts: u64,
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("seq", &self.seq)
            .field("ct", &Redacted)
            .field("ts", &self.ts)
            .finish()
    }
}

/// Reply to every `/q/*` request: one result per item, same order and length as `items`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QueueResponse {
    pub results: Vec<ItemResult>,
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

    const RECIPIENT: [u8; 32] = [0x11; 32];
    const SENDER: [u8; 32] = [0x22; 32];
    const QUEUE: QueueId = [0x33; 32];
    const TS: u64 = 1_700_000_000_000;

    #[test]
    fn sign_then_verify() {
        let bytes = signing_bytes(CommandName::Send, &QUEUE, TS, b"ciphertext");
        let sig = sign(&SENDER, &bytes);
        assert!(verify(&public_key(&SENDER), &bytes, &sig));
        // Deterministic: the same key and bytes give the same signature.
        assert_eq!(sig, sign(&SENDER, &bytes));
    }

    #[test]
    fn other_key_command_queue_ts_or_body_fails() {
        let bytes = signing_bytes(CommandName::Recv, &QUEUE, TS, &7u64.to_be_bytes());
        let sig = sign(&RECIPIENT, &bytes);
        let public = public_key(&RECIPIENT);
        assert!(verify(&public, &bytes, &sig));

        assert!(!verify(&public_key(&SENDER), &bytes, &sig), "key");

        for other in [
            CommandName::Create,
            CommandName::Send,
            CommandName::Ack,
            CommandName::Delete,
        ] {
            let changed = signing_bytes(other, &QUEUE, TS, &7u64.to_be_bytes());
            assert!(!verify(&public, &changed, &sig), "command {other:?}");
        }

        let mut queue = QUEUE;
        queue[31] ^= 1;
        let changed = signing_bytes(CommandName::Recv, &queue, TS, &7u64.to_be_bytes());
        assert!(!verify(&public, &changed, &sig), "queue_id");

        let changed = signing_bytes(CommandName::Recv, &QUEUE, TS + 1, &7u64.to_be_bytes());
        assert!(!verify(&public, &changed, &sig), "ts");

        let changed = signing_bytes(CommandName::Recv, &QUEUE, TS, &8u64.to_be_bytes());
        assert!(!verify(&public, &changed, &sig), "body");
    }

    #[test]
    fn signing_bytes_layout() {
        let body = b"body";
        let bytes = signing_bytes(CommandName::Delete, &QUEUE, TS, body);
        let mut want = b"chatproject-queue-v1\0DELETE\0".to_vec();
        want.extend_from_slice(&QUEUE);
        want.extend_from_slice(&TS.to_be_bytes());
        want.extend_from_slice(&Sha256::digest(body));
        assert_eq!(bytes, want);
        assert_eq!(bytes.capacity(), bytes.len(), "one exact allocation");

        let names = [
            (CommandName::Create, "CREATE", "/q/create"),
            (CommandName::Send, "SEND", "/q/send"),
            (CommandName::Recv, "RECV", "/q/recv"),
            (CommandName::Ack, "ACK", "/q/ack"),
            (CommandName::Delete, "DELETE", "/q/delete"),
        ];
        for (cmd, name, path) in names {
            let bytes = signing_bytes(cmd, &QUEUE, TS, &[]);
            assert_eq!(&bytes[21..21 + name.len()], name.as_bytes());
            assert_eq!(cmd.path(), path);
        }
    }

    #[test]
    fn verify_refuses_bad_keys_and_signatures_without_panicking() {
        let bytes = signing_bytes(CommandName::Ack, &QUEUE, TS, &3u64.to_be_bytes());
        let sig = sign(&RECIPIENT, &bytes);
        let public = public_key(&RECIPIENT);

        // Not a curve point, and the identity (a weak, small-order key).
        let mut not_a_point = [0xff; 32];
        not_a_point[31] = 0x7f;
        let mut identity = [0u8; 32];
        identity[0] = 1;
        for key in [not_a_point, identity, [0; 32]] {
            assert!(!verify(&key, &bytes, &sig));
        }

        for sig in [[0; 64], [0xff; 64]] {
            assert!(!verify(&public, &bytes, &sig));
        }
        for i in 0..64 {
            let mut bad = sig;
            bad[i] ^= 0x01;
            assert!(!verify(&public, &bytes, &bad), "byte {i}");
        }
    }

    #[test]
    fn decode_refuses_bad_base64_and_wrong_lengths() {
        let id = BASE64URL_NOPAD.encode(&QUEUE);
        assert_eq!(decode_array::<32>(&id), Ok(QUEUE));
        let sig = BASE64URL_NOPAD.encode(&[9; 64]);
        assert_eq!(decode_array::<64>(&sig), Ok([9; 64]));

        assert_eq!(decode_array::<64>(&id), Err(DecodeError::Length));
        assert_eq!(decode_array::<32>(&sig), Err(DecodeError::Length));
        assert_eq!(decode_array::<32>(""), Err(DecodeError::Length));
        let short = BASE64URL_NOPAD.encode(&[1; 31]);
        assert_eq!(decode_array::<32>(&short), Err(DecodeError::Length));

        // Bad length for base64, bad symbols, standard (not url) alphabet, padding,
        // non-zero trailing bits.
        for bad in ["A", "!!!!", "ab+/", "YQ==", "YR"] {
            assert_eq!(decode_ct(bad), Err(DecodeError::Base64), "{bad}");
        }
        for symbol in ["!", "+", "/", "="] {
            let bad = format!("{symbol}{}", &id[1..]);
            assert_eq!(decode_array::<32>(&bad), Err(DecodeError::Base64), "{bad}");
        }
        let mut padded = id.clone();
        padded.push('=');
        assert!(decode_array::<32>(&padded).is_err());

        let max = vec![5u8; MAX_CT];
        assert_eq!(decode_ct(&BASE64URL_NOPAD.encode(&max)), Ok(max));
        let over = BASE64URL_NOPAD.encode(&vec![5u8; MAX_CT + 1]);
        assert_eq!(decode_ct(&over), Err(DecodeError::Length));
        assert_eq!(decode_ct(""), Ok(Vec::new()));
    }

    #[test]
    fn signed_item_verifies_after_json_round_trip() {
        // The path a client and the server take together: build, sign, send as JSON,
        // decode at the boundary, rebuild the signing bytes, verify.
        let ct = b"mls ciphertext".to_vec();
        let sig = sign(&SENDER, &signing_bytes(CommandName::Send, &QUEUE, TS, &ct));
        let item = SendItem {
            queue_id: BASE64URL_NOPAD.encode(&QUEUE),
            ct: BASE64URL_NOPAD.encode(&ct),
            ts: TS,
            sig: BASE64URL_NOPAD.encode(&sig),
        };
        let json = serde_json::to_string(&QueueRequest { items: vec![item] }).unwrap();

        let got: QueueRequest<SendItem> = serde_json::from_str(&json).unwrap();
        let item = &got.items[0];
        let queue_id = decode_array::<32>(&item.queue_id).unwrap();
        let ct = decode_ct(&item.ct).unwrap();
        let sig = decode_array::<64>(&item.sig).unwrap();
        let bytes = signing_bytes(CommandName::Send, &queue_id, item.ts, &ct);
        assert!(verify(&public_key(&SENDER), &bytes, &sig));
    }

    #[test]
    fn json_shapes_round_trip() {
        fn check<T>(value: &T, json: &str)
        where
            T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug,
        {
            assert_eq!(serde_json::to_string(value).unwrap(), json);
            assert_eq!(&serde_json::from_str::<T>(json).unwrap(), value);
        }

        check(
            &QueueRequest {
                items: vec![CreateItem {
                    queue_id: "cQ".to_owned(),
                    recipient_pub: "cg".to_owned(),
                    sender_pub: "cw".to_owned(),
                    ts: 5,
                    sig: "c2ln".to_owned(),
                }],
            },
            r#"{"items":[{"queue_id":"cQ","recipient_pub":"cg","sender_pub":"cw","ts":5,"sig":"c2ln"}]}"#,
        );
        check(
            &QueueRequest {
                items: vec![SendItem {
                    queue_id: "cQ".to_owned(),
                    ct: "Y3Q".to_owned(),
                    ts: 6,
                    sig: "c2ln".to_owned(),
                }],
            },
            r#"{"items":[{"queue_id":"cQ","ct":"Y3Q","ts":6,"sig":"c2ln"}]}"#,
        );
        check(
            &QueueRequest {
                items: vec![RecvItem {
                    queue_id: "cQ".to_owned(),
                    since_seq: 3,
                    ts: 7,
                    sig: "c2ln".to_owned(),
                }],
            },
            r#"{"items":[{"queue_id":"cQ","since_seq":3,"ts":7,"sig":"c2ln"}]}"#,
        );
        check(
            &QueueRequest {
                items: vec![AckItem {
                    queue_id: "cQ".to_owned(),
                    upto_seq: 4,
                    ts: 8,
                    sig: "c2ln".to_owned(),
                }],
            },
            r#"{"items":[{"queue_id":"cQ","upto_seq":4,"ts":8,"sig":"c2ln"}]}"#,
        );
        check(
            &QueueRequest {
                items: vec![DeleteItem {
                    queue_id: "cQ".to_owned(),
                    ts: 9,
                    sig: "c2ln".to_owned(),
                }],
            },
            r#"{"items":[{"queue_id":"cQ","ts":9,"sig":"c2ln"}]}"#,
        );

        check(
            &QueueResponse {
                results: vec![
                    ItemResult::Ok,
                    ItemResult::Exists,
                    ItemResult::Gone,
                    ItemResult::Full,
                    ItemResult::Limited,
                    ItemResult::Bad,
                    ItemResult::Entries {
                        entries: vec![
                            Entry {
                                seq: 1,
                                ct: "YQ".to_owned(),
                                ts: 10,
                            },
                            Entry {
                                seq: 2,
                                ct: "Yg".to_owned(),
                                ts: 11,
                            },
                        ],
                    },
                    ItemResult::Entries { entries: vec![] },
                ],
            },
            concat!(
                r#"{"results":[{"result":"ok"},{"result":"exists"},{"result":"gone"},"#,
                r#"{"result":"full"},{"result":"limited"},{"result":"bad"},"#,
                r#"{"result":"entries","entries":[{"seq":1,"ct":"YQ","ts":10},{"seq":2,"ct":"Yg","ts":11}]},"#,
                r#"{"result":"entries","entries":[]}]}"#,
            ),
        );

        assert!(serde_json::from_str::<ItemResult>(r#"{"result":"nope"}"#).is_err());
        assert!(serde_json::from_str::<ItemResult>(r#"{"result":"entries"}"#).is_err());
        assert!(serde_json::from_str::<ItemResult>(r#""ok""#).is_err());
        assert!(
            serde_json::from_str::<QueueRequest<SendItem>>(
                r#"{"items":[{"queue_id":"cQ","ts":1,"sig":"c2ln"}]}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<QueueRequest<RecvItem>>(
                r#"{"items":[{"queue_id":"cQ","since_seq":-1,"ts":1,"sig":"c2ln"}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn debug_hides_ids_keys_sigs_and_ct() {
        let shown = format!(
            "{:?} {:?} {:?} {:?} {:?} {:?}",
            QueueRequest {
                items: vec![CreateItem {
                    queue_id: "secretqueue".to_owned(),
                    recipient_pub: "secretrecipient".to_owned(),
                    sender_pub: "secretsender".to_owned(),
                    ts: 1,
                    sig: "secretsig".to_owned(),
                }],
            },
            SendItem {
                queue_id: "secretqueue".to_owned(),
                ct: "secretct".to_owned(),
                ts: 2,
                sig: "secretsig".to_owned(),
            },
            RecvItem {
                queue_id: "secretqueue".to_owned(),
                since_seq: 3,
                ts: 4,
                sig: "secretsig".to_owned(),
            },
            AckItem {
                queue_id: "secretqueue".to_owned(),
                upto_seq: 5,
                ts: 6,
                sig: "secretsig".to_owned(),
            },
            DeleteItem {
                queue_id: "secretqueue".to_owned(),
                ts: 7,
                sig: "secretsig".to_owned(),
            },
            QueueResponse {
                results: vec![ItemResult::Entries {
                    entries: vec![Entry {
                        seq: 8,
                        ct: "secretct".to_owned(),
                        ts: 9,
                    }],
                }],
            },
        );
        assert!(!shown.contains("secret"), "{shown}");
        // What is left is enough to debug with: shapes, sequence numbers and times.
        for kept in [
            "CreateItem",
            "ts: 2",
            "since_seq: 3",
            "upto_seq: 5",
            "seq: 8",
            "Entries",
        ] {
            assert!(shown.contains(kept), "{kept} missing from {shown}");
        }
    }
}
