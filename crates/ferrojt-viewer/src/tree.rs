//! The "Scene Outliner" panel: a left-hand tree of the JT scene graph.
//!
//! Each row mirrors one attachment of a [`ferrojt::model::Node`] in the graph:
//! a disclosure triangle, a kind glyph, the node name, and an eye toggle on the
//! right — the layout of the reference screenshot. Clicking a row selects the
//! node, which also highlights its geometry in the viewport (and vice versa, see
//! [`crate::picking`]).

use egui::containers::collapsing_header::paint_default_icon;
use egui::{
    Color32, Frame, Id, Label, Layout, Margin, Order, Rect, RichText, ScrollArea, Sense, Shape,
    Stroke, Ui, UiBuilder, Window, pos2, vec2,
};

use crate::scene::ViewerScene;

/// Width of the outliner column, in points.
pub const WIDTH: f32 = 320.0;

/// A rename in progress: the row being edited and the text typed so far.
pub struct Rename {
    /// Row under edit.
    pub row: usize,
    /// Text being typed.
    pub text: String,
}

/// What the user did to the outliner this frame.
#[derive(Default)]
pub struct Action {
    /// A row was renamed.
    pub renamed: Option<(usize, String)>,
}

/// Draw the outliner anchored at `viewport.min`, [`WIDTH`] wide and as tall as
/// the window.
pub fn show(
    ctx: &egui::Context,
    scene: &mut ViewerScene,
    viewport: Rect,
    filter: &mut String,
    renaming: &mut Option<Rename>,
) -> Action {
    let mut action = Action::default();

    let style = ctx.style_of(ctx.theme());
    let frame = Frame::new()
        .inner_margin(Margin::symmetric(8, 6))
        .fill(style.visuals.panel_fill)
        .stroke(Stroke::new(1.0, style.visuals.window_fill));

    let mut open = None;

    Window::new("Scene Outliner")
        .fixed_pos(viewport.min)
        .fixed_size(vec2(WIDTH, viewport.height()))
        .movable(false)
        .resizable(false)
        .collapsible(false)
        .title_bar(true)
        .order(Order::Middle)
        .frame(frame)
        .show(ctx, |ui| {
            ui.label(
                "Drag and drop a JT file to open it · click a row to select it · \
                 click the eye icon to toggle visibility.",
            );
            ui.separator();

            let filtered = !filter.trim().is_empty();
            let changed = ui
                .add(
                    egui::TextEdit::singleline(filter)
                        .hint_text("Filter nodes")
                        .desired_width((ui.available_width() - 96.0).max(40.0)),
                )
                .changed();
            if ui.button("Expand").clicked() {
                open = Some(true);
            }
            if ui.button("Collapse").clicked() {
                open = Some(false);
            }
            if changed && filtered {
                // A filter shows branches that are closed, so open them all.
                scene.set_rows_open(true);
            }
            ui.separator();

            let shown: Vec<usize> = if filtered {
                filtered_rows(scene, filter.trim())
            } else {
                scene.visible_rows()
            };

            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for &id in &shown {
                        draw_row(ui, scene, id, filtered, renaming, &mut action);
                    }
                });
        });

    if let Some(open) = open {
        scene.set_rows_open(open);
    }

    action
}

/// Rows to draw while filtering: every row whose name or class matches, plus
/// the rows leading to a match. The collapse state is ignored while filtering.
fn filtered_rows(scene: &ViewerScene, filter: &str) -> Vec<usize> {
    let needle = filter.to_lowercase();
    let mut keep = vec![false; scene.row_count()];
    for row in scene.rows() {
        if row.name.to_lowercase().contains(&needle) || row.kind.to_lowercase().contains(&needle) {
            let mut parent = Some(row.id);
            while let Some(p) = parent {
                keep[p] = true;
                parent = scene.row(p).and_then(|r| r.parent);
            }
        }
    }
    scene
        .rows()
        .iter()
        .filter(|row| keep[row.id])
        .map(|row| row.id)
        .collect()
}

fn draw_row(
    ui: &mut Ui,
    scene: &mut ViewerScene,
    id: usize,
    filtering: bool,
    renaming: &mut Option<Rename>,
    action: &mut Action,
) {
    let Some(model_row) = scene.row(id) else {
        return;
    };
    let depth = model_row.depth;
    let name = model_row.name.clone();
    let kind = model_row.kind.clone();
    let has_children = scene.has_children(id);
    let children = scene.child_rows(id).len();
    let node = model_row.node;
    let instances = model_row.instances;

    let collapsed = !filtering && scene.is_collapsed(id);
    let drawn = scene.is_drawn(id);
    let visible = scene.is_visible(id);
    let selected = scene.selected() == Some(id);

    let height = ui
        .spacing()
        .interact_size
        .y
        .max(ui.text_style_height(&egui::TextStyle::Body));
    let rect = ui
        .allocate_rect(
            Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), height)),
            Sense::hover(),
        )
        .rect;

    if selected {
        ui.painter()
            .rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
    }

    let icon = ui.spacing().icon_width;
    let indent = if filtering { 0 } else { depth };
    let mut x = rect.min.x + 2.0 + indent as f32 * ui.spacing().indent;
    let mid = rect.center().y;

    // Disclosure triangle.
    let tri_rect = Rect::from_min_size(pos2(x, rect.min.y), vec2(height, height));
    let tri = ui.interact(tri_rect, Id::new(("tri", id)), Sense::click());
    if has_children {
        let openness = if collapsed { 0.0 } else { 1.0 };
        paint_default_icon(ui, openness, &tri);
    }
    if tri.clicked() {
        scene.toggle_collapse(id);
    }
    x += height + 2.0;

    // Kind glyph.
    let (glyph, glyph_color) = kind_glyph(&kind);
    if !glyph.is_empty() {
        ui.painter().text(
            pos2(x + icon * 0.5, mid),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(icon),
            glyph_color,
        );
    }
    x += icon + 4.0;

    // Eye toggle, pinned to the right edge of the row.
    let eye_rect = Rect::from_min_size(
        pos2(rect.max.x - icon - 4.0, rect.min.y),
        vec2(icon, height),
    );
    let eye = ui.interact(eye_rect, Id::new(("eye", id)), Sense::click());
    paint_eye(ui, eye_rect, visible, eye.hovered());
    if eye.clicked() {
        scene.set_visible(id, !visible);
    }

    // A node reached twice — an `Instance` sharing its subtree — says how often,
    // which is why one screw shows up in two places.
    let mut name_right = eye_rect.min.x - 4.0;
    if instances > 1 {
        let right = eye_rect.min.x - 4.0;
        name_right = right - 26.0;
        ui.painter().text(
            pos2(right, mid),
            egui::Align2::RIGHT_CENTER,
            format!("\u{d7}{instances}"),
            egui::FontId::proportional(icon * 0.9),
            Color32::from_gray(150),
        );
    }

    // Name — a text field while renaming, otherwise a truncated label.
    let name_rect = Rect::from_min_max(pos2(x, rect.min.y), pos2(name_right, rect.max.y));
    match renaming.as_mut() {
        Some(edit) if edit.row == id => {
            let response = ui.put(
                name_rect,
                egui::TextEdit::singleline(&mut edit.text).desired_width(name_rect.width()),
            );
            response.request_focus();
            if response.lost_focus() {
                scene.set_name(id, &edit.text);
                action.renamed = Some((id, std::mem::take(&mut edit.text)));
                *renaming = None;
            }
        }
        _ => {
            // `Ui::put` centres and justifies, which would float the names to the
            // middle of the row — a left-to-right child keeps them in a column.
            let inner = ui.scope_builder(
                UiBuilder::new()
                    .max_rect(name_rect)
                    .layout(Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    let text = if drawn {
                        RichText::new(&name)
                    } else {
                        RichText::new(&name).color(Color32::from_gray(110))
                    };
                    ui.add(Label::new(text).truncate())
                },
            );
            let label = inner
                .inner
                .on_hover_text(format!("JT node #{node} · {kind} · {children} children"));
            if label.clicked() {
                scene.set_selected(Some(id));
            }
            if label.double_clicked() {
                *renaming = Some(Rename {
                    row: id,
                    text: name.clone(),
                });
            }
        }
    }
}

/// The JT class name is stored verbatim on the node, so the glyph is picked by
/// substring rather than from an enum.
fn kind_glyph(kind: &str) -> (&'static str, Color32) {
    const BLUE: Color32 = Color32::from_rgb(120, 170, 230);
    const AMBER: Color32 = Color32::from_rgb(230, 190, 120);
    const GREEN: Color32 = Color32::from_rgb(150, 210, 150);
    const CYAN: Color32 = Color32::from_rgb(140, 200, 200);
    const GREY: Color32 = Color32::from_rgb(170, 170, 170);

    if kind.contains("Instance") {
        ("\u{29c9}", GREEN)
    } else if kind.contains("RangeOfDetail") {
        ("\u{2263}", CYAN)
    } else if kind.contains("LevelOfDetail") {
        ("\u{2261}", CYAN)
    } else if kind.contains("Light") {
        ("\u{2600}", Color32::from_rgb(250, 220, 120))
    } else if kind.contains("Material") {
        ("\u{25d0}", Color32::from_rgb(220, 150, 200))
    } else if kind.contains("MetaData") {
        ("\u{2139}", BLUE)
    } else if kind.contains("Shape") || kind.contains("Segment") {
        ("\u{25c6}", AMBER)
    } else if kind.contains("Assembly") || kind.contains("Partition") {
        ("\u{25a3}", BLUE)
    } else if kind.contains("Group") {
        ("\u{25a6}", GREY)
    } else if kind.is_empty() {
        ("", GREY)
    } else {
        ("\u{25cb}", GREY)
    }
}

/// A minimal eye: an ellipse with a pupil, crossed out when hidden.
fn paint_eye(ui: &Ui, rect: Rect, visible: bool, hovered: bool) {
    let center = rect.center();
    let radius = vec2(rect.height() * 0.36, rect.height() * 0.24);
    let color = if hovered {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    if visible {
        ui.painter().add(Shape::ellipse_stroke(
            center,
            radius,
            Stroke::new(1.2, color),
        ));
        ui.painter().circle_filled(center, radius.y * 0.55, color);
    } else {
        ui.painter().add(Shape::ellipse_stroke(
            center,
            radius,
            Stroke::new(1.2, Color32::from_gray(110)),
        ));
        let pad = radius.x * 0.9;
        ui.painter().line_segment(
            [center - vec2(pad, radius.y), center + vec2(pad, radius.y)],
            Stroke::new(1.4, color),
        );
    }
}
