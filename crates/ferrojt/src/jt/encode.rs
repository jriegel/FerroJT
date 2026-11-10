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
use crate::jt::version::JtVersion;
use crate::jt::write::write_version;

/// The 16-byte marker a JT 10.x tri-strip set stores before its embedded
/// element header; the reader compares it and skips nine opaque bytes.
const TOPOLOGY_MARKER: [u8; 16] = [
    0x30, 0xf8, 0x4c, 0xbe, 0xbc, 0x4f, 0x9b, 0x5f, 0xb9, 0x26, 0x92, 0x78, 0xd2, 0xe1, 0x09, 0x01,
];

/// The low byte of a 64-bit vertex binding: three position components, one
/// normal component, no colour.
const BINDING_LSB: u8 = 0x4a;

/// The nine bytes a JT 10.x tri-strip set stores after the topology marker.
/// They hold a logical element header (GUID tail, flag, id) that the reader
/// skips without inspecting, so a neutral value is written.
const EMBEDDED_HEADER: [u8; 9] = [0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01];

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
        let mut ring: Vec<Vec<i32>> = vec![Vec::new(); positions.len()];
        for (tri, welded) in tri_welded.iter().enumerate() {
            for corner in 0..3 {
                ring[welded[corner] as usize].push(tri as i32);
            }
        }
        for (welded, ring) in ring.iter_mut().enumerate() {
            if ring.len() < 3 {
                return Err(Error::invalid(
                    0,
                    "a vertex of the mesh has fewer than three faces, so it cannot be written \
                     as JT 9.x/10.x",
                ));
            }
            let normal = vertex_normal(&tri_welded, &positions, ring);
            let (axis_u, axis_v) = tangent_basis(normal);
            let center = positions[welded];
            let mut angles: Vec<(i32, f64)> = ring
                .iter()
                .map(|&tri| {
                    let (du, dv) = ring_angle(
                        &tri_welded[tri as usize],
                        welded as i32,
                        &positions,
                        center,
                        axis_u,
                        axis_v,
                    );
                    (tri, dv.atan2(du))
                })
                .collect();
            angles.sort_by(|a, b| a.1.total_cmp(&b.1));
            *ring = angles.into_iter().map(|(tri, _)| tri).collect();
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

/// The angle of triangle `tri` about `center`, measured in the tangent plane of
/// `center` from the midpoints of the two edges leaving that vertex.
fn ring_angle(
    corners: &[i32; 3],
    center: i32,
    positions: &[[f32; 3]],
    origin: [f32; 3],
    axis_u: [f64; 3],
    axis_v: [f64; 3],
) -> (f64, f64) {
    let mut du = 0.0;
    let mut dv = 0.0;
    for &other in corners {
        if other == center {
            continue;
        }
        let point = positions[other as usize];
        let dx = f64::from(point[0] - origin[0]);
        let dy = f64::from(point[1] - origin[1]);
        let dz = f64::from(point[2] - origin[2]);
        du += dx * axis_u[0] + dy * axis_u[1] + dz * axis_u[2];
        dv += dx * axis_v[0] + dy * axis_v[1] + dz * axis_v[2];
    }
    (du, dv)
}

/// The summed face normal of the triangles in `ring`.
fn vertex_normal(triangles: &[[i32; 3]], positions: &[[f32; 3]], ring: &[i32]) -> [f64; 3] {
    let mut sum = [0.0f64; 3];
    for &tri in ring {
        let [a, b, c] = triangles[tri as usize];
        let pa = positions[a as usize];
        let pb = positions[b as usize];
        let pc = positions[c as usize];
        let ux = f64::from(pb[0] - pa[0]);
        let uy = f64::from(pb[1] - pa[1]);
        let uz = f64::from(pb[2] - pa[2]);
        let vx = f64::from(pc[0] - pa[0]);
        let vy = f64::from(pc[1] - pa[1]);
        let vz = f64::from(pc[2] - pa[2]);
        sum[0] += uy * vz - uz * vy;
        sum[1] += uz * vx - ux * vz;
        sum[2] += ux * vy - uy * vx;
    }
    sum
}

/// Two unit vectors spanning the plane perpendicular to `normal`.
fn tangent_basis(normal: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let len = vec_len(normal);
    let n = if len > 0.0 {
        [normal[0] / len, normal[1] / len, normal[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    };
    let helper = if n[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let u = cross(helper, n);
    let ulen = vec_len(u);
    if ulen == 0.0 {
        return ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    }
    let u = [u[0] / ulen, u[1] / ulen, u[2] / ulen];
    (u, cross(n, u))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn vec_len(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
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

/// Appends one mark-2 package holding `values` verbatim.
///
/// A null package stores its values as raw 32-bit words; an empty package is
/// only its zero value count, because the codec byte is never reached.
fn package2(values: &[i32], out: &mut Vec<u8>) {
    write_u32(out, values.len() as u32);
    if values.is_empty() {
        return;
    }
    write_u8(out, 0);
    write_u32(out, values.len() as u32 * 32);
    for value in values {
        write_u32(out, *value as u32);
    }
}

/// Appends one mark-2 package holding raw `words`.
fn package2_words(words: &[u32], out: &mut Vec<u8>) {
    write_u32(out, words.len() as u32);
    if words.is_empty() {
        return;
    }
    write_u8(out, 0);
    write_u32(out, words.len() as u32 * 32);
    for word in words {
        write_u32(out, *word);
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
        package2(&streams.degrees[context], out);
    }
    package2(&streams.valences, out);
    package2(&streams.groups, out);
    package2(&lag1_residuals(&streams.face_flags), out);
    for context in 0..8 {
        package2(&streams.attr_masks[context], out);
    }
    // The two auxiliary mask arrays of context 7. JT 10.x folds them into one
    // stream and never reads the second.
    package2(&[], out);
    if version.major < 10 {
        package2(&[], out);
    }
    package2_words(&streams.attr_masks_large, out);
    package2(&lag1_residuals(&streams.split_faces), out);
    package2(&streams.split_positions, out);
    write_u32(out, 0);
}

/// Writes the 64-bit vertex binding of JT 9.x/10.x: three position components
/// and one normal component.
fn write_binding(out: &mut Vec<u8>) {
    write_u8(out, BINDING_LSB);
    write_u32(out, 0);
    write_u8(out, 0);
    write_u8(out, 0);
    write_u8(out, 0);
}

/// Writes the four quantization parameters of a lossless shape.
fn write_quantization_params(out: &mut Vec<u8>) {
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
            package2(&lag1_residuals(&codes), out);
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
            package2(&lag1_residuals(&exponents), out);
            package2(&lag1_residuals(&mantissas), out);
        }
    }
    write_u32(out, 0);
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
            package2_words(&codes, out);
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
            package2(&exponents, out);
            package2(&mantissas, out);
        }
    }
    write_u32(out, 0);
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
    if version.major >= 10 {
        write_version(out, order, version.major, 1)?;
        write_version(out, order, version.major, 2)?;
        write_u32(out, 0);
        out.extend_from_slice(&TOPOLOGY_MARKER);
        out.extend_from_slice(&EMBEDDED_HEADER);
    } else {
        write_version(out, order, version.major, 2)?;
        write_u32(out, 0);
        write_version(out, order, version.major, 1)?;
    }
    // The dual walk, then the binding and counts framing the two payloads.
    write_mesh_input(&encoded.streams, version, out);
    write_binding(out);
    write_quantization_params(out);
    write_u32(out, encoded.positions.len() as u32);
    write_u32(out, encoded.normals.len() as u32);
    write_coords(&encoded, version, out);
    write_normals(&encoded, version, out);
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

    #[test]
    fn a_closed_cube_survives_the_topological_round_trip() {
        round_trip(&cube(), 36);
    }

    #[test]
    fn a_closed_octahedron_survives_the_topological_round_trip() {
        round_trip(&octahedron(), 24);
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
}
