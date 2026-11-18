//! The read-only `rootfs` report: how full the system partition is,
//! where its space goes, what `rootfs plan` could move off it, and what
//! stays with advice only.

use super::classify::{Category, classify};
use super::plan::{
    CleanupMode, DESTINATION_MARGIN_BYTES, keep_backup_installer, keep_pinned, select_mode,
};
use super::walk::{Candidate, NotWalkedReason, Owner, Walk, WalkStop};
use crate::btrfs::walk::StopReason;
use crate::document::{Completion, Envelope};
use crate::output::{JsonPath, Notice, escape_human, human_bytes};
use crate::rootfs::delegate::Delegate;
use crate::space::{
    SystemPartition, SystemPartitionLevel, SystemPartitionLevelReason, classify_system_partition,
};
use crate::why_not_freed::{OpenHolder, ProcessScan};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// DSM needs the patch size plus 130 MB free on `/` for an update, and
/// up to twice the patch size plus 130 MB for an HDD migration (KI#77166,
/// KI#94475). Below this much available, a large update may not fit.
pub const UPDATE_HEADROOM_BYTES: u64 = 1 << 30;
/// First-level rows the human report lists, at most.
const HUMAN_ROWS: usize = 12;
/// Second-level rows kept per first-level row.
const CHILD_ROWS: usize = 10;
/// Second-level rows the human report lists per first-level row.
const HUMAN_CHILD_ROWS: usize = 4;

/// What statfs says about `/`, and where it is mounted from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub source: String,
    pub fstype: String,
    pub total_bytes: u64,
    /// `f_bfree`: free to root.
    pub free_bytes: u64,
    /// `f_bavail`: free to everyone else, as `df` shows it.
    pub available_bytes: u64,
    pub inodes_total: u64,
    pub inodes_free: u64,
}

/// DSM's `/var/log` quota (project 1001 on an ext4 system partition).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LogQuota {
    pub project_id: u32,
    pub used_bytes: u64,
    pub soft_limit_bytes: u64,
    pub hard_limit_bytes: u64,
}

/// What the report was made from.
pub struct Inputs<'a> {
    pub header: Header,
    pub log_quota: Option<LogQuota>,
    pub walk: &'a Walk,
    pub elapsed: Duration,
    pub open_deleted: ProcessScan,
    pub tools: Vec<Delegate>,
    /// The tools a deep plan would run.
    pub deep_tools: Vec<Delegate>,
    /// The last boot (`btime`), Unix seconds, which plan's update guard uses.
    pub boot: i64,
    /// The name Package Center shows, by package id.
    pub package_names: &'a BTreeMap<Vec<u8>, String>,
}

#[derive(Debug, Serialize)]
pub struct RootfsDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub filesystem: Filesystem,
    pub log_quota: Option<LogQuotaJson>,
    pub walk: WalkSummary,
    /// First-level paths, most bytes first.
    pub usage: Vec<UsageRow>,
    /// Bytes per owner, most first.
    pub owners: Vec<OwnerRow>,
    /// The owners under 1 MiB, folded out of `owners`.
    pub owners_small: Option<SmallRows>,
    pub cleanable: ReportCleanable,
    /// Recognised owners `rootfs plan` never selects, with advice.
    pub report_only: Vec<ReportOnly>,
    /// The report-only owners under 1 MiB, folded out of `report_only`.
    pub report_only_small: Option<SmallRows>,
    pub open_deleted: OpenDeleted,
    pub not_walked: Vec<NotWalkedPath>,
}

#[derive(Debug, Serialize)]
pub struct Filesystem {
    pub mount: &'static str,
    pub source: String,
    pub fstype: String,
    pub total_bytes: u64,
    /// `total - free`, as `df` counts used.
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    /// `df`'s Use%: used over used plus available, rounded up.
    pub used_percent: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<SystemPartitionLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level_reasons: Option<Vec<SystemPartitionLevelReason>>,
    pub inodes_total: u64,
    pub inodes_used: u64,
    pub nearly_full: bool,
}

#[derive(Debug, Serialize)]
pub struct LogQuotaJson {
    pub path: &'static str,
    #[serde(flatten)]
    pub quota: LogQuota,
}

#[derive(Debug, Serialize)]
pub struct WalkSummary {
    pub entries: u64,
    pub allocated_bytes: u64,
    pub complete: bool,
    pub stop_reason: Option<&'static str>,
    pub unreadable_dirs: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct UsageRow {
    #[serde(flatten)]
    pub entry: UsageEntry,
    /// Its largest second-level paths, most bytes first.
    pub children: Vec<UsageEntry>,
    /// Bytes of the second-level paths not listed.
    pub other_children_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct UsageEntry {
    #[serde(flatten)]
    pub path: JsonPath,
    pub bytes: u64,
    /// Share of `filesystem.used_bytes`, one decimal.
    pub percent_of_used: f64,
    pub category: Category,
    pub package: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OwnerRow {
    pub category: Category,
    /// The package, or the user-tooling folder.
    pub name: Option<String>,
    /// The package's name in Package Center, when its `INFO` gives one.
    pub display_name: Option<String>,
    pub bytes: u64,
    pub percent_of_used: f64,
}

/// Rows below `below_bytes`, folded into one: many packages keep a few
/// KiB on `/`, and listing each one hides the rows that matter.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SmallRows {
    pub below_bytes: u64,
    pub items: u64,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Cleanable {
    /// Allocated bytes of the entries `rootfs plan` could move.
    pub bytes: u64,
    /// Those bytes plus the margin `rootfs plan` keeps free on the destination.
    pub required_destination_bytes: u64,
    pub items: u64,
    pub by_category: Vec<CategoryTotal>,
}

#[derive(Debug, Serialize)]
pub struct ReportCleanable {
    #[serde(flatten)]
    pub totals: Cleanable,
    /// DSM's fixed tools `rootfs plan` would run on this host.
    pub tools: Vec<Delegate>,
    /// What `rootfs plan --mode deep` could move, Standard's items included.
    pub deep: DeepCleanable,
}

#[derive(Debug, Serialize)]
pub struct DeepCleanable {
    #[serde(flatten)]
    pub totals: Cleanable,
    /// Bytes deep moves beyond Standard's.
    pub additional_bytes: u64,
    pub tools: Vec<Delegate>,
}

#[derive(Debug, Serialize)]
pub struct CategoryTotal {
    pub category: Category,
    pub bytes: u64,
    pub items: u64,
}

#[derive(Debug, Serialize)]
pub struct ReportOnly {
    pub category: Category,
    pub name: Option<String>,
    pub bytes: u64,
    pub advice: RootfsAdvice,
}

#[derive(Debug, Serialize)]
pub struct OpenDeleted {
    /// Allocated bytes of the distinct deleted inodes held, an estimate.
    pub bytes_estimate: u64,
    pub holders: Vec<Holder>,
    pub unreadable_processes: u64,
}

#[derive(Debug, Serialize)]
pub struct Holder {
    #[serde(flatten)]
    pub holder: OpenHolder,
    pub advice: RootfsAdvice,
}

/// A stable advice identifier emitted by `rootfs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdviceCode {
    UserTooling,
    PackageData,
    LiveLogs,
    LogCenterDb,
    GeoipDatabase,
    OwnState,
    DsmKeep,
    RestartSyslogNg,
    RestartHolder,
}

/// A management destination exposed by a rootfs advice row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManageInId {
    PackageCenter,
}

#[derive(Debug, Serialize)]
pub struct RootfsAdvice {
    pub code: AdviceCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manage_in_id: Option<ManageInId>,
    pub message: String,
}

impl RootfsAdvice {
    fn new(code: AdviceCode, message: impl Into<String>) -> Self {
        RootfsAdvice {
            code,
            manage_in_id: None,
            message: message.into(),
        }
    }

    fn with_manage_in_id(mut self, id: ManageInId) -> Self {
        self.manage_in_id = Some(id);
        self
    }
}

#[derive(Debug, Serialize)]
pub struct NotWalkedPath {
    #[serde(flatten)]
    pub path: JsonPath,
    pub reason: NotWalkedReason,
}

/// Builds the report from what was read.
pub fn build(inputs: Inputs<'_>, now: SystemTime) -> RootfsDocument {
    let header = &inputs.header;
    let walk = inputs.walk;
    let used = header.total_bytes.saturating_sub(header.free_bytes);
    let classification =
        classify_system_partition(Some(header.total_bytes), Some(header.available_bytes));
    let mut envelope = Envelope::new("rootfs", now);
    let partition = SystemPartition::try_new(
        header.source.clone(),
        header.fstype.clone(),
        header.total_bytes,
        header.available_bytes,
    );
    envelope
        .warnings
        .extend(partition.as_ref().and_then(SystemPartition::notice));
    envelope
        .warnings
        .extend(update_headroom(header.available_bytes));
    envelope.warnings.extend(walk_warnings(walk));
    envelope.completion = completion(walk);
    let (owners, owners_small) =
        fold_small(owners(walk, used, inputs.package_names), |row| row.bytes);
    let (report_only, report_only_small) = fold_small(report_only(walk), |row| row.bytes);
    RootfsDocument {
        envelope,
        filesystem: Filesystem {
            mount: "/",
            source: header.source.clone(),
            fstype: header.fstype.clone(),
            total_bytes: header.total_bytes,
            used_bytes: used,
            free_bytes: header.free_bytes,
            available_bytes: header.available_bytes,
            used_percent: df_percent(used, header.available_bytes),
            level: classification.as_ref().map(|value| value.level),
            level_reasons: classification.map(|value| value.level_reasons),
            inodes_total: header.inodes_total,
            inodes_used: header.inodes_total.saturating_sub(header.inodes_free),
            nearly_full: partition
                .as_ref()
                .is_some_and(|partition| partition.nearly_full),
        },
        log_quota: inputs.log_quota.map(|quota| LogQuotaJson {
            path: "/var/log",
            quota,
        }),
        walk: WalkSummary {
            entries: walk.entries,
            allocated_bytes: walk.allocated_bytes,
            complete: walk.stopped.is_none(),
            stop_reason: walk.stopped.map(stop_name),
            unreadable_dirs: walk.unreadable,
            elapsed_ms: u64::try_from(inputs.elapsed.as_millis()).unwrap_or(u64::MAX),
        },
        usage: usage(walk, used),
        owners,
        owners_small,
        cleanable: report_cleanable(
            walk,
            header.available_bytes,
            crate::time::unix_seconds(now)
                .try_into()
                .unwrap_or(i64::MAX),
            inputs.boot,
            inputs.tools,
            inputs.deep_tools,
        ),
        report_only,
        report_only_small,
        open_deleted: open_deleted(inputs.open_deleted),
        not_walked: walk
            .not_walked
            .iter()
            .map(|n| NotWalkedPath {
                path: JsonPath::new(&n.path),
                reason: n.reason,
            })
            .collect(),
    }
}

fn completion(walk: &Walk) -> Completion {
    match walk.stopped {
        None if walk.unreadable == 0 => Completion::Complete,
        None | Some(WalkStop::EntryLimit) => Completion::Partial,
        Some(WalkStop::Stopped(StopReason::RuntimeLimit)) => Completion::RuntimeLimit,
        Some(WalkStop::Stopped(StopReason::ResourceLimit | StopReason::DiskLimit)) => {
            Completion::ResourceLimit
        }
        Some(WalkStop::Stopped(StopReason::Cancelled)) => Completion::Cancelled,
        Some(WalkStop::Stopped(StopReason::Paused)) => Completion::Paused,
    }
}

fn stop_name(stop: WalkStop) -> &'static str {
    match stop {
        WalkStop::EntryLimit => "entry_limit",
        WalkStop::Stopped(StopReason::Paused) => "paused",
        WalkStop::Stopped(StopReason::Cancelled) => "cancelled",
        WalkStop::Stopped(StopReason::RuntimeLimit) => "runtime_limit",
        WalkStop::Stopped(StopReason::ResourceLimit) => "resource_limit",
        WalkStop::Stopped(StopReason::DiskLimit) => "disk_limit",
    }
}

fn update_headroom(available: u64) -> Option<Notice> {
    (available < UPDATE_HEADROOM_BYTES).then(|| {
        Notice::new(
            "update_headroom_low",
            format!(
                "{} available on /: a DSM update needs the patch size plus 130 MB free there (an HDD migration up to twice the patch size plus 130 MB), so a large update may not fit",
                human_bytes(available)
            ),
        )
    })
}

fn walk_warnings(walk: &Walk) -> Vec<Notice> {
    let mut warnings = Vec::new();
    if let Some(stop) = walk.stopped {
        warnings.push(Notice::new(
            "walk_incomplete",
            format!(
                "the walk stopped after {} entries ({}): the totals below are lower bounds",
                walk.entries,
                stop_name(stop)
            ),
        ));
    }
    if walk.unreadable > 0 {
        warnings.push(Notice::new(
            "unreadable_directories",
            format!(
                "{} directories could not be read and are not counted",
                walk.unreadable
            ),
        ));
    }
    warnings
}

/// `df`'s Use%.
fn df_percent(used: u64, available: u64) -> u64 {
    let base = u128::from(used) + u128::from(available);
    if base == 0 {
        return 0;
    }
    (u128::from(used) * 100).div_ceil(base) as u64
}

/// `bytes` as a percentage of `used`, one decimal.
fn percent(bytes: u64, used: u64) -> f64 {
    if used == 0 {
        return 0.0;
    }
    (u128::from(bytes) * 1000 / u128::from(used)) as f64 / 10.0
}

fn usage_entry(path: &[u8], bytes: u64, used: u64) -> UsageEntry {
    let class = super::classify::classify(path, super::classify::Kind::Dir);
    UsageEntry {
        path: JsonPath::new(path),
        bytes,
        percent_of_used: percent(bytes, used),
        category: class.category,
        package: super::classify::package_name(path).map(escape_human),
    }
}

fn usage(walk: &Walk, used: u64) -> Vec<UsageRow> {
    let is_first_level = |path: &[u8]| path.iter().filter(|&&b| b == b'/').count() == 1;
    let mut rows: Vec<UsageRow> = walk
        .rows
        .iter()
        .filter(|(path, _)| is_first_level(path))
        .map(|(path, &bytes)| {
            let mut children: Vec<(&Vec<u8>, u64)> = walk
                .rows
                .range(path.clone()..)
                .skip(1)
                .take_while(|(child, _)| child.starts_with(path))
                .filter(|(child, _)| child.get(path.len()) == Some(&b'/'))
                .map(|(child, &bytes)| (child, bytes))
                .collect();
            children.sort_by_key(|&(child, bytes)| (Reverse(bytes), child.clone()));
            let other_children_bytes = children.iter().skip(CHILD_ROWS).map(|c| c.1).sum();
            UsageRow {
                entry: usage_entry(path, bytes, used),
                children: children
                    .into_iter()
                    .take(CHILD_ROWS)
                    .map(|(child, bytes)| usage_entry(child, bytes, used))
                    .collect(),
                other_children_bytes,
            }
        })
        .collect();
    rows.sort_by_key(|row| (Reverse(row.entry.bytes), row.entry.path.path.clone()));
    rows
}

fn owners(walk: &Walk, used: u64, package_names: &BTreeMap<Vec<u8>, String>) -> Vec<OwnerRow> {
    let mut rows: Vec<OwnerRow> = walk
        .owners
        .iter()
        .filter(|&(_, &bytes)| bytes > 0)
        .map(|(owner, &bytes)| OwnerRow {
            category: owner.category,
            name: owner.name.as_deref().map(escape_human),
            display_name: match owner.category {
                Category::PackageData => owner
                    .name
                    .as_ref()
                    .and_then(|id| package_names.get(id))
                    .cloned(),
                _ => None,
            },
            bytes,
            percent_of_used: percent(bytes, used),
        })
        .collect();
    rows.sort_by_key(|row| Reverse(row.bytes));
    rows
}

/// Rows smaller than this are folded into one [`SmallRows`].
pub const SMALL_ROW_BYTES: u64 = 1 << 20;

/// Splits `rows` into those of at least [`SMALL_ROW_BYTES`], in their
/// order, and the fold of the rest (`None` when there are none).
fn fold_small<T>(rows: Vec<T>, bytes: impl Fn(&T) -> u64) -> (Vec<T>, Option<SmallRows>) {
    let (kept, small): (Vec<T>, Vec<T>) = rows
        .into_iter()
        .partition(|row| bytes(row) >= SMALL_ROW_BYTES);
    let folded = (!small.is_empty()).then(|| SmallRows {
        below_bytes: SMALL_ROW_BYTES,
        items: small.len() as u64,
        bytes: small.iter().map(&bytes).sum(),
    });
    (kept, folded)
}

/// `candidates` totalled by category, largest first.
pub fn cleanable(candidates: &[Candidate]) -> Cleanable {
    let mut totals: BTreeMap<Category, (u64, u64)> = BTreeMap::new();
    for candidate in candidates {
        let total = totals.entry(candidate.category).or_default();
        total.0 += candidate.allocated_bytes;
        total.1 += 1;
    }
    let mut by_category: Vec<CategoryTotal> = totals
        .into_iter()
        .map(|(category, (bytes, items))| CategoryTotal {
            category,
            bytes,
            items,
        })
        .collect();
    by_category.sort_by_key(|total| Reverse(total.bytes));
    let bytes = by_category.iter().map(|t| t.bytes).sum::<u64>();
    Cleanable {
        bytes,
        required_destination_bytes: bytes.saturating_add(DESTINATION_MARGIN_BYTES),
        items: by_category.iter().map(|t| t.items).sum(),
        by_category,
    }
}

/// What each mode's plan would move at `now`, after the same size, age,
/// boot and pinned-file limits; only held-open files (read when planning)
/// may still stay.
fn report_cleanable(
    walk: &Walk,
    root_available: u64,
    now: i64,
    boot: i64,
    tools: Vec<Delegate>,
    deep_tools: Vec<Delegate>,
) -> ReportCleanable {
    let none = BTreeMap::new();
    let standard_candidates: Vec<Candidate> = walk
        .candidates
        .iter()
        .filter(|c| classify(&c.path, c.kind).movable)
        .cloned()
        .collect();
    let mut standard = select_mode(
        &standard_candidates,
        &none,
        now,
        boot,
        CleanupMode::Standard,
    );
    keep_pinned(&mut standard, &walk.pinned);
    let mut deep = select_mode(&walk.candidates, &none, now, boot, CleanupMode::Deep);
    keep_pinned(&mut deep, &walk.pinned);
    keep_backup_installer(&mut deep, root_available);
    let totals = cleanable(&standard.items);
    let deep = cleanable(&deep.items);
    ReportCleanable {
        tools,
        deep: DeepCleanable {
            additional_bytes: deep.bytes.saturating_sub(totals.bytes),
            totals: deep,
            tools: deep_tools,
        },
        totals,
    }
}

fn report_only(walk: &Walk) -> Vec<ReportOnly> {
    let mut rows: Vec<ReportOnly> = walk
        .owners
        .iter()
        .filter(|&(_, &bytes)| bytes > 0)
        .filter_map(|(owner, &bytes)| {
            let name = owner.name.as_deref().map(escape_human);
            Some(ReportOnly {
                category: owner.category,
                advice: advice(owner, name.as_deref())?,
                name,
                bytes,
            })
        })
        .collect();
    rows.sort_by_key(|row| Reverse(row.bytes));
    rows
}

/// The advice for an owner `rootfs plan` never selects; `None` for the
/// movable categories and for DSM's own files and anything unknown.
fn advice(owner: &Owner, name: Option<&str>) -> Option<RootfsAdvice> {
    let (code, message) = match owner.category {
        Category::PackageData => (
            AdviceCode::PackageData,
            match name {
                Some(name) => format!(
                    "managed by the package {name}: free it through the package (its settings or Package Center); never delete its files by hand"
                ),
                None => "DSM's package folders: free space through the packages in Package Center".to_string(),
            },
        ),
        Category::UserTooling => (
            AdviceCode::UserTooling,
            format!(
                "installed by you or your scripts in {}, not by DSM: move what you no longer need to a shared folder yourself; rootfs plan never selects it",
                name.unwrap_or("/root or /usr/local")
            ),
        ),
        Category::Logs => (
            AdviceCode::LiveLogs,
            "live logs, capped by DSM's /var/log quota: DSM rotates them; rootfs plan moves only the rotated archives".to_string(),
        ),
        Category::LogCenterDb => (
            AdviceCode::LogCenterDb,
            "Log Center's databases: archive or reduce them in Log Center, not by hand".to_string(),
        ),
        Category::GeoipDatabase => (
            AdviceCode::GeoipDatabase,
            "the firewall's GeoIP database: Synology support removes it and has DSM download it again (synodbudupdate -p geoip-database); rootfs plan leaves it".to_string(),
        ),
        Category::OwnState => (
            AdviceCode::OwnState,
            "synoreclaim's own state: `synoreclaim cache clear`, or scan with --state-dir on a data volume".to_string(),
        ),
        Category::DsmKeep => (
            AdviceCode::DsmKeep,
            "Synology says never to delete these (update configuration and evidence)".to_string(),
        ),
        Category::PreferenceImage
        | Category::SambaCache
        | Category::ToolCache
        | Category::BackupInstaller
        | Category::UpdateLeftover
        | Category::LogArchive
        | Category::CrashDump
        | Category::TempDebris
        | Category::DsmSystem
        | Category::Other => return None,
    };
    let advice = RootfsAdvice::new(code, message);
    Some(if code == AdviceCode::PackageData {
        advice.with_manage_in_id(ManageInId::PackageCenter)
    } else {
        advice
    })
}

fn open_deleted(scan: ProcessScan) -> OpenDeleted {
    OpenDeleted {
        bytes_estimate: scan.bytes_estimate,
        holders: scan
            .holders
            .into_iter()
            .map(|holder| Holder {
                advice: holder_advice(&holder.comm),
                holder,
            })
            .collect(),
        unreadable_processes: scan.unreadable_processes,
    }
}

fn holder_advice(comm: &str) -> RootfsAdvice {
    if comm == "syslog-ng" {
        RootfsAdvice::new(
            AdviceCode::RestartSyslogNg,
            "restarting syslog-ng frees it (synosystemctl restart syslog-ng), DSM's common workaround",
        )
    } else {
        RootfsAdvice::new(
            AdviceCode::RestartHolder,
            format!(
                "the space returns when {comm} closes the files: restart it, or the package or service that runs it"
            ),
        )
    }
}

/// A category as the human report names it.
pub fn category_label(category: Category) -> &'static str {
    match category {
        Category::UpdateLeftover => "update leftovers",
        Category::LogArchive => "rotated log archives",
        Category::CrashDump => "crash dumps",
        Category::TempDebris => "temporary debris",
        Category::Logs => "logs",
        Category::LogCenterDb => "Log Center databases",
        Category::GeoipDatabase => "GeoIP database",
        Category::PreferenceImage => "DSM user images",
        Category::SambaCache => "Winbind cache",
        Category::ToolCache => "package manager caches",
        Category::BackupInstaller => "Hyper Backup restore installer",
        Category::PackageData => "package",
        Category::UserTooling => "your tooling",
        Category::OwnState => "synoreclaim state",
        Category::DsmKeep => "DSM (keep)",
        Category::DsmSystem => "DSM",
        Category::Other => "",
    }
}

fn owner_label(category: Category, name: Option<&str>) -> String {
    match (category, name) {
        (Category::PackageData, Some(name)) => format!("package {name}"),
        (Category::PackageData, None) => "packages".to_string(),
        (Category::UserTooling, Some(folder)) => format!("your tooling ({folder})"),
        (category, _) => category_label(category).to_string(),
    }
}

/// The human `rootfs` report.
pub fn render(doc: &RootfsDocument) -> String {
    let mut out = String::new();
    render_header(doc, &mut out);
    render_usage(doc, &mut out);
    render_cleanable(doc, &mut out);
    render_report_only(doc, &mut out);
    render_open_deleted(doc, &mut out);
    for warning in &doc.envelope.warnings {
        out.push_str(&format!("warning: {}\n", warning.message));
    }
    out
}

fn render_header(doc: &RootfsDocument, out: &mut String) {
    let fs = &doc.filesystem;
    out.push_str(&format!(
        "system partition /  {}  {}\n  {} total, {} used ({}%), {} available\n",
        escape_human(fs.source.as_bytes()),
        fs.fstype,
        human_bytes(fs.total_bytes),
        human_bytes(fs.used_bytes),
        fs.used_percent,
        human_bytes(fs.available_bytes),
    ));
    out.push_str(&format!(
        "  inodes {} of {} used ({}%)\n",
        fs.inodes_used,
        fs.inodes_total,
        df_percent(fs.inodes_used, fs.inodes_total - fs.inodes_used)
    ));
    match &doc.log_quota {
        Some(q) => out.push_str(&format!(
            "  /var/log quota {} used of {} (hard limit {})\n",
            human_bytes(q.quota.used_bytes),
            human_bytes(q.quota.soft_limit_bytes),
            human_bytes(q.quota.hard_limit_bytes),
        )),
        None => out.push_str("  /var/log quota not found\n"),
    }
    out.push_str(&format!(
        "  walked {} entries in {:.1} s{}\n",
        doc.walk.entries,
        doc.walk.elapsed_ms as f64 / 1000.0,
        if doc.walk.complete {
            ""
        } else {
            " (stopped early)"
        }
    ));
}

fn render_usage(doc: &RootfsDocument, out: &mut String) {
    out.push_str("\nWHERE THE SPACE GOES     BYTES   USED  OWNER\n");
    let line = |entry: &UsageEntry, indent: &str| {
        let label = owner_label(entry.category, entry.package.as_deref());
        format!(
            "{:<22} {:>9} {:>5.1}%  {}\n",
            format!("{indent}{}", escape_human(&entry.path.bytes())),
            human_bytes(entry.bytes),
            entry.percent_of_used,
            label
        )
        .replace("  \n", "\n")
    };
    for row in doc.usage.iter().take(HUMAN_ROWS) {
        out.push_str(&line(&row.entry, ""));
        for child in row.children.iter().take(HUMAN_CHILD_ROWS) {
            out.push_str(&line(child, "  "));
        }
    }
    for path in &doc.not_walked {
        if path.reason == NotWalkedReason::MountPoint {
            out.push_str(&format!(
                "{:<22} another filesystem, not counted\n",
                escape_human(&path.path.bytes())
            ));
        }
    }
    let known: Vec<String> = doc
        .owners
        .iter()
        .filter(|row| row.category != Category::Other && row.bytes >= 1 << 20)
        .map(|row| {
            format!(
                "{} {}",
                owner_label(row.category, row.name.as_deref()),
                human_bytes(row.bytes)
            )
        })
        .collect();
    if !known.is_empty() {
        out.push_str(&format!("by owner: {}\n", known.join(", ")));
    }
}

fn render_cleanable(doc: &RootfsDocument, out: &mut String) {
    let cleanable = &doc.cleanable.totals;
    let deep = &doc.cleanable.deep;
    if cleanable.items == 0 {
        out.push_str("\nnothing for rootfs plan to move\n");
    } else {
        out.push_str(&format!(
            "\nrootfs plan --to <shared folder> could move {} in {} items off /:\n",
            human_bytes(cleanable.bytes),
            cleanable.items
        ));
        render_category_totals(cleanable, out);
    }
    if deep.additional_bytes > 0 {
        out.push_str(&format!(
            "rootfs plan --mode deep could move {} in {} items in all\n",
            human_bytes(deep.totals.bytes),
            deep.totals.items
        ));
    }
}

/// One `  <category>  <bytes>  <n> items` line per category.
pub fn render_category_totals(cleanable: &Cleanable, out: &mut String) {
    for total in &cleanable.by_category {
        out.push_str(&format!(
            "  {:<22} {:>9}  {} items\n",
            category_label(total.category),
            human_bytes(total.bytes),
            total.items
        ));
    }
}

fn render_report_only(doc: &RootfsDocument, out: &mut String) {
    if doc.report_only.is_empty() {
        return;
    }
    out.push_str("\nreport only, never moved by synoreclaim:\n");
    for row in &doc.report_only {
        out.push_str(&format!(
            "  {:>9}  {}: {}\n",
            human_bytes(row.bytes),
            owner_label(row.category, row.name.as_deref()),
            row.advice.message
        ));
    }
    if let Some(small) = &doc.report_only_small {
        out.push_str(&format!(
            "  {:>9}  other items under {} ({} items)\n",
            human_bytes(small.bytes),
            human_bytes(small.below_bytes),
            small.items
        ));
    }
}

fn render_open_deleted(doc: &RootfsDocument, out: &mut String) {
    let open = &doc.open_deleted;
    if open.holders.is_empty() {
        out.push_str("\ndeleted but still open: none found\n");
        return;
    }
    out.push_str(&format!(
        "\ndeleted but still open: about {}\n",
        human_bytes(open.bytes_estimate)
    ));
    for holder in &open.holders {
        out.push_str(&format!(
            "  {:>9}  {} (pid {}, {} files): {}\n",
            human_bytes(holder.holder.bytes_estimate),
            holder.holder.comm,
            holder.holder.pid,
            holder.holder.inodes,
            holder.advice.message
        ));
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::rootfs::classify::Kind;
    use crate::rootfs::walk::{Candidate, Identity, NotWalked};

    pub const MIB: u64 = 1 << 20;

    fn candidate(path: &str, category: Category, bytes: u64) -> Candidate {
        Candidate {
            path: path.as_bytes().to_vec(),
            kind: Kind::File,
            category,
            identity: Identity {
                device: 2049,
                inode: 12,
                file_type: libc::S_IFREG,
                size: bytes,
                mtime: 1_790_000_000,
                mtime_nsec: 0,
                nlink: 1,
                ctime: 0,
            },
            allocated_bytes: bytes,
        }
    }

    fn owner(category: Category, name: Option<&str>) -> Owner {
        Owner {
            category,
            name: name.map(|n| n.as_bytes().to_vec()),
        }
    }

    /// A walk shaped like a test NAS's system partition (DSM 7.4, 2026-09-30).
    pub fn walk() -> Walk {
        let rows = [
            ("/usr", 2600 * MIB),
            ("/usr/local", 2500 * MIB),
            ("/usr/syno", 90 * MIB),
            ("/root", 461 * MIB),
            ("/var", 231 * MIB),
            ("/var/db", 87 * MIB),
            ("/var/log", 64 * MIB),
            ("/var/packages", 30 * MIB),
            ("/upd@te", 80 * MIB),
            ("/.SynoUpgrade.tar", 20 * MIB),
        ];
        let owners = [
            (owner(Category::UserTooling, Some("/usr/local")), 1967 * MIB),
            (owner(Category::PackageData, Some("SynoFinder")), 533 * MIB),
            (owner(Category::UserTooling, Some("/root")), 461 * MIB),
            (owner(Category::DsmSystem, None), 90 * MIB),
            (owner(Category::GeoipDatabase, None), 84 * MIB),
            (owner(Category::Logs, None), 54 * MIB),
            (owner(Category::LogArchive, None), 10 * MIB),
            (owner(Category::UpdateLeftover, None), 100 * MIB),
            (owner(Category::Other, None), 83 * MIB),
            (owner(Category::PackageData, Some("Spreadsheet")), 100 << 10),
            (owner(Category::PackageData, Some("Git")), MIB - 1),
        ];
        Walk {
            entries: 133_740,
            allocated_bytes: 3392 * MIB,
            rows: rows
                .into_iter()
                .map(|(p, b)| (p.as_bytes().to_vec(), b))
                .collect(),
            owners: owners.into_iter().collect(),
            candidates: vec![
                candidate("/.SynoUpgrade.tar", Category::UpdateLeftover, 20 * MIB),
                candidate("/upd@te/hda1.tgz", Category::UpdateLeftover, 80 * MIB),
                candidate("/var/log/disk-latency/a.xz", Category::LogArchive, 10 * MIB),
            ],
            not_walked: vec![NotWalked {
                path: b"/volume1".to_vec(),
                reason: NotWalkedReason::MountPoint,
            }],
            unreadable: 0,
            stopped: None,
            pinned: Vec::new(),
        }
    }

    pub fn header(available: u64) -> Header {
        Header {
            source: "/dev/md0".into(),
            fstype: "ext4".into(),
            total_bytes: 8 << 30,
            free_bytes: available + (100 << 20),
            available_bytes: available,
            inodes_total: 524_288,
            inodes_free: 390_546,
        }
    }

    pub fn open_deleted() -> ProcessScan {
        ProcessScan {
            bytes_estimate: 12 * MIB,
            holders: vec![OpenHolder {
                pid: 1234,
                comm: "syslog-ng".into(),
                inodes: 2,
                bytes_estimate: 12 * MIB,
            }],
            unreadable_processes: 0,
        }
    }

    pub fn document(walk: &Walk, available: u64) -> RootfsDocument {
        build(
            Inputs {
                header: header(available),
                log_quota: Some(LogQuota {
                    project_id: 1001,
                    used_bytes: 65_116 * 1024,
                    soft_limit_bytes: 204_800 * 1024,
                    hard_limit_bytes: 225_280 * 1024,
                }),
                walk,
                elapsed: Duration::from_millis(12_345),
                open_deleted: open_deleted(),
                tools: vec![
                    Delegate::RotateLogs,
                    Delegate::DeleteJournal,
                    Delegate::RestartSyslogNg,
                ],
                deep_tools: vec![
                    Delegate::DeleteLog,
                    Delegate::Logrotate,
                    Delegate::DeleteJournal,
                    Delegate::RestartSyslogNg,
                ],
                // After the fixture's leftovers changed: none is a waiting update.
                boot: 1_790_000_001,
                package_names: &BTreeMap::from([(
                    b"SynoFinder".to_vec(),
                    "Universal Search".to_string(),
                )]),
            },
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{MIB, document, walk};
    use super::*;

    const GIB: u64 = 1 << 30;

    fn json(doc: &RootfsDocument) -> serde_json::Value {
        serde_json::to_value(doc).unwrap()
    }

    #[test]
    fn the_header_has_df_numbers_inodes_and_the_log_quota() {
        let walk = walk();
        let doc = json(&document(&walk, 4 * GIB));
        let fs = &doc["filesystem"];
        assert_eq!(doc["command"], "rootfs");
        assert_eq!(doc["completion"], "complete");
        assert_eq!(fs["mount"], "/");
        assert_eq!(fs["source"], "/dev/md0");
        assert_eq!(fs["used_bytes"], 4 * GIB - (100 << 20));
        assert_eq!(fs["available_bytes"], 4 * GIB);
        assert_eq!(fs["used_percent"], 50);
        assert_eq!(fs["inodes_used"], 133_742);
        assert_eq!(fs["level"], "ok");
        assert_eq!(fs["level_reasons"], serde_json::json!([]));
        assert_eq!(fs["nearly_full"], false);
        assert_eq!(doc["log_quota"]["path"], "/var/log");
        assert_eq!(doc["log_quota"]["project_id"], 1001);
        assert_eq!(doc["log_quota"]["hard_limit_bytes"], 225_280 * 1024);
        assert_eq!(
            doc["cleanable"]["required_destination_bytes"],
            doc["cleanable"]["bytes"].as_u64().unwrap()
                + crate::rootfs::plan::DESTINATION_MARGIN_BYTES
        );
        assert_eq!(doc["walk"]["complete"], true);
        assert_eq!(doc["walk"]["elapsed_ms"], 12_345);
        assert_eq!(doc["warnings"], serde_json::json!([]));
    }

    #[test]
    fn filesystem_level_uses_the_shared_system_partition_rules() {
        let walk = walk();
        let doc = json(&document(&walk, 518_999_999));
        assert_eq!(doc["filesystem"]["level"], "full");
        assert_eq!(
            doc["filesystem"]["level_reasons"],
            serde_json::json!([
                "used_over_recommended",
                "update_headroom_low",
                "update_may_fail"
            ])
        );
    }

    #[test]
    fn low_space_warns_about_dsm_updates_and_a_nearly_full_partition() {
        let walk = walk();
        let tight = document(&walk, 900 << 20);
        let codes: Vec<&str> = tight.envelope.warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, ["update_headroom_low"]);
        assert!(
            tight.envelope.warnings[0]
                .message
                .contains("patch size plus 130 MB")
        );
        let full = document(&walk, 300 << 20);
        let codes: Vec<&str> = full.envelope.warnings.iter().map(|w| w.code).collect();
        assert_eq!(
            codes,
            ["system_partition_nearly_full", "update_headroom_low"]
        );
    }

    #[test]
    fn inconsistent_partition_space_keeps_the_rootfs_report_readable() {
        let walk = walk();
        let mut header = super::testing::header(4 * GIB);
        header.total_bytes = 0;
        let doc = build(
            Inputs {
                header,
                log_quota: None,
                walk: &walk,
                elapsed: Duration::ZERO,
                open_deleted: super::testing::open_deleted(),
                tools: Vec::new(),
                deep_tools: Vec::new(),
                boot: i64::MAX,
                package_names: &BTreeMap::new(),
            },
            SystemTime::UNIX_EPOCH,
        );

        assert!(!doc.filesystem.nearly_full);
        assert_eq!(doc.filesystem.level, None);
        assert_eq!(doc.filesystem.level_reasons, None);
        let serialized = serde_json::to_value(&doc).unwrap();
        assert!(serialized.pointer("/filesystem/level").is_none());
        assert!(serialized.pointer("/filesystem/level_reasons").is_none());
        assert!(
            doc.envelope
                .warnings
                .iter()
                .all(|warning| warning.code != "system_partition_nearly_full")
        );
    }

    #[test]
    fn usage_ranks_first_level_paths_with_their_largest_children() {
        let walk = walk();
        let doc = json(&document(&walk, 4 * GIB));
        let usage = doc["usage"].as_array().unwrap();
        let paths: Vec<&str> = usage.iter().map(|r| r["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            ["/usr", "/root", "/var", "/upd@te", "/.SynoUpgrade.tar"]
        );
        assert_eq!(usage[0]["category"], "dsm_system");
        assert_eq!(usage[0]["percent_of_used"], 65.0);
        let children: Vec<&str> = usage[0]["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["path"].as_str().unwrap())
            .collect();
        assert_eq!(children, ["/usr/local", "/usr/syno"]);
        assert_eq!(usage[0]["children"][0]["category"], "user_tooling");
        assert_eq!(usage[1]["category"], "user_tooling");
        assert_eq!(usage[2]["children"][2]["category"], "package_data");
        assert_eq!(usage[3]["category"], "update_leftover");
        assert_eq!(usage[3]["other_children_bytes"], 0);
    }

    #[test]
    fn cleanable_totals_what_plan_could_move_by_category() {
        let walk = walk();
        let doc = json(&document(&walk, 4 * GIB));
        assert_eq!(doc["cleanable"]["bytes"], 110 << 20);
        assert_eq!(doc["cleanable"]["items"], 3);
        assert_eq!(
            doc["cleanable"]["by_category"][0]["category"],
            "update_leftover"
        );
        assert_eq!(doc["cleanable"]["by_category"][0]["bytes"], 100 << 20);
        assert_eq!(
            doc["cleanable"]["by_category"][1]["category"],
            "log_archive"
        );
    }

    #[test]
    fn cleanable_lists_the_tools_a_plan_would_run() {
        let walk = walk();
        let doc = json(&document(&walk, 4 * GIB));
        assert_eq!(
            doc["cleanable"]["tools"],
            serde_json::json!(["rotate_logs", "delete_journal", "restart_syslog_ng"])
        );
        assert_eq!(
            doc["cleanable"]["deep"]["tools"],
            serde_json::json!([
                "delete_log",
                "logrotate",
                "delete_journal",
                "restart_syslog_ng"
            ])
        );
    }

    #[test]
    fn cleanable_matches_what_each_mode_s_plan_would_move() {
        use crate::rootfs::walk::Identity;
        const NOW: i64 = 1_790_000_000;
        let file = |path: &str, category: Category, bytes: u64, mtime: i64| Candidate {
            path: path.as_bytes().to_vec(),
            kind: crate::rootfs::classify::Kind::File,
            category,
            identity: Identity {
                device: 2049,
                inode: 12,
                file_type: libc::S_IFREG,
                size: bytes,
                mtime,
                mtime_nsec: 0,
                nlink: 1,
                ctime: 0,
            },
            allocated_bytes: bytes,
        };
        let old = NOW - 30 * 24 * 3600;
        let mut walk = walk();
        walk.candidates = vec![
            file(
                "/.log.junior/logs.tar.1",
                Category::UpdateLeftover,
                512 << 10,
                old,
            ),
            file("/root/.npm/_cacache/x", Category::ToolCache, 95 * MIB, old),
            file(
                "/var/db/geoip-database/GeoLite2-City.mmdb",
                Category::GeoipDatabase,
                80 * MIB,
                old,
            ),
            file(
                "/var/log/messages.1.xz",
                Category::LogArchive,
                10 * MIB,
                old,
            ),
            file("/var/log/messages.2.xz", Category::LogArchive, MIB, NOW),
            file(
                "/var/synobackup/HyperBackup-ED/HyperBackup-ED.latest.spk",
                Category::BackupInstaller,
                19 * MIB,
                old,
            ),
        ];

        let short = json(&document(&walk, 206 * MIB));
        assert_eq!(
            short["cleanable"]["bytes"],
            10 * MIB,
            "standard keeps junior and recent logs"
        );
        assert_eq!(short["cleanable"]["items"], 1);
        let deep = &short["cleanable"]["deep"];
        let deep_bytes = (512 << 10) + 95 * MIB + 80 * MIB + 11 * MIB + 19 * MIB;
        assert_eq!(deep["bytes"], deep_bytes);
        assert_eq!(deep["items"], 6);
        assert_eq!(deep["additional_bytes"], deep_bytes - 10 * MIB);
        assert_eq!(
            deep["required_destination_bytes"],
            deep_bytes + crate::rootfs::plan::DESTINATION_MARGIN_BYTES
        );
        assert_eq!(deep["by_category"][0]["category"], "tool_cache");

        let roomy = json(&document(&walk, 400 * MIB));
        assert_eq!(roomy["cleanable"]["deep"]["bytes"], deep_bytes - 19 * MIB);
    }

    #[test]
    fn package_advice_names_package_center_as_its_management_destination() {
        let walk = walk();
        let doc = json(&document(&walk, 4 * GIB));
        let package = doc["report_only"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["advice"]["code"] == "package_data")
            .unwrap();
        assert_eq!(package["advice"]["manage_in_id"], "package_center");
    }

    #[test]
    fn report_only_owners_carry_advice_and_movable_or_dsm_owners_do_not() {
        let walk = walk();
        let doc = document(&walk, 4 * GIB);
        let rows: Vec<(Category, Option<&str>, AdviceCode)> = doc
            .report_only
            .iter()
            .map(|r| (r.category, r.name.as_deref(), r.advice.code))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    Category::UserTooling,
                    Some("/usr/local"),
                    AdviceCode::UserTooling
                ),
                (
                    Category::PackageData,
                    Some("SynoFinder"),
                    AdviceCode::PackageData
                ),
                (
                    Category::UserTooling,
                    Some("/root"),
                    AdviceCode::UserTooling
                ),
                (Category::GeoipDatabase, None, AdviceCode::GeoipDatabase),
                (Category::Logs, None, AdviceCode::LiveLogs),
            ]
        );
        assert!(doc.report_only[1].advice.message.contains("SynoFinder"));
    }

    #[test]
    fn owners_under_1_mib_fold_into_one_row_per_list() {
        let walk = walk();
        let doc = document(&walk, 4 * GIB);
        let folded = Some(SmallRows {
            below_bytes: SMALL_ROW_BYTES,
            items: 2,
            bytes: (100 << 10) + MIB - 1,
        });
        assert_eq!(doc.report_only.len(), 5);
        assert_eq!(doc.report_only_small, folded);
        assert!(doc.owners.iter().all(|row| row.bytes >= SMALL_ROW_BYTES));
        assert_eq!(doc.owners_small, folded);
        let text = render(&doc);
        assert!(
            text.contains("  1.1 MB  other items under 1 MB (2 items)\n"),
            "{text}"
        );

        let mut large = super::testing::walk();
        large.owners.retain(|_, bytes| *bytes >= SMALL_ROW_BYTES);
        let unfolded = document(&large, 4 * GIB);
        assert_eq!(unfolded.report_only_small, None);
        assert_eq!(unfolded.owners_small, None);
    }

    #[test]
    fn deleted_open_files_name_their_holder_and_how_to_free_them() {
        let walk = walk();
        let doc = json(&document(&walk, 4 * GIB));
        let holder = &doc["open_deleted"]["holders"][0];
        assert_eq!(doc["open_deleted"]["bytes_estimate"], 12 << 20);
        assert_eq!(holder["comm"], "syslog-ng");
        assert_eq!(holder["pid"], 1234);
        assert_eq!(holder["advice"]["code"], "restart_syslog_ng");
        assert_eq!(holder_advice("smbd").code, AdviceCode::RestartHolder);
    }

    #[test]
    fn a_walk_that_stopped_early_is_partial_or_paused() {
        let mut walk = walk();
        walk.stopped = Some(WalkStop::EntryLimit);
        let doc = document(&walk, 4 * GIB);
        assert_eq!(doc.envelope.completion, Completion::Partial);
        assert_eq!(doc.walk.stop_reason, Some("entry_limit"));
        assert_eq!(doc.envelope.warnings[0].code, "walk_incomplete");
        walk.stopped = Some(WalkStop::Stopped(StopReason::Paused));
        assert_eq!(
            document(&walk, 4 * GIB).envelope.completion,
            Completion::Paused
        );
        walk.stopped = None;
        walk.unreadable = 2;
        let unreadable = document(&walk, 4 * GIB);
        assert_eq!(unreadable.envelope.completion, Completion::Partial);
        assert_eq!(
            unreadable.envelope.warnings[0].code,
            "unreadable_directories"
        );
    }

    #[test]
    fn the_synthetic_report_is_the_json_golden() {
        let walk = walk();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/json/rootfs.json");
        let mut rendered = json(&document(&walk, 4 * GIB));
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        rendered["tool_version"] = golden["tool_version"].clone();
        assert_eq!(rendered, golden);
    }

    #[test]
    fn the_human_report_shows_space_then_usage_then_cleanable_then_open_files() {
        let walk = walk();
        let text = render(&document(&walk, 4 * GIB));
        let expected = [
            "system partition /  /dev/md0  ext4\n  8 GB total, 3.9 GB used (50%), 4 GB available\n",
            "  inodes 133742 of 524288 used (26%)\n",
            "  /var/log quota 63.6 MB used of 200 MB (hard limit 220 MB)\n",
            "  walked 133740 entries in 12.3 s\n",
            "WHERE THE SPACE GOES     BYTES   USED  OWNER\n",
            "/usr                      2.5 GB  65.0%  DSM\n",
            "  /usr/local              2.4 GB  62.5%  your tooling\n",
            "/var                      231 MB   5.7%\n",
            "  /var/packages            30 MB   0.7%  packages\n",
            "/volume1               another filesystem, not counted\n",
            "by owner: your tooling (/usr/local) 1.9 GB, package SynoFinder 533 MB",
            "rootfs plan --to <shared folder> could move 110 MB in 3 items off /:\n",
            "  update leftovers          100 MB  2 items\n",
            "report only, never moved by synoreclaim:\n",
            "     533 MB  package SynoFinder: managed by the package SynoFinder",
            "deleted but still open: about 12 MB\n      12 MB  syslog-ng (pid 1234, 2 files): restarting syslog-ng",
        ];
        let mut rest = text.as_str();
        for part in expected {
            let at = rest
                .find(part)
                .unwrap_or_else(|| panic!("{part:?} not in order in:\n{text}"));
            rest = &rest[at + part.len()..];
        }
    }
}
