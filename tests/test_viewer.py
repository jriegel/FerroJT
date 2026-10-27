#!/usr/bin/env python
"""Interactive JT viewer: loads OpenJT files with FerroJT and shows them in an
Open3D GUI window.

This is a *script*, not a pytest test case: importing it never opens a window,
so collecting it under ``testpaths = ["tests"]`` is harmless. Run it with::

    .\\.venv\\Scripts\\python.exe tests\\test_viewer.py data\\ExampleFiles\\example_block_jt9.5.jt
    .\\.venv\\Scripts\\python.exe tests\\test_viewer.py            # then File -> Open

Controls
--------
=================  =========================================================
left mouse drag    orbit around the model
right mouse drag   look around (pivots at the eye, pairs well with flying)
middle mouse drag  pan in the view plane
mouse wheel        dolly towards / away from the target
``w a s d``        fly forward / left / backward / right
``q z``            fly up / down
``ESC``            close the window (Open3D handles it itself)
``File`` menu      Open... / Exit
=================  =========================================================

Notes
-----
* JT files are converted to a temporary ``.glb`` because Open3D has no
  in-memory glTF reader; ``ferrojt`` writes, ``open3d.io.read_triangle_model``
  reads it back, which keeps materials and normals.
* FerroJT exports Z-up geometry, so the viewer keeps ``+Z`` as the up axis.
* Drag and drop: Open3D 0.20 registers a GLFW drop callback, but it forwards
  every path to ``gui::Window::OnDragDropped()``, which is an empty C++ stub
  with no Python binding — drops are unreachable from Python. On Windows the
  viewer therefore subclasses the GLFW window with
  ``comctl32.SetWindowSubclass()`` and handles ``WM_DROPFILES`` itself (best
  effort; ``File -> Open`` always works). A plain
  ``SetWindowLongPtrW(GWLP_WNDPROC)`` subclass is never called on that window.
* On exit Open3D aborts with
  ``Postcondition in ~VulkanFboCache:72 ... Please explicitly call terminate()``
  (isl-org/Open3D#7593). Nothing in the Python API can prevent it, so the
  non-zero exit status of this script is expected and not a viewer failure.
"""

from __future__ import annotations

import argparse
import atexit
import math
import shutil
import sys
import tempfile
from pathlib import Path

import ferrojt

try:
    import numpy as np
    import open3d as o3d
    import open3d.visualization.gui as gui
    import open3d.visualization.rendering as rendering
except ImportError:  # pragma: no cover - open3d is an optional extra
    np = None
    o3d = None
    gui = None
    rendering = None

TITLE = "FerroJT viewer"
MODEL_NAME = "__model__"

MENU_OPEN = 1
MENU_EXIT = 2

WORLD_UP = np.array([0.0, 0.0, 1.0], dtype=np.float64) if np else None
FLY_KEYS = frozenset(b"wasdzq")

WIDTH = 1024
HEIGHT = 768


# --------------------------------------------------------------------------- #
# Camera maths
# --------------------------------------------------------------------------- #
def _to_spherical(vec):
    """Cartesian -> (radius, azimuth, elevation) with +Z up."""
    radius = float(np.linalg.norm(vec))
    if radius <= 1e-12:
        return 0.0, 0.0, 0.0
    sin_el = max(-1.0, min(1.0, float(vec[2]) / radius))
    return radius, math.atan2(float(vec[1]), float(vec[0])), math.asin(sin_el)


def _from_spherical(radius, azimuth, elevation):
    cos_el = math.cos(elevation)
    return np.array(
        [
            radius * cos_el * math.cos(azimuth),
            radius * cos_el * math.sin(azimuth),
            radius * math.sin(elevation),
        ],
        dtype=np.float64,
    )


def _unit(vec, fallback):
    norm = float(np.linalg.norm(vec))
    if norm <= 1e-12:
        return np.asarray(fallback, dtype=np.float64)
    return np.asarray(vec, dtype=np.float64) / norm


# --------------------------------------------------------------------------- #
# Windows-only drag and drop
# --------------------------------------------------------------------------- #
# The ctypes window procedure has to outlive the call that installs it.
_DROP_STATE = []


def _install_drop_handler(hwnd, on_path):
    """Subclass the window procedure so that ``WM_DROPFILES`` reaches Python.

    Returns True when the hook is in place. Never raises: drag and drop is a
    convenience, ``File -> Open`` is the fallback.
    """
    if sys.platform != "win32":
        return False
    try:
        import ctypes
        from ctypes import wintypes

        comctl32 = ctypes.windll.comctl32
        shell32 = ctypes.windll.shell32

        WM_DROPFILES = 0x0233
        LRESULT = ctypes.c_ssize_t
        SUBCLASSPROC = ctypes.WINFUNCTYPE(
            LRESULT,
            wintypes.HWND,
            wintypes.UINT,
            wintypes.WPARAM,
            wintypes.LPARAM,
            ctypes.c_size_t,
            ctypes.c_size_t,
        )

        shell32.DragAcceptFiles.argtypes = [wintypes.HWND, wintypes.BOOL]
        shell32.DragQueryFileW.restype = wintypes.UINT
        # wintypes has no HDROP, the drop handle is an opaque pointer.
        shell32.DragQueryFileW.argtypes = [
            ctypes.c_void_p,
            wintypes.UINT,
            wintypes.LPWSTR,
            wintypes.UINT,
        ]
        shell32.DragFinish.argtypes = [ctypes.c_void_p]
        comctl32.SetWindowSubclass.restype = wintypes.BOOL
        comctl32.SetWindowSubclass.argtypes = [
            wintypes.HWND,
            SUBCLASSPROC,
            ctypes.c_size_t,
            ctypes.c_size_t,
        ]
        comctl32.DefSubclassProc.restype = LRESULT
        comctl32.DefSubclassProc.argtypes = [
            wintypes.HWND,
            wintypes.UINT,
            wintypes.WPARAM,
            wintypes.LPARAM,
        ]

        shell32.DragAcceptFiles(hwnd, True)

        def wnd_proc(handle, message, w_param, l_param, subclass_id, ref):
            if message != WM_DROPFILES:
                return comctl32.DefSubclassProc(handle, message, w_param, l_param)
            drop = l_param
            try:
                count = shell32.DragQueryFileW(drop, 0xFFFFFFFF, None, 0)
                for index in range(count):
                    buffer = ctypes.create_unicode_buffer(4096)
                    if shell32.DragQueryFileW(drop, index, buffer, 4096):
                        on_path(buffer.value)
            except Exception as exc:  # pragma: no cover - defensive
                print(f"[viewer] drop handler failed: {exc}")
            finally:
                shell32.DragFinish(drop)
            return 0

        # The callable object owns the native code pointer the window now calls:
        # it must stay alive for as long as the window does.
        subclass_proc = SUBCLASSPROC(wnd_proc)
        if not comctl32.SetWindowSubclass(hwnd, subclass_proc, 1, 0):
            return False
        _DROP_STATE.append((hwnd, subclass_proc))
        return True
    except Exception as exc:  # pragma: no cover - platform specific
        print(f"[viewer] could not install the drop handler: {exc}")
        return False


def _find_window_handle(title):
    """Return the HWND of the Open3D window of this process, or 0 if none.

    Matching on the GLFW window class keeps this independent of the title,
    which changes as soon as a file is loaded; the title is only used as a
    fallback.
    """
    if sys.platform != "win32":
        return 0
    try:
        import ctypes
        import os
        from ctypes import wintypes

        user32 = ctypes.windll.user32

        EnumWindowsProc = ctypes.WINFUNCTYPE(
            wintypes.BOOL, wintypes.HWND, wintypes.LPARAM
        )
        user32.EnumWindows.argtypes = [EnumWindowsProc, wintypes.LPARAM]
        user32.GetWindowThreadProcessId.argtypes = [
            wintypes.HWND,
            ctypes.POINTER(wintypes.DWORD),
        ]
        user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        user32.IsWindowVisible.argtypes = [wintypes.HWND]
        user32.IsWindowVisible.restype = wintypes.BOOL

        process_id = wintypes.DWORD(0)
        found = 0
        fallback = 0

        def visit(handle, _lparam):
            nonlocal found, fallback
            if found:
                return False
            user32.GetWindowThreadProcessId(handle, ctypes.byref(process_id))
            if process_id.value != os.getpid() or not user32.IsWindowVisible(handle):
                return True
            name = ctypes.create_unicode_buffer(256)
            user32.GetClassNameW(handle, name, 256)
            if name.value == "GLFW30":
                # ``handle`` arrives as a plain int (c_void_p argument type).
                found = int(handle or 0)
                return False
            text = ctypes.create_unicode_buffer(256)
            user32.GetWindowTextW(handle, text, 256)
            if not fallback and text.value == title:
                fallback = int(handle or 0)
            return True

        # The callback owns a native code pointer that user32 calls back into,
        # so it must be a named local: a temporary can be collected while the
        # native call is still in flight, which crashes the process.
        callback = EnumWindowsProc(visit)
        user32.EnumWindows(callback, 0)
        del callback
        return found or fallback
    except Exception:  # pragma: no cover - platform specific
        return 0


# --------------------------------------------------------------------------- #
# The viewer
# --------------------------------------------------------------------------- #
class Viewer:
    """A ``SceneWidget`` plus an FPS overlay, custom mouse and key handling."""

    ORBIT = "orbit"
    LOOK = "look"
    PAN = "pan"

    def __init__(self, width=WIDTH, height=HEIGHT):
        app = gui.Application.instance
        self._app = app
        self._window = app.create_window(TITLE, width, height)

        # The FPS label sits on top of the scene, so the window cannot use a
        # layout: register the callback *before* adding the children and place
        # both frames by hand (see open3d's own mouse_and_point_coord.py).
        self._window.set_on_layout(self._on_layout)

        self._scene_widget = gui.SceneWidget()
        self._scene_widget.scene = rendering.Open3DScene(self._window.renderer)
        self._window.add_child(self._scene_widget)

        self._fps_label = gui.Label("fps --")
        self._fps_label.text_color = gui.Color(1.0, 0.85, 0.2)
        self._window.add_child(self._fps_label)

        scene = self._scene_widget.scene
        scene.set_background([0.12, 0.12, 0.14, 1.0])
        scene.set_lighting(
            rendering.Open3DScene.LightingProfile.NO_SHADOWS, [0.2, 0.3, -1.0]
        )
        scene.show_axes(True)
        scene.show_skybox(False)

        # The built-in interactor always runs, and there is no way to switch it
        # off. ROTATE_IBL only turns the (unused) image based lighting, so it is
        # the one mode that leaves the camera alone: every camera gesture below
        # is ours. It does briefly show the skybox while a drag is in progress,
        # which is why the skybox is switched off here and the model is kept
        # lit by the lighting profile above.
        self._scene_widget.set_view_controls(
            gui.SceneWidget.Controls.ROTATE_IBL
        )
        self._scene_widget.set_on_mouse(self._on_mouse)

        self._window.set_on_key(self._on_key)
        self._window.set_on_tick_event(self._on_tick)
        self._window.set_on_close(self._on_close)
        self._window.set_on_menu_item_activated(MENU_OPEN, self._on_menu_open)
        self._window.set_on_menu_item_activated(MENU_EXIT, self._on_menu_quit)
        self._window.set_focus_widget(self._scene_widget)

        # Camera state is the single source of truth; it is pushed to the
        # renderer through SceneWidget.look_at(), which also keeps the far
        # plane and the centre of rotation up to date.
        self._eye = np.array([3.0, -3.0, 2.0], dtype=np.float64)
        self._target = np.zeros(3, dtype=np.float64)
        self._up = WORLD_UP.copy()
        self._extent = 1.0
        self._fly_speed = 1.0

        self._mode = None
        self._last_x = 0
        self._last_y = 0
        self._keys = set()

        self._last_tick = app.now
        self._fps_time = app.now
        self._fps_count = 0
        self._hwnd = 0
        self._drop_hook_tried = False
        self._last_dir = Path.cwd()
        self._tmp_dir = Path(tempfile.mkdtemp(prefix="ferrojt_viewer_"))
        atexit.register(shutil.rmtree, self._tmp_dir, ignore_errors=True)

        self._apply_camera()

    # -- layout ------------------------------------------------------------ #
    def _on_layout(self, layout_context):
        rect = self._window.content_rect
        self._scene_widget.frame = rect
        preferred = self._fps_label.calc_preferred_size(
            layout_context, gui.Widget.Constraints()
        )
        margin = 10
        self._fps_label.frame = gui.Rect(
            rect.x + margin, rect.y + margin, preferred.width, preferred.height
        )

    # -- camera ------------------------------------------------------------ #
    def _apply_camera(self):
        self._scene_widget.look_at(
            self._target.astype(np.float32),
            self._eye.astype(np.float32),
            self._up.astype(np.float32),
        )

    def _view_basis(self):
        forward = self._target - self._eye
        forward = _unit(forward, [0.0, 1.0, 0.0])
        right = _unit(np.cross(forward, WORLD_UP), [1.0, 0.0, 0.0])
        up = np.cross(right, forward)
        return forward, right, up

    def _orbit(self, dx, dy):
        radius, azimuth, elevation = _to_spherical(self._eye - self._target)
        if radius <= 1e-12:
            return
        azimuth -= dx * 0.006
        elevation = max(-1.5, min(1.5, elevation + dy * 0.006))
        self._eye = self._target + _from_spherical(radius, azimuth, elevation)
        self._apply_camera()

    def _look(self, dx, dy):
        radius, azimuth, elevation = _to_spherical(self._target - self._eye)
        if radius <= 1e-12:
            return
        azimuth -= dx * 0.006
        elevation = max(-1.5, min(1.5, elevation - dy * 0.006))
        self._target = self._eye + _from_spherical(radius, azimuth, elevation)
        self._apply_camera()

    def _pan(self, dx, dy):
        forward, right, up = self._view_basis()
        distance = float(np.linalg.norm(self._target - self._eye))
        fov = float(self._scene_widget.scene.camera.get_field_of_view())
        height = max(1, self._scene_widget.frame.height)
        # World units covered by one pixel at the target's depth.
        scale = 2.0 * distance * math.tan(0.5 * fov) / height
        delta = (-dx * right + dy * up) * scale
        self._eye += delta
        self._target += delta
        self._apply_camera()

    def _dolly(self, wheel_dy):
        forward, _, _ = self._view_basis()
        offset = self._target - self._eye
        distance = float(np.linalg.norm(offset))
        if distance <= 1e-12:
            return
        factor = math.exp(-0.12 * float(wheel_dy))
        low, high = 1e-4 * self._extent, 100.0 * self._extent
        new_distance = min(max(distance * factor, low), high)
        self._eye = self._target - offset / distance * new_distance
        self._apply_camera()

    def _fly(self, dt):
        forward, right, up = self._view_basis()
        move = np.zeros(3, dtype=np.float64)
        if ord("w") in self._keys:
            move += forward
        if ord("s") in self._keys:
            move -= forward
        if ord("d") in self._keys:
            move += right
        if ord("a") in self._keys:
            move -= right
        if ord("q") in self._keys:
            move += WORLD_UP
        if ord("z") in self._keys:
            move -= WORLD_UP
        if not move.any():
            return
        step = self._fly_speed * dt
        self._eye += move * step
        self._target += move * step
        self._apply_camera()

    # -- events ------------------------------------------------------------ #
    def _on_mouse(self, event):
        ignored = gui.Widget.EventCallbackResult.IGNORED
        consumed = gui.Widget.EventCallbackResult.CONSUMED
        kind = event.type

        if kind == gui.MouseEvent.Type.WHEEL:
            self._dolly(event.wheel_dy)
            return consumed

        if kind == gui.MouseEvent.Type.BUTTON_DOWN:
            if event.is_button_down(gui.MouseButton.RIGHT):
                self._mode = self.LOOK
            elif event.is_button_down(gui.MouseButton.MIDDLE):
                self._mode = self.PAN
            elif event.is_button_down(gui.MouseButton.LEFT):
                self._mode = self.ORBIT
            else:
                self._mode = None
                return ignored
            self._last_x = event.x
            self._last_y = event.y
            # Returning CONSUMED makes the window grab the mouse, which is what
            # delivers the DRAG events to this widget.
            return consumed

        if kind == gui.MouseEvent.Type.DRAG and self._mode is not None:
            dx = event.x - self._last_x
            dy = event.y - self._last_y
            self._last_x = event.x
            self._last_y = event.y
            if self._mode == self.ORBIT:
                self._orbit(dx, dy)
            elif self._mode == self.LOOK:
                self._look(dx, dy)
            elif self._mode == self.PAN:
                self._pan(dx, dy)
            return consumed

        if kind == gui.MouseEvent.Type.BUTTON_UP and self._mode is not None:
            self._mode = None
            return consumed

        return ignored

    def _on_key(self, event):
        # Returning True keeps the key away from the built-in fly interactor.
        if event.key in FLY_KEYS:
            if event.type == gui.KeyEvent.Type.DOWN:
                self._keys.add(event.key)
            else:
                self._keys.discard(event.key)
            return True
        return False

    def _on_tick(self):
        now = self._app.now
        dt = min(max(now - self._last_tick, 0.0), 0.1)
        self._last_tick = now
        if self._keys:
            self._fly(dt)

        # Returning True forces a redraw, so the tick rate *is* the frame rate.
        self._fps_count += 1
        elapsed = now - self._fps_time
        if elapsed >= 1.0:
            self._fps_label.text = f"{self._fps_count / elapsed:5.1f} fps"
            self._fps_count = 0
            self._fps_time = now

        if self._hwnd == 0 and not self._drop_hook_tried:
            self._grab_window_handle()
        return True

    def _on_close(self):
        return True

    def _grab_window_handle(self):
        self._drop_hook_tried = True
        handle = _find_window_handle(self._window.title)
        if not handle:
            print("[viewer] window handle not found, drag and drop disabled")
            return
        self._hwnd = handle
        if _install_drop_handler(handle, self._on_dropped_path):
            print("[viewer] drop a .jt file onto the window to load it")

    def _on_dropped_path(self, path):
        def load():
            self.load(path)

        gui.Application.instance.post_to_main_thread(self._window, load)

    # -- menus and loading -------------------------------------------------- #
    def _on_menu_open(self):
        dialog = gui.FileDialog(gui.FileDialog.OPEN, "Open JT file", self._window.theme)
        dialog.add_filter(".jt", "JT files (.jt)")
        dialog.add_filter("", "All files")
        dialog.set_path(str(self._last_dir))
        dialog.set_on_cancel(self._on_dialog_cancel)
        dialog.set_on_done(self._on_dialog_done)
        self._window.show_dialog(dialog)

    def _on_dialog_cancel(self):
        self._window.close_dialog()

    def _on_dialog_done(self, filename):
        self._window.close_dialog()
        self.load(filename)

    def _on_menu_quit(self):
        self._app.quit()

    def load(self, path):
        path = Path(path)
        if not path.is_file():
            print(f"[viewer] no such file: {path}")
            return
        self._last_dir = path.parent

        try:
            model = ferrojt.open(str(path))
        except Exception as exc:
            print(f"[viewer] ferrojt could not read {path}: {exc}")
            return

        glb = self._tmp_dir / (path.stem + ".glb")
        try:
            model.save(str(glb), "glb")
        except Exception as exc:
            print(f"[viewer] could not convert {path} to glTF: {exc}")
            return

        scene = self._scene_widget.scene
        scene.clear_geometry()
        try:
            triangle_model = o3d.io.read_triangle_model(str(glb))
        except Exception as exc:
            print(f"[viewer] open3d could not read {glb}: {exc}")
            return
        if triangle_model is None or not triangle_model.meshes:
            print(f"[viewer] {path} contains no triangles")
            return
        scene.add_model(MODEL_NAME, triangle_model)

        bounds = scene.bounding_box
        extent = float(bounds.get_max_extent())
        if extent <= 0.0:
            print(f"[viewer] {path} has an empty bounding box")
            return
        center = np.asarray(bounds.get_center(), dtype=np.float64)
        self._extent = extent
        self._fly_speed = 0.5 * extent
        self._target = center
        self._up = WORLD_UP.copy()
        direction = _unit(np.array([1.0, -1.0, 0.6], dtype=np.float64), [1, 0, 0])
        self._eye = center + direction * (2.5 * extent)
        self._apply_camera()

        self._window.title = f"{path.name} - {TITLE}"
        # The handle lookup also matches on the title as a fallback, so retry it
        # if the first attempt (before the window was mapped) came up empty.
        if self._hwnd == 0:
            self._grab_window_handle()
        print(
            f"[viewer] {path.name}: {model.node_count} nodes, "
            f"{model.mesh_count} meshes, {model.vertex_count} vertices, "
            f"{model.triangle_count} triangles"
        )


def _build_menubar():
    """Build the global menu bar once, no matter how many windows exist."""
    app = gui.Application.instance
    if app.menubar is not None:
        return
    file_menu = gui.Menu()
    file_menu.add_item("Open...", MENU_OPEN)
    file_menu.add_separator()
    file_menu.add_item("Exit", MENU_EXIT)
    menu = gui.Menu()
    menu.add_menu("File", file_menu)
    app.menubar = menu


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("file", nargs="?", help="JT file to open at startup")
    parser.add_argument("--width", type=int, default=WIDTH, help="window width")
    parser.add_argument("--height", type=int, default=HEIGHT, help="window height")
    args = parser.parse_args(argv)

    if o3d is None or gui is None:
        print('open3d is required: pip install -e ".[test,visual]"', file=sys.stderr)
        return 2

    o3d.utility.set_verbosity_level(o3d.utility.VerbosityLevel.Error)
    app = gui.Application.instance
    app.initialize()
    _build_menubar()

    viewer = Viewer(args.width, args.height)
    if args.file:
        viewer.load(args.file)

    print(
        "controls: left-drag orbit | right-drag look | middle-drag pan | "
        "wheel dolly | w a s d (+ q z) fly | ESC close"
    )
    app.run()
    # Open3D 0.20 aborts while tearing down the Vulkan engine after run()
    # returns (isl-org/Open3D#7593); the exit status is not ours to fix.
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
