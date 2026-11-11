"""Tests for the Python bindings.

The samples under ``data/ExampleFiles`` and the reference dumps under
``data/Reference`` are the same fixtures the Rust test suite uses, so the
Python layer is checked against the very same oracle.
"""

from __future__ import annotations

import json
import struct
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
    for name in ("JtLsg", "JtNode", "JtMesh", "JtArray"):
        assert getattr(ferrojt, name) is not None, name
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

    usd = model.to_bytes("usd").decode("utf-8")
    assert usd.startswith("#usda 1.0\n(")
    assert "metersPerUnit = 0.001" in usd
    assert 'upAxis = "Z"' in usd

    usd_file = tmp_path / "model.usda"
    model.save(str(usd_file))
    text = usd_file.read_text(encoding="utf-8")
    # Each mesh is authored once in an invisible library and referenced by the
    # node that draws it.
    assert 'def Xform "FerroJT_Geometry"' in text
    assert 'token visibility = "invisible"' in text
    assert "point3f[] points" in text
    assert "prepend references = [</FerroJT_Geometry/GEOMETRY_0>]" in text
    assert text.count("point3f[] points") == model.mesh_count


@needs_sample
def test_usd_export_reuses_materials(tmp_path) -> None:
    """Two parts, two appearances: the plate file keeps both, the block file
    collapses its repeated colour attribute into one material."""
    for name, materials in (("example_block_jt9.5.jt", 1), ("opening_protection_plate1_jt9.5.jt", 2)):
        model = ferrojt.open(str(SAMPLES / name))
        text = model.to_bytes("usd").decode("utf-8")
        assert text.count('def Material "Material_') == materials
        assert text.count("def Xform \"GEOMETRY_") == model.mesh_count
        out = tmp_path / f"{name}.usda"
        model.save(str(out))
        assert out.read_text(encoding="utf-8") == text


#: The diffuse colours a sample paints its parts with, sorted, as its
#: reference dump states them. The plate files are the revealing ones: a grey
#: plate and a yellow-green screw, which is the appearance a viewer shows. The
#: 8.x writers round the cube colour differently from the 9.x ones.
COLOURS = {
    "example_block_jt10.3.jt": [[0.596063, 0.666667, 0.686259, 1.0]],
    "example_block_jt8.1.jt": [[0.6, 0.6, 0.6, 1.0]],
    "example_block_jt9.5.jt": [[0.596063, 0.666667, 0.686259, 1.0]],
    "opening_protection_plate1_jt8.0.jt": [
        [0.6, 0.6, 0.6, 1.0],
        [0.8, 1.0, 0.2, 1.0],
    ],
    "opening_protection_plate1_jt9.5.jt": [
        [0.6, 0.6, 0.6, 1.0],
        [0.8, 1.0, 0.2, 1.0],
    ],
}


def colour_ids() -> list[str]:
    return [name for name in COLOURS if (SAMPLES / name).is_file()]


def glb_json(data: bytes) -> dict:
    """Return the JSON document of a GLB blob.

    A GLB is a 12 byte header, then a chunk whose 4 byte length comes before
    its type, so the JSON itself starts at offset 20.
    """
    assert data[:4] == b"glTF", "not a GLB"
    (json_length,) = struct.unpack_from("<I", data, 12)
    return json.loads(data[20 : 20 + json_length])


@needs_sample
@pytest.mark.parametrize("name", colour_ids(), ids=colour_ids())
def test_the_colour_of_a_sample_reaches_the_gltf(name: str) -> None:
    """The colour of a JT part has to arrive as ``baseColorFactor``.

    ``Model.to_json()`` only reports counts, so the exported document is
    parsed to see the materials themselves.
    """
    model = ferrojt.read(str(SAMPLES / name))
    document = glb_json(model.to_bytes("glb"))

    materials = document["materials"]
    # A file repeats one appearance on every shape of a part, so the export
    # holds one material per distinct colour, not one per attribute.
    assert len(materials) == len(COLOURS[name]), f"{name}: materials were duplicated"

    got = sorted(
        material["pbrMetallicRoughness"]["baseColorFactor"] for material in materials
    )
    for colour, want in zip(got, sorted(COLOURS[name])):
        # The reference dumps print six significant digits, so compare loosely.
        assert all(abs(a - b) < 1e-5 for a, b in zip(colour, want)), name

    # The reference glTF writer pairs the colour with a matte, double sided
    # look. A roughness derived from the shininess exponent of a JT file would
    # make every part a mirror and blow the colour out to white.
    for material in materials:
        pbr = material["pbrMetallicRoughness"]
        assert pbr["metallicFactor"] == 0.2
        assert pbr["roughnessFactor"] == 0.8
        assert material["doubleSided"] is True
        # What a metallic/roughness workflow cannot express survives in
        # ``extras``, so a consumer does not have to re-read the JT file.
        assert set(material["extras"]) >= {
            "ambient",
            "specular",
            "shininess",
            "reflectivity",
        }, name

    # And every mesh has to point at one of those materials.
    for mesh in document["meshes"]:
        for primitive in mesh["primitives"]:
            assert primitive["material"] < len(materials), name


# ---------------------------------------------------------------------------
# The scene graph of a file: JtLsg, JtNode, JtMesh and JtArray.
#
# These handles expose the LSG of the JT file itself rather than the neutral
# model the exporters build, so every LOD tier of a file is visible and the
# representation id of a mesh is the identity that instancing shares.
# ---------------------------------------------------------------------------

PLATE_95 = SAMPLES / "opening_protection_plate1_jt9.5.jt"
PLATE_80 = SAMPLES / "opening_protection_plate1_jt8.0.jt"

needs_plate95 = pytest.mark.skipif(
    not PLATE_95.is_file(), reason="the plate sample is not checked out"
)
needs_plate80 = pytest.mark.skipif(
    not PLATE_80.is_file(), reason="the 8.0 plate sample is not checked out"
)


@needs_sample
def test_lsg_reports_the_scene_graph_of_a_file() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    graph = jt.lsg
    assert graph is not None
    assert graph.path.endswith("example_block_jt9.5.jt")

    # The block file is a Partition with one Meta Data child, one Part and
    # three LOD tiers of one shape.
    assert graph.node_count == 10
    assert graph.representation_count == 3
    assert len(graph.nodes()) == graph.node_count

    root = graph.root()
    assert root is not None and root == jt.root()
    assert root.class_name == "Partition"
    assert root.id == 0
    assert root.position == 0
    assert root.name == "example_block_nx8.5.asm"
    assert root.partition_file_name == "example_block_nx8_5.jt"
    assert root.up_axis == 0
    assert len(root) == root.child_count() == 1
    assert [child.class_name for child in root] == ["MetaData"]
    assert root[0] == root[-1] == graph.nodes()[1]
    assert len(root.descendants()) == graph.node_count - 1

    # A Partition carries neither transform nor properties.
    assert root.transform is None
    assert root.matrix is None
    assert root.properties() == {}
    assert root.attribute_names == []
    assert root.material() is None
    assert root.late_loads() == []

    classes = [node.class_name for node in graph.nodes()]
    assert classes[0] == "Partition"
    assert "Part" in classes and "Shape" in classes
    assert repr(graph).startswith("JtLsg(path=")


@needs_sample
def test_a_node_reports_its_own_jt_details() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    graph = jt.lsg

    part = graph.nodes()[2]
    assert part.class_name == "Part"
    assert part.name == "example_block_750.part"
    # The key/value stream of the late loads: the same pairs the dump prints.
    # Note that ``Name::`` is the property the translator wrote, which is not
    # always the object name of the node — the block file spells the one
    # without the extension and the other with it.
    properties = part.properties()
    assert properties["Name::"] == "example_block_750"
    assert properties["LAYER::"] == "1"
    assert properties["CAD_MASS::"]
    assert part.layers == [1]

    shape = next(node for node in graph.nodes() if node.class_name == "Shape")
    assert shape.bounds == [0.0, 0.0, 0.0, 100.0, 80.0, 60.0]
    assert shape.area == pytest.approx(37600.0)
    assert shape.is_tri_strip_set is True
    assert shape.attribute_names == ["Material"]
    material = shape.material()
    assert material is not None
    assert set(material) == {
        "ambient",
        "diffuse",
        "specular",
        "emission",
        "shininess",
        "reflectivity",
    }
    assert material["diffuse"] == pytest.approx(
        [0.596063, 0.666667, 0.686259, 1.0], abs=1e-5
    )
    # The shape itself has no transform attribute — the segment type of its
    # representation is the plain "Shape" of a JT 9.x file.
    assert shape.transform is None
    assert shape.mesh_ids == [4]
    assert shape.subtree_mesh_ids() == [4]
    assert [mesh.id for mesh in shape.meshes()] == [4]
    assert [mesh.id for mesh in shape.subtree_meshes()] == [4]


@needs_sample
def test_lsg_meshes_are_the_representations_of_the_file() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    graph = jt.lsg

    meshes = graph.meshes()
    assert [mesh.id for mesh in meshes] == [4, 5, 6]
    assert [mesh.id for mesh in jt.meshes()] == [4, 5, 6]
    for mesh in meshes:
        assert mesh.element_class == "JtElement_ShapeLOD_TriStripSet"
        assert mesh.segment_type_name == "Shape"
        assert mesh.segment
        assert mesh.index_count == 36
        assert mesh.vertex_count == 24
        assert mesh.normal_count == 24
        assert mesh.triangle_count == 12
        assert mesh.is_empty is False
        assert mesh.normal_binding is None
        assert mesh.array_names == ["indices", "vertices", "normals"]
        assert repr(mesh).startswith("JtMesh(id=")

    # JT 9.x stores one tier per "Shape" segment, so all three tiers of the
    # block are visible here even though the export keeps only the finest.
    assert sum(mesh.triangle_count for mesh in meshes) == 36

    assert graph.mesh(4) == meshes[0]
    assert hash(graph.mesh(4)) == hash(meshes[0])
    with pytest.raises(IndexError, match="no representation"):
        graph.mesh(0)
    with pytest.raises(IndexError, match="no representation"):
        graph.mesh(999)


@needs_plate95
def test_an_instance_shares_one_representation_instead_of_copying_it() -> None:
    """The plate file holds one screw and instantiates it twice.

    The two ``Instance`` nodes carry no geometry of their own; they point at
    one shared subtree, so both see the same representation ids.
    """
    jt = ferrojt.JtFile(str(PLATE_95))
    assert jt.open()
    graph = jt.lsg

    assert graph.node_count == 21
    assert graph.representation_count == 6
    assert [mesh.id for mesh in graph.meshes()] == [4, 5, 6, 11, 12, 13]

    instances = [node for node in graph.nodes() if node.is_instance]
    assert len(instances) == 2
    assert [node.name for node in instances] == ["shcs.asm", "shcs.asm"]
    assert [node.position for node in instances] == [10, 20]

    first, second = instances
    assert first.instance_target() == second.instance_target()
    target = first.instance_target()
    assert target is not None
    assert target.class_name == "MetaData"
    # The layer tag of the plate sits on that meta data, not on the part.
    assert target.layers == [20]
    assert target.properties()["LAYER::"] == "20"

    # An instance has no representation itself, only the subtree it targets.
    assert first.mesh_ids == [] and second.mesh_ids == []
    assert first.subtree_mesh_ids() == second.subtree_mesh_ids() == [11, 12, 13]
    assert [mesh.id for mesh in first.subtree_meshes()] == [11, 12, 13]

    # The two placements differ only by the translation of the instance.
    assert first.transform["translation"] == [0.0, -25.0, 15.0]
    assert second.transform["translation"] == [0.0, 25.0, 15.0]
    for node in (first, second):
        assert node.rotation == [0.0, 0.0, 0.0, 1.0]
        assert node.scale == [1.0, 1.0, 1.0]
        assert node.matrix is not None
    assert first.translation == [0.0, -25.0, 15.0]

    # One screw, two placements: the shared representations appear once each.
    screw = [mesh for mesh in graph.meshes() if mesh.id in (11, 12, 13)]
    assert len(screw) == 3


@needs_plate80
def test_an_8_x_file_names_its_lod_tiers_by_segment_type() -> None:
    """JT 8.x uses one segment type per tier, JT 9.x uses "Shape" for all."""
    jt = ferrojt.JtFile(str(PLATE_80))
    assert jt.open()
    graph = jt.lsg

    assert graph.node_count == 25
    assert graph.representation_count == 8
    meshes = graph.meshes()
    assert [mesh.id for mesh in meshes] == [4, 5, 6, 7, 11, 12, 13, 14]
    assert [mesh.segment_type_name for mesh in meshes] == [
        "Shape LOD0",
        "Shape LOD1",
        "Shape LOD2",
        "Shape LOD3",
        "Shape LOD0",
        "Shape LOD1",
        "Shape LOD2",
        "Shape LOD3",
    ]
    assert [mesh.segment_type for mesh in meshes] == [7, 8, 9, 10, 7, 8, 9, 10]
    # The 8.x writers store a per-vertex normal binding.
    assert {mesh.normal_binding for mesh in meshes} == {1}

    instances = [node for node in graph.nodes() if node.is_instance]
    assert [node.subtree_mesh_ids() for node in instances] == [
        [11, 12, 13, 14],
        [11, 12, 13, 14],
    ]


@needs_sample
def test_mesh_arrays_are_numpy_compatible_and_zero_copy() -> None:
    numpy = pytest.importorskip("numpy")
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    mesh = jt.lsg.mesh(4)

    arrays = mesh.arrays()
    assert list(arrays) == ["indices", "vertices", "normals"]

    indices = numpy.asarray(mesh.array("indices"))
    vertices = numpy.asarray(mesh.array("vertices"))
    normals = numpy.asarray(mesh.array("normals"))

    assert indices.dtype == numpy.int32
    assert indices.shape == (36,)
    assert vertices.dtype == numpy.float32
    assert vertices.shape == (24, 3)
    assert normals.shape == (24, 3)

    # Zero copy: NumPy borrows the decoded arrays of the file.
    assert indices.flags.owndata is False
    assert vertices.flags.owndata is False
    assert numpy.shares_memory(vertices, numpy.asarray(mesh.array("vertices")))
    assert numpy.shares_memory(vertices, numpy.asarray(memoryview(mesh.array("vertices"))))

    # The buffer itself is read-only and shaped like the channel.
    view = memoryview(mesh.array("vertices"))
    assert view.readonly is True
    assert view.ndim == 2
    assert view.shape == (24, 3)
    assert view.strides == (12, 4)
    assert view.format == "f"
    assert view.nbytes == 24 * 3 * 4
    with pytest.raises((TypeError, NotImplementedError)):
        view[0][0] = 1.0
    # NumPy sees the read-only flag and refuses to hand out a writable view.
    with pytest.raises(ValueError):
        numpy.asarray(mesh.array("vertices")).flags.writeable = True

    # The handle agrees with what NumPy sees.
    array = mesh.array("vertices")
    assert array.ndim == 2
    assert array.shape == [24, 3]
    assert array.itemsize == 4
    assert array.format == "f"
    assert array.mesh_id == 4
    assert len(array) == len(view) == len(vertices) == 24
    assert repr(array).startswith("JtArray(mesh=4")

    # The values are the ones the file holds: the block spans 100 x 80 x 60.
    assert vertices.min(axis=0).tolist() == [0.0, 0.0, 0.0]
    assert vertices.max(axis=0).tolist() == [100.0, 80.0, 60.0]
    assert indices.min() == 0
    assert indices.max() == 23

    with pytest.raises(ValueError, match="unknown array"):
        mesh.array("bogus")


@needs_sample
def test_node_handles_compare_by_position_in_the_graph() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    graph = jt.lsg

    assert graph.root() == jt.root() == graph.nodes()[0]
    assert hash(graph.root()) == hash(jt.root())
    assert graph.nodes()[0] != graph.nodes()[1]
    assert graph.root() != 7

    root = graph.root()
    with pytest.raises(IndexError, match="it has 1"):
        root[5]
    assert root[-1] is not None
    assert list(root) == [root[0]]
    assert iter(root).__class__.__name__ != "list"
    assert root.unresolved_children() == 0
    assert repr(root).startswith('JtNode(class="Partition"')


#: The layer filters each sample declares, and the one an export uses by
#: default. A JT 8.x file declares none at all, and the newer ones all leave
#: ``ACTIVELAYERFILTER::`` empty, so the first name in sorted order wins.
FILTERS = {
    "example_block_jt10.3.jt": (["ALL", "LAYCUR"], "ALL"),
    "example_block_jt8.1.jt": ([], None),
    "example_block_jt9.5.jt": (["ALL", "LAYCUR"], "ALL"),
    "opening_protection_plate1_jt8.0.jt": ([], None),
    "opening_protection_plate1_jt9.5.jt": (["ALL", "LAYCUR"], "ALL"),
}


def sample_ids() -> list[str]:
    return [name for name in FILTERS if (SAMPLES / name).is_file()]


@needs_sample
def test_the_declared_layer_filters_are_the_ones_of_the_reference() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    assert jt.layer_filters() == ["ALL", "LAYCUR"]
    assert jt.active_layer_filter() == "ALL"

    # The dump still shows every filter of the file, filtered export or not.
    assert "layer-filter" in jt.json()


@pytest.mark.parametrize("name", sample_ids(), ids=sample_ids())
def test_layer_filters_of_a_sample(name: str) -> None:
    declared, default = FILTERS[name]
    jt = ferrojt.JtFile(str(SAMPLES / name))
    assert jt.open(), f"{name} could not be read"

    assert jt.layer_filters() == declared
    assert jt.active_layer_filter() == default


@pytest.mark.parametrize("name", sample_ids(), ids=sample_ids())
def test_no_sample_loses_a_part_to_its_own_filter(name: str) -> None:
    declared, _ = FILTERS[name]
    jt = ferrojt.JtFile(str(SAMPLES / name))
    assert jt.open(), f"{name} could not be read"

    filtered = jt.model()
    unfiltered = jt.model(all_layers=True)
    assert filtered.node_count == unfiltered.node_count
    assert filtered.triangle_count == unfiltered.triangle_count

    # Asking for a filter the file does not declare is an error, and the
    # message names the ones it does.
    if declared:
        with pytest.raises(ValueError, match=", ".join(declared)):
            jt.model(layer_filter="nope")
        assert jt.model(layer_filter=declared[-1]).triangle_count == filtered.triangle_count
    else:
        with pytest.raises(ValueError, match="no layer filters"):
            jt.model(layer_filter="nope")


@needs_sample
def test_layer_filter_and_all_layers_cannot_be_combined() -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    with pytest.raises(ValueError, match="cannot be combined"):
        jt.model(layer_filter="ALL", all_layers=True)
    with pytest.raises(ValueError, match="cannot be combined"):
        ferrojt.open(str(BLOCK_95), layer_filter="ALL", all_layers=True)


@needs_sample
def test_read_accepts_the_layer_keywords(tmp_path) -> None:
    model = ferrojt.read(str(BLOCK_95))
    assert model.triangle_count == 12

    # A file without layer information exports everything either way.
    other = ferrojt.read(str(SAMPLES / "example_block_jt8.1.jt"), all_layers=True)
    assert other.triangle_count > 0

    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    assert jt.write_gltf(str(tmp_path / "laycur"), layer_filter="LAYCUR") is True
    assert (tmp_path / "laycur.gltf").is_file()
    assert jt.writeGlTf(str(tmp_path / "all"), all_layers=True) is True
    assert (tmp_path / "all.gltf").is_file()


@needs_sample
def test_a_file_without_layer_information_has_no_default_filter() -> None:
    jt = ferrojt.JtFile(str(SAMPLES / "example_block_jt8.1.jt"))
    assert jt.open()

    assert jt.layer_filters() == []
    assert jt.active_layer_filter() is None
    assert jt.model().triangle_count == jt.model(all_layers=True).triangle_count


#: The one sample without a reference dump. It is also the only one whose file
#: declares representations with no vertices at all — 36 of them.
SERVER = SAMPLES / "SampleJT_Server.jt"

needs_server = pytest.mark.skipif(
    not SERVER.is_file(), reason="the server sample is not checked out"
)


@needs_server
def test_shapes_without_vertices_are_left_out_of_the_export() -> None:
    """A representation that declares no vertices keeps its node, not a mesh.

    glTF requires a ``POSITION`` attribute in every primitive, so such a shape
    used to abort the whole document with "a primitive without a POSITION
    attribute cannot be exported to glTF".
    """
    model = ferrojt.read(str(SERVER))
    assert model.node_count == 2158
    assert model.mesh_count == 105
    assert model.triangle_count == 124154

    document = glb_json(model.to_bytes("glb"))
    assert len(document["nodes"]) == model.node_count
    assert len(document["meshes"]) == model.mesh_count
    for mesh in document["meshes"]:
        for primitive in mesh["primitives"]:
            assert "POSITION" in primitive["attributes"], "a primitive has no POSITION"
            assert primitive["attributes"]["POSITION"] is not None


@needs_server
def test_the_file_view_keeps_a_representation_without_vertices() -> None:
    """The export drops a shape with no vertices, the file view keeps it.

    ``SampleJT_Server.jt`` declares 36 such shapes; they are instanced, so the
    file has exactly one representation for them. The arrays stay empty rather
    than missing, which lets a consumer walk the graph without special cases.
    """
    numpy = pytest.importorskip("numpy")
    jt = ferrojt.JtFile(str(SERVER))
    assert jt.open()
    graph = jt.lsg

    assert graph.node_count == 2158
    assert len(graph.nodes()) == 2158
    assert len(graph.root().descendants()) == 2157

    # 106 representations, one of them empty — the export keeps the other 105.
    assert graph.representation_count == 106
    empty = [mesh.id for mesh in graph.meshes() if mesh.is_empty]
    assert len(empty) == 1
    assert len([mesh for mesh in graph.meshes() if not mesh.is_empty]) == 105

    mesh = graph.mesh(empty[0])
    assert mesh.vertex_count == 0
    assert mesh.triangle_count == 0
    for name in mesh.array_names:
        array = mesh.array(name)
        assert len(array) == 0
        data = numpy.asarray(array)
        assert data.size == 0
        assert data.dtype == (numpy.int32 if name == "indices" else numpy.float32)
        assert memoryview(array).nbytes == 0


# ---------------------------------------------------------------------------
# Writing a file back out at the revision of the caller's choice.
#
# JT frames nearly everything by major version and stores its triangles in one
# of two representations: 8.x uses the vertex-based lossless one, 9.x and 10.x
# the topologically compressed coder. A rewrite therefore stays inside the
# family of the source, and inside a family it only moves the version gates.
# ---------------------------------------------------------------------------


def triangle_set(path: str) -> list[tuple[bytes, bytes, bytes]]:
    """Every triangle of a file as the bytes of its three corner positions.

    Each triangle is rotated to start at its smallest corner and the whole list
    is sorted, so the set does not depend on how a representation lays out its
    vertex table. That is the only comparison that survives a revision change:
    the topological coder of JT 9.x and 10.x keeps one vertex instance per
    triangle corner and orders them differently from the source.
    """
    numpy = pytest.importorskip("numpy")
    jt = ferrojt.JtFile(path)
    assert jt.open()
    out = []
    for mesh in jt.lsg.meshes():
        if mesh.is_empty:
            continue
        vertices = numpy.asarray(mesh.array("vertices")).reshape(-1, 3)
        for triangle in numpy.asarray(mesh.array("indices")).reshape(-1, 3):
            corners = [vertices[index].tobytes() for index in triangle]
            start = min(range(3), key=lambda at: corners[at])
            out.append(tuple(corners[(start + at) % 3] for at in range(3)))
    return sorted(out)


def toc_of(path: str) -> dict[str, object]:
    """The version banner and the segment types of a file."""
    jt = ferrojt.JtFile(path)
    assert jt.open()
    return json.loads(jt.toc())


@needs_sample
def test_a_write_without_a_revision_keeps_the_revision_of_the_source(tmp_path) -> None:
    source = tmp_path / "block.jt"
    source.write_bytes(BLOCK_95.read_bytes())

    jt = ferrojt.JtFile(str(source))
    assert jt.open()
    target = tmp_path / "again.jt"
    assert jt.write_jt(str(target), all_lods=True, all_layers=True) is True

    toc = toc_of(str(target))
    assert (toc["MajorVersion"], toc["MinorVersion"]) == ("9", "5")
    assert len(toc["TocTable"]) == 8
    assert triangle_set(str(target)) == triangle_set(str(BLOCK_95))


@needs_sample
def test_a_write_without_a_revision_drops_the_coarse_levels_of_detail(
    tmp_path,
) -> None:
    """The default is the export view, not a copy.

    Leaving ``all_lods`` out keeps only the finest tier, which is what an
    exporter wants — and it is what makes the rewrite smaller than the source.
    The revision still stays at 9.5, because keeping it is what allows the
    segments in between to be copied byte for byte.
    """
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    target = tmp_path / "finest.jt"
    assert jt.write_jt(str(target)) is True

    toc = toc_of(str(target))
    assert (toc["MajorVersion"], toc["MinorVersion"]) == ("9", "5")
    assert len(toc["TocTable"]) == 6, "two shape segments less than the source"

    again = ferrojt.JtFile(str(target))
    assert again.open()
    assert again.lsg.node_count == 10, "no node is ever dropped"
    assert again.lsg.representation_count == 1
    assert again.model().triangle_count == 12
    # The nodes that lost their representation are still dumped, and their
    # attributes still close the object cleanly.
    assert json.loads(again.json())["LSG"] is not None


@needs_plate95
def test_a_file_can_be_written_at_another_revision(tmp_path) -> None:
    jt = ferrojt.JtFile(str(PLATE_95))
    assert jt.open()
    target = tmp_path / "plate10.jt"
    assert jt.write_jt(str(target), all_lods=True, all_layers=True, version="10.3") is True

    toc = toc_of(str(target))
    assert (toc["MajorVersion"], toc["MinorVersion"]) == ("10", "3")
    assert len(toc["TocTable"]) == 15
    # JT 10.x names its levels of detail by segment type 7, 8 and 9; the 9.5
    # source calls all six of them type 6.
    kinds = sorted(row[1] for row in toc["TocTable"])
    assert kinds == [1, 3, 3, 4, 4, 4, 4, 7, 7, 8, 8, 9, 9, 17, 17]

    again = ferrojt.JtFile(str(target))
    assert again.open()
    assert again.lsg.node_count == 21
    assert again.lsg.representation_count == 6
    assert sum(mesh.triangle_count for mesh in again.lsg.meshes()) == 922
    assert triangle_set(str(target)) == triangle_set(str(PLATE_95))


@needs_sample
def test_a_revision_change_explodes_the_vertex_table(tmp_path) -> None:
    numpy = pytest.importorskip("numpy")
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()
    target = tmp_path / "block10.jt"
    assert jt.write_jt(str(target), all_lods=True, all_layers=True, version="10.3") is True

    again = ferrojt.JtFile(str(target))
    assert again.open()
    before = sorted(mesh.index_count for mesh in jt.lsg.meshes())
    after = sorted(mesh.index_count for mesh in again.lsg.meshes())
    assert before == after, "the coder keeps the triangle count"

    # The vertex table is what the coder changes: a 24-corner cube comes back
    # as 36 instances per representation, because the dual walk keeps one
    # vertex instance per triangle corner. The index count, and therefore the
    # triangle count, is untouched.
    def tables(graph) -> list[int]:
        return sorted(mesh.vertex_count for mesh in graph.meshes())

    assert tables(jt.lsg) == [24, 24, 24]
    assert tables(again.lsg) == [36, 36, 36]
    assert numpy.asarray(again.lsg.meshes()[0].array("indices")).tolist() == list(range(36))


@needs_sample
def test_the_two_representation_families_cannot_be_exchanged(tmp_path) -> None:
    want = "only the coder of the revision of the source file"
    for name, revision in [
        ("example_block_jt8.1.jt", "10.3"),
        ("example_block_jt9.5.jt", "8.1"),
        ("opening_protection_plate1_jt8.0.jt", "9.5"),
    ]:
        jt = ferrojt.JtFile(str(SAMPLES / name))
        assert jt.open()
        with pytest.raises(ValueError, match=want):
            jt.write_jt(str(tmp_path / f"{name}-{revision}.jt"), version=revision)


@needs_server
def test_a_mesh_the_coder_cannot_hold_is_refused(tmp_path) -> None:
    """The topological coder needs a closed two-manifold.

    ``SampleJT_Server.jt`` is a real CAD assembly with open edges, so its
    shapes cannot be re-encoded — the write is refused rather than producing a
    file whose shapes come back empty.
    """
    jt = ferrojt.JtFile(str(SERVER))
    assert jt.open()
    with pytest.raises(ValueError, match="not a closed two-manifold"):
        jt.write_jt(str(tmp_path / "server10.jt"), version="10.3")

    # A segment whose framing depends on the revision cannot be copied either:
    # JT 10.x added the info segment, which no 9.x file can hold.
    block = ferrojt.JtFile(str(SAMPLES / "example_block_jt10.3.jt"))
    assert block.open()
    with pytest.raises(ValueError, match="segment type 31"):
        block.write_jt(str(tmp_path / "block95.jt"), version="9.5")


@needs_sample
def test_an_unsupported_revision_is_refused(tmp_path) -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    with pytest.raises(ValueError, match="cannot be written"):
        jt.write_jt(str(tmp_path / "nope.jt"), version="11")
    with pytest.raises(ValueError, match="is not a JT version number"):
        jt.write_jt(str(tmp_path / "nope.jt"), version="nonsense")

    # A bare major picks the minor this crate writes for that revision.
    target = tmp_path / "bare.jt"
    assert jt.write_jt(str(target), version="9") is True
    assert toc_of(str(target))["MinorVersion"] == "5"


@needs_sample
def test_write_jt_rejects_an_impossible_layer_choice(tmp_path) -> None:
    jt = ferrojt.JtFile(str(BLOCK_95))
    assert jt.open()

    with pytest.raises(ValueError, match="cannot be combined"):
        jt.write_jt(str(tmp_path / "clash.jt"), layer_filter="ALL", all_layers=True)
    with pytest.raises(ValueError, match="nope"):
        jt.write_jt(str(tmp_path / "nope.jt"), layer_filter="nope")
    assert not (tmp_path / "clash.jt").exists()
    assert not (tmp_path / "nope.jt").exists()
