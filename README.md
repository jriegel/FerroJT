# FerroJT

Read [OpenJT](https://www.jt-open.com/) (`.jt`) CAD files and convert them to
[glTF 2.0](https://www.khronos.org/gltf/) (`.gltf` / `.glb`) and
[USD](https://openusd.org/) (`.usda`).

FerroJT is a Rust workspace with three parts:

| Crate | Purpose |
| --- | --- |
| [`crates/ferrojt`](crates/ferrojt) | The library: the neutral `Model` graph, binary codecs, the JT reader and the glTF/USD writers. |
| [`crates/ferrojt-cli`](crates/ferrojt-cli) | The `ferrojt` command line tool (`info`, `inspect`, `convert`, `ls`). |
| [`crates/ferrojt-python`](crates/ferrojt-python) | `pyo3` bindings, published as the Python package `ferrojt`. |

## Status

| Area | State |
| --- | --- |
| Neutral model (nodes, meshes, attributes, materials, textures) | done |
| Binary helpers (endianness, bit reader, zlib inflate/deflate) | done |
| glTF 2.0 / GLB writer | done |
| USD writer | stub — returns `NotImplemented` |
| JT reader | stub — returns `NotImplemented` |
| CLI | done |
| Python bindings | done (packaged with `maturin`) |

The JT reader is intentionally a boundary, not an implementation: everything
around it (model, codecs, exporters, CLI, bindings) is finished and tested, so a
reader only has to fill `crates/ferrojt/src/jt`.

## Layout

```text
crates/
  ferrojt/                 library
    src/
      model/               neutral scene graph (nodes, meshes, materials, ...)
      codec/               endian, bit reader, compression
      jt/                  OpenJT reader (boundary)
      export/              gltf/glb writer, glb framing, usd stub
      error.rs  lib.rs
  ferrojt-cli/             `ferrojt` binary
  ferrojt-python/          pyo3 extension module `_ferrojt`
python/ferrojt/            pure python package that re-exports _ferrojt
tests/                     pytest suite for the python package
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
cargo run -p ferrojt-cli -- convert gear.jt -f glb -o gear.glb
```

Until the JT reader lands these print a clear "not implemented yet" message
instead of a stack trace.

## Python

```sh
pip install maturin
maturin develop            # into the active virtual environment
pytest
```

```python
import ferrojt

model = ferrojt.open("gear.jt")
print(model)                       # Model(nodes=..., meshes=..., ...)
model.save("gear.glb")             # format inferred from the extension
glb_bytes = model.to_bytes("glb")
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

## License

MIT OR Apache-2.0.
