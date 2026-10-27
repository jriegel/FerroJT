"""Tests for the Python bindings.

The samples under ``data/ExampleFiles`` and the reference dumps under
``data/Reference`` are the same fixtures the Rust test suite uses, so the
Python layer is checked against the very same oracle.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import ferrojt

DATA = Path(__file__).resolve().parent.parent / "data"
SAMPLES = DATA / "ExampleFiles"
REFERENCES = DATA / "Reference"

#: A small JT 9.5 file with a reference dump next to it.
BLOCK_95 = SAMPLES / "example_block_jt9.5.jt"
BLOCK_95_REF = REFERENCES / "example_block_jt9.5.json"

needs_sample = pytest.mark.skipif(
    not BLOCK_95.is_file() or not BLOCK_95_REF.is_file(),
    reason="the sample JT files are not checked out",
)


def drop_file_names(value: object) -> object:
    """Remove every ``FileName`` key so paths do not have to match."""
    if isinstance(value, dict):
        value.pop("FileName", None)
        for item in value.values():
            drop_file_names(item)
    elif isinstance(value, list):
        for item in value:
            drop_file_names(item)
    return value


def parsed(text: str) -> object:
    """Parse a dump, ignoring the file name of every object in it."""
    return drop_file_names(json.loads(text))


def test_package_exports() -> None:
    assert ferrojt.__version__
    assert callable(ferrojt.open)
    assert callable(ferrojt.detect_format)
    assert callable(ferrojt.dump)
    assert ferrojt.JtFile is not None
    assert "glb" in ferrojt.formats()


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("gear.jt", "jt"),
        ("gear.GLB", "glb"),
        ("gear.gltf", "gltf"),
        ("gear.usda", "usd"),
        ("no-extension", "jt"),
    ],
)
def test_detect_format(name: str, expected: str) -> None:
    assert ferrojt.detect_format(name) == expected


def test_open_reports_missing_file(tmp_path) -> None:
    missing = tmp_path / "does-not-exist.jt"
    with pytest.raises(OSError):
        ferrojt.open(str(missing))


def test_open_rejects_a_file_that_is_not_jt(tmp_path) -> None:
    empty = tmp_path / "empty.jt"
    empty.write_bytes(b"")
    with pytest.raises(ValueError):
        ferrojt.open(str(empty))


@needs_sample
def test_model_repr_and_counts() -> None:
    model = ferrojt.open(str(BLOCK_95))
    assert model.node_count > 0
    # The file holds three identical levels of detail of one cube; an export
    # carries only the finest one.
    assert model.mesh_count == 1
    assert model.triangle_count == 12
    assert model.vertex_count > 0
    assert repr(model).startswith("Model(nodes=")
    assert any("block" in name for name in model.node_names())


@needs_sample
def test_jt_file_open_and_close() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert not jt.is_open
    # Nothing has been read, so the dump helpers return the empty string.
    assert jt.json() == ""
    assert jt.toc() == ""

    assert jt.open() is True
    assert jt.is_open
    assert jt.FileName == str(BLOCK_95)
    assert jt.file_name == str(BLOCK_95)
    assert repr(jt).endswith("open=True)")

    jt.file_name = str(SAMPLES / "example_block_jt8.1.jt")
    assert jt.FileName == str(SAMPLES / "example_block_jt8.1.jt")


def test_jt_file_reports_an_unreadable_file(tmp_path) -> None:
    missing = tmp_path / "does-not-exist.jt"
    jt = ferrojt.JtFile(str(missing))
    assert jt.open() is False
    assert not jt.is_open
    with pytest.raises(RuntimeError):
        jt.model()


@needs_sample
def test_jt_file_json_matches_the_reference() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    want = json.loads(BLOCK_95_REF.read_text(encoding="utf-8-sig"))
    assert parsed(jt.json()) == drop_file_names(want)


def pairs() -> list[tuple[Path, Path]]:
    """Every sample that has a reference dump next to it."""
    if not SAMPLES.is_dir():
        return []
    found = []
    for sample in sorted(SAMPLES.glob("*.jt")):
        reference = REFERENCES / f"{sample.stem}.json"
        if reference.is_file():
            found.append((sample, reference))
    return found


@pytest.mark.parametrize(
    ("sample", "reference"),
    pairs(),
    ids=[sample.name for sample, _ in pairs()],
)
def test_every_sample_matches_its_reference(sample: Path, reference: Path) -> None:
    """The Python layer reproduces the reference dump of the C++ tool."""
    jt = ferrojt.JtFile(str(sample))
    assert jt.open(), f"{sample} could not be read"

    want = json.loads(reference.read_text(encoding="utf-8-sig"))
    assert parsed(jt.json()) == drop_file_names(want)


@needs_sample
def test_jt_file_toc_is_a_subset_of_the_dump() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    toc_text = jt.toc()
    toc = json.loads(toc_text)
    assert toc["MajorVersion"] == "9"
    assert toc["MinorVersion"] == "5"
    assert len(toc["TocTable"]) == 8

    # DUMP_TOC alone keeps the scene graph but leaves the index arrays empty,
    # DUMP_ALL fills them.
    assert '"Indices":[],' in toc_text
    assert '"Indices":[],' not in jt.json()


@needs_sample
def test_jt_file_dump_matches_the_cli_output() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    assert parsed(jt.json()) == parsed(ferrojt.dump(str(BLOCK_95)))


def test_dump_raises_for_an_unreadable_file(tmp_path) -> None:
    with pytest.raises(OSError):
        ferrojt.dump(str(tmp_path / "nope.jt"))


@needs_sample
def test_jt_file_writes_gltf(tmp_path) -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    assert jt.writeGlTf(str(tmp_path / "block")) is True
    assert (tmp_path / "block.gltf").is_file()

    # An extension that is already there is kept as it is.
    assert jt.write_gltf(str(tmp_path / "block.glb")) is True
    assert (tmp_path / "block.glb").is_file()

    document = json.loads((tmp_path / "block.gltf").read_text(encoding="utf-8"))
    assert document["asset"]["version"]
    assert document["meshes"]


@needs_sample
def test_jt_file_model_is_exportable(tmp_path) -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    model = jt.model()
    assert model.triangle_count == 12
    data = model.to_bytes("glb")
    assert data[:4] == b"glTF"
    assert json.loads(model.to_json())["nodes"] == model.node_count

    out = tmp_path / "model.gltf"
    model.save(str(out))
    assert json.loads(out.read_text(encoding="utf-8"))["meshes"]

    # USD export is a planned feature, not a implemented one.
    with pytest.raises(NotImplementedError):
        model.save(str(tmp_path / "model.usda"))
