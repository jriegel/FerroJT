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

mod meshes;
mod picking;
mod scene;
mod tree;
mod trihedron;

use std::path::{Path, PathBuf};

use egui::{Frame, Margin, Pos2, Rect, RichText, Stroke, vec2};
use ferrojt::Model;
use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::color::Color;
use kiss3d::event::{Action as EventAction, Key, MouseButton, WindowEvent};
use kiss3d::light::Light;
use kiss3d::prelude::{Vec2, Vec3};
use kiss3d::window::Window;

use crate::scene::ViewerScene;

/// Height of the toolbar strip, in logical points.
const TOOLBAR: f32 = 34.0;

/// What the toolbar asked for. The buttons cannot act on the window directly:
/// [`Window::draw_ui`] borrows the window while the closure runs.
#[derive(Default)]
struct Commands {
    open: bool,
    fit: bool,
    exit: bool,
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
            viewport: Viewport {
                left: tree::WIDTH,
                top: TOOLBAR,
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
        let model = match ferrojt::open(path) {
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
        let triangles = meshes::triangle_count(&decoded);
        let nodes = model.nodes.len();
        let meshes = model.meshes.len();

        let mut viewer = ViewerScene::new(&model, &decoded);
        // One directional light plus the window's ambient term is enough to read
        // the shape; the materials carry the colours the file declares.
        viewer
            .root_mut()
            .add_light(Light::directional(Vec3::new(0.35, 0.5, -1.0).normalize()));

        self.path = Some(path.to_path_buf());
        self.model = Some(model);
        self.decoded = decoded;
        self.viewer = viewer;
        self.filter.clear();
        self.renaming = None;
        self.status = format!(
            "{} · {nodes} nodes · {meshes} meshes · {triangles} triangles",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        );

        fit(camera, &self.viewer, self.viewport);
    }
}

/// Frame the whole scene: focus on its centre and back off until it fits.
///
/// The projection always spans the **whole window** — kiss3d has no viewport
/// rectangle — so the outliner and toolbar are simply painted over the image.
/// `fit` therefore sizes the model against the free rectangle and pushes the
/// focus point sideways to move the model out from under the outliner.
fn fit(camera: &mut OrbitCamera3d, viewer: &ViewerScene, vp: Viewport) {
    let Some((min, max)) = viewer.world_bounds_all() else {
        return;
    };
    let center = (min + max) * 0.5;
    let radius = ((max - min).length() * 0.5).max(1.0e-3);

    // The orbit camera clamps `dist` to [min_dist, max_dist], so widen those
    // before moving the camera — a 2 m bracket and a 2 km frame share this code.
    camera.set_min_dist(radius * 1.0e-4);
    camera.set_max_dist(radius * 1.0e4);
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
        let up = view.inverse_transform_vector(Vec3::Y);
        camera.set_at(center - right * (dx * per_point) + up * (dy * per_point));
    }
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
    /// `window`.
    fn of(window: &Window) -> Self {
        let size = window.size();
        let ppp = window.scale_factor() as f32;
        let win_w = size.x as f32 / ppp;
        let win_h = size.y as f32 / ppp;
        Self {
            left: tree::WIDTH,
            top: TOOLBAR,
            width: (win_w - tree::WIDTH).max(1.0),
            height: (win_h - TOOLBAR).max(1.0),
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

    let mut camera = OrbitCamera3d::new_with_frustum(
        std::f32::consts::FRAC_PI_4,
        1.0e-3,
        1.0e6,
        Vec3::new(3.0, 3.0, 3.0),
        Vec3::ZERO,
    );
    camera.set_up_axis(Vec3::Z);

    // The first `fit` happens inside `load`, so the viewport has to be known
    // before it.
    let mut app = App {
        viewport: Viewport::of(&window),
        ..App::default()
    };
    if let Some(arg) = std::env::args().nth(1) {
        app.load(Path::new(&arg), &mut camera);
    }

    while window.render_3d(app.viewer.root_mut(), &mut camera).await {
        app.viewport = Viewport::of(&window);

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

        // --- egui: toolbar, outliner, trihedron ---------------------------
        let mut commands = Commands::default();
        window.draw_ui(|ctx| {
            let screen = ctx.input(|i| i.raw.screen_rect.unwrap_or_else(|| i.viewport_rect()));
            let style = ctx.style_of(ctx.theme());
            egui::Window::new("ferrojt.toolbar")
                .title_bar(false)
                .fixed_pos(screen.min)
                .fixed_size(vec2(screen.width(), TOOLBAR))
                .movable(false)
                .resizable(false)
                .collapsible(false)
                .order(egui::Order::Middle)
                .frame(
                    Frame::new()
                        .inner_margin(Margin::symmetric(8, 4))
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
                        ui.separator();
                        ui.label(RichText::new(&app.status).weak());
                    });
                });

            let outliner = Rect::from_min_max(
                Pos2::new(screen.min.x, screen.min.y + TOOLBAR),
                Pos2::new(screen.min.x + tree::WIDTH, screen.max.y),
            );
            tree::show(
                ctx,
                &mut app.viewer,
                outliner,
                &mut app.filter,
                &mut app.renaming,
            );

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
            fit(&mut camera, &app.viewer, app.viewport);
        }
        if commands.open {
            if let Some(path) = pick_file() {
                app.load(&path, &mut camera);
            }
        }
        if commands.exit {
            window.close();
        }
    }
}

/// Native file dialog for the toolbar button.
fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open a JT file")
        .add_filter("JT", &["jt"])
        .pick_file()
}
