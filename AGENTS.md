# FerroJT — agent notes

Rust cargo workspace plus a Python wrapper that reads OpenJT (`.jt`) and exports
glTF 2.0 / GLB and (planned) USD. Read this before changing code; the design
detail lives in `docs/`.

## Layout

| Path | What it is |
| --- | --- |
| `crates/ferrojt` | The library: `model/` neutral scene graph, `codec/` binary helpers, `jt/` OpenJT reader (segment/TOC, element objects, decode codecs, `dump.rs` JSON writer), `export/` glTF/GLB writer and the USD stub. |
| `crates/ferrojt-cli` | The `ferrojt` binary (clap): `info`, `inspect`, `dump`, `convert`, `ls`. |
| `crates/ferrojt-python` | `pyo3` cdylib `_ferrojt`; `pyproject.toml` + `python/ferrojt/` + `tests/`. |
| `python/ferrojt/` | Pure-Python package re-exporting `_ferrojt` (`__init__.py`, `_ferrojt.pyi`, `py.typed`). |
| `data/ExampleFiles/` | Sample JT files: JT 8.0, 8.1, 9.5, 10.3. |
| `data/Reference/` | Reference JSON dumps from the JT tooling **and** reference PNG pictures — **the oracle**. |
| `example/` | GPL-2.0 C++ sources (TKJT, ToolsLib, JtDump, PyOpenJt). Behavioural reference only, never linked or copied; gitignored. |
| `docs/` | `README.md` index, `architecture.md`, `jt-reader.md`, `dump.md`, `usd-writer.md`. |

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
is named `test_*`, so pytest still collects 41 tests):

```powershell
.\.venv\Scripts\python.exe tests\test_viewer.py data\ExampleFiles\example_block_jt9.5.jt
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
  `target/`, `dist/`, `.venv/`, `data/`, `example/` are not.

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
  position under `RangeLod` (first group = 0, next = 1, …). Segment type cannot
  identify it — JT 8.x/10.x use types 7..10 for LOD0..LOD3, but JT 9.x uses type 6
  ("Shape") for every tier. `to_model()` exports only the finest tier, mirroring
  `example/ToolsLib/src/JtGltfMesh.cpp`; the state is `LodWalk { inside, level }`
  threaded through `build_node`. Nodes are never dropped, only geometry, and each
  mesh records `Mesh::lod_level`. `jt::dump` is *not* filtered.
- Pinned LOD0 counts (`crates/ferrojt/tests/model.rs`): block 8.1/9.5/10.3 →
  1 mesh / 12 triangles; plate1-8.0 → 2 / 617; plate1-9.5 → 2 / 486.
  `tests/test_ferrojt.py` asserts `mesh_count == 1`, `triangle_count == 12`.
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
  material)` (not `.add`), `TriangleMesh` has no `.materials`. Comparison is
  `1 - mean|Δ|/255` with `FERROJT_VISUAL_THRESHOLD` (0.995) / `FERROJT_VISUAL_EPS`
  (8); references are `data/Reference/<stem>.png`, diffs go to the gitignored
  `tests/_visual_out/`.
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

## Language / platform gotchas hit before

- Rust byte literals treat `\020` as octal — write `\x00`.
- `Path::with_extension()` truncates at the first dot of a multi-dot stem.
- `Select-String -Path dir\*.rs` does not recurse; use `Get-ChildItem -Recurse`.

## Toolchain baseline

rustc/cargo 1.97.1, host `x86_64-pc-windows-msvc`, VS 2022 MSVC, Python 3.13.7,
maturin 1.15.0, ziglang 0.16.0 (pip), open3d 0.20.0 (pip, newest for 3.13),
git 2.50.1. Pinned deps in the root
`Cargo.toml`: thiserror 2.0, serde 1.0, serde_json 1.0, flate2 1.1, half 2.7,
bytemuck 1.25, tracing 0.1, rayon 1.12, gltf-json 1.4, gltf 1.4 (dev), base64 0.22,
lzma-rs 0.3, clap 4.6, anyhow 1.0, tracing-subscriber 0.3, indicatif 0.18,
pyo3 0.30 (`abi3-py310`). No USD crate chosen yet.

## Open work

The USD writer is still a stub returning `Error::NotImplemented`
(`crates/ferrojt/src/export/usd.rs`); `docs/usd-writer.md` holds the mapping and
the two candidate implementations, and a test asserts `NotImplementedError` for
`.usda`. macOS and ARM wheels need CI (`maturin generate-ci github`).
