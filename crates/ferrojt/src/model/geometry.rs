//! Geometry: meshes, primitives, vertex attributes and the raw data they point at.

use serde::{Deserialize, Serialize};

use super::Aabb;

/// Component type of a scalar element in an attribute buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentType {
    /// Signed 8 bit integer.
    I8,
    /// Unsigned 8 bit integer.
    U8,
    /// Signed 16 bit integer.
    I16,
    /// Unsigned 16 bit integer.
    U16,
    /// Signed 32 bit integer.
    I32,
    /// Unsigned 32 bit integer.
    U32,
    /// Signed 64 bit integer (not expressible in glTF; widened on export).
    I64,
    /// Unsigned 64 bit integer (not expressible in glTF; widened on export).
    U64,
    /// 32 bit float.
    F32,
    /// 16 bit IEEE half float.
    F16,
    /// 64 bit float.
    F64,
}

impl ComponentType {
    /// Size of one component in bytes.
    pub fn byte_len(self) -> usize {
        match self {
            ComponentType::I8 | ComponentType::U8 => 1,
            ComponentType::I16 | ComponentType::U16 | ComponentType::F16 => 2,
            ComponentType::I32 | ComponentType::U32 | ComponentType::F32 => 4,
            ComponentType::I64 | ComponentType::U64 | ComponentType::F64 => 8,
        }
    }
}

/// How vertices are interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Topology {
    /// Independent points.
    PointList,
    /// Independent line segments.
    LineList,
    /// A connected polyline.
    LineStrip,
    /// Independent triangles.
    #[default]
    TriangleList,
    /// A connected triangle strip.
    TriangleStrip,
    /// Independent triangles with an explicit adjacency list.
    TriangleFan,
}

/// A named array of fixed width components inside a [`BufferBlock`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attribute {
    /// Attribute name, e.g. `POSITION`, `NORMAL`, `COLOR_0`, `TEXCOORD_0`.
    pub name: String,
    /// Index of the [`BufferBlock`] holding the bytes.
    pub buffer: usize,
    /// Byte offset of the first element inside that block.
    pub offset: usize,
    /// Number of elements.
    pub count: usize,
    /// Components per element, 1..=16.
    pub components: u8,
    /// Component type.
    pub component_type: ComponentType,
    /// Whether the values are normalised integers in glTF terms.
    pub normalized: bool,
    /// Optional component wise minimum, used by exporters.
    pub min: Option<Vec<f64>>,
    /// Optional component wise maximum, used by exporters.
    pub max: Option<Vec<f64>>,
}

impl Attribute {
    /// Byte length of one element.
    pub fn element_byte_len(&self) -> usize {
        usize::from(self.components) * self.component_type.byte_len()
    }

    /// Byte length of the whole attribute.
    pub fn byte_len(&self) -> usize {
        self.count * self.element_byte_len()
    }
}

/// A contiguous chunk of decoded vertex / index data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferBlock {
    /// Raw little endian bytes.
    pub bytes: Vec<u8>,
}

impl BufferBlock {
    /// Wrap raw bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

/// One drawable thing: a set of attributes plus an optional index buffer.
///
/// Attributes are indices into [`Accessors::attributes`], which is owned by
/// [`Model::accessors`](super::Model::accessors).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Primitive {
    /// Vertex attribute indices.
    pub attributes: Vec<usize>,
    /// Optional index attribute index.
    pub indices: Option<usize>,
    /// How to interpret the vertices.
    pub topology: Topology,
    /// Optional index into [`Model::materials`](super::Model::materials).
    pub material: Option<usize>,
    /// Local space bounds of the primitive.
    pub bounds: Option<Aabb>,
    /// Instance / LOD metadata carried over from JT.
    pub extras: Option<serde_json::Value>,
}

/// A mesh is a collection of primitives sharing a local coordinate space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mesh {
    /// Stable identifier from the source file, when available.
    pub name: Option<String>,
    /// Primitives.
    pub primitives: Vec<Primitive>,
    /// Local space bounds.
    pub bounds: Option<Aabb>,
    /// LOD level of this mesh, `0` being the highest quality.
    pub lod_level: Option<u32>,
    /// Source specific metadata.
    pub extras: Option<serde_json::Value>,
}

/// Attribute blocks plus the buffers they live in, owned by the [`Model`](super::Model).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Accessors {
    /// Raw data blocks.
    pub buffers: Vec<BufferBlock>,
    /// Attribute table.
    pub attributes: Vec<Attribute>,
}

impl Accessors {
    /// Append a block and return its index.
    pub fn push_buffer(&mut self, bytes: Vec<u8>) -> usize {
        self.buffers.push(BufferBlock::new(bytes));
        self.buffers.len() - 1
    }

    /// Append an attribute and return its index.
    pub fn push_attribute(&mut self, attribute: Attribute) -> usize {
        self.attributes.push(attribute);
        self.attributes.len() - 1
    }

    /// Resolve an attribute index.
    pub fn attribute(&self, index: usize) -> Option<&Attribute> {
        self.attributes.get(index)
    }

    /// Resolve an attribute index together with the bytes it points at.
    pub fn attribute_bytes(&self, index: usize) -> Option<&[u8]> {
        let attribute = self.attributes.get(index)?;
        self.buffers
            .get(attribute.buffer)?
            .bytes
            .get(attribute.offset..attribute.offset + attribute.byte_len())
    }
}
