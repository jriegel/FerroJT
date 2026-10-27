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
