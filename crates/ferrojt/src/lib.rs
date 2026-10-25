//! FerroJT: read OpenJT (JT) files and convert them to other 3D formats.
//!
//! The crate is organised in four layers:
//!
//! - [`model`] &mdash; a format agnostic scene model (the "IR" every reader produces
//!   and every exporter consumes).
//! - [`codec`] &mdash; low level binary helpers (byte order, segment framing,
//!   DEFLATE) shared by format readers.
//! - [`jt`] &mdash; the OpenJT reader. This is the insertion point for a JT
//!   implementation: everything below it is already wired to consume it.
//! - [`export`] &mdash; writers for the target formats (glTF/GLB today, USD
//!   reserved).
//!
//! ```no_run
//! use ferrojt::Format;
//!
//! let model = ferrojt::open("gear.jt")?;
//! let glb = ferrojt::export::to_format(Format::Glb, &model)?;
//! std::fs::write("gear.glb", glb)?;
//! # Ok::<(), ferrojt::Error>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codec;
pub mod error;
pub mod export;
pub mod jt;
pub mod model;

pub use error::{Error, Result};
pub use model::{Format, Model};

/// Open a file, guessing its container format from the extension.
pub fn open(path: impl AsRef<std::path::Path>) -> Result<Model> {
    let path = path.as_ref();
    open_format(Format::detect(path), path)
}

/// Open a file whose container format is already known.
pub fn open_format(format: Format, path: impl AsRef<std::path::Path>) -> Result<Model> {
    let path = path.as_ref();
    tracing::debug!(%format, path = %path.display(), "opening");
    match format {
        Format::Jt => jt::open(path),
        Format::Gltf | Format::Glb => Err(Error::UnsupportedFormat(format.to_string())),
        Format::Usd => Err(Error::UnsupportedFormat(format.to_string())),
    }
}
