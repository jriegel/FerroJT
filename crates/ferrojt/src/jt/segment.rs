//! File framing: the fixed header, the TOC table and the segment envelope.
//!
//! A JT file is a flat container: a 105/109 byte header, then a table of
//! contents pointing at self-describing segments. Each segment carries its own
//! GUID, type and length, and (for the segments that can hold geometry) a
//! compression envelope. Everything above this module works on the *payload*
//! of one segment at a time.

use std::borrow::Cow;

use crate::codec::{ByteOrder, decompress_xz, inflate};
use crate::error::{Error, Result};
use crate::jt::guid::Guid;
use crate::jt::reader::Cursor;

/// Length of the ASCII version banner at the start of every JT file.
const VERSION_BANNER_LEN: usize = 80;

/// Human readable name of a segment type, as used by the reference JSON writer.
pub fn segment_type_name(kind: i32) -> &'static str {
    match kind {
        1 => "Logical Scene Graph",
        2 => "JT B-Rep",
        3 => "PMI Data",
        4 => "Meta Data",
        6 => "Shape",
        7 => "Shape LOD0",
        8 => "Shape LOD1",
        9 => "Shape LOD2",
        10 => "Shape LOD3",
        11 => "Shape LOD4",
        12 => "Shape LOD5",
        13 => "Shape LOD6",
        14 => "Shape LOD7",
        15 => "Shape LOD8",
        16 => "Shape LOD9",
        17 => "XT B-Rep",
        18 => "Wireframe Representation",
        20 => "ULP",
        23 => "STT",
        24 => "LWPA",
        30 => "MultiXT B-Rep",
        31 => "InfoSegment",
        32 => "AEC Shape",
        33 => "STEP B-rep",
        _ => "Unknown",
    }
}

/// Segment types that carry a compression envelope.
fn has_compression_header(kind: i32) -> bool {
    matches!(kind, 1 | 2 | 3 | 4 | 17 | 18 | 20 | 24)
}

/// The fixed part of a JT file: version, byte order, TOC offset and LSG GUID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHeader {
    /// Major version, 8, 9 or 10.
    pub major: u32,
    /// Minor version as written in the banner.
    pub minor: u32,
    /// Byte order declared by the header.
    pub order: ByteOrder,
    /// Absolute offset of the TOC table.
    pub toc_offset: u64,
    /// GUID of the logical scene graph segment.
    pub lsg_guid: Guid,
}

impl FileHeader {
    /// `"9.5"`, the way the version banner spells it.
    pub fn version_text(&self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

/// Parse the fixed header of a JT file.
pub fn read_header(data: &[u8]) -> Result<FileHeader> {
    if data.len() < VERSION_BANNER_LEN + 1 {
        return Err(Error::invalid(0, "file is too short to hold a JT header"));
    }
    let banner = String::from_utf8_lossy(&data[..VERSION_BANNER_LEN]);
    if !banner.contains("Version") {
        return Err(Error::invalid(
            0,
            "the 80 byte header has no version marker",
        ));
    }
    // The banner is whitespace separated; the second token is "<major>.<minor>".
    let version = banner
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::invalid(0, "the header has no version token"))?;
    let (major, minor) = parse_version(version)?;

    let order = if data[VERSION_BANNER_LEN] == 0 {
        ByteOrder::Little
    } else {
        ByteOrder::Big
    };

    let mut cursor = Cursor::new(&data[VERSION_BANNER_LEN + 1..], order, major);
    let _reserved = cursor.read_i32()?;
    // JT 10.x widened the TOC offset to 64 bits.
    let toc_offset = if major >= 10 {
        cursor.read_u64()?
    } else {
        cursor.read_i32()? as u64
    };
    let lsg_guid = cursor.read_guid()?;
    Ok(FileHeader {
        major,
        minor,
        order,
        toc_offset,
        lsg_guid,
    })
}

/// Both halves must parse completely, matching the reference's use of
/// `std::from_chars` with an end-pointer check.
fn parse_version(text: &str) -> Result<(u32, u32)> {
    let dot = text
        .find('.')
        .filter(|at| *at >= 1)
        .ok_or_else(|| Error::invalid(0, "the version token is not \"major.minor\""))?;
    let parse = |part: &str| -> Result<u32> {
        let value: i32 = part
            .parse()
            .map_err(|_| Error::invalid(0, format!("invalid version number {part:?}")))?;
        u32::try_from(value).map_err(|_| Error::invalid(0, "the version number is negative"))
    };
    Ok((parse(&text[..dot])?, parse(&text[dot + 1..])?))
}

/// One row of the TOC table, in file order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TocEntry {
    /// GUID of the segment this row points at.
    pub guid: Guid,
    /// Absolute offset of the segment.
    pub offset: u64,
    /// Declared length of the segment, including its envelope.
    pub length: u32,
    /// Segment type, taken from the top byte of the attribute field.
    pub kind: i32,
}

/// Read the TOC table at `offset`.
///
/// The rows are returned in file order, which is also the order the reference
/// writer uses for its `TocTable` array.
pub fn read_toc(data: &[u8], offset: u64, header: &FileHeader) -> Result<Vec<TocEntry>> {
    let start = file_index(data, offset)?;
    let mut cursor = Cursor::new(&data[start..], header.order, header.major);
    let count = cursor.read_i32()?;
    if count < 1 {
        return Err(Error::invalid(offset, "the TOC table is empty"));
    }
    let row = if header.major >= 10 { 32 } else { 28 };
    let mut entries = Vec::with_capacity(
        usize::try_from(count)
            .unwrap_or(0)
            .min(cursor.remaining() / row),
    );
    for _ in 0..count {
        let guid = cursor.read_guid()?;
        let segment_offset = if header.major >= 10 {
            cursor.read_u64()?
        } else {
            cursor.read_i32()? as u64
        };
        let length = cursor.read_u32()?;
        let attrib = cursor.read_u32()?;
        entries.push(TocEntry {
            guid,
            offset: segment_offset,
            length,
            kind: ((attrib >> 24) & 0xFF) as i32,
        });
    }
    Ok(entries)
}

/// A segment envelope that has been read, with its (possibly decompressed) payload.
#[derive(Clone, Debug)]
pub struct Segment<'a> {
    /// GUID of the segment.
    pub guid: Guid,
    /// Segment type.
    pub kind: i32,
    /// Declared length of the segment, including its envelope.
    pub size: i32,
    /// Payload the object readers work on.
    pub body: Cow<'a, [u8]>,
    /// File bytes consumed by the envelope, i.e. the offset of the payload.
    envelope: usize,
    /// File bytes that fed the payload: the compressed stream, or the slice
    /// taken straight out of the file for an uncompressed segment.
    input: usize,
    /// True when the payload came out of a decompressor.
    compressed: bool,
}

impl Segment<'_> {
    /// Name of the segment type.
    pub fn type_name(&self) -> &'static str {
        segment_type_name(self.kind)
    }

    /// Mirror the reference length check once the payload has been parsed.
    ///
    /// Reading past the declared end is an error; bytes left over are only a
    /// warning, exactly as in the reference. For a compressed segment the
    /// reference compares against however much of the compressed stream its
    /// lazy inflate happened to pull, which is not observable here — the whole
    /// declared stream is counted instead.
    pub fn check_length(&self, body_used: usize) -> Result<()> {
        let consumed = self.envelope
            + if self.compressed {
                self.input
            } else {
                body_used
            };
        let declared =
            usize::try_from(self.size).map_err(|_| Error::corrupt("segment length is negative"))?;
        if consumed > declared {
            return Err(Error::corrupt(format!(
                "{} extra bytes were read after the end of the segment",
                consumed - declared
            )));
        }
        if consumed < declared {
            tracing::warn!(
                "{} segment bytes remained after reading, ignored",
                declared - consumed
            );
        }
        Ok(())
    }
}

/// Read the envelope of the segment at `offset` and return its payload.
pub fn read_segment<'a>(data: &'a [u8], offset: u64, header: &FileHeader) -> Result<Segment<'a>> {
    let start = file_index(data, offset)?;
    let mut file = Cursor::new(&data[start..], header.order, header.major);
    let guid = file.read_guid()?;
    let kind = file.read_i32()?;
    let size = file.read_i32()?;
    if size < 0 {
        return Err(Error::invalid(offset, "segment length is negative"));
    }

    let mut compressed = false;
    let input;
    let envelope = file.position();
    let body: Cow<'a, [u8]> = if has_compression_header(kind) {
        let flag = file.read_u32()?;
        let data_length = file.read_i32()?;
        let algorithm = file.read_u8()?;
        if matches!((flag, algorithm), (2, 2) | (3, 3)) {
            let want = usize::try_from(data_length - 1)
                .map_err(|_| Error::invalid(offset, "compressed segment length is not positive"))?;
            let available = file.remaining();
            if available < want {
                return Err(Error::invalid(
                    offset,
                    format!(
                        "segment declares {want} compressed bytes but only {available} are available"
                    ),
                ));
            }
            let packed = file.read_bytes(want)?;
            let payload = if flag == 2 {
                inflate(packed)?
            } else {
                decompress_xz(packed)?
            };
            compressed = true;
            input = want;
            Cow::Owned(payload)
        } else {
            // Not compressed: the payload runs to the declared end of the segment.
            let declared_body = usize::try_from(size).unwrap_or(0).saturating_sub(envelope);
            let take = declared_body.min(file.remaining());
            input = take;
            Cow::Borrowed(file.read_bytes(take)?)
        }
    } else {
        let declared_body = usize::try_from(size).unwrap_or(0).saturating_sub(envelope);
        let take = declared_body.min(file.remaining());
        input = take;
        Cow::Borrowed(file.read_bytes(take)?)
    };

    Ok(Segment {
        guid,
        kind,
        size,
        body,
        envelope,
        input,
        compressed,
    })
}

/// Turn an absolute file offset into an index into `data`.
fn file_index(data: &[u8], offset: u64) -> Result<usize> {
    let index = usize::try_from(offset)
        .map_err(|_| Error::invalid(offset, "offset does not fit in memory addressing"))?;
    if index >= data.len() {
        return Err(Error::invalid(
            offset,
            format!("offset is past the end of the file ({} bytes)", data.len()),
        ));
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(version: &str, order: u8, major: u32, toc: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let banner = format!("{version} JT  DM 9.4.0.0");
        let mut banner = banner.into_bytes();
        banner.resize(VERSION_BANNER_LEN, b' ');
        out.extend_from_slice(&banner);
        out.push(order);
        out.extend_from_slice(&0i32.to_le_bytes());
        if major >= 10 {
            out.extend_from_slice(&toc.to_le_bytes());
        } else {
            out.extend_from_slice(&(toc as i32).to_le_bytes());
        }
        out.extend_from_slice(&[0u8; 16]);
        out
    }

    #[test]
    fn header_reads_version_and_offsets() {
        let data = header_bytes("Version 9.5", 0, 9, 0x100);
        let header = read_header(&data).unwrap();
        assert_eq!(header.major, 9);
        assert_eq!(header.minor, 5);
        assert_eq!(header.order, ByteOrder::Little);
        assert_eq!(header.toc_offset, 0x100);
        assert_eq!(header.version_text(), "9.5");
    }

    #[test]
    fn jt10_uses_a_wider_toc_offset() {
        let data = header_bytes("Version 10.0", 0, 10, 0x1_0000_0000);
        let header = read_header(&data).unwrap();
        assert_eq!(header.major, 10);
        assert_eq!(header.toc_offset, 0x1_0000_0000);
    }

    #[test]
    fn byte_order_byte_selects_big_endian() {
        let data = header_bytes("Version 8.0", 1, 8, 0x40);
        assert_eq!(read_header(&data).unwrap().order, ByteOrder::Big);
    }

    #[test]
    fn version_must_parse_completely() {
        let data = header_bytes("Version 9.x", 0, 9, 0x40);
        assert!(read_header(&data).is_err());
        let data = header_bytes("9.5", 0, 9, 0x40);
        assert!(read_header(&data).is_err());
    }

    #[test]
    fn toc_rows_keep_file_order() {
        let header = FileHeader {
            major: 9,
            minor: 5,
            order: ByteOrder::Little,
            toc_offset: 0,
            lsg_guid: Guid::default(),
        };
        let mut data = Vec::new();
        data.extend_from_slice(&2i32.to_le_bytes());
        for i in 0..2u8 {
            data.extend_from_slice(&[i; 16]);
            data.extend_from_slice(&((0x100 + u64::from(i)) as i32).to_le_bytes());
            data.extend_from_slice(&(64u32 + u32::from(i)).to_le_bytes());
            data.extend_from_slice(&(0x0400_0000u32 + u32::from(i)).to_le_bytes());
        }
        let rows = read_toc(&data, 0, &header).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, 4);
        assert_eq!(rows[1].offset, 0x101);
        assert_eq!(segment_type_name(rows[0].kind), "Meta Data");
    }

    #[test]
    fn empty_toc_is_rejected() {
        let header = FileHeader {
            major: 9,
            minor: 5,
            order: ByteOrder::Little,
            toc_offset: 0,
            lsg_guid: Guid::default(),
        };
        let data = 0i32.to_le_bytes().to_vec();
        assert!(read_toc(&data, 0, &header).is_err());
    }

    #[test]
    fn plain_segment_payload_is_sliced_to_the_declared_size() {
        let header = FileHeader {
            major: 9,
            minor: 5,
            order: ByteOrder::Little,
            toc_offset: 0,
            lsg_guid: Guid::default(),
        };
        let mut data = Vec::new();
        data.extend_from_slice(&[0x11u8; 16]);
        data.extend_from_slice(&6i32.to_le_bytes()); // type 6 has no envelope
        data.extend_from_slice(&36i32.to_le_bytes()); // 16 + 4 + 4 + 12 body
        data.extend_from_slice(&[7u8; 12]);
        data.extend_from_slice(&[0xAAu8; 8]);
        let segment = read_segment(&data, 0, &header).unwrap();
        assert_eq!(segment.guid.bytes(), &[0x11u8; 16]);
        assert_eq!(segment.kind, 6);
        assert_eq!(segment.body.len(), 12);
        assert!(segment.check_length(12).is_ok());
        assert!(segment.check_length(13).is_err());
    }

    #[test]
    fn compressed_segment_is_inflated_eagerly() {
        let header = FileHeader {
            major: 9,
            minor: 5,
            order: ByteOrder::Little,
            toc_offset: 0,
            lsg_guid: Guid::default(),
        };
        let payload = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let packed = crate::codec::deflate(&payload).unwrap();
        let envelope = 16 + 4 + 4 + 4 + 1;
        let mut data = Vec::new();
        data.extend_from_slice(&[0x11u8; 16]);
        data.extend_from_slice(&1i32.to_le_bytes());
        data.extend_from_slice(&((envelope + packed.len() as i32) as i32).to_le_bytes());
        data.extend_from_slice(&2u32.to_le_bytes()); // compression flag
        data.extend_from_slice(&((packed.len() + 1) as i32).to_le_bytes()); // data length incl. algorithm
        data.push(2); // deflate
        data.extend_from_slice(&packed);
        let segment = read_segment(&data, 0, &header).unwrap();
        assert_eq!(segment.body.as_ref(), payload.as_slice());
        assert!(segment.check_length(0).is_ok());
    }
}
