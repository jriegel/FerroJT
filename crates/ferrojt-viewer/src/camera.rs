//! The orbit camera, plus the one per-frame hook the backdrop needs.
//!
//! Kiss3d drives a frame in a fixed order: the window events are handed to
//! [`Camera3d::handle_event`], then [`Camera3d::update`] runs, and only after
//! that is the scene prepared and drawn — `camera.update(&self.canvas)` at
//! `window/rendering.rs:240`, `scene.data_mut().prepare(..)` at `:597`, both
//! inside the same `render_single_frame`. The camera is therefore final from
//! `update` onwards, and `prepare` is where every node's world transform is
//! propagated into the uniforms that get uploaded.
//!
//! That ordering is why the backdrop cannot be placed from the application
//! loop. Code written between two `render_3d` calls runs *after* the previous
//! frame was submitted, so it sees the camera as it was then and the gradient
//! is re-placed one frame late: while the pointer orbits the model the quad
//! lags a frame behind the eye and the gradient visibly swims, then snaps back
//! once the drag stops. There is no public pre-render hook, so the viewer wraps
//! the camera and does the work in `update`.

use std::ops::{Deref, DerefMut};

use kiss3d::camera::{Camera3d, OrbitCamera3d};
use kiss3d::event::{MouseButton, WindowEvent};
use kiss3d::glamx::Mat4;
use kiss3d::prelude::{Pose3, Vec3};
use kiss3d::window::Canvas;

use crate::backdrop::Backdrop;

/// The viewport camera: an [`OrbitCamera3d`] that also keeps the gradient
/// pinned to the frustum.
///
/// Everything else passes straight through — the viewer keeps calling
/// `set_at`, `set_dist`, `fov` and the rest on the wrapped camera through
/// [`Deref`].
pub struct ViewerCamera {
    orbit: OrbitCamera3d,
    /// The gradient to re-place each frame, `None` while no file is loaded.
    backdrop: Option<Backdrop>,
}

impl ViewerCamera {
    /// Wrap `orbit`.
    pub fn new(orbit: OrbitCamera3d) -> Self {
        Self {
            orbit,
            backdrop: None,
        }
    }

    /// The gradient that gets pinned to the frustum, or `None` for none.
    ///
    /// The backdrop belongs to the scene of one loaded file, so this is called
    /// once per load; the previous one goes with the scene it was built for.
    pub fn set_backdrop(&mut self, backdrop: Option<Backdrop>) {
        self.backdrop = backdrop;
    }

    /// The wrapped camera, mutably — [`fit`](crate::fit) replaces it wholesale
    /// to re-fit the clip planes, and assigning through [`DerefMut`] would also
    /// replace the backdrop slot.
    pub fn inner_mut(&mut self) -> &mut OrbitCamera3d {
        &mut self.orbit
    }

    /// Point the pan at the middle mouse button, see [`rebind_pan`].
    pub fn rebind_pan(&mut self) {
        rebind_pan(&mut self.orbit);
    }
}

/// Move the built-in pan from the right to the **middle** mouse button.
///
/// Kiss3d's orbit camera pans on [`kiss3d::event::MouseButton::Button2`],
/// which is the right button — and the right button is the one a context menu
/// needs. The middle (wheel) button has no built-in binding at all, so moving
/// the pan there costs nothing: `Canvas::get_mouse_button` tracks every button
/// (`wgpu_canvas.rs:1359`), and the pan fires from the `CursorPos` handler
/// whenever `drag_button` is held (`orbit3d.rs:549`), so no custom event code is
/// involved. Rotation stays on the left button.
///
/// This has to be re-applied whenever the camera is **rebuilt**: [`fit`](crate::fit)
/// replaces it through [`OrbitCamera3d::new_with_frustum`] to re-fit the clip
/// planes, and a fresh camera starts with the default `Button2` again.
pub fn rebind_pan(camera: &mut OrbitCamera3d) {
    camera.rebind_drag_button(Some(MouseButton::Button3));
}

impl Deref for ViewerCamera {
    type Target = OrbitCamera3d;

    fn deref(&self) -> &Self::Target {
        &self.orbit
    }
}

impl DerefMut for ViewerCamera {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.orbit
    }
}

/// The camera kiss3d renders with.
///
/// Every method is delegated except [`Camera3d::update`]; the defaults
/// (`project`, `unproject`, `num_passes`, `start_pass`, `render_complete`) go
/// through the delegated `transformation`, so they behave exactly as they would
/// on the wrapped camera.
impl Camera3d for ViewerCamera {
    fn handle_event(&mut self, canvas: &Canvas, event: &WindowEvent) {
        self.orbit.handle_event(canvas, event);
    }

    fn eye(&self) -> Vec3 {
        self.orbit.eye()
    }

    fn view_transform(&self) -> Pose3 {
        self.orbit.view_transform()
    }

    fn transformation(&self) -> Mat4 {
        self.orbit.transformation()
    }

    fn inverse_transformation(&self) -> Mat4 {
        self.orbit.inverse_transformation()
    }

    fn clip_planes(&self) -> (f32, f32) {
        self.orbit.clip_planes()
    }

    fn update(&mut self, canvas: &Canvas) {
        self.orbit.update(canvas);
        // The camera is final here and the scene has not been prepared yet, so
        // the quad is placed against the same pose the frame is drawn with.
        // `aspect` is the whole canvas, not the free area: the projection spans
        // it and the panels only cover part of the picture.
        if let Some(backdrop) = self.backdrop.as_mut() {
            let (width, height) = canvas.size();
            let aspect = width as f32 / (height.max(1)) as f32;
            backdrop.place(&self.orbit, aspect);
        }
    }

    fn view_transform_pair(&self, pass: usize) -> (Pose3, Mat4) {
        self.orbit.view_transform_pair(pass)
    }

    fn render_layers(&self) -> u32 {
        self.orbit.render_layers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiss3d::prelude::Vec2;

    fn orbit() -> OrbitCamera3d {
        OrbitCamera3d::new_with_frustum(
            std::f32::consts::FRAC_PI_4,
            0.25,
            250.0,
            Vec3::new(3.0, -4.0, 5.0),
            Vec3::new(1.0, 1.0, 2.0),
        )
    }

    /// The viewer keeps reading the camera through the trait — picking, the
    /// level-of-detail projection and the trihedron all do — so a method that
    /// was not delegated would silently answer with something else.
    #[test]
    fn the_wrapper_answers_for_the_camera_it_wraps() {
        let inner = orbit();
        let wrapper = ViewerCamera::new(orbit());
        assert_eq!(wrapper.eye(), inner.eye());
        assert_eq!(wrapper.view_transform(), inner.view_transform());
        assert_eq!(wrapper.transformation(), inner.transformation());
        assert_eq!(
            wrapper.inverse_transformation(),
            inner.inverse_transformation()
        );
        assert_eq!(wrapper.clip_planes(), inner.clip_planes());
        assert_eq!(wrapper.view_transform_pair(0), inner.view_transform_pair(0));
        assert_eq!(wrapper.num_passes(), inner.num_passes());
        assert_eq!(wrapper.render_layers(), inner.render_layers());
        // The default `project` runs on the delegated `transformation`, so it
        // has to agree with the wrapped camera's.
        let point = Vec3::new(2.0, 3.0, 4.0);
        let size = Vec2::new(800.0, 600.0);
        assert_eq!(wrapper.project(point, size), inner.project(point, size));
    }

    /// `fit` replaces the camera wholesale; the backdrop belongs to the scene and
    /// has to survive that, and the new frustum has to be the one everything
    /// else reads back through the wrapper.
    #[test]
    fn replacing_the_camera_keeps_the_backdrop_slot() {
        let mut camera = ViewerCamera::new(OrbitCamera3d::new_with_frustum(
            std::f32::consts::FRAC_PI_4,
            0.1,
            1.0e3,
            Vec3::new(3.0, 3.0, 3.0),
            Vec3::ZERO,
        ));
        // No file is loaded, so nothing is placed — and nothing panics either.
        camera.set_backdrop(None);
        *camera.inner_mut() = orbit();
        let fresh = orbit();
        assert_eq!(camera.eye(), fresh.eye());
        assert_eq!(camera.clip_planes(), fresh.clip_planes());
        // A rebuilt camera is a fresh one, bindings included — which is exactly
        // why `fit` has to re-apply the pan.
        rebind_pan(camera.inner_mut());
        assert_eq!(camera.drag_button(), Some(MouseButton::Button3));
    }

    /// The right button has to be free for the context menu, so the pan lives on
    /// the middle button and rotation stays where it was.
    #[test]
    fn the_middle_button_pans_and_the_right_one_is_free() {
        let mut camera = ViewerCamera::new(orbit());
        assert_eq!(
            camera.drag_button(),
            Some(MouseButton::Button2),
            "kiss3d pans on the right button until told otherwise"
        );
        camera.rebind_pan();
        assert_eq!(camera.drag_button(), Some(MouseButton::Button3));
        assert_eq!(camera.rotate_button(), Some(MouseButton::Button1));
    }
}
