//! The bit-based integer codecs used by JT codec packages.
//!
//! Four codecs share the [`BitReader`] interface:
//!
//! | codec      | used by        | probability table |
//! |------------|----------------|-------------------|
//! | bitlength  | mark 1 packages | none              |
//! | bitlength2 | mark 2 packages | none              |
//! | huffman    | mark 1 packages | mark 1            |
//! | arithmetic | both            | mark 1 / 2 / 3    |
//!
//! Every function decodes exactly `count` values; the bit stream is expected to
//! be long enough, and running out of it is reported as a corrupt stream.

use super::bits::{BitReader, BitSource};
use super::prob::{Context, ESCAPE};
use crate::error::{Error, Result};

/// Field width step used by the mark 1 bitlength codec.
const STEP_SIZE: i32 = 2;

/// Turns a codec field width into a bit count, rejecting the widths the
/// reference implementation would only reach through undefined behaviour.
fn checked_width(value: i32, what: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| Error::corrupt(format!("{what}: field width went negative ({value})")))
}

/// Decodes `count` values with the mark 1 bitlength codec.
pub fn bitlength<S: BitSource>(reader: &mut BitReader<S>, count: usize) -> Result<Vec<i32>> {
    let mut out = Vec::with_capacity(count);
    let mut field_width: i32 = 0;
    while out.len() < count {
        if reader.read_bit()? != 0 {
            let first_bit = reader.read_bit()?;
            let step = if first_bit != 0 {
                STEP_SIZE
            } else {
                -STEP_SIZE
            };
            loop {
                field_width += step;
                if reader.read_bit()? != first_bit {
                    break;
                }
            }
        }
        let width = checked_width(field_width, "bitlength")?;
        out.push(reader.read_i32_or_0(width)?);
    }
    Ok(out)
}

/// Decodes `count` values with the mark 2 bitlength codec.
pub fn bitlength2<S: BitSource>(
    reader: &mut BitReader<S>,
    count: usize,
    is_v10: bool,
) -> Result<Vec<i32>> {
    if is_v10 {
        return bitlength2_v10(reader, count);
    }
    if reader.read_bit()? != 0 {
        bitlength2_variable(reader, count)
    } else {
        bitlength2_fixed(reader, count)
    }
}

/// Nibble-encoded signed integer used by the JT 10.x bitlength2 codec.
fn nibbler<S: BitSource>(reader: &mut BitReader<S>) -> Result<i32> {
    let mut result: i32 = 0;
    let mut nibbles: i32 = 0;
    loop {
        let part = reader.read_u32(4)?;
        result |= (part << (nibbles as u32 * 4)) as i32;
        let more = reader.read_u32(1)?;
        nibbles += 1;
        if more == 0 || nibbles >= 8 {
            break;
        }
    }
    let width = nibbles * 4;
    if width < 32 {
        let shift = (32 - width) as u32;
        result = ((result as u32) << shift) as i32 >> shift;
    }
    Ok(result)
}

fn bitlength2_v10<S: BitSource>(reader: &mut BitReader<S>, count: usize) -> Result<Vec<i32>> {
    let mut out = Vec::with_capacity(count);
    if reader.read_bit()? == 0 {
        let min = nibbler(reader)?;
        let max = nibbler(reader)?;
        let mut span = max.wrapping_sub(min) as u32;
        let mut width: u32 = 0;
        while span != 0 {
            width += 1;
            span >>= 1;
        }
        while out.len() < count {
            let residual = if width == 0 {
                0
            } else {
                reader.read_u32(width as usize)?
            };
            out.push((residual as i32).wrapping_add(min));
        }
        return Ok(out);
    }

    let mean = nibbler(reader)?;
    let mut field_width: i32 = 0;
    while out.len() < count {
        let mut change;
        loop {
            change = reader.read_i32(4)?;
            field_width += change;
            if change != -8 && change != 7 {
                break;
            }
        }
        let run_len = reader.read_u32(4)? as usize;
        let run_end = (out.len() + run_len).min(count);
        let width = checked_width(field_width, "bitlength2 (v10)")?;
        while out.len() < run_end {
            let residual = if width == 0 {
                0
            } else {
                reader.read_i32(width)?
            };
            out.push(residual.wrapping_add(mean));
        }
    }
    Ok(out)
}

fn bitlength2_fixed<S: BitSource>(reader: &mut BitReader<S>, count: usize) -> Result<Vec<i32>> {
    let min_bits = reader.read_u32(6)?;
    let max_bits = reader.read_u32(6)?;
    if min_bits > 32 || max_bits > 32 {
        return Err(Error::corrupt(format!(
            "bitlength2: symbol widths {min_bits}/{max_bits} exceed 32 bits"
        )));
    }
    let min = reader.read_i32_or_0(min_bits as usize)?;
    let max = reader.read_i32_or_0(max_bits as usize)?;
    let range = max.wrapping_sub(min);
    let mut out = Vec::with_capacity(count);
    if range <= 0 {
        out.resize(count, min);
        return Ok(out);
    }
    let mut field_width: u32 = 1;
    let mut rest = range >> 1;
    while rest != 0 {
        field_width += 1;
        rest >>= 1;
    }
    while out.len() < count {
        let residual = reader.read_u32(field_width as usize)?;
        out.push(residual.wrapping_add(min as u32) as i32);
    }
    Ok(out)
}

fn bitlength2_variable<S: BitSource>(reader: &mut BitReader<S>, count: usize) -> Result<Vec<i32>> {
    let mean = reader.read_i32(32)?;
    let change_bits = reader.read_u32(3)?;
    let run_bits = reader.read_u32(3)?;
    if change_bits == 0 || run_bits == 0 {
        return Err(Error::corrupt(format!(
            "bitlength2: control widths {change_bits}/{run_bits} must not be zero"
        )));
    }
    let half = 1i32 << (change_bits - 1);
    let max_decrease = -half;
    let max_increase = half - 1;

    let mut field_width: i32 = 0;
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let mut change;
        loop {
            change = reader.read_i32(change_bits as usize)?;
            field_width += change;
            if change != max_decrease && change != max_increase {
                break;
            }
        }
        // The reference does not clamp the run length; the outer bound stops it.
        let run_len = reader.read_u32(run_bits as usize)? as usize;
        let run_end = out.len() + run_len;
        let width = checked_width(field_width, "bitlength2")?;
        while out.len() < run_end && out.len() < count {
            out.push(if width == 0 {
                mean
            } else {
                reader.read_i32(width)?.wrapping_add(mean)
            });
        }
    }
    Ok(out)
}

/// One node of a Huffman tree, kept in a flat arena.
#[derive(Clone, Copy, Debug)]
struct Node {
    occurrences: i32,
    left: Option<usize>,
    right: Option<usize>,
    symbol: i32,
    value: i32,
}

impl Node {
    fn is_leaf(self) -> bool {
        self.left.is_none() && self.right.is_none()
    }
}

/// Inserts `index` into the heap, reproducing the reference sift-up exactly.
fn heap_add(nodes: &[Node], heap: &mut Vec<usize>, index: usize) {
    heap.push(index);
    let mut i = heap.len();
    while i != 1 && nodes[heap[(i / 2) - 1]].occurrences > nodes[index].occurrences {
        heap[i - 1] = heap[(i / 2) - 1];
        i /= 2;
    }
    heap[i - 1] = index;
}

/// Removes the root of the heap, reproducing the reference sift-down exactly.
fn heap_remove(nodes: &[Node], heap: &mut Vec<usize>) {
    if heap.is_empty() {
        return;
    }
    let mut size = heap.len();
    let y = heap[size - 1];
    let mut i = 1;
    let mut child = 2;
    size -= 1;
    while child <= size {
        if child < size && nodes[heap[child - 1]].occurrences > nodes[heap[child]].occurrences {
            child += 1;
        }
        if nodes[y].occurrences < nodes[heap[child - 1]].occurrences {
            break;
        }
        heap[i - 1] = heap[child - 1];
        i = child;
        child *= 2;
    }
    heap[i - 1] = y;
    heap.pop();
}

/// The smallest node, removing it from the heap.
fn heap_top(nodes: &[Node], heap: &mut Vec<usize>) -> Option<usize> {
    if heap.is_empty() {
        return None;
    }
    let index = heap[0];
    heap_remove(nodes, heap);
    Some(index)
}

/// Builds the Huffman tree implied by `context`.
fn build_tree(context: &Context) -> Result<(Vec<Node>, usize)> {
    let mut nodes: Vec<Node> = Vec::with_capacity(context.entries().len() * 2);
    let mut heap: Vec<usize> = Vec::with_capacity(context.entries().len());
    for entry in context.entries() {
        nodes.push(Node {
            occurrences: entry.occurrences as i32,
            left: None,
            right: None,
            symbol: entry.symbol,
            value: entry.value,
        });
        let index = nodes.len() - 1;
        heap_add(&nodes, &mut heap, index);
    }
    if nodes.is_empty() {
        return Err(Error::corrupt("huffman: probability table is empty"));
    }
    if nodes.len() == 1 {
        return Err(Error::corrupt(format!(
            "huffman: probability table has a single symbol: {}",
            context.entries()[0].value
        )));
    }
    while heap.len() > 1 {
        let first = heap_top(&nodes, &mut heap).expect("heap is not empty");
        let second = heap_top(&nodes, &mut heap).expect("heap is not empty");
        let occurrences = nodes[first]
            .occurrences
            .wrapping_add(nodes[second].occurrences);
        nodes.push(Node {
            occurrences,
            left: Some(first),
            right: Some(second),
            symbol: 0xdead_beefu32 as i32,
            value: 0,
        });
        let index = nodes.len() - 1;
        heap_add(&nodes, &mut heap, index);
    }
    let root = heap_top(&nodes, &mut heap).expect("heap holds the root");
    Ok((nodes, root))
}

/// Decodes `count` values with the Huffman codec.
pub fn huffman<S: BitSource>(
    reader: &mut BitReader<S>,
    count: usize,
    context: &Context,
    oob: &[i32],
) -> Result<Vec<i32>> {
    let (nodes, root) = build_tree(context)?;
    let mut out = Vec::with_capacity(count);
    let mut oob_pos = 0usize;
    while out.len() < count {
        let mut index = root;
        loop {
            let bit = reader.read_bit()?;
            let next = if bit != 0 {
                nodes[index].left
            } else {
                nodes[index].right
            };
            index = next.ok_or_else(|| Error::corrupt("huffman: code does not match the tree"))?;
            if nodes[index].is_leaf() {
                break;
            }
        }
        if nodes[index].symbol != ESCAPE {
            out.push(nodes[index].value);
        } else {
            out.push(oob.get(oob_pos).copied().ok_or_else(|| {
                Error::corrupt(format!(
                    "huffman: out-of-band data exhausted (need {} have {}, {} values so far)",
                    oob_pos + 1,
                    oob.len(),
                    out.len()
                ))
            })?);
            oob_pos += 1;
        }
    }
    Ok(out)
}

/// Decodes `count` values with the arithmetic codec.
pub fn arithmetic<S: BitSource>(
    reader: &mut BitReader<S>,
    count: usize,
    contexts: &[Context],
    oob: &[i32],
) -> Result<Vec<i32>> {
    if contexts.is_empty() {
        return Err(Error::corrupt("arithmetic: no probability table"));
    }
    let mut current = 0usize;
    let mut low: u16 = 0x0000;
    let mut high: u16 = 0xFFFF;
    let mut code: u16 = reader.read_u32(16)? as u16;

    let mut out = Vec::with_capacity(count);
    let mut oob_pos = 0usize;
    while out.len() < count {
        let context = contexts
            .get(current)
            .ok_or_else(|| Error::corrupt(format!("arithmetic: context {current} is unknown")))?;
        let entries = context.entries();
        if entries.is_empty() {
            return Err(Error::corrupt("arithmetic: probability table is empty"));
        }
        let total = context.total();
        if total == 0 {
            return Err(Error::corrupt(
                "arithmetic: probability table has no occurrences",
            ));
        }

        // The reference promotes to `int` first, so a reversed range becomes a
        // huge unsigned value rather than wrapping at 16 bits.
        let range = (i32::from(high) - i32::from(low) + 1) as u32;
        if range == 0 {
            return Err(Error::corrupt("arithmetic: empty range"));
        }
        let target = ((i32::from(code) - i32::from(low) + 1) as u32)
            .wrapping_mul(total)
            .wrapping_sub(1)
            / range;

        let mut position = 0usize;
        let mut symbol_high = entries[0].occurrences;
        while symbol_high <= target {
            position += 1;
            let entry = entries
                .get(position)
                .ok_or_else(|| Error::corrupt("arithmetic: symbol lookup left the table"))?;
            symbol_high = symbol_high.wrapping_add(entry.occurrences);
        }
        let entry = entries[position];
        let symbol_low = symbol_high.wrapping_sub(entry.occurrences);

        if entry.symbol != ESCAPE {
            out.push(entry.value);
        } else if current == 0 {
            out.push(
                oob.get(oob_pos)
                    .copied()
                    .ok_or_else(|| Error::corrupt("arithmetic: out-of-band data exhausted"))?,
            );
            oob_pos += 1;
        }
        current = entry.next_context as usize;

        high = (i32::from(low) + i32::from((range.wrapping_mul(symbol_high) / total) as u16) - 1)
            as u16;
        low = (i32::from(low) + i32::from((range.wrapping_mul(symbol_low) / total) as u16)) as u16;

        // Shift out the leading bits the two bounds agree on.
        let mask = high ^ low;
        if mask & 0x8000 == 0 {
            let mut shift = 1u32;
            let mut probe = !mask;
            while probe & 0x4000 != 0 {
                probe <<= 1;
                shift += 1;
            }
            low = low.wrapping_shl(shift);
            high = !((u32::from(!high)) << shift) as u16;
            code = code.wrapping_shl(shift);
            code |= reader.read_u32(shift as usize)? as u16;
        }

        // Shift out the second leading bit when the bounds straddle it.
        if high & 0x4000 == 0 && low & 0x4000 != 0 {
            let mut shift = 1u32;
            let mut probe = 0x2000u16;
            while high & probe == 0 && low & probe != 0 {
                probe >>= 1;
                shift += 1;
            }
            low = low.wrapping_shl(shift);
            high = !((u32::from(!high)) << shift) as u16;
            code = code.wrapping_shl(shift);
            code |= reader.read_u32(shift as usize)? as u16;
            low &= 0x7FFF;
            high |= 0x8000;
            code ^= 0x8000;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::bits::u32_slice;
    use super::*;
    use crate::codec::ByteOrder;
    use crate::jt::decode::prob::Entry;

    /// Big-endian words, matching the mark 1 probability streams.
    fn reader(words: &[u32]) -> (Vec<u8>, Vec<u8>) {
        let mut bytes = Vec::with_capacity(words.len() * 4);
        for word in words {
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        (bytes, Vec::new())
    }

    #[test]
    fn bitlength_decodes_a_single_run() {
        // Prefix "0" then a zero-width field, repeated: every value is 0.
        let (bytes, _) = reader(&[0x0000_0000]);
        let mut bits = u32_slice(&bytes, ByteOrder::Big);
        bits.load_words(1).expect("load");
        let values = bitlength(&mut bits, 4).expect("decode");
        assert_eq!(values, [0, 0, 0, 0]);
    }

    #[test]
    fn bitlength_grows_the_field_width() {
        // Each "1 1 0" prefix widens the field by two bits: the first value is
        // read as a 2-bit field, the second as a 4-bit field.
        let mut bits_stream: Vec<u32> = Vec::new();
        let push = |value: u32, width: usize, out: &mut Vec<u32>| {
            for shift in (0..width).rev() {
                out.push((value >> shift) & 1);
            }
        };
        push(0b110, 3, &mut bits_stream); // prefix: width += 2
        push(0b11, 2, &mut bits_stream); // value 3, sign extended to -1
        push(0b110, 3, &mut bits_stream); // prefix: width += 2 again
        push(0b0001, 4, &mut bits_stream); // value 1
        while bits_stream.len() % 32 != 0 {
            bits_stream.push(0);
        }
        let mut word = 0u32;
        let mut bytes = Vec::new();
        for (index, bit) in bits_stream.iter().enumerate() {
            word |= bit << (31 - (index % 32));
            if index % 32 == 31 {
                bytes.extend_from_slice(&word.to_be_bytes());
                word = 0;
            }
        }
        if !bytes.is_empty() {
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        let mut bits = u32_slice(&bytes, ByteOrder::Big);
        bits.load_words(bytes.len() / 4).expect("load");
        let values = bitlength(&mut bits, 2).expect("decode");
        assert_eq!(values, [-1, 1]);
    }

    #[test]
    fn bitlength2_fixed_fills_the_min_when_the_range_is_empty() {
        let mut stream: Vec<u32> = Vec::new();
        let push = |value: u32, width: usize, out: &mut Vec<u32>| {
            for shift in (0..width).rev() {
                out.push((value >> shift) & 1);
            }
        };
        push(0, 1, &mut stream); // fixed width mode
        push(4, 6, &mut stream); // min bits
        push(4, 6, &mut stream); // max bits
        push(5, 4, &mut stream); // min value
        push(5, 4, &mut stream); // max value
        while stream.len() % 32 != 0 {
            stream.push(0);
        }
        let mut bytes = Vec::new();
        let mut word = 0u32;
        for (index, bit) in stream.iter().enumerate() {
            word |= bit << (31 - (index % 32));
            if index % 32 == 31 {
                bytes.extend_from_slice(&word.to_be_bytes());
                word = 0;
            }
        }
        let mut bits = u32_slice(&bytes, ByteOrder::Big);
        bits.load_words(bytes.len() / 4).expect("load");
        let values = bitlength2(&mut bits, 3, false).expect("decode");
        assert_eq!(values, [5, 5, 5]);
    }

    #[test]
    fn huffman_walks_the_tree() {
        // Two symbols with equal weights: the heap keeps the first entry as the
        // left child, so bit 1 selects it.
        let context = Context::new(vec![
            Entry {
                symbol: 0,
                occurrences: 1,
                value: 7,
                next_context: 0,
            },
            Entry {
                symbol: 1,
                occurrences: 1,
                value: 9,
                next_context: 0,
            },
        ]);
        let (bytes, _) = reader(&[0b10 << 30]);
        let mut bits = u32_slice(&bytes, ByteOrder::Big);
        bits.load_words(1).expect("load");
        let values = huffman(&mut bits, 2, &context, &[]).expect("decode");
        assert_eq!(values, [7, 9]);
    }

    #[test]
    fn arithmetic_decodes_a_uniform_context() {
        // One symbol covering the whole range: every value is the same.
        let context = Context::new(vec![Entry {
            symbol: 0,
            occurrences: 10,
            value: 42,
            next_context: 0,
        }]);
        let (bytes, _) = reader(&[0x0000_0000, 0x0000_0000]);
        let mut bits = u32_slice(&bytes, ByteOrder::Big);
        bits.load_words(2).expect("load");
        let values = arithmetic(&mut bits, 3, &[context], &[]).expect("decode");
        assert_eq!(values, [42, 42, 42]);
    }
}
