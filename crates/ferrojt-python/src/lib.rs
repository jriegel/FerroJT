//! Python bindings for FerroJT.
//!
//! The compiled extension module is named `_ferrojt`; the pure Python package
//! `ferrojt` re-exports it and adds the ergonomic helpers.
//!
//! [`JtFile`] mirrors the C++ `PyOpenJt` extension: a file object that is
//! constructed with a path, opened explicitly, and then asked for a JSON dump or
//! a glTF file. [`Model`] is the FerroJT side of the same thing and is what the
//! exporters consume.

use pyo3::exceptions::{PyNotImplementedError, PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// Convert a FerroJT error into a Python exception.
fn py_err(err: ferrojt::Error) -> PyErr {
    match err {
        ferrojt::Error::NotImplemented(what) => PyNotImplementedError::new_err(what),
        ferrojt::Error::Io(io) => PyOSError::new_err(io.to_string()),
        ferrojt::Error::Invalid { offset, message } => {
            PyValueError::new_err(format!("{message} at offset {offset}"))
        }
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
    ///
    /// When `format` is omitted it is taken from the extension of `path`.
    #[pyo3(signature = (path, format = None))]
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

/// Object representing an open JT file.
///
/// The object is created with a path and nothing is read until [`JtFile::open`]
/// is called, which mirrors the C++ `PyOpenJt.JtFile` class:
///
/// ```python
/// import ferrojt
///
/// jt = ferrojt.JtFile("gear.jt")
/// if jt.open():
///     print(jt.json())
///     jt.writeGlTf("gear")
/// ```
#[pyclass(name = "JtFile")]
pub struct JtFile {
    file_name: String,
    inner: Option<ferrojt::jt::JtFile>,
}

#[pymethods]
impl JtFile {
    /// Create a file object for `file_name`; nothing is read yet.
    #[new]
    fn new(file_name: String) -> Self {
        JtFile {
            file_name,
            inner: None,
        }
    }

    /// The path of the JT file, the attribute name the C++ extension uses.
    #[getter]
    #[pyo3(name = "FileName")]
    fn file_name(&self) -> &str {
        &self.file_name
    }

    #[setter]
    #[pyo3(name = "FileName")]
    fn set_file_name(&mut self, value: String) {
        self.file_name = value;
    }

    /// Snake case alias of the `FileName` attribute.
    #[getter]
    #[pyo3(name = "file_name")]
    fn file_name_snake(&self) -> &str {
        &self.file_name
    }

    #[setter]
    #[pyo3(name = "file_name")]
    fn set_file_name_snake(&mut self, value: String) {
        self.file_name = value;
    }

    /// Read the header, the TOC and the logical scene graph.
    ///
    /// Returns `True` on success and `False` when the file cannot be read,
    /// exactly like the C++ extension does.
    fn open(&mut self, py: Python<'_>) -> PyResult<bool> {
        let name = self.file_name.clone();
        let loaded = py.detach(|| ferrojt::jt::JtFile::open(&name).ok());
        self.inner = loaded;
        Ok(self.inner.is_some())
    }

    /// `True` once the file has been read successfully.
    #[getter]
    fn is_open(&self) -> bool {
        self.inner.is_some()
    }

    /// The whole file as a JSON string, in the layout the JT tooling uses.
    ///
    /// Returns the empty string while the file is closed.
    fn json(&self, py: Python<'_>) -> PyResult<String> {
        let Some(file) = self.inner.as_ref() else {
            return Ok(String::new());
        };
        let text = py
            .detach(|| ferrojt::jt::dump(file, ferrojt::jt::DUMP_ALL, 1))
            .map_err(py_err)?;
        Ok(text)
    }

    /// The header and TOC table as a JSON string. Returns the empty string
    /// while the file is closed.
    fn toc(&self, py: Python<'_>) -> PyResult<String> {
        let Some(file) = self.inner.as_ref() else {
            return Ok(String::new());
        };
        let text = py
            .detach(|| ferrojt::jt::dump(file, ferrojt::jt::DUMP_TOC, 1))
            .map_err(py_err)?;
        Ok(text)
    }

    /// Write the file as glTF and return `True`, or return `False` while the
    /// file is closed.
    ///
    /// Like the C++ extension, `path` is a name without an extension: `.gltf`
    /// is appended unless the name already ends in `.gltf` or `.glb`.
    #[pyo3(name = "writeGlTf")]
    fn write_gl_tf(&self, py: Python<'_>, path: &str) -> PyResult<bool> {
        let Some(file) = self.inner.as_ref() else {
            return Ok(false);
        };
        let target = gltf_target(path);
        let model = file.to_model();
        let format = ferrojt::Format::detect(&target);
        py.detach(|| ferrojt::export::write_to_file(format, &model, &target))
            .map_err(py_err)?;
        Ok(true)
    }

    /// Snake case alias of [`JtFile::write_gl_tf`].
    #[pyo3(name = "write_gltf")]
    fn write_gltf(&self, py: Python<'_>, path: &str) -> PyResult<bool> {
        self.write_gl_tf(py, path)
    }

    /// The scene as a [`Model`], which is what the exporters consume.
    ///
    /// Only the finest level of detail of a range LOD is turned into a mesh.
    ///
    /// Raises `RuntimeError` while the file is closed.
    fn model(&self) -> PyResult<Model> {
        let Some(file) = self.inner.as_ref() else {
            return Err(PyRuntimeError::new_err(format!(
                "{} is not open",
                self.file_name
            )));
        };
        Ok(Model {
            inner: file.to_model(),
        })
    }

    fn __repr__(&self) -> String {
        if self.inner.is_some() {
            format!("JtFile({:?}, open=True)", self.file_name)
        } else {
            format!("JtFile({:?}, open=False)", self.file_name)
        }
    }
}

/// The C++ writer appends `.gltf` to whatever name it is given; FerroJT keeps
/// that behaviour but honours an extension that is already there.
fn gltf_target(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".gltf") || lower.ends_with(".glb") {
        path.to_string()
    } else {
        format!("{path}.gltf")
    }
}

/// FerroJT: read OpenJT files and convert them to glTF / USD.
#[pymodule]
fn _ferrojt(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(pyo3::wrap_pyfunction!(open, module)?)?;
    module.add_function(pyo3::wrap_pyfunction!(detect_format, module)?)?;
    module.add_class::<Model>()?;
    module.add_class::<JtFile>()?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
