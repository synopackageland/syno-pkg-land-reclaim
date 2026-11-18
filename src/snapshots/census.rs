//! The snapshot census (SPEC 5.14 steps 1-3): per share, which of the
//! roots read reference each extent a snapshot references, joined with
//! the extent tree to tell whether a root that was not read may too.

use super::*;
use crate::btrfs::backref::DataBackref;
use crate::btrfs::extent_refs::{ExtentRefs, Plan, ranges, resolve_each, resolve_in};
use crate::btrfs::keys::{EXTENT_DATA_KEY, INODE_ITEM_KEY, Key};
use crate::btrfs::owners::OwnerWalk;
use crate::btrfs::parser::{ExtentKind, parse_file_extent};
use crate::btrfs::qgroup::read_qgroups;
use crate::btrfs::search::SearchRange;
use crate::btrfs::source::MetadataSource;
use crate::btrfs::subvolume::{FullPathResolver, Subvolume, SubvolumeSet};
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_batches};
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::Path;

mod checkpoint;
mod spill;

/// Extent-tree searches of nearby candidates are merged up to this gap.
const COALESCE_GAP: u64 = 16 << 20;
/// Candidates joined with the extent tree at a time.
const JOIN_CHUNK: usize = 1 << 14;
/// Model bytes one census entry costs besides its owner words.
const ENTRY_BYTES: u64 = 32;
/// Model bytes one owner-histogram bucket costs besides its owner ids.
const HISTOGRAM_ENTRY_BYTES: u64 = 80;
/// Model bytes one tallied held file costs.
const HELD_FILE_BYTES: u64 = 64;

#[cfg(test)]
thread_local! { static CENSUS_PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
fn observe_census_memory(bytes: usize) {
    CENSUS_PEAK.with(|peak| peak.set(peak.get().max(bytes)));
}

/// A snapshot file's byte range within a data extent.
#[derive(Debug, Clone, Copy)]
struct ExtentUse {
    /// Census index of the root and the inode that reference it.
    root: usize,
    ino: u64,
    bytenr: u64,
    disk_num_bytes: u64,
    start: u64,
    end: Option<u64>,
    mappable: bool,
}

#[derive(Debug, Clone, Copy)]
struct HistogramValue {
    bytes: u64,
    extents: u64,
    pinned_unreferenced_bytes: Option<u64>,
}

impl ExtentUse {
    fn new(
        (root, ino): (usize, u64),
        disk: crate::btrfs::parser::DiskExtent,
        mappable: bool,
    ) -> Self {
        ExtentUse {
            root,
            ino,
            bytenr: disk.disk_bytenr,
            disk_num_bytes: disk.disk_num_bytes,
            start: disk.offset,
            end: disk.offset.checked_add(disk.num_bytes),
            mappable: mappable && disk.num_bytes > 0,
        }
    }
}

/// Unreferenced bytes in a physical extent, if every file range maps to it.
fn pinned_unreferenced(ranges: &[ExtentUse], bytenr: u64, allocated: u64) -> Option<u64> {
    let uses = uses_of(ranges, bytenr);
    if uses.is_empty() {
        return None;
    }

    for usage in uses {
        let end = usage.end?;
        if !usage.mappable || usage.disk_num_bytes != allocated || end > allocated {
            return None;
        }
    }
    crate::model::extent::unreferenced_range_bytes_sorted(
        uses.iter()
            .map(|usage| (usage.start, usage.end.expect("range was validated"))),
        allocated,
    )
}

/// The file ranges of extent `bytenr` in `ranges`, sorted by bytenr.
fn uses_of(ranges: &[ExtentUse], bytenr: u64) -> &[ExtentUse] {
    let first = ranges.partition_point(|range| range.bytenr < bytenr);
    let last = ranges.partition_point(|range| range.bytenr <= bytenr);
    &ranges[first..last]
}

/// Snapshot-only bytes of one file.
#[derive(Debug, Clone, Copy)]
struct FileTotals {
    /// Census index of the newest root naming it.
    root: usize,
    bytes: u64,
    unknown: u64,
}

/// Snapshot-only bytes per file, for at most `budget` files.
struct FileTally {
    budget: u64,
    complete: bool,
    files: HashMap<u64, FileTotals>,
}

impl FileTally {
    fn new(budget: u64) -> Self {
        FileTally {
            budget,
            complete: true,
            files: HashMap::new(),
        }
    }

    /// Adds an extent only snapshots reference to each file of `uses`.
    fn add(
        &mut self,
        uses: &[ExtentUse],
        hold: Hold,
        bytes: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(), WalkError> {
        let mut inodes: Vec<(u64, usize)> = uses.iter().map(|u| (u.ino, u.root)).collect();
        // Sorted, the first root of each inode is its newest.
        inodes.sort_unstable();
        inodes.dedup_by_key(|&mut (ino, _)| ino);
        for (ino, root) in inodes {
            let tallied = self.files.len() as u64;
            let totals = match self.files.entry(ino) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) if tallied < self.budget => {
                    reserve(boundary, HELD_FILE_BYTES)?;
                    entry.insert(FileTotals {
                        root,
                        bytes: 0,
                        unknown: 0,
                    })
                }
                std::collections::hash_map::Entry::Vacant(_) => {
                    self.complete = false;
                    continue;
                }
            };
            totals.root = totals.root.min(root);
            match hold {
                Hold::Resolved => totals.bytes += bytes,
                _ => totals.unknown += bytes,
            }
        }
        Ok(())
    }
}

/// A selected live inode's reference to a data extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveUse {
    pub bytenr: u64,
    pub tree: u64,
    pub ino: u64,
}

/// A mixed selection: snapshot roots deleted whole, and the extent
/// references of the live inodes that lose every name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deletion {
    pub snapshots: BTreeSet<u64>,
    pub uses: Vec<LiveUse>,
}

/// What deleting `deletion` frees. Every live tree it touches and the
/// snapshots of their shares that `scan` reaches are read, so an extent
/// counts as freed only if every reference to it is deleted.
pub fn freed_by_deleting<S: MetadataSource + ?Sized>(
    source: &mut S,
    set: &SubvolumeSet,
    deletion: &Deletion,
    scan: &SnapshotScan,
    boundary: &mut dyn BatchBoundary,
) -> Result<DeletionAnswer, WalkError> {
    let mut stats = AnalysisStats::default();
    let mut walk = OwnerWalk::with_global_budget(scan.owner_budget, scan.global_owner_budget);
    let plan = DeletionPlan::new(set, deletion, scan.depth);
    let mut pass = Pass {
        source,
        set,
        walk: &mut walk,
        stats: &mut stats,
        boundary,
        spill: None,
    };
    let tally = pass.delete(&plan, deletion)?;
    stats.owner_lookups = walk.lookups;
    Ok(DeletionAnswer {
        snapshots: deletion.snapshots.iter().copied().collect(),
        freed_bytes: tally.freed.bytes,
        unknown_bytes: tally.freed.unknown,
        unknown_extents: tally.unknown_extents,
        snapshots_not_materialized: plan.skipped.len() as u64,
        stats,
    })
}

/// A census root, and whether the deletion removes what it references.
#[derive(Debug, Clone, Copy)]
struct CensusRoot {
    id: u64,
    deleted: bool,
}

/// The roots deleting a mixed selection reads.
#[derive(Debug, Default)]
struct DeletionPlan {
    roots: Vec<CensusRoot>,
    /// Census indexes of each live tree's selected and remaining inodes.
    lives: BTreeMap<u64, (usize, usize)>,
    /// Census indexes of the snapshots read, deleted ones first.
    snapshots: Vec<usize>,
    /// Snapshots of the lives' shares left unread.
    skipped: BTreeSet<u64>,
}

impl DeletionPlan {
    fn new(set: &SubvolumeSet, deletion: &Deletion, depth: Depth) -> Self {
        let mut lives: BTreeSet<u64> = deletion.uses.iter().map(|u| u.tree).collect();
        for live in set.subvolumes.iter().filter(|s| !s.readonly) {
            let of = set.share_snapshots(live.id);
            if of
                .snapshots
                .iter()
                .any(|id| deletion.snapshots.contains(id))
            {
                lives.insert(live.id);
            }
        }
        let mut plan = DeletionPlan::default();
        let mut read = BTreeSet::new();
        for &live in &lives {
            let selected = plan.push(live, true);
            let rest = plan.push(live, false);
            plan.lives.insert(live, (selected, rest));
            let snapshots = newest_first(set, &set.share_snapshots(live).snapshots);
            let newest = depth.take(snapshots.len());
            for (i, snapshot) in snapshots.iter().enumerate() {
                let deleted = deletion.snapshots.contains(&snapshot.id);
                if deleted || i < newest {
                    read.insert(snapshot.id);
                } else {
                    plan.skipped.insert(snapshot.id);
                }
            }
        }
        read.extend(deletion.snapshots.iter().copied());
        plan.skipped.retain(|id| !read.contains(id));
        let (deleted, kept): (Vec<u64>, Vec<u64>) = read
            .into_iter()
            .partition(|id| deletion.snapshots.contains(id));
        for id in deleted {
            let index = plan.push(id, true);
            plan.snapshots.push(index);
        }
        for id in kept {
            let index = plan.push(id, false);
            plan.snapshots.push(index);
        }
        plan
    }

    fn push(&mut self, id: u64, deleted: bool) -> usize {
        self.roots.push(CensusRoot { id, deleted });
        self.roots.len() - 1
    }

    /// Every tree read, once.
    fn read_ids(&self) -> Vec<u64> {
        let ids: BTreeSet<u64> = self.roots.iter().map(|r| r.id).collect();
        ids.into_iter().collect()
    }
}

/// What a deletion census counted.
#[derive(Debug, Default)]
struct DeletionTally {
    freed: Freed,
    unknown_extents: u64,
}

/// What to analyze.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Live subvolumes whose snapshots are analyzed.
    pub lives: Vec<u64>,
    pub depth: Depth,
    pub owner_budget: u32,
    pub global_owner_budget: u64,
    /// Files with snapshot-only data tallied per share at most.
    pub held_files_budget: u64,
}

/// Reads the snapshots of every live subvolume in `request`.
pub fn analyze<S: MetadataSource + ?Sized>(
    source: &mut S,
    set: &SubvolumeSet,
    request: &Request,
    boundary: &mut dyn BatchBoundary,
) -> Result<SnapshotAnalysis, WalkError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let scratch = crate::spill::Spill::create(std::env::temp_dir().join(format!(
        "synoreclaim-snapshots-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )))?;
    analyze_in(source, set, request, boundary, scratch.dir())
}

/// The complete snapshot inventory, without reading any snapshot or live file
/// extent tree. Kept even when the expensive census is deferred to another job.
pub fn inventory<S: MetadataSource + ?Sized>(
    source: &mut S,
    set: &SubvolumeSet,
    lives: &[u64],
    boundary: &mut dyn BatchBoundary,
) -> Result<SnapshotAnalysis, WalkError> {
    let mut shares = Vec::new();
    for &live in lives {
        let share = plan_share(source, set, live, Depth::Newest(0), boundary)?;
        if !share.snapshots.is_empty() || !share.pending.is_empty() {
            shares.push(share);
        }
    }
    Ok(SnapshotAnalysis {
        depth: Depth::Newest(0),
        owner_budget: DEFAULT_OWNER_BUDGET,
        global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
        shares,
        holdings: Vec::new(),
        quota: None,
        stats: AnalysisStats::default(),
    })
}

/// Uses the scan's private spill filesystem, including its free-space checks.
pub fn analyze_in<S: MetadataSource + ?Sized>(
    source: &mut S,
    set: &SubvolumeSet,
    request: &Request,
    boundary: &mut dyn BatchBoundary,
    dir: &Path,
) -> Result<SnapshotAnalysis, WalkError> {
    analyze_checkpointed(source, set, request, boundary, dir, None)
}

/// Completed shares survive pause/resume; scratch sorting stays bounded and disposable.
pub fn analyze_checkpointed<S: MetadataSource + ?Sized>(
    source: &mut S,
    set: &SubvolumeSet,
    request: &Request,
    boundary: &mut dyn BatchBoundary,
    dir: &Path,
    checkpoint: Option<&Path>,
) -> Result<SnapshotAnalysis, WalkError> {
    if let Some(path) = checkpoint {
        std::fs::create_dir_all(path)?;
    }
    let mut stats = AnalysisStats::default();
    let mut shares = Vec::new();
    let mut holdings = Vec::new();
    for &live in &request.lives {
        let mut share = plan_share(source, set, live, request.depth, boundary)?;
        if share.snapshots.is_empty() && share.pending.is_empty() {
            continue;
        }
        let planned = share.clone();
        let cached = checkpoint
            .map(|path| checkpoint::load(path, set, &share, request, boundary))
            .transpose()?
            .flatten();
        let (share_holdings, share_stats) = if let Some((saved, holdings, stats)) = cached {
            share = saved;
            (holdings, stats)
        } else {
            let mut share_stats = AnalysisStats::default();
            let mut walk = OwnerWalk::with_global_budget(
                request.owner_budget,
                request
                    .global_owner_budget
                    .saturating_sub(stats.owner_lookups),
            );
            let mut pass = Pass {
                source: &mut *source,
                set,
                walk: &mut walk,
                stats: &mut share_stats,
                boundary: &mut *boundary,
                spill: Some(dir),
            };
            let held = match pass.run(&share, request.held_files_budget) {
                Ok((held, files)) => {
                    share.held_files = Some(files);
                    held
                }
                Err(error) if vanished(&error) => {
                    for snapshot in &mut share.snapshots {
                        snapshot.materialized = false;
                    }
                    Vec::new()
                }
                Err(error) => return Err(error),
            };
            share_stats.owner_lookups = walk.lookups;
            if let Some(path) = checkpoint {
                checkpoint::save(path, set, &planned, request, &share, &held, &share_stats)?;
            }
            (held, share_stats)
        };
        stats.trees_read += share_stats.trees_read;
        stats.items += share_stats.items;
        stats.extents += share_stats.extents;
        stats.owner_lookups += share_stats.owner_lookups;
        stats.owner_unresolved_extents += share_stats.owner_unresolved_extents;
        holdings.extend(share_holdings);
        shares.push(share);
    }
    holdings.sort_by(|a, b| (&a.owners, a.hold).cmp(&(&b.owners, b.hold)));
    let quota = match set.quota_enabled {
        true => Some(quota_check(source, &shares, boundary)?),
        false => None,
    };
    Ok(SnapshotAnalysis {
        depth: request.depth,
        owner_budget: request.owner_budget,
        global_owner_budget: request.global_owner_budget,
        shares,
        holdings,
        quota,
        stats,
    })
}

/// The snapshots of `live`, newest first, the first `depth` of them
/// marked for reading.
fn plan_share<S: MetadataSource + ?Sized>(
    source: &mut S,
    set: &SubvolumeSet,
    live: u64,
    depth: Depth,
    boundary: &mut dyn BatchBoundary,
) -> Result<ShareAnalysis, WalkError> {
    let of = set.share_snapshots(live);
    let snapshots = newest_first(set, &of.snapshots);
    let read = depth.take(snapshots.len());
    let subvolume_path = set
        .full_subvolume_path(source, live, boundary)
        .or_else(|error| {
            if vanished(&error) {
                Ok(None)
            } else {
                Err(error)
            }
        })?
        .unwrap_or_else(|| set.subvolume_path(live));
    let mut snapshot_info = Vec::with_capacity(snapshots.len());
    for (i, snapshot) in snapshots.iter().enumerate() {
        let subvolume_path = set
            .full_subvolume_path(source, snapshot.id, boundary)
            .or_else(|error| {
                if vanished(&error) {
                    Ok(None)
                } else {
                    Err(error)
                }
            })?
            .unwrap_or_else(|| set.subvolume_path(snapshot.id));
        snapshot_info.push(SnapshotInfo {
            id: snapshot.id,
            name: snapshot.name.clone(),
            subvolume_path,
            otime: snapshot.otime,
            generation: snapshot.generation,
            materialized: i < read,
        });
    }
    Ok(ShareAnalysis {
        live,
        name: set.get(live).map(|s| s.name.clone()).unwrap_or_default(),
        subvolume_path,
        snapshots: snapshot_info,
        pending: of.pending,
        held_files: None,
    })
}

fn vanished(error: &WalkError) -> bool {
    matches!(error, WalkError::Source(crate::btrfs::source::SourceError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound)
}

/// The subvolumes `ids`, newest first by creation time.
fn newest_first<'a>(set: &'a SubvolumeSet, ids: &[u64]) -> Vec<&'a Subvolume> {
    let mut snapshots: Vec<&Subvolume> = ids.iter().filter_map(|&id| set.get(id)).collect();
    snapshots.sort_by_key(|s| std::cmp::Reverse((s.otime.unwrap_or(0), s.id)));
    snapshots
}

fn quota_check<S: MetadataSource + ?Sized>(
    source: &mut S,
    shares: &[ShareAnalysis],
    boundary: &mut dyn BatchBoundary,
) -> Result<QuotaCheck, WalkError> {
    let qgroups = read_qgroups(source, boundary)?;
    let listed: BTreeSet<u64> = shares
        .iter()
        .flat_map(|s| s.snapshots.iter().map(|x| x.id).chain([s.live]))
        .collect();
    Ok(QuotaCheck {
        consistent: qgroups.consistent(),
        exclusive: qgroups
            .exclusive
            .into_iter()
            .filter(|(id, _)| listed.contains(id))
            .collect(),
    })
}

/// Which read roots reference each extent: one bit per root, in a flat
/// arena of `stride` words per extent.
struct Census {
    slots: HashMap<u64, u32>,
    words: Vec<u64>,
    ranges: Vec<ExtentUse>,
    stride: usize,
}

impl Census {
    fn new(roots: usize) -> Self {
        Census {
            slots: HashMap::new(),
            words: Vec::new(),
            ranges: Vec::new(),
            stride: roots.div_ceil(64).max(1),
        }
    }

    fn entry_bytes(&self) -> u64 {
        ENTRY_BYTES + 8 * self.stride as u64 + std::mem::size_of::<ExtentUse>() as u64
    }

    /// Records that root `index` references `bytenr`, adding the extent
    /// when `add` is set.
    fn mark(&mut self, bytenr: u64, index: usize, add: bool) {
        let slot = match self.slots.get(&bytenr) {
            Some(&slot) => slot,
            None if add => {
                let slot = (self.words.len() / self.stride) as u32;
                self.words.resize(self.words.len() + self.stride, 0);
                self.slots.insert(bytenr, slot);
                slot
            }
            None => return,
        };
        self.words[slot as usize * self.stride + index / 64] |= 1 << (index % 64);
    }

    fn owners(&self, slot: u32) -> &[u64] {
        let start = slot as usize * self.stride;
        &self.words[start..start + self.stride]
    }

    /// Extents by bytenr with their slots.
    fn sorted(&mut self) -> Vec<(u64, u32)> {
        let mut out: Vec<(u64, u32)> = self.slots.drain().collect();
        out.sort_unstable();
        out
    }
}

/// Reads census roots and classifies what they reference.
struct Pass<'a, S: ?Sized> {
    source: &'a mut S,
    set: &'a SubvolumeSet,
    walk: &'a mut OwnerWalk,
    stats: &'a mut AnalysisStats,
    boundary: &'a mut dyn BatchBoundary,
    spill: Option<&'a Path>,
}

impl<S: MetadataSource + ?Sized> Pass<'_, S> {
    /// The owner histogram of `share`, and the files only its snapshots
    /// hold, tallying at most `held_files_budget` of them.
    fn run(
        &mut self,
        share: &ShareAnalysis,
        held_files_budget: u64,
    ) -> Result<(Vec<Holding>, HeldFiles), WalkError> {
        // Snapshots first, then the live share, which only marks extents
        // a snapshot references.
        let mut roots: Vec<u64> = share.materialized().map(|s| s.id).collect();
        if roots.is_empty() {
            return Ok((Vec::new(), HeldFiles::default()));
        }
        roots.push(share.live);
        let skipped: BTreeSet<u64> = share.snapshots.iter().map(|s| s.id).collect();
        let mut sorter = spill::Sorter::new(self.spill.expect("snapshot spill directory"));
        for (index, &root) in roots.iter().enumerate() {
            let add = root != share.live;
            let range = SearchRange::new(root, Key::MIN, Key::MAX);
            let walk = walk_batches(self.source, range, self.boundary, |items, boundary| {
                for item in items.iter().filter(|i| i.key.item_type == EXTENT_DATA_KEY) {
                    let extent =
                        parse_file_extent(item.data).map_err(|source| WalkError::Item {
                            key: item.key,
                            source,
                        })?;
                    if let ExtentKind::Regular(disk) | ExtentKind::Prealloc(disk) = extent.kind
                        && disk.disk_bytenr != 0
                    {
                        sorter.push(
                            ExtentUse::new(
                                (index, item.key.objectid),
                                disk,
                                extent.compression == 0
                                    && extent.encryption == 0
                                    && extent.other_encoding == 0,
                            ),
                            add,
                            boundary,
                        )?;
                    }
                }
                Ok(())
            })?;
            self.stats.trees_read += 1;
            self.stats.items += walk.items;
        }
        let mut sorted = sorter.finish(self.boundary)?;
        let mut histogram: HashMap<(Vec<u64>, Hold), HistogramValue> = HashMap::new();
        let mut tally = FileTally::new(held_files_budget);
        loop {
            let chunk = sorted.chunk(self.boundary)?;
            if chunk.is_empty() {
                break;
            }
            self.stats.extents += chunk.len() as u64;
            let candidates: Vec<(u64, u32)> = chunk
                .iter()
                .enumerate()
                .map(|(i, group)| (group.bytenr, i as u32))
                .collect();
            let covered = ranges(
                candidates.iter().map(|&(b, _)| b),
                Plan::Coalesce {
                    max_gap: COALESCE_GAP,
                },
            );
            let set = self.set;
            let walk = &mut *self.walk;
            let stats = &mut *self.stats;
            let spill = self.spill;
            resolve_each(
                &mut *self.source,
                &covered,
                candidates.into_iter(),
                |&(b, _)| b,
                &mut *self.boundary,
                |source, (bytenr, slot), refs, boundary| {
                    let mut pass = Pass {
                        source,
                        set,
                        walk,
                        stats,
                        boundary,
                        spill,
                    };
                    #[cfg(test)]
                    observe_census_memory(refs.map_or(0, |r| {
                        r.backrefs.capacity() * std::mem::size_of::<DataBackref>()
                    }));
                    let group = &chunk[slot as usize];
                    let owners: Vec<u64> = group.roots.iter().map(|&index| roots[index]).collect();
                    let (hold, bytes) = match &refs {
                        Some(refs) if !refs.malformed => {
                            let hold = pass.classify(&roots, &skipped, &owners, refs)?;
                            (hold, refs.num_bytes)
                        }
                        _ => (Hold::Unknown, 0),
                    };
                    let pinned =
                        if hold == Hold::Resolved && owners.len() == 1 && owners[0] != share.live {
                            refs.as_ref().and_then(|extent| {
                                pinned_unreferenced(&group.uses, bytenr, extent.num_bytes)
                            })
                        } else {
                            None
                        };
                    if hold != Hold::External && !owners.contains(&share.live) {
                        tally.add(&group.uses, hold, bytes, pass.boundary)?;
                    }
                    match histogram.entry((owners, hold)) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            let totals = entry.get_mut();
                            totals.bytes += bytes;
                            totals.extents += 1;
                            totals.pinned_unreferenced_bytes =
                                match (totals.pinned_unreferenced_bytes, pinned) {
                                    (Some(total), Some(value)) => total.checked_add(value),
                                    _ => None,
                                };
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let growth = HISTOGRAM_ENTRY_BYTES
                                + entry.key().0.len() as u64 * std::mem::size_of::<u64>() as u64;
                            reserve(pass.boundary, growth)?;
                            entry.insert(HistogramValue {
                                bytes,
                                extents: 1,
                                pinned_unreferenced_bytes: pinned,
                            });
                        }
                    }
                    Ok(())
                },
            )?;
        }
        let mut holdings = Vec::new();
        reserve(
            self.boundary,
            histogram.len() as u64 * std::mem::size_of::<Holding>() as u64,
        )?;
        holdings
            .try_reserve_exact(histogram.len())
            .map_err(|_| WalkError::Stopped(crate::btrfs::walk::StopReason::ResourceLimit))?;
        for ((mut owners, hold), totals) in histogram {
            owners.sort_unstable();
            holdings.push(Holding {
                owners,
                hold,
                bytes: totals.bytes,
                extents: totals.extents,
                pinned_unreferenced_bytes: totals.pinned_unreferenced_bytes,
            });
        }
        holdings.sort_by(|a, b| (&a.owners, a.hold).cmp(&(&b.owners, b.hold)));
        let held_files = self.held_files(tally, &roots, share.live)?;
        Ok((holdings, held_files))
    }

    /// The largest tallied files, named in their newest snapshot.
    fn held_files(
        &mut self,
        tally: FileTally,
        roots: &[u64],
        live: u64,
    ) -> Result<HeldFiles, WalkError> {
        let complete = tally.complete;
        let count = tally.files.len() as u64;
        let mut largest: Vec<(u64, FileTotals)> = tally.files.into_iter().collect();
        largest.sort_unstable_by_key(|&(ino, t)| {
            (
                std::cmp::Reverse(t.bytes),
                std::cmp::Reverse(t.unknown),
                ino,
            )
        });
        largest.truncate(HELD_FILES_KEPT);
        let mut paths = FullPathResolver::default();
        let mut files = Vec::with_capacity(largest.len());
        for (ino, totals) in largest {
            let snapshot = roots[totals.root];
            let path = paths.inode_path(self.source, snapshot, ino, self.boundary)?;
            files.push(HeldFile {
                snapshot,
                ino,
                path: path.unwrap_or_default(),
                in_live: self.has_inode(live, ino)?,
                bytes: totals.bytes,
                unknown_bytes: totals.unknown,
            });
        }
        Ok(HeldFiles {
            complete,
            count,
            files,
        })
    }

    /// Whether tree `root` has inode `ino`.
    fn has_inode(&mut self, root: u64, ino: u64) -> Result<bool, WalkError> {
        let range = SearchRange::new(
            root,
            Key::new(ino, INODE_ITEM_KEY, 0),
            Key::new(ino, INODE_ITEM_KEY, u64::MAX),
        );
        let walk = walk_batches(self.source, range, self.boundary, |_, _| Ok(()))?;
        Ok(walk.items > 0)
    }

    /// Counts the extents only deleted roots reference: live uses first,
    /// then deleted snapshots, then the roots that only mark.
    fn delete(
        &mut self,
        plan: &DeletionPlan,
        deletion: &Deletion,
    ) -> Result<DeletionTally, WalkError> {
        let mut census = Census::new(plan.roots.len());
        reserve(
            self.boundary,
            deletion.uses.len() as u64 * census.entry_bytes(),
        )?;
        for u in &deletion.uses {
            census.mark(u.bytenr, plan.lives[&u.tree].0, true);
        }
        for &index in &plan.snapshots {
            let root = plan.roots[index];
            let pick = |_| Some((index, root.deleted));
            self.read_tree(&mut census, root.id, root.deleted, pick)?;
        }
        let inodes: BTreeSet<(u64, u64)> = deletion.uses.iter().map(|u| (u.tree, u.ino)).collect();
        for (&live, &(_, rest)) in &plan.lives {
            let pick = |ino| (!inodes.contains(&(live, ino))).then_some((rest, false));
            self.read_tree(&mut census, live, false, pick)?;
        }
        let read = plan.read_ids();
        let extents = census.sorted();
        self.stats.extents += extents.len() as u64;
        let mut tally = DeletionTally::default();
        for chunk in extents.chunks(JOIN_CHUNK) {
            for (_, slot, refs) in self.join(chunk)? {
                let owners: Vec<CensusRoot> = bits(census.owners(slot))
                    .map(|index| plan.roots[index])
                    .collect();
                if !owners.iter().all(|r| r.deleted) {
                    continue;
                }
                let ids: Vec<u64> = owners.iter().map(|r| r.id).collect();
                let hold = match &refs {
                    Some(refs) if !refs.malformed => {
                        self.classify(&read, &plan.skipped, &ids, refs)?
                    }
                    _ => Hold::Unknown,
                };
                let bytes = refs.as_ref().map_or(0, |r| r.num_bytes);
                match hold {
                    Hold::Resolved => tally.freed.bytes += bytes,
                    Hold::Unknown => {
                        tally.freed.unknown += bytes;
                        tally.unknown_extents += 1;
                    }
                    Hold::External => {}
                }
            }
        }
        Ok(tally)
    }

    /// Marks every data extent tree `root` references for the census
    /// root `pick` gives the item's inode, adding the extent when it says
    /// so; `adds` tells whether it ever does.
    fn read_tree(
        &mut self,
        census: &mut Census,
        root: u64,
        adds: bool,
        pick: impl Fn(u64) -> Option<(usize, bool)>,
    ) -> Result<(), WalkError> {
        let range = SearchRange::new(root, Key::MIN, Key::MAX);
        let entry_bytes = census.entry_bytes();
        let walk = walk_batches(self.source, range, self.boundary, |items, boundary| {
            if adds
                && let ControlFlow::Break(reason) =
                    boundary.reserve(items.len() as u64 * entry_bytes)
            {
                return Err(WalkError::Stopped(reason));
            }
            for item in items.iter().filter(|i| i.key.item_type == EXTENT_DATA_KEY) {
                let extent = parse_file_extent(item.data).map_err(|source| WalkError::Item {
                    key: item.key,
                    source,
                })?;
                if let ExtentKind::Regular(disk) | ExtentKind::Prealloc(disk) = extent.kind
                    && disk.disk_bytenr != 0
                    && let Some((index, add)) = pick(item.key.objectid)
                {
                    census.mark(disk.disk_bytenr, index, add);
                    if add {
                        census.ranges.push(ExtentUse::new(
                            (index, item.key.objectid),
                            disk,
                            extent.compression == 0
                                && extent.encryption == 0
                                && extent.other_encoding == 0,
                        ));
                    }
                }
            }
            Ok(())
        })?;
        self.stats.trees_read += 1;
        self.stats.items += walk.items;
        Ok(())
    }

    /// The extent-tree references of each extent in `chunk`.
    fn join(
        &mut self,
        chunk: &[(u64, u32)],
    ) -> Result<Vec<(u64, u32, Option<ExtentRefs>)>, WalkError> {
        let covered = ranges(
            chunk.iter().map(|&(b, _)| b),
            Plan::Coalesce {
                max_gap: COALESCE_GAP,
            },
        );
        let mut out = Vec::with_capacity(chunk.len());
        resolve_in(
            self.source,
            &covered,
            chunk.iter().copied(),
            |&(bytenr, _)| bytenr,
            self.boundary,
            |(bytenr, slot), refs| out.push((bytenr, slot, refs.cloned())),
        )?;
        #[cfg(test)]
        observe_census_memory(
            out.iter()
                .map(|(_, _, refs)| {
                    refs.as_ref().map_or(0, |r| {
                        r.backrefs.capacity() * std::mem::size_of::<DataBackref>()
                    })
                })
                .sum::<usize>()
                + out.capacity() * std::mem::size_of::<(u64, u32, Option<ExtentRefs>)>(),
        );
        Ok(out)
    }

    /// Whether a root other than the `read` ones references an extent
    /// whose read owners are `owners`; `skipped` are known snapshots
    /// left unread.
    fn classify(
        &mut self,
        read: &[u64],
        skipped: &BTreeSet<u64>,
        owners: &[u64],
        refs: &ExtentRefs,
    ) -> Result<Hold, WalkError> {
        let mut named = BTreeSet::new();
        let mut parents = Vec::new();
        for backref in &refs.backrefs {
            match *backref {
                DataBackref::Direct { root, .. } => {
                    named.insert(root);
                }
                DataBackref::Shared { parent, .. } => parents.push(parent),
            }
        }
        if !parents.is_empty() {
            match self.walk.anchors(self.source, &parents, self.boundary)? {
                Some(anchors) => named.extend(anchors),
                None => {
                    self.stats.owner_unresolved_extents += 1;
                    return Ok(Hold::Unknown);
                }
            }
        }
        let set = self.set;
        if named
            .iter()
            .any(|&r| set.get(r).is_none() || set.relocating.contains(&r))
        {
            return Ok(Hold::Unknown);
        }
        let universe: BTreeSet<u64> = named.iter().flat_map(|&r| set.sharing_family(r)).collect();
        if owners.iter().any(|o| !universe.contains(o)) {
            return Ok(Hold::Unknown);
        }
        let mut hold = Hold::Resolved;
        for root in universe.iter().filter_map(|&r| set.get(r)) {
            if read.contains(&root.id) || root.pending_deletion {
                continue;
            }
            let skipped = skipped.contains(&root.id);
            match named.contains(&root.id) {
                true if !skipped => return Ok(Hold::External),
                true => hold = Hold::Unknown,
                false if root.generation >= refs.generation => hold = Hold::Unknown,
                false => {}
            }
        }
        Ok(hold)
    }
}

fn reserve(boundary: &mut dyn BatchBoundary, bytes: u64) -> Result<(), WalkError> {
    match boundary.reserve(bytes) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(reason) => Err(WalkError::Stopped(reason)),
    }
}

/// Indexes of the set bits of `words`.
fn bits(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
    words.iter().enumerate().flat_map(|(w, &word)| {
        (0..64)
            .filter(move |b| word & (1 << b) != 0)
            .map(move |b| w * 64 + b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::backref::testing::{encode_data_extent, encode_tree_block};
    use crate::btrfs::backref::{
        BlockBackref, EXTENT_ITEM_KEY, EXTENT_TREE_OBJECTID, METADATA_ITEM_KEY,
    };
    use crate::btrfs::keys::QUOTA_TREE_OBJECTID;
    use crate::btrfs::qgroup::{QGROUP_INFO_KEY, QGROUP_STATUS_FLAG_ON, QGROUP_STATUS_KEY};
    use crate::btrfs::subvolume::Link;
    use crate::btrfs::walk::StopReason;
    use crate::btrfs::walk::testing::{FakeTrees, NoStop};
    use crate::model::testing::{TreeItem, whole_extent};

    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const LIVE: u64 = 256;
    const OLD: u64 = 300;
    const NEW: u64 = 301;
    const OTHER: u64 = 400;

    #[test]
    fn inventory_lists_every_snapshot_without_reading_file_extents() {
        let mut source = FakeTrees::new([(OLD, vec![file(257, MIB)])]);
        let analysis = inventory(&mut source, &set(), &[LIVE], &mut NoStop).unwrap();
        assert_eq!(analysis.shares[0].snapshots.len(), 2);
        assert!(analysis.shares[0].snapshots.iter().all(|s| !s.materialized));
        assert_eq!(analysis.shares[0].held_files, None);
        assert!(analysis.holdings.is_empty());
        assert_eq!(analysis.stats, AnalysisStats::default());
    }

    #[test]
    fn snapshot_join_does_not_retain_a_chunk_of_many_owner_backrefs() {
        let files: Vec<TreeItem> = (0..512).map(|i| file(257 + i, (i + 1) * MIB)).collect();
        // Only the newest snapshot is materialized. Every extent also has
        // many inode references in the older snapshot, outside the chunk's
        // use/owner count but present in its extent-tree backref vector.
        let mut backrefs: Vec<DataBackref> = (0..500)
            .map(|i| DataBackref::Direct {
                root: OLD,
                objectid: 1000 + i,
                offset: 0,
                count: 1,
            })
            .collect();
        backrefs.push(direct(NEW));
        let trees = vec![
            (NEW, files),
            (LIVE, Vec::new()),
            (
                EXTENT_TREE_OBJECTID,
                (0..512)
                    .map(|i| extent((i + 1) * MIB, 60, &backrefs))
                    .collect(),
            ),
        ];
        CENSUS_PEAK.with(|p| p.set(0));
        let result = run(&set(), trees, Depth::Newest(1));
        assert_eq!(result.holdings, vec![holding(&[NEW], Hold::Unknown, 512)]);
        let peak = CENSUS_PEAK.with(|p| p.get());
        eprintln!("snapshot many-owner join retained model: {peak} bytes");
        assert!(peak < 4 << 20, "join retained {peak} bytes of backrefs");
    }

    #[test]
    fn pausing_during_the_next_share_keeps_the_completed_share() {
        use std::cell::Cell;
        use std::rc::Rc;
        struct PauseSecond {
            source: FakeTrees,
            flag: Rc<Cell<bool>>,
        }
        impl MetadataSource for PauseSecond {
            fn search(
                &mut self,
                range: &SearchRange,
            ) -> Result<crate::btrfs::source::RawBatch<'_>, crate::btrfs::source::SourceError>
            {
                if range.tree_id == 900 {
                    self.flag.set(true);
                }
                self.source.search(range)
            }
        }
        struct Pause(Rc<Cell<bool>>);
        impl BatchBoundary for Pause {
            fn after_batch(&mut self) -> ControlFlow<crate::btrfs::walk::StopReason> {
                if self.0.get() {
                    ControlFlow::Break(crate::btrfs::walk::StopReason::Paused)
                } else {
                    ControlFlow::Continue(())
                }
            }
        }
        let spill = crate::spill::Spill::create(
            std::env::temp_dir().join(format!("snapshot-share-pause-{}", std::process::id())),
        )
        .unwrap();
        let checkpoint = spill.dir().join("checkpoint");
        let mut inventory = set();
        inventory.subvolumes.push(subvolume(900, 10, Some(9), 50));
        inventory.subvolumes.sort_by_key(|s| s.id);
        let mut request = request(Depth::All);
        request.lives.push(OTHER);
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (900, vec![file(257, 2 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![directs(MIB, &[OLD]), directs(2 * MIB, &[900])],
            ),
        ];
        let flag = Rc::new(Cell::new(false));
        let mut source = PauseSecond {
            source: FakeTrees::new(trees.clone()),
            flag: flag.clone(),
        };
        assert!(matches!(
            analyze_checkpointed(
                &mut source,
                &inventory,
                &request,
                &mut Pause(flag),
                spill.dir(),
                Some(&checkpoint)
            ),
            Err(WalkError::Stopped(crate::btrfs::walk::StopReason::Paused))
        ));
        assert!(checkpoint.join(format!("{LIVE}.json")).is_file());
        assert!(!checkpoint.join(format!("{OTHER}.json")).exists());
        source.source.trees.remove(&OLD);
        let resumed = analyze_checkpointed(
            &mut source,
            &inventory,
            &request,
            &mut NoStop,
            spill.dir(),
            Some(&checkpoint),
        )
        .unwrap();
        let fresh = analyze(
            &mut FakeTrees::new(trees),
            &inventory,
            &request,
            &mut NoStop,
        )
        .unwrap();
        assert_eq!(resumed, fresh);
    }

    #[test]
    fn changed_live_generation_recomputes_only_that_share_on_resume() {
        let spill = crate::spill::Spill::create(
            std::env::temp_dir().join(format!("snapshot-live-resume-{}", std::process::id())),
        )
        .unwrap();
        let checkpoint = spill.dir().join("checkpoint");
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (EXTENT_TREE_OBJECTID, vec![directs(MIB, &[OLD])]),
        ];
        analyze_checkpointed(
            &mut FakeTrees::new(trees),
            &set(),
            &request(Depth::All),
            &mut NoStop,
            spill.dir(),
            Some(&checkpoint),
        )
        .unwrap();
        let mut changed = set();
        changed
            .subvolumes
            .iter_mut()
            .find(|s| s.id == LIVE)
            .unwrap()
            .generation += 1;
        // The now-empty tree must replace the cached nonempty answer.
        let resumed = analyze_checkpointed(
            &mut FakeTrees::new([]),
            &changed,
            &request(Depth::All),
            &mut NoStop,
            spill.dir(),
            Some(&checkpoint),
        )
        .unwrap();
        let fresh = analyze(
            &mut FakeTrees::new([]),
            &changed,
            &request(Depth::All),
            &mut NoStop,
        )
        .unwrap();
        assert_eq!(resumed, fresh);
        assert!(resumed.holdings.is_empty());
    }

    #[test]
    fn completed_snapshot_shares_resume_without_reading_their_extent_trees() {
        let spill = crate::spill::Spill::create(
            std::env::temp_dir().join(format!("snapshot-resume-{}", std::process::id())),
        )
        .unwrap();
        let checkpoint = spill.dir().join("checkpoint");
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (EXTENT_TREE_OBJECTID, vec![directs(MIB, &[OLD])]),
        ];
        let expected = analyze_checkpointed(
            &mut FakeTrees::new(trees),
            &set(),
            &request(Depth::All),
            &mut NoStop,
            spill.dir(),
            Some(&checkpoint),
        )
        .unwrap();
        // An empty source would yield zero without reusing the completed census.
        let resumed = analyze_checkpointed(
            &mut FakeTrees::new([]),
            &set(),
            &request(Depth::All),
            &mut NoStop,
            spill.dir(),
            Some(&checkpoint),
        )
        .unwrap();
        assert_eq!(resumed, expected);
        let path = checkpoint.join(format!("{LIVE}.json"));
        let original = std::fs::read(&path).unwrap();
        let mut altered: serde_json::Value = serde_json::from_slice(&original).unwrap();
        altered["holdings"][0]["bytes"] = serde_json::json!(123);
        std::fs::write(&path, serde_json::to_vec(&altered).unwrap()).unwrap();
        assert!(matches!(
            analyze_checkpointed(
                &mut FakeTrees::new([]),
                &set(),
                &request(Depth::All),
                &mut NoStop,
                spill.dir(),
                Some(&checkpoint)
            ),
            Err(WalkError::Checkpoint(_))
        ));
        std::fs::write(&path, original).unwrap();
        let mut changed = set();
        changed
            .subvolumes
            .iter_mut()
            .find(|s| s.id == OLD)
            .unwrap()
            .generation += 1;
        assert!(matches!(
            analyze_checkpointed(
                &mut FakeTrees::new([]),
                &changed,
                &request(Depth::All),
                &mut NoStop,
                spill.dir(),
                Some(&checkpoint)
            ),
            Err(WalkError::Checkpoint(_))
        ));
        std::fs::write(checkpoint.join(format!("{LIVE}.json")), b"broken").unwrap();
        assert!(matches!(
            analyze_checkpointed(
                &mut FakeTrees::new([]),
                &set(),
                &request(Depth::All),
                &mut NoStop,
                spill.dir(),
                Some(&checkpoint)
            ),
            Err(WalkError::Checkpoint(_))
        ));
    }

    #[test]
    fn five_hundred_snapshots_keep_the_census_bounded_and_totals_identical() {
        let mut set = SubvolumeSet {
            subvolumes: vec![subvolume(LIVE, 1, None, 50)],
            ..SubvolumeSet::default()
        };
        let ids: Vec<u64> = (1000..1500).collect();
        let files: Vec<TreeItem> = (0..1024).map(|i| file(257 + i, (i + 1) * MIB)).collect();
        let mut trees = vec![(LIVE, Vec::new())];
        for &id in &ids {
            set.subvolumes.push(subvolume(id, 2, Some(1), 50));
            trees.push((id, files.clone()));
        }
        trees.push((
            EXTENT_TREE_OBJECTID,
            (0..1024)
                .map(|i| directs((i + 1) * MIB, &[ids[0]]))
                .collect(),
        ));
        CENSUS_PEAK.with(|p| p.set(0));
        let result = run(&set, trees, Depth::All);
        assert_eq!(result.holdings, vec![holding(&ids, Hold::Resolved, 1024)]);
        assert_eq!(result.stats.extents, 1024);
        let peak = CENSUS_PEAK.with(|p| p.get());
        eprintln!("snapshot census retained model: {peak} bytes for 512000 references");
        assert!(
            peak < 8 << 20,
            "snapshot references must be spilled: retained {peak} bytes"
        );
    }

    #[test]
    fn repeated_snapshot_ranges_keep_all_owners_without_retaining_every_copy() {
        let mut set = SubvolumeSet {
            subvolumes: vec![subvolume(LIVE, 1, None, 50)],
            ..SubvolumeSet::default()
        };
        let ids: Vec<u64> = (1000..1500).collect();
        let files: Vec<TreeItem> = (0..512).map(|i| file(257 + i, MIB)).collect();
        let mut trees = vec![(LIVE, Vec::new())];
        for &id in &ids {
            set.subvolumes.push(subvolume(id, 2, Some(1), 50));
            trees.push((id, files.clone()));
        }
        trees.push((EXTENT_TREE_OBJECTID, vec![directs(MIB, &[ids[0]])]));
        CENSUS_PEAK.with(|p| p.set(0));
        let result = run(&set, trees, Depth::All);
        assert_eq!(result.holdings, vec![holding(&ids, Hold::Resolved, 1)]);
        assert_eq!(
            result
                .share(LIVE)
                .unwrap()
                .held_files
                .as_ref()
                .unwrap()
                .count,
            512
        );
        let peak = CENSUS_PEAK.with(|p| p.get());
        assert!(peak < 8 << 20, "identical ranges retained {peak} bytes");
    }

    fn subvolume(id: u64, uuid: u8, parent_uuid: Option<u8>, generation: u64) -> Subvolume {
        Subvolume {
            id,
            parent: None,
            name: format!("s{id}").into_bytes(),
            generation,
            readonly: parent_uuid.is_some(),
            pending_deletion: false,
            uuid: Some([uuid; 16]),
            parent_uuid: parent_uuid.map(|p| [p; 16]),
            last_snapshot: 0,
            otime: Some(id),
        }
    }

    /// Live share 256 with snapshots 300 and 301 (newer), and an
    /// unrelated share 400; every root last changed in generation 50.
    fn set() -> SubvolumeSet {
        SubvolumeSet {
            subvolumes: vec![
                subvolume(LIVE, 1, None, 50),
                subvolume(OLD, 2, Some(1), 50),
                subvolume(NEW, 3, Some(1), 50),
                subvolume(OTHER, 9, None, 50),
            ],
            ..SubvolumeSet::default()
        }
    }

    /// A file of tree `ino` referencing all of extent `bytenr`.
    fn file(ino: u64, bytenr: u64) -> TreeItem {
        whole_extent(ino, 0, bytenr, 4 * KIB)
    }

    /// Extent `bytenr` of 4 KiB allocated in `generation` with `backrefs`.
    fn extent(bytenr: u64, generation: u64, backrefs: &[DataBackref]) -> TreeItem {
        let mut data = encode_data_extent(backrefs.len() as u64, backrefs);
        data[8..16].copy_from_slice(&generation.to_le_bytes());
        (Key::new(bytenr, EXTENT_ITEM_KEY, 4 * KIB), data)
    }

    fn direct(root: u64) -> DataBackref {
        DataBackref::Direct {
            root,
            objectid: 257,
            offset: 0,
            count: 1,
        }
    }

    fn directs(bytenr: u64, roots: &[u64]) -> TreeItem {
        extent(
            bytenr,
            60,
            &roots.iter().map(|&r| direct(r)).collect::<Vec<_>>(),
        )
    }

    fn request(depth: Depth) -> Request {
        Request {
            lives: vec![LIVE],
            depth,
            owner_budget: DEFAULT_OWNER_BUDGET,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            held_files_budget: DEFAULT_HELD_FILES_BUDGET,
        }
    }

    fn run(set: &SubvolumeSet, trees: Vec<(u64, Vec<TreeItem>)>, depth: Depth) -> SnapshotAnalysis {
        analyze(
            &mut FakeTrees::new(trees),
            set,
            &request(depth),
            &mut NoStop,
        )
        .unwrap()
    }

    #[test]
    fn snapshot_report_keeps_directory_components_for_iscsi_management() {
        fn inode_ref(name: &[u8]) -> Vec<u8> {
            let mut data = 1_u64.to_le_bytes().to_vec();
            data.extend((name.len() as u16).to_le_bytes());
            data.extend(name);
            data
        }

        let mut set = set();
        set.subvolumes.insert(
            0,
            Subvolume {
                id: 5,
                parent: None,
                name: Vec::new(),
                generation: 50,
                readonly: false,
                pending_deletion: false,
                uuid: None,
                parent_uuid: None,
                last_snapshot: 0,
                otime: None,
            },
        );
        for root in &mut set.subvolumes[1..4] {
            root.parent = Some(Link {
                parent_id: 5,
                dirid: 301,
            });
        }
        let refs = vec![
            (
                Key::new(300, crate::btrfs::keys::INODE_REF_KEY, 256),
                inode_ref(b"@iSCSI"),
            ),
            (
                Key::new(301, crate::btrfs::keys::INODE_REF_KEY, 300),
                inode_ref(b"Snapshot"),
            ),
        ];
        let analysis = analyze(
            &mut FakeTrees::new([(5, refs)]),
            &set,
            &request(Depth::All),
            &mut NoStop,
        )
        .unwrap();
        let share = analysis.share(LIVE).unwrap();

        assert_eq!(share.subvolume_path, b"@iSCSI/Snapshot/s256");
        assert!(
            share
                .snapshots
                .iter()
                .all(|snapshot| snapshot.subvolume_path.starts_with(b"@iSCSI/Snapshot/"))
        );
        assert_eq!(manage_in_share(share), SAN_MANAGER);
    }

    fn holding(owners: &[u64], hold: Hold, extents: u64) -> Holding {
        Holding {
            owners: owners.to_vec(),
            hold,
            bytes: extents * 4 * KIB,
            extents,
            pinned_unreferenced_bytes: (hold == Hold::Resolved && owners.len() == 1).then_some(0),
        }
    }

    #[test]
    fn owners_are_the_read_roots_referencing_each_extent_a_snapshot_references() {
        let trees = vec![
            (
                OLD,
                vec![file(257, MIB), file(258, 2 * MIB), file(259, 3 * MIB)],
            ),
            (NEW, vec![file(257, 2 * MIB), file(258, 3 * MIB)]),
            (LIVE, vec![file(257, 3 * MIB), file(258, 4 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    directs(MIB, &[OLD]),
                    directs(2 * MIB, &[OLD, NEW]),
                    directs(3 * MIB, &[LIVE, OLD, NEW]),
                    directs(4 * MIB, &[LIVE]),
                ],
            ),
        ];
        let a = run(&set(), trees, Depth::All);
        assert_eq!(
            a.holdings,
            vec![
                holding(&[LIVE, OLD, NEW], Hold::Resolved, 1),
                holding(&[OLD], Hold::Resolved, 1),
                holding(&[OLD, NEW], Hold::Resolved, 1),
            ]
        );
        let share = a.share(LIVE).unwrap();
        let order: Vec<u64> = share.snapshots.iter().map(|s| s.id).collect();
        assert_eq!(order, vec![NEW, OLD]);
        assert_eq!(a.stats.trees_read, 3);
        assert_eq!(a.stats.extents, 3);
        assert_eq!(a.quota, None);
    }

    fn name(ino: u64, parent: u64, name: &[u8]) -> TreeItem {
        let mut data = 1_u64.to_le_bytes().to_vec();
        data.extend((name.len() as u16).to_le_bytes());
        data.extend(name);
        (
            Key::new(ino, crate::btrfs::keys::INODE_REF_KEY, parent),
            data,
        )
    }

    fn inode(ino: u64) -> TreeItem {
        (
            Key::new(ino, crate::btrfs::keys::INODE_ITEM_KEY, 0),
            vec![0; 160],
        )
    }

    /// OLD: `gone` (257, deleted) and 258; NEW: `kept` (258, rewritten
    /// live) and 260, whose extent a missing root may reference too.
    fn held_trees() -> Vec<(u64, Vec<TreeItem>)> {
        vec![
            (
                OLD,
                vec![
                    name(257, 256, b"gone"),
                    file(257, MIB),
                    file(258, 2 * MIB),
                    whole_extent(258, 4 * KIB, 5 * MIB, 4 * KIB),
                    file(259, 3 * MIB),
                ],
            ),
            (
                NEW,
                vec![
                    name(258, 256, b"kept"),
                    file(258, 2 * MIB),
                    file(259, 3 * MIB),
                    file(260, 6 * MIB),
                ],
            ),
            (LIVE, vec![inode(258), file(259, 3 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    directs(MIB, &[OLD]),
                    directs(2 * MIB, &[OLD, NEW]),
                    directs(3 * MIB, &[LIVE, OLD, NEW]),
                    directs(5 * MIB, &[OLD]),
                    directs(6 * MIB, &[NEW, 999]),
                ],
            ),
        ]
    }

    #[test]
    fn files_only_snapshots_hold_are_listed_largest_first() {
        let a = run(&set(), held_trees(), Depth::All);
        let files = a.share(LIVE).unwrap().held_files.clone().unwrap();
        assert_eq!(
            files,
            HeldFiles {
                complete: true,
                count: 3,
                files: vec![
                    HeldFile {
                        snapshot: NEW,
                        ino: 258,
                        path: b"kept".to_vec(),
                        in_live: true,
                        bytes: 8 * KIB,
                        unknown_bytes: 0,
                    },
                    HeldFile {
                        snapshot: OLD,
                        ino: 257,
                        path: b"gone".to_vec(),
                        in_live: false,
                        bytes: 4 * KIB,
                        unknown_bytes: 0,
                    },
                    HeldFile {
                        snapshot: NEW,
                        ino: 260,
                        path: Vec::new(),
                        in_live: false,
                        bytes: 0,
                        unknown_bytes: 4 * KIB,
                    },
                ],
            }
        );
    }

    #[test]
    fn files_past_the_budget_are_not_tallied_and_the_list_says_so() {
        let request = Request {
            held_files_budget: 1,
            ..request(Depth::All)
        };
        let a = analyze(
            &mut FakeTrees::new(held_trees()),
            &set(),
            &request,
            &mut NoStop,
        )
        .unwrap();
        let files = a.share(LIVE).unwrap().held_files.clone().unwrap();
        assert!(!files.complete);
        assert_eq!(files.count, 1);
        assert_eq!(files.files[0].ino, 257);
        assert_eq!(files.files[0].bytes, 4 * KIB);
    }

    #[test]
    fn a_skipped_snapshot_leaves_what_it_may_reference_unknown() {
        let mut set = set();
        set.subvolumes[1].generation = 20; // OLD last changed in 20
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (
                NEW,
                vec![file(257, MIB), file(258, 2 * MIB), file(259, 3 * MIB)],
            ),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    // Named by the skipped snapshot.
                    directs(MIB, &[OLD, NEW]),
                    // Allocated after the skipped snapshot last changed.
                    extent(2 * MIB, 30, &[direct(NEW)]),
                    // Older, so the skipped snapshot may share its leaf.
                    extent(3 * MIB, 10, &[direct(NEW)]),
                ],
            ),
        ];
        let a = run(&set, trees, Depth::Newest(1));
        assert_eq!(
            a.holdings,
            vec![
                holding(&[NEW], Hold::Resolved, 1),
                holding(&[NEW], Hold::Unknown, 2),
            ]
        );
        let share = a.share(LIVE).unwrap();
        assert_eq!(share.skipped(), 1);
        assert_eq!(a.stats.trees_read, 2);
    }

    #[test]
    fn a_root_outside_the_share_referencing_an_extent_holds_it() {
        let trees = vec![
            (OLD, vec![file(257, MIB), file(258, 2 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![directs(MIB, &[OLD, OTHER]), directs(2 * MIB, &[OLD, 999])],
            ),
        ];
        let a = run(&set(), trees, Depth::All);
        assert_eq!(
            a.holdings,
            vec![
                holding(&[OLD], Hold::External, 1),
                holding(&[OLD], Hold::Unknown, 1),
            ]
        );
    }

    #[test]
    fn shared_backrefs_are_resolved_through_the_tree_blocks_reaching_the_leaf() {
        let leaf = |bytenr, root| {
            (
                Key::new(bytenr, METADATA_ITEM_KEY, 0),
                encode_tree_block(1, false, &[BlockBackref::Root(root)]),
            )
        };
        let shared = |parent| DataBackref::Shared { parent, count: 1 };
        let trees = vec![
            (OLD, vec![file(257, MIB), file(258, 2 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    extent(MIB, 60, &[shared(64 * MIB)]),
                    // Reflinked into a leaf only the other share reaches.
                    extent(2 * MIB, 60, &[direct(OLD), shared(65 * MIB)]),
                    leaf(64 * MIB, LIVE),
                    leaf(65 * MIB, OTHER),
                ],
            ),
        ];
        let a = run(&set(), trees, Depth::All);
        assert_eq!(
            a.holdings,
            vec![
                holding(&[OLD], Hold::Resolved, 1),
                holding(&[OLD], Hold::External, 1),
            ]
        );
        assert_eq!(a.stats.owner_lookups, 2);
    }

    #[test]
    fn an_owner_walk_over_budget_leaves_the_extent_unknown() {
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    extent(
                        MIB,
                        60,
                        &[DataBackref::Shared {
                            parent: 64 * MIB,
                            count: 1,
                        }],
                    ),
                    (
                        Key::new(64 * MIB, METADATA_ITEM_KEY, 0),
                        encode_tree_block(1, false, &[BlockBackref::Parent(65 * MIB)]),
                    ),
                    (
                        Key::new(65 * MIB, METADATA_ITEM_KEY, 0),
                        encode_tree_block(1, false, &[BlockBackref::Root(LIVE)]),
                    ),
                ],
            ),
        ];
        let request = Request {
            owner_budget: 1,
            ..request(Depth::All)
        };
        let a = analyze(&mut FakeTrees::new(trees), &set(), &request, &mut NoStop).unwrap();
        assert_eq!(a.holdings, vec![holding(&[OLD], Hold::Unknown, 1)]);
        assert_eq!(a.stats.owner_unresolved_extents, 1);
    }

    #[test]
    fn global_owner_budget_exhaustion_keeps_the_extent_bytes_unknown() {
        let shared = DataBackref::Shared {
            parent: 2 * MIB,
            count: 1,
        };
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    extent(MIB, 60, &[shared]),
                    (
                        Key::new(2 * MIB, METADATA_ITEM_KEY, 0),
                        encode_tree_block(1, false, &[BlockBackref::Parent(3 * MIB)]),
                    ),
                    (
                        Key::new(3 * MIB, METADATA_ITEM_KEY, 0),
                        encode_tree_block(1, false, &[BlockBackref::Root(OLD)]),
                    ),
                ],
            ),
        ];
        let request = Request {
            global_owner_budget: 1,
            ..request(Depth::All)
        };

        let analysis = analyze(&mut FakeTrees::new(trees), &set(), &request, &mut NoStop).unwrap();

        assert_eq!(analysis.stats.owner_lookups, 1);
        assert_eq!(analysis.stats.owner_unresolved_extents, 1);
        assert_eq!(analysis.holdings, vec![holding(&[OLD], Hold::Unknown, 1)]);
        assert_eq!(analysis.freed(&BTreeSet::from([OLD])).unknown, 4 * KIB);
        assert_eq!(analysis.pinned_unreferenced(OLD), None);
    }

    #[test]
    fn snapshot_pinned_bytes_use_the_union_of_partial_file_ranges() {
        let reference = |ino, file_offset, extent_offset| {
            crate::model::testing::uncompressed(crate::model::testing::extent_item(
                ino,
                file_offset,
                crate::model::testing::REGULAR,
                crate::btrfs::parser::DiskExtent {
                    disk_bytenr: MIB,
                    disk_num_bytes: 8 * KIB,
                    offset: extent_offset,
                    num_bytes: 4 * KIB,
                },
            ))
        };
        let backrefs = [direct(OLD), direct(OLD)];
        let trees = vec![
            (OLD, vec![reference(257, 0, 0), reference(258, 0, 2 * KIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![(
                    Key::new(MIB, EXTENT_ITEM_KEY, 8 * KIB),
                    encode_data_extent(backrefs.len() as u64, &backrefs),
                )],
            ),
        ];

        let analysis = run(&set(), trees, Depth::All);

        assert_eq!(analysis.pinned_unreferenced(OLD), Some(2 * KIB));
        assert_eq!(
            analysis.holdings[0].pinned_unreferenced_bytes,
            Some(2 * KIB)
        );
    }

    #[test]
    fn compressed_snapshot_extents_have_unknown_pinned_bytes() {
        let mut reference = file(257, MIB);
        reference.1[16] = 1;
        let trees = vec![
            (OLD, vec![reference]),
            (EXTENT_TREE_OBJECTID, vec![directs(MIB, &[OLD])]),
        ];

        let analysis = run(&set(), trees, Depth::All);

        assert_eq!(analysis.holdings[0].hold, Hold::Resolved);
        assert_eq!(analysis.holdings[0].pinned_unreferenced_bytes, None);
        assert_eq!(analysis.pinned_unreferenced(OLD), None);
    }

    #[test]
    fn a_missing_or_malformed_extent_counts_as_unknown() {
        let trees = vec![
            (OLD, vec![file(257, MIB), file(258, 2 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![(Key::new(2 * MIB, EXTENT_ITEM_KEY, 4 * KIB), vec![1, 2])],
            ),
        ];
        let a = run(&set(), trees, Depth::All);
        assert_eq!(
            a.holdings,
            vec![Holding {
                owners: vec![OLD],
                hold: Hold::Unknown,
                bytes: 0,
                extents: 2,
                pinned_unreferenced_bytes: None,
            }]
        );
    }

    #[test]
    fn a_snapshot_awaiting_the_cleaner_never_holds_bytes() {
        let mut set = set();
        let mut pending = subvolume(302, 4, Some(1), 90);
        pending.pending_deletion = true;
        set.subvolumes.push(pending);
        set.subvolumes.sort_by_key(|s| s.id);
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (EXTENT_TREE_OBJECTID, vec![directs(MIB, &[OLD, 302])]),
        ];
        let a = run(&set, trees, Depth::All);
        assert_eq!(a.holdings, vec![holding(&[OLD], Hold::Resolved, 1)]);
        assert_eq!(a.share(LIVE).unwrap().pending, vec![302]);
    }

    #[test]
    fn a_consistent_quota_tree_is_kept_for_the_listed_subvolumes() {
        let mut set = set();
        set.quota_enabled = true;
        let words = |v: &[u64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let info = |id, excl| {
            (
                Key::new(0, QGROUP_INFO_KEY, id),
                words(&[1, excl, excl, excl, excl]),
            )
        };
        let trees = vec![(
            QUOTA_TREE_OBJECTID,
            vec![
                (
                    Key::new(0, QGROUP_STATUS_KEY, 0),
                    words(&[1, 1, QGROUP_STATUS_FLAG_ON, 0]),
                ),
                info(OLD, 8 * KIB),
                info(OTHER, 1),
            ],
        )];
        let a = run(&set, trees, Depth::All);
        assert_eq!(
            a.quota,
            Some(QuotaCheck {
                consistent: true,
                exclusive: BTreeMap::from([(OLD, 8 * KIB)]),
            })
        );
        assert_eq!(a.qgroup_exclusive(OLD), Some(8 * KIB));
    }

    #[test]
    fn the_census_charges_its_growth_before_reading_each_batch() {
        struct Budget(u64);
        impl BatchBoundary for Budget {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                ControlFlow::Continue(())
            }
            fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
                match self.0.checked_sub(bytes) {
                    Some(left) => {
                        self.0 = left;
                        ControlFlow::Continue(())
                    }
                    None => ControlFlow::Break(StopReason::ResourceLimit),
                }
            }
        }
        let files: Vec<TreeItem> = (0..10).map(|i| file(257 + i, (i + 1) * MIB)).collect();
        let mut source = FakeTrees::new(vec![(OLD, files)]);
        let err = analyze(&mut source, &set(), &request(Depth::All), &mut Budget(100)).unwrap_err();
        assert!(matches!(err, WalkError::Stopped(StopReason::ResourceLimit)));
    }

    #[test]
    fn owner_histogram_growth_is_charged_to_the_memory_budget() {
        struct RejectHistogramGrowth(Vec<u64>);
        impl BatchBoundary for RejectHistogramGrowth {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                ControlFlow::Continue(())
            }
            fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
                self.0.push(bytes);
                if bytes == HISTOGRAM_ENTRY_BYTES + 8 {
                    ControlFlow::Break(StopReason::ResourceLimit)
                } else {
                    ControlFlow::Continue(())
                }
            }
        }
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (EXTENT_TREE_OBJECTID, vec![directs(MIB, &[OLD])]),
        ];
        let mut boundary = RejectHistogramGrowth(Vec::new());

        let result = analyze(
            &mut FakeTrees::new(trees),
            &set(),
            &request(Depth::All),
            &mut boundary,
        );

        assert!(matches!(
            result,
            Err(WalkError::Stopped(StopReason::ResourceLimit))
        ));
        assert!(boundary.0.contains(&(HISTOGRAM_ENTRY_BYTES + 8)));
    }

    fn snapshot_scan(depth: Depth) -> SnapshotScan {
        SnapshotScan {
            depth,
            owner_budget: DEFAULT_OWNER_BUDGET,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            held_files_budget: DEFAULT_HELD_FILES_BUDGET,
        }
    }

    /// Deleting snapshots `deleted` with the live inodes of `uses`, each
    /// `(ino, bytenr)` of the live share.
    fn delete(
        set: &SubvolumeSet,
        trees: Vec<(u64, Vec<TreeItem>)>,
        deleted: &[u64],
        uses: &[(u64, u64)],
        depth: Depth,
    ) -> DeletionAnswer {
        let deletion = Deletion {
            snapshots: deleted.iter().copied().collect(),
            uses: uses
                .iter()
                .map(|&(ino, bytenr)| LiveUse {
                    bytenr,
                    tree: LIVE,
                    ino,
                })
                .collect(),
        };
        freed_by_deleting(
            &mut FakeTrees::new(trees),
            set,
            &deletion,
            &snapshot_scan(depth),
            &mut NoStop,
        )
        .unwrap()
    }

    #[test]
    fn a_live_file_and_the_snapshot_sharing_its_leaf_free_its_extent_together() {
        // The file predates OLD, which shares its leaf: only LIVE is named.
        let trees = || {
            vec![
                (OLD, vec![file(257, MIB)]),
                (LIVE, vec![file(257, MIB)]),
                (EXTENT_TREE_OBJECTID, vec![directs(MIB, &[LIVE])]),
            ]
        };
        let both = delete(&set(), trees(), &[OLD], &[(257, MIB)], Depth::All);
        assert_eq!((both.freed_bytes, both.unknown_bytes), (4 * KIB, 0));
        assert_eq!(both.snapshots, vec![OLD]);
        let live_only = delete(&set(), trees(), &[], &[(257, MIB)], Depth::All);
        assert_eq!((live_only.freed_bytes, live_only.unknown_bytes), (0, 0));
        let other = delete(&set(), trees(), &[NEW], &[(257, MIB)], Depth::All);
        assert_eq!((other.freed_bytes, other.unknown_bytes), (0, 0));
    }

    #[test]
    fn a_live_reference_outside_the_selection_keeps_the_extent() {
        let trees = vec![
            (OLD, vec![file(257, MIB)]),
            (LIVE, vec![file(257, MIB), file(258, MIB)]),
            (EXTENT_TREE_OBJECTID, vec![directs(MIB, &[LIVE, LIVE])]),
        ];
        let a = delete(&set(), trees, &[OLD], &[(257, MIB)], Depth::All);
        assert_eq!((a.freed_bytes, a.unknown_bytes), (0, 0));
    }

    #[test]
    fn a_mixed_set_frees_what_only_its_members_reference() {
        let trees = vec![
            (
                OLD,
                vec![file(257, MIB), file(258, 2 * MIB), file(259, 3 * MIB)],
            ),
            (NEW, vec![file(258, 2 * MIB)]),
            (LIVE, vec![file(259, 3 * MIB), file(260, 4 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    directs(MIB, &[OLD]),
                    directs(2 * MIB, &[OLD, NEW]),
                    directs(3 * MIB, &[LIVE]),
                    directs(4 * MIB, &[LIVE]),
                ],
            ),
        ];
        let a = delete(
            &set(),
            trees,
            &[OLD],
            &[(259, 3 * MIB), (260, 4 * MIB)],
            Depth::All,
        );
        assert_eq!((a.freed_bytes, a.unknown_bytes), (12 * KIB, 0));
        assert_eq!(a.snapshots_not_materialized, 0);
        assert_eq!(a.stats.trees_read, 3);
    }

    #[test]
    fn a_shared_leaf_reached_only_by_deleted_roots_frees_its_extent() {
        let leaf = |bytenr, root| {
            (
                Key::new(bytenr, METADATA_ITEM_KEY, 0),
                encode_tree_block(1, false, &[BlockBackref::Root(root)]),
            )
        };
        let shared = |parent| DataBackref::Shared { parent, count: 1 };
        let trees = vec![
            (OLD, vec![file(257, MIB), file(258, 2 * MIB)]),
            (LIVE, vec![file(257, MIB), file(258, 2 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    extent(MIB, 60, &[shared(64 * MIB)]),
                    // Reflinked into a leaf only the other share reaches.
                    extent(2 * MIB, 60, &[direct(LIVE), shared(65 * MIB)]),
                    leaf(64 * MIB, OLD),
                    leaf(65 * MIB, OTHER),
                ],
            ),
        ];
        let a = delete(
            &set(),
            trees,
            &[OLD],
            &[(257, MIB), (258, 2 * MIB)],
            Depth::All,
        );
        assert_eq!((a.freed_bytes, a.unknown_bytes), (4 * KIB, 0));
        assert_eq!(a.stats.owner_lookups, 2);
    }

    #[test]
    fn an_unread_snapshot_that_may_share_a_leaf_leaves_its_extent_unknown() {
        // Depth 1 reads NEW only; OLD last changed in generation 50.
        let trees = vec![
            (NEW, vec![file(257, MIB), file(258, 2 * MIB)]),
            (LIVE, vec![file(257, MIB), file(258, 2 * MIB)]),
            (
                EXTENT_TREE_OBJECTID,
                vec![
                    extent(MIB, 10, &[direct(LIVE)]),
                    extent(2 * MIB, 60, &[direct(LIVE)]),
                ],
            ),
        ];
        let a = delete(
            &set(),
            trees,
            &[NEW],
            &[(257, MIB), (258, 2 * MIB)],
            Depth::Newest(1),
        );
        assert_eq!((a.freed_bytes, a.unknown_bytes), (4 * KIB, 4 * KIB));
        assert_eq!(a.unknown_extents, 1);
        assert_eq!(a.snapshots_not_materialized, 1);
    }

    #[test]
    fn a_missing_extent_item_is_counted_unknown() {
        let trees = vec![(OLD, vec![file(257, MIB)]), (LIVE, vec![file(257, MIB)])];
        let a = delete(&set(), trees, &[OLD], &[(257, MIB)], Depth::All);
        assert_eq!((a.freed_bytes, a.unknown_extents), (0, 1));
    }

    #[test]
    fn a_deletion_charges_its_live_references_to_the_memory_budget() {
        struct Refuse;
        impl BatchBoundary for Refuse {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                ControlFlow::Continue(())
            }
            fn reserve(&mut self, _: u64) -> ControlFlow<StopReason> {
                ControlFlow::Break(StopReason::ResourceLimit)
            }
        }
        let deletion = Deletion {
            snapshots: BTreeSet::new(),
            uses: vec![LiveUse {
                bytenr: MIB,
                tree: LIVE,
                ino: 257,
            }],
        };
        let result = freed_by_deleting(
            &mut FakeTrees::new(Vec::new()),
            &set(),
            &deletion,
            &snapshot_scan(Depth::All),
            &mut Refuse,
        );
        assert!(matches!(
            result,
            Err(WalkError::Stopped(StopReason::ResourceLimit))
        ));
    }
}
