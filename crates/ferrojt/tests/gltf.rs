//! glTF in, JT out.
//!
//! Every sample goes the whole way: JT -> neutral model -> GLB -> neutral
//! model -> JT, and the file that comes back out is checked against the source.
//! Each hop is covered on its own elsewhere (`tests/model.rs` exports to glTF,
//! `tests/build.rs` builds a JT file from nothing), so what is pinned here is
//! the seam between them: that a GLB FerroJT wrote can be read by FerroJT, and
//! that the model it yields feeds the JT writer at all.
//!
//! The conversion is not lossless and does not pretend to be. Three things are
//! expected to differ and are pinned as such:
//!
//! - The export declares metres and `+Y` up and the JT writer reconciles that
//!   on the root partition instead of rescaling every vertex. So the written
//!   file has **one node more** than the source and its root carries a scale of
//!   1000 plus a quarter turn about X — see
//!   [`the_unit_conversion_lives_on_the_root_not_in_the_vertices`].
//! - Only the finest level of detail survives the glTF hop, so geometry is
//!   compared against the finest tier of the source.
//! - The topological coder of JT 9.x and 10.x re-orders and re-explodes the
//!   vertex table, so geometry is compared as a set of welded triangles.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use ferrojt::jt::{
    self, Attribute, JtFile, JtVersion, LodGeometry, Node, SceneGraph, transform_of,
};
use ferrojt::model::{self, ComponentType, Format, Model, Topology};

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// The sample files of the repository.
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

/// The sample whose name contains `needle`.
fn sample(needle: &str) -> PathBuf {
    samples()
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(needle))
        })
        .unwrap_or_else(|| panic!("no sample matching {needle:?}"))
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// A unique temporary path. There is no `tempfile` dev-dependency, so the
/// process id keeps parallel test binaries from colliding and the counter keeps
/// the tests inside one binary apart — several of them route the same sample
/// through [`through_glb`], and two would otherwise share one GLB and delete it
/// out from under each other.
fn temp_path(stem: &str, extension: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "ferrojt-gltf-{}-{}-{stem}.{extension}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// Put bytes in a temporary file so the header probe, which takes a path, can
/// see them.
fn temp_file(stem: &str, bytes: &[u8]) -> PathBuf {
    let path = temp_path(stem, "jt");
    std::fs::write(&path, bytes).expect("writing a temporary file must work");
    path
}

/// JT -> model -> GLB -> model. The GLB goes through a file on disk so that the
/// format detection of [`ferrojt::open`] is part of the path being tested.
fn through_glb(source: &Path) -> (JtFile, Model, Vec<u8>, Model) {
    let label = name(source);
    let data = std::fs::read(source).unwrap_or_else(|err| panic!("{label}: {err}"));
    let file = JtFile::read(source, &data).unwrap_or_else(|err| panic!("{label}: {err}"));
    let model = file.to_model();
    let glb = ferrojt::export::to_format(Format::Glb, &model)
        .unwrap_or_else(|err| panic!("{label}: exporting to GLB failed: {err}"));
    let path = temp_path(&label, "glb");
    std::fs::write(&path, &glb).unwrap_or_else(|err| panic!("{label}: {err}"));
    let again =
        ferrojt::open(&path).unwrap_or_else(|err| panic!("{label}: the GLB we wrote: {err}"));
    let _ = std::fs::remove_file(&path);
    (file, model, glb, again)
}

/// Read the bytes [`ferrojt::jt::model_to_bytes`] produced back as a file.
fn written(source: &Path, bytes: &[u8]) -> JtFile {
    let label = name(source);
    JtFile::read(source, bytes).unwrap_or_else(|err| panic!("{label}: the output: {err}"))
}

// ---------------------------------------------------------------------------
// geometry, comparable across revisions
// ---------------------------------------------------------------------------

/// A position welded to an integer key. The coder of JT 9.x and 10.x keeps one
/// vertex instance per triangle corner, so a vertex table cannot be compared
/// across revisions — only the set of positions it is built from can.
fn position_key(point: &[f32]) -> [u64; 3] {
    let mut key = [0u64; 3];
    for (slot, value) in key.iter_mut().zip(point) {
        *slot = (f64::from(*value) * 1_000.0).round() as i64 as u64;
    }
    key
}

/// A triangle as three welded corners, rotated to start at its smallest one.
/// The coder re-orders the corners of a triangle too, so the rotation makes the
/// key independent of the fan order.
fn triangle_key(corners: &[[u64; 3]; 3]) -> [u64; 9] {
    let start = (0..3)
        .min_by_key(|at| corners[*at])
        .expect("a triangle has three corners");
    [0, 1, 2]
        .iter()
        .flat_map(|at| corners[(start + at) % 3])
        .collect::<Vec<_>>()
        .try_into()
        .expect("nine components")
}

/// Every triangle of one representation.
fn geometry_triangles(geometry: &LodGeometry) -> BTreeSet<[u64; 9]> {
    let mut out = BTreeSet::new();
    for triangle in geometry.indices.chunks_exact(3) {
        let corners = [0, 1, 2].map(|at| {
            let slot = triangle[at] as usize;
            position_key(&geometry.vertices[slot * 3..slot * 3 + 3])
        });
        out.insert(triangle_key(&corners));
    }
    out
}

/// Every representation reachable from one node of a graph, each counted once.
///
/// An `Instance` and the shape it instantiates draw the same representation and
/// the walk reaches both, so the set is keyed on the loaded element the late
/// load resolves to rather than on the node.
fn representations(graph: &SceneGraph, start: usize) -> Vec<&LodGeometry> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let mut visited: BTreeSet<usize> = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(pos) = stack.pop() {
        if !visited.insert(pos) {
            continue;
        }
        let Some(node) = graph.node_at(pos) else {
            continue;
        };
        for load in node.late_loads() {
            let Some(geometry) = graph.geometry_for(load) else {
                continue;
            };
            // An empty representation carries no triangle at all; the reader
            // keeps the node and drops the geometry, and so does every export,
            // because glTF mandates POSITION.
            if geometry.vertices.is_empty() {
                continue;
            }
            if let Some(id) = graph.loaded_pos(load) {
                if seen.insert(id) {
                    out.push(geometry);
                }
            }
        }
        stack.extend(graph.children_of(node).into_iter().flatten());
    }
    out
}

/// The finest level of detail of a JT file: the first group under each of its
/// range LODs, which is the tier [`JtFile::to_model`] exports. A file without a
/// range LOD holds a single tier, which is all of its geometry — and so is a
/// file written from a model, because the builder has no LOD node at all.
fn finest_triangles(file: &JtFile) -> BTreeSet<[u64; 9]> {
    let graph = file.lsg.as_ref().expect("a scene graph");
    let mut out = BTreeSet::new();
    let mut lods = 0;
    for stored in &graph.nodes {
        if stored.node.class_name() != "RangeLOD" {
            continue;
        }
        lods += 1;
        if let Some(first) = graph.children_of(&stored.node).into_iter().flatten().next() {
            for geometry in representations(graph, first) {
                out.extend(geometry_triangles(geometry));
            }
        }
    }
    if lods == 0 {
        for pos in 0..graph.nodes.len() {
            for geometry in representations(graph, pos) {
                out.extend(geometry_triangles(geometry));
            }
        }
    }
    out
}

/// One float component of a neutral attribute.
fn read_component(kind: ComponentType, bytes: &[u8]) -> Option<f64> {
    let take = |n: usize| -> Option<&[u8]> {
        if bytes.len() < n {
            None
        } else {
            Some(&bytes[..n])
        }
    };
    Some(match kind {
        ComponentType::I8 => take(1)?[0] as i8 as f64,
        ComponentType::U8 => take(1)?[0] as f64,
        ComponentType::I16 => i16::from_le_bytes(take(2)?.try_into().ok()?) as f64,
        ComponentType::U16 => u16::from_le_bytes(take(2)?.try_into().ok()?) as f64,
        ComponentType::I32 => i32::from_le_bytes(take(4)?.try_into().ok()?) as f64,
        ComponentType::U32 => u32::from_le_bytes(take(4)?.try_into().ok()?) as f64,
        ComponentType::I64 => i64::from_le_bytes(take(8)?.try_into().ok()?) as f64,
        ComponentType::U64 => u64::from_le_bytes(take(8)?.try_into().ok()?) as f64,
        ComponentType::F32 => f32::from_le_bytes(take(4)?.try_into().ok()?) as f64,
        ComponentType::F64 => f64::from_le_bytes(take(8)?.try_into().ok()?),
        ComponentType::F16 => half::f16::from_le_bytes(take(2)?.try_into().ok()?).to_f64(),
    })
}

/// The welded position of every vertex slot behind an attribute.
fn position_corners(model: &Model, handle: usize) -> Option<Vec<[u64; 3]>> {
    let attribute = model.accessors.attribute(handle)?;
    let bytes = model.accessors.attribute_bytes(handle)?;
    let width = attribute.component_type.byte_len();
    let mut out = Vec::with_capacity(attribute.count);
    for element in bytes.chunks_exact(attribute.element_byte_len()) {
        let mut point = [0.0f64; 3];
        for (slot, component) in point.iter_mut().zip(element.chunks_exact(width)) {
            *slot = read_float(attribute.component_type, component)?;
        }
        out.push(point.map(|value| (value * 1_000.0).round() as i64 as u64));
    }
    Some(out)
}

/// The slots behind an index attribute.
fn index_list(model: &Model, handle: usize) -> Option<Vec<u32>> {
    let attribute = model.accessors.attribute(handle)?;
    let bytes = model.accessors.attribute_bytes(handle)?;
    let width = attribute.component_type.byte_len();
    let mut out = Vec::with_capacity(attribute.count);
    for chunk in bytes.chunks_exact(width) {
        out.push(read_component(attribute.component_type, chunk)? as u32);
    }
    Some(out)
}

/// A POSITION component, which is always a float in the models under test.
fn read_float(kind: ComponentType, bytes: &[u8]) -> Option<f64> {
    match kind {
        ComponentType::F16 | ComponentType::F32 | ComponentType::F64 => read_component(kind, bytes),
        _ => None,
    }
}

/// Every triangle of a neutral model, welded the way a JT one is.
fn model_triangles(model: &Model) -> BTreeSet<[u64; 9]> {
    let mut out = BTreeSet::new();
    for mesh in &model.meshes {
        for primitive in &mesh.primitives {
            if primitive.topology != Topology::TriangleList {
                continue;
            }
            let Some(position) = primitive.attributes.iter().find_map(|handle| {
                model
                    .accessors
                    .attribute(*handle)
                    .filter(|attribute| attribute.name == "POSITION")
                    .map(|_| *handle)
            }) else {
                continue;
            };
            let Some(corners) = position_corners(model, position) else {
                continue;
            };
            let Some(slots) = primitive
                .indices
                .and_then(|handle| index_list(model, handle))
            else {
                continue;
            };
            for triangle in slots.chunks_exact(3) {
                let corners = [0, 1, 2].map(|at| corners[triangle[at] as usize]);
                out.insert(triangle_key(&corners));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// the rest of the file, comparable as multisets
// ---------------------------------------------------------------------------

/// The class name of every node of a graph, sorted. The writer renumbers every
/// object id and walks in its own order, so only the multiset of classes can
/// match.
fn classes(file: &JtFile) -> Vec<String> {
    let graph = file.lsg.as_ref().expect("a scene graph");
    let mut out: Vec<String> = graph
        .nodes
        .iter()
        .map(|stored| stored.node.class_name().to_string())
        .collect();
    out.sort();
    out
}

/// The class name of every node of a graph, counted.
fn class_counts(file: &JtFile) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for class in classes(file) {
        *out.entry(class).or_default() += 1;
    }
    out
}

/// How far below the root a class first appears, following the reference of an
/// `Instance` to its target the way a consumer walk does. `None` when the class
/// is unreachable from the root at all. A consumer descends a fixed handful of
/// classes from the root to reach a part, so this depth is what decides whether
/// a file renders or merely parses.
fn depth_of_class(file: &JtFile, class: &str) -> Option<usize> {
    let graph = file.lsg.as_ref().expect("a scene graph");
    let mut stack = vec![(graph.root.expect("a root node"), 0usize)];
    let mut best = None;
    while let Some((pos, depth)) = stack.pop() {
        let node = graph.node_at(pos).expect("a node");
        if node.class_name() == class {
            best = Some(best.map_or(depth, |seen: usize| seen.min(depth)));
        }
        let depth = depth + 1;
        if let Some(target) = graph.instance_target(node) {
            stack.push((target, depth));
        }
        stack.extend(
            graph
                .children_of(node)
                .into_iter()
                .flatten()
                .map(|c| (c, depth)),
        );
    }
    best
}

/// The shading of every material attribute of a graph, sorted. The writer
/// collapses materials by their shading values, so the multiset of those values
/// is what has to survive.
fn shadings(file: &JtFile) -> Vec<String> {
    let graph = file.lsg.as_ref().expect("a scene graph");
    let mut out: Vec<String> = graph
        .attributes
        .iter()
        .filter_map(|stored| match &stored.attribute {
            Attribute::Material(material) => Some(format!(
                "{:?} {:?} {:?} {:?} {} {}",
                material.ambient,
                material.diffuse,
                material.specular,
                material.emission,
                material.shininess,
                material.reflectivity,
            )),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// How many nodes of a graph are instances.
fn instance_count(file: &JtFile) -> usize {
    file.lsg
        .as_ref()
        .expect("a scene graph")
        .nodes
        .iter()
        .filter(|stored| matches!(stored.node, Node::Instance(_)))
        .count()
}

// ---------------------------------------------------------------------------
// the import on its own
// ---------------------------------------------------------------------------

#[test]
fn a_glb_file_is_detected_and_reads_as_a_model() {
    let source = sample("example_block_jt9.5");
    let (_, _, glb, again) = through_glb(&source);
    assert_eq!(Format::detect("x.glb"), Format::Glb);
    assert!(Format::Glb.is_readable());
    assert!(!glb.is_empty());

    // What the glTF reader declares about the file it just read.
    assert_eq!(again.unit.as_deref(), Some("meter"));
    assert_eq!(again.up_axis.as_deref(), Some("+Y"));
    let extras = again
        .extras
        .clone()
        .expect("the reader declares its format");
    assert_eq!(extras["format"], "glTF");
    assert!(again.source.is_some());

    // The same scene the JT read produced: one mesh, twelve triangles, one material.
    let jt = ferrojt::open(&source).expect("the sample must open");
    assert_eq!(again.nodes.len(), jt.nodes.len(), "nodes");
    assert_eq!(again.meshes.len(), jt.meshes.len(), "meshes");
    assert_eq!(again.triangle_count(), jt.triangle_count(), "triangles");
    assert_eq!(again.materials.len(), jt.materials.len(), "materials");
}

#[test]
fn a_glb_file_round_trips_through_bytes() {
    let source = sample("example_block_jt9.5");
    let (_, _, glb, _) = through_glb(&source);
    let model = ferrojt::import::from_bytes(&glb, None).expect("the GLB must read from memory");
    assert_eq!(model.meshes.len(), 1);
    assert_eq!(model.triangle_count(), 12);

    // The same scene through the text form must agree on the counts.
    let text = ferrojt::export::to_format(Format::Gltf, &model).expect("gltf must export");
    let path = temp_path("inline", "gltf");
    std::fs::write(&path, &text).expect("writing the .gltf must work");
    let again = ferrojt::open(&path).expect("the .gltf must read back");
    let _ = std::fs::remove_file(&path);
    assert_eq!(again.meshes.len(), 1);
    assert_eq!(again.triangle_count(), 12);
    assert_eq!(again.unit.as_deref(), Some("meter"));
}

#[test]
fn usd_can_be_written_but_not_read() {
    assert!(Format::Usd.is_writable());
    assert!(!Format::Usd.is_readable());
    let err =
        ferrojt::open_format(Format::Usd, "anything.usda").expect_err("USD has no reader here");
    assert!(matches!(err, ferrojt::Error::UnsupportedFormat(_)));
}

// ---------------------------------------------------------------------------
// the whole path
// ---------------------------------------------------------------------------

/// What each sample must come back as after JT -> GLB -> model -> JT.
struct Expected {
    /// Revision the writer lands on when it is not asked for one.
    major: u32,
    minor: u32,
    /// Nodes of the model read back out of the written file.
    nodes: usize,
    meshes: usize,
    triangles: usize,
    /// Materials of that model, which is the number of distinct shadings.
    materials: usize,
}

/// The expectation for one sample.
fn expectation(path: &Path) -> Expected {
    let stem = name(path);
    let block = || Expected {
        major: 10,
        minor: 3,
        nodes: 16,
        meshes: 1,
        triangles: 12,
        materials: 1,
    };
    match stem.as_str() {
        // The server assembly has thousands of edges the topological coder
        // cannot cross, so it falls back to the revision that can hold its
        // meshes. Its 23 source materials collapse to 20 because the source
        // repeats three colour sets across its 106 shapes.
        _ if stem.contains("Server") => Expected {
            major: 8,
            minor: 1,
            nodes: 2_693,
            meshes: 106,
            triangles: 129_067,
            materials: 20,
        },
        _ if stem.contains("example_block_jt8.1") => block(),
        _ if stem.contains("example_block_jt9.5") => block(),
        _ if stem.contains("example_block_jt10.3") => Expected {
            nodes: 17,
            ..block()
        },
        // One screw of the older plate is not a closed two-manifold, so that
        // file falls back too — the fallback is per file, not per revision.
        _ if stem.contains("plate1_jt8.0") => Expected {
            major: 8,
            minor: 1,
            nodes: 32,
            meshes: 2,
            triangles: 617,
            materials: 2,
        },
        _ if stem.contains("plate1_jt9.5") => Expected {
            nodes: 28,
            meshes: 2,
            triangles: 486,
            materials: 2,
            ..block()
        },
        other => panic!("no expectation for {other}"),
    }
}

#[test]
fn every_sample_goes_from_glb_to_jt() {
    for source in samples() {
        let label = name(&source);
        let want = expectation(&source);
        let (file, _, _, again) = through_glb(&source);

        let bytes = jt::model_to_bytes(&again, None)
            .unwrap_or_else(|err| panic!("{label}: writing failed: {err}"));
        assert!(!bytes.is_empty(), "{label}: nothing was written");

        let path = temp_file(&label, &bytes);
        let header = jt::read_header(&path).unwrap_or_else(|err| panic!("{label}: {err}"));
        assert_eq!(
            (header.major, header.minor),
            (want.major, want.minor),
            "{label}: the wrong revision"
        );
        let out = written(&source, &bytes);
        let _ = std::fs::remove_file(&path);
        let model = out.to_model();

        // Five nodes more than the neutral graph that went in: the root
        // partition the builder always adds, which is where the unit conversion
        // lives, and the meta data, instance, part, level-of-detail span and
        // tier group a consumer walking the graph for a part expects to find.
        assert_eq!(
            model.nodes.len(),
            want.nodes,
            "{label}: unexpected node count"
        );
        assert_eq!(
            out.lsg.as_ref().expect("a scene graph").nodes.len(),
            want.nodes,
            "{label}: the scene graph and the model must agree on the node count"
        );
        assert_eq!(model.meshes.len(), want.meshes, "{label}: meshes");
        assert_eq!(model.triangle_count(), want.triangles, "{label}: triangles");
        assert_eq!(model.materials.len(), want.materials, "{label}: materials");

        // Geometry: the finest tier of the source, as a set of welded triangles.
        assert_eq!(
            model_triangles(&model),
            finest_triangles(&file),
            "{label}: the triangles changed"
        );
    }
}

#[test]
fn the_unit_conversion_lives_on_the_root_not_in_the_vertices() {
    let source = sample("example_block_jt9.5");
    let (file, model, _, again) = through_glb(&source);
    let bytes = jt::model_to_bytes(&again, None).expect("writing must work");
    let out = written(&source, &bytes);
    let graph = out.lsg.as_ref().expect("a scene graph");
    let root = graph.root().expect("a root node");

    // glTF is metres and +Y up, JT is millimetres and +Z up, so the root
    // partition scales by 1000 and turns a quarter turn about X.
    let transform = transform_of(graph, root);
    for (got, want) in transform.scale.iter().zip([1_000.0f32, 1_000.0, 1_000.0]) {
        assert!(
            (got - want).abs() < 1.0,
            "the root must scale metres into millimetres: {transform:?}"
        );
    }
    let diagonal = std::f32::consts::FRAC_1_SQRT_2;
    let [x, y, z, w] = transform.rotation;
    assert!(
        (w.abs() - diagonal).abs() < 1e-5
            && (x.abs() - diagonal).abs() < 1e-5
            && y.abs() < 1e-5
            && z.abs() < 1e-5,
        "the root must turn +Y up onto +Z: {transform:?}"
    );
    assert!(
        transform.translation.iter().all(|value| value.abs() < 1e-3),
        "the root must not move the model: {transform:?}"
    );

    // And the vertices are untouched: the block is still 100 x 80 x 60 in its
    // own units, exactly as the source has it.
    let want = model
        .meshes
        .iter()
        .filter_map(|mesh| mesh.bounds)
        .collect::<Vec<_>>();
    let got = out
        .to_model()
        .meshes
        .iter()
        .filter_map(|mesh| mesh.bounds)
        .collect::<Vec<_>>();
    assert_eq!(got.len(), want.len(), "a mesh lost its bounds");
    for (got, want) in got.iter().zip(&want) {
        for axis in 0..3 {
            assert!(
                (got.min[axis] - want.min[axis]).abs() < 1e-3
                    && (got.max[axis] - want.max[axis]).abs() < 1e-3,
                "the vertices must not be rescaled: {got:?} against {want:?}"
            );
        }
    }
    assert_eq!(
        file.to_model().triangle_count(),
        out.to_model().triangle_count(),
        "the triangle count must not change"
    );
}

#[test]
fn a_converted_file_names_its_root_partition() {
    // A consumer that builds a scene graph of its own creates the root node
    // from the file name of the root partition, so a converted file has to name
    // one. Bytes have no destination and take the name of the file the model
    // was read from; a written file knows its own name and takes that.
    let source = sample("example_block_jt9.5");
    let (_, from_jt, _, again) = through_glb(&source);

    let bytes = jt::model_to_bytes(&from_jt, None).expect("writing must work");
    let out = written(&source, &bytes);
    let Node::Partition(partition) = out
        .lsg
        .as_ref()
        .expect("a scene graph")
        .root()
        .expect("a root node")
    else {
        panic!("the root of a written file is a partition");
    };
    // The sample name has a dot inside its stem — `example_block_jt9.5` — so
    // the extension has to be read off the last dot.
    assert_eq!(partition.file_name, "example_block_jt9.5.jt");
    assert_eq!(partition.name, "example_block_jt9.5.jt");

    let path = temp_path("rooted", "jt");
    jt::model_to_file(&again, &path, None).expect("writing must work");
    let file = JtFile::open(&path).expect("the file on disk parses");
    let _ = std::fs::remove_file(&path);
    let Node::Partition(partition) = file
        .lsg
        .as_ref()
        .expect("a scene graph")
        .root()
        .expect("a root node")
    else {
        panic!("the root of a written file is a partition");
    };
    assert_eq!(
        partition.file_name,
        path.file_stem().expect("a stem").to_string_lossy() + ".jt",
        "a file on disk names its root after itself"
    );
}

#[test]
fn a_converted_file_wraps_its_graph_in_the_classes_a_consumer_walks() {
    // A consumer that builds a scene graph of its own walks from the root
    // partition down to a part and reads the geometry under the level-of-detail
    // span of that part. Shapes straight under the partition — which is what an
    // unwrapped conversion used to write — leave that walk with nothing. So the
    // writer mirrors the chain of the samples:
    //
    //   Partition -> MetaData -> Instance -> Part -> RangeLOD -> Group -> Shape
    //
    // with the instance *referring* to the part rather than containing it, and
    // exactly one tier in the span, because a neutral model has one level of
    // detail.
    let source = sample("example_block_jt9.5");
    let (file, _, _, again) = through_glb(&source);
    let bytes = jt::model_to_bytes(&again, None).expect("writing must work");
    let out = written(&source, &bytes);
    let graph = out.lsg.as_ref().expect("a scene graph");

    // The chain, walked from the root down to the tier group. An instance does
    // not hold its target as a child, so the walk follows the reference
    // instead. Below the tier group the graph of the model continues, and that
    // part is not part of the wrapper.
    let mut path: Vec<String> = Vec::new();
    let mut pos = graph.root.expect("a root node");
    let mut span = None;
    let mut part = None;
    for _ in 0..6 {
        let node = graph.node_at(pos).expect("a node");
        let class = node.class_name();
        path.push(class.to_string());
        if class == "RangeLOD" {
            span = Some(pos);
        }
        if class == "Part" {
            part = Some(pos);
        }
        let next = if class == "Instance" {
            graph.instance_target(node).expect("an instance target")
        } else {
            let mut children = graph.children_of(node).into_iter().flatten();
            let Some(next) = children.next() else { break };
            assert!(
                children.next().is_none(),
                "the wrapper chain must not branch: {class} has more than one child"
            );
            next
        };
        pos = next;
    }
    assert_eq!(
        path,
        [
            "Partition",
            "MetaData",
            "Instance",
            "Part",
            "RangeLOD",
            "Group"
        ],
        "the classes from the root down to the tier group"
    );

    // The instance refers to the part instead of holding it as a child.
    let instance = graph
        .node_at(2)
        .and_then(|node| graph.instance_target(node))
        .expect("the instance must name its target");
    assert_eq!(
        Some(instance),
        part,
        "the instance of the wrapper refers to the part"
    );

    // The span holds exactly one tier group — the finest, and the only one a
    // neutral model has — and geometry hangs under it.
    let span = graph
        .node_at(span.expect("a level-of-detail span"))
        .expect("the span");
    let tiers = graph.children_of(span);
    assert_eq!(tiers.len(), 1, "the span holds one tier");
    assert_eq!(
        graph
            .node_at(tiers[0].expect("the tier group"))
            .expect("the tier")
            .class_name(),
        "Group",
        "a tier of a range span is a group"
    );
    assert!(
        !finest_triangles(&out).is_empty(),
        "the geometry must hang under the tier group"
    );
    assert!(
        classes(&out).iter().any(|class| class == "Shape"),
        "a shape node must sit under the tier group"
    );

    // The same triangles the source's finest tier holds, so the chain did not
    // move the geometry out of reach of the LOD walk.
    assert_eq!(
        finest_triangles(&out),
        finest_triangles(&file),
        "walking the span of the written file finds the geometry of the source"
    );
}

#[test]
fn a_converted_file_dumps_as_valid_json() {
    // The wrapper chain ends in a tier group whose children are the shapes of
    // the model, and a shape is not part of the dump. So the group is written
    // with no children at all, and every member the dump writer emits before it
    // must close without a separator — a dangling comma makes the whole dump
    // unparseable. None of the samples hits that case, so it is pinned here.
    let source = sample("example_block_jt9.5");
    let (_, _, _, again) = through_glb(&source);
    for version in [JtVersion::V8_1, JtVersion::V10_3] {
        let bytes = jt::model_to_bytes(&again, Some(version))
            .unwrap_or_else(|err| panic!("JT {version}: writing failed: {err}"));
        let out = written(&source, &bytes);
        for config in [
            ferrojt::jt::DUMP_TOC,
            ferrojt::jt::DUMP_TOC | ferrojt::jt::DUMP_MESH,
            ferrojt::jt::DUMP_ALL,
        ] {
            let text = jt::dump(&out, config, 1)
                .unwrap_or_else(|err| panic!("JT {version}, config {config}: {err}"));
            serde_json::from_str::<serde_json::Value>(&text).unwrap_or_else(|err| {
                panic!("JT {version}, config {config}: the dump of a written file: {err}")
            });
        }
    }
}

#[test]
fn a_requested_revision_is_obeyed() {
    let source = sample("example_block_jt9.5");
    let (_, _, _, again) = through_glb(&source);
    for version in [JtVersion::V8_1, JtVersion::V9_5, JtVersion::V10_3] {
        let bytes = jt::model_to_bytes(&again, Some(version))
            .unwrap_or_else(|err| panic!("JT {version}: writing failed: {err}"));
        let path = temp_file("revision", &bytes);
        let header = jt::read_header(&path).expect("the banner must read");
        let _ = std::fs::remove_file(&path);
        assert_eq!((header.major, header.minor), (version.major, version.minor));
        let out = written(&source, &bytes);
        assert_eq!(
            out.to_model().triangle_count(),
            12,
            "JT {version}: triangles"
        );
    }
}

#[test]
fn an_uncodable_mesh_falls_back_to_the_revision_that_holds_it() {
    let source = sample("SampleJT_Server");
    let (_, _, _, again) = through_glb(&source);

    // Asked for 10.x it refuses: the server assembly has thousands of edges
    // shared by more than two triangles, so none of its meshes can be coded
    // topologically.
    let err = jt::model_to_bytes(&again, Some(JtVersion::V10_3))
        .expect_err("the server sample is not topologically encodable");
    let ferrojt::Error::Unsupported(message) = err else {
        panic!("expected an unsupported error, got {err:?}");
    };
    assert!(
        message.contains("two-manifold") && message.contains("JT 8.x"),
        "unexpected message: {message}"
    );

    // Unasked, the whole file drops to JT 8.x, whose vertex based
    // representation holds any triangle soup. One revision per file, so a
    // single uncodable mesh moves every shape of the model.
    let bytes = jt::model_to_bytes(&again, None).expect("the fallback must write");
    let path = temp_file("fallback", &bytes);
    let header = jt::read_header(&path).expect("the banner must read");
    let _ = std::fs::remove_file(&path);
    assert_eq!((header.major, header.minor), (8, 1));
    let out = written(&source, &bytes);
    assert_eq!(out.to_model().triangle_count(), 129_067);
}

// ---------------------------------------------------------------------------
// refusals and the identity case
// ---------------------------------------------------------------------------

#[test]
fn a_model_without_triangles_is_refused() {
    // A mesh with positions but no triangle at all cannot become a JT shape.
    let mut model = Model::new();
    model.unit = Some("millimeter".to_string());
    model.up_axis = Some("+Z".to_string());
    let part = model.push_node(model::Node::named("part"));
    model.nodes[part].kind = Some("Part".to_string());
    let shape = model.push_node(model::Node::named("shape"));
    model.nodes[shape].kind = Some("Shape".to_string());
    model.nodes[part].children.push(shape);
    model.default_scene_mut().roots.push(part);

    let mesh = model::Mesh {
        name: Some("no triangles".to_string()),
        primitives: vec![model::Primitive {
            attributes: vec![position_attribute(&mut model, &[0.0, 0.0, 0.0])],
            indices: None,
            topology: Topology::TriangleList,
            material: None,
            bounds: None,
            extras: None,
        }],
        bounds: None,
        lod_level: None,
        extras: None,
    };
    let mesh_at = model.push_mesh(mesh);
    model.nodes[shape].instances.push(model::Instance {
        mesh: mesh_at,
        material: None,
        lod_bias: 0.0,
    });

    let err = jt::model_to_bytes(&model, None).expect_err("there is nothing to write");
    assert!(
        matches!(&err, ferrojt::Error::Invalid { message, .. } if message.contains("must come in triangles")),
        "unexpected error: {err:?}"
    );
}

/// Append one POSITION attribute of `points` and return its handle.
fn position_attribute(model: &mut Model, points: &[f32]) -> usize {
    let buffer = model.accessors.push_buffer(
        points
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect(),
    );
    model.accessors.push_attribute(model::Attribute {
        name: "POSITION".to_string(),
        buffer,
        offset: 0,
        count: points.len() / 3,
        components: 3,
        component_type: ComponentType::F32,
        normalized: false,
        min: None,
        max: None,
    })
}

#[test]
fn an_unknown_unit_or_up_axis_is_refused() {
    let source = sample("example_block_jt9.5");
    let (_, _, _, mut again) = through_glb(&source);

    again.unit = Some("furlong".to_string());
    let err = jt::model_to_bytes(&again, None).expect_err("an unknown unit cannot be converted");
    assert!(
        matches!(&err, ferrojt::Error::Unsupported(message) if message.contains("furlong")),
        "unexpected error: {err:?}"
    );

    again.unit = Some("meter".to_string());
    again.up_axis = Some("+Q".to_string());
    let err = jt::model_to_bytes(&again, None).expect_err("an unknown axis cannot be rotated");
    // The axis is normalised before it is reported, so the sign is gone.
    assert!(
        matches!(&err, ferrojt::Error::Unsupported(message) if message.contains("is not an up axis")),
        "unexpected error: {err:?}"
    );
}

#[test]
fn a_non_triangle_primitive_is_refused() {
    let source = sample("example_block_jt9.5");
    let (_, _, _, mut again) = through_glb(&source);
    again.meshes[0].primitives[0].topology = Topology::LineList;
    let err = jt::model_to_bytes(&again, None).expect_err("lines have no JT shape type");
    assert!(
        matches!(&err, ferrojt::Error::Unsupported(message) if message.contains("no JT shape type")),
        "unexpected error: {err:?}"
    );
}

#[test]
fn a_model_read_from_jt_needs_no_conversion() {
    // Reading a JT file gives a model that already declares millimetres and
    // +Z, so writing it back must not insert a scaling root at all. JT itself
    // stores neither, so that declaration is the convention of the format.
    let source = sample("example_block_jt9.5");
    let data = std::fs::read(&source).expect("the sample must be readable");
    let file = JtFile::read(&source, &data).expect("the sample must parse");
    let model = ferrojt::open(&source).expect("the sample must open");
    assert_eq!(model.unit.as_deref(), Some("millimeter"));
    assert_eq!(model.up_axis.as_deref(), Some("+Z"));

    let bytes = jt::model_to_bytes(&model, None).expect("writing must work");
    let out = written(&source, &bytes);
    let graph = out.lsg.as_ref().expect("a scene graph");
    let transform = transform_of(graph, graph.root().expect("a root"));
    assert!(
        transform.is_identity(),
        "a millimetre model must not be rescaled: {transform:?}"
    );
    let again = out.to_model();
    // Exactly as many nodes as the source model: the partition of the model
    // **becomes** the root partition of the file rather than hanging under a
    // second one.
    assert_eq!(again.nodes.len(), model.nodes.len(), "nodes");
    assert_eq!(again.meshes.len(), model.meshes.len(), "meshes");
    assert_eq!(again.triangle_count(), model.triangle_count(), "triangles");
    // A consumer reaches a part by descending a fixed handful of classes from
    // the root, so a root partition holding a second partition leaves the
    // geometry out of its reach: the file parses, reports a root, and then
    // never produces a frame. The partition of the model is therefore merged
    // with the root of the writer, and the part stays where the sample has it.
    assert_eq!(
        class_counts(&out).get("Partition").copied(),
        Some(1),
        "a written file owns exactly one root partition"
    );
    assert_eq!(
        depth_of_class(&out, "Part"),
        depth_of_class(&file, "Part"),
        "the part of the written file sits as close to the root as in the source"
    );
    assert_eq!(
        finest_triangles(&out),
        finest_triangles(&file),
        "walking down from the root of the written file finds the geometry of the source"
    );
}

#[test]
fn a_written_file_never_stacks_a_partition_under_its_own_root() {
    // The blocker an external renderer showed: a file that parsed, reported a
    // root and produced no frame at all. The writer owned a root partition and
    // then re-emitted the partition of the model as a child of it, so the part
    // sat three levels down. A consumer reaches a part by descending a fixed
    // handful of classes from the root, and a walk that runs out of classes
    // before it reaches one has nothing to build a shape from.
    //
    // So the partition of a model is merged with the root of the writer, and the
    // depth of the part in a written file is pinned against the file the model
    // came from.
    for source in samples() {
        let label = name(&source);
        let data = std::fs::read(&source).unwrap_or_else(|err| panic!("{label}: {err}"));
        let file = JtFile::read(&source, &data).unwrap_or_else(|err| panic!("{label}: {err}"));
        let model = file.to_model();

        let bytes = jt::model_to_bytes(&model, None)
            .unwrap_or_else(|err| panic!("{label}: writing failed: {err}"));
        let out = written(&source, &bytes);

        assert_eq!(
            class_counts(&out).get("Partition").copied(),
            Some(1),
            "{label}: a written file owns exactly one root partition\n{}",
            classes(&out).join(" ")
        );
        let want = depth_of_class(&file, "Part");
        let got = depth_of_class(&out, "Part");
        assert!(
            got.is_some_and(|depth| want.is_some_and(|want| depth <= want)),
            "{label}: the part must be no deeper than in the source: {got:?} against {want:?}"
        );
        assert_eq!(
            class_counts(&out).get("Partition").copied(),
            Some(1),
            "{label}: a written file owns exactly one root partition\n{}",
            classes(&out).join(" ")
        );
        assert_eq!(
            finest_triangles(&out),
            finest_triangles(&file),
            "{label}: walking down from the root must find the geometry of the source"
        );
    }
}

#[test]
fn a_level_of_detail_span_keeps_its_class_through_the_model_path() {
    // The reader declares a range span with the class `RangeLOD`, and a model
    // read out of a JT file carries that name in its node kinds. A writer that
    // mapped it to a plain group would still produce a file this library reads
    // back — but the span is what tells a consumer where the levels of detail
    // begin, so dropping it loses the LOD structure of the file.
    let source = sample("example_block_jt9.5");
    let data = std::fs::read(&source).expect("the sample must be readable");
    let file = JtFile::read(&source, &data).expect("the sample must parse");
    let spans = class_counts(&file)
        .get("RangeLOD")
        .copied()
        .unwrap_or_default();
    assert!(spans > 0, "the sample must hold a level-of-detail span");

    let bytes = jt::model_to_bytes(&file.to_model(), None).expect("writing must work");
    let out = written(&source, &bytes);
    assert_eq!(
        class_counts(&out).get("RangeLOD").copied(),
        Some(spans),
        "every span of the source must come back as a span"
    );
}

#[test]
fn a_shape_node_without_geometry_becomes_a_group() {
    // A JT shape whose representation is empty comes back as a node of class
    // `Shape` carrying no instance. JT cannot express that, so the writer
    // demotes the node to a group and keeps its subtree rather than dropping
    // it — the same policy the reader and the exporters apply.
    let mut model = Model::new();
    model.unit = Some("millimeter".to_string());
    model.up_axis = Some("+Z".to_string());
    let part = model.push_node(model::Node::named("part"));
    model.nodes[part].kind = Some("Part".to_string());
    let shapeless = model.push_node(model::Node::named("shape without geometry"));
    model.nodes[shapeless].kind = Some("Shape".to_string());
    model.nodes[part].children.push(shapeless);
    model.default_scene_mut().roots.push(part);

    let bytes = jt::model_to_bytes(&model, None).expect("the graph must still write");
    let out = written(&sample("example_block_jt9.5"), &bytes);
    assert_eq!(classes(&out), vec!["Group", "Part", "Partition"]);
}

#[test]
fn a_shape_with_geometry_writes_a_shape_node() {
    // The other half of the same rule: a node that carries geometry and has no
    // children becomes the shape itself.
    let mut model = Model::new();
    model.unit = Some("millimeter".to_string());
    model.up_axis = Some("+Z".to_string());
    let part = model.push_node(model::Node::named("part"));
    model.nodes[part].kind = Some("Part".to_string());
    let shape = model.push_node(model::Node::named("cube"));
    model.nodes[shape].kind = Some("Shape".to_string());
    model.nodes[part].children.push(shape);
    model.default_scene_mut().roots.push(part);

    let mesh = cube(&mut model);
    let mesh_at = model.push_mesh(mesh);
    model.nodes[shape].instances.push(model::Instance {
        mesh: mesh_at,
        material: None,
        lod_bias: 0.0,
    });

    let bytes = jt::model_to_bytes(&model, None).expect("the cube must write");
    let out = written(&sample("example_block_jt9.5"), &bytes);
    assert_eq!(classes(&out), vec!["Part", "Partition", "Shape"]);
    let again = out.to_model();
    assert_eq!(again.meshes.len(), 1);
    assert_eq!(again.triangle_count(), 12);
    assert_eq!(
        model_triangles(&again),
        model_triangles(&model),
        "the cube must come back unchanged"
    );
}

/// A closed unit cube: twelve triangles, twenty-four positions. Closed is what
/// JT 9.x and 10.x need, so this fixture writes at the newest revision. The
/// attributes go onto `model` because that is where a mesh points at them.
fn cube(model: &mut Model) -> model::Mesh {
    let corners: [[f32; 3]; 24] = [
        // -Z
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        // +Z
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [0.0, 1.0, 1.0],
        // -X
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0],
        [0.0, 1.0, 0.0],
        // +X
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [1.0, 1.0, 0.0],
        // -Y
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        // +Y
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 1.0],
        [1.0, 1.0, 1.0],
        [1.0, 1.0, 0.0],
    ];
    let faces: [[u32; 4]; 6] = [
        [0, 1, 2, 3],
        [5, 4, 6, 7],
        [8, 9, 10, 11],
        [13, 12, 14, 15],
        [16, 17, 18, 19],
        [21, 20, 22, 23],
    ];

    let points: Vec<f32> = corners.concat();
    let position = position_attribute(model, &points);

    let indices: Vec<u32> = faces
        .iter()
        .flat_map(|face| [face[0], face[1], face[2], face[0], face[2], face[3]])
        .collect();
    let buffer = model
        .accessors
        .push_buffer(indices.iter().flat_map(|v| v.to_le_bytes()).collect());
    let index = model.accessors.push_attribute(model::Attribute {
        name: "indices".to_string(),
        buffer,
        offset: 0,
        count: indices.len(),
        components: 1,
        component_type: ComponentType::U32,
        normalized: false,
        min: None,
        max: None,
    });

    model::Mesh {
        name: Some("cube".to_string()),
        primitives: vec![model::Primitive {
            attributes: vec![position],
            indices: Some(index),
            topology: Topology::TriangleList,
            material: None,
            bounds: None,
            extras: None,
        }],
        bounds: Some(model::Aabb::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0])),
        lod_level: None,
        extras: None,
    }
}

#[test]
fn a_shared_subtree_is_instantiated_not_copied() {
    // The plate samples hold one screw and instantiate it twice. A node
    // reachable from several parents must come back as one node plus an
    // instance per visit, so the mesh stays one representation in the file.
    //
    // The glTF hop does not carry the JT `Instance` class, so the writer
    // re-derives instancing from the graph shape: N parents give N - 1
    // instances, because the first visit builds the node and only the later
    // ones need a share. The source's two instances therefore become one.
    let source = sample("plate1_jt9.5");
    let (file, _, _, again) = through_glb(&source);
    assert_eq!(
        instance_count(&file),
        2,
        "the plate sample instantiates its screw twice"
    );

    let bytes = jt::model_to_bytes(&again, None).expect("writing must work");
    let out = written(&source, &bytes);
    assert_eq!(
        instance_count(&out),
        2,
        "the shared screw must be instanced, next to the instance of the wrapper part"
    );
    // One representation, not two: the share survived rather than a copy.
    assert_eq!(
        out.to_model().meshes.len(),
        2,
        "the screw must not be copied"
    );
}

#[test]
fn an_instance_of_a_sample_keeps_its_placement_and_its_detached_target() {
    // The plate samples hold one screw and instantiate it twice, at `y = -25`
    // and `y = +25`. Two rules decide whether that comes out as two screws or
    // as one:
    //
    // - The placement lives on the **instance**, not on the node it refers to.
    //   A file stores the same part at two positions by giving each instance its
    //   own `GeometricTransform`; the neutral graph keeps it the same way. An
    //   instance written without its transform collapses every copy onto one
    //   place.
    // - The target is reached **only** through the reference. Hanging it under
    //   the root as well — which is what a writer that treats it as an ordinary
    //   child does — draws the subtree a third time, at the identity.
    //
    // This goes straight from a JT file through the neutral model to a written
    // JT file, without a glTF hop, because the hop erases the JT `Instance`
    // class and the writer would then re-derive instancing from the graph shape.
    let source = sample("plate1_jt9.5");
    let data = std::fs::read(&source).expect("the sample must be readable");
    let file = JtFile::read(&source, &data).expect("the sample must parse");
    let source_instances = placements(&file);
    assert_eq!(
        source_instances.len(),
        2,
        "the plate sample instantiates its screw twice"
    );

    let bytes = jt::model_to_bytes(&file.to_model(), None).expect("writing must work");
    let out = written(&source, &bytes);
    let got = placements(&out);
    assert_eq!(
        got, source_instances,
        "every instance of the source must come back with its own placement and name"
    );

    // The subtree an instance refers to is not a child of anything: the
    // reference is the only way to it, exactly as in the source.
    for label in ["source", "written"] {
        let file = if label == "source" { &file } else { &out };
        let graph = file.lsg.as_ref().expect("a scene graph");
        let referenced: Vec<usize> = graph
            .nodes
            .iter()
            .filter_map(|stored| graph.instance_target(&stored.node))
            .collect();
        assert_eq!(
            referenced.len(),
            2,
            "{label}: the two instances must name a target"
        );
        assert_eq!(
            referenced[0], referenced[1],
            "{label}: both instances must share one target rather than copy it"
        );
        let target = referenced[0];
        let as_child = graph
            .nodes
            .iter()
            .filter(|stored| {
                graph
                    .children_of(&stored.node)
                    .into_iter()
                    .flatten()
                    .any(|child| child == target)
            })
            .count();
        assert_eq!(
            as_child, 0,
            "{label}: the target of an instance must not also hang as a child — \
             that draws the shared subtree a third time, at the identity"
        );
    }

    // And the geometry of both instances is still reachable from the root.
    assert_eq!(
        finest_triangles(&out),
        finest_triangles(&file),
        "walking down from the root must find the geometry of the source"
    );
}

/// Where the instances of a file sit: their translation and their name, sorted
/// so that the writer's own node order does not matter.
fn placements(file: &JtFile) -> Vec<([i64; 3], String)> {
    let graph = file.lsg.as_ref().expect("a scene graph");
    let mut out: Vec<([i64; 3], String)> = graph
        .nodes
        .iter()
        .filter(|stored| matches!(stored.node, Node::Instance(_)))
        .map(|stored| {
            let transform = transform_of(graph, &stored.node);
            let key = [
                (f64::from(transform.translation[0]) * 1_000.0).round() as i64,
                (f64::from(transform.translation[1]) * 1_000.0).round() as i64,
                (f64::from(transform.translation[2]) * 1_000.0).round() as i64,
            ];
            (key, stored.node.name().to_string())
        })
        .collect();
    out.sort();
    out
}

#[test]
fn the_neutral_graph_of_a_sample_survives_the_hop() {
    // A written file states the neutral graph, not the JT graph it came from:
    // the glTF hop erases every JT node class, so the writer re-derives the
    // classes from the shape of the graph. The invariant is exactly one root
    // partition, the wrapper chain a consumer walks down to a part, one shape
    // per mesh, and a group for every other node — the parts, meta data nodes
    // and LOD containers of the source all collapse to groups.
    for source in samples() {
        let label = name(&source);
        let (file, _, _, again) = through_glb(&source);
        let bytes = jt::model_to_bytes(&again, None)
            .unwrap_or_else(|err| panic!("{label}: writing failed: {err}"));
        let out = written(&source, &bytes);

        let got = class_counts(&out);
        let of = |class: &str| got.get(class).copied().unwrap_or_default();

        assert_eq!(of("Partition"), 1, "{label}: exactly one root partition");
        assert_eq!(
            of("Shape"),
            again.meshes.len(),
            "{label}: one shape node per mesh\n{}",
            classes(&out).join(" ")
        );
        assert_eq!(
            of("Group"),
            again.nodes.len() - again.meshes.len() + 1,
            "{label}: every other node becomes a group, plus the tier group of the wrapper\n{}",
            classes(&out).join(" ")
        );
        // A neutral graph has none of the classes a CAD writer puts between its
        // root partition and its shapes, so the hop adds exactly one of each.
        for class in ["MetaData", "Part", "RangeLOD"] {
            assert_eq!(
                of(class),
                1,
                "{label}: the graph gains exactly one {class} node\n{}",
                classes(&out).join(" ")
            );
        }
        // Nothing else is ever emitted, and no node of the neutral graph is
        // lost on the way.
        let total: usize = got.values().sum();
        assert_eq!(
            total,
            out.lsg.as_ref().expect("a scene graph").nodes.len(),
            "{label}: the class multiset must cover the whole graph"
        );
        assert_eq!(
            total,
            again.nodes.len() + 5 + of("Instance"),
            "{label}: the root partition, the five wrapper nodes, the graph of the \
             model and one instance node per share"
        );
        assert!(
            of("Instance") <= again.nodes.len() + 1,
            "{label}: an instance per shared visit at most, plus the wrapper one"
        );
        // The source graph is richer: it names the classes the hop erases.
        assert!(
            !class_counts(&file).contains_key("Group")
                || of("Group")
                    != class_counts(&file)
                        .get("Group")
                        .copied()
                        .unwrap_or_default(),
            "{label}: the source classes are not the written ones"
        );
    }
}

#[test]
fn the_materials_of_a_sample_survive_the_hop() {
    // A JT file repeats one colour set on every shape of a part; the model
    // collapses those to distinct materials. The builder writes one attribute
    // per shape node, so the written file repeats a shading as often as the
    // source did — what has to survive is the set of distinct shadings.
    for source in samples() {
        let label = name(&source);
        let (file, _, _, again) = through_glb(&source);
        let bytes = jt::model_to_bytes(&again, None)
            .unwrap_or_else(|err| panic!("{label}: writing failed: {err}"));
        let out = written(&source, &bytes);

        let source_shadings: BTreeSet<String> = shadings(&file).into_iter().collect();
        let all = shadings(&out);
        let got: BTreeSet<String> = all.iter().cloned().collect();
        assert!(
            !got.is_empty(),
            "{label}: the written file carries no material at all"
        );
        for shading in &got {
            assert!(
                source_shadings.contains(shading),
                "{label}: a shading appeared that the source never had: {shading}"
            );
        }
        // The builder attaches one appearance to each shape node, which is how
        // JT stores it — the model's distinct materials are the distinct values
        // of that stream, never more than the model declares.
        let shapes = class_counts(&out).get("Shape").copied().unwrap_or_default();
        assert_eq!(
            all.len(),
            shapes,
            "{label}: one material attribute per shape node"
        );
        assert!(
            got.len() <= again.materials.len(),
            "{label}: {} distinct shadings for a model that declares {} materials",
            got.len(),
            again.materials.len()
        );
    }
}
