//! Byte order aware reading and writing.

use std::io::{Read, Write};

use crate::error::{Error, Result};

/// Byte order of a binary stream or buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ByteOrder {
    /// Most significant byte first (JT metadata, X3.282).
    Big,
    /// Least significant byte first (JT vertex data on most platforms).
    Little,
}

impl ByteOrder {
    /// The byte order used by the machine executing this code.
    pub fn native() -> Self {
        if cfg!(target_endian = "little") {
            ByteOrder::Little
        } else {
            ByteOrder::Big
        }
    }

    fn is_big(self) -> bool {
        matches!(self, ByteOrder::Big)
    }
}

/// Extension trait adding byte order aware primitives to any [`Read`] source.
pub trait ReaderExt: Read {
    /// Read an `u16`.
    fn read_u16_into(&mut self, order: ByteOrder) -> Result<u16> {
        let mut buf = [0u8; 2];
        self.read_exact_or_err(&mut buf)?;
        Ok(if order.is_big() {
            u16::from_be_bytes(buf)
        } else {
            u16::from_le_bytes(buf)
        })
    }

    /// Read an `u32`.
    fn read_u32_into(&mut self, order: ByteOrder) -> Result<u32> {
        let mut buf = [0u8; 4];
        self.read_exact_or_err(&mut buf)?;
        Ok(if order.is_big() {
            u32::from_be_bytes(buf)
        } else {
            u32::from_le_bytes(buf)
        })
    }

    /// Read an `i32`.
    fn read_i32_into(&mut self, order: ByteOrder) -> Result<i32> {
        Ok(self.read_u32_into(order)? as i32)
    }

    /// Read an `u64`.
    fn read_u64_into(&mut self, order: ByteOrder) -> Result<u64> {
        let mut buf = [0u8; 8];
        self.read_exact_or_err(&mut buf)?;
        Ok(if order.is_big() {
            u64::from_be_bytes(buf)
        } else {
            u64::from_le_bytes(buf)
        })
    }

    /// Read an `f32`.
    fn read_f32_into(&mut self, order: ByteOrder) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32_into(order)?))
    }

    /// Read an `f64`.
    fn read_f64_into(&mut self, order: ByteOrder) -> Result<f64> {
        Ok(f64::from_bits(self.read_u64_into(order)?))
    }

    /// Read exactly `len` bytes into a vector.
    fn read_vec(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read_exact_or_err(&mut buf)?;
        Ok(buf)
    }

    /// `read_exact` mapped onto [`Error::Invalid`] on truncation.
    fn read_exact_or_err(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = self.read(&mut buf[filled..])?;
            if n == 0 {
                return Err(Error::invalid(
                    filled as u64,
                    format!(
                        "unexpected end of stream: wanted {} bytes, got {filled}",
                        buf.len()
                    ),
                ));
            }
            filled += n;
        }
        Ok(())
    }
}

impl<R: Read> ReaderExt for R {}

/// Extension trait adding byte order aware primitives to any [`Write`] sink.
pub trait WriterExt: Write {
    /// Write an `u16`.
    fn write_u16_value(&mut self, value: u16, order: ByteOrder) -> Result<()> {
        let bytes = if order.is_big() {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        self.write_all(&bytes)?;
        Ok(())
    }

    /// Write an `u32`.
    fn write_u32_value(&mut self, value: u32, order: ByteOrder) -> Result<()> {
        let bytes = if order.is_big() {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        self.write_all(&bytes)?;
        Ok(())
    }

    /// Write an `f32`.
    fn write_f32_value(&mut self, value: f32, order: ByteOrder) -> Result<()> {
        self.write_u32_value(value.to_bits(), order)
    }
}

impl<W: Write> WriterExt for W {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_and_little_endian_differ() {
        let raw = [0x01u8, 0x02, 0x03, 0x04];
        let mut big = &raw[..];
        let mut little = &raw[..];
        assert_eq!(big.read_u32_into(ByteOrder::Big).unwrap(), 0x0102_0304);
        assert_eq!(
            little.read_u32_into(ByteOrder::Little).unwrap(),
            0x0403_0201
        );
    }

    #[test]
    fn truncated_stream_reports_invalid() {
        let raw = [0x01u8, 0x02];
        let mut cursor = &raw[..];
        let err = cursor.read_u32_into(ByteOrder::Big).unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }));
    }

    #[test]
    fn write_round_trip() {
        let mut out = Vec::new();
        out.write_u32_value(0xDEAD_BEEF, ByteOrder::Big).unwrap();
        out.write_f32_value(1.5, ByteOrder::Little).unwrap();
        let mut cursor = out.as_slice();
        assert_eq!(cursor.read_u32_into(ByteOrder::Big).unwrap(), 0xDEAD_BEEF);
        assert_eq!(cursor.read_f32_into(ByteOrder::Little).unwrap(), 1.5);
    }
}
