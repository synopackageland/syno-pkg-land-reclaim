//! Compact, on-disk-unit-derived ownership index for volume overview.

use super::owner_table::OwnerTables;
use crate::btrfs::subvolume::SubvolumeSet;
use crate::model::namespace::{FileKind, RecycleEntryCtime};
use crate::model::tree::{NodeFlags, ScopeTree};
use crate::overview::{
    DirectOwnerDiagnostic, DirectOwnerRootClass, DirectOwnerRootKind, DirectOwnerSubcase,
    LiveOwner, Owner, UnknownReason,
};
use crate::scan::UnitInfo;
use crate::{dsm, scan};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::ops::ControlFlow;

const OWNER_MASK: u64 = 0x0f00_0000_0000_0000;
const OWNER_ID_MASK: u64 = !OWNER_MASK;
const OWNER_PENDING: u64 = 0x0100_0000_0000_0000;
const OWNER_SNAPSHOT: u64 = 0x0200_0000_0000_0000;
pub(super) const OWNER_RECYCLE: u64 = 0x0300_0000_0000_0000;
const OWNER_SERVICE: u64 = 0x0400_0000_0000_0000;
pub(super) const OWNER_SHARE: u64 = 0x0500_0000_0000_0000;
const OWNER_UNKNOWN: u64 = 0x0600_0000_0000_0000;
pub(super) const OWNER_CONFLICT: u64 = 0x0700_0000_0000_0000;
pub(super) const OWNER_SHARED: u64 = 0x0800_0000_0000_0000;
const OWNER_UNMAPPED_DIRECT_PATH: u64 = 0x0900_0000_0000_0000;
const OWNER_SYSTEM_OTHER: u64 = 0x0a00_0000_0000_0000;

#[derive(Debug, Clone, Copy)]
pub(super) struct OwnerRecord {
    pub(super) inode: u64,
    pub(super) owner: u64,
}

/// Sorted per-tree owner records, plus source share names for JSON.
#[derive(Debug, Default)]
pub(super) struct OwnerIndex {
    by_tree: OwnerTables,
    root_owners: BTreeMap<u64, u64>,
    share_names: BTreeMap<u64, Vec<u8>>,
    share_ids: BTreeMap<Vec<u8>, u64>,
    shares_by_uuid: BTreeMap<[u8; 16], u64>,
    recycle_ctimes: Vec<RecycleEntryCtime>,
    volume_tree: u64,
    pub(super) state: super::own_writes::StateOwnership,
}

impl OwnerIndex {
    pub(super) fn build(
        units: &[UnitInfo],
        before: &SubvolumeSet,
        mount: &[u8],
        volume_tree: u64,
        spill: &std::path::Path,
        boundary: &mut dyn crate::btrfs::walk::BatchBoundary,
    ) -> Result<Self, crate::btrfs::walk::WalkError> {
        let mut index = OwnerIndex {
            volume_tree,
            ..OwnerIndex::default()
        };
        index.collect_shares(before, volume_tree);

        let mut marker_roots = Vec::new();
        let mut child_roots = BTreeMap::new();
        for (unit_id, unit) in units.iter().enumerate() {
            let estimated = tree_memory_estimate(unit)?;
            reserve(boundary, estimated)?;
            let tree = load_tree(&unit.files)?;
            append_effective_recycle_ctimes(
                &tree,
                &unit.summary.recycle_ctime_entries,
                &mut index.recycle_ctimes,
            )?;
            let roots = tree.roots();
            let Some(&root_node) = roots.first() else {
                continue;
            };
            let local_root = tree.path(root_node);
            let root_inode = tree.node(root_node).inode;
            let tree_id = tree.inode(root_inode).map(|inode| inode.tree);
            let absolute_root = match unit.placement.parent {
                None => local_root.clone(),
                Some(_) => tree_id
                    .and_then(|id| child_roots.remove(&id))
                    .unwrap_or_else(|| local_root.clone()),
            };
            if let Some(inode) = tree.inode(root_inode) {
                index.root_owners.insert(
                    inode.tree,
                    encode(classify_path(
                        &absolute_root,
                        tree.node(root_node).flags,
                        mount,
                        &index.share_ids,
                        &marker_roots,
                    )),
                );
            }

            for node_id in 0..tree.len() as u32 {
                let Some(service) = dsm::marked_by(tree.name(node_id)) else {
                    continue;
                };
                let local_path = tree.path(node_id);
                let suffix = local_path
                    .strip_prefix(local_root.as_slice())
                    .unwrap_or_default();
                let mut full_path = absolute_root.clone();
                full_path.extend_from_slice(suffix);
                let Some(cut) = full_path.iter().rposition(|byte| *byte == b'/') else {
                    continue;
                };
                let parent = &full_path[..cut];
                if parent != mount {
                    marker_roots.push((parent.to_vec(), dsm::service_id(service)));
                }
            }

            for child in &unit.summary.attached {
                let local_path = tree.path(child.node);
                let suffix = local_path
                    .strip_prefix(local_root.as_slice())
                    .unwrap_or_default();
                let mut path = absolute_root.clone();
                path.extend_from_slice(suffix);
                child_roots.insert(child.subvolume, path);
            }

            let mut records = Vec::new();
            for node_id in 0..tree.len() as u32 {
                let node = tree.node(node_id);
                let Some(inode) = tree.inode(node.inode) else {
                    continue;
                };
                if inode.kind != FileKind::Regular || inode.inline_bytes >= inode.size {
                    continue;
                }
                let owner = if node.flags.contains(NodeFlags::ENCRYPTED) {
                    Owner::Unknown(UnknownReason::CoverageGap)
                } else {
                    let local_path = tree.path(node_id);
                    let suffix = local_path
                        .strip_prefix(local_root.as_slice())
                        .unwrap_or_default();
                    let mut full_path = absolute_root.clone();
                    full_path.extend_from_slice(suffix);
                    classify_path(
                        &full_path,
                        node.flags,
                        mount,
                        &index.share_ids,
                        &marker_roots,
                    )
                };
                grow_records(&mut records, boundary)?;
                records.push(OwnerRecord {
                    inode: inode.objectid,
                    owner: encode(owner),
                });
            }
            records.sort_unstable_by_key(|record| record.inode);
            let records = collapse(records);
            if let Some(tree_id) = tree_id {
                index.by_tree.insert(
                    tree_id,
                    &records,
                    &spill.join(format!("owners-{unit_id}")),
                )?;
            }
        }
        index.recycle_ctimes.sort_unstable_by(|a, b| {
            (a.tree, a.inode)
                .cmp(&(b.tree, b.inode))
                .then_with(|| b.ctime.cmp(&a.ctime))
        });
        index
            .recycle_ctimes
            .dedup_by(|a, b| a.tree == b.tree && a.inode == b.inode);
        Ok(index)
    }

    fn collect_shares(&mut self, before: &SubvolumeSet, volume_tree: u64) {
        for subvolume in &before.subvolumes {
            if !subvolume.is_share(volume_tree) {
                continue;
            }
            self.share_names
                .insert(subvolume.id, subvolume.name.clone());
            self.share_ids.insert(subvolume.name.clone(), subvolume.id);
            if let Some(uuid) = subvolume.uuid {
                self.shares_by_uuid.insert(uuid, subvolume.id);
            }
        }
    }

    pub(super) fn share_names(&self) -> BTreeMap<u64, Vec<u8>> {
        self.share_names.clone()
    }

    pub(super) fn resolve_owner<S: crate::btrfs::source::MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        root: u64,
        inode: u64,
        before: &SubvolumeSet,
        boundary: &mut dyn crate::btrfs::walk::BatchBoundary,
    ) -> Result<Owner, crate::btrfs::walk::WalkError> {
        let owner = self.owner(root, inode, before)?;
        let writable = before
            .get(root)
            .is_some_and(|s| !s.readonly && !s.pending_deletion);
        if writable
            && !before.relocating.contains(&root)
            && root != crate::btrfs::keys::TREE_RELOC_OBJECTID
            && self.lookup(root, inode)?.is_none()
            && self.state.owns(source, root, inode, boundary)?
        {
            return Ok(Owner::Live(LiveOwner::Service {
                service: dsm::service_id(dsm::syno_reclaim()),
            }));
        }
        Ok(owner)
    }

    pub(super) fn owner(&self, root: u64, inode: u64, before: &SubvolumeSet) -> io::Result<Owner> {
        Ok(self.owner_with_record(root, inode, before, self.lookup(root, inode)?))
    }

    fn owner_with_record(
        &self,
        root: u64,
        inode: u64,
        before: &SubvolumeSet,
        record: Option<u64>,
    ) -> Owner {
        if root == crate::btrfs::keys::TREE_RELOC_OBJECTID || before.relocating.contains(&root) {
            return Owner::Unknown(UnknownReason::RelocationTree);
        }
        let Some(subvolume) = before.get(root) else {
            return self.direct_owner_failure(root, DirectOwnerSubcase::RootNotDiscovered, before);
        };
        if subvolume.pending_deletion {
            return Owner::Pending;
        }
        if subvolume.readonly {
            return match self.snapshot_owner(root, before) {
                Some(owner) => owner,
                None => self.direct_owner_failure(
                    root,
                    DirectOwnerSubcase::ReadonlyRootParentUuidUnmapped,
                    before,
                ),
            };
        }
        if let Some(owner) = record
            && owner
                == encode(Owner::Live(LiveOwner::Service {
                    service: dsm::service_id(dsm::syno_reclaim()),
                }))
        {
            return decode(owner);
        }
        if let Some(owner) = self.service_root_owner(root, before) {
            return owner;
        }
        let Some(owner) = record else {
            return self.direct_owner_failure(
                root,
                DirectOwnerSubcase::WritableRootInodeUnindexed,
                before,
            );
        };
        let owner = self.decode_direct_owner(root, owner, before);
        self.with_recycle_ctime(owner, root, inode)
    }

    pub(super) fn root_owner(&self, root: u64, before: &SubvolumeSet) -> Owner {
        if root == crate::btrfs::keys::TREE_RELOC_OBJECTID || before.relocating.contains(&root) {
            return Owner::Unknown(UnknownReason::RelocationTree);
        }
        let Some(subvolume) = before.get(root) else {
            return self.direct_owner_failure(root, DirectOwnerSubcase::RootNotDiscovered, before);
        };
        if subvolume.pending_deletion {
            return Owner::Pending;
        }
        if subvolume.readonly {
            return match self.snapshot_owner(root, before) {
                Some(owner) => owner,
                None => self.direct_owner_failure(
                    root,
                    DirectOwnerSubcase::ReadonlyRootParentUuidUnmapped,
                    before,
                ),
            };
        }
        if let Some(owner) = self.service_root_owner(root, before) {
            return owner;
        }
        let Some(owner) = self.root_owners.get(&root).copied() else {
            return self.direct_owner_failure(
                root,
                DirectOwnerSubcase::WritableRootInodeUnindexed,
                before,
            );
        };
        let owner = self.decode_direct_owner(root, owner, before);
        self.with_recycle_ctime(owner, root, crate::btrfs::keys::FIRST_FREE_OBJECTID)
    }

    fn with_recycle_ctime(&self, owner: Owner, root: u64, inode: u64) -> Owner {
        let Owner::Live(LiveOwner::Recycle { share }) = owner else {
            return owner;
        };
        let ctime = self
            .recycle_ctimes
            .binary_search_by_key(&(root, inode), |entry| (entry.tree, entry.inode))
            .ok()
            .map(|i| self.recycle_ctimes[i].ctime);
        ctime.map_or(owner, |ctime| Owner::RecycleEntry { share, ctime })
    }

    fn lookup(&self, root: u64, inode: u64) -> io::Result<Option<u64>> {
        self.by_tree.lookup(root, inode)
    }

    fn snapshot_owner(&self, root: u64, before: &SubvolumeSet) -> Option<Owner> {
        self.lineage_owner(root, before)
            .or_else(|| self.share_snapshot_folder_owner(root, before))
            .or_else(|| self.containing_service_owner(root, before))
    }

    /// Follows `parent_uuid` from `root` to a share or service root.
    fn lineage_owner(&self, root: u64, before: &SubvolumeSet) -> Option<Owner> {
        let mut at = before.get(root)?;
        for _ in 0..=before.subvolumes.len() {
            if self.share_names.contains_key(&at.id) {
                return Some(Owner::Snapshot { share: at.id });
            }
            if let Some(owner) = self.service_root_owner(at.id, before) {
                return Some(owner);
            }
            let uuid = at.parent_uuid?;
            if let Some(&share) = self.shares_by_uuid.get(&uuid) {
                return Some(Owner::Snapshot { share });
            }
            at = before.subvolumes.iter().find(|s| s.uuid == Some(uuid))?;
        }
        None
    }

    /// DSM keeps a share's snapshots in `@sharesnap/<share>/`, also when
    /// the subvolume a snapshot was taken from is gone (a restored share,
    /// or a promoted Snapshot Replication copy).
    fn share_snapshot_folder_owner(&self, root: u64, before: &SubvolumeSet) -> Option<Owner> {
        let folder = before.sharesnap_folder(root)?;
        let share = *self.share_ids.get(&folder.name)?;
        Some(Owner::Snapshot { share })
    }

    /// A root stored inside a service root, such as a LUN snapshot below
    /// `@iSCSI`, belongs to that service.
    fn containing_service_owner(&self, root: u64, before: &SubvolumeSet) -> Option<Owner> {
        let mut at = before.get(root)?;
        for _ in 0..before.subvolumes.len() {
            at = before.get(at.parent?.parent_id)?;
            if let Some(owner) = self.service_root_owner(at.id, before) {
                return Some(owner);
            }
        }
        None
    }

    fn service_root_owner(&self, root: u64, before: &SubvolumeSet) -> Option<Owner> {
        let subvolume = before.get(root)?;
        if !subvolume.readonly
            && subvolume.in_root_dir_of(self.volume_tree)
            && let Some(service) = dsm::service(&subvolume.name, dsm::Depth::VolumeRoot)
        {
            return Some(Owner::Live(LiveOwner::Service {
                service: dsm::service_id(service),
            }));
        }
        match self.root_owners.get(&root).copied().map(decode) {
            Some(Owner::Live(LiveOwner::Service { service }))
                if service != dsm::OTHER_SERVICE_ID =>
            {
                Some(Owner::Live(LiveOwner::Service { service }))
            }
            _ => None,
        }
    }

    fn decode_direct_owner(&self, root: u64, code: u64, before: &SubvolumeSet) -> Owner {
        if code & OWNER_MASK == OWNER_CONFLICT {
            return self.direct_owner_failure(
                root,
                DirectOwnerSubcase::IndexedOwnerConflict,
                before,
            );
        }
        match decode(code) {
            Owner::UnmappedDirectPath | Owner::Unknown(UnknownReason::DirectOwnerUnresolved) => {
                self.direct_owner_failure(root, DirectOwnerSubcase::IndexedPathUnmapped, before)
            }
            owner => owner,
        }
    }

    fn direct_owner_failure(
        &self,
        root: u64,
        subcase: DirectOwnerSubcase,
        before: &SubvolumeSet,
    ) -> Owner {
        let Some(subvolume) = before.get(root) else {
            return Owner::DirectOwnerUnresolved(DirectOwnerDiagnostic {
                subcase,
                root_id: root,
                root_kind: DirectOwnerRootKind::NotDiscovered,
                root_class: DirectOwnerRootClass::NotDiscovered,
                at_directory: false,
            });
        };
        let at_directory = subvolume.name.starts_with(b"@");
        let root_kind = if subvolume.readonly {
            DirectOwnerRootKind::ReadOnly
        } else {
            DirectOwnerRootKind::Writable
        };
        let root_class = if subvolume.readonly {
            DirectOwnerRootClass::Snapshot
        } else {
            let service = subvolume
                .in_root_dir_of(self.volume_tree)
                .then(|| dsm::service(&subvolume.name, dsm::Depth::VolumeRoot))
                .flatten();
            match service.map(|service| service.code) {
                Some(code) if code.starts_with("active_backup") => {
                    DirectOwnerRootClass::ActiveBackup
                }
                Some("iscsi_luns") => DirectOwnerRootClass::Iscsi,
                Some(_) => DirectOwnerRootClass::OtherService,
                None if self.share_names.contains_key(&root) => DirectOwnerRootClass::SharedFolder,
                None if at_directory => DirectOwnerRootClass::AtDirectory,
                None if subvolume.name.starts_with(b"#") => DirectOwnerRootClass::HashDirectory,
                None if dsm::service(&subvolume.name, dsm::Depth::Below).is_some() => {
                    DirectOwnerRootClass::OtherService
                }
                None => DirectOwnerRootClass::Other,
            }
        };
        Owner::DirectOwnerUnresolved(DirectOwnerDiagnostic {
            subcase,
            root_id: root,
            root_kind,
            root_class,
            at_directory,
        })
    }
}

pub(super) fn classify_path(
    path: &[u8],
    flags: NodeFlags,
    mount: &[u8],
    share_ids: &BTreeMap<Vec<u8>, u64>,
    marker_roots: &[(Vec<u8>, u16)],
) -> Owner {
    if flags.contains(NodeFlags::ENCRYPTED) {
        return Owner::Unknown(UnknownReason::CoverageGap);
    }
    if flags.contains(NodeFlags::OWN_STATE) {
        return Owner::Live(LiveOwner::Service {
            service: dsm::service_id(dsm::syno_reclaim()),
        });
    }
    let share = path
        .strip_prefix(mount)
        .and_then(|rest| rest.strip_prefix(b"/"))
        .and_then(|rest| rest.split(|byte| *byte == b'/').next())
        .and_then(|first| share_ids.get(first).copied());
    if flags.contains(NodeFlags::RECYCLE) {
        return share.map_or(Owner::UnmappedDirectPath, |share| {
            Owner::Live(LiveOwner::Recycle { share })
        });
    }
    if let Some(service) = dsm::service_of(path, mount) {
        return Owner::Live(LiveOwner::Service {
            service: dsm::service_id(service),
        });
    }
    if let Some((_, service)) = marker_roots.iter().find(|(root, _)| {
        path == root
            || path
                .strip_prefix(root.as_slice())
                .is_some_and(|rest| rest.starts_with(b"/"))
    }) {
        return Owner::Live(LiveOwner::Service { service: *service });
    }
    if flags.contains(NodeFlags::SYSTEM) {
        return Owner::Live(LiveOwner::Service {
            service: dsm::OTHER_SERVICE_ID,
        });
    }
    if let Some(share) = share {
        return Owner::Live(LiveOwner::Share { share });
    }
    let Some(rest) = path
        .strip_prefix(mount)
        .and_then(|rest| rest.strip_prefix(b"/"))
    else {
        // The mixed volume root itself cannot identify a shared leaf's paths.
        return Owner::UnmappedDirectPath;
    };
    if rest.is_empty() {
        return Owner::UnmappedDirectPath;
    }
    if rest.starts_with(b"@") {
        Owner::Live(LiveOwner::Service {
            service: dsm::OTHER_SERVICE_ID,
        })
    } else {
        Owner::Live(LiveOwner::SystemOther)
    }
}

fn encode(owner: Owner) -> u64 {
    match owner {
        Owner::Pending => OWNER_PENDING,
        Owner::Snapshot { share } => OWNER_SNAPSHOT | share,
        Owner::SharedBetweenFolders => OWNER_SHARED,
        Owner::Live(LiveOwner::SystemOther) => OWNER_SYSTEM_OTHER,
        Owner::Live(LiveOwner::Recycle { share }) => OWNER_RECYCLE | share,
        Owner::RecycleEntry { share, .. } => OWNER_RECYCLE | share,
        Owner::Live(LiveOwner::Service { service }) => OWNER_SERVICE | u64::from(service),
        Owner::Live(LiveOwner::Share { share }) => OWNER_SHARE | share,
        Owner::Unknown(reason) => OWNER_UNKNOWN | reason as u64,
        Owner::UnmappedDirectPath => OWNER_UNMAPPED_DIRECT_PATH,
        Owner::DirectOwnerUnresolved(_) => {
            OWNER_UNKNOWN | UnknownReason::DirectOwnerUnresolved as u64
        }
    }
}

fn decode(code: u64) -> Owner {
    let id = code & OWNER_ID_MASK;
    match code & OWNER_MASK {
        OWNER_PENDING => Owner::Pending,
        OWNER_SNAPSHOT => Owner::Snapshot { share: id },
        OWNER_SHARED => Owner::SharedBetweenFolders,
        OWNER_SYSTEM_OTHER => Owner::Live(LiveOwner::SystemOther),
        OWNER_RECYCLE => Owner::Live(LiveOwner::Recycle { share: id }),
        OWNER_SERVICE => Owner::Live(LiveOwner::Service { service: id as u16 }),
        OWNER_SHARE => Owner::Live(LiveOwner::Share { share: id }),
        OWNER_CONFLICT => Owner::Unknown(UnknownReason::DirectOwnerUnresolved),
        OWNER_UNKNOWN => Owner::Unknown(unknown_reason(id)),
        OWNER_UNMAPPED_DIRECT_PATH => Owner::UnmappedDirectPath,
        _ => Owner::Unknown(UnknownReason::DirectOwnerUnresolved),
    }
}

fn unknown_reason(id: u64) -> UnknownReason {
    match id {
        0 => UnknownReason::MissingOwner,
        1 => UnknownReason::IncompleteReferences,
        2 => UnknownReason::DirectOwnerUnresolved,
        3 => UnknownReason::SharedTreeOwnerUnresolved,
        4 => UnknownReason::RelocationTree,
        5 => UnknownReason::CoverageGap,
        6 => UnknownReason::OwnerBudgetExhausted,
        _ => UnknownReason::PinnedRangesUnknown,
    }
}

fn collapse(records: Vec<OwnerRecord>) -> Vec<OwnerRecord> {
    let mut records = records;
    let mut used = 0usize;
    for read in 0..records.len() {
        let record = records[read];
        if used > 0 && records[used - 1].inode == record.inode {
            let prior = &mut records[used - 1];
            prior.owner = combine_owner(prior.owner, record.owner);
        } else {
            records[used] = record;
            used += 1;
        }
    }
    records.truncate(used);
    records
}

pub(super) fn combine_owner(prior: u64, next: u64) -> u64 {
    if prior == next {
        prior
    } else if live_owner(prior) && live_owner(next) {
        OWNER_SHARED
    } else {
        OWNER_CONFLICT
    }
}

fn live_owner(code: u64) -> bool {
    matches!(
        code & OWNER_MASK,
        OWNER_RECYCLE | OWNER_SERVICE | OWNER_SHARE | OWNER_SHARED | OWNER_SYSTEM_OTHER
    )
}

fn load_tree(path: &std::path::Path) -> io::Result<ScopeTree> {
    ScopeTree::load(&fs::read(path.join(scan::TREE))?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn append_effective_recycle_ctimes(
    tree: &ScopeTree,
    entries: &[RecycleEntryCtime],
    effective: &mut Vec<RecycleEntryCtime>,
) -> Result<(), crate::btrfs::walk::WalkError> {
    let count = (0..tree.len() as u32)
        .filter(|&node_id| {
            let node = tree.node(node_id);
            node.inode != crate::model::tree::NO_INODE
                && node.flags.contains(NodeFlags::RECYCLE)
                && !recycle_bin_container(tree, node_id)
        })
        .count();
    effective
        .try_reserve(count)
        .map_err(|_| crate::btrfs::walk::WalkError::Capacity("overview recycle ctime index"))?;

    // The newest recycled ctime at or above each node already visited, so
    // a deep tree costs one climb per node rather than one per ancestor.
    let mut newest: HashMap<u32, u64> = HashMap::new();
    newest
        .try_reserve(count)
        .map_err(|_| crate::btrfs::walk::WalkError::Capacity("overview recycle ctime index"))?;
    for node_id in 0..tree.len() as u32 {
        let node = tree.node(node_id);
        let Some(ctime) = recycled_ctime(tree, entries, node_id) else {
            continue;
        };
        let Some(inode) = tree.inode(node.inode) else {
            continue;
        };
        let above = newest_at_or_above(tree, entries, node.parent, &mut newest);
        effective.push(RecycleEntryCtime {
            tree: inode.tree,
            inode: inode.objectid,
            ctime: ctime.max(above),
        });
    }
    Ok(())
}

/// The ctime of `node` when it is an entry of a Recycle Bin (not the bin).
fn recycled_ctime(tree: &ScopeTree, entries: &[RecycleEntryCtime], node: u32) -> Option<u64> {
    let found = tree.node(node);
    if found.inode == crate::model::tree::NO_INODE
        || !found.flags.contains(NodeFlags::RECYCLE)
        || recycle_bin_container(tree, node)
    {
        return None;
    }
    let inode = tree.inode(found.inode)?;
    ctime_for(entries, inode.tree, inode.objectid)
}

/// The newest recycled ctime of `node` and its ancestors (0 for none),
/// remembered in `newest` for every node it climbs through.
fn newest_at_or_above(
    tree: &ScopeTree,
    entries: &[RecycleEntryCtime],
    node: u32,
    newest: &mut HashMap<u32, u64>,
) -> u64 {
    let mut climbed = Vec::new();
    let mut at = node;
    let mut ctime = 0;
    while at != crate::model::tree::VIRTUAL_ROOT {
        if let Some(&known) = newest.get(&at) {
            ctime = known;
            break;
        }
        climbed.push(at);
        at = tree.node(at).parent;
    }
    for &node in climbed.iter().rev() {
        ctime = ctime.max(recycled_ctime(tree, entries, node).unwrap_or(0));
        newest.insert(node, ctime);
    }
    ctime
}

fn ctime_for(entries: &[RecycleEntryCtime], tree: u64, inode: u64) -> Option<u64> {
    entries
        .binary_search_by_key(&(tree, inode), |entry| (entry.tree, entry.inode))
        .ok()
        .map(|index| entries[index].ctime)
}

fn recycle_bin_container(tree: &ScopeTree, node: u32) -> bool {
    if tree.name(node) == b"#recycle" {
        return true;
    }
    tree.node(node).parent == crate::model::tree::VIRTUAL_ROOT
        && tree.path(node).ends_with(b"/#recycle")
}

fn tree_memory_estimate(unit: &UnitInfo) -> io::Result<u64> {
    let stored_bytes = fs::metadata(unit.files.join(scan::TREE))?.len();
    let nodes = unit.summary.nodes;
    let inodes = unit.summary.inodes;
    let node_bytes = std::mem::size_of::<crate::model::tree::Node>() as u64
        + std::mem::size_of::<(u32, u32)>() as u64
        + std::mem::size_of::<u32>() as u64;
    let inode_bytes = std::mem::size_of::<crate::model::tree::TreeInode>() as u64;
    let ctime_size = std::mem::size_of::<RecycleEntryCtime>() as u64;
    let recycle_ctime_bytes = unit
        .summary
        .recycle_ctimes
        .saturating_add(unit.summary.recycle_ctime_nodes)
        .saturating_mul(ctime_size);
    Ok(stored_bytes
        .saturating_add(nodes.saturating_mul(node_bytes))
        .saturating_add(inodes.saturating_mul(inode_bytes))
        .saturating_add(recycle_ctime_bytes))
}

fn reserve(
    boundary: &mut dyn crate::btrfs::walk::BatchBoundary,
    bytes: u64,
) -> Result<(), crate::btrfs::walk::WalkError> {
    match boundary.reserve(bytes) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(reason) => Err(crate::btrfs::walk::WalkError::Stopped(reason)),
    }
}

fn grow_records(
    records: &mut Vec<OwnerRecord>,
    boundary: &mut dyn crate::btrfs::walk::BatchBoundary,
) -> Result<(), crate::btrfs::walk::WalkError> {
    if records.len() == records.capacity() {
        let next = records.capacity().max(4).saturating_mul(2);
        let bytes = (next - records.capacity()) * std::mem::size_of::<OwnerRecord>();
        reserve(boundary, bytes as u64)?;
        records.reserve(next - records.capacity());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::subvolume::Subvolume;

    fn direct_issue(
        subcase: crate::overview::DirectOwnerSubcase,
        root_id: u64,
        root_kind: crate::overview::DirectOwnerRootKind,
        root_class: crate::overview::DirectOwnerRootClass,
        at_directory: bool,
    ) -> Owner {
        Owner::DirectOwnerUnresolved(crate::overview::DirectOwnerDiagnostic {
            subcase,
            root_id,
            root_kind,
            root_class,
            at_directory,
        })
    }

    fn subvolume(id: u64, name: &[u8], readonly: bool, parent_uuid: Option<[u8; 16]>) -> Subvolume {
        Subvolume {
            id,
            parent: Some(crate::btrfs::subvolume::Link {
                parent_id: crate::btrfs::keys::FS_TREE_OBJECTID,
                dirid: crate::btrfs::keys::FIRST_FREE_OBJECTID,
            }),
            name: name.to_vec(),
            generation: 1,
            readonly,
            pending_deletion: false,
            uuid: Some([id as u8; 16]),
            parent_uuid,
            last_snapshot: 0,
            otime: None,
        }
    }

    fn subvolumes(mut items: Vec<Subvolume>) -> SubvolumeSet {
        items.sort_unstable_by_key(|item| item.id);
        SubvolumeSet {
            subvolumes: items,
            ..SubvolumeSet::default()
        }
    }

    fn shared_folder_index(root: u64, name: &[u8]) -> OwnerIndex {
        OwnerIndex {
            share_names: BTreeMap::from([(root, name.to_vec())]),
            share_ids: BTreeMap::from([(name.to_vec(), root)]),
            volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
            ..OwnerIndex::default()
        }
    }

    #[test]
    fn classifies_service_recycle_share_and_coverage_paths() {
        let shares = BTreeMap::from([(b"photos".to_vec(), 256)]);
        let service = classify_path(
            b"/volume1/@docker/container/file",
            NodeFlags::SYSTEM,
            b"/volume1",
            &shares,
            &[],
        );
        assert_eq!(
            service,
            Owner::Live(LiveOwner::Service {
                service: dsm::service_id_by_code("container_manager")
            })
        );
        assert_eq!(
            classify_path(
                b"/volume1/photos/#recycle/old.jpg",
                NodeFlags::RECYCLE,
                b"/volume1",
                &shares,
                &[],
            ),
            Owner::Live(LiveOwner::Recycle { share: 256 })
        );
        assert_eq!(
            classify_path(
                b"/volume1/photos/album/image.jpg",
                NodeFlags::default(),
                b"/volume1",
                &shares,
                &[],
            ),
            Owner::Live(LiveOwner::Share { share: 256 })
        );
        assert_eq!(
            classify_path(
                b"/volume1/photos/locked/file",
                NodeFlags::ENCRYPTED,
                b"/volume1",
                &shares,
                &[],
            ),
            Owner::Unknown(UnknownReason::CoverageGap)
        );
        assert_eq!(
            classify_path(
                b"/volume1/unindexed/file",
                NodeFlags::default(),
                b"/volume1",
                &shares,
                &[],
            ),
            Owner::Live(LiveOwner::SystemOther)
        );
    }

    /// Many writable snapshot/clone trees must not retain one owner record
    /// per file across the volume. Trees intentionally alternate categories,
    /// so a uniform-owner shortcut cannot satisfy this memory regression.
    #[test]
    fn many_subvolume_owner_index_has_bounded_retained_memory() {
        use crate::btrfs::walk::testing::NoStop;
        use crate::model::testing::{DIR, FILE, entry, inode, read_tree};
        use crate::model::tree::{Marks, RootKind};
        use crate::scan::{TREE, UnitSummary};
        use crate::spill::{Spill, testing::scratch};
        const ROOTS: u64 = 500;
        const FILES: u64 = 512;
        let dir = scratch("many-owner-trees");
        let spill = Spill::create(dir.join("run")).unwrap();
        let mut units = Vec::new();
        let mut roots = Vec::new();
        for root in 256..256 + ROOTS {
            roots.push(crate::model::testing::linked(
                root,
                5,
                256,
                &format!("photos{root}"),
            ));
            let mut items = vec![
                inode(256, DIR, 0, 1),
                inode(257, DIR, 0, 1),
                entry(256, 2, "#recycle", 257),
            ];
            for n in 0..FILES {
                items.push(inode(258 + n, FILE, 4096, 1));
                items.push(entry(
                    if n % 2 == 0 { 256 } else { 257 },
                    3 + n,
                    &format!("file{n}"),
                    258 + n,
                ));
            }
            let ns = read_tree(root, items).unwrap();
            let mut tree = ScopeTree::new();
            tree.add_root(
                &ns,
                256,
                format!("/volume1/photos{root}").as_bytes(),
                RootKind::Path,
                &SubvolumeSet::default(),
                &Marks::default(),
            )
            .unwrap();
            tree.prepare();
            let files = spill.path(&format!("unit{root}"));
            fs::create_dir(&files).unwrap();
            tree.store(fs::File::create(files.join(TREE)).unwrap())
                .unwrap();
            units.push(UnitInfo {
                files,
                summary: UnitSummary {
                    nodes: tree.len() as u64,
                    inodes: tree.inode_count() as u64,
                    ..UnitSummary::default()
                },
                ..UnitInfo::default()
            });
        }
        let before = subvolumes(roots);
        let index =
            OwnerIndex::build(&units, &before, b"/volume1", 5, spill.dir(), &mut NoStop).unwrap();
        let retained: usize = index.by_tree.retained_bytes();
        eprintln!(
            "owner records: {} files, {retained} retained bytes",
            ROOTS * FILES
        );
        assert!(
            retained < 2 << 20,
            "owner index retains {retained} bytes across {ROOTS} trees"
        );
        for root in 256..256 + ROOTS {
            assert_eq!(
                index.owner(root, 258, &before).unwrap(),
                Owner::Live(LiveOwner::Share { share: root })
            );
            assert_eq!(
                index.owner(root, 259, &before).unwrap(),
                Owner::Live(LiveOwner::Recycle { share: root })
            );
            assert!(index.lookup(root, 258 + FILES).unwrap().is_none());
        }
        assert!(index.by_tree.retained_bytes() < 2 << 20);
        eprintln!(
            "owner cache after lookups: {} retained bytes",
            index.by_tree.retained_bytes()
        );
    }

    #[test]
    fn recycle_ctimes_are_joined_by_tree_and_inode() {
        let owners = OwnerIndex {
            recycle_ctimes: vec![
                RecycleEntryCtime {
                    tree: 100,
                    inode: 7,
                    ctime: 20,
                },
                RecycleEntryCtime {
                    tree: 101,
                    inode: 7,
                    ctime: 10,
                },
            ],
            ..OwnerIndex::default()
        };
        let recycle = Owner::Live(LiveOwner::Recycle { share: 256 });

        assert_eq!(
            owners.with_recycle_ctime(recycle, 100, 7),
            Owner::RecycleEntry {
                share: 256,
                ctime: 20,
            }
        );
        assert_eq!(
            owners.with_recycle_ctime(recycle, 101, 7),
            Owner::RecycleEntry {
                share: 256,
                ctime: 10,
            }
        );
    }

    #[test]
    fn a_recycled_directory_ctime_covers_its_unchanged_descendants() {
        let ns = crate::model::testing::read_tree(
            256,
            vec![
                crate::model::testing::inode_with_ctime(
                    256,
                    crate::model::testing::DIR,
                    0,
                    1,
                    timespec(10),
                ),
                crate::model::testing::entry(256, 2, "#recycle", 257),
                crate::model::testing::inode_with_ctime(
                    257,
                    crate::model::testing::DIR,
                    0,
                    1,
                    timespec(20),
                ),
                crate::model::testing::entry(257, 2, "old-folder", 258),
                crate::model::testing::inode_with_ctime(
                    258,
                    crate::model::testing::DIR,
                    0,
                    1,
                    timespec(300),
                ),
                crate::model::testing::entry(258, 2, "old-file", 259),
                crate::model::testing::inode_with_ctime(
                    259,
                    crate::model::testing::FILE,
                    0,
                    1,
                    timespec(200),
                ),
            ],
        )
        .unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/volume1",
            crate::model::tree::RootKind::Path,
            &SubvolumeSet::default(),
            &crate::model::tree::Marks::default(),
        )
        .unwrap();

        let direct = tree.take_recycle_ctimes();
        let mut effective = Vec::new();
        super::append_effective_recycle_ctimes(&tree, &direct, &mut effective).unwrap();

        assert_eq!(effective.len(), 2);
        assert_eq!(
            effective
                .iter()
                .find(|entry| entry.inode == 259)
                .unwrap()
                .ctime,
            300
        );
    }

    #[test]
    fn a_deep_recycled_chain_takes_the_newest_ctime_above_each_entry() {
        use crate::model::testing::{DIR, entry, inode_with_ctime, read_tree};
        const DEPTH: u64 = 2000;
        let ctime = |level: u64| if level == 70 { 9_000 } else { 100 + level };
        let mut items = vec![
            inode_with_ctime(256, DIR, 0, 1, timespec(10)),
            entry(256, 2, "#recycle", 257),
            inode_with_ctime(257, DIR, 0, 1, timespec(20)),
        ];
        for level in 0..DEPTH {
            let (parent, child) = (257 + level, 258 + level);
            items.push(entry(parent, 2, "d", child));
            items.push(inode_with_ctime(child, DIR, 0, 1, timespec(ctime(level))));
        }
        let ns = read_tree(256, items).unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/volume1",
            crate::model::tree::RootKind::Path,
            &SubvolumeSet::default(),
            &crate::model::tree::Marks::default(),
        )
        .unwrap();

        let direct = tree.take_recycle_ctimes();
        let mut effective = Vec::new();
        super::append_effective_recycle_ctimes(&tree, &direct, &mut effective).unwrap();

        assert_eq!(effective.len(), crate::model::tree::MAX_DEPTH - 1);
        for found in &effective {
            let level = found.inode - 258;
            let expected = (0..=level).map(ctime).max().unwrap();
            assert_eq!(found.ctime, expected, "level {level}");
        }
    }

    fn timespec(sec: u64) -> crate::btrfs::parser::BtrfsTimespec {
        crate::btrfs::parser::BtrfsTimespec { sec, nsec: 0 }
    }

    #[test]
    fn an_unknown_root_reports_its_direct_owner_subcase() {
        let owners = OwnerIndex::default();
        assert_eq!(
            owners.owner(900, 17, &SubvolumeSet::default()).unwrap(),
            direct_issue(
                crate::overview::DirectOwnerSubcase::RootNotDiscovered,
                900,
                crate::overview::DirectOwnerRootKind::NotDiscovered,
                crate::overview::DirectOwnerRootClass::NotDiscovered,
                false,
            )
        );
    }

    #[test]
    fn an_unmapped_readonly_root_reports_snapshot_ancestry_failure() {
        let before = subvolumes(vec![subvolume(257, b"snapshot-root", true, Some([9; 16]))]);
        let owners = OwnerIndex::default();
        assert_eq!(
            owners.owner(257, 17, &before).unwrap(),
            direct_issue(
                crate::overview::DirectOwnerSubcase::ReadonlyRootParentUuidUnmapped,
                257,
                crate::overview::DirectOwnerRootKind::ReadOnly,
                crate::overview::DirectOwnerRootClass::Snapshot,
                false,
            )
        );
    }

    /// DSM keeps a share's snapshots in `@sharesnap/<share>/`.
    fn share_snapshot(share: &[u8], parent_uuid: Option<[u8; 16]>) -> SubvolumeSet {
        let sharesnap = subvolume(284, b"@sharesnap", false, None);
        let mut holder = subvolume(1388, share, false, None);
        holder.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: sharesnap.id,
            dirid: crate::btrfs::keys::FIRST_FREE_OBJECTID,
        });
        let mut snapshot = subvolume(1390, b"GMT+08-2024.12.31-15.37.42", true, parent_uuid);
        snapshot.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: holder.id,
            dirid: crate::btrfs::keys::FIRST_FREE_OBJECTID,
        });
        subvolumes(vec![sharesnap, holder, snapshot])
    }

    #[test]
    fn a_readonly_root_in_a_shares_sharesnap_folder_is_its_snapshot() {
        let before = share_snapshot(b"Master", Some([0x16; 16]));
        let owners = shared_folder_index(1391, b"Master");
        let snapshot = Owner::Snapshot { share: 1391 };
        assert_eq!(owners.owner(1390, 17, &before).unwrap(), snapshot);
        assert_eq!(owners.root_owner(1390, &before), snapshot);
    }

    #[test]
    fn a_readonly_root_in_the_folder_of_no_share_belongs_to_snapshot_replication() {
        let before = share_snapshot(b"gone", Some([0x16; 16]));
        let owners = OwnerIndex {
            volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
            ..shared_folder_index(1391, b"Master")
        };
        let service = Owner::Live(LiveOwner::Service {
            service: dsm::service_id_by_code("share_snapshots"),
        });
        assert_eq!(owners.owner(1390, 17, &before).unwrap(), service);
    }

    #[test]
    fn a_readonly_root_stored_inside_a_service_root_belongs_to_that_service() {
        let iscsi = subvolume(1019, b"@iSCSI", false, None);
        let mut holder = subvolume(1351, b"Snapshot", false, None);
        holder.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: iscsi.id,
            dirid: crate::btrfs::keys::FIRST_FREE_OBJECTID,
        });
        let mut snapshot = subvolume(1537, b"lun-snapshot", true, Some([0xc6; 16]));
        snapshot.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: holder.id,
            dirid: 700,
        });
        let before = subvolumes(vec![iscsi, holder, snapshot]);
        let owners = OwnerIndex {
            volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
            ..OwnerIndex::default()
        };
        let service = Owner::Live(LiveOwner::Service {
            service: dsm::service_id_by_code("iscsi_luns"),
        });
        assert_eq!(owners.owner(1537, 17, &before).unwrap(), service);
    }

    #[test]
    fn a_writable_root_without_an_inode_owner_reports_its_subcase() {
        let before = subvolumes(vec![subvolume(256, b"backup", false, None)]);
        let owners = shared_folder_index(256, b"backup");
        assert_eq!(
            owners.owner(256, 17, &before).unwrap(),
            direct_issue(
                crate::overview::DirectOwnerSubcase::WritableRootInodeUnindexed,
                256,
                crate::overview::DirectOwnerRootKind::Writable,
                crate::overview::DirectOwnerRootClass::SharedFolder,
                false,
            )
        );
    }

    #[test]
    fn an_unindexed_inode_in_a_service_root_keeps_the_root_service_owner() {
        let root = 1216;
        let before = subvolumes(vec![subvolume(root, b"@ABM_Encrypted@", false, None)]);
        let service = Owner::Live(LiveOwner::Service {
            service: dsm::service_id_by_code("active_backup_volume"),
        });
        let owners = OwnerIndex {
            volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
            ..OwnerIndex::default()
        };

        assert_eq!(owners.owner(root, 34_681_212, &before).unwrap(), service);
    }

    #[test]
    fn readonly_descendants_of_a_service_root_map_to_that_service() {
        let service_root = subvolume(1216, b"@ABM_Encrypted@", false, None);
        let snapshot = subvolume(1269, b"snapshot", true, service_root.uuid);
        let before = subvolumes(vec![service_root, snapshot]);
        let service = Owner::Live(LiveOwner::Service {
            service: dsm::service_id_by_code("active_backup_volume"),
        });
        let owners = OwnerIndex {
            volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
            ..OwnerIndex::default()
        };

        assert_eq!(owners.owner(1269, 34_681_212, &before).unwrap(), service);
    }

    #[test]
    fn nested_service_roots_keep_unindexed_inode_ownership() {
        let mut service_root = subvolume(400, b"service-parent", false, None);
        service_root.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: crate::btrfs::keys::FS_TREE_OBJECTID,
            dirid: crate::btrfs::keys::FIRST_FREE_OBJECTID,
        });
        let mut nested_root = subvolume(401, b"nested-root", false, None);
        nested_root.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: service_root.id,
            dirid: 500,
        });
        let before = subvolumes(vec![service_root, nested_root]);
        let service = Owner::Live(LiveOwner::Service {
            service: dsm::service_id_by_code("active_backup_volume"),
        });
        let owners = OwnerIndex {
            root_owners: BTreeMap::from([(401, encode(service))]),
            volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
            ..OwnerIndex::default()
        };

        assert_eq!(owners.owner(401, 17, &before).unwrap(), service);
    }

    #[test]
    fn readonly_descendants_of_nested_service_roots_keep_service_ownership() {
        let mut service_root = subvolume(400, b"service-parent", false, None);
        service_root.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: crate::btrfs::keys::FS_TREE_OBJECTID,
            dirid: crate::btrfs::keys::FIRST_FREE_OBJECTID,
        });
        let mut nested_root = subvolume(401, b"nested-root", false, None);
        nested_root.parent = Some(crate::btrfs::subvolume::Link {
            parent_id: service_root.id,
            dirid: 500,
        });
        let snapshot = subvolume(402, b"snapshot-root", true, nested_root.uuid);
        let before = subvolumes(vec![service_root, nested_root, snapshot]);
        let service = Owner::Live(LiveOwner::Service {
            service: dsm::service_id_by_code("active_backup_volume"),
        });
        let owners = OwnerIndex {
            root_owners: BTreeMap::from([(401, encode(service))]),
            volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
            ..OwnerIndex::default()
        };

        assert_eq!(owners.owner(402, 17, &before).unwrap(), service);
    }

    #[test]
    fn an_indexed_unknown_at_directory_is_other_dsm_data() {
        let before = subvolumes(vec![subvolume(301, b"@vendor-cache", false, None)]);
        let mut owners = OwnerIndex::default();
        let encoded = encode(classify_path(
            b"/volume2/@vendor-cache/file",
            NodeFlags::default(),
            b"/volume2",
            &BTreeMap::new(),
            &[],
        ));
        owners
            .by_tree
            .insert(
                301,
                &[OwnerRecord {
                    inode: 17,
                    owner: encoded,
                }],
                &crate::spill::testing::scratch("owner-other-service").join("owners"),
            )
            .unwrap();
        assert_eq!(
            owners.owner(301, 17, &before).unwrap(),
            Owner::Live(LiveOwner::Service {
                service: dsm::OTHER_SERVICE_ID
            })
        );
    }

    #[test]
    fn service_roots_map_to_services_and_unknown_snapshots_keep_diagnostics() {
        for (root, name, readonly, expected) in [
            (
                310,
                b"@ActiveBackup".as_slice(),
                false,
                Owner::Live(LiveOwner::Service {
                    service: dsm::service_id_by_code("active_backup"),
                }),
            ),
            (
                311,
                b"@iSCSI".as_slice(),
                false,
                Owner::Live(LiveOwner::Service {
                    service: dsm::service_id_by_code("iscsi_luns"),
                }),
            ),
            (
                312,
                b"snapshot".as_slice(),
                true,
                direct_issue(
                    crate::overview::DirectOwnerSubcase::ReadonlyRootParentUuidUnmapped,
                    312,
                    crate::overview::DirectOwnerRootKind::ReadOnly,
                    crate::overview::DirectOwnerRootClass::Snapshot,
                    false,
                ),
            ),
        ] {
            let before = subvolumes(vec![subvolume(root, name, readonly, None)]);
            let owners = OwnerIndex {
                volume_tree: crate::btrfs::keys::FS_TREE_OBJECTID,
                ..OwnerIndex::default()
            };
            let issue = owners.owner(root, 17, &before).unwrap();
            assert_eq!(issue, expected);
        }
    }

    #[test]
    fn conflicting_indexed_owners_keep_a_distinct_direct_owner_subcase() {
        let before = subvolumes(vec![subvolume(256, b"backup", false, None)]);
        let mut owners = shared_folder_index(256, b"backup");
        owners
            .by_tree
            .insert(
                256,
                &[OwnerRecord {
                    inode: 17,
                    owner: OWNER_CONFLICT,
                }],
                &crate::spill::testing::scratch("owner-conflict").join("owners"),
            )
            .unwrap();
        assert_eq!(
            owners.owner(256, 17, &before).unwrap(),
            direct_issue(
                crate::overview::DirectOwnerSubcase::IndexedOwnerConflict,
                256,
                crate::overview::DirectOwnerRootKind::Writable,
                crate::overview::DirectOwnerRootClass::SharedFolder,
                false,
            )
        );
    }

    #[test]
    fn marker_files_classify_their_containing_folder_as_package_data() {
        let shares = BTreeMap::from([(b"backup".to_vec(), 256)]);
        let markers = vec![(
            b"/volume1/backup/target".to_vec(),
            dsm::service_id_by_code("hyper_backup"),
        )];
        assert_eq!(
            classify_path(
                b"/volume1/backup/target/old-version",
                NodeFlags::default(),
                b"/volume1",
                &shares,
                &markers,
            ),
            Owner::Live(LiveOwner::Service {
                service: dsm::service_id_by_code("hyper_backup")
            })
        );
    }

    #[test]
    fn owner_codes_round_trip_and_conflicting_hardlinks_become_unknown() {
        let owners = [
            Owner::Pending,
            Owner::Snapshot { share: 256 },
            Owner::SharedBetweenFolders,
            Owner::Live(LiveOwner::Recycle { share: 256 }),
            Owner::Live(LiveOwner::Service { service: 4 }),
            Owner::Live(LiveOwner::Share { share: 256 }),
            Owner::Unknown(UnknownReason::OwnerBudgetExhausted),
        ];
        for owner in owners {
            assert_eq!(decode(encode(owner)), owner);
        }
        let conflict = collapse(vec![
            OwnerRecord {
                inode: 5,
                owner: encode(Owner::Live(LiveOwner::Share { share: 256 })),
            },
            OwnerRecord {
                inode: 5,
                owner: encode(Owner::Live(LiveOwner::Recycle { share: 256 })),
            },
        ]);
        assert_eq!(decode(conflict[0].owner), Owner::SharedBetweenFolders);
    }
}
