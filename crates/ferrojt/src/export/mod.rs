//! Writers for the target formats.
//!
//! Every exporter consumes the same [`Model`], so adding a format means adding
//! a module that implements [`Exporter`] and registering it in
//! [`to_format`]. JT is the exception: it is written by [`crate::jt`], which
//! needs the revision to write at rather than a fixed container.

mod glb;
mod gltf;
mod usd;

pub use glb::{GLB_HEADER_MAGIC, GLB_VERSION, GlbBuilder, parse_header};
pub use gltf::GltfExporter;
pub use usd::UsdExporter;

pub(crate) use gltf::read_component;

use crate::error::Result;
use crate::model::{Format, Model};

/// Writes a [`Model`] into some container format.
pub trait Exporter {
    /// The container this exporter produces.
    const FORMAT: Format;

    /// Serialize `model` into `out`.
    fn write(&self, model: &Model, out: &mut dyn std::io::Write) -> Result<()>;
}

/// Serialize `model` to `format` and return the bytes.
pub fn to_format(format: Format, model: &Model) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match format {
        Format::Gltf => GltfExporter::json().write(model, &mut out)?,
        Format::Glb => GltfExporter::glb().write(model, &mut out)?,
        Format::Usd => UsdExporter.write(model, &mut out)?,
        Format::Jt => return crate::jt::model_to_bytes(model, None),
    }
    Ok(out)
}

/// Serialize `model` to `format` and write it to `path`.
///
/// JT goes through [`crate::jt::model_to_file`] rather than [`to_format`]: the
/// root partition of a JT file is named after the file it lives in, so the
/// writer has to see `path` — and the revision it lands on is chosen from the
/// meshes of the model, not fixed by the container.
pub fn write_to_file(
    format: Format,
    model: &Model,
    path: impl AsRef<std::path::Path>,
) -> Result<()> {
    if format == Format::Jt {
        return crate::jt::model_to_file(model, path.as_ref(), None);
    }
    let bytes = to_format(format, model)?;
    std::fs::write(path, bytes)?;
    Ok(())
}
