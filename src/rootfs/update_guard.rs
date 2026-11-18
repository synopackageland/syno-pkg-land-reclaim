//! DSM's own markers of an update that is staged or running. While any
//! is present, `rootfs plan` and `rootfs apply` refuse: `/upd@te`,
//! `/@autoupdate` and `/.SynoUpgrade*` are then the update itself, not
//! leftovers.
//!
//! The markers are libsynoinstall's (DSM7-3-new): a downloaded patch is
//! staged while `UpdateUtil::CheckDownloadedPatchType` (cpp/lib/
//! update_cpputil.cpp) finds one of its info files, which
//! `_LaunchSystemUpdate` (cpp/lib/launch.cpp) removes as the install
//! starts; an update or its download runs while DSM's pid files exist
//! (cpp/include/update_common.h, update_cpputil.h; `/var/run` is cleared
//! at boot).

use crate::btrfs::walk::{BatchBoundary, StopReason};
use crate::guard::Refusal;
use std::io;
use std::ops::ControlFlow;
use std::path::Path;

/// Info files of a downloaded, not yet installed patch.
pub const STAGED: [&str; 4] = [
    "autoupdate.dsm.info",
    "manualupdate.dsm.info",
    "autoupdate.small.info",
    "manualupdate.small.info",
];

/// Pid files of a running update, its preparation or its download.
pub const RUNNING: [&str; 3] = [
    "var/run/dsmautoupdate.pid",
    "var/run/dsmautoupdate_prepare.pid",
    "var/run/downloadDSM.pid",
];

/// The first marker present below `root`, as an absolute path on it.
pub fn active_marker(root: &Path) -> Option<&'static str> {
    STAGED
        .iter()
        .chain(&RUNNING)
        .copied()
        .find(|marker| std::fs::symlink_metadata(root.join(marker)).is_ok())
}

/// Refuses while a DSM update is staged or running on `root`; `what`
/// says what did not happen.
pub fn check(root: &Path, what: &str) -> Result<(), Refusal> {
    match active_marker(root) {
        None => Ok(()),
        Some(marker) => Err(Refusal::new(
            "dsm_update_active",
            format!(
                "a DSM update is staged or running (/{marker} exists), so /upd@te, /@autoupdate \
                 and the upgrade files are that update, not leftovers; finish or cancel it in \
                 Control Panel > Update & Restore, then plan again; {what}"
            ),
        )),
    }
}

/// Wraps a run's boundary: once a DSM update is staged or running on
/// `root`, the run stops at the next boundary and `marker` names why.
pub struct UpdateWatch<'a> {
    root: &'a Path,
    inner: &'a mut dyn BatchBoundary,
    pub marker: Option<&'static str>,
}

impl<'a> UpdateWatch<'a> {
    pub fn new(root: &'a Path, inner: &'a mut dyn BatchBoundary) -> Self {
        UpdateWatch {
            root,
            inner,
            marker: None,
        }
    }
}

impl BatchBoundary for UpdateWatch<'_> {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        let flow = self.inner.after_batch();
        if let Some(marker) = active_marker(self.root) {
            self.marker = Some(marker);
            return ControlFlow::Break(StopReason::Paused);
        }
        flow
    }
}

/// When the system last booted (Unix seconds): `btime` in `/proc/stat`.
/// An update installed since then waits for the restart in `/upd@te` and
/// the upgrade files, with its markers already gone.
pub fn boot_time(proc_root: &Path) -> io::Result<i64> {
    let stat = std::fs::read_to_string(proc_root.join("stat"))?;
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|seconds| seconds.trim().parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no btime in /proc/stat"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_staged_or_running_update_marker_refuses() {
        for marker in STAGED.iter().chain(&RUNNING) {
            let root = crate::spill::testing::scratch("rootfs-update-guard");
            std::fs::create_dir_all(root.join("var/run")).unwrap();
            assert_eq!(active_marker(&root), None);
            assert!(check(&root, "no plan was written").is_ok());
            std::fs::write(root.join(marker), b"1").unwrap();
            assert_eq!(active_marker(&root), Some(*marker), "{marker}");
            let refusal = check(&root, "no plan was written").unwrap_err();
            assert_eq!(refusal.code, "dsm_update_active");
            assert!(
                refusal.message.contains(&format!("/{marker}")),
                "{}",
                refusal.message
            );
            assert!(
                refusal.message.ends_with("no plan was written"),
                "{}",
                refusal.message
            );
        }
    }

    #[test]
    fn the_boot_time_is_btime_in_proc_stat() {
        let proc_root = crate::spill::testing::scratch("rootfs-btime");
        std::fs::write(
            proc_root.join("stat"),
            b"cpu  1 2 3\nintr 5\nbtime 1789999000\nprocesses 9\n",
        )
        .unwrap();
        assert_eq!(boot_time(&proc_root).unwrap(), 1_789_999_000);
        std::fs::write(proc_root.join("stat"), b"cpu  1 2 3\n").unwrap();
        assert_eq!(
            boot_time(&proc_root).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    /// Plants `marker` below `root` at its first boundary.
    struct Plant<'a> {
        root: &'a Path,
        marker: &'a str,
    }

    impl BatchBoundary for Plant<'_> {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            std::fs::write(self.root.join(self.marker), b"1").unwrap();
            ControlFlow::Continue(())
        }
    }

    #[test]
    fn a_marker_that_appears_mid_run_stops_it_at_the_next_boundary() {
        let root = crate::spill::testing::scratch("rootfs-update-watch");
        std::fs::create_dir_all(root.join("var/run")).unwrap();
        let mut plant = Plant {
            root: &root,
            marker: RUNNING[0],
        };
        let mut watch = UpdateWatch::new(&root, &mut plant);
        assert_eq!(watch.marker, None);

        assert_eq!(watch.after_batch(), ControlFlow::Break(StopReason::Paused));
        assert_eq!(watch.marker, Some(RUNNING[0]));
    }

    #[test]
    fn without_a_marker_the_watch_defers_to_what_it_wraps() {
        let root = crate::spill::testing::scratch("rootfs-update-watch-quiet");
        struct Cancel;
        impl BatchBoundary for Cancel {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                ControlFlow::Break(StopReason::Cancelled)
            }
        }
        let mut cancel = Cancel;
        let mut watch = UpdateWatch::new(&root, &mut cancel);
        assert_eq!(
            watch.after_batch(),
            ControlFlow::Break(StopReason::Cancelled)
        );
        assert_eq!(watch.marker, None);
    }

    #[test]
    fn a_marker_that_is_a_symlink_or_folder_still_counts() {
        let root = crate::spill::testing::scratch("rootfs-update-guard-kinds");
        std::fs::create_dir(root.join(STAGED[0])).unwrap();
        assert_eq!(active_marker(&root), Some(STAGED[0]));
        let root = crate::spill::testing::scratch("rootfs-update-guard-link");
        std::os::unix::fs::symlink("/nonexistent", root.join(STAGED[1])).unwrap();
        assert_eq!(active_marker(&root), Some(STAGED[1]));
    }
}
