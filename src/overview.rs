//! Data-level volume reconciliation (SPEC 5.13).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Why an allocated data extent could not be assigned to a known owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    MissingOwner,
    IncompleteReferences,
    DirectOwnerUnresolved,
    SharedTreeOwnerUnresolved,
    RelocationTree,
    CoverageGap,
    OwnerBudgetExhausted,
    PinnedRangesUnknown,
}

/// Which direct-owner lookup failed while assigning an extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectOwnerSubcase {
    RootNotDiscovered,
    ReadonlyRootParentUuidUnmapped,
    WritableRootInodeUnindexed,
    IndexedPathUnmapped,
    IndexedOwnerConflict,
}

/// Why the unreferenced part of an extent could not be measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinnedRangesSubcase {
    /// Allocation references changed since the retained scan
    ExtentChanged,
    /// A referenced root has been deleted and awaits the Btrfs cleaner
    RootUnsearchable,
    /// No scanned file range references the extent.
    NoScannedUse,
    /// A shared tree block references the extent and scanned ranges leave
    /// part of it unreferenced, so unscanned file ranges may cover it.
    SharedReference,
    /// A scanned range does not fit inside the extent.
    RangeUnmappable,
    /// Reading the unscanned references' leaves would pass the read
    /// budget.
    ReadBudgetExhausted,
    /// A leaf or root did not hold the references the extent tree
    /// declares, or could not be read.
    LeafUnverified,
}

/// Structural facts for an unresolved direct-reference root. No names or
/// paths are retained in this summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectOwnerRootKind {
    NotDiscovered,
    ReadOnly,
    Writable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectOwnerRootClass {
    NotDiscovered,
    Snapshot,
    ActiveBackup,
    Iscsi,
    SharedFolder,
    AtDirectory,
    HashDirectory,
    OtherService,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DirectOwnerDiagnostic {
    pub subcase: DirectOwnerSubcase,
    pub root_id: u64,
    pub root_kind: DirectOwnerRootKind,
    pub root_class: DirectOwnerRootClass,
    pub at_directory: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectOwnerSubcaseBytes {
    pub bytes: u64,
    /// Root ids map to only the requested structural class and byte total.
    pub roots: std::collections::BTreeMap<u64, DirectOwnerRootBytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectOwnerRootBytes {
    pub kind: DirectOwnerRootKind,
    pub class: DirectOwnerRootClass,
    pub at_directory: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    PendingCleanup,
    SnapshotOnly,
    RecycleBin,
    DsmService,
    SystemOther,
    ShareLive,
    SharedBetweenFolders,
    PinnedUnreferenced,
    Unknown,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum LiveOwner {
    Recycle { share: u64 },
    Service { service: u16 },
    SystemOther,
    Share { share: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Owner {
    Pending,
    Snapshot {
        share: u64,
    },
    Live(LiveOwner),
    /// A direct live reference below a Recycle Bin, with the entry ctime.
    RecycleEntry {
        share: u64,
        ctime: u64,
    },
    SharedBetweenFolders,
    Unknown(UnknownReason),
    /// A direct-reference path was not classified while building the index.
    UnmappedDirectPath,
    /// An unresolved direct owner with the safe root facts for diagnosis.
    DirectOwnerUnresolved(DirectOwnerDiagnostic),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentInput {
    pub bytes: u64,
    pub refs_complete: bool,
    pub owners: Option<Vec<Owner>>,
    pub pinned_bytes: Result<u64, PinnedRangesSubcase>,
    pub reclaimable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentPart {
    pub kind: PartKind,
    /// Bytes attributed to this part, after removing known pinned bytes.
    pub bytes: u64,
    pub pinned_bytes: u64,
    /// `Some` only when deleting the paths is proven to free this extent.
    pub reclaimable_bytes: Option<u64>,
    /// The single live bucket for recycle, service or shared-folder parts.
    pub owner: Option<LiveOwner>,
    /// Source shared folders for snapshot-only extents.
    pub snapshot_shares: Vec<u64>,
    /// Why this extent was left in unknown.
    pub unknown_reason: Option<UnknownReason>,
    /// More specific direct-owner diagnostics, when that is the unknown cause.
    pub direct_owner_diagnostic: Option<DirectOwnerDiagnostic>,
    /// Why pinned bytes were not measured, when that is the unknown cause.
    pub pinned_ranges_subcase: Option<PinnedRangesSubcase>,
    /// Newest entry ctime when a Recycle Bin owns this extent.
    pub recycle_ctime: Option<u64>,
}

/// Why an owner leaves its extent unknown, whatever its pinned bytes.
fn unknown_owner(owners: &[Owner]) -> Option<(UnknownReason, Option<DirectOwnerDiagnostic>)> {
    owners.iter().find_map(|owner| match owner {
        Owner::Unknown(reason) => Some((*reason, None)),
        Owner::UnmappedDirectPath => Some((UnknownReason::DirectOwnerUnresolved, None)),
        Owner::DirectOwnerUnresolved(diagnostic) => {
            Some((UnknownReason::DirectOwnerUnresolved, Some(*diagnostic)))
        }
        _ => None,
    })
}

/// Whether every owner is known, so the extent's pinned bytes decide
/// its attribution.
pub fn owners_resolved(owners: &[Owner]) -> bool {
    !owners.is_empty() && unknown_owner(owners).is_none()
}

/// Attributes one physical extent once, with unknown taking precedence
/// whenever its references or pinned ranges are not fully known.
pub fn attribute_extent(input: &ExtentInput) -> ExtentPart {
    let Some(owners) = input.owners.as_deref().filter(|o| !o.is_empty()) else {
        return unknown(input.bytes, UnknownReason::MissingOwner, None);
    };
    if !input.refs_complete {
        return unknown(input.bytes, UnknownReason::IncompleteReferences, None);
    }
    if let Some((reason, direct_owner_diagnostic)) = unknown_owner(owners) {
        return unknown(input.bytes, reason, direct_owner_diagnostic);
    }
    let pinned = match input.pinned_bytes {
        Ok(n) if n <= input.bytes => n,
        Ok(_) => return pinned_unknown(input.bytes, PinnedRangesSubcase::RangeUnmappable),
        Err(subcase) => return pinned_unknown(input.bytes, subcase),
    };

    let all_pending = owners.iter().all(|o| *o == Owner::Pending);
    let snapshot_only = owners
        .iter()
        .all(|o| matches!(o, Owner::Pending | Owner::Snapshot { .. }))
        && owners.iter().any(|o| matches!(o, Owner::Snapshot { .. }));
    let snapshot_shares: BTreeSet<u64> = owners
        .iter()
        .filter_map(|owner| match owner {
            Owner::Snapshot { share } => Some(*share),
            _ => None,
        })
        .collect();
    let live: BTreeSet<LiveOwner> = owners.iter().filter_map(live_owner).collect();
    let shared_live = owners.contains(&Owner::SharedBetweenFolders);

    let kind = if all_pending {
        PartKind::PendingCleanup
    } else if snapshot_only {
        PartKind::SnapshotOnly
    } else if shared_live {
        PartKind::SharedBetweenFolders
    } else {
        match live.iter().copied().collect::<Vec<_>>().as_slice() {
            [LiveOwner::Recycle { .. }] => PartKind::RecycleBin,
            [LiveOwner::Service { .. }] => PartKind::DsmService,
            [LiveOwner::SystemOther] => PartKind::SystemOther,
            [LiveOwner::Share { .. }] => PartKind::ShareLive,
            [] => return unknown(input.bytes, UnknownReason::MissingOwner, None),
            _ => PartKind::SharedBetweenFolders,
        }
    };
    let only_live = owners.iter().all(|owner| live_owner(owner).is_some()) && live.len() == 1;
    let reclaimable_bytes = (input.reclaimable
        && only_live
        && matches!(kind, PartKind::RecycleBin | PartKind::ShareLive))
    .then_some(input.bytes);
    let recycle_ctime = match (kind, live.iter().next()) {
        (PartKind::RecycleBin, Some(LiveOwner::Recycle { share })) => recycle_ctime(owners, *share),
        _ => None,
    };
    ExtentPart {
        kind,
        bytes: input.bytes - pinned,
        pinned_bytes: pinned,
        reclaimable_bytes,
        owner: (live.len() == 1).then(|| *live.iter().next().expect("one owner")),
        snapshot_shares: snapshot_shares.into_iter().collect(),
        unknown_reason: None,
        direct_owner_diagnostic: None,
        pinned_ranges_subcase: None,
        recycle_ctime,
    }
}

fn live_owner(owner: &Owner) -> Option<LiveOwner> {
    match owner {
        Owner::Live(owner) => Some(*owner),
        Owner::RecycleEntry { share, .. } => Some(LiveOwner::Recycle { share: *share }),
        _ => None,
    }
}

fn recycle_ctime(owners: &[Owner], share: u64) -> Option<u64> {
    let mut newest = None;
    let mut found = false;
    for owner in owners {
        match owner {
            Owner::RecycleEntry {
                share: owner_share,
                ctime,
            } if *owner_share == share => {
                newest = Some(newest.map_or(*ctime, |latest: u64| latest.max(*ctime)));
                found = true;
            }
            Owner::Live(LiveOwner::Recycle { share: owner_share }) if *owner_share == share => {
                return None;
            }
            _ => {}
        }
    }
    found.then_some(newest.expect("found a Recycle Bin ctime"))
}

fn unknown(
    bytes: u64,
    reason: UnknownReason,
    direct_owner_diagnostic: Option<DirectOwnerDiagnostic>,
) -> ExtentPart {
    ExtentPart {
        kind: PartKind::Unknown,
        bytes,
        pinned_bytes: 0,
        reclaimable_bytes: None,
        owner: None,
        snapshot_shares: Vec::new(),
        unknown_reason: Some(reason),
        direct_owner_diagnostic,
        pinned_ranges_subcase: None,
        recycle_ctime: None,
    }
}

fn pinned_unknown(bytes: u64, subcase: PinnedRangesSubcase) -> ExtentPart {
    ExtentPart {
        pinned_ranges_subcase: Some(subcase),
        ..unknown(bytes, UnknownReason::PinnedRangesUnknown, None)
    }
}

/// The extent-tree sum represented by an extent's attributed and pinned bytes.
pub fn sum_parts(parts: &[ExtentPart]) -> u64 {
    parts
        .iter()
        .map(|p| p.bytes.saturating_add(p.pinned_bytes))
        .sum()
}

/// Bytes and proven reclaimable bytes attributed to one share or service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerBytes {
    pub bytes: u64,
    pub reclaimable_bytes: u64,
}

/// Per-entry timestamps and extent bytes grouped by their newest entry time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecycleCtimeData {
    #[serde(default)]
    pub bytes: std::collections::BTreeMap<u64, std::collections::BTreeMap<u64, OwnerBytes>>,
    #[serde(default)]
    pub unmeasured_bytes: u64,
}

/// Display-only evidence shared by report headers, volume cards and history.
/// Cleanup and completion values never depend on this summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approximation: Option<crate::report::Approximation>,
    pub unknown_bytes: u64,
    pub used_bytes: Option<u64>,
    /// Namespace or scan coverage is missing, independently of unresolved refs.
    pub coverage_gaps: bool,
    /// Present only when dangling entries in trees whose generation changed
    /// during the scan are the report's whole coverage gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_churn: Option<NamespaceChurn>,
}

/// Entries that vanished while a changing tree was being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceChurn {
    pub dangling_entries: u64,
}

impl Attribution {
    pub fn of(header: &crate::report::Header) -> Option<Self> {
        if let Some(data) = &header.allocated_overview {
            return Some(Self {
                approximation: None,
                unknown_bytes: data
                    .items
                    .iter()
                    .map(|row| row.unknown_bytes)
                    .fold(0, u64::saturating_add),
                used_bytes: header
                    .vfs_space
                    .as_ref()
                    .map(|space| space.total_bytes.saturating_sub(space.free_bytes)),
                coverage_gaps: header.consistency == crate::report::Consistency::Partial
                    || header.coverage.dangling_entries > 0
                    || header.coverage.encrypted_directories > 0,
                namespace_churn: None,
            });
        }
        let data = header.overview.as_ref()?;
        let coverage = &header.coverage;
        let coverage_gaps = header.consistency == crate::report::Consistency::Partial
            || coverage.dangling_entries > 0
            || coverage
                .namespace_uncertain_subvolumes
                .as_ref()
                .is_none_or(|roots| !roots.is_empty())
            || other_coverage_gaps(coverage, data);
        let used_bytes = header
            .space
            .as_ref()
            .and_then(|space| space.end.as_ref())
            .and_then(|space| space.statfs.as_ref().ok())
            .map(|space| space.total_bytes.saturating_sub(space.free_bytes));
        Some(Self {
            approximation: coverage.approximation.clone(),
            unknown_bytes: data.unknown.values().copied().fold(0, u64::saturating_add),
            used_bytes,
            coverage_gaps,
            namespace_churn: namespace_churn(header, data),
        })
    }
}

/// Gaps that generation changes during the scan cannot explain.
fn other_coverage_gaps(coverage: &crate::report::Coverage, data: &OverviewData) -> bool {
    coverage.encrypted_directories > 0
        || coverage.unlinked_subvolume_entries > 0
        || coverage.cycles > 0
        || coverage.unsupported_locations > 0
        || coverage.missing_subvolume_roots > 0
        || coverage.reference_accounting == "unavailable"
        || data.unknown.contains_key(&UnknownReason::CoverageGap)
}

fn namespace_churn(header: &crate::report::Header, data: &OverviewData) -> Option<NamespaceChurn> {
    let coverage = &header.coverage;
    let uncertain = coverage.namespace_uncertain_subvolumes.as_ref()?;
    let changed: BTreeSet<u64> = header
        .change_reasons
        .iter()
        .filter_map(|reason| match reason {
            crate::report::ChangeReason::SubvolumeGenerationChanged { subvolume_id, .. } => {
                Some(*subvolume_id)
            }
            _ => None,
        })
        .collect();
    let explained = !uncertain.is_empty() && uncertain.iter().all(|tree| changed.contains(tree));
    let all_roots_read = coverage.unreadable_roots == Some(0);
    (coverage.dangling_entries > 0
        && explained
        && all_roots_read
        && !other_coverage_gaps(coverage, data))
    .then_some(NamespaceChurn {
        dangling_entries: coverage.dangling_entries,
    })
}

/// Fixed-size overview counters retained with a complete volume report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_analysis: Option<crate::scan::attribution::Measurement>,
    /// Raw share names encoded as lowercase hexadecimal, keyed by id.
    pub share_names_hex: std::collections::BTreeMap<u64, String>,
    pub extent_count: u64,
    pub extent_bytes: u64,
    pub unknown_extents: u64,
    pub pending_cleanup_bytes: u64,
    pub snapshot_only_bytes: std::collections::BTreeMap<u64, u64>,
    /// Snapshot-only extents held by snapshots from multiple shares.
    #[serde(default)]
    pub snapshot_multi_share_bytes: u64,
    pub recycle_bin: std::collections::BTreeMap<u64, OwnerBytes>,
    /// Additive format-4 Recycle Bin age detail; absent in older reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recycle_ctime: Option<RecycleCtimeData>,
    pub dsm_services: std::collections::BTreeMap<u16, u64>,
    /// Indexed volume paths outside the known shares and DSM service table.
    #[serde(default)]
    pub system_other_bytes: u64,
    pub share_live: std::collections::BTreeMap<u64, OwnerBytes>,
    pub shared_between_folders_bytes: u64,
    pub pinned_unreferenced_bytes: u64,
    pub unknown: std::collections::BTreeMap<UnknownReason, u64>,
    /// Additive detail for direct-owner unresolved bytes, keyed by subcase.
    #[serde(default)]
    pub direct_owner_subcases:
        std::collections::BTreeMap<DirectOwnerSubcase, DirectOwnerSubcaseBytes>,
    /// Additive detail for pinned-range unknown bytes, keyed by subcase.
    #[serde(default)]
    pub pinned_ranges_subcases: std::collections::BTreeMap<PinnedRangesSubcase, u64>,
    /// Subvolumes awaiting the Btrfs cleaner when the extents were
    /// attributed, ascending; `None` in reports written before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_roots: Option<Vec<u64>>,
}

impl OverviewData {
    /// Adds one extent's once-only attribution to the retained counters.
    pub fn add(&mut self, part: &ExtentPart, extent_bytes: u64) {
        self.extent_count += 1;
        self.extent_bytes += extent_bytes;
        self.pinned_unreferenced_bytes += part.pinned_bytes;
        match part.kind {
            PartKind::PendingCleanup => self.pending_cleanup_bytes += part.bytes,
            PartKind::SnapshotOnly => match part.snapshot_shares.as_slice() {
                [share] => *self.snapshot_only_bytes.entry(*share).or_default() += part.bytes,
                [] => {
                    *self.unknown.entry(UnknownReason::MissingOwner).or_default() += extent_bytes;
                }
                _ => self.snapshot_multi_share_bytes += part.bytes,
            },
            PartKind::RecycleBin => {
                let Some(LiveOwner::Recycle { share }) = part.owner else {
                    *self.unknown.entry(UnknownReason::MissingOwner).or_default() += extent_bytes;
                    return;
                };
                let row = self.recycle_bin.entry(share).or_default();
                row.bytes += part.bytes;
                row.reclaimable_bytes += part.reclaimable_bytes.unwrap_or(0);
                if let Some(times) = &mut self.recycle_ctime {
                    match part.recycle_ctime {
                        Some(ctime) => {
                            let age = times
                                .bytes
                                .entry(share)
                                .or_default()
                                .entry(ctime)
                                .or_default();
                            age.bytes += part.bytes;
                            age.reclaimable_bytes += part.reclaimable_bytes.unwrap_or(0);
                        }
                        None => times.unmeasured_bytes += part.bytes,
                    }
                }
            }
            PartKind::DsmService => {
                let Some(LiveOwner::Service { service }) = part.owner else {
                    *self.unknown.entry(UnknownReason::MissingOwner).or_default() += extent_bytes;
                    return;
                };
                *self.dsm_services.entry(service).or_default() += part.bytes;
            }
            PartKind::SystemOther => self.system_other_bytes += part.bytes,
            PartKind::ShareLive => {
                let Some(LiveOwner::Share { share }) = part.owner else {
                    *self.unknown.entry(UnknownReason::MissingOwner).or_default() += extent_bytes;
                    return;
                };
                let row = self.share_live.entry(share).or_default();
                row.bytes += part.bytes;
                row.reclaimable_bytes += part.reclaimable_bytes.unwrap_or(0);
            }
            PartKind::SharedBetweenFolders => {
                self.shared_between_folders_bytes += part.bytes;
            }
            PartKind::PinnedUnreferenced => unreachable!("pinned bytes are split from an extent"),
            PartKind::Unknown => {
                self.unknown_extents += 1;
                *self
                    .unknown
                    .entry(part.unknown_reason.unwrap_or(UnknownReason::CoverageGap))
                    .or_default() += extent_bytes;
                if let Some(diagnostic) = part.direct_owner_diagnostic {
                    let subcase = self
                        .direct_owner_subcases
                        .entry(diagnostic.subcase)
                        .or_default();
                    subcase.bytes += extent_bytes;
                    let root =
                        subcase
                            .roots
                            .entry(diagnostic.root_id)
                            .or_insert(DirectOwnerRootBytes {
                                kind: diagnostic.root_kind,
                                class: diagnostic.root_class,
                                at_directory: diagnostic.at_directory,
                                bytes: 0,
                            });
                    debug_assert_eq!(root.kind, diagnostic.root_kind);
                    debug_assert_eq!(root.class, diagnostic.root_class);
                    debug_assert_eq!(root.at_directory, diagnostic.at_directory);
                    root.bytes += extent_bytes;
                }
                if let Some(subcase) = part.pinned_ranges_subcase {
                    *self.pinned_ranges_subcases.entry(subcase).or_default() += extent_bytes;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn churn_header(
        dangling: u64,
        reasons: Vec<crate::report::ChangeReason>,
    ) -> crate::report::Header {
        let mut header = crate::report::testing::header(Vec::new());
        header.consistency = crate::report::Consistency::Partial;
        header.change_reasons = reasons;
        header.coverage.dangling_entries = dangling;
        header.coverage.namespace_uncertain_subvolumes = Some(vec![256]);
        header.coverage.unreadable_roots = Some(0);
        header.coverage.reference_accounting = "partial".into();
        header.overview = Some(OverviewData::default());
        header
    }

    fn changed(subvolume_id: u64) -> crate::report::ChangeReason {
        crate::report::ChangeReason::SubvolumeGenerationChanged {
            subvolume_id,
            selected: true,
            generation_before: 1,
            generation_after: 2,
        }
    }

    fn churn_of(header: &crate::report::Header) -> Option<u64> {
        Attribution::of(header)?
            .namespace_churn
            .map(|churn| churn.dangling_entries)
    }

    #[test]
    fn dangling_entries_in_a_changed_tree_are_namespace_churn_and_still_a_coverage_gap() {
        let header = churn_header(3, vec![changed(256), changed(261)]);
        assert_eq!(churn_of(&header), Some(3));
        assert!(Attribution::of(&header).unwrap().coverage_gaps);
    }

    #[test]
    fn namespace_churn_needs_generation_change_evidence_in_every_uncertain_tree() {
        assert_eq!(churn_of(&churn_header(1, Vec::new())), None);
        assert_eq!(churn_of(&churn_header(1, vec![changed(261)])), None);
        let mut two_trees = churn_header(2, vec![changed(256)]);
        two_trees.coverage.namespace_uncertain_subvolumes = Some(vec![256, 258]);
        assert_eq!(churn_of(&two_trees), None);
        let mut legacy = churn_header(1, vec![changed(256)]);
        legacy.coverage.namespace_uncertain_subvolumes = None;
        assert_eq!(churn_of(&legacy), None);
        assert_eq!(churn_of(&churn_header(0, vec![changed(256)])), None);
    }

    #[test]
    fn any_other_coverage_gap_withdraws_namespace_churn() {
        let gaps: [fn(&mut crate::report::Coverage); 7] = [
            |c| c.encrypted_directories = 1,
            |c| c.unlinked_subvolume_entries = 1,
            |c| c.cycles = 1,
            |c| c.unsupported_locations = 1,
            |c| c.missing_subvolume_roots = 1,
            |c| c.unreadable_roots = Some(1),
            |c| c.unreadable_roots = None,
        ];
        for gap in gaps {
            let mut header = churn_header(1, vec![changed(256)]);
            gap(&mut header.coverage);
            assert_eq!(churn_of(&header), None);
        }
        let mut header = churn_header(1, vec![changed(256)]);
        header.coverage.reference_accounting = "unavailable".into();
        assert_eq!(churn_of(&header), None);
        let mut header = churn_header(1, vec![changed(256)]);
        header
            .overview
            .as_mut()
            .unwrap()
            .unknown
            .insert(UnknownReason::CoverageGap, 1);
        assert_eq!(churn_of(&header), None);
    }

    fn extent(owners: Option<Vec<Owner>>, pinned: Option<u64>) -> ExtentInput {
        ExtentInput {
            bytes: 100,
            refs_complete: true,
            owners,
            pinned_bytes: pinned.ok_or(PinnedRangesSubcase::NoScannedUse),
            reclaimable: false,
        }
    }

    #[test]
    fn applies_pending_snapshot_and_live_precedence_once_per_extent() {
        let cases = [
            (vec![Owner::Pending], PartKind::PendingCleanup),
            (
                vec![Owner::Pending, Owner::Snapshot { share: 7 }],
                PartKind::SnapshotOnly,
            ),
            (
                vec![Owner::Live(LiveOwner::Recycle { share: 7 })],
                PartKind::RecycleBin,
            ),
            (
                vec![Owner::Live(LiveOwner::Service { service: 2 })],
                PartKind::DsmService,
            ),
            (
                vec![Owner::Live(LiveOwner::Share { share: 7 })],
                PartKind::ShareLive,
            ),
            (
                vec![
                    Owner::Live(LiveOwner::Share { share: 7 }),
                    Owner::Live(LiveOwner::Service { service: 2 }),
                ],
                PartKind::SharedBetweenFolders,
            ),
            (
                vec![Owner::SharedBetweenFolders],
                PartKind::SharedBetweenFolders,
            ),
        ];

        for (owners, expected) in cases {
            let part = attribute_extent(&extent(Some(owners), Some(0)));
            assert_eq!(part.kind, expected);
            assert_eq!(part.bytes, 100);
            assert_eq!(part.bytes + part.pinned_bytes, 100);
        }
    }

    #[test]
    fn pinned_unreferenced_bytes_are_removed_from_the_owner_part() {
        let part = attribute_extent(&extent(
            Some(vec![Owner::Live(LiveOwner::Share { share: 7 })]),
            Some(24),
        ));

        assert_eq!(part.kind, PartKind::ShareLive);
        assert_eq!(part.bytes, 76);
        assert_eq!(part.pinned_bytes, 24);
    }

    #[test]
    fn only_path_parts_can_report_proven_reclaimable_bytes() {
        let mut input = extent(
            Some(vec![Owner::Live(LiveOwner::Recycle { share: 7 })]),
            Some(0),
        );
        input.reclaimable = true;
        assert_eq!(attribute_extent(&input).reclaimable_bytes, Some(100));

        input.owners = Some(vec![Owner::Live(LiveOwner::Service { service: 2 })]);
        assert_eq!(attribute_extent(&input).reclaimable_bytes, None);
    }

    #[test]
    fn live_data_kept_by_a_snapshot_is_not_reclaimable() {
        let mut input = extent(
            Some(vec![
                Owner::Snapshot { share: 7 },
                Owner::Live(LiveOwner::Share { share: 7 }),
            ]),
            Some(0),
        );
        input.reclaimable = true;

        let part = attribute_extent(&input);

        assert_eq!(part.kind, PartKind::ShareLive);
        assert_eq!(part.reclaimable_bytes, None);
        assert_eq!(part.snapshot_shares, vec![7]);
    }

    #[test]
    fn unresolved_owners_or_pinned_ranges_are_unknown_without_losing_the_extent() {
        for input in [
            extent(None, Some(0)),
            extent(
                Some(vec![Owner::Unknown(
                    UnknownReason::SharedTreeOwnerUnresolved,
                )]),
                Some(0),
            ),
            extent(Some(vec![Owner::Snapshot { share: 7 }]), None),
        ] {
            let part = attribute_extent(&input);
            assert_eq!(part.kind, PartKind::Unknown);
            assert_eq!(part.bytes, 100);
            assert_eq!(part.pinned_bytes, 0);
        }
    }

    #[test]
    fn ref_count_mismatches_and_owner_budget_exhaustion_are_unknown() {
        let mut mismatch = extent(
            Some(vec![Owner::Live(LiveOwner::Share { share: 7 })]),
            Some(0),
        );
        mismatch.refs_complete = false;
        assert_eq!(attribute_extent(&mismatch).kind, PartKind::Unknown);

        let exhausted = extent(None, Some(0));
        assert_eq!(attribute_extent(&exhausted).kind, PartKind::Unknown);
    }

    #[test]
    fn all_parts_add_to_the_extent_tree_sum_without_double_counting() {
        let extents = [
            extent(Some(vec![Owner::Pending]), Some(0)),
            extent(Some(vec![Owner::Snapshot { share: 7 }]), Some(0)),
            extent(
                Some(vec![Owner::Live(LiveOwner::Recycle { share: 7 })]),
                Some(8),
            ),
            extent(
                Some(vec![Owner::Live(LiveOwner::Service { service: 2 })]),
                Some(0),
            ),
            extent(
                Some(vec![Owner::Live(LiveOwner::Share { share: 9 })]),
                Some(0),
            ),
            extent(
                Some(vec![
                    Owner::Live(LiveOwner::Share { share: 7 }),
                    Owner::Live(LiveOwner::Share { share: 9 }),
                ]),
                Some(0),
            ),
            extent(None, Some(0)),
        ];
        let parts: Vec<_> = extents.iter().map(attribute_extent).collect();

        assert_eq!(sum_parts(&parts), 7 * 100);
    }

    #[test]
    fn overview_counters_keep_extent_bytes_and_pinned_bytes_once() {
        let mut recycle = extent(
            Some(vec![Owner::Live(LiveOwner::Recycle { share: 3 })]),
            Some(8),
        );
        recycle.reclaimable = true;
        let cases = [
            extent(Some(vec![Owner::Pending]), Some(0)),
            extent(Some(vec![Owner::Snapshot { share: 3 }]), Some(0)),
            recycle,
            extent(
                Some(vec![Owner::Live(LiveOwner::Service { service: 2 })]),
                Some(0),
            ),
            extent(
                Some(vec![Owner::Live(LiveOwner::Share { share: 3 })]),
                Some(0),
            ),
            extent(
                Some(vec![
                    Owner::Live(LiveOwner::Share { share: 3 }),
                    Owner::Live(LiveOwner::Share { share: 4 }),
                ]),
                Some(0),
            ),
            extent(
                Some(vec![Owner::Unknown(UnknownReason::OwnerBudgetExhausted)]),
                Some(0),
            ),
        ];
        let mut data = OverviewData::default();
        for input in cases {
            data.add(&attribute_extent(&input), input.bytes);
        }

        assert_eq!(data.extent_count, 7);
        assert_eq!(data.extent_bytes, 7 * 100);
        assert_eq!(data.pending_cleanup_bytes, 100);
        assert_eq!(data.snapshot_only_bytes[&3], 100);
        assert_eq!(
            data.recycle_bin[&3],
            OwnerBytes {
                bytes: 92,
                reclaimable_bytes: 100
            }
        );
        assert_eq!(data.dsm_services[&2], 100);
        assert_eq!(
            data.share_live[&3],
            OwnerBytes {
                bytes: 100,
                reclaimable_bytes: 0
            }
        );
        assert_eq!(data.shared_between_folders_bytes, 100);
        assert_eq!(data.pinned_unreferenced_bytes, 8);
        assert_eq!(data.unknown[&UnknownReason::OwnerBudgetExhausted], 100);
    }

    #[test]
    fn direct_owner_subcase_counters_preserve_the_aggregate_and_root_facts() {
        let diagnostic = DirectOwnerDiagnostic {
            subcase: DirectOwnerSubcase::RootNotDiscovered,
            root_id: 900,
            root_kind: DirectOwnerRootKind::NotDiscovered,
            root_class: DirectOwnerRootClass::NotDiscovered,
            at_directory: false,
        };
        let part = attribute_extent(&extent(
            Some(vec![Owner::DirectOwnerUnresolved(diagnostic)]),
            Some(0),
        ));
        let mut data = OverviewData::default();
        data.add(&part, 100);

        assert_eq!(data.unknown[&UnknownReason::DirectOwnerUnresolved], 100);
        let subcase = &data.direct_owner_subcases[&DirectOwnerSubcase::RootNotDiscovered];
        assert_eq!(subcase.bytes, 100);
        assert_eq!(subcase.roots[&900].bytes, 100);
        assert_eq!(subcase.roots[&900].kind, DirectOwnerRootKind::NotDiscovered);
        assert_eq!(
            subcase.roots[&900].class,
            DirectOwnerRootClass::NotDiscovered
        );
        assert!(!subcase.roots[&900].at_directory);
    }

    #[test]
    fn pinned_range_subcase_counters_preserve_the_aggregate() {
        let mut input = extent(
            Some(vec![Owner::Live(LiveOwner::Service { service: 2 })]),
            None,
        );
        input.pinned_bytes = Err(PinnedRangesSubcase::SharedReference);
        let part = attribute_extent(&input);
        let mut data = OverviewData::default();
        data.add(&part, 100);

        assert_eq!(
            part.pinned_ranges_subcase,
            Some(PinnedRangesSubcase::SharedReference)
        );
        assert_eq!(data.unknown[&UnknownReason::PinnedRangesUnknown], 100);
        assert_eq!(
            data.pinned_ranges_subcases[&PinnedRangesSubcase::SharedReference],
            100
        );
    }

    #[test]
    fn pinned_bytes_beyond_the_extent_are_unmappable() {
        let part = attribute_extent(&extent(
            Some(vec![Owner::Live(LiveOwner::Share { share: 7 })]),
            Some(101),
        ));

        assert_eq!(
            part.unknown_reason,
            Some(UnknownReason::PinnedRangesUnknown)
        );
        assert_eq!(
            part.pinned_ranges_subcase,
            Some(PinnedRangesSubcase::RangeUnmappable)
        );
    }

    #[test]
    fn older_overview_data_deserializes_without_optional_direct_owner_details() {
        let mut old = serde_json::to_value(OverviewData::default()).unwrap();
        old.as_object_mut().unwrap().remove("direct_owner_subcases");
        old.as_object_mut()
            .unwrap()
            .remove("pinned_ranges_subcases");
        let decoded: OverviewData = serde_json::from_value(old).unwrap();
        assert!(decoded.direct_owner_subcases.is_empty());
        assert!(decoded.pinned_ranges_subcases.is_empty());
    }

    #[test]
    fn snapshot_extents_from_multiple_shares_are_counted_once() {
        let input = extent(
            Some(vec![
                Owner::Snapshot { share: 3 },
                Owner::Snapshot { share: 4 },
            ]),
            Some(0),
        );
        let mut data = OverviewData::default();

        data.add(&attribute_extent(&input), input.bytes);

        assert!(data.snapshot_only_bytes.is_empty());
        assert_eq!(data.snapshot_multi_share_bytes, 100);
    }
}
