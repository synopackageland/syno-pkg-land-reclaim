//! TREE_SEARCH_V2 result buffers: a sequence of `btrfs_ioctl_search_header`
//! records, each followed by `len` bytes of item data (SPEC 8).

use crate::btrfs::bytes::LeReader;
use crate::btrfs::keys::Key;
use std::ops::Range;

/// Size of `struct btrfs_ioctl_search_header`.
pub const SEARCH_HEADER_LEN: usize = 32;

/// The inclusive lexicographic key range of one tree search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchRange {
    pub tree_id: u64,
    pub min: Key,
    pub max: Key,
    /// The kernel skips tree blocks last written before this
    /// transaction; 0 reads them all.
    pub min_transid: u64,
}

impl SearchRange {
    pub const fn new(tree_id: u64, min: Key, max: Key) -> Self {
        SearchRange {
            tree_id,
            min,
            max,
            min_transid: 0,
        }
    }

    /// This range, reading only tree blocks written in transaction
    /// `transid` or later (what `btrfs subvolume find-new` searches).
    pub const fn since(self, transid: u64) -> Self {
        SearchRange {
            min_transid: transid,
            ..self
        }
    }

    pub fn contains(&self, key: Key) -> bool {
        self.min <= key && key <= self.max
    }
}

/// One returned item; `data` borrows from the result buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    pub key: Key,
    pub transid: u64,
    pub data: &'a [u8],
}

/// ABI violations in a returned search buffer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    #[error("item {index}: search header truncated at byte {offset}")]
    TruncatedHeader { index: u32, offset: usize },
    #[error("item {index}: {len} data bytes at byte {offset} exceed the {available} available")]
    ItemOverrun {
        index: u32,
        offset: usize,
        len: u32,
        available: usize,
    },
    #[error("item {index}: key type {value} does not fit in a byte")]
    InvalidType { index: u32, value: u32 },
    #[error("item {index}: key {key:?} does not follow the previous key")]
    KeyNotIncreasing { index: u32, key: Key },
    #[error("item {index}: key {key:?} lies outside the requested range")]
    KeyOutOfRange { index: u32, key: Key },
}

/// Parses and validates the `nr_items` items at the start of `buf`.
pub fn parse_batch<'a>(
    buf: &'a [u8],
    nr_items: u32,
    range: &SearchRange,
) -> Result<Vec<Item<'a>>, BatchError> {
    let mut reader = LeReader::new(buf);
    let mut items: Vec<Item<'a>> = Vec::new();
    for index in 0..nr_items {
        let item = read_item(&mut reader, index)?;
        if !range.contains(item.key) {
            return Err(BatchError::KeyOutOfRange {
                index,
                key: item.key,
            });
        }
        if items.last().is_some_and(|prev| prev.key >= item.key) {
            return Err(BatchError::KeyNotIncreasing {
                index,
                key: item.key,
            });
        }
        items.push(item);
    }
    Ok(items)
}

/// Number of leading bytes of `buf` occupied by `nr_items` items.
pub fn batch_len(buf: &[u8], nr_items: u32) -> Result<usize, BatchError> {
    let mut reader = LeReader::new(buf);
    for index in 0..nr_items {
        read_item(&mut reader, index)?;
    }
    Ok(reader.position())
}

/// Keys and data byte ranges of the `nr_items` items at the start of `buf`,
/// for rewriting item data in place.
pub(crate) fn item_ranges(
    buf: &[u8],
    nr_items: u32,
) -> Result<Vec<(Key, Range<usize>)>, BatchError> {
    let mut reader = LeReader::new(buf);
    (0..nr_items)
        .map(|index| {
            let item = read_item(&mut reader, index)?;
            let end = reader.position();
            Ok((item.key, end - item.data.len()..end))
        })
        .collect()
}

fn read_item<'a>(reader: &mut LeReader<'a>, index: u32) -> Result<Item<'a>, BatchError> {
    let offset = reader.position();
    let truncated = |_| BatchError::TruncatedHeader { index, offset };
    let transid = reader.u64().map_err(truncated)?;
    let objectid = reader.u64().map_err(truncated)?;
    let key_offset = reader.u64().map_err(truncated)?;
    let raw_type = reader.u32().map_err(truncated)?;
    let len = reader.u32().map_err(truncated)?;
    let item_type = u8::try_from(raw_type).map_err(|_| BatchError::InvalidType {
        index,
        value: raw_type,
    })?;
    let data_offset = reader.position();
    let data = reader
        .take(len as usize)
        .map_err(|e| BatchError::ItemOverrun {
            index,
            offset: data_offset,
            len,
            available: e.available,
        })?;
    Ok(Item {
        key: Key::new(objectid, item_type, key_offset),
        transid,
        data,
    })
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::btrfs::keys::Key;

    /// Encodes one search header plus item data as the kernel returns it.
    pub fn encode_item(transid: u64, key: Key, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&transid.to_le_bytes());
        out.extend_from_slice(&key.objectid.to_le_bytes());
        out.extend_from_slice(&key.offset.to_le_bytes());
        out.extend_from_slice(&u32::from(key.item_type).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testing::encode_item;
    use super::*;
    use crate::btrfs::keys::Key;

    fn full_range() -> SearchRange {
        SearchRange::new(5, Key::MIN, Key::MAX)
    }

    #[test]
    fn a_range_reads_every_block_unless_limited_to_newer_ones() {
        assert_eq!(full_range().min_transid, 0);
        let newer = full_range().since(42);
        assert_eq!(newer.min_transid, 42);
        assert_eq!(
            (newer.tree_id, newer.min, newer.max),
            (5, Key::MIN, Key::MAX)
        );
    }

    #[test]
    fn parses_items_in_order() {
        let mut buf = encode_item(7, Key::new(256, 1, 0), &[1, 2, 3]);
        buf.extend(encode_item(8, Key::new(256, 96, 2), &[4]));
        let items = parse_batch(&buf, 2, &full_range()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, Key::new(256, 1, 0));
        assert_eq!(items[0].transid, 7);
        assert_eq!(items[0].data, &[1, 2, 3]);
        assert_eq!(items[1].key, Key::new(256, 96, 2));
        assert_eq!(items[1].data, &[4]);
    }

    #[test]
    fn ignores_unused_buffer_tail() {
        let mut buf = encode_item(7, Key::new(256, 1, 0), &[1]);
        buf.extend([0xAA; 64]);
        assert_eq!(parse_batch(&buf, 1, &full_range()).unwrap().len(), 1);
    }

    #[test]
    fn zero_items_is_empty() {
        assert!(parse_batch(&[], 0, &full_range()).unwrap().is_empty());
    }

    #[test]
    fn rejects_truncated_header() {
        let buf = encode_item(7, Key::new(256, 1, 0), &[]);
        assert_eq!(
            parse_batch(&buf[..31], 1, &full_range()),
            Err(BatchError::TruncatedHeader {
                index: 0,
                offset: 0
            })
        );
    }

    #[test]
    fn rejects_more_items_claimed_than_present() {
        let buf = encode_item(7, Key::new(256, 1, 0), &[1]);
        assert_eq!(
            parse_batch(&buf, 2, &full_range()),
            Err(BatchError::TruncatedHeader {
                index: 1,
                offset: 33
            })
        );
    }

    #[test]
    fn rejects_item_data_overrunning_buffer() {
        let mut buf = encode_item(7, Key::new(256, 1, 0), &[1, 2, 3]);
        buf.truncate(34);
        assert_eq!(
            parse_batch(&buf, 1, &full_range()),
            Err(BatchError::ItemOverrun {
                index: 0,
                offset: 32,
                len: 3,
                available: 2
            })
        );
    }

    #[test]
    fn rejects_item_type_wider_than_a_byte() {
        let mut buf = encode_item(7, Key::new(256, 1, 0), &[]);
        buf[24..28].copy_from_slice(&256u32.to_le_bytes());
        assert_eq!(
            parse_batch(&buf, 1, &full_range()),
            Err(BatchError::InvalidType {
                index: 0,
                value: 256
            })
        );
    }

    #[test]
    fn rejects_non_increasing_keys() {
        let mut buf = encode_item(7, Key::new(256, 96, 2), &[]);
        buf.extend(encode_item(7, Key::new(256, 96, 2), &[]));
        assert_eq!(
            parse_batch(&buf, 2, &full_range()),
            Err(BatchError::KeyNotIncreasing {
                index: 1,
                key: Key::new(256, 96, 2)
            })
        );
    }

    #[test]
    fn batch_len_covers_exactly_the_returned_items() {
        let mut buf = encode_item(7, Key::new(256, 1, 0), &[1, 2, 3]);
        buf.extend(encode_item(8, Key::new(256, 96, 2), &[4]));
        let used = buf.len();
        buf.extend([0; 100]);
        assert_eq!(batch_len(&buf, 2), Ok(used));
        assert_eq!(batch_len(&buf, 0), Ok(0));
    }

    #[test]
    fn item_ranges_locate_each_item_payload() {
        let mut buf = encode_item(7, Key::new(256, 1, 0), &[1, 2, 3]);
        buf.extend(encode_item(8, Key::new(256, 96, 2), &[4]));
        let first = SEARCH_HEADER_LEN..SEARCH_HEADER_LEN + 3;
        let second = 2 * SEARCH_HEADER_LEN + 3..2 * SEARCH_HEADER_LEN + 4;
        assert_eq!(
            item_ranges(&buf, 2),
            Ok(vec![
                (Key::new(256, 1, 0), first),
                (Key::new(256, 96, 2), second)
            ])
        );
        assert!(item_ranges(&buf, 3).is_err());
    }

    #[test]
    fn rejects_keys_outside_requested_range() {
        let range = SearchRange::new(5, Key::new(256, 0, 0), Key::new(300, 255, u64::MAX));
        let buf = encode_item(7, Key::new(301, 1, 0), &[]);
        assert_eq!(
            parse_batch(&buf, 1, &range),
            Err(BatchError::KeyOutOfRange {
                index: 0,
                key: Key::new(301, 1, 0)
            })
        );
    }
}
