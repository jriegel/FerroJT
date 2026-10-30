//! Scene hierarchy: model, scenes, nodes, transforms and instances.

use serde::{Deserialize, Serialize};

use super::{Aabb, Accessors, Material, Mesh, Primitive, Texture, Topology};

/// A rigid body transform: translation, rotation and scale.
///
/// Stored in the JT convention (right handed, counter clockwise rotations,
/// quaternion as `x, y, z, w`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Translation in model units.
    pub translation: [f32; 3],
    /// Rotation quaternion, `x, y, z, w`.
    pub rotation: [f32; 4],
    /// Non uniform scale.
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// The identity transform.
    pub const IDENTITY: Transform = Transform {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    };

    /// A pure translation.
    pub fn translated(translation: [f32; 3]) -> Self {
        Self {
            translation,
            ..Self::IDENTITY
        }
    }

    /// `true` if this transform leaves coordinates unchanged.
    pub fn is_identity(&self) -> bool {
        self.translation == [0.0; 3]
            && self.rotation == [0.0, 0.0, 0.0, 1.0]
            && self.scale == [1.0; 3]
    }
}

/// A reference to a mesh, possibly with per instance overrides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    /// Index into [`Model::meshes`].
    pub mesh: usize,
    /// Optional index into [`Model::materials`] overriding the primitive's own.
    pub material: Option<usize>,
    /// Visibility / LOD weight in `[0, 1]`.
    pub lod_bias: f32,
}

/// A node in the scene graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Display name from the source file.
    pub name: Option<String>,
    /// Node kind, free form so readers can carry source specific types.
    pub kind: Option<String>,
    /// Transform relative to the parent node.
    pub transform: Transform,
    /// Geometry drawn at this node.
    pub instances: Vec<Instance>,
    /// Child node indices.
    pub children: Vec<usize>,
    /// Local space bounds, when known.
    pub bounds: Option<Aabb>,
    /// Source specific metadata.
    pub extras: Option<serde_json::Value>,
}

impl Default for Node {
    fn default() -> Self {
        Self {
            name: None,
            kind: None,
            transform: Transform::IDENTITY,
            instances: Vec::new(),
            children: Vec::new(),
            bounds: None,
            extras: None,
        }
    }
}

impl Node {
    /// A named node with an identity transform.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Self::default()
        }
    }
}

/// A named root set of nodes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Scene name.
    pub name: Option<String>,
    /// Root node indices.
    pub roots: Vec<usize>,
}

/// A complete model: nodes, meshes, materials, textures and their data.
///
/// All cross references are indices into the vectors of this struct, which
/// keeps the graph cheap to build and easy to walk in parallel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// Nodes of the scene graph.
    pub nodes: Vec<Node>,
    /// Scenes (root sets).
    pub scenes: Vec<Scene>,
    /// Meshes.
    pub meshes: Vec<Mesh>,
    /// Materials.
    pub materials: Vec<Material>,
    /// Textures.
    pub textures: Vec<Texture>,
    /// Vertex / index / image data.
    pub accessors: Accessors,
    /// Units of the model, e.g. `millimeter`.
    pub unit: Option<String>,
    /// Up axis of the source coordinate system, e.g. `+Z`.
    pub up_axis: Option<String>,
    /// Global bounds.
    pub bounds: Option<Aabb>,
    /// Source file identifier.
    pub source: Option<String>,
    /// Producer metadata (JT version, originator, ...).
    pub extras: Option<serde_json::Value>,
}

impl Model {
    /// An empty model with a single default scene.
    pub fn new() -> Self {
        Self {
            scenes: vec![Scene::default()],
            ..Self::default()
        }
    }

    /// The default scene, creating one if the model has none.
    pub fn default_scene_mut(&mut self) -> &mut Scene {
        if self.scenes.is_empty() {
            self.scenes.push(Scene::default());
        }
        &mut self.scenes[0]
    }

    /// Append a node and return its index.
    pub fn push_node(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Append a mesh and return its index.
    pub fn push_mesh(&mut self, mesh: Mesh) -> usize {
        self.meshes.push(mesh);
        self.meshes.len() - 1
    }

    /// Append a material and return its index.
    pub fn push_material(&mut self, material: Material) -> usize {
        self.materials.push(material);
        self.materials.len() - 1
    }

    /// Return the index of `material`, appending it unless an identical
    /// material is already in the model.
    ///
    /// A source format repeats one appearance across many shapes; keeping a
    /// single entry per distinct material is what a glTF consumer expects.
    pub fn add_material(&mut self, material: Material) -> usize {
        if let Some(index) = self
            .materials
            .iter()
            .position(|existing| *existing == material)
        {
            return index;
        }
        self.push_material(material)
    }

    /// Total number of triangles across all meshes, when index counts are known.
    pub fn triangle_count(&self) -> usize {
        self.meshes
            .iter()
            .flat_map(|mesh| mesh.primitives.iter())
            .map(|primitive| {
                let count = self.primitive_vertex_count(primitive);
                match primitive.topology {
                    Topology::TriangleList | Topology::TriangleFan => count / 3,
                    Topology::TriangleStrip => count.saturating_sub(2),
                    _ => 0,
                }
            })
            .sum()
    }

    /// Total number of vertices across all primitives.
    pub fn vertex_count(&self) -> usize {
        self.meshes
            .iter()
            .flat_map(|mesh| mesh.primitives.iter())
            .map(|primitive| self.primitive_vertex_count(primitive))
            .sum()
    }

    /// Vertex count of a primitive, taken from its index buffer or POSITION.
    fn primitive_vertex_count(&self, primitive: &Primitive) -> usize {
        if let Some(indices) = primitive.indices {
            if let Some(attribute) = self.accessors.attribute(indices) {
                return attribute.count;
            }
        }
        primitive
            .attributes
            .iter()
            .filter_map(|index| self.accessors.attribute(*index))
            .find(|attribute| attribute.name == "POSITION")
            .map(|attribute| attribute.count)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_transform_round_trips() {
        assert!(Transform::IDENTITY.is_identity());
        assert!(!Transform::translated([1.0, 0.0, 0.0]).is_identity());
    }

    #[test]
    fn new_model_has_default_scene() {
        let mut model = Model::new();
        let root = model.push_node(Node::named("root"));
        model.default_scene_mut().roots.push(root);
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.scenes[0].roots, vec![root]);
    }
}
