//! The MLS ciphertext a push payload carries (spec 5.3): an `MlsMessageOut` PrivateMessage with
//! its `group_id` field emptied, so the proxy, Google and Apple never see the conversation id
//! the group id starts with. The receiver knows the conversation from the push tag and puts the
//! group id back before opening.
//!
//! The TLS bytes are edited in place, never decoded into OpenMLS types and encoded again: the
//! PrivateMessage AAD covers `group_id`, so only the exact original bytes open.
//!
//! Layout (RFC 9420 6): `version u16 | wire_format u16 | group_id<V> | rest`, where `<V>` is
//! the minimal QUIC variable-length size of 1, 2 or 4 bytes and `wire_format` 2 is a
//! PrivateMessage. `version` and `rest` are copied as they are.

/// Why a ciphertext could not be stripped or restored. Never carries ciphertext bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PushCtError {
    /// Not an MLS PrivateMessage (a Commit, a Welcome, anything else).
    #[error("not an mls private message")]
    NotPrivate,
    /// Truncated, or a size field that is not a minimal 1, 2 or 4 byte size.
    #[error("malformed mls message")]
    Malformed,
    /// `restore_group_id` was given a ciphertext whose group id is still there.
    #[error("group id not stripped")]
    NotStripped,
}

const HEADER: usize = 4;
const PRIVATE_MESSAGE: [u8; 2] = [0, 2];
/// Largest size a 4-byte variable-length size can hold (MLS limits sizes to 30 bits).
const MAX_SIZE: usize = (1 << 30) - 1;

/// The ciphertext with its `group_id` replaced by an empty one. Never longer than the input;
/// an already stripped ciphertext comes back unchanged.
pub fn strip_group_id(ciphertext: &[u8]) -> Result<Vec<u8>, PushCtError> {
    let (group_id, rest) = split(ciphertext)?;
    let mut out = Vec::with_capacity(ciphertext.len() - group_id.len());
    out.extend_from_slice(&ciphertext[..HEADER]);
    out.push(0);
    out.extend_from_slice(rest);
    Ok(out)
}

/// The inverse of [`strip_group_id`]: the stripped ciphertext with `group_id` put back.
pub fn restore_group_id(stripped: &[u8], group_id: &[u8]) -> Result<Vec<u8>, PushCtError> {
    let (present, rest) = split(stripped)?;
    if !present.is_empty() {
        return Err(PushCtError::NotStripped);
    }
    let size = encode_size(group_id.len())?;
    let mut out = Vec::with_capacity(HEADER + size.len() + group_id.len() + rest.len());
    out.extend_from_slice(&stripped[..HEADER]);
    out.extend_from_slice(&size);
    out.extend_from_slice(group_id);
    out.extend_from_slice(rest);
    Ok(out)
}

/// The `group_id` field's content and everything after the field.
fn split(ciphertext: &[u8]) -> Result<(&[u8], &[u8]), PushCtError> {
    let header = ciphertext.get(..HEADER).ok_or(PushCtError::Malformed)?;
    if header[2..] != PRIVATE_MESSAGE {
        return Err(PushCtError::NotPrivate);
    }
    let (len, body) = decode_size(&ciphertext[HEADER..])?;
    if body.len() < len {
        return Err(PushCtError::Malformed);
    }
    Ok(body.split_at(len))
}

/// A minimal QUIC variable-length size (RFC 9000 16, RFC 9420 2.1.2) and what follows it.
fn decode_size(bytes: &[u8]) -> Result<(usize, &[u8]), PushCtError> {
    let first = *bytes.first().ok_or(PushCtError::Malformed)?;
    let width = match first >> 6 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => return Err(PushCtError::Malformed),
    };
    let field = bytes.get(..width).ok_or(PushCtError::Malformed)?;
    let value = field[1..]
        .iter()
        .fold(usize::from(first & 0x3f), |value, byte| {
            (value << 8) | usize::from(*byte)
        });
    let minimal = match width {
        1 => true,
        2 => value >= 1 << 6,
        _ => value >= 1 << 14,
    };
    if !minimal {
        return Err(PushCtError::Malformed);
    }
    Ok((value, &bytes[width..]))
}

fn encode_size(len: usize) -> Result<Vec<u8>, PushCtError> {
    Ok(match len {
        0..0x40 => vec![len as u8],
        0x40..0x4000 => (0x4000 | len as u16).to_be_bytes().to_vec(),
        0x4000..=MAX_SIZE => (0x8000_0000 | len as u32).to_be_bytes().to_vec(),
        _ => return Err(PushCtError::Malformed),
    })
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    /// A PrivateMessage-shaped value: header, group id, then `rest`.
    fn private(group_id: &[u8], rest: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 1, 0, 2];
        out.extend_from_slice(&encode_size(group_id.len()).unwrap());
        out.extend_from_slice(group_id);
        out.extend_from_slice(rest);
        out
    }

    #[test]
    fn strips_and_restores_every_size_width() {
        for len in [0, 1, 48, 63, 64, 300, 16_383, 16_384, 70_000] {
            let group_id: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let original = private(&group_id, b"epoch-and-the-rest");
            let stripped = strip_group_id(&original).unwrap();
            assert_eq!(stripped, private(&[], b"epoch-and-the-rest"), "{len}");
            assert!(stripped.len() <= original.len());
            assert_eq!(restore_group_id(&stripped, &group_id).unwrap(), original);
            // Stripping twice changes nothing.
            assert_eq!(strip_group_id(&stripped).unwrap(), stripped);
        }
    }

    #[test]
    fn version_is_copied_as_it_is() {
        let mut original = private(&[7; 48], &[9; 20]);
        original[..2].copy_from_slice(&[0xab, 0xcd]);
        let stripped = strip_group_id(&original).unwrap();
        assert_eq!(stripped[..2], [0xab, 0xcd]);
        assert_eq!(restore_group_id(&stripped, &[7; 48]).unwrap(), original);
    }

    #[test]
    fn other_wire_formats_are_not_private() {
        for format in [1u8, 3, 4, 5, 6, 0] {
            let mut message = private(&[1; 48], &[2; 10]);
            message[3] = format;
            assert_eq!(strip_group_id(&message), Err(PushCtError::NotPrivate));
            assert_eq!(
                restore_group_id(&message, &[1; 48]),
                Err(PushCtError::NotPrivate)
            );
        }
        let mut high = private(&[1; 48], &[]);
        high[2] = 1; // wire format 0x0102
        assert_eq!(strip_group_id(&high), Err(PushCtError::NotPrivate));
    }

    #[test]
    fn restoring_a_complete_ciphertext_is_refused() {
        let original = private(&[1; 48], &[2; 10]);
        assert_eq!(
            restore_group_id(&original, &[1; 48]),
            Err(PushCtError::NotStripped)
        );
    }

    #[test]
    fn malformed_sizes_are_errors() {
        // Truncated anywhere before the end of the group id.
        let original = private(&[1; 48], &[]);
        for end in 0..original.len() {
            assert_eq!(
                strip_group_id(&original[..end]),
                Err(PushCtError::Malformed),
                "{end}"
            );
        }
        // An 8-byte size, and non-minimal 2 and 4 byte sizes.
        for size in [
            &[0xc0, 0, 0, 0, 0, 0, 0, 0][..],
            &[0x40, 0x05],
            &[0x80, 0, 0, 5],
        ] {
            let mut message = vec![0, 1, 0, 2];
            message.extend_from_slice(size);
            message.extend_from_slice(&[0; 8]);
            assert_eq!(strip_group_id(&message), Err(PushCtError::Malformed));
        }
        // A size claiming more than there is.
        assert_eq!(
            strip_group_id(&[0, 1, 0, 2, 0xbf, 0xff, 0xff, 0xff, 1]),
            Err(PushCtError::Malformed)
        );
    }

    #[test]
    fn any_input_is_an_error_or_a_result_never_a_panic() {
        let original = private(&[3; 48], &[4; 64]);
        for at in 0..original.len() {
            for flip in [0x01, 0x40, 0x80, 0xff] {
                let mut bytes = original.clone();
                bytes[at] ^= flip;
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let _ = strip_group_id(&bytes);
                    let _ = restore_group_id(&bytes, &[3; 48]);
                }));
                assert!(outcome.is_ok(), "byte {at} ^ {flip:#x}");
            }
        }
    }
}
