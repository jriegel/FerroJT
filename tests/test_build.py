"""Building a JT file from nothing through the Python wrapper.

The same proof the Rust builder tests use, from the other side: create an empty
file, fill its scene graph with nodes, attach a cube mesh, instance the part at
two different places, save it and read it back. What comes back has to be what
went in — the graph, the names, the placements, and the twelve triangles of the
cube.
"""

from __future__ import annotations

import json

import numpy as np
import pytest

import ferrojt

#: Half the edge length of the cube.
HALF = 5.0

#: The eight corners of the cube.
CORNERS = [
    [-HALF, -HALF, -HALF],
    [HALF, -HALF, -HALF],
    [HALF, HALF, -HALF],
    [-HALF, HALF, -HALF],
    [-HALF, -HALF, HALF],
    [HALF, -HALF, HALF],
    [HALF, HALF, HALF],
    [-HALF, HALF, HALF],
]

#: The six faces, corners counter-clockwise seen from outside so the normal of
#: the plane points away from the cube.
FACES = [
    [0, 3, 2, 1],
    [4, 5, 6, 7],
    [0, 1, 5, 4],
    [2, 3, 7, 6],
    [1, 2, 6, 5],
    [3, 0, 4, 7],
]

#: The twelve triangles as indices into :data:`CORNERS`.
TRIANGLES = [
    corner
    for face in FACES
    for corner in (face[0], face[1], face[2], face[0], face[2], face[3])
]

#: The corners of the cube as a flat list, the way a modeller hands them over.
VERTICES = [value for corner in CORNERS for value in corner]


def cube() -> ferrojt.MeshData:
    """The cube, shared corners and all.

    A JT 8.x faceted shape cannot share a corner, because the normal belongs to
    the vertex slot, so the builder explodes the mesh to one slot per triangle
    corner — 36 slots for 12 triangles.
    """
    return ferrojt.MeshData(VERTICES, TRIANGLES)


def build() -> tuple[ferrojt.JtBuilder, ferrojt.NodeHandle]:
    """A partition holding one named part with a cube, instanced twice.

    Returns the builder and the part, so a test can reach the instances through
    the children of the root.
    """
    builder = ferrojt.JtBuilder()
    root = builder.root()
    part = builder.add_node(root, "Part", "cube")
    shape = builder.add_shape(part, cube(), name="cube shape")
    builder.set_material(
        shape, ferrojt.MaterialData(diffuse=[0.2, 0.6, 0.9, 1.0])
    )
    builder.set_translation(part, [-20.0, 0.0, 0.0])
    first = builder.instance_of(root, part)
    second = builder.instance_of(root, part)
    builder.set_translation(first, [-20.0, 0.0, 0.0])
    builder.set_translation(second, [20.0, 0.0, 0.0])
    return builder, part


def test_a_new_builder_holds_only_its_root() -> None:
    builder = ferrojt.JtBuilder()
    assert builder.node_count == 1
    assert builder.mesh_count == 0
    assert builder.compress is True

    root = builder.root()
    assert root.index == 0
    assert root.class_name == "Partition"
    assert root.name is None
    assert builder.children(root) == []
    assert builder.class_of(root) == "Partition"
    assert builder.name(root) is None


def test_the_mesh_of_the_cube_is_exploded() -> None:
    mesh = cube()
    assert mesh.triangle_count == 12
    assert mesh.vertex_count == 36, "one slot per triangle corner"
    assert len(mesh.vertices) == 108
    assert len(mesh.normals) == 108, "a faceted normal cannot be shared"
    assert mesh.bounds == [-HALF, -HALF, -HALF, HALF, HALF, HALF]
    mesh.validate()

    # Every corner of the source cube comes back once per triangle that uses it.
    exploded = np.reshape(mesh.vertices, (-1, 3)).tolist()
    used = [tuple(CORNERS[corner]) for corner in TRIANGLES]
    assert sorted(map(tuple, exploded)) == sorted(used)


def test_a_mesh_without_triangles_is_refused() -> None:
    with pytest.raises(ValueError):
        ferrojt.MeshData([0.0, 0.0, 0.0], [])
    with pytest.raises(ValueError):
        ferrojt.MeshData([0.0, 0.0, 0.0], [0, 1, 2, 0, 2])
    with pytest.raises(ValueError):
        ferrojt.MeshData([0.0, 0.0, 0.0], [0, 1, 5])


def test_an_unknown_node_class_is_refused() -> None:
    builder = ferrojt.JtBuilder()
    with pytest.raises(ValueError, match="unknown node class"):
        builder.add_node(builder.root(), "Bolt", "bolt")


def test_a_leaf_cannot_hold_children_and_the_root_cannot_be_instanced() -> None:
    builder = ferrojt.JtBuilder()
    shape = builder.add_shape(builder.root(), cube(), name="solid")
    with pytest.raises(ValueError, match="children"):
        builder.add_node(shape, "Group", "under")
    instance = builder.instance_of(builder.root(), shape)
    with pytest.raises(ValueError, match="children"):
        builder.add_node(instance, "Part", "under")
    with pytest.raises(ValueError, match="cannot be instanced"):
        builder.instance_of(builder.root(), builder.root())


def test_a_shape_without_a_mesh_cannot_be_written(tmp_path) -> None:
    builder = ferrojt.JtBuilder()
    builder.add_node(builder.root(), "Shape", "hollow")
    with pytest.raises(ValueError, match="no mesh"):
        builder.save(str(tmp_path / "hollow.jt"))


def test_a_handle_of_another_builder_is_refused() -> None:
    first = ferrojt.JtBuilder()
    second = ferrojt.JtBuilder()
    with pytest.raises(ValueError, match="different builder"):
        first.add_node(second.root(), "Part", "elsewhere")
    with pytest.raises(ValueError, match="different builder"):
        second.set_translation(first.root(), [1.0, 0.0, 0.0])


def test_the_graph_a_builder_accumulates() -> None:
    builder, part = build()
    assert builder.node_count == 5
    assert builder.mesh_count == 1
    assert builder.class_of(part) == "Part"
    assert builder.name(part) == "cube"

    root = builder.root()
    children = builder.children(root)
    assert [child.index for child in children] == [1, 3, 4]
    assert [child.class_name for child in children] == ["Part", "Instance", "Instance"]
    assert builder.instance_target(children[1]).index == part.index
    assert builder.instance_target(children[2]).index == part.index
    assert children[1] != children[2], "two instances are two nodes"
    assert builder.instance_target(children[1]) == builder.instance_target(children[2])
    assert part.instance_target() is None


def test_a_mesh_can_be_added_on_its_own(tmp_path) -> None:
    builder = ferrojt.JtBuilder()
    mesh = builder.add_mesh(cube())
    assert builder.mesh_count == 1
    assert mesh.vertex_count == 36
    assert mesh.triangle_count == 12
    shape = builder.add_node(builder.root(), "Shape", "loose")
    builder.set_mesh(shape, mesh)
    with pytest.raises(ValueError, match="already holds"):
        builder.set_mesh(shape, mesh)

    path = tmp_path / "loose.jt"
    builder.save(str(path))
    assert ferrojt.open(str(path)).triangle_count == 12


def test_a_transform_matrix_is_read_back_column_major(tmp_path) -> None:
    builder = ferrojt.JtBuilder()
    part = builder.add_node(builder.root(), "Part", "moved")
    # Column major: the translation is the last column, at 12..14.
    builder.set_transform(
        part,
        [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 10, 20, 30, 1],
    )
    path = tmp_path / "matrix.jt"
    builder.save(str(path))

    file = ferrojt.JtFile(str(path))
    file.open()
    node = file.lsg.nodes()[1]
    assert node.translation == pytest.approx((10.0, 20.0, 30.0))

    with pytest.raises(ValueError, match="16 values"):
        builder.set_transform(part, [1.0, 2.0])


def test_a_placement_with_rotation_and_scale_survives(tmp_path) -> None:
    builder = ferrojt.JtBuilder()
    part = builder.add_node(builder.root(), "Part", "placed")
    # A quarter turn about Z as a JT quaternion (x, y, z, w).
    half = np.sqrt(0.5)
    builder.set_placement(part, [1.0, 2.0, 3.0], [0.0, 0.0, half, half], [2.0, 2.0, 2.0])
    path = tmp_path / "placed.jt"
    builder.save(str(path))

    file = ferrojt.JtFile(str(path))
    file.open()
    node = file.lsg.nodes()[1]
    assert node.translation == pytest.approx((1.0, 2.0, 3.0))
    assert node.scale == pytest.approx((2.0, 2.0, 2.0))
    assert node.rotation is not None
    assert node.rotation[2] == pytest.approx(half)

    with pytest.raises(ValueError, match="needs 4 values"):
        builder.set_placement(part, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0])


def test_a_built_file_reads_back_as_jt_10_3(tmp_path) -> None:
    builder, _ = build()
    assert builder.version == "10.3", "the newest revision is the default"
    data = builder.to_bytes()
    assert isinstance(data, bytes)
    assert data.startswith(b"Version 10.3 JT"), "the JT banner"
    assert data[80] == 0, "little endian"

    path = tmp_path / "built.jt"
    builder.save(str(path))
    assert path.is_file()
    assert path.read_bytes() == data, "save and to_bytes agree"
    assert ferrojt.detect_format(str(path)) == "jt"

    file = ferrojt.JtFile(str(path))
    file.open()
    toc = json.loads(file.toc())
    assert toc["MajorVersion"] == "10"
    assert toc["MinorVersion"] == "3"
    assert len(toc["TocTable"]) == 2, "the scene graph and its one shape segment"


def test_every_supported_revision_reaches_its_own_banner(tmp_path) -> None:
    # The revision is not only a banner string: it names the shape segments the
    # way that revision does, and it decides which coder holds the triangles.
    for revision, major, minor, shape_kinds in [
        ("8.1", "8", "1", {7}),
        ("9.5", "9", "5", {6}),
        ("10.3", "10", "3", {7}),
    ]:
        builder, _ = build()
        builder.version = revision
        assert builder.version == revision, "the builder keeps it"
        data = builder.to_bytes()
        assert data.startswith(f"Version {revision} JT".encode()), "the JT banner"

        path = tmp_path / f"built-{revision}.jt"
        builder.save(str(path))
        assert path.read_bytes() == data, "save and to_bytes agree"

        file = ferrojt.JtFile(str(path))
        assert file.open() is True
        toc = json.loads(file.toc())
        assert (toc["MajorVersion"], toc["MinorVersion"]) == (major, minor)
        assert {row[1] for row in toc["TocTable"]} & {6, 7, 8} == shape_kinds
        assert file.lsg.node_count == 5
        assert file.lsg.representation_count == 1
        assert file.model().triangle_count == 12

    builder = ferrojt.JtBuilder()
    builder.set_version("9")
    assert builder.version == "9.5", "a bare major picks the minor of that revision"
    with pytest.raises(ValueError, match="cannot be written"):
        builder.version = "11"
    assert builder.version == "9.5", "a refused revision leaves the builder alone"


def test_the_scene_graph_survives_the_round_trip(tmp_path) -> None:
    builder, _ = build()
    path = tmp_path / "graph.jt"
    builder.save(str(path))

    file = ferrojt.JtFile(str(path))
    assert file.open() is True
    lsg = file.lsg
    assert lsg is not None
    assert lsg.node_count == 5
    assert lsg.representation_count == 1
    nodes = lsg.nodes()
    assert [node.position for node in nodes] == [0, 1, 2, 3, 4]
    assert [node.class_name for node in nodes] == [
        "Partition",
        "Part",
        "Shape",
        "Instance",
        "Instance",
    ]
    assert nodes[1].name == "cube"
    assert nodes[2].name == "cube shape"
    assert nodes[1].is_instance is False
    assert nodes[3].is_instance is True
    assert [child.position for child in lsg.root().children()] == [1, 3, 4]
    assert nodes[3].instance_target() == nodes[1]


def test_the_cube_of_a_jt_8x_file_comes_back_in_strip_order(tmp_path) -> None:
    builder, _ = build()
    builder.version = "8.1"
    path = tmp_path / "cube.jt"
    builder.save(str(path))

    file = ferrojt.JtFile(str(path))
    file.open()
    lsg = file.lsg
    meshes = lsg.meshes()
    assert len(meshes) == 1
    mesh = meshes[0]
    assert mesh.vertex_count == 36
    assert mesh.normal_count == 36
    assert mesh.index_count == 36
    assert mesh.triangle_count == 12
    assert mesh.normal_binding == 1
    assert not mesh.is_empty

    source = cube()
    positions = np.asarray(mesh.array("vertices")).reshape(-1, 3)
    normals = np.asarray(mesh.array("normals")).reshape(-1, 3)
    # The strip walk renumbers the slots, so compare the triangles as sets of
    # corners rather than the index buffers.
    assert sorted(map(tuple, positions.tolist())) == sorted(
        tuple(point) for point in np.reshape(source.vertices, (-1, 3)).tolist()
    )
    assert sorted(map(tuple, normals.tolist())) == sorted(
        tuple(point) for point in np.reshape(source.normals, (-1, 3)).tolist()
    )
    assert np.asarray(mesh.array("indices")).tolist() == [
        0, 1, 2, 3, 5, 4, 6, 7, 8, 9, 11, 10, 12, 13, 14, 15, 17, 16,
        18, 19, 20, 21, 23, 22, 24, 25, 26, 27, 29, 28, 30, 31, 32, 33, 35, 34,
    ]


def test_the_cube_of_a_jt_10x_file_comes_back_through_the_coder(tmp_path) -> None:
    builder, _ = build()
    path = tmp_path / "cube10.jt"
    builder.save(str(path))

    file = ferrojt.JtFile(str(path))
    file.open()
    mesh = file.lsg.meshes()[0]
    assert mesh.triangle_count == 12
    assert mesh.vertex_count == 36
    assert mesh.normal_count == 36
    # The 64-bit vertex binding of a topological shape has no per-slot flag.
    assert mesh.normal_binding is None

    # The dual walk of the coder emits its own corner order, so it numbers the
    # slots as it goes rather than keeping the strip order of the source.
    assert np.asarray(mesh.array("indices")).tolist() == list(range(36))

    source = cube()
    positions = np.asarray(mesh.array("vertices")).reshape(-1, 3)
    normals = np.asarray(mesh.array("normals")).reshape(-1, 3)
    assert sorted(map(tuple, positions.tolist())) == sorted(
        tuple(point) for point in np.reshape(source.vertices, (-1, 3)).tolist()
    )
    assert sorted(map(tuple, normals.tolist())) == sorted(
        tuple(point) for point in np.reshape(source.normals, (-1, 3)).tolist()
    )
    assert file.model().triangle_count == 12


def test_an_open_mesh_needs_the_vertex_based_representation(tmp_path) -> None:
    # The topological coder of JT 9.x and 10.x only accepts a closed
    # two-manifold; the vertex-based representation of 8.x has no such rule.
    open_shell = ferrojt.MeshData(
        [0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 0, 0, 1, 0],
        [0, 1, 2, 3, 4, 5],
    )
    for revision in ["9.5", "10.3"]:
        builder = ferrojt.JtBuilder()
        builder.version = revision
        builder.add_shape(builder.root(), open_shell, name="open")
        with pytest.raises(ValueError, match="not a closed two-manifold"):
            builder.save(str(tmp_path / f"open{revision}.jt"))

    builder = ferrojt.JtBuilder()
    builder.version = "8.1"
    builder.add_shape(builder.root(), open_shell, name="open")
    builder.save(str(tmp_path / "open8.jt"))
    assert ferrojt.open(str(tmp_path / "open8.jt")).triangle_count == 2


def test_both_instances_share_one_representation(tmp_path) -> None:
    builder, part = build()
    path = tmp_path / "shared.jt"
    builder.save(str(path))

    file = ferrojt.JtFile(str(path))
    file.open()
    nodes = file.lsg.nodes()
    assert nodes[1].subtree_mesh_ids() == nodes[3].subtree_mesh_ids()
    assert nodes[3].subtree_mesh_ids() == nodes[4].subtree_mesh_ids()
    assert nodes[3].subtree_mesh_ids() == [0], "one representation, reached thrice"
    assert nodes[3].mesh_ids == []

    # Each instance keeps its own placement.
    assert nodes[1].translation == pytest.approx((-20.0, 0.0, 0.0))
    assert nodes[3].translation == pytest.approx((-20.0, 0.0, 0.0))
    assert nodes[4].translation == pytest.approx((20.0, 0.0, 0.0))

    # The export view keeps every node and draws the cube three times.
    model = file.model()
    assert model.node_count == 5
    assert model.mesh_count == 1
    assert model.triangle_count == 12
    assert model.material_count == 1


def test_an_uncompressed_graph_reads_back_the_same(tmp_path) -> None:
    builder, _ = build()
    builder.compress = False
    assert builder.compress is False
    path = tmp_path / "plain.jt"
    builder.save(str(path))

    model = ferrojt.open(str(path))
    assert model.node_count == 5
    assert model.triangle_count == 12


def test_a_written_file_can_be_read_by_every_reader(tmp_path) -> None:
    builder, _ = build()
    path = tmp_path / "again.jt"
    builder.save(str(path))

    # The dump of a built file is valid JSON and holds the five objects.
    dump = ferrojt.dump(str(path))
    assert '"Partition"' in dump
    assert '"Instance"' in dump
    assert dump.count('"NumIndices"') == 3, "one shape, reached by three paths"

    # And it converts like any other JT file.
    glb = tmp_path / "again.glb"
    model = ferrojt.open(str(path))
    model.save(str(glb))
    assert glb.is_file()
    assert glb.read_bytes()[:4] == b"glTF"
    usda = tmp_path / "again.usda"
    model.save(str(usda), "usda")
    assert "#usda 1.0" in usda.read_text(encoding="utf-8")


def test_a_builder_can_be_extended_after_a_write(tmp_path) -> None:
    builder = ferrojt.JtBuilder()
    path = tmp_path / "grown.jt"
    builder.save(str(path))
    assert ferrojt.open(str(path)).node_count == 1

    builder.add_node(builder.root(), "Part", "late")
    builder.save(str(path))
    reopened = ferrojt.JtFile(str(path))
    reopened.open()
    assert reopened.lsg.node_count == 2
    assert reopened.lsg.nodes()[1].name == "late"
