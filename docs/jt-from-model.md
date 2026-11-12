# Model → JT

> **Status: implemented.** `crates/ferrojt/src/jt/from_model.rs` writes a
> neutral [`Model`](../crates/ferrojt/src/model/scene.rs) out as a JT file by
> handing it to [`JtBuilder`](jt-builder.md). That closes the loop: a `.gltf`
> or `.glb` file read by [the glTF reader](gltf-reader.md) can be written as
> `.jt`, and so can a model that was built in memory.
> `crates/ferrojt/tests/gltf.rs` pins the full hop JT → GLB → model → JT for
> all six samples.

## Three ways to produce a JT file

| Path | Needs | Entry point |
| --- | --- | --- |
| Re-serialise a file that was read | a source `JtFile` | [`jt::write_to_bytes`](jt-writer.md) |
| Build a file node by node | nothing | [`JtBuilder`](jt-builder.md) |
| Write a neutral scene graph | a `Model` | `jt::model_to_bytes` / `jt::model_to_file` |

The third is a thin layer over the second: it walks the graph, translates each
node into a builder node, and calls the builder. Nothing is copied from a source
file, so there is no TOC to preserve and no segment to pass through.

`export::to_format(Format::Jt, model)` routes here, and `Format::Jt` is marked
in `Format::is_writable`. `Format::Jt` is *not* writable through the
`Exporter` trait — the trait has no place for the revision to write at, so the
dispatcher special-cases it.

## Which revision gets written

`version: Option<JtVersion>` selects it. `None` means *the newest revision the
meshes of this model can be coded at*:

- JT 9.x and 10.x store a shape in the **dual vertex-face coder**, which needs a
  **closed two-manifold** — every edge shared by exactly two triangles, every
  vertex ring a single cycle.
- JT 8.x stores the **vertex-based lossless** representation, which holds any
  triangle soup at the cost of an exploded vertex table.

So every mesh is probed with `jt::encode::encode` after the graph is assembled.
When one fails and no revision was asked for, the **whole file** drops to
`JtVersion::V8_1` — the choice is per file, because a JT file has one
representation family, not one per shape. Passing `version` explicitly
suppresses the fallback and reports the offending mesh instead:

```text
unsupported: a mesh with 500 triangles is not a closed two-manifold, so it
cannot be written as JT 10.3; write it at JT 8.x or repair the mesh
```

Measured on the samples, after a JT → GLB → JT hop:

| Sample | Revision | Why |
| --- | --- | --- |
| `example_block` 8.1 / 9.5 / 10.3 | 10.3 | a cube codes cleanly |
| `opening_protection_plate1_jt9.5` | 10.3 | both screws code cleanly |
| `opening_protection_plate1_jt8.0` | **8.1** | one screw is not a two-manifold |
| `SampleJT_Server.jt` | **8.1** | thousands of edges the coder cannot cross |

The fallback is not tied to the revision of the source — the 8.0 plate and the
9.5 plate disagree, and the server sample is a 9.5 file that writes as 8.1.

## Units and up axis live on the root

A JT file is millimetres with +Z up. A `Model` declares its frame, so the two
are reconciled **on the root partition transform**, never by touching vertices:

```rust
model.unit = Some("millimeter".to_owned());
model.up_axis = Some("+Z".to_owned());
```

`jt::model.rs::build_model` declares that for every model read out of a JT file,
and `import::gltf` declares `meter` / `+Y` for every model read out of glTF.
Consequences:

- A JT → JT write is an **identity transform** — no scale, no rotation.
- A glTF → JT write puts `scale = 1000` and a quarter turn about +X on the root
  partition (quaternion `[0.70710678, 0, 0, 0.70710678]` in JT's `x y z w`), and
  the mesh bounds stay exactly what they were, e.g. `[0,0,0]..[100,80,60]`.
- A model that declares nothing is assumed to be in the JT frame already.

JT stores neither unit nor up axis in the scene graph — the axis indicator of
the root partition is not part of the element the format defines, and
`object::Partition::read` hardcodes it — so the declaration is the convention
the format prescribes. Every sample confirms the unit by carrying
`JT_PROP_MEASUREMENT_UNITS:: = "millimeters"` in its meta data.

An unknown unit or up axis is refused rather than defaulted:
`the unit "furlong" has no millimetre equivalent …`,
`"Q" is not an up axis …`.

## Node classes

`Node::kind` carries the JT class name of a node that came out of a JT file, so
it maps straight back: `Partition`, `Part`, `MetaData`, `Shape`, `Instance`.
Everything else — the `"Group"` a glTF node gets, and the `LOD`, `RangeLOD` and
unknown classes the builder has no node type for — becomes a `Group`, the one JT
container carrying no data of its own.

That gives the invariant `the_neutral_graph_of_a_sample_survives_the_hop` pins:
a file written from a model is exactly

```text
Partition 1 · Shape = meshes · Group = nodes − meshes · Instance = extra placements
```

Two demotions are deliberate. A `Shape` node whose mesh has no geometry becomes
a `Group` (glTF mandates a `POSITION` attribute, and an empty JT shape is not
worth writing) — the same policy the reader applies to the 36 empty
representations of the server sample. And a node holding several representations
keeps only the first, because the builder has no level-of-detail node; the
reader collects tiers finest-first, so that is the finest one.

## Instancing

`Writer::built` records the builder slot of each model node **before** its
children are walked. A node reached from several parents therefore becomes one
JT node plus an `Instance`, not a copy — and a node whose `kind` is `Instance`
records its *target* as its own slot, so a twin placement points at the same
subtree. That doubles as the cycle guard.

`a_shared_subtree_is_instantiated_not_copied` pins it: the plate samples place
one screw twice, the source has two `Instance` nodes, the written file has one
(the second placement is reached through the shared subtree), and the model read
back still reports two meshes.

## Materials

`material_of` is the inverse of what the JT reader does. `diffuse` is the base
colour and `emission` the emissive factor; the ambient, specular, shininess and
reflectivity that do not fit a glTF-style material went to `Material::extras`
and come back from there. `metallic` and `roughness` have no JT equivalent at
all and are dropped.

One material attribute is written per **shape node**, not per distinct material,
so a written file carries as many material attributes as it has shapes while
`Model::materials` of the read-back file counts the distinct ones. The server
sample's 23 source materials collapse to 20 because the source repeats three
colour sets across its 105 shapes.

## Entry points

| Layer | Call |
| --- | --- |
| Rust | `ferrojt::jt::model_to_bytes(&model, version)` / `model_to_file(&model, path, version)` |
| Rust, no revision | `export::to_format(Format::Jt, &model)` / `export::write_to_file` |
| CLI | `ferrojt convert model.glb -o out.jt` — add `--jt-version 8.1` to force a revision |
| Python | `model.save("out.jt")`, `model.to_bytes("jt", version="8.1")` |

`ferrojt convert` reads a glTF input through `import`, a JT input through the JT
reader (so the layer filter and the LOD tier still apply), and writes either one
to JT. After the write it reports the revision the file actually landed on:

```text
data\ExampleFiles\SampleJT_Server.jt -> target\server_auto.jt [8.1]
data\ExampleFiles\example_block_jt9.5.jt -> target\block_auto.jt [10.3]
```

In Python, `version` is a **string** (`"10.3"`, or a bare major that fills in the
minor) because pyo3 0.30 cannot express a tri-state keyword; leaving it out is
distinct from naming one, and it is the difference between the 8.1 fallback and
the refusal above.

## What does not survive

- Level of detail: the model has no tier, so a hop leaves one representation per
  part. `jt::dump` and `JtFile.lsg` keep every tier; the model does not.
- Non-triangle primitives: `POINTS`, `LINES` and the strip topologies are refused
  rather than re-indexed.
- JT key/value properties: they live in `Node::extras`, which the builder has no
  way to emit. The viewer's detail pane is their only exit.
- Layer filters: they are a property of a JT *file*, applied on read. A model
  that came from glTF has none, and a written file declares none of its own.
