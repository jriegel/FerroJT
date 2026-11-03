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

/// One level-of-detail branch of the scene graph.
///
/// A range LOD node lists the same part at several tiers, finest first, and a
/// model loaded with
/// [`JtFile::to_model_with_all_lods`](ferrojt::jt::JtFile::to_model_with_all_lods)
/// keeps every one of them. Each tier is a row of its own — the file makes it
/// one — so switching a level of detail means showing one branch of this group
/// and hiding the others.
#[derive(Clone, Debug, Default)]
pub struct LodGroup {
    /// The range LOD row the tiers hang under.
    pub parent: usize,
    /// The tier rows, from the finest tier to the coarsest one.
    pub tiers: Vec<usize>,
    /// Index into `tiers` of the tier currently drawn.
    pub tier: usize,
    /// World space box of the stack, the same for every tier.
    bounds: Option<(Vec3, Vec3)>,
    /// Triangles each tier draws, indexed like `tiers`.
    triangles: Vec<usize>,
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
    /// Triangles attached at this row, used for the live count in the toolbar.
    triangles: usize,
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
    /// The level-of-detail stacks of the file, in graph order.
    lod_groups: Vec<LodGroup>,
    /// Tier rows currently switched off by the level-of-detail selection.
    lod_hidden: HashSet<usize>,
    /// `true` while the tier of a group follows its size on screen.
    auto_lod: bool,
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
            lod_groups: Vec::new(),
            lod_hidden: HashSet::new(),
            auto_lod: true,
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

        scene.detect_lods();
        scene
    }

    /// Collect the level-of-detail stacks of the graph, measure them, and make
    /// only the selected tier of each one visible.
    fn detect_lods(&mut self) {
        self.lod_groups = detect_stacks(&self.rows, &self.child_rows);
        self.measure_lods();
        // Only the selected tier of a stack may be drawn: the tiers describe
        // the same part, so showing two of them at once is Z-fighting.
        self.apply_lod_state();
    }

    /// Measure each stack: its box and what each tier costs to draw.
    ///
    /// Both are static — the world poses never change — so this runs once per
    /// load and the per-frame selection only has to project a box.
    fn measure_lods(&mut self) {
        let boxes: Vec<Option<(Vec3, Vec3)>> = (0..self.rows.len())
            .map(|id| self.world_bounds(id))
            .collect();
        let counts: Vec<usize> = self.slots.iter().map(|slot| slot.triangles).collect();
        measure_stacks(&mut self.lod_groups, &self.child_rows, &counts, &boxes);
    }

    /// The world space box of a stack.
    pub fn lod_bounds(&self, group: usize) -> Option<(Vec3, Vec3)> {
        self.lod_groups.get(group)?.bounds
    }

    /// Triangles each tier of a stack draws, finest tier first.
    pub fn tier_triangles(&self, group: usize) -> &[usize] {
        self.lod_groups
            .get(group)
            .map_or(&[], |entry| &entry.triangles)
    }

    /// The level-of-detail stack whose range LOD row is `id`.
    ///
    /// Used by the outliner to badge the range LOD with the tier it currently
    /// draws.
    pub fn lod_stack_at(&self, id: usize) -> Option<&LodGroup> {
        self.lod_groups.iter().find(|entry| entry.parent == id)
    }

    /// Triangles currently on screen, counting only the selected tier of each
    /// stack.
    pub fn drawn_triangle_count(&self) -> usize {
        let counts: Vec<usize> = self.slots.iter().map(|slot| slot.triangles).collect();
        drawn_triangle_count(&self.rows, &self.visible, &self.lod_hidden, &counts)
    }

    /// Triangles the file would draw with every tier of every stack visible.
    ///
    /// Counted per attachment, like [`ViewerScene::drawn_triangle_count`], so
    /// the two are comparable: an instantiated part counts once per instance in
    /// both. That is what the renderer rasterises, which is not the same as the
    /// number of unique meshes in the file.
    pub fn total_triangle_count(&self) -> usize {
        self.slots.iter().map(|slot| slot.triangles).sum()
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
        let mut triangles = 0usize;

        for instance in &model_node.instances {
            let Some(primitives) = decoded.get(instance.mesh) else {
                continue;
            };
            for primitive in primitives {
                triangles += primitive.mesh.borrow().num_indices() as usize / 3;
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
            triangles,
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

    /// Close every branch at `level` or deeper, leaving the levels above it open.
    ///
    /// A freshly loaded file calls this with `1`: the roots and their children are
    /// on screen, everything below them is one click away. Opening all 4489 rows
    /// of `SampleJT_Server.jt` shows nothing but a scrollbar, and closing all but
    /// the roots hides the parts the outliner exists to reach.
    pub fn collapse_below(&mut self, level: usize) {
        self.collapsed = collapsed_below(&self.rows, &self.child_rows, level);
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

    /// `false` when a row or one of its ancestors is hidden, or when the row is
    /// a tier of a level-of-detail stack that is not the selected one.
    pub fn is_drawn(&self, id: usize) -> bool {
        row_drawn(id, &self.rows, &self.visible, &self.lod_hidden)
    }

    /// Triangles attached at a row, not counting its children.
    ///
    /// The detail pane reports this per attachment, the same basis
    /// [`ViewerScene::total_triangle_count`] uses, so the two add up.
    pub fn row_triangles(&self, id: usize) -> usize {
        self.slots.get(id).map_or(0, |slot| slot.triangles)
    }

    /// The level-of-detail stacks of the scene, finest tier first.
    pub fn lod_groups(&self) -> &[LodGroup] {
        &self.lod_groups
    }

    /// `true` while the tier of a stack follows its size on screen.
    pub fn auto_lod(&self) -> bool {
        self.auto_lod
    }

    /// Turn screen-size driven levels of detail on or off. With them off, every
    /// stack draws its finest tier.
    pub fn set_auto_lod(&mut self, enabled: bool) {
        if self.auto_lod == enabled {
            return;
        }
        self.auto_lod = enabled;
        for group in &mut self.lod_groups {
            group.tier = 0;
        }
        self.apply_lod_state();
    }

    /// The tier index currently drawn for a stack.
    pub fn lod_tier(&self, group: usize) -> Option<usize> {
        self.lod_groups.get(group).map(|entry| entry.tier)
    }

    /// Show tier `tier` of a stack, clamped to what the file declares.
    pub fn set_lod_tier(&mut self, group: usize, tier: usize) {
        let Some(entry) = self.lod_groups.get_mut(group) else {
            return;
        };
        let tier = clamp_tier(entry.tiers.len(), tier);
        if entry.tier == tier {
            return;
        }
        entry.tier = tier;
        self.apply_lod_state();
    }

    /// Push the selected tier of every stack into the kiss3d graph.
    ///
    /// The tiers are ordinary rows, so the eye toggle of the outliner still
    /// applies on top of this: a tier is drawn when it is the selected one of
    /// its stack and nothing above it is hidden.
    fn apply_lod_state(&mut self) {
        self.lod_hidden = hidden_tiers(&self.lod_groups);
        for id in 0..self.slots.len() {
            let drawn = self.is_drawn(id);
            self.slots[id].group.clone().set_visible(drawn);
        }
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
        for corner in box_corners(Vec3::from(bounds.min), Vec3::from(bounds.max)) {
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

/// Union of two world space boxes.
fn union_box(a: Option<(Vec3, Vec3)>, b: Option<(Vec3, Vec3)>) -> Option<(Vec3, Vec3)> {
    match (a, b) {
        (Some((a_min, a_max)), Some((b_min, b_max))) => Some((a_min.min(b_min), a_max.max(b_max))),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// The level-of-detail stacks of a row tree, in graph order.
///
/// The reader numbers the tiers of a range LOD by the position of its
/// children, so the tiers are exactly the children of a `RangeLOD` row, in
/// order: a `Group` per tier in the plate samples, the bare `Shape` of a tier
/// in the NIST parts. A range LOD with a single tier — which is what
/// `SampleJT_Server.jt` declares for all 584 of them — is not a stack and is
/// left alone.
fn detect_stacks(rows: &[Row], child_rows: &[Vec<usize>]) -> Vec<LodGroup> {
    let mut stacks = Vec::new();
    for (id, row) in rows.iter().enumerate() {
        if row.kind != "RangeLOD" {
            continue;
        }
        let tiers: Vec<usize> = children_of(child_rows, id).to_vec();
        if tiers.len() < 2 {
            continue;
        }
        stacks.push(LodGroup {
            parent: id,
            tiers,
            ..LodGroup::default()
        });
    }
    stacks
}

/// Measure the box and the per-tier cost of every stack.
///
/// A tier's cost is the geometry of its whole subtree, because the shapes of a
/// tier hang below its group row, not on the row itself. Where a stack nests in
/// another one, only the tier of the inner stack that is on screen is counted.
fn measure_stacks(
    groups: &mut [LodGroup],
    child_rows: &[Vec<usize>],
    counts: &[usize],
    boxes: &[Option<(Vec3, Vec3)>],
) {
    // Which stack a tier row belongs to, so a nested stack is not counted with
    // all of its tiers at once.
    let mut owner = vec![None; counts.len()];
    for (group, entry) in groups.iter().enumerate() {
        for &row in &entry.tiers {
            owner[row] = Some(group);
        }
    }

    let mut measured = Vec::with_capacity(groups.len());
    for group in 0..groups.len() {
        let mut triangles = Vec::new();
        let mut bounds: Option<(Vec3, Vec3)> = None;
        for index in 0..groups[group].tiers.len() {
            let root = groups[group].tiers[index];
            let (count, box_) =
                subtree_cost(root, group, &owner, groups, counts, child_rows, boxes);
            triangles.push(count);
            bounds = union_box(bounds, box_);
        }
        measured.push((triangles, bounds));
    }
    for (group, (triangles, bounds)) in measured.into_iter().enumerate() {
        groups[group].triangles = triangles;
        groups[group].bounds = bounds;
    }
}

/// The tier rows the current selection switches off.
fn hidden_tiers(groups: &[LodGroup]) -> HashSet<usize> {
    let mut hidden = HashSet::new();
    for entry in groups {
        for (index, &row) in entry.tiers.iter().enumerate() {
            if index != entry.tier {
                hidden.insert(row);
            }
        }
    }
    hidden
}

/// A tier index clamped to what a stack of `depth` tiers declares, so an
/// out-of-range request selects the coarsest tier instead of hiding the part.
fn clamp_tier(depth: usize, tier: usize) -> usize {
    tier.min(depth.saturating_sub(1))
}

/// `true` when `id` reaches the screen: nothing above it is hidden and, if it is
/// a tier of a stack or hangs below one, it is the tier that stack draws.
///
/// The kiss3d graph is flat, so switching a tier off does not hide the rows
/// below it by itself — the whole ancestor chain has to be checked.
fn row_drawn(id: usize, rows: &[Row], visible: &[bool], hidden: &HashSet<usize>) -> bool {
    let mut current = Some(id);
    while let Some(c) = current {
        if hidden.contains(&c) || !visible.get(c).copied().unwrap_or(true) {
            return false;
        }
        current = rows.get(c).and_then(|row| row.parent);
    }
    true
}

/// Triangles on screen: the rows that are drawn, at their own cost.
fn drawn_triangle_count(
    rows: &[Row],
    visible: &[bool],
    hidden: &HashSet<usize>,
    counts: &[usize],
) -> usize {
    rows.iter()
        .filter(|row| row_drawn(row.id, rows, visible, hidden))
        .map(|row| counts.get(row.id).copied().unwrap_or(0))
        .sum()
}

/// Direct child rows of `id` in a row tree.
fn children_of(child_rows: &[Vec<usize>], id: usize) -> &[usize] {
    child_rows.get(id).map_or(&[], |v| v.as_slice())
}

/// The branches to close so that only the levels up to `level` are open.
///
/// A row is closed when it sits at `level` or deeper *and* has child rows —
/// closing a leaf would be a state the disclosure triangle cannot show. The
/// child list, not [`Row::has_children`], decides: that flag comes from the
/// model graph, and a child skipped by the cycle guard is not a row.
fn collapsed_below(rows: &[Row], child_rows: &[Vec<usize>], level: usize) -> HashSet<usize> {
    rows.iter()
        .filter(|row| row.depth >= level && child_rows.get(row.id).is_some_and(|v| !v.is_empty()))
        .map(|row| row.id)
        .collect()
}

/// What drawing the subtree below `row` costs: triangles and world box.
///
/// `owner[row]` is the stack a tier row belongs to. A row that is a tier of a
/// *different* stack is only followed when it is that stack's selected tier —
/// otherwise its branch is off screen and would both inflate the cost and
/// enlarge the box of `group`.
fn subtree_cost(
    row: usize,
    group: usize,
    owner: &[Option<usize>],
    groups: &[LodGroup],
    counts: &[usize],
    child_rows: &[Vec<usize>],
    boxes: &[Option<(Vec3, Vec3)>],
) -> (usize, Option<(Vec3, Vec3)>) {
    let mut triangles = 0usize;
    let mut bounds: Option<(Vec3, Vec3)> = None;
    let mut stack = vec![row];
    while let Some(id) = stack.pop() {
        if let Some(other) = owner[id] {
            if other != group
                && groups[other].tiers.iter().position(|&t| t == row) != Some(groups[other].tier)
            {
                continue;
            }
        }
        triangles += counts[id];
        bounds = union_box(bounds, boxes[id]);
        stack.extend(children_of(child_rows, id).iter().copied());
    }
    (triangles, bounds)
}

/// The eight corners of an axis aligned box.
pub(crate) fn box_corners(min: Vec3, max: Vec3) -> [Vec3; 8] {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A row tree of one `RangeLOD` with `tiers` `Group` children, each holding
    /// one shape. The finest tier carries the most triangles and the smallest
    /// box, which is what the samples do.
    ///
    /// Built as rows rather than as a model: the row tree is what the level-of
    /// detail logic works on, and a `ViewerScene` needs a kiss3d window context
    /// to build its GPU nodes.
    fn stacked_rows(tiers: usize) -> (Vec<Row>, Vec<Vec<usize>>) {
        let mut rows = vec![Row {
            id: 0,
            node: 0,
            parent: None,
            depth: 0,
            name: "range lod".to_string(),
            kind: "RangeLOD".to_string(),
            has_children: true,
            instances: 0,
        }];
        let mut child_rows = vec![Vec::new()];
        for tier in 0..tiers {
            let group = rows.len();
            let shape = group + 1;
            rows.push(Row {
                id: group,
                node: group,
                parent: Some(0),
                depth: 1,
                name: format!("tier {tier}"),
                kind: "Group".to_string(),
                has_children: true,
                instances: 0,
            });
            rows.push(Row {
                id: shape,
                node: shape,
                parent: Some(group),
                depth: 2,
                name: format!("shape {tier}"),
                kind: "Shape".to_string(),
                has_children: false,
                instances: 1,
            });
            child_rows.push(vec![shape]);
            child_rows.push(Vec::new());
            child_rows[0].push(group);
        }
        (rows, child_rows)
    }

    /// Triangles per row and world box per row of [`stacked_rows`].
    fn costs(rows: &[Row], tiers: usize) -> (Vec<usize>, Vec<Option<(Vec3, Vec3)>>) {
        let mut counts = vec![0usize; rows.len()];
        let mut boxes = vec![None; rows.len()];
        for tier in 0..tiers {
            let shape = 2 + 2 * tier;
            counts[shape] = 3 * (tiers - tier);
            boxes[shape] = Some((Vec3::ZERO, Vec3::new(1.0 + tier as f32, 1.0, 1.0)));
        }
        (counts, boxes)
    }

    /// A stack is only a stack when a range LOD has more than one tier: the
    /// server sample declares 584 range LODs with a single group each, and
    /// those have to stay untouched.
    #[test]
    fn a_single_tier_range_lod_is_not_a_stack() {
        let (rows, child_rows) = stacked_rows(1);
        assert!(detect_stacks(&rows, &child_rows).is_empty());
    }

    /// The NIST parts list the shape of a tier directly under the range LOD,
    /// without a group around it; that shape is the tier.
    #[test]
    fn a_bare_shape_is_a_tier_too() {
        let (mut rows, mut child_rows) = stacked_rows(2);
        let shape = rows.len();
        rows.push(Row {
            id: shape,
            node: shape,
            parent: Some(0),
            depth: 1,
            name: "bare shape".to_string(),
            kind: "Shape".to_string(),
            has_children: false,
            instances: 1,
        });
        child_rows.push(Vec::new());
        child_rows[0].insert(0, shape);
        let stacks = detect_stacks(&rows, &child_rows);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].tiers, [shape, 1, 3]);
    }

    #[test]
    fn every_tier_of_a_stack_is_found_in_order() {
        let (rows, child_rows) = stacked_rows(3);
        let stacks = detect_stacks(&rows, &child_rows);
        assert_eq!(stacks.len(), 1);
        let stack = &stacks[0];
        assert_eq!(stack.parent, 0);
        assert_eq!(stack.tiers.len(), 3);
        // The tiers are the group rows, in the order the file lists them.
        for (index, &row) in stack.tiers.iter().enumerate() {
            assert_eq!(rows[row].kind, "Group");
            assert_eq!(rows[row].name, format!("tier {index}"));
        }
    }

    /// Showing two tiers of one stack at once is the Z-fighting this avoids, so
    /// exactly one tier of each stack may be drawn, and the count follows it.
    #[test]
    fn only_the_selected_tier_is_drawn() {
        let (rows, child_rows) = stacked_rows(3);
        let mut stacks = detect_stacks(&rows, &child_rows);
        let (counts, boxes) = costs(&rows, 3);
        measure_stacks(&mut stacks, &child_rows, &counts, &boxes);
        let visible = vec![true; rows.len()];
        let tiers = stacks[0].tiers.clone();

        for (tier, &tier_row) in tiers.iter().enumerate() {
            stacks[0].tier = tier;
            let hidden = hidden_tiers(&stacks);
            let drawn: Vec<usize> = rows
                .iter()
                .filter(|row| row_drawn(row.id, &rows, &visible, &hidden))
                .map(|row| row.id)
                .collect();
            assert_eq!(
                drawn,
                vec![0, tier_row, tier_row + 1],
                "tier {tier} should be the only one drawn"
            );
            assert_eq!(
                drawn_triangle_count(&rows, &visible, &hidden, &counts),
                3 * (3 - tier)
            );
        }
    }

    /// The measured cost of a tier is the geometry of its whole subtree, and
    /// the box of a stack is the union of its tiers — the shapes of a tier hang
    /// below its group row, not on the row itself.
    #[test]
    fn a_tier_costs_its_whole_subtree() {
        let (rows, child_rows) = stacked_rows(3);
        let mut stacks = detect_stacks(&rows, &child_rows);
        let (counts, boxes) = costs(&rows, 3);
        measure_stacks(&mut stacks, &child_rows, &counts, &boxes);
        assert_eq!(stacks[0].triangles, [9, 6, 3]);
        let (min, max) = stacks[0].bounds.expect("the stack has geometry");
        assert_eq!(min, Vec3::ZERO);
        assert_eq!(max, Vec3::new(3.0, 1.0, 1.0));
    }

    /// An out-of-range tier must not hide the part: the coarsest tier wins.
    #[test]
    fn an_out_of_range_tier_selects_the_coarsest_one() {
        assert_eq!(clamp_tier(3, 0), 0);
        assert_eq!(clamp_tier(3, 2), 2);
        assert_eq!(clamp_tier(3, 3), 2);
        assert_eq!(clamp_tier(3, usize::MAX), 2);
        assert_eq!(clamp_tier(1, usize::MAX), 0);
        assert_eq!(clamp_tier(0, usize::MAX), 0);
    }

    /// The toolbar reports the on-screen count against the count of the file,
    /// so the two have to be counted the same way — per attachment, not per
    /// unique mesh — and the first can never exceed the second.
    #[test]
    fn the_drawn_count_never_exceeds_the_total() {
        let (rows, child_rows) = stacked_rows(3);
        let mut stacks = detect_stacks(&rows, &child_rows);
        let (counts, boxes) = costs(&rows, 3);
        measure_stacks(&mut stacks, &child_rows, &counts, &boxes);
        let visible = vec![true; rows.len()];
        let total: usize = counts.iter().sum();
        assert_eq!(total, 18);

        for tier in 0..stacks[0].tiers.len() {
            stacks[0].tier = tier;
            let drawn = drawn_triangle_count(&rows, &visible, &hidden_tiers(&stacks), &counts);
            assert!(drawn <= total, "tier {tier}: {drawn} > {total}");
        }
        // Every tier of a stack is the same part, so the tiers add up to the
        // total: with all of them visible the whole file is on screen.
        let nothing_hidden = HashSet::new();
        assert_eq!(
            drawn_triangle_count(&rows, &visible, &nothing_hidden, &counts),
            total
        );
    }

    /// The eye toggle of the outliner still applies on top of the level of
    /// detail, and it hides a whole branch.
    #[test]
    fn hiding_a_branch_hides_its_geometry() {
        let (rows, child_rows) = stacked_rows(2);
        let stacks = detect_stacks(&rows, &child_rows);
        let hidden = hidden_tiers(&stacks);
        let (counts, _) = costs(&rows, 2);

        let visible = vec![true; rows.len()];
        assert_eq!(drawn_triangle_count(&rows, &visible, &hidden, &counts), 6);

        // Closing the eye of the group row of the drawn tier takes it off screen.
        let mut visible = vec![true; rows.len()];
        visible[stacks[0].tiers[0]] = false;
        assert!(!row_drawn(stacks[0].tiers[0] + 1, &rows, &visible, &hidden));
        assert_eq!(drawn_triangle_count(&rows, &visible, &hidden, &counts), 0);
        // The child rows of a stack are still walked for the eye toggle.
        assert_eq!(child_rows[stacks[0].parent], stacks[0].tiers);
    }

    /// A loaded file opens its first two levels: the roots at depth 0 and their
    /// children at depth 1 stay visible, and every branch at depth 1 or deeper
    /// is closed. Leaves are never in the set — there is no triangle to open one.
    #[test]
    fn only_the_top_levels_stay_open() {
        let (rows, child_rows) = stacked_rows(2);
        // 0: range LOD (depth 0) · 1, 3: tier groups (depth 1) · 2, 4: shapes.
        assert_eq!(
            collapsed_below(&rows, &child_rows, 1),
            [1, 3].into_iter().collect::<HashSet<usize>>()
        );
        // One level only: the roots alone.
        assert_eq!(
            collapsed_below(&rows, &child_rows, 0),
            [0, 1, 3].into_iter().collect::<HashSet<usize>>()
        );
        // Deeper than the tree: nothing is closed.
        assert!(collapsed_below(&rows, &child_rows, 9).is_empty());
    }
}
