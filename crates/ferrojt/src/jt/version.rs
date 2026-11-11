//! The JT file version a writer emits.
//!
//! JT frames almost everything by **major** version: 8.x stores the vertex-based
//! representation and 32-bit offsets, 9.x switches the shape LODs to the
//! topological coder and widens the version fields, and 10.x widens the TOC
//! offsets to 64 bits and adds the attribute and late-load trailers. The
//! **minor** version is never read by the reader — it only appears in the banner
//! and in the `"MinorVersion"` key of [`dump`](super::dump) — so a writer picks a
//! minor that matches what the major implies.

use std::str::FromStr;

use crate::error::{Error, Result};

/// A JT file version.
///
/// The major number must be 8, 9 or 10; anything else is rejected by
/// [`JtVersion::validate`]. The default is [`JtVersion::V10_3`], the newest
/// version this crate can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JtVersion {
    /// The major version: 8, 9 or 10.
    pub major: u32,
    /// The minor version, informational only.
    pub minor: u32,
}

impl JtVersion {
    /// JT 8.0, the oldest version the reader accepts.
    pub const V8_0: Self = Self { major: 8, minor: 0 };
    /// JT 8.1, the version the classic JT tooling writes for 8.x.
    pub const V8_1: Self = Self { major: 8, minor: 1 };
    /// JT 9.5, the common 9.x revision.
    pub const V9_5: Self = Self { major: 9, minor: 5 };
    /// JT 10.3, the newest version this crate writes.
    pub const V10_3: Self = Self {
        major: 10,
        minor: 3,
    };

    /// A version from a major and minor number.
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// True when the major number is one this crate can write.
    #[must_use]
    pub const fn is_supported(&self) -> bool {
        matches!(self.major, 8..=10)
    }

    /// Reject a major number this crate cannot write.
    pub fn validate(&self) -> Result<()> {
        if self.is_supported() {
            Ok(())
        } else {
            Err(Error::invalid(
                0,
                format!(
                    "JT version {}.{} cannot be written, only 8.x, 9.x and 10.x are supported",
                    self.major, self.minor
                ),
            ))
        }
    }

    /// True when the file stores 32-bit TOC and header offsets.
    #[must_use]
    pub const fn offsets_are_32_bit(&self) -> bool {
        self.major < 10
    }

    /// True when version fields are single bytes instead of 16-bit words.
    #[must_use]
    pub const fn versions_are_bytes(&self) -> bool {
        self.major >= 10
    }

    /// True when object and attribute bases carry a version field.
    #[must_use]
    pub const fn has_object_versions(&self) -> bool {
        self.major > 8
    }

    /// True when an attribute base carries the two inhibit fields.
    #[must_use]
    pub const fn has_attribute_trailer(&self) -> bool {
        self.major >= 10
    }

    /// True when the vertex-based (lossless) shape representation is available.
    ///
    /// JT 9.x and 10.x files carry their triangle data in the topologically
    /// compressed representation instead; a `Shape_Vertex` element there holds
    /// no geometry at all.
    #[must_use]
    pub const fn has_vertex_based_representation(&self) -> bool {
        self.major < 9
    }

    /// True when a shape LOD element uses the topological coder.
    #[must_use]
    pub const fn has_topological_representation(&self) -> bool {
        self.major >= 9
    }

    /// True when a file written at `self` may be rewritten at `other`.
    ///
    /// JT stores triangle data either in the vertex-based lossless
    /// representation (up to JT 8.x) or in the topologically compressed one
    /// (from JT 9.0 on), and a file carries only the coder of its own era.
    /// Re-encoding across that boundary needs a mesh that is a closed
    /// two-manifold without degenerate triangles, which most real CAD
    /// tessellations are not, so it is not offered: the 8.x family and the
    /// 9.x/10.x family are the only mutually compatible groups.
    ///
    /// ```
    /// use ferrojt::jt::JtVersion;
    ///
    /// assert!(JtVersion::V8_1.compatible(JtVersion::V8_0));
    /// assert!(JtVersion::V9_5.compatible(JtVersion::V10_3));
    /// assert!(!JtVersion::V8_1.compatible(JtVersion::V10_3));
    /// ```
    #[must_use]
    pub const fn compatible(&self, other: Self) -> bool {
        (self.major < 9) == (other.major < 9)
    }
}

impl Default for JtVersion {
    fn default() -> Self {
        Self::V10_3
    }
}

impl std::fmt::Display for JtVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Parse a version given as `"8"`, `"9.5"` or `"10.3"`.
///
/// A bare major number picks the minor this crate writes for that major.
impl FromStr for JtVersion {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        let (major, minor) = match text.split_once('.') {
            Some((major, minor)) => (major, Some(minor)),
            None => (text, None),
        };
        let parse = |text: &str| -> Result<u32> {
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::invalid(
                    0,
                    format!("'{text}' is not a JT version number"),
                ));
            }
            Ok(text.parse::<u32>().expect("digits already checked"))
        };
        let major = parse(major)?;
        let version = match minor {
            Some(minor) => Self::new(major, parse(minor)?),
            None => match major {
                8 => Self::V8_1,
                9 => Self::V9_5,
                10 => Self::V10_3,
                _ => Self::new(major, 0),
            },
        };
        version.validate()?;
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_newest_supported_version() {
        assert_eq!(JtVersion::default(), JtVersion::V10_3);
        assert!(JtVersion::default().is_supported());
    }

    #[test]
    fn a_bare_major_picks_the_minor_that_crate_writes() {
        assert_eq!("8".parse::<JtVersion>().unwrap(), JtVersion::V8_1);
        assert_eq!("9".parse::<JtVersion>().unwrap(), JtVersion::V9_5);
        assert_eq!("10".parse::<JtVersion>().unwrap(), JtVersion::V10_3);
    }

    #[test]
    fn an_explicit_minor_is_kept() {
        assert_eq!(
            "8.0".parse::<JtVersion>().unwrap(),
            JtVersion { major: 8, minor: 0 }
        );
        assert_eq!("10.1".parse::<JtVersion>().unwrap().minor, 1);
    }

    #[test]
    fn an_unsupported_major_is_rejected() {
        for text in ["7", "11", "10.0.1", "x", "", "9."] {
            assert!(
                text.parse::<JtVersion>().is_err(),
                "'{text}' must not parse"
            );
        }
        assert!(JtVersion::new(11, 0).validate().is_err());
    }

    #[test]
    fn the_framing_gates_follow_the_major() {
        let v8 = JtVersion::V8_1;
        let v9 = JtVersion::V9_5;
        let v10 = JtVersion::V10_3;
        assert!(v8.offsets_are_32_bit() && v9.offsets_are_32_bit());
        assert!(!v10.offsets_are_32_bit());
        assert!(!v8.has_object_versions() && v9.has_object_versions());
        assert!(!v9.has_attribute_trailer() && v10.has_attribute_trailer());
        assert!(v8.has_vertex_based_representation());
        assert!(v9.has_topological_representation());
        assert!(!v9.has_vertex_based_representation());
    }

    #[test]
    fn a_version_prints_as_major_dot_minor() {
        assert_eq!(JtVersion::V9_5.to_string(), "9.5");
    }
}
