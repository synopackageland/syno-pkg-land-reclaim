//! Privacy-preserving summaries for support tickets (SPEC 5.17).

use crate::btrfs::live::OpenTarget;
use crate::cli::{AnswerArgs, OutputArgs, ScanLimits, TargetArgs};
use crate::dsm;
use crate::jobs;
use crate::model::namespace::FileKind;
use crate::model::tree::Metrics;
use crate::query::{self, KindFilter, Measure, TopQuery};
use crate::report::{ChangeReason, Header, ReportError, ReportReader};
use crate::resolve::{self, Environment, fsid_text};
use crate::resource::Profile;
use crate::state::Store;
use crate::target::Scope;
use serde::Serialize;
use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime};

const SCHEMA_VERSION: u32 = 1;
const TOP_ROWS_AFTER_TRIM: usize = 20;
const SNAPSHOT_ROWS_AFTER_TRIM: usize = 10;

#[derive(Debug, thiserror::Error)]
enum BundleError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(
        "support bundle still needs {actual} bytes after detail reduction; --max-bytes is {limit}"
    )]
    TooLarge { actual: usize, limit: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NameKind {
    Volume,
    Share,
    Directory,
    File,
    Snapshot,
    Filesystem,
}

impl NameKind {
    fn prefix(self) -> &'static str {
        match self {
            NameKind::Volume => "volume",
            NameKind::Share => "share",
            NameKind::Directory => "dir",
            NameKind::File => "file",
            NameKind::Snapshot => "snapshot",
            NameKind::Filesystem => "filesystem",
        }
    }
}

#[derive(Default)]
struct TokenRegistry {
    include_paths: bool,
    tokens: HashMap<Vec<u8>, String>,
    next: HashMap<NameKind, usize>,
}

impl TokenRegistry {
    fn token(&mut self, kind: NameKind, value: &[u8]) -> String {
        let key = value.to_vec();
        if let Some(token) = self.tokens.get(&key) {
            return token.clone();
        }
        let next = self.next.entry(kind).or_default();
        *next += 1;
        let token = format!("{}-{next:02}", kind.prefix());
        self.tokens.insert(key, token.clone());
        token
    }

    fn name(&mut self, kind: NameKind, value: &[u8]) -> String {
        if self.include_paths {
            String::from_utf8_lossy(value).into_owned()
        } else {
            self.token(kind, value)
        }
    }

    fn token_path(&mut self, path: &[u8], mount: &[u8], leaf: NameKind) -> String {
        if self.include_paths {
            return String::from_utf8_lossy(path).into_owned();
        }
        let Some(rest) = path
            .strip_prefix(mount)
            .filter(|rest| rest.is_empty() || rest.starts_with(b"/"))
        else {
            return self.token(NameKind::Directory, path);
        };
        let mut tokens = vec![self.token(NameKind::Volume, mount)];
        let components: Vec<&[u8]> = rest
            .split(|byte| *byte == b'/')
            .filter(|part| !part.is_empty())
            .collect();
        for (index, component) in components.iter().enumerate() {
            let kind = if is_fixed_dsm_name(component) {
                tokens.push(String::from_utf8_lossy(component).into_owned());
                continue;
            } else if components.get(index.wrapping_sub(1)) == Some(&&b"#snapshot"[..]) {
                NameKind::Snapshot
            } else if index == 0 {
                NameKind::Share
            } else if index + 1 == components.len() {
                leaf
            } else {
                NameKind::Directory
            };
            tokens.push(self.name(kind, component));
        }
        tokens.join("/")
    }
}

fn is_fixed_dsm_name(name: &[u8]) -> bool {
    matches!(name, b"#recycle" | b"#snapshot" | b"@eaDir" | b"@sharesnap")
        || dsm::is_fixed_service_name(name)
}

#[derive(Debug, Default, Serialize)]
struct BundleDocument {
    tool_version: String,
    schema_version: u32,
    command: String,
    system: SystemSummary,
    volumes: Vec<VolumeSummary>,
    doctor_checks: Vec<DoctorCheck>,
    overview: Option<OverviewSummary>,
    scan_summary: Option<ScanSummary>,
    top_rows: Vec<TopSummary>,
    why_not_freed: Option<WhyNotFreedSummary>,
    snapshots: Vec<SnapshotGroup>,
    active_jobs: Vec<ActiveJobState>,
    report_missing: bool,
    include_paths: bool,
    warnings: Vec<&'static str>,
    missing_parts: Vec<&'static str>,
    dropped: DroppedDetail,
}

#[derive(Debug, Default, Serialize)]
struct SystemSummary {
    dsm_version: Option<String>,
    dsm_build: Option<u32>,
    model_family: Option<String>,
    kernel: Option<String>,
    architecture: String,
}

#[derive(Debug, Default, Serialize)]
struct VolumeSummary {
    filesystem: Option<String>,
    mount: String,
    total_bytes: Option<u64>,
    used_bytes: Option<u64>,
    free_bytes: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
struct DoctorCheck {
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct OverviewSummary {
    completion: crate::document::Completion,
    parts: Vec<OverviewPartSummary>,
    reconciliation: crate::document::OverviewReconciliation,
}

#[derive(Debug, Serialize)]
struct OverviewPartSummary {
    kind: crate::overview::PartKind,
    bytes: u64,
    reclaimable_bytes: Option<u64>,
    held_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reasons: Vec<OverviewReasonSummary>,
}

#[derive(Debug, Serialize)]
struct OverviewReasonSummary {
    code: crate::overview::UnknownReason,
    bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct MetricSummary {
    logical_bytes: u64,
    physical_bytes: u64,
    reclaimable_bytes: Option<u64>,
    unknown_bytes: u64,
    pinned_unreferenced_bytes: u64,
    entries: u64,
    files: u64,
}

impl From<&Metrics> for MetricSummary {
    fn from(metrics: &Metrics) -> Self {
        MetricSummary {
            logical_bytes: metrics.logical,
            physical_bytes: metrics.physical,
            reclaimable_bytes: (metrics.unknown == 0).then_some(metrics.reclaimable),
            unknown_bytes: metrics.unknown,
            pinned_unreferenced_bytes: metrics.pinned,
            entries: metrics.entries,
            files: metrics.files,
        }
    }
}

#[derive(Debug, Serialize)]
struct ScanSummary {
    consistency: &'static str,
    scan_started: String,
    scan_finished: String,
    change_reasons: Vec<&'static str>,
    coverage: crate::report::Coverage,
    timings_and_memory: crate::report::ScanStats,
}

#[derive(Debug, Clone, Serialize)]
struct TopSummary {
    name: String,
    kind: String,
    metrics: MetricSummary,
    service: Option<&'static str>,
    path_count: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
struct SnapshotGroup {
    share: String,
    materialized: usize,
    not_materialized: usize,
    rows: Vec<String>,
}

/// The `why-not-freed` rows without `--watch` or `--processes`.
#[derive(Debug, Serialize)]
struct WhyNotFreedSummary {
    completion: crate::document::Completion,
    reasons: Vec<ReasonSummary>,
}

#[derive(Debug, Serialize)]
struct ReasonSummary {
    code: String,
    unknown: bool,
    lower_bound: bool,
    bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    items: Vec<ShareBytes>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    parts: Vec<OverviewReasonSummary>,
}

#[derive(Debug, Serialize)]
struct ShareBytes {
    share: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct ActiveJobState {
    state: &'static str,
    phase: &'static str,
}

#[derive(Debug, Default, Serialize)]
struct DroppedDetail {
    top_rows: usize,
    snapshot_rows: usize,
    doctor_details: bool,
}

/// Writes a read-only bundle from the newest existing volume report. It
/// never starts a scan, and the caller receives 3 when report sections are
/// missing from the bundle.
pub fn run(
    target: &Path,
    output: &Path,
    include_paths: bool,
    max_bytes: u64,
    state_dir: &Path,
) -> Result<i32, Box<dyn Error>> {
    let env = Environment::load()?;
    let resolved = resolve::resolve_path(&env, target, Some(Scope::Volume)).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: {}", error.code, error.message),
        )
    })?;
    let mut tokens = TokenRegistry {
        include_paths,
        ..TokenRegistry::default()
    };
    let mut bundle = BundleDocument {
        tool_version: env!("CARGO_PKG_VERSION").into(),
        schema_version: SCHEMA_VERSION,
        command: "support-bundle".into(),
        system: system_summary(),
        volumes: volume_summaries(&env, &mut tokens),
        include_paths,
        warnings: if include_paths {
            vec!["Path names are included by explicit request."]
        } else {
            Vec::new()
        },
        ..BundleDocument::default()
    };

    let doctor = crate::doctor::run(resolved.path.clone(), Profile::Low, state_dir, 512 << 20);
    bundle.doctor_checks = doctor_checks(&doctor);
    bundle.active_jobs = active_job_states(state_dir)?;
    bundle.overview = overview_from_existing_report(&resolved.mount, state_dir)?;
    if bundle.overview.is_none() {
        bundle.missing_parts.push("overview");
    }
    bundle.why_not_freed =
        why_not_freed_from_existing_report(&resolved.mount, state_dir, &mut tokens)?;
    if bundle.why_not_freed.is_none() {
        bundle.missing_parts.push("why_not_freed");
    }

    let store = if state_dir.is_dir() {
        Some(Store::open(state_dir, crate::sys::effective_uid())?)
    } else {
        None
    };
    let latest = match store.as_ref() {
        Some(store) => store
            .reports(&fsid_text(&resolved.fsid))?
            .reports
            .into_iter()
            .find(|(_, header)| has_volume_root(header, resolved.mount.as_os_str().as_bytes())),
        None => None,
    };
    if let Some((report_path, _)) = latest {
        let reader = ReportReader::open(fs::File::open(report_path)?)?;
        let header = reader.header().clone();
        bundle.scan_summary = Some(scan_summary(&header));
        bundle.top_rows = top_rows(reader, &mut tokens)?;
        bundle.snapshots = snapshot_summaries(&header, &mut tokens);
        if bundle.snapshots.iter().all(|share| share.materialized == 0) {
            bundle.missing_parts.push("snapshots");
        }
    } else {
        bundle.report_missing = true;
        bundle.missing_parts.push("report");
        bundle.missing_parts.push("snapshots");
    }

    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let bytes = encode_limited(&mut bundle, max_bytes)?;
    write_output(output, &bytes)?;
    Ok(if bundle.missing_parts.is_empty() {
        0
    } else {
        3
    })
}

fn why_not_freed_from_existing_report(
    mount: &Path,
    state_dir: &Path,
    tokens: &mut TokenRegistry,
) -> Result<Option<WhyNotFreedSummary>, Box<dyn Error>> {
    if !state_dir.is_dir() {
        return Ok(None);
    }
    let (document, _) = crate::commands::why_not_freed_document(
        mount,
        crate::commands::WhyArgs {
            since: None,
            processes: false,
            max_age: Duration::from_secs(86_400),
            // Paths never enter a bundle.
            limit: 0,
        },
        OutputArgs {
            json: true,
            quiet: true,
        },
        state_dir,
    )?;
    Ok(document
        .results
        .first()
        .map(|result| why_not_freed_summary(document.envelope.completion, &result.reasons, tokens)))
}

fn why_not_freed_summary(
    completion: crate::document::Completion,
    entries: &[crate::document::ReasonEntry],
    tokens: &mut TokenRegistry,
) -> WhyNotFreedSummary {
    WhyNotFreedSummary {
        completion,
        reasons: entries
            .iter()
            .map(|entry| reason_summary(entry, tokens))
            .collect(),
    }
}

fn reason_summary(
    entry: &crate::document::ReasonEntry,
    tokens: &mut TokenRegistry,
) -> ReasonSummary {
    use crate::why_not_freed::Reason;
    let code = serde_json::to_value(&entry.reason).expect("a reason serializes")["code"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut summary = ReasonSummary {
        code,
        unknown: entry.unknown,
        lower_bound: false,
        bytes: None,
        count: None,
        items: Vec::new(),
        parts: Vec::new(),
    };
    let mut share = |name: &Option<crate::why_not_freed::ShareName>, id: u64, bytes: u64| {
        let raw = name.as_ref().map_or_else(
            || format!("subvolume {id}").into_bytes(),
            |n| {
                n.share_hex
                    .as_deref()
                    .and_then(crate::report::hex_bytes::decode)
                    .unwrap_or_else(|| n.share.clone().into_bytes())
            },
        );
        ShareBytes {
            share: tokens.name(NameKind::Share, &raw),
            bytes,
        }
    };
    match &entry.reason {
        Reason::PendingSubvolumeCleanup(row) => {
            summary.bytes = row.bytes;
            summary.count = row.count;
            summary.lower_bound = row.lower_bound;
        }
        Reason::HeldBySnapshots(row) => {
            summary.bytes = Some(row.bytes);
            summary.lower_bound = row.lower_bound;
            summary.items = row
                .items
                .iter()
                .map(|i| share(&i.name, i.subvolume_id, i.bytes))
                .collect();
        }
        Reason::RecycleBin(row) => {
            summary.bytes = Some(row.bytes);
            summary.lower_bound = row.lower_bound;
            summary.items = row
                .items
                .iter()
                .map(|i| share(&i.name, i.subvolume_id, i.bytes))
                .collect();
        }
        Reason::OpenUnlinked(row) => summary.bytes = Some(row.report_orphan_bytes),
        Reason::HardLink { bytes } => summary.bytes = *bytes,
        Reason::CommitPending { .. } => {}
        Reason::UnresolvedReferences(row) => {
            summary.bytes = Some(row.bytes);
            summary.parts = row
                .reasons
                .iter()
                .map(|part| OverviewReasonSummary {
                    code: part.code,
                    bytes: part.bytes,
                })
                .collect();
        }
    }
    summary
}

fn has_volume_root(header: &Header, mount: &[u8]) -> bool {
    header
        .roots
        .iter()
        .any(|root| root.scope == Scope::Volume && root.path == mount)
}

fn system_summary() -> SystemSummary {
    let version = fs::read_to_string(crate::cache::DSM_VERSION_FILE).ok();
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|value| value.trim().to_string());
    SystemSummary {
        dsm_version: version
            .as_deref()
            .and_then(|text| config_value(text, "productversion")),
        dsm_build: version
            .as_deref()
            .and_then(|text| config_value(text, "buildnumber"))
            .and_then(|value| value.parse().ok()),
        model_family: model_family(),
        kernel,
        architecture: std::env::consts::ARCH.into(),
    }
}

fn config_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

fn model_family() -> Option<String> {
    let synoinfo = fs::read_to_string("/etc.defaults/synoinfo.conf").ok()?;
    let unique = config_value(&synoinfo, "unique")?;
    let family = unique.strip_prefix("synology_")?.split('_').next()?;
    (!family.is_empty() && family.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .then(|| family.to_string())
}

fn volume_summaries(env: &Environment, tokens: &mut TokenRegistry) -> Vec<VolumeSummary> {
    env.volumes
        .iter()
        .map(|volume| {
            let mount = volume.primary.mount_point.as_os_str().as_bytes();
            let mut summary = VolumeSummary {
                filesystem: volume
                    .fsid
                    .as_ref()
                    .ok()
                    .map(|fsid| tokens.token(NameKind::Filesystem, fsid)),
                mount: tokens.token_path(mount, mount, NameKind::Directory),
                ..VolumeSummary::default()
            };
            if let Ok(opened) = OpenTarget::open(&volume.primary.mount_point)
                && let Ok(space) = crate::sys::filesystem_space(opened.file.as_fd())
            {
                summary.total_bytes = Some(space.total);
                summary.used_bytes = Some(space.total.saturating_sub(space.free));
                summary.free_bytes = Some(space.free);
            }
            summary
        })
        .collect()
}

fn doctor_checks(report: &crate::doctor::DoctorReport) -> Vec<DoctorCheck> {
    report
        .checks
        .iter()
        .map(|check| DoctorCheck {
            name: check.name.into(),
            status: match check.status {
                crate::doctor::Status::Pass => "pass",
                crate::doctor::Status::Warn => "warn",
                crate::doctor::Status::Fail => "fail",
                crate::doctor::Status::Skip => "skip",
            }
            .into(),
            detail: safe_doctor_detail(check.name, check.status),
        })
        .collect()
}

fn safe_doctor_detail(name: &str, status: crate::doctor::Status) -> Option<String> {
    let detail = match name {
        "filesystem" => "filesystem type checked",
        "privilege" if status == crate::doctor::Status::Pass => "running as root",
        "privilege" => "root access is required for full metadata access",
        "fs_info" => "Btrfs filesystem information checked",
        "features" => "Btrfs feature flags checked",
        "subvolume_id" => "target subvolume identity checked",
        "state_dir" => "state directory placement checked",
        "state_space" => "state directory free space checked",
        "system_partition" => "system partition checked",
        "scheduling" => "low-priority scheduling checked",
        "tree_search_v2" => "Btrfs metadata query checked",
        "subvolumes" => "subvolume discovery checked",
        "item_types" => "metadata item types sampled",
        "pending_deletion" => "pending deletion count checked",
        _ => return None,
    };
    Some(detail.into())
}

fn overview_from_existing_report(
    mount: &Path,
    state_dir: &Path,
) -> Result<Option<OverviewSummary>, Box<dyn Error>> {
    if !state_dir.is_dir() {
        return Ok(None);
    }
    let document = crate::commands::overview_document(
        TargetArgs {
            operands: vec![mount.as_os_str().to_os_string()],
            from_file: None,
            null: false,
            scope: Some(Scope::Volume),
        },
        AnswerArgs {
            refresh: false,
            no_scan: true,
            max_age: Duration::from_secs(86_400),
        },
        OutputArgs {
            json: true,
            quiet: true,
        },
        ScanLimits::report_only(),
        state_dir,
    )?;
    Ok(overview_summary(&document))
}

fn overview_summary(document: &crate::document::OverviewDocument) -> Option<OverviewSummary> {
    let result = document.results.first()?;
    Some(OverviewSummary {
        completion: document.envelope.completion,
        parts: result
            .parts
            .iter()
            .map(|part| OverviewPartSummary {
                kind: part.kind,
                bytes: part.bytes,
                reclaimable_bytes: part.reclaimable_bytes,
                held_bytes: part.held_bytes,
                reasons: part
                    .reasons
                    .iter()
                    .map(|reason| OverviewReasonSummary {
                        code: reason.code,
                        bytes: reason.bytes,
                    })
                    .collect(),
            })
            .collect(),
        reconciliation: result.reconciliation.clone(),
    })
}

fn scan_summary(header: &Header) -> ScanSummary {
    let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    ScanSummary {
        consistency: header.consistency.name(),
        scan_started: crate::time::rfc3339(at(header.scan_started)),
        scan_finished: crate::time::rfc3339(at(header.scan_finished)),
        change_reasons: header
            .change_reasons
            .iter()
            .map(change_reason_code)
            .collect(),
        coverage: header.coverage.clone(),
        timings_and_memory: header.stats.clone(),
    }
}

fn change_reason_code(reason: &ChangeReason) -> &'static str {
    match reason {
        ChangeReason::SubvolumeGenerationChanged { .. } => "subvolume_generation_changed",
        ChangeReason::SubvolumeAppeared { .. } => "subvolume_appeared",
        ChangeReason::SubvolumeVanished { .. } => "subvolume_vanished",
        ChangeReason::SubvolumeSnapshotted { .. } => "subvolume_snapshotted",
        ChangeReason::SubvolumeUuidChanged { .. } => "subvolume_uuid_changed",
        ChangeReason::MaintenanceRunning { .. } => "maintenance_running",
        ChangeReason::MaintenanceProbeFailed { .. } => "maintenance_probe_failed",
        ChangeReason::RelocationTree { .. } => "relocation_tree",
    }
}

fn top_rows<R: io::Read>(
    reader: ReportReader<R>,
    tokens: &mut TokenRegistry,
) -> Result<Vec<TopSummary>, ReportError> {
    let header_mount = reader.header().mount.clone();
    let mount = header_mount.as_slice();
    let query = TopQuery {
        under: mount,
        services: true,
        by: Measure::Reclaimable,
        kind: KindFilter::All,
        include_system: false,
        limit: 50,
        only_shared: false,
    };
    let mut walk = query::TopWalk::new(mount, &query);
    reader.for_each(|path, record| walk.visit(path, record))?;
    let Some(answer) = walk.finish() else {
        return Ok(Vec::new());
    };
    Ok(
        query::rows(&answer.ranking, &answer.services, Measure::Reclaimable, 50)
            .into_iter()
            .map(|row| match row {
                query::Row::Entry(entry) => {
                    let kind = if entry.kind == FileKind::Directory {
                        NameKind::Directory
                    } else {
                        NameKind::File
                    };
                    TopSummary {
                        name: tokens.token_path(&entry.path, mount, kind),
                        kind: entry.kind.letter().to_string(),
                        metrics: MetricSummary::from(entry.metrics_without_package_data()),
                        service: None,
                        path_count: None,
                    }
                }
                query::Row::Service(line) => TopSummary {
                    name: line.path.as_deref().map_or_else(
                        || {
                            line.service
                                .map_or("other_dsm_service_data".into(), |s| s.code.into())
                        },
                        |path| tokens.token_path(path, mount, NameKind::Directory),
                    ),
                    kind: "service".into(),
                    metrics: MetricSummary::from(&line.metrics),
                    service: line.service.map(|service| service.code),
                    path_count: Some(line.paths),
                },
            })
            .collect(),
    )
}

fn snapshot_summaries(header: &Header, tokens: &mut TokenRegistry) -> Vec<SnapshotGroup> {
    let Some(analysis) = &header.snapshots else {
        return Vec::new();
    };
    analysis
        .shares
        .iter()
        .map(|share| SnapshotGroup {
            share: tokens.name(NameKind::Share, &share.name),
            materialized: share
                .snapshots
                .iter()
                .filter(|item| item.materialized)
                .count(),
            not_materialized: share
                .snapshots
                .iter()
                .filter(|item| !item.materialized)
                .count(),
            rows: share
                .snapshots
                .iter()
                .map(|item| tokens.name(NameKind::Snapshot, &item.name))
                .collect(),
        })
        .collect()
}

fn active_job_states(state_dir: &Path) -> Result<Vec<ActiveJobState>, jobs::JobError> {
    jobs::list(&state_dir.join("jobs")).map(|jobs| {
        jobs.into_iter()
            .filter(|job| !job.state.is_finished())
            .map(|job| ActiveJobState {
                state: job.state.name(),
                phase: match job.phase.as_str() {
                    "discover" => "discover",
                    "scan" => "scan",
                    "resolve-refs" => "resolve-refs",
                    "aggregate" => "aggregate",
                    "publish" => "publish",
                    _ => "unknown",
                },
            })
            .collect()
    })
}

fn encode_limited(bundle: &mut BundleDocument, max_bytes: usize) -> Result<Vec<u8>, BundleError> {
    let mut encoded = json_bytes(bundle)?;
    if encoded.len() <= max_bytes {
        return Ok(encoded);
    }

    if bundle.top_rows.len() > TOP_ROWS_AFTER_TRIM {
        let removed = bundle.top_rows.len() - TOP_ROWS_AFTER_TRIM;
        bundle.top_rows.truncate(TOP_ROWS_AFTER_TRIM);
        bundle.dropped.top_rows += removed;
        encoded = json_bytes(bundle)?;
        if encoded.len() <= max_bytes {
            return Ok(encoded);
        }
    }

    for share in &mut bundle.snapshots {
        if share.rows.len() > SNAPSHOT_ROWS_AFTER_TRIM {
            let removed = share.rows.len() - SNAPSHOT_ROWS_AFTER_TRIM;
            share.rows.truncate(SNAPSHOT_ROWS_AFTER_TRIM);
            bundle.dropped.snapshot_rows += removed;
        }
    }
    encoded = json_bytes(bundle)?;
    if encoded.len() <= max_bytes {
        return Ok(encoded);
    }

    for check in &mut bundle.doctor_checks {
        check.detail = None;
    }
    bundle.dropped.doctor_details = true;
    encoded = json_bytes(bundle)?;
    if encoded.len() <= max_bytes {
        return Ok(encoded);
    }
    Err(BundleError::TooLarge {
        actual: encoded.len(),
        limit: max_bytes,
    })
}

fn json_bytes(bundle: &BundleDocument) -> Result<Vec<u8>, serde_json::Error> {
    let mut value = serde_json::to_value(bundle)?;
    redact_devices(&mut value, &mut HashMap::new());
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Replaces every device path (`/dev/...`) in the strings of `value`
/// with a token, whatever `--include-paths` says: a device names the
/// system, not the user's data.
fn redact_devices(value: &mut serde_json::Value, tokens: &mut HashMap<String, String>) {
    match value {
        serde_json::Value::String(text) => *text = redact_device_paths(text, tokens),
        serde_json::Value::Array(items) => {
            for item in items {
                redact_devices(item, tokens);
            }
        }
        serde_json::Value::Object(fields) => {
            for field in fields.values_mut() {
                redact_devices(field, tokens);
            }
        }
        _ => {}
    }
}

fn redact_device_paths(text: &str, tokens: &mut HashMap<String, String>) -> String {
    let is_path_char = |c: char| c.is_ascii_alphanumeric() || "._-/".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/dev/") {
        let starts_word = !rest[..at].ends_with(is_path_char);
        out.push_str(&rest[..at]);
        let path_len = rest[at..]
            .find(|c| !is_path_char(c))
            .unwrap_or(rest.len() - at);
        let path = &rest[at..at + path_len];
        if starts_word {
            let next = tokens.len() + 1;
            let token = tokens
                .entry(path.to_string())
                .or_insert_with(|| format!("device-{next:02}"));
            out.push_str(token);
        } else {
            out.push_str(path);
        }
        rest = &rest[at + path_len..];
    }
    out.push_str(rest);
    out
}

fn write_output(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path == Path::new("-") {
        return io::stdout().write_all(bytes);
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_why_not_freed_rows_with_share_tokens() {
        use crate::why_not_freed::{HeldBySnapshots, Reason, ShareName, SnapshotShare};
        let held = Reason::HeldBySnapshots(HeldBySnapshots {
            bytes: 5,
            multi_share_bytes: 0,
            items: vec![SnapshotShare {
                subvolume_id: 261,
                name: Some(ShareName {
                    share: "photos".into(),
                    share_hex: None,
                }),
                bytes: 5,
                snapshots: Some(3),
                newest_snapshot_otime: None,
                manage_in: crate::snapshots::SNAPSHOT_REPLICATION,
                manage_in_id: crate::dsm::place_id(crate::snapshots::SNAPSHOT_REPLICATION),
                files: Some(crate::snapshots::answer::HeldFilesJson {
                    complete: true,
                    count: 1,
                    files: vec![crate::snapshots::answer::HeldFileJson {
                        snapshot_id: 1000,
                        snapshot: crate::output::JsonPath::new(b"GMT+08-secret-snap"),
                        ino: 257,
                        path: Some(crate::output::JsonPath::new(b"secret-file.jpg")),
                        live_path: Some(crate::output::JsonPath::new(
                            b"/volume1/photos/secret-file.jpg",
                        )),
                        in_live: false,
                        bytes: 5,
                        unknown_bytes: 0,
                    }],
                }),
            }],
            lower_bound: true,
        });
        let entries = [
            crate::document::ReasonEntry::new(held, "/volume1"),
            crate::document::ReasonEntry::new(Reason::HardLink { bytes: None }, "/volume1"),
        ];
        let mut tokens = TokenRegistry::default();

        let summary =
            why_not_freed_summary(crate::document::Completion::Partial, &entries, &mut tokens);
        let json = serde_json::to_value(&summary).unwrap();

        assert_eq!(json["completion"], "partial");
        let rows = json["reasons"].as_array().unwrap();
        assert_eq!(rows[0]["code"], "held_by_snapshots");
        assert_eq!(rows[0]["bytes"], 5);
        assert_eq!(rows[0]["lower_bound"], true);
        assert_eq!(rows[0]["items"][0]["share"], "share-01");
        assert_eq!(rows[1]["code"], "hard_link");
        assert_eq!(rows[1]["bytes"], serde_json::Value::Null);
        assert!(!json.to_string().contains("photos"), "{json}");
        assert!(!json.to_string().contains("secret"), "{json}");
    }

    #[test]
    fn top_rows_show_folders_without_the_package_data_their_service_rows_count() {
        let bytes = crate::query::tests::package_report_bytes();
        let reader = crate::report::ReportReader::open(bytes.as_slice()).unwrap();
        let mut tokens = TokenRegistry {
            include_paths: true,
            ..TokenRegistry::default()
        };

        let rows = top_rows(reader, &mut tokens).unwrap();

        let share = rows.iter().find(|row| row.name == "/v/share").unwrap();
        assert_eq!(share.metrics.logical_bytes, 1 << 20);
    }

    #[test]
    fn keeps_fixed_dsm_names_but_tokenizes_names_that_only_match_a_pattern() {
        let mut tokens = TokenRegistry::default();
        let backup = tokens.token_path(
            b"/volume1/@ActiveBackup-zebra9/data",
            b"/volume1",
            NameKind::File,
        );
        let hyper = tokens.token_path(
            b"/volume1/photos/@eaDir/team-zebra9.hbk",
            b"/volume1",
            NameKind::Directory,
        );
        let marker = tokens.token_path(
            b"/volume1/photos/@ActiveBackup/x",
            b"/volume1",
            NameKind::File,
        );

        assert!(!backup.contains("zebra9"), "{backup}");
        assert!(!hyper.contains("zebra9"), "{hyper}");
        assert!(hyper.contains("/@eaDir/"), "{hyper}");
        assert!(marker.contains("/@ActiveBackup/"), "{marker}");
    }

    #[test]
    fn tokenizes_paths_stably_without_retaining_names() {
        let mut tokens = TokenRegistry::default();
        let first = tokens.token_path(
            b"/volume1/photos/private/movie.mkv",
            b"/volume1",
            NameKind::File,
        );
        let repeated = tokens.token_path(
            b"/volume1/photos/private/movie.mkv",
            b"/volume1",
            NameKind::File,
        );
        let another = tokens.token_path(
            b"/volume1/photos/private/other.mkv",
            b"/volume1",
            NameKind::File,
        );

        assert_eq!(first, "volume-01/share-01/dir-01/file-01");
        assert_eq!(repeated, first);
        assert_eq!(another, "volume-01/share-01/dir-01/file-02");
        assert!(!first.contains("volume1"));
        assert!(!first.contains("photos"));
        assert!(!first.contains("private"));
        assert!(!first.contains("movie.mkv"));
        assert_eq!(tokens.token(NameKind::Directory, b"photos"), "share-01");
    }

    #[test]
    fn always_tokenizes_filesystem_uuids_even_when_paths_are_included() {
        let mut tokens = TokenRegistry {
            include_paths: true,
            ..TokenRegistry::default()
        };
        let uuid = b"12345678-1234-5678-1234-567812345678";

        assert_eq!(tokens.token(NameKind::Filesystem, uuid), "filesystem-01");
        assert_eq!(tokens.token(NameKind::Filesystem, uuid), "filesystem-01");
    }

    #[test]
    fn enforces_size_cap_by_dropping_details_in_specified_order() {
        let long = "x".repeat(250);
        let mut bundle = BundleDocument {
            top_rows: vec![
                TopSummary {
                    name: long.clone(),
                    kind: "f".into(),
                    metrics: MetricSummary::default(),
                    service: None,
                    path_count: None,
                };
                50
            ],
            snapshots: vec![
                SnapshotGroup {
                    share: "share-01".into(),
                    materialized: 15,
                    not_materialized: 0,
                    rows: vec![long.clone(); 15],
                },
                SnapshotGroup {
                    share: "share-02".into(),
                    materialized: 15,
                    not_materialized: 0,
                    rows: vec![long.clone(); 15],
                },
            ],
            doctor_checks: vec![DoctorCheck {
                name: "features".into(),
                status: "pass".into(),
                detail: Some("diagnostic detail".repeat(8_000)),
            }],
            ..BundleDocument::default()
        };

        let bytes = encode_limited(&mut bundle, 20_000).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(bytes.len() <= 20_000);
        assert_eq!(decoded["top_rows"].as_array().unwrap().len(), 20);
        assert_eq!(
            decoded["snapshots"][0]["rows"].as_array().unwrap().len(),
            10
        );
        assert_eq!(
            decoded["snapshots"][1]["rows"].as_array().unwrap().len(),
            10
        );
        assert!(decoded["doctor_checks"][0]["detail"].is_null());
        assert_eq!(decoded["dropped"]["top_rows"], 30);
        assert_eq!(decoded["dropped"]["snapshot_rows"], 10);
        assert!(decoded["dropped"]["doctor_details"].as_bool().unwrap());
    }

    #[test]
    fn redacts_device_paths_in_every_string_even_when_paths_are_included() {
        let mut header = crate::report::testing::header(Vec::new());
        header.stats.pinned_leaf_reader_unavailable =
            Some("device /dev/mapper/cachedev_0: the device has no valid superblock".into());
        let mut bundle = BundleDocument {
            include_paths: true,
            scan_summary: Some(scan_summary(&header)),
            top_rows: vec![
                TopSummary {
                    name: "/volume1/dev/sda1".into(),
                    kind: "f".into(),
                    metrics: MetricSummary::default(),
                    service: None,
                    path_count: None,
                },
                TopSummary {
                    name: "/dev/sda1 and /dev/md2, then /dev/mapper/cachedev_0".into(),
                    kind: "f".into(),
                    metrics: MetricSummary::default(),
                    service: None,
                    path_count: None,
                },
            ],
            ..BundleDocument::default()
        };

        let bytes = encode_limited(&mut bundle, 1 << 20).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(
            decoded["scan_summary"]["timings_and_memory"]["pinned_leaf_reader_unavailable"],
            "device device-01: the device has no valid superblock"
        );
        assert_eq!(decoded["top_rows"][0]["name"], "/volume1/dev/sda1");
        assert_eq!(
            decoded["top_rows"][1]["name"],
            "device-02 and device-03, then device-01"
        );
    }

    #[test]
    fn creates_output_files_with_mode_0600() {
        use std::os::unix::fs::MetadataExt;

        let id = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("synoreclaim-bundle-{id}"));
        fs::create_dir(&directory).unwrap();
        let output = directory.join("bundle.json");

        write_output(&output, b"{}\n").unwrap();
        assert_eq!(fs::metadata(&output).unwrap().mode() & 0o777, 0o600);

        fs::remove_dir_all(directory).unwrap();
    }
}
