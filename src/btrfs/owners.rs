//! Owner resolution for shared tree blocks (SPEC 8.1 step 3, 5.14 step
//! 2): from a leaf named by a `SHARED_DATA_REF`, follow `SHARED_BLOCK_REF`
//! edges up the extent tree to the `TREE_BLOCK_REF`s at their tops. The
//! roots found there, and the roots sharing their paths, are the only
//! trees that can reach the leaf.

use crate::btrfs::backref::*;
use crate::btrfs::keys::Key;
use crate::btrfs::search::{Item, SearchRange};
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::WalkStats;
use crate::btrfs::walk::{BatchBoundary, StopReason, WalkError, walk_tree};
use std::collections::{BTreeSet, HashMap};
use std::ops::ControlFlow;

/// Resolved blocks kept before the memo starts over, bounding its memory.
const MEMO_CAPACITY: usize = 1 << 18;
/// Conservative per-slot allowance for hash-table slack and allocation overhead.
const MEMO_SLOT_OVERHEAD_BYTES: u64 = 16;
/// Maximum extent-tree block lookups across a snapshot analysis by default.
pub const DEFAULT_GLOBAL_OWNER_BUDGET: u64 = 100_000;

/// How one block resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolution {
    /// The roots at the tops of its backref chains.
    Roots(Vec<u64>),
    /// A block on the way is missing or malformed.
    Broken,
    /// The lookups this call may make ran out.
    OverBudget,
}

/// Walks tree-block backrefs, remembering each block it resolved.
#[derive(Debug)]
pub struct OwnerWalk {
    /// Tree-block lookups one call may make.
    budget: u32,
    /// Tree-block lookups all calls may make together.
    global_budget: u64,
    memo: HashMap<u64, Option<Vec<u64>>>,
    /// Extent-tree searches made so far.
    pub lookups: u64,
    /// Anchor queries stopped because they reached their lookup budget.
    pub budget_capped_extents: u64,
    pub stats: WalkStats,
    /// The most recent anchor query stopped at its configured budget.
    pub budget_exhausted: bool,
}

impl OwnerWalk {
    pub fn new(budget: u32) -> Self {
        Self::with_global_budget(budget, DEFAULT_GLOBAL_OWNER_BUDGET)
    }

    pub fn with_global_budget(budget: u32, global_budget: u64) -> Self {
        OwnerWalk {
            budget,
            global_budget,
            memo: HashMap::new(),
            lookups: 0,
            budget_capped_extents: 0,
            stats: WalkStats::default(),
            budget_exhausted: false,
        }
    }

    /// The roots at the tops of the backref chains of every block in
    /// `blocks`; `None` when a block is missing or malformed or when
    /// resolving them needs more than the budget of lookups.
    pub fn anchors<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        blocks: &[u64],
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Option<BTreeSet<u64>>, WalkError> {
        self.budget_exhausted = false;
        let mut left = self.budget;
        let mut roots = BTreeSet::new();
        for &block in blocks {
            match self.resolve(source, block, &mut left, &mut Vec::new(), boundary)? {
                Resolution::Roots(found) => roots.extend(found),
                Resolution::Broken => return Ok(None),
                Resolution::OverBudget => {
                    self.budget_exhausted = true;
                    self.budget_capped_extents += 1;
                    return Ok(None);
                }
            }
        }
        Ok(Some(roots))
    }

    fn resolve<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        block: u64,
        left: &mut u32,
        path: &mut Vec<u64>,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Resolution, WalkError> {
        if let Some(known) = self.memo.get(&block) {
            return Ok(known.clone().map_or(Resolution::Broken, Resolution::Roots));
        }
        if path.contains(&block) {
            return Ok(Resolution::Broken);
        }
        if self.lookups >= self.global_budget {
            return Ok(Resolution::OverBudget);
        }
        let Some(spent) = left.checked_sub(1) else {
            return Ok(Resolution::OverBudget);
        };
        *left = spent;
        let resolution = match self.lookup(source, block, boundary)? {
            None => Resolution::Broken,
            Some(backrefs) => {
                path.push(block);
                let up = self.climb(source, &backrefs, left, path, boundary);
                path.pop();
                up?
            }
        };
        self.remember(block, &resolution, boundary)?;
        Ok(resolution)
    }

    /// Resolves the backrefs of one block.
    fn climb<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        backrefs: &[BlockBackref],
        left: &mut u32,
        path: &mut Vec<u64>,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Resolution, WalkError> {
        let mut roots = BTreeSet::new();
        for backref in backrefs {
            match *backref {
                BlockBackref::Root(root) => {
                    roots.insert(root);
                }
                BlockBackref::Parent(parent) => {
                    match self.resolve(source, parent, left, path, boundary)? {
                        Resolution::Roots(found) => roots.extend(found),
                        other => return Ok(other),
                    }
                }
            }
        }
        Ok(Resolution::Roots(roots.into_iter().collect()))
    }

    /// Keeps what a block resolved to; running out of budget says
    /// nothing about the block itself.
    fn remember(
        &mut self,
        block: u64,
        resolution: &Resolution,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(), WalkError> {
        let roots_len = match resolution {
            Resolution::Roots(roots) => roots.len(),
            Resolution::Broken => 0,
            Resolution::OverBudget => return Ok(()),
        };
        if self.memo.len() >= MEMO_CAPACITY {
            self.memo.clear();
        }

        let entry_bytes = std::mem::size_of::<(u64, Option<Vec<u64>>)>() as u64;
        let map_growth = if self.memo.len() == self.memo.capacity() {
            let target_capacity = self
                .memo
                .capacity()
                .saturating_mul(2)
                .saturating_add(2)
                .max(4);
            (target_capacity as u64).saturating_mul(entry_bytes + MEMO_SLOT_OVERHEAD_BYTES)
        } else {
            entry_bytes + MEMO_SLOT_OVERHEAD_BYTES
        };
        let roots_bytes = (roots_len as u64).saturating_mul(8);
        if let ControlFlow::Break(reason) = boundary.reserve(map_growth.saturating_add(roots_bytes))
        {
            return Err(WalkError::Stopped(reason));
        }
        self.memo
            .try_reserve(1)
            .map_err(|_| WalkError::Stopped(StopReason::ResourceLimit))?;
        let known = match resolution {
            Resolution::Roots(roots) => {
                let mut copy = Vec::new();
                copy.try_reserve_exact(roots.len())
                    .map_err(|_| WalkError::Stopped(StopReason::ResourceLimit))?;
                copy.extend_from_slice(roots);
                Some(copy)
            }
            Resolution::Broken => None,
            Resolution::OverBudget => unreachable!("returned above"),
        };
        self.memo.insert(block, known);
        Ok(())
    }

    /// The backrefs of tree block `block`; `None` when it has no tree
    /// block item or one of its items is malformed.
    fn lookup<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        block: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Option<Vec<BlockBackref>>, WalkError> {
        self.lookups += 1;
        let range = SearchRange::new(
            EXTENT_TREE_OBJECTID,
            Key::new(block, EXTENT_ITEM_KEY, 0),
            Key::new(block, SHARED_BLOCK_REF_KEY, u64::MAX),
        );
        let mut found = BlockItems::default();
        let stats = walk_tree(source, range, boundary, |item| {
            found.add(item);
            Ok(())
        })?;
        self.stats.add(&stats);
        Ok(found.finish())
    }
}

/// The items of one tree block collected from the extent tree.
#[derive(Default)]
struct BlockItems {
    seen_item: bool,
    malformed: bool,
    backrefs: Vec<BlockBackref>,
}

impl BlockItems {
    fn add(&mut self, item: Item<'_>) {
        let parsed = match item.key.item_type {
            EXTENT_ITEM_KEY | METADATA_ITEM_KEY if !self.seen_item => {
                self.seen_item = true;
                parse_tree_block_item(item.key, item.data)
            }
            TREE_BLOCK_REF_KEY | SHARED_BLOCK_REF_KEY => {
                parse_keyed_block_backref(item.key, item.data).map(|b| vec![b])
            }
            _ => Err(BackrefError::NotATreeBlock),
        };
        match parsed {
            Ok(backrefs) => self.backrefs.extend(backrefs),
            Err(_) => self.malformed = true,
        }
    }

    fn finish(self) -> Option<Vec<BlockBackref>> {
        (self.seen_item && !self.malformed).then_some(self.backrefs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::backref::testing::encode_tree_block;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};
    use crate::btrfs::walk::{BatchBoundary, StopReason, WalkError};
    use std::ops::ControlFlow;

    const MIB: u64 = 1 << 20;

    fn block(bytenr: u64, backrefs: &[BlockBackref]) -> (Key, Vec<u8>) {
        (
            Key::new(bytenr, METADATA_ITEM_KEY, 0),
            encode_tree_block(backrefs.len() as u64, false, backrefs),
        )
    }

    fn walk(items: Vec<(Key, Vec<u8>)>, budget: u32, blocks: &[u64]) -> Option<BTreeSet<u64>> {
        let mut source = FakeSource::new(items, 4);
        OwnerWalk::new(budget)
            .anchors(&mut source, blocks, &mut NoStop)
            .unwrap()
    }

    struct StopBeforeReservation;

    impl BatchBoundary for StopBeforeReservation {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }

        fn reserve(&mut self, _bytes: u64) -> ControlFlow<StopReason> {
            ControlFlow::Break(StopReason::ResourceLimit)
        }
    }

    #[derive(Default)]
    struct ReservationLog(Vec<u64>);

    impl BatchBoundary for ReservationLog {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }

        fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
            self.0.push(bytes);
            ControlFlow::Continue(())
        }
    }

    #[test]
    fn owner_walk_stops_before_retaining_an_unbudgeted_resolution() {
        let mut source = FakeSource::new(vec![block(MIB, &[BlockBackref::Root(257)])], 4);
        let mut owners = OwnerWalk::new(4);

        let result = owners.anchors(&mut source, &[MIB], &mut StopBeforeReservation);

        assert!(matches!(
            result,
            Err(WalkError::Stopped(StopReason::ResourceLimit))
        ));
        assert!(owners.memo.is_empty());
    }

    #[test]
    fn owner_walk_charges_the_memo_slot_and_cached_roots() {
        let mut source = FakeSource::new(vec![block(MIB, &[BlockBackref::Root(257)])], 4);
        let mut owners = OwnerWalk::new(4);
        let mut boundary = ReservationLog::default();

        assert_eq!(
            owners.anchors(&mut source, &[MIB], &mut boundary).unwrap(),
            Some(BTreeSet::from([257]))
        );
        assert_eq!(boundary.0.len(), 1);
        let entry_bytes = std::mem::size_of::<(u64, Option<Vec<u64>>)>() as u64;
        assert_eq!(
            boundary.0[0],
            4 * (entry_bytes + MEMO_SLOT_OVERHEAD_BYTES) + std::mem::size_of::<u64>() as u64
        );
    }

    #[test]
    fn a_leaf_with_a_tree_block_ref_is_reached_by_that_root() {
        let items = vec![block(MIB, &[BlockBackref::Root(257)])];
        assert_eq!(walk(items, 4, &[MIB]), Some(BTreeSet::from([257])));
    }

    #[test]
    fn shared_block_refs_lead_up_to_every_root_above() {
        let items = vec![
            block(MIB, &[BlockBackref::Parent(2 * MIB)]),
            block(2 * MIB, &[BlockBackref::Root(257)]),
            (Key::new(2 * MIB, TREE_BLOCK_REF_KEY, 258), Vec::new()),
            (Key::new(2 * MIB, SHARED_BLOCK_REF_KEY, 3 * MIB), Vec::new()),
            block(3 * MIB, &[BlockBackref::Root(300)]),
        ];
        assert_eq!(
            walk(items, 4, &[MIB]),
            Some(BTreeSet::from([257, 258, 300]))
        );
    }

    #[test]
    fn a_resolved_block_is_not_looked_up_again() {
        let items = vec![
            block(MIB, &[BlockBackref::Parent(3 * MIB)]),
            block(2 * MIB, &[BlockBackref::Parent(3 * MIB)]),
            block(3 * MIB, &[BlockBackref::Root(257)]),
        ];
        let mut source = FakeSource::new(items, 4);
        let mut owners = OwnerWalk::new(2);
        let first = owners.anchors(&mut source, &[MIB], &mut NoStop).unwrap();
        assert_eq!(first, Some(BTreeSet::from([257])));
        // Within budget only because the shared parent is remembered.
        let second = owners
            .anchors(&mut source, &[2 * MIB], &mut NoStop)
            .unwrap();
        assert_eq!(second, Some(BTreeSet::from([257])));
        assert_eq!(owners.lookups, 3);
    }

    #[test]
    fn global_budget_caps_block_lookups_across_extents() {
        let items = vec![
            block(MIB, &[BlockBackref::Root(257)]),
            block(2 * MIB, &[BlockBackref::Root(258)]),
        ];
        let mut source = FakeSource::new(items, 4);
        let mut owners = OwnerWalk::with_global_budget(8, 1);

        assert_eq!(
            owners.anchors(&mut source, &[MIB], &mut NoStop).unwrap(),
            Some(BTreeSet::from([257]))
        );
        assert_eq!(
            owners
                .anchors(&mut source, &[2 * MIB], &mut NoStop)
                .unwrap(),
            None
        );
        assert_eq!(owners.lookups, 1);
    }

    #[test]
    fn running_out_of_lookups_leaves_the_owners_unresolved_but_retryable() {
        let items = vec![
            block(MIB, &[BlockBackref::Parent(2 * MIB)]),
            block(2 * MIB, &[BlockBackref::Parent(3 * MIB)]),
            block(3 * MIB, &[BlockBackref::Root(257)]),
        ];
        let mut source = FakeSource::new(items, 4);
        let mut owners = OwnerWalk::new(2);
        assert_eq!(
            owners.anchors(&mut source, &[MIB], &mut NoStop).unwrap(),
            None
        );
        let mut generous = OwnerWalk {
            budget: 3,
            ..owners
        };
        assert_eq!(
            generous.anchors(&mut source, &[MIB], &mut NoStop).unwrap(),
            Some(BTreeSet::from([257]))
        );
    }

    #[test]
    fn a_missing_malformed_or_cyclic_block_is_unresolved() {
        assert_eq!(walk(Vec::new(), 4, &[MIB]), None);
        let malformed = vec![(Key::new(MIB, METADATA_ITEM_KEY, 0), vec![1, 2, 3])];
        assert_eq!(walk(malformed, 4, &[MIB]), None);
        let cycle = vec![
            block(MIB, &[BlockBackref::Parent(2 * MIB)]),
            block(2 * MIB, &[BlockBackref::Parent(MIB)]),
        ];
        assert_eq!(walk(cycle, 8, &[MIB]), None);
    }
}
