//! The materialized scope of one filesystem (SPEC 5.1, 7 "Scan scope"):
//! the selected subtrees of one or more subvolume namespaces joined into
//! one node arena under a virtual root, with per-node metrics.
//!
//! Metrics count each inode's logical size and each physical extent once
//! per subtree (SPEC 3): hard links and reflinks never double-count.

use crate::btrfs::extent_refs::ExtentRefs;
use crate::btrfs::keys::FIRST_FREE_OBJECTID;
use crate::btrfs::subvolume::SubvolumeSet;
use crate::dsm::{self, Depth};
use crate::model::accounting::{
    AccountingSummary, Blockers, Held, HeldClass, HeldUse, Holder, Judgement, RootClass, Verdict,
    judge,
};
use crate::model::extent::ExtentUse;
use crate::model::namespace::{
    EntryTarget, FileKind, Inode, ModelCounts, Namespace, NamespaceError, RecycleEntryCtime,
    SpilledRefs,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;

/// Parent of every selected root; its metrics cover the whole target set.
pub const VIRTUAL_ROOT: u32 = 0;
/// Node without an inode: the virtual root, or a subvolume boundary that
/// is a placeholder or not attached yet.
pub const NO_INODE: u32 = u32::MAX;
const NO_PARENT: u32 = u32::MAX;
/// A node without a value of its own in a directory-only [`Spread`].
const NO_SLOT: u32 = u32::MAX;

mod inode_allocation;
mod outside;
mod stored;
pub use inode_allocation::{AllocationItem, AllocationOverview, InodeAllocation, InodeTree};

pub(crate) use child_flags as inode_child_flags;
pub(crate) use marker as inode_marker;
pub use outside::{Excluded, Outside};
use outside::{Freed, PackageExtent, Packages};
pub use stored::Corrupt;
/// Lower-file names of an eCryptfs share (SPEC 4.6).
const ECRYPTFS_NAME_PREFIX: &[u8] = b"ECRYPTFS_FNEK_ENCRYPTED.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeFlags(u8);

impl NodeFlags {
    /// DSM-managed: counted, hidden from default rankings (SPEC 4.6).
    pub const SYSTEM: NodeFlags = NodeFlags(1);
    /// Inside a `#recycle` bin: an ordinary cleanup candidate.
    pub const RECYCLE: NodeFlags = NodeFlags(1 << 1);
    /// eCryptfs lower directory: names are ciphertext and never shown.
    pub const ENCRYPTED: NodeFlags = NodeFlags(1 << 2);
    /// Root of a nested subvolume (a boundary).
    pub const SUBVOLUME_ROOT: NodeFlags = NodeFlags(1 << 3);
    /// This tool's own state directory.
    pub const OWN_STATE: NodeFlags = NodeFlags(1 << 4);
    /// Empty stand-in for a nested subvolume a snapshot did not carry.
    pub const PLACEHOLDER: NodeFlags = NodeFlags(1 << 5);
    /// Read-only snapshot boundary whose tree was not read.
    pub const SNAPSHOT: NodeFlags = NodeFlags(1 << 6);
    /// Package-managed data starts here (SPEC 5.7): a guarded service
    /// path, or a folder holding a package marker.
    pub const PACKAGE: NodeFlags = NodeFlags(1 << 7);
    const INHERITED: NodeFlags = NodeFlags(1 | 1 << 1 | 1 << 4);

    pub fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits(bits: u8) -> Self {
        NodeFlags(bits)
    }

    pub fn contains(self, other: NodeFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn with(self, other: NodeFlags) -> Self {
        NodeFlags(self.0 | other.0)
    }

    fn inherited(self) -> Self {
        NodeFlags(self.0 & Self::INHERITED.0)
    }
}

/// Whether a root is a volume mount root, where `@`-prefixed directories
/// are DSM service roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Volume,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub parent: u32,
    name_offset: u32,
    name_len: u32,
    /// Index into the inode table, or `NO_INODE`.
    pub inode: u32,
    pub kind: FileKind,
    pub flags: NodeFlags,
}

/// One inode of the scope; hard links share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeInode {
    pub tree: u64,
    pub objectid: u64,
    pub size: u64,
    pub nlink: u32,
    pub kind: FileKind,
    pub inline_bytes: u64,
}

impl TreeInode {
    /// Apparent data size: regular files only.
    fn logical(&self) -> u64 {
        if self.kind == FileKind::Regular {
            self.size
        } else {
            0
        }
    }
}

/// Directory depth below a root beyond which entries are no longer named
/// individually; their space is attributed to the directory at this depth.
pub const MAX_DEPTH: usize = 256;

/// Metadata contradictions skipped while materializing (SPEC 13, 22.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Anomalies {
    pub dangling_entries: u64,
    pub cycles: u64,
    pub unsupported_locations: u64,
    pub missing_subvolume_roots: u64,
}

impl Anomalies {
    pub fn add(&mut self, other: &Anomalies) {
        self.dangling_entries += other.dangling_entries;
        self.cycles += other.cycles;
        self.unsupported_locations += other.unsupported_locations;
        self.missing_subvolume_roots += other.missing_subvolume_roots;
    }

    pub fn total(&self) -> u64 {
        self.dangling_entries
            + self.cycles
            + self.unsupported_locations
            + self.missing_subvolume_roots
    }
}

/// Directories recognized by identity `(tree, inode)` rather than by name.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    /// Lower directories of mounted eCryptfs shares.
    pub encrypted: BTreeSet<(u64, u64)>,
    pub own_state: BTreeSet<(u64, u64)>,
}

/// A linked nested subvolume whose tree still has to be attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    pub subvolume: u64,
    pub node: u32,
}

/// Subtree totals of one node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Metrics {
    /// Apparent size of distinct regular-file inodes.
    pub logical: u64,
    /// Allocated bytes of distinct whole physical extents.
    pub physical: u64,
    /// Distinct physical extents.
    pub extents: u64,
    /// Data bytes stored inline in metadata.
    pub inline: u64,
    /// Nodes in the subtree, including the node itself.
    pub entries: u64,
    /// Regular-file names in the subtree.
    pub files: u64,
    /// Allocated bytes of whole extents that deleting the subtree frees
    /// (resolved references only, SPEC 3).
    #[serde(default)]
    pub reclaimable: u64,
    /// Allocated bytes of extents whose references are unresolved.
    #[serde(default)]
    pub unknown: u64,
    /// Parts of reclaimable extents that no file range references.
    #[serde(default)]
    pub pinned: u64,
}

impl Metrics {
    /// Footprint bytes proven to have references outside the subtree.
    pub fn shared(&self) -> u64 {
        self.physical
            .saturating_sub(self.reclaimable.saturating_add(self.unknown))
    }

    pub fn add(&mut self, other: &Metrics) {
        self.logical = self.logical.wrapping_add(other.logical);
        self.physical = self.physical.wrapping_add(other.physical);
        self.extents = self.extents.wrapping_add(other.extents);
        self.inline = self.inline.wrapping_add(other.inline);
        self.entries = self.entries.wrapping_add(other.entries);
        self.files = self.files.wrapping_add(other.files);
        self.reclaimable = self.reclaimable.wrapping_add(other.reclaimable);
        self.unknown = self.unknown.wrapping_add(other.unknown);
        self.pinned = self.pinned.wrapping_add(other.pinned);
    }

    pub fn sub(&mut self, other: &Metrics) {
        self.logical = self.logical.wrapping_sub(other.logical);
        self.physical = self.physical.wrapping_sub(other.physical);
        self.extents = self.extents.wrapping_sub(other.extents);
        self.inline = self.inline.wrapping_sub(other.inline);
        self.entries = self.entries.wrapping_sub(other.entries);
        self.files = self.files.wrapping_sub(other.files);
        self.reclaimable = self.reclaimable.wrapping_sub(other.reclaimable);
        self.unknown = self.unknown.wrapping_sub(other.unknown);
        self.pinned = self.pinned.wrapping_sub(other.pinned);
    }
}

/// The namespace being materialized and what classifies its entries.
struct Source<'a> {
    ns: &'a Namespace,
    subvolumes: &'a SubvolumeSet,
    marks: &'a Marks,
}

impl Source<'_> {
    fn identity(&self, ino: u64) -> (u64, u64) {
        (self.ns.tree_id(), ino)
    }

    fn identity_flags(&self, ino: u64) -> NodeFlags {
        if self.marks.own_state.contains(&self.identity(ino)) {
            NodeFlags::OWN_STATE
        } else {
            NodeFlags::default()
        }
    }

    fn is_encrypted(&self, dir: u64) -> bool {
        self.marks.encrypted.contains(&self.identity(dir))
            || self
                .ns
                .child_entries(dir)
                .any(|(name, _)| name.starts_with(ECRYPTFS_NAME_PREFIX))
    }
}

pub(crate) fn child_flags(parent: NodeFlags, name: &[u8], at_volume_root: bool) -> NodeFlags {
    let mut flags = parent.inherited();
    if name == b"@eaDir" || name == b"#snapshot" || (at_volume_root && name.starts_with(b"@")) {
        flags = flags.with(NodeFlags::SYSTEM);
    }
    if name == b"#recycle" {
        flags = flags.with(NodeFlags::RECYCLE);
    }
    let depth = if at_volume_root {
        Depth::VolumeRoot
    } else {
        Depth::Below
    };
    let guarded = dsm::service(name, depth).is_some_and(|s| s.guarded);
    if guarded && marker(name, at_volume_root).is_none() {
        flags = flags.with(NodeFlags::PACKAGE);
    }
    flags
}

/// The marker an entry `name` is, when it marks the folder holding it as
/// package data; the volume root is not a share, so nothing marks it.
pub(crate) fn marker(name: &[u8], at_volume_root: bool) -> Option<&'static [u8]> {
    dsm::markers().find(|m| *m == name && !at_volume_root)
}

fn path_has_recycle_component(path: &[u8]) -> bool {
    path.split(|&byte| byte == b'/')
        .any(|part| part == b"#recycle")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeTree {
    nodes: Vec<Node>,
    names: Vec<u8>,
    inodes: Vec<TreeInode>,
    /// Ctimes for names inside Recycle Bins; transferred into the unit summary.
    recycle_ctimes: Vec<RecycleEntryCtime>,
    /// Inodes with several links, so later names find the same entry.
    linked_inodes: HashMap<(u64, u64), u32>,
    /// `(inode, node)`, one per name: a collapsed directory holding
    /// several names of an inode appears once for each.
    links: Vec<(u32, u32)>,
    uses: Vec<ExtentUse>,
    roots: Vec<u32>,
    /// Folders a package marker made package data, with the marker.
    markers: HashMap<u32, &'static [u8]>,
    anomalies: Anomalies,
}

impl Default for ScopeTree {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeTree {
    /// An upper bound of the bytes expanding namespace parts of sizes `c`
    /// adds: every entry, inode (with its lookup for spilled refs) and
    /// held file extent.
    pub fn expansion_bytes(c: ModelCounts) -> u64 {
        let entry = (size_of::<Node>() + size_of::<(u32, u32)>()) as u64;
        c.entries * entry
            + c.name_bytes
            + c.inodes * (size_of::<TreeInode>() + size_of::<(u64, u32)>()) as u64
            + c.recycle_entries * size_of::<RecycleEntryCtime>() as u64
            + c.extents * size_of::<ExtentUse>() as u64
    }

    pub fn new() -> Self {
        ScopeTree {
            nodes: vec![Node {
                parent: NO_PARENT,
                name_offset: 0,
                name_len: 0,
                inode: NO_INODE,
                kind: FileKind::Directory,
                flags: NodeFlags::default(),
            }],
            names: Vec::new(),
            inodes: Vec::new(),
            recycle_ctimes: Vec::new(),
            linked_inodes: HashMap::new(),
            links: Vec::new(),
            uses: Vec::new(),
            roots: Vec::new(),
            markers: HashMap::new(),
            anomalies: Anomalies::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    pub fn node(&self, node: u32) -> &Node {
        &self.nodes[node as usize]
    }

    pub fn inode(&self, inode: u32) -> Option<&TreeInode> {
        self.inodes.get(inode as usize)
    }

    /// Entry inode ctimes captured below each `#recycle` path.
    pub fn recycle_ctimes(&self) -> &[RecycleEntryCtime] {
        &self.recycle_ctimes
    }

    pub fn take_recycle_ctimes(&mut self) -> Vec<RecycleEntryCtime> {
        let mut entries = std::mem::take(&mut self.recycle_ctimes);
        entries.sort_unstable_by(|a, b| {
            (a.tree, a.inode)
                .cmp(&(b.tree, b.inode))
                .then_with(|| b.ctime.cmp(&a.ctime))
        });
        entries.dedup_by(|a, b| a.tree == b.tree && a.inode == b.inode);
        entries
    }

    pub fn inode_count(&self) -> usize {
        self.inodes.len()
    }

    pub fn roots(&self) -> Vec<u32> {
        self.roots.clone()
    }

    pub fn anomalies(&self) -> &Anomalies {
        &self.anomalies
    }

    /// Nodes whose contents are hidden eCryptfs lower data.
    pub fn encrypted(&self) -> Vec<u32> {
        (0..self.nodes.len() as u32)
            .filter(|&n| self.node(n).flags.contains(NodeFlags::ENCRYPTED))
            .collect()
    }

    /// The marker that made folder `node` package data.
    pub fn marker(&self, node: u32) -> Option<&'static [u8]> {
        self.markers.get(&node).copied()
    }

    pub fn name(&self, node: u32) -> &[u8] {
        let n = self.node(node);
        let start = n.name_offset as usize;
        &self.names[start..start + n.name_len as usize]
    }

    /// Full path: the root's name, then `/`-joined entry names.
    pub fn path(&self, node: u32) -> Vec<u8> {
        let mut chain = Vec::new();
        let mut at = node;
        while at != VIRTUAL_ROOT && at != NO_PARENT {
            chain.push(at);
            at = self.node(at).parent;
        }
        let mut path = Vec::new();
        for (i, n) in chain.iter().rev().enumerate() {
            if i > 0 {
                path.push(b'/');
            }
            path.extend_from_slice(self.name(*n));
        }
        path
    }

    /// Adds the subtree of inode `root_ino` of `ns`, named `name`, under
    /// the virtual root. Returns the linked nested subvolumes inside it.
    pub fn add_root(
        &mut self,
        ns: &Namespace,
        root_ino: u64,
        name: &[u8],
        kind: RootKind,
        subvolumes: &SubvolumeSet,
        marks: &Marks,
    ) -> Result<Vec<Pending>, NamespaceError> {
        let src = Source {
            ns,
            subvolumes,
            marks,
        };
        let inode = ns
            .inode(root_ino)
            .ok_or(NamespaceError::MissingInode(root_ino))?;
        let file_kind = FileKind::from_mode(inode.mode);
        let mut flags = src.identity_flags(root_ino);
        if path_has_recycle_component(name) {
            flags = flags.with(NodeFlags::RECYCLE);
        }
        self.remember_recycle_ctime(ns.tree_id(), inode, flags);
        let index = self.intern(ns, inode);
        let node = self.push_node(VIRTUAL_ROOT, name, file_kind, flags, index);
        self.roots.push(node);
        let mut pending = Vec::new();
        if file_kind == FileKind::Directory {
            self.expand(&src, root_ino, node, kind == RootKind::Volume, &mut pending);
        }
        Ok(pending)
    }

    /// Attaches the tree of a pending nested subvolume under its boundary:
    /// the one-tree reference that split accounting is tested against.
    #[cfg(test)]
    pub fn attach(
        &mut self,
        pending: &Pending,
        ns: &Namespace,
        subvolumes: &SubvolumeSet,
        marks: &Marks,
    ) -> Vec<Pending> {
        let src = Source {
            ns,
            subvolumes,
            marks,
        };
        let mut more = Vec::new();
        let flags = self.nodes[pending.node as usize].flags;
        let Some(index) = self.subvolume_root(ns, flags) else {
            return more;
        };
        self.nodes[pending.node as usize].inode = index;
        self.links.push((index, pending.node));
        self.expand(&src, FIRST_FREE_OBJECTID, pending.node, false, &mut more);
        more
    }

    /// Adds the tree of a nested subvolume as a root named `name`, with
    /// its boundary's `flags`: what `attach` puts below that boundary.
    pub fn add_subvolume_root(
        &mut self,
        ns: &Namespace,
        name: &[u8],
        flags: NodeFlags,
        subvolumes: &SubvolumeSet,
        marks: &Marks,
    ) -> Vec<Pending> {
        let src = Source {
            ns,
            subvolumes,
            marks,
        };
        let mut more = Vec::new();
        let Some(index) = self.subvolume_root(ns, flags) else {
            return more;
        };
        let node = self.push_node(VIRTUAL_ROOT, name, FileKind::Directory, flags, index);
        self.roots.push(node);
        self.expand(&src, FIRST_FREE_OBJECTID, node, false, &mut more);
        more
    }

    /// The scope's inode of the root directory of `ns`, if it has one.
    fn subvolume_root(&mut self, ns: &Namespace, flags: NodeFlags) -> Option<u32> {
        let Some(inode) = ns.inode(FIRST_FREE_OBJECTID) else {
            self.anomalies.missing_subvolume_roots += 1;
            return None;
        };
        self.remember_recycle_ctime(ns.tree_id(), inode, flags);
        Some(self.intern(ns, inode))
    }

    fn remember_recycle_ctime(&mut self, tree: u64, inode: &Inode, flags: NodeFlags) {
        if flags.contains(NodeFlags::RECYCLE) {
            self.recycle_ctimes.push(RecycleEntryCtime {
                tree,
                inode: inode.objectid,
                ctime: inode.ctime,
            });
        }
    }

    /// Adds `flag` to `node`.
    pub fn mark(&mut self, node: u32, flag: NodeFlags) {
        let flags = self.nodes[node as usize].flags.with(flag);
        self.nodes[node as usize].flags = flags;
    }

    fn push_node(
        &mut self,
        parent: u32,
        name: &[u8],
        kind: FileKind,
        flags: NodeFlags,
        inode: u32,
    ) -> u32 {
        let node = self.nodes.len() as u32;
        self.nodes.push(Node {
            parent,
            name_offset: self.names.len() as u32,
            name_len: name.len() as u32,
            inode,
            kind,
            flags,
        });
        self.names.extend_from_slice(name);
        if inode != NO_INODE {
            self.links.push((inode, node));
        }
        node
    }

    /// The scope's inode for `inode` of `ns`, adding it and its extent
    /// uses on first sight.
    fn intern(&mut self, ns: &Namespace, inode: &Inode) -> u32 {
        let identity = (ns.tree_id(), inode.objectid);
        if inode.nlink > 1
            && let Some(&index) = self.linked_inodes.get(&identity)
        {
            return index;
        }
        let index = self.inodes.len() as u32;
        self.inodes.push(TreeInode {
            tree: identity.0,
            objectid: inode.objectid,
            size: inode.size,
            nlink: inode.nlink,
            kind: FileKind::from_mode(inode.mode),
            inline_bytes: ns.inline_bytes(inode.objectid),
        });
        self.uses.extend(
            ns.extents(inode.objectid)
                .iter()
                .map(|r| ExtentUse::new(r, index)),
        );
        if inode.nlink > 1 {
            self.linked_inodes.insert(identity, index);
        }
        index
    }

    /// Depth-first materialization of the entries below directory `dir`.
    fn expand(
        &mut self,
        src: &Source<'_>,
        dir: u64,
        node: u32,
        at_volume_root: bool,
        pending: &mut Vec<Pending>,
    ) {
        let mut visited = HashSet::from([dir]);
        let mut stack = vec![(dir, node, at_volume_root, 0usize)];
        while let Some((dir, node, at_root, depth)) = stack.pop() {
            if src.is_encrypted(dir) {
                let flags = self.node(node).flags.with(NodeFlags::ENCRYPTED);
                self.nodes[node as usize].flags = flags;
                self.collapse(src, dir, node, &mut visited);
                continue;
            }
            if depth >= MAX_DEPTH {
                // Deeper entries stay accounted to this directory but are not
                // named, so walks and path lengths stay bounded.
                self.collapse(src, dir, node, &mut visited);
                continue;
            }
            let parent_flags = self.node(node).flags;
            for (name, target) in src.ns.child_entries(dir) {
                if let Some(marker) = marker(name, at_root) {
                    self.mark(node, NodeFlags::PACKAGE);
                    self.markers.insert(node, marker);
                }
                let flags = child_flags(parent_flags, name, at_root);
                match target {
                    EntryTarget::Inode(ino) => {
                        let Some(inode) = src.ns.inode(ino) else {
                            // The scan replaces its own state files while it
                            // reads them (SPEC 22.4): no gap in user data.
                            if !parent_flags.contains(NodeFlags::OWN_STATE) {
                                self.anomalies.dangling_entries += 1;
                            }
                            continue;
                        };
                        let kind = FileKind::from_mode(inode.mode);
                        if kind == FileKind::Directory && !visited.insert(ino) {
                            self.anomalies.cycles += 1;
                            continue;
                        }
                        let flags = flags.with(src.identity_flags(ino));
                        self.remember_recycle_ctime(src.ns.tree_id(), inode, flags);
                        let index = self.intern(src.ns, inode);
                        let child = self.push_node(node, name, kind, flags, index);
                        if kind == FileKind::Directory {
                            stack.push((ino, child, false, depth + 1));
                        }
                    }
                    EntryTarget::Subvolume(id) => {
                        let linked = src.ns.links_subvolume(dir, name, id, src.subvolumes);
                        let boundary = if linked {
                            NodeFlags::SUBVOLUME_ROOT
                        } else {
                            NodeFlags::PLACEHOLDER
                        };
                        let child = self.push_node(
                            node,
                            name,
                            FileKind::Directory,
                            flags.with(boundary),
                            NO_INODE,
                        );
                        if linked {
                            pending.push(Pending {
                                subvolume: id,
                                node: child,
                            });
                        }
                    }
                    EntryTarget::Other(_) => self.anomalies.unsupported_locations += 1,
                }
            }
        }
    }

    /// Attributes every inode below `dir` to `node` without naming them.
    fn collapse(&mut self, src: &Source<'_>, dir: u64, node: u32, visited: &mut HashSet<u64>) {
        let mut stack = vec![dir];
        while let Some(dir) = stack.pop() {
            for (_, target) in src.ns.child_entries(dir) {
                let EntryTarget::Inode(ino) = target else {
                    continue;
                };
                let Some(inode) = src.ns.inode(ino) else {
                    self.anomalies.dangling_entries += 1;
                    continue;
                };
                let is_dir = FileKind::from_mode(inode.mode) == FileKind::Directory;
                if is_dir && !visited.insert(ino) {
                    self.anomalies.cycles += 1;
                    continue;
                }
                let index = self.intern(src.ns, inode);
                self.links.push((index, node));
                if is_dir {
                    stack.push(ino);
                }
            }
        }
    }

    /// `(node, depth)` of every node below the virtual root: each root in
    /// order added, then its subtree depth-first with siblings by name.
    pub fn preorder(&self) -> Vec<(u32, u32)> {
        let (start, children) = self.child_index();
        let mut order = Vec::with_capacity(self.nodes.len() - 1);
        let mut stack: Vec<(u32, u32)> = self.roots.iter().rev().map(|&r| (r, 0)).collect();
        while let Some((node, depth)) = stack.pop() {
            order.push((node, depth));
            let kids = &children[start[node as usize]..start[node as usize + 1]];
            stack.extend(kids.iter().rev().map(|&c| (c, depth + 1)));
        }
        order
    }

    /// Children of node `n`, sorted by name, are
    /// `children[start[n]..start[n + 1]]` (the virtual root has none).
    fn child_index(&self) -> (Vec<usize>, Vec<u32>) {
        let mut start = vec![0usize; self.nodes.len() + 1];
        for node in self.nodes.iter().skip(1) {
            if node.parent != VIRTUAL_ROOT {
                start[node.parent as usize + 1] += 1;
            }
        }
        for i in 1..start.len() {
            start[i] += start[i - 1];
        }
        let mut fill = start.clone();
        let mut children = vec![0u32; start[self.nodes.len()]];
        for (i, node) in self.nodes.iter().enumerate().skip(1) {
            if node.parent != VIRTUAL_ROOT {
                let slot = &mut fill[node.parent as usize];
                children[*slot] = i as u32;
                *slot += 1;
            }
        }
        for n in 0..self.nodes.len() {
            children[start[n]..start[n + 1]]
                .sort_unstable_by(|&a, &b| self.name(a).cmp(self.name(b)));
        }
        (start, children)
    }

    /// Orders extent uses and inode links for accounting.
    pub fn prepare(&mut self) {
        self.uses.sort_unstable();
        self.links.sort_unstable();
    }

    /// Subtree metrics of every node without reference accounting.
    #[cfg(test)]
    pub fn aggregate(&self) -> Vec<Metrics> {
        let mut tree = self.clone();
        tree.prepare();
        let mut accountant = Accountant::new(&tree, None);
        for bytenr in accountant.candidates() {
            accountant.extent(bytenr, None);
        }
        accountant.finish().metrics
    }

    /// Every extent use with what classifying it needs, in extent order;
    /// the tree must be prepared.
    pub fn held_uses(&self) -> impl Iterator<Item = HeldUse> + '_ {
        self.uses.iter().map(|u| self.held(u))
    }

    /// The uses of `refs` whose inodes are in scope, in the order read;
    /// links must be prepared and `refs` come from the tree whose inodes
    /// the scope holds.
    pub fn spilled_uses(
        &self,
        mut refs: SpilledRefs,
        mut emit: impl FnMut(HeldUse) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut index: Vec<(u64, u32)> = (0..self.inodes.len() as u32)
            .map(|n| (self.inodes[n as usize].objectid, n))
            .collect();
        index.sort_unstable();
        while let Some(r) = refs.read()? {
            let start = index.partition_point(|&(objectid, _)| objectid < r.objectid);
            let holders = index[start..].iter().take_while(|e| e.0 == r.objectid);
            for &(_, inode) in holders {
                emit(self.held(&ExtentUse { inode, ..r.extent }))?;
            }
        }
        Ok(())
    }

    /// What classifying `u` needs; links must be prepared.
    fn held(&self, u: &ExtentUse) -> HeldUse {
        let inode = &self.inodes[u.inode as usize];
        let names = self.names_of(u.inode).len();
        HeldUse {
            extent: *u,
            holder: Holder {
                tree: inode.tree,
                objectid: inode.objectid,
                external_names: u64::from(inode.nlink) > names as u64,
                shared_leaf: u.flags & ExtentUse::SHARED_LEAF != 0,
            },
            several_names: names > 1,
        }
    }

    /// The nodes naming inode `inode`; links must be prepared.
    fn names_of(&self, inode: u32) -> &[(u32, u32)] {
        let start = self.links.partition_point(|l| l.0 < inode);
        let end = self.links.partition_point(|l| l.0 <= inode);
        &self.links[start..end]
    }
}

/// Distinct extent bytenrs of prepared uses, ascending.
#[derive(Clone)]
pub struct Bytenrs<'a> {
    uses: &'a [ExtentUse],
}

impl Iterator for Bytenrs<'_> {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        let first = self.uses.first()?.bytenr;
        let len = self.uses.partition_point(|u| u.bytenr == first);
        self.uses = &self.uses[len..];
        Some(first)
    }
}

/// Subtree metrics and blockers of every node, indexed like the nodes.
#[derive(Debug)]
pub struct Aggregate {
    pub metrics: Vec<Metrics>,
    pub blockers: Vec<Blockers>,
    /// Retained bytes per holder class of each directory.
    pub held: DirHeld,
    /// The part of each node's blockers that holds for its ancestors.
    pub external: Vec<Blockers>,
    pub summary: AccountingSummary,
    /// What each directory above package data includes of it.
    pub excluded: HashMap<u32, Excluded>,
    /// The roots that are package data themselves.
    pub package_roots: HashSet<u32>,
}

impl Aggregate {
    pub fn totals(&self) -> crate::report::NodeTotals<'_> {
        crate::report::NodeTotals {
            metrics: &self.metrics,
            blockers: &self.blockers,
            held: &self.held,
            excluded: &self.excluded,
        }
    }

    /// What the scope below `node` contributes where it is nested.
    pub fn nested(&self, node: u32) -> Nested {
        Nested {
            metrics: self.metrics[node as usize],
            external: self.external[node as usize],
            blockers: self.blockers[node as usize],
            held: self.held.get(node),
            package: self.package_roots.contains(&node),
            excluded: self.excluded.get(&node).copied(),
        }
    }

    /// The values of a directory above package data without that data.
    pub fn outside(&self, node: u32) -> Option<Outside> {
        let excluded = self.excluded.get(&node)?;
        Some(excluded.leave_out(self.metrics[node as usize], self.held.get(node)))
    }
}

/// Values kept for directories only (SPEC 5.4: about 6 B per entry).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirHeld {
    /// Each node's index into `values`, or `NO_SLOT`.
    slots: Vec<u32>,
    values: Vec<Held>,
}

impl DirHeld {
    /// The directory `node`'s retained bytes per class; zero for others.
    pub fn get(&self, node: u32) -> Held {
        match self.slots.get(node as usize) {
            Some(&slot) if slot != NO_SLOT => self.values[slot as usize],
            _ => Held::default(),
        }
    }
}

/// The totals of a scope nested at a boundary of another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Nested {
    pub metrics: Metrics,
    pub external: Blockers,
    pub blockers: Blockers,
    pub held: Held,
    /// The nested root is package data itself.
    pub package: bool,
    /// What the nested root includes of package data below it.
    pub excluded: Option<Excluded>,
}

/// Where the owning subtree of a reclaimable extent lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// In this scope: the lowest common ancestor of the holding nodes.
    Here,
    /// Above this scope, whose holders all lose a local blocker.
    Above,
}

/// One judged extent as it applies to one scope.
pub struct Attribution<'a> {
    pub judgement: Judgement,
    /// This scope's inodes holding the extent, one per use.
    pub inodes: &'a [u32],
    /// Boundary nodes of nested scopes holding the extent, whose subtree
    /// counts are added beforehand.
    pub children: &'a [u32],
    pub owner: Owner,
}

/// Attributes each extent, fed in bytenr order, to the scope (SPEC 9.1,
/// 11): footprint to every holding subtree once, reclaimable bytes to
/// the minimal subtree holding all its references, unknown bytes to
/// every holding subtree.
pub struct Accountant<'a> {
    tree: &'a ScopeTree,
    roots: Option<&'a dyn RootClass>,
    spread: Spread<'a, Metrics>,
    held: Spread<'a, Held>,
    /// Blockers that hold for every ancestor too.
    external: Vec<Blockers>,
    /// Blockers of nodes below a reclaimable extent's owning subtree.
    local: Vec<Blockers>,
    next: usize,
    nodes: Vec<u32>,
    /// The directories of `nodes`.
    dirs: Vec<u32>,
    summary: AccountingSummary,
    /// `None` while the scope holds no package data.
    packages: Option<Packages<'a>>,
}

impl<'a> Accountant<'a> {
    /// Bytes an accountant and its aggregate allocate for `tree`.
    pub fn bytes_needed(tree: &ScopeTree) -> u64 {
        let per_node = size_of::<Metrics>() + 2 * size_of::<u32>() + 3 * size_of::<Blockers>();
        let dirs = tree
            .nodes
            .iter()
            .filter(|n| n.kind == FileKind::Directory)
            .count()
            + 1;
        let per_dir = size_of::<Held>() + size_of::<u32>();
        let packages = outside::any_left_out(&tree.nodes);
        let per_node = per_node + usize::from(packages) * Packages::bytes_per_node();
        (tree.nodes.len() * per_node + dirs * per_dir) as u64
    }

    /// `tree` must be prepared; without `roots` only footprint counts.
    pub fn new(tree: &'a ScopeTree, roots: Option<&'a dyn RootClass>) -> Self {
        let mut spread = Spread::<Metrics>::new(&tree.nodes);
        for (i, node) in tree.nodes.iter().enumerate().skip(1) {
            let own = Metrics {
                entries: 1,
                files: u64::from(node.kind == FileKind::Regular),
                ..Metrics::default()
            };
            spread.acc[i].add(&own);
        }
        let mut nodes = Vec::new();
        let mut packages = Packages::of(&tree.nodes);
        for group in tree.links.chunk_by(|a, b| a.0 == b.0) {
            let inode = &tree.inodes[group[0].0 as usize];
            let value = Metrics {
                logical: inode.logical(),
                inline: inode.inline_bytes,
                ..Metrics::default()
            };
            nodes.clear();
            nodes.extend(group.iter().map(|&(_, node)| node));
            spread.add(&nodes, &value);
            if let Some(packages) = &mut packages {
                packages.inode(&nodes, &value);
            }
        }
        Accountant {
            tree,
            roots,
            spread,
            held: Spread::over_directories(&tree.nodes),
            external: vec![Blockers::default(); tree.nodes.len()],
            local: vec![Blockers::default(); tree.nodes.len()],
            next: 0,
            nodes,
            dirs: Vec::new(),
            summary: AccountingSummary::default(),
            packages,
        }
    }

    /// The extents to feed to `extent`, in order.
    pub fn candidates(&self) -> Bytenrs<'a> {
        Bytenrs {
            uses: &self.tree.uses[self.next..],
        }
    }

    /// Counts the scope nested at boundary `node` as that node's subtree;
    /// before any attribution.
    pub fn nest(&mut self, node: u32, nested: &Nested) {
        let n = node as usize;
        self.spread.acc[n] = nested.metrics;
        let slot = self.held.slot(node);
        self.held.acc[slot] = nested.held;
        self.external[n] = self.external[n].with(nested.external);
        self.local[n] = self.local[n].with(nested.blockers);
        let nodes = &self.tree.nodes;
        self.packages
            .get_or_insert_with(|| Packages::none(nodes))
            .nest(node, nested);
    }

    /// Attributes the next candidate, `bytenr`, given its references.
    pub fn extent(&mut self, bytenr: u64, refs: Option<&ExtentRefs>) {
        let tree = self.tree;
        let held: Vec<HeldUse> = self.own_uses(bytenr).iter().map(|u| tree.held(u)).collect();
        debug_assert!(!held.is_empty(), "extent {bytenr} fed out of order");
        let judgement = judge(&held, refs, self.roots);
        self.summary.count(&judgement);
        let inodes: Vec<u32> = held.iter().map(|u| u.extent.inode).collect();
        self.attribute(
            bytenr,
            &Attribution {
                judgement,
                inodes: &inodes,
                children: &[],
                owner: Owner::Here,
            },
        );
    }

    /// Attributes extent `bytenr`, judged across scopes, to this scope's
    /// holders of it and to the nested scopes in `at`. Extents come in
    /// bytenr order and none this scope holds may be skipped.
    pub fn attribute(&mut self, bytenr: u64, at: &Attribution) {
        self.next += self.own_uses(bytenr).len();
        self.apply(at);
    }

    /// This scope's uses of `bytenr`, which must not precede the next one.
    fn own_uses(&self, bytenr: u64) -> &'a [ExtentUse] {
        let uses = &self.tree.uses[self.next..];
        debug_assert!(uses.first().is_none_or(|u| u.bytenr >= bytenr));
        &uses[..uses.partition_point(|u| u.bytenr == bytenr)]
    }

    /// Attributes one judged extent to this scope's holders of it and to
    /// the nested scopes holding it.
    fn apply(&mut self, at: &Attribution) {
        self.holding_nodes(at.inodes, at.children);
        let disk_bytes = at.judgement.disk_bytes;
        let footprint = Metrics {
            physical: disk_bytes,
            extents: 1,
            ..Metrics::default()
        };
        self.spread_over_holders(at.children, &footprint);
        let mut freed = Freed::Above;
        match at.judgement.verdict {
            None => {}
            Some(Verdict::Reclaimable) => freed = self.reclaimable(&at.judgement, at),
            Some(Verdict::Retained(blockers)) => {
                self.mark_external(blockers);
                if let Some(class) = HeldClass::of(blockers) {
                    self.hold(at.children, &Held::of(class, disk_bytes), None);
                }
            }
            Some(Verdict::Unknown) => {
                let unknown = Metrics {
                    unknown: disk_bytes,
                    ..Metrics::default()
                };
                self.spread_over_holders(at.children, &unknown);
                self.mark_external(Blockers::UNKNOWN);
            }
        }
        self.exclude_packages(at, freed);
    }

    /// Leaves the extent out of the directories above package data where
    /// it is that data's alone.
    fn exclude_packages(&mut self, at: &Attribution, freed: Freed) {
        let Some(packages) = &mut self.packages else {
            return;
        };
        let nodes = &self.tree.nodes;
        let recycled = |n: &u32| nodes[*n as usize].flags.contains(NodeFlags::RECYCLE);
        packages.extent(&PackageExtent {
            judgement: &at.judgement,
            holders: &self.nodes,
            children: at.children,
            freed,
            all_recycled: self.nodes.iter().all(recycled),
        });
    }

    /// Collects the nodes naming `inodes` and the `children`.
    fn holding_nodes(&mut self, inodes: &[u32], children: &[u32]) {
        self.nodes.clear();
        for &inode in inodes {
            let names = self.tree.names_of(inode);
            self.nodes.extend(names.iter().map(|&(_, node)| node));
        }
        self.nodes.extend_from_slice(children);
        self.nodes.sort_unstable();
        self.nodes.dedup();
    }

    /// Counts `value` once per holding subtree; nested scopes' counts
    /// already include it.
    fn spread_over_holders(&mut self, children: &[u32], value: &Metrics) {
        self.spread.add(&self.nodes, value);
        for &child in children {
            self.spread.acc[child as usize].sub(value);
        }
    }

    /// Counts `value` once per directory holding the extent, below
    /// `owner` when one frees it; nested scopes' counts already include it.
    fn hold(&mut self, children: &[u32], value: &Held, owner: Option<u32>) {
        let mut dirs = std::mem::take(&mut self.dirs);
        dirs.clear();
        dirs.extend(self.nodes.iter().map(|&n| self.dir_of(n)));
        dirs.sort_unstable();
        dirs.dedup();
        self.held.add(&dirs, value);
        self.dirs = dirs;
        for &child in children {
            let slot = self.held.slot(child);
            self.held.acc[slot].sub(value);
        }
        if let Some(owner) = owner {
            let slot = self.held.slot(self.dir_of(owner));
            self.held.acc[slot].sub(value);
        }
    }

    /// Holds `bytes` linked inside the scope by `class` in the
    /// directories below `owner`, or in every one when it is outside the
    /// scope; links in a `#recycle` hold them as recycle bin bytes.
    fn hold_linked(&mut self, class: HeldClass, bytes: u64, children: &[u32], owner: Option<u32>) {
        let outside = self.outside_recycle_bins();
        if owner.is_some() && outside.is_empty() {
            self.hold(children, &Held::of(HeldClass::RecycleBin, bytes), owner);
            return;
        }
        self.hold(children, &Held::of(class, bytes), owner);
        if let Some(owner) = owner {
            self.held_by_recycle_bins(class, bytes, &outside, owner);
        }
    }

    /// The holding nodes outside every `#recycle`.
    fn outside_recycle_bins(&self) -> Vec<u32> {
        let nodes = &self.tree.nodes;
        let outside = |n: &u32| !nodes[*n as usize].flags.contains(NodeFlags::RECYCLE);
        self.nodes.iter().copied().filter(outside).collect()
    }

    /// Moves the bytes of a local link to the recycle bin class in the
    /// directories below `owner` whose other links all lie in a
    /// `#recycle`: those at or above every link `outside` one.
    fn held_by_recycle_bins(&mut self, class: HeldClass, bytes: u64, outside: &[u32], owner: u32) {
        let lowest = self.common_ancestor(outside);
        let (from, to) = (self.dir_of(lowest), self.dir_of(owner));
        if from == to {
            return;
        }
        let mut moved = Held::of(HeldClass::RecycleBin, bytes);
        moved.sub(&Held::of(class, bytes));
        let (from, to) = (self.held.slot(from), self.held.slot(to));
        self.held.acc[from].add(&moved);
        self.held.acc[to].sub(&moved);
    }

    /// `node` if it is a directory, else its parent.
    fn dir_of(&self, node: u32) -> u32 {
        let n = &self.tree.nodes[node as usize];
        if node == VIRTUAL_ROOT || n.kind == FileKind::Directory {
            node
        } else {
            n.parent
        }
    }

    fn mark_external(&mut self, blockers: Blockers) {
        for &n in &self.nodes {
            self.external[n as usize] = self.external[n as usize].with(blockers);
        }
    }

    /// Counts a reclaimable extent; returns where it is freed.
    fn reclaimable(&mut self, judgement: &Judgement, at: &Attribution) -> Freed {
        let owner = match at.owner {
            Owner::Here => {
                let owner = self.common_ancestor(&self.nodes);
                let value = Metrics {
                    reclaimable: judgement.disk_bytes,
                    pinned: judgement.pinned,
                    ..Metrics::default()
                };
                self.spread.acc[owner as usize].add(&value);
                owner
            }
            Owner::Above => VIRTUAL_ROOT,
        };
        if let Some(class) = HeldClass::of(judgement.local) {
            let freed_by = (at.owner == Owner::Here).then_some(owner);
            self.hold_linked(class, judgement.disk_bytes, at.children, freed_by);
        }
        for &start in &self.nodes {
            let mut at = start;
            while at != owner {
                self.local[at as usize] = self.local[at as usize].with(judgement.local);
                at = self.tree.nodes[at as usize].parent;
            }
        }
        match at.owner {
            Owner::Here => Freed::At(owner),
            Owner::Above => Freed::Above,
        }
    }

    /// The lowest common ancestor of `nodes`, which must be non-empty.
    fn common_ancestor(&self, nodes: &[u32]) -> u32 {
        let first = nodes[0];
        let mut path = vec![first];
        let mut at = first;
        while at != VIRTUAL_ROOT {
            at = self.tree.nodes[at as usize].parent;
            path.push(at);
        }
        let mut deepest_common = 0;
        for &start in &nodes[1..] {
            let mut at = start;
            let index = loop {
                if let Some(i) = path.iter().position(|&p| p == at) {
                    break i;
                }
                at = self.tree.nodes[at as usize].parent;
            };
            deepest_common = deepest_common.max(index);
        }
        path[deepest_common]
    }

    pub fn finish(self) -> Aggregate {
        let mut external = self.external;
        let nodes = &self.tree.nodes;
        for i in (1..nodes.len()).rev() {
            let parent = nodes[i].parent as usize;
            external[parent] = external[parent].with(external[i]);
        }
        let blockers = external
            .iter()
            .zip(&self.local)
            .map(|(e, l)| e.with(*l))
            .collect();
        let slots = self.held.slots.clone().unwrap_or_default();
        Aggregate {
            metrics: self.spread.into_subtree_sums(),
            held: DirHeld {
                slots,
                values: self.held.into_subtree_sums(),
            },
            blockers,
            external,
            summary: self.summary,
            excluded: self.packages.map(Packages::finish).unwrap_or_default(),
            package_roots: self
                .tree
                .roots
                .iter()
                .copied()
                .filter(|&r| {
                    nodes[r as usize].flags.contains(NodeFlags::PACKAGE)
                        || nodes[r as usize].flags.contains(NodeFlags::OWN_STATE)
                })
                .collect(),
        }
    }
}

/// A value [`Spread`] adds and removes.
trait Tally: Copy + Default {
    fn add(&mut self, other: &Self);
    fn sub(&mut self, other: &Self);
}

impl Tally for Held {
    fn add(&mut self, other: &Self) {
        Held::add(self, other);
    }

    fn sub(&mut self, other: &Self) {
        Held::sub(self, other);
    }
}

impl Tally for Metrics {
    fn add(&mut self, other: &Self) {
        Metrics::add(self, other);
    }

    fn sub(&mut self, other: &Self) {
        Metrics::sub(self, other);
    }
}

/// Adds one value per distinct subtree: every node on the union of the
/// root paths of a node set gets `+value` and every edge of that union
/// `-value` at its parent, so subtree sums count the value once in each
/// subtree holding any of the nodes.
struct Spread<'a, T> {
    nodes: &'a [Node],
    /// Where each node's value is kept, `NO_SLOT` for none; `None`
    /// keeps one per node.
    slots: Option<Vec<u32>>,
    /// By slot.
    acc: Vec<T>,
    stamp: Vec<u32>,
    color: u32,
}

impl<'a, T: Tally> Spread<'a, T> {
    fn new(nodes: &'a [Node]) -> Self {
        Spread {
            nodes,
            slots: None,
            acc: vec![T::default(); nodes.len()],
            stamp: vec![0; nodes.len()],
            color: 0,
        }
    }

    /// A spread keeping values for the virtual root and directories only;
    /// it may only be given those.
    fn over_directories(nodes: &'a [Node]) -> Self {
        let mut next = 0;
        let slots: Vec<u32> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                if i == VIRTUAL_ROOT as usize || n.kind == FileKind::Directory {
                    next += 1;
                    next - 1
                } else {
                    NO_SLOT
                }
            })
            .collect();
        Spread {
            nodes,
            slots: Some(slots),
            acc: vec![T::default(); next as usize],
            stamp: vec![0; next as usize],
            color: 0,
        }
    }

    fn slot(&self, node: u32) -> usize {
        match &self.slots {
            None => node as usize,
            Some(slots) => slots[node as usize] as usize,
        }
    }

    fn add(&mut self, nodes: &[u32], value: &T) {
        if let [only] = nodes {
            // A single holder: its subtree sums already count it once.
            let slot = self.slot(*only);
            self.acc[slot].add(value);
            return;
        }
        self.next_color();
        for &start in nodes {
            let mut at = start;
            while self.stamp[self.slot(at)] != self.color {
                let slot = self.slot(at);
                self.stamp[slot] = self.color;
                self.acc[slot].add(value);
                let parent = self.nodes[at as usize].parent;
                if parent == NO_PARENT {
                    break;
                }
                let slot = self.slot(parent);
                self.acc[slot].sub(value);
                at = parent;
            }
        }
    }

    fn next_color(&mut self) {
        if self.color == u32::MAX {
            self.stamp.fill(0);
            self.color = 0;
        }
        self.color += 1;
    }

    /// Parents precede their children, so one reverse pass sums subtrees.
    fn into_subtree_sums(mut self) -> Vec<T> {
        for i in (1..self.nodes.len()).rev() {
            let slot = self.slot(i as u32);
            if slot == NO_SLOT as usize {
                continue;
            }
            let parent = self.slot(self.nodes[i].parent);
            let child = self.acc[slot];
            self.acc[parent].add(&child);
        }
        self.acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::parser::DiskExtent;
    use crate::btrfs::subvolume::SubvolumeSet;
    use crate::model::namespace::Namespace;
    use crate::model::testing::*;

    const TREE: u64 = 257;
    const MIB: u64 = 1 << 20;

    fn ns(items: Vec<TreeItem>) -> Namespace {
        read_tree(TREE, items).unwrap()
    }

    fn scope(ns: &Namespace, root: u64) -> ScopeTree {
        let mut tree = ScopeTree::new();
        tree.add_root(
            ns,
            root,
            b"/v",
            RootKind::Path,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        tree
    }

    #[test]
    fn keeps_ctimes_for_entries_inside_recycle_bins() {
        let times = [100, 200, 300, 400];
        let ns = ns(vec![
            inode_with_ctime(256, DIR, 0, 1, timespec(times[0])),
            entry(256, 2, "share", 257),
            inode_with_ctime(257, DIR, 0, 1, timespec(times[1])),
            entry(257, 2, "live", 258),
            entry(257, 3, "#recycle", 259),
            inode_with_ctime(258, FILE, 0, 1, timespec(times[2])),
            inode_with_ctime(259, DIR, 0, 1, timespec(times[2])),
            entry(259, 2, "old", 260),
            inode_with_ctime(260, FILE, 0, 1, timespec(times[3])),
        ]);

        let tree = scope(&ns, 256);

        assert_eq!(
            tree.recycle_ctimes(),
            &[
                RecycleEntryCtime {
                    tree: TREE,
                    inode: 259,
                    ctime: 300,
                },
                RecycleEntryCtime {
                    tree: TREE,
                    inode: 260,
                    ctime: 400,
                },
            ]
        );
    }

    #[test]
    fn recycle_ctimes_transfer_to_the_unit_summary() {
        let ns = ns(vec![
            inode_with_ctime(256, DIR, 0, 1, timespec(100)),
            entry(256, 2, "#recycle", 257),
            inode_with_ctime(257, DIR, 0, 1, timespec(200)),
        ]);
        let mut tree = scope(&ns, 256);

        let entries = tree.take_recycle_ctimes();

        assert_eq!(
            entries,
            [RecycleEntryCtime {
                tree: TREE,
                inode: 257,
                ctime: 200,
            }]
        );
        assert!(tree.recycle_ctimes().is_empty());
    }

    fn timespec(sec: u64) -> crate::btrfs::parser::BtrfsTimespec {
        crate::btrfs::parser::BtrfsTimespec { sec, nsec: 0 }
    }

    fn node(tree: &ScopeTree, path: &str) -> u32 {
        (0..tree.len() as u32)
            .find(|&n| tree.path(n) == path.as_bytes())
            .unwrap_or_else(|| panic!("no node {path}"))
    }

    fn metrics(tree: &ScopeTree, path: &str) -> Metrics {
        tree.aggregate()[node(tree, path) as usize]
    }

    fn paths(tree: &ScopeTree) -> Vec<String> {
        let mut out: Vec<String> = (1..tree.len() as u32)
            .map(|n| String::from_utf8(tree.path(n)).unwrap())
            .collect();
        out.sort();
        out
    }

    /// `/v` holding `a` (1 MiB, one extent) and `dir/b` (2 MiB, two extents).
    fn simple() -> Vec<TreeItem> {
        vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "a", 257),
            entry(256, 3, "dir", 258),
            inode(257, FILE, MIB, 1),
            whole_extent(257, 0, 10 * MIB, MIB),
            inode(258, DIR, 0, 1),
            entry(258, 2, "b", 259),
            inode(259, FILE, 2 * MIB, 1),
            whole_extent(259, 0, 20 * MIB, MIB),
            whole_extent(259, MIB, 30 * MIB, MIB),
        ]
    }

    #[test]
    fn preorder_lists_each_root_then_its_subtree_by_name() {
        let tree = scope(&ns(simple()), 256);
        let order: Vec<(String, u32)> = tree
            .preorder()
            .into_iter()
            .map(|(n, depth)| (String::from_utf8(tree.path(n)).unwrap(), depth))
            .collect();
        assert_eq!(
            order,
            [
                ("/v".to_string(), 0),
                ("/v/a".to_string(), 1),
                ("/v/dir".to_string(), 1),
                ("/v/dir/b".to_string(), 2)
            ]
        );
    }

    #[test]
    fn materializes_the_selected_subtree_with_full_paths() {
        let tree = scope(&ns(simple()), 256);
        assert_eq!(paths(&tree), ["/v", "/v/a", "/v/dir", "/v/dir/b"]);
        let dir = tree.node(node(&tree, "/v/dir"));
        assert_eq!(dir.kind, FileKind::Directory);
    }

    #[test]
    fn aggregates_logical_physical_and_extent_counts_upwards() {
        let tree = scope(&ns(simple()), 256);
        let root = metrics(&tree, "/v");
        assert_eq!(
            (root.logical, root.physical, root.extents),
            (3 * MIB, 3 * MIB, 3)
        );
        assert_eq!((root.entries, root.files), (4, 2));
        let b = metrics(&tree, "/v/dir/b");
        assert_eq!((b.logical, b.physical, b.extents), (2 * MIB, 2 * MIB, 2));
        assert_eq!(tree.aggregate()[0], root, "the virtual root sums its roots");
    }

    #[test]
    fn a_partially_referenced_extent_counts_whole_and_once() {
        // Bookend: two file ranges reference parts of one 1 MiB extent.
        let part = |file_offset, offset| {
            extent_item(
                257,
                file_offset,
                REGULAR,
                DiskExtent {
                    disk_bytenr: 10 * MIB,
                    disk_num_bytes: MIB,
                    offset,
                    num_bytes: 4096,
                },
            )
        };
        let tree = scope(
            &ns(vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "a", 257),
                inode(257, FILE, 8192, 1),
                part(0, 0),
                part(4096, 65536),
            ]),
            256,
        );
        let a = metrics(&tree, "/v/a");
        assert_eq!((a.logical, a.physical, a.extents), (8192, MIB, 1));
    }

    #[test]
    fn reflinked_files_share_one_physical_extent_in_their_directory() {
        let tree = scope(
            &ns(vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "x", 257),
                entry(256, 3, "y", 258),
                inode(257, FILE, MIB, 1),
                whole_extent(257, 0, 10 * MIB, MIB),
                inode(258, FILE, MIB, 1),
                whole_extent(258, 0, 10 * MIB, MIB),
            ]),
            256,
        );
        assert_eq!(metrics(&tree, "/v/x").physical, MIB);
        assert_eq!(metrics(&tree, "/v/y").physical, MIB);
        let root = metrics(&tree, "/v");
        assert_eq!(
            (root.logical, root.physical, root.extents),
            (2 * MIB, MIB, 1)
        );
    }

    #[test]
    fn a_hard_link_counts_in_each_directory_and_once_above_them() {
        let tree = scope(
            &ns(vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "d1", 257),
                entry(256, 3, "d2", 258),
                inode(257, DIR, 0, 1),
                entry(257, 2, "h", 259),
                inode(258, DIR, 0, 1),
                entry(258, 2, "h", 259),
                inode(259, FILE, MIB, 2),
                whole_extent(259, 0, 10 * MIB, MIB),
            ]),
            256,
        );
        for dir in ["/v/d1", "/v/d2"] {
            let m = metrics(&tree, dir);
            assert_eq!((m.logical, m.physical), (MIB, MIB), "{dir}");
        }
        let root = metrics(&tree, "/v");
        assert_eq!((root.logical, root.physical, root.files), (MIB, MIB, 2));
        let h1 = tree.node(node(&tree, "/v/d1/h"));
        let h2 = tree.node(node(&tree, "/v/d2/h"));
        assert_eq!(h1.inode, h2.inode, "both names share one inode");
        assert_eq!(tree.inode(h1.inode).unwrap().nlink, 2);
    }

    #[test]
    fn sparse_and_compressed_files_keep_logical_and_physical_apart() {
        let hole = DiskExtent {
            disk_bytenr: 0,
            disk_num_bytes: 0,
            offset: 0,
            num_bytes: MIB - 4096,
        };
        let compressed = DiskExtent {
            disk_bytenr: 40 * MIB,
            disk_num_bytes: 4096,
            offset: 0,
            num_bytes: 131072,
        };
        let tree = scope(
            &ns(vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "sparse", 257),
                entry(256, 3, "zip", 258),
                inode(257, FILE, MIB, 1),
                whole_extent(257, 0, 10 * MIB, 4096),
                extent_item(257, 4096, REGULAR, hole),
                inode(258, FILE, 131072, 1),
                extent_item(258, 0, REGULAR, compressed),
            ]),
            256,
        );
        let sparse = metrics(&tree, "/v/sparse");
        assert_eq!((sparse.logical, sparse.physical), (MIB, 4096));
        let zip = metrics(&tree, "/v/zip");
        assert_eq!((zip.logical, zip.physical), (131072, 4096));
    }

    #[test]
    fn inline_data_is_counted_apart_from_physical_extents() {
        let mut inline = crate::btrfs::parser::testing::encode_extent_header(0);
        inline.extend(b"hello");
        let tree = scope(
            &ns(vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "small", 257),
                inode(257, FILE, 5, 1),
                (crate::btrfs::keys::Key::new(257, 108, 0), inline),
            ]),
            256,
        );
        let m = metrics(&tree, "/v");
        assert_eq!((m.logical, m.physical, m.inline), (5, 0, 5));
    }

    #[test]
    fn a_file_can_be_a_root() {
        let tree = scope(&ns(simple()), 257);
        assert_eq!(paths(&tree), ["/v"]);
        assert_eq!(metrics(&tree, "/v").physical, MIB);
    }

    #[test]
    fn missing_root_inode_is_an_error() {
        let mut tree = ScopeTree::new();
        let err = tree
            .add_root(
                &ns(simple()),
                999,
                b"/v",
                RootKind::Path,
                &SubvolumeSet::default(),
                &Marks::default(),
            )
            .unwrap_err();
        assert!(matches!(err, NamespaceError::MissingInode(999)));
    }

    #[test]
    fn dangling_entries_are_skipped_and_counted() {
        let mut items = simple();
        items.push(entry(258, 3, "gone", 999));
        let tree = scope(&ns(items), 256);
        assert_eq!(paths(&tree), ["/v", "/v/a", "/v/dir", "/v/dir/b"]);
        assert_eq!(tree.anomalies().dangling_entries, 1);
    }

    #[test]
    fn dangling_entries_inside_the_state_directory_are_not_counted() {
        let mut items = simple();
        items.push(entry(258, 3, "replaced", 999));
        let marks = Marks {
            own_state: [(TREE, 258)].into(),
            ..Marks::default()
        };
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns(items),
            256,
            b"/v",
            RootKind::Path,
            &SubvolumeSet::default(),
            &marks,
        )
        .unwrap();
        assert_eq!(paths(&tree), ["/v", "/v/a", "/v/dir", "/v/dir/b"]);
        assert_eq!(tree.anomalies().dangling_entries, 0);
    }

    fn with_subvolume(items: &mut Vec<TreeItem>) {
        items.push(subvolume_entry(256, 9, "share", 300));
    }

    #[test]
    fn linked_nested_subvolumes_are_attached_under_their_boundary() {
        let mut items = simple();
        with_subvolume(&mut items);
        let parent = ns(items);
        let subvolumes = SubvolumeSet {
            subvolumes: vec![linked(300, TREE, 256, "share")],
            ..SubvolumeSet::default()
        };
        let mut tree = ScopeTree::new();
        let pending = tree
            .add_root(
                &parent,
                256,
                b"/v",
                RootKind::Volume,
                &subvolumes,
                &Marks::default(),
            )
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].subvolume, 300);
        let child = read_tree(
            300,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "f", 257),
                inode(257, FILE, MIB, 1),
                whole_extent(257, 0, 10 * MIB, MIB),
            ],
        )
        .unwrap();
        let more = tree.attach(&pending[0], &child, &subvolumes, &Marks::default());
        assert!(more.is_empty());
        assert!(paths(&tree).contains(&"/v/share/f".to_string()));
        let share = tree.node(node(&tree, "/v/share"));
        assert!(share.flags.contains(NodeFlags::SUBVOLUME_ROOT));
        // /v/a and /v/share/f reference the same extent: counted once.
        let root = metrics(&tree, "/v");
        assert_eq!((root.physical, root.extents), (3 * MIB, 3));
        assert_eq!(metrics(&tree, "/v/share").physical, MIB);
    }

    #[test]
    fn unlinked_subvolume_entries_are_empty_placeholders() {
        let mut items = simple();
        with_subvolume(&mut items);
        let mut tree = ScopeTree::new();
        let pending = tree
            .add_root(
                &ns(items),
                256,
                b"/v",
                RootKind::Path,
                &SubvolumeSet::default(),
                &Marks::default(),
            )
            .unwrap();
        assert!(pending.is_empty());
        let share = tree.node(node(&tree, "/v/share"));
        assert!(share.flags.contains(NodeFlags::PLACEHOLDER));
    }

    #[test]
    fn several_roots_union_their_extents_in_the_virtual_root() {
        let n = ns(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "x", 257),
            entry(256, 3, "y", 258),
            inode(257, FILE, MIB, 1),
            whole_extent(257, 0, 10 * MIB, MIB),
            inode(258, FILE, MIB, 1),
            whole_extent(258, 0, 10 * MIB, MIB),
        ]);
        let mut tree = ScopeTree::new();
        let none = SubvolumeSet::default();
        tree.add_root(&n, 257, b"/v/x", RootKind::Path, &none, &Marks::default())
            .unwrap();
        tree.add_root(&n, 258, b"/v/y", RootKind::Path, &none, &Marks::default())
            .unwrap();
        let all = tree.aggregate();
        assert_eq!((all[0].logical, all[0].physical), (2 * MIB, MIB));
        assert_eq!(tree.roots(), vec![1, 2]);
    }

    fn flags_of(tree: &ScopeTree, path: &str) -> NodeFlags {
        tree.node(node(tree, path)).flags
    }

    #[test]
    fn synology_service_directories_are_classified() {
        let n = ns(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "@appstore", 257),
            entry(256, 3, "share", 258),
            inode(257, DIR, 0, 1),
            entry(257, 2, "pkg", 259),
            inode(258, DIR, 0, 1),
            entry(258, 2, "@eaDir", 260),
            entry(258, 3, "#recycle", 261),
            entry(258, 4, "@keep", 262),
            entry(258, 5, "#snapshot", 263),
            inode(259, FILE, 1, 1),
            inode(260, DIR, 0, 1),
            inode(261, DIR, 0, 1),
            entry(261, 2, "old", 264),
            inode(262, DIR, 0, 1),
            inode(263, DIR, 0, 1),
            inode(264, FILE, 1, 1),
        ]);
        let mut tree = ScopeTree::new();
        tree.add_root(
            &n,
            256,
            b"/volume1",
            RootKind::Volume,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        assert!(flags_of(&tree, "/volume1/@appstore").contains(NodeFlags::SYSTEM));
        assert!(flags_of(&tree, "/volume1/@appstore/pkg").contains(NodeFlags::SYSTEM));
        assert!(flags_of(&tree, "/volume1/share").is_empty());
        assert!(flags_of(&tree, "/volume1/share/@eaDir").contains(NodeFlags::SYSTEM));
        assert!(flags_of(&tree, "/volume1/share/#snapshot").contains(NodeFlags::SYSTEM));
        assert!(
            flags_of(&tree, "/volume1/share/@keep").is_empty(),
            "@ is special only at the volume root"
        );
        assert!(flags_of(&tree, "/volume1/share/#recycle").contains(NodeFlags::RECYCLE));
        assert!(flags_of(&tree, "/volume1/share/#recycle/old").contains(NodeFlags::RECYCLE));
    }

    #[test]
    fn package_data_starts_at_guarded_service_paths_and_marked_folders() {
        let n = ns(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "@ActiveBackup", 257),
            entry(256, 3, "share", 258),
            inode(257, DIR, 0, 1),
            inode(258, DIR, 0, 1),
            entry(258, 2, "abb", 259),
            entry(258, 3, "hb", 260),
            entry(258, 4, "plain", 261),
            inode(259, DIR, 0, 1),
            entry(259, 2, "@ActiveBackup", 262),
            inode(260, DIR, 0, 1),
            entry(260, 2, "SynologyHyperBackup.bkpi", 263),
            inode(261, DIR, 0, 1),
            inode(262, DIR, 0, 1),
            inode(263, FILE, 1, 1),
        ]);
        let mut tree = ScopeTree::new();
        tree.add_root(
            &n,
            256,
            b"/volume1",
            RootKind::Volume,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        let package = |path| flags_of(&tree, path).contains(NodeFlags::PACKAGE);
        assert!(
            !package("/volume1"),
            "a marker at the volume root marks no share"
        );
        assert!(
            !package("/volume1/@ActiveBackup"),
            "the database is not guarded"
        );
        assert!(package("/volume1/share/abb"));
        assert!(
            !package("/volume1/share/abb/@ActiveBackup"),
            "a marker marks its folder, not itself"
        );
        assert!(package("/volume1/share/hb"));
        assert!(!package("/volume1/share/hb/SynologyHyperBackup.bkpi"));
        assert!(!package("/volume1/share"));
        assert!(!package("/volume1/share/plain"));
        let marker = |path| tree.marker(node(&tree, path));
        assert_eq!(marker("/volume1/share/abb"), Some(&b"@ActiveBackup"[..]));
        assert_eq!(
            marker("/volume1/share/hb"),
            Some(&b"SynologyHyperBackup.bkpi"[..])
        );
        assert_eq!(marker("/volume1/share"), None);
        assert_eq!(marker("/volume1"), None);
    }

    #[test]
    fn a_path_root_does_not_treat_at_names_as_volume_services() {
        let n = ns(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "@data", 257),
            inode(257, DIR, 0, 1),
        ]);
        let tree = scope(&n, 256);
        assert!(flags_of(&tree, "/v/@data").is_empty());
    }

    #[test]
    fn ecryptfs_lower_directories_hide_names_but_keep_bytes() {
        let n = ns(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "@secret@", 257),
            inode(257, DIR, 0, 1),
            entry(257, 2, "ECRYPTFS_FNEK_ENCRYPTED.FWabc", 258),
            entry(257, 3, "ECRYPTFS_FNEK_ENCRYPTED.FWdef", 259),
            inode(258, FILE, 12288, 1),
            whole_extent(258, 0, 10 * MIB, 12288),
            inode(259, DIR, 0, 1),
            entry(259, 2, "ECRYPTFS_FNEK_ENCRYPTED.FWghi", 260),
            inode(260, FILE, 8192, 1),
            whole_extent(260, 0, 20 * MIB, 8192),
        ]);
        let tree = scope(&n, 256);
        assert_eq!(paths(&tree), ["/v", "/v/@secret@"]);
        let secret = metrics(&tree, "/v/@secret@");
        assert_eq!((secret.physical, secret.extents), (20480, 2));
        assert!(flags_of(&tree, "/v/@secret@").contains(NodeFlags::ENCRYPTED));
        assert_eq!(tree.encrypted(), vec![node(&tree, "/v/@secret@")]);
    }

    #[test]
    fn marked_directories_are_flagged_by_identity() {
        let marks = Marks {
            encrypted: [(TREE, 258)].into(),
            own_state: [(TREE, 258)].into(),
        };
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns(simple()),
            256,
            b"/v",
            RootKind::Path,
            &SubvolumeSet::default(),
            &marks,
        )
        .unwrap();
        let dir = flags_of(&tree, "/v/dir");
        assert!(dir.contains(NodeFlags::ENCRYPTED) && dir.contains(NodeFlags::OWN_STATE));
        assert_eq!(paths(&tree), ["/v", "/v/a", "/v/dir"]);
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;
    use crate::btrfs::backref::DataBackref;
    use crate::btrfs::extent_refs::ExtentRefs;
    use crate::btrfs::keys::INODE_ITEM_KEY;
    use crate::btrfs::parser::DiskExtent;
    use crate::btrfs::subvolume::LeafSharing;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};
    use crate::model::accounting::{HeldClass, RootClass};
    use crate::model::testing::*;
    use crate::spill::testing::scratch;
    use std::collections::BTreeMap;

    const TREE: u64 = 257;
    const MIB: u64 = 1 << 20;
    const KIB: u64 = 1 << 10;

    struct Roots;

    impl RootClass for Roots {
        fn external_root(&self, root: u64) -> Blockers {
            if root == TREE {
                Blockers::LIVE_REFLINK
            } else {
                Blockers::SNAPSHOT
            }
        }

        fn shared_leaf(&self, _tree: u64) -> Blockers {
            Blockers::SHARED_TREE
        }
    }

    fn scope(items: Vec<TreeItem>) -> ScopeTree {
        let ns = read_tree(TREE, items).unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v",
            RootKind::Path,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        tree
    }

    /// Extent `bytenr` of `len` with one direct backref per `(inode, count)`.
    fn owned(bytenr: u64, len: u64, owners: &[(u64, u32)]) -> (u64, ExtentRefs) {
        let backrefs: Vec<DataBackref> = owners
            .iter()
            .map(|&(objectid, count)| DataBackref::Direct {
                root: TREE,
                objectid,
                offset: 0,
                count,
            })
            .collect();
        let refs = backrefs.iter().map(|b| u64::from(b.count())).sum();
        (
            bytenr,
            ExtentRefs {
                num_bytes: len,
                refs,
                generation: 1,
                backrefs,
                malformed: false,
            },
        )
    }

    fn account(tree: &mut ScopeTree, refs: &BTreeMap<u64, ExtentRefs>) -> Aggregate {
        tree.prepare();
        let mut accountant = Accountant::new(tree, Some(&Roots));
        for bytenr in accountant.candidates() {
            accountant.extent(bytenr, refs.get(&bytenr));
        }
        accountant.finish()
    }

    fn at<'a>(tree: &ScopeTree, agg: &'a Aggregate, path: &str) -> (&'a Metrics, Blockers) {
        let n = (0..tree.len() as u32)
            .find(|&n| tree.path(n) == path.as_bytes())
            .unwrap_or_else(|| panic!("no node {path}"));
        (&agg.metrics[n as usize], agg.blockers[n as usize])
    }

    /// `/v/a` (one extent) and `/v/dir/b` (two extents).
    fn simple() -> Vec<TreeItem> {
        vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "a", 257),
            entry(256, 3, "dir", 258),
            inode(257, FILE, MIB, 1),
            whole_extent(257, 0, 10 * MIB, MIB),
            inode(258, DIR, 0, 1),
            entry(258, 2, "b", 259),
            inode(259, FILE, 2 * MIB, 1),
            whole_extent(259, 0, 20 * MIB, MIB),
            whole_extent(259, MIB, 30 * MIB, MIB),
        ]
    }

    #[test]
    fn state_directory_has_a_package_free_folder_display_without_changing_accounting() {
        let ns = read_tree(TREE, simple()).unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v",
            RootKind::Path,
            &SubvolumeSet::default(),
            &Marks {
                own_state: [(TREE, 258)].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let refs = BTreeMap::from([
            owned(10 * MIB, MIB, &[(257, 1)]),
            owned(20 * MIB, MIB, &[(259, 1)]),
            owned(30 * MIB, MIB, &[(259, 1)]),
        ]);
        let agg = account(&mut tree, &refs);
        let root = (0..tree.len() as u32)
            .find(|&n| tree.path(n) == b"/v")
            .unwrap();
        assert_eq!(agg.metrics[root as usize].physical, 3 * MIB);
        assert_eq!(agg.metrics[root as usize].reclaimable, 3 * MIB);
        let outside = agg
            .outside(root)
            .expect("state data leaves the ordinary folder display");
        assert_eq!(outside.metrics.physical, MIB);
        assert_eq!(outside.metrics.reclaimable, MIB);
    }

    #[test]
    fn exclusively_owned_extents_are_reclaimable_where_they_live() {
        let mut tree = scope(simple());
        let refs = BTreeMap::from([
            owned(10 * MIB, MIB, &[(257, 1)]),
            owned(20 * MIB, MIB, &[(259, 1)]),
            owned(30 * MIB, MIB, &[(259, 1)]),
        ]);
        let agg = account(&mut tree, &refs);
        let (v, blockers) = at(&tree, &agg, "/v");
        assert_eq!(
            (v.physical, v.reclaimable, v.unknown),
            (3 * MIB, 3 * MIB, 0)
        );
        assert!(blockers.is_empty());
        assert_eq!(at(&tree, &agg, "/v/a").0.reclaimable, MIB);
        assert_eq!(at(&tree, &agg, "/v/dir").0.reclaimable, 2 * MIB);
        assert_eq!(agg.metrics[0].reclaimable, 3 * MIB);
        assert_eq!(agg.summary.reclaimable_extents, 3);
    }

    #[test]
    fn a_reflink_pair_is_reclaimable_only_by_their_common_ancestor() {
        let mut tree = scope(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "d1", 257),
            entry(256, 3, "d2", 258),
            inode(257, DIR, 0, 1),
            entry(257, 2, "x", 259),
            inode(258, DIR, 0, 1),
            entry(258, 2, "y", 260),
            inode(259, FILE, MIB, 1),
            whole_extent(259, 0, 10 * MIB, MIB),
            inode(260, FILE, MIB, 1),
            whole_extent(260, 0, 10 * MIB, MIB),
        ]);
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(259, 1), (260, 1)])]);
        let agg = account(&mut tree, &refs);
        for path in ["/v/d1", "/v/d1/x", "/v/d2/y"] {
            let (m, blockers) = at(&tree, &agg, path);
            assert_eq!((m.physical, m.reclaimable), (MIB, 0), "{path}");
            assert_eq!(blockers, Blockers::LIVE_REFLINK, "{path}");
        }
        let (v, blockers) = at(&tree, &agg, "/v");
        assert_eq!((v.physical, v.reclaimable), (MIB, MIB));
        assert!(blockers.is_empty());
    }

    #[test]
    fn extents_shared_by_two_roots_are_reclaimable_only_together() {
        let ns = read_tree(
            TREE,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "d1", 257),
                entry(256, 3, "d2", 258),
                inode(257, DIR, 0, 1),
                entry(257, 2, "x", 259),
                inode(258, DIR, 0, 1),
                entry(258, 2, "y", 260),
                inode(259, FILE, MIB, 1),
                whole_extent(259, 0, 10 * MIB, MIB),
                inode(260, FILE, MIB, 1),
                whole_extent(260, 0, 10 * MIB, MIB),
            ],
        )
        .unwrap();
        let mut tree = ScopeTree::new();
        for (ino, name) in [(257, "/v/d1"), (258, "/v/d2")] {
            tree.add_root(
                &ns,
                ino,
                name.as_bytes(),
                RootKind::Path,
                &SubvolumeSet::default(),
                &Marks::default(),
            )
            .unwrap();
        }
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(259, 1), (260, 1)])]);
        let agg = account(&mut tree, &refs);
        for root in ["/v/d1", "/v/d2"] {
            let (m, blockers) = at(&tree, &agg, root);
            assert_eq!((m.physical, m.reclaimable), (MIB, 0), "{root}");
            assert_eq!(blockers, Blockers::LIVE_REFLINK, "{root}");
        }
        let all = &agg.metrics[VIRTUAL_ROOT as usize];
        assert_eq!((all.physical, all.reclaimable), (MIB, MIB));
    }

    #[test]
    fn hard_links_free_data_only_with_their_last_name() {
        let items = |nlink| {
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "d1", 257),
                entry(256, 3, "d2", 258),
                inode(257, DIR, 0, 1),
                entry(257, 2, "h", 259),
                inode(258, DIR, 0, 1),
                entry(258, 2, "h", 259),
                inode(259, FILE, MIB, nlink),
                whole_extent(259, 0, 10 * MIB, MIB),
            ]
        };
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(259, 1)])]);
        let mut tree = scope(items(2));
        let agg = account(&mut tree, &refs);
        let (d1, blockers) = at(&tree, &agg, "/v/d1");
        assert_eq!((d1.reclaimable, blockers), (0, Blockers::HARD_LINK));
        assert_eq!(at(&tree, &agg, "/v").0.reclaimable, MIB);
        // A third name outside the selection keeps the inode alive.
        let mut tree = scope(items(3));
        let agg = account(&mut tree, &refs);
        let (v, blockers) = at(&tree, &agg, "/v");
        assert_eq!((v.reclaimable, blockers), (0, Blockers::HARD_LINK));
        assert_eq!(agg.metrics[0].reclaimable, 0);
    }

    #[test]
    fn hard_links_inside_an_encrypted_directory_count_every_name() {
        let items = |nlink| {
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "@secret@", 257),
                inode(257, DIR, 0, 1),
                entry(257, 2, "ECRYPTFS_FNEK_ENCRYPTED.FWabc", 259),
                entry(257, 3, "ECRYPTFS_FNEK_ENCRYPTED.FWdef", 259),
                inode(259, FILE, MIB, nlink),
                whole_extent(259, 0, 10 * MIB, MIB),
            ]
        };
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(259, 1)])]);
        let mut tree = scope(items(2));
        let agg = account(&mut tree, &refs);
        let (secret, blockers) = at(&tree, &agg, "/v/@secret@");
        assert_eq!((secret.reclaimable, blockers), (MIB, Blockers::default()));
        let mut tree = scope(items(3));
        let agg = account(&mut tree, &refs);
        let (secret, blockers) = at(&tree, &agg, "/v/@secret@");
        assert_eq!((secret.reclaimable, blockers), (0, Blockers::HARD_LINK));
    }

    #[test]
    fn directories_past_the_depth_cap_are_accounted_but_not_named() {
        let deep = MAX_DEPTH as u64 + 10;
        let mut items = vec![inode(256, DIR, 0, 1)];
        for i in 0..deep {
            let (parent, child) = (256 + i, 257 + i);
            items.push(entry(parent, 2, "d", child));
            items.push(inode(child, DIR, 0, 1));
        }
        let file = 257 + deep;
        items.push(entry(file - 1, 2, "f", file));
        items.push(inode(file, FILE, MIB, 1));
        items.push(whole_extent(file, 0, 10 * MIB, MIB));
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(file, 1)])]);
        let mut tree = scope(items);
        let named = tree.preorder().iter().map(|&(_, d)| d).max().unwrap();
        assert_eq!(named as usize, MAX_DEPTH);
        let agg = account(&mut tree, &refs);
        assert_eq!(agg.metrics[0].reclaimable, MIB);
        assert_eq!(tree.anomalies().cycles, 0);
    }

    #[test]
    fn external_references_retain_and_mark_every_holding_subtree() {
        let mut tree = scope(simple());
        let refs = BTreeMap::from([
            owned(10 * MIB, MIB, &[(257, 1), (999, 1)]),
            owned(20 * MIB, MIB, &[(259, 1)]),
            owned(30 * MIB, MIB, &[(259, 1)]),
        ]);
        let agg = account(&mut tree, &refs);
        let (a, blockers) = at(&tree, &agg, "/v/a");
        assert_eq!((a.physical, a.reclaimable, a.unknown), (MIB, 0, 0));
        assert_eq!(blockers, Blockers::LIVE_REFLINK);
        let (v, blockers) = at(&tree, &agg, "/v");
        assert_eq!(v.reclaimable, 2 * MIB);
        assert_eq!(blockers, Blockers::LIVE_REFLINK);
        assert!(at(&tree, &agg, "/v/dir").1.is_empty());
        assert_eq!(agg.summary.retained_extents, 1);
        assert_eq!(agg.summary.blocker_bytes(Blockers::LIVE_REFLINK), MIB);
    }

    #[test]
    fn unresolved_extents_are_unknown_not_reclaimable() {
        let mut tree = scope(simple());
        let refs = BTreeMap::from([owned(20 * MIB, MIB, &[(259, 1)])]);
        let agg = account(&mut tree, &refs);
        let (a, blockers) = at(&tree, &agg, "/v/a");
        assert_eq!((a.reclaimable, a.unknown), (0, MIB));
        assert_eq!(blockers, Blockers::UNKNOWN);
        let (dir, _) = at(&tree, &agg, "/v/dir");
        assert_eq!((dir.reclaimable, dir.unknown), (MIB, MIB));
        assert_eq!(agg.metrics[0].unknown, 2 * MIB);
        assert_eq!(agg.summary.unknown_extents, 2);
    }

    fn held(tree: &ScopeTree, agg: &Aggregate, path: &str) -> Vec<(&'static str, u64)> {
        let n = (0..tree.len() as u32)
            .find(|&n| tree.path(n) == path.as_bytes())
            .unwrap_or_else(|| panic!("no node {path}"));
        let held = agg.held.get(n);
        HeldClass::ALL
            .iter()
            .filter(|c| held.get(**c) > 0)
            .map(|c| (c.name(), held.get(*c)))
            .collect()
    }

    #[test]
    fn directories_split_what_they_retain_by_holder() {
        let mut tree = scope(simple());
        let snapshot = DataBackref::Direct {
            root: 300,
            objectid: 259,
            offset: 0,
            count: 1,
        };
        let (bytenr, mut kept) = owned(20 * MIB, MIB, &[(259, 1)]);
        kept.backrefs.push(snapshot);
        kept.refs += 1;
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(257, 1), (999, 1)]), (bytenr, kept)]);
        let agg = account(&mut tree, &refs);
        assert_eq!(held(&tree, &agg, "/v/dir"), [("snapshot", MIB)]);
        assert_eq!(
            held(&tree, &agg, "/v"),
            [("snapshot", MIB), ("reflink", MIB)]
        );
        assert_eq!(held(&tree, &agg, "/v/a"), [], "files keep no split");
    }

    #[test]
    fn links_inside_a_directory_hold_bytes_only_below_their_common_ancestor() {
        let mut tree = scope(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "d1", 257),
            entry(256, 3, "d2", 258),
            inode(257, DIR, 0, 1),
            entry(257, 2, "x", 259),
            entry(257, 3, "h", 261),
            inode(258, DIR, 0, 1),
            entry(258, 2, "y", 260),
            entry(258, 3, "h", 261),
            inode(259, FILE, MIB, 1),
            whole_extent(259, 0, 10 * MIB, MIB),
            inode(260, FILE, MIB, 1),
            whole_extent(260, 0, 10 * MIB, MIB),
            inode(261, FILE, MIB, 2),
            whole_extent(261, 0, 20 * MIB, MIB),
        ]);
        let refs = BTreeMap::from([
            owned(10 * MIB, MIB, &[(259, 1), (260, 1)]),
            owned(20 * MIB, MIB, &[(261, 1)]),
        ]);
        let agg = account(&mut tree, &refs);
        for dir in ["/v/d1", "/v/d2"] {
            assert_eq!(
                held(&tree, &agg, dir),
                [("hard_link", MIB), ("reflink", MIB)],
                "{dir}"
            );
        }
        assert_eq!(held(&tree, &agg, "/v"), []);
    }

    #[test]
    fn bytes_whose_other_links_are_all_in_a_recycle_bin_are_held_by_it() {
        let mut tree = scope(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "p", 257),
            inode(257, DIR, 0, 1),
            entry(257, 2, "2019", 258),
            entry(257, 3, "#recycle", 259),
            inode(258, DIR, 0, 1),
            entry(258, 2, "x", 260),
            inode(259, DIR, 0, 1),
            entry(259, 2, "x", 260),
            inode(260, FILE, MIB, 2),
            whole_extent(260, 0, 10 * MIB, MIB),
        ]);
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(260, 1)])]);
        let agg = account(&mut tree, &refs);
        assert_eq!(held(&tree, &agg, "/v/p/2019"), [("recycle_bin", MIB)]);
        assert_eq!(held(&tree, &agg, "/v/p/#recycle"), [("hard_link", MIB)]);
        assert_eq!(held(&tree, &agg, "/v/p"), []);
    }

    /// `/v/share` holding plain files, an Active Backup destination
    /// (`abb`, with a Hyper Backup folder deeper inside) and a Hyper
    /// Backup folder; `/v/other` beside it. `docs/r` reflinks an extent of
    /// the backup data, `docs/h` is a hard link into it and `docs/q`
    /// reflinks a file of the Hyper Backup folder in `inner`.
    fn package_items(cut: &[&str]) -> Vec<TreeItem> {
        let dirent = |parent, index, name: &'static str, child| {
            (!cut.contains(&name)).then(|| entry(parent, index, name, child))
        };
        let file = |ino, bytenr| {
            vec![
                inode(ino, FILE, MIB, 1),
                whole_extent(ino, 0, bytenr * MIB, MIB),
            ]
        };
        let mut items = vec![inode(256, DIR, 0, 1), inode(257, DIR, 0, 1)];
        items.extend(
            [
                dirent(256, 2, "share", 257),
                dirent(256, 3, "other", 270),
                dirent(257, 2, "docs", 258),
                dirent(257, 3, "abb", 262),
                dirent(257, 4, "b.hbk", 268),
                dirent(258, 2, "a", 259),
                dirent(258, 3, "r", 260),
                dirent(258, 4, "h", 261),
                dirent(262, 2, "@ActiveBackup", 263),
                dirent(262, 3, "h", 261),
                dirent(262, 4, "u", 267),
                dirent(263, 2, "blob", 264),
                dirent(263, 3, "c.hbk", 265),
                dirent(265, 2, "f", 266),
                dirent(268, 2, "v", 269),
                dirent(270, 2, "z", 271),
                dirent(257, 5, "inner", 272),
                dirent(272, 2, "x.hbk", 273),
                dirent(273, 2, "y", 274),
                dirent(258, 5, "q", 275),
            ]
            .into_iter()
            .flatten(),
        );
        for dir in [258, 262, 263, 265, 268, 270, 272, 273] {
            items.push(inode(dir, DIR, 0, 1));
        }
        for (ino, bytenr) in [
            (259, 10),
            (260, 20),
            (266, 50),
            (267, 60),
            (269, 70),
            (271, 80),
        ] {
            items.extend(file(ino, bytenr));
        }
        items.extend([
            inode(261, FILE, MIB, 2),
            whole_extent(261, 0, 30 * MIB, MIB),
            inode(264, FILE, 2 * MIB, 1),
            whole_extent(264, 0, 20 * MIB, MIB),
            whole_extent(264, MIB, 40 * MIB, MIB),
            inode(274, FILE, MIB, 1),
            whole_extent(274, 0, 90 * MIB, MIB),
            inode(275, FILE, MIB, 1),
            whole_extent(275, 0, 90 * MIB, MIB),
        ]);
        items
    }

    fn package_refs() -> BTreeMap<u64, ExtentRefs> {
        BTreeMap::from([
            owned(10 * MIB, MIB, &[(259, 1)]),
            owned(20 * MIB, MIB, &[(260, 1), (264, 1)]),
            owned(30 * MIB, MIB, &[(261, 1)]),
            owned(40 * MIB, MIB, &[(264, 1), (999, 1)]),
            owned(50 * MIB, MIB, &[(266, 1)]),
            owned(70 * MIB, MIB, &[(269, 1)]),
            owned(80 * MIB, MIB, &[(271, 1)]),
            owned(90 * MIB, MIB, &[(274, 1), (275, 1)]),
        ])
    }

    /// Each node's values, with the package data below it left out where
    /// the accountant keeps that apart.
    fn outside_values(tree: &ScopeTree, agg: &Aggregate) -> BTreeMap<Vec<u8>, (Metrics, Held)> {
        (1..tree.len() as u32)
            .map(|n| {
                let whole = (agg.metrics[n as usize], agg.held.get(n));
                let outside = agg.outside(n).map(|o| (o.metrics, o.held));
                (tree.path(n), outside.unwrap_or(whole))
            })
            .collect()
    }

    #[test]
    fn values_without_package_data_are_those_of_a_scope_without_it() {
        let refs = package_refs();
        let mut whole = scope(package_items(&[]));
        let whole_agg = account(&mut whole, &refs);
        let outside = outside_values(&whole, &whole_agg);
        let checks: [(&[&str], &[&str]); 2] = [
            (
                &["abb", "b.hbk", "x.hbk"],
                &[
                    "/v",
                    "/v/share",
                    "/v/share/docs",
                    "/v/share/inner",
                    "/v/other",
                ],
            ),
            (&["c.hbk"], &["/v/share/abb", "/v/share/abb/@ActiveBackup"]),
        ];
        for (cut, compared) in checks {
            let mut without = scope(package_items(cut));
            let without_agg = account(&mut without, &refs);
            let expected = outside_values(&without, &without_agg);
            for path in compared {
                let path = path.as_bytes().to_vec();
                assert_eq!(outside[&path], expected[&path], "{path:?} without {cut:?}");
            }
        }
        let node = |path: &str| {
            (0..whole.len() as u32)
                .find(|&n| whole.path(n) == path.as_bytes())
                .unwrap()
        };
        let share = node("/v/share");
        let v = whole_agg.metrics[share as usize];
        assert!(whole_agg.outside(share).unwrap().metrics.physical < v.physical);
        assert!(whole_agg.outside(node("/v/share/docs")).is_none());
    }

    /// `/v/share` holding `docs` and a `#recycle` with a deeper bin
    /// inside it. `docs/h` is a hard link into the bin and `docs/r`
    /// reflinks a file in it.
    fn recycle_items(cut: &[&str]) -> Vec<TreeItem> {
        let dirent = |parent, index, name: &'static str, child| {
            (!cut.contains(&name)).then(|| entry(parent, index, name, child))
        };
        let mut items = vec![inode(256, DIR, 0, 1), inode(257, DIR, 0, 1)];
        items.extend(
            [
                dirent(256, 2, "share", 257),
                dirent(257, 2, "docs", 258),
                dirent(257, 3, "#recycle", 259),
                dirent(258, 2, "a", 260),
                dirent(258, 3, "h", 262),
                dirent(258, 4, "r", 263),
                dirent(259, 2, "old", 261),
                dirent(259, 3, "h", 262),
                dirent(259, 4, "sub", 264),
                dirent(264, 2, "#recycle", 265),
                dirent(265, 2, "deep", 266),
            ]
            .into_iter()
            .flatten(),
        );
        for dir in [258, 259, 264, 265] {
            items.push(inode(dir, DIR, 0, 1));
        }
        for (ino, bytenr) in [(260, 10), (261, 20), (263, 20), (266, 40)] {
            items.extend([
                inode(ino, FILE, MIB, 1),
                whole_extent(ino, 0, bytenr * MIB, MIB),
            ]);
        }
        items.extend([
            inode(262, FILE, MIB, 2),
            whole_extent(262, 0, 30 * MIB, MIB),
        ]);
        items
    }

    #[test]
    fn values_without_package_data_leave_out_a_recycle_bin_below() {
        let refs = BTreeMap::from([
            owned(10 * MIB, MIB, &[(260, 1)]),
            owned(20 * MIB, MIB, &[(261, 1), (263, 1)]),
            owned(30 * MIB, MIB, &[(262, 1)]),
            owned(40 * MIB, MIB, &[(266, 1)]),
        ]);
        let mut whole = scope(recycle_items(&[]));
        let whole_agg = account(&mut whole, &refs);
        let outside = outside_values(&whole, &whole_agg);
        let mut without = scope(recycle_items(&["#recycle"]));
        let without_agg = account(&mut without, &refs);
        let expected = outside_values(&without, &without_agg);
        for path in ["/v", "/v/share"] {
            let path = path.as_bytes().to_vec();
            assert_eq!(outside[&path], expected[&path], "{path:?}");
        }
        let docs = (0..whole.len() as u32)
            .find(|&n| whole.path(n) == b"/v/share/docs")
            .unwrap();
        assert!(whole_agg.outside(docs).is_none(), "holds no Recycle Bin");
    }

    #[test]
    fn links_in_two_recycle_bins_hold_each_other_as_recycle_bin_bytes() {
        let mut tree = scope(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "a", 257),
            entry(256, 3, "b", 258),
            inode(257, DIR, 0, 1),
            entry(257, 2, "#recycle", 259),
            inode(258, DIR, 0, 1),
            entry(258, 2, "#recycle", 260),
            inode(259, DIR, 0, 1),
            entry(259, 2, "x", 261),
            inode(260, DIR, 0, 1),
            entry(260, 2, "x", 261),
            inode(261, FILE, MIB, 2),
            whole_extent(261, 0, 10 * MIB, MIB),
        ]);
        let refs = BTreeMap::from([owned(10 * MIB, MIB, &[(261, 1)])]);
        let agg = account(&mut tree, &refs);
        for dir in ["/v/a", "/v/a/#recycle", "/v/b", "/v/b/#recycle"] {
            assert_eq!(held(&tree, &agg, dir), [("recycle_bin", MIB)], "{dir}");
            assert_eq!(at(&tree, &agg, dir).1, Blockers::HARD_LINK, "{dir}");
        }
        assert_eq!(held(&tree, &agg, "/v"), []);
    }

    #[test]
    fn unknown_bytes_are_in_no_holder_class() {
        let mut tree = scope(simple());
        let agg = account(&mut tree, &BTreeMap::new());
        assert_eq!(held(&tree, &agg, "/v"), []);
        assert_eq!(at(&tree, &agg, "/v").0.unknown, 3 * MIB);
    }

    #[test]
    fn unreferenced_parts_of_reclaimable_extents_are_pinned() {
        // Bookend: the file keeps the first and last quarter of 1 MiB.
        let part = |file_offset, offset| {
            uncompressed(extent_item(
                257,
                file_offset,
                REGULAR,
                DiskExtent {
                    disk_bytenr: 10 * MIB,
                    disk_num_bytes: MIB,
                    offset,
                    num_bytes: 256 * KIB,
                },
            ))
        };
        let mut tree = scope(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "a", 257),
            inode(257, FILE, MIB, 1),
            part(0, 0),
            part(768 * KIB, 768 * KIB),
        ]);
        let refs = BTreeMap::from([(
            10 * MIB,
            ExtentRefs {
                num_bytes: MIB,
                refs: 2,
                generation: 1,
                backrefs: vec![DataBackref::Direct {
                    root: TREE,
                    objectid: 257,
                    offset: 0,
                    count: 2,
                }],
                malformed: false,
            },
        )]);
        let agg = account(&mut tree, &refs);
        let (a, _) = at(&tree, &agg, "/v/a");
        assert_eq!((a.reclaimable, a.pinned), (MIB, 512 * KIB));
    }

    #[test]
    fn without_references_only_footprint_is_aggregated() {
        let mut tree = scope(simple());
        tree.prepare();
        let mut accountant = Accountant::new(&tree, None);
        for bytenr in accountant.candidates() {
            accountant.extent(bytenr, None);
        }
        let agg = accountant.finish();
        let v = agg.metrics[0];
        assert_eq!((v.physical, v.reclaimable, v.unknown), (3 * MIB, 0, 0));
        assert!(agg.blockers.iter().all(|b| b.is_empty()));
    }

    const SHARE: u64 = 300;

    /// `/v` with subvolume `share` linked; `/v/a` and `/v/share/f` hold
    /// extent 10 MiB, `/v/dir/b` holds 20 and 30 MiB.
    fn nested() -> (Namespace, Namespace, SubvolumeSet) {
        let mut items = simple();
        items.push(subvolume_entry(256, 9, "share", SHARE));
        let parent = read_tree(TREE, items).unwrap();
        let child = read_tree(
            SHARE,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "f", 257),
                inode(257, FILE, MIB, 1),
                whole_extent(257, 0, 10 * MIB, MIB),
            ],
        )
        .unwrap();
        let subvolumes = SubvolumeSet {
            subvolumes: vec![linked(SHARE, TREE, 256, "share")],
            ..SubvolumeSet::default()
        };
        (parent, child, subvolumes)
    }

    fn nested_refs() -> BTreeMap<u64, ExtentRefs> {
        let (bytenr, mut shared) = owned(10 * MIB, MIB, &[(257, 1)]);
        shared.backrefs.push(DataBackref::Direct {
            root: SHARE,
            objectid: 257,
            offset: 0,
            count: 1,
        });
        shared.refs = 2;
        BTreeMap::from([
            (bytenr, shared),
            owned(20 * MIB, MIB, &[(259, 1)]),
            owned(30 * MIB, MIB, &[(259, 1)]),
        ])
    }

    fn parent_scope(parent: &Namespace, subvolumes: &SubvolumeSet) -> (ScopeTree, Pending) {
        let mut tree = ScopeTree::new();
        let pending = tree
            .add_root(
                parent,
                256,
                b"/v",
                RootKind::Volume,
                subvolumes,
                &Marks::default(),
            )
            .unwrap();
        (tree, pending[0])
    }

    #[test]
    fn spilled_refs_give_the_uses_held_refs_do() {
        let mut items = simple();
        items.retain(|(key, _)| key.objectid != 257 || key.item_type != INODE_ITEM_KEY);
        items.push(inode(257, FILE, MIB, 2));
        items.push(entry(256, 4, "again", 257));
        let scope = |ns: &Namespace| {
            let mut tree = ScopeTree::new();
            let (subvolumes, marks) = (SubvolumeSet::default(), Marks::default());
            tree.add_root(ns, 256, b"/v", RootKind::Volume, &subvolumes, &marks)
                .unwrap();
            tree.prepare();
            tree
        };
        let held = scope(&read_tree(TREE, items.clone()).unwrap());
        let expected: Vec<HeldUse> = held.held_uses().collect();
        assert!(expected.iter().any(|u| u.several_names));

        let path = scratch("spilled").join("refs");
        let mut source = FakeSource::new(items, 3);
        let sharing = LeafSharing::default();
        let ns = Namespace::read_spilling(&mut source, TREE, sharing, &mut NoStop, path.clone())
            .unwrap();
        let tree = scope(&ns);
        assert_eq!(tree.held_uses().count(), 0);
        let mut uses = Vec::new();
        tree.spilled_uses(SpilledRefs::new(path), |u| {
            uses.push(u);
            Ok(())
        })
        .unwrap();
        uses.sort_by_key(|u| u.extent);
        assert_eq!(uses, expected);
    }

    /// `/v` with the share of `nested()` whose tree holds `f` and an
    /// Active Backup destination: in `abb`, or at the share root.
    fn package_share(at_root: bool) -> (ScopeTree, Pending, ScopeTree) {
        let (parent_ns, _, subvolumes) = nested();
        let (parent, boundary) = parent_scope(&parent_ns, &subvolumes);
        let mut items = vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "f", 257),
            inode(257, FILE, MIB, 1),
            whole_extent(257, 0, 10 * MIB, MIB),
            inode(258, DIR, 0, 1),
            entry(258, 2, "@ActiveBackup", 259),
            inode(259, DIR, 0, 1),
            entry(259, 2, "g", 260),
            inode(260, FILE, 2 * MIB, 1),
            whole_extent(260, 0, 40 * MIB, 2 * MIB),
        ];
        items.push(match at_root {
            true => entry(256, 3, "@ActiveBackup", 259),
            false => entry(256, 3, "abb", 258),
        });
        let child_ns = read_tree(SHARE, items).unwrap();
        let mut child = ScopeTree::new();
        child.add_subvolume_root(
            &child_ns,
            b"share",
            parent.node(boundary.node).flags,
            &subvolumes,
            &Marks::default(),
        );
        (parent, boundary, child)
    }

    /// The outer scope's aggregate with the share's nested in it, and the
    /// share's own.
    fn account_nested(
        parent: &mut ScopeTree,
        boundary: &Pending,
        child: &mut ScopeTree,
    ) -> (Aggregate, Aggregate) {
        let none = BTreeMap::new();
        let inner = account(child, &none);
        parent.prepare();
        let mut outer = Accountant::new(parent, Some(&Roots));
        outer.nest(boundary.node, &inner.nested(child.roots()[0]));
        for bytenr in outer.candidates() {
            outer.extent(bytenr, None);
        }
        (outer.finish(), inner)
    }

    #[test]
    fn package_data_in_a_nested_scope_is_left_out_above_its_boundary() {
        let (mut parent, boundary, mut child) = package_share(false);
        let (outer, inner) = account_nested(&mut parent, &boundary, &mut child);
        let root = child.roots()[0];
        let nested = inner.nested(root);
        assert!(!nested.package);
        let excluded = nested.excluded.expect("the share holds package data");
        assert_eq!(
            (excluded.metrics.physical, excluded.metrics.entries),
            (2 * MIB, 3)
        );
        let v = at(&parent, &outer, "/v").0;
        let n = (0..parent.len() as u32)
            .find(|&n| parent.path(n) == b"/v")
            .unwrap();
        let outside = outer.outside(n).unwrap().metrics;
        assert_eq!(outside.physical, v.physical - 2 * MIB);
        assert_eq!(outside.unknown, v.unknown - 2 * MIB);
        assert_eq!(outside.entries, v.entries - 3);

        let (mut parent, boundary, mut child) = package_share(true);
        let (outer, inner) = account_nested(&mut parent, &boundary, &mut child);
        let root = child.roots()[0];
        assert!(inner.nested(root).package, "a share holding a marker");
        let share = inner.metrics[root as usize];
        let v = at(&parent, &outer, "/v").0;
        let outside = outer.outside(n).unwrap().metrics;
        assert_eq!(outside.physical, v.physical - share.physical);
        assert_eq!(outside.entries, v.entries - share.entries);
    }

    #[test]
    fn a_nested_scope_accounted_apart_matches_one_attached_tree() {
        let (parent_ns, child_ns, subvolumes) = nested();
        let refs = nested_refs();
        let (mut whole, pending) = parent_scope(&parent_ns, &subvolumes);
        whole.attach(&pending, &child_ns, &subvolumes, &Marks::default());
        let expected = account(&mut whole, &refs);

        let (mut parent, boundary) = parent_scope(&parent_ns, &subvolumes);
        let mut child = ScopeTree::new();
        child.add_subvolume_root(
            &child_ns,
            b"share",
            parent.node(boundary.node).flags,
            &subvolumes,
            &Marks::default(),
        );
        parent.prepare();
        child.prepare();
        let shared: Vec<HeldUse> = parent
            .held_uses()
            .chain(child.held_uses())
            .filter(|u| u.extent.bytenr == 10 * MIB)
            .collect();
        let judgement = judge(&shared, refs.get(&(10 * MIB)), Some(&Roots));
        let inodes = |tree: &ScopeTree| -> Vec<u32> {
            let uses = tree.held_uses().filter(|u| u.extent.bytenr == 10 * MIB);
            uses.map(|u| u.extent.inode).collect()
        };
        let mut inner = Accountant::new(&child, Some(&Roots));
        inner.attribute(
            10 * MIB,
            &Attribution {
                judgement,
                inodes: &inodes(&child),
                children: &[],
                owner: Owner::Above,
            },
        );
        let inner = inner.finish();
        let mut outer = Accountant::new(&parent, Some(&Roots));
        outer.nest(boundary.node, &inner.nested(child.roots()[0]));
        outer.attribute(
            10 * MIB,
            &Attribution {
                judgement,
                inodes: &inodes(&parent),
                children: &[boundary.node],
                owner: Owner::Here,
            },
        );
        for bytenr in outer.candidates() {
            outer.extent(bytenr, refs.get(&bytenr));
        }
        let outer = outer.finish();

        for path in ["/v", "/v/a", "/v/dir"] {
            assert_eq!(at(&parent, &outer, path), at(&whole, &expected, path));
        }
        assert_eq!(outer.metrics[0], expected.metrics[0]);
        assert_eq!(
            at(&child, &inner, "share"),
            at(&whole, &expected, "/v/share")
        );
        assert_eq!(
            at(&child, &inner, "share/f"),
            at(&whole, &expected, "/v/share/f")
        );
        assert_eq!(at(&whole, &expected, "/v").0.reclaimable, 3 * MIB);
        for path in ["/v", "/v/dir"] {
            assert_eq!(held(&parent, &outer, path), held(&whole, &expected, path));
        }
        assert_eq!(held(&whole, &expected, "/v"), []);
        assert_eq!(held(&child, &inner, "share"), [("reflink", MIB)]);
        assert_eq!(held(&whole, &expected, "/v/share"), [("reflink", MIB)]);
    }
}
