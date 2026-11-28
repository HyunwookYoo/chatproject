//! What one queue entry holds (M8 spec 6.4, 8.1): a frame exactly as a direct send writes it to
//! the device (the complete kind 17 envelope: an outbox row's wire, a queued Commit's or
//! Welcome's body, a sealed `PushHandle` or `QueueRef` frame), and the device that sent it.
//! A direct send learns the sender from its authenticated session; a queue has none, so the
//! entry names it. The name is only a hint: what the frame carries is checked as it would be
//! from that device directly (the MLS leaf and its credential, the first-Commit hash of a
//! Welcome), so a member naming another device only gets its frame refused.
//!
//! Canonical CBOR `[2, sender_peer, sender_install, frame]`, sealed for its queue (spec 9.5): the
//! server gets `nonce || AES-256-GCM(entry)` under a key HKDF derives from the queue's
//! `sender_secret`, which every member of the conversation holds and the server never does. So
//! the server sees no account, device or conversation id, and nothing that links a rotated queue
//! to the one before it (each queue has its own secret). The nonce is derived from the entry
//! under the same secret, so equal entries in one queue are equal bytes: the server's
//! ciphertext-hash dedup and the renewal of a re-sent entry's hold (spec 9.2, 9.3) depend on it.
//! What that shows the server, that an entry was put in again, it is told anyway. At most
//! `queue_wire::MAX_CT` bytes sealed.
use aes_gcm::aead::{Aead, Key, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use minicbor::{Decoder, Encoder};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::account::{InstallationId, PeerId};
use crate::{CoreError, ErrorCode};

const VERSION: u64 = 2;
const KEY_INFO: &[u8] = b"ChatProject/m8/queue-entry/v2/key";
const NONCE_INFO: &[u8] = b"ChatProject/m8/queue-entry/v2/nonce";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub sender_peer: PeerId,
    pub sender_install: InstallationId,
    /// The kind 17 envelope the sender writes to a device it reaches directly.
    pub frame: Vec<u8>,
}

fn invalid() -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, "invalid_queue_entry")
}

fn id(d: &mut Decoder<'_>) -> Result<[u8; 32], CoreError> {
    d.bytes()
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())
}

/// The queue's entry cipher, from its sender key's HKDF.
fn cipher(keys: &Hkdf<Sha256>) -> Aes256Gcm {
    let mut key = Zeroizing::new([0; 32]);
    keys.expand(KEY_INFO, key.as_mut_slice())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_slice()))
}

/// The nonce `plain` is sealed under in the queue: a function of both, so one entry has one
/// sealed form per queue.
fn nonce(keys: &Hkdf<Sha256>, plain: &[u8]) -> [u8; NONCE_LEN] {
    let mut nonce = [0; NONCE_LEN];
    keys.expand_multi_info(&[NONCE_INFO, Sha256::digest(plain).as_slice()], &mut nonce)
        .expect("12 bytes is a valid HKDF-SHA256 length");
    nonce
}

impl QueueEntry {
    /// The bytes a `SEND` into the queue whose sender key is `sender_secret` carries.
    pub fn seal(&self, sender_secret: &[u8; 32]) -> Vec<u8> {
        let keys = Hkdf::<Sha256>::new(None, sender_secret);
        let plain = self.encode();
        let nonce = nonce(&keys, &plain);
        let sealed = cipher(&keys)
            .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
            .expect("an entry is far below the AES-GCM length limit");
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        out
    }

    /// The entry `sealed` holds, if it was sealed for the queue whose sender key is
    /// `sender_secret` by [`QueueEntry::seal`]: any other bytes, or another queue's, never open.
    pub fn open(sealed: &[u8], sender_secret: &[u8; 32]) -> Result<Self, CoreError> {
        if sealed.len() > queue_wire::MAX_CT || sealed.len() < NONCE_LEN + TAG_LEN {
            return Err(invalid());
        }
        let keys = Hkdf::<Sha256>::new(None, sender_secret);
        let (at, body) = sealed.split_at(NONCE_LEN);
        let plain = cipher(&keys)
            .decrypt(Nonce::from_slice(at), body)
            .map_err(|_| invalid())?;
        if nonce(&keys, &plain) != at {
            return Err(invalid());
        }
        Self::decode(&plain)
    }

    fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(4)
            .unwrap()
            .u64(VERSION)
            .unwrap()
            .bytes(&self.sender_peer.0)
            .unwrap()
            .bytes(&self.sender_install.0)
            .unwrap()
            .bytes(&self.frame)
            .unwrap();
        e.into_writer()
    }

    fn decode(bytes: &[u8]) -> Result<Self, CoreError> {
        let mut d = Decoder::new(bytes);
        if d.array().map_err(|_| invalid())? != Some(4) || d.u64().map_err(|_| invalid())? != VERSION
        {
            return Err(invalid());
        }
        let entry = Self {
            sender_peer: PeerId(id(&mut d)?),
            sender_install: InstallationId(id(&mut d)?),
            frame: d.bytes().map_err(|_| invalid())?.to_vec(),
        };
        if entry.frame.is_empty() || entry.encode() != bytes {
            return Err(invalid());
        }
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [5; 32];

    fn entry() -> QueueEntry {
        QueueEntry {
            sender_peer: PeerId([1; 32]),
            sender_install: InstallationId([2; 32]),
            frame: vec![3; 61_440],
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn an_entry_opens_with_its_queues_key_and_shows_nothing_sealed() {
        let entry = entry();
        let sealed = entry.seal(&SECRET);
        assert!(sealed.len() <= queue_wire::MAX_CT);
        assert!(QueueEntry::open(&sealed, &SECRET).ok() == Some(entry.clone()));
        assert!(!contains(&sealed, &entry.sender_peer.0));
        assert!(!contains(&sealed, &entry.sender_install.0));
        assert!(!contains(&sealed, &[3; 32]));
    }

    /// The server's dedup: the same entry for the same queue is the same bytes. Another queue
    /// (a rotated one, another device's) gets other bytes, nonce included.
    #[test]
    fn sealing_is_the_same_per_queue_and_unlinkable_across_queues() {
        let entry = entry();
        assert_eq!(entry.seal(&SECRET), entry.seal(&SECRET));
        let (here, there) = (entry.seal(&SECRET), entry.seal(&[6; 32]));
        assert_ne!(here[..NONCE_LEN], there[..NONCE_LEN]);
        assert_ne!(here, there);
        let other = QueueEntry {
            frame: vec![4; 10],
            ..entry
        };
        assert_ne!(other.seal(&SECRET)[..NONCE_LEN], here[..NONCE_LEN]);
    }

    #[test]
    fn a_tampered_or_foreign_entry_never_opens() {
        let sealed = entry().seal(&SECRET);
        assert!(QueueEntry::open(&sealed, &[6; 32]).is_err());
        for at in [0, NONCE_LEN, sealed.len() / 2, sealed.len() - 1] {
            let mut flipped = sealed.clone();
            flipped[at] ^= 1;
            assert!(QueueEntry::open(&flipped, &SECRET).is_err(), "byte {at}");
        }
        let mut longer = sealed.clone();
        longer.push(0);
        assert!(QueueEntry::open(&longer, &SECRET).is_err());
        assert!(QueueEntry::open(&sealed[..NONCE_LEN + TAG_LEN - 1], &SECRET).is_err());
        assert!(QueueEntry::open(b"not an entry", &SECRET).is_err());
        // The same entry under a nonce not derived from it (a valid AES-GCM ciphertext) does
        // not open: one entry has one sealed form.
        let plain = entry().encode();
        let other = [9; NONCE_LEN];
        let mut odd = other.to_vec();
        let keys = Hkdf::<Sha256>::new(None, &SECRET);
        odd.extend(
            cipher(&keys)
                .encrypt(Nonce::from_slice(&other), plain.as_slice())
                .unwrap(),
        );
        assert!(QueueEntry::open(&odd, &SECRET).is_err());
    }

    #[test]
    fn only_canonical_version_2_cbor_decodes() {
        let bytes = entry().encode();
        assert!(QueueEntry::decode(&bytes).ok() == Some(entry()));
        // Trailing bytes, another version (1 was the unsealed form), an empty frame, a short
        // id, no array.
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(QueueEntry::decode(&longer).is_err());
        let mut other = bytes.clone();
        other[1] = 1;
        assert!(QueueEntry::decode(&other).is_err());
        let empty = QueueEntry {
            frame: Vec::new(),
            ..entry()
        };
        assert!(QueueEntry::decode(&empty.encode()).is_err());
        let mut e = Encoder::new(Vec::new());
        e.array(4).unwrap().u64(2).unwrap().bytes(&[1; 31]).unwrap();
        e.bytes(&[2; 32]).unwrap().bytes(&[3]).unwrap();
        assert!(QueueEntry::decode(&e.into_writer()).is_err());
        assert!(QueueEntry::decode(b"not an entry").is_err());
    }
}
