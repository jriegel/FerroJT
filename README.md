# FerroJT

Read [OpenJT](https://www.jt-open.com/) (`.jt`) CAD files and convert them to
[glTF 2.0](https://www.khronos.org/gltf/) (`.gltf` / `.glb`) and
[USD](https://openusd.org/) (`.usda`).

FerroJT is a Rust workspace with four parts:

| Crate | Purpose |
| --- | --- |
| [`crates/ferrojt`](crates/ferrojt) | The library: the neutral `Model` graph, binary codecs, the JT reader and the glTF/USD writers. |
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
| glTF 2.0 / GLB writer | done |
| USD writer | stub — returns `NotImplemented` |
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
                           decode codecs and the JSON dump writer
      export/              gltf/glb writer, glb framing, usd stub
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
cargo run -p ferrojt-cli -- convert gear.jt --layer-filter LAYCUR -o gear.glb
cargo run -p ferrojt-cli -- convert gear.jt --no-layer-filter -o gear.glb
```

`dump` writes the same JSON the reference JT dump tool produces: the header, the
TOC table and the logical scene graph with its LOD geometry. `--no-toc` drops the
TOC table and `--no-indices` keeps the index arrays empty; several files become
one JSON array. A dump is never filtered by layer.

`convert` applies the layer filter the file declares as active, or its first
filter when the file leaves that unset: `--layer-filter NAME` picks another one
of the declared filters, and `--no-layer-filter` exports every part.

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
glb_bytes = model.to_bytes("glb")
```

`JtFile` is the JT native view of the same file and mirrors the original C++
`PyOpenJt` extension: it is constructed with a path, opened explicitly, and then
asked for a JSON dump or a glTF file.

```python
jt = ferrojt.JtFile("gear.jt")
if jt.open():
    print(jt.toc())                # header + TOC table as JSON
    print(jt.json())               # the full dump
    jt.writeGlTf("gear")           # writes gear.gltf
    print(jt.model())              # the neutral Model, for the exporters

print(ferrojt.dump("gear.jt"))     # one line shortcut for the same dump
```

The export honours the layer filters of the file. `layer_filters()` lists the
names it declares and `active_layer_filter()` the one applied by default;
`layer_filter="NAME"` picks another one and `all_layers=True` keeps every part.
The keywords work on `ferrojt.open`, `ferrojt.read`, `JtFile.model`,
`JtFile.writeGlTf` and `JtFile.write_gltf`; combining the two of them raises
`ValueError`.

Requires Python 3.10 or newer.

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

`pyo3` is built with the `abi3-py310` feature, so the extension uses the stable
ABI: **one wheel per platform covers every CPython from 3.10 upwards**, there is
no wheel per Python version. `build-wheel.bat` builds them all into `dist/`:

```bat
build-wheel.bat            windows wheel + linux wheel + sdist
build-wheel.bat windows    host wheel only
build-wheel.bat linux      cross compiled linux wheel only
build-wheel.bat sdist      source distribution only
```

It produces

```text
ferrojt-0.1.0-cp310-abi3-win_amd64.whl                                    Windows
ferrojt-0.1.0-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl   Linux, glibc >= 2.17
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
* **`#![forbid(unsafe_code)]`** in the core library.

The reasoning behind these choices is in [`docs/`](docs): the neutral model and
the two-layer JT design in [architecture.md](docs/architecture.md), what the
reader guarantees in [jt-reader.md](docs/jt-reader.md), and how the JSON dump
reaches the reference output in [dump.md](docs/dump.md).

## License

MIT OR Apache-2.0.
