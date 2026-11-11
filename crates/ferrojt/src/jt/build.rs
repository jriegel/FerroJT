//! Building a JT file from scratch.
//!
//! [`write_to_bytes`] re-serialises the scene graph of a file that already
//! exists and copies every other segment out of that file byte for byte, so it
//! cannot create a file out of nothing. This module can: [`JtBuilder`] starts
//! with an empty scene graph, accumulates nodes, meshes and instances, and
//! writes a complete JT file.
//!
//! The files it emits are **JT 8.1**. That is the newest version whose
//! representation codec is available here: the dual vertex-face coder of JT
//! 9.x and 10.x has no encoder in this crate, so a from-scratch file uses the
//! vertex-based lossless representation of JT 8.x — the one [`LodGeometry`]
//! hands back with `params[0] == 0`.

use std::path::Path;

use crate::codec::{ByteOrder, WriterExt, deflate};
use crate::error::{Error, Result};
use crate::jt::guid::{Guid, types};
use crate::jt::segment::TocEntry;
use crate::jt::write::{
    BASE_ATTRIBUTE, BASE_GROUP, BASE_INSTANCE, BASE_SHAPE, header_length, segment_bytes,
    write_atom, write_attribute_base, write_box, write_counts, write_deferred,
    write_end_of_elements, write_guid, write_mb_string, write_version,
};
use crate::model::Transform;

use super::LodGeometry;

/// The major version the builder writes.
const MAJOR: u32 = 8;
/// The minor version stored in the banner.
const MINOR: u32 = 1;
/// Byte order of every file the builder emits.
const ORDER: ByteOrder = ByteOrder::Little;
/// The property key that names a node.
const NAME_KEY: &str = "JT_PROP_NAME";
/// The property key that points a shape at its representation segment.
const SHAPE_KEY: &str = "JT_LLPROP_SHAPEIMPL";
/// The state every property atom stores.
const PROPERTY_STATE: u32 = 0x4000_0000;
/// Base type byte of a string property atom.
const BASE_PROPERTY_STRING: u8 = 5;
/// Base type byte of a late loaded property atom.
const BASE_PROPERTY_LATE_LOADED: u8 = 8;
/// Base type byte of an element atom.
const BASE_ELEMENT: u8 = 4;
/// Bytes between the length field and the body of an atom outside the LSG.
///
/// The scene graph numbers its objects, so an LSG atom carries an object id
/// and its length counts 21 envelope bytes. A representation segment numbers
/// nothing before JT 9.x, so its length counts only the GUID and the base type
/// byte — see [`read_element_envelope`].
const ELEMENT_ENVELOPE: usize = 16 + 1;
/// Bytes between the body of a segment and its declared size when the segment
/// carries no compression header, as a shape segment does not.
const PLAIN_SEGMENT_ENVELOPE: usize = 4 + 16 + 4;

/// The triangles of one shape, in the layout a JT representation stores.
///
/// The three corners of a triangle are three separate slots: `vertices` holds
/// three positions per triangle, `indices` counts those slots, and `normals`
/// holds one normal per slot. A cube therefore has 36 vertices, 36 normals and
/// 36 indices for its 12 triangles — JT 8.x stores a faceted shape that way,
/// and it is what [`LodGeometry`] hands back when a file is read again.
#[derive(Clone, Debug, Default)]
pub struct MeshData {
    vertices: Vec<f32>,
    normals: Vec<f32>,
    indices: Vec<i32>,
}

impl MeshData {
    /// Build a mesh from positions and slot indices, with flat normals.
    ///
    /// Each triangle gets the normal of its own plane, which is what a CAD
    /// writer stores for a faceted part. A corner shared by several triangles
    /// is stored once per triangle, because a faceted normal cannot be shared
    /// either: the mesh is exploded, so `vertices` comes back longer than it
    /// went in and `indices` counts the slots of the result.
    ///
    /// # Errors
    ///
    /// See [`MeshData::validate`].
    pub fn new(vertices: Vec<f32>, indices: Vec<i32>) -> Result<Self> {
        let mesh = Self {
            vertices,
            normals: Vec::new(),
            indices,
        };
        mesh.validate()?;
        let (vertices, _, indices) = explode(&mesh.vertices, &[], &mesh.indices);
        let normals = flat_normals(&vertices, &indices);
        Ok(Self {
            vertices,
            normals,
            indices,
        })
    }

    /// Build a mesh with one explicit normal per slot.
    ///
    /// The mesh is exploded the way [`MeshData::new`] explodes it, so the
    /// normals of a shared corner are repeated once per triangle.
    ///
    /// # Errors
    ///
    /// See [`MeshData::validate`].
    pub fn with_normals(vertices: Vec<f32>, normals: Vec<f32>, indices: Vec<i32>) -> Result<Self> {
        let mesh = Self {
            vertices,
            normals,
            indices,
        };
        mesh.validate()?;
        let (vertices, normals, indices) = explode(&mesh.vertices, &mesh.normals, &mesh.indices);
        Ok(Self {
            vertices,
            normals,
            indices,
        })
    }

    /// Build a mesh from a representation that was read out of a file.
    #[must_use]
    pub fn from_geometry(geometry: &LodGeometry) -> Self {
        Self {
            vertices: geometry.vertices.clone(),
            normals: geometry.normals.clone(),
            indices: geometry.indices.clone(),
        }
    }

    /// The positions, three floats per slot.
    #[must_use]
    pub fn vertices(&self) -> &[f32] {
        &self.vertices
    }

    /// The normals, three floats per slot.
    #[must_use]
    pub fn normals(&self) -> &[f32] {
        &self.normals
    }

    /// The slot indices, three per triangle.
    #[must_use]
    pub fn indices(&self) -> &[i32] {
        &self.indices
    }

    /// The number of triangles.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Check that the three arrays describe one consistent mesh.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] is returned when there are no triangles, when the
    /// positions are not a whole number of triples, when the index count is
    /// not a multiple of three, when an index is out of range, or when normals
    /// are present but not one triple per slot.
    pub fn validate(&self) -> Result<()> {
        if self.vertices.len() % 3 != 0 {
            return Err(Error::invalid(
                0,
                "the vertices of a mesh must be triples of floats",
            ));
        }
        if self.indices.is_empty() {
            return Err(Error::invalid(0, "a mesh needs at least one triangle"));
        }
        if self.indices.len() % 3 != 0 {
            return Err(Error::invalid(
                0,
                "the indices of a mesh must come in triangles",
            ));
        }
        let slots = self.vertices.len() / 3;
        if let Some(bad) = self
            .indices
            .iter()
            .find(|index| **index < 0 || **index as usize >= slots)
        {
            return Err(Error::invalid(
                0,
                format!("the index {bad} is outside the {slots} slots of the mesh"),
            ));
        }
        if !self.normals.is_empty() && self.normals.len() != self.vertices.len() {
            return Err(Error::invalid(
                0,
                "a mesh needs one normal triple per vertex slot",
            ));
        }
        Ok(())
    }

    /// The axis aligned box of the positions, `None` when there are none.
    #[must_use]
    pub fn bounds(&self) -> Option<[f32; 6]> {
        let mut box_ = [f32::MAX, f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MIN];
        for point in self.vertices.chunks(3) {
            for axis in 0..3 {
                box_[axis] = box_[axis].min(point[axis]);
                box_[3 + axis] = box_[3 + axis].max(point[axis]);
            }
        }
        (box_[0] <= box_[3]).then_some(box_)
    }
}

/// Rebuild a mesh so that every triangle corner is its own vertex slot, the way
/// a JT 8.x faceted shape stores it. `normals` may be empty, in which case it is
/// passed through unchanged.
fn explode(vertices: &[f32], normals: &[f32], indices: &[i32]) -> (Vec<f32>, Vec<f32>, Vec<i32>) {
    let with_normals = !normals.is_empty();
    let mut out_vertices: Vec<f32> = Vec::with_capacity(indices.len() * 3);
    let mut out_normals: Vec<f32> = Vec::new();
    let mut out_indices: Vec<i32> = Vec::with_capacity(indices.len());
    for (corner, index) in indices.iter().enumerate() {
        let at = usize::try_from(*index).unwrap_or(0) * 3;
        out_vertices.extend_from_slice(&vertices[at..at + 3]);
        if with_normals {
            out_normals.extend_from_slice(&normals[at..at + 3]);
        }
        out_indices.push(corner as i32);
    }
    (out_vertices, out_normals, out_indices)
}

/// The normal of the plane of each triangle, one triple per slot.
fn flat_normals(vertices: &[f32], indices: &[i32]) -> Vec<f32> {
    let mut normals = vec![0.0f32; vertices.len()];
    let point = |slot: i32| -> [f32; 3] {
        let at = usize::try_from(slot).unwrap_or(0) * 3;
        let at = at.min(vertices.len().saturating_sub(3));
        [vertices[at], vertices[at + 1], vertices[at + 2]]
    };
    for triangle in indices.chunks(3) {
        let (a, b, c) = (point(triangle[0]), point(triangle[1]), point(triangle[2]));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let n = if length > 0.0 {
            [n[0] / length, n[1] / length, n[2] / length]
        } else {
            [0.0, 0.0, 1.0]
        };
        for slot in triangle {
            let Ok(at) = usize::try_from(*slot) else {
                continue;
            };
            if at * 3 + 2 >= normals.len() {
                continue;
            }
            normals[at * 3] = n[0];
            normals[at * 3 + 1] = n[1];
            normals[at * 3 + 2] = n[2];
        }
    }
    normals
}

/// The class of a node the builder creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildNode {
    /// The single root of the scene graph.
    Partition,
    /// A part: a named node that owns a shape.
    Part,
    /// A meta data node: a group that carries key/value properties.
    MetaData,
    /// A group: an interior node with children.
    Group,
    /// A shape node: the leaf that holds one mesh.
    Shape,
    /// An instance: a reference to the subtree of another node.
    Instance,
}

impl BuildNode {
    /// Whether this class can have children.
    #[must_use]
    pub fn accepts_children(self) -> bool {
        !matches!(self, Self::Shape | Self::Instance)
    }
}

/// A handle to a node of a file under construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JtNode(usize);

impl JtNode {
    /// The position of this node inside the builder.
    #[must_use]
    pub fn index(&self) -> usize {
        self.0
    }
}

/// A handle to a mesh added to a builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JtMesh(usize);

impl JtMesh {
    /// The position of this mesh inside the builder.
    #[must_use]
    pub fn index(&self) -> usize {
        self.0
    }
}

/// The shading of a shape, written as a JT material attribute.
#[derive(Clone, Copy, Debug)]
pub struct MaterialData {
    /// Ambient colour, RGBA.
    pub ambient: [f32; 4],
    /// Diffuse colour, RGBA. This is the colour an export shows.
    pub diffuse: [f32; 4],
    /// Specular colour, RGBA.
    pub specular: [f32; 4],
    /// Emissive colour, RGBA.
    pub emission: [f32; 4],
    /// Shininess exponent.
    pub shininess: f32,
    /// Reflectivity, only stored from JT 10.x on.
    pub reflectivity: f32,
}

impl Default for MaterialData {
    fn default() -> Self {
        Self {
            ambient: [0.75, 0.75, 0.75, 1.0],
            diffuse: [0.75, 0.75, 0.75, 1.0],
            specular: [0.0, 0.0, 0.0, 1.0],
            emission: [0.0, 0.0, 0.0, 1.0],
            shininess: 128.0,
            reflectivity: 0.0,
        }
    }
}

/// One node of the graph under construction.
#[derive(Clone, Debug)]
struct Slot {
    class: BuildNode,
    name: Option<String>,
    children: Vec<usize>,
    target: Option<usize>,
    mesh: Option<usize>,
    matrix: Option<[f64; 16]>,
    material: Option<MaterialData>,
}

/// A JT file being built up node by node.
///
/// The builder keeps the whole scene graph in memory and turns it into bytes on
/// demand, so [`JtBuilder::write_to_bytes`] can run more than once and a graph
/// can still be extended after a write.
///
/// ```no_run
/// use ferrojt::jt::{BuildNode, JtBuilder, MeshData};
///
/// let mut build = JtBuilder::new();
/// let part = build.add_node(build.root(), BuildNode::Part, "cube")?;
/// let cube = MeshData::new(vec![0.0; 12], vec![0, 1, 2, 0, 2, 3])?;
/// build.add_shape(part, "cube", cube)?;
/// let first = build.instance_of(build.root(), part)?;
/// build.set_translation(first, [10.0, 0.0, 0.0])?;
/// build.write_to_file("out.jt")?;
/// # Ok::<(), ferrojt::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct JtBuilder {
    nodes: Vec<Slot>,
    meshes: Vec<MeshData>,
    root: JtNode,
    compress: bool,
}

impl Default for JtBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl JtBuilder {
    /// Start a file that holds only its root partition.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: vec![Slot {
                class: BuildNode::Partition,
                name: None,
                children: Vec::new(),
                target: None,
                mesh: None,
                matrix: None,
                material: None,
            }],
            meshes: Vec::new(),
            root: JtNode(0),
            compress: true,
        }
    }

    /// The root partition of the graph.
    #[must_use]
    pub fn root(&self) -> JtNode {
        self.root
    }

    /// The number of nodes added so far.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The number of meshes added so far.
    #[must_use]
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Decide whether the scene graph segment is DEFLATE compressed.
    #[must_use]
    pub fn set_compress(&mut self, compress: bool) -> &mut Self {
        self.compress = compress;
        self
    }

    /// Whether the scene graph segment is DEFLATE compressed.
    #[must_use]
    pub fn compress(&self) -> bool {
        self.compress
    }

    /// Add a node under `parent` and return its handle.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] is returned when `parent` is not a node of this
    /// builder or when it is a `Shape` or `Instance`, which have no children.
    pub fn add_node<'a>(
        &mut self,
        parent: JtNode,
        class: BuildNode,
        name: impl Into<Option<&'a str>>,
    ) -> Result<JtNode> {
        self.check_node(parent)?;
        if !self.nodes[parent.0].class.accepts_children() {
            return Err(Error::invalid(
                0,
                format!(
                    "a {:?} node cannot have children",
                    self.nodes[parent.0].class
                ),
            ));
        }
        let pos = self.nodes.len();
        self.nodes.push(Slot {
            class,
            name: name.into().map(str::to_owned),
            children: Vec::new(),
            target: None,
            mesh: None,
            matrix: None,
            material: None,
        });
        self.nodes[parent.0].children.push(pos);
        Ok(JtNode(pos))
    }

    /// Add a shape node holding `mesh` under `parent`.
    ///
    /// # Errors
    ///
    /// See [`JtBuilder::add_node`] and [`MeshData::validate`].
    pub fn add_shape<'a>(
        &mut self,
        parent: JtNode,
        name: impl Into<Option<&'a str>>,
        mesh: MeshData,
    ) -> Result<JtNode> {
        mesh.validate()?;
        let shape = self.add_node(parent, BuildNode::Shape, name)?;
        self.meshes.push(mesh);
        self.nodes[shape.0].mesh = Some(self.meshes.len() - 1);
        Ok(shape)
    }

    /// Add an instance of `target` under `parent`.
    ///
    /// An instance shares the subtree of its target instead of copying it, so a
    /// mesh reached through two instances stays one representation in the file.
    ///
    /// # Errors
    ///
    /// See [`JtBuilder::add_node`]. [`Error::Invalid`] is also returned when
    /// `target` is the root partition, which cannot be instanced.
    pub fn instance_of(&mut self, parent: JtNode, target: JtNode) -> Result<JtNode> {
        self.check_node(target)?;
        if target == self.root {
            return Err(Error::invalid(0, "the root partition cannot be instanced"));
        }
        let instance = self.add_node(parent, BuildNode::Instance, None)?;
        self.nodes[instance.0].target = Some(target.0);
        Ok(instance)
    }

    /// Add a mesh to the file without attaching it to a node.
    #[must_use]
    pub fn add_mesh(&mut self, mesh: MeshData) -> JtMesh {
        self.meshes.push(mesh);
        JtMesh(self.meshes.len() - 1)
    }

    /// The mesh a handle refers to.
    #[must_use]
    pub fn mesh(&self, mesh: JtMesh) -> Option<&MeshData> {
        self.meshes.get(mesh.0)
    }

    /// Attach an existing mesh to a shape node.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] is returned when `node` is not a shape of this
    /// builder or already holds a mesh.
    pub fn set_mesh(&mut self, node: JtNode, mesh: JtMesh) -> Result<&mut Self> {
        self.check_node(node)?;
        self.check_mesh(mesh)?;
        if self.nodes[node.0].class != BuildNode::Shape {
            return Err(Error::invalid(0, "only a shape node can hold a mesh"));
        }
        if self.nodes[node.0].mesh.is_some() {
            return Err(Error::invalid(0, "that shape already holds a mesh"));
        }
        self.nodes[node.0].mesh = Some(mesh.0);
        Ok(self)
    }

    /// Set the transform matrix of a node, column major.
    ///
    /// Element (row, column) is at `4 * column + row` and the translation is
    /// the last column, which is how JT stores a matrix.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] is returned when `node` is not a node of this
    /// builder.
    pub fn set_transform(&mut self, node: JtNode, matrix: &[f64; 16]) -> Result<&mut Self> {
        self.check_node(node)?;
        self.nodes[node.0].matrix = Some(*matrix);
        Ok(self)
    }

    /// Set a translation only.
    ///
    /// # Errors
    ///
    /// See [`JtBuilder::set_transform`].
    pub fn set_translation(&mut self, node: JtNode, translation: [f32; 3]) -> Result<&mut Self> {
        self.set_transform(node, &translation_matrix(translation))
    }

    /// Set a translation, a rotation quaternion `(x, y, z, w)` and a scale.
    ///
    /// # Errors
    ///
    /// See [`JtBuilder::set_transform`].
    pub fn set_placement(
        &mut self,
        node: JtNode,
        translation: [f32; 3],
        rotation: [f32; 4],
        scale: [f32; 3],
    ) -> Result<&mut Self> {
        self.set_transform(node, &placement_matrix(translation, rotation, scale))
    }

    /// Set the transform of a node from the neutral scene graph type.
    ///
    /// # Errors
    ///
    /// See [`JtBuilder::set_transform`].
    pub fn set_node_transform(&mut self, node: JtNode, transform: &Transform) -> Result<&mut Self> {
        self.set_placement(
            node,
            transform.translation,
            transform.rotation,
            transform.scale,
        )
    }

    /// Set the material of a node.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] is returned when `node` is not a node of this
    /// builder.
    pub fn set_material(&mut self, node: JtNode, material: MaterialData) -> Result<&mut Self> {
        self.check_node(node)?;
        self.nodes[node.0].material = Some(material);
        Ok(self)
    }

    /// Set the name of a node.
    ///
    /// # Errors
    ///
    /// See [`JtBuilder::set_transform`].
    pub fn set_name(&mut self, node: JtNode, name: &str) -> Result<&mut Self> {
        self.check_node(node)?;
        self.nodes[node.0].name = Some(name.to_owned());
        Ok(self)
    }

    /// The children of a node, in the order they were added.
    #[must_use]
    pub fn children(&self, node: JtNode) -> Vec<JtNode> {
        self.nodes
            .get(node.0)
            .map(|slot| slot.children.iter().copied().map(JtNode).collect())
            .unwrap_or_default()
    }

    /// The class of a node.
    #[must_use]
    pub fn class(&self, node: JtNode) -> Option<BuildNode> {
        self.nodes.get(node.0).map(|slot| slot.class)
    }

    /// The name of a node.
    #[must_use]
    pub fn name(&self, node: JtNode) -> Option<&str> {
        self.nodes.get(node.0).and_then(|slot| slot.name.as_deref())
    }

    /// The node an instance refers to.
    #[must_use]
    pub fn instance_target(&self, node: JtNode) -> Option<JtNode> {
        self.nodes
            .get(node.0)
            .and_then(|slot| slot.target)
            .map(JtNode)
    }

    /// Write the file to disk.
    ///
    /// # Errors
    ///
    /// See [`JtBuilder::write_to_bytes`]; a failure of the target file is
    /// reported as [`Error::Io`].
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = self.write_to_bytes()?;
        std::fs::write(path.as_ref(), bytes)?;
        Ok(())
    }

    /// Turn the graph into the bytes of a JT file.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] is returned when a shape node has no mesh, when an
    /// instance has no target, or when the graph is not a tree — a node that is
    /// its own ancestor, or a node that is not reachable from the root.
    pub fn write_to_bytes(&self) -> Result<Vec<u8>> {
        let plan = self.plan()?;
        let lsg_guid = self.segment_guid(0);
        let body = self.lsg_body(&plan)?;
        // The scene graph segment carries a compression header, so its payload
        // has to be the DEFLATE stream itself when the flag says so.
        let payload = if self.compress { deflate(&body)? } else { body };

        let mut segments: Vec<Vec<u8>> = Vec::with_capacity(plan.shapes.len() + 1);
        segments.push(segment_bytes(ORDER, &lsg_guid, 1, &payload, self.compress)?);
        for (shape, element) in plan.shapes.iter().zip(&plan.representations) {
            segments.push(shape_segment(shape.guid, element));
        }

        let sizes: usize = segments.iter().map(|bytes| bytes.len()).sum();
        let mut out: Vec<u8> =
            Vec::with_capacity(header_length(MAJOR) + sizes + 4 + 28 * segments.len());
        write_banner(&mut out, header_length(MAJOR) + sizes, &lsg_guid)?;

        let mut offset = out.len();
        let mut toc: Vec<TocEntry> = Vec::with_capacity(segments.len());
        for (pos, bytes) in segments.iter().enumerate() {
            toc.push(TocEntry {
                guid: self.segment_guid(pos),
                offset: offset as u64,
                length: bytes.len() as u32,
                kind: if pos == 0 { 1 } else { 7 },
            });
            offset += bytes.len();
            out.extend_from_slice(bytes);
        }
        write_toc(&mut out, &toc)?;
        Ok(out)
    }

    /// Reject a node handle that is not part of this builder.
    fn check_node(&self, node: JtNode) -> Result<()> {
        if node.0 >= self.nodes.len() {
            return Err(Error::invalid(
                0,
                format!("node {} is not part of this file", node.0),
            ));
        }
        Ok(())
    }

    /// Reject a mesh handle that is not part of this builder.
    fn check_mesh(&self, mesh: JtMesh) -> Result<()> {
        if mesh.0 >= self.meshes.len() {
            return Err(Error::invalid(
                0,
                format!("mesh {} is not part of this file", mesh.0),
            ));
        }
        Ok(())
    }

    /// The GUID of the `pos`th segment of the file.
    ///
    /// The bytes are a marker byte and a counter in an otherwise empty GUID, so
    /// they cannot collide with a class GUID or with the end-of-element marker.
    fn segment_guid(&self, pos: usize) -> Guid {
        let mut bytes = [0u8; 16];
        bytes[8] = 0xfe;
        bytes[12..].copy_from_slice(&(pos as u32).to_be_bytes());
        Guid::from_bytes(bytes)
    }

    /// Order the graph for writing and number every object.
    fn plan(&self) -> Result<Plan> {
        let mut order: Vec<usize> = Vec::with_capacity(self.nodes.len());
        let mut visited = vec![false; self.nodes.len()];
        let mut on_path = vec![false; self.nodes.len()];
        self.walk(self.root.0, &mut order, &mut visited, &mut on_path)?;
        if order.len() != self.nodes.len() {
            let orphans: Vec<String> = (0..self.nodes.len())
                .filter(|pos| !visited[*pos])
                .map(|pos| {
                    self.nodes[pos]
                        .name
                        .clone()
                        .unwrap_or_else(|| pos.to_string())
                })
                .collect();
            return Err(Error::invalid(
                0,
                format!(
                    "the nodes {} are not reachable from the root partition",
                    orphans.join(", ")
                ),
            ));
        }

        // Nodes first, then the attributes they refer to — one id table for
        // both, exactly as the scene graph reader expects.
        let mut ids = vec![0i32; self.nodes.len()];
        for (pos, node) in order.iter().enumerate() {
            ids[*node] = pos as i32;
        }

        let mut attributes: Vec<Attribute> = Vec::new();
        let mut attribute_of = vec![Vec::new(); self.nodes.len()];
        for node in &order {
            let slot = &self.nodes[*node];
            if let Some(matrix) = slot.matrix {
                let id = (order.len() + attributes.len()) as i32;
                attributes.push(Attribute {
                    guid: types::ATTRIBUTE_GEOMETRIC_TRANSFORM,
                    id,
                    body: transform_body(&matrix)?,
                });
                attribute_of[*node].push(id);
            }
            if let Some(material) = slot.material {
                let id = (order.len() + attributes.len()) as i32;
                attributes.push(Attribute {
                    guid: types::ATTRIBUTE_MATERIAL,
                    id,
                    body: material_body(&material)?,
                });
                attribute_of[*node].push(id);
            }
        }

        let mut shapes: Vec<ShapeSlot> = Vec::new();
        for node in &order {
            let slot = &self.nodes[*node];
            match slot.class {
                BuildNode::Shape => {
                    let Some(mesh) = slot.mesh else {
                        return Err(Error::invalid(
                            0,
                            format!(
                                "the shape node {} has no mesh",
                                slot.name.clone().unwrap_or_else(|| node.to_string())
                            ),
                        ));
                    };
                    shapes.push(ShapeSlot {
                        node: *node,
                        mesh,
                        guid: self.segment_guid(1 + shapes.len()),
                    });
                }
                BuildNode::Instance if slot.target.is_none() => {
                    return Err(Error::invalid(0, "an instance node has no target"));
                }
                _ => {}
            }
        }

        let representations = shapes
            .iter()
            .map(|shape| representation(&self.meshes[shape.mesh]))
            .collect::<Result<Vec<_>>>()?;

        Ok(Plan {
            order,
            ids,
            attributes,
            attribute_of,
            shapes,
            representations,
        })
    }

    /// Depth first walk that writes a parent before its children.
    fn walk(
        &self,
        pos: usize,
        order: &mut Vec<usize>,
        visited: &mut [bool],
        on_path: &mut [bool],
    ) -> Result<()> {
        if on_path[pos] {
            return Err(Error::invalid(
                0,
                "the scene graph contains a cycle, so it cannot be written",
            ));
        }
        on_path[pos] = true;
        order.push(pos);
        visited[pos] = true;
        for child in &self.nodes[pos].children {
            if !visited[*child] {
                self.walk(*child, order, visited, on_path)?;
            }
        }
        on_path[pos] = false;
        Ok(())
    }

    /// The body of the scene graph segment: atoms, properties and the table.
    fn lsg_body(&self, plan: &Plan) -> Result<Vec<u8>> {
        // A property id lives in the same table as a node or attribute id, and
        // the table ends a row with the key `0`. Properties therefore continue
        // the numbering of the nodes and attributes instead of starting at 0 —
        // a key of 0 would end the row before it began.
        let first = (plan.order.len() + plan.attributes.len()) as i32;
        let mut next = first;
        let mut properties: Vec<Property> = Vec::new();
        let name_key = add_string_property(&mut properties, &mut next, NAME_KEY);
        let shape_key = add_string_property(&mut properties, &mut next, SHAPE_KEY);
        let mut rows: Vec<Row> = Vec::new();

        for node in &plan.order {
            let slot = &self.nodes[*node];
            let mut pairs = Vec::new();
            if let Some(name) = &slot.name {
                let value = add_string_property(&mut properties, &mut next, name);
                pairs.push((name_key, value));
            }
            if slot.class == BuildNode::Shape {
                let shape = plan
                    .shapes
                    .iter()
                    .find(|shape| shape.node == *node)
                    .ok_or_else(|| Error::invalid(0, "internal: a shape lost its segment"))?;
                let value = next;
                next += 1;
                properties.push(Property {
                    guid: types::PROPERTY_LATE_LOADED,
                    base: BASE_PROPERTY_LATE_LOADED,
                    id: value,
                    body: late_load_body(&shape.guid, 7),
                });
                pairs.push((shape_key, value));
            }
            if !pairs.is_empty() {
                rows.push(Row {
                    object_id: plan.ids[*node],
                    pairs,
                });
            }
        }

        let mut body: Vec<u8> = Vec::new();
        for node in &plan.order {
            let slot = &self.nodes[*node];
            let guid = match slot.class {
                BuildNode::Partition => types::NODE_PARTITION,
                BuildNode::Part => types::NODE_PART,
                BuildNode::MetaData => types::NODE_META_DATA,
                BuildNode::Group => types::NODE_GROUP,
                BuildNode::Shape => types::NODE_SHAPE_TRI_STRIP_SET,
                BuildNode::Instance => types::NODE_INSTANCE,
            };
            let base = match slot.class {
                BuildNode::Instance => BASE_INSTANCE,
                BuildNode::Shape => BASE_SHAPE,
                _ => BASE_GROUP,
            };
            let children: Vec<i32> = slot.children.iter().map(|c| plan.ids[*c]).collect();
            let target = slot.target.map(|t| plan.ids[t]).unwrap_or(-1);
            let bounds = self.node_bounds(*node, plan);
            let node_body = node_body(
                slot.class,
                &plan.attribute_of[*node],
                &children,
                target,
                &bounds,
            )?;
            write_atom(&mut body, ORDER, &guid, base, plan.ids[*node], &node_body)?;
            for id in &plan.attribute_of[*node] {
                let attribute = plan
                    .attributes
                    .iter()
                    .find(|attribute| attribute.id == *id)
                    .ok_or_else(|| Error::invalid(0, "internal: an attribute was lost"))?;
                write_atom(
                    &mut body,
                    ORDER,
                    &attribute.guid,
                    BASE_ATTRIBUTE,
                    attribute.id,
                    &attribute.body,
                )?;
            }
        }
        write_end_of_elements(&mut body, ORDER)?;

        for property in &properties {
            write_atom(
                &mut body,
                ORDER,
                &property.guid,
                property.base,
                property.id,
                &property.body,
            )?;
        }
        write_end_of_elements(&mut body, ORDER)?;

        body.write_i16_value(1, ORDER)?;
        body.write_i32_value(rows.len() as i32, ORDER)?;
        for row in &rows {
            body.write_i32_value(row.object_id, ORDER)?;
            for (key, value) in &row.pairs {
                body.write_i32_value(*key, ORDER)?;
                body.write_i32_value(*value, ORDER)?;
            }
            body.write_i32_value(0, ORDER)?;
        }
        Ok(body)
    }

    /// The box a partition or shape node stores.
    fn node_bounds(&self, pos: usize, plan: &Plan) -> [f32; 6] {
        let slot = &self.nodes[pos];
        if slot.class == BuildNode::Shape {
            return slot
                .mesh
                .and_then(|mesh| self.meshes[mesh].bounds())
                .unwrap_or([0.0; 6]);
        }
        if slot.class != BuildNode::Partition {
            return [0.0; 6];
        }
        // A partition stores the box of everything below it. The reader keeps
        // it and no export reads it, but a real box is cheap to provide.
        let mut all = [0.0f32; 6];
        let mut found = false;
        for shape in &plan.shapes {
            let Some(box_) = self.meshes[shape.mesh].bounds() else {
                continue;
            };
            if !found {
                all = box_;
                found = true;
                continue;
            }
            for axis in 0..3 {
                all[axis] = all[axis].min(box_[axis]);
                all[3 + axis] = all[3 + axis].max(box_[3 + axis]);
            }
        }
        all
    }
}

/// Frame one representation as the body of a shape segment.
fn shape_segment(guid: Guid, element: &[u8]) -> Vec<u8> {
    let mut segment: Vec<u8> = Vec::new();
    let total = PLAIN_SEGMENT_ENVELOPE + element.len() + 20;
    write_guid(&mut segment, ORDER, &guid).expect("writing to a vector cannot fail");
    segment
        .write_i32_value(7, ORDER)
        .expect("writing to a vector cannot fail");
    segment
        .write_i32_value(total as i32, ORDER)
        .expect("writing to a vector cannot fail");
    segment.extend_from_slice(element);
    write_end_of_elements(&mut segment, ORDER).expect("writing to a vector cannot fail");
    segment
}

/// Add a string property and return its id, taken from the running object id.
fn add_string_property(properties: &mut Vec<Property>, next: &mut i32, value: &str) -> i32 {
    let id = *next;
    *next += 1;
    let mut body: Vec<u8> = Vec::new();
    body.write_u32_value(PROPERTY_STATE, ORDER)
        .expect("writing to a vector cannot fail");
    write_mb_string(&mut body, ORDER, value).expect("writing to a vector cannot fail");
    properties.push(Property {
        guid: types::PROPERTY_STRING,
        base: BASE_PROPERTY_STRING,
        id,
        body,
    });
    id
}

/// An attribute the plan has serialised.
#[derive(Clone, Debug)]
struct Attribute {
    guid: Guid,
    id: i32,
    body: Vec<u8>,
}

/// A property atom of the scene graph.
#[derive(Clone, Debug)]
struct Property {
    guid: Guid,
    base: u8,
    id: i32,
    body: Vec<u8>,
}

/// One row of the property table.
#[derive(Clone, Debug)]
struct Row {
    object_id: i32,
    pairs: Vec<(i32, i32)>,
}

/// A shape node together with the segment that holds its triangles.
#[derive(Clone, Debug)]
struct ShapeSlot {
    node: usize,
    mesh: usize,
    guid: Guid,
}

/// The graph of a builder, ordered and numbered.
#[derive(Clone, Debug)]
struct Plan {
    order: Vec<usize>,
    ids: Vec<i32>,
    attributes: Vec<Attribute>,
    attribute_of: Vec<Vec<i32>>,
    shapes: Vec<ShapeSlot>,
    representations: Vec<Vec<u8>>,
}

/// Write the banner and the fixed fields of a JT 8.1 file.
fn write_banner(out: &mut Vec<u8>, toc_offset: usize, lsg: &Guid) -> Result<()> {
    let text = format!(
        "Version {MAJOR}.{MINOR} JT  FerroJT {}",
        env!("CARGO_PKG_VERSION")
    );
    let mut banner = [b' '; 80];
    let bytes = text.as_bytes();
    banner[..bytes.len()].copy_from_slice(bytes);
    banner[78] = b'\n';
    banner[79] = b'\r';
    out.write_bytes(&banner)?;
    // Byte 80 is the byte order flag, 0 for little endian, and JT 8.x keeps the
    // TOC offset at 32 bits.
    out.write_u8_value(0)?;
    out.write_i32_value(0, ORDER)?;
    out.write_i32_value(toc_offset as i32, ORDER)?;
    write_guid(out, ORDER, lsg)?;
    if out.len() != header_length(MAJOR) {
        return Err(Error::invalid(0, "internal: the fixed header is malformed"));
    }
    Ok(())
}

/// Write the TOC table at the end of the file.
fn write_toc(out: &mut Vec<u8>, toc: &[TocEntry]) -> Result<()> {
    out.write_i32_value(toc.len() as i32, ORDER)?;
    for entry in toc {
        write_guid(out, ORDER, &entry.guid)?;
        out.write_i32_value(entry.offset as i32, ORDER)?;
        out.write_u32_value(entry.length, ORDER)?;
        out.write_u32_value((entry.kind as u32) << 24, ORDER)?;
    }
    Ok(())
}

/// Serialise the body of one node, with its references already renumbered.
fn node_body(
    class: BuildNode,
    attributes: &[i32],
    children: &[i32],
    target: i32,
    bounds: &[f32; 6],
) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    // JT 8.x stores no class version in a node base, only its flags and the
    // attributes it refers to.
    out.write_u32_value(0, ORDER)?;
    write_deferred(&mut out, ORDER, attributes)?;
    match class {
        BuildNode::Partition => {
            write_deferred(&mut out, ORDER, children)?;
            // No "untransformed box" flag, so the one box below is the last
            // field the reader stops at.
            out.write_i32_value(0, ORDER)?;
            write_mb_string(&mut out, ORDER, "")?;
            write_box(&mut out, ORDER, bounds)?;
            out.write_f32_value(0.0, ORDER)?;
            write_counts(&mut out, ORDER)?;
        }
        BuildNode::Group => {
            write_deferred(&mut out, ORDER, children)?;
        }
        // A shape and an instance have no child list at all: their reader goes
        // straight from the attribute vector to the fields below.
        BuildNode::Shape | BuildNode::Instance => {}
        BuildNode::MetaData => {
            write_deferred(&mut out, ORDER, children)?;
            write_version(&mut out, ORDER, MAJOR, 1)?;
        }
        BuildNode::Part => {
            write_deferred(&mut out, ORDER, children)?;
            write_version(&mut out, ORDER, MAJOR, 1)?;
            write_version(&mut out, ORDER, MAJOR, 1)?;
            out.write_i32_value(0, ORDER)?;
        }
    }
    match class {
        BuildNode::Shape => {
            // JT 8.x stores a reserved box and then the untransformed one.
            write_box(&mut out, ORDER, &[0.0; 6])?;
            write_box(&mut out, ORDER, bounds)?;
            out.write_f32_value(0.0, ORDER)?;
            write_counts(&mut out, ORDER)?;
            out.write_i32_value(0, ORDER)?;
            out.write_f32_value(0.0, ORDER)?;
        }
        BuildNode::Instance => {
            // One reference, not a length prefixed vector.
            out.write_i32_value(target, ORDER)?;
        }
        _ => {}
    }
    Ok(out)
}

/// Serialise the body of a geometric transform attribute.
fn transform_body(matrix: &[f64; 16]) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    write_attribute_base(&mut out, ORDER, MAJOR, 1)?;
    out.write_u16_value(0xffff, ORDER)?;
    // JT 8.x stores single precision values.
    for value in matrix {
        out.write_f32_value(*value as f32, ORDER)?;
    }
    Ok(out)
}

/// Serialise the body of a material attribute.
fn material_body(material: &MaterialData) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    write_attribute_base(&mut out, ORDER, MAJOR, 1)?;
    // Flag 0 stores every colour as four components.
    out.write_u16_value(0, ORDER)?;
    for colour in [
        material.ambient,
        material.diffuse,
        material.specular,
        material.emission,
    ] {
        for component in colour {
            out.write_f32_value(component, ORDER)?;
        }
    }
    out.write_f32_value(material.shininess, ORDER)?;
    Ok(out)
}

/// The body of a late loaded property: state, segment GUID and segment type.
fn late_load_body(segment: &Guid, kind: i32) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.write_u32_value(PROPERTY_STATE, ORDER)
        .expect("writing to a vector cannot fail");
    write_guid(&mut out, ORDER, segment).expect("writing to a vector cannot fail");
    out.write_i32_value(kind, ORDER)
        .expect("writing to a vector cannot fail");
    out
}

/// Serialise the element atom of one representation, envelope included.
///
/// A representation segment numbers no objects before JT 9.x, so the atom
/// carries no object id and its length counts only the GUID and the base type
/// byte.
fn representation(mesh: &MeshData) -> Result<Vec<u8>> {
    let strips = mesh.indices().len() / 3;
    let with_normals = !mesh.normals().is_empty();

    // A JT 8.x representation stores every triangle as its own three slot
    // strip, and the reader walks a strip alternating between the two windings
    // — `0,1,2` then `3,5,4` then `6,7,8`. The slot number *is* the index the
    // reader emits, so the slots it walks over are permuted: `emit` is that
    // permutation, and `written` inverts it to say which corner of the source
    // mesh belongs in which slot of the file. A mesh that shares its corners
    // between triangles is therefore written exploded, one slot per corner, so
    // that the geometry comes back unchanged even though the index list is
    // renumbered by the strip walk.
    let emit: Vec<usize> = (0..strips)
        .flat_map(|strip| {
            let base = strip * 3;
            if strip % 2 == 0 {
                [base, base + 1, base + 2]
            } else {
                [base, base + 2, base + 1]
            }
        })
        .collect();
    let mut written: Vec<usize> = vec![0; emit.len()];
    for (corner, slot) in mesh.indices().iter().enumerate() {
        let Ok(at) = usize::try_from(*slot) else {
            continue;
        };
        written[emit[corner]] = at;
    }

    let mut body: Vec<u8> = Vec::new();
    // Shape LOD base: the class version, the 32 bit vertex binding of JT 8.x
    // and the quantization parameters of a lossless shape.
    write_version(&mut body, ORDER, MAJOR, 1)?;
    body.write_u32_value(1, ORDER)?;
    for value in [0u8, 0, 0, 0] {
        body.write_u8_value(value)?;
    }
    // The trailing version of the tri-strip-set element.
    write_version(&mut body, ORDER, MAJOR, 1)?;
    // The vertex based representation: its version, the per-slot bindings and
    // the parameters saying the geometry is stored lossless.
    write_version(&mut body, ORDER, MAJOR, 1)?;
    body.write_u8_value(u8::from(with_normals))?;
    body.write_u8_value(0)?;
    body.write_u8_value(0)?;
    for value in [0u8, 0, 0, 0] {
        body.write_u8_value(value)?;
    }
    // The strip offsets, as a null codec plus the residuals the stride-1
    // predictor of the reader needs to reproduce them.
    let offsets: Vec<i32> = (0..=strips).map(|strip| (strip * 3) as i32).collect();
    let residuals = stride_residuals(&offsets);
    body.write_u8_value(0)?;
    body.write_i32_value(residuals.len() as i32, ORDER)?;
    for value in &residuals {
        body.write_u32_value(*value as u32, ORDER)?;
    }
    // The vertex block: the unpacked length, the packed length, then the
    // normals and positions interleaved one slot at a time.
    let mut unpacked: Vec<u8> =
        Vec::with_capacity(written.len() * if with_normals { 24 } else { 12 });
    for slot in &written {
        let at = slot * 3;
        if with_normals {
            for axis in 0..3 {
                unpacked.write_f32_value(mesh.normals()[at + axis], ORDER)?;
            }
        }
        for axis in 0..3 {
            unpacked.write_f32_value(mesh.vertices()[at + axis], ORDER)?;
        }
    }
    let packed = deflate(&unpacked)?;
    body.write_i32_value(unpacked.len() as i32, ORDER)?;
    body.write_i32_value(packed.len() as i32, ORDER)?;
    body.write_bytes(&packed)?;

    let mut out: Vec<u8> = Vec::new();
    out.write_i32_value((ELEMENT_ENVELOPE + body.len()) as i32, ORDER)?;
    write_guid(&mut out, ORDER, &types::SHAPE_LOD_TRI_STRIP_SET)?;
    out.write_u8_value(BASE_ELEMENT)?;
    out.write_bytes(&body)?;
    Ok(out)
}

/// The residuals a stride-1 predictor needs to reproduce `values`.
fn stride_residuals(values: &[i32]) -> Vec<i32> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if index < 4 {
                *value
            } else {
                let v1 = values[index - 1];
                let v2 = values[index - 2];
                value.wrapping_sub(v1.wrapping_add(v1.wrapping_sub(v2)))
            }
        })
        .collect()
}

/// The column major matrix of a translation.
fn translation_matrix(translation: [f32; 3]) -> [f64; 16] {
    placement_matrix(translation, [0.0, 0.0, 0.0, 1.0], [1.0; 3])
}

/// The column major matrix of a translation, a quaternion `(x, y, z, w)` and a scale.
fn placement_matrix(translation: [f32; 3], rotation: [f32; 4], scale: [f32; 3]) -> [f64; 16] {
    let [x, y, z, w] = rotation;
    let length = (x * x + y * y + z * z + w * w).sqrt();
    let (x, y, z, w) = if length > 0.0 {
        (x / length, y / length, z / length, w / length)
    } else {
        (0.0, 0.0, 0.0, 1.0)
    };
    let rotation = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - w * z),
        2.0 * (x * z + w * y),
        2.0 * (x * y + w * z),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - w * x),
        2.0 * (x * z - w * y),
        2.0 * (y * z + w * x),
        1.0 - 2.0 * (x * x + y * y),
    ];
    // Column major: element (row, column) is at 4 * column + row, and the
    // translation is the last column.
    let mut matrix = [0.0f64; 16];
    for column in 0..3 {
        for row in 0..3 {
            matrix[4 * column + row] =
                f64::from(rotation[3 * row + column]) * f64::from(scale[column]);
        }
    }
    matrix[12] = f64::from(translation[0]);
    matrix[13] = f64::from(translation[1]);
    matrix[14] = f64::from(translation[2]);
    matrix[15] = 1.0;
    matrix
}
