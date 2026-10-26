//! JSON dump of a JT file in the layout produced by the classic JT tooling.
//!
//! This is a *JT native* view: it walks the logical scene graph exactly as the
//! file stores it and never re-interprets the data as a [`crate::model::Model`].
//! It exists so that the output can be compared against reference dumps and so
//! that round-trip tests can be written against real files.
//!
//! The entry points mirror the reference tool one to one:
//!
//! - [`write_model`] emits the file banner and the `TocTable`,
//! - [`write_meshes_without_lsg`] emits the `MeshTable` fallback used when a
//!   file has no logical scene graph,
//! - [`recurse_down_the_tree`] emits the `LSG` object,
//! - [`write_key_value_stream`] emits a meta data table,
//! - [`dump`] ties them together.
//!
//! Every number is rendered with [`format_g`], which reproduces the C `%g`
//! conversion the reference inherits from `operator<<(ostream&, float)`.

use std::collections::BTreeMap;
use std::io::Write;

use crate::Result;
use crate::jt::model::JtFile;
use crate::jt::model::SceneGraph;
use crate::jt::object::{Attribute, Element, LateLoad, LodGeometry, Node};
use crate::jt::segment::segment_type_name;

/// Emit the table of contents.
pub const DUMP_TOC: i32 = 1;
/// Emit the shape LOD data of every element.
pub const DUMP_LOD0: i32 = 2;
/// Emit the index arrays of a mesh.
pub const DUMP_MESH: i32 = 4;
/// Emit everything; the value the reference tool always passes.
pub const DUMP_ALL: i32 = i32::MAX;

/// The layer information decoded from a meta data key/value stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerInfo {
    /// Value of the `ACTIVELAYERFILTER::` key.
    pub active: String,
    /// Named filters keyed by filter name. The ordering is significant: the
    /// reference keeps them in a `std::map` and emits them sorted.
    pub filters: BTreeMap<String, Vec<u32>>,
}

/// Render a value the way the C `printf` `%g` conversion does at the default
/// precision of six significant digits.
///
/// The reference tool writes every number through `operator<<`, which is `%g`.
/// The rules implemented here are:
///
/// * six significant digits, with trailing zeros removed;
/// * fixed notation while the decimal exponent is in `[-4, 6)`, otherwise
///   scientific notation with a lower case `e` and at least two exponent
///   digits.
#[must_use]
pub fn format_g(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    if !value.is_finite() {
        return if value.is_nan() {
            "nan".to_owned()
        } else if value.is_sign_negative() {
            "-inf".to_owned()
        } else {
            "inf".to_owned()
        };
    }
    let (digits, exp) = split(value);
    render(&digits, exp, value < 0.0)
}

/// The significant digits (trailing zeros removed) and the decimal exponent.
fn split(value: f64) -> (String, i32) {
    // `{:.5e}` is exactly six significant digits, rounded the way `printf`
    // rounds; the exponent it reports is the decimal exponent of the value.
    let text = format!("{value:.5e}");
    let mut digits = digits_of(&text);
    let exp = exponent_of(&text);
    while digits.ends_with('0') {
        digits.pop();
    }
    (digits, exp)
}

/// The significant digits of a `{}` or `{e}` rendering, without sign or point.
fn digits_of(text: &str) -> String {
    text.split('e')
        .next()
        .unwrap_or(text)
        .chars()
        .filter(char::is_ascii_digit)
        .collect()
}

/// The decimal exponent of an `{e}` rendering.
fn exponent_of(text: &str) -> i32 {
    text.split('e')
        .nth(1)
        .and_then(|tail| tail.parse().ok())
        .unwrap_or(0)
}

/// Assemble the final text from digits, exponent and sign.
fn render(digits: &str, exp: i32, negative: bool) -> String {
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if digits.is_empty() {
        return out;
    }
    if !(-4..6).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        let power = exp.abs();
        if power < 10 {
            out.push('0');
        }
        out.push_str(&power.to_string());
    } else if exp < 0 {
        out.push('0');
        out.push('.');
        out.push_str(&"0".repeat((-(exp + 1)) as usize));
        out.push_str(digits);
    } else {
        let point = (exp + 1) as usize;
        if digits.len() <= point {
            out.push_str(digits);
            out.push_str(&"0".repeat(point - digits.len()));
        } else {
            out.push_str(&digits[..point]);
            out.push('.');
            out.push_str(&digits[point..]);
        }
    }
    out
}

/// Write `level` tabs, the reference tool's indentation helper.
fn tabs(out: &mut impl Write, level: i32) -> Result<()> {
    for _ in 0..level.max(0) {
        out.write_all(b"\t")?;
    }
    Ok(())
}

/// Write a comma separated list of numbers without any spacing.
fn write_numbers<T: Copy + Into<f64>>(out: &mut impl Write, values: &[T]) -> Result<()> {
    let mut first = true;
    for value in values {
        if !first {
            out.write_all(b",")?;
        }
        first = false;
        let text = format_g((*value).into());
        out.write_all(text.as_bytes())?;
    }
    Ok(())
}

/// Write a string, escaping backslashes the way the reference tool does.
fn write_text(out: &mut impl Write, text: &str) -> Result<()> {
    for ch in text.chars() {
        if ch == '\\' {
            out.write_all(b"\\\\")?;
        } else {
            let mut buffer = [0_u8; 4];
            out.write_all(ch.encode_utf8(&mut buffer).as_bytes())?;
        }
    }
    Ok(())
}

/// Write the `"name"` member of a node.
fn write_name(out: &mut impl Write, indent: i32, name: &str) -> Result<()> {
    tabs(out, indent)?;
    out.write_all(b"\"name\":\"")?;
    write_text(out, name)?;
    out.write_all(b"\",\n")?;
    Ok(())
}

/// Write the file banner and, when `config` asks for it, the table of contents.
///
/// The `FileName` value is the path the file was opened from. Backslashes are
/// turned into slashes first, which is what the reference tool does before it
/// opens the file.
pub fn write_model(file: &JtFile, out: &mut impl Write, indent: i32, config: i32) -> Result<()> {
    tabs(out, indent)?;
    out.write_all(b"\"FileName\":\"")?;
    let path = file.path.display().to_string().replace('\\', "/");
    write_text(out, &path)?;
    out.write_all(b"\",\n")?;

    tabs(out, indent)?;
    writeln!(out, "\"MajorVersion\":\"{}\",", file.header.major)?;
    tabs(out, indent)?;
    writeln!(out, "\"MinorVersion\":\"{}\",", file.header.minor)?;

    if config & DUMP_TOC == 0 || file.toc.is_empty() {
        return Ok(());
    }

    tabs(out, indent)?;
    out.write_all(b"\"TocTable\": [\n")?;
    let mut first = true;
    for entry in &file.toc {
        tabs(out, indent + 1)?;
        if !first {
            out.write_all(b",")?;
        }
        first = false;
        writeln!(
            out,
            "[\"{guid}\",{kind},\"{name}\"]",
            guid = entry.guid,
            kind = entry.kind,
            name = segment_type_name(entry.kind),
        )?;
    }
    tabs(out, indent)?;
    out.write_all(b"\n],\n")?;
    Ok(())
}

/// Write the `MeshTable` fallback for a file whose logical scene graph is
/// missing, listing every shape LOD segment of the table of contents.
pub fn write_meshes_without_lsg(file: &JtFile, out: &mut impl Write) -> Result<()> {
    if file.toc.is_empty() {
        return Ok(());
    }
    tabs(out, 1)?;
    out.write_all(b"\"MeshTable\": [\n")?;
    let mut first = true;
    for entry in &file.toc {
        if entry.kind != 7 {
            continue;
        }
        tabs(out, 2)?;
        if !first {
            out.write_all(b",")?;
        }
        first = false;
        writeln!(out, "[\"{guid}\",7]", guid = entry.guid)?;
    }
    tabs(out, 1)?;
    out.write_all(b"\n]\n")?;
    Ok(())
}

/// Write a meta data key/value stream as a JSON object.
///
/// `stream` holds NUL separated keys and values, the layout the
/// [`Element::ProxyMetaData`] element stores. Control characters are escaped
/// with a doubled backslash, which is what the reference tool emits.
pub fn write_key_value_stream(stream: &[u8], out: &mut impl Write, indent: i32) -> Result<()> {
    tabs(out, indent)?;
    out.write_all(b"{\n")?;
    tabs(out, indent)?;
    out.write_all(b"\t\"")?;
    let mut key = true;
    let mut at = 0;
    while at < stream.len() {
        let byte = stream[at];
        at += 1;
        if byte == 0 {
            if key {
                out.write_all(b"\":\"")?;
            } else {
                out.write_all(b"\"")?;
                if at == stream.len() {
                    out.write_all(b"\n")?;
                } else {
                    out.write_all(b",\n")?;
                    tabs(out, indent)?;
                    out.write_all(b"\t\"")?;
                }
            }
            key = !key;
            continue;
        }
        match byte {
            b'\n' => out.write_all(b"\\\\n")?,
            b'"' => out.write_all(b"\\\\\"")?,
            b'\r' => out.write_all(b"\\\\r")?,
            0x08 => out.write_all(b"\\\\b")?,
            0x0c => out.write_all(b"\\\\f")?,
            b'\t' => out.write_all(b"\\\\t")?,
            b'\\' => out.write_all(b"\\\\")?,
            _ => out.write_all(&[byte])?,
        }
    }
    tabs(out, indent)?;
    out.write_all(b"}\n")?;
    Ok(())
}

/// Concatenate the meta data streams the late loads of a node refer to.
///
/// Only meta data segments carry a key/value stream; PMI segments are ignored
/// and so are the shape segments that hold geometry.
#[must_use]
pub fn handle_late_loads_meta(graph: &SceneGraph, loads: &[LateLoad]) -> Vec<u8> {
    let mut stream = Vec::new();
    for load in loads {
        if let Some(Element::ProxyMetaData(meta)) = graph.loaded_element(load) {
            stream.extend_from_slice(&meta.stream);
        }
    }
    stream
}

/// Collect the numbers of the `LAYER::` key of a meta data stream.
#[must_use]
pub fn scan_for_layer(stream: &[u8]) -> Vec<u32> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        if key_at(stream, at, b"LAYER::") {
            at += 8;
            read_numbers(stream, &mut at, &mut result);
            at = skip_nul(stream, at);
            continue;
        }
        at = skip_nul(stream, skip_field(stream, at));
        at = skip_nul(stream, skip_field(stream, at));
    }
    result
}

/// Decode the `ACTIVELAYERFILTER::` and `LAYERFILTERnnn::` keys of a meta data
/// stream.
#[must_use]
pub fn scan_for_layer_filter(stream: &[u8]) -> LayerInfo {
    let mut info = LayerInfo::default();
    let mut at = 0;
    while at < stream.len() {
        if key_at(stream, at, b"ACTIVELAYERFILTER::") {
            at += 20;
            let mut value = String::new();
            while at < stream.len() && stream[at] != 0 {
                value.push(stream[at] as char);
                at += 1;
            }
            info.active = value;
            at = skip_nul(stream, at);
            continue;
        }
        if layer_filter_at(stream, at) {
            at += 17;
            let mut name = String::new();
            while at < stream.len() && stream[at] != b'=' && stream[at] != 0 {
                name.push(stream[at] as char);
                at += 1;
            }
            if at < stream.len() && stream[at] == b'=' {
                at += 1;
            }
            let mut numbers = Vec::new();
            read_numbers(stream, &mut at, &mut numbers);
            info.filters.insert(name, numbers);
            at = skip_nul(stream, at);
            continue;
        }
        at = skip_nul(stream, skip_field(stream, at));
        at = skip_nul(stream, skip_field(stream, at));
    }
    info
}

/// Is `key` present at `at` and terminated by a NUL?
fn key_at(stream: &[u8], at: usize, key: &[u8]) -> bool {
    stream[at..].starts_with(key) && stream.get(at + key.len()) == Some(&0)
}

/// Is `at` the start of a `LAYERFILTERnnn::` key?
fn layer_filter_at(stream: &[u8], at: usize) -> bool {
    let Some(head) = stream.get(at..at + 17) else {
        return false;
    };
    head[..11] == *b"LAYERFILTER"
        && head[11..14].iter().all(u8::is_ascii_digit)
        && head[14..16] == *b"::"
        && head[16] == 0
}

/// Read a comma separated list of numbers up to the NUL that ends the value.
fn read_numbers(stream: &[u8], at: &mut usize, out: &mut Vec<u32>) {
    while *at < stream.len() && stream[*at] != 0 {
        let mut text = Vec::new();
        while *at < stream.len() && stream[*at] != b',' && stream[*at] != 0 {
            text.push(stream[*at]);
            *at += 1;
        }
        let text = String::from_utf8_lossy(&text);
        if let Ok(value) = text.trim().parse::<i64>() {
            out.push(value as u32);
        }
        if *at >= stream.len() || stream[*at] == 0 {
            break;
        }
        *at += 1;
    }
}

/// Advance past a NUL, or stay put when the stream ended.
fn skip_nul(stream: &[u8], at: usize) -> usize {
    if at < stream.len() && stream[at] == 0 {
        at + 1
    } else {
        at
    }
}

/// Advance to the next NUL, or to the end of the stream.
fn skip_field(stream: &[u8], at: usize) -> usize {
    let mut at = at;
    while at < stream.len() && stream[at] != 0 {
        at += 1;
    }
    at
}

/// Write the `LSG` object, or the object of one of its descendants.
///
/// `indent` is the indentation of the object itself. It is increased while the
/// content is written, exactly like the reference tool's by-reference counter.
/// A node type the reference does not know emits nothing at all.
pub fn recurse_down_the_tree(
    config: i32,
    indent: &mut i32,
    out: &mut impl Write,
    graph: &SceneGraph,
    node: &Node,
) -> Result<()> {
    if let Node::Partition(partition) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"LSG\":\n")?;
        tabs(out, *indent)?;
        out.write_all(b"{\n")?;
        *indent += 1;
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"Partition\",\n")?;
        write_name(out, *indent, &partition.name)?;
        handle_attributes_json(config, indent, out, graph, node)?;
        handle_all_children_json(config, indent, out, graph, node)?;
        *indent -= 1;
        tabs(out, *indent)?;
        out.write_all(b"}\n")?;
        return Ok(());
    }

    if matches!(
        node,
        Node::Shape(_) | Node::ShapeVertex(_) | Node::Unsupported { .. }
    ) {
        tracing::debug!(
            name = node.name(),
            class = node.class_name(),
            "node type is not part of the dump",
        );
        return Ok(());
    }

    tabs(out, *indent)?;
    out.write_all(b"{\n")?;
    *indent += 1;

    if let Node::Part(part) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"Part\",\n")?;
        write_name(out, *indent, &part.name)?;
        let meta = handle_late_loads_meta(graph, node.late_loads());
        if !meta.is_empty() {
            tabs(out, *indent)?;
            out.write_all(b"\"properties\":\n")?;
            write_key_value_stream(&meta, out, *indent)?;
            tabs(out, *indent)?;
            out.write_all(b",\"layer\":[")?;
            write_layers(out, &scan_for_layer(&meta))?;
            out.write_all(b"],\n")?;
        }
    } else if let Node::MetaData(meta_data) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"MetaData\",\n")?;
        write_name(out, *indent, &meta_data.name)?;
        let meta = handle_late_loads_meta(graph, node.late_loads());
        if !meta.is_empty() {
            tabs(out, *indent)?;
            out.write_all(b"\"properties\":\n")?;
            write_key_value_stream(&meta, out, *indent)?;
            tabs(out, *indent)?;
            out.write_all(b",\"layer-filter\":[")?;
            let info = scan_for_layer_filter(&meta);
            let mut first = true;
            for (name, layers) in &info.filters {
                if !first {
                    out.write_all(b",")?;
                }
                first = false;
                out.write_all(b"{ \"name\":\"")?;
                write_text(out, name)?;
                out.write_all(b"\", \"layer\":[")?;
                write_layers(out, layers)?;
                out.write_all(b"]}")?;
            }
            out.write_all(b"],\n")?;
            tabs(out, *indent)?;
            out.write_all(b"\"active-layer-filter\":\"")?;
            write_text(out, &info.active)?;
            out.write_all(b"\",\n")?;
        }
    } else if let Node::RangeLod(lod) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"RangeLOD\",\n")?;
        write_name(out, *indent, &lod.name)?;
        tabs(out, *indent)?;
        out.write_all(b"\"center\":[")?;
        write_numbers(out, &lod.center)?;
        out.write_all(b"],\n")?;
        tabs(out, *indent)?;
        out.write_all(b"\"limits\":[")?;
        // The reference walks its limit vector from index one, which drops the
        // leading entry; an empty vector stays empty.
        write_numbers(out, lod.limits.get(1..).unwrap_or(&[]))?;
        out.write_all(b"],\n")?;
    } else if let Node::Lod(lod) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"LOD\",\n")?;
        write_name(out, *indent, &lod.name)?;
    } else if let Node::Group(group) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"Group\",\n")?;
        write_name(out, *indent, &group.name)?;
    } else if let Node::Instance(instance) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"Instance\",\n")?;
        write_name(out, *indent, &instance.name)?;
        handle_attributes_json(config, indent, out, graph, node)?;
        tabs(out, *indent)?;
        out.write_all(b"\"InstancedObject\":\n")?;
        if let Some(target) = graph
            .instance_target(node)
            .and_then(|pos| graph.node_at(pos))
        {
            recurse_down_the_tree(config, indent, out, graph, target)?;
        }
    } else if let Node::ShapeTriStripSet(shape) = node {
        tabs(out, *indent)?;
        out.write_all(b"\"jt_type\":\"TriStripSet\",\n")?;
        write_name(out, *indent, &shape.name)?;
        handle_attributes_json(config, indent, out, graph, node)?;
        for load in node.late_loads() {
            if let Some(geometry) = graph.geometry_for(load) {
                write_geometry(config, out, *indent, geometry)?;
            }
        }
    }

    if !matches!(node, Node::Instance(_) | Node::ShapeTriStripSet(_)) {
        handle_attributes_json(config, indent, out, graph, node)?;
        handle_all_children_json(config, indent, out, graph, node)?;
    }
    *indent -= 1;
    tabs(out, *indent)?;
    out.write_all(b"}\n")?;
    Ok(())
}

/// Write a list of layer numbers.
fn write_layers(out: &mut impl Write, layers: &[u32]) -> Result<()> {
    let mut first = true;
    for layer in layers {
        if !first {
            out.write_all(b",")?;
        }
        first = false;
        write!(out, "{layer}")?;
    }
    Ok(())
}

/// Write the geometry of one shape LOD element.
fn write_geometry(
    config: i32,
    out: &mut impl Write,
    indent: i32,
    geometry: &LodGeometry,
) -> Result<()> {
    tabs(out, indent)?;
    writeln!(out, "\"NumIndices\":{},", geometry.indices.len())?;
    tabs(out, indent)?;
    writeln!(out, "\"NumVertices\":{},", geometry.vertex_count)?;
    tabs(out, indent)?;
    writeln!(out, "\"NumNormals\":{},", geometry.normal_count)?;

    tabs(out, indent)?;
    out.write_all(b"\"Indices\":[")?;
    if config & DUMP_MESH != 0 {
        let mut first = true;
        for index in &geometry.indices {
            if !first {
                out.write_all(b",")?;
            }
            first = false;
            write!(out, "{index}")?;
        }
    }
    out.write_all(b"],\n")?;

    tabs(out, indent)?;
    out.write_all(b"\"Vertices\":[")?;
    write_numbers(out, &geometry.vertices)?;
    out.write_all(b"],\n")?;

    tabs(out, indent)?;
    out.write_all(b"\"Normals\":[")?;
    write_numbers(out, &geometry.normals)?;
    out.write_all(b"]\n")?;
    Ok(())
}

/// Write the attributes attached to a node.
pub fn handle_attributes_json(
    _config: i32,
    indent: &mut i32,
    out: &mut impl Write,
    graph: &SceneGraph,
    node: &Node,
) -> Result<()> {
    for attribute in graph.attributes_of(node) {
        match attribute {
            Attribute::GeometricTransform(transform) => {
                tabs(out, *indent)?;
                out.write_all(b"\"Attribute_GeometricTransform\": [")?;
                write_numbers(out, &transform.matrix)?;
                out.write_all(b"],\n")?;
            }
            Attribute::Material(material) => {
                let colours = [
                    ("Attribute_MaterialDiffuseColor", &material.diffuse),
                    ("Attribute_AmbientColor", &material.ambient),
                    ("Attribute_SpecularColor", &material.specular),
                    ("Attribute_EmissionColor", &material.emission),
                ];
                for (name, colour) in colours {
                    tabs(out, *indent)?;
                    write!(out, "\"{name}\": [")?;
                    write_numbers(out, colour)?;
                    out.write_all(b"],\n")?;
                }
            }
            Attribute::Unsupported { class_name } => {
                tracing::debug!(class_name, "attribute type is not part of the dump");
                tabs(out, *indent)?;
                out.write_all(b"Unknown Attribute type \n")?;
            }
        }
    }
    Ok(())
}

/// Write the children of a node. A child that does not resolve becomes the
/// empty object, as it does in the reference.
pub fn handle_all_children_json(
    config: i32,
    indent: &mut i32,
    out: &mut impl Write,
    graph: &SceneGraph,
    node: &Node,
) -> Result<()> {
    let children = graph.children_of(node);
    if children.is_empty() {
        return Ok(());
    }
    tabs(out, *indent)?;
    out.write_all(b"\"children\":\n")?;
    tabs(out, *indent)?;
    out.write_all(b"[\n")?;
    *indent += 1;
    for (position, child) in children.iter().enumerate() {
        if position != 0 {
            tabs(out, *indent)?;
            out.write_all(b",\n")?;
        }
        match child.and_then(|pos| graph.node_at(pos)) {
            Some(child) => recurse_down_the_tree(config, indent, out, graph, child)?,
            None => {
                tabs(out, *indent)?;
                out.write_all(b"{}")?;
            }
        }
    }
    *indent -= 1;
    tabs(out, *indent)?;
    out.write_all(b"]\n")?;
    Ok(())
}

/// Read a JT file and return its JSON dump.
///
/// The result is one complete JSON object, the way the reference tool writes a
/// single file. `indent` is the indentation of that object's members; the
/// reference uses one for a single file and two when several files are dumped
/// as an array.
///
/// # Errors
///
/// [`Error::Io`](crate::Error::Io) is returned when the dump cannot be built.
pub fn dump(file: &JtFile, config: i32, indent: i32) -> Result<String> {
    let mut out = Vec::new();
    out.write_all(b"{\n")?;
    write_model(file, &mut out, indent, config)?;
    if let Some(graph) = file.lsg.as_ref() {
        if let Some(root) = graph.root() {
            let mut level = indent;
            recurse_down_the_tree(config, &mut level, &mut out, graph, root)?;
            out.write_all(b"}\n")?;
            return Ok(String::from_utf8_lossy(&out).into_owned());
        }
    }
    write_meshes_without_lsg(file, &mut out)?;
    out.write_all(b"}\n")?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_printf_g() {
        let cases = [
            (0.0_f64, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (80.0, "80"),
            (100.0, "100"),
            (0.6, "0.6"),
            (0.12, "0.12"),
            (14.9998, "14.9998"),
            (-15.0001, "-15.0001"),
            (39.9991, "39.9991"),
            (0.5, "0.5"),
            (1.0 / 3.0, "0.333333"),
            (123_456.0, "123456"),
            (1_234_567.0, "1.23457e+06"),
            (-11e7, "-1.1e+08"),
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (-5.722_05e-5, "-5.72205e-05"),
            (-1.301_04e-16, "-1.30104e-16"),
            (1.836_97e-15, "1.83697e-15"),
            (8.3286e-26, "8.3286e-26"),
            (2.966_46e-17, "2.96646e-17"),
            (7.830_64e-6, "7.83064e-06"),
            (0.257_58, "0.25758"),
        ];
        for (value, want) in cases {
            assert_eq!(format_g(value), want, "format_g({value})");
        }
    }

    #[test]
    fn formats_thirty_two_bit_values() {
        assert_eq!(format_g(f64::from(-5.722_05e-5_f32)), "-5.72205e-05");
        assert_eq!(format_g(f64::from(1.0 / 3.0_f32)), "0.333333");
        assert_eq!(format_g(f64::from(16_777_216.0_f32)), "1.67772e+07");
    }

    #[test]
    fn writes_a_key_value_stream() {
        let stream = b"Name::\x00block\x00CAD_MASS::\x000.257580\x00";
        let mut out = Vec::new();
        write_key_value_stream(stream, &mut out, 0).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\n\t\"Name::\":\"block\",\n\t\"CAD_MASS::\":\"0.257580\"\n}\n"
        );
    }

    #[test]
    fn escapes_control_characters_in_a_stream() {
        let stream = b"k\0a\\b\"c\nd\0";
        let mut out = Vec::new();
        write_key_value_stream(stream, &mut out, 0).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("a\\\\b"), "{text}");
        assert!(text.contains("\\\\\"c"), "{text}");
        assert!(text.contains("\\\\nd"), "{text}");
    }

    #[test]
    fn scans_layers_and_filters() {
        let stream = b"LAYER::\x0020\x00LAYERFILTER001::\x00ALL=1,2,256\x00\
            LAYERFILTER000::\x00LAYCUR=20\x00ACTIVELAYERFILTER::\x00\x00";
        assert_eq!(scan_for_layer(stream), vec![20]);
        let info = scan_for_layer_filter(stream);
        assert_eq!(info.active, "");
        let keys: Vec<&str> = info.filters.keys().map(String::as_str).collect();
        assert_eq!(keys, ["ALL", "LAYCUR"]);
        assert_eq!(info.filters["ALL"], vec![1, 2, 256]);
        assert_eq!(info.filters["LAYCUR"], vec![20]);
    }

    #[test]
    fn ignores_a_truncated_key() {
        let stream = b"LAYERFIL\0value\0";
        assert!(scan_for_layer_filter(stream).filters.is_empty());
        assert!(scan_for_layer(stream).is_empty());
    }
}
