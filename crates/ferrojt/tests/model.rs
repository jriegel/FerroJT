//! An export carries one representation per part: the finest level of detail
//! of a range LOD. The coarser tiers of the same part must not appear as extra
//! meshes, and every mesh has to say which tier it came from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a sample exports once the coarse tiers are dropped.
struct Expectation {
    /// Meshes in the model, one per range LOD.
    meshes: usize,
    /// Triangles of those meshes.
    triangles: usize,
}

const EXPECTED: &[(&str, Expectation)] = &[
    // Three identical tiers of one cube: only the first is kept.
    (
        "example_block_jt10.3.jt",
        Expectation {
            meshes: 1,
            triangles: 12,
        },
    ),
    (
        "example_block_jt8.1.jt",
        Expectation {
            meshes: 1,
            triangles: 12,
        },
    ),
    (
        "example_block_jt9.5.jt",
        Expectation {
            meshes: 1,
            triangles: 12,
        },
    ),
    // Four tiers per part; the finest holds 666 and 1185 indices.
    (
        "opening_protection_plate1_jt8.0.jt",
        Expectation {
            meshes: 2,
            triangles: 617,
        },
    ),
    // Three tiers per part; the finest holds 516 and 942 indices.
    (
        "opening_protection_plate1_jt9.5.jt",
        Expectation {
            meshes: 2,
            triangles: 486,
        },
    ),
];

fn samples() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("ExampleFiles");
    let mut files = std::fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("reading {}: {err}", root.display()))
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jt"))
        .collect::<Vec<_>>();
    files.sort();
    assert!(!files.is_empty(), "no sample files found");
    files
}

#[test]
fn only_the_finest_level_of_detail_is_exported() {
    let mut checked = 0usize;
    for path in samples() {
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let file = ferrojt::jt::JtFile::open(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let model = file.to_model();

        let Some((_, expected)) = EXPECTED.iter().find(|(key, _)| *key == name) else {
            continue;
        };
        checked += 1;

        assert_eq!(
            model.meshes.len(),
            expected.meshes,
            "{name}: the model holds the wrong number of meshes"
        );
        assert_eq!(
            model.triangle_count(),
            expected.triangles,
            "{name}: the model holds the wrong number of triangles"
        );

        // Every mesh of a range LOD records the tier it was taken from, and a
        // part contributes exactly one mesh, which has to be the finest one.
        for mesh in &model.meshes {
            assert_eq!(mesh.lod_level, Some(0), "{name}: a coarse tier leaked");
        }
    }
    assert_eq!(
        checked,
        EXPECTED.len(),
        "not every expectation was matched by a sample file"
    );
}

#[test]
fn every_level_of_detail_can_be_loaded() {
    // A screen-size driven viewer needs the coarse tiers to exist, so
    // `to_model_with_all_lods()` keeps them. The scene graph is unchanged — a
    // tier is a node of its own in the file — only the meshes multiply, and
    // each one records which tier it came from.
    //
    // What a sample gains is pinned: the block files declare three tiers of one
    // cube, the plate files four and three tiers per part, and the server
    // sample declares a single tier for all 584 of its range LODs, so it must
    // come out identical to `to_model()`.
    struct All {
        meshes: usize,
        triangles: usize,
        tiers: usize,
    }
    const EXTRA: &[(&str, All)] = &[
        (
            "example_block_jt10.3.jt",
            All {
                meshes: 3,
                triangles: 36,
                tiers: 3,
            },
        ),
        (
            "example_block_jt8.1.jt",
            All {
                meshes: 3,
                triangles: 36,
                tiers: 3,
            },
        ),
        (
            "example_block_jt9.5.jt",
            All {
                meshes: 3,
                triangles: 36,
                tiers: 3,
            },
        ),
        (
            "opening_protection_plate1_jt8.0.jt",
            All {
                meshes: 8,
                triangles: 1_137,
                tiers: 4,
            },
        ),
        (
            "opening_protection_plate1_jt9.5.jt",
            All {
                meshes: 6,
                triangles: 922,
                tiers: 3,
            },
        ),
        // One tier per part: nothing to add.
        (
            "SampleJT_Server.jt",
            All {
                meshes: 106,
                triangles: 129_067,
                tiers: 1,
            },
        ),
    ];

    let mut checked = 0usize;
    for path in samples() {
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let Some((_, expected)) = EXTRA.iter().find(|(key, _)| *key == name) else {
            continue;
        };
        checked += 1;

        let file = ferrojt::jt::JtFile::open(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let finest = file.to_model();
        let all = file.to_model_with_all_lods();

        assert_eq!(
            all.meshes.len(),
            expected.meshes,
            "{name}: the wrong number of meshes with every tier loaded"
        );
        assert_eq!(
            all.triangle_count(),
            expected.triangles,
            "{name}: the wrong number of triangles with every tier loaded"
        );
        assert_eq!(
            all.nodes.len(),
            finest.nodes.len(),
            "{name}: loading every tier changed the scene graph"
        );
        assert_eq!(
            all.materials.len(),
            finest.materials.len(),
            "{name}: loading every tier changed the materials"
        );

        // Each tier index appears as often as there are parts, and no tier
        // beyond what the file declares shows up.
        let mut histogram: Vec<(u32, usize)> = Vec::new();
        for mesh in &all.meshes {
            let tier = mesh
                .lod_level
                .unwrap_or_else(|| panic!("{name}: a mesh does not say which tier it is"));
            assert!(
                (tier as usize) < expected.tiers,
                "{name}: tier {tier} is not declared by the file"
            );
            match histogram.iter_mut().find(|(t, _)| *t == tier) {
                Some(entry) => entry.1 += 1,
                None => histogram.push((tier, 1)),
            }
        }
        histogram.sort();
        let parts = expected.meshes / expected.tiers;
        for (tier, count) in &histogram {
            assert_eq!(
                *count, parts,
                "{name}: tier {tier} holds the wrong number of parts"
            );
        }
        assert_eq!(
            histogram.len(),
            expected.tiers,
            "{name}: a declared tier is missing"
        );

        // The finest tier alone is exactly what `to_model()` produces.
        let kept: Vec<_> = all
            .meshes
            .iter()
            .filter(|mesh| mesh.lod_level == Some(0))
            .collect();
        assert_eq!(
            kept.len(),
            finest.meshes.len(),
            "{name}: the finest tier of the full load does not match the default"
        );
    }
    assert_eq!(
        checked,
        EXTRA.len(),
        "not every expectation was matched by a sample file"
    );
}

#[test]
fn every_sample_exports_to_gltf() {
    // glTF requires a POSITION attribute in every primitive, so a
    // representation that declares no vertices at all must never reach the
    // writer. `SampleJT_Server.jt` carries 36 such shapes and used to abort the
    // whole document with "a primitive without a POSITION attribute".
    for path in samples() {
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let file = ferrojt::jt::JtFile::open(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let model = file.to_model();

        for mesh in &model.meshes {
            assert!(
                !mesh.primitives.is_empty(),
                "{name}: the mesh {:?} holds no primitive",
                mesh.name
            );
            for primitive in &mesh.primitives {
                let positions = primitive
                    .attributes
                    .iter()
                    .map(|handle| &model.accessors.attributes[*handle])
                    .find(|attribute| attribute.name == "POSITION");
                let positions = positions.unwrap_or_else(|| {
                    panic!("{name}: the mesh {:?} has no POSITION attribute", mesh.name)
                });
                assert!(
                    positions.count > 0,
                    "{name}: the mesh {:?} has an empty POSITION attribute",
                    mesh.name
                );
            }
        }

        ferrojt::export::to_format(ferrojt::Format::Glb, &model)
            .unwrap_or_else(|err| panic!("{name}: {err}"));
    }
}

#[test]
fn the_server_sample_exports_every_shape() {
    // `SampleJT_Server.jt` is the odd one out of the samples: it has no
    // reference dump, and it is the file that showed the POSITION failure. One
    // of its shapes (36 instances) has faces with more than 64 attributes; it
    // read as empty until the wide attribute masks were read as the plain
    // word array they are. Its counts are pinned so that a change in how
    // representations are read becomes visible.
    let file = sample("SampleJT_Server.jt");
    let model = file.to_model();

    assert_eq!(model.nodes.len(), 2_158, "nodes");
    assert_eq!(model.meshes.len(), 106, "meshes");
    assert_eq!(model.materials.len(), 23, "materials");
    assert_eq!(model.triangle_count(), 129_067, "triangles");

    // Every shape node carries the mesh of its representation.
    let shapes = model
        .nodes
        .iter()
        .filter(|node| node.kind.as_deref() == Some("Shape"))
        .count();
    assert_eq!(shapes, model.meshes.len(), "a shape lost its geometry");
    let drawn = model
        .nodes
        .iter()
        .filter(|node| !node.instances.is_empty())
        .count();
    assert_eq!(drawn, model.meshes.len(), "a mesh lost its node");
}

/// Rebuild the column-major 4x4 that a decomposed [`ferrojt::model::Transform`]
/// stands for.
///
/// JT stores the matrix **column major**: the element of row `r`, column `c`
/// sits at flat index `4 * c + r`, so the translation is the last column. The
/// reference confirms it twice over — `JtAttribute_GeometricTransform.hxx` maps
/// the array with `Eigen::Map` (column major by default) and documents the
/// translation as being "in the last column", and `JtGltfMesh.cpp` assigns
/// `actTrsf(row, col) = GetTrsf()[4 * col + row]`.
fn matrix_of(transform: ferrojt::model::Transform) -> [f64; 16] {
    let [x, y, z, w] = transform.rotation.map(f64::from);
    let scales = transform.scale.map(f64::from);
    let rotation = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];

    let mut matrix = [0.0f64; 16];
    for column in 0..3 {
        for row in 0..3 {
            matrix[4 * column + row] = rotation[row][column] * scales[column];
        }
    }
    for row in 0..3 {
        matrix[12 + row] = f64::from(transform.translation[row]);
    }
    matrix[15] = 1.0;
    matrix
}

/// `true` when a stored matrix leaves coordinates unchanged.
fn is_identity(matrix: &[f64; 16]) -> bool {
    matrix.iter().enumerate().all(|(index, value)| {
        let row = index % 4;
        let column = index / 4;
        *value == if row == column { 1.0 } else { 0.0 }
    })
}

/// `true` when two matrices agree to within the precision of the model.
fn close(a: &[f64; 16], b: &[f64; 16]) -> bool {
    a.iter()
        .zip(b)
        .all(|(left, right)| (left - right).abs() < 1e-3)
}

#[test]
fn every_transform_of_a_sample_survives_the_decomposition() {
    // The reader splits a JT transform into translation, rotation and scale, and
    // every consumer composes those back into a matrix. Rebuilding the matrix
    // from the three parts therefore has to give what the file stored.
    //
    // This is pinned against the server sample because it is the only one whose
    // transforms actually rotate: the others are plain translations, whose flat
    // array reads the same either way, so a transposed layout is invisible in
    // them and the rotated parts of this file spin the wrong way.
    let file = sample("SampleJT_Server.jt");
    let graph = file.lsg.as_ref().expect("the sample has a scene graph");
    let model = file.to_model();

    let stored: Vec<[f64; 16]> = graph
        .nodes
        .iter()
        .filter_map(|stored| {
            graph
                .attributes_of(&stored.node)
                .into_iter()
                .find_map(|attribute| match attribute {
                    ferrojt::jt::Attribute::GeometricTransform(attribute) => Some(attribute.matrix),
                    _ => None,
                })
        })
        .filter(|matrix| !is_identity(matrix))
        .collect();
    assert!(
        stored.len() > 100,
        "the sample lost its rotations: {} non-identity transforms",
        stored.len()
    );

    let rebuilt: Vec<[f64; 16]> = model
        .nodes
        .iter()
        .filter(|node| !node.transform.is_identity())
        .map(|node| matrix_of(node.transform))
        .collect();

    let mut unmatched = rebuilt;
    let mut matched = 0usize;
    for matrix in &stored {
        let Some(position) = unmatched.iter().position(|other| close(matrix, other)) else {
            let closest = unmatched
                .iter()
                .map(|other| {
                    (
                        other
                            .iter()
                            .zip(matrix)
                            .map(|(left, right)| (left - right).abs())
                            .fold(0.0f64, f64::max),
                        other,
                    )
                })
                .min_by(|left, right| left.0.total_cmp(&right.0));
            panic!(
                "no model transform reproduces the stored matrix {matrix:?}; the closest is \
                 {closest:?}"
            );
        };
        unmatched.swap_remove(position);
        matched += 1;
    }
    assert_eq!(
        matched,
        stored.len(),
        "a transform of the scene graph did not survive into the model"
    );
}

#[test]
fn a_rotated_instance_keeps_the_rotation_of_the_file() {
    // The captive screws of `SampleJT_Server.jt` are turned 93 degrees about an
    // axis in the XZ plane. The first column of a rotation is the image of the
    // X axis, and the file says that image is (0.884254, 0, 0.467006). Reading
    // the flat array row major puts the first *row* there instead — the inverse
    // rotation — which is the bug that left the screws mis-oriented in the
    // front plate while leaving their positions untouched.
    let file = sample("SampleJT_Server.jt");
    let model = file.to_model();

    let node = model
        .nodes
        .iter()
        .find(|node| {
            let [x, y, z] = node.transform.translation;
            (x - 0.7).abs() < 1e-3 && (y - 30.1).abs() < 1e-3 && (z - 53.3).abs() < 1e-3
        })
        .expect("the screw instance placed at (0.7, 30.1, 53.3)");

    let matrix = matrix_of(node.transform);
    let image_of_x = [matrix[0], matrix[1], matrix[2]];
    let expected = [0.884254, 0.0, 0.467006];
    for (actual, want) in image_of_x.iter().zip(expected) {
        assert!(
            (actual - want).abs() < 1e-3,
            "the image of the X axis is {image_of_x:?}, the file says {expected:?}"
        );
    }
}

#[test]
fn every_node_of_the_scene_graph_survives() {
    // Dropping a tier must not drop the node that holds it: the graph keeps
    // its shape so that viewers can still switch levels of detail themselves.
    for (name, _) in EXPECTED {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the workspace root")
            .join("data")
            .join("ExampleFiles")
            .join(name);
        let file = ferrojt::jt::JtFile::open(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let graph = file
            .lsg
            .as_ref()
            .unwrap_or_else(|| panic!("{}: no logical scene graph", path.display()));
        let model = file.to_model();

        let range_lods = graph
            .nodes
            .iter()
            .filter(|stored| stored.node.class_name() == "RangeLOD")
            .count();
        let groups = graph
            .nodes
            .iter()
            .filter(|stored| stored.node.class_name() == "Group")
            .count();

        let lod_nodes = model
            .nodes
            .iter()
            .filter(|node| node.kind.as_deref() == Some("RangeLOD"))
            .count();
        let group_nodes = model
            .nodes
            .iter()
            .filter(|node| node.kind.as_deref() == Some("Group"))
            .count();

        assert_eq!(
            lod_nodes, range_lods,
            "{name}: a range LOD disappeared from the model"
        );
        assert_eq!(
            group_nodes, groups,
            "{name}: a level of detail branch disappeared from the model"
        );
    }
}

#[test]
fn every_instance_keeps_the_subtree_it_instantiates() {
    // The plate samples hold one screw and instantiate it twice, at y = -25
    // and y = +25. An instance must never collapse into an empty node: the
    // viewer has to draw the screw twice, once per instance transform.
    for name in [
        "opening_protection_plate1_jt8.0.jt",
        "opening_protection_plate1_jt9.5.jt",
    ] {
        let file = sample(name);
        let graph = file
            .lsg
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no logical scene graph"));
        let model = file.to_model();

        let in_graph = graph
            .nodes
            .iter()
            .filter(|stored| stored.node.class_name() == "Instance")
            .count();
        let instances = model
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind.as_deref() == Some("Instance"))
            .collect::<Vec<_>>();
        assert_eq!(
            instances.len(),
            in_graph,
            "{name}: an instance disappeared from the model"
        );
        assert_eq!(instances.len(), 2, "{name}: expected two instances");

        let mut targets = Vec::new();
        for (index, node) in &instances {
            assert_eq!(
                node.children.len(),
                1,
                "{name}: instance {index} lost the subtree it instantiates"
            );
            targets.push(node.children[0]);
        }

        // Both instances point at the same translated subtree, which is what
        // glTF encodes as one node with two parents: the geometry is shared,
        // the transforms are not.
        assert_eq!(
            targets[0], targets[1],
            "{name}: the two instances do not share one subtree"
        );
        let target = &model.nodes[targets[0]];
        assert!(
            !target.children.is_empty(),
            "{name}: the instanced subtree is empty"
        );

        // The instance transforms are what separate the two copies.
        let offsets = instances
            .iter()
            .map(|(_, node)| node.transform.translation)
            .collect::<Vec<_>>();
        assert_ne!(
            offsets[0], offsets[1],
            "{name}: both instances carry the same transform"
        );
    }
}

/// The layer filters the samples declare, and the one the export applies by
/// default. A file of the JT 8.x era has none at all, and the newer ones all
/// leave `ACTIVELAYERFILTER::` empty, so the first of the sorted names wins.
const FILTERS: &[(&str, &[&str], Option<&str>)] = &[
    ("example_block_jt10.3.jt", &["ALL", "LAYCUR"], Some("ALL")),
    ("example_block_jt8.1.jt", &[], None),
    ("example_block_jt9.5.jt", &["ALL", "LAYCUR"], Some("ALL")),
    ("opening_protection_plate1_jt8.0.jt", &[], None),
    (
        "opening_protection_plate1_jt9.5.jt",
        &["ALL", "LAYCUR"],
        Some("ALL"),
    ),
];

fn sample(name: &str) -> ferrojt::jt::JtFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("ExampleFiles")
        .join(name);
    ferrojt::jt::JtFile::open(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The diffuse colours the reference dump of a sample reports, sorted. The
/// plate files paint the plate grey and the screw yellow-green, which is the
/// one appearance a viewer notices; the cube is a single bluish grey.
const COLOURS: &[(&str, &[[f32; 4]])] = &[
    (
        "example_block_jt10.3.jt",
        &[[0.596_063, 0.666_667, 0.686_259, 1.0]],
    ),
    // The 8.0/8.1 writers round the cube colour differently from 9.5/10.3.
    ("example_block_jt8.1.jt", &[[0.6, 0.6, 0.6, 1.0]]),
    (
        "example_block_jt9.5.jt",
        &[[0.596_063, 0.666_667, 0.686_259, 1.0]],
    ),
    (
        "opening_protection_plate1_jt8.0.jt",
        &[[0.6, 0.6, 0.6, 1.0], [0.8, 1.0, 0.2, 1.0]],
    ),
    (
        "opening_protection_plate1_jt9.5.jt",
        &[[0.6, 0.6, 0.6, 1.0], [0.8, 1.0, 0.2, 1.0]],
    ),
];

/// The colours of a sample as the reference dump states them, read from the
/// `Attribute_MaterialDiffuseColor` keys of its JSON oracle.
fn reference_colours(name: &str) -> Vec<[f32; 4]> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("Reference")
        .join(Path::new(name).with_extension("json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    // The reference dumps start with a UTF-8 byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{name}: {err}"));

    let mut found = Vec::new();
    let mut stack = vec![&value];
    while let Some(current) = stack.pop() {
        match current {
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    if key == "Attribute_MaterialDiffuseColor" {
                        let numbers = item.as_array().unwrap_or_else(|| {
                            panic!("{name}: the diffuse colour is not an array")
                        });
                        let colour = numbers
                            .iter()
                            .map(|number| number.as_f64().expect("a number") as f32)
                            .collect::<Vec<_>>();
                        found.push([colour[0], colour[1], colour[2], colour[3]]);
                    }
                    stack.push(item);
                }
            }
            serde_json::Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    found.sort_by(|a, b| {
        a.iter()
            .zip(b)
            .fold(std::cmp::Ordering::Equal, |acc, (x, y)| {
                acc.then(x.total_cmp(y))
            })
    });
    found.dedup();
    found
}

/// Compare two colours channel by channel. The reference dumps print a number
/// with C's `%g`, i.e. six significant digits, so the decimal parsed from a dump
/// is only within ~1e-6 of the `float` the file really stores — while the
/// colours that must never be confused (the grey plate and the yellow-green
/// screw, or the two ways the 8.x and 9.x writers round the cube) differ by
/// thousandths.
fn assert_colour(actual: &[f32], expected: &[f32], context: &str) {
    assert_eq!(actual.len(), expected.len(), "{context}");
    for (got, want) in actual.iter().zip(expected) {
        assert!((got - want).abs() <= 1e-5, "{context}: {got} is not {want}");
    }
}

#[test]
fn the_diffuse_colour_of_a_sample_reaches_the_model() {
    for (name, colours) in COLOURS {
        let file = sample(name);
        let model = file.to_model();

        // The dump and the export have to agree on what the file says.
        let wanted = reference_colours(name);
        assert_eq!(wanted.len(), colours.len(), "{name}");
        for (got, want) in wanted.iter().zip(colours.iter()) {
            assert_colour(got, want, &format!("{name}: the reference dump"));
        }

        let mut got = model
            .materials
            .iter()
            .map(|material| material.base_color.factor)
            .collect::<Vec<_>>();
        got.sort_by(|a, b| {
            a.iter()
                .zip(b)
                .fold(std::cmp::Ordering::Equal, |acc, (x, y)| {
                    acc.then(x.total_cmp(y))
                })
        });
        for (index, colour) in colours.iter().enumerate() {
            assert_colour(
                &got[index],
                colour,
                &format!("{name}: the model does not carry the colours of the file"),
            );
        }

        // A file repeats one appearance on every shape of a part, so the model
        // holds one material per distinct colour, not one per attribute.
        assert_eq!(
            model.materials.len(),
            colours.len(),
            "{name}: a repeated material was appended instead of reused"
        );

        // Every mesh of the export has to point at one of them.
        for mesh in &model.meshes {
            for primitive in &mesh.primitives {
                assert!(
                    primitive.material.is_some(),
                    "{name}: a mesh carries no material"
                );
            }
        }
    }
}

#[test]
fn the_material_of_the_export_matches_the_model() {
    // The colour has to survive the writer as well: `baseColorFactor` is what a
    // glTF viewer paints the mesh with.
    for (name, colours) in COLOURS {
        let model = sample(name).to_model();
        let bytes = ferrojt::export::to_format(ferrojt::Format::Glb, &model)
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        let (json_len, _) =
            ferrojt::export::parse_header(&bytes).unwrap_or_else(|err| panic!("{name}: {err}"));
        let json: serde_json::Value = serde_json::from_slice(&bytes[20..20 + json_len])
            .unwrap_or_else(|err| panic!("{name}: the glTF JSON is not valid: {err}"));

        let materials = json["materials"].as_array().expect("materials");
        assert_eq!(materials.len(), colours.len(), "{name}");
        let mut got = materials
            .iter()
            .map(|material| {
                material["pbrMetallicRoughness"]["baseColorFactor"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{name}: a material has no baseColorFactor"))
                    .iter()
                    .map(|number| number.as_f64().expect("a number") as f32)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        got.sort_by(|a, b| {
            a.iter()
                .zip(b)
                .fold(std::cmp::Ordering::Equal, |acc, (x, y)| {
                    acc.then(x.total_cmp(y))
                })
        });
        for (index, colour) in colours.iter().enumerate() {
            assert_colour(
                &got[index],
                colour,
                &format!("{name}: the exported baseColorFactor is not the colour of the file"),
            );
        }

        // The reference writer pairs the colour with a matte, double sided
        // look; a shininess derived roughness would render the parts as
        // mirrors and blow the colour out to white.
        for material in materials {
            let pbr = &material["pbrMetallicRoughness"];
            assert_eq!(pbr["metallicFactor"], serde_json::json!(0.2), "{name}");
            assert_eq!(pbr["roughnessFactor"], serde_json::json!(0.8), "{name}");
            assert_eq!(material["doubleSided"], serde_json::json!(true), "{name}");

            // What a metallic/roughness workflow cannot hold has to survive in
            // `extras`, or a consumer would have to re-read the JT file.
            let extras = &material["extras"];
            for field in ["ambient", "specular", "shininess", "reflectivity"] {
                assert!(
                    extras.get(field).is_some(),
                    "{name}: the exported material lost the JT {field}"
                );
            }
        }

        // And the primitives actually reference those materials.
        for mesh in json["meshes"].as_array().expect("meshes") {
            for primitive in mesh["primitives"].as_array().expect("primitives") {
                let index = primitive["material"].as_u64().expect("a material index");
                assert!(
                    (index as usize) < materials.len(),
                    "{name}: a primitive points outside the material table"
                );
            }
        }
    }
}

#[test]
fn the_declared_layer_filters_are_the_ones_of_the_reference_dumps() {
    for (name, declared, default) in FILTERS {
        let file = sample(name);
        let graph = file
            .lsg
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no logical scene graph"));

        let found = graph
            .layer_info()
            .map(|info| info.filters.keys().cloned().collect::<Vec<String>>())
            .unwrap_or_default();
        let found = found.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(&found[..], *declared, "{name}: unexpected layer filters");

        let filter = file
            .layer_filter(None)
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(
            filter.as_ref().and_then(|filter| filter.name.as_deref()),
            *default,
            "{name}: the default filter is not the expected one"
        );
    }
}

#[test]
fn no_sample_loses_a_part_to_its_own_filter() {
    // Every part of the samples is either tagged with a layer the default
    // filter covers or carries no tag at all, so the layer filter must not
    // change what the export writes.
    for (name, _, _) in FILTERS {
        let file = sample(name);
        let filtered = file.to_model();
        let unfiltered = file.to_model_unfiltered();

        assert_eq!(
            filtered.nodes.len(),
            unfiltered.nodes.len(),
            "{name}: the filter dropped a node"
        );
        assert_eq!(
            filtered.triangle_count(),
            unfiltered.triangle_count(),
            "{name}: the filter dropped geometry"
        );

        let declared = FILTERS
            .iter()
            .find(|(key, _, _)| key == name)
            .expect("expectation")
            .1;
        let extras = filtered.extras.expect("the model carries extras");
        if declared.is_empty() {
            assert!(
                extras.get("layer_filter").is_none(),
                "{name}: a file without filters records one"
            );
            continue;
        }
        assert_eq!(
            extras["layer_filter"]["name"].as_str().expect("a name"),
            "ALL",
            "{name}: the applied filter is not recorded"
        );
    }
}

#[test]
fn the_key_value_properties_of_a_sample_reach_its_nodes() {
    // A JT file keeps its metadata in the NUL-separated key/value stream behind
    // a node's late loads — the same stream `jt::dump` prints as `properties`.
    // The viewer shows it per node, so it has to travel in `Node::extras` rather
    // than be re-read from the file. The reference dumps are the oracle.
    for (name, _, _) in FILTERS {
        let file = sample(name);
        let model = file.to_model();
        let wanted = reference_properties(name);

        let mut got: Vec<(String, BTreeMap<String, String>)> = model
            .nodes
            .iter()
            .filter_map(|node| Some((node.kind.clone()?, node.extras.as_ref()?)))
            .filter_map(|(kind, extras)| Some((kind, extras.get("properties")?)))
            .map(|(kind, properties)| {
                let pairs = properties
                    .as_object()
                    .unwrap_or_else(|| panic!("{name}: properties is not an object"))
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            value
                                .as_str()
                                .unwrap_or_else(|| {
                                    panic!("{name}: the value of {key} is not a string")
                                })
                                .to_owned(),
                        )
                    })
                    .collect();
                (kind, pairs)
            })
            .collect();
        got.sort();
        got.dedup();

        // The dump expands an instanced subtree once per instance while the
        // model keeps one shared node for it (plate1-9.5 instantiates its screw
        // twice), so the comparison is over the *distinct* property sets: every
        // stream the file declares must be present, and the model must not
        // invent one the dump does not show.
        assert_eq!(
            got.len(),
            wanted.len(),
            "{name}: a node lost or gained its key/value stream"
        );
        for (actual, expected) in got.iter().zip(&wanted) {
            assert_eq!(actual, expected, "{name}: the properties differ");
        }
    }
}

/// Every `properties` object of the reference dump of a sample, paired with the
/// `jt_type` of the node carrying it, sorted and deduplicated. The dump prints
/// them under the node they belong to, so walking the whole JSON for that key is
/// enough — and pairing with the class also proves the reader attached each
/// stream to a node of the right kind instead of merging them somewhere else.
fn reference_properties(name: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("Reference")
        .join(Path::new(name).with_extension("json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{name}: {err}"));

    let mut found = Vec::new();
    let mut stack = vec![&value];
    while let Some(current) = stack.pop() {
        match current {
            serde_json::Value::Object(map) => {
                let kind = map
                    .get("jt_type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                for (key, item) in map {
                    if key == "properties" {
                        let object = item
                            .as_object()
                            .unwrap_or_else(|| panic!("{name}: properties is not an object"));
                        let pairs = object
                            .iter()
                            .map(|(key, value)| {
                                (key.clone(), value.as_str().unwrap_or("").to_owned())
                            })
                            .collect::<BTreeMap<_, _>>();
                        if !pairs.is_empty() {
                            found.push((kind.clone(), pairs));
                        }
                    }
                    stack.push(item);
                }
            }
            serde_json::Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    found.sort();
    found.dedup();
    found
}

#[test]
fn asking_for_a_filter_the_file_does_not_declare_fails() {
    for (name, declared, _) in FILTERS {
        let file = sample(name);
        let error = file
            .to_model_with_filter("nope")
            .expect_err("{name}: nope is not a declared filter");
        let message = error.to_string();
        if declared.is_empty() {
            assert!(message.contains("no layer filters"), "{name}: {message}");
            continue;
        }
        assert!(
            message.contains("ALL, LAYCUR"),
            "{name}: the message has to name the filters: {message}"
        );

        // A declared filter always resolves, even when the file leaves the
        // active one unset.
        let filter = file
            .layer_filter(Some("LAYCUR"))
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(
            filter.expect("LAYCUR is declared").name.as_deref(),
            Some("LAYCUR")
        );
    }
}

#[test]
fn a_move_to_front_package_decodes() {
    // The JT 10.5 NIST assembly stores integer packages with the move-to-front
    // codec (mark 2 codec 5). Before that codec was read, every shape segment
    // that used it failed and the assembly exported no mesh at all.
    let model = sample("NIST/nist-mtc-assembly.jt").to_model();
    assert!(
        model.meshes.len() >= 7,
        "the assembly holds only {} meshes",
        model.meshes.len()
    );
    assert!(
        model.triangle_count() >= 43_924,
        "the assembly holds only {} triangles",
        model.triangle_count()
    );
}
