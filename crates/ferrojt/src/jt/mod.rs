//! OpenJT (JT) reader.
//!
//! This module is the boundary where a JT implementation plugs in. The rest of
//! the crate only depends on [`open`] returning a [`Model`], so the reader can
//! be filled in incrementally without touching the exporters or the CLI.
//!
//! The layers, bottom up:
//!
//! - a byte order aware cursor over a slice, re-exported as [`Cursor`],
//! - the GUID type [`Guid`] and the registry of known object classes,
//!   [`class_of`],
//! - the fixed header [`FileHeader`], the TOC table [`TocEntry`] and segment
//!   framing [`Segment`], including the DEFLATE/XZ envelopes,
//! - the object classes: nodes [`Node`], attributes [`Attribute`], properties
//!   [`Property`] and representation elements [`Element`], with the shape LOD
//!   decoders [`ShapeLodVertex`] and [`ShapeLodTriStripSet`],
//! - the lossless, quantized and topologically compressed codecs behind those
//!   representations: [`Package`], [`VertexData`] and [`MeshInput`],
//! - the loader that ties it together: [`JtFile`], [`SceneGraph`], the
//!   property table and the late-loaded representations [`LoadedElement`],
//! - a JT native JSON writer that reports a file the way the classic JT
//!   tooling does: [`dump`], [`write_model`] and [`recurse_down_the_tree`].

use std::path::Path;

use crate::error::Result;
use crate::model::Model;

mod decode;
mod dump;
mod guid;
mod model;
mod object;
mod reader;
mod segment;

pub use decode::{
    Context, Decoded, ESCAPE, Entry, Kind, MeshInput, Package, PointQuantizer, Topology,
    UnifiedMesh, UniformQuantizer, Unpack, VertexData,
};
pub use dump::{
    DUMP_ALL, DUMP_LOD0, DUMP_MESH, DUMP_TOC, LayerInfo, dump, format_g, handle_all_children_json,
    handle_attributes_json, handle_late_loads_meta, recurse_down_the_tree, scan_for_layer,
    scan_for_layer_filter, write_key_value_stream, write_meshes_without_lsg, write_model,
};
pub use guid::Guid;
pub use model::{
    JtFile, LoadedElement, ObjectRef, SceneGraph, StoredAttribute, StoredNode, StoredProperty,
};
pub use object::{
    Attribute, Class, Deferred, Element, GeometricTransform, LateLoad, LodGeometry, Material, Node,
    NodeBase, Property, RawElement, ShapeLodTriStripSet, ShapeLodVertex, class_of,
    read_element_envelope, skip_element,
};
pub use reader::Cursor;
pub use segment::{FileHeader, Segment, TocEntry, segment_type_name};

/// Open a JT file and translate it into a [`Model`].
pub fn open(path: impl AsRef<Path>) -> Result<Model> {
    let path = path.as_ref();
    tracing::debug!(path = %path.display(), "jt::open");
    Ok(JtFile::open(path)?.to_model())
}

/// Read the JT header (version, byte order, TOC offset, LSG GUID).
pub fn read_header(path: impl AsRef<Path>) -> Result<FileHeader> {
    let path = path.as_ref();
    tracing::debug!(path = %path.display(), "jt::read_header");
    let data = std::fs::read(path)?;
    segment::read_header(&data)
}
