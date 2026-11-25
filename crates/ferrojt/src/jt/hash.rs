//! The hash function the JT specification prescribes for its check values.
//!
//! Annex E of the PSI 14 part 1 specification hands out a reference
//! implementation of Bob Jenkins' lookup2 hash and states that a reader
//! *should* compare every stored check value against one it computes itself.
//! Strict consumers do: a shape whose stored hash does not match is not drawn
//! at all, so a writer has to fill these fields with real values rather than
//! zeros.
//!
//! The seed carries the hash of the previous field, which is how a reader
//! combines several fields — the order is fixed by the specification and is
//! part of the contract.

/// The golden-ratio word the hash starts its two accumulators from.
const WORD: u32 = 0x9e37_79b9;

/// The avalanche step of the hash, transcribed from the specification.
fn mix(mut a: u32, mut b: u32, mut c: u32) -> (u32, u32, u32) {
    a = a.wrapping_sub(b);
    a = a.wrapping_sub(c);
    a ^= c >> 13;
    b = b.wrapping_sub(c);
    b = b.wrapping_sub(a);
    b ^= a << 8;
    c = c.wrapping_sub(a);
    c = c.wrapping_sub(b);
    c ^= b >> 13;
    a = a.wrapping_sub(b);
    a = a.wrapping_sub(c);
    a ^= c >> 12;
    b = b.wrapping_sub(c);
    b = b.wrapping_sub(a);
    b ^= a << 16;
    c = c.wrapping_sub(a);
    c = c.wrapping_sub(b);
    c ^= b >> 5;
    a = a.wrapping_sub(b);
    a = a.wrapping_sub(c);
    a ^= c >> 3;
    b = b.wrapping_sub(c);
    b = b.wrapping_sub(a);
    b ^= a << 10;
    c = c.wrapping_sub(a);
    c = c.wrapping_sub(b);
    c ^= b >> 15;
    (a, b, c)
}

/// The hash of an array of 32-bit words, continued from `seed`.
#[must_use]
pub(crate) fn hash32(words: &[u32], seed: u32) -> u32 {
    let mut a = WORD;
    let mut b = WORD;
    let mut c = seed;
    let mut rest = words;
    while rest.len() >= 3 {
        a = a.wrapping_add(rest[0]);
        b = b.wrapping_add(rest[1]);
        c = c.wrapping_add(rest[2]);
        (a, b, c) = mix(a, b, c);
        rest = &rest[3..];
    }
    c = c.wrapping_add(words.len() as u32);
    match rest {
        [lo, hi] => {
            a = a.wrapping_add(*lo);
            b = b.wrapping_add(*hi);
        }
        [lo] => a = a.wrapping_add(*lo),
        _ => {}
    }
    mix(a, b, c).2
}

/// The hash of an array of 16-bit words, continued from `seed`. The
/// specification prescribes this variant for the vertex flags of the topology —
/// the only symbol stream it hashes at half width.
#[must_use]
pub(crate) fn hash16(words: &[u16], seed: u32) -> u32 {
    let mut a = WORD;
    let mut b = WORD;
    let mut c = seed;
    let mut rest = words;
    while rest.len() >= 6 {
        a = a.wrapping_add(u32::from(rest[0]) | (u32::from(rest[1]) << 16));
        b = b.wrapping_add(u32::from(rest[2]) | (u32::from(rest[3]) << 16));
        c = c.wrapping_add(u32::from(rest[4]) | (u32::from(rest[5]) << 16));
        (a, b, c) = mix(a, b, c);
        rest = &rest[6..];
    }
    c = c.wrapping_add(words.len() as u32);
    // The tail as the specification transcribes it: a fall-through switch, so a
    // short key fills the accumulators from the bottom and the fifth word lands
    // in the high half of `c`.
    let word = |index: usize| -> u32 { u32::from(rest[index]) };
    match rest.len() {
        5 => {
            c = c.wrapping_add(word(4) << 16);
            b = b.wrapping_add(word(3) << 16);
            b = b.wrapping_add(word(2));
            a = a.wrapping_add(word(1) << 16);
            a = a.wrapping_add(word(0));
        }
        4 => {
            b = b.wrapping_add(word(3) << 16);
            b = b.wrapping_add(word(2));
            a = a.wrapping_add(word(1) << 16);
            a = a.wrapping_add(word(0));
        }
        3 => {
            b = b.wrapping_add(word(2));
            a = a.wrapping_add(word(1) << 16);
            a = a.wrapping_add(word(0));
        }
        2 => {
            a = a.wrapping_add(word(1) << 16);
            a = a.wrapping_add(word(0));
        }
        1 => a = a.wrapping_add(word(0)),
        _ => {}
    }
    mix(a, b, c).2
}

/// The eight context groups of a symbol stream as slices, for
/// [`TopologySymbols`].
pub(crate) fn slices(values: &[Vec<i32>]) -> [&[i32]; 8] {
    let empty: &[i32] = &[];
    let mut out = [empty; 8];
    for (slot, values) in out.iter_mut().zip(values) {
        *slot = values;
    }
    out
}

/// The decoded symbol streams of one topologically compressed shape, as the
/// hash consumes them.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TopologySymbols<'a> {
    pub(crate) degrees: [&'a [i32]; 8],
    pub(crate) valences: &'a [i32],
    pub(crate) groups: &'a [i32],
    pub(crate) face_flags: &'a [i32],
    pub(crate) attr_masks: [&'a [i32]; 8],
    /// The 32 most significant bits of the context-7 masks, one per mask symbol.
    pub(crate) attr_masks_high: &'a [i32],
    /// The second auxiliary stream of context 7, which only JT 8.x and 9.x
    /// store — see [`composite_hash`] for why it matters to the hash.
    pub(crate) attr_masks_aux: &'a [i32],
    pub(crate) attr_masks_large: &'a [u32],
    pub(crate) split_faces: &'a [i32],
    pub(crate) split_positions: &'a [i32],
}

/// The composite hash of a whole topology, which the specification requires a
/// writer to store so that a reader can prove it decoded the symbol streams
/// correctly.
///
/// The fields are hashed in the order the specification fixes — the eight degree
/// arrays, the valences, the groups, the vertex flags as 16-bit words, the
/// attribute masks of contexts 0 to 6, the low and then the high half of the
/// context-7 masks, the wide masks of the high-degree faces, and finally the two
/// split streams — each step seeded with the hash of the previous one. The order
/// is part of the contract: a strict consumer refuses to draw a shape whose
/// stored value does not match.
///
/// `auxiliary_pair` says whether the file stores one or two auxiliary
/// attribute-mask streams for context 7. JT 8.x and 9.x store a pair and hash
/// both, JT 10.x folds them into one and hashes only that — and hashing an
/// empty array is not a no-op, so the call cannot simply be made with nothing
/// in it. The published pseudocode shows only the single call, which is why a
/// nine-revision file written from it fails the check of a strict viewer.
#[must_use]
pub(crate) fn composite_hash(symbols: &TopologySymbols<'_>, auxiliary_pair: bool) -> u32 {
    let words = |values: &[i32]| -> Vec<u32> { values.iter().map(|&value| value as u32).collect() };
    let mut hash = 0;
    for context in 0..8 {
        hash = hash32(&words(symbols.degrees[context]), hash);
    }
    hash = hash32(&words(symbols.valences), hash);
    hash = hash32(&words(symbols.groups), hash);
    let flags: Vec<u16> = symbols
        .face_flags
        .iter()
        .map(|&value| value as u16)
        .collect();
    hash = hash16(&flags, hash);
    for context in 0..7 {
        hash = hash32(&words(symbols.attr_masks[context]), hash);
    }
    hash = hash32(&words(symbols.attr_masks[7]), hash);
    // The high halves of the same context-7 masks: a second pass over the same
    // count, which has to happen even when no mask reaches past bit 30.
    let high: Vec<u32> = symbols.attr_masks_high.iter().map(|&v| v as u32).collect();
    hash = hash32(&high, hash);
    if auxiliary_pair {
        let aux: Vec<u32> = symbols.attr_masks_aux.iter().map(|&v| v as u32).collect();
        hash = hash32(&aux, hash);
    }
    hash = hash32(symbols.attr_masks_large, hash);
    hash = hash32(&words(symbols.split_faces), hash);
    hash32(&words(symbols.split_positions), hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word hash reproduces the coordinate check values of the samples.
    #[test]
    fn the_word_hash_reproduces_a_coordinate_check_value() {
        let codes = [
            vec![511, 511, 511, 0, 0, 0, 0, 511],
            vec![0, 511, 0, 0, 511, 0, 511, 511],
            vec![0, 0, 511, 0, 0, 511, 511, 511],
        ];
        let mut hash = 0;
        for component in &codes {
            hash = hash32(component, hash);
        }
        assert_eq!(hash, 0xaa_da_64_c9);
    }

    /// The symbol streams of the first shape of `example_block_jt9.5.jt`, as
    /// the reader decodes them, hash to the value the file stores.
    #[test]
    fn the_composite_hash_reproduces_a_sample_topology() {
        let degrees: [Vec<i32>; 8] = [
            vec![5, 4, 5, 4, 5, 3],
            vec![6, 4],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let masks: [Vec<i32>; 8] = [
            Vec::new(),
            vec![7],
            vec![7, 13, 7],
            vec![22, 26, 21],
            vec![21],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let symbols = TopologySymbols {
            degrees: slices(&degrees),
            valences: &[3; 12],
            groups: &[0, 4, 1, 1, 4, 3, 5, 3, 2, 5, 2, 0],
            face_flags: &[0; 12],
            attr_masks: slices(&masks),
            attr_masks_high: &[],
            attr_masks_aux: &[],
            attr_masks_large: &[],
            split_faces: &[],
            split_positions: &[],
        };
        assert_eq!(composite_hash(&symbols, true), 0x1d_e7_96_a1);
    }

    /// The same topology written at JT 10.x drops one hash call and lands on
    /// the value the JT 10.3 sample of the same block stores.
    #[test]
    fn a_ten_x_topology_hashes_one_call_shorter() {
        let degrees: [Vec<i32>; 8] = [
            vec![5, 4, 5, 4, 5, 3],
            vec![6, 4],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let masks: [Vec<i32>; 8] = [
            Vec::new(),
            vec![7],
            vec![7, 13, 7],
            vec![22, 26, 21],
            vec![21],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        let symbols = TopologySymbols {
            degrees: slices(&degrees),
            valences: &[3; 12],
            groups: &[0, 4, 1, 1, 4, 3, 5, 3, 2, 5, 2, 0],
            face_flags: &[0; 12],
            attr_masks: slices(&masks),
            attr_masks_high: &[],
            attr_masks_aux: &[],
            attr_masks_large: &[],
            split_faces: &[],
            split_positions: &[],
        };
        assert_eq!(composite_hash(&symbols, false), 0x3f_37_a2_e7);
    }
}
