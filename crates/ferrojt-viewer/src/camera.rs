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
use crate::lighting::LightRig;

/// The viewport camera: an [`OrbitCamera3d`] that also keeps the gradient
/// pinned to the frustum and the lamps locked to the view.
///
/// Everything else passes straight through — the viewer keeps calling
/// `set_at`, `set_dist`, `fov` and the rest on the wrapped camera through
/// [`Deref`].
pub struct ViewerCamera {
    orbit: OrbitCamera3d,
    /// The gradient to re-place each frame, `None` while no file is loaded.
    backdrop: Option<Backdrop>,
    /// The lamps to turn with the eye, `None` while no file is loaded.
    rig: Option<LightRig>,
}

impl ViewerCamera {
    /// Wrap `orbit`.
    pub fn new(orbit: OrbitCamera3d) -> Self {
        Self {
            orbit,
            backdrop: None,
            rig: None,
        }
    }

    /// The gradient that gets pinned to the frustum, or `None` for none.
    ///
    /// The backdrop belongs to the scene of one loaded file, so this is called
    /// once per load; the previous one goes with the scene it was built for.
    pub fn set_backdrop(&mut self, backdrop: Option<Backdrop>) {
        self.backdrop = backdrop;
    }

    /// The lamps that get turned with the eye, or `None` for none.
    ///
    /// Like [`Self::set_backdrop`] this belongs to the scene of one loaded file
    /// and is called once per load.
    pub fn set_light_rig(&mut self, rig: Option<LightRig>) {
        self.rig = rig;
    }

    /// The wrapped camera, mutably — [`fit`](crate::fit) replaces it wholesale
    /// to re-fit the clip planes, and assigning through [`DerefMut`] would also
    /// replace the backdrop slot.
    pub fn inner_mut(&mut self) -> &mut OrbitCamera3d {
        &mut self.orbit
    }

    /// The wrapped camera, read-only: [`CameraPose::of`] reads the view that is
    /// on screen, which is what the toolbar shows.
    pub fn inner(&self) -> &OrbitCamera3d {
        &self.orbit
    }

    /// Point the pan at the middle mouse button, see [`rebind_pan`].
    pub fn rebind_pan(&mut self) {
        rebind_pan(&mut self.orbit);
    }
}

/// The camera pose in a form a person can read, copy and hand back.
///
/// An orbit camera is fully described by where its eye is and what it looks at:
/// distance, yaw and pitch all follow from the pair, and
/// [`OrbitCamera3d::look_at`] restores them. The text form is what the toolbar
/// shows and what the `--camera` command-line option takes, so a view someone
/// is looking at can be reopened somewhere else exactly:
///
/// ```text
/// eye 12.34,-56.78,90.12 at 1.23,4.56,7.89
/// ```
///
/// Coordinates are model units (JT is millimetres) printed with two decimals,
/// so a restored pose is within hundredths of a millimetre of the original. The
/// **up axis is not part of the pose**: it comes from the loaded file, so the
/// same text means the same view for the same file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraPose {
    /// Where the eye is.
    pub eye: Vec3,
    /// The point it looks at.
    pub at: Vec3,
}

impl CameraPose {
    /// The pose `camera` holds right now.
    pub fn of(camera: &OrbitCamera3d) -> Self {
        Self {
            eye: camera.eye(),
            at: camera.at(),
        }
    }

    /// The one-line text form, which [`str::parse`] reads back.
    pub fn text(&self) -> String {
        format!("eye {} at {}", triple(self.eye), triple(self.at))
    }

    /// Restore the pose on `camera`.
    ///
    /// The up axis has to be the one of the file this pose came from — set it
    /// first ([`OrbitCamera3d::set_up_axis`]), because `look_at` resolves yaw
    /// and pitch against it. Kiss3d also clamps the distance into the camera's
    /// zoom range, which [`crate::fit`] fits to the model, so a pose from the
    /// same file is never clamped.
    pub fn apply(&self, camera: &mut OrbitCamera3d) {
        camera.look_at(self.eye, self.at);
    }
}

/// `x,y,z` with two decimals, and a plain `0.00` for a value that rounds to
/// zero so the text never reads `-0.00`.
fn triple(v: Vec3) -> String {
    let part = |a: f32| {
        if (a * 100.0).round() == 0.0 { 0.0 } else { a }
    };
    format!("{:.2},{:.2},{:.2}", part(v.x), part(v.y), part(v.z))
}

impl std::str::FromStr for CameraPose {
    type Err = String;

    /// Reads [`Self::text`]: `eye x,y,z at x,y,z`, in either order, with an
    /// optional `=` or `:` after a key. Keys are case-insensitive.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut eye: Option<Vec3> = None;
        let mut at: Option<Vec3> = None;
        let mut words = text.split_whitespace();
        while let Some(word) = words.next() {
            // `eye=1,2,3` and `eye: 1,2,3` both spell the same field.
            let (key, inline) = match word.split_once(['=', ':']) {
                Some((key, value)) => (key, Some(value)),
                None => (word, None),
            };
            let slot = match key.to_ascii_lowercase().as_str() {
                "eye" => &mut eye,
                "at" => &mut at,
                other => {
                    return Err(format!(
                        "unknown camera field {other:?}; expected \"eye x,y,z at x,y,z\""
                    ));
                }
            };
            let value = match inline
                .filter(|value| !value.is_empty())
                .or_else(|| words.next())
            {
                Some(value) => value,
                None => return Err(format!("camera field {key:?} has no value")),
            };
            // A triple typed with spaces after the commas arrives as several
            // words, so keep taking words until the commas add up to a triple.
            let mut value = value.to_string();
            while value.matches(',').count() < 2 {
                match words.next() {
                    Some(next) => {
                        let next = next.trim_matches(',');
                        if !value.ends_with(',') {
                            value.push(',');
                        }
                        value.push_str(next);
                    }
                    None => break,
                }
            }
            *slot = Some(triple_of(&value)?);
        }
        let (Some(eye), Some(at)) = (eye, at) else {
            return Err(format!(
                "expected an eye and an at: \"eye x,y,z at x,y,z\", not {text:?}"
            ));
        };
        if eye == at {
            return Err("the eye and the point it looks at are the same".to_string());
        }
        Ok(Self { eye, at })
    }
}

/// Three comma-separated finite numbers.
fn triple_of(text: &str) -> Result<Vec3, String> {
    let parts: Vec<f32> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .map_err(|_| format!("{text:?} is not a coordinate triple of numbers"))
        })
        .collect::<Result<_, _>>()?;
    let [x, y, z] = parts.as_slice() else {
        return Err(format!("{text:?} is not a coordinate triple of numbers"));
    };
    let v = Vec3::new(*x, *y, *z);
    if !v.is_finite() {
        return Err(format!("{text:?} is not a finite coordinate triple"));
    }
    Ok(v)
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
        // The lamps travel with the eye for the same reason the gradient does:
        // `prepare` reads their world rotation into the light uniforms later in
        // this very frame.
        if let Some(rig) = self.rig.as_mut() {
            rig.place(&self.orbit);
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
        camera.set_light_rig(None);
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

    /// The text in the toolbar is the whole point of [`CameraPose`]: it has to
    /// say exactly what the camera is doing, and the same text has to put the
    /// camera back there.
    #[test]
    fn the_pose_text_describes_the_camera_it_came_from() {
        let camera = orbit();
        let pose = CameraPose::of(&camera);
        assert_eq!(pose.eye, Vec3::new(3.0, -4.0, 5.0));
        assert_eq!(pose.at, Vec3::new(1.0, 1.0, 2.0));
        assert_eq!(pose.text(), "eye 3.00,-4.00,5.00 at 1.00,1.00,2.00");
        assert_eq!(pose.text().parse::<CameraPose>().expect("reads back"), pose);
    }

    /// A quoted view is only useful if applying it to another camera of the same
    /// file gives the same view — distance, yaw and pitch included.
    #[test]
    fn a_quoted_view_reopens_the_same_view() {
        let source = orbit();
        let text = CameraPose::of(&source).text();
        let mut other = OrbitCamera3d::new_with_frustum(
            std::f32::consts::FRAC_PI_4,
            0.25,
            250.0,
            Vec3::new(-7.0, 2.0, -9.0),
            Vec3::new(20.0, 20.0, 20.0),
        );
        text.parse::<CameraPose>()
            .expect("parsed")
            .apply(&mut other);
        assert_eq!(other.eye(), source.eye());
        assert_eq!(other.at(), source.at());
        assert_eq!(other.dist(), source.dist());
        assert_eq!(other.yaw(), source.yaw());
        assert_eq!(other.pitch(), source.pitch());
    }

    /// Someone reads the text off a screen and types it back, so the parser has
    /// to take what a person plausibly writes — and refuse what is not a view.
    #[test]
    fn the_parser_takes_what_a_person_writes() {
        for text in [
            "eye 1,2,3 at 4,5,6",
            "  EYE 1, 2, 3 AT 4,5,6 ",
            "at 4,5,6 eye 1,2,3",
            "eye=1,2,3 at:4,5,6",
        ] {
            let pose = text.parse::<CameraPose>().expect(text);
            assert_eq!(pose.eye, Vec3::new(1.0, 2.0, 3.0), "{text}");
            assert_eq!(pose.at, Vec3::new(4.0, 5.0, 6.0), "{text}");
        }
    }

    #[test]
    fn a_text_that_is_not_a_view_is_refused() {
        for text in [
            "",
            "eye 1,2,3",
            "1,2,3 4,5,6",
            "eye 1,2 at 4,5,6",
            "eye 1,2,3 at 1,2,3",
            "eye 1,2,3 nose 4,5,6",
            "eye x,y,z at 1,2,3",
        ] {
            assert!(text.parse::<CameraPose>().is_err(), "{text:?} parsed");
        }
    }

    /// A pose read from one camera, printed and read back, must not drift: the
    /// toolbar text is what gets quoted, and rounding it twice would walk away
    /// from the view it describes.
    #[test]
    fn printing_a_pose_does_not_move_it() {
        let mut camera = orbit();
        let first = CameraPose::of(&camera);
        let text = first.text();
        text.parse::<CameraPose>()
            .expect("parsed")
            .apply(&mut camera);
        assert_eq!(CameraPose::of(&camera).text(), text);
    }

    /// The bar is recomputed from the camera every frame, so what it says has to
    /// describe the camera after a manual orbit, not the pose the file opened on.
    #[test]
    fn the_readout_follows_a_move_of_the_camera() {
        let mut camera = orbit();
        let before = CameraPose::of(&camera).text();
        camera.set_yaw(camera.yaw() + 0.6);
        camera.set_dist(camera.dist() * 0.5);
        let after = CameraPose::of(&camera).text();
        assert_ne!(before, after, "the text must change with the camera");
        // And the new text still describes the camera it came from, so what is
        // on screen can be handed back to the viewer verbatim.
        let mut reopened = orbit();
        after
            .parse::<CameraPose>()
            .expect("the readout of a moved camera parses")
            .apply(&mut reopened);
        assert_eq!(CameraPose::of(&reopened).text(), after);
    }
}
