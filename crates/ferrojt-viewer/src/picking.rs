//! CPU picking: the closest visible outliner row under a viewport pixel.
//!
//! The scene is small enough (a few thousand rows at most) that a plain ray /
//! axis-aligned-box test is faster and far more robust than a GPU pass. Each row
//! owns an [`Aabb`](ferrojt::model::Aabb) in model space; the box corners are
//! pushed through the row's world transform and tested against the ray that
//! [`Camera3d::unproject`] gives for the cursor.

use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::prelude::{Vec2, Vec3};

use crate::scene::ViewerScene;

/// The row whose geometry the ray hits first, or `None` for empty space.
///
/// `cursor` and `size` are in the same (physical) pixels as
/// [`Window::size`](kiss3d::Window::size), which is what
/// [`Camera3d::unproject`] expects.
pub fn pick(
    camera: &OrbitCamera3d,
    scene: &ViewerScene,
    cursor: Vec2,
    size: Vec2,
) -> Option<usize> {
    let (origin, dir) = camera.unproject(cursor, size);
    if dir.length_squared() == 0.0 {
        return None;
    }

    let mut best: Option<(f32, usize)> = None;
    for id in scene.visible_rows() {
        // A row without a shape has nothing to hit, and a row hidden by its own
        // or an ancestor's eye toggle is not drawn either — `visible_rows` only
        // filters the collapsed branches of the outliner.
        if !scene.is_drawn(id) {
            continue;
        }
        let Some((min, max)) = scene.world_bounds(id) else {
            continue;
        };
        let Some(t) = slab(min, max, origin, dir) else {
            continue;
        };
        match best {
            Some((best_t, _)) if t >= best_t => {}
            _ => best = Some((t, id)),
        }
    }

    best.map(|(_, id)| id)
}

/// Ray / axis-aligned-box intersection, returning the nearest positive entry
/// distance. Boxes with an empty extent still register when the ray crosses them.
fn slab(min: Vec3, max: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    let mut tmin = 0.0f32;
    let mut tmax = f32::MAX;
    let min = [min.x, min.y, min.z];
    let max = [max.x, max.y, max.z];
    let org = [origin.x, origin.y, origin.z];
    let dir = [dir.x, dir.y, dir.z];

    for axis in 0..3 {
        if dir[axis].abs() < f32::EPSILON {
            // The ray runs parallel to this slab.
            if org[axis] < min[axis] || org[axis] > max[axis] {
                return None;
            }
            continue;
        }
        let inv = 1.0 / dir[axis];
        let mut t0 = (min[axis] - org[axis]) * inv;
        let mut t1 = (max[axis] - org[axis]) * inv;
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
        }
        tmin = tmin.max(t0);
        tmax = tmax.min(t1);
        if tmin > tmax {
            return None;
        }
    }

    Some(tmin)
}
