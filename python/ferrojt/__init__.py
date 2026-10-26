"""FerroJT: read OpenJT (JT) files and convert them to glTF / USD.

The heavy lifting happens in the compiled Rust extension module
:mod:`ferrojt._ferrojt`; this package only re-exports it and adds a couple of
convenience helpers.
"""

from __future__ import annotations

from ._ferrojt import JtFile, Model, __version__, detect_format, open

__all__ = [
    "JtFile",
    "Model",
    "open",
    "detect_format",
    "__version__",
    "formats",
    "read",
    "dump",
]


def formats() -> list[str]:
    """Return the container formats FerroJT knows about."""
    return ["jt", "gltf", "glb", "usd"]


def read(path: str) -> Model:
    """Alias for :func:`open` that reads a CAD file into a :class:`Model`."""
    return open(path)


def dump(path: str) -> str:
    """Return the JSON dump of a JT file, as the ``jt-dump`` tool writes it."""
    jt_file = JtFile(path)
    if not jt_file.open():
        msg = f"{path} is not a readable JT file"
        raise OSError(msg)
    return jt_file.json()
