//! Topological (dual vertex-face) mesh encoding for JT 9.x and 10.x.
//!
//! JT 9.x and 10.x carry a shape's triangles in one representation: the
//! *topologically compressed* tri-strip set decoded by
//! [`MeshInput`](crate::jt::decode::MeshInput). That coder stores the **dual**
//! of the input triangle mesh — a coder *face* is an input vertex (its degree
//! is that vertex's valence) and a coder *vertex* is an input triangle (its
//! valence is three). The reference sources contain no encoder, so this module
//! supplies one.
//!
//! # How it works
//!
//! The decoder is a deterministic walk over the dual graph that consults a
//! symbol stream at a handful of decision points. This encoder runs that same
//! walk against a mirror of the decoder's mesh state and, at each decision
//! point, emits the symbol that makes the decoder build the target mesh.
//! Because the walk is the decoder's own, the streams it produces are consumed
//! down to the last symbol and so pass the reader's exhaustiveness check —
//! which matters, because a stream that is merely *close* decodes to a silently
//! empty topology.
//!
//! The decoder numbers its slots from whatever it meets first, so the encoder
//! records two alignments: `off_f[face]` is the decoder slot holding the first
//! triangle of a coder face's ring, and `off_v[vertex]` the slot holding the
//! first corner of a coder vertex's triangle. Every target slot is then that
//! offset plus the ring position, taken modulo the degree or valence.
//!
//! # Feasibility
//!
//! The dual of a triangle mesh is a closed two-manifold only when every input
//! vertex has at least three incident triangles and every edge exactly two. A
//! triangle soup or an open shell therefore cannot be encoded, and [`encode`]
//! rejects those with [`Error::invalid`].

use std::collections::HashMap;

use crate::codec::{ByteOrder, WriterExt};
use crate::error::{Error, Result};
use crate::jt::build::MeshData;
use crate::jt::hash::{TopologySymbols, composite_hash, hash32, slices};
use crate::jt::version::JtVersion;
use crate::jt::write::write_version;

/// The object type id of the logical element a JT 10.x tri-strip set embeds
/// around its coder input.
///
/// JT 10.x wraps the topological data in a **second, nested element header** —
/// length, object type id, base type and object id — sitting immediately after
/// the first vertex binding. A strict consumer walks through it to reach the
/// coder input, so a file that stores the same bytes without that framing
/// parses here and renders nothing elsewhere. Neither published format
/// description names this id and no OpenJT source contains it, so it is taken
/// verbatim from the sample files.
const TOPOLOGY_ELEMENT_GUID: [u8; 16] = [
    0xad, 0xa5, 0x30, 0xf8, 0x4c, 0xbe, 0xbc, 0x4f, 0x9b, 0x5f, 0xb9, 0x26, 0x92, 0x78, 0xd2, 0xe1,
];

/// The base type of that embedded element: 9 is "no base class".
const TOPOLOGY_ELEMENT_BASE_TYPE: u8 = 9;

/// The object id of the embedded element. Every sample stores 1, and nothing
/// resolves it against the property table.
const TOPOLOGY_ELEMENT_OBJECT_ID: u32 = 1;

/// The `TopoMesh LOD Data` of Figure 88 that follows the embedded element's
/// envelope: a version, the object id of the vertex records this top mesh
/// shares with other top meshes, and a flag. Every sample stores 0 for the
/// vertex records of a single-mesh file, and 0 renders identically.
const TOPOLOGY_LOD_VERSION: u8 = 1;

/// The object id of the vertex records of the embedded element.
const TOPOLOGY_VERTEX_RECORDS_ID: u32 = 0;

/// The flag closing the `TopoMesh LOD Data`.
const TOPOLOGY_LOD_FLAG: u8 = 1;

/// The width of the embedded element header written before the coder input of
/// a JT 10.x tri-strip set: its length, object type id, base type, object id,
/// and the three members of the `TopoMesh LOD Data`.
const TOPOLOGY_EMBEDDED_HEADER: usize = 4 + 16 + 1 + 4 + 1 + 4 + 1;

/// The low byte of a 64-bit vertex binding: three position components, one
/// normal component, no colour.
const BINDING_LSB: u8 = 0x4a;

/// The symbol streams of a dual walk, as *intended* values.
///
/// The reader applies a residual predictor to two of these streams, so
/// [`write_mesh_input`] stores face flags and split faces as residuals and the
/// rest verbatim.
#[derive(Clone, Debug, Default)]
struct Streams {
    degrees: [Vec<i32>; 8],
    valences: Vec<i32>,
    groups: Vec<i32>,
    face_flags: Vec<i32>,
    attr_masks: [Vec<i32>; 8],
    /// The auxiliary high-bit stream of context 7: one value per mask symbol
    /// written into [`Streams::attr_masks`]`[7]`.
    ///
    /// The reader folds the first of these into a context-7 mask as its 32 MSBs
    /// and reads the second for framing only, so a mask that needs no bit above
    /// 31 still needs one value (zero) per mask symbol in each
    /// array. A consumer that renders a shape from its topological
    /// representation refuses a *context-7 face whose auxiliary streams are
    /// empty*, which is why the count must track the mask count rather than the
    /// mask value.
    attr_masks_high: Vec<i32>,
    attr_masks_large: Vec<u32>,
    split_faces: Vec<i32>,
    split_positions: Vec<i32>,
}

/// A fully encoded representation, ready to be framed into an element body.
#[derive(Clone, Debug)]
pub(crate) struct Encoded {
    streams: Streams,
    /// One position per coder face, in coder-face order — what the reader
    /// indexes with `vertex_indices`.
    positions: Vec<[f32; 3]>,
    /// One normal per face attribute, in attribute-id order — what the reader
    /// indexes with `normal_indices`.
    normals: Vec<[f32; 3]>,
}

/// Encode `mesh` for a JT 9.x or 10.x topologically compressed representation.
///
/// # Errors
///
/// [`Error::invalid`] is returned when the mesh is not a closed, consistently
/// oriented two-manifold: the dual coder cannot represent an open shell, a
/// triangle whose corners weld together, or an edge shared by other than
/// exactly two triangles.
pub(crate) fn encode(mesh: &MeshData) -> Result<Encoded> {
    let target = Target::build(mesh)?;
    if target.triangles.is_empty() {
        return Err(Error::invalid(
            0,
            "an empty mesh cannot be written as JT 9.x/10.x",
        ));
    }
    let mut walker = Walk::new(&target);
    walker.run()?;
    if walker.vertices.len() != target.triangles.len()
        || walker.faces.len() != target.positions.len()
    {
        return Err(Error::invalid(
            0,
            "the mesh could not be walked as a closed dual mesh",
        ));
    }
    Ok(walker.finish())
}

/// The input mesh as the coder sees it: welded positions, triangles over them,
/// and the counter-clockwise ring of triangles around every vertex.
struct Target {
    /// Unique positions, one per coder face.
    positions: Vec<[f32; 3]>,
    /// The three welded corners of every input triangle, in input winding.
    triangles: Vec<[i32; 3]>,
    /// The counter-clockwise ring of triangle ids around each welded vertex.
    ring: Vec<Vec<i32>>,
    /// The input normal of each (triangle, corner) pair, indexed
    /// `3 * triangle + corner`.
    corner_normals: Vec<[f32; 3]>,
}

impl Target {
    /// Weld the input positions and derive the dual adjacency.
    fn build(mesh: &MeshData) -> Result<Self> {
        let vertices = mesh.vertices();
        let indices = mesh.indices();
        let normals = mesh.normals();
        if indices.len() % 3 != 0 || vertices.len() % 3 != 0 {
            return Err(Error::invalid(0, "a mesh to encode is not a triangle list"));
        }
        let slot_count = vertices.len() / 3;

        // Weld positions to unique vertices, remembering each input slot.
        let mut weld_of_slot: HashMap<[u32; 3], i32> = HashMap::new();
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut welded_slot = vec![-1i32; slot_count];
        for slot in 0..slot_count {
            let point = [
                vertices[3 * slot],
                vertices[3 * slot + 1],
                vertices[3 * slot + 2],
            ];
            let key = [point[0].to_bits(), point[1].to_bits(), point[2].to_bits()];
            let id = match weld_of_slot.get(&key) {
                Some(&id) => id,
                None => {
                    let id = positions.len() as i32;
                    positions.push(point);
                    weld_of_slot.insert(key, id);
                    id
                }
            };
            welded_slot[slot] = id;
        }

        // Triangles over the welded vertices, keeping each corner's normal.
        let triangles = indices.len() / 3;
        let mut tri_welded: Vec<[i32; 3]> = Vec::with_capacity(triangles);
        let mut corner_normals: Vec<[f32; 3]> = Vec::with_capacity(indices.len());
        for tri in 0..triangles {
            let mut welded = [-1i32; 3];
            for corner in 0..3 {
                let slot = indices[3 * tri + corner];
                let at = usize::try_from(slot)
                    .map_err(|_| Error::invalid(0, "a mesh to encode has a negative index"))?;
                if at >= slot_count {
                    return Err(Error::invalid(
                        0,
                        "a mesh to encode has an index out of range",
                    ));
                }
                welded[corner] = welded_slot[at];
                corner_normals.push([normals[at * 3], normals[at * 3 + 1], normals[at * 3 + 2]]);
            }
            if welded[0] == welded[1] || welded[1] == welded[2] || welded[0] == welded[2] {
                return Err(Error::invalid(
                    0,
                    "a mesh to encode has a triangle whose corners weld to one vertex",
                ));
            }
            tri_welded.push(welded);
        }

        // Every edge must be shared by exactly two triangles, and every vertex
        // must carry at least three of them, or the dual is not a closed
        // two-manifold the coder can walk.
        let mut edges: HashMap<(i32, i32), i32> = HashMap::new();
        for welded in &tri_welded {
            for corner in 0..3 {
                let a = welded[corner];
                let b = welded[(corner + 1) % 3];
                let key = if a < b { (a, b) } else { (b, a) };
                *edges.entry(key).or_insert(0) += 1;
            }
        }
        if edges.values().any(|&count| count != 2) {
            return Err(Error::invalid(
                0,
                "the mesh is not a closed two-manifold, so it cannot be written as JT 9.x/10.x",
            ));
        }

        // The counter-clockwise ring of triangles around each welded vertex.
        // The order comes from walking the fan across its shared edges rather
        // than sorting the triangles by their angle about the vertex: a screw
        // thread projects onto its own fan, so an angular order can name
        // triangles that do not share an edge, which the dual walk cannot
        // follow.
        let mut ring: Vec<Vec<i32>> = vec![Vec::new(); positions.len()];
        for (tri, welded) in tri_welded.iter().enumerate() {
            for corner in 0..3 {
                ring[welded[corner] as usize].push(tri as i32);
            }
        }
        let mut across: HashMap<(i32, i32), Vec<i32>> = HashMap::new();
        for (tri, welded) in tri_welded.iter().enumerate() {
            for corner in 0..3 {
                let a = welded[corner];
                let b = welded[(corner + 1) % 3];
                let key = if a < b { (a, b) } else { (b, a) };
                across.entry(key).or_default().push(tri as i32);
            }
        }
        for (vertex, fan) in ring.iter_mut().enumerate() {
            if fan.len() < 3 {
                return Err(Error::invalid(
                    0,
                    "a vertex of the mesh has fewer than three faces, so it cannot be written \
                     as JT 9.x/10.x",
                ));
            }
            let welded = vertex as i32;
            let start = fan[0];
            let mut order: Vec<i32> = Vec::with_capacity(fan.len());
            let mut current = start;
            while !order.contains(&current) {
                order.push(current);
                let corners = tri_welded[current as usize];
                let at = (0..3)
                    .find(|&corner| corners[corner] == welded)
                    .expect("a triangle of the fan holds the vertex of the fan");
                // The winding is counter-clockwise about the outward normal, so
                // the next triangle of the fan shares the edge arriving at the
                // vertex: the one leaving the corner before it.
                let other = corners[(at + 2) % 3];
                let key = if welded < other {
                    (welded, other)
                } else {
                    (other, welded)
                };
                let Some(pair) = across.get(&key).filter(|pair| pair.len() == 2) else {
                    return Err(Error::invalid(
                        0,
                        "an edge of the mesh is not shared by exactly two triangles, so it \
                         cannot be written as JT 9.x/10.x",
                    ));
                };
                current = if pair[0] == current { pair[1] } else { pair[0] };
            }
            if order.len() != fan.len() {
                return Err(Error::invalid(
                    0,
                    "the triangles around a vertex of the mesh do not form a single ring, so \
                     it cannot be written as JT 9.x/10.x",
                ));
            }
            *fan = order;
        }

        Ok(Self {
            positions,
            triangles: tri_welded,
            ring,
            corner_normals,
        })
    }

    /// Position of triangle `tri` in the ring of welded vertex `welded`.
    fn ring_index(&self, welded: i32, tri: i32) -> i32 {
        self.ring[welded as usize]
            .iter()
            .position(|&t| t == tri)
            .map_or(-1, |at| at as i32)
    }

    /// Which corner of `tri` holds welded vertex `welded`.
    fn corner_index(&self, welded: i32, tri: i32) -> i32 {
        (0..3)
            .find(|&corner| self.triangles[tri as usize][corner] == welded)
            .map_or(-1, |corner| corner as i32)
    }
}

/// `IncModN`: `value + 1`, wrapping to zero at `len - 1`.
fn inc_mod_n(value: i32, len: i32) -> i32 {
    if value == len - 1 { 0 } else { value + 1 }
}

/// `DecModN`: `value - 1`, wrapping to `len - 1` at zero.
fn dec_mod_n(value: i32, len: i32) -> i32 {
    if value == 0 { len - 1 } else { value - 1 }
}

/// Which of the eight attribute-mask streams a face of `degree` is read from.
///
/// Unlike the degree stream, which is chosen from the *face context* of the
/// vertex being completed, the mask stream is chosen from the degree the reader
/// has just decoded — `(degree - 2)` clamped into the array.
fn mask_context(degree: i32) -> usize {
    (degree - 2).clamp(0, 7) as usize
}

/// One vertex of the mirror mesh: its valence and its incident-face slots.
#[derive(Clone, Debug)]
struct MirrorVertex {
    valence: i32,
    faces: Vec<i32>,
}

/// One face of the mirror mesh: its degree, its incident-vertex slots and the
/// attribute ids bound to them.
#[derive(Clone, Debug)]
struct MirrorFace {
    degree: i32,
    empty: i32,
    attrs: Vec<i32>,
    vertices: Vec<i32>,
}

/// The encoder's walk: a mirror of the decoder's mesh state that chooses a
/// symbol at each decision point instead of reading one.
struct Walk<'a> {
    target: &'a Target,
    vertices: Vec<MirrorVertex>,
    faces: Vec<MirrorFace>,
    active: Vec<i32>,
    removed: Vec<u32>,
    attr_counter: i32,
    streams: Streams,
    /// Coder vertex id to input triangle, and the reverse.
    tri_of_cv: Vec<i32>,
    cv_of_tri: HashMap<i32, i32>,
    /// Coder face id to welded vertex, and the reverse.
    weld_of_cf: Vec<i32>,
    cf_of_weld: HashMap<i32, i32>,
    /// The ring position sitting at decoder slot zero of each element.
    off_v: Vec<i32>,
    off_f: Vec<i32>,
}

impl<'a> Walk<'a> {
    fn new(target: &'a Target) -> Self {
        Self {
            target,
            vertices: Vec::new(),
            faces: Vec::new(),
            active: Vec::new(),
            removed: Vec::new(),
            attr_counter: 0,
            streams: Streams::default(),
            tri_of_cv: Vec::new(),
            cv_of_tri: HashMap::new(),
            weld_of_cf: Vec::new(),
            cf_of_weld: HashMap::new(),
            off_v: Vec::new(),
            off_f: Vec::new(),
        }
    }

    /// Runs the walk over every connected component of the dual.
    fn run(&mut self) -> Result<()> {
        while self.run_component()? {}
        Ok(())
    }

    /// Encodes one connected component; false once every triangle is placed.
    fn run_component(&mut self) -> Result<bool> {
        let Some(tri) = self.unplaced_triangle() else {
            return Ok(false);
        };
        let vertex = self.read_vertex(tri)?;
        let valence = self.vertices[vertex as usize].valence;
        for slot in 0..valence {
            self.activate_face(vertex, slot)?;
        }
        while let Some(face) = self.next_active_face() {
            self.complete_vertex(face)?;
            self.bit_set(face);
        }
        Ok(true)
    }

    /// The first input triangle not yet bound to a coder vertex.
    fn unplaced_triangle(&self) -> Option<i32> {
        (0..self.target.triangles.len() as i32).find(|tri| !self.cv_of_tri.contains_key(tri))
    }

    /// The welded vertex coder vertex `cv` needs at face slot `slot`.
    fn wanted_welded(&self, cv: i32, slot: i32) -> i32 {
        let tri = self.tri_of_cv[cv as usize];
        let ring = (self.off_v[cv as usize] + slot).rem_euclid(3) as usize;
        self.target.triangles[tri as usize][ring]
    }

    /// Creates the coder vertex for input triangle `tri`.
    ///
    /// Every coder vertex is a triangle, so its valence is three, its group is
    /// zero and its flag stays zero — which is what the reference tooling
    /// writes and what the reader's exhausted defaults assume.
    fn read_vertex(&mut self, tri: i32) -> Result<i32> {
        if self.cv_of_tri.contains_key(&tri) {
            return Err(Error::invalid(
                0,
                "internal: the dual walk placed a triangle twice",
            ));
        }
        let vertex = self.vertices.len() as i32;
        self.streams.valences.push(3);
        self.streams.groups.push(0);
        self.streams.face_flags.push(0);
        self.vertices.push(MirrorVertex {
            valence: 3,
            faces: vec![-1; 3],
        });
        self.tri_of_cv.push(tri);
        self.cv_of_tri.insert(tri, vertex);
        self.off_v.push(0);
        Ok(vertex)
    }

    /// The compression context of the faces already incident on `cv`.
    fn face_context(&self, cv: i32) -> usize {
        let v = &self.vertices[cv as usize];
        let mut known_faces = 0i32;
        let mut known_degree = 0i32;
        for &face in &v.faces {
            if face < 0 || face as usize >= self.faces.len() {
                continue;
            }
            known_faces += 1;
            known_degree += self.faces[face as usize].degree;
        }
        let context = match v.valence {
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

    /// Emits the all-set attribute mask of a face of `degree`.
    ///
    /// The reader takes its mask from the inline stream up to a degree of 64,
    /// and one inline symbol is a 32-bit word widened to 64 bits with its sign.
    /// A word without the sign bit reaches at most 31 set bits, and a word with
    /// it sets the whole upper half as well, so only degrees up to 31 can be
    /// described that way. A degree above 64 goes through the wide bit array,
    /// which the reader consults for exactly that case. A degree between the
    /// two has no encoding at all: any mask the reader could build there would
    /// count more attributes than the face has slots, which the reader treats
    /// as a split rather than a face.
    fn emit_attr_mask(&mut self, context: usize, degree: i32) -> Result<()> {
        if degree > 64 {
            for _ in 0..(degree as usize).div_ceil(32) {
                self.streams.attr_masks_large.push(u32::MAX);
            }
            return Ok(());
        }
        if degree > 31 {
            return Err(Error::invalid(
                0,
                format!(
                    "a vertex of the mesh has {degree} faces, which the JT 9.x/10.x attribute \
                     mask cannot describe"
                ),
            ));
        }
        self.streams.attr_masks[context].push((1 << degree) - 1);
        // A context-7 mask is widened with two auxiliary streams, and the reader
        // indexes them with the *same* read position as the mask itself. Every
        // mask symbol therefore needs a value in each, even when — as here, with
        // a degree of at most 31 — no bit above 29 is ever set. A consumer that
        // renders from the topological representation rejects a context-7 face
        // whose auxiliary streams are empty.
        if context == 7 {
            self.streams.attr_masks_high.push(0);
        }
        Ok(())
    }

    /// Creates or splits the coder face that coder vertex `cv` wants at slot
    /// `slot`.
    ///
    /// A face that already exists is reached through a split symbol, which
    /// points back a measured number of entries in the active list and names
    /// the slot the vertex belongs at.
    fn read_face(&mut self, cv: i32, slot: i32) -> Result<FaceChoice> {
        let welded = self.wanted_welded(cv, slot);
        let tri = self.tri_of_cv[cv as usize];
        if let Some(&face) = self.cf_of_weld.get(&welded) {
            self.streams.degrees[self.face_context(cv)].push(0);
            let index = self
                .active
                .iter()
                .rposition(|&f| f == face)
                .ok_or_else(|| Error::invalid(0, "internal: a split points at an inactive face"))?;
            self.streams
                .split_faces
                .push((self.active.len() - index) as i32);
            let position = self.slot_of_face(face, tri);
            self.streams.split_positions.push(position);
            return Ok(FaceChoice::Split { face, position });
        }

        let degree = self.target.ring[welded as usize].len() as i32;
        self.streams.degrees[self.face_context(cv)].push(degree);
        // The mask streams are *not* indexed by the face context: the reader
        // picks the array from the degree it has just read, so the two streams
        // walk different arrays and must be fed separately.
        self.emit_attr_mask(mask_context(degree), degree)?;
        let face = self.faces.len() as i32;
        let mut attrs = Vec::with_capacity(degree as usize);
        for _ in 0..degree {
            attrs.push(self.attr_counter);
            self.attr_counter += 1;
        }
        self.faces.push(MirrorFace {
            degree,
            empty: degree,
            attrs,
            vertices: vec![-1; degree as usize],
        });
        self.weld_of_cf.push(welded);
        self.cf_of_weld.insert(welded, face);
        self.off_f.push(self.target.ring_index(welded, tri));
        Ok(FaceChoice::New(face))
    }

    /// The decoder slot of `face` holding input triangle `tri`.
    fn slot_of_face(&self, face: i32, tri: i32) -> i32 {
        let welded = self.weld_of_cf[face as usize];
        let index = self.target.ring_index(welded, tri);
        (self.off_f[face as usize] + index).rem_euclid(self.faces[face as usize].degree)
    }

    /// Links `cv` into `face` at `vertex_slot` and across the two shared edges.
    fn add_vertex_to_face(&mut self, cv: i32, face_slot: i32, face: i32, vertex_slot: i32) {
        let degree = self.faces[face as usize].degree;
        let slot_ccw = inc_mod_n(vertex_slot, degree);
        let slot_cw = dec_mod_n(vertex_slot, degree);
        self.set_face_vertex(face, vertex_slot, cv);

        let previous = self.faces[face as usize].vertices[slot_cw as usize];
        if previous != -1 {
            let base = self.find_face_slot(previous, face);
            let target = inc_mod_n(face_slot, self.vertices[cv as usize].valence);
            if self.vertices[cv as usize].faces[target as usize] == -1 {
                let source = dec_mod_n(base, self.vertices[previous as usize].valence);
                let face_at_source = self.vertices[previous as usize].faces[source as usize];
                self.vertices[cv as usize].faces[target as usize] = face_at_source;
            }
        }

        let next = self.faces[face as usize].vertices[slot_ccw as usize];
        if next != -1 {
            let base = self.find_face_slot(next, face);
            let target = dec_mod_n(face_slot, self.vertices[cv as usize].valence);
            if self.vertices[cv as usize].faces[target as usize] == -1 {
                let source = inc_mod_n(base, self.vertices[next as usize].valence);
                let face_at_source = self.vertices[next as usize].faces[source as usize];
                self.vertices[cv as usize].faces[target as usize] = face_at_source;
            }
        }
    }

    /// Writes `cv` into `face` slot `slot`, keeping the free count current.
    fn set_face_vertex(&mut self, face: i32, slot: i32, cv: i32) {
        let face = &mut self.faces[face as usize];
        if face.vertices[slot as usize] != cv {
            face.empty -= 1;
            face.vertices[slot as usize] = cv;
        }
    }

    /// Slot of `target` in the face ring of `vertex`, or `-1`.
    fn find_face_slot(&self, vertex: i32, target: i32) -> i32 {
        self.vertices[vertex as usize]
            .faces
            .iter()
            .position(|&f| f == target)
            .map_or(-1, |at| at as i32)
    }

    /// Slot of `target` in the vertex ring of `face`, or `-1`.
    fn find_vertex_slot(&self, face: i32, target: i32) -> i32 {
        self.faces[face as usize]
            .vertices
            .iter()
            .position(|&v| v == target)
            .map_or(-1, |at| at as i32)
    }

    /// Creates the face hanging off `cv` at face slot `slot`.
    fn activate_face(&mut self, cv: i32, slot: i32) -> Result<()> {
        match self.read_face(cv, slot)? {
            FaceChoice::New(face) => {
                self.vertices[cv as usize].faces[slot as usize] = face;
                self.set_face_vertex(face, 0, cv);
                self.active.push(face);
            }
            FaceChoice::Split { face, position } => {
                self.vertices[cv as usize].faces[slot as usize] = face;
                self.add_vertex_to_face(cv, slot, face, position);
            }
        }
        Ok(())
    }

    /// Creates the vertex hanging off `face` at vertex slot `slot`.
    fn activate_vertex(&mut self, face: i32, slot: i32) -> Result<i32> {
        let welded = self.weld_of_cf[face as usize];
        let ring = &self.target.ring[welded as usize];
        let tri = ring[(self.off_f[face as usize] + slot).rem_euclid(ring.len() as i32) as usize];
        let cv = self.read_vertex(tri)?;
        self.off_v[cv as usize] = self.target.corner_index(welded, tri);
        self.vertices[cv as usize].faces[0] = face;
        self.add_vertex_to_face(cv, 0, face, slot);
        Ok(cv)
    }

    /// Fills the empty vertex slots of `face` with new vertices.
    fn complete_vertex(&mut self, face: i32) -> Result<()> {
        loop {
            let slot = self.find_vertex_slot(face, -1);
            if slot == -1 {
                return Ok(());
            }
            let cv = self.activate_vertex(face, slot)?;
            if self.faces[face as usize].vertices[slot as usize] != cv
                || self.vertices[cv as usize].faces[0] != face
            {
                return Ok(());
            }
            self.complete_face(cv, slot)?;
        }
    }

    /// Links or creates the remaining faces around `cv`.
    fn complete_face(&mut self, cv: i32, vertex_slot: i32) -> Result<()> {
        let valence = self.vertices[cv as usize].valence;

        // Counter-clockwise from face slot 0, linking faces the topology
        // already decoded implies.
        let mut face_previous = self.vertices[cv as usize].faces[0];
        let mut slot_previous = vertex_slot;
        let mut index = 1;
        loop {
            let face_next = self.vertices[cv as usize].faces[index as usize];
            if face_next == -1 {
                break;
            }
            slot_previous = dec_mod_n(slot_previous, self.faces[face_previous as usize].degree);
            let neighbour = self.faces[face_previous as usize].vertices[slot_previous as usize];
            if neighbour == -1 {
                break;
            }
            let mut slot_next = self.find_vertex_slot(face_next, neighbour);
            if slot_next <= -1 {
                return Ok(());
            }
            slot_next = dec_mod_n(slot_next, self.faces[face_next as usize].degree);
            self.add_vertex_to_face(cv, index, face_next, slot_next);
            face_previous = face_next;
            slot_previous = slot_next;
            index += 1;
            if index >= valence {
                return Ok(());
            }
        }

        // Clockwise from face slot 0 for the same reason.
        let mut last = index;
        let mut face_previous = self.vertices[cv as usize].faces[0];
        let mut slot_previous = vertex_slot;
        let mut index = valence - 1;
        loop {
            let face_next = self.vertices[cv as usize].faces[index as usize];
            if face_next == -1 {
                break;
            }
            slot_previous = inc_mod_n(slot_previous, self.faces[face_previous as usize].degree);
            let neighbour = self.faces[face_previous as usize].vertices[slot_previous as usize];
            if neighbour == -1 {
                break;
            }
            let mut slot_next = self.find_vertex_slot(face_next, neighbour);
            if slot_next <= -1 {
                return Ok(());
            }
            slot_next = inc_mod_n(slot_next, self.faces[face_next as usize].degree);
            self.add_vertex_to_face(cv, index, face_next, slot_next);
            face_previous = face_next;
            slot_previous = slot_next;
            index -= 1;
            if index < last {
                return Ok(());
            }
        }

        // Whatever is left has to come from the symbol stream.
        while last <= index {
            self.activate_face(cv, last)?;
            last += 1;
        }
        Ok(())
    }

    /// Picks the active face with the fewest free slots in the recent window.
    fn next_active_face(&mut self) -> Option<i32> {
        while let Some(&face) = self.active.last() {
            if self.bit_test(face) {
                self.active.pop();
            } else {
                break;
            }
        }
        let scan_width = 16i64;
        let mut best = -1;
        let mut lowest = i32::MAX;
        let mut index = self.active.len() as i64 - 1;
        while index >= (self.active.len() as i64 - scan_width).max(0) {
            let face = self.active[index as usize];
            if self.bit_test(face) {
                self.active.remove(index as usize);
                index -= 1;
                continue;
            }
            let empty = self.faces[face as usize].empty;
            if empty < lowest {
                lowest = empty;
                best = face;
            }
            index -= 1;
        }
        (best != -1).then_some(best)
    }

    fn bit_test(&self, bit: i32) -> bool {
        let word = (bit >> 5) as usize;
        word < self.removed.len() && (self.removed[word] & (1 << (bit & 31))) != 0
    }

    fn bit_set(&mut self, bit: i32) {
        let word = (bit >> 5) as usize;
        while self.removed.len() <= word {
            self.removed.push(0);
        }
        self.removed[word] |= 1 << (bit & 31);
    }

    /// Turns the finished mirror into the coordinate and normal payloads.
    ///
    /// The reader takes a corner's position from its coder face and its normal
    /// from the attribute bound to that corner's slot, so the payloads are laid
    /// out in exactly those orders.
    fn finish(&self) -> Encoded {
        let positions: Vec<[f32; 3]> = self
            .weld_of_cf
            .iter()
            .map(|&welded| self.target.positions[welded as usize])
            .collect();
        let mut normals = vec![[0.0f32; 3]; self.attr_counter as usize];
        for (face_id, face) in self.faces.iter().enumerate() {
            let welded = self.weld_of_cf[face_id];
            for (slot, &attr) in face.attrs.iter().enumerate() {
                let cv = face.vertices[slot];
                if cv < 0 {
                    continue;
                }
                let tri = self.tri_of_cv[cv as usize];
                let corner = self.target.corner_index(welded, tri);
                if corner < 0 {
                    continue;
                }
                normals[attr as usize] =
                    self.target.corner_normals[3 * tri as usize + corner as usize];
            }
        }
        Encoded {
            streams: self.streams.clone(),
            positions,
            normals,
        }
    }
}

/// What the decoder was told to do at a face decision point.
#[derive(Clone, Copy, Debug)]
enum FaceChoice {
    /// A brand new coder face.
    New(i32),
    /// An existing face, reached through the split symbols.
    Split { face: i32, position: i32 },
}

/// Appends one mark-2 package holding `values`, coded with bitlength2.
///
/// An empty package is only its zero value count, because the codec byte is
/// never reached — `loadCDP2` returns a null package for a zero count without
/// reading further.
fn package2(values: &[i32], version: JtVersion, out: &mut Vec<u8>) {
    write_u32(out, values.len() as u32);
    if values.is_empty() {
        return;
    }
    // Codec 1, `JtDecode_Int32CDP_Bitlength2`. The null codec (0) is accepted by
    // the OpenJT reference reader but produces no geometry in a consumer that
    // renders a shape from its topological representation: no file written by
    // real CAD tooling stores a non-empty package any other way than with
    // bitlength2 or one of the two entropy codecs.
    write_u8(out, 1);
    let min = values.iter().copied().min().unwrap_or(0);
    let max = values.iter().copied().max().unwrap_or(0);
    let span = i64::from(max) - i64::from(min);
    let width = u32::try_from(span).map_or(32, |span| u32::BITS - span.leading_zeros());
    let mut bits = BitWriter::default();
    if version.major >= 10 {
        // `decodeV10`, fixed range: the minimum and maximum as nibbler values,
        // then one unsigned field of the width of the span per value.
        bits.bit(false);
        bits.nibbler(min);
        bits.nibbler(max);
        for &value in values {
            bits.u((i64::from(value) - i64::from(min)) as u32, width);
        }
    } else if span > 0 && span < (1 << 31) {
        // `decodeFixedWidth`: a six-bit bit count for the minimum, a six-bit bit
        // count for the maximum, both values in those widths, and then one
        // unsigned field of the width of the span per value.
        bits.bit(false);
        bits.u(signed_bits(min), 6);
        bits.u(signed_bits(max), 6);
        bits.i(min, signed_bits(min));
        bits.i(max, signed_bits(max));
        for &value in values {
            bits.u((i64::from(value) - i64::from(min)) as u32, width);
        }
    } else if span == 0 {
        // The same fixed-range framing with no fields at all: the reader computes
        // a range of zero and fills the package with the minimum it just read.
        bits.bit(false);
        bits.u(signed_bits(min), 6);
        bits.u(signed_bits(max), 6);
        bits.i(min, signed_bits(min));
        bits.i(max, signed_bits(max));
    } else {
        // The range of a JT 9.x fixed-range package is read as a **signed** int,
        // so a span of 2^31 or more reads as negative and the reader fills the
        // whole package with the minimum. Such a package goes down the variable
        // branch, which names its field width per run instead.
        bits.bit(true);
        write_variable_bits(values, min, width + 1, &mut bits);
    }
    bits.finish(out);
}

/// Appends the variable-range code text of the JT 9.x bitlength2 codec.
///
/// One mean, then per run a change to the field width and the number of values
/// it covers. A change field of `change_bits` bits continues the run when it
/// holds the extreme of its range, so the width is approached in steps of three
/// and closed by a change of at most two. The mean is the minimum and the
/// residuals are the offsets from it, so the width has to leave room for a sign
/// bit — except at the full 32 bits, where the reader takes the raw difference
/// and adds it with wraparound.
fn write_variable_bits(values: &[i32], min: i32, width: u32, bits: &mut BitWriter) {
    const CHANGE_BITS: u32 = 3;
    const RUN_BITS: u32 = 3;
    /// The largest change that ends a run rather than continuing it.
    const LAST_STEP: u32 = 2;
    /// The largest run length a `run_bits`-bit count can name.
    const MAX_RUN: usize = (1 << RUN_BITS) - 1;

    let width = width.min(32);
    bits.i(min, 32);
    bits.u(CHANGE_BITS, CHANGE_BITS);
    bits.u(RUN_BITS, RUN_BITS);

    let mut remaining = width;
    while remaining > LAST_STEP {
        bits.i(3, CHANGE_BITS);
        remaining -= 3;
    }
    bits.i(remaining as i32, CHANGE_BITS);

    let mut position = 0;
    while position < values.len() {
        let run = (values.len() - position).min(MAX_RUN);
        bits.u(run as u32, RUN_BITS);
        for &value in &values[position..position + run] {
            bits.u((i64::from(value) - i64::from(min)) as u32, width);
        }
        position += run;
    }
}

/// Appends one mark-2 package holding raw `words`.
///
/// The decoder hands the package's values to the codec as signed integers, so a
/// coordinate's IEEE bit pattern goes in as its two's complement value.
fn package2_words(words: &[u32], version: JtVersion, out: &mut Vec<u8>) {
    let values: Vec<i32> = words.iter().map(|&word| word as i32).collect();
    package2(&values, version, out);
}

/// Appends the high-degree face attribute masks: a bare `VecU32`, i.e. `i32
/// count` followed by the words themselves.
///
/// Of all the symbol streams of a topological shape this is the only one the
/// format does not store as an integer codec package, so no codec byte follows
/// the count. The reader mirrors that in `mesh::read_u32_array`.
fn write_u32_array(words: &[u32], out: &mut Vec<u8>) {
    write_u32(out, words.len() as u32);
    for &word in words {
        write_u32(out, word);
    }
}

/// The smallest nibble count a `nibblerGet` of that many nibbles returns `value`
/// for. Each nibble carries four bits low-first and the reader sign-extends from
/// the whole run, so the count has to be exact.
fn nibbles_for(value: i32) -> u32 {
    (1..=8)
        .find(|&nibbles| {
            let half = 1i64 << (4 * nibbles - 1);
            i64::from(value) >= -half && i64::from(value) < half
        })
        .expect("every i32 fits eight nibbles")
}

/// The smallest bit count a `ReadI32Or0` of that many bits returns `value` for.
/// Zero bits is legal and reads as zero.
fn signed_bits(value: i32) -> u32 {
    if value == 0 {
        return 0;
    }
    (1..=32)
        .find(|&bits| {
            let half = 1i64 << (bits - 1);
            i64::from(value) >= -half && i64::from(value) < half
        })
        .expect("every i32 fits 32 bits")
}

/// A most-significant-bit-first sink for a CDP code text.
///
/// `JtDecode_BitReader` takes the bits of a package in order out of the most
/// significant bit of the first word, so the first bit written here is the most
/// significant bit of the first word, and the last word is padded at the bottom.
#[derive(Default)]
struct BitWriter {
    bits: Vec<u8>,
}

impl BitWriter {
    fn bit(&mut self, value: bool) {
        self.bits.push(u8::from(value));
    }

    fn u(&mut self, value: u32, width: u32) {
        for shift in (0..width).rev() {
            self.bits.push(((value >> shift) & 1) as u8);
        }
    }

    fn i(&mut self, value: i32, width: u32) {
        self.u(value as u32, width);
    }

    fn nibbler(&mut self, value: i32) {
        let nibbles = nibbles_for(value);
        for index in 0..nibbles {
            self.u(((value >> (4 * index)) as u32) & 0xF, 4);
            self.bit(index + 1 < nibbles);
        }
    }

    /// Writes the bit count of the code text and then the words holding it.
    fn finish(self, out: &mut Vec<u8>) {
        write_u32(out, self.bits.len() as u32);
        for chunk in self.bits.chunks(32) {
            let mut word = 0u32;
            for &bit in chunk {
                word = (word << 1) | u32::from(bit);
            }
            write_u32(out, word << (32 - chunk.len()));
        }
    }
}

/// The residuals a lag-1 predictor needs so that decoding reproduces `values`.
///
/// The reader leaves the first four values alone and then adds each value to
/// its predecessor, so the stored stream is those four values followed by
/// successive differences.
fn lag1_residuals(values: &[i32]) -> Vec<i32> {
    values
        .iter()
        .enumerate()
        .map(|(index, &value)| {
            if index < 4 {
                value
            } else {
                value.wrapping_sub(values[index - 1])
            }
        })
        .collect()
}

/// Writes the symbol packages of the dual walk in the order the reader loads
/// them, followed by the trailing hash it skips.
fn write_mesh_input(streams: &Streams, version: JtVersion, out: &mut Vec<u8>) {
    for context in 0..8 {
        package2(&streams.degrees[context], version, out);
    }
    package2(&streams.valences, version, out);
    package2(&streams.groups, version, out);
    package2(&lag1_residuals(&streams.face_flags), version, out);
    for context in 0..8 {
        package2(&streams.attr_masks[context], version, out);
    }
    // The two auxiliary mask arrays of context 7. JT 10.x folds them into one
    // stream and never reads the second. Both hold one value per context-7 mask
    // symbol; the mask here never needs a bit above 31, so both are zero. The
    // composite hash below makes one call per stream the file stores, so the
    // pair has to be hashed only where the pair is written.
    package2(&streams.attr_masks_high, version, out);
    let auxiliary_pair = version.major < 10;
    if auxiliary_pair {
        package2(&streams.attr_masks_high, version, out);
    }
    write_u32_array(&streams.attr_masks_large, out);
    package2(&lag1_residuals(&streams.split_faces), version, out);
    package2(&streams.split_positions, version, out);
    write_u32(
        out,
        composite_hash(
            &TopologySymbols {
                degrees: slices(&streams.degrees),
                valences: &streams.valences,
                groups: &streams.groups,
                face_flags: &streams.face_flags,
                attr_masks: slices(&streams.attr_masks),
                attr_masks_high: &streams.attr_masks_high,
                attr_masks_aux: &streams.attr_masks_high,
                attr_masks_large: &streams.attr_masks_large,
                split_faces: &streams.split_faces,
                split_positions: &streams.split_positions,
            },
            auxiliary_pair,
        ),
    );
}

/// Writes the 64-bit vertex binding of JT 9.x/10.x: three position components
/// and one normal component.
pub(crate) fn write_binding(out: &mut Vec<u8>) {
    write_u8(out, BINDING_LSB);
    write_u32(out, 0);
    write_u8(out, 0);
    write_u8(out, 0);
    write_u8(out, 0);
}

/// Writes the four quantization parameters of a lossless shape.
pub(crate) fn write_quantization_params(out: &mut Vec<u8>) {
    for value in [0u8, 0, 0, 0] {
        write_u8(out, value);
    }
}

/// Writes the 27-byte point quantizer of an unquantized coordinate set: three
/// components with a zero bit count, which selects the raw scheme.
fn write_quantizer(out: &mut Vec<u8>) {
    for _ in 0..3 {
        write_f32(out, 0.0);
        write_f32(out, 0.0);
        write_u8(out, 0);
    }
}

/// The combined hash of one array of vertex attributes: the words of each
/// component in turn, as the specification prescribes for a coordinate or
/// normal set written without quantization.
///
/// The granularity of the chain belongs to the revision. A nine-revision file
/// hashes one word per step, a ten-revision file one whole component array per
/// step, even though both store the very same words. The two CAD samples of the
/// same block prove it: their decoded vertex records are identical word for
/// word, and their stored hashes differ exactly in that respect.
fn attribute_hash(values: &[[f32; 3]], version: JtVersion) -> u32 {
    let mut hash = 0;
    if version.major >= 10 {
        for axis in 0..3 {
            let words: Vec<u32> = values.iter().map(|value| value[axis].to_bits()).collect();
            hash = hash32(&words, hash);
        }
        return hash;
    }
    for axis in 0..3 {
        for value in values {
            hash = hash32(&[value[axis].to_bits()], hash);
        }
    }
    hash
}

/// Writes the coordinate payload: raw IEEE words from JT 10.x on, an
/// exponent/mantissa split before that. The reader applies a lag-1 predictor to
/// coordinates, so the stored words are residuals.
fn write_coords(encoded: &Encoded, version: JtVersion, out: &mut Vec<u8>) {
    write_u32(out, encoded.positions.len() as u32);
    write_u8(out, 3);
    write_quantizer(out);
    if version.major >= 10 {
        for axis in 0..3 {
            let codes: Vec<i32> = encoded
                .positions
                .iter()
                .map(|p| p[axis].to_bits() as i32)
                .collect();
            package2(&lag1_residuals(&codes), version, out);
        }
    } else {
        for axis in 0..3 {
            let exponents: Vec<i32> = encoded
                .positions
                .iter()
                .map(|p| (p[axis].to_bits() >> 23) as i32)
                .collect();
            let mantissas: Vec<i32> = encoded
                .positions
                .iter()
                .map(|p| (p[axis].to_bits() & 0x007f_ffff) as i32)
                .collect();
            package2(&lag1_residuals(&exponents), version, out);
            package2(&lag1_residuals(&mantissas), version, out);
        }
    }
    write_u32(out, attribute_hash(&encoded.positions, version));
}

/// Writes the normal payload. The reader applies no predictor to normals, so
/// the words are stored as they are.
fn write_normals(encoded: &Encoded, version: JtVersion, out: &mut Vec<u8>) {
    write_u32(out, encoded.normals.len() as u32);
    write_u8(out, 3);
    write_u8(out, 0);
    if version.major >= 10 {
        for axis in 0..3 {
            let codes: Vec<u32> = encoded.normals.iter().map(|n| n[axis].to_bits()).collect();
            package2_words(&codes, version, out);
        }
    } else {
        for axis in 0..3 {
            let exponents: Vec<i32> = encoded
                .normals
                .iter()
                .map(|n| (n[axis].to_bits() >> 23) as i32)
                .collect();
            let mantissas: Vec<i32> = encoded
                .normals
                .iter()
                .map(|n| (n[axis].to_bits() & 0x007f_ffff) as i32)
                .collect();
            package2(&exponents, version, out);
            package2(&mantissas, version, out);
        }
    }
    write_u32(out, attribute_hash(&encoded.normals, version));
}

/// Serialises the body of a JT 9.x/10.x topologically compressed tri-strip set.
///
/// The body runs from the shape LOD base through the two payloads. The trailing
/// version of the element is written by the caller, matching the reader, which
/// reads the representation *before* that version.
///
/// # Errors
///
/// See [`encode`] for the meshes this representation cannot hold.
pub(crate) fn write_topological_body(
    mesh: &MeshData,
    version: JtVersion,
    out: &mut Vec<u8>,
) -> Result<()> {
    let encoded = encode(mesh)?;
    let order = ByteOrder::Little;
    // The shape LOD base: its class version, the element version, and the
    // 64-bit vertex binding.
    write_version(out, order, version.major, 1)?;
    write_version(out, order, version.major, 1)?;
    write_binding(out);
    // Where the coder input of a JT 10.x tri-strip set begins: it is wrapped in
    // a second, embedded logical element whose header the reader must walk
    // through to find it. The width is the same as the fields a JT 9.x file
    // stores in its place, which is why a mis-framed header still parses here
    // and renders nothing in a consumer that follows the nesting.
    let embedded = if version.major >= 10 {
        let at = out.len();
        write_u32(out, 0);
        out.extend_from_slice(&TOPOLOGY_ELEMENT_GUID);
        write_u8(out, TOPOLOGY_ELEMENT_BASE_TYPE);
        write_u32(out, TOPOLOGY_ELEMENT_OBJECT_ID);
        write_u8(out, TOPOLOGY_LOD_VERSION);
        write_u32(out, TOPOLOGY_VERTEX_RECORDS_ID);
        write_u8(out, TOPOLOGY_LOD_FLAG);
        debug_assert_eq!(out.len() - at, TOPOLOGY_EMBEDDED_HEADER);
        Some(at)
    } else {
        write_version(out, order, version.major, 2)?;
        write_u32(out, 0);
        write_version(out, order, version.major, 1)?;
        None
    };
    // The dual walk, then the binding and counts framing the two payloads.
    write_mesh_input(&encoded.streams, version, out);
    write_binding(out);
    write_quantization_params(out);
    write_u32(out, encoded.positions.len() as u32);
    write_u32(out, encoded.normals.len() as u32);
    write_coords(&encoded, version, out);
    write_normals(&encoded, version, out);
    // The flag field announced by the flag bit of the vertex binding: one value
    // per vertex attribute, so one per normal. The OpenJT reader stops after the
    // hash of the normals and never looks at it, which is why a missing flag
    // stream goes unnoticed here; a consumer that renders a shape from its
    // topological representation refuses the whole shape instead. The values
    // themselves are not read by either, so they are all zero. The element
    // version closing the body follows in the caller.
    write_u32(out, encoded.normals.len() as u32);
    package2(&vec![0; encoded.normals.len()], version, out);
    // The embedded element of a JT 10.x tri-strip set stops exactly one byte
    // before the element containing it: the version byte the caller appends
    // closes the outer element, and the length written here spans everything
    // between its own header and that byte.
    if let Some(at) = embedded {
        let length = u32::try_from(out.len() - at - 4).unwrap_or(u32::MAX);
        out[at..at + 4].copy_from_slice(&length.to_le_bytes());
    }
    Ok(())
}

fn write_u8(out: &mut Vec<u8>, value: u8) {
    out.write_u8_value(value)
        .expect("writing to a vector cannot fail");
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.write_u32_value(value, ByteOrder::Little)
        .expect("writing to a vector cannot fail");
}

fn write_f32(out: &mut Vec<u8>, value: f32) {
    out.write_f32_value(value, ByteOrder::Little)
        .expect("writing to a vector cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jt::decode::{Package, Unpack};
    use crate::jt::guid::types::SHAPE_LOD_TRI_STRIP_SET;
    use crate::jt::object::{Element, LodGeometry};
    use crate::jt::reader::Cursor;

    /// A closed cube: eight corners and twelve outward-facing triangles.
    fn cube() -> MeshData {
        let vertices = vec![
            -1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, -1.0, -1.0, -1.0, 1.0,
            1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
        ];
        let indices = vec![
            0, 3, 2, 0, 2, 1, 4, 5, 6, 4, 6, 7, 0, 4, 7, 0, 7, 3, 1, 2, 6, 1, 6, 5, 0, 1, 5, 0, 5,
            4, 3, 7, 6, 3, 6, 2,
        ];
        MeshData::new(vertices, indices).expect("the cube is a valid mesh")
    }

    /// A closed octahedron: six corners of four triangles each, so its coder
    /// faces all have degree four rather than the cube's mix of four and six.
    fn octahedron() -> MeshData {
        let vertices = vec![
            1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
            -1.0,
        ];
        let indices = vec![
            0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5,
        ];
        MeshData::new(vertices, indices).expect("the octahedron is a valid mesh")
    }

    /// Reads a body back the way a file does: the element appends its own
    /// trailing version after the representation.
    fn read_back(body: &[u8], version: JtVersion) -> Element {
        let mut data = body.to_vec();
        write_version(&mut data, ByteOrder::Little, version.major, 1)
            .expect("writing to a vector cannot fail");
        let mut cursor = Cursor::new(&data, ByteOrder::Little, version.major);
        Element::read(&SHAPE_LOD_TRI_STRIP_SET, &mut cursor)
            .expect("the encoded representation must read back")
    }

    fn point(geometry: &LodGeometry, index: i32) -> [f32; 3] {
        let Ok(at) = usize::try_from(index) else {
            return [f32::NAN; 3];
        };
        if at * 3 + 2 >= geometry.vertices.len() {
            return [f32::NAN; 3];
        }
        [
            geometry.vertices[at * 3],
            geometry.vertices[at * 3 + 1],
            geometry.vertices[at * 3 + 2],
        ]
    }

    /// The triangles of a decoded shape as a winding-insensitive sorted set, so
    /// that a walk which numbers its slots differently still compares equal.
    fn triangle_set(geometry: &LodGeometry) -> Vec<[[f32; 3]; 3]> {
        let mut out: Vec<[[f32; 3]; 3]> = geometry
            .indices
            .chunks(3)
            .map(|corner| {
                let mut triangle = [
                    point(geometry, corner[0]),
                    point(geometry, corner[1]),
                    point(geometry, corner[2]),
                ];
                triangle.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                triangle
            })
            .collect();
        out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// The (position, normal) pairs a mesh stores, as a sorted set. Normals are
    /// compared by magnitude because a walk may flip the winding of a triangle.
    fn shading_set(vertices: &[f32], normals: &[f32]) -> Vec<([f32; 3], [f32; 3])> {
        let mut out: Vec<([f32; 3], [f32; 3])> = vertices
            .chunks(3)
            .zip(normals.chunks(3))
            .map(|(p, n)| ([p[0], p[1], p[2]], [n[0].abs(), n[1].abs(), n[2].abs()]))
            .collect();
        out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out.dedup();
        out
    }

    /// The hash of a vertex array chains at a different granularity per
    /// revision, and the difference is invisible in the bytes: both chains see
    /// the same words in the same order and differ only in where the length
    /// terms and the `mix` calls fall. A strict viewer checks that hash and
    /// blanks the shape when it disagrees, so the split must stay pinned.
    #[test]
    fn the_vertex_hash_chains_one_word_at_nine_and_one_array_at_ten() {
        let values = [[1.0f32, 2.0, 4.0], [0.5, 3.0, 5.0]];
        let words =
            |axis: usize| -> Vec<u32> { values.iter().map(|v| v[axis].to_bits()).collect() };
        let mut per_word = 0;
        for axis in 0..3 {
            for value in values {
                per_word = hash32(&[value[axis].to_bits()], per_word);
            }
        }
        let mut per_array = 0;
        for axis in 0..3 {
            per_array = hash32(&words(axis), per_array);
        }
        assert_ne!(per_word, per_array, "the two chains must differ");
        assert_eq!(attribute_hash(&values, JtVersion::V9_5), per_word);
        assert_eq!(attribute_hash(&values, JtVersion::V10_3), per_array);
    }

    /// A JT 10.x tri-strip set wraps its coder input in a second, nested
    /// logical element. The header is the same width as the fields a JT 9.x
    /// file stores in the same place, so a body that carries those fields
    /// unframed still reads here — and renders nothing in a consumer that
    /// walks the nesting. The length must then stop one byte before the outer
    /// element, whose closing version the caller appends.
    #[test]
    fn a_ten_x_body_wraps_its_coder_input_in_an_embedded_element() {
        let mut body = Vec::new();
        write_topological_body(&cube(), JtVersion::V10_3, &mut body)
            .expect("the cube is a valid mesh for the coder");
        let guid = body
            .windows(TOPOLOGY_ELEMENT_GUID.len())
            .position(|window| window == TOPOLOGY_ELEMENT_GUID)
            .expect("the embedded element must name its object type id");
        // Two byte-wide versions and the 64-bit vertex binding precede it, and
        // its own length prefix precedes the object type id.
        let at = guid - 4;
        assert_eq!(at, 2 + 8);
        assert_eq!(body[at + 20], TOPOLOGY_ELEMENT_BASE_TYPE);
        assert_eq!(
            &body[at + 21..at + 25],
            TOPOLOGY_ELEMENT_OBJECT_ID.to_le_bytes()
        );
        assert_eq!(body[at + 25], TOPOLOGY_LOD_VERSION);
        assert_eq!(
            &body[at + 26..at + 30],
            TOPOLOGY_VERTEX_RECORDS_ID.to_le_bytes()
        );
        assert_eq!(body[at + 30], TOPOLOGY_LOD_FLAG);
        // The coder input begins immediately behind the embedded header, and it
        // begins at the same place a JT 9.x file keeps the same three fields
        // loose: the two revisions carry the identical dual walk, so the value
        // count of its first package is the same number at the same distance
        // behind the vertex binding. A header that is merely those bytes
        // without the framing moves that distance and the shape renders blank.
        let mut nine = Vec::new();
        write_topological_body(&cube(), JtVersion::V9_5, &mut nine)
            .expect("the cube is a valid mesh for the coder");
        // Two word-wide versions and the 64-bit vertex binding, then the same
        // three fields standing loose: a word, a double word and a word.
        let nine_at = 2 + 2 + 8 + 2 + 4 + 2;
        assert_eq!(
            &body[at + TOPOLOGY_EMBEDDED_HEADER..at + TOPOLOGY_EMBEDDED_HEADER + 4],
            &nine[nine_at..nine_at + 4],
            "the coder input of a JT 10.x shape must begin right behind the \
             embedded header, at the same distance a JT 9.x file keeps its \
             unframed fields"
        );

        // The element containing the embedded one closes with its own version,
        // which the caller appends after this body.
        write_version(&mut body, ByteOrder::Little, 10, 1)
            .expect("writing to a vector cannot fail");
        let length =
            u32::from_le_bytes(body[at..at + 4].try_into().expect("four bytes make a u32"));
        assert_ne!(length, 0, "the embedded element must not be empty");
        assert_eq!(
            usize::try_from(length).expect("the length fits a usize"),
            body.len() - at - 4 - 1,
            "the embedded element must stop exactly where the outer element's \
             closing version begins"
        );
    }

    /// JT 9.x has no such nesting: the same three fields stand on their own.
    #[test]
    fn a_nine_x_body_carries_no_embedded_element() {
        let mut body = Vec::new();
        write_topological_body(&cube(), JtVersion::V9_5, &mut body)
            .expect("the cube is a valid mesh for the coder");
        assert!(
            !body
                .windows(TOPOLOGY_ELEMENT_GUID.len())
                .any(|window| window == TOPOLOGY_ELEMENT_GUID)
        );
    }

    #[test]
    fn a_closed_cube_survives_the_topological_round_trip() {
        round_trip(&cube(), 36);
    }

    #[test]
    fn a_closed_octahedron_survives_the_topological_round_trip() {
        round_trip(&octahedron(), 24);
    }

    #[test]
    fn a_context_seven_face_carries_both_auxiliary_mask_streams() {
        // A coder face of degree nine reads its attribute mask from context 7,
        // and the reader widens that mask by ORing two auxiliary arrays into it
        // **at the same read position as the mask itself**. So every context-7
        // mask symbol needs a value in each auxiliary stream even when, as here,
        // no bit above 29 is ever set — and a consumer that renders a shape from
        // its topological representation refuses a context-7 face whose
        // auxiliary streams are empty, which blanks the whole shape.
        //
        // The cube (degrees 4 and 6) and the octahedron (degree 4) never reach
        // context 7, which is why a block always rendered and only a file with a
        // high-valence vertex — the plate's screws — came out empty.
        let mesh = enneagon_bipyramid();
        let encoded = encode(&mesh).expect("a bipyramid is a closed two-manifold");
        let masks = encoded.streams.attr_masks[7].len();
        assert!(
            masks > 0,
            "the fixture must produce a coder face of degree nine"
        );
        assert_eq!(
            masks,
            encoded.streams.attr_masks_high.len(),
            "every context-7 mask symbol needs one auxiliary value"
        );

        for version in [JtVersion::V9_5, JtVersion::V10_3] {
            let mut body = Vec::new();
            write_topological_body(&mesh, version, &mut body).expect("the mesh can be encoded");
            let auxiliary = auxiliary_masks(&body, version);
            // JT 10.x folds the pair into one stream and never reads the second.
            assert_eq!(
                auxiliary.len(),
                if version.major >= 10 { 1 } else { 2 },
                "{version} stores one auxiliary mask stream, not two"
            );
            for (at, package) in auxiliary.iter().enumerate() {
                assert!(
                    package.out_val_count() > 0,
                    "{version} left auxiliary mask stream {at} empty"
                );
                assert_eq!(
                    package.out_val_count(),
                    masks,
                    "auxiliary mask stream {at} of {version} does not track the \
                     context-7 masks"
                );
            }
        }
    }

    /// A closed bipyramid over a nine-gon: two apices of nine triangles each and
    /// nine equator corners of four. The apices are coder faces of **degree
    /// nine**, the first degree whose attribute mask is read from context 7.
    fn enneagon_bipyramid() -> MeshData {
        let mut vertices = vec![0.0, 0.0, 1.0, 0.0, 0.0, -1.0];
        for step in 0..9i32 {
            let angle = std::f32::consts::TAU * step as f32 / 9.0;
            vertices.extend_from_slice(&[angle.cos(), angle.sin(), 0.0]);
        }
        let mut indices = Vec::new();
        for step in 0..9i32 {
            let next = (step + 1) % 9;
            // Both cones wound outward, so the mesh is consistently oriented.
            indices.extend_from_slice(&[0, 2 + step, 2 + next]);
            indices.extend_from_slice(&[1, 2 + next, 2 + step]);
        }
        MeshData::new(vertices, indices).expect("a bipyramid is a valid mesh")
    }

    /// A closed bipyramid over a seventy-gon: its two apices are coder faces of
    /// **degree seventy**. That is the first degree whose attribute mask no
    /// longer fits the eight inline streams — the reader takes
    /// `ceil(70 / 32) = 3` words per apex from the high-degree array, which the
    /// format stores as a plain array rather than a codec package.
    fn heptegintagon_bipyramid() -> MeshData {
        let base = 70i32;
        let mut vertices = vec![0.0, 0.0, 1.0, 0.0, 0.0, -1.0];
        for step in 0..base {
            let angle = std::f32::consts::TAU * step as f32 / base as f32;
            vertices.extend_from_slice(&[angle.cos(), angle.sin(), 0.0]);
        }
        let mut indices = Vec::new();
        for step in 0..base {
            let next = (step + 1) % base;
            indices.extend_from_slice(&[0, 2 + step, 2 + next]);
            indices.extend_from_slice(&[1, 2 + next, 2 + step]);
        }
        MeshData::new(vertices, indices).expect("a bipyramid is a valid mesh")
    }

    /// The high-degree attribute masks must be framed exactly as the format
    /// stores them. Written as a codec package they shift every following
    /// stream, and because the array is the last one before the split symbols
    /// the coder then dies on garbage: the shape is dropped without a word of
    /// complaint beyond the exhausted-stream message.
    #[test]
    fn a_wide_attribute_mask_round_trips_through_the_plain_array() {
        let mesh = heptegintagon_bipyramid();
        let encoded = encode(&mesh).expect("the bipyramid can be encoded");
        assert!(
            !encoded.streams.attr_masks_large.is_empty(),
            "the fixture never reached the high-degree attribute masks, so it \
             proves nothing"
        );
        // 140 triangles of three corners each.
        round_trip(&mesh, 420);
    }

    /// The two auxiliary attribute-mask streams of a body, read in the order
    /// [`write_mesh_input`] writes them. Walking the framing that precedes them
    /// is part of the check: a stream written out of order is a stream no
    /// reader finds.
    fn auxiliary_masks(body: &[u8], version: JtVersion) -> Vec<Package> {
        let mut cursor = Cursor::new(body, ByteOrder::Little, version.major);
        let next = |cursor: &mut Cursor<'_>| {
            Package::load2(cursor).expect("a symbol package of the walk must read back")
        };
        // The shape LOD base: two class versions and the 64-bit vertex binding.
        cursor
            .skip(if version.major >= 10 { 2 } else { 4 })
            .expect("the shape LOD base");
        cursor.skip(8).expect("the vertex binding");
        if version.major >= 10 {
            // The tri-strip set of JT 10.x wraps its coder input in an embedded
            // logical element header.
            cursor
                .skip(TOPOLOGY_EMBEDDED_HEADER)
                .expect("the embedded element header of a JT 10.x shape");
        } else {
            cursor.skip(2 + 4 + 2).expect("the representation version");
        }
        // The symbol streams up to the pair in question.
        for _ in 0..8 {
            next(&mut cursor);
        }
        next(&mut cursor); // valences
        next(&mut cursor); // groups
        next(&mut cursor); // face flags
        for _ in 0..8 {
            next(&mut cursor);
        }
        let mut out = vec![next(&mut cursor)];
        if version.major < 10 {
            out.push(next(&mut cursor));
        }
        out
    }

    /// Encodes `mesh` for both revisions that carry a topological representation
    /// and checks that the reader hands back exactly the same shape. `corners`
    /// is the number of triangle corners of the input.
    fn round_trip(mesh: &MeshData, corners: usize) {
        for version in [JtVersion::V9_5, JtVersion::V10_3] {
            let mut body = Vec::new();
            write_topological_body(mesh, version, &mut body).expect("the mesh can be encoded");
            let element = read_back(&body, version);
            let geometry = element
                .geometry()
                .expect("a tri strip set carries geometry");
            // The encoder sets every bit of every attribute mask, so each face
            // slot — that is, each corner of the input — gets its own normal
            // attribute and therefore its own output record.
            assert_eq!(
                geometry.indices.len(),
                corners,
                "{version} lost corners of the mesh"
            );
            assert_eq!(
                geometry.vertex_count, corners,
                "{version} changed the records of the mesh"
            );
            assert_eq!(
                geometry.normal_count, corners,
                "{version} lost the normals of the mesh"
            );
            assert_eq!(
                triangle_set(geometry),
                triangle_set(&LodGeometry {
                    indices: mesh.indices().to_vec(),
                    vertices: mesh.vertices().to_vec(),
                    normals: mesh.normals().to_vec(),
                    vertex_count: corners,
                    normal_count: corners,
                    normal_binding: None,
                }),
                "{version} changed the triangles of the mesh"
            );
            assert_eq!(
                shading_set(&geometry.vertices, &geometry.normals),
                shading_set(mesh.vertices(), mesh.normals()),
                "{version} changed the shading of the mesh"
            );
        }
    }

    #[test]
    fn an_open_shell_is_refused_instead_of_encoded_wrong() {
        // A single triangle has three edges that belong to one triangle each,
        // so its dual is not a closed mesh and cannot be encoded at all.
        let mesh = MeshData::new(
            vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            vec![0, 1, 2],
        )
        .expect("one triangle is a valid mesh");
        let mut body = Vec::new();
        let error = write_topological_body(&mesh, JtVersion::V10_3, &mut body)
            .expect_err("an open shell cannot be encoded");
        assert!(
            error.to_string().contains("closed two-manifold"),
            "the error says {error}"
        );
        assert!(body.is_empty(), "nothing may be written for a refused mesh");
    }

    #[test]
    fn every_package_we_write_decodes_back_with_its_own_codec() {
        // A non-empty package is written with bitlength2 (codec 1), the codec
        // every real file uses. The null codec (0) is accepted by the reference
        // reader but produces no geometry in a consumer that renders a shape
        // from its topological representation, so it must never be emitted for
        // values that are there. Reading the package back with our own decoder
        // proves the code text is well formed for the revision written.
        for version in [JtVersion::V9_5, JtVersion::V10_3] {
            for values in [
                vec![0, 1, 2, 3],
                vec![7],
                vec![-5, -5, -5],
                vec![i32::MIN, -1, 0, 1, i32::MAX],
                (0..200).map(|i| (i * 37) % 1000 - 500).collect(),
            ] {
                let mut body = Vec::new();
                package2(&values, version, &mut body);
                assert_eq!(body[4] as u32, 1, "{version} wrote the null codec");
                assert_eq!(
                    decode_package(&body, version),
                    values,
                    "{version} did not decode the package back"
                );
            }
            let mut empty = Vec::new();
            package2(&[], version, &mut empty);
            assert_eq!(empty, vec![0, 0, 0, 0], "an empty package is its count");
        }
    }

    /// Read one mark 2 package from `body` and decode it the way a file does.
    fn decode_package(body: &[u8], version: JtVersion) -> Vec<i32> {
        let mut cursor = Cursor::new(body, ByteOrder::Little, version.major);
        let mut package = Package::load2(&mut cursor).expect("the package we wrote must read back");
        package
            .decode(ByteOrder::Little, Unpack::Null)
            .expect("the code text we wrote must decode")
    }
}
