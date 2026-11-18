//! Drives a `MetadataSource` over a whole key range in batches, validating
//! every batch and stopping cooperatively at batch boundaries.

use crate::btrfs::keys::{DIR_INDEX_KEY, Key};
use crate::btrfs::parser::ItemError;
use crate::btrfs::search::{BatchError, Item, SearchRange, parse_batch};
use crate::btrfs::source::{MetadataSource, SourceError};
use std::ops::ControlFlow;

/// Why a cooperative batch boundary ended a walk early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Paused,
    /// A cancel request: the job ends and is not resumable.
    Cancelled,
    RuntimeLimit,
    /// The process working set reached the memory budget (SPEC 22.4).
    ResourceLimit,
    /// The state filesystem fell below `--min-free` (SPEC 22.4).
    DiskLimit,
}

/// Called between ioctl batches: the only place a walk yields or stops.
pub trait BatchBoundary {
    fn after_batch(&mut self) -> ControlFlow<StopReason>;

    /// A streaming filesystem adapter can enter aggregation before it returns
    /// its nodes to the common scan pipeline.
    fn phase(&mut self, _phase: &'static str) {}

    /// Called before the in-memory model grows by about `bytes`, so a
    /// budget can stop the work before it is exceeded.
    fn reserve(&mut self, _bytes: u64) -> ControlFlow<StopReason> {
        ControlFlow::Continue(())
    }

    /// Restores saved counts without charging them as fresh metadata I/O.
    fn restored(&mut self, batch: &WalkStats) {
        self.took(batch);
    }

    /// Called with what each batch read once it is taken, so a stopped
    /// walk still says how far it got.
    fn took(&mut self, _batch: &WalkStats) {}
}

#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("malformed search buffer: {0}")]
    Batch(#[from] BatchError),
    #[error("item {key:?}: {source}")]
    Item { key: Key, source: ItemError },
    #[error("stopped at a batch boundary: {0:?}")]
    Stopped(StopReason),
    #[error("resume checkpoint cannot be used: {0}; start a fresh scan")]
    Checkpoint(String),
    #[error("in-memory model limit: {0}")]
    Capacity(&'static str),
    #[error("scan spill file: {0}")]
    Spill(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WalkStats {
    pub batches: u64,
    pub items: u64,
    /// Payload bytes of the items read.
    pub item_bytes: u64,
    /// `DIR_INDEX` items among them: one per directory entry, so a scan
    /// can count the entries of a tree while it is still read.
    pub dir_index_items: u64,
}

impl WalkStats {
    /// What one batch of `nr_items` items read.
    pub fn batch(nr_items: u32, items: &[Item<'_>]) -> WalkStats {
        WalkStats {
            batches: 1,
            items: u64::from(nr_items),
            item_bytes: items.iter().map(|i| i.data.len() as u64).sum(),
            dir_index_items: items
                .iter()
                .filter(|i| i.key.item_type == DIR_INDEX_KEY)
                .count() as u64,
        }
    }

    pub fn add(&mut self, other: &WalkStats) {
        self.batches += other.batches;
        self.items += other.items;
        self.item_bytes += other.item_bytes;
        self.dir_index_items += other.dir_index_items;
    }
}

/// Visits every item in `range` in key order.
pub fn walk_tree<S, F>(
    source: &mut S,
    range: SearchRange,
    boundary: &mut dyn BatchBoundary,
    mut visit: F,
) -> Result<WalkStats, WalkError>
where
    S: MetadataSource + ?Sized,
    F: FnMut(Item<'_>) -> Result<(), WalkError>,
{
    walk_batches(source, range, boundary, |items, _| {
        items.iter().try_for_each(|&item| visit(item))
    })
}

/// Visits every batch of `range` in key order; `visit` gets the boundary
/// so it can charge a batch's growth before taking it.
pub fn walk_batches<S, F>(
    source: &mut S,
    range: SearchRange,
    boundary: &mut dyn BatchBoundary,
    mut visit: F,
) -> Result<WalkStats, WalkError>
where
    S: MetadataSource + ?Sized,
    F: FnMut(&[Item<'_>], &mut dyn BatchBoundary) -> Result<(), WalkError>,
{
    let mut stats = WalkStats::default();
    let mut request = range;
    loop {
        let batch = source.search(&request)?;
        let nr_items = batch.nr_items;
        let items = parse_batch(batch.buf, nr_items, &request)?;
        let Some(last) = items.last().map(|item| item.key) else {
            return Ok(stats);
        };
        visit(&items, boundary)?;
        let batch = WalkStats::batch(nr_items, &items);
        stats.add(&batch);
        boundary.took(&batch);
        if let ControlFlow::Break(reason) = boundary.after_batch() {
            return Err(WalkError::Stopped(reason));
        }
        match last.successor() {
            Some(next) if next <= range.max => request.min = next,
            _ => return Ok(stats),
        }
    }
}

/// The capacity `try_grow` gives `v` for `additional` more elements;
/// `None` if it already has room.
fn grown_capacity<T>(v: &Vec<T>, additional: usize) -> Option<usize> {
    let needed = v.len().saturating_add(additional);
    (needed > v.capacity()).then(|| needed.max(v.capacity().saturating_mul(3) / 2))
}

/// Bytes `try_grow(v, additional)` would allocate, to charge first.
pub fn growth_bytes<T>(v: &Vec<T>, additional: usize) -> u64 {
    grown_capacity(v, additional).map_or(0, |cap| {
        let extra = (cap - v.capacity()) as u64;
        extra.saturating_mul(size_of::<T>() as u64)
    })
}

/// Makes room for `additional` more elements of `v`; an allocation
/// failure stops the walk like the memory budget does (SPEC 22.4).
pub fn try_grow<T>(v: &mut Vec<T>, additional: usize) -> Result<(), WalkError> {
    let Some(cap) = grown_capacity(v, additional) else {
        return Ok(());
    };
    v.try_reserve_exact(cap - v.len())
        .map_err(|_| WalkError::Stopped(StopReason::ResourceLimit))
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::btrfs::keys::Key;
    use crate::btrfs::search::SearchRange;
    use crate::btrfs::search::testing::encode_item;
    use crate::btrfs::source::{MetadataSource, RawBatch, SourceError};
    use crate::btrfs::walk::{BatchBoundary, StopReason};
    use std::ops::ControlFlow;

    /// A boundary that never yields or stops.
    pub struct NoStop;

    impl BatchBoundary for NoStop {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }
    }

    /// An in-memory tree returning at most `per_call` items per search.
    pub struct FakeSource {
        pub items: Vec<(Key, Vec<u8>)>,
        pub per_call: usize,
        pub calls: Vec<SearchRange>,
        /// Generation of the leaf holding each item.
        pub leaf_generation: fn(Key) -> u64,
        buf: Vec<u8>,
    }

    impl FakeSource {
        pub fn new(mut items: Vec<(Key, Vec<u8>)>, per_call: usize) -> Self {
            items.sort_by_key(|(k, _)| *k);
            FakeSource {
                items,
                per_call,
                calls: Vec::new(),
                leaf_generation: |_| 1,
                buf: Vec::new(),
            }
        }
    }

    impl MetadataSource for FakeSource {
        fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            self.calls.push(*range);
            self.buf.clear();
            let hits = self
                .items
                .iter()
                .filter(|(k, _)| range.contains(*k))
                .filter(|(k, _)| (self.leaf_generation)(*k) >= range.min_transid)
                .take(self.per_call);
            let mut nr_items = 0;
            for (key, data) in hits {
                self.buf
                    .extend(encode_item((self.leaf_generation)(*key), *key, data));
                nr_items += 1;
            }
            Ok(RawBatch {
                nr_items,
                buf: &self.buf,
            })
        }
    }

    /// In-memory trees by id; a tree it lacks is empty.
    pub struct FakeTrees {
        pub trees: std::collections::HashMap<u64, FakeSource>,
    }

    impl FakeTrees {
        pub fn new(trees: impl IntoIterator<Item = (u64, Vec<(Key, Vec<u8>)>)>) -> Self {
            FakeTrees {
                trees: trees
                    .into_iter()
                    .map(|(id, items)| (id, FakeSource::new(items, 3)))
                    .collect(),
            }
        }
    }

    impl MetadataSource for FakeTrees {
        fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            let tree = self
                .trees
                .entry(range.tree_id)
                .or_insert_with(|| FakeSource::new(Vec::new(), 3));
            tree.search(range)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{FakeSource, NoStop};
    use super::*;
    use crate::btrfs::keys::{DIR_INDEX_KEY, INODE_ITEM_KEY, Key};
    use crate::btrfs::search::SearchRange;
    use crate::btrfs::source::RawBatch;
    use std::ops::ControlFlow;

    struct CountingBoundary {
        batches: u32,
        stop_after: u32,
    }

    impl BatchBoundary for CountingBoundary {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            self.batches += 1;
            if self.batches >= self.stop_after {
                ControlFlow::Break(StopReason::Paused)
            } else {
                ControlFlow::Continue(())
            }
        }
    }

    fn keys(n: u64) -> Vec<(Key, Vec<u8>)> {
        (0..n)
            .map(|i| (Key::new(256 + i, 1, 0), vec![i as u8]))
            .collect()
    }

    #[test]
    fn visits_every_item_across_batches_in_key_order() {
        let mut source = FakeSource::new(keys(5), 2);
        let mut seen = Vec::new();
        let range = SearchRange::new(5, Key::MIN, Key::MAX);
        let stats = walk_tree(&mut source, range, &mut NoStop, |item| {
            seen.push(item.key.objectid);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec![256, 257, 258, 259, 260]);
        assert_eq!(
            stats,
            WalkStats {
                batches: 3,
                items: 5,
                item_bytes: 5,
                dir_index_items: 0,
            }
        );
    }

    /// Keeps what each batch read.
    struct Taken(Vec<WalkStats>);

    impl BatchBoundary for Taken {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }

        fn took(&mut self, batch: &WalkStats) {
            self.0.push(*batch);
        }
    }

    #[test]
    fn each_batch_counts_its_directory_entries() {
        let items = vec![
            (Key::new(256, INODE_ITEM_KEY, 0), vec![0]),
            (Key::new(256, DIR_INDEX_KEY, 2), vec![0]),
            (Key::new(256, DIR_INDEX_KEY, 3), vec![0]),
        ];
        let mut source = FakeSource::new(items, 2);
        let mut taken = Taken(Vec::new());
        let range = SearchRange::new(5, Key::MIN, Key::MAX);
        let stats = walk_batches(&mut source, range, &mut taken, |_, _| Ok(())).unwrap();
        let per_batch: Vec<u64> = taken.0.iter().map(|b| b.dir_index_items).collect();
        assert_eq!(per_batch, vec![1, 1]);
        assert_eq!(stats.dir_index_items, 2);
    }

    #[test]
    fn resumes_each_batch_after_the_last_returned_key() {
        let mut source = FakeSource::new(keys(3), 2);
        let range = SearchRange::new(5, Key::MIN, Key::MAX);
        walk_tree(&mut source, range, &mut NoStop, |_| Ok(())).unwrap();
        let mins: Vec<Key> = source.calls.iter().map(|r| r.min).collect();
        assert_eq!(
            mins,
            vec![Key::MIN, Key::new(257, 1, 1), Key::new(258, 1, 1)]
        );
    }

    #[test]
    fn stops_when_last_key_is_the_range_maximum() {
        let mut source = FakeSource::new(keys(2), 10);
        let range = SearchRange::new(5, Key::MIN, Key::new(257, 1, 0));
        walk_tree(&mut source, range, &mut NoStop, |_| Ok(())).unwrap();
        assert_eq!(source.calls.len(), 1);
    }

    #[test]
    fn batch_boundary_can_stop_the_walk() {
        let mut source = FakeSource::new(keys(5), 2);
        let mut boundary = CountingBoundary {
            batches: 0,
            stop_after: 1,
        };
        let range = SearchRange::new(5, Key::MIN, Key::MAX);
        let err = walk_tree(&mut source, range, &mut boundary, |_| Ok(())).unwrap_err();
        assert!(matches!(err, WalkError::Stopped(StopReason::Paused)));
        assert_eq!(source.calls.len(), 1);
    }

    #[test]
    fn visitor_errors_abort_the_walk() {
        let mut source = FakeSource::new(keys(5), 2);
        let range = SearchRange::new(5, Key::MIN, Key::MAX);
        let err = walk_tree(&mut source, range, &mut NoStop, |item| {
            Err(WalkError::Item {
                key: item.key,
                source: ItemError::EmptyName,
            })
        })
        .unwrap_err();
        assert!(matches!(err, WalkError::Item { .. }));
    }

    #[test]
    fn growth_is_charged_before_it_is_allocated() {
        let mut v: Vec<u64> = Vec::with_capacity(4);
        v.extend([1, 2, 3, 4]);
        assert_eq!(growth_bytes(&v, 0), 0);
        let charged = growth_bytes(&v, 1);
        assert!(charged >= 8, "{charged}");
        try_grow(&mut v, 1).unwrap();
        assert_eq!((v.capacity() - 4) * size_of::<u64>(), charged as usize);
    }

    #[test]
    fn growth_that_cannot_be_allocated_stops_the_walk() {
        let mut v: Vec<u64> = Vec::new();
        let everything = (usize::MAX as u64).saturating_mul(size_of::<u64>() as u64);
        assert_eq!(growth_bytes(&v, usize::MAX), everything);
        let err = try_grow(&mut v, usize::MAX / 2).unwrap_err();
        assert!(matches!(err, WalkError::Stopped(StopReason::ResourceLimit)));
        assert_eq!(v.capacity(), 0);
    }

    struct LyingSource;

    impl MetadataSource for LyingSource {
        fn search(&mut self, _: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
            Ok(RawBatch {
                nr_items: 1,
                buf: &[],
            })
        }
    }

    #[test]
    fn malformed_batches_are_errors() {
        let range = SearchRange::new(5, Key::MIN, Key::MAX);
        let err = walk_tree(&mut LyingSource, range, &mut NoStop, |_| Ok(())).unwrap_err();
        assert!(matches!(err, WalkError::Batch(_)));
    }
}
