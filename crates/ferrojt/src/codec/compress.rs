//! DEFLATE / zlib helpers.
//!
//! JT stores most of its large segments (vertex lists, N-Lists, metadata) as
//! raw DEFLATE streams, sometimes with a zlib header. These helpers normalise
//! both cases so readers can call [`inflate`] without caring.

use std::io::Read;

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::error::{Error, Result};

/// `true` if `bytes` starts with a zlib header whose check bits are valid.
pub fn is_deflate_signature(bytes: &[u8]) -> bool {
    // CMF/FLG: CM == 8 and (CMF << 8 | FLG) % 31 == 0.
    let Some((cmf, flg)) = bytes.first().zip(bytes.get(1)) else {
        return false;
    };
    cmf & 0x0f == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0
}

/// Inflate `bytes`, auto detecting a zlib header.
///
/// JT's "no header" streams are raw DEFLATE, which is what `flate2`'s
/// [`DeflateDecoder`] expects.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    if is_deflate_signature(bytes) {
        let mut decoder = ZlibDecoder::new(bytes);
        decoder.read_to_end(&mut out)?;
    } else {
        let mut decoder = DeflateDecoder::new(bytes);
        decoder.read_to_end(&mut out)?;
    }
    if out.is_empty() {
        return Err(Error::invalid(0, "inflated segment is empty"));
    }
    Ok(out)
}

/// Deflate `bytes` with a zlib header (used by round trip tests and writers).
pub fn deflate(bytes: &[u8]) -> Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    std::io::Write::write_all(&mut encoder, bytes)?;
    Ok(encoder.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_zlib() {
        let payload = b"ferrojt segment payload".repeat(32);
        let packed = deflate(&payload).unwrap();
        assert!(is_deflate_signature(&packed));
        assert_eq!(inflate(&packed).unwrap(), payload);
    }
}
