//! Most significant bit first bit reader, used by JT precision trimmed
//! vertex data where components are stored in fewer than 32 bits.

use crate::error::{Error, Result};

/// Reads `1..=64` bit fields from a byte slice, MSB first.
#[derive(Clone, Debug)]
pub struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    /// Create a reader over `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    /// Bits remaining.
    pub fn bits_left(&self) -> usize {
        self.data.len() * 8 - self.bit_pos
    }

    /// Read `bits` bits (1..=64) as an unsigned value, MSB first.
    pub fn read_bits(&mut self, bits: u32) -> Result<u64> {
        if bits == 0 || bits > 64 {
            return Err(Error::invalid(
                self.bit_pos as u64,
                format!("invalid bit width {bits}"),
            ));
        }
        if (self.bits_left() as u32) < bits {
            return Err(Error::invalid(
                self.bit_pos as u64,
                format!("ran out of bits reading {bits} bits"),
            ));
        }

        let mut value = 0u64;
        let mut remaining = bits;
        while remaining > 0 {
            let byte = self.bit_pos / 8;
            let bit = self.bit_pos % 8;
            let available = 8 - bit;
            let take = available.min(remaining as usize) as u32;

            let chunk = ((self.data[byte] << bit) >> (8 - take)) as u64;
            value = (value << take) | chunk;

            self.bit_pos += take as usize;
            remaining -= take;
        }
        Ok(value)
    }

    /// Read `bits` bits and interpret them as a two's complement signed value.
    pub fn read_signed(&mut self, bits: u32) -> Result<i64> {
        if bits == 0 || bits > 63 {
            return Err(Error::invalid(
                self.bit_pos as u64,
                format!("invalid bit width {bits}"),
            ));
        }
        let raw = self.read_bits(bits)? as i64;
        let shift = 64 - bits as i64;
        Ok((raw << shift) >> shift)
    }

    /// Skip to the next byte boundary, returning how many bits were skipped.
    pub fn align(&mut self) -> usize {
        let pad = (8 - (self.bit_pos % 8)) % 8;
        self.bit_pos += pad;
        pad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_packed_fields() {
        // 0b1010_0101 0b1100_0000
        let data = [0b1010_0101u8, 0b1100_0000];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read_bits(3).unwrap(), 0b101);
        assert_eq!(reader.read_bits(5).unwrap(), 0b0_0101);
        assert_eq!(reader.read_bits(8).unwrap(), 0b1100_0000);
        assert_eq!(reader.bits_left(), 0);
    }

    #[test]
    fn signed_fields_use_twos_complement() {
        let data = [0b1111_1111u8];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read_signed(4).unwrap(), -1);
        assert_eq!(reader.read_signed(4).unwrap(), -1);
    }

    #[test]
    fn align_pads_to_byte() {
        let data = [0u8; 4];
        let mut reader = BitReader::new(&data);
        reader.read_bits(3).unwrap();
        assert_eq!(reader.align(), 5);
        assert_eq!(reader.bit_pos, 8);
        assert_eq!(reader.align(), 0);
    }
}
