//! Flattening a scene merges every mesh of a file into one: node transforms are
//! baked into the coordinates and an instanced part is copied to every place it
//! appears. The counts below are the measured result for the reference samples;
//! an independent walk over the model graph re-checks the coordinates, because a
//! transform read the wrong way round still produces a picture.

use std::path::{Path, PathBuf};

use ferrojt::model::{ComponentType, Format, Model, Topology};

/// What flattening a sample produces.
struct Expectation {
    /// Vertices of the merged mesh: the slots of every mesh copy, so an
    /// instanced part contributes its slots once per placement.
    vertices: usize,
    /// Triangles of the merged mesh: the placement-expanded count, which is
    /// larger than [`Model::triangle_count`] whenever a part is instanced.
    triangles: usize,
    /// Runs of the merged mesh that share one material.
    groups: usize,
}

const EXPECTED: &[(&str, Expectation)] = &[
    (
        "example_block_jt10.3.jt",
        Expectation {
            vertices: 24,
            triangles: 12,
            groups: 1,
        },
    ),
    (
        "example_block_jt9.5.jt",
        Expectation {
            vertices: 24,
            triangles: 12,
            groups: 1,
        },
    ),
    (
        "example_block_jt8.1.jt",
        Expectation {
            vertices: 24,
            triangles: 12,
            groups: 1,
        },
    ),
    // One screw instantiated twice: both placements end up in the merged mesh,
    // which is why the count is larger than the 617 triangles of the model.
    (
        "opening_protection_plate1_jt8.0.jt",
        Expectation {
            vertices: 1084,
            triangles: 1012,
            groups: 2,
        },
    ),
    (
        "opening_protection_plate1_jt9.5.jt",
        Expectation {
            vertices: 923,
            triangles: 800,
            groups: 2,
        },
    ),
    // 2158 nodes and 106 representations, instanced into 592 material runs.
    (
        "SampleJT_Server.jt",
        Expectation {
            vertices: 1_145_208,
            triangles: 1_193_657,
            groups: 592,
        },
    ),
];

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data")
        .join("ExampleFiles")
        .join(name)
}

fn model(name: &str) -> Model {
    ferrojt::jt::JtFile::open(sample(name))
        .unwrap_or_else(|err| panic!("{name}: {err}"))
        .to_model()
}

// ---------------------------------------------------------------------------
// An independent walk over the scene graph, as the oracle for the merged arrays.
// ---------------------------------------------------------------------------

/// A 3D affine map in `f64`: the same convention as the flattener, reached by a
/// separate implementation so a mistake in one cannot cancel out in the other.
#[derive(Clone, Copy)]
struct Map {
    linear: [[f64; 3]; 3],
    offset: [f64; 3],
}

const IDENTITY: Map = Map {
    linear: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    offset: [0.0; 3],
};

fn multiply(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, value) in out_row.iter_mut().enumerate() {
            *value = (0..3).map(|step| a[row][step] * b[step][column]).sum();
        }
    }
    out
}

fn apply(map: Map, v: [f64; 3]) -> [f64; 3] {
    let mut out = map.offset;
    for (row, value) in out.iter_mut().enumerate() {
        *value += map.linear[row][0] * v[0] + map.linear[row][1] * v[1] + map.linear[row][2] * v[2];
    }
    out
}

/// `parent` followed by a node's own transform: rotation first, then the scale
/// on the columns, and the parent's linear part scales the child's translation.
fn then(parent: Map, node: &ferrojt::model::Transform) -> Map {
    let [x, y, z, w] = node.rotation.map(f64::from);
    let [sx, sy, sz] = node.scale.map(f64::from);
    let turned = [
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
    let scaled = [
        [turned[0][0] * sx, turned[0][1] * sy, turned[0][2] * sz],
        [turned[1][0] * sx, turned[1][1] * sy, turned[1][2] * sz],
        [turned[2][0] * sx, turned[2][1] * sy, turned[2][2] * sz],
    ];
    let linear = multiply(parent.linear, scaled);
    let moved = apply(
        Map {
            linear: parent.linear,
            offset: [0.0; 3],
        },
        node.translation.map(f64::from),
    );
    Map {
        linear,
        offset: [
            moved[0] + parent.offset[0],
            moved[1] + parent.offset[1],
            moved[2] + parent.offset[2],
        ],
    }
}

/// What the merged arrays have to contain.
struct Expected {
    vertices: usize,
    triangles: usize,
    sums: [f64; 3],
    mins: [f64; 3],
    maxs: [f64; 3],
}

fn walk(model: &Model, index: usize, parent: Map, path: &mut Vec<usize>, out: &mut Expected) {
    if path.contains(&index) {
        return;
    }
    path.push(index);
    if let Some(node) = model.nodes.get(index) {
        let world = then(parent, &node.transform);
        for instance in &node.instances {
            if let Some(mesh) = model.meshes.get(instance.mesh) {
                copy(model, mesh, world, out);
            }
        }
        for child in &node.children {
            walk(model, *child, world, path, out);
        }
    }
    path.pop();
}

/// The slots and coordinates one copy of one mesh contributes.
fn copy(model: &Model, mesh: &ferrojt::model::Mesh, world: Map, out: &mut Expected) {
    for primitive in &mesh.primitives {
        if primitive.topology != Topology::TriangleList {
            continue;
        }
        let Some(position) = handle_of(model, primitive, "POSITION") else {
            continue;
        };
        let stored = model.accessors.attribute(position).expect("POSITION").count;
        let coordinates = values(model, position);
        let slots: Vec<usize> = match primitive.indices {
            Some(handles) => values(model, handles)
                .iter()
                .map(|value| (*value).max(0.0) as usize)
                .collect(),
            None => (0..stored).collect(),
        };
        // A slot the file never stored counts as its origin, transformed.
        let count = stored.max(slots.iter().copied().max().unwrap_or(0) + 1);
        for slot in 0..count {
            let point = if slot < stored {
                [
                    coordinate(&coordinates, slot, 0),
                    coordinate(&coordinates, slot, 1),
                    coordinate(&coordinates, slot, 2),
                ]
            } else {
                [0.0; 3]
            };
            let where_ = apply(world, point);
            for (axis, value) in where_.iter().enumerate() {
                out.sums[axis] += value;
                out.mins[axis] = out.mins[axis].min(*value);
                out.maxs[axis] = out.maxs[axis].max(*value);
            }
            out.vertices += 1;
        }
        out.triangles += slots.len() / 3;
    }
}

fn coordinate(values: &[f64], row: usize, part: usize) -> f64 {
    values.get(row * 3 + part).copied().unwrap_or(0.0)
}

/// The attribute of a primitive that answers to `name`, by handle.
fn handle_of(model: &Model, primitive: &ferrojt::model::Primitive, name: &str) -> Option<usize> {
    primitive.attributes.iter().copied().find(|handle| {
        model
            .accessors
            .attribute(*handle)
            .is_some_and(|attribute| attribute.name.eq_ignore_ascii_case(name))
    })
}

/// Every component of an attribute as `f64`, little endian, row by row.
fn values(model: &Model, handle: usize) -> Vec<f64> {
    let attribute = model.accessors.attribute(handle).expect("attribute");
    let bytes = model
        .accessors
        .attribute_bytes(handle)
        .expect("attribute bytes");
    let stride = attribute.element_byte_len();
    let item = attribute.component_type.byte_len();
    (0..attribute.count)
        .flat_map(|row| {
            (0..attribute.components).map(move |part| {
                let start = row * stride + usize::from(part) * item;
                let end = start + item;
                if end > bytes.len() {
                    return 0.0;
                }
                let field = &bytes[start..end];
                match attribute.component_type {
                    ComponentType::I8 => field[0] as i8 as f64,
                    ComponentType::U8 => field[0] as f64,
                    ComponentType::I16 => i16::from_le_bytes([field[0], field[1]]) as f64,
                    ComponentType::U16 => u16::from_le_bytes([field[0], field[1]]) as f64,
                    ComponentType::I32 => {
                        i32::from_le_bytes(field.try_into().expect("4 bytes")) as f64
                    }
                    ComponentType::U32 => {
                        u32::from_le_bytes(field.try_into().expect("4 bytes")) as f64
                    }
                    ComponentType::F32 => {
                        f32::from_le_bytes(field.try_into().expect("4 bytes")) as f64
                    }
                    ComponentType::F64 => f64::from_le_bytes(field.try_into().expect("8 bytes")),
                    other => panic!("the samples never store {other:?} components"),
                }
            })
        })
        .collect()
}

fn expectation(model: &Model) -> Expected {
    let mut out = Expected {
        vertices: 0,
        triangles: 0,
        sums: [0.0; 3],
        mins: [f64::MAX; 3],
        maxs: [f64::MIN; 3],
    };
    for root in model
        .scenes
        .iter()
        .flat_map(|scene| scene.roots.iter())
        .copied()
    {
        walk(model, root, IDENTITY, &mut Vec::new(), &mut out);
    }
    out
}

// ---------------------------------------------------------------------------

#[test]
fn every_sample_flattens_into_one_mesh_and_one_node() {
    for (name, expected) in EXPECTED {
        let whole = model(name);
        let merged = whole.flatten();
        assert_eq!(
            merged.vertex_count(),
            expected.vertices,
            "{name}: wrong number of merged vertices"
        );
        assert_eq!(
            merged.triangle_count(),
            expected.triangles,
            "{name}: wrong number of merged triangles"
        );
        assert_eq!(
            merged.groups.len(),
            expected.groups,
            "{name}: wrong number of material runs"
        );
        assert!(!merged.is_empty(), "{name}: the merged mesh is empty");

        let one = whole.flattened();
        assert_eq!(one.meshes.len(), 1, "{name}: not exactly one mesh");
        assert_eq!(one.nodes.len(), 1, "{name}: not exactly one node");
        let roots = one
            .scenes
            .iter()
            .flat_map(|scene| scene.roots.iter())
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(roots, vec![0], "{name}: the merged node is not a root");
        assert_eq!(
            one.triangle_count(),
            expected.triangles,
            "{name}: the merged model lost triangles"
        );
        let flattened = one
            .extras
            .as_ref()
            .and_then(|extras| extras.get("flattened"))
            .and_then(serde_json::Value::as_bool);
        assert_eq!(flattened, Some(true), "{name}: the merge is not recorded");

        // Instances expand, so a merged mesh never holds less than the file.
        assert!(
            expected.triangles >= whole.triangle_count(),
            "{name}: flattening dropped geometry ({} < {})",
            expected.triangles,
            whole.triangle_count()
        );
    }
}

#[test]
fn the_merged_coordinates_match_an_independent_walk() {
    for (name, _) in EXPECTED {
        let whole = model(name);
        let merged = whole.flatten();
        let want = expectation(&whole);

        assert_eq!(
            merged.vertex_count(),
            want.vertices,
            "{name}: the merged mesh has the wrong number of slots"
        );
        assert_eq!(
            merged.triangle_count(),
            want.triangles,
            "{name}: the merged mesh has the wrong number of triangles"
        );

        let mut sums = [0.0f64; 3];
        for point in merged.vertices.chunks_exact(3) {
            for (total, value) in sums.iter_mut().zip(point) {
                *total += f64::from(*value);
            }
        }
        let scale = want
            .sums
            .map(f64::abs)
            .into_iter()
            .fold(1.0, f64::max)
            .max(1.0);
        for (axis, (got, expected)) in sums.iter().zip(want.sums).enumerate() {
            let error = (*got - expected).abs();
            assert!(
                error <= scale * 1e-4,
                "{name}: axis {axis} of the merged mesh is off by {error}, expected {expected}"
            );
        }

        let box_ = merged.bounds.expect("bounds");
        for axis in 0..3 {
            assert!(
                (f64::from(box_.min[axis]) - want.mins[axis]).abs() < 1e-3
                    && (f64::from(box_.max[axis]) - want.maxs[axis]).abs() < 1e-3,
                "{name}: the merged bounds are wrong on axis {axis}: {:?} vs {:?}",
                [box_.min[axis], box_.max[axis]],
                [want.mins[axis], want.maxs[axis]]
            );
        }
    }
}

#[test]
fn the_indices_of_a_merged_mesh_stay_inside_it() {
    for (name, expected) in EXPECTED {
        let merged = model(name).flatten();
        let slots = merged.vertex_count() as u32;
        assert_eq!(
            merged.indices.len(),
            expected.triangles * 3,
            "{name}: wrong index count"
        );
        assert!(
            merged.indices.iter().all(|index| *index < slots),
            "{name}: an index of the merged mesh is out of range"
        );
        for group in &merged.groups {
            assert!(
                group.first_vertex + group.vertex_count <= merged.vertex_count()
                    && group.first_index + group.index_count <= merged.indices.len(),
                "{name}: a material run points outside the mesh"
            );
        }
    }
}

#[test]
fn a_flattened_model_survives_a_gltf_hop() {
    let name = "opening_protection_plate1_jt9.5.jt";
    let flat = model(name).flattened();
    let bytes = ferrojt::export::to_format(Format::Glb, &flat).expect("the glTF export failed");
    let back = ferrojt::import::from_bytes(&bytes, None).expect("the glTF re-read failed");

    assert_eq!(back.meshes.len(), 1, "the glTF hop split the mesh");
    assert_eq!(back.nodes.len(), 1, "the glTF hop changed the node count");
    assert_eq!(
        back.triangle_count(),
        flat.triangle_count(),
        "the glTF hop lost triangles"
    );
    assert_eq!(back.nodes[0].instances.len(), 1, "the mesh is not attached");
    // Both material runs came back as their own primitive.
    assert_eq!(
        back.meshes[0].primitives.len(),
        flat.meshes[0].primitives.len(),
        "a material run did not survive the hop"
    );
}

#[test]
fn a_flattened_model_binds_materials_per_primitive_in_usd() {
    let name = "opening_protection_plate1_jt9.5.jt";
    let flat = model(name).flattened();
    let text = ferrojt::export::UsdExporter
        .to_text(&flat)
        .expect("the USD export failed");

    // One geometry group for the whole file, one Mesh prim per material run.
    assert_eq!(
        text.matches("def Xform \"GEOMETRY_").count(),
        1,
        "a flattened model must hold one geometry group"
    );
    assert_eq!(
        text.matches("point3f[] points").count(),
        flat.meshes[0].primitives.len(),
        "a material run of the merged mesh has no geometry"
    );
    assert_eq!(
        text.matches("def Material \"Material_").count(),
        2,
        "the materials of the merged runs are missing"
    );
    assert_eq!(
        text.matches("rel material:binding = ").count(),
        2,
        "the merged primitives are not bound to their materials"
    );
    assert!(
        !text.contains(": shared subtree\""),
        "a flattened model has nothing to share"
    );
}

#[test]
fn a_flattened_model_can_be_written_as_jt() {
    let name = "opening_protection_plate1_jt9.5.jt";
    let flat = model(name).flattened();
    let path = std::env::temp_dir().join(format!("ferrojt_flatten_{}.jt", std::process::id()));
    ferrojt::jt::model_to_file(&flat, &path, None).expect("the JT writer failed");

    let one = ferrojt::jt::JtFile::open(&path)
        .expect("the written JT file")
        .to_model();
    let _ = std::fs::remove_file(&path);

    // One shape now draws every triangle of the file.
    assert_eq!(one.meshes.len(), 1, "the JT file has more than one shape");
    assert_eq!(
        one.triangle_count(),
        flat.triangle_count(),
        "the JT file lost triangles"
    );
}
