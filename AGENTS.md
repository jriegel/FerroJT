# FerroJT — agent notes

Rust cargo workspace plus a Python wrapper that reads OpenJT (`.jt`) and glTF 2.0
/ GLB, exports glTF 2.0 / GLB, ASCII USD (`.usda`) and JT, and can write a read
file back out as JT. Read this before changing code; the design detail lives in
`docs/`.

## Layout

| Path | What it is |
| --- | --- |
| `crates/ferrojt` | The library: `model/` neutral scene graph, `codec/` binary helpers, `import/` the glTF/GLB reader (`gltf.rs`), `jt/` OpenJT reader (segment/TOC, element objects, decode codecs, `dump.rs` JSON writer), `jt/version.rs` the `JtVersion` a file is read or written at, `jt/encode.rs` the topological (dual vertex-face) encoder, `jt/hash.rs` the Jenkins lookup2 hashes a strict consumer verifies, `jt/write.rs` the JT writer, `jt/build.rs` the builder that creates a file from nothing, `jt/from_model.rs` (private) the writer that turns a neutral `Model` into a file, `model/flatten.rs` the merge of a whole file into one mesh, and `export/` glTF/GLB writer and the ASCII USD writer. |
| `crates/ferrojt-cli` | The `ferrojt` binary (clap): `info`, `inspect`, `dump`, `convert`, `write`, `ls`. |
| `crates/ferrojt-python` | `pyo3` cdylib `_ferrojt`; `lib.rs` is the export view (`Model`) plus `lsg.rs` the file view (`JtLsg`/`JtNode`/`JtMesh`/`JtArray`) and `build.rs` the builder (`JtBuilder`/`NodeHandle`/`MeshHandle`/`MeshData`/`MaterialData`), `mesh.rs` the merged-mesh buffer of `Model::merged_mesh`/`ferrojt.read_mesh` and `array.rs` the buffer plumbing `JtArray` and that buffer share; `pyproject.toml` + `python/ferrojt/` + `tests/`. |
| `crates/ferrojt-viewer` | `ferrojt-viewer` binary: egui outliner + kiss3d viewport. `meshes.rs` JT→GPU meshes, `scene.rs` row tree/transforms/LOD stacks, `lod.rs` screen-area tier selection, `tree.rs` outliner, `details.rs` key/value pane for the selected row, `backdrop.rs` viewport gradient, `camera.rs` `Camera3d` wrapper that places the backdrop, `picking.rs`, `trihedron.rs`. |
| `python/ferrojt/` | Pure-Python package re-exporting `_ferrojt` (`__init__.py`, `_ferrojt.pyi`, `py.typed`). |
| `data/ExampleFiles/` | Sample JT files: JT 8.0, 8.1, 9.5, 10.3. |
| `data/Reference/` | Reference JSON dumps from the JT tooling **and** reference PNG pictures — **the oracle**. |
| `tests/` | Python suite (`test_build.py`, `test_ferrojt.py`, `test_jt2go.py`, `test_visual.py`) plus the tools `jt2go_picture.py` (drives an installed Siemens JT2Go and counts the pixels it draws — the **strict** writer oracle, see below) and `test_viewer.py` (interactive Open3D viewer probe). Output dirs `_visual_out/`, `_jt2go_out/`, `_jt2go_full/` are gitignored. |
| `example/` | GPL-2.0 C++ sources (TKJT, ToolsLib, JtDump, PyOpenJt). Behavioural reference only, never linked or copied; gitignored. |
| `docs/` | `README.md` index, `architecture.md`, `jt-reader.md`, `gltf-reader.md`, `jt-writer.md`, `jt-builder.md`, `jt-from-model.md`, `dump.md`, `usd-writer.md` and `viewer.png` — the viewer picture shown at the top of the top-level `README.md` (Git LFS). |

## Commands

```sh
cargo build --workspace
cargo test  --workspace --exclude ferrojt-python   # must exclude, see below
cargo clippy --workspace --all-targets
cargo fmt --all
cargo doc --no-deps -p ferrojt
```

Round-trip a sample through the JT writer (`write` refuses to overwrite, so a
sweep needs `--force`):

```powershell
cargo run -p ferrojt-cli -- write data\ExampleFiles\example_block_jt9.5.jt -o target\out.jt --force
```

The same at another revision — `--jt-version` (8.x/9.x/10.x, and a family change
is refused, see below):

```powershell
cargo run -p ferrojt-cli -- write data\ExampleFiles\example_block_jt9.5.jt -o target\out103.jt --force --jt-version 10.3
```

Build a file from nothing (the builder, not the writer):

```powershell
cargo test -p ferrojt --test build          # the Rust side
.\.venv\Scripts\python.exe -m pytest tests\test_build.py -q   # the Python side
```

Convert a glTF file to JT (the neutral-`Model` writer, not the rewriter — it
prints the revision the file actually landed on):

```powershell
cargo run -p ferrojt-cli -- convert target\block.glb -o target\block.jt --force
cargo run -p ferrojt-cli -- info target\block.jt
```

Merge a whole file into one mesh (`--flatten` exists on both `convert` and
`write`; it conflicts with `--all-lods`, and a merged mesh of touching parts is
no longer a closed two-manifold, so the JT writer falls back to 8.x by itself):

```powershell
cargo run -p ferrojt-cli -- convert data\ExampleFiles\opening_protection_plate1_jt9.5.jt -o target\plate.glb --force --flatten
cargo run -p ferrojt-cli -- write data\ExampleFiles\opening_protection_plate1_jt9.5.jt -o target\plate.jt --force --flatten
```

`ferrojt-python` must be excluded from `cargo test`: its default features enable
`pyo3/extension-module`, so a test binary cannot resolve the Python symbols.

Python (`.venv` at the repo root, gitignored):

```powershell
$env:VIRTUAL_ENV = "$PWD\.venv"
.\.venv\Scripts\python.exe -m maturin develop
.\.venv\Scripts\python.exe -m pytest
```

`maturin develop` takes **no `-i`** — that flag belongs to `maturin build`. With
`VIRTUAL_ENV` set it installs into the right environment on its own.

Visual regression (needs `open3d`, `pip install -e ".[test,visual]"`; the tests skip
without it):

```powershell
.\.venv\Scripts\python.exe tests\test_visual.py --write   # recreate data/Reference/*.png
.\.venv\Scripts\python.exe tests\test_visual.py           # compare, diff -> tests/_visual_out/
```

Interactive viewer (also needs `open3d`; it is a tool, not a test — nothing in it
is named `test_*`, so pytest still collects 127 tests):

```powershell
.\.venv\Scripts\python.exe tests\test_viewer.py data\ExampleFiles\example_block_jt9.5.jt
```

External render check — open a file in Siemens JT2Go and count the pixels it
actually draws (Windows only, needs a JT2Go install; the tool skips when none is
found). This is the strict oracle for the writer, see below:

```powershell
.\.venv\Scripts\python.exe tests\jt2go_picture.py data\ExampleFiles\example_block_jt10.3.jt
.\.venv\Scripts\python.exe tests\jt2go_picture.py target\out.jt --out tests\_jt2go_out
.\.venv\Scripts\python.exe -m pytest tests\test_jt2go.py -q   # the same as a test
```

Rust viewer (native window, no Python):

```powershell
cargo run -p ferrojt-viewer -- data\ExampleFiles\example_block_jt9.5.jt
```

Wheels for PyPI: `build-wheel.bat [windows|linux|sdist]` (default all) → `dist/`.
Upload with `python -m twine upload dist\*`.

## Ground rules

- `#![forbid(unsafe_code)]` and `#![warn(missing_docs)]` in `crates/ferrojt` — keep both.
- Edition 2024, `rust-version = "1.85"`: no `is_multiple_of` (1.87), no
  `String::pop_if` (1.87), no let-chains (1.88). Use `% n != 0` / `div_ceil`.
- Errors: `Error::invalid(offset, message)` / `Error::Corrupt(String)`. `Error` is
  not const-constructible. A `write_all(..)` tail expression needs `?; Ok(())`.
- Rustdoc warns on intra-doc links to private modules — link only to re-exported
  public items. `jt::from_model` is a **private** module, so a doc comment on
  `model_to_bytes` must link `[crate::jt]`, never `(self)`.
- No `[workspace.lints]`; lints are per-crate.
- Dual license `MIT OR Apache-2.0`. Author string is `Juergen Riegel` in both
  `Cargo.toml` and `pyproject.toml`.
- Commit on `main`, one commit per completed unit. `Cargo.lock` is committed;
  `target/`, `dist/`, `.venv/`, `example/` are not. `data/` **is** committed
  (samples, reference JSON and reference PNGs) — only `data/fmsr_*` is ignored.

## Things that will bite you

- **JT dump parity is byte for byte.** `jt::dump(file, config, indent)` must match
  `data/Reference/{stem}.json` exactly. Config bits: `DUMP_TOC=1`, `DUMP_LOD0=2`
  (declared but never read by the writer), `DUMP_MESH=4` (only empties
  `"Indices"`, the LSG object is always written), `DUMP_ALL=i32::MAX`.
  Numbers use a Rust `format_g` that reproduces C `%g`.
- Reference JSON has a UTF-8 BOM — read with `encoding='utf-8-sig'`. Comparison is
  semantic: parse, then drop `FileName` recursively.
- `SampleJT_Server.jt` has no reference and its dump is **not valid JSON**: the C++
  writer emits a separator comma and then nothing for a `Shape_Vertex` child of an
  `Instance`. The Rust writer reproduces that deliberately — do not "fix" it, and
  filter that sample out of any test that parses JSON.
- The C++ readers ignore primitive failures and resynchronise by skipping to the
  element end; the Rust reader mirrors that tolerance. `JtData_Vector::operator[]`
  is unbounds-checked — never reproduce an out-of-bounds read (the `RangeLOD`
  `limits` off-by-one is fixed as `limits.get(1..).unwrap_or(&[])`).
- **The topology of a real CAD file can name a vertex index past its own
  coordinate array** — 98 of the 9,302 files of `C:\Data\CLA_EXPORT_JR` do it,
  usually exactly `index == unique_vertex_count`. `decode/vertex.rs::Decoded::get`
  must stay bounds-safe and read `0.0` (it panicked on `data[i * comp_count + j]`
  until that was fixed, and one bad index lost the whole file); the copy loop in
  `object.rs` pads zeros and emits one `tracing::warn!` per shape naming how many
  components were out of range. `is_within(i)` is the predicate. Pinned by
  `a_read_past_the_decoded_array_reads_as_zero`. Sweeping such a corpus from
  Python needs `except BaseException`: pyo3 surfaces a Rust panic as
  `PanicException`, which does **not** derive from `Exception`.
- **LOD tier is positional, not encoded.** JT never stores a tier: it is the child
  position under `RangeLod` (first group = 0, next = 1, …). Segment type cannot
  identify it — JT 8.x/10.x use types 7..10 for LOD0..LOD3, but JT 9.x uses type 6
  ("Shape") for every tier. `to_model()` exports only the finest tier **that has
  vertices**, mirroring `example/ToolsLib/src/JtGltfMesh.cpp` with one deliberate
  deviation: a tier whose shape declares no vertices at all does not settle the
  span, so the next tier that really has geometry is exported (a strict tier-0 rule
  draws nothing for those files); the state is
  `LodWalk { inside, level, tiers, keep }` threaded through `build_node`, and
  `next_tier(class, produced)` clears `keep` as soon as a tier delivered
  geometry — `jt::write.rs::Plan` uses the same rule via its `geometry_rows`
  counter, so a rewrite and an export agree. Only the
  **direct** children of a range span are tiers — `for_child()` clears `tiers` one
  level down — because a span may wrap an assembly whose nested groups are not
  tiers; the reference counts any nested group, which is invisible in the samples
  and fatal for a written wrapper. Nodes are never dropped, only geometry, and
  each mesh records `Mesh::lod_level`. `jt::dump` is *not* filtered. Pinned by
  `a_span_keeps_the_finest_tier_that_has_geometry` and
  `an_empty_finest_tier_falls_through_to_the_next_one`; found by sweeping
  `C:\Data\CLA_EXPORT_JR`, where **171 files** store their only shape in tier 1
  because their LOD0 is empty (and draw fine in the NOVO viewer).
- Pinned LOD0 counts (`crates/ferrojt/tests/model.rs`): block 8.1/9.5/10.3 →
  1 mesh / 12 triangles; plate1-8.0 → 2 / 617; plate1-9.5 → 2 / 486.
  `tests/test_ferrojt.py` asserts `mesh_count == 1`, `triangle_count == 12`.
- **`JtFile::to_model_with_all_lods()` keeps every tier** (viewer only; `to_model()`
  is unchanged by construction). Pinned in `every_level_of_detail_can_be_loaded()`:
  block 8.1/9.5/10.3 → 3 meshes / 36 tris / 3 tiers; plate1-8.0 → 8 / 1137 / 4;
  plate1-9.5 → 6 / 922 / 3; `SampleJT_Server.jt` → 106 / 129067 / **1** — its 584
  `RangeLOD`s each have exactly one `Group` child, so screen-area LOD switching is
  a **no-op there** and the toolbar checkbox is hidden. Use block/plate to demo it.
- **The viewer row tree is a tree, not the graph.** `ViewerScene::new` walks the
  model graph with a *path* cycle guard, so a node reachable from several parents
  becomes one row **per path**: `SampleJT_Server.jt` has 2158 model nodes but
  **4489 rows** (198 shared nodes, max 54 visits). That is why the toolbar counts
  triangles **per attachment** on both sides of the fraction —
  `ViewerScene::total_triangle_count()` (sum of `Slot::triangles`) and
  `drawn_triangle_count()` share that basis, and `Model::triangle_count()`
  (unique meshes, 129067) does **not**: mixing them printed
  `1016789 of 124154 triangles`. `meshes::triangle_count` was deleted for this.
- **`ViewerScene` cannot be built in a test.** `SceneNode3d::mesh()` needs the
  kiss3d `WINDOW_CACHE` thread-local (`window_cache.rs:13`, `texture_manager.rs:392`
  `unwrap()` on `None`) until a window exists. So the LOD logic in `scene.rs` lives
  in **pure free functions** over the row tree (`detect_stacks`, `measure_stacks`,
  `hidden_tiers`, `clamp_tier`, `row_drawn`, `drawn_triangle_count`,
  `subtree_cost`) and the tests build `Vec<Row>`/`Vec<Vec<usize>>` fixtures instead.
  `row_drawn` must walk the **whole ancestor chain** — the kiss3d graph is flat, so
  hiding a tier does not hide its Shape children by itself.
- **Screen-area LOD (`lod.rs`).** One step per frame (`step`), finer when
  `tris[current] < pixels * TRIANGLES_PER_PIXEL` (1.5), coarser only when the finer
  tier would cost `>= budget * HYSTERESIS` (1.6) — that asymmetry is what stops
  flapping. `projected_area` must **not** use `Camera3d::project`: it divides by
  `w` and returns finite mirrored pixels for geometry behind the eye
  (`camera3d.rs:158`), so `!point.is_finite()` never fires. Use
  `camera.transformation()` and classify corners by `clip.w <= 0.0`: all behind →
  `None` (pick the coarsest tier), some behind or a span over `4 * size` →
  `f32::MAX` (pick the finest).
- **An `Instance` shares its subtree, it is not copied.** The plate samples hold
  one screw and instantiate it twice (`y = -25` and `y = +25`). `build_node`
  memoises by graph position in `Built { done: HashMap<usize, usize> }`, recorded
  *before* the children are walked, so a second visit returns the node already
  built and both instances point at the same model index — valid glTF (one node,
  two parents), and it keeps the pinned counts and `every_node_of_the_scene_graph_survives`
  green. The old `expanded: HashSet` returned a *fresh, childless* node on the
  second visit, which is why one screw vanished in the viewer. `done` doubles as
  the cycle guard; there is no separate path set.
- `to_quaternion` must return a **unit** quaternion: the trace branch is
  `s = 0.5 / sqrt(trace + 1)`, `w = 0.25 / s` (the `0.5 / s` version gave `w = 2`
  for the identity). glTF normalises, so the bug only shows as a scaled mesh when
  the model is consumed directly.
- **A JT transform matrix is column major.** `GeometricTransform::matrix` is flat
  column major: element (row, col) is `matrix[4 * col + row]`, the translation is
  the last **column** at 12..14, and the scale is the length of each **column**.
  `jt/model.rs::decompose` walked it row major, which returns `Rᵀ` — the inverse
  rotation. Translation is identical under both readings, so positions looked
  right and only rotated parts were mis-oriented: `SampleJT_Server.jt` showed the
  captive screws / knobs / casters spun the wrong way in both `test_viewer.py`
  (via glTF) and `ferrojt-viewer` (via `Model`), which is why the fix is in the
  library, not the viewer. Citations: `JtAttribute_GeometricTransform.hxx:35`
  (`Eigen::Map<const Eigen::Matrix4d>`, "translation in the last column"),
  `JtGltfMesh.cpp:601-616` (`actTrsf(row, col) = GetTrsf()[4 * col + row]`),
  `JTData_SceneGraph.cxx:347` (`aMatrix(aX, aY) = GetTrsf()[aY * 4 + aX]`).
  **Every test matrix before this was identity, symmetric or a pure translation,
  where a transposition is invisible** — determinant is 1 either way, so it cannot
  discriminate either. `tests/model.rs` now round-trips all 1218 non-identity
  transforms of the server sample (`every_transform_of_a_sample_survives_the_decomposition`)
  and pins the screw rotation (`a_rotated_instance_keeps_the_rotation_of_the_file`).
  Composition is parent-relative and correct (`JTVis_ScenegraphTasks.cxx:78`
  composes `myTransform *= node.Transform()`); `JtGltfMesh.cpp` instead replaces
  the matrix and bakes it into vertices — deliberate deviation. After the fix the
  `SampleJT_Server.png` reference had to be regenerated (1.26 % of pixels changed,
  all on the front panel edges and the lower knobs/casters).
- **JT colours *are* exported — `test_visual.py` forces flat grey on purpose.**
  A "the colour is missing" report is almost always the viewer or the grey
  oracle, not the writer. The mapping is `diffuse → baseColorFactor`,
  `emission → emissiveFactor`, and the fixed `metallic 0.2` / `roughness 0.8` /
  `doubleSided true` of `JtGltfMesh.cpp:531-545`. Do **not** derive roughness
  from the JT shininess exponent: the samples all carry 128, which as a glTF
  roughness is a near mirror and blows the colour out to white (that was the
  original bug). Ambient/specular/shininess/reflectivity go to the glTF material
  `extras` (`gltf::raw_extras`). `Model::add_material` dedups, so plate1 exports
  2 materials although the attribute repeats 9–12 times. Colour proof:
  `tests/model.rs` pins the colours against the reference dumps, and
  `test_visual.py` renders plate1-9.5 with its own materials and requires
  chroma > 40 on > 0.3 % of the pixels (measured 0.96 %; the grey render is
  0.00 %) — the PNG references cannot show colour at all.
- **Open3D blank frames are not limited to the first one.** `render_to_image()`
  also returns an all-zero frame at random positions after `clear_geometry()`
  (seen on the first sample and again mid-run). `test_visual.py::shoot` renders
  up to `MAX_FRAMES` and keeps the first frame whose max channel is > 0; a
  single discard is not enough and the failure looks like "similarity 0.129,
  100% of the pixels differ".
- **Screen-space ambient occlusion blanks the colour pass for the server mesh.**
  `SampleJT_Server.jt` (~1.02 M triangles merged, 20 meshes via
  `read_triangle_model`) renders as an all-zero 640×480 frame in roughly 5 of 6
  fresh processes — `shoot` never gets a frame, and the failure again looks like
  "100% of the pixels differ". Nothing is wrong with the geometry: `render_to_
  depth_image(z_in_view_space=True)` returns ~18 760 finite pixels while the
  colour pass is zero, so the mesh rasterises. `render.scene.view.
  set_ambient_occlusion(False)` fixes it (8/8, 8/8, 7/8, 8/8 in four fresh
  processes, good frame always at index 2); the other view knobs do not —
  `set_antialiasing(False)`, `set_sample_count(1)`, `set_shadowing(False)` are
  0/6, `set_post_processing(False)` 1/6. It is picture-neutral: AO on vs off
  scores 0.99809 on every sample with ≤ 0.001 % of the pixels changed, which is
  the noise floor of the comparison itself, so the five older references still
  pass unchanged. Not NaNs (0), not degenerate triangles, not the camera, not
  the lighting profile, not pacing; splitting the mesh into 2/4/8/16 geometries
  or decimating is worse, and vertex clustering changes the picture.
- **Layer filters gate `Node::Part` only.** The filters live in the
  `LAYERFILTERnnn::` key/value streams of a `MetaData` node, the tags in the
  `LAYER::` stream of a `Part`; both are read from the concatenated
  `ProxyMetaData` late loads via `scan_for_layer_filter` / `scan_for_layer`
  (`jt::dump` helpers), so dump and export agree on what the file says.
  `JtFile::layer_filter(name)`: no name → the filter named by
  `ACTIVELAYERFILTER::`, and **no filter at all** when that value is empty or
  names nothing declared — the rule of `JtGltfMesh.cpp:305-308`
  (`if (layerInfo.ActiveLayerFilter.size() != 0)`). The old fallback to the
  **first key of the sorted `BTreeMap`** was the JT Layer combo box's habit of
  landing on index 0 when its `findText` fails (`JTGui_MainWindow.cxx:475-493`):
  a UI artifact, and it hid *all* geometry of every Teamcenter export whose
  alphabetically first filter is `Additional Publications` / `[WIREFRAME]`.
  Unknown name → `Error::invalid(0, …)` listing the available names. A part with
  no tag is **kept** (deviation from `JtGltfMesh.cpp`, keeps plate1-9.5 at
  2/486); `Partition`/`MetaData`/`LOD`/`RangeLOD`/`Group`/`Instance` are never
  gated, and a rejected part keeps its node but loses geometry, children and
  instance target. API: `to_model()` / `to_model_unfiltered()` /
  `to_model_with_filter(&str)`; the effective filter is
  `Model::extras["layer_filter"] = {"name", "layers"}`, recorded **only when a
  filter ran**. `LayerFilter` is re-exported from `ferrojt::jt`. `jt::dump` stays
  unfiltered. No sample names an active filter, so every sample exports
  unfiltered by default and the pinned counts are unchanged — a drop needs a
  named filter or the synthetic fixture in `jt/model.rs`.
- **The server sample's "36 empty representations" were a decode failure, not
  empty shapes.** `SampleJT_Server.jt` has one shape (file-view representation
  id 164, reached from many nodes) whose topological stream could not be read, so
  its `LodGeometry` came back with no vertices: `push_mesh` skipped the empty
  channel, the `Primitive` had **no attributes at all**, and
  `export/gltf.rs::to_primitive` refused the whole document ("a primitive without
  a POSITION attribute cannot be exported to glTF"). The reader still drops a
  representation without vertices in `build_node` (`geometry.vertices.is_empty()`
  → `continue`) — keep that policy and the POSITION guard, which are the right
  answer for hand-built and degenerate models — but no reference sample hits it
  any more. Counts for that sample: nodes 2158, **meshes 106**, materials 23,
  triangles 129067 — pinned in `tests/model.rs`, `tests/gltf.rs`, `tests/write.rs`,
  `tests/usd.rs` and `tests/test_ferrojt.py`. `JtGltfMesh.cpp` would instead append
  zero-length accessors; deliberate deviation.
- **`jt/decode/cdp.rs` implements codec 5 (Move-to-Front).** A MARK-2 integer
  package may carry `CodecId 5`: *Window Values* (a `ByteCDP` package, count ≤ 16)
  and *Window Offsets* (an `Int32CDP` package of the **same** count) first, then
  the coded values. The window starts **empty** (capacity 16), an index at or
  beyond the current window size is an escape that pulls the next raw value from
  the offset stream, and inserting at the front truncates the window back to 16.
  Real CAD exports (the `C:\Data\CLA_EXPORT_JR` corpus) use it for the split-face
  stream of nearly every shape. TKJT reads those files no further than we used
  to: it `Load2`s that package and dies with `invalid CodecId 5`, so the PSI/JT
  spec plus a byte walk are the oracle there, not `example/`.
- **The High-Degree Face Attribute Masks stream is a bare `VecU32`, not a CDP
  package.** It is an `i32 count` followed by `count` `u32` words with **no codec
  byte**, at every revision (spec: "all remaining face attribute bit vectors,
  adjoined end-to-end, and encoded as a single array of unsigned integers"), and
  it sits between *Face Attribute Mask 8 (32 MSBs)* and *Split Face Syms*. Every
  sibling stream is a `VecI32{Int32CDP}` and TKJT calls `Load2` here, so a file
  with a single face of degree > 64 desynchronises the whole shape in TKJT — and
  used to here, as `unknown mark 2 codec 255` (the first mask word's low byte).
  `decode/mesh.rs::read_u32_array` reads it, `encode.rs::write_u32_array` writes
  it. An **empty** array frames identically under both readings, which is why none
  of the reference samples caught it; the regression fixture is
  `encode.rs::heptegintagon_bipyramid` (two coder vertices of degree 70).
- `pyo3` 0.30 cannot express a tri-state keyword: nested
  `Option<Option<&str>>` is ambiguous (`SomeWrap<T>` matches both `T` and
  `Option<T>`, E0283). Model it as two parameters — the Python API is
  `layer_filter="NAME"` / `all_layers=True`, and combining them is
  `ValueError`. CLI equivalent: `--layer-filter NAME` (conflicts with
  `--no-layer-filter`) / `--no-layer-filter`.
- `pyo3` 0.30: releasing the GIL is the **method** `py.detach(|| ...)` (re-acquire
  with `Python::attach`); `Python::allow_threads` is gone. Optional args need
  `#[pyo3(signature = (path, format = None))]`. Error mapping:
  `Io`→`PyOSError`, `Invalid`→`PyValueError`, `NotImplemented`→`PyNotImplementedError`.
- `pyo3` is built with `abi3-py311`, so one wheel per platform covers CPython
  ≥ 3.11. Tags: `cp311-abi3-win_amd64`,
  `cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64`. **3.11 is the floor
  because `__getbuffer__` is compiled out below it** — pyo3 gates the
  `bf_getbuffer`/`bf_releasebuffer` trampolines on `Py_3_11` (the stable ABI only
  gained the slot then), which is what `JtArray` is built on. Bumping back to
  `abi3-py310` silently drops the buffer protocol.
- **The LSG view (`crates/ferrojt-python/src/lsg.rs`).** `JtFile.lsg` is a
  `JtLsg` handle holding the parsed `ferrojt::jt::JtFile`; `JtNode`/`JtMesh`/
  `JtArray` are thin handles that keep it alive through a `Py<Lsg>`, so a handle
  stays valid on its own and a NumPy view of an array keeps the whole file alive.
  This is the **file** view (`SceneGraph`, every LOD tier, JT class names, the
  whole key/value stream, layer tags, nothing filtered) — `Model` stays the
  export view and is unchanged. The **representation id** is the position in
  `SceneGraph::loaded` (`model.rs::loaded_pos`, keyed by `loaded_ids`/`property_id`
  so an `Instance` and its target share it) and is the instancing key: an
  `Instance` has `mesh_ids == []` and shares `subtree_mesh_ids()` with its twin.
  The server sample **instances one shape into many nodes**, so 106 representations
  carry its geometry — `representation_count` is 106 and every one of them decodes
  now; the file view and the export view agree on it.
- **`Lsg::representations()` must filter `element.geometry().is_some()`.**
  `SceneGraph::loaded` also holds Meta Data / `ProxyMetaData` late loads, so the
  raw length counts too much: block 9.5 went 7 → 3. The ids are **not**
  `0..count` — block 9.5 is `[4, 5, 6]`, plate1-9.5 `[4, 5, 6, 11, 12, 13]` — so
  `mesh(id)` raises `IndexError` unless the id is in that list.
- **`JtArray` zero copy.** `__getbuffer__(slf: Bound<'_, Self>, view, flags)` +
  `slf.into_ptr()` as the owner, `__releasebuffer__(&self, view)` frees the
  heap-owned `CString` in `view.format` and the `Box<[Py_ssize_t; 4]>` in
  `view.internal` (shape/strides/format **must not** point at a local). Indices
  are 1-D `(m,)` `int32`, vertices/normals 2-D `(n, 3)` `float32` with strides
  `(12, 4)`; `readonly = 1` always, and `PyBUF_WRITABLE` is refused. An **empty**
  channel still hands out a zero-length buffer pointed at a `static EMPTY`
  array — returning `None` makes `numpy.asarray` fall back to a 0-d **object**
  array instead of an empty `float32`. `__len__` is `dims[0]` (rows), not the
  product, so it agrees with `len(memoryview(a))` and `len(np.asarray(a))`.
  Proof: `flags.owndata is False`, `np.shares_memory` across two `asarray` calls,
  and all 106 representations of the server sample in ~0.9 s.
- **pyo3 0.30 handle classes.** `Py<T>` is **not** `Clone` — `impl<T> Clone for
  Py<T>` sits behind the `py-clone` feature (`instance.rs:2127`), so never
  `#[derive(Clone)]` a pyclass holding a `Py<_>`; use `clone_ref(py)`. `Py<Self>`
  is a valid `#[pymethods]` receiver, and `Py::get(&self) -> &T` needs
  `T: PyClass<Frozen = True> + Sync` — legal here because nothing in
  `crates/ferrojt/src` has interior mutability. **`tp_iter` must return a real
  iterator**: returning a `Py<PyList>` compiles but raises `TypeError: iter()
  returned non-iterator of type 'list'`; use
  `PyList::new(py, items)?.try_iter()?.into_any().unbind()` as `PyResult<Py<PyAny>>`
  (`Py<PyIterator>` is not `Py<PyAny>`).
- maturin on Windows: always pass `-i <venv python>` for a cross build (otherwise
  it probes the Microsoft Store `python3` stub), and put
  `.venv\Lib\site-packages\ziglang` on `PATH` — `ZIG=` is ignored.
  Never combine `--locked` with `--sdist`. `maturin upload`/`publish` are
  deprecated → use twine.
- The sdist file list comes from `cargo package --list`, so anything gitignored
  (including `data/`) is absent unless added via `[tool.maturin] include`.
- **Open3D 0.20 (`tests/test_visual.py`)**: the **first** `render_to_image()` of a
  freshly created `OffscreenRenderer` returns a black frame — render one frame and
  discard it. Creating a **second** renderer in one process aborts the Vulkan engine
  (`Precondition in destroy:1416 … MaterialInstance "unlitBackground" … still in
  use`), so the module keeps one process-wide renderer and calls
  `scene.clear_geometry()` between samples. Without `scene.set_background()` **and**
  `scene.set_lighting()` the picture is all zeros. API names:
  `MaterialRecord.base_color` (not `.color`), `scene.add_geometry(name, mesh,
  material)` (not `.add`), `TriangleMesh` has no `.materials`; the post-processing
  knobs are on `scene.view` (`set_ambient_occlusion`, `set_antialiasing(enabled,
  temporal)`, `set_post_processing`, `set_sample_count`, `set_shadowing`) and
  `Open3DScene` has no `get_post_process_setting`; `TriangleMesh` has
  `merge_close_vertices(eps)`, not `merge_vertices`. Comparison is
  `1 - mean|Δ|/255` with `FERROJT_VISUAL_THRESHOLD` (0.995) / `FERROJT_VISUAL_EPS`
  (8); references are `data/Reference/<stem>.png`, diffs go to the gitignored
  `tests/_visual_out/`. Six samples are covered — the three `example_block`, the
  two `opening_protection_plate1` and `SampleJT_Server.jt`.
- Cosmetic, ignorable: the MSVC linker note (`Bibliothek "..._ferrojt.dll.lib" …`)
  surfaces as `warning: linker stdout` for `ferrojt-python`.
- **Open3D GUI (`tests/test_viewer.py`)**: there is no `gui.O3DVisualizer` — build a
  plain `gui.Window` + `gui.SceneWidget`. `SceneWidget::Mouse` **always** runs the
  built-in interactor and always returns CONSUMED, so a Python handler cannot
  suppress it: pick the inert `Controls.ROTATE_IBL` (it only rotates an unused IBL,
  but flashes the skybox — `scene.show_skybox(False)`, and these three
  `show_*` are **methods**, not properties). `DRAG` events only arrive while the
  `BUTTON_DOWN` returned CONSUMED (mouse grabber). `Window.set_on_key` returning
  True blocks the focused `SceneWidget`; ESC closes before any callback. Ticks run
  at ~100 Hz unless a redraw is forced — `set_on_tick_event` returning True gives a
  real FPS number. With ≥2 children frames are not auto-assigned: register
  `set_on_layout` **before** `add_child` and place both from `content_rect`
  (menubar costs 28 px). `gui.MouseEvent`/`KeyEvent` have **no Python constructor**
  but every property has a setter — drive the callbacks with duck-typed fakes in
  probes. Drag and drop does not work out of the box (GLFW forwards drops to an
  empty unbound `Window::OnDragDropped`), so the viewer hooks `WM_DROPFILES`
  itself: find the window by class `GLFW30` + pid via `EnumWindows` (the callback
  receives the HWND as a **plain int**, `.value` raises), then
  `comctl32.SetWindowSubclass` — `SetWindowLongPtrW(GWLP_WNDPROC)` is silently
  never called on a GLFW window. Pass other messages to `DefSubclassProc`, keep
  every ctypes callback object alive (GC → 0xC0000005), `wintypes` has no `HDROP`
  (use `c_void_p`), `GetLastError`/`DragFinish` live in kernel32/shell32.
  Synthetic `PostMessageW` **key** messages reach `set_on_key`, synthetic **mouse**
  messages never reach open3d (raw input); `SendMessageW` from a tick callback
  crashes. Exiting aborts in `~VulkanFboCache:72` (exit `0xC0000409`) — that is
  isl-org/Open3D#7593, expected, `main()` still returns 0.
- **File→Open is the Win32 shell dialog on both sides.** The Rust viewer already
  gets it: `rfd 0.17` selects the `win_cid` backend (the Common Item Dialog,
  `IFileOpenDialog`) on Windows, so `pick_file()` in `main.rs` needs no change.
  The Python viewer used `open3d.visualization.gui.FileDialog`, which is a widget
  Open3D draws inside its own window — no favourites, libraries, network places,
  pinned folders or type-ahead — so it now calls `comdlg32.GetOpenFileNameW`
  through ctypes (`_win32_open_dialog`), with the Open3D widget kept as the
  non-Windows fallback. It returns `(available, path)`, and the caller **must**
  use that pair: `(True, None)` is a cancel and must not pop a second dialog,
  `(False, None)` is "no Win32 here" and falls back. Pass `self._hwnd` (grab it
  first) as `hwndOwner` or the dialog is not modal to the viewer. ctypes details:
  `sizeof(OPENFILENAMEW)` is **152** on x64 and `lStructSize` must be that; the
  field is `nFilterIndex`, **not** `nCustFilter`; an `LPWSTR` field rejects a
  `create_unicode_buffer` outright (`TypeError: incompatible types,
  c_wchar_Array_N`) and needs `ctypes.cast(buffer, ctypes.c_wchar_p)`; keep every
  buffer in a named local so it outlives the call. Flags: `OFN_PATHMUSTEXIST`
  `0x800` and `OFN_FILEMUSTEXIST` `0x1000` (**not** `0x8`/`0x10`),
  `OFN_EXPLORER` `0x80000` (modern dialog), `OFN_NOCHANGEDIR` `0x8`,
  `OFN_ENABLESIZING` `0x800000`. The filter is one double-NUL-terminated run of
  `"desc\0*.ext\0"` pairs.
- **A real shell dialog cannot be driven from a background process.** Probing the
  dialog works read-only — find it by class `#32770` + pid, `EnumChildWindows`
  shows the name edit as `ComboBoxEx32`/`ComboBox`/`Edit` all with id `1148`
  (`cmb13`), the type combo `1136`, OK `1`, Cancel `2` — and `WM_CLOSE` gives a
  clean cancel. But `SetForegroundWindow` from a non-foreground process fails with
  `ERROR_ACCESS_DENIED` (5), so `SendInput` (which needs `sizeof(INPUT) == 40` on
  x64 — pad the union to 32 bytes or it returns 0) injects into nothing, and
  `SetDlgItemTextW`/`BM_CLICK`/`WM_COMMAND`+`IDOK` are all ignored by the modern
  dialog. Verify the *select* path deterministically instead: stub
  `ctypes.windll.comdlg32` with an object whose `GetOpenFileNameW` writes into
  `lpstrFile` and returns 1, then read back `lStructSize`, `nMaxFile`, `Flags`,
  the filter pairs and the returned path.
- **`crates/ferrojt-viewer` (egui + kiss3d).** kiss3d 0.47 pins **egui 0.36** —
  do not bump one without the other. `draw_ui` hands out only `&egui::Context`,
  so its closure cannot touch `window`: collect flags/commands and act after the
  call. Place the panels with `egui::Window`/`Area` + `fixed_pos`/`fixed_size` +
  `order(Order::Middle)`. The prelude has no `Mat4` — import `kiss3d::glamx::Mat4`
  (kiss3d does `pub use glamx;`); `Rot3` **is** `glam::Quat`;
  never call `SceneNode3d::update()` or `world_pose()`. `add_child` panics on an
  already-parented node, so every instance gets fresh nodes; the kiss3d node
  graph is **flat**, so `set_visible` has to be re-applied down the whole row
  tree. `project`/`unproject` take **physical pixels** while egui works in
  points — multiply by `pixels_per_point`.
- **An egui `Area`/`Window` is as big as its contents, not as big as you asked.**
  `Area::end` stores `state.size = content_ui.min_size()` (`area.rs:668`) and
  `Resize::end` does the same for a non-resizable `Window` (`resize.rs:352-360`);
  `fixed_size`/`default_size` only seed the first frame. Two consequences, both
  hit here: an **empty** area collapses to a point after frame 1, which is why the
  divider handle was dead until `outliner_resize` pins it with
  `Ui::set_min_size(strip.size())`; and **overflowing children widen the panel**,
  which is why `SampleJT_Server.jt` (rows at depth 19–20, `depth · spacing().indent`
  = 360 pt in a 302 pt column) pushed the outliner to 402 pt past its own divider.
  `tree::indent_step` clamps the per-level indent to
  `(available_width − left − right) / deepest`. Labels already `truncate()`, so the
  culprit was indentation, not long names — measure before guessing.
- **Outliner width is runtime state.** `tree::DEFAULT_WIDTH` seeds
  `App::tree_width`, and four places must agree: `Viewport::of`, `App::default`,
  `outliner_rect()` and `tree::show`'s frame. `outliner_resize` returns the new
  width (clamped to `MIN_TREE_WIDTH` .. `screen.width() * MAX_TREE_SHARE`) and
  sets `fit`, because the free rectangle changed shape.
- **The outliner column has a second runtime height.** `details::DEFAULT_HEIGHT`
  (210) seeds `App::details_height`; `tree_rect`/`details_rect`/`split_y`/
  `outliner_split` must agree, and `MIN_DETAILS_HEIGHT` (90) ..
  `outliner.height() * MAX_DETAILS_SHARE` (0.6) are the clamps. Each half is its
  own `egui::Window`, so both are placed from a rect and the split line is shared
  (`tree.max.y == details.min.y`). `outliner_split` must **not** set
  `commands.fit` — the free viewport rectangle is unchanged when the column is
  split taller — while `outliner_resize` must. Dragging **up** grows the detail
  pane, so the delta is subtracted from the height.
- **kiss3d pans on the RIGHT button by default.** `OrbitCamera3d` starts with
  `rotate_button: Some(Button1)` and `drag_button: Some(MouseButton::Button2)`
  (right); winit maps Middle → `Button3`, Right → `Button2`
  (`wgpu_canvas.rs:1979`/`2200`), and pan fires from the `CursorPos` handler when
  `drag_button` is held (`orbit3d.rs:549`). `camera.rs::rebind_pan` sets it to
  `Button3` so the right button stays free for a context menu. **`fit()` rebuilds
  the camera with `new_with_frustum`, which restores `Button2`** — call
  `camera::rebind_pan(&mut fresh)` inside `fit()` *and* `camera.rebind_pan()`
  once in `main()`.
- **JT key/value properties live in the late-load meta data stream**, not the LSG
  property table (which only carries `JT_PROP_NAME` + the late loads). Read them
  with `jt::key_value_pairs(&handle_late_loads_meta(graph, node.late_loads()))` —
  that is the *whole* stream, `LAYER::`/`LAYERFILTERnnn::` plus `Name::`,
  `CAD_MASS::`, `Translation Date::` and whatever else the CAD system wrote.
  `jt/model.rs::node_properties` stores it as `Node::extras["properties"]` (a
  sorted map, `None` when empty so plain nodes and existing fixtures are
  unchanged). Neither exporter emits node extras (glTF `to_node` sets
  `extras: None`, USD reads only `model.extras["layer_filter"]`), so pinned
  counts are unchanged and the viewer's detail pane is their only exit. The
  parity test `the_key_value_properties_of_a_sample_reach_its_nodes()` must
  **sort and dedup both sides**: `jt::dump` expands an instanced subtree once per
  instance (plate1-9.5 → 6 `properties` objects) while the model keeps **one
  shared node**, and each map is paired with the node class so a set cannot be
  credited to the wrong node.
- **`ferrojt-viewer` now depends on `serde_json`** (workspace) — the detail pane
  walks `Node::extras`. `Painter::hline(x_range, y, stroke)` takes the **x range
  first**, and a `format!` placeholder cannot contain a call expression, so the
  `details.rs` number helpers return `String`s that go into locals before
  interpolation.
- **Driving egui from a test works.** `ctx.run_ui(RawInput { screen_rect, events,
  focused, ..})` + `output.drop_without_applying_deltas()`; send
  `Event::PointerMoved` / `PointerButton`. Two gotchas: hit testing uses the
  widget rects of the **previous** pass and an area is not interactable during
  its first (sizing) pass, so **two warm-up frames** must precede the press; and
  egui clears `dragged` the moment the release event is seen, so the release must
  be a frame of its own or the last move is dropped (`drag_delta` is the
  **per-frame** pointer delta, not the total — move in small steps). Pinned in
  `main.rs::tests`.
- **kiss3d frame order: anything done between two `render_3d` calls is one frame late.**
  `render_3d` → `render` → `render_chain` (`window/rendering.rs`) does
  `handle_events()` and then `render_single_frame()`, which calls
  `camera.update(&self.canvas)` at `:240` and `scene.data_mut().prepare(..)` at
  `:597`. Re-placing the backdrop from the app loop (after the previous frame was
  submitted) used the previous frame's eye, so the gradient **swam during a drag
  and snapped back when it stopped** — that was the "background moves with a delay"
  report. Fix: `camera.rs::ViewerCamera` wraps `OrbitCamera3d`, holds the
  `Backdrop` and calls `backdrop.place(&self.orbit, aspect)` inside
  `Camera3d::update`, i.e. the same frame. It `Deref`s to `OrbitCamera3d`, so
  `lod::select`/`picking::pick`/`trihedron::show` are unchanged — but **`fit`
  assigns `*camera = fresh`, so it must receive `camera.inner_mut()`** or the
  wrapper (and its backdrop slot) is replaced. Proof: `target\imgcmp.py` on
  auto-orbit frames — 100 k+ px change across the image while a 100×60 gradient
  patch has **0 px changed, max delta 0**.
- **FPS is measured in the app loop, not by kiss3d.** `Window::render_timings`
  reports CPU/GPU cost *inside* a frame, never the interval between frames. The
  `FpsMeter` in `main.rs` counts ticks over a `FPS_WINDOW` (0.5 s) window,
  divides `frames - 1` intervals by the span, and **restarts the window when a gap
  exceeds `STALLED_FRAME` (0.25 s)** — otherwise a blocked frame reads as a
  collapse. `fps_text` prints `-- fps` until the first window closes. Reserve
  `FPS_RESERVE` (58 pt) in the toolbar's right-to-left strip next to
  `ABOUT_RESERVE`.
- **A newly opened file opens only the top levels of the outliner**
  (`TREE_OPEN_LEVEL = 1` → `ViewerScene::collapse_below`). A server assembly has
  thousands of rows and twenty levels, so expanding everything shows nothing but a
  scrollbar. The pure `collapsed_below(rows, child_rows, level)` seeds `collapsed`
  with every branch at `depth >= level`; it keys on **child rows**, not
  `Row::has_children`, because a child skipped by the path cycle guard is not a
  row. Pinned in `scene.rs::tests::only_the_top_levels_stay_open`.
- **The viewport gradient is a scene quad, not a background** (`backdrop.rs`).
  `Window::set_background_color` is a flat clear colour; the skybox is
  equirectangular so its image rows follow **world Y** (the band rotates with the
  camera) and it forces IBL with no way to disable it; a `PostProcessingEffect`
  cannot work because the ping-pong targets carry their own depth, not the scene
  depth. So: a unit quad with `emissive = white × emissive_map` (an 8 px
  `RgbaImage` added via `TextureManager::add_image` — linear filter, no mipmaps),
  black albedo and `set_light_layers(0)` so it is unlit, `DEPTH 0.9` / `COVER
  1.05`, re-placed against the camera every frame. It spans the **whole canvas**
  (measured: a solid-red quad covers x 9..1008, y 38..787) — the panels merely
  occlude it. `OrbitCamera3d::view_transform()` is the **world→view** pose. The
  tone map darkens sRGB bytes by ~20 levels, so `TOP [132,138,150]` renders as
  ~(114,121,134). Being a plain scene node it is not an outliner row, not picked,
  not an LOD candidate and not in `world_bounds_all`.
- **egui's `OpenUrl` output is never read by kiss3d 0.47**, so the About button
  calls `webbrowser::open` **after** `draw_ui` returns. `ABOUT_RESERVE` exists
  because egui 0.36 has no text-measurement API — the status row has to leave the
  right-hand strip free for the right-to-left `Layout`.
- While the pointer is on the divider strip egui owns the mouse and kiss3d drops
  the events (`events.rs` consults `is_egui_capturing_mouse`), so dragging the
  divider does not also orbit the camera.
- **Never `eprintln!` per outliner row.** 4489 lines per frame starves the app so
  much that `PrintWindow` captures a frame with **no egui panels at all** — it
  looks like a catastrophic rendering regression but is only the log flood.
- **Viewer camera fit: `fov` is VERTICAL over the FULL canvas** (kiss3d
  `orbit3d.rs:457-461` computes `aspect = framebuffer.x / framebuffer.y`), and
  0.47 has no render-area API, so the egui panels only occlude. `fit` therefore
  sizes against the free rect left by the outliner/toolbar and then shifts
  `set_at` by `view.inverse_transform_vector(Vec3::X/Y)` to re-centre the model
  in the visible part. `Model::bounds` is **never set** by the JT reader
  (`info --json` → `"bounds": null`) — compute bounds from the slots.
- **Re-fit the depth range in `fit()` or you get Z-fighting.** kiss3d 0.47 has no
  near/far setter and no polygon offset, so `fit` rebuilds the camera with
  `OrbitCamera3d::new_with_frustum(fov, radius*NEAR, dist + radius*FAR, eye, at)`
  (`NEAR 1e-3`, `FAR 1e2`, `MIN_DIST 2e-2`) and assigns it. The default
  `1e-3 .. 1e6` on the 82.6 mm server model gave a depth resolution of ~0.12 mm at
  the centre (`z²/(near·2²⁴)`, `Depth32Float`, `CompareFunction::Less`, no bias)
  and 44 genuinely coplanar AABB pairs (< 0.02 mm) plus 79/105 instances thinner
  than 0.6 mm hatched. Call `set_min_dist`/`set_max_dist` **before**
  `set_up_axis`, and set `set_at`/`set_dist` after constructing.
- **Viewer transform composition: a JT `Transform` is `T·R·S`.** kiss3d keeps
  `world_scale` separate from `world_transform` (`do_propagate_transforms`) and
  glamx `Pose3 * Pose3` ignores scale, so a parent's scale never reaches a
  child's translation. Compose as
  `Pose3::from_parts(parent_scale * child_translation, child_rotation)` and put
  the accumulated scale on the **drawable** (`SceneNode3d::mesh(mesh, scale)`),
  never on the group. `SampleJT_Server.jt` has nine assembly nodes at scale
  1/25.4 (inch→mm); the split composition inflated the fit AABB ~25× (557 mm
  against 48 mm) and the model framed tiny. The viewer AABB must match
  `target/glb_bounds.py <export.glb>` — that script composes the GLB chain and
  is the cross-check.
- **Viewer probes:** the display DPI is 1.25, so a screenshot harness must call
  `SetProcessDpiAwarenessContext(-4)` or the capture is cropped to 800×600.
  `window.size()` is physical px, `scale_factor() -> f64`, egui `screen_rect` is
  in points. A **running viewer blocks `cargo build`** (os error 5 on the exe) —
  `Stop-Process -Name ferrojt-viewer -Force` first. `close_key` defaults to
  `None`, so `Exit` calls `window.close()`. Against the JT WARN flood use
  `RUST_LOG=error`; `ferrojt-cli info` prints bounds only with `--json`.
  To compare two captures, use `\.venv\Scripts\python.exe target\imgcmp.py a.png
  b.png … [--patch X Y W H]` (numpy/open3d — PIL is not installed); the PowerShell
  pixel-diff scripts silently produce empty arrays and false "0 px changed".
- **USD (`export/usd.rs`) is hand-written ASCII — the grammar is strict.** No USD
  dependency (`#![forbid(unsafe_code)]` rules out an FFI binding), so `.usda` is
  emitted with `format!` and checked against `usd-core` in a scratch venv
  (`target\_usdvenv`, gitignored — **not** a project dependency; `target\usd_check.py
  <file.usda> <MESHES> <TRIS> <MATERIALS>` reports `OK` per sample). Parse traps:
  `prepend apiSchemas = ["MaterialBindingAPI"]` is **prim metadata** and must sit in
  the parenthesised block after the prim name (in the body it is a parse error); a
  property's metadata block opens on the line that closes its array — `] (` … `)`, a
  `(` on its own line is a parse error (that is where `interpolation = "vertex"`
  goes); `quatf xformOp:orient = (w, x, y, z)` is **real first** while JT is `x,y,z,w`;
  identifiers are `[A-Za-z_][A-Za-z0-9_]*` so JT dots become `_`.
- **USD: the sibling-name `BTreeSet` is created once per parent, not per child.**
  `Plan::assign` hoists `let mut sibling: BTreeSet<String>` above the child loop; a
  per-child set lets two prims land on one path (`Duplicate prim 'shcs_asm'` →
  `Failed to open layer`) — the plate samples instantiate one part twice under one
  parent and both children carry the same name.
- **USD: references target the per-mesh *group*, and that group is not instanceable.**
  Consumers `prepend references = [</FerroJT_Geometry/GEOMETRY_n>]` because that group
  is where `rel material:binding` sits (a reference only carries what the referenced
  prim states). Making the group `instanceable` turns every consumer into an instance
  proxy and any later write fails with "authoring to an instance proxy is not
  allowed" — only the inner `def Mesh` carries `instanceable = true`. A node reached
  from several parents gets a thin wrapper that `prepend references` the shared path
  and **authors no ops of its own** (its `doc` says `JT <kind>: shared subtree`). The
  library `Xform` is a **sibling** of the default prim with `token visibility =
  "invisible"`.
- **USD counts are pinned in `crates/ferrojt/tests/usd.rs`** — meshes / triangles /
  materials / shared wrappers: block 8.1, 9.5, 10.3 → 1 / 12 / 1 / 0;
  plate1-8.0 → 2 / 617 / 2 / 1; plate1-9.5 → 2 / 486 / 2 / 1;
  `SampleJT_Server.jt` → 106 / 129067 / 23 / **529**. Materials collapse by
  **shading values** (diffuse, opacity, metallic, roughness, emissive, `doubleSided`)
  — not name — so `def Material "Material_` count ≤ `Model::materials.len()`; the test's
  `shading()` key is `[u32;10]` and must mirror `UsdMaterial::same_shading` exactly.
  `number()` uses Rust's shortest round-trip form, so asserts expect
  `translate = (1, 2, 3)` and `extent = [(0, 0, 0), (1, 1, 0)]`, **not** `25.0`.
- **No sample names an active layer filter**, so no sample's USD records
  `ferrojt:layer_filter` in `customLayerData` (that is how a consumer tells
  "unfiltered" from "filtered"). block-9.5/10.3 and plate1-9.5 *declare* `ALL`
  (+ `LAYCUR`) and leave `ACTIVELAYERFILTER::` empty; plate1-9.5 tags the
  **instance target** (the `MetaData` node the two `Instance`s point at, position
  11) with `LAYER:: = 20`, **not** the `Part` — the block 9.5 `Part` (position 2)
  is the sample where the tag sits on the part itself. A *named* filter is
  recorded via `Model::extras["layer_filter"]` → `ferrojt:layer_filter` /
  `int[] ferrojt:layers`.
- **usd-core 26.11 Python API** (for `target\usd_*.py` probes): no
  `Stage.GetCustomLayerData` — read `stage.GetRootLayer().customLayerData`; `Prim` has
  no `GetCompositionErrors` (only `Stage`); `Relationship::GetTargets()`, not
  `GetResolvedPath`; `prim.GetMetadata("references")` returns an `Sdf.ReferenceListOp`
  which is **not iterable** — use `GetAppliedItems()`, because a `prepend` leaves
  `addedItems`/`explicitItems` empty; a reference's path is `.primPath`; `Sdf.Path` has
  no `startswith`; `UsdValidation.ValidationContext` takes validators, not a `Stage`.
- **`flatten` is a transform of the neutral `Model`, not a writer**
  (`model/flatten.rs`): `Model::flatten() -> Flattened` (merged
  `vertices`/`normals`/`indices` plus the `groups` that keep the colours apart)
  and `Model::flattened() -> Model` (one node, one mesh), so glTF, USD, JT and
  the Python buffers all get it without a special code path. It composes the
  transforms of the parents down onto the vertices (JT matrices are column major
  `T·R·S`) and emits an instanced subtree **once per placement**; a node that
  reaches itself is treated as a cycle and emitted where it closes. Only the
  triangle lists are merged — an index naming a slot past the vertex array pads
  zeros — and materials are **not** globally deduped: each contiguous
  same-shading run is one group, which lets one glTF mesh carry one primitive
  per colour. USD then has to bind **per primitive** (`export/usd.rs` passes the
  binding into `write_primitive` only when the primitives of a mesh disagree),
  so the flattened plate is 2 library `Mesh` prims with 800 triangles and
  `target\usd_check.py` takes `2 800 2`. For JT two consequences are documented
  rather than fixed: a `Shape` holds exactly one material, so a flattened file
  keeps the colour of the first group only; and `jt/encode.rs` welds coincident
  positions, so merging parts that touch (the plate and its screw) is **no
  longer a closed two-manifold** — with no revision requested the writer falls
  back to JT 8.1 on its own, with 9.x/10.x requested it refuses. Pinned by
  `crates/ferrojt/tests/flatten.rs`,
  `test_a_flattened_file_written_from_a_model_renders` (the strict JT2Go oracle)
  and the flattened picture test, which compares against the **same**
  `data/Reference/<stem>.png` as the unmerged one.
- **`tests/test_visual.py` is flaky by 1–2 random samples per run** (similarity ≈0.13,
  "100% of the pixels differ") from the documented Open3D blank-frame issue.
  `shoot` now retries `MAX_FRAMES` (32, env `FERROJT_VISUAL_FRAMES`) and keeps the
  first frame whose brightest channel passes `FRAME_MIN` (200 — a blank frame is
  0, the lit background is 231), and when that still fails it spawns fresh
  `--render` sub-processes (`render_glb_fresh`, `FRESH_ATTEMPTS` 3, env
  `FERROJT_VISUAL_PROCESSES`) because a second renderer **in one process** aborts
  the Vulkan engine. The merged server mesh of the flatten test (~1.19 M
  triangles) can burn ~28 of those frames; the single-sample runs are stable.
  `pytest tests\test_ferrojt.py` alone remains the fast stable check.
- **JT writer: only the LSG is re-serialised** (`jt/write.rs`). There is **no JT
  writer anywhere in `example/`**, so the framing was read out of the sample
  bytes. Representation/meta data/PMI/B-rep segments are copied byte for byte
  out of the source (`write_to_bytes` therefore needs the source `data`), and a
  segment is dropped only when every referrer is gone. The new TOC is always
  appended at the **end**; 5 of 6 sources put it right after the header — both
  legal.
- **Writer version fields are gated per class**: `RangeLod` (its second
  version), `Shape` and `Instance` only when `major > 8`, while `MetaData` and
  `Part` write version/reserved fields **unconditionally**. Writing one extra
  byte per node shifts everything and surfaces as
  `element body read past its declared end` on JT 8.x.
- **An attribute referenced by several nodes is written once**, under the first
  referrer, and numbered once — dedup on the attribute position in **both**
  `number_nodes_and_attributes()` (`seen`) and `lsg_body()` (`written`).
- **An `Instance` target is one bare `i32`**, not a length-prefixed vector
  (`Instance::read` reads a single `Deferred`). A wrong target makes
  `jt::dump`'s `recurse_down_the_tree` (no cycle guard) stack-overflow.
- **Compression must be written as flag 2 / algorithm 2** (zlib, i.e.
  `codec::deflate`). XZ (flag 3) is read but never emitted, even though
  block-10.3 and the server sample store their LSG that way. `dataLength` counts
  the algorithm byte, so it is the **compressed length + 1**. **Never emit flag
  0** for a kind that carries a compression envelope: the file parses here and a
  strict consumer refuses it, which is why `compress` defaults to `true` in
  `WriteOptions::default()`, in the CLI (`--compress` / `--no-compress`,
  `default_value_t = true`) and in Python (`write_jt(compress=True)`). Only the
  two test variants that deliberately check the uncompressed path pass `false`.
- **A non-LSG segment ends with a property table of its own**: end-of-elements
  (`i32 16` + 16×`0xff`) then `i16 version 1` + `i32 0` rows = **6 bytes from JT
  9.x on**. Writing a shape segment without it makes `ferrojt-cli info` report
  leftover bytes; `target\eoe.py` checks `after_eoe=6` per segment.
- **A `Shape` node has a vertex-binding tail after `size`/`compression`, and its
  layout is per revision**: 8.x = three `i32` bindings + 4 quantization bytes;
  9.x = `i16` version + 8-byte binding + quantization + a **second** binding only
  when `version != 1`; 10.x = `u8` version + one 8-byte binding. The reference
  *reads* all of it, so omitting it corrupts every following node. Parsed as
  `object::ShapeLod` (`Vertex1`/`Vertex2`/`Binding`/`None`) and re-emitted
  verbatim by `write.rs::write_shape_lod`; a coarse tier of a sample quantizes
  with `11 06 0b 06`, so zeros are not a stand-in.
- **A `Partition` node is not just a Group**: `i32 flags | mb string file_name |
  box | f32 area | 6×i32 counts`, and the **second (untransformed) box is stored
  only when `flags & 1` and `major < 10`** — JT 10.x sets the flag and stores
  nothing. All of it round-trips now (`object::Partition` gained `flags`,
  `bounds`, `area`, `counts`, `untransformed_bounds`); `read_int_pairs` returns
  `[i32;6]` and `write_counts` takes it.
- **A property atom carries a class version from JT 9.x on**: `length | guid | base
  | [oid if major>8] | [class version if major>8] | state u32 | [class version if
  major>8] | mb string`. The version is `2` at 10.x and `1` at 9.x. Sample 10.3:
  `2d 00 00 00 | guid | 05 | 0e 00 00 00 | 02 | 00 00 00 40 | 02 | 07 00 00 00 |
  "SUBNODE"`.
- **That class version must name the revision the file is written at — for every
  property atom, including a class the reader does not know. Zero is fatal.** A
  strict consumer reads the **first** one to look the class up and refuses the
  whole file when the value names nothing it knows: that is the
  `Corrupt file detected / Aborting import / Unable in find root node` report on
  a rewritten file, and it is what `jtinfo.exe` said about a 9.5 → 10.3 rewrite
  until it was fixed. `build::property_class(version)` (made `pub(crate)`) is the
  single rule — 1 at 9.x, 2 at 10.x — used by the builder in both slots and by
  `write.rs::string_property_body` / `late_loaded_property_body`. An **unknown**
  class cannot be re-serialised, so `write_property_atom` clones the atom and
  patches the object id and **both** versions in place (`set_property_class`).
  **The width of the version field belongs to the revision being written, not to
  the source** — byte at 10.x, word below. A consumer finds the value at the
  state word, so a word-framed atom inside a 10.x file makes it read the value
  two bytes early and give up on the rest of the property table: the file
  parses, `jtinfo` reports a root, and an external renderer draws **nothing**,
  because the shape late loads further down the table are never reached. That
  was the last blank render of a 9.5 → 10.3 rewrite. The real 10.3 sample proves
  the rule: it stores its one atom of the unknown class `10dd1019` **byte**
  framed (`len 15`) where the 9.5 sample stores the same class **word** framed
  (`len 17`) — a CAD system re-frames it. `set_property_class` re-frames the
  body and rewrites the atom's own **length prefix**, which changes with it.
  Detect the *source* width from the state word `PROPERTY_STATE = 0x4000_0000`
  (`has_state` at `at+1` = byte, `at+2` = word); `PROPERTY_VERSION_OFFSET = 25`
  is the first version inside the atom. `element.base_type` is `0` for an unknown
  class, so never use it for framing. Pinned by five unit tests in `write.rs`
  and, at file level, by `a_rewritten_file_names_the_class_version_of_its_own_revision`
  (which now also asserts the **width** matches the revision) and
  `an_unknown_property_class_is_re_framed_for_the_revision_being_written` in
  `tests/write.rs` — both frame the LSG payload itself (two end-of-elements
  sections; the property atoms are in the second) because
  `model.rs::read_property_atoms` **skips** an unknown-class atom entirely, so
  the parsed graph cannot show it. Note `class_of` returns `None` for that class,
  so a scan must test `!matches!(known, Some(c) if c != Class::Property)`, not
  `== Some(Class::Property)`.
- **`jtinfo`'s `# Properties:` count is NOT a geometry oracle.** The 9.5 source
  and a good 9.5 rewrite report 18, but a *good* 10.x rewrite reports 9 and a
  model-path 10.x file reports 0 — all of them render. Only `# JtkAssemblies: ≥ 1`
  plus the NOVO render decide anything.
- **A root `Partition` MUST carry a non-empty file name and a property-table row,
  or a strict consumer refuses the file.** `JTData_SceneGraph.cxx:152-176` creates
  the root node **only when `FileName()` is non-empty** and then asserts one
  exists — that is the `Bad file detected / Unable in find root node` report on a
  glTF→JT conversion. So the builder writes the name **twice**: the `Partition`
  struct field *and* a property row for object id `0` with
  `PartitionType = "Assembly"` + `JT_PROP_NAME`, pushed **first** in `lsg_body`.
  Samples put a CAD name in the property (`example_block_nx1872.asm;0;16777215:`
  at 10.3, `san666.asm;0;0:` for the server) and the `.jt` name in the field; we
  write the **same** `<stem>.jt` in both — legal, because `Node::bind_name`
  keeps only the first `;` token. `DEFAULT_FILE_NAME = "model.jt"` is the
  fallback for a bare `write_to_bytes()`; never write an empty name.
- **Which name, from where**: `model_to_file` / `export::write_to_file` /
  `JtBuilder::write_to_file` / Python `save` name the root after the **output
  path** (`build::file_name_for_path`), `model_to_bytes` / `to_bytes` /
  `export::to_format` after `Model::source`. That is why `export::write_to_file`
  and `Model::save` special-case `Format::Jt` and call `model_to_file` — the
  writer has to see the path. Split the extension at the **last** dot
  (`rsplit_once`): `example_block_jt9.5.jt` has a dot inside its stem, and
  `split_once` produced `example_block.jt`. Python: `JtBuilder.set_file_name()` /
  `.file_name`.
- **`save()` and `to_bytes()` no longer agree byte for byte** unless a name was
  set — `save` derives one from the path. `test_a_built_file_reads_back_as_jt_10_3`
  and `test_every_supported_revision_reaches_its_own_banner` therefore call
  `set_file_name` matching the path stem before comparing.
- **A property atom is emitted exactly when a surviving table row refers to it.**
  Numbering and emission must use one rule (`collect_rows()` decides the
  surviving rows *before* `number_properties()`), or a second write of an
  already written file shrinks it — that is the fixed-point assert in
  `a_lossless_round_trip_reproduces_the_file` (`bytes.len() == twice.len()`).
- **`SampleJT_Server.jt` carries 55 property atoms of the unknown class
  `10dd1019`** (not in `guid::types` nor in the C++ sources) that **are** framed
  like known ones: `21 00 00 00 | <guid> | 05 | 57 0f 00 00 | <body>` — base
  type 5 at offset 20, real object id 3927 at offset 21, length 33. But
  `read_element_envelope` returns `object_id = 0` and a 20-byte envelope for an
  unknown class, so `write.rs::read_property_atom` reads the id itself
  (`UNKNOWN_ENVELOPE = 20`, `OBJECT_ID_OFFSET = 21`). Dropping those atoms lost
  55 table pairs and, through `read_property_table`'s `JT_PROP_NAME` key lookup,
  55 string values (`PROPERTY_STRING 4562 → 4507`).
- **A filtered write legitimately drops `PROPERTY_LATE_LOAD` atoms** with their
  representations (block-9.5: 47 → 45 atoms, rows stay 6). `tests/write.rs`
  therefore compares `values()` (every property that is *not* a late load) for
  equality and `late_loads()` as `<=` — asserting the whole property list equal
  is wrong.
- **A CDP package is never codec 0 unless it is empty.** `encode.rs::package2`
  writes the value count first, and only the count-0 case may return before the
  codec byte; emitting `0` (null) for a non-empty package makes every consumer
  read **zero** vertices, i.e. a file that loads and draws nothing.
- **The JT 9.x fixed-range bitlength2 branch cannot carry a span ≥ 2³¹** — the
  spec reads the range back as a *signed* word and fills every value with `min`
  when it is not positive. Such packages take the variable branch
  (`encode.rs::write_variable_bits`, `CHANGE_BITS`/`RUN_BITS` = 3).
- **A topological shape needs a flag package per normal**, after the normals:
  `i32 count` + `package2(&vec![0; n])`. Without it the decoder stops short of
  the next channel.
- **The `Shape` vertex-binding tail is framed by the revision being written,
  not the source**: 16 B at 8.x, 22 B at 9.x, 9 B at 10.x. Keeping the source
  framing across a revision change shifts every following node.
- **`class_of` must map `"RangeLOD"` and `"LOD"` to `BuildNode::RangeLod`.**
  Anything else silently downgrades a level-of-detail span to a `Group`, and a
  consumer walking `Part → RangeLod → tier` finds no tier.
- **A context-7 attribute mask needs one value in *each* auxiliary mask stream,
  even a zero.** `mask_context(degree) = (degree - 2).clamp(0, 7)`, so context 7
  means a coder vertex of degree ≥ 9, and the reader
  (`JtDecode_MeshCoderDriver.cxx:1390-1411`) indexes `_faceAttributeMask8_30` and
  `_faceAttributeMask8_4` with the **same read position** as the mask itself and
  ORs both at bit 30. `encode.rs` therefore pushes a `0` into
  `Streams::attr_masks_high` for every context-7 mask symbol and emits it after
  the eight degree packages — **two** packages at 9.x, **one** at 10.x, where the
  second is never read. Empty aux packages make a strict consumer refuse that
  face, i.e. the whole shape renders blank. A cube (degrees 4/6) and an
  octahedron (degree 4) can never reach context 7, and the **plate** is the only
  sample with degree-9 coder vertices (its screws) — which is why every block
  rewrite rendered and only the plate came out empty. Splice proof: swapping only
  those two packages of the CAD's own shape segment gives 207 109 ink px, while
  swapping only the context-7 mask word gives 0. Pinned by
  `a_context_seven_face_carries_both_auxiliary_mask_streams` (`encode.rs::tests`,
  fixture `enneagon_bipyramid()` = two apices of 9 triangles). When walking the
  body by hand, the 64-bit vertex binding is **8** bytes (flag + `i32` + 3), not
  9, and a count-0 package decodes to `Payload::Null(vec![])` — test emptiness
  with `out_val_count()`, never `is_empty()`.
- **`jtinfo.exe` is not a geometry oracle.** Its `# Triangles:` comes from
  key/value metadata, so it reports a healthy number for a file that draws
  nothing. Use it only for the structural verdict; the geometry oracle is
  `C:\Tools\20261113_Mercedes_NOVO\render-jt.mjs` (Node + Playwright driving
  `UberEngineServer.exe`), wrapped by `target\novorender.py`, which prints the
  count of ink pixels of the PNG it writes (`-1` = no PNG = hang). Every good
  render measures the same as its source: block **590 296** at 9.5 and at 10.3,
  plate1-9.5 **291 167** (rewriter *and* model path), `SampleJT_Server.jt`
  **356 011** (written at 9.5, all LODs, no layer filter), and a `TestBlock.glb`
  conversion **628 416**. One plate representation (a 152-triangle screw) alone
  measures 207 109 — that is the fixture the encoder bisection used.
- **JT2Go is the STRICT oracle, and it is the only one that caught three writer
  defects.** `tests/jt2go_picture.py` drives an installed Siemens JT2Go
  (`C:\Users\<user>\JT2Go\Products\JT2Go\VisView_NG.exe`, **not** `visview.exe`)
  through ctypes and counts the pixels it really draws. NOVO and `jtinfo`
  accepted every one of the files that JT2Go drew blank, so "NOVO renders it"
  proves nothing about a strict consumer. Three distinguishable outcomes, never
  conflate them: **PASS** with a large ink count, **FAIL 352** (the file loads,
  the shape is rejected — 352 px is the trihedron icon alone, nothing else), and
  `JT2Go never loaded …` after `LOAD_TIMEOUT` 120 s (the file is structurally
  refused, no MDI document ever appears). `DEFAULT_MIN_INK = 2000` separates
  blank from drawn by three orders of magnitude.
- **JT2Go window anatomy and capture.** The frame is created **off screen** at
  negative coordinates — `ShowWindow(hwnd, SW_MAXIMIZE=3)` first. Frame title
  prefix `JT2Go`, class is an unstable MFC string. Frame → `MDIClient` → MDI
  document class `AfxFrameOrView140` **titled with the file name** (= load
  complete, ~5 s) → drawing surface class **`DMUtils`** (913×879 maximized).
  Grab `DMUtils`, not the frame, or the ribbon/outliner/status bar pollute the
  ink count. Only `PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT=2)` on a
  `GetWindowDC` + `CreateCompatibleBitmap` + `GetDIBits` (top-down 32-bit,
  `biHeight = -height`, stride `((w*4+3)//4)*4`) captures the OpenGL viewport.
  Background = median of the four viewport corners (white); geometry when
  `max|Δchannel| > INK_EPS (24)`. PNG byte size is a fast proxy: 18 762 B
  renders, 4 896 B is blank. Set `SetProcessDpiAwarenessContext(-4)` and
  `$env:PYTHONIOENCODING='utf-8'`. The script does **not** glob — expand the
  file list in PowerShell.
- **JT2Go ink baselines** (all through every writer path): `example_block` at
  every revision and every path (rewrite 8.1→8.1, 9.5→9.5, 10.3→10.3,
  glTF→8.1/9.5/10.3) → **387 467**, geometry 738×651.
  `opening_protection_plate1` 9.5 rewrite / model path→10.3 / glTF→10.3 /
  glTF→8.1 → **226 982**; the 8.0→8.1 rewrite **226 971**. Hand-built chain cube
  and `out.jt` (`TestBlock.glb`→10.3) → **352 001**. `sweep_SampleJT_Server_9.5`
  → **143 741**. A **flattened** file draws too, and measures far above its own
  baseline because one merged shape is framed much larger: block 9.5
  `write --flatten --jt-version 10.3` → **1 691 354** (geometry 2301×1434),
  plate 9.5 `write --flatten` (auto JT 8.1) → **986 849**. The pytest of
  `test_a_flattened_file_written_from_a_model_renders` only requires it to clear
  `DEFAULT_MIN_INK`, not to match a number.
- **JT2Go verifies the hashes.** It checks the topology **Composite Hash** *and*
  each **vertex-data hash** independently and blanks the shape on any mismatch —
  it does not merely tolerate them like the lenient readers here. The primitive
  is **Bob Jenkins lookup2 (1996)**, published in the format spec itself
  (`example\Documents\PSI14_1_V3_JTIAP_Format_Description_and_annexs.pdf`
  Annex E, pages 231–234; and `Siemens JT V10.6 Format Description Rev A`
  Annex C — text corpora `target\_pdfall.txt` 5 119 617 B and
  `target\_jt106.txt` 1 165 506 B, extract with `target\pdftext.py`, stdlib
  only). The spec prints `hash()` (byte key), `hash3()` (u16 key, = `jthash16`)
  and the `mix` macro, but **never prints the body of `hash2()` (the u32 key,
  = `hash32`)** — it is pinned by evidence instead: 3 words per `mix`, `c +=
  number of words` added **after the main loop and before the tail switch**,
  1-word remainder → `a`, 2-word remainder → `a` then `b`, final `mix`, return
  `c`. That transcription lives in `target\compall.py` and must be **imported,
  never retyped**. The Rust implementation is `jt/hash.rs` (`hash32`, `hash16`,
  `composite_hash`, `TopologySymbols`) — the module is **crate-private**, so an
  integration test cannot call it and must check the bytes instead.
- **The composite hash is revision-dependent: 11 mask-region calls at 9.x, 10 at
  10.x.** The chain (spec §7.1.4.1 Figure 95) is 8 degree arrays → valences →
  groups → `hash16` flags → attr masks 0..6 → attr masks[7] low 32 → attr
  masks[7] high 32 → (`mask8_30`) → (`mask8_4`, **9.x only**) → large masks →
  split syms → split positions, and it **ends there** — the vertex records are
  *not* part of it. Reproduced for 12 of 12 CAD shapes. The flag is
  `MeshInput::auxiliary_pair = cursor.major() < 10`.
- **The vertex-data hash granularity is revision-dependent too**: JT 9.x feeds
  **one word per `hash32` call**, JT 10.x feeds **one whole component array per
  call**. Component-major, record-minor, over the **unique** coordinates.
  Verified 6/6 at 9.5 and 6/6 at 10.3. Implemented in
  `encode.rs::attribute_hash(values, version)`. Granularity, not content, was
  the whole difference.
- **A JT 10.x tri-strip set wraps its coder input in a second, embedded logical
  element** immediately after the first vertex binding:
  `i32 length | guid ada530f8-4cbe-bc4f-9b5f-b9269278d2e1 | u8 base type 9 |
  i32 object id 1 | u8 TopoMesh version 1 | u32 vertex-records id 0 | u8 flag 1`
  = **31 bytes** (`TOPOLOGY_EMBEDDED_HEADER`), coder input 31 bytes later. At
  9.x the same place holds those last three fields **loose** and unframed. The
  embedded length stops **exactly one byte before the outer element closes** —
  the caller appends the version byte that closes the outer element, so
  `length = outer - 1 - (at + 4)`. Because the mis-framed header had the **same
  total width**, everything downstream still parsed: this reader, NOVO and
  `jtinfo` all accepted a file JT2Go drew blank. The guid appears in **no** spec
  (search the spec's own `0xXXXXXXXX, 0xXXXX, …` notation, not the dashed form —
  92 hits in `_jt106.txt`, none of them this one), in no file of `example/`, and
  in no `guid::types` — it is taken from the sample bytes and now a named
  constant in both `encode.rs` and `object.rs`, and `read_shape_lod_data`
  **rejects** a different guid outright. Fixing the framing alone took
  `hx103.jt` from 352 px to **387 467** while leaving the sparse attribute masks
  and the all-bitlength2 codec mix in place — those two suspects are **not**
  blockers, do not chase them.
- **`write_topological_body` writes TWO version fields before the first vertex
  binding, WORD-wide (2 B) at 9.x and BYTE-wide (1 B) at 10.x.** So the coder
  input sits at body offset **18** at 9.x and **41** at 10.x. A test that
  compares the two revisions must account for it.
- **`maturin develop` after EVERY Rust writer change** before regenerating any
  Python-driven fixture: a stale `_ferrojt` extension silently reproduces the old
  framing while the CLI produces the new one — that cost a whole bisection round
  when `chain_10.3.jt` still rendered 352 px after the source was fixed.
- **`17 element bytes remained after reading, corrected` is benign** — it shows
  up on `hx103.jt`, `chain_9.5.jt`, `chain_10.3.jt` and `w103to103.jt`, all of
  which render perfectly. Always run a warning check against a known-good
  control before chasing it.
- **When a byte-level framing test fails, check the WRITER first** by running the
  probe (`target\emb2.py <file> <control>`, `target\headhex.py`) on the exact
  file the test builds. Here the writer was byte-identical to the CAD sample and
  the bug was in the test's offset arithmetic. Keep **cursor-relative and
  file-relative offsets strictly separate** inside one helper —
  `embedded_elements()` in `tests/write.rs` mixes them at its peril: `at =
  cursor.position() + shape.position()` (read *before* `read_len()`),
  `outer = atom.body_offset + atom.body_len`, expected `outer - 1 - (at + 4)`.
  `Cursor::read_len()` already returns `usize`.
- **The revision a file is written at is `jt::JtVersion`** (`jt/version.rs`,
  re-exported, `Default = V10_3`). Consts `V8_0/V8_1/V9_5/V10_3`; the gates are
  methods — `offsets_are_32_bit()` (<10), `versions_are_bytes()` (≥10),
  `has_object_versions()` (>8), `has_attribute_trailer()` (≥10),
  `has_vertex_based_representation()` (<9), `has_topological_representation()` (≥9)
  — and `compatible(other)` is `(major < 9) == (other.major < 9)`. **`FromStr`
  calls `validate()` itself**, so `"7".parse::<JtVersion>()` is an `Err` and a
  bare major fills in 8.1 / 9.5 / 10.3; CLI `--jt-version` and Python
  `version=` therefore never see an unsupported value.
- **The writer keeps the revision of the source when `version` is `None`** —
  that is what makes the rewrite lossless, so do not "make it consistent" with
  the builder's 10.3 default. A **family change** (8.x ↔ 9.x/10.x) is refused:
  `a JT {src} file cannot be written as JT {v}: JT 8.x stores its triangles in
  the vertex based representation, JT 9.x and 10.x in the topologically
  compressed one, and only the coder of the revision of the source file is
  available`. A **major** change sets `restructured = version.major !=
  source.major`, which re-names the shape segments (`segment_kinds`) and
  re-encodes them; a same-revision write must leave both alone or the server
  sample's kind-7 rows become 6 and the write tests fail.
- **"9.x names every shape segment type 6" is per file, not per revision.**
  `example_block_jt9.5` stores 6,6,6 and plate1-9.5 six kind-6 rows, but
  `SampleJT_Server.jt` is a 9.5 file storing its 106 shape segments as kind 7.
  That is why the whole restructure is gated on `restructured`.
- **A non-LSG element envelope carries an object id from JT 9.x on**: pass
  `element_stores_object_id(major)` (i.e. `major > 8`) to
  `read_element_envelope`, never a literal `false` — reverting that breaks the
  builder's 9.x/10.x output in 6 tests. It is re-exported from `jt/mod.rs`
  because rustdoc warns about intra-doc links to private items.
- **`write.rs::write_property_atom` parses an atom with the framing of the
  SOURCE revision** (`self.file.header.major`) and re-serialises it with the
  target's. Parsing with the target gates gives
  `length N exceeds the remaining M bytes` (`reader.rs::read_len`).
- **The topological coder explodes and re-orders the vertex table.** For
  block-9.5 → 10.3 the float count goes 72 → 108 per representation and
  `JtMesh.vertex_count` (the **unique** count) 24 → 36; index counts do not
  change. Never compare geometry across revisions as bytes, as a digest of
  `indices/vertices/normals`, or order-sensitively — compare an
  **order-insensitive set of triangles** (`tests/write.rs::triangles`,
  `tests/test_ferrojt.py::triangle_set`). `normal_binding` is `Some(1)` for an
  8.x shape and `None` for a 9.x/10.x one.
- **`jt/encode.rs` builds the fan order around a coder vertex by crossing
  shared edges** (`across: HashMap<(i32,i32), Vec<i32>>`), never by sorting the
  triangles by angle in a tangent plane: a screw thread projects onto its own
  fan, an angular sort names non-adjacent triangles, and the dual walk dies with
  "the dual walk placed a triangle twice". Encodability is
  `bad_edges == 0 && doubled == 0 && unused == 0 && low_degree == 0` on welded
  positions — `euler == 2` is **not** the criterion (plate1's screws are genus 1,
  `euler = -2`, and encode fine). `SampleJT_Server.jt` has thousands of bad
  edges, so it can only be written as 9.x, never 10.x.
- **A written file is framed tighter than its source**: `ferrojt-cli info` warns
  35 "bytes remained" times on source plate1-9.5 and 23 on the written 10.3
  file, because the source declares trailing bytes no reader reads. Documented,
  deliberately not reproduced — the pictures are identical (1.00000).
- **A filtered write leaves a `Shape` node with zero late loads** (`walk()` skips
  the coarse tiers), and `dump.rs` must not then emit a dangling comma: the
  `ShapeTriStripSet` branch collects its geometry up front and passes a `comma`
  flag to `handle_attributes`, whose terminators come from
  `terminator(comma, following)`. None of the five reference dumps hits the case
  (their `TriStripSet` count equals their `NumIndices` count), so this fix cannot
  break byte parity — and `JtFile.toc()` returns invalid JSON without it.
- **`JtMesh.vertex_count` (unique) ≠ `LodGeometry.vertices.len()` (floats).**
  Pinning the wrong one makes a Python test assert 72/108 where the API reports
  24/36.
- **A bare `write` of block-9.5** (no options) keeps the revision but only the
  finest tier: 6 TOC rows `[1,3,4,4,6,17]`, 10 nodes, 1 representation,
  12 triangles. Pinned in both `tests/write.rs` and `tests/test_ferrojt.py` —
  a "keeps the revision" test must pass `all_lods=True, all_layers=True` to see
  all 8 rows.
- **The builder (`jt/build.rs`) defaults to JT 10.3** and emits the topological
  (tri-strip-set) representation there; `set_version`/`version` (Rust) and the
  `version` property plus `set_version()` (Python) pick 8.x/9.x instead. Banner
  `Version 10.3 JT`, header 109 bytes (LSG at 109) for 10.x against 105 for
  8.x/9.x. A 9.x/10.x mesh must be a **closed two-manifold** — an open shell is
  refused with "the mesh is not a closed two-manifold, so it cannot be written
  as JT 9.x/10.x", and only JT 8.x can hold it. `Format::Jt` **is** writable,
  but `export::to_format` special-cases it (`Format::Jt =>
  crate::jt::model_to_bytes(model, None)`) because the `Exporter` trait has no
  revision parameter — it never goes through an `Exporter` impl.
- **A JT 8.x shape cannot share a vertex slot, and the strip walk renumbers the
  indices.** `MeshData::new`/`with_normals` **explode** the mesh (one slot per
  triangle corner — a cube becomes 36 vertices / 36 normals / 36 indices, and
  `vertices()` comes back three times longer). `representation()` then builds
  `emit` (the winding-alternating walk `expand_strips` in `object.rs` performs:
  `0,1,2` then `3,5,4` then `6,7,8`) and its inverse `written`, and writes the
  geometry per **written** slot. The slot number *is* the index the reader
  emits, so assuming file slot == source slot panics with
  `index out of bounds: the len is 24 but the index is 24`. `normal_binding` is
  `u8::from(with_normals)`, not a hardcoded `1`. **A Rust fixture that is
  always pre-exploded hides this** — `a_compact_mesh_is_exploded_and_comes_back_unchanged`
  is the test that feeds the compact form.
- **Builder handles are opaque tokens.** `JtNode(usize)`/`JtMesh(usize)` have
  private fields, so outside the crate only `.index()` exists; `check_node` /
  `check_mesh` reject a handle of another builder. Property ids must **continue**
  the node+attribute numbering — the property table ends a row with key `0`, so a
  property numbered 0 truncates its own row. `write_to_bytes` refuses a shape
  with no mesh, an instance with no target, a cycle and unreachable nodes.
- **Python names for the builder**: `JtBuilder`/`NodeHandle`/`MeshHandle` —
  `JtNode` and `JtMesh` are already the LSG view. `class` is a keyword, so the
  accessor is `class_of` (`#[pyo3(name = "class_of")]`) and `add_node`'s
  parameter is `class_name`; `add_shape(parent, mesh, *, name=None)`.
  `MeshData.bounds` returns a **list** of six, not a tuple.
  `add_node`/`add_detached_node` need a `#[pyo3(signature = (…, name = None))]`
  or the optional name is a required positional argument; the stub in
  `_ferrojt.pyi` says `name: str | None = ...` and would lie without it.
  `class_name` accepts `"RangeLOD"` (also `rangelod`/`rangelode`).
  **A new `BuildNode` variant must be covered in `src/build.rs::class_name` or
  the cdylib fails to build** (`error[E0004]: pattern … not covered`) — `maturin
  develop` breaks before any test runs.
- **Python writing entry points**: `JtFile.write_jt(path, *, layer_filter=None,
  all_layers=False, all_lods=False, compress=False, version=None)` and
  `JtBuilder.version` / `set_version()`. `version` is a **string** (`"10.3"`,
  `"9"` → 9.5) because pyo3 0.30 cannot express a tri-state `Option<Option<&str>>`
  keyword. `JtFile.model()` and `ferrojt.open/read` still have **no `all_lods`** —
  every LOD tier is only reachable through `jt.lsg.meshes()` in Python.
- **glTF is a reader now** (`import/gltf.rs`, `import::from_file`/`from_bytes`).
  It fills the same `Model` and **declares** `unit = "meter"`, `up_axis = "+Y"`.
  `Format::is_readable()` is `Jt | Gltf | Glb` and `crate::open_format` is the
  single dispatch point — `ferrojt::open`, `ferrojt-cli` and `ferrojt.open` all go
  through it, so a new reader needs no CLI or Python change beyond the format name.
  `Model.unit` / `Model.up_axis` are Python properties.
- **`jt/from_model.rs` is the neutral-`Model` JT writer** — a **private** module,
  re-exported as `jt::model_to_bytes(model, Option<JtVersion>)` and
  `jt::model_to_file`. It builds its own graph through `JtBuilder`; it does not
  copy segments, so unlike `jt::write_to_bytes` it needs no source file.
- **The two-manifold fallback is per file, not per mesh.** `model_to_bytes` probes
  every mesh with `jt::encode::encode`; if one fails and `version` is `None` it
  rebuilds the whole file at **JT 8.1**, and if `version` was asked for it refuses
  with "a mesh with N triangles is not a closed two-manifold, so it cannot be
  written as JT {v}; write it at JT 8.x or repair the mesh". Measured: block
  8.1/9.5/10.3 and plate1-9.5 → 10.3; **plate1-8.0 and `SampleJT_Server.jt` → 8.1**.
  The CLI prints the revision it landed on (`[8.1]`), which is how the fallback is
  observable without a test.
- **The frame is a declared convention, and the conversion lives on the root.**
  `jt/model.rs::build_model` sets `millimeter` / `+Z` (JT stores no unit and
  `Partition::read` hardcodes `up_axis: 0`, but every sample carries
  `JT_PROP_MEASUREMENT_UNITS:: = "millimeters"`). So a JT→JT write is an identity
  and a glTF→JT write puts `scale = 1000` plus a quarter turn about +X on the root
  `Partition` (`root_transform`, quaternion `[0.70710678, 0, 0, 0.70710678]` in
  `x y z w`) and leaves the geometry alone — bounds of a 100×80×60 mm block are
  unchanged. Unknown unit/up axis → `Error::Unsupported`.
- **A glTF hop erases every JT node class.** A file written from a `Model` is
  exactly `Partition 1 · MetaData 1 · Part 1 · RangeLOD 1 ·
  Shape = meshes · Group = nodes − meshes + 1 · Instance = extra + 1`;
  `Node::kind` is the free-form class carrier that round-trips through
  `class_of`, and anything unknown becomes `Group`. Written LSG node count ==
  `model.nodes.len() + 5 + instances` for a glTF-sourced model, `+ 1` for a
  JT-sourced one — `already_wrapped()` sees a `Part`/`MetaData`/`LOD`/`RangeLOD`
  class in the model and skips the wrapper, `partition_root()` sees a `Partition`
  and **merges** it (see the next bullet).
- **A model that carries its own `Partition` is merged with the builder root, not
  stacked under it.** `partition_root()` finds the scene root whose class is
  `Partition` and `Writer::adopt` moves its children and transform onto the
  builder's own root, so the file owns exactly one partition. Stacking a second
  one put the `Part` one level deeper than the fixed handful of classes a
  consumer descends to reach geometry: the file **parses**, `jtinfo` reports
  `# JtkAssemblies: 1`, and the external renderer then produces **no frame at
  all** — `render-jt.mjs` sits idle for 300 s and dies with
  `WebSocket closed unexpectedly (code 1000, clean=false)`, exit 1, no PNG. That
  silent hang, not an error, was the "viewers show an empty render" report.
- **An `Instance` carries its own placement, and its target is a *detached* node.**
  In `jt/from_model.rs` the `GeometricTransform` **and** the name of an instance
  node go on the `Instance` (`set_node_transform` / `set_name`), never on the node
  it refers to. Dropping them collapses every instance of a part onto one place —
  the two screws of the plate (t = [0,−25,15] and [0,+25,15]) drew on top of each
  other. And the target must be created with `JtBuilder::add_detached_node`:
  `Writer::place(index, Option<JtNode>)` / `Writer::detached()` write it when no
  child list refers to it, exactly as every sample stores a part. Hanging it under
  the root as well draws the shared subtree a **third** time at the identity.
  Plate model path: 280 046 → 267 466 (transform) → **291 167** (detached) = the
  exact source baseline. Pinned by
  `an_instance_of_a_sample_keeps_its_placement_and_its_detached_target`
  (`tests/gltf.rs`, plate1-9.5 straight through `to_model()` → `model_to_bytes`
  with **no glTF hop**, so the JT `Instance` class survives; a glTF hop turns it
  into a `Group` and the test would prove nothing).
- **A converted file must carry the chain a consumer walks.** Shapes straight
  under the root partition leave a strict importer with nothing, which is the
  "the external program cannot load the converted JT" report. `Writer::wrap` in
  `jt/from_model.rs` puts `Partition → MetaData → Instance → Part → RangeLOD →
  Group` under the root and hangs the graph of the model on that group, mirroring
  every sample. Two details bite: the `Part` is created with
  `JtBuilder::add_detached_node` and **referred to** by the `Instance` (an
  instance holds no children, so `add_node(instance, …)` is refused and a detached
  node nothing refers to is reported as unreachable), and the `RangeLOD` hangs
  under the **part**. The instance is named `<stem>.part` (`part_name`, after the
  `_model1_117.part` of the samples); the part node stays unnamed, as in the
  samples. The span holds exactly **one** tier — LOD 0, the only one a neutral
  model has. Pinned by `a_converted_file_wraps_its_graph_in_the_classes_a_consumer_walks`
  (Rust, `tests/gltf.rs`) and the same-named Python test, and by
  `a_level_of_detail_span_can_be_built_by_hand` (`tests/build.rs`) /
  `test_a_level_of_detail_span_can_be_built_by_hand` (`tests/test_build.py`) for
  the hand-built side.
- **`jt::dump` closes every member with a `comma` flag.** A container node that
  lost all of its children — a filtered `RangeLOD`, a `Part` whose representation
  was dropped — otherwise ends `"name":"",` right before `}` and the JSON will
  not parse. `write_name(.., comma)` and the terminator of each following member
  take it; `recurse_down_the_tree` computes `has_children`/`has_attributes` once.
  Writing `b"\n"` instead of `b"\"\n"` for the no-comma case drops the closing
  quote and reads as `Invalid control character`. None of the six reference dumps
  has a dangling comma (`target\dangling.py` counts them), so byte parity is
  untouched.
- **A bare built file is structurally thinner than a CAD file.** Samples:
  `Partition → MetaData → Instance → Part → RangeLod → Group×3 → Shape×3`
  (11 nodes, 14 atoms, 67 property atoms, 9 TOC rows incl. PMI(3), Material(8/9),
  B-rep(17), InfoSegment(31)). `JtBuilder` on its own still writes
  `Partition(+GeometricTransform) → Shape(+Material)` (2 nodes, 4 atoms, 6
  properties, 2 TOC rows) — the wrapper chain is `from_model`'s job, not the
  builder's, and a hand-built graph has to add it itself. What stays thin in
  both: there is **no type-4 Meta Data segment**, and the segment GUIDs are
  synthetic counters (`00000000-0000-0000-fe00-00000000000N`), not UUIDs. Also:
  the `Partition` node-count range is the written graph, not the instance-expanded
  one (2 vs the samples' 7/18/4489), and the trailing `size` of a `Shape` stays
  `0` (samples 2084/1712/1112) because no reader uses it.
- **A strict consumer also refuses a bare builder graph** — `jtinfo.exe` says
  `Unable in find root node` for `target\build_{8.0,8.1,9.5,10.3}.jt`, which are
  `Partition → Shape`. That is by design (the chain is `from_model`'s job), and
  `target\build_chain.py` proves the hand-built `Partition → MetaData → Instance
  → Part → RangeLOD → Group → Shape` passes at all four revisions. Two gotchas
  there: the `Instance` must come from `instance_of(meta, part)` where `part` is
  an `add_detached_node("Part")` — `add_node(root, "Instance")` followed by
  `instance_of(inst, part)` is wrong — and `maturin develop` first, or a stale
  wheel hides the Rust fix.
- **The external oracle is `C:\Tools\JtUtilities\JtUtilities\bin64\jtinfo.exe`**
  (12.7.1.0). A run is a real pass only when `# JtkAssemblies:` is ≥ 1: `0`
  means the file parsed but its root was lost, exit `5` with
  `ERROR: JtkCADImporter::import()` is a rejection, exit `-1073741819`/`4` is a
  crash. Capture the raw output plus `$LASTEXITCODE` (`$o = & jtinfo.exe $f 2>&1
  | Out-String; $code = $LASTEXITCODE`) — piping through `Select-String` loses
  the exit code. `target\jtsweep.py` runs every rewrite at every legal revision,
  every glTF→JT at every revision and the four hand-built chains through it and
  prints `N of M accepted`; it is **46 of 46**, and the only non-passing lines
  are `SKIP` where the writer deliberately refuses an impossible request (a
  family change, or a non-two-manifold mesh at 9.x/10.x).
- **`ShapeStats::of` counts distinct (position, normal) pairs**, which is what the
  partition count range reports — 26 for the exploded flat-normal cube — while the
  written 8.x representation declares 36 slots. For a glTF cube both are 24, so no
  sample shows the difference; do not treat the two numbers as interchangeable.
- **`from_model` emits one material attribute per shape *node***, so the distinct
  count is smaller than `Model::materials.len()` — the server sample writes 20
  attributes for 23 materials.
- **A GLB chunk length must be the *padded* length**, and the chunk header is
  length **then** type. Pinned by `export/glb.rs::a_chunk_length_counts_its_own_padding`.
- **`JtFile::lsg` is `Option<SceneGraph>` and `SceneGraph` has no `node_count()`** —
  in Rust use `.as_ref().expect(..).nodes.len()`. The Python `JtLsg.node_count`
  **is** a property (a different type), so the two do not mirror each other here.
- **The pytest summary line never surfaces under redirection** (Open3D/Vulkan
  native output interleaves), so `Select-String 'passed|failed'` finds nothing.
  Trust `$LASTEXITCODE` plus `pytest --co -q` per file: 23 build + 83 ferrojt +
  4 jt2go + 17 visual = **127**.
- **Parallel tests need pid *and* an atomic counter in the temp file name** — the
  pid alone collides across the worker processes of one run.
- **Picture parity for a written file must be measured one process per picture**
  and compared as PNGs: for the ~1M-triangle server mesh Open3D returns
  permanent all-zero frames partway through a process, so retrying inside one
  process does not help. Probe: `target\rt_picture.py` (`render <glb> <png>`,
  then `compare <stem>`). All six samples: source vs written **1.00000**.
- **`multi_replace_string_in_file` needs `filePath` as the FIRST key** of each
  replacement object — putting it last yields "must have required property
  'filePath'".

## Language / platform gotchas hit before

- Rust byte literals treat `\020` as octal — write `\x00`.
- `Path::with_extension()` truncates at the first dot of a multi-dot stem.
- `Select-String -Path dir\*.rs` does not recurse; use `Get-ChildItem -Recurse`.
- `egui::Label` has no `on_hover_text` — put it on the `Response` from `ui.add(..)`.
- `std::time::Instant` has no `as_secs_f32`; use `now.duration_since(start).as_secs_f32()`.

## Toolchain baseline

rustc/cargo 1.97.1, host `x86_64-pc-windows-msvc`, VS 2022 MSVC, Python 3.13.7,
maturin 1.15.0, ziglang 0.16.0 (pip), open3d 0.20.0 (pip, newest for 3.13),
git 2.50.1. Pinned deps in the root
`Cargo.toml`: thiserror 2.0, serde 1.0, serde_json 1.0, flate2 1.1, half 2.7,
bytemuck 1.25, tracing 0.1, rayon 1.12, gltf-json 1.4, gltf 1.4 (now a **runtime**
dep of `ferrojt` for the glTF reader, no longer dev-only), base64 0.22,
lzma-rs 0.3, clap 4.6, anyhow 1.0, tracing-subscriber 0.3, indicatif 0.18,
kiss3d 0.47 (`egui` feature) + egui 0.36 + rfd 0.17 + image 0.25 (no features) +
webbrowser 1.2,
pyo3 0.30 (`abi3-py310`). No USD crate — `export/usd.rs` writes `.usda` by hand.

## Open work

USD: `.usdc` (binary) and `.usdz` (packaged) output, and JT textures into the
`UsdPreviewSurface` — `docs/usd-writer.md` holds the mapping and the open work.
JT: keeping the source TOC position instead of always appending the table —
`docs/jt-writer.md` holds the details. The builder (`jt/build.rs`) creates a file
from nothing at JT 10.3 by default (8.x/9.x on request) — `docs/jt-builder.md`
lists what is still open there (XZ, key/value properties, layer filters, and the
`MetaData`/`Part`/`RangeLod` structure plus real segment UUIDs). Writing a neutral
`Model` (`jt/from_model.rs`, `docs/jt-from-model.md`) carries no LOD tiers, no
key/value properties and no layer filters yet.
macOS and ARM wheels need CI (`maturin generate-ci github`).
