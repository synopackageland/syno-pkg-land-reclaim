//! Which snapshot holds the space, and what deleting a set of snapshots
//! frees (SPEC 5.14). A scan with `--snapshots` reads the file extents of
//! each share's newest snapshots and of the live share, resolves every
//! extent a snapshot references against the extent tree, and keeps one
//! histogram of bytes per owner set. Any selection is then one set
//! operation over that histogram, never a sum of per-snapshot values.

pub mod answer;
pub mod census;

use crate::report::hex_bytes;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Snapshots a `--snapshots` scan materializes per share by default.
pub const DEFAULT_NEWEST: u32 = 64;
/// Tree-block lookups the owner walk of one extent may make.
pub const DEFAULT_OWNER_BUDGET: u32 = 64;
/// Tree-block lookups the owner walk may make across one scan.
pub const DEFAULT_GLOBAL_OWNER_BUDGET: u64 = crate::btrfs::owners::DEFAULT_GLOBAL_OWNER_BUDGET;
/// Files with snapshot-only data one share's census tallies at most.
pub const DEFAULT_HELD_FILES_BUDGET: u64 = 100_000;
/// Largest held files a report keeps per share.
pub const HELD_FILES_KEPT: usize = 100;
/// Where snapshots under the volume-root `@iSCSI` tree are managed.
pub const SAN_MANAGER: &str = crate::dsm::SAN_MANAGER_ISCSI;
/// Where shared-folder snapshots are managed.
pub const SNAPSHOT_REPLICATION: &str = crate::dsm::SNAPSHOT_REPLICATION_SNAPSHOTS;

/// The management surface recorded from a report's full subvolume path.
pub fn manage_in(subvolume_path: &[u8]) -> &'static str {
    if subvolume_path
        .split(|&b| b == b'/')
        .any(|part| part == b"@iSCSI")
    {
        SAN_MANAGER
    } else {
        SNAPSHOT_REPLICATION
    }
}

/// The stable id of that surface (SPEC 4.6, GAP-14).
pub fn manage_in_id(subvolume_path: &[u8]) -> &'static str {
    crate::dsm::place_id(manage_in(subvolume_path))
}

/// How many snapshots of each share a scan reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    /// The newest this many, by creation time.
    Newest(u32),
    All,
}

impl Depth {
    /// How many of `count` snapshots it reads.
    pub fn take(self, count: usize) -> usize {
        match self {
            Depth::Newest(n) => count.min(n as usize),
            Depth::All => count,
        }
    }
}

/// What a `--snapshots` scan reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotScan {
    pub depth: Depth,
    pub owner_budget: u32,
    #[serde(default = "default_global_owner_budget")]
    pub global_owner_budget: u64,
    #[serde(default = "default_held_files_budget")]
    pub held_files_budget: u64,
}

fn default_held_files_budget() -> u64 {
    DEFAULT_HELD_FILES_BUDGET
}

fn default_global_owner_budget() -> u64 {
    DEFAULT_GLOBAL_OWNER_BUDGET
}

/// Who, besides the owners, may still reference an extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// Only the owners reference it.
    Resolved,
    /// A root that was not read references it too.
    External,
    /// Whether another root references it is not known: an unread root
    /// may, or its backrefs could not be resolved.
    Unknown,
}

/// Bytes of the extents referenced by exactly `owners` among the roots
/// read (SPEC 5.14 step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    /// Subvolume ids, ascending.
    pub owners: Vec<u64>,
    pub hold: Hold,
    pub bytes: u64,
    pub extents: u64,
    /// Unreferenced bytes within extents freed by this exact owner set.
    #[serde(default)]
    pub pinned_unreferenced_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub id: u64,
    #[serde(with = "hex_bytes")]
    pub name: Vec<u8>,
    /// The full linked path captured at scan time, for service-specific hints.
    #[serde(default, with = "hex_bytes")]
    pub subvolume_path: Vec<u8>,
    /// Unix seconds.
    pub otime: Option<u64>,
    pub generation: u64,
    /// Its file extents were read.
    pub materialized: bool,
}

/// A file whose data extents only the share's snapshots reference
/// (SPEC 5.14 "Files held only by snapshots").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldFile {
    /// The newest read snapshot that references it.
    pub snapshot: u64,
    pub ino: u64,
    /// Below that snapshot's root; empty when the name is unresolved.
    #[serde(with = "hex_bytes")]
    pub path: Vec<u8>,
    /// The live share still has the inode: rewritten, not deleted.
    pub in_live: bool,
    /// Extents only snapshots reference, each counted once.
    pub bytes: u64,
    /// Extents only snapshots reference among the roots read, whose
    /// other owners could not be ruled out.
    pub unknown_bytes: u64,
}

/// The largest held files of a share.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldFiles {
    /// Every file with snapshot-only data was tallied.
    pub complete: bool,
    /// Files tallied.
    pub count: u64,
    /// Largest first, at most `HELD_FILES_KEPT`.
    pub files: Vec<HeldFile>,
}

/// A live subvolume and its snapshots, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareAnalysis {
    pub live: u64,
    #[serde(with = "hex_bytes")]
    pub name: Vec<u8>,
    /// The subvolume-root path captured at scan time, used for DSM hints.
    #[serde(default, with = "hex_bytes")]
    pub subvolume_path: Vec<u8>,
    pub snapshots: Vec<SnapshotInfo>,
    /// Deleted snapshots awaiting the cleaner: they never hold bytes back.
    pub pending: Vec<u64>,
    /// `None` in reports written before the list existed.
    #[serde(default)]
    pub held_files: Option<HeldFiles>,
}

impl ShareAnalysis {
    pub fn materialized(&self) -> impl Iterator<Item = &SnapshotInfo> {
        self.snapshots.iter().filter(|s| s.materialized)
    }

    /// Whether `id` is its live subvolume, a snapshot, or a pending one.
    fn holds_root(&self, id: u64) -> bool {
        self.live == id || self.pending.contains(&id) || self.snapshots.iter().any(|s| s.id == id)
    }

    pub fn skipped(&self) -> usize {
        self.snapshots.iter().filter(|s| !s.materialized).count()
    }
}

/// DSM's snapshot service for this share, derived from its live and snapshot
/// paths persisted in the scan report.
pub fn manage_in_share(share: &ShareAnalysis) -> &'static str {
    if manage_in(&share.subvolume_path) == SAN_MANAGER
        || share
            .snapshots
            .iter()
            .any(|snapshot| manage_in(&snapshot.subvolume_path) == SAN_MANAGER)
    {
        SAN_MANAGER
    } else {
        SNAPSHOT_REPLICATION
    }
}

/// The stable id of the DSM service for this share (SPEC 4.6, GAP-14).
pub fn manage_in_share_id(share: &ShareAnalysis) -> &'static str {
    crate::dsm::place_id(manage_in_share(share))
}

/// The quota tree's view, kept only when it is worth a cross-check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaCheck {
    /// Quotas on, no rescan running, not flagged inconsistent.
    pub consistent: bool,
    /// Level-0 exclusive bytes of the listed subvolumes.
    pub exclusive: BTreeMap<u64, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<AnalysisMeasurement>,
    pub trees_read: u64,
    pub items: u64,
    /// Extents some read snapshot references.
    pub extents: u64,
    pub owner_lookups: u64,
    /// Extents whose owner walk hit the budget or a broken block.
    pub owner_unresolved_extents: u64,
}

/// Time and memory of an additional snapshot-only phase. Namespace figures
/// remain those of the original scan, with their original freshness evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisMeasurement {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_live_subvolumes: Vec<u64>,
    pub started: u64,
    pub finished: u64,
    pub elapsed_ms: u64,
    pub peak_rss_bytes: Option<u64>,
}

/// What a `--snapshots` scan found, kept in the report header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAnalysis {
    pub depth: Depth,
    pub owner_budget: u32,
    #[serde(default = "default_global_owner_budget")]
    pub global_owner_budget: u64,
    pub shares: Vec<ShareAnalysis>,
    pub holdings: Vec<Holding>,
    /// `None` without a quota tree.
    pub quota: Option<QuotaCheck>,
    pub stats: AnalysisStats,
}

/// Snapshot roots a path scan's selection is deleted with, and how to
/// read their shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionRequest {
    pub snapshots: BTreeSet<u64>,
    pub scan: SnapshotScan,
}

/// What deleting snapshot roots and live paths together frees (SPEC
/// 5.14 mixed sets): extents every reference of which goes with them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionAnswer {
    /// The snapshot roots deleted, ascending.
    pub snapshots: Vec<u64>,
    pub freed_bytes: u64,
    /// Bytes of extents that may be freed but could not be proven.
    pub unknown_bytes: u64,
    /// Extents whose references could not be resolved; the bytes of a
    /// missing or malformed one are not known at all.
    pub unknown_extents: u64,
    /// Snapshots of the selection's shares that were not read.
    pub snapshots_not_materialized: u64,
    pub stats: AnalysisStats,
}

/// What deleting a selection frees: bytes proven freed, and bytes it
/// may free that could not be proven.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Freed {
    pub bytes: u64,
    pub unknown: u64,
    /// Extents behind `unknown`, including those whose size is not known.
    pub unknown_extents: u64,
    /// External bytes a selected snapshot of another share may be the
    /// external owner of; an extent may count once per share.
    pub cross_share: u64,
    pub cross_share_extents: u64,
}

impl SnapshotAnalysis {
    /// A changing live tree can make the census miss a new live reference.
    /// Keep those sizes as bounds while preserving stable shares' answers.
    pub fn invalidate_live_comparison(&mut self, lives: &BTreeSet<u64>) {
        let roots: BTreeSet<u64> = self
            .shares
            .iter()
            .filter(|s| lives.contains(&s.live))
            .flat_map(|s| std::iter::once(s.live).chain(s.snapshots.iter().map(|s| s.id)))
            .collect();
        for holding in &mut self.holdings {
            if holding.hold == Hold::Resolved && holding.owners.iter().any(|id| roots.contains(id))
            {
                holding.hold = Hold::Unknown;
                holding.pinned_unreferenced_bytes = None;
            }
        }
        for share in self.shares.iter_mut().filter(|s| lives.contains(&s.live)) {
            if let Some(files) = &mut share.held_files {
                for file in &mut files.files {
                    file.unknown_bytes = file.unknown_bytes.saturating_add(file.bytes);
                    file.bytes = 0;
                }
            }
        }
    }

    /// Deleting every root in `selected` together: extents all of whose
    /// read owners are selected (SPEC 5.14 "freed by deleting set S").
    pub fn freed(&self, selected: &BTreeSet<u64>) -> Freed {
        let mut freed = Freed::default();
        for h in &self.holdings {
            if !h.owners.iter().all(|o| selected.contains(o)) {
                continue;
            }
            match h.hold {
                Hold::Resolved => freed.bytes += h.bytes,
                Hold::Unknown => {
                    freed.unknown += h.bytes;
                    freed.unknown_extents += h.extents;
                }
                Hold::External if self.reaches_another_share(h, selected) => {
                    freed.cross_share += h.bytes;
                    freed.cross_share_extents += h.extents;
                }
                Hold::External => {}
            }
        }
        freed
    }

    /// Whether `selected` holds a root outside the share whose census
    /// counted `holding`.
    fn reaches_another_share(&self, holding: &Holding, selected: &BTreeSet<u64>) -> bool {
        let Some(share) = holding
            .owners
            .first()
            .and_then(|&id| self.shares.iter().find(|s| s.holds_root(id)))
        else {
            return false;
        };
        selected.iter().any(|&id| !share.holds_root(id))
    }

    /// Bytes referenced by both `a` and `b`.
    pub fn shared_between(&self, a: u64, b: u64) -> u64 {
        self.holdings
            .iter()
            .filter(|h| h.owners.contains(&a) && h.owners.contains(&b))
            .map(|h| h.bytes)
            .sum()
    }

    pub fn share(&self, live: u64) -> Option<&ShareAnalysis> {
        self.shares.iter().find(|s| s.live == live)
    }

    /// The shares a target answers for: every one for a volume, else
    /// the one whose live subvolume holds the target.
    pub fn shares_for(&self, volume: bool, tree: u64) -> Vec<&ShareAnalysis> {
        self.shares
            .iter()
            .filter(|s| volume || s.live == tree)
            .collect()
    }

    /// Snapshot `id` and its share.
    pub fn snapshot(&self, id: u64) -> Option<(&ShareAnalysis, &SnapshotInfo)> {
        self.shares.iter().find_map(|share| {
            share
                .snapshots
                .iter()
                .find(|s| s.id == id)
                .map(|s| (share, s))
        })
    }

    /// Qgroup exclusive bytes of `id`, when the numbers are consistent.
    pub fn qgroup_exclusive(&self, id: u64) -> Option<u64> {
        let quota = self.quota.as_ref().filter(|q| q.consistent)?;
        quota.exclusive.get(&id).copied()
    }

    /// Pinned bytes in extents freed by deleting snapshot `id` alone.
    /// `None` means at least one relevant extent has unresolved ownership
    /// or a file-range mapping that cannot be measured exactly.
    pub fn pinned_unreferenced(&self, id: u64) -> Option<u64> {
        if self
            .holdings
            .iter()
            .any(|h| h.owners.contains(&id) && h.hold == Hold::Unknown)
        {
            return None;
        }
        self.holdings
            .iter()
            .filter(|h| h.owners.as_slice() == [id] && h.hold == Hold::Resolved)
            .try_fold(0u64, |total, h| {
                total.checked_add(h.pinned_unreferenced_bytes?)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_uses_the_full_root_tree_path() {
        assert_eq!(manage_in(b"@iSCSI/lun-uuid"), SAN_MANAGER);
        assert_eq!(manage_in(b"@syno/@iSCSI/Snapshot/lun-uuid"), SAN_MANAGER);
        assert_eq!(manage_in(b"backup/@iSCSI/lun-uuid"), SAN_MANAGER);
        assert_eq!(
            manage_in(b"backup/name@iSCSI/lun-uuid"),
            SNAPSHOT_REPLICATION
        );
        assert_eq!(manage_in(b"backup/snapshot-name"), SNAPSHOT_REPLICATION);
    }

    #[test]
    fn older_snapshot_reports_without_paths_still_load() {
        let info = SnapshotInfo {
            id: 300,
            name: b"snapshot".to_vec(),
            subvolume_path: b"@iSCSI/Snapshot/snapshot".to_vec(),
            otime: Some(1),
            generation: 2,
            materialized: true,
        };
        let mut value = serde_json::to_value(info).unwrap();
        value.as_object_mut().unwrap().remove("subvolume_path");

        let decoded: SnapshotInfo = serde_json::from_value(value).unwrap();

        assert!(decoded.subvolume_path.is_empty());
    }

    fn holding(owners: &[u64], hold: Hold, bytes: u64) -> Holding {
        Holding {
            owners: owners.to_vec(),
            hold,
            bytes,
            extents: 1,
            pinned_unreferenced_bytes: (hold == Hold::Resolved && owners.len() == 1).then_some(0),
        }
    }

    fn analysis(holdings: Vec<Holding>) -> SnapshotAnalysis {
        SnapshotAnalysis {
            depth: Depth::Newest(DEFAULT_NEWEST),
            owner_budget: DEFAULT_OWNER_BUDGET,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            shares: Vec::new(),
            holdings,
            quota: None,
            stats: AnalysisStats::default(),
        }
    }

    fn set(ids: &[u64]) -> BTreeSet<u64> {
        ids.iter().copied().collect()
    }

    /// Live share 256 and snapshots 300 (older) and 301.
    fn sample() -> SnapshotAnalysis {
        analysis(vec![
            holding(&[300], Hold::Resolved, 10),
            holding(&[301], Hold::Resolved, 20),
            holding(&[300, 301], Hold::Resolved, 40),
            holding(&[256, 300, 301], Hold::Resolved, 80),
            holding(&[301], Hold::Unknown, 160),
            holding(&[300], Hold::External, 320),
        ])
    }

    #[test]
    fn changed_live_data_makes_only_its_share_unknown() {
        let mut a = two_shares();
        let stable = a.freed(&set(&[400]));
        assert!(a.freed(&set(&[300])).bytes > 0);
        a.invalidate_live_comparison(&set(&[256]));
        assert_eq!(a.freed(&set(&[300])).bytes, 0);
        assert!(a.freed(&set(&[300])).unknown > 0);
        assert_eq!(a.freed(&set(&[400])), stable);
    }

    #[test]
    fn a_set_frees_what_only_its_members_hold_not_the_sum_of_exclusives() {
        let a = sample();
        let freed = |ids: &[u64]| {
            let f = a.freed(&set(ids));
            (f.bytes, f.unknown)
        };
        assert_eq!(freed(&[300]), (10, 0));
        assert_eq!(freed(&[301]), (20, 160));
        assert_eq!(freed(&[300, 301]), (70, 160));
    }

    fn share(live: u64, snapshots: &[u64]) -> ShareAnalysis {
        ShareAnalysis {
            live,
            name: Vec::new(),
            subvolume_path: Vec::new(),
            snapshots: snapshots
                .iter()
                .map(|&id| SnapshotInfo {
                    id,
                    name: Vec::new(),
                    subvolume_path: Vec::new(),
                    otime: None,
                    generation: 1,
                    materialized: true,
                })
                .collect(),
            pending: Vec::new(),
            held_files: None,
        }
    }

    /// Share 256 with snapshots 300 and 301; share 257 with snapshot 400.
    /// A reflink copy across the shares is kept only by 300 and 400.
    fn two_shares() -> SnapshotAnalysis {
        let mut a = sample();
        a.holdings.push(holding(&[400], Hold::External, 640));
        a.holdings.push(holding(&[256], Hold::External, 1280));
        a.shares = vec![share(256, &[300, 301]), share(257, &[400])];
        a
    }

    #[test]
    fn external_bytes_may_be_freed_only_by_a_set_across_shares() {
        let a = two_shares();
        assert_eq!(a.freed(&set(&[300, 301])).cross_share, 0);
        assert_eq!(a.freed(&set(&[400])).cross_share, 0);
        let across = a.freed(&set(&[300, 400]));
        assert_eq!((across.bytes, across.cross_share), (10, 320 + 640));
        assert_eq!(across.cross_share_extents, 2);
    }

    #[test]
    fn unknown_extents_count_even_when_their_size_is_not_known() {
        let mut a = sample();
        a.holdings.push(Holding {
            extents: 3,
            ..holding(&[300], Hold::Unknown, 0)
        });
        assert_eq!(a.freed(&set(&[300])).unknown_extents, 3);
        assert_eq!(a.freed(&set(&[301])).unknown_extents, 1);
        assert_eq!(a.freed(&set(&[256])).unknown_extents, 0);
    }

    #[test]
    fn bytes_shared_with_the_live_share_are_never_freed() {
        let a = sample();
        assert_eq!(a.shared_between(300, 256), 80);
        assert_eq!(a.shared_between(301, 256), 80);
        assert_eq!(a.freed(&set(&[256, 300, 301])).bytes, 150);
    }

    #[test]
    fn a_volume_answers_for_every_share_and_a_path_for_its_own() {
        let mut a = sample();
        a.shares = [256, 257]
            .map(|live| ShareAnalysis {
                live,
                name: Vec::new(),
                subvolume_path: Vec::new(),
                snapshots: Vec::new(),
                pending: Vec::new(),
                held_files: None,
            })
            .to_vec();
        let lives = |shares: Vec<&ShareAnalysis>| shares.iter().map(|s| s.live).collect::<Vec<_>>();
        assert_eq!(lives(a.shares_for(true, 5)), vec![256, 257]);
        assert_eq!(lives(a.shares_for(false, 257)), vec![257]);
        assert!(a.shares_for(false, 5).is_empty());
    }

    #[test]
    fn depth_bounds_how_many_snapshots_are_read() {
        assert_eq!(Depth::Newest(64).take(10), 10);
        assert_eq!(Depth::Newest(64).take(100), 64);
        assert_eq!(Depth::All.take(100), 100);
    }

    #[test]
    fn qgroup_numbers_show_only_when_consistent() {
        let mut a = sample();
        assert_eq!(a.qgroup_exclusive(300), None);
        a.quota = Some(QuotaCheck {
            consistent: false,
            exclusive: BTreeMap::from([(300, 12)]),
        });
        assert_eq!(a.qgroup_exclusive(300), None);
        a.quota.as_mut().unwrap().consistent = true;
        assert_eq!(a.qgroup_exclusive(300), Some(12));
        assert_eq!(a.qgroup_exclusive(301), None);
    }
}
