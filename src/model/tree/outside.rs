//! Directory values with the package data below them left out (SPEC 5.4
//! top rows): a ranked folder holding an Active Backup destination or a
//! Hyper Backup folder shows what it holds apart from that data, which
//! its service row already counts. A `#recycle` below it is left out the
//! same way, since the Recycle Bin is a row of its own.
//!
//! Only directories above package data (the "slotted" ones) differ from
//! their full values. For each, the accountant keeps what its full values
//! include of package data strictly below it; values without it are the
//! full values minus that. A holder inside package data counts up to the
//! package root it lies in (its cut), not above.

use super::{Metrics, NO_PARENT, Node, NodeFlags};
use crate::model::accounting::{Held, HeldClass, Judgement, Verdict};
use crate::model::namespace::FileKind;
use std::collections::{HashMap, HashSet};

/// What a directory's values include of package data strictly below it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Excluded {
    pub metrics: Metrics,
    pub held: Held,
}

impl Excluded {
    /// Full directory values without what they include of package data.
    pub fn leave_out(&self, mut metrics: Metrics, mut held: Held) -> Outside {
        metrics.sub(&self.metrics);
        held.sub(&self.held);
        Outside { metrics, held }
    }

    fn add(&mut self, other: &Excluded) {
        self.metrics.add(&other.metrics);
        self.held.add(&other.held);
    }
}

/// A directory's values with the package data strictly below it left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outside {
    pub metrics: Metrics,
    pub held: Held,
}

/// Where an extent's reclaimable bytes count in its scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Freed {
    /// At this owning node and above.
    At(u32),
    /// Above the scope.
    Above,
}

/// One extent as it applies to the directories above package data.
pub(super) struct PackageExtent<'a> {
    pub judgement: &'a Judgement,
    /// Every holding node, nested boundaries included.
    pub holders: &'a [u32],
    pub children: &'a [u32],
    pub freed: Freed,
    /// Whether every holding node lies in a `#recycle`.
    pub all_recycled: bool,
}

const NO_CUT: u32 = u32::MAX;

pub(super) struct Packages<'a> {
    nodes: &'a [Node],
    /// The lowest package root at or above each node, or `NO_CUT`.
    cut: Vec<u32>,
    /// Boundaries whose nested scope is itself package data.
    nested: HashSet<u32>,
    slots: HashMap<u32, usize>,
    excluded: Vec<Excluded>,
    /// Per slot: the colour of the last extent held below it.
    covered: Vec<u32>,
    /// Per slot: the colour of the last extent a holder reaching it held.
    reached: Vec<u32>,
    color: u32,
    /// The nodes and slots covered by the current extent.
    touched: Vec<(u32, usize)>,
}

impl<'a> Packages<'a> {
    /// `None` when no node of `nodes` starts package data.
    pub fn of(nodes: &'a [Node]) -> Option<Self> {
        if !any_left_out(nodes) {
            return None;
        }
        let mut packages = Packages::none(nodes);
        for (i, node) in nodes.iter().enumerate() {
            let above = match node.parent {
                NO_PARENT => NO_CUT,
                parent => packages.cut[parent as usize],
            };
            let starts = left_out_at(nodes, node);
            packages.cut[i] = if starts { i as u32 } else { above };
            if starts {
                packages.slot_above(i as u32);
            }
        }
        packages.exclude_entries();
        Some(packages)
    }

    /// Tracks no package data of its own; nested scopes may add some.
    pub fn none(nodes: &'a [Node]) -> Self {
        Packages {
            nodes,
            cut: vec![NO_CUT; nodes.len()],
            nested: HashSet::new(),
            slots: HashMap::new(),
            excluded: Vec::new(),
            covered: Vec::new(),
            reached: Vec::new(),
            color: 0,
            touched: Vec::new(),
        }
    }

    /// Bytes `of` keeps per node of a scope with package data.
    pub fn bytes_per_node() -> usize {
        size_of::<u32>()
    }

    /// Gives every strict ancestor of `node` a slot.
    fn slot_above(&mut self, node: u32) {
        let mut at = self.nodes[node as usize].parent;
        while at != NO_PARENT && !self.slots.contains_key(&at) {
            self.slots.insert(at, self.excluded.len());
            self.excluded.push(Excluded::default());
            self.covered.push(0);
            self.reached.push(0);
            at = self.nodes[at as usize].parent;
        }
    }

    /// Adds `value` to every strict ancestor of `node`.
    fn exclude_above(&mut self, node: u32, value: &Excluded) {
        let mut at = self.nodes[node as usize].parent;
        while at != NO_PARENT {
            self.excluded[self.slots[&at]].add(value);
            at = self.nodes[at as usize].parent;
        }
    }

    /// Entries in package data count only up to their package root.
    fn exclude_entries(&mut self) {
        let mut counts: HashMap<u32, Metrics> = HashMap::new();
        for (node, &cut) in self.nodes.iter().zip(&self.cut) {
            if cut != NO_CUT {
                let count = counts.entry(cut).or_default();
                count.entries += 1;
                count.files += u64::from(node.kind == FileKind::Regular);
            }
        }
        for (root, metrics) in counts {
            let value = Excluded {
                metrics,
                held: Held::default(),
            };
            self.exclude_above(root, &value);
        }
    }

    /// Counts the nested scope at boundary `node`: all of it when it is
    /// package data, else the package data it holds.
    pub fn nest(&mut self, node: u32, nested: &super::Nested) {
        let value = if nested.package {
            self.nested.insert(node);
            Excluded {
                metrics: nested.metrics,
                held: nested.held,
            }
        } else {
            match nested.excluded {
                Some(excluded) => excluded,
                None => return,
            }
        };
        self.slot_above(node);
        self.exclude_above(node, &value);
    }

    /// Leaves an inode's logical bytes out where none of its names
    /// reaches.
    pub fn inode(&mut self, names: &[u32], value: &Metrics) {
        if !self.touches(names, &[]) {
            return;
        }
        self.cover(names);
        let value = Excluded {
            metrics: *value,
            held: Held::default(),
        };
        self.exclude_unreached(&value);
    }

    /// Leaves one extent out where it is package data's alone.
    pub fn extent(&mut self, at: &PackageExtent<'_>) {
        if !self.touches(at.holders, at.children) {
            return;
        }
        self.cover(at.holders);
        let judgement = at.judgement;
        let disk = judgement.disk_bytes;
        let mut alone = Metrics {
            physical: disk,
            extents: 1,
            ..Metrics::default()
        };
        let mut held = Held::default();
        match (judgement.verdict, at.freed) {
            (Some(Verdict::Unknown), _) => alone.unknown = disk,
            (Some(Verdict::Retained(blockers)), _) => held = held_as(blockers, disk),
            (Some(Verdict::Reclaimable), Freed::Above) => held = held_as(judgement.local, disk),
            (Some(Verdict::Reclaimable), Freed::At(owner)) => {
                self.exclude_freed(at, owner);
                if let Some(class) = HeldClass::of(judgement.local) {
                    let class = if at.all_recycled {
                        HeldClass::RecycleBin
                    } else {
                        class
                    };
                    self.exclude_held_below(owner, &Held::of(class, disk));
                }
            }
            (None, _) => {}
        }
        let value = Excluded {
            metrics: alone,
            held,
        };
        self.exclude_unreached(&value);
    }

    /// Adds `value` to the slots the current extent covers but no holder
    /// reaches.
    fn exclude_unreached(&mut self, value: &Excluded) {
        for i in 0..self.touched.len() {
            let (_, slot) = self.touched[i];
            if self.reached[slot] != self.color {
                self.excluded[slot].add(value);
            }
        }
    }

    /// Whether package data holds any of `holders`, or a nested scope of
    /// package data any of `children`.
    fn touches(&self, holders: &[u32], children: &[u32]) -> bool {
        holders.iter().any(|&h| self.cut[h as usize] != NO_CUT)
            || children.iter().any(|c| self.nested.contains(c))
    }

    /// Stamps, in a new colour, the slots with a holder below them and
    /// those a holder reaches; `touched` lists the first.
    fn cover(&mut self, holders: &[u32]) {
        self.color = self.color.wrapping_add(1);
        if self.color == 0 {
            self.covered.fill(0);
            self.reached.fill(0);
            self.color = 1;
        }
        self.touched.clear();
        for &holder in holders {
            let cut = self.cut[holder as usize];
            let mut reaches = true;
            let mut at = holder;
            while at != NO_PARENT {
                if let Some(&slot) = self.slots.get(&at) {
                    if self.covered[slot] != self.color {
                        self.covered[slot] = self.color;
                        self.touched.push((at, slot));
                    }
                    if reaches {
                        self.reached[slot] = self.color;
                    }
                }
                if at == cut {
                    reaches = false;
                }
                at = self.nodes[at as usize].parent;
            }
        }
    }

    /// Reclaimable bytes owned at `owner` count at `owner` and above
    /// only where every holder reaches; above that, a holder that still
    /// reaches keeps them as its local blocker's bytes.
    fn exclude_freed(&mut self, at: &PackageExtent<'_>, owner: u32) {
        let judgement = at.judgement;
        let mut chain = Vec::new();
        let mut node = owner;
        while node != NO_PARENT {
            chain.push(node);
            node = self.nodes[node as usize].parent;
        }
        let cuts: Vec<u32> = at
            .holders
            .iter()
            .map(|&h| self.cut[h as usize])
            .filter(|&c| c != NO_CUT)
            .collect();
        let nested = at.children.iter().any(|c| self.nested.contains(c));
        let start = if nested || cuts.iter().any(|c| !chain.contains(c)) {
            0
        } else {
            match chain.iter().position(|n| cuts.contains(n)) {
                Some(cut) => cut + 1,
                None => return,
            }
        };
        let freed = Metrics {
            reclaimable: judgement.disk_bytes,
            pinned: judgement.pinned,
            ..Metrics::default()
        };
        let kept = HeldClass::of(judgement.local)
            .filter(|_| !nested)
            .map(|class| Held::of(class, judgement.disk_bytes));
        for node in &chain[start..] {
            let Some(&slot) = self.slots.get(node) else {
                continue;
            };
            self.excluded[slot].metrics.add(&freed);
            if let Some(kept) = &kept
                && self.reached[slot] == self.color
            {
                self.excluded[slot].held.sub(kept);
            }
        }
    }

    /// Held bytes of an extent freed at `owner` count in the directories
    /// strictly below the owner's.
    fn exclude_held_below(&mut self, owner: u32, value: &Held) {
        let node = &self.nodes[owner as usize];
        let mut at = if node.kind == FileKind::Directory {
            owner
        } else {
            node.parent
        };
        let mut above = HashSet::new();
        while at != NO_PARENT {
            above.insert(at);
            at = self.nodes[at as usize].parent;
        }
        for i in 0..self.touched.len() {
            let (node, slot) = self.touched[i];
            if self.reached[slot] != self.color && !above.contains(&node) {
                self.excluded[slot].held.add(value);
            }
        }
    }

    /// What each slotted node's values include of package data below it.
    pub fn finish(self) -> HashMap<u32, Excluded> {
        self.slots
            .into_iter()
            .map(|(node, slot)| (node, self.excluded[slot]))
            .collect()
    }
}

/// Whether any node of `nodes` starts data ranked folders leave out.
pub(super) fn any_left_out(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| left_out_at(nodes, node))
}

/// Whether ranked folders leave out `node` and all below it: package data
/// (SPEC 5.7), or a Recycle Bin, which its own row counts. A `#recycle`
/// inside another one lies in that one's row.
fn left_out_at(nodes: &[Node], node: &Node) -> bool {
    let recycled = |n: &Node| n.flags.contains(NodeFlags::RECYCLE);
    node.flags.contains(NodeFlags::PACKAGE)
        || node.flags.contains(NodeFlags::OWN_STATE)
        || (recycled(node) && node.parent != NO_PARENT && !recycled(&nodes[node.parent as usize]))
}

fn held_as(blockers: crate::model::accounting::Blockers, bytes: u64) -> Held {
    HeldClass::of(blockers)
        .map(|class| Held::of(class, bytes))
        .unwrap_or_default()
}
