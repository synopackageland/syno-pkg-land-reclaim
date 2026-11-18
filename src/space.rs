//! Free space on the state filesystem during a scan (SPEC 22.4). On DSM
//! the default state directory lies on the system partition (`/`, md0),
//! which DSM itself needs, so a scan there keeps a larger reserve.

use crate::btrfs::walk::StopReason;
use crate::mounts::Mount;
use crate::output::{Notice, human_bytes};
use serde::Serialize;
use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Share of the system partition that `volumes` and `doctor` report as
/// nearly full.
pub const SYSTEM_RESERVE_PERCENT: u64 = 5;
/// Share of the system partition a scan keeps available for its state and
/// spill writes, because DSM also needs this filesystem.
pub const SYSTEM_STATE_RESERVE_PERCENT: u64 = 3;

/// `SYSTEM_RESERVE_PERCENT` of a partition of `total` bytes.
pub fn system_reserve(total: u64) -> u64 {
    (u128::from(total) * u128::from(SYSTEM_RESERVE_PERCENT) / 100) as u64
}

/// `SYSTEM_STATE_RESERVE_PERCENT` of a partition of `total` bytes.
pub fn system_state_reserve(total: u64) -> u64 {
    (u128::from(total) * u128::from(SYSTEM_STATE_RESERVE_PERCENT) / 100) as u64
}

/// DSM's recommended maximum use of the system partition.
pub const SYSTEM_PARTITION_RECOMMENDED_USED_PERCENT: u64 = 70;
/// The patch size in the md0-full SOP's DSM 7.2.1-69057 Update 1 example.
pub const SYSTEM_PARTITION_EXAMPLE_PATCH_BYTES: u64 = 389_000_000;
/// Extra room for a standard update, from Synology CSS KI#77166.
pub const SYSTEM_PARTITION_UPDATE_MARGIN_BYTES: u64 = 130_000_000;
/// T2: the SOP's example patch plus the extra room in KI#77166.
pub const SYSTEM_PARTITION_TYPICAL_UPDATE_ROOM_BYTES: u64 =
    SYSTEM_PARTITION_EXAMPLE_PATCH_BYTES + SYSTEM_PARTITION_UPDATE_MARGIN_BYTES;
/// T3: worst-case room from KI#77166 and KI#94475, rounded up to 1 GiB by
/// the CLI because the waiting patch size is not known.
pub const SYSTEM_PARTITION_UPDATE_HEADROOM_BYTES: u64 = 1 << 30;
/// Above this much available space the system partition is Ok whatever
/// T1–T4 say: 900 MB as DSM counts it (900 × 1024² bytes).
pub const SYSTEM_PARTITION_NO_WARNING_ABOVE_BYTES: u64 = 900 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPartitionLevel {
    Ok,
    GettingFull,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPartitionLevelReason {
    UsedOverRecommended,
    UpdateHeadroomLow,
    UpdateMayFail,
    NearlyFull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPartitionClassification {
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub used_percent: u8,
    pub level: SystemPartitionLevel,
    pub level_reasons: Vec<SystemPartitionLevelReason>,
}

/// Classify readable system partition space using the shared T1–T4 rules.
/// `None` means the filesystem space was unreadable
/// or inconsistent. The `rootfs` filesystem status can use this same function.
pub fn classify_system_partition(
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
) -> Option<SystemPartitionClassification> {
    let (total, free) = (total_bytes?, free_bytes?);
    if total == 0 || free > total {
        return None;
    }

    let used = total - free;
    let used_percent = (u128::from(used) * 100 / u128::from(total)) as u8;
    let used_over_recommended = u128::from(used) * 100
        >= u128::from(total) * u128::from(SYSTEM_PARTITION_RECOMMENDED_USED_PERCENT);
    let update_headroom_low = free < SYSTEM_PARTITION_UPDATE_HEADROOM_BYTES;
    let update_may_fail = free < SYSTEM_PARTITION_TYPICAL_UPDATE_ROOM_BYTES;
    let nearly_full = free < system_reserve(total);

    let mut level_reasons = Vec::new();
    if used_over_recommended {
        level_reasons.push(SystemPartitionLevelReason::UsedOverRecommended);
    }
    if update_headroom_low {
        level_reasons.push(SystemPartitionLevelReason::UpdateHeadroomLow);
    }
    if update_may_fail {
        level_reasons.push(SystemPartitionLevelReason::UpdateMayFail);
    }
    if nearly_full {
        level_reasons.push(SystemPartitionLevelReason::NearlyFull);
    }

    let full_below = SYSTEM_PARTITION_TYPICAL_UPDATE_ROOM_BYTES.max(system_reserve(total));
    let level = if free > SYSTEM_PARTITION_NO_WARNING_ABOVE_BYTES {
        SystemPartitionLevel::Ok
    } else if free < full_below {
        SystemPartitionLevel::Full
    } else if used_over_recommended || update_headroom_low {
        SystemPartitionLevel::GettingFull
    } else {
        SystemPartitionLevel::Ok
    };

    Some(SystemPartitionClassification {
        used_bytes: used,
        free_bytes: free,
        used_percent,
        level,
        level_reasons,
    })
}

/// The space a scan leaves available on the state filesystem:
/// `--min-free`, raised to the state reserve when the state or spill
/// directory is on the system partition of `system_total` bytes.
pub fn reserve(min_free: u64, system_total: Option<u64>) -> u64 {
    system_total.map_or(min_free, |total| min_free.max(system_state_reserve(total)))
}

/// The system partition (`/`): on DSM md0, ext4, holding DSM itself.
/// Not a data volume; synoreclaim neither scans nor cleans it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemPartition {
    pub mount: String,
    pub source: String,
    pub fstype: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub used_percent: u8,
    pub level: SystemPartitionLevel,
    pub level_reasons: Vec<SystemPartitionLevelReason>,
    /// Available space is below `SYSTEM_RESERVE_PERCENT` of its size.
    pub nearly_full: bool,
}

impl SystemPartition {
    pub fn try_new(
        source: String,
        fstype: String,
        total_bytes: u64,
        available_bytes: u64,
    ) -> Option<Self> {
        let classification = classify_system_partition(Some(total_bytes), Some(available_bytes))?;
        Some(SystemPartition {
            mount: "/".into(),
            source,
            fstype,
            total_bytes,
            available_bytes,
            used_bytes: classification.used_bytes,
            free_bytes: classification.free_bytes,
            used_percent: classification.used_percent,
            level: classification.level,
            nearly_full: classification
                .level_reasons
                .contains(&SystemPartitionLevelReason::NearlyFull),
            level_reasons: classification.level_reasons,
        })
    }

    /// Its space, such as `/ (/dev/md0, ext4): 4.5 GB of 7.8 GB available`.
    pub fn summary(&self) -> String {
        format!(
            "{} ({}, {}): {} of {} available",
            self.mount,
            self.source,
            self.fstype,
            human_bytes(self.available_bytes),
            human_bytes(self.total_bytes)
        )
    }

    /// The warning when it is nearly full.
    pub fn notice(&self) -> Option<Notice> {
        self.nearly_full.then(|| {
            Notice::new(
                "system_partition_nearly_full",
                format!(
                    "the system partition {}, below {SYSTEM_RESERVE_PERCENT}%: this is DSM's own partition, not a data volume; \
                     `synoreclaim rootfs` shows what fills it, and \
                     `synoreclaim rootfs plan --to <folder in a shared folder>` plans moving DSM's leftovers off it",
                    self.summary()
                ),
            )
        })
    }
}

/// The mount of `/`: the last one listed, since later mounts cover
/// earlier ones.
pub fn root_mount(mounts: &[Mount]) -> Option<&Mount> {
    mounts.iter().rfind(|m| m.mount_point == Path::new("/"))
}

/// The system partition as mounted now.
#[cfg(target_os = "linux")]
pub fn system_partition(mounts: &[Mount]) -> Option<SystemPartition> {
    use std::os::fd::AsFd;
    let mount = root_mount(mounts)?;
    let root = std::fs::File::open("/").ok()?;
    let space = crate::sys::filesystem_space(root.as_fd()).ok()?;
    SystemPartition::try_new(
        mount.source.display().to_string(),
        mount.fstype.clone(),
        space.total,
        space.available,
    )
}

/// Watches the state and spill filesystems at most once per interval,
/// plus the largest size the spill directory reached.
type AvailableSpace = Box<dyn Fn(&Path) -> io::Result<u64>>;

pub struct DiskWatch {
    spill_dir: PathBuf,
    state_dir: PathBuf,
    state_reserve: u64,
    spill_reserve: u64,
    available: AvailableSpace,
    every: Duration,
    last: Option<Instant>,
    pub spill_peak_bytes: u64,
}

impl DiskWatch {
    pub fn new(
        spill_dir: PathBuf,
        state_dir: PathBuf,
        state_reserve: u64,
        spill_reserve: u64,
        every: Duration,
        available: AvailableSpace,
    ) -> Self {
        DiskWatch {
            spill_dir,
            state_dir,
            state_reserve,
            spill_reserve,
            available,
            every,
            last: None,
            spill_peak_bytes: 0,
        }
    }

    /// Stops with `DiskLimit` below the reserve. A failed free-space
    /// reading does not stop the scan: the cache checks again on write.
    pub fn check(&mut self, now: Instant) -> ControlFlow<StopReason> {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < self.every)
        {
            return ControlFlow::Continue(());
        }
        self.last = Some(now);
        self.sample_spill();
        let state_below_reserve =
            (self.available)(&self.state_dir).is_ok_and(|available| available < self.state_reserve);
        let spill_below_reserve =
            (self.available)(&self.spill_dir).is_ok_and(|available| available < self.spill_reserve);
        if state_below_reserve || spill_below_reserve {
            ControlFlow::Break(StopReason::DiskLimit)
        } else {
            ControlFlow::Continue(())
        }
    }

    /// Updates the spill peak with the spill directory's current size.
    pub fn sample_spill(&mut self) {
        let spilled = crate::spill::dir_bytes(&self.spill_dir);
        self.spill_peak_bytes = self.spill_peak_bytes.max(spilled);
    }
}

/// The size of the system partition when `state_dir`, or where it
/// would be created, lies on it: on the device of `/`.
#[cfg(target_os = "linux")]
pub fn system_partition_total(state_dir: &Path) -> Option<u64> {
    use std::os::fd::AsFd;
    let base = state_dir.ancestors().find(|p| p.is_dir())?;
    if !same_filesystem(base, Path::new("/")) {
        return None;
    }
    let root = std::fs::File::open("/").ok()?;
    crate::sys::filesystem_space(root.as_fd())
        .ok()
        .map(|s| s.total)
}

/// Whether existing paths, or their nearest existing directory ancestors,
/// resolve to the same filesystem device.
#[cfg(target_os = "linux")]
pub fn same_filesystem(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let device = |path: &Path| {
        let base = path.ancestors().find(|p| p.is_dir())?;
        std::fs::metadata(base).ok().map(|metadata| metadata.dev())
    };
    device(left).zip(device(right)).is_some_and(|(a, b)| a == b)
}

/// Bytes available to unprivileged users on the filesystem of `dir`.
#[cfg(target_os = "linux")]
pub fn available(dir: &Path) -> io::Result<u64> {
    use std::os::fd::AsFd;
    Ok(crate::sys::filesystem_space(std::fs::File::open(dir)?.as_fd())?.available)
}

/// The spill directory of a scan, for `DiskWatch`.
pub fn spill_dir(root: &Path, job_id: &str, fsid: &str) -> PathBuf {
    root.join(format!("{job_id}-{fsid}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::testing::scratch;
    use std::cell::Cell;
    use std::rc::Rc;

    #[cfg(target_os = "linux")]
    #[test]
    fn a_state_dir_is_on_the_system_partition_when_it_shares_the_device_of_root() {
        let total = system_partition_total(Path::new("/not-created-yet/state"));
        assert!(total.is_some_and(|t| t > 0));
        assert_eq!(system_partition_total(Path::new("/proc/self")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nearest_existing_ancestors_resolve_state_and_spill_filesystems() {
        assert!(same_filesystem(
            Path::new("/not-created/state"),
            Path::new("/")
        ));
        assert!(!same_filesystem(Path::new("/proc/self"), Path::new("/")));
    }

    fn partition(available: u64) -> SystemPartition {
        SystemPartition::try_new("/dev/md0".into(), "ext4".into(), 8 << 30, available).unwrap()
    }

    #[test]
    fn system_partition_level_matches_the_two_documented_layouts() {
        const EXT4_LAYOUT_BYTES: u64 = 8_500_000_000;
        const SMALL_LAYOUT_BYTES: u64 = 2_500_000_000;
        const UPDATE_ROOM_BYTES: u64 = 519_000_000;
        const ONE_GIB: u64 = 1 << 30;

        let level = |total, free| {
            classify_system_partition(Some(total), Some(free))
                .unwrap()
                .level
        };

        // At exactly 70% used the larger layout names T1, but more than
        // 900 MiB available keeps it Ok.
        let at_recommended_limit =
            classify_system_partition(Some(EXT4_LAYOUT_BYTES), Some(2_550_000_000)).unwrap();
        assert_eq!(at_recommended_limit.used_percent, 70);
        assert_eq!(at_recommended_limit.level, SystemPartitionLevel::Ok);
        assert!(
            at_recommended_limit
                .level_reasons
                .contains(&SystemPartitionLevelReason::UsedOverRecommended)
        );
        assert_eq!(
            level(EXT4_LAYOUT_BYTES, 2_550_000_001),
            SystemPartitionLevel::Ok
        );

        // T2 is patch + 130 MB; the state becomes Full only below that floor.
        assert_eq!(
            level(EXT4_LAYOUT_BYTES, UPDATE_ROOM_BYTES - 1),
            SystemPartitionLevel::Full
        );
        let update_may_fail =
            classify_system_partition(Some(EXT4_LAYOUT_BYTES), Some(UPDATE_ROOM_BYTES - 1))
                .unwrap();
        assert!(
            update_may_fail
                .level_reasons
                .contains(&SystemPartitionLevelReason::UpdateMayFail)
        );
        assert_eq!(
            level(EXT4_LAYOUT_BYTES, UPDATE_ROOM_BYTES),
            SystemPartitionLevel::GettingFull
        );

        // T3's 1 GiB reason is strict on the 2.5 GB layout; above 900 MiB
        // available it is not a warning.
        let headroom_low =
            classify_system_partition(Some(SMALL_LAYOUT_BYTES), Some(ONE_GIB - 1)).unwrap();
        assert!(
            headroom_low
                .level_reasons
                .contains(&SystemPartitionLevelReason::UpdateHeadroomLow)
        );
        assert_eq!(
            level(SMALL_LAYOUT_BYTES, ONE_GIB - 1),
            SystemPartitionLevel::Ok
        );
        assert_eq!(level(SMALL_LAYOUT_BYTES, ONE_GIB), SystemPartitionLevel::Ok);
        assert_eq!(
            level(SMALL_LAYOUT_BYTES, UPDATE_ROOM_BYTES - 1),
            SystemPartitionLevel::Full
        );
        assert_eq!(
            level(SMALL_LAYOUT_BYTES, UPDATE_ROOM_BYTES),
            SystemPartitionLevel::GettingFull
        );

        // T4 adds a reason when 5% of the larger layout is not available.
        let nearly_full =
            classify_system_partition(Some(EXT4_LAYOUT_BYTES), Some(424_999_999)).unwrap();
        assert_eq!(nearly_full.level, SystemPartitionLevel::Full);
        assert!(
            nearly_full
                .level_reasons
                .contains(&SystemPartitionLevelReason::NearlyFull)
        );
    }

    #[test]
    fn system_partition_above_900_mib_available_is_never_warned_about() {
        const SMALL_LAYOUT_BYTES: u64 = 2_500_000_000;
        const LARGE_LAYOUT_BYTES: u64 = 20 << 30;
        const LIMIT: u64 = 900 * 1024 * 1024;
        assert_eq!(SYSTEM_PARTITION_NO_WARNING_ABOVE_BYTES, LIMIT);

        let level = |total, free| {
            classify_system_partition(Some(total), Some(free))
                .unwrap()
                .level
        };

        // Below 1 GiB and over 70% used, yet more than 900 MiB available.
        assert_eq!(
            level(SMALL_LAYOUT_BYTES, LIMIT + 1),
            SystemPartitionLevel::Ok
        );
        assert_eq!(
            level(SMALL_LAYOUT_BYTES, LIMIT),
            SystemPartitionLevel::GettingFull
        );
        assert_eq!(
            level(SMALL_LAYOUT_BYTES, LIMIT - 1),
            SystemPartitionLevel::GettingFull
        );

        // Below 5% of a large partition, yet more than 900 MiB available.
        assert_eq!(
            level(LARGE_LAYOUT_BYTES, LIMIT + 1),
            SystemPartitionLevel::Ok
        );
        assert_eq!(level(LARGE_LAYOUT_BYTES, LIMIT), SystemPartitionLevel::Full);
    }

    #[test]
    fn system_partition_level_is_absent_when_space_cannot_be_read() {
        assert_eq!(classify_system_partition(None, None), None);
        assert_eq!(classify_system_partition(Some(8_500_000_000), None), None);
        assert_eq!(classify_system_partition(None, Some(0)), None);
        assert_eq!(classify_system_partition(Some(0), Some(0)), None);
        assert_eq!(classify_system_partition(Some(1), Some(2)), None);
    }

    #[test]
    fn system_partition_construction_rejects_a_zero_total() {
        assert_eq!(
            SystemPartition::try_new("/dev/md0".into(), "ext4".into(), 0, 0),
            None
        );
    }

    #[test]
    fn system_partition_construction_rejects_available_space_above_total() {
        assert_eq!(
            SystemPartition::try_new("/dev/md0".into(), "ext4".into(), 1, 2),
            None
        );
    }

    #[test]
    fn readable_system_partition_serializes_the_classification() {
        let partition = SystemPartition::try_new(
            "/dev/md0".into(),
            "ext4".into(),
            8_500_000_000,
            2_550_000_000,
        )
        .unwrap();
        let value = serde_json::to_value(partition).unwrap();
        assert_eq!(value["used_bytes"], 5_950_000_000u64);
        assert_eq!(value["free_bytes"], 2_550_000_000u64);
        assert_eq!(value["available_bytes"], 2_550_000_000u64);
        assert_eq!(value["used_percent"], 70);
        assert_eq!(value["level"], "ok");
        assert_eq!(value["level_reasons"][0], "used_over_recommended");
    }

    #[test]
    fn the_system_partition_is_nearly_full_below_five_percent_available() {
        let roomy = partition(4 << 30);
        assert!(!roomy.nearly_full);
        assert_eq!(roomy.notice(), None);
        assert_eq!(
            roomy.summary(),
            "/ (/dev/md0, ext4): 4 GB of 8 GB available"
        );
        let just_above = partition(410 << 20);
        assert!(!just_above.nearly_full);
        assert_eq!(just_above.notice(), None);
        let full = partition(400 << 20);
        assert!(full.nearly_full);
        let notice = full.notice().unwrap();
        assert_eq!(notice.code, "system_partition_nearly_full");
        assert!(
            notice.message.starts_with(
                "the system partition / (/dev/md0, ext4): 400 MB of 8 GB available, below 5%: "
            ),
            "{}",
            notice.message
        );
        assert!(notice.message.contains("not a data volume"));
        assert!(
            notice.message.ends_with(
                "`synoreclaim rootfs` shows what fills it, and \
                 `synoreclaim rootfs plan --to <folder in a shared folder>` plans moving DSM's leftovers off it"
            ),
            "{}",
            notice.message
        );
    }

    #[test]
    fn the_root_mount_is_the_last_one_on_slash() {
        let mount = |point: &str, source: &str| Mount {
            device: "0:1".into(),
            root: b"/".to_vec(),
            mount_point: point.into(),
            fstype: "ext4".into(),
            source: source.into(),
            options: Vec::new(),
        };
        let mounts = [
            mount("/", "rootfs"),
            mount("/volume1", "/dev/x"),
            mount("/", "/dev/md0"),
        ];
        assert_eq!(root_mount(&mounts).unwrap().source, Path::new("/dev/md0"));
        assert_eq!(root_mount(&mounts[1..2]), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_live_system_partition_is_the_root_filesystem() {
        let mounts = crate::mounts::read_mounts().unwrap();
        let root = system_partition(&mounts).unwrap();
        assert_eq!(root.mount, "/");
        assert!(root.total_bytes >= root.available_bytes && root.total_bytes > 0);
    }

    #[test]
    fn the_system_partition_keeps_three_percent_unless_min_free_is_larger() {
        assert_eq!(
            system_reserve(8 << 30),
            ((8u64 << 30) as u128 * 5 / 100) as u64
        );
        assert_eq!(reserve(512 << 20, None), 512 << 20);
        assert_eq!(
            reserve(128 << 20, Some(8 << 30)),
            ((8u64 << 30) as u128 * 3 / 100) as u64
        );
        assert_eq!(reserve(2 << 30, Some(8 << 30)), 2 << 30);
    }

    fn watch(dir: &Path, free: Rc<Cell<u64>>) -> DiskWatch {
        DiskWatch::new(
            dir.to_path_buf(),
            dir.to_path_buf(),
            1000,
            1000,
            Duration::from_secs(1),
            Box::new(move |_| Ok(free.get())),
        )
    }

    fn watch_with_free_space(
        state_dir: &Path,
        spill_dir: &Path,
        state_free: Rc<Cell<u64>>,
        spill_free: Rc<Cell<u64>>,
    ) -> DiskWatch {
        let state_path = state_dir.to_path_buf();
        DiskWatch::new(
            spill_dir.to_path_buf(),
            state_dir.to_path_buf(),
            1000,
            2000,
            Duration::from_secs(1),
            Box::new(move |path| {
                Ok(if path == state_path {
                    state_free.get()
                } else {
                    spill_free.get()
                })
            }),
        )
    }

    #[test]
    fn below_the_reserve_the_scan_stops_and_the_spill_peak_is_kept() {
        let dir = scratch("space");
        let free = Rc::new(Cell::new(5000));
        let mut w = watch(&dir, free.clone());
        let t0 = Instant::now();
        std::fs::write(dir.join("refs"), [0; 300]).unwrap();
        assert_eq!(w.check(t0), ControlFlow::Continue(()));
        assert_eq!(w.spill_peak_bytes, 300);
        std::fs::remove_file(dir.join("refs")).unwrap();
        free.set(999);
        assert_eq!(
            w.check(t0 + Duration::from_secs(2)),
            ControlFlow::Break(StopReason::DiskLimit)
        );
        assert_eq!(w.spill_peak_bytes, 300);
    }

    #[test]
    fn the_spill_filesystem_keeps_its_own_reserve() {
        let base = scratch("space-spill-reserve");
        let state_dir = base.join("state");
        let spill_dir = base.join("spill");
        std::fs::create_dir(&state_dir).unwrap();
        std::fs::create_dir(&spill_dir).unwrap();
        let mut w = watch_with_free_space(
            &state_dir,
            &spill_dir,
            Rc::new(Cell::new(5000)),
            Rc::new(Cell::new(1999)),
        );
        assert_eq!(
            w.check(Instant::now()),
            ControlFlow::Break(StopReason::DiskLimit)
        );
    }

    #[test]
    fn the_state_filesystem_keeps_its_own_reserve() {
        let base = scratch("space-state-reserve");
        let state_dir = base.join("state");
        let spill_dir = base.join("spill");
        std::fs::create_dir(&state_dir).unwrap();
        std::fs::create_dir(&spill_dir).unwrap();
        let mut w = watch_with_free_space(
            &state_dir,
            &spill_dir,
            Rc::new(Cell::new(999)),
            Rc::new(Cell::new(5000)),
        );
        assert_eq!(
            w.check(Instant::now()),
            ControlFlow::Break(StopReason::DiskLimit)
        );
    }

    #[test]
    fn checks_are_throttled_and_unreadable_space_does_not_stop() {
        let dir = scratch("space");
        let free = Rc::new(Cell::new(5000));
        let mut w = watch(&dir, free.clone());
        let t0 = Instant::now();
        assert_eq!(w.check(t0), ControlFlow::Continue(()));
        free.set(0);
        assert_eq!(
            w.check(t0 + Duration::from_millis(500)),
            ControlFlow::Continue(())
        );
        let mut failing = DiskWatch::new(
            dir.clone(),
            dir.clone(),
            1000,
            1000,
            Duration::ZERO,
            Box::new(|_| Err(io::Error::other("statvfs"))),
        );
        assert_eq!(failing.check(t0), ControlFlow::Continue(()));
    }
}
