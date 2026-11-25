//! Writing a JT file back out.
//!
//! [`write_to_bytes`] rebuilds a file from a [`JtFile`] that was read from
//! disk, optionally dropping the coarse tiers of a range LOD or the parts a
//! layer filter rejects. What survives is written back the way the reader
//! expects it, so reading the result again yields the same scene graph, the
//! same geometry and the same metadata — the same dump and the same picture.
//!
//! By default only the logical scene graph is re-serialised. Every other
//! segment — the shape representations, the meta data, the PMI and the B-rep
//! segments — is copied out of the source file byte for byte, because a
//! representation is stored in one of the quantised or topologically compressed
//! encodings the reader decodes but does not re-encode. Such a segment is left
//! out only when every node that referenced it is gone.
//!
//! [`WriteOptions::version`] can ask for another JT revision. Rewriting the
//! scene graph at another revision is a matter of the version gates the reader
//! uses, and the segments in between are copied unchanged — that works for the
//! meta data, PMI and B-rep segments. A shape representation cannot be copied,
//! because the revision decides which coder holds its triangles, so it is
//! rebuilt from the decoded geometry with the encoder of the target revision.
//! That is lossy in a hard way: the topological coder of JT 9.x and 10.x only
//! accepts a closed two-manifold without degenerate triangles, which most
//! tessellations are not, and JT 8.x has no coder at all for such a mesh. A
//! rewrite that a representation cannot survive is refused rather than written
//! as a file whose shapes come back empty, and the 8.x and the 9.x/10.x
//! families are therefore not offered to each other — see
//! [`JtVersion::compatible`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::codec::{ByteOrder, WriterExt, deflate};
use crate::error::{Error, Result};
use crate::jt::Cursor;
use crate::jt::build::{MeshData, property_class, representation, shape_segment};
use crate::jt::guid::{Guid, types};
use crate::jt::model::{JtFile, LayerFilter, LodWalk, ObjectRef, SceneGraph};
use crate::jt::object::{
    Attribute, Class, LateLoad, LateLoaded, Node, NodeBase, Property, RawElement, Shape, ShapeLod,
    StringProperty, class_of, read_element_envelope,
};
use crate::jt::segment::{FileHeader, TocEntry, has_compression_header, read_segment};
use crate::jt::version::JtVersion;

/// Base type byte of an instance atom.
pub(crate) const BASE_INSTANCE: u8 = 0;
/// Base type byte of a group, part, meta data, partition or range LOD atom.
pub(crate) const BASE_GROUP: u8 = 1;
/// Base type byte of a shape atom.
pub(crate) const BASE_SHAPE: u8 = 2;
/// Base type byte of an attribute atom.
pub(crate) const BASE_ATTRIBUTE: u8 = 3;
/// Envelope of an atom of a known class: length, GUID, base type, object id.
const KNOWN_ENVELOPE: usize = 4 + 16 + 1 + 4;
/// Envelope of an atom whose class the reader does not know: length and GUID.
///
/// The reader stops after the GUID there, but a file still frames such an atom
/// with a base type and an object id — see [`read_property_atom`].
const UNKNOWN_ENVELOPE: usize = 4 + 16;
/// Offset of the object id inside an atom of a known class.
const OBJECT_ID_OFFSET: usize = 4 + 16 + 1;
/// Offset of the class version inside a property atom.
///
/// A property atom stores that version, then the state, then a second version
/// field, and the first one is what names the class. JT 8.x stores none.
const PROPERTY_VERSION_OFFSET: usize = OBJECT_ID_OFFSET + 4;
/// The state every property atom stores.
///
/// The reader keeps it without interpreting it, and the reference writes this
/// value for every property it emits.
const PROPERTY_STATE: u32 = 0x4000_0000;
/// Envelope of a segment whose type carries a compression header.
///
/// The declared size counts the four bytes of the size field itself, the GUID,
/// the segment type, the compression flag, the length of the compressed stream
/// and the algorithm byte — so a segment is 33 bytes bigger than its payload.
pub(crate) const COMPRESSION_ENVELOPE: usize = 4 + 16 + 4 + 4 + 4 + 1;

/// What a rewritten file should keep.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// Keep every tier of a range LOD.
    ///
    /// Off by default, which keeps only the finest tier — the same selection
    /// [`JtFile::to_model`] makes for an export.
    pub all_lods: bool,
    /// Ignore the layer filters of the file.
    pub no_layer_filter: bool,
    /// Use this named filter instead of the default one.
    ///
    /// Combining this with [`WriteOptions::no_layer_filter`] is an error.
    pub layer_filter: Option<String>,
    /// DEFLATE the rebuilt scene graph.
    ///
    /// On by default, and it should stay on: a segment of a type that carries a
    /// compression header is only read as it is declared when the flag names a
    /// known algorithm, so a strict consumer refuses a file whose scene graph is
    /// stored raw. No sample of any revision does that. Turning this off keeps
    /// the bytes of the source segment as they are, which is what a byte-level
    /// comparison of a round trip wants.
    pub compress: bool,
    /// Write the file at this JT revision instead of the one it has.
    ///
    /// `None`, the default, keeps the revision of the source file, which is
    /// what makes the rewrite lossless: every segment that is not the scene
    /// graph is then copied byte for byte. `Some` rebuilds the scene graph, the
    /// header and the TOC table at that revision, copies the segments whose
    /// framing does not depend on it, and re-encodes every shape
    /// representation with the coder of the target revision.
    ///
    /// Only revisions of the same representation family can be exchanged: JT
    /// 8.x stores its triangles in the vertex based lossless representation,
    /// JT 9.x and 10.x in the topologically compressed one, and a file carries
    /// only the coder of its own era. See [`JtVersion::compatible`].
    ///
    /// [`JtVersion::compatible`]: crate::jt::JtVersion::compatible
    pub version: Option<JtVersion>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            all_lods: false,
            no_layer_filter: false,
            layer_filter: None,
            compress: true,
            version: None,
        }
    }
}

/// Rebuild a JT file.
///
/// `data` is the content of the file `file` was read from: its banner is
/// reused and the segments that are not rebuilt are copied out of it unchanged,
/// unless [`WriteOptions::version`] asks for another revision.
///
/// # Errors
///
/// [`Error::Invalid`] is returned when the file has no logical scene graph,
/// when a named layer filter does not exist, when the requested revision is
/// unsupported or of another representation family than the source, when a
/// representation cannot be re-encoded for the requested revision, or when the
/// scene graph holds a class the writer does not model.
pub fn write_to_bytes(file: &JtFile, data: &[u8], options: &WriteOptions) -> Result<Vec<u8>> {
    if options.no_layer_filter && options.layer_filter.is_some() {
        return Err(Error::invalid(
            0,
            "a layer filter cannot be named and ignored at the same time",
        ));
    }
    let source = JtVersion::new(file.header.major, file.header.minor);
    let version = options.version.unwrap_or(source);
    version.validate()?;
    if !source.compatible(version) {
        return Err(Error::invalid(
            0,
            format!(
                "a JT {source} file cannot be written as JT {version}: JT 8.x stores its \
                 triangles in the vertex based representation, JT 9.x and 10.x in the \
                 topologically compressed one, and only the coder of the revision of the \
                 source file is available"
            ),
        ));
    }
    let graph = file.lsg.as_ref().ok_or_else(|| {
        Error::invalid(
            0,
            "the file has no logical scene graph, so it cannot be written",
        )
    })?;
    let filter = resolve_filter(file, options)?;
    let plan = Plan::build(graph, options, filter.as_ref());
    // The segment type of a representation follows its level of detail, which
    // JT never stores — it is the position under the range LOD. Only a write
    // that changes the major revision has to work it out: otherwise the types
    // of the source file are kept as they are.
    let restructured = version.major != source.major;
    let kinds = if restructured {
        segment_kinds(graph, &plan, version)?
    } else {
        BTreeMap::new()
    };

    let lsg_offset = file
        .segment_offset(&file.header.lsg_guid)
        .ok_or_else(|| Error::invalid(0, "the scene graph is not in the TOC table"))?;
    let segment = read_segment(data, lsg_offset, &file.header)?;
    let (atoms, rows) = read_atoms_and_table(&segment.body, file.header.order, file.header.major)?;

    let mut writer = Writer {
        file,
        graph,
        options,
        plan: &plan,
        version,
        kinds,
        ids: HashMap::new(),
        atoms,
        rows,
        pairs: HashMap::new(),
    };
    writer.number_nodes_and_attributes();
    writer.collect_rows();
    writer.number_properties();
    writer.build(data)
}

/// Rebuild a JT file and store it at `path`.
///
/// The file `file` was read from is read again to copy the segments that are
/// not rebuilt.
///
/// # Errors
///
/// See [`write_to_bytes`], plus any I/O error while reading or writing.
pub fn write_to_file(file: &JtFile, path: &Path, options: &WriteOptions) -> Result<()> {
    let data = std::fs::read(&file.path)?;
    let bytes = write_to_bytes(file, &data, options)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// The filter a rewrite applies, `None` when nothing is filtered.
fn resolve_filter(file: &JtFile, options: &WriteOptions) -> Result<Option<LayerFilter>> {
    if options.no_layer_filter {
        return Ok(None);
    }
    match options.layer_filter.as_deref() {
        Some(name) => file.layer_filter(Some(name)),
        None => file.layer_filter(None),
    }
}

/// The segment type each surviving segment gets in the target revision.
///
/// A shape segment is named by the level of detail it holds: JT 9.x calls every
/// one of them type 6 ("Shape"), JT 8.x and 10.x number them 7 to 16 for LOD0
/// to LOD9. JT never stores that tier — it is the position of the group under
/// its range LOD — so it is taken from the walk that decides which tiers to
/// keep, exactly as [`JtFile::to_model`] records it on a mesh. A segment no
/// surviving node references is left out, and its type stays as the source
/// named it.
///
/// [`JtFile::to_model`]: crate::jt::JtFile::to_model
fn segment_kinds(
    graph: &SceneGraph,
    plan: &Plan,
    version: JtVersion,
) -> Result<BTreeMap<Guid, i32>> {
    let mut kinds: BTreeMap<Guid, i32> = BTreeMap::new();
    let Some(root) = graph.root else {
        return Ok(kinds);
    };
    collect_kinds(
        graph,
        plan,
        root,
        LodWalk::ROOT,
        version,
        &mut kinds,
        &mut HashSet::new(),
    )?;
    Ok(kinds)
}

/// Walk the surviving nodes once per path and record the type of each segment.
///
/// A subtree reached more than once is walked again on purpose: the tier of a
/// representation depends on where it is reached from, and a file that
/// instantiates one shape under two range LODs at different tiers names its
/// segment accordingly. `path` is the cycle guard — a reference cycle is
/// walked once per branch, never forever.
fn collect_kinds(
    graph: &SceneGraph,
    plan: &Plan,
    pos: usize,
    tier: LodWalk,
    version: JtVersion,
    kinds: &mut BTreeMap<Guid, i32>,
    path: &mut HashSet<usize>,
) -> Result<()> {
    let Some(&index) = plan.index_of.get(&pos) else {
        return Ok(());
    };
    if !path.insert(pos) {
        return Ok(());
    }
    let kept = &plan.kept[index];
    let node = &graph.nodes[kept.pos].node;
    for load in &kept.loads {
        let Some(at) = graph.loaded_pos(load) else {
            continue;
        };
        let stored = &graph.loaded[at];
        if stored.element.geometry().is_none() {
            continue;
        }
        let kind = match tier.level() {
            Some(_) if version.major == 9 => 6,
            Some(level) if level >= 10 => {
                return Err(Error::invalid(
                    0,
                    "a level of detail beyond LOD9 has no segment type",
                ));
            }
            Some(level) => 7 + level as i32,
            None => {
                if version.major == 9 {
                    6
                } else {
                    stored.segment_type
                }
            }
        };
        if let Some(&known) = kinds.get(&stored.segment) {
            if known != kind {
                return Err(Error::invalid(
                    0,
                    format!(
                        "the representation {} is used at two different levels of detail, so its \
                         segment type cannot be chosen",
                        stored.segment
                    ),
                ));
            }
        }
        kinds.insert(stored.segment, kind);
    }
    let mut tier = if matches!(node, Node::RangeLod(_)) {
        LodWalk::FIRST
    } else {
        tier
    };
    for child in &kept.children {
        let Some(child) = child else {
            continue;
        };
        let child_pos = plan.kept[*child].pos;
        collect_kinds(
            graph,
            plan,
            child_pos,
            tier.for_child(),
            version,
            kinds,
            path,
        )?;
        // This walk only numbers tiers, it never decides which survive.
        tier.next_tier(graph.nodes[child_pos].node.class_name(), false);
    }
    if let Some(target) = kept.target {
        let target_pos = plan.kept[target].pos;
        collect_kinds(
            graph,
            plan,
            target_pos,
            tier.for_child(),
            version,
            kinds,
            path,
        )?;
    }
    path.remove(&pos);
    Ok(())
}

/// `true` for a segment holding a shape representation: types 6 to 16.
fn is_shape_segment(kind: i32) -> bool {
    (6..=16).contains(&kind)
}

/// `true` when the framing of a segment type does not depend on the revision.
///
/// The logical scene graph is rebuilt from scratch and a shape representation
/// is re-encoded, so neither counts here. Everything else the reader models
/// either carries no version-gated field at all — a wireframe, ULP, LWPA or
/// AEC segment — or stores its revision inside its own body, as an extended
/// B-rep does in its version triplet.
fn is_copyable_segment(kind: i32) -> bool {
    matches!(kind, 2 | 3 | 4 | 17 | 18 | 20 | 23 | 24 | 30 | 32 | 33)
}

/// One node that survives, with the references it still has.
struct Kept {
    /// Position of the node inside [`SceneGraph::nodes`].
    pos: usize,
    /// Attributes, as positions inside [`SceneGraph::attributes`].
    attributes: Vec<usize>,
    /// Late loads that survive, as the node listed them.
    loads: Vec<LateLoad>,
    /// Children, as indices into the kept list; `None` is a reference the
    /// source file could not resolve either.
    children: Vec<Option<usize>>,
    /// The instantiated subtree, as an index into the kept list.
    target: Option<usize>,
}

/// What a rewrite keeps.
struct Plan {
    /// The surviving nodes, in the order they are written out.
    kept: Vec<Kept>,
    /// Position in the source graph to index in [`Plan::kept`].
    index_of: HashMap<usize, usize>,
    /// Segments a surviving node still references.
    segments: HashSet<Guid>,
    /// Representations kept so far that really carry vertices. A range span
    /// compares this around each child to find its finest tier with geometry.
    geometry_rows: usize,
}

impl Plan {
    /// Decide which nodes survive the level-of-detail and layer filters.
    ///
    /// The walk mirrors [`JtFile::to_model`]: a part the layer filter rejects
    /// keeps its node but loses its attributes, its representations and its
    /// children, and below a range LOD only the finest tier that has geometry
    /// keeps its representations — an empty LOD0 falls through to the next one.
    /// A subtree reached more than once — an instance and the
    /// shape it instantiates — is kept once, so the rewrite stays an instance
    /// rather than turning into a copy.
    fn build(graph: &SceneGraph, options: &WriteOptions, filter: Option<&LayerFilter>) -> Self {
        let mut plan = Plan {
            kept: Vec::new(),
            index_of: HashMap::new(),
            segments: HashSet::new(),
            geometry_rows: 0,
        };
        if let Some(root) = graph.root {
            plan.walk(graph, options, filter, root, LodWalk::ROOT);
        }
        plan
    }

    fn walk(
        &mut self,
        graph: &SceneGraph,
        options: &WriteOptions,
        filter: Option<&LayerFilter>,
        pos: usize,
        tier: LodWalk,
    ) -> usize {
        if let Some(&index) = self.index_of.get(&pos) {
            return index;
        }
        let index = self.kept.len();
        self.index_of.insert(pos, index);
        self.kept.push(Kept {
            pos,
            attributes: Vec::new(),
            loads: Vec::new(),
            children: Vec::new(),
            target: None,
        });
        let node = &graph.nodes[pos].node;
        let drawn = match node {
            Node::Part(_) => filter.is_none_or(|filter| filter.keeps(&graph.layers_of(node))),
            _ => true,
        };
        if !drawn {
            return index;
        }

        for deferred in node.attributes() {
            if let Some(ObjectRef::Attribute(at)) = graph.object(deferred.object_id) {
                self.kept[index].attributes.push(at);
            }
        }

        // A tier that is dropped loses the representation of its shape, but a
        // node that carries meta data keeps those late loads.
        let keeps_geometry = options.all_lods || tier.keeps_geometry();
        for load in node.late_loads() {
            let loaded = graph.loaded_pos(load).and_then(|at| graph.loaded.get(at));
            let geometry = loaded.is_some_and(|stored| stored.element.geometry().is_some());
            if geometry && !keeps_geometry {
                continue;
            }
            if let Some(stored) = loaded {
                self.segments.insert(stored.segment);
            }
            self.kept[index].loads.push(*load);
            // Only a representation with vertices makes a tier the finest one
            // of its span; an empty LOD0 lets the next tier through.
            if loaded.is_some_and(|stored| {
                stored
                    .element
                    .geometry()
                    .is_some_and(|lod| !lod.vertices.is_empty())
            }) {
                self.geometry_rows += 1;
            }
        }

        let mut tier = if matches!(node, Node::RangeLod(_)) {
            LodWalk::FIRST
        } else {
            tier
        };
        for child in graph.children_of(node) {
            let Some(child) = child else {
                self.kept[index].children.push(None);
                continue;
            };
            let before = self.geometry_rows;
            let kept = self.walk(graph, options, filter, child, tier.for_child());
            self.kept[index].children.push(Some(kept));
            tier.next_tier(
                graph.nodes[child].node.class_name(),
                self.geometry_rows > before,
            );
        }
        if let Some(target) = graph.instance_target(node) {
            let kept = self.walk(graph, options, filter, target, tier.for_child());
            self.kept[index].target = Some(kept);
        }
        index
    }
}

/// One property atom of the source scene graph, kept as the file wrote it.
///
/// [`read_property_atom`] decides which atoms are picked up.
struct Atom {
    /// Object id the source file gave the atom.
    object_id: i32,
    /// The whole atom, its length field included.
    bytes: Vec<u8>,
}

/// One row of the property table of the source scene graph.
struct Row {
    /// Object id the row belongs to.
    object_id: i32,
    /// Key/value id pairs, as the source file listed them.
    pairs: Vec<(i32, i32)>,
}

/// Builds the bytes of a rewritten file.
struct Writer<'a> {
    file: &'a JtFile,
    graph: &'a SceneGraph,
    options: &'a WriteOptions,
    plan: &'a Plan,
    /// Revision the file is written at.
    version: JtVersion,
    /// Segment type each surviving segment gets in the target revision, keyed
    /// by segment GUID.
    kinds: BTreeMap<Guid, i32>,
    /// Old object id to new object id, for every object that survives.
    ids: HashMap<i32, i32>,
    /// Property atoms of the source scene graph, in file order.
    atoms: Vec<Atom>,
    /// Property table rows of the source scene graph, as the source listed them.
    rows: Vec<Row>,
    /// The rows that survive, keyed by the old id of their object. The ids in
    /// the pairs are still the old ones; only the numbering translates them.
    pairs: HashMap<i32, Vec<(i32, i32)>>,
}

impl Writer<'_> {
    /// Number every node and attribute that survives, in the order they are
    /// written out. Nodes and attributes share one id table, exactly as in the
    /// source file.
    fn number_nodes_and_attributes(&mut self) {
        let mut next = 0i32;
        let mut seen: HashSet<usize> = HashSet::new();
        for kept in &self.plan.kept {
            self.ids.insert(self.graph.nodes[kept.pos].id, next);
            next += 1;
            for at in &kept.attributes {
                // Several nodes can share one attribute; it is written once and
                // therefore numbered once.
                if !seen.insert(*at) {
                    continue;
                }
                self.ids.insert(self.graph.attributes[*at].id, next);
                next += 1;
            }
        }
    }

    /// Drop the pairs a filter has emptied and the rows left with nothing.
    ///
    /// A late load that was dropped goes with its representation, so the pair
    /// that carried it goes as well. This has to be settled before the
    /// properties are numbered: whether an atom is written out is decided by the
    /// rows that survive, so numbering and emission follow one rule and writing
    /// an already written file changes nothing.
    fn collect_rows(&mut self) {
        // A key or a value is only writable when its own atom was picked up.
        let known: HashSet<i32> = self.atoms.iter().map(|atom| atom.object_id).collect();
        let kept_loads = self.kept_loads();
        for row in &self.rows {
            if !self.ids.contains_key(&row.object_id) {
                continue;
            }
            let pairs: Vec<(i32, i32)> = row
                .pairs
                .iter()
                .filter(|(_, value)| self.referenced(*value, &kept_loads))
                .filter(|(key, value)| known.contains(key) && known.contains(value))
                .copied()
                .collect();
            if !pairs.is_empty() {
                self.pairs.insert(row.object_id, pairs);
            }
        }
    }

    /// Number the properties, after the nodes and attributes and in the order
    /// the atoms appear in the source. A property is numbered exactly when a
    /// surviving row refers to it, so that the atoms written out and the table
    /// that names them stay in step.
    fn number_properties(&mut self) {
        let mut next = self.ids.len() as i32;
        let mut used: HashSet<i32> = HashSet::new();
        for pairs in self.pairs.values() {
            for (key, value) in pairs {
                used.insert(*key);
                used.insert(*value);
            }
        }
        for atom in &self.atoms {
            if used.contains(&atom.object_id) {
                self.ids.insert(atom.object_id, next);
                next += 1;
            }
        }
    }

    /// The late-load properties that survive, by property object id.
    fn kept_loads(&self) -> HashSet<i32> {
        self.plan
            .kept
            .iter()
            .flat_map(|kept| kept.loads.iter().map(|load| load.property_id))
            .collect()
    }

    /// Does a surviving row still refer to the property `value`?
    fn referenced(&self, value: i32, kept_loads: &HashSet<i32>) -> bool {
        match self.graph.property(value) {
            Some(Property::LateLoaded(_)) => kept_loads.contains(&value),
            _ => true,
        }
    }

    /// The new id of an object that survives, `-1` when it does not.
    fn id(&self, old: i32) -> i32 {
        self.ids.get(&old).copied().unwrap_or(-1)
    }

    /// Assemble the whole file.
    fn build(&self, data: &[u8]) -> Result<Vec<u8>> {
        let header = &self.file.header;
        let order = header.order;
        let major = self.version.major;
        let lsg_guid = header.lsg_guid;

        let body = self.lsg_body()?;
        let payload = if self.options.compress {
            deflate(&body)?
        } else {
            body
        };
        let kind = self
            .file
            .toc
            .iter()
            .find(|entry| entry.guid == lsg_guid)
            .map_or(1, |entry| entry.kind);
        let lsg = segment_bytes(order, &lsg_guid, kind, &payload, self.options.compress)?;

        // Every other segment is copied out of the source file, except one no
        // surviving node references any more. A shape representation cannot be
        // copied when the major revision changes — its triangles live in the
        // coder of the revision of the source file — so it is rebuilt from the
        // decoded geometry. A different minor revision changes no framing at
        // all: every version gate of the specification looks at the major.
        let dropped = self.dropped_segments();
        let restructured = self.version.major != header.major;
        let mut segments: Vec<(Guid, i32, Vec<u8>)> = Vec::with_capacity(self.file.toc.len());
        for entry in &self.file.toc {
            if entry.guid == lsg_guid {
                segments.push((lsg_guid, kind, lsg.clone()));
                continue;
            }
            if dropped.contains(&entry.guid) {
                tracing::debug!(guid = %entry.guid, "a segment no node references is left out");
                continue;
            }
            let kind = if restructured {
                self.kinds.get(&entry.guid).copied().unwrap_or(entry.kind)
            } else {
                entry.kind
            };
            if restructured && is_shape_segment(entry.kind) {
                let bytes = self.representation_segment(&entry.guid, kind)?;
                segments.push((entry.guid, kind, bytes));
                continue;
            }
            if restructured && !is_copyable_segment(entry.kind) {
                return Err(Error::invalid(
                    0,
                    format!(
                        "the segment {} has segment type {}, whose framing depends on the JT \
                         revision, so it cannot be rewritten as JT {}",
                        entry.guid, entry.kind, self.version
                    ),
                ));
            }
            let end = entry
                .offset
                .checked_add(u64::from(entry.length))
                .ok_or_else(|| Error::invalid(entry.offset, "the segment length overflows"))?;
            let start = usize::try_from(entry.offset)
                .map_err(|_| Error::invalid(entry.offset, "the segment offset is out of range"))?;
            if end as usize > data.len() {
                return Err(Error::invalid(
                    entry.offset,
                    "the TOC table points past the file",
                ));
            }
            segments.push((entry.guid, kind, data[start..end as usize].to_vec()));
        }

        let header_len = header_length(major);
        let total = header_len + segments.iter().map(|entry| entry.2.len()).sum::<usize>();
        let mut out: Vec<u8> = Vec::with_capacity(total + 64);
        if self.version != JtVersion::new(header.major, header.minor) {
            write_generated_banner(&mut out, order, self.version, &lsg_guid, total)?;
        } else {
            write_banner(&mut out, data, header, total)?;
        }
        let mut offset = header_len;
        let mut toc: Vec<TocEntry> = Vec::with_capacity(segments.len());
        for (guid, kind, bytes) in &segments {
            out.extend_from_slice(bytes);
            toc.push(TocEntry {
                guid: *guid,
                offset: offset as u64,
                length: bytes.len() as u32,
                kind: *kind,
            });
            offset += bytes.len();
        }
        write_toc(&mut out, &toc, order, major)?;
        Ok(out)
    }

    /// Rebuild one shape representation segment for the target revision.
    ///
    /// The geometry is the one the reader decoded from the source file, so the
    /// triangles are the same; only the coder changes. A mesh the coder of the
    /// target revision cannot hold is an error rather than a segment whose
    /// shape comes back empty.
    fn representation_segment(&self, guid: &Guid, kind: i32) -> Result<Vec<u8>> {
        let stored = self
            .graph
            .loaded
            .iter()
            .find(|stored| stored.segment == *guid)
            .ok_or_else(|| {
                Error::invalid(0, format!("the segment {guid} holds no representation"))
            })?;
        let Some(geometry) = stored.element.geometry() else {
            return Err(Error::invalid(
                0,
                format!("the segment {guid} holds no shape representation"),
            ));
        };
        let mesh = MeshData::from_geometry(geometry);
        let element = representation(&mesh, self.version).map_err(|err| match err {
            Error::Invalid { message, .. } => Error::invalid(
                0,
                format!(
                    "the representation {guid} cannot be written as JT {}: {message}",
                    self.version
                ),
            ),
            err => err,
        })?;
        Ok(shape_segment(*guid, kind, &element, self.version))
    }

    /// Segments the source file references but no surviving node does.
    fn dropped_segments(&self) -> HashSet<Guid> {
        let mut all: HashSet<Guid> = HashSet::new();
        for stored in &self.graph.nodes {
            for load in stored.node.late_loads() {
                if let Some(at) = self.graph.loaded_pos(load) {
                    all.insert(self.graph.loaded[at].segment);
                }
            }
        }
        all.retain(|guid| !self.plan.segments.contains(guid));
        all
    }

    /// Serialise the logical scene graph: the node and attribute atoms, then
    /// the properties, then the table that ties them together.
    fn lsg_body(&self) -> Result<Vec<u8>> {
        let order = self.file.header.order;
        let major = self.version.major;
        let mut body: Vec<u8> = Vec::new();

        let kept_ids: Vec<i32> = self
            .plan
            .kept
            .iter()
            .map(|kept| self.id(self.graph.nodes[kept.pos].id))
            .collect();
        // Several nodes can share one attribute, and it is written once, under
        // the first node that refers to it.
        let mut written: HashSet<usize> = HashSet::new();
        for (index, kept) in self.plan.kept.iter().enumerate() {
            let node = &self.graph.nodes[kept.pos].node;
            let out = OutNode {
                id: kept_ids[index],
                attributes: kept
                    .attributes
                    .iter()
                    .map(|at| self.id(self.graph.attributes[*at].id))
                    .collect(),
                children: kept
                    .children
                    .iter()
                    .map(|child| child.map_or(-1, |child| kept_ids[child]))
                    .collect(),
                target: kept.target.map(|at| kept_ids[at]),
            };
            let bytes = node_body(node, &out, order, major)?;
            write_atom(
                &mut body,
                order,
                &node_guid(node),
                node_base_type(node),
                out.id,
                &bytes,
            )?;
            for at in &kept.attributes {
                if !written.insert(*at) {
                    continue;
                }
                let stored = &self.graph.attributes[*at];
                let bytes = attribute_body(&stored.attribute, order, major)?;
                write_atom(
                    &mut body,
                    order,
                    &attribute_guid(&stored.attribute)?,
                    BASE_ATTRIBUTE,
                    self.id(stored.id),
                    &bytes,
                )?;
            }
        }
        write_end_of_elements(&mut body, order)?;

        let restructured = self.version.major != self.file.header.major;
        for atom in &self.atoms {
            let Some(new) = self.ids.get(&atom.object_id) else {
                continue;
            };
            if restructured {
                self.write_property_atom(&mut body, atom, *new)?;
                continue;
            }
            let mut bytes = atom.bytes.clone();
            let start = OBJECT_ID_OFFSET;
            bytes[start..start + 4].copy_from_slice(&ordered_i32(*new, order));
            body.extend_from_slice(&bytes);
        }
        write_end_of_elements(&mut body, order)?;

        let rows: Vec<Vec<i32>> = self
            .rows
            .iter()
            .filter_map(|row| {
                let pairs = self.pairs.get(&row.object_id)?;
                let mut out = vec![*self.ids.get(&row.object_id)?];
                for (key, value) in pairs {
                    out.push(*self.ids.get(key)?);
                    out.push(*self.ids.get(value)?);
                }
                Some(out)
            })
            .collect();
        body.write_i16_value(1, order)?;
        body.write_i32_value(rows.len() as i32, order)?;
        for row in &rows {
            for value in row {
                body.write_i32_value(*value, order)?;
            }
            body.write_i32_value(0, order)?;
        }
        Ok(body)
    }

    /// Write one property atom for the target revision.
    ///
    /// The property classes of the specification are re-serialised with the
    /// version gates of the target revision, and the segment type of a late
    /// load is the one the target revision gives that segment. A property of a
    /// class the reader does not know cannot be parsed, so it keeps its bytes
    /// and only its object id and its class version move.
    ///
    /// The bytes are those of the source file, so they are parsed with the
    /// framing of that revision: reading a JT 9.x atom with the gates of JT
    /// 10.x would misalign the cursor before anything is rewritten.
    ///
    /// The class version cannot be kept. A strict consumer reads it to look the
    /// class up and refuses the whole file when the value names nothing it
    /// knows, so a JT 9.x atom rewritten into a JT 10.x file has to store the
    /// version a JT 10.x file uses — and it has to store it at the width of the
    /// target revision, which [`set_property_class`] explains.
    fn write_property_atom(&self, out: &mut Vec<u8>, atom: &Atom, new: i32) -> Result<()> {
        let order = self.file.header.order;
        let source_major = self.file.header.major;
        let class = property_class(self.version);
        let mut cursor = Cursor::new(&atom.bytes, order, source_major);
        let Some(element) = read_element_envelope(&mut cursor, true)? else {
            return Err(Error::invalid(0, "a property atom has no envelope"));
        };
        let known = class_of(&element.guid) == Some(Class::Property);
        let body = &atom.bytes[element.body_offset..element.body_offset + element.body_len];
        let parsed = if known {
            Some(Property::read(
                &element.guid,
                &mut Cursor::new(body, order, source_major),
            )?)
        } else {
            None
        };
        let new_body = match parsed.as_ref() {
            Some(Property::String(value)) => {
                Some(string_property_body(value, order, self.version, class)?)
            }
            Some(Property::LateLoaded(load)) => Some(late_loaded_property_body(
                load,
                order,
                self.version,
                class,
                &self.kinds,
            )?),
            _ => None,
        };
        let Some(new_body) = new_body else {
            // Nothing to re-serialise: keep the atom as the source wrote it,
            // except for the fields a rewrite at another revision has to
            // correct and that can be reached without parsing the body.
            let mut bytes = atom.bytes.clone();
            let start = OBJECT_ID_OFFSET;
            bytes[start..start + 4].copy_from_slice(&ordered_i32(new, order));
            set_property_class(&mut bytes, order, source_major, self.version, class)?;
            out.extend_from_slice(&bytes);
            return Ok(());
        };
        write_atom(out, order, &element.guid, element.base_type, new, &new_body)
    }
}

/// A node with every reference already renumbered.
struct OutNode {
    /// New object id.
    id: i32,
    /// New ids of the attributes of the node.
    attributes: Vec<i32>,
    /// New ids of the children; `-1` is a reference that resolves to nothing.
    children: Vec<i32>,
    /// New id of the instantiated subtree.
    target: Option<i32>,
}

/// Read the property atoms and the property table of a scene graph body.
///
/// The reader throws away the keys of the table, so the writer has to pick them
/// up itself before anything is renumbered.
fn read_atoms_and_table(
    body: &[u8],
    order: ByteOrder,
    major: u32,
) -> Result<(Vec<Atom>, Vec<Row>)> {
    let mut cursor = Cursor::new(body, order, major);
    // The node and attribute atoms come first and end with a marker.
    while let Some(element) = read_element_envelope(&mut cursor, true)? {
        cursor.seek(element.body_offset + element.body_len)?;
    }
    let mut atoms = Vec::new();
    while let Some(element) = read_element_envelope(&mut cursor, true)? {
        let end = element.body_offset + element.body_len;
        if let Some(atom) = read_property_atom(body, &element, order) {
            atoms.push(atom);
        }
        cursor.seek(end)?;
    }

    let _table_version = cursor.read_i16()?;
    let count = cursor.read_i32()?;
    let mut rows = Vec::new();
    for _ in 0..count {
        let object_id = cursor.read_i32()?;
        let mut pairs = Vec::new();
        loop {
            let key = cursor.read_i32()?;
            if key == 0 {
                break;
            }
            let value = cursor.read_i32()?;
            pairs.push((key, value));
        }
        rows.push(Row { object_id, pairs });
    }
    Ok((atoms, rows))
}

/// Pick up one property atom, with the object id the file gave it.
///
/// The reader only knows the three property classes of the specification and
/// reports no object id for anything else, yet a file may store a property of
/// another class, frame it exactly like a known one — base type and object id
/// included — and refer to it from the table by that id. Reading the id out of
/// the atom keeps such a property renumberable, so its value survives the
/// rewrite instead of taking the row that names it down with it.
fn read_property_atom(body: &[u8], element: &RawElement, order: ByteOrder) -> Option<Atom> {
    let known = class_of(&element.guid).is_some();
    let envelope = if known {
        KNOWN_ENVELOPE
    } else {
        UNKNOWN_ENVELOPE
    };
    let start = element.body_offset.checked_sub(envelope)?;
    let end = element.body_offset + element.body_len;
    let object_id = if known {
        element.object_id
    } else {
        read_i32_at(body, start + OBJECT_ID_OFFSET, order)?
    };
    Some(Atom {
        object_id,
        bytes: body[start..end].to_vec(),
    })
}

/// Read an `i32` of the given byte order out of a byte slice.
fn read_i32_at(bytes: &[u8], at: usize, order: ByteOrder) -> Option<i32> {
    let ok: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(match order {
        ByteOrder::Little => i32::from_le_bytes(ok),
        ByteOrder::Big => i32::from_be_bytes(ok),
    })
}

/// Bytes of a file header: the banner of the source plus the fixed fields.
fn write_banner(
    out: &mut Vec<u8>,
    data: &[u8],
    header: &FileHeader,
    toc_offset: usize,
) -> Result<()> {
    let order = header.order;
    if data.len() < 80 {
        return Err(Error::invalid(0, "the source file is too short"));
    }
    out.extend_from_slice(&data[..80]);
    out.write_u8_value(if order == ByteOrder::Big { 1 } else { 0 })?;
    out.write_i32_value(0, order)?;
    if header.major >= 10 {
        out.write_u64_value(toc_offset as u64, order)?;
    } else {
        out.write_i32_value(toc_offset as i32, order)?;
    }
    write_guid(out, order, &header.lsg_guid)?;
    Ok(())
}

/// Length of the fixed header of a JT `major` version.
pub(crate) fn header_length(major: u32) -> usize {
    80 + 1 + 4 + if major >= 10 { 8 } else { 4 } + 16
}

/// Write a banner for a file of another revision.
///
/// The banner of the source declares the revision it holds, so it cannot be
/// copied when the target differs — the reader takes the version from the
/// second word of that line. A banner in the shape the writer of this crate
/// produces replaces it, with the target revision named in it.
fn write_generated_banner(
    out: &mut Vec<u8>,
    order: ByteOrder,
    version: JtVersion,
    lsg: &Guid,
    toc_offset: usize,
) -> Result<()> {
    let text = format!(
        "Version {} JT  FerroJT {}",
        version,
        env!("CARGO_PKG_VERSION")
    );
    let mut banner = [b' '; 80];
    let bytes = text.as_bytes();
    if bytes.len() > 78 {
        return Err(Error::invalid(0, "internal: the banner does not fit"));
    }
    banner[..bytes.len()].copy_from_slice(bytes);
    banner[78] = b'\n';
    banner[79] = b'\r';
    out.write_bytes(&banner)?;
    out.write_u8_value(if order == ByteOrder::Big { 1 } else { 0 })?;
    out.write_i32_value(0, order)?;
    if version.offsets_are_32_bit() {
        out.write_i32_value(toc_offset as i32, order)?;
    } else {
        out.write_u64_value(toc_offset as u64, order)?;
    }
    write_guid(out, order, lsg)?;
    if out.len() != header_length(version.major) {
        return Err(Error::invalid(0, "internal: the fixed header is malformed"));
    }
    Ok(())
}

/// The body of a string property at `version`.
///
/// A class version precedes the state and another one follows it from JT 9.x
/// on; JT 8.x stores only the state and the string. Both hold `class`, the
/// version of the class the target revision names — not the one the source
/// stored, which a consumer of another revision does not know.
fn string_property_body(
    property: &StringProperty,
    order: ByteOrder,
    version: JtVersion,
    class: u32,
) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    if version.has_object_versions() {
        write_version(&mut out, order, version.major, class)?;
    }
    out.write_u32_value(PROPERTY_STATE, order)?;
    if version.has_object_versions() {
        write_version(&mut out, order, version.major, class)?;
    }
    write_mb_string(&mut out, order, &property.value)?;
    Ok(out)
}

/// The body of a late loaded property at `version`.
///
/// The segment type is the one the target revision gives that segment, and the
/// object id of the element inside it moves with the element itself. JT 9.x
/// appends a reserved word after the id, JT 10.x does not. Both class versions
/// hold `class`, the version of the class the target revision names.
fn late_loaded_property_body(
    load: &LateLoaded,
    order: ByteOrder,
    version: JtVersion,
    class: u32,
    kinds: &BTreeMap<Guid, i32>,
) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    if version.has_object_versions() {
        write_version(&mut out, order, version.major, class)?;
    }
    out.write_u32_value(PROPERTY_STATE, order)?;
    if version.has_object_versions() {
        write_version(&mut out, order, version.major, class)?;
    }
    write_guid(&mut out, order, &load.guid)?;
    let kind = kinds.get(&load.guid).copied().unwrap_or(load.segment_type);
    out.write_i32_value(kind, order)?;
    if version.has_object_versions() {
        out.write_i32_value(load.object_id, order)?;
        if version.major < 10 {
            out.write_i32_value(0, order)?;
        }
    }
    Ok(out)
}

/// Re-frame the class versions of a property atom that is kept as the source wrote it.
///
/// A property atom body is a class version, the state word, a second class
/// version and the value, and each version field is a byte from JT 10.x on and
/// a word below. The width belongs to the revision the file is **written** at,
/// not to the one it came from: a consumer reads the state word to find the
/// value, so an atom kept at the width of the source makes it read the value
/// two bytes early. That is not harmless — a strict consumer stops reading the
/// property table at the first atom it cannot make sense of, which on a JT 9.x
/// file rewritten at JT 10.x means the shape late loads further down the table
/// are never found and the file loads with no geometry at all.
///
/// The width is found from the atom itself: it is the framing that puts the
/// state word where it belongs. An atom whose state is nowhere to be found is
/// left alone rather than guessed at. Re-framing changes the length of the
/// body, so the length prefix of the atom is rewritten as well.
fn set_property_class(
    atom: &mut Vec<u8>,
    order: ByteOrder,
    source_major: u32,
    version: JtVersion,
    class: u32,
) -> Result<()> {
    if source_major <= 8 {
        // JT 8.x stores no class version at all, so there is nothing to name.
        return Ok(());
    }
    let at = PROPERTY_VERSION_OFFSET;
    let word = if has_state(atom, at + 1, order) {
        false
    } else if has_state(atom, at + 2, order) {
        true
    } else {
        return Ok(());
    };
    // The second version sits right before the value, so the state word names
    // its offset as well.
    let second = if word { at + 6 } else { at + 5 };
    let Some(value) = atom
        .get(second + if word { 2 } else { 1 }..)
        .map(<[u8]>::to_vec)
    else {
        return Ok(());
    };
    // A JT 8.x target stores no class version, but it can never reach here: a
    // family change is refused before anything is written.
    let mut body: Vec<u8> = Vec::new();
    write_version(&mut body, order, version.major, class)?;
    body.write_u32_value(PROPERTY_STATE, order)?;
    write_version(&mut body, order, version.major, class)?;
    atom.truncate(at);
    atom.extend_from_slice(&body);
    atom.extend_from_slice(&value);
    let length = i32::try_from(atom.len() - 4)
        .map_err(|_| Error::invalid(0, "a property atom is too large"))?;
    atom[..4].copy_from_slice(&ordered_i32(length, order));
    Ok(())
}

/// Does the atom hold the property state word at `at`?
fn has_state(atom: &[u8], at: usize, order: ByteOrder) -> bool {
    let Ok(ok) = atom.get(at..at + 4).unwrap_or(&[]).try_into() else {
        return false;
    };
    let value = if order == ByteOrder::Big {
        u32::from_be_bytes(ok)
    } else {
        u32::from_le_bytes(ok)
    };
    value == PROPERTY_STATE
}

/// Write the TOC table at the end of the file.
fn write_toc(out: &mut Vec<u8>, toc: &[TocEntry], order: ByteOrder, major: u32) -> Result<()> {
    out.write_i32_value(toc.len() as i32, order)?;
    for entry in toc {
        write_guid(out, order, &entry.guid)?;
        if major >= 10 {
            out.write_u64_value(entry.offset, order)?;
        } else {
            out.write_i32_value(entry.offset as i32, order)?;
        }
        out.write_u32_value(entry.length, order)?;
        out.write_u32_value((entry.kind as u32) << 24, order)?;
    }
    Ok(())
}

/// Frame a payload as a whole segment, envelope included.
pub(crate) fn segment_bytes(
    order: ByteOrder,
    guid: &Guid,
    kind: i32,
    payload: &[u8],
    compressed: bool,
) -> Result<Vec<u8>> {
    if !has_compression_header(kind) {
        return Err(Error::invalid(
            0,
            "the scene graph segment has no compression header to write",
        ));
    }
    let mut out: Vec<u8> = Vec::with_capacity(COMPRESSION_ENVELOPE + payload.len());
    write_guid(&mut out, order, guid)?;
    out.write_i32_value(kind, order)?;
    out.write_i32_value((COMPRESSION_ENVELOPE + payload.len()) as i32, order)?;
    // Flag 2 with algorithm 2 is the DEFLATE the reader inflates; anything
    // else means the payload runs to the end of the segment. XZ (flag 3) is
    // never written, even for a source file that used it.
    out.write_u32_value(u32::from(compressed) * 2, order)?;
    // The reference counts the compressed stream together with the algorithm
    // byte that precedes it, and the reader subtracts one again.
    out.write_i32_value((payload.len() + 1) as i32, order)?;
    out.write_u8_value(u8::from(compressed) * 2)?;
    out.write_bytes(payload)?;
    Ok(out)
}

/// The four bytes of an `i32` in `order`.
fn ordered_i32(value: i32, order: ByteOrder) -> [u8; 4] {
    if order == ByteOrder::Big {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    }
}

/// Write one element atom.
pub(crate) fn write_atom(
    out: &mut Vec<u8>,
    order: ByteOrder,
    guid: &Guid,
    base: u8,
    object_id: i32,
    body: &[u8],
) -> Result<()> {
    out.write_i32_value((16 + 1 + 4 + body.len()) as i32, order)?;
    write_guid(out, order, guid)?;
    out.write_u8_value(base)?;
    out.write_i32_value(object_id, order)?;
    out.write_bytes(body)?;
    Ok(())
}

/// Write the marker that ends a run of atoms.
pub(crate) fn write_end_of_elements(out: &mut Vec<u8>, order: ByteOrder) -> Result<()> {
    out.write_i32_value(16, order)?;
    out.write_bytes(&[0xffu8; 16])?;
    Ok(())
}

/// Write a GUID as three numeric fields plus the tail, as the reader reads it.
pub(crate) fn write_guid(out: &mut Vec<u8>, order: ByteOrder, guid: &Guid) -> Result<()> {
    let bytes = guid.bytes();
    out.write_u32_value(
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        order,
    )?;
    out.write_u16_value(u16::from_be_bytes([bytes[4], bytes[5]]), order)?;
    out.write_u16_value(u16::from_be_bytes([bytes[6], bytes[7]]), order)?;
    out.write_bytes(&bytes[8..])?;
    Ok(())
}

/// Write a class version: one byte from JT 10.x on, two before that.
pub(crate) fn write_version(
    out: &mut Vec<u8>,
    order: ByteOrder,
    major: u32,
    version: u32,
) -> Result<()> {
    if major >= 10 {
        out.write_u8_value(version as u8)?;
    } else {
        out.write_i16_value(version as i16, order)?;
    }
    Ok(())
}

/// Write a UTF-16 string: a count of code units, then the units.
pub(crate) fn write_mb_string(out: &mut Vec<u8>, order: ByteOrder, value: &str) -> Result<()> {
    let units: Vec<u16> = value.encode_utf16().collect();
    out.write_i32_value(units.len() as i32, order)?;
    for unit in units {
        out.write_u16_value(unit, order)?;
    }
    Ok(())
}

/// Write a vector of object references.
pub(crate) fn write_deferred(out: &mut Vec<u8>, order: ByteOrder, ids: &[i32]) -> Result<()> {
    out.write_i32_value(ids.len() as i32, order)?;
    for id in ids {
        out.write_i32_value(*id, order)?;
    }
    Ok(())
}

/// Write a vector of floats.
fn write_floats(out: &mut Vec<u8>, order: ByteOrder, values: &[f32]) -> Result<()> {
    out.write_i32_value(values.len() as i32, order)?;
    for value in values {
        out.write_f32_value(*value, order)?;
    }
    Ok(())
}

/// The class GUID a node is written with.
fn node_guid(node: &Node) -> Guid {
    match node {
        Node::Partition(_) => types::NODE_PARTITION,
        Node::Part(_) => types::NODE_PART,
        Node::MetaData(_) => types::NODE_META_DATA,
        Node::Lod(_) => types::NODE_LOD,
        Node::RangeLod(_) => types::NODE_RANGE_LOD,
        Node::Instance(_) => types::NODE_INSTANCE,
        Node::Group(_) => types::NODE_GROUP,
        Node::Shape(_) => types::NODE_SHAPE_BASE,
        Node::ShapeVertex(_) => types::NODE_SHAPE_VERTEX,
        Node::ShapeTriStripSet(_) => types::NODE_SHAPE_TRI_STRIP_SET,
        Node::Unsupported { .. } => types::NODE_BASE,
    }
}

/// The base type byte a node atom carries.
fn node_base_type(node: &Node) -> u8 {
    match node {
        Node::Instance(_) => BASE_INSTANCE,
        Node::Shape(_) | Node::ShapeVertex(_) | Node::ShapeTriStripSet(_) => BASE_SHAPE,
        _ => BASE_GROUP,
    }
}

/// The class GUID an attribute is written with.
fn attribute_guid(attribute: &Attribute) -> Result<Guid> {
    Ok(match attribute {
        Attribute::GeometricTransform(_) => types::ATTRIBUTE_GEOMETRIC_TRANSFORM,
        Attribute::Material(_) => types::ATTRIBUTE_MATERIAL,
        Attribute::Unsupported { class_name } => {
            return Err(Error::invalid(
                0,
                format!("the attribute class {class_name} cannot be written"),
            ));
        }
    })
}

/// The shape fields shared by the three shape classes.
fn shape_of(node: &Node) -> Option<&Shape> {
    match node {
        Node::Shape(shape) => Some(shape),
        Node::ShapeVertex(shape) => Some(&shape.base),
        Node::ShapeTriStripSet(shape) => Some(&shape.base),
        _ => None,
    }
}

/// Serialise the body of one node, with its references already renumbered.
fn node_body(node: &Node, out_node: &OutNode, order: ByteOrder, major: u32) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    match node {
        Node::Group(group) => {
            write_group(&mut out, &group.base, out_node, order, major)?;
        }
        Node::Partition(partition) => {
            write_group(&mut out, &partition.base.base, out_node, order, major)?;
            if major >= 10 {
                write_version(&mut out, order, major, 1)?;
            }
            out.write_i32_value(partition.flags, order)?;
            write_mb_string(&mut out, order, &partition.file_name)?;
            write_box(&mut out, order, &partition.bounds)?;
            out.write_f32_value(partition.area, order)?;
            write_counts(&mut out, order, &partition.counts)?;
            // Bit 0 of the flags announces a second box, which JT 8.x and 9.x
            // store here and JT 10.x drops.
            if let Some(untransformed) = partition.untransformed_bounds {
                if major < 10 {
                    write_box(&mut out, order, &untransformed)?;
                }
            }
        }
        Node::MetaData(meta) => {
            write_group(&mut out, &meta.base.base, out_node, order, major)?;
            write_version(&mut out, order, major, 1)?;
        }
        Node::Part(part) => {
            write_group(&mut out, &part.base.base.base, out_node, order, major)?;
            write_version(&mut out, order, major, 1)?;
            write_version(&mut out, order, major, 1)?;
            out.write_i32_value(0, order)?;
        }
        Node::Lod(lod) => {
            write_group(&mut out, &lod.base.base, out_node, order, major)?;
            if major > 8 {
                write_version(&mut out, order, major, 1)?;
            }
            if major < 10 {
                write_floats(&mut out, order, &lod.limits)?;
                out.write_f32_value(lod.signal, order)?;
            }
        }
        Node::RangeLod(range) => {
            write_group(&mut out, &range.base.base.base, out_node, order, major)?;
            if major > 8 {
                write_version(&mut out, order, major, 1)?;
            }
            if major < 10 {
                write_floats(&mut out, order, &range.base.limits)?;
                out.write_f32_value(range.base.signal, order)?;
            }
            if major > 8 {
                write_version(&mut out, order, major, 1)?;
            }
            write_floats(&mut out, order, &range.limits)?;
            for value in range.center {
                out.write_f32_value(value, order)?;
            }
        }
        Node::Instance(instance) => {
            write_node_base(&mut out, &instance.base, out_node, order, major)?;
            if major > 8 {
                write_version(&mut out, order, major, instance.base.version)?;
            }
            // A single reference, not a length-prefixed vector.
            out.write_i32_value(out_node.target.unwrap_or(-1), order)?;
        }
        Node::Shape(_) | Node::ShapeVertex(_) | Node::ShapeTriStripSet(_) => {
            let shape = shape_of(node)
                .ok_or_else(|| Error::invalid(0, "internal: a shape node lost its fields"))?;
            write_node_base(&mut out, &shape.base, out_node, order, major)?;
            if major > 8 {
                write_version(&mut out, order, major, shape.base.version)?;
            }
            if major >= 10 {
                write_box(&mut out, order, &shape.bounds)?;
            } else {
                // JT 8.x and 9.x store a reserved box first and the
                // untransformed one second; both get the box that is kept.
                write_box(&mut out, order, &shape.bounds)?;
                write_box(&mut out, order, &shape.bounds)?;
            }
            out.write_f32_value(shape.area, order)?;
            write_counts(&mut out, order, &shape.counts)?;
            out.write_i32_value(shape.size, order)?;
            out.write_f32_value(shape.compression, order)?;
            write_shape_lod(&mut out, order, &shape.lod, major)?;
        }
        Node::Unsupported { class_name } => {
            return Err(Error::invalid(
                0,
                format!("the node class {class_name} cannot be written"),
            ));
        }
    }
    Ok(out)
}

/// Write the fields every node with a node base starts with.
fn write_node_base(
    out: &mut Vec<u8>,
    base: &NodeBase,
    node: &OutNode,
    order: ByteOrder,
    major: u32,
) -> Result<()> {
    if major > 8 {
        write_version(out, order, major, base.version)?;
    }
    out.write_u32_value(base.flags, order)?;
    write_deferred(out, order, &node.attributes)?;
    Ok(())
}

/// Write a group: its node base, its version and its children.
fn write_group(
    out: &mut Vec<u8>,
    base: &NodeBase,
    node: &OutNode,
    order: ByteOrder,
    major: u32,
) -> Result<()> {
    write_node_base(out, base, node, order, major)?;
    if major > 8 {
        write_version(out, order, major, 1)?;
    }
    write_deferred(out, order, &node.children)?;
    Ok(())
}

/// Write the three `I32[2]` count arrays of a shape or partition.
pub(crate) fn write_counts(out: &mut Vec<u8>, order: ByteOrder, counts: &[i32; 6]) -> Result<()> {
    for count in counts {
        out.write_i32_value(*count, order)?;
    }
    Ok(())
}

/// Write the shape description tail of a shape node, which follows the
/// compression ratio and holds the quantization of the vertex data.
///
/// The layout is picked by the revision being **written**, not by the one the
/// node was read at: a JT 10.x node has a one byte version and one binding, a
/// JT 9.x node a two byte version, one binding, the quantization parameters
/// and — from version 2 on — a second binding, and a JT 8.x node three four
/// byte bindings followed by the quantization parameters. Re-emitting the
/// layout of the source instead shifts every byte after the tail — the nodes
/// that follow, the end of the element list and the property table — and a
/// strict consumer then loses the meta data, the layer filters, the PMI and
/// the B-rep of the file: a 9.x node read at 22 bytes and written into a JT
/// 10.x file moves them by 13 bytes each.
///
/// Within one revision the bindings and the quantization parameters are
/// written back exactly as they were read: a coarse level of detail of a
/// sample quantizes with `11 06 0b 06`, so zeros are not a stand-in for what
/// the file says. Only a revision change re-frames them, and a family change
/// (JT 8.x against JT 9.x or 10.x) is refused before this is reached, so the
/// branches that carry a binding across that boundary are a fallback rather
/// than a supported path.
fn write_shape_lod(out: &mut Vec<u8>, order: ByteOrder, lod: &ShapeLod, major: u32) -> Result<()> {
    if major >= 10 {
        // One byte version and one 64 bit binding, no quantization parameters.
        match lod {
            ShapeLod::Binding { version, binding } => {
                out.write_u8_value(*version)?;
                out.write_bytes(binding)?;
            }
            ShapeLod::Vertex2 { binding, .. } => {
                out.write_u8_value(1)?;
                out.write_bytes(binding)?;
            }
            ShapeLod::Vertex1 { .. } | ShapeLod::None => {
                out.write_u8_value(1)?;
                crate::jt::encode::write_binding(&mut *out);
            }
        }
        return Ok(());
    }
    if major > 8 {
        match lod {
            ShapeLod::Vertex2 {
                version,
                binding,
                quantization,
                second_binding,
            } => {
                out.write_i16_value(*version as i16, order)?;
                out.write_bytes(binding)?;
                out.write_bytes(quantization)?;
                if let Some(second) = second_binding {
                    out.write_bytes(second)?;
                }
            }
            ShapeLod::Binding { binding, .. } => {
                out.write_i16_value(2, order)?;
                out.write_bytes(binding)?;
                crate::jt::encode::write_quantization_params(&mut *out);
                out.write_bytes(binding)?;
            }
            ShapeLod::Vertex1 { .. } | ShapeLod::None => {
                out.write_i16_value(2, order)?;
                crate::jt::encode::write_binding(&mut *out);
                crate::jt::encode::write_quantization_params(&mut *out);
                crate::jt::encode::write_binding(&mut *out);
            }
        }
        return Ok(());
    }
    match lod {
        ShapeLod::Vertex1 {
            normal_binding,
            texture_binding,
            color_binding,
            quantization,
        } => {
            out.write_i32_value(*normal_binding, order)?;
            out.write_i32_value(*texture_binding, order)?;
            out.write_i32_value(*color_binding, order)?;
            out.write_bytes(quantization)?;
        }
        // Nothing was read, or the node comes from a later revision: write what
        // a JT 8.x writer emits for an unquantized shape.
        _ => {
            out.write_i32_value(1, order)?;
            out.write_i32_value(0, order)?;
            out.write_i32_value(0, order)?;
            crate::jt::encode::write_quantization_params(&mut *out);
        }
    }
    Ok(())
}

/// Write an axis aligned box.
pub(crate) fn write_box(out: &mut Vec<u8>, order: ByteOrder, values: &[f32; 6]) -> Result<()> {
    for value in values {
        out.write_f32_value(*value, order)?;
    }
    Ok(())
}

/// Serialise the body of an attribute.
fn attribute_body(attribute: &Attribute, order: ByteOrder, major: u32) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    match attribute {
        Attribute::GeometricTransform(transform) => {
            write_attribute_base(&mut out, order, major, transform.version)?;
            if major > 8 {
                write_version(&mut out, order, major, transform.version)?;
            }
            // Every slot is stored, so the mask says so.
            out.write_u16_value(0xffff, order)?;
            for value in &transform.matrix {
                if major > 8 {
                    out.write_f64_value(*value, order)?;
                } else {
                    out.write_f32_value(*value as f32, order)?;
                }
            }
        }
        Attribute::Material(material) => {
            write_attribute_base(&mut out, order, major, material.version)?;
            if major > 8 {
                write_version(&mut out, order, major, material.version)?;
            }
            // Flag 0 stores every colour as four components, which is also
            // what the reader hands back for a single grey value.
            out.write_u16_value(0, order)?;
            for colour in [
                material.ambient,
                material.diffuse,
                material.specular,
                material.emission,
            ] {
                for component in colour {
                    out.write_f32_value(component, order)?;
                }
            }
            out.write_f32_value(material.shininess, order)?;
            if major >= 10 {
                out.write_f32_value(material.reflectivity, order)?;
                out.write_f32_value(0.0, order)?;
                if material.version >= 2 {
                    out.write_u32_value(0, order)?;
                }
            } else if material.version == 2 {
                out.write_f32_value(material.reflectivity, order)?;
            }
        }
        Attribute::Unsupported { class_name } => {
            return Err(Error::invalid(
                0,
                format!("the attribute class {class_name} cannot be written"),
            ));
        }
    }
    Ok(out)
}

/// Write the fields every attribute starts with.
pub(crate) fn write_attribute_base(
    out: &mut Vec<u8>,
    order: ByteOrder,
    major: u32,
    version: u32,
) -> Result<()> {
    if major > 8 {
        write_version(out, order, major, version)?;
    }
    out.write_u8_value(0)?;
    out.write_u32_value(0, order)?;
    if major >= 10 {
        out.write_u32_value(0, order)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An atom of `word` framing: the class version is a byte below JT 10.x and
    /// a word from there on, and a file can hold one framed the other way.
    fn atom(word: bool, version: u32) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let head = if word {
            let mut head = Vec::new();
            head.write_u16_value(version as u16, ByteOrder::Little)
                .unwrap();
            head.write_u32_value(PROPERTY_STATE, ByteOrder::Little)
                .unwrap();
            head.write_u16_value(version as u16, ByteOrder::Little)
                .unwrap();
            head
        } else {
            let mut head = Vec::new();
            head.write_u8_value(version as u8).unwrap();
            head.write_u32_value(PROPERTY_STATE, ByteOrder::Little)
                .unwrap();
            head.write_u8_value(version as u8).unwrap();
            head
        };
        let payload = b"value";
        out.write_i32_value(
            (16 + 1 + 4 + head.len() + payload.len()) as i32,
            ByteOrder::Little,
        )
        .unwrap();
        out.write_bytes(&[0x10, 0xdd, 0x10, 0x6e, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .unwrap();
        out.write_u8_value(5).unwrap();
        out.write_i32_value(7, ByteOrder::Little).unwrap();
        out.write_bytes(&head).unwrap();
        out.write_bytes(payload).unwrap();
        out
    }

    #[test]
    fn a_byte_framed_atom_gets_a_byte_class_version() {
        let mut bytes = atom(false, 1);
        let before = bytes.len();
        set_property_class(&mut bytes, ByteOrder::Little, 10, JtVersion::V10_3, 2).unwrap();
        assert_eq!(before, bytes.len());
        assert_eq!(2, bytes[PROPERTY_VERSION_OFFSET] as u32);
        // The second field is re-framed too, and it names the same revision.
        assert_eq!(2, bytes[PROPERTY_VERSION_OFFSET + 5] as u32);
        assert_eq!(
            PROPERTY_STATE,
            u32::from_le_bytes(
                bytes[PROPERTY_VERSION_OFFSET + 1..PROPERTY_VERSION_OFFSET + 5]
                    .try_into()
                    .unwrap()
            )
        );
        assert_eq!(b"value", &bytes[PROPERTY_VERSION_OFFSET + 6..]);
    }

    #[test]
    fn a_word_framed_atom_is_re_framed_to_the_revision_being_written() {
        // A JT 9.x atom copied into a JT 10.x file must stop being word framed.
        // A consumer finds the value at the state word, so a word-framed atom
        // in a byte-framed file makes it read two bytes early; it then gives up
        // on the property table, and with it on the shape late loads further
        // down — a rewritten file that loads with no geometry at all. The real
        // JT 10.3 sample stores its one atom of the unknown class `10dd1019`
        // byte framed (length 15) where the JT 9.5 source stores it word
        // framed (length 17), which is the framing this test pins.
        let mut bytes = atom(true, 1);
        set_property_class(&mut bytes, ByteOrder::Little, 9, JtVersion::V10_3, 2).unwrap();
        // The envelope is 25 bytes and the body is now 1 + 4 + 1 + 5.
        assert_eq!(32, i32::from_le_bytes(bytes[..4].try_into().unwrap()));
        assert_eq!(32, bytes.len() - 4);
        assert_eq!(2, bytes[PROPERTY_VERSION_OFFSET] as u32);
        assert_eq!(
            PROPERTY_STATE,
            u32::from_le_bytes(
                bytes[PROPERTY_VERSION_OFFSET + 1..PROPERTY_VERSION_OFFSET + 5]
                    .try_into()
                    .unwrap()
            )
        );
        assert_eq!(2, bytes[PROPERTY_VERSION_OFFSET + 5] as u32);
        assert_eq!(b"value", &bytes[PROPERTY_VERSION_OFFSET + 6..]);
    }

    #[test]
    fn a_byte_framed_atom_becomes_word_framed_at_jt_9x() {
        // The same rule in the other direction: a JT 10.x atom written into a
        // JT 9.x file gains the wider field, and its length prefix grows.
        let mut bytes = atom(false, 1);
        set_property_class(&mut bytes, ByteOrder::Little, 10, JtVersion::V9_5, 1).unwrap();
        // The envelope is 25 bytes and the body is now 2 + 4 + 2 + 5.
        assert_eq!(34, i32::from_le_bytes(bytes[..4].try_into().unwrap()));
        assert_eq!(34, bytes.len() - 4);
        assert_eq!(
            1,
            u16::from_le_bytes(
                bytes[PROPERTY_VERSION_OFFSET..PROPERTY_VERSION_OFFSET + 2]
                    .try_into()
                    .unwrap()
            ) as u32
        );
        assert_eq!(
            PROPERTY_STATE,
            u32::from_le_bytes(
                bytes[PROPERTY_VERSION_OFFSET + 2..PROPERTY_VERSION_OFFSET + 6]
                    .try_into()
                    .unwrap()
            )
        );
        assert_eq!(b"value", &bytes[PROPERTY_VERSION_OFFSET + 8..]);
    }

    #[test]
    fn a_jt_8x_atom_stores_no_class_version_at_all() {
        let mut bytes = atom(false, 1);
        let before = bytes.clone();
        set_property_class(&mut bytes, ByteOrder::Little, 8, JtVersion::V9_5, 2).unwrap();
        assert_eq!(before, bytes);
    }

    #[test]
    fn an_atom_without_a_state_word_is_left_alone() {
        let mut bytes = atom(false, 1);
        bytes[PROPERTY_VERSION_OFFSET + 1..PROPERTY_VERSION_OFFSET + 5]
            .copy_from_slice(&0u32.to_le_bytes());
        let before = bytes.clone();
        set_property_class(&mut bytes, ByteOrder::Little, 10, JtVersion::V10_3, 2).unwrap();
        assert_eq!(before, bytes);
    }

    #[test]
    fn a_shape_tail_is_re_framed_for_the_revision_being_written() {
        // The tail of a `Shape` node is framed by the revision the file is
        // written at, not by the one the node was read at. A JT 9.x node reads
        // as a two byte version, one binding and the quantization parameters —
        // 22 bytes — while a JT 10.x node is a one byte version and one binding,
        // 9 bytes. Keeping the source framing moves every byte behind it, so a
        // consumer loses the meta data, the layer filters, the PMI and the B-rep
        // of a rewritten file and renders nothing.
        let order = ByteOrder::Little;
        let source = ShapeLod::Vertex2 {
            version: 2,
            binding: [0x21, 0, 0, 0, 0, 0, 0, 0],
            quantization: [0x11, 0x06, 0x0b, 0x06],
            second_binding: Some([0x21, 0, 0, 0, 0, 0, 0, 0]),
        };
        let mut at_9_5 = Vec::new();
        write_shape_lod(&mut at_9_5, order, &source, 9).expect("writing cannot fail");
        assert_eq!(at_9_5.len(), 22, "a JT 9.x tail keeps its own framing");
        assert_eq!(at_9_5[10..14], [0x11, 0x06, 0x0b, 0x06], "quantization");

        let mut at_10_3 = Vec::new();
        write_shape_lod(&mut at_10_3, order, &source, 10).expect("writing cannot fail");
        assert_eq!(at_10_3.len(), 9, "a JT 9.x node written into JT 10.x");
        assert_eq!(at_10_3[0] as u32, 1, "one byte version");
        assert_eq!(
            at_10_3[1..9],
            source_binding(&source),
            "the binding survives"
        );

        let mut at_8_1 = Vec::new();
        write_shape_lod(&mut at_8_1, order, &source, 8).expect("writing cannot fail");
        assert_eq!(
            at_8_1.len(),
            16,
            "a JT 8.x tail is three bindings plus quantization"
        );
    }

    /// The 64-bit vertex binding stored in a shape tail, for the comparison
    /// above.
    fn source_binding(lod: &ShapeLod) -> [u8; 8] {
        match lod {
            ShapeLod::Vertex2 { binding, .. } => *binding,
            ShapeLod::Binding { binding, .. } => *binding,
            ShapeLod::Vertex1 { .. } | ShapeLod::None => [0; 8],
        }
    }
}
