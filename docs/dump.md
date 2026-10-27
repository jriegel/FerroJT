# JT JSON dump contract

`jt::dump` in `crates/ferrojt/src/jt/dump.rs` reproduces the JSON that the JT
tooling writes for a file. It is a *JT native* view: it walks the TOC table and
the logical scene graph, not the neutral [`Model`](../crates/ferrojt/src/model/scene.rs),
because the reference output contains things the model deliberately drops
(anonymous nodes, the raw key/value property streams, the TOC itself).

The reference dumps live in `data/Reference/`, one per sample in
`data/ExampleFiles/`. Matching them is the acceptance test.

## Entry points

| Item | Behaviour |
| --- | --- |
| `jt::dump(file, config, indent) -> Result<String>` | The whole document: `{`, the header block, then the tree or the orphan meshes, then `}`. |
| `jt::write_model(file, out, indent, config)` | The header block: `FileName`, `MajorVersion`, `MinorVersion` and, when `DUMP_TOC` is set, `TocTable`. |
| `jt::write_meshes_without_lsg(file, out)` | The fallback for a file with no logical scene graph. |
| `jt::write_key_value_stream(stream, out, indent)` | A raw proxy/property stream rendered as `"Key": "Value"` pairs. |
| `jt::format_g(value) -> String` | The C `%g` conversion, used for every number in the output. |

## The config bits

`config` is a bit set, exactly as in the reference tool:

| Bit | Value | Effect |
| --- | --- | --- |
| `DUMP_TOC` | `1` | Emit the `TocTable` array. |
| `DUMP_LOD0` | `2` | Reserved. The writer never reads it, and neither did the reference. |
| `DUMP_MESH` | `4` | Fill `"Indices"`; without it the array is written empty. |
| `DUMP_ALL` | `i32::MAX` | Everything on. |

`ferrojt dump --no-toc` clears `DUMP_TOC`, `--no-indices` clears `DUMP_MESH`.
`ferrojt.JtFile.toc()` dumps with `DUMP_TOC` alone, which is why it still
contains the tree but has empty `Indices` arrays.

## Number formatting

Every number goes through `format_g`, which reproduces `printf("%g")`: six
significant digits, fixed notation for exponents in `[-4, 6)`, scientific
otherwise, and `-0` preserved. `serde_json`'s shortest round trip representation
is *not* equivalent — `1234567.0` is `1.23457e+06` under `%g` but `1234567.0`
under `serde_json` — so the dump is built by hand rather than by serialising a
`serde_json::Value`.

## Known deviations from well formed JSON

`recurse_down_the_tree` writes nothing at all for a `Shape`, `ShapeVertex` or
unsupported node, mirroring the reference tool, which only reports those types on
stderr. When such a node is not the first child, the separator that
`handle_all_children_json` emitted before it is left dangling, so
`data/ExampleFiles/SampleJT_Server.jt` dumps with a trailing comma before a `]`.
The Rust writer reproduces that behaviour rather than fixing it, so output stays
comparable; that file's dump is therefore not parseable JSON. Every sample that
has a reference dump does parse, and `dump_is_valid_json_for_every_sample` checks
that for all three config combinations.

Reference files also carry a UTF-8 BOM; strip it (Rust `\u{feff}`, Python
`encoding="utf-8-sig"`) before parsing.

## Layer information

A `MetaData` node reports its `LAYERFILTERnnn::` streams as a `"layer-filter"`
array of `{ "name", "layer": [...] }` objects plus `"active-layer-filter"`, and a
`Part` with a `LAYER::` tag reports it as `"layer": [...]`. `scan_for_layer` and
`scan_for_layer_filter` parse those streams, and `jt::model` reads the very same
ones to decide what an export keeps — but a dump is never filtered: it reports
every filter, every tag and every part, whatever the export does with them. See
[jt-reader.md](jt-reader.md).

## Comparison rules

`crates/ferrojt/tests/dump.rs` and `tests/test_ferrojt.py` both compare
*semantically*: parse both sides and compare values, after recursively removing
`FileName`, because the reference dumps were produced on a different machine.
Whole numbers are compared by value too, since the reference files were
re-written by a JSON library that respelled `1234570` as `1234570.0`. Do not
compare raw text.
