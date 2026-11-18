//! Report files (SPEC 12, 14): one immutable file per filesystem scope.
//!
//! Layout: `SRREP01\n`, a little-endian u32 header length, the JSON
//! header, a u64 node count, then one record per node in preorder, then
//! `SRREND1\n` and an FNV-1a 64 checksum of everything before it.
//! A record is: varint depth, kind letter, flags byte, varint name
//! length, name bytes, then varints logical, physical, extents, inline,
//! entries, files, reclaimable, unknown, pinned, nlink, tree, objectid,
//! then a blockers byte; a directory then has one varint per holder
//! class of its retained bytes, a varint marker code (the package marker
//! that made it package data, see `dsm::marker_code`) and a byte that is
//! 1 when its values without the package data below it follow: nine
//! metric varints as above and one varint per holder class. Depth-0
//! names are full paths.

use crate::btrfs::source::Maintenance;
use crate::btrfs::source::SpaceWindow;
use crate::codec::{self, Decoder, Fnv, HashingWriter};
use crate::model::accounting::AccountingSummary;
use crate::model::accounting::{Blockers, Held};
use crate::model::namespace::FileKind;
use crate::model::namespace::RecycleEntryCtime;
use crate::model::tree::{DirHeld, Excluded, Metrics, NO_INODE, NodeFlags, Outside, ScopeTree};
use crate::snapshots::SnapshotAnalysis;
use crate::target::Scope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::ops::ControlFlow;

pub const FORMAT: u32 = 5;
const MAGIC: &[u8; 8] = b"SRREP01\n";
const END: &[u8; 8] = b"SRREND1\n";
/// Larger headers are rejected as corrupt.
const MAX_HEADER: u32 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub filesystem: crate::filesystem::FilesystemKind,
    pub capabilities: crate::filesystem::Capabilities,
    pub format: u32,
    pub tool_version: String,
    pub fsid: String,
    #[serde(with = "hex_bytes")]
    pub mount: Vec<u8>,
    pub job_id: String,
    /// Unix seconds.
    pub scan_started: u64,
    pub scan_finished: u64,
    pub profile: String,
    pub roots: Vec<RootInfo>,
    pub consistency: Consistency,
    /// Why `consistency` is `changed-during-scan`.
    #[serde(default)]
    pub change_reasons: Vec<ChangeReason>,
    /// Generation changes left out of `change_reasons`: only the state
    /// directory's own writes made them (SPEC 22.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_dir_writes: Vec<StateDirWrites>,
    pub subvolumes: Vec<SubvolumeState>,
    pub coverage: Coverage,
    pub totals: Metrics,
    /// Extent verdicts and retained bytes per blocker class.
    #[serde(default)]
    pub accounting: AccountingSummary,
    pub stats: ScanStats,
    /// What a `--snapshots` scan found (SPEC 5.14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<SnapshotAnalysis>,
    /// Read-only filesystem-space samples around a volume scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceWindow>,
    /// Extent-tree attribution for volume overviews (SPEC 5.13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview: Option<crate::overview::OverviewData>,
    /// Additive format-4 ctime index for paths inside Recycle Bins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recycle_ctimes: Vec<RecycleEntryCtime>,
    /// What deleting snapshot roots with the selection frees (SPEC 5.14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion: Option<crate::snapshots::DeletionAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_space: Option<crate::btrfs::source::FsSpace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_overview: Option<crate::model::tree::AllocationOverview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootInfo {
    #[serde(with = "hex_bytes")]
    pub path: Vec<u8>,
    pub scope: Scope,
    /// Btrfs tree id or ext4 st_dev, interpreted by Header.filesystem.
    #[serde(rename = "namespace_id")]
    pub tree: u64,
    pub ino: u64,
}

/// SPEC 13: no detected change is not proof of an atomic view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Consistency {
    ObservedStable,
    ChangedDuringScan,
    Partial,
}

impl Consistency {
    pub fn name(self) -> &'static str {
        match self {
            Consistency::ObservedStable => "observed-stable",
            Consistency::ChangedDuringScan => "changed-during-scan",
            Consistency::Partial => "partial",
        }
    }
}

/// A subvolume whose generation moved during the scan only because of
/// writes inside the tool's own state directory (SPEC 22.4): every
/// inode it changed lies there, so it is no change reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDirWrites {
    pub subvolume_id: u64,
    pub generation_before: u64,
    pub generation_after: u64,
    /// Inodes changed, all inside the state directory.
    pub inodes: u64,
}

/// One change a scan saw that makes it `changed-during-scan` (SPEC 13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ChangeReason {
    /// Its generation moved between the first and the final discovery.
    /// `selected` when its namespace is in the report.
    SubvolumeGenerationChanged {
        subvolume_id: u64,
        selected: bool,
        generation_before: u64,
        generation_after: u64,
    },
    /// A subvolume the first discovery did not see; `snapshot_of` when
    /// its `parent_uuid` names a known subvolume.
    SubvolumeAppeared {
        subvolume_id: u64,
        generation: u64,
        /// Reasons written before this field read as having no uuid.
        #[serde(default)]
        has_uuid: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_of: Option<u64>,
    },
    /// A selected subvolume the final discovery did not see.
    SubvolumeVanished {
        subvolume_id: u64,
        generation_before: u64,
    },
    /// A selected subvolume's `ROOT_ITEM.last_snapshot` moved: it was
    /// snapshotted, so a new root shares its tree.
    SubvolumeSnapshotted {
        subvolume_id: u64,
        last_snapshot_before: u64,
        last_snapshot_after: u64,
    },
    /// A selected subvolume's uuid changed: another subvolume now has its
    /// id.
    SubvolumeUuidChanged { subvolume_id: u64 },
    /// A probe before or after the scan saw the operation running.
    MaintenanceRunning { operation: Maintenance },
    /// A probe failed, so the operation cannot be ruled out; `errno`
    /// when the kernel gave one.
    MaintenanceProbeFailed {
        operation: Maintenance,
        errno: Option<i32>,
    },
    /// A relocation tree copied the subvolume: a balance was moving its
    /// extents.
    RelocationTree { subvolume_id: u64 },
}

impl std::fmt::Display for ChangeReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            ChangeReason::SubvolumeGenerationChanged {
                subvolume_id,
                selected: true,
                generation_before,
                generation_after,
            } => write!(
                f,
                "selected subvolume {subvolume_id} generation {generation_before} -> {generation_after}"
            ),
            ChangeReason::SubvolumeGenerationChanged {
                subvolume_id,
                selected: false,
                generation_before,
                generation_after,
            } => write!(
                f,
                "subvolume {subvolume_id} outside the selection generation {generation_before} -> {generation_after}"
            ),
            ChangeReason::SubvolumeAppeared {
                subvolume_id,
                has_uuid,
                snapshot_of,
                ..
            } => {
                write!(f, "subvolume {subvolume_id} appeared")?;
                match (has_uuid, snapshot_of) {
                    (false, _) => write!(f, " without a uuid"),
                    (true, Some(source)) => write!(f, " as a snapshot of {source}"),
                    (true, None) => Ok(()),
                }
            }
            ChangeReason::SubvolumeSnapshotted {
                subvolume_id,
                last_snapshot_before,
                last_snapshot_after,
            } => write!(
                f,
                "selected subvolume {subvolume_id} snapshotted (last snapshot {last_snapshot_before} -> {last_snapshot_after})"
            ),
            ChangeReason::SubvolumeUuidChanged { subvolume_id } => {
                write!(f, "selected subvolume {subvolume_id} uuid changed")
            }
            ChangeReason::SubvolumeVanished { subvolume_id, .. } => {
                write!(f, "selected subvolume {subvolume_id} vanished")
            }
            ChangeReason::MaintenanceRunning { operation } => {
                write!(f, "{} running", operation.name())
            }
            ChangeReason::MaintenanceProbeFailed { operation, errno } => {
                write!(f, "{} probe failed", operation.name())?;
                match errno {
                    Some(errno) => write!(f, " (errno {errno})"),
                    None => Ok(()),
                }
            }
            ChangeReason::RelocationTree { subvolume_id } => {
                write!(f, "subvolume {subvolume_id} being relocated")
            }
        }
    }
}

/// A consistency state followed by the reasons for it, if any.
pub fn consistency_text(consistency: Consistency, reasons: &[ChangeReason]) -> String {
    if reasons.is_empty() {
        return consistency.name().to_string();
    }
    let reasons: Vec<String> = reasons.iter().map(ToString::to_string).collect();
    format!("{} ({})", consistency.name(), reasons.join("; "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubvolumeRole {
    /// Its namespace was read and materialized.
    Scanned,
    /// A read-only snapshot below a volume root, left for reference
    /// accounting (Phase 2); its bytes are not in the totals.
    SnapshotSkipped,
    /// Its namespace came from a unit sealed by an earlier scan.
    Reused,
}

impl SubvolumeRole {
    /// Whether its namespace is in the report.
    pub fn is_materialized(self) -> bool {
        matches!(self, SubvolumeRole::Scanned | SubvolumeRole::Reused)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubvolumeState {
    pub id: u64,
    pub role: SubvolumeRole,
    pub generation_before: u64,
    /// `None` when the subvolume vanished during the scan.
    pub generation_after: Option<u64>,
    /// Unix seconds when a reused unit's namespace was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured: Option<u64>,
    /// Hex `ROOT_ITEM.uuid` at the final discovery, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// `ROOT_ITEM.last_snapshot` at the final discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_snapshot: Option<u64>,
    /// Subvolumes whose `parent_uuid` names this one at the final
    /// discovery: its snapshots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<u64>,
}

/// When the namespaces a report combines were first read: the run's
/// start, or earlier when it reused units (SPEC 22.2: a report built
/// from reused units is not one continuous view).
pub fn capture_started(run_started: u64, subvolumes: &[SubvolumeState]) -> u64 {
    subvolumes
        .iter()
        .filter_map(|s| s.captured)
        .fold(run_started, u64::min)
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// What the report does not cover (SPEC 22.6): unknown is not zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// A completed best-effort read. Display evidence only; never cleanup authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approximation: Option<Approximation>,
    /// eCryptfs lower directories: bytes counted, names hidden.
    pub encrypted_directories: u64,
    /// Subvolumes with namespace gaps, encrypted entries or failed roots.
    /// None on older reports: absence is not proof of completeness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_uncertain_subvolumes: Option<Vec<u64>>,
    pub snapshots_not_materialized: u64,
    /// Snapshots whose file extents a `--snapshots` scan read.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub snapshots_materialized: u64,
    /// Subvolume boundaries that are not linked here (no namespace).
    pub unlinked_subvolume_entries: u64,
    pub dangling_entries: u64,
    pub cycles: u64,
    pub unsupported_locations: u64,
    pub missing_subvolume_roots: u64,
    /// Requested roots that could not be read. None on older reports:
    /// absence is not proof that every root was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable_roots: Option<u64>,
    /// `complete`, `partial` (some extents unresolved) or `unavailable`.
    pub reference_accounting: String,
    /// Extents whose references could not be resolved (SPEC 8.1).
    #[serde(default)]
    pub unresolved_extents: u64,
}

/// Coverage lost to live changes or bounded reference resolution. Byte counts
/// are known unaccounted bytes, not an upper bound on every skipped namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approximation {
    pub changed_subvolumes: u64,
    pub skipped_roots: u64,
    pub unaccounted_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStats {
    pub subvolumes_discovered: u64,
    pub subvolumes_scanned: u64,
    pub subvolumes_reused: u64,
    pub entries: u64,
    pub inodes: u64,
    pub extents: u64,
    pub items: u64,
    pub batches: u64,
    /// Payload bytes of the metadata items read.
    #[serde(default)]
    pub item_bytes: u64,
    /// Extent-tree range searches and the items they returned.
    #[serde(default)]
    pub reference_searches: u64,
    #[serde(default)]
    pub reference_items: u64,
    /// Tree-block lookups made by the volume overview owner walk.
    #[serde(default)]
    pub owner_lookups: u64,
    /// Extents whose overview owner walk reached `--owner-budget`.
    #[serde(default)]
    pub owner_budget_capped_extents: u64,
    /// Leaves read by address to measure overview pinned bytes, the
    /// bytes read, and reads a remembered leaf saved (SPEC 5.13).
    #[serde(default)]
    pub pinned_leaf_reads: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_read_plan: Option<crate::btrfs::read_budget::ReadBudget>,
    #[serde(default)]
    pub pinned_leaf_read_bytes: u64,
    #[serde(default)]
    pub pinned_leaf_cache_hits: u64,
    /// Root searches for the direct references of unscanned roots.
    #[serde(default)]
    pub pinned_root_searches: u64,
    /// Why those leaves could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_leaf_reader_unavailable: Option<String>,
    pub elapsed_ms: u64,
    pub peak_rss_bytes: Option<u64>,
}

/// Lowercase hex, lossless for non-UTF-8 paths.
pub mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        if text.len() % 2 != 0 {
            return None;
        }
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
            .collect()
    }

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        decode(&text).ok_or_else(|| serde::de::Error::custom("invalid hex"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("not a synoreclaim report")]
    Magic,
    #[error("report header: {0}")]
    Header(String),
    #[error("report format {0} is not supported (expected {FORMAT})")]
    Format(u32),
    #[error("report is truncated or corrupt")]
    Corrupt,
    #[error("report checksum mismatch")]
    Checksum,
}

/// Per-node totals of a scope tree, as an accountant aggregates them.
#[derive(Debug, Clone, Copy)]
pub struct NodeTotals<'a> {
    pub metrics: &'a [Metrics],
    pub blockers: &'a [Blockers],
    pub held: &'a DirHeld,
    /// What each directory above package data includes of it.
    pub excluded: &'a HashMap<u32, Excluded>,
}

/// Writes `tree` with its per-node `totals`. Returns the bytes written.
pub fn write<W: Write>(
    out: W,
    header: &Header,
    tree: &ScopeTree,
    totals: NodeTotals<'_>,
) -> io::Result<u64> {
    let order = tree.preorder();
    write_records(out, header, order.len() as u64, |w| {
        for &(node, depth) in &order {
            encode_record(w, &tree_record(tree, totals, node, depth))?;
        }
        Ok(())
    })
}

/// The record of `node` of `tree`, whose per-node totals are `totals`.
pub fn tree_record<'a>(
    tree: &'a ScopeTree,
    totals: NodeTotals<'_>,
    node: u32,
    depth: u32,
) -> NodeRecord<'a> {
    let n = tree.node(node);
    let (nlink, tree_id, objectid) = match tree.inode(n.inode).filter(|_| n.inode != NO_INODE) {
        Some(i) => (i.nlink, i.tree, i.objectid),
        None => (0, 0, 0),
    };
    let metrics = totals.metrics[node as usize];
    let held = totals.held.get(node);
    NodeRecord {
        depth,
        kind: n.kind,
        flags: n.flags,
        name: tree.name(node),
        metrics,
        blockers: totals.blockers[node as usize],
        held,
        marker: tree.marker(node),
        outside: totals
            .excluded
            .get(&node)
            .map(|e| e.leave_out(metrics, held)),
        nlink,
        tree: tree_id,
        objectid,
    }
}

/// Writes a report whose `node_count` records `records` encodes in
/// preorder. Returns the bytes written.
pub fn write_records<W: Write>(
    out: W,
    header: &Header,
    node_count: u64,
    records: impl FnOnce(&mut HashingWriter<W>) -> io::Result<()>,
) -> io::Result<u64> {
    let json = serde_json::to_vec(header).map_err(io::Error::other)?;
    let mut w = HashingWriter::new(out);
    w.put(MAGIC)?;
    w.put(&(json.len() as u32).to_le_bytes())?;
    w.put(&json)?;
    w.put(&node_count.to_le_bytes())?;
    records(&mut w)?;
    let checksum = w.checksum();
    w.put(END)?;
    w.put(&checksum.to_le_bytes())?;
    w.flush()?;
    Ok(w.written())
}

/// Appends one node record in report encoding.
pub fn encode_record<W: Write>(w: &mut HashingWriter<W>, r: &NodeRecord) -> io::Result<()> {
    w.varint(u64::from(r.depth))?;
    w.put(&[r.kind.letter() as u8, r.flags.bits()])?;
    w.varint(r.name.len() as u64)?;
    w.put(r.name)?;
    encode_metrics(w, &r.metrics)?;
    w.varint(u64::from(r.nlink))?;
    w.varint(r.tree)?;
    w.varint(r.objectid)?;
    w.put(&[r.blockers.bits()])?;
    if r.kind == FileKind::Directory {
        encode_held(w, &r.held)?;
        w.varint(r.marker.map_or(0, crate::dsm::marker_code))?;
        w.put(&[u8::from(r.outside.is_some())])?;
        if let Some(outside) = &r.outside {
            encode_metrics(w, &outside.metrics)?;
            encode_held(w, &outside.held)?;
        }
    }
    Ok(())
}

fn encode_metrics<W: Write>(w: &mut HashingWriter<W>, m: &Metrics) -> io::Result<()> {
    for v in [
        m.logical,
        m.physical,
        m.extents,
        m.inline,
        m.entries,
        m.files,
        m.reclaimable,
        m.unknown,
        m.pinned,
    ] {
        w.varint(v)?;
    }
    Ok(())
}

fn encode_held<W: Write>(w: &mut HashingWriter<W>, held: &Held) -> io::Result<()> {
    for bytes in held.0 {
        w.varint(bytes)?;
    }
    Ok(())
}

/// Reads one node record in report encoding; `None` when the input is
/// exhausted or malformed.
pub fn decode_record<'a>(d: &mut Decoder<'a>) -> Option<NodeRecord<'a>> {
    if d.is_empty() {
        return None;
    }
    let depth = u32::try_from(d.varint()?).ok()?;
    let kind = FileKind::from_letter(char::from(d.byte()?))?;
    let flags = NodeFlags::from_bits(d.byte()?);
    let len = usize::try_from(d.varint()?).ok()?;
    let name = d.take(len)?;
    let metrics = decode_metrics(d)?;
    let nlink = u32::try_from(d.varint()?).ok()?;
    let tree = d.varint()?;
    let objectid = d.varint()?;
    let blockers = Blockers::from_bits(d.byte()?);
    let (mut held, mut marker, mut outside) = (Held::default(), None, None);
    if kind == FileKind::Directory {
        held = decode_held(d)?;
        marker = match d.varint()? {
            0 => None,
            code => Some(crate::dsm::marker_of_code(code)?),
        };
        outside = match d.byte()? {
            0 => None,
            1 => Some(Outside {
                metrics: decode_metrics(d)?,
                held: decode_held(d)?,
            }),
            _ => return None,
        };
    }
    Some(NodeRecord {
        depth,
        kind,
        flags,
        name,
        metrics,
        blockers,
        held,
        marker,
        outside,
        nlink,
        tree,
        objectid,
    })
}

fn decode_metrics(d: &mut Decoder<'_>) -> Option<Metrics> {
    Some(Metrics {
        logical: d.varint()?,
        physical: d.varint()?,
        extents: d.varint()?,
        inline: d.varint()?,
        entries: d.varint()?,
        files: d.varint()?,
        reclaimable: d.varint()?,
        unknown: d.varint()?,
        pinned: d.varint()?,
    })
}

fn decode_held(d: &mut Decoder<'_>) -> Option<Held> {
    let mut held = Held::default();
    for bytes in &mut held.0 {
        *bytes = d.varint()?;
    }
    Some(held)
}

/// Reads only the header, for choosing among reports.
pub fn read_header<R: Read>(mut input: R) -> Result<Header, ReportError> {
    let mut magic = [0u8; 8];
    input
        .read_exact(&mut magic)
        .map_err(|_| ReportError::Magic)?;
    if &magic != MAGIC {
        return Err(ReportError::Magic);
    }
    let mut len = [0u8; 4];
    input
        .read_exact(&mut len)
        .map_err(|_| ReportError::Corrupt)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_HEADER {
        return Err(ReportError::Corrupt);
    }
    let mut json = vec![0u8; len as usize];
    input
        .read_exact(&mut json)
        .map_err(|_| ReportError::Corrupt)?;
    parse_header(&json)
}

fn parse_header(json: &[u8]) -> Result<Header, ReportError> {
    #[derive(Deserialize)]
    struct Versioned {
        format: u32,
    }
    let versioned: Versioned =
        serde_json::from_slice(json).map_err(|e| ReportError::Header(e.to_string()))?;
    if versioned.format != FORMAT {
        return Err(ReportError::Format(versioned.format));
    }
    serde_json::from_slice(json).map_err(|e| ReportError::Header(e.to_string()))
}

/// One node as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRecord<'a> {
    pub depth: u32,
    pub kind: FileKind,
    pub flags: NodeFlags,
    pub name: &'a [u8],
    pub metrics: Metrics,
    pub blockers: Blockers,
    /// Retained bytes per holder class; directories only.
    pub held: Held,
    /// The package marker that made a directory package data.
    pub marker: Option<&'static [u8]>,
    /// A directory's values without the package data below it, when it
    /// holds some.
    pub outside: Option<Outside>,
    pub nlink: u32,
    /// Subvolume tree and inode; 0 for nodes without an inode.
    pub tree: u64,
    pub objectid: u64,
}

/// A whole report, checked for structure and checksum.
#[derive(Debug)]
pub struct Report {
    pub header: Header,
    bytes: Vec<u8>,
    nodes_start: usize,
    nodes_end: usize,
    node_count: u64,
}

impl Report {
    pub fn parse(bytes: Vec<u8>) -> Result<Report, ReportError> {
        let header = read_header(bytes.as_slice())?;
        let json_len = u32::from_le_bytes(bytes[8..12].try_into().expect("read_header checked"));
        let count_at = 12 + json_len as usize;
        let trailer_at = bytes.len().checked_sub(16).ok_or(ReportError::Corrupt)?;
        if trailer_at < count_at + 8 || &bytes[trailer_at..trailer_at + 8] != END {
            return Err(ReportError::Corrupt);
        }
        let stored = u64::from_le_bytes(bytes[trailer_at + 8..].try_into().expect("8 bytes"));
        if codec::checksum(&bytes[..trailer_at]) != stored {
            return Err(ReportError::Checksum);
        }
        let node_count = u64::from_le_bytes(bytes[count_at..count_at + 8].try_into().expect("8"));
        let report = Report {
            header,
            nodes_start: count_at + 8,
            nodes_end: trailer_at,
            node_count,
            bytes,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn node_count(&self) -> u64 {
        self.node_count
    }

    fn validate(&self) -> Result<(), ReportError> {
        let mut decoder = self.decoder();
        let mut order = Preorder::default();
        for _ in 0..self.node_count {
            let record = decode_record(&mut decoder).ok_or(ReportError::Corrupt)?;
            order.check(&record)?;
        }
        if decoder.at() != self.nodes_end {
            return Err(ReportError::Corrupt);
        }
        Ok(())
    }

    fn decoder(&self) -> Decoder<'_> {
        Decoder::new(&self.bytes[..self.nodes_end], self.nodes_start)
    }

    /// Visits nodes in preorder with each node's full path.
    pub fn for_each<'a>(
        &'a self,
        mut visit: impl FnMut(&[u8], &NodeRecord<'a>) -> ControlFlow<()>,
    ) {
        let mut paths = Paths::default();
        let mut decoder = self.decoder();
        while let Some(record) = decode_record(&mut decoder) {
            if visit(paths.enter(&record), &record).is_break() {
                return;
            }
        }
    }
}

/// A report read record by record from `input`, never whole: what a
/// query that walks it once needs, in memory bounded by one read chunk.
/// Structure and checksum are checked as it goes; what a visitor
/// gathered stands only when [`ReportReader::for_each`] returns `Ok`.
pub struct ReportReader<R> {
    header: Header,
    node_count: u64,
    input: Input<R>,
}

impl<R: Read> ReportReader<R> {
    /// Reads and checks the header.
    pub fn open(input: R) -> Result<Self, ReportError> {
        let mut input = Input::new(input);
        if input.take(MAGIC.len()).map_err(|_| ReportError::Magic)? != MAGIC {
            return Err(ReportError::Magic);
        }
        let len = u32::from_le_bytes(input.take(4)?.try_into().expect("4 bytes"));
        if len > MAX_HEADER {
            return Err(ReportError::Corrupt);
        }
        let header = parse_header(input.take(len as usize)?)?;
        let node_count = u64::from_le_bytes(input.take(8)?.try_into().expect("8 bytes"));
        Ok(ReportReader {
            header,
            node_count,
            input,
        })
    }

    /// Number of records to stream when republishing an additive analysis.
    pub fn node_count(&self) -> u64 {
        self.node_count
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The header, once the rest of the report is read unparsed and
    /// its checksum checked: for an answer from the header alone.
    pub fn checked_header(mut self) -> Result<Header, ReportError> {
        self.input.skip_to_trailer()?;
        self.input.check_trailer()?;
        Ok(self.header)
    }

    /// Visits nodes in preorder with each node's full path, like
    /// [`Report::for_each`], then checks the rest of the report.
    pub fn for_each(
        self,
        mut visit: impl FnMut(&[u8], &NodeRecord<'_>) -> ControlFlow<()>,
    ) -> Result<(), ReportError> {
        self.for_each_span(|path, record, _| visit(path, record))
    }

    /// Same checked walk, also exposing each encoded record's absolute byte range.
    pub fn for_each_span(
        mut self,
        mut visit: impl FnMut(&[u8], &NodeRecord<'_>, std::ops::Range<u64>) -> ControlFlow<()>,
    ) -> Result<(), ReportError> {
        let mut paths = Paths::default();
        let mut order = Preorder::default();
        let mut stopped = false;
        for _ in 0..self.node_count {
            let input = &mut self.input;
            input.fill(MAX_RECORD)?;
            let mut decoder = Decoder::new(&input.buf[input.at..input.end], 0);
            let record = decode_record(&mut decoder).ok_or(ReportError::Corrupt)?;
            let len = decoder.at();
            let start = input.consumed;
            input.hash.update(&input.buf[input.at..input.at + len]);
            input.at += len;
            input.consumed += len as u64;
            order.check(&record)?;
            if visit(paths.enter(&record), &record, start..input.consumed).is_break() {
                stopped = true;
                break;
            }
        }
        if stopped {
            self.input.skip_to_trailer()?;
        }
        self.input.check_trailer()
    }
}

/// Display-only bounded subtree decoding. The caller validates a report-bound range;
/// destructive operations continue using the original checked reader.
pub fn display_subtree(
    mut file: std::fs::File,
    range: std::ops::Range<u64>,
    depth: u32,
    root: &[u8],
    mut visit: impl FnMut(&[u8], &NodeRecord<'_>) -> ControlFlow<()>,
) -> Result<(), ReportError> {
    use std::io::{Seek, SeekFrom};
    let size = file.metadata()?.len();
    if range.start >= range.end || range.end > size.saturating_sub(TRAILER as u64) {
        return Err(ReportError::Corrupt);
    }
    file.seek(SeekFrom::Start(range.start))?;
    let len = range.end - range.start;
    let mut input = Input::new(file.take(len));
    let mut paths = Paths::default();
    let mut order = Preorder::default();
    while input.consumed < len {
        input.fill(MAX_RECORD)?;
        let mut decoder = Decoder::new(&input.buf[input.at..input.end], 0);
        let mut record = decode_record(&mut decoder).ok_or(ReportError::Corrupt)?;
        let count = decoder.at();
        let first = input.consumed == 0;
        if (first && record.depth != depth) || (!first && record.depth <= depth) {
            return Err(ReportError::Corrupt);
        }
        if first {
            let expected = if depth == 0 {
                root
            } else {
                root.rsplit(|b| *b == b'/').next().unwrap_or(root)
            };
            if record.name != expected {
                return Err(ReportError::Corrupt);
            }
            record.name = root;
        }
        record.depth -= depth;
        order.check(&record)?;
        if visit(paths.enter(&record), &record).is_break() {
            return Ok(());
        }
        input.at += count;
        input.consumed += count as u64;
    }
    Ok(())
}

/// Longer records are corrupt: a name is at most a full path.
const MAX_RECORD: usize = 64 << 10;
const CHUNK: usize = 1 << 20;
/// `END` and the checksum.
const TRAILER: usize = 16;

/// Report bytes read ahead in chunks and checksummed as consumed.
struct Input<R> {
    input: R,
    buf: Vec<u8>,
    /// The first unconsumed byte of `buf`.
    at: usize,
    /// The end of the bytes read into `buf`.
    end: usize,
    eof: bool,
    hash: Fnv,
    consumed: u64,
}

impl<R: Read> Input<R> {
    fn new(input: R) -> Self {
        Input {
            input,
            buf: Vec::new(),
            at: 0,
            end: 0,
            eof: false,
            hash: Fnv::new(),
            consumed: 0,
        }
    }

    /// Makes at least `want` unconsumed bytes available, fewer only at
    /// the end of the input. Returns how many are.
    fn fill(&mut self, want: usize) -> io::Result<usize> {
        if self.end - self.at < want {
            self.buf.copy_within(self.at..self.end, 0);
            self.end -= self.at;
            self.at = 0;
            if self.buf.len() < want {
                self.buf.resize(want.max(CHUNK), 0);
            }
        }
        while self.end < want && !self.eof {
            let n = match self.input.read(&mut self.buf[self.end..]) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                read => read?,
            };
            self.end += n;
            self.eof = n == 0;
        }
        Ok(self.end - self.at)
    }

    /// The unconsumed bytes read so far.
    fn unconsumed(&self) -> &[u8] {
        &self.buf[self.at..self.end]
    }

    /// Consumes exactly `len` bytes.
    fn take(&mut self, len: usize) -> Result<&[u8], ReportError> {
        if self.fill(len)? < len {
            return Err(ReportError::Corrupt);
        }
        self.consume(len);
        Ok(&self.buf[self.at - len..self.at])
    }

    fn consume(&mut self, len: usize) {
        self.hash.update(&self.buf[self.at..self.at + len]);
        self.at += len;
        self.consumed += len as u64;
    }

    /// Consumes everything but the trailer, unread.
    fn skip_to_trailer(&mut self) -> Result<(), ReportError> {
        loop {
            let available = self.fill(TRAILER + CHUNK)?;
            if available < TRAILER {
                return Err(ReportError::Corrupt);
            }
            self.consume(available - TRAILER);
            if self.eof {
                return Ok(());
            }
        }
    }

    /// The input ends in `END` and the checksum of everything consumed.
    fn check_trailer(&mut self) -> Result<(), ReportError> {
        if self.fill(TRAILER + 1)? != TRAILER {
            return Err(ReportError::Corrupt);
        }
        let trailer = self.unconsumed();
        if &trailer[..8] != END {
            return Err(ReportError::Corrupt);
        }
        let stored = u64::from_le_bytes(trailer[8..].try_into().expect("8 bytes"));
        if stored != self.hash.value() {
            return Err(ReportError::Checksum);
        }
        Ok(())
    }
}

/// Checks that records come in preorder: the first at depth 0, each
/// other at most one deeper than the one before.
#[derive(Debug, Default)]
struct Preorder {
    last: Option<u32>,
}

impl Preorder {
    fn check(&mut self, record: &NodeRecord<'_>) -> Result<(), ReportError> {
        let valid = match self.last {
            None => record.depth == 0,
            Some(last) => record.depth <= last + 1,
        };
        self.last = Some(record.depth);
        valid.then_some(()).ok_or(ReportError::Corrupt)
    }
}

/// Builds each record's full path from the records before it.
#[derive(Debug, Default)]
struct Paths {
    path: Vec<u8>,
    /// Where the path of each open depth ends.
    ends: Vec<usize>,
}

impl Paths {
    /// The full path of `record`, the next record in preorder.
    fn enter(&mut self, record: &NodeRecord<'_>) -> &[u8] {
        self.ends.truncate(record.depth as usize);
        self.path.truncate(self.ends.last().copied().unwrap_or(0));
        if record.depth > 0 {
            self.path.push(b'/');
        }
        self.path.extend_from_slice(record.name);
        self.ends.push(self.path.len());
        &self.path
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn header(roots: Vec<RootInfo>) -> Header {
        Header {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            capabilities: crate::filesystem::FilesystemKind::Btrfs.capabilities(),
            format: FORMAT,
            tool_version: "0.1.0".into(),
            fsid: "00000000-0000-0000-0000-000000000001".into(),
            mount: b"/v".to_vec(),
            job_id: "20260927T000000Z-1".into(),
            scan_started: 1_790_000_000,
            scan_finished: 1_790_000_060,
            profile: "low".into(),
            roots,
            consistency: Consistency::ObservedStable,
            change_reasons: Vec::new(),
            state_dir_writes: Vec::new(),
            subvolumes: vec![SubvolumeState {
                id: 257,
                role: SubvolumeRole::Scanned,
                generation_before: 9,
                generation_after: Some(9),
                captured: None,
                uuid: None,
                last_snapshot: None,
                snapshots: Vec::new(),
            }],
            coverage: Coverage {
                reference_accounting: "unavailable".into(),
                ..Coverage::default()
            },
            totals: Metrics::default(),
            accounting: AccountingSummary::default(),
            stats: ScanStats::default(),
            snapshots: None,
            space: None,
            overview: None,
            recycle_ctimes: Vec::new(),
            deletion: None,
            vfs_space: None,
            allocated_overview: None,
        }
    }

    /// Writes `tree` with its footprint only.
    pub fn write_footprint<W: Write>(out: W, header: &Header, tree: &ScopeTree) -> io::Result<u64> {
        let metrics = tree.aggregate();
        let blockers = vec![Blockers::default(); tree.len()];
        let totals = NodeTotals {
            metrics: &metrics,
            blockers: &blockers,
            held: &DirHeld::default(),
            excluded: &HashMap::new(),
        };
        write(out, header, tree, totals)
    }

    pub fn encode(header: &Header, tree: &ScopeTree) -> Vec<u8> {
        let blockers = vec![Blockers::default(); tree.len()];
        encode_accounted(header, tree, &tree.aggregate(), &blockers)
    }

    pub fn encode_accounted(
        header: &Header,
        tree: &ScopeTree,
        metrics: &[Metrics],
        blockers: &[Blockers],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let totals = NodeTotals {
            metrics,
            blockers,
            held: &DirHeld::default(),
            excluded: &HashMap::new(),
        };
        write(&mut out, header, tree, totals).unwrap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::btrfs::subvolume::SubvolumeSet;
    use crate::model::testing::*;
    use crate::model::tree::{Marks, RootKind};

    const MIB: u64 = 1 << 20;

    #[test]
    fn a_capture_starts_at_the_oldest_reused_unit() {
        let state = |id, captured| SubvolumeState {
            id,
            role: SubvolumeRole::Reused,
            generation_before: 1,
            generation_after: Some(1),
            captured,
            uuid: None,
            last_snapshot: None,
            snapshots: Vec::new(),
        };
        assert_eq!(capture_started(500, &[state(256, None)]), 500);
        let reused = [
            state(256, Some(300)),
            state(257, Some(200)),
            state(258, None),
        ];
        assert_eq!(capture_started(500, &reused), 200);
        assert_eq!(capture_started(100, &reused), 100, "never after the run");
    }

    fn tree() -> ScopeTree {
        let ns = read_tree(
            257,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "a", 257),
                entry(256, 3, "d\u{1b}", 258),
                inode(257, FILE, MIB, 2),
                whole_extent(257, 0, 10 * MIB, MIB),
                inode(258, DIR, 0, 1),
                entry(258, 2, "b", 257),
            ],
        )
        .unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v/s",
            RootKind::Path,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        tree
    }

    fn root_info() -> Vec<RootInfo> {
        vec![RootInfo {
            path: b"/v/s".to_vec(),
            scope: Scope::Path,
            tree: 257,
            ino: 256,
        }]
    }

    fn nodes(report: &Report) -> Vec<(Vec<u8>, NodeRecord<'_>)> {
        let mut out = Vec::new();
        report.for_each(|path, record| {
            out.push((path.to_vec(), *record));
            ControlFlow::Continue(())
        });
        out
    }

    #[test]
    fn round_trips_header_and_nodes_in_preorder() {
        let header = header(root_info());
        let report = Report::parse(encode(&header, &tree())).unwrap();
        assert_eq!(report.header, header);
        assert_eq!(report.node_count(), 4);
        let nodes = nodes(&report);
        let paths: Vec<&[u8]> = nodes.iter().map(|(p, _)| p.as_slice()).collect();
        assert_eq!(
            paths,
            [&b"/v/s"[..], b"/v/s/a", b"/v/s/d\x1b", b"/v/s/d\x1b/b"]
        );
        let (_, root) = nodes[0];
        assert_eq!(
            (root.kind, root.tree, root.objectid),
            (FileKind::Directory, 257, 256)
        );
        assert_eq!(root.metrics.logical, MIB);
        assert_eq!(root.metrics.physical, MIB);
        assert_eq!(root.metrics.entries, 4);
        let (_, file) = nodes[3];
        assert_eq!(
            (file.kind, file.nlink, file.objectid),
            (FileKind::Regular, 2, 257)
        );
    }

    #[test]
    fn round_trips_accounting_metrics_and_blockers() {
        let tree = tree();
        let mut metrics = tree.aggregate();
        let file = (0..tree.len())
            .find(|&n| tree.path(n as u32) == b"/v/s/a")
            .unwrap();
        metrics[file].reclaimable = 3;
        metrics[file].unknown = 5;
        metrics[file].pinned = 7;
        let mut blockers = vec![Blockers::default(); tree.len()];
        blockers[file] = Blockers::SNAPSHOT.with(Blockers::HARD_LINK);
        let header = header(root_info());
        let report = Report::parse(encode_accounted(&header, &tree, &metrics, &blockers)).unwrap();
        let nodes = nodes(&report);
        let (_, a) = nodes[1];
        let m = a.metrics;
        assert_eq!((m.reclaimable, m.unknown, m.pinned), (3, 5, 7));
        assert_eq!(a.blockers, blockers[file]);
        assert!(nodes[0].1.blockers.is_empty());
    }

    struct SameTree;

    impl crate::model::accounting::RootClass for SameTree {
        fn external_root(&self, _root: u64) -> Blockers {
            Blockers::LIVE_REFLINK
        }

        fn shared_leaf(&self, _tree: u64) -> Blockers {
            Blockers::SNAPSHOT
        }
    }

    #[test]
    fn directories_keep_what_holds_their_retained_bytes() {
        use crate::btrfs::backref::DataBackref;
        use crate::btrfs::extent_refs::ExtentRefs;
        use crate::model::accounting::HeldClass;
        use crate::model::tree::Accountant;
        let mut tree = tree();
        tree.prepare();
        let refs = ExtentRefs {
            num_bytes: MIB,
            refs: 1,
            generation: 1,
            backrefs: vec![DataBackref::Direct {
                root: 257,
                objectid: 257,
                offset: 0,
                count: 1,
            }],
            malformed: false,
        };
        let mut accountant = Accountant::new(&tree, Some(&SameTree));
        accountant.extent(10 * MIB, Some(&refs));
        let aggregate = accountant.finish();
        let mut bytes = Vec::new();
        write(&mut bytes, &header(root_info()), &tree, aggregate.totals()).unwrap();
        let report = Report::parse(bytes).unwrap();
        let held: Vec<(Vec<u8>, u64)> = nodes(&report)
            .iter()
            .map(|(path, r)| (path.clone(), r.held.get(HeldClass::HardLink)))
            .collect();
        assert_eq!(
            held,
            [
                (b"/v/s".to_vec(), 0),
                (b"/v/s/a".to_vec(), 0),
                (b"/v/s/d\x1b".to_vec(), MIB),
                (b"/v/s/d\x1b/b".to_vec(), 0),
            ]
        );
    }

    #[test]
    fn a_header_keeps_why_the_scan_changed() {
        let mut header = header(root_info());
        header.consistency = Consistency::ChangedDuringScan;
        header.change_reasons = vec![
            ChangeReason::SubvolumeGenerationChanged {
                subvolume_id: 256,
                selected: false,
                generation_before: 20,
                generation_after: 21,
            },
            ChangeReason::MaintenanceProbeFailed {
                operation: Maintenance::Balance,
                errno: None,
            },
        ];
        let report = Report::parse(encode(&header, &tree())).unwrap();
        assert_eq!(report.header.change_reasons, header.change_reasons);
    }

    #[test]
    fn change_reasons_read_as_one_phrase_each() {
        let reasons = [
            ChangeReason::SubvolumeGenerationChanged {
                subvolume_id: 256,
                selected: true,
                generation_before: 20,
                generation_after: 21,
            },
            ChangeReason::SubvolumeGenerationChanged {
                subvolume_id: 258,
                selected: false,
                generation_before: 7,
                generation_after: 9,
            },
            ChangeReason::SubvolumeAppeared {
                subvolume_id: 259,
                generation: 40,
                has_uuid: true,
                snapshot_of: None,
            },
            ChangeReason::SubvolumeVanished {
                subvolume_id: 257,
                generation_before: 9,
            },
            ChangeReason::MaintenanceRunning {
                operation: Maintenance::DevReplace,
            },
            ChangeReason::MaintenanceProbeFailed {
                operation: Maintenance::Balance,
                errno: Some(1),
            },
            ChangeReason::MaintenanceProbeFailed {
                operation: Maintenance::Balance,
                errno: None,
            },
            ChangeReason::RelocationTree { subvolume_id: 256 },
            ChangeReason::SubvolumeAppeared {
                subvolume_id: 260,
                generation: 41,
                has_uuid: false,
                snapshot_of: None,
            },
            ChangeReason::SubvolumeAppeared {
                subvolume_id: 261,
                generation: 41,
                has_uuid: true,
                snapshot_of: Some(256),
            },
            ChangeReason::SubvolumeSnapshotted {
                subvolume_id: 256,
                last_snapshot_before: 30,
                last_snapshot_after: 41,
            },
            ChangeReason::SubvolumeUuidChanged { subvolume_id: 256 },
        ];
        let phrases: Vec<String> = reasons.iter().map(ToString::to_string).collect();
        assert_eq!(
            phrases,
            [
                "selected subvolume 256 generation 20 -> 21",
                "subvolume 258 outside the selection generation 7 -> 9",
                "subvolume 259 appeared",
                "selected subvolume 257 vanished",
                "dev-replace running",
                "balance probe failed (errno 1)",
                "balance probe failed",
                "subvolume 256 being relocated",
                "subvolume 260 appeared without a uuid",
                "subvolume 261 appeared as a snapshot of 256",
                "selected subvolume 256 snapshotted (last snapshot 30 -> 41)",
                "selected subvolume 256 uuid changed",
            ]
        );
    }

    #[test]
    fn consistency_is_explained_by_its_reasons() {
        assert_eq!(
            consistency_text(Consistency::ObservedStable, &[]),
            "observed-stable"
        );
        let reasons = [
            ChangeReason::SubvolumeAppeared {
                subvolume_id: 259,
                generation: 40,
                has_uuid: true,
                snapshot_of: None,
            },
            ChangeReason::MaintenanceRunning {
                operation: Maintenance::Balance,
            },
        ];
        assert_eq!(
            consistency_text(Consistency::ChangedDuringScan, &reasons),
            "changed-during-scan (subvolume 259 appeared; balance running)"
        );
    }

    #[test]
    fn a_header_written_before_change_reasons_has_none() {
        let mut json = serde_json::to_value(header(root_info())).unwrap();
        json.as_object_mut().unwrap().remove("change_reasons");
        json.as_object_mut().unwrap().remove("recycle_ctimes");
        let old: Header = serde_json::from_value(json).unwrap();
        assert_eq!(old.change_reasons, []);
        assert!(old.recycle_ctimes.is_empty());
    }

    #[test]
    fn reads_the_header_alone() {
        let header = header(root_info());
        let bytes = encode(&header, &tree());
        assert_eq!(read_header(&bytes[..]).unwrap(), header);
    }

    #[test]
    fn rejects_other_files_and_formats() {
        assert!(matches!(
            Report::parse(b"hello".to_vec()),
            Err(ReportError::Magic)
        ));
        let mut header = header(root_info());
        header.format = 3;
        assert!(matches!(
            Report::parse(encode(&header, &tree())),
            Err(ReportError::Format(3))
        ));
    }

    /// `/v/s`: `a` (1 MiB) and `abb/@ActiveBackup/blob` (2 MiB).
    fn package_tree() -> ScopeTree {
        let ns = read_tree(
            257,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "a", 257),
                entry(256, 3, "abb", 258),
                inode(257, FILE, MIB, 1),
                whole_extent(257, 0, 10 * MIB, MIB),
                inode(258, DIR, 0, 1),
                entry(258, 2, "@ActiveBackup", 259),
                inode(259, DIR, 0, 1),
                entry(259, 2, "blob", 260),
                inode(260, FILE, 2 * MIB, 1),
                whole_extent(260, 0, 20 * MIB, 2 * MIB),
            ],
        )
        .unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v/s",
            RootKind::Path,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        tree
    }

    #[test]
    fn directories_keep_their_values_without_package_data_and_what_marks_it() {
        use crate::model::accounting::HeldClass;
        use crate::model::tree::{Excluded, Outside};
        let tree = package_tree();
        let at = |path: &[u8]| {
            (0..tree.len() as u32)
                .find(|&n| tree.path(n) == path)
                .unwrap()
        };
        let (root, abb) = (at(b"/v/s"), at(b"/v/s/abb"));
        let metrics = tree.aggregate();
        let blockers = vec![Blockers::default(); tree.len()];
        let held = Held::of(HeldClass::HardLink, 5);
        let excluded = HashMap::from([(
            root,
            Excluded {
                metrics: metrics[abb as usize],
                held,
            },
        )]);
        let totals = NodeTotals {
            metrics: &metrics,
            blockers: &blockers,
            held: &DirHeld::default(),
            excluded: &excluded,
        };
        let mut bytes = Vec::new();
        write(&mut bytes, &header(root_info()), &tree, totals).unwrap();
        let report = Report::parse(bytes.clone()).unwrap();
        let nodes = nodes(&report);
        type Found<'a> = (&'a [u8], Option<&'a [u8]>, Option<Outside>);
        let found: Vec<Found<'_>> = nodes
            .iter()
            .map(|(path, r)| (path.as_slice(), r.marker, r.outside))
            .collect();
        let mut without = metrics[root as usize];
        without.sub(&metrics[abb as usize]);
        let mut left = Held::default();
        left.sub(&held);
        assert_eq!(
            found,
            [
                (
                    &b"/v/s"[..],
                    None,
                    Some(Outside {
                        metrics: without,
                        held: left,
                    })
                ),
                (b"/v/s/a", None, None),
                (b"/v/s/abb", Some(&b"@ActiveBackup"[..]), None),
                (b"/v/s/abb/@ActiveBackup", None, None),
                (b"/v/s/abb/@ActiveBackup/blob", None, None),
            ]
        );
        let parsed: Vec<String> = nodes
            .iter()
            .map(|(path, record)| format!("{path:?} {record:?}"))
            .collect();
        assert_eq!(streamed(&bytes).unwrap().1, parsed);
    }

    #[test]
    fn rejects_corruption_and_truncation() {
        let bytes = encode(&header(root_info()), &tree());
        let mut flipped = bytes.clone();
        let at = flipped.len() - 20;
        flipped[at] ^= 1;
        assert!(matches!(Report::parse(flipped), Err(ReportError::Checksum)));
        for len in [bytes.len() - 1, bytes.len() - 17, 20] {
            assert!(Report::parse(bytes[..len].to_vec()).is_err(), "len {len}");
        }
    }

    /// Hands out one byte per read, so every record straddles refills.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some((&first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            buf[0] = first;
            self.0 = rest;
            Ok(1)
        }
    }

    fn streamed(bytes: &[u8]) -> Result<(Header, Vec<String>), ReportError> {
        let reader = ReportReader::open(Trickle(bytes))?;
        let header = reader.header().clone();
        let mut seen = Vec::new();
        reader.for_each(|path, record| {
            seen.push(format!("{path:?} {record:?}"));
            ControlFlow::Continue(())
        })?;
        Ok((header, seen))
    }

    #[test]
    fn streams_the_records_a_parsed_report_holds() {
        let header = header(root_info());
        let bytes = encode(&header, &tree());
        let parsed: Vec<String> = nodes(&Report::parse(bytes.clone()).unwrap())
            .iter()
            .map(|(path, record)| format!("{path:?} {record:?}"))
            .collect();
        assert_eq!(streamed(&bytes).unwrap(), (header, parsed));
    }

    #[test]
    fn a_stream_rejects_what_parsing_rejects() {
        assert!(matches!(streamed(b"hello"), Err(ReportError::Magic)));
        let bytes = encode(&header(root_info()), &tree());
        let mut flipped = bytes.clone();
        let at = flipped.len() - 20;
        flipped[at] ^= 1;
        assert!(matches!(streamed(&flipped), Err(ReportError::Checksum)));
        for len in [bytes.len() - 1, bytes.len() - 17, 20] {
            assert!(streamed(&bytes[..len]).is_err(), "len {len}");
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(streamed(&extra).is_err());
    }

    #[test]
    fn a_stream_stopped_early_still_checks_the_rest() {
        let bytes = encode(&header(root_info()), &tree());
        let mut flipped = bytes.clone();
        let at = flipped.len() - 20;
        flipped[at] ^= 1;
        let reader = ReportReader::open(Trickle(&flipped)).unwrap();
        let mut visits = 0;
        let result = reader.for_each(|_, _| {
            visits += 1;
            ControlFlow::Break(())
        });
        assert_eq!(visits, 1);
        assert!(matches!(result, Err(ReportError::Checksum)));
    }

    #[test]
    fn a_checked_header_still_checks_the_whole_report() {
        let header = header(root_info());
        let bytes = encode(&header, &tree());
        let checked = ReportReader::open(Trickle(&bytes))
            .unwrap()
            .checked_header();
        assert_eq!(checked.unwrap(), header);
        let mut flipped = bytes.clone();
        let at = flipped.len() - 20;
        flipped[at] ^= 1;
        let checked = ReportReader::open(Trickle(&flipped))
            .unwrap()
            .checked_header();
        assert!(matches!(checked, Err(ReportError::Checksum)));
    }

    #[test]
    fn hex_is_lossless() {
        assert_eq!(hex_bytes::encode(b"/v/\xff"), "2f762fff");
        assert_eq!(hex_bytes::decode("2f762fff").unwrap(), b"/v/\xff");
        assert_eq!(hex_bytes::decode("2f7"), None);
        assert_eq!(hex_bytes::decode("zz"), None);
    }
}
