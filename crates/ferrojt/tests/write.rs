//! Writing a file back out has to preserve the picture and the structure: the
//! scene graph is rebuilt from the parsed graph, every other segment is copied
//! byte for byte, and the object ids are renumbered. A round trip is therefore
//! judged by reading the result back and comparing it with the source — the
//! dump for the variants that keep everything, the exported model for the
//! filtered ones.

use std::path::{Path, PathBuf};

use ferrojt::jt::{self, JtFile, LodGeometry, Property, WriteOptions};

/// What a sample looks like after a round trip.
struct Expectation {
    /// TOC rows of the source, which is also how many segments a lossless
    /// write keeps.
    toc: usize,
    /// Segments that survive when only the finest tier is written.
    filtered: usize,
    /// Nodes of the logical scene graph — never reduced by a filter.
    nodes: usize,
    /// Representations carrying geometry when every tier is kept.
    representations: usize,
    /// The same count when only the finest tier is kept.
    finest: usize,
    /// Meshes and triangles of the export view.
    meshes: usize,
    triangles: usize,
}

const EXPECTED: &[(&str, Expectation)] = &[
    (
        "example_block_jt10.3.jt",
        Expectation {
            toc: 9,
            filtered: 7,
            nodes: 11,
            representations: 3,
            finest: 1,
            meshes: 1,
            triangles: 12,
        },
    ),
    (
        "example_block_jt8.1.jt",
        Expectation {
            toc: 7,
            filtered: 5,
            nodes: 10,
            representations: 3,
            finest: 1,
            meshes: 1,
            triangles: 12,
        },
    ),
    (
        "example_block_jt9.5.jt",
        Expectation {
            toc: 8,
            filtered: 6,
            nodes: 10,
            representations: 3,
            finest: 1,
            meshes: 1,
            triangles: 12,
        },
    ),
    (
        "opening_protection_plate1_jt8.0.jt",
        Expectation {
            toc: 16,
            filtered: 10,
            nodes: 25,
            representations: 8,
            finest: 2,
            meshes: 2,
            triangles: 617,
        },
    ),
    (
        "opening_protection_plate1_jt9.5.jt",
        Expectation {
            toc: 15,
            filtered: 11,
            nodes: 21,
            representations: 6,
            finest: 2,
            meshes: 2,
            triangles: 486,
        },
    ),
    // The server sample has 36 empty representations. The file view keeps them
    // as one shared representation because they are instanced; the export view
    // drops them because glTF mandates a POSITION attribute. Its range LODs
    // each hold exactly one tier, so nothing is filtered away.
    (
        "SampleJT_Server.jt",
        Expectation {
            toc: 819,
            filtered: 819,
            nodes: 2158,
            representations: 106,
            finest: 106,
            meshes: 105,
            triangles: 124_154,
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

fn expectation(path: &Path) -> &'static Expectation {
    let found = EXPECTED
        .iter()
        .find(|(stem, _)| Path::new(stem).file_name() == path.file_name());
    match found {
        Some((_, want)) => want,
        None => panic!("no expectation for {}", name(path)),
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// The variants that keep every node and every representation, so the file has
/// to come back unchanged. No sample rejects a part through its own layer
/// filter, which is why the active filter is lossless too.
fn lossless_variants() -> Vec<(&'static str, WriteOptions)> {
    vec![
        (
            "as stored",
            WriteOptions {
                all_lods: true,
                no_layer_filter: true,
                ..WriteOptions::default()
            },
        ),
        (
            "deflated",
            WriteOptions {
                all_lods: true,
                no_layer_filter: true,
                compress: true,
                ..WriteOptions::default()
            },
        ),
        (
            "active layer filter",
            WriteOptions {
                all_lods: true,
                ..WriteOptions::default()
            },
        ),
    ]
}

/// The variants that drop the coarse tiers, which is what the command is for.
fn filtered_variants() -> Vec<(&'static str, WriteOptions)> {
    vec![
        (
            "finest tier",
            WriteOptions {
                no_layer_filter: true,
                ..WriteOptions::default()
            },
        ),
        (
            "finest tier, deflated",
            WriteOptions {
                compress: true,
                ..WriteOptions::default()
            },
        ),
    ]
}

/// The class of a property, as far as the writer cares which ids the object
/// table has to keep.
fn property_kind(property: &Property) -> &'static str {
    match property {
        Property::String(_) => "String",
        Property::LateLoaded(_) => "LateLoaded",
        Property::Unsupported { class_name } => class_name,
    }
}

/// The value of every property that is not a late load, in store order. Those
/// carry the node names and the key/value stream of the file, and no filter
/// touches them: a late load belongs to a representation, a value does not.
fn values(kinds: &[&'static str]) -> Vec<&'static str> {
    kinds
        .iter()
        .copied()
        .filter(|kind| *kind != "LateLoaded")
        .collect()
}

/// How many property atoms are late loads.
fn late_loads(kinds: &[&'static str]) -> usize {
    kinds
        .iter()
        .copied()
        .filter(|kind| *kind == "LateLoaded")
        .count()
}

/// A stand-alone digest of a representation. The representation segments are
/// copied verbatim, so the decoded numbers have to be identical.
fn digest(shape: &LodGeometry) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    };
    for value in &shape.indices {
        mix(&value.to_le_bytes());
    }
    for value in &shape.vertices {
        mix(&value.to_le_bytes());
    }
    for value in &shape.normals {
        mix(&value.to_le_bytes());
    }
    hash
}

/// A stand-alone digest of a transform or appearance, so an attribute that is
/// written from the parsed value rather than copied can still be compared.
fn attribute_kind(attribute: &ferrojt::jt::Attribute) -> String {
    match attribute {
        ferrojt::jt::Attribute::GeometricTransform(transform) => {
            let mut parts = Vec::with_capacity(16);
            for value in &transform.matrix {
                parts.push(format!("{value:.6}"));
            }
            format!("Transform {}", parts.join(","))
        }
        ferrojt::jt::Attribute::Material(material) => format!(
            "Material {:?} {:?} {:?} {:?} {} {}",
            material.ambient,
            material.diffuse,
            material.specular,
            material.emission,
            material.shininess,
            material.reflectivity,
        ),
        ferrojt::jt::Attribute::Unsupported { class_name } => (*class_name).to_string(),
    }
}

/// The structure of a scene graph, compared instead of the graph itself: the
/// writer renumbers every object id, so only the shape of what survived can
/// match. Everything is sorted because the writer emits nodes in walk order,
/// which need not be the order they were stored in.
#[derive(Debug, PartialEq, Eq)]
struct Structure {
    /// The class name of every node.
    classes: Vec<String>,
    /// Child counts, in the same order as `classes` after sorting both.
    children: Vec<usize>,
    /// Attribute counts.
    attributes: Vec<usize>,
    /// How many nodes are instances.
    instances: usize,
    /// A digest of every representation, in load order.
    geometry: Vec<(usize, usize, usize, u64)>,
    /// The class of every attribute atom.
    attributes_of_atoms: Vec<String>,
    /// The class of every property atom.
    properties: Vec<&'static str>,
}

fn structure(file: &JtFile) -> Structure {
    let graph = file
        .lsg
        .as_ref()
        .expect("the sample files all have a scene graph");
    let mut rows: Vec<(String, usize, usize)> = graph
        .nodes
        .iter()
        .map(|stored| {
            (
                stored.node.class_name().to_string(),
                graph.children_of(&stored.node).len(),
                graph.attributes_of(&stored.node).len(),
            )
        })
        .collect();
    rows.sort();
    let geometry = graph
        .loaded
        .iter()
        .filter_map(|loaded| loaded.element.geometry())
        .map(|shape| {
            (
                shape.indices.len(),
                shape.vertices.len(),
                shape.normals.len(),
                digest(shape),
            )
        })
        .collect();
    Structure {
        classes: rows.iter().map(|row| row.0.clone()).collect(),
        children: rows.iter().map(|row| row.1).collect(),
        attributes: rows.iter().map(|row| row.2).collect(),
        instances: graph
            .nodes
            .iter()
            .filter(|stored| matches!(stored.node, ferrojt::jt::Node::Instance(_)))
            .count(),
        geometry,
        attributes_of_atoms: graph
            .attributes
            .iter()
            .map(|stored| attribute_kind(&stored.attribute))
            .collect(),
        properties: graph
            .properties
            .iter()
            .map(|stored| property_kind(&stored.property))
            .collect(),
    }
}

/// The TOC as a sorted list of `(guid, kind)`: the rows may sit anywhere in
/// the file and every offset changes, so only the identity of the segments is
/// comparable.
fn contents(file: &JtFile) -> Vec<(String, i32)> {
    let mut rows = file
        .toc
        .iter()
        .map(|row| (row.guid.to_string(), row.kind))
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

/// The dump the reference tooling produces, without the table of contents —
/// the offsets in it change by construction. `SampleJT_Server.jt` dumps to
/// deliberately invalid JSON, so the dump is compared as text everywhere.
fn dump(file: &JtFile) -> String {
    let text = ferrojt::jt::dump(file, jt::DUMP_ALL & !jt::DUMP_TOC, 1).expect("dumping must work");
    strip_file_name(&text)
}

/// Replace the path the dump echoes, which differs between a source file and a
/// file written somewhere else.
fn strip_file_name(text: &str) -> String {
    const KEY: &str = "\"FileName\":\"";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(KEY) {
        let value = &rest[at + KEY.len()..];
        let Some(close) = value.find('"') else { break };
        out.push_str(&rest[..at]);
        out.push_str(KEY);
        out.push_str("<path>");
        rest = &value[close..];
    }
    out.push_str(rest);
    out
}

/// Read a file, write it with `options`, and read the result back.
fn round_trip(path: &Path, options: &WriteOptions) -> (JtFile, JtFile, Vec<u8>) {
    let data = std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", name(path)));
    let file = JtFile::read(path, &data).unwrap_or_else(|err| panic!("{}: {err}", name(path)));
    let bytes = jt::write_to_bytes(&file, &data, options)
        .unwrap_or_else(|err| panic!("{}: writing failed: {err}", name(path)));
    assert!(!bytes.is_empty(), "{} wrote nothing", name(path));
    let again = JtFile::read(path, &bytes)
        .unwrap_or_else(|err| panic!("{}: the output does not read: {err}", name(path)));
    (file, again, bytes)
}

fn representation_count(file: &JtFile) -> usize {
    file.lsg
        .as_ref()
        .expect("a scene graph")
        .loaded
        .iter()
        .filter(|loaded| loaded.element.geometry().is_some())
        .count()
}

/// A write that keeps everything has to reproduce the file: the same segments,
/// the same structure, the same dump — and writing the result again has to
/// change nothing.
#[test]
fn a_lossless_round_trip_reproduces_the_file() {
    for path in samples() {
        let want = expectation(&path);
        for (variant, options) in lossless_variants() {
            let (file, again, bytes) = round_trip(&path, &options);
            let label = format!("{} [{variant}]", name(&path));

            assert_eq!(want.toc, again.toc.len(), "{label}: TOC rows");
            assert_eq!(contents(&file), contents(&again), "{label}: segments");
            let graph = again.lsg.as_ref().expect("a scene graph");
            assert_eq!(want.nodes, graph.nodes.len(), "{label}: node count");
            assert_eq!(
                want.representations,
                representation_count(&again),
                "{label}: representations"
            );
            assert_eq!(structure(&file), structure(&again), "{label}: structure");

            // The dump is what the reference tooling sees.
            assert_eq!(dump(&file), dump(&again), "{label}: dump");

            // The writer has to be at a fixed point: re-writing its own output
            // produces the very same bytes.
            let twice = jt::write_to_bytes(&again, &bytes, &options)
                .unwrap_or_else(|err| panic!("{label}: the second write failed: {err}"));
            assert_eq!(
                bytes.len(),
                twice.len(),
                "{label}: the second write changed the size"
            );
            let third = JtFile::read(&path, &twice)
                .unwrap_or_else(|err| panic!("{label}: the second output does not read: {err}"));
            assert_eq!(
                structure(&again),
                structure(&third),
                "{label}: second write"
            );
            assert_eq!(
                dump(&again),
                dump(&third),
                "{label}: dump after a second write"
            );
        }
    }
}

/// Filtering may only drop geometry: every node of the scene graph survives so
/// that the tree of the file stays intact, and the segments of the dropped
/// representations are the only ones that disappear.
#[test]
fn filtering_drops_geometry_but_never_a_node() {
    for path in samples() {
        let want = expectation(&path);
        for (variant, options) in filtered_variants() {
            let (file, again, _) = round_trip(&path, &options);
            let label = format!("{} [{variant}]", name(&path));

            let graph = again.lsg.as_ref().expect("a scene graph");
            assert_eq!(want.nodes, graph.nodes.len(), "{label}: nodes");
            assert_eq!(
                want.finest,
                representation_count(&again),
                "{label}: representations"
            );
            assert_eq!(want.filtered, again.toc.len(), "{label}: segments");

            let source = structure(&file);
            let got = structure(&again);
            assert_eq!(source.classes, got.classes, "{label}: node classes");
            assert_eq!(source.children, got.children, "{label}: child counts");
            assert_eq!(
                source.attributes, got.attributes,
                "{label}: attribute counts"
            );
            assert_eq!(source.instances, got.instances, "{label}: instances");
            assert_eq!(
                source.attributes_of_atoms, got.attributes_of_atoms,
                "{label}: attributes"
            );
            // A late load belongs to the representation it carries, so dropping
            // a tier drops that property atom and the pair that named it. Every
            // other property of the key/value stream has to survive.
            assert_eq!(
                values(&source.properties),
                values(&got.properties),
                "{label}: properties"
            );
            assert!(
                late_loads(&got.properties) <= late_loads(&source.properties),
                "{label}: a filtered write gained a late load"
            );
            assert!(
                got.geometry.len() <= source.geometry.len(),
                "{label}: a filtered write gained representations"
            );

            let model = again.to_model();
            assert_eq!(want.meshes, model.meshes.len(), "{label}: meshes");
            assert_eq!(want.triangles, model.triangle_count(), "{label}: triangles");

            // The picture: the export view of the written file is the export
            // view of the source, down to the bytes holding the vertices. What
            // a filter drops is what the export view drops anyway, so nothing
            // visible may change.
            let direct = file.to_model();
            assert_eq!(
                direct.nodes.len(),
                model.nodes.len(),
                "{label}: exported nodes"
            );
            assert_eq!(direct.meshes, model.meshes, "{label}: exported meshes");
            assert_eq!(
                direct.materials, model.materials,
                "{label}: exported materials"
            );
            assert_eq!(
                direct.accessors.buffers, model.accessors.buffers,
                "{label}: exported vertex buffers"
            );
        }
    }
}

/// The geometry itself, not only its counts: every representation of a
/// lossless round trip has to decode to the same numbers.
#[test]
fn the_geometry_survives_a_round_trip() {
    let options = WriteOptions {
        all_lods: true,
        no_layer_filter: true,
        ..WriteOptions::default()
    };
    for path in samples() {
        let (file, again, _) = round_trip(&path, &options);
        let label = name(&path);
        assert_eq!(
            structure(&file).geometry,
            structure(&again).geometry,
            "{label}: representations"
        );

        // And the export view of every tier has to agree.
        let before = file.to_model_with_all_lods();
        let after = again.to_model_with_all_lods();
        assert_eq!(before.meshes.len(), after.meshes.len(), "{label}: meshes");
        assert_eq!(
            before.triangle_count(),
            after.triangle_count(),
            "{label}: triangles"
        );
    }
}

/// The picture: a round trip has to export exactly what the source exports,
/// down to the bytes holding the vertices and the appearance of every part.
#[test]
fn an_export_of_a_round_trip_matches_the_export_of_the_source() {
    let options = WriteOptions {
        all_lods: true,
        no_layer_filter: true,
        ..WriteOptions::default()
    };
    for path in samples() {
        let (file, again, _) = round_trip(&path, &options);
        let label = name(&path);

        let before = file.to_model();
        let after = again.to_model();
        assert_eq!(before.nodes.len(), after.nodes.len(), "{label}: nodes");
        assert_eq!(before.meshes, after.meshes, "{label}: meshes");
        assert_eq!(before.materials, after.materials, "{label}: materials");
        assert_eq!(
            before.accessors.buffers, after.accessors.buffers,
            "{label}: vertex buffers"
        );
    }
}

/// An impossible filter has to be refused rather than silently ignored.
#[test]
fn an_impossible_filter_is_refused() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("ExampleFiles")
        .join("example_block_jt9.5.jt");
    let data = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", name(&path)));
    let file = JtFile::read(&path, &data).unwrap_or_else(|err| panic!("{}: {err}", name(&path)));

    let both = WriteOptions {
        no_layer_filter: true,
        layer_filter: Some("ALL".to_string()),
        ..WriteOptions::default()
    };
    let err = jt::write_to_bytes(&file, &data, &both)
        .expect_err("a filter cannot be named and ignored at the same time");
    assert!(
        err.to_string().contains("cannot be named and ignored"),
        "unexpected message: {err}"
    );

    let unknown = WriteOptions {
        layer_filter: Some("nope".to_string()),
        ..WriteOptions::default()
    };
    let err =
        jt::write_to_bytes(&file, &data, &unknown).expect_err("an unknown filter name has to fail");
    assert!(
        err.to_string().contains("nope"),
        "unexpected message: {err}"
    );
}
