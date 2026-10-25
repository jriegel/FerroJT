# USD writer plan

> **Status: stub.** `crates/ferrojt/src/export/usd.rs` compiles and is wired into
> [`Format::Usd`](../crates/ferrojt/src/model/format.rs), but
> `UsdExporter::to_bytes()` returns `Error::NotImplemented("writing USD files")`.
> No USD dependency has been chosen or added to the workspace yet.

## Why it is a stub

glTF is the format the pipeline can prove today: `gltf-json` is a dependency and
the writer is tested. USD has no suitable pure-Rust crate in the local registry,
and the obvious alternative (a binding against the Pixar USD SDK) requires a
native SDK on the build machine, which would break `cargo build --workspace` for
everyone. The scaffold therefore keeps the API shape and leaves the payload
empty.

## Model → USD mapping

The scene layer maps almost one to one onto USD:

| model | USD |
| --- | --- |
| `Model` | one stage; `metersPerUnit` from `Model::unit`, `upAxis` from `Model::up_axis`, `upVector` implied by the axis |
| `Scene` | the default prim plus `def Xform "..."` roots from `Scene::roots` |
| `Node` | `Xform` with `translate` / `rotate` (quat, `x y z w` → USD `GfQuatd` is `w x y z`) / `scale` ops |
| `Mesh` | `Mesh` with `points`, `normals`, `faceVertexIndices`, `faceVertexCounts`, `primvars:st`, `primvars:displayColor` |
| `Primitive::topology` | `faceVertexCounts`; strips/fans must be triangulated or emitted as `linear` subdivision hints |
| `Material` | `Material` + a `UsdPreviewSurface` shader, bound with `MaterialBindingAPI` |
| `Channel::factor` | `inputs:diffuseColor`, `metallic`, `roughness`, `emissiveColor`, `opacity` |
| `Texture` | file path asset, or an in-memory asset written next to the stage |
| `Aabb` | `tokens:purpose` / extent computed by USD; may be written as `extent` |

`Model::accessors` is the bridge: the writer reads `Attribute` bytes exactly like
`export/gltf.rs` does (`read_component`, `widen`), so the two exporters share the
same accessor semantics.

## Two candidate implementations

### A. Hand-written `.usda` (ASCII) writer — recommended first step

* No new dependency; `Format::extension()` already returns `"usda"`.
* A `.usda` file is a small, well specified text grammar (`#usda 1.0`,
  `def Xform "..." { ... }`, typed array literals), so it can be emitted with
  `write!` into the `&mut dyn io::Write` that `Exporter::write` already provides.
* Deterministic, diffable, human readable — ideal for tests that assert on
  structure.
* Limitation: no `.usdc` (binary) or `.usdz` (packaged) output, and no USD
  composition features beyond what is written by hand.

Sketch of the shape to implement:

```text
#usda 1.0
(
    defaultPrim = "Root"
    metersPerUnit = 0.001
    upAxis = "Y"
)

def Xform "Root"
{
    def Xform "gear" ( ) { ... }
}
```

Suggested module split inside `export/usd.rs` (or `export/usd/` once it grows):

1. `StageHeader` — version block, `metersPerUnit`, `upAxis`, `defaultPrim`.
2. `PathBuilder` — sanitize `Node::name` into a valid USD identifier and keep
   paths unique (USD paths must be absolute and unique).
3. `write_xform` / `write_mesh` / `write_material` — one function per schema.
4. A `Value` formatter for `Point3f[]`, `Normal3f[]`, `int[]`, `float2[]`,
   `color3f[]` literals.

### B. Native USD bindings

* Requires the Pixar USD SDK (or a prebuilt distribution) on every build
  machine, plus an `unsafe` FFI layer that conflicts with
  `#![forbid(unsafe_code)]` in `crates/ferrojt`.
* Would have to live in its own crate, e.g. `crates/ferrojt-usd`, so the core
  library stays dependency free and `forbid(unsafe_code)` stays intact.
* Buys `.usdc`, `.usdz`, validation against the real schemas, and composition.

Decision: start with (A). Revisit (B) only if binary/packaged output becomes a
hard requirement.

## Contract to keep while it is a stub

* `Format::Usd` stays in the enum and `is_writable()` currently returns `false`
  for it, which is what makes `ferrojt convert --format usd` fail with
  `usd cannot be written by ferrojt` instead of producing an empty file.
* When the writer lands:
  1. make `Format::is_writable()` return `true` for `Usd`,
  2. implement `UsdExporter::to_bytes` and `Exporter::write`,
  3. drop the `NotImplemented` assertion in `export/usd.rs` and in
     `crates/ferrojt-cli` (`convert` already handles both cases),
  4. update the status table in `README.md` and the flowchart in
     `architecture.md`.

## Tests expected with the writer

* A single triangle mesh produces a stage whose `points`/`faceVertexIndices`
  match the accessor bytes.
* Non-identity `Transform` round trips through the `Xform` ops (remember the
  quaternion component order change).
* A material with a base color factor emits a `UsdPreviewSurface` with
  `inputs:diffuseColor` and a binding on the mesh.
* Duplicate or empty node names yield unique, valid USD paths.
* `Model::unit` maps to the right `metersPerUnit` (JT millimetres → `0.001`).
