"""FerroJT: read OpenJT (JT) and glTF files and convert them to JT / glTF / USD.

The heavy lifting happens in the compiled Rust extension module
:mod:`ferrojt._ferrojt`; this package only re-exports it and adds a couple of
convenience helpers.
"""

from __future__ import annotations

from typing import Any

from ._ferrojt import (
    JtArray,
    JtBuilder,
    JtFile,
    JtLsg,
    JtMesh,
    JtNode,
    MaterialData,
    MeshArray,
    MeshBuffer,
    MeshData,
    MeshHandle,
    Model,
    NodeHandle,
    __version__,
    detect_format,
    open,
    read_mesh,
)

__all__ = [
    "JtArray",
    "JtBuilder",
    "JtFile",
    "JtLsg",
    "JtMesh",
    "JtNode",
    "MaterialData",
    "MeshArray",
    "MeshBuffer",
    "MeshData",
    "MeshHandle",
    "Model",
    "NodeHandle",
    "open",
    "read_mesh",
    "detect_format",
    "__version__",
    "formats",
    "read",
    "dump",
]


def formats() -> list[str]:
    """Return the container formats FerroJT knows about."""
    return ["jt", "gltf", "glb", "usd"]


def read(path: str, **kwargs: Any) -> Model:
    """Alias for :func:`open` that reads a CAD file into a :class:`Model`.

    Accepts the same keywords: ``layer_filter="NAME"`` selects one of the
    layer filters the file declares, and ``all_layers=True`` exports every
    part instead of applying the filter of the file (both only apply to JT).
    ``flatten=True`` merges everything that was read into a single mesh.
    """
    return open(path, **kwargs)


def dump(path: str) -> str:
    """Return the JSON dump of a JT file, as the ``jt-dump`` tool writes it."""
    jt_file = JtFile(path)
    if not jt_file.open():
        msg = f"{path} is not a readable JT file"
        raise OSError(msg)
    return jt_file.json()
