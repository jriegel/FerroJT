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
needs a source.

The builder is the other direction. Nothing is copied — the banner, the table of
contents, the scene graph, the property table and the shape segments are all
emitted here, from the graph that was assembled by hand. A neutral `Model` is
written by [`jt::from_model`](jt-from-model.md), which is a walk over the graph
that ends up calling this builder.

## Which revision a built file uses

A JT file stores its triangles in one of two families of representation. JT 8.x
uses the **vertex-based lossless** one — the form [`LodGeometry`] hands back as
`params[0] == 0` when a file is read — and JT 9.x and 10.x use the **dual
vertex-face coder**, which this crate now encodes as well as decodes
(`jt/encode.rs`). So a from-scratch file can be written at any revision, and the
builder defaults to the newest one it supports: **JT 10.3**.

[`JtBuilder::set_version`] picks another, and [`JtBuilder::version`] reports the
one in effect. The choice is visible in three places and nowhere else: the
banner, the width of the TOC offsets, and the segment type of a shape. Every
reader in the workspace, the CLI, the exporters and the Python layer treat the
result like any other JT file.

The coder is not a neutral container, and the two families cannot be traded for
each other:

* The coder **explodes the vertex table**: it keeps one vertex instance per
  triangle corner, so a 24-corner cube comes back as 36 slots, and it orders
  them differently from the way they went in. The triangle count is untouched.
  For JT 8.x the explosion is done on the way in by `MeshData`, for 9.x/10.x by
  the coder itself — either way the caller sees the same triangles.
* The coder needs a **closed two-manifold**. Every edge must be shared by
  exactly two triangles, every vertex ring must be a single cycle, and the fan
  order around a vertex is built combinatorially by crossing shared edges —
  never by sorting the triangles by angle, which breaks as soon as a part
  projects onto its own fan, as a screw thread does.
* A mesh that fails that is refused for 9.x and 10.x and can only be written as
  JT 8.x, which has no such requirement.

## Entry points

| Item | Behaviour |
| --- | --- |
| `JtBuilder::new()` / `JtBuilder::default()` | An empty file: one root `Partition`, no meshes, JT 10.3. |
| `add_node(parent, class, name)` | Add a `Partition`/`Part`/`MetaData`/`Group`/`Shape`/`Instance` node. |
| `add_shape(parent, name, mesh)` | Add a `Shape` node holding a mesh. |
| `instance_of(parent, target)` | Share the subtree of `target` instead of copying it. |
| `add_mesh(mesh)` / `set_mesh(node, mesh)` | Keep a mesh without a node, then attach it. |
| `set_transform` / `set_translation` / `set_placement` / `set_node_transform` | The transform of a node. |
| `set_material(node, material)` | A JT material attribute on a node. |
| `set_version(version)` / `version()` | The revision to write, `JtVersion`; default `10.3`. |
| `write_to_file(path)` / `write_to_bytes()` | The complete file, DEFLATEd unless `compress` is off. |
| `ferrojt::jt::JtBuilder` in Python | `JtBuilder`, `NodeHandle`, `MeshHandle`, `MeshData`, `MaterialData`, and `builder.version` as a readable and assignable string. |

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

At JT 9.x and 10.x the shape segment holds a **tri-strip set** instead, and the
layout is the coder's: the vertex and face streams are built by
`jt/encode.rs`, which welds the positions, walks the dual graph and emits the
per-vertex attribute masks. The strip offsets go through the null codec (byte 0)
as before, the payload through DEFLATE. A 9.x/10.x shape carries no normal
binding byte at all — `LodGeometry::normal_binding` comes back `None`, where an
8.x shape reports `Some(1)`.

## File layout

* **Banner**: `Version 10.3 JT  FerroJT <version>`, 80 bytes, byte order flag `0`
  (little endian) at 80, reserved `i32`, TOC offset as `u64` from JT 10 on and
  `i32` below that, and the GUID of the scene graph segment. The scene graph
  therefore starts at offset **109** for JT 10.x and **105** for 8.x and 9.x.
* **Segments**: the LSG first, then one shape segment per representation. The TOC
  table is appended at the end.
* **Segment type of a shape**: 7 for JT 8.x and 10.x, 6 for JT 9.x. That is what
  the samples do — and it is per file, not per revision: `SampleJT_Server.jt` is
  a 9.5 file that stores its 106 shape segments as type 7.
* **Segment GUIDs** are synthetic: a `0xfe` marker byte and a counter in an
  otherwise empty GUID, so they cannot collide with a class GUID or with the
  end-of-elements marker.
* **Object ids**: nodes in walk order, then the attributes they refer to, then
  the property atoms — one table for all three, as the scene graph reader
  expects. Properties must **continue** that numbering rather than start at 0,
  because the property table ends a row with the key `0`.
* **Version fields** follow the revision: `u8` from JT 10 on and `i16` below,
  a class version on `RangeLod`, `Shape` and `Instance` only from 9.x on, a
  second type word on an attribute base only from 10.x on, one box on a `Shape`
  from 10.x on and two below, `limits` and `signal` on a `RangeLod` only below
  10.x, and a TOC row of 32 bytes from 10.x and 28 below.
* **Node bodies** follow the per-class field order of the revision: no class
  version in a node base below 9.x, `Part` and `MetaData` carrying their version
  fields unconditionally, `Shape` and `Instance` carrying no child list at all,
  and an `Instance` target as one bare `i32`.
* **Late loads**: a `Shape` gets a `JT_LLPROP_SHAPEIMPL` property pointing at its
  segment GUID with the segment type above.
* **Compression**: flag 2 / algorithm 2 (zlib), like the writer. XZ is read but
  never emitted.

## Rejected graphs

`write_to_bytes` reports `Error::Invalid` rather than writing something a reader
would trip over:

* a `Shape` node with no mesh;
* an `Instance` with no target;
* a cycle — a node that is its own ancestor;
* a node not reachable from the root partition, named in the message;
* a mesh the coder of the revision cannot hold — an open edge, a vertex ring
  that is not a single cycle, a vertex of fewer than three faces, or a degree
  the attribute mask cannot describe. All of those are writable as JT 8.x.

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

* the banner says JT 10.3 by default, little endian, and the TOC has the two
  segments; each supported revision reaches its own banner, its own header
  length and its own shape segment type;
* the classes, names, child lists and instance targets survive;
* at JT 8.x the representation comes back with the strip permutation applied to
  its indices, the same triangles, the same per-slot normals, 36 slots and a
  normal binding of `1`; at 9.x/10.x it comes back through the coder with
  indices `0..36`, no normal binding, and the same triangles in a different
  vertex order;
* one representation is shared by the part and both instances, and each instance
  keeps its own placement;
* `to_model()` gives five nodes, one mesh, twelve triangles and one attachment;
* a **compact** mesh — shared corners — is exploded and comes back triangle for
  triangle, which is the case a pre-exploded fixture cannot catch;
* an open shell is refused at 9.x and 10.x and written at 8.x;
* the bytes are deterministic, and the uncompressed variant decodes to the same
  geometry.

`tests/test_build.py` runs the same proof through the Python layer and adds the
export side: the dump of a built file is valid JSON, and the file converts to
glTF and USD like any sample.

## Open work

* XZ output for the scene graph segment.
* Key/value properties on a node, so a built file can carry `CAD_MASS::` and the
  like instead of only a name.
* Layer filters: a built file declares none, so nothing gates on them.

Writing a neutral `Model` is **not** open work any more — that is
[`jt::from_model`](jt-from-model.md), which builds its own graph here rather than
reattaching the representation ids of a source file.

[`LodGeometry`]: ../crates/ferrojt/src/jt/object.rs
[`JtBuilder::set_version`]: ../crates/ferrojt/src/jt/build.rs
[`JtBuilder::version`]: ../crates/ferrojt/src/jt/build.rs
