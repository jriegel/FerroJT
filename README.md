# FerroJT

Read [OpenJT](https://www.jt-open.com/) (`.jt`) CAD files and convert them to
[glTF 2.0](https://www.khronos.org/gltf/) (`.gltf` / `.glb`) and
[USD](https://openusd.org/) (`.usda`).

FerroJT is a Rust workspace with three parts:

| Crate | Purpose |
| --- | --- |
| [`crates/ferrojt`](crates/ferrojt) | The library: the neutral `Model` graph, binary codecs, the JT reader and the glTF/USD writers. |
| [`crates/ferrojt-cli`](crates/ferrojt-cli) | The `ferrojt` command line tool (`info`, `inspect`, `dump`, `convert`, `ls`). |
| [`crates/ferrojt-python`](crates/ferrojt-python) | `pyo3` bindings, published as the Python package `ferrojt`. |

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

The reader covers the versions found in `data/ExampleFiles`: JT 8.0, 8.1, 9.5
and 10.3. Every sample with a reference dump under `data/Reference` is compared
semantically in `crates/ferrojt/tests/dump.rs` and in `tests/test_ferrojt.py`.

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
python/ferrojt/            pure python package that re-exports _ferrojt
tests/                     pytest suite for the python package
data/
  ExampleFiles/            sample JT files
  Reference/               reference JSON dumps used as the oracle
docs/                      design notes
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
```

`dump` writes the same JSON the reference JT dump tool produces: the header, the
TOC table and the logical scene graph with its LOD geometry. `--no-toc` drops the
TOC table and `--no-indices` keeps the index arrays empty; several files become
one JSON array.

## Python

```sh
pip install maturin
maturin develop            # into the active virtual environment
pytest
```

`maturin develop` installs into the environment that is currently active; if the
virtual environment was created but not activated, point `VIRTUAL_ENV` at it
first.

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

Requires Python 3.10 or newer.

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
* **`#![forbid(unsafe_code)]`** in the core library.

The reasoning behind these choices is in [`docs/`](docs): the neutral model and
the two-layer JT design in [architecture.md](docs/architecture.md), what the
reader guarantees in [jt-reader.md](docs/jt-reader.md), and how the JSON dump
reaches the reference output in [dump.md](docs/dump.md).

## License

MIT OR Apache-2.0.
