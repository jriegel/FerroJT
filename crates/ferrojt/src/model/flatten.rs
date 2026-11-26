//! Baking a whole scene graph into one mesh.
//!
//! A JT file spreads its geometry over a scene graph: parts, instances of the
//! same part at several placements, level-of-detail spans and assemblies with a
//! transform of their own. [`Flattened`] throws that structure away and keeps a
//! single triangle soup: every mesh of the model is copied once **per place it
//! is drawn**, with the world transform baked into the coordinates, so what is
//! left is one mesh that shows the same pixels and has nothing left to
//! traverse.
//!
//! The walk follows the same rules as the exporters: it starts at the scene
//! roots, descends into every child, and expands a subtree shared by several
//! parents once per path — which is what duplicates an instanced screw onto
//! both sides of a plate. What is *not* kept is the structure itself: node
//! names, key/value properties, textures (there are no texture coordinates to
//! carry over) and the level-of-detail tiers. Which tiers and layers are in the
//! input is decided before flattening, by the [`crate::jt`] read options,
//! exactly as for any other export.

use serde_json::json;

use crate::export::read_component;
use crate::model::geometry::{Attribute, BufferBlock, ComponentType, Mesh, Primitive, Topology};
use crate::model::{Aabb, Instance, Model, Node, Scene, Transform};

/// The geometry of a whole model, baked into one mesh.
///
/// Built by [`Flattened::of`]; [`Model::flattened`] wraps the same arrays back
/// into a [`Model`] the exporters consume unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flattened {
    /// World-space positions, three values per vertex.
    pub vertices: Vec<f32>,
    /// World-space unit normals, three values per vertex.
    ///
    /// Empty when no primitive of the model carried a `NORMAL` attribute. A
    /// primitive without normals alongside one that has them gets smooth
    /// normals computed from its own triangles, so the channel stays aligned
    /// with the positions.
    pub normals: Vec<f32>,
    /// Triangle indices into [`Flattened::vertices`], three per triangle.
    pub indices: Vec<u32>,
    /// Runs of indices that share one material, in merge order.
    pub groups: Vec<FlatGroup>,
    /// Bounds of the merged geometry, `None` when there is none.
    pub bounds: Option<Aabb>,
}

/// One run of the merged mesh: a range of indices and the vertices they name.
///
/// The merged mesh keeps one primitive per run so the colours of the file
/// survive the merge; the arrays themselves are one continuous mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlatGroup {
    /// Index into [`Model::materials`], `None` for unshaded geometry.
    pub material: Option<usize>,
    /// First vertex of the run.
    pub first_vertex: usize,
    /// Number of vertices in the run.
    pub vertex_count: usize,
    /// First index of the run.
    pub first_index: usize,
    /// Number of indices in the run, always a multiple of three.
    pub index_count: usize,
}

impl Flattened {
    /// Merge every mesh of `model` into one.
    ///
    /// The walk starts at the scene roots, so the model has to carry them —
    /// which every reader of this crate does. A node reachable from several
    /// parents is walked once per path, which is how an instanced part ends up
    /// drawn at each of its placements; a node that is its own ancestor through
    /// a cycle is skipped. Geometry a consumer could not use anyway (a
    /// primitive without a `POSITION` attribute, a topology that is not a
    /// triangle list) is skipped as well.
    #[must_use]
    pub fn of(model: &Model) -> Self {
        let mut out = Flattened::default();
        // Normals are all or nothing: one merged mesh cannot mix primitives
        // with and without them, so a primitive that has none gets computed
        // ones as soon as any other primitive has them.
        let want_normals = model.meshes.iter().any(|mesh| {
            mesh.primitives.iter().any(|primitive| {
                primitive
                    .attributes
                    .iter()
                    .any(|handle| named(model, *handle, "NORMAL"))
            })
        });
        let roots: Vec<usize> = model
            .scenes
            .iter()
            .flat_map(|scene| scene.roots.iter().copied())
            .collect();
        let mut path: Vec<usize> = Vec::new();
        for root in roots {
            walk(
                model,
                root,
                &Matrix::IDENTITY,
                want_normals,
                &mut path,
                &mut out,
            );
        }
        out
    }

    /// Vertices of the merged mesh.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / 3
    }

    /// Triangles of the merged mesh.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// `true` when nothing was merged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Append the run of one primitive, merging it into the previous group when
    /// the material agrees, so a part that repeats one colour across its
    /// primitives stays one run.
    fn push_group(&mut self, material: Option<usize>, vertex_count: usize, index_count: usize) {
        let first_vertex = self.vertices.len() / 3 - vertex_count;
        let first_index = self.indices.len() - index_count;
        let mut merged = false;
        if let Some(last) = self.groups.last_mut() {
            if last.material == material
                && last.first_index + last.index_count == self.indices.len() - index_count
            {
                last.vertex_count += vertex_count;
                last.index_count += index_count;
                merged = true;
            }
        }
        if merged {
            return;
        }
        self.groups.push(FlatGroup {
            material,
            first_vertex,
            vertex_count,
            first_index,
            index_count,
        });
    }

    /// The arrays as a [`Model`]: one node, one mesh, one primitive per group.
    ///
    /// `source` supplies everything that is not geometry — unit, up axis,
    /// source name and the materials of the runs, of which only the used ones
    /// are kept.
    #[must_use]
    pub fn to_model(&self, source: &Model) -> Model {
        let name = merge_name(source);
        let mut model = Model {
            unit: source.unit.clone(),
            up_axis: source.up_axis.clone(),
            source: source.source.clone(),
            bounds: self.bounds,
            ..Model::default()
        };
        let mut extras = source.extras.clone().unwrap_or_else(|| json!({}));
        if let Some(map) = extras.as_object_mut() {
            map.insert("flattened".to_string(), json!(true));
            map.insert("flat_groups".to_string(), json!(self.groups.len()));
        }
        model.extras = Some(extras);

        // --- the merged buffers ---------------------------------------------
        // The index buffer of the model re-bases every run onto its own vertex
        // slice: an index names a vertex of its own primitive in glTF, USD and
        // JT, while [`Flattened::indices`] are the global indices of the
        // merged mesh.
        let mut index_bytes: Vec<u8> = Vec::with_capacity(self.indices.len() * 4);
        for group in &self.groups {
            let end = group.first_index + group.index_count;
            for index in &self.indices[group.first_index..end] {
                let local = *index - group.first_vertex as u32;
                index_bytes.extend_from_slice(&local.to_le_bytes());
            }
        }
        let position_bytes: Vec<u8> = self
            .vertices
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        model.accessors.buffers = vec![
            BufferBlock::new(index_bytes),
            BufferBlock::new(position_bytes),
        ];
        if !self.normals.is_empty() {
            let normal_bytes: Vec<u8> = self
                .normals
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
            model.accessors.buffers.push(BufferBlock::new(normal_bytes));
        }

        // --- materials: only the runs that exist ----------------------------
        let mut kept: Vec<usize> = Vec::new();
        for group in &self.groups {
            if let Some(index) = group.material {
                if !kept.contains(&index) {
                    kept.push(index);
                }
            }
        }
        model.materials = kept
            .iter()
            .filter_map(|index| source.materials.get(*index).cloned())
            .collect();

        // --- one mesh, one primitive per run --------------------------------
        let with_normals = !self.normals.is_empty();
        let mut primitives: Vec<Primitive> = Vec::with_capacity(self.groups.len());
        let mut index_offset = 0usize;
        for group in &self.groups {
            let material = group
                .material
                .and_then(|index| kept.iter().position(|used| *used == index));
            let indices = model.accessors.push_attribute(Attribute {
                name: "indices".to_string(),
                buffer: 0,
                offset: index_offset,
                count: group.index_count,
                components: 1,
                component_type: ComponentType::U32,
                normalized: false,
                min: None,
                max: None,
            });
            index_offset += group.index_count * 4;
            let mut attributes = vec![model.accessors.push_attribute(Attribute {
                name: "POSITION".to_string(),
                buffer: 1,
                offset: group.first_vertex * 12,
                count: group.vertex_count,
                components: 3,
                component_type: ComponentType::F32,
                normalized: false,
                min: None,
                max: None,
            })];
            if with_normals {
                attributes.push(model.accessors.push_attribute(Attribute {
                    name: "NORMAL".to_string(),
                    buffer: 2,
                    offset: group.first_vertex * 12,
                    count: group.vertex_count,
                    components: 3,
                    component_type: ComponentType::F32,
                    normalized: false,
                    min: None,
                    max: None,
                }));
            }
            primitives.push(Primitive {
                attributes,
                indices: Some(indices),
                topology: Topology::TriangleList,
                material,
                bounds: None,
                extras: None,
            });
        }
        model.meshes.push(Mesh {
            name: Some(name.clone()),
            primitives,
            bounds: self.bounds,
            lod_level: Some(0),
            extras: Some(json!({ "flattened": true })),
        });

        // A plain `Group`, so writing the model back as JT still gets the
        // wrapper chain a consumer walks (partition, meta data, instance, part,
        // level-of-detail span).
        model.nodes.push(Node {
            name: Some(name.clone()),
            kind: Some("Group".to_string()),
            instances: vec![Instance {
                mesh: 0,
                material: None,
                lod_bias: 0.0,
            }],
            bounds: self.bounds,
            ..Node::default()
        });
        model.scenes.push(Scene {
            name: Some(name),
            roots: vec![0],
        });
        model
    }
}

impl Model {
    /// A copy of the model with one mesh: every instance baked into world
    /// space, every node transform folded into the coordinates.
    ///
    /// No exporter needs support for this — a model with a single node and a
    /// single mesh is an ordinary scene graph — so flattening is an option of
    /// the caller, not of a writer. The materials of the file survive as one
    /// primitive each; the scene graph itself does not.
    #[must_use]
    pub fn flattened(&self) -> Model {
        Flattened::of(self).to_model(self)
    }

    /// The merged geometry of the model as raw arrays, without rebuilding it as
    /// a scene graph.
    #[must_use]
    pub fn flatten(&self) -> Flattened {
        Flattened::of(self)
    }
}

/// Name of the merged mesh: the stem of the source, or a fixed stand-in.
fn merge_name(model: &Model) -> String {
    model
        .source
        .as_deref()
        .map(|source| {
            let base = source.rsplit(['/', '\\']).next().unwrap_or(source);
            match base.rfind('.') {
                Some(dot) if dot > 0 => &base[..dot],
                _ => base,
            }
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or("flattened")
        .to_string()
}

/// Depth first walk that accumulates the world transform of every node.
fn walk(
    model: &Model,
    index: usize,
    parent: &Matrix,
    want_normals: bool,
    path: &mut Vec<usize>,
    out: &mut Flattened,
) {
    // A node reached through itself is a cycle; a node reached from two parents
    // is not, and that second case is exactly the duplication an instance needs.
    if path.contains(&index) {
        return;
    }
    path.push(index);
    if let Some(node) = model.nodes.get(index) {
        let world = parent.then(&node.transform);
        for instance in &node.instances {
            if let Some(mesh) = model.meshes.get(instance.mesh) {
                emit(model, mesh, instance.material, &world, want_normals, out);
            }
        }
        for child in &node.children {
            walk(model, *child, &world, want_normals, path, out);
        }
    }
    path.pop();
}

/// Copy one mesh into the merged arrays, transformed by `world`.
fn emit(
    model: &Model,
    mesh: &Mesh,
    override_material: Option<usize>,
    world: &Matrix,
    want_normals: bool,
    out: &mut Flattened,
) {
    for primitive in &mesh.primitives {
        if primitive.topology != Topology::TriangleList {
            continue;
        }
        let Some(slots) = slot_list(model, primitive) else {
            continue;
        };
        let Some(positions) = array_of(model, primitive, "POSITION") else {
            continue;
        };

        // The largest slot the index list names. A real CAD export can point
        // one past its own coordinate array and the reader already reads such
        // a vertex as zeros, so the merged mesh pads the same zeros: the
        // triangle count of the file is preserved either way.
        let highest = slots.iter().copied().max().unwrap_or(0) as usize;
        let count = positions.count.max(highest + 1);

        let mut local: Vec<[f32; 3]> = Vec::with_capacity(count);
        for slot in 0..count {
            let point = if slot < positions.count {
                world.point(positions.point(slot))
            } else {
                world.point([0.0; 3])
            };
            local.push(point);
            out.vertices.extend_from_slice(&point);
            out.bounds = Some(match out.bounds {
                Some(box_) => box_.union(Aabb::new(point, point)),
                None => Aabb::new(point, point),
            });
        }

        if want_normals {
            match array_of(model, primitive, "NORMAL") {
                Some(normals) => {
                    let normal_matrix = world.normals();
                    for slot in 0..count {
                        let vector = if slot < normals.count {
                            normals.point(slot)
                        } else {
                            [0.0; 3]
                        };
                        let turned = normalize(&normal_matrix.vector(vector));
                        out.normals.extend_from_slice(&turned);
                    }
                }
                None => computed_normals(&mut out.normals, &local, &slots),
            }
        }

        let base = out.vertices.len() / 3 - count;
        let first_index = out.indices.len();
        for slot in &slots {
            out.indices.push((base + *slot as usize) as u32);
        }
        let index_count = out.indices.len() - first_index;
        out.push_group(override_material.or(primitive.material), count, index_count);
    }
}

/// The index list of a primitive, or the implicit `0..count` when it has none.
fn slot_list(model: &Model, primitive: &Primitive) -> Option<Vec<u32>> {
    if let Some(handle) = primitive.indices {
        let indices = array(model, handle)?;
        let mut slots = Vec::with_capacity(indices.count);
        for position in 0..indices.count {
            slots.push(component(&indices, position, 0).max(0.0) as u32);
        }
        Some(slots)
    } else {
        let count = primitive
            .attributes
            .iter()
            .filter_map(|handle| model.accessors.attribute(*handle))
            .map(|attribute| attribute.count)
            .max()
            .unwrap_or(0);
        Some((0..count as u32).collect())
    }
}

/// Smooth normals for a primitive that has none, from its own triangles.
fn computed_normals(out: &mut Vec<f32>, local: &[[f32; 3]], slots: &[u32]) {
    let base = out.len() / 3;
    out.resize(out.len() + local.len() * 3, 0.0);
    if local.is_empty() {
        return;
    }
    for triangle in slots.chunks(3) {
        if triangle.len() < 3 {
            break;
        }
        let corners = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let a = local[corners[0]];
        let b = local[corners[1]];
        let c = local[corners[2]];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let face = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        for corner in corners {
            let slot = (base + corner) * 3;
            out[slot] += face[0];
            out[slot + 1] += face[1];
            out[slot + 2] += face[2];
        }
    }
    for corner in 0..local.len() {
        let slot = (base + corner) * 3;
        let turned = normalize(&[out[slot], out[slot + 1], out[slot + 2]]);
        out[slot] = turned[0];
        out[slot + 1] = turned[1];
        out[slot + 2] = turned[2];
    }
}

/// A resolved attribute: its bytes and how to read an element out of them.
struct Reader<'a> {
    bytes: &'a [u8],
    count: usize,
    element: usize,
    item: usize,
    component_type: ComponentType,
}

impl Reader<'_> {
    /// The three components of one element.
    fn point(&self, element: usize) -> [f32; 3] {
        [
            component(self, element, 0),
            component(self, element, 1),
            component(self, element, 2),
        ]
    }
}

/// One component of one element; anything out of range reads as zero.
fn component(reader: &Reader<'_>, element: usize, part: usize) -> f32 {
    let start = element * reader.element + part * reader.item;
    let end = start + reader.item;
    if element >= reader.count || end > reader.bytes.len() {
        return 0.0;
    }
    read_component(reader.component_type, &reader.bytes[start..end]).unwrap_or(0.0) as f32
}

/// Resolve an attribute of a primitive by name, case insensitive.
fn array_of<'a>(model: &'a Model, primitive: &Primitive, name: &str) -> Option<Reader<'a>> {
    let handle = primitive
        .attributes
        .iter()
        .copied()
        .find(|handle| named(model, *handle, name))?;
    array(model, handle)
}

fn array(model: &Model, handle: usize) -> Option<Reader<'_>> {
    let attribute = model.accessors.attribute(handle)?;
    let bytes = model.accessors.attribute_bytes(handle)?;
    Some(Reader {
        bytes,
        count: attribute.count,
        element: attribute.element_byte_len(),
        item: attribute.component_type.byte_len(),
        component_type: attribute.component_type,
    })
}

/// `true` if the attribute handle names this attribute.
fn named(model: &Model, handle: usize, name: &str) -> bool {
    model
        .accessors
        .attribute(handle)
        .is_some_and(|attribute| attribute.name.eq_ignore_ascii_case(name))
}

fn normalize(v: &[f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < f32::EPSILON {
        return [0.0, 0.0, 0.0];
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

/// A world transform: a 3x3 linear part and a translation, composed exactly as
/// a scene graph composes them — the parent first, then the node's own
/// `translation * rotation * scale`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Matrix {
    linear: [[f32; 3]; 3],
    offset: [f32; 3],
}

impl Matrix {
    const IDENTITY: Matrix = Matrix {
        linear: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        offset: [0.0; 3],
    };

    /// `self` followed by a node's own transform.
    fn then(&self, next: &Transform) -> Matrix {
        let linear = multiply(self.linear, scaled(rotation(&next.rotation), &next.scale));
        Matrix {
            linear,
            offset: add(&self.vector(next.translation), &self.offset),
        }
    }

    fn point(&self, p: [f32; 3]) -> [f32; 3] {
        add(&self.vector(p), &self.offset)
    }

    fn vector(&self, v: [f32; 3]) -> [f32; 3] {
        [
            self.linear[0][0] * v[0] + self.linear[0][1] * v[1] + self.linear[0][2] * v[2],
            self.linear[1][0] * v[0] + self.linear[1][1] * v[1] + self.linear[1][2] * v[2],
            self.linear[2][0] * v[0] + self.linear[2][1] * v[1] + self.linear[2][2] * v[2],
        ]
    }

    /// The matrix normals have to use: the transpose of the inverse, so a
    /// non-uniformly scaled part keeps its normals perpendicular to its
    /// surface.
    fn normals(&self) -> Matrix {
        let m = self.linear;
        // The cofactor matrix is the transpose of the adjugate, which is what
        // an inverse transpose needs.
        let cofactor = [
            [
                det2(m[1][1], m[1][2], m[2][1], m[2][2]),
                -det2(m[1][0], m[1][2], m[2][0], m[2][2]),
                det2(m[1][0], m[1][1], m[2][0], m[2][1]),
            ],
            [
                -det2(m[0][1], m[0][2], m[2][1], m[2][2]),
                det2(m[0][0], m[0][2], m[2][0], m[2][2]),
                -det2(m[0][0], m[0][1], m[2][0], m[2][1]),
            ],
            [
                det2(m[0][1], m[0][2], m[1][1], m[1][2]),
                -det2(m[0][0], m[0][2], m[1][0], m[1][2]),
                det2(m[0][0], m[0][1], m[1][0], m[1][1]),
            ],
        ];
        let determinant =
            m[0][0] * cofactor[0][0] + m[0][1] * cofactor[0][1] + m[0][2] * cofactor[0][2];
        if determinant.abs() < f32::EPSILON {
            // A degenerate part: keep the linear part rather than throw the
            // normals away.
            return *self;
        }
        let inverse = 1.0 / determinant;
        Matrix {
            linear: cofactor.map(|row| row.map(|value| value * inverse)),
            offset: [0.0; 3],
        }
    }
}

fn det2(a: f32, b: f32, c: f32, d: f32) -> f32 {
    a * d - b * c
}

fn multiply(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, value) in out_row.iter_mut().enumerate() {
            *value = a[row][0] * b[0][column] + a[row][1] * b[1][column] + a[row][2] * b[2][column];
        }
    }
    out
}

fn scaled(mut m: [[f32; 3]; 3], scale: &[f32; 3]) -> [[f32; 3]; 3] {
    for (column, factor) in scale.iter().enumerate() {
        for row in m.iter_mut() {
            row[column] *= factor;
        }
    }
    m
}

/// A quaternion (`x, y, z, w`) as a rotation matrix, the inverse of the
/// `to_quaternion` of the JT reader.
fn rotation(q: &[f32; 4]) -> [[f32; 3]; 3] {
    let [x, y, z, w] = *q;
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

fn add(a: &[f32; 3], b: &[f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Material;

    /// One triangle: a right triangle with legs 2 and 1 in the `z=0` plane.
    fn model(with_normals: bool) -> Model {
        let mut model = Model::default();
        let positions = model.accessors.push_attribute(Attribute {
            name: "POSITION".to_string(),
            buffer: 0,
            offset: 0,
            count: 3,
            components: 3,
            component_type: ComponentType::F32,
            normalized: false,
            min: None,
            max: None,
        });
        let mut attributes = vec![positions];
        if with_normals {
            attributes.push(model.accessors.push_attribute(Attribute {
                name: "NORMAL".to_string(),
                buffer: 0,
                offset: 36,
                count: 3,
                components: 3,
                component_type: ComponentType::F32,
                normalized: false,
                min: None,
                max: None,
            }));
        }
        let indices = model.accessors.push_attribute(Attribute {
            name: "indices".to_string(),
            buffer: 0,
            offset: if with_normals { 72 } else { 36 },
            count: 3,
            components: 1,
            component_type: ComponentType::U32,
            normalized: false,
            min: None,
            max: None,
        });
        let mut bytes: Vec<u8> = Vec::new();
        for point in [[0.0f32, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for value in point {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        if with_normals {
            for _ in 0..3 {
                for value in [0.0f32, 0.0, 1.0] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        for index in 0..3u32 {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        model.accessors.buffers = vec![BufferBlock::new(bytes)];
        model.meshes.push(Mesh {
            name: Some("unit".to_string()),
            primitives: vec![Primitive {
                attributes,
                indices: Some(indices),
                topology: Topology::TriangleList,
                material: None,
                bounds: None,
                extras: None,
            }],
            bounds: None,
            lod_level: Some(0),
            extras: None,
        });
        model.nodes.push(Node {
            name: Some("part".to_string()),
            instances: vec![Instance {
                mesh: 0,
                material: None,
                lod_bias: 0.0,
            }],
            ..Node::default()
        });
        model.scenes.push(Scene {
            name: None,
            roots: vec![0],
        });
        model
    }

    #[test]
    fn an_identity_model_merges_into_itself() {
        let flat = Flattened::of(&model(true));
        assert_eq!(flat.vertex_count(), 3);
        assert_eq!(flat.triangle_count(), 1);
        assert_eq!(flat.groups.len(), 1);
        assert_eq!(
            flat.bounds,
            Some(Aabb::new([0.0, 0.0, 0.0], [2.0, 1.0, 0.0]))
        );
    }

    #[test]
    fn a_model_without_a_single_normal_keeps_the_channel_empty() {
        let flat = Flattened::of(&model(false));
        assert!(flat.normals.is_empty());
        assert_eq!(flat.triangle_count(), 1);
    }

    #[test]
    fn an_instance_is_copied_to_every_placement() {
        let mut model = model(true);
        model.nodes.push(Node {
            name: Some("instance".to_string()),
            transform: Transform::translated([0.0, 0.0, 5.0]),
            instances: vec![Instance {
                mesh: 0,
                material: None,
                lod_bias: 0.0,
            }],
            ..Node::default()
        });
        model.scenes[0].roots = vec![0, 1];
        let flat = Flattened::of(&model);
        assert_eq!(flat.vertex_count(), 6);
        assert_eq!(flat.triangle_count(), 2);
        assert_eq!(
            flat.bounds,
            Some(Aabb::new([0.0, 0.0, 0.0], [2.0, 1.0, 5.0]))
        );
    }

    #[test]
    fn a_shared_subtree_is_walked_once_per_path() {
        // The plate case: one screw, reached through two placed groups.
        let mut model = model(true);
        model.nodes[0].instances.clear();
        let target = model.push_node(Node {
            name: Some("screw".to_string()),
            instances: vec![Instance {
                mesh: 0,
                material: None,
                lod_bias: 0.0,
            }],
            ..Node::default()
        });
        let mut carriers = Vec::new();
        for offset in [0.0f32, 25.0] {
            carriers.push(model.push_node(Node {
                name: None,
                transform: Transform::translated([0.0, offset, 0.0]),
                children: vec![target],
                ..Node::default()
            }));
        }
        model.nodes[0].children = carriers;
        let flat = Flattened::of(&model);
        assert_eq!(flat.triangle_count(), 2);
        assert_eq!(
            flat.bounds,
            Some(Aabb::new([0.0, 0.0, 0.0], [2.0, 26.0, 0.0]))
        );
    }

    #[test]
    fn a_cycle_does_not_hang_the_walk() {
        let mut model = model(true);
        model.nodes[0].children = vec![0];
        let flat = Flattened::of(&model);
        assert_eq!(flat.triangle_count(), 1);
    }

    #[test]
    fn an_index_past_the_coordinates_reads_as_zero() {
        // The real-CAD case: the topology names vertex 7 of a three vertex
        // array. The triangle survives and its vertex sits where the reader
        // puts it, at the transformed origin.
        let mut model = model(true);
        let indices = model.accessors.push_attribute(Attribute {
            name: "indices".to_string(),
            buffer: 0,
            offset: 84,
            count: 3,
            components: 1,
            component_type: ComponentType::U32,
            normalized: false,
            min: None,
            max: None,
        });
        let mut bytes = model.accessors.buffers[0].bytes.clone();
        for index in [0u32, 1, 7] {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        model.accessors.buffers[0] = BufferBlock::new(bytes);
        model.meshes[0].primitives[0].indices = Some(indices);
        let flat = Flattened::of(&model);
        assert_eq!(flat.triangle_count(), 1);
        assert_eq!(flat.vertex_count(), 8);
        assert_eq!(flat.indices[2], 7);
    }

    #[test]
    fn a_rotation_of_the_parent_turns_the_geometry() {
        // A quarter turn about +Z moves (2,0,0) to (0,2,0).
        let mut model = model(true);
        model.nodes[0].transform = Transform {
            rotation: [
                0.0,
                0.0,
                (std::f32::consts::FRAC_PI_4).sin(),
                (std::f32::consts::FRAC_PI_4).cos(),
            ],
            ..Transform::IDENTITY
        };
        let flat = Flattened::of(&model);
        let bounds = flat.bounds.expect("a triangle has bounds");
        // (2,0,0) -> (0,2,0) and (0,1,0) -> (-1,0,0).
        assert!(
            (bounds.min[0] + 1.0).abs() < 1e-5,
            "min x was {}",
            bounds.min[0]
        );
        assert!(
            (bounds.max[1] - 2.0).abs() < 1e-5,
            "max y was {}",
            bounds.max[1]
        );
    }

    #[test]
    fn a_scale_of_the_parent_scales_the_translation_of_the_child() {
        // The inch to millimetre case: a parent at 1/25.4 above a child offset
        // by 25.4 units lands the child one unit out, not 25.4.
        let mut model = model(true);
        model.nodes[0].instances.clear();
        model.nodes[0].transform = Transform {
            scale: [1.0 / 25.4; 3],
            ..Transform::IDENTITY
        };
        let child = model.push_node(Node {
            name: None,
            transform: Transform::translated([25.4, 0.0, 0.0]),
            instances: vec![Instance {
                mesh: 0,
                material: None,
                lod_bias: 0.0,
            }],
            ..Node::default()
        });
        model.nodes[0].children = vec![child];
        let flat = Flattened::of(&model);
        let bounds = flat.bounds.expect("two triangles have bounds");
        // The child sits at 25.4 * 1/25.4 = 1 and its two units of geometry are
        // scaled the same way.
        let expected = 1.0 + 2.0 / 25.4;
        assert!(
            (bounds.max[0] - expected).abs() < 1e-4,
            "max x was {} instead of {expected}",
            bounds.max[0]
        );
    }

    #[test]
    fn a_flattened_model_is_one_node_and_one_mesh() {
        let mut source = model(true);
        source.source = Some("opening_protection_plate1_jt9.5.jt".to_string());
        let flat = source.flattened();
        assert_eq!(flat.nodes.len(), 1);
        assert_eq!(flat.meshes.len(), 1);
        assert_eq!(flat.meshes[0].primitives.len(), 1);
        assert_eq!(flat.scenes[0].roots, vec![0]);
        assert_eq!(flat.triangle_count(), 1);
        assert_eq!(
            flat.extras,
            Some(json!({"flattened": true, "flat_groups": 1}))
        );
        assert_eq!(
            flat.meshes[0].name.as_deref(),
            Some("opening_protection_plate1_jt9.5")
        );
        // The merged buffers are sliced per group, so the attribute bytes have
        // to read back as the merged coordinates.
        let position = flat.meshes[0].primitives[0].attributes[0];
        let bytes = flat
            .accessors
            .attribute_bytes(position)
            .expect("the positions survive");
        assert_eq!(bytes.len(), 3 * 3 * 4);
    }

    #[test]
    fn an_unshaded_run_drops_unused_materials() {
        let mut source = model(true);
        source.materials.push(Material::default());
        source.materials.push(Material::default());
        let flat = source.flattened();
        assert!(flat.materials.is_empty());
    }

    #[test]
    fn two_materials_make_two_groups_and_two_primitives() {
        let mut source = model(true);
        let plain = source.add_material(Material::default());
        let tinted = Material {
            name: Some("tinted".to_string()),
            ..Material::default()
        };
        let tinted = source.add_material(tinted);
        assert_ne!(plain, tinted);
        let another = source.meshes[0].primitives[0].clone();
        source.meshes[0].primitives.push(another);
        source.meshes[0].primitives[0].material = Some(plain);
        source.meshes[0].primitives[1].material = Some(tinted);

        let flat = Flattened::of(&source);
        assert_eq!(flat.groups.len(), 2);
        assert_eq!(flat.triangle_count(), 2);
        let model = flat.to_model(&source);
        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.meshes[0].primitives.len(), 2);
        assert_eq!(model.materials.len(), 2);
        assert_eq!(
            model.meshes[0].primitives[1].material,
            Some(1),
            "the material indices are renumbered"
        );
    }

    #[test]
    fn the_normal_matrix_undoes_a_non_uniform_scale() {
        // A part stretched in x: a normal tilted 45 degrees in the xy plane
        // stays perpendicular to the stretched surface.
        let matrix = Matrix::IDENTITY.then(&Transform {
            scale: [2.0, 1.0, 1.0],
            ..Transform::IDENTITY
        });
        let normals = matrix.normals();
        let turned = normalize(&normals.vector([1.0, 1.0, 0.0]));
        let expected = normalize(&[0.5, 1.0, 0.0]);
        for axis in 0..3 {
            assert!(
                (turned[axis] - expected[axis]).abs() < 1e-5,
                "axis {axis}: {} instead of {}",
                turned[axis],
                expected[axis]
            );
        }
    }
}
