# Design notes

This directory holds the notes that explain *why* the code looks the way it
does. They are not a specification of the JT format itself.

| Note | Contents |
| --- | --- |
| [architecture.md](architecture.md) | How the crates, the neutral model and the readers/writers fit together. |
| [jt-reader.md](jt-reader.md) | The contract the OpenJT reader satisfies: entry points, submodules, output rules, layer filters and tests. |
| [jt-writer.md](jt-writer.md) | Writing a JT file back out: what is re-serialised and what is copied, the renumbering rules, choosing the JT revision, the framing facts and the round-trip contract. |
| [jt-builder.md](jt-builder.md) | Creating a JT file from nothing: the default JT 10.3 layout and the 8.x one, why a mesh is exploded, the strip permutation and what a builder rejects. |
| [gltf-reader.md](gltf-reader.md) | The glTF 2.0 / GLB reader: entry points, what is kept as authored, the declared unit and up axis, the GLB chunk-length rule and what does not survive. |
| [jt-from-model.md](jt-from-model.md) | Writing a neutral `Model` as JT: the three ways to produce a file, choosing the revision and the two-manifold fallback, the unit conversion on the root transform, the node class and instancing rules. |
| [dump.md](dump.md) | The JT native JSON dump: config bits, `%g` formatting and how the reference dumps are compared. |
| [usd-writer.md](usd-writer.md) | The model → USD mapping, how meshes are written once and referenced, and how materials are deduplicated. |

[../AGENTS.md](../AGENTS.md) holds the short version of the same rules — build
commands, the parity constraints and the known pitfalls — for automated
contributors.

## Reading another format

1. Create `crates/ferrojt/src/<format>/` and parse into the neutral
   [`Model`](../crates/ferrojt/src/model/scene.rs), declaring its `unit` and
   `up_axis` while you are there — a writer converts on the strength of that
   declaration instead of guessing.
2. Add a `Format` variant, teach `Format::detect`/`FromStr` its extensions, and
   mark it in `Format::is_readable`.
3. Register it in `crate::open_format`.
4. Add a sample under `data/ExampleFiles` and a test that pins its node and
   triangle counts.

`ferrojt-cli` and `ferrojt-python` go through `open_format`, so they need no
change beyond the format name.

## Adding a writer

1. Add `crates/ferrojt/src/export/<format>.rs` implementing
   [`Exporter`](../crates/ferrojt/src/export/mod.rs).
2. Register it in `export::to_format` and mark the format in `Format::is_writable`.
3. Add the format to `TargetFormat` in `crates/ferrojt-cli/src/cli.rs`.
