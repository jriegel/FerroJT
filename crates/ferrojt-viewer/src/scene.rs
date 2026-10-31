//! Build a kiss3d scene graph from the neutral FerroJT model.
//!
//! The outliner and the selection work on *rows*: one row per attachment of a
//! model node in the scene graph. JT shares a subtree between instances (the
//! plate samples hold one screw and instantiate it twice), and kiss3d bakes the
//! parent transform into every child, so a single shared `SceneNode3d` would end
//! up with the transform of whichever parent was written last. Each attachment
//! therefore gets its own `SceneNode3d`; the `Rc<RefCell<GpuMesh3d>>` behind it
//! stays shared, so only the transform state is duplicated.
//!
//! World transforms are tracked here rather than read back from kiss3d: the
//! viewer keeps its own chain (`parent_pose * node_pose`,
//! `parent_scale * node_scale`) and derives the matrix the renderer applies,
//! `T * R * S`. That also gives picking a box to test without touching the
//! private vertex buffers of the GPU meshes.

use std::collections::HashSet;

use ferrojt::model::{Aabb, Material, Model, Transform};
use kiss3d::prelude::*;

use crate::meshes::Decoded;

/// Selected geometry is drawn in this colour.
const HIGHLIGHT: Color = Color::new(1.0, 0.55, 0.10, 1.0);

/// Where a node sits in the outliner.
#[derive(Clone, Debug)]
pub struct Row {
    /// Unique id of this row (a node attached twice has two rows).
    pub id: usize,
    /// Index into [`Model::nodes`].
    pub node: usize,
    /// Parent row id, `None` for a root.
    pub parent: Option<usize>,
    /// Nesting depth, used for the indentation.
    pub depth: usize,
    /// Display name.
    pub name: String,
    /// Source class of the node, e.g. `JtPart`.
    pub kind: String,
    /// `true` if the row has child rows.
    pub has_children: bool,
    /// Number of mesh instances attached at this node.
    pub instances: usize,
}

/// The 3D side of one outliner row.
struct Slot {
    /// Group node carrying the node transform.
    group: SceneNode3d,
    /// Nodes that actually hold geometry.
    drawables: Vec<SceneNode3d>,
    /// Colour each drawable was built with, so a highlight can be undone.
    colors: Vec<Color>,
    /// Rotation and translation of the whole chain.
    pose: Pose3,
    /// Scale of the whole chain, applied before the rotation.
    scale: Vec3,
    /// Local space bounds of the geometry attached at this row.
    local: Option<Aabb>,
}

/// A loaded model plus the kiss3d graph built from it.
pub struct ViewerScene {
    root: SceneNode3d,
    rows: Vec<Row>,
    slots: Vec<Slot>,
    child_rows: Vec<Vec<usize>>,
    collapsed: HashSet<usize>,
    visible: Vec<bool>,
    selected: Option<usize>,
}

impl Default for ViewerScene {
    fn default() -> Self {
        Self {
            root: SceneNode3d::empty(),
            rows: Vec::new(),
            slots: Vec::new(),
            child_rows: Vec::new(),
            collapsed: HashSet::new(),
            visible: Vec::new(),
            selected: None,
        }
    }
}

/// One entry of the graph walk: a model node plus where it hangs in the tree.
struct Pending {
    node: usize,
    parent: Option<usize>,
    depth: usize,
    pose: Pose3,
    scale: Vec3,
}

impl ViewerScene {
    /// Build the kiss3d graph of `model`.
    pub fn new(model: &Model, decoded: &[Vec<Decoded>]) -> Self {
        let mut scene = Self::default();

        let mut stack: Vec<Pending> = model
            .scenes
            .iter()
            .flat_map(|scene| scene.roots.iter().copied())
            .map(|root| Pending {
                node: root,
                parent: None,
                depth: 0,
                pose: Pose3::IDENTITY,
                scale: Vec3::ONE,
            })
            .collect();
        // Depth first, but keep the sibling order left to right.
        stack.reverse();

        // `path[d]` is the model node the current branch came through at depth
        // `d`; it turns a cyclic graph into a skipped edge instead of a loop.
        let mut path: Vec<usize> = Vec::new();

        while let Some(pending) = stack.pop() {
            let Some(model_node) = model.nodes.get(pending.node) else {
                continue;
            };
            path.truncate(pending.depth);
            if path.contains(&pending.node) {
                continue;
            }
            path.push(pending.node);

            let local = node_pose(model_node.transform);
            let local_scale = Vec3::from(model_node.transform.scale);
            // A transform is `T * R * S`, so a parent scale scales the child's
            // translation as well — `Pose3 * Pose3` alone would leave it alone
            // and blow up a branch whose parent converts units (the server
            // sample has nine nodes at 1/25.4).
            let pose =
                pending.pose * Pose3::from_parts(pending.scale * local.translation, local.rotation);
            let scale = pending.scale * local_scale;

            let id = scene.rows.len();
            scene.rows.push(Row {
                id,
                node: pending.node,
                parent: pending.parent,
                depth: pending.depth,
                name: model_node
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("<node {}>", pending.node)),
                kind: model_node.kind.clone().unwrap_or_default(),
                has_children: !model_node.children.is_empty(),
                instances: model_node.instances.len(),
            });
            scene.visible.push(true);
            scene.child_rows.push(Vec::new());
            if let Some(parent) = pending.parent {
                scene.child_rows[parent].push(id);
            }

            let slot = scene.build_slot(model, decoded, pending.node, pose, scale);
            let group = slot.group.clone();
            scene.slots.push(slot);
            scene.root.add_child(group);

            for child in model_node.children.iter().rev() {
                stack.push(Pending {
                    node: *child,
                    parent: Some(id),
                    depth: pending.depth + 1,
                    pose,
                    scale,
                });
            }
        }

        scene
    }

    /// The kiss3d subtree of one attachment.
    fn build_slot(
        &mut self,
        model: &Model,
        decoded: &[Vec<Decoded>],
        node: usize,
        pose: Pose3,
        scale: Vec3,
    ) -> Slot {
        let model_node = &model.nodes[node];
        let mut group = SceneNode3d::empty();
        group.set_pose(pose);
        // The scale lives on the geometry, not on the group: kiss3d keeps
        // `world_scale` apart from `world_transform` and never scales a child's
        // translation by it, so a scaled group would misplace its whole subtree.
        // With the group at unit scale its pose chain is a plain matrix chain.

        let mut drawables = Vec::new();
        let mut colors = Vec::new();
        let mut local = None;

        for instance in &model_node.instances {
            let Some(primitives) = decoded.get(instance.mesh) else {
                continue;
            };
            for primitive in primitives {
                let material = instance
                    .material
                    .or(primitive.material)
                    .and_then(|index| model.materials.get(index));
                let mut drawable = SceneNode3d::mesh(primitive.mesh.clone(), scale);
                let color = apply_material(&mut drawable, material);
                group.add_child(drawable.clone());
                drawables.push(drawable);
                colors.push(color);
                local = union(local, primitive.bounds);
            }
        }

        Slot {
            group,
            drawables,
            colors,
            pose,
            scale,
            local,
        }
    }

    /// The rows to render in the outliner, honouring the collapsed state.
    pub fn visible_rows(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            if !self.hidden_by_collapse(row.id) {
                out.push(row.id);
            }
        }
        out
    }

    fn hidden_by_collapse(&self, id: usize) -> bool {
        let mut parent = self.rows[id].parent;
        while let Some(p) = parent {
            if self.collapsed.contains(&p) {
                return true;
            }
            parent = self.rows[p].parent;
        }
        false
    }

    /// All rows, in depth first order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Look up a row by its id.
    pub fn row(&self, id: usize) -> Option<&Row> {
        self.rows.get(id)
    }

    /// Direct child rows of a row.
    pub fn child_rows(&self, id: usize) -> &[usize] {
        self.child_rows.get(id).map_or(&[], |v| v.as_slice())
    }

    /// `true` if the row has child rows.
    pub fn has_children(&self, id: usize) -> bool {
        self.child_rows.get(id).is_some_and(|v| !v.is_empty())
    }

    /// Open or close one branch of the tree.
    pub fn toggle_collapse(&mut self, id: usize) {
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
    }

    /// `true` if the branch is closed.
    pub fn is_collapsed(&self, id: usize) -> bool {
        self.collapsed.contains(&id)
    }

    /// Expand (`true`) or collapse every branch of the tree.
    pub fn set_rows_open(&mut self, open: bool) {
        self.collapsed.clear();
        if !open {
            for row in &self.rows {
                if row.has_children {
                    self.collapsed.insert(row.id);
                }
            }
        }
    }

    /// Toggle the visibility of a row and, through it, of its whole subtree.
    ///
    /// The kiss3d graph is flat — every row's group hangs off `root` with its
    /// world pose baked in, because kiss3d multiplies parent transforms into
    /// children and each attachment needs its own transform. So a hidden branch
    /// has to be pushed down the row tree explicitly; `root`'s children are
    /// siblings, not the outliner's subtree.
    pub fn set_visible(&mut self, id: usize, visible: bool) {
        if id >= self.slots.len() {
            return;
        }
        self.visible[id] = visible;
        self.apply_visibility(id);
    }

    /// Re-apply the effective visibility of `id` and everything below it.
    fn apply_visibility(&mut self, id: usize) {
        if let Some(slot) = self.slots.get(id) {
            let drawn = self.is_drawn(id);
            slot.group.clone().set_visible(drawn);
        }
        let children = self.child_rows.get(id).cloned().unwrap_or_default();
        for child in children {
            self.apply_visibility(child);
        }
    }

    /// `false` once a row was hidden with the eye toggle.
    pub fn is_visible(&self, id: usize) -> bool {
        self.visible.get(id).copied().unwrap_or(true)
    }

    /// `false` when a row or one of its ancestors is hidden.
    pub fn is_drawn(&self, id: usize) -> bool {
        let mut current = Some(id);
        while let Some(c) = current {
            if !self.is_visible(c) {
                return false;
            }
            current = self.rows[c].parent;
        }
        true
    }

    /// Highlight the geometry of the selected row.
    ///
    /// Selecting in the viewport has to be visible in the outliner too, so every
    /// closed branch above the row is opened before it becomes the selection.
    pub fn set_selected(&mut self, id: Option<usize>) {
        if self.selected == id {
            return;
        }
        if let Some(previous) = self.selected.take() {
            self.recolor(previous);
        }
        let Some(id) = id else {
            self.selected = None;
            return;
        };
        let mut parent = self.rows.get(id).and_then(|row| row.parent);
        while let Some(p) = parent {
            self.collapsed.remove(&p);
            parent = self.rows.get(p).and_then(|row| row.parent);
        }
        self.selected = Some(id);
        let Some(slot) = self.slots.get(id) else {
            return;
        };
        for drawable in slot.drawables.clone() {
            drawable.clone().set_color(HIGHLIGHT);
        }
    }

    /// The currently selected row.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Rename a row. This only changes the outliner label, never the file.
    pub fn set_name(&mut self, id: usize, name: &str) {
        if let Some(row) = self.rows.get_mut(id) {
            row.name = name.to_string();
        }
    }

    /// World space bounds of a row, used for picking and for the camera fit.
    pub fn world_bounds(&self, id: usize) -> Option<(Vec3, Vec3)> {
        let slot = self.slots.get(id)?;
        let bounds = slot.local?;
        if bounds.is_empty() {
            return None;
        }
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for corner in corners(bounds) {
            // The renderer applies `T * R * S`, so scale first.
            let point = slot.pose * (corner * slot.scale);
            min = min.min(point);
            max = max.max(point);
        }
        Some((min, max))
    }

    /// World space bounds of everything that is currently drawn.
    pub fn world_bounds_all(&self) -> Option<(Vec3, Vec3)> {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        let mut found = false;
        for row in &self.rows {
            if !self.is_drawn(row.id) {
                continue;
            }
            let entry = self.world_bounds(row.id);
            let Some((lo, hi)) = entry else {
                continue;
            };
            found = true;
            min = min.min(lo);
            max = max.max(hi);
        }
        if found { Some((min, max)) } else { None }
    }

    /// The kiss3d root to hand to the window.
    pub fn root_mut(&mut self) -> &mut SceneNode3d {
        &mut self.root
    }

    /// Number of outliner rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn recolor(&mut self, id: usize) {
        let Some(slot) = self.slots.get(id) else {
            return;
        };
        let drawables = slot.drawables.clone();
        let colors = slot.colors.clone();
        for (mut drawable, color) in drawables.into_iter().zip(colors) {
            drawable.set_color(color);
        }
    }
}

/// Colour the geometry ends up with, so the highlight can be undone.
fn apply_material(node: &mut SceneNode3d, material: Option<&Material>) -> Color {
    let fallback = Color::new(0.8, 0.8, 0.8, 1.0);
    let Some(material) = material else {
        return fallback;
    };
    let base = material.base_color.factor;
    let color = Color::new(base[0], base[1], base[2], base[3]);
    node.set_color(color);
    node.set_metallic(material.metallic.factor[0]);
    node.set_roughness(material.roughness.factor[0]);
    let emissive = material.emissive.factor;
    node.set_emissive(Color::new(emissive[0], emissive[1], emissive[2], 1.0));
    // JT faces are double sided, so never cull.
    node.enable_backface_culling(false);
    color
}

/// The local pose of a model node.
fn node_pose(transform: Transform) -> Pose3 {
    let rotation = Quat::from_xyzw(
        transform.rotation[0],
        transform.rotation[1],
        transform.rotation[2],
        transform.rotation[3],
    )
    .normalize();
    Pose3::from_parts(Vec3::from(transform.translation), rotation)
}

/// Union of two boxes.
fn union(a: Option<Aabb>, b: Option<Aabb>) -> Option<Aabb> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// The eight corners of a box.
fn corners(bounds: Aabb) -> [Vec3; 8] {
    let min = Vec3::from(bounds.min);
    let max = Vec3::from(bounds.max);
    [
        min,
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, max.y, max.z),
        max,
    ]
}
