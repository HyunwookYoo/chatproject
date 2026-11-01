//! Canonical M4 inner frames. Text hashes exclude the account envelope and length prefix.
use super::types::MessageKey;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Clone, PartialEq, Eq)]
pub struct TextFrame {
    pub key: MessageKey,
    pub lamport: u64,
    pub prev_hash: [u8; 32],
    pub sent_at_ms: u64,
    pub body: String,
}
impl TextFrame {
    pub(crate) fn encode(&self) -> (Zeroizing<Vec<u8>>, [u8; 32]) {
        let inner = encode_text(self);
        (
            Zeroizing::new(crate::account::auth::envelope(16, &inner)),
            Sha256::digest(&inner).into(),
        )
    }
}
pub fn encode_text(frame: &TextFrame) -> Zeroizing<Vec<u8>> {
    let this = frame;
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(10)
        .unwrap()
        .u8(1)
        .unwrap()
        .u8(0)
        .unwrap()
        .bytes(&this.key.conversation.0)
        .unwrap()
        .bytes(&this.key.id.0)
        .unwrap()
        .bytes(&this.key.sender.0)
        .unwrap()
        .bytes(&this.key.installation.0)
        .unwrap()
        .u64(this.lamport)
        .unwrap()
        .bytes(&this.prev_hash)
        .unwrap()
        .u64(this.sent_at_ms)
        .unwrap()
        .str(&this.body)
        .unwrap();
    Zeroizing::new(e.into_writer())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{InstallationId, PeerId};
    use crate::message::{store::conversation_id, types::MessageId};
    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks_exact(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }
    #[test]
    fn independent_canonical_text_vectors() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/m4-wire-vectors.json"))
                .unwrap();
        let conversation = conversation_id(PeerId([1; 32]), PeerId([2; 32])).unwrap();
        assert_eq!(
            conversation.0.as_slice(),
            hex(v["conversation"].as_str().unwrap())
        );
        assert_eq!(
            conversation,
            conversation_id(PeerId([2; 32]), PeerId([1; 32])).unwrap()
        );
        for vector in v["vectors"].as_array().unwrap() {
            let frame = TextFrame {
                key: MessageKey {
                    conversation,
                    sender: PeerId([1; 32]),
                    installation: InstallationId([3; 32]),
                    id: MessageId([4; 16]),
                },
                lamport: vector["lamport"].as_u64().unwrap(),
                sent_at_ms: vector["sent_at_ms"].as_u64().unwrap(),
                body: vector["body"].as_str().unwrap().into(),
                prev_hash: hex(vector["prev_hash"].as_str().unwrap())
                    .try_into()
                    .unwrap(),
            };
            let (wire, hash) = frame.encode();
            assert!(
                *wire == hex(vector["wire"].as_str().unwrap()),
                "canonical wire mismatch"
            );
            assert_eq!(hash.as_slice(), hex(vector["hash"].as_str().unwrap()));
            let mut d = minicbor::Decoder::new(&wire);
            assert_eq!(d.array().unwrap(), Some(3));
            assert_eq!(d.u8().unwrap(), 1);
            assert_eq!(d.u8().unwrap(), 16);
            let inner = d.bytes().unwrap();
            assert!(
                inner == hex(vector["inner"].as_str().unwrap()),
                "canonical inner mismatch"
            );
            assert_eq!(hash, <[u8; 32]>::from(Sha256::digest(inner)));
            assert_eq!(d.position(), wire.len());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptKind {
    Delivered,
    Read,
}
#[derive(Clone, PartialEq, Eq)]
pub struct ReceiptFrame {
    pub kind: ReceiptKind,
    pub key: MessageKey,
    pub text_hash: [u8; 32],
    pub recipient: crate::account::InstallationId,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    ContactRequired,
    UnsupportedVersion,
    InvalidMessage,
    StorageUnavailable,
}
#[derive(Clone, PartialEq, Eq)]
pub enum Frame {
    Text(TextFrame),
    Receipt(ReceiptFrame),
    ReceiptApplied(ReceiptFrame),
    Reject(RejectReason),
}

fn invalid() -> crate::CoreError {
    crate::CoreError::new(crate::ErrorCode::InvalidMessage, "invalid_message")
}
fn uint(d: &mut minicbor::Decoder<'_>) -> Result<u64, crate::CoreError> {
    d.u64().map_err(|_| invalid())
}
fn bytes<const N: usize>(d: &mut minicbor::Decoder<'_>) -> Result<[u8; N], crate::CoreError> {
    d.bytes()
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())
}
fn key(d: &mut minicbor::Decoder<'_>) -> Result<MessageKey, crate::CoreError> {
    Ok(MessageKey {
        conversation: super::types::ConversationId(bytes(d)?),
        id: super::types::MessageId(bytes(d)?),
        sender: crate::account::PeerId(bytes(d)?),
        installation: crate::account::InstallationId(bytes(d)?),
    })
}
fn receipt_kind(n: u64) -> Result<ReceiptKind, crate::CoreError> {
    match n {
        1 => Ok(ReceiptKind::Delivered),
        2 => Ok(ReceiptKind::Read),
        _ => Err(invalid()),
    }
}
impl ReceiptKind {
    pub(crate) fn number(self) -> u8 {
        match self {
            Self::Delivered => 1,
            Self::Read => 2,
        }
    }
}
impl Frame {
    pub(crate) fn encode(&self) -> Zeroizing<Vec<u8>> {
        if let Self::Text(text) = self {
            return encode_text(text);
        }
        let mut e = minicbor::Encoder::new(Vec::new());
        match self {
            Self::Receipt(r) | Self::ReceiptApplied(r) => {
                let applied = matches!(self, Self::ReceiptApplied(_));
                e.array(if applied { 9 } else { 8 })
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .u8(if applied { 4 } else { r.kind.number() })
                    .unwrap();
                e.bytes(&r.key.conversation.0)
                    .unwrap()
                    .bytes(&r.key.id.0)
                    .unwrap()
                    .bytes(&r.key.sender.0)
                    .unwrap()
                    .bytes(&r.key.installation.0)
                    .unwrap()
                    .bytes(&r.text_hash)
                    .unwrap()
                    .bytes(&r.recipient.0)
                    .unwrap();
                if applied {
                    e.u8(r.kind.number()).unwrap();
                }
            }
            Self::Reject(reason) => {
                e.array(3)
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .u8(3)
                    .unwrap()
                    .u8(match reason {
                        RejectReason::ContactRequired => 1,
                        RejectReason::UnsupportedVersion => 2,
                        RejectReason::InvalidMessage => 3,
                        RejectReason::StorageUnavailable => 4,
                    })
                    .unwrap();
            }
            Self::Text(_) => unreachable!(),
        }
        Zeroizing::new(e.into_writer())
    }
    pub(crate) fn envelope(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(crate::account::auth::envelope(16, &self.encode()))
    }
}
pub fn decode(inner: &[u8]) -> Result<Frame, crate::CoreError> {
    // The complete account envelope must fit, including its canonical bstr header.
    if inner.len() > 61434 {
        return Err(invalid());
    }
    let mut d = minicbor::Decoder::new(inner);
    let fields = d.array().map_err(|_| invalid())?.ok_or_else(invalid)?;
    if uint(&mut d)? != 1 {
        return Err(crate::CoreError::new(
            crate::ErrorCode::UnsupportedMessageVersion,
            "unsupported_message_version",
        ));
    }
    let kind = uint(&mut d)?;
    let frame = match (kind, fields) {
        (0, 10) => {
            let key = key(&mut d)?;
            let lamport = uint(&mut d)?;
            let prev_hash = bytes(&mut d)?;
            let sent_at_ms = uint(&mut d)?;
            let body = d.str().map_err(|_| invalid())?;
            if body.is_empty()
                || body.len() > 16384
                || lamport > i64::MAX as u64
                || sent_at_ms > i64::MAX as u64
            {
                return Err(invalid());
            }
            Frame::Text(TextFrame {
                key,
                lamport,
                prev_hash,
                sent_at_ms,
                body: body.into(),
            })
        }
        (1 | 2, 8) | (4, 9) => {
            let key = key(&mut d)?;
            let text_hash = bytes(&mut d)?;
            let recipient = crate::account::InstallationId(bytes(&mut d)?);
            let r = ReceiptFrame {
                key,
                text_hash,
                recipient,
                kind: receipt_kind(if kind == 4 { uint(&mut d)? } else { kind })?,
            };
            if kind == 4 {
                Frame::ReceiptApplied(r)
            } else {
                Frame::Receipt(r)
            }
        }
        (3, 3) => Frame::Reject(match uint(&mut d)? {
            1 => RejectReason::ContactRequired,
            2 => RejectReason::UnsupportedVersion,
            3 => RejectReason::InvalidMessage,
            4 => RejectReason::StorageUnavailable,
            _ => return Err(invalid()),
        }),
        _ => return Err(invalid()),
    };
    if d.position() != inner.len() || frame.encode().as_slice() != inner {
        return Err(invalid());
    }
    Ok(frame)
}
