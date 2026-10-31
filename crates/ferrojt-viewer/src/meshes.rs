//! Decode the neutral FerroJT model straight into kiss3d GPU meshes.
//!
//! The viewer never goes through glTF: the attribute buffers of
//! [`Model::accessors`](ferrojt::model::Model) are read directly and handed to
//! [`GpuMesh3d`]. Each primitive becomes one `GpuMesh3d` wrapped in an
//! `Rc<RefCell<..>>`, so an instanced subtree shares the vertex data instead of
//! copying it.

use std::cell::RefCell;
use std::rc::Rc;

use ferrojt::model::{Aabb, Attribute, ComponentType, Model, Primitive, Topology};
use kiss3d::prelude::*;

/// A decoded primitive: geometry plus the material it should be drawn with.
pub struct Decoded {
    /// Shared between every instance of the mesh.
    pub mesh: Rc<RefCell<GpuMesh3d>>,
    /// Index into [`Model::materials`], when the primitive carries one.
    pub material: Option<usize>,
    /// Local space bounds of this primitive.
    pub bounds: Option<Aabb>,
}

/// Decode every primitive of every mesh of `model`.
///
/// The outer vector is indexed like [`Model::meshes`]; the inner one holds the
/// primitives that actually have a `POSITION` attribute (a JT representation
/// without vertices yields nothing, matching the reader's policy).
pub fn build(model: &Model) -> Vec<Vec<Decoded>> {
    model
        .meshes
        .iter()
        .map(|mesh| {
            mesh.primitives
                .iter()
                .filter_map(|primitive| decode(model, primitive))
                .collect()
        })
        .collect()
}

/// Number of triangles a decoded model would draw.
pub fn triangle_count(decoded: &[Vec<Decoded>]) -> usize {
    decoded
        .iter()
        .flat_map(|primitives| primitives.iter())
        .map(|entry| entry.mesh.borrow().num_indices() as usize / 3)
        .sum()
}

fn decode(model: &Model, primitive: &Primitive) -> Option<Decoded> {
    let positions = attribute_floats(model, primitive, "POSITION")?;
    if positions.is_empty() {
        return None;
    }

    let coords: Vec<Vec3> = positions
        .chunks_exact(3)
        .map(|c| Vec3::new(c[0], c[1], c[2]))
        .collect();
    if coords.is_empty() {
        return None;
    }

    let indices = primitive.indices.and_then(|index| {
        let attribute = model.accessors.attribute(index)?;
        let bytes = model.accessors.attribute_bytes(index)?;
        Some(
            decode_floats(attribute, bytes)
                .iter()
                .map(|v| (*v as i64) as u32)
                .collect::<Vec<u32>>(),
        )
    });

    let faces = faces(primitive.topology, coords.len(), indices.as_deref());
    if faces.is_empty() {
        return None;
    }

    // Only use the file's normals when they line up with the vertices;
    // otherwise kiss3d derives them from the faces.
    let normals = attribute_floats(model, primitive, "NORMAL").and_then(|values| {
        if values.len() != coords.len() * 3 {
            return None;
        }
        Some(
            values
                .chunks_exact(3)
                .map(|c| {
                    let n = Vec3::new(c[0], c[1], c[2]);
                    if n.length_squared() > 0.0 {
                        n.normalize()
                    } else {
                        n
                    }
                })
                .collect(),
        )
    });

    let mesh = GpuMesh3d::new(coords, faces, normals, None, false);
    Some(Decoded {
        mesh: Rc::new(RefCell::new(mesh)),
        material: primitive.material,
        bounds: primitive.bounds,
    })
}

/// Resolve the named vertex attribute of a primitive and decode it to `f32`.
fn attribute_floats(model: &Model, primitive: &Primitive, name: &str) -> Option<Vec<f32>> {
    let index = *primitive.attributes.iter().find(|index| {
        model
            .accessors
            .attribute(**index)
            .is_some_and(|attribute| attribute.name.eq_ignore_ascii_case(name))
    })?;
    let attribute = model.accessors.attribute(index)?;
    let bytes = model.accessors.attribute_bytes(index)?;
    let values = decode_floats(attribute, bytes);
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// Little-endian scalar elements of an attribute, widened to `f32`.
fn decode_floats(attribute: &Attribute, bytes: &[u8]) -> Vec<f32> {
    let count = attribute.count * usize::from(attribute.components);
    let size = attribute.component_type.byte_len();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = i * size;
        let Some(raw) = bytes.get(start..start + size) else {
            break;
        };
        out.push(scalar(attribute.component_type, raw));
    }
    out
}

fn scalar(kind: ComponentType, raw: &[u8]) -> f32 {
    match kind {
        ComponentType::I8 => raw
            .first()
            .copied()
            .map(|b| (b as i8) as f32)
            .unwrap_or(0.0),
        ComponentType::U8 => raw.first().copied().map(|b| b as f32).unwrap_or(0.0),
        ComponentType::I16 => i16::from_le_bytes([raw[0], raw[1]]) as f32,
        ComponentType::U16 => u16::from_le_bytes([raw[0], raw[1]]) as f32,
        ComponentType::I32 => i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f32,
        ComponentType::U32 => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f32,
        ComponentType::I64 => i64::from_le_bytes([
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
        ]) as f32,
        ComponentType::U64 => u64::from_le_bytes([
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
        ]) as f32,
        ComponentType::F32 => f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
        ComponentType::F16 => half::f16::from_le_bytes([raw[0], raw[1]]).to_f32(),
        ComponentType::F64 => f64::from_le_bytes([
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
        ]) as f32,
    }
}

/// Triangle index list for a topology, from an explicit index buffer or from
/// the vertex order.
fn faces(topology: Topology, vertex_count: usize, indices: Option<&[u32]>) -> Vec<[u32; 3]> {
    let synthetic;
    let sequence: &[u32] = match indices {
        Some(values) => values,
        None => {
            synthetic = (0..vertex_count as u32).collect::<Vec<u32>>();
            &synthetic
        }
    };

    match topology {
        Topology::TriangleList => sequence
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect(),
        Topology::TriangleStrip => (0..sequence.len().saturating_sub(2))
            .map(|i| {
                // Alternate the winding so every triangle keeps the same facing.
                if i % 2 == 0 {
                    [sequence[i], sequence[i + 1], sequence[i + 2]]
                } else {
                    [sequence[i + 1], sequence[i], sequence[i + 2]]
                }
            })
            .collect(),
        Topology::TriangleFan => (1..sequence.len().saturating_sub(1))
            .map(|i| [sequence[0], sequence[i], sequence[i + 1]])
            .collect(),
        Topology::PointList | Topology::LineList | Topology::LineStrip => Vec::new(),
    }
}
