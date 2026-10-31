//! `ferrojt-viewer` — an interactive JT viewer built on egui and kiss3d.
//!
//! The window follows the reference layout: a *Scene Outliner* column on the
//! left listing the JT logical scene graph, and a 3D viewport filling the rest,
//! with a rotation trihedron in its upper-left corner and a small toolbar across
//! the top.
//!
//! Geometry does **not** go through glTF: [`ferrojt::Model`] accessors are
//! decoded straight into kiss3d [`GpuMesh3d`](kiss3d::resource::GpuMesh3d)
//! buffers ([`meshes`]), and the scene graph becomes a tree of
//! [`SceneNode3d`](kiss3d::scene::SceneNode3d) groups ([`scene`]) so that
//! selecting a row and selecting a shape are the same operation.

mod backdrop;
mod lod;
mod meshes;
mod picking;
mod scene;
mod tree;
mod trihedron;

use std::path::{Path, PathBuf};

use egui::{
    Align, CursorIcon, Frame, Id, Label, LayerId, Layout, Margin, Order, Pos2, Rect, RichText,
    Sense, Stroke, vec2,
};
use ferrojt::Model;
use ferrojt::jt::JtFile;
use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::color::Color;
use kiss3d::event::{Action as EventAction, Key, MouseButton, WindowEvent};
use kiss3d::light::Light;
use kiss3d::prelude::{Vec2, Vec3};
use kiss3d::window::Window;

use crate::scene::ViewerScene;

/// Height of the toolbar strip, in logical points.
const TOOLBAR: f32 = 34.0;

/// Gap left around the floating toolbar, in logical points.
///
/// The toolbar does not span the window edge to edge: it is a bar with rounded
/// corners set `MARGIN` points in from the top and from both sides, so the
/// viewport gradient shows through on all three sides and the bar reads as
/// floating rather than as a strip welded to the window.
const MARGIN: f32 = 5.0;

/// Smallest the outliner column can be dragged to, in logical points.
const MIN_TREE_WIDTH: f32 = 160.0;

/// Largest share of the window the outliner column may take.
const MAX_TREE_SHARE: f32 = 0.6;

/// Half the width of the strip that grabs the divider, in logical points.
///
/// Six points in total is about as narrow as a pointer target can be and still
/// be hit deliberately; the divider itself is drawn one point wide in the middle
/// of it.
const DIVIDER_GRAB: f32 = 3.0;

/// The project page the toolbar's About button opens.
const PROJECT_URL: &str = "https://github.com/jriegel/FerroJT";

/// Width the About button and its separator are given in the toolbar.
///
/// The status labels truncate rather than pushing the button off the right end,
/// so the bar needs a fixed allowance for it — egui 0.36 has no way to measure
/// a button before laying it out.
const ABOUT_RESERVE: f32 = 78.0;

/// Near plane of the fitted frustum, as a fraction of the model radius.
///
/// The clip planes are what makes or breaks a CAD model: JT parts sit flush
/// against each other by design — `SampleJT_Server.jt` has 44 pairs of node
/// boxes closer than 0.02 mm and 79 of its 105 instances thinner than 0.6 mm —
/// and the depth buffer is a `Depth32Float`, whose spacing at distance `z` is
/// about `z² / (near · 2²⁴)`. With the wide frustum this viewer used to start
/// from (`1e-3 .. 1e6` on an 82 mm model) that is 0.12 mm at the centre of the
/// model, coarser than the gaps it has to separate, so coplanar faces trade
/// the depth test pixel by pixel: the diagonal hatching of Z-fighting.
/// `near = r / 1000` brings the spacing down to a few micrometres.
const NEAR: f32 = 1.0e-3;

/// Far plane and largest orbit distance, as a multiple of the model radius.
const FAR: f32 = 1.0e2;

/// Closest orbit distance, as a fraction of the model radius.
///
/// It has to stay beyond [`NEAR`], otherwise zooming in pushes the near plane
/// through the camera and the model disappears instead of growing.
const MIN_DIST: f32 = 2.0e-2;

/// What the toolbar asked for. The buttons cannot act on the window directly:
/// [`Window::draw_ui`] borrows the window while the closure runs.
#[derive(Default)]
struct Commands {
    open: bool,
    fit: bool,
    exit: bool,
    /// The LOD toggle was clicked, so the scene has to be re-measured.
    lod: bool,
    /// The About button was clicked.
    about: bool,
}

/// Everything that survives a frame.
struct App {
    /// File currently on screen.
    path: Option<PathBuf>,
    /// Neutral model behind the scene, kept for its counts and up axis.
    model: Option<Model>,
    /// Decoded geometry, indexed like `Model::meshes`.
    decoded: Vec<Vec<meshes::Decoded>>,
    /// The kiss3d scene plus the outliner rows derived from it.
    viewer: ViewerScene,
    /// Outliner filter text.
    filter: String,
    /// Outliner row under rename.
    renaming: Option<tree::Rename>,
    /// Up direction of the loaded model, used by the trihedron's view presets.
    up: Vec3,
    /// One-line summary shown in the toolbar.
    status: String,
    /// Level-of-detail summary shown next to the toggle: what is on screen and
    /// what the file would allow.
    lod_status: String,
    /// `true` while the toolbar shows the level-of-detail toggle. A file with a
    /// single tier per part has nothing to switch, so the control is left out.
    show_lod: bool,
    /// Width of the outliner column, in logical points. The user drags the
    /// divider between the column and the viewport to change it.
    tree_width: f32,
    /// The gradient behind the model, re-oriented to face the camera each frame.
    backdrop: Option<backdrop::Backdrop>,
    /// Aspect ratio of the 3D viewport, kept up to date each frame.
    viewport: Viewport,
}

impl Default for App {
    fn default() -> Self {
        Self {
            path: None,
            model: None,
            decoded: Vec::new(),
            viewer: ViewerScene::default(),
            filter: String::new(),
            renaming: None,
            up: Vec3::Z,
            status: String::from("Drag and drop a .jt file, or press Open."),
            lod_status: String::new(),
            show_lod: false,
            tree_width: tree::DEFAULT_WIDTH,
            backdrop: None,
            viewport: Viewport {
                left: tree::DEFAULT_WIDTH,
                top: TOOLBAR + MARGIN,
                width: 480.0,
                height: 566.0,
                win_w: 800.0,
                win_h: 600.0,
            },
        }
    }
}

impl App {
    /// Read `path` and rebuild the scene, the outliner and the camera fit.
    fn load(&mut self, path: &Path, camera: &mut OrbitCamera3d) {
        // Every level of detail is loaded, not just the finest one: the viewer
        // picks a tier per part from the area it covers on screen, which needs
        // the coarse tiers to exist. `ferrojt::open` would keep only tier 0.
        let model = match JtFile::open(path).map(|file| file.to_model_with_all_lods()) {
            Ok(model) => model,
            Err(err) => {
                self.status = format!("{}: {err}", path.display());
                return;
            }
        };

        // JT stores its model space with +Z up; the orbit camera assumes +Y
        // unless told otherwise.
        let up = up_axis_of(&model);
        self.up = up;
        camera.set_up_axis(up);

        let decoded = meshes::build(&model);
        let nodes = model.nodes.len();
        let meshes = model.meshes.len();

        let mut viewer = ViewerScene::new(&model, &decoded);
        // One directional light plus the window's ambient term is enough to read
        // the shape; the materials carry the colours the file declares.
        viewer
            .root_mut()
            .add_light(Light::directional(Vec3::new(0.35, 0.5, -1.0).normalize()));
        // The gradient is an ordinary scene node, so unlike a skybox it stays out
        // of the outliner, the picker, the LOD stacks and the scene bounds.
        let backdrop = backdrop::Backdrop::new(viewer.root_mut());

        self.path = Some(path.to_path_buf());
        self.model = Some(model);
        self.decoded = decoded;
        self.backdrop = Some(backdrop);
        self.viewer = viewer;
        self.filter.clear();
        self.renaming = None;
        self.show_lod = !self.viewer.lod_groups().is_empty();
        self.status = format!(
            "{} · {nodes} nodes · {meshes} meshes",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        );
        self.lod_status = lod_summary(&self.viewer, self.viewer.total_triangle_count());

        fit(camera, &self.viewer, self.viewport, self.up);
    }

    /// Refresh the level-of-detail line of the toolbar.
    fn refresh_lod_status(&mut self) {
        let loaded = self.viewer.total_triangle_count();
        self.lod_status = lod_summary(&self.viewer, loaded);
    }
}

/// What is on screen and what the file declares, e.g.
/// `LOD 1/3 · 486 of 1492 triangles`. The tiers of the samples all have the
/// same depth, so one number pair describes the file; a mixed graph shows the
/// range instead. A file with a single tier per part has nothing to switch, so
/// it only reports what is drawn.
fn lod_summary(viewer: &ViewerScene, loaded: usize) -> String {
    let triangles = viewer.drawn_triangle_count();
    let groups = viewer.lod_groups();
    let Some(deepest) = groups.iter().map(|group| group.tiers.len()).max() else {
        return if triangles == loaded {
            format!("{triangles} triangles")
        } else {
            format!("{triangles} of {loaded} triangles")
        };
    };
    let shallowest = groups
        .iter()
        .map(|group| group.tiers.len())
        .min()
        .unwrap_or(0);
    let depth = if deepest == shallowest {
        deepest.to_string()
    } else {
        format!("{shallowest}..{deepest}")
    };
    let tier = groups.iter().map(|group| group.tier).max().unwrap_or(0) + 1;
    format!("LOD {tier}/{depth} · {triangles} of {loaded} triangles")
}

/// Frame the whole scene: focus on its centre and back off until it fits.
///
/// The projection always spans the **whole window** — kiss3d has no viewport
/// rectangle — so the outliner and toolbar are simply painted over the image.
/// `fit` therefore sizes the model against the free rectangle and pushes the
/// focus point sideways to move the model out from under the outliner.
///
/// The clip planes are re-fitted with the model. Kiss3d 0.47 exposes no setter
/// for them — [`OrbitCamera3d::new_with_frustum`] is the only way in — so the
/// camera is rebuilt from the state just computed and the orbit angles, focus
/// point and zoom limits are carried over.
fn fit(camera: &mut OrbitCamera3d, viewer: &ViewerScene, vp: Viewport, up: Vec3) {
    let Some((min, max)) = viewer.world_bounds_all() else {
        return;
    };
    let center = (min + max) * 0.5;
    let radius = ((max - min).length() * 0.5).max(1.0e-3);

    // The orbit camera clamps `dist` to [min_dist, max_dist], so widen those
    // before moving the camera — a 2 m bracket and a 2 km frame share this code.
    let min_dist = radius * MIN_DIST;
    let max_dist = radius * FAR;
    camera.set_min_dist(min_dist);
    camera.set_max_dist(max_dist);
    camera.set_at(center);

    // `fov` is the vertical angle over the whole window height, so the world
    // half-extent of the free rectangle at distance `d` is
    // `d * tan(fov/2) * vp.height / win_h` vertically and
    // `d * tan(fov/2) * vp.width / win_h` horizontally.
    let tan_v = (camera.fov() * 0.5).tan();
    let limit_v = tan_v * vp.height / vp.win_h;
    let limit_h = tan_v * vp.width / vp.win_h;
    let dist = radius / limit_v.min(limit_h) * 1.10;
    camera.set_dist(dist);

    // The focus point projects to the middle of the *window*; the free area is
    // offset from that, so shift the focus by the same offset in world units.
    let dx = vp.left + vp.width * 0.5 - vp.win_w * 0.5;
    let dy = vp.top + vp.height * 0.5 - vp.win_h * 0.5;
    if dx != 0.0 || dy != 0.0 {
        let per_point = 2.0 * dist * tan_v / vp.win_h;
        let view = camera.view_transform();
        // The view transform maps world → view space, so its inverse rotation
        // gives the world directions of the screen axes.
        let right = view.inverse_transform_vector(Vec3::X);
        let screen_up = view.inverse_transform_vector(Vec3::Y);
        camera.set_at(center - right * (dx * per_point) + screen_up * (dy * per_point));
    }

    // Re-fit the clip planes: the orbit distance is now within a factor of two
    // of the model radius, so the depth range can be too — see [`NEAR`].
    // Kiss3d 0.47 has no setter for them, so the camera is rebuilt and its
    // state carried over.
    let at = camera.at();
    let eye = camera.eye();
    let mut fresh = OrbitCamera3d::new_with_frustum(
        camera.fov(),
        (radius * NEAR).max(1.0e-6),
        dist + radius * FAR,
        eye,
        at,
    );
    // The constructor starts with a +Y up axis and its own zoom limits, and its
    // `look_at` clamps the distance to those; put everything back the way the
    // loaded model and the fit want it. The limits go first because
    // `set_up_axis` re-derives yaw and pitch through `look_at`, which clamps
    // against them.
    fresh.set_min_dist(min_dist);
    fresh.set_max_dist(max_dist);
    fresh.set_up_axis(up);
    fresh.set_at(at);
    fresh.set_dist(dist);
    *camera = fresh;
}

/// The up direction of the loaded model, `+Z` unless the source says otherwise.
///
/// The neutral model spells the axis out as text (`"+Z"`, `"Y_UP"`, …) and the
/// JT reader leaves it unset for every sample, so `+Z` is the default.
fn up_axis_of(model: &Model) -> Vec3 {
    let axis = model.up_axis.clone().unwrap_or_default();
    if axis.to_ascii_uppercase().contains('Y') {
        Vec3::Y
    } else {
        Vec3::Z
    }
}

/// Aspect ratio of the 3D viewport: the window minus the outliner column and
/// the toolbar strip. kiss3d reports physical pixels, the panel widths are
/// egui points, so the scale factor has to be applied to the former.
#[derive(Clone, Copy, Debug)]
struct Viewport {
    /// Left edge of the free area, in points.
    left: f32,
    /// Top edge of the free area, in points.
    top: f32,
    /// Size of the free area, in points.
    width: f32,
    height: f32,
    /// Size of the whole window, in points.
    win_w: f32,
    win_h: f32,
}

impl Viewport {
    /// The area left over once the toolbar and the outliner are subtracted from
    /// `window`. The toolbar floats [`MARGIN`] points below the top edge, so the
    /// free area starts below that gap as well.
    fn of(window: &Window, tree_width: f32) -> Self {
        let size = window.size();
        let ppp = window.scale_factor() as f32;
        let win_w = size.x as f32 / ppp;
        let win_h = size.y as f32 / ppp;
        Self {
            left: tree_width,
            top: TOOLBAR + MARGIN,
            width: (win_w - tree_width).max(1.0),
            height: (win_h - TOOLBAR - MARGIN).max(1.0),
            win_w,
            win_h,
        }
    }
}

#[kiss3d::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mut window = Window::new("FerroJT Viewer").await;
    window.set_background_color(Color::new(0.13, 0.14, 0.16, 1.0));
    window.set_ambient(0.35);

    // Placeholder frustum: `fit` rebuilds the camera with planes sized to the
    // model as soon as a file is loaded.
    let mut camera = OrbitCamera3d::new_with_frustum(
        std::f32::consts::FRAC_PI_4,
        0.1,
        1.0e3,
        Vec3::new(3.0, 3.0, 3.0),
        Vec3::ZERO,
    );
    camera.set_up_axis(Vec3::Z);

    // The first `fit` happens inside `load`, so the viewport has to be known
    // before it.
    let mut app = App {
        viewport: Viewport::of(&window, tree::DEFAULT_WIDTH),
        ..App::default()
    };
    if let Some(arg) = std::env::args().nth(1) {
        app.load(Path::new(&arg), &mut camera);
    }

    while window.render_3d(app.viewer.root_mut(), &mut camera).await {
        app.viewport = Viewport::of(&window, app.tree_width);

        // --- input events -------------------------------------------------
        let mut fit_requested = false;
        let mut click: Option<Vec2> = None;
        {
            let mut events = window.events();
            for event in events.iter() {
                match event.value {
                    WindowEvent::MouseButton(MouseButton::Button1, EventAction::Press, _) => {
                        // The outliner, the toolbar and the trihedron are egui
                        // widgets; while one of them owns the pointer the click
                        // belongs to it, not to the viewport.
                        if !window.is_egui_capturing_mouse() {
                            if let Some((x, y)) = window.cursor_pos() {
                                click = Some(Vec2::new(x as f32, y as f32));
                            }
                        }
                    }
                    WindowEvent::Key(Key::F, EventAction::Press, _)
                        if !window.is_egui_capturing_keyboard() =>
                    {
                        fit_requested = true;
                    }
                    _ => {}
                }
            }
        }

        if let Some(cursor) = click {
            let size = window.size();
            let size = Vec2::new(size.x as f32, size.y as f32);
            let hit = picking::pick(&camera, &app.viewer, cursor, size);
            app.viewer.set_selected(hit);
        }

        // Only the first dropped file is opened: a drop of several files would
        // otherwise load them all into one scene.
        if let Some(path) = window.dropped_files().into_iter().next() {
            app.load(&path, &mut camera);
        }

        // Pick a tier per part from the area it covers on screen. This runs
        // before the widgets so the toolbar reports what this frame draws.
        let size = window.size();
        lod::select(
            &camera,
            &mut app.viewer,
            Vec2::new(size.x as f32, size.y as f32),
        );
        app.refresh_lod_status();

        // Keep the gradient square facing the camera and large enough to cover
        // the frustum, so it reads as a backdrop rather than as a card in the
        // scene.
        if let Some(backdrop) = app.backdrop.as_mut() {
            let aspect = size.x as f32 / (size.y.max(1) as f32);
            backdrop.place(&camera, aspect);
        }

        // --- egui: toolbar, outliner, trihedron ---------------------------
        let mut commands = Commands::default();
        window.draw_ui(|ctx| {
            let screen = ctx.input(|i| i.raw.screen_rect.unwrap_or_else(|| i.viewport_rect()));
            let style = ctx.style_of(ctx.theme());
            egui::Window::new("ferrojt.toolbar")
                .title_bar(false)
                .fixed_pos(Pos2::new(screen.min.x + MARGIN, screen.min.y + MARGIN))
                .fixed_size(vec2((screen.width() - 2.0 * MARGIN).max(1.0), TOOLBAR))
                .movable(false)
                .resizable(false)
                .collapsible(false)
                .order(Order::Middle)
                .frame(
                    Frame::new()
                        .inner_margin(Margin::symmetric(8, 4))
                        .corner_radius(6.0)
                        .fill(style.visuals.panel_fill)
                        .stroke(Stroke::new(1.0, style.visuals.window_fill)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Open").clicked() {
                            commands.open = true;
                        }
                        if ui.button("View All").clicked() {
                            commands.fit = true;
                        }
                        if ui.button("Exit").clicked() {
                            commands.exit = true;
                        }
                        if app.show_lod {
                            ui.separator();
                            let mut auto = app.viewer.auto_lod();
                            if ui
                                .checkbox(&mut auto, "LOD by screen size")
                                .on_hover_text(
                                    "Choose the level of detail of each \
                                                 part from the area it covers",
                                )
                                .clicked()
                            {
                                app.viewer.set_auto_lod(auto);
                                commands.lod = true;
                            }
                        }
                        ui.separator();
                        // The status text must give way to the About button
                        // instead of pushing it off the right end of the bar, so
                        // the labels get a box of what is left after an
                        // allowance for the button and truncate inside it.
                        let room = (ui.available_width() - ABOUT_RESERVE).max(0.0);
                        let height = ui.spacing().interact_size.y;
                        ui.allocate_ui_with_layout(
                            vec2(room, height),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add(
                                    Label::new(RichText::new(&app.lod_status).weak()).truncate(),
                                );
                                ui.separator();
                                ui.add(Label::new(RichText::new(&app.status).weak()).truncate());
                            },
                        );
                        // About sits hard against the right end of the bar: the
                        // reverse layout consumes whatever the labels left over,
                        // so it stays right-justified at any bar width.
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .button("About")
                                .on_hover_text("Open the FerroJT project page")
                                .clicked()
                            {
                                commands.about = true;
                            }
                        });
                    });
                });

            let outliner = outliner_rect(screen, app.tree_width);
            tree::show(
                ctx,
                &mut app.viewer,
                outliner,
                &mut app.filter,
                &mut app.renaming,
            );

            if let Some(width) = outliner_resize(ctx, screen, outliner, app.tree_width) {
                app.tree_width = width;
                // The free rectangle changed shape, so re-frame the model.
                commands.fit = true;
            }

            let up = app.up;
            trihedron::show(
                ctx,
                &mut camera,
                up,
                Pos2::new(outliner.max.x + 14.0, outliner.min.y + 14.0),
            );
        });

        // --- window actions, now that `draw_ui` has released the window ----
        if commands.fit || fit_requested {
            let up = app.up;
            fit(&mut camera, &app.viewer, app.viewport, up);
        }
        if commands.lod {
            // Turning the selection off restores the finest tier everywhere.
            app.refresh_lod_status();
        }
        if commands.open {
            if let Some(path) = pick_file() {
                app.load(&path, &mut camera);
            }
        }
        if commands.exit {
            window.close();
        }
        if commands.about {
            open_project_page(&mut app.status);
        }
    }
}

/// Open the project page in the user's browser.
///
/// egui's own `Context::open_url` is not an option: it only appends an
/// `OpenUrl` command to egui's output, and kiss3d 0.47 never reads egui's
/// command list, so the click would silently do nothing.
fn open_project_page(status: &mut String) {
    if let Err(err) = webbrowser::open(PROJECT_URL) {
        *status = format!("{PROJECT_URL}: {err}");
    }
}

/// The outliner column: the left edge of the window to the divider, below the
/// toolbar and its margin, down to the bottom edge.
fn outliner_rect(screen: Rect, width: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new(screen.min.x, screen.min.y + TOOLBAR + MARGIN),
        Pos2::new(screen.min.x + width, screen.max.y),
    )
}

/// The divider between the outliner and the viewport, and what dragging it does.
///
/// Returns the new column width when the divider moved, `None` otherwise.
///
/// The handle is an [`egui::Area`] rather than a widget because it has to span
/// the whole height of the window without taking part in the outliner's layout,
/// and it is drawn in [`Order::Foreground`] so it sits above both panels. Its
/// closure is not empty for a reason: an area sizes itself to its contents, so
/// an empty one collapses to a point after its first frame and the handle stops
/// responding — `set_min_size` is what pins it to the strip. An area can only
/// grow that `Ui`, which starts as a zero-size point at the area's top-left
/// corner, so the size stays exact.
///
/// Side effect worth knowing: while the pointer is on this strip egui owns the
/// mouse, and kiss3d then drops the events that would otherwise orbit the camera
/// (`events.rs` consults `is_egui_capturing_mouse`). Dragging the divider
/// therefore does not also spin the model.
fn outliner_resize(ctx: &egui::Context, screen: Rect, outliner: Rect, width: f32) -> Option<f32> {
    let id = Id::new("ferrojt.outliner.resize");
    let strip = Rect::from_min_max(
        Pos2::new(outliner.max.x - DIVIDER_GRAB, outliner.min.y),
        Pos2::new(outliner.max.x + DIVIDER_GRAB, screen.max.y),
    );
    let response = egui::Area::new(id)
        .fixed_pos(strip.min)
        .default_size(strip.size())
        .sense(Sense::drag())
        .order(Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_size(strip.size());
        })
        .response;

    if response.hovered() || response.dragged() {
        ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
    }

    // The divider is otherwise invisible — the outliner's own frame already
    // draws its border — so it is re-drawn here and brightened while it can be
    // grabbed, which is what makes the handle discoverable at all.
    let style = ctx.style_of(ctx.theme());
    let color = if response.dragged() {
        style.visuals.widgets.active.fg_stroke.color
    } else if response.hovered() {
        style.visuals.widgets.hovered.fg_stroke.color
    } else {
        style.visuals.widgets.noninteractive.bg_stroke.color
    };
    ctx.layer_painter(LayerId::new(Order::Foreground, id))
        .vline(outliner.max.x, outliner.y_range(), Stroke::new(1.0, color));

    if !response.dragged() {
        return None;
    }
    // The column may never be narrower than its own controls nor wider than most
    // of the window, so the viewport always keeps a usable share.
    let widest = (screen.width() * MAX_TREE_SHARE).max(MIN_TREE_WIDTH);
    let next = (width + response.drag_delta().x).clamp(MIN_TREE_WIDTH, widest);
    (next != width).then_some(next)
}

/// Native file dialog for the toolbar button.
fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open a JT file")
        .add_filter("JT", &["jt"])
        .pick_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 700.0))
    }

    #[test]
    fn the_outliner_column_starts_below_the_toolbar() {
        let rect = outliner_rect(screen(), 320.0);
        assert_eq!(rect.min, Pos2::new(0.0, TOOLBAR + MARGIN));
        assert_eq!(rect.max.x, 320.0);
        assert_eq!(rect.max.y, 700.0);
    }

    /// Drives the divider with synthetic pointer events — the same events the
    /// window forwards — and returns the width after the drag.
    ///
    /// The pointer goes down on `grab`, moves to `to` in small steps (a drag is
    /// accumulated from the per-frame pointer delta, so one big jump would be
    /// clamped away), and is released. `start` is the width before the drag.
    fn dragged_width(grab: f32, to: f32, start: f32) -> f32 {
        let ctx = egui::Context::default();
        let screen = screen();
        let mut width = start;
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                focused: true,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                let outliner = outliner_rect(screen, width);
                if let Some(next) = outliner_resize(ui.ctx(), screen, outliner, width) {
                    width = next;
                }
            });
            output.drop_without_applying_deltas();
        };

        // Hit testing uses the widget rects of the *previous* pass, and an area
        // is not interactable during its first (sizing) pass — so two frames go
        // by before a press can land on the handle. Warm it up.
        for _ in 0..2 {
            frame(vec![egui::Event::PointerMoved(Pos2::new(grab, 400.0))]);
        }

        let steps = 20;
        for step in 0..=steps {
            let x = grab + (to - grab) * (step as f32 / steps as f32);
            let at = Pos2::new(x, 400.0);
            let mut events = vec![egui::Event::PointerMoved(at)];
            if step == 0 {
                events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
            }
            frame(events);
        }
        // The release goes in a frame of its own: egui clears `dragged` as soon
        // as it sees the button go up, so a move bundled with the release is not
        // counted as a drag delta.
        frame(vec![egui::Event::PointerButton {
            pos: Pos2::new(to, 400.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }]);
        width
    }

    #[test]
    fn dragging_the_divider_moves_it_by_the_pointer() {
        // Grabbed at the divider (x = 320) and pulled 100 points right.
        let got = dragged_width(320.0, 420.0, 320.0);
        assert!((got - 420.0).abs() < 1.0, "got {got}");
    }

    #[test]
    fn the_divider_stops_at_its_clamps() {
        // Never narrower than the controls need, never wider than most of the
        // window — the viewport has to keep a usable share.
        assert_eq!(dragged_width(320.0, 0.0, 320.0), MIN_TREE_WIDTH);
        assert_eq!(
            dragged_width(320.0, 999.0, 320.0),
            screen().width() * MAX_TREE_SHARE
        );
    }
}
