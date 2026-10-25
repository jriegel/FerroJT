//! Every sample file has to read through the JT loader and reproduce the
//! geometry the reference tooling reports.

use std::path::{Path, PathBuf};

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

/// What a sample is expected to produce.
struct Expectation {
    /// `major.minor` from the version banner.
    version: &'static str,
    /// Number of TOC rows, which the reference echoes in `TocTable`.
    toc: usize,
    /// `NumIndices, NumVertices, NumNormals` of every representation.
    geometry: &'static [(usize, usize, usize)],
}

const EXPECTED: &[(&str, Expectation)] = &[
    (
        "example_block_jt10.3.jt",
        Expectation {
            version: "10.3",
            toc: 9,
            geometry: &[(36, 24, 24), (36, 24, 24), (36, 24, 24)],
        },
    ),
    (
        "example_block_jt8.1.jt",
        Expectation {
            version: "8.1",
            toc: 7,
            geometry: &[(36, 24, 24), (36, 24, 24), (36, 24, 24)],
        },
    ),
    (
        "example_block_jt9.5.jt",
        Expectation {
            version: "9.5",
            toc: 8,
            geometry: &[(36, 24, 24), (36, 24, 24), (36, 24, 24)],
        },
    ),
    (
        "opening_protection_plate1_jt8.0.jt",
        Expectation {
            version: "8.0",
            toc: 16,
            geometry: &[
                (666, 242, 242),
                (387, 108, 108),
                (255, 80, 80),
                (36, 24, 24),
                (1185, 421, 421),
                (507, 164, 164),
                (339, 120, 120),
                (36, 24, 24),
            ],
        },
    ),
    (
        "opening_protection_plate1_jt9.5.jt",
        Expectation {
            version: "9.5",
            toc: 15,
            geometry: &[
                (516, 207, 207),
                (312, 140, 140),
                (228, 112, 112),
                (942, 358, 358),
                (456, 198, 198),
                (312, 150, 150),
            ],
        },
    ),
];

#[test]
fn every_sample_reads() {
    let mut checked = 0usize;
    for path in samples() {
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let file = ferrojt::jt::JtFile::open(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let graph = file
            .lsg
            .as_ref()
            .unwrap_or_else(|| panic!("{}: no logical scene graph", path.display()));
        let root = graph
            .root()
            .unwrap_or_else(|| panic!("{}: the scene graph has no root", path.display()));
        assert_eq!(
            root.class_name(),
            "Partition",
            "{name}: the root of the scene graph is not a partition"
        );

        let mut geometry: Vec<(usize, usize, usize)> = graph
            .loaded
            .iter()
            .filter_map(|stored| stored.element.geometry())
            .map(|g| (g.index_count(), g.vertex_count, g.normal_count))
            .collect();
        geometry.sort_unstable();

        let Some((_, expected)) = EXPECTED.iter().find(|(key, _)| *key == name) else {
            continue;
        };
        checked += 1;

        assert_eq!(
            file.header.version_text(),
            expected.version,
            "{name}: unexpected version"
        );
        assert_eq!(file.toc.len(), expected.toc, "{name}: unexpected TOC");

        let mut want: Vec<_> = expected.geometry.to_vec();
        want.sort_unstable();
        assert_eq!(
            geometry, want,
            "{name}: the representations do not match the reference"
        );

        let model = file.to_model();
        println!(
            "{name}  JT {}  toc {}  nodes {}  attrs {}  props {}  loaded {}  meshes {}  tris {}",
            file.header.version_text(),
            file.toc.len(),
            graph.nodes.len(),
            graph.attributes.len(),
            graph.properties.len(),
            graph.loaded.len(),
            model.meshes.len(),
            model.triangle_count(),
        );
    }
    assert_eq!(
        checked,
        EXPECTED.len(),
        "not every expectation was matched by a sample file"
    );
}
