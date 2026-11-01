//! The "Node Details" panel: the key/value half of the outliner column.
//!
//! The tree on top lists the JT logical scene graph; this pane reports what the
//! selected row actually carries. The two halves are separate [`egui::Window`]s
//! because each needs its own title bar and scroll area, and a window can only
//! be placed from a rect — `main.rs` splits the column and hands each half one.
//!
//! The interesting content is the key/value stream a JT node keeps in its
//! meta data: `Name::`, `CAD_MASS::`, `Translation Date::`, the layer tags. The
//! reader collects those into [`Node::extras`](ferrojt::model::Node::extras)
//! under `"properties"`, which is where this pane reads them back. Neither the
//! glTF nor the USD writer emits them, so the viewer is their only exit.

use egui::{
    Align, Frame, Label, Layout, Margin, Order, Rect, RichText, ScrollArea, Stroke, Ui, Window,
    vec2,
};
use ferrojt::model::{Mesh, Model, Node};

use crate::scene::{Row, ViewerScene};

/// Starting height of the detail pane, in points.
///
/// Only a starting point: the pane is resizable, so the height actually drawn
/// comes from the `viewport` rect the caller passes in.
pub const DEFAULT_HEIGHT: f32 = 210.0;

/// One line of the panel.
enum Line {
    /// A heading above a group of pairs.
    Section(&'static str),
    /// A key and its value.
    Pair(String, String),
}

/// Draw the details of the selected row at `viewport`.
///
/// `model` is the neutral scene the rows were built from — the rows only carry
/// an index into it, so the properties live there and not in
/// [`ViewerScene`]. With no file loaded, or no row selected, the pane says so
/// rather than sitting empty.
pub fn show(ctx: &egui::Context, model: Option<&Model>, scene: &ViewerScene, viewport: Rect) {
    let style = ctx.style_of(ctx.theme());
    let frame = Frame::new()
        .inner_margin(Margin::symmetric(8, 6))
        .fill(style.visuals.panel_fill)
        .stroke(Stroke::new(1.0, style.visuals.window_fill));

    Window::new("Node Details")
        .fixed_pos(viewport.min)
        .fixed_size(viewport.size())
        .movable(false)
        .resizable(false)
        .collapsible(false)
        .title_bar(true)
        .order(Order::Middle)
        .frame(frame)
        .show(ctx, |ui| {
            let Some(model) = model else {
                ui.label("Open a JT file to see its nodes here.");
                return;
            };
            let Some(row) = scene.selected().and_then(|id| scene.row(id)) else {
                ui.label("Click a row in the tree above to see what that node carries.");
                return;
            };
            let Some(node) = model.nodes.get(row.node) else {
                return;
            };

            let lines = detail_lines(model, scene, row, node);
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for line in &lines {
                        match line {
                            Line::Section(title) => {
                                ui.add_space(4.0);
                                ui.add(Label::new(RichText::new(*title).small().strong()));
                                ui.separator();
                            }
                            Line::Pair(key, value) => pair(ui, key, value),
                        }
                    }
                });
        });
}

/// Everything worth reporting about one row, in display order.
fn detail_lines(model: &Model, scene: &ViewerScene, row: &Row, node: &Node) -> Vec<Line> {
    let mut out = vec![Line::Section("Node")];

    out.push(Line::Pair("model index".to_owned(), row.node.to_string()));
    out.push(Line::Pair("class".to_owned(), row.kind.clone()));
    out.push(Line::Pair(
        "name".to_owned(),
        node.name.clone().unwrap_or_else(|| "(unnamed)".to_owned()),
    ));
    out.push(Line::Pair("tree depth".to_owned(), row.depth.to_string()));
    out.push(Line::Pair(
        "child rows".to_owned(),
        scene.child_rows(row.id).len().to_string(),
    ));
    out.push(Line::Pair(
        "visible".to_owned(),
        yes_no(scene.is_visible(row.id)),
    ));
    out.push(Line::Pair(
        "drawn".to_owned(),
        yes_no(scene.is_drawn(row.id)),
    ));

    out.push(Line::Section("Transform"));
    let transform = node.transform;
    out.push(Line::Pair(
        "translation".to_owned(),
        triple(transform.translation),
    ));
    out.push(Line::Pair("rotation".to_owned(), quad(transform.rotation)));
    out.push(Line::Pair("scale".to_owned(), triple(transform.scale)));
    if let Some((min, max)) = scene.world_bounds(row.id) {
        out.push(Line::Pair("world box min".to_owned(), triple(min.into())));
        out.push(Line::Pair("world box max".to_owned(), triple(max.into())));
    }

    out.push(Line::Section("Geometry"));
    if node.instances.is_empty() {
        out.push(Line::Pair("instances".to_owned(), "none".to_owned()));
    }
    for (index, instance) in node.instances.iter().enumerate() {
        let Some(mesh) = model.meshes.get(instance.mesh) else {
            continue;
        };
        let name = mesh
            .name
            .clone()
            .unwrap_or_else(|| format!("<mesh {}>", instance.mesh));
        out.push(Line::Pair(format!("mesh {index}"), name));
        out.push(Line::Pair(
            "triangles".to_owned(),
            triangle_count(model, mesh).to_string(),
        ));
        out.push(Line::Pair(
            "attributes".to_owned(),
            attribute_names(model, mesh).join(", "),
        ));
        // The material index has to stay in scope for the placeholder name, so
        // the match is spelled out instead of folded into a map chain.
        let material = match instance.material {
            Some(index) => model
                .materials
                .get(index)
                .map(|material| {
                    material
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("<material {index}>"))
                })
                .unwrap_or_else(|| format!("<material {index}>")),
            None => "none".to_owned(),
        };
        out.push(Line::Pair("material".to_owned(), material));
        if let Some(level) = mesh.lod_level {
            out.push(Line::Pair("lod tier".to_owned(), level.to_string()));
        }
    }
    out.push(Line::Pair(
        "row triangles".to_owned(),
        scene.row_triangles(row.id).to_string(),
    ));

    // A range LOD row owns a stack of tiers; report which one is on screen.
    let group = scene
        .lod_groups()
        .iter()
        .position(|entry| entry.parent == row.id);
    if let Some(group) = group {
        let tiers = scene.tier_triangles(group).len();
        out.push(Line::Section("Level of detail"));
        out.push(Line::Pair(
            "tier drawn".to_owned(),
            format!(
                "{} of {tiers}",
                scene
                    .lod_tier(group)
                    .map_or_else(|| "?".to_owned(), |t| t.to_string())
            ),
        ));
        out.push(Line::Pair(
            "tier triangles".to_owned(),
            scene
                .tier_triangles(group)
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" / "),
        ));
    }

    let mut properties = false;
    if let Some(serde_json::Value::Object(extra)) = node.extras.as_ref() {
        if let Some(serde_json::Value::Object(values)) = extra.get("properties") {
            if !values.is_empty() {
                properties = true;
                out.push(Line::Section("JT properties"));
                for (key, value) in values {
                    out.push(Line::Pair(
                        key.clone(),
                        value.as_str().unwrap_or("").to_owned(),
                    ));
                }
            }
        }
    }
    if !properties {
        out.push(Line::Section("JT properties"));
        out.push(Line::Pair(
            "value".to_owned(),
            "this node declares none".to_owned(),
        ));
    }

    out
}

/// One key/value row, in two columns.
///
/// The key column gets a fixed share of the width so the values line up, and
/// **both** columns truncate: an egui window is exactly as wide as its widest
/// child, and a JT value can be a 256-entry layer filter or a weld-type table —
/// an untruncated label would push the panel past the divider the user dragged.
/// A truncated label shows its full text in a tooltip on hover, which is
/// `Label::show_tooltip_when_elided` and on by default.
fn pair(ui: &mut Ui, key: &str, value: &str) {
    let height = ui.spacing().interact_size.y;
    let width = (ui.available_width() * 0.34).max(70.0);
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            vec2(width, height),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add(Label::new(RichText::new(key).weak()).truncate());
            },
        );
        ui.add(Label::new(value).truncate());
    });
}

/// Triangles a mesh draws, summed over its primitives.
fn triangle_count(model: &Model, mesh: &Mesh) -> usize {
    mesh.primitives
        .iter()
        .map(|primitive| {
            primitive
                .indices
                .and_then(|index| model.accessors.attributes.get(index))
                .map_or(0, |attribute| attribute.count / 3)
        })
        .sum()
}

/// Vertex attribute names of a mesh, e.g. `POSITION, NORMAL`.
fn attribute_names(model: &Model, mesh: &Mesh) -> Vec<String> {
    let mut names: Vec<String> = mesh
        .primitives
        .iter()
        .flat_map(|primitive| primitive.attributes.iter())
        .filter_map(|&index| model.accessors.attributes.get(index))
        .map(|attribute| attribute.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// A short decimal: integral values lose their fraction entirely, the rest keep
/// four digits — enough to read a millimetre coordinate without a wall of zeros.
fn num(value: f32) -> String {
    if value.fract() == 0.0 && value.abs() < 1.0e7 {
        format!("{}", value as i64)
    } else {
        format!("{value:.4}")
    }
}

fn triple(value: [f32; 3]) -> String {
    let [x, y, z] = [num(value[0]), num(value[1]), num(value[2])];
    format!("({x}, {y}, {z})")
}

fn quad(value: [f32; 4]) -> String {
    let [x, y, z, w] = [num(value[0]), num(value[1]), num(value[2]), num(value[3])];
    format!("({x}, {y}, {z}, {w})")
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_lose_their_fraction_only_when_integral() {
        assert_eq!(num(0.0), "0");
        assert_eq!(num(-25.4), "-25.4000");
        assert_eq!(num(48.0), "48");
        assert_eq!(triple([1.0, 2.5, -3.0]), "(1, 2.5000, -3)");
        assert_eq!(quad([0.0, 0.0, 0.0, 1.0]), "(0, 0, 0, 1)");
    }

    #[test]
    fn a_mesh_counts_its_triangles_per_primitive() {
        use ferrojt::model::{Attribute, ComponentType, Primitive, Topology};

        let mut model = Model::new();
        let position = model.accessors.push_attribute(Attribute {
            name: "POSITION".to_owned(),
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
            name: "INDICES".to_owned(),
            buffer: 0,
            offset: 0,
            count: 6,
            components: 3,
            component_type: ComponentType::U32,
            normalized: false,
            min: None,
            max: None,
        });
        let mesh = model.push_mesh(Mesh {
            name: Some("two triangles".to_owned()),
            primitives: vec![
                Primitive {
                    attributes: vec![position],
                    indices: Some(indices),
                    topology: Topology::TriangleList,
                    material: None,
                    bounds: None,
                    extras: None,
                },
                Primitive {
                    attributes: vec![position],
                    indices: None,
                    topology: Topology::TriangleList,
                    material: None,
                    bounds: None,
                    extras: None,
                },
            ],
            ..Mesh::default()
        });
        assert_eq!(triangle_count(&model, &model.meshes[mesh]), 2);
        assert_eq!(attribute_names(&model, &model.meshes[mesh]), ["POSITION"]);
    }
}
