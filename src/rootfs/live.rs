//! Reads the report's inputs from a live root: statfs, the `/var/log`
//! project quota, a paced walk, and one `/proc` scan for deleted files
//! still open on the root's filesystem.

use super::classify::Category;
use super::report::{self, Header, Inputs, LogQuota, RootfsDocument};
use super::walk::{self, Limits, Walk};
use crate::btrfs::walk::BatchBoundary;
use crate::output::escape_human;
use crate::sys;
use crate::why_not_freed::ProcessScan;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

/// Where the report reads from; production passes `/`, `/proc` and the
/// mount table's other mount points.
pub struct Sources<'a> {
    pub root: &'a Path,
    /// Mount points below the root, as paths relative to it.
    pub mount_points: BTreeSet<Vec<u8>>,
    /// The block device and filesystem type the root is mounted from.
    pub source: String,
    pub fstype: String,
    pub proc_root: &'a Path,
}

/// The report of `sources.root`, its walk paced by `boundary`.
pub fn report(
    sources: &Sources<'_>,
    limits: Limits,
    boundary: &mut dyn BatchBoundary,
    now: SystemTime,
) -> io::Result<RootfsDocument> {
    // Deep's table is Standard's and more: one walk totals both modes.
    let surveyed = survey_mode(sources, limits, boundary, super::plan::CleanupMode::Deep)?;
    Ok(report::build(surveyed.inputs(), now))
}

/// What the report and `rootfs plan` read from a live root.
pub struct Survey {
    /// `st_dev` of the root.
    pub device: u64,
    pub header: Header,
    pub log_quota: Option<LogQuota>,
    pub walk: Walk,
    pub elapsed: Duration,
    pub open_deleted: ProcessScan,
    /// The last boot; the far future when `/proc/stat` cannot tell, so no
    /// update leftover counts as changed since.
    pub boot: i64,
    /// The name Package Center shows, by package id, for the walked packages.
    pub package_names: BTreeMap<Vec<u8>, String>,
}

impl Survey {
    pub fn inputs(&self) -> Inputs<'_> {
        Inputs {
            header: self.header.clone(),
            log_quota: self.log_quota,
            walk: &self.walk,
            elapsed: self.elapsed,
            open_deleted: self.open_deleted.clone(),
            tools: crate::rootfs::delegate::available(&self.open_deleted),
            deep_tools: crate::rootfs::delegate::available_mode(
                &self.open_deleted,
                super::plan::CleanupMode::Deep,
            ),
            boot: self.boot,
            package_names: &self.package_names,
        }
    }
}

/// Reads `sources.root`, its walk paced by `boundary`.
pub fn survey(
    sources: &Sources<'_>,
    limits: Limits,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<Survey> {
    survey_mode(
        sources,
        limits,
        boundary,
        super::plan::CleanupMode::Standard,
    )
}

pub fn survey_mode(
    sources: &Sources<'_>,
    limits: Limits,
    boundary: &mut dyn BatchBoundary,
    mode: super::plan::CleanupMode,
) -> io::Result<Survey> {
    let root = File::open(sources.root)?;
    let device = root.metadata()?.dev();
    let space = sys::filesystem_space(root.as_fd())?;
    let inodes = sys::filesystem_inodes(root.as_fd())?;
    let header = Header {
        source: sources.source.clone(),
        fstype: sources.fstype.clone(),
        total_bytes: space.total,
        free_bytes: space.free,
        available_bytes: space.available,
        inodes_total: inodes.total,
        inodes_free: inodes.free,
    };
    let started = Instant::now();
    let walked = walk::walk_mode(
        sources.root,
        &sources.mount_points,
        limits,
        boundary,
        &sys::inode_flags,
        mode,
    )?;
    let elapsed = started.elapsed();
    let (found, unreadable) = crate::open_files::deleted_files(sources.proc_root, |file| {
        file.metadata().is_ok_and(|meta| meta.dev() == device)
    });
    let package_names = package_names(sources.root, &walked);
    Ok(Survey {
        device,
        header,
        log_quota: log_quota(sources),
        package_names,
        walk: walked,
        elapsed,
        open_deleted: crate::why_not_freed::process_scan(&found, unreadable),
        boot: super::update_guard::boot_time(sources.proc_root).unwrap_or(i64::MAX),
    })
}

/// The largest `INFO` read; DSM's run to a few KiB.
const INFO_MAX_BYTES: u64 = 64 << 10;

/// The `displayname` in `/var/packages/<id>/INFO` of each package that
/// owns bytes, the name Package Center shows.
fn package_names(root: &Path, walked: &Walk) -> BTreeMap<Vec<u8>, String> {
    walked
        .owners
        .keys()
        .filter(|owner| owner.category == Category::PackageData)
        .filter_map(|owner| {
            let id = owner.name.as_deref()?;
            Some((id.to_vec(), display_name(root, id)?))
        })
        .collect()
}

fn display_name(root: &Path, id: &[u8]) -> Option<String> {
    if id.is_empty() || id.contains(&b'/') || id == b"." || id == b".." {
        return None;
    }
    let path = root
        .join("var/packages")
        .join(OsStr::from_bytes(id))
        .join("INFO");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut info = Vec::new();
    file.take(INFO_MAX_BYTES).read_to_end(&mut info).ok()?;
    info.split(|&b| b == b'\n').find_map(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let name = line.strip_prefix(b"displayname=\"")?.strip_suffix(b"\"")?;
        let name = escape_human(name.trim_ascii());
        (!name.is_empty()).then_some(name)
    })
}

/// DSM's `/var/log` quota, when `/var/log` has a project with a limit.
fn log_quota(sources: &Sources<'_>) -> Option<LogQuota> {
    let log = File::open(sources.root.join("var/log")).ok()?;
    let project_id = sys::project_id(log.as_fd()).ok().filter(|&id| id != 0)?;
    let quota = sys::project_quota(Path::new(&sources.source), project_id).ok()?;
    (quota.hard_limit > 0 || quota.soft_limit > 0).then_some(LogQuota {
        project_id,
        used_bytes: quota.used,
        soft_limit_bytes: quota.soft_limit,
        hard_limit_bytes: quota.hard_limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::walk::StopReason;
    use std::ops::ControlFlow;

    struct Unpaced;

    impl BatchBoundary for Unpaced {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }
    }

    #[test]
    fn a_fake_root_reports_its_space_usage_and_movable_leftovers() {
        let root = crate::spill::testing::scratch("rootfs-live");
        std::fs::create_dir_all(root.join("upd@te")).unwrap();
        std::fs::create_dir_all(root.join("var/log")).unwrap();
        std::fs::write(root.join("upd@te/hda1.tgz"), vec![1u8; 8192]).unwrap();
        let sources = Sources {
            root: &root,
            mount_points: BTreeSet::new(),
            source: "/dev/md0".into(),
            fstype: "ext4".into(),
            proc_root: &root.join("no-proc"),
        };

        let doc = report(&sources, Limits::default(), &mut Unpaced, SystemTime::now()).unwrap();

        assert!(doc.filesystem.total_bytes > 0);
        assert_eq!(doc.filesystem.source, "/dev/md0");
        assert!(doc.log_quota.is_none());
        assert_eq!(doc.walk.entries, 4);
        assert_eq!(doc.usage[0].entry.path.path, "/upd@te");
        assert_eq!(doc.cleanable.totals.items, 1);
        assert_eq!(doc.open_deleted.unreadable_processes, 1);
    }

    #[test]
    fn package_rows_carry_the_name_package_center_shows() {
        let root = crate::spill::testing::scratch("rootfs-live-names");
        std::fs::create_dir_all(root.join("var/packages/ActiveInsight")).unwrap();
        std::fs::create_dir_all(root.join("var/packages/NoInfo")).unwrap();
        std::fs::create_dir_all(root.join("usr/local/packages/@apptemp/ActiveInsight")).unwrap();
        std::fs::write(
            root.join("var/packages/ActiveInsight/INFO"),
            "package=\"ActiveInsight\"\ndisplayname_cht=\"x\"\ndisplayname=\"Active Insight\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("usr/local/packages/@apptemp/ActiveInsight/cache"),
            vec![1u8; 2 << 20],
        )
        .unwrap();
        // Rows under 1 MiB fold into one; these two stay rows of their own.
        std::fs::write(root.join("var/packages/NoInfo/data"), vec![1u8; 2 << 20]).unwrap();
        let sources = Sources {
            root: &root,
            mount_points: BTreeSet::new(),
            source: "/dev/md0".into(),
            fstype: "ext4".into(),
            proc_root: &root.join("no-proc"),
        };

        let doc = report(&sources, Limits::default(), &mut Unpaced, SystemTime::now()).unwrap();
        let rows = serde_json::to_value(&doc).unwrap()["owners"].clone();
        let row = |id: &str| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|row| row["category"] == "package_data" && row["name"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("{id} in {rows}"))
        };

        assert_eq!(row("ActiveInsight")["display_name"], "Active Insight");
        assert!(row("NoInfo")["display_name"].is_null());
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .all(|row| row["category"] != "package_data" || row["name"].is_string()),
            "{rows}"
        );
    }
}
