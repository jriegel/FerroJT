//! Buffered bit reader used by the integer codec packages.
//!
//! JT stores codec payloads as a sequence of 32-bit words that are consumed
//! most-significant-bit first. Two refill styles exist and they are *not*
//! interchangeable:
//!
//! * the *raw* style refills with a **bit count** and keeps any leftover bits
//!   between refills; its payload words are always big-endian, and the bytes
//!   are pulled from the surrounding reader on demand (probability tables have
//!   no length prefix),
//! * the *word* style refills with a whole number of 32-bit words, discards
//!   leftover bits, and its payload follows the endianness of the file.
//!
//! Both styles are implemented once over a [`BitSource`], which decides whether
//! the payload bytes come from an already decoded slice or from a live
//! [`Cursor`].

use crate::codec::ByteOrder;
use crate::error::{Error, Result};
use crate::jt::reader::Cursor;

/// Shifts a bit buffer left, dropping bits that fall off the top.
fn shifted_left(value: u32, bits: usize) -> u32 {
    if bits >= 32 { 0 } else { value << bits }
}

/// Supplies the payload bytes consumed by a [`BitReader`].
pub trait BitSource {
    /// Append the next `len` payload bytes to `out`.
    fn fill(&mut self, len: usize, out: &mut Vec<u8>) -> Result<()>;
}

/// Payload taken from a byte slice that is already in memory.
#[derive(Clone, Debug)]
pub struct SliceSource<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceSource<'a> {
    /// Wrap `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl BitSource for SliceSource<'_> {
    fn fill(&mut self, len: usize, out: &mut Vec<u8>) -> Result<()> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| Error::corrupt("codec bit stream length overflow"))?;
        if end > self.data.len() {
            return Err(Error::corrupt(format!(
                "codec bit stream truncated: wanted {len} bytes at {}",
                self.pos
            )));
        }
        out.extend_from_slice(&self.data[self.pos..end]);
        self.pos = end;
        Ok(())
    }
}

/// Payload pulled from a [`Cursor`] on demand.
#[derive(Debug)]
pub struct CursorSource<'c, 'a> {
    cursor: &'c mut Cursor<'a>,
}

impl<'c, 'a> CursorSource<'c, 'a> {
    /// Read through `cursor`.
    pub fn new(cursor: &'c mut Cursor<'a>) -> Self {
        Self { cursor }
    }
}

impl BitSource for CursorSource<'_, '_> {
    fn fill(&mut self, len: usize, out: &mut Vec<u8>) -> Result<()> {
        let bytes = self.cursor.read_bytes(len)?;
        out.extend_from_slice(bytes);
        Ok(())
    }
}

/// A bit stream consumed MSB first.
#[derive(Debug)]
pub struct BitReader<S> {
    source: S,
    /// Payload bytes of the most recent refill.
    buffer: Vec<u8>,
    /// Bit buffer; the `bits_loaded` most significant bits are unread.
    bits_buf: u32,
    bits_loaded: usize,
    /// Byte offset in `buffer` of the next whole word.
    word_pos: usize,
    /// Whether payload words are stored little-endian.
    swap: bool,
}

impl<S: BitSource> BitReader<S> {
    fn new(source: S, swap: bool) -> Self {
        Self {
            source,
            buffer: Vec::new(),
            bits_buf: 0,
            bits_loaded: 0,
            word_pos: 0,
            swap,
        }
    }

    fn word(&mut self) -> Result<u32> {
        if self.word_pos + 4 > self.buffer.len() {
            return Err(Error::corrupt(format!(
                "codec bit stream exhausted at byte {}",
                self.word_pos
            )));
        }
        let chunk = [
            self.buffer[self.word_pos],
            self.buffer[self.word_pos + 1],
            self.buffer[self.word_pos + 2],
            self.buffer[self.word_pos + 3],
        ];
        self.word_pos += 4;
        Ok(if self.swap {
            u32::from_le_bytes(chunk)
        } else {
            u32::from_be_bytes(chunk)
        })
    }

    fn load_bits(&mut self) -> Result<()> {
        self.bits_buf = self.word()?;
        Ok(())
    }

    /// Refill so that at least `bits` bits are available, preserving leftovers.
    ///
    /// The leading partial word is merged into the bit buffer (its first byte
    /// becomes the most significant byte) and whole words continue to feed
    /// [`BitReader::read_bit`] and friends.
    pub fn load(&mut self, bits: usize) -> Result<()> {
        let new_bytes = bits
            .checked_sub(self.bits_loaded)
            .ok_or_else(|| Error::corrupt("codec bit stream asked for fewer bits than held"))?
            .div_ceil(8);
        if new_bytes == 0 {
            return Ok(());
        }
        let preload_bytes = new_bytes % 4;
        if self.bits_loaded + preload_bytes * 8 > 32 {
            return Err(Error::corrupt("codec bit buffer overflow"));
        }
        self.buffer.clear();
        self.source.fill(new_bytes, &mut self.buffer)?;
        // The partial leading word: the first byte of the payload becomes the
        // most significant byte of the bit buffer.
        let mut first: u32 = 0;
        for (index, byte) in self.buffer[..preload_bytes].iter().enumerate() {
            first |= u32::from(*byte) << (24 - index * 8);
        }
        if self.bits_loaded != 0 {
            self.bits_buf |= first >> self.bits_loaded;
            self.bits_loaded += preload_bytes * 8;
        } else {
            self.bits_buf = first;
            self.bits_loaded = preload_bytes * 8;
        }
        self.word_pos = preload_bytes;
        Ok(())
    }

    /// Refill with a whole number of words, discarding any leftover bits.
    pub fn load_words(&mut self, words: usize) -> Result<()> {
        let len = words
            .checked_mul(4)
            .ok_or_else(|| Error::corrupt("codec bit stream length overflow"))?;
        self.buffer.clear();
        self.source.fill(len, &mut self.buffer)?;
        self.bits_buf = 0;
        self.bits_loaded = 0;
        self.word_pos = 0;
        Ok(())
    }

    /// Read one bit as `0` or `1`.
    pub fn read_bit(&mut self) -> Result<u32> {
        if self.bits_loaded == 0 {
            self.load_bits()?;
            self.bits_loaded = 31;
        } else {
            self.bits_loaded -= 1;
        }
        let result = self.bits_buf >> 31;
        self.bits_buf <<= 1;
        Ok(result)
    }

    fn bit_buffer(&mut self, bits: usize) -> Result<u32> {
        if bits > 32 {
            return Err(Error::corrupt(format!(
                "codec field width {bits} exceeds 32 bits"
            )));
        }
        if self.bits_loaded >= bits {
            let result = self.bits_buf;
            self.bits_buf = shifted_left(self.bits_buf, bits);
            self.bits_loaded -= bits;
            Ok(result)
        } else if self.bits_loaded == 0 {
            self.load_bits()?;
            let result = self.bits_buf;
            self.bits_buf = shifted_left(self.bits_buf, bits);
            self.bits_loaded = 32 - bits;
            Ok(result)
        } else {
            let result = self.bits_buf;
            let head = self.bits_loaded;
            let tail = bits - head;
            self.load_bits()?;
            let result = result | (self.bits_buf >> head);
            self.bits_buf = shifted_left(self.bits_buf, tail);
            self.bits_loaded = 32 - tail;
            Ok(result)
        }
    }

    /// Read `bits` bits (1..=32) as an unsigned value.
    pub fn read_u32(&mut self, bits: usize) -> Result<u32> {
        if bits == 0 {
            return Err(Error::corrupt("codec field width must not be zero"));
        }
        Ok(self.bit_buffer(bits)? >> (32 - bits))
    }

    /// Read `bits` bits (1..=32) as a signed value.
    pub fn read_i32(&mut self, bits: usize) -> Result<i32> {
        if bits == 0 {
            return Err(Error::corrupt("codec field width must not be zero"));
        }
        Ok((self.bit_buffer(bits)? as i32) >> (32 - bits))
    }

    /// Read `bits` bits (0..=32) as an unsigned value, zero for a zero width.
    pub fn read_u32_or_0(&mut self, bits: usize) -> Result<u32> {
        if bits == 0 {
            Ok(0)
        } else {
            self.read_u32(bits)
        }
    }

    /// Read `bits` bits (0..=32) as a signed value, zero for a zero width.
    pub fn read_i32_or_0(&mut self, bits: usize) -> Result<i32> {
        if bits == 0 {
            Ok(0)
        } else {
            self.read_i32(bits)
        }
    }
}

/// Big-endian bit stream over a byte slice (probability tables).
#[cfg(test)]
#[must_use]
pub fn raw_slice(data: &[u8]) -> BitReader<SliceSource<'_>> {
    BitReader::new(SliceSource::new(data), false)
}

/// Bit stream whose words follow `order` (codec text held in a slice).
#[must_use]
pub fn u32_slice(data: &[u8], order: ByteOrder) -> BitReader<SliceSource<'_>> {
    BitReader::new(SliceSource::new(data), order == ByteOrder::Little)
}

/// Big-endian bit stream pulled from `cursor` (probability tables have no
/// length prefix, so their bytes are read as the table demands them).
pub fn raw_cursor<'c, 'a>(cursor: &'c mut Cursor<'a>) -> BitReader<CursorSource<'c, 'a>> {
    BitReader::new(CursorSource::new(cursor), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first_across_words() {
        let data = [0b1000_0000u8, 0, 0, 0, 0b1000_0000, 0, 0, 0];
        let mut reader = raw_slice(&data);
        reader.load(64).expect("load");
        assert_eq!(reader.read_bit().expect("bit"), 1);
        assert_eq!(reader.read_u32(31).expect("31 bits"), 0);
        assert_eq!(reader.read_bit().expect("bit"), 1);
        assert_eq!(reader.read_u32(31).expect("31 bits"), 0);
        assert!(reader.read_bit().is_err(), "the stream is bounded");
    }

    #[test]
    fn refill_pulls_only_the_requested_bytes() {
        // A raw refill never reads ahead: after asking for 16 bits only two
        // bytes are in the buffer, and the next refill picks up after them.
        let data = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
        let mut reader = raw_slice(&data);
        reader.load(16).expect("load");
        assert_eq!(reader.read_u32(16).expect("half word"), 0x1234);
        reader.load(32).expect("load");
        assert_eq!(reader.read_u32(32).expect("word"), 0x5678_9ABC);
        reader.load(16).expect("load");
        assert_eq!(reader.read_u32(16).expect("half word"), 0xDEF0);
    }

    #[test]
    fn partial_preload_merges_behind_the_bits_still_held() {
        // Four bits remain after the first refill; the next single byte has to
        // be stacked behind them rather than replacing them.
        let data = [0x12u8, 0x34];
        let mut reader = raw_slice(&data);
        reader.load(8).expect("load");
        assert_eq!(reader.read_u32(4).expect("nibble"), 0x1);
        reader.load(12).expect("load");
        assert_eq!(reader.read_u32(12).expect("merged"), 0x234);
    }

    #[test]
    fn little_endian_words_are_swapped() {
        let data = [0x01, 0x00, 0x00, 0x00];
        let mut reader = u32_slice(&data, ByteOrder::Little);
        reader.load_words(1).expect("load");
        assert_eq!(reader.read_u32(32).expect("word"), 1);
    }

    #[test]
    fn signed_fields_sign_extend() {
        let data = [0b1111_1111u8, 0xFF, 0xFF, 0xFF];
        let mut reader = raw_slice(&data);
        reader.load(32).expect("load");
        assert_eq!(reader.read_i32(4).expect("nibble"), -1);
    }

    #[test]
    fn cursor_source_pulls_bytes_on_demand() {
        let data = [0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut reader = raw_cursor(&mut cursor);
        reader.load(32).expect("load");
        let first = reader.read_u32(32).expect("word");
        reader.load(32).expect("load");
        let second = reader.read_u32(32).expect("word");
        // The source pulls four bytes per refill.
        assert_eq!(cursor.position(), 8);
        assert_eq!(first, 0x0001_0203);
        assert_eq!(second, 0x0405_0607);
    }

    #[test]
    fn leftover_bits_carry_between_raw_refills() {
        // One byte is loaded, one bit is consumed, then a second refill must
        // merge its leading partial word behind the seven bits still held.
        let data = [0b1011_0100u8, 0xFF];
        let mut reader = raw_slice(&data);
        reader.load(4).expect("first byte");
        assert_eq!(reader.read_bit().expect("bit"), 1);
        reader.load(15).expect("second byte");
        assert_eq!(reader.read_u32(15).expect("merged bits"), 0x34FF);
    }
}
