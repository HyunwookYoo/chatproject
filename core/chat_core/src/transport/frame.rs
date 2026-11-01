use crate::{CoreError, ErrorCode};

pub const ALPN: &[u8] = b"chatproject/m2/1";
pub const MAGIC: &[u8; 4] = b"CPM2";

pub fn validate_text(text: &str) -> Result<(), CoreError> {
    if !(1..=4096).contains(&text.len()) {
        return Err(CoreError::new(
            ErrorCode::MessageTooLarge,
            "probe text must contain 1..4096 UTF-8 bytes",
        ));
    }
    Ok(())
}

pub fn protocol_error() -> CoreError {
    CoreError::new(ErrorCode::TransportFailed, "invalid M2 text frame")
}

pub async fn read_text<R: tokio::io::AsyncRead + Unpin>(
    input: &mut R,
) -> Result<Option<String>, CoreError> {
    use tokio::io::AsyncReadExt;

    let mut header = [0; 4];
    if input
        .read(&mut header[..1])
        .await
        .map_err(|_| protocol_error())?
        == 0
    {
        return Ok(None);
    }
    input
        .read_exact(&mut header[1..])
        .await
        .map_err(|_| protocol_error())?;
    let length = u32::from_be_bytes(header);
    if !(1..=4096).contains(&length) {
        return Err(CoreError::new(
            ErrorCode::MessageTooLarge,
            "probe text must contain 1..4096 UTF-8 bytes",
        ));
    }
    let mut body = vec![0; length as usize];
    input
        .read_exact(&mut body)
        .await
        .map_err(|_| protocol_error())?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|_| protocol_error())
}

pub async fn write_text<W: tokio::io::AsyncWrite + Unpin>(
    out: &mut W,
    text: &str,
) -> Result<(), CoreError> {
    use tokio::io::AsyncWriteExt;

    validate_text(text)?;
    out.write_all(&(text.len() as u32).to_be_bytes())
        .await
        .map_err(|_| protocol_error())?;
    out.write_all(text.as_bytes())
        .await
        .map_err(|_| protocol_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn rejects_length_before_reading_body() {
        let mut input = &4097_u32.to_be_bytes()[..];
        assert_eq!(
            read_text(&mut input).await.unwrap_err().code,
            ErrorCode::MessageTooLarge
        );
    }

    #[test]
    fn checks_bytes_not_characters() {
        assert!(validate_text(&"가".repeat(1365)).is_ok());
        assert_eq!(
            validate_text(&"가".repeat(1366)).unwrap_err().code,
            ErrorCode::MessageTooLarge
        );
        assert_eq!(
            validate_text("").unwrap_err().code,
            ErrorCode::MessageTooLarge
        );
    }

    #[tokio::test]
    async fn accepts_one_and_4096_bytes() {
        for text in ["x".to_owned(), "x".repeat(4096)] {
            let mut wire = Vec::new();
            write_text(&mut wire, &text).await.unwrap();
            assert_eq!(&wire[..4], &(text.len() as u32).to_be_bytes());
            assert_eq!(&wire[4..], text.as_bytes());
            let mut input = wire.as_slice();
            assert_eq!(read_text(&mut input).await.unwrap(), Some(text));
            assert_eq!(read_text(&mut input).await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn rejects_zero_and_oversized_wire_lengths() {
        for header in [0_u32.to_be_bytes(), u32::MAX.to_be_bytes()] {
            let mut input = header.as_slice();
            assert_eq!(
                read_text(&mut input).await.unwrap_err().code,
                ErrorCode::MessageTooLarge
            );
        }
    }

    #[tokio::test]
    async fn rejects_invalid_text_before_writing() {
        for text in ["".to_owned(), "x".repeat(4097), "가".repeat(1366)] {
            let mut wire = Vec::new();
            assert_eq!(
                write_text(&mut wire, &text).await.unwrap_err().code,
                ErrorCode::MessageTooLarge
            );
            assert!(wire.is_empty());
        }
    }

    #[tokio::test]
    async fn only_boundary_eof_is_normal() {
        let header = [0, 0, 0, 3];
        let mut empty = &[][..];
        assert_eq!(read_text(&mut empty).await.unwrap(), None);
        for end in 1..4 {
            let mut input = &header[..end];
            assert_eq!(read_text(&mut input).await.unwrap_err(), protocol_error());
        }
        for wire in [
            &[0, 0, 0, 3][..],
            &[0, 0, 0, 3, b'a'][..],
            &[0, 0, 0, 3, b'a', b'b'][..],
        ] {
            let mut input = wire;
            assert_eq!(read_text(&mut input).await.unwrap_err(), protocol_error());
        }
    }

    #[tokio::test]
    async fn rejects_invalid_utf8() {
        let mut input = &[0, 0, 0, 1, 0xff][..];
        let error = read_text(&mut input).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::TransportFailed);
        assert_eq!(error, protocol_error());
    }

    #[tokio::test]
    async fn reads_sequential_frames_in_order() {
        let mut input = &[0, 0, 0, 1, b'a', 0, 0, 0, 3, 0xea, 0xb0, 0x80][..];
        assert_eq!(read_text(&mut input).await.unwrap(), Some("a".to_owned()));
        assert_eq!(read_text(&mut input).await.unwrap(), Some("가".to_owned()));
        assert_eq!(read_text(&mut input).await.unwrap(), None);
    }

    #[tokio::test]
    async fn writes_header_then_utf8_body() {
        let mut wire = Vec::new();
        write_text(&mut wire, "가").await.unwrap();
        assert_eq!(wire, [0, 0, 0, 3, 0xea, 0xb0, 0x80]);
    }

    #[tokio::test]
    async fn reads_fragmented_duplex_frames() {
        let (mut writer, mut reader) = tokio::io::duplex(1);
        let send = tokio::spawn(async move {
            for byte in [0, 0, 0, 3, 0xea, 0xb0, 0x80, 0, 0, 0, 1, b'z'] {
                writer.write_all(&[byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
            writer.shutdown().await.unwrap();
        });
        assert_eq!(read_text(&mut reader).await.unwrap(), Some("가".to_owned()));
        assert_eq!(read_text(&mut reader).await.unwrap(), Some("z".to_owned()));
        assert_eq!(read_text(&mut reader).await.unwrap(), None);
        send.await.unwrap();
    }

    #[tokio::test]
    async fn maps_io_failure_to_protocol_error() {
        let (mut writer, reader) = tokio::io::duplex(1);
        drop(reader);
        assert_eq!(
            write_text(&mut writer, "a").await.unwrap_err(),
            protocol_error()
        );
    }
}
