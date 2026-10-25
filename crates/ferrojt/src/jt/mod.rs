//! OpenJT (JT) reader.
//!
//! This module is the boundary where a JT implementation plugs in. The rest of
//! the crate only depends on [`open`] returning a [`Model`], so the reader can
//! be filled in incrementally without touching the exporters or the CLI.
//!
//! Planned shape of a full implementation (left as extension points):
//!
//! - `jt::ios` &mdash; parse the `SegmentObjectIdentifier` and
//!   `MetadataSegmentDescription`/`MetadataSegment` (X3.282 §A.3).
//! - `jt::lod` &mdash; LOD0/LOD1/LOD2 vertex, normal, colour and instance
//!   segments, including precision trimmed components via
//!   [`crate::codec::BitReader`].
//! - `jt::shape` &mdash; the shape structure tree, translated into
//!   [`crate::model::Node`].
//!
//! Until then every entry point returns [`crate::Error::NotImplemented`].

use std::path::Path;

use crate::error::{Error, Result};
use crate::model::Model;

/// Open a JT file and translate it into a [`Model`].
pub fn open(path: impl AsRef<Path>) -> Result<Model> {
    let path = path.as_ref();
    tracing::debug!(path = %path.display(), "jt::open");
    Err(Error::not_implemented("reading JT files"))
}

/// Read the JT header (magic, version, originator) without parsing the body.
pub fn read_header(path: impl AsRef<Path>) -> Result<JtHeader> {
    let path = path.as_ref();
    tracing::debug!(path = %path.display(), "jt::read_header");
    Err(Error::not_implemented("reading JT headers"))
}

/// Summary of a JT file's header segment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JtHeader {
    /// JT specification version, e.g. 10500 for JT 10.5.
    pub version: u32,
    /// Byte order flag from the header.
    pub big_endian: bool,
    /// `JT_OPEN` / `JT_V10000` style magic string.
    pub magic: String,
    /// Originating application.
    pub originator: String,
    /// File size in bytes.
    pub file_size: u64,
    /// Units of the model, e.g. `millimeter`.
    pub unit: String,
    /// Up axis of the model, e.g. `+Z`.
    pub up_axis: String,
}
