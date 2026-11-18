//! Command result documents (SPEC 14): JSON schema v1 and the human
//! rendering of the same data.

use crate::cli::DEFAULT_STATE_DIR;
use crate::dsm::Service;
use crate::holders::{Fix, held_by, held_by_text};
use crate::model::accounting::{Blockers, Held};
use crate::model::tree::NodeFlags;
use crate::output::{
    Align, JsonMetrics, JsonPath, Notice, TargetError, aligned_table, escape_human, human_bytes,
    human_count, table,
};
use crate::package::PackageData;
use crate::progress::{CleanupProgress, JobProgress, RootfsProgress};
use crate::query::Entry;
use crate::report::{
    ChangeReason, Consistency, Coverage, Header, ScanStats, StateDirWrites, SubvolumeState,
    consistency_text,
};
use crate::resource::{MemoryTrip, SchedulingReport};
use crate::snapshots::SAN_MANAGER;
use crate::snapshots::answer::{
    HeldFileJson, HeldFilesJson, ShareJson, SimulationJson, SnapshotJson,
};
use crate::space::SystemPartition;
use crate::summary::{PackageOverlap, ServiceSummary, ServiceUse};
use crate::target::Scope;
use crate::time::{human_age, human_duration, rfc3339};
use serde::{Deserialize, Serialize};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod overview;
pub use overview::{
    AttributionInfo, DataReconciliation, DfReconciliation, OverviewAction, OverviewDf,
    OverviewDocument, OverviewFilesystem, OverviewItem, OverviewPart, OverviewReason,
    OverviewReasonRoot, OverviewReasonSubcase, OverviewReconciliation, OverviewResult,
    OverviewSpace, OverviewSpaceInfo, SubcaseCode, render_overview,
};
mod why_not_freed;
pub use why_not_freed::{
    Action, ReasonEntry, WhyNotFreedDocument, WhyNotFreedResult, render_why_not_freed,
};

pub const SCHEMA_VERSION: u32 = 1;

/// How a command ended (SPEC 22.1, 22.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    Complete,
    /// A usable result with gaps: errors, skipped targets, or coverage.
    Partial,
    /// Another writer holds a filesystem lock.
    Busy,
    /// Stopped by a stop request (Ctrl+C, SIGTERM, SIGHUP).
    Paused,
    RuntimeLimit,
    ResourceLimit,
    /// Stopped by a cancel request; the job ended.
    Cancelled,
    Failed,
}

impl Completion {
    pub fn name(self) -> &'static str {
        match self {
            Completion::Complete => "complete",
            Completion::Partial => "partial",
            Completion::Busy => "busy",
            Completion::Paused => "paused",
            Completion::RuntimeLimit => "runtime_limit",
            Completion::ResourceLimit => "resource_limit",
            Completion::Cancelled => "cancelled",
            Completion::Failed => "failed",
        }
    }

    /// A run stopped before it finished: a request, signal or limit.
    pub fn is_stop(self) -> bool {
        matches!(
            self,
            Completion::Paused
                | Completion::RuntimeLimit
                | Completion::ResourceLimit
                | Completion::Cancelled
        )
    }

    /// SPEC 22.7 exit codes; `signal` is the stop signal, if any.
    pub fn exit_code(self, signal: Option<i32>) -> i32 {
        match self {
            Completion::Complete => 0,
            Completion::Failed => 1,
            Completion::Partial => 3,
            Completion::Busy => 4,
            Completion::Paused => match signal {
                Some(2) => 130,
                Some(15) => 143,
                Some(1) => 129,
                _ => 10,
            },
            Completion::RuntimeLimit | Completion::ResourceLimit | Completion::Cancelled => 10,
        }
    }
}

/// Requested versus effective resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resources {
    pub scheduling: SchedulingReport,
    pub memory_budget_bytes: u64,
    pub max_runtime_seconds: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeTarget {
    #[serde(flatten)]
    pub path: JsonPath,
    pub scope: Scope,
    pub tree: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode_device: Option<u64>,
    pub ino: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoveredTarget {
    #[serde(flatten)]
    pub path: JsonPath,
    pub covered_by: JsonPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishedReport {
    #[serde(flatten)]
    pub file: JsonPath,
    pub bytes: u64,
}

/// One filesystem of a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilesystemScan {
    pub filesystem: crate::filesystem::FilesystemKind,
    pub capabilities: crate::filesystem::Capabilities,
    pub fsid: String,
    pub mount: JsonPath,
    pub completion: Completion,
    pub targets: Vec<ScopeTarget>,
    pub covered: Vec<CoveredTarget>,
    pub consistency: Option<Consistency>,
    pub change_reasons: Vec<ChangeReason>,
    /// Generation changes left out of `change_reasons`: only the state
    /// directory's own writes made them (SPEC 22.4).
    pub state_dir_writes: Vec<StateDirWrites>,
    pub scan_started: Option<String>,
    pub scan_finished: Option<String>,
    pub totals: Option<JsonMetrics>,
    pub stats: Option<ScanStats>,
    /// Set when the memory budget stopped the scan.
    pub resource_limit: Option<MemoryStop>,
    pub coverage: Option<FilesystemCoverage>,
    pub subvolumes: Option<Vec<SubvolumeState>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode_walk: Option<InodeWalkStats>,
    pub report: Option<PublishedReport>,
    pub errors: Vec<TargetError>,
    pub warnings: Vec<Notice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InodeWalkStats {
    pub entries: u64,
    pub inodes: u64,
    pub elapsed_ms: u64,
    pub peak_rss_bytes: Option<u64>,
}

/// Preserve coverage semantics while making unsupported Btrfs counters null.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemCoverage {
    pub inner: Coverage,
    pub filesystem: crate::filesystem::FilesystemKind,
}

impl std::ops::Deref for FilesystemCoverage {
    type Target = Coverage;
    fn deref(&self) -> &Coverage {
        &self.inner
    }
}

impl Serialize for FilesystemCoverage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.inner).map_err(serde::ser::Error::custom)?;
        if self.filesystem == crate::filesystem::FilesystemKind::Ext4 {
            for field in [
                "namespace_uncertain_subvolumes",
                "snapshots_not_materialized",
                "snapshots_materialized",
                "unlinked_subvolume_entries",
                "missing_subvolume_roots",
                "unresolved_extents",
            ] {
                value[field] = serde_json::Value::Null;
            }
        }
        value.serialize(serializer)
    }
}

impl FilesystemScan {
    pub fn new(fsid: String, mount: &[u8], targets: Vec<ScopeTarget>) -> Self {
        FilesystemScan {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            capabilities: crate::filesystem::FilesystemKind::Btrfs.capabilities(),
            fsid,
            mount: JsonPath::new(mount),
            completion: Completion::Complete,
            targets,
            covered: Vec::new(),
            consistency: None,
            change_reasons: Vec::new(),
            state_dir_writes: Vec::new(),
            scan_started: None,
            scan_finished: None,
            totals: None,
            stats: None,
            resource_limit: None,
            coverage: None,
            subvolumes: Some(Vec::new()),
            inode_walk: None,
            report: None,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// Where a scan stopped on its memory budget (SPEC 22.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryStop {
    /// The tree being read: an fs tree, or the extent tree while resolving.
    pub tree: Option<u64>,
    pub budget_bytes: u64,
    /// The check that tripped; null when an allocation failed instead.
    pub resident_bytes: Option<u64>,
    pub reserved_bytes: Option<u64>,
    pub projected_bytes: Option<u64>,
}

impl MemoryStop {
    pub fn new(tree: u64, budget_bytes: u64, trip: Option<MemoryTrip>) -> Self {
        MemoryStop {
            tree: Some(tree),
            budget_bytes,
            resident_bytes: trip.map(|t| t.resident_bytes),
            reserved_bytes: trip.map(|t| t.reserved_bytes),
            projected_bytes: trip.map(|t| t.projected_bytes()),
        }
    }

    fn line(&self) -> String {
        let why = match (
            self.resident_bytes,
            self.reserved_bytes,
            self.projected_bytes,
        ) {
            (Some(resident), Some(reserved), Some(projected)) => format!(
                "{} resident + {} needed = {}",
                human_bytes(resident),
                human_bytes(reserved),
                human_bytes(projected)
            ),
            _ => "an allocation failed".to_string(),
        };
        format!(
            "  memory budget {}: {why}, tree {}\n",
            human_bytes(self.budget_bytes),
            self.tree
                .map_or_else(|| "inode namespace".to_string(), |tree| tree.to_string())
        )
    }
}

/// Fields every document starts with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Envelope {
    pub schema_version: u32,
    pub command: &'static str,
    pub tool_version: &'static str,
    pub generated_at: String,
    pub completion: Completion,
    pub job_id: Option<String>,
    pub errors: Vec<TargetError>,
    pub warnings: Vec<Notice>,
    /// What to run for volume targets without a report.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scan_hints: Vec<ScanHint>,
}

impl Envelope {
    pub fn new(command: &'static str, now: SystemTime) -> Self {
        Envelope {
            schema_version: SCHEMA_VERSION,
            command,
            tool_version: env!("CARGO_PKG_VERSION"),
            generated_at: rfc3339(now),
            completion: Completion::Complete,
            job_id: None,
            errors: Vec::new(),
            warnings: Vec::new(),
            scan_hints: Vec::new(),
        }
    }
}

/// What to run for a volume target no report covers, and what the
/// last completed scan of it took (SPEC 5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanHint {
    #[serde(flatten)]
    pub target: JsonPath,
    pub command: String,
    pub last_scan: Option<LastScan>,
}

/// `bytes` as one shell word: as-is when plain, else single-quoted. A
/// name holding control or invisible formatting characters is written
/// with `$'...'` escapes, so printing the word never sends them to a
/// terminal.
pub fn shell_word(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c);
    if !text.is_empty() && text.chars().all(plain) {
        return text.into_owned();
    }
    if !text.chars().any(unprintable) {
        return format!("'{}'", text.replace('\'', r"'\''"));
    }
    let mut word = String::from("$'");
    for c in text.chars() {
        match c {
            '\'' => word.push_str(r"\'"),
            '\\' => word.push_str(r"\\"),
            c if unprintable(c) => {
                let mut utf8 = [0; 4];
                for byte in c.encode_utf8(&mut utf8).bytes() {
                    word.push_str(&format!(r"\x{byte:02x}"));
                }
            }
            c => word.push(c),
        }
    }
    word.push('\'');
    word
}

/// Control characters, and the invisible ones that reorder or hide text.
fn unprintable(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastScan {
    pub finished: String,
    pub elapsed_ms: u64,
    pub entries: u64,
}

impl ScanHint {
    /// The hint for volume `mount`, scanned with `state_dir`.
    pub fn new(mount: &[u8], state_dir: &Path, last_scan: Option<LastScan>) -> Self {
        let mut command = format!("synoreclaim scan {}", shell_word(mount));
        if state_dir != Path::new(DEFAULT_STATE_DIR) {
            command.push_str(" --state-dir ");
            command.push_str(&shell_word(state_dir.as_os_str().as_bytes()));
        }
        ScanHint {
            target: JsonPath::new(mount),
            command,
            last_scan,
        }
    }

    /// The hint with `flags` added to its scan command.
    pub fn with_flags(mut self, flags: &str) -> Self {
        self.command.push(' ');
        self.command.push_str(flags);
        self
    }

    pub fn human(&self) -> String {
        let estimate = match &self.last_scan {
            Some(last) => format!(
                "its last full scan took {} ({} entries, finished {})",
                human_duration(Duration::from_millis(last.elapsed_ms)),
                human_count(last.entries),
                last.finished
            ),
            None => "no completed scan of it is recorded to estimate from".into(),
        };
        format!(
            "hint: run `{}` to answer for {}; {estimate}",
            self.command,
            escape_path(&self.target)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub resources: Resources,
    pub requested_targets: Vec<JsonPath>,
    pub filesystems: Vec<FilesystemScan>,
}

/// A cleanup that made reports stale, as answers name it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupMark {
    pub plan_id: String,
    /// The execution log's file name under `cleanup/logs`.
    pub execution_log: String,
    pub applied_at: String,
}

/// Whether a cleanup made a report stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marked {
    No,
    By(CleanupMark),
    /// The record is unreadable, so any report may be stale.
    Unknown,
}

/// The report an answer came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportInfo {
    pub filesystem: crate::filesystem::FilesystemKind,
    pub capabilities: crate::filesystem::Capabilities,
    pub fsid: String,
    #[serde(flatten)]
    pub file: JsonPath,
    pub job_id: String,
    pub scan_started: String,
    pub scan_finished: String,
    pub age_seconds: u64,
    pub stale: bool,
    pub consistency: Consistency,
    pub change_reasons: Vec<ChangeReason>,
    pub coverage: FilesystemCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<crate::overview::Attribution>,
    /// The newest cleanup applied since this report's scan (SPEC 5.7).
    pub cleanup_since_report: Option<CleanupMark>,
    /// The cleanup record could not be read, so a cleanup may have run.
    #[serde(skip)]
    pub cleanup_record_unreadable: bool,
}

impl ReportInfo {
    /// Marks the report stale when a cleanup ran after its scan.
    pub fn with_cleanup(mut self, marked: Marked) -> Self {
        match marked {
            Marked::No => {}
            Marked::By(mark) => {
                self.stale = true;
                self.cleanup_since_report = Some(mark);
            }
            Marked::Unknown => {
                self.stale = true;
                self.cleanup_record_unreadable = true;
            }
        }
        self
    }

    pub fn new(file: &[u8], header: &Header, now: SystemTime, max_age: Duration) -> Self {
        let age = now
            .duration_since(at(header.scan_finished))
            .unwrap_or(Duration::ZERO);
        ReportInfo {
            filesystem: header.filesystem,
            capabilities: header.capabilities.clone(),
            fsid: header.fsid.clone(),
            file: JsonPath::new(file),
            job_id: header.job_id.clone(),
            scan_started: rfc3339(at(header.scan_started)),
            scan_finished: rfc3339(at(header.scan_finished)),
            age_seconds: age.as_secs(),
            stale: age > max_age,
            consistency: header.consistency,
            change_reasons: header.change_reasons.clone(),
            coverage: FilesystemCoverage {
                inner: header.coverage.clone(),
                filesystem: header.filesystem,
            },
            attribution: crate::overview::Attribution::of(header),
            cleanup_since_report: None,
            cleanup_record_unreadable: false,
        }
    }
}

fn at(unix: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(unix)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryJson {
    #[serde(flatten)]
    pub path: JsonPath,
    pub kind: &'static str,
    pub flags: Vec<&'static str>,
    pub metrics: JsonMetrics,
    /// Why some of the footprint is not reclaimable.
    pub blockers: Vec<&'static str>,
    pub nlink: u32,
    pub tree: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode_device: Option<u64>,
    pub objectid: u64,
    /// The package-managed data the entry is, lies in or holds (SPEC
    /// 5.7): free it in the package, never by deleting its files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_data: Option<PackageDataJson>,
    /// A directory's values without the package data below it, which
    /// service rows count; `top` shows and ranks these (SPEC 5.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub without_package_data: Option<JsonMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageDataJson {
    /// The service code of [`crate::dsm`].
    pub service: &'static str,
    pub package: &'static str,
    pub manage_in: &'static str,
    /// Stable id of the DSM place (SPEC 4.6, GAP-14).
    pub manage_in_id: &'static str,
    pub confirmed: bool,
    /// The marker entry, when a marker made a folder package data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    /// The entry holds the data below itself rather than being in it.
    pub holds: bool,
    pub message: String,
}

const FLAG_NAMES: [(NodeFlags, &str); 7] = [
    (NodeFlags::SYSTEM, "system"),
    (NodeFlags::RECYCLE, "recycle"),
    (NodeFlags::ENCRYPTED, "encrypted"),
    (NodeFlags::SUBVOLUME_ROOT, "subvolume_root"),
    (NodeFlags::OWN_STATE, "own_state"),
    (NodeFlags::PLACEHOLDER, "unlinked_subvolume"),
    (NodeFlags::SNAPSHOT, "snapshot_not_read"),
];

impl From<&Entry> for EntryJson {
    fn from(e: &Entry) -> Self {
        EntryJson {
            path: JsonPath::new(&e.path),
            kind: e.kind.name(),
            flags: FLAG_NAMES
                .iter()
                .filter(|(f, _)| e.flags.contains(*f))
                .map(|(_, name)| *name)
                .collect(),
            metrics: JsonMetrics::from(&e.metrics),
            blockers: e.blockers.names(),
            nlink: e.nlink,
            tree: Some(e.tree),
            inode_device: None,
            objectid: e.objectid,
            package_data: None,
            without_package_data: e.outside.as_ref().map(|o| JsonMetrics::from(&o.metrics)),
        }
    }
}

impl EntryJson {
    pub fn with_filesystem(mut self, filesystem: crate::filesystem::FilesystemKind) -> Self {
        if filesystem == crate::filesystem::FilesystemKind::Ext4 {
            self.inode_device = self.tree.take();
        }
        self.metrics = self.metrics.with_filesystem(filesystem);
        self.without_package_data = self
            .without_package_data
            .map(|metrics| metrics.with_filesystem(filesystem));
        self
    }
    /// The entry and the package data it is, lies in or holds.
    pub fn with_package(entry: &Entry, package: Option<&PackageData>) -> Self {
        EntryJson {
            package_data: package.map(PackageDataJson::from),
            ..EntryJson::from(entry)
        }
    }
}

/// A row of `top` (SPEC 5.4): an entry, or a DSM service's paths as one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopRow {
    #[serde(flatten)]
    pub entry: EntryJson,
    /// The service a service row adds up; `None` for entries.
    pub service: Option<ServiceRef>,
    /// Whether `cleanup` may delete the row's paths.
    pub cleanup_eligible: bool,
    /// Who holds the bytes deleting the row would not free, largest first.
    pub held_by: Vec<HeldJson>,
    /// The `HELD BY` column.
    #[serde(skip)]
    pub held_text: String,
    pub where_to_fix: FixJson,
    /// Entire full footprint is already represented by this row in the same
    /// JSON answer. Absent for CLI subset metrics and older answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub within: Option<JsonPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceRef {
    /// A [`crate::dsm`] code, or `other_dsm_service_data`.
    pub id: &'static str,
    pub names: String,
    pub managed_by: &'static str,
    pub manage_in: &'static str,
    /// Stable id of the DSM place (SPEC 4.6, GAP-14).
    pub manage_in_id: &'static str,
    pub confirmed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<&'static str>,
    /// The first paths added up.
    pub paths: Vec<JsonPath>,
    pub path_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldJson {
    /// A holder class of [`crate::holders::Holder::name`].
    pub class: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixJson {
    /// A [`Fix::token`].
    pub token: &'static str,
    /// The full DSM place to act in, when the fix is in DSM.
    pub manage_in: Option<&'static str>,
    /// Stable id of that place (SPEC 4.6, GAP-14); null with `manage_in`.
    pub manage_in_id: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<&'static str>,
    /// The synoreclaim command that acts or tells more.
    pub command: Option<Vec<String>>,
    /// The column text.
    #[serde(skip)]
    pub label: &'static str,
    /// What `--explain` prints.
    #[serde(skip)]
    pub hint: String,
}

impl FixJson {
    /// The fix of `path`, below the volume `mount` and a directory when
    /// `dir`.
    fn new(fix: Fix, path: &[u8], mount: &[u8], dir: bool) -> Self {
        FixJson {
            token: fix.token(),
            manage_in: fix.manage_in(),
            manage_in_id: fix.manage_in_id(),
            explanation: fix.explanation(),
            command: fix.command(path, mount, dir),
            label: fix.label(),
            hint: fix.hint(),
        }
    }
}

/// The holders of `held` and `unknown` bytes and the column text.
fn held_json(held: &Held, unknown: u64) -> (Vec<HeldJson>, String) {
    let holders = held_by(held, unknown);
    let json = holders
        .iter()
        .map(|&(holder, bytes)| HeldJson {
            class: holder.name(),
            bytes,
        })
        .collect();
    (json, held_by_text(&holders))
}

impl TopRow {
    /// Freed only through the package managing its data.
    fn in_package(&self) -> bool {
        self.where_to_fix.token == "package"
    }

    /// The `PATH` column.
    fn label(&self) -> String {
        match &self.service {
            Some(s) if s.id == "other_dsm_service_data" && s.path_count == 1 => {
                format!("[DSM] {}", escape_path(&self.entry.path))
            }
            Some(s) if s.id == "other_dsm_service_data" => {
                format!("[DSM] other @ entries ({} paths)", s.path_count)
            }
            Some(s) => format!("[DSM] {} ({})", s.managed_by, s.names),
            None if self.entry.flags.contains(&"snapshot_not_read") => {
                format!("{} (snapshot, not read)", escape_path(&self.entry.path))
            }
            None => escape_path(&self.entry.path),
        }
    }

    /// A ranked entry below `mount` and the package data it is, lies in
    /// or holds; only data it is or lies in is fixed in the package.
    pub fn entry(entry: &Entry, mount: &[u8], package: Option<&PackageData>) -> Self {
        let protected = crate::guard::protected_target_reason(
            Path::new(std::ffi::OsStr::from_bytes(&entry.path)),
            Path::new(std::ffi::OsStr::from_bytes(mount)),
        );
        let managing = package.filter(|p| !p.holds).map(|p| p.service);
        let dir = entry.kind == crate::model::namespace::FileKind::Directory;
        let fix = Fix::for_entry(entry, mount, managing);
        let (held_by, _) = held_json(&entry.retained(), entry.metrics.unknown);
        let (_, held_text) = held_json(
            &entry.retained_without_package_data(),
            entry.metrics_without_package_data().unknown,
        );
        TopRow {
            entry: EntryJson::with_package(entry, package),
            service: None,
            cleanup_eligible: package.is_none() && protected.is_none(),
            held_by,
            held_text,
            where_to_fix: FixJson::new(fix, &entry.path, mount, dir),
            within: None,
        }
    }

    /// A service line's paths as one row, freed only in its package.
    pub fn service(line: &ServiceUse) -> Self {
        let (id, names, managed_by, manage_in, manage_in_id, confirmed) =
            service_facts(line.service);
        let first = line.listed.first().map_or(&[][..], |p| p);
        let mut metrics = JsonMetrics::from(&line.metrics);
        if line.service == Some(crate::dsm::syno_reclaim()) {
            metrics.reclaimable = None;
            metrics.reclaimable_resolved = 0;
        }
        let entry = match &line.entry {
            Some(entry) => EntryJson {
                kind: "service",
                metrics,
                without_package_data: None,
                ..EntryJson::from(entry)
            },
            None => EntryJson {
                path: JsonPath::new(first),
                kind: "service",
                flags: Vec::new(),
                metrics,
                blockers: Vec::new(),
                nlink: 0,
                tree: None,
                inode_device: None,
                objectid: 0,
                package_data: None,
                without_package_data: None,
            },
        };
        let where_to_fix = match line.service {
            Some(service) => FixJson::new(Fix::Package(service), first, b"", true),
            None => FixJson {
                token: "package",
                manage_in: Some(manage_in),
                manage_in_id: Some(manage_in_id),
                explanation: None,
                command: None,
                label: "the package",
                hint: crate::dsm::hint(managed_by, manage_in, confirmed),
            },
        };
        let (held_by, held_text) = held_json(&line.held, line.metrics.unknown);
        TopRow {
            entry,
            service: Some(ServiceRef {
                id,
                names,
                managed_by,
                manage_in,
                manage_in_id,
                confirmed,
                explanation: line.service.and_then(|service| service.explanation),
                paths: line.listed.iter().map(|p| JsonPath::new(p)).collect(),
                path_count: line.paths,
            }),
            cleanup_eligible: false,
            held_by,
            held_text,
            where_to_fix,
            within: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopResult {
    pub report: ReportInfo,
    pub target: EntryJson,
    pub entries: Vec<TopRow>,
    /// Detail rows of DSM service data not listed.
    pub hidden_system: u64,
    /// DSM service data below the target, ranked or hidden.
    pub services: ServicesJson,
    /// Deeper package paths also included in a ranked entry.
    #[serde(skip)]
    pub package_overlaps: Vec<PackageOverlap>,
}

impl TopResult {
    pub fn with_filesystem(mut self) -> Self {
        let filesystem = self.report.filesystem;
        self.target = self.target.with_filesystem(filesystem);
        for row in &mut self.entries {
            row.entry = row.entry.clone().with_filesystem(filesystem);
            if !self.report.capabilities.cleanup {
                row.cleanup_eligible = false;
            }
        }
        for service in &mut self.services.lines {
            service.metrics = service.metrics.clone().with_filesystem(filesystem);
        }
        self
    }
}

/// One line of the DSM service data summary (survey item A1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceLine {
    /// A [`crate::dsm`] code, or `other_dsm_service_data`.
    pub service: &'static str,
    /// The entry names it covers, such as `@eaDir` or `*.hbk`.
    pub names: String,
    pub package: &'static str,
    /// Where in DSM to manage or free it.
    pub manage_in: &'static str,
    /// Stable id of the DSM place (SPEC 4.6, GAP-14).
    pub manage_in_id: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<&'static str>,
    /// Whether the layout was seen on DSM or is documented by Synology.
    pub confirmed: bool,
    /// The path, when the line is one path.
    #[serde(flatten)]
    pub path: Option<JsonPath>,
    pub paths: u64,
    /// Sums of each path deleted alone; footprints shared between the
    /// paths count once per path.
    pub metrics: JsonMetrics,
    pub snapshots_not_read: u64,
    /// Some or all of this summary's metadata is already included in folder rows.
    pub included_in_rows: bool,
}

/// The code, names, package, place and confirmation of a service line;
/// `None` is other `@…` volume-root data.
fn service_facts(
    service: Option<&Service>,
) -> (
    &'static str,
    String,
    &'static str,
    &'static str,
    &'static str,
    bool,
) {
    match service {
        Some(s) => (
            s.code,
            s.names(),
            s.package,
            s.manage_in,
            s.manage_in_id(),
            s.confirmed,
        ),
        None => (
            "other_dsm_service_data",
            "@*".to_string(),
            "DSM or a package",
            crate::dsm::OWNING_PACKAGE,
            crate::dsm::place_id(crate::dsm::OWNING_PACKAGE),
            false,
        ),
    }
}

impl From<&ServiceUse> for ServiceLine {
    fn from(u: &ServiceUse) -> Self {
        let (service, names, package, manage_in, manage_in_id, confirmed) =
            service_facts(u.service);
        let mut metrics = JsonMetrics::from(&u.metrics);
        if service == "syno_reclaim" {
            metrics.reclaimable = None;
            metrics.reclaimable_resolved = 0;
        }
        ServiceLine {
            service,
            names,
            package,
            manage_in,
            manage_in_id,
            explanation: u.service.and_then(|service| service.explanation),
            confirmed,
            path: u.path.as_deref().map(JsonPath::new),
            paths: u.paths,
            metrics,
            snapshots_not_read: u.snapshots_not_read,
            included_in_rows: u.included_in_rows,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ServicesJson {
    /// Largest footprint first.
    pub lines: Vec<ServiceLine>,
    /// Snapshots below the target that are in no line.
    pub other_snapshots_not_read: u64,
}

impl From<&ServiceSummary> for ServicesJson {
    fn from(s: &ServiceSummary) -> Self {
        ServicesJson {
            lines: s.uses.iter().map(ServiceLine::from).collect(),
            other_snapshots_not_read: s.other_snapshots_not_read,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub requested_by: &'static str,
    pub ranked_by: &'static str,
    pub kind: &'static str,
    pub limit: usize,
    pub include_system: bool,
    pub results: Vec<TopResult>,
    pub scans: Vec<FilesystemScan>,
    /// The human columns after `PATH`; JSON has every value.
    #[serde(skip)]
    pub columns: Vec<Column>,
    /// Print each row's hint and command under it.
    #[serde(skip)]
    pub explain: bool,
}

/// A human column of `top` after `PATH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Column {
    Size,
    Physical,
    CanFree,
    Shared,
    HeldBy,
    WhereToFix,
}

impl Column {
    pub const DEFAULT: [Column; 5] = [
        Column::Size,
        Column::Physical,
        Column::CanFree,
        Column::HeldBy,
        Column::WhereToFix,
    ];

    fn title(self) -> &'static str {
        match self {
            Column::Size => "SIZE",
            Column::Physical => "PHYSICAL",
            Column::CanFree => "CAN FREE",
            Column::Shared => "SHARED",
            Column::HeldBy => "HELD BY",
            Column::WhereToFix => "WHERE TO FIX",
        }
    }

    fn align(self) -> Align {
        match self {
            Column::HeldBy | Column::WhereToFix => Align::Left,
            _ => Align::Right,
        }
    }

    fn cell(self, row: &TopRow) -> String {
        let entry = &row.entry;
        let m = entry
            .without_package_data
            .as_ref()
            .unwrap_or(&entry.metrics);
        match self {
            Column::Size => human_bytes(m.logical),
            Column::Physical => human_bytes(m.physical),
            Column::CanFree if row.in_package() => format!("{}*", can_free(m)),
            Column::CanFree => can_free(m),
            Column::Shared => human_bytes(m.shared),
            Column::HeldBy => row.held_text.clone(),
            Column::WhereToFix => row.where_to_fix.label.to_string(),
        }
    }
}

/// An entry of `shared` (SPEC 5.8).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SharedEntry {
    #[serde(flatten)]
    pub entry: EntryJson,
    /// What shares the footprint; see [`sharing_class`].
    pub sharing: &'static str,
    /// `metrics.shared / metrics.physical`, 0 without a footprint.
    pub shared_ratio: f64,
}

impl From<&Entry> for SharedEntry {
    fn from(e: &Entry) -> Self {
        let ratio = crate::query::Measure::SharedRatio.of(&e.metrics);
        SharedEntry {
            entry: EntryJson::from(e),
            sharing: sharing_class(e.blockers),
            shared_ratio: ratio as f64 / 1_000_000.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SharedResult {
    pub report: ReportInfo,
    pub target: SharedEntry,
    pub entries: Vec<SharedEntry>,
    pub hidden_system: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SharedDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub ranked_by: &'static str,
    pub kind: &'static str,
    pub limit: usize,
    pub include_system: bool,
    pub results: Vec<SharedResult>,
    pub scans: Vec<FilesystemScan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectResult {
    pub report: ReportInfo,
    pub entry: EntryJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<Vec<Notice>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub results: Vec<InspectResult>,
    pub scans: Vec<FilesystemScan>,
}

/// Retained bytes of one blocker class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockerBytes {
    pub blocker: &'static str,
    pub bytes: u64,
}

/// Deleting a selection together (SPEC 5.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SimulateResult {
    pub report: ReportInfo,
    /// Each selected path, with its values when deleted alone.
    pub selection: Vec<EntryJson>,
    /// The whole selection deleted together.
    pub combined: JsonMetrics,
    /// Footprint bytes kept by references outside the selection.
    pub retained: u64,
    /// Resolved bytes freed only because the paths go together.
    pub together_bonus: u64,
    pub blockers: Vec<BlockerBytes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<Vec<Notice>>,
    /// What still holds the data after the delete, and when the freed
    /// space shows (SPEC 5.6).
    pub after_delete: Vec<Notice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_snapshots: Option<WithSnapshotsJson>,
}

/// Snapshot roots deleted together with a selection's paths (SPEC 5.14
/// mixed sets): one combined result, never a sum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WithSnapshotsJson {
    pub snapshots: Vec<JsonPath>,
    pub snapshot_ids: Vec<u64>,
    /// Bytes deleting the paths and the snapshots together frees.
    pub freed_bytes: u64,
    /// Bytes that may be freed too but could not be proven.
    pub unknown_bytes: u64,
    /// Extents whose references could not be resolved.
    pub unknown_extents: u64,
    /// Other snapshots of these shares that were not read: extents they
    /// may reference are unknown.
    pub snapshots_not_materialized: u64,
    pub manage_in: String,
    /// Stable ids of the DSM places `manage_in` names (GAP-14); several
    /// when the snapshots live in more than one place.
    pub manage_in_ids: Vec<&'static str>,
}

impl WithSnapshotsJson {
    /// Whether some value is unknown: exit 3 (SPEC 5.12).
    pub fn has_unknown(&self) -> bool {
        self.unknown_bytes > 0 || self.unknown_extents > 0 || self.snapshots_not_materialized > 0
    }
}

/// `snapshots` for one target (SPEC 5.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotsResult {
    #[serde(flatten)]
    pub target: JsonPath,
    pub report: ReportInfo,
    pub shares: Vec<ShareJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_analysis: Option<crate::snapshots::AnalysisMeasurement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotsDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub results: Vec<SnapshotsResult>,
}

/// One real shared-folder Recycle Bin (SPEC 5.16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecycleShareJson {
    pub share: JsonPath,
    pub items: u64,
    pub bytes: u64,
    /// Null when unresolved references prevent an exact value.
    pub reclaimable_bytes: Option<u64>,
    pub held_by: Vec<HeldJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest: Option<Vec<RecycleOldestJson>>,
    /// The newer share report this row comes from, when it is not the
    /// answer's own `report_info` (a share scanned on its own after the
    /// volume scan, SPEC 5.16).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_info: Option<ReportInfo>,
    #[serde(skip)]
    pub held_text: String,
}

impl RecycleShareJson {
    pub fn from_bin(bin: &crate::recycle::RecycleBin) -> Self {
        let (held_by, held_text) = held_json(&bin.entry.retained(), bin.entry.metrics.unknown);
        Self {
            share: JsonPath::new(&bin.share),
            items: bin.entry.metrics.entries.saturating_sub(1),
            bytes: bin.entry.metrics.physical,
            reclaimable_bytes: (bin.entry.metrics.unknown == 0)
                .then_some(bin.entry.metrics.reclaimable),
            held_by,
            oldest: None,
            report_info: None,
            held_text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecycleOldestJson {
    pub tree: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode_device: Option<u64>,
    pub objectid: u64,
    pub path: JsonPath,
    pub ctime: Option<String>,
    pub bytes: u64,
    pub reclaimable_bytes: Option<u64>,
}

impl RecycleOldestJson {
    pub(crate) fn from_item(
        item: &crate::recycle::OldestItem,
        filesystem: crate::filesystem::FilesystemKind,
    ) -> Self {
        Self {
            tree: (filesystem == crate::filesystem::FilesystemKind::Btrfs).then_some(item.tree),
            inode_device: (filesystem == crate::filesystem::FilesystemKind::Ext4)
                .then_some(item.tree),
            objectid: item.objectid,
            path: JsonPath::new(&item.path),
            ctime: item
                .ctime
                .map(|ctime| rfc3339(UNIX_EPOCH + Duration::from_secs(ctime))),
            bytes: item.bytes,
            reclaimable_bytes: item.reclaimable_bytes,
        }
    }
}

/// What Reclaim moved into one shared folder's Recycle Bin after the
/// answering report's scan, so not yet in its size (SPEC 5.16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MovedByReclaimJson {
    pub share: JsonPath,
    pub items: u64,
    pub bytes: u64,
    /// When the latest of those moves ran.
    pub last_moved: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecycleDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub target: JsonPath,
    pub report_info: Option<ReportInfo>,
    pub shares: Vec<RecycleShareJson>,
    /// Moves into these Recycle Bins since the answering report's scan.
    pub moved_by_reclaim: Vec<MovedByReclaimJson>,
    pub scans: Vec<FilesystemScan>,
}

pub fn render_recycle(doc: &RecycleDocument) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}: report ", escape_path(&doc.target)));
    if let Some(report) = &doc.report_info {
        out.push_str(&report_line(report));
    } else {
        out.push_str("not available");
    }
    out.push('\n');
    for share in &doc.shares {
        if let Some(report) = &share.report_info {
            out.push_str(&format!(
                "{}: newer share report {}\n",
                escape_path(&share.share),
                report_line(report)
            ));
        }
    }
    out.push_str(&table(
        &[
            "RECYCLE BIN (#recycle)",
            "ITEMS",
            "SIZE",
            "CAN FREE",
            "OLDEST",
            "HELD BY",
        ],
        &doc.shares
            .iter()
            .map(|share| {
                let can_free = share
                    .reclaimable_bytes
                    .map_or_else(|| "unknown".to_string(), human_bytes);
                vec![
                    escape_path(&share.share),
                    human_count(share.items),
                    human_bytes(share.bytes),
                    can_free,
                    share
                        .oldest
                        .as_ref()
                        .and_then(|items| items.first())
                        .and_then(|item| item.ctime.as_deref())
                        .unwrap_or("—")
                        .to_string(),
                    share.held_text.clone(),
                ]
            })
            .collect::<Vec<_>>(),
    ));
    let oldest_rows: Vec<Vec<String>> = doc
        .shares
        .iter()
        .flat_map(|share| {
            share.oldest.iter().flatten().map(move |item| {
                vec![
                    escape_path(&share.share),
                    escape_path(&item.path),
                    item.ctime.as_deref().unwrap_or("unknown").to_string(),
                    human_bytes(item.bytes),
                    item.reclaimable_bytes
                        .map_or_else(|| "unknown".to_string(), human_bytes),
                ]
            })
        })
        .collect();
    if !oldest_rows.is_empty() {
        out.push_str("\nOLDEST ITEMS\n");
        out.push_str(&table(
            &["SHARE", "PATH", "CTIME", "SIZE", "CAN FREE"],
            &oldest_rows,
        ));
    }
    for moved in &doc.moved_by_reclaim {
        out.push_str(&format!(
            "Moved by Reclaim into {} since the scan: {} item(s), {} (not in the sizes above yet)\n",
            escape_path(&moved.share),
            human_count(moved.items),
            human_bytes(moved.bytes)
        ));
    }
    out.push_str(
        "Emptying in DSM: Control Panel > Shared Folder > Action > Empty Recycle Bin, or a Recycle Bin task with a retention policy in Task Scheduler.\n",
    );
    notices(&mut out, &doc.envelope);
    out
}

pub fn render_snapshots(doc: &SnapshotsDocument) -> String {
    let mut out = String::new();
    for result in &doc.results {
        for share in &result.shares {
            render_share(&mut out, share, &result.report);
        }
    }
    notices(&mut out, &doc.envelope);
    out
}

fn render_share(out: &mut String, share: &ShareJson, report: &ReportInfo) {
    out.push_str(&format!(
        "{}  {} snapshots, report {}\n\n",
        escape_path(&share.share),
        share.snapshots.len(),
        report_line(report)
    ));
    let manager = if share.manage_in == SAN_MANAGER {
        "SAN Manager"
    } else {
        "Snapshot Replication"
    };
    let snapshot_header = format!("SNAPSHOT ({manager})");
    let oldest = share.snapshots.len().saturating_sub(1);
    let rows: Vec<Vec<String>> = share
        .snapshots
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mark = if i == oldest && i > 0 {
                "  (oldest)"
            } else {
                ""
            };
            vec![
                format!("{}{mark}", escape_path(&s.name)),
                s.otime.as_deref().map_or("-".into(), taken),
                exclusive_cell(s),
                s.shared_with_live_bytes
                    .map_or("not read".into(), human_bytes),
                s.qgroup_exclusive_bytes.map_or("—".into(), human_bytes),
            ]
        })
        .collect();
    out.push_str(&table(
        &[
            &snapshot_header,
            "TAKEN (UTC)",
            "EXCLUSIVE",
            "SHARED W/ LIVE",
            "QGROUP EXCL",
        ],
        &rows,
    ));
    out.push_str(&format!(
        "Held only by snapshots of this share: {} (sum of EXCLUSIVE is {}; the rest is shared between snapshots)\n",
        human_bytes(share.held_only_by_snapshots_bytes),
        human_bytes(share.exclusive_sum())
    ));
    if share.unknown_bytes > 0 {
        out.push_str(&format!(
            "Unknown: {} whose other owners could not be ruled out\n",
            human_bytes(share.unknown_bytes)
        ));
    }
    match &share.held_files {
        Some(held) => render_held_files(out, held),
        None if share.held_only_by_snapshots_bytes > 0 => out.push_str(
            "This report does not name the files; rescan with --snapshots to list them\n",
        ),
        None => {}
    }
    if share.snapshots_not_materialized > 0 {
        out.push_str(&format!(
            "{} older snapshot(s) not read: what they may hold is unknown; scan with `--snapshots all` to read them\n",
            share.snapshots_not_materialized
        ));
    }
    if !share.pending_deletion_ids.is_empty() {
        out.push_str(&format!(
            "{} deleted snapshot(s) await the Btrfs cleaner; their space frees without further action\n",
            share.pending_deletion_ids.len()
        ));
    }
    if let Some(sim) = &share.simulation {
        render_simulation(out, sim);
    }
    out.push_str(&format!(
        "Delete snapshots in DSM: {} > {}.\n\n",
        share.manage_in,
        escape_path(&share.share)
    ));
}

/// `Deleting X (n snapshots) frees Y.` and what it leaves uncounted.
fn render_simulation(out: &mut String, sim: &SimulationJson) {
    out.push_str(&format!(
        "\nDeleting {} ({} snapshots) frees {}{}.\n",
        sim.selector,
        sim.selected_ids.len(),
        human_bytes(sim.freed_bytes),
        plus_unknown(sim.unknown_bytes)
    ));
    if !sim.not_materialized_ids.is_empty() {
        out.push_str(&format!(
            "{} selected snapshot(s) were not read; what they alone hold is not counted\n",
            sim.not_materialized_ids.len()
        ));
    }
    if let Some(upper) = sim.upper_bound_bytes.filter(|_| !sim.exact) {
        out.push_str(&format!("It frees at most {}.\n", human_bytes(upper)));
    }
    if let Some(command) = &sim.exact_command {
        out.push_str(&format!("For the exact figure, run `{command}`.\n"));
    }
}

/// What the paths and the snapshot roots free together.
fn render_with_snapshots(out: &mut String, with: &WithSnapshotsJson) {
    out.push_str(&format!(
        "  with {} snapshot(s) deleted too:\n",
        with.snapshots.len()
    ));
    for path in &with.snapshots {
        out.push_str(&format!("    {}\n", escape_path(path)));
    }
    out.push_str(&format!(
        "  together they free {}{}\n",
        human_bytes(with.freed_bytes),
        plus_unknown(with.unknown_bytes)
    ));
    if with.snapshots_not_materialized > 0 {
        out.push_str(&format!(
            "  {} other snapshot(s) of these shares were not read; what they may share is unknown\n",
            with.snapshots_not_materialized
        ));
    }
    out.push_str(&format!("  Delete snapshots in DSM: {}.\n", with.manage_in));
}

/// `, plus up to 1 MB unknown`, or nothing for no unknown bytes.
fn plus_unknown(bytes: u64) -> String {
    match bytes {
        0 => String::new(),
        n => format!(", plus up to {} unknown", human_bytes(n)),
    }
}

/// `2026-09-27 00:00` from an RFC 3339 time.
fn taken(rfc: &str) -> String {
    rfc.get(..16).unwrap_or(rfc).replace('T', " ")
}

/// The largest files only snapshots hold, and whether the list is whole.
fn render_held_files(out: &mut String, held: &HeldFilesJson) {
    if !held.files.is_empty() {
        let title = format!(
            "FILES HELD ONLY BY SNAPSHOTS (largest {} of {})",
            held.files.len(),
            held.count
        );
        let rows: Vec<Vec<String>> = held
            .files
            .iter()
            .map(|file| {
                vec![
                    held_file_name(file),
                    unknown_cell(file.bytes, file.unknown_bytes),
                    escape_path(&file.snapshot),
                ]
            })
            .collect();
        out.push('\n');
        out.push_str(&table(&[&title, "BYTES", "IN SNAPSHOT"], &rows));
    }
    if !held.complete {
        out.push_str(
            "More files hold snapshot-only data than the scan tallied; rescan with a larger --held-files-budget to list them all\n",
        );
    }
}

/// A held file by its live path, marked deleted or rewritten.
fn held_file_name(file: &HeldFileJson) -> String {
    match &file.live_path {
        Some(path) => {
            let state = if file.in_live { "rewritten" } else { "deleted" };
            format!("{}  ({state})", escape_path(path))
        }
        None => format!("inode {}, name not resolved", file.ino),
    }
}

fn unknown_cell(bytes: u64, unknown: u64) -> String {
    match unknown {
        0 => human_bytes(bytes),
        _ => format!("{}+?", human_bytes(bytes)),
    }
}

fn exclusive_cell(s: &SnapshotJson) -> String {
    match (s.exclusive_bytes, s.unknown_bytes) {
        (None, _) => "not read".into(),
        (Some(bytes), unknown) => unknown_cell(bytes, unknown.unwrap_or(0)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SimulateDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub results: Vec<SimulateResult>,
    /// Snapshot roots deleted together, one set per filesystem (SPEC 5.14).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub snapshot_sets: Vec<SnapshotSetResult>,
    pub scans: Vec<FilesystemScan>,
}

/// `simulate-delete` of snapshot roots, answered from a report of a scan
/// with `--snapshots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotSetResult {
    pub report: ReportInfo,
    pub snapshots: Vec<JsonPath>,
    pub simulation: SimulationJson,
    pub manage_in: String,
    /// Stable ids of the DSM places `manage_in` names (GAP-14).
    pub manage_in_ids: Vec<&'static str>,
}

/// Retained bytes per blocker class (an extent counts in each of its
/// classes), then unresolved bytes as `unknown`.
pub fn blocker_summary(header: &Header) -> Vec<BlockerBytes> {
    let mut out: Vec<BlockerBytes> = (0..7)
        .filter(|&i| header.accounting.class_bytes[i] > 0)
        .map(|i| BlockerBytes {
            blocker: Blockers::from_bits(1 << i).names()[0],
            bytes: header.accounting.class_bytes[i],
        })
        .collect();
    let unknown = header.accounting.class_bytes[7] + header.totals.unknown;
    if unknown > 0 {
        out.push(BlockerBytes {
            blocker: "unknown",
            bytes: unknown,
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeInfo {
    pub filesystem: crate::filesystem::FilesystemKind,
    pub capabilities: crate::filesystem::Capabilities,
    pub mount: JsonPath,
    pub fsid: Option<String>,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub metadata_access: bool,
    pub reports: usize,
    /// Reports of this filesystem that cannot be read: damaged, or from
    /// another report format.
    pub incompatible_reports: usize,
    pub last_scan: Option<String>,
    pub last_consistency: Option<Consistency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attribution: Option<crate::overview::Attribution>,
    /// `None` when subvolume discovery failed.
    pub pending_deletion: Option<PendingDeletion>,
    /// The volume's shared-folder roots (SPEC 5.2).
    pub shares: Vec<VolumeShare>,
    pub notes: Vec<Notice>,
}

/// One shared-folder root of a volume (SPEC 5.2): a writable subvolume
/// linked directly below the volume root with a user name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeShare {
    /// Its name below the volume root.
    pub name: JsonPath,
    /// Its path below the volume mount.
    pub path: JsonPath,
    pub subvolume_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
    /// Physical bytes of the share's live namespace in the newest volume
    /// report; `None` when no volume report exists. No scan is run to
    /// fill it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

impl VolumeInfo {
    /// Counts `listing`'s reports and notes the unreadable ones, whose
    /// volume looks unscanned until it is scanned again.
    pub fn add_reports(&mut self, listing: &crate::state::Listing) {
        self.reports = listing.reports.len();
        self.incompatible_reports = listing.unreadable.len();
        if let Some((_, h)) = listing.reports.first() {
            self.last_scan = Some(rfc3339(
                SystemTime::UNIX_EPOCH + Duration::from_secs(h.scan_finished),
            ));
            self.last_consistency = Some(h.consistency);
            self.last_attribution = crate::overview::Attribution::of(h);
        }
        if self.incompatible_reports > 0 {
            self.notes.push(Notice::new(
                "report_incompatible",
                format!(
                    "{} reports cannot be read (damaged, or from an older synoreclaim); \
                     scan again",
                    self.incompatible_reports
                ),
            ));
        }
    }
}

/// Deleted subvolumes the Btrfs cleaner has not freed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingDeletion {
    pub count: usize,
    /// Always `None`: not known before the cleaner frees it (SPEC 5.2).
    pub bytes: Option<u64>,
}

/// What `cache status` shows of one filesystem's cache (SPEC 5.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheStatus {
    pub format: u32,
    pub fsid: String,
    /// Bytes of the units the manifest names.
    pub bytes: u64,
    /// By subvolume.
    pub units: Vec<CacheUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheUnit {
    pub subvolume: u64,
    pub uuid: String,
    /// Which scope of the subvolume the unit holds, in hex.
    pub slot: String,
    pub generation: u64,
    pub readonly: bool,
    /// Unix seconds when its namespace was read.
    pub captured: u64,
    pub bytes: u64,
    /// Whether a scan at the time asked may reuse it instead of reading.
    pub reusable: bool,
}

/// The answer to `pause` / `cancel --json` (SPEC 22.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobRequestDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    /// `requested`, `cancelled` (a stopped job ended at once), or a
    /// refusal: `too_late`, `not_running`, `finished`, `no_job`, `busy`,
    /// `not_pausable` (a cleanup job).
    pub outcome: &'static str,
    pub message: String,
    /// The job's state after the request; null when there is no such job.
    pub state: Option<&'static str>,
}

/// A filesystem a job's targets resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobFilesystemInfo {
    pub fsid: String,
    pub mount: JsonPath,
}

/// One scan job (SPEC 22.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobInfo {
    pub job_id: String,
    /// `scan` or `cleanup-apply` (SPEC 5.7).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_s: Option<u64>,
    pub state: &'static str,
    pub phase: String,
    pub targets: Vec<JsonPath>,
    pub scope: Option<Scope>,
    /// What the targets resolved to in the latest run; empty before.
    pub filesystems: Vec<JobFilesystemInfo>,
    pub profile: &'static str,
    pub created: String,
    pub updated: String,
    pub resumes: u32,
    /// Units scanned or reused by the latest run.
    pub units: u64,
    /// Live progress of a running scan; absent when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<JobProgress>,
    /// Live progress of a cleanup job; absent for a scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<CleanupProgress>,
    /// Live progress of a rootfs apply job; absent for scans and cleanup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rootfs: Option<RootfsProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rootfs_plan: Option<crate::progress::RootfsPlanDetails>,
    /// Why the latest run stopped.
    pub reason: Option<String>,
    /// The binary that started the job; empty for an older one.
    pub tool_version: String,
    /// Whether this binary can resume the job (same tool and formats).
    pub compatible: bool,
    /// `pause` or `cancel` while a running job has one pending.
    pub request: Option<&'static str>,
    /// A request that came too late: the latest run finished anyway.
    pub request_ignored: Option<&'static str>,
    /// How the latest run ended; `None` before one has.
    pub result: Option<JobResultInfo>,
    /// The DSM user a `webapi` call started the job for; null otherwise.
    pub requested_by: Option<String>,
}

/// The one line `scan --detach` prints (SPEC 5.3): the queued job, or
/// why none was queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetachDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    /// `self` or `none`.
    pub launcher: &'static str,
    /// `scan_in_progress` when NAS-wide admission refused this request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'static str>,
    /// The existing scan to follow; no new job was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_job_id: Option<String>,
    /// What runs the job in the foreground: `scan --job-id`.
    pub run: Vec<String>,
    /// Where its whole scan document will be once it has run.
    pub result: Option<JsonPath>,
    /// Where the output of its own process goes, for `self`.
    pub log: Option<JsonPath>,
}

impl DetachDocument {
    pub fn new(envelope: Envelope, launcher: crate::cli::Launcher) -> Self {
        DetachDocument {
            envelope,
            launcher: match launcher {
                crate::cli::Launcher::Process => "self",
                crate::cli::Launcher::External => "none",
            },
            outcome: None,
            running_job_id: None,
            run: Vec::new(),
            result: None,
            log: None,
        }
    }
}

/// How a job's latest run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobResultInfo {
    pub completion: String,
    pub exit_code: i32,
    /// The reports it published.
    pub reports: Vec<JsonPath>,
    /// The distinct codes of its errors.
    pub error_codes: Vec<String>,
    /// Its whole scan document.
    pub document: JsonPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobsDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub jobs: Vec<JobInfo>,
}

/// What `history` shows (SPEC 5.20): one entry per completed scan or
/// finished job, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub scans: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    pub job_id: String,
    /// Filesystem identity from the history filename; absent for job-only entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fsid: Option<String>,
    /// Completed or partial before superseding; absent for legacy records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_outcome: Option<&'static str>,
    /// The scan's roots: a volume mount or share paths.
    pub targets: Vec<JsonPath>,
    pub volume: bool,
    /// The resource profile the scan ran with; null when not recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub started: String,
    pub finished: String,
    /// This run's elapsed time.
    pub elapsed_ms: u64,
    /// All runs of the job so far, earlier resumes included.
    pub total_elapsed_ms: u64,
    /// Namespace entries read (the report's entry count).
    pub entries: u64,
    pub subvolumes: u64,
    /// Units reused from the cache by this run.
    pub reused_units: u64,
    /// The last full read of this scope, what an ETA should use; null
    /// when no full read is recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_read_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_read_entries: Option<u64>,
    /// `completed`, `partial`, `cancelled`, `failed`, `paused` or
    /// `superseded`.
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    /// Largest size the scan's spill directory reached.
    pub spill_peak_bytes: u64,
    /// The published report, while its file still exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<JsonPath>,
    /// Bytes of the published report.
    pub report_bytes: Option<u64>,
    /// Filesystem bytes around the scan (statvfs, as `volumes` reports
    /// them); null when the scan could not read them.
    pub df_total_bytes: Option<u64>,
    pub df_used_bytes: Option<u64>,
    pub df_free_bytes: Option<u64>,
    /// Btrfs DATA block-group bytes in use at the scan's end; null when
    /// unavailable. The overview reconciliation number.
    pub data_used_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<crate::overview::Attribution>,
}

/// The global index is derived from the units on every scan and is not
/// part of the cache (SPEC 19 Phase 3).
pub const GLOBAL_INDEX: &str = "rebuilt_each_scan";

/// One filesystem of `cache status` or `cache clear`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheInfo {
    pub mount: JsonPath,
    pub completion: Completion,
    /// What remains after the action.
    #[serde(flatten)]
    pub status: CacheStatus,
    pub global_index: &'static str,
    /// Units `cache clear` removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleared: Option<usize>,
    pub errors: Vec<TargetError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    /// `status` or `clear`.
    pub action: &'static str,
    pub filesystems: Vec<CacheInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumesDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub volumes: Vec<VolumeInfo>,
    pub excluded: Vec<TargetError>,
    /// DSM's own partition, listed apart from the data volumes.
    pub system_partition: Option<SystemPartition>,
}

/// Lines explaining `inspect` values (SPEC 5.5 `--explain`).
pub fn explain(entry: &Entry, header: &Header) -> Vec<Notice> {
    let mut notes: Vec<Notice> = entry
        .blockers
        .names()
        .into_iter()
        .map(|name| Notice::new(name, blocker_message(name)))
        .collect();
    let m = &entry.metrics;
    if m.unknown > 0 {
        notes.push(Notice::new(
            "unresolved_references",
            format!(
                "{} have references that could not be resolved; they are not counted as reclaimable",
                human_bytes(m.unknown)
            ),
        ));
    }
    if m.pinned > 0 {
        notes.push(Notice::new(
            "pinned_unreferenced",
            format!(
                "{} of reclaimable extents are referenced by no file range (bookends); deletion frees them too",
                human_bytes(m.pinned)
            ),
        ));
    }
    if entry.nlink > 1 {
        notes.push(Notice::new(
            "hard_links",
            format!(
                "{} links: logical size and extents count once; links outside the report scope keep the data",
                entry.nlink
            ),
        ));
    }
    if header.coverage.snapshots_not_materialized > 0 {
        notes.push(Notice::new(
            "snapshots_not_materialized",
            format!(
                "{} snapshot(s) in scope were not read; data they share is not visible here",
                header.coverage.snapshots_not_materialized
            ),
        ));
    }
    if entry.flags.contains(NodeFlags::ENCRYPTED) {
        notes.push(Notice::new(
            "unsupported_encrypted",
            "eCryptfs lower data: bytes counted, names not shown",
        ));
    }
    if entry.flags.contains(NodeFlags::SYSTEM) {
        notes.push(Notice::new("system", "DSM-managed; not a cleanup target"));
    }
    if let Some(service) = crate::dsm::service_of(&entry.path, &header.mount) {
        notes.push(Notice::new("dsm_service", service.note()));
    }
    if header.coverage.reference_accounting != "complete" {
        notes.push(Notice::new(
            "reference_accounting_partial",
            format!(
                "{} extents in the report have unresolved references",
                header.coverage.unresolved_extents
            ),
        ));
    }
    if header.consistency != Consistency::ObservedStable {
        notes.push(Notice::new(
            "consistency",
            format!(
                "report is {}",
                consistency_text(header.consistency, &header.change_reasons)
            ),
        ));
    }
    notes
}

/// What shares an entry's footprint (SPEC 5.8): `snapshot`, `live`,
/// `hard_link`, `mixed`, `unknown`, or `none`. Unresolved references
/// count only when nothing else is known.
pub fn sharing_class(blockers: Blockers) -> &'static str {
    let classes = [
        (
            Blockers::SNAPSHOT
                .with(Blockers::SHARED_TREE)
                .with(Blockers::PENDING_DELETION),
            "snapshot",
        ),
        (
            Blockers::LIVE_REFLINK.with(Blockers::OTHER_SUBVOLUME),
            "live",
        ),
        (Blockers::HARD_LINK, "hard_link"),
    ];
    let found: Vec<&str> = classes
        .iter()
        .filter(|(bits, _)| blockers.bits() & bits.bits() != 0)
        .map(|(_, name)| *name)
        .collect();
    match found[..] {
        [] if blockers.is_empty() => "none",
        [] => "unknown",
        [one] => one,
        _ => "mixed",
    }
}

/// Why a blocker keeps bytes allocated (SPEC 3 external blockers).
pub fn blocker_message(name: &str) -> &'static str {
    match name {
        "snapshot" => "a snapshot references some extents",
        "live_reflink" => "another live file (reflink or clone) references some extents",
        "hard_link" => "an inode keeps a hard link outside the selection",
        "other_subvolume" => "another subvolume references some extents",
        "shared_tree" => {
            "some extents are referenced through a tree block shared by snapshotting (conservative)"
        }
        "pending_subvolume_deletion" => {
            "a deleted subvolume awaiting the Btrfs cleaner holds some extents; they free later"
        }
        "orphan_inode" => {
            "an inode without names (unlinked but open, or orphan) references some extents"
        }
        _ => "some references could not be resolved",
    }
}

fn notices(out: &mut String, envelope: &Envelope) {
    for e in &envelope.errors {
        out.push_str(&format!(
            "error: {}: {} ({})\n",
            escape_path(&e.target),
            e.message,
            e.code
        ));
    }
    for w in &envelope.warnings {
        out.push_str(&format!("warning: {} ({})\n", w.message, w.code));
    }
    for hint in &envelope.scan_hints {
        out.push_str(&hint.human());
        out.push('\n');
    }
}

/// `left − right` as a signed count.
pub(crate) fn signed_delta(left: u64, right: u64) -> i64 {
    if left >= right {
        i64::try_from(left - right).unwrap_or(i64::MAX)
    } else {
        i64::try_from(right - left)
            .unwrap_or(i64::MAX)
            .saturating_neg()
    }
}

/// Bytes with a sign: `+1 MB`, `−2 GB`.
fn signed_bytes(bytes: i64) -> String {
    format!(
        "{}{}",
        if bytes >= 0 { "+" } else { "−" },
        human_bytes(bytes.unsigned_abs())
    )
}

fn escape_path(p: &JsonPath) -> String {
    escape_human(&p.bytes())
}

/// Reclaimable bytes for humans: exact, a resolved part with `+?` for
/// unresolved references, or `unknown` (SPEC 3).
fn can_free(m: &JsonMetrics) -> String {
    match m.reclaimable {
        Some(exact) => human_bytes(exact),
        None if m.reclaimable_resolved == 0 => "unknown".to_string(),
        None => format!("{}+?", human_bytes(m.reclaimable_resolved)),
    }
}

pub fn render_filesystem(out: &mut String, fs: &FilesystemScan) {
    out.push_str(&format!(
        "filesystem {} ({}): {}{}\n",
        fs.fsid,
        escape_path(&fs.mount),
        fs.completion.name(),
        fs.consistency
            .map_or(String::new(), |c| format!(", {}", c.name()))
    ));
    for t in &fs.targets {
        let scope = format!("{:?}", t.scope).to_lowercase();
        out.push_str(&format!("  target {} ({scope})\n", escape_path(&t.path)));
    }
    for c in &fs.covered {
        out.push_str(&format!(
            "  skipped {}: inside {}\n",
            escape_path(&c.path),
            escape_path(&c.covered_by)
        ));
    }
    if let Some(stats) = &fs.stats {
        let changed: Vec<String> = fs.change_reasons.iter().map(ToString::to_string).collect();
        out.push_str(&format!(
            "  subvolumes: {} discovered, {} scanned, {} reused, {} snapshots not read\n",
            stats.subvolumes_discovered,
            stats.subvolumes_scanned,
            stats.subvolumes_reused,
            fs.coverage
                .as_ref()
                .map_or(0, |c| c.snapshots_not_materialized)
        ));
        out.push_str(&format!(
            "  entries {}, inodes {}, extents {}, changed during scan: {}\n",
            stats.entries,
            stats.inodes,
            stats.extents,
            if changed.is_empty() {
                "none".to_string()
            } else {
                changed.join("; ")
            }
        ));
    }
    if let Some(stop) = &fs.resource_limit {
        out.push_str(&stop.line());
    }
    if let (Some(stats), Some(totals)) = (&fs.stats, &fs.totals) {
        out.push_str(&format!(
            "  logical {}, physical {}, can free {}, elapsed {:.1}s\n",
            human_bytes(totals.logical),
            human_bytes(totals.physical),
            can_free(totals),
            stats.elapsed_ms as f64 / 1000.0
        ));
    }
    if let Some(report) = &fs.report {
        out.push_str(&format!(
            "  report {} ({})\n",
            escape_path(&report.file),
            human_bytes(report.bytes)
        ));
    }
    filesystem_notices(out, &fs.errors, &fs.warnings);
}

/// A filesystem's own errors and warnings, indented below it.
fn filesystem_notices(out: &mut String, errors: &[TargetError], warnings: &[Notice]) {
    for e in errors {
        out.push_str(&format!(
            "  error: {}: {} ({})\n",
            escape_path(&e.target),
            e.message,
            e.code
        ));
    }
    for w in warnings {
        out.push_str(&format!("  warning: {} ({})\n", w.message, w.code));
    }
}

pub fn render_scan(doc: &ScanDocument) -> String {
    let mut out = format!(
        "job {} ({}, profile {})\n",
        doc.envelope.job_id.as_deref().unwrap_or("-"),
        doc.envelope.completion.name(),
        doc.resources.scheduling.profile.name()
    );
    for fs in &doc.filesystems {
        render_filesystem(&mut out, fs);
    }
    notices(&mut out, &doc.envelope);
    out
}

/// The note under the table for data a ranked folder leaves out: a
/// Recycle Bin ranks as its own row, package data in its service's row.
fn overlap_note(overlap: &PackageOverlap) -> String {
    let (data, row) = match &overlap.service_path {
        Some(path) if overlap.service.ranks_contents => {
            ("Recycle Bin data", escape_path(&JsonPath::new(path)))
        }
        _ => ("package data", overlap.service.package.to_string()),
    };
    format!(
        "  {} leaves out {} of {data}, counted in the {row} row\n",
        escape_path(&JsonPath::new(&overlap.target)),
        human_bytes(overlap.bytes),
    )
}

fn report_line(report: &ReportInfo) -> String {
    format!(
        "{} ({} ago{}), {}",
        report.scan_finished,
        human_age(Duration::from_secs(report.age_seconds)),
        if report.stale { ", stale" } else { "" },
        consistency_text(report.consistency, &report.change_reasons)
    )
}

pub fn render_top(doc: &TopDocument) -> String {
    let mut out = String::new();
    for result in &doc.results {
        out.push_str(&format!(
            "{}: report {}\n",
            escape_path(&result.target.path),
            report_line(&result.report)
        ));
        out.push_str(&top_rows(&result.entries, &doc.columns, doc.explain));
        let shown = |target: &[u8]| {
            let target = JsonPath::new(target);
            result.entries.iter().any(|row| row.entry.path == target)
        };
        for overlap in result
            .package_overlaps
            .iter()
            .filter(|o| o.bytes > 0 && shown(&o.target))
        {
            out.push_str(&overlap_note(overlap));
        }
        if result.hidden_system > 0 {
            out.push_str(&format!(
                "({} detail rows of DSM service data hidden; --include-system lists them)\n",
                result.hidden_system
            ));
        }
        render_services(&mut out, &result.services);
    }
    for fs in &doc.scans {
        render_filesystem(&mut out, fs);
    }
    notices(&mut out, &doc.envelope);
    out
}

/// The ranked rows in `columns`, each followed by its hint and command
/// when `explain`, then the footnote of package data and the package
/// data lines.
fn top_rows(rows: &[TopRow], columns: &[Column], explain: bool) -> String {
    let mut header = vec!["PATH"];
    header.extend(columns.iter().map(|c| c.title()));
    let mut aligns = vec![Align::Left];
    aligns.extend(columns.iter().map(|c| c.align()));
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut cells = vec![row.label()];
            cells.extend(columns.iter().map(|c| c.cell(row)));
            cells
        })
        .collect();
    let text = aligned_table(&header, &aligns, &cells);
    let mut lines = text.lines();
    let mut out = format!("{}\n", lines.next().unwrap_or_default());
    for (line, row) in lines.zip(rows) {
        out.push_str(line);
        out.push('\n');
        if explain {
            let fix = &row.where_to_fix;
            out.push_str(&format!("  {}\n", fix.hint));
            if let Some(argv) = &fix.command {
                let words: Vec<String> = argv.iter().map(|w| shell_word(w.as_bytes())).collect();
                out.push_str(&format!("  $ {}\n", words.join(" ")));
            }
        }
    }
    if columns.contains(&Column::CanFree) && rows.iter().any(TopRow::in_package) {
        out.push_str(
            "* frees only through the managing package; synoreclaim cleanup refuses service data.\n",
        );
    }
    package_lines(&mut out, rows.iter().map(|row| &row.entry));
    out
}

/// The package data each entry is, lies in or holds.
fn package_lines<'e>(out: &mut String, entries: impl Iterator<Item = &'e EntryJson>) {
    for e in entries {
        if let Some(data) = &e.package_data {
            out.push_str(&format!("  {}: {}\n", escape_path(&e.path), data.message));
        }
    }
}

/// The DSM service data summary: one row per line, then where to
/// manage each package's data.
fn render_services(out: &mut String, services: &ServicesJson) {
    if services.lines.is_empty() && services.other_snapshots_not_read == 0 {
        return;
    }
    out.push_str("DSM service data (each path counted alone):\n");
    let rows: Vec<Vec<String>> = services
        .lines
        .iter()
        .map(|line| {
            let m = &line.metrics;
            vec![
                service_item(line),
                human_bytes(m.physical),
                can_free(m),
                human_bytes(m.shared),
                human_bytes(m.unknown),
                if line.included_in_rows {
                    format!("{} (included in folder rows; do not add)", line.package)
                } else {
                    line.package.to_string()
                },
            ]
        })
        .collect();
    out.push_str(&table(
        &[
            "ITEM", "PHYSICAL", "CAN FREE", "RETAINED", "UNKNOWN", "DATA",
        ],
        &rows,
    ));
    for line in services.lines.iter().filter(|l| l.snapshots_not_read > 0) {
        out.push_str(&format!(
            "  {}: {} snapshots not read; the space only they hold is not known\n",
            service_item(line),
            line.snapshots_not_read
        ));
    }
    if services.other_snapshots_not_read > 0 {
        out.push_str(&format!(
            "  {} more snapshot not read elsewhere below the target\n",
            services.other_snapshots_not_read
        ));
    }
    let mut hinted: Vec<&str> = Vec::new();
    for line in &services.lines {
        if hinted.contains(&line.service) {
            continue;
        }
        hinted.push(line.service);
        let base = crate::dsm::hint(line.package, line.manage_in, line.confirmed);
        let hint = line
            .explanation
            .map_or(base.clone(), |explanation| format!("{base}; {explanation}"));
        out.push_str(&format!("  {}: {hint}\n", line.names));
    }
}

fn service_item(line: &ServiceLine) -> String {
    match &line.path {
        Some(path) => escape_path(path),
        None if line.service == "other_dsm_service_data" => {
            format!("other @ entries ({} paths)", line.paths)
        }
        None => format!("{} ({} paths)", line.names, line.paths),
    }
}

pub fn render_shared(doc: &SharedDocument) -> String {
    let mut out = String::new();
    for result in &doc.results {
        out.push_str(&format!(
            "{}: report {}\n",
            escape_path(&result.target.entry.path),
            report_line(&result.report)
        ));
        let rows: Vec<Vec<String>> = result
            .entries
            .iter()
            .map(|s| {
                let m = &s.entry.metrics;
                vec![
                    escape_path(&s.entry.path),
                    human_bytes(m.logical),
                    human_bytes(m.physical),
                    human_bytes(m.shared),
                    format!("{:.0}%", s.shared_ratio * 100.0),
                    s.sharing.to_string(),
                ]
            })
            .collect();
        out.push_str(&table(
            &["PATH", "SIZE", "PHYSICAL", "SHARED", "RATIO", "SHARED WITH"],
            &rows,
        ));
        if result.hidden_system > 0 {
            out.push_str(&format!(
                "({} DSM-managed entries hidden; --include-system shows them)\n",
                result.hidden_system
            ));
        }
    }
    for fs in &doc.scans {
        render_filesystem(&mut out, fs);
    }
    notices(&mut out, &doc.envelope);
    out
}

pub fn render_inspect(doc: &InspectDocument) -> String {
    let mut out = String::new();
    for result in &doc.results {
        let e = &result.entry;
        out.push_str(&format!("{}\n", escape_path(&e.path)));
        let mut line =
            |label: &str, value: String| out.push_str(&format!("  {label:<12}{value}\n"));
        line("type", e.kind.to_string());
        line("report", report_line(&result.report));
        line("logical", human_bytes(e.metrics.logical));
        line("physical", human_bytes(e.metrics.physical));
        line("shared", human_bytes(e.metrics.shared));
        line("can free", can_free(&e.metrics));
        if let Some(extents) = e.metrics.extents {
            line("extents", extents.to_string());
        }
        if e.kind == "dir" {
            line("files", e.metrics.files.to_string());
            line("entries", e.metrics.entries.to_string());
        }
        if e.nlink > 1 {
            line("hard links", e.nlink.to_string());
        }
        if !e.flags.is_empty() {
            line("flags", e.flags.join(" "));
        }
        note_lines(&mut out, result.explain.iter().flatten());
    }
    for fs in &doc.scans {
        render_filesystem(&mut out, fs);
    }
    notices(&mut out, &doc.envelope);
    out
}

fn entry_rows(entries: &[EntryJson]) -> String {
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            let mut path = escape_path(&e.path);
            if e.flags.contains(&"snapshot_not_read") {
                path.push_str(" (snapshot, not read)");
            }
            vec![
                path,
                human_bytes(e.metrics.logical),
                human_bytes(e.metrics.physical),
                can_free(&e.metrics),
                human_bytes(e.metrics.shared),
            ]
        })
        .collect();
    let mut out = table(&["PATH", "SIZE", "PHYSICAL", "CAN FREE", "SHARED"], &rows);
    package_lines(&mut out, entries.iter());
    out
}

pub fn render_simulate(doc: &SimulateDocument) -> String {
    let mut out = String::new();
    for result in &doc.results {
        out.push_str(&format!(
            "deleting {} paths together: report {}\n",
            result.selection.len(),
            report_line(&result.report)
        ));
        out.push_str(&entry_rows(&result.selection));
        let m = &result.combined;
        let mut line =
            |label: &str, value: String| out.push_str(&format!("  {label:<14} {value}\n"));
        line("selected size", human_bytes(m.logical));
        line("physical", human_bytes(m.physical));
        line("can free", can_free(m));
        line("retained", human_bytes(result.retained));
        if m.unknown > 0 {
            line("unknown", human_bytes(m.unknown));
        }
        line("together bonus", human_bytes(result.together_bonus));
        if !result.blockers.is_empty() {
            let text: Vec<String> = result
                .blockers
                .iter()
                .map(|b| format!("{} {}", b.blocker, human_bytes(b.bytes)))
                .collect();
            line("blockers", text.join(", "));
        }
        if let Some(with) = &result.with_snapshots {
            render_with_snapshots(&mut out, with);
        }
        note_lines(&mut out, result.explain.iter().flatten());
        out.push_str("after the delete:\n");
        note_lines(&mut out, &result.after_delete);
    }
    for set in &doc.snapshot_sets {
        out.push_str(&format!(
            "deleting {} snapshots together: report {}\n",
            set.snapshots.len(),
            report_line(&set.report)
        ));
        for path in &set.snapshots {
            out.push_str(&format!("  {}\n", escape_path(path)));
        }
        render_simulation(&mut out, &set.simulation);
        out.push_str(&format!("Delete snapshots in DSM: {}.\n", set.manage_in));
    }
    for fs in &doc.scans {
        render_filesystem(&mut out, fs);
    }
    notices(&mut out, &doc.envelope);
    out
}

/// `  - message (code)` lines.
fn note_lines<'a>(out: &mut String, notes: impl IntoIterator<Item = &'a Notice>) {
    for note in notes {
        out.push_str(&format!("  - {} ({})\n", note.message, note.code));
    }
}

pub fn render_volumes(doc: &VolumesDocument) -> String {
    let rows: Vec<Vec<String>> = doc
        .volumes
        .iter()
        .map(|v| {
            vec![
                escape_path(&v.mount),
                v.fsid.clone().unwrap_or_else(|| "-".into()),
                v.total_bytes.map_or("-".into(), human_bytes),
                v.used_bytes.map_or("-".into(), human_bytes),
                v.free_bytes.map_or("-".into(), human_bytes),
                if v.metadata_access { "yes" } else { "no" }.into(),
                v.last_scan.clone().unwrap_or_else(|| "never".into()),
            ]
        })
        .collect();
    let mut out = table(
        &[
            "MOUNT",
            "UUID",
            "SIZE",
            "USED",
            "FREE",
            "METADATA",
            "LAST SCAN",
        ],
        &rows,
    );
    for v in &doc.volumes {
        let pending = v.pending_deletion.as_ref().map_or(0, |p| p.count);
        if let Some(text) = crate::release::pending_cleaner(pending) {
            out.push_str(&format!("{}: {text}\n", escape_path(&v.mount)));
        }
    }
    for e in &doc.excluded {
        out.push_str(&format!(
            "excluded: {}: {} ({})\n",
            escape_path(&e.target),
            e.message,
            e.code
        ));
    }
    if let Some(system) = &doc.system_partition {
        out.push_str(&format!(
            "system partition {} (DSM's own, not a data volume)\n",
            system.summary()
        ));
    }
    notices(&mut out, &doc.envelope);
    out
}

pub fn render_jobs(doc: &JobsDocument) -> String {
    if doc.jobs.is_empty() {
        return "no jobs\n".into();
    }
    let rows: Vec<Vec<String>> = doc
        .jobs
        .iter()
        .map(|j| {
            vec![
                j.job_id.clone(),
                j.state.into(),
                job_phase(j),
                j.units.to_string(),
                j.resumes.to_string(),
                j.updated.clone(),
                j.reason.clone().unwrap_or_else(|| "-".into()),
                j.targets
                    .iter()
                    .map(escape_path)
                    .collect::<Vec<_>>()
                    .join(" "),
            ]
        })
        .collect();
    let mut out = table(
        &[
            "JOB", "STATE", "PHASE", "UNITS", "RESUMES", "UPDATED", "REASON", "TARGETS",
        ],
        &rows,
    );
    notices(&mut out, &doc.envelope);
    out
}

/// A job's phase, with a cleanup's items done so far.
fn job_phase(job: &JobInfo) -> String {
    match &job.cleanup {
        Some(c) => format!("{} {}/{} items", job.phase, c.items_done, c.items_total),
        None => job.phase.clone(),
    }
}

pub fn render_history(doc: &HistoryDocument) -> String {
    if doc.scans.is_empty() {
        return "no scans recorded\n".into();
    }
    let rows: Vec<Vec<String>> = doc
        .scans
        .iter()
        .map(|e| {
            vec![
                e.finished.clone(),
                human_duration(Duration::from_millis(e.elapsed_ms)),
                human_count(e.entries),
                e.outcome.to_string(),
                e.profile.clone().unwrap_or_else(|| "-".into()),
                e.peak_rss_bytes
                    .map(human_bytes)
                    .unwrap_or_else(|| "-".into()),
                e.targets
                    .iter()
                    .map(escape_path)
                    .collect::<Vec<_>>()
                    .join(" "),
            ]
        })
        .collect();
    let mut out = table(
        &[
            "FINISHED", "TOOK", "FILES", "OUTCOME", "PROFILE", "PEAK RSS", "TARGETS",
        ],
        &rows,
    );
    notices(&mut out, &doc.envelope);
    out
}

pub fn render_cache(doc: &CacheDocument) -> String {
    let mut out = String::new();
    for fs in &doc.filesystems {
        render_cache_info(&mut out, fs);
    }
    notices(&mut out, &doc.envelope);
    out
}

fn render_cache_info(out: &mut String, fs: &CacheInfo) {
    let status = &fs.status;
    out.push_str(&format!(
        "{} ({}): {} units, {}, cache format {}\n",
        escape_path(&fs.mount),
        status.fsid,
        status.units.len(),
        human_bytes(status.bytes),
        status.format
    ));
    if let Some(cleared) = fs.cleared {
        out.push_str(&format!("cleared {cleared} units\n"));
    }
    if !status.units.is_empty() {
        out.push_str(&cache_rows(&status.units));
    }
    out.push_str("global index: not kept; rebuilt from the units on every scan\n");
    filesystem_notices(out, &fs.errors, &[]);
}

fn cache_rows(units: &[CacheUnit]) -> String {
    let rows: Vec<Vec<String>> = units
        .iter()
        .map(|u| {
            vec![
                u.subvolume.to_string(),
                u.slot.clone(),
                if u.readonly { "read-only" } else { "writable" }.into(),
                u.generation.to_string(),
                rfc3339(UNIX_EPOCH + Duration::from_secs(u.captured)),
                human_bytes(u.bytes),
                if u.reusable { "yes" } else { "no" }.into(),
            ]
        })
        .collect();
    table(
        &[
            "SUBVOLUME",
            "SLOT",
            "KIND",
            "GENERATION",
            "CAPTURED",
            "SIZE",
            "REUSABLE",
        ],
        &rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::source::Maintenance;

    #[test]
    fn shell_words_escape_control_and_invisible_characters() {
        assert_eq!(shell_word(b"/volume1/photos"), "/volume1/photos");
        assert_eq!(shell_word(b"/volume1/my photos"), "'/volume1/my photos'");
        assert_eq!(
            shell_word(b"/volume1/a\x1b[2J\nb's\\"),
            r"$'/volume1/a\x1b[2J\x0ab\'s\\'"
        );
        assert_eq!(
            shell_word("/volume1/x\u{202e}gpj.exe".as_bytes()),
            r"$'/volume1/x\xe2\x80\xaegpj.exe'"
        );
        assert_eq!(
            shell_word("/volume1/caf\u{e9}".as_bytes()),
            "'/volume1/caf\u{e9}'"
        );
        for word in [shell_word(b"\x07\x1b]0;t\x07"), shell_word(b"\xc2\x9b")] {
            assert!(!word.chars().any(unprintable), "{word:?}");
        }
    }

    #[test]
    fn recycle_reports_metrics_and_holders_and_optional_oldest_items() {
        let bin = crate::recycle::RecycleBin {
            share: b"photos".to_vec(),
            path: b"/volume1/photos/#recycle".to_vec(),
            entry: Entry {
                path: b"/volume1/photos/#recycle".to_vec(),
                kind: crate::model::namespace::FileKind::Directory,
                flags: NodeFlags::from_bits(0),
                metrics: crate::model::tree::Metrics {
                    physical: 10,
                    reclaimable: 6,
                    entries: 3,
                    ..Default::default()
                },
                blockers: Blockers::default(),
                held: Held::of(crate::model::accounting::HeldClass::HardLink, 4),
                nlink: 1,
                tree: 257,
                objectid: 300,
                outside: None,
            },
        };
        let share = RecycleShareJson::from_bin(&bin);
        assert_eq!(share.items, 2);
        assert_eq!(share.bytes, 10);
        assert_eq!(share.reclaimable_bytes, Some(6));
        assert_eq!(share.held_by[0].class, "hard_link");

        let mut unknown_bin = bin.clone();
        unknown_bin.entry.metrics.unknown = 1;
        unknown_bin.entry.held = Held::of(crate::model::accounting::HeldClass::HardLink, 3);
        let unknown = RecycleShareJson::from_bin(&unknown_bin);
        assert_eq!(unknown.reclaimable_bytes, None);
        assert_eq!(unknown.held_by.len(), 2);
        assert_eq!(unknown.held_by[0].class, "hard_link");
        assert_eq!(unknown.held_by[1].class, "unknown");

        let doc = RecycleDocument {
            envelope: Envelope::new("recycle", UNIX_EPOCH),
            target: JsonPath::new(b"/volume1"),
            report_info: None,
            shares: vec![share.clone()],
            moved_by_reclaim: Vec::new(),
            scans: Vec::new(),
        };
        let json = serde_json::to_value(&doc).unwrap();
        let share_json = json["shares"][0].as_object().unwrap();
        assert!(!share_json.contains_key("oldest"));
        assert!(!share_json.contains_key("ctime"));
        // Only a row a newer share report answers names its own report.
        assert!(!share_json.contains_key("report_info"));
        let human = render_recycle(&doc);
        assert!(human.contains("photos"));
        assert!(human.contains("hard link 4 B"));
        assert!(!human.contains("OLDEST ITEMS"));
        assert!(human.contains("Control Panel > Shared Folder > Action > Empty Recycle Bin"));

        let mut share_with_oldest = share;
        share_with_oldest.oldest = Some(vec![RecycleOldestJson {
            tree: Some(256),
            inode_device: None,
            objectid: 300,
            path: JsonPath::new(b"/volume1/photos/#recycle/old.bin"),
            ctime: Some("2026-09-29T00:00:00Z".into()),
            bytes: 10,
            reclaimable_bytes: Some(8),
        }]);
        let doc = RecycleDocument {
            shares: vec![share_with_oldest],
            ..doc
        };
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            json["shares"][0]["oldest"][0]["ctime"],
            "2026-09-29T00:00:00Z"
        );
        assert!(render_recycle(&doc).contains("OLDEST ITEMS"));
    }

    #[test]
    fn a_scan_hint_gives_the_exact_command_and_the_last_scans_cost() {
        let last = LastScan {
            finished: "2026-09-28T10:00:00Z".into(),
            elapsed_ms: 911_000,
            entries: 5_510_000,
        };
        let hint = ScanHint::new(b"/volume1", Path::new(DEFAULT_STATE_DIR), Some(last));
        assert_eq!(hint.command, "synoreclaim scan /volume1");
        assert_eq!(
            hint.human(),
            "hint: run `synoreclaim scan /volume1` to answer for /volume1; its last full scan took ~15 min (5.51M entries, finished 2026-09-28T10:00:00Z)"
        );
        let hint = ScanHint::new(b"/volume 2", Path::new("/run/it's"), None);
        assert_eq!(
            hint.command,
            r"synoreclaim scan '/volume 2' --state-dir '/run/it'\''s'"
        );
        assert!(
            hint.human()
                .ends_with("; no completed scan of it is recorded to estimate from")
        );
    }

    #[test]
    fn snapshots_render_as_the_dsm_snapshot_list_with_what_a_selection_frees() {
        use crate::snapshots::answer::share_answer;
        use crate::snapshots::*;
        let info = |id, day: u64, materialized| SnapshotInfo {
            id,
            name: format!("GMT+08-2026.09.{day:02}-00.00.00").into_bytes(),
            subvolume_path: Vec::new(),
            otime: Some(1_790_000_000 - (30 - day) * 86_400),
            generation: 1,
            materialized,
        };
        let holding = |owners: &[u64], hold, bytes| Holding {
            owners: owners.to_vec(),
            hold,
            bytes,
            extents: 1,
            pinned_unreferenced_bytes: (hold == Hold::Resolved && owners.len() == 1).then_some(0),
        };
        let analysis = SnapshotAnalysis {
            depth: Depth::Newest(2),
            owner_budget: DEFAULT_OWNER_BUDGET,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            shares: vec![ShareAnalysis {
                live: 256,
                name: b"backup".to_vec(),
                subvolume_path: b"backup".to_vec(),
                snapshots: vec![
                    info(302, 27, true),
                    info(301, 20, true),
                    info(300, 1, false),
                ],
                pending: Vec::new(),
                held_files: Some(HeldFiles {
                    complete: false,
                    count: 5,
                    files: vec![
                        HeldFile {
                            snapshot: 301,
                            ino: 258,
                            path: b"db/dump.sql".to_vec(),
                            in_live: true,
                            bytes: 3 << 30,
                            unknown_bytes: 0,
                        },
                        HeldFile {
                            snapshot: 302,
                            ino: 257,
                            path: b"gone".to_vec(),
                            in_live: false,
                            bytes: 1 << 30,
                            unknown_bytes: 0,
                        },
                        HeldFile {
                            snapshot: 302,
                            ino: 260,
                            path: Vec::new(),
                            in_live: false,
                            bytes: 0,
                            unknown_bytes: 1 << 20,
                        },
                    ],
                }),
            }],
            holdings: vec![
                holding(&[301], Hold::Resolved, 3 << 30),
                holding(&[301, 302], Hold::Resolved, 5 << 30),
                holding(&[256, 301, 302], Hold::Resolved, 7 << 30),
                holding(&[302], Hold::Unknown, 1 << 20),
            ],
            quota: None,
            stats: AnalysisStats::default(),
        };
        let header = crate::report::testing::header(Vec::new());
        let now = UNIX_EPOCH + Duration::from_secs(header.scan_finished + 7200);
        let selection = "older-than:5d".parse().unwrap();
        let doc = SnapshotsDocument {
            envelope: Envelope::new("snapshots", now),
            results: vec![SnapshotsResult {
                snapshot_analysis: None,
                target: JsonPath::new(b"/v/backup"),
                report: ReportInfo::new(b"/r", &header, now, Duration::from_secs(86_400)),
                shares: vec![share_answer(
                    &analysis,
                    &analysis.shares[0],
                    Some(&selection),
                    now,
                    &crate::snapshots::answer::Listing {
                        mount: b"/v",
                        limit: 10,
                    },
                    &crate::snapshots::answer::Rescan {
                        mount: b"/v",
                        state_dir: Path::new("/s"),
                    },
                )],
            }],
        };
        let text = render_snapshots(&doc);
        for line in [
            "backup  3 snapshots, report ",
            "GMT+08-2026.09.27-00.00.00            2026-09-18 14:13      0 B+?",
            "GMT+08-2026.09.01-00.00.00  (oldest)",
            "not read",
            "Held only by snapshots of this share: 8 GB (sum of EXCLUSIVE is 3 GB; the rest is shared between snapshots)",
            "Unknown: 1 MB whose other owners could not be ruled out",
            "FILES HELD ONLY BY SNAPSHOTS (largest 3 of 5)  BYTES                 IN SNAPSHOT",
            "/v/backup/db/dump.sql  (rewritten)              3 GB  GMT+08-2026.09.20-00.00.00",
            "/v/backup/gone  (deleted)                       1 GB  GMT+08-2026.09.27-00.00.00",
            "inode 260, name not resolved                   0 B+?  GMT+08-2026.09.27-00.00.00",
            "More files hold snapshot-only data than the scan tallied; rescan with a larger --held-files-budget to list them all",
            "1 older snapshot(s) not read",
            "Deleting older-than:5d (2 snapshots) frees 3 GB.",
            "1 selected snapshot(s) were not read",
            "Delete snapshots in DSM: Snapshot Replication > Snapshots > backup.",
        ] {
            assert!(text.contains(line), "{line:?} not in\n{text}");
        }
        let mut old = doc.clone();
        old.results[0].shares[0].held_files = None;
        assert!(
            render_snapshots(&old).contains(
                "This report does not name the files; rescan with --snapshots to list them"
            )
        );
    }

    #[test]
    fn volumes_list_the_system_partition_apart_from_the_data_volumes() {
        let mut doc = VolumesDocument {
            envelope: Envelope::new("volumes", UNIX_EPOCH),
            volumes: Vec::new(),
            excluded: Vec::new(),
            system_partition: SystemPartition::try_new(
                "/dev/md0".into(),
                "ext4".into(),
                8 << 30,
                4 << 30,
            ),
        };
        let text = render_volumes(&doc);
        assert!(
            text.contains(
                "system partition / (/dev/md0, ext4): 4 GB of 8 GB available (DSM's own, not a data volume)\n"
            ),
            "{text}"
        );
        doc.system_partition = None;
        assert!(!render_volumes(&doc).contains("system partition"));
    }

    #[test]
    fn jobs_show_state_phase_work_and_why_they_stopped() {
        let job = JobInfo {
            job_id: "20260928T010203Z-1f".into(),
            kind: "scan",
            plan_id: None,
            elapsed_s: None,
            state: "paused",
            phase: "scan".into(),
            targets: vec![JsonPath::new(b"/volume1")],
            scope: None,
            filesystems: Vec::new(),
            profile: "low",
            created: rfc3339(UNIX_EPOCH),
            updated: rfc3339(UNIX_EPOCH + Duration::from_secs(90)),
            resumes: 1,
            units: 12,
            progress: None,
            cleanup: None,
            rootfs: None,
            rootfs_plan: None,
            reason: Some("runtime_limit".into()),
            tool_version: "0.1.0".into(),
            compatible: true,
            request: None,
            request_ignored: None,
            result: None,
            requested_by: None,
        };
        let mut doc = JobsDocument {
            envelope: Envelope::new("jobs", UNIX_EPOCH),
            jobs: vec![job],
        };
        let text = render_jobs(&doc);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("JOB"), "{text}");
        for part in [
            "20260928T010203Z-1f",
            "paused",
            "scan",
            "12",
            "runtime_limit",
            "/volume1",
        ] {
            assert!(lines[1].contains(part), "{part}: {text}");
        }
        let cleanup = &mut doc.jobs[0];
        (cleanup.kind, cleanup.state, cleanup.phase) =
            ("cleanup-apply", "running", "execute".into());
        cleanup.cleanup = Some(CleanupProgress {
            items_total: 10,
            items_done: 4,
            ..CleanupProgress::default()
        });
        let text = render_jobs(&doc);
        assert!(text.contains("execute 4/10 items"), "{text}");
        doc.jobs.clear();
        assert_eq!(render_jobs(&doc), "no jobs\n");
    }

    #[test]
    fn cache_status_shows_each_unit_and_that_no_index_is_kept() {
        let unit = |subvolume, readonly, reusable| CacheUnit {
            subvolume,
            uuid: "00ff".into(),
            slot: "0000000000000abc".into(),
            generation: 10,
            readonly,
            captured: 0,
            bytes: 1 << 20,
            reusable,
        };
        let status = CacheStatus {
            format: 1,
            fsid: "ab12".into(),
            bytes: 2 << 20,
            units: vec![unit(256, false, false), unit(300, true, true)],
        };
        let mut doc = CacheDocument {
            envelope: Envelope::new("cache", UNIX_EPOCH),
            action: "status",
            filesystems: vec![CacheInfo {
                mount: JsonPath::new(b"/volume1"),
                completion: Completion::Complete,
                status,
                global_index: GLOBAL_INDEX,
                cleared: None,
                errors: Vec::new(),
            }],
        };
        let text = render_cache(&doc);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "/volume1 (ab12): 2 units, 2 MB, cache format 1");
        assert!(
            lines[2].starts_with("256") && lines[2].ends_with("no"),
            "{text}"
        );
        assert!(lines[3].contains("read-only") && lines[3].ends_with("yes"));
        assert!(lines[4].starts_with("global index: not kept"), "{text}");
        doc.filesystems[0].cleared = Some(2);
        assert!(render_cache(&doc).contains("cleared 2 units"));
    }

    #[test]
    fn exit_codes_follow_spec_22_7() {
        assert_eq!(Completion::Complete.exit_code(None), 0);
        assert_eq!(Completion::Failed.exit_code(None), 1);
        assert_eq!(Completion::Partial.exit_code(None), 3);
        assert_eq!(Completion::Busy.exit_code(None), 4);
        assert_eq!(Completion::ResourceLimit.exit_code(None), 10);
        assert_eq!(Completion::RuntimeLimit.exit_code(None), 10);
        assert_eq!(Completion::Paused.exit_code(None), 10);
        assert_eq!(Completion::Paused.exit_code(Some(2)), 130);
        assert_eq!(Completion::Paused.exit_code(Some(15)), 143);
        assert_eq!(Completion::Paused.exit_code(Some(1)), 129);
        assert_eq!(Completion::Cancelled.exit_code(None), 10);
    }

    #[test]
    fn a_cancelled_run_stopped_early() {
        assert!(Completion::Cancelled.is_stop());
        assert!(!Completion::Busy.is_stop());
    }

    #[test]
    fn completion_names_match_json() {
        for c in [
            Completion::Complete,
            Completion::Partial,
            Completion::Busy,
            Completion::Paused,
            Completion::RuntimeLimit,
            Completion::ResourceLimit,
            Completion::Cancelled,
            Completion::Failed,
        ] {
            assert_eq!(serde_json::to_value(c).unwrap(), c.name());
        }
    }

    fn entry(path: &str, flags: Vec<&'static str>) -> EntryJson {
        EntryJson {
            path: JsonPath::new(path.as_bytes()),
            kind: "dir",
            flags,
            metrics: JsonMetrics::from(&crate::model::tree::Metrics {
                logical: 2048,
                physical: 1024,
                unknown: 1024,
                ..Default::default()
            }),
            blockers: Vec::new(),
            nlink: 1,
            tree: Some(5),
            inode_device: None,
            objectid: 256,
            package_data: None,
            without_package_data: None,
        }
    }

    fn abb_share() -> PackageDataJson {
        PackageDataJson {
            service: "active_backup_data",
            package: "Active Backup for Business (backup data)",
            manage_in: "Active Backup for Business",
            manage_in_id: "active_backup_business",
            confirmed: true,
            marker: Some("@ActiveBackup".into()),
            holds: false,
            message: "package-managed data (@ActiveBackup): managed by …".into(),
        }
    }

    #[test]
    fn entries_name_the_package_data_they_are_in_or_hold_only_when_they_do() {
        let plain = serde_json::to_value(entry("/v/a", vec![])).unwrap();
        assert!(plain.get("package_data").is_none());
        let abb = EntryJson {
            package_data: Some(abb_share()),
            ..entry("/v/abb", vec![])
        };
        let json = serde_json::to_value(&abb).unwrap();
        assert_eq!(
            json["package_data"],
            serde_json::json!({
                "service": "active_backup_data",
                "package": "Active Backup for Business (backup data)",
                "manage_in": "Active Backup for Business",
                "manage_in_id": "active_backup_business",
                "confirmed": true,
                "marker": "@ActiveBackup",
                "holds": false,
                "message": "package-managed data (@ActiveBackup): managed by …",
            })
        );
    }

    #[test]
    fn ranked_rows_are_followed_by_the_package_data_they_are_in_or_hold() {
        let abb = EntryJson {
            package_data: Some(abb_share()),
            ..entry("/v/abb", vec![])
        };
        let text = super::entry_rows(&[entry("/v/a", vec![]), abb]);
        assert!(
            text.ends_with("\n  /v/abb: package-managed data (@ActiveBackup): managed by …\n"),
            "{text}"
        );
        assert_eq!(text.matches(": package-managed").count(), 1, "{text}");
    }

    #[test]
    fn explain_names_blockers_and_unresolved_bytes() {
        use crate::model::accounting::Blockers;
        let header = crate::report::testing::header(Vec::new());
        let entry = Entry {
            path: b"/v/a".to_vec(),
            kind: crate::model::namespace::FileKind::Regular,
            flags: NodeFlags::default(),
            metrics: crate::model::tree::Metrics {
                physical: 3 << 20,
                reclaimable: 1 << 20,
                unknown: 1 << 20,
                pinned: 4096,
                ..Default::default()
            },
            blockers: Blockers::SNAPSHOT.with(Blockers::HARD_LINK),
            held: Default::default(),
            nlink: 2,
            tree: 257,
            objectid: 300,
            outside: None,
        };
        let codes: Vec<&str> = explain(&entry, &header).iter().map(|n| n.code).collect();
        assert_eq!(
            codes,
            [
                "snapshot",
                "hard_link",
                "unresolved_references",
                "pinned_unreferenced",
                "hard_links",
                "reference_accounting_partial",
            ]
        );
    }

    #[test]
    fn blocker_summary_lists_retained_classes_and_unknown_bytes() {
        use crate::model::accounting::Blockers;
        let mut header = crate::report::testing::header(Vec::new());
        header.accounting.class_bytes[0] = 3 << 20; // snapshot
        header.accounting.class_bytes[2] = 1 << 20; // hard_link
        header.totals.unknown = 4096;
        let summary = blocker_summary(&header);
        let pairs: Vec<(&str, u64)> = summary.iter().map(|b| (b.blocker, b.bytes)).collect();
        assert_eq!(
            pairs,
            [
                ("snapshot", 3 << 20),
                ("hard_link", 1 << 20),
                ("unknown", 4096)
            ]
        );
        assert_eq!(Blockers::UNKNOWN.names(), ["unknown"]);
    }

    #[test]
    fn volumes_count_and_note_the_reports_they_cannot_read() {
        let mut info = VolumeInfo {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            capabilities: crate::filesystem::FilesystemKind::Btrfs.capabilities(),
            mount: JsonPath::new(b"/volume1"),
            fsid: None,
            total_bytes: None,
            used_bytes: None,
            free_bytes: None,
            metadata_access: true,
            reports: 0,
            incompatible_reports: 0,
            last_scan: None,
            last_consistency: None,
            last_attribution: None,
            pending_deletion: None,
            shares: Vec::new(),
            notes: Vec::new(),
        };
        let listing = crate::state::Listing {
            reports: Vec::new(),
            unreadable: vec![
                ("a.srrep".into(), crate::report::ReportError::Format(3)),
                ("b.srrep".into(), crate::report::ReportError::Magic),
            ],
        };
        info.add_reports(&listing);
        assert_eq!((info.reports, info.incompatible_reports), (0, 2));
        let note = info.notes.iter().find(|n| n.code == "report_incompatible");
        assert!(
            note.is_some_and(|n| n.message.contains("2 reports")),
            "{:?}",
            info.notes
        );
        info.notes.clear();
        info.add_reports(&crate::state::Listing::default());
        assert!(info.notes.is_empty());
    }

    #[test]
    fn volumes_say_when_deleted_subvolumes_still_wait_for_the_cleaner() {
        let volume = |pending| VolumeInfo {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            capabilities: crate::filesystem::FilesystemKind::Btrfs.capabilities(),
            mount: JsonPath::new(b"/volume1"),
            fsid: None,
            total_bytes: None,
            used_bytes: None,
            free_bytes: None,
            metadata_access: true,
            reports: 0,
            incompatible_reports: 0,
            last_scan: None,
            last_consistency: None,
            last_attribution: None,
            pending_deletion: Some(PendingDeletion {
                count: pending,
                bytes: None,
            }),
            shares: Vec::new(),
            notes: Vec::new(),
        };
        let doc = VolumesDocument {
            envelope: Envelope::new("volumes", at(0)),
            volumes: vec![volume(0), volume(3)],
            excluded: Vec::new(),
            system_partition: None,
        };
        let text = render_volumes(&doc);
        assert_eq!(text.matches("await the Btrfs cleaner").count(), 1, "{text}");
        assert!(text.contains("/volume1: 3 deleted subvolume(s)"), "{text}");
        let json = serde_json::to_value(&doc.volumes[1]).unwrap();
        assert_eq!(
            json["pending_deletion"],
            serde_json::json!({"count": 3, "bytes": null})
        );
    }

    #[test]
    fn simulate_delete_renders_the_combined_selection() {
        let header = crate::report::testing::header(Vec::new());
        let now = at(header.scan_finished + 60);
        let combined = crate::model::tree::Metrics {
            logical: 4 << 20,
            physical: 4 << 20,
            reclaimable: 3 << 20,
            ..Default::default()
        };
        let doc = SimulateDocument {
            envelope: Envelope::new("simulate-delete", now),
            results: vec![SimulateResult {
                report: ReportInfo::new(b"/s/r", &header, now, Duration::from_secs(86_400)),
                selection: vec![entry("/v/a", vec![]), entry("/v/b", vec![])],
                combined: JsonMetrics::from(&combined),
                retained: 1 << 20,
                together_bonus: 2 << 20,
                blockers: vec![BlockerBytes {
                    blocker: "snapshot",
                    bytes: 1 << 20,
                }],
                explain: None,
                after_delete: crate::release::after_delete(&["snapshot"]),
                with_snapshots: Some(WithSnapshotsJson {
                    snapshots: vec![JsonPath::new(b"/v/s/#snapshot/b")],
                    snapshot_ids: vec![301],
                    freed_bytes: 12 << 20,
                    unknown_bytes: 1 << 20,
                    unknown_extents: 1,
                    snapshots_not_materialized: 2,
                    manage_in: crate::snapshots::SNAPSHOT_REPLICATION.to_string(),
                    manage_in_ids: vec![crate::dsm::place_id(
                        crate::snapshots::SNAPSHOT_REPLICATION,
                    )],
                }),
            }],
            snapshot_sets: vec![SnapshotSetResult {
                report: ReportInfo::new(b"/s/r", &header, now, Duration::from_secs(86_400)),
                snapshots: vec![JsonPath::new(b"/v/s/#snapshot/a")],
                simulation: SimulationJson {
                    selector: "/v/s/#snapshot/a".into(),
                    selected_ids: vec![300],
                    freed_bytes: 4 << 20,
                    unknown_bytes: 1 << 20,
                    not_materialized_ids: Vec::new(),
                    upper_bound_bytes: Some(5 << 20),
                    exact: false,
                    inexact_reasons: vec![crate::snapshots::answer::InexactReason::UnknownOwners],
                    exact_command: None,
                },
                manage_in: crate::snapshots::SNAPSHOT_REPLICATION.to_string(),
                manage_in_ids: vec![crate::dsm::place_id(crate::snapshots::SNAPSHOT_REPLICATION)],
            }],
            scans: Vec::new(),
        };
        let text = render_simulate(&doc);
        assert!(text.contains("deleting 1 snapshots together"), "{text}");
        assert!(
            text.contains("frees 4 MB, plus up to 1 MB unknown."),
            "{text}"
        );
        assert!(text.contains("It frees at most 5 MB.\n"), "{text}");
        assert!(text.contains("Snapshot Replication > Snapshots."), "{text}");
        assert!(
            text.contains("after the delete:\n  - snapshots keep this data"),
            "{text}"
        );
        assert!(text.contains("(delayed_release)"), "{text}");
        assert!(text.contains("deleting 2 paths together"), "{text}");
        assert!(text.contains("can free       3 MB"), "{text}");
        assert!(text.contains("together bonus 2 MB"), "{text}");
        assert!(text.contains("snapshot 1 MB"), "{text}");
        assert!(
            text.contains(
                "with 1 snapshot(s) deleted too:\n    /v/s/#snapshot/b\n  together they free 12 MB, plus up to 1 MB unknown\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("2 other snapshot(s) of these shares were not read"),
            "{text}"
        );
    }

    #[test]
    fn sharing_is_classified_from_blockers() {
        use crate::model::accounting::Blockers as B;
        let cases = [
            (B::default(), "none"),
            (B::SNAPSHOT, "snapshot"),
            (B::SHARED_TREE.with(B::PENDING_DELETION), "snapshot"),
            (B::LIVE_REFLINK.with(B::OTHER_SUBVOLUME), "live"),
            (B::HARD_LINK, "hard_link"),
            (B::UNKNOWN.with(B::ORPHAN_INODE), "unknown"),
            (B::SNAPSHOT.with(B::UNKNOWN), "snapshot"),
            (B::SNAPSHOT.with(B::HARD_LINK), "mixed"),
        ];
        for (blockers, class) in cases {
            assert_eq!(sharing_class(blockers), class, "{:?}", blockers.names());
        }
    }

    #[test]
    fn shared_entries_carry_their_ratio_and_class() {
        let e = Entry {
            path: b"/v/a".to_vec(),
            kind: crate::model::namespace::FileKind::Regular,
            flags: NodeFlags::default(),
            metrics: crate::model::tree::Metrics {
                logical: 4 << 20,
                physical: 4 << 20,
                reclaimable: 1 << 20,
                ..Default::default()
            },
            blockers: Blockers::SNAPSHOT,
            held: Default::default(),
            nlink: 1,
            tree: 257,
            objectid: 300,
            outside: None,
        };
        let shared = SharedEntry::from(&e);
        let json = serde_json::to_value(&shared).unwrap();
        assert_eq!(json["path"], "/v/a");
        assert_eq!(json["sharing"], "snapshot");
        assert_eq!(json["shared_ratio"], 0.75);
        assert_eq!(json["metrics"]["shared"], 3 << 20);

        let header = crate::report::testing::header(Vec::new());
        let now = at(header.scan_finished + 60);
        let doc = SharedDocument {
            envelope: Envelope::new("shared", now),
            ranked_by: "shared",
            kind: "all",
            limit: 20,
            include_system: false,
            results: vec![SharedResult {
                report: ReportInfo::new(b"/s/r", &header, now, Duration::from_secs(86_400)),
                target: shared.clone(),
                entries: vec![shared],
                hidden_system: 0,
            }],
            scans: Vec::new(),
        };
        let text = render_shared(&doc);
        assert!(text.contains("RATIO"), "{text}");
        assert!(text.contains("75%"), "{text}");
        assert!(text.contains("snapshot"), "{text}");
    }

    #[test]
    fn can_free_marks_unresolved_bytes() {
        let m = |reclaimable, unknown| {
            JsonMetrics::from(&crate::model::tree::Metrics {
                physical: 8192,
                reclaimable,
                unknown,
                ..Default::default()
            })
        };
        assert_eq!(can_free(&m(4096, 0)), "4 KB");
        assert_eq!(can_free(&m(4096, 1)), "4 KB+?");
        assert_eq!(can_free(&m(0, 1)), "unknown");
        assert_eq!(can_free(&m(0, 0)), "0 B");
    }

    const GIB: u64 = 1 << 30;

    fn ranked(path: &str, reclaimable: u64, held: crate::model::accounting::Held) -> Entry {
        Entry {
            path: path.as_bytes().to_vec(),
            kind: crate::model::namespace::FileKind::Directory,
            flags: NodeFlags::default(),
            metrics: crate::model::tree::Metrics {
                logical: 4 * GIB,
                physical: 4 * GIB,
                reclaimable,
                ..Default::default()
            },
            blockers: Blockers::default(),
            held,
            nlink: 1,
            tree: 5,
            objectid: 300,
            outside: None,
        }
    }

    #[test]
    fn an_entry_row_says_who_holds_its_bytes_and_where_to_fix_them() {
        use crate::model::accounting::{Held, HeldClass};
        let vm = ranked("/v/share/vm", GIB, Held::of(HeldClass::Snapshot, 3 * GIB));
        let json = serde_json::to_value(TopRow::entry(&vm, b"/v", None)).unwrap();
        assert_eq!(json["path"], "/v/share/vm");
        assert_eq!(json["kind"], "dir");
        assert!(json["service"].is_null());
        assert_eq!(json["cleanup_eligible"], true);
        assert_eq!(
            json["held_by"],
            serde_json::json!([{"class": "snapshot", "bytes": 3 * GIB}])
        );
        assert_eq!(
            json["where_to_fix"],
            serde_json::json!({
                "token": "snapshot_replication",
                "manage_in": "Snapshot Replication > Snapshots",
                "manage_in_id": "snapshot_replication",
                "command": ["synoreclaim", "snapshots", "/v/share"],
            })
        );
        let free = ranked("/v/share/old", 4 * GIB, Held::default());
        let json = serde_json::to_value(TopRow::entry(&free, b"/v", None)).unwrap();
        assert_eq!(json["held_by"], serde_json::json!([]));
        assert_eq!(
            json["where_to_fix"]["command"],
            serde_json::json!(["synoreclaim", "cleanup", "plan", "/v/share/old"])
        );
        let bin = ranked("/v/share/#recycle", 4 * GIB, Held::default());
        let json = serde_json::to_value(TopRow::entry(&bin, b"/v", None)).unwrap();
        assert_eq!(
            (
                json["cleanup_eligible"].clone(),
                json["where_to_fix"]["token"].clone()
            ),
            (false.into(), "recycle_bin".into())
        );
    }

    #[test]
    fn package_data_rows_are_not_cleanup_eligible() {
        let abb = crate::package::PackageData {
            service: crate::dsm::service(b"@ActiveBackup", crate::dsm::Depth::Below).unwrap(),
            marker: Some(b"@ActiveBackup"),
            holds: false,
        };
        let share = ranked("/v/abb", 4 * GIB, Default::default());
        let json = serde_json::to_value(TopRow::entry(&share, b"/v", Some(&abb))).unwrap();
        assert_eq!(json["cleanup_eligible"], false);
        assert_eq!(json["where_to_fix"]["token"], "package");
        assert_eq!(json["package_data"]["marker"], "@ActiveBackup");
        let holder = crate::package::PackageData { holds: true, ..abb };
        let parent = ranked("/v/share", 4 * GIB, Default::default());
        let json = serde_json::to_value(TopRow::entry(&parent, b"/v", Some(&holder))).unwrap();
        assert_eq!(
            json["cleanup_eligible"], false,
            "deleting it deletes the data"
        );
        assert_eq!(
            json["where_to_fix"]["token"], "cleanup",
            "fixed by its own bytes"
        );
    }

    #[test]
    fn a_service_row_adds_up_its_paths_and_is_never_cleanup_eligible() {
        use crate::model::accounting::{Held, HeldClass};
        let metadata = crate::summary::ServiceUse {
            service: crate::dsm::service(b"@eaDir", crate::dsm::Depth::Below),
            paths: 2,
            metrics: crate::model::tree::Metrics {
                physical: 3 * GIB,
                reclaimable: GIB,
                unknown: GIB,
                ..Default::default()
            },
            held: Held::of(HeldClass::Snapshot, GIB),
            listed: vec![b"/v/a/@eaDir".to_vec(), b"/v/b/@eaDir".to_vec()],
            ..Default::default()
        };
        let json = serde_json::to_value(TopRow::service(&metadata)).unwrap();
        assert_eq!(json["kind"], "service");
        assert_eq!(json["path"], "/v/a/@eaDir");
        assert_eq!(json["cleanup_eligible"], false);
        let service = &json["service"];
        assert_eq!(service["id"], "folder_metadata");
        assert_eq!(service["names"], "@eaDir");
        assert_eq!(service["manage_in_id"], "owning_folder");
        assert_eq!(service["confirmed"], false);
        assert_eq!(service["path_count"], 2);
        assert_eq!(
            service["paths"],
            serde_json::json!([{"path": "/v/a/@eaDir"}, {"path": "/v/b/@eaDir"}])
        );
        assert_eq!(
            json["held_by"],
            serde_json::json!([
                {"class": "snapshot", "bytes": GIB},
                {"class": "unknown", "bytes": GIB},
            ])
        );
        assert_eq!(json["where_to_fix"]["token"], "package");
        assert_eq!(json["where_to_fix"]["manage_in"], service["manage_in"]);
        assert_eq!(
            json["where_to_fix"]["manage_in_id"],
            service["manage_in_id"]
        );
        let other = crate::summary::ServiceUse {
            listed: vec![b"/v/@tmp".to_vec()],
            paths: 1,
            ..Default::default()
        };
        let json = serde_json::to_value(TopRow::service(&other)).unwrap();
        assert_eq!(json["service"]["id"], "other_dsm_service_data");
        assert_eq!(json["where_to_fix"]["token"], "package");
    }

    fn row(entry: EntryJson) -> TopRow {
        TopRow {
            entry,
            service: None,
            cleanup_eligible: true,
            held_by: Vec::new(),
            held_text: "—".into(),
            where_to_fix: FixJson::new(Fix::Cleanup, b"", b"", true),
            within: None,
        }
    }

    fn top_doc(entries: Vec<TopRow>, hidden_system: u64, services: ServicesJson) -> TopDocument {
        let header = crate::report::testing::header(Vec::new());
        let now = at(header.scan_finished + 120);
        TopDocument {
            envelope: Envelope::new("top", now),
            requested_by: "reclaimable",
            ranked_by: "reclaimable",
            kind: "all",
            limit: 20,
            include_system: false,
            results: vec![TopResult {
                report: ReportInfo::new(b"/s/r", &header, now, Duration::from_secs(86_400)),
                target: entry("/v", vec![]),
                entries,
                hidden_system,
                services,
                package_overlaps: Vec::new(),
            }],
            scans: Vec::new(),
            columns: Column::DEFAULT.to_vec(),
            explain: false,
        }
    }

    #[test]
    fn top_renders_unknown_values_and_snapshot_boundaries() {
        let entries = vec![
            row(entry("/v/a\x1b", vec![])),
            row(entry("/v/snap", vec!["snapshot_not_read"])),
        ];
        let text = render_top(&top_doc(entries, 2, ServicesJson::default()));
        assert!(!text.contains("DSM service data ("), "{text}");
        assert!(text.starts_with("/v: report 2026-"), "{text}");
        assert!(text.contains("(2m ago), observed-stable\n"), "{text}");
        assert!(text.contains("/v/a\\x1b"), "{text}");
        assert!(text.contains("/v/snap (snapshot, not read)"), "{text}");
        assert!(text.contains("unknown"), "{text}");
        assert!(
            text.contains(
                "(2 detail rows of DSM service data hidden; --include-system lists them)"
            ),
            "{text}"
        );
    }

    fn line_of<'t>(text: &'t str, start: &str) -> &'t str {
        text.lines()
            .find(|line| line.starts_with(start))
            .unwrap_or_else(|| panic!("no {start}: {text}"))
    }

    #[test]
    fn top_rows_name_who_holds_their_bytes_and_where_to_fix_them() {
        use crate::model::accounting::{Held, HeldClass};
        let vm = ranked("/v/share/vm", GIB, Held::of(HeldClass::Snapshot, 3 * GIB));
        let old = ranked("/v/share/old", 4 * GIB, Held::default());
        let metadata = crate::summary::ServiceUse {
            service: crate::dsm::service(b"@eaDir", crate::dsm::Depth::Below),
            paths: 2,
            metrics: crate::model::tree::Metrics {
                physical: 3 * GIB,
                reclaimable: GIB,
                ..Default::default()
            },
            held: Held::of(HeldClass::HardLink, 2 * GIB),
            listed: vec![b"/v/a/@eaDir".to_vec(), b"/v/b/@eaDir".to_vec()],
            ..Default::default()
        };
        let entries = vec![
            TopRow::entry(&old, b"/v", None),
            TopRow::service(&metadata),
            TopRow::entry(&vm, b"/v", None),
        ];
        let mut doc = top_doc(entries, 0, ServicesJson::default());
        let text = render_top(&doc);
        let header = line_of(&text, "PATH");
        assert!(header.contains("  CAN FREE  HELD BY  "), "{text}");
        assert!(header.ends_with("  WHERE TO FIX"), "{text}");
        let vm = line_of(&text, "/v/share/vm");
        assert!(vm.contains("  snapshot 3 GB  "), "{text}");
        assert!(vm.ends_with("  Snapshot Replication"), "{text}");
        let old = line_of(&text, "/v/share/old");
        assert!(old.contains("  —  "), "{text}");
        assert!(old.ends_with("  cleanup"), "{text}");
        let service = line_of(
            &text,
            "[DSM] DSM indexing (thumbnails and file metadata) (@eaDir)",
        );
        assert!(service.contains("1 GB*  hard link 2 GB"), "{text}");
        assert!(service.ends_with("  the folder it belongs to"), "{text}");
        assert!(
            text.contains(
                "\n* frees only through the managing package; synoreclaim cleanup refuses service data.\n"
            ),
            "{text}"
        );
        assert!(!text.contains("$ synoreclaim"), "{text}");

        doc.explain = true;
        let text = render_top(&doc);
        assert!(
            text.contains(
                "/v/share/old"
            ) && text.contains(
                "\n  mostly reclaimable: review its deletion with cleanup plan\n  $ synoreclaim cleanup plan /v/share/old\n"
            ),
            "{text}"
        );
        assert!(text.contains("\n  managed by DSM indexing"), "{text}");

        doc.explain = false;
        doc.columns = vec![Column::Size, Column::CanFree];
        let text = render_top(&doc);
        assert!(line_of(&text, "PATH").ends_with("SIZE  CAN FREE"), "{text}");
    }

    #[test]
    fn a_row_holding_package_data_shows_its_values_without_it() {
        use crate::model::accounting::{Held, HeldClass};
        let mut share = ranked("/v/share", 0, Held::of(HeldClass::Snapshot, 4 * GIB));
        let mut without = share.metrics;
        without.logical = GIB;
        without.physical = GIB;
        share.outside = Some(Box::new(crate::model::tree::Outside {
            metrics: without,
            held: Held::of(HeldClass::Snapshot, GIB),
        }));
        let doc = top_doc(
            vec![TopRow::entry(&share, b"/v", None)],
            0,
            ServicesJson::default(),
        );
        let human = render_top(&doc);
        let row = line_of(&human, "/v/share ");
        assert!(row.contains("1 GB"), "{human}");
        assert!(!row.contains("4 GB"), "{human}");
        let json = serde_json::to_value(&doc).unwrap();
        let entry = &json["results"][0]["entries"][0];
        assert_eq!(
            entry["metrics"]["physical"],
            4 * GIB,
            "JSON keeps full values"
        );
        assert_eq!(entry["without_package_data"]["physical"], GIB);
        assert_eq!(entry["held_by"][0]["bytes"], 4 * GIB);
    }

    #[test]
    fn package_overlap_notes_follow_the_table_and_stay_out_of_json() {
        let share = ranked("/v/share", 20 * GIB, Default::default());
        let mut doc = top_doc(
            vec![TopRow::entry(&share, b"/v", None)],
            0,
            ServicesJson::default(),
        );
        doc.results[0].package_overlaps = vec![PackageOverlap {
            target: b"/v/share".to_vec(),
            service: crate::dsm::service(b"vault.hbk", crate::dsm::Depth::Below).unwrap(),
            service_path: Some(b"/v/share/vault.hbk".to_vec()),
            bytes: 8 * GIB,
        }];

        let human = render_top(&doc);
        let row = human.find("\n/v/share ").unwrap();
        let note = human
            .find("\n  /v/share leaves out 8 GB of package data, counted in the Hyper Backup (backup data) row")
            .unwrap();
        assert!(note > row, "{human}");
        let json = serde_json::to_value(&doc).unwrap();
        assert!(json["results"][0].get("package_overlaps").is_none());
    }

    #[test]
    fn a_recycle_bin_a_ranked_folder_leaves_out_is_noted_with_its_row() {
        let share = ranked("/v/share", 20 * GIB, Default::default());
        let mut doc = top_doc(
            vec![TopRow::entry(&share, b"/v", None)],
            0,
            ServicesJson::default(),
        );
        doc.results[0].package_overlaps = vec![PackageOverlap {
            target: b"/v/share".to_vec(),
            service: crate::dsm::service(b"#recycle", crate::dsm::Depth::Below).unwrap(),
            service_path: Some(b"/v/share/#recycle".to_vec()),
            bytes: 2 * GIB,
        }];

        let human = render_top(&doc);
        assert!(
            human.contains(
                "\n  /v/share leaves out 2 GB of Recycle Bin data, counted in the /v/share/#recycle row\n"
            ),
            "{human}"
        );
    }

    #[test]
    fn nothing_left_out_needs_no_note() {
        let share = ranked("/v/share", 20 * GIB, Default::default());
        let mut doc = top_doc(
            vec![TopRow::entry(&share, b"/v", None)],
            0,
            ServicesJson::default(),
        );
        doc.results[0].package_overlaps = vec![PackageOverlap {
            target: b"/v/share".to_vec(),
            service: crate::dsm::service(b"#recycle", crate::dsm::Depth::Below).unwrap(),
            service_path: Some(b"/v/share/#recycle".to_vec()),
            bytes: 0,
        }];

        let human = render_top(&doc);
        assert!(!human.contains("leaves out"), "{human}");
    }

    #[test]
    fn a_folder_the_table_does_not_show_gets_no_note() {
        let share = ranked("/v/share", 20 * GIB, Default::default());
        let mut doc = top_doc(
            vec![TopRow::entry(&share, b"/v", None)],
            0,
            ServicesJson::default(),
        );
        doc.results[0].package_overlaps = vec![PackageOverlap {
            target: b"/v/homes".to_vec(),
            service: crate::dsm::service(b"#recycle", crate::dsm::Depth::Below).unwrap(),
            service_path: Some(b"/v/homes/alice/#recycle".to_vec()),
            bytes: GIB,
        }];

        let human = render_top(&doc);
        assert!(!human.contains("leaves out"), "{human}");
    }

    fn services() -> ServicesJson {
        let m = |physical, reclaimable| crate::model::tree::Metrics {
            physical,
            reclaimable,
            ..Default::default()
        };
        let summary = crate::summary::ServiceSummary {
            uses: vec![
                crate::summary::ServiceUse {
                    service: crate::dsm::service(b"@synologydrive", crate::dsm::Depth::VolumeRoot),
                    path: Some(b"/v/@synologydrive".to_vec()),
                    paths: 1,
                    metrics: m(40 << 30, 10 << 30),
                    snapshots_not_read: 0,
                    ..Default::default()
                },
                crate::summary::ServiceUse {
                    service: crate::dsm::service(b"@eaDir", crate::dsm::Depth::Below),
                    path: None,
                    paths: 27,
                    metrics: m(1 << 30, 1 << 30),
                    snapshots_not_read: 0,
                    ..Default::default()
                },
                crate::summary::ServiceUse {
                    service: None,
                    path: None,
                    paths: 3,
                    metrics: m(1 << 20, 0),
                    snapshots_not_read: 0,
                    ..Default::default()
                },
                crate::summary::ServiceUse {
                    service: crate::dsm::service(b"@sharesnap", crate::dsm::Depth::VolumeRoot),
                    path: Some(b"/v/@sharesnap".to_vec()),
                    paths: 1,
                    metrics: m(0, 0),
                    snapshots_not_read: 4,
                    ..Default::default()
                },
            ],
            other_snapshots_not_read: 1,
        };
        ServicesJson::from(&summary)
    }

    #[test]
    fn service_lines_name_the_package_and_split_the_footprint() {
        let json = serde_json::to_value(services()).unwrap();
        let drive = &json["lines"][0];
        assert_eq!(drive["service"], "synology_drive");
        assert_eq!(drive["package"], "Synology Drive Server");
        assert_eq!(drive["path"], "/v/@synologydrive");
        assert_eq!(drive["confirmed"], true);
        assert_eq!(drive["metrics"]["reclaimable"], 10u64 << 30);
        assert_eq!(drive["metrics"]["shared"], 30u64 << 30);
        let other = &json["lines"][2];
        assert_eq!(other["service"], "other_dsm_service_data");
        assert!(other["path"].is_null());
        assert_eq!(other["paths"], 3);
        assert_eq!(json["lines"][3]["snapshots_not_read"], 4);
        assert_eq!(json["other_snapshots_not_read"], 1);
    }

    #[test]
    fn top_iscsi_service_rows_carry_the_fixed_explanation() {
        let service = crate::dsm::service(b"@iSCSI", crate::dsm::Depth::VolumeRoot).unwrap();
        let line = crate::summary::ServiceUse {
            service: Some(service),
            path: Some(b"/v/@iSCSI".to_vec()),
            paths: 1,
            ..Default::default()
        };
        let row = TopRow::service(&line);
        assert!(row.where_to_fix.hint.contains("UNMAP"));
        let json = serde_json::to_value(row).unwrap();
        assert!(
            json["where_to_fix"]["explanation"]
                .as_str()
                .unwrap_or_default()
                .contains("UNMAP")
        );

        let services = ServicesJson::from(&ServiceSummary {
            uses: vec![line],
            ..Default::default()
        });
        let json = serde_json::to_value(services).unwrap();
        assert!(
            json["lines"][0]["explanation"]
                .as_str()
                .unwrap_or_default()
                .contains("Virtual Machine Manager > Image")
        );
        let mut human = String::new();
        render_services(
            &mut human,
            &ServicesJson::from(&ServiceSummary {
                uses: vec![crate::summary::ServiceUse {
                    service: Some(service),
                    path: Some(b"/v/@iSCSI".to_vec()),
                    paths: 1,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        );
        assert!(human.contains("UNMAP"), "{human}");
    }

    #[test]
    fn top_shows_dsm_service_data_with_where_to_manage_it() {
        let doc = top_doc(vec![row(entry("/v/a", vec![]))], 2, services());
        let text = render_top(&doc);
        let section = text.split_once("DSM service data").expect(&text).1;
        for part in [
            "/v/@synologydrive",
            "40 GB",
            "10 GB",
            "30 GB",
            "Synology Drive Server",
            "@eaDir (27 paths)",
            "other @ entries (3 paths)",
            "4 snapshots not read",
            "manage it in Synology Drive Admin Console",
            "1 more snapshot not read",
        ] {
            assert!(section.contains(part), "{part}: {text}");
        }
        assert!(section.contains("layout not confirmed"), "{text}");
    }

    #[test]
    fn target_lines_keep_the_case_of_their_path() {
        let mut fs = FilesystemScan::new("f".into(), b"/v", Vec::new());
        fs.targets.push(ScopeTarget {
            path: JsonPath::new(b"/v/ActiveBackupforBusiness/@ActiveBackup"),
            scope: Scope::Path,
            tree: Some(257),
            inode_device: None,
            ino: 300,
        });
        let mut out = String::new();
        render_filesystem(&mut out, &fs);
        assert!(
            out.contains("  target /v/ActiveBackupforBusiness/@ActiveBackup (path)\n"),
            "{out}"
        );
    }

    #[test]
    fn a_stopped_filesystem_still_shows_what_it_read() {
        let mut fs = FilesystemScan::new("f".into(), b"/v", Vec::new());
        fs.completion = Completion::ResourceLimit;
        fs.stats = Some(crate::report::ScanStats {
            subvolumes_discovered: 3,
            subvolumes_scanned: 2,
            entries: 41,
            ..Default::default()
        });
        let mut out = String::new();
        render_filesystem(&mut out, &fs);
        assert!(out.contains("3 discovered, 2 scanned"), "{out}");
        assert!(out.contains("entries 41"), "{out}");
        assert!(!out.contains("logical"), "no totals: {out}");
    }

    #[test]
    fn a_memory_stop_says_what_tripped_the_budget() {
        let trip = crate::resource::MemoryTrip {
            budget_bytes: 64 << 20,
            resident_bytes: 60 << 20,
            reserved_bytes: 8 << 20,
        };
        let mut fs = FilesystemScan::new("f".into(), b"/v", Vec::new());
        fs.completion = Completion::ResourceLimit;
        fs.resource_limit = Some(MemoryStop::new(257, 64 << 20, Some(trip)));
        let json = serde_json::to_value(&fs).unwrap();
        assert_eq!(
            json["resource_limit"],
            serde_json::json!({
                "tree": 257,
                "budget_bytes": 64 << 20,
                "resident_bytes": 60 << 20,
                "reserved_bytes": 8 << 20,
                "projected_bytes": 68 << 20,
            })
        );
        let mut out = String::new();
        render_filesystem(&mut out, &fs);
        assert!(
            out.contains("  memory budget 64 MB: 60 MB resident + 8 MB needed = 68 MB, tree 257\n"),
            "{out}"
        );

        fs.resource_limit = Some(MemoryStop::new(2, 64 << 20, None));
        let json = serde_json::to_value(&fs).unwrap();
        assert_eq!(
            json["resource_limit"]["projected_bytes"],
            serde_json::Value::Null
        );
        let mut out = String::new();
        render_filesystem(&mut out, &fs);
        assert!(
            out.contains("  memory budget 64 MB: an allocation failed, tree 2\n"),
            "{out}"
        );
    }

    fn balance_running() -> Vec<ChangeReason> {
        vec![
            ChangeReason::SubvolumeAppeared {
                subvolume_id: 259,
                generation: 40,
                has_uuid: true,
                snapshot_of: None,
            },
            ChangeReason::MaintenanceRunning {
                operation: Maintenance::Balance,
            },
        ]
    }

    #[test]
    fn a_changed_filesystem_says_why() {
        let mut fs = FilesystemScan::new("f".into(), b"/v", Vec::new());
        fs.consistency = Some(Consistency::ChangedDuringScan);
        fs.change_reasons = balance_running();
        fs.stats = Some(crate::report::ScanStats::default());
        let mut out = String::new();
        render_filesystem(&mut out, &fs);
        assert!(
            out.contains("changed during scan: subvolume 259 appeared; balance running\n"),
            "{out}"
        );
    }

    #[test]
    fn answers_from_a_changed_report_say_why() {
        let mut header = crate::report::testing::header(Vec::new());
        header.consistency = Consistency::ChangedDuringScan;
        header.change_reasons = balance_running();
        let info = ReportInfo::new(b"/s/r", &header, at(0), Duration::from_secs(60));
        assert!(
            report_line(&info)
                .ends_with(", changed-during-scan (subvolume 259 appeared; balance running)"),
            "{}",
            report_line(&info)
        );
    }

    #[test]
    fn the_consistency_note_says_why() {
        let mut header = crate::report::testing::header(Vec::new());
        header.consistency = Consistency::ChangedDuringScan;
        header.change_reasons = balance_running();
        let entry = Entry {
            path: b"/v/a".to_vec(),
            kind: crate::model::namespace::FileKind::Regular,
            flags: NodeFlags::default(),
            metrics: crate::model::tree::Metrics::default(),
            blockers: Blockers::default(),
            held: Default::default(),
            nlink: 1,
            tree: 257,
            objectid: 300,
            outside: None,
        };
        let notes = explain(&entry, &header);
        let note = notes.iter().find(|n| n.code == "consistency").unwrap();
        assert_eq!(
            note.message,
            "report is changed-during-scan (subvolume 259 appeared; balance running)"
        );
    }

    #[test]
    fn explain_names_the_package_managing_a_known_service_path() {
        use crate::model::accounting::Blockers;
        let header = crate::report::testing::header(Vec::new());
        let entry = |path: &[u8]| Entry {
            path: path.to_vec(),
            kind: crate::model::namespace::FileKind::Directory,
            flags: NodeFlags::SYSTEM,
            metrics: crate::model::tree::Metrics::default(),
            blockers: Blockers::default(),
            held: Default::default(),
            nlink: 1,
            tree: 5,
            objectid: 300,
            outside: None,
        };
        let service = |path: &[u8]| {
            explain(&entry(path), &header)
                .into_iter()
                .find(|n| n.code == "dsm_service")
                .map(|n| n.message)
        };
        assert_eq!(
            service(b"/v/@docker/overlay2").unwrap(),
            "@docker: managed by Container Manager; manage it in Container Manager (remove unused images and containers)"
        );
        let hbk = service(b"/v/share/task.hbk").unwrap();
        assert!(hbk.ends_with("; cleanup refuses it"), "{hbk}");
        assert_eq!(service(b"/v/@SYNO.core.gz"), None);
    }

    #[test]
    fn report_info_says_which_cleanup_made_the_report_stale() {
        let header = crate::report::testing::header(Vec::new());
        let now = at(header.scan_finished);
        let fresh = || ReportInfo::new(b"/s/r", &header, now, Duration::from_secs(60));
        let mark = CleanupMark {
            plan_id: "cleanup-1".into(),
            execution_log: "cleanup-1-1.jsonl".into(),
            applied_at: "2026-09-29T12:00:00Z".into(),
        };

        let unmarked = serde_json::to_value(fresh().with_cleanup(Marked::No)).unwrap();
        let marked = serde_json::to_value(fresh().with_cleanup(Marked::By(mark))).unwrap();
        let unknown = serde_json::to_value(fresh().with_cleanup(Marked::Unknown)).unwrap();

        assert_eq!(unmarked["stale"], false);
        assert_eq!(unmarked["cleanup_since_report"], serde_json::Value::Null);
        assert_eq!(marked["stale"], true);
        assert_eq!(
            marked["cleanup_since_report"],
            serde_json::json!({
                "plan_id": "cleanup-1",
                "execution_log": "cleanup-1-1.jsonl",
                "applied_at": "2026-09-29T12:00:00Z"
            })
        );
        assert_eq!(unknown["stale"], true);
        assert_eq!(unknown["cleanup_since_report"], serde_json::Value::Null);
    }

    #[test]
    fn report_info_says_why_the_report_changed() {
        let mut header = crate::report::testing::header(Vec::new());
        header.change_reasons = vec![ChangeReason::MaintenanceRunning {
            operation: Maintenance::Balance,
        }];
        let info = ReportInfo::new(b"/s/r", &header, at(0), Duration::from_secs(60));
        assert_eq!(
            serde_json::to_value(&info).unwrap()["change_reasons"],
            serde_json::json!([{"kind": "maintenance-running", "operation": "balance"}])
        );
    }

    #[test]
    fn a_filesystem_lists_no_change_reasons_until_a_scan_judged_it() {
        let fs = FilesystemScan::new("f".into(), b"/v", Vec::new());
        assert_eq!(
            serde_json::to_value(&fs).unwrap()["change_reasons"],
            serde_json::json!([])
        );
    }

    #[test]
    fn report_info_marks_stale_reports() {
        let header = crate::report::testing::header(Vec::new());
        let now = at(header.scan_finished + 90_000);
        let info = ReportInfo::new(b"/s/r.srrep", &header, now, Duration::from_secs(86_400));
        assert!(info.stale);
        assert_eq!(info.age_seconds, 90_000);
        assert_eq!(info.scan_finished, rfc3339(at(header.scan_finished)));
    }
}
