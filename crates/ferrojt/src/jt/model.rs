//! The model loader: TOC, segments, the logical scene graph and its tables.
//!
//! This mirrors the reference pipeline one to one. The fixed header points at a
//! TOC table, the TOC points at the logical scene graph segment, and that
//! segment holds node and attribute atoms, then property atoms, then a property
//! table tying the two together. Representation data lives in other segments
//! and is pulled in through late-load properties.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::{Error, Result};
use crate::jt::dump::{LayerInfo, handle_late_loads_meta, scan_for_layer, scan_for_layer_filter};
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

/// The layer filter an export applies to the parts of a scene graph.
///
/// A JT file declares its named filters in the key/value stream of a meta data
/// node and tags each part with the layers it belongs to. The filter decides
/// which of those parts are drawn; it never removes a node from the scene
/// graph, only the geometry of the part it rejects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerFilter {
    /// Name of the filter, `None` when the file declares none.
    pub name: Option<String>,
    /// Layer numbers the filter selects.
    pub layers: Vec<u32>,
}

impl LayerFilter {
    /// `true` when the filter selects layers, i.e. when parts have to be
    /// checked against it at all.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.layers.is_empty()
    }

    /// Does a part tagged with `layers` survive this filter?
    ///
    /// A part that carries no layer tag at all is not affected by a filter and
    /// is kept, which is what the classic JT viewer does for an untagged part.
    #[must_use]
    pub fn keeps(&self, layers: &[u32]) -> bool {
        if !self.is_active() || layers.is_empty() {
            return true;
        }
        layers.iter().any(|layer| self.layers.contains(layer))
    }
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

    /// The named layer filters declared by the meta data nodes of the graph.
    ///
    /// A file can spread its meta data over several nodes; the first one that
    /// declares filters wins, in file order, which is what the classic JT
    /// viewer stores and puts in its filter box.
    #[must_use]
    pub fn layer_info(&self) -> Option<LayerInfo> {
        for stored in &self.nodes {
            if !matches!(stored.node, Node::MetaData(_)) {
                continue;
            }
            let stream = handle_late_loads_meta(self, stored.node.late_loads());
            if stream.is_empty() {
                continue;
            }
            let info = scan_for_layer_filter(&stream);
            if !info.filters.is_empty() {
                return Some(info);
            }
        }
        None
    }

    /// The layer numbers a part node is tagged with.
    ///
    /// A part that has no key/value stream, or one without a `LAYER::` key, is
    /// not tagged and answers with an empty vector.
    #[must_use]
    pub fn layers_of(&self, node: &Node) -> Vec<u32> {
        let stream = handle_late_loads_meta(self, node.late_loads());
        if stream.is_empty() {
            return Vec::new();
        }
        scan_for_layer(&stream)
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
    ///
    /// A range LOD holds the same shape at several levels of detail. Only the
    /// highest one is turned into a mesh, so an export does not repeat the
    /// geometry of a part once per tier. Every node of the scene graph is
    /// still translated, and [`Mesh::lod_level`] records which tier a mesh
    /// came from.
    ///
    /// When the file declares layer filters, the default one also decides
    /// which parts contribute geometry; see [`JtFile::layer_filter`].
    #[must_use]
    pub fn to_model(&self) -> Model {
        self.build_model(self.layer_filter(None).ok().flatten().as_ref())
    }

    /// Translate the file into a [`Model`] that contains every part.
    ///
    /// The layer filters of the file are ignored, which is what the JSON dump
    /// shows and what `ferrojt convert --no-layer-filter` writes.
    #[must_use]
    pub fn to_model_unfiltered(&self) -> Model {
        self.build_model(None)
    }

    /// Translate the file into a [`Model`] with one named layer filter.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] is returned when `name` is not a filter of this
    /// file, and the message lists the filters that are available.
    pub fn to_model_with_filter(&self, name: &str) -> Result<Model> {
        let filter = self.layer_filter(Some(name))?;
        Ok(self.build_model(filter.as_ref()))
    }

    /// The filter an export uses, `None` when the file has no layer
    /// information at all.
    ///
    /// `name` selects one of the filters the file declares. `None` asks for
    /// the default one: the filter named by `ACTIVELAYERFILTER::`, or, when
    /// the file leaves that empty as most writers do, the first filter in
    /// sorted order — the selection the classic JT viewer makes.
    ///
    /// # Errors
    ///
    /// See [`JtFile::to_model_with_filter`].
    pub fn layer_filter(&self, name: Option<&str>) -> Result<Option<LayerFilter>> {
        let Some(graph) = self.lsg.as_ref() else {
            return Ok(None);
        };
        let Some(info) = graph.layer_info() else {
            if name.is_some() {
                return Err(Error::invalid(0, "this file declares no layer filters"));
            }
            return Ok(None);
        };
        let chosen = match name {
            Some(name) => {
                let layers = info.filters.get(name).ok_or_else(|| {
                    let available = info
                        .filters
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    Error::invalid(0, format!("there is no layer filter {name:?}; {available}"))
                })?;
                Some(LayerFilter {
                    name: Some(name.to_owned()),
                    layers: layers.clone(),
                })
            }
            None => {
                // An empty `ACTIVELAYERFILTER::` is the common case: the writer
                // knows the filters but has not picked one, so the first of the
                // sorted names is used, exactly as the viewer's filter box does.
                let active = if info.active.is_empty() {
                    info.filters.keys().next()
                } else {
                    info.filters
                        .get_key_value(&info.active)
                        .map(|(name, _)| name)
                };
                let Some(active) = active else {
                    return Ok(None);
                };
                Some(LayerFilter {
                    name: Some(active.clone()),
                    layers: info.filters.get(active).cloned().unwrap_or_default(),
                })
            }
        };
        Ok(chosen)
    }

    /// Walk the scene graph into a [`Model`], drawing only the parts the given
    /// filter accepts.
    #[must_use]
    fn build_model(&self, filter: Option<&LayerFilter>) -> Model {
        let mut model = Model::new();
        model.source = Some(self.path.display().to_string());
        let mut extras = json!({
            "format": "JT",
            "version": self.header.version_text(),
            "segments": self.toc.len(),
        });
        if let Some(filter) = filter {
            extras["layer_filter"] = json!({
                "name": filter.name,
                "layers": filter.layers,
            });
        }
        model.extras = Some(extras);
        let Some(graph) = &self.lsg else {
            return model;
        };
        let Some(root) = graph.root else {
            return model;
        };
        let index = build_node(
            graph,
            &mut model,
            root,
            &mut Built::default(),
            LodWalk::ROOT,
            filter,
        );
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
    lod_level: Option<u32>,
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
        lod_level,
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

/// What has been translated from the scene graph so far.
///
/// An instance node points at a subtree that is shared rather than copied: the
/// reference reader walks that subtree once per instance, and a viewer expects
/// to see the instantiated shape as many times as the file instantiates it.
/// Every graph position is still translated exactly once, so the shared
/// subtree appears as one [`model::Node`] that both instances refer to — which
/// is also what glTF encodes as a node listed by two parents.
#[derive(Default)]
struct Built {
    /// Graph position to the model node built for it.
    ///
    /// Recorded before the children are walked, so a reference cycle — or a
    /// subtree reached twice — resolves to the node already under way instead
    /// of recursing forever.
    done: HashMap<usize, usize>,
}

/// Recursively translate one graph node into a [`model::Node`].
///
/// A part whose layers are rejected by `filter` keeps its node — the graph
/// keeps its shape — but neither its attributes, its geometry nor its children
/// are translated, which is what the reference glTF writer skips as well.
fn build_node(
    graph: &SceneGraph,
    model: &mut Model,
    pos: usize,
    built: &mut Built,
    lod: LodWalk,
    filter: Option<&LayerFilter>,
) -> usize {
    if let Some(&index) = built.done.get(&pos) {
        return index;
    }
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

    let drawn = match (node, filter) {
        (Node::Part(_), Some(filter)) => filter.keeps(&graph.layers_of(node)),
        _ => true,
    };
    if drawn && lod.keeps_geometry() {
        let level = lod.level();
        for load in node.late_loads() {
            if let Some(geometry) = graph.geometry_for(load) {
                if geometry.vertices.is_empty() {
                    // A file may declare a representation with no vertices at
                    // all — `SampleJT_Server.jt` does for 36 of its shapes. The
                    // node stays, there is simply nothing to draw, and a
                    // primitive without positions is not expressible in glTF.
                    tracing::debug!(
                        node = node.name(),
                        "a representation declares no vertices and is left out"
                    );
                    continue;
                }
                let mesh = push_mesh(model, geometry, material, name.clone(), level);
                out.instances.push(model::Instance {
                    mesh,
                    material,
                    lod_bias: 1.0,
                });
            }
        }
    }

    let index = model.push_node(out);
    built.done.insert(pos, index);
    if !drawn {
        return index;
    }

    let mut children = Vec::new();
    let mut walk = if matches!(node, Node::RangeLod(_)) {
        LodWalk::FIRST
    } else {
        lod
    };
    for child in graph.children_of(node).into_iter().flatten() {
        children.push(build_node(graph, model, child, built, walk, filter));
        let class_name = graph
            .nodes
            .get(child)
            .map_or("", |stored| stored.node.class_name());
        walk.next_tier(class_name);
    }
    if let Some(target) = graph.instance_target(node) {
        children.push(build_node(graph, model, target, built, walk, filter));
    }
    model.nodes[index].children = children;
    index
}

/// Where a node sits in the level of detail stack of a range LOD.
///
/// JT does not store the tier of a representation: a range LOD simply lists
/// its children from the finest to the coarsest one. The reference glTF
/// writer numbers them while walking, starting at zero and moving on at every
/// group of that subtree, and drops the shape of anything that is not tier
/// zero. This mirrors that, so the same file exports the same mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LodWalk {
    /// Set below a range LOD, where the tier of a branch matters.
    inside: bool,
    /// Tier of the branch currently being walked.
    level: u32,
}

impl LodWalk {
    /// Outside of any range LOD, where every representation is kept.
    const ROOT: Self = Self {
        inside: false,
        level: 0,
    };

    /// The first child of a range LOD.
    const FIRST: Self = Self {
        inside: true,
        level: 0,
    };

    /// `true` when the representations of this branch belong in the model.
    #[must_use]
    fn keeps_geometry(self) -> bool {
        !self.inside || self.level == 0
    }

    /// The tier to record on a mesh, if a range LOD defines one.
    #[must_use]
    fn level(self) -> Option<u32> {
        self.inside.then_some(self.level)
    }

    /// Move past a child of a range LOD. Only a group counts as a tier, and a
    /// group nested in a group moves on too, as it does in the reference.
    fn next_tier(&mut self, class_name: &str) {
        if self.inside && class_name == "Group" {
            self.level += 1;
        }
    }
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
///
/// A file repeats the same colour set on every shape of a part, so an
/// identical material is reused rather than appended again: the plate samples
/// carry the attribute nine or twelve times but describe only two materials.
fn material_of(graph: &SceneGraph, model: &mut Model, node: &Node) -> Option<usize> {
    let source = graph
        .attributes_of(node)
        .iter()
        .find_map(|attribute| match attribute {
            Attribute::Material(attribute) => Some(attribute.clone()),
            _ => None,
        })?;
    Some(model.add_material(to_material(&source)))
}

/// Translate a JT material attribute into a [`PbrMaterial`].
///
/// JT stores a fixed-function colour set rather than a metallic/roughness
/// workflow, so only the diffuse and emissive colours carry over directly. The
/// reference glTF writer pairs them with a fixed metal-free plastic look and
/// double sided geometry, which is what a CAD viewer expects; the shininess it
/// cannot express is kept in [`extras`](PbrMaterial::extras) for a consumer
/// that wants it.
fn to_material(source: &Material) -> PbrMaterial {
    PbrMaterial {
        base_color: Channel::constant(source.diffuse),
        metallic: Channel::constant([METALLIC_FACTOR; 4]),
        roughness: Channel::constant([ROUGHNESS_FACTOR; 4]),
        emissive: Channel::constant(source.emission),
        double_sided: true,
        extras: Some(json!({
            "ambient": source.ambient,
            "specular": source.specular,
            "shininess": source.shininess,
            "reflectivity": source.reflectivity,
        })),
        ..PbrMaterial::default()
    }
}

/// Metallic factor of a JT material, as in the reference glTF writer.
const METALLIC_FACTOR: f32 = 0.2;

/// Roughness factor of a JT material, as in the reference glTF writer.
///
/// The shininess exponent of a JT file cannot be mapped onto roughness
/// directly: the samples all carry a highlight exponent of 128, which as a
/// glTF roughness would be a near mirror and blows the colour out to white
/// under the default lighting of a viewer.
const ROUGHNESS_FACTOR: f32 = 0.8;

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
///
/// The result is unit length, which the glTF writer also insists on: a
/// non-normalised quaternion silently scales the geometry it rotates.
fn to_quaternion(m: [[f64; 3]; 3]) -> [f32; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let (x, y, z, w) = if trace > 0.0 {
        let s = 0.5 / (trace + 1.0).sqrt();
        (
            (m[2][1] - m[1][2]) * s,
            (m[0][2] - m[2][0]) * s,
            (m[1][0] - m[0][1]) * s,
            0.25 / s,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::ByteOrder;
    use crate::jt::object::{
        Deferred, Group, MetaData, Part, Partition, ProxyMetaData, ShapeLodVertex,
    };

    /// The filters of the fixture: `ALL` covers layer 1, `LAYCUR` only layer
    /// 20, and the file leaves the active one unset.
    const FILTERS: &[u8] = b"LAYERFILTER001::\x00ALL=1,2,256\x00LAYERFILTER000::\x00LAYCUR=20\x00ACTIVELAYERFILTER::\x00\x00";

    /// The layer tag of the tagged part of the fixture.
    const TAG: &[u8] = b"LAYER::\x001\x00";

    /// One triangle, enough to make a part contribute a mesh.
    fn geometry() -> LodGeometry {
        LodGeometry {
            indices: vec![0, 1, 2],
            vertices: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            vertex_count: 3,
            ..LodGeometry::default()
        }
    }

    /// Register the element a late-load property refers to.
    fn register(graph: &mut SceneGraph, property_id: i32, element: Element) {
        graph.loaded_ids.insert(property_id, graph.loaded.len());
        graph.loaded.push(LoadedElement {
            property_id,
            segment: Guid::default(),
            segment_type: 3,
            element,
        });
    }

    /// A hand built scene graph: a root that holds a meta data node, a part
    /// tagged with layer 1 which in turn holds a group, and a part that carries
    /// no layer tag at all.
    ///
    /// `filters` and `tag` are the key/value streams of the meta data node and
    /// of the tagged part; `None` stands for a node without such a stream,
    /// which is how a file with no layer information at all looks.
    fn fixture(filters: Option<&[u8]>, tag: Option<&[u8]>) -> JtFile {
        let mut graph = SceneGraph::default();

        let kids = vec![
            Deferred { object_id: 1 },
            Deferred { object_id: 2 },
            Deferred { object_id: 3 },
        ];
        let root = Partition {
            base: Group {
                children: kids.clone(),
                name: "root".to_owned(),
                ..Group::default()
            },
            file_name: "fixture.jt".to_owned(),
            up_axis: 0,
            name: "root".to_owned(),
            children: kids,
        };
        graph.nodes.push(StoredNode {
            id: 0,
            node: Node::Partition(root),
        });
        graph.objects.insert(0, ObjectRef::Node(0));

        let mut meta = MetaData {
            name: "MetaData".to_owned(),
            ..MetaData::default()
        };
        if let Some(stream) = filters {
            meta.base.base.late_loads.push(LateLoad {
                property_id: 11,
                guid: Guid::default(),
                segment_type: 3,
                object_id: 1,
            });
            register(
                &mut graph,
                11,
                Element::ProxyMetaData(ProxyMetaData {
                    stream: stream.to_vec(),
                }),
            );
        }
        graph.nodes.push(StoredNode {
            id: 1,
            node: Node::MetaData(meta),
        });
        graph.objects.insert(1, ObjectRef::Node(1));

        let mut tagged = Part {
            name: "tagged.part".to_owned(),
            ..Part::default()
        };
        tagged.base.base.children = vec![Deferred { object_id: 4 }];
        if let Some(stream) = tag {
            tagged.base.base.base.late_loads.push(LateLoad {
                property_id: 12,
                guid: Guid::default(),
                segment_type: 3,
                object_id: 1,
            });
            register(
                &mut graph,
                12,
                Element::ProxyMetaData(ProxyMetaData {
                    stream: stream.to_vec(),
                }),
            );
        }
        tagged.base.base.base.late_loads.push(LateLoad {
            property_id: 13,
            guid: Guid::default(),
            segment_type: 3,
            object_id: 1,
        });
        register(
            &mut graph,
            13,
            Element::ShapeLodVertex(ShapeLodVertex {
                geometry: geometry(),
            }),
        );
        graph.nodes.push(StoredNode {
            id: 2,
            node: Node::Part(tagged),
        });
        graph.objects.insert(2, ObjectRef::Node(2));

        let mut plain = Part {
            name: "plain.part".to_owned(),
            ..Part::default()
        };
        plain.base.base.base.late_loads.push(LateLoad {
            property_id: 14,
            guid: Guid::default(),
            segment_type: 3,
            object_id: 1,
        });
        register(
            &mut graph,
            14,
            Element::ShapeLodVertex(ShapeLodVertex {
                geometry: geometry(),
            }),
        );
        graph.nodes.push(StoredNode {
            id: 3,
            node: Node::Part(plain),
        });
        graph.objects.insert(3, ObjectRef::Node(3));

        graph.nodes.push(StoredNode {
            id: 4,
            node: Node::Group(Group {
                name: "child.group".to_owned(),
                ..Group::default()
            }),
        });
        graph.objects.insert(4, ObjectRef::Node(4));

        graph.root = Some(0);
        JtFile {
            path: PathBuf::from("fixture.jt"),
            header: FileHeader {
                major: 9,
                minor: 5,
                order: ByteOrder::Little,
                toc_offset: 0,
                lsg_guid: Guid::default(),
            },
            toc: Vec::new(),
            lsg: Some(graph),
            offsets: HashMap::new(),
        }
    }

    /// Position of the model node with a given name.
    fn node_index(model: &Model, name: &str) -> usize {
        model
            .nodes
            .iter()
            .position(|node| node.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("the model has no node {name:?}"))
    }

    #[test]
    fn a_filter_only_rejects_parts_that_carry_a_layer_tag() {
        let filter = LayerFilter {
            name: None,
            layers: vec![7],
        };
        assert!(filter.is_active());
        assert!(filter.keeps(&[]), "an untagged part is not affected");
        assert!(filter.keeps(&[7]));
        assert!(!filter.keeps(&[8]));
        assert!(
            LayerFilter::default().keeps(&[8]),
            "an empty filter selects nothing to check"
        );
    }

    #[test]
    fn the_default_filter_is_the_first_of_the_declared_ones() {
        let file = fixture(Some(FILTERS), Some(TAG));
        let filter = file
            .layer_filter(None)
            .expect("the fixture resolves")
            .expect("the fixture declares filters");
        assert_eq!(filter.name.as_deref(), Some("ALL"));
        assert_eq!(filter.layers, vec![1, 2, 256]);

        let model = file.to_model();
        assert_eq!(model.meshes.len(), 2, "layer 1 is part of ALL");
        let extras = model.extras.expect("the model carries extras");
        assert_eq!(extras["layer_filter"]["name"], "ALL");
        assert_eq!(extras["layer_filter"]["layers"], json!([1, 2, 256]));
    }

    #[test]
    fn a_filter_drops_the_geometry_of_the_parts_it_rejects() {
        let file = fixture(Some(FILTERS), Some(TAG));
        let model = file
            .to_model_with_filter("LAYCUR")
            .expect("LAYCUR is declared");
        assert_eq!(model.meshes.len(), 1, "only the untagged part is left");

        // The rejected part keeps its node, but loses its mesh and its subtree.
        let dropped = node_index(&model, "tagged.part");
        assert!(model.nodes[dropped].instances.is_empty());
        assert!(model.nodes[dropped].children.is_empty());
        assert_eq!(model.nodes.len(), 4, "the child of that part is gone");

        let all = file.to_model();
        assert_eq!(all.nodes.len(), 5, "the graph keeps its shape");
        let kept = node_index(&all, "tagged.part");
        assert_eq!(all.nodes[kept].instances.len(), 1);
        assert_eq!(all.nodes[kept].children.len(), 1);
    }

    #[test]
    fn a_part_on_another_layer_is_dropped_by_the_default_filter() {
        let file = fixture(Some(FILTERS), Some(b"LAYER::\x0020\x00"));
        assert_eq!(file.to_model().meshes.len(), 1, "ALL does not cover 20");
        assert_eq!(
            file.to_model_unfiltered().meshes.len(),
            2,
            "ignoring the filters keeps both parts"
        );
        assert_eq!(
            file.to_model_with_filter("LAYCUR")
                .expect("LAYCUR is declared")
                .meshes
                .len(),
            2,
            "LAYCUR covers 20"
        );
    }

    #[test]
    fn an_unknown_filter_names_the_ones_that_exist() {
        let file = fixture(Some(FILTERS), Some(TAG));
        let error = file
            .to_model_with_filter("nope")
            .expect_err("nope is not declared");
        let message = error.to_string();
        assert!(
            message.contains("ALL, LAYCUR"),
            "the message has to list the filters: {message}"
        );
    }

    #[test]
    fn a_file_without_layer_information_exports_everything() {
        let file = fixture(None, None);
        let graph = file.lsg.as_ref().expect("the fixture has a graph");
        assert!(graph.layer_info().is_none());
        assert_eq!(file.layer_filter(None).expect("nothing to resolve"), None);
        assert_eq!(file.to_model().meshes.len(), 2);
        assert_eq!(file.to_model_unfiltered().meshes.len(), 2);
        assert_eq!(file.to_model().nodes.len(), 5);
        assert!(
            file.to_model()
                .extras
                .expect("extras")
                .get("layer_filter")
                .is_none(),
            "no filter means nothing to record"
        );

        let error = file
            .layer_filter(Some("ALL"))
            .expect_err("the fixture declares no filter");
        assert!(error.to_string().contains("no layer filters"));
    }

    #[test]
    fn layers_and_filters_come_from_the_key_value_streams() {
        let file = fixture(Some(FILTERS), Some(TAG));
        let graph = file.lsg.as_ref().expect("the fixture has a graph");

        let info = graph
            .layer_info()
            .expect("the meta data node declares filters");
        assert_eq!(info.active, "");
        assert_eq!(
            info.filters.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["ALL", "LAYCUR"]
        );
        assert_eq!(info.filters["LAYCUR"], vec![20]);

        assert_eq!(
            graph.layers_of(graph.node_at(2).expect("the tagged part")),
            vec![1]
        );
        assert!(
            graph
                .layers_of(graph.node_at(3).expect("the untagged part"))
                .is_empty()
        );
    }

    #[test]
    fn a_transform_attribute_yields_a_unit_quaternion() {
        // A quarter turn about Z: the quaternion has to be normalised, or the
        // glTF writer scales the geometry it rotates.
        let matrix = [
            0.0, -1.0, 0.0, 0.0, //
            1.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            4.0, 5.0, 6.0, 1.0,
        ];
        let transform = decompose(&matrix);
        assert_eq!(transform.translation, [4.0, 5.0, 6.0]);
        assert_eq!(transform.scale, [1.0, 1.0, 1.0]);
        let [x, y, z, w] = transform.rotation;
        let norm = (x * x + y * y + z * z + w * w).sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "not unit length: {norm}");
        let half_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
        assert!((z - half_sqrt2).abs() < 1e-6, "wrong sine: {z}");
        assert!((w - half_sqrt2).abs() < 1e-6, "wrong cosine: {w}");
        assert!(x.abs() < 1e-6 && y.abs() < 1e-6, "off axis: {x} {y}");

        // The identity matrix, which takes the trace branch, stays identity.
        let identity = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0,
        ];
        assert_eq!(decompose(&identity).rotation, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_representation_without_vertices_keeps_its_node_but_has_no_mesh() {
        // `SampleJT_Server.jt` declares shapes that carry no vertices at all.
        // Such a representation cannot become a glTF primitive — that format
        // needs a POSITION attribute in every one — so it is left out while the
        // node that holds it stays where the scene graph put it.
        let mut file = fixture(None, None);
        for loaded in &mut file.lsg.as_mut().unwrap().loaded {
            if loaded.property_id == 14 {
                loaded.element = Element::ShapeLodVertex(ShapeLodVertex::default());
            }
        }

        let model = file.to_model();
        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.meshes[0].name.as_deref(), Some("tagged.part"));

        let plain = node_index(&model, "plain.part");
        assert!(model.nodes[plain].instances.is_empty());
        assert_eq!(model.nodes[plain].kind.as_deref(), Some("Part"));
    }
}
