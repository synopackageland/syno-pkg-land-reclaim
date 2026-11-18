//! Where the tool's own state directory lies relative to a scan (SPEC
//! 22.4): its writes advance the generation of the subvolume holding it.
//! A scan ignores such a change when every inode it changed lies inside
//! the state directory.

use crate::cli::DEFAULT_STATE_DIR;
use crate::output::Notice;
use crate::report::{ChangeReason, StateDirWrites};
use crate::target::{Resolved, Scope};
use std::path::{Path, PathBuf};

/// A state location: filesystem id, subvolume and inode.
pub type Identity = ([u8; 16], u64, u64);

/// Ordered from harmless to worst, so the worst of several locations is
/// their maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Placement {
    /// On another filesystem, or unknown.
    Elsewhere,
    /// On the scanned filesystem, in a subvolume outside the selection.
    SameFilesystem { subvolume_id: u64 },
    /// Inside a selected subvolume.
    InSelection { subvolume_id: u64 },
}

/// The worst placement of `locations` for a scan of `fsid` whose
/// selection contains the subvolumes `selected` accepts.
pub fn classify(
    locations: &[Identity],
    fsid: &[u8; 16],
    selected: impl Fn(u64) -> bool,
) -> Placement {
    locations
        .iter()
        .filter(|(id, _, _)| id == fsid)
        .map(|&(_, subvolume_id, _)| {
            if selected(subvolume_id) {
                Placement::InSelection { subvolume_id }
            } else {
                Placement::SameFilesystem { subvolume_id }
            }
        })
        .max()
        .unwrap_or(Placement::Elsewhere)
}

/// Subdirectories a state directory writes to.
const SUBDIRS: [&str; 6] = ["cache", "spill", "reports", "jobs", "cleanup", "history"];

/// Where writes to `state_dir` land: the directory, or its nearest
/// existing ancestor when it is not created yet, then its existing
/// subdirectories (each could be a mount or a subvolume of its own).
pub fn location_paths(state_dir: &Path) -> Vec<PathBuf> {
    let Some(base) = state_dir.ancestors().find(|p| p.is_dir()) else {
        return Vec::new();
    };
    let mut paths = vec![base.to_path_buf()];
    if base == state_dir {
        paths.extend(
            SUBDIRS
                .iter()
                .map(|name| base.join(name))
                .filter(|p| p.is_dir()),
        );
    }
    paths
}

/// The identities of `state_dir`'s locations on Btrfs.
#[cfg(target_os = "linux")]
pub fn locations(state_dir: &Path) -> Vec<Identity> {
    location_paths(state_dir)
        .iter()
        .filter_map(|p| crate::resolve::identity(p).ok())
        .collect()
}

/// A hint when the subvolume holding the state directory still counts
/// as changed: not every inode it changed could be traced to the state
/// directory (SPEC 22.4).
pub fn change_hint(
    locations: &[Identity],
    fsid: &[u8; 16],
    reasons: &[ChangeReason],
) -> Option<Notice> {
    let holds_state = |id: u64| locations.iter().any(|&(f, s, _)| &f == fsid && s == id);
    reasons.iter().find_map(|reason| match *reason {
        ChangeReason::SubvolumeGenerationChanged { subvolume_id, .. } if holds_state(subvolume_id) => {
            Some(Notice::new(
                "state_dir_changed",
                format!(
                    "subvolume {subvolume_id} holds the state directory, but not every inode it changed during the scan lies inside it: the change stays"
                ),
            ))
        }
        _ => None,
    })
}

/// A notice for each subvolume whose change the scan ignored because
/// only the state directory's own writes made it.
pub fn writes_ignored(writes: &[StateDirWrites]) -> Vec<Notice> {
    writes
        .iter()
        .map(|w| {
            Notice::new(
                "state_dir_writes_ignored",
                format!(
                    "subvolume {} changed during the scan only through {} inodes inside the state directory: that change was ignored",
                    w.subvolume_id, w.inodes
                ),
            )
        })
        .collect()
}

/// Whether scanning `targets` selects subvolume `subvolume_id`: a volume
/// target selects them all, a path target its own.
pub fn selects(targets: &[Resolved], subvolume_id: u64) -> bool {
    targets
        .iter()
        .any(|t| t.scope == Scope::Volume || t.tree == subvolume_id)
}

impl Placement {
    /// The warning a scan gives about its state directory, if any: the
    /// scan ignores changes only the state directory's writes made, so
    /// it names no fix.
    pub fn notice(self) -> Option<Notice> {
        const IGNORED: &str = "a change made only by its own writes is ignored; any other change there still makes the scan changed-during-scan";
        match self {
            Placement::Elsewhere => None,
            Placement::SameFilesystem { subvolume_id } => Some(Notice::new(
                "state_dir_on_scanned_filesystem",
                format!(
                    "the state directory is on the scanned filesystem (subvolume {subvolume_id}): {IGNORED}"
                ),
            )),
            Placement::InSelection { subvolume_id } => Some(Notice::new(
                "state_dir_in_selection",
                format!(
                    "the state directory is inside selected subvolume {subvolume_id}: {IGNORED}"
                ),
            )),
        }
    }

    /// The warning, recommending the default state directory only when
    /// it has room for a scan's state.
    pub fn advice(self, default_has_room: bool, alternative: Option<&Path>) -> Option<Notice> {
        let fix = match (default_has_room, alternative) {
            (true, _) => format!(
                "use the default {DEFAULT_STATE_DIR} or a --state-dir on another filesystem"
            ),
            (false, Some(path)) => format!(
                "the default {DEFAULT_STATE_DIR} lacks room for a scan's state: use --state-dir {} on a data volume outside the scan selection",
                path.display()
            ),
            (false, None) => format!(
                "the default {DEFAULT_STATE_DIR} lacks room for a scan's state: no data volume with enough free space was confirmed; set --state-dir on a data volume outside the scan selection"
            ),
        };
        match self {
            Placement::Elsewhere => None,
            Placement::SameFilesystem { subvolume_id } => Some(Notice::new(
                "state_dir_on_scanned_filesystem",
                format!(
                    "the state directory is on the scanned filesystem (subvolume {subvolume_id}): its own writes can make the scan changed-during-scan; {fix}"
                ),
            )),
            Placement::InSelection { subvolume_id } => Some(Notice::new(
                "state_dir_in_selection",
                format!(
                    "the state directory is inside selected subvolume {subvolume_id}: its own writes make the scan changed-during-scan, which cleanup refuses; {fix}"
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::testing::scratch;
    use crate::target::Scope;

    const FS: [u8; 16] = [1; 16];
    const OTHER: [u8; 16] = [2; 16];

    #[test]
    fn state_on_another_filesystem_is_elsewhere() {
        let placement = classify(&[(OTHER, 256, 300)], &FS, |_| true);
        assert_eq!(placement, Placement::Elsewhere);
        assert_eq!(classify(&[], &FS, |_| true), Placement::Elsewhere);
    }

    #[test]
    fn state_in_an_unselected_subvolume_is_on_the_same_filesystem() {
        let placement = classify(&[(FS, 257, 300)], &FS, |id| id == 256);
        assert_eq!(placement, Placement::SameFilesystem { subvolume_id: 257 });
    }

    #[test]
    fn state_in_a_selected_subvolume_is_in_the_selection() {
        let placement = classify(&[(FS, 256, 300)], &FS, |id| id == 256);
        assert_eq!(placement, Placement::InSelection { subvolume_id: 256 });
    }

    #[test]
    fn the_worst_location_wins() {
        let locations = [(OTHER, 256, 1), (FS, 256, 2), (FS, 257, 3)];
        let placement = classify(&locations, &FS, |id| id == 256);
        assert_eq!(placement, Placement::InSelection { subvolume_id: 256 });
    }

    #[test]
    fn a_missing_state_dir_lies_where_it_would_be_created() {
        let base = scratch("placement-missing");
        let paths = location_paths(&base.join("state/deeper"));
        assert_eq!(paths, [base]);
    }

    #[test]
    fn a_state_dir_lies_where_it_and_its_subdirectories_are() {
        let state = scratch("placement-subdirs");
        std::fs::create_dir(state.join("cache")).unwrap();
        std::fs::create_dir(state.join("reports")).unwrap();
        let paths = location_paths(&state);
        assert_eq!(
            paths,
            [state.clone(), state.join("cache"), state.join("reports")]
        );
    }

    fn target(scope: Scope, tree: u64) -> Resolved {
        Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: "/v/t".into(),
            path: "/v/t".into(),
            scope,
            fsid: FS,
            mount: "/v".into(),
            tree,
            ino: 256,
        }
    }

    #[test]
    fn path_targets_select_their_own_subvolumes() {
        let targets = [target(Scope::Path, 256), target(Scope::Path, 258)];
        assert!(selects(&targets, 256));
        assert!(selects(&targets, 258));
        assert!(!selects(&targets, 257));
    }

    #[test]
    fn a_volume_target_selects_every_subvolume() {
        assert!(selects(&[target(Scope::Volume, 5)], 257));
    }

    #[test]
    fn state_elsewhere_gives_no_notice() {
        assert_eq!(Placement::Elsewhere.notice(), None);
    }

    #[test]
    fn a_scan_says_changes_made_only_by_the_state_directory_are_ignored() {
        let notice = Placement::InSelection { subvolume_id: 256 }
            .notice()
            .unwrap();
        assert_eq!(notice.code, "state_dir_in_selection");
        assert_eq!(
            notice.message,
            "the state directory is inside selected subvolume 256: a change made only by its own writes is ignored; any other change there still makes the scan changed-during-scan"
        );
        let notice = Placement::SameFilesystem { subvolume_id: 257 }
            .notice()
            .unwrap();
        assert_eq!(notice.code, "state_dir_on_scanned_filesystem");
        assert_eq!(
            notice.message,
            "the state directory is on the scanned filesystem (subvolume 257): a change made only by its own writes is ignored; any other change there still makes the scan changed-during-scan"
        );
    }

    #[test]
    fn state_in_the_selection_names_the_subvolume_and_the_fix() {
        let notice = Placement::InSelection { subvolume_id: 256 }
            .advice(true, None)
            .unwrap();
        assert_eq!(notice.code, "state_dir_in_selection");
        assert_eq!(
            notice.message,
            "the state directory is inside selected subvolume 256: its own writes make the scan changed-during-scan, which cleanup refuses; use the default /var/lib/synoreclaim or a --state-dir on another filesystem"
        );
    }

    #[test]
    fn state_on_the_scanned_filesystem_names_the_subvolume_and_the_fix() {
        let notice = Placement::SameFilesystem { subvolume_id: 257 }
            .advice(true, None)
            .unwrap();
        assert_eq!(notice.code, "state_dir_on_scanned_filesystem");
        assert_eq!(
            notice.message,
            "the state directory is on the scanned filesystem (subvolume 257): its own writes can make the scan changed-during-scan; use the default /var/lib/synoreclaim or a --state-dir on another filesystem"
        );
    }

    #[test]
    fn without_room_at_the_default_the_fix_is_a_data_volume_outside_the_selection() {
        for placement in [
            Placement::InSelection { subvolume_id: 256 },
            Placement::SameFilesystem { subvolume_id: 257 },
        ] {
            let message = placement.advice(false, None).unwrap().message;
            assert!(
                message.ends_with(
                    "; the default /var/lib/synoreclaim lacks room for a scan's state: no data volume with enough free space was confirmed; set --state-dir on a data volume outside the scan selection"
                ),
                "{message}"
            );
        }
        assert_eq!(Placement::Elsewhere.advice(false, None), None);
    }

    #[test]
    fn an_unavailable_default_recommends_the_detected_data_volume_path() {
        let alternative = Path::new("/volume2/synoreclaim");
        let message = Placement::InSelection { subvolume_id: 256 }
            .advice(false, Some(alternative))
            .unwrap()
            .message;
        assert!(message.contains("lacks room"), "{message}");
        assert!(
            message.ends_with(
                "; the default /var/lib/synoreclaim lacks room for a scan's state: use --state-dir /volume2/synoreclaim on a data volume outside the scan selection"
            ),
            "{message}"
        );
        assert!(!message.contains("use the default /var/lib/synoreclaim"));
    }

    fn generation_changed(subvolume_id: u64) -> ChangeReason {
        ChangeReason::SubvolumeGenerationChanged {
            subvolume_id,
            selected: true,
            generation_before: 20,
            generation_after: 23,
        }
    }

    #[test]
    fn a_kept_change_of_the_state_subvolume_says_why_it_stays() {
        let reasons = [generation_changed(257), generation_changed(256)];
        let hint = change_hint(&[(FS, 256, 300)], &FS, &reasons).unwrap();
        assert_eq!(hint.code, "state_dir_changed");
        assert_eq!(
            hint.message,
            "subvolume 256 holds the state directory, but not every inode it changed during the scan lies inside it: the change stays"
        );
    }

    #[test]
    fn each_ignored_change_gets_a_notice_with_its_inode_count() {
        let writes = [StateDirWrites {
            subvolume_id: 256,
            generation_before: 20,
            generation_after: 23,
            inodes: 12,
        }];
        let notices = writes_ignored(&writes);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].code, "state_dir_writes_ignored");
        assert_eq!(
            notices[0].message,
            "subvolume 256 changed during the scan only through 12 inodes inside the state directory: that change was ignored"
        );
        assert_eq!(writes_ignored(&[]), []);
    }

    #[test]
    fn other_changes_give_no_hint() {
        let reasons = [generation_changed(257)];
        assert_eq!(change_hint(&[(FS, 256, 300)], &FS, &reasons), None);
        let elsewhere = [(OTHER, 257, 300)];
        assert_eq!(change_hint(&elsewhere, &FS, &reasons), None);
    }
}
