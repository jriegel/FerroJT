"""Smoke tests for the Python bindings.

Reading JT files is not implemented yet, so the tests that would need a real
model are skipped rather than failing.
"""

from __future__ import annotations

import pytest

import ferrojt


def test_package_exports() -> None:
    assert ferrojt.__version__
    assert callable(ferrojt.open)
    assert callable(ferrojt.detect_format)
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
    with pytest.raises(RuntimeError):
        ferrojt.open(str(missing))


def test_open_jt_is_not_implemented_yet(tmp_path) -> None:
    empty = tmp_path / "empty.jt"
    empty.write_bytes(b"")
    with pytest.raises(NotImplementedError):
        ferrojt.open(str(empty))


def test_model_repr_and_counts() -> None:
    # No reader exists yet, so there is nothing to build a Model from.
    pytest.skip("needs a JT reader or a model constructor in the bindings")
