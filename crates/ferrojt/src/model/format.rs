//! Container formats FerroJT knows about.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::error::{Error, Result};

/// A file container that FerroJT can read and/or write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// OpenJT (JT) package.
    Jt,
    /// glTF 2.0 JSON asset.
    Gltf,
    /// glTF 2.0 binary container.
    Glb,
    /// Universal Scene Description.
    Usd,
}

impl Format {
    /// Best guess for the format of `path`, based on its extension.
    ///
    /// Defaults to [`Format::Jt`] for unknown extensions, because JT is the
    /// primary input format of this crate.
    pub fn detect(path: impl AsRef<Path>) -> Self {
        let ext = path
            .as_ref()
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "gltf" => Format::Gltf,
            "glb" => Format::Glb,
            "usd" | "usda" | "usdc" => Format::Usd,
            _ => Format::Jt,
        }
    }

    /// Canonical file extension, without the leading dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Jt => "jt",
            Format::Gltf => "gltf",
            Format::Glb => "glb",
            Format::Usd => "usda",
        }
    }

    /// `true` if [`crate::open_format`] can read this format today.
    pub fn is_readable(self) -> bool {
        matches!(self, Format::Jt)
    }

    /// `true` if [`crate::export`] can write this format today.
    pub fn is_writable(self) -> bool {
        matches!(self, Format::Gltf | Format::Glb)
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Format::Jt => "jt",
            Format::Gltf => "gltf",
            Format::Glb => "glb",
            Format::Usd => "usd",
        };
        f.write_str(name)
    }
}

impl FromStr for Format {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "jt" | "openjt" => Ok(Format::Jt),
            "gltf" => Ok(Format::Gltf),
            "glb" => Ok(Format::Glb),
            "usd" | "usda" | "usdc" => Ok(Format::Usd),
            other => Err(Error::UnsupportedFormat(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_uses_extension() {
        assert_eq!(Format::detect("a.jt"), Format::Jt);
        assert_eq!(Format::detect("a.GLB"), Format::Glb);
        assert_eq!(Format::detect("a.usda"), Format::Usd);
        assert_eq!(Format::detect("no-extension"), Format::Jt);
    }

    #[test]
    fn round_trips_through_name() {
        for format in [Format::Jt, Format::Gltf, Format::Glb, Format::Usd] {
            assert_eq!(format.to_string().parse::<Format>().unwrap(), format);
        }
    }
}
