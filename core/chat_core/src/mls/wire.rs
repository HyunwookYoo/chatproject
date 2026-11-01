//! Frame kind 17: MLS control and message frames and their replies.
//!
//! Every inner body is canonical CBOR `[1, op, ...]` and at most 61,434 B, so the complete
//! account envelope `auth::envelope(17, inner)` stays within the 61,440 B frame limit. Requests
//! use ops 0-5; replies use ops 8-14 so a request can never be read as a reply. Decoding follows
//! `message::wire::decode`: the size is checked before anything is parsed, byte strings are
//! borrowed (never allocated from a length claim) until their length is checked, and the result
//! must re-encode to exactly the input bytes.
use crate::account::InstallationId;
use crate::message::types::ConversationId;
use crate::{CoreError, ErrorCode};
use minicbor::{Decoder, Encoder};
use zeroize::Zeroizing;

type R<T> = Result<T, CoreError>;

const VERSION: u64 = 1;
const KIND: u8 = 17;
/// Largest inner body: the envelope adds a 1-byte array head, version, kind and a 3-byte bstr
/// head, so the whole frame is at most 61,440 B.
pub(crate) const MAX_INNER: usize = 61_434;
/// KeyPackage limit in `JoinRequest` and the `KeyPackage` reply.
pub(crate) const MAX_KEY_PACKAGE: usize = super::group::MAX_KEY_PACKAGE_BYTES;
/// Largest `DecryptFailed.class` (1 past, 2 future, 3 out of order, 4 no group, 5 other).
const MAX_CLASS: u64 = 5;

const OP_APP: u64 = 0;
const OP_GROUP_QUERY: u64 = 1;
const OP_KEY_PACKAGE_REQUEST: u64 = 2;
const OP_JOIN_REQUEST: u64 = 3;
const OP_COMMIT: u64 = 4;
const OP_WELCOME: u64 = 5;
const OP_ACK: u64 = 8;
const OP_DECRYPT_FAILED: u64 = 9;
const OP_GROUP_INFO: u64 = 10;
const OP_KEY_PACKAGE: u64 = 11;
const OP_ACCEPTED: u64 = 12;
const OP_BUSY: u64 = 13;
const OP_CONTACT_REQUIRED: u64 = 14;

/// A request sent to another device.
#[derive(Clone, PartialEq, Eq)]
pub enum MlsFrame {
    /// An MLS application message (`MlsMessageOut` TLS).
    App {
        conversation: ConversationId,
        ciphertext: Vec<u8>,
    },
    GroupQuery {
        conversation: ConversationId,
    },
    KeyPackageRequest,
    JoinRequest {
        conversation: ConversationId,
        key_package: Vec<u8>,
    },
    Commit {
        conversation: ConversationId,
        epoch: u64,
        commit: Vec<u8>,
    },
    Welcome {
        conversation: ConversationId,
        welcome: Vec<u8>,
        sender_install: InstallationId,
    },
}

/// The answer to an [`MlsFrame`].
#[derive(Clone, PartialEq, Eq)]
pub enum MlsReply {
    /// The receiver durably stored the message; `ct_hash` is the SHA-256 of the ciphertext.
    Ack {
        ct_hash: [u8; 32],
    },
    /// The ciphertext cannot be opened; `class` is the `group::CLASS_*` number. No content.
    DecryptFailed {
        class: u8,
        epoch: u64,
    },
    /// With `exists == false` the epoch and hash are ignored on encode and decode as 0 / zeros.
    GroupInfo {
        exists: bool,
        epoch: u64,
        first_commit_hash: [u8; 32],
    },
    KeyPackage(Vec<u8>),
    Accepted,
    Busy,
    ContactRequired,
}

fn invalid() -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, "invalid_mls_frame")
}
fn unsupported() -> CoreError {
    CoreError::new(
        ErrorCode::UnsupportedMessageVersion,
        "unsupported_message_version",
    )
}
fn uint(d: &mut Decoder<'_>) -> R<u64> {
    d.u64().map_err(|_| invalid())
}
/// An integer that fits an SQL INTEGER column.
fn epoch(d: &mut Decoder<'_>) -> R<u64> {
    let n = uint(d)?;
    if n > i64::MAX as u64 {
        return Err(invalid());
    }
    Ok(n)
}
fn fixed<const N: usize>(d: &mut Decoder<'_>) -> R<[u8; N]> {
    d.bytes()
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())
}
/// A non-empty byte string of at most `max` bytes. The length is checked on the borrowed slice
/// before it is copied.
fn blob(d: &mut Decoder<'_>, max: usize) -> R<Vec<u8>> {
    let b = d.bytes().map_err(|_| invalid())?;
    if b.is_empty() || b.len() > max {
        return Err(invalid());
    }
    Ok(b.to_vec())
}
fn conversation(d: &mut Decoder<'_>) -> R<ConversationId> {
    Ok(ConversationId(fixed(d)?))
}

/// Starts an encoder at `[1, op, ...]` with `fields` array items in total.
fn start(fields: u64, op: u64) -> Encoder<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    e.array(fields)
        .unwrap()
        .u64(VERSION)
        .unwrap()
        .u64(op)
        .unwrap();
    e
}
fn finish(e: Encoder<Vec<u8>>) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(e.into_writer())
}
/// Checks size, array head, version and op before any field is read.
fn open(inner: &[u8]) -> R<(Decoder<'_>, u64, u64)> {
    if inner.len() > MAX_INNER {
        return Err(invalid());
    }
    let mut d = Decoder::new(inner);
    let fields = d.array().map_err(|_| invalid())?.ok_or_else(invalid)?;
    if uint(&mut d)? != VERSION {
        return Err(unsupported());
    }
    let op = uint(&mut d)?;
    Ok((d, fields, op))
}

impl MlsFrame {
    /// The canonical CBOR body, without the account envelope.
    pub fn inner(&self) -> Zeroizing<Vec<u8>> {
        let mut e;
        match self {
            Self::App {
                conversation,
                ciphertext,
            } => {
                e = start(4, OP_APP);
                e.bytes(&conversation.0).unwrap().bytes(ciphertext).unwrap();
            }
            Self::GroupQuery { conversation } => {
                e = start(3, OP_GROUP_QUERY);
                e.bytes(&conversation.0).unwrap();
            }
            Self::KeyPackageRequest => {
                e = start(2, OP_KEY_PACKAGE_REQUEST);
            }
            Self::JoinRequest {
                conversation,
                key_package,
            } => {
                e = start(4, OP_JOIN_REQUEST);
                e.bytes(&conversation.0)
                    .unwrap()
                    .bytes(key_package)
                    .unwrap();
            }
            Self::Commit {
                conversation,
                epoch,
                commit,
            } => {
                e = start(5, OP_COMMIT);
                e.bytes(&conversation.0)
                    .unwrap()
                    .u64(*epoch)
                    .unwrap()
                    .bytes(commit)
                    .unwrap();
            }
            Self::Welcome {
                conversation,
                welcome,
                sender_install,
            } => {
                e = start(5, OP_WELCOME);
                e.bytes(&conversation.0)
                    .unwrap()
                    .bytes(welcome)
                    .unwrap()
                    .bytes(&sender_install.0)
                    .unwrap();
            }
        }
        finish(e)
    }
    /// The complete kind 17 account envelope.
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(crate::account::auth::envelope(KIND, &self.inner()))
    }
    pub fn decode(inner: &[u8]) -> R<Self> {
        let (mut d, fields, op) = open(inner)?;
        let frame = match (op, fields) {
            (OP_APP, 4) => Self::App {
                conversation: conversation(&mut d)?,
                ciphertext: blob(&mut d, MAX_INNER)?,
            },
            (OP_GROUP_QUERY, 3) => Self::GroupQuery {
                conversation: conversation(&mut d)?,
            },
            (OP_KEY_PACKAGE_REQUEST, 2) => Self::KeyPackageRequest,
            (OP_JOIN_REQUEST, 4) => Self::JoinRequest {
                conversation: conversation(&mut d)?,
                key_package: blob(&mut d, MAX_KEY_PACKAGE)?,
            },
            (OP_COMMIT, 5) => Self::Commit {
                conversation: conversation(&mut d)?,
                epoch: epoch(&mut d)?,
                commit: blob(&mut d, MAX_INNER)?,
            },
            (OP_WELCOME, 5) => Self::Welcome {
                conversation: conversation(&mut d)?,
                welcome: blob(&mut d, MAX_INNER)?,
                sender_install: InstallationId(fixed(&mut d)?),
            },
            _ => return Err(invalid()),
        };
        if d.position() != inner.len() || *frame.inner() != *inner {
            return Err(invalid());
        }
        Ok(frame)
    }
}

impl MlsReply {
    /// The canonical CBOR body, without the account envelope.
    pub fn inner(&self) -> Zeroizing<Vec<u8>> {
        let mut e;
        match self {
            Self::Ack { ct_hash } => {
                e = start(3, OP_ACK);
                e.bytes(ct_hash).unwrap();
            }
            Self::DecryptFailed { class, epoch } => {
                e = start(4, OP_DECRYPT_FAILED);
                e.u8(*class).unwrap().u64(*epoch).unwrap();
            }
            Self::GroupInfo { exists: false, .. } => {
                e = start(3, OP_GROUP_INFO);
                e.bool(false).unwrap();
            }
            Self::GroupInfo {
                exists: true,
                epoch,
                first_commit_hash,
            } => {
                e = start(5, OP_GROUP_INFO);
                e.bool(true)
                    .unwrap()
                    .u64(*epoch)
                    .unwrap()
                    .bytes(first_commit_hash)
                    .unwrap();
            }
            Self::KeyPackage(bytes) => {
                e = start(3, OP_KEY_PACKAGE);
                e.bytes(bytes).unwrap();
            }
            Self::Accepted => e = start(2, OP_ACCEPTED),
            Self::Busy => e = start(2, OP_BUSY),
            Self::ContactRequired => e = start(2, OP_CONTACT_REQUIRED),
        }
        finish(e)
    }
    /// The complete kind 17 account envelope.
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(crate::account::auth::envelope(KIND, &self.inner()))
    }
    pub fn decode(inner: &[u8]) -> R<Self> {
        let (mut d, fields, op) = open(inner)?;
        let reply = match (op, fields) {
            (OP_ACK, 3) => Self::Ack {
                ct_hash: fixed(&mut d)?,
            },
            (OP_DECRYPT_FAILED, 4) => {
                let class = uint(&mut d)?;
                if !(1..=MAX_CLASS).contains(&class) {
                    return Err(invalid());
                }
                Self::DecryptFailed {
                    class: class as u8,
                    epoch: epoch(&mut d)?,
                }
            }
            (OP_GROUP_INFO, 3) => {
                if d.bool().map_err(|_| invalid())? {
                    return Err(invalid());
                }
                Self::GroupInfo {
                    exists: false,
                    epoch: 0,
                    first_commit_hash: [0; 32],
                }
            }
            (OP_GROUP_INFO, 5) => {
                if !d.bool().map_err(|_| invalid())? {
                    return Err(invalid());
                }
                Self::GroupInfo {
                    exists: true,
                    epoch: epoch(&mut d)?,
                    first_commit_hash: fixed(&mut d)?,
                }
            }
            (OP_KEY_PACKAGE, 3) => Self::KeyPackage(blob(&mut d, MAX_KEY_PACKAGE)?),
            (OP_ACCEPTED, 2) => Self::Accepted,
            (OP_BUSY, 2) => Self::Busy,
            (OP_CONTACT_REQUIRED, 2) => Self::ContactRequired,
            _ => return Err(invalid()),
        };
        if d.position() != inner.len() || *reply.inner() != *inner {
            return Err(invalid());
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks_exact(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn independent_vectors_for_requests_and_replies() {
        let root: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/m5-wire-vectors.json"))
                .unwrap();
        let conv = ConversationId(
            hex(root["conversation"].as_str().unwrap())
                .try_into()
                .unwrap(),
        );
        let install = InstallationId(
            hex(root["installation"].as_str().unwrap())
                .try_into()
                .unwrap(),
        );
        let hash: [u8; 32] = hex(root["hash"].as_str().unwrap()).try_into().unwrap();
        let mut seen = 0;
        for v in root["vectors"].as_array().unwrap() {
            let name = v["name"].as_str().unwrap();
            let fi = &v["fields"];
            let f = |k: &str| hex(fi[k].as_str().unwrap());
            let inner = hex(v["inner"].as_str().unwrap());
            let wire = hex(v["wire"].as_str().unwrap());
            if v["kind"] == "frame" {
                let frame = match v["op"].as_u64().unwrap() {
                    0 => MlsFrame::App {
                        conversation: conv,
                        ciphertext: f("ciphertext"),
                    },
                    1 => MlsFrame::GroupQuery { conversation: conv },
                    2 => MlsFrame::KeyPackageRequest,
                    3 => MlsFrame::JoinRequest {
                        conversation: conv,
                        key_package: f("key_package"),
                    },
                    4 => MlsFrame::Commit {
                        conversation: conv,
                        epoch: fi["epoch"].as_u64().unwrap(),
                        commit: f("commit"),
                    },
                    5 => MlsFrame::Welcome {
                        conversation: conv,
                        welcome: f("welcome"),
                        sender_install: install,
                    },
                    op => panic!("op {op}"),
                };
                assert!(*frame.inner() == inner, "inner: {name}");
                assert!(*frame.encode() == wire, "wire: {name}");
                assert!(MlsFrame::decode(&inner).unwrap() == frame, "decode: {name}");
            } else {
                let reply = match v["op"].as_u64().unwrap() {
                    8 => MlsReply::Ack { ct_hash: hash },
                    9 => MlsReply::DecryptFailed {
                        class: fi["class"].as_u64().unwrap() as u8,
                        epoch: fi["epoch"].as_u64().unwrap(),
                    },
                    10 if fi["exists"].as_bool().unwrap() => MlsReply::GroupInfo {
                        exists: true,
                        epoch: fi["epoch"].as_u64().unwrap(),
                        first_commit_hash: hash,
                    },
                    10 => MlsReply::GroupInfo {
                        exists: false,
                        epoch: 0,
                        first_commit_hash: [0; 32],
                    },
                    11 => MlsReply::KeyPackage(f("key_package")),
                    12 => MlsReply::Accepted,
                    13 => MlsReply::Busy,
                    14 => MlsReply::ContactRequired,
                    op => panic!("op {op}"),
                };
                assert!(*reply.inner() == inner, "inner: {name}");
                assert!(*reply.encode() == wire, "wire: {name}");
                assert!(MlsReply::decode(&inner).unwrap() == reply, "decode: {name}");
            }
            seen += 1;
        }
        assert_eq!(seen, 29);
    }

    /// `MlsMessageOut` TLS length of a sealed `MAX_BUCKET` (60,416 B plaintext) message, measured
    /// by `group::tests::max_bucket_frame_fits_61440`.
    const MAX_BUCKET_CIPHERTEXT: usize = 60_598;

    #[test]
    fn max_bucket_app_frame_fits_the_frame_limit() {
        let frame = MlsFrame::App {
            conversation: ConversationId([5; 32]),
            ciphertext: vec![0xa5; MAX_BUCKET_CIPHERTEXT],
        };
        // `[1, 0, conversation32, ciphertext]`: the shape Task 3 measured.
        assert_eq!(frame.inner()[..3], [0x84, 1, 0]);
        assert_eq!(frame.inner().len(), 60_638);
        assert_eq!(frame.encode().len(), 60_644);
        assert!(frame.inner().len() <= MAX_INNER);
        assert!(frame.encode().len() <= 61_440);
        assert!(MlsFrame::decode(&frame.inner()).unwrap() == frame);
    }
}
