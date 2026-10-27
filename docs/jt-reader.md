# OpenJT reader contract

> **Implemented.** The reader lives in `crates/ferrojt/src/jt` and covers the
> versions found in `data/ExampleFiles` (JT 8.0, 8.1, 9.5 and 10.3). The
> contract below is what it satisfies today; it is kept because the rest of the
> workspace still depends on it.

## Entry points

| Item | Behaviour |
| --- | --- |
| `jt::open(path) -> Result<Model>` | Parses the file and converts the JT native view into a populated `Model`. |
| `jt::read_header(path) -> Result<FileHeader>` | Parses only the header banner (version, byte order, TOC offset, LSG GUID) for `ferrojt info`. |
| `jt::JtFile::open(path)` | The JT native view: header, TOC table and the decoded logical scene graph. This is what `ferrojt dump` and `ferrojt.JtFile` use. |
| `jt::dump(file, config, indent) -> Result<String>` | The JSON layout the JT tooling writes; see [dump.md](dump.md). |

Submodules:

* `jt::segment` — header banner, TOC table, segment framing and the
  DEFLATE/XZ envelopes.
* `jt::reader` — the endian aware cursor every element reader is built on.
* `jt::object` — LSG node types, attributes and element objects (shape LODs,
  proxy meta data, XT B-rep).
* `jt::decode` — the compression codecs: Huffman, the range/CDP driver, the
  vertex coders and the mesh coder.
* `jt::model` — `JtFile` plus `SceneGraph`, the handle based JT scene.
* `jt::guid` — the object identifier type and the well known type GUIDs.

## What the reader produces

1. `Model::source` set to the file name and `Model::extras` carrying the format,
   the version and the segment count; `unit`/`up_axis` stay at their defaults
   because the JT header does not store them.
2. One `Node` per JT node instance, with `name`, `kind` (the JT node type as a
   string), a `Transform` in JT's right handed / CCW / `x y z w` convention, and
   `children` in file order.
3. Geometry appended to `Model::accessors` as `BufferBlock` bytes plus one
   `Attribute` per channel, named `POSITION` and `NORMAL`.
   `Primitive::attributes` holds the returned handles.
4. Only the finest level of detail of a range LOD. A `RangeLOD` lists its
   children from the finest tier down and JT does not store that tier, so it is
   counted while walking: the first group below a `RangeLOD` is tier 0, the next
   tier 1, and so on. A shape at any other tier contributes no mesh, which is
   what the reference glTF writer does. The nodes themselves are all kept, so
   the graph keeps its shape, and `Mesh::lod_level` records the tier a mesh came
   from. `jt::dump` is not filtered — a dump still reports every tier.
5. `bounds` filled wherever the file provides them; otherwise left `None` so the
   exporter can compute them.
6. `Model::bounds` as the union of the root nodes.

## Constraints

* No `unsafe` — `crates/ferrojt` is `#![forbid(unsafe_code)]`.
* Use `codec::ByteOrder` explicitly; never assume host endianness.
* Use `codec::BitReader` for the packed header fields (MSB first).
* Use `codec::inflate` for DEFLATE compressed segments and `codec::decompress_xz`
  for the XZ envelopes JT 10.x uses.
* Every malformed byte sequence must return `Error::Invalid { offset, message }`
  with the offset of the offending field, never panic and never read past the
  buffer.
* Truncated files must be detected before a slice is indexed.
* A single damaged element must not lose the file: the reader logs a warning and
  skips to the end of the element, the way the reference implementation does.

## Tests

* `crates/ferrojt/tests/samples.rs` — every sample opens, the TOC count and the
  vertex/index counts of every representation are pinned, and the root is a
  `Partition`.
* `crates/ferrojt/tests/model.rs` — a converted model holds one mesh per range
  LOD, tagged `lod_level: Some(0)`, and keeps every node of the graph.
* `crates/ferrojt/tests/dump.rs` — every sample with a reference dump under
  `data/Reference` matches it semantically, and every dump is valid JSON.
* Unit tests in `jt/decode` compare the codecs against the reference output byte
  for byte.
