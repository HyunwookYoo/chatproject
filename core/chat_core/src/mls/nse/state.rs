//! CNS2: the snapshot file the app hands the notification extension. One conversation's
//! `mls_kv` rows (its own scope plus the global rows) and the group id.
//!
//! Integers are big-endian u32:
//! `b"CNS2" | gid_len | gid | n | n x (key_len | value_len | key | value)`, keys strictly
//! ascending, so one set of rows has exactly one encoding.
//!
//! Every length is checked against the bytes that are left in the input, and nothing is
//! reserved from a claimed count beyond what the remaining bytes could hold: a corrupt file
//! must not make the extension (a ~15 MB process) allocate by what the file claims.

use std::fmt;

pub(super) const MAGIC: &[u8; 4] = b"CNS2";
const MAX_GROUP_ID: usize = 64;
const MAX_KEY: usize = 4_096;
const MAX_VALUE: usize = 4_194_304;
const MAX_ENTRIES: usize = 1 << 20;
/// A row takes at least its two length fields.
const ROW_HEAD: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BadSnapshot(pub(super) &'static str);
impl fmt::Display for BadSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

pub(super) struct Snapshot<'a> {
    pub group_id: &'a [u8],
    pub rows: Vec<(&'a [u8], &'a [u8])>,
}
impl fmt::Debug for Snapshot<'_> {
    /// Never prints the rows: they hold key material.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Snapshot({} rows)", self.rows.len())
    }
}

pub(super) fn encode(group_id: &[u8], rows: &[(Vec<u8>, Vec<u8>)]) -> Result<Vec<u8>, BadSnapshot> {
    if group_id.len() > MAX_GROUP_ID || rows.len() > MAX_ENTRIES {
        return Err(BadSnapshot("snapshot too large"));
    }
    let mut sorted: Vec<&(Vec<u8>, Vec<u8>)> = rows.iter().collect();
    sorted.sort();
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(group_id.len() as u32).to_be_bytes());
    out.extend_from_slice(group_id);
    out.extend_from_slice(&(sorted.len() as u32).to_be_bytes());
    let mut previous: Option<&[u8]> = None;
    for (key, value) in sorted {
        if key.is_empty() || key.len() > MAX_KEY || value.len() > MAX_VALUE {
            return Err(BadSnapshot("row too large"));
        }
        if previous == Some(key.as_slice()) {
            return Err(BadSnapshot("duplicate key"));
        }
        previous = Some(key);
        out.extend_from_slice(&(key.len() as u32).to_be_bytes());
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(key);
        out.extend_from_slice(value);
    }
    Ok(out)
}

/// The unread part of the input.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], BadSnapshot> {
        if len > self.0.len() {
            return Err(BadSnapshot("truncated"));
        }
        let (taken, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(taken)
    }

    /// A big-endian u32 length or count.
    fn be_len(&mut self) -> Result<usize, BadSnapshot> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| BadSnapshot("truncated"))?;
        Ok(u32::from_be_bytes(bytes) as usize)
    }
}

/// How many rows to reserve room for: the claimed count, but never more than the remaining
/// bytes could hold.
fn reservation(claimed: usize, remaining: usize) -> usize {
    claimed.min(remaining / ROW_HEAD)
}

/// Parses a snapshot. The result borrows from `input`, so the file is never held next to a
/// second copy of itself.
pub(super) fn decode(input: &[u8]) -> Result<Snapshot<'_>, BadSnapshot> {
    let mut r = Reader(input);
    if r.take(4)? != MAGIC {
        return Err(BadSnapshot("bad magic"));
    }
    let group_len = r.be_len()?;
    if group_len > MAX_GROUP_ID {
        return Err(BadSnapshot("group id too long"));
    }
    let group_id = r.take(group_len)?;
    let count = r.be_len()?;
    if count > MAX_ENTRIES {
        return Err(BadSnapshot("too many entries"));
    }
    if count > r.0.len() / ROW_HEAD {
        return Err(BadSnapshot("truncated"));
    }
    let mut rows: Vec<(&[u8], &[u8])> = Vec::with_capacity(reservation(count, r.0.len()));
    for _ in 0..count {
        let (key_len, value_len) = (r.be_len()?, r.be_len()?);
        if key_len == 0 || key_len > MAX_KEY || value_len > MAX_VALUE {
            return Err(BadSnapshot("field too large"));
        }
        let (key, value) = (r.take(key_len)?, r.take(value_len)?);
        if rows.last().is_some_and(|(previous, _)| *previous >= key) {
            return Err(BadSnapshot("keys out of order"));
        }
        rows.push((key, value));
    }
    if !r.0.is_empty() {
        return Err(BadSnapshot("trailing bytes"));
    }
    Ok(Snapshot { group_id, rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(order: impl Iterator<Item = u32>) -> Vec<(Vec<u8>, Vec<u8>)> {
        order
            .map(|i| {
                (
                    format!("key-{i:05}").into_bytes(),
                    vec![1, i as u8, (i >> 8) as u8],
                )
            })
            .collect()
    }

    fn sample() -> Vec<(Vec<u8>, Vec<u8>)> {
        vec![
            (b"k2".to_vec(), b"value two".to_vec()),
            (b"k1".to_vec(), Vec::new()),
        ]
    }

    fn owned(snapshot: &Snapshot<'_>) -> Vec<(Vec<u8>, Vec<u8>)> {
        snapshot
            .rows
            .iter()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect()
    }

    #[test]
    fn round_trip_keeps_group_id_and_rows_in_key_order() {
        let bytes = encode(b"group", &sample()).unwrap();
        let snapshot = decode(&bytes).unwrap();
        assert_eq!(snapshot.group_id, b"group");
        let mut expected = sample();
        expected.sort();
        assert_eq!(owned(&snapshot), expected);
    }

    #[test]
    fn encoding_is_deterministic() {
        // Not a coincidence of two entries: 1,000 rows in ascending and in a scrambled order
        // (37 is coprime to 1,000, so it visits every row once) differ unless encode sorts.
        let ascending = rows(0..1000);
        let scrambled = rows((0..1000).map(|i| (i * 37) % 1000));
        assert_ne!(ascending, scrambled);
        assert_eq!(
            encode(b"g", &ascending).unwrap(),
            encode(b"g", &scrambled).unwrap()
        );
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = encode(b"g", &sample()).unwrap();
        bytes[0] = b'X';
        assert_eq!(decode(&bytes).unwrap_err().to_string(), "bad magic");
        // The M1b format is not this one.
        bytes[..4].copy_from_slice(b"CNS1");
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn every_truncation_is_rejected() {
        let bytes = encode(b"g", &sample()).unwrap();
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut]).is_err(), "{cut}");
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode(b"g", &sample()).unwrap();
        bytes.push(0);
        assert_eq!(decode(&bytes).unwrap_err().to_string(), "trailing bytes");
    }

    #[test]
    fn oversized_group_id_length_is_rejected() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn entry_count_beyond_the_remaining_bytes_is_rejected_before_reserving() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(b'g');
        // Under MAX_ENTRIES, so only the remaining-bytes bound can catch it.
        bytes.extend_from_slice(&(MAX_ENTRIES as u32).to_be_bytes());
        assert_eq!(decode(&bytes).unwrap_err().to_string(), "truncated");
        // And the reservation itself is bounded by what the input could hold.
        assert_eq!(reservation(MAX_ENTRIES, 0), 0);
        assert_eq!(reservation(MAX_ENTRIES, 80), 80 / ROW_HEAD);
        assert_eq!(reservation(3, 8_000), 3);
        bytes.truncate(bytes.len() - 4);
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(decode(&bytes).unwrap_err().to_string(), "too many entries");
    }

    #[test]
    fn field_lengths_beyond_the_remaining_bytes_or_the_limits_are_rejected() {
        let head = |key_len: u32, value_len: u32| {
            let mut bytes = MAGIC.to_vec();
            bytes.extend_from_slice(&0u32.to_be_bytes());
            bytes.extend_from_slice(&1u32.to_be_bytes());
            bytes.extend_from_slice(&key_len.to_be_bytes());
            bytes.extend_from_slice(&value_len.to_be_bytes());
            bytes
        };
        assert!(decode(&head(u32::MAX, 0)).is_err());
        assert!(decode(&head(1, u32::MAX)).is_err());
        assert!(decode(&head(0, 0)).is_err(), "empty key");
        assert!(decode(&head((MAX_KEY + 1) as u32, 0)).is_err());
        assert!(decode(&head(1, (MAX_VALUE + 1) as u32)).is_err());
    }

    #[test]
    fn keys_must_be_strictly_ascending() {
        let duplicate = encode(b"g", &[(b"a".to_vec(), vec![1]), (b"a".to_vec(), vec![2])]);
        assert!(duplicate.is_err(), "encode refuses a duplicate key");
        let mut bytes =
            encode(b"g", &[(b"a".to_vec(), vec![1]), (b"b".to_vec(), vec![1])]).unwrap();
        let second_key = bytes.len() - 2; // the last row ends: key b'b', value [1]
        bytes[second_key] = b'A'; // "A" sorts before "a"
        assert_eq!(decode(&bytes).unwrap_err().to_string(), "keys out of order");
        bytes[second_key] = b'a';
        assert_eq!(decode(&bytes).unwrap_err().to_string(), "keys out of order");
    }
}
