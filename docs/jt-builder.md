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
| `add_node(parent, class, name)` | Add a `Partition`/`Part`/`MetaData`/`Group`/`Shape`/`Instance`/`RangeLOD` node. |
| `add_detached_node(class, name)` | Add a node no child list refers to, for an `Instance` to point at. |
| `add_shape(parent, name, mesh)` | Add a `Shape` node holding a mesh. |
| `instance_of(parent, target)` | Share the subtree of `target` instead of copying it. |
| `add_mesh(mesh)` / `set_mesh(node, mesh)` | Keep a mesh without a node, then attach it. |
| `set_transform` / `set_translation` / `set_placement` / `set_node_transform` | The transform of a node. |
| `set_material(node, material)` | A JT material attribute on a node. |
| `set_version(version)` / `version()` | The revision to write, `JtVersion`; default `10.3`. |
| `set_file_name(name)` / `file_name()` | The name of the root partition — see [naming the root](#naming-the-root-partition). An empty string clears it. |
| `write_to_file(path)` / `write_to_bytes()` | The complete file, DEFLATEd unless `compress` is off. |
| `ferrojt::jt::JtBuilder` in Python | `JtBuilder`, `NodeHandle`, `MeshHandle`, `MeshData`, `MaterialData`, `builder.version` as a readable and assignable string, and `builder.file_name` / `builder.set_file_name()`. |

Handles are opaque: `JtNode(usize)` and `JtMesh(usize)` have private fields, so
outside the crate they are tokens, and every operation checks that a token
belongs to the builder it is used with.

## The chain a consumer walks

A consumer that builds a scene graph of its own does not look for shapes under
the root partition — it walks down to a **part** and reads the geometry under the
level-of-detail span of that part. Every sample stores it the same way:

```text
Partition -> MetaData -> Instance -> Part -> RangeLOD -> Group -> Shape
```

Two details of that chain are easy to get wrong, and both are what
`a_level_of_detail_span_can_be_built_by_hand` pins:

* The `Part` is **not a child** of the node above it. An `Instance` refers to it
  through the target of its element and carries the placement, so the part is
  created with `add_detached_node` and must be named by some `instance_of` — a
  node nothing refers to is reported as unreachable.
* The `RangeLOD` hangs under the **part**, not under the instance: an instance
  node has no children at all, which `add_node` refuses.

A `RangeLOD` element is written byte for byte as the samples store it — the
second version field only from JT 9.x on, then the centre, the signal and the
triangle-count limits. The tier of a span is a plain `Group`; JT never encodes
which tier a group is, the position under the span is the tier.

The builder does **not** insert that chain on its own: `add_shape(root, …)`
produces `Partition -> Shape`, which every reader in the workspace accepts and a
strict consumer refuses with the same *"Unable in find root node"* an unnamed
partition gets. Assembling the chain is the caller's job —
[`jt::from_model`](jt-from-model.md) does it for a neutral `Model`, and a
hand-built graph has to do it itself. `target/build_chain.py` is the proof that
the hand-built form loads: the same cube passes at 8.0, 8.1, 9.5 and 10.3 once
the chain is there and fails at nothing before it is.

## Naming the root partition

A root `Partition` carries a file name, and it is not decoration. The reference
assistant builds its own scene graph and creates the root node **only when that
name is non-empty** — `JTData_SceneGraph.cxx:152-176` guards `PushNode` with
`if (!aPartitionRecord->FileName().isEmpty())` and then asserts that a root node
exists at all. A consumer of that shape refuses an unnamed file with something
like *"Bad file detected … Unable in find root node"*, which is what a built
file used to look like to a strict importer.

So the builder writes the name twice, exactly as the samples do:

* as the `file_name` string of the `Partition` struct itself, and
* as a row in the scene graph property table for object id `0`, holding
  `PartitionType = "Assembly"` and `JT_PROP_NAME = <name>`.

Where the name comes from depends on how the file is produced:

| Call | Root name |
| --- | --- |
| `write_to_file(path)` | the stem of `path` with a `.jt` extension, unless `set_file_name` said otherwise |
| `write_to_bytes()` | whatever `set_file_name` set, else the fallback `model.jt` |

The fallback is a real name rather than an empty string so that even a
from-nothing `to_bytes()` produces a loadable file. The samples put a CAD
assembly name (`example_block_nx1872.asm;0;16777215:`, `san666.asm;0;0:`) in the
property and the `.jt` name in the struct field; we write the **same** `<stem>.jt`
in both, which the reader tolerates because `Node::bind_name` keeps only the first
`;`-separated token of a name.

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
per-vertex attribute masks. A mask of **context 7** — a coder vertex of degree
9 or more — is widened by auxiliary streams that carry one value per mask
symbol: two of them at JT 10.x (bits 0..31 and 32..63), three at 8.x/9.x
(bits 0..29, 30..59 and 60..63). None of them may be left empty; see
[framing facts the writer depends on](jt-writer.md#framing-facts-the-writer-depends-on).
The strip offsets go through the null codec (byte 0)
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
* **Property table**: the root partition's row goes first, so the name of the
  file is the first thing a consumer that looks up object id `0` finds. A
  property atom carries a class version from JT 9.x on — `2` at 10.x, `1` at
  9.x — between its object id and its state word, and again after it. Neither
  may be zero: a strict consumer reads the first one to look the class up and
  refuses the whole file when the value names nothing it knows, which is why
  `property_class` derives it from the revision instead of leaving it out.
* **Shape node tail**: after the trailing size and compression, the vertex
  bindings of the representation follow in a revision-specific layout — three
  `I32` bindings plus quantization parameters at 8.x, a version, a 64-bit
  binding and quantization parameters (plus a second binding from version 2) at
  9.x, and a version plus one binding at 10.x. The reference reads all of it, so
  a writer has to store it; `ShapeLod` in `jt/object.rs` is the parsed form.
* **Segment trailer**: a non-LSG segment ends with the end-of-elements marker
  **and** a property table of its own — `i16 version 1`, `i32 0` rows — six
  bytes from JT 9.x on.
* **Compression**: flag 2 / algorithm 2 (zlib), like the writer. XZ is read but
  never emitted. The `dataLength` of a compressed segment counts the algorithm
  byte, so it is the compressed length **plus one**.

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
| `Partition` name | `<stem>.jt` in both the struct field and `JT_PROP_NAME` | a CAD assembly name in the property, the `.jt` name in the field |
| `Partition` flags | `1`, asking for a second box — which JT 10.x then does not store | the same at 10.x, a real second box at 8.x and 9.x |
| `Partition` area and count arrays | summed area of the shapes below, and the node range of the written graph | varies; the count range is instance-expanded in a CAD file |
| `Partition` box | the box of every mesh below it | the same, usually |
| `Shape` vertex bindings | the binding of the representation being written | the same |
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
* the root partition **names the file**: an explicit `set_file_name` reaches both
  the struct field and the `PartitionType`/`JT_PROP_NAME` property row, a
  `write_to_file` takes the name of the path it writes to, and a bare
  `write_to_bytes()` falls back to `model.jt` — never an empty name;
* the partition's flag, box, area and counts read back as written;
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
* Structure: a built file is `Partition` over `Shape` nodes. A CAD file puts a
  `MetaData` node under the partition and wraps each part in
  `Part` → `RangeLod` → `Group`, and it carries a type-4 Meta Data segment, a
  PMI segment and an info segment in the table of contents. Nothing in this
  workspace needs those, but a consumer that expects a `Part` under the root will
  not find one.
* Segment GUIDs are counters, not UUIDs, so a file cannot be merged with another
  by GUID.
* The `size` at the end of a `Shape` node stays `0`. Every reader in the
  workspace ignores it and so does the reference; a CAD file stores the length of
  its representation there.

Writing a neutral `Model` is **not** open work any more — that is
[`jt::from_model`](jt-from-model.md), which builds its own graph here rather than
reattaching the representation ids of a source file.

[`LodGeometry`]: ../crates/ferrojt/src/jt/object.rs
[`JtBuilder::set_version`]: ../crates/ferrojt/src/jt/build.rs
[`JtBuilder::version`]: ../crates/ferrojt/src/jt/build.rs
