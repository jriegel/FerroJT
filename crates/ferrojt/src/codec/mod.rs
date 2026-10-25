//! Low level binary helpers shared by format readers.
//!
//! JT stores its metadata big endian and its vertex data either big or little
//! endian depending on the segment, so byte order is carried explicitly rather
//! than assumed.

mod bits;
mod compress;
mod endian;

pub use bits::BitReader;
pub use compress::{deflate, inflate, is_deflate_signature};
pub use endian::{ByteOrder, ReaderExt, WriterExt};
