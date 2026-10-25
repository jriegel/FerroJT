//! JT object type identifiers.
//!
//! A JT GUID is stored as one `U32`, two `U16` and eight `U8` values, each
//! written in the byte order of the file. [`crate::jt::reader`] normalises that
//! into the logical order kept here (big-endian inside the three numeric
//! fields), so the constants below can be written the way they read in the
//! specification.
//!
//! The textual form used by the JT tooling keeps the same split, so the last
//! eight groups are single bytes rather than the two four-digit groups a RFC
//! 4122 GUID would use:
//!
//! ```text
//! file (LE)  17 23 7b 83 | 73 0f | ec 11 | 80 00 cd 25 74 4c 16 19
//! logical    83 7b 23 17 | 0f 73 | 11 ec | 80 00 cd 25 74 4c 16 19
//! text       837b2317-0f73-11ec-80-00-cd-25-74-4c-16-19
//! ```

use core::fmt;

/// A JT object type identifier.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    /// Build a GUID from its raw file bytes (the same order as [`Guid`] stores
    /// them, i.e. big-endian within each of the three leading fields).
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Guid(bytes)
    }

    /// The 16 logical bytes of the identifier.
    pub const fn bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The all-`0xff` marker that terminates a logical scene graph element
    /// table.
    pub const END_OF_ELEMENT_MARKER: Guid = Guid([0xff; 16]);

    /// Build a GUID from the dotted text form used by JT files and dumps.
    ///
    /// Accepts both the JT form (`837b2317-0f73-11ec-80-00-cd-25-74-4c-16-19`)
    /// and the RFC 4122 form (`837b2317-0f73-11ec-8000-cd25744c1619`).
    pub fn parse(text: &str) -> Option<Guid> {
        let hex = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        };
        let digits: Vec<u8> = text
            .bytes()
            .filter(|b| *b != b'-')
            .map(hex)
            .collect::<Option<Vec<_>>>()?;
        if digits.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = (digits[i * 2] << 4) | digits[i * 2 + 1];
        }
        Some(Guid(bytes))
    }

    /// Append the JT textual form to `out`.
    pub fn write_text(&self, out: &mut String) {
        let b = &self.0;
        let word = |v: u32, digits: usize, out: &mut String| {
            for shift in (0..digits).rev() {
                out.push(char::from_digit((v >> (shift * 4)) & 0xf, 16).unwrap_or('0'));
            }
        };
        word(u32::from_be_bytes([b[0], b[1], b[2], b[3]]), 8, out);
        out.push('-');
        word(u32::from(u16::from_be_bytes([b[4], b[5]])), 4, out);
        out.push('-');
        word(u32::from(u16::from_be_bytes([b[6], b[7]])), 4, out);
        for byte in &b[8..16] {
            out.push('-');
            word(u32::from(*byte), 2, out);
        }
    }

    /// The JT textual form of this GUID.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(39);
        self.write_text(&mut out);
        out
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Guid({})", self)
    }
}

/// Well known JT object type identifiers.
pub mod types {
    use super::Guid;

    pub const NODE_BASE: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x35, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_GROUP: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x1b, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_INSTANCE: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x2a, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_LOD: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x2c, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_PARTITION: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x3e, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_RANGE_LOD: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x4c, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_SHAPE_BASE: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x59, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_SHAPE_TRI_STRIP_SET: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x77, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const NODE_SHAPE_VERTEX: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x7f, 0x2a, 0xc8, 0x11, 0xd1, 0x6b, 0x9b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const SHAPE_LOD_BASE: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0xa4, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const SHAPE_LOD_TRI_STRIP_SET: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0xab, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const SHAPE_LOD_VERTEX: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0xb0, 0x2a, 0xc8, 0x11, 0xd1, 0x6b, 0x9b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const ATTRIBUTE_MATERIAL: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x30, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const ATTRIBUTE_GEOMETRIC_TRANSFORM: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x83, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const PROPERTY_BASE: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x4b, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const PROPERTY_STRING: Guid = Guid::from_bytes([
        0x10, 0xdd, 0x10, 0x6e, 0x2a, 0xc8, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
    pub const PROPERTY_LATE_LOADED: Guid = Guid::from_bytes([
        0xe0, 0xb0, 0x5b, 0xe5, 0xfb, 0xbd, 0x11, 0xd1, 0xa3, 0xa7, 0x00, 0xaa, 0x00, 0xd1, 0x09,
        0x54,
    ]);
    pub const NODE_PART: Guid = Guid::from_bytes([
        0xce, 0x35, 0x72, 0x44, 0x38, 0xfb, 0x11, 0xd1, 0xa5, 0x06, 0x00, 0x60, 0x97, 0xbd, 0xc6,
        0xe1,
    ]);
    pub const NODE_META_DATA: Guid = Guid::from_bytes([
        0xce, 0x35, 0x72, 0x45, 0x38, 0xfb, 0x11, 0xd1, 0xa5, 0x06, 0x00, 0x60, 0x97, 0xbd, 0xc6,
        0xe1,
    ]);
    pub const ELEMENT_PROXY_META_DATA: Guid = Guid::from_bytes([
        0xce, 0x35, 0x72, 0x47, 0x38, 0xfb, 0x11, 0xd1, 0xa5, 0x06, 0x00, 0x60, 0x97, 0xbd, 0xc6,
        0xe1,
    ]);
    pub const ELEMENT_META_DATA_PMI_MANAGER: Guid = Guid::from_bytes([
        0xce, 0x35, 0x72, 0x49, 0x38, 0xfb, 0x11, 0xd1, 0xa5, 0x06, 0x00, 0x60, 0x97, 0xbd, 0xc6,
        0xe1,
    ]);
    pub const XTB_REP: Guid = Guid::from_bytes([
        0x87, 0x3a, 0x70, 0xe0, 0x2a, 0xc9, 0x11, 0xd1, 0x9b, 0x6b, 0x00, 0x80, 0xc7, 0xbb, 0x59,
        0x97,
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_jt_tooling() {
        // Logical order, as produced by the reader for the file bytes
        // `17 23 7b 83 73 0f ec 11 80 00 cd 25 74 4c 16 19` in a little-endian file.
        let guid = Guid::from_bytes([
            0x83, 0x7b, 0x23, 0x17, 0x0f, 0x73, 0x11, 0xec, 0x80, 0x00, 0xcd, 0x25, 0x74, 0x4c,
            0x16, 0x19,
        ]);
        assert_eq!(guid.to_text(), "837b2317-0f73-11ec-80-00-cd-25-74-4c-16-19");
    }

    #[test]
    fn parses_text_forms() {
        let text = "837b2317-0f73-11ec-80-00-cd-25-74-4c-16-19";
        let guid = Guid::parse(text).expect("parses");
        assert_eq!(guid.to_text(), text);
        assert_eq!(
            Guid::parse("837b2317-0f73-11ec-8000-cd25744c1619"),
            Some(guid),
            "the RFC 4122 spelling must round-trip to the same bytes"
        );
        assert_eq!(
            Guid::parse("837b231-0f73-11ec-80-00-cd-25-74-4c-16-19"),
            None
        );
    }

    /// Every registered identifier must spell exactly what the reference
    /// implementation registers it as, otherwise the class lookup silently
    /// falls back to "unknown" and whole parts of a file are skipped.
    #[test]
    fn registered_guids_match_the_reference_spelling() {
        let cases: &[(&Guid, &str)] = &[
            (
                &types::NODE_BASE,
                "10dd1035-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_GROUP,
                "10dd101b-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_INSTANCE,
                "10dd102a-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_LOD,
                "10dd102c-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_PARTITION,
                "10dd103e-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_RANGE_LOD,
                "10dd104c-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_SHAPE_BASE,
                "10dd1059-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_SHAPE_TRI_STRIP_SET,
                "10dd1077-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::NODE_SHAPE_VERTEX,
                "10dd107f-2ac8-11d1-6b-9b-00-80-c7-bb-59-97",
            ),
            (
                &types::SHAPE_LOD_BASE,
                "10dd10a4-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::SHAPE_LOD_TRI_STRIP_SET,
                "10dd10ab-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::SHAPE_LOD_VERTEX,
                "10dd10b0-2ac8-11d1-6b-9b-00-80-c7-bb-59-97",
            ),
            (
                &types::ATTRIBUTE_MATERIAL,
                "10dd1030-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::ATTRIBUTE_GEOMETRIC_TRANSFORM,
                "10dd1083-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::PROPERTY_BASE,
                "10dd104b-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::PROPERTY_STRING,
                "10dd106e-2ac8-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
            (
                &types::PROPERTY_LATE_LOADED,
                "e0b05be5-fbbd-11d1-a3-a7-00-aa-00-d1-09-54",
            ),
            (
                &types::NODE_PART,
                "ce357244-38fb-11d1-a5-06-00-60-97-bd-c6-e1",
            ),
            (
                &types::NODE_META_DATA,
                "ce357245-38fb-11d1-a5-06-00-60-97-bd-c6-e1",
            ),
            (
                &types::ELEMENT_PROXY_META_DATA,
                "ce357247-38fb-11d1-a5-06-00-60-97-bd-c6-e1",
            ),
            (
                &types::ELEMENT_META_DATA_PMI_MANAGER,
                "ce357249-38fb-11d1-a5-06-00-60-97-bd-c6-e1",
            ),
            (
                &types::XTB_REP,
                "873a70e0-2ac9-11d1-9b-6b-00-80-c7-bb-59-97",
            ),
        ];
        for (guid, text) in cases {
            assert_eq!(&guid.to_text(), text, "GUID constant must round-trip");
            assert_eq!(Guid::parse(text), Some(**guid), "text must parse back");
        }
    }

    #[test]
    fn known_guids_are_distinct() {
        let all = [
            types::NODE_BASE,
            types::NODE_GROUP,
            types::NODE_PARTITION,
            types::NODE_PART,
            types::NODE_META_DATA,
            types::SHAPE_LOD_TRI_STRIP_SET,
            types::PROPERTY_LATE_LOADED,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }
}
