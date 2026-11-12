//! The glTF 2.0 reader: `.gltf` and `.glb` in, [`Model`] out.
//!
//! This is the mirror image of [`crate::export::GltfExporter`], so the mapping
//! of names, attributes and materials is the same in both directions. Two
//! differences are worth knowing:
//!
//! - The vertex data is kept exactly as the file stores it — one
//!   [`crate::model::BufferBlock`] per glTF buffer and one
//!   [`crate::model::Attribute`] per accessor, with the accessor's `min` and
//!   `max` carried over as the author wrote them. Nothing is widened.
//! - Images are *not* decoded. A [`crate::model::Texture`] holds the encoded
//!   bytes (PNG/JPEG/...) plus their MIME type, which is what a JT file stores.
//!
//! A node matrix is decomposed into translation, rotation and scale because
//! that is what JT and USD store; the quaternion comes out as `x, y, z, w`,
//! matching [`crate::model::Transform::rotation`].

use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use gltf::Gltf;
use gltf::image::Source as ImageSource;
use gltf::mesh::Semantic;
use gltf::mesh::{Mode, Primitive as GltfPrimitive};
use gltf::scene::Transform;
use gltf_json::accessor::ComponentType as GltfComponentType;
use gltf_json::accessor::Type as GltfDimensions;

use crate::error::{Error, Result};
use crate::model::{
    Aabb, Attribute, Channel, ComponentType, Instance, Material, Mesh, Model, Node, Primitive,
    Texture, Topology, Transform as NodeTransform,
};

/// Read a `.gltf` or `.glb` file into a [`Model`].
///
/// The container is detected from the magic bytes rather than the extension, so
/// a mis-named file still loads. Relative buffer and image URIs are resolved
/// against the directory of `path`.
pub fn from_file(path: impl AsRef<Path>) -> Result<Model> {
    let path = path.as_ref();
    let gltf = Gltf::open(path).map_err(|error| Error::corrupt(format!("{path:?}: {error}")))?;
    let base = path.parent().unwrap_or(Path::new("."));
    let Gltf { document, blob } = gltf;
    let buffers = gltf::import_buffers(&document, Some(base), blob)
        .map_err(|error| Error::corrupt(format!("{path:?}: {error}")))?;
    Reader {
        source: Some(path.display().to_string()),
    }
    .build(&document, &buffers, Some(base))
}

/// Read a `.gltf` or `.glb` asset from memory into a [`Model`].
///
/// `base` resolves external buffer and image URIs; when it is `None`, a file
/// that references anything outside itself is refused.
pub fn from_bytes(bytes: &[u8], base: Option<&Path>) -> Result<Model> {
    let gltf = Gltf::from_slice(bytes).map_err(|error| Error::corrupt(format!("glTF: {error}")))?;
    let Gltf { document, blob } = gltf;
    let buffers = gltf::import_buffers(&document, base, blob)
        .map_err(|error| Error::corrupt(format!("glTF: {error}")))?;
    Reader { source: None }.build(&document, &buffers, base)
}

/// One pass over a parsed glTF document.
struct Reader {
    source: Option<String>,
}

impl Reader {
    fn build(
        &mut self,
        gltf: &gltf::Document,
        buffers: &[gltf::buffer::Data],
        base: Option<&Path>,
    ) -> Result<Model> {
        let mut model = Model::new();
        model.source = self.source.clone();
        // glTF 2.0 is metric and Y up by contract. The model records that so a
        // writer aiming at another convention can convert instead of guess.
        model.unit = Some("meter".to_owned());
        model.up_axis = Some("+Y".to_owned());
        model.extras = Some(serde_json::json!({ "format": "glTF" }));

        for buffer in gltf.buffers() {
            let data = buffers
                .get(buffer.index())
                .ok_or_else(|| Error::invalid(buffer.index() as u64, "glTF buffer has no data"))?;
            model.accessors.push_buffer(data.to_vec());
        }

        // Textures, materials and meshes are pushed in document order, so a
        // glTF index and a model index stay the same number.
        for texture in gltf.textures() {
            let (bytes, mime_type) = image_bytes(&model, &texture, base)?;
            let buffer = model.accessors.push_buffer(bytes);
            model.textures.push(Texture {
                name: texture.source().name().map(str::to_owned),
                buffer,
                mime_type,
                extras: extras(texture.extras()),
            });
        }

        for material in gltf.materials() {
            model.materials.push(to_material(&material)?);
        }

        for mesh in gltf.meshes() {
            let mut out = Mesh {
                name: mesh.name().map(str::to_owned),
                ..Mesh::default()
            };
            for primitive in mesh.primitives() {
                out.primitives.push(to_primitive(&mut model, &primitive)?);
            }
            out.bounds = merge_bounds(out.primitives.iter().filter_map(|p| p.bounds));
            model.push_mesh(out);
        }

        // Node indices first, then the children: a glTF node may be the child
        // of several parents, and the model stores children by index.
        let nodes: Vec<gltf::scene::Node<'_>> = gltf.nodes().collect();
        let mut mapped = vec![usize::MAX; nodes.len()];
        for node in &nodes {
            mapped[node.index()] = model.push_node(to_node(node));
        }
        for node in &nodes {
            let index = mapped[node.index()];
            model.nodes[index].children =
                node.children().map(|child| mapped[child.index()]).collect();
        }

        model.default_scene_mut().roots = match gltf.default_scene() {
            Some(scene) => scene.nodes().map(|node| mapped[node.index()]).collect(),
            // A file without a default scene: every unparented node.
            None => {
                let mut parented = vec![false; nodes.len()];
                for node in &nodes {
                    for child in node.children() {
                        parented[child.index()] = true;
                    }
                }
                nodes
                    .iter()
                    .filter(|node| !parented[node.index()])
                    .map(|node| mapped[node.index()])
                    .collect()
            }
        };

        let mut bounds: Option<Aabb> = None;
        for mesh in model
            .nodes
            .iter()
            .flat_map(|node| node.instances.iter().map(|instance| instance.mesh))
        {
            if let Some(box_) = model.meshes.get(mesh).and_then(|mesh| mesh.bounds) {
                bounds = Some(bounds.map_or(box_, |acc| acc.union(box_)));
            }
        }
        model.bounds = bounds;
        Ok(model)
    }
}

/// The encoded bytes and MIME type of a texture's image.
fn image_bytes(
    model: &Model,
    texture: &gltf::Texture<'_>,
    base: Option<&Path>,
) -> Result<(Vec<u8>, String)> {
    let image = texture.source();
    Ok(match image.source() {
        ImageSource::View { view, mime_type } => {
            let start = view.offset();
            let end = start
                .checked_add(view.length())
                .ok_or_else(|| Error::corrupt("an image view overflows its buffer"))?;
            let bytes = model
                .accessors
                .buffers
                .get(view.buffer().index())
                .and_then(|block| block.bytes.get(start..end))
                .ok_or_else(|| {
                    Error::corrupt(format!(
                        "the image {} reaches past its buffer",
                        image.index()
                    ))
                })?;
            (bytes.to_vec(), mime_type.to_owned())
        }
        ImageSource::Uri { uri, mime_type } => (
            read_uri(uri, base)?,
            mime_type.unwrap_or("image/png").to_owned(),
        ),
    })
}

/// Read the bytes of an image referenced by URI — a data URI or a file.
fn read_uri(uri: &str, base: Option<&Path>) -> Result<Vec<u8>> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (head, encoded) = rest
            .split_once(',')
            .ok_or_else(|| Error::corrupt(format!("the data URI {uri:?} has no payload")))?;
        if !head.contains("base64") {
            return Err(Error::Unsupported(format!(
                "the data URI {uri:?} is not base64 encoded"
            )));
        }
        return STANDARD
            .decode(encoded)
            .map_err(|error| Error::corrupt(format!("the data URI {uri:?}: {error}")));
    }
    let base = base.ok_or_else(|| {
        Error::Unsupported(format!(
            "the image {uri:?} is an external file and no base directory was given"
        ))
    })?;
    std::fs::read(base.join(uri)).map_err(Error::Io)
}

fn to_node(node: &gltf::scene::Node<'_>) -> Node {
    let transform = match node.transform() {
        Transform::Matrix { matrix } => decompose(&matrix),
        Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => NodeTransform {
            translation,
            rotation,
            scale,
        },
    };
    Node {
        name: node.name().map(str::to_owned),
        // The JT writer picks its node class from this. A glTF node carries no
        // class of its own, and a plain transform node is a JT Group.
        kind: Some("Group".to_owned()),
        transform,
        instances: node
            .mesh()
            .map(|mesh| Instance {
                mesh: mesh.index(),
                material: mesh
                    .primitives()
                    .next()
                    .and_then(|primitive| primitive.material().index()),
                lod_bias: 1.0,
            })
            .into_iter()
            .collect(),
        children: Vec::new(),
        bounds: None,
        extras: extras(node.extras()),
    }
}

/// A glTF node matrix is column major; the model keeps translation, rotation
/// and scale, which is what both JT and USD store.
fn decompose(matrix: &[[f32; 4]; 4]) -> NodeTransform {
    let column = |axis: usize| [matrix[0][axis], matrix[1][axis], matrix[2][axis]];
    let translation = [matrix[3][0], matrix[3][1], matrix[3][2]];
    let scale = [norm(&column(0)), norm(&column(1)), norm(&column(2))];
    if scale.contains(&0.0) {
        return NodeTransform {
            translation,
            ..NodeTransform::IDENTITY
        };
    }
    // Column `c` divided by its length is the image of the unit axis `c`, which
    // is column `c` of the rotation; so row `r`, column `c` is `column(c)[r]`.
    let rotation: [[f32; 3]; 3] =
        std::array::from_fn(|row| std::array::from_fn(|col| column(col)[row] / scale[col]));
    NodeTransform {
        translation,
        rotation: to_quaternion(&rotation),
        scale,
    }
}

fn norm(v: &[f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// A 3x3 rotation matrix as `x, y, z, w`, matching
/// [`crate::model::Transform::rotation`].
///
/// The trace branch is `s = 0.5 / sqrt(trace + 1)` so the result is a **unit**
/// quaternion; the `0.5 / s` variant yields `w = 2` for the identity.
fn to_quaternion(m: &[[f32; 3]; 3]) -> [f32; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let (w, x, y, z) = if trace > 0.0 {
        let s = 0.5 / (trace + 1.0).sqrt();
        (
            0.25 / s,
            (m[2][1] - m[1][2]) * s,
            (m[0][2] - m[2][0]) * s,
            (m[1][0] - m[0][1]) * s,
        )
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = 2.0 * (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt();
        (
            (m[2][1] - m[1][2]) / s,
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
        )
    } else if m[1][1] > m[2][2] {
        let s = 2.0 * (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt();
        (
            (m[0][2] - m[2][0]) / s,
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
        )
    } else {
        let s = 2.0 * (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt();
        (
            (m[1][0] - m[0][1]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
        )
    };
    [x, y, z, w]
}

fn to_primitive(model: &mut Model, primitive: &GltfPrimitive<'_>) -> Result<Primitive> {
    if primitive.morph_targets().next().is_some() {
        return Err(Error::Unsupported(
            "a glTF primitive with morph targets cannot be read".to_owned(),
        ));
    }
    let mut out = Primitive {
        topology: to_topology(primitive.mode())?,
        material: primitive.material().index(),
        ..Primitive::default()
    };
    if let Some(indices) = primitive.indices() {
        out.indices = Some(push_accessor(model, &indices, "indices")?);
    }
    for (semantic, accessor) in primitive.attributes() {
        let name = attribute_name(&semantic);
        let handle = push_accessor(model, &accessor, &name)?;
        out.attributes.push(handle);
    }
    let position = out
        .attributes
        .iter()
        .copied()
        .find(|handle| {
            model
                .accessors
                .attribute(*handle)
                .is_some_and(|attribute| attribute.name == "POSITION")
        })
        .ok_or_else(|| {
            Error::invalid(
                primitive.index() as u64,
                "a glTF primitive without a POSITION attribute cannot be read",
            )
        })?;
    out.bounds = model
        .accessors
        .attribute(position)
        .and_then(|attribute| aabb(attribute.min.as_ref(), attribute.max.as_ref()));
    Ok(out)
}

/// The inverse of the exporter's semantic mapping.
fn attribute_name(semantic: &Semantic) -> String {
    match semantic {
        Semantic::Positions => "POSITION".to_owned(),
        Semantic::Normals => "NORMAL".to_owned(),
        Semantic::Tangents => "TANGENT".to_owned(),
        Semantic::Colors(set) => format!("COLOR_{set}"),
        Semantic::TexCoords(set) => format!("TEXCOORD_{set}"),
        Semantic::Joints(set) => format!("JOINTS_{set}"),
        Semantic::Weights(set) => format!("WEIGHTS_{set}"),
        Semantic::Extras(other) => other.clone(),
    }
}

fn to_topology(mode: Mode) -> Result<Topology> {
    Ok(match mode {
        Mode::Points => Topology::PointList,
        Mode::Lines => Topology::LineList,
        Mode::LineStrip => Topology::LineStrip,
        Mode::Triangles => Topology::TriangleList,
        Mode::TriangleStrip => Topology::TriangleStrip,
        Mode::TriangleFan => Topology::TriangleFan,
        Mode::LineLoop => {
            return Err(Error::Unsupported(
                "the glTF primitive mode LINE_LOOP has no FerroJT equivalent".to_owned(),
            ));
        }
    })
}

/// Copy one accessor into the model, keeping its bytes rather than re-packing
/// the values. The buffer view stride is honoured, so an interleaved vertex
/// buffer works without being de-interleaved.
fn push_accessor(model: &mut Model, accessor: &gltf::Accessor<'_>, name: &str) -> Result<usize> {
    if accessor.sparse().is_some() {
        return Err(Error::Unsupported(
            "a sparse glTF accessor cannot be read".to_owned(),
        ));
    }
    if !supported_dimensions(accessor.dimensions()) {
        return Err(Error::Unsupported(format!(
            "the glTF accessor {name:?} is a matrix attribute, which the scene model cannot hold"
        )));
    }
    let view = accessor.view().ok_or_else(|| {
        Error::invalid(
            accessor.index() as u64,
            "a glTF accessor without a buffer view cannot be read",
        )
    })?;
    let size = accessor.size();
    let step = view.stride().unwrap_or(size).max(size);
    let start = view.offset() + accessor.offset();
    let end = start
        .checked_add(accessor.count().saturating_sub(1) * step + size)
        .ok_or_else(|| Error::corrupt("a glTF accessor overflows its range"))?;
    let bytes = model
        .accessors
        .buffers
        .get(view.buffer().index())
        .and_then(|block| block.bytes.get(start..end))
        .ok_or_else(|| {
            Error::corrupt(format!(
                "the glTF accessor {name:?} reaches past its buffer"
            ))
        })?
        .to_vec();

    let buffer = model.accessors.push_buffer(bytes);
    let components = u8::try_from(accessor.dimensions().multiplicity())
        .map_err(|_| Error::corrupt("a glTF attribute has too many components"))?;
    Ok(model.accessors.push_attribute(Attribute {
        name: name.to_owned(),
        buffer,
        offset: 0,
        count: accessor.count(),
        components,
        component_type: to_component_type(accessor.data_type())?,
        normalized: accessor.normalized(),
        min: values(accessor.min()),
        max: values(accessor.max()),
    }))
}

fn to_component_type(kind: GltfComponentType) -> Result<ComponentType> {
    Ok(match kind {
        GltfComponentType::I8 => ComponentType::I8,
        GltfComponentType::U8 => ComponentType::U8,
        GltfComponentType::I16 => ComponentType::I16,
        GltfComponentType::U16 => ComponentType::U16,
        GltfComponentType::U32 => ComponentType::U32,
        GltfComponentType::F32 => ComponentType::F32,
    })
}

/// glTF only allows scalars and vectors in a vertex attribute; a matrix would
/// collide with [`crate::model::Attribute::components`].
fn supported_dimensions(kind: GltfDimensions) -> bool {
    matches!(
        kind,
        GltfDimensions::Scalar | GltfDimensions::Vec2 | GltfDimensions::Vec3 | GltfDimensions::Vec4
    )
}

fn to_material(material: &gltf::Material<'_>) -> Result<Material> {
    let pbr = material.pbr_metallic_roughness();
    let base_color = pbr.base_color_factor();
    let metallic = pbr.metallic_factor();
    let roughness = pbr.roughness_factor();
    let emissive = material.emissive_factor();

    let mut out = Material {
        name: material.name().map(str::to_owned),
        base_color: channel(base_color, pbr.base_color_texture()),
        metallic: Channel::constant([metallic, metallic, metallic, 1.0]),
        // glTF packs metallic and roughness into one texture; the exporter
        // sources it from `roughness`, so the read puts it back there.
        roughness: channel(
            [roughness, roughness, roughness, 1.0],
            pbr.metallic_roughness_texture(),
        ),
        emissive: Channel::constant([emissive[0], emissive[1], emissive[2], 1.0]),
        double_sided: material.double_sided(),
        extras: extras(material.extras()),
        ..Material::default()
    };
    if let Some(texture) = material.normal_texture() {
        out.normal = Some(texture.texture().index());
    }
    if let Some(texture) = material.occlusion_texture() {
        out.occlusion = Some(texture.texture().index());
    }
    if let Some(texture) = material.emissive_texture() {
        out.emissive_texture = Some(texture.texture().index());
    }
    Ok(out)
}

fn channel(factor: [f32; 4], texture: Option<gltf::texture::Info<'_>>) -> Channel {
    match texture {
        Some(info) => Channel {
            factor,
            texture: Some(info.texture().index()),
            tex_coord: info.tex_coord() as u8,
        },
        None => Channel::constant(factor),
    }
}

/// `extras` as a plain JSON value, `None` when absent.
fn extras(extras: &gltf_json::Extras) -> Option<serde_json::Value> {
    let raw = extras.as_deref()?;
    serde_json::from_str(raw.get()).ok()
}

fn values(value: Option<serde_json::Value>) -> Option<Vec<f64>> {
    Some(
        value?
            .as_array()?
            .iter()
            .filter_map(|value| value.as_f64())
            .collect(),
    )
}

fn aabb(min: Option<&Vec<f64>>, max: Option<&Vec<f64>>) -> Option<Aabb> {
    let min = min?.as_slice();
    let max = max?.as_slice();
    if min.len() < 3 || max.len() < 3 {
        return None;
    }
    let to_f32 = |values: &[f64]| [values[0] as f32, values[1] as f32, values[2] as f32];
    Some(Aabb::new(to_f32(min), to_f32(max)))
}

fn merge_bounds(mut bounds: impl Iterator<Item = Aabb>) -> Option<Aabb> {
    let mut acc = bounds.next()?;
    for box_ in bounds {
        acc = acc.union(box_);
    }
    Some(acc)
}
