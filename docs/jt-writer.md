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
  read but never written. A segment whose kind carries a compression header is
  compressed **by default** — `WriteOptions::compress` is `true`, and so are
  `--compress` in the CLI and `compress=` in Python. A strict consumer refuses
  such a segment when the flag says 0, so an uncompressed rewrite of a file that
  stores its scene graph compressed parses here and is rejected there.
* Property atom: `i32 length | GUID | u8 base type | i32 object id | class
  version | u32 state | class version | body`. The class version is `u8` from
  JT 10.x on and `i16` below, and it has to name a version the consumer knows
  for **its own** revision — `1` at 9.x, `2` at 10.x. Zero is fatal: a strict
  consumer reads that value to look the class up and refuses the whole file when
  it names nothing it knows, which is the `Corrupt file detected / Unable in
  find root node` report on a rewritten file.
* **Both the value and the width of that field belong to the revision being
  written, not to the source.** A JT 9.x atom carried into a JT 10.x file is
  re-framed from `i16` to `u8`, in **both** slots, and the atom's own length
  prefix is rewritten with it (`write.rs::set_property_class`). A consumer
  locates the value through the state word `0x4000_0000`, so a word-framed atom
  inside a byte-framed file makes it read two bytes early and give up on the
  rest of the property table: the file parses, `jtinfo` reports a root, and an
  external renderer draws **nothing** — the shape late loads further down the
  table are never reached. The samples prove the rule: the JT 10.3 block stores
  its one atom of the class the reader does not know (`10dd1019`) byte framed
  (length 15) where the JT 9.5 block stores the same class word framed
  (length 17), so a CAD system re-frames it too.
* The same applies to a property of a class the reader does not know — it cannot
  be re-serialised, so the atom is cloned and its object id and **both** class
  versions are patched in place, with the body re-framed and its length prefix
  corrected. Which width the **source** used is read from the state word, never
  inferred from the revision; `element.base_type` is `0` for an unknown class
  and must not be used for framing either.
* Element: `i32 length | GUID | u8 base type | i32 object id | body`, where the
  length excludes its own field — the stride of an atom is `4 + length`. The
  envelope of a known class is 25 bytes, of an unknown class 20. A **non-LSG**
  element carries the object id from JT 9.x on, which is what
  `jt::element_stores_object_id` reports; passing `false` there silently breaks
  the 9.x and 10.x output.
* End of elements: `i32 16` followed by sixteen `0xff`. A **non-LSG** segment
  closes with that marker **and** a property table of its own — `i16 version 1`
  then `i32 0` rows — six bytes from JT 9.x on. A shape segment written without
  it reads back with six bytes left over.
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
* The `Shape` vertex binding tail is framed by the revision **being written**,
  not copied from the source: three `i32` bindings plus four quantization bytes
  at 8.x (16 bytes), an `i16` version plus an 8-byte binding plus quantization
  plus a second binding from version 2 on at 9.x (22 bytes, quantization at
  offset 10), and a `u8` version plus one 8-byte binding at 10.x (9 bytes). A
  rewrite that changes revision and keeps the source layout leaves 22 bytes
  where a consumer expects 9, and every field after it — and every node after
  it — is read from the wrong offset. `write_shape_lod` re-frames it;
  `a_shape_tail_is_re_framed_for_the_revision_being_written` pins the three
  lengths.
* A CDP data package never uses codec `0` (null) unless it is **empty**. Every
  real file stores codec `1` (bitlength2) for its coordinates, normals, indices,
  valences and groups, and a consumer that renders refuses a package whose codec
  it did not expect. `package2` therefore always writes codec 1 and encodes the
  values itself.
* The JT 9.x **fixed-range** form of bitlength2 cannot carry a span of 2³¹ or
  more. The decoder reads `max − min` as a *signed* integer and fills the whole
  package with the minimum when that value is not positive, so a package holding
  raw IEEE bit patterns — which is what `package2_words` feeds in for a
  coordinate — silently decodes as a run of identical values. Such a package
  goes down the **variable** branch instead (`write_variable_bits`), which the
  same decoder reads correctly at 9.x. `every_package_we_write_decodes_back_with_its_own_codec`
  round-trips every package the writer emits through its own decoder at both
  revisions, which is how the overflow was found.
* A topological shape whose vertex binding announces a flag bit must actually
  carry the per-normal **flag package** after the normals. The OpenJT reader
  ignores that section, a rendering consumer stops on the missing field and
  draws nothing.
* A **context-7 attribute mask** is widened by two **auxiliary streams**, and
  both need one value per mask symbol even when no bit above 29 is ever set.
  `mask_context(degree) = (degree − 2).clamp(0, 7)`, so context 7 means a coder
  vertex of degree ≥ 9; the decoder indexes `_faceAttributeMask8_30` and
  `_faceAttributeMask8_4` with the *same* read position as the mask itself and
  ORs both at bit 30. `encode.rs` keeps them in `Streams::attr_masks_high`,
  pushes a `0` for every context-7 symbol, and emits them after the eight degree
  packages — two packages at 9.x, one at 10.x, where the second is never read.
  A consumer that renders refuses a context-7 face whose auxiliary streams are
  empty, so the whole shape comes out blank. A cube (degrees 4 and 6) and an
  octahedron (degree 4) can never reach context 7, and the screws of
  `opening_protection_plate1` are the only degree-9 coder vertices in the
  samples — which is why every block rewrite rendered and only the plate did
  not. `a_context_seven_face_carries_both_auxiliary_mask_streams` pins it on a
  nine-gon bipyramid, whose two apices have degree 9.
* A JT 10.x topological shape wraps its coder input in a **second, embedded
  logical element**, immediately after the first vertex binding:
  `i32 length | GUID | u8 base type 9 | i32 object id 1 | u8 version 1 |
  u32 vertex-records id 0 | u8 flag 1` — 31 bytes
  (`encode::TOPOLOGY_EMBEDDED_HEADER`), with the coder input starting 31 bytes
  after the length field. At 9.x the same three last fields sit there **loose**
  and unframed. The embedded length stops exactly one byte before the outer
  element closes, because the caller appends the version byte that closes it:
  `length = outer − 1 − (at + 4)`. The mis-framed header has the **same total
  width**, so a lenient reader, a lenient renderer and `jtinfo` all accept the
  file and a strict one draws nothing. The reader rejects a foreign GUID
  outright (`object::read_shape_lod_data`). That GUID, `ada530f8-4cbe-bc4f-9b5f-
  b9269278d2e1`, appears in no specification, in no file of `example/` and in no
  `guid::types` — it is taken from the sample bytes and is a named constant in
  both `encode.rs` and `object.rs`. `write_topological_body` also writes **two**
  version fields before the first vertex binding, word-wide at 9.x and byte-wide
  at 10.x, so the coder input sits at body offset 18 at 9.x and 41 at 10.x.
  `a_ten_x_shape_wraps_its_coder_input_in_an_embedded_element` and
  `a_nine_x_shape_carries_no_embedded_element` pin both.

## The hashes a strict consumer verifies

A JT file carries three checksums per topological shape, and a strict consumer
recomputes all of them and **refuses to draw the shape** when one disagrees — it
does not warn, and it does not fall back. The primitive is Bob Jenkins' lookup2
hash (1996), published in the format specification itself; `jt/hash.rs` holds it
(`hash32` over `u32` words, `hash16` over `u16` shorts, `composite_hash`). The
spec prints `hash()` and `hash3()` but never the body of `hash2()`, so the word
form is pinned by evidence: three words per `mix`, the word count added **after**
the main loop and **before** the tail switch, a one-word remainder feeding `a`, a
two-word remainder feeding `a` then `b`, a final `mix`, return `c`. It reproduces
the stored value of every shape of every sample.

* The **Composite Hash** of the topology is a chain of `hash16` calls over the
  symbol arrays, and the chain is **revision dependent**: eight degree arrays,
  valences, groups, the flag array, attribute masks 0..6, the low and high 32
  bits of mask 7, the large masks, the split symbols and the split positions —
  eleven calls at 9.x and **ten** at 10.x, where the `mask8_4` region is not
  hashed. The vertex records are not part of it. The flag is
  `MeshInput::auxiliary_pair = cursor.major() < 10`, and the reader verifies the
  same chain it reports. Reproduced for all twelve CAD shapes of the samples.
* The **vertex-data hashes** of the coordinates and normals are revision
  dependent in their **granularity**, not their content: JT 9.x feeds one word
  per `hash32` call and chains the seed through, JT 10.x feeds one whole
  component array per call. Component-major, record-minor, over the **unique**
  coordinates. `encode::attribute_hash` implements both; verified 6/6 at 9.5 and
  6/6 at 10.3. Getting only the content right — feeding one array at both
  revisions — reproduces the 10.x value and blanks every 9.x shape.

## Deliberate deviations

The writer rebuilds the node bodies from the parsed values, so a few fields it
does not model come back as zero where the source had something. None of it is
visible in a dump, because the reader discards each of them:

| Field | Written | Source |
| --- | --- | --- |
| `Part` reserved `i32` | `0` | e.g. `6488065` |
| `Material` colour flags | `0` — four components each | a file may store one grey value |

Everything else in a `Partition` or `Shape` node is round-tripped now, because a
consumer outside the workspace reads it: the partition's flags, name, box, area
and counts (with the second box only where the revision stores one, i.e. when
flag bit 0 is set below JT 10), and the counts, size, compression and vertex
binding tail of a shape node. The binding tail is revision-specific — three `I32`
bindings plus quantization parameters at 8.x, a version, a binding and the
quantization parameters at 9.x (plus a second binding from version 2 on), and a
version plus one binding at 10.x — and it is copied verbatim, since a coarse
level of detail of a sample quantizes with `11 06 0b 06` where zeros would be a
lie. `jt::dump` prints none of it, so no reference dump had to change.

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
  the only comparison that survives the coder re-ordering the vertex table. The
  class version of every property atom is checked too, in the bytes of the scene
  graph rather than through the parsed graph, because a property of an unknown
  class never reaches the parsed graph at all. The check covers the **width** of
  that field as well as its value, and
  `an_unknown_property_class_is_re_framed_for_the_revision_being_written` runs it
  on the single atom of a class the reader does not know that the JT 9.5 block
  carries.
* **a shape without geometry**: a write that keeps only the finest tier leaves a
  coarse `Shape` node with no late load at all, and its dump must still be valid
  JSON. None of the five reference dumps has such a node, so the byte-for-byte
  parity check cannot catch it and `jt::dump` closes an attribute list on a flag
  instead of a constant comma.
* **an external consumer**: everything above is judged by a reader that tolerates
  a lot. `target/jtsweep.py` writes every sample at every legal revision, every
  glTF conversion at every revision and a hand-built chain at each of the four,
  and hands each result to `jtinfo.exe` from the JT tooling. A file counts as
  accepted only when it reports at least one assembly — `0` means the file parsed
  but its root was lost, which is a failure. Four rejections found that way are
  pinned above: an uncompressed capable segment, a property atom naming class
  version zero, a property atom of a restructured write naming the class version
  of the source, and a root `Partition` with an empty file name. All 46 outputs
  pass now. The number of properties `jtinfo` prints is **not** part of the
  verdict: a good JT 9.5 rewrite reports 18 where a good 10.x rewrite reports 9
  and a converted file reports 0, and all of them render.
* **an external renderer**: `jtinfo.exe` reports a triangle count taken from the
  key/value meta data, not from the geometry, so accepting a file says nothing
  about whether anything can be drawn from it. The geometry oracle is a viewer
  that actually renders: `node render-jt.mjs --input <file.jt> --output <png>
  --view iso`. A file with no drawable geometry does not report an error — it
  parses, resolves, and then **hangs** until the stream times out and no PNG is
  written. `target/novorender.py` runs a list of files through it and prints
  the count of non-background pixels per picture, which is how the codec,
  flag-field, shape-tail, auxiliary-mask and instance defects above were
  confirmed fixed. A written file has to measure what its source does:

  | Input | Written | Ink pixels |
  | --- | --- | --- |
  | `example_block` at 9.5 or 10.3 | either revision, all levels, no filter | 590 296 |
  | `opening_protection_plate1_jt9.5` | 9.5 and 10.3, rewriter and model path | 291 167 |
  | `SampleJT_Server.jt` | 9.5, all levels, no filter | 356 011 |
  | `TestBlock.glb` converted to JT | 10.3 | 628 416 |

  One representation of the plate — a 152-triangle screw — measures 207 109 on
  its own, and that is the fixture the encoder bisection ran on.
* **the strict consumer**: that renderer is **lenient**, and so is `jtinfo`. Both
  accepted every file that Siemens JT2Go drew blank, so "it renders here" proves
  nothing about a real consumer. `tests/jt2go_picture.py` is the strict oracle:
  it drives an installed JT2Go through `ctypes`, waits for the MDI document,
  captures the `DMUtils` drawing surface with `PrintWindow(..,
  PW_RENDERFULLCONTENT)` and counts the pixels differing from the background.
  Three outcomes, never to be conflated: a large ink count, **352 px** (the file
  loads and the shape is rejected — 352 is the trihedron icon alone), and no MDI
  document at all after 120 s (the file is structurally refused). It is the only
  oracle that caught the composite hash, the vertex hash granularity and the
  embedded element defects above, and `tests/test_jt2go.py` runs the three writer
  paths — rewrite, model path and hand-built chain — through it. Baselines: the
  block at every revision and every path **387 467**, the plate **226 982**
  (226 971 for the 8.0 → 8.1 rewrite), a hand-built cube and a `TestBlock.glb`
  conversion **352 001**, the server sample written at 9.5 **143 741**.

## Open work

* Preserving the source TOC position instead of always appending the table.
* Re-encoding representations, which would let this writer change the
  representation family inside one revision rather than only across a major.

Writing a file from a neutral `Model` — the thing that used to be open work
here — lives in [`jt::from_model`](jt-from-model.md) instead: it builds a new
file through [`JtBuilder`](jt-builder.md) rather than copying segments out of a
source, which is the right trade when the input never was a JT file.
