//! The plaintext carried inside an MLS application message: the M5 subset of `ChatMessage`
//! (parent design 7.2) as CBOR, padded to a size bucket *before* encryption.
//!
//! Layout: `[1, msg_id, lamport, prev_hash, sent_at_ms, content, pad]` where `content` is
//! `[1, body]` (Text), `[2, kind, [target, ...]]` (Receipt) or `[n, data]` (anything else:
//! `Custom`), and `pad` is a byte string sized so the whole encoding is exactly one bucket.
//! The padding lives inside the AEAD, so neither the ciphertext nor a push payload reveals the
//! real length beyond the bucket.
use std::fmt;

use minicbor::{Decoder, Encoder, data::Type};
use zeroize::Zeroizing;

use crate::message::types::MessageId;
use crate::message::wire::ReceiptKind;
use crate::{CoreError, ErrorCode};

type R<T> = Result<T, CoreError>;

/// Plaintext bucket whose sealed message stays within a push payload (<= 2,900 B ciphertext).
/// Measured: `group::tests::push_bucket_ciphertext_fits_2900`.
pub(crate) const PUSH_BUCKET: usize = 2_560;
/// Largest plaintext bucket; its sealed kind 17 App frame stays within the 61,440 B frame limit.
/// Measured: `group::tests::max_bucket_frame_fits_61440`.
pub(crate) const MAX_BUCKET: usize = 60_416;
/// Padded plaintext lengths: 1 KiB / push / 16 KiB / max.
pub(crate) const BUCKETS: [usize; 4] = [1_024, PUSH_BUCKET, 16_384, MAX_BUCKET];
/// Upper bound on the messages one `Receipt` may cover.
pub(crate) const MAX_RECEIPT_TARGETS: usize = 50;

const VERSION: u64 = 1;
const TEXT: u64 = 1;
const RECEIPT: u64 = 2;
/// Content arrays are at most this long; a longer claim is malformed.
const MAX_CONTENT_ITEMS: u64 = 16;

fn invalid(detail: &'static str) -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, detail)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Content {
    Text {
        body: String,
    },
    Receipt {
        kind: ReceiptKind,
        targets: Vec<MessageId>,
    },
    /// Any content number this version does not know. 1 and 2 are reserved for the variants above.
    Custom {
        kind: u32,
        data: Vec<u8>,
    },
}

impl fmt::Debug for Content {
    /// Never prints bodies or payloads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { body } => write!(f, "Text({} B)", body.len()),
            Self::Receipt { kind, targets } => write!(f, "Receipt({kind:?}, {})", targets.len()),
            Self::Custom { kind, data } => write!(f, "Custom({kind}, {} B)", data.len()),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    pub msg_id: MessageId,
    pub lamport: u64,
    pub prev_hash: [u8; 32],
    pub sent_at_ms: u64,
    pub content: Content,
}

impl fmt::Debug for ChatMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatMessage")
            .field("msg_id", &self.msg_id)
            .field("lamport", &self.lamport)
            .field("content", &self.content)
            .finish_non_exhaustive()
    }
}

/// CBOR byte string header for `len` using exactly `header` bytes (1, 2 or 3), or `None` when
/// `len` does not fit that header. Non-minimal headers are valid CBOR and let the padding hit
/// a bucket exactly where the minimal header sizes would skip a length.
fn bytes_header(header: usize, len: usize) -> Option<Vec<u8>> {
    match header {
        1 if len < 24 => Some(vec![0x40 | len as u8]),
        2 if len <= 0xff => Some(vec![0x58, len as u8]),
        3 if len <= 0xffff => Some(vec![0x59, (len >> 8) as u8, len as u8]),
        _ => None,
    }
}

impl ChatMessage {
    /// CBOR encoding padded with zeros to exactly the smallest bucket that holds it.
    /// `MessageTooLarge` if even `MAX_BUCKET` cannot, `InvalidMessage` for malformed content.
    pub(crate) fn encode_padded(&self) -> R<Zeroizing<Vec<u8>>> {
        let estimate = 96
            + match &self.content {
                Content::Text { body } => body.len(),
                Content::Receipt { targets, .. } => 17 * targets.len(),
                Content::Custom { data, .. } => data.len(),
            };
        let mut e = Encoder::new(Vec::with_capacity(estimate));
        self.encode_head(&mut e)?;
        let head = Zeroizing::new(e.into_writer());
        // The pad is at least one byte (an empty byte string).
        let bucket = BUCKETS
            .into_iter()
            .find(|bucket| *bucket > head.len())
            .ok_or_else(|| CoreError::new(ErrorCode::MessageTooLarge, "mls payload too large"))?;
        let remaining = bucket - head.len();
        let (header, pad_len) = (1..=3)
            .find_map(|header| {
                let pad_len = remaining.checked_sub(header)?;
                bytes_header(header, pad_len).map(|bytes| (bytes, pad_len))
            })
            .ok_or_else(|| invalid("mls payload padding failed"))?;
        let mut out = Zeroizing::new(Vec::with_capacity(bucket));
        out.extend_from_slice(&head);
        out.extend_from_slice(&header);
        out.resize(bucket, 0);
        debug_assert_eq!(out.len(), head.len() + header.len() + pad_len);
        Ok(out)
    }

    fn encode_head(&self, e: &mut Encoder<Vec<u8>>) -> R<()> {
        match &self.content {
            Content::Receipt { targets, .. } if targets.len() > MAX_RECEIPT_TARGETS => {
                return Err(invalid("too many receipt targets"));
            }
            Content::Custom { kind, .. } if matches!(u64::from(*kind), TEXT | RECEIPT) => {
                return Err(invalid("reserved content number"));
            }
            _ => {}
        }
        // Writing into a `Vec` cannot fail, so the errors below are unreachable.
        let failed = |_| invalid("mls payload encoding failed");
        (|| {
            e.array(7)?
                .u64(VERSION)?
                .bytes(&self.msg_id.0)?
                .u64(self.lamport)?
                .bytes(&self.prev_hash)?
                .u64(self.sent_at_ms)?;
            match &self.content {
                Content::Text { body } => {
                    e.array(2)?.u64(TEXT)?.str(body)?;
                }
                Content::Receipt { kind, targets } => {
                    e.array(3)?
                        .u64(RECEIPT)?
                        .u8(kind.number())?
                        .array(targets.len() as u64)?;
                    for target in targets {
                        e.bytes(&target.0)?;
                    }
                }
                Content::Custom { kind, data } => {
                    e.array(2)?.u64(u64::from(*kind))?.bytes(data)?;
                }
            }
            Ok::<(), minicbor::encode::Error<std::convert::Infallible>>(())
        })()
        .map_err(failed)
    }

    /// Inverse of `encode_padded`. The input must be exactly one bucket long.
    pub(crate) fn decode_padded(input: &[u8]) -> R<ChatMessage> {
        if !BUCKETS.contains(&input.len()) {
            return Err(invalid("mls payload is not a bucket size"));
        }
        let mut d = Decoder::new(input);
        let message = Self::decode_fields(&mut d).map_err(|_| invalid("invalid mls payload"))?;
        if d.position() != input.len() {
            return Err(invalid("invalid mls payload"));
        }
        Ok(message)
    }

    fn decode_fields(d: &mut Decoder<'_>) -> Result<ChatMessage, minicbor::decode::Error> {
        fn fixed<const N: usize>(d: &mut Decoder<'_>) -> Result<[u8; N], minicbor::decode::Error> {
            d.bytes()?
                .try_into()
                .map_err(|_| minicbor::decode::Error::message("wrong length"))
        }
        let bad = |m: &'static str| minicbor::decode::Error::message(m);
        if d.array()? != Some(7) || d.u64()? != VERSION {
            return Err(bad("version"));
        }
        let msg_id = MessageId(fixed(d)?);
        let lamport = d.u64()?;
        let prev_hash = fixed(d)?;
        let sent_at_ms = d.u64()?;
        let items = d.array()?.ok_or_else(|| bad("indefinite"))?;
        if items == 0 || items > MAX_CONTENT_ITEMS {
            return Err(bad("content"));
        }
        let content = match d.u64()? {
            TEXT if items == 2 => Content::Text {
                body: d.str()?.to_owned(),
            },
            RECEIPT if items == 3 => {
                let kind = match d.u8()? {
                    1 => ReceiptKind::Delivered,
                    2 => ReceiptKind::Read,
                    _ => return Err(bad("receipt kind")),
                };
                let count = d.array()?.ok_or_else(|| bad("indefinite"))?;
                if count > MAX_RECEIPT_TARGETS as u64 {
                    return Err(bad("receipt targets"));
                }
                let mut targets = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    targets.push(MessageId(fixed(d)?));
                }
                Content::Receipt { kind, targets }
            }
            TEXT | RECEIPT => return Err(bad("content shape")),
            number => {
                let kind = u32::try_from(number).map_err(|_| bad("content number"))?;
                let mut data = Vec::new();
                if items == 2 && d.datatype()? == Type::Bytes {
                    data = d.bytes()?.to_vec();
                } else {
                    for _ in 1..items {
                        d.skip()?;
                    }
                }
                Content::Custom { kind, data }
            }
        };
        // The pad is a byte string running to the end of the bucket.
        d.bytes()?;
        Ok(ChatMessage {
            msg_id,
            lamport,
            prev_hash,
            sent_at_ms,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(content: Content) -> ChatMessage {
        ChatMessage {
            msg_id: MessageId([3; 16]),
            lamport: 9,
            prev_hash: [4; 32],
            sent_at_ms: 1_700_000_000_000,
            content,
        }
    }
    fn text(len: usize) -> ChatMessage {
        message(Content::Text {
            body: "a".repeat(len),
        })
    }

    #[test]
    fn padding_hits_buckets() {
        // Fixed fields take well under 100 B, so these land on 1 KiB, 1 KiB, push, 16 KiB, max.
        for (len, expected) in [
            (1, BUCKETS[0]),
            (900, BUCKETS[0]),
            (1_000, BUCKETS[1]),
            (16_000, BUCKETS[2]),
            (16_384, BUCKETS[3]),
        ] {
            let encoded = text(len).encode_padded().unwrap();
            assert_eq!(encoded.len(), expected, "body of {len} B");
            assert_eq!(ChatMessage::decode_padded(&encoded).unwrap(), text(len));
        }
        // Every plaintext length around each bucket edge (including the lengths where the pad's
        // length header changes size) lands exactly on a bucket and round-trips.
        for edge in BUCKETS {
            for len in edge - 200..=edge - 90 {
                let m = text(len);
                let encoded = m.encode_padded().unwrap();
                assert!(BUCKETS.contains(&encoded.len()), "body of {len} B");
                assert_eq!(ChatMessage::decode_padded(&encoded).unwrap(), m);
            }
        }
        // A body that cannot fit even the largest bucket is an error, as is a non-bucket input.
        let too_big = text(MAX_BUCKET);
        assert_eq!(
            too_big.encode_padded().unwrap_err().code,
            ErrorCode::MessageTooLarge
        );
        let mut encoded = text(1).encode_padded().unwrap().to_vec();
        encoded.pop();
        assert!(ChatMessage::decode_padded(&encoded).is_err());
        encoded.extend_from_slice(&[0, 0]);
        assert!(ChatMessage::decode_padded(&encoded).is_err());
        assert!(ChatMessage::decode_padded(&[]).is_err());
    }

    #[test]
    fn every_variant_round_trips() {
        for content in [
            Content::Text {
                body: "안녕 hello".into(),
            },
            Content::Receipt {
                kind: ReceiptKind::Read,
                targets: (0..MAX_RECEIPT_TARGETS as u8)
                    .map(|i| MessageId([i; 16]))
                    .collect(),
            },
            Content::Receipt {
                kind: ReceiptKind::Delivered,
                targets: vec![],
            },
            Content::Custom {
                kind: 77,
                data: vec![1, 2, 3],
            },
        ] {
            let m = message(content);
            let encoded = m.encode_padded().unwrap();
            assert_eq!(encoded.len(), BUCKETS[0]);
            assert_eq!(ChatMessage::decode_padded(&encoded).unwrap(), m);
        }
    }

    #[test]
    fn rejects_malformed_content() {
        let too_many = message(Content::Receipt {
            kind: ReceiptKind::Read,
            targets: vec![MessageId([1; 16]); MAX_RECEIPT_TARGETS + 1],
        });
        assert!(too_many.encode_padded().is_err());
        for kind in [1, 2] {
            let reserved = message(Content::Custom { kind, data: vec![] });
            assert!(reserved.encode_padded().is_err());
        }
    }

    #[test]
    fn unknown_content_decodes_as_custom() {
        // A future variant: content number 40 with two fields, then the usual pad.
        let mut e = Encoder::new(Vec::new());
        e.array(7)
            .unwrap()
            .u64(VERSION)
            .unwrap()
            .bytes(&[3; 16])
            .unwrap()
            .u64(5)
            .unwrap()
            .bytes(&[4; 32])
            .unwrap()
            .u64(6)
            .unwrap()
            .array(3)
            .unwrap()
            .u64(40)
            .unwrap()
            .str("x")
            .unwrap()
            .u8(9)
            .unwrap();
        let mut bytes = e.into_writer();
        let remaining = BUCKETS[0] - bytes.len();
        bytes.extend_from_slice(&bytes_header(3, remaining - 3).unwrap());
        bytes.resize(BUCKETS[0], 0);
        let decoded = ChatMessage::decode_padded(&bytes).unwrap();
        assert_eq!(decoded.msg_id, MessageId([3; 16]));
        assert_eq!(decoded.lamport, 5);
        assert_eq!(
            decoded.content,
            Content::Custom {
                kind: 40,
                data: vec![]
            }
        );

        // A bytes payload under an unknown number is kept.
        let m = message(Content::Custom {
            kind: 41,
            data: vec![9; 10],
        });
        assert_eq!(
            ChatMessage::decode_padded(&m.encode_padded().unwrap()).unwrap(),
            m
        );
    }

    #[test]
    fn debug_never_prints_bodies() {
        let m = message(Content::Text {
            body: "secret words".into(),
        });
        assert!(!format!("{m:?}").contains("secret"));
    }
}
