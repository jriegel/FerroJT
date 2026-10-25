//! Integer codec packages (JT "CDP" blocks).
//!
//! A package stores a sequence of integers in one of two framings:
//!
//! * *mark 1* (`loadCDP1`), used by the vertex data of JT 9.x,
//! * *mark 2* (`loadCDP2`), used by the mesh coder and by JT 10.x vertex data.
//!
//! Both framings wrap one of the codecs in [`super::codecs`], optionally with a
//! nested out-of-band package for the values the probability table cannot
//! express. The compressed words are kept as loaded and only turned into
//! integers by [`Package::decode`], which consumes the package.

use super::bits::{raw_cursor, u32_slice};
use super::codecs;
use super::prob::Context;
use super::unpack::Unpack;
use crate::codec::ByteOrder;
use crate::error::{Error, Result};
use crate::jt::reader::Cursor;

/// Codec id used by both framings.
const CODEC_NULL: u8 = 0;
/// Mark 1 bitlength codec.
const CODEC_BITLENGTH: u8 = 1;
/// Mark 1 Huffman codec.
const CODEC_HUFFMAN: u8 = 2;
/// Arithmetic codec, used by both framings.
const CODEC_ARITHMETIC: u8 = 3;
/// Mark 2 split value codec.
const CODEC_CHOPPER: u8 = 4;

/// The bit-based codecs reachable from a package header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Mark 1 bitlength.
    Bitlength,
    /// Mark 2 bitlength, with the JT 10.x variant selected by the file version.
    Bitlength2 {
        /// Whether the JT 10.x nibbler framing is used.
        is_v10: bool,
    },
    /// Mark 1 Huffman.
    Huffman,
    /// Arithmetic.
    Arithmetic,
}

/// The payload of a loaded package.
#[derive(Clone, Debug)]
enum Payload {
    /// Values stored verbatim as 32-bit words.
    Null {
        /// The stored words.
        words: Vec<u32>,
    },
    /// A bit-based codec with its compressed words.
    Bits {
        /// Which codec to run.
        kind: Kind,
        /// Probability tables.
        contexts: Vec<Context>,
        /// Nested package supplying the escaped values.
        oob: Option<Box<Package>>,
        /// How many values the codec produces.
        out_count: usize,
        /// The compressed words, in file byte order.
        code_text: Vec<u8>,
    },
    /// A mark 2 split value package.
    Chopper {
        /// High bits.
        msb: Box<Package>,
        /// Low bits.
        lsb: Box<Package>,
        /// Left shift applied to the high bits.
        shift: u32,
        /// Bias added to the result.
        bias: i32,
    },
}

/// An integer codec package.
#[derive(Clone, Debug)]
pub struct Package {
    payload: Option<Payload>,
}

impl Package {
    /// A package holding `words` verbatim.
    #[must_use]
    pub fn null(words: Vec<u32>) -> Self {
        Self {
            payload: Some(Payload::Null { words }),
        }
    }

    /// An empty package, which decodes to nothing.
    #[must_use]
    pub const fn empty() -> Self {
        Self { payload: None }
    }

    /// True when the package holds no payload.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.payload.is_none()
    }

    /// The number of values [`Package::decode`] produces.
    ///
    /// Must be queried before decoding, which consumes the payload.
    #[must_use]
    pub fn out_val_count(&self) -> usize {
        match self.payload.as_ref() {
            None => 0,
            Some(Payload::Null { words }) => words.len(),
            Some(Payload::Bits { out_count, .. }) => *out_count,
            Some(Payload::Chopper { msb, .. }) => msb.out_val_count(),
        }
    }

    /// Reads a mark 1 package from `cursor`.
    pub fn load1(cursor: &mut Cursor<'_>) -> Result<Self> {
        let codec = cursor.read_u8()?;
        if codec == CODEC_NULL {
            let count = cursor.read_len()?;
            let words = cursor.read_u32_vec(count)?;
            return Ok(Self::null(words));
        }
        let kind = match codec {
            CODEC_BITLENGTH => Kind::Bitlength,
            CODEC_HUFFMAN => Kind::Huffman,
            CODEC_ARITHMETIC => Kind::Arithmetic,
            other => {
                return Err(cursor.error(format!(
                    "unknown mark 1 codec {other} in an integer codec package"
                )));
            }
        };

        let mut contexts = Vec::new();
        let mut oob = None;
        let mut context_count = 0u8;
        if codec > CODEC_BITLENGTH {
            context_count = cursor.read_u8()?;
            contexts = Vec::with_capacity(usize::from(context_count));
            // The probability tables have no length prefix: their bytes are
            // pulled from the cursor as the tables ask for them, and any bits
            // left over when the last table is read are discarded.
            {
                let mut table_reader = raw_cursor(cursor);
                let mut first = Context::default();
                first.read_mark1_first(&mut table_reader)?;
                let template = first.clone();
                contexts.push(first);
                for _ in 1..usize::from(context_count) {
                    let mut context = Context::default();
                    context.read_mark1_next(&mut table_reader, &template)?;
                    contexts.push(context);
                }
            }

            // The out-of-band count is read without validation; a negative
            // value simply means "no out-of-band package".
            let oob_count = cursor.read_i32()?;
            if oob_count > 0 {
                oob = Some(Box::new(Self::load1(cursor)?));
            }
        }

        // The code text length is stored but never used: the payload carries
        // its own word count.
        let _code_text_length = cursor.read_i32()?;
        let out_count_raw = cursor.read_i32()?;
        if out_count_raw < 0 {
            return Err(cursor.error(format!(
                "negative value count {out_count_raw} in an integer codec package"
            )));
        }
        let out_count = out_count_raw as usize;
        if context_count > 1 {
            let _symbol_count = cursor.read_i32()?;
        }

        let word_count = cursor.read_i32()?;
        if word_count < 0 {
            return Err(cursor.error(format!(
                "negative code text length {word_count} in an integer codec package"
            )));
        }
        let code_text = read_words(cursor, word_count as usize)?;

        Ok(Self {
            payload: Some(Payload::Bits {
                kind,
                contexts,
                oob,
                out_count,
                code_text,
            }),
        })
    }

    /// Reads a mark 2 package from `cursor`.
    pub fn load2(cursor: &mut Cursor<'_>) -> Result<Self> {
        let value_count_raw = cursor.read_i32()?;
        if value_count_raw == 0 {
            return Ok(Self::null(Vec::new()));
        }
        if value_count_raw < 0 {
            return Err(cursor.error(format!(
                "negative value count {value_count_raw} in an integer codec package"
            )));
        }
        let value_count = value_count_raw as usize;
        let codec = cursor.read_u8()?;

        if codec == CODEC_CHOPPER {
            let chop = cursor.read_u8()?;
            if chop == 0 {
                return Self::load2(cursor);
            }
            let bias = cursor.read_i32()?;
            let span = cursor.read_u8()?;
            let msb = Box::new(Self::load2(cursor)?);
            let lsb = Box::new(Self::load2(cursor)?);
            if msb.out_val_count() != value_count || lsb.out_val_count() != value_count {
                return Err(cursor.error(format!(
                    "chopped data size mismatch: expected {value_count}, got {} and {}",
                    msb.out_val_count(),
                    lsb.out_val_count()
                )));
            }
            // The reference stores the shift in a byte, so it wraps.
            let shift = u32::from(span.wrapping_sub(chop));
            return Ok(Self {
                payload: Some(Payload::Chopper {
                    msb,
                    lsb,
                    shift,
                    bias,
                }),
            });
        }

        if codec != CODEC_NULL && codec != CODEC_BITLENGTH && codec != CODEC_ARITHMETIC {
            return Err(cursor.error(format!(
                "unknown mark 2 codec {codec} in an integer codec package"
            )));
        }

        let code_text_length = cursor.read_i32()?;
        if code_text_length <= 0 {
            return Err(cursor.error(format!(
                "invalid code text length {code_text_length} in an integer codec package"
            )));
        }
        let words = usize::try_from((code_text_length + 31) / 32)
            .map_err(|_| cursor.error("code text length overflow"))?;

        if codec == CODEC_NULL {
            let words = cursor.read_u32_vec(words)?;
            return Ok(Self::null(words));
        }

        let kind = if codec == CODEC_BITLENGTH {
            Kind::Bitlength2 {
                is_v10: cursor.major() >= 10,
            }
        } else {
            Kind::Arithmetic
        };

        // The compressed words come before the probability table.
        let code_text = read_words(cursor, words)?;

        let mut contexts = Vec::new();
        let mut oob = None;
        if codec == CODEC_ARITHMETIC {
            let is_v10 = cursor.major() >= 10;
            {
                let mut table_reader = raw_cursor(cursor);
                let mut context = Context::default();
                if is_v10 {
                    context.read_mark3(&mut table_reader)?;
                } else {
                    context.read_mark2(&mut table_reader)?;
                }
                contexts.push(context);
            }
            let has_escape = !is_v10 || contexts[0].has_escape();
            if has_escape {
                oob = Some(Box::new(Self::load2(cursor)?));
            }
        }

        Ok(Self {
            payload: Some(Payload::Bits {
                kind,
                contexts,
                oob,
                out_count: value_count,
                code_text,
            }),
        })
    }

    /// Decodes the package and applies `unpack` to the result.
    ///
    /// The payload is consumed: decoding the same package twice yields nothing.
    pub fn decode(&mut self, order: ByteOrder, unpack: Unpack) -> Result<Vec<i32>> {
        let Some(payload) = self.payload.take() else {
            return Ok(Vec::new());
        };
        let mut values = match payload {
            Payload::Null { words } => words.iter().map(|word| *word as i32).collect(),
            Payload::Chopper {
                mut msb,
                mut lsb,
                shift,
                bias,
            } => {
                let high = msb.decode(order, Unpack::Null)?;
                let low = lsb.decode(order, Unpack::Null)?;
                high.iter()
                    .zip(low.iter())
                    .map(|(hi, lo)| (hi.wrapping_shl(shift) | *lo).wrapping_add(bias))
                    .collect()
            }
            Payload::Bits {
                kind,
                contexts,
                mut oob,
                out_count,
                code_text,
            } => {
                let oob_values = match oob.as_mut() {
                    Some(package) => package.decode(order, Unpack::Null)?,
                    None => Vec::new(),
                };
                let mut reader = u32_slice(&code_text, order);
                reader.load_words(code_text.len() / 4)?;
                match kind {
                    Kind::Bitlength => codecs::bitlength(&mut reader, out_count)?,
                    Kind::Bitlength2 { is_v10 } => {
                        codecs::bitlength2(&mut reader, out_count, is_v10)?
                    }
                    Kind::Huffman => {
                        let context = contexts.first().ok_or_else(|| {
                            Error::corrupt("huffman package without a probability table")
                        })?;
                        codecs::huffman(&mut reader, out_count, context, &oob_values)?
                    }
                    Kind::Arithmetic => {
                        codecs::arithmetic(&mut reader, out_count, &contexts, &oob_values)?
                    }
                }
            }
        };
        unpack.apply(&mut values);
        Ok(values)
    }
}

/// Reads `words` 32-bit words as raw bytes, keeping the file byte order.
fn read_words(cursor: &mut Cursor<'_>, words: usize) -> Result<Vec<u8>> {
    let len = words
        .checked_mul(4)
        .ok_or_else(|| cursor.error("code text length overflow"))?;
    Ok(cursor.read_bytes(len)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn little_endian(values: &[i32]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }

    #[test]
    fn mark1_null_package_round_trips() {
        let mut data = vec![CODEC_NULL];
        data.extend_from_slice(&3i32.to_le_bytes());
        data.extend_from_slice(&little_endian(&[1, -2, 3]));
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut package = Package::load1(&mut cursor).expect("load");
        assert_eq!(package.out_val_count(), 3);
        let values = package
            .decode(ByteOrder::Little, Unpack::Null)
            .expect("decode");
        assert_eq!(values, [1, -2, 3]);
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn mark2_null_package_uses_the_byte_length() {
        // 96 bits = 3 words, read without a count prefix.
        let mut data = Vec::new();
        data.extend_from_slice(&3i32.to_le_bytes());
        data.push(CODEC_NULL);
        data.extend_from_slice(&96i32.to_le_bytes());
        data.extend_from_slice(&little_endian(&[7, 8, 9]));
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut package = Package::load2(&mut cursor).expect("load");
        assert_eq!(package.out_val_count(), 3);
        let values = package
            .decode(ByteOrder::Little, Unpack::Null)
            .expect("decode");
        assert_eq!(values, [7, 8, 9]);
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn mark2_empty_value_count_is_a_null_package() {
        let data = 0i32.to_le_bytes().to_vec();
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let package = Package::load2(&mut cursor).expect("load");
        assert_eq!(package.out_val_count(), 0);
    }

    #[test]
    fn chopper_combines_high_and_low_bits() {
        // chop = 3, span = 5 -> shift 2, bias 100; two values of two bits each.
        let mut data = Vec::new();
        data.extend_from_slice(&2i32.to_le_bytes()); // value count
        data.push(CODEC_CHOPPER);
        data.push(3); // chop
        data.extend_from_slice(&100i32.to_le_bytes()); // bias
        data.push(5); // span
        // High bits 1 and 2, low bits 0 and 1.
        let high = little_endian(&[1, 2]);
        let low = little_endian(&[0, 1]);
        for words in [&high, &low] {
            data.extend_from_slice(&2i32.to_le_bytes()); // value count
            data.push(CODEC_NULL);
            data.extend_from_slice(&64i32.to_le_bytes()); // 64 bits = 2 words
            data.extend_from_slice(words);
        }
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut package = Package::load2(&mut cursor).expect("load");
        assert_eq!(package.out_val_count(), 2);
        let values = package
            .decode(ByteOrder::Little, Unpack::Null)
            .expect("decode");
        assert_eq!(values, [104, 109]);
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn unpack_is_applied_after_decoding() {
        let mut data = vec![CODEC_NULL];
        data.extend_from_slice(&5i32.to_le_bytes());
        data.extend_from_slice(&little_endian(&[0, 0, 0, 0, 5]));
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let mut package = Package::load1(&mut cursor).expect("load");
        let values = package
            .decode(ByteOrder::Little, Unpack::Lag1)
            .expect("decode");
        assert_eq!(values, [0, 0, 0, 0, 5]);
    }
}
