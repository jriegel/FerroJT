//! Writers for the target formats.
//!
//! Every exporter consumes the same [`Model`], so adding a format means adding
//! a module that implements [`Exporter`] and registering it in
//! [`to_format`].

mod glb;
mod gltf;
mod usd;

pub use glb::{GLB_HEADER_MAGIC, GLB_VERSION, GlbBuilder, parse_header};
pub use gltf::GltfExporter;
pub use usd::UsdExporter;

use crate::error::{Error, Result};
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
        Format::Jt => return Err(Error::UnsupportedFormat("jt (write)".into())),
    }
    Ok(out)
}

/// Serialize `model` to `format` and write it to `path`.
pub fn write_to_file(
    format: Format,
    model: &Model,
    path: impl AsRef<std::path::Path>,
) -> Result<()> {
    let bytes = to_format(format, model)?;
    std::fs::write(path, bytes)?;
    Ok(())
}
