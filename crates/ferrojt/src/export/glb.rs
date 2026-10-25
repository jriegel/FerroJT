//! GLB (glTF 2.0 binary container) framing.
//!
//! `gltf-json` only writes the JSON document, so the binary container is
//! assembled here:
//!
//! ```text
//! +---------------------------------------------------------------+
//! | header: magic "glTF" | version 2 | total length             |
//! +---------------------------------------------------------------+
//! | chunk header: length | type 0x4E4F534A ("JSON")              |
//! | JSON text, padded with 0x20 to a multiple of 4               |
//! +---------------------------------------------------------------+
//! | chunk header: length | type 0x004E4942 ("BIN\0")             |
//! | binary buffer, padded with 0x00 to a multiple of 4           |
//! +---------------------------------------------------------------+
//! ```

use crate::error::{Error, Result};

/// `"glTF"` &mdash; the GLB magic at offset 0.
pub const GLB_HEADER_MAGIC: [u8; 4] = *b"glTF";

/// GLB format version written by this crate.
pub const GLB_VERSION: u32 = 2;

/// Chunk type of the JSON chunk.
pub const JSON_CHUNK_TYPE: u32 = 0x4E4F_534A;

/// Chunk type of the binary chunk.
pub const BIN_CHUNK_TYPE: u32 = 0x004E_4942;

/// Header size in bytes.
pub const HEADER_LEN: usize = 12;

/// Chunk header size in bytes.
pub const CHUNK_HEADER_LEN: usize = 8;

/// Assembles a GLB from a JSON document and an optional binary buffer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlbBuilder {
    json: Vec<u8>,
    bin: Option<Vec<u8>>,
}

impl GlbBuilder {
    /// Start from an already serialized glTF document.
    pub fn new(json: impl Into<Vec<u8>>) -> Self {
        Self {
            json: json.into(),
            bin: None,
        }
    }

    /// Attach the binary buffer referenced by buffer 0.
    pub fn with_bin(mut self, bin: impl Into<Vec<u8>>) -> Self {
        self.bin = Some(bin.into());
        self
    }

    /// Total size of the resulting file, including padding.
    pub fn len(&self) -> usize {
        let json_len = pad4(self.json.len());
        let bin_len = self.bin.as_ref().map(|b| pad4(b.len())).unwrap_or(0);
        HEADER_LEN
            + CHUNK_HEADER_LEN
            + json_len
            + if bin_len == 0 {
                0
            } else {
                CHUNK_HEADER_LEN + bin_len
            }
    }

    /// `true` if no binary chunk would be emitted.
    pub fn is_empty(&self) -> bool {
        self.bin.is_none()
    }

    /// Produce the finished container.
    pub fn finish(self) -> Result<Vec<u8>> {
        if self.json.is_empty() {
            return Err(Error::invalid(0, "glTF JSON document is empty"));
        }
        let total = self.len() as u32;
        let mut out = Vec::with_capacity(total as usize);

        out.extend_from_slice(&GLB_HEADER_MAGIC);
        out.extend_from_slice(&GLB_VERSION.to_le_bytes());
        out.extend_from_slice(&total.to_le_bytes());

        write_chunk(&mut out, JSON_CHUNK_TYPE, &self.json, b' ');
        if let Some(bin) = &self.bin {
            if !bin.is_empty() {
                write_chunk(&mut out, BIN_CHUNK_TYPE, bin, 0u8);
            }
        }
        debug_assert_eq!(out.len(), total as usize);
        Ok(out)
    }
}

fn write_chunk(out: &mut Vec<u8>, chunk_type: u32, payload: &[u8], pad: u8) {
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&chunk_type.to_le_bytes());
    out.extend_from_slice(payload);
    let pad_len = pad4(payload.len()) - payload.len();
    out.resize(out.len() + pad_len, pad);
}

/// Round `len` up to the next multiple of four, as required by the GLB spec.
pub fn pad4(len: usize) -> usize {
    len.div_ceil(4) * 4
}

/// Parse the GLB header, returning `(json_len, bin_len)`.
///
/// Used by tests and by a future glTF reader.
pub fn parse_header(bytes: &[u8]) -> Result<(usize, Option<usize>)> {
    if bytes.len() < HEADER_LEN + CHUNK_HEADER_LEN {
        return Err(Error::invalid(0, "file too small to be a GLB"));
    }
    if bytes[0..4] != GLB_HEADER_MAGIC {
        return Err(Error::invalid(0, "missing 'glTF' magic"));
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != GLB_VERSION {
        return Err(Error::invalid(
            4,
            format!("unsupported GLB version {version}"),
        ));
    }
    let total = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if total > bytes.len() {
        return Err(Error::invalid(8, "GLB length exceeds file size"));
    }

    let json_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    let json_type = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    if json_type != JSON_CHUNK_TYPE {
        return Err(Error::invalid(16, "first GLB chunk is not JSON"));
    }

    let bin_start = HEADER_LEN + CHUNK_HEADER_LEN + pad4(json_len);
    let bin = if bin_start + CHUNK_HEADER_LEN <= total {
        let bin_len = u32::from_le_bytes([
            bytes[bin_start],
            bytes[bin_start + 1],
            bytes[bin_start + 2],
            bytes[bin_start + 3],
        ]) as usize;
        let bin_type = u32::from_le_bytes([
            bytes[bin_start + 4],
            bytes[bin_start + 5],
            bytes[bin_start + 6],
            bytes[bin_start + 7],
        ]);
        if bin_type != BIN_CHUNK_TYPE {
            return Err(Error::invalid(
                bin_start as u64 + 4,
                "second GLB chunk is not BIN",
            ));
        }
        Some(bin_len)
    } else {
        None
    };
    Ok((json_len, bin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_to_multiple_of_four() {
        assert_eq!(pad4(0), 0);
        assert_eq!(pad4(1), 4);
        assert_eq!(pad4(4), 4);
        assert_eq!(pad4(5), 8);
    }

    #[test]
    fn json_only_glb_round_trips() {
        let file = GlbBuilder::new(br#"{"asset":{"version":"2.0"}}"#.to_vec())
            .finish()
            .unwrap();
        let (json_len, bin) = parse_header(&file).unwrap();
        assert_eq!(json_len, 27);
        assert_eq!(bin, None);
        assert_eq!(file.len() % 4, 0);
    }

    #[test]
    fn json_plus_bin_round_trips() {
        let file = GlbBuilder::new(br#"{"asset":{"version":"2.0"}}"#.to_vec())
            .with_bin(vec![1, 2, 3])
            .finish()
            .unwrap();
        let (json_len, bin) = parse_header(&file).unwrap();
        assert_eq!(json_len, 27);
        assert_eq!(bin, Some(3));
        assert_eq!(
            file.len(),
            u32::from_le_bytes([file[8], file[9], file[10], file[11]]) as usize
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let err = parse_header(b"NOPE0123456789").unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }));
    }
}
