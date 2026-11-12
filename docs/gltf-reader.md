# glTF reader

> **Status: implemented.** `crates/ferrojt/src/import/gltf.rs` reads `.gltf` and
> `.glb` into the neutral [`Model`](../crates/ferrojt/src/model/scene.rs), so a
> glTF file can be inspected, exported to USD, and — through
> [`jt::model_to_bytes`](jt-from-model.md) — written back out as JT.
> `crates/ferrojt/tests/gltf.rs` pins the whole hop, glTF → JT, for all six
> samples in `data/ExampleFiles`.

## Entry points

| Item | Behaviour |
| --- | --- |
| `import::from_file(path)` | Read a `.gltf` or `.glb`; relative buffer and image URIs resolve against the directory of `path`. |
| `import::from_bytes(bytes, base)` | Read from memory. `base` resolves external URIs; `None` refuses a file that references anything outside itself. |
| `ferrojt::open(path)` / `ferrojt::open_format(Format::Glb, path)` | The dispatchers. `Format::detect` picks the container from the extension, `Format::is_readable` says what can be read. |
| `ferrojt.open("x.glb")` in Python | The same, returning a `Model`. |

The container is detected from the **magic bytes** inside `gltf::Gltf::open`,
not from the extension, so a mis-named file still loads; the extension only
decides which reader the dispatcher picks.

## What is kept as authored

The reader is deliberately boring, because the model is a transfer structure and
not a scene graph to be normalised:

- One [`BufferBlock`](../crates/ferrojt/src/model/geometry.rs) per glTF buffer
  and one `Attribute` per accessor, with the accessor's `min`/`max` carried over
  as the author wrote them. Nothing is widened, nothing is de-interleaved.
- Textures, materials and meshes are pushed in document order, so a glTF index
  and a model index stay the same number.
- Images are **not** decoded. A `Texture` holds the encoded bytes plus their
  MIME type — which is exactly what a JT file stores, so no round trip needs a
  codec.
- A node matrix is decomposed into translation, rotation and scale, because
  that is what JT and USD store. The quaternion comes out `x, y, z, w`, the
  convention of `model::Transform::rotation`.

## The frame is declared, not inferred

glTF 2.0 is metric and Y up **by contract**, so the reader writes it into the
model:

```rust
model.unit = Some("meter".to_owned());
model.up_axis = Some("+Y".to_owned());
```

The JT reader does the mirror thing and declares `millimeter` / `+Z` (see
[jt-from-model.md](jt-from-model.md)). That is what makes a unit conversion a
declared fact rather than a guess, and it is why a JT → glTF → JT round trip
does not drift.

## GLB framing gotcha

`export/glb.rs` owns the GLB container, and the one rule that a hand-written
reader breaks is the chunk length: it must be the **padded** length, not the
length of the payload. A 27-byte JSON document sits in a 28-byte chunk; declaring
27 leaves the pad byte outside the chunk, and the next reader interprets it as
the first byte of the BIN chunk header — which surfaces as a checksum mismatch or
"failed to fill whole buffer" rather than as anything about padding. Pinned by
`a_chunk_length_counts_its_own_padding` in `export/glb.rs`.

Header: magic `glTF`, version, total length. Each chunk header is **length first,
then type**. JSON is padded with `0x20`, BIN with `0x00`.

## What does not survive the hop

The neutral model has no room for everything glTF can say, and JT has no node
type for some of what it can. The rules live in the writer, not the reader, and
are pinned in `tests/gltf.rs`:

- **Every JT node class is erased.** A file written from a model that came out of
  glTF is exactly `{Partition: 1, Shape: meshes, Group: nodes − meshes}` plus one
  `Instance` per extra placement. `Node::kind` is what carries a JT class through
  a JT → model → JT hop; a glTF node gets `"Group"` and stays one.
- **Only plain triangle lists have a JT shape type.** A `POINTS`, `LINES` or
  strip primitive is refused rather than re-indexed.
- **A `Shape` node whose mesh is empty becomes a `Group`.** glTF mandates a
  `POSITION` attribute, and JT has no empty shape worth writing.
- **Level of detail is lost.** The model has no tier, so a JT file that goes out
  through glTF comes back with one representation per part.

## Parallel tests and temp files

`tests/gltf.rs` writes real files, and two tests that both derive their temp name
from the sample stem race on the same path — the symptom is a corrupt-file error
in a test that never touches a corrupt file. The helper keeps a process id **and
an atomic counter** in the name.
