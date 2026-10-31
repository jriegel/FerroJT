//! The viewport backdrop: a vertical colour gradient behind the model.
//!
//! Kiss3d has no gradient background. [`Window::set_background_color`]
//! (kiss3d::window::Window) is a single clear colour, the skybox is an
//! equirectangular panorama whose image rows follow **world** Y — so with the
//! `+Z`-up axis a JT model uses it would render as a rotating horizontal band,
//! never a screen-vertical gradient — and setting a skybox also forces
//! image-based lighting on every material, which there is no way to switch off.
//! A post-processing effect is no better: the ping-pong targets it draws into
//! carry their own depth buffer, not the scene's, so it cannot restrict itself
//! to background pixels and would tint the model too.
//!
//! So the gradient is real geometry: a quad parked at the far end of the
//! frustum, billboarded to face the eye, and shaded entirely by an emissive map
//! generated in memory. Black albedo removes the ambient term, light layer `0`
//! removes every analytic light and its specular, and `set_casts_shadows(false)`
//! keeps it out of the shadow map — what is left is exactly the texture, so the
//! two endpoints below are the two endpoints of the picture.
//!
//! Being a plain scene node it is invisible to the parts of the viewer that
//! walk the row tree: it is not an outliner row, it is not picked
//! ([`crate::picking`]), it is not an LOD candidate ([`crate::lod`]), and it is
//! not in [`ViewerScene::world_bounds_all`](crate::scene::ViewerScene), so it
//! never inflates the camera fit.

use std::cell::RefCell;
use std::rc::Rc;

use image::{DynamicImage, RgbaImage};
use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::color::Color;
use kiss3d::prelude::{Pose3, Vec2, Vec3};
use kiss3d::resource::{GpuMesh3d, TextureManager};
use kiss3d::scene::SceneNode3d;

/// Name the gradient is registered under in the global texture manager.
///
/// The manager caches by name, so re-creating the backdrop for each loaded file
/// reuses one texture.
const TEXTURE: &str = "ferrojt.backdrop";

/// Side of the gradient texture, in pixels.
///
/// The gradient is a smooth ramp, so this only has to be fine enough that
/// bilinear filtering cannot show a step; 8 keeps the texture at 256 bytes.
const TEXTURE_SIZE: u32 = 8;

/// Top of the gradient — the sky end — as sRGB bytes.
///
/// The texture is uploaded as `Rgba8UnormSrgb`, so these are display-referred
/// values: they are decoded to linear before shading and re-encoded on the way
/// to the screen. They are still not the picture, though, because kiss3d shades
/// into an HDR target and tone-maps it (`Window::set_hdr_settings` is never
/// called here), which costs about twenty levels at default exposure. Both
/// endpoints were therefore chosen by measuring a screenshot, not by matching an
/// intent: these give roughly `(110, 117, 131)` at the top of the window and
/// `(45, 50, 60)` at the bottom.
///
/// The shape follows the JT Assistant backdrop (`bgShader.vert`: `0.91, 0.93,
/// 0.94` down to `0.62, 0.64, 0.67`), darkened because this UI is dark.
const TOP: [u8; 3] = [132, 138, 150];

/// Bottom of the gradient — the ground end — as sRGB bytes.
const BOTTOM: [u8; 3] = [66, 70, 80];

/// Distance of the quad from the eye, as a share of the far clip plane.
///
/// It has to sit behind everything the model can show, and the only distance
/// that is guaranteed to be beyond the scene from every angle is the far plane:
/// orbiting pulls the eye back but never past it, and zooming in leaves the
/// model straddling the camera. A share of it keeps the quad inside the
/// frustum, where kiss3d's bounding-sphere culling still sees it.
const DEPTH: f32 = 0.9;

/// How much of the frustum the quad covers, as a multiple of the exact size.
///
/// A billboard exactly as large as the frustum section covers every pixel only
/// up to floating-point rounding; the margin hides the seam that would otherwise
/// show as a hairline of clear colour along the window edges.
const COVER: f32 = 1.05;

/// The gradient quad, attached to the scene root of the scene it belongs to.
pub struct Backdrop {
    node: SceneNode3d,
}

impl Backdrop {
    /// Add a backdrop to `root`.
    pub fn new(root: &mut SceneNode3d) -> Self {
        // A unit quad in its local XY plane. The UVs put v = 0 — the first row
        // of the generated image, i.e. `TOP` — at local +Y, which `place` turns
        // into screen up.
        let mesh = Rc::new(RefCell::new(GpuMesh3d::new(
            vec![
                Vec3::new(-0.5, -0.5, 0.0),
                Vec3::new(0.5, -0.5, 0.0),
                Vec3::new(0.5, 0.5, 0.0),
                Vec3::new(-0.5, 0.5, 0.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
            Some(vec![Vec3::Z; 4]),
            Some(vec![
                Vec2::new(0.0, 1.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, 0.0),
            ]),
            false,
        )));

        let mut node = SceneNode3d::mesh(mesh, Vec3::ONE);
        node.set_color(Color::new(0.0, 0.0, 0.0, 1.0));
        node.set_light_layers(0);
        node.set_casts_shadows(false);
        node.enable_backface_culling(false);
        node.set_emissive(Color::new(1.0, 1.0, 1.0, 1.0));
        let texture = TextureManager::get_global_manager(|tm| tm.add_image(gradient(), TEXTURE));
        node.set_emissive_map(texture);

        root.add_child(node.clone());
        Self { node }
    }

    /// Park the quad at the far end of the frustum and turn it to face the eye.
    ///
    /// `aspect` is the ratio of the whole window, not of the free area: the
    /// projection always spans the full canvas, so the gradient has to as well
    /// and the panels merely cover part of it.
    pub fn place(&mut self, camera: &OrbitCamera3d, aspect: f32) {
        let eye = camera.eye();
        let (near, far) = camera.clip_planes();
        let distance = (far * DEPTH).max(near * 1.05).max(f32::EPSILON);

        // `fov` is the vertical angle over the whole window, so the frustum
        // section at `distance` is `2 · d · tan(fov / 2)` tall and that times
        // the aspect ratio wide.
        let half_tan = (camera.fov() * 0.5).tan();
        let height = 2.0 * distance * half_tan * COVER;
        let width = height * aspect.max(f32::EPSILON);

        // [`OrbitCamera3d::view_transform`] is the **view** pose — it maps world
        // directions into view space, where the camera looks down `-Z`. So the
        // direction the camera looks, in world terms, is the inverse rotation
        // applied to `-Z`, and the inverse rotation itself is the local → world
        // rotation that lays the quad across the frustum with its `+Z` normal
        // back out of the screen towards the eye and its `+Y` along screen up.
        let view = camera.view_transform();
        let rotation = view.rotation.inverse();
        let position = eye + view.inverse_transform_vector(Vec3::NEG_Z) * distance;

        self.node.set_pose(Pose3::from_parts(position, rotation));
        self.node.set_local_scale(width, height, 1.0);
    }
}

/// The gradient, top row first: `TOP` at v = 0 down to `BOTTOM` at v = 1.
fn gradient() -> DynamicImage {
    let mut image = RgbaImage::new(TEXTURE_SIZE, TEXTURE_SIZE);
    let last = (TEXTURE_SIZE - 1) as f32;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let _ = x;
        let t = y as f32 / last;
        let channel = |top: u8, bottom: u8| (top as f32 + (bottom as f32 - top as f32) * t) as u8;
        *pixel = image::Rgba([
            channel(TOP[0], BOTTOM[0]),
            channel(TOP[1], BOTTOM[1]),
            channel(TOP[2], BOTTOM[2]),
            255,
        ]);
    }
    DynamicImage::ImageRgba8(image)
}
