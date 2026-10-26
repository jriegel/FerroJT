//! The model loader: TOC, segments, the logical scene graph and its tables.
//!
//! This mirrors the reference pipeline one to one. The fixed header points at a
//! TOC table, the TOC points at the logical scene graph segment, and that
//! segment holds node and attribute atoms, then property atoms, then a property
//! table tying the two together. Representation data lives in other segments
//! and is pulled in through late-load properties.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::Result;
use crate::jt::guid::Guid;
use crate::jt::object::{
    Attribute, Class, Element, LateLoad, LodGeometry, Material, Node, Property, class_of,
    read_element_envelope, skip_element,
};
use crate::jt::reader::Cursor;
use crate::jt::segment::{self, FileHeader, TocEntry};
use crate::model::{
    self, Aabb, Attribute as GeometryAttribute, Channel, ComponentType, Material as PbrMaterial,
    Mesh, Model, Primitive, Topology, Transform,
};

/// Property key whose value carries an object's display name.
const NAME_KEY: &str = "JT_PROP_NAME";

/// What an object id of a segment table refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectRef {
    /// Index into [`SceneGraph::nodes`].
    Node(usize),
    /// Index into [`SceneGraph::attributes`].
    Attribute(usize),
}

/// A node together with the id it is stored under.
#[derive(Clone, Debug)]
pub struct StoredNode {
    /// Object id as stored in the segment.
    pub id: i32,
    /// The node.
    pub node: Node,
}

/// An attribute together with the id it is stored under.
#[derive(Clone, Debug)]
pub struct StoredAttribute {
    /// Object id as stored in the segment.
    pub id: i32,
    /// The attribute.
    pub attribute: Attribute,
}

/// A property together with the id it is stored under.
#[derive(Clone, Debug)]
pub struct StoredProperty {
    /// Object id as stored in the segment.
    pub id: i32,
    /// The property.
    pub property: Property,
}

/// An element that a late-load property pulled in from its own segment.
#[derive(Clone, Debug)]
pub struct LoadedElement {
    /// Object id of the property the element was loaded for.
    pub property_id: i32,
    /// GUID of the segment the element came from.
    pub segment: Guid,
    /// Segment type of that segment.
    pub segment_type: i32,
    /// The element.
    pub element: Element,
}

/// The logical scene graph of one JT file.
#[derive(Clone, Debug, Default)]
pub struct SceneGraph {
    /// Every node atom of the segment, in file order.
    pub nodes: Vec<StoredNode>,
    /// Every attribute atom of the segment, in file order.
    pub attributes: Vec<StoredAttribute>,
    /// Every property atom of the segment, in file order.
    pub properties: Vec<StoredProperty>,
    /// Elements reached through a late-load property, in load order.
    pub loaded: Vec<LoadedElement>,
    /// Index of the root node inside [`SceneGraph::nodes`].
    pub root: Option<usize>,
    objects: HashMap<i32, ObjectRef>,
    property_ids: HashMap<i32, usize>,
    loaded_ids: HashMap<i32, usize>,
}

impl SceneGraph {
    /// Resolve an object id of the segment table.
    #[must_use]
    pub fn object(&self, id: i32) -> Option<ObjectRef> {
        self.objects.get(&id).copied()
    }

    /// The root node, when the segment had one.
    #[must_use]
    pub fn root(&self) -> Option<&Node> {
        self.root
            .and_then(|pos| self.nodes.get(pos))
            .map(|stored| &stored.node)
    }

    /// Position of the node with a given object id.
    #[must_use]
    pub fn node_pos(&self, id: i32) -> Option<usize> {
        match self.object(id)? {
            ObjectRef::Node(pos) => Some(pos),
            ObjectRef::Attribute(_) => None,
        }
    }

    /// Look a node up by its object id.
    #[must_use]
    pub fn node(&self, id: i32) -> Option<&Node> {
        self.node_pos(id)
            .and_then(|pos| self.nodes.get(pos))
            .map(|stored| &stored.node)
    }

    /// Look a node up by its position in [`SceneGraph::nodes`].
    #[must_use]
    pub fn node_at(&self, pos: usize) -> Option<&Node> {
        self.nodes.get(pos).map(|stored| &stored.node)
    }

    /// Look a property up by its object id.
    #[must_use]
    pub fn property(&self, id: i32) -> Option<&Property> {
        self.property_ids
            .get(&id)
            .and_then(|pos| self.properties.get(*pos))
            .map(|stored| &stored.property)
    }

    /// Resolve the attributes a node refers to.
    ///
    /// Ids that do not resolve, or that resolve to something which is not an
    /// attribute, are dropped — the reference drops them when its
    /// `dynamic_pointer_cast` fails.
    #[must_use]
    pub fn attributes_of(&self, node: &Node) -> Vec<&Attribute> {
        node.attributes()
            .iter()
            .filter_map(|deferred| match self.object(deferred.object_id)? {
                ObjectRef::Attribute(pos) => Some(&self.attributes.get(pos)?.attribute),
                ObjectRef::Node(_) => None,
            })
            .collect()
    }

    /// Resolve the children of any node that has children.
    ///
    /// Unresolved references stay `None` so that a writer can render the empty
    /// object the reference renders for them.
    #[must_use]
    pub fn children_of(&self, node: &Node) -> Vec<Option<usize>> {
        node.children()
            .iter()
            .map(|deferred| self.node_pos(deferred.object_id))
            .collect()
    }

    /// The node an instance refers to.
    #[must_use]
    pub fn instance_target(&self, node: &Node) -> Option<usize> {
        match node {
            Node::Instance(instance) => self.node_pos(instance.target.object_id),
            _ => None,
        }
    }

    /// The element a late-load property refers to.
    #[must_use]
    pub fn loaded_element(&self, load: &LateLoad) -> Option<&Element> {
        self.loaded_ids
            .get(&load.property_id)
            .and_then(|pos| self.loaded.get(*pos))
            .map(|stored| &stored.element)
    }

    /// Geometry of the representation a late-load property refers to.
    #[must_use]
    pub fn geometry_for(&self, load: &LateLoad) -> Option<&LodGeometry> {
        self.loaded_element(load)?.geometry()
    }

    fn bind_name(&mut self, id: i32, name: &str) {
        let Some(pos) = self.node_pos(id) else {
            return;
        };
        if let Some(stored) = self.nodes.get_mut(pos) {
            stored.node.bind_name(name);
        }
    }

    fn set_late_loads(&mut self, id: i32, loads: Vec<LateLoad>) {
        let Some(pos) = self.node_pos(id) else {
            return;
        };
        let Some(stored) = self.nodes.get_mut(pos) else {
            return;
        };
        if let Some(slot) = stored.node.late_loads_mut() {
            *slot = loads;
        }
    }
}

/// A JT file that has been read into memory.
#[derive(Clone, Debug)]
pub struct JtFile {
    /// Path the file was read from.
    pub path: PathBuf,
    /// Fixed header of the file.
    pub header: FileHeader,
    /// TOC table, in file order.
    pub toc: Vec<TocEntry>,
    /// The logical scene graph, absent when the file has no LSG segment.
    pub lsg: Option<SceneGraph>,
    offsets: HashMap<Guid, u64>,
}

impl JtFile {
    /// Read a JT file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path)?;
        Self::read(path, &data)
    }

    /// Parse an in-memory JT file. `path` is only used for diagnostics.
    pub fn read(path: &Path, data: &[u8]) -> Result<Self> {
        let header = segment::read_header(data)?;
        let toc = segment::read_toc(data, header.toc_offset, &header)?;
        let mut offsets = HashMap::new();
        for entry in &toc {
            offsets.insert(entry.guid, entry.offset);
        }

        let lsg = match offsets.get(&header.lsg_guid).copied() {
            Some(offset) => Some(read_scene_graph(data, offset, &header, &offsets)?),
            None => {
                tracing::warn!(path = %path.display(), "the file has no logical scene graph segment");
                None
            }
        };

        Ok(JtFile {
            path: path.to_path_buf(),
            header,
            toc,
            lsg,
            offsets,
        })
    }

    /// Look a segment up by GUID.
    #[must_use]
    pub fn segment_offset(&self, guid: &Guid) -> Option<u64> {
        self.offsets.get(guid).copied()
    }

    /// Translate the file into the format agnostic [`Model`] used by exporters.
    #[must_use]
    pub fn to_model(&self) -> Model {
        let mut model = Model::new();
        model.source = Some(self.path.display().to_string());
        model.extras = Some(json!({
            "format": "JT",
            "version": self.header.version_text(),
            "segments": self.toc.len(),
        }));
        let Some(graph) = &self.lsg else {
            return model;
        };
        let Some(root) = graph.root else {
            return model;
        };
        let index = build_node(graph, &mut model, root, &mut HashSet::new());
        model.default_scene_mut().roots = vec![index];
        model.bounds = model.nodes.get(index).and_then(|node| node.bounds);
        model
    }
}

/// Read the whole content of an LSG segment.
fn read_scene_graph(
    data: &[u8],
    offset: u64,
    header: &FileHeader,
    offsets: &HashMap<Guid, u64>,
) -> Result<SceneGraph> {
    let segment = segment::read_segment(data, offset, header)?;
    let mut cursor = Cursor::new(&segment.body, header.order, header.major);
    let mut graph = SceneGraph::default();

    // Node and attribute atoms share one id table, exactly as in the reference.
    read_node_atoms(&mut cursor, &mut graph)?;
    read_property_atoms(&mut cursor, &mut graph)?;
    read_property_table(&mut cursor, &mut graph)?;
    segment.check_length(cursor.position())?;

    load_late_loads(data, header, offsets, &mut graph)?;
    Ok(graph)
}

/// Read node and attribute atoms until the end-of-element marker.
fn read_node_atoms(cursor: &mut Cursor<'_>, graph: &mut SceneGraph) -> Result<()> {
    while let Some(element) = read_element_envelope(cursor, true)? {
        if let Some(class) = class_of(&element.guid) {
            cursor.seek(element.body_offset)?;
            match class {
                Class::Node => {
                    let node = Node::read(&element.guid, cursor)?;
                    let pos = graph.nodes.len();
                    graph.nodes.push(StoredNode {
                        id: element.object_id,
                        node,
                    });
                    graph
                        .objects
                        .insert(element.object_id, ObjectRef::Node(pos));
                    if graph.root.is_none() {
                        graph.root = Some(pos);
                    }
                }
                Class::Attribute => {
                    let attribute = Attribute::read(&element.guid, cursor)?;
                    let pos = graph.attributes.len();
                    graph.attributes.push(StoredAttribute {
                        id: element.object_id,
                        attribute,
                    });
                    graph
                        .objects
                        .insert(element.object_id, ObjectRef::Attribute(pos));
                }
                Class::Property | Class::Element => {}
            }
        }
        finish_element(cursor, &element)?;
    }
    Ok(())
}

/// Read property atoms until the end-of-element marker.
fn read_property_atoms(cursor: &mut Cursor<'_>, graph: &mut SceneGraph) -> Result<()> {
    while let Some(element) = read_element_envelope(cursor, true)? {
        if class_of(&element.guid) == Some(Class::Property) {
            cursor.seek(element.body_offset)?;
            let property = Property::read(&element.guid, cursor)?;
            let pos = graph.properties.len();
            graph.properties.push(StoredProperty {
                id: element.object_id,
                property,
            });
            graph.property_ids.insert(element.object_id, pos);
        }
        finish_element(cursor, &element)?;
    }
    Ok(())
}

/// Mirror the reference length check of a single element.
fn finish_element(cursor: &mut Cursor<'_>, element: &crate::jt::RawElement) -> Result<()> {
    let end = element
        .body_offset
        .checked_add(element.body_len)
        .ok_or_else(|| cursor.error("element length overflow"))?;
    if cursor.position() > end {
        return Err(cursor.error("element body read past its declared end"));
    }
    if cursor.position() < end {
        tracing::warn!(
            "{} element bytes remained after reading, corrected",
            end - cursor.position()
        );
        skip_element(cursor, element)?;
    }
    Ok(())
}

/// Read the property table that ties properties to objects.
fn read_property_table(cursor: &mut Cursor<'_>, graph: &mut SceneGraph) -> Result<()> {
    let _table_version = cursor.read_i16()?;
    let length = cursor.read_i32()?;
    for _ in 0..length {
        let object_id = cursor.read_i32()?;
        let mut late_loads: Vec<LateLoad> = Vec::new();
        let mut name: Option<String> = None;
        loop {
            let key = cursor.read_i32()?;
            if key == 0 {
                break;
            }
            let value = cursor.read_i32()?;
            let Some(property) = graph.property(value) else {
                continue;
            };
            match property {
                Property::LateLoaded(property) => {
                    late_loads.push(LateLoad::from_property(value, property));
                }
                Property::String(text) => {
                    // The name of a property is itself a string property, so the
                    // key has to be resolved before it can be recognised.
                    let key_is_name = graph
                        .property(key)
                        .and_then(Property::string_value)
                        .is_some_and(|key_text| key_text == NAME_KEY);
                    if key_is_name {
                        name = Some(text.value.clone());
                    }
                }
                Property::Unsupported { .. } => {}
            }
        }
        if let Some(name) = name {
            graph.bind_name(object_id, &name);
        }
        if !late_loads.is_empty() {
            graph.set_late_loads(object_id, late_loads);
        }
    }
    Ok(())
}

/// Pull in every segment a late-load property points at.
///
/// Each property loads its own copy, as the reference caches per property. A
/// segment that cannot be read is reported and skipped: the reference keeps
/// going when its `Load()` fails, and so do we.
fn load_late_loads(
    data: &[u8],
    header: &FileHeader,
    offsets: &HashMap<Guid, u64>,
    graph: &mut SceneGraph,
) -> Result<()> {
    let pending: Vec<LateLoad> = graph
        .nodes
        .iter()
        .flat_map(|stored| stored.node.late_loads().iter().copied())
        .collect();
    for load in pending {
        let Some(offset) = offsets.get(&load.guid).copied() else {
            tracing::warn!(guid = %load.guid, "the segment of a late load is not in the TOC");
            continue;
        };
        let element = match read_element_of_segment(data, offset, header) {
            Ok(element) => element,
            Err(err) => {
                tracing::warn!(guid = %load.guid, error = %err, "failed to read a late loaded segment");
                continue;
            }
        };
        let Some(element) = element else {
            continue;
        };
        let pos = graph.loaded.len();
        graph.loaded.push(LoadedElement {
            property_id: load.property_id,
            segment: load.guid,
            segment_type: load.segment_type,
            element,
        });
        graph.loaded_ids.insert(load.property_id, pos);
    }
    Ok(())
}

/// Read the first object of a non-LSG segment, like `ReadSegment` does.
///
/// A segment that fails to read yields no object rather than an error, exactly
/// as the reference does: `readSegment` reports the failure and returns a null
/// object, and the late load that asked for it simply resolves to nothing. A
/// malformed segment therefore never invalidates the rest of the file.
fn read_element_of_segment(
    data: &[u8],
    offset: u64,
    header: &FileHeader,
) -> Result<Option<Element>> {
    match read_element_of_segment_impl(data, offset, header) {
        Ok(element) => Ok(element),
        Err(err) => {
            tracing::warn!(
                offset,
                error = %err,
                "failed to read a segment, its content is ignored"
            );
            Ok(None)
        }
    }
}

fn read_element_of_segment_impl(
    data: &[u8],
    offset: u64,
    header: &FileHeader,
) -> Result<Option<Element>> {
    let segment = segment::read_segment(data, offset, header)?;
    let mut cursor = Cursor::new(&segment.body, header.order, header.major);
    let mut first = None;
    // The reference reads exactly one element here, then checks the length.
    if let Some(element) = read_element_envelope(&mut cursor, false)? {
        if class_of(&element.guid).is_some() {
            cursor.seek(element.body_offset)?;
            first = Some(Element::read(&element.guid, &mut cursor)?);
        }
        finish_element(&mut cursor, &element)?;
    }
    segment.check_length(cursor.position())?;
    Ok(first)
}

/// Append the geometry of a representation as a mesh, returning its index.
fn push_mesh(
    model: &mut Model,
    geometry: &LodGeometry,
    material: Option<usize>,
    name: Option<String>,
) -> usize {
    let mut primitive = Primitive {
        topology: Topology::TriangleList,
        material,
        bounds: bounds_of_positions(&geometry.vertices),
        ..Primitive::default()
    };

    if !geometry.indices.is_empty() {
        let bytes: Vec<u8> = geometry
            .indices
            .iter()
            .inspect(|index| {
                if **index < 0 {
                    tracing::warn!("an index of the representation is negative");
                }
            })
            .flat_map(|index| u32::try_from(*index).unwrap_or(0).to_le_bytes())
            .collect();
        let buffer = model.accessors.push_buffer(bytes);
        primitive.indices = Some(model.accessors.push_attribute(GeometryAttribute {
            name: "indices".to_owned(),
            buffer,
            offset: 0,
            count: geometry.indices.len(),
            components: 1,
            component_type: ComponentType::U32,
            normalized: false,
            min: None,
            max: None,
        }));
    }

    for (attribute_name, values) in [
        ("POSITION", &geometry.vertices),
        ("NORMAL", &geometry.normals),
    ] {
        if values.is_empty() {
            continue;
        }
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let buffer = model.accessors.push_buffer(bytes);
        let (min, max) = if attribute_name == "POSITION" {
            position_range(values)
        } else {
            (None, None)
        };
        primitive
            .attributes
            .push(model.accessors.push_attribute(GeometryAttribute {
                name: attribute_name.to_owned(),
                buffer,
                offset: 0,
                count: values.len() / 3,
                components: 3,
                component_type: ComponentType::F32,
                normalized: false,
                min,
                max,
            }));
    }

    model.push_mesh(Mesh {
        name,
        primitives: vec![primitive],
        bounds: bounds_of_positions(&geometry.vertices),
        lod_level: None,
        extras: None,
    })
}

/// Component wise min/max of a flat position buffer.
fn position_range(values: &[f32]) -> (Option<Vec<f64>>, Option<Vec<f64>>) {
    let Some(bounds) = bounds_of_positions(values) else {
        return (None, None);
    };
    (
        Some(bounds.min.iter().map(|v| f64::from(*v)).collect()),
        Some(bounds.max.iter().map(|v| f64::from(*v)).collect()),
    )
}

/// Bounds of a flat position buffer.
fn bounds_of_positions(values: &[f32]) -> Option<Aabb> {
    if values.len() < 3 {
        return None;
    }
    let mut bounds = Aabb::EMPTY;
    for point in values.chunks_exact(3) {
        bounds.push_point([point[0], point[1], point[2]]);
    }
    (!bounds.is_empty()).then_some(bounds)
}

/// Recursively translate one graph node into a [`model::Node`].
fn build_node(
    graph: &SceneGraph,
    model: &mut Model,
    pos: usize,
    expanded: &mut HashSet<usize>,
) -> usize {
    let Some(node) = graph.nodes.get(pos).map(|stored| &stored.node) else {
        return model.push_node(model::Node::default());
    };
    let name = (!node.name().is_empty()).then(|| node.name().to_owned());
    let material = material_of(graph, model, node);

    let mut out = model::Node {
        name: name.clone(),
        kind: Some(node.class_name().to_owned()),
        transform: transform_of(graph, node),
        bounds: node
            .bounds()
            .map(|b| Aabb::new([b[0], b[1], b[2]], [b[3], b[4], b[5]])),
        ..model::Node::default()
    };

    for load in node.late_loads() {
        if let Some(geometry) = graph.geometry_for(load) {
            let mesh = push_mesh(model, geometry, material, name.clone());
            out.instances.push(model::Instance {
                mesh,
                material,
                lod_bias: 1.0,
            });
        }
    }

    let index = model.push_node(out);
    if !expanded.insert(pos) {
        return index;
    }

    let mut children = Vec::new();
    for child in graph.children_of(node).into_iter().flatten() {
        children.push(build_node(graph, model, child, expanded));
    }
    if let Some(target) = graph.instance_target(node) {
        children.push(build_node(graph, model, target, expanded));
    }
    model.nodes[index].children = children;
    index
}

/// The transform described by a node's geometric transform attribute.
fn transform_of(graph: &SceneGraph, node: &Node) -> Transform {
    graph
        .attributes_of(node)
        .iter()
        .find_map(|attribute| match attribute {
            Attribute::GeometricTransform(attribute) => Some(decompose(&attribute.matrix)),
            _ => None,
        })
        .unwrap_or(Transform::IDENTITY)
}

/// The material described by a node's material attribute, appended on demand.
fn material_of(graph: &SceneGraph, model: &mut Model, node: &Node) -> Option<usize> {
    let source = graph
        .attributes_of(node)
        .iter()
        .find_map(|attribute| match attribute {
            Attribute::Material(attribute) => Some(attribute.clone()),
            _ => None,
        })?;
    Some(model.push_material(to_material(&source)))
}

/// Translate a JT material attribute into a [`PbrMaterial`].
///
/// JT stores a fixed-function colour set rather than a metallic/roughness
/// workflow, so shininess is mapped onto roughness inversely.
fn to_material(source: &Material) -> PbrMaterial {
    PbrMaterial {
        base_color: Channel::constant(source.diffuse),
        emissive: Channel::constant(source.emission),
        roughness: Channel::constant([roughness_of(source.shininess); 4]),
        extras: Some(json!({
            "ambient": source.ambient,
            "specular": source.specular,
            "shininess": source.shininess,
            "reflectivity": source.reflectivity,
        })),
        ..PbrMaterial::default()
    }
}

/// Map a shininess exponent onto a roughness factor.
fn roughness_of(shininess: f32) -> f32 {
    let roughness = 1.0 - (shininess / (shininess + 1.0)).sqrt();
    roughness.clamp(0.0, 1.0)
}

/// Split a row-major 4x4 matrix into translation, rotation and scale.
fn decompose(matrix: &[f64; 16]) -> Transform {
    let column = |axis: usize| [matrix[axis], matrix[4 + axis], matrix[8 + axis]];
    let length = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();

    let scales = [length(column(0)), length(column(1)), length(column(2))];
    let translation = [matrix[12] as f32, matrix[13] as f32, matrix[14] as f32];
    if scales.contains(&0.0) {
        return Transform {
            translation,
            ..Transform::IDENTITY
        };
    }

    let inverse = [1.0 / scales[0], 1.0 / scales[1], 1.0 / scales[2]];
    let rotation = [
        [
            column(0)[0] * inverse[0],
            column(1)[0] * inverse[1],
            column(2)[0] * inverse[2],
        ],
        [
            column(0)[1] * inverse[0],
            column(1)[1] * inverse[1],
            column(2)[1] * inverse[2],
        ],
        [
            column(0)[2] * inverse[0],
            column(1)[2] * inverse[1],
            column(2)[2] * inverse[2],
        ],
    ];

    Transform {
        translation,
        rotation: to_quaternion(rotation),
        scale: [scales[0] as f32, scales[1] as f32, scales[2] as f32],
    }
}

/// Convert a rotation matrix to an `x, y, z, w` quaternion.
fn to_quaternion(m: [[f64; 3]; 3]) -> [f32; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let (x, y, z, w) = if trace > 0.0 {
        let s = 0.5 / (trace + 1.0).sqrt();
        (
            (m[2][1] - m[1][2]) * s,
            (m[0][2] - m[2][0]) * s,
            (m[1][0] - m[0][1]) * s,
            0.5 / s,
        )
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = 2.0 * (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt();
        (
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        )
    } else if m[1][1] > m[2][2] {
        let s = 2.0 * (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt();
        (
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        )
    } else {
        let s = 2.0 * (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt();
        (
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        )
    };
    [x as f32, y as f32, z as f32, w as f32]
}
