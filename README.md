# FerroJT

Read [OpenJT](https://www.jt-open.com/) (`.jt`) CAD files and convert them to
[glTF 2.0](https://www.khronos.org/gltf/) (`.gltf` / `.glb`) and
[USD](https://openusd.org/) (`.usda`) — or read glTF 2.0 and write OpenJT.

![The FerroJT viewer showing an opening protection plate](docs/viewer.png)

*`ferrojt-viewer` on `opening_protection_plate1_jt9.5.jt`: the logical scene
graph in the outliner, the decoded geometry in the viewport, and a key/value
detail pane under the tree that reports what the selected node carries.*

## Why this exists

JT is the lightweight 3D format the Siemens CAD world produces. A `.jt` file
carries the whole product structure of an assembly — parts, instances,
transforms, levels of detail, colours, layers, metadata — as a compressed binary
scene graph, which makes it the common denominator for exchanging large
assemblies between CAD systems. It is also rarely readable outside them: the
tooling around it is proprietary, and the open reference implementation is a
C++ code base from the last decade.

FerroJT is a Rust reader and exporter for it, written against the OpenJT
specification with the reference tooling used only as an oracle — its sources are
never linked or copied. It opens a `.jt` file, decodes the scene graph and its
LOD geometry, and hands you a neutral `Model` of nodes, meshes, attributes and
materials. That `Model` is the hinge of the library: three writers turn it into
the formats the rest of the pipeline consumes — glTF 2.0 for engines and the web,
ASCII USD for stage tools, and JT itself — and a glTF reader fills the same graph
from the other side, so a `.glb` can be handed to a CAD tool. JT can also be
produced from nothing at all, by a builder that assembles the scene graph by hand.
The same library is also a command line tool, a Python package and a desktop
viewer, and the core crate is `#![forbid(unsafe_code)]` with no native
dependencies.

Correctness is measured, not assumed. The JSON dump the library writes is
compared **byte for byte** with the reference dumps of the JT tooling, and the
glTF export is compared as a **rendered picture** with committed reference PNGs.
Every sample in `data/ExampleFiles` — JT 8.0, 8.1, 9.5 and 10.3 — is in both
suites, so a regression in the decoder fails a test rather than turning up as a
broken model downstream.

## Try it

```sh
cargo run -p ferrojt-cli -- convert data/ExampleFiles/example_block_jt9.5.jt -o block.glb
cargo run -p ferrojt-cli -- convert block.glb -o block.jt
cargo run -p ferrojt-viewer -- data/ExampleFiles/example_block_jt9.5.jt
```

```python
import ferrojt

model = ferrojt.open("data/ExampleFiles/example_block_jt9.5.jt")
model.save("block.glb")          # or "block.usda" for ASCII USD, "block.jt" for JT

model = ferrojt.open("block.glb")   # glTF in, JT out
model.save("block.jt", version="10.3")
```

FerroJT is a Rust workspace with four parts:

| Crate | Purpose |
| --- | --- |
| [`crates/ferrojt`](crates/ferrojt) | The library: the neutral `Model` graph, binary codecs, the JT and glTF readers and the glTF/USD/JT writers. |
| [`crates/ferrojt-cli`](crates/ferrojt-cli) | The `ferrojt` command line tool (`info`, `inspect`, `dump`, `convert`, `ls`). |
| [`crates/ferrojt-python`](crates/ferrojt-python) | `pyo3` bindings, published as the Python package `ferrojt`. |
| [`crates/ferrojt-viewer`](crates/ferrojt-viewer) | The `ferrojt-viewer` desktop app: an egui outliner next to a kiss3d viewport. |

## Status

| Area | State |
| --- | --- |
| Neutral model (nodes, meshes, attributes, materials, textures) | done |
| Binary helpers (endianness, bit reader, zlib inflate/deflate, XZ) | done |
| JT reader (7.x – 10.x: header, TOC, LSG, elements, LOD geometry) | done |
| JT JSON dump (the layout the JT tooling uses) | done — byte for byte against the reference dumps in `data/Reference` |
| JT decode codecs (Huffman, range/CDP, vertex and mesh coders) | done |
| glTF 2.0 / GLB reader | done — `.gltf` / `.glb` into the same neutral `Model`, declaring `meter` / `+Y` |
| glTF 2.0 / GLB writer | done |
| USD writer (ASCII `.usda`) | done — each mesh written once and referenced, materials deduplicated by value |
| JT writer (rebuild a read file, LOD and layer filtered) | done — a round trip re-reads to the same dump, structure and picture; `--jt-version` rewrites it as 8.x, 9.x or 10.x |
| JT builder (create a file from nothing, meshes and instances) | done — JT 10.3 by default, `tests/test_build.py` builds a cube at each revision and reads it back |
| JT writer from a neutral `Model` (glTF → JT) | done — `convert model.glb -o out.jt`, unit and up axis converted on the root, falling back to JT 8.x when a mesh is not a closed two-manifold |
| CLI | done |
| Python bindings | done (packaged with `maturin`) |
| Visual regression (rendered export vs. reference picture) | done (`tests/test_visual.py`, Open3D) |
| Interactive viewer (drag and drop, fly camera) | done (`tests/test_viewer.py`, Open3D) |
| Interactive 3D viewer (outliner, picking, gizmo) | done (`crates/ferrojt-viewer`, egui + kiss3d) |

The reader covers the versions found in `data/ExampleFiles`: JT 8.0, 8.1, 9.5
and 10.3. Every sample with a reference dump under `data/Reference` is compared
semantically in `crates/ferrojt/tests/dump.rs` and in `tests/test_ferrojt.py`, and
the three versions of `example_block` are additionally compared as a rendered
picture in `tests/test_visual.py`.

## Layout

```text
crates/
  ferrojt/                 library
    src/
      model/               neutral scene graph (nodes, meshes, materials, ...)
      codec/               endian, bit reader, compression
      jt/                  OpenJT reader, segment/TOC table, element objects,
                           decode codecs, the JSON dump writer, the JT writer,
                           the builder that creates a file from nothing and the
                           writer that turns a neutral Model into a file
      import/              glTF / GLB reader
      export/              gltf/glb writer, glb framing, ascii usd writer
      error.rs  lib.rs
  ferrojt-cli/             `ferrojt` binary
  ferrojt-python/          pyo3 extension module `_ferrojt`
  ferrojt-viewer/          egui + kiss3d viewer application
python/ferrojt/            pure python package that re-exports _ferrojt
tests/                     pytest suite for the python package (incl. the visual
                           regression test and its diff pictures) plus the
                           interactive viewer
data/
  ExampleFiles/            sample JT files
  Reference/               reference JSON dumps and reference pictures (the oracle)
docs/                      design notes
build-wheel.bat            builds the PyPI wheels (Windows + Linux + sdist)
AGENTS.md                  conventions and pitfalls for automated contributors
```

## Building

```sh
cargo build --workspace
cargo test  --workspace --exclude ferrojt-python
cargo clippy --workspace --all-targets
```

`ferrojt-python` is excluded from `cargo test` because its default features
enable `extension-module`, which leaves Python symbols unresolved in a test
binary.

## Command line

```sh
cargo run -p ferrojt-cli -- info    gear.jt
cargo run -p ferrojt-cli -- ls      gear.jt
cargo run -p ferrojt-cli -- inspect gear.jt --pretty
cargo run -p ferrojt-cli -- dump    gear.jt -o gear.json
cargo run -p ferrojt-cli -- convert gear.jt -f glb -o gear.glb
cargo run -p ferrojt-cli -- convert gear.jt -f usd -o gear.usda
cargo run -p ferrojt-cli -- convert gear.jt --layer-filter LAYCUR -o gear.glb
cargo run -p ferrojt-cli -- convert gear.jt --no-layer-filter -o gear.glb
cargo run -p ferrojt-cli -- convert model.glb -o model.jt
cargo run -p ferrojt-cli -- convert model.glb -o model.jt --jt-version 8.1
cargo run -p ferrojt-cli -- write    gear.jt -o gear.out.jt
cargo run -p ferrojt-cli -- write    gear.jt --all-lods --no-layer-filter
cargo run -p ferrojt-cli -- write    gear.jt --layer-filter LAYCUR --compress
cargo run -p ferrojt-cli -- write    gear.jt -o gear10.jt --jt-version 10.3
```

`dump` writes the same JSON the reference JT dump tool produces: the header, the
TOC table and the logical scene graph with its LOD geometry. `--no-toc` drops the
TOC table and `--no-indices` keeps the index arrays empty; several files become
one JSON array. A dump is never filtered by layer.

`convert` applies the layer filter the file declares as active, or its first
filter when the file leaves that unset: `--layer-filter NAME` picks another one
of the declared filters, and `--no-layer-filter` exports every part. The input
may also be a `.gltf` or `.glb` file, in which case the layer keywords are
refused — only a JT file declares filters — and the output may be `-f jt`, which
writes the model as OpenJT through the builder rather than the rewriter. The last
line of output names the format, and for JT the revision the file actually landed
on.

`write` rebuilds the file as JT. It applies the same selection an export
applies — the finest tier below a range LOD and the active layer filter —
unless `--all-lods` or `--no-layer-filter` says otherwise, and it copies every
segment but the scene graph out of the source unchanged. The result is re-read
and reported, so the second line of output says what the filters cost.

`--jt-version 9.5` writes the result at another revision instead of the one of
the source. JT stores its triangles in one of two families — 8.x in the
vertex-based representation, 9.x and 10.x in the topologically compressed coder —
so a rewrite stays inside the family of the source, and a shape the coder cannot
hold (a CAD assembly with open edges) is refused rather than written as a file
whose shapes come back empty. See [docs/jt-writer.md](docs/jt-writer.md).

For `convert … -f jt` the same gate applies, but there is no source revision to
stay inside: unless `--jt-version` says otherwise, a mesh the coder cannot hold
moves the whole file down to JT 8.x instead of failing, and the output line says
so. See [docs/jt-from-model.md](docs/jt-from-model.md).

## Rust viewer

```sh
cargo run -p ferrojt-viewer -- data/ExampleFiles/example_block_jt9.5.jt
```

A native window with the logical scene graph on the left and the model on the
right. The geometry is built straight from the JT meshes — the `Model` is turned
into `kiss3d` GPU meshes in memory, nothing goes through glTF — so the viewer
also shows what the reader decoded rather than what the writer emitted. Materials
come from the JT attributes: `diffuse` becomes the base colour, `emission` the
emissive term, and a mesh without an attribute renders grey.

| Input | Action |
| --- | --- |
| `Open`, or drag a `.jt` file onto the window | load it, refit the camera |
| `View All`, or `F` | frame the whole visible model |
| `Exit` | close the window |
| click a row / click the model | select — both views highlight it |
| eye icon | show or hide a node and its subtree |
| double-click a row | rename it |
| `Filter nodes`, `Expand`, `Collapse` | narrow or open the tree |
| drag in the top-left gizmo | orbit; click its ball for the standard views |
| left drag / wheel in the viewport | orbit / dolly |
| middle drag in the viewport | pan (the right button is free for a context menu) |
| drag the vertical divider | resize the outliner column |
| drag the horizontal divider | resize the detail pane |

The outliner column is split: the tree of the logical scene graph on top and a
key/value detail pane below, reporting what the selected row carries — its class
and name, transform, world box, meshes with triangle counts, materials and LOD
tier, and the raw properties a JT file stores in the meta data stream of that
node (`Name::`, `CAD_MASS::`, `LAYER::` and the rest). Those properties are read
by the library and kept in `Node::extras`; neither the glTF nor the USD writer
emits them, so the detail pane is their only exit.

Hiding a node or filtering the tree never removes it from the graph: a hidden
subtree keeps its rows, its place in the tree and its selection, and `View All`
frames only what is drawn.

## Python

```sh
pip install maturin
maturin develop            # into the active virtual environment
pytest
```

`maturin develop` installs into the environment that is currently active; if the
virtual environment was created but not activated, point `VIRTUAL_ENV` at it
first.

The visual regression tests need an extra: `pip install -e ".[test,visual]"` adds
`open3d` and `numpy`. Without them those tests skip instead of failing.

```python
import ferrojt

model = ferrojt.open("gear.jt")
print(model)                       # Model(nodes=..., meshes=..., ...)
model.save("gear.glb")             # format inferred from the extension
model.save("gear.usda")            # ASCII USD
model.save("gear.jt")              # OpenJT, through the builder
model.to_bytes("usd")              # the same layer as bytes
glb_bytes = model.to_bytes("glb")
```

A glTF file reads into the same `Model`, and `unit` and `up_axis` say what frame
it was authored in — `meter` / `+Y` for glTF, `millimeter` / `+Z` for JT. Writing
JT converts that frame on the root node, so the geometry keeps its shape:

```python
model = ferrojt.open("model.glb")
print(model.unit, model.up_axis)   # meter +Y
model.save("model.jt")                   # written as JT 10.3 when it fits
model.save("model.jt", version="8.1")    # or insist on a revision
model.to_bytes("jt", version="9.5")      # the same as bytes
```

`version` only applies to JT and is refused for the other formats. When a mesh is
not a closed two-manifold the topological coder of JT 9.x and 10.x cannot hold
it, so a write without `version` drops the file to JT 8.x and an explicit
`version` raises instead.

`JtFile` is the JT native view of the same file and mirrors the original C++
`PyOpenJt` extension: it is constructed with a path, opened explicitly, and then
asked for a JSON dump or a glTF file.

```python
jt = ferrojt.JtFile("gear.jt")
if jt.open():
    print(jt.toc())                # header + TOC table as JSON
    print(jt.json())               # the full dump
    jt.writeGlTf("gear")           # writes gear.gltf
    jt.write_jt("gear.out.jt")     # the file back out as JT
    print(jt.model())              # the neutral Model, for the exporters

print(ferrojt.dump("gear.jt"))     # one line shortcut for the same dump
```

`write_jt` takes the same selection keywords as the exporters plus `all_lods`,
`compress` and `version="10.3"` to rewrite the file at another revision; without
`version` it keeps the revision of the source.

The export honours the layer filters of the file. `layer_filters()` lists the
names it declares and `active_layer_filter()` the one applied by default;
`layer_filter="NAME"` picks another one and `all_layers=True` keeps every part.
The keywords work on `ferrojt.open`, `ferrojt.read`, `JtFile.model`,
`JtFile.writeGlTf` and `JtFile.write_gltf`; combining the two of them raises
`ValueError`.

### Walking the scene graph

`Model` is what the exporters build: one node tree, only the finest LOD tier, the
layer filter already applied. `JtFile.lsg` is the other view — the logical scene
graph **of the file**, with every tier, the JT class names, the key/value
properties a translator wrote, the layer tags and the attributes.

```python
jt = ferrojt.JtFile("gear.jt")
jt.open()
graph = jt.lsg                       # JtLsg: the LSG of the file

root = graph.root()                  # JtNode
print(root.class_name, root.name)    # 'Partition' 'gear.asm'
for child in root:                   # iterate, len(), root[0], root[-1]
    print(child.class_name, child.name, child.properties())

node = graph.nodes()[2]              # every node, in file order
print(node.class_name, node.bounds, node.area)
print(node.properties())             # {'Name::': ..., 'CAD_MASS::': ...}
print(node.layers, node.material())  # layer tags, shading of a shape
print(node.transform)                # {'translation': ..., 'rotation': ..., 'scale': ...}
```

Meshes are the special case, because a JT file *instances* them: the plate
samples hold one screw and place it twice. A representation is identified by its
id — its position in the representation list of the file — so the two placements
report the same ids and the geometry is read once.

```python
for mesh in graph.meshes():          # every representation of the file, once
    print(mesh.id, mesh.vertex_count, mesh.triangle_count)

for instance in (n for n in graph.nodes() if n.is_instance):
    print(instance.name, instance.transform["translation"])
    print(instance.mesh_ids)         # [] — an instance carries no geometry
    print(instance.subtree_mesh_ids())   # the ids it shares with its twin
```

Each array of a representation implements the buffer protocol, so NumPy wraps it
**without copying** — the view points straight at the decoded buffers of the file
and keeps them alive. That is how a mesh gets to OpenMesh, trimesh or VTK without
any C++ dependency.

```python
import numpy as np

mesh = graph.mesh(4)
vertices = np.asarray(mesh.array("vertices"))   # float32, (n, 3), read-only
normals = np.asarray(mesh.array("normals"))     # float32, (n, 3)
indices = np.asarray(mesh.array("indices"))     # int32, (m,)
assert vertices.flags.owndata is False          # borrowed, not copied
mesh.array_names                                # ['indices', 'vertices', 'normals']
mesh.arrays()                                   # all three as a dict
```

Requires Python 3.11 or newer.

### Building a file

`JtBuilder` is the way in: it starts as an empty file holding only its root
`Partition`, you fill the scene graph node by node, and `save` writes it as
JT 10.3 — `builder.version = "8.1"` asks for another revision. The nodes are
opaque handles that belong to their builder, so a handle of another builder is
refused.

```python
import ferrojt

builder = ferrojt.JtBuilder()
root = builder.root()
part = builder.add_node(root, "Part", "cube")
shape = builder.add_shape(part, ferrojt.MeshData(vertices, indices), name="cube shape")
builder.set_material(shape, ferrojt.MaterialData(diffuse=[0.2, 0.6, 0.9, 1.0]))
builder.set_translation(part, [-20.0, 0.0, 0.0])

left = builder.instance_of(root, part)     # shares the subtree of part
right = builder.instance_of(root, part)    # ... and its geometry
builder.set_translation(right, [20.0, 0.0, 0.0])
builder.save("cube.jt")
```

A mesh is handed over the way a modeller emits one — shared corners and all —
and the builder explodes it to one slot per triangle corner, because a JT 8.x
faceted shape stores the normal per slot and cannot share a corner. At JT 9.x and
10.x the coder does that explosion itself, so the vertex table of a representation
comes back larger and in a different order while the triangles do not change. Read
the file back with `ferrojt.open` or `ferrojt.JtFile` like any other: the graph,
the names, the placements and the triangles come back unchanged, and the two
instances still share one representation. `docs/jt-builder.md` holds the layout
the writer emits and what it leaves open.

### Visual regression

`tests/test_visual.py` renders the glTF export of a sample and compares the picture
with a reference PNG next to the reference dumps, named after the sample:
`data/Reference/example_block_jt9.5.png`. It is the same workflow as the JSON oracle
— the reference is committed, a mismatch is a test failure.

```sh
python tests/test_visual.py --write           # (re)create the references
python tests/test_visual.py                   # compare, write diff pictures
python tests/test_visual.py --threshold 0.99  # loosen the match
```

The picture is deterministic: a fixed 640x480 frame, the classic isometric view
direction `[1, 1, 1]`, a fit-all camera derived from the bounding sphere, one flat
grey material and a shadowless headlight, so the render reports the geometry and not
how a loader happens to read a glTF material. A GPU shader is not bit exact across
drivers, so the comparison is a similarity score rather than equality: the render
passes when at least `0.995` of the possible colour difference is left unused
(`FERROJT_VISUAL_THRESHOLD`), and a pixel counts as changed when it moves by more
than `8` (`FERROJT_VISUAL_EPS`). A failure writes a three panel picture — render,
reference, amplified difference — to `tests/_visual_out/`.

Six samples are covered: the three `example_block` versions, the two
`opening_protection_plate1` ones and `SampleJT_Server.jt`. Two Open3D offscreen
quirks are worked around in the test: a frame can come back blank, so the render is
retried until it contains the background, and screen-space ambient occlusion makes
the colour pass return an all-zero frame for the ~1M-triangle server mesh, so it is
switched off — invisibly, measured on the smaller samples.

### Interactive viewer

`tests/test_viewer.py` opens an Open3D window and shows a JT file, so the export can
be looked at rather than only counted:

```sh
python tests/test_viewer.py                       # empty, load through File ▸ Open
python tests/test_viewer.py data/ExampleFiles/example_block_jt9.5.jt
```

| Input | Action |
| --- | --- |
| drag a `.jt` file onto the window, or `File ▸ Open` | load it |
| left drag | orbit around the model |
| right drag | look around from the camera |
| middle drag | pan |
| wheel | dolly in and out |
| `w` `a` `s` `d` | fly forward, right, back, left |
| `q` `z` | fly up / down |
| `ESC` | close (same as `File ▸ Exit`) |

The frame rate is shown in the top left corner. The file goes through the same
glTF export as `ferrojt convert` — written to a temporary `.glb` and read back —
so the viewer shows exactly what the writer produces, materials included. It needs
the same extra as the visual regression test, `pip install -e ".[test,visual]"`.

### Wheels

`pyo3` is built with the `abi3-py311` feature, so the extension uses the stable
ABI: **one wheel per platform covers every CPython from 3.11 upwards**, there is
no wheel per Python version. `build-wheel.bat` builds them all into `dist/`:

```bat
build-wheel.bat            windows wheel + linux wheel + sdist
build-wheel.bat windows    host wheel only
build-wheel.bat linux      cross compiled linux wheel only
build-wheel.bat sdist      source distribution only
```

It produces

```text
ferrojt-0.1.0-cp311-abi3-win_amd64.whl                                    Windows
ferrojt-0.1.0-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl   Linux, glibc >= 2.17
ferrojt-0.1.0.tar.gz                                                      source distribution
```

Upload the whole set together with `python -m twine upload dist\*` — PyPI serves
the file matching the installer, and the sdist covers anything else. A single
universal wheel is not possible for a native extension: the platform tag is part
of the file name.

The Linux wheel is cross compiled on Windows with `zig` as the linker, so no
Docker is needed. The script installs both prerequisites itself: `zig` comes from
the `maturin[zig]` extra and the `x86_64-unknown-linux-gnu` standard library from
`rustup target add`. macOS and the ARM wheels cannot be produced on a Windows
host; `maturin generate-ci github > .github\workflows\CI.yml` generates the full
matrix instead.

## Design notes

* **One neutral model.** Readers build a [`Model`](crates/ferrojt/src/model/scene.rs);
  writers consume it. Adding a format never touches the others.
* **The frame is declared, not assumed.** A reader says what units and up axis it
  read — JT reports `millimeter` / `+Z`, glTF `meter` / `+Y` — and a writer that
  targets another frame converts on the root node instead of touching the
  geometry. So a JT → JT write is an identity and a glTF → JT write costs one
  scale and one quarter turn.
* **Handles, not pointers.** Cross references (`node -> mesh`, `primitive ->
  attribute`) are `usize` indices into the owning `Vec` on `Model`. The graph is
  copyable, serializable to JSON and safe to walk in parallel.
* **Attributes are views.** Geometry lives in opaque [`BufferBlock`](crates/ferrojt/src/model/geometry.rs)
  byte buffers; an [`Attribute`](crates/ferrojt/src/model/geometry.rs) is
  `(buffer, offset, count, components, component_type)`. Readers never copy
  vertex data, and exporters widen what glTF cannot express (`f16`/`f64` → `f32`).
* **One mesh per part.** A range LOD stores the same shape at several levels of
  detail; `to_model()` keeps only the finest one and tags it with
  `Mesh::lod_level`, so a glTF or USD export does not repeat a part once per
  tier. A JT dump still reports every tier.
* **Layers filter parts, not nodes.** A JT file names its layer filters in the
  meta data and tags each part with the layers it belongs to; an export drops the
  geometry of a part whose layers the active filter does not accept, but keeps
  every node of the scene graph. A file without layer information exports all of
  its parts, and a dump always shows everything.
* **USD references, it does not copy.** Every mesh is authored once in an
  invisible geometry library and `prepend references` by each consumer, and a
  node reached from several parents is referenced rather than duplicated; the
  layer records which layer filter produced it. Materials are collapsed by their
  shading values, so a part repeated a dozen times costs one material prim.
* **`#![forbid(unsafe_code)]`** in the core library.

The reasoning behind these choices is in [`docs/`](docs): the neutral model and
the two-layer JT design in [architecture.md](docs/architecture.md), what the
reader guarantees in [jt-reader.md](docs/jt-reader.md), how the JSON dump
reaches the reference output in [dump.md](docs/dump.md), and how a glTF file
becomes a JT file in [gltf-reader.md](docs/gltf-reader.md) and
[jt-from-model.md](docs/jt-from-model.md).

## License

MIT OR Apache-2.0.
