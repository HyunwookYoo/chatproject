//! Snapshot of one receiver's OpenMLS `MemoryStorage` map plus the group id.
//!
//! Format, integers big-endian u32:
//! `b"CNS1" | gid_len | gid | n | n x (key_len | value_len | key | value)`, keys sorted.

use std::collections::HashMap;
use std::io::{self, Read};

const MAGIC: &[u8; 4] = b"CNS1";
/// No single field is larger than this. A corrupt length must not make the NSE allocate gigabytes.
const MAX_FIELD: usize = 64 << 20;
/// Upper bound on entries for the same reason.
const MAX_ENTRIES: usize = 1 << 20;

/// The `MemoryStorage::values` map.
pub type Map = HashMap<Vec<u8>, Vec<u8>>;

pub fn encode(group_id: &[u8], map: &Map) -> Vec<u8> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort();
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(group_id.len() as u32).to_be_bytes());
    out.extend_from_slice(group_id);
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (key, value) in entries {
        out.extend_from_slice(&(key.len() as u32).to_be_bytes());
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(key);
        out.extend_from_slice(value);
    }
    out
}

/// Streams the snapshot, so the file is never held in memory next to the map.
pub fn read(r: &mut impl Read) -> io::Result<(Vec<u8>, Map)> {
    if bytes(r, 4)? != MAGIC {
        return Err(invalid("bad magic"));
    }
    let len = u32(r)?;
    let group_id = bytes(r, len)?;
    let n = u32(r)?;
    if n > MAX_ENTRIES {
        return Err(invalid("too many entries"));
    }
    let mut map = HashMap::with_capacity(n);
    for _ in 0..n {
        let (key_len, value_len) = (u32(r)?, u32(r)?);
        let key = bytes(r, key_len)?;
        map.insert(key, bytes(r, value_len)?);
    }
    Ok((group_id, map))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn u32(r: &mut impl Read) -> io::Result<usize> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b) as usize)
}

fn bytes(r: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    if len > MAX_FIELD {
        return Err(invalid("field too large"));
    }
    let mut v = vec![0; len];
    r.read_exact(&mut v)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Map {
        let mut map = Map::new();
        map.insert(b"k2".to_vec(), b"value two".to_vec());
        map.insert(b"k1".to_vec(), Vec::new());
        map
    }

    #[test]
    fn round_trip_keeps_group_id_and_map() {
        let bytes = encode(b"group", &sample());
        let (group_id, map) = read(&mut &bytes[..]).unwrap();
        assert_eq!(group_id, b"group");
        assert_eq!(map, sample());
    }

    #[test]
    fn encoding_is_deterministic() {
        assert_eq!(encode(b"g", &sample()), encode(b"g", &sample()));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = encode(b"g", &sample());
        bytes[0] = b'X';
        assert_eq!(read(&mut &bytes[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_file_is_rejected() {
        let bytes = encode(b"g", &sample());
        let cut = &bytes[..bytes.len() - 1];
        assert_eq!(read(&mut &cut[..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn oversized_length_is_rejected_without_allocating() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes()); // group id length: 4 GiB
        assert_eq!(read(&mut &bytes[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_entry_count_is_rejected_without_allocating() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(b'g');
        bytes.extend_from_slice(&u32::MAX.to_be_bytes()); // entry count
        assert_eq!(read(&mut &bytes[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
