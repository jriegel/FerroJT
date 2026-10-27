//! An export carries one representation per part: the finest level of detail
//! of a range LOD. The coarser tiers of the same part must not appear as extra
//! meshes, and every mesh has to say which tier it came from.

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
