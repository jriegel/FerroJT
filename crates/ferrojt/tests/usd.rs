//! A USD export writes each mesh exactly once, references it from every node
//! that draws it, and collapses materials that shade identically. The layer
//! filter that produced the model has to be recorded so a consumer can see
//! which parts were gated out.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ferrojt::export::UsdExporter;
use ferrojt::model::Model;

/// What a sample exports to USD.
struct Expectation {
    /// Meshes in the model, one per part.
    meshes: usize,
    /// `UsdPreviewSurface` prims after materials are collapsed by value.
    materials: usize,
    /// Wrapper prims for nodes reachable from more than one parent.
    shared: usize,
    /// The layer filter the export records, `None` when the file has none.
    filter: Option<&'static str>,
}

const EXPECTED: &[(&str, Expectation)] = &[
    (
        "example_block_jt10.3.jt",
        Expectation {
            meshes: 1,
            materials: 1,
            shared: 0,
            filter: Some("ALL"),
        },
    ),
    // JT 8.1 stores no layer information at all.
    (
        "example_block_jt8.1.jt",
        Expectation {
            meshes: 1,
            materials: 1,
            shared: 0,
            filter: None,
        },
    ),
    (
        "example_block_jt9.5.jt",
        Expectation {
            meshes: 1,
            materials: 1,
            shared: 0,
            filter: Some("ALL"),
        },
    ),
    (
        "opening_protection_plate1_jt8.0.jt",
        Expectation {
            meshes: 2,
            materials: 2,
            shared: 1,
            filter: None,
        },
    ),
    // One screw, instantiated twice: the second visit is a wrapper.
    (
        "opening_protection_plate1_jt9.5.jt",
        Expectation {
            meshes: 2,
            materials: 2,
            shared: 1,
            filter: Some("ALL"),
        },
    ),
    // 2158 nodes, 198 of them shared, and 23 appearances that really differ.
    (
        "SampleJT_Server.jt",
        Expectation {
            meshes: 106,
            materials: 23,
            shared: 529,
            filter: None,
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

fn export(model: &Model) -> String {
    UsdExporter
        .to_text(model)
        .unwrap_or_else(|err| panic!("{}: {err}", model.source.as_deref().unwrap_or("model")))
}

/// The shading values a `UsdPreviewSurface` carries, as the writer sees them.
/// Two materials with the same key must end up as one prim.
fn shading(model: &Model) -> BTreeSet<[u32; 10]> {
    model
        .materials
        .iter()
        .map(|material| {
            [
                material.base_color.factor[0].to_bits(),
                material.base_color.factor[1].to_bits(),
                material.base_color.factor[2].to_bits(),
                material.base_color.factor[3].to_bits(),
                material.metallic.factor[0].to_bits(),
                material.roughness.factor[0].to_bits(),
                material.emissive.factor[0].to_bits(),
                material.emissive.factor[1].to_bits(),
                material.emissive.factor[2].to_bits(),
                u32::from(material.double_sided),
            ]
        })
        .collect()
}

#[test]
fn every_sample_produces_a_well_formed_layer() {
    let mut checked = 0usize;
    for path in samples() {
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let Some((_, expected)) = EXPECTED.iter().find(|(key, _)| *key == name) else {
            continue;
        };
        checked += 1;

        let file = ferrojt::jt::JtFile::open(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let model = file.to_model();
        let text = export(&model);

        // A layer starts with the version line and states its units.
        assert!(text.starts_with("#usda 1.0\n("), "{name}: no USD header");
        assert!(text.contains("metersPerUnit = 0.001"), "{name}: mm missing");
        assert!(text.contains("upAxis = \"Z\""), "{name}: up axis missing");

        // One library group per mesh, and no geometry anywhere else.
        assert_eq!(
            text.matches("def Xform \"GEOMETRY_").count(),
            expected.meshes,
            "{name}: the geometry library holds the wrong number of meshes"
        );
        assert_eq!(
            text.matches("def Xform \"GEOMETRY_").count(),
            model.meshes.len(),
            "{name}: a mesh of the model is missing from the library"
        );
        assert_eq!(
            text.matches("point3f[] points").count(),
            expected.meshes,
            "{name}: geometry is authored more than once"
        );

        // Every node that draws a mesh points at the library instead.
        let attachments = model
            .nodes
            .iter()
            .filter(|node| !node.instances.is_empty())
            .count();
        assert_eq!(
            text.matches("prepend references = [</FerroJT_Geometry/")
                .count(),
            attachments,
            "{name}: a node drawing a mesh has no reference to it"
        );

        // A node reached from several parents is referenced, not copied.
        assert_eq!(
            text.matches(": shared subtree\"").count(),
            expected.shared,
            "{name}: wrong number of shared-subtree wrappers"
        );

        // Materials collapse onto the values their shader carries.
        assert_eq!(
            text.matches("def Material \"Material_").count(),
            expected.materials,
            "{name}: wrong number of USD materials"
        );
        assert_eq!(
            expected.materials,
            shading(&model).len(),
            "{name}: the pinned material count no longer matches the model"
        );
        assert!(
            expected.materials <= model.materials.len(),
            "{name}: deduplication cannot produce more materials than the model has"
        );

        // The filter that produced the model is recorded in the layer.
        match expected.filter {
            Some(filter) => {
                assert!(
                    text.contains(&format!("string \"ferrojt:layer_filter\" = \"{filter}\"")),
                    "{name}: the layer filter is not recorded"
                );
                assert!(
                    text.contains("int[] \"ferrojt:layers\" = ["),
                    "{name}: the layers of the filter are not recorded"
                );
            }
            None => assert!(
                !text.contains("ferrojt:layer_filter"),
                "{name}: a filter was recorded for a file that declares none"
            ),
        }
    }
    assert_eq!(
        checked,
        EXPECTED.len(),
        "not every expectation was matched by a sample file"
    );
}

#[test]
fn the_library_is_invisible_and_its_meshes_instanceable() {
    let file = ferrojt::jt::JtFile::open(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the workspace root")
            .join("data")
            .join("ExampleFiles")
            .join("example_block_jt9.5.jt"),
    )
    .expect("open sample");
    let model = file.to_model();
    let text = export(&model);

    // The library is a sibling of the default prim, closed before it opens,
    // and hidden from the scene.
    assert!(text.contains(
        "\n}\n\ndef Xform \"FerroJT_Geometry\"\n{\n    token visibility = \"invisible\""
    ));
    // The Mesh prims carry the flag so a consumer keeps them as instances; the
    // Xform groups around them do not, or binding to them would be illegal.
    assert_eq!(
        text.matches("instanceable = true").count(),
        model.meshes.len()
    );
    // The binding sits on the group, so a reference to it brings the
    // appearance along.
    assert!(text.contains("rel material:binding = </example_block_jt9/Materials/Material_0>"));
    // And the geometry itself is a triangle mesh with explicit faces.
    assert!(text.contains("uniform token subdivisionScheme = \"linear\""));
    assert!(text.contains("int[] faceVertexCounts"));
    assert!(text.contains("int[] faceVertexIndices"));
}

#[test]
fn a_forced_layer_filter_reaches_the_layer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("ExampleFiles");
    let file = ferrojt::jt::JtFile::open(root.join("opening_protection_plate1_jt9.5.jt"))
        .expect("open sample");

    // `LAYCUR` is the second filter of the file and holds only layer 20, which
    // is where both parts are. Nothing is dropped, but the layer has to say
    // which filter ran.
    let model = file.to_model_with_filter("LAYCUR").expect("filter");
    let text = export(&model);
    assert!(text.contains("string \"ferrojt:layer_filter\" = \"LAYCUR\""));
    assert!(text.contains("int[] \"ferrojt:layers\" = [20]"));
    assert_eq!(
        text.matches("def Xform \"GEOMETRY_").count(),
        2,
        "LAYCUR covers the layers both parts are on"
    );

    // Ignoring the filters entirely leaves the geometry alone and records
    // nothing about it.
    let unfiltered = export(&file.to_model_unfiltered());
    assert!(!unfiltered.contains("ferrojt:layer_filter"));
    assert_eq!(
        unfiltered.matches("def Xform \"GEOMETRY_").count(),
        2,
        "no sample loses a part to its own filter"
    );

    // An unknown filter is refused before anything is written.
    let error = file.to_model_with_filter("nope").unwrap_err();
    assert!(error.to_string().contains("there is no layer filter"));
}

#[test]
fn the_unit_and_up_axis_of_the_model_reach_the_header() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("ExampleFiles");
    let file = ferrojt::jt::JtFile::open(root.join("example_block_jt9.5.jt")).expect("open sample");
    let mut model = file.to_model();
    model.unit = Some("inch".into());
    model.up_axis = Some("Y".into());
    let text = export(&model);
    assert!(text.contains("metersPerUnit = 0.0254"));
    assert!(text.contains("upAxis = \"Y\""));
}

#[test]
fn exporting_the_same_model_twice_gives_the_same_text() {
    // A diffable layer is the point of the ASCII writer, so nothing may depend
    // on hash order or iteration order.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("ExampleFiles");
    for name in [
        "example_block_jt9.5.jt",
        "opening_protection_plate1_jt9.5.jt",
        "SampleJT_Server.jt",
    ] {
        let file = ferrojt::jt::JtFile::open(root.join(name)).expect("open sample");
        let model = file.to_model();
        assert_eq!(export(&model), export(&model), "{name}: not deterministic");
    }
}
