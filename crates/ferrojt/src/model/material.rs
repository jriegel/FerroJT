//! Appearance: a small physically based material model that maps cleanly onto
//! both glTF `pbrMetallicRoughness` and USD `UsdPreviewSurface`.

use serde::{Deserialize, Serialize};

/// A texture / value channel of a material.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    /// Linear factor applied to the constant.
    pub factor: [f32; 4],
    /// Index into [`Model::textures`](super::Model::textures), when textured.
    pub texture: Option<usize>,
    /// Which UV set the texture samples.
    pub tex_coord: u8,
}

impl Channel {
    /// A channel with a constant factor and no texture.
    pub fn constant(factor: [f32; 4]) -> Self {
        Self {
            factor,
            texture: None,
            tex_coord: 0,
        }
    }
}

/// A material description.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Material {
    /// Material name.
    pub name: Option<String>,
    /// Base colour (linear RGBA).
    pub base_color: Channel,
    /// Metallic factor channel (only `.factor[0]` is used).
    pub metallic: Channel,
    /// Roughness factor channel (only `.factor[0]` is used).
    pub roughness: Channel,
    /// Emissive factor (linear RGB).
    pub emissive: Channel,
    /// Index of an occlusion texture.
    pub occlusion: Option<usize>,
    /// Index of a normal map texture.
    pub normal: Option<usize>,
    /// Index of an emissive texture.
    pub emissive_texture: Option<usize>,
    /// Triangle culling behaviour.
    pub double_sided: bool,
    /// Source specific metadata.
    pub extras: Option<serde_json::Value>,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: None,
            base_color: Channel::constant([1.0, 1.0, 1.0, 1.0]),
            metallic: Channel::constant([0.0, 0.0, 0.0, 1.0]),
            roughness: Channel::constant([0.5, 0.5, 0.5, 1.0]),
            emissive: Channel::constant([0.0, 0.0, 0.0, 1.0]),
            occlusion: None,
            normal: None,
            emissive_texture: None,
            double_sided: false,
            extras: None,
        }
    }
}

/// A texture reference. Pixels live in a [`BufferBlock`](super::BufferBlock).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Texture {
    /// Texture name.
    pub name: Option<String>,
    /// Index of the [`BufferBlock`](super::BufferBlock) holding encoded image bytes.
    pub buffer: usize,
    /// Container of the encoded image, e.g. `image/png`.
    pub mime_type: String,
    /// Source specific metadata.
    pub extras: Option<serde_json::Value>,
}
