//! Format agnostic scene model: the intermediate representation that every
//! reader produces and every exporter consumes.

mod bounds;
mod format;
mod geometry;
mod material;
mod scene;

pub use bounds::Aabb;
pub use format::Format;
pub use geometry::{Accessors, Attribute, BufferBlock, ComponentType, Mesh, Primitive, Topology};
pub use material::{Channel, Material, Texture};
pub use scene::{Instance, Model, Node, Scene, Transform};
