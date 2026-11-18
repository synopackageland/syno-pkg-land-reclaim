//! Data checksums from the checksum tree (tree 7), read with
//! TREE_SEARCH_V2: one checksum per data sector, keyed by the logical
//! address of the first sector an item covers. No file data is read.

use crate::btrfs::keys::Key;
use crate::btrfs::search::SearchRange;
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_tree};

pub const CSUM_TREE_OBJECTID: u64 = 7;
/// `BTRFS_EXTENT_CSUM_OBJECTID` (-10).
pub const EXTENT_CSUM_OBJECTID: u64 = (-10i64) as u64;
pub const EXTENT_CSUM_KEY: u8 = 128;
/// Leaf header and one item header: what a checksum item cannot use of
/// a leaf.
const LEAF_OVERHEAD: u64 = 101 + 25;

/// How checksums are laid out on one filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub sector: u64,
    pub csum_size: usize,
    /// How far before an address the item covering it may start: what
    /// one leaf-sized item covers.
    pub lookback: u64,
}

impl Layout {
    pub fn new(sector: u32, nodesize: u32, csum_size: usize) -> Self {
        let per_item = (u64::from(nodesize).saturating_sub(LEAF_OVERHEAD)) / csum_size as u64;
        Layout {
            sector: u64::from(sector),
            csum_size,
            lookback: per_item * u64::from(sector),
        }
    }
}

/// The checksums of the sectors of `[start, start + len)`, which must be
/// sector aligned, concatenated in address order; `None` when a sector
/// has none (NOCOW or `nodatasum` data). Counts its searches.
pub fn read<S: MetadataSource + ?Sized>(
    source: &mut S,
    layout: &Layout,
    start: u64,
    len: u64,
    boundary: &mut dyn BatchBoundary,
    searches: &mut u64,
) -> Result<Option<Vec<u8>>, WalkError> {
    if len == 0 {
        return Ok(Some(Vec::new()));
    }
    let sectors = len.div_ceil(layout.sector) as usize;
    let size = layout.csum_size;
    let mut out = vec![0u8; sectors * size];
    let mut filled = vec![false; sectors];
    let end = start.saturating_add(len);
    let range = SearchRange::new(
        CSUM_TREE_OBJECTID,
        Key::new(
            EXTENT_CSUM_OBJECTID,
            EXTENT_CSUM_KEY,
            start.saturating_sub(layout.lookback),
        ),
        Key::new(EXTENT_CSUM_OBJECTID, EXTENT_CSUM_KEY, end - 1),
    );
    let stats = walk_tree(source, range, boundary, |item| {
        if item.key.objectid != EXTENT_CSUM_OBJECTID || item.key.item_type != EXTENT_CSUM_KEY {
            return Ok(());
        }
        let first = item.key.offset;
        for (i, csum) in item.data.chunks_exact(size).enumerate() {
            let at = first.saturating_add(i as u64 * layout.sector);
            if at < start {
                continue;
            }
            if at >= end {
                break;
            }
            let sector = ((at - start) / layout.sector) as usize;
            out[sector * size..(sector + 1) * size].copy_from_slice(csum);
            filled[sector] = true;
        }
        Ok(())
    })?;
    *searches += stats.batches.max(1);
    Ok(filled.iter().all(|&f| f).then_some(out))
}

/// Ranges closer than this share one search.
const SPAN_GAP: u64 = 1 << 20;

/// The checksums of many sector-aligned ranges `(start, len)`, each like
/// [`read`], with one search per span of ranges less than 1 MiB apart:
/// for folders of many small files.
pub fn read_many<S: MetadataSource + ?Sized>(
    source: &mut S,
    layout: &Layout,
    ranges: &[(u64, u64)],
    boundary: &mut dyn BatchBoundary,
    searches: &mut u64,
) -> Result<Vec<Option<Vec<u8>>>, WalkError> {
    let size = layout.csum_size;
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&i| ranges[i]);
    let mut out: Vec<Vec<u8>> = ranges
        .iter()
        .map(|&(_, len)| vec![0u8; len.div_ceil(layout.sector) as usize * size])
        .collect();
    let mut filled: Vec<Vec<bool>> = ranges
        .iter()
        .map(|&(_, len)| vec![false; len.div_ceil(layout.sector) as usize])
        .collect();
    let mut at = 0;
    while at < order.len() {
        // One span: ranges starting within SPAN_GAP of the span's end.
        let first = ranges[order[at]].0;
        let mut end = first + ranges[order[at]].1;
        let mut last = at + 1;
        while last < order.len() && ranges[order[last]].0 <= end.saturating_add(SPAN_GAP) {
            end = end.max(ranges[order[last]].0 + ranges[order[last]].1);
            last += 1;
        }
        let members = &order[at..last];
        if end > first {
            let range = SearchRange::new(
                CSUM_TREE_OBJECTID,
                Key::new(
                    EXTENT_CSUM_OBJECTID,
                    EXTENT_CSUM_KEY,
                    first.saturating_sub(layout.lookback),
                ),
                Key::new(EXTENT_CSUM_OBJECTID, EXTENT_CSUM_KEY, end - 1),
            );
            let stats = walk_tree(source, range, boundary, |item| {
                if item.key.objectid != EXTENT_CSUM_OBJECTID
                    || item.key.item_type != EXTENT_CSUM_KEY
                {
                    return Ok(());
                }
                let item_start = item.key.offset;
                let item_end =
                    item_start.saturating_add((item.data.len() / size) as u64 * layout.sector);
                for &r in members {
                    let (start, len) = ranges[r];
                    let lo = start.max(item_start);
                    let hi = (start + len).min(item_end);
                    let mut a = lo;
                    while a < hi {
                        let src = ((a - item_start) / layout.sector) as usize * size;
                        let dst = ((a - start) / layout.sector) as usize;
                        out[r][dst * size..(dst + 1) * size]
                            .copy_from_slice(&item.data[src..src + size]);
                        filled[r][dst] = true;
                        a += layout.sector;
                    }
                }
                Ok(())
            })?;
            *searches += stats.batches.max(1);
        }
        at = last;
    }
    Ok(out
        .into_iter()
        .zip(filled)
        .map(|(c, f)| f.iter().all(|&x| x).then_some(c))
        .collect())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::model::testing::TreeItem;

    /// Checksum items of `sectors` 4 KiB sectors starting at `bytenr`,
    /// each checksum the sector's index plus `seed`, split into items of
    /// at most `per_item` sectors.
    pub fn csum_items(bytenr: u64, sectors: u32, seed: u32, per_item: u32) -> Vec<TreeItem> {
        let mut items = Vec::new();
        let mut i = 0;
        while i < sectors {
            let n = per_item.min(sectors - i);
            let data: Vec<u8> = (i..i + n)
                .flat_map(|s| (s.wrapping_add(seed)).to_le_bytes())
                .collect();
            items.push((
                Key::new(
                    EXTENT_CSUM_OBJECTID,
                    EXTENT_CSUM_KEY,
                    bytenr + u64::from(i) * 4096,
                ),
                data,
            ));
            i += n;
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};

    fn layout() -> Layout {
        Layout::new(4096, 16384, 4)
    }

    #[test]
    fn a_layout_looks_back_as_far_as_one_leaf_item_reaches() {
        // (16384 - 126) / 4 = 4064 checksums of 4 KiB each.
        assert_eq!(layout().lookback, 4064 * 4096);
    }

    #[test]
    fn reads_the_checksums_of_a_range_starting_inside_an_item() {
        let mut items = csum_items(1 << 30, 64, 100, 16);
        items.extend(csum_items(2 << 30, 4, 7, 16));
        let mut source = FakeSource::new(items, 3);
        let mut searches = 0;
        let got = read(
            &mut source,
            &layout(),
            (1 << 30) + 10 * 4096,
            20 * 4096,
            &mut NoStop,
            &mut searches,
        )
        .unwrap()
        .unwrap();
        let expected: Vec<u8> = (10u32..30).flat_map(|s| (s + 100).to_le_bytes()).collect();
        assert_eq!(got, expected);
        assert!(searches >= 1);
    }

    #[test]
    fn many_ranges_share_searches_and_keep_their_own_gaps() {
        let mut items = csum_items(1 << 30, 8, 0, 3);
        items.extend(csum_items((1 << 30) + 12 * 4096, 4, 50, 3));
        let mut source = FakeSource::new(items, 100);
        let mut searches = 0;
        let ranges = [
            ((1 << 30) + 12 * 4096, 2 * 4096),
            (1 << 30, 2 * 4096),
            ((1 << 30) + 6 * 4096, 4 * 4096),
            ((1 << 30) + 4096, 4096),
        ];
        let got = read_many(&mut source, &layout(), &ranges, &mut NoStop, &mut searches).unwrap();
        let csums = |seed: u32, from: u32, n: u32| -> Vec<u8> {
            (from..from + n)
                .flat_map(|s| (s + seed).to_le_bytes())
                .collect()
        };
        assert_eq!(got[0], Some(csums(50, 0, 2)));
        assert_eq!(got[1], Some(csums(0, 0, 2)));
        // Sectors 8 and 9 have no checksum.
        assert_eq!(got[2], None);
        assert_eq!(got[3], Some(csums(0, 1, 1)));
        assert_eq!(searches, 1);
    }

    #[test]
    fn a_sector_without_a_checksum_leaves_the_range_unchecked() {
        let mut items = csum_items(1 << 30, 8, 0, 8);
        // Sectors 8..10 have no checksum: NOCOW data.
        items.extend(csum_items((1 << 30) + 10 * 4096, 6, 10, 8));
        let mut source = FakeSource::new(items, 100);
        let mut searches = 0;
        let got = read(
            &mut source,
            &layout(),
            1 << 30,
            16 * 4096,
            &mut NoStop,
            &mut searches,
        )
        .unwrap();
        assert_eq!(got, None);
    }
}
