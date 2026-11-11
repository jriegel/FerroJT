# USD writer

> **Status: implemented.** `crates/ferrojt/src/export/usd.rs` writes an ASCII
> `.usda` layer by hand — no USD dependency, no `unsafe`. `Format::Usd` is
> writable, so `ferrojt convert file.jt -o file.usda -f usd` and
> `model.save("file.usda")` both work. `.usdc` (binary) and `.usdz` (packaged)
> are still open work.

## Why hand-written ASCII

USD has no suitable pure-Rust crate in the local registry, and the obvious
alternative — a binding against the Pixar USD SDK — needs a native SDK on every
build machine and an `unsafe` FFI layer, which conflicts with
`#![forbid(unsafe_code)]` in `crates/ferrojt`.

A `.usda` layer is a small, well-specified text grammar (`#usda 1.0`,
`def Xform "..." { ... }`, typed array literals), so it can be emitted with
`format!` into a `String`. The result is deterministic, diffable and testable
with plain string asserts, and it can be checked against the real thing: the
samples were validated with `usd-core` 26.11 in a throwaway virtual environment
that is **not** a project dependency.

## Model → USD mapping

| model | USD |
| --- | --- |
| `Model` | one layer: `defaultPrim`, `metersPerUnit` from `Model::unit` (millimetres → `0.001` by default), `upAxis` from `Model::up_axis` (`Z` by default) |
| `Scene` | the default prim; every `Scene::roots` node is written inside it |
| `Node` | `def Xform` with `double3 xformOp:translate`, `quatf xformOp:orient`, `float3 xformOp:scale` and `uniform token[] xformOpOrder` |
| `Mesh` | one `def Xform "GEOMETRY_n"` group in the `FerroJT_Geometry` library, referenced by every consumer |
| `Primitive` | `def Mesh` (`points`, `normals`, `faceVertexCounts`, `faceVertexIndices`, `primvars:st`, `extent`), or `def BasisCurves` / `def Points` for the non-triangle topologies |
| `Primitive::topology` | strips and fans are triangulated into `faceVertexCounts`; `subdivisionScheme = "linear"` keeps the faces exactly as given |
| `Material` | `def Material` + a `def Shader "PreviewSurface"` (`UsdPreviewSurface`), bound with `prepend apiSchemas = ["MaterialBindingAPI"]` and `rel material:binding` |
| `Channel::factor` | `inputs:diffuseColor`, `inputs:metalness`, `inputs:roughness`, `inputs:emissiveColor`, `inputs:opacity` |
| `Aabb` | `float3[] extent` |
| `Model::extras["layer_filter"]` | `customLayerData` keys `ferrojt:layer_filter` and `ferrojt:layers` |

`Model::accessors` is the bridge: the writer reads `Attribute` bytes exactly
like `export/gltf.rs` does (`read_component`, `split_suffix`), and reuses its
`depth_first` walk, so the two exporters agree on traversal order and accessor
semantics.

Level of detail and layer filtering already happened in the reader, so a USD
layer gets the same subset of the file a glTF export writes.

## Instancing: a mesh is written once

Every mesh is authored **exactly once**, in a `FerroJT_Geometry` group that sits
beside the default prim and carries `token visibility = "invisible"`. Each node
that draws it gets `prepend references = [</FerroJT_Geometry/GEOMETRY_n>]` plus
its own transform ops, and the `Mesh` prim carries `instanceable = true` so a
consumer treats the repeats as instances rather than copies.

Three details are load-bearing:

- The reference targets the per-mesh **group**, not the `Mesh` prim, because
  that group is where `rel material:binding` sits and a reference only carries
  what the referenced prim itself states.
- The group is deliberately **not** `instanceable`. Making it instanceable turns
  every consumer into an instance proxy and any later write to it fails with
  *"authoring to an instance proxy is not allowed"*.
- A scene node reached from several parents — a JT `Instance` shares its subtree,
  and the reader keeps it as one `Node` with two parents — is written once under
  its first parent. Every other parent gets a thin child `Xform` whose metadata
  says `doc = "JT <kind>: shared subtree"` and which `prepend references` that
  path and **authors nothing of its own**, so the node's transform and whole
  subtree come through unchanged. That is USD's equivalent of glTF listing one
  node in two `children` arrays.

Measured on the samples (library meshes / triangles, materials, shared
wrappers):

| sample | meshes | triangles | materials | shared wrappers |
| --- | --- | --- | --- | --- |
| `example_block_jt9.5.jt` | 1 | 12 | 1 | 0 |
| `opening_protection_plate1_jt9.5.jt` | 2 | 486 | 2 | 1 |
| `SampleJT_Server.jt` | 106 | 129 067 | 23 | 529 |

## Materials: deduplicated by value

`Model::materials` is collapsed onto the values a `UsdPreviewSurface` actually
carries — diffuse, opacity, metallic, roughness, emissive and `doubleSided` —
not by name. A JT file that repeats one appearance across a dozen shapes yields
one `Material_0`, and each model material maps to a slot
(`Plan::material_slot`). Two materials whose names differ but whose values are
identical are the same material; two that differ only in **opacity** are not.

The binding is authored on the library group, so a consumer can override it
without touching the geometry. A node whose `Instance::material` differs from
the material its mesh carries gets its own `rel material:binding` as an
override. Textures are not exported yet; a textured channel falls back to its
factor.

`SampleJT_Server.jt` carries 23 model materials and all 23 survive: grouping the
authored shaders by shading values finds 23 distinct values in 23 prims, so
nothing was collapsed that should have stayed apart.

## Layer filters

The reader already gates `Node::Part` on the layer filters of the file, so the
writer only has to record what ran. `Model::extras["layer_filter"]` becomes

```text
customLayerData = {
    string "ferrojt:generator" = "FerroJT 0.1.0"
    string "ferrojt:source" = "data\\ExampleFiles\\example_block_jt9.5.jt"
    string "ferrojt:layer_filter" = "ALL"
    int[] "ferrojt:layers" = [1, 2, 3, ...]
}
```

A file that declares no filters (`SampleJT_Server.jt`, the JT 8.1 block) records
nothing, and `to_model_unfiltered()` records nothing either — which is how a
consumer can tell the two apart.

## Syntax details that bite

- `prepend apiSchemas = ["MaterialBindingAPI"]` is **prim metadata** and has to
  go in the parenthesised block after the prim name. In the body it is a parse
  error.
- A property's metadata block opens on the line that closes its array:
  `point3f[] points = [ ... ] (` … `)`. A `(` on its own line is a parse error.
  That block is where `interpolation = "vertex"` goes.
- A USD quaternion literal is `real, i, j, k`; a JT rotation is `x, y, z, w`. The
  components are reordered, and the quaternion is normalised first.
- USD identifiers are `[A-Za-z_][A-Za-z0-9_]*`. JT names are full of dots
  (`example_block_nx4.asm`), so every other character becomes `_`, a leading
  digit becomes `_`, and an empty name falls back to `Node_<index>`.
- Sibling names must be unique. A JT file happily instantiates one part twice
  under one parent and both children carry the same name, so the
  de-duplicating `BTreeSet` has to be created **once per parent**, not once per
  child — otherwise two prims land on one path and the layer fails to open
  (`Duplicate prim 'shcs_asm'` → `Failed to open layer`).
- `number()` uses Rust's shortest round-trip form, so `25.0` is written `25`.
  `.usda` also accepts `nan`, `inf` and `-inf`.

## Validating against the real SDK

`usd-core` is **not** a dependency; it lives in a scratch virtual environment
(`target/_usdvenv`, gitignored). `target/usd_check.py <file.usda> <meshes>
<triangles> <materials>` opens the layer, reports `stage.GetCompositionErrors()`,
walks `Usd.PrimRange(stage.GetPseudoRoot(), Usd.TraverseInstanceProxies())` and
separates the library counts from the counts a consumer sees. All six samples
report `OK`. API facts worth remembering:

- `Stage.GetCustomLayerData()` does not exist — read
  `stage.GetRootLayer().customLayerData`.
- `Prim` has no `GetCompositionErrors`; only `Stage` does.
- `Usd.Relationship` has `GetTargets()`, not `GetResolvedPath()`.
- `prim.GetMetadata("references")` returns an `Sdf.ReferenceListOp`, which is not
  iterable — use `GetAppliedItems()`, since a `prepend` leaves `addedItems` and
  `explicitItems` empty. A reference's path is `.primPath`.
- `Sdf.Path` has no `startswith` — stringify first.
- `UsdValidation.ValidationContext` takes validators, not a `Stage`.

## Tests

- `crates/ferrojt/src/export/usd.rs` — unit tests over hand-built models: header
  and units, name sanitising, unique sibling paths, one mesh referenced twice, a
  shared subtree referenced rather than copied, materials collapsed by value, a
  per-node binding override, library placement, quaternion order, vertex
  interpolation, `Points` / `BasisCurves`, strip and fan triangulation, the
  recorded layer filter, and the two error paths (a primitive without `POSITION`,
  a child outside the node table).
- `crates/ferrojt/tests/usd.rs` — the same properties pinned per sample, plus
  determinism (`export(model) == export(model)`) and a forced layer filter.
- `tests/test_ferrojt.py` — `model.save("model.usda")` and
  `model.to_bytes("usd")` through the bindings.

## Open work

- `.usdc` (binary) and `.usdz` (packaged) output.
- Textures: a JT texture is not carried into the shader yet.
- Per-instance materials on a node that draws several meshes at once — the
  per-mesh bindings on the library already say everything there is to say, so no
  override is authored there today.
