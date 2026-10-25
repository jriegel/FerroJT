//! Probability tables shared by the Huffman and arithmetic codecs.
//!
//! Four on-disk layouts exist; the widths of the header fields decide which:
//!
//! | layout      | entry count | symbol | occurrences | value | next context | min value |
//! |-------------|-------------|--------|-------------|-------|--------------|-----------|
//! | mark 1 head | 32          | 6      | 6           | 6     | 6            | 32        |
//! | mark 1 tail | 32          | 6      | 6           | 0     | 6            | 0         |
//! | mark 2      | 16          | 6      | 6           | 6     | 0            | 32        |
//! | mark 3      | 16          | 1 flag | 6           | 7     | –            | 32        |

use super::bits::{BitReader, BitSource};
use crate::error::{Error, Result};

/// One symbol of a probability table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// Symbol id; [`ESCAPE`] marks an out-of-band value.
    pub symbol: i32,
    /// Weight of the symbol.
    pub occurrences: u32,
    /// Value emitted for the symbol.
    pub value: i32,
    /// Context selected after the symbol is decoded.
    pub next_context: u32,
}

/// Symbol id reserved for out-of-band values.
pub const ESCAPE: i32 = -2;

/// A probability table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    entries: Vec<Entry>,
    total: u32,
}

impl Context {
    /// Build a table from `entries`, summing the occurrence counts.
    #[must_use]
    pub fn new(entries: Vec<Entry>) -> Self {
        let total = entries
            .iter()
            .fold(0u32, |sum, entry| sum.wrapping_add(entry.occurrences));
        Self { entries, total }
    }

    /// Table entries.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Sum of all occurrence counts.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.total
    }

    /// True when the table contains the escape symbol.
    #[must_use]
    pub fn has_escape(&self) -> bool {
        self.entries.iter().any(|entry| entry.symbol == ESCAPE)
    }

    /// Reads the first table of a mark 1 codec.
    pub fn read_mark1_first<S: BitSource>(&mut self, reader: &mut BitReader<S>) -> Result<()> {
        self.read(reader, 32, 6, 6, 6, 6, 32)
    }

    /// Reads a following table of a mark 1 codec; values are inherited from
    /// the first table by symbol id.
    pub fn read_mark1_next<S: BitSource>(
        &mut self,
        reader: &mut BitReader<S>,
        first: &Context,
    ) -> Result<()> {
        self.read(reader, 32, 6, 6, 0, 6, 0)?;
        for entry in &mut self.entries {
            if let Some(inherited) = first
                .entries
                .iter()
                .rev()
                .find(|other| other.symbol == entry.symbol)
            {
                entry.value = inherited.value;
            }
        }
        Ok(())
    }

    /// Reads a mark 2 table (JT 9.x arithmetic codec).
    pub fn read_mark2<S: BitSource>(&mut self, reader: &mut BitReader<S>) -> Result<()> {
        self.read(reader, 16, 6, 6, 6, 0, 32)
    }

    /// Reads a mark 3 table (JT 10.x arithmetic codec).
    pub fn read_mark3<S: BitSource>(&mut self, reader: &mut BitReader<S>) -> Result<()> {
        reader.load(16 + 6 + 7 + 32)?;
        let entry_count = reader.read_u32(16)? as usize;
        let occ_bits = reader.read_u32(6)? as usize;
        let value_bits = reader.read_u32(7)? as usize;
        let min_value = reader.read_i32(32)?;
        if occ_bits > 32 || value_bits > 32 {
            return Err(Error::corrupt(format!(
                "probability table field widths {occ_bits}/{value_bits} exceed 32 bits"
            )));
        }
        reader
            .load(entry_count.saturating_mul(1 + occ_bits + value_bits))
            .map_err(corrupt_table)?;
        self.entries = Vec::with_capacity(entry_count.min(1 << 16));
        self.total = 0;
        for _ in 0..entry_count {
            let symbol = i32::from(reader.read_u32(1)? != 0) * ESCAPE;
            let occurrences = reader.read_u32_or_0(occ_bits)?;
            let value = (reader.read_u32_or_0(value_bits)? as i32).wrapping_add(min_value);
            self.entries.push(Entry {
                symbol,
                occurrences,
                value,
                next_context: 0,
            });
            self.total = self.total.wrapping_add(occurrences);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn read<S: BitSource>(
        &mut self,
        reader: &mut BitReader<S>,
        count_width: usize,
        symbol_width: usize,
        occ_width: usize,
        value_width: usize,
        next_width: usize,
        min_width: usize,
    ) -> Result<()> {
        reader
            .load(count_width + symbol_width + occ_width + value_width + next_width + min_width)?;
        let entry_count = reader.read_u32_or_0(count_width)? as usize;
        let symbol_bits = reader.read_u32_or_0(symbol_width)? as usize;
        let occ_bits = reader.read_u32_or_0(occ_width)? as usize;
        let value_bits = reader.read_u32_or_0(value_width)? as usize;
        let next_bits = reader.read_u32_or_0(next_width)? as usize;
        let min_value = reader.read_i32_or_0(min_width)?;
        reader
            .load(entry_count.saturating_mul(symbol_bits + occ_bits + value_bits + next_bits))
            .map_err(corrupt_table)?;
        self.entries = Vec::with_capacity(entry_count.min(1 << 16));
        self.total = 0;
        for _ in 0..entry_count {
            let symbol = (reader.read_u32_or_0(symbol_bits)? as i32).wrapping_sub(2);
            let occurrences = reader.read_u32_or_0(occ_bits)?;
            let value = (reader.read_u32_or_0(value_bits)? as i32).wrapping_add(min_value);
            let next_context = reader.read_u32_or_0(next_bits)?;
            self.entries.push(Entry {
                symbol,
                occurrences,
                value,
                next_context,
            });
            self.total = self.total.wrapping_add(occurrences);
        }
        Ok(())
    }
}

/// Turns a bit-stream failure inside a table body into a table-specific error.
fn corrupt_table(error: Error) -> Error {
    match error {
        Error::Corrupt(message) => Error::corrupt(format!("probability table: {message}")),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::super::bits::raw_slice;
    use super::*;

    /// Builds a mark 2 table by hand: two entries, values 5 and 7.
    #[test]
    fn mark2_table_round_trips() {
        // Header: 16-bit count, 6/6/6 symbol/occ/value widths, 0 next, 32-bit min.
        let mut bits: Vec<u8> = Vec::new();
        let push = |value: u32, width: usize, bits: &mut Vec<u8>| {
            for shift in (0..width).rev() {
                bits.push(((value >> shift) & 1) as u8);
            }
        };
        push(2, 16, &mut bits); // entry count
        push(6, 6, &mut bits); // symbol bits
        push(6, 6, &mut bits); // occurrence bits
        push(6, 6, &mut bits); // value bits
        push(0, 0, &mut bits); // next bits
        push(0, 32, &mut bits); // min value
        // Entries: symbol 0 (raw 2), occ 3, value 5; symbol 1 (raw 3), occ 1, value 7.
        push(2, 6, &mut bits);
        push(3, 6, &mut bits);
        push(5, 6, &mut bits);
        push(3, 6, &mut bits);
        push(1, 6, &mut bits);
        push(7, 6, &mut bits);
        let mut bytes = Vec::new();
        for byte in 0..bits.len().div_ceil(8) {
            let mut value = 0u8;
            for index in 0..8 {
                if byte * 8 + index < bits.len() {
                    value |= bits[byte * 8 + index] << (7 - index);
                }
            }
            bytes.push(value);
        }
        let mut reader = raw_slice(&bytes);
        let mut context = Context::default();
        context.read_mark2(&mut reader).expect("read");
        assert_eq!(context.entries().len(), 2);
        assert_eq!(context.total(), 4);
        assert_eq!(context.entries()[0].value, 5);
        assert_eq!(context.entries()[1].symbol, 1);
        assert_eq!(context.entries()[1].value, 7);
        assert!(!context.has_escape());
    }
}
