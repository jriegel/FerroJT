//! Writing a neutral [`Model`] out as a JT file.
//!
//! This is the third way to produce a file, next to the two in [`crate::jt`]:
//! [`JtBuilder`] builds a file from nothing, node by node, and
//! [`crate::jt::write_to_bytes`] re-serialises a file that was read from disk.
//! This module sits between them: it takes the neutral scene graph — which is
//! what [`crate::import`] produces for a glTF file and what
//! [`JtFile::to_model`] produces for a JT one — and hands it to [`JtBuilder`].
//!
//! # Revision
//!
//! The default is [`JtVersion::V10_3`], the newest revision the builder can
//! emit. JT 9.x and 10.x store a shape in the *topologically compressed*
//! representation, and only a closed two-manifold can be coded that way, so
//! every mesh is probed with the coder first. When one of them cannot be coded
//! and no revision was asked for, the whole file drops to [`JtVersion::V8_1`],
//! whose vertex based representation holds any triangle soup at the cost of an
//! exploded vertex table. Passing `version` explicitly suppresses the fallback
//! and reports the meshes that cannot be coded instead.
//!
//! # Units and orientation
//!
//! A JT file is in millimetres with +Z up; the samples in `data/ExampleFiles`
//! all are. A [`Model`] declares its frame, and a glTF file declares metres
//! with +Y up, so the two are reconciled on the root partition rather than by
//! touching vertices: the scale converts the declared unit to millimetres and
//! the rotation brings the declared up axis onto +Z. [`JtFile::to_model`]
//! declares millimetres and +Z for the same reason, so a model read out of a
//! JT file comes back with an identity root transform; a model that declares
//! nothing is assumed to be in that frame too.
//!
//! # What does not survive
//!
//! The builder has no level-of-detail node, so a node holding several
//! representations keeps only the first one — the finest tier, which is the
//! order the reader collected them in. Lines and points have no JT shape type
//! here, so a primitive that is not a plain triangle list is refused rather
//! than re-indexed.
//!
//! [`JtFile::to_model`]: crate::jt::JtFile::to_model

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};
use crate::export::read_component;
use crate::model::{Attribute, Material, Mesh, Model, Node, Primitive, Topology};

use super::build::{BuildNode, JtBuilder, JtMesh, JtNode, MaterialData, MeshData};
use super::encode::encode;
use super::version::JtVersion;

/// A rotation as a row-major 3x3 matrix: row `r` says where output axis `r`
/// comes from.
type Orientation = [[f32; 3]; 3];

/// The orientation of a model that is already +Z up.
const IDENTITY: Orientation = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Millimetres per unit a [`Model`] is allowed to declare.
const METERS_PER_UNIT: &[(&str, f64)] = &[
    ("meter", 1.0),
    ("meters", 1.0),
    ("centimeter", 0.01),
    ("millimeter", 0.001),
    ("inch", 0.0254),
    ("foot", 0.3048),
    ("mile", 1609.344),
    ("yard", 0.9144),
    ("angstrom", 1e-10),
    ("nanometer", 1e-9),
    ("micrometer", 1e-6),
    ("decimeter", 0.1),
    ("kilometer", 1000.0),
];

/// Write a model out as a JT file.
///
/// `version` selects the revision. `None` means the newest one the meshes of
/// this model can be written at: [`JtVersion::V10_3`] unless a mesh is not a
/// closed two-manifold, in which case the file goes to [`JtVersion::V8_1`].
/// See [the JT module](crate::jt) for how the unit and up axis of the model
/// are converted to the millimetre +Z frame of the file.
///
/// # Errors
///
/// [`Error::Unsupported`] if the model declares a unit or an up axis JT cannot
/// express, a primitive is not a triangle list, or a mesh cannot be coded at an
/// explicitly requested revision. [`Error::Invalid`] if the model has no scene
/// roots or the graph is malformed.
pub fn model_to_bytes(model: &Model, version: Option<JtVersion>) -> Result<Vec<u8>> {
    let mut build = JtBuilder::new();
    if let Some(version) = version {
        build.set_version(version)?;
    }

    let scale = unit_scale(model)?;
    let orientation = orientation(model)?;
    if scale != 1.0 || orientation != IDENTITY {
        build.set_transform(build.root(), &root_transform(scale, &orientation))?;
    }

    let mut writer = Writer {
        model,
        build,
        shapes: Vec::new(),
        materials: Vec::new(),
        built: vec![None; model.nodes.len()],
    };
    writer.write_scene()?;

    if let Some(mesh) = writer.shapes.iter().find(|mesh| encode(mesh).is_err()) {
        if version.is_some() {
            return Err(Error::Unsupported(format!(
                "a mesh with {} triangles is not a closed two-manifold, so it cannot be \
                 written as JT {}; write it at JT 8.x or repair the mesh",
                mesh.triangle_count(),
                writer.build.version(),
            )));
        }
        // One revision per file, so a single uncodable mesh moves every shape
        // of the model to the vertex based representation.
        writer.build.set_version(JtVersion::V8_1)?;
    }

    writer.build.write_to_bytes()
}

/// Write a model out as a JT file at `path`.
///
/// See [`model_to_bytes`] for the revision and unit handling.
///
/// # Errors
///
/// As [`model_to_bytes`], plus [`Error::Io`] when the file cannot be written.
pub fn model_to_file(model: &Model, path: &Path, version: Option<JtVersion>) -> Result<()> {
    fs::write(path, model_to_bytes(model, version)?)?;
    Ok(())
}

/// The factor that turns the unit of the model into millimetres.
///
/// A model that declares no unit is taken to be in millimetres already, which
/// is the case for every model read out of a JT file.
fn unit_scale(model: &Model) -> Result<f32> {
    let Some(unit) = model.unit.as_deref() else {
        return Ok(1.0);
    };
    let key = normalise(unit);
    let meters = METERS_PER_UNIT
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, meters)| *meters)
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "the unit {unit:?} has no millimetre equivalent, so the model cannot be \
                 written as JT"
            ))
        })?;
    Ok((meters / 0.001) as f32)
}

/// The rotation that brings the up axis of the model onto +Z.
///
/// A model that declares no up axis is taken to be +Z up already.
fn orientation(model: &Model) -> Result<Orientation> {
    let Some(axis) = model.up_axis.as_deref() else {
        return Ok(IDENTITY);
    };
    let key = normalise(axis).to_ascii_uppercase();
    Ok(match key.as_str() {
        "Z" => IDENTITY,
        // +Y up: (x, y, z) -> (x, -z, y), a quarter turn about +X.
        "Y" => [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]],
        // -Y up: (x, y, z) -> (x, z, -y).
        "-Y" => [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
        // +X up: (x, y, z) -> (z, y, -x), a quarter turn about -Y.
        "X" => [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
        // -X up: (x, y, z) -> (-z, y, x).
        "-X" => [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        other => {
            return Err(Error::Unsupported(format!(
                "{other:?} is not an up axis, so the model cannot be written as JT"
            )));
        }
    })
}

/// A unit or axis name as the table holds it: lower case and without a `+`.
fn normalise(value: &str) -> String {
    let trimmed = value.trim().to_ascii_lowercase();
    trimmed.strip_prefix('+').unwrap_or(&trimmed).to_owned()
}

/// The root partition transform: the orientation, scaled by the unit factor.
///
/// Column major, as [`JtBuilder::set_transform`] wants it. The scale is
/// uniform, so it commutes with the rotation.
fn root_transform(scale: f32, orientation: &Orientation) -> [f64; 16] {
    let mut matrix = [0.0; 16];
    for (row, line) in orientation.iter().enumerate() {
        for (column, value) in line.iter().enumerate() {
            matrix[4 * column + row] = f64::from(*value) * f64::from(scale);
        }
    }
    matrix[15] = 1.0;
    matrix
}

/// The JT class a neutral node maps to.
///
/// A model read out of a JT file carries the JT class name in [`Node::kind`],
/// so it maps straight back. Everything else — the `"Group"` a glTF node gets,
/// and the `"LOD"`, `"RangeLOD"` and unknown classes the builder has no node
/// type for — becomes a group, the one JT container carrying no data of its
/// own.
fn class_of(kind: Option<&str>) -> BuildNode {
    match kind {
        Some("Partition") => BuildNode::Partition,
        Some("Part") => BuildNode::Part,
        Some("MetaData") => BuildNode::MetaData,
        Some("Shape") => BuildNode::Shape,
        Some("Instance") => BuildNode::Instance,
        _ => BuildNode::Group,
    }
}

/// The JT attributes of a neutral material.
///
/// This is the inverse of what the JT reader does: `diffuse` is the base colour
/// and `emission` the emissive factor, and the ambient, specular, shininess and
/// reflectivity that did not fit into a glTF-style material went to
/// [`Material::extras`] and come back from there. What is left over keeps the
/// JT default; `metallic` and `roughness` have no JT equivalent at all.
fn material_of(material: &Material) -> MaterialData {
    let mut data = MaterialData {
        diffuse: material.base_color.factor,
        emission: material.emissive.factor,
        ..MaterialData::default()
    };
    let Some(extras) = material.extras.as_ref() else {
        return data;
    };
    if let Some(ambient) = color(extras.get("ambient")) {
        data.ambient = ambient;
    }
    if let Some(specular) = color(extras.get("specular")) {
        data.specular = specular;
    }
    if let Some(shininess) = extras.get("shininess").and_then(serde_json::Value::as_f64) {
        data.shininess = shininess as f32;
    }
    if let Some(reflectivity) = extras
        .get("reflectivity")
        .and_then(serde_json::Value::as_f64)
    {
        data.reflectivity = reflectivity as f32;
    }
    data
}

/// A four-component colour out of a JSON value.
fn color(value: Option<&serde_json::Value>) -> Option<[f32; 4]> {
    let items = value?.as_array()?;
    if items.len() != 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = item.as_f64()? as f32;
    }
    Some(out)
}

/// The JT shape data of one neutral mesh.
///
/// The primitives of a mesh are merged into one JT shape, because a JT shape
/// node holds exactly one representation, and the indices of each primitive are
/// offset by the running vertex count as they are copied. A primitive without
/// indices contributes its vertices in order.
///
/// Both [`MeshData`] constructors explode the mesh — one vertex slot per
/// triangle corner — which is what the JT 8.x representation wants and what the
/// topological coder welds back together again.
fn mesh_data(model: &Model, mesh: &Mesh) -> Result<MeshData> {
    let mut vertices: Vec<f32> = Vec::new();
    let mut normals: Vec<f32> = Vec::new();
    let mut indices: Vec<i32> = Vec::new();
    let mut with_normals = false;
    for primitive in &mesh.primitives {
        if primitive.topology != Topology::TriangleList {
            return Err(Error::Unsupported(format!(
                "the topology {:?} of the mesh {:?} has no JT shape type here",
                primitive.topology, mesh.name
            )));
        }
        let position = find(model, primitive, "POSITION")?.ok_or_else(|| {
            Error::invalid(
                0,
                format!("the mesh {:?} has no POSITION attribute", mesh.name),
            )
        })?;
        let points = read_vector(model, position, 3)?;
        let base = i32::try_from(vertices.len() / 3)
            .map_err(|_| Error::invalid(0, format!("the mesh {:?} is too large", mesh.name)))?;
        let count = points.len() / 3;
        vertices.extend_from_slice(&points);

        match find(model, primitive, "NORMAL")? {
            Some(handle) => {
                let vectors = read_vector(model, handle, 3)?;
                if vectors.len() != points.len() {
                    return Err(Error::invalid(
                        0,
                        "a NORMAL attribute must hold one vector per vertex",
                    ));
                }
                normals.extend_from_slice(&vectors);
                with_normals = true;
            }
            None if !normals.is_empty() => {
                return Err(Error::invalid(
                    0,
                    "only some primitives of a mesh carry normals",
                ));
            }
            None => {}
        }

        match primitive.indices {
            Some(handle) => {
                let attribute = attribute(model, handle)?;
                if attribute.components != 1 {
                    return Err(Error::invalid(0, "an index attribute must be a list"));
                }
                let width = attribute.component_type.byte_len();
                for chunk in data(model, handle)?.chunks_exact(width) {
                    let value = read_component(attribute.component_type, chunk)? as i32;
                    if value < 0 || value >= count as i32 {
                        return Err(Error::invalid(
                            0,
                            format!("the index {value} is outside its primitive"),
                        ));
                    }
                    indices.push(value + base);
                }
            }
            None => indices.extend(base..base + count as i32),
        }
    }
    if with_normals {
        MeshData::with_normals(vertices, normals, indices)
    } else {
        MeshData::new(vertices, indices)
    }
}

/// The attribute of a primitive by name, ignoring case as the exporters do.
fn find(model: &Model, primitive: &Primitive, name: &str) -> Result<Option<usize>> {
    for handle in &primitive.attributes {
        if attribute(model, *handle)?.name.eq_ignore_ascii_case(name) {
            return Ok(Some(*handle));
        }
    }
    Ok(None)
}

/// The neutral attribute behind a handle.
fn attribute(model: &Model, handle: usize) -> Result<&Attribute> {
    model
        .accessors
        .attribute(handle)
        .ok_or_else(|| Error::invalid(0, "a mesh attribute is out of range"))
}

/// The bytes behind an attribute.
fn data(model: &Model, handle: usize) -> Result<&[u8]> {
    model
        .accessors
        .attribute_bytes(handle)
        .ok_or_else(|| Error::invalid(0, "the data behind a mesh attribute is missing"))
}

/// The floats of an attribute, `width` per element.
fn read_vector(model: &Model, handle: usize, width: usize) -> Result<Vec<f32>> {
    let attribute = attribute(model, handle)?;
    if usize::from(attribute.components) != width {
        return Err(Error::invalid(
            0,
            format!(
                "the attribute {:?} has {} components per element, {width} were expected",
                attribute.name, attribute.components
            ),
        ));
    }
    let width = attribute.component_type.byte_len();
    let mut out = Vec::new();
    for element in data(model, handle)?.chunks_exact(attribute.element_byte_len()) {
        for component in element.chunks_exact(width) {
            out.push(read_component(attribute.component_type, component)? as f32);
        }
    }
    Ok(out)
}

/// The nodes of the model, walked top-down onto the builder.
struct Writer<'a> {
    model: &'a Model,
    build: JtBuilder,
    /// Every shape handed to the builder, kept so the encodability of the file
    /// can be probed afterwards: a `JtMesh` handle cannot be turned back into
    /// the data behind it.
    shapes: Vec<MeshData>,
    /// The JT attributes of each neutral material, by index.
    materials: Vec<Option<MaterialData>>,
    /// The builder slot each model node landed in, once it has been written.
    /// Recording it *before* the children are walked is what turns a subtree
    /// reached from several parents into one node plus an instance, and what
    /// stops a cycle from recursing forever.
    built: Vec<Option<JtNode>>,
}

impl Writer<'_> {
    /// Attach every scene root to the single root partition, as the JT samples
    /// do: one Partition at the top, everything below it.
    fn write_scene(&mut self) -> Result<()> {
        let roots = &self
            .model
            .scenes
            .first()
            .filter(|scene| !scene.roots.is_empty())
            .ok_or_else(|| Error::invalid(0, "the model has no scene roots to write"))?
            .roots;
        for root in roots {
            self.node(*root, self.build.root())?;
        }
        Ok(())
    }

    /// Write `index` under `parent` and return the node it landed in.
    fn node(&mut self, index: usize, parent: JtNode) -> Result<JtNode> {
        let node = self
            .model
            .nodes
            .get(index)
            .ok_or_else(|| Error::invalid(0, "a node of the model is out of range"))?;
        if let Some(slot) = self.built.get(index).copied().flatten() {
            return self.share(parent, slot);
        }

        // A JT instance node carries no data of its own, and the neutral graph
        // keeps its target as the last child. Recording the *target* as the slot
        // of this node makes a second visit point at the same subtree. The
        // target is looked up before it is built: when it has been written
        // already it must be instantiated where it is, not copied under the
        // root and instantiated a second time.
        if class_of(node.kind.as_deref()) == BuildNode::Instance {
            let target = *node
                .children
                .last()
                .ok_or_else(|| Error::invalid(0, "an instance node has no target"))?;
            let target = match self.built.get(target).copied().flatten() {
                Some(slot) => slot,
                None => self.node(target, self.build.root())?,
            };
            let instance = self.share(parent, target)?;
            self.built[index] = Some(target);
            return Ok(instance);
        }

        // JT keeps the geometry on the shape node itself, and a shape has no
        // children. A node that declares no JT class of its own — every glTF
        // node, and any neutral group — becomes the shape when nothing hangs
        // below it. A node that declares a container class keeps that class and
        // gets a shape child to hold its geometry. JT can also not express a
        // shape without geometry (a coarse LOD tier, whose representation the
        // reader left out) nor a shape carrying children, and such a node
        // becomes a group: keeping the node beats dropping its subtree.
        let declared = class_of(node.kind.as_deref());
        let geometry = !node.instances.is_empty();
        let shapeless = declared == BuildNode::Shape && (!geometry || !node.children.is_empty());
        let class = if shapeless {
            BuildNode::Group
        } else {
            declared
        };
        let own_shape = geometry
            && node.children.is_empty()
            && matches!(class, BuildNode::Shape | BuildNode::Group);
        let class = if own_shape { BuildNode::Shape } else { class };
        let slot = self.build.add_node(parent, class, node.name.as_deref())?;
        self.built[index] = Some(slot);
        if !node.transform.is_identity() {
            self.build.set_node_transform(slot, &node.transform)?;
        }

        // JT keeps the material on the node holding the geometry, so a shape
        // that has to move to a child takes it along.
        if geometry {
            let mesh = self.shape_of(node)?;
            let drawable = if own_shape {
                self.build.set_mesh(slot, mesh)?;
                slot
            } else {
                let shape = self.build.add_node(slot, BuildNode::Shape, None)?;
                self.build.set_mesh(shape, mesh)?;
                shape
            };
            if let Some(material) = self.material_of_node(node) {
                self.build.set_material(drawable, material)?;
            }
        }

        for child in &node.children {
            self.node(*child, slot)?;
        }
        Ok(slot)
    }

    /// Point `parent` at an already written node instead of copying it.
    fn share(&mut self, parent: JtNode, target: JtNode) -> Result<JtNode> {
        if target == self.build.root() {
            return Err(Error::invalid(0, "a scene root cannot be instantiated"));
        }
        self.build.instance_of(parent, target)
    }

    /// The mesh of a node. A node holding several representations keeps the
    /// first one, which is the finest tier of a JT file.
    fn shape_of(&mut self, node: &Node) -> Result<JtMesh> {
        let mesh = self
            .model
            .meshes
            .get(node.instances[0].mesh)
            .ok_or_else(|| Error::invalid(0, "a mesh of the model is out of range"))?;
        let data = mesh_data(self.model, mesh)?;
        data.validate()?;
        self.shapes.push(data.clone());
        Ok(self.build.add_mesh(data))
    }

    /// The material of a node.
    ///
    /// The JT reader puts the material of the attribute onto the instance it
    /// creates for the node, so that is the first place to look; a glTF node
    /// carries it on the primitive of its mesh instead.
    fn material_of_node(&mut self, node: &Node) -> Option<MaterialData> {
        let instance = node.instances.first()?;
        let index = instance.material.or_else(|| {
            self.model
                .meshes
                .get(instance.mesh)?
                .primitives
                .first()?
                .material
        })?;
        Some(self.material(index))
    }

    /// The JT attributes of a neutral material, memoised by index.
    fn material(&mut self, index: usize) -> MaterialData {
        if let Some(data) = self.materials.get(index).copied().flatten() {
            return data;
        }
        let data = self
            .model
            .materials
            .get(index)
            .map_or(MaterialData::default(), material_of);
        if self.materials.len() <= index {
            self.materials.resize(index + 1, None);
        }
        self.materials[index] = Some(data);
        data
    }
}
