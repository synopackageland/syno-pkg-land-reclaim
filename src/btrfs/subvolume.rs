//! Subvolume discovery from the root tree (tree 1): `ROOT_ITEM` for
//! identity/generation and `ROOT_BACKREF` for the parent link.

use crate::btrfs::keys::*;
use crate::btrfs::parser::{RootItem, parse_inode_refs, parse_root_item, parse_root_ref};
use crate::btrfs::search::{Item, SearchRange};
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_tree};
use std::collections::{BTreeMap, BTreeSet};

/// Where a subvolume is linked into its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub parent_id: u64,
    /// Directory inode in the parent subvolume.
    pub dirid: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subvolume {
    pub id: u64,
    /// `None` for the top-level subvolume or an unlinked (deleted) one.
    pub parent: Option<Link>,
    /// Name within the parent directory; empty when unlinked.
    pub name: Vec<u8>,
    pub generation: u64,
    pub readonly: bool,
    /// Deleted and awaiting the Btrfs cleaner (`refs == 0`, SPEC 8.1).
    pub pending_deletion: bool,
    pub uuid: Option<[u8; 16]>,
    pub parent_uuid: Option<[u8; 16]>,
    /// `ROOT_ITEM.last_snapshot`.
    pub last_snapshot: u64,
    /// Creation time, Unix seconds, when the root item records it.
    pub otime: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubvolumeSet {
    /// Sorted by subvolume ID.
    pub subvolumes: Vec<Subvolume>,
    /// A quota tree exists, so qgroup accounting runs on commits.
    pub quota_enabled: bool,
    /// Roots a balance is relocating: each has a relocation tree sharing
    /// its blocks (SPEC 8.1).
    pub relocating: BTreeSet<u64>,
}

/// Which tree blocks of a subvolume snapshotting may have shared, from
/// the kernel's own test (`btrfs_block_can_be_shared`): a block written
/// after the root's last snapshot never is. File extents found in older
/// leaves may be referenced implicitly by another root (SPEC 8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeafSharing {
    /// Leaves with a generation up to this one may be shared; 0: none.
    pub up_to_generation: u64,
    /// The root is a read-only snapshot or directly related to one.
    pub with_snapshot: bool,
}

impl LeafSharing {
    pub fn may_share(&self, leaf_generation: u64) -> bool {
        leaf_generation <= self.up_to_generation
    }
}

impl SubvolumeSet {
    pub fn get(&self, id: u64) -> Option<&Subvolume> {
        self.subvolumes
            .binary_search_by_key(&id, |s| s.id)
            .ok()
            .map(|i| &self.subvolumes[i])
    }

    /// The linked subvolume names from the filesystem root to `id`.
    /// Directory names between subvolume roots are not represented here.
    pub fn subvolume_path(&self, id: u64) -> Vec<u8> {
        let mut components = Vec::new();
        let mut at = id;
        for _ in 0..=self.subvolumes.len() {
            let Some(root) = self.get(at) else {
                break;
            };
            if root.name.is_empty() {
                break;
            }
            components.push(root.name.as_slice());
            let Some(parent) = root.parent else {
                break;
            };
            at = parent.parent_id;
        }
        components.reverse();
        let mut path = Vec::new();
        for (index, component) in components.into_iter().enumerate() {
            if index > 0 {
                path.push(b'/');
            }
            path.extend_from_slice(component);
        }
        path
    }

    /// The full linked path of a subvolume, including directories between
    /// subvolume roots. `None` means the root or one of its directory links
    /// could not be resolved from the Btrfs metadata.
    pub fn full_subvolume_path<S: MetadataSource + ?Sized>(
        &self,
        source: &mut S,
        id: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Option<Vec<u8>>, WalkError> {
        FullPathResolver::default().root_path(source, self, id, boundary, &mut BTreeSet::new())
    }

    pub fn leaf_sharing(&self, id: u64) -> LeafSharing {
        let Some(root) = self.get(id) else {
            return LeafSharing::default();
        };
        LeafSharing {
            up_to_generation: if self.may_share_blocks(root) {
                root.last_snapshot
            } else {
                0
            },
            with_snapshot: self.with_snapshot(id),
        }
    }

    /// Subvolumes whose `parent_uuid` names subvolume `id`: its snapshots.
    pub fn snapshots_of(&self, id: u64) -> Vec<u64> {
        let Some(uuid) = self.get(id).and_then(|s| s.uuid) else {
            return Vec::new();
        };
        self.subvolumes
            .iter()
            .filter(|s| s.parent_uuid == Some(uuid))
            .map(|s| s.id)
            .collect()
    }

    /// Whether subvolume `id` is a read-only snapshot or directly related
    /// to one.
    pub fn with_snapshot(&self, id: u64) -> bool {
        self.get(id).is_some_and(|root| {
            root.readonly
                || self
                    .subvolumes
                    .iter()
                    .any(|s| s.id != id && s.readonly && root.is_related(s))
        })
    }
}

/// Resolves names from `INODE_REF` chains, caching directory paths.
#[derive(Default)]
pub struct FullPathResolver {
    roots: BTreeMap<u64, Option<Vec<u8>>>,
    directories: BTreeMap<(u64, u64), Option<Vec<u8>>>,
}

impl FullPathResolver {
    fn root_path<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        set: &SubvolumeSet,
        id: u64,
        boundary: &mut dyn BatchBoundary,
        visiting: &mut BTreeSet<u64>,
    ) -> Result<Option<Vec<u8>>, WalkError> {
        if id == FS_TREE_OBJECTID {
            return Ok(Some(Vec::new()));
        }
        if let Some(path) = self.roots.get(&id) {
            return Ok(path.clone());
        }
        if !visiting.insert(id) {
            return Ok(None);
        }

        let result: Result<Option<Vec<u8>>, WalkError> = (|| {
            let Some(root) = set.get(id) else {
                return Ok(None);
            };
            let Some(link) = root.parent else {
                return Ok(None);
            };
            let Some(mut path) = self.root_path(source, set, link.parent_id, boundary, visiting)?
            else {
                return Ok(None);
            };
            let Some(directory) =
                self.directory_path(source, link.parent_id, link.dirid, boundary)?
            else {
                return Ok(None);
            };
            append_component(&mut path, &directory);
            append_component(&mut path, &root.name);
            Ok(Some(path))
        })();

        visiting.remove(&id);
        let path = result?;
        self.roots.insert(id, path.clone());
        Ok(path)
    }

    /// The path of inode `ino` below the root of tree `tree_id`, by its
    /// first name. `None` when a name on the way cannot be resolved.
    pub fn inode_path<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        tree_id: u64,
        ino: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Option<Vec<u8>>, WalkError> {
        let Some((parent, name)) = first_inode_ref(source, tree_id, ino, boundary)? else {
            return Ok(None);
        };
        let Some(mut path) = self.directory_path(source, tree_id, parent, boundary)? else {
            return Ok(None);
        };
        append_component(&mut path, &name);
        Ok(Some(path))
    }

    fn directory_path<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        tree_id: u64,
        dirid: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Option<Vec<u8>>, WalkError> {
        if dirid == FIRST_FREE_OBJECTID {
            return Ok(Some(Vec::new()));
        }
        if let Some(path) = self.directories.get(&(tree_id, dirid)) {
            return Ok(path.clone());
        }

        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = dirid;
        let mut complete = false;
        for _ in 0..1024 {
            if current == FIRST_FREE_OBJECTID {
                complete = true;
                break;
            }
            if !seen.insert(current) {
                break;
            }

            let mut parent = None;
            let mut ambiguous = false;
            let range = SearchRange::new(
                tree_id,
                Key::new(current, INODE_REF_KEY, 0),
                Key::new(current, INODE_REF_KEY, u64::MAX),
            );
            walk_tree(source, range, boundary, |item| {
                let refs = parse_inode_refs(item.data).map_err(|source| WalkError::Item {
                    key: item.key,
                    source,
                })?;
                for inode_ref in refs {
                    let candidate = (item.key.offset, inode_ref.name.to_vec());
                    if parent
                        .as_ref()
                        .is_some_and(|previous| previous != &candidate)
                    {
                        ambiguous = true;
                    } else {
                        parent = Some(candidate);
                    }
                }
                Ok(())
            })?;

            if ambiguous {
                break;
            }
            let Some((parent_dir, name)) = parent else {
                break;
            };
            names.push(name);
            current = parent_dir;
        }

        let path = if complete {
            let mut path = Vec::new();
            for name in names.iter().rev() {
                append_component(&mut path, name);
            }
            Some(path)
        } else {
            None
        };
        self.directories.insert((tree_id, dirid), path.clone());
        Ok(path)
    }
}

/// The parent directory and name of the first `INODE_REF` of `ino`.
fn first_inode_ref<S: MetadataSource + ?Sized>(
    source: &mut S,
    tree_id: u64,
    ino: u64,
    boundary: &mut dyn BatchBoundary,
) -> Result<Option<(u64, Vec<u8>)>, WalkError> {
    let mut first = None;
    let range = SearchRange::new(
        tree_id,
        Key::new(ino, INODE_REF_KEY, 0),
        Key::new(ino, INODE_REF_KEY, u64::MAX),
    );
    walk_tree(source, range, boundary, |item| {
        if first.is_none() {
            let refs = parse_inode_refs(item.data).map_err(|source| WalkError::Item {
                key: item.key,
                source,
            })?;
            first = refs
                .first()
                .map(|inode_ref| (item.key.offset, inode_ref.name.to_vec()));
        }
        Ok(())
    })?;
    Ok(first)
}

fn append_component(path: &mut Vec<u8>, component: &[u8]) {
    if component.is_empty() {
        return;
    }
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(component);
}

/// The snapshots of one live subvolume (SPEC 5.14).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshots {
    /// Read-only subvolumes whose `parent_uuid` chain leads to it.
    pub snapshots: Vec<u64>,
    /// Such subvolumes deleted and awaiting the cleaner.
    pub pending: Vec<u64>,
}

impl SubvolumeSet {
    /// The subvolume in whose root directory `subvolume` is linked.
    pub fn root_dir_parent<'a>(&'a self, subvolume: &Subvolume) -> Option<&'a Subvolume> {
        let link = subvolume.parent?;
        subvolume
            .in_root_dir_of(link.parent_id)
            .then(|| self.get(link.parent_id))
            .flatten()
    }

    /// The `@sharesnap/<share>` folder holding the read-only root `id`:
    /// DSM keeps a share's snapshots there, also when the subvolume the
    /// snapshot was taken from is gone, so its `parent_uuid` lineage
    /// reaches nothing present (a restored share, or a promoted Snapshot
    /// Replication copy). `None` unless `id` is linked in such a folder's
    /// root directory; the folder's name is the share name.
    pub fn sharesnap_folder(&self, id: u64) -> Option<&Subvolume> {
        let folder = self.root_dir_parent(self.get(id)?)?;
        let sharesnap = self.root_dir_parent(folder)?;
        (sharesnap.name == b"@sharesnap").then_some(folder)
    }

    /// The read-only subvolumes whose chain of `parent_uuid`s leads to
    /// `live`, or that DSM keeps in its `@sharesnap` folder, in id order.
    pub fn share_snapshots(&self, live: u64) -> Snapshots {
        let mut of = Snapshots::default();
        let Some(live) = self.get(live) else {
            return of;
        };
        let descends = |s: &Subvolume| {
            let Some(target) = live.uuid else {
                return false;
            };
            let mut at = s;
            for _ in 0..self.subvolumes.len() {
                match at.parent_uuid {
                    Some(p) if p == target => return true,
                    Some(p) => match self.subvolumes.iter().find(|x| x.uuid == Some(p)) {
                        Some(next) => at = next,
                        None => return false,
                    },
                    None => return false,
                }
            }
            false
        };
        let in_sharesnap = |s: &Subvolume| {
            self.sharesnap_folder(s.id).is_some_and(|folder| {
                folder.name == live.name
                    && self
                        .root_dir_parent(folder)
                        .is_some_and(|sharesnap| sharesnap.parent == live.parent)
            })
        };
        for s in self
            .subvolumes
            .iter()
            .filter(|s| s.readonly || s.pending_deletion)
        {
            if s.id != live.id && (descends(s) || in_sharesnap(s)) {
                match s.pending_deletion {
                    true => of.pending.push(s.id),
                    false => of.snapshots.push(s.id),
                }
            }
        }
        of
    }

    /// Every root that may share tree blocks with `id`: itself, its
    /// relatives through `parent_uuid`s, and any root whose relations the
    /// root items hide; every root when `id` has no uuid.
    pub fn sharing_family(&self, id: u64) -> BTreeSet<u64> {
        let Some(root) = self.get(id).filter(|r| r.uuid.is_some()) else {
            return self.subvolumes.iter().map(|s| s.id).chain([id]).collect();
        };
        let related = self.relatives(root).chain(self.hidden_relations(id));
        related.map(|s| s.id).chain([id]).collect()
    }
}

impl SubvolumeSet {
    /// Whether another root may still share tree blocks `root` had at its
    /// last snapshot. Only root items that prove no such root exists clear
    /// it: every snapshot carries its source's uuid as `parent_uuid`, and a
    /// snapshot whose source is gone keeps pointing at a missing uuid.
    fn may_share_blocks(&self, root: &Subvolume) -> bool {
        root.uuid.is_none()
            || self.relocating.contains(&root.id)
            || self.hidden_relations(root.id).next().is_some()
            || self.relatives(root).next().is_some()
    }

    /// Subvolumes other than `id` whose root items cannot rule out a
    /// relation to any root: a `parent_uuid` naming a missing uuid, or a
    /// snapshotted root without a uuid.
    fn hidden_relations(&self, id: u64) -> impl Iterator<Item = &Subvolume> {
        let uuids: BTreeSet<[u8; 16]> = self.subvolumes.iter().filter_map(|s| s.uuid).collect();
        self.subvolumes.iter().filter(move |s| {
            s.parent_uuid.is_some_and(|p| !uuids.contains(&p))
                || (s.id != id && s.uuid.is_none() && s.last_snapshot > 0)
        })
    }

    /// Subvolumes linked to `root` through a chain of `parent_uuid`s.
    fn relatives<'a>(&'a self, root: &'a Subvolume) -> impl Iterator<Item = &'a Subvolume> {
        let mut family: BTreeSet<u64> = BTreeSet::from([root.id]);
        loop {
            let joined: Vec<u64> = self
                .subvolumes
                .iter()
                .filter(|s| !family.contains(&s.id))
                .filter(|s| {
                    family
                        .iter()
                        .filter_map(|&id| self.get(id))
                        .any(|f| s.is_related(f))
                })
                .map(|s| s.id)
                .collect();
            if joined.is_empty() {
                break;
            }
            family.extend(joined);
        }
        self.subvolumes
            .iter()
            .filter(move |s| s.id != root.id && family.contains(&s.id))
    }
}

impl Subvolume {
    /// Whether this subvolume is a shared-folder root of the volume
    /// rooted at `volume_tree`: a present, writable, linked direct child
    /// of that root whose name is not a DSM `@…`/`#…` name (SPEC 4.6).
    pub fn is_share(&self, volume_tree: u64) -> bool {
        self.in_root_dir_of(volume_tree)
            && !self.readonly
            && !self.pending_deletion
            && !self.name.starts_with(b"@")
            && !self.name.starts_with(b"#")
    }

    /// Linked in the root directory of subvolume `parent`.
    pub fn in_root_dir_of(&self, parent: u64) -> bool {
        self.parent
            .is_some_and(|link| link.parent_id == parent && link.dirid == FIRST_FREE_OBJECTID)
    }

    /// Snapshot source, snapshot, or sibling snapshot of `other`.
    fn is_related(&self, other: &Subvolume) -> bool {
        let points_at =
            |a: &Subvolume, b: &Subvolume| a.parent_uuid.is_some_and(|p| Some(p) == b.uuid);
        points_at(self, other)
            || points_at(other, self)
            || self.parent_uuid.is_some() && self.parent_uuid == other.parent_uuid
    }
}

fn is_subvolume_id(id: u64) -> bool {
    id == FS_TREE_OBJECTID || (FIRST_FREE_OBJECTID..=LAST_FREE_OBJECTID).contains(&id)
}

#[derive(Default)]
struct Discovery {
    roots: BTreeMap<u64, RootItem>,
    links: BTreeMap<u64, (Link, Vec<u8>)>,
    quota_enabled: bool,
}

impl Discovery {
    fn add(&mut self, item: Item<'_>) -> Result<(), WalkError> {
        let key = item.key;
        let invalid = |source| WalkError::Item { key, source };
        match key.item_type {
            ROOT_ITEM_KEY if key.objectid == QUOTA_TREE_OBJECTID => self.quota_enabled = true,
            ROOT_ITEM_KEY if is_subvolume_id(key.objectid) => {
                let root = parse_root_item(item.data).map_err(invalid)?;
                self.roots.insert(key.objectid, root);
            }
            ROOT_BACKREF_KEY if is_subvolume_id(key.objectid) => {
                let r = parse_root_ref(item.data).map_err(invalid)?;
                let link = Link {
                    parent_id: key.offset,
                    dirid: r.dirid,
                };
                self.links.insert(key.objectid, (link, r.name.to_vec()));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(mut self) -> SubvolumeSet {
        let subvolumes = self
            .roots
            .iter()
            .map(|(&id, root)| {
                let (parent, name) = self.links.remove(&id).unzip();
                Subvolume {
                    id,
                    parent,
                    name: name.unwrap_or_default(),
                    generation: root.generation,
                    readonly: root.is_readonly(),
                    pending_deletion: root.refs == 0,
                    uuid: root.uuid,
                    parent_uuid: root.parent_uuid,
                    last_snapshot: root.last_snapshot,
                    otime: root.otime,
                }
            })
            .collect();
        SubvolumeSet {
            subvolumes,
            quota_enabled: self.quota_enabled,
            relocating: BTreeSet::new(),
        }
    }
}

/// Enumerates subvolumes and snapshots from the root tree.
pub fn discover<S: MetadataSource + ?Sized>(
    source: &mut S,
    boundary: &mut dyn BatchBoundary,
) -> Result<SubvolumeSet, WalkError> {
    let range = SearchRange::new(
        ROOT_TREE_OBJECTID,
        Key::new(FS_TREE_OBJECTID, 0, 0),
        Key::new(LAST_FREE_OBJECTID, u8::MAX, u64::MAX),
    );
    let mut discovery = Discovery::default();
    walk_tree(source, range, boundary, |item| discovery.add(item))?;
    Ok(discovery.finish())
}

/// The roots that relocation trees in the root tree copy: a balance is
/// moving their extents.
pub fn relocation_trees<S: MetadataSource + ?Sized>(
    source: &mut S,
    boundary: &mut dyn BatchBoundary,
) -> Result<BTreeSet<u64>, WalkError> {
    let range = SearchRange::new(
        ROOT_TREE_OBJECTID,
        Key::new(TREE_RELOC_OBJECTID, ROOT_ITEM_KEY, 0),
        Key::new(TREE_RELOC_OBJECTID, ROOT_ITEM_KEY, u64::MAX),
    );
    let mut found = BTreeSet::new();
    walk_tree(source, range, boundary, |item| {
        found.insert(item.key.offset);
        Ok(())
    })?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::parser::testing::{encode_root_item, encode_root_ref};
    use crate::btrfs::walk::testing::{FakeSource, FakeTrees, NoStop};

    fn inode_ref(name: &[u8]) -> Vec<u8> {
        let mut data = 1_u64.to_le_bytes().to_vec();
        data.extend((name.len() as u16).to_le_bytes());
        data.extend(name);
        data
    }

    fn root(id: u64, generation: u64, flags: u64, refs: u32) -> (Key, Vec<u8>) {
        let item = RootItem {
            generation,
            root_dirid: FIRST_FREE_OBJECTID,
            bytenr: 0,
            last_snapshot: 0,
            flags,
            refs,
            uuid: Some([id as u8; 16]),
            parent_uuid: None,
            otime: None,
        };
        (
            Key::new(id, ROOT_ITEM_KEY, 0),
            encode_root_item(&item, true),
        )
    }

    fn backref(id: u64, parent: u64, dirid: u64, name: &str) -> (Key, Vec<u8>) {
        (
            Key::new(id, ROOT_BACKREF_KEY, parent),
            encode_root_ref(dirid, 1, name.as_bytes()),
        )
    }

    fn discover_items(items: Vec<(Key, Vec<u8>)>) -> SubvolumeSet {
        discover(&mut FakeSource::new(items, 2), &mut NoStop).unwrap()
    }

    #[test]
    fn discovers_top_level_and_nested_subvolumes() {
        let set = discover_items(vec![
            root(FS_TREE_OBJECTID, 10, 0, 1),
            root(256, 20, 0, 1),
            backref(256, FS_TREE_OBJECTID, 256, "share"),
            root(257, 30, 1, 1),
            backref(257, 256, 300, "snap"),
        ]);
        assert_eq!(set.subvolumes.len(), 3);
        let top = &set.subvolumes[0];
        assert_eq!(
            (top.id, top.parent, top.name.as_slice()),
            (5, None, &b""[..])
        );
        let share = set.get(256).unwrap();
        assert_eq!(
            share.parent,
            Some(Link {
                parent_id: 5,
                dirid: 256
            })
        );
        assert_eq!(share.name, b"share");
        assert_eq!(share.generation, 20);
        assert!(!share.readonly);
        let snap = set.get(257).unwrap();
        assert!(snap.readonly);
        assert_eq!(
            snap.parent,
            Some(Link {
                parent_id: 256,
                dirid: 300
            })
        );
    }

    #[test]
    fn a_share_is_a_present_writable_named_direct_child_of_the_volume_root() {
        let child = |name: &[u8]| Subvolume {
            id: 256,
            parent: Some(Link {
                parent_id: FS_TREE_OBJECTID,
                dirid: FIRST_FREE_OBJECTID,
            }),
            name: name.to_vec(),
            generation: 1,
            readonly: false,
            pending_deletion: false,
            uuid: None,
            parent_uuid: None,
            last_snapshot: 0,
            otime: None,
        };
        assert!(child(b"photos").is_share(FS_TREE_OBJECTID));
        assert!(child(b"homes").is_share(FS_TREE_OBJECTID));
        for rejected in [b"@synologydrive".as_slice(), b"#recycle", b"@docker"] {
            assert!(!child(rejected).is_share(FS_TREE_OBJECTID), "{rejected:?}");
        }
        let mut readonly = child(b"snap");
        readonly.readonly = true;
        assert!(!readonly.is_share(FS_TREE_OBJECTID));
        let mut pending = child(b"gone");
        pending.pending_deletion = true;
        assert!(!pending.is_share(FS_TREE_OBJECTID));
        let mut nested = child(b"nested");
        nested.parent = Some(Link {
            parent_id: 256,
            dirid: FIRST_FREE_OBJECTID,
        });
        assert!(!nested.is_share(FS_TREE_OBJECTID));
        let mut not_a_root_link = child(b"mounted");
        not_a_root_link.parent = Some(Link {
            parent_id: FS_TREE_OBJECTID,
            dirid: 300,
        });
        assert!(!not_a_root_link.is_share(FS_TREE_OBJECTID));
        let top = Subvolume {
            parent: None,
            ..child(b"top")
        };
        assert!(!top.is_share(FS_TREE_OBJECTID));
    }

    #[test]
    fn root_tree_path_keeps_ancestor_components_for_service_detection() {
        let set = discover_items(vec![
            root(FS_TREE_OBJECTID, 10, 0, 1),
            root(256, 20, 0, 1),
            backref(256, FS_TREE_OBJECTID, 256, "@iSCSI"),
            root(257, 30, 0, 1),
            backref(257, 256, 300, "lun-uuid"),
            root(258, 40, 1, 1),
            backref(258, 257, 301, "snapshot-name"),
        ]);

        assert_eq!(set.subvolume_path(258), b"@iSCSI/lun-uuid/snapshot-name");
        assert_eq!(set.subvolume_path(FS_TREE_OBJECTID), b"");
    }

    #[test]
    fn full_subvolume_path_includes_directory_components_between_root_links() {
        let set = discover_items(vec![
            root(FS_TREE_OBJECTID, 10, 0, 1),
            root(256, 20, 0, 1),
            backref(256, FS_TREE_OBJECTID, 256, "@iSCSI"),
            root(257, 30, 1, 1),
            backref(257, FS_TREE_OBJECTID, 301, "lun-uuid"),
        ]);
        let mut source = FakeTrees::new([(
            FS_TREE_OBJECTID,
            vec![
                (Key::new(300, INODE_REF_KEY, 256), inode_ref(b"@iSCSI")),
                (Key::new(301, INODE_REF_KEY, 300), inode_ref(b"Snapshot")),
            ],
        )]);

        let path = set
            .full_subvolume_path(&mut source, 257, &mut NoStop)
            .unwrap();

        assert_eq!(
            path.as_deref(),
            Some(b"@iSCSI/Snapshot/lun-uuid".as_slice())
        );
    }

    #[test]
    fn an_inode_path_names_the_file_below_its_tree_root() {
        let mut source = FakeTrees::new([(
            300,
            vec![
                (Key::new(258, INODE_REF_KEY, 256), inode_ref(b"photos")),
                (Key::new(259, INODE_REF_KEY, 258), inode_ref(b"a.jpg")),
                // A hard link: the first name is the one shown.
                (Key::new(260, INODE_REF_KEY, 256), inode_ref(b"first")),
                (Key::new(260, INODE_REF_KEY, 258), inode_ref(b"second")),
                // Its directory has no name.
                (Key::new(261, INODE_REF_KEY, 999), inode_ref(b"lost")),
            ],
        )]);
        let mut paths = FullPathResolver::default();
        let mut path = |ino| {
            paths
                .inode_path(&mut source, 300, ino, &mut NoStop)
                .unwrap()
        };

        assert_eq!(path(259).as_deref(), Some(b"photos/a.jpg".as_slice()));
        assert_eq!(path(260).as_deref(), Some(b"first".as_slice()));
        assert_eq!(path(261), None);
        assert_eq!(path(262), None);
    }

    #[test]
    fn internal_trees_are_not_subvolumes() {
        let set = discover_items(vec![
            root(FS_TREE_OBJECTID, 10, 0, 1),
            root(7, 10, 0, 1),
            root(9, 10, 0, 1),
        ]);
        assert_eq!(set.subvolumes.len(), 1);
    }

    #[test]
    fn quota_tree_presence_means_qgroups_are_enabled() {
        let without = discover_items(vec![root(FS_TREE_OBJECTID, 10, 0, 1)]);
        assert!(!without.quota_enabled);
        let with = discover_items(vec![root(FS_TREE_OBJECTID, 10, 0, 1), root(8, 10, 0, 1)]);
        assert!(with.quota_enabled);
    }

    #[test]
    fn keeps_the_creation_time_of_each_subvolume() {
        let (key, mut item) = root(257, 30, 1, 1);
        let mut parsed = parse_root_item(&item).unwrap();
        parsed.otime = Some(1_780_000_000);
        item = encode_root_item(&parsed, true);
        let set = discover_items(vec![root(FS_TREE_OBJECTID, 10, 0, 1), (key, item)]);
        assert_eq!(set.get(257).unwrap().otime, Some(1_780_000_000));
    }

    #[test]
    fn zero_refs_marks_pending_deletion() {
        let set = discover_items(vec![root(FS_TREE_OBJECTID, 10, 0, 1), root(258, 40, 0, 0)]);
        assert!(set.get(258).unwrap().pending_deletion);
        assert!(!set.get(5).unwrap().pending_deletion);
    }

    fn subvolume(id: u64, uuid: Option<u8>, parent_uuid: Option<u8>) -> Subvolume {
        Subvolume {
            id,
            parent: None,
            name: Vec::new(),
            generation: 1,
            readonly: false,
            pending_deletion: false,
            uuid: uuid.map(|u| [u; 16]),
            parent_uuid: parent_uuid.map(|u| [u; 16]),
            last_snapshot: 0,
            otime: None,
        }
    }

    fn set(subvolumes: Vec<Subvolume>) -> SubvolumeSet {
        SubvolumeSet {
            subvolumes,
            ..SubvolumeSet::default()
        }
    }

    fn readonly(id: u64, uuid: u8, parent_uuid: u8) -> Subvolume {
        Subvolume {
            readonly: true,
            ..subvolume(id, Some(uuid), Some(parent_uuid))
        }
    }

    #[test]
    fn snapshots_of_a_share_follow_the_parent_uuid_chain() {
        let mut pending = readonly(262, 7, 1);
        pending.pending_deletion = true;
        let writable_clone = subvolume(263, Some(8), Some(1));
        let set = set(vec![
            subvolume(256, Some(1), None),
            readonly(257, 2, 1),
            readonly(258, 3, 2),
            subvolume(259, Some(4), None),
            readonly(260, 5, 4),
            readonly(261, 6, 99),
            pending,
            writable_clone,
        ]);
        let of = set.share_snapshots(256);
        assert_eq!(of.snapshots, vec![257, 258]);
        assert_eq!(of.pending, vec![262]);
        assert_eq!(set.share_snapshots(259).snapshots, vec![260]);
        assert!(set.share_snapshots(261).snapshots.is_empty());
    }

    /// DSM keeps a share's snapshots in `@sharesnap/<share>/`. A restored
    /// share is a writable clone of a snapshot whose own source is gone,
    /// so the snapshot's `parent_uuid` names nothing on the volume. The
    /// volume is mounted from its `@syno` subvolume, so the shares and
    /// `@sharesnap` are its root-directory children, not the FS tree's.
    fn restored_share() -> SubvolumeSet {
        let syno = Subvolume {
            name: b"@syno".to_vec(),
            parent: Some(Link {
                parent_id: FS_TREE_OBJECTID,
                dirid: FIRST_FREE_OBJECTID,
            }),
            ..subvolume(257, Some(57), None)
        };
        let at_volume_root = |id: u64, name: &[u8]| Subvolume {
            name: name.to_vec(),
            parent: Some(Link {
                parent_id: syno.id,
                dirid: FIRST_FREE_OBJECTID,
            }),
            ..subvolume(id, Some(id as u8), None)
        };
        let sharesnap = at_volume_root(284, b"@sharesnap");
        let folder = Subvolume {
            name: b"Master".to_vec(),
            parent: Some(Link {
                parent_id: sharesnap.id,
                dirid: FIRST_FREE_OBJECTID,
            }),
            ..subvolume(288, Some(32), None)
        };
        let snapshot = Subvolume {
            name: b"GMT+08-2026.09.29-12.00.00".to_vec(),
            parent: Some(Link {
                parent_id: folder.id,
                dirid: FIRST_FREE_OBJECTID,
            }),
            ..readonly(289, 11, 0x16)
        };
        // The restored share is a writable clone of the snapshot.
        let live = Subvolume {
            parent_uuid: Some([11; 16]),
            ..at_volume_root(285, b"Master")
        };
        let mut names = vec![
            subvolume(FS_TREE_OBJECTID, Some(5), None),
            syno,
            sharesnap,
            live,
            folder,
            snapshot,
        ];
        names.sort_unstable_by_key(|s| s.id);
        set(names)
    }

    #[test]
    fn a_restored_shares_snapshots_are_found_in_its_sharesnap_folder() {
        assert_eq!(restored_share().share_snapshots(285).snapshots, vec![289]);
    }

    #[test]
    fn a_sharesnap_folder_of_another_share_is_not_a_snapshot_of_this_one() {
        let mut other = restored_share();
        other.subvolumes.iter_mut().for_each(|s| {
            if s.id == 285 {
                s.name = b"Photos".to_vec();
            }
        });
        assert!(other.share_snapshots(285).snapshots.is_empty());
    }

    #[test]
    fn the_sharesnap_folder_is_not_itself_a_share_with_snapshots() {
        assert!(restored_share().share_snapshots(288).snapshots.is_empty());
    }

    #[test]
    fn the_sharing_family_is_the_uuid_relatives_plus_hidden_relations() {
        let base = vec![
            subvolume(256, Some(1), None),
            readonly(257, 2, 1),
            subvolume(259, Some(4), None),
        ];
        let plain = set(base.clone());
        assert_eq!(plain.sharing_family(256), BTreeSet::from([256, 257]));
        assert_eq!(plain.sharing_family(259), BTreeSet::from([259]));
        let mut hidden = base.clone();
        hidden.push(readonly(261, 6, 99));
        let hidden = set(hidden);
        assert_eq!(hidden.sharing_family(259), BTreeSet::from([259, 261]));
        let mut no_uuid = base;
        no_uuid.push(subvolume(262, None, None));
        let no_uuid = set(no_uuid);
        assert_eq!(no_uuid.sharing_family(262).len(), 4);
    }

    #[test]
    fn a_never_snapshotted_subvolume_shares_no_leaves() {
        let set = set(vec![
            subvolume(5, Some(5), None),
            subvolume(256, Some(1), None),
        ]);
        assert_eq!(set.leaf_sharing(256), LeafSharing::default());
    }

    #[test]
    fn leaves_up_to_the_last_snapshot_may_be_shared_with_the_snapshot() {
        let mut data = subvolume(256, Some(1), None);
        data.last_snapshot = 40;
        let mut snap = subvolume(257, Some(2), Some(1));
        snap.readonly = true;
        snap.last_snapshot = 40;
        let set = set(vec![subvolume(5, Some(5), None), data, snap]);
        let shared = LeafSharing {
            up_to_generation: 40,
            with_snapshot: true,
        };
        assert_eq!(set.leaf_sharing(256), shared);
        assert_eq!(set.leaf_sharing(257), shared);
    }

    #[test]
    fn writable_siblings_and_pending_deletions_are_relatives() {
        let mut a = subvolume(258, Some(3), Some(9));
        a.last_snapshot = 50;
        let b = subvolume(259, Some(4), Some(9));
        let mut deleted = subvolume(260, Some(6), None);
        deleted.pending_deletion = true;
        let mut clone = subvolume(261, Some(7), Some(6));
        clone.last_snapshot = 60;
        let set = set(vec![subvolume(5, Some(5), None), a, b, deleted, clone]);
        let expect = |g| LeafSharing {
            up_to_generation: g,
            with_snapshot: false,
        };
        assert_eq!(set.leaf_sharing(258), expect(50));
        assert_eq!(set.leaf_sharing(261), expect(60));
    }

    fn snapshotted(id: u64, uuid: Option<u8>, parent_uuid: Option<u8>) -> Subvolume {
        Subvolume {
            last_snapshot: 40,
            ..subvolume(id, uuid, parent_uuid)
        }
    }

    const UNSHARED: LeafSharing = LeafSharing {
        up_to_generation: 0,
        with_snapshot: false,
    };

    const MAY_SHARE: LeafSharing = LeafSharing {
        up_to_generation: 40,
        with_snapshot: false,
    };

    /// Once the cleaner has dropped every snapshot, no root item points
    /// at the subvolume, so its old leaves are its own again.
    #[test]
    fn old_leaves_are_unshared_once_no_snapshot_remains() {
        let set = set(vec![
            subvolume(5, Some(5), None),
            snapshotted(256, Some(1), None),
        ]);
        assert_eq!(set.leaf_sharing(256), UNSHARED);
        assert_eq!(set.leaf_sharing(999), LeafSharing::default());
    }

    #[test]
    fn a_snapshot_awaiting_the_cleaner_still_shares() {
        let mut deleted = snapshotted(257, Some(2), Some(1));
        deleted.pending_deletion = true;
        let set = set(vec![snapshotted(256, Some(1), None), deleted]);
        assert_eq!(set.leaf_sharing(256), MAY_SHARE);
    }

    /// A snapshot of a snapshot shares through its source, and a
    /// writable snapshot shares like a read-only one.
    #[test]
    fn any_relative_in_the_uuid_chain_may_share() {
        let set = set(vec![
            snapshotted(256, Some(1), None),
            snapshotted(257, Some(2), Some(1)),
            snapshotted(258, Some(3), Some(2)),
        ]);
        assert_eq!(set.leaf_sharing(256), MAY_SHARE);
        assert_eq!(set.leaf_sharing(258), MAY_SHARE);
    }

    /// A deleted intermediate snapshot leaves its own snapshots pointing
    /// at a missing uuid: they may descend from any subvolume.
    #[test]
    fn a_parent_uuid_naming_no_subvolume_hides_the_relation() {
        let set = set(vec![
            snapshotted(256, Some(1), None),
            snapshotted(258, Some(3), Some(2)),
        ]);
        assert_eq!(set.leaf_sharing(256), MAY_SHARE);
    }

    /// Without a uuid the subvolume's snapshots cannot name it.
    #[test]
    fn subvolumes_without_a_uuid_stay_conservative() {
        let set1 = set(vec![snapshotted(256, None, None)]);
        assert_eq!(set1.leaf_sharing(256), MAY_SHARE);
        let set2 = set(vec![
            snapshotted(5, None, None),
            snapshotted(256, Some(1), None),
        ]);
        assert_eq!(
            set2.leaf_sharing(256),
            MAY_SHARE,
            "a snapshot of 5 names no parent"
        );
        let set3 = set(vec![
            subvolume(5, None, None),
            snapshotted(256, Some(1), None),
        ]);
        assert_eq!(set3.leaf_sharing(256), UNSHARED, "5 was never snapshotted");
    }

    #[test]
    fn relocation_trees_name_the_roots_they_relocate() {
        let mut reloc = root(TREE_RELOC_OBJECTID, 60, 0, 1);
        reloc.0.offset = 256;
        let items = vec![root(FS_TREE_OBJECTID, 10, 0, 1), root(256, 20, 0, 1), reloc];
        let mut source = FakeSource::new(items, 2);
        let found = relocation_trees(&mut source, &mut NoStop).unwrap();
        assert_eq!(found, BTreeSet::from([256]));
        let none = vec![root(FS_TREE_OBJECTID, 10, 0, 1)];
        let found = relocation_trees(&mut FakeSource::new(none, 2), &mut NoStop).unwrap();
        assert!(found.is_empty());
    }

    /// A relocation tree is a copy of the root: it shares its blocks.
    #[test]
    fn a_root_being_relocated_may_share() {
        let mut set = set(vec![snapshotted(256, Some(1), None)]);
        set.relocating = BTreeSet::from([256]);
        assert_eq!(set.leaf_sharing(256), MAY_SHARE);
    }

    #[test]
    fn snapshots_with_offset_keys_are_found() {
        let mut snapshot = root(259, 50, 1, 1);
        snapshot.0.offset = 44;
        let set = discover_items(vec![root(FS_TREE_OBJECTID, 10, 0, 1), snapshot]);
        assert!(set.get(259).is_some());
    }
}
