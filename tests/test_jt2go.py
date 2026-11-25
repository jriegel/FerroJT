"""Render the JT this library writes in Siemens JT2Go and require a picture.

This is the external oracle for the writer. A file this workspace produces can
parse here, pass ``jtinfo`` and even render in the NOVO engine while a strict
consumer shows nothing at all — the picture stays blank because the viewer
rejects the shape. Three such defects were found this way, and none of them was
visible in any of the other oracles:

* the composite hash of a topological shape, which is one call shorter at
  JT 10.x than at JT 9.x;
* the hash of a vertex array, which chains one word per call at JT 9.x and one
  whole component array per call at JT 10.x;
* the second, embedded logical element a JT 10.x tri-strip set wraps its coder
  input in — the same width as the fields a JT 9.x file keeps loose there, so
  everything lenient accepts a body that omits the framing.

So the check here is not a picture comparison but the only question a strict
consumer answers: does anything get drawn? ``jt2go_picture.render`` opens one
file in a fresh viewer, grabs the drawing surface with ``PrintWindow``, and
:func:`jt2go_picture.measure` counts the pixels that differ from the viewport
backdrop. A viewport that holds no geometry still yields a few hundred pixels —
the trihedron icon in the corner — which is why the threshold is well above
that and far below the smallest real picture.

JT2Go is a Windows GUI application, so the whole module skips when it cannot be
found: pass ``--exe`` on the command line of ``tests/jt2go_picture.py`` or set
``JT2GO_EXE`` to point at ``VisView_NG.exe``.

Run it as a script to render files by hand::

    .\\.venv\\Scripts\\python.exe tests\\jt2go_picture.py data\\ExampleFiles\\example_block_jt10.3.jt
    .\\.venv\\Scripts\\python.exe tests\\jt2go_picture.py target\\out.jt --out tests\\_jt2go_out
"""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path

import pytest

import ferrojt

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
SAMPLES = ROOT / "data" / "ExampleFiles"

# The harness lives next to this test but is not a test module: it is a tool,
# and its name must not start with ``test_`` so pytest leaves it alone. Loading
# it by path keeps it out of ``sys.path`` and out of the collected count.
_spec = importlib.util.spec_from_file_location("jt2go_picture", HERE / "jt2go_picture.py")
jt2go = importlib.util.module_from_spec(_spec)
sys.modules["jt2go_picture"] = jt2go
_spec.loader.exec_module(jt2go)  # type: ignore[union-attr]

#: Where the pictures of a failing run are written; not part of the repository.
OUT_DIR = HERE / "_jt2go_out"

#: How much of the viewport has to be geometry for a file to count as drawn.
#: A viewport with no geometry at all measures 352 pixels — the trihedron icon
#: of the drawing surface — while the smallest real picture of this suite
#: measures 226 982, so the margin either way is three orders of magnitude.
MIN_INK = int(os.environ.get("FERROJT_JT2GO_MIN_INK", jt2go.DEFAULT_MIN_INK))


def _viewer() -> str:
    """The viewer binary, or a skip when JT2Go is not installed."""

    try:
        return jt2go.find_viewer()
    except jt2go.Jt2GoError as exc:
        pytest.skip(str(exc))


@pytest.fixture(scope="module")
def viewer() -> str:
    """One discovery of the viewer for the whole module."""

    return _viewer()


def draw(viewer: str, path: Path) -> None:
    """Render ``path`` and fail with the picture written out when nothing is drawn."""

    OUT_DIR.mkdir(exist_ok=True)
    raw, width, height = jt2go.render(path, viewer)
    stats = jt2go.measure(raw, width, height)
    ink = int(stats["ink"])
    if ink < MIN_INK:
        picture = OUT_DIR / f"{path.stem}.png"
        jt2go.save_png(picture, raw, width, height)
        box = stats["box"]
        extent = f"{box[2]}x{box[3]}" if box else "nothing"
        pytest.fail(
            f"JT2Go drew no geometry from {path.name}: {ink} ink pixels of "
            f"{width}x{height}, geometry {extent} — the picture is in {picture}"
        )


def write_sample(name: str, target: Path, **options: object) -> Path:
    """Write ``name`` back out as JT into ``target`` and return it."""

    jt = ferrojt.JtFile(str(SAMPLES / name))
    assert jt.open(), f"{name} could not be read"
    assert jt.write_jt(str(target), **options), f"{name} could not be written"
    return target


def test_a_rewritten_file_renders(viewer: str, tmp_path: Path) -> None:
    """A file re-serialised at its own revision still draws.

    The revision is asked for explicitly: the writer keeps the revision of the
    source when it is not, and a test that means "10.x" has to say so.
    """

    written = write_sample(
        "example_block_jt9.5.jt",
        tmp_path / "block95at103.jt",
        all_lods=True,
        all_layers=True,
        version="10.3",
    )
    draw(viewer, written)


def test_a_file_written_from_a_model_renders(viewer: str, tmp_path: Path) -> None:
    """A file built from the neutral model — the glTF path — still draws.

    That writer encodes the topology itself instead of copying the segment of
    the source, so it is the one that carries the hash and framing recipes.
    """

    model = ferrojt.open(str(SAMPLES / "example_block_jt9.5.jt"))
    target = tmp_path / "blockmodel103.jt"
    model.save(str(target), "jt", version="10.3")
    draw(viewer, target)


def test_a_hand_built_file_renders(viewer: str, tmp_path: Path) -> None:
    """A file built from nothing still draws.

    The bare builder output is a root partition over a shape, which a strict
    consumer refuses outright; what is rendered here is the chain every sample
    carries — ``Partition -> MetaData -> Instance -> Part -> RangeLOD -> Group
    -> Shape`` — because that is what a consumer walks to reach geometry.
    """

    builder = ferrojt.JtBuilder()
    builder.version = "10.3"
    target = tmp_path / "built103.jt"
    builder.set_file_name(target.name)
    root = builder.root()
    meta = builder.add_node(root, "MetaData", "meta")
    part = builder.add_detached_node("Part")
    instance = builder.instance_of(meta, part)
    builder.set_name(instance, "cube.part")
    span = builder.add_node(part, "RangeLOD")
    group = builder.add_node(span, "Group", "cube")
    corners = [
        (0.0, 0.0, 0.0),
        (1.0, 0.0, 0.0),
        (1.0, 1.0, 0.0),
        (0.0, 1.0, 0.0),
        (0.0, 0.0, 1.0),
        (1.0, 0.0, 1.0),
        (1.0, 1.0, 1.0),
        (0.0, 1.0, 1.0),
    ]
    vertices = [value for corner in corners for value in corner]
    indices = [
        0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1,
        1, 5, 6, 1, 6, 2, 2, 6, 7, 2, 7, 3, 3, 7, 4, 3, 4, 0,
    ]
    builder.add_shape(group, ferrojt.MeshData(vertices, indices), name="cube")
    builder.save(str(target))
    draw(viewer, target)
