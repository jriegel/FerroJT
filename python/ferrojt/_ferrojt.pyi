"""Type stubs for the compiled FerroJT extension module."""

from __future__ import annotations

__version__: str


class Model:
    """A loaded scene: nodes, meshes, materials and their data."""

    @property
    def node_count(self) -> int: ...
    @property
    def mesh_count(self) -> int: ...
    @property
    def material_count(self) -> int: ...
    @property
    def vertex_count(self) -> int: ...
    @property
    def triangle_count(self) -> int: ...

    def node_names(self) -> list[str]: ...
    def to_bytes(self, format: str) -> bytes: ...
    def save(self, path: str, format: str | None = None) -> None: ...
    def to_json(self) -> str: ...


def open(
    path: str,
    *,
    layer_filter: str | None = ...,
    all_layers: bool = ...,
) -> Model:
    """Read a CAD file into a :class:`Model`.

    *layer_filter* selects one of the named layer filters the file declares.
    Leaving it out applies the filter the file marks as active, which drops the
    meshes of every part that is not on one of its layers. *all_layers* keeps
    every part and ignores the filters of the file.
    """


def detect_format(path: str) -> str: ...


class JtFile:
    """Object representing an open JT file.

    Constructed with a path; nothing is read until :meth:`open` is called.
    """

    def __init__(self, file_name: str) -> None: ...

    #: The path of the JT file, the name the original C++ extension uses.
    FileName: str
    #: Snake case alias of :attr:`FileName`.
    file_name: str

    @property
    def is_open(self) -> bool: ...

    def open(self) -> bool: ...
    def json(self) -> str: ...
    def toc(self) -> str: ...
    def writeGlTf(
        self,
        path: str,
        *,
        layer_filter: str | None = ...,
        all_layers: bool = ...,
    ) -> bool: ...
    def write_gltf(
        self,
        path: str,
        *,
        layer_filter: str | None = ...,
        all_layers: bool = ...,
    ) -> bool: ...
    def model(
        self, *, layer_filter: str | None = ..., all_layers: bool = ...
    ) -> Model: ...
    def layer_filters(self) -> list[str]: ...
    def active_layer_filter(self) -> str | None: ...
