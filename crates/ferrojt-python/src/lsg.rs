//! Python access to the logical scene graph (LSG) of a JT file.
//!
//! [`Model`](crate::Model) is the export view: one mesh per part, only the
//! finest level of detail, the layer filter of the file already applied. The
//! types here are the *file* view — every node of the scene graph, every
//! representation the file late-loads (all levels of detail, also the ones a
//! filter would drop), with the JT class names, the key/value properties and
//! the attributes the CAD system wrote.
//!
//! The handles are views into the parsed file rather than copies of it:
//!
//! * [`Lsg`] owns one parsed `ferrojt::jt::JtFile` and is shared by every
//!   handle that came from it, so the decoded data cannot move while a handle
//!   lives.
//! * [`JtNode`] is a position in that graph, [`JtMesh`] is one representation
//!   of it, and [`JtArray`] is one channel of a representation.
//!
//! A representation is identified by its position in the late-load list of the
//! graph, and that id is what makes instancing usable from Python: an
//! `Instance` node and the shape it instantiates resolve to the *same* id, so
//! a consumer loads the geometry once and places it as often as the file
//! instantiates it.

use std::collections::HashSet;
use std::ffi::CString;
use std::ffi::{c_int, c_void};
use std::ptr;

use pyo3::exceptions::{PyBufferError, PyIndexError, PyValueError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use ferrojt::jt::{
    Attribute, JtFile as RawFile, LodGeometry, Node, SceneGraph, decompose_transform,
    handle_late_loads_meta, key_value_pairs, segment_type_name,
};

/// The parsed content of one JT file, shared by every handle of its graph.
///
/// This is the object that keeps the decoded geometry alive: a [`JtNode`],
/// [`JtMesh`] or [`JtArray`] holds a reference to it, which is what makes the
/// zero-copy arrays of [`JtArray`] safe — the bytes they point at cannot be
/// freed or moved while an array is alive.
#[pyclass(frozen, name = "JtLsg")]
pub struct Lsg {
    file: RawFile,
}

impl Lsg {
    /// Wrap a parsed file. Called by [`crate::JtFile`].
    pub fn new(file: RawFile) -> Self {
        Lsg { file }
    }

    /// The scene graph, absent when the file has no LSG segment.
    fn graph(&self) -> Option<&SceneGraph> {
        self.file.lsg.as_ref()
    }

    /// The parsed file behind the graph.
    pub fn file(&self) -> &RawFile {
        &self.file
    }
}

#[pymethods]
impl Lsg {
    /// Path of the file the graph was read from.
    #[getter]
    fn path(&self) -> String {
        self.file.path.display().to_string()
    }

    /// Number of nodes of the logical scene graph.
    #[getter]
    fn node_count(&self) -> usize {
        self.graph().map_or(0, |graph| graph.nodes.len())
    }

    /// Number of representations of the file: the late loads that decoded
    /// geometry. The meta data late loads are not representations.
    #[getter]
    fn representation_count(&self) -> usize {
        self.representations().len()
    }

    /// Ids of the representations of the file, ascending.
    fn representations(&self) -> Vec<usize> {
        let Some(graph) = self.graph() else {
            return Vec::new();
        };
        graph
            .loaded
            .iter()
            .enumerate()
            .filter(|(_, stored)| stored.element.geometry().is_some())
            .map(|(id, _)| id)
            .collect()
    }

    /// The root node of the graph, `None` when the file has no LSG segment.
    pub fn root(slf: Py<Self>, py: Python<'_>) -> PyResult<Option<Py<JtNode>>> {
        let Some(pos) = slf.get().graph().and_then(|graph| graph.root) else {
            return Ok(None);
        };
        JtNode::new(py, &slf, pos).map(Some)
    }

    /// Every node of the graph, in the order the file stored them.
    fn nodes(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<Py<JtNode>>> {
        let count = slf.get().node_count();
        (0..count).map(|pos| JtNode::new(py, &slf, pos)).collect()
    }

    /// Every representation of the file, ordered by id.
    pub fn meshes(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<Py<JtMesh>>> {
        slf.get()
            .representations()
            .into_iter()
            .map(|id| JtMesh::new(py, &slf, id))
            .collect()
    }

    /// One representation by id, as reported by [`JtMesh::id`].
    fn mesh(slf: Py<Self>, py: Python<'_>, id: usize) -> PyResult<Py<JtMesh>> {
        if !slf.get().representations().contains(&id) {
            return Err(PyIndexError::new_err(format!(
                "the file has no representation {id}"
            )));
        }
        JtMesh::new(py, &slf, id)
    }

    fn __repr__(&self) -> String {
        format!(
            "JtLsg(path={:?}, nodes={}, representations={})",
            self.path(),
            self.node_count(),
            self.representation_count()
        )
    }
}

/// One node of the logical scene graph.
///
/// The handle is a position in the graph of its file, so it stays valid as
/// long as the file object it came from is open, and two handles pointing at
/// the same node compare equal.
#[pyclass(frozen, name = "JtNode")]
pub struct JtNode {
    lsg: Py<Lsg>,
    pos: usize,
}

impl JtNode {
    /// A handle for a node position of `lsg`.
    pub fn new(py: Python<'_>, lsg: &Py<Lsg>, pos: usize) -> PyResult<Py<Self>> {
        Py::new(
            py,
            JtNode {
                lsg: lsg.clone_ref(py),
                pos,
            },
        )
    }

    fn graph(&self) -> Option<&SceneGraph> {
        self.lsg.get().graph()
    }

    fn node(&self) -> Option<&Node> {
        self.graph()?.node_at(self.pos)
    }

    /// The matrix of the transform attribute of the node, as stored.
    fn stored_matrix(&self) -> Option<[f64; 16]> {
        self.graph()?
            .attributes_of(self.node()?)
            .into_iter()
            .find_map(|attribute| match attribute {
                Attribute::GeometricTransform(transform) => Some(transform.matrix),
                _ => None,
            })
    }

    /// The material attribute of the node, as stored.
    fn stored_material(&self) -> Option<ferrojt::jt::Material> {
        self.graph()?
            .attributes_of(self.node()?)
            .into_iter()
            .find_map(|attribute| match attribute {
                Attribute::Material(material) => Some(material),
                _ => None,
            })
            .cloned()
    }
}

#[pymethods]
impl JtNode {
    /// The JT class of the node: `Partition`, `Part`, `MetaData`, `LOD`,
    /// `RangeLOD`, `Instance`, `Group` or `Shape`.
    #[getter]
    fn class_name(&self) -> &'static str {
        self.node().map_or("", |node| node.class_name())
    }

    /// The object id of the node in the segment table of its file.
    #[getter]
    fn id(&self) -> Option<i32> {
        self.graph()?.nodes.get(self.pos).map(|stored| stored.id)
    }

    /// Position of the node inside the node list of its file.
    #[getter]
    fn position(&self) -> usize {
        self.pos
    }

    /// Name from the `JT_PROP_NAME` property, empty when the node has none.
    #[getter]
    fn name(&self) -> String {
        self.node()
            .map_or_else(String::new, |node| node.name().to_owned())
    }

    /// Element version of the node class.
    #[getter]
    fn version(&self) -> u32 {
        self.node().map_or(0, |node| node.base().version)
    }

    /// Node flags as stored.
    #[getter]
    fn flags(&self) -> u32 {
        self.node().map_or(0, |node| node.base().flags)
    }

    /// `True` for a shape node whose representation is a tri-strip set.
    #[getter]
    fn is_tri_strip_set(&self) -> bool {
        self.node().is_some_and(Node::is_tri_strip_set)
    }

    /// `True` when the node is an `Instance` of another node.
    #[getter]
    fn is_instance(&self) -> bool {
        matches!(self.node(), Some(Node::Instance(_)))
    }

    /// Number of child references of the node.
    fn child_count(&self) -> usize {
        self.node().map_or(0, |node| node.children().len())
    }

    /// The children, in file order. A reference that does not resolve to a
    /// node is skipped — see [`JtNode::unresolved_children`].
    fn children(&self, py: Python<'_>) -> PyResult<Vec<Py<JtNode>>> {
        let Some(node) = self.node() else {
            return Ok(Vec::new());
        };
        let children: Vec<usize> = self
            .graph()
            .into_iter()
            .flat_map(|graph| graph.children_of(node))
            .flatten()
            .collect();
        children
            .into_iter()
            .map(|pos| JtNode::new(py, &self.lsg, pos))
            .collect()
    }

    /// Number of child references that do not resolve to a node.
    fn unresolved_children(&self) -> usize {
        let (Some(graph), Some(node)) = (self.graph(), self.node()) else {
            return 0;
        };
        graph
            .children_of(node)
            .into_iter()
            .filter(|child| child.is_none())
            .count()
    }

    /// The node an `Instance` refers to, `None` for every other class.
    fn instance_target(&self, py: Python<'_>) -> PyResult<Option<Py<JtNode>>> {
        let target = self
            .graph()
            .and_then(|graph| self.node().and_then(|node| graph.instance_target(node)));
        match target {
            Some(pos) => JtNode::new(py, &self.lsg, pos).map(Some),
            None => Ok(None),
        }
    }

    /// Every node below this one, in depth-first order and each node once.
    ///
    /// The target of an `Instance` is followed, and a node reached twice is
    /// reported once, so a multiply instantiated or cyclic subtree terminates.
    fn descendants(&self, py: Python<'_>) -> PyResult<Vec<Py<JtNode>>> {
        let Some(graph) = self.graph() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![self.pos];
        while let Some(pos) = stack.pop() {
            if !seen.insert(pos) {
                continue;
            }
            if pos != self.pos {
                out.push(JtNode::new(py, &self.lsg, pos)?);
            }
            let Some(node) = graph.node_at(pos) else {
                continue;
            };
            for child in graph.children_of(node).into_iter().rev().flatten() {
                stack.push(child);
            }
            if let Some(target) = graph.instance_target(node) {
                stack.push(target);
            }
        }
        Ok(out)
    }

    /// The ids of the representations this very node carries.
    ///
    /// An `Instance` usually has none of its own — the geometry sits on the
    /// shape it instantiates, which [`JtNode::subtree_mesh_ids`] reports.
    #[getter]
    fn mesh_ids(&self) -> Vec<usize> {
        let (Some(graph), Some(node)) = (self.graph(), self.node()) else {
            return Vec::new();
        };
        node.late_loads()
            .iter()
            .filter_map(|load| graph.loaded_pos(load))
            .filter(|id| {
                graph
                    .loaded
                    .get(*id)
                    .is_some_and(|stored| stored.element.geometry().is_some())
            })
            .collect()
    }

    /// The ids of every representation in the subtree below this node,
    /// following instance targets and reporting each representation once.
    ///
    /// This is what an `Instance` draws. The ids are the ids of
    /// [`JtMesh::id`], so two instances of one part answer with the same id
    /// and its geometry loads once.
    fn subtree_mesh_ids(&self) -> Vec<usize> {
        let Some(graph) = self.graph() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen_nodes = HashSet::new();
        let mut stack = vec![self.pos];
        while let Some(pos) = stack.pop() {
            if !seen_nodes.insert(pos) {
                continue;
            }
            let Some(node) = graph.node_at(pos) else {
                continue;
            };
            for load in node.late_loads() {
                let represented = graph.loaded_pos(load).filter(|id| {
                    graph
                        .loaded
                        .get(*id)
                        .is_some_and(|stored| stored.element.geometry().is_some())
                });
                if let Some(id) = represented {
                    out.push(id);
                }
            }
            for child in graph.children_of(node).into_iter().rev().flatten() {
                stack.push(child);
            }
            if let Some(target) = graph.instance_target(node) {
                stack.push(target);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The representations this very node carries.
    fn meshes(&self, py: Python<'_>) -> PyResult<Vec<Py<JtMesh>>> {
        self.mesh_ids()
            .into_iter()
            .map(|id| JtMesh::new(py, &self.lsg, id))
            .collect()
    }

    /// Every representation in the subtree below this node.
    fn subtree_meshes(&self, py: Python<'_>) -> PyResult<Vec<Py<JtMesh>>> {
        self.subtree_mesh_ids()
            .into_iter()
            .map(|id| JtMesh::new(py, &self.lsg, id))
            .collect()
    }

    /// The key/value properties of the node, sorted by key.
    ///
    /// This is the whole stream the late loads of the node point at, so it
    /// carries `Name::`, `CAD_MASS::`, `Translation Date::`, the `LAYER::` tag
    /// and whatever else the CAD system wrote — the same stream the JSON dump
    /// prints.
    fn properties<'a>(&self, py: Python<'a>) -> PyResult<Bound<'a, PyDict>> {
        let dict = PyDict::new(py);
        let pairs = self
            .graph()
            .and_then(|graph| self.node().map(|node| (graph, node)))
            .map(|(graph, node)| key_value_pairs(&handle_late_loads_meta(graph, node.late_loads())))
            .unwrap_or_default();
        for (key, value) in pairs {
            dict.set_item(key, value)?;
        }
        Ok(dict)
    }

    /// The layer numbers a part is tagged with, empty when it carries no tag.
    #[getter]
    fn layers(&self) -> Vec<u32> {
        let (Some(graph), Some(node)) = (self.graph(), self.node()) else {
            return Vec::new();
        };
        graph.layers_of(node)
    }

    /// Class names of the attributes attached to the node.
    #[getter]
    fn attribute_names(&self) -> Vec<&'static str> {
        let (Some(graph), Some(node)) = (self.graph(), self.node()) else {
            return Vec::new();
        };
        graph
            .attributes_of(node)
            .into_iter()
            .map(Attribute::class_name)
            .collect()
    }

    /// The 4x4 transform of the node as stored, **column major**: the element
    /// of row `r`, column `c` is at index `4 * c + r` and the translation is
    /// the last column, at 12..14. `None` when the node has no transform
    /// attribute.
    #[getter]
    fn matrix(&self) -> Option<[f64; 16]> {
        self.stored_matrix()
    }

    /// The transform of the node as the exporters see it: a `translation`, a
    /// unit `rotation` quaternion `(x, y, z, w)` and a per-axis `scale`, taken
    /// apart from the stored matrix. `None` when the node has no transform
    /// attribute.
    ///
    /// These are the values *of the attribute*, relative to the parent — a
    /// consumer that places geometry has to compose them down the path, the
    /// same way the exporters do.
    #[getter]
    fn transform<'a>(&self, py: Python<'a>) -> PyResult<Option<Bound<'a, PyDict>>> {
        let (Some(graph), Some(node)) = (self.graph(), self.node()) else {
            return Ok(None);
        };
        let has_transform = graph
            .attributes_of(node)
            .iter()
            .any(|attribute| matches!(attribute, Attribute::GeometricTransform(_)));
        if !has_transform {
            return Ok(None);
        }
        let parts = ferrojt::jt::transform_of(graph, node);
        let dict = PyDict::new(py);
        dict.set_item("translation", parts.translation)?;
        dict.set_item("rotation", parts.rotation)?;
        dict.set_item("scale", parts.scale)?;
        Ok(Some(dict))
    }

    /// Translation of the transform attribute, or `None` without one.
    #[getter]
    fn translation(&self) -> Option<[f32; 3]> {
        let matrix = self.stored_matrix()?;
        Some(decompose_transform(&matrix).translation)
    }

    /// Rotation of the transform attribute as a unit quaternion
    /// `(x, y, z, w)`, or `None` without one.
    #[getter]
    fn rotation(&self) -> Option<[f32; 4]> {
        let matrix = self.stored_matrix()?;
        Some(decompose_transform(&matrix).rotation)
    }

    /// Per-axis scale of the transform attribute, or `None` without one.
    #[getter]
    fn scale(&self) -> Option<[f32; 3]> {
        let matrix = self.stored_matrix()?;
        Some(decompose_transform(&matrix).scale)
    }

    /// The material attribute of the node as a dict of `ambient`, `diffuse`,
    /// `specular` and `emission` (each RGBA) plus `shininess` and
    /// `reflectivity`. `None` when the node has no material attribute.
    fn material<'a>(&self, py: Python<'a>) -> PyResult<Option<Bound<'a, PyDict>>> {
        let Some(source) = self.stored_material() else {
            return Ok(None);
        };
        let dict = PyDict::new(py);
        dict.set_item("ambient", source.ambient)?;
        dict.set_item("diffuse", source.diffuse)?;
        dict.set_item("specular", source.specular)?;
        dict.set_item("emission", source.emission)?;
        dict.set_item("shininess", source.shininess)?;
        dict.set_item("reflectivity", source.reflectivity)?;
        Ok(Some(dict))
    }

    /// Local space bounds `(min_x, min_y, min_z, max_x, max_y, max_z)` of a
    /// shape node, `None` for every other class.
    #[getter]
    fn bounds(&self) -> Option<[f32; 6]> {
        self.node().and_then(Node::bounds)
    }

    /// Surface area a shape node declares.
    #[getter]
    fn area(&self) -> Option<f32> {
        match self.node() {
            Some(Node::Shape(shape)) => Some(shape.area),
            Some(Node::ShapeVertex(shape)) => Some(shape.base.area),
            Some(Node::ShapeTriStripSet(shape)) => Some(shape.base.area),
            _ => None,
        }
    }

    /// The percentage limits of an `LOD` or `RangeLOD` node.
    #[getter]
    fn lod_limits(&self) -> Option<Vec<f32>> {
        match self.node() {
            Some(Node::Lod(lod)) => Some(lod.limits.clone()),
            Some(Node::RangeLod(lod)) => Some(lod.limits.clone()),
            _ => None,
        }
    }

    /// The signal value an `LOD` or `RangeLOD` compares its limits against.
    #[getter]
    fn lod_signal(&self) -> Option<f32> {
        match self.node() {
            Some(Node::Lod(lod)) => Some(lod.signal),
            Some(Node::RangeLod(lod)) => Some(lod.base.signal),
            _ => None,
        }
    }

    /// The centre a `RangeLOD` measures its tiers from.
    #[getter]
    fn lod_center(&self) -> Option<[f32; 3]> {
        match self.node() {
            Some(Node::RangeLod(lod)) => Some(lod.center),
            _ => None,
        }
    }

    /// The axis indicator of a `Partition` node.
    #[getter]
    fn up_axis(&self) -> Option<i32> {
        match self.node() {
            Some(Node::Partition(partition)) => Some(partition.up_axis),
            _ => None,
        }
    }

    /// The file name a `Partition` node declares, `None` for every other class.
    #[getter]
    fn partition_file_name(&self) -> Option<&str> {
        match self.node() {
            Some(Node::Partition(partition)) => Some(partition.file_name.as_str()),
            _ => None,
        }
    }

    /// The late-load references of the node as tuples of
    /// `(property id, segment GUID, segment type, segment type name, object id)`.
    fn late_loads(&self) -> Vec<(i32, String, i32, &'static str, i32)> {
        self.node()
            .map(|node| {
                node.late_loads()
                    .iter()
                    .map(|load| {
                        (
                            load.property_id,
                            load.guid.to_string(),
                            load.segment_type,
                            segment_type_name(load.segment_type),
                            load.object_id,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Number of child references, so that `len(node)` works.
    fn __len__(&self) -> usize {
        self.child_count()
    }

    /// Child by index, so that `node[i]` works. A reference that does not
    /// resolve to a node is returned as `None`.
    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<Option<Py<JtNode>>> {
        let (Some(graph), Some(node)) = (self.graph(), self.node()) else {
            return Err(PyIndexError::new_err("the node has no child {index}"));
        };
        let children = graph.children_of(node);
        let len = children.len() as isize;
        let slot = if index < 0 { index + len } else { index };
        if slot < 0 || slot >= len {
            return Err(PyIndexError::new_err(format!(
                "the node has no child {index}, it has {len}"
            )));
        }
        match children[slot as usize] {
            Some(pos) => JtNode::new(py, &self.lsg, pos).map(Some),
            None => Ok(None),
        }
    }

    /// Iterate the direct children, unresolved references as `None`.
    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let children = self
            .graph()
            .and_then(|graph| self.node().map(|node| graph.children_of(node)))
            .unwrap_or_default();
        let items = children
            .into_iter()
            .map(|child| match child {
                Some(pos) => JtNode::new(py, &self.lsg, pos).map(Py::into_any),
                None => Ok(py.None()),
            })
            .collect::<PyResult<Vec<_>>>()?;
        // CPython calls `tp_iternext` on whatever `tp_iter` hands back, so the
        // list itself will not do — it has to be its iterator.
        Ok(PyList::new(py, items)?.try_iter()?.into_any().unbind())
    }

    fn __eq__(&self, other: &JtNode) -> bool {
        self.pos == other.pos && self.lsg.as_ptr() == other.lsg.as_ptr()
    }

    fn __hash__(&self) -> isize {
        self.pos as isize
    }

    fn __repr__(&self) -> String {
        format!(
            "JtNode(class={:?}, name={:?}, id={}, children={})",
            self.class_name(),
            self.name(),
            self.id().unwrap_or(-1),
            self.child_count()
        )
    }
}

/// One representation of a JT file: the geometry a shape node late-loads.
///
/// The id is the position of the representation in the file, so every handle
/// for the same representation has the same id and the geometry of an
/// instantiated part is loaded exactly once.
#[pyclass(frozen, name = "JtMesh")]
pub struct JtMesh {
    lsg: Py<Lsg>,
    id: usize,
}

impl JtMesh {
    /// A handle for a representation position of `lsg`.
    pub fn new(py: Python<'_>, lsg: &Py<Lsg>, id: usize) -> PyResult<Py<Self>> {
        Py::new(
            py,
            JtMesh {
                lsg: lsg.clone_ref(py),
                id,
            },
        )
    }

    fn graph(&self) -> Option<&SceneGraph> {
        self.lsg.get().graph()
    }

    fn geometry(&self) -> Option<&LodGeometry> {
        self.graph()?
            .loaded
            .get(self.id)
            .and_then(|stored| stored.element.geometry())
    }
}

#[pymethods]
impl JtMesh {
    /// Id of the representation: its position in the representation list of
    /// the file. Two nodes that share geometry report the same id.
    #[getter]
    fn id(&self) -> usize {
        self.id
    }

    /// Name of the node the representation belongs to, empty when it has none.
    #[getter]
    fn name(&self) -> String {
        let Some(graph) = self.graph() else {
            return String::new();
        };
        graph
            .nodes
            .iter()
            .find(|stored| {
                stored
                    .node
                    .late_loads()
                    .iter()
                    .any(|load| graph.loaded_pos(load) == Some(self.id))
            })
            .map_or_else(String::new, |stored| stored.node.name().to_owned())
    }

    /// Element class of the representation:
    /// `JtElement_ShapeLOD_Vertex` or `JtElement_ShapeLOD_TriStripSet`.
    #[getter]
    fn element_class(&self) -> Option<&'static str> {
        self.graph()?
            .loaded
            .get(self.id)
            .map(|stored| stored.element.class_name())
    }

    /// GUID of the segment the representation was read from.
    #[getter]
    fn segment(&self) -> Option<String> {
        self.graph()?
            .loaded
            .get(self.id)
            .map(|stored| stored.segment.to_string())
    }

    /// Segment type of that segment.
    #[getter]
    fn segment_type(&self) -> Option<i32> {
        self.graph()?
            .loaded
            .get(self.id)
            .map(|stored| stored.segment_type)
    }

    /// Segment type of that segment as a name, `SNCS` for a shape.
    #[getter]
    fn segment_type_name(&self) -> Option<&'static str> {
        self.segment_type().map(segment_type_name)
    }

    /// Number of indices of the representation.
    #[getter]
    fn index_count(&self) -> usize {
        self.geometry().map_or(0, |geometry| geometry.indices.len())
    }

    /// Number of unique vertex positions.
    #[getter]
    fn vertex_count(&self) -> usize {
        self.geometry().map_or(0, |geometry| {
            geometry.vertex_count.max(geometry.vertices.len() / 3)
        })
    }

    /// Number of unique normals.
    #[getter]
    fn normal_count(&self) -> usize {
        self.geometry().map_or(0, |geometry| {
            geometry.normal_count.max(geometry.normals.len() / 3)
        })
    }

    /// Triangles of the representation: the decoder always yields a triangle
    /// list, also for a tri-strip set.
    #[getter]
    fn triangle_count(&self) -> usize {
        self.geometry()
            .map_or(0, |geometry| geometry.indices.len() / 3)
    }

    /// `True` when the representation declares no vertices at all. A JT file
    /// may hold empty shapes — `SampleJT_Server.jt` has 36 of them — and an
    /// export leaves those out because glTF requires positions.
    #[getter]
    fn is_empty(&self) -> bool {
        self.geometry()
            .is_none_or(|geometry| geometry.vertices.is_empty())
    }

    /// The normal binding of a JT 8.x representation, `None` for the newer
    /// topological ones.
    #[getter]
    fn normal_binding(&self) -> Option<u8> {
        self.geometry().and_then(|geometry| geometry.normal_binding)
    }

    /// Names of the array channels of the representation.
    #[getter]
    fn array_names(&self) -> Vec<&'static str> {
        vec!["indices", "vertices", "normals"]
    }

    /// One channel as a [`JtArray`]: `"indices"`, `"vertices"` or `"normals"`.
    fn array(&self, py: Python<'_>, name: &str) -> PyResult<Py<JtArray>> {
        let Some(channel) = Channel::parse(name) else {
            return Err(PyValueError::new_err(format!(
                "unknown array {name:?}, expected one of: indices, vertices, normals"
            )));
        };
        if self.geometry().is_none() {
            return Err(PyValueError::new_err(format!(
                "representation {} has no geometry",
                self.id
            )));
        }
        let owned = Py::new(
            py,
            JtMesh {
                lsg: self.lsg.clone_ref(py),
                id: self.id,
            },
        )?;
        Py::new(
            py,
            JtArray {
                mesh: owned,
                channel,
            },
        )
    }

    /// Every channel as a dict of [`JtArray`]; empty for a representation
    /// without geometry.
    fn arrays<'a>(&self, py: Python<'a>) -> PyResult<Bound<'a, PyDict>> {
        let dict = PyDict::new(py);
        if self.geometry().is_none() {
            return Ok(dict);
        }
        for name in ["indices", "vertices", "normals"] {
            dict.set_item(name, self.array(py, name)?)?;
        }
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!(
            "JtMesh(id={}, name={:?}, indices={}, vertices={}, normals={})",
            self.id,
            self.name(),
            self.index_count(),
            self.vertex_count(),
            self.normal_count()
        )
    }

    fn __eq__(&self, other: &JtMesh) -> bool {
        self.id == other.id && self.lsg.as_ptr() == other.lsg.as_ptr()
    }

    fn __hash__(&self) -> isize {
        self.id as isize
    }
}

/// The array channels of a representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    Indices,
    Vertices,
    Normals,
}

impl Channel {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "indices" | "index" => Some(Channel::Indices),
            "vertices" | "positions" | "vertex" => Some(Channel::Vertices),
            "normals" | "normal" => Some(Channel::Normals),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Channel::Indices => "indices",
            Channel::Vertices => "vertices",
            Channel::Normals => "normals",
        }
    }

    /// `struct` format code of one element.
    fn format(self) -> &'static str {
        match self {
            Channel::Indices => "i",
            Channel::Vertices | Channel::Normals => "f",
        }
    }

    /// Bytes per element of a channel: all three are 32 bit.
    fn itemsize(self) -> usize {
        4
    }
}

/// One array of a representation, exposed through the Python buffer protocol.
///
/// `memoryview` and `numpy.asarray` wrap it **without a copy**: the view
/// points straight at the decoded buffers of the file and keeps this object
/// alive, which in turn keeps the file alive. The view is read-only, because
/// the arrays of a JT file are what the file says they are.
#[pyclass(frozen, name = "JtArray")]
pub struct JtArray {
    mesh: Py<JtMesh>,
    channel: Channel,
}

/// A resolved array: where the bytes are and how they are laid out.
struct Layout {
    data: *const u8,
    bytes: usize,
    itemsize: usize,
    format: &'static str,
    dims: Vec<usize>,
}

#[pymethods]
impl JtArray {
    /// Name of the channel.
    #[getter]
    fn name(&self) -> &'static str {
        self.channel.name()
    }

    /// Id of the representation the channel belongs to.
    #[getter]
    fn mesh_id(&self) -> usize {
        self.mesh.get().id()
    }

    /// Number of dimensions of the array.
    #[getter]
    fn ndim(&self) -> usize {
        self.dims().len()
    }

    /// Shape of the array: `(indices,)` for the indices and `(n, 3)` for the
    /// vertices and normals.
    #[getter]
    fn shape(&self) -> Vec<usize> {
        self.dims()
    }

    /// Bytes per element.
    #[getter]
    fn itemsize(&self) -> usize {
        self.channel.itemsize()
    }

    /// `struct` format code of one element: `"i"` for the indices, `"f"` for
    /// the vertices and normals.
    #[getter]
    fn format(&self) -> &'static str {
        self.channel.format()
    }

    /// Number of rows, so that `len(array)` agrees with `len(memoryview(array))`
    /// and `len(numpy.asarray(array))`: the count of indices for the index
    /// channel, the count of vertices or normals for the others.
    fn __len__(&self) -> usize {
        self.dims().first().copied().unwrap_or(0)
    }

    fn __repr__(&self) -> String {
        format!(
            "JtArray(mesh={}, name={:?}, shape={:?}, format={:?})",
            self.mesh_id(),
            self.channel.name(),
            self.dims(),
            self.channel.format()
        )
    }

    /// Hand out the buffer. The view points at the decoded arrays of the file
    /// and holds a reference to this object, which keeps them alive.
    ///
    /// Only compiled for CPython 3.11 and newer: the stable ABI has no
    /// `bf_getbuffer` slot before then, which is why the crate needs
    /// `pyo3/abi3-py311`.
    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        if view.is_null() {
            return Err(PyBufferError::new_err("the buffer view is null"));
        }
        if flags & ffi::PyBUF_WRITABLE == ffi::PyBUF_WRITABLE {
            return Err(PyBufferError::new_err(
                "the arrays of a JT file are read-only",
            ));
        }
        let Some(layout) = slf.get().layout() else {
            return Err(PyBufferError::new_err("the representation has no geometry"));
        };
        let owner = slf.into_ptr();
        // SAFETY: `view` is a valid `Py_buffer` (checked above); `layout` only
        // carries `Copy` values describing bytes that outlive the view,
        // because `owner` is the owned reference transferred into it and the
        // decoded geometry is never moved once it has been read.
        unsafe { fill_view(view, flags, &layout, owner) };
        Ok(())
    }

    /// Release what [`JtArray::__getbuffer__`] allocated for a view.
    unsafe fn __releasebuffer__(&self, view: *mut ffi::Py_buffer) {
        // SAFETY: `view` was filled by `__getbuffer__`, which stores a leaked
        // `CString` in `format` and a leaked shape/stride block in `internal`.
        unsafe {
            if !(*view).format.is_null() {
                drop(CString::from_raw((*view).format));
                (*view).format = ptr::null_mut();
            }
            if !(*view).internal.is_null() {
                drop(Box::from_raw((*view).internal as *mut [ffi::Py_ssize_t; 4]));
                (*view).internal = ptr::null_mut();
            }
        }
    }
}

impl JtArray {
    /// Shape of the channel, derived from the decoded element counts.
    fn dims(&self) -> Vec<usize> {
        let Some(geometry) = self.mesh.get().geometry() else {
            return Vec::new();
        };
        match self.channel {
            Channel::Indices => vec![geometry.indices.len()],
            Channel::Vertices => shaped(geometry.vertices.len()),
            Channel::Normals => shaped(geometry.normals.len()),
        }
    }

    /// Resolve the channel against the geometry of the representation.
    ///
    /// A representation without vertices (`SampleJT_Server.jt` has 36 of them)
    /// still resolves: it yields a zero-length buffer, so `numpy.asarray` gets
    /// an empty array of the right dtype instead of failing the buffer
    /// protocol, which would make NumPy fall back to a zero-dimensional object
    /// array.
    fn layout(&self) -> Option<Layout> {
        let geometry = self.mesh.get().geometry()?;
        let (values, count) = match self.channel {
            Channel::Indices => (
                geometry.indices.as_ptr() as *const u8,
                geometry.indices.len(),
            ),
            Channel::Vertices => (
                geometry.vertices.as_ptr() as *const u8,
                geometry.vertices.len(),
            ),
            Channel::Normals => (
                geometry.normals.as_ptr() as *const u8,
                geometry.normals.len(),
            ),
        };
        // An empty `Vec` has a null pointer; a buffer must always point at
        // something, even with a length of zero.
        let data = if count == 0 {
            &EMPTY as *const [u8; 4] as *const u8
        } else {
            values
        };
        Some(Layout {
            data,
            bytes: count * self.channel.itemsize(),
            itemsize: self.channel.itemsize(),
            format: self.channel.format(),
            dims: match self.channel {
                Channel::Indices => vec![count],
                Channel::Vertices | Channel::Normals => shaped(count),
            },
        })
    }
}

/// Stand-in address for a zero-length buffer.
static EMPTY: [u8; 4] = [0; 4];

/// A flat channel of `3 * n` floats as `(n, 3)`, flat when the count is not a
/// multiple of three.
fn shaped(count: usize) -> Vec<usize> {
    if count % 3 != 0 {
        return vec![count];
    }
    vec![count / 3, 3]
}

/// Fill a `Py_buffer` from a resolved layout.
///
/// # Safety
///
/// `view` must be a valid, non-null `Py_buffer`. `owner` must be an owned
/// reference that this call transfers into the view, and the bytes `layout`
/// points at must outlive the view.
unsafe fn fill_view(
    view: *mut ffi::Py_buffer,
    flags: c_int,
    layout: &Layout,
    owner: *mut ffi::PyObject,
) {
    let want_nd = flags & ffi::PyBUF_ND == ffi::PyBUF_ND;
    let want_strides = flags & ffi::PyBUF_STRIDES == ffi::PyBUF_STRIDES;
    let want_format = flags & ffi::PyBUF_FORMAT == ffi::PyBUF_FORMAT;

    // The shape and the strides have to stay alive for the whole life of the
    // view, so they go into one block this exporter owns and gives back in
    // `__releasebuffer__`. Layout: `[rows, cols, row stride, item size]`; the
    // shape is read from the first entry and the strides from the third, so a
    // one dimensional channel keeps `cols` at one and its row stride equals
    // the item size.
    let rows = layout.dims.first().copied().unwrap_or(0);
    let cols = layout.dims.get(1).copied().unwrap_or(1);
    let block = Box::into_raw(Box::new([
        rows as ffi::Py_ssize_t,
        cols as ffi::Py_ssize_t,
        (layout.itemsize * cols) as ffi::Py_ssize_t,
        layout.itemsize as ffi::Py_ssize_t,
    ]));
    let sizes = block as *mut ffi::Py_ssize_t;

    // SAFETY: `view` is valid, `sizes` points inside the leaked block above,
    // and `owner` is the reference this call is required to transfer.
    unsafe {
        (*view).buf = layout.data as *mut c_void;
        (*view).obj = owner;
        (*view).len = layout.bytes as ffi::Py_ssize_t;
        (*view).itemsize = layout.itemsize as ffi::Py_ssize_t;
        (*view).readonly = 1;
        (*view).ndim = if want_nd {
            layout.dims.len() as c_int
        } else {
            1
        };
        (*view).format = if want_format {
            CString::new(layout.format)
                .unwrap_or_else(|_| CString::new("").expect("the empty string has no NUL"))
                .into_raw()
        } else {
            ptr::null_mut()
        };
        (*view).shape = if want_nd { sizes } else { ptr::null_mut() };
        // Only the first entry of a strides array is read for a C ordered
        // array, which is what the channels are, so the row stride goes first.
        (*view).strides = if want_strides {
            sizes.add(2)
        } else {
            ptr::null_mut()
        };
        (*view).suboffsets = ptr::null_mut();
        (*view).internal = block as *mut c_void;
    }
}
