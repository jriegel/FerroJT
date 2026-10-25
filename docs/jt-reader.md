# OpenJT reader contract

> **Placeholder.** The format details and the reference implementation will be
> supplied; this file records the contract the reader has to satisfy so the rest
> of the workspace stays stable.

## Entry points

| Item | Current state | Required behaviour |
| --- | --- | --- |
| `jt::open(path) -> Result<Model>` | `Err(NotImplemented("reading JT files"))` | Parse the whole file into a populated `Model`. |
| `jt::read_header(path) -> Result<JtHeader>` | `Err(NotImplemented("reading JT headers"))` | Parse only the header, cheaply, for `ferrojt info`. |
| `JtHeader` | defined (`version`, `big_endian`, `magic`, `originator`, `file_size`, `unit`, `up_axis`) | Feed `Model::unit` / `Model::up_axis` and `Model::source`. |

Planned submodules, named in `jt/mod.rs`:

* `jt::ios` — the segment/object instance table and its aliases.
* `jt::lod` — LOD tiers, level 0 triangle data and the lower level tessellations.
* `jt::shape` — B-rep / shape structure and the node hierarchy.

## What the reader must produce

1. `Model::source` set to the file name, `unit` and `up_axis` taken from the
   header so exporters can rescale.
2. One `Node` per JT node instance, with `name`, `kind` (the JT node type as a
   string), a `Transform` in JT's right handed / CCW / `x y z w` convention, and
   `children` in file order.
3. Geometry appended to `Model::accessors` as `BufferBlock` bytes plus one
   `Attribute` per channel, named `POSITION`, `NORMAL`, `COLOR_n`, `TEXCOORD_n`.
   `Primitive::attributes` holds the returned handles.
4. `bounds` filled wherever the file provides them; otherwise left `None` so the
   exporter can compute them.
5. `Model::bounds` as the union of the root nodes.

## Constraints

* No `unsafe` — `crates/ferrojt` is `#![forbid(unsafe_code)]`.
* Use `codec::ByteOrder` explicitly; never assume host endianness.
* Use `codec::BitReader` for the packed header fields (MSB first).
* Use `codec::inflate` for DEFLATE compressed segments.
* Every malformed byte sequence must return `Error::Invalid { offset, message }`
  with the offset of the offending field, never panic and never read past the
  buffer.
* Truncated files must be detected before a slice is indexed.

## Tests expected with the reader

* Header round trip for each JT version.
* A single instance LOD0 triangle strip producing exact `POSITION` bytes and
  correct `Aabb`.
* A compressed segment inflating to the same bytes as the uncompressed one.
* A truncated file returning `Error::Invalid`, asserted with `#[should_panic]`
  free checks.
* End to end: `ferrojt::open(sample.jt)` then `export::to_format(Format::Glb, ..)`
  parses back with the `gltf` dev dependency.
