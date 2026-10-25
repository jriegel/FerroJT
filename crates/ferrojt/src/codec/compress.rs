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

/// Magic starting an `.xz` stream (`FD 37 7A 58 5A 00`).
pub const XZ_SIGNATURE: [u8; 6] = [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00];

/// Largest output a single [`decompress_xz`] call may produce.
///
/// JT only declares the *compressed* length of a segment, so a bound is needed
/// to keep a damaged file from exhausting memory.
pub const XZ_OUTPUT_LIMIT: usize = 256 * 1024 * 1024;

/// `true` if `bytes` starts with an `.xz` stream header.
pub fn is_xz_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(&XZ_SIGNATURE)
}

fn find_xz_streams(bytes: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut at = 0usize;
    while let Some(offset) = find_subslice(&bytes[at..], &XZ_SIGNATURE) {
        found.push(at + offset);
        at += offset + XZ_SIGNATURE.len();
    }
    found
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Decompress the `.xz` streams stored in `bytes`.
///
/// JT 10.x stores segment payloads as one or more concatenated `.xz` streams
/// (the reference reader uses `LZMA_CONCATENATED`), so the payload is split on
/// stream magics and every stream is appended to the same output buffer.
pub fn decompress_xz(bytes: &[u8]) -> Result<Vec<u8>> {
    let streams = find_xz_streams(bytes);
    if streams.is_empty() {
        return Err(Error::invalid(0, "segment has no xz stream header"));
    }
    let mut out = Vec::new();
    for (index, &start) in streams.iter().enumerate() {
        let end = streams.get(index + 1).copied().unwrap_or(bytes.len());
        // Stream padding between the footer and the next magic is zero bytes;
        // the footer itself ends with `YZ`, so trimming is unambiguous.
        let chunk = trim_zero_padding(&bytes[start..end]);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(chunk));
        lzma_rs::xz_decompress(&mut reader, &mut out)
            .map_err(|e| Error::corrupt(format!("xz stream at offset {start} failed: {e}")))?;
        if out.len() > XZ_OUTPUT_LIMIT {
            return Err(Error::corrupt(format!(
                "xz stream expands past the {} byte limit",
                XZ_OUTPUT_LIMIT
            )));
        }
    }
    if out.is_empty() {
        return Err(Error::invalid(0, "decompressed segment is empty"));
    }
    Ok(out)
}

fn trim_zero_padding(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    &bytes[..end]
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
