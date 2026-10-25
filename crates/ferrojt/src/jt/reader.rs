//! Byte-order aware cursor over a JT buffer.
//!
//! Every JT value is stored in the byte order declared by the file header, so
//! the reader keeps a [`ByteOrder`] and a [`major`] version (element version
//! numbers change width at JT 10.0) and hands out already decoded primitives.

use crate::codec::ByteOrder;
use crate::error::{Error, Result};
use crate::jt::guid::Guid;

/// A forward-only cursor over one JT buffer (a file, or a single decompressed
/// segment).
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    order: ByteOrder,
    major: u32,
}

impl<'a> Cursor<'a> {
    /// Wrap `data`.
    pub fn new(data: &'a [u8], order: ByteOrder, major: u32) -> Self {
        Cursor {
            data,
            pos: 0,
            order,
            major,
        }
    }

    /// The byte order of the underlying file.
    pub fn order(&self) -> ByteOrder {
        self.order
    }

    /// The major JT version of the underlying file.
    pub fn major(&self) -> u32 {
        self.major
    }

    /// Current absolute offset.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Number of bytes left in the buffer.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Move the cursor to an absolute offset.
    pub fn seek(&mut self, offset: usize) -> Result<()> {
        if offset > self.data.len() {
            return Err(Error::invalid(
                offset as u64,
                format!("seek past end of buffer ({} bytes)", self.data.len()),
            ));
        }
        self.pos = offset;
        Ok(())
    }

    /// Fail with an offset relative to the start of this buffer.
    pub fn error(&self, message: impl Into<String>) -> Error {
        Error::invalid(self.pos as u64, message)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or_else(|| {
            Error::invalid(self.pos as u64, "length overflow while reading bytes")
        })?;
        if end > self.data.len() {
            return Err(Error::invalid(
                self.pos as u64,
                format!(
                    "unexpected end of buffer: wanted {len} bytes, {} available",
                    self.data.len() - self.pos
                ),
            ));
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Read `len` raw bytes.
    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        self.take(len)
    }

    /// Skip `len` raw bytes.
    pub fn skip(&mut self, len: usize) -> Result<()> {
        self.take(len)?;
        Ok(())
    }

    /// The raw bytes between the cursor start and the current position.
    pub fn consumed(&self, start: usize) -> Result<&'a [u8]> {
        if self.pos < start || start > self.data.len() {
            return Err(self.error("invalid consumed range"));
        }
        Ok(&self.data[start..self.pos])
    }

    /// Read an `u8`.
    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// Read an `i8`.
    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    /// Read an `u16`.
    pub fn read_u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(if self.order == ByteOrder::Big {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    }

    /// Read an `i16`.
    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(self.read_u16()? as i16)
    }

    /// Read an `u32`.
    pub fn read_u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(if self.order == ByteOrder::Big {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    /// Read an `i32`.
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_u32()? as i32)
    }

    /// Read an `u64`.
    pub fn read_u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(if self.order == ByteOrder::Big {
            u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        } else {
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        })
    }

    /// Read an `f32`.
    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    /// Read an `f64`.
    pub fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    /// Read `len` `f32` values.
    pub fn read_f32_vec(&mut self, len: usize) -> Result<Vec<f32>> {
        let mut out = Vec::with_capacity(len.min(self.remaining() / 4));
        for _ in 0..len {
            out.push(self.read_f32()?);
        }
        Ok(out)
    }

    /// Read `len` `u32` values.
    pub fn read_u32_vec(&mut self, len: usize) -> Result<Vec<u32>> {
        let mut out = Vec::with_capacity(len.min(self.remaining() / 4));
        for _ in 0..len {
            out.push(self.read_u32()?);
        }
        Ok(out)
    }

    /// Read a GUID: one `U32`, two `U16` and eight `U8`, each in file order.
    pub fn read_guid(&mut self) -> Result<Guid> {
        let word = self.read_u32()?;
        let a = self.read_u16()?;
        let b = self.read_u16()?;
        let tail = self.take(8)?;
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&word.to_be_bytes());
        bytes[4..6].copy_from_slice(&a.to_be_bytes());
        bytes[6..8].copy_from_slice(&b.to_be_bytes());
        bytes[8..16].copy_from_slice(tail);
        Ok(Guid::from_bytes(bytes))
    }

    /// Element version numbers are `I16` up to JT 9.x and `U8` from JT 10.0 on.
    pub fn read_version(&mut self) -> Result<u32> {
        if self.major >= 10 {
            Ok(u32::from(self.read_u8()?))
        } else {
            Ok(self.read_i16()? as u32)
        }
    }

    /// Read a length-prefixed array of `f32`, where the length itself is an
    /// `i32` (the JT `ReadVec` idiom).
    pub fn read_vec_f32(&mut self) -> Result<Vec<f32>> {
        let len = self.read_len()?;
        self.read_f32_vec(len)
    }

    /// Read a length-prefixed array of `i32`.
    pub fn read_vec_i32(&mut self) -> Result<Vec<i32>> {
        let len = self.read_len()?;
        let mut out = Vec::with_capacity(len.min(self.remaining() / 4));
        for _ in 0..len {
            out.push(self.read_i32()?);
        }
        Ok(out)
    }

    /// Read a length-prefixed array of GUIDs.
    pub fn read_vec_guid(&mut self) -> Result<Vec<Guid>> {
        let len = self.read_len()?;
        let mut out = Vec::with_capacity(len.min(self.remaining() / 16));
        for _ in 0..len {
            out.push(self.read_guid()?);
        }
        Ok(out)
    }

    /// Read a non-negative length prefix, rejecting absurd values early.
    pub fn read_len(&mut self) -> Result<usize> {
        let raw = self.read_i32()?;
        if raw < 0 {
            return Err(self.error(format!("negative length {raw}")));
        }
        let len = raw as usize;
        if len > self.remaining() {
            return Err(self.error(format!(
                "length {len} exceeds the remaining {} bytes",
                self.remaining()
            )));
        }
        Ok(len)
    }

    /// Read a UTF-16 string: an `I32` count followed by that many code units.
    ///
    /// JT stores the count without a terminator; the reference reader also
    /// stops at the first NUL, which is reproduced here.
    pub fn read_mb_string(&mut self) -> Result<String> {
        let count = self.read_len()?;
        let bytes = self.take(count * 2)?;
        let units: Vec<u16> = if self.order == ByteOrder::Big {
            bytes
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect()
        } else {
            bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        };
        Ok(decode_utf16(&units))
    }

    /// Read a byte string: an `I32` count followed by that many bytes,
    /// interpreted as UTF-8 with invalid sequences replaced.
    pub fn read_string(&mut self) -> Result<String> {
        let count = self.read_len()?;
        let bytes = self.take(count)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Decode UTF-16 code units, stopping at the first NUL and keeping unpaired
/// surrogates as the replacement character.
fn decode_utf16(units: &[u16]) -> String {
    let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_primitives() {
        let data = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C,
        ];
        let mut c = Cursor::new(&data, ByteOrder::Little, 9);
        assert_eq!(c.read_u16().unwrap(), 0x0201);
        assert_eq!(c.read_u32().unwrap(), 0x0605_0403);
        assert_eq!(c.read_u16().unwrap(), 0x0807);
        assert_eq!(c.read_i16().unwrap(), 0x0A09);
        assert_eq!(c.position(), 10);
    }

    #[test]
    fn big_endian_primitives() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let mut c = Cursor::new(&data, ByteOrder::Big, 9);
        assert_eq!(c.read_u16().unwrap(), 0x0102);
        assert_eq!(c.read_u32().unwrap(), 0x0304_0506);
        assert_eq!(c.read_u16().unwrap(), 0x0708);
    }

    #[test]
    fn version_width_depends_on_major() {
        let data = [0x02, 0x00, 0x03];
        let mut c = Cursor::new(&data, ByteOrder::Little, 9);
        assert_eq!(c.read_version().unwrap(), 2);
        let mut c = Cursor::new(&data, ByteOrder::Little, 10);
        assert_eq!(c.read_version().unwrap(), 2);
        assert_eq!(c.position(), 1);
    }

    #[test]
    fn guid_is_split_into_three_numeric_fields() {
        let data = [
            0x17, 0x23, 0x7b, 0x83, 0x73, 0x0f, 0xec, 0x11, 0x80, 0x00, 0xcd, 0x25, 0x74, 0x4c,
            0x16, 0x19,
        ];
        let mut c = Cursor::new(&data, ByteOrder::Little, 8);
        assert_eq!(
            c.read_guid().unwrap().to_text(),
            "837b2317-0f73-11ec-80-00-cd-25-74-4c-16-19"
        );
    }

    #[test]
    fn mb_string_stops_at_nul() {
        let data = [0x03, 0x00, 0x00, 0x00, b'A', 0, b'B', 0, 0, 0];
        let mut c = Cursor::new(&data, ByteOrder::Little, 8);
        assert_eq!(c.read_mb_string().unwrap(), "AB");
        assert_eq!(c.position(), 10);
    }

    #[test]
    fn truncated_read_reports_offset() {
        let data = [0x00, 0x01];
        let mut c = Cursor::new(&data, ByteOrder::Little, 9);
        c.skip(1).unwrap();
        let err = c.read_u32().unwrap_err();
        match err {
            Error::Invalid { offset, .. } => assert_eq!(offset, 1),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn length_prefix_is_bounded_by_the_buffer() {
        let data = [0xff, 0xff, 0xff, 0x7f];
        let mut c = Cursor::new(&data, ByteOrder::Little, 9);
        assert!(c.read_len().is_err());
    }
}
