# Design notes

This directory holds the notes that explain *why* the code looks the way it
does. They are not a specification of the JT format itself.

| Note | Contents |
| --- | --- |
| [architecture.md](architecture.md) | How the crates, the neutral model and the readers/writers fit together. |
| [jt-reader.md](jt-reader.md) | The contract the OpenJT reader has to satisfy. **Placeholder — to be filled in with the implementation to follow.** |
| [usd-writer.md](usd-writer.md) | The model → USD mapping and the two candidate implementations. |

## Adding a reader

1. Create `crates/ferrojt/src/jt/<segment>.rs` and parse into the neutral model.
2. Fill `jt::open` so it returns a populated [`Model`](../crates/ferrojt/src/model/scene.rs).
3. Set `Format::is_readable` accordingly and add a round trip test.

Nothing in `export/`, `ferrojt-cli` or `ferrojt-python` needs to change.

## Adding a writer

1. Add `crates/ferrojt/src/export/<format>.rs` implementing
   [`Exporter`](../crates/ferrojt/src/export/mod.rs).
2. Register it in `export::to_format` and mark the format in `Format::is_writable`.
3. Add the format to `TargetFormat` in `crates/ferrojt-cli/src/cli.rs`.
