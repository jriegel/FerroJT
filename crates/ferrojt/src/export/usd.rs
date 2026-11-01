//! USD (Universal Scene Description) export: a hand written `.usda` layer.
//!
//! USD is the second target format after glTF. The writer emits ASCII USD
//! (`.usda`) directly, with no dependency on the Pixar SDK, which keeps the
//! output deterministic, diffable and testable with plain string asserts.
//!
//! ## Mapping
//!
//! | model | USD |
//! |---|---|
//! | [`Model`](crate::model::Model) | one layer: `defaultPrim`, `metersPerUnit` from `Model::unit`, `upAxis` from `Model::up_axis` |
//! | [`Scene`](crate::model::Scene) | the default prim plus one `Xform` per scene root |
//! | [`Node`](crate::model::Node) | `Xform` with `translate` / `orient` / `scale` ops |
//! | [`Mesh`](crate::model::Mesh) | one prim under the geometry library, referenced by every consumer |
//! | [`Primitive`](crate::model::Primitive) | `points`, `normals`, `faceVertexCounts`, `faceVertexIndices`, `primvars:st`, `extent` |
//! | [`Material`](crate::model::Material) | `Material` + a `UsdPreviewSurface` `Shader`, bound with `MaterialBindingAPI` |
//! | [`Aabb`](crate::model::Aabb) | `extent` |
//!
//! ## Instancing
//!
//! A mesh is written **exactly once**, in a geometry library that sits beside
//! the default prim and is marked `visibility = "invisible"`. Every node that
//! draws it gets `prepend references = <its library prim>` plus its own
//! transform ops, and the mesh prim itself carries `instanceable = true` so a
//! consumer treats the repeats as instances rather than copies. The reference
//! targets the per-mesh group, not the mesh prim, because that group is where
//! the material binding sits and a reference only carries what the referenced
//! prim itself states.
//!
//! The same mechanism covers a scene node reached from several parents — a JT
//! `Instance` shares its subtree rather than copying it, and the reader keeps
//! it as one [`Node`](crate::model::Node) with two parents. Such a node is
//! written once under its first parent; every other parent gets a thin child
//! `Xform` that references it and authors nothing of its own, so the node's
//! transform and subtree come through unchanged. That is USD's equivalent of
//! glTF listing one node in two parent `children` arrays.
//!
//! ## Materials
//!
//! Materials are deduplicated by their *shading values* — base colour,
//! metallic, roughness, emissive and culling — not by name, so a JT file that
//! repeats one appearance across a dozen shapes yields one USD material. The
//! binding sits on the library prim, where a consumer can still override it.
//! Textures are not exported yet; a textured channel falls back to its factor.
//!
//! ## Coordinate conventions
//!
//! JT stores a rotation as `x, y, z, w` and USD writes a quaternion as
//! `real, i, j, k`, so the components are reordered. The op order
//! `translate`, `orient`, `scale` composes as `T · R · S`, which is what a JT
//! `GeometricTransform` means. JT packages are millimetre based and Z up, so
//! those are the defaults when [`Model::unit`] and [`Model::up_axis`] are empty.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::export::Exporter;
use crate::export::gltf::{depth_first, normalize_quaternion, read_component, split_suffix};
use crate::model::{ComponentType, Format, Mesh, Model, Node, Primitive, Topology};

/// Name of the prim holding every mesh, as a sibling of the default prim.
const GEOMETRY_SCOPE: &str = "FerroJT_Geometry";
/// Name of the scope holding the materials, inside the default prim.
const MATERIALS_SCOPE: &str = "Materials";
/// Default prim name when the model gives no hint.
const DEFAULT_PRIM: &str = "Root";
/// JT packages are millimetre based, so that is the fallback scale.
const DEFAULT_METERS_PER_UNIT: f64 = 0.001;
/// Sentinel for "written at the top level, under the default prim".
const NO_PARENT: usize = usize::MAX;
/// Sentinel for "not reachable from any scene root".
const UNREACHED: usize = usize::MAX - 1;

/// Writes a [`Model`](crate::model::Model) as a USD `.usda` layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsdExporter;

impl UsdExporter {
    /// Serialize to a `.usda` string.
    pub fn to_text(&self, model: &Model) -> Result<String> {
        let plan = Plan::new(model)?;
        plan.render(model)
    }

    /// Serialize to `.usda` bytes, UTF-8 like every USD text layer.
    pub fn to_bytes(&self, model: &Model) -> Result<Vec<u8>> {
        Ok(self.to_text(model)?.into_bytes())
    }
}

impl Exporter for UsdExporter {
    const FORMAT: Format = Format::Usd;

    fn write(&self, model: &Model, out: &mut dyn Write) -> Result<()> {
        out.write_all(&self.to_bytes(model)?)?;
        Ok(())
    }
}

/// A USD material, keyed by the values its shader carries.
#[derive(Clone, Debug, PartialEq)]
struct UsdMaterial {
    /// Prim name inside the material scope.
    name: String,
    /// `inputs:diffuseColor`.
    diffuse: [f32; 3],
    /// Alpha of the base colour channel, written as `inputs:opacity`.
    opacity: f32,
    /// `inputs:metalness`.
    metallic: f32,
    /// `inputs:roughness`.
    roughness: f32,
    /// `inputs:emissiveColor`.
    emissive: [f32; 3],
    /// Whether the geometry is rendered from both sides.
    double_sided: bool,
}

impl UsdMaterial {
    /// `true` when two materials would produce the same shader, which is what
    /// makes them worth collapsing into one prim. The name is deliberately not
    /// part of this: two JT shapes may name the same appearance differently.
    fn same_shading(&self, other: &Self) -> bool {
        self.diffuse == other.diffuse
            && self.opacity == other.opacity
            && self.metallic == other.metallic
            && self.roughness == other.roughness
            && self.emissive == other.emissive
            && self.double_sided == other.double_sided
    }
}

/// Where every model node and mesh ends up, worked out before anything is
/// written so that a shared node can be referenced by absolute path.
struct Plan {
    /// Name of the default prim.
    default_prim: String,
    /// Absolute path of the default prim.
    default_path: String,
    /// Absolute path of the geometry library.
    library_path: String,
    /// Absolute path of the material scope.
    materials_path: String,
    /// Model node index to its USD path; empty when the node is unreachable.
    node_path: Vec<String>,
    /// The parent a node is written under, [`NO_PARENT`] for a scene root.
    owner: Vec<usize>,
    /// Path of the library prim a mesh instance references, per mesh.
    mesh_path: Vec<String>,
    /// Path of the wrapper prim a shared child gets under a non-owning parent.
    shared_path: HashMap<(usize, usize), String>,
    /// Model material index to the USD material it collapses into.
    material_slot: Vec<usize>,
    /// The USD materials, in the order they are written.
    materials: Vec<UsdMaterial>,
    /// Meters per model unit.
    meters_per_unit: f64,
    /// USD up axis.
    up_axis: &'static str,
}

impl Plan {
    /// Resolve every path and every material slot.
    fn new(model: &Model) -> Result<Self> {
        let order = depth_first(model);
        let mut owner = vec![UNREACHED; model.nodes.len()];
        for root in model.scenes.iter().flat_map(|scene| &scene.roots) {
            let slot = owner
                .get_mut(*root)
                .ok_or_else(|| Error::invalid(*root as u64, "scene root is out of range"))?;
            if *slot == UNREACHED {
                *slot = NO_PARENT;
            }
        }
        for index in &order {
            let children = model.nodes[*index].children.clone();
            for child in children {
                let slot = owner.get_mut(child).ok_or_else(|| {
                    Error::invalid(child as u64, "scene graph child is out of range")
                })?;
                if *slot == UNREACHED {
                    *slot = *index;
                }
            }
        }

        let default_prim = default_name(model);
        let default_path = format!("/{default_prim}");
        let mut plan = Plan {
            default_path,
            default_prim,
            library_path: format!("/{GEOMETRY_SCOPE}"),
            materials_path: String::new(),
            node_path: vec![String::new(); model.nodes.len()],
            owner,
            mesh_path: Vec::with_capacity(model.meshes.len()),
            shared_path: HashMap::new(),
            material_slot: Vec::new(),
            materials: Vec::new(),
            meters_per_unit: meters_per_unit(model)?,
            up_axis: up_axis(model)?,
        };
        plan.materials_path = format!("{}/{}", plan.default_path, MATERIALS_SCOPE);

        // The material scope is reserved before any node is named, so a part
        // actually called "Materials" still lands on a unique path.
        let mut roots: BTreeSet<String> = BTreeSet::new();
        roots.insert(MATERIALS_SCOPE.to_owned());
        for root in model.scenes.iter().flat_map(|scene| &scene.roots) {
            if plan.owner[*root] != NO_PARENT || !plan.node_path[*root].is_empty() {
                continue;
            }
            plan.assign(model, *root, &plan.default_path.clone(), &mut roots)?;
        }

        for index in 0..model.meshes.len() {
            plan.mesh_path
                .push(format!("{}/{}", plan.library_path, mesh_group(index)));
        }
        plan.collect_materials(model);
        Ok(plan)
    }

    /// Name `index` under `parent` and recurse into the children written here;
    /// a child written elsewhere gets a wrapper name instead.
    fn assign(
        &mut self,
        model: &Model,
        index: usize,
        parent: &str,
        used: &mut BTreeSet<String>,
    ) -> Result<()> {
        let node = model
            .nodes
            .get(index)
            .ok_or_else(|| Error::invalid(index as u64, "scene graph node is out of range"))?;
        let name = unique_name(&node_name(model, index, node), used);
        let path = format!("{parent}/{name}");
        self.node_path[index] = path.clone();
        let children = node.children.clone();
        // One set for the whole sibling group: two children with the same source
        // name (an `Instance` twice, say) must not land on the same USD path.
        let mut sibling: BTreeSet<String> = BTreeSet::new();
        for child in children {
            if self.owner[child] == index {
                self.assign(model, child, &path, &mut sibling)?;
            } else {
                let child_node = &model.nodes[child];
                let wrapper = unique_name(&node_name(model, child, child_node), &mut sibling);
                self.shared_path
                    .insert((index, child), format!("{path}/{wrapper}"));
            }
        }
        Ok(())
    }

    /// Collapse [`Model::materials`](crate::model::Model::materials) onto the
    /// values a `UsdPreviewSurface` actually carries.
    fn collect_materials(&mut self, model: &Model) {
        let mut slots = vec![usize::MAX; model.materials.len()];
        for (index, material) in model.materials.iter().enumerate() {
            let usd = UsdMaterial {
                name: format!("Material_{}", self.materials.len()),
                diffuse: [
                    material.base_color.factor[0],
                    material.base_color.factor[1],
                    material.base_color.factor[2],
                ],
                opacity: material.base_color.factor[3],
                metallic: material.metallic.factor[0],
                roughness: material.roughness.factor[0],
                emissive: [
                    material.emissive.factor[0],
                    material.emissive.factor[1],
                    material.emissive.factor[2],
                ],
                double_sided: material.double_sided,
            };
            let slot = match self
                .materials
                .iter()
                .position(|existing| existing.same_shading(&usd))
            {
                Some(existing) => existing,
                None => {
                    self.materials.push(usd);
                    self.materials.len() - 1
                }
            };
            slots[index] = slot;
        }
        self.material_slot = slots;
    }

    /// The USD material a model material maps to.
    fn material_path(&self, index: usize) -> Option<String> {
        let slot = *self.material_slot.get(index)?;
        (slot != usize::MAX)
            .then(|| format!("{}/{}", self.materials_path, self.materials[slot].name))
    }

    /// The material of a mesh, when every primitive agrees on one.
    fn mesh_material(&self, mesh: &Mesh) -> Option<usize> {
        let first = mesh.primitives.first()?.material?;
        mesh.primitives
            .iter()
            .all(|primitive| primitive.material == Some(first))
            .then_some(first)
    }

    /// The whole layer.
    fn render(&self, model: &Model) -> Result<String> {
        let mut out = String::new();
        self.write_header(model, &mut out);
        out.push_str(&format!("def Xform \"{}\"\n{{\n", self.default_prim));
        self.write_materials(&mut out);
        let mut written: BTreeSet<usize> = BTreeSet::new();
        for root in model.scenes.iter().flat_map(|scene| &scene.roots) {
            if self.owner[*root] != NO_PARENT || !written.insert(*root) {
                continue;
            }
            self.write_node(model, *root, 1, &mut out)?;
        }
        out.push_str("}\n\n");
        self.write_library(model, &mut out)?;
        Ok(out)
    }

    fn write_header(&self, model: &Model, out: &mut String) {
        out.push_str("#usda 1.0\n(\n    customLayerData = {\n");
        out.push_str(&format!(
            "        string \"ferrojt:generator\" = {}\n",
            quote(&format!("FerroJT {}", env!("CARGO_PKG_VERSION")))
        ));
        if let Some(source) = model.source.as_deref() {
            out.push_str(&format!(
                "        string \"ferrojt:source\" = {}\n",
                quote(source)
            ));
        }
        // The reader records the layer filter that produced this model; a
        // consumer should be able to see which parts were gated out.
        let filter = model
            .extras
            .as_ref()
            .and_then(|extras| extras.get("layer_filter"));
        if let Some(name) = filter
            .and_then(|filter| filter.get("name"))
            .and_then(Value::as_str)
        {
            out.push_str(&format!(
                "        string \"ferrojt:layer_filter\" = {}\n",
                quote(name)
            ));
        }
        if let Some(layers) = filter
            .and_then(|filter| filter.get("layers"))
            .and_then(Value::as_array)
        {
            out.push_str(&format!(
                "        int[] \"ferrojt:layers\" = [{}]\n",
                layers
                    .iter()
                    .filter_map(Value::as_i64)
                    .map(|layer| layer.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push_str("    }\n");
        out.push_str(&format!(
            "    defaultPrim = {}\n    doc = {}\n    metersPerUnit = {}\n    upAxis = \"{}\"\n)\n\n",
            quote(&self.default_prim),
            quote(&format!(
                "generated by FerroJT {}{}",
                env!("CARGO_PKG_VERSION"),
                model
                    .source
                    .as_deref()
                    .map(|source| format!(" from {source}"))
                    .unwrap_or_default()
            )),
            self.meters_per_unit,
            self.up_axis,
        ));
    }

    fn write_materials(&self, out: &mut String) {
        out.push_str(&format!("    def Scope \"{MATERIALS_SCOPE}\"\n    {{\n"));
        for material in &self.materials {
            let name = &material.name;
            let path = format!("{}/{}", self.materials_path, material.name);
            out.push_str(&format!(
                "        def Material \"{name}\"\n        {{\n            token outputs:surface.connect = <{path}.outputs:surface>\n\n            def Shader \"PreviewSurface\"\n            {{\n                uniform token info:id = \"UsdPreviewSurface\"\n                color3f inputs:diffuseColor = ({}, {}, {})\n                color3f inputs:emissiveColor = ({}, {}, {})\n                float inputs:metalness = {}\n                float inputs:opacity = {}\n                float inputs:roughness = {}\n                token outputs:surface\n            }}\n        }}\n",
                number(material.diffuse[0]),
                number(material.diffuse[1]),
                number(material.diffuse[2]),
                number(material.emissive[0]),
                number(material.emissive[1]),
                number(material.emissive[2]),
                number(material.metallic),
                number(material.opacity),
                number(material.roughness),
            ));
        }
        out.push_str("    }\n\n");
    }

    /// One `Xform` per model node.
    fn write_node(
        &self,
        model: &Model,
        index: usize,
        depth: usize,
        out: &mut String,
    ) -> Result<()> {
        let node = model
            .nodes
            .get(index)
            .ok_or_else(|| Error::invalid(index as u64, "scene graph node is out of range"))?;
        let indent = "    ".repeat(depth);
        let inner = "    ".repeat(depth + 1);
        let name = path_name(&self.node_path[index]);

        let mut references: Vec<String> = Vec::new();
        for instance in &node.instances {
            let target = self.mesh_path.get(instance.mesh).cloned().ok_or_else(|| {
                Error::invalid(
                    instance.mesh as u64,
                    "node instance references a missing mesh",
                )
            })?;
            if !references.contains(&target) {
                references.push(target);
            }
        }

        // A binding here overrides the one on the referenced library prim, so
        // it is only worth writing when the node really asks for something
        // else than its mesh carries.
        let binding = self.override_binding(model, node);

        out.push_str(&format!("{indent}def Xform \"{name}\"\n"));
        let mut meta: Vec<String> = Vec::new();
        if let Some(kind) = node.kind.as_deref() {
            let original = node.name.as_deref().unwrap_or("");
            if original != name {
                meta.push(format!(
                    "doc = {}",
                    quote(&format!("JT {kind}: {original}"))
                ));
            } else {
                meta.push(format!("doc = {}", quote(&format!("JT {kind}"))));
            }
        }
        if binding.is_some() {
            meta.push("prepend apiSchemas = [\"MaterialBindingAPI\"]".to_owned());
        }
        if !references.is_empty() {
            meta.push(format!(
                "prepend references = [{}]",
                references
                    .iter()
                    .map(|target| format!("<{target}>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if meta.is_empty() {
            out.push_str(&format!("{indent}{{\n"));
        } else {
            out.push_str(&format!("{indent}(\n"));
            for entry in &meta {
                out.push_str(&format!("{inner}{entry}\n"));
            }
            out.push_str(&format!("{indent})\n{indent}{{\n"));
        }

        if let Some(target) = binding {
            out.push_str(&format!("{inner}rel material:binding = <{target}>\n"));
        }
        self.write_transform(node, depth + 1, out);

        for child in &node.children {
            if self.owner[*child] == index {
                self.write_node(model, *child, depth + 1, out)?;
            } else {
                self.write_shared(model, index, *child, depth + 1, out)?;
            }
        }
        out.push_str(&format!("{indent}}}\n"));
        Ok(())
    }

    /// A child that is written under a different parent: a thin prim that
    /// references it and authors nothing, so the node's own transform and
    /// whole subtree come through unchanged.
    fn write_shared(
        &self,
        model: &Model,
        parent: usize,
        child: usize,
        depth: usize,
        out: &mut String,
    ) -> Result<()> {
        let indent = "    ".repeat(depth);
        let path = self
            .shared_path
            .get(&(parent, child))
            .cloned()
            .or_else(|| self.node_path.get(child).cloned())
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                Error::invalid(
                    child as u64,
                    "scene graph child is not reachable from a scene root",
                )
            })?;
        let target = self
            .node_path
            .get(child)
            .filter(|path| !path.is_empty())
            .cloned()
            .ok_or_else(|| {
                Error::invalid(
                    child as u64,
                    "scene graph child is not reachable from a scene root",
                )
            })?;
        let kind = model
            .nodes
            .get(child)
            .and_then(|node| node.kind.as_deref())
            .unwrap_or("Xform");
        out.push_str(&format!(
            "{indent}def Xform \"{}\"\n{indent}(\n{}    doc = {}\n{}    prepend references = <{target}>\n{indent})\n{indent}{{\n{indent}}}\n",
            path_name(&path),
            indent,
            quote(&format!("JT {kind}: shared subtree")),
            indent,
        ));
        Ok(())
    }

    /// A material binding on a consumer, only when it differs from the one the
    /// referenced geometry already carries.
    fn override_binding(&self, model: &Model, node: &Node) -> Option<String> {
        let instance = node.instances.first()?;
        if node.instances.len() != 1 {
            // Several meshes, possibly several appearances: the per-mesh
            // bindings already say everything there is to say.
            return None;
        }
        let wanted = instance.material?;
        let owned = model
            .meshes
            .get(instance.mesh)
            .and_then(|mesh| self.mesh_material(mesh));
        (owned != Some(wanted))
            .then(|| self.material_path(wanted))
            .flatten()
    }

    /// `translate`, `orient`, `scale` — composed as `T · R · S`, the JT order.
    fn write_transform(&self, node: &Node, depth: usize, out: &mut String) {
        let indent = "    ".repeat(depth);
        let transform = node.transform;
        let moved = transform.translation != [0.0; 3];
        let turned = transform.rotation != [0.0, 0.0, 0.0, 1.0];
        let sized = transform.scale != [1.0; 3];
        if !moved && !turned && !sized {
            return;
        }
        if moved {
            out.push_str(&format!(
                "{indent}double3 xformOp:translate = ({}, {}, {})\n",
                number(transform.translation[0]),
                number(transform.translation[1]),
                number(transform.translation[2]),
            ));
        }
        if turned {
            let q = normalize_quaternion(transform.rotation);
            // JT is x, y, z, w; a USD quaternion literal is real, i, j, k.
            out.push_str(&format!(
                "{indent}quatf xformOp:orient = ({}, {}, {}, {})\n",
                number(q[3]),
                number(q[0]),
                number(q[1]),
                number(q[2]),
            ));
        }
        if sized {
            out.push_str(&format!(
                "{indent}float3 xformOp:scale = ({}, {}, {})\n",
                number(transform.scale[0]),
                number(transform.scale[1]),
                number(transform.scale[2]),
            ));
        }
        let order: Vec<&str> = [
            moved.then_some("xformOp:translate"),
            turned.then_some("xformOp:orient"),
            sized.then_some("xformOp:scale"),
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push_str(&format!(
            "{indent}uniform token[] xformOpOrder = [{}]\n",
            order
                .iter()
                .map(|op| format!("\"{op}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    /// The geometry library: every mesh exactly once, hidden from the scene.
    fn write_library(&self, model: &Model, out: &mut String) -> Result<()> {
        out.push_str(&format!(
            "def Xform \"{GEOMETRY_SCOPE}\"\n{{\n    token visibility = \"invisible\"\n"
        ));
        for (index, mesh) in model.meshes.iter().enumerate() {
            let binding = self
                .mesh_material(mesh)
                .and_then(|material| self.material_path(material));
            out.push_str(&format!("\n    def Xform \"{}\"", mesh_group(index)));
            if binding.is_some() {
                out.push_str(
                    "\n    (\n        prepend apiSchemas = [\"MaterialBindingAPI\"]\n    )",
                );
            }
            out.push_str("\n    {\n");
            if let Some(target) = &binding {
                out.push_str(&format!("        rel material:binding = <{target}>\n"));
            }
            for (slot, primitive) in mesh.primitives.iter().enumerate() {
                write_primitive(model, primitive, slot, mesh, 2, out)?;
            }
            out.push_str("    }\n");
        }
        out.push_str("}\n");
        Ok(())
    }
}

/// Write one primitive: a `Mesh` for the triangle topologies, `BasisCurves`
/// or `Points` for the rest.
fn write_primitive(
    model: &Model,
    primitive: &Primitive,
    slot: usize,
    mesh: &Mesh,
    depth: usize,
    out: &mut String,
) -> Result<()> {
    let indent = "    ".repeat(depth);
    let inner = "    ".repeat(depth + 1);
    let position = find_attribute(model, primitive, "POSITION")?.ok_or_else(|| {
        Error::Unsupported(
            "a primitive without a POSITION attribute cannot be exported to USD".into(),
        )
    })?;
    let positions = values(model, position)?;
    let points = format_vectors(model, position, &positions, 3);

    match primitive.topology {
        Topology::PointList => {
            out.push_str(&format!(
                "\n{indent}def Points \"{}\"\n{indent}{{\n",
                primitive_name(slot)
            ));
            array(out, &inner, "point3f[] points", &points, "");
            out.push_str(&format!("{indent}}}\n"));
            return Ok(());
        }
        Topology::LineList | Topology::LineStrip => {
            let counts: Vec<String> = match primitive.topology {
                Topology::LineList => vec!["2".to_owned(); points.len() / 2],
                _ => vec![points.len().to_string()],
            };
            out.push_str(&format!(
                "\n{indent}def BasisCurves \"{}\"\n{indent}{{\n{inner}uniform token type = \"linear\"\n",
                primitive_name(slot)
            ));
            array(out, &inner, "int[] curveVertexCounts", &counts, "");
            array(out, &inner, "point3f[] points", &points, "");
            out.push_str(&format!("{indent}}}\n"));
            return Ok(());
        }
        _ => {}
    }

    let faces = triangulate(model, primitive, positions.len() / 3)?;
    let normal = find_attribute(model, primitive, "NORMAL")?;

    let mut primvars: Vec<(String, String, Vec<String>)> = Vec::new();
    for handle in &primitive.attributes {
        let attribute = model
            .accessors
            .attribute(*handle)
            .ok_or_else(|| Error::invalid(*handle as u64, "attribute handle is out of range"))?;
        let upper = attribute.name.to_ascii_uppercase();
        let (base, set) = split_suffix(&upper);
        if base == "POSITION" || base == "NORMAL" {
            continue;
        }
        let width = usize::from(attribute.components);
        let (name, ty) = match base {
            "TEXCOORD" if (2..=4).contains(&width) => (
                if set == 0 {
                    "st".to_owned()
                } else {
                    format!("st{set}")
                },
                format!("texCoord{width}f"),
            ),
            "COLOR" if (3..=4).contains(&width) => (
                if set == 0 {
                    "displayColor".to_owned()
                } else {
                    format!("displayColor_{set}")
                },
                format!("color{width}f"),
            ),
            other if (2..=4).contains(&width) => {
                (other.to_ascii_lowercase(), format!("float{width}"))
            }
            other if width == 1 => (other.to_ascii_lowercase(), "float".to_owned()),
            other => {
                return Err(Error::Unsupported(format!(
                    "vertex attribute '{other}' with {width} components has no USD primvar type"
                )));
            }
        };
        let converted = format_vectors(model, *handle, &values(model, *handle)?, width);
        primvars.push((name, ty, converted));
    }

    out.push_str(&format!(
        "\n{indent}def Mesh \"{}\"\n{indent}(\n{inner}instanceable = true\n{indent})\n{indent}{{\n{inner}uniform token subdivisionScheme = \"linear\"\n",
        primitive_name(slot)
    ));
    let double_sided = primitive
        .material
        .and_then(|material| model.materials.get(material))
        .is_some_and(|material| material.double_sided);
    if double_sided {
        out.push_str(&format!("{inner}uniform bool doubleSided = 1\n"));
    }
    if let Some(bounds) = primitive.bounds.or(mesh.bounds) {
        out.push_str(&format!(
            "{inner}float3[] extent = [({}, {}, {}), ({}, {}, {})]\n",
            number(bounds.min[0]),
            number(bounds.min[1]),
            number(bounds.min[2]),
            number(bounds.max[0]),
            number(bounds.max[1]),
            number(bounds.max[2]),
        ));
    }
    let counts: Vec<String> = vec!["3".to_owned(); faces.len()];
    let indices: Vec<String> = faces
        .iter()
        .flat_map(|face| face.iter().map(|index| index.to_string()))
        .collect();
    array(out, &inner, "point3f[] points", &points, "");
    array(out, &inner, "int[] faceVertexCounts", &counts, "");
    array(out, &inner, "int[] faceVertexIndices", &indices, "");
    if let Some(handle) = normal.filter(|_| !positions.is_empty()) {
        let converted = format_vectors(model, handle, &values(model, handle)?, 3);
        vertex_array(out, &inner, "normal3f[] normals", &converted);
    }
    for (name, ty, converted) in primvars {
        vertex_array(out, &inner, &format!("{ty}[] primvars:{name}"), &converted);
    }
    out.push_str(&format!("{indent}}}\n"));
    Ok(())
}

/// An array literal plus its `interpolation = "vertex"` block: the per-vertex
/// data of a USD mesh must say so, or a consumer assumes uniform values.
///
/// The block has to open on the same line as the closing bracket — a `(` of its
/// own is a parse error — which is why [`array`] takes it as a trailer.
fn vertex_array(out: &mut String, inner: &str, declaration: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    let block = format!(" (\n{inner}    interpolation = \"vertex\"\n{inner})");
    array(out, inner, declaration, values, &block);
}

/// Emit `name = [` with one entry per line, then `]`, then `trailing`.
///
/// USD accepts a one-line array literal, but a mesh with a hundred thousand
/// vertices would produce a line no diff tool can read, so every entry gets
/// its own line.
fn array(out: &mut String, inner: &str, name: &str, values: &[String], trailing: &str) {
    out.push_str(&format!("{inner}{name} = [\n"));
    let item = format!("{inner}    ");
    for (index, value) in values.iter().enumerate() {
        let comma = if index + 1 == values.len() { "" } else { "," };
        out.push_str(&format!("{item}{value}{comma}\n"));
    }
    out.push_str(&format!("{inner}]{trailing}\n"));
}

/// The index list of a primitive, expanded into triangles.
///
/// USD meshes only know triangles, so a strip or a fan is unrolled here rather
/// than left for the consumer.
fn triangulate(
    model: &Model,
    primitive: &Primitive,
    vertex_count: usize,
) -> Result<Vec<[usize; 3]>> {
    let ring: Vec<usize> = match primitive.indices {
        Some(handle) => index_values(model, handle)?,
        None => (0..vertex_count).collect(),
    };
    let faces: Vec<[usize; 3]> = match primitive.topology {
        Topology::TriangleList => ring
            .chunks(3)
            .filter(|chunk| chunk.len() == 3)
            .map(|chunk| [chunk[0], chunk[1], chunk[2]])
            .collect(),
        Topology::TriangleStrip => (2..ring.len())
            .map(|i| [ring[i - 2], ring[i - 1], ring[i]])
            .collect(),
        Topology::TriangleFan => (2..ring.len())
            .map(|i| [ring[0], ring[i - 1], ring[i]])
            .collect(),
        other => {
            return Err(Error::Unsupported(format!(
                "{other:?} is not a triangle topology"
            )));
        }
    };
    if faces
        .iter()
        .any(|face| face.iter().any(|index| *index >= vertex_count))
    {
        return Err(Error::invalid(
            primitive.indices.unwrap_or_default() as u64,
            "an index is outside the vertex range",
        ));
    }
    Ok(faces)
}

/// The attribute of a primitive whose name matches `wanted`, case insensitively.
fn find_attribute(model: &Model, primitive: &Primitive, wanted: &str) -> Result<Option<usize>> {
    for handle in &primitive.attributes {
        let attribute = model
            .accessors
            .attribute(*handle)
            .ok_or_else(|| Error::invalid(*handle as u64, "attribute handle is out of range"))?;
        if attribute.name.eq_ignore_ascii_case(wanted) {
            return Ok(Some(*handle));
        }
    }
    Ok(None)
}

/// Decode every component of an attribute as an `f64`.
fn values(model: &Model, handle: usize) -> Result<Vec<f64>> {
    let attribute = model
        .accessors
        .attribute(handle)
        .ok_or_else(|| Error::invalid(handle as u64, "attribute handle is out of range"))?;
    let bytes = model
        .accessors
        .attribute_bytes(handle)
        .ok_or_else(|| Error::invalid(handle as u64, "attribute extends past its buffer"))?;
    let element = attribute.component_type.byte_len();
    if element == 0 || bytes.len() % element != 0 {
        return Err(Error::invalid(
            handle as u64,
            "attribute length is not a whole number of components",
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() / element);
    for chunk in bytes.chunks_exact(element) {
        out.push(read_component(attribute.component_type, chunk)?);
    }
    Ok(out)
}

/// Decode an index attribute as vertex indices.
fn index_values(model: &Model, handle: usize) -> Result<Vec<usize>> {
    let decoded = values(model, handle)?;
    decoded
        .iter()
        .map(|value| {
            if !value.is_finite() || *value < 0.0 || *value > usize::MAX as f64 {
                return Err(Error::invalid(handle as u64, "an index is out of range"));
            }
            Ok(*value as usize)
        })
        .collect()
}

/// Group decoded components into `(a, b, c)` style literals, one per element.
fn format_vectors(model: &Model, handle: usize, values: &[f64], width: usize) -> Vec<String> {
    let component = model
        .accessors
        .attribute(handle)
        .map(|attribute| attribute.component_type)
        .unwrap_or(ComponentType::F32);
    values
        .chunks(width.max(1))
        .map(|chunk| {
            format!(
                "({})",
                chunk
                    .iter()
                    .map(|value| scalar(component, *value))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect()
}

/// One component as a USD literal, in the shape its storage implies.
fn scalar(component: ComponentType, value: f64) -> String {
    match component {
        ComponentType::F32 | ComponentType::F16 => number(value as f32),
        ComponentType::F64 => number(value),
        ComponentType::I8
        | ComponentType::U8
        | ComponentType::I16
        | ComponentType::U16
        | ComponentType::I32
        | ComponentType::U32 => (value.round() as i64).to_string(),
        ComponentType::I64 | ComponentType::U64 => format!("{value:.0}"),
    }
}

/// A float literal. Rust's shortest round-trip form is what USD wants, and
/// `.usda` accepts `nan` / `inf` / `-inf` for the non-finite cases.
fn number(value: impl Into<f64>) -> String {
    let value = value.into();
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "inf".to_owned()
        } else {
            "-inf".to_owned()
        };
    }
    value.to_string()
}

/// A USD string literal.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The last component of an absolute prim path.
fn path_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Prim name of the `index`th mesh group in the geometry library.
fn mesh_group(index: usize) -> String {
    format!("GEOMETRY_{index}")
}

/// Prim name of the `slot`th primitive of a mesh.
fn primitive_name(slot: usize) -> String {
    if slot == 0 {
        "Mesh".to_owned()
    } else {
        format!("Mesh_{}", slot + 1)
    }
}

/// A usable USD identifier: `[A-Za-z_][A-Za-z0-9_]*`.
///
/// JT names are full of dots — `example_block_nx4.asm` — which USD rejects, so
/// every character outside the set becomes an underscore.
fn sanitize(name: &str, fallback: usize) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        out.push(if ch.is_ascii_alphanumeric() || ch == '_' {
            ch
        } else {
            '_'
        });
    }
    if out.is_empty() {
        return format!("Node_{fallback}");
    }
    if out.as_bytes()[0].is_ascii_digit() {
        out.insert(0, '_');
    }
    out
}

/// Make `name` unique inside `used`, appending `_2`, `_3`, ... as needed.
fn unique_name(name: &str, used: &mut BTreeSet<String>) -> String {
    let mut candidate = name.to_owned();
    let mut counter = 2;
    while !used.insert(candidate.clone()) {
        candidate = format!("{name}_{counter}");
        counter += 1;
    }
    candidate
}

/// The display name of a node: its source name when it has one.
fn node_name(model: &Model, index: usize, node: &Node) -> String {
    let _ = model;
    sanitize(node.name.as_deref().unwrap_or(""), index)
}

/// The name of the default prim, from the scene or the source file.
fn default_name(model: &Model) -> String {
    let hint = model
        .scenes
        .first()
        .and_then(|scene| scene.name.clone())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            model
                .source
                .as_deref()
                .and_then(|source| source.rsplit(['/', '\\']).next())
                .map(|file| file.split('.').next().unwrap_or(file).to_owned())
        });
    sanitize(hint.unwrap_or_else(|| DEFAULT_PRIM.to_owned()).as_str(), 0)
}

/// Meters per model unit, from [`Model::unit`]; JT is millimetre based.
fn meters_per_unit(model: &Model) -> Result<f64> {
    let Some(unit) = model.unit.as_deref() else {
        return Ok(DEFAULT_METERS_PER_UNIT);
    };
    Ok(
        match unit
            .to_ascii_lowercase()
            .replace([' ', '-', '_'], "")
            .as_str()
        {
            "meter" | "meters" | "metre" | "metres" | "m" => 1.0,
            "decimeter" | "decimeters" | "decimetre" | "decimetres" => 0.1,
            "centimeter" | "centimeters" | "centimetre" | "centimetres" | "cm" => 0.01,
            "millimeter" | "millimeters" | "millimetre" | "millimetres" | "mm" => 0.001,
            "micrometer" | "micrometers" | "micron" | "microns" => 1e-6,
            "nanometer" | "nanometers" => 1e-9,
            "kilometer" | "kilometers" | "kilometre" | "kilometres" | "km" => 1000.0,
            "inch" | "inches" => 0.0254,
            "foot" | "feet" => 0.3048,
            "yard" | "yards" => 0.9144,
            "mil" | "mils" => 0.000_025_4,
            "microinch" | "microinches" => 2.54e-8,
            other => {
                return Err(Error::Unsupported(format!(
                    "unit '{other}' has no USD metersPerUnit equivalent"
                )));
            }
        },
    )
}

/// The USD up axis, from [`Model::up_axis`]; JT is Z up.
fn up_axis(model: &Model) -> Result<&'static str> {
    let Some(axis) = model.up_axis.as_deref() else {
        return Ok("Z");
    };
    Ok(
        match axis
            .to_ascii_uppercase()
            .replace(['+', ' ', '-', '_'], "")
            .as_str()
        {
            "X" => "X",
            "Y" => "Y",
            "Z" => "Z",
            other => {
                return Err(Error::Unsupported(format!(
                    "up axis '{other}' is not one of X, Y or Z"
                )));
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Aabb, Attribute, Channel, Instance, Material, Transform};
    use serde_json::json;

    /// A flat `f32` array as its own buffer plus an attribute over it.
    fn attribute(model: &mut Model, name: &str, values: &[f32], components: u8) -> usize {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let buffer = model.accessors.push_buffer(bytes);
        model.accessors.push_attribute(Attribute {
            name: name.into(),
            buffer,
            offset: 0,
            count: values.len() / usize::from(components),
            components,
            component_type: ComponentType::F32,
            normalized: false,
            min: None,
            max: None,
        })
    }

    /// Vertex indices as their own buffer plus an attribute over it.
    fn indices(model: &mut Model, values: &[u32]) -> usize {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let buffer = model.accessors.push_buffer(bytes);
        model.accessors.push_attribute(Attribute {
            name: "INDICES".into(),
            buffer,
            offset: 0,
            count: values.len(),
            components: 1,
            component_type: ComponentType::U32,
            normalized: false,
            min: None,
            max: None,
        })
    }

    const CORNERS: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    /// Indent of a Mesh prim body: the library nests them two levels deep.
    const IN: &str = "            ";
    /// Indent of one array entry inside that body.
    const ITEM: &str = "                ";

    /// A single triangle, bound to `material` when given.
    fn triangle_mesh(model: &mut Model, material: Option<usize>) -> usize {
        let position = attribute(model, "POSITION", &CORNERS, 3);
        let index = indices(model, &[0, 1, 2]);
        model.push_mesh(Mesh {
            name: Some("triangle".into()),
            primitives: vec![Primitive {
                attributes: vec![position],
                indices: Some(index),
                topology: Topology::TriangleList,
                material,
                bounds: Some(Aabb::new([0.0; 3], [1.0, 1.0, 0.0])),
                extras: None,
            }],
            bounds: Some(Aabb::new([0.0; 3], [1.0, 1.0, 0.0])),
            lod_level: Some(0),
            extras: None,
        })
    }

    /// A scene named `asm` whose single root node draws one triangle.
    fn single_mesh_model() -> Model {
        let mut model = Model::new();
        model.default_scene_mut().name = Some("asm".into());
        let mesh = triangle_mesh(&mut model, None);
        let node = model.push_node(Node {
            name: Some("part.asm".into()),
            kind: Some("Shape".into()),
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

    /// A root with `names` as children, all drawing `mesh`. Returns the root.
    fn instanced_model(model: &mut Model, mesh: usize, names: &[&str]) -> usize {
        model.default_scene_mut().name = Some("asm".into());
        let root = model.push_node(Node::named("top"));
        for name in names {
            let child = model.push_node(Node {
                name: Some((*name).into()),
                instances: vec![Instance {
                    mesh,
                    material: None,
                    lod_bias: 1.0,
                }],
                ..Node::default()
            });
            model.nodes[root].children.push(child);
        }
        model.default_scene_mut().roots.push(root);
        root
    }

    #[test]
    fn a_layer_starts_with_the_usd_header() {
        let text = UsdExporter.to_text(&single_mesh_model()).unwrap();
        assert!(text.starts_with("#usda 1.0\n("));
        assert!(text.contains("defaultPrim = \"asm\""));
        // JT is millimetre based and Z up, so that is the fallback.
        assert!(text.contains("metersPerUnit = 0.001"));
        assert!(text.contains("upAxis = \"Z\""));
        // An identity transform is worth no ops at all.
        assert!(!text.contains("xformOp"));
    }

    #[test]
    fn the_unit_and_up_axis_of_the_model_reach_the_header() {
        let mut model = single_mesh_model();
        model.unit = Some("inch".into());
        model.up_axis = Some("+Y".into());
        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains("metersPerUnit = 0.0254"));
        assert!(text.contains("upAxis = \"Y\""));
    }

    #[test]
    fn a_unit_usd_cannot_express_is_refused() {
        let mut model = single_mesh_model();
        model.unit = Some("furlong".into());
        let error = UsdExporter.to_text(&model).unwrap_err();
        assert!(error.to_string().contains("furlong"));
    }

    #[test]
    fn a_source_name_becomes_a_usd_identifier() {
        // JT names are full of dots, which USD rejects.
        let text = UsdExporter.to_text(&single_mesh_model()).unwrap();
        assert!(text.contains("def Xform \"part_asm\""));
        assert!(text.contains("doc = \"JT Shape: part.asm\""));
    }

    #[test]
    fn siblings_with_the_same_source_name_get_unique_paths() {
        // USD rejects two prims of one name under one parent, and a JT file
        // happily repeats a name across instances.
        let mut model = Model::new();
        model.default_scene_mut().name = Some("asm".into());
        let root = model.push_node(Node::named("top"));
        let first = model.push_node(Node::named("screw.asm"));
        let second = model.push_node(Node::named("screw.asm"));
        let third = model.push_node(Node::named("screw.asm"));
        model.nodes[root].children = vec![first, second, third];
        model.default_scene_mut().roots.push(root);
        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains("def Xform \"screw_asm\""));
        assert!(text.contains("def Xform \"screw_asm_2\""));
        assert!(text.contains("def Xform \"screw_asm_3\""));
    }

    #[test]
    fn a_mesh_is_written_once_and_referenced_by_every_node() {
        let mut model = Model::new();
        let mesh = triangle_mesh(&mut model, None);
        instanced_model(&mut model, mesh, &["left", "right"]);
        let text = UsdExporter.to_text(&model).unwrap();

        // Exactly one copy of the geometry, in the library …
        assert_eq!(text.matches("def Xform \"GEOMETRY_0\"").count(), 1);
        assert_eq!(text.matches("point3f[] points").count(), 1);
        // … and one reference per node that draws it.
        assert_eq!(
            text.matches("prepend references = [</FerroJT_Geometry/GEOMETRY_0>]")
                .count(),
            2
        );
        // The library mesh is instanceable, so the repeats stay instances
        // rather than copies in a consumer.
        assert_eq!(text.matches("instanceable = true").count(), 1);
    }

    #[test]
    fn a_node_reached_from_two_parents_is_referenced_not_copied() {
        let mut model = Model::new();
        let mesh = triangle_mesh(&mut model, None);
        model.default_scene_mut().name = Some("asm".into());
        let root = model.push_node(Node::named("top"));
        let first = model.push_node(Node::named("left"));
        let second = model.push_node(Node::named("right"));
        let shared = model.push_node(Node {
            name: Some("screw".into()),
            kind: Some("Shape".into()),
            transform: Transform::translated([0.0, 25.0, 0.0]),
            instances: vec![Instance {
                mesh,
                material: None,
                lod_bias: 1.0,
            }],
            ..Node::default()
        });
        model.nodes[first].children.push(shared);
        model.nodes[second].children.push(shared);
        model.nodes[root].children = vec![first, second];
        model.default_scene_mut().roots.push(root);

        let text = UsdExporter.to_text(&model).unwrap();
        // Written once, under its first parent …
        assert_eq!(text.matches("point3f[] points").count(), 1);
        // … and pulled in by a wrapper that authors nothing of its own, so the
        // node's own transform comes through with the reference.
        assert!(text.contains("prepend references = </asm/top/left/screw>"));
        assert!(text.contains("doc = \"JT Shape: shared subtree\""));
        assert_eq!(
            text.matches("double3 xformOp:translate = (0, 25, 0)")
                .count(),
            1
        );
    }

    #[test]
    fn materials_with_the_same_values_collapse_into_one_prim() {
        let mut model = Model::new();
        let grey = model.push_material(Material {
            name: Some("grey".into()),
            base_color: Channel::constant([0.5, 0.5, 0.5, 1.0]),
            ..Material::default()
        });
        // Same shading values, different name: still one USD material.
        let again = model.push_material(Material {
            name: Some("grey again".into()),
            base_color: Channel::constant([0.5, 0.5, 0.5, 1.0]),
            ..Material::default()
        });
        let red = model.push_material(Material {
            name: Some("red".into()),
            base_color: Channel::constant([1.0, 0.0, 0.0, 1.0]),
            ..Material::default()
        });
        let first = triangle_mesh(&mut model, Some(grey));
        let second = triangle_mesh(&mut model, Some(again));
        let third = triangle_mesh(&mut model, Some(red));
        let root = instanced_model(&mut model, first, &["a"]);
        for mesh in [second, third] {
            let child = model.push_node(Node {
                instances: vec![Instance {
                    mesh,
                    material: None,
                    lod_bias: 1.0,
                }],
                ..Node::default()
            });
            model.nodes[root].children.push(child);
        }

        let text = UsdExporter.to_text(&model).unwrap();
        assert_eq!(text.matches("def Material ").count(), 2);
        // Every mesh still gets its own library group and its own binding.
        assert_eq!(text.matches("def Xform \"GEOMETRY_").count(), 3);
        assert_eq!(
            text.matches("rel material:binding = </asm/Materials/Material_0>")
                .count(),
            2
        );
        assert_eq!(
            text.matches("rel material:binding = </asm/Materials/Material_1>")
                .count(),
            1
        );
    }

    #[test]
    fn a_node_can_override_the_appearance_of_its_mesh() {
        let mut model = Model::new();
        let grey = model.push_material(Material::default());
        let red = model.push_material(Material {
            base_color: Channel::constant([1.0, 0.0, 0.0, 1.0]),
            ..Material::default()
        });
        let mesh = triangle_mesh(&mut model, Some(grey));
        instanced_model(&mut model, mesh, &["part"]);
        // The instance asks for something else than its mesh carries.
        model.nodes[1].instances[0].material = Some(red);
        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains("rel material:binding = </asm/Materials/Material_0>"));
        assert!(text.contains("rel material:binding = </asm/Materials/Material_1>"));
    }

    #[test]
    fn a_binding_is_prim_metadata_on_the_library_group() {
        // `apiSchemas` in a prim *body* is a parse error, and a reference only
        // carries what the referenced prim states — so the binding has to sit
        // on the group the consumers reference.
        let mut model = Model::new();
        let material = model.push_material(Material::default());
        let mesh = triangle_mesh(&mut model, Some(material));
        instanced_model(&mut model, mesh, &["part"]);
        let text = UsdExporter.to_text(&model).unwrap();
        assert!(
            text.contains("    def Xform \"GEOMETRY_0\"\n    (\n        prepend apiSchemas = [\"MaterialBindingAPI\"]\n    )\n    {\n        rel material:binding = </asm/Materials/Material_0>")
        );
    }

    #[test]
    fn the_library_is_a_sibling_of_the_default_prim_and_invisible() {
        let text = UsdExporter.to_text(&single_mesh_model()).unwrap();
        assert!(
            text.contains(
                "def Xform \"FerroJT_Geometry\"\n{\n    token visibility = \"invisible\""
            )
        );
        assert!(text.contains("\n}\n\ndef Xform \"FerroJT_Geometry\""));
    }

    #[test]
    fn a_quaternion_is_written_real_first() {
        let mut model = single_mesh_model();
        // 180° about Z is `x, y, z, w = 0, 0, 1, 0`; a USD quaternion literal is
        // `real, i, j, k`, so `w` moves to the front and `z` to the back.
        model.nodes[0].transform.rotation = [0.0, 0.0, 1.0, 0.0];
        model.nodes[0].transform.translation = [1.0, 2.0, 3.0];
        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains("quatf xformOp:orient = (0, 0, 0, 1)"));
        assert!(text.contains("double3 xformOp:translate = (1, 2, 3)"));
        // T · R · S, which is what a JT `GeometricTransform` means.
        assert!(text.contains(
            "uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:orient\"]"
        ));
    }

    #[test]
    fn per_vertex_arrays_declare_vertex_interpolation() {
        let mut model = Model::new();
        model.default_scene_mut().name = Some("asm".into());
        let position = attribute(&mut model, "POSITION", &CORNERS, 3);
        let normal = attribute(
            &mut model,
            "NORMAL",
            &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            3,
        );
        let uv = attribute(&mut model, "TEXCOORD_0", &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0], 2);
        let color = attribute(
            &mut model,
            "COLOR_0",
            &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            3,
        );
        let index = indices(&mut model, &[0, 1, 2]);
        let mesh = model.push_mesh(Mesh {
            primitives: vec![Primitive {
                attributes: vec![position, normal, uv, color],
                indices: Some(index),
                topology: Topology::TriangleList,
                material: None,
                bounds: Some(Aabb::new([0.0; 3], [1.0, 1.0, 0.0])),
                extras: None,
            }],
            ..Mesh::default()
        });
        instanced_model(&mut model, mesh, &["part"]);

        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains("normal3f[] normals = ["));
        assert!(text.contains("texCoord2f[] primvars:st = ["));
        assert!(text.contains("color3f[] primvars:displayColor = ["));
        assert_eq!(text.matches("interpolation = \"vertex\"").count(), 3);
        // The block has to open on the line that closes the array — a `(` of
        // its own is a parse error.
        assert!(text.contains(&format!("] (\n{ITEM}interpolation = \"vertex\"\n{IN})")));
        assert!(text.contains(&format!("{IN}float3[] extent = [(0, 0, 0), (1, 1, 0)]")));
    }

    #[test]
    fn points_and_lines_get_their_own_prim_type() {
        let mut model = Model::new();
        let points = attribute(&mut model, "POSITION", &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 3);
        let point_mesh = model.push_mesh(Mesh {
            primitives: vec![Primitive {
                attributes: vec![points],
                indices: None,
                topology: Topology::PointList,
                material: None,
                bounds: None,
                extras: None,
            }],
            ..Mesh::default()
        });
        let line = attribute(
            &mut model,
            "POSITION",
            &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0],
            3,
        );
        let line_mesh = model.push_mesh(Mesh {
            primitives: vec![Primitive {
                attributes: vec![line],
                indices: None,
                topology: Topology::LineList,
                material: None,
                bounds: None,
                extras: None,
            }],
            ..Mesh::default()
        });
        let root = instanced_model(&mut model, point_mesh, &["dots"]);
        let line_node = model.push_node(Node {
            instances: vec![Instance {
                mesh: line_mesh,
                material: None,
                lod_bias: 1.0,
            }],
            ..Node::default()
        });
        model.nodes[root].children.push(line_node);

        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains("def Points \"Mesh\""));
        assert!(text.contains("def BasisCurves \"Mesh\""));
        assert!(text.contains("uniform token type = \"linear\""));
        assert!(text.contains(&format!(
            "{IN}int[] curveVertexCounts = [\n{ITEM}2,\n{ITEM}2\n{IN}]"
        )));
        assert!(!text.contains("def Mesh \"Mesh\""));
    }

    #[test]
    fn a_strip_or_fan_is_triangulated() {
        let mut model = Model::new();
        let position = attribute(&mut model, "POSITION", &CORNERS, 3);
        let index = indices(&mut model, &[0, 1, 2]);
        let mesh = model.push_mesh(Mesh {
            primitives: vec![Primitive {
                attributes: vec![position],
                indices: Some(index),
                topology: Topology::TriangleStrip,
                material: None,
                bounds: None,
                extras: None,
            }],
            ..Mesh::default()
        });
        instanced_model(&mut model, mesh, &["part"]);
        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains(&format!("{IN}int[] faceVertexCounts = [\n{ITEM}3\n{IN}]")));
        assert!(text.contains(&format!(
            "{IN}int[] faceVertexIndices = [\n{ITEM}0,\n{ITEM}1,\n{ITEM}2\n{IN}]"
        )));
    }

    #[test]
    fn the_layer_filter_that_produced_the_model_is_recorded() {
        let mut model = single_mesh_model();
        model.source = Some("data/ExampleFiles/x.jt".into());
        model.extras = Some(json!({ "layer_filter": { "name": "Layer 3", "layers": [1, 7] } }));
        let text = UsdExporter.to_text(&model).unwrap();
        assert!(text.contains("string \"ferrojt:layer_filter\" = \"Layer 3\""));
        assert!(text.contains("int[] \"ferrojt:layers\" = [1, 7]"));
        assert!(text.contains("string \"ferrojt:source\" = \"data/ExampleFiles/x.jt\""));
        assert!(text.contains("string \"ferrojt:generator\" = \"FerroJT"));
    }

    #[test]
    fn a_primitive_without_positions_is_refused() {
        // glTF mandates POSITION and so does a USD mesh.
        let mut model = single_mesh_model();
        model.meshes[0].primitives[0].attributes.clear();
        let error = UsdExporter.to_text(&model).unwrap_err();
        assert!(error.to_string().contains("POSITION"));
    }

    #[test]
    fn a_child_outside_the_node_table_is_reported() {
        let mut model = single_mesh_model();
        model.nodes[0].children.push(999);
        let error = UsdExporter.to_text(&model).unwrap_err();
        assert!(error.to_string().contains("out of range"));
    }
}
