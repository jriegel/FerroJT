//! Screen-size driven level of detail.
//!
//! A JT range LOD lists the same part from the finest to the coarsest tier, and
//! a model loaded with
//! [`JtFile::to_model_with_all_lods`](ferrojt::jt::JtFile::to_model_with_all_lods)
//! keeps all of them. Showing every tier at once is wrong — the tiers describe
//! the same shape, so their faces land on top of each other and fight for the
//! depth test — and always showing the finest one wastes triangles on a part
//! that covers a handful of pixels. This module picks one tier per stack per
//! frame, from the area the part's box covers on screen.
//!
//! The rule is a triangle budget: a triangle smaller than a pixel cannot add
//! detail, so a stack keeps the coarsest tier that still paints about
//! [`TRIANGLES_PER_PIXEL`] triangles per pixel of its projected box. The check
//! only ever moves one tier per frame and asks for a margin before going
//! coarser, which is what stops a stack from flipping back and forth when the
//! camera sits on a threshold.

use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::glamx::Vec4Swizzles;
use kiss3d::prelude::{Vec2, Vec3};

use crate::scene::ViewerScene;
use crate::scene::box_corners;

/// Triangles a stack should still have per pixel of its projected box.
///
/// Below one triangle per pixel the extra geometry is sub-pixel and only costs
/// fill and setup. `1.0` is the point where a mesh is exactly resolved; a CAD
/// viewer wants a little more than that for silhouettes, so the value sits
/// slightly above one.
const TRIANGLES_PER_PIXEL: f32 = 1.5;

/// Margin a coarser tier needs before the stack drops to it.
///
/// Without it, a tier that is exactly at the budget flips to the next one and
/// back as soon as the camera moves a pixel.
const HYSTERESIS: f32 = 1.6;

/// Refresh the tier of every level-of-detail stack for the current camera.
///
/// `size` is the window size in physical pixels, the same space
/// [`Camera3d::project`] works in.
pub fn select(camera: &OrbitCamera3d, scene: &mut ViewerScene, size: Vec2) {
    if !scene.auto_lod() {
        return;
    }

    for group in 0..scene.lod_groups().len() {
        let Some((min, max)) = scene.lod_bounds(group) else {
            continue;
        };
        let Some(pixels) = projected_area(camera, min, max, size) else {
            // The box is behind the camera or off to the side: nothing of it is
            // on screen, so the coarsest tier is the right answer.
            scene.set_lod_tier(group, usize::MAX);
            continue;
        };

        let tiers = scene.tier_triangles(group);
        let current = scene.lod_tier(group).unwrap_or(0);
        let budget = pixels * TRIANGLES_PER_PIXEL;
        let next = step(tiers, current, budget);
        if next != current {
            scene.set_lod_tier(group, next);
        }
    }
}

/// The next tier to try, at most one step away from `current`.
fn step(triangles: &[usize], current: usize, budget: f32) -> usize {
    if triangles.is_empty() {
        return 0;
    }
    let current = current.min(triangles.len() - 1);

    // Too few triangles for the pixels it covers: one step finer.
    if (triangles[current] as f32) < budget && current > 0 {
        return current - 1;
    }

    // Enough triangles, and the next coarser tier still has margin: one step
    // coarser. The margin is what makes the switch stick.
    if let Some(&coarser) = triangles.get(current + 1) {
        if (coarser as f32) >= budget * HYSTERESIS {
            return current + 1;
        }
    }

    current
}

/// The area of the box `min .. max` on screen, in square physical pixels.
///
/// The eight corners are pushed through the projection of [`Camera3d`] and the
/// extent of the result is taken. The clip-space `w` of a corner is positive in
/// front of the eye and negative behind it, which separates the two cases that
/// make the projection meaningless: a box wholly behind the camera (`None`,
/// nothing of it can be seen) and one straddling the near plane (`f32::MAX`,
/// the camera is inside the part, so it covers the screen).
fn projected_area(camera: &OrbitCamera3d, min: Vec3, max: Vec3, size: Vec2) -> Option<f32> {
    let transformation = camera.transformation();
    let corners = box_corners(min, max);
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    let mut behind = 0usize;

    for corner in corners {
        let clip = transformation * corner.extend(1.0);
        if !clip.is_finite() {
            return None;
        }
        if clip.w <= 0.0 {
            behind += 1;
        }
        let point = clip.xyz() / clip.w;
        min_x = min_x.min((1.0 + point.x) * size.x / 2.0);
        max_x = max_x.max((1.0 + point.x) * size.x / 2.0);
        min_y = min_y.min((1.0 + point.y) * size.y / 2.0);
        max_y = max_y.max((1.0 + point.y) * size.y / 2.0);
    }

    // Wholly behind the eye: the corners mirror into the frustum and the extent
    // says nothing, so report that there is nothing to size up.
    if behind == corners.len() {
        return None;
    }
    // Straddling the near plane: the camera is inside the part and it covers
    // the whole screen.
    if behind > 0 {
        return Some(f32::MAX);
    }

    // A box far larger than the window: keep the finest tier, which is what a
    // close-up needs.
    if max_x - min_x > 4.0 * size.x || max_y - min_y > 4.0 * size.y {
        return Some(f32::MAX);
    }

    Some(((max_x - min_x) * (max_y - min_y)).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A part covering a lot of screen keeps the finest tier, one reduced to a
    /// few pixels drops to the coarsest — one step per frame, so the check is
    /// `step` rather than a search.
    #[test]
    fn a_small_projection_drops_one_tier_at_a_time() {
        // 12 / 6 / 3 triangles for the three tiers of `example_block`.
        let tiers = [12usize, 6, 3];

        // 100 x 100 px: 15 000 triangles wanted, so the finest tier stays.
        assert_eq!(step(&tiers, 0, 10_000.0 * 1.5), 0);
        // 2 x 2 px: 6 wanted. The coarser tier has no margin (6 >= 6 * 1.6 is
        // false), so it holds at the finest.
        assert_eq!(step(&tiers, 0, 4.0 * 1.5), 0);
        // 1 x 1 px: 1.5 wanted, 6 >= 2.4 — one step coarser.
        assert_eq!(step(&tiers, 0, 1.5), 1);
        // And from there one more step down.
        assert_eq!(step(&tiers, 1, 1.5), 2);
        // The coarsest tier is the floor.
        assert_eq!(step(&tiers, 2, 1.5), 2);
    }

    /// A stack never jumps to a tier more than one step away, which is what
    /// keeps a camera move from swapping the whole chain in one frame.
    #[test]
    fn it_never_skips_a_tier() {
        let tiers = [100_000usize, 10_000, 1_000, 100];
        assert_eq!(step(&tiers, 0, 1.0), 1);
        assert_eq!(step(&tiers, 3, 1.0e9), 2);
    }

    /// Going finer needs only that the current tier is under budget; going
    /// coarser needs the margin, so a tier sitting on the threshold is stable.
    #[test]
    fn the_hysteresis_stops_flapping() {
        let tiers = [100usize, 50];
        // Budget exactly at the coarser tier: no margin, so it stays fine.
        assert_eq!(step(&tiers, 0, 50.0), 0);
        // Budget below the margin: it drops.
        assert_eq!(step(&tiers, 0, 30.0), 1);
        // And at that budget the coarse tier is happy to stay.
        assert_eq!(step(&tiers, 1, 30.0), 1);
    }

    #[test]
    fn an_empty_stack_stays_at_zero() {
        assert_eq!(step(&[], 0, 10.0), 0);
        assert_eq!(step(&[7], 4, 10.0), 0);
    }

    #[test]
    fn corners_are_the_eight_box_vertices() {
        let vertices = box_corners(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 2.0, 3.0));
        let mut seen: Vec<[f32; 3]> = vertices.iter().map(|c| c.to_array()).collect();
        seen.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let expected: Vec<[f32; 3]> = [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 3.0],
            [0.0, 2.0, 0.0],
            [0.0, 2.0, 3.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 3.0],
            [1.0, 2.0, 0.0],
            [1.0, 2.0, 3.0],
        ]
        .to_vec();
        assert_eq!(seen, expected);
    }

    /// The projected area has to shrink as the camera backs off, otherwise the
    /// budget rule never fires. `project` is pure math, so a plain camera works.
    #[test]
    fn backing_off_shrinks_the_projected_area() {
        let size = Vec2::new(1_200.0, 800.0);
        let min = Vec3::new(-40.0, -30.0, -10.0);
        let max = Vec3::new(40.0, 30.0, 10.0);

        let near = OrbitCamera3d::new_with_frustum(
            std::f32::consts::FRAC_PI_4,
            0.1,
            1.0e4,
            Vec3::new(0.0, 0.0, 120.0),
            Vec3::ZERO,
        );
        let far = OrbitCamera3d::new_with_frustum(
            std::f32::consts::FRAC_PI_4,
            0.1,
            1.0e5,
            Vec3::new(0.0, 0.0, 1_200.0),
            Vec3::ZERO,
        );

        let close = projected_area(&near, min, max, size).expect("in front of the camera");
        let small = projected_area(&far, min, max, size).expect("in front of the camera");
        assert!(close > small);
        // Ten times the distance: about a hundredth of the area.
        assert!(small * 80.0 < close && small * 120.0 > close);

        // A box behind the eye has no area, which is what sends an off-screen
        // stack to its coarsest tier.
        let behind = OrbitCamera3d::new_with_frustum(
            std::f32::consts::FRAC_PI_4,
            0.1,
            1.0e4,
            Vec3::new(0.0, 0.0, 120.0),
            Vec3::new(0.0, 0.0, 400.0),
        );
        assert!(projected_area(&behind, min, max, size).is_none());
    }
}
