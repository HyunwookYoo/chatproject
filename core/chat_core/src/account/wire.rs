//! Shared primitives for the definite, canonical account CBOR arrays.
use minicbor::{Decoder, Encoder};

use super::DeviceKind;
use crate::{CoreError, ErrorCode};

pub(crate) fn invalid() -> CoreError {
    CoreError::new(ErrorCode::AuthFailed, "invalid account wire data")
}

pub(crate) fn array(d: &mut Decoder<'_>, length: u64) -> Result<(), CoreError> {
    if d.array().map_err(|_| invalid())? != Some(length) {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn uint(d: &mut Decoder<'_>) -> Result<u64, CoreError> {
    d.u64().map_err(|_| invalid())
}

pub(crate) fn bytes<const N: usize>(d: &mut Decoder<'_>) -> Result<[u8; N], CoreError> {
    d.bytes()
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())
}

pub(crate) fn kind(d: &mut Decoder<'_>) -> Result<DeviceKind, CoreError> {
    match uint(d)? {
        0 => Ok(DeviceKind::Desktop),
        1 => Ok(DeviceKind::Mobile),
        _ => Err(invalid()),
    }
}

pub(crate) fn put_kind(e: &mut Encoder<Vec<u8>>, kind: DeviceKind) {
    e.u8(match kind {
        DeviceKind::Desktop => 0,
        DeviceKind::Mobile => 1,
    })
    .unwrap();
}

pub(crate) fn canonical(input: &[u8], encoded: &[u8]) -> Result<(), CoreError> {
    if input != encoded {
        return Err(invalid());
    }
    Ok(())
}
