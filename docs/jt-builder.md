# JT builder

> **Status: implemented.** `crates/ferrojt/src/jt/build.rs` creates a `.jt` file
> out of nothing: [`JtBuilder`](../crates/ferrojt/src/jt/build.rs) starts with a
> scene graph holding only its root `Partition`, accumulates nodes, meshes and
> instances, and writes a complete file. `crates/ferrojt/tests/build.rs` pins the
> Rust side and `tests/test_build.py` the Python side, both by writing a cube,
> instancing it twice and reading the result back.

## What this is not

[`jt::write_to_bytes`](jt-writer.md) rewrites a file it has read: it rebuilds the
scene graph and copies every other segment out of the source byte for byte, so it
needs a source. It also cannot write a neutral `Model`, which is why
`Format::Jt` stays non-writable in `export/`.

The builder is the other direction. Nothing is copied — the banner, the table of
contents, the scene graph, the property table and the shape segments are all
emitted here, from the graph that was assembled by hand.

## Why the files are JT 8.1

A JT file stores its triangles in one of the quantised or topologically
compressed encodings, and the reader decodes all of them. Re-encoding is a
separate problem with its own parameter search, and the dual vertex-face coder
JT 9.x and 10.x use has no encoder in this crate at all. So a from-scratch file
uses the newest version whose representation can be written directly: **JT 8.1**,
with the vertex-based lossless representation — the one [`LodGeometry`] hands
back as `params[0] == 0` when a file is read.

That choice is visible in the output and nowhere else. Every reader in the
workspace, the CLI, the exporters and the Python layer treat the result like any
other JT file.

## Entry points

| Item | Behaviour |
| --- | --- |
| `JtBuilder::new()` / `JtBuilder::default()` | An empty file: one root `Partition`, no meshes. |
| `add_node(parent, class, name)` | Add a `Partition`/`Part`/`MetaData`/`Group`/`Shape`/`Instance` node. |
| `add_shape(parent, name, mesh)` | Add a `Shape` node holding a mesh. |
| `instance_of(parent, target)` | Share the subtree of `target` instead of copying it. |
| `add_mesh(mesh)` / `set_mesh(node, mesh)` | Keep a mesh without a node, then attach it. |
| `set_transform` / `set_translation` / `set_placement` / `set_node_transform` | The transform of a node. |
| `set_material(node, material)` | A JT material attribute on a node. |
| `write_to_file(path)` / `write_to_bytes()` | The complete file, JT 8.1, DEFLATEd unless `compress` is off. |
| `ferrojt::jt::JtBuilder` in Python | `JtBuilder`, `NodeHandle`, `MeshHandle`, `MeshData`, `MaterialData`. |

Handles are opaque: `JtNode(usize)` and `JtMesh(usize)` have private fields, so
outside the crate they are tokens, and every operation checks that a token
belongs to the builder it is used with.

## The mesh layout, and why a mesh is exploded

A JT 8.x faceted shape cannot share a vertex slot: the normal is bound to the
slot, so a corner used by three faces would need three normals. `MeshData`
therefore **explodes** the mesh on the way in — one slot per triangle corner —
and a cube comes back as 36 positions, 36 normals and 36 indices for its twelve
triangles. A caller hands over shared corners the way a modeller emits them and
gets the exploded form out of `vertices()`.

The second half of the problem is the strip walk. A JT 8.x representation stores
each triangle as its own one-triangle strip, and `expand_strips` in
`jt/object.rs` walks those strips alternating between the two windings: `0,1,2`
then `3,5,4` then `6,7,8`. The **slot number is the index the reader emits**, so
the indices of a written file are never the indices that went in.

`representation()` handles that with two vectors:

* `emit` — the permutation the reader walks, strip by strip;
* `written` — its inverse, saying which corner of the source mesh belongs in
  which slot of the file.

The geometry is written per written slot, so the slots come back renumbered and
the triangles do not. The normal binding byte follows the mesh
(`u8::from(with_normals)`) rather than being hardcoded, and the payload
interleaves normal then position per slot, DEFLATEd, behind the unpacked and
packed lengths.

## File layout

* **Banner**: `Version 8.1 JT  FerroJT <version>`, 80 bytes, byte order flag `0`
  (little endian) at 80, reserved `i32`, TOC offset as `i32` (JT 8.x), and the
  GUID of the scene graph segment.
* **Segments**: the LSG first, then one shape segment per representation. The TOC
  table is appended at the end.
* **Segment GUIDs** are synthetic: a `0xfe` marker byte and a counter in an
  otherwise empty GUID, so they cannot collide with a class GUID or with the
  end-of-elements marker.
* **Object ids**: nodes in walk order, then the attributes they refer to, then
  the property atoms — one table for all three, as the scene graph reader
  expects. Properties must **continue** that numbering rather than start at 0,
  because the property table ends a row with the key `0`.
* **Node bodies** follow the per-class field order of JT 8.x: no class version in
  a node base, `Part` and `MetaData` carrying their version fields
  unconditionally, `Shape` and `Instance` carrying no child list at all, and an
  `Instance` target as one bare `i32`.
* **Late loads**: a `Shape` gets a `JT_LLPROP_SHAPEIMPL` property pointing at its
  segment GUID with segment type 7.
* **Compression**: flag 2 / algorithm 2 (zlib), like the writer. XZ is read but
  never emitted.

## Rejected graphs

`write_to_bytes` reports `Error::Invalid` rather than writing something a reader
would trip over:

* a `Shape` node with no mesh;
* an `Instance` with no target;
* a cycle — a node that is its own ancestor;
* a node not reachable from the root partition, named in the message.

`MeshData::validate` is checked when a mesh is built, not when the file is
written: no triangles, positions that are not triples, an index count that is not
a multiple of three, an index outside the slots, or normals without exactly one
triple per slot.

## Deliberate deviations

| Field | Written | A translator writes |
| --- | --- | --- |
| `Partition` flags | `0` | often `1`, asking for a second box |
| `Partition` area and count arrays | `0` | varies |
| `Partition` box | the box of every mesh below it | the same, usually |
| `Shape` reserved box | zeros | the transformed box |
| `Shape` trailing size and compression | `0` | e.g. `9272` |
| `Material` colour flags | `0` — four components each | a file may store one grey value |
| Node names | the `JT_PROP_NAME` property | the same |

None of it is read back by anything in the workspace, and none of it shows up in
`jt::dump`.

## How it is judged

`crates/ferrojt/tests/build.rs` writes a fixture graph — a partition, a named
part holding a cube, and two instances of that part at different places — and
reads it back:

* the banner says JT 8.1, little endian, and the TOC has the two segments;
* the classes, names, child lists and instance targets survive;
* the representation comes back with the strip permutation applied to its
  indices, the same triangles, the same per-slot normals, and 36 slots;
* one representation is shared by the part and both instances, and each instance
  keeps its own placement;
* `to_model()` gives five nodes, one mesh, twelve triangles and one attachment;
* a **compact** mesh — shared corners — is exploded and comes back triangle for
  triangle, which is the case a pre-exploded fixture cannot catch;
* the bytes are deterministic, and the uncompressed variant decodes to the same
  geometry.

`tests/test_build.py` runs the same proof through the Python layer and adds the
export side: the dump of a built file is valid JSON, and the file converts to
glTF and USD like any sample.

## Open work

* JT 9.x and 10.x output, which needs an encoder for the dual vertex-face coder.
* XZ output for the scene graph segment.
* Key/value properties on a node, so a built file can carry `CAD_MASS::` and the
  like instead of only a name.
* Layer filters: a built file declares none, so nothing gates on them.
* Writing JT from `export/`, i.e. from a neutral `Model` — the builder is the
  first half of that, but a `Model` has no representation ids to reattach.

[`LodGeometry`]: ../crates/ferrojt/src/jt/object.rs
