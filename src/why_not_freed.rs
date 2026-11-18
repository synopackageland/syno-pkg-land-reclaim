//! Why deleted data still holds space (SPEC 5.15): each mechanism that
//! keeps deleted data allocated, with bytes only where a report or a live
//! probe measured them. Unknown stays `None`, never zero.

use crate::model::accounting::Blockers;
use crate::overview::{OverviewData, UnknownReason};
use crate::report::Header;
use crate::snapshots::answer::{HeldFilesJson, Listing, held_files_json};
use crate::snapshots::{SNAPSHOT_REPLICATION, ShareAnalysis, manage_in_share, manage_in_share_id};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A shared folder's name: display text, plus lossless hex when the
/// bytes are not UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareName {
    pub share: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_hex: Option<String>,
}

impl ShareName {
    /// The name the report recorded for share `id`; `None` when it
    /// recorded none.
    fn of(data: &OverviewData, id: u64) -> Option<Self> {
        let bytes = crate::report::hex_bytes::decode(data.share_names_hex.get(&id)?)?;
        let path = crate::output::JsonPath::new(&bytes);
        Some(ShareName {
            share: path.path,
            share_hex: path.path_hex,
        })
    }
}

/// Deleted subvolumes the Btrfs cleaner has not freed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingCleanup {
    /// Pending roots now; `None` when the live probe failed.
    pub count: Option<usize>,
    /// Bytes the report attributed to pending roots, while its pending
    /// set is still the live one; `None` when that is not known.
    pub bytes: Option<u64>,
    /// Pending now but not when the report was taken.
    pub appeared_after_report: Option<usize>,
    /// Pending when the report was taken but already cleaned.
    pub cleaned_since_report: Option<usize>,
    /// The report left bytes unresolved, and some may belong here.
    pub lower_bound: bool,
    /// Active pending roots cannot be filtered because reports lack a deletion time.
    pub since_not_measured: bool,
}

/// The pending-cleanup row from the report's pending roots and bytes and
/// the live probe.
pub fn pending_cleanup(
    report_roots: Option<&[u64]>,
    report_bytes: u64,
    live: Option<&BTreeSet<u64>>,
) -> PendingCleanup {
    let Some(live) = live else {
        return PendingCleanup {
            count: None,
            bytes: None,
            appeared_after_report: None,
            cleaned_since_report: None,
            lower_bound: false,
            since_not_measured: false,
        };
    };
    let report: Option<BTreeSet<u64>> = report_roots.map(|ids| ids.iter().copied().collect());
    let appeared = report.as_ref().map(|r| live.difference(r).count());
    let cleaned = report.as_ref().map(|r| r.difference(live).count());
    // The report's pending bytes are one total: they stand for the live
    // set only while it is the report's set.
    let bytes = if live.is_empty() {
        Some(0)
    } else if appeared == Some(0) && cleaned == Some(0) {
        Some(report_bytes)
    } else {
        None
    };
    PendingCleanup {
        count: Some(live.len()),
        bytes,
        appeared_after_report: appeared,
        cleaned_since_report: cleaned,
        lower_bound: false,
        since_not_measured: false,
    }
}

/// Extents held only by one share's snapshots (overview part 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotShare {
    pub subvolume_id: u64,
    #[serde(flatten)]
    pub name: Option<ShareName>,
    pub bytes: u64,
    /// Snapshots of the share at the end of the scan.
    pub snapshots: Option<usize>,
    /// Unix seconds of its newest snapshot, when a `--snapshots` scan
    /// read it: only data deleted before then can be held here.
    pub newest_snapshot_otime: Option<u64>,
    pub manage_in: &'static str,
    /// Stable id of that DSM place (SPEC 4.6, GAP-14).
    pub manage_in_id: &'static str,
    /// Its largest files only snapshots hold, when a `--snapshots` scan
    /// listed them (SPEC 5.14).
    pub files: Option<HeldFilesJson>,
}

/// Deleted (or overwritten) share data that snapshots still hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldBySnapshots {
    pub bytes: u64,
    /// Held only by snapshots, but of more than one share.
    pub multi_share_bytes: u64,
    /// By share, most bytes first.
    pub items: Vec<SnapshotShare>,
    /// The report left bytes unresolved, and some may belong here.
    pub lower_bound: bool,
}

/// The snapshot row from a volume report's overview attribution, naming
/// at most `limit` files per share.
pub fn held_by_snapshots(header: &Header, data: &OverviewData, limit: usize) -> HeldBySnapshots {
    let listing = Listing {
        mount: &header.mount,
        limit,
    };
    // The census can name files whose bytes the volume report left
    // unattributed (COWed leaves): those shares show with no bytes.
    let named = header
        .snapshots
        .iter()
        .flat_map(|a| &a.shares)
        .filter(|share| {
            share.held_files.as_ref().is_some_and(|held| held.count > 0)
                && !data.snapshot_only_bytes.contains_key(&share.live)
        });
    let mut items: Vec<SnapshotShare> = data
        .snapshot_only_bytes
        .iter()
        .map(|(&id, &bytes)| (id, bytes))
        .chain(named.map(|share| (share.live, 0)))
        .map(|(id, bytes)| {
            let analysis = header
                .snapshots
                .as_ref()
                .and_then(|a| a.shares.iter().find(|share| share.live == id));
            SnapshotShare {
                subvolume_id: id,
                name: ShareName::of(data, id),
                bytes,
                snapshots: header
                    .subvolumes
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.snapshots.len()),
                newest_snapshot_otime: analysis.and_then(newest_otime),
                manage_in: analysis.map_or(SNAPSHOT_REPLICATION, manage_in_share),
                manage_in_id: analysis.map_or(
                    crate::dsm::place_id(SNAPSHOT_REPLICATION),
                    manage_in_share_id,
                ),
                files: analysis.and_then(|share| {
                    let held = share.held_files.as_ref()?;
                    Some(held_files_json(share, held, &listing))
                }),
            }
        })
        .collect();
    items.sort_by_key(|i| Reverse(i.bytes));
    let multi_share_bytes = data.snapshot_multi_share_bytes;
    HeldBySnapshots {
        bytes: items.iter().map(|i| i.bytes).sum::<u64>() + multi_share_bytes,
        multi_share_bytes,
        items,
        lower_bound: false,
    }
}

fn newest_otime(share: &ShareAnalysis) -> Option<u64> {
    share.snapshots.iter().filter_map(|s| s.otime).max()
}

/// One share's Recycle Bin (overview part 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecycleShare {
    pub subvolume_id: u64,
    #[serde(flatten)]
    pub name: Option<ShareName>,
    pub bytes: u64,
    pub reclaimable_bytes: u64,
}

/// Deleted files moved into Recycle Bins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecycleBins {
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    /// The `--since` window, when given.
    pub since_seconds: Option<u64>,
    /// Physical bytes attributed to entries with ctime in the window;
    /// `None` when any attributed Recycle Bin bytes lack a ctime.
    pub since_bytes: Option<u64>,
    /// By share, most bytes first.
    pub items: Vec<RecycleShare>,
    /// The report left bytes unresolved, and some may belong here.
    pub lower_bound: bool,
}

/// The Recycle Bin row; `since` asks for the recently moved part.
pub fn recycle_bins(data: &OverviewData, since: Option<Duration>, now: SystemTime) -> RecycleBins {
    let mut items: Vec<RecycleShare> = data
        .recycle_bin
        .iter()
        .map(|(&id, owned)| RecycleShare {
            subvolume_id: id,
            name: ShareName::of(data, id),
            bytes: owned.bytes,
            reclaimable_bytes: owned.reclaimable_bytes,
        })
        .collect();
    items.sort_by_key(|i| Reverse(i.bytes));
    let since_bytes = since.and_then(|duration| {
        let total = data
            .recycle_bin
            .values()
            .fold(0u64, |sum, share| sum.saturating_add(share.bytes));
        if total == 0 {
            return Some(0);
        }
        let times = data.recycle_ctime.as_ref()?;
        if times.unmeasured_bytes > 0 {
            return None;
        }
        let measured = times
            .bytes
            .values()
            .flat_map(|ages| ages.values())
            .fold(0u64, |sum, age| sum.saturating_add(age.bytes));
        if measured != total {
            return None;
        }
        let cutoff = crate::time::unix_seconds(now.checked_sub(duration).unwrap_or(UNIX_EPOCH));
        Some(
            times
                .bytes
                .values()
                .flat_map(|ages| ages.range(cutoff..).map(|(_, bytes)| bytes.bytes))
                .fold(0u64, u64::saturating_add),
        )
    });
    RecycleBins {
        bytes: items.iter().map(|i| i.bytes).sum(),
        reclaimable_bytes: items.iter().map(|i| i.reclaimable_bytes).sum(),
        since_seconds: since.map(|d| d.as_secs()),
        since_bytes,
        items,
        lower_bound: false,
    }
}

/// Extents that could not be attributed: they may hold deleted data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unresolved {
    pub bytes: u64,
    pub reasons: Vec<UnresolvedPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnresolvedPart {
    pub code: UnknownReason,
    pub bytes: u64,
}

/// The overview's unknown part, by reason.
pub fn unresolved(data: &OverviewData) -> Unresolved {
    let mut reasons: Vec<UnresolvedPart> = data
        .unknown
        .iter()
        .map(|(&code, &bytes)| UnresolvedPart { code, bytes })
        .collect();
    reasons.sort_by_key(|r| Reverse(r.bytes));
    Unresolved {
        bytes: reasons.iter().map(|r| r.bytes).sum(),
        reasons,
    }
}

/// A deleted regular file of the volume that a process still has open or
/// mapped, as a `/proc` scan found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDeleted {
    pub pid: u32,
    pub comm: Vec<u8>,
    pub dev: u64,
    pub ino: u64,
    /// `st_blocks × 512`.
    pub allocated_bytes: u64,
}

/// One process holding deleted files of the volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenHolder {
    pub pid: u32,
    pub comm: String,
    pub inodes: usize,
    /// Allocated bytes of those inodes, an estimate.
    pub bytes_estimate: u64,
}

/// What a one-shot `/proc` scan found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessScan {
    /// Allocated bytes of the distinct inodes held, an estimate.
    pub bytes_estimate: u64,
    /// Most bytes first.
    pub holders: Vec<OpenHolder>,
    /// Processes whose descriptors or mappings could not be read.
    pub unreadable_processes: u64,
}

/// `(st_dev, st_ino)`.
type Inode = (u64, u64);

/// Groups what the scan found by process, each inode counted once in
/// the total however many processes or descriptors hold it.
pub fn process_scan(found: &[OpenDeleted], unreadable_processes: u64) -> ProcessScan {
    let mut inodes: BTreeMap<Inode, u64> = BTreeMap::new();
    let mut by_pid: BTreeMap<u32, (&[u8], BTreeSet<Inode>)> = BTreeMap::new();
    for file in found {
        inodes.insert((file.dev, file.ino), file.allocated_bytes);
        by_pid
            .entry(file.pid)
            .or_insert_with(|| (&file.comm, BTreeSet::new()))
            .1
            .insert((file.dev, file.ino));
    }
    let mut holders: Vec<OpenHolder> = by_pid
        .into_iter()
        .map(|(pid, (comm, held))| OpenHolder {
            pid,
            comm: crate::output::escape_human(comm),
            inodes: held.len(),
            bytes_estimate: held.iter().map(|inode| inodes[inode]).sum(),
        })
        .collect();
    holders.sort_by_key(|h| Reverse(h.bytes_estimate));
    ProcessScan {
        bytes_estimate: inodes.values().sum(),
        holders,
        unreadable_processes,
    }
}

/// Deleted files that are still open, from the report and optionally
/// from a live `/proc` scan (`--processes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenUnlinked {
    /// Bytes the report found retained by inodes without names.
    pub report_orphan_bytes: u64,
    /// `None` without `--processes`.
    pub processes: Option<ProcessScan>,
}

/// The commit interval from a Btrfs mount's options (`commit=N`, default
/// 30 s).
pub fn commit_interval(options: &[Vec<u8>]) -> u64 {
    options
        .iter()
        .find_map(|option| {
            std::str::from_utf8(option.strip_prefix(b"commit=")?)
                .ok()?
                .parse()
                .ok()
        })
        .unwrap_or(DEFAULT_COMMIT_SECONDS)
}

/// The Btrfs default transaction commit interval.
const DEFAULT_COMMIT_SECONDS: u64 = 30;

/// One mechanism that keeps deleted data allocated, in the order the
/// command lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Reason {
    PendingSubvolumeCleanup(PendingCleanup),
    HeldBySnapshots(HeldBySnapshots),
    RecycleBin(RecycleBins),
    OpenUnlinked(OpenUnlinked),
    /// Names that no longer exist leave no trace: never derivable here.
    HardLink {
        bytes: Option<u64>,
    },
    /// Frees nothing by itself: deletions show once committed.
    CommitPending {
        interval_s: u64,
    },
    UnresolvedReferences(Unresolved),
}

impl Reason {
    /// A value this row should give is not known. The hard-link row is
    /// unknown by definition and does not count.
    pub fn is_unknown(&self) -> bool {
        match self {
            Reason::PendingSubvolumeCleanup(row) => {
                row.count.is_none() || row.bytes.is_none() || row.since_not_measured
            }
            Reason::RecycleBin(row) => row.since_seconds.is_some() && row.since_bytes.is_none(),
            Reason::OpenUnlinked(row) => row
                .processes
                .as_ref()
                .is_some_and(|scan| scan.unreadable_processes > 0),
            Reason::UnresolvedReferences(row) => row.bytes > 0,
            Reason::HeldBySnapshots(_) | Reason::HardLink { .. } | Reason::CommitPending { .. } => {
                false
            }
        }
    }

    pub fn since_not_measured(&self) -> bool {
        match self {
            Reason::PendingSubvolumeCleanup(row) => row.since_not_measured,
            Reason::RecycleBin(row) => row.since_seconds.is_some() && row.since_bytes.is_none(),
            _ => false,
        }
    }
}

/// The live, cheap probes taken when the command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probes {
    /// Subvolumes awaiting the cleaner now; `None` when the probe failed.
    pub pending: Option<BTreeSet<u64>>,
    /// `None` without `--processes`.
    pub processes: Option<ProcessScan>,
    pub commit_interval_s: u64,
}

/// Every row, from a volume report with overview data and the probes.
pub fn reasons(
    header: &Header,
    data: &OverviewData,
    probes: Probes,
    since: Option<Duration>,
    files_limit: usize,
) -> Vec<Reason> {
    let unresolved = unresolved(data);
    let lower_bound = unresolved.bytes > 0;
    let mut pending = pending_cleanup(
        data.pending_roots.as_deref(),
        data.pending_cleanup_bytes,
        probes.pending.as_ref(),
    );
    pending.lower_bound = lower_bound;
    pending.since_not_measured = since.is_some() && pending.count.is_none_or(|count| count > 0);
    let mut snapshots = held_by_snapshots(header, data, files_limit);
    snapshots.lower_bound = lower_bound;
    let mut recycle = recycle_bins(data, since, SystemTime::now());
    recycle.lower_bound = lower_bound;
    vec![
        Reason::PendingSubvolumeCleanup(pending),
        Reason::HeldBySnapshots(snapshots),
        Reason::RecycleBin(recycle),
        Reason::OpenUnlinked(OpenUnlinked {
            report_orphan_bytes: header.accounting.blocker_bytes(Blockers::ORPHAN_INODE),
            processes: probes.processes,
        }),
        Reason::HardLink { bytes: None },
        Reason::CommitPending {
            interval_s: probes.commit_interval_s,
        },
        Reason::UnresolvedReferences(unresolved),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::output::JsonPath;
    use crate::overview::OwnerBytes;
    use crate::report::testing::header;
    use crate::report::{SubvolumeRole, SubvolumeState};
    use crate::snapshots::{Depth, SAN_MANAGER, SnapshotAnalysis, SnapshotInfo};

    fn named(shares: &[(u64, &[u8])]) -> OverviewData {
        OverviewData {
            share_names_hex: shares
                .iter()
                .map(|(id, name)| (*id, crate::report::hex_bytes::encode(name)))
                .collect(),
            ..OverviewData::default()
        }
    }

    fn live(id: u64, snapshots: Vec<u64>) -> SubvolumeState {
        SubvolumeState {
            id,
            role: SubvolumeRole::Scanned,
            generation_before: 9,
            generation_after: Some(9),
            captured: None,
            uuid: None,
            last_snapshot: None,
            snapshots,
        }
    }

    fn analysed(live: u64, otimes: &[u64], path: &[u8]) -> ShareAnalysis {
        ShareAnalysis {
            live,
            name: b"photos".to_vec(),
            subvolume_path: path.to_vec(),
            snapshots: otimes
                .iter()
                .enumerate()
                .map(|(i, &otime)| SnapshotInfo {
                    id: 1000 + i as u64,
                    name: Vec::new(),
                    subvolume_path: Vec::new(),
                    otime: Some(otime),
                    generation: 1,
                    materialized: true,
                })
                .collect(),
            pending: Vec::new(),
            held_files: None,
        }
    }

    #[test]
    fn snapshot_only_bytes_are_listed_per_share_most_first() {
        let mut data = named(&[(261, b"photos"), (262, b"backup")]);
        data.snapshot_only_bytes = BTreeMap::from([(261, 4), (262, 9)]);
        data.snapshot_multi_share_bytes = 2;
        let mut h = header(Vec::new());
        h.subvolumes = vec![live(261, vec![1000, 1001]), live(262, vec![1002])];
        h.snapshots = Some(SnapshotAnalysis {
            depth: Depth::All,
            owner_budget: 64,
            global_owner_budget: 100,
            shares: vec![analysed(261, &[50, 70], b"@iSCSI/lun")],
            holdings: Vec::new(),
            quota: None,
            stats: Default::default(),
        });

        let row = held_by_snapshots(&h, &data, 10);

        assert_eq!((row.bytes, row.multi_share_bytes), (15, 2));
        let ids: Vec<u64> = row.items.iter().map(|i| i.subvolume_id).collect();
        assert_eq!(ids, [262, 261]);
        assert_eq!(row.items[0].name.as_ref().unwrap().share, "backup");
        assert_eq!(row.items[0].snapshots, Some(1));
        assert_eq!(row.items[0].newest_snapshot_otime, None);
        assert_eq!(row.items[0].manage_in, SNAPSHOT_REPLICATION);
        assert_eq!(row.items[1].snapshots, Some(2));
        assert_eq!(row.items[1].newest_snapshot_otime, Some(70));
        assert_eq!(row.items[1].manage_in, SAN_MANAGER);
        assert_eq!(row.items[0].files, None, "no --snapshots analysis of it");
    }

    #[test]
    fn the_snapshot_row_names_the_largest_files_a_snapshots_scan_found() {
        let mut data = named(&[(261, b"photos")]);
        data.snapshot_only_bytes = BTreeMap::from([(261, 9)]);
        let mut share = analysed(261, &[50], b"photos");
        share.held_files = Some(crate::snapshots::HeldFiles {
            complete: true,
            count: 2,
            files: [(257, b"a.jpg"), (258, b"b.jpg")]
                .map(|(ino, path)| crate::snapshots::HeldFile {
                    snapshot: 1000,
                    ino,
                    path: path.to_vec(),
                    in_live: false,
                    bytes: 4,
                    unknown_bytes: 0,
                })
                .to_vec(),
        });
        let mut h = header(Vec::new());
        h.snapshots = Some(SnapshotAnalysis {
            depth: Depth::All,
            owner_budget: 64,
            global_owner_budget: 100,
            shares: vec![share],
            holdings: Vec::new(),
            quota: None,
            stats: Default::default(),
        });

        let row = held_by_snapshots(&h, &data, 1);

        let files = row.items[0].files.as_ref().unwrap();
        assert_eq!((files.complete, files.count), (true, 2));
        assert_eq!(files.files.len(), 1);
        let live = format!("{}/photos/a.jpg", String::from_utf8_lossy(&h.mount));
        assert_eq!(
            files.files[0].live_path,
            Some(JsonPath::new(live.as_bytes()))
        );
    }

    #[test]
    fn a_share_whose_bytes_stayed_unattributed_still_names_its_held_files() {
        let data = named(&[(261, b"photos")]);
        let mut share = analysed(261, &[50], b"photos");
        share.held_files = Some(crate::snapshots::HeldFiles {
            complete: true,
            count: 1,
            files: vec![crate::snapshots::HeldFile {
                snapshot: 1000,
                ino: 257,
                path: b"a.jpg".to_vec(),
                in_live: false,
                bytes: 4,
                unknown_bytes: 0,
            }],
        });
        let mut empty = analysed(262, &[51], b"backup");
        empty.held_files = Some(crate::snapshots::HeldFiles {
            complete: true,
            count: 0,
            files: Vec::new(),
        });
        let mut h = header(Vec::new());
        h.snapshots = Some(SnapshotAnalysis {
            depth: Depth::All,
            owner_budget: 64,
            global_owner_budget: 100,
            shares: vec![share, empty],
            holdings: Vec::new(),
            quota: None,
            stats: Default::default(),
        });

        let row = held_by_snapshots(&h, &data, 10);

        assert_eq!(row.items.len(), 1);
        assert_eq!((row.items[0].subvolume_id, row.items[0].bytes), (261, 0));
        assert_eq!(row.items[0].files.as_ref().unwrap().count, 1);
    }

    #[test]
    fn a_share_missing_from_the_subvolume_list_has_no_snapshot_count() {
        let mut data = named(&[]);
        data.snapshot_only_bytes = BTreeMap::from([(300, 4)]);

        let row = held_by_snapshots(&header(Vec::new()), &data, 10);

        assert_eq!(row.items[0].snapshots, None);
        assert_eq!(row.items[0].name, None);
    }

    #[test]
    fn recycle_bins_sum_their_shares_and_measure_recent_extent_bytes() {
        let mut data = named(&[(261, b"photos"), (262, b"backup")]);
        data.recycle_bin = BTreeMap::from([
            (
                261,
                OwnerBytes {
                    bytes: 3,
                    reclaimable_bytes: 2,
                },
            ),
            (
                262,
                OwnerBytes {
                    bytes: 5,
                    reclaimable_bytes: 5,
                },
            ),
        ]);
        let now = 1_790_000_000;
        data.recycle_ctime = Some(crate::overview::RecycleCtimeData {
            bytes: BTreeMap::from([
                (
                    261,
                    BTreeMap::from([
                        (
                            now - 86_399,
                            OwnerBytes {
                                bytes: 2,
                                reclaimable_bytes: 1,
                            },
                        ),
                        (
                            now - 86_401,
                            OwnerBytes {
                                bytes: 1,
                                reclaimable_bytes: 1,
                            },
                        ),
                    ]),
                ),
                (
                    262,
                    BTreeMap::from([(
                        now - 10,
                        OwnerBytes {
                            bytes: 5,
                            reclaimable_bytes: 5,
                        },
                    )]),
                ),
            ]),
            unmeasured_bytes: 0,
        });

        let row = recycle_bins(
            &data,
            Some(Duration::from_secs(86_400)),
            std::time::UNIX_EPOCH + Duration::from_secs(now),
        );

        assert_eq!((row.bytes, row.reclaimable_bytes), (8, 7));
        assert_eq!(
            (row.since_seconds, row.since_bytes),
            (Some(86_400), Some(7))
        );
        let ids: Vec<u64> = row.items.iter().map(|i| i.subvolume_id).collect();
        assert_eq!(ids, [262, 261]);
        assert_eq!(
            recycle_bins(&data, None, std::time::UNIX_EPOCH).since_seconds,
            None
        );
    }

    #[test]
    fn an_unmeasured_recycle_extent_keeps_since_bytes_unknown() {
        let mut data = named(&[(261, b"photos")]);
        data.recycle_bin = BTreeMap::from([(
            261,
            OwnerBytes {
                bytes: 5,
                reclaimable_bytes: 5,
            },
        )]);
        data.recycle_ctime = Some(crate::overview::RecycleCtimeData {
            bytes: BTreeMap::from([(
                261,
                BTreeMap::from([(
                    100,
                    OwnerBytes {
                        bytes: 3,
                        reclaimable_bytes: 3,
                    },
                )]),
            )]),
            unmeasured_bytes: 2,
        });

        let row = recycle_bins(
            &data,
            Some(Duration::from_secs(10)),
            std::time::UNIX_EPOCH + Duration::from_secs(100),
        );

        assert_eq!(row.since_bytes, None);
        assert!(Reason::RecycleBin(row).is_unknown());
    }

    #[test]
    fn unresolved_bytes_keep_their_reasons() {
        let mut data = named(&[]);
        data.unknown = BTreeMap::from([
            (UnknownReason::OwnerBudgetExhausted, 6),
            (UnknownReason::MissingOwner, 1),
        ]);

        let row = unresolved(&data);

        assert_eq!(row.bytes, 7);
        assert_eq!(
            row.reasons,
            [
                UnresolvedPart {
                    code: UnknownReason::OwnerBudgetExhausted,
                    bytes: 6
                },
                UnresolvedPart {
                    code: UnknownReason::MissingOwner,
                    bytes: 1
                },
            ]
        );
    }

    fn open(pid: u32, ino: u64, bytes: u64) -> OpenDeleted {
        OpenDeleted {
            pid,
            comm: format!("p{pid}").into_bytes(),
            dev: 40,
            ino,
            allocated_bytes: bytes,
        }
    }

    #[test]
    fn an_inode_two_processes_hold_counts_once_in_the_total() {
        let found = [
            open(7, 1, 10),
            open(7, 1, 10),
            open(7, 2, 5),
            open(9, 1, 10),
        ];

        let scan = process_scan(&found, 2);

        assert_eq!(scan.bytes_estimate, 15);
        assert_eq!(scan.unreadable_processes, 2);
        assert_eq!(
            scan.holders,
            [
                OpenHolder {
                    pid: 7,
                    comm: "p7".into(),
                    inodes: 2,
                    bytes_estimate: 15
                },
                OpenHolder {
                    pid: 9,
                    comm: "p9".into(),
                    inodes: 1,
                    bytes_estimate: 10
                },
            ]
        );
    }

    #[test]
    fn the_commit_interval_defaults_to_thirty_seconds() {
        let options = |text: &str| -> Vec<Vec<u8>> {
            text.split(',').map(|o| o.as_bytes().to_vec()).collect()
        };
        assert_eq!(
            commit_interval(&options("rw,relatime,space_cache,subvolid=5")),
            30
        );
        assert_eq!(commit_interval(&options("rw,commit=120,ssd")), 120);
        assert_eq!(commit_interval(&options("rw,commit=x")), 30);
    }

    fn probes() -> Probes {
        Probes {
            pending: Some(set(&[])),
            processes: None,
            commit_interval_s: 30,
        }
    }

    fn codes(reasons: &[Reason]) -> Vec<String> {
        reasons
            .iter()
            .map(|r| {
                serde_json::to_value(r).unwrap()["code"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn every_row_is_listed_in_order_and_a_clean_report_has_no_unknown() {
        let mut h = header(Vec::new());
        h.accounting.class_bytes[6] = 4;
        let rows = reasons(&h, &named(&[]), probes(), None, 10);

        assert_eq!(
            codes(&rows),
            [
                "pending_subvolume_cleanup",
                "held_by_snapshots",
                "recycle_bin",
                "open_unlinked",
                "hard_link",
                "commit_pending",
                "unresolved_references",
            ]
        );
        assert!(!rows.iter().any(Reason::is_unknown), "{rows:?}");
        assert_eq!(
            rows[3],
            Reason::OpenUnlinked(OpenUnlinked {
                report_orphan_bytes: 4,
                processes: None,
            })
        );
        assert_eq!(rows[4], Reason::HardLink { bytes: None });
        assert_eq!(rows[5], Reason::CommitPending { interval_s: 30 });
    }

    #[test]
    fn unmeasured_values_mark_their_rows_unknown() {
        let mut data = named(&[]);
        data.pending_roots = Some(vec![300]);
        data.unknown = BTreeMap::from([(UnknownReason::OwnerBudgetExhausted, 6)]);
        let mut live = probes();
        live.pending = Some(set(&[300, 301]));
        live.processes = Some(process_scan(&[], 1));
        let rows = reasons(
            &header(Vec::new()),
            &data,
            live,
            Some(Duration::from_secs(60)),
            10,
        );

        let unknown: Vec<String> = codes(&rows)
            .into_iter()
            .zip(&rows)
            .filter(|(_, r)| r.is_unknown())
            .map(|(code, _)| code)
            .collect();
        assert_eq!(
            unknown,
            [
                "pending_subvolume_cleanup",
                "open_unlinked",
                "unresolved_references",
            ]
        );
    }

    #[test]
    fn unresolved_bytes_make_the_attributed_rows_lower_bounds() {
        let lower_bounds = |rows: &[Reason]| -> Vec<bool> {
            rows.iter()
                .filter_map(|r| serde_json::to_value(r).unwrap()["lower_bound"].as_bool())
                .collect()
        };
        let clean = reasons(&header(Vec::new()), &named(&[]), probes(), None, 10);
        let mut data = named(&[]);
        data.unknown = BTreeMap::from([(UnknownReason::PinnedRangesUnknown, 6)]);
        let partial = reasons(&header(Vec::new()), &data, probes(), None, 10);

        assert_eq!(lower_bounds(&clean), [false; 3]);
        assert_eq!(lower_bounds(&partial), [true; 3]);
    }

    #[test]
    fn a_failed_pending_probe_is_unknown() {
        let mut live = probes();
        live.pending = None;
        let rows = reasons(&header(Vec::new()), &named(&[]), live, None, 10);

        assert!(rows[0].is_unknown());
    }

    #[test]
    fn since_marks_pending_roots_unknown_without_a_deletion_time() {
        let mut data = named(&[]);
        data.pending_roots = Some(vec![300]);
        data.pending_cleanup_bytes = 18;
        let mut live = probes();
        live.pending = Some(set(&[300]));

        let rows = reasons(
            &header(Vec::new()),
            &data,
            live.clone(),
            Some(Duration::from_secs(60)),
            10,
        );

        let Reason::PendingSubvolumeCleanup(pending) = &rows[0] else {
            panic!("wrong first row: {:?}", rows[0]);
        };
        assert_eq!((pending.count, pending.bytes), (Some(1), Some(18)));
        assert!(pending.since_not_measured);
        assert!(rows[0].is_unknown());

        let unfiltered = reasons(&header(Vec::new()), &data, live, None, 10);
        assert!(!unfiltered[0].is_unknown());
    }

    fn set(ids: &[u64]) -> BTreeSet<u64> {
        ids.iter().copied().collect()
    }

    #[test]
    fn pending_roots_unchanged_since_the_report_keep_its_bytes() {
        let row = pending_cleanup(Some(&[300, 301]), 18, Some(&set(&[300, 301])));

        assert_eq!(
            row,
            PendingCleanup {
                count: Some(2),
                bytes: Some(18),
                appeared_after_report: Some(0),
                cleaned_since_report: Some(0),
                lower_bound: false,
                since_not_measured: false,
            }
        );
    }

    #[test]
    fn a_root_deleted_after_the_report_has_no_measured_bytes() {
        let row = pending_cleanup(Some(&[300]), 18, Some(&set(&[300, 302])));

        assert_eq!((row.count, row.bytes), (Some(2), None));
        assert_eq!(row.appeared_after_report, Some(1));
    }

    #[test]
    fn a_partly_cleaned_pending_set_cannot_split_the_report_total() {
        let row = pending_cleanup(Some(&[300, 301]), 18, Some(&set(&[300])));

        assert_eq!((row.count, row.bytes), (Some(1), None));
        assert_eq!(row.cleaned_since_report, Some(1));
    }

    #[test]
    fn nothing_pending_now_holds_nothing() {
        let row = pending_cleanup(Some(&[300]), 18, Some(&set(&[])));

        assert_eq!((row.count, row.bytes), (Some(0), Some(0)));
        assert_eq!(row.cleaned_since_report, Some(1));
    }

    #[test]
    fn a_failed_probe_leaves_the_row_unknown() {
        let row = pending_cleanup(Some(&[300]), 18, None);

        assert_eq!((row.count, row.bytes), (None, None));
    }

    #[test]
    fn a_report_without_pending_roots_gives_counts_only() {
        let row = pending_cleanup(None, 18, Some(&set(&[300])));

        assert_eq!((row.count, row.bytes), (Some(1), None));
        assert_eq!(row.appeared_after_report, None);
    }
}
