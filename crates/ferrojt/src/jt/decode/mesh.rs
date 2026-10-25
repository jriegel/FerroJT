//! Topologically compressed mesh decoding (JT "dual VF mesh" coder).
//!
//! The compressed representation stores a polygon mesh as a stream of symbols
//! describing its *dual* graph: for every vertex a valence, a group and a flag
//! word, and for every face a degree plus a ring of attribute bits. The
//! decoder walks connected components of the dual graph, creating one vertex
//! and one face at a time and hooking them into a [`DualVFMesh`] until every
//! slot is filled. The finished dual mesh is then flattened back into the
//! vertex and normal index arrays a renderer expects.
//!
//! The symbol streams are integer codec packages in the *mark 2* framing, read
//! by [`MeshInput::load`] in the fixed order the format defines.

use std::collections::HashMap;

use super::cdp::Package;
use super::unpack::Unpack;
use crate::codec::ByteOrder;
use crate::error::{Error, Result};
use crate::jt::reader::Cursor;

/// Number of faces inspected when choosing the next active face.
const ACTIVE_SCAN_WIDTH: i64 = 16;

/// Face degrees up to this size keep their attribute ring in a 64-bit mask.
const INLINE_MASK_BITS: i32 = 64;

/// Largest valence or degree a single element may declare.
///
/// Far above any real mesh, and keeps a corrupt stream from asking for a
/// multi-gigabyte slot array before the decoder can notice the damage.
const MAX_RING: i32 = 1 << 20;

/// A bit vector addressed by bit number, backed by 32-bit words.
fn bit_test(words: &[u32], bit: i32) -> bool {
    if bit < 0 {
        return false;
    }
    let word = (bit >> 5) as usize;
    word < words.len() && (words[word] & (1 << (bit & 31))) != 0
}

/// Sets one bit of a [`bit_test`] backed vector, growing it as needed.
fn bit_set(words: &mut Vec<u32>, bit: i32) {
    if bit < 0 {
        return;
    }
    let word = (bit >> 5) as usize;
    while words.len() <= word {
        words.push(0);
    }
    words[word] |= 1 << (bit & 31);
}

/// Number of bits covered by `bits` words of attribute mask.
fn mask_word_count(bits: i32) -> usize {
    usize::try_from(bits).unwrap_or(0) / 32 + 1
}

/// Reads index `index` out of `values`, yielding `-1` when out of range.
///
/// The reference implementation indexes its slot arrays directly; clamping
/// keeps a malformed stream from aborting the process.
fn slot_at(values: &[i32], index: i64) -> i32 {
    values
        .get(usize::try_from(index).unwrap_or(usize::MAX))
        .copied()
        .unwrap_or(-1)
}

/// `IncModN` of the reference: `value + 1`, wrapping to zero at `len - 1`.
fn inc_mod_n(value: i32, len: i32) -> i32 {
    if value == len - 1 { 0 } else { value + 1 }
}

/// `DecModN` of the reference: `value - 1`, wrapping to `len - 1` at zero.
fn dec_mod_n(value: i32, len: i32) -> i32 {
    if value == 0 { len - 1 } else { value - 1 }
}

/// One vertex of a dual mesh: its valence plus the slot range holding the
/// incident faces.
#[derive(Clone, Copy, Debug)]
struct VertexEntry {
    valence: i32,
    /// Decoded with the valence, but no flag of it is acted upon.
    #[allow(dead_code)]
    flags: i32,
    group: i32,
    face_base: i32,
}

/// One face of a dual mesh: its degree plus the slot ranges holding the
/// incident vertices and the face attributes.
#[derive(Clone, Debug)]
struct FaceEntry {
    degree: i32,
    empty_degree: i32,
    attr_count: i32,
    /// Decoded with the degree, but no flag of it is acted upon.
    #[allow(dead_code)]
    flags: i32,
    attr_mask: u64,
    attr_mask_bits: Vec<u32>,
    vertex_base: i32,
    attr_base: i32,
}

/// A dual vertex-facet mesh: a closed two-manifold polygon mesh stored so that
/// every vertex knows its incident faces and every face its incident vertices.
#[derive(Clone, Debug, Default)]
struct DualVFMesh {
    vertices: Vec<VertexEntry>,
    faces: Vec<FaceEntry>,
    vertex_face_slots: Vec<i32>,
    face_vertex_slots: Vec<i32>,
    face_attr_slots: Vec<i32>,
}

impl DualVFMesh {
    /// Number of vertices created so far.
    fn vertex_count(&self) -> i32 {
        self.vertices.len() as i32
    }

    /// Number of faces created so far.
    fn face_count(&self) -> i32 {
        self.faces.len() as i32
    }

    /// Valence of `vertex`, or `0` when it does not exist.
    fn valence(&self, vertex: i32) -> i32 {
        self.vertices
            .get(usize::try_from(vertex).unwrap_or(usize::MAX))
            .map_or(0, |entry| entry.valence)
    }

    /// Degree of `face`, or `0` when it does not exist.
    fn degree(&self, face: i32) -> i32 {
        self.faces
            .get(usize::try_from(face).unwrap_or(usize::MAX))
            .map_or(0, |entry| entry.degree)
    }

    /// True when `face` exists and has a non-zero degree.
    fn is_valid_face(&self, face: i32) -> bool {
        self.faces
            .get(usize::try_from(face).unwrap_or(usize::MAX))
            .is_some_and(|entry| entry.degree != 0)
    }

    /// Face stored in slot `slot` of `vertex`, or `-1` for an empty slot.
    fn face(&self, vertex: i32, slot: i32) -> i32 {
        let base = match self
            .vertices
            .get(usize::try_from(vertex).unwrap_or(usize::MAX))
        {
            Some(entry) => entry.face_base,
            None => return -1,
        };
        slot_at(&self.vertex_face_slots, i64::from(base) + i64::from(slot))
    }

    /// Vertex stored in slot `slot` of `face`, or `-1` for an empty slot.
    fn vertex(&self, face: i32, slot: i32) -> i32 {
        let base = match self.faces.get(usize::try_from(face).unwrap_or(usize::MAX)) {
            Some(entry) => entry.vertex_base,
            None => return -1,
        };
        slot_at(&self.face_vertex_slots, i64::from(base) + i64::from(slot))
    }

    /// Number of still unfilled vertex slots of `face`.
    fn empty_face_slots(&self, face: i32) -> i32 {
        self.faces
            .get(usize::try_from(face).unwrap_or(usize::MAX))
            .map_or(0, |entry| entry.empty_degree)
    }

    /// Creates vertex `vertex`.
    ///
    /// A valence of zero leaves the vertex without any face slots, exactly as
    /// in the reference.
    fn new_vertex(&mut self, vertex: i32, valence: i32, flags: i32) {
        debug_assert_eq!(vertex, self.vertex_count());
        let face_base = if valence == 0 {
            -1
        } else {
            let base = self.vertex_face_slots.len() as i32;
            self.vertex_face_slots.resize(
                usize::try_from(i64::from(base) + i64::from(valence)).unwrap_or(0),
                -1,
            );
            base
        };
        self.vertices.push(VertexEntry {
            valence,
            flags,
            group: -1,
            face_base,
        });
    }

    /// Records the group id of `vertex`.
    fn set_vertex_group(&mut self, vertex: i32, group: i32) {
        if let Some(entry) = self
            .vertices
            .get_mut(usize::try_from(vertex).unwrap_or(usize::MAX))
        {
            entry.group = group;
        }
    }

    /// Group id of `vertex`, or `-1` when it does not exist.
    fn vertex_group(&self, vertex: i32) -> i32 {
        self.vertices
            .get(usize::try_from(vertex).unwrap_or(usize::MAX))
            .map_or(-1, |entry| entry.group)
    }

    /// Creates face `face` with its attribute ring either inline or in words.
    fn new_face(
        &mut self,
        face: i32,
        degree: i32,
        attr_count: i32,
        attr_mask: u64,
        attr_mask_bits: Vec<u32>,
    ) {
        debug_assert_eq!(face, self.face_count());
        let vertex_base = self.face_vertex_slots.len() as i32;
        let attr_base = if attr_count > 0 {
            let base = self.face_attr_slots.len() as i32;
            self.face_attr_slots.resize(
                usize::try_from(i64::from(base) + i64::from(attr_count)).unwrap_or(0),
                -1,
            );
            base
        } else {
            -1
        };
        self.face_vertex_slots.resize(
            usize::try_from(i64::from(vertex_base) + i64::from(degree.max(0))).unwrap_or(0),
            -1,
        );
        self.faces.push(FaceEntry {
            degree: degree as u16 as i32,
            empty_degree: degree as u16 as i32,
            attr_count: attr_count as u16 as i32,
            flags: 0,
            attr_mask,
            attr_mask_bits,
            vertex_base,
            attr_base,
        });
    }

    /// Stores `attr` in attribute slot `slot` of `face`.
    fn set_face_attr(&mut self, face: i32, slot: i32, attr: i32) -> Result<()> {
        let base = self
            .faces
            .get(usize::try_from(face).unwrap_or(usize::MAX))
            .map_or(-1, |entry| entry.attr_base);
        let index = i64::from(base) + i64::from(slot);
        if index < 0 || index as usize >= self.face_attr_slots.len() {
            return Err(Error::corrupt(format!(
                "face attribute slot {slot} of face {face} is out of range"
            )));
        }
        self.face_attr_slots[index as usize] = attr;
        Ok(())
    }

    /// Attaches `face` to slot `slot` of `vertex`.
    fn set_vertex_face(&mut self, vertex: i32, slot: i32, face: i32) -> Result<()> {
        let base = self
            .vertices
            .get(usize::try_from(vertex).unwrap_or(usize::MAX))
            .map_or(-1, |entry| entry.face_base);
        let index = i64::from(base) + i64::from(slot);
        if index < 0 || index as usize >= self.vertex_face_slots.len() {
            return Err(Error::corrupt(format!(
                "face slot {slot} of vertex {vertex} is out of range"
            )));
        }
        self.vertex_face_slots[index as usize] = face;
        Ok(())
    }

    /// Attaches `vertex` to slot `slot` of `face`, keeping the free slot count
    /// up to date.
    fn set_face_vertex(&mut self, face: i32, slot: i32, vertex: i32) -> Result<()> {
        let index = {
            let Some(entry) = self.faces.get(usize::try_from(face).unwrap_or(usize::MAX)) else {
                return Err(Error::corrupt(format!("face {face} does not exist")));
            };
            i64::from(entry.vertex_base) + i64::from(slot)
        };
        if index < 0 || index as usize >= self.face_vertex_slots.len() {
            return Err(Error::corrupt(format!(
                "vertex slot {slot} of face {face} is out of range"
            )));
        }
        if self.face_vertex_slots[index as usize] != vertex {
            if let Some(entry) = self
                .faces
                .get_mut(usize::try_from(face).unwrap_or(usize::MAX))
            {
                entry.empty_degree -= 1;
            }
            self.face_vertex_slots[index as usize] = vertex;
        }
        Ok(())
    }

    /// Slot of `target` in the vertex ring of `face`, or `-1`.
    fn find_vertex_slot(&self, face: i32, target: i32) -> i32 {
        let Some(entry) = self.faces.get(usize::try_from(face).unwrap_or(usize::MAX)) else {
            return -1;
        };
        for slot in 0..entry.degree {
            if self.vertex(face, slot) == target {
                return slot;
            }
        }
        -1
    }

    /// Slot of `target` in the face ring of `vertex`, or `-1`.
    fn find_face_slot(&self, vertex: i32, target: i32) -> i32 {
        let Some(entry) = self
            .vertices
            .get(usize::try_from(vertex).unwrap_or(usize::MAX))
        else {
            return -1;
        };
        for slot in 0..entry.valence {
            if self.face(vertex, slot) == target {
                return slot;
            }
        }
        -1
    }

    /// Attribute id of the face attribute bound to the pair (`vertex`, `face`).
    fn vertex_face_attr(&self, vertex: i32, face: i32) -> i32 {
        let Some(entry) = self.faces.get(usize::try_from(face).unwrap_or(usize::MAX)) else {
            return -1;
        };
        if entry.attr_count <= 0 {
            return -1;
        }
        let mut attr_slot = entry.attr_count - 1;
        for vertex_slot in 0..entry.degree {
            let set = if entry.degree <= INLINE_MASK_BITS {
                (entry.attr_mask >> (vertex_slot as u32 & 63)) & 1 != 0
            } else {
                bit_test(&entry.attr_mask_bits, vertex_slot)
            };
            if set {
                attr_slot += 1;
            }
            if attr_slot >= entry.attr_count {
                attr_slot -= entry.attr_count;
            }
            if self.vertex(face, vertex_slot) == vertex {
                return slot_at(
                    &self.face_attr_slots,
                    i64::from(entry.attr_base) + i64::from(attr_slot),
                );
            }
        }
        -1
    }
}

/// The decoded symbol streams of one mesh, with the read cursors that walk them.
#[derive(Clone, Debug, Default)]
struct Symbols {
    degrees: Vec<Vec<i32>>,
    valences: Vec<i32>,
    groups: Vec<i32>,
    face_flags: Vec<i32>,
    attr_masks: Vec<Vec<i32>>,
    mask8_30: Vec<i32>,
    mask8_4: Vec<i32>,
    attr_masks_large: Vec<u32>,
    split_faces: Vec<i32>,
    split_positions: Vec<i32>,
    degree_pos: Vec<usize>,
    attr_mask_pos: Vec<usize>,
    valence_pos: usize,
    group_pos: usize,
    face_flag_pos: usize,
    attr_mask_large_pos: usize,
    split_face_pos: usize,
    split_position_pos: usize,
}

impl Symbols {
    /// Next face degree symbol of context `context`.
    ///
    /// Like the reference, a cursor that reaches the end of its stream stops
    /// there: it keeps handing out its default value forever instead of running
    /// past the end. The completeness check in [`MeshInput::decode`] relies on
    /// this, so a stream that is never fully consumed is detected.
    fn next_degree(&mut self, context: usize) -> i32 {
        let position = &mut self.degree_pos[context];
        let Some(&value) = self.degrees[context].get(*position) else {
            return -1;
        };
        *position += 1;
        value
    }

    /// Next vertex valence symbol.
    fn next_valence(&mut self) -> i32 {
        let Some(&value) = self.valences.get(self.valence_pos) else {
            return -1;
        };
        self.valence_pos += 1;
        value
    }

    /// Next vertex group symbol.
    fn next_group(&mut self) -> i32 {
        let Some(&value) = self.groups.get(self.group_pos) else {
            return -1;
        };
        self.group_pos += 1;
        value
    }

    /// Next vertex flag symbol; zero once the stream is exhausted.
    fn next_face_flag(&mut self) -> i32 {
        let Some(&value) = self.face_flags.get(self.face_flag_pos) else {
            return 0;
        };
        self.face_flag_pos += 1;
        value
    }

    /// Next face attribute mask of context `context`.
    ///
    /// Context 7 also folds in the two auxiliary mask arrays. Both are shifted
    /// by 30 bits, which is what the reference does; the second shift is a bug
    /// in that implementation and is reproduced here so the output matches.
    fn next_attr_mask(&mut self, context: usize) -> i64 {
        let position = self.attr_mask_pos[context];
        let mut value = self.attr_masks[context]
            .get(position)
            .copied()
            .map_or(0, i64::from);
        if context == 7 {
            let mut high = 0i64;
            if let Some(value) = self.mask8_4.get(position) {
                high += i64::from(*value) << 30;
            }
            if let Some(value) = self.mask8_30.get(position) {
                high += i64::from(*value) << 30;
            }
            value |= high;
        }
        self.attr_mask_pos[context] += 1;
        value
    }

    /// Next wide attribute mask, covering `degree` bits.
    fn next_attr_mask_bits(&mut self, degree: i32) -> Vec<u32> {
        let mut words = vec![0u32; mask_word_count(degree)];
        if self.attr_mask_large_pos >= self.attr_masks_large.len() {
            return words;
        }
        let needed = usize::try_from(degree).unwrap_or(0).div_ceil(32);
        let available = self.attr_masks_large.len() - self.attr_mask_large_pos;
        let count = needed.min(available);
        words[..count].copy_from_slice(
            &self.attr_masks_large[self.attr_mask_large_pos..self.attr_mask_large_pos + count],
        );
        self.attr_mask_large_pos += needed;
        words
    }

    /// Next split face symbol.
    fn next_split_face(&mut self) -> i32 {
        let value = self
            .split_faces
            .get(self.split_face_pos)
            .copied()
            .unwrap_or(-1);
        self.split_face_pos += 1;
        value
    }

    /// Next split position symbol.
    fn next_split_position(&mut self) -> i32 {
        let value = self
            .split_positions
            .get(self.split_position_pos)
            .copied()
            .unwrap_or(-1);
        self.split_position_pos += 1;
        value
    }

    /// True when every stream was consumed to its end by the decoder.
    fn is_exhausted(&self) -> bool {
        self.degree_pos
            .iter()
            .zip(self.degrees.iter())
            .all(|(position, stream)| *position == stream.len())
            && self
                .attr_mask_pos
                .iter()
                .zip(self.attr_masks.iter())
                .all(|(position, stream)| *position == stream.len())
            && self.valence_pos == self.valences.len()
            && self.group_pos == self.groups.len()
            && self.face_flag_pos == self.face_flags.len()
            && self.attr_mask_large_pos == self.attr_masks_large.len()
            && self.split_face_pos == self.split_faces.len()
            && self.split_position_pos == self.split_positions.len()
    }
}

/// The result of flattening a decoded dual mesh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Topology {
    vertex_indices: Vec<i32>,
    normal_indices: Vec<i32>,
}

impl Topology {
    /// Index of the unique vertex record for each output corner.
    #[must_use]
    pub fn vertex_indices(&self) -> &[i32] {
        &self.vertex_indices
    }

    /// Index of the unique normal record for each output corner, `-1` when the
    /// corner has no normal.
    #[must_use]
    pub fn normal_indices(&self) -> &[i32] {
        &self.normal_indices
    }

    /// Number of output corners.
    #[must_use]
    pub fn len(&self) -> usize {
        self.vertex_indices.len()
    }

    /// True when the mesh has no corners at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vertex_indices.is_empty()
    }
}

/// Encoded mesh symbols as stored in a topologically compressed shape LOD.
#[derive(Clone, Debug)]
pub struct MeshInput {
    degrees: [Package; 8],
    valences: Package,
    groups: Package,
    face_flags: Package,
    attr_masks: [Package; 8],
    mask8_30: Package,
    mask8_4: Package,
    attr_masks_large: Package,
    split_faces: Package,
    split_positions: Package,
}

impl MeshInput {
    /// Reads the mesh symbol packages from `cursor`.
    ///
    /// JT 10.x stores a single wide attribute mask array where earlier versions
    /// stored a pair, so the second of those is skipped there.
    pub fn load(cursor: &mut Cursor<'_>) -> Result<Self> {
        let mut degrees = [const { Package::empty() }; 8];
        for package in &mut degrees {
            *package = Package::load2(cursor)?;
        }
        let valences = Package::load2(cursor)?;
        let groups = Package::load2(cursor)?;
        let face_flags = Package::load2(cursor)?;
        let mut attr_masks = [const { Package::empty() }; 8];
        for package in &mut attr_masks {
            *package = Package::load2(cursor)?;
        }
        let mask8_30 = Package::load2(cursor)?;
        let mask8_4 = if cursor.major() >= 10 {
            Package::empty()
        } else {
            Package::load2(cursor)?
        };
        let attr_masks_large = Package::load2(cursor)?;
        let split_faces = Package::load2(cursor)?;
        let split_positions = Package::load2(cursor)?;
        // A trailing checksum over the symbols is not verified by the reader.
        let _hash = cursor.read_u32()?;
        Ok(Self {
            degrees,
            valences,
            groups,
            face_flags,
            attr_masks,
            mask8_30,
            mask8_4,
            attr_masks_large,
            split_faces,
            split_positions,
        })
    }

    /// Decodes the symbol streams and rebuilds the mesh.
    pub fn decode(mut self, order: ByteOrder) -> Result<Topology> {
        let mut degrees = Vec::with_capacity(8);
        for package in &mut self.degrees {
            degrees.push(package.decode(order, Unpack::Null)?);
        }
        let mut attr_masks = Vec::with_capacity(8);
        for package in &mut self.attr_masks {
            attr_masks.push(package.decode(order, Unpack::Null)?);
        }
        let symbols = Symbols {
            degree_pos: vec![0; degrees.len()],
            attr_mask_pos: vec![0; attr_masks.len()],
            degrees,
            attr_masks,
            valences: self.valences.decode(order, Unpack::Null)?,
            groups: self.groups.decode(order, Unpack::Null)?,
            face_flags: self.face_flags.decode(order, Unpack::Lag1)?,
            mask8_30: self.mask8_30.decode(order, Unpack::Null)?,
            mask8_4: self.mask8_4.decode(order, Unpack::Null)?,
            attr_masks_large: self
                .attr_masks_large
                .decode(order, Unpack::Null)?
                .into_iter()
                .map(|value| value as u32)
                .collect(),
            split_faces: self.split_faces.decode(order, Unpack::Lag1)?,
            split_positions: self.split_positions.decode(order, Unpack::Null)?,
            ..Symbols::default()
        };
        let mut decoder = Decoder {
            symbols,
            mesh: DualVFMesh::default(),
            active_faces: Vec::new(),
            removed: Vec::new(),
            attr_counter: 0,
        };
        decoder.run()?;
        if !decoder.symbols.is_exhausted() {
            // The reference drops the mesh when symbols are left over; doing
            // the same keeps the output identical.
            tracing::warn!("mesh symbol streams are not fully consumed");
            return Ok(Topology::default());
        }
        Ok(decode_dual_mesh(&decoder.mesh))
    }
}

/// Rebuilds one connected component of the dual mesh at a time.
struct Decoder {
    symbols: Symbols,
    mesh: DualVFMesh,
    active_faces: Vec<i32>,
    removed: Vec<u32>,
    attr_counter: i32,
}

impl Decoder {
    /// Runs the decoder over every connected component of the mesh.
    fn run(&mut self) -> Result<()> {
        self.mesh = DualVFMesh::default();
        self.active_faces.clear();
        self.removed.clear();
        self.attr_counter = 0;
        while self.run_component()? {}
        Ok(())
    }

    /// Decodes one connected component, false when the streams are done.
    fn run_component(&mut self) -> Result<bool> {
        let Some(vertex) = self.read_vertex()? else {
            return Ok(false);
        };
        let valence = self.mesh.valence(vertex);
        for slot in 0..valence {
            self.activate_face(vertex, slot)?;
        }
        while let Some(face) = self.next_active_face() {
            self.complete_vertex(face)?;
            bit_set(&mut self.removed, face);
        }
        Ok(true)
    }

    /// Reads a valence, group and flag symbol to create the next vertex.
    fn read_vertex(&mut self) -> Result<Option<i32>> {
        let valence = self.symbols.next_valence();
        if valence <= -1 {
            return Ok(None);
        }
        if valence > MAX_RING {
            return Err(Error::corrupt(format!(
                "vertex valence symbol {valence} is out of range"
            )));
        }
        let vertex = self.mesh.vertex_count();
        let group = self.symbols.next_group();
        let flags = self.symbols.next_face_flag();
        self.mesh.new_vertex(vertex, valence, flags);
        self.mesh.set_vertex_group(vertex, group);
        Ok(Some(vertex))
    }

    /// Compression context of the faces incident on `vertex`.
    fn face_context(&self, vertex: i32) -> usize {
        let valence = self.mesh.valence(vertex);
        let mut known_faces = 0i32;
        let mut known_degree = 0i32;
        for slot in 0..valence {
            let face = self.mesh.face(vertex, slot);
            if !self.mesh.is_valid_face(face) {
                continue;
            }
            known_faces += 1;
            known_degree += self.mesh.degree(face);
        }
        let context = match valence {
            3 => {
                let target = known_faces * 6;
                if known_degree < target {
                    0
                } else if known_degree == target {
                    1
                } else {
                    2
                }
            }
            4 => {
                let target = known_faces * 4;
                if known_degree < target {
                    3
                } else if known_degree == target {
                    4
                } else {
                    5
                }
            }
            5 => 6,
            _ => 7,
        };
        context as usize
    }

    /// Reads a degree and attribute mask symbol to create the next face.
    ///
    /// `-1` means the slot refers to an already active face (a split), `-2`
    /// means the stream ended.
    fn read_face(&mut self, vertex: i32) -> Result<i32> {
        let context = self.face_context(vertex);
        let symbol = self.symbols.next_degree(context);
        if symbol == 0 {
            return Ok(-1);
        }
        if symbol < 0 {
            return Ok(-2);
        }
        let degree = symbol;
        if degree > MAX_RING {
            return Err(Error::corrupt(format!(
                "face degree symbol {degree} is out of range"
            )));
        }
        let face = self.mesh.face_count();
        let attr_context = (degree - 2).clamp(0, 7) as usize;
        if degree <= INLINE_MASK_BITS {
            let mask = self.symbols.next_attr_mask(attr_context) as u64;
            let attr_count = mask.count_ones() as i32;
            self.mesh
                .new_face(face, degree, attr_count, mask, Vec::new());
            if attr_count > degree {
                return Ok(-1);
            }
        } else {
            let bits = self.symbols.next_attr_mask_bits(degree);
            let attr_count = (0..degree).filter(|slot| bit_test(&bits, *slot)).count() as i32;
            self.mesh.new_face(face, degree, attr_count, 0, bits);
            if attr_count > degree {
                return Ok(-1);
            }
        }
        for slot in 0..self.mesh.faces[face as usize].attr_count {
            let attr = self.attr_counter;
            self.attr_counter += 1;
            self.mesh.set_face_attr(face, slot, attr)?;
        }
        Ok(face)
    }

    /// Resolves a split symbol into the face it points back at.
    fn read_split_face(&mut self) -> i32 {
        let symbol = self.symbols.next_split_face();
        if symbol <= -1 {
            return -1;
        }
        let offset = symbol as usize;
        if offset == 0 || offset > self.active_faces.len() {
            return -1;
        }
        self.active_faces[self.active_faces.len() - offset]
    }

    /// Reads the vertex slot a split connects into.
    fn read_split_position(&mut self) -> i32 {
        let symbol = self.symbols.next_split_position();
        if symbol <= -1 { -1 } else { symbol }
    }

    /// Creates the face hanging off `vertex` at face slot `slot`.
    fn activate_face(&mut self, vertex: i32, slot: i32) -> Result<i32> {
        let face = self.read_face(vertex)?;
        if face >= 0 {
            self.mesh.set_vertex_face(vertex, slot, face)?;
            self.mesh.set_face_vertex(face, 0, vertex)?;
            self.active_faces.push(face);
        } else if face == -1 {
            let split = self.read_split_face();
            let position = self.read_split_position();
            if split < 0 || position < 0 {
                return Err(Error::corrupt(format!(
                    "topological split of vertex {vertex} points at face {split} slot {position}"
                )));
            }
            self.mesh.set_vertex_face(vertex, slot, split)?;
            self.add_vertex_to_face(vertex, slot, split, position)?;
        }
        Ok(face)
    }

    /// Creates the vertex hanging off `face` at vertex slot `slot`.
    fn activate_vertex(&mut self, face: i32, slot: i32) -> Result<i32> {
        let Some(vertex) = self.read_vertex()? else {
            return Err(Error::corrupt("vertex symbol stream ended inside a face"));
        };
        self.mesh.set_vertex_face(vertex, 0, face)?;
        self.add_vertex_to_face(vertex, 0, face, slot)?;
        Ok(vertex)
    }

    /// Links `vertex` into `face` at `slot` and across the two shared edges.
    fn add_vertex_to_face(
        &mut self,
        vertex: i32,
        face_slot: i32,
        face: i32,
        vertex_slot: i32,
    ) -> Result<()> {
        let degree = self.mesh.degree(face);
        let slot_ccw = inc_mod_n(vertex_slot, degree);
        let slot_cw = dec_mod_n(vertex_slot, degree);
        self.mesh.set_face_vertex(face, vertex_slot, vertex)?;

        let previous = self.mesh.vertex(face, slot_cw);
        if previous != -1 {
            let base = self.mesh.find_face_slot(previous, face);
            let target = inc_mod_n(face_slot, self.mesh.valence(vertex));
            if self.mesh.face(vertex, target) == -1 {
                let source = dec_mod_n(base, self.mesh.valence(previous));
                let face_at_source = self.mesh.face(previous, source);
                self.mesh.set_vertex_face(vertex, target, face_at_source)?;
            }
        }

        let next = self.mesh.vertex(face, slot_ccw);
        if next != -1 {
            let base = self.mesh.find_face_slot(next, face);
            let target = dec_mod_n(face_slot, self.mesh.valence(vertex));
            if self.mesh.face(vertex, target) == -1 {
                let source = inc_mod_n(base, self.mesh.valence(next));
                let face_at_source = self.mesh.face(next, source);
                self.mesh.set_vertex_face(vertex, target, face_at_source)?;
            }
        }
        Ok(())
    }

    /// Fills the empty vertex slots of `face` with new vertices.
    fn complete_vertex(&mut self, face: i32) -> Result<()> {
        loop {
            let slot = self.mesh.find_vertex_slot(face, -1);
            if slot == -1 {
                return Ok(());
            }
            let vertex = self.activate_vertex(face, slot)?;
            if self.mesh.vertex(face, slot) != vertex || self.mesh.face(vertex, 0) != face {
                return Ok(());
            }
            self.complete_face(vertex, slot)?;
        }
    }

    /// Links or creates the remaining faces around `vertex`.
    fn complete_face(&mut self, vertex: i32, vertex_slot: i32) -> Result<()> {
        let valence = self.mesh.valence(vertex);

        // Counter-clockwise from face slot 0, linking faces that the already
        // decoded topology implies.
        let mut face_previous = self.mesh.face(vertex, 0);
        let mut slot_previous = vertex_slot;
        let mut index = 1;
        loop {
            let face_next = self.mesh.face(vertex, index);
            if face_next == -1 {
                break;
            }
            slot_previous = dec_mod_n(slot_previous, self.mesh.degree(face_previous));
            let neighbour = self.mesh.vertex(face_previous, slot_previous);
            if neighbour == -1 {
                break;
            }
            let mut slot_next = self.mesh.find_vertex_slot(face_next, neighbour);
            if slot_next <= -1 {
                return Ok(());
            }
            slot_next = dec_mod_n(slot_next, self.mesh.degree(face_next));
            self.add_vertex_to_face(vertex, index, face_next, slot_next)?;
            face_previous = face_next;
            slot_previous = slot_next;
            index += 1;
            if index >= valence {
                return Ok(());
            }
        }

        // Clockwise from face slot 0 for the same reason.
        let mut last = index;
        let mut face_previous = self.mesh.face(vertex, 0);
        let mut slot_previous = vertex_slot;
        let mut index = self.mesh.valence(vertex) - 1;
        loop {
            let face_next = self.mesh.face(vertex, index);
            if face_next == -1 {
                break;
            }
            slot_previous = inc_mod_n(slot_previous, self.mesh.degree(face_previous));
            let neighbour = self.mesh.vertex(face_previous, slot_previous);
            if neighbour == -1 {
                break;
            }
            let mut slot_next = self.mesh.find_vertex_slot(face_next, neighbour);
            if slot_next <= -1 {
                return Ok(());
            }
            slot_next = inc_mod_n(slot_next, self.mesh.degree(face_next));
            self.add_vertex_to_face(vertex, index, face_next, slot_next)?;
            face_previous = face_next;
            slot_previous = slot_next;
            index -= 1;
            if index < last {
                return Ok(());
            }
        }

        // Whatever is left has to be read from the symbol stream.
        while last <= index {
            let face = self.activate_face(vertex, last)?;
            if face < -1 {
                return Ok(());
            }
            last += 1;
        }
        Ok(())
    }

    /// Picks the active face with the fewest free slots in the recent window.
    fn next_active_face(&mut self) -> Option<i32> {
        while let Some(face) = self.active_faces.last().copied() {
            if bit_test(&self.removed, face) {
                self.active_faces.pop();
            } else {
                break;
            }
        }
        let mut best = -1;
        let mut lowest = i32::MAX;
        let mut index = self.active_faces.len() as i64 - 1;
        while index >= (self.active_faces.len() as i64 - ACTIVE_SCAN_WIDTH).max(0) {
            let face = self.active_faces[index as usize];
            if bit_test(&self.removed, face) {
                self.active_faces.remove(index as usize);
                index -= 1;
                continue;
            }
            let empty = self.mesh.empty_face_slots(face);
            if empty < lowest {
                lowest = empty;
                best = face;
            }
            index -= 1;
        }
        (best != -1).then_some(best)
    }
}

/// Flattens the dual mesh into per-corner vertex and normal indices.
fn decode_dual_mesh(mesh: &DualVFMesh) -> Topology {
    let vertex_count = mesh.vertex_count();
    let mut starts = vec![-1i32; usize::try_from(vertex_count).unwrap_or(0)];
    let mut total = 0i32;
    for vertex in 0..vertex_count {
        if mesh.vertex_group(vertex) >= 0 {
            starts[vertex as usize] = total;
            total += mesh.valence(vertex);
        }
    }
    let size = usize::try_from(total).unwrap_or(0);
    let mut vertex_indices = vec![0i32; size];
    let mut normal_indices = vec![0i32; size];
    for vertex in 0..vertex_count {
        let start = starts[vertex as usize];
        if start < 0 {
            continue;
        }
        let valence = mesh.valence(vertex);
        for slot in 0..valence {
            let index = usize::try_from(i64::from(start) + i64::from(slot)).unwrap_or(0);
            let face = mesh.face(vertex, slot);
            if let Some(target) = vertex_indices.get_mut(index) {
                *target = face;
            }
            if let Some(target) = normal_indices.get_mut(index) {
                *target = mesh.vertex_face_attr(vertex, face);
            }
        }
    }
    Topology {
        vertex_indices,
        normal_indices,
    }
}

/// Unique (vertex, normal) pairs of a topologically decoded shape LOD.
///
/// When a shape binds both coordinates and normals the two index streams are
/// unified: each pair of indices seen in the mesh gets one output record, in
/// first-seen order.
#[derive(Clone, Debug, Default)]
pub struct UnifiedMesh {
    indices: Vec<i32>,
    pair_source: Vec<i32>,
    pair_count: usize,
}

impl UnifiedMesh {
    /// Builds the unified index list from the two mesh index streams.
    #[must_use]
    pub fn new(topology: &Topology, unique_vertices: usize) -> Self {
        let mut result = Self {
            indices: vec![0; topology.vertex_indices().len()],
            pair_source: vec![0; topology.vertex_indices().len()],
            pair_count: 0,
        };
        let mut pairs: Vec<HashMap<i32, i32>> = vec![HashMap::new(); unique_vertices];
        for (position, (vertex, normal)) in topology
            .vertex_indices()
            .iter()
            .zip(topology.normal_indices())
            .enumerate()
        {
            let Some(map) = pairs.get_mut(usize::try_from(*vertex).unwrap_or(usize::MAX)) else {
                continue;
            };
            if let Some(id) = map.get(normal) {
                result.indices[position] = *id;
                continue;
            }
            let id = result.pair_count as i32;
            map.insert(*normal, id);
            result.pair_source[result.pair_count] = position as i32;
            result.indices[position] = id;
            result.pair_count += 1;
        }
        result.pair_source.truncate(result.pair_count);
        result
    }

    /// Index of the unique pair for each mesh corner.
    #[must_use]
    pub fn indices(&self) -> &[i32] {
        &self.indices
    }

    /// For each output record, the corner it was built from.
    #[must_use]
    pub fn sources(&self) -> &[i32] {
        &self.pair_source
    }

    /// Number of unique (vertex, normal) pairs.
    #[must_use]
    pub fn pair_count(&self) -> usize {
        self.pair_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a mark 2 null package holding `words`.
    fn package2(words: &[u32], out: &mut Vec<u8>) {
        out.extend_from_slice(&(words.len() as i32).to_le_bytes());
        if words.is_empty() {
            // A zero value count is the whole package: the codec byte is never
            // reached.
            return;
        }
        out.push(0);
        out.extend_from_slice(&((words.len() as u32 * 32) as i32).to_le_bytes());
        for word in words {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }

    /// Appends `values` as a mark 2 null package.
    fn values(values: &[i32], out: &mut Vec<u8>) {
        let words: Vec<u32> = values.iter().map(|value| *value as u32).collect();
        package2(&words, out);
    }

    /// Builds a file image holding an empty mesh: every stream is empty.
    fn empty_mesh(major: i32) -> Vec<u8> {
        let mut data = Vec::new();
        for _ in 0..23 {
            values(&[], &mut data);
        }
        if major < 10 {
            values(&[], &mut data);
        }
        data.extend_from_slice(&0u32.to_le_bytes()); // hash
        data
    }

    #[test]
    fn empty_streams_decode_to_nothing() {
        let data = empty_mesh(9);
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 9);
        let input = MeshInput::load(&mut cursor).expect("load");
        assert_eq!(cursor.remaining(), 0, "the hash is consumed too");
        let topology = input.decode(ByteOrder::Little).expect("decode");
        assert!(topology.is_empty());
    }

    #[test]
    fn ten_x_skips_the_second_mask_array() {
        let data = empty_mesh(10);
        let mut cursor = Cursor::new(&data, ByteOrder::Little, 10);
        let input = MeshInput::load(&mut cursor).expect("load");
        assert!(input.mask8_4.is_empty());
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn context_seven_merges_both_mask_arrays() {
        let mut symbols = Symbols {
            degrees: vec![Vec::new(); 8],
            attr_masks: vec![
                vec![0x0000_0001],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![0x0000_0002],
            ],
            mask8_30: vec![0x0000_0003],
            mask8_4: vec![0x0000_0004],
            degree_pos: vec![0; 8],
            attr_mask_pos: vec![0; 8],
            ..Symbols::default()
        };
        // The low word comes from the context array, the high bits from both
        // auxiliary arrays shifted by 30 - the reference behaviour.
        let mask = symbols.next_attr_mask(7);
        let expected = 2i64 | (i64::from(4) << 30) | (i64::from(3) << 30);
        assert_eq!(mask, expected);
        assert_eq!(symbols.attr_mask_pos[7], 1);
        assert_eq!(symbols.next_attr_mask(7), 0);
    }

    #[test]
    fn negative_mask_words_sign_extend() {
        let mut symbols = Symbols {
            degrees: vec![Vec::new(); 8],
            attr_masks: vec![
                vec![-1; 8],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
            ],
            degree_pos: vec![0; 8],
            attr_mask_pos: vec![0; 8],
            ..Symbols::default()
        };
        // The symbol is an i32 -1 that the reference widens to int64_t and then
        // hands to a uint64_t mask, so every bit ends up set.
        assert_eq!(symbols.next_attr_mask(0).count_ones(), 64);
    }

    #[test]
    fn wide_masks_are_read_word_by_word() {
        let mut symbols = Symbols {
            attr_masks_large: vec![1, 2, 4, 8],
            ..Symbols::default()
        };
        let bits = symbols.next_attr_mask_bits(65);
        assert_eq!(bits, vec![1, 2, 4]);
        assert_eq!(symbols.attr_mask_large_pos, 3);
        // The reference copies whole words without clamping to the end of the
        // stream, so a short tail reads past it; the zeros stand in for that.
        let bits = symbols.next_attr_mask_bits(96);
        assert_eq!(bits, vec![8, 0, 0, 0]);
    }

    #[test]
    fn face_context_follows_valence_and_known_degree() {
        let mut mesh = DualVFMesh::default();
        mesh.new_vertex(0, 4, 0);
        let mut decoder = Decoder {
            symbols: Symbols::default(),
            mesh,
            active_faces: Vec::new(),
            removed: Vec::new(),
            attr_counter: 0,
        };
        // Nothing is known about the vertex yet, and a total known degree of 0
        // equals 4 * 0 known faces, which selects the exact context.
        assert_eq!(decoder.face_context(0), 4);
        decoder.mesh.new_face(0, 4, 0, 0, Vec::new());
        decoder.mesh.set_vertex_face(0, 0, 0).expect("slot");
        assert_eq!(decoder.face_context(0), 4);
        decoder.mesh.new_face(1, 3, 0, 0, Vec::new());
        decoder.mesh.set_vertex_face(0, 1, 1).expect("slot");
        assert_eq!(decoder.face_context(0), 3);
        decoder.mesh.new_vertex(1, 5, 0);
        assert_eq!(decoder.face_context(1), 6);
        decoder.mesh.new_vertex(2, 7, 0);
        assert_eq!(decoder.face_context(2), 7);
    }

    #[test]
    fn ring_arithmetic_wraps_at_both_ends() {
        assert_eq!(inc_mod_n(2, 3), 0);
        assert_eq!(inc_mod_n(1, 3), 2);
        assert_eq!(dec_mod_n(0, 3), 2);
        assert_eq!(dec_mod_n(1, 3), 0);
    }

    #[test]
    fn attribute_lookup_walks_the_degree_ring() {
        let mut mesh = DualVFMesh::default();
        mesh.new_vertex(0, 1, 0);
        mesh.new_vertex(1, 1, 0);
        // A triangle with attributes on vertex slots 0 and 2.
        mesh.new_face(0, 3, 2, 0b101, Vec::new());
        mesh.set_face_attr(0, 0, 7).expect("attr");
        mesh.set_face_attr(0, 1, 8).expect("attr");
        mesh.set_face_vertex(0, 0, 0).expect("slot");
        mesh.set_face_vertex(0, 2, 1).expect("slot");
        assert_eq!(mesh.vertex_face_attr(0, 0), 7);
        assert_eq!(mesh.vertex_face_attr(1, 0), 8);
        assert_eq!(mesh.vertex_face_attr(5, 0), -1);
        assert_eq!(mesh.vertex_face_attr(0, -1), -1);
    }

    #[test]
    fn unified_pairs_are_numbered_in_first_seen_order() {
        let topology = Topology {
            vertex_indices: vec![0, 0, 1, 0],
            normal_indices: vec![0, 1, 0, 0],
        };
        let unified = UnifiedMesh::new(&topology, 2);
        assert_eq!(unified.indices(), &[0, 1, 2, 0]);
        assert_eq!(unified.pair_count(), 3);
        assert_eq!(unified.sources(), &[0, 1, 2]);
    }
}
