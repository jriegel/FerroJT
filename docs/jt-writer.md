# JT writer

> **Status: implemented.** `crates/ferrojt/src/jt/write.rs` rebuilds a `.jt`
> file from a [`JtFile`](../crates/ferrojt/src/jt/model.rs) that was read from
> disk. `ferrojt write file.jt -o out.jt` uses it, and
> `crates/ferrojt/tests/write.rs` pins the round trip. This is the only
> direction *this* writer covers: it rewrites a file it has read and always
> needs a source. Writing a neutral `Model` is a different path, in
> [`jt::from_model`](jt-from-model.md).

## Why only the scene graph is re-serialised

A JT file stores its geometry in one of the quantised or topologically
compressed encodings — Huffman, the range/CDP driver, the length-zone and
angle coders, the mesh coder. The reader decodes all of them; only the two
representations a writer can actually target have an encoder here: the
vertex-based lossless one of JT 8.x and the dual vertex-face coder of JT 9.x and
10.x (`jt/encode.rs`). The quantised coders have none, and nothing in the
reference tooling re-encodes anything at all. So the writer takes the safe route:

* the **logical scene graph** is rebuilt from the parsed graph, because that is
  where a filter has to act;
* **every other segment** — shape representations, meta data, PMI, B-rep — is
  copied out of the source file byte for byte;
* a segment is dropped only when every node that referenced it is gone, which
  is what makes `--all-lods` shrink the TOC table;
* a shape segment is **re-encoded** only when the target revision forces it,
  which is a change of major version — see [Choosing the revision](#choosing-the-revision).

That is also why `write_to_bytes` needs the bytes of the source file: the
banner is reused verbatim and the segments are copied from there.

## Entry points

| Item | Behaviour |
| --- | --- |
| `jt::write_to_bytes(file, data, options) -> Result<Vec<u8>>` | Rebuild the file in memory. `data` is the content of the file `file` was read from. |
| `jt::write_to_file(file, path, options) -> Result<()>` | Same, reading the source again and storing the result. |
| `jt::WriteOptions` | `all_lods`, `no_layer_filter`, `layer_filter`, `compress`, `version`. |
| `jt::JtVersion` | The revision to write: `8.0`, `8.1`, `9.5`, `10.3`; parses from `"9"` or `"10.3"`. |
| `ferrojt write in.jt -o out.jt` | The CLI: writes, re-reads the result and reports both counts. `--jt-version 10.3` picks the revision. |

## Choosing the revision

`version` is `None` by default, which keeps the revision of the source — that is
what makes the rewrite lossless, since the segments in between are copied byte
for byte and their framing is version dependent. Setting it rewrites the file at
another revision, and three rules decide how far that can go.

**The family cannot be changed.** JT 8.x stores its triangles in the vertex-based
lossless representation, JT 9.x and 10.x in the topologically compressed coder,
and a shape of one family does not exist in the other: a vertex-based
representation read by a 9.x/10.x reader comes back with **zero triangles**. So
`8.1 → 10.3` and `9.5 → 8.1` are refused outright, with the message

> `a JT 8.1 file cannot be written as JT 10.3: JT 8.x stores its triangles in the
> vertex based representation, JT 9.x and 10.x in the topologically compressed
> one, and only the coder of the revision of the source file is available`

**Inside a family, the major version decides how much is restructured.**
`9.5 → 10.3` keeps every segment and re-serialises the framing of the scene
graph; the property atoms of the source are parsed with the **source** framing
and re-emitted with the target's. When the major version changes, the shape
segments are re-encoded from the decoded geometry and their segment types are
renumbered, and any segment whose own framing depends on the revision — the info
segment JT 10.x added, for instance — makes the rewrite impossible rather than
silently wrong:

> `the segment <guid> has segment type 31, whose framing depends on the JT
> revision, so it cannot be rewritten as JT 9.5`

**The coder needs a closed two-manifold.** A real CAD assembly has open edges, so
its shapes cannot be re-encoded and the write is refused instead of producing a
file whose shapes come back empty:

> `the representation <guid> cannot be written as JT 10.3: the mesh is not a
> closed two-manifold, so it cannot be written as JT 9.x/10.x`

What a restructure costs in the geometry is worth knowing: the coder keeps one
vertex instance per triangle corner and orders the slots its own way, so the
vertex table of a representation **grows and is re-ordered** while the triangle
count does not change. A 24-corner cube of a 9.5 file comes back as 36 slots at
10.3. Cross-revision geometry is therefore only comparable as a set of triangles,
never as a digest of the index, vertex or normal arrays.

The segment type of a shape is `7 + level` for JT 8.x and 10.x and `6` for 9.x —
and that is a property of the file, not of the revision: `SampleJT_Server.jt` is
a 9.5 file that stores its 106 shape segments as type 7. The renumbering therefore
only runs when the major version actually changes.

## What a rewrite keeps

`Plan::build` walks the graph the way `JtFile::to_model` does, with the same
two filters:

* **Level of detail.** Below a `RangeLOD` only the finest tier keeps its
  representation unless `all_lods` is set — the same positional tier count the
  reader uses, because JT never stores the tier.
* **Layer filter.** A `Part` the filter rejects keeps its node but loses its
  attributes, its representations, its children and its instance target. By
  default the filter the file declares as active is applied;
  `no_layer_filter` writes every part, and `layer_filter` names another one.

Nodes are never dropped, only geometry — the tree of the file keeps its shape.
A subtree reached more than once (an `Instance` and its target) is kept once, so
the output stays an instance instead of turning into a copy.

## Renumbering

The writer emits nodes in walk order, so every object id changes. Nodes and
attributes share one id table, exactly as in the source file, and the property
table is translated with it. Three rules keep that consistent:

1. An attribute referenced by several nodes is **written once**, under the
   first node that refers to it, and numbered once.
2. A table pair survives only when both of its atoms survive. A dropped tier
   takes its late-load property and the pair that carried it with it.
3. A property atom is emitted **exactly when a surviving row refers to it**.
   Numbering and emission must use one rule: when they diverge, writing an
   already written file changes its size, which is the cheapest way to catch
   this.

The last rule has a subtlety. `jt::object::read_element_envelope` reports no
object id for a class it does not know, but a file may still store a property
of another class — `SampleJT_Server.jt` carries 55 of type `10dd1019` — frame
it exactly like a known one, base type and object id included, and refer to it
from the table by that id. `read_property_atom` reads the id out of the atom
instead of trusting the reader, so those properties stay renumberable and their
string values survive the rewrite.

## Framing facts the writer depends on

Read out of the samples, since no reference writer exists:

* Header: 80 ASCII banner bytes, byte order at 80, reserved `i32` at 81..85,
  TOC offset at 85..89 (`i32` below JT 10, `u64` from 10 on), LSG GUID at
  89..105. The new TOC table always goes at the **end** of the file; five of
  the six samples put it right after the header, and both are legal.
* Segment: GUID(16), kind `i32`, size `i32`; when the kind carries a
  compression header, also flag `u32`, data length `i32` and algorithm `u8` —
  33 bytes. `dataLength = size − 33 + 1` when compressed.
* Compression is written as **flag 2 / algorithm 2** (zlib). XZ (flag 3) is
  read but never written.
* Element: `i32 length | GUID | u8 base type | i32 object id | body`, where the
  length excludes its own field — the stride of an atom is `4 + length`. The
  envelope of a known class is 25 bytes, of an unknown class 20. A **non-LSG**
  element carries the object id from JT 9.x on, which is what
  `jt::element_stores_object_id` reports; passing `false` there silently breaks
  the 9.x and 10.x output.
* End of elements: `i32 16` followed by sixteen `0xff`.
* Version fields are `i16` up to JT 9.x and `u8` from 10.x on. Whether a class
  carries one at all is per class: `RangeLod`, `Shape` and `Instance` gate it
  on `major > 8`, while `MetaData` and `Part` have it unconditionally. Getting
  this wrong shifts every later byte and shows up as
  "element body read past its declared end".
* An `Instance` target is **one bare `i32`**, not a length-prefixed vector. A
  wrong target makes `jt::dump` follow a cycle forever.
* LSG body: node and attribute atoms, end of elements, property atoms, end of
  elements, then the table `i16 version | i32 rows | rows × (i32 object,
  (i32 key, i32 value)×, i32 0)`.

## Deliberate deviations

The writer rebuilds the node bodies from the parsed values, so a few fields it
does not model come back as zero where the source had something. None of it is
visible in a dump, because the reader discards each of them:

| Field | Written | Source |
| --- | --- | --- |
| `Partition` flags | `0` | often `1`, which asks for a second (untransformed) box |
| `Partition` area and count arrays | `0` | varies |
| `Part` reserved `i32` | `0` | e.g. `6488065` |
| `Shape` trailing size and compression | `0` | e.g. `9272` |
| `Material` colour flags | `0` — four components each | a file may store one grey value |

## How the round trip is judged

`crates/ferrojt/tests/write.rs` reads the output back and compares it with the
source:

* **lossless** (`--all-lods --no-layer-filter`, and the deflated variant): the
  same segments, the same node and representation counts, the same structure,
  and the same `jt::dump` — the dump being what the reference tooling sees.
  Then the output is written a second time and must come back the same size,
  which is the fixed-point check on the renumbering.
* **filtered**: every node survives, the representations drop to the finest
  tier, the segments of the dropped representations disappear, and every
  property that is not a late load survives.
* **the picture**: the export view of the written file is byte-identical to the
  export view of the source — nodes, meshes, materials and the accessor
  buffers. `tests/test_visual.py` renders the export of a source file against a
  reference picture; the same oracle applied to a written file scores
  1.00000 for all six samples.
* **a revision change**: the banner, the TOC segment types and the representation
  family are checked, the refusal of an impossible change is checked, and the
  triangles of a re-encoded file are compared as a set of welded corner triples —
  the only comparison that survives the coder re-ordering the vertex table.
* **a shape without geometry**: a write that keeps only the finest tier leaves a
  coarse `Shape` node with no late load at all, and its dump must still be valid
  JSON. None of the five reference dumps has such a node, so the byte-for-byte
  parity check cannot catch it and `jt::dump` closes an attribute list on a flag
  instead of a constant comma.

## Open work

* Preserving the source TOC position instead of always appending the table.
* Re-encoding representations, which would let this writer change the
  representation family inside one revision rather than only across a major.

Writing a file from a neutral `Model` — the thing that used to be open work
here — lives in [`jt::from_model`](jt-from-model.md) instead: it builds a new
file through [`JtBuilder`](jt-builder.md) rather than copying segments out of a
source, which is the right trade when the input never was a JT file.
