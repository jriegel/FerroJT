//! JT objects: the logical scene graph nodes, their attributes, their
//! properties and the representation elements referenced by late loads.
//!
//! Every element in a JT file starts with the same envelope — length, type
//! GUID, base type tag and object id — handled by
//! [`read_element_envelope`]. What follows is per class and is what the
//! `read` implementations below consume.

use crate::codec::ByteOrder;
use crate::error::{Error, Result};
use crate::jt::decode::{Decoded, MeshInput, Package, UnifiedMesh, Unpack, VertexData};
use crate::jt::guid::{Guid, types};
use crate::jt::reader::Cursor;

/// The 16 bytes a JT 10.x topologically compressed tri-strip representation is
/// prefixed with.
const TOPOLOGY_MARKER: [u8; 16] = [
    0x30, 0xf8, 0x4c, 0xbe, 0xbc, 0x4f, 0x9b, 0x5f, 0xb9, 0x26, 0x92, 0x78, 0xd2, 0xe1, 0x09, 0x01,
];

/// The `PerVertex` value of the JT 8.x vertex binding enumeration.
const NORMAL_BINDING_PER_VERTEX: u8 = 1;

/// An element reference: an id plus the GUID of the segment holding it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LateLoad {
    /// Object id of the property that carries the reference.
    pub property_id: i32,
    /// Segment GUID to load.
    pub guid: Guid,
    /// Segment type of that segment.
    pub segment_type: i32,
    /// Object id inside that segment.
    pub object_id: i32,
}

impl LateLoad {
    /// Build the reference a late-load property stands for.
    ///
    /// The reference stores the id of the property itself, not the id of the
    /// segment payload, so that loading it repeatedly stays cheap.
    #[must_use]
    pub fn from_property(property_id: i32, property: &LateLoaded) -> Self {
        LateLoad {
            property_id,
            guid: property.guid,
            segment_type: property.segment_type,
            object_id: property.object_id,
        }
    }
}

/// A deferred reference to an object in the same segment table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deferred {
    /// Object id as stored; resolved against the segment's object table.
    pub object_id: i32,
}

impl Deferred {
    /// Read one deferred reference.
    pub fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        Ok(Deferred {
            object_id: cursor.read_i32()?,
        })
    }

    /// Read a length-prefixed array of deferred references.
    pub fn read_vec(cursor: &mut Cursor<'_>) -> Result<Vec<Self>> {
        let count = cursor.read_len()?;
        (0..count).map(|_| Self::read(cursor)).collect()
    }
}

/// Fields shared by every node.
#[derive(Clone, Debug, Default)]
pub struct NodeBase {
    /// Element version number of the node class.
    pub version: u32,
    /// Node flags.
    pub flags: u32,
    /// Attribute references.
    pub attributes: Vec<Deferred>,
    /// Late-load references collected from the property table.
    pub late_loads: Vec<LateLoad>,
}

impl NodeBase {
    fn read(cursor: &mut Cursor<'_>, with_attributes: bool) -> Result<Self> {
        let major = cursor.major();
        let mut base = NodeBase::default();
        if major > 8 {
            base.version = cursor.read_version()?;
        }
        base.flags = cursor.read_u32()?;
        if with_attributes {
            base.attributes = Deferred::read_vec(cursor)?;
        }
        Ok(base)
    }
}

/// A logical scene graph node.
#[derive(Clone, Debug)]
pub enum Node {
    /// The single root of the scene graph.
    Partition(Partition),
    /// A part, optionally carrying layer information.
    Part(Part),
    /// A meta data node, carrying layer filters.
    MetaData(MetaData),
    /// A level-of-detail selector.
    Lod(Lod),
    /// A range based level-of-detail selector.
    RangeLod(RangeLod),
    /// A reference to another node.
    Instance(Instance),
    /// A plain group.
    Group(Group),
    /// A shape summary node.
    Shape(Shape),
    /// A shape holding a vertex based representation.
    ShapeVertex(ShapeVertex),
    /// A shape holding a topologically compressed tri-strip representation.
    ShapeTriStripSet(ShapeTriStripSet),
    /// A class the reader knows the GUID of but does not model.
    Unsupported {
        /// Class name from the class registry.
        class_name: &'static str,
    },
}

impl Node {
    /// Read the body of a node whose class is known from its GUID.
    pub(crate) fn read(guid: &Guid, cursor: &mut Cursor<'_>) -> Result<Self> {
        let node = if *guid == types::NODE_PARTITION {
            Node::Partition(Partition::read(cursor)?)
        } else if *guid == types::NODE_PART {
            Node::Part(Part::read(cursor)?)
        } else if *guid == types::NODE_META_DATA {
            Node::MetaData(MetaData::read(cursor)?)
        } else if *guid == types::NODE_LOD {
            Node::Lod(Lod::read(cursor)?)
        } else if *guid == types::NODE_RANGE_LOD {
            Node::RangeLod(RangeLod::read(cursor)?)
        } else if *guid == types::NODE_INSTANCE {
            Node::Instance(Instance::read(cursor)?)
        } else if *guid == types::NODE_GROUP {
            Node::Group(Group::read(cursor)?)
        } else if *guid == types::NODE_SHAPE_TRI_STRIP_SET {
            Node::ShapeTriStripSet(ShapeTriStripSet::read(cursor)?)
        } else if *guid == types::NODE_SHAPE_VERTEX {
            Node::ShapeVertex(ShapeVertex::read(cursor)?)
        } else if *guid == types::NODE_SHAPE_BASE {
            Node::Shape(Shape::read(cursor)?)
        } else if *guid == types::NODE_BASE {
            Node::Group(Group::read(cursor)?)
        } else {
            return Err(Error::invalid(
                cursor.position() as u64,
                "unknown node type",
            ));
        };
        Ok(node)
    }

    /// Class name used by the reference JSON writer.
    pub fn class_name(&self) -> &'static str {
        match self {
            Node::Partition(_) => "Partition",
            Node::Part(_) => "Part",
            Node::MetaData(_) => "MetaData",
            Node::Lod(_) => "LOD",
            Node::RangeLod(_) => "RangeLOD",
            Node::Instance(_) => "Instance",
            Node::Group(_) => "Group",
            Node::Shape(_) | Node::ShapeVertex(_) | Node::ShapeTriStripSet(_) => "Shape",
            Node::Unsupported { class_name } => class_name,
        }
    }

    /// Node base fields, for every node class.
    pub fn base(&self) -> &NodeBase {
        match self {
            Node::Partition(p) => &p.base.base,
            Node::Part(p) => &p.base.base.base,
            Node::MetaData(m) => &m.base.base,
            Node::Lod(l) => &l.base.base,
            Node::RangeLod(l) => &l.base.base.base,
            Node::Instance(i) => &i.base,
            Node::Group(g) => &g.base,
            Node::Shape(s) => &s.base,
            Node::ShapeVertex(s) => &s.base.base,
            Node::ShapeTriStripSet(s) => &s.base.base,
            Node::Unsupported { .. } => &EMPTY_BASE,
        }
    }

    /// Node base fields, mutably, or `None` for a class that is not modelled.
    fn base_mut(&mut self) -> Option<&mut NodeBase> {
        match self {
            Node::Unsupported { .. } => None,
            Node::Partition(p) => Some(&mut p.base.base),
            Node::Part(p) => Some(&mut p.base.base.base),
            Node::MetaData(m) => Some(&mut m.base.base),
            Node::Lod(l) => Some(&mut l.base.base),
            Node::RangeLod(l) => Some(&mut l.base.base.base),
            Node::Instance(i) => Some(&mut i.base),
            Node::Group(g) => Some(&mut g.base),
            Node::Shape(s) => Some(&mut s.base),
            Node::ShapeVertex(s) => Some(&mut s.base.base),
            Node::ShapeTriStripSet(s) => Some(&mut s.base.base),
        }
    }

    /// Attribute references declared by the node.
    pub fn attributes(&self) -> &[Deferred] {
        &self.base().attributes
    }

    /// True for the shape classes that carry a tri-strip representation. The
    /// reference writer treats every shape vertex node as a tri-strip set, so
    /// both classes and their base class answer yes.
    #[must_use]
    pub fn is_tri_strip_set(&self) -> bool {
        matches!(
            self,
            Node::ShapeTriStripSet(_) | Node::ShapeVertex(_) | Node::Shape(_)
        )
    }

    /// Child references declared by the node, empty for nodes without children.
    pub fn children(&self) -> &[Deferred] {
        match self {
            Node::Partition(p) => &p.children,
            Node::Part(p) => &p.base.base.children,
            Node::MetaData(m) => &m.base.children,
            Node::Lod(l) => &l.base.children,
            Node::RangeLod(l) => &l.base.base.children,
            Node::Group(g) => &g.children,
            _ => &NO_CHILDREN,
        }
    }

    /// The name decoded from the `JT_PROP_NAME` property, if any.
    pub fn name(&self) -> &str {
        match self {
            Node::Partition(p) => &p.name,
            Node::Part(p) => &p.name,
            Node::MetaData(m) => &m.name,
            Node::Lod(l) => &l.name,
            Node::RangeLod(l) => &l.name,
            Node::Instance(i) => &i.name,
            Node::Group(g) => &g.name,
            Node::Shape(s) => &s.name,
            Node::ShapeVertex(s) => &s.name,
            Node::ShapeTriStripSet(s) => &s.name,
            Node::Unsupported { .. } => "",
        }
    }

    /// Bind a name coming from the property table.
    ///
    /// Only the first `;`-separated token is kept, and leading separators are
    /// skipped, exactly as the reference does. A value made only of separators
    /// clears the name.
    pub fn bind_name(&mut self, name: &str) {
        let slot = match self {
            Node::Partition(p) => &mut p.name,
            Node::Part(p) => &mut p.name,
            Node::MetaData(m) => &mut m.name,
            Node::Lod(l) => &mut l.name,
            Node::RangeLod(l) => &mut l.name,
            Node::Instance(i) => &mut i.name,
            Node::Group(g) => &mut g.name,
            Node::Shape(s) => &mut s.name,
            Node::ShapeVertex(s) => &mut s.name,
            Node::ShapeTriStripSet(s) => &mut s.name,
            Node::Unsupported { .. } => return,
        };
        let start = name.find(|c: char| c != ';').unwrap_or(name.len());
        let end = name[start..].find(';').map_or(name.len(), |at| start + at);
        *slot = name[start..end].to_owned();
    }

    /// Local space bounds of a shape node, `None` for every other class.
    #[must_use]
    pub fn bounds(&self) -> Option<[f32; 6]> {
        match self {
            Node::Shape(s) => Some(s.bounds),
            Node::ShapeVertex(s) => Some(s.base.bounds),
            Node::ShapeTriStripSet(s) => Some(s.base.bounds),
            _ => None,
        }
    }

    /// Late-load references collected from the property table.
    pub fn late_loads(&self) -> &[LateLoad] {
        &self.base().late_loads
    }

    /// Late-load references collected from the property table, or `None` for a
    /// class that is not modelled.
    pub fn late_loads_mut(&mut self) -> Option<&mut Vec<LateLoad>> {
        self.base_mut().map(|base| &mut base.late_loads)
    }
}

static EMPTY_BASE: NodeBase = NodeBase {
    version: 0,
    flags: 0,
    attributes: Vec::new(),
    late_loads: Vec::new(),
};
static NO_CHILDREN: Vec<Deferred> = Vec::new();

/// The root of the logical scene graph.
#[derive(Clone, Debug)]
pub struct Partition {
    /// Group fields (node base plus child references).
    pub base: Group,
    /// File name from the segment header.
    pub file_name: String,
    /// Axis indicator, as stored.
    pub up_axis: i32,
    /// Name from the property table.
    pub name: String,
    /// Child references.
    pub children: Vec<Deferred>,
}

impl Partition {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = Group::read(cursor)?;
        if cursor.major() >= 10 {
            let _ = cursor.read_version()?;
        }
        let _flags = cursor.read_i32()?;
        let file_name = cursor.read_mb_string()?;
        let has_untransformed = (_flags & 1) != 0;
        read_bounds(cursor)?;
        let _area = cursor.read_f32()?;
        read_int_pairs(cursor)?;
        if has_untransformed && cursor.major() < 10 {
            read_bounds(cursor)?;
        }
        let children = base.children.clone();
        Ok(Partition {
            base,
            file_name,
            up_axis: 0,
            name: String::new(),
            children,
        })
    }
}

/// A part node.
#[derive(Clone, Debug, Default)]
pub struct Part {
    /// Meta data fields.
    pub base: MetaData,
    /// Name from the property table.
    pub name: String,
}

impl Part {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = MetaData::read(cursor)?;
        let _version = cursor.read_version()?;
        let _reserved = cursor.read_i32()?;
        Ok(Part {
            base,
            ..Part::default()
        })
    }
}

/// A meta data node.
#[derive(Clone, Debug, Default)]
pub struct MetaData {
    /// Group fields.
    pub base: Group,
    /// Name from the property table.
    pub name: String,
}

impl MetaData {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = Group::read(cursor)?;
        let _version = cursor.read_version()?;
        Ok(MetaData {
            base,
            ..MetaData::default()
        })
    }
}

/// A level-of-detail selector.
#[derive(Clone, Debug, Default)]
pub struct Lod {
    /// Group fields.
    pub base: Group,
    /// Percentage limits.
    pub limits: Vec<f32>,
    /// Signal value the limits are compared against.
    pub signal: f32,
    /// Name from the property table.
    pub name: String,
}

impl Lod {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = Group::read(cursor)?;
        let mut limits = Vec::new();
        let mut signal = 0.0f32;
        if cursor.major() > 8 {
            let _version = cursor.read_version()?;
        }
        if cursor.major() < 10 {
            limits = cursor.read_vec_f32()?;
            signal = cursor.read_f32()?;
        }
        Ok(Lod {
            base,
            limits,
            signal,
            name: String::new(),
        })
    }
}

/// A range based level-of-detail selector.
#[derive(Clone, Debug, Default)]
pub struct RangeLod {
    /// LOD fields.
    pub base: Lod,
    /// Range limits.
    pub limits: Vec<f32>,
    /// Centre of the range.
    pub center: [f32; 3],
    /// Name from the property table.
    pub name: String,
}

impl RangeLod {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = Lod::read(cursor)?;
        if cursor.major() > 8 {
            let _version = cursor.read_version()?;
        }
        let limits = cursor.read_vec_f32()?;
        let center = [cursor.read_f32()?, cursor.read_f32()?, cursor.read_f32()?];
        Ok(RangeLod {
            base,
            limits,
            center,
            name: String::new(),
        })
    }
}

/// A reference to another node.
#[derive(Clone, Debug, Default)]
pub struct Instance {
    /// Node base fields.
    pub base: NodeBase,
    /// Referenced node.
    pub target: Deferred,
    /// Name from the property table.
    pub name: String,
}

impl Instance {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = NodeBase::read(cursor, true)?;
        if cursor.major() > 8 {
            let _version = cursor.read_version()?;
        }
        Ok(Instance {
            base,
            target: Deferred::read(cursor)?,
            name: String::new(),
        })
    }
}

/// A plain group node.
#[derive(Clone, Debug, Default)]
pub struct Group {
    /// Node base fields.
    pub base: NodeBase,
    /// Child references.
    pub children: Vec<Deferred>,
    /// Name from the property table.
    pub name: String,
}

impl Group {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = NodeBase::read(cursor, true)?;
        if cursor.major() > 8 {
            let _version = cursor.read_version()?;
        }
        let children = Deferred::read_vec(cursor)?;
        Ok(Group {
            base,
            children,
            name: String::new(),
        })
    }
}

/// A shape summary node.
#[derive(Clone, Debug, Default)]
pub struct Shape {
    /// Node base fields.
    pub base: NodeBase,
    /// Axis aligned bounds.
    pub bounds: [f32; 6],
    /// Surface area.
    pub area: f32,
    /// Name from the property table.
    pub name: String,
}

impl Shape {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = NodeBase::read(cursor, true)?;
        if cursor.major() > 8 {
            let _version = cursor.read_version()?;
        }
        // JT 10.x stores a single (untransformed) box, earlier versions store
        // the reserved box first and the untransformed one second; the
        // reference hands out the untransformed box.
        let bounds = if cursor.major() >= 10 {
            read_bounds(cursor)?
        } else {
            let _reserved = read_bounds(cursor)?;
            read_bounds(cursor)?
        };
        let area = cursor.read_f32()?;
        read_int_pairs(cursor)?;
        let _size = cursor.read_i32()?;
        let _compression = cursor.read_f32()?;
        Ok(Shape {
            base,
            bounds,
            area,
            name: String::new(),
        })
    }
}

/// A shape node referencing a vertex based representation.
#[derive(Clone, Debug, Default)]
pub struct ShapeVertex {
    /// Shape fields.
    pub base: Shape,
    /// Name from the property table.
    pub name: String,
}

impl ShapeVertex {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = Shape::read(cursor)?;
        Ok(ShapeVertex {
            base,
            ..ShapeVertex::default()
        })
    }
}

/// A shape node referencing a topologically compressed tri-strip representation.
#[derive(Clone, Debug, Default)]
pub struct ShapeTriStripSet {
    /// Shape fields.
    pub base: Shape,
    /// Name from the property table.
    pub name: String,
}

impl ShapeTriStripSet {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let base = Shape::read(cursor)?;
        Ok(ShapeTriStripSet {
            base,
            ..ShapeTriStripSet::default()
        })
    }
}

/// Read the six `F32` of an axis aligned box.
fn read_bounds(cursor: &mut Cursor<'_>) -> Result<[f32; 6]> {
    let mut box_ = [0.0f32; 6];
    for value in &mut box_ {
        *value = cursor.read_f32()?;
    }
    Ok(box_)
}

/// Read the three `I32[2]` arrays that follow the bounds of a shape or
/// partition: the instance, LOD and triangle counts.
fn read_int_pairs(cursor: &mut Cursor<'_>) -> Result<()> {
    for _ in 0..6 {
        let _ = cursor.read_i32()?;
    }
    Ok(())
}

/// An attribute attached to a node.
#[derive(Clone, Debug)]
pub enum Attribute {
    /// A 4x4 transformation.
    GeometricTransform(GeometricTransform),
    /// A material.
    Material(Material),
    /// A class the reader knows the GUID of but does not model.
    Unsupported {
        /// Class name from the class registry.
        class_name: &'static str,
    },
}

impl Attribute {
    /// Read the body of an attribute whose class is known from its GUID.
    pub(crate) fn read(guid: &Guid, cursor: &mut Cursor<'_>) -> Result<Self> {
        let attribute = if *guid == types::ATTRIBUTE_GEOMETRIC_TRANSFORM {
            Attribute::GeometricTransform(GeometricTransform::read(cursor)?)
        } else if *guid == types::ATTRIBUTE_MATERIAL {
            Attribute::Material(Material::read(cursor)?)
        } else {
            return Err(Error::invalid(
                cursor.position() as u64,
                "unknown attribute type",
            ));
        };
        Ok(attribute)
    }

    /// Class name used by the reference JSON writer.
    pub fn class_name(&self) -> &'static str {
        match self {
            Attribute::GeometricTransform(_) => "GeometricTransform",
            Attribute::Material(_) => "Material",
            Attribute::Unsupported { class_name } => class_name,
        }
    }
}
/// Read the fields shared by every attribute.
fn read_attribute_base(cursor: &mut Cursor<'_>) -> Result<u32> {
    let major = cursor.major();
    let mut version = 0u32;
    if major > 8 {
        version = cursor.read_version()?;
    }
    let _state_flags = cursor.read_u8()?;
    let _field_inhibit = cursor.read_u32()?;
    if major >= 10 {
        let _field_final = cursor.read_u32()?;
    }
    Ok(version)
}

/// A 4x4 transformation matrix.
#[derive(Clone, Debug)]
pub struct GeometricTransform {
    /// Version of the attribute.
    pub version: u32,
    /// Row-major matrix; unset slots hold the identity.
    pub matrix: [f64; 16],
}

impl GeometricTransform {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let version = read_attribute_base(cursor)?;
        if cursor.major() > 8 {
            let _ = cursor.read_version()?;
        }
        let stored = cursor.read_u16()?;
        let wide = cursor.major() > 8;
        const IDENTITY: [f64; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let mut matrix = IDENTITY;
        let mut mask = 0x8000u16;
        for (iter, value) in matrix.iter_mut().enumerate() {
            if (mask & stored) != 0 {
                *value = if wide {
                    cursor.read_f64()?
                } else {
                    f64::from(cursor.read_f32()?)
                };
            }
            mask >>= 1;
            let _ = iter;
        }
        Ok(GeometricTransform { version, matrix })
    }
}

/// A material.
#[derive(Clone, Debug, Default)]
pub struct Material {
    /// Version of the attribute.
    pub version: u32,
    /// Ambient colour.
    pub ambient: [f32; 4],
    /// Diffuse colour.
    pub diffuse: [f32; 4],
    /// Specular colour.
    pub specular: [f32; 4],
    /// Emissive colour.
    pub emission: [f32; 4],
    /// Shininess exponent.
    pub shininess: f32,
    /// Reflectivity, present for JT 10.x and for version 2 of earlier files.
    pub reflectivity: f32,
}

impl Material {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let version = read_attribute_base(cursor)?;
        if cursor.major() > 8 {
            let _ = cursor.read_version()?;
        }
        let flags = cursor.read_u16()?;
        let ambient = read_colour(cursor, (flags & 3) == 3)?;
        let diffuse = read_colour(cursor, false)?;
        let specular = read_colour(cursor, (flags & 5) == 5)?;
        let emission = read_colour(cursor, (flags & 9) == 9)?;
        let shininess = cursor.read_f32()?;
        let mut reflectivity = 0.0f32;
        if cursor.major() >= 10 {
            reflectivity = cursor.read_f32()?;
            let _bumpiness = cursor.read_f32()?;
            if version >= 2 {
                let _palette = cursor.read_u32()?;
            }
        } else if version == 2 {
            reflectivity = cursor.read_f32()?;
        }
        Ok(Material {
            version,
            ambient,
            diffuse,
            specular,
            emission,
            shininess,
            reflectivity,
        })
    }
}

/// Read one colour: four components, or a single grey value expanded to one.
fn read_colour(cursor: &mut Cursor<'_>, single: bool) -> Result<[f32; 4]> {
    if single {
        let value = cursor.read_f32()?;
        return Ok([value, value, value, 1.0]);
    }
    let mut colour = [0.0f32; 4];
    for component in &mut colour {
        *component = cursor.read_f32()?;
    }
    Ok(colour)
}

/// A named property attached to a node.
#[derive(Clone, Debug)]
pub enum Property {
    /// A string value; the name of a property is itself a string property.
    String(StringProperty),
    /// A reference to another segment.
    LateLoaded(LateLoaded),
    /// A class the reader knows the GUID of but does not model.
    Unsupported {
        /// Class name from the class registry.
        class_name: &'static str,
    },
}

impl Property {
    /// The value of a string property, `None` for every other kind.
    #[must_use]
    pub fn string_value(&self) -> Option<&str> {
        match self {
            Property::String(property) => Some(&property.value),
            Property::LateLoaded(_) | Property::Unsupported { .. } => None,
        }
    }

    /// Read the body of a property whose class is known from its GUID.
    pub(crate) fn read(guid: &Guid, cursor: &mut Cursor<'_>) -> Result<Self> {
        let property = if *guid == types::PROPERTY_STRING {
            Property::String(StringProperty::read(cursor)?)
        } else if *guid == types::PROPERTY_LATE_LOADED {
            Property::LateLoaded(LateLoaded::read(cursor)?)
        } else if *guid == types::PROPERTY_BASE {
            Property::Unsupported {
                class_name: "Base property",
            }
        } else {
            return Err(Error::invalid(
                cursor.position() as u64,
                "unknown property type",
            ));
        };
        Ok(property)
    }
}

/// Read the fields shared by every property.
fn read_property_base(cursor: &mut Cursor<'_>) -> Result<u32> {
    let major = cursor.major();
    let mut version = 0u32;
    if major > 8 {
        version = cursor.read_version()?;
    }
    let _state_flags = cursor.read_u32()?;
    Ok(version)
}

/// A string property.
#[derive(Clone, Debug, Default)]
pub struct StringProperty {
    /// Version of the property.
    pub version: u32,
    /// The value.
    pub value: String,
}

impl StringProperty {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let version = read_property_base(cursor)?;
        if cursor.major() > 8 {
            let _ = cursor.read_version()?;
        }
        Ok(StringProperty {
            version,
            value: cursor.read_mb_string()?,
        })
    }
}

/// A reference to an object stored in another segment.
#[derive(Clone, Debug, Default)]
pub struct LateLoaded {
    /// Version of the property.
    pub version: u32,
    /// Segment GUID holding the object.
    pub guid: Guid,
    /// Segment type of that segment.
    pub segment_type: i32,
    /// Object id inside that segment.
    pub object_id: i32,
}

impl LateLoaded {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let version = read_property_base(cursor)?;
        if cursor.major() > 8 {
            let _ = cursor.read_version()?;
        }
        let guid = cursor.read_guid()?;
        let segment_type = cursor.read_i32()?;
        let mut object_id = 0i32;
        if cursor.major() > 8 {
            object_id = cursor.read_i32()?;
            if cursor.major() < 10 {
                let _reserved = cursor.read_i32()?;
            }
        }
        Ok(LateLoaded {
            version,
            guid,
            segment_type,
            object_id,
        })
    }
}

/// A representation element, as stored in a non-LSG segment.
#[derive(Clone, Debug)]
pub enum Element {
    /// A vertex based representation (JT 8.x only carries usable data here).
    ShapeLodVertex(ShapeLodVertex),
    /// A topologically compressed tri-strip representation.
    ShapeLodTriStripSet(ShapeLodTriStripSet),
    /// A base LOD element, which carries no geometry.
    ShapeLodBase,
    /// A key/value stream describing an object.
    ProxyMetaData(ProxyMetaData),
    /// An extended B-rep, not modelled.
    XtBRep,
    /// A PMI manager, not modelled.
    PmiManager,
    /// A class the reader knows the GUID of but does not model.
    Unsupported {
        /// Class name from the class registry.
        class_name: &'static str,
    },
}

impl Element {
    /// Read the body of an element whose class is known from its GUID.
    pub(crate) fn read(guid: &Guid, cursor: &mut Cursor<'_>) -> Result<Self> {
        let element = if *guid == types::SHAPE_LOD_TRI_STRIP_SET {
            Element::ShapeLodTriStripSet(ShapeLodTriStripSet::read(cursor)?)
        } else if *guid == types::SHAPE_LOD_VERTEX {
            Element::ShapeLodVertex(ShapeLodVertex::read(cursor)?)
        } else if *guid == types::SHAPE_LOD_BASE {
            ShapeLodBase::read(cursor)?;
            Element::ShapeLodBase
        } else if *guid == types::ELEMENT_PROXY_META_DATA {
            Element::ProxyMetaData(ProxyMetaData::read(cursor)?)
        } else if *guid == types::XTB_REP {
            XtBRep::read(cursor)?;
            Element::XtBRep
        } else if *guid == types::ELEMENT_META_DATA_PMI_MANAGER {
            PmiManager::read(cursor)?;
            Element::PmiManager
        } else {
            return Err(Error::invalid(
                cursor.position() as u64,
                "unknown element type",
            ));
        };
        Ok(element)
    }

    /// The name of the element class, as registered by the reference
    /// implementation.
    #[must_use]
    pub fn class_name(&self) -> &'static str {
        match self {
            Element::ShapeLodVertex(_) => "JtElement_ShapeLOD_Vertex",
            Element::ShapeLodTriStripSet(_) => "JtElement_ShapeLOD_TriStripSet",
            Element::ShapeLodBase => "JtElement_ShapeLOD_Base",
            Element::ProxyMetaData(_) => "JtElement_ProxyMetaData",
            Element::XtBRep => "JtElement_XTBRep",
            Element::PmiManager => "JtElement_MetaData_PMIManager",
            Element::Unsupported { class_name } => class_name,
        }
    }

    /// Geometry carried by the element, when it is a shape representation.
    #[must_use]
    pub fn geometry(&self) -> Option<&LodGeometry> {
        match self {
            Element::ShapeLodVertex(element) => Some(&element.geometry),
            Element::ShapeLodTriStripSet(element) => Some(&element.geometry),
            _ => None,
        }
    }
}

/// Geometry decoded from a representation element.
#[derive(Clone, Debug, Default)]
pub struct LodGeometry {
    /// Index buffer into the unified vertex/normal pairs.
    pub indices: Vec<i32>,
    /// Vertex positions, three components per unique vertex.
    pub vertices: Vec<f32>,
    /// Normals, three components per unique normal.
    pub normals: Vec<f32>,
    /// Number of unique vertices.
    pub vertex_count: usize,
    /// Number of unique normals.
    pub normal_count: usize,
    /// The normal binding of a JT 8.x representation, `None` for the newer
    /// topological representations.
    pub normal_binding: Option<u8>,
}

impl LodGeometry {
    /// Number of indices.
    pub fn index_count(&self) -> usize {
        self.indices.len()
    }

    /// Positions as stored: three components per vertex.
    #[must_use]
    pub fn raw_vertices(&self) -> &[f32] {
        &self.vertices
    }

    /// Normals as stored: three components per normal.
    #[must_use]
    pub fn raw_normals(&self) -> &[f32] {
        &self.normals
    }
}

/// A shape LOD base element, which carries no data of its own.
#[derive(Clone, Debug, Default)]
pub struct ShapeLodBase;

impl ShapeLodBase {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        if cursor.major() > 8 {
            let _version = cursor.read_version()?;
        }
        Ok(ShapeLodBase)
    }
}

/// A vertex based representation element.
#[derive(Clone, Debug, Default)]
pub struct ShapeLodVertex {
    /// Decoded geometry, empty for JT 9.x and 10.x where the reference itself
    /// stops after the header.
    pub geometry: LodGeometry,
}

impl ShapeLodVertex {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let geometry = read_shape_lod_data(cursor, false)?;
        if cursor.major() < 9 {
            return Ok(ShapeLodVertex {
                geometry: read_vertex_based_compressed_rep(cursor)?,
            });
        }
        // The reference stops here for a vertex shape LOD: the representation
        // data of JT 9.x/10.x files is only carried by the tri-strip variant.
        Ok(ShapeLodVertex {
            geometry: geometry.unwrap_or_default(),
        })
    }
}

/// A topologically compressed tri-strip representation element.
#[derive(Clone, Debug, Default)]
pub struct ShapeLodTriStripSet {
    /// Decoded geometry.
    pub geometry: LodGeometry,
}

impl ShapeLodTriStripSet {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        // For JT 9.x and 10.x the representation is read inside the shape LOD
        // data, before the trailing version of this element.
        let geometry = read_shape_lod_data(cursor, true)?;
        let _version = cursor.read_version()?;
        if cursor.major() < 9 {
            return Ok(ShapeLodTriStripSet {
                geometry: read_vertex_based_compressed_rep(cursor)?,
            });
        }
        Ok(ShapeLodTriStripSet {
            geometry: geometry.unwrap_or_default(),
        })
    }
}

/// Vertex/normal binding flags decoded from a shape LOD header.
#[derive(Clone, Copy, Debug, Default)]
struct Binding {
    vertex_components: usize,
    normal_binding: bool,
    normal_components: usize,
    /// Parsed from the binding, but colour and texture channels are not read.
    #[allow(dead_code)]
    colour_components: usize,
    /// Parsed from the binding, but colour and texture channels are not read.
    #[allow(dead_code)]
    texture_components: usize,
}

impl Binding {
    fn has_normals(&self) -> bool {
        self.normal_binding && self.normal_components > 0
    }
}

/// Read the common part of a shape LOD element.
///
/// Returns the decoded geometry when the representation is carried inside the
/// shape LOD data, which is the case for a topologically compressed tri strip
/// set of JT 9.x and 10.x.
fn read_shape_lod_data(cursor: &mut Cursor<'_>, tri_strip: bool) -> Result<Option<LodGeometry>> {
    let major = cursor.major();
    if major < 9 {
        if major > 7 {
            let _version = cursor.read_version()?;
        }
        // The 32 bit vertex binding of JT 8.x is read again by the
        // representation reader, which is where its meaning lies.
        cursor.read_bytes(4)?;
        read_quantization_params(cursor)?;
        return Ok(None);
    }

    // JT 9.x and 10.x start with the LOD base, which only carries a version.
    let _base_version = cursor.read_version()?;
    let _version = cursor.read_version()?;
    let binding = read_vertex_binding2(cursor)?;
    if major >= 10 {
        let _version4 = cursor.read_version()?;
        let _mesh_version = cursor.read_version()?;
        let _tri_id = cursor.read_i32()?;
        if tri_strip {
            let marker = cursor.read_bytes(16)?;
            if marker != TOPOLOGY_MARKER {
                return Err(Error::corrupt("the topology marker of the LOD is invalid"));
            }
            cursor.skip(9)?;
        }
    } else {
        let _mesh_version = cursor.read_version()?;
        let _tri_id = cursor.read_i32()?;
        let _version4 = cursor.read_version()?;
    }
    if tri_strip {
        return read_topologically_compressed_data(cursor, &binding).map(Some);
    }
    Ok(None)
}

/// Read the 64 bit vertex binding of JT 9.x and 10.x.
fn read_vertex_binding2(cursor: &mut Cursor<'_>) -> Result<Binding> {
    let little = cursor.order() == ByteOrder::Little;
    // The binding is a 64 bit field: one byte of low flags, the texture flags,
    // two reserved bytes and one byte of high flags.
    let (lsb, text_flags, msb) = if little {
        let lsb = cursor.read_u8()?;
        let text_flags = cursor.read_u32()?;
        cursor.skip(2)?;
        let msb = cursor.read_u8()?;
        (lsb, text_flags, msb)
    } else {
        let msb = cursor.read_u8()?;
        cursor.skip(2)?;
        let text_flags = cursor.read_u32()?;
        let lsb = cursor.read_u8()?;
        (lsb, text_flags, msb)
    };
    let _ = msb;
    let vertex = lsb & 0x7;
    let normal = (lsb >> 3) & 0x1;
    let colour = (lsb >> 4) & 0x3;
    let _flag = (lsb >> 6) & 0x1;
    let texture = (text_flags >> 4) & 0xF;
    Ok(Binding {
        vertex_components: match vertex {
            1 => 2,
            2 => 3,
            4 => 4,
            _ => 0,
        },
        normal_binding: normal != 0,
        normal_components: usize::from(normal != 0),
        colour_components: match colour {
            1 => 3,
            2 => 4,
            _ => 0,
        },
        texture_components: match texture {
            1 => 1,
            2 => 2,
            4 => 3,
            8 => 4,
            _ => 0,
        },
    })
}

/// Read the four quantisation bit counts. They are stored but not used to
/// dequantise the topological path, which carries its own quantiser data.
fn read_quantization_params(cursor: &mut Cursor<'_>) -> Result<[u8; 4]> {
    let mut params = [0u8; 4];
    for value in &mut params {
        *value = cursor.read_u8()?;
    }
    Ok(params)
}

/// Read `count` XYZ triples of `f32` out of `data` starting at `offset`.
///
/// Returns the values together with the offset just past them.
fn read_f32_triples(
    data: &[u8],
    mut offset: usize,
    order: ByteOrder,
    count: usize,
) -> Result<(Vec<f32>, usize)> {
    let mut values = Vec::with_capacity(count * 3);
    for _ in 0..count {
        let Some(window) = data.get(offset..offset + 12) else {
            return Err(Error::invalid(
                offset as u64,
                "the raw vertex data is shorter than declared",
            ));
        };
        for component in 0..3 {
            let bytes: [u8; 4] = window[component * 4..component * 4 + 4]
                .try_into()
                .expect("slice of four bytes");
            let word = if order == ByteOrder::Big {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            };
            values.push(f32::from_bits(word));
        }
        offset += 12;
    }
    Ok((values, offset))
}

/// Read the JT 8.x vertex based compressed representation.
fn read_vertex_based_compressed_rep(cursor: &mut Cursor<'_>) -> Result<LodGeometry> {
    let _version = cursor.read_version()?;
    let normal_binding = cursor.read_u8()?;
    let _text_coord_binding = cursor.read_u8()?;
    let _colour_binding = cursor.read_u8()?;
    let params = read_quantization_params(cursor)?;

    let mut primitives = Package::load1(cursor)?;
    let offsets = primitives.decode(cursor.order(), Unpack::Stride1)?;
    if offsets.len() < 2 {
        return Err(Error::corrupt(
            "the primitive list of the shape rep is empty",
        ));
    }
    if params[0] == 0 {
        read_lossless_rep(cursor, normal_binding, &offsets)
    } else {
        read_quantized_rep(cursor, normal_binding, &offsets)
    }
}

/// Expand the primitive list of a JT 8.x rep into a triangle index list.
///
/// `map` turns a slot of the primitive list into the emitted index, which is
/// the slot itself for the lossless path and the stored vertex index for the
/// quantised one.
fn expand_strips(offsets: &[i32], mut emit: impl FnMut(i32)) {
    let mut off1 = 1i32;
    let mut off2 = 2i32;
    for window in offsets.windows(2) {
        let mut origin = window[0];
        while origin < window[1] - 2 {
            emit(origin);
            emit(origin + off1);
            emit(origin + off2);
            off1 ^= 3;
            off2 ^= 3;
            origin += 1;
        }
    }
}

/// Read the uncompressed (lossless) JT 8.x representation.
fn read_lossless_rep(
    cursor: &mut Cursor<'_>,
    normal_binding: u8,
    offsets: &[i32],
) -> Result<LodGeometry> {
    let _uncompressed = cursor.read_i32()?;
    let declared = cursor.read_i32()?;
    let compressed = usize::try_from(declared).unwrap_or(0);

    // The vertex and normal arrays may sit behind a DEFLATE stream. The
    // reference wraps the remaining element bytes in an inflate reader, which
    // consumes exactly the declared number of bytes; taking them up front keeps
    // the enclosing element aligned while the triples are read from the result.
    let packed = if compressed > 0 {
        Some(cursor.read_bytes(compressed)?)
    } else {
        None
    };

    let vertex_count = offsets[offsets.len() - 1];
    let count = usize::try_from(vertex_count).unwrap_or(0);
    let with_normals = normal_binding == NORMAL_BINDING_PER_VERTEX;
    let mut vertices = Vec::with_capacity(count * 3);
    let mut normals = Vec::new();
    match packed {
        None => {
            for _ in 0..count {
                if with_normals {
                    for _ in 0..3 {
                        normals.push(cursor.read_f32()?);
                    }
                }
                for _ in 0..3 {
                    vertices.push(cursor.read_f32()?);
                }
            }
        }
        Some(packed) => {
            let data = crate::codec::inflate(packed)?;
            let mut offset = 0usize;
            for _ in 0..count {
                if with_normals {
                    let (part, next) = read_f32_triples(&data, offset, cursor.order(), 1)?;
                    offset = next;
                    normals.extend_from_slice(&part);
                }
                let (part, next) = read_f32_triples(&data, offset, cursor.order(), 1)?;
                offset = next;
                vertices.extend_from_slice(&part);
            }
        }
    }
    let mut indices = Vec::new();
    expand_strips(offsets, |slot| indices.push(slot));
    Ok(LodGeometry {
        indices,
        vertices,
        normals,
        vertex_count: count,
        normal_count: if with_normals { count } else { 0 },
        normal_binding: Some(normal_binding),
    })
}

/// Read the quantised JT 8.x representation.
fn read_quantized_rep(
    cursor: &mut Cursor<'_>,
    normal_binding: u8,
    offsets: &[i32],
) -> Result<LodGeometry> {
    let mut coords = VertexData::load_quantized_coords(cursor)?;
    let vertices = coords.decode(cursor.order())?;
    let normals = if normal_binding != 0 {
        let mut norms = VertexData::load_quantized_normals(cursor)?;
        norms.decode(cursor.order())?
    } else {
        Decoded::default()
    };
    let mut strip = Package::load1(cursor)?;
    let slots = strip.decode(cursor.order(), Unpack::StripIdx)?;

    let mut indices = Vec::new();
    expand_strips(offsets, |slot| {
        let index = slots
            .get(usize::try_from(slot).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(-1);
        indices.push(index);
    });
    Ok(LodGeometry {
        indices,
        vertices: vertices.values().to_vec(),
        normals: normals.values().to_vec(),
        vertex_count: vertices.count(),
        normal_count: normals.count(),
        normal_binding: Some(normal_binding),
    })
}

/// Read the JT 9.x/10.x topologically compressed representation.
fn read_topologically_compressed_data(
    cursor: &mut Cursor<'_>,
    binding: &Binding,
) -> Result<LodGeometry> {
    // The mesh symbol streams come first, before the binding and counts.
    let mesh_input = MeshInput::load(cursor)?;
    let _binding = read_vertex_binding2(cursor)?;
    read_quantization_params(cursor)?;
    let vertex_count = cursor.read_i32()?;
    let _attribute_count = cursor.read_i32()?;
    if vertex_count == 0 {
        return Ok(LodGeometry::default());
    }

    let coords_present = binding.vertex_components > 0;
    let normals_present = binding.has_normals();

    let loaded_coords = coords_present
        .then(|| VertexData::load_compressed_coords(cursor))
        .transpose()?;
    let unique_vertex_count = loaded_coords
        .as_ref()
        .map_or(0, |data| data.out_vertex_count());
    let unique_vertices = match loaded_coords {
        Some(mut data) => data.decode(cursor.order())?,
        None => Decoded::default(),
    };

    let unique_normals = if normals_present {
        let mut norms = VertexData::load_compressed_normals(cursor)?;
        norms.decode(cursor.order())?
    } else {
        Decoded::default()
    };

    let topology = mesh_input.decode(cursor.order())?;
    let vertex_indices = topology.vertex_indices();
    let normal_indices = topology.normal_indices();

    if coords_present && normals_present {
        let unified = UnifiedMesh::new(&topology, unique_vertex_count);
        let pairs = unified.pair_count();
        let indices = unified.indices().to_vec();
        let mut vertices = Vec::with_capacity(pairs * unique_vertices.comp_count());
        let mut normals = Vec::with_capacity(pairs * unique_normals.comp_count());
        for &source in unified.sources().iter().take(pairs) {
            let index = usize::try_from(source).unwrap_or(usize::MAX);
            let vertex = vertex_indices.get(index).copied().unwrap_or(-1);
            let normal = normal_indices.get(index).copied().unwrap_or(-1);
            copy_component(&unique_vertices, vertex, &mut vertices);
            copy_component(&unique_normals, normal, &mut normals);
        }
        return Ok(LodGeometry {
            indices,
            vertices,
            normals,
            vertex_count: pairs,
            normal_count: pairs,
            normal_binding: None,
        });
    }
    if coords_present {
        return Ok(LodGeometry {
            indices: vertex_indices.to_vec(),
            vertices: unique_vertices.values().to_vec(),
            normals: Vec::new(),
            vertex_count: unique_vertices.count(),
            normal_count: 0,
            normal_binding: None,
        });
    }
    Ok(LodGeometry {
        indices: normal_indices.to_vec(),
        vertices: Vec::new(),
        normals: unique_normals.values().to_vec(),
        vertex_count: 0,
        normal_count: unique_normals.count(),
        normal_binding: None,
    })
}

/// Copy one vertex/normal out of a decoded component array.
fn copy_component(decoded: &Decoded, index: i32, out: &mut Vec<f32>) {
    let Ok(index) = usize::try_from(index) else {
        out.resize(out.len() + decoded.comp_count(), 0.0);
        return;
    };
    for component in 0..decoded.comp_count() {
        out.push(decoded.get(index, component));
    }
}

/// A key/value stream describing an object.
#[derive(Clone, Debug, Default)]
pub struct ProxyMetaData {
    /// The raw stream, NUL separated, exactly as the reference keeps it.
    pub stream: Vec<u8>,
}

impl ProxyMetaData {
    fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let _version = cursor.read_version()?;
        let mut stream = Vec::new();
        // The reference ignores the result of every string read here: a table
        // that runs past the end of the element simply ends, and the element is
        // recovered by skipping to its declared end.
        let mut key = read_table_key(cursor);
        while !key.is_empty() {
            push_cstr(&mut stream, &key);
            let Ok(value_type) = cursor.read_u8() else {
                break;
            };
            match value_type {
                1 => push_cstr(&mut stream, &read_table_key(cursor)),
                2 => {
                    let value = cursor.read_i32()?;
                    push_cstr(&mut stream, &value.to_string());
                }
                3 => {
                    let value = cursor.read_f32()?;
                    push_cstr(&mut stream, &format!("{value:.6}"));
                }
                4 => {
                    let year = cursor.read_i16()?;
                    let month = cursor.read_i16()?;
                    let day = cursor.read_i16()?;
                    let hour = cursor.read_i16()?;
                    let minute = cursor.read_i16()?;
                    let second = cursor.read_i16()?;
                    push_cstr(
                        &mut stream,
                        &format!("{year}-{month}-{day}T{hour}:{minute}:{second}"),
                    );
                }
                other => {
                    return Err(Error::corrupt(format!(
                        "meta data value type {other} is unknown"
                    )));
                }
            }
            key = read_table_key(cursor);
        }
        Ok(ProxyMetaData { stream })
    }
}

/// Read one entry of a meta data table, yielding an empty string when the
/// table runs past the end of the element.
fn read_table_key(cursor: &mut Cursor<'_>) -> String {
    cursor.read_mb_string().unwrap_or_default()
}

/// Append a NUL terminated string to a key/value stream.
fn push_cstr(stream: &mut Vec<u8>, text: &str) {
    stream.extend_from_slice(text.as_bytes());
    stream.push(0);
}

/// An extended B-rep. Its content is not modelled.
#[derive(Clone, Debug, Default)]
pub struct XtBRep;

impl XtBRep {
    fn read(cursor: &mut Cursor<'_>) -> Result<()> {
        let _version = cursor.read_i32()?;
        let _major = cursor.read_i32()?;
        let _minor = cursor.read_i32()?;
        if cursor.major() > 8 {
            let _build = cursor.read_i32()?;
        }
        let count = cursor.read_len()?;
        cursor.skip(count)?;
        Ok(())
    }
}

/// A PMI manager. Its content is not modelled.
#[derive(Clone, Debug, Default)]
pub struct PmiManager;

impl PmiManager {
    fn read(cursor: &mut Cursor<'_>) -> Result<()> {
        let _version = cursor.read_version()?;
        Ok(())
    }
}

/// An element as stored in a segment, before its class is resolved.
#[derive(Clone, Debug)]
pub struct RawElement {
    /// Class GUID.
    pub guid: Guid,
    /// Base type tag.
    pub base_type: u8,
    /// Object id, when the table stores one.
    pub object_id: i32,
    /// Offset of the body inside the segment payload.
    pub body_offset: usize,
    /// Length of the body in bytes.
    pub body_len: usize,
}

/// Read the envelope of one element, leaving the cursor on its body.
///
/// `lsg` selects the LSG segment layout, which always stores an object id;
/// other segments store one only from JT 9.x on.
///
/// Returns `None` at the end-of-element marker.
pub fn read_element_envelope(cursor: &mut Cursor<'_>, lsg: bool) -> Result<Option<RawElement>> {
    let length = cursor.read_i32()?;
    let start = cursor.position();
    if length < 0 {
        return Err(cursor.error("element length is negative"));
    }
    let guid = cursor.read_guid()?;
    if guid == Guid::END_OF_ELEMENT_MARKER {
        return Ok(None);
    }
    let known = class_of(&guid).is_some();
    let base_type = if known { cursor.read_u8()? } else { 0 };
    let object_id = if known && (lsg || cursor.major() > 8) {
        cursor.read_i32()?
    } else {
        0
    };
    let body_offset = cursor.position();
    let end = start
        .checked_add(usize::try_from(length).unwrap_or(0))
        .ok_or_else(|| cursor.error("element length overflow"))?;
    if end > cursor.position() + cursor.remaining() {
        return Err(cursor.error("element extends past the end of its segment"));
    }
    let body_len = end - body_offset;
    Ok(Some(RawElement {
        guid,
        base_type,
        object_id,
        body_offset,
        body_len,
    }))
}

/// Skip past an element whose body was not parsed.
pub fn skip_element(cursor: &mut Cursor<'_>, element: &RawElement) -> Result<()> {
    let end = element
        .body_offset
        .checked_add(element.body_len)
        .ok_or_else(|| cursor.error("element length overflow"))?;
    cursor.seek(end)
}

/// The class a GUID stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A logical scene graph node.
    Node,
    /// An attribute.
    Attribute,
    /// A property.
    Property,
    /// A representation element.
    Element,
}

/// Look a class GUID up in the registry.
pub fn class_of(guid: &Guid) -> Option<Class> {
    if guid == &types::NODE_PARTITION
        || guid == &types::NODE_PART
        || guid == &types::NODE_META_DATA
        || guid == &types::NODE_LOD
        || guid == &types::NODE_RANGE_LOD
        || guid == &types::NODE_INSTANCE
        || guid == &types::NODE_GROUP
        || guid == &types::NODE_BASE
        || guid == &types::NODE_SHAPE_BASE
        || guid == &types::NODE_SHAPE_VERTEX
        || guid == &types::NODE_SHAPE_TRI_STRIP_SET
    {
        Some(Class::Node)
    } else if guid == &types::ATTRIBUTE_GEOMETRIC_TRANSFORM || guid == &types::ATTRIBUTE_MATERIAL {
        Some(Class::Attribute)
    } else if guid == &types::PROPERTY_STRING
        || guid == &types::PROPERTY_LATE_LOADED
        || guid == &types::PROPERTY_BASE
    {
        Some(Class::Property)
    } else if guid == &types::SHAPE_LOD_TRI_STRIP_SET
        || guid == &types::SHAPE_LOD_VERTEX
        || guid == &types::SHAPE_LOD_BASE
        || guid == &types::ELEMENT_PROXY_META_DATA
        || guid == &types::XTB_REP
        || guid == &types::ELEMENT_META_DATA_PMI_MANAGER
    {
        Some(Class::Element)
    } else {
        None
    }
}
