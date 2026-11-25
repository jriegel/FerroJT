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
| `ferrojt.JtFile.lsg` | The same native view as python objects — `JtLsg`, `JtNode`, `JtMesh`, `JtArray`; see "The file view" below. |

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
   `Primitive::attributes` holds the returned handles. A representation that
   declares no vertices at all contributes no mesh: glTF needs a `POSITION`
   attribute in every primitive, so there is nothing to write. The node that
   holds it is kept, exactly as for a filtered part or a coarse tier — the graph
   keeps its shape, only the drawing is skipped. No reference sample reaches
   that case any more: the shapes of `SampleJT_Server.jt` that used to come back
   without vertices were a decode failure, not empty shapes.
4. Only the finest level of detail of a range LOD *that has geometry*. A
   `RangeLOD` lists its children from the finest tier down and JT does not store
   that tier, so it is counted while walking: the first group below a `RangeLOD`
   is tier 0, the next tier 1, and so on. A shape at a coarser tier contributes
   no mesh, which is what the reference glTF writer does — with one deliberate
   deviation: a tier whose shape declares no vertices at all (an empty LOD0,
   which real CAD files do write) does not settle the span, so the next tier
   that really has vertices is the one that is kept. A consumer displays the
   part from that tier, while a strict "tier 0 only" export would draw nothing.
   The nodes themselves are all kept, so the graph keeps its shape, and
   `Mesh::lod_level` records the tier a mesh came
   from. `jt::dump` is not filtered — a dump still reports every tier.
5. An `Instance` node keeps the subtree it points at. JT stores an instantiated
   part once and refers to it from several `Instance` nodes — the plate samples
   hold one screw and instantiate it twice, at `y = -25` and `y = +25` — so
   every graph position is translated exactly once and both instances end up as
   children of the same model node. That is what glTF encodes as one node listed
   by two parents: the geometry is shared, the instance transforms are not. A
   subtree reached twice resolves to the node already built rather than being
   copied, which also breaks a reference cycle.
6. `bounds` filled wherever the file provides them; otherwise left `None` so the
   exporter can compute them.
7. `Model::bounds` as the union of the root nodes.
8. `Model::extras["layer_filter"]` naming the filter the export applied, when
   the file actually names an active one; see below.
9. One `Material` per distinct appearance, reached from
   `Primitive::material`; see the materials section below.
10. `Node::extras["properties"]` holding every key/value pair of the meta data
    stream the node's late loads point at — `Name::`, `CAD_MASS::`,
    `Translation Date::`, `LAYER::`, the `LAYERFILTERnnn::` records and
    whatever else the CAD system wrote. The pairs come from
    `jt::key_value_pairs`, are stored as a sorted map, and stay `None` when the
    stream is empty. Neither exporter emits them, so they are there for a
    consumer that wants them — the viewer's detail pane shows them.

## Fields the reader keeps for a writer

`jt::object` used to discard the summary fields of a root `Partition` and the
vertex description tail of a `Shape`, which meant a rewrite could not reproduce
them and a strict consumer got zeros where the source had values. Both are parsed
now:

* `Partition { flags, file_name, bounds, area, counts, untransformed_bounds }`.
  Bit 0 of `flags` announces the untransformed box, which JT 8.x and 9.x store
  after the counts and JT 10.x does not — so `untransformed_bounds` is `Some`
  only when the flag is set *and* the revision is below 10, and a writer must
  apply the same second condition or every later byte shifts.
* `Shape { counts, size, compression, lod }`, where `lod: ShapeLod` is the
  revision-specific tail after the compression ratio: `Vertex1` (three `I32`
  bindings plus quantization parameters) at 8.x, `Vertex2` (a version, a 64-bit
  binding, the quantization parameters and a second binding from version 2 on) at
  9.x, and `Binding` (a version and one binding) at 10.x. The bindings and the
  quantization parameters are kept verbatim — a coarse tier of a sample quantizes
  with `11 06 0b 06`, so zeros are not a stand-in.
* `size` is stored because the source stores it. The reference reads it and never
  uses it, and nothing in this workspace does either.

None of it reaches `jt::dump` or the neutral `Model`, so the reference JSON dumps
and the pinned export counts are unchanged by it.

## Codec packages

`jt::decode::cdp` reads the integer codec packages of the vertex coders and the
mesh coder in both framings (mark 1 `loadCDP1`, mark 2 `loadCDP2`). Two framings
of real CAD exports were missing and cost a whole Teamcenter export corpus its
geometry:

* **Codec id 5 is move-to-front.** A mark 2 package of that id holds two complete
  nested packages — the window *values* and the window *offsets*. A negative slot
  escapes to the next stored value, a slot in the window selects it, and selecting
  moves the value to the front of a window whose capacity is 16. The window starts
  empty, so a slot for a slot not yet filled escapes too; that is what makes an
  offset stream such as `-1, 16, 1, 0, 1, 2, 3` decode to the values first stored
  in slots 4 and 5. TKJT cannot read such a package either.
* **The high-degree face attribute masks are a plain array.** The
  *High-Degree Face Attribute Masks* stream of the mesh coder — between the split
  stream masks and *Split Face Syms* — is `i32` count plus that many `u32`, with
  **no codec byte**, while every sibling is a codec package. TKJT reads it as a
  package, so any file with a face of more than 64 edges desynchronises there:
  the next package header reads as garbage (commonly `codec 255`) and the whole
  shape is lost. An empty array frames identically under both readings, which is
  why no sample in `data/ExampleFiles` exposes it.

A decoded topology also carries a composite hash, and `jt::decode::mesh` verifies
it against the symbols (`TopologySymbols::composite`). A mismatch is reported as a
warning naming both hashes — the strongest available check that a stream was read
with the right framing, since a wrong reading of any package changes the hash.

Last, a file can be internally inconsistent: the topology of a shape may name a
vertex index at or just past the end of the coordinate array the file itself
declares (98 of the 9,302 files of one Teamcenter export do it, usually exactly
`index == unique_vertex_count`). The C++ reference walks off the end there,
because `JtData_Vector::operator[]` is unbounds-checked. FerroJT never reproduces
that read: `Decoded::get` returns 0 for an index outside the array, and
`object::read_topologically_compressed_data` counts the affected components and
reports them with one warning per shape. The rest of the shape survives.

## Materials and colours

A JT file stores a fixed-function colour set — ambient, diffuse, specular,
emission, a shininess exponent and a reflectivity — on a `Part` or `Shape`
node as attributes, and repeats the same set on every shape below that part.
The reader maps it once per distinct set:

| JT attribute | Neutral model | glTF |
| --- | --- | --- |
| `MaterialDiffuseColor` | `Material::base_color` | `pbrMetallicRoughness.baseColorFactor` |
| `MaterialEmissionColor` | `Material::emissive` | `emissiveFactor` |
| — | `metallic` = `0.2` | `pbrMetallicRoughness.metallicFactor` |
| — | `roughness` = `0.8` | `pbrMetallicRoughness.roughnessFactor` |
| — | `double_sided` = `true` | `doubleSided` |
| `MaterialAmbientColor`, `MaterialSpecularColor`, `MaterialShininess`, `MaterialReflectivity` | `Material::extras` | `extras` of the material |

The two fixed factors are what the reference glTF writer of `example/` uses
(`ToolsLib/src/JtGltfMesh.cpp`), and they matter: JT has no metallic/roughness
workflow, and the samples all carry a shininess exponent of 128. Feeding such
an exponent to glTF as a roughness makes every part a near mirror, and under
the default lighting of a viewer the colour blows out to white — which looks
exactly like a colour that was never exported. The exponent is therefore kept
in `extras` for a consumer that can use it, and the geometry is double sided
because a JT part is not guaranteed to have consistent winding.

`Model::add_material` stores one entry per distinct material, so the plate
samples export two materials even though they repeat the colour attribute
twelve times; the reference writer appends one per mesh instead, which is a
harmless deviation — the same appearance, fewer entries.

## Layer filters

JT keeps a lightweight layer model in the key/value streams of the meta data:
`LAYERFILTERnnn::` declares a named filter and the layers it accepts, and
`ACTIVELAYERFILTER::` names the one the CAD system had selected when the file
was written. Every `Part` can carry a `LAYER::` tag with the layers it belongs
to. The JT Layer tool in `example/` reads those streams with its own ad-hoc
scanning, and FerroJT mirrors the result:

* `SceneGraph::layer_info()` returns the declared filters plus the active name,
  and `SceneGraph::layers_of(node)` the layers a node is tagged with. Both read
  the same concatenated `ProxyMetaData` streams the JSON dump reports, so the
  dump and the export never disagree about what the file says.
* `JtFile::layer_filter(name)` resolves a filter. Omitting the name yields the
  filter named by `ACTIVELAYERFILTER::`, and **nothing at all** when that value
  is empty or names no declared filter — the same rule the reference glTF writer
  uses (`ToolsLib/src/JtGltfMesh.cpp`: a filter is applied only when
  `ActiveLayerFilter` is non-empty). Falling back to the alphabetically first
  declared filter, as the JT Layer tool's combo box does when its `findText`
  fails, is a user-interface habit and not a file semantic: real exports (for
  example Teamcenter) name a publication or wireframe filter first, and applying
  it silently hides the geometry the CAD viewer itself draws. An unknown name is
  an `Error::Invalid` that lists the available filters.
* `build_node` gates `Node::Part` only: a part whose tag shares no layer with
  the filter contributes no geometry and no children. Everything else —
  `Partition`, `MetaData`, `LOD`, `RangeLOD`, `Group`, `Instance` — is never
  gated, and a part without a tag is always kept. The node itself is always
  kept, so the scene graph keeps its shape even when a part is filtered out.
* `to_model()` applies that default, `to_model_with_filter("NAME")` a chosen
  one, and `to_model_unfiltered()` none at all. The effective filter is recorded
  in `Model::extras["layer_filter"]` as `{ "name": …, "layers": […] }`.
* `jt::dump` is not filtered: the JSON export always reports every filter, every
  tag and every part, whatever the export decides to do.

Of the samples, the JT 8.x files declare no filter at all, and the JT 9.5/10.3
ones declare `ALL` and `LAYCUR` with an empty active name — so none of them
applies a filter by default, and no part of any sample is ever lost to its own
layer model. Naming a filter explicitly (`to_model_with_filter("LAYCUR")`) does
gate parts, which is what the synthetic fixture in `jt/model.rs` covers. Real
CAD exports are the case that matters: a Teamcenter export typically declares
`DMU (DMU)`, `Additional Publications` or a `[WIREFRAME]` variant and leaves the
active name empty, so the default export shows the whole assembly while
selecting one of those names deliberately hides parts of it.

## The file view (`JtLsg` / `JtNode` / `JtMesh` / `JtArray`)

`Model` is the **export** view: one node tree, finest LOD tier only, the layer
filter already applied, JT names normalised. `ferrojt.JtFile.lsg` is the **file**
view — the decoded `SceneGraph` itself, exposed as python handles by
`crates/ferrojt-python/src/lsg.rs`. Nothing is filtered and nothing is renamed:
every LOD tier, the JT class name, the whole key/value stream, the layer tags and
the attributes are reachable, which is what a python script that wants to pick
and choose needs.

* `JtLsg` is the handle on the parsed file: `root()`, `nodes()` (every node in
  file order), `meshes()`, `mesh(id)`, `node_count`, `representation_count`.
* `JtNode` is a position in the graph, not a copy of a subtree. It iterates
  (`len()`, `node[i]` with negative indices, `for child in node`), compares and
  hashes by that position, and reports `class_name`, `id`, `name`, `version`,
  `flags`, `is_instance`, `is_tri_strip_set`, `layers`, `attribute_names`,
  `properties()`, `material()`, `late_loads()`, the transform as `matrix` /
  `transform` / `translation` / `rotation` / `scale`, plus the per-kind fields
  `bounds`, `area`, `lod_limits`, `lod_signal`, `lod_center`, `up_axis` and
  `partition_file_name`. Indexing and iteration keep a child reference that does
  not resolve to a node — it comes back as `None`, and
  `unresolved_children()` counts them; `children()` skips them.
* Every handle keeps the file alive, so a node or an array stays valid on its own
  and a NumPy view of an array keeps the whole file readable.

### Representation ids

A JT shape node does not own its geometry, it late-loads it, and an `Instance`
refers to a subtree that is stored once. So the geometry is keyed by **position
in the representation list of the file** — `SceneGraph::loaded`, resolved by
`model.rs::loaded_pos`, which matches on the late-load ids so an `Instance` and
its target share one id. That id is the instancing key:

* `node.mesh_ids` is the geometry of that node alone — empty for an `Instance`.
* `node.subtree_mesh_ids()` is everything below it, so the two placements of one
  screw report the same ids and the geometry is read once.
* `meshes()`, `mesh(id)` and `representation_count` only list entries that
  actually carry geometry, because `SceneGraph::loaded` also holds meta data late
  loads. The ids are **not** `0..count`: block 9.5 has three representations,
  `[4, 5, 6]`, and `mesh(0)` is an `IndexError`.
* Unlike `to_model()`, the file view **keeps** a representation with no vertices:
  glTF mandates a `POSITION` attribute and drops it, a python consumer may still
  want to see it. No reference sample has such a representation today — the ones
  in `SampleJT_Server.jt` were a decode failure that is now fixed — so all 106
  representations of that sample carry geometry and its export view has 106 meshes.

### Zero-copy arrays

`mesh.array("indices" | "vertices" | "normals")` returns a `JtArray` that
implements the buffer protocol directly over the decoded `LodGeometry` of the
file — no intermediate `bytes`, no copy, no C++ dependency:

| Channel | Shape | Format | Strides |
| --- | --- | --- | --- |
| `indices` | `(m,)` | `int32` (`"i"`) | `(4,)` |
| `vertices` | `(n, 3)` | `float32` (`"f"`) | `(12, 4)` |
| `normals` | `(n, 3)` | `float32` (`"f"`) | `(12, 4)` |

`memoryview(a)` and `numpy.asarray(a)` both borrow: `flags.owndata is False`, and
`np.shares_memory` holds between two calls. The view is always read-only —
requesting `PyBUF_WRITABLE` is a `BufferError`, because the arrays of a JT file
are what the file says they are. An **empty** channel still hands out a
zero-length buffer, pointed at a static stand-in since a `Vec` of length zero has
a null pointer: returning nothing would make `numpy.asarray` fall back to a 0-d
**object** array instead of an empty `float32`. `len(a)` is the number of rows, so
it agrees with `len(memoryview(a))` and `len(np.asarray(a))`.

Python 3.11 is the floor for this: the stable ABI only gained the `bf_getbuffer`
slot then, so pyo3 compiles `__getbuffer__` out below it.

## Constraints

* No `unsafe` — `crates/ferrojt` is `#![forbid(unsafe_code)]`.
* Use `codec::ByteOrder` explicitly; never assume host endianness.
* Use `codec::BitReader` for the packed header fields (MSB first).
* Use `codec::inflate` for DEFLATE compressed segments and `codec::decompress_xz`
  for the XZ envelopes JT 10.x uses.
* Every malformed byte sequence must return `Error::Invalid { offset, message }`
  with the offset of the offending field, never panic and never read past the
  buffer.
* Two Siemens specific segment kinds are not decoded: kind 23 (`"STT"`, the
  structured tessellation a JT 10.5 export writes) keeps its payload XZ encoded,
  because that kind carries no compression envelope — the reference reader skips
  the envelope there too — and kind 17 (`"XT B-rep"`) does decompress, but its
  TLV framed objects are not parsed. A shape whose representation lives in such a
  segment keeps its node, loses that tier's geometry, and the level of detail
  fall-through takes the tier below it. Out of 9 302 exported files in a
  Teamcenter sweep exactly two were affected, and in both the tier below LOD0
  drew the part.
* Truncated files must be detected before a slice is indexed.
* A single damaged element must not lose the file: the reader logs a warning and
  skips to the end of the element, the way the reference implementation does.

## Tests

* `crates/ferrojt/tests/samples.rs` — every sample opens, the TOC count and the
  vertex/index counts of every representation are pinned, and the root is a
  `Partition`.
* `crates/ferrojt/tests/model.rs` — a converted model holds one mesh per range
  LOD, tagged `lod_level: Some(0)`, keeps every node of the graph, keeps the
  subtree of every `Instance` and lets two instances share one, applies the
  layer filters the samples declare without losing a part of them, carries the
  diffuse colours of the file — both in the model and in the exported glTF — as
  one material per distinct colour, and exports to glTF for every sample: each
  primitive of each one has a non-empty `POSITION` attribute, which is what
  a shape whose topology cannot be decoded used to break.
* `crates/ferrojt/tests/dump.rs` — every sample with a reference dump under
  `data/Reference` matches it semantically, and every dump is valid JSON.
* Unit tests in `jt/decode` compare the codecs against the reference output byte
  for byte.
* Unit tests in `jt/model` build a synthetic scene graph, because no sample has
  a part on a layer its filter rejects or a representation without vertices.
* `tests/test_ferrojt.py` — the same reference dumps through the python bindings,
  plus the file view: the node list and class names of a sample, its
  representation ids, two `Instance` nodes sharing one representation, the JT 8.x
  tier names carried by the segment type, and the zero-copy contract of `JtArray`
  (`owndata is False`, `np.shares_memory`, read-only).
* `tests/test_visual.py` — the export of every sample — the three
  `example_block` versions, the two `opening_protection_plate1` ones and
  `SampleJT_Server.jt` — is rendered offscreen and compared with a reference
  picture in `data/Reference`, so a regression in the geometry shows as a
  picture rather than only as a count.
  See [../README.md](../README.md), "Visual regression".
* `tests/test_viewer.py` is not a test but a tool: an Open3D window that loads a
  JT file through drag and drop or `File ▸ Open` and shows it with a fly camera.
  It goes through the same glTF export, so it is the quickest way to see what a
  change to the writer did to the picture. See [../README.md](../README.md),
  "Interactive viewer".
