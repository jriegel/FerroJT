//! The rotation trihedron in the upper-left corner of the viewport.
//!
//! It is drawn with the egui painter rather than as 3D geometry: the three model
//! axes are pushed through the camera's rotation, and the three pointing closest
//! to the eye are painted as arrows. Dragging anywhere inside the gizmo applies
//! exactly the same yaw / pitch increments the orbit camera uses for a left-drag
//! in the viewport, so the two feel identical; clicking the centre ball cycles
//! the standard front / top / right views.

use egui::{Align2, Area, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, Vec2, vec2};
use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::prelude::{Rot3, Vec3};

/// Half the size of the gizmo box, in logical points.
const BOX: Vec2 = vec2(112.0, 112.0);
/// Axis length in points.
const RADIUS: f32 = 40.0;
/// Radius of the clickable centre ball.
const BALL: f32 = 13.0;
/// Same as [`OrbitCamera3d`]'s default `yaw_step` / `pitch_step`.
const STEP: f32 = 0.005;

/// Standard views: the direction the *eye* sits in, in model space.
const VIEWS: [(&str, Vec3); 6] = [
    ("Front", Vec3::new(0.0, -1.0, 0.0)),
    ("Right", Vec3::new(1.0, 0.0, 0.0)),
    ("Top", Vec3::new(0.0, 0.0, 1.0)),
    ("Back", Vec3::new(0.0, 1.0, 0.0)),
    ("Left", Vec3::new(-1.0, 0.0, 0.0)),
    ("Bottom", Vec3::new(0.0, 0.0, -1.0)),
];

/// Paint the trihedron with its top-left corner at `origin` and drive `camera`
/// from the pointer.
pub fn show(ctx: &egui::Context, camera: &mut OrbitCamera3d, up: Vec3, origin: Pos2) {
    let inner = Area::new(Id::new("ferrojt.trihedron"))
        .fixed_pos(origin)
        .default_size(BOX)
        .movable(false)
        .constrain(false)
        .sense(Sense::click_and_drag())
        .show(ctx, |ui| {
            ui.set_min_size(BOX);
            let center = ui.cursor().min + BOX * 0.5;
            draw(ui, center, camera);
            let ball = ui.interact(
                Rect::from_center_size(center, Vec2::splat(2.0 * BALL)),
                Id::new("ferrojt.trihedron.ball"),
                Sense::click(),
            );
            let clicked = ball.clicked();
            ball.on_hover_text("Cycle the standard views")
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            clicked
        });

    // A drag inside the gizmo box rotates the camera exactly like a left-drag in
    // the viewport: egui reports points, the camera works in physical pixels.
    let response = inner.response;
    if response.dragged() {
        let delta = response.drag_delta() * ctx.pixels_per_point();
        camera.set_yaw(camera.yaw() + delta.x * STEP);
        camera.set_pitch(camera.pitch() - delta.y * STEP);
    }

    if inner.inner {
        let next = next_view(camera);
        aim(camera, up, VIEWS[next].1);
    }
}

/// Paint the axes; `center` is the gizmo's centre in egui points.
fn draw(ui: &mut egui::Ui, center: Pos2, camera: &OrbitCamera3d) {
    let axes = [
        ('X', Vec3::X, Color32::from_rgb(226, 74, 74)),
        ('Y', Vec3::Y, Color32::from_rgb(74, 190, 96)),
        ('Z', Vec3::Z, Color32::from_rgb(86, 140, 240)),
    ];

    // View space has +Y up and the eye looking down -Z, so an axis with a larger
    // view-space Z is nearer to the viewer. egui's Y grows downwards, hence the
    // sign flip when turning a view-space direction into a screen offset.
    let rotation = camera.view_transform().rotation;
    let mut ends: Vec<(f32, Vec2, char, Color32)> = axes
        .iter()
        .map(|(letter, axis, color)| {
            let view = rotation * *axis;
            (view.z, vec2(view.x, -view.y), *letter, *color)
        })
        .collect();
    ends.sort_by(|a, b| a.0.total_cmp(&b.0));

    let painter = ui.painter();
    // The soft disc marks the gizmo as an overlay and makes the drag target
    // visible without hiding the model behind it.
    painter.circle_filled(center, RADIUS + 8.0, Color32::from_black_alpha(70));

    for (depth, offset, letter, color) in &ends {
        let tip = center + *offset * RADIUS;
        let toward = *depth >= 0.0;
        let stroke = if toward {
            Stroke::new(2.2, *color)
        } else {
            Stroke::new(1.2, color.gamma_multiply(0.45))
        };
        painter.line_segment([center, tip], stroke);
        if !toward {
            continue;
        }
        painter.circle_filled(tip, 6.5, *color);
        let dir = if offset.length_sq() > 0.0 {
            *offset / offset.length()
        } else {
            vec2(0.0, -1.0)
        };
        let label = center + dir * (RADIUS + 12.0);
        painter.text(
            label,
            Align2::CENTER_CENTER,
            letter.to_string(),
            FontId::proportional(13.0),
            *color,
        );
    }

    // The ball sits on top of everything the axes draw through it.
    let nearest = ends.last().map(|e| e.2).unwrap_or(' ');
    painter.circle_filled(center, BALL, Color32::from_gray(38));
    painter.circle_stroke(center, BALL, Stroke::new(1.0, Color32::from_gray(120)));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        nearest.to_string(),
        FontId::proportional(11.0),
        Color32::from_gray(200),
    );
}

/// Which preset comes next, based on where the eye currently sits.
fn next_view(camera: &OrbitCamera3d) -> usize {
    let eye = camera.eye() - camera.at();
    let mut best = 0;
    let mut best_dot = f32::MIN;
    for (index, (_, dir)) in VIEWS.iter().enumerate() {
        let dot = eye.dot(*dir);
        if dot > best_dot {
            best_dot = dot;
            best = index;
        }
    }
    (best + 1) % VIEWS.len()
}

/// Put the eye on the `dir` side of the focus point, keeping `up` upright.
///
/// [`OrbitCamera3d`] stores yaw / pitch in the *view* frame (the one where `up`
/// has been rotated onto +Y), so the model-space direction has to go through the
/// same rotation before it can become angles — the inverse of its own
/// `look_at` / `eye` maths.
fn aim(camera: &mut OrbitCamera3d, up: Vec3, dir: Vec3) {
    let to_y_up = Rot3::from_rotation_arc(up.normalize(), Vec3::Y);
    let view = to_y_up * dir.normalize();
    let pitch = (view.y / view.length()).clamp(-1.0, 1.0).acos();
    let yaw = view.z.atan2(view.x);
    camera.set_yaw(yaw);
    camera.set_pitch(pitch);
}
