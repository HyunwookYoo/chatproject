//! Stable account safety number; device replacement never changes this digest.
use super::PeerId;
use super::SafetyDisplay;
use sha2::{Digest, Sha256};
pub fn safety_digest(a: PeerId, b: PeerId) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"ChatProject/safety/v1");
    h.update(a.min(b).0);
    h.update(a.max(b).0);
    h.finalize().into()
}
pub fn safety_display(a: PeerId, b: PeerId) -> SafetyDisplay {
    let digest = safety_digest(a, b);
    let hex = data_encoding::HEXLOWER.encode(&digest);
    let groups = hex
        .as_bytes()
        .chunks(8)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(4)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&a.min(b).0)
        .unwrap()
        .bytes(&a.max(b).0)
        .unwrap()
        .bytes(&digest)
        .unwrap();
    SafetyDisplay {
        digest,
        groups,
        qr: format!(
            "chatproject:safety/1/{}",
            data_encoding::BASE64URL_NOPAD.encode(&e.into_writer())
        ),
    }
}
