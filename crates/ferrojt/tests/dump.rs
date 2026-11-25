//! The JSON dump has to reproduce what the reference tooling produced for the
//! sample files.
//!
//! The reference files under `data/Reference` were written by the classic JT
//! tool and then re-indented, so the comparison is semantic: both sides are
//! parsed into [`serde_json::Value`] and compared. `FileName` is ignored
//! because it holds the path the reference tool happened to read from.

use std::path::{Path, PathBuf};

use ferrojt::jt::{DUMP_ALL, dump};

/// Every sample that has a reference dump next to it.
fn pairs() -> Vec<(PathBuf, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("data");
    let mut pairs = Vec::new();
    for entry in std::fs::read_dir(root.join("ExampleFiles"))
        .unwrap_or_else(|err| panic!("reading {}: {err}", root.join("ExampleFiles").display()))
    {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|ext| ext != "jt") {
            continue;
        }
        let reference = root.join("Reference").join(format!(
            "{}.json",
            path.file_stem().expect("stem").to_string_lossy()
        ));
        if reference.exists() {
            pairs.push((path, reference));
        }
    }
    pairs.sort();
    assert!(
        !pairs.is_empty(),
        "no sample file has a reference dump to compare against"
    );
    pairs
}

/// Drop `FileName`, which records where the reference tool read the file from,
/// and compare numbers by value.
///
/// The reference files were re-written by a JSON library, so a whole number
/// that the C++ writer emitted as `-0` or `1234570` came back as `0` and
/// `1234570.0`. Comparing the numeric value rather than the token keeps the
/// comparison about the data instead of about the spelling.
fn normalize(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("FileName");
            for entry in map.values_mut() {
                normalize(entry);
            }
        }
        serde_json::Value::Array(values) => {
            for entry in values {
                normalize(entry);
            }
        }
        serde_json::Value::Number(number) => {
            if let Some(rewritten) = number.as_f64().and_then(serde_json::Number::from_f64) {
                *number = rewritten;
            }
        }
        _ => {}
    }
}

#[test]
fn dump_matches_the_reference() {
    for (sample, reference) in pairs() {
        let file = ferrojt::jt::JtFile::open(&sample)
            .unwrap_or_else(|err| panic!("reading {}: {err}", sample.display()));
        let text = dump(&file, DUMP_ALL, 1)
            .unwrap_or_else(|err| panic!("dumping {}: {err}", sample.display()));

        let want = std::fs::read_to_string(&reference)
            .unwrap_or_else(|err| panic!("reading {}: {err}", reference.display()));
        let want = want.strip_prefix('\u{feff}').unwrap_or(&want).trim_start();
        let mut want: serde_json::Value = serde_json::from_str(want)
            .unwrap_or_else(|err| panic!("{} is not JSON: {err}", reference.display()));
        let mut got: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("the dump of {} is not JSON: {err}", sample.display()));
        normalize(&mut want);
        normalize(&mut got);

        if want != got {
            panic!(
                "the dump of {} differs from {}\nfirst difference: {}",
                sample.display(),
                reference.display(),
                first_difference(&got, &want, "")
            );
        }
    }
}

/// Describe the first place where two JSON values disagree.
fn first_difference(got: &serde_json::Value, want: &serde_json::Value, path: &str) -> String {
    match (got, want) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for key in b.keys() {
                match a.get(key) {
                    None => return format!("{path}/{key} missing, want {}", b[key]),
                    Some(value) => {
                        let found = first_difference(value, &b[key], &format!("{path}/{key}"));
                        if !found.is_empty() {
                            return found;
                        }
                    }
                }
            }
            for key in a.keys() {
                if !b.contains_key(key) {
                    return format!("{path}/{key} unexpected, got {}", a[key]);
                }
            }
            String::new()
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            if a.len() != b.len() {
                return format!("{path} has {} entries, want {}", a.len(), b.len());
            }
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                let found = first_difference(x, y, &format!("{path}[{index}]"));
                if !found.is_empty() {
                    return found;
                }
            }
            String::new()
        }
        (got, want) => {
            if got == want {
                String::new()
            } else {
                format!("{path} is {got}, want {want}")
            }
        }
    }
}

#[test]
fn dump_is_valid_json_for_every_sample() {
    for (sample, _) in pairs() {
        let file = ferrojt::jt::JtFile::open(&sample)
            .unwrap_or_else(|err| panic!("reading {}: {err}", sample.display()));
        for config in [
            ferrojt::jt::DUMP_TOC,
            ferrojt::jt::DUMP_TOC | ferrojt::jt::DUMP_LOD0,
            DUMP_ALL,
        ] {
            let text =
                dump(&file, config, 1).unwrap_or_else(|err| panic!("dumping {config}: {err}"));
            serde_json::from_str::<serde_json::Value>(&text)
                .unwrap_or_else(|err| panic!("config {config} of {}: {err}", sample.display()));
        }
    }
}

#[test]
fn dump_stays_valid_json_when_a_shape_loses_its_segment() {
    // A Shape node whose representation cannot be read stays in the graph with
    // nothing to write: a level of detail left out of a filtered write, or a
    // segment the reader does not decode (a Siemens "STT" segment, for example,
    // hides an XZ payload behind a segment type that carries no compression
    // envelope). The object must still close cleanly - a separator after its
    // name makes the whole dump unparseable. The builder writes a bare
    // partition and a shape with no attribute, which is the thinnest case.
    let mut build = ferrojt::jt::JtBuilder::new();
    build
        .set_version(ferrojt::jt::JtVersion::V8_1)
        .expect("JT 8.1 is a supported revision");
    let part = build
        .add_node(build.root(), ferrojt::jt::BuildNode::Part, "part")
        .expect("the root takes a part");
    let triangle = ferrojt::jt::MeshData::new(
        vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        vec![0, 1, 2],
    )
    .expect("a single triangle");
    build.add_shape(part, "shape", triangle).expect("a shape");

    let path = PathBuf::from("shape-without-a-segment.jt");
    let mut data = build.write_to_bytes().expect("the graph can be written");
    let at = usize::try_from(
        ferrojt::jt::JtFile::read(&path, &data)
            .expect("read the built file")
            .toc
            .iter()
            .find(|entry| matches!(entry.kind, 3 | 7 | 8))
            .expect("a shape segment")
            .offset,
    )
    .expect("the segment offset");
    // The envelope of a segment is its GUID (16 bytes), its type and its size;
    // a size of zero leaves the element inside nothing to read.
    data[at + 20..at + 24].copy_from_slice(&0i32.to_le_bytes());

    let broken = ferrojt::jt::JtFile::read(&path, &data).expect("read the broken file");
    let text = dump(&broken, DUMP_ALL, 1).expect("dump the broken file");
    assert!(
        text.contains("\"jt_type\":\"TriStripSet\""),
        "the shape node is gone from the dump"
    );
    serde_json::from_str::<serde_json::Value>(&text)
        .unwrap_or_else(|err| panic!("the dump of a shape without its segment: {err}"));
}
