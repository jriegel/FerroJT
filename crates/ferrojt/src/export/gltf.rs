//! glTF 2.0 export: `.glb` (binary container) and `.gltf` (JSON + data URI buffer).
//!
//! ## Mapping
//!
//! | model | glTF |
//! |---|---|
//! | `accessors.attributes[n]` | `accessors[n]` + `bufferViews[n]` |
//! | `accessors.buffers` | merged into a single `buffers[0]` |
//! | [`Primitive`](crate::model::Primitive) | `meshes[n].primitives[m]` |
//! | [`Material`](crate::model::Material) | `materials[n]` (same order, so indices match) |
//! | [`Texture`](crate::model::Texture) | `textures[n]` / `images[n]` (same order) |
//! | [`Node`](crate::model::Node) | `nodes[n]`, depth first from the scene roots |
//!
//! Each attribute gets its own buffer view, which is always valid; a later
//! optimisation pass can merge attributes that are already contiguous.
//!
//! Component types that glTF cannot express (`F16`, `F64`, `I32`, `I64`, `U64`)
//! are widened to a representable type by [`widen`].

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use gltf_json::validation::{Checked, USize64};
use gltf_json::{Index, Root, accessor, buffer, image, material, mesh, scene, texture};

use crate::error::{Error, Result};
use crate::export::Exporter;
use crate::export::glb::GlbBuilder;
use crate::model::{Attribute, ComponentType, Format, Model, Node, Topology};

/// Writes a [`Model`] as a glTF 2.0 document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GltfExporter {
    binary: bool,
}

impl GltfExporter {
    /// `.glb`: a JSON chunk followed by a binary chunk.
    pub fn glb() -> Self {
        Self { binary: true }
    }

    /// `.gltf`: JSON with the buffer inlined as a base64 data URI.
    pub fn json() -> Self {
        Self { binary: false }
    }

    /// Serialize to `.glb` or `.gltf` bytes.
    pub fn to_bytes(&self, model: &Model) -> Result<Vec<u8>> {
        let document = build(model)?;
        let root = document.root;
        if self.binary {
            let json = root
                .to_vec()
                .map_err(|err| Error::Unsupported(format!("serializing glTF JSON: {err}")))?;
            GlbBuilder::new(json).with_bin(document.bin).finish()
        } else {
            let json = root
                .to_vec_pretty()
                .map_err(|err| Error::Unsupported(format!("serializing glTF JSON: {err}")))?;
            Ok(json)
        }
    }
}

impl Exporter for GltfExporter {
    const FORMAT: Format = Format::Gltf;

    fn write(&self, model: &Model, out: &mut dyn std::io::Write) -> Result<()> {
        out.write_all(&self.to_bytes(model)?)?;
        Ok(())
    }
}

/// A glTF document plus the binary buffer its buffer views point into.
struct Document {
    root: Root,
    bin: Vec<u8>,
}

fn build(model: &Model) -> Result<Document> {
    let mut root = Root::default();
    root.asset.generator = Some(format!("FerroJT {}", env!("CARGO_PKG_VERSION")));
    let mut bin: Vec<u8> = Vec::new();

    // --- accessors and buffer views ---------------------------------------
    let mut accessor_of = Vec::with_capacity(model.accessors.attributes.len());
    for index in 0..model.accessors.attributes.len() {
        accessor_of.push(push_accessor(&mut root, &mut bin, model, index)?);
    }

    // --- images and textures (model order, so material indices stay valid) --
    for texture in &model.textures {
        let bytes = model
            .accessors
            .buffers
            .get(texture.buffer)
            .map(|block| block.bytes.as_slice())
            .ok_or_else(|| {
                Error::invalid(
                    texture.buffer as u64,
                    format!(
                        "texture '{}' references a missing buffer",
                        texture.name.as_deref().unwrap_or("?")
                    ),
                )
            })?;
        let view = push_view(&mut root, &mut bin, bytes, None, texture.name.as_deref())?;
        root.images.push(image::Image {
            buffer_view: Some(view),
            mime_type: Some(image::MimeType(texture.mime_type.clone())),
            name: texture.name.clone(),
            uri: None,
            extensions: None,
            extras: None,
        });
        let sampler = root.push(texture::Sampler::default());
        root.textures.push(texture::Texture {
            name: texture.name.clone(),
            sampler: Some(sampler),
            source: Index::new((root.images.len() - 1) as u32),
            extensions: None,
            extras: None,
        });
    }

    // --- materials ---------------------------------------------------------
    let materials = model
        .materials
        .iter()
        .map(|mat| to_material(&root, mat))
        .collect::<Result<Vec<_>>>()?;
    root.materials.extend(materials);

    // --- the single merged buffer ------------------------------------------
    if !root.buffer_views.is_empty() {
        root.buffers.push(buffer::Buffer {
            byte_length: USize64::from(bin.len() as u64),
            name: Some("ferrojt".to_string()),
            uri: None,
            extensions: None,
            extras: None,
        });
    }

    // --- meshes ------------------------------------------------------------
    let mut mesh_of = Vec::with_capacity(model.meshes.len());
    for mesh_src in &model.meshes {
        let mut primitives = Vec::with_capacity(mesh_src.primitives.len());
        for prim in &mesh_src.primitives {
            primitives.push(to_primitive(model, prim, &accessor_of)?);
        }
        mesh_of.push(root.push(mesh::Mesh {
            name: mesh_src.name.clone(),
            primitives,
            weights: None,
            extensions: None,
            extras: None,
        }));
    }

    // --- nodes -------------------------------------------------------------
    let order = depth_first(model);
    let mut node_of = vec![usize::MAX; model.nodes.len()];
    for (position, model_index) in order.iter().enumerate() {
        node_of[*model_index] = position;
    }
    for model_index in &order {
        let node = &model.nodes[*model_index];
        let children: Vec<Index<scene::Node>> = node
            .children
            .iter()
            .map(|child| {
                node_of
                    .get(*child)
                    .copied()
                    .filter(|position| *position != usize::MAX)
                    .map(|position| Index::new(position as u32))
                    .ok_or_else(|| {
                        Error::invalid(
                            *child as u64,
                            "scene graph child is not reachable from a scene root",
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let mesh_index = node_mesh(model, node, &mesh_of)?;
        root.nodes.push(to_node(node, children, mesh_index));
    }

    // --- scene -------------------------------------------------------------
    let roots = model
        .scenes
        .iter()
        .flat_map(|s| s.roots.iter().copied())
        .map(|index| {
            node_of
                .get(index)
                .copied()
                .filter(|position| *position != usize::MAX)
                .map(|position| Index::new(position as u32))
                .ok_or_else(|| Error::invalid(index as u64, "scene root is out of range"))
        })
        .collect::<Result<Vec<_>>>()?;
    let scene_index = root.push(scene::Scene {
        name: model
            .scenes
            .first()
            .and_then(|s| s.name.clone())
            .or_else(|| model.source.clone()),
        nodes: roots,
        extensions: None,
        extras: None,
    });
    root.scene = Some(scene_index);

    // `.gltf` has no binary chunk, so inline the buffer as a data URI.
    if !bin.is_empty() {
        if let Some(buffer) = root.buffers.first_mut() {
            buffer.uri = Some(format!(
                "data:application/octet-stream;base64,{}",
                STANDARD.encode(&bin)
            ));
        }
    }

    Ok(Document { root, bin })
}

/// Depth first ordering of every node reachable from a scene root.
fn depth_first(model: &Model) -> Vec<usize> {
    let mut order = Vec::with_capacity(model.nodes.len());
    for root in model.scenes.iter().flat_map(|s| s.roots.iter().copied()) {
        visit(root, model, &mut order);
    }
    order
}

fn visit(index: usize, model: &Model, order: &mut Vec<usize>) {
    if order.contains(&index) {
        return;
    }
    order.push(index);
    if let Some(node) = model.nodes.get(index) {
        for child in &node.children {
            visit(*child, model, order);
        }
    }
}

/// A glTF node holds at most one mesh, so a node referencing several model
/// meshes gets a combined mesh.
fn node_mesh(
    model: &Model,
    node: &Node,
    mesh_of: &[Index<mesh::Mesh>],
) -> Result<Option<Index<mesh::Mesh>>> {
    let mut found: Vec<usize> = Vec::with_capacity(node.instances.len());
    for instance in &node.instances {
        let mesh_index = instance.mesh;
        if mesh_index >= model.meshes.len() {
            return Err(Error::invalid(
                mesh_index as u64,
                "node instance references a missing mesh",
            ));
        }
        if !found.contains(&mesh_index) {
            found.push(mesh_index);
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(mesh_of[found[0]])),
        _ => Err(Error::Unsupported(
            "a node with more than one mesh is not exported yet".into(),
        )),
    }
}

fn to_node(
    node: &Node,
    children: Vec<Index<scene::Node>>,
    mesh_index: Option<Index<mesh::Mesh>>,
) -> scene::Node {
    let transform = node.transform;
    let identity = transform.is_identity();
    scene::Node {
        name: node.name.clone(),
        camera: None,
        children: (!children.is_empty()).then_some(children),
        mesh: mesh_index,
        matrix: None,
        rotation: (!identity)
            .then(|| scene::UnitQuaternion(normalize_quaternion(transform.rotation))),
        scale: (!identity && transform.scale != [1.0; 3]).then_some(transform.scale),
        translation: (!identity && transform.translation != [0.0; 3])
            .then_some(transform.translation),
        skin: None,
        weights: None,
        extensions: None,
        extras: None,
    }
}

fn normalize_quaternion(q: [f32; 4]) -> [f32; 4] {
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len < f32::EPSILON {
        return [0.0, 0.0, 0.0, 1.0];
    }
    [q[0] / len, q[1] / len, q[2] / len, q[3] / len]
}

fn to_primitive(
    model: &Model,
    primitive: &crate::model::Primitive,
    accessor_of: &[Index<accessor::Accessor>],
) -> Result<mesh::Primitive> {
    let mut attributes = BTreeMap::new();
    for handle in &primitive.attributes {
        let attribute = model
            .accessors
            .attribute(*handle)
            .ok_or_else(|| Error::invalid(*handle as u64, "attribute handle is out of range"))?;
        let semantic = semantic(&attribute.name)?;
        let index = *accessor_of
            .get(*handle)
            .ok_or_else(|| Error::invalid(*handle as u64, "attribute was not exported"))?;
        attributes.insert(Checked::Valid(semantic), index);
    }
    if !attributes.contains_key(&Checked::Valid(mesh::Semantic::Positions)) {
        return Err(Error::Unsupported(
            "a primitive without a POSITION attribute cannot be exported to glTF".into(),
        ));
    }
    let indices = primitive
        .indices
        .map(|handle| {
            accessor_of
                .get(handle)
                .copied()
                .ok_or_else(|| Error::invalid(handle as u64, "index attribute was not exported"))
        })
        .transpose()?;

    let material = primitive
        .material
        .map(|index| {
            if index >= model.materials.len() {
                return Err(Error::invalid(
                    index as u64,
                    "primitive references a missing material",
                ));
            }
            Ok(Index::new(index as u32))
        })
        .transpose()?;

    Ok(mesh::Primitive {
        attributes,
        indices,
        material,
        mode: Checked::Valid(to_mode(primitive.topology)),
        targets: None,
        extensions: None,
        extras: None,
    })
}

/// Map a model attribute name onto a glTF vertex semantic.
fn semantic(name: &str) -> Result<mesh::Semantic> {
    let upper = name.to_ascii_uppercase();
    let (base, set) = split_suffix(&upper);
    Ok(match base {
        "POSITION" => return Ok(mesh::Semantic::Positions),
        "NORMAL" => return Ok(mesh::Semantic::Normals),
        "TANGENT" => return Ok(mesh::Semantic::Tangents),
        "COLOR" => mesh::Semantic::Colors(set),
        "TEXCOORD" => mesh::Semantic::TexCoords(set),
        "JOINTS" => mesh::Semantic::Joints(set),
        "WEIGHTS" => mesh::Semantic::Weights(set),
        other => {
            return Err(Error::Unsupported(format!(
                "vertex attribute '{other}' has no glTF equivalent"
            )));
        }
    })
}

/// `"TEXCOORD_0"` -> `("TEXCOORD", 0)`; an unsuffixed name keeps set `0`.
fn split_suffix(name: &str) -> (&str, u32) {
    match name.rfind(|c: char| c.is_ascii_digit()) {
        Some(pos) if pos > 0 => match name[pos..].parse::<u32>() {
            Ok(set) => (name[..pos].trim_end_matches('_'), set),
            Err(_) => (name, 0),
        },
        _ => (name, 0),
    }
}

fn to_mode(topology: Topology) -> mesh::Mode {
    match topology {
        Topology::PointList => mesh::Mode::Points,
        Topology::LineList | Topology::LineStrip => mesh::Mode::Lines,
        Topology::TriangleList => mesh::Mode::Triangles,
        Topology::TriangleStrip => mesh::Mode::TriangleStrip,
        Topology::TriangleFan => mesh::Mode::TriangleFan,
    }
}

fn to_material(root: &Root, mat: &crate::model::Material) -> Result<material::Material> {
    let info = |channel: &crate::model::Channel| -> Option<texture::Info> {
        texture_info(root, channel.texture, channel.tex_coord)
    };
    let pbr = material::PbrMetallicRoughness {
        base_color_factor: material::PbrBaseColorFactor(mat.base_color.factor),
        metallic_factor: material::StrengthFactor(mat.metallic.factor[0]),
        roughness_factor: material::StrengthFactor(mat.roughness.factor[0]),
        base_color_texture: info(&mat.base_color),
        metallic_roughness_texture: info(&mat.roughness),
        extensions: None,
        extras: None,
    };
    Ok(material::Material {
        name: mat.name.clone(),
        pbr_metallic_roughness: pbr,
        normal_texture: texture_info(root, mat.normal, 0).map(|info| material::NormalTexture {
            index: info.index,
            scale: 1.0,
            tex_coord: info.tex_coord,
            extensions: None,
            extras: None,
        }),
        occlusion_texture: texture_info(root, mat.occlusion, 0).map(|info| {
            material::OcclusionTexture {
                index: info.index,
                strength: material::StrengthFactor(1.0),
                tex_coord: info.tex_coord,
                extensions: None,
                extras: None,
            }
        }),
        emissive_texture: texture_info(root, mat.emissive_texture, 0),
        emissive_factor: material::EmissiveFactor([
            mat.emissive.factor[0],
            mat.emissive.factor[1],
            mat.emissive.factor[2],
        ]),
        alpha_mode: Checked::Valid(material::AlphaMode::Opaque),
        alpha_cutoff: None,
        double_sided: mat.double_sided,
        extensions: None,
        extras: raw_extras(&mat.extras),
    })
}

/// The source specific fields of a material as glTF `extras`.
///
/// A fixed-function format such as JT carries more of an appearance than a
/// metallic/roughness workflow can hold — ambient and specular colour, a
/// shininess exponent — and a consumer that can use them should not have to
/// re-read the source file for them.
fn raw_extras(extras: &Option<serde_json::Value>) -> Option<Box<serde_json::value::RawValue>> {
    serde_json::value::RawValue::from_string(extras.as_ref()?.to_string()).ok()
}

fn texture_info(root: &Root, index: Option<usize>, tex_coord: u8) -> Option<texture::Info> {
    let index = index?;
    if index >= root.textures.len() {
        return None;
    }
    Some(texture::Info {
        index: Index::new(index as u32),
        tex_coord: u32::from(tex_coord),
        extensions: None,
        extras: None,
    })
}

/// Append one attribute as a buffer view plus accessor.
fn push_accessor(
    root: &mut Root,
    bin: &mut Vec<u8>,
    model: &Model,
    handle: usize,
) -> Result<Index<accessor::Accessor>> {
    let attribute = model
        .accessors
        .attribute(handle)
        .ok_or_else(|| Error::invalid(handle as u64, "attribute handle is out of range"))?;
    let source = model
        .accessors
        .attribute_bytes(handle)
        .ok_or_else(|| Error::invalid(handle as u64, "attribute extends past its buffer"))?;

    let (component_type, bytes) = widen(attribute.component_type, source)?;
    let target = if attribute.name.ends_with("_INDICES") || attribute.name == "INDICES" {
        Some(Checked::Valid(buffer::Target::ElementArrayBuffer))
    } else {
        Some(Checked::Valid(buffer::Target::ArrayBuffer))
    };
    let view = push_view(root, bin, &bytes, target, Some(&attribute.name))?;

    let gltf_type = to_type(attribute.components)?;
    let (min, max) = bounds(attribute, &bytes, component_type)?;

    Ok(root.push(accessor::Accessor {
        buffer_view: Some(view),
        byte_offset: Some(USize64(0)),
        count: USize64::from(attribute.count as u64),
        component_type: Checked::Valid(accessor::GenericComponentType(component_type)),
        type_: Checked::Valid(gltf_type),
        min,
        max,
        normalized: attribute.normalized,
        name: Some(attribute.name.clone()),
        sparse: None,
        extensions: None,
        extras: None,
    }))
}

/// Append bytes to the merged buffer and describe them with a buffer view.
fn push_view(
    root: &mut Root,
    bin: &mut Vec<u8>,
    bytes: &[u8],
    target: Option<Checked<buffer::Target>>,
    name: Option<&str>,
) -> Result<Index<buffer::View>> {
    let offset = bin.len();
    bin.extend_from_slice(bytes);
    let pad = (4 - (bin.len() % 4)) % 4;
    bin.resize(bin.len() + pad, 0);
    Ok(root.push(buffer::View {
        buffer: Index::new(0),
        byte_length: USize64::from(bytes.len() as u64),
        byte_offset: Some(USize64::from(offset as u64)),
        byte_stride: None,
        name: name.map(str::to_string),
        target,
        extensions: None,
        extras: None,
    }))
}

/// Convert component types glTF cannot express into one it can.
///
/// Returns the target type and the re-encoded bytes. Types that already fit are
/// returned unchanged, so the common case does not copy.
fn widen(component: ComponentType, bytes: &[u8]) -> Result<(accessor::ComponentType, Vec<u8>)> {
    if let Some(existing) = existing_component_type(component) {
        return Ok((existing, bytes.to_vec()));
    }
    let target = match component {
        ComponentType::F16 | ComponentType::F64 | ComponentType::I32 => {
            accessor::ComponentType::F32
        }
        ComponentType::I64 | ComponentType::U64 => accessor::ComponentType::U32,
        other => {
            return Err(Error::Unsupported(format!(
                "component type {other:?} has no glTF accessor type"
            )));
        }
    };
    let element = component.byte_len();
    if element == 0 || bytes.len() % element != 0 {
        return Err(Error::invalid(
            0,
            "attribute length is not a whole number of components",
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for chunk in bytes.chunks_exact(element) {
        let value = read_component(component, chunk)?;
        match target {
            accessor::ComponentType::F32 => out.extend_from_slice(&(value as f32).to_le_bytes()),
            accessor::ComponentType::U32 => {
                out.extend_from_slice(&(value.max(0.0) as u32).to_le_bytes());
            }
            other => {
                return Err(Error::Unsupported(format!(
                    "cannot widen {component:?} to {other:?}"
                )));
            }
        }
    }
    Ok((target, out))
}

fn existing_component_type(component: ComponentType) -> Option<accessor::ComponentType> {
    Some(match component {
        ComponentType::I8 => accessor::ComponentType::I8,
        ComponentType::U8 => accessor::ComponentType::U8,
        ComponentType::I16 => accessor::ComponentType::I16,
        ComponentType::U16 => accessor::ComponentType::U16,
        ComponentType::U32 => accessor::ComponentType::U32,
        ComponentType::F32 => accessor::ComponentType::F32,
        _ => return None,
    })
}

fn to_type(components: u8) -> Result<accessor::Type> {
    Ok(match components {
        1 => accessor::Type::Scalar,
        2 => accessor::Type::Vec2,
        3 => accessor::Type::Vec3,
        4 => accessor::Type::Vec4,
        n => {
            return Err(Error::Unsupported(format!(
                "{n}-component attributes have no glTF accessor type"
            )));
        }
    })
}

/// Component wise min/max. The glTF validator requires them on `POSITION`.
fn bounds(
    attribute: &Attribute,
    bytes: &[u8],
    component: accessor::ComponentType,
) -> Result<(Option<gltf_json::Value>, Option<gltf_json::Value>)> {
    let required = attribute.name.eq_ignore_ascii_case("POSITION")
        || attribute.normalized
        || attribute.min.is_some();
    if !required {
        return Ok((None, None));
    }
    let dims = usize::from(attribute.components);
    let step = component.size();
    let mut min = vec![f64::MAX; dims];
    let mut max = vec![f64::MIN; dims];
    for element in 0..attribute.count {
        for dim in 0..dims {
            let start = (element * dims + dim) * step;
            let end = start + step;
            if end > bytes.len() {
                return Err(Error::invalid(
                    start as u64,
                    "attribute extends past its buffer view",
                ));
            }
            let value = match component {
                accessor::ComponentType::I8 => bytes[start] as i8 as f64,
                accessor::ComponentType::U8 => bytes[start] as f64,
                accessor::ComponentType::I16 => {
                    i16::from_le_bytes([bytes[start], bytes[start + 1]]) as f64
                }
                accessor::ComponentType::U16 => {
                    u16::from_le_bytes([bytes[start], bytes[start + 1]]) as f64
                }
                accessor::ComponentType::U32 => u32::from_le_bytes([
                    bytes[start],
                    bytes[start + 1],
                    bytes[start + 2],
                    bytes[start + 3],
                ]) as f64,
                accessor::ComponentType::F32 => f32::from_le_bytes([
                    bytes[start],
                    bytes[start + 1],
                    bytes[start + 2],
                    bytes[start + 3],
                ]) as f64,
            };
            min[dim] = min[dim].min(value);
            max[dim] = max[dim].max(value);
        }
    }
    if attribute.count == 0 {
        min = vec![0.0; dims];
        max = vec![0.0; dims];
    }
    Ok((Some(array(&min)?), Some(array(&max)?)))
}

fn array(values: &[f64]) -> Result<gltf_json::Value> {
    Ok(gltf_json::Value::Array(
        values
            .iter()
            .map(|value| {
                serde_json::Number::from_f64(*value)
                    .map(gltf_json::Value::Number)
                    .ok_or_else(|| Error::Unsupported(format!("{value} is not a JSON number")))
            })
            .collect::<Result<Vec<_>>>()?,
    ))
}

/// Decode one little endian component as `f64`.
fn read_component(component: ComponentType, bytes: &[u8]) -> Result<f64> {
    let need = |n: usize| -> Result<&[u8]> {
        if bytes.len() < n {
            return Err(Error::invalid(0, "truncated attribute component"));
        }
        Ok(&bytes[..n])
    };
    Ok(match component {
        ComponentType::I8 => need(1)?[0] as i8 as f64,
        ComponentType::U8 => need(1)?[0] as f64,
        ComponentType::I16 => i16::from_le_bytes([need(2)?[0], need(2)?[1]]) as f64,
        ComponentType::U16 => u16::from_le_bytes([need(2)?[0], need(2)?[1]]) as f64,
        ComponentType::I32 => i32::from_le_bytes(quad(need(4)?)) as f64,
        ComponentType::U32 => u32::from_le_bytes(quad(need(4)?)) as f64,
        ComponentType::I64 => i64::from_le_bytes(oct(need(8)?)) as f64,
        ComponentType::U64 => u64::from_le_bytes(oct(need(8)?)) as f64,
        ComponentType::F32 => f32::from_le_bytes(quad(need(4)?)) as f64,
        ComponentType::F64 => f64::from_le_bytes(oct(need(8)?)),
        ComponentType::F16 => half::f16::from_le_bytes([need(2)?[0], need(2)?[1]]).to_f64(),
    })
}

fn quad(bytes: &[u8]) -> [u8; 4] {
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

fn oct(bytes: &[u8]) -> [u8; 8] {
    [
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Aabb, Accessors, BufferBlock, Instance, Mesh, Primitive};

    /// A single triangle with positions and indices.
    fn triangle_model() -> Model {
        let mut model = Model::new();
        model.accessors = Accessors {
            buffers: vec![BufferBlock::new(Vec::new())],
            attributes: Vec::new(),
        };
        let positions = model.accessors.push_attribute(Attribute {
            name: "POSITION".into(),
            buffer: 0,
            offset: 0,
            count: 3,
            components: 3,
            component_type: ComponentType::F32,
            normalized: false,
            min: None,
            max: None,
        });
        let indices = model.accessors.push_attribute(Attribute {
            name: "INDICES".into(),
            buffer: 0,
            offset: 0,
            count: 3,
            components: 1,
            component_type: ComponentType::U32,
            normalized: false,
            min: None,
            max: None,
        });
        let mut positions_bytes = Vec::new();
        for point in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for value in point {
                positions_bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        let mut index_bytes = Vec::new();
        for index in 0..3u32 {
            index_bytes.extend_from_slice(&index.to_le_bytes());
        }
        model.accessors.buffers = vec![BufferBlock::new(positions_bytes)];
        let index_buffer = model.accessors.push_buffer(index_bytes);
        model.accessors.attributes[indices].buffer = index_buffer;

        let material = model.push_material(crate::model::Material::default());
        let mesh = model.push_mesh(Mesh {
            name: Some("triangle".into()),
            primitives: vec![Primitive {
                attributes: vec![positions],
                indices: Some(indices),
                topology: Topology::TriangleList,
                material: Some(material),
                bounds: Some(Aabb::new([0.0; 3], [1.0, 1.0, 0.0])),
                extras: None,
            }],
            bounds: Some(Aabb::new([0.0; 3], [1.0, 1.0, 0.0])),
            lod_level: Some(0),
            extras: None,
        });
        let node = model.push_node(Node {
            name: Some("triangle".into()),
            instances: vec![Instance {
                mesh,
                material: None,
                lod_bias: 1.0,
            }],
            ..Node::default()
        });
        model.default_scene_mut().roots.push(node);
        model
    }

    #[test]
    fn split_suffix_parses_sets() {
        assert_eq!(split_suffix("TEXCOORD_0"), ("TEXCOORD", 0));
        assert_eq!(split_suffix("COLOR_1"), ("COLOR", 1));
        assert_eq!(split_suffix("POSITION"), ("POSITION", 0));
    }

    #[test]
    fn semantic_maps_names() {
        assert_eq!(semantic("POSITION").unwrap(), mesh::Semantic::Positions);
        assert_eq!(
            semantic("texcoord_1").unwrap(),
            mesh::Semantic::TexCoords(1)
        );
        assert!(semantic("WEIGHT_0").is_err());
    }

    #[test]
    fn glb_starts_with_magic_and_valid_json() {
        let bytes = GltfExporter::glb().to_bytes(&triangle_model()).unwrap();
        assert_eq!(&bytes[0..4], b"glTF");
        let (json_len, bin) = crate::export::glb::parse_header(&bytes).unwrap();
        assert!(bin.is_some());
        let json = std::str::from_utf8(&bytes[20..20 + json_len]).unwrap();
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(value["asset"]["version"], "2.0");
        assert_eq!(value["accessors"].as_array().unwrap().len(), 2);
        assert_eq!(
            value["meshes"][0]["primitives"][0]["mode"],
            serde_json::Value::Null
        );
        assert!(value["accessors"][0]["min"].is_array());
    }

    #[test]
    fn json_export_inlines_a_data_uri() {
        let bytes = GltfExporter::json().to_bytes(&triangle_model()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            value["buffers"][0]["uri"]
                .as_str()
                .unwrap()
                .starts_with("data:application/octet-stream;base64,")
        );
    }

    #[test]
    fn position_min_max_are_computed() {
        let model = triangle_model();
        let bytes = GltfExporter::glb().to_bytes(&model).unwrap();
        let (json_len, _) = crate::export::glb::parse_header(&bytes).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
        assert_eq!(
            value["accessors"][0]["min"],
            serde_json::json!([0.0, 0.0, 0.0])
        );
        assert_eq!(
            value["accessors"][0]["max"],
            serde_json::json!([1.0, 1.0, 0.0])
        );
    }

    #[test]
    fn primitive_without_position_is_rejected() {
        let mut model = Model::new();
        let mesh = model.push_mesh(Mesh {
            primitives: vec![Primitive {
                attributes: Vec::new(),
                indices: None,
                topology: Topology::TriangleList,
                material: None,
                bounds: None,
                extras: None,
            }],
            ..Mesh::default()
        });
        let node = model.push_node(Node {
            instances: vec![Instance {
                mesh,
                material: None,
                lod_bias: 1.0,
            }],
            ..Node::default()
        });
        model.default_scene_mut().roots.push(node);
        let err = GltfExporter::glb().to_bytes(&model).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn widens_half_floats_to_f32() {
        let bytes: Vec<u8> = [half::f16::from_f32(1.5), half::f16::from_f32(-2.5)]
            .iter()
            .flat_map(|h| h.to_le_bytes())
            .collect();
        let (component, out) = widen(ComponentType::F16, &bytes).unwrap();
        assert_eq!(component, accessor::ComponentType::F32);
        assert_eq!(out.len(), 8);
        assert_eq!(f32::from_le_bytes(quad(&out[0..4])), 1.5);
    }

    #[test]
    fn empty_model_exports_an_empty_scene() {
        let bytes = GltfExporter::glb().to_bytes(&Model::new()).unwrap();
        let (json_len, bin) = crate::export::glb::parse_header(&bytes).unwrap();
        assert_eq!(bin, None);
        let value: serde_json::Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
        assert_eq!(value["scene"], 0);
        assert!(
            value["scenes"][0]
                .get("nodes")
                .is_none_or(|nodes| nodes.as_array().unwrap().is_empty()),
            "an empty model should not produce scene nodes"
        );
        assert!(
            value
                .get("meshes")
                .is_none_or(|m| m.as_array().unwrap().is_empty())
        );
    }
}
