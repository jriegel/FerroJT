# Architecture

```mermaid
flowchart LR
  JT[OpenJT file] -->|jt::open| M[Model]
  JT -->|jt::JtFile::open| JN[JT native view]
  JN -->|jt::dump| JSON[JSON dump]
  JN -->|to_model| M
  M -->|export::to_format| GLTF[glTF / GLB]
  M -->|export::to_format| USD[USD]
  M -->|serde| JSON
  CLI[ferrojt CLI] --> M
  CLI --> JN
  PY[python ferrojt] --> M
  PY --> JN
```

Everything flows through one neutral structure, [`Model`](../crates/ferrojt/src/model/scene.rs).
Readers produce it, writers consume it, and the CLI and Python bindings are thin
wrappers over `ferrojt::open` plus `ferrojt::export::*`.

JT is read twice over. `jt::JtFile` keeps the format's own shape — header, TOC
table and a handle based `SceneGraph` — because `ferrojt dump` and
`ferrojt.JtFile` have to reproduce the JT tooling byte for byte, which the
neutral model cannot. `JtFile::to_model()` then flattens that view into a
`Model` for the exporters.

## The neutral model

| Type | File | Role |
| --- | --- | --- |
| `Model` | `model/scene.rs` | Owns every table: `nodes`, `scenes`, `meshes`, `materials`, `textures`, `accessors`, plus `unit`, `up_axis`, `bounds`. |
| `Node` / `Transform` / `Instance` | `model/scene.rs` | Scene graph. `Transform` stores JT's right handed, CCW, `x y z w` quaternion convention. |
| `Mesh` / `Primitive` | `model/geometry.rs` | Geometry. A primitive lists attribute *handles* (`Vec<usize>`), an optional index handle, a `Topology` and an optional material index. |
| `Attribute` / `BufferBlock` | `model/geometry.rs` | `Attribute` is a typed view — `(buffer, offset, count, components, component_type, normalized)` — into an opaque `BufferBlock`. |
| `Accessors` | `model/geometry.rs` | The two tables above plus `push_buffer` / `push_attribute` / `attribute` / `attribute_bytes`. |
| `Material` / `Channel` / `Texture` | `model/material.rs` | A small metallic/roughness model that maps cleanly onto both glTF and USD. |
| `Aabb` | `model/bounds.rs` | Axis aligned bounds. |

Design rules:

* All cross references are `usize` indices into the owning `Vec` on `Model`. No
  `Rc`, no lifetimes, no cycles — the graph is `Clone + Serialize` and can be
  walked with `rayon`.
* Vertex data is never restructured on read. A reader appends bytes to a
  `BufferBlock` and describes them with an `Attribute`; exporters reinterpret or
  widen on demand.
* `crates/ferrojt` is `#![forbid(unsafe_code)]` and has no `unsafe` blocks.

## Codecs

`codec/` holds the primitives a JT reader needs, independent of the format:

* `endian` — `ByteOrder` plus `ReaderExt` / `WriterExt` on `std::io::Read` /
  `Write`. JT stores metadata big endian and vertex data in either order, so the
  order is always an explicit argument.
* `bits` — an MSB first `BitReader` for the packed JT header fields.
* `compress` — zlib and XZ signature checks plus `inflate`, `deflate` and
  `decompress_xz`. JT 10.x concatenates several XZ streams per segment, so
  `find_xz_streams` splits them before decoding.

## The JT reader

`jt/` is the format layer, and it is deliberately separate from `model/`:

* `jt::segment` — header banner, TOC table, segment framing, compression
  envelopes.
* `jt::reader` — the endian aware cursor every element reader uses.
* `jt::object` — LSG nodes, attributes and element objects.
* `jt::decode` — Huffman, the range/CDP driver, the vertex coders and the mesh
  coder, verified byte for byte against the reference output.
* `jt::model` — `JtFile` and `SceneGraph`, plus `to_model()`.
* `jt::dump` — the JSON layout the JT tooling writes, including the C `%g`
  number formatting that `format_g` reproduces.

See [jt-reader.md](jt-reader.md) and [usd-writer.md](usd-writer.md) for the two
contracts that bound the workspace.

## Exporters

`export/mod.rs` defines `Exporter { FORMAT, write }` and the dispatchers
`to_format` and `write_to_file`.

* `export/gltf.rs` builds a `gltf_json::Root` directly (no `gltf` dependency in
  the library) and merges every buffer into one BIN buffer. `.gltf` output inlines
  that buffer as a base64 data URI.
* `export/glb.rs` does the GLB framing that `gltf-json` does not provide: header,
  JSON chunk padded with `0x20`, BIN chunk padded with `0x00`.
* `export/usd.rs` is a documented stub; see [usd-writer.md](usd-writer.md).

## Error handling

`error::Error` distinguishes `Io`, `Json`, `UnsupportedFormat`, `Unsupported`,
`NotImplemented`, `Invalid { offset, message }` and `Corrupt`. The CLI maps
`NotImplemented` to a friendly message, and the Python bindings map it to
`NotImplementedError`.
