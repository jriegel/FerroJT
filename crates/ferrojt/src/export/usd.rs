//! USD (Universal Scene Description) export &mdash; placeholder.
//!
//! USD is the second target format after glTF. Nothing here writes a real
//! `.usda` / `.usdc` / `.usdz` yet; the module exists so that
//! [`Format::Usd`](crate::model::Format::Usd) has a single, obvious home.
//!
//! ## Planned shape
//!
//! The scene layer maps almost one to one onto USD:
//!
//! | model | USD |
//! |---|---|
//! | [`Model`](crate::model::Model) | one stage, `metersPerUnit` from `Model::unit`, `upAxis` from `Model::up_axis` |
//! | [`Scene`](crate::model::Scene) | `def Xform "..."` roots under the default prim |
//! | [`Node`](crate::model::Node) | `Xform` (translate / rotate / scale ops) |
//! | [`Mesh`](crate::model::Mesh) | `Mesh` with `points`, `normals`, `faceVertexIndices`, `faceVertexCounts`, `primvars:st` |
//! | [`Material`](crate::model::Material) | `Material` + `Shader` (`UsdPreviewSurface`) bound via a `MaterialBindingAPI` |
//! | [`Texture`](crate::model::Texture) | file path or in memory asset referenced by the shader |
//!
//! Candidate crates to build on (not chosen yet, and deliberately not
//! dependencies so the scaffold stays buildable):
//!
//! * a text (`.usda`) writer implemented here, which needs no dependency and is
//!   the easiest path to something readable, or
//! * a native USD binding crate, which needs the USD SDK on the build machine.
//!
//! Until then [`UsdExporter`] returns [`Error::NotImplemented`].

use crate::error::{Error, Result};
use crate::export::Exporter;
use crate::model::{Format, Model};

/// Writes a [`Model`] as USD. Currently a stub.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsdExporter;

impl UsdExporter {
    /// Serialize to USD bytes. Always fails until the writer exists.
    pub fn to_bytes(&self, _model: &Model) -> Result<Vec<u8>> {
        Err(Error::not_implemented("writing USD files"))
    }
}

impl Exporter for UsdExporter {
    const FORMAT: Format = Format::Usd;

    fn write(&self, _model: &Model, _out: &mut dyn std::io::Write) -> Result<()> {
        Err(Error::not_implemented("writing USD files"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_export_is_a_stub() {
        let err = UsdExporter.to_bytes(&Model::new()).unwrap_err();
        assert!(matches!(err, Error::NotImplemented(_)));
    }
}
