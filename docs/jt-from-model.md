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

`export::to_format(Format::Jt, model)` routes here for bytes, and
`export::write_to_file(Format::Jt, model, path)` routes to `model_to_file` so the
written file can name its root partition after its own path. `Format::Jt` is
marked in `Format::is_writable`. It is *not* writable through the `Exporter`
trait — the trait has no place for the revision to write at, so the dispatcher
special-cases it.

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
Partition 1 · MetaData 1 · Part 1 · RangeLOD 1
Shape = meshes · Group = nodes − meshes + 1 · Instance = extra placements + 1
```

The five extra nodes are the wrapper of the next section; the `+1` on `Group` is
the tier group of that wrapper, and the `+1` on `Instance` is the instance that
names the wrapper part. So the written LSG holds `model.nodes.len() + 5 +
instances` nodes for a model that arrives without JT classes — every glTF file
and any graph built by hand — and `+ 1` for a model read out of a JT file, which
brings its own partition and is merged with the root rather than wrapped.

Two demotions are deliberate. A `Shape` node whose mesh has no geometry becomes
a `Group` (glTF mandates a `POSITION` attribute, and an empty JT shape is not
worth writing) — the same policy the reader applies to a representation that
comes back without vertices. And a node holding several representations
keeps only the first, because the builder has no level-of-detail node; the
reader collects tiers finest-first, so that is the finest one.

## The wrapper chain

Shapes straight under the root partition are not what a consumer walks. It
descends from the partition to a **part** and reads the geometry under the
level-of-detail span of that part, so a file whose graph skips that chain reads
as one that will not load — which is exactly what a converted glTF file used to
be. `Writer::wrap` therefore puts the chain of the samples under the root:

```text
Partition -> MetaData -> Instance -> Part -> RangeLOD -> Group -> the graph of the model
```

* The `Part` is created with `add_detached_node` and named by the `Instance`,
  because an instance *refers* to its target instead of holding it as a child —
  that is where a JT file stores a part and its placement.
* The instance is named `<stem>.part` after the file (`part_name`), mirroring the
  `_model1_117.part` of the block samples. The part node itself stays unnamed,
  as in the samples.
* The span holds exactly **one** tier, a `Group`, because a neutral model has one
  representation per node — LOD 0, the finest. `a_converted_file_wraps_its_graph_in_the_classes_a_consumer_walks`
  walks the six classes and pins that single tier.
* `partition_root` merges instead of stacking. The builder always owns a root
  partition, and a model read out of a JT file brings one of its own. Adopting
  that node — moving its children and its transform onto the builder root — is
  what keeps the file to exactly one partition. Stacking a second one under the
  root puts the `Part` a level deeper than the fixed handful of classes a
  consumer descends to reach geometry, and such a file parses, reports a root,
  and then never produces a frame: the external renderer sits idle until its
  stream times out rather than reporting an error.
* `already_wrapped` stops a second chain for the rest of the classes: a model
  that carries a `Part`, `MetaData`, `LOD` or `RangeLOD` but no partition of its
  own has its graph written as it is, because wrapping it would duplicate the
  classes a consumer walks down to the geometry.

`a_converted_file_wraps_its_graph_in_the_classes_a_consumer_walks` (Rust) and
`test_a_converted_file_wraps_its_graph_in_the_classes_a_consumer_walks` (Python)
pin the chain from both sides, including that the instance has no children and
that the triangles under the tier are those of the source file.

The chain is one of four things a strict consumer checks; the other three are the
root file name above, a compression flag that matches what the segment actually
holds, and a property atom whose class version names the revision being written.
All four are spelled out in
[framing facts the writer depends on](jt-writer.md#framing-facts-the-writer-depends-on),
and `target/jtsweep.py` runs every conversion at every legal revision through the
Siemens `jtinfo.exe` to prove it.

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

Two things belong to the `Instance` itself rather than to the subtree it refers
to, and both were missing at first:

* **Its placement and its name.** The `GeometricTransform` of an instance node
  goes on the `Instance` (`set_node_transform`), as does its name. Putting it on
  the target instead — or dropping it — collapses every placement of a part onto
  one place: the two screws of the plate, at `y = −25` and `y = +25`, drew on top
  of each other.
* **A target no child list refers to.** `Writer::place` writes a node detached
  (`add_detached_node`) when nothing above it names it as a child, which is how
  every sample stores a part: it is reached only through the reference of an
  `Instance`. Hanging it under the root as well draws the shared subtree a third
  time, at the identity.

`an_instance_of_a_sample_keeps_its_placement_and_its_detached_target` pins both
for `opening_protection_plate1_jt9.5` — the translations and names of the two
instances, that they share one target, and that this target is a child of no
node at all. It goes from the file straight through `to_model()` and
`model_to_bytes` **without a glTF hop**, because a glTF hop turns the JT
`Instance` class into a `Group` and the test would then prove nothing.

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
colour sets across its 106 shapes.

## Entry points

| Layer | Call | Root partition named after |
| --- | --- | --- |
| Rust | `ferrojt::jt::model_to_bytes(&model, version)` | `Model::source` |
| Rust | `ferrojt::jt::model_to_file(&model, path, version)` | `path` |
| Rust, no revision | `export::to_format(Format::Jt, &model)` | `Model::source` |
| Rust, no revision | `export::write_to_file(Format::Jt, &model, path)` | `path` |
| CLI | `ferrojt convert model.glb -o out.jt` — add `--jt-version 8.1` to force a revision | the output path |
| Python | `model.save("out.jt")`, `model.to_bytes("jt", version="8.1")` | `save` the path, `to_bytes` `Model::source` |

`export::write_to_file` and `Model::save` route JT to `model_to_file` rather than
through `to_format`: the root partition of a JT file is named after the file it
lives in, so the writer has to see the path. `to_format` still produces JT bytes
— it just cannot know a destination — and the revision a file lands on is chosen
from the meshes of the model, not fixed by the container.

## Naming the root partition

A root `Partition` must carry a non-empty file name, and it must have a row in
the scene graph property table: `JTData_SceneGraph.cxx:152-176` of the reference
assistant creates the root node only when `FileName()` is non-empty and then
asserts that a root node exists. A strict consumer refuses a file without one
("Bad file detected … Unable in find root node"), so both are written — see
[naming the root](jt-builder.md#naming-the-root-partition).

The name is derived from the destination when there is one and from the model
otherwise, which keeps a rewritten file indistinguishable from a fresh one:

* `model_to_file` takes the stem of the output path and gives it a `.jt`
  extension, so `target\Testglb.jt` produces a root named `Testglb.jt`.
* `model_to_bytes` takes `Model::source` — set by the JT reader and by
  `import::from_file`, `None` for `import::from_bytes` — so a JT→glTF→JT hop
  keeps the original name. With no source at all the builder falls back to
  `model.jt`.

The extension is split at the **last** dot: `example_block_jt9.5.jt` has a dot
inside its stem, and splitting at the first one named the root `example_block.jt`.

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
  part — the wrapper span holds a single tier. `jt::dump` and `JtFile.lsg` keep
  every tier; the model does not.
- Non-triangle primitives: `POINTS`, `LINES` and the strip topologies are refused
  rather than re-indexed.
- JT key/value properties: they live in `Node::extras`, which the builder has no
  way to emit. The viewer's detail pane is their only exit.
- Layer filters: they are a property of a JT *file*, applied on read. A model
  that came from glTF has none, and a written file declares none of its own.
- Node classes beyond the ones above, and the `MetaData` / `Part` / `RangeLod` /
  `Group` wrapper structure a CAD file uses. A consumer that insists on a `Part`
  under the root will not find one — see the open work in
  [`jt-builder.md`](jt-builder.md).
