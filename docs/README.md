# Design notes

This directory holds the notes that explain *why* the code looks the way it
does. They are not a specification of the JT format itself.

| Note | Contents |
| --- | --- |
| [architecture.md](architecture.md) | How the crates, the neutral model and the readers/writers fit together. |
| [jt-reader.md](jt-reader.md) | The contract the OpenJT reader satisfies: entry points, submodules, output rules and tests. |
| [dump.md](dump.md) | The JT native JSON dump: config bits, `%g` formatting and how the reference dumps are compared. |
| [usd-writer.md](usd-writer.md) | The model → USD mapping and the two candidate implementations. The USD writer itself is still a stub. |

## Reading another format

1. Create `crates/ferrojt/src/<format>/` and parse into the neutral
   [`Model`](../crates/ferrojt/src/model/scene.rs).
2. Add a `Format` variant, teach `Format::detect`/`FromStr` its extensions, and
   mark it in `Format::is_readable`.
3. Add a sample under `data/ExampleFiles` and a test that pins its node and
   triangle counts.

Nothing in `export/`, `ferrojt-cli` or `ferrojt-python` needs to change.

## Adding a writer

1. Add `crates/ferrojt/src/export/<format>.rs` implementing
   [`Exporter`](../crates/ferrojt/src/export/mod.rs).
2. Register it in `export::to_format` and mark the format in `Format::is_writable`.
3. Add the format to `TargetFormat` in `crates/ferrojt-cli/src/cli.rs`.
