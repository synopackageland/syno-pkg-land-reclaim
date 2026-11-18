//! What still holds data after a delete, and when DSM shows the freed
//! space (survey A5), in the words of DSM's own tools.

use crate::output::Notice;

/// What keeps a blocker class's bytes after the selection is deleted:
/// `(code, blocker classes, message)`, in output order.
const HOLDERS: &[(&str, &[&str], &str)] = &[
    (
        "snapshot",
        &["snapshot", "shared_tree"],
        "snapshots keep this data until they are deleted or expire: see Snapshot Replication > Snapshots, or the share's snapshot list",
    ),
    (
        "hard_link",
        &["hard_link"],
        "other hard links to the same files keep it until they are deleted too",
    ),
    (
        "reflink",
        &["live_reflink", "other_subvolume"],
        "reflink copies elsewhere (File Station or Drive copies on Btrfs, clones in other shared folders) keep the extents they share",
    ),
    (
        "pending_cleaner",
        &["pending_subvolume_deletion"],
        "a deleted snapshot or shared folder still waits for the Btrfs cleaner; its space is freed later without any action",
    ),
    (
        "orphan_inode",
        &["orphan_inode"],
        "files that are deleted but still open keep their space until the program holding them closes them",
    ),
];

/// Reclaim's cleanup, like a delete through File Station, SMB or AFP,
/// moves files into the share's Recycle Bin: space is freed only once it
/// is emptied.
pub const RECYCLE_BIN: &str = "these values are freed once the items are gone for good: Reclaim's cleanup and File Station, SMB and AFP deletes move them into the share's #recycle, which frees nothing until the Recycle Bin is emptied (Control Panel > Shared Folder > Action > Empty Recycle Bin)";

/// Why Storage Manager and `df` show the freed space late.
pub const DELAYED_RELEASE: &str = "Storage Manager and df show freed space only after the next Btrfs transaction commit and, for deleted snapshots and shared folders, after the Btrfs cleaner finishes: minutes, or longer on a busy volume";

/// What `count` subvolumes deleted but not yet cleaned mean for free
/// space; `None` when there are none.
pub fn pending_cleaner(count: usize) -> Option<String> {
    (count > 0).then(|| {
        format!(
            "{count} deleted subvolume(s) (snapshots or shared folders) await the Btrfs cleaner; their space will be freed later without any action, and how much is not known until then"
        )
    })
}

/// What still holds data after deleting a selection with these blocker
/// classes, then the Recycle Bin and delayed-release notes.
pub fn after_delete(blockers: &[&str]) -> Vec<Notice> {
    let mut notes: Vec<Notice> = HOLDERS
        .iter()
        .filter(|(_, classes, _)| classes.iter().any(|c| blockers.contains(c)))
        .map(|(code, _, message)| Notice::new(code, *message))
        .collect();
    notes.push(Notice::new("recycle_bin", RECYCLE_BIN));
    notes.push(Notice::new("delayed_release", DELAYED_RELEASE));
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_deletions_are_space_the_cleaner_frees_later() {
        assert_eq!(pending_cleaner(0), None);
        let text = pending_cleaner(2).unwrap();
        assert!(
            text.starts_with(
                "2 deleted subvolume(s) (snapshots or shared folders) await the Btrfs cleaner"
            ),
            "{text}"
        );
        assert!(text.contains("space will be freed later"), "{text}");
    }

    #[test]
    fn each_blocker_names_what_keeps_the_data_in_dsm_terms() {
        let codes = |blockers: &[&str]| -> Vec<&str> {
            after_delete(blockers).iter().map(|n| n.code).collect()
        };
        assert_eq!(
            codes(&["snapshot", "shared_tree", "hard_link"]),
            ["snapshot", "hard_link", "recycle_bin", "delayed_release"]
        );
        assert_eq!(
            codes(&[
                "live_reflink",
                "other_subvolume",
                "pending_subvolume_deletion"
            ]),
            [
                "reflink",
                "pending_cleaner",
                "recycle_bin",
                "delayed_release"
            ]
        );
        let snapshot = &after_delete(&["snapshot"])[0].message;
        assert!(snapshot.contains("Snapshot Replication"), "{snapshot}");
        let recycle = &after_delete(&[])[0].message;
        assert!(recycle.contains("#recycle"), "{recycle}");
    }
}
