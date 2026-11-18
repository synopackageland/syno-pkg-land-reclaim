//! Reads the extent tree's references for a sorted set of data extents
//! (SPEC 8.1): nearby extents are coalesced into range searches, and a
//! volume scan streams the whole span once in bytenr order (SPEC 9.1).

use crate::btrfs::backref::*;
use crate::btrfs::keys::Key;
use crate::btrfs::search::{Item, SearchRange, parse_batch};
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::{BatchBoundary, WalkError, WalkStats, growth_bytes, try_grow, walk_tree};
use std::iter::Peekable;
use std::ops::ControlFlow;

/// Every reference the extent tree records for one data extent.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExtentRefs {
    /// Extent length from the `EXTENT_ITEM` key.
    pub num_bytes: u64,
    /// Total references the item declares.
    pub refs: u64,
    /// Transaction that allocated the extent.
    pub generation: u64,
    /// Inline then keyed backrefs.
    pub backrefs: Vec<DataBackref>,
    /// Some reference could not be read, so the set is incomplete.
    pub malformed: bool,
}

/// How candidate extents become searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// One range from the first to the last candidate.
    Stream,
    /// Candidates at most `max_gap` bytes apart share one range.
    Coalesce { max_gap: u64 },
}

/// Inclusive `(first, last)` bytenr ranges covering sorted `candidates`.
pub fn ranges(candidates: impl Iterator<Item = u64>, plan: Plan) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for bytenr in candidates {
        match (out.last_mut(), plan) {
            (Some(last), Plan::Stream) => last.1 = bytenr,
            (Some(last), Plan::Coalesce { max_gap }) if bytenr - last.1 <= max_gap => {
                last.1 = bytenr
            }
            _ => out.push((bytenr, bytenr)),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveStats {
    pub ranges: u64,
    pub batches: u64,
    pub items: u64,
    pub item_bytes: u64,
}

/// Streams every data extent in bytenr order with its complete extent-tree
/// references. The callback runs between search batches, so it may make
/// bounded owner-resolution searches against the same metadata source.
pub fn stream_data<S, F>(
    source: &mut S,
    boundary: &mut dyn BatchBoundary,
    on_extent: F,
) -> Result<ResolveStats, WalkError>
where
    S: MetadataSource + ?Sized,
    F: FnMut(&mut S, u64, Option<&ExtentRefs>, &mut dyn BatchBoundary) -> Result<(), WalkError>,
{
    let range = SearchRange::new(EXTENT_TREE_OBJECTID, Key::MIN, Key::MAX);
    stream_range(source, range, boundary, on_extent)
}

fn stream_range<S, F>(
    source: &mut S,
    range: SearchRange,
    boundary: &mut dyn BatchBoundary,
    mut on_extent: F,
) -> Result<ResolveStats, WalkError>
where
    S: MetadataSource + ?Sized,
    F: FnMut(&mut S, u64, Option<&ExtentRefs>, &mut dyn BatchBoundary) -> Result<(), WalkError>,
{
    // Nested searches overwrite the source buffer. Keep one flat copy instead
    // of allocating a Vec for every item in every batch: those small allocations
    // interleaved with owner-cache entries fragment the musl allocator.
    let mut payload = Vec::<u8>::new();
    let mut copied = Vec::<(Key, std::ops::Range<usize>)>::new();
    let mut current: Option<(u64, ExtentRefs)> = None;
    let mut request = range;
    let mut walk = WalkStats::default();
    loop {
        payload.clear();
        copied.clear();
        let (last, batch_stats) = {
            let batch = source.search(&request)?;
            let items = parse_batch(batch.buf, batch.nr_items, &request)?;
            let Some(last) = items.last().map(|item| item.key) else {
                break;
            };
            let relevant = |item: &&Item<'_>| {
                matches!(
                    item.key.item_type,
                    EXTENT_ITEM_KEY | EXTENT_DATA_REF_KEY | SHARED_DATA_REF_KEY
                )
            };
            let count = items.iter().filter(relevant).count();
            let bytes = items
                .iter()
                .filter(relevant)
                .map(|item| item.data.len())
                .sum();
            let growth = growth_bytes(&payload, bytes).saturating_add(growth_bytes(&copied, count));
            if let ControlFlow::Break(reason) = boundary.reserve(growth) {
                return Err(WalkError::Stopped(reason));
            }
            try_grow(&mut payload, bytes)?;
            try_grow(&mut copied, count)?;
            for item in items.iter().filter(relevant) {
                let start = payload.len();
                payload.extend_from_slice(item.data);
                copied.push((item.key, start..payload.len()));
            }
            let stats = WalkStats::batch(batch.nr_items, &items);
            (last, stats)
        };
        for (key, span) in &copied {
            let data = &payload[span.clone()];
            if current
                .as_ref()
                .is_some_and(|(bytenr, _)| *bytenr != key.objectid)
            {
                emit(source, boundary, &mut current, &mut on_extent)?;
            }
            match key.item_type {
                EXTENT_ITEM_KEY => {
                    if let Some((bytenr, refs)) = current.as_mut() {
                        if *bytenr == key.objectid {
                            refs.malformed = true;
                        }
                    } else {
                        let item = Item {
                            key: *key,
                            transid: 0,
                            data,
                        };
                        if let Some(refs) = data_extent(item) {
                            current = Some((key.objectid, refs));
                        }
                    }
                }
                EXTENT_DATA_REF_KEY | SHARED_DATA_REF_KEY => {
                    if let Some((bytenr, refs)) = current.as_mut()
                        && *bytenr == key.objectid
                    {
                        collect(
                            refs,
                            Item {
                                key: *key,
                                transid: 0,
                                data,
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        walk.add(&batch_stats);
        boundary.took(&batch_stats);
        if let ControlFlow::Break(reason) = boundary.after_batch() {
            return Err(WalkError::Stopped(reason));
        }
        match last.successor() {
            Some(next) if next <= range.max => request.min = next,
            _ => break,
        }
    }
    emit(source, boundary, &mut current, &mut on_extent)?;
    Ok(ResolveStats {
        ranges: 1,
        batches: walk.batches,
        items: walk.items,
        item_bytes: walk.item_bytes,
    })
}

/// Resolves sorted distinct candidates one at a time, without retaining a
/// batch of backref vectors. Callbacks may perform nested metadata searches.
pub fn resolve_each<S, C, I, F>(
    source: &mut S,
    covered: &[(u64, u64)],
    candidates: I,
    bytenr: impl Fn(&C) -> u64,
    boundary: &mut dyn BatchBoundary,
    mut on_extent: F,
) -> Result<ResolveStats, WalkError>
where
    S: MetadataSource + ?Sized,
    I: Iterator<Item = C>,
    F: FnMut(&mut S, C, Option<&ExtentRefs>, &mut dyn BatchBoundary) -> Result<(), WalkError>,
{
    let mut candidates = candidates.peekable();
    let mut stats = ResolveStats::default();
    for &(first, last) in covered {
        let range = SearchRange::new(
            EXTENT_TREE_OBJECTID,
            Key::new(first, 0, 0),
            Key::new(last, u8::MAX, u64::MAX),
        );
        let read = stream_range(
            source,
            range,
            boundary,
            |source, address, refs, boundary| {
                while candidates.peek().is_some_and(|c| bytenr(c) < address) {
                    on_extent(source, candidates.next().expect("peeked"), None, boundary)?;
                }
                if candidates.peek().is_some_and(|c| bytenr(c) == address) {
                    on_extent(source, candidates.next().expect("peeked"), refs, boundary)?;
                }
                Ok(())
            },
        )?;
        while candidates.peek().is_some_and(|c| bytenr(c) <= last) {
            on_extent(source, candidates.next().expect("peeked"), None, boundary)?;
        }
        stats.ranges += read.ranges;
        stats.batches += read.batches;
        stats.items += read.items;
        stats.item_bytes += read.item_bytes;
    }
    for candidate in candidates {
        on_extent(source, candidate, None, boundary)?;
    }
    Ok(stats)
}

fn data_extent(item: Item<'_>) -> Option<ExtentRefs> {
    match parse_extent_item(item.data) {
        Ok(extent) if extent.is_data() => Some(ExtentRefs {
            num_bytes: item.key.offset,
            refs: extent.refs,
            generation: extent.generation,
            backrefs: extent.inline,
            malformed: false,
        }),
        Err(_) if data_flag(item.data) => Some(ExtentRefs {
            num_bytes: item.key.offset,
            malformed: true,
            ..ExtentRefs::default()
        }),
        _ => None,
    }
}

fn data_flag(data: &[u8]) -> bool {
    data.get(16..24)
        .and_then(|flags| flags.try_into().ok())
        .map(u64::from_le_bytes)
        .is_some_and(|flags| flags & EXTENT_FLAG_DATA != 0)
}

fn emit<S, F>(
    source: &mut S,
    boundary: &mut dyn BatchBoundary,
    current: &mut Option<(u64, ExtentRefs)>,
    on_extent: &mut F,
) -> Result<(), WalkError>
where
    S: MetadataSource + ?Sized,
    F: FnMut(&mut S, u64, Option<&ExtentRefs>, &mut dyn BatchBoundary) -> Result<(), WalkError>,
{
    if let Some((bytenr, refs)) = current.take() {
        on_extent(source, bytenr, Some(&refs), boundary)?;
    }
    Ok(())
}

/// Calls `on_extent` once per candidate, in order, with its references
/// or `None` when the extent tree has no item for it. `candidates` must
/// be sorted and distinct.
pub fn resolve<S, I>(
    source: &mut S,
    candidates: I,
    plan: Plan,
    boundary: &mut dyn BatchBoundary,
    on_extent: impl FnMut(u64, Option<&ExtentRefs>),
) -> Result<ResolveStats, WalkError>
where
    S: MetadataSource + ?Sized,
    I: Iterator<Item = u64> + Clone,
{
    let ranges = ranges(candidates.clone(), plan);
    resolve_in(source, &ranges, candidates, |&c| c, boundary, on_extent)
}

/// `resolve` for candidates identified by `bytenr` and covered by
/// `ranges`, handing each candidate itself to `on_extent`.
pub fn resolve_in<S, C, I>(
    source: &mut S,
    ranges: &[(u64, u64)],
    candidates: I,
    bytenr: impl Fn(&C) -> u64,
    boundary: &mut dyn BatchBoundary,
    on_extent: impl FnMut(C, Option<&ExtentRefs>),
) -> Result<ResolveStats, WalkError>
where
    S: MetadataSource + ?Sized,
    I: Iterator<Item = C>,
{
    let mut stats = ResolveStats::default();
    let mut join = Join {
        candidates: candidates.peekable(),
        bytenr,
        current: None,
        on_extent,
    };
    for &(first, last) in ranges {
        let range = SearchRange::new(
            EXTENT_TREE_OBJECTID,
            Key::new(first, 0, 0),
            Key::new(last, u8::MAX, u64::MAX),
        );
        let walk = walk_tree(source, range, boundary, |item| {
            join.visit(item);
            Ok(())
        })?;
        join.flush_through(last);
        stats.ranges += 1;
        stats.batches += walk.batches;
        stats.items += walk.items;
        stats.item_bytes += walk.item_bytes;
    }
    join.flush_through(u64::MAX);
    Ok(stats)
}

/// Merges the extent-tree item stream with the candidate list.
struct Join<C, I: Iterator<Item = C>, B, F> {
    candidates: Peekable<I>,
    bytenr: B,
    /// The candidate whose items are being collected.
    current: Option<(C, ExtentRefs)>,
    on_extent: F,
}

impl<C, I, B, F> Join<C, I, B, F>
where
    I: Iterator<Item = C>,
    B: Fn(&C) -> u64,
    F: FnMut(C, Option<&ExtentRefs>),
{
    fn visit(&mut self, item: Item<'_>) {
        let bytenr = item.key.objectid;
        if self
            .current
            .as_ref()
            .is_some_and(|(c, _)| (self.bytenr)(c) != bytenr)
        {
            self.emit_current();
        }
        if self.current.is_none() {
            self.skip_missing_before(bytenr);
            if self.peek() != Some(bytenr) {
                return;
            }
            let candidate = self.candidates.next().expect("peeked");
            let refs = ExtentRefs {
                malformed: true, // until its EXTENT_ITEM is seen
                ..ExtentRefs::default()
            };
            self.current = Some((candidate, refs));
        }
        let (_, refs) = self.current.as_mut().expect("collecting");
        collect(refs, item);
    }

    fn peek(&mut self) -> Option<u64> {
        self.candidates.peek().map(&self.bytenr)
    }

    fn emit_current(&mut self) {
        if let Some((candidate, refs)) = self.current.take() {
            (self.on_extent)(candidate, Some(&refs));
        }
    }

    fn skip_missing_before(&mut self, bytenr: u64) {
        while self.peek().is_some_and(|c| c < bytenr) {
            let candidate = self.candidates.next().expect("peeked");
            (self.on_extent)(candidate, None);
        }
    }

    /// Ends the current extent and every candidate up to `last`.
    fn flush_through(&mut self, last: u64) {
        self.emit_current();
        self.skip_missing_before(last);
        if self.peek() == Some(last) {
            let candidate = self.candidates.next().expect("peeked");
            (self.on_extent)(candidate, None);
        }
    }
}

fn collect(refs: &mut ExtentRefs, item: Item<'_>) {
    match item.key.item_type {
        EXTENT_ITEM_KEY => match parse_extent_item(item.data) {
            Ok(extent) if extent.is_data() && refs.backrefs.is_empty() => {
                refs.num_bytes = item.key.offset;
                refs.refs = extent.refs;
                refs.generation = extent.generation;
                refs.backrefs = extent.inline;
                refs.malformed = false;
            }
            _ => refs.malformed = true,
        },
        EXTENT_DATA_REF_KEY | SHARED_DATA_REF_KEY => {
            match parse_keyed_backref(item.key, item.data) {
                Ok(backref) => refs.backrefs.push(backref),
                Err(_) => refs.malformed = true,
            }
        }
        // Tree-block items and backrefs, block groups, owner refs.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::backref::testing::*;
    use crate::btrfs::keys::Key;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};

    const MIB: u64 = 1 << 20;

    fn direct(root: u64, objectid: u64, count: u32) -> DataBackref {
        DataBackref::Direct {
            root,
            objectid,
            offset: 0,
            count,
        }
    }

    fn extent(bytenr: u64, len: u64, refs: u64, inline: &[DataBackref]) -> (Key, Vec<u8>) {
        (
            Key::new(bytenr, EXTENT_ITEM_KEY, len),
            encode_data_extent(refs, inline),
        )
    }

    #[test]
    fn coalesces_nearby_extents_into_ranges() {
        let candidates = [MIB, 2 * MIB, 40 * MIB, 41 * MIB, 100 * MIB];
        let plan = Plan::Coalesce { max_gap: 16 * MIB };
        assert_eq!(
            ranges(candidates.iter().copied(), plan),
            vec![(MIB, 2 * MIB), (40 * MIB, 41 * MIB), (100 * MIB, 100 * MIB)]
        );
        assert_eq!(
            ranges(candidates.iter().copied(), Plan::Stream),
            vec![(MIB, 100 * MIB)]
        );
        assert!(ranges(std::iter::empty(), Plan::Stream).is_empty());
    }

    #[test]
    fn reports_each_candidate_once_in_order_with_all_its_backrefs() {
        let a = direct(257, 300, 1);
        let shared = DataBackref::Shared {
            parent: 90 * MIB,
            count: 1,
        };
        let keyed = direct(258, 301, 2);
        let items = vec![
            extent(MIB, 4096, 3, &[a]),
            // Not a candidate: skipped.
            extent(MIB + 4096, 4096, 1, &[a]),
            extent(3 * MIB, 8192, 3, &[shared]),
            (
                Key::new(3 * MIB, EXTENT_DATA_REF_KEY, 77),
                encode_data_ref(&keyed),
            ),
            (Key::new(3 * MIB, METADATA_ITEM_KEY, 0), vec![0; 33]),
        ];
        let mut source = FakeSource::new(items.clone(), 2);
        let mut seen = Vec::new();
        let stats = resolve(
            &mut source,
            [MIB, 2 * MIB, 3 * MIB].into_iter(),
            Plan::Stream,
            &mut NoStop,
            |bytenr, refs| seen.push((bytenr, refs.cloned())),
        )
        .unwrap();
        assert_eq!(
            seen,
            vec![
                (
                    MIB,
                    Some(ExtentRefs {
                        num_bytes: 4096,
                        refs: 3,
                        generation: 7,
                        backrefs: vec![a],
                        malformed: false,
                    })
                ),
                (2 * MIB, None),
                (
                    3 * MIB,
                    Some(ExtentRefs {
                        num_bytes: 8192,
                        refs: 3,
                        generation: 7,
                        backrefs: vec![shared, keyed],
                        malformed: false,
                    })
                ),
            ]
        );
        assert_eq!(stats.ranges, 1);
        assert!(stats.item_bytes > 0);
        assert!(
            source
                .calls
                .iter()
                .all(|r| r.tree_id == EXTENT_TREE_OBJECTID)
        );
        let mut source = FakeSource::new(items, 2);
        let mut streamed = Vec::new();
        resolve_each(
            &mut source,
            &[(MIB, 3 * MIB)],
            [MIB, 2 * MIB, 3 * MIB].into_iter(),
            |&address| address,
            &mut NoStop,
            |source, address, refs, _| {
                // A nested search overwrites the source's borrowed buffer.
                source.search(&SearchRange::new(900, Key::MIN, Key::MAX))?;
                streamed.push((address, refs.cloned()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(streamed, seen);
    }

    #[test]
    fn candidates_past_the_last_item_are_missing() {
        let items = vec![extent(MIB, 4096, 1, &[direct(257, 300, 1)])];
        let mut source = FakeSource::new(items, 8);
        let mut seen = Vec::new();
        let plan = Plan::Coalesce { max_gap: 0 };
        let stats = resolve(
            &mut source,
            [MIB, 5 * MIB].into_iter(),
            plan,
            &mut NoStop,
            |b, r| seen.push((b, r.is_some())),
        )
        .unwrap();
        assert_eq!(seen, vec![(MIB, true), (5 * MIB, false)]);
        assert_eq!(stats.ranges, 2);
    }

    #[test]
    fn unparsable_references_mark_the_extent_malformed() {
        let mut bad = encode_data_extent(1, &[]);
        bad.push(TREE_BLOCK_REF_KEY);
        let items = vec![
            (Key::new(MIB, EXTENT_ITEM_KEY, 4096), bad),
            extent(2 * MIB, 4096, 1, &[]),
            (Key::new(2 * MIB, SHARED_DATA_REF_KEY, 5), vec![1]),
        ];
        let mut source = FakeSource::new(items, 8);
        let mut malformed = Vec::new();
        resolve(
            &mut source,
            [MIB, 2 * MIB].into_iter(),
            Plan::Stream,
            &mut NoStop,
            |_, r| malformed.push(r.unwrap().malformed),
        )
        .unwrap();
        assert_eq!(malformed, vec![true, true]);
    }

    #[test]
    fn streams_every_data_extent_with_its_keyed_references() {
        let a = direct(257, 300, 1);
        let b = direct(258, 301, 2);
        let items = vec![
            extent(MIB, 4096, 1, &[a]),
            (Key::new(MIB, EXTENT_DATA_REF_KEY, 77), encode_data_ref(&b)),
            // A metadata extent is not part of the data-space total.
            (Key::new(2 * MIB, METADATA_ITEM_KEY, 0), vec![0; 33]),
            extent(3 * MIB, 8192, 2, &[]),
            (Key::new(3 * MIB, SHARED_DATA_REF_KEY, 5), vec![1]),
        ];
        let mut source = FakeSource::new(items, 2);
        let mut seen = Vec::new();
        let stats = stream_data(&mut source, &mut NoStop, |_, bytenr, refs, _| {
            seen.push((bytenr, refs.cloned()));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            seen,
            vec![
                (
                    MIB,
                    Some(ExtentRefs {
                        num_bytes: 4096,
                        refs: 1,
                        generation: 7,
                        backrefs: vec![a, b],
                        malformed: false,
                    })
                ),
                (
                    3 * MIB,
                    Some(ExtentRefs {
                        num_bytes: 8192,
                        refs: 2,
                        generation: 7,
                        backrefs: Vec::new(),
                        malformed: true,
                    })
                ),
            ]
        );
        assert_eq!(stats.ranges, 1);
        assert!(stats.batches >= 2);
    }

    #[test]
    fn streaming_reuses_copy_storage_across_nested_searches() {
        struct CopyBudget(u64);
        impl BatchBoundary for CopyBudget {
            fn after_batch(&mut self) -> ControlFlow<crate::btrfs::walk::StopReason> {
                ControlFlow::Continue(())
            }
            fn reserve(&mut self, bytes: u64) -> ControlFlow<crate::btrfs::walk::StopReason> {
                self.0 += bytes;
                if self.0 > 64 << 10 {
                    ControlFlow::Break(crate::btrfs::walk::StopReason::ResourceLimit)
                } else {
                    ControlFlow::Continue(())
                }
            }
        }
        let items = (1..=4096)
            .map(|n| extent(n * 4096, 4096, 1, &[direct(257, n, 1)]))
            .collect();
        let mut source = FakeSource::new(items, 32);
        let mut seen = 0;
        stream_data(
            &mut source,
            &mut CopyBudget(0),
            |source, address, refs, _| {
                assert_eq!(refs.unwrap().backrefs, [direct(257, address / 4096, 1)]);
                source.search(&SearchRange::new(900, Key::MIN, Key::MAX))?;
                seen += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, 4096);
    }

    #[test]
    fn stream_callback_can_resolve_another_extent_tree_range_before_resume() {
        let items = vec![
            extent(MIB, 4096, 1, &[direct(257, 300, 1)]),
            extent(3 * MIB, 8192, 1, &[direct(257, 301, 1)]),
        ];
        let mut source = FakeSource::new(items, 1);
        let mut seen = Vec::new();
        stream_data(&mut source, &mut NoStop, |source, bytenr, refs, _| {
            if bytenr == MIB {
                let range = SearchRange::new(
                    EXTENT_TREE_OBJECTID,
                    Key::new(3 * MIB, EXTENT_ITEM_KEY, 0),
                    Key::new(3 * MIB, EXTENT_ITEM_KEY, u64::MAX),
                );
                source.search(&range)?;
            }
            seen.push((bytenr, refs.is_some()));
            Ok(())
        })
        .unwrap();

        assert_eq!(seen, [(MIB, true), (3 * MIB, true)]);
        assert!(source.calls.len() > 2);
    }
}
