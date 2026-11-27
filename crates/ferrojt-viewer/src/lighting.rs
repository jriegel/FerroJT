//! The viewer's light rig: three lamps that travel with the camera.
//!
//! Kiss3d shades with a real PBR model (GGX + Smith + Schlick, see
//! `builtin/default.wgsl`), but its ambient term is a **flat** multiplier
//! unless a skybox is set — and a skybox would bring image-based lighting and
//! the equirectangular panorama problem the backdrop already works around
//! ([`crate::backdrop`]). So every highlight has to come from analytic lights.
//! A single directional light with the default intensity does two things badly:
//! the side of the model that turns away from it falls back to the flat ambient
//! and reads as a matte wash, and there is almost no specular response anywhere,
//! which is why a glossy CAD shell looks like plaster next to a CAD viewer that
//! lights the same file from several sides.
//!
//! The rig is therefore camera-relative: the lamps keep a fixed position in
//! **view** space, so the model stays lit while it is orbited, exactly like the
//! head-light of a CAD viewer. That is possible because a directional light's
//! world direction *is* rotated by its node's transform
//! (`world_direction = world_rotation * local_direction` at
//! `scene/scene_node3d.rs:150-172`), and because `Camera3d::update` runs before
//! the scene is prepared (`window/rendering.rs:240` before `:597`), the same
//! frame the backdrop is placed in — see [`crate::camera`].
//!
//! The lamps cast **no** shadows. Real-time shadow mapping is on by default in
//! kiss3d, and a directional light shadowing a dense CAD shell against itself
//! from a fixed 2048 px atlas produces depth-acne mottling over the whole part —
//! the classic "the surface is covered in patches" artifact. Turning it off also
//! skips a full pre-pass over every triangle each frame.

use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::color::Color;
use kiss3d::light::Light;
#[cfg(test)]
use kiss3d::prelude::Quat;
use kiss3d::prelude::{Pose3, Vec3};
use kiss3d::scene::SceneNode3d;

/// One lamp of the rig.
///
/// `travel` is the direction the light travels **in view space** — from the
/// lamp towards the model, so the lamp itself sits at `-travel`. It is held as
/// a plain direction and normalised where it is applied. The camera
/// looks down `-Z`, with `+X` to the right and `+Y` up, which is what makes the
/// signs below readable: a negative `z` puts the lamp on the camera side of the
/// model, a negative `y` puts it above.
struct Lamp {
    travel: Vec3,
    color: [f32; 3],
    intensity: f32,
}

/// The three lamps: a key from the upper right, a cool fill from the left, and
/// a near-vertical top light for the horizontal faces a machined part is full
/// of. Their intensities sum to a little over the single light the viewer used
/// to carry, while the window's ambient went down — contrast, not brightness.
const LAMPS: [Lamp; 3] = [
    // Key: upper right, in front of the model.
    Lamp {
        travel: Vec3::new(-0.40, -0.60, -0.70),
        color: [1.0, 1.0, 1.0],
        intensity: 1.9,
    }, // Fill: low left, cool and weak, so the shaded side keeps its colour
    // instead of falling to the flat ambient.
    Lamp {
        travel: Vec3::new(0.75, -0.15, -0.55),
        color: [0.62, 0.70, 0.82],
        intensity: 0.8,
    },
    // Top: straight down, which is what puts a readable gradient on flanges,
    // webs and any other large horizontal face.
    Lamp {
        travel: Vec3::new(0.05, -0.95, -0.25),
        color: [0.95, 0.95, 0.98],
        intensity: 0.8,
    },
];

/// The lamps, attached to the scene root of the loaded file and turned by the
/// camera every frame.
///
/// Like the backdrop this is a plain scene node, so it is not an outliner row,
/// not picked, not an LOD candidate and not part of the scene bounds. A light
/// node carries no geometry at all ([`SceneNode3d::new_light`] is an empty
/// node), so it cannot be drawn by accident.
pub struct LightRig {
    nodes: Vec<SceneNode3d>,
}

impl LightRig {
    /// Add the rig to `root`.
    pub fn attach(root: &mut SceneNode3d) -> Self {
        let nodes = LAMPS
            .iter()
            .map(|lamp| {
                root.add_light(
                    Light::directional(lamp.travel.normalize())
                        .with_color(Color::new(lamp.color[0], lamp.color[1], lamp.color[2], 1.0))
                        .with_intensity(lamp.intensity)
                        .with_casts_shadows(false),
                )
            })
            .collect();
        Self { nodes }
    }

    /// Turn the lamps so they keep their place relative to the eye.
    ///
    /// The root of a kiss3d scene is the identity, so the **local** pose of a
    /// node is its world pose; giving every lamp the camera's local→world
    /// rotation puts the whole rig back into world space with the view space it
    /// was designed in. The position of a directional light is irrelevant, only
    /// its rotation is read.
    pub fn place(&mut self, camera: &OrbitCamera3d) {
        // `view_transform` maps world into view, so its inverse is the
        // local→world rotation — the same reading [`crate::backdrop`] uses.
        let to_world = camera.view_transform().rotation.inverse();
        for node in &mut self.nodes {
            node.set_pose(Pose3::from_parts(Vec3::ZERO, to_world));
        }
    }

    /// The lamps' travel directions in world space for a camera whose
    /// local→world rotation is `to_world`.
    ///
    /// Split out from [`LightRig::place`] because a light node needs a live
    /// kiss3d window; this is the part worth pinning down in a test, so the
    /// helper only exists for the tests of this module.
    #[cfg(test)]
    pub fn world_directions(to_world: Quat) -> Vec<Vec3> {
        LAMPS
            .iter()
            .map(|lamp| (to_world * lamp.travel).normalize())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) {
        assert!((a - b).length() < 1.0e-6, "{a} != {b}");
    }

    /// Kiss3d keeps at most eight lights per frame; the rig must fit, and the
    /// ambient plus three analytic lamps is the whole shading model.
    #[test]
    fn the_rig_fits_inside_the_light_budget() {
        assert!(LAMPS.len() <= 8, "{} lamps", LAMPS.len());
        assert!(!LAMPS.is_empty());
    }

    /// Every lamp has to be unit length once applied and sit on the camera side
    /// of the model; a lamp behind the part lights faces the user cannot see.
    #[test]
    fn every_lamp_points_from_the_camera_side_of_the_model() {
        for direction in LightRig::world_directions(Quat::IDENTITY) {
            assert!((direction.length() - 1.0).abs() < 1.0e-6, "{direction}");
            // Negative `z` in view space = the lamp is on the camera side, so
            // the light reaches the faces the user is actually looking at.
            assert!(direction.z < 0.0, "{direction} travels away");
        }
        for lamp in LAMPS.iter() {
            assert!(lamp.intensity > 0.0);
        }
    }

    /// The point of the rig: a lamp keeps its angle to the eye however the
    /// model is orbited, so the part never goes dark on one side.
    #[test]
    fn the_lamps_keep_their_place_relative_to_the_eye() {
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            for angle in [0.0f32, 0.7, std::f32::consts::FRAC_PI_2, 2.4] {
                let to_world = Quat::from_axis_angle(axis, angle);
                // The direction the camera looks, in world space.
                let forward = to_world * Vec3::NEG_Z;
                for (travelled, lamp) in LightRig::world_directions(to_world).iter().zip(LAMPS) {
                    // The same angle measured in view space, where it is fixed
                    // by design: the camera looks down -Z.
                    let wanted = -lamp.travel.normalize().z;
                    let got = forward.dot(*travelled);
                    assert!(
                        (wanted - got).abs() < 1.0e-5,
                        "{axis} at {angle}: {got} != {wanted}"
                    );
                }
            }
        }
    }

    /// The key has to come from above and to the right of the view, and the
    /// fill from the other side — otherwise the rig is one light twice over.
    #[test]
    fn the_key_and_the_fill_cross_the_model_from_opposite_sides() {
        let dirs = LightRig::world_directions(Quat::IDENTITY);
        close(dirs[0], LAMPS[0].travel.normalize());
        assert!(dirs[0].x < 0.0, "the key belongs on the right");
        assert!(dirs[0].y < 0.0, "the key belongs above");
        assert!(dirs[1].x > 0.0, "the fill belongs on the left");
        // And they are not the same lamp: the angle between them is wide.
        let spread = dirs[0].dot(dirs[1]);
        assert!(spread < 0.5, "the lamps are too close together: {spread}");
    }
}
