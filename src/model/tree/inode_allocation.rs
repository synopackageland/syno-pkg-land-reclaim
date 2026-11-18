//! VFS inode allocations adapted to the existing tree and hard-link accountant.
//! Allocation identities are (device, inode), never physical extent addresses.

use super::*;
use crate::overview::PartKind;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AllocationOverview {
    pub items: Vec<AllocationItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AllocationItem {
    pub kind: PartKind,
    pub name_hex: String,
    pub service: Option<u16>,
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    pub unknown_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeAllocation {
    pub device: u64,
    pub inode: u64,
    pub kind: FileKind,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub links: u32,
    pub ctime: u64,
    pub uncertain: bool,
}

impl InodeAllocation {
    pub fn judgement(&self, observed_names: u64) -> Judgement {
        let verdict = if self.uncertain || observed_names == 0 || self.links == 0 {
            Verdict::Unknown
        } else if self.kind != FileKind::Directory && observed_names != u64::from(self.links) {
            Verdict::Retained(Blockers::HARD_LINK)
        } else {
            Verdict::Reclaimable
        };
        Judgement {
            disk_bytes: self.allocated_bytes,
            verdict: Some(verdict),
            local: if observed_names > 1 {
                Blockers::HARD_LINK
            } else {
                Blockers::default()
            },
            pinned: 0,
        }
    }

    /// The existing accountant's contribution to one holding subtree. The
    /// streaming adapter needs only whether this subtree contains every name,
    /// whether any names are unseen, and what holds the remaining names. A
    /// constant-size representative tree preserves the same hard-link and
    /// Recycle Bin rules without retaining all of an inode's paths in RAM.
    pub fn subtree_totals(
        &self,
        seen: u64,
        inside: u64,
        remaining_recycled: bool,
        directory: bool,
    ) -> (Metrics, Held, Blockers) {
        let complete = self.judgement(seen).verdict == Some(Verdict::Reclaimable);
        // The usual case has every name inside this subtree. Avoid building
        // a representative tree twice per inode per ancestor on large ext4
        // volumes. Partial/uncertain and split hard links retain the general
        // accountant below, including its Recycle Bin attribution.
        if complete && inside == seen {
            return (
                Metrics {
                    logical: if self.kind == FileKind::Regular {
                        self.logical_bytes
                    } else {
                        0
                    },
                    physical: self.allocated_bytes,
                    reclaimable: self.allocated_bytes,
                    ..Metrics::default()
                },
                Held::default(),
                Blockers::default(),
            );
        }
        let aliases = inside < seen;
        let mut allocation = *self;
        allocation.links = if complete {
            if aliases { 2 } else { 1 }
        } else {
            3
        };
        allocation.uncertain |= self.judgement(seen).verdict == Some(Verdict::Unknown);
        let mut tree = InodeTree::default();
        let parent = tree.add_inode_allocation(
            VIRTUAL_ROOT,
            b"here",
            InodeAllocation {
                device: self.device.wrapping_add(1),
                inode: 1,
                kind: FileKind::Directory,
                logical_bytes: 0,
                allocated_bytes: 0,
                links: 1,
                ctime: 0,
                uncertain: false,
            },
            false,
        );
        let leaf = tree.add_inode_allocation(parent, b"file", allocation, false);
        if aliases {
            let other = tree.add_inode_allocation(
                VIRTUAL_ROOT,
                b"there",
                InodeAllocation {
                    device: self.device.wrapping_add(1),
                    inode: 2,
                    kind: FileKind::Directory,
                    logical_bytes: 0,
                    allocated_bytes: 0,
                    links: 1,
                    ctime: 0,
                    uncertain: false,
                },
                false,
            );
            tree.add_inode_allocation(
                other,
                if remaining_recycled {
                    b"#recycle"
                } else {
                    b"file"
                },
                allocation,
                false,
            );
        }
        let aggregate = tree.account_inode_allocations();
        let node = if directory { parent } else { leaf };
        let mut metrics = aggregate.metrics[node as usize];
        metrics.entries = 0;
        metrics.files = 0;
        (
            metrics,
            aggregate.held.get(node),
            aggregate.blockers[node as usize],
        )
    }
}

#[derive(Debug, Default)]
pub struct InodeTree {
    tree: ScopeTree,
    allocations: Vec<InodeAllocation>,
    identities: HashMap<(u64, u64), u32>,
    volume_roots: HashSet<u32>,
}

impl InodeTree {
    pub fn into_tree(self) -> ScopeTree {
        self.tree
    }
    pub fn tree(&self) -> &ScopeTree {
        &self.tree
    }

    pub fn add_inode_allocation(
        &mut self,
        parent: u32,
        name: &[u8],
        allocation: InodeAllocation,
        volume_root: bool,
    ) -> u32 {
        let key = (allocation.device, allocation.inode);
        let index = match self.identities.get(&key) {
            Some(&index) => {
                let previous = &mut self.allocations[index as usize];
                if *previous != allocation {
                    previous.uncertain = true;
                    previous.allocated_bytes =
                        previous.allocated_bytes.max(allocation.allocated_bytes);
                }
                index
            }
            None => {
                let index = self.allocations.len() as u32;
                self.allocations.push(allocation);
                self.identities.insert(key, index);
                self.tree.inodes.push(TreeInode {
                    tree: allocation.device,
                    objectid: allocation.inode,
                    size: allocation.logical_bytes,
                    nlink: if allocation.kind == FileKind::Directory {
                        1
                    } else {
                        allocation.links
                    },
                    kind: allocation.kind,
                    inline_bytes: 0,
                });
                index
            }
        };
        let at_root = self.volume_roots.contains(&parent);
        let flags = if parent == VIRTUAL_ROOT {
            NodeFlags::default()
        } else {
            child_flags(self.tree.node(parent).flags, name, at_root)
        };
        if let Some(marker) = marker(name, at_root) {
            self.tree.mark(parent, NodeFlags::PACKAGE);
            self.tree.markers.insert(parent, marker);
        }
        let node = self
            .tree
            .push_node(parent, name, allocation.kind, flags, index);
        if parent == VIRTUAL_ROOT {
            self.tree.roots.push(node);
        }
        if volume_root {
            self.volume_roots.insert(node);
        }
        node
    }

    pub fn mark_uncertain(&mut self, node: u32) {
        let index = self.tree.node(node).inode;
        self.allocations[index as usize].uncertain = true;
    }

    /// A Recycle Bin belongs only to a DSM-listed, pinned share root.
    pub fn classify_shares(
        &mut self,
        shares: &BTreeMap<(u64, u64), Vec<u8>>,
        own_state: Option<(u64, u64)>,
    ) {
        for node in 1..self.tree.nodes.len() {
            let entry = self.tree.nodes[node];
            let inode = self.tree.inodes[entry.inode as usize];
            let identity = (inode.tree, inode.objectid);
            let parent = self.tree.nodes[entry.parent as usize];
            let parent_inode = self.tree.inode(parent.inode);
            let parent_share = parent_inode
                .is_some_and(|inode| shares.contains_key(&(inode.tree, inode.objectid)));
            let mut flags = NodeFlags::from_bits(entry.flags.bits() & !NodeFlags::RECYCLE.bits());
            if parent.flags.contains(NodeFlags::RECYCLE)
                || (parent_share && self.tree.name(node as u32) == b"#recycle")
            {
                flags = flags.with(NodeFlags::RECYCLE);
            }
            if parent.flags.contains(NodeFlags::OWN_STATE) || own_state == Some(identity) {
                flags = flags.with(NodeFlags::OWN_STATE);
            }
            self.tree.nodes[node].flags = flags;
        }
    }

    pub fn allocation_overview(
        &mut self,
        shares: &BTreeMap<(u64, u64), Vec<u8>>,
    ) -> AllocationOverview {
        self.tree.prepare();
        let mut items = BTreeMap::<(PartKind, Vec<u8>, Option<u16>), (u64, u64, u64)>::new();
        for (index, allocation) in self.allocations.iter().enumerate() {
            let names = self.tree.names_of(index as u32);
            let owners: BTreeSet<_> = names
                .iter()
                .map(|&(_, node)| self.allocation_owner(node, shares))
                .collect();
            let key = if allocation.uncertain {
                (PartKind::Unknown, Vec::new(), None)
            } else if owners.len() > 1 {
                (PartKind::SharedBetweenFolders, Vec::new(), None)
            } else {
                owners
                    .into_iter()
                    .next()
                    .unwrap_or((PartKind::Unknown, Vec::new(), None))
            };
            let complete = allocation.kind == FileKind::Directory
                || names.len() as u64 == u64::from(allocation.links);
            let row = items.entry(key).or_default();
            row.0 += allocation.allocated_bytes;
            if allocation.uncertain {
                row.2 += allocation.allocated_bytes;
            } else if complete && allocation.links > 0 {
                row.1 += allocation.allocated_bytes;
            }
        }
        AllocationOverview {
            items: items
                .into_iter()
                .map(
                    |((kind, name, service), (bytes, reclaimable_bytes, unknown_bytes))| {
                        AllocationItem {
                            kind,
                            name_hex: crate::report::hex_bytes::encode(&name),
                            service,
                            bytes,
                            reclaimable_bytes,
                            unknown_bytes,
                        }
                    },
                )
                .collect(),
        }
    }

    fn allocation_owner(
        &self,
        node: u32,
        shares: &BTreeMap<(u64, u64), Vec<u8>>,
    ) -> (PartKind, Vec<u8>, Option<u16>) {
        let flags = self.tree.node(node).flags;
        let mut current = node;
        let mut share = None;
        let mut service = None;
        let mut top_name = Vec::new();
        while current != VIRTUAL_ROOT {
            let entry = self.tree.node(current);
            let inode = self.tree.inodes[entry.inode as usize];
            if let Some(name) = shares.get(&(inode.tree, inode.objectid)) {
                share = Some(name.clone());
            }
            let at_root = self.volume_roots.contains(&entry.parent);
            if let Some(found) = dsm::service(
                self.tree.name(current),
                if at_root {
                    Depth::VolumeRoot
                } else {
                    Depth::Below
                },
            ) {
                service = Some(dsm::service_id(found));
            }
            if at_root {
                top_name = self.tree.name(current).to_vec();
            }
            current = entry.parent;
        }
        if let Some(service) = service {
            return (PartKind::DsmService, Vec::new(), Some(service));
        }
        if let Some(name) = share {
            return (
                if flags.contains(NodeFlags::RECYCLE) {
                    PartKind::RecycleBin
                } else {
                    PartKind::ShareLive
                },
                name,
                None,
            );
        }
        (PartKind::SystemOther, top_name, None)
    }

    pub fn account_inode_allocations(&mut self) -> Aggregate {
        self.tree.prepare();
        let mut accountant = Accountant::new(&self.tree, None);
        for (index, allocation) in self.allocations.iter().enumerate() {
            let names = self.tree.names_of(index as u32).len();
            let judgement = allocation.judgement(names as u64);
            accountant.summary.count(&judgement);
            accountant.apply(&Attribution {
                judgement,
                inodes: &[index as u32],
                children: &[],
                owner: Owner::Here,
            });
        }
        let mut result = accountant.finish();
        for metrics in &mut result.metrics {
            metrics.extents = 0;
        }
        for excluded in result.excluded.values_mut() {
            excluded.metrics.extents = 0;
        }
        result
    }
}
