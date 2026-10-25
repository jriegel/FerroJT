//! Decoding of the JT integer compression layer.
//!
//! The JT format stores integer arrays (vertex coordinates, normals, indices,
//! and the whole topological mesh description) in *integer codec packages*.
//! This module reproduces that layer:
//!
//! * [`bits`] — the most-significant-bit-first bit reader the codecs run on,
//! * [`prob`] — the probability tables the arithmetic and Huffman codecs use,
//! * [`codecs`] — bitlength, bitlength2, Huffman and arithmetic decoding,
//! * [`unpack`] — the secondary predictor applied after decoding,
//! * [`cdp`] — the package framings that tie the above together,
//! * [`vertex`] — vertex coordinate and normal data,
//! * [`mesh`] — the topologically compressed mesh coder.
//!
//! Everything here is a clean-room Rust implementation written from the
//! published JT specification; the byte streams it reads are the only thing
//! shared with any other implementation.

mod bits;
mod cdp;
mod codecs;
mod mesh;
mod prob;
mod unpack;
mod vertex;

pub use cdp::{Kind, Package};
pub use mesh::{MeshInput, Topology, UnifiedMesh};
pub use prob::{Context, ESCAPE, Entry};
pub use unpack::Unpack;
pub use vertex::{Decoded, PointQuantizer, UniformQuantizer, VertexData};
