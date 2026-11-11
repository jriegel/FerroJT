//! Building a JT file from nothing: the Python view of [`ferrojt::jt::JtBuilder`].
//!
//! [`Builder`] creates an empty file and fills its scene graph with nodes,
//! meshes and instances, then writes the result as a JT file — JT 10.3 unless
//! [`Builder.version`] asks for another revision. It is the mirror of the
//! read-only [`crate::lsg`] view: the handles here address slots of the graph
//! being *built*, not positions of a file that was read.

use std::sync::atomic::{AtomicUsize, Ordering};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use ferrojt::jt::{
    BuildNode as RustClass, JtBuilder as RustBuilder, JtMesh as MeshId, JtNode as NodeId,
    JtVersion as RustVersion, MaterialData as RustMaterial, MeshData as RustMesh,
};

use crate::py_err;

/// The id handed to the next builder, so a handle can tell whose it is.
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

/// The node class named by `name`, spelled as in JT.
fn class_of(name: &str) -> PyResult<RustClass> {
    match name.to_ascii_lowercase().replace('_', "").as_str() {
        "partition" => Ok(RustClass::Partition),
        "part" => Ok(RustClass::Part),
        "metadata" => Ok(RustClass::MetaData),
        "group" => Ok(RustClass::Group),
        "shape" => Ok(RustClass::Shape),
        "instance" => Ok(RustClass::Instance),
        other => Err(PyValueError::new_err(format!(
            "unknown node class {other:?}; use Partition, Part, MetaData, Group, Shape or \
             Instance"
        ))),
    }
}

/// The JT name of a node class, the spelling `JtNode.class_name` reports.
fn class_name(class: RustClass) -> &'static str {
    match class {
        RustClass::Partition => "Partition",
        RustClass::Part => "Part",
        RustClass::MetaData => "MetaData",
        RustClass::Group => "Group",
        RustClass::Shape => "Shape",
        RustClass::Instance => "Instance",
    }
}

/// Turn a flat list of floats into a fixed size array.
fn array<const N: usize>(values: Vec<f32>, what: &str) -> PyResult<[f32; N]> {
    if values.len() != N {
        return Err(PyValueError::new_err(format!(
            "{what} needs {N} values, got {}",
            values.len()
        )));
    }
    let mut out = [0.0f32; N];
    out.copy_from_slice(&values);
    Ok(out)
}

/// A triangle mesh to put into a JT file.
///
/// `vertices` is a flat list of triples and `indices` a flat list of triangle
/// corners. Without `normals` the normal of each face is computed.
#[pyclass(name = "MeshData")]
#[derive(Clone, Debug)]
pub struct MeshData {
    inner: RustMesh,
}

#[pymethods]
impl MeshData {
    /// Create a mesh from flat `vertices`, flat triangle `indices` and optional
    /// flat `normals`.
    #[new]
    #[pyo3(signature = (vertices = Vec::new(), indices = Vec::new(), normals = None))]
    fn new(vertices: Vec<f32>, indices: Vec<i32>, normals: Option<Vec<f32>>) -> PyResult<Self> {
        let inner = match normals {
            Some(normals) => RustMesh::with_normals(vertices, normals, indices),
            None => RustMesh::new(vertices, indices),
        };
        inner.map(|inner| Self { inner }).map_err(py_err)
    }

    /// The vertex positions as a flat list of triples.
    #[getter]
    fn vertices(&self) -> Vec<f32> {
        self.inner.vertices().to_vec()
    }

    /// The vertex normals as a flat list of triples.
    #[getter]
    fn normals(&self) -> Vec<f32> {
        self.inner.normals().to_vec()
    }

    /// The triangle indices as a flat list of triples.
    #[getter]
    fn indices(&self) -> Vec<i32> {
        self.inner.indices().to_vec()
    }

    /// The number of vertex slots.
    #[getter]
    fn vertex_count(&self) -> usize {
        self.inner.vertices().len() / 3
    }

    /// The number of triangles.
    #[getter]
    fn triangle_count(&self) -> usize {
        self.inner.triangle_count()
    }

    /// The bounding box as `(min_x, min_y, min_z, max_x, max_y, max_z)`.
    #[getter]
    fn bounds(&self) -> Option<Vec<f32>> {
        self.inner.bounds().map(|bounds| bounds.to_vec())
    }

    /// Check that the mesh can be written.
    fn validate(&self) -> PyResult<()> {
        self.inner.validate().map_err(py_err)
    }

    fn __repr__(&self) -> String {
        format!(
            "MeshData(vertices={}, triangles={})",
            self.vertex_count(),
            self.triangle_count()
        )
    }
}

/// The shading of a node, written as a JT material attribute.
#[pyclass(name = "MaterialData")]
#[derive(Clone, Copy, Debug)]
pub struct MaterialData {
    inner: RustMaterial,
}

#[pymethods]
impl MaterialData {
    /// Create a material; every channel is optional and keeps its JT default.
    #[new]
    #[pyo3(signature = (ambient = None, diffuse = None, specular = None, emission = None,
                        shininess = None, reflectivity = None))]
    fn new(
        ambient: Option<Vec<f32>>,
        diffuse: Option<Vec<f32>>,
        specular: Option<Vec<f32>>,
        emission: Option<Vec<f32>>,
        shininess: Option<f32>,
        reflectivity: Option<f32>,
    ) -> PyResult<Self> {
        let mut inner = RustMaterial::default();
        if let Some(value) = ambient {
            inner.ambient = array(value, "ambient")?;
        }
        if let Some(value) = diffuse {
            inner.diffuse = array(value, "diffuse")?;
        }
        if let Some(value) = specular {
            inner.specular = array(value, "specular")?;
        }
        if let Some(value) = emission {
            inner.emission = array(value, "emission")?;
        }
        if let Some(value) = shininess {
            inner.shininess = value;
        }
        if let Some(value) = reflectivity {
            inner.reflectivity = value;
        }
        Ok(Self { inner })
    }

    /// The ambient colour as `(r, g, b, a)`.
    #[getter]
    fn ambient(&self) -> Vec<f32> {
        self.inner.ambient.to_vec()
    }

    #[setter]
    fn set_ambient(&mut self, value: Vec<f32>) -> PyResult<()> {
        self.inner.ambient = array(value, "ambient")?;
        Ok(())
    }

    /// The diffuse colour as `(r, g, b, a)`.
    #[getter]
    fn diffuse(&self) -> Vec<f32> {
        self.inner.diffuse.to_vec()
    }

    #[setter]
    fn set_diffuse(&mut self, value: Vec<f32>) -> PyResult<()> {
        self.inner.diffuse = array(value, "diffuse")?;
        Ok(())
    }

    /// The specular colour as `(r, g, b, a)`.
    #[getter]
    fn specular(&self) -> Vec<f32> {
        self.inner.specular.to_vec()
    }

    #[setter]
    fn set_specular(&mut self, value: Vec<f32>) -> PyResult<()> {
        self.inner.specular = array(value, "specular")?;
        Ok(())
    }

    /// The emissive colour as `(r, g, b, a)`.
    #[getter]
    fn emission(&self) -> Vec<f32> {
        self.inner.emission.to_vec()
    }

    #[setter]
    fn set_emission(&mut self, value: Vec<f32>) -> PyResult<()> {
        self.inner.emission = array(value, "emission")?;
        Ok(())
    }

    /// The shininess exponent.
    #[getter]
    fn shininess(&self) -> f32 {
        self.inner.shininess
    }

    #[setter]
    fn set_shininess(&mut self, value: f32) {
        self.inner.shininess = value;
    }

    /// The reflectivity.
    #[getter]
    fn reflectivity(&self) -> f32 {
        self.inner.reflectivity
    }

    #[setter]
    fn set_reflectivity(&mut self, value: f32) {
        self.inner.reflectivity = value;
    }

    fn __repr__(&self) -> String {
        format!(
            "MaterialData(diffuse=({}, {}, {}, {}), shininess={})",
            self.inner.diffuse[0],
            self.inner.diffuse[1],
            self.inner.diffuse[2],
            self.inner.diffuse[3],
            self.inner.shininess,
        )
    }
}

/// A node of a graph under construction.
///
/// A handle is a token: it names one node of one [`Builder`], and every
/// operation on it goes back through that builder.
#[pyclass(frozen, name = "NodeHandle")]
pub struct NodeHandle {
    builder: Py<Builder>,
    owner: usize,
    node: NodeId,
}

impl NodeHandle {
    /// A handle for `node` of the builder that carries `owner`.
    fn new(
        py: Python<'_>,
        builder: &Py<Builder>,
        owner: usize,
        node: NodeId,
    ) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                builder: builder.clone_ref(py),
                owner,
                node,
            },
        )
    }

    /// The builder this handle was made by, borrowed immutably.
    fn borrowed<'py>(&self, py: Python<'py>) -> PyResult<PyRef<'py, Builder>> {
        self.builder.bind(py).try_borrow().map_err(Into::into)
    }
}

#[pymethods]
impl NodeHandle {
    /// The position of this node in the graph being built.
    #[getter]
    fn index(&self) -> usize {
        self.node.index()
    }

    /// The JT class name of this node.
    #[getter]
    fn class_name(&self, py: Python<'_>) -> PyResult<Option<&'static str>> {
        let builder = self.borrowed(py)?;
        builder.owned(self)?;
        Ok(builder.class_name_of(self.node))
    }

    /// The name of this node.
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let builder = self.borrowed(py)?;
        builder.owned(self)?;
        Ok(builder.inner.name(self.node).map(str::to_owned))
    }

    /// The children of this node, in the order they were added.
    fn children(&self, py: Python<'_>) -> PyResult<Vec<Py<NodeHandle>>> {
        let children = {
            let builder = self.borrowed(py)?;
            builder.owned(self)?;
            builder.inner.children(self.node)
        };
        children
            .into_iter()
            .map(|child| NodeHandle::new(py, &self.builder, self.owner, child))
            .collect()
    }

    /// The node this instance refers to.
    fn instance_target(&self, py: Python<'_>) -> PyResult<Option<Py<NodeHandle>>> {
        let target = {
            let builder = self.borrowed(py)?;
            builder.owned(self)?;
            builder.inner.instance_target(self.node)
        };
        target
            .map(|target| NodeHandle::new(py, &self.builder, self.owner, target))
            .transpose()
    }

    fn __eq__(&self, other: &Self) -> bool {
        self.node == other.node && self.builder.is(&other.builder)
    }

    fn __hash__(&self) -> u64 {
        self.node.index() as u64
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let builder = match self.borrowed(py) {
            Ok(builder) => builder,
            Err(_) => return format!("NodeHandle(#{})", self.node.index()),
        };
        let class = builder.class_name_of(self.node).unwrap_or("?");
        let index = self.node.index();
        match builder.inner.name(self.node) {
            Some(name) => format!("NodeHandle(#{index}, {class}, {name:?})"),
            None => format!("NodeHandle(#{index}, {class})"),
        }
    }
}

/// A mesh of a graph under construction.
#[pyclass(frozen, name = "MeshHandle")]
pub struct MeshHandle {
    builder: Py<Builder>,
    owner: usize,
    mesh: MeshId,
}

impl MeshHandle {
    /// A handle for mesh `mesh` of the builder that carries `owner`.
    fn new(
        py: Python<'_>,
        builder: &Py<Builder>,
        owner: usize,
        mesh: MeshId,
    ) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                builder: builder.clone_ref(py),
                owner,
                mesh,
            },
        )
    }

    /// The builder this handle was made by, borrowed immutably.
    fn borrowed<'py>(&self, py: Python<'py>) -> PyResult<PyRef<'py, Builder>> {
        self.builder.bind(py).try_borrow().map_err(Into::into)
    }
}

#[pymethods]
impl MeshHandle {
    /// The position of this mesh in the file being built.
    #[getter]
    fn index(&self) -> usize {
        self.mesh.index()
    }

    /// The number of vertex slots of this mesh.
    #[getter]
    fn vertex_count(&self, py: Python<'_>) -> PyResult<usize> {
        let builder = self.borrowed(py)?;
        builder.owned_mesh(self)?;
        let mesh = builder.inner.mesh(self.mesh);
        Ok(mesh.map_or(0, |mesh| mesh.vertices().len() / 3))
    }

    /// The number of triangles of this mesh.
    #[getter]
    fn triangle_count(&self, py: Python<'_>) -> PyResult<usize> {
        let builder = self.borrowed(py)?;
        builder.owned_mesh(self)?;
        let mesh = builder.inner.mesh(self.mesh);
        Ok(mesh.map_or(0, RustMesh::triangle_count))
    }

    fn __eq__(&self, other: &Self) -> bool {
        self.mesh == other.mesh && self.builder.is(&other.builder)
    }

    fn __hash__(&self) -> u64 {
        self.mesh.index() as u64
    }

    fn __repr__(&self) -> String {
        format!("MeshHandle(#{})", self.mesh.index())
    }
}

/// A new JT file: an empty scene graph that can be filled and written.
///
/// The graph starts as a root `Partition`; add nodes with [`Builder::add_node`],
/// shapes with [`Builder::add_shape`], and share a subtree with
/// [`Builder::instance_of`]. [`Builder::save`] writes the file at the revision
/// of [`Builder.version`], which is JT 10.3 until it is changed.
///
/// JT 9.x and 10.x store their triangles in the topologically compressed
/// representation, which only accepts a closed two-manifold; JT 8.x stores them
/// in the vertex based one and accepts anything. See [`Builder.version`].
#[pyclass(name = "JtBuilder")]
pub struct Builder {
    inner: RustBuilder,
    id: usize,
}

impl Builder {
    /// Reject a node handle that belongs to another builder.
    fn owned(&self, node: &NodeHandle) -> PyResult<()> {
        if node.owner == self.id {
            Ok(())
        } else {
            Err(PyValueError::new_err(
                "that node belongs to a different builder",
            ))
        }
    }

    /// Reject a mesh handle that belongs to another builder.
    fn owned_mesh(&self, mesh: &MeshHandle) -> PyResult<()> {
        if mesh.owner == self.id {
            Ok(())
        } else {
            Err(PyValueError::new_err(
                "that mesh belongs to a different builder",
            ))
        }
    }

    /// The JT class name of a node of this builder.
    fn class_name_of(&self, node: NodeId) -> Option<&'static str> {
        self.inner.class(node).map(class_name)
    }
}

#[pymethods]
impl Builder {
    /// Create a builder with an empty scene graph.
    #[new]
    fn new() -> Self {
        Self {
            inner: RustBuilder::new(),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The root partition of the graph.
    fn root(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<NodeHandle>> {
        let (root, id) = {
            let builder = slf.bind(py).borrow();
            (builder.inner.root(), builder.id)
        };
        NodeHandle::new(py, &slf, id, root)
    }

    /// The number of nodes added so far.
    #[getter]
    fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// The number of meshes added so far.
    #[getter]
    fn mesh_count(&self) -> usize {
        self.inner.mesh_count()
    }

    /// Whether the scene graph segment is DEFLATE compressed. On by default.
    #[getter]
    fn compress(&self) -> bool {
        self.inner.compress()
    }

    #[setter]
    fn set_compress(&mut self, compress: bool) {
        let _ = self.inner.set_compress(compress);
    }

    /// The JT revision the file is written at, as `"major.minor"`.
    ///
    /// JT 10.3 until [`Builder.set_version`] says otherwise.
    #[getter]
    fn version(&self) -> String {
        self.inner.version().to_string()
    }

    /// Write the file at revision `value`: `"8.0"`, `"8.1"`, `"9.5"` or
    /// `"10.3"`, or a bare major to take the default minor of that revision.
    ///
    /// A mesh of a 9.x or 10.x file has to be a closed two-manifold, since that
    /// is the only thing the topologically compressed coder accepts; a mesh
    /// that is not can still be written as 8.x.
    #[pyo3(name = "set_version")]
    fn set_version_name(&mut self, value: &str) -> PyResult<()> {
        let version = value
            .parse::<RustVersion>()
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        self.inner.set_version(version).map_err(py_err)?;
        Ok(())
    }

    /// Setter of [`Builder.version`], so `builder.version = "9.5"` works.
    #[setter]
    fn set_version(&mut self, value: &str) -> PyResult<()> {
        self.set_version_name(value)
    }

    /// Add a node of `class_name` under `parent` and return its handle.
    ///
    /// `class_name` is a JT class name: `Partition`, `Part`, `MetaData`,
    /// `Group`, `Shape` or `Instance`. A `Shape` or `Instance` node cannot have
    /// children.
    fn add_node(
        slf: Py<Self>,
        py: Python<'_>,
        parent: &NodeHandle,
        class_name: &str,
        name: Option<&str>,
    ) -> PyResult<Py<NodeHandle>> {
        let class = class_of(class_name)?;
        let (node, id) = {
            let mut builder = slf.bind(py).borrow_mut();
            builder.owned(parent)?;
            let node = builder
                .inner
                .add_node(parent.node, class, name)
                .map_err(py_err)?;
            (node, builder.id)
        };
        NodeHandle::new(py, &slf, id, node)
    }

    /// Add a shape node holding `mesh` under `parent`.
    #[pyo3(signature = (parent, mesh, *, name = None))]
    fn add_shape(
        slf: Py<Self>,
        py: Python<'_>,
        parent: &NodeHandle,
        mesh: &MeshData,
        name: Option<&str>,
    ) -> PyResult<Py<NodeHandle>> {
        let (node, id) = {
            let mut builder = slf.bind(py).borrow_mut();
            builder.owned(parent)?;
            let node = builder
                .inner
                .add_shape(parent.node, name, mesh.inner.clone())
                .map_err(py_err)?;
            (node, builder.id)
        };
        NodeHandle::new(py, &slf, id, node)
    }

    /// Add an instance of `target` under `parent`.
    ///
    /// The instance shares the subtree of its target instead of copying it, so
    /// a mesh reached through two instances stays one representation.
    fn instance_of(
        slf: Py<Self>,
        py: Python<'_>,
        parent: &NodeHandle,
        target: &NodeHandle,
    ) -> PyResult<Py<NodeHandle>> {
        let (node, id) = {
            let mut builder = slf.bind(py).borrow_mut();
            builder.owned(parent)?;
            builder.owned(target)?;
            let node = builder
                .inner
                .instance_of(parent.node, target.node)
                .map_err(py_err)?;
            (node, builder.id)
        };
        NodeHandle::new(py, &slf, id, node)
    }

    /// Add a mesh to the file without attaching it to a node.
    fn add_mesh(slf: Py<Self>, py: Python<'_>, mesh: &MeshData) -> PyResult<Py<MeshHandle>> {
        let (handle, id) = {
            let mut builder = slf.bind(py).borrow_mut();
            let handle = builder.inner.add_mesh(mesh.inner.clone());
            (handle, builder.id)
        };
        MeshHandle::new(py, &slf, id, handle)
    }

    /// Attach an existing mesh to a shape node.
    fn set_mesh(&mut self, node: &NodeHandle, mesh: &MeshHandle) -> PyResult<()> {
        self.owned(node)?;
        self.owned_mesh(mesh)?;
        let _ = self.inner.set_mesh(node.node, mesh.mesh).map_err(py_err)?;
        Ok(())
    }

    /// Set the transform matrix of a node, column major, 16 values.
    ///
    /// Element (row, column) is at index `4 * column + row` and the translation
    /// is the last column, which is how JT stores a matrix.
    fn set_transform(&mut self, node: &NodeHandle, matrix: Vec<f64>) -> PyResult<()> {
        self.owned(node)?;
        if matrix.len() != 16 {
            return Err(PyValueError::new_err(format!(
                "a matrix needs 16 values, got {}",
                matrix.len()
            )));
        }
        let mut out = [0.0f64; 16];
        out.copy_from_slice(&matrix);
        let _ = self.inner.set_transform(node.node, &out).map_err(py_err)?;
        Ok(())
    }

    /// Set a translation only.
    fn set_translation(&mut self, node: &NodeHandle, translation: Vec<f32>) -> PyResult<()> {
        self.owned(node)?;
        let translation = array(translation, "a translation")?;
        let _ = self
            .inner
            .set_translation(node.node, translation)
            .map_err(py_err)?;
        Ok(())
    }

    /// Set a translation, a rotation quaternion `(x, y, z, w)` and a scale.
    fn set_placement(
        &mut self,
        node: &NodeHandle,
        translation: Vec<f32>,
        rotation: Vec<f32>,
        scale: Vec<f32>,
    ) -> PyResult<()> {
        self.owned(node)?;
        let translation = array(translation, "a translation")?;
        let rotation = array(rotation, "a rotation")?;
        let scale = array(scale, "a scale")?;
        let _ = self
            .inner
            .set_placement(node.node, translation, rotation, scale)
            .map_err(py_err)?;
        Ok(())
    }

    /// Set the material of a node.
    fn set_material(&mut self, node: &NodeHandle, material: &MaterialData) -> PyResult<()> {
        self.owned(node)?;
        let _ = self
            .inner
            .set_material(node.node, material.inner)
            .map_err(py_err)?;
        Ok(())
    }

    /// Set the name of a node.
    fn set_name(&mut self, node: &NodeHandle, name: &str) -> PyResult<()> {
        self.owned(node)?;
        let _ = self.inner.set_name(node.node, name).map_err(py_err)?;
        Ok(())
    }

    /// The children of a node, in the order they were added.
    fn children(slf: Py<Self>, py: Python<'_>, node: &NodeHandle) -> PyResult<Vec<Py<NodeHandle>>> {
        let (children, owner) = {
            let builder = slf.bind(py).borrow();
            builder.owned(node)?;
            (builder.inner.children(node.node), builder.id)
        };
        children
            .into_iter()
            .map(|child| NodeHandle::new(py, &slf, owner, child))
            .collect()
    }

    /// The JT class name of a node.
    #[pyo3(name = "class_of")]
    fn class(&self, node: &NodeHandle) -> PyResult<Option<&'static str>> {
        self.owned(node)?;
        Ok(self.class_name_of(node.node))
    }

    /// The name of a node.
    fn name(&self, node: &NodeHandle) -> PyResult<Option<&str>> {
        self.owned(node)?;
        Ok(self.inner.name(node.node))
    }

    /// The node an instance refers to.
    fn instance_target(
        slf: Py<Self>,
        py: Python<'_>,
        node: &NodeHandle,
    ) -> PyResult<Option<Py<NodeHandle>>> {
        let (target, owner) = {
            let builder = slf.bind(py).borrow();
            builder.owned(node)?;
            (builder.inner.instance_target(node.node), builder.id)
        };
        target
            .map(|target| NodeHandle::new(py, &slf, owner, target))
            .transpose()
    }

    /// The bytes of the JT file.
    fn to_bytes(&self) -> PyResult<Py<PyBytes>> {
        let bytes = self.inner.write_to_bytes().map_err(py_err)?;
        Python::attach(|py| Ok(PyBytes::new(py, &bytes).unbind()))
    }

    /// Write the JT file to `path`.
    fn save(&self, path: &str) -> PyResult<()> {
        self.inner.write_to_file(path).map_err(py_err)
    }

    fn __repr__(&self) -> String {
        format!(
            "JtBuilder(nodes={}, meshes={})",
            self.inner.node_count(),
            self.inner.mesh_count(),
        )
    }
}
