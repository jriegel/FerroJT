# FerroJT — agent notes

Rust cargo workspace plus a Python wrapper that reads OpenJT (`.jt`) and exports
glTF 2.0 / GLB and ASCII USD (`.usda`). Read this before changing code; the
design detail lives in `docs/`.

## Layout

| Path | What it is |
| --- | --- |
| `crates/ferrojt` | The library: `model/` neutral scene graph, `codec/` binary helpers, `jt/` OpenJT reader (segment/TOC, element objects, decode codecs, `dump.rs` JSON writer), `export/` glTF/GLB writer and the ASCII USD writer. |
| `crates/ferrojt-cli` | The `ferrojt` binary (clap): `info`, `inspect`, `dump`, `convert`, `ls`. |
| `crates/ferrojt-python` | `pyo3` cdylib `_ferrojt`; `pyproject.toml` + `python/ferrojt/` + `tests/`. |
| `crates/ferrojt-viewer` | `ferrojt-viewer` binary: egui outliner + kiss3d viewport. `meshes.rs` JT→GPU meshes, `scene.rs` row tree/transforms/LOD stacks, `lod.rs` screen-area tier selection, `tree.rs` outliner, `details.rs` key/value pane for the selected row, `backdrop.rs` viewport gradient, `camera.rs` `Camera3d` wrapper that places the backdrop, `picking.rs`, `trihedron.rs`. |
| `python/ferrojt/` | Pure-Python package re-exporting `_ferrojt` (`__init__.py`, `_ferrojt.pyi`, `py.typed`). |
| `data/ExampleFiles/` | Sample JT files: JT 8.0, 8.1, 9.5, 10.3. |
| `data/Reference/` | Reference JSON dumps from the JT tooling **and** reference PNG pictures — **the oracle**. |
| `example/` | GPL-2.0 C++ sources (TKJT, ToolsLib, JtDump, PyOpenJt). Behavioural reference only, never linked or copied; gitignored. |
| `docs/` | `README.md` index, `architecture.md`, `jt-reader.md`, `dump.md`, `usd-writer.md` and `viewer.png` — the viewer picture shown at the top of the top-level `README.md` (Git LFS). |

## Commands

```sh
cargo build --workspace
cargo test  --workspace --exclude ferrojt-python   # must exclude, see below
cargo clippy --workspace --all-targets
cargo fmt --all
cargo doc --no-deps -p ferrojt
```

`ferrojt-python` must be excluded from `cargo test`: its default features enable
`pyo3/extension-module`, so a test binary cannot resolve the Python symbols.

Python (`.venv` at the repo root, gitignored):

```powershell
$env:VIRTUAL_ENV = "$PWD\.venv"
.\.venv\Scripts\python.exe -m maturin develop
.\.venv\Scripts\python.exe -m pytest
```

Visual regression (needs `open3d`, `pip install -e ".[test,visual]"`; the tests skip
without it):

```powershell
.\.venv\Scripts\python.exe tests\test_visual.py --write   # recreate data/Reference/*.png
.\.venv\Scripts\python.exe tests\test_visual.py           # compare, diff -> tests/_visual_out/
```

Interactive viewer (also needs `open3d`; it is a tool, not a test — nothing in it
is named `test_*`, so pytest still collects 53 tests):

```powershell
.\.venv\Scripts\python.exe tests\test_viewer.py data\ExampleFiles\example_block_jt9.5.jt
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
  public items.
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
- **LOD tier is positional, not encoded.** JT never stores a tier: it is the child
  position under `RangeLod` (first child = 0, next = 1, …), **whatever the child's
  class**. The plate samples wrap each tier in a `Group`; the NIST parts list the
  finer tiers as bare `Shape`s (`Shape`, `Shape`, `Group { Shape }`), and counting
  groups only (as `example/ToolsLib/src/JtGltfMesh.cpp` does) exported all three
  tiers stacked. Segment type cannot identify it — JT 8.x/10.x use types 7..10 for
  LOD0..LOD3, but JT 9.x uses type 6 ("Shape") for every tier. `to_model()` exports
  only the finest tier; the state is `LodWalk { inside, level }` threaded through
  `build_node`, set per child of a `RangeLod` and inherited below it. Nodes are
  never dropped, only geometry, and each mesh records `Mesh::lod_level`.
  `jt::dump` is *not* filtered. NIST counts are pinned in
  `every_child_of_a_range_lod_is_a_tier` (they equal Aspose.3D 26.10's).
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
  `1016789 of 124154 triangles` (counts from before the wide-mask fix). `meshes::triangle_count` was deleted for this.
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
  `ACTIVELAYERFILTER::`, else the **first key of the sorted `BTreeMap`** (all
  samples leave it empty; the JT Layer tool combo box lands on index 0).
  Unknown name → `Error::invalid(0, …)` listing the available names. A part with
  no tag is **kept** (deviation from `JtGltfMesh.cpp`, keeps plate1-9.5 at
  2/486); `Partition`/`MetaData`/`LOD`/`RangeLOD`/`Group`/`Instance` are never
  gated, and a rejected part keeps its node but loses geometry, children and
  instance target. API: `to_model()` / `to_model_unfiltered()` /
  `to_model_with_filter(&str)`; the effective filter is
  `Model::extras["layer_filter"] = {"name", "layers"}`. `LayerFilter` is
  re-exported from `ferrojt::jt`. `jt::dump` stays unfiltered. No sample loses a
  part to its own filter, so the pinned counts above are unchanged — a drop can
  only be shown with a forced filter or the synthetic fixture in `jt/model.rs`.
- **The wide attribute masks are a plain `VecU32`, not a codec package.** After
  `mask8_30` (and the JT 9 `mask8_4`), the masks of faces with more than 64
  attributes are an `I32` count and that many raw `U32`s. Read as a mark 2
  package, an empty array still parses (count 0), so only files with such faces
  break: the next packages are misaligned and the mesh comes out empty or
  fails. `SampleJT_Server.jt` has one such shape (36 instances), which used to
  read as 36 empty representations; it now exports 106 meshes / 129067
  triangles (850 drawn, 1193657 triangles drawn, the same as Aspose.3D).
- **An empty representation keeps its node, not a mesh.** `push_mesh` skips an empty channel, so
  such a shape yields a `Primitive` with **no attributes at all**, and
  `export/gltf.rs::to_primitive` refuses the whole document ("a primitive without
  a POSITION attribute cannot be exported to glTF") because glTF mandates
  POSITION. The reader drops the representation in `build_node` (`geometry.vertices
  .is_empty()` → `continue`): the node stays, no mesh is made — same policy as the
  layer filters and the coarse LOD tiers. `JtGltfMesh.cpp` instead appends
  zero-length accessors; deliberate deviation. No sample has an empty
  representation any more (the server sample's came from the wide-mask bug
  above), so `the_server_sample_exports_every_shape` pins nodes 2158, meshes 106,
  materials 23, triangles 129067 (`tests/model.rs`, `tests/test_ferrojt.py`).
  Keep the POSITION check in the writer: it is the guard for hand-built models.
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
- `pyo3` is built with `abi3-py310`, so one wheel per platform covers CPython
  ≥ 3.10. Tags: `cp310-abi3-win_amd64`,
  `cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64`.
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
- **JT 8.x samples and `SampleJT_Server.jt` declare NO layer filters**, so their layers
  record no `ferrojt:layer_filter` in `customLayerData` (that is how a consumer tells
  "unfiltered" from "filtered by a file with no layers"). block-9.5/10.3 and plate1-9.5
  declare `ALL` (+ `LAYCUR`); plate1-9.5 tags its parts `LAYER:: = 20`. Recorded via
  `Model::extras["layer_filter"]` → `ferrojt:layer_filter` / `int[] ferrojt:layers`.
- **usd-core 26.11 Python API** (for `target\usd_*.py` probes): no
  `Stage.GetCustomLayerData` — read `stage.GetRootLayer().customLayerData`; `Prim` has
  no `GetCompositionErrors` (only `Stage`); `Relationship::GetTargets()`, not
  `GetResolvedPath`; `prim.GetMetadata("references")` returns an `Sdf.ReferenceListOp`
  which is **not iterable** — use `GetAppliedItems()`, because a `prepend` leaves
  `addedItems`/`explicitItems` empty; a reference's path is `.primPath`; `Sdf.Path` has
  no `startswith`; `UsdValidation.ValidationContext` takes validators, not a `Stage`.
- **`tests/test_visual.py` is flaky by 1–2 random samples per run** (similarity ≈0.13,
  "100% of the pixels differ") from the documented Open3D blank-frame issue —
  `shoot` retries `MAX_FRAMES` but still loses occasionally, and a re-run picks a
  different sample. Re-run before investigating; `pytest tests\test_ferrojt.py` alone
  is the stable check.

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
bytemuck 1.25, tracing 0.1, rayon 1.12, gltf-json 1.4, gltf 1.4 (dev), base64 0.22,
lzma-rs 0.3, clap 4.6, anyhow 1.0, tracing-subscriber 0.3, indicatif 0.18,
kiss3d 0.47 (`egui` feature) + egui 0.36 + rfd 0.17 + image 0.25 (no features) +
webbrowser 1.2,
pyo3 0.30 (`abi3-py310`). No USD crate — `export/usd.rs` writes `.usda` by hand.

## Open work

USD: `.usdc` (binary) and `.usdz` (packaged) output, and JT textures into the
`UsdPreviewSurface` — `docs/usd-writer.md` holds the mapping and the open work.
macOS and ARM wheels need CI (`maturin generate-ci github`).
