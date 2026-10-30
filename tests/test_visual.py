"""Visual regression tests: render the glTF export of a JT sample and compare
the picture with a committed reference.

The reference pictures live next to the reference dumps in
``data/Reference`` and are named after the sample, so the workflow is the same
as for the JSON oracle: the export is rendered, the picture is compared with
the reference, and a mismatch produces a diff picture instead of a bare
assertion.

Rendering is done by Open3D offscreen (no window, no display needed) with a
fixed picture size, a fixed isometric view direction and a fit-all camera, so
the same build of the same file produces the same picture. Because a GPU
shader is not bit exact across drivers, the comparison is a similarity score
rather than equality: the picture passes when at least ``THRESHOLD`` of the
possible colour difference is left unused.

Two Open3D quirks shape :func:`render_sample`: the first frame of a freshly
created ``OffscreenRenderer`` comes back empty, so one frame is rendered and
discarded, and creating a renderer per picture is both slow and unstable, so
the whole process shares a single one.

Run it as a script to (re)create the references or to inspect a mismatch::

    python tests/test_visual.py --write          # (re)create the references
    python tests/test_visual.py                  # compare, write diff pictures
    python tests/test_visual.py --threshold 0.99 # loosen the match
    python tests/test_visual.py --open           # also open the diff pictures
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

import pytest

import ferrojt

try:  # Open3D is an optional extra: without it the tests skip, not fail.
    import numpy as np
    import open3d as o3d
except ImportError:  # pragma: no cover - depends on the environment
    np = None  # type: ignore[assignment]
    o3d = None  # type: ignore[assignment]

DATA = Path(__file__).resolve().parent.parent / "data"
SAMPLES = DATA / "ExampleFiles"
REFERENCES = DATA / "Reference"
#: Where mismatch pictures are written; not part of the repository.
DIFF_DIR = Path(__file__).resolve().parent / "_visual_out"

#: The samples to render: the three versions of the same cube, one per JT
#: generation, and the two versions of an assembly that instantiates one screw
#: twice — the instances have to show up as two copies of that screw.
MODELS = [
    "example_block_jt8.1.jt",
    "example_block_jt9.5.jt",
    "example_block_jt10.3.jt",
    "opening_protection_plate1_jt8.0.jt",
    "opening_protection_plate1_jt9.5.jt",
]

# --- the fixed picture ------------------------------------------------------

#: Picture size in pixels; the reference is stored at exactly this size.
WIDTH = 640
HEIGHT = 480
#: Vertical field of view in degrees of the fit-all camera.
FOV_DEG = 30.0
#: The classic isometric direction: looking at a corner of the unit cube.
VIEW_DIRECTION = np.array([1.0, 1.0, 1.0]) if np is not None else None
#: World up, which is the Z axis of a JT file.
UP = np.array([0.0, 0.0, 1.0]) if np is not None else None
#: Extra room around the model so edges are not flush with the frame.
MARGIN = 1.15
#: Opaque white background, so a missing mesh is visible as a blank picture.
BACKGROUND = [1.0, 1.0, 1.0, 1.0]
#: One flat grey material for every mesh: the picture then depends on the
#: geometry alone and not on how a loader happens to read a glTF material.
BASE_COLOR = (0.72, 0.72, 0.72, 1.0)
SHADER = "defaultLit"

#: How alike the pictures have to be, in ``0..1``. Overridable so a different
#: GPU or Open3D version does not fail the suite outright.
THRESHOLD = float(os.environ.get("FERROJT_VISUAL_THRESHOLD", "0.995"))
#: Pixels are counted as differing when they move by more than this much.
PIXEL_EPS = float(os.environ.get("FERROJT_VISUAL_EPS", "8.0"))

needs_open3d = pytest.mark.skipif(o3d is None, reason="open3d is not installed")
needs_numpy = pytest.mark.skipif(np is None, reason="numpy is not installed")


def reference_path(sample: str) -> Path:
    """The reference picture of ``sample``, next to the reference dumps."""
    return REFERENCES / f"{Path(sample).stem}.png"


def sample_path(sample: str) -> Path:
    """The JT file of ``sample`` under ``data/ExampleFiles``."""
    return SAMPLES / sample


def export_glb(sample: str, work_dir: Path) -> Path:
    """Write the glTF export of ``sample`` into ``work_dir`` and return it."""
    model = ferrojt.open(str(sample_path(sample)))
    out = work_dir / f"{Path(sample).stem}.glb"
    model.save(str(out), "glb")
    return out


def fit_camera(
    render: "o3d.visualization.rendering.OffscreenRenderer",
    mesh: "o3d.geometry.TriangleMesh",
) -> None:
    """Place the camera so that the whole mesh fills the frame.

    The distance follows from the bounding sphere of the mesh and the field of
    view, which is the "fit all" zoom of a viewer; the direction and the up
    vector are the fixed ones of this module, so the picture is reproducible.
    """
    box = mesh.get_axis_aligned_bounding_box()
    center = np.asarray(box.get_center(), dtype=np.float64)
    radius = 0.5 * float(np.linalg.norm(np.asarray(box.get_extent(), dtype=np.float64)))
    if radius <= 0.0:  # an empty mesh still has to produce a picture
        radius = 1.0
    distance = radius / np.sin(np.radians(FOV_DEG / 2.0)) * MARGIN
    eye = center + VIEW_DIRECTION * distance
    render.setup_camera(FOV_DEG, center.tolist(), eye.tolist(), UP.tolist())


_render: "o3d.visualization.rendering.OffscreenRenderer | None" = None


def get_renderer() -> "o3d.visualization.rendering.OffscreenRenderer":
    """The one offscreen renderer of this process, created on first use.

    Open3D releases its Vulkan materials lazily, so a second renderer in the
    same process aborts the engine; every picture is drawn by this instance.
    """
    global _render
    if _render is None:
        o3d.utility.set_verbosity_level(o3d.utility.VerbosityLevel.Error)
        _render = o3d.visualization.rendering.OffscreenRenderer(WIDTH, HEIGHT)
        _render.scene.set_background(BACKGROUND)
        # A headlight: no scene lights, so no shadow placement to disagree about.
        _render.scene.set_lighting(_render.scene.LightingProfile.NO_SHADOWS, [0.0, 0.0, 0.0])
    return _render


def render_sample(sample: str, work_dir: Path) -> "np.ndarray":
    """Export ``sample`` and return the picture of it as ``H x W x 3`` uint8."""
    mesh = o3d.io.read_triangle_mesh(str(export_glb(sample, work_dir)))
    if not mesh.has_vertex_normals():
        mesh.compute_vertex_normals()

    render = get_renderer()
    render.scene.clear_geometry()
    material = o3d.visualization.rendering.MaterialRecord()
    material.shader = SHADER
    material.base_color = BASE_COLOR
    render.scene.add_geometry("model", mesh, material)
    fit_camera(render, mesh)
    render.render_to_image()  # the first frame of a new view comes back empty
    image = np.asarray(render.render_to_image())
    return np.ascontiguousarray(image[..., :3], dtype=np.uint8)


@dataclass
class Comparison:
    """The outcome of comparing one picture with its reference."""

    sample: str
    similarity: float
    threshold: float
    max_difference: float
    differing: float
    diff_path: Path | None

    @property
    def passed(self) -> bool:
        """Whether the picture is close enough to the reference."""
        return self.similarity >= self.threshold

    def describe(self) -> str:
        """One line of human readable output."""
        if self.passed:
            return (
                f"PASS {self.sample}: similarity {self.similarity:.5f} "
                f">= {self.threshold:.5f}"
            )
        return (
            f"FAIL {self.sample}: similarity {self.similarity:.5f} "
            f"< {self.threshold:.5f}, {self.differing * 100:.2f}% of the pixels "
            f"differ by more than {PIXEL_EPS:g} (max {self.max_difference:.0f})"
        )


def similarity(a: "np.ndarray", b: "np.ndarray") -> tuple[float, float, float]:
    """Compare two pictures.

    Returns the similarity in ``0..1`` — ``1`` for identical pictures — the
    largest per-channel difference, and the fraction of pixels that differ by
    more than :data:`PIXEL_EPS`.
    """
    if a.shape != b.shape:
        return 0.0, 255.0, 1.0
    left = a.astype(np.float64)
    right = b.astype(np.float64)
    difference = np.abs(left - right)
    score = 1.0 - float(difference.mean()) / 255.0
    largest = float(difference.max())
    changed = float((difference.max(axis=2) > PIXEL_EPS).mean())
    return score, largest, changed


def diff_picture(a: "np.ndarray", b: "np.ndarray") -> "np.ndarray":
    """A picture with the render, the reference and an amplified difference."""
    if a.shape != b.shape:
        height = max(a.shape[0], b.shape[0])
        width = max(a.shape[1], b.shape[1])
        a = pad(a, height, width)
        b = pad(b, height, width)
    difference = np.abs(a.astype(np.float64) - b.astype(np.float64)).max(axis=2)
    # Amplify so a one-level difference is visible, and tint it red.
    heat = np.clip(difference * 10.0, 0.0, 255.0)
    marked = np.full_like(a, 255, dtype=np.uint8)
    marked[..., 0] = 255
    marked[..., 1] = (255 - heat).astype(np.uint8)
    marked[..., 2] = (255 - heat).astype(np.uint8)
    gap = np.full((a.shape[0], 6, 3), 40, dtype=np.uint8)
    return np.concatenate([a, gap, b, gap, marked], axis=1)


def pad(image: "np.ndarray", height: int, width: int) -> "np.ndarray":
    """Pad a picture with white to ``height x width``."""
    out = np.full((height, width, 3), 255, dtype=np.uint8)
    out[: image.shape[0], : image.shape[1]] = image[..., :3]
    return out


def write_picture(path: Path, image: "np.ndarray") -> None:
    """Store ``image`` as a PNG file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    if not o3d.io.write_image(str(path), o3d.geometry.Image(image)):
        raise OSError(f"could not write {path}")


def show(path: Path) -> None:
    """Open a picture in the desktop viewer, best effort."""
    try:
        if sys.platform == "win32":
            os.startfile(str(path))  # type: ignore[attr-defined]
        else:
            subprocess.Popen(["xdg-open", str(path)])
    except OSError:  # pragma: no cover - only when there is no desktop
        print(f"could not open {path}", file=sys.stderr)


def compare(sample: str, work_dir: Path, threshold: float = THRESHOLD) -> Comparison:
    """Render ``sample`` and compare it with its reference picture."""
    reference = reference_path(sample)
    if not reference.is_file():
        raise FileNotFoundError(
            f"there is no reference picture {reference}; "
            f"create it with 'python tests/test_visual.py --write'"
        )
    actual = render_sample(sample, work_dir)
    wanted = np.asarray(o3d.io.read_image(str(reference)))[..., :3]
    score, largest, changed = similarity(actual, wanted)
    diff_path: Path | None = None
    if score < threshold:
        diff_path = DIFF_DIR / f"{Path(sample).stem}-diff.png"
        write_picture(diff_path, diff_picture(actual, wanted))
    return Comparison(sample, score, threshold, largest, changed, diff_path)


# --- the pytest side --------------------------------------------------------

needs_samples = pytest.mark.skipif(
    not all(sample_path(name).is_file() for name in MODELS),
    reason="the sample JT files are not checked out",
)


@needs_open3d
@needs_numpy
@needs_samples
@pytest.mark.parametrize("sample", MODELS)
def test_the_render_matches_the_reference_picture(sample: str, tmp_path: Path) -> None:
    """The glTF export of a sample renders like its reference picture."""
    result = compare(sample, tmp_path)
    message = result.describe()
    if result.diff_path is not None:
        message += f"\ndiff picture: {result.diff_path}"
    assert result.passed, message


@needs_open3d
@needs_numpy
@needs_samples
def test_a_reference_picture_exists_for_every_sample() -> None:
    """The references are part of the repository, like the JSON dumps."""
    missing = [str(reference_path(name)) for name in MODELS if not reference_path(name).is_file()]
    assert not missing, "missing reference picture(s): " + ", ".join(missing)


@needs_numpy
def test_the_similarity_of_a_picture_with_itself_is_one() -> None:
    """The metric itself: identical pictures score 1, unrelated ones do not."""
    image = np.full((4, 4, 3), 128, dtype=np.uint8)
    assert similarity(image, image.copy()) == (1.0, 0.0, 0.0)
    other = image.copy()
    other[1, 1] = [255, 0, 0]
    score, largest, changed = similarity(image, other)
    # the biggest channel delta of that one pixel is 128 - 0
    assert score < 1.0 and largest == 128.0 and 0.0 < changed < 1.0
    assert similarity(image, np.zeros((4, 4, 3), dtype=np.uint8))[0] == pytest.approx(
        1.0 - 128.0 / 255.0
    )
    assert similarity(image, np.zeros((6, 4, 3), dtype=np.uint8))[0] == 0.0


# --- the script side --------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    """Render every sample, compare, and report. See the module docstring."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--write",
        action="store_true",
        help="write the reference pictures instead of comparing",
    )
    parser.add_argument(
        "--sample",
        action="append",
        choices=MODELS,
        help="only this sample (repeatable), every sample by default",
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=THRESHOLD,
        help=f"similarity a picture needs to pass (default {THRESHOLD:g})",
    )
    parser.add_argument(
        "--open",
        action="store_true",
        help="open the diff pictures in the desktop viewer",
    )
    args = parser.parse_args(argv)

    if o3d is None:
        print("open3d is not installed: pip install open3d", file=sys.stderr)
        return 2
    names = args.sample or MODELS
    missing = [str(sample_path(name)) for name in names if not sample_path(name).is_file()]
    if missing:
        print("missing sample file(s): " + ", ".join(missing), file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory() as name:
        work_dir = Path(name)
        failures = 0
        for sample in names:
            if args.write:
                picture = render_sample(sample, work_dir)
                target = reference_path(sample)
                write_picture(target, picture)
                print(f"wrote {target} ({WIDTH}x{HEIGHT})")
                continue
            try:
                result = compare(sample, work_dir, args.threshold)
            except FileNotFoundError as error:
                print(f"SKIP {sample}: {error}", file=sys.stderr)
                failures += 1
                continue
            print(result.describe())
            if result.diff_path is not None:
                print(f"  diff picture: {result.diff_path}")
                if args.open:
                    show(result.diff_path)
            failures += 0 if result.passed else 1

    if args.write:
        return 0
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
