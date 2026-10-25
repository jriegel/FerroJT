//! Python bindings for FerroJT.
//!
//! The compiled extension module is named `_ferrojt`; the pure Python package
//! `ferrojt` re-exports it and adds the ergonomic helpers.

use pyo3::exceptions::{PyNotImplementedError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// Convert a FerroJT error into a Python exception.
fn py_err(err: ferrojt::Error) -> PyErr {
    match err {
        ferrojt::Error::NotImplemented(what) => PyNotImplementedError::new_err(what),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Open a CAD file and return a [`Model`].
#[pyfunction]
fn open(path: &str) -> PyResult<Model> {
    let model = ferrojt::open(path).map_err(py_err)?;
    Ok(Model { inner: model })
}

/// Return the container format detected for `path`.
#[pyfunction]
fn detect_format(path: &str) -> PyResult<String> {
    Ok(ferrojt::Format::detect(path).to_string())
}

/// A loaded scene.
#[pyclass(name = "Model")]
pub struct Model {
    inner: ferrojt::Model,
}

#[pymethods]
impl Model {
    /// Number of scene graph nodes.
    #[getter]
    fn node_count(&self) -> usize {
        self.inner.nodes.len()
    }

    /// Number of meshes.
    #[getter]
    fn mesh_count(&self) -> usize {
        self.inner.meshes.len()
    }

    /// Number of materials.
    #[getter]
    fn material_count(&self) -> usize {
        self.inner.materials.len()
    }

    /// Total vertices across all primitives.
    #[getter]
    fn vertex_count(&self) -> usize {
        self.inner.vertex_count()
    }

    /// Total triangles across all primitives.
    #[getter]
    fn triangle_count(&self) -> usize {
        self.inner.triangle_count()
    }

    /// Node names, in scene order.
    fn node_names(&self) -> Vec<String> {
        self.inner
            .nodes
            .iter()
            .map(|n| n.name.clone().unwrap_or_default())
            .collect()
    }

    /// Serialize the model to `format` (`"gltf"`, `"glb"` or `"usd"`) and
    /// return the bytes.
    fn to_bytes(&self, format: &str) -> PyResult<Py<PyBytes>> {
        let format = format.parse::<ferrojt::Format>().map_err(py_err)?;
        let bytes = ferrojt::export::to_format(format, &self.inner).map_err(py_err)?;
        Python::attach(|py| Ok(PyBytes::new(py, &bytes).unbind()))
    }

    /// Serialize the model to `format` and write it to `path`.
    fn save(&self, path: &str, format: Option<&str>) -> PyResult<()> {
        let format = match format {
            Some(f) => f.parse::<ferrojt::Format>().map_err(py_err)?,
            None => ferrojt::Format::detect(path),
        };
        ferrojt::export::write_to_file(format, &self.inner, path).map_err(py_err)
    }

    /// A JSON summary of the model, useful for debugging.
    fn to_json(&self) -> PyResult<String> {
        serde_json_summary(&self.inner).map_err(py_err)
    }

    fn __repr__(&self) -> String {
        format!(
            "Model(nodes={}, meshes={}, materials={}, vertices={}, triangles={})",
            self.node_count(),
            self.mesh_count(),
            self.material_count(),
            self.vertex_count(),
            self.triangle_count()
        )
    }
}

/// JSON summary helper kept out of `#[pymethods]` so serde stays a Rust-only detail.
fn serde_json_summary(model: &ferrojt::Model) -> ferrojt::Result<String> {
    let summary = serde_json::json!({
        "nodes": model.nodes.len(),
        "meshes": model.meshes.len(),
        "materials": model.materials.len(),
        "unit": model.unit,
        "up_axis": model.up_axis,
        "source": model.source,
    });
    Ok(serde_json::to_string_pretty(&summary)?)
}

/// FerroJT: read OpenJT files and convert them to glTF / USD.
#[pymodule]
fn _ferrojt(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(open, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(detect_format, module)?)?;
    module.add_class::<Model>()?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
