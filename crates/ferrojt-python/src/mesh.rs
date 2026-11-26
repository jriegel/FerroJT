//! One merged mesh of a model, handed to Python as raw buffers.
//!
//! [`Model::flatten`](ferrojt::Model::flatten) collapses a whole file into a
//! single mesh: every placement of every shape is baked into the vertex
//! coordinates, so an instanced part appears once per placement. This module
//! exposes exactly that — the three channels of the merged mesh — as Python
//! objects that `memoryview` and NumPy wrap **without a copy**, which is what
//! makes a merged mesh of a million triangles usable:
//!
//! ```python
//! import ferrojt
//!
//! mesh = ferrojt.read_mesh("gear.jt")
//! mesh.vertex_count, mesh.triangle_count
//! numpy.asarray(mesh.vertices)   # (n, 3) float32, shares memory
//! numpy.asarray(mesh.indices)    # (m,) int32, shares memory
//! ```
//!
//! The merge itself does one copy — baking the transforms into new coordinate
//! arrays is the whole point of flattening — but handing those arrays out costs
//! nothing, and a view keeps the mesh alive for as long as it is used.

use std::ffi::c_int;

use pyo3::exceptions::PyValueError;
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use ferrojt::model::{Aabb, Flattened, Material};

use crate::array::{self, Channel};

/// One material group of a merged mesh: which colour it carries and how much
/// of the mesh it is.
struct Group {
    material: Option<String>,
    color: [f32; 4],
    metallic: f32,
    roughness: f32,
    emissive: [f32; 3],
    vertex_count: usize,
    triangle_count: usize,
}

/// The single mesh a whole file merges into, as raw vertex, normal and index
/// arrays.
///
/// The arrays are exactly what [`ferrojt::Model::flatten`] produced:
///
/// * `vertices` — `(n, 3)` `float32`, already in world space
/// * `normals` — `(n, 3)` `float32`, empty when nothing in the file carried
///   any, and computed for the shapes that lacked them otherwise
/// * `indices` — `(m,)` `int32`, three per triangle
#[pyclass(frozen, name = "MeshBuffer")]
pub struct MeshBuffer {
    vertices: Vec<f32>,
    normals: Vec<f32>,
    indices: Vec<i32>,
    groups: Vec<Group>,
    bounds: Option<Aabb>,
}

impl MeshBuffer {
    /// Take the merged geometry out of a [`Flattened`], naming each group by
    /// the material of the model it came from.
    pub(crate) fn new(flat: Flattened, materials: &[Material]) -> Self {
        let groups = flat
            .groups
            .iter()
            .map(|group| {
                let material = group.material.and_then(|index| materials.get(index));
                Group {
                    material: material.and_then(|material| material.name.clone()),
                    color: material.map_or([1.0, 1.0, 1.0, 1.0], |m| m.base_color.factor),
                    metallic: material.map_or(0.0, |m| m.metallic.factor[0]),
                    roughness: material.map_or(0.5, |m| m.roughness.factor[0]),
                    emissive: [
                        material.map_or(0.0, |m| m.emissive.factor[0]),
                        material.map_or(0.0, |m| m.emissive.factor[1]),
                        material.map_or(0.0, |m| m.emissive.factor[2]),
                    ],
                    vertex_count: group.vertex_count,
                    triangle_count: group.index_count / 3,
                }
            })
            .collect();
        MeshBuffer {
            vertices: flat.vertices,
            normals: flat.normals,
            indices: flat.indices.into_iter().map(|i| i as i32).collect(),
            groups,
            bounds: flat.bounds,
        }
    }

    /// One channel of the merged mesh as a resolved layout.
    fn layout(&self, channel: Channel) -> array::Layout {
        match channel {
            Channel::Indices => array::Layout::of(&self.indices, channel),
            Channel::Vertices => array::Layout::of(&self.vertices, channel),
            Channel::Normals => array::Layout::of(&self.normals, channel),
        }
    }

    /// One channel of this same Python object as a `MeshArray`.
    ///
    /// The channel arrays never copy: they hold a reference to the very
    /// `MeshBuffer` whose arrays they point at, so the buffers cannot move
    /// while a view lives.
    fn channel(slf: Py<Self>, py: Python<'_>, channel: Channel) -> PyResult<Py<MeshArray>> {
        Py::new(
            py,
            MeshArray {
                mesh: slf.clone_ref(py),
                channel,
            },
        )
    }
}

#[pymethods]
impl MeshBuffer {
    /// Rows of the vertex array.
    #[getter]
    fn vertex_count(&self) -> usize {
        self.vertices.len() / 3
    }

    /// Number of indices; three per triangle.
    #[getter]
    fn index_count(&self) -> usize {
        self.indices.len()
    }

    /// Number of triangles in the merged mesh.
    #[getter]
    fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// `True` when the merged mesh carries normals.
    #[getter]
    fn has_normals(&self) -> bool {
        !self.normals.is_empty()
    }

    /// Number of material groups of the merged mesh.
    #[getter]
    fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// The material groups in order, each a dict with `"material"` (a name or
    /// `None`), `"color"` (RGBA), `"metallic"`, `"roughness"`, `"emissive"`
    /// (RGB), `"vertices"` and `"triangles"`. Consecutive placements that share
    /// a material are one group, so a file with two colours has two groups no
    /// matter how many parts it has.
    fn groups<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for group in &self.groups {
            let item = PyDict::new(py);
            item.set_item("material", group.material.clone())?;
            item.set_item("color", group.color)?;
            item.set_item("metallic", group.metallic)?;
            item.set_item("roughness", group.roughness)?;
            item.set_item("emissive", group.emissive)?;
            item.set_item("vertices", group.vertex_count)?;
            item.set_item("triangles", group.triangle_count)?;
            list.append(item)?;
        }
        Ok(list)
    }

    /// Names of the materials of the groups, in group order; `None` for an
    /// unshaded group.
    #[getter]
    fn material_names(&self) -> Vec<Option<String>> {
        self.groups
            .iter()
            .map(|group| group.material.clone())
            .collect()
    }

    /// World space bounds of the merged mesh as `[min_x, min_y, min_z,
    /// max_x, max_y, max_z]`, or `None` when the file had no geometry.
    #[getter]
    fn bounds(&self) -> Option<[f32; 6]> {
        self.bounds.map(|box_| {
            [
                box_.min[0],
                box_.min[1],
                box_.min[2],
                box_.max[0],
                box_.max[1],
                box_.max[2],
            ]
        })
    }

    /// One channel as a buffer: `"indices"`, `"vertices"` or `"normals"`.
    fn array(slf: Py<Self>, py: Python<'_>, name: &str) -> PyResult<Py<MeshArray>> {
        let Some(channel) = Channel::parse(name) else {
            return Err(PyValueError::new_err(format!(
                "unknown channel {name:?}: use one of 'indices', 'vertices', 'normals'"
            )));
        };
        MeshBuffer::channel(slf, py, channel)
    }

    /// All three channels as a dict, keyed by channel name.
    fn arrays(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let dict = PyDict::new(py);
        for channel in Channel::all() {
            dict.set_item(
                channel.name(),
                MeshBuffer::channel(slf.clone_ref(py), py, channel)?,
            )?;
        }
        Ok(dict.unbind())
    }

    /// The vertex array: `(n, 3)` `float32` in world space.
    #[getter]
    fn vertices(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<MeshArray>> {
        MeshBuffer::channel(slf, py, Channel::Vertices)
    }

    /// The normal array: `(n, 3)` `float32`; empty when nothing carried normals.
    #[getter]
    fn normals(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<MeshArray>> {
        MeshBuffer::channel(slf, py, Channel::Normals)
    }

    /// The index array: `(m,)` `int32`.
    #[getter]
    fn indices(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<MeshArray>> {
        MeshBuffer::channel(slf, py, Channel::Indices)
    }

    fn __repr__(&self) -> String {
        format!(
            "MeshBuffer(vertices={}, triangles={}, groups={}, normals={})",
            self.vertex_count(),
            self.triangle_count(),
            self.group_count(),
            self.has_normals()
        )
    }
}

/// One channel of a merged mesh, exposed through the Python buffer protocol.
///
/// `numpy.asarray` wraps it without a copy and keeps the mesh alive for as
/// long as the resulting array exists.
#[pyclass(frozen, name = "MeshArray")]
pub struct MeshArray {
    mesh: Py<MeshBuffer>,
    channel: Channel,
}

#[pymethods]
impl MeshArray {
    /// Name of the channel.
    #[getter]
    fn name(&self) -> &'static str {
        self.channel.name()
    }

    /// Number of dimensions of the array.
    #[getter]
    fn ndim(&self) -> usize {
        self.dims().len()
    }

    /// Shape of the array: `(m,)` for the indices and `(n, 3)` for the
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

    /// Number of rows, so that `len(a)` agrees with `len(memoryview(a))` and
    /// `len(numpy.asarray(a))`.
    fn __len__(&self) -> usize {
        self.dims().first().copied().unwrap_or(0)
    }

    fn __repr__(&self) -> String {
        format!(
            "MeshArray(name={:?}, shape={:?}, format={:?})",
            self.channel.name(),
            self.dims(),
            self.channel.format()
        )
    }

    /// Hand out the buffer. The view points at the arrays of the merged mesh
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
        let layout = slf.get().layout();
        unsafe { array::get_buffer(slf, view, flags, Some(layout), "a merged mesh is read-only") }
    }

    /// Release what [`MeshArray::__getbuffer__`] allocated for a view.
    unsafe fn __releasebuffer__(&self, view: *mut ffi::Py_buffer) {
        unsafe { array::release_view(view) };
    }
}

impl MeshArray {
    fn dims(&self) -> Vec<usize> {
        self.mesh.get().layout(self.channel).dims
    }

    fn layout(&self) -> array::Layout {
        self.mesh.get().layout(self.channel)
    }
}
