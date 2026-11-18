//! `doctor`: read-only environment diagnostics (SPEC 5.10). Evaluation and
//! rendering are pure; `run` gathers live facts on Linux.

use crate::btrfs::keys::{Key, describe_type_counts, is_known_fs_tree_type};
use crate::btrfs::search::{SearchRange, parse_batch};
use crate::btrfs::source::{MetadataSource, SourceError};
use crate::btrfs::subvolume::{Subvolume, SubvolumeSet};
use crate::btrfs::walk::WalkError;
use crate::btrfs::{BTRFS_SUPER_MAGIC, COMPAT_SYNO_CASELESS, FeatureFlags};
use crate::placement::Placement;
use crate::resource::SchedulingReport;
use crate::space::SystemPartition;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn new(name: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Check {
            name,
            status,
            detail: detail.into(),
        }
    }

    pub fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Check::new(name, Status::Pass, detail)
    }

    pub fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Check::new(name, Status::Warn, detail)
    }

    pub fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Check::new(name, Status::Fail, detail)
    }

    pub fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Check::new(name, Status::Skip, detail)
    }
}

/// Item types seen in the first search batch of a subvolume tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ItemSample {
    pub items: u64,
    pub unknown_types: BTreeMap<u8, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SubvolumeSummary {
    pub count: usize,
    pub readonly: usize,
    pub pending_deletion: usize,
    /// Data bytes the pending deletions hold: always unknown, since a
    /// root item counts only the bytes of its tree blocks (SPEC 5.10).
    pub pending_deletion_bytes: Option<u64>,
    pub quota_enabled: bool,
}

impl SubvolumeSummary {
    pub fn of(set: &SubvolumeSet) -> Self {
        let count_where =
            |f: fn(&Subvolume) -> bool| set.subvolumes.iter().filter(|s| f(s)).count();
        SubvolumeSummary {
            count: set.subvolumes.len(),
            readonly: count_where(|s| s.readonly),
            pending_deletion: count_where(|s| s.pending_deletion),
            pending_deletion_bytes: None,
            quota_enabled: set.quota_enabled,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub target: PathBuf,
    pub kernel: Option<String>,
    pub filesystem_uuid: Option<String>,
    pub subvolume_id: Option<u64>,
    pub checks: Vec<Check>,
    /// DSM's own system partition, separately from the data volume diagnostics.
    pub system_partition: Option<SystemPartition>,
    pub subvolumes: Option<SubvolumeSummary>,
    pub item_sample: Option<ItemSample>,
    pub scheduling: Option<SchedulingReport>,
}

impl DoctorReport {
    pub fn new(target: PathBuf) -> Self {
        DoctorReport {
            schema_version: SCHEMA_VERSION,
            target,
            kernel: None,
            filesystem_uuid: None,
            subvolume_id: None,
            checks: Vec::new(),
            system_partition: None,
            subvolumes: None,
            item_sample: None,
            scheduling: None,
        }
    }

    pub fn exit_code(&self) -> i32 {
        let failed = self.checks.iter().any(|c| c.status == Status::Fail);
        i32::from(failed)
    }

    pub fn render_human(&self) -> String {
        let mut out = format!("target: {}\n", self.target.display());
        for check in &self.checks {
            let _ = writeln!(
                out,
                "{}  {:<16} {}",
                check.status.label(),
                check.name,
                check.detail
            );
        }
        out
    }

    pub fn render_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("doctor report serializes")
    }
}

fn or_unknown<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".to_string(), |v| v.to_string())
}

pub fn check_scheduling(report: &SchedulingReport) -> Check {
    let detail = format!(
        "profile {}: nice {} (requested {}), io class {} (requested {}), duty {}%",
        report.profile.name(),
        or_unknown(report.applied_nice),
        or_unknown(report.requested_nice),
        or_unknown(report.applied_io_class),
        report.requested_io_class,
        report.duty_percent
    );
    if report.applied_as_requested() {
        Check::pass("scheduling", detail)
    } else {
        Check::warn("scheduling", detail)
    }
}

pub fn check_item_sample(sample: &ItemSample) -> Check {
    let mut detail = format!("{} items sampled from the first batch", sample.items);
    if !sample.unknown_types.is_empty() {
        let _ = write!(
            detail,
            "; other item types (skipped): {}",
            describe_type_counts(&sample.unknown_types)
        );
    }
    Check::pass("item_types", detail)
}

/// Subvolumes deleted but not yet cleaned: space freed later (A6).
pub fn check_pending_deletion(summary: &SubvolumeSummary) -> Option<Check> {
    crate::release::pending_cleaner(summary.pending_deletion)
        .map(|detail| Check::pass("pending_deletion", detail))
}

pub fn check_features(flags: &io::Result<FeatureFlags>) -> Check {
    const NAME: &str = "features";
    let flags = match flags {
        Ok(flags) => flags,
        Err(e) => return Check::warn(NAME, format!("BTRFS_IOC_GET_FEATURES failed: {e}")),
    };
    let mut detail = format!(
        "compat {:#x}, compat_ro {:#x}, incompat {:#x}",
        flags.compat, flags.compat_ro, flags.incompat
    );
    if flags.compat & COMPAT_SYNO_CASELESS != 0 {
        detail.push_str("; SYNO_CASELESS set (type 91 caseless name index expected)");
    }
    Check::pass(NAME, detail)
}

pub fn format_uuid(uuid: &[u8; 16]) -> String {
    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

pub fn check_filesystem(fs_type: &io::Result<u64>) -> Check {
    match fs_type {
        Ok(BTRFS_SUPER_MAGIC) => Check::pass("filesystem", "btrfs"),
        Ok(crate::ext4::SUPER_MAGIC) => Check::pass("filesystem", "ext4"),
        Ok(other) => Check::fail(
            "filesystem",
            format!("unsupported filesystem (statfs type {other:#x})"),
        ),
        Err(e) => Check::fail("filesystem", format!("statfs failed: {e}")),
    }
}

pub fn check_privilege(euid: u32) -> Check {
    if euid == 0 {
        Check::pass("privilege", "running as root")
    } else {
        Check::warn(
            "privilege",
            format!("euid {euid}; raw metadata access needs root"),
        )
    }
}

/// Whether the filesystem's writer lock is free: taken and released at
/// once, it creates only an empty lock file on tmpfs.
#[cfg(target_os = "linux")]
pub fn check_job_lock(lock: &Result<(), crate::state::StateError>) -> Check {
    match lock {
        Ok(()) => Check::pass("job_lock", "no scan is writing this filesystem"),
        Err(e @ crate::state::StateError::Busy { .. }) => Check::warn("job_lock", e.to_string()),
        Err(e) => Check::fail("job_lock", format!("writer lock unusable: {e}")),
    }
}

/// Whether DSM's system partition is nearly full: reported, never fixed.
pub fn check_system_partition(partition: Option<&crate::space::SystemPartition>) -> Check {
    const NAME: &str = "system_partition";
    let Some(partition) = partition else {
        return Check::skip(NAME, "cannot read the root filesystem's space");
    };
    match partition.notice() {
        None => Check::pass(
            NAME,
            format!("{} (DSM's own, not a data volume)", partition.summary()),
        ),
        Some(notice) => Check::warn(NAME, notice.message),
    }
}

/// Space on the state filesystem for the scans `doctor` checks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSpace {
    pub available: u64,
    pub min_free: u64,
    /// The system partition's size when this state or spill path is on it.
    pub system_total: Option<u64>,
    /// The largest spill recorded for scans of the target's filesystem.
    pub projected_spill: Option<u64>,
}

/// Whether a scan's spill fits above the free-space reserve (SPEC 22.4).
pub fn check_state_space(state_dir: &Path, space: &io::Result<StateSpace>) -> Check {
    check_state_space_with_alternative(state_dir, space, None)
}

fn check_state_space_with_alternative(
    state_dir: &Path,
    space: &io::Result<StateSpace>,
    alternative: Option<&Path>,
) -> Check {
    use crate::output::human_bytes;
    const NAME: &str = "state_space";
    let space = match space {
        Ok(space) => space,
        Err(e) => {
            return Check::warn(
                NAME,
                format!("{}: cannot read free space: {e}", state_dir.display()),
            );
        }
    };
    let reserve = crate::space::reserve(space.min_free, space.system_total);
    let why = if reserve > space.min_free {
        format!(
            "{}% of the system partition it is on, above --min-free",
            crate::space::SYSTEM_STATE_RESERVE_PERCENT
        )
    } else {
        "--min-free".to_string()
    };
    let mut detail = format!(
        "{}: {} available, reserve {} ({why})",
        state_dir.display(),
        human_bytes(space.available),
        human_bytes(reserve)
    );
    let headroom = space.available.checked_sub(reserve);
    let warn = match (headroom, space.projected_spill) {
        (None, _) => {
            detail.push_str("; already below the reserve: a scan would stop at once");
            true
        }
        (Some(headroom), Some(spill)) if headroom < spill => {
            let _ = write!(
                detail,
                "; a scan spilling like the largest recorded one ({}) would reach the reserve and pause, resumable",
                human_bytes(spill)
            );
            true
        }
        (Some(_), Some(spill)) => {
            let _ = write!(
                detail,
                "; largest recorded scan spill {}",
                human_bytes(spill)
            );
            false
        }
        (Some(_), None) => {
            detail.push_str("; no completed scan recorded to project its spill from");
            false
        }
    };
    if warn {
        if let Some(path) = alternative {
            let _ = write!(
                detail,
                "; use --state-dir {} on a data volume outside the scan selection",
                path.display()
            );
        } else {
            detail.push_str(
                "; no data volume with enough free space was confirmed; set --state-dir on a data volume outside the scan selection",
            );
        }
        Check::warn(NAME, detail)
    } else {
        Check::pass(NAME, detail)
    }
}

fn check_spill_space_with_alternative(
    spill_dir: &Path,
    space: &io::Result<StateSpace>,
    alternative: Option<&Path>,
) -> Check {
    let mut check = check_state_space_with_alternative(spill_dir, space, alternative);
    check.name = "spill_space";
    check
}

/// Whether a scan's state fits on `space`: its largest recorded spill
/// above the reserve (SPEC 22.4).
pub fn has_room(space: &StateSpace) -> bool {
    let reserve = crate::space::reserve(space.min_free, space.system_total);
    space.available >= reserve.saturating_add(space.projected_spill.unwrap_or(0))
}

#[cfg(target_os = "linux")]
fn state_locations_have_room(
    state_dir: &Path,
    history_dir: &Path,
    min_free: u64,
    fsid: &str,
) -> bool {
    let spill_dir = state_dir.join("spill");
    [state_dir, spill_dir.as_path()]
        .iter()
        .all(|dir| space_at(dir, history_dir, min_free, fsid).is_ok_and(|space| has_room(&space)))
}

#[cfg(target_os = "linux")]
fn is_data_volume_mount(path: &Path) -> bool {
    let Some(name) = path
        .parent()
        .filter(|parent| *parent == Path::new("/"))
        .and_then(|_| path.file_name())
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("volume"))
    else {
        return false;
    };
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(target_os = "linux")]
fn recommended_state_dir(
    target_fsid: &[u8; 16],
    history_dir: &Path,
    min_free: u64,
    fsid: &str,
) -> Option<PathBuf> {
    let environment = crate::resolve::Environment::load().ok()?;
    let mut data_volumes: Vec<PathBuf> = environment
        .volumes
        .iter()
        .filter_map(|volume| {
            let volume_fsid = volume.fsid.as_ref().ok()?;
            (volume_fsid != target_fsid && is_data_volume_mount(&volume.primary.mount_point))
                .then(|| volume.primary.mount_point.clone())
        })
        .collect();
    data_volumes.extend(
        crate::mounts::unsupported_volumes(&environment.mounts)
            .into_iter()
            .filter(|mount| mount.fstype == "ext4")
            .filter(|mount| is_data_volume_mount(&mount.mount_point))
            .map(|mount| mount.mount_point.clone()),
    );
    data_volumes.into_iter().find_map(|volume| {
        let candidate = volume.join("synoreclaim");
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if !metadata.is_dir() => return None,
            Err(error) if error.kind() != io::ErrorKind::NotFound => return None,
            _ => {}
        }
        state_locations_have_room(&candidate, history_dir, min_free, fsid).then_some(candidate)
    })
}

/// Whether the state directory's writes land on the target's filesystem,
/// recommending the default state directory only if it `default_has_room`.
pub fn check_state_dir(
    state_dir: &Path,
    placement: Placement,
    default_has_room: bool,
    alternative: Option<&Path>,
) -> Check {
    const NAME: &str = "state_dir";
    match placement.advice(default_has_room, alternative) {
        None => Check::pass(
            NAME,
            format!("{} is not on this filesystem", state_dir.display()),
        ),
        Some(notice) => Check::warn(NAME, format!("{}: {}", state_dir.display(), notice.message)),
    }
}

pub fn check_tree_search(probe: &Result<u32, SourceError>) -> Check {
    const NAME: &str = "tree_search_v2";
    match probe {
        Ok(_) => Check::pass(NAME, "BTRFS_IOC_TREE_SEARCH_V2 works"),
        Err(SourceError::Io(e)) => match e.raw_os_error() {
            Some(libc::EPERM) => Check::fail(NAME, "permission denied; needs root (CAP_SYS_ADMIN)"),
            Some(libc::ENOTTY) => Check::fail(NAME, "not supported by this kernel"),
            _ => Check::fail(NAME, format!("failed: {e}")),
        },
        Err(e) => Check::fail(NAME, format!("failed: {e}")),
    }
}

/// One TREE_SEARCH_V2 call for the top-level root item: whether raw
/// metadata access works. Returns the items found.
pub fn probe_tree_search<S: MetadataSource + ?Sized>(source: &mut S) -> Result<u32, SourceError> {
    use crate::btrfs::keys::{FS_TREE_OBJECTID, ROOT_ITEM_KEY, ROOT_TREE_OBJECTID};
    let range = SearchRange::new(
        ROOT_TREE_OBJECTID,
        Key::new(FS_TREE_OBJECTID, ROOT_ITEM_KEY, 0),
        Key::new(FS_TREE_OBJECTID, ROOT_ITEM_KEY, u64::MAX),
    );
    source.search(&range).map(|batch| batch.nr_items)
}

/// What the helper's health check learned from the Btrfs volumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeProbe {
    /// The NAS has no Btrfs volume, so there is nothing to search.
    NoBtrfsVolume,
    /// A volume answered the search; `volumes` counts the Btrfs volumes.
    Searched { volumes: usize, items: u32 },
    /// Btrfs volumes exist and none of them could be searched.
    Failed,
}

/// Searches the first Btrfs volume that opens and answers. A package folder
/// or any other path on another filesystem plays no part: only Btrfs volumes
/// are probed, and a NAS without one is not a failure.
pub fn probe_volumes<S: MetadataSource>(
    volumes: &[crate::mounts::BtrfsVolume],
    mut open: impl FnMut(&crate::mounts::Mount) -> io::Result<S>,
) -> VolumeProbe {
    if volumes.is_empty() {
        return VolumeProbe::NoBtrfsVolume;
    }
    for volume in volumes {
        let Ok(mut source) = open(&volume.primary) else {
            continue;
        };
        if let Ok(items) = probe_tree_search(&mut source) {
            return VolumeProbe::Searched {
                volumes: volumes.len(),
                items,
            };
        }
    }
    VolumeProbe::Failed
}

/// Counts item types in the first batch of `tree_id` only, so the check
/// stays cheap on a large volume.
pub fn sample_item_types<S: MetadataSource + ?Sized>(
    source: &mut S,
    tree_id: u64,
) -> Result<ItemSample, WalkError> {
    let range = SearchRange::new(tree_id, Key::MIN, Key::MAX);
    let batch = source.search(&range)?;
    let items = parse_batch(batch.buf, batch.nr_items, &range)?;
    let mut sample = ItemSample {
        items: items.len() as u64,
        ..ItemSample::default()
    };
    for item in items
        .iter()
        .filter(|i| !is_known_fs_tree_type(i.key.item_type))
    {
        *sample.unknown_types.entry(item.key.item_type).or_default() += 1;
    }
    Ok(sample)
}

/// The state filesystem's space for scans of the filesystem `fsid`,
/// read where the state directory is or would be created.
#[cfg(target_os = "linux")]
fn state_space(state_dir: &Path, min_free: u64, fsid: &str) -> io::Result<StateSpace> {
    space_at(state_dir, state_dir, min_free, fsid)
}

/// The space at `dir` for scans of `fsid`, projected from the spill
/// history kept in `state_dir`.
#[cfg(target_os = "linux")]
fn space_at(dir: &Path, state_dir: &Path, min_free: u64, fsid: &str) -> io::Result<StateSpace> {
    let base = dir
        .ancestors()
        .find(|p| p.is_dir())
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let records = crate::history::load(&crate::history::dir(state_dir), fsid);
    Ok(StateSpace {
        available: crate::space::available(base)?,
        min_free,
        system_total: crate::space::system_partition_total(dir),
        projected_spill: records.iter().map(|r| r.spill_peak_bytes).max(),
    })
}

/// Gathers live diagnostics for `target`. Read-only apart from lowering
/// this process's own scheduling priority.
#[cfg(target_os = "linux")]
pub fn run(
    target: PathBuf,
    profile: crate::resource::Profile,
    state_dir: &Path,
    min_free: u64,
) -> DoctorReport {
    use crate::btrfs::live::{LiveSource, OpenTarget};
    use crate::btrfs::subvolume::discover;
    use crate::resource::{Pacer, SystemClock, apply_profile};
    use crate::sys;
    use std::os::fd::AsFd;

    let mut report = DoctorReport::new(target);
    let opened = match OpenTarget::open(&report.target) {
        Ok(opened) => opened,
        Err(e) => {
            report
                .checks
                .push(Check::fail("target", format!("cannot open: {e}")));
            return report;
        }
    };
    let filesystem = check_filesystem(&sys::filesystem_type(opened.file.as_fd()));
    let not_btrfs = filesystem.status == Status::Fail;
    report.checks.push(filesystem);
    if not_btrfs {
        return report;
    }
    report.checks.push(check_privilege(sys::effective_uid()));
    report.kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string());
    if sys::filesystem_type(opened.file.as_fd()).ok() == Some(crate::ext4::SUPER_MAGIC) {
        let environment = crate::resolve::Environment::load();
        match environment.and_then(|env| {
            crate::resolve::resolve_path(&env, &report.target, None)
                .map_err(|error| io::Error::other(error.message))
        }) {
            Ok(target) => {
                report.filesystem_uuid = Some(crate::resolve::fsid_text(&target.fsid));
                report
                    .checks
                    .push(Check::pass("fs_info", "ext4 volume identity verified"));
            }
            Err(error) => report
                .checks
                .push(Check::fail("fs_info", error.to_string())),
        }
        report.checks.push(Check::skip(
            "tree_search",
            "ext4 uses an inode metadata walk",
        ));
        report.checks.push(Check::skip(
            "cleanup",
            "ext4_read_only: cleanup is not available on this volume",
        ));
        report
            .checks
            .push(check_scheduling(&apply_profile(profile)));
        return report;
    }
    let fs_info = sys::fs_info(opened.file.as_fd());
    match &fs_info {
        Ok(info) => {
            let uuid = format_uuid(&info.fsid);
            let detail = format!(
                "uuid {uuid}, {} device(s), nodesize {}",
                info.num_devices, info.nodesize
            );
            report.filesystem_uuid = Some(uuid);
            report.checks.push(Check::pass("fs_info", detail));
        }
        Err(e) => report.checks.push(Check::fail(
            "fs_info",
            format!("BTRFS_IOC_FS_INFO failed: {e}"),
        )),
    }
    report
        .checks
        .push(check_features(&sys::feature_flags(opened.file.as_fd())));
    let subvolume_id = match opened.subvolume_id() {
        Ok(id) => {
            report.checks.push(Check::pass(
                "subvolume_id",
                format!("target is in subvolume {id}"),
            ));
            Some(id)
        }
        Err(e) => {
            report.checks.push(Check::fail(
                "subvolume_id",
                format!("BTRFS_IOC_INO_LOOKUP failed: {e}"),
            ));
            None
        }
    };
    report.subvolume_id = subvolume_id;
    if let Ok(info) = &fs_info {
        let placement =
            crate::placement::classify(&crate::placement::locations(state_dir), &info.fsid, |id| {
                Some(id) == subvolume_id
            });
        let fsid = format_uuid(&info.fsid);
        let default_dir = Path::new(crate::cli::DEFAULT_STATE_DIR);
        let default_has_room = state_locations_have_room(default_dir, state_dir, min_free, &fsid);
        let alternative = recommended_state_dir(&info.fsid, state_dir, min_free, &fsid);
        report.checks.push(check_state_dir(
            state_dir,
            placement,
            default_has_room,
            alternative.as_deref(),
        ));
        let space = state_space(state_dir, min_free, &fsid);
        report.checks.push(check_state_space_with_alternative(
            state_dir,
            &space,
            alternative.as_deref(),
        ));
        let spill_dir = state_dir.join("spill");
        if !crate::space::same_filesystem(state_dir, &spill_dir) {
            let space = space_at(&spill_dir, state_dir, min_free, &fsid);
            report.checks.push(check_spill_space_with_alternative(
                &spill_dir,
                &space,
                alternative.as_deref(),
            ));
        }
    }
    let partition = crate::mounts::read_mounts()
        .ok()
        .and_then(|mounts| crate::space::system_partition(&mounts));
    report.system_partition = partition.clone();
    report
        .checks
        .push(check_system_partition(partition.as_ref()));
    let scheduling = apply_profile(profile);
    report.checks.push(check_scheduling(&scheduling));
    report.scheduling = Some(scheduling);

    let settings = profile.settings();
    let mut source = LiveSource::new(opened.file, settings.search_buffer);
    let probe = probe_tree_search(&mut source);
    let search = check_tree_search(&probe);
    let search_failed = search.status == Status::Fail;
    report.checks.push(search);
    if search_failed {
        return report;
    }
    let mut pacer = Pacer::new(settings.duty_percent, SystemClock::default(), None);
    match discover(&mut source, &mut pacer) {
        Ok(set) => {
            let summary = SubvolumeSummary::of(&set);
            let detail = format!(
                "{} subvolume(s), {} read-only, {} pending deletion",
                summary.count, summary.readonly, summary.pending_deletion
            );
            report.checks.push(Check::pass("subvolumes", detail));
            report.checks.extend(check_pending_deletion(&summary));
            let quota = if summary.quota_enabled {
                "enabled"
            } else {
                "disabled"
            };
            report.checks.push(Check::pass("qgroups", quota));
            report.subvolumes = Some(summary);
        }
        Err(e) => report
            .checks
            .push(Check::fail("subvolumes", format!("discovery failed: {e}"))),
    }
    if let Some(tree_id) = subvolume_id {
        match sample_item_types(&mut source, tree_id) {
            Ok(sample) => {
                report.checks.push(check_item_sample(&sample));
                report.item_sample = Some(sample);
            }
            Err(e) => report
                .checks
                .push(Check::fail("item_types", format!("sampling failed: {e}"))),
        }
    }
    report.checks.push(Check::skip(
        "cache",
        "doctor does not read report state; `synoreclaim volumes` lists reports",
    ));
    if let Some(uuid) = &report.filesystem_uuid {
        let lock = crate::state::lock_filesystem(
            std::path::Path::new(crate::state::LOCK_DIR),
            uuid,
            sys::effective_uid(),
            &crate::state::LockHolder::new(None, "doctor"),
        )
        .map(drop);
        report.checks.push(check_job_lock(&lock));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::keys::*;
    use crate::btrfs::source::RawBatch;
    use crate::btrfs::walk::testing::FakeSource;
    use crate::resource::Profile;

    fn os_error(code: i32) -> io::Error {
        io::Error::from_raw_os_error(code)
    }

    #[test]
    fn pending_deletions_get_their_own_check_with_bytes_unknown() {
        let mut summary = SubvolumeSummary::default();
        assert_eq!(check_pending_deletion(&summary), None);
        summary.pending_deletion = 1;
        let check = check_pending_deletion(&summary).unwrap();
        assert_eq!(
            (check.name, check.status),
            ("pending_deletion", Status::Pass)
        );
        assert!(check.detail.contains("freed later"), "{}", check.detail);
        assert_eq!(summary.pending_deletion_bytes, None);
    }

    #[test]
    fn formats_uuid_in_canonical_groups() {
        let uuid = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(format_uuid(&uuid), "12345678-9abc-def0-0123-456789abcdef");
    }

    #[test]
    fn filesystem_check_accepts_btrfs_and_ext4() {
        assert_eq!(check_filesystem(&Ok(0x9123_683E)).status, Status::Pass);
        let ext4 = check_filesystem(&Ok(0xEF53));
        assert_eq!(ext4.status, Status::Pass);
        assert_eq!(ext4.detail, "ext4");
        assert_eq!(
            check_filesystem(&Err(os_error(libc::ENOENT))).status,
            Status::Fail
        );
    }

    #[test]
    fn the_system_partition_check_warns_only_when_it_is_nearly_full() {
        use crate::space::SystemPartition;
        let partition = |available| {
            SystemPartition::try_new("/dev/md0".into(), "ext4".into(), 8 << 30, available).unwrap()
        };
        let roomy = check_system_partition(Some(&partition(4 << 30)));
        assert_eq!(roomy.status, Status::Pass);
        assert_eq!(
            roomy.detail,
            "/ (/dev/md0, ext4): 4 GB of 8 GB available (DSM's own, not a data volume)"
        );
        let just_above_threshold = check_system_partition(Some(&partition(410 << 20)));
        assert_eq!(just_above_threshold.status, Status::Pass);
        let full = check_system_partition(Some(&partition(400 << 20)));
        assert_eq!(full.status, Status::Warn);
        assert_eq!(full.detail, partition(400 << 20).notice().unwrap().message);
        assert_eq!(check_system_partition(None).status, Status::Skip);
    }

    fn state_space(available: u64, projected_spill: Option<u64>) -> StateSpace {
        StateSpace {
            available,
            min_free: 512 << 20,
            system_total: None,
            projected_spill,
        }
    }

    #[test]
    fn the_state_space_check_projects_the_largest_recorded_spill_above_the_reserve() {
        let dir = Path::new("/s");
        let roomy = check_state_space(dir, &Ok(state_space(4 << 30, Some(700 << 20))));
        assert_eq!(roomy.status, Status::Pass);
        assert_eq!(
            roomy.detail,
            "/s: 4 GB available, reserve 512 MB (--min-free); largest recorded scan spill 700 MB"
        );
        let unknown = check_state_space(dir, &Ok(state_space(4 << 30, None)));
        assert_eq!(unknown.status, Status::Pass);
        assert!(
            unknown
                .detail
                .ends_with("; no completed scan recorded to project its spill from"),
            "{}",
            unknown.detail
        );
        let tight = check_state_space(dir, &Ok(state_space(1 << 30, Some(700 << 20))));
        assert_eq!(tight.status, Status::Warn);
        assert!(
            tight.detail.contains(
                "; a scan spilling like the largest recorded one (700 MB) would reach the reserve and pause, resumable"
            ),
            "{}",
            tight.detail
        );
        let below = check_state_space(dir, &Ok(state_space(100 << 20, None)));
        assert_eq!(below.status, Status::Warn);
        assert!(
            below
                .detail
                .contains("; already below the reserve: a scan would stop at once"),
            "{}",
            below.detail
        );
        assert!(
            below
                .detail
                .ends_with(
                    "; no data volume with enough free space was confirmed; set --state-dir on a data volume outside the scan selection"
                ),
            "{}",
            below.detail
        );
        assert!(
            !below
                .detail
                .contains("use the default /var/lib/synoreclaim")
        );
        assert_eq!(
            check_state_space(dir, &Err(os_error(libc::EACCES))).status,
            Status::Warn
        );
    }

    #[test]
    fn on_the_system_partition_the_state_space_reserve_is_three_percent_of_it() {
        let space = StateSpace {
            min_free: 128 << 20,
            system_total: Some(8 << 30),
            ..state_space(4 << 30, None)
        };
        let check = check_state_space(Path::new("/var/lib/synoreclaim"), &Ok(space));
        assert!(
            check.detail.starts_with(
                "/var/lib/synoreclaim: 4 GB available, reserve 245.8 MB (3% of the system partition it is on, above --min-free)"
            ),
            "{}",
            check.detail
        );
    }

    #[test]
    fn a_low_state_partition_names_a_safe_data_volume_alternative() {
        let check = check_state_space_with_alternative(
            Path::new("/var/lib/synoreclaim"),
            &Ok(StateSpace {
                available: 1 << 30,
                min_free: 512 << 20,
                system_total: Some(8 << 30),
                projected_spill: Some(700 << 20),
            }),
            Some(Path::new("/volume2/synoreclaim")),
        );
        assert_eq!(check.status, Status::Warn);
        assert!(
            check.detail.ends_with(
                "; use --state-dir /volume2/synoreclaim on a data volume outside the scan selection"
            ),
            "{}",
            check.detail
        );
    }

    #[test]
    fn privilege_check_warns_when_not_root() {
        assert_eq!(check_privilege(0).status, Status::Pass);
        assert_eq!(check_privilege(1000).status, Status::Warn);
    }

    #[test]
    fn search_probe_explains_permission_and_support_failures() {
        assert_eq!(check_tree_search(&Ok(3)).status, Status::Pass);
        let eperm = check_tree_search(&Err(SourceError::Io(os_error(libc::EPERM))));
        assert_eq!(eperm.status, Status::Fail);
        assert!(eperm.detail.contains("CAP_SYS_ADMIN"));
        let enotty = check_tree_search(&Err(SourceError::Io(os_error(libc::ENOTTY))));
        assert!(enotty.detail.contains("not supported"));
    }

    #[test]
    fn samples_item_types_from_one_batch() {
        let items = vec![
            (Key::new(256, INODE_ITEM_KEY, 0), vec![0; 4]),
            (Key::new(256, 163, 0), vec![1]),
            (Key::new(257, INODE_ITEM_KEY, 0), vec![0; 4]),
            (Key::new(300, INODE_ITEM_KEY, 0), vec![0; 4]),
        ];
        let mut source = FakeSource::new(items, 3);
        let sample = sample_item_types(&mut source, 257).unwrap();
        assert_eq!(sample.items, 3);
        assert_eq!(sample.unknown_types, BTreeMap::from([(163, 1)]));
        assert_eq!(source.calls.len(), 1);
    }

    struct CountingSource(u32);

    impl MetadataSource for CountingSource {
        fn search(&mut self, _: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            Ok(RawBatch {
                nr_items: self.0,
                buf: &[],
            })
        }
    }

    struct BrokenSource;

    impl MetadataSource for BrokenSource {
        fn search(&mut self, _: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            Err(SourceError::Io(io::Error::from_raw_os_error(25)))
        }
    }

    fn volume(point: &str) -> crate::mounts::BtrfsVolume {
        crate::mounts::btrfs_volumes(&crate::mounts::parse_mountinfo(
            format!("45 17 0:36 / {point} rw - btrfs /dev/mapper/{point:?} rw\n").as_bytes(),
        ))
        .remove(0)
    }

    #[test]
    fn without_a_btrfs_volume_there_is_nothing_to_probe_and_nothing_failed() {
        let outcome = probe_volumes::<CountingSource>(&[], |_| panic!("nothing to open"));
        assert_eq!(outcome, VolumeProbe::NoBtrfsVolume);
    }

    #[test]
    fn a_btrfs_volume_that_answers_the_search_is_enough() {
        let volumes = [volume("/volume1"), volume("/volume2")];
        let outcome = probe_volumes(&volumes, |_| Ok(CountingSource(4)));
        assert_eq!(
            outcome,
            VolumeProbe::Searched {
                volumes: 2,
                items: 4
            }
        );
    }

    #[test]
    fn a_volume_that_does_not_open_is_skipped_for_the_next() {
        let volumes = [volume("/volume1"), volume("/volume2")];
        let outcome = probe_volumes(&volumes, |mount| {
            if mount.mount_point == std::path::Path::new("/volume1") {
                Err(io::Error::from_raw_os_error(2))
            } else {
                Ok(CountingSource(1))
            }
        });
        assert_eq!(
            outcome,
            VolumeProbe::Searched {
                volumes: 2,
                items: 1
            }
        );
    }

    #[test]
    fn btrfs_volumes_that_cannot_be_searched_fail_the_probe() {
        let volumes = [volume("/volume1")];
        assert_eq!(
            probe_volumes(&volumes, |_| Ok(BrokenSource)),
            VolumeProbe::Failed
        );
        assert_eq!(
            probe_volumes::<BrokenSource>(&volumes, |_| Err(io::Error::from_raw_os_error(13))),
            VolumeProbe::Failed
        );
    }

    struct GarbageSource;

    impl MetadataSource for GarbageSource {
        fn search(&mut self, _: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            Ok(RawBatch {
                nr_items: 2,
                buf: &[0; 8],
            })
        }
    }

    #[test]
    fn sample_rejects_malformed_batches() {
        assert!(matches!(
            sample_item_types(&mut GarbageSource, 5),
            Err(WalkError::Batch(_))
        ));
    }

    #[test]
    fn item_sample_check_names_unknown_types_without_failing() {
        let sample = ItemSample {
            items: 9,
            unknown_types: BTreeMap::from([(163, 2)]),
        };
        let check = check_item_sample(&sample);
        assert_eq!(check.status, Status::Pass);
        assert!(check.detail.contains("type 163 x2"));
    }

    #[test]
    fn item_sample_check_names_the_caseless_index_type() {
        let sample = ItemSample {
            items: 9,
            unknown_types: BTreeMap::from([(91, 4), (163, 1)]),
        };
        let check = check_item_sample(&sample);
        assert!(
            check
                .detail
                .contains("BTRFS_DIR_ITEM_CASELESS_KEY (91) x4, type 163 x1"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn feature_check_reports_synology_caseless_names() {
        let flags = FeatureFlags {
            compat: 0xe000_0000_0000_0000,
            compat_ro: 0x3,
            incompat: 0x16b,
        };
        let check = check_features(&Ok(flags));
        assert_eq!(check.status, Status::Pass);
        assert_eq!(
            check.detail,
            "compat 0xe000000000000000, compat_ro 0x3, incompat 0x16b; SYNO_CASELESS set (type 91 caseless name index expected)"
        );
        let plain = check_features(&Ok(FeatureFlags {
            compat: 0,
            compat_ro: 0,
            incompat: 0x161,
        }));
        assert_eq!(plain.detail, "compat 0x0, compat_ro 0x0, incompat 0x161");
        let failed = check_features(&Err(os_error(libc::ENOTTY)));
        assert_eq!(failed.status, Status::Warn);
    }

    fn scheduling(applied_nice: Option<i32>, applied_io: Option<&'static str>) -> SchedulingReport {
        SchedulingReport {
            profile: Profile::Low,
            requested_nice: Some(19),
            applied_nice,
            requested_io_class: "idle",
            applied_io_class: applied_io,
            duty_percent: 100,
            warnings: Vec::new(),
            effectiveness_unknown: true,
        }
    }

    #[test]
    fn scheduling_check_reads_as_plain_values() {
        let check = check_scheduling(&scheduling(Some(19), Some("idle")));
        assert_eq!(check.status, Status::Pass);
        assert_eq!(
            check.detail,
            "profile low: nice 19 (requested 19), io class idle (requested idle), duty 100%"
        );
    }

    #[test]
    fn scheduling_check_warns_and_says_unknown_when_not_applied() {
        let check = check_scheduling(&scheduling(None, None));
        assert_eq!(check.status, Status::Warn);
        assert_eq!(
            check.detail,
            "profile low: nice unknown (requested 19), io class unknown (requested idle), duty 100%"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn job_lock_reports_a_running_writer() {
        use crate::state::StateError;
        let free = check_job_lock(&Ok(()));
        assert_eq!(free.status, Status::Pass);
        let busy = check_job_lock(&Err(StateError::Busy {
            fsid: "f".into(),
            holder: None,
        }));
        assert_eq!(busy.status, Status::Warn);
        assert!(
            busy.detail.contains("another synoreclaim"),
            "{}",
            busy.detail
        );
        let broken = check_job_lock(&Err(StateError::Insecure("/run/x".into())));
        assert_eq!(broken.status, Status::Fail);
    }

    #[test]
    fn state_dir_check_passes_when_the_state_is_elsewhere() {
        let check = check_state_dir(
            Path::new("/var/lib/synoreclaim"),
            Placement::Elsewhere,
            true,
            None,
        );
        assert_eq!(check.status, Status::Pass);
        assert_eq!(
            check.detail,
            "/var/lib/synoreclaim is not on this filesystem"
        );
    }

    #[test]
    fn state_dir_check_warns_with_the_fix_when_on_this_filesystem() {
        let path = Path::new("/volume1/share/state");
        let placement = Placement::InSelection { subvolume_id: 256 };
        let check = check_state_dir(path, placement, true, None);
        assert_eq!(check.status, Status::Warn);
        let expected = format!(
            "{}: {}",
            path.display(),
            placement.advice(true, None).unwrap().message
        );
        assert_eq!(check.detail, expected);
        let beside = check_state_dir(
            path,
            Placement::SameFilesystem { subvolume_id: 257 },
            true,
            None,
        );
        assert_eq!(beside.status, Status::Warn);
    }

    #[test]
    fn state_dir_check_does_not_recommend_a_default_without_room() {
        let path = Path::new("/volume1/share/state");
        let placement = Placement::InSelection { subvolume_id: 256 };
        let alternative = Path::new("/volume2/synoreclaim");
        let check = check_state_dir(path, placement, false, Some(alternative));
        assert_eq!(check.status, Status::Warn);
        assert_eq!(
            check.detail,
            format!(
                "{}: {}",
                path.display(),
                placement.advice(false, Some(alternative)).unwrap().message
            )
        );
    }

    #[test]
    fn a_state_filesystem_has_room_for_the_reserve_and_the_largest_recorded_spill() {
        assert!(has_room(&state_space(4 << 30, Some(700 << 20))));
        assert!(!has_room(&state_space(1 << 30, Some(700 << 20))));
        assert!(has_room(&state_space(1 << 30, None)));
        // A test NAS: 228 MiB free on / is below the 512 MiB reserve alone.
        assert!(!has_room(&state_space(228 << 20, None)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn data_volume_recommendations_only_use_volume_mount_roots() {
        assert!(is_data_volume_mount(Path::new("/volume2")));
        assert!(!is_data_volume_mount(Path::new("/volume2/share")));
        assert!(!is_data_volume_mount(Path::new("/volumeUSB1")));
    }

    #[test]
    fn exit_code_is_one_only_when_a_check_fails() {
        let mut report = DoctorReport::new("/volume1".into());
        report.checks.push(Check::pass("filesystem", "btrfs"));
        report.checks.push(Check::warn("privilege", "not root"));
        assert_eq!(report.exit_code(), 0);
        report.checks.push(Check::fail("tree_search_v2", "EPERM"));
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn human_output_lists_each_check() {
        let mut report = DoctorReport::new("/volume1".into());
        report.checks.push(Check::pass("filesystem", "btrfs"));
        report
            .checks
            .push(Check::skip("cache", "doctor does not read report state"));
        let text = report.render_human();
        assert!(text.contains("target: /volume1"));
        assert!(text.contains("PASS  filesystem"));
        assert!(text.contains("SKIP  cache"));
    }

    #[test]
    fn json_output_carries_schema_version_and_checks() {
        let mut report = DoctorReport::new("/volume1".into());
        report.checks.push(Check::fail("filesystem", "not btrfs"));
        let json: serde_json::Value = serde_json::from_str(&report.render_json()).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["checks"][0]["name"], "filesystem");
        assert_eq!(json["checks"][0]["status"], "fail");
    }
}
