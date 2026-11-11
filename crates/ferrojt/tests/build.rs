//! A file can also be built from nothing: nodes are added one by one, a mesh is
//! attached to a shape node, and an instance shares the subtree it points at
//! instead of copying it. The only proof that the bytes are right is reading
//! them back with the JT reader, so every test here builds a graph, turns it
//! into bytes, parses those bytes again and compares what came back with what
//! went in.

use std::path::{Path, PathBuf};

use ferrojt::codec::ByteOrder;
use ferrojt::jt::{
    Attribute, BuildNode, Element, JtBuilder, JtFile, MaterialData, MeshData, SceneGraph,
};

/// Half the edge length of the test cube.
const HALF: f32 = 5.0;
/// The eight corners of the cube, as slot indices into [`CORNERS`].
const CORNERS: [[f32; 3]; 8] = [
    [-HALF, -HALF, -HALF],
    [HALF, -HALF, -HALF],
    [HALF, HALF, -HALF],
    [-HALF, HALF, -HALF],
    [-HALF, -HALF, HALF],
    [HALF, -HALF, HALF],
    [HALF, HALF, HALF],
    [-HALF, HALF, HALF],
];
/// The six faces, each as a quad whose corners run counter-clockwise seen from
/// outside, so that the normal of the plane points away from the cube.
const FACES: [[usize; 4]; 6] = [
    [0, 3, 2, 1],
    [4, 5, 6, 7],
    [0, 1, 5, 4],
    [2, 3, 7, 6],
    [1, 2, 6, 5],
    [3, 0, 4, 7],
];

/// The cube as JT stores it: three slots per triangle, so 36 positions, 36
/// normals and 36 indices for 12 triangles.
fn cube() -> MeshData {
    let mut vertices: Vec<f32> = Vec::new();
    let mut indices: Vec<i32> = Vec::new();
    for face in FACES {
        for corner in [face[0], face[1], face[2], face[0], face[2], face[3]] {
            indices.push((vertices.len() / 3) as i32);
            vertices.extend_from_slice(&CORNERS[corner]);
        }
    }
    MeshData::new(vertices, indices).expect("the cube is a valid mesh")
}

/// A partition holding one named part with a cube, instanced twice at
/// different places.
fn builder() -> JtBuilder {
    let mut build = JtBuilder::new();
    let part = build
        .add_node(build.root(), BuildNode::Part, "cube")
        .expect("the root takes a part");
    let shape = build
        .add_shape(part, "cube shape", cube())
        .expect("a part takes a shape");
    build
        .set_material(
            shape,
            MaterialData {
                diffuse: [0.2, 0.6, 0.9, 1.0],
                ..MaterialData::default()
            },
        )
        .expect("the shape is a node of this builder");
    build
        .set_translation(part, [-20.0, 0.0, 0.0])
        .expect("the part is a node of this builder");
    let first = build
        .instance_of(build.root(), part)
        .expect("a part can be instanced");
    build
        .set_translation(first, [-20.0, 0.0, 0.0])
        .expect("the instance is a node of this builder");
    let second = build
        .instance_of(build.root(), part)
        .expect("a part can be instanced twice");
    build
        .set_translation(second, [20.0, 0.0, 0.0])
        .expect("the instance is a node of this builder");
    build
}

/// A path in the temporary directory, unique per test process.
fn temp_path(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("ferrojt-build-{}-{}.jt", std::process::id(), name));
    let _ = std::fs::remove_file(&path);
    path
}

/// Read bytes back as a file, with the scene graph guaranteed to be present.
fn parse(bytes: &[u8]) -> JtFile {
    let file = JtFile::read(Path::new("built.jt"), bytes).expect("the built file must parse");
    assert!(file.lsg.is_some(), "the built file has no scene graph");
    file
}

fn graph(file: &JtFile) -> &SceneGraph {
    file.lsg.as_ref().expect("the scene graph")
}

fn classes(file: &JtFile) -> Vec<&'static str> {
    graph(file)
        .nodes
        .iter()
        .map(|stored| stored.node.class_name())
        .collect()
}

/// The corners of every triangle of a mesh, sorted so that the same triangle
/// compares equal whatever winding it is stored in. JT expands a strip with
/// alternating winding, so the order and winding of the triangles is not part
/// of what a round trip has to preserve — the triangles themselves are.
fn triangle_corners(indices: &[i32], vertices: &[f32]) -> Vec<[f32; 9]> {
    let mut out: Vec<[f32; 9]> = Vec::new();
    for triangle in indices.chunks(3) {
        if triangle.len() != 3 {
            continue;
        }
        let mut corners = [0.0f32; 9];
        for (slot, index) in triangle.iter().enumerate() {
            let from = usize::try_from(*index).expect("the index is in range");
            corners[slot * 3..slot * 3 + 3].copy_from_slice(&vertices[from * 3..from * 3 + 3]);
        }
        corners.sort_by(|a, b| a.partial_cmp(b).expect("the corners are finite"));
        out.push(corners);
    }
    out.sort_by(|a, b| {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| x.total_cmp(y))
            .find(|order| !order.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[test]
fn a_built_file_reads_back_as_jt_8_1() {
    let bytes = builder()
        .write_to_bytes()
        .expect("the graph can be written");
    let file = parse(&bytes);

    assert_eq!(file.header.major, 8);
    assert_eq!(file.header.minor, 1);
    assert_eq!(file.header.order, ByteOrder::Little);

    // One scene graph plus one representation segment per shape.
    assert_eq!(file.toc.len(), 2);
    assert_eq!(file.toc[0].kind, 1, "the scene graph segment");
    assert_eq!(file.toc[1].kind, 7, "the representation segment");
    assert_eq!(file.header.lsg_guid, file.toc[0].guid);

    // Every row points at real bytes, and the declared length fits the file.
    for entry in &file.toc {
        assert!(entry.offset < bytes.len() as u64);
        assert!(u64::from(entry.length) <= bytes.len() as u64);
    }
    // The scene graph comes first and the segments do not overlap.
    assert_eq!(file.toc[0].offset, 105);
    assert!(file.toc[1].offset > file.toc[0].offset);

    // Writing the same graph twice yields the same bytes.
    assert_eq!(bytes, builder().write_to_bytes().expect("deterministic"));
}

#[test]
fn the_scene_graph_of_a_built_file_survives_the_round_trip() {
    let bytes = builder()
        .write_to_bytes()
        .expect("the graph can be written");
    let file = parse(&bytes);
    let lsg = graph(&file);

    // Root, part, shape and the two instances.
    assert_eq!(
        classes(&file),
        ["Partition", "Part", "Shape", "Instance", "Instance"]
    );
    assert_eq!(lsg.root, Some(0));

    let root = lsg.root().expect("a root node");
    assert_eq!(
        lsg.children_of(root),
        vec![Some(1), Some(3), Some(4)],
        "the part and both instances hang off the partition"
    );

    let part = lsg.node_at(1).expect("the part");
    assert_eq!(part.class_name(), "Part");
    assert_eq!(part.name(), "cube", "the name travelled through the table");
    assert_eq!(lsg.children_of(part), vec![Some(2)]);

    let shape = lsg.node_at(2).expect("the shape");
    assert_eq!(shape.name(), "cube shape");
    assert_eq!(shape.class_name(), "Shape");
    assert_eq!(
        shape.bounds(),
        Some([-HALF, -HALF, -HALF, HALF, HALF, HALF])
    );

    // One late load per shape, and it is the representation segment.
    assert_eq!(shape.late_loads().len(), 1);
    let load = &shape.late_loads()[0];
    assert_eq!(load.segment_type, 7);
    assert!(file.segment_offset(&load.guid).is_some());
    assert_eq!(load.guid, file.toc[1].guid);

    // The two instances point back at the part.
    for pos in [3, 4] {
        let instance = lsg.node_at(pos).expect("an instance");
        assert_eq!(instance.class_name(), "Instance");
        assert_eq!(lsg.instance_target(instance), Some(1));
    }
}

#[test]
fn the_triangles_of_a_built_mesh_come_back_unchanged() {
    let source = cube();
    let bytes = builder()
        .write_to_bytes()
        .expect("the graph can be written");
    let file = parse(&bytes);
    let lsg = graph(&file);

    assert_eq!(lsg.loaded.len(), 1, "one representation was loaded");
    let loaded = &lsg.loaded[0];
    assert_eq!(loaded.segment_type, 7);
    assert!(matches!(loaded.element, Element::ShapeLodTriStripSet(_)));
    let geometry = loaded.element.geometry().expect("a representation");

    // JT stores a *strip* set, and the reader expands each strip with
    // alternating winding, so the index buffer comes back in strip order
    // rather than as it was written: every second triangle has its last two
    // corners swapped. The writer stores those two slots the other way round,
    // so the swap lands on the original corners and the *triangles* are the
    // ones that were given — which is what has to match.
    assert_eq!(
        geometry.indices,
        [
            0, 1, 2, 3, 5, 4, 6, 7, 8, 9, 11, 10, 12, 13, 14, 15, 17, 16, 18, 19, 20, 21, 23, 22,
            24, 25, 26, 27, 29, 28, 30, 31, 32, 33, 35, 34
        ],
        "the strip order the reader expands to"
    );
    assert_eq!(
        triangle_corners(&geometry.indices, &geometry.vertices),
        triangle_corners(source.indices(), source.vertices()),
        "the same 12 triangles came back"
    );
    // A cube has one normal per face and the swap stays inside a face, so the
    // normals read back in the order they were given.
    assert_eq!(geometry.normals, source.normals());
    assert_eq!(geometry.vertex_count, 36);
    assert_eq!(geometry.normal_count, 36);
    assert_eq!(geometry.normal_binding, Some(1));

    // And the export view sees the same cube.
    let model = file.to_model();
    assert_eq!(model.meshes.len(), 1);
    assert_eq!(model.triangle_count(), 12);
    assert_eq!(model.vertex_count(), 36);
}

#[test]
fn a_compact_mesh_is_exploded_and_comes_back_unchanged() {
    // The same cube written the way a modeller hands it over: eight shared
    // corners and indices that point at them. A JT 8.x faceted shape cannot
    // share a corner, because the normal belongs to the slot, so the builder
    // explodes the mesh — and because the strip walk renumbers the slots, the
    // writer has to permute the geometry to match. The triangles are what has
    // to survive.
    let mut indices: Vec<i32> = Vec::new();
    for face in FACES {
        for corner in [face[0], face[1], face[2], face[0], face[2], face[3]] {
            indices.push(corner as i32);
        }
    }
    let corners: Vec<f32> = CORNERS.iter().flatten().copied().collect();
    let source = MeshData::new(corners.clone(), indices.clone()).expect("the cube is valid");
    assert_eq!(source.vertices().len(), 108, "the mesh was exploded");
    assert_eq!(source.indices().len(), 36);
    assert_eq!(
        triangle_corners(source.indices(), source.vertices()),
        triangle_corners(&indices, &corners),
        "exploding keeps the triangles"
    );

    let mut build = JtBuilder::new();
    let shape = build
        .add_shape(build.root(), "compact", source.clone())
        .expect("the root takes a shape");
    assert_eq!(build.class(shape), Some(BuildNode::Shape));
    let bytes = build.write_to_bytes().expect("the graph can be written");
    let file = parse(&bytes);
    let geometry = graph(&file).loaded[0]
        .element
        .geometry()
        .expect("a representation");

    assert_eq!(
        triangle_corners(&geometry.indices, &geometry.vertices),
        triangle_corners(&indices, &corners),
        "the shared corners came back as their own slots"
    );
    assert_eq!(geometry.vertex_count, 36);
    assert_eq!(geometry.normal_count, 36);
    assert_eq!(
        geometry.normals,
        source.normals(),
        "each corner kept the normal of its face"
    );
    assert_eq!(parse(&bytes).to_model().triangle_count(), 12);
}

#[test]
fn an_instance_shares_one_representation_and_keeps_its_own_placement() {
    let bytes = builder()
        .write_to_bytes()
        .expect("the graph can be written");
    let file = parse(&bytes);
    let lsg = graph(&file);

    // Only the shape refers to a representation, so the file holds one mesh
    // even though three nodes reach it.
    let part = lsg.node_at(1).expect("the part");
    let shape = lsg.node_at(2).expect("the shape");
    let first = lsg.node_at(3).expect("the first instance");
    let second = lsg.node_at(4).expect("the second instance");
    assert_eq!(shape.late_loads().len(), 1, "the shape refers once");
    assert_eq!(lsg.loaded_pos(&shape.late_loads()[0]), Some(0));
    for node in [part, first, second] {
        assert!(
            node.late_loads().is_empty(),
            "a {} refers to nothing of its own",
            node.class_name()
        );
    }
    assert_eq!(lsg.loaded.len(), 1);

    // Each placement is its own attribute on its own node.
    let matrices: Vec<[f32; 3]> = [3, 4]
        .iter()
        .map(|pos| {
            let node = lsg.node_at(*pos).expect("an instance");
            let attribute = lsg
                .attributes_of(node)
                .into_iter()
                .find_map(|attribute| match attribute {
                    Attribute::GeometricTransform(transform) => Some(transform.matrix),
                    _ => None,
                })
                .expect("an instance carries its placement");
            // A JT matrix is column major, so the translation is the last
            // column of the first three rows.
            [
                attribute[12] as f32,
                attribute[13] as f32,
                attribute[14] as f32,
            ]
        })
        .collect();
    assert_eq!(matrices, [[-20.0, 0.0, 0.0], [20.0, 0.0, 0.0]]);

    // The export keeps every node. The partition holds the part and both
    // instances, and each instance node points at the one model node the part
    // was built into — that is what sharing means here.
    let model = file.to_model();
    assert_eq!(model.nodes.len(), 5);
    assert_eq!(model.meshes.len(), 1);
    let attachments: usize = model.nodes.iter().map(|node| node.instances.len()).sum();
    assert_eq!(attachments, 1, "only the shape itself draws the cube");
    let root = &model.nodes[model.scenes[0]
        .roots
        .first()
        .copied()
        .expect("the partition is the root of the scene")];
    assert_eq!(root.children.len(), 3, "the part plus both instances");
    let shared: Vec<usize> = root.children[1..]
        .iter()
        .map(|child| {
            let instance = &model.nodes[*child];
            assert_eq!(instance.children.len(), 1, "an instance holds its target");
            instance.children[0]
        })
        .collect();
    assert_eq!(shared[0], shared[1], "both instances reach one subtree");
    assert_eq!(
        model.nodes[shared[0]].children.len(),
        1,
        "the part holds the shape"
    );
}

#[test]
fn a_material_reaches_the_shape_that_carries_it() {
    let bytes = builder()
        .write_to_bytes()
        .expect("the graph can be written");
    let file = parse(&bytes);
    let lsg = graph(&file);

    let shape = lsg.node_at(2).expect("the shape");
    let material = lsg
        .attributes_of(shape)
        .into_iter()
        .find_map(|attribute| match attribute {
            Attribute::Material(material) => Some(material.clone()),
            _ => None,
        })
        .expect("the shape carries its material");
    assert_eq!(material.diffuse, [0.2, 0.6, 0.9, 1.0]);
    assert_eq!(material.shininess, 128.0);

    let model = file.to_model();
    assert_eq!(model.materials.len(), 1);
}

#[test]
fn an_uncompressed_scene_graph_reads_back_the_same() {
    let mut build = builder();
    let _ = build.set_compress(false);
    let plain = build.write_to_bytes().expect("the graph can be written");
    let file = parse(&plain);

    assert_eq!(
        classes(&file),
        classes(&parse(&builder().write_to_bytes().expect("compressed")))
    );
    let model = file.to_model();
    assert_eq!(model.meshes.len(), 1);
    assert_eq!(model.triangle_count(), 12);
}

#[test]
fn a_built_file_can_be_written_to_disk_and_opened_again() {
    let path = temp_path("disk");
    builder()
        .write_to_file(&path)
        .expect("the file can be written");
    assert!(path.exists());

    let file = JtFile::open(&path).expect("the file on disk parses");
    assert_eq!(file.header.major, 8);
    assert_eq!(classes(&file).len(), 5);
    assert_eq!(file.to_model().triangle_count(), 12);

    std::fs::remove_file(&path).expect("the temporary file can be removed");
}

#[test]
fn an_incomplete_graph_is_refused() {
    // A shape without triangles cannot be represented.
    let empty = MeshData::new(vec![0.0, 0.0, 0.0], Vec::new());
    assert!(matches!(empty, Err(ferrojt::Error::Invalid { .. })));
    assert!(MeshData::default().validate().is_err());

    // A shape node without a mesh cannot be written: glTF mandates POSITION.
    let mut build = JtBuilder::new();
    let shape = build
        .add_node(build.root(), BuildNode::Shape, "hollow")
        .expect("the root takes a shape");
    let err = build.write_to_bytes().expect_err("a shape needs a mesh");
    assert!(err.to_string().contains("no mesh"));

    // A leaf cannot hold children, and the root cannot be instanced.
    assert!(
        build
            .add_node(shape, BuildNode::Group, None::<&str>)
            .is_err_and(|err| err.to_string().contains("children"))
    );
    assert!(build.instance_of(shape, build.root()).is_err());
}

#[test]
fn the_builder_keeps_the_graph_it_accumulates() {
    // The builder links every node it adds, so a handle always names a node of
    // this file and the graph stays a tree: what it cannot produce is a cycle,
    // and what it reports is the shape of what was built.
    let mut build = JtBuilder::new();
    let root = build.root();
    let group = build
        .add_node(root, BuildNode::Group, "group")
        .expect("the root takes a group");
    let shape = build
        .add_shape(group, "cube", cube())
        .expect("a group takes a shape");
    let twin = build
        .instance_of(root, group)
        .expect("a group can be instanced");

    assert_eq!(build.node_count(), 4);
    assert_eq!(build.mesh_count(), 1);
    assert_eq!(build.children(root), vec![group, twin]);
    assert_eq!(build.children(group), vec![shape]);
    assert_eq!(build.class(shape), Some(BuildNode::Shape));
    assert_eq!(build.name(group), Some("group"));
    assert_eq!(build.instance_target(twin), Some(group));
    assert_eq!(build.instance_target(group), None);
    assert_eq!(cube().triangle_count(), 12);
    assert_eq!(
        cube().bounds(),
        Some([-HALF, -HALF, -HALF, HALF, HALF, HALF])
    );
    assert!(!BuildNode::Shape.accepts_children());
    assert!(BuildNode::Part.accepts_children());
}
