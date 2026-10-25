//! Low level binary helpers shared by format readers.
//!
//! JT stores its metadata big endian and its vertex data either big or little
//! endian depending on the segment, so byte order is carried explicitly rather
//! than assumed.

mod bits;
mod compress;
mod endian;

pub use bits::BitReader;
pub use compress::{
    XZ_OUTPUT_LIMIT, XZ_SIGNATURE, decompress_xz, deflate, inflate, is_deflate_signature,
    is_xz_signature,
};
pub use endian::{ByteOrder, ReaderExt, WriterExt};
