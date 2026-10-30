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
   the file declares any; see below.
9. One `Material` per distinct appearance, reached from
   `Primitive::material`; see the materials section below.

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
  filter named by `ACTIVELAYERFILTER::`, falling back to the first one in name
  order when the file leaves that empty — the samples all do, and the JT Layer
  tool's combo box lands on index 0 in exactly that case. An unknown name is an
  `Error::Invalid` that lists the available filters.
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
ones declare `ALL` and `LAYCUR` with an empty active name; every part of every
sample survives its own default filter, so the pinned export counts do not
change.

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
  LOD, tagged `lod_level: Some(0)`, keeps every node of the graph, keeps the
  subtree of every `Instance` and lets two instances share one, applies the
  layer filters the samples declare without losing a part of them, and carries
  the diffuse colours of the file — both in the model and in the exported
  glTF — as one material per distinct colour.
* `crates/ferrojt/tests/dump.rs` — every sample with a reference dump under
  `data/Reference` matches it semantically, and every dump is valid JSON.
* Unit tests in `jt/decode` compare the codecs against the reference output byte
  for byte.
* Unit tests in `jt/model` build a synthetic scene graph, because no sample has
  a part on a layer its filter rejects.
* `tests/test_ferrojt.py` — the same reference dumps through the python bindings.
* `tests/test_visual.py` — the export of every sample — the three
  `example_block` versions and the two `opening_protection_plate1` ones — is
  rendered offscreen and compared with a reference picture in `data/Reference`,
  so a regression in the geometry shows as a picture rather than only as a count.
  See [../README.md](../README.md), "Visual regression".
* `tests/test_viewer.py` is not a test but a tool: an Open3D window that loads a
  JT file through drag and drop or `File ▸ Open` and shows it with a fly camera.
  It goes through the same glTF export, so it is the quickest way to see what a
  change to the writer did to the picture. See [../README.md](../README.md),
  "Interactive viewer".
