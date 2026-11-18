//! One filesystem's scan (SPEC 5.3, 7, 13, 19 Phase 3): discover
//! subvolumes, read the namespaces the selected roots need, materialize
//! them one unit at a time, and judge consistency from generations
//! before and after.

use crate::btrfs::backref::DataBackref;
use crate::btrfs::extent_refs::{ExtentRefs, Plan, ResolveStats, ranges, resolve_in, stream_data};
use crate::btrfs::keys::FIRST_FREE_OBJECTID;
use crate::btrfs::leaf_ranges::{
    BlockReader, DEFAULT_READ_BUDGET, LeafRangeStats, LeafRanges, LeafReadPlan, Unresolved,
};
use crate::btrfs::owners::OwnerWalk;
use crate::btrfs::source::{MetadataSource, SourceError, SpaceSnapshot, SpaceWindow};
use crate::btrfs::subvolume::{Subvolume, SubvolumeSet, discover, relocation_trees};
use crate::btrfs::walk::{BatchBoundary, StopReason, WalkError, WalkStats};
use crate::codec::{Fnv, HashingWriter};
use crate::duplicates::candidates::{UNIT_FILE as CANDIDATES, UnitWriter};
use crate::model::accounting::{
    AccountingSummary, Blockers, HeldUse, RootClass, Verdict, judge, pinned_bytes,
    unreferenced_file_bytes,
};
use crate::model::namespace::{Namespace, RecycleEntryCtime, SpilledRefs, TreeStats};
use crate::model::tree::{
    Accountant, Aggregate, Anomalies, Attribution, Marks, Metrics, Nested, NodeFlags, Pending,
    RootKind, ScopeTree, VIRTUAL_ROOT,
};
use crate::overview::{
    ExtentInput, OverviewData, Owner as OverviewOwner, PinnedRangesSubcase, UnknownReason,
    attribute_extent, owners_resolved,
};
use crate::report::{
    ChangeReason, Consistency, Coverage, ScanStats, StateDirWrites, SubvolumeRole, SubvolumeState,
    hex_bytes, tree_record,
};
use crate::snapshots::census::{
    Deletion, LiveUse, Request, analyze_checkpointed, freed_by_deleting, inventory,
};
use crate::snapshots::{DeletionAnswer, DeletionRequest, SnapshotAnalysis, SnapshotScan};
use crate::spill::Spill;
use crate::target::Scope;
use crate::units::{
    Fragment, Group, JudgedReader, Merge, Placement, RUN_USES, Router, Sorter, UnitKey, splice,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

pub mod attribution;
mod own_writes;
mod owner_table;
mod owners;

/// Path scans search candidate extents at most this far apart in one
/// range: reading the items between is cheaper than another descent.
pub const COALESCE_GAP: u64 = 16 << 20;

/// A selected root on this filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRequest {
    /// Full path, the root's name in the report.
    pub path: Vec<u8>,
    pub scope: Scope,
    pub tree: u64,
    pub ino: u64,
}

/// What the scan tells its caller as it goes.
#[derive(Debug)]
pub enum ScanEvent<'a> {
    Discovered {
        subvolumes: usize,
        /// Of them writable: the most units a volume scan reads.
        writable: usize,
    },
    /// A sealed unit of this tree stands in for reading it.
    UnitReused {
        tree: u64,
        stats: &'a TreeStats,
    },
    TreeStarted {
        tree: u64,
        /// The subvolume's link name; empty when it is unlinked or the
        /// top-level tree.
        name: &'a [u8],
    },
    /// This unit changed after its saved position and is being reread.
    UnitRestarted {
        tree: u64,
    },
    TreeRead {
        tree: u64,
        stats: &'a TreeStats,
    },
    /// Reading the extent tree for this many distinct extents.
    Resolving {
        extents: u64,
    },
    Aggregating {
        nodes: usize,
    },
    /// Reading the snapshots of this many shares (SPEC 5.14).
    AnalyzingSnapshots {
        shares: usize,
    },
}

/// Pacing, stopping and progress for a scan.
pub trait ScanControl {
    fn boundary(&mut self) -> &mut dyn BatchBoundary;
    fn event(&mut self, event: ScanEvent<'_>);
    /// Snapshot roots to delete together with the selection (SPEC 5.14
    /// mixed sets); taken once per scan.
    fn deletion(&mut self) -> Option<DeletionRequest> {
        None
    }
}

#[derive(Debug)]
pub struct ScanOutcome {
    pub stopped: Option<StopReason>,
    pub allocated_overview: Option<crate::model::tree::AllocationOverview>,
    pub totals: Metrics,
    pub nodes: ReportNodes,
    pub accounting: AccountingSummary,
    pub subvolumes: Vec<SubvolumeState>,
    pub coverage: Coverage,
    pub stats: ScanStats,
    pub consistency: Consistency,
    /// Why `consistency` is `changed-during-scan`; empty when nothing
    /// changed.
    pub change_reasons: Vec<ChangeReason>,
    /// Generation changes left out of `change_reasons` because only the
    /// state directory's own writes made them (SPEC 22.4).
    pub state_dir_writes: Vec<StateDirWrites>,
    /// Roots that vanished before their tree was read: `(path, reason)`.
    pub root_errors: Vec<(Vec<u8>, String)>,
    /// What `--snapshots` found.
    pub snapshots: Option<SnapshotAnalysis>,
    /// Why `--snapshots` found nothing.
    pub snapshot_error: Option<String>,
    /// `statfs` and `BTRFS_IOC_SPACE_INFO` captured around a volume scan.
    pub space: Option<SpaceWindow>,
    /// Extent-tree attribution captured by a volume scan.
    pub overview: Option<crate::overview::OverviewData>,
    /// Per-inode ctimes recorded for Recycle Bin entries in the selected roots.
    pub recycle_ctimes: Vec<RecycleEntryCtime>,
    /// What deleting the requested snapshots with the selection frees.
    pub deletion: Option<DeletionAnswer>,
    /// Why that could not be answered.
    pub deletion_error: Option<String>,
    /// Each unit's tree and tier-0 duplicate candidate file, for the
    /// report's sidecar (`duplicates::candidates::publish`).
    pub candidates: Vec<(u64, PathBuf)>,
}

#[derive(Debug, Clone, Default)]
struct OwnerWalkStats {
    lookups: u64,
    budget_capped_extents: u64,
    /// What reading unscanned references for pinned bytes read.
    pinned_reads: LeafRangeStats,
    /// Why those references could not be read.
    pinned_reader_unavailable: Option<String>,
    plan: Option<crate::btrfs::read_budget::ReadBudget>,
}

#[derive(Debug, Clone)]
pub(crate) struct ScanOptions {
    pub snapshots: Option<SnapshotScan>,
    pub owner_budget: u32,
    /// Leaf reads and root searches a volume overview may make to
    /// measure pinned bytes.
    pub pinned_read_budget: u64,
    /// How those reads are batched.
    pub leaf_reads: LeafReadPlan,
    pub pending: Option<PathBuf>,
    /// Record tier-0 duplicate candidates with each unit (on by default).
    pub duplicate_candidates: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            snapshots: None,
            owner_budget: crate::snapshots::DEFAULT_OWNER_BUDGET,
            pinned_read_budget: DEFAULT_READ_BUDGET,
            leaf_reads: LeafReadPlan::default(),
            pending: None,
            duplicate_candidates: true,
        }
    }
}

/// What the volume overview may spend resolving owners and pinned bytes.
#[derive(Debug, Clone)]
struct OverviewBudgets {
    owner: u32,
    pinned_reads: u64,
    leaf_reads: LeafReadPlan,
    used_bytes: u64,
    pending: Option<PathBuf>,
}

/// A scan's report records, spliced from its spilled units' fragments.
#[derive(Debug)]
pub enum ReportNodes {
    Stream {
        spill: Spill,
        count: u64,
    },
    Spilled {
        spill: Spill,
        /// Units of selected roots, in report order.
        tops: Vec<usize>,
        count: u64,
    },
}

impl ReportNodes {
    pub fn count(&self) -> u64 {
        match self {
            Self::Stream { count, .. } => *count,
            Self::Spilled { count, .. } => *count,
        }
    }

    /// Writes every record in preorder.
    pub fn write<W: Write>(&self, w: &mut HashingWriter<W>) -> io::Result<()> {
        match self {
            Self::Stream { spill, .. } => {
                let mut reader = crate::ext4::records::Reader::new(spill.path("nodes"));
                while let Some(record) = reader.read()? {
                    let mut borrowed = record.borrowed();
                    if borrowed.outside.is_some_and(|outside| {
                        outside.metrics == borrowed.metrics && outside.held == borrowed.held
                    }) {
                        borrowed.outside = None;
                    }
                    if record.depth > 0 {
                        borrowed.name = record
                            .name
                            .rsplit(|&byte| byte == b'/')
                            .next()
                            .expect("component");
                    }
                    crate::report::encode_record(w, &borrowed)?;
                }
            }
            Self::Spilled { spill, tops, .. } => {
                let path = |unit: usize| spill.path(&part(unit, "fragment"));
                for &unit in tops {
                    splice(w, &path, unit, 0)?;
                }
            }
        }
        Ok(())
    }
}

/// The spill file of one part of a unit.
fn part(unit: usize, name: &str) -> String {
    format!("{unit}.{name}")
}

/// Where a run seals the units it completes and finds reusable ones.
pub trait UnitCache {
    /// Unix seconds now.
    fn now(&self) -> u64;
    /// The files of a sealed unit that may stand in for `key`.
    fn find(&mut self, key: &UnitKey) -> Option<PathBuf>;
    fn find_checked(&mut self, key: &UnitKey) -> Result<Option<PathBuf>, WalkError> {
        Ok(self.find(key))
    }
    /// A durable job's saved identity, before checking its full key and files.
    fn checkpoint_key(&self, _tree: u64) -> Option<UnitKey> {
        None
    }
    fn namespace_checkpoint(&self) -> Option<PathBuf> {
        None
    }
    fn namespace_checkpoint_for(&self, _tree: u64) -> Option<PathBuf> {
        self.namespace_checkpoint()
    }
    fn checkpoint_keys(&self) -> Result<Vec<UnitKey>, WalkError> {
        Ok(Vec::new())
    }
    /// Seals the unit files in `staging`, read at `captured`; returns
    /// where they are now.
    fn seal(&mut self, key: &UnitKey, captured: u64, staging: &Path) -> Result<PathBuf, WalkError>;
}

/// Seals nothing: every unit is read.
#[derive(Debug, Default)]
pub struct NoCache;

impl UnitCache for NoCache {
    fn now(&self) -> u64 {
        0
    }

    fn find(&mut self, _: &UnitKey) -> Option<PathBuf> {
        None
    }

    fn seal(&mut self, _: &UnitKey, _: u64, staging: &Path) -> Result<PathBuf, WalkError> {
        Ok(staging.to_path_buf())
    }
}

/// A scan that ended early, with the work it had done by then.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct ScanStopped {
    pub error: WalkError,
    pub stats: ScanStats,
}

impl From<WalkError> for Box<ScanStopped> {
    fn from(error: WalkError) -> Self {
        Box::new(ScanStopped {
            error,
            stats: ScanStats::default(),
        })
    }
}

/// Scans `roots` (all on the filesystem behind `source`) one unit at a
/// time, spilling each to `spill` and sealing it in `cache`, or reusing
/// a sealed one. Read-only subvolumes nested below a root are
/// snapshots: they are listed but not materialized, since their bytes
/// are external references, unless `snapshots` asks for them.
pub(crate) fn scan_filesystem<S: MetadataSource + ?Sized>(
    source: &mut S,
    roots: &[RootRequest],
    marks: &Marks,
    options: ScanOptions,
    control: &mut dyn ScanControl,
    spill: Spill,
    cache: &mut dyn UnitCache,
) -> Result<ScanOutcome, Box<ScanStopped>> {
    let space_start = if roots.iter().any(|root| root.scope == Scope::Volume) {
        source.space_snapshot()
    } else {
        None
    };
    let maintenance = maintenance_changes(source);
    let mut before = discover_relocating(source, control.boundary())?;
    control.event(ScanEvent::Discovered {
        subvolumes: before.subvolumes.len(),
        writable: before.subvolumes.iter().filter(|s| !s.readonly).count(),
    });
    validate_saved_roots(&before, &cache.checkpoint_keys()?)?;
    resume_own_state_generations(
        source,
        marks,
        &mut before,
        |tree| cache.checkpoint_key(tree),
        control.boundary(),
    )?;
    let mut run = Run {
        spill,
        units: Vec::new(),
        roles: BTreeMap::new(),
        captured: BTreeMap::new(),
        complete: BTreeSet::new(),
        stats: ScanStats {
            subvolumes_discovered: before.subvolumes.len() as u64,
            ..ScanStats::default()
        },
        root_errors: Vec::new(),
        uncertain_roots: BTreeSet::new(),
        vanished: BTreeSet::new(),
        candidates: options.duplicate_candidates,
    };
    let selection = Selection {
        roots,
        marks,
        snapshots: options.snapshots,
        budgets: OverviewBudgets {
            owner: options.owner_budget,
            pinned_reads: options.pinned_read_budget,
            leaf_reads: options.leaf_reads,
            used_bytes: space_start
                .as_ref()
                .and_then(|s| s.statfs.as_ref().ok())
                .map_or(0, |s| s.total_bytes.saturating_sub(s.free_bytes)),
            pending: options.pending,
        },
        before: &before,
        candidates: options.duplicate_candidates,
    };
    match run.stages(source, &selection, control, cache, space_start) {
        Ok(mut done) => {
            done.maintenance = merge_changes(maintenance, done.maintenance);
            match own_writes::state_dir_writes(
                source,
                &marks.own_state,
                &before,
                &done.after,
                control.boundary(),
            ) {
                Ok(writes) => Ok(run.finish(done, &before, writes)),
                Err(error) => Err(Box::new(ScanStopped {
                    error,
                    stats: run.partial_stats(),
                })),
            }
        }
        Err(error) => Err(Box::new(ScanStopped {
            error,
            stats: run.partial_stats(),
        })),
    }
}

/// Checkpoint files can themselves advance the containing tree's generation.
/// Reuse the original baseline only when every changed inode is proven inside
/// our state directory. Full identity/input keys and checksums are still checked
/// by the unit and namespace readers; other changes reread only their unit.
fn resume_own_state_generations<S: MetadataSource + ?Sized>(
    source: &mut S,
    marks: &Marks,
    before: &mut SubvolumeSet,
    saved_key: impl Fn(u64) -> Option<UnitKey>,
    boundary: &mut dyn BatchBoundary,
) -> Result<(), WalkError> {
    let trees: BTreeSet<u64> = marks.own_state.iter().map(|&(tree, _)| tree).collect();
    for tree in trees {
        let Some(saved) = saved_key(tree) else {
            continue;
        };
        let Some(current) = before.get(tree) else {
            continue;
        };
        if saved.generation == current.generation
            || saved.readonly
            || current.readonly
            || saved.uuid
                != current
                    .uuid
                    .map_or(String::new(), |id| hex_bytes::encode(&id))
        {
            continue;
        }
        let mut prior = before.clone();
        prior
            .subvolumes
            .iter_mut()
            .find(|root| root.id == tree)
            .expect("present")
            .generation = saved.generation;
        let writes =
            own_writes::state_dir_writes(source, &marks.own_state, &prior, before, boundary)?;
        if writes.iter().any(|write| write.subvolume_id == tree) {
            before
                .subvolumes
                .iter_mut()
                .find(|root| root.id == tree)
                .expect("present")
                .generation = saved.generation;
        }
    }
    Ok(())
}

/// Check retained root identities before accepting resume. Generation changes
/// trigger a per-unit reread in the worker; they do not invalidate the job.
pub(crate) fn validate_checkpoint_roots<S: MetadataSource + ?Sized>(
    source: &mut S,
    _marks: &Marks,
    keys: &[UnitKey],
    boundary: &mut dyn BatchBoundary,
) -> Result<(), WalkError> {
    let current = discover_relocating(source, boundary)?;
    if !current.relocating.is_empty() {
        return Err(WalkError::Checkpoint(
            "checkpoint trees are being relocated".into(),
        ));
    }
    validate_saved_roots(&current, keys)
}

fn validate_saved_roots(current: &SubvolumeSet, keys: &[UnitKey]) -> Result<(), WalkError> {
    if !keys.is_empty() && !current.relocating.is_empty() {
        return Err(WalkError::Checkpoint(
            "checkpoint trees are being relocated".into(),
        ));
    }
    for saved in keys {
        let root = current.get(saved.subvolume).ok_or_else(|| {
            WalkError::Checkpoint(format!(
                "saved subvolume {} no longer exists",
                saved.subvolume
            ))
        })?;
        if saved.uuid != root.uuid.map_or(String::new(), |id| hex_bytes::encode(&id)) {
            return Err(WalkError::Checkpoint(format!(
                "the subvolume identity changed (subvolume {})",
                saved.subvolume
            )));
        }
    }
    Ok(())
}

/// Balance and device replace rewrite extent mappings (SPEC 8.1). A
/// failed probe cannot rule one out.
fn maintenance_changes<S: MetadataSource + ?Sized>(source: &mut S) -> Vec<ChangeReason> {
    source
        .maintenance_probes()
        .into_iter()
        .filter_map(|(operation, running)| match running {
            Ok(false) => None,
            Ok(true) => Some(ChangeReason::MaintenanceRunning { operation }),
            Err(e) => Some(ChangeReason::MaintenanceProbeFailed {
                operation,
                errno: e.raw_os_error(),
            }),
        })
        .collect()
}

/// The subvolumes and the roots a balance is relocating.
fn discover_relocating<S: MetadataSource + ?Sized>(
    source: &mut S,
    boundary: &mut dyn BatchBoundary,
) -> Result<SubvolumeSet, WalkError> {
    let mut set = discover(source, boundary)?;
    set.relocating = relocation_trees(source, boundary)?;
    Ok(set)
}

/// A relocation tree seen by either discovery.
fn relocation_changes(before: &SubvolumeSet, after: &SubvolumeSet) -> Vec<ChangeReason> {
    before
        .relocating
        .union(&after.relocating)
        .map(|&subvolume_id| ChangeReason::RelocationTree { subvolume_id })
        .collect()
}

/// `first` followed by the changes of `later` it does not already hold.
fn merge_changes(mut first: Vec<ChangeReason>, later: Vec<ChangeReason>) -> Vec<ChangeReason> {
    for change in later {
        if !first.contains(&change) {
            first.push(change);
        }
    }
    first
}

fn route_group(
    group: &Group,
    refs: Option<&ExtentRefs>,
    classes: &dyn RootClass,
    summary: &mut AccountingSummary,
    router: &mut Router,
    failed: &mut Option<io::Error>,
) -> bool {
    let held: Vec<HeldUse> = group.uses.iter().map(|&(_, use_)| use_).collect();
    let judgement = judge(&held, refs, Some(classes));
    let reclaimable = judgement.verdict == Some(Verdict::Reclaimable);
    summary.count(&judgement);
    if let Err(error) = router.route(group, judgement) {
        failed.get_or_insert(error);
    }
    reclaimable
}

#[allow(clippy::too_many_arguments)]
fn overview_owners<S: MetadataSource + ?Sized, R: BlockReader>(
    source: &mut S,
    bytenr: u64,
    refs: &ExtentRefs,
    owners: &mut owners::OwnerIndex,
    before: &SubvolumeSet,
    walk: &mut OwnerWalk,
    mut leaves: Option<&mut LeafRanges<R>>,
    boundary: &mut dyn BatchBoundary,
) -> Result<(Vec<OverviewOwner>, bool), WalkError> {
    let declared = refs
        .backrefs
        .iter()
        .map(|reference| u64::from(reference.count()))
        .sum::<u64>();
    let complete = !refs.malformed && refs.num_bytes > 0 && declared == refs.refs;
    if !complete {
        return Ok((
            vec![OverviewOwner::Unknown(UnknownReason::IncompleteReferences)],
            false,
        ));
    }
    let mut resolved = Vec::new();
    let mut shared = Vec::new();
    for reference in &refs.backrefs {
        match *reference {
            DataBackref::Direct { root, objectid, .. } => {
                resolved.push(owners.resolve_owner(source, root, objectid, before, boundary)?)
            }
            DataBackref::Shared { parent, .. } => shared.push(parent),
        }
    }
    if !shared.is_empty() {
        match walk.anchors(source, &shared, boundary)? {
            Some(roots) => {
                let needs_paths = roots
                    .iter()
                    .any(|&root| shared_root_needs_paths(owners.root_owner(root, before)));
                if needs_paths && let Some(leaves) = leaves.as_mut() {
                    // The first anchor query enforces the per-extent budget;
                    // these per-leaf queries reuse its verified memo entries.
                    for &reference in &refs.backrefs {
                        let DataBackref::Shared { parent, .. } = reference else {
                            continue;
                        };
                        let Some(roots) = walk.anchors(source, &[parent], boundary)? else {
                            resolved.push(OverviewOwner::Unknown(
                                UnknownReason::SharedTreeOwnerUnresolved,
                            ));
                            continue;
                        };
                        let inodes = if roots
                            .iter()
                            .any(|&root| shared_root_needs_paths(owners.root_owner(root, before)))
                        {
                            leaves.shared_inodes(source, bytenr, refs, reference, boundary)?
                        } else {
                            Err(Unresolved::Unverified)
                        };
                        for root in roots {
                            let owner = owners.root_owner(root, before);
                            if shared_root_needs_paths(owner)
                                && let Ok(inodes) = &inodes
                            {
                                for &inode in inodes {
                                    resolved
                                        .push(owners.resolve_owner(
                                            source, root, inode, before, boundary,
                                        )?);
                                }
                            } else {
                                resolved.push(owner);
                            }
                        }
                    }
                } else {
                    resolved.extend(
                        roots
                            .into_iter()
                            .map(|root| owners.root_owner(root, before)),
                    );
                }
            }
            None => resolved.push(OverviewOwner::Unknown(if walk.budget_exhausted {
                UnknownReason::OwnerBudgetExhausted
            } else {
                UnknownReason::SharedTreeOwnerUnresolved
            })),
        }
    }
    Ok((resolved, true))
}

fn shared_root_needs_paths(owner: OverviewOwner) -> bool {
    matches!(owner, OverviewOwner::DirectOwnerUnresolved(diagnostic)
        if diagnostic.subcase == crate::overview::DirectOwnerSubcase::IndexedPathUnmapped)
}

/// Bytes of a volume extent that no scanned file range references, or
/// why unscanned references may cover a different range. Scanned ranges
/// that leave nothing unreferenced prove the answer whatever else refers
/// to the extent.
fn overview_pinned(group: Option<&Group>, refs: &ExtentRefs) -> Result<u64, PinnedRangesSubcase> {
    let group = group.ok_or(PinnedRangesSubcase::NoScannedUse)?;
    let uses: Vec<_> = group.uses.iter().map(|&(_, u)| u.extent).collect();
    let pinned = pinned_bytes(&uses, refs.num_bytes).ok_or(PinnedRangesSubcase::RangeUnmappable)?;
    let shared = refs
        .backrefs
        .iter()
        .any(|r| matches!(r, DataBackref::Shared { .. }));
    if shared && pinned > 0 {
        return Err(PinnedRangesSubcase::SharedReference);
    }
    Ok(pinned)
}

/// `overview_pinned`, measured from the ranges the unscanned references
/// hold when the scanned ranges alone cannot tell and `leaves` can read
/// them.
fn overview_pinned_read<S: MetadataSource + ?Sized, R: BlockReader>(
    source: &mut S,
    bytenr: u64,
    group: Option<&Group>,
    refs: &ExtentRefs,
    leaves: Option<&mut LeafRanges<R>>,
    boundary: &mut dyn BatchBoundary,
) -> Result<Result<u64, PinnedRangesSubcase>, WalkError> {
    let scanned = overview_pinned(group, refs);
    let read = match (scanned, leaves) {
        (Err(PinnedRangesSubcase::NoScannedUse), Some(leaves)) => {
            leaves.all_ranges(source, bytenr, refs, boundary)?
        }
        (Err(PinnedRangesSubcase::SharedReference), Some(leaves)) => {
            leaves.shared_ranges(source, bytenr, refs, boundary)?
        }
        _ => return Ok(scanned),
    };
    let read = match read {
        Ok(read) => read,
        Err(Unresolved::BudgetExhausted) => {
            return Ok(Err(PinnedRangesSubcase::ReadBudgetExhausted));
        }
        Err(Unresolved::Unverified) => return Ok(Err(PinnedRangesSubcase::LeafUnverified)),
        Err(Unresolved::RootUnsearchable) => return Ok(scanned),
    };
    let uses = group
        .iter()
        .flat_map(|g| g.uses.iter().map(|(_, u)| u.extent.file_range()));
    Ok(unreferenced_file_bytes(uses.chain(read), refs.num_bytes)
        .ok_or(PinnedRangesSubcase::RangeUnmappable))
}

/// A volume extent judged but not yet attributed to its overview part.
struct HeldExtent {
    bytenr: u64,
    group: Option<Group>,
    refs: ExtentRefs,
    owners: Vec<OverviewOwner>,
    refs_complete: bool,
    /// Every owner is resolved, so its pinned bytes are worth reading.
    owned: bool,
    reclaimable: bool,
}

/// Volume extents held so the shared leaves their pinned bytes need are
/// prefetched together, then attributed.
struct HeldExtents {
    held: Vec<HeldExtent>,
    window: usize,
}

impl HeldExtents {
    fn new(window: usize) -> Self {
        HeldExtents {
            held: Vec::new(),
            window: window.max(1),
        }
    }

    fn push(&mut self, extent: HeldExtent) {
        self.held.push(extent);
    }

    fn full(&self) -> bool {
        self.held.len() >= self.window
    }

    /// The leaves the shared references of the held extents name, where
    /// the scanned ranges alone cannot tell their pinned bytes.
    fn shared_parents(&self) -> Vec<u64> {
        let unmeasured = |e: &&HeldExtent| {
            e.owned
                && matches!(
                    overview_pinned(e.group.as_ref(), &e.refs),
                    Err(PinnedRangesSubcase::NoScannedUse | PinnedRangesSubcase::SharedReference)
                )
        };
        self.held
            .iter()
            .filter(unmeasured)
            .flat_map(|e| e.refs.backrefs.iter())
            .filter_map(|r| match *r {
                DataBackref::Shared { parent, .. } => Some(parent),
                DataBackref::Direct { .. } => None,
            })
            .collect()
    }

    /// Measures the pinned bytes of the held extents, after prefetching
    /// their shared leaves when more than one is held, and adds each to
    /// `overview`.
    fn flush<S: MetadataSource + ?Sized, R: BlockReader>(
        &mut self,
        source: &mut S,
        mut leaves: Option<&mut LeafRanges<R>>,
        overview: &mut OverviewData,
        mut pending: Option<&mut attribution::Capture>,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(), WalkError> {
        if let Some(leaves) = leaves.as_deref_mut()
            && self.window > 1
        {
            leaves.prefetch(source, &self.shared_parents(), boundary)?;
        }
        for extent in self.held.drain(..) {
            let pinned_bytes = overview_pinned_read(
                source,
                extent.bytenr,
                extent.group.as_ref(),
                &extent.refs,
                leaves.as_deref_mut().filter(|_| extent.owned),
                boundary,
            )?;
            if pinned_bytes == Err(PinnedRangesSubcase::ReadBudgetExhausted)
                && extent.owned
                && let Some(pending) = pending.as_deref_mut()
            {
                pending.push(&attribution::Pending {
                    bytenr: extent.bytenr,
                    refs: extent.refs.clone(),
                    owners: extent.owners.clone(),
                })?;
            }
            let part = attribute_extent(&ExtentInput {
                bytes: extent.refs.num_bytes,
                refs_complete: extent.refs_complete,
                owners: Some(extent.owners),
                pinned_bytes,
                reclaimable: extent.reclaimable,
            });
            overview.add(&part, extent.refs.num_bytes);
        }
        Ok(())
    }
}

/// What the stages of a run leave for its report.
struct Stages {
    totals: Metrics,
    summary: AccountingSummary,
    resolved: ResolveStats,
    owner_walk: OwnerWalkStats,
    after: SubvolumeSet,
    /// Balance or device replace seen running, or not ruled out.
    maintenance: Vec<ChangeReason>,
    snapshots: Result<Option<SnapshotAnalysis>, String>,
    space: Option<SpaceWindow>,
    overview: Option<crate::overview::OverviewData>,
    recycle_ctimes: Vec<RecycleEntryCtime>,
    deletion: Result<Option<DeletionAnswer>, String>,
}

type JudgeOutcome = (
    AccountingSummary,
    (Metrics, Metrics),
    ResolveStats,
    Option<OverviewData>,
    OwnerWalkStats,
);

/// Stops before the model grows past the memory budget.
fn reserve(control: &mut dyn ScanControl, bytes: u64) -> Result<(), WalkError> {
    match control.boundary().reserve(bytes) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(reason) => Err(WalkError::Stopped(reason)),
    }
}

/// An upper bound of the bytes expanding `roots` of `ns` adds: their
/// subtrees, capped by the whole namespace when it includes the subvolume
/// root. A file-only fragment lacks the ancestor paths needed for that
/// cap's Recycle Bin count.
fn roots_expansion(ns: &Namespace, roots: &[&RootRequest]) -> u64 {
    let subtrees: u64 = roots
        .iter()
        .map(|r| {
            ScopeTree::expansion_bytes(
                ns.subtree_counts_with_recycle(r.ino, path_has_recycle_component(&r.path)),
            )
        })
        .sum();
    if ns.inode(FIRST_FREE_OBJECTID).is_some() {
        subtrees.min(ScopeTree::expansion_bytes(ns.counts()))
    } else {
        subtrees
    }
}

fn path_has_recycle_component(path: &[u8]) -> bool {
    path.split(|&byte| byte == b'/')
        .any(|part| part == b"#recycle")
}

/// Blocker classes of references from outside the scope.
struct ScopeRoots<'a> {
    subvolumes: &'a SubvolumeSet,
    roles: &'a BTreeMap<u64, SubvolumeRole>,
    /// Trees materialized from their root directory.
    complete: &'a BTreeSet<u64>,
}

impl RootClass for ScopeRoots<'_> {
    fn external_root(&self, root: u64) -> Blockers {
        match self.subvolumes.get(root) {
            None => Blockers::UNKNOWN,
            Some(s) if s.pending_deletion => Blockers::PENDING_DELETION,
            Some(s) if s.readonly => Blockers::SNAPSHOT,
            // Every named inode of a complete tree is in the scope.
            Some(_) if self.complete.contains(&root) => Blockers::ORPHAN_INODE,
            Some(_) if self.roles.get(&root).is_some_and(|r| r.is_materialized()) => {
                Blockers::LIVE_REFLINK
            }
            Some(_) => Blockers::OTHER_SUBVOLUME,
        }
    }

    fn shared_leaf(&self, tree: u64) -> Blockers {
        if self.subvolumes.with_snapshot(tree) {
            Blockers::SNAPSHOT
        } else {
            Blockers::SHARED_TREE
        }
    }
}

/// A linked, writable nested subvolume whose unit is still to be built.
struct Attachment {
    parent: usize,
    node: u32,
    subvolume: u64,
    name: Vec<u8>,
    flags: NodeFlags,
}

/// The files of a unit's stored tree, its extent uses and its summary.
const TREE: &str = "tree";
const EXTENTS: &str = "extents";
const SUMMARY: &str = "summary";

/// Bumped when a unit's files would be built differently.
const UNIT_VERSION: &str = concat!("7:", env!("CARGO_PKG_VERSION"));

/// What a unit's tree holds, kept with its sealed files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UnitSummary {
    #[serde(default)]
    tree: u64,
    /// Unix seconds when its namespace was read.
    captured: u64,
    #[serde(default)]
    read_stats: TreeStats,
    /// The subvolume root of an attached unit exists.
    rooted: bool,
    nodes: u64,
    inodes: u64,
    #[serde(default)]
    recycle_ctime_nodes: u64,
    #[serde(default)]
    recycle_ctimes: u64,
    #[serde(default)]
    recycle_ctime_entries: Vec<RecycleEntryCtime>,
    encrypted: u64,
    placeholders: u64,
    anomalies: Anomalies,
    /// Linked writable subvolumes below it.
    attached: Vec<SealedLink>,
    /// Linked read-only subvolumes below it, not read.
    snapshots: Vec<u64>,
    /// Tier-0 duplicate candidate records in its `CANDIDATES` file.
    #[serde(default)]
    candidates: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedLink {
    subvolume: u64,
    node: u32,
    #[serde(with = "hex_bytes")]
    name: Vec<u8>,
    flags: u8,
}

/// A spilled unit.
#[derive(Debug, Clone, Default)]
struct UnitInfo {
    /// The directory of its `TREE` and `EXTENTS`.
    files: PathBuf,
    placement: Placement,
    summary: UnitSummary,
    /// Report records of its fragment, spliced ones excluded.
    records: u64,
}

/// What the units are built from.
struct Selection<'a> {
    roots: &'a [RootRequest],
    marks: &'a Marks,
    snapshots: Option<SnapshotScan>,
    budgets: OverviewBudgets,
    before: &'a SubvolumeSet,
    /// Units record tier-0 duplicate candidates.
    candidates: bool,
}

impl Selection<'_> {
    /// The key of the unit of `tree`: `slot` names which of its scopes,
    /// `boundary` the name and flags an attached unit takes from above.
    fn key(&self, tree: u64, slot: &[u8], boundary: Option<(&[u8], NodeFlags)>) -> Option<UnitKey> {
        let subvolume = self.before.get(tree)?;
        let mut inputs = Fnv::new();
        let mut put = |bytes: &[u8]| {
            inputs.update(&(bytes.len() as u64).to_le_bytes());
            inputs.update(bytes);
        };
        put(UNIT_VERSION.as_bytes());
        put(&[u8::from(self.candidates)]);
        if let Some((name, flags)) = boundary {
            put(name);
            put(&[flags.bits()]);
        }
        let marked = |set: &BTreeSet<(u64, u64)>| -> Vec<u64> {
            set.iter().filter(|m| m.0 == tree).map(|m| m.1).collect()
        };
        for ino in marked(&self.marks.encrypted) {
            put(&ino.to_le_bytes());
        }
        put(b"own");
        for ino in marked(&self.marks.own_state) {
            put(&ino.to_le_bytes());
        }
        for s in &self.before.subvolumes {
            if let Some(link) = s.parent.filter(|l| l.parent_id == tree) {
                put(&s.id.to_le_bytes());
                put(&link.dirid.to_le_bytes());
                put(&s.name);
                put(&[u8::from(s.readonly), u8::from(s.pending_deletion)]);
            }
        }
        let sharing = self.before.leaf_sharing(tree);
        put(&sharing.up_to_generation.to_le_bytes());
        put(&[u8::from(sharing.with_snapshot)]);
        let mut slot_hash = Fnv::new();
        slot_hash.update(slot);
        Some(UnitKey {
            subvolume: tree,
            uuid: subvolume
                .uuid
                .map_or(String::new(), |u| hex_bytes::encode(&u)),
            generation: subvolume.generation,
            readonly: subvolume.readonly,
            slot: slot_hash.value(),
            inputs: inputs.value(),
        })
    }
}

/// The slot of the unit of a tree's selected roots.
fn roots_slot(roots: &[&RootRequest]) -> Vec<u8> {
    let mut slot = b"roots".to_vec();
    for r in roots {
        slot.extend_from_slice(&r.ino.to_le_bytes());
        slot.push(u8::from(r.scope == Scope::Volume));
        slot.extend_from_slice(&(r.path.len() as u64).to_le_bytes());
        slot.extend_from_slice(&r.path);
    }
    slot
}

struct Run {
    spill: Spill,
    units: Vec<UnitInfo>,
    roles: BTreeMap<u64, SubvolumeRole>,
    /// Capture times of reused units.
    captured: BTreeMap<u64, u64>,
    /// Trees materialized from their root directory.
    complete: BTreeSet<u64>,
    stats: ScanStats,
    root_errors: Vec<(Vec<u8>, String)>,
    uncertain_roots: BTreeSet<u64>,
    /// Trees deleted after discovery, whose search failed with ENOENT.
    vanished: BTreeSet<u64>,
    /// Units record tier-0 duplicate candidates.
    candidates: bool,
}

impl Run {
    fn stages<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        selection: &Selection<'_>,
        control: &mut dyn ScanControl,
        cache: &mut dyn UnitCache,
        space_start: Option<SpaceSnapshot>,
    ) -> Result<Stages, WalkError> {
        self.build(source, selection, control, cache)?;
        let (summary, corrections, resolved, overview, owner_walk) =
            self.judge(source, selection, control)?;
        let totals = self.aggregate(corrections, control)?;
        let snapshots = self.analyze_snapshots(
            source,
            selection,
            control,
            cache
                .namespace_checkpoint()
                .map(|p| p.with_extension("snapshots"))
                .as_deref(),
        )?;
        let deletion = self.analyze_deletion(source, selection.before, control)?;
        let after = discover_relocating(source, control.boundary())?;
        let recycle_ctimes = self.collect_recycle_ctimes(control)?;
        let space_end = if selection
            .roots
            .iter()
            .any(|root| root.scope == Scope::Volume)
        {
            source.space_snapshot()
        } else {
            None
        };
        Ok(Stages {
            totals,
            summary,
            resolved,
            owner_walk,
            after,
            maintenance: maintenance_changes(source),
            snapshots,
            space: (space_start.is_some() || space_end.is_some()).then_some(SpaceWindow {
                start: space_start,
                end: space_end,
            }),
            overview,
            recycle_ctimes,
            deletion,
        })
    }

    /// What deleting the requested snapshots with the selection frees.
    /// Only a stop ends the scan; a failure leaves the report without it.
    fn analyze_deletion<S: MetadataSource + ?Sized>(
        &self,
        source: &mut S,
        set: &SubvolumeSet,
        control: &mut dyn ScanControl,
    ) -> Result<Result<Option<DeletionAnswer>, String>, WalkError> {
        let Some(request) = control.deletion() else {
            return Ok(Ok(None));
        };
        let deletion = Deletion {
            snapshots: request.snapshots,
            uses: self.live_uses()?,
        };
        match freed_by_deleting(source, set, &deletion, &request.scan, control.boundary()) {
            Ok(answer) => Ok(Ok(Some(answer))),
            Err(stop @ WalkError::Stopped(_)) => Err(stop),
            Err(e) => Ok(Err(e.to_string())),
        }
    }

    /// The extent references of selected inodes that lose every name.
    fn live_uses(&self) -> io::Result<Vec<LiveUse>> {
        let extents = self.units.iter().map(|u| u.files.join(EXTENTS)).collect();
        let mut merge = Merge::open(extents)?;
        let mut uses = Vec::new();
        for group in merge.by_ref() {
            let deleted = group.uses.iter().map(|&(_, u)| u.holder);
            uses.extend(deleted.filter(|h| !h.external_names).map(|h| LiveUse {
                bytenr: group.bytenr,
                tree: h.tree,
                ino: h.objectid,
            }));
        }
        merge.finish()?;
        Ok(uses)
    }

    /// Reads the snapshots of every materialized writable subvolume. Only
    /// a stop ends the scan; a failure leaves the report without them.
    fn analyze_snapshots<S: MetadataSource + ?Sized>(
        &self,
        source: &mut S,
        selection: &Selection<'_>,
        control: &mut dyn ScanControl,
        checkpoint: Option<&Path>,
    ) -> Result<Result<Option<SnapshotAnalysis>, String>, WalkError> {
        let set = selection.before;
        let lives: Vec<u64> = self
            .roles
            .iter()
            .filter(|(_, role)| role.is_materialized())
            .filter(|&(&id, _)| set.get(id).is_some_and(|s| !s.readonly))
            .map(|(&id, _)| id)
            .collect();
        let Some(scan) = selection.snapshots else {
            return match inventory(source, set, &lives, control.boundary()) {
                Ok(analysis) => Ok(Ok(Some(analysis))),
                Err(stop @ WalkError::Stopped(_)) => Err(stop),
                Err(e) => Ok(Err(e.to_string())),
            };
        };
        control.event(ScanEvent::AnalyzingSnapshots {
            shares: lives.len(),
        });
        let request = Request {
            lives,
            depth: scan.depth,
            owner_budget: scan.owner_budget,
            global_owner_budget: scan.global_owner_budget,
            held_files_budget: scan.held_files_budget,
        };
        match analyze_checkpointed(
            source,
            set,
            &request,
            control.boundary(),
            self.spill.dir(),
            checkpoint,
        ) {
            Ok(analysis) => Ok(Ok(Some(analysis))),
            Err(stop @ WalkError::Stopped(_)) => Err(stop),
            Err(e) => Ok(Err(e.to_string())),
        }
    }

    /// What the units read and spilled so far hold.
    fn partial_stats(&self) -> ScanStats {
        let scanned = self
            .roles
            .values()
            .filter(|&&r| r == SubvolumeRole::Scanned);
        ScanStats {
            subvolumes_scanned: scanned.count() as u64,
            entries: self.entries(),
            inodes: self.units.iter().map(|u| u.summary.inodes).sum(),
            ..self.stats.clone()
        }
    }

    fn path(&self, unit: usize, name: &str) -> PathBuf {
        self.spill.path(&part(unit, name))
    }

    /// Materializes and spills one unit per tree of selected roots, then
    /// one per linked writable subvolume below them, reusing sealed
    /// units that stand in for them.
    fn build<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        selection: &Selection<'_>,
        control: &mut dyn ScanControl,
        cache: &mut dyn UnitCache,
    ) -> Result<(), WalkError> {
        let before = selection.before;
        let mut by_tree: BTreeMap<u64, Vec<&RootRequest>> = BTreeMap::new();
        for root in selection.roots {
            by_tree.entry(root.tree).or_default().push(root);
        }
        let mut queue = VecDeque::new();
        for (&tree_id, roots) in &by_tree {
            let key = selection.key(tree_id, &roots_slot(roots), None);
            let whole = roots.iter().any(|r| r.ino == FIRST_FREE_OBJECTID);
            if self.reuse(cache, key.as_ref(), None, &mut queue, control)? {
                if whole {
                    self.complete.insert(tree_id);
                }
                continue;
            }
            let captured = cache.now();
            let Some(ns) = self.read_surviving(
                source,
                tree_id,
                Some(roots),
                before,
                control,
                (cache, key.as_ref()),
            )?
            else {
                self.root_errors.extend(roots.iter().map(|root| {
                    (
                        root.path.clone(),
                        "subvolume deleted during the scan".into(),
                    )
                }));
                continue;
            };
            reserve(control, roots_expansion(&ns, roots))?;
            let mut tree = ScopeTree::new();
            let mut pending = Vec::new();
            let errors = self.root_errors.len();
            for root in roots {
                let kind = match root.scope {
                    Scope::Volume => RootKind::Volume,
                    Scope::Path => RootKind::Path,
                };
                match tree.add_root(&ns, root.ino, &root.path, kind, before, selection.marks) {
                    Ok(more) => {
                        if root.ino == FIRST_FREE_OBJECTID {
                            self.complete.insert(tree_id);
                        }
                        pending.extend(more);
                    }
                    Err(e) => {
                        self.uncertain_roots.insert(tree_id);
                        self.root_errors.push((root.path.clone(), e.to_string()));
                    }
                }
            }
            let read_stats = ns.stats().clone();
            drop(ns);
            let unit = self.store((tree_id, tree), None, pending, before, captured, &mut queue)?;
            self.units[unit].summary.read_stats = read_stats;
            self.save_summary(unit)?;
            // A scope with a missing root is partial: never sealed.
            if let Some(key) = key.filter(|_| self.root_errors.len() == errors) {
                self.seal(cache, &key, unit)?;
            }
        }
        while let Some(a) = queue.pop_front() {
            let key = selection.key(a.subvolume, b"attached", Some((&a.name, a.flags)));
            self.complete.insert(a.subvolume);
            let parent = Some((a.parent, a.node));
            if self.reuse(cache, key.as_ref(), parent, &mut queue, control)? {
                continue;
            }
            let captured = cache.now();
            let Some(ns) = self.read_surviving(
                source,
                a.subvolume,
                None,
                before,
                control,
                (cache, key.as_ref()),
            )?
            else {
                self.complete.remove(&a.subvolume);
                continue;
            };
            reserve(
                control,
                ScopeTree::expansion_bytes(ns.subtree_counts_with_recycle(
                    FIRST_FREE_OBJECTID,
                    a.flags.contains(NodeFlags::RECYCLE),
                )),
            )?;
            let mut tree = ScopeTree::new();
            let marks = selection.marks;
            let pending = tree.add_subvolume_root(&ns, &a.name, a.flags, before, marks);
            let read_stats = ns.stats().clone();
            drop(ns);
            let unit = self.store(
                (a.subvolume, tree),
                parent,
                pending,
                before,
                captured,
                &mut queue,
            )?;
            self.units[unit].summary.read_stats = read_stats;
            self.save_summary(unit)?;
            if let Some(key) = key {
                self.seal(cache, &key, unit)?;
            }
        }
        Ok(())
    }

    /// Takes the sealed unit of `key` as the next unit, if the cache has
    /// one that may stand in for it.
    fn reuse(
        &mut self,
        cache: &mut dyn UnitCache,
        key: Option<&UnitKey>,
        parent: Option<(usize, u32)>,
        queue: &mut VecDeque<Attachment>,
        control: &mut dyn ScanControl,
    ) -> Result<bool, WalkError> {
        let Some(key) = key else {
            return Ok(false);
        };
        let Some(files) = cache.find_checked(key)? else {
            return Ok(false);
        };
        let Some(summary) = fs::read(files.join(SUMMARY))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<UnitSummary>(&bytes).ok())
        else {
            return Ok(false);
        };
        self.roles.insert(key.subvolume, SubvolumeRole::Reused);
        self.captured.insert(key.subvolume, summary.captured);
        self.stats.subvolumes_reused += 1;
        if cache.namespace_checkpoint().is_some() {
            self.stats.items += summary.read_stats.walk.items;
            self.stats.batches += summary.read_stats.walk.batches;
            self.stats.item_bytes += summary.read_stats.walk.item_bytes;
        }
        control.event(ScanEvent::UnitReused {
            tree: key.subvolume,
            stats: &summary.read_stats,
        });
        self.push(files, parent, summary, queue);
        Ok(true)
    }

    /// Adds a unit, queueing the writable subvolumes linked in it and
    /// noting the read-only ones as skipped snapshots.
    fn push(
        &mut self,
        files: PathBuf,
        parent: Option<(usize, u32)>,
        summary: UnitSummary,
        queue: &mut VecDeque<Attachment>,
    ) -> usize {
        let unit = self.units.len();
        for &id in &summary.snapshots {
            self.roles.insert(id, SubvolumeRole::SnapshotSkipped);
        }
        for link in &summary.attached {
            queue.push_back(Attachment {
                parent: unit,
                node: link.node,
                subvolume: link.subvolume,
                name: link.name.clone(),
                flags: NodeFlags::from_bits(link.flags),
            });
        }
        self.units.push(UnitInfo {
            files,
            placement: Placement { parent },
            summary,
            records: 0,
        });
        unit
    }

    /// Where the unit read next spills its extent refs until it is
    /// stored.
    fn refs_path(&self) -> PathBuf {
        self.spill.path(&format!("{}.refs", self.units.len()))
    }

    fn save_summary(&self, unit: usize) -> io::Result<()> {
        let info = &self.units[unit];
        serde_json::to_writer(File::create(info.files.join(SUMMARY))?, &info.summary)
            .map_err(io::Error::other)
    }

    /// Seals `unit`'s files; they stay where the cache puts them.
    fn seal(
        &mut self,
        cache: &mut dyn UnitCache,
        key: &UnitKey,
        unit: usize,
    ) -> Result<(), WalkError> {
        let info = &mut self.units[unit];
        info.files = cache.seal(key, info.summary.captured, &info.files)?;
        Ok(())
    }

    /// `read`, except that a tree the kernel no longer has (ENOENT: the
    /// subvolume was deleted after discovery) is noted as vanished and
    /// uncertain instead of failing the scan.
    fn read_surviving<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        tree_id: u64,
        roots: Option<&[&RootRequest]>,
        subvolumes: &SubvolumeSet,
        control: &mut dyn ScanControl,
        checkpoint: (&dyn UnitCache, Option<&UnitKey>),
    ) -> Result<Option<Namespace>, WalkError> {
        match self.read(source, tree_id, roots, subvolumes, control, checkpoint) {
            Err(WalkError::Source(SourceError::Io(e))) if e.kind() == io::ErrorKind::NotFound => {
                self.vanished.insert(tree_id);
                self.uncertain_roots.insert(tree_id);
                Ok(None)
            }
            read => read.map(Some),
        }
    }

    fn read<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        tree_id: u64,
        roots: Option<&[&RootRequest]>,
        subvolumes: &SubvolumeSet,
        control: &mut dyn ScanControl,
        checkpoint: (&dyn UnitCache, Option<&UnitKey>),
    ) -> Result<Namespace, WalkError> {
        let (cache, key) = checkpoint;
        control.event(ScanEvent::TreeStarted {
            tree: tree_id,
            name: subvolumes
                .get(tree_id)
                .map(|s| s.name.as_slice())
                .unwrap_or_default(),
        });
        if let Some(key) = key
            && cache
                .checkpoint_key(tree_id)
                .is_some_and(|saved| saved != *key)
        {
            control.event(ScanEvent::UnitRestarted { tree: tree_id });
        }
        let sharing = subvolumes.leaf_sharing(tree_id);
        let refs = self.refs_path();
        let mut tally = Tally {
            boundary: control.boundary(),
            taken: WalkStats::default(),
        };
        let checkpoint = cache
            .namespace_checkpoint_for(tree_id)
            .filter(|_| roots.is_none_or(|roots| roots.iter().any(|r| r.scope == Scope::Volume)));
        let ns = if let (Some(checkpoint), Some(key)) = (&checkpoint, key) {
            let persistent_refs = checkpoint.with_extension("refs");
            let ns = Namespace::read_checkpointed(
                source,
                tree_id,
                sharing,
                &mut tally,
                persistent_refs.clone(),
                checkpoint,
                key,
            );
            if ns.is_ok() {
                fs::rename(&persistent_refs, &refs)?;
            }
            ns
        } else if let Some(roots) =
            roots.filter(|roots| roots.iter().all(|r| r.scope == Scope::Path))
        {
            Namespace::read_file_roots_spilling(
                source,
                tree_id,
                sharing,
                &mut tally,
                refs,
                roots.iter().map(|r| r.ino),
            )
        } else {
            Namespace::read_spilling(source, tree_id, sharing, &mut tally, refs)
        };
        let walk = tally.taken;
        self.stats.items += walk.items;
        self.stats.batches += walk.batches;
        self.stats.item_bytes += walk.item_bytes;
        let ns = ns?;
        self.roles.insert(tree_id, SubvolumeRole::Scanned);
        control.event(ScanEvent::TreeRead {
            tree: tree_id,
            stats: ns.stats(),
        });
        Ok(ns)
    }

    /// Spills `tree`, read at `captured`, as the next unit; linked
    /// read-only subvolumes are marked as snapshots.
    fn store(
        &mut self,
        selection: (u64, ScopeTree),
        parent: Option<(usize, u32)>,
        pending: Vec<Pending>,
        subvolumes: &SubvolumeSet,
        captured: u64,
        queue: &mut VecDeque<Attachment>,
    ) -> io::Result<usize> {
        let (tree_id, mut tree) = selection;
        let mut summary = UnitSummary {
            tree: tree_id,
            captured,
            ..UnitSummary::default()
        };
        for p in pending {
            if subvolumes.get(p.subvolume).is_some_and(|s| s.readonly) {
                summary.snapshots.push(p.subvolume);
                tree.mark(p.node, NodeFlags::SNAPSHOT);
            } else {
                summary.attached.push(SealedLink {
                    subvolume: p.subvolume,
                    node: p.node,
                    name: tree.name(p.node).to_vec(),
                    flags: tree.node(p.node).flags.bits(),
                });
            }
        }
        tree.prepare();
        summary.recycle_ctime_nodes = tree.recycle_ctimes().len() as u64;
        summary.recycle_ctime_entries = tree.take_recycle_ctimes();
        summary.recycle_ctimes = summary.recycle_ctime_entries.len() as u64;
        let files = self.spill.path(&self.units.len().to_string());
        fs::create_dir(&files)?;
        let mut out = BufWriter::new(File::create(files.join(TREE))?);
        tree.store(&mut out)?;
        out.flush()?;
        let mut sorter = Sorter::new(files.clone(), RUN_USES);
        let refs = self.refs_path();
        let mut candidates = self
            .candidates
            .then(|| UnitWriter::create(&files.join(CANDIDATES)))
            .transpose()?;
        tree.spilled_uses(SpilledRefs::new(refs.clone()), |u| {
            if let (Some(out), Some(inode)) = (&mut candidates, tree.inode(u.extent.inode)) {
                out.extent(inode, &u.extent)?;
            }
            sorter.push(u)
        })?;
        sorter.finish(files.join(EXTENTS))?;
        summary.candidates = candidates.map(UnitWriter::finish).transpose()?.unwrap_or(0);
        fs::remove_file(refs)?;
        summary.rooted = !tree.roots().is_empty();
        summary.nodes = tree.len() as u64;
        summary.inodes = tree.inode_count() as u64;
        summary.encrypted = tree.encrypted().len() as u64;
        summary.placeholders = (0..tree.len() as u32)
            .filter(|&n| tree.node(n).flags.contains(NodeFlags::PLACEHOLDER))
            .count() as u64;
        summary.anomalies = tree.anomalies().clone();
        let json = serde_json::to_vec(&summary).map_err(io::Error::other)?;
        fs::write(files.join(SUMMARY), json)?;
        Ok(self.push(files, parent, summary, queue))
    }

    /// Judges every extent the units hold against the extent tree, in
    /// one merged pass, and routes the judgements to the units.
    fn judge<S: MetadataSource + ?Sized>(
        &self,
        source: &mut S,
        selection: &Selection<'_>,
        control: &mut dyn ScanControl,
    ) -> Result<JudgeOutcome, WalkError> {
        let (roots, before, budgets) = (selection.roots, selection.before, &selection.budgets);
        let classes = ScopeRoots {
            subvolumes: before,
            roles: &self.roles,
            complete: &self.complete,
        };
        let extents: Vec<PathBuf> = (0..self.units.len())
            .map(|u| self.units[u].files.join(EXTENTS))
            .collect();
        let volume = roots.iter().any(|r| r.scope == Scope::Volume);
        let plan = Plan::Coalesce {
            max_gap: COALESCE_GAP,
        };
        let mut count = 0u64;
        let mut first = Merge::open(extents.clone())?;
        let ranges = if volume {
            count = first.by_ref().count() as u64;
            Vec::new()
        } else {
            ranges(
                first.by_ref().inspect(|_| count += 1).map(|g| g.bytenr),
                plan,
            )
        };
        first.finish()?;
        control.event(ScanEvent::Resolving { extents: count });
        let placement = self.units.iter().map(|u| u.placement).collect();
        let judged = (0..self.units.len())
            .map(|u| self.path(u, "judged"))
            .collect();
        let mut router = Router::new(placement, judged);
        let mut summary = AccountingSummary::default();
        let mut failed = None;
        let mut merge = Merge::open(extents)?;
        let (resolved, overview, owner_stats) = if volume {
            let root = roots
                .iter()
                .find(|root| root.scope == Scope::Volume)
                .expect("volume scan has a volume root");
            let mut owners = owners::OwnerIndex::build(
                &self.units,
                before,
                &root.path,
                root.tree,
                self.spill.dir(),
                control.boundary(),
            )?;
            owners.state.roots = selection.marks.own_state.clone();
            let mut overview = OverviewData {
                share_names_hex: owners
                    .share_names()
                    .into_iter()
                    .map(|(id, name)| (id, hex_bytes::encode(&name)))
                    .collect(),
                pending_roots: Some(pending_roots(before)),
                recycle_ctime: Some(Default::default()),
                ..OverviewData::default()
            };
            let plan = crate::btrfs::read_budget::ReadBudget::select(
                budgets.pinned_reads,
                budgets.used_bytes,
                count,
                before.subvolumes.iter().filter(|s| s.readonly).count() as u64,
                0,
            );
            let mut pending = budgets
                .pending
                .as_deref()
                .map(attribution::Capture::new)
                .transpose()?;
            let mut owner_walk = OwnerWalk::new(budgets.owner);
            let (mut leaves, pinned_reader_unavailable) =
                match source.block_reader(control.boundary())? {
                    Ok(reader) => {
                        let leaves = LeafRanges::new(reader, plan.selected_reads)
                            .with_threads(budgets.leaf_reads.threads)
                            .with_cache_leaves(budgets.leaf_reads.cache_leaves);
                        (Some(leaves), None)
                    }
                    Err(why) => (None, Some(why)),
                };
            let mut held = HeldExtents::new(budgets.leaf_reads.window);
            let mut next_group = merge.next();
            let resolved = stream_data(
                source,
                control.boundary(),
                |source, bytenr, refs, boundary| {
                    while next_group
                        .as_ref()
                        .is_some_and(|group| group.bytenr < bytenr)
                    {
                        let group = next_group.take().expect("checked");
                        route_group(
                            &group,
                            None,
                            &classes,
                            &mut summary,
                            &mut router,
                            &mut failed,
                        );
                        next_group = merge.next();
                    }
                    let group = if next_group
                        .as_ref()
                        .is_some_and(|group| group.bytenr == bytenr)
                    {
                        let group = next_group.take();
                        next_group = merge.next();
                        group
                    } else {
                        None
                    };
                    let reclaimable = if let Some(group) = &group {
                        route_group(
                            group,
                            refs,
                            &classes,
                            &mut summary,
                            &mut router,
                            &mut failed,
                        )
                    } else {
                        false
                    };
                    if let Some(refs) = refs {
                        let (extent_owners, refs_complete) = overview_owners(
                            source,
                            bytenr,
                            refs,
                            &mut owners,
                            before,
                            &mut owner_walk,
                            leaves.as_mut(),
                            boundary,
                        )?;
                        let owned = refs_complete && owners_resolved(&extent_owners);
                        held.push(HeldExtent {
                            bytenr,
                            group,
                            refs: refs.clone(),
                            owners: extent_owners,
                            refs_complete,
                            owned,
                            reclaimable,
                        });
                        if held.full() {
                            held.flush(
                                source,
                                leaves.as_mut(),
                                &mut overview,
                                pending.as_mut(),
                                boundary,
                            )?;
                        }
                    }
                    Ok(())
                },
            )?;
            held.flush(
                source,
                leaves.as_mut(),
                &mut overview,
                pending.as_mut(),
                control.boundary(),
            )?;
            if let Some(pending) = pending {
                pending.finish(before, &overview, plan.clone())?;
            }
            while let Some(group) = next_group {
                route_group(
                    &group,
                    None,
                    &classes,
                    &mut summary,
                    &mut router,
                    &mut failed,
                );
                next_group = merge.next();
            }
            merge.finish()?;
            let mut resolved = resolved;
            resolved.ranges += owner_walk.lookups;
            resolved.batches += owner_walk.stats.batches;
            resolved.items += owner_walk.stats.items;
            resolved.item_bytes += owner_walk.stats.item_bytes;
            let pinned_reads = leaves.map(|l| l.stats).unwrap_or_default();
            resolved.batches += pinned_reads.walk.batches;
            resolved.items += pinned_reads.walk.items;
            resolved.item_bytes += pinned_reads.walk.item_bytes;
            let owner_stats = OwnerWalkStats {
                lookups: owner_walk.lookups,
                budget_capped_extents: owner_walk.budget_capped_extents,
                pinned_reads,
                pinned_reader_unavailable,
                plan: Some(plan),
            };
            (resolved, Some(overview), owner_stats)
        } else {
            let resolved = resolve_in(
                source,
                &ranges,
                merge.by_ref(),
                |g: &Group| g.bytenr,
                control.boundary(),
                |group, refs| {
                    route_group(
                        &group,
                        refs,
                        &classes,
                        &mut summary,
                        &mut router,
                        &mut failed,
                    );
                },
            )?;
            merge.finish()?;
            (resolved, None, OwnerWalkStats::default())
        };
        if let Some(e) = failed {
            return Err(e.into());
        }
        let corrections = (router.added, router.removed);
        router.finish()?;
        Ok((summary, corrections, resolved, overview, owner_stats))
    }

    /// Aggregates each unit from its judgements, children first, into
    /// its report fragment. Returns the filesystem totals.
    fn aggregate(
        &mut self,
        (added, removed): (Metrics, Metrics),
        control: &mut dyn ScanControl,
    ) -> Result<Metrics, WalkError> {
        control.event(ScanEvent::Aggregating {
            nodes: self.entries() as usize + 1,
        });
        let mut children: Vec<Vec<(u32, usize)>> = vec![Vec::new(); self.units.len()];
        for (unit, info) in self.units.iter().enumerate() {
            if let Some((parent, node)) = info.placement.parent {
                children[parent].push((node, unit));
            }
        }
        let mut nested: Vec<Option<Nested>> = vec![None; self.units.len()];
        let mut totals = Metrics::default();
        for unit in (0..self.units.len()).rev() {
            let tree = ScopeTree::load(&fs::read(self.units[unit].files.join(TREE))?)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            reserve(control, Accountant::bytes_needed(&tree))?;
            let mut accountant = Accountant::new(&tree, None);
            let mut spliced = HashMap::new();
            for &(node, child) in &children[unit] {
                if let Some(n) = &nested[child] {
                    accountant.nest(node, n);
                    spliced.insert(node, child);
                }
            }
            let mut judged = JudgedReader::new(self.path(unit, "judged"));
            while let Some(j) = judged.read()? {
                let at = Attribution {
                    judgement: j.judgement,
                    inodes: &j.inodes,
                    children: &j.children,
                    owner: j.owner,
                };
                accountant.attribute(j.bytenr, &at);
            }
            let aggregate = accountant.finish();
            match self.units[unit].placement.parent {
                Some(_) => nested[unit] = tree.roots().first().map(|&r| aggregate.nested(r)),
                None => totals.add(&aggregate.metrics[VIRTUAL_ROOT as usize]),
            }
            let path = self.path(unit, "fragment");
            self.units[unit].records = write_fragment(path, &tree, &aggregate, &spliced)?;
        }
        totals.add(&added);
        totals.sub(&removed);
        Ok(totals)
    }

    /// Nodes below the virtual root, each boundary once: a rooted
    /// attached unit repeats its boundary node as its root.
    fn entries(&self) -> u64 {
        let repeated = self
            .units
            .iter()
            .filter(|u| u.placement.parent.is_some() && u.summary.rooted)
            .count() as u64;
        let nodes = self.units.iter().map(|u| u.summary.nodes - 1);
        nodes.sum::<u64>() - repeated
    }

    fn collect_recycle_ctimes(
        &mut self,
        control: &mut dyn ScanControl,
    ) -> Result<Vec<RecycleEntryCtime>, WalkError> {
        let count = self
            .units
            .iter()
            .try_fold(0usize, |count, unit| {
                count.checked_add(unit.summary.recycle_ctime_entries.len())
            })
            .ok_or(WalkError::Capacity("Recycle Bin ctime report"))?;
        reserve(
            control,
            (count as u64).saturating_mul(std::mem::size_of::<RecycleEntryCtime>() as u64),
        )?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| WalkError::Capacity("Recycle Bin ctime report"))?;
        for unit in &mut self.units {
            entries.append(&mut unit.summary.recycle_ctime_entries);
        }
        entries.sort_unstable_by(|a, b| {
            (a.tree, a.inode)
                .cmp(&(b.tree, b.inode))
                .then_with(|| b.ctime.cmp(&a.ctime))
        });
        entries.dedup_by(|a, b| a.tree == b.tree && a.inode == b.inode);
        Ok(entries)
    }

    fn finish(
        self,
        done: Stages,
        before: &SubvolumeSet,
        state_dir_writes: Vec<StateDirWrites>,
    ) -> ScanOutcome {
        let Stages {
            totals,
            summary,
            resolved,
            owner_walk,
            after,
            maintenance,
            snapshots,
            space,
            overview,
            recycle_ctimes,
            deletion,
        } = done;
        let (snapshots, snapshot_error) = match snapshots {
            Ok(analysis) => (analysis, None),
            Err(e) => (None, Some(e)),
        };
        let (deletion, deletion_error) = match deletion {
            Ok(answer) => (answer, None),
            Err(e) => (None, Some(e)),
        };
        let analyzed: BTreeSet<u64> = snapshots
            .iter()
            .flat_map(|a| a.shares.iter())
            .flat_map(|s| s.materialized().map(|x| x.id))
            .collect();
        // Snapshots deleted with the selection count as selected too.
        let read: BTreeSet<u64> = deletion
            .iter()
            .flat_map(|d| d.snapshots.iter().copied())
            .chain(analyzed.iter().copied())
            .collect();
        let subvolumes: Vec<SubvolumeState> = self
            .roles
            .iter()
            .map(|(&id, &role)| SubvolumeState {
                id,
                role,
                generation_before: before.get(id).map_or(0, |s| s.generation),
                generation_after: after.get(id).map(|s| s.generation),
                captured: self.captured.get(&id).copied(),
                uuid: after
                    .get(id)
                    .and_then(|s| s.uuid)
                    .map(|u| hex_bytes::encode(&u)),
                last_snapshot: after.get(id).map(|s| s.last_snapshot),
                snapshots: after.snapshots_of(id),
            })
            .collect();
        let mut anomalies = Anomalies::default();
        for u in &self.units {
            anomalies.add(&u.summary.anomalies);
        }
        let mut namespace_uncertain = self.uncertain_roots.clone();
        for unit in &self.units {
            let summary = &unit.summary;
            if summary.anomalies.total() > 0 || summary.encrypted > 0 || summary.placeholders > 0 {
                namespace_uncertain.insert(summary.tree);
            }
        }
        let overview_unknown = overview.as_ref().map_or(0, |data| data.unknown_extents);
        let mut coverage = Coverage {
            approximation: None,
            encrypted_directories: self.units.iter().map(|u| u.summary.encrypted).sum(),
            namespace_uncertain_subvolumes: (!namespace_uncertain.contains(&0))
                .then(|| namespace_uncertain.into_iter().collect()),
            snapshots_not_materialized: subvolumes
                .iter()
                .filter(|s| s.role == SubvolumeRole::SnapshotSkipped && !analyzed.contains(&s.id))
                .count() as u64,
            snapshots_materialized: analyzed.len() as u64,
            unlinked_subvolume_entries: self.units.iter().map(|u| u.summary.placeholders).sum(),
            dangling_entries: anomalies.dangling_entries,
            cycles: anomalies.cycles,
            unsupported_locations: anomalies.unsupported_locations,
            missing_subvolume_roots: anomalies.missing_subvolume_roots,
            unreadable_roots: Some(self.root_errors.len() as u64),
            reference_accounting: if summary.unknown_extents == 0 && overview_unknown == 0 {
                "complete".into()
            } else {
                "partial".into()
            },
            unresolved_extents: summary.unknown_extents.max(overview_unknown),
        };
        let partial = !self.root_errors.is_empty()
            || anomalies.total() > 0
            || coverage.encrypted_directories > 0;
        let mut change_reasons = [
            subvolume_changes(before, &after, &self.roles, &read),
            relocation_changes(before, &after),
            maintenance,
        ]
        .concat();
        change_reasons.extend(
            self.vanished
                .iter()
                .map(|&id| ChangeReason::SubvolumeVanished {
                    subvolume_id: id,
                    generation_before: before.get(id).map_or(0, |s| s.generation),
                }),
        );
        change_reasons.retain(|reason| !own_writes::explained(reason, &state_dir_writes));
        // Keep consistency and namespace evidence intact for certification.
        // Only the read's completion policy changes; real errors still abort.
        if anomalies.cycles == 0
            && anomalies.unsupported_locations == 0
            && coverage.encrypted_directories == 0
            && snapshot_error.is_none()
            && deletion_error.is_none()
            && (partial
                || !change_reasons.is_empty()
                || coverage.unresolved_extents > 0
                || coverage.snapshots_not_materialized > 0)
        {
            let changed: BTreeSet<u64> = change_reasons
                .iter()
                .filter_map(|reason| match reason {
                    ChangeReason::SubvolumeGenerationChanged { subvolume_id, .. }
                    | ChangeReason::SubvolumeVanished { subvolume_id, .. }
                    | ChangeReason::SubvolumeAppeared { subvolume_id, .. }
                    | ChangeReason::SubvolumeSnapshotted { subvolume_id, .. }
                    | ChangeReason::SubvolumeUuidChanged { subvolume_id } => Some(*subvolume_id),
                    _ => None,
                })
                .collect();
            let overview_bytes = overview.as_ref().map_or(0, |data| {
                data.unknown.values().copied().fold(0, u64::saturating_add)
            });
            coverage.approximation = Some(crate::report::Approximation {
                changed_subvolumes: changed.len() as u64,
                skipped_roots: self.root_errors.len() as u64,
                unaccounted_bytes: totals.unknown.max(overview_bytes),
            });
        }
        let consistency = if partial {
            Consistency::Partial
        } else if !change_reasons.is_empty() {
            Consistency::ChangedDuringScan
        } else {
            Consistency::ObservedStable
        };
        let stats = ScanStats {
            subvolumes_scanned: count(&subvolumes, SubvolumeRole::Scanned),
            extents: totals.extents,
            items: self.stats.items + resolved.items,
            batches: self.stats.batches + resolved.batches,
            item_bytes: self.stats.item_bytes + resolved.item_bytes,
            reference_searches: resolved.ranges,
            reference_items: resolved.items,
            owner_lookups: owner_walk.lookups,
            owner_budget_capped_extents: owner_walk.budget_capped_extents,
            pinned_read_plan: owner_walk.plan.clone(),
            pinned_leaf_reads: owner_walk.pinned_reads.leaf_reads,
            pinned_leaf_read_bytes: owner_walk.pinned_reads.bytes_read,
            pinned_leaf_cache_hits: owner_walk.pinned_reads.cache_hits,
            pinned_root_searches: owner_walk.pinned_reads.direct_searches,
            pinned_leaf_reader_unavailable: owner_walk.pinned_reader_unavailable.clone(),
            ..self.partial_stats()
        };
        let candidates = self
            .units
            .iter()
            .map(|u| (u.summary.tree, u.files.join(CANDIDATES)))
            .collect();
        let nodes = ReportNodes::Spilled {
            tops: (0..self.units.len())
                .filter(|&u| self.units[u].placement.parent.is_none())
                .collect(),
            count: self.units.iter().map(|u| u.records).sum(),
            spill: self.spill,
        };
        ScanOutcome {
            stopped: None,
            allocated_overview: None,
            totals,
            nodes,
            accounting: summary,
            subvolumes,
            coverage,
            stats,
            consistency,
            change_reasons,
            state_dir_writes,
            root_errors: self.root_errors,
            snapshots,
            snapshot_error,
            space,
            overview,
            recycle_ctimes,
            deletion,
            deletion_error,
            candidates,
        }
    }
}

/// Subvolumes awaiting the Btrfs cleaner, ascending.
fn pending_roots(subvolumes: &SubvolumeSet) -> Vec<u64> {
    subvolumes
        .subvolumes
        .iter()
        .filter(|s| s.pending_deletion)
        .map(|s| s.id)
        .collect()
}

/// The subvolume changes that make the scan `changed-during-scan`
/// (SPEC 13): a selected subvolume changed or vanished, a writable one
/// that could reflink a candidate extent changed, or a new one, which
/// could share one, appeared. Snapshots in `analyzed` count as selected.
fn subvolume_changes(
    before: &SubvolumeSet,
    after: &SubvolumeSet,
    roles: &BTreeMap<u64, SubvolumeRole>,
    analyzed: &BTreeSet<u64>,
) -> Vec<ChangeReason> {
    let selected =
        |id: u64| analyzed.contains(&id) || roles.get(&id).is_some_and(|r| r.is_materialized());
    let changed = before.subvolumes.iter().filter_map(|b| {
        let selected = selected(b.id);
        match after.get(b.id) {
            None => selected.then_some(ChangeReason::SubvolumeVanished {
                subvolume_id: b.id,
                generation_before: b.generation,
            }),
            Some(a) => (a.generation != b.generation && (selected || !a.readonly)).then_some(
                ChangeReason::SubvolumeGenerationChanged {
                    subvolume_id: b.id,
                    selected,
                    generation_before: b.generation,
                    generation_after: a.generation,
                },
            ),
        }
    });
    let appeared = after
        .subvolumes
        .iter()
        .filter(|a| before.get(a.id).is_none())
        .map(|a| ChangeReason::SubvolumeAppeared {
            subvolume_id: a.id,
            generation: a.generation,
            has_uuid: a.uuid.is_some(),
            snapshot_of: a
                .parent_uuid
                .and_then(|p| subvolume_with_uuid(before, after, p)),
        });
    let snapshotted = before
        .subvolumes
        .iter()
        .filter(|b| selected(b.id))
        .filter_map(|b| after.get(b.id).map(|a| (b, a)))
        .filter_map(|(b, a)| snapshot_change(b, a));
    changed.chain(appeared).chain(snapshotted).collect()
}

/// How a selected subvolume's snapshot state moved between discoveries.
fn snapshot_change(before: &Subvolume, after: &Subvolume) -> Option<ChangeReason> {
    if before.uuid != after.uuid {
        return Some(ChangeReason::SubvolumeUuidChanged {
            subvolume_id: before.id,
        });
    }
    (before.last_snapshot != after.last_snapshot).then_some(ChangeReason::SubvolumeSnapshotted {
        subvolume_id: before.id,
        last_snapshot_before: before.last_snapshot,
        last_snapshot_after: after.last_snapshot,
    })
}

/// The subvolume either discovery saw with `uuid`.
fn subvolume_with_uuid(before: &SubvolumeSet, after: &SubvolumeSet, uuid: [u8; 16]) -> Option<u64> {
    [after, before]
        .iter()
        .flat_map(|set| &set.subvolumes)
        .find(|s| s.uuid == Some(uuid))
        .map(|s| s.id)
}

/// Writes a unit's records in preorder; child units with a root are
/// spliced in at their boundaries. Returns the records written.
fn write_fragment(
    path: PathBuf,
    tree: &ScopeTree,
    aggregate: &Aggregate,
    spliced: &HashMap<u32, usize>,
) -> io::Result<u64> {
    let mut fragment = Fragment::new(path);
    for (node, depth) in tree.preorder() {
        match spliced.get(&node) {
            Some(&child) => fragment.splice(child, depth)?,
            None => fragment.record(&tree_record(tree, aggregate.totals(), node, depth))?,
        }
    }
    fragment.finish()
}

fn count(subvolumes: &[SubvolumeState], role: SubvolumeRole) -> u64 {
    subvolumes.iter().filter(|s| s.role == role).count() as u64
}

/// Counts what a tree read took, including a read that stops part way.
struct Tally<'a> {
    boundary: &'a mut dyn BatchBoundary,
    taken: WalkStats,
}

impl BatchBoundary for Tally<'_> {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        self.boundary.after_batch()
    }

    fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
        self.boundary.reserve(bytes)
    }

    fn restored(&mut self, batch: &WalkStats) {
        self.taken.add(batch);
        self.boundary.restored(batch);
    }

    fn took(&mut self, batch: &WalkStats) {
        self.taken.add(batch);
        self.boundary.took(batch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::backref::testing::{encode_data_extent, encode_tree_block};
    use crate::btrfs::backref::{
        BlockBackref, DataBackref, EXTENT_ITEM_KEY, EXTENT_TREE_OBJECTID, METADATA_ITEM_KEY,
    };
    use crate::btrfs::keys::{
        EXTENT_DATA_KEY, FS_TREE_OBJECTID, Key, ROOT_TREE_OBJECTID, TREE_RELOC_OBJECTID,
    };
    use crate::btrfs::search::SearchRange;
    use crate::btrfs::source::{
        BlockGroupSpace, FsSpace, Maintenance, Probe, RawBatch, ReaderResult, SourceError,
        SpaceSnapshot,
    };
    use crate::btrfs::walk::testing::{FakeSource, NoStop};
    use crate::model::accounting::Blockers;
    use crate::model::testing::*;
    use crate::report::testing::header;
    use crate::report::{self, Report};
    use crate::spill::testing::scratch;
    use std::collections::HashMap;
    use std::ops::ControlFlow;

    const MIB: u64 = 1 << 20;

    /// Trees by id; the root tree switches to `root_after` once any
    /// subvolume tree has been searched. `maintenance` answers the
    /// maintenance probes in order, then reports none.
    struct FakeFilesystem {
        root: FakeSource,
        trees_after_namespace: HashMap<u64, FakeSource>,
        root_after: Option<FakeSource>,
        trees: HashMap<u64, FakeSource>,
        maintenance: VecDeque<bool>,
        space: Option<SpaceSnapshot>,
        space_reads: usize,
        /// Tree blocks by address, for one block reader.
        blocks: Option<crate::btrfs::leaf_ranges::testing::FakeBlocks>,
    }

    impl MetadataSource for FakeFilesystem {
        fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            if range.tree_id == EXTENT_TREE_OBJECTID {
                self.trees
                    .extend(std::mem::take(&mut self.trees_after_namespace));
            }
            if range.tree_id == ROOT_TREE_OBJECTID {
                return self.root.search(range);
            }
            if let Some(after) = self.root_after.take() {
                self.root = after;
            }
            self.trees
                .get_mut(&range.tree_id)
                .expect("tree exists")
                .search(range)
        }

        fn maintenance_probes(&mut self) -> Vec<Probe> {
            let running = self.maintenance.pop_front().unwrap_or(false);
            vec![(Maintenance::Balance, Ok(running))]
        }

        fn space_snapshot(&mut self) -> Option<SpaceSnapshot> {
            self.space_reads += 1;
            self.space.clone()
        }

        fn block_reader(&mut self, _boundary: &mut dyn BatchBoundary) -> ReaderResult {
            Ok(match self.blocks.take() {
                Some(blocks) => Ok(Box::new(blocks)),
                None => Err("no blocks".into()),
            })
        }
    }

    struct Recorder {
        stop: NoStop,
        events: Vec<String>,
    }

    impl ScanControl for Recorder {
        fn boundary(&mut self) -> &mut dyn BatchBoundary {
            &mut self.stop
        }

        fn event(&mut self, event: ScanEvent<'_>) {
            self.events.push(match event {
                ScanEvent::Discovered {
                    subvolumes,
                    writable,
                } => format!("discovered {subvolumes} ({writable} writable)"),
                ScanEvent::UnitRestarted { tree } => format!("restarted {tree}"),
                ScanEvent::UnitReused { tree, .. } => format!("reused {tree}"),
                ScanEvent::TreeStarted { tree, .. } => format!("tree {tree}"),
                ScanEvent::TreeRead { tree, stats } => format!("read {tree} {}", stats.inode_items),
                ScanEvent::Aggregating { nodes } => format!("aggregate {nodes}"),
                ScanEvent::Resolving { extents } => format!("resolve {extents}"),
                ScanEvent::AnalyzingSnapshots { shares } => format!("snapshots of {shares}"),
            });
        }
    }

    fn root_tree(generation_of_share: u64) -> Vec<TreeItem> {
        vec![
            root_item(FS_TREE_OBJECTID, 10, false),
            snapshotted_root_item(256, generation_of_share, 0),
            root_backref(256, FS_TREE_OBJECTID, 256, "share"),
            snapshot_root_item(257, 30, 256),
            root_backref(257, FS_TREE_OBJECTID, 258, "snap"),
        ]
    }

    /// Tree 5: `/share` (subvolume 256), `/@sharesnap/snap` (snapshot
    /// 257), `/f` (1 MiB). Tree 256: `/big` (4 MiB).
    fn filesystem(root_after: Option<Vec<TreeItem>>) -> FakeFilesystem {
        let top = vec![
            inode(256, DIR, 0, 1),
            subvolume_entry(256, 2, "share", 256),
            entry(256, 3, "@sharesnap", 258),
            entry(256, 4, "f", 257),
            inode(257, FILE, MIB, 1),
            whole_extent(257, 0, 10 * MIB, MIB),
            inode(258, DIR, 0, 1),
            subvolume_entry(258, 2, "snap", 257),
        ];
        let share = vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "big", 257),
            inode(257, FILE, 4 * MIB, 1),
            whole_extent(257, 0, 20 * MIB, 4 * MIB),
        ];
        // `/f` is exclusive; the snapshot also references `/big`.
        let extents = vec![
            data_extent(10 * MIB, MIB, &[(FS_TREE_OBJECTID, 257)]),
            data_extent(20 * MIB, 4 * MIB, &[(256, 257), (257, 257)]),
            // A data extent whose direct owner is absent from the namespace.
            data_extent(30 * MIB, 8 * MIB, &[(256, 999)]),
        ];
        FakeFilesystem {
            trees_after_namespace: HashMap::new(),
            root: FakeSource::new(root_tree(20), 2),
            root_after: root_after.map(|items| FakeSource::new(items, 2)),
            trees: HashMap::from([
                (FS_TREE_OBJECTID, FakeSource::new(top, 3)),
                (256, FakeSource::new(share, 3)),
                (EXTENT_TREE_OBJECTID, FakeSource::new(extents, 3)),
            ]),
            maintenance: VecDeque::new(),
            space: None,
            space_reads: 0,
            blocks: None,
        }
    }

    fn volume() -> RootRequest {
        RootRequest {
            path: b"/v".to_vec(),
            scope: Scope::Volume,
            tree: FS_TREE_OBJECTID,
            ino: 256,
        }
    }

    fn spill() -> Spill {
        Spill::create(scratch("scan").join("spill")).unwrap()
    }

    /// The report `outcome` writes.
    fn report(outcome: &ScanOutcome) -> Report {
        let mut out = Vec::new();
        report::write_records(&mut out, &header(Vec::new()), outcome.nodes.count(), |w| {
            outcome.nodes.write(w)
        })
        .unwrap();
        Report::parse(out).unwrap()
    }

    /// The report record at `path`, owned.
    fn record(outcome: &ScanOutcome, path: &[u8]) -> Found {
        let report = report(outcome);
        let mut found = None;
        report.for_each(|p, r| {
            if p == path {
                found = Some(Found {
                    flags: r.flags,
                    blockers: r.blockers,
                    metrics: r.metrics,
                });
            }
            ControlFlow::Continue(())
        });
        found.unwrap_or_else(|| panic!("no record {}", String::from_utf8_lossy(path)))
    }

    #[derive(Debug)]
    struct Found {
        flags: NodeFlags,
        blockers: Blockers,
        metrics: Metrics,
    }

    fn scan(fs: &mut FakeFilesystem, roots: &[RootRequest]) -> (ScanOutcome, Vec<String>) {
        scan_cached(fs, roots, &mut NoCache)
    }

    fn scan_cached(
        fs: &mut FakeFilesystem,
        roots: &[RootRequest],
        cache: &mut dyn UnitCache,
    ) -> (ScanOutcome, Vec<String>) {
        scan_marked(fs, roots, &Marks::default(), cache)
    }

    fn scan_marked(
        fs: &mut FakeFilesystem,
        roots: &[RootRequest],
        marks: &Marks,
        cache: &mut dyn UnitCache,
    ) -> (ScanOutcome, Vec<String>) {
        let mut control = Recorder {
            stop: NoStop,
            events: Vec::new(),
        };
        let outcome = scan_filesystem(
            fs,
            roots,
            marks,
            ScanOptions::default(),
            &mut control,
            spill(),
            cache,
        )
        .unwrap();
        (outcome, control.events)
    }

    /// Seals units by moving them to a directory that outlives each run.
    struct DirCache {
        dir: PathBuf,
        sealed: HashMap<UnitKey, PathBuf>,
        now: u64,
    }

    impl DirCache {
        fn new() -> Self {
            DirCache {
                dir: scratch("sealed"),
                sealed: HashMap::new(),
                now: 1000,
            }
        }
    }

    impl UnitCache for DirCache {
        fn now(&self) -> u64 {
            self.now
        }

        fn find(&mut self, key: &UnitKey) -> Option<PathBuf> {
            self.sealed.get(key).cloned()
        }

        fn seal(
            &mut self,
            key: &UnitKey,
            captured: u64,
            staging: &Path,
        ) -> Result<PathBuf, WalkError> {
            assert_eq!(captured, self.now);
            let sealed = self.dir.join(key.name());
            fs::rename(staging, &sealed)?;
            self.sealed.insert(key.clone(), sealed.clone());
            Ok(sealed)
        }
    }

    /// Every report record, rendered.
    fn records(outcome: &ScanOutcome) -> Vec<String> {
        let mut records = Vec::new();
        report(outcome).for_each(|p, r| {
            records.push(format!("{} {r:?}", String::from_utf8_lossy(p)));
            ControlFlow::Continue(())
        });
        records
    }

    fn roles(outcome: &ScanOutcome) -> Vec<(u64, SubvolumeRole)> {
        outcome.subvolumes.iter().map(|s| (s.id, s.role)).collect()
    }

    fn trees_read(events: &[String]) -> Vec<&str> {
        let read = events.iter().filter(|e| e.starts_with("tree "));
        read.map(|e| &e[5..]).collect()
    }

    #[test]
    fn indexed_volume_data_is_other_but_a_missing_inode_stays_unknown() {
        let (outcome, _) = scan(&mut filesystem(None), &[volume()]);
        let data = outcome.overview.unwrap();
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["system_other_bytes"], MIB);
        assert_eq!(data.unknown[&UnknownReason::DirectOwnerUnresolved], 8 * MIB);
        assert_eq!(data.extent_bytes, 13 * MIB);
        assert_eq!(
            data.direct_owner_subcases
                [&crate::overview::DirectOwnerSubcase::WritableRootInodeUnindexed]
                .bytes,
            8 * MIB,
        );
        assert!(
            !data
                .direct_owner_subcases
                .contains_key(&crate::overview::DirectOwnerSubcase::IndexedPathUnmapped)
        );
        assert_eq!(data.share_live[&256].bytes, 4 * MIB);
        assert_eq!(data.share_live[&256].reclaimable_bytes, 0);
    }

    /// A shared leaf reached by the mixed volume root and a snapshot.
    /// Its file inode has an indexed volume path; the root alone has none.
    fn shared_volume_leaf(inode: u64, count: u32, valid: bool) -> FakeFilesystem {
        use crate::btrfs::leaf_ranges::testing::{FakeBlocks, file_item};
        use crate::btrfs::tree_block::{Expected, testing::leaf};
        let mut fs = filesystem(None);
        let parent = 40 * MIB;
        fs.trees.insert(
            EXTENT_TREE_OBJECTID,
            FakeSource::new(
                vec![
                    (
                        Key::new(10 * MIB, EXTENT_ITEM_KEY, MIB),
                        encode_data_extent(
                            u64::from(count),
                            &[DataBackref::Shared { parent, count }],
                        ),
                    ),
                    data_extent(20 * MIB, 4 * MIB, &[(256, 257), (257, 257)]),
                    data_extent(30 * MIB, 8 * MIB, &[(256, 999)]),
                    (
                        Key::new(parent, METADATA_ITEM_KEY, 0),
                        encode_tree_block(
                            2,
                            false,
                            &[
                                BlockBackref::Root(FS_TREE_OBJECTID),
                                BlockBackref::Root(257),
                            ],
                        ),
                    ),
                ],
                3,
            ),
        );
        let expected = Expected {
            bytenr: parent,
            generation: if valid { 7 } else { 8 },
            fsid: [3; 16],
        };
        fs.blocks = Some(FakeBlocks {
            fsid: expected.fsid,
            blocks: HashMap::from([(
                parent,
                leaf(
                    &expected,
                    4096,
                    &[(
                        Key::new(inode, EXTENT_DATA_KEY, 0),
                        file_item(10 * MIB, MIB, 0, MIB),
                    )],
                ),
            )]),
            reads: Vec::new(),
        });
        fs
    }

    #[test]
    fn a_verified_shared_leaf_attributes_an_indexed_volume_path_without_freeing_it() {
        let (direct, _) = scan(&mut filesystem(None), &[volume()]);
        let (shared, _) = scan(&mut shared_volume_leaf(257, 1, true), &[volume()]);
        let data = shared.overview.as_ref().unwrap();
        assert_eq!(data.system_other_bytes, MIB, "{data:?}");
        assert_eq!(data.unknown.values().sum::<u64>(), 8 * MIB);
        assert_eq!(data.extent_bytes, direct.overview.unwrap().extent_bytes);
        assert_eq!(
            data.extent_bytes,
            data.system_other_bytes
                + data.share_live[&256].bytes
                + data.unknown.values().sum::<u64>()
        );
        // Attribution is display-only: the existing shared-tree verdict stays conservative.
        assert_eq!(record(&shared, b"/v/f").metrics.reclaimable, 0);
        assert_eq!(record(&shared, b"/v/f").metrics.shared(), MIB);

        // Disposable sidecars preserve the checked stream's ranking and the
        // new overview counters; an old report still keeps its own attribution.
        use crate::query::{KindFilter, Measure, TopQuery, TopWalk};
        let path = scratch("shared-owner-sidecar").join("report.srrep");
        let mut saved_header = header(Vec::new());
        saved_header.overview = Some(data.clone());
        saved_header.totals = shared.totals;
        report::write_records(
            File::create(&path).unwrap(),
            &saved_header,
            shared.nodes.count(),
            |w| shared.nodes.write(w),
        )
        .unwrap();
        let query = TopQuery {
            under: b"/v",
            kind: KindFilter::All,
            by: Measure::Physical,
            services: true,
            include_system: false,
            only_shared: false,
            limit: 20,
        };
        let mut checked = TopWalk::new(b"/v", &query);
        crate::report::ReportReader::open(File::open(&path).unwrap())
            .unwrap()
            .for_each(|path, node| checked.visit(path, node))
            .unwrap();
        assert_eq!(
            crate::report_summary::index::top(&path, b"/v", &query).unwrap(),
            checked.finish()
        );
        assert_eq!(
            crate::report_summary::ensure(&path)
                .unwrap()
                .header
                .overview
                .as_ref(),
            Some(data)
        );
    }

    #[test]
    fn a_shared_leaf_with_unindexed_inodes_or_unverified_metadata_stays_unknown() {
        for (inode, count, valid) in [(999, 1, true), (257, 2, true), (257, 1, false)] {
            let (outcome, _) = scan(&mut shared_volume_leaf(inode, count, valid), &[volume()]);
            let data = outcome.overview.unwrap();
            assert_eq!(data.system_other_bytes, 0);
            assert_eq!(data.unknown.values().sum::<u64>(), 9 * MIB);
            assert_eq!(data.extent_bytes, 13 * MIB);
        }
    }

    #[test]
    fn shared_leaf_inodes_are_not_assigned_roots_from_a_different_leaf() {
        let mut fs = shared_volume_leaf(257, 1, true);
        let first = 40 * MIB;
        let second = 50 * MIB;
        fs.trees.insert(
            EXTENT_TREE_OBJECTID,
            FakeSource::new(
                vec![
                    (
                        Key::new(10 * MIB, EXTENT_ITEM_KEY, MIB),
                        encode_data_extent(
                            2,
                            &[
                                DataBackref::Shared {
                                    parent: first,
                                    count: 1,
                                },
                                DataBackref::Shared {
                                    parent: second,
                                    count: 1,
                                },
                            ],
                        ),
                    ),
                    data_extent(20 * MIB, 4 * MIB, &[(256, 257), (257, 257)]),
                    data_extent(30 * MIB, 8 * MIB, &[(256, 999)]),
                    (
                        Key::new(first, METADATA_ITEM_KEY, 0),
                        encode_tree_block(1, false, &[BlockBackref::Root(FS_TREE_OBJECTID)]),
                    ),
                    // This leaf's inode does not exist in the mixed root. Its known
                    // shared-folder root already provides a uniform owner, and needs
                    // neither an inode lookup in the other root nor another leaf read.
                    (
                        Key::new(second, METADATA_ITEM_KEY, 0),
                        encode_tree_block(1, false, &[BlockBackref::Root(256)]),
                    ),
                ],
                3,
            ),
        );
        let (outcome, _) = scan(&mut fs, &[volume()]);
        let data = outcome.overview.unwrap();
        assert_eq!(data.shared_between_folders_bytes, MIB);
        assert_eq!(data.unknown.values().sum::<u64>(), 8 * MIB);
        assert_eq!(data.extent_bytes, 13 * MIB);
    }

    #[test]
    fn a_second_scan_reuses_sealed_units_for_the_same_report() {
        let mut cache = DirCache::new();
        let (first, _) = scan_cached(&mut filesystem(None), &[volume()], &mut cache);
        cache.now = 1100;
        let (second, events) = scan_cached(&mut filesystem(None), &[volume()], &mut cache);
        assert_eq!(trees_read(&events), Vec::<&str>::new());
        let reused: Vec<&String> = events.iter().filter(|e| e.starts_with("reused")).collect();
        assert_eq!(reused, ["reused 5", "reused 256"]);
        assert_eq!(records(&second), records(&first));
        assert_eq!(second.totals, first.totals);
        assert_eq!(
            roles(&second),
            [
                (5, SubvolumeRole::Reused),
                (256, SubvolumeRole::Reused),
                (257, SubvolumeRole::SnapshotSkipped)
            ]
        );
        assert!(
            second
                .subvolumes
                .iter()
                .take(2)
                .all(|s| s.captured == Some(1000))
        );
        assert_eq!(second.stats.subvolumes_reused, 2);
        assert_eq!(second.stats.subvolumes_scanned, 0);
        assert_eq!(second.stats.entries, first.stats.entries);
        assert_eq!(second.consistency, Consistency::ObservedStable);
    }

    /// The tier-0 duplicate candidates the units of `outcome` hold.
    fn candidates(outcome: &ScanOutcome) -> Vec<(u64, u64, u64, u64)> {
        let mut all = Vec::new();
        for (tree, file) in &outcome.candidates {
            for c in crate::duplicates::candidates::read_unit(file, *tree).unwrap() {
                all.push((c.tree, c.objectid, c.size, c.first_block));
            }
        }
        all
    }

    #[test]
    fn units_record_large_files_for_duplicates_and_keep_them_when_reused() {
        let mut cache = DirCache::new();
        let (first, _) = scan_cached(&mut filesystem(None), &[volume()], &mut cache);
        let expected = vec![
            (FS_TREE_OBJECTID, 257, MIB, 10 * MIB),
            (256, 257, 4 * MIB, 20 * MIB),
        ];
        assert_eq!(candidates(&first), expected);
        cache.now = 1100;
        let (second, events) = scan_cached(&mut filesystem(None), &[volume()], &mut cache);
        assert_eq!(trees_read(&events), Vec::<&str>::new());
        assert_eq!(candidates(&second), expected);
    }

    /// Trees of sorted items, searched by binary search, so a benchmark
    /// measures the scan and not the fake.
    struct SortedTrees(HashMap<u64, Vec<TreeItem>>, Vec<u8>);

    impl MetadataSource for SortedTrees {
        fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            self.1.clear();
            let mut nr_items = 0;
            if let Some(items) = self.0.get(&range.tree_id) {
                let start = items.partition_point(|(k, _)| *k < range.min);
                for (key, data) in items[start..].iter().take(4096) {
                    if *key > range.max {
                        break;
                    }
                    self.1
                        .extend(crate::btrfs::search::testing::encode_item(1, *key, data));
                    nr_items += 1;
                }
            }
            Ok(RawBatch {
                nr_items,
                buf: &self.1,
            })
        }
    }

    /// One volume of `files` files in folders of 1,000; every tenth file
    /// is 2 MiB, the rest 16 KiB, each in its own extent.
    fn bench_volume(files: u64) -> SortedTrees {
        let folders = files.div_ceil(1000);
        let mut top = vec![inode(256, DIR, 0, 1)];
        let mut extents = Vec::new();
        for d in 0..folders {
            let dir = 257 + d;
            top.push(entry(256, 2 + d, &format!("d{d}"), dir));
            top.push(inode(dir, DIR, 0, 1));
        }
        for f in 0..files {
            let ino = 257 + folders + f;
            let size = if f % 10 == 0 { 2 * MIB } else { 16 << 10 };
            let bytenr = (1 << 30) + f * 4 * MIB;
            top.push(entry(257 + f / 1000, 2 + f % 1000, &format!("f{f}"), ino));
            top.push(inode(ino, FILE, size, 1));
            top.push(whole_extent(ino, 0, bytenr, size));
            extents.push(data_extent(bytenr, size, &[(FS_TREE_OBJECTID, ino)]));
        }
        let mut trees = HashMap::from([
            (
                ROOT_TREE_OBJECTID,
                vec![root_item(FS_TREE_OBJECTID, 10, false)],
            ),
            (FS_TREE_OBJECTID, top),
            (EXTENT_TREE_OBJECTID, extents),
        ]);
        for items in trees.values_mut() {
            items.sort_by_key(|(k, _)| *k);
        }
        SortedTrees(trees, Vec::new())
    }

    /// Tier-0 duplicate candidates read nothing new: on a volume of
    /// 200,000 files they must cost no more than noise. The survey
    /// measured the same record at about 0% of a 1M-file scan on a loop
    /// device; this checks the scan code alone, where the share is
    /// largest. Run it in release:
    /// `cargo test --release --lib -- --ignored duplicate_candidates_cost --nocapture`.
    #[test]
    #[ignore = "benchmark; run in release"]
    fn duplicate_candidates_cost_no_more_than_noise() {
        let mut fs = bench_volume(200_000);
        let mut run = |candidates: bool| {
            let started = std::time::Instant::now();
            let outcome = scan_filesystem(
                &mut fs,
                &[volume()],
                &Marks::default(),
                ScanOptions {
                    duplicate_candidates: candidates,
                    ..ScanOptions::default()
                },
                &mut Recorder {
                    stop: NoStop,
                    events: Vec::new(),
                },
                spill(),
                &mut NoCache,
            )
            .unwrap();
            let elapsed = started.elapsed();
            let records = candidates.then(|| self::candidates(&outcome).len());
            (elapsed, records)
        };
        let (mut without, mut with) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            without.push(run(false).0);
            let (elapsed, records) = run(true);
            assert_eq!(records, Some(20_000));
            with.push(elapsed);
        }
        without.sort();
        with.sort();
        let (base, rec) = (without[2], with[2]);
        eprintln!("median scan without {base:?}, with candidates {rec:?}");
        assert!(
            rec.as_secs_f64() <= base.as_secs_f64() * 1.05 + 0.02,
            "tier-0 records cost {rec:?} against {base:?}"
        );
    }

    #[test]
    fn a_changed_generation_rereads_only_that_subvolume() {
        let mut cache = DirCache::new();
        scan_cached(&mut filesystem(None), &[volume()], &mut cache);
        let mut changed = filesystem(None);
        changed.root = FakeSource::new(root_tree(21), 2);
        let (outcome, events) = scan_cached(&mut changed, &[volume()], &mut cache);
        assert_eq!(trees_read(&events), ["256"]);
        assert_eq!(outcome.stats.subvolumes_reused, 1);
        assert_eq!(outcome.stats.subvolumes_scanned, 1);
        assert_eq!(outcome.totals.physical, 5 * MIB);
    }

    /// Stops at the first batch boundary inside tree `.0`.
    struct StopInTree(u64, bool);

    impl BatchBoundary for StopInTree {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            if self.1 {
                return ControlFlow::Break(StopReason::Paused);
            }
            ControlFlow::Continue(())
        }
    }

    impl ScanControl for StopInTree {
        fn boundary(&mut self) -> &mut dyn BatchBoundary {
            self
        }

        fn event(&mut self, event: ScanEvent<'_>) {
            if let ScanEvent::TreeStarted { tree, .. } = event {
                self.1 |= tree == self.0;
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn administrator_pause_in_the_first_tree_saves_a_usable_checkpoint_and_resumes() {
        use crate::cache::{Budget, Cache, Reuse, Sealing};
        use crate::jobs::{Job, JobState, Live, Request};
        let state = scratch("first-tree-resume");
        let jobs = state.join("jobs");
        let live = Live::start(
            &jobs,
            crate::sys::effective_uid(),
            Job::new(
                "saved-scan",
                &[b"/volume1".to_vec()],
                Some(Scope::Volume),
                &crate::cli::ScanLimits::report_only(),
                100,
            ),
        )
        .unwrap();
        let dir = jobs.join("saved-scan/checkpoint");
        let open = || {
            Sealing::checkpoint(
                Cache::open(
                    &dir,
                    "fs",
                    crate::sys::effective_uid(),
                    Reuse {
                        trust_writable: false,
                        writable_max_age: 0,
                    },
                    Budget {
                        max_bytes: u64::MAX,
                        min_free: 0,
                    },
                )
                .unwrap(),
            )
        };
        // An administrator's durable request is consumed at a real metadata batch boundary.
        struct AdminPause<'a> {
            live: &'a Live,
            reading: bool,
        }
        impl BatchBoundary for AdminPause<'_> {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                if self.reading && self.live.requested() == Some(Request::Pause) {
                    ControlFlow::Break(StopReason::Paused)
                } else {
                    ControlFlow::Continue(())
                }
            }
        }
        impl ScanControl for AdminPause<'_> {
            fn boundary(&mut self) -> &mut dyn BatchBoundary {
                self
            }
            fn event(&mut self, event: ScanEvent<'_>) {
                if matches!(event, ScanEvent::TreeStarted { .. }) {
                    self.reading = true;
                }
            }
        }
        crate::jobs::request(
            &jobs,
            crate::sys::effective_uid(),
            "saved-scan",
            Request::Pause,
            150,
        )
        .unwrap();
        let mut fs = filesystem(None);
        let stopped = scan_filesystem(
            &mut fs,
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut AdminPause {
                live: &live,
                reading: false,
            },
            spill(),
            &mut open(),
        )
        .unwrap_err();
        assert!(matches!(
            stopped.error,
            WalkError::Stopped(StopReason::Paused)
        ));
        live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        assert!(
            !dir.join("fs/manifest").exists(),
            "paused before completing even the first tree"
        );
        assert!(dir.join("fs/namespace.json").is_file());
        assert!(dir.join("fs/namespace.refs").is_file());
        let keys = Cache::checkpoint_keys(&dir, "fs").unwrap();
        assert_eq!(keys.len(), 1);
        validate_checkpoint_roots(&mut fs, &Marks::default(), &keys, &mut NoStop).unwrap();
        let calls = fs.trees[&FS_TREE_OBJECTID].calls.len();
        let last = fs.trees[&FS_TREE_OBJECTID].items[2].0;
        let queued = crate::jobs::queue_resume(
            &jobs,
            crate::sys::effective_uid(),
            "saved-scan",
            Some(crate::resource::Profile::Fast),
            300,
        )
        .unwrap();
        assert_eq!(queued.state, JobState::Queued);
        let worker = Live::launch(&jobs, crate::sys::effective_uid(), "saved-scan", 301).unwrap();
        assert_eq!(worker.job.state, JobState::Running);
        assert_eq!(worker.job.resumes, 1);
        assert_eq!(worker.requested(), None);
        let (resumed, _) = scan_cached(&mut fs, &[volume()], &mut open());
        assert_eq!(
            fs.trees[&FS_TREE_OBJECTID].calls[calls].min,
            last.successor().unwrap(),
            "the worker continues after the saved search key instead of starting over"
        );
        let (fresh, _) = scan(&mut filesystem(None), &[volume()]);
        assert_eq!(records(&resumed), records(&fresh));
        assert_eq!(resumed.totals, fresh.totals);
        worker.finish(JobState::Completed, None, 400).unwrap();
        assert_eq!(
            crate::jobs::load(&jobs, "saved-scan").unwrap().state,
            JobState::Completed
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn paused_scan_keeps_completed_units_and_search_key_across_package_upgrade() {
        use crate::cache::{Budget, Cache, Reuse, Sealing};
        let state = scratch("job-checkpoint").join("state");
        let jobs = state.join("jobs");
        let live = crate::jobs::Live::start(
            &jobs,
            crate::sys::effective_uid(),
            crate::jobs::Job::new(
                "saved-scan",
                &[b"/volume1".to_vec()],
                Some(Scope::Volume),
                &crate::cli::ScanLimits::report_only(),
                100,
            ),
        )
        .unwrap();
        let dir = jobs.join("saved-scan/checkpoint");
        let open = || {
            Sealing::checkpoint(
                Cache::open(
                    &dir,
                    "fs",
                    crate::sys::effective_uid(),
                    Reuse {
                        trust_writable: false,
                        writable_max_age: 0,
                    },
                    Budget {
                        max_bytes: u64::MAX,
                        min_free: 0,
                    },
                )
                .unwrap(),
            )
        };
        let mut fs = filesystem(None);
        let stopped = scan_filesystem(
            &mut fs,
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut StopInTree(256, false),
            spill(),
            &mut open(),
        )
        .unwrap_err();
        assert!(matches!(
            stopped.error,
            WalkError::Stopped(StopReason::Paused)
        ));
        // The package stop/upgrade tears down launch/grant state, while the
        // next package process must reopen these exact durable bytes.
        live.finish(crate::jobs::JobState::Paused, Some("paused"), 200)
            .unwrap();
        let manifest = fs::read(dir.join("fs/manifest")).unwrap();
        let namespace = fs::read(dir.join("fs/namespace.json")).unwrap();
        let privilege = state.join("privilege");
        fs::create_dir_all(&privilege).unwrap();
        fs::write(privilege.join("old-runtime"), b"old").unwrap();
        assert!(
            crate::root_entry::lifecycle::stop(
                &state,
                &privilege,
                crate::sys::effective_uid(),
                300
            )
            .is_empty()
        );
        assert!(!privilege.exists());
        assert_eq!(fs::read(dir.join("fs/manifest")).unwrap(), manifest);
        assert_eq!(fs::read(dir.join("fs/namespace.json")).unwrap(), namespace);
        let keys = Cache::checkpoint_keys(&dir, "fs").unwrap();
        validate_checkpoint_roots(&mut fs, &Marks::default(), &keys, &mut NoStop).unwrap();
        let top_calls = fs.trees[&FS_TREE_OBJECTID].calls.len();
        let share_calls = fs.trees[&256].calls.len();
        let last = fs.trees[&256].items[2].0;
        crate::jobs::queue_resume(&jobs, crate::sys::effective_uid(), "saved-scan", None, 400)
            .unwrap();
        let resumed_job =
            crate::jobs::Live::launch(&jobs, crate::sys::effective_uid(), "saved-scan", 401)
                .unwrap();
        assert_eq!(resumed_job.job.job_id, "saved-scan");
        assert_eq!(resumed_job.job.resumes, 1);
        let (resumed, events) = scan_cached(&mut fs, &[volume()], &mut open());
        assert!(events.iter().any(|e| e == "reused 5"));
        let extra = &fs.trees[&FS_TREE_OBJECTID].calls[top_calls..];
        assert_eq!(
            extra.len(),
            1,
            "only a snapshot path lookup follows the saved namespace"
        );
        assert!(
            extra.iter().all(
                |range| range.min.item_type == crate::btrfs::keys::INODE_REF_KEY
                    && range.min.objectid == range.max.objectid
            ),
            "the finished namespace must not be walked again"
        );
        assert_eq!(
            fs.trees[&256].calls[share_calls].min,
            last.successor().unwrap()
        );
        let (fresh, _) = scan(&mut filesystem(None), &[volume()]);
        assert_eq!(resumed.totals, fresh.totals);
        assert_eq!(resumed.overview, fresh.overview);
        assert_eq!(records(&resumed), records(&fresh));
        resumed_job
            .finish(crate::jobs::JobState::Completed, None, 500)
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn changed_completed_subvolume_restarts_only_that_unit() {
        use crate::cache::{Budget, Cache, Reuse, Sealing};
        let dir = scratch("changed-checkpoint").join("cache");
        let open = || {
            Sealing::checkpoint(
                Cache::open(
                    &dir,
                    "fs",
                    crate::sys::effective_uid(),
                    Reuse {
                        trust_writable: false,
                        writable_max_age: 0,
                    },
                    Budget {
                        max_bytes: u64::MAX,
                        min_free: 0,
                    },
                )
                .unwrap(),
            )
        };
        scan_filesystem(
            &mut filesystem(None),
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut StopInTree(256, false),
            spill(),
            &mut open(),
        )
        .unwrap_err();
        let mut changed = filesystem(None);
        let mut roots = root_tree(20);
        roots[0] = root_item(FS_TREE_OBJECTID, 99, false);
        changed.root = FakeSource::new(roots, 2);
        let keys = Cache::checkpoint_keys(&dir, "fs").unwrap();
        validate_checkpoint_roots(&mut changed, &Marks::default(), &keys, &mut NoStop).unwrap();
        let mut control = Recorder {
            stop: NoStop,
            events: Vec::new(),
        };
        let resumed = scan_filesystem(
            &mut changed,
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut control,
            spill(),
            &mut open(),
        )
        .unwrap();
        assert_eq!(trees_read(&control.events), ["5", "256"]);
        assert!(
            changed.trees[&FS_TREE_OBJECTID]
                .calls
                .iter()
                .any(|range| range.min == Key::MIN)
        );
        assert!(
            !changed.trees[&256]
                .calls
                .iter()
                .any(|range| range.min == Key::MIN),
            "the unchanged partial share continues after its saved key"
        );
        let (fresh, _) = scan(&mut filesystem(None), &[volume()]);
        assert_eq!(resumed.totals, fresh.totals);
        assert_eq!(records(&resumed), records(&fresh));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn business_writes_between_pause_and_resume_restart_only_the_changed_unit() {
        use crate::cache::{Budget, Cache, Reuse, Sealing};
        for (partial, outside) in [(true, false), (false, false), (true, true), (false, true)] {
            let dir = scratch("checkpoint-own-writes").join("cache");
            let open = || {
                Sealing::checkpoint(
                    Cache::open(
                        &dir,
                        "fs",
                        crate::sys::effective_uid(),
                        Reuse {
                            trust_writable: false,
                            writable_max_age: 0,
                        },
                        Budget {
                            max_bytes: u64::MAX,
                            min_free: 0,
                        },
                    )
                    .unwrap(),
                )
            };
            let mut first = state_filesystem(root_tree(20), false);
            if partial {
                let stopped = scan_filesystem(
                    &mut first,
                    &[volume()],
                    &state_marks(),
                    ScanOptions::default(),
                    &mut StopInTree(256, false),
                    spill(),
                    &mut open(),
                )
                .unwrap_err();
                assert!(matches!(
                    stopped.error,
                    WalkError::Stopped(StopReason::Paused)
                ));
            } else {
                scan_marked(&mut first, &[volume()], &state_marks(), &mut open());
            }
            // A business write changes the saved prefix (directory entry and
            // logical size), so blindly continuing its search key would be stale.
            let live = || {
                let mut fs = state_filesystem(root_tree(21), outside);
                fs.root = FakeSource::new(root_tree(21), 2);
                if outside {
                    for (key, data) in &mut fs.trees.get_mut(&256).unwrap().items {
                        if *key == Key::new(257, crate::btrfs::keys::INODE_ITEM_KEY, 0) {
                            data[16..24].copy_from_slice(&4096u64.to_le_bytes());
                        }
                        if *key == Key::new(256, crate::btrfs::keys::DIR_INDEX_KEY, 2) {
                            *data = entry(256, 2, "business-written", 257).1;
                        }
                    }
                }
                fs
            };
            let mut changed = live();
            let keys = Cache::checkpoint_keys(&dir, "fs").unwrap();
            validate_checkpoint_roots(&mut changed, &state_marks(), &keys, &mut NoStop).unwrap();
            let mut control = Recorder {
                stop: NoStop,
                events: Vec::new(),
            };
            let resumed = scan_filesystem(
                &mut changed,
                &[volume()],
                &state_marks(),
                ScanOptions::default(),
                &mut control,
                spill(),
                &mut open(),
            )
            .unwrap();
            let (fresh, _) = scan_marked(&mut live(), &[volume()], &state_marks(), &mut NoCache);
            assert_eq!(resumed.totals, fresh.totals);
            assert_eq!(records(&resumed), records(&fresh));
            assert!(control.events.iter().any(|e| e == "reused 5"));
            assert_eq!(
                trees_read(&control.events),
                if outside || partial {
                    vec!["256"]
                } else {
                    vec![]
                }
            );
            let restarted = changed.trees[&256]
                .calls
                .iter()
                .any(|range| range.min == Key::MIN && range.min_transid == 0);
            assert_eq!(restarted, outside, "only a business-written unit restarts");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pausing_a_changed_completed_unit_keeps_the_later_partial_checkpoint() {
        use crate::cache::{Budget, Cache, Reuse, Sealing};
        let dir = scratch("multiple-partial-units").join("cache");
        let open = || {
            Sealing::checkpoint(
                Cache::open(
                    &dir,
                    "fs",
                    crate::sys::effective_uid(),
                    Reuse {
                        trust_writable: false,
                        writable_max_age: 0,
                    },
                    Budget {
                        max_bytes: u64::MAX,
                        min_free: 0,
                    },
                )
                .unwrap(),
            )
        };
        scan_filesystem(
            &mut filesystem(None),
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut StopInTree(256, false),
            spill(),
            &mut open(),
        )
        .unwrap_err();
        let partial = fs::read(dir.join("fs/namespace.json")).unwrap();
        let changed = || {
            let mut source = filesystem(None);
            let mut roots = root_tree(20);
            roots[0] = root_item(FS_TREE_OBJECTID, 99, false);
            source.root = FakeSource::new(roots, 2);
            source
        };
        let stopped = scan_filesystem(
            &mut changed(),
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut StopInTree(FS_TREE_OBJECTID, false),
            spill(),
            &mut open(),
        )
        .unwrap_err();
        assert!(matches!(
            stopped.error,
            WalkError::Stopped(StopReason::Paused)
        ));
        assert_eq!(fs::read(dir.join("fs/namespace.json")).unwrap(), partial);
        assert!(dir.join("fs/namespace-5.json").is_file());
        let keys = Cache::checkpoint_keys(&dir, "fs").unwrap();
        assert_eq!(keys.len(), 3, "sealed old unit plus both partial units");
        let mut source = changed();
        validate_checkpoint_roots(&mut source, &Marks::default(), &keys, &mut NoStop).unwrap();
        let (resumed, _) = scan_cached(&mut source, &[volume()], &mut open());
        let (fresh, _) = scan(&mut changed(), &[volume()]);
        assert_eq!(records(&resumed), records(&fresh));
        assert_eq!(resumed.totals, fresh.totals);
        assert!(
            !source.trees[&FS_TREE_OBJECTID]
                .calls
                .iter()
                .any(|r| r.min == Key::MIN)
        );
        assert!(!source.trees[&256].calls.iter().any(|r| r.min == Key::MIN));
    }

    #[test]
    fn resume_rejects_deleted_or_replaced_roots_and_relocation_but_allows_generation_changes() {
        let mut source = filesystem(None);
        let mut roots = discover_relocating(&mut source, &mut NoStop).unwrap();
        let root = roots.get(256).unwrap();
        let mut key = UnitKey {
            subvolume: 256,
            uuid: root.uuid.map_or(String::new(), |id| hex_bytes::encode(&id)),
            generation: 0,
            readonly: root.readonly,
            slot: 0,
            inputs: 0,
        };
        validate_saved_roots(&roots, &[key.clone()]).unwrap();
        key.uuid.push_str("changed");
        assert!(matches!(
            validate_saved_roots(&roots, &[key.clone()]),
            Err(WalkError::Checkpoint(_))
        ));
        key.subvolume = 9999;
        assert!(
            validate_saved_roots(&roots, &[key.clone()])
                .unwrap_err()
                .to_string()
                .contains("no longer exists")
        );
        roots.relocating.insert(256);
        assert!(
            validate_saved_roots(&roots, &[key])
                .unwrap_err()
                .to_string()
                .contains("relocated")
        );
    }

    #[test]
    fn a_stopped_scan_keeps_the_units_it_completed() {
        let mut cache = DirCache::new();
        let marks = Marks::default();
        let mut stop = StopInTree(256, false);
        let stopped = scan_filesystem(
            &mut filesystem(None),
            &[volume()],
            &marks,
            ScanOptions::default(),
            &mut stop,
            spill(),
            &mut cache,
        );
        assert!(stopped.is_err());
        let (outcome, events) = scan_cached(&mut filesystem(None), &[volume()], &mut cache);
        assert_eq!(trees_read(&events), ["256"], "restarts the unit it was in");
        assert_eq!(outcome.stats.subvolumes_reused, 1);
        assert_eq!(outcome.totals.physical, 5 * MIB);
    }

    /// A data `EXTENT_ITEM` with one inline backref per `(root, inode)`.
    fn data_extent(bytenr: u64, len: u64, owners: &[(u64, u64)]) -> TreeItem {
        let backrefs: Vec<DataBackref> = owners
            .iter()
            .map(|&(root, objectid)| DataBackref::Direct {
                root,
                objectid,
                offset: 0,
                count: 1,
            })
            .collect();
        (
            Key::new(bytenr, EXTENT_ITEM_KEY, len),
            encode_data_extent(owners.len() as u64, &backrefs),
        )
    }

    fn shared_data_extent(bytenr: u64, len: u64, parent: u64) -> TreeItem {
        (
            Key::new(bytenr, EXTENT_ITEM_KEY, len),
            encode_data_extent(1, &[DataBackref::Shared { parent, count: 1 }]),
        )
    }

    fn tree_block(bytenr: u64, root: u64) -> TreeItem {
        (
            Key::new(bytenr, METADATA_ITEM_KEY, 0),
            encode_tree_block(1, false, &[BlockBackref::Root(root)]),
        )
    }

    #[test]
    fn a_volume_owner_budget_is_configurable_and_reported() {
        let mut capped_fs = filesystem(None);
        let extents = capped_fs.trees.get_mut(&EXTENT_TREE_OBJECTID).unwrap();
        extents
            .items
            .push(shared_data_extent(40 * MIB, MIB, 50 * MIB));
        extents.items.push(tree_block(50 * MIB, 256));
        let capped = scan_filesystem(
            &mut capped_fs,
            &[volume()],
            &Marks::default(),
            ScanOptions {
                snapshots: None,
                owner_budget: 0,
                ..ScanOptions::default()
            },
            &mut Recorder {
                stop: NoStop,
                events: Vec::new(),
            },
            spill(),
            &mut NoCache,
        )
        .unwrap();
        assert_eq!(capped.stats.owner_lookups, 0);
        assert_eq!(capped.stats.owner_budget_capped_extents, 1);
        assert_eq!(
            capped.overview.unwrap().unknown[&UnknownReason::OwnerBudgetExhausted],
            MIB
        );

        let mut resolved_fs = filesystem(None);
        let extents = resolved_fs.trees.get_mut(&EXTENT_TREE_OBJECTID).unwrap();
        extents
            .items
            .push(shared_data_extent(40 * MIB, MIB, 50 * MIB));
        extents.items.push(tree_block(50 * MIB, 256));
        let resolved = scan_filesystem(
            &mut resolved_fs,
            &[volume()],
            &Marks::default(),
            ScanOptions {
                snapshots: None,
                owner_budget: 1,
                ..ScanOptions::default()
            },
            &mut Recorder {
                stop: NoStop,
                events: Vec::new(),
            },
            spill(),
            &mut NoCache,
        )
        .unwrap();
        assert_eq!(resolved.stats.owner_lookups, 1);
        assert_eq!(resolved.stats.owner_budget_capped_extents, 0);
        assert!(
            !resolved
                .overview
                .unwrap()
                .unknown
                .contains_key(&UnknownReason::OwnerBudgetExhausted)
        );
    }

    /// `filesystem` with a 1 MiB extent that only a shared leaf of
    /// share 256 references, over its first half, and that leaf readable.
    fn shared_leaf_filesystem() -> FakeFilesystem {
        use crate::btrfs::leaf_ranges::testing::{FakeBlocks, file_item};
        use crate::btrfs::tree_block::{Expected, testing::leaf};
        let fsid = [9; 16];
        let expected = Expected {
            bytenr: 50 * MIB,
            generation: 7,
            fsid,
        };
        let half = file_item(40 * MIB, MIB, 0, MIB / 2);
        let block = leaf(
            &expected,
            4096,
            &[(Key::new(300, EXTENT_DATA_KEY, 0), half)],
        );
        let mut fs = filesystem(None);
        let extents = fs.trees.get_mut(&EXTENT_TREE_OBJECTID).unwrap();
        extents
            .items
            .push(shared_data_extent(40 * MIB, MIB, 50 * MIB));
        extents.items.push(tree_block(50 * MIB, 256));
        fs.blocks = Some(FakeBlocks {
            fsid,
            blocks: [(50 * MIB, block)].into_iter().collect(),
            reads: Vec::new(),
        });
        fs
    }

    #[test]
    fn deferred_attribution_resolves_pinned_bytes_without_walking_the_namespace() {
        let scratch = spill();
        let queue = scratch.path("deferred");
        let outcome = scan_filesystem(
            &mut two_leaf_filesystem(),
            &[volume()],
            &Marks::default(),
            ScanOptions {
                pinned_read_budget: 0,
                pending: Some(queue.clone()),
                ..Default::default()
            },
            &mut Recorder {
                stop: NoStop,
                events: Vec::new(),
            },
            spill(),
            &mut NoCache,
        )
        .unwrap();
        assert_eq!(
            outcome
                .stats
                .pinned_read_plan
                .as_ref()
                .unwrap()
                .selected_reads,
            0
        );
        let mut fs = two_leaf_filesystem();
        let set = discover(&mut fs, &mut NoStop).unwrap();
        let manifest = attribution::open(&queue, &set).unwrap();
        let blocks = fs.block_reader(&mut NoStop).unwrap().unwrap();
        let (after, measured) = attribution::process(
            &mut fs,
            blocks,
            &manifest,
            &queue,
            &scratch.path("checkpoint"),
            DEFAULT_READ_BUDGET,
            &mut NoStop,
        )
        .unwrap();
        assert_eq!(measured.processed_extents, 2);
        assert!(measured.unknown_after < measured.unknown_before);
        assert_eq!(after.pinned_unreferenced_bytes, MIB / 2 + MIB * 3 / 4);
        assert_eq!(
            after.extent_count,
            outcome.overview.as_ref().unwrap().extent_count
        );
        assert_eq!(
            after.extent_bytes,
            outcome.overview.as_ref().unwrap().extent_bytes
        );
        assert!(
            fs.trees
                .iter()
                .filter(|(tree, _)| **tree != EXTENT_TREE_OBJECTID)
                .all(|(_, source)| source.calls.is_empty()),
            "deep pass walked a namespace"
        );
        // Allocation changes cannot turn stale queue evidence into attributed bytes.
        let mut changed = two_leaf_filesystem();
        changed
            .trees
            .get_mut(&EXTENT_TREE_OBJECTID)
            .unwrap()
            .items
            .retain(|(key, _)| key.objectid != 40 * MIB);
        let blocks = changed.block_reader(&mut NoStop).unwrap().unwrap();
        let (after, measured) = attribution::process(
            &mut changed,
            blocks,
            &manifest,
            &queue,
            &scratch.path("changed-checkpoint"),
            DEFAULT_READ_BUDGET,
            &mut NoStop,
        )
        .unwrap();
        assert_eq!(measured.changed_extents, 1);
        assert_eq!(
            after.pinned_ranges_subcases[&PinnedRangesSubcase::ExtentChanged],
            MIB
        );
    }

    #[test]
    fn deferred_attribution_resumes_after_a_durable_chunk() {
        use crate::btrfs::leaf_ranges::testing::{FakeBlocks, file_item};
        use crate::btrfs::tree_block::{Expected, testing::leaf};
        fn model() -> FakeFilesystem {
            let mut fs = filesystem(None);
            let mut blocks = HashMap::new();
            let extents = fs.trees.get_mut(&EXTENT_TREE_OBJECTID).unwrap();
            for i in 0..300 {
                let at = (1000 * MIB) + i * 4096;
                let data = (40 + i) * MIB;
                extents
                    .items
                    .extend([shared_data_extent(data, MIB, at), tree_block(at, 256)]);
                blocks.insert(
                    at,
                    leaf(
                        &Expected {
                            bytenr: at,
                            generation: 7,
                            fsid: [9; 16],
                        },
                        4096,
                        &[(
                            Key::new(300, EXTENT_DATA_KEY, 0),
                            file_item(data, MIB, 0, MIB / 2),
                        )],
                    ),
                );
            }
            extents.items.sort_by_key(|(key, _)| *key);
            fs.blocks = Some(FakeBlocks {
                fsid: [9; 16],
                blocks,
                reads: Vec::new(),
            });
            fs
        }
        struct PauseAtCheckpoint(PathBuf);
        impl BatchBoundary for PauseAtCheckpoint {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                if self.0.is_file() {
                    ControlFlow::Break(StopReason::Paused)
                } else {
                    ControlFlow::Continue(())
                }
            }
        }
        let scratch = spill();
        let queue = scratch.path("queue");
        let checkpoint = scratch.path("deep");
        scan_filesystem(
            &mut model(),
            &[volume()],
            &Marks::default(),
            ScanOptions {
                pinned_read_budget: 0,
                pending: Some(queue.clone()),
                ..Default::default()
            },
            &mut Recorder {
                stop: NoStop,
                events: Vec::new(),
            },
            spill(),
            &mut NoCache,
        )
        .unwrap();
        let mut fs = model();
        let set = discover(&mut fs, &mut NoStop).unwrap();
        let manifest = attribution::open(&queue, &set).unwrap();
        let blocks = fs.block_reader(&mut NoStop).unwrap().unwrap();
        let paused = attribution::process(
            &mut fs,
            blocks,
            &manifest,
            &queue,
            &checkpoint,
            DEFAULT_READ_BUDGET,
            &mut PauseAtCheckpoint(checkpoint.join("progress.json")),
        );
        assert!(matches!(
            paused,
            Err(WalkError::Stopped(StopReason::Paused))
        ));
        let mut fs = model();
        let blocks = fs.block_reader(&mut NoStop).unwrap().unwrap();
        let (resumed, measurement) = attribution::process(
            &mut fs,
            blocks,
            &manifest,
            &queue,
            &checkpoint,
            DEFAULT_READ_BUDGET,
            &mut NoStop,
        )
        .unwrap();
        assert_eq!(measurement.processed_extents, 300);
        assert_eq!(measurement.reads, 300);
        let mut fs = model();
        let blocks = fs.block_reader(&mut NoStop).unwrap().unwrap();
        // Each of the two durable saves below takes one virtual hour. A virtual
        // clock, not a sleep and a wall-clock bound: under qemu the real work
        // around the saves is slow and uneven enough that `wall - measured`
        // missed a fixed bound. An hour dwarfs any real run time, so the
        // bounds below hold on any host.
        const SAVE: std::time::Duration = std::time::Duration::from_secs(3600);
        attribution::checkpoint_test_delay(SAVE);
        let (fresh, fresh_measurement) = attribution::process(
            &mut fs,
            blocks,
            &manifest,
            &queue,
            &scratch.path("fresh"),
            DEFAULT_READ_BUDGET,
            &mut NoStop,
        )
        .unwrap();
        attribution::checkpoint_test_delay(std::time::Duration::ZERO);
        // 300 extents make one full chunk of 256, so there are two durable
        // saves. Only the final save follows the last timing sample; the
        // earlier save's time must be included in elapsed.
        let save_ms = SAVE.as_millis() as u64;
        assert!(
            (save_ms..2 * save_ms).contains(&fresh_measurement.elapsed_ms),
            "checkpoint I/O missing from elapsed: measured={}, one save={save_ms}",
            fresh_measurement.elapsed_ms
        );
        assert_eq!(fresh, resumed);
        assert_eq!(fresh_measurement.reads, measurement.reads);
        assert!(
            fs.trees
                .iter()
                .filter(|(tree, _)| **tree != EXTENT_TREE_OBJECTID)
                .all(|(_, source)| source.calls.is_empty())
        );
    }

    #[test]
    fn a_volume_overview_reads_shared_leaves_for_pinned_bytes() {
        let mut fs = shared_leaf_filesystem();

        let outcome = scan_filesystem(
            &mut fs,
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut Recorder {
                stop: NoStop,
                events: Vec::new(),
            },
            spill(),
            &mut NoCache,
        )
        .unwrap();
        let (unread, _) = scan(&mut filesystem(None), &[volume()]);

        let overview = outcome.overview.unwrap();
        assert!(overview.pinned_ranges_subcases.is_empty(), "{overview:?}");
        assert_eq!(overview.pinned_unreferenced_bytes, MIB / 2);
        assert_eq!(outcome.stats.pinned_leaf_reads, 1);
        assert_eq!(outcome.stats.pinned_leaf_read_bytes, 4096);
        assert_eq!(outcome.stats.pinned_leaf_reader_unavailable, None);
        assert_eq!(
            unread.stats.pinned_leaf_reader_unavailable.as_deref(),
            Some("no blocks")
        );
    }

    /// `filesystem` with two unscanned extents: half of the one at
    /// 40 MiB is referenced from the leaf at 60 MiB, a quarter of the
    /// one at 41 MiB from the leaf at 50 MiB.
    fn two_leaf_filesystem() -> FakeFilesystem {
        use crate::btrfs::leaf_ranges::testing::{FakeBlocks, file_item};
        use crate::btrfs::tree_block::{Expected, testing::leaf};
        let fsid = [9; 16];
        let block = |at: u64, extent: u64, len: u64| {
            let expected = Expected {
                bytenr: at,
                generation: 7,
                fsid,
            };
            let item = file_item(extent, MIB, 0, len);
            leaf(
                &expected,
                4096,
                &[(Key::new(300, EXTENT_DATA_KEY, 0), item)],
            )
        };
        let mut fs = filesystem(None);
        let extents = fs.trees.get_mut(&EXTENT_TREE_OBJECTID).unwrap();
        extents.items.extend([
            shared_data_extent(40 * MIB, MIB, 60 * MIB),
            shared_data_extent(41 * MIB, MIB, 50 * MIB),
            tree_block(50 * MIB, 256),
            tree_block(60 * MIB, 256),
        ]);
        fs.blocks = Some(FakeBlocks {
            fsid,
            blocks: [
                (50 * MIB, block(50 * MIB, 41 * MIB, MIB / 4)),
                (60 * MIB, block(60 * MIB, 40 * MIB, MIB / 2)),
            ]
            .into_iter()
            .collect(),
            reads: Vec::new(),
        });
        fs
    }

    fn two_leaf_overview(budget: u64, leaf_reads: LeafReadPlan) -> (OverviewData, ScanStats) {
        let outcome = scan_filesystem(
            &mut two_leaf_filesystem(),
            &[volume()],
            &Marks::default(),
            ScanOptions {
                pinned_read_budget: budget,
                leaf_reads,
                ..ScanOptions::default()
            },
            &mut Recorder {
                stop: NoStop,
                events: Vec::new(),
            },
            spill(),
            &mut NoCache,
        )
        .unwrap();
        (outcome.overview.unwrap(), outcome.stats)
    }

    const WINDOW: LeafReadPlan = LeafReadPlan {
        window: 8,
        threads: 2,
        cache_leaves: 64,
    };

    #[test]
    fn a_pinned_read_window_measures_what_reading_one_extent_at_a_time_does() {
        let (one, one_stats) = two_leaf_overview(10, LeafReadPlan::default());
        let (window, window_stats) = two_leaf_overview(10, WINDOW);

        assert_eq!(window, one);
        assert_eq!(one.pinned_unreferenced_bytes, MIB / 2 + 3 * MIB / 4);
        assert_eq!(window_stats.pinned_leaf_reads, 2);
        assert_eq!(one_stats.pinned_leaf_reads, 2);
    }

    #[test]
    fn a_pinned_read_window_reads_its_leaves_together_in_address_order() {
        let (one, _) = two_leaf_overview(1, LeafReadPlan::default());
        let (window, _) = two_leaf_overview(1, WINDOW);

        assert_eq!(one.pinned_unreferenced_bytes, MIB / 2);
        assert_eq!(window.pinned_unreferenced_bytes, 3 * MIB / 4);
    }

    /// `filesystem` with 257 a snapshot of share 256 holding `/big`
    /// and, alone, `/old` (8 MiB).
    fn snapshot_filesystem() -> FakeFilesystem {
        use crate::btrfs::parser::testing::encode_root_item;
        use crate::btrfs::parser::{ROOT_SUBVOL_RDONLY, RootItem};
        let mut fs = filesystem(None);
        let snapshot = RootItem {
            generation: 30,
            root_dirid: FIRST_FREE_OBJECTID,
            bytenr: 0,
            last_snapshot: 0,
            flags: ROOT_SUBVOL_RDONLY,
            refs: 1,
            uuid: Some([7; 16]),
            parent_uuid: Some([6; 16]),
            otime: Some(1_780_000_000),
        };
        let share = RootItem {
            generation: 20,
            flags: 0,
            uuid: Some([6; 16]),
            parent_uuid: None,
            ..snapshot
        };
        let key = |id| Key::new(id, crate::btrfs::keys::ROOT_ITEM_KEY, 0);
        let mut root = root_tree(20);
        root[1] = (key(256), encode_root_item(&share, true));
        root[3] = (key(257), encode_root_item(&snapshot, true));
        fs.root = FakeSource::new(root, 2);
        let tree = vec![
            inode(256, DIR, 0, 1),
            whole_extent(257, 0, 20 * MIB, 4 * MIB),
            whole_extent(258, 0, 30 * MIB, 8 * MIB),
        ];
        fs.trees.insert(257, FakeSource::new(tree, 3));
        let extents = fs.trees.get_mut(&EXTENT_TREE_OBJECTID).unwrap();
        extents
            .items
            .push(data_extent(40 * MIB, 8 * MIB, &[(257, 258)]));
        fs
    }

    fn snapshot_scan() -> SnapshotScan {
        SnapshotScan {
            depth: crate::snapshots::Depth::Newest(64),
            owner_budget: 64,
            global_owner_budget: crate::snapshots::DEFAULT_GLOBAL_OWNER_BUDGET,
            held_files_budget: crate::snapshots::DEFAULT_HELD_FILES_BUDGET,
        }
    }

    #[test]
    fn a_snapshot_scan_reads_the_snapshots_of_each_materialized_share() {
        use crate::snapshots::{Hold, Holding};
        let mut fs = snapshot_filesystem();
        let marks = Marks::default();
        let mut control = Recorder {
            stop: NoStop,
            events: Vec::new(),
        };
        let outcome = scan_filesystem(
            &mut fs,
            &[volume()],
            &marks,
            ScanOptions {
                snapshots: Some(snapshot_scan()),
                owner_budget: crate::snapshots::DEFAULT_OWNER_BUDGET,
                ..ScanOptions::default()
            },
            &mut control,
            spill(),
            &mut NoCache,
        )
        .unwrap();
        let analysis = outcome
            .snapshots
            .unwrap_or_else(|| panic!("analysis failed: {:?}", outcome.snapshot_error));
        let holding = |owners: Vec<u64>, bytes| Holding {
            pinned_unreferenced_bytes: (owners.len() == 1).then_some(0),
            owners,
            hold: Hold::Resolved,
            bytes,
            extents: 1,
        };
        assert_eq!(
            analysis.holdings,
            vec![
                holding(vec![256, 257], 4 * MIB),
                holding(vec![257], 8 * MIB)
            ]
        );
        assert_eq!(analysis.shares.len(), 1);
        assert_eq!(outcome.coverage.snapshots_materialized, 1);
        assert_eq!(outcome.coverage.snapshots_not_materialized, 0);
        assert!(control.events.contains(&"snapshots of 2".to_string()));
    }

    #[test]
    fn a_volume_scan_attaches_nested_subvolumes_and_skips_snapshots() {
        let (outcome, events) = scan(&mut filesystem(None), &[volume()]);
        assert_eq!(outcome.totals.logical, 5 * MIB);
        assert_eq!(outcome.totals.physical, 5 * MIB);
        let roles: Vec<(u64, SubvolumeRole)> =
            outcome.subvolumes.iter().map(|s| (s.id, s.role)).collect();
        assert_eq!(
            roles,
            [
                (5, SubvolumeRole::Scanned),
                (256, SubvolumeRole::Scanned),
                (257, SubvolumeRole::SnapshotSkipped)
            ]
        );
        assert_eq!(outcome.coverage.snapshots_not_materialized, 1);
        let snap = record(&outcome, b"/v/@sharesnap/snap");
        assert!(snap.flags.contains(NodeFlags::SNAPSHOT));
        assert_eq!(outcome.consistency, Consistency::ObservedStable);
        assert_eq!(outcome.stats.subvolumes_discovered, 3);
        assert_eq!(outcome.stats.subvolumes_scanned, 2);
        assert_eq!(outcome.stats.entries, 6);
        assert_eq!(outcome.stats.extents, 2);
        assert_eq!(
            events,
            [
                "discovered 3 (2 writable)",
                "tree 5",
                "read 5 3",
                "tree 256",
                "read 256 2",
                "resolve 2",
                "aggregate 7"
            ]
        );
    }

    #[test]
    fn a_volume_scan_captures_space_at_both_ends() {
        let mut fs = filesystem(None);
        let sample = SpaceSnapshot {
            statfs: Ok(FsSpace {
                total_bytes: 1000,
                free_bytes: 400,
                available_bytes: 350,
            }),
            block_groups: Ok(vec![BlockGroupSpace {
                flags: 1,
                total_bytes: 800,
                used_bytes: 600,
            }]),
        };
        fs.space = Some(sample.clone());

        let (outcome, _) = scan(&mut fs, &[volume()]);

        assert_eq!(
            outcome.space,
            Some(SpaceWindow {
                start: Some(sample.clone()),
                end: Some(sample),
            })
        );
        assert_eq!(fs.space_reads, 2);
    }

    #[test]
    fn a_volume_scan_streams_unselected_data_extents_into_overview() {
        let (outcome, _) = scan(&mut filesystem(None), &[volume()]);
        let overview = outcome.overview.expect("volume overview data");

        assert_eq!(overview.extent_count, 3);
        assert_eq!(overview.extent_bytes, 13 * MIB);
        assert_eq!(overview.share_live[&256].bytes, 4 * MIB);
        assert_eq!(overview.unknown_extents, 1);
        assert_eq!(overview.unknown.values().copied().sum::<u64>(), 8 * MIB);
        assert_eq!(overview.system_other_bytes, MIB);
    }

    fn group_of(ranges: &[(u32, u32)], disk_bytes: u64) -> Group {
        let uses = ranges
            .iter()
            .map(|&(start, end)| {
                let extent = crate::model::extent::ExtentUse {
                    bytenr: 10 * MIB,
                    inode: 0,
                    start,
                    end,
                    flags: 0,
                    disk_bytes,
                };
                let holder = crate::model::accounting::Holder {
                    tree: FS_TREE_OBJECTID,
                    objectid: 257,
                    external_names: false,
                    shared_leaf: false,
                };
                let held = HeldUse {
                    extent,
                    holder,
                    several_names: false,
                };
                (0, held)
            })
            .collect();
        Group {
            bytenr: 10 * MIB,
            uses,
        }
    }

    fn refs_with(backref: DataBackref) -> ExtentRefs {
        ExtentRefs {
            num_bytes: 4096,
            refs: 1,
            backrefs: vec![backref],
            malformed: false,
            ..ExtentRefs::default()
        }
    }

    const DIRECT: DataBackref = DataBackref::Direct {
        root: FS_TREE_OBJECTID,
        objectid: 257,
        offset: 0,
        count: 1,
    };

    #[test]
    fn overview_pinned_bytes_name_why_they_are_unknown() {
        let direct = refs_with(DIRECT);
        let shared = refs_with(DataBackref::Shared {
            parent: 20 * MIB,
            count: 1,
        });
        let half = group_of(&[(0, 2048)], 4096);
        let beyond = group_of(&[(0, 8192)], 4096);

        assert_eq!(overview_pinned(Some(&half), &direct), Ok(2048));
        assert_eq!(
            overview_pinned(None, &direct),
            Err(PinnedRangesSubcase::NoScannedUse)
        );
        assert_eq!(
            overview_pinned(Some(&half), &shared),
            Err(PinnedRangesSubcase::SharedReference)
        );
        assert_eq!(
            overview_pinned(Some(&beyond), &direct),
            Err(PinnedRangesSubcase::RangeUnmappable)
        );
    }

    #[test]
    fn shared_references_cannot_pin_an_extent_scanned_ranges_already_cover() {
        let shared = refs_with(DataBackref::Shared {
            parent: 20 * MIB,
            count: 1,
        });
        let whole = group_of(&[(0, 2048), (1024, 4096)], 4096);

        assert_eq!(overview_pinned(Some(&whole), &shared), Ok(0));
    }

    #[test]
    fn a_volume_scan_records_the_roots_awaiting_the_cleaner() {
        let mut items = root_tree(20);
        items.push(pending_root_item(300, 15));
        let mut fs = filesystem(None);
        fs.root = FakeSource::new(items, 2);
        let (outcome, _) = scan(&mut fs, &[volume()]);
        let overview = outcome.overview.expect("volume overview data");

        assert_eq!(overview.pending_roots, Some(vec![300]));
    }

    /// A source holding leaf 20 MiB (generation 7) with one item for
    /// `[2048, 4096)` of the 10 MiB extent, and root 300 with one item for
    /// `[0, 1024)` of it; `leaves` reads with `budget`.
    fn leaf_world(
        budget: u64,
    ) -> (
        crate::btrfs::walk::testing::FakeTrees,
        LeafRanges<crate::btrfs::leaf_ranges::testing::FakeBlocks>,
    ) {
        use crate::btrfs::leaf_ranges::testing::{FakeBlocks, file_item};
        use crate::btrfs::tree_block::{Expected, testing::leaf};
        let fsid = [3; 16];
        let item = |start, len| file_item(10 * MIB, 4096, start, len);
        let expected = Expected {
            bytenr: 20 * MIB,
            generation: 7,
            fsid,
        };
        let block = leaf(
            &expected,
            4096,
            &[(Key::new(258, EXTENT_DATA_KEY, 2048), item(2048, 2048))],
        );
        let source = crate::btrfs::walk::testing::FakeTrees::new([
            (
                EXTENT_TREE_OBJECTID,
                vec![(
                    Key::new(20 * MIB, METADATA_ITEM_KEY, 0),
                    encode_tree_block(1, false, &[BlockBackref::Root(300)]),
                )],
            ),
            (
                300,
                vec![(Key::new(257, EXTENT_DATA_KEY, 0), item(0, 1024))],
            ),
        ]);
        let blocks = FakeBlocks {
            fsid,
            blocks: [(20 * MIB, block)].into_iter().collect(),
            reads: Vec::new(),
        };
        (source, LeafRanges::new(blocks, budget))
    }

    #[test]
    fn unscanned_references_are_read_to_measure_pinned_bytes() {
        let shared = refs_with(DataBackref::Shared {
            parent: 20 * MIB,
            count: 1,
        });
        let unscanned = refs_with(DataBackref::Direct {
            root: 300,
            objectid: 257,
            offset: 0,
            count: 1,
        });
        let half = group_of(&[(0, 2048)], 4096);
        let (mut source, mut leaves) = leaf_world(10);
        let mut read = |group, refs| {
            overview_pinned_read(
                &mut source,
                10 * MIB,
                group,
                refs,
                Some(&mut leaves),
                &mut NoStop,
            )
            .unwrap()
        };

        assert_eq!(read(Some(&half), &shared), Ok(0));
        assert_eq!(read(None, &unscanned), Ok(3072));
        assert_eq!(read(Some(&half), &refs_with(DIRECT)), Ok(2048));
    }

    #[test]
    fn unread_references_name_why_their_pinned_bytes_are_unknown() {
        let shared = |count| {
            refs_with(DataBackref::Shared {
                parent: 20 * MIB,
                count,
            })
        };
        let half = group_of(&[(0, 2048)], 4096);
        let (mut source, mut spent) = leaf_world(0);
        let (_, mut unused) = leaf_world(10);
        let no_leaves: Option<&mut LeafRanges<crate::btrfs::leaf_ranges::testing::FakeBlocks>> =
            None;

        assert_eq!(
            overview_pinned_read(
                &mut source,
                10 * MIB,
                Some(&half),
                &shared(1),
                Some(&mut spent),
                &mut NoStop
            )
            .unwrap(),
            Err(PinnedRangesSubcase::ReadBudgetExhausted)
        );
        assert_eq!(
            overview_pinned_read(
                &mut source,
                10 * MIB,
                Some(&half),
                &shared(2),
                Some(&mut unused),
                &mut NoStop
            )
            .unwrap(),
            Err(PinnedRangesSubcase::LeafUnverified)
        );
        assert_eq!(
            overview_pinned_read(
                &mut source,
                10 * MIB,
                Some(&half),
                &shared(1),
                no_leaves,
                &mut NoStop
            )
            .unwrap(),
            Err(PinnedRangesSubcase::SharedReference)
        );
    }

    #[test]
    fn a_shared_tree_owner_budget_exhaustion_is_counted_as_unknown() {
        let refs = ExtentRefs {
            num_bytes: MIB,
            refs: 1,
            backrefs: vec![DataBackref::Shared {
                parent: 10 * MIB,
                count: 1,
            }],
            malformed: false,
            ..ExtentRefs::default()
        };
        let mut source = FakeSource::new(Vec::new(), 4);
        let (owners, complete) = overview_owners(
            &mut source,
            10 * MIB,
            &refs,
            &mut owners::OwnerIndex::default(),
            &SubvolumeSet::default(),
            &mut OwnerWalk::new(0),
            None::<&mut LeafRanges<crate::btrfs::leaf_ranges::testing::FakeBlocks>>,
            &mut NoStop,
        )
        .unwrap();
        let part = attribute_extent(&ExtentInput {
            bytes: MIB,
            refs_complete: complete,
            owners: Some(owners),
            pinned_bytes: Ok(0),
            reclaimable: false,
        });

        assert_eq!(part.kind, crate::overview::PartKind::Unknown);
        assert_eq!(
            part.unknown_reason,
            Some(UnknownReason::OwnerBudgetExhausted)
        );
    }

    #[test]
    fn references_from_a_snapshot_retain_data_the_live_tree_would_free() {
        let (outcome, _) = scan(&mut filesystem(None), &[volume()]);
        let totals = outcome.totals;
        assert_eq!((totals.reclaimable, totals.unknown), (MIB, 0));
        assert_eq!(outcome.coverage.reference_accounting, "partial");
        assert_eq!(outcome.coverage.unresolved_extents, 1);
        assert_eq!(
            outcome.accounting.blocker_bytes(Blockers::SNAPSHOT),
            4 * MIB
        );
        assert_eq!(
            record(&outcome, b"/v/share/big").blockers,
            Blockers::SNAPSHOT
        );
        assert_eq!(outcome.stats.reference_searches, 1);
        assert!(outcome.stats.item_bytes > 0);
    }

    #[test]
    fn unresolved_references_make_accounting_partial() {
        let mut fs = filesystem(None);
        fs.trees
            .insert(EXTENT_TREE_OBJECTID, FakeSource::new(Vec::new(), 3));
        let (outcome, _) = scan(&mut fs, &[volume()]);
        assert_eq!(outcome.totals.unknown, 5 * MIB);
        assert_eq!(outcome.coverage.reference_accounting, "partial");
        assert_eq!(outcome.coverage.unresolved_extents, 2);
    }

    /// Mid-balance: a relocation tree copies subvolume 256, and the data
    /// relocation tree (-9) holds a new copy of `/f`'s extent.
    #[test]
    fn a_balance_caught_mid_relocation_is_changed_and_unknown() {
        let mut reloc = root_item(TREE_RELOC_OBJECTID, 60, false);
        reloc.0.offset = 256;
        let mut tree = root_tree(20);
        tree.push(reloc);
        let mut fs = filesystem(None);
        fs.root = FakeSource::new(tree, 2);
        let data_reloc = u64::MAX - 8;
        let extents = vec![
            data_extent(10 * MIB, MIB, &[(FS_TREE_OBJECTID, 257), (data_reloc, 257)]),
            data_extent(20 * MIB, 4 * MIB, &[(256, 257), (257, 257)]),
        ];
        fs.trees
            .insert(EXTENT_TREE_OBJECTID, FakeSource::new(extents, 3));
        let (outcome, _) = scan(&mut fs, &[volume()]);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert!(
            outcome
                .change_reasons
                .contains(&ChangeReason::RelocationTree { subvolume_id: 256 }),
            "{:?}",
            outcome.change_reasons
        );
        assert_eq!(outcome.totals.unknown, MIB);
    }

    #[test]
    fn a_reference_from_an_undiscovered_root_is_unknown() {
        let mut fs = filesystem(None);
        let extents = vec![
            data_extent(10 * MIB, MIB, &[(FS_TREE_OBJECTID, 257), (999, 257)]),
            data_extent(20 * MIB, 4 * MIB, &[(256, 257), (257, 257)]),
        ];
        fs.trees
            .insert(EXTENT_TREE_OBJECTID, FakeSource::new(extents, 3));
        let (outcome, _) = scan(&mut fs, &[volume()]);
        assert_eq!(outcome.totals.unknown, MIB);
        assert_eq!(outcome.coverage.reference_accounting, "partial");
        assert_eq!(outcome.coverage.unresolved_extents, 1);
    }

    #[test]
    fn a_generation_change_marks_the_scan_changed() {
        let (outcome, _) = scan(&mut filesystem(Some(root_tree(21))), &[volume()]);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        let share = outcome.subvolumes.iter().find(|s| s.id == 256).unwrap();
        assert_eq!(
            (share.generation_before, share.generation_after),
            (20, Some(21))
        );
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::SubvolumeGenerationChanged {
                subvolume_id: 256,
                selected: true,
                generation_before: 20,
                generation_after: 21,
            }]
        );
    }

    #[test]
    fn live_scan_finishes_with_churn_and_unresolved_bytes_without_retrying() {
        let mut fs = filesystem(Some(root_tree(21)));
        fs.trees
            .get_mut(&256)
            .unwrap()
            .items
            .push(entry(256, 3, "gone", 999));
        fs.trees
            .get_mut(&256)
            .unwrap()
            .items
            .sort_by_key(|(key, _)| *key);
        let (outcome, events) = scan(&mut fs, &[volume()]);
        assert_eq!(outcome.coverage.dangling_entries, 1);
        let completion =
            crate::commands::scan_completion(false, outcome.consistency, &outcome.coverage);
        assert_eq!(completion, crate::document::Completion::Complete);
        assert_eq!(completion.exit_code(None), 0);
        let accuracy = outcome
            .coverage
            .approximation
            .as_ref()
            .expect("accuracy evidence");
        assert_eq!(accuracy.changed_subvolumes, 1);
        assert_eq!(accuracy.unaccounted_bytes, 8 * MIB);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("tree 256"))
                .count(),
            1
        );
    }

    #[test]
    fn live_scan_still_aborts_on_io_and_permission_errors() {
        struct Broken(i32);
        impl MetadataSource for Broken {
            fn search(&mut self, _: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
                Err(io::Error::from_raw_os_error(self.0).into())
            }
        }
        for errno in [libc::EIO, libc::EACCES, libc::EPERM] {
            let stopped = scan_filesystem(
                &mut Broken(errno),
                &[volume()],
                &Marks::default(),
                ScanOptions::default(),
                &mut StopAt(100),
                spill(),
                &mut NoCache,
            )
            .unwrap_err();
            assert!(
                matches!(stopped.error, WalkError::Source(SourceError::Io(e)) if e.raw_os_error() == Some(errno))
            );
        }
    }

    /// `filesystem` whose share (tree 256) holds the state directory
    /// `/share/state` (inode 300) with the report `r` (301); transaction
    /// 21 wrote both, and `/share/big` too when `user_changed`.
    fn state_filesystem(root_after: Vec<TreeItem>, user_changed: bool) -> FakeFilesystem {
        let mut fs = filesystem(Some(root_after));
        let big_transid = if user_changed { 21 } else { 1 };
        let mut share = FakeSource::new(
            vec![
                inode(256, DIR, 0, 1),
                inode_ref(256, 256, ".."),
                entry(256, 2, "big", 257),
                entry(256, 3, "state", 300),
                inode_written(257, FILE, big_transid),
                inode_ref(257, 256, "big"),
                whole_extent(257, 0, 20 * MIB, 4 * MIB),
                inode_written(300, DIR, 21),
                inode_ref(300, 256, "state"),
                entry(300, 2, "r", 301),
                inode_written(301, FILE, 21),
                inode_ref(301, 300, "r"),
            ],
            3,
        );
        share.leaf_generation = if user_changed {
            (|key: Key| if key.objectid >= 257 { 21 } else { 20 }) as fn(Key) -> u64
        } else {
            |key| if key.objectid >= 300 { 21 } else { 20 }
        };
        fs.trees.insert(256, share);
        fs
    }

    fn state_marks() -> Marks {
        Marks {
            own_state: [(256, 300)].into(),
            ..Marks::default()
        }
    }

    #[test]
    fn cache_files_created_after_namespace_read_belong_to_this_app_only_with_inode_evidence() {
        let mut fs = state_filesystem(root_tree(21), false);
        // The namespace sees only the empty state directory. Cache sealing then
        // creates inode 301, with an extent visible to the later extent walk.
        let mut after = fs.trees.remove(&256).unwrap();
        let before = FakeSource::new(
            vec![
                inode(256, DIR, 0, 1),
                inode_ref(256, 256, ".."),
                entry(256, 2, "big", 257),
                entry(256, 3, "state", 300),
                inode(257, FILE, 4 * MIB, 1),
                inode_ref(257, 256, "big"),
                whole_extent(257, 0, 20 * MIB, 4 * MIB),
                inode_written(300, DIR, 20),
                inode_ref(300, 256, "state"),
            ],
            3,
        );
        after.items.push(whole_extent(301, 0, 40 * MIB, 2 * MIB));
        after.items.sort_by_key(|item| item.0);
        after.leaf_generation = |_| 21;
        fs.blocks = Some(crate::btrfs::leaf_ranges::testing::FakeBlocks {
            fsid: [0; 16],
            blocks: HashMap::new(),
            reads: Vec::new(),
        });
        fs.trees.insert(256, before);
        fs.trees_after_namespace.insert(256, after);
        fs.trees.insert(
            EXTENT_TREE_OBJECTID,
            FakeSource::new(
                vec![
                    data_extent(10 * MIB, MIB, &[(FS_TREE_OBJECTID, 257)]),
                    data_extent(20 * MIB, 4 * MIB, &[(256, 257), (257, 257)]),
                    data_extent(30 * MIB, 8 * MIB, &[(256, 999)]),
                    data_extent(40 * MIB, 2 * MIB, &[(256, 301)]),
                ],
                3,
            ),
        );
        let (outcome, _) = scan_marked(&mut fs, &[volume()], &state_marks(), &mut NoCache);
        let data = outcome.overview.as_ref().unwrap();
        let service = crate::dsm::service_id_by_code("syno_reclaim");
        assert_eq!(
            data.dsm_services.get(&service),
            Some(&(2 * MIB)),
            "{data:?}"
        );
        assert_eq!(data.unknown.values().sum::<u64>(), 8 * MIB);
        assert_eq!(data.extent_bytes, 15 * MIB);
        assert_eq!(
            data.extent_bytes,
            data.system_other_bytes
                + data.share_live.values().map(|s| s.bytes).sum::<u64>()
                + data.dsm_services.values().sum::<u64>()
                + data.unknown.values().sum::<u64>()
        );
        assert_eq!(outcome.totals.reclaimable, MIB);
        use crate::query::{KindFilter, Measure, TopQuery, TopWalk};
        let path = scratch("own-state-sidecar").join("report.srrep");
        let mut saved_header = header(Vec::new());
        saved_header.overview = Some(data.clone());
        saved_header.totals = outcome.totals;
        report::write_records(
            File::create(&path).unwrap(),
            &saved_header,
            outcome.nodes.count(),
            |w| outcome.nodes.write(w),
        )
        .unwrap();
        for by in [Measure::Physical, Measure::Reclaimable] {
            let query = TopQuery {
                under: b"/v",
                kind: KindFilter::All,
                by,
                services: true,
                include_system: false,
                only_shared: false,
                limit: 200,
            };
            let mut checked = TopWalk::new(b"/v", &query);
            crate::report::ReportReader::open(File::open(&path).unwrap())
                .unwrap()
                .for_each(|path, node| checked.visit(path, node))
                .unwrap();
            let mut checked = checked.finish().unwrap();
            checked.include_own_state(data);
            let own = checked
                .services
                .uses
                .iter()
                .find(|u| u.service == Some(crate::dsm::syno_reclaim()))
                .unwrap();
            assert_eq!(own.metrics.physical, 2 * MIB);
            assert_eq!(own.metrics.reclaimable, 0);
            let json = crate::document::ServiceLine::from(own);
            assert_eq!(json.metrics.reclaimable, None);
            assert_eq!(json.manage_in_id, "syno_reclaim");
            let rows = crate::query::full_rows(&checked.ranking, &checked.services, by, 200);
            let includes = crate::query::inclusions(&rows);
            let distinct = rows
                .iter()
                .zip(&includes)
                .filter(|(_, parent)| parent.is_none())
                .map(|(row, _)| row.full_metrics().physical)
                .sum::<u64>();
            assert_eq!(
                distinct + data.unknown.values().sum::<u64>(),
                data.extent_bytes,
                "new state bytes are not already contained in the old namespace's folder rows"
            );
            for _ in 0..2 {
                assert_eq!(
                    crate::report_summary::index::top(&path, b"/v", &query).unwrap(),
                    Some(checked.clone())
                );
            }
        }
        assert_eq!(
            crate::report_summary::ensure(&path)
                .unwrap()
                .header
                .overview
                .as_ref(),
            Some(data)
        );
    }

    #[test]
    fn a_generation_change_made_only_by_state_dir_writes_is_ignored() {
        let mut fs = state_filesystem(root_tree(21), false);
        let (outcome, _) = scan_marked(&mut fs, &[volume()], &state_marks(), &mut NoCache);
        assert_eq!(outcome.change_reasons, []);
        assert_eq!(outcome.consistency, Consistency::ObservedStable);
        assert_eq!(
            outcome.state_dir_writes,
            [StateDirWrites {
                subvolume_id: 256,
                generation_before: 20,
                generation_after: 21,
                inodes: 2,
            }]
        );
    }

    #[test]
    fn a_change_outside_the_state_dir_keeps_the_generation_change() {
        let mut fs = state_filesystem(root_tree(21), true);
        let (outcome, _) = scan_marked(&mut fs, &[volume()], &state_marks(), &mut NoCache);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(outcome.state_dir_writes, []);
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::SubvolumeGenerationChanged {
                subvolume_id: 256,
                selected: true,
                generation_before: 20,
                generation_after: 21,
            }]
        );
    }

    #[test]
    fn other_subvolumes_keep_their_generation_changes() {
        let mut after = root_tree(21);
        after[0] = root_item(FS_TREE_OBJECTID, 11, false);
        let mut fs = state_filesystem(after, false);
        let (outcome, _) = scan_marked(&mut fs, &[volume()], &state_marks(), &mut NoCache);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::SubvolumeGenerationChanged {
                subvolume_id: FS_TREE_OBJECTID,
                selected: true,
                generation_before: 10,
                generation_after: 11,
            }]
        );
        assert_eq!(outcome.state_dir_writes.len(), 1);
    }

    #[test]
    fn without_a_state_dir_in_the_subvolume_its_change_stays() {
        let mut fs = state_filesystem(root_tree(21), false);
        let (outcome, _) = scan(&mut fs, &[volume()]);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(outcome.state_dir_writes, []);
    }

    fn file_f() -> RootRequest {
        RootRequest {
            path: b"/v/f".to_vec(),
            scope: Scope::Path,
            tree: FS_TREE_OBJECTID,
            ino: 257,
        }
    }

    #[test]
    fn a_generation_change_outside_the_scanned_roots_marks_the_scan_changed() {
        // Subvolume 256 is not scanned, yet could reflink `/f`'s extent.
        let (outcome, _) = scan(&mut filesystem(Some(root_tree(21))), &[file_f()]);
        assert_eq!(outcome.totals.reclaimable, MIB);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::SubvolumeGenerationChanged {
                subvolume_id: 256,
                selected: false,
                generation_before: 20,
                generation_after: 21,
            }]
        );
    }

    #[test]
    fn a_subvolume_created_during_the_scan_marks_it_changed() {
        let mut after = root_tree(20);
        after.push(root_item(259, 40, false));
        let (outcome, _) = scan(&mut filesystem(Some(after)), &[file_f()]);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::SubvolumeAppeared {
                subvolume_id: 259,
                generation: 40,
                has_uuid: true,
                snapshot_of: None,
            }]
        );
    }

    #[test]
    fn a_snapshot_taken_during_the_scan_names_its_source() {
        let mut after = root_tree(20);
        after[1] = snapshotted_root_item(256, 21, 21);
        after.push(snapshot_root_item(259, 21, 256));
        let mut fs = filesystem(Some(after));
        let mut before = root_tree(20);
        before[1] = snapshotted_root_item(256, 20, 0);
        fs.root = FakeSource::new(before, 2);
        let (outcome, _) = scan(&mut fs, &[volume()]);
        assert!(
            outcome
                .change_reasons
                .contains(&ChangeReason::SubvolumeAppeared {
                    subvolume_id: 259,
                    generation: 21,
                    has_uuid: true,
                    snapshot_of: Some(256),
                })
        );
        assert!(
            outcome
                .change_reasons
                .contains(&ChangeReason::SubvolumeSnapshotted {
                    subvolume_id: 256,
                    last_snapshot_before: 0,
                    last_snapshot_after: 21,
                })
        );
        let share = outcome.subvolumes.iter().find(|s| s.id == 256).unwrap();
        assert_eq!(share.snapshots, [257, 259]);
    }

    #[test]
    fn a_scanned_subvolume_keeps_its_uuid_and_snapshots_at_the_end() {
        let mut after = root_tree(20);
        after[1] = snapshotted_root_item(256, 20, 15);
        let mut fs = filesystem(Some(after.clone()));
        fs.root = FakeSource::new(after, 2);
        let (outcome, _) = scan(&mut fs, &[volume()]);
        let share = outcome.subvolumes.iter().find(|s| s.id == 256).unwrap();
        let uuid = hex_bytes::encode(&uuid_of(256));
        assert_eq!(share.uuid.as_deref(), Some(uuid.as_str()));
        assert_eq!(share.last_snapshot, Some(15));
    }

    #[test]
    fn a_scanned_subvolume_that_vanished_marks_the_scan_changed() {
        let mut after = root_tree(20);
        after.retain(|(key, _)| key.objectid != 256);
        let (outcome, _) = scan(&mut filesystem(Some(after)), &[volume()]);
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::SubvolumeVanished {
                subvolume_id: 256,
                generation_before: 20,
            }]
        );
    }

    /// A filesystem whose `vanished` trees fail their searches with ENOENT,
    /// as the kernel does once a subvolume is vanished.
    struct Vanishing {
        fs: FakeFilesystem,
        vanished: BTreeSet<u64>,
    }

    impl MetadataSource for Vanishing {
        fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            if self.vanished.contains(&range.tree_id) {
                return Err(std::io::Error::from_raw_os_error(libc::ENOENT).into());
            }
            self.fs.search(range)
        }

        fn maintenance_probes(&mut self) -> Vec<Probe> {
            self.fs.maintenance_probes()
        }
    }

    fn scan_vanishing(
        vanished: &[u64],
        roots: &[RootRequest],
    ) -> Result<ScanOutcome, Box<ScanStopped>> {
        let mut source = Vanishing {
            fs: filesystem(None),
            vanished: vanished.iter().copied().collect(),
        };
        let mut control = Recorder {
            stop: NoStop,
            events: Vec::new(),
        };
        scan_filesystem(
            &mut source,
            roots,
            &Marks::default(),
            ScanOptions::default(),
            &mut control,
            spill(),
            &mut NoCache,
        )
    }

    #[test]
    fn a_subvolume_deleted_before_its_tree_is_read_marks_the_scan_changed() {
        let outcome = scan_vanishing(&[256], &[volume()]).unwrap();
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::SubvolumeVanished {
                subvolume_id: 256,
                generation_before: 20,
            }]
        );
        assert_eq!(
            outcome.coverage.namespace_uncertain_subvolumes,
            Some(vec![256])
        );
        assert!(!roles(&outcome).iter().any(|&(id, _)| id == 256));
        assert_eq!(outcome.stats.subvolumes_scanned, 1);
    }

    #[test]
    fn live_scan_skips_a_vanished_snapshot_and_keeps_other_results() {
        let mut source = Vanishing {
            fs: snapshot_filesystem(),
            vanished: BTreeSet::from([257]),
        };
        let mut control = Recorder {
            stop: NoStop,
            events: Vec::new(),
        };
        let outcome = scan_filesystem(
            &mut source,
            &[volume()],
            &Marks::default(),
            ScanOptions {
                snapshots: Some(snapshot_scan()),
                ..ScanOptions::default()
            },
            &mut control,
            spill(),
            &mut NoCache,
        )
        .unwrap();
        assert!(outcome.snapshot_error.is_none());
        let analysis = outcome.snapshots.expect("remaining snapshot results");
        assert!(
            !analysis
                .shares
                .iter()
                .flat_map(|share| share.materialized())
                .any(|snapshot| snapshot.id == 257)
        );
        assert!(outcome.coverage.approximation.is_some());
    }

    #[test]
    fn a_path_root_in_a_deleted_subvolume_is_an_unreadable_root() {
        let big = RootRequest {
            path: b"/v/share/big".to_vec(),
            scope: Scope::Path,
            tree: 256,
            ino: 257,
        };
        let outcome = scan_vanishing(&[256], &[big]).unwrap();
        assert_eq!(outcome.consistency, Consistency::Partial);
        assert_eq!(outcome.root_errors.len(), 1);
        assert_eq!(outcome.root_errors[0].0, b"/v/share/big");
        assert_eq!(
            outcome.coverage.namespace_uncertain_subvolumes,
            Some(vec![256])
        );
        assert!(
            outcome
                .change_reasons
                .contains(&ChangeReason::SubvolumeVanished {
                    subvolume_id: 256,
                    generation_before: 20,
                })
        );
    }

    #[test]
    fn a_path_scan_of_an_unchanged_filesystem_is_observed_stable() {
        let (outcome, _) = scan(&mut filesystem(Some(root_tree(20))), &[file_f()]);
        assert_eq!(outcome.consistency, Consistency::ObservedStable);
        assert_eq!(outcome.change_reasons, []);
    }

    #[test]
    fn a_balance_or_replace_at_either_end_marks_the_scan_changed() {
        for probes in [[true, false], [false, true], [true, true]] {
            let mut fs = filesystem(None);
            fs.maintenance = probes.into();
            let (outcome, _) = scan(&mut fs, &[volume()]);
            assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
            assert_eq!(
                outcome.change_reasons,
                [ChangeReason::MaintenanceRunning {
                    operation: Maintenance::Balance
                }]
            );
        }
    }

    #[test]
    fn a_failed_maintenance_probe_marks_the_scan_changed() {
        struct Failing(FakeFilesystem);
        impl MetadataSource for Failing {
            fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
                self.0.search(range)
            }

            fn maintenance_probes(&mut self) -> Vec<Probe> {
                let failed = std::io::Error::from_raw_os_error(libc::EPERM);
                vec![(Maintenance::DevReplace, Err(failed))]
            }
        }
        let mut control = Recorder {
            stop: NoStop,
            events: Vec::new(),
        };
        let mut fs = Failing(filesystem(None));
        let outcome = scan_filesystem(
            &mut fs,
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut control,
            spill(),
            &mut NoCache,
        );
        let outcome = outcome.unwrap();
        assert_eq!(outcome.consistency, Consistency::ChangedDuringScan);
        assert_eq!(
            outcome.change_reasons,
            [ChangeReason::MaintenanceProbeFailed {
                operation: Maintenance::DevReplace,
                errno: Some(libc::EPERM),
            }]
        );
    }

    #[test]
    fn a_file_selection_reads_only_its_inode_items() {
        let mut fs = filesystem(None);
        let (outcome, _) = scan(&mut fs, &[file_f()]);
        assert_eq!(outcome.totals.reclaimable, MIB);
        assert_eq!(outcome.stats.items, 3); // INODE_ITEM, EXTENT_DATA and its extent-tree item.
        assert!(
            fs.trees[&FS_TREE_OBJECTID]
                .calls
                .iter()
                .all(|range| { range.min.objectid == 257 && range.max.objectid == 257 })
        );
    }

    #[test]
    fn file_only_reads_preserve_external_and_selected_hard_links() {
        let mut fs = filesystem(None);
        fs.trees.insert(
            FS_TREE_OBJECTID,
            FakeSource::new(
                vec![
                    inode(256, DIR, 0, 1),
                    entry(256, 2, "f", 257),
                    entry(256, 3, "outside", 258),
                    inode(258, DIR, 0, 1),
                    entry(258, 2, "alias", 257),
                    inode(257, FILE, MIB, 2),
                    whole_extent(257, 0, 10 * MIB, MIB),
                ],
                1,
            ),
        );
        let (one, _) = scan(&mut fs, &[file_f()]);
        assert_eq!(one.totals.reclaimable, 0);
        assert_eq!(record(&one, b"/v/f").blockers, Blockers::HARD_LINK);
        assert!(
            fs.trees[&FS_TREE_OBJECTID]
                .calls
                .iter()
                .any(|range| { range.min == Key::MIN && range.max == Key::MAX }),
            "hard-linked selections keep the complete namespace proof"
        );
        let mut alias = file_f();
        alias.path = b"/v/outside/alias".to_vec();
        let (both, _) = scan(&mut fs, &[alias, file_f()]);
        assert_eq!(both.totals.physical, MIB);
        assert_eq!(both.totals.reclaimable, MIB);
    }

    #[test]
    fn file_only_reads_keep_recycle_membership_and_ctime() {
        let mut fs = filesystem(None);
        let tree = fs.trees.get_mut(&FS_TREE_OBJECTID).unwrap();
        tree.items
            .retain(|(key, _)| *key != Key::new(257, crate::btrfs::keys::INODE_ITEM_KEY, 0));
        tree.items.push(inode_with_ctime(
            257,
            FILE,
            MIB,
            1,
            crate::btrfs::parser::BtrfsTimespec { sec: 123, nsec: 0 },
        ));
        tree.items.sort_by_key(|(key, _)| *key);
        let mut root = file_f();
        root.path = b"/v/share/#recycle/old".to_vec();
        let (outcome, _) = scan(&mut fs, &[root]);
        assert!(
            record(&outcome, b"/v/share/#recycle/old")
                .flags
                .contains(NodeFlags::RECYCLE)
        );
        assert_eq!(
            outcome.recycle_ctimes,
            vec![RecycleEntryCtime {
                tree: FS_TREE_OBJECTID,
                inode: 257,
                ctime: 123
            }]
        );
    }

    #[test]
    fn file_namespace_reserves_recycle_ctimes_for_its_selected_roots() {
        let ns = Namespace::read(
            &mut FakeSource::new(vec![inode(257, FILE, MIB, 1)], 1),
            FS_TREE_OBJECTID,
            &mut NoStop,
        )
        .unwrap();
        let plain = file_f();
        let mut recycled = file_f();
        recycled.path = b"/v/share/#recycle/old".to_vec();
        let ordinary_bytes = roots_expansion(&ns, &[&plain]);
        let recycled_bytes = roots_expansion(&ns, &[&recycled]);
        assert!(recycled_bytes >= ordinary_bytes + size_of::<RecycleEntryCtime>() as u64);
    }

    #[test]
    fn path_roots_share_one_read_of_their_tree() {
        let roots = [
            RootRequest {
                path: b"/v/f".to_vec(),
                scope: Scope::Path,
                tree: FS_TREE_OBJECTID,
                ino: 257,
            },
            RootRequest {
                path: b"/v/@sharesnap".to_vec(),
                scope: Scope::Path,
                tree: FS_TREE_OBJECTID,
                ino: 258,
            },
        ];
        let (outcome, events) = scan(&mut filesystem(None), &roots);
        assert_eq!(outcome.totals.physical, MIB);
        assert_eq!(events.iter().filter(|e| e.starts_with("tree")).count(), 1);
    }

    #[test]
    fn a_vanished_root_is_a_partial_result() {
        let roots = [
            volume(),
            RootRequest {
                path: b"/w/gone".to_vec(),
                scope: Scope::Path,
                tree: FS_TREE_OBJECTID,
                ino: 999,
            },
        ];
        let (outcome, _) = scan(&mut filesystem(None), &roots);
        assert_eq!(outcome.consistency, Consistency::Partial);
        assert_eq!(outcome.root_errors.len(), 1);
        assert_eq!(
            serde_json::to_value(&outcome.coverage).unwrap()["namespace_uncertain_subvolumes"],
            serde_json::json!([FS_TREE_OBJECTID])
        );
        assert_eq!(outcome.root_errors[0].0, b"/w/gone");
        assert_eq!(outcome.coverage.unreadable_roots, Some(1));
    }

    #[test]
    fn a_scan_whose_roots_all_read_records_zero_unreadable_roots() {
        let (outcome, _) = scan(&mut filesystem(None), &[volume()]);
        assert_eq!(outcome.coverage.unreadable_roots, Some(0));
    }

    struct StopAt(u32);

    impl BatchBoundary for StopAt {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            if self.0 == 0 {
                return ControlFlow::Break(StopReason::Paused);
            }
            self.0 -= 1;
            ControlFlow::Continue(())
        }
    }

    impl ScanControl for StopAt {
        fn boundary(&mut self) -> &mut dyn BatchBoundary {
            self
        }

        fn event(&mut self, _: ScanEvent<'_>) {}
    }

    /// Refuses any reservation above its allowance.
    struct Budget {
        allowance: u64,
        largest: u64,
    }

    impl BatchBoundary for Budget {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }

        fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
            self.largest = self.largest.max(bytes);
            if bytes > self.allowance {
                return ControlFlow::Break(StopReason::ResourceLimit);
            }
            ControlFlow::Continue(())
        }
    }

    impl ScanControl for Budget {
        fn boundary(&mut self) -> &mut dyn BatchBoundary {
            self
        }

        fn event(&mut self, _: ScanEvent<'_>) {}
    }

    #[test]
    fn model_growth_is_reserved_before_it_happens() {
        let mut roomy = Budget {
            allowance: u64::MAX,
            largest: 0,
        };
        scan_filesystem(
            &mut filesystem(None),
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut roomy,
            spill(),
            &mut NoCache,
        )
        .unwrap();
        assert!(roomy.largest > 0);
        let mut tight = Budget {
            allowance: roomy.largest - 1,
            largest: 0,
        };
        let result = scan_filesystem(
            &mut filesystem(None),
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut tight,
            spill(),
            &mut NoCache,
        );
        let stopped = result.unwrap_err();
        assert!(matches!(
            stopped.error,
            WalkError::Stopped(StopReason::ResourceLimit)
        ));
        assert!(stopped.stats.subvolumes_discovered > 0);
        assert!(stopped.stats.subvolumes_scanned > 0, "the trees it read");
        assert!(stopped.stats.items > 0 && stopped.stats.batches > 0);
        assert!(stopped.stats.item_bytes > 0, "the metadata bytes it read");
    }

    #[test]
    fn a_stop_inside_a_tree_counts_what_it_read_of_it() {
        let stopped = scan_filesystem(
            &mut filesystem(None),
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut StopInTree(FS_TREE_OBJECTID, false),
            spill(),
            &mut NoCache,
        )
        .unwrap_err();
        assert_eq!(stopped.stats.subvolumes_scanned, 0);
        assert!(stopped.stats.items > 0 && stopped.stats.batches == 1);
        assert!(stopped.stats.item_bytes > 0);
    }

    /// Every reservation a scan makes.
    #[derive(Default)]
    struct Reservations(Vec<u64>);

    impl BatchBoundary for Reservations {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }

        fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
            self.0.push(bytes);
            ControlFlow::Continue(())
        }
    }

    impl ScanControl for Reservations {
        fn boundary(&mut self) -> &mut dyn BatchBoundary {
            self
        }

        fn event(&mut self, _: ScanEvent<'_>) {}
    }

    #[test]
    fn a_path_scan_reserves_its_subtree_not_the_whole_tree() {
        let mut fs = filesystem(None);
        let top = fs.trees.get_mut(&FS_TREE_OBJECTID).unwrap();
        let path = scratch("scan").join("top.refs");
        let ns = Namespace::read_spilling(
            top,
            FS_TREE_OBJECTID,
            crate::btrfs::subvolume::LeafSharing::default(),
            &mut NoStop,
            path,
        )
        .unwrap();
        let whole = ScopeTree::expansion_bytes(ns.counts());
        let file = ScopeTree::expansion_bytes(ns.subtree_counts(257));
        assert!(file < whole);
        let mut log = Reservations::default();
        scan_filesystem(
            &mut filesystem(None),
            &[file_f()],
            &Marks::default(),
            ScanOptions::default(),
            &mut log,
            spill(),
            &mut NoCache,
        )
        .unwrap();
        assert!(log.0.contains(&file), "{:?}", log.0);
        assert!(!log.0.contains(&whole), "{:?}", log.0);
    }

    #[test]
    fn a_stop_request_ends_the_scan() {
        let result = scan_filesystem(
            &mut filesystem(None),
            &[volume()],
            &Marks::default(),
            ScanOptions::default(),
            &mut StopAt(4),
            spill(),
            &mut NoCache,
        );
        assert!(matches!(
            result.map_err(|s| s.error),
            Err(WalkError::Stopped(StopReason::Paused))
        ));
    }

    fn fs_of(
        roots: Vec<TreeItem>,
        trees: Vec<(u64, Vec<TreeItem>)>,
        extents: Vec<TreeItem>,
    ) -> FakeFilesystem {
        let mut sources: HashMap<u64, FakeSource> = trees
            .into_iter()
            .map(|(id, items)| (id, FakeSource::new(items, 3)))
            .collect();
        sources.insert(EXTENT_TREE_OBJECTID, FakeSource::new(extents, 3));
        FakeFilesystem {
            trees_after_namespace: HashMap::new(),
            root: FakeSource::new(roots, 2),
            root_after: None,
            trees: sources,
            maintenance: VecDeque::new(),
            space: None,
            space_reads: 0,
            blocks: None,
        }
    }

    #[test]
    fn many_subvolumes_preserve_physical_totals_and_overview_categories() {
        const ROOTS: u64 = 500;
        let mut roots = vec![root_item(5, 10, false)];
        let mut top = vec![inode(256, DIR, 0, 1)];
        let mut trees = Vec::new();
        let mut extents = Vec::new();
        for root in 256..256 + ROOTS {
            let name = format!("share{root}");
            roots.extend([
                root_item(root, 10, false),
                root_backref(root, 5, 256, &name),
            ]);
            top.push(subvolume_entry(256, root, &name, root));
            let bytenr = root * MIB;
            trees.push((
                root,
                vec![
                    inode(256, DIR, 0, 1),
                    inode(257, DIR, 0, 1),
                    entry(256, 2, "#recycle", 257),
                    inode(258, FILE, MIB, 1),
                    entry(256, 3, "live", 258),
                    whole_extent(258, 0, bytenr, MIB),
                    inode(259, FILE, MIB, 1),
                    entry(257, 4, "deleted", 259),
                    whole_extent(259, 0, (root + ROOTS) * MIB, MIB),
                ],
            ));
            extents.push(data_extent(bytenr, MIB, &[(root, 258)]));
            extents.push(data_extent((root + ROOTS) * MIB, MIB, &[(root, 259)]));
        }
        trees.push((5, top));
        let (outcome, _) = scan(&mut fs_of(roots, trees, extents), &[volume()]);
        assert!(outcome.stopped.is_none());
        assert_eq!(outcome.totals.physical, ROOTS * 2 * MIB);
        assert_eq!(outcome.totals.reclaimable, ROOTS * 2 * MIB);
        let overview = outcome.overview.unwrap();
        assert_eq!(overview.extent_count, ROOTS * 2);
        assert_eq!(overview.extent_bytes, ROOTS * 2 * MIB);
        assert!(overview.unknown.is_empty());
        assert_eq!(
            overview.share_live.values().map(|v| v.bytes).sum::<u64>(),
            ROOTS * MIB
        );
        assert_eq!(
            overview.recycle_bin.values().map(|v| v.bytes).sum::<u64>(),
            ROOTS * MIB
        );
        for root in 256..256 + ROOTS {
            assert_eq!(overview.share_live[&root].bytes, MIB);
            assert_eq!(overview.recycle_bin[&root].bytes, MIB);
        }
    }

    /// Tree 5: `/#recycle/x` (10 MiB) and `/#recycle/s1` (subvolume 256).
    /// Tree 256: `/z` (30 MiB) and `/s2` (subvolume 257). Tree 257: `/y`,
    /// a reflink of `x`.
    fn three_levels() -> FakeFilesystem {
        fs_of(
            vec![
                root_item(FS_TREE_OBJECTID, 10, false),
                root_item(256, 10, false),
                root_backref(256, FS_TREE_OBJECTID, 257, "s1"),
                root_item(257, 10, false),
                root_backref(257, 256, 256, "s2"),
            ],
            vec![
                (
                    FS_TREE_OBJECTID,
                    vec![
                        inode(256, DIR, 0, 1),
                        entry(256, 2, "#recycle", 257),
                        inode(257, DIR, 0, 1),
                        entry(257, 2, "x", 258),
                        subvolume_entry(257, 3, "s1", 256),
                        inode(258, FILE, MIB, 1),
                        whole_extent(258, 0, 10 * MIB, MIB),
                    ],
                ),
                (
                    256,
                    vec![
                        inode(256, DIR, 0, 1),
                        entry(256, 2, "z", 257),
                        subvolume_entry(256, 3, "s2", 257),
                        inode(257, FILE, MIB, 1),
                        whole_extent(257, 0, 30 * MIB, MIB),
                    ],
                ),
                (
                    257,
                    vec![
                        inode(256, DIR, 0, 1),
                        entry(256, 2, "y", 257),
                        inode(257, FILE, MIB, 1),
                        whole_extent(257, 0, 10 * MIB, MIB),
                    ],
                ),
            ],
            vec![
                data_extent(10 * MIB, MIB, &[(FS_TREE_OBJECTID, 258), (257, 257)]),
                data_extent(30 * MIB, MIB, &[(256, 257)]),
            ],
        )
    }

    #[test]
    fn a_reflink_across_nested_subvolumes_is_owned_by_their_common_ancestor() {
        let (outcome, _) = scan(&mut three_levels(), &[volume()]);
        let recycle = record(&outcome, b"/v/#recycle");
        assert_eq!(
            (recycle.metrics.physical, recycle.metrics.reclaimable),
            (2 * MIB, 2 * MIB)
        );
        assert!(recycle.blockers.is_empty());
        let s1 = record(&outcome, b"/v/#recycle/s1");
        assert_eq!(
            (s1.metrics.physical, s1.metrics.reclaimable),
            (2 * MIB, MIB)
        );
        assert_eq!(s1.blockers, Blockers::LIVE_REFLINK);
        let y = record(&outcome, b"/v/#recycle/s1/s2/y");
        assert_eq!(y.blockers, Blockers::LIVE_REFLINK);
        assert!(y.flags.contains(NodeFlags::RECYCLE), "flags cross units");
        assert_eq!(outcome.totals.reclaimable, 2 * MIB);
        assert_eq!(outcome.stats.entries, 7, "each boundary once");
        assert_eq!(outcome.nodes.count(), 7);
    }

    #[test]
    fn roots_in_different_trees_share_an_extent_once() {
        let mut fs = three_levels();
        let roots = [
            RootRequest {
                path: b"/v/#recycle/x".to_vec(),
                scope: Scope::Path,
                tree: FS_TREE_OBJECTID,
                ino: 258,
            },
            RootRequest {
                path: b"/v/#recycle/s1/s2".to_vec(),
                scope: Scope::Path,
                tree: 257,
                ino: 256,
            },
        ];
        let (outcome, _) = scan(&mut fs, &roots);
        assert_eq!(
            (
                outcome.totals.physical,
                outcome.totals.extents,
                outcome.totals.reclaimable
            ),
            (MIB, 1, MIB)
        );
        let x = record(&outcome, b"/v/#recycle/x");
        assert_eq!(
            (x.metrics.reclaimable, x.blockers),
            (0, Blockers::LIVE_REFLINK)
        );
    }

    /// Records nothing but asks the scan to delete snapshots with the
    /// selection.
    struct Deleting {
        stop: NoStop,
        request: Option<DeletionRequest>,
    }

    impl ScanControl for Deleting {
        fn boundary(&mut self) -> &mut dyn BatchBoundary {
            &mut self.stop
        }

        fn event(&mut self, _: ScanEvent<'_>) {}

        fn deletion(&mut self) -> Option<DeletionRequest> {
            self.request.take()
        }
    }

    #[test]
    fn a_selection_deleted_with_a_snapshot_frees_what_only_they_reference() {
        let big = RootRequest {
            path: b"/v/share/big".to_vec(),
            scope: Scope::Path,
            tree: 256,
            ino: 257,
        };
        let mut control = Deleting {
            stop: NoStop,
            request: Some(DeletionRequest {
                snapshots: BTreeSet::from([257]),
                scan: snapshot_scan(),
            }),
        };
        let outcome = scan_filesystem(
            &mut snapshot_filesystem(),
            &[big],
            &Marks::default(),
            ScanOptions::default(),
            &mut control,
            spill(),
            &mut NoCache,
        )
        .unwrap();
        // Alone, the snapshot keeps `/big`; together they free it and
        // the snapshot's own `/old`.
        assert_eq!(record(&outcome, b"/v/share/big").metrics.reclaimable, 0);
        let deletion = outcome.deletion.expect("answered");
        assert_eq!(deletion.snapshots, vec![257]);
        assert_eq!(
            (deletion.freed_bytes, deletion.unknown_bytes),
            (12 * MIB, 0)
        );
        assert_eq!(outcome.deletion_error, None);
    }

    #[test]
    fn a_snapshot_deleted_with_the_selection_that_vanishes_marks_the_scan_changed() {
        let mut fs = snapshot_filesystem();
        let mut after = fs.root.items.clone();
        after.retain(|(key, _)| key.objectid != 257);
        fs.root_after = Some(FakeSource::new(after, 2));
        let mut control = Deleting {
            stop: NoStop,
            request: Some(DeletionRequest {
                snapshots: BTreeSet::from([257]),
                scan: snapshot_scan(),
            }),
        };
        let outcome = scan_filesystem(
            &mut fs,
            &[file_f()],
            &Marks::default(),
            ScanOptions::default(),
            &mut control,
            spill(),
            &mut NoCache,
        )
        .unwrap();
        assert!(
            outcome
                .change_reasons
                .contains(&ChangeReason::SubvolumeVanished {
                    subvolume_id: 257,
                    generation_before: 30,
                })
        );
    }

    #[test]
    fn a_scan_without_a_deletion_request_answers_none() {
        let (outcome, _) = scan(&mut snapshot_filesystem(), &[volume()]);
        assert_eq!(outcome.deletion, None);
    }
}
