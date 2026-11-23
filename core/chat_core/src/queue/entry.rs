//! What one queue entry holds (M8 spec 6.4, 8.1): a frame exactly as a direct send writes it to
//! the device (the complete kind 17 envelope: an outbox row's wire, a queued Commit's or
//! Welcome's body, a sealed `PushHandle` or `QueueRef` frame), and the device that sent it.
//! A direct send learns the sender from its authenticated session; a queue has none, so the
//! entry names it. The name is only a hint: what the frame carries is checked as it would be
//! from that device directly (the MLS leaf and its credential, the first-Commit hash of a
//! Welcome), so a member naming another device only gets its frame refused.
//!
//! Canonical CBOR `[1, sender_peer, sender_install, frame]`, at most `queue_wire::MAX_CT` bytes.
use minicbor::{Decoder, Encoder};

use crate::account::{InstallationId, PeerId};
use crate::{CoreError, ErrorCode};

const VERSION: u64 = 1;

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

impl QueueEntry {
    pub fn encode(&self) -> Vec<u8> {
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

    pub fn decode(bytes: &[u8]) -> Result<Self, CoreError> {
        if bytes.len() > queue_wire::MAX_CT {
            return Err(invalid());
        }
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

    #[test]
    fn an_entry_reads_back_and_nothing_else_does() {
        let entry = QueueEntry {
            sender_peer: PeerId([1; 32]),
            sender_install: InstallationId([2; 32]),
            frame: vec![3; 61_440],
        };
        let bytes = entry.encode();
        assert!(bytes.len() <= queue_wire::MAX_CT);
        assert!(QueueEntry::decode(&bytes).ok() == Some(entry.clone()));
        // Trailing bytes, another version, an empty frame, a short id, no array.
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(QueueEntry::decode(&longer).is_err());
        let mut other = bytes.clone();
        other[1] = 2;
        assert!(QueueEntry::decode(&other).is_err());
        let empty = QueueEntry {
            frame: Vec::new(),
            ..entry.clone()
        };
        assert!(QueueEntry::decode(&empty.encode()).is_err());
        let mut e = Encoder::new(Vec::new());
        e.array(4).unwrap().u64(1).unwrap().bytes(&[1; 31]).unwrap();
        e.bytes(&[2; 32]).unwrap().bytes(&[3]).unwrap();
        assert!(QueueEntry::decode(&e.into_writer()).is_err());
        assert!(QueueEntry::decode(b"not an entry").is_err());
    }
}
