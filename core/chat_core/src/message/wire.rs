//! Canonical immutable Text encoding. Strict decoding and receipt RPCs are added by T6.
use super::types::MessageKey;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub(crate) struct TextFrame {
    pub key: MessageKey,
    pub lamport: u64,
    pub prev_hash: [u8; 32],
    pub sent_at_ms: u64,
    pub body: String,
}
impl TextFrame {
    pub(crate) fn encode(&self) -> (Zeroizing<Vec<u8>>, [u8; 32]) {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(10)
            .unwrap()
            .u8(1)
            .unwrap()
            .u8(0)
            .unwrap()
            .bytes(&self.key.conversation.0)
            .unwrap()
            .bytes(&self.key.id.0)
            .unwrap()
            .bytes(&self.key.sender.0)
            .unwrap()
            .bytes(&self.key.installation.0)
            .unwrap()
            .u64(self.lamport)
            .unwrap()
            .bytes(&self.prev_hash)
            .unwrap()
            .u64(self.sent_at_ms)
            .unwrap()
            .str(&self.body)
            .unwrap();
        let inner = Zeroizing::new(e.into_writer());
        (
            Zeroizing::new(crate::account::auth::envelope(16, &inner)),
            Sha256::digest(&inner).into(),
        )
    }
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
