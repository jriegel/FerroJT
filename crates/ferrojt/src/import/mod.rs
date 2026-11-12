//! Readers for the source formats.
//!
//! Every reader produces the same [`crate::model::Model`], so adding a format
//! means adding a
//! module with an `open`/`from_*` pair and registering it in
//! [`crate::open_format`]. The OpenJT reader lives in [`crate::jt`] because it
//! is also the largest part of the crate; the glTF reader lives here.

mod gltf;

pub use gltf::{from_bytes, from_file};
