//! The file ranges an extent's unscanned references hold (SPEC 5.13):
//! a `SHARED_DATA_REF` names the leaf holding its file extent items, so
//! that leaf is read by address instead of materialising the snapshots
//! sharing it; an `EXTENT_DATA_REF` to an unscanned root is found by a
//! bounded search of that root. Reads stop at a budget.

use crate::btrfs::backref::{
    DataBackref, EXTENT_FLAG_TREE_BLOCK, EXTENT_ITEM_KEY, EXTENT_TREE_OBJECTID, METADATA_ITEM_KEY,
    parse_extent_item,
};
use crate::btrfs::extent_refs::ExtentRefs;
use crate::btrfs::keys::{EXTENT_DATA_KEY, Key};
use crate::btrfs::parser::{ExtentKind, is_inline_extent, parse_file_extent};
use crate::btrfs::search::{Item, SearchRange};
use crate::btrfs::source::{MetadataSource, SourceError};
use crate::btrfs::tree_block::{Expected, parse_leaf};
use crate::btrfs::walk::{BatchBoundary, WalkError, WalkStats, walk_tree};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::rc::Rc;

/// Auto admission: select a volume-scaled I/O budget, independently of RAM.
/// Explicit numeric limits (including zero) remain exact.
pub const DEFAULT_READ_BUDGET: u64 = u64::MAX;
/// Parsed leaves kept for later extents, bounding the memory they hold.
const CACHE_LEAVES: usize = 256;
/// Largest decoded length of a compressed extent: the bound on a file
/// extent item's offset into it.
const MAX_COMPRESSED_DECODED: u64 = 128 << 10;

/// How a volume overview batches the leaf reads its pinned bytes need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafReadPlan {
    /// Extents held so their shared leaves are prefetched together.
    pub window: usize,
    /// Prefetch reads in flight at once.
    pub threads: usize,
    /// Parsed leaves kept for later extents.
    pub cache_leaves: usize,
}

impl Default for LeafReadPlan {
    /// One extent at a time, one read at a time.
    fn default() -> Self {
        LeafReadPlan {
            window: 1,
            threads: 1,
            cache_leaves: CACHE_LEAVES,
        }
    }
}

/// Reads metadata blocks by logical address.
pub trait BlockReader {
    /// The filesystem id every block header carries.
    fn fsid(&self) -> [u8; 16];

    /// The tree block at logical address `bytenr`.
    fn read(&mut self, bytenr: u64) -> io::Result<&[u8]>;

    /// The leaf `parse` makes of the block at each of `bytenrs`, given
    /// its index, with up to `threads` reads at once.
    fn read_each(&mut self, bytenrs: &[u64], threads: usize, parse: &Parse<'_>) -> Vec<ReadLeaf> {
        let _ = threads;
        bytenrs
            .iter()
            .enumerate()
            .map(|(i, &bytenr)| parse(i, self.read(bytenr)))
            .collect()
    }
}

impl<R: BlockReader + ?Sized> BlockReader for Box<R> {
    fn fsid(&self) -> [u8; 16] {
        (**self).fsid()
    }

    fn read(&mut self, bytenr: u64) -> io::Result<&[u8]> {
        (**self).read(bytenr)
    }

    fn read_each(&mut self, bytenrs: &[u64], threads: usize, parse: &Parse<'_>) -> Vec<ReadLeaf> {
        (**self).read_each(bytenrs, threads, parse)
    }
}

/// A leaf read in a batch: the bytes read and its file extent
/// references, `None` when it could not be read or verified.
pub struct ReadLeaf {
    bytes: u64,
    refs: Option<Vec<LeafRef>>,
}

/// Turns the block read at an index of a batch into its leaf.
pub type Parse<'a> = dyn Fn(usize, io::Result<&[u8]>) -> ReadLeaf + Sync + 'a;

/// The range `[start, end)` of an extent's decoded data one file
/// extent item references.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct FileRange {
    pub start: u64,
    pub end: u64,
    /// A compressed extent: the range is not one of its disk bytes.
    pub opaque: bool,
}

/// Why the ranges of an extent's references are not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolved {
    /// Reading them needs more reads than the budget has left.
    BudgetExhausted,
    /// A leaf or root did not hold the references the extent tree
    /// declares, or could not be read.
    Unverified,
    /// A direct reference's root cannot be searched: it is deleted and
    /// awaits the cleaner.
    RootUnsearchable,
}

/// What resolving ranges read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LeafRangeStats {
    pub leaf_reads: u64,
    pub bytes_read: u64,
    pub cache_hits: u64,
    pub direct_searches: u64,
    /// Extent-tree and root searches.
    pub walk: WalkStats,
}

/// One file extent item of a leaf.
#[derive(Debug, Clone, Copy)]
struct LeafRef {
    inode: u64,
    bytenr: u64,
    disk_bytes: u64,
    range: FileRange,
}

/// A read leaf, `None` when it could not be verified.
type Leaf = Option<Rc<[LeafRef]>>;

type Ranges = Result<Vec<FileRange>, Unresolved>;

/// Resolves reference ranges with at most `budget` reads.
pub struct LeafRanges<R> {
    reader: R,
    budget: u64,
    capacity: usize,
    threads: usize,
    cache: HashMap<u64, Leaf>,
    order: VecDeque<u64>,
    pub stats: LeafRangeStats,
}

impl<R: BlockReader> LeafRanges<R> {
    pub fn new(reader: R, budget: u64) -> Self {
        LeafRanges {
            reader,
            budget,
            capacity: CACHE_LEAVES,
            threads: 1,
            cache: HashMap::new(),
            order: VecDeque::new(),
            stats: LeafRangeStats::default(),
        }
    }

    /// Prefetches with up to `threads` reads at once.
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads;
        self
    }

    /// Keeps up to `leaves` parsed leaves.
    pub fn with_cache_leaves(mut self, leaves: usize) -> Self {
        self.capacity = leaves;
        self
    }

    /// The ranges the shared-leaf references of the extent at `bytenr`
    /// hold; its direct references are left to the scan.
    pub fn shared_ranges<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        bytenr: u64,
        refs: &ExtentRefs,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Ranges, WalkError> {
        self.ranges(source, bytenr, refs, false, boundary)
    }

    /// The ranges every reference of the extent at `bytenr` holds.
    pub fn all_ranges<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        bytenr: u64,
        refs: &ExtentRefs,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Ranges, WalkError> {
        self.ranges(source, bytenr, refs, true, boundary)
    }

    fn ranges<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        bytenr: u64,
        refs: &ExtentRefs,
        direct: bool,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Ranges, WalkError> {
        let mut found = Vec::new();
        for backref in &refs.backrefs {
            let part = match *backref {
                DataBackref::Shared { parent, count } => {
                    self.shared(source, bytenr, parent, count, boundary)?
                }
                DataBackref::Direct {
                    root,
                    objectid,
                    offset,
                    count,
                } if direct => {
                    let owner = Key::new(objectid, EXTENT_DATA_KEY, offset);
                    self.direct(source, bytenr, refs.num_bytes, root, owner, count, boundary)?
                }
                DataBackref::Direct { .. } => continue,
            };
            match part {
                Ok(ranges) => found.extend(ranges),
                Err(unresolved) => return Ok(Err(unresolved)),
            }
        }
        found.sort_unstable();
        Ok(Ok(found))
    }

    /// Reads the leaves among `parents` not yet remembered, once each and
    /// in address order, while the budget lasts and the cache holds them.
    pub fn prefetch<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        parents: &[u64],
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(), WalkError> {
        let mut wanted: Vec<u64> = parents
            .iter()
            .copied()
            .filter(|p| !self.cache.contains_key(p))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        wanted.truncate(self.capacity);
        let mut expected = Vec::new();
        for parent in wanted {
            if !self.spend() {
                break;
            }
            match self.generation(source, parent, boundary)? {
                Some(generation) => expected.push(Expected {
                    bytenr: parent,
                    generation,
                    fsid: self.reader.fsid(),
                }),
                None => self.remember(parent, None, boundary),
            }
        }
        let bytenrs: Vec<u64> = expected.iter().map(|e| e.bytenr).collect();
        let read = self.reader.read_each(&bytenrs, self.threads, &|i, block| {
            read_leaf(block, &expected[i])
        });
        for (bytenr, leaf) in bytenrs.into_iter().zip(read) {
            self.stats.leaf_reads += 1;
            self.stats.bytes_read += leaf.bytes;
            self.remember(bytenr, leaf.refs.map(Rc::from), boundary);
        }
        Ok(())
    }

    /// Reads and remembers leaf `parent`; `None` once the budget is spent.
    fn fetch<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        parent: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Option<Leaf>, WalkError> {
        if !self.spend() {
            return Ok(None);
        }
        let leaf = self.load(source, parent, boundary)?;
        self.remember(parent, leaf.clone(), boundary);
        Ok(Some(leaf))
    }

    /// The `count` ranges leaf `parent` holds for the extent.
    fn shared<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        bytenr: u64,
        parent: u64,
        count: u32,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Ranges, WalkError> {
        Ok(self
            .shared_refs(source, bytenr, parent, count, boundary)?
            .map(|refs| refs.into_iter().map(|r| r.range).collect()))
    }

    /// Inodes in a verified shared leaf that hold exactly the declared
    /// references to this allocation. Root anchors alone cannot classify
    /// a leaf reached by a mixed volume root. This is display attribution,
    /// never a proof that deleting the paths frees the allocation.
    pub fn shared_inodes<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        bytenr: u64,
        refs: &ExtentRefs,
        reference: DataBackref,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Result<Vec<u64>, Unresolved>, WalkError> {
        let DataBackref::Shared { parent, count } = reference else {
            return Ok(Err(Unresolved::Unverified));
        };
        if count == 0 {
            return Ok(Err(Unresolved::Unverified));
        }
        Ok(self
            .shared_refs(source, bytenr, parent, count, boundary)?
            .and_then(|items| {
                if items.iter().any(|item| item.disk_bytes != refs.num_bytes) {
                    Err(Unresolved::Unverified)
                } else {
                    Ok(items.into_iter().map(|item| item.inode).collect())
                }
            }))
    }

    fn shared_refs<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        bytenr: u64,
        parent: u64,
        count: u32,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Result<Vec<LeafRef>, Unresolved>, WalkError> {
        let leaf = match self.cache.get(&parent) {
            Some(leaf) => {
                self.stats.cache_hits += 1;
                leaf.clone()
            }
            None => match self.fetch(source, parent, boundary)? {
                Some(leaf) => leaf,
                None => return Ok(Err(Unresolved::BudgetExhausted)),
            },
        };
        let Some(leaf) = leaf else {
            return Ok(Err(Unresolved::Unverified));
        };
        let refs: Vec<_> = leaf
            .iter()
            .filter(|r| r.bytenr == bytenr)
            .copied()
            .collect();
        Ok(if refs.len() == count as usize {
            Ok(refs)
        } else {
            Err(Unresolved::Unverified)
        })
    }

    /// The `count` ranges the file extent items of `owner`'s inode in
    /// `root` hold for the extent: items whose file offset minus extent
    /// offset is `owner.offset`, which bounds their keys by the extent.
    #[allow(clippy::too_many_arguments)]
    fn direct<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        bytenr: u64,
        num_bytes: u64,
        root: u64,
        owner: Key,
        count: u32,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Ranges, WalkError> {
        if !self.spend() {
            return Ok(Err(Unresolved::BudgetExhausted));
        }
        self.stats.direct_searches += 1;
        let mut ranges = Vec::new();
        let mut malformed = false;
        for span in direct_spans(root, owner, num_bytes) {
            let walked = walk_tree(source, span, boundary, |item| {
                match leaf_ref(&item) {
                    Ok(Some(r))
                        if r.bytenr == bytenr
                            && item.key.offset.wrapping_sub(r.range.start) == owner.offset =>
                    {
                        ranges.push(r.range)
                    }
                    Ok(_) => {}
                    Err(()) => malformed = true,
                }
                Ok(())
            });
            let stats = match walked {
                Err(WalkError::Source(SourceError::Io(e)))
                    if e.kind() == io::ErrorKind::NotFound =>
                {
                    return Ok(Err(Unresolved::RootUnsearchable));
                }
                walked => walked?,
            };
            self.stats.walk.add(&stats);
        }
        if malformed {
            return Ok(Err(Unresolved::Unverified));
        }
        Ok(counted(ranges, count))
    }

    /// Takes one read from the budget.
    fn spend(&mut self) -> bool {
        match self.budget.checked_sub(1) {
            Some(left) => {
                self.budget = left;
                true
            }
            None => false,
        }
    }

    /// Reads leaf `parent` once the extent tree names its generation.
    fn load<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        parent: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Leaf, WalkError> {
        let Some(generation) = self.generation(source, parent, boundary)? else {
            return Ok(None);
        };
        let expected = Expected {
            bytenr: parent,
            generation,
            fsid: self.reader.fsid(),
        };
        self.stats.leaf_reads += 1;
        let leaf = read_leaf(self.reader.read(parent), &expected);
        self.stats.bytes_read += leaf.bytes;
        Ok(leaf.refs.map(Rc::from))
    }

    /// The generation the extent tree records for tree block `block`.
    fn generation<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        block: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Option<u64>, WalkError> {
        let range = SearchRange::new(
            EXTENT_TREE_OBJECTID,
            Key::new(block, EXTENT_ITEM_KEY, 0),
            Key::new(block, METADATA_ITEM_KEY, u64::MAX),
        );
        let mut generation = None;
        let stats = walk_tree(source, range, boundary, |item| {
            if generation.is_none() {
                generation = parse_extent_item(item.data)
                    .ok()
                    .filter(|e| e.flags & EXTENT_FLAG_TREE_BLOCK != 0)
                    .map(|e| e.generation);
            }
            Ok(())
        })?;
        self.stats.walk.add(&stats);
        Ok(generation)
    }

    /// Keeps `leaf` for later extents, evicting the oldest when full; the
    /// cache stops growing once the memory budget says so.
    fn remember(&mut self, parent: u64, leaf: Leaf, boundary: &mut dyn BatchBoundary) {
        if self.cache.len() < self.capacity {
            let len = leaf.as_ref().map_or(0, |refs| refs.len());
            let bytes = (len * size_of::<LeafRef>() + size_of::<Leaf>() + 16) as u64;
            if boundary.reserve(bytes).is_break() {
                self.capacity = self.cache.len();
            }
        }
        if self.capacity == 0 {
            return;
        }
        if self.cache.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.cache.remove(&oldest);
        }
        self.cache.insert(parent, leaf);
        self.order.push_back(parent);
    }
}

/// The key spans holding `owner`'s file extent items of an extent of
/// `num_bytes`: file offsets from `owner.offset` on, by less than the
/// largest extent offset, wrapping as the kernel's `u64` offset does.
fn direct_spans(root: u64, owner: Key, num_bytes: u64) -> Vec<SearchRange> {
    let reach = num_bytes.max(MAX_COMPRESSED_DECODED) - 1;
    let at = |offset| Key { offset, ..owner };
    match owner.offset.checked_add(reach) {
        Some(last) => vec![SearchRange::new(root, owner, at(last))],
        None => vec![
            SearchRange::new(root, owner, at(u64::MAX)),
            SearchRange::new(root, at(0), at(owner.offset.wrapping_add(reach))),
        ],
    }
}

/// `ranges` when they are exactly the `count` the backref declares.
fn counted(ranges: Vec<FileRange>, count: u32) -> Ranges {
    if ranges.len() == count as usize {
        Ok(ranges)
    } else {
        Err(Unresolved::Unverified)
    }
}

/// The leaf `block` holds once it verifies as `expected`.
fn read_leaf(block: io::Result<&[u8]>, expected: &Expected) -> ReadLeaf {
    let Ok(block) = block else {
        return ReadLeaf {
            bytes: 0,
            refs: None,
        };
    };
    ReadLeaf {
        bytes: block.len() as u64,
        refs: parse_leaf(block, expected)
            .ok()
            .and_then(|items| leaf_refs(&items)),
    }
}

/// The data extent references of a leaf's items; `None` when one is
/// malformed.
fn leaf_refs(items: &[Item<'_>]) -> Option<Vec<LeafRef>> {
    items
        .iter()
        .filter_map(|item| leaf_ref(item).transpose())
        .collect::<Result<_, ()>>()
        .ok()
}

/// The data extent reference of a file extent item, if it has one.
fn leaf_ref(item: &Item<'_>) -> Result<Option<LeafRef>, ()> {
    if item.key.item_type != EXTENT_DATA_KEY || is_inline_extent(item.data) {
        return Ok(None);
    }
    let extent = parse_file_extent(item.data).map_err(|_| ())?;
    let disk = match extent.kind {
        ExtentKind::Regular(disk) | ExtentKind::Prealloc(disk) => disk,
        ExtentKind::Inline { .. } => return Ok(None),
    };
    if disk.disk_bytenr == 0 {
        return Ok(None);
    }
    let end = disk.offset.checked_add(disk.num_bytes).ok_or(())?;
    Ok(Some(LeafRef {
        inode: item.key.objectid,
        bytenr: disk.disk_bytenr,
        disk_bytes: disk.disk_num_bytes,
        range: FileRange {
            start: disk.offset,
            end,
            opaque: extent.compression != 0,
        },
    }))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::btrfs::parser::DiskExtent;
    use crate::btrfs::parser::testing::encode_regular_extent;
    use std::collections::HashMap;

    /// An uncompressed file extent item referencing `[offset, offset+len)`
    /// of the extent of `disk_bytes` at `bytenr`.
    pub fn file_item(bytenr: u64, disk_bytes: u64, offset: u64, len: u64) -> Vec<u8> {
        let mut data = encode_regular_extent(&DiskExtent {
            disk_bytenr: bytenr,
            disk_num_bytes: disk_bytes,
            offset,
            num_bytes: len,
        });
        // No compression.
        data[16] = 0;
        data
    }

    /// Blocks by address; a missing block fails to read.
    pub struct FakeBlocks {
        pub fsid: [u8; 16],
        pub blocks: HashMap<u64, Vec<u8>>,
        pub reads: Vec<u64>,
    }

    impl BlockReader for FakeBlocks {
        fn fsid(&self) -> [u8; 16] {
            self.fsid
        }

        fn read(&mut self, bytenr: u64) -> io::Result<&[u8]> {
            self.reads.push(bytenr);
            self.blocks
                .get(&bytenr)
                .map(Vec::as_slice)
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{self, FakeBlocks};
    use super::*;
    use crate::btrfs::backref::testing::encode_tree_block;
    use crate::btrfs::backref::{BlockBackref, EXTENT_TREE_OBJECTID, METADATA_ITEM_KEY};
    use crate::btrfs::keys::EXTENT_DATA_KEY;
    use crate::btrfs::tree_block::Expected;
    use crate::btrfs::tree_block::testing::leaf;
    use crate::btrfs::walk::testing::{FakeTrees, NoStop};

    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const FSID: [u8; 16] = [0x5A; 16];
    const EXTENT: u64 = 700 * MIB;
    const OTHER: u64 = 800 * MIB;
    const LEAF: u64 = 30 * MIB;
    const SECOND_LEAF: u64 = 31 * MIB;
    /// `encode_tree_block` writes generation 7.
    const LEAF_GENERATION: u64 = 7;

    fn file_item(bytenr: u64, offset: u64, len: u64) -> Vec<u8> {
        testing::file_item(bytenr, MIB, offset, len)
    }

    fn leaf_at(bytenr: u64, generation: u64, items: &[(Key, Vec<u8>)]) -> Vec<u8> {
        let expected = Expected {
            bytenr,
            generation,
            fsid: FSID,
        };
        leaf(&expected, 4096, items)
    }

    fn extent_tree(leaves: &[u64]) -> (u64, Vec<(Key, Vec<u8>)>) {
        let items = leaves
            .iter()
            .map(|&bytenr| {
                (
                    Key::new(bytenr, METADATA_ITEM_KEY, 0),
                    encode_tree_block(1, false, &[BlockBackref::Root(300)]),
                )
            })
            .collect();
        (EXTENT_TREE_OBJECTID, items)
    }

    fn refs(backrefs: Vec<DataBackref>) -> ExtentRefs {
        ExtentRefs {
            num_bytes: MIB,
            refs: backrefs.iter().map(|r| u64::from(r.count())).sum(),
            generation: 5,
            backrefs,
            malformed: false,
        }
    }

    fn blocks(blocks: Vec<(u64, Vec<u8>)>) -> FakeBlocks {
        FakeBlocks {
            fsid: FSID,
            blocks: blocks.into_iter().collect(),
            reads: Vec::new(),
        }
    }

    fn range(start: u64, end: u64) -> FileRange {
        FileRange {
            start,
            end,
            opaque: false,
        }
    }

    fn payload_leaf() -> Vec<u8> {
        leaf_at(
            LEAF,
            LEAF_GENERATION,
            &[
                (
                    Key::new(257, EXTENT_DATA_KEY, 0),
                    file_item(EXTENT, 0, 256 * KIB),
                ),
                (
                    Key::new(257, EXTENT_DATA_KEY, 256 * KIB),
                    file_item(OTHER, 0, 4 * KIB),
                ),
                (
                    Key::new(258, EXTENT_DATA_KEY, 0),
                    file_item(EXTENT, 512 * KIB, 256 * KIB),
                ),
            ],
        )
    }

    fn shared(parent: u64, count: u32) -> DataBackref {
        DataBackref::Shared { parent, count }
    }

    #[test]
    fn a_shared_leaf_gives_the_ranges_of_its_items_for_the_extent() {
        let mut source = FakeTrees::new([extent_tree(&[LEAF])]);
        let mut leaves = LeafRanges::new(blocks(vec![(LEAF, payload_leaf())]), 10);

        let found = leaves
            .shared_ranges(
                &mut source,
                EXTENT,
                &refs(vec![shared(LEAF, 2)]),
                &mut NoStop,
            )
            .unwrap();

        assert_eq!(
            found,
            Ok(vec![range(0, 256 * KIB), range(512 * KIB, 768 * KIB)])
        );
        assert_eq!(leaves.stats.leaf_reads, 1);
    }

    #[test]
    fn shared_inode_identity_requires_matching_allocation_and_obeys_the_read_budget() {
        let reference = shared(LEAF, 2);
        for capacity in [0, CACHE_LEAVES] {
            let mut source = FakeTrees::new([extent_tree(&[LEAF])]);
            let mut leaves = LeafRanges::new(blocks(vec![(LEAF, payload_leaf())]), 10)
                .with_cache_leaves(capacity);
            let mut extent = refs(vec![reference]);
            assert_eq!(
                leaves
                    .shared_inodes(&mut source, EXTENT, &extent, reference, &mut NoStop)
                    .unwrap(),
                Ok(vec![257, 258])
            );
            extent.num_bytes += 4096;
            assert_eq!(
                leaves
                    .shared_inodes(&mut source, EXTENT, &extent, reference, &mut NoStop)
                    .unwrap(),
                Err(Unresolved::Unverified)
            );
        }
        let mut source = FakeTrees::new([extent_tree(&[LEAF])]);
        let mut leaves = LeafRanges::new(blocks(vec![(LEAF, payload_leaf())]), 0);
        assert_eq!(
            leaves
                .shared_inodes(
                    &mut source,
                    EXTENT,
                    &refs(vec![reference]),
                    reference,
                    &mut NoStop
                )
                .unwrap(),
            Err(Unresolved::BudgetExhausted)
        );
        assert_eq!(leaves.stats.leaf_reads, 0);
        let mut source = FakeTrees::new([extent_tree(&[LEAF])]);
        let empty = leaf_at(LEAF, LEAF_GENERATION, &[]);
        let mut leaves = LeafRanges::new(blocks(vec![(LEAF, empty)]), 10);
        let zero = shared(LEAF, 0);
        assert_eq!(
            leaves
                .shared_inodes(&mut source, EXTENT, &refs(vec![zero]), zero, &mut NoStop)
                .unwrap(),
            Err(Unresolved::Unverified)
        );
    }

    #[test]
    fn a_leaf_holding_another_count_of_references_is_unverified() {
        let mut source = FakeTrees::new([extent_tree(&[LEAF])]);
        let mut leaves = LeafRanges::new(blocks(vec![(LEAF, payload_leaf())]), 10);

        let found = leaves
            .shared_ranges(
                &mut source,
                EXTENT,
                &refs(vec![shared(LEAF, 3)]),
                &mut NoStop,
            )
            .unwrap();

        assert_eq!(found, Err(Unresolved::Unverified));
    }

    #[test]
    fn an_unreadable_or_rewritten_leaf_is_unverified() {
        let rewritten = leaf_at(SECOND_LEAF, LEAF_GENERATION + 1, &[]);
        let mut source = FakeTrees::new([extent_tree(&[LEAF, SECOND_LEAF])]);
        let mut leaves = LeafRanges::new(blocks(vec![(SECOND_LEAF, rewritten)]), 10);

        for parent in [LEAF, SECOND_LEAF] {
            let found = leaves
                .shared_ranges(
                    &mut source,
                    EXTENT,
                    &refs(vec![shared(parent, 1)]),
                    &mut NoStop,
                )
                .unwrap();
            assert_eq!(found, Err(Unresolved::Unverified));
        }
    }

    #[test]
    fn a_leaf_the_extent_tree_does_not_hold_is_unverified_without_a_read() {
        let mut source = FakeTrees::new([extent_tree(&[])]);
        let mut leaves = LeafRanges::new(blocks(vec![(LEAF, payload_leaf())]), 10);

        let found = leaves
            .shared_ranges(
                &mut source,
                EXTENT,
                &refs(vec![shared(LEAF, 2)]),
                &mut NoStop,
            )
            .unwrap();

        assert_eq!(found, Err(Unresolved::Unverified));
        assert!(leaves.reader.reads.is_empty());
    }

    #[test]
    fn reads_stop_at_the_budget_and_remembered_leaves_cost_nothing() {
        let second = leaf_at(
            SECOND_LEAF,
            LEAF_GENERATION,
            &[(Key::new(259, EXTENT_DATA_KEY, 0), file_item(EXTENT, 0, MIB))],
        );
        let mut source = FakeTrees::new([extent_tree(&[LEAF, SECOND_LEAF])]);
        let mut leaves = LeafRanges::new(
            blocks(vec![(LEAF, payload_leaf()), (SECOND_LEAF, second)]),
            1,
        );
        let first = refs(vec![shared(LEAF, 2)]);
        let both = refs(vec![shared(LEAF, 2), shared(SECOND_LEAF, 1)]);

        let once = leaves
            .shared_ranges(&mut source, EXTENT, &first, &mut NoStop)
            .unwrap();
        let again = leaves
            .shared_ranges(&mut source, EXTENT, &first, &mut NoStop)
            .unwrap();
        let over = leaves
            .shared_ranges(&mut source, EXTENT, &both, &mut NoStop)
            .unwrap();

        assert!(once.is_ok());
        assert_eq!(again, once);
        assert_eq!(over, Err(Unresolved::BudgetExhausted));
        assert_eq!(leaves.reader.reads, vec![LEAF]);
        assert_eq!(leaves.stats.cache_hits, 2);
    }

    #[test]
    fn prefetched_leaves_are_read_once_in_address_order_then_served_from_the_cache() {
        let second = leaf_at(
            SECOND_LEAF,
            LEAF_GENERATION,
            &[(Key::new(259, EXTENT_DATA_KEY, 0), file_item(EXTENT, 0, MIB))],
        );
        let mut source = FakeTrees::new([extent_tree(&[LEAF, SECOND_LEAF])]);
        let mut leaves = LeafRanges::new(
            blocks(vec![(LEAF, payload_leaf()), (SECOND_LEAF, second)]),
            10,
        );
        let both = refs(vec![shared(LEAF, 2), shared(SECOND_LEAF, 1)]);

        leaves
            .prefetch(&mut source, &[SECOND_LEAF, LEAF, SECOND_LEAF], &mut NoStop)
            .unwrap();
        let found = leaves
            .shared_ranges(&mut source, EXTENT, &both, &mut NoStop)
            .unwrap();

        assert_eq!(found.map(|r| r.len()), Ok(3));
        assert_eq!(leaves.reader.reads, vec![LEAF, SECOND_LEAF]);
        assert_eq!(leaves.stats.leaf_reads, 2);
        assert_eq!(leaves.stats.cache_hits, 2);
    }

    /// Blocks that record each batch asked of them and its thread count.
    struct Batches(FakeBlocks, Vec<(Vec<u64>, usize)>);

    impl BlockReader for Batches {
        fn fsid(&self) -> [u8; 16] {
            self.0.fsid()
        }

        fn read(&mut self, bytenr: u64) -> io::Result<&[u8]> {
            self.0.read(bytenr)
        }

        fn read_each(
            &mut self,
            bytenrs: &[u64],
            threads: usize,
            parse: &Parse<'_>,
        ) -> Vec<ReadLeaf> {
            self.1.push((bytenrs.to_vec(), threads));
            self.0.read_each(bytenrs, threads, parse)
        }
    }

    #[test]
    fn prefetched_leaves_are_read_as_one_batch_on_the_given_threads() {
        let second = leaf_at(SECOND_LEAF, LEAF_GENERATION, &[]);
        let mut source = FakeTrees::new([extent_tree(&[LEAF, SECOND_LEAF])]);
        let reader = Batches(
            blocks(vec![(LEAF, payload_leaf()), (SECOND_LEAF, second)]),
            Vec::new(),
        );
        let mut leaves = LeafRanges::new(reader, 10).with_threads(4);

        leaves
            .prefetch(&mut source, &[SECOND_LEAF, LEAF, OTHER], &mut NoStop)
            .unwrap();
        let found = leaves
            .shared_ranges(
                &mut source,
                EXTENT,
                &refs(vec![shared(LEAF, 2)]),
                &mut NoStop,
            )
            .unwrap();

        assert_eq!(leaves.reader.1, vec![(vec![LEAF, SECOND_LEAF], 4)]);
        assert_eq!(found.map(|r| r.len()), Ok(2));
        assert_eq!(leaves.stats.leaf_reads, 2);
        assert_eq!(leaves.stats.bytes_read, 2 * 4096);
    }

    #[test]
    fn a_prefetch_reads_no_more_leaves_than_the_cache_keeps() {
        let second = leaf_at(SECOND_LEAF, LEAF_GENERATION, &[]);
        let mut source = FakeTrees::new([extent_tree(&[LEAF, SECOND_LEAF])]);
        let mut leaves = LeafRanges::new(
            blocks(vec![(LEAF, payload_leaf()), (SECOND_LEAF, second)]),
            10,
        )
        .with_cache_leaves(1);

        leaves
            .prefetch(&mut source, &[SECOND_LEAF, LEAF], &mut NoStop)
            .unwrap();

        assert_eq!(leaves.reader.reads, vec![LEAF]);
    }

    #[test]
    fn direct_references_are_found_in_their_root_within_the_extent() {
        let root = (
            300,
            vec![
                (
                    Key::new(257, EXTENT_DATA_KEY, 4 * KIB),
                    file_item(EXTENT, 0, 64 * KIB),
                ),
                (
                    Key::new(257, EXTENT_DATA_KEY, 68 * KIB),
                    file_item(OTHER, 0, 4 * KIB),
                ),
                (
                    Key::new(257, EXTENT_DATA_KEY, 72 * KIB),
                    file_item(EXTENT, 4 * KIB, 4 * KIB),
                ),
            ],
        );
        let mut source = FakeTrees::new([extent_tree(&[]), root]);
        let mut leaves = LeafRanges::new(blocks(Vec::new()), 10);
        let direct = |count| DataBackref::Direct {
            root: 300,
            objectid: 257,
            offset: 4 * KIB,
            count,
        };
        // 72 KiB - 4 KiB is not the ref's offset: that item belongs to
        // another backref.

        let found = leaves
            .all_ranges(&mut source, EXTENT, &refs(vec![direct(1)]), &mut NoStop)
            .unwrap();
        let miscounted = leaves
            .all_ranges(&mut source, EXTENT, &refs(vec![direct(2)]), &mut NoStop)
            .unwrap();

        assert_eq!(found, Ok(vec![range(0, 64 * KIB)]));
        assert_eq!(miscounted, Err(Unresolved::Unverified));
        assert_eq!(leaves.stats.direct_searches, 2);
    }

    /// Trees of which root 300 is deleted: the kernel no longer
    /// searches it.
    struct DeadRoot(FakeTrees);

    impl MetadataSource for DeadRoot {
        fn search(
            &mut self,
            range: &crate::btrfs::search::SearchRange,
        ) -> Result<crate::btrfs::source::RawBatch<'_>, crate::btrfs::source::SourceError> {
            if range.tree_id == 300 {
                return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
            }
            self.0.search(range)
        }
    }

    #[test]
    fn a_direct_reference_to_a_root_the_kernel_cannot_search_stays_unresolved() {
        let mut source = DeadRoot(FakeTrees::new([extent_tree(&[])]));
        let mut leaves = LeafRanges::new(blocks(Vec::new()), 10);
        let direct = DataBackref::Direct {
            root: 300,
            objectid: 257,
            offset: 0,
            count: 1,
        };

        let found = leaves
            .all_ranges(&mut source, EXTENT, &refs(vec![direct]), &mut NoStop)
            .unwrap();

        assert_eq!(found, Err(Unresolved::RootUnsearchable));
    }

    #[test]
    fn a_direct_reference_of_a_clone_before_its_extent_offset_wraps() {
        let root = (
            300,
            vec![(
                Key::new(257, EXTENT_DATA_KEY, 0),
                file_item(EXTENT, 512 * KIB, 4 * KIB),
            )],
        );
        let mut source = FakeTrees::new([extent_tree(&[]), root]);
        let mut leaves = LeafRanges::new(blocks(Vec::new()), 10);
        let wrapped = DataBackref::Direct {
            root: 300,
            objectid: 257,
            offset: 0u64.wrapping_sub(512 * KIB),
            count: 1,
        };

        let found = leaves
            .all_ranges(&mut source, EXTENT, &refs(vec![wrapped]), &mut NoStop)
            .unwrap();

        assert_eq!(found, Ok(vec![range(512 * KIB, 516 * KIB)]));
    }

    #[test]
    fn shared_ranges_leave_direct_references_to_the_scan() {
        let mut source = FakeTrees::new([extent_tree(&[LEAF])]);
        let mut leaves = LeafRanges::new(blocks(vec![(LEAF, payload_leaf())]), 10);
        let mixed = refs(vec![
            DataBackref::Direct {
                root: 256,
                objectid: 300,
                offset: 0,
                count: 1,
            },
            shared(LEAF, 2),
        ]);

        let found = leaves
            .shared_ranges(&mut source, EXTENT, &mixed, &mut NoStop)
            .unwrap();

        assert_eq!(found.map(|r| r.len()), Ok(2));
        assert_eq!(leaves.stats.direct_searches, 0);
    }
}
