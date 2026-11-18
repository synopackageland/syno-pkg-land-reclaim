//! Tree blocks read by address (SPEC 8.1): the header and the items of a
//! leaf, accepted only when checksum, address, filesystem and generation
//! prove the block is the one the extent tree names.

use crate::btrfs::bytes::LeReader;
use crate::btrfs::crc32c::crc32c;
use crate::btrfs::keys::Key;
use crate::btrfs::search::Item;

/// Checksum field at the start of every tree block.
const CSUM_LEN: usize = 32;
/// Size of `struct btrfs_header`.
pub const HEADER_LEN: usize = 101;
/// Size of `struct btrfs_item`.
const ITEM_LEN: usize = 25;

/// What the block at an address must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expected {
    pub bytenr: u64,
    /// Transaction that wrote the block, from its extent-tree item.
    pub generation: u64,
    pub fsid: [u8; 16],
}

/// Why a read block is not the expected leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlockError {
    #[error("tree block checksum does not match")]
    Checksum,
    #[error("tree block holds another address")]
    Address,
    #[error("tree block belongs to another filesystem")]
    Filesystem,
    #[error("tree block was written by another transaction")]
    Generation,
    #[error("tree block is a node, not a leaf")]
    NotLeaf,
    #[error("tree block items are malformed")]
    Malformed,
}

/// The items of the leaf `block`, each with the leaf's generation as
/// its `transid`, once `block` proves to be `expected`.
pub fn parse_leaf<'a>(block: &'a [u8], expected: &Expected) -> Result<Vec<Item<'a>>, BlockError> {
    let header = block.get(..HEADER_LEN).ok_or(BlockError::Malformed)?;
    let stored = u32::from_le_bytes(header[..4].try_into().expect("four bytes"));
    if stored != crc32c(&block[CSUM_LEN..]) {
        return Err(BlockError::Checksum);
    }
    let mut r = LeReader::new(&header[CSUM_LEN..]);
    let fsid = r.take(16).expect("in header");
    let bytenr = r.u64().expect("in header");
    r.skip(8 + 16).expect("in header");
    let generation = r.u64().expect("in header");
    r.skip(8).expect("in header");
    let nritems = r.u32().expect("in header");
    let level = r.u8().expect("in header");
    if bytenr != expected.bytenr {
        return Err(BlockError::Address);
    }
    if fsid != expected.fsid {
        return Err(BlockError::Filesystem);
    }
    if generation != expected.generation {
        return Err(BlockError::Generation);
    }
    if level != 0 {
        return Err(BlockError::NotLeaf);
    }
    leaf_items(block, nritems, generation)
}

fn leaf_items(block: &[u8], nritems: u32, transid: u64) -> Result<Vec<Item<'_>>, BlockError> {
    let data = &block[HEADER_LEN..];
    let count = nritems as usize;
    if count.saturating_mul(ITEM_LEN) > data.len() {
        return Err(BlockError::Malformed);
    }
    let mut r = LeReader::new(data);
    let mut items: Vec<Item<'_>> = Vec::with_capacity(count);
    for _ in 0..nritems {
        let (key, offset, size) = read_item(&mut r).ok_or(BlockError::Malformed)?;
        let end = offset.checked_add(size).ok_or(BlockError::Malformed)?;
        let bytes = data.get(offset..end).ok_or(BlockError::Malformed)?;
        if items.last().is_some_and(|prev| prev.key >= key) {
            return Err(BlockError::Malformed);
        }
        items.push(Item {
            key,
            transid,
            data: bytes,
        });
    }
    Ok(items)
}

fn read_item(r: &mut LeReader<'_>) -> Option<(Key, usize, usize)> {
    let objectid = r.u64().ok()?;
    let item_type = r.u8().ok()?;
    let offset = r.u64().ok()?;
    let data_offset = r.u32().ok()? as usize;
    let size = r.u32().ok()? as usize;
    Some((Key::new(objectid, item_type, offset), data_offset, size))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A leaf of `nodesize` bytes holding `items`, with a valid checksum.
    pub fn leaf(expected: &Expected, nodesize: usize, items: &[(Key, Vec<u8>)]) -> Vec<u8> {
        let mut block = vec![0u8; nodesize];
        block[32..48].copy_from_slice(&expected.fsid);
        block[48..56].copy_from_slice(&expected.bytenr.to_le_bytes());
        block[80..88].copy_from_slice(&expected.generation.to_le_bytes());
        block[88..96].copy_from_slice(&5u64.to_le_bytes());
        block[96..100].copy_from_slice(&(items.len() as u32).to_le_bytes());
        let mut data_end = nodesize - HEADER_LEN;
        for (index, (key, data)) in items.iter().enumerate() {
            data_end -= data.len();
            let at = HEADER_LEN + index * ITEM_LEN;
            block[at..at + 8].copy_from_slice(&key.objectid.to_le_bytes());
            block[at + 8] = key.item_type;
            block[at + 9..at + 17].copy_from_slice(&key.offset.to_le_bytes());
            block[at + 17..at + 21].copy_from_slice(&(data_end as u32).to_le_bytes());
            block[at + 21..at + 25].copy_from_slice(&(data.len() as u32).to_le_bytes());
            let start = HEADER_LEN + data_end;
            block[start..start + data.len()].copy_from_slice(data);
        }
        seal(&mut block);
        block
    }

    /// Rewrites the checksum after a test changed `block`.
    pub fn seal(block: &mut [u8]) {
        let csum = crate::btrfs::crc32c::crc32c(&block[CSUM_LEN..]);
        block[..4].copy_from_slice(&csum.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{leaf, seal};
    use super::*;

    const NODESIZE: usize = 4096;

    fn expected() -> Expected {
        Expected {
            bytenr: 30 << 20,
            generation: 7,
            fsid: [0xAB; 16],
        }
    }

    fn items() -> Vec<(Key, Vec<u8>)> {
        vec![
            (Key::new(257, 1, 0), vec![1, 2, 3]),
            (Key::new(257, 108, 0), vec![4; 53]),
        ]
    }

    #[test]
    fn reads_the_items_of_a_valid_leaf() {
        let block = leaf(&expected(), NODESIZE, &items());

        let parsed = parse_leaf(&block, &expected()).unwrap();

        let found: Vec<_> = parsed.iter().map(|i| (i.key, i.data.to_vec())).collect();
        assert_eq!(found, items());
        assert!(parsed.iter().all(|i| i.transid == 7));
    }

    #[test]
    fn refuses_a_block_whose_checksum_does_not_match() {
        let mut block = leaf(&expected(), NODESIZE, &items());
        block[200] ^= 1;

        assert_eq!(parse_leaf(&block, &expected()), Err(BlockError::Checksum));
    }

    #[test]
    fn refuses_a_block_of_another_address_filesystem_or_generation() {
        let block = leaf(&expected(), NODESIZE, &items());
        let moved = Expected {
            bytenr: expected().bytenr + 4096,
            ..expected()
        };
        let other_fs = Expected {
            fsid: [0xCD; 16],
            ..expected()
        };
        let rewritten = Expected {
            generation: 8,
            ..expected()
        };

        assert_eq!(parse_leaf(&block, &moved), Err(BlockError::Address));
        assert_eq!(parse_leaf(&block, &other_fs), Err(BlockError::Filesystem));
        assert_eq!(parse_leaf(&block, &rewritten), Err(BlockError::Generation));
    }

    #[test]
    fn refuses_a_node_above_the_leaves() {
        let mut block = leaf(&expected(), NODESIZE, &items());
        block[100] = 1;
        seal(&mut block);

        assert_eq!(parse_leaf(&block, &expected()), Err(BlockError::NotLeaf));
    }

    #[test]
    fn refuses_items_whose_data_leaves_the_block_or_keys_that_do_not_increase() {
        let mut beyond = leaf(&expected(), NODESIZE, &items());
        let size_at = HEADER_LEN + ITEM_LEN + 21;
        beyond[size_at..size_at + 4].copy_from_slice(&(NODESIZE as u32).to_le_bytes());
        seal(&mut beyond);
        let mut reversed = items();
        reversed.reverse();
        let unordered = leaf(&expected(), NODESIZE, &reversed);
        let mut crowded = leaf(&expected(), NODESIZE, &items());
        crowded[96..100].copy_from_slice(&(NODESIZE as u32).to_le_bytes());
        seal(&mut crowded);

        assert_eq!(parse_leaf(&beyond, &expected()), Err(BlockError::Malformed));
        assert_eq!(
            parse_leaf(&unordered, &expected()),
            Err(BlockError::Malformed)
        );
        assert_eq!(
            parse_leaf(&crowded, &expected()),
            Err(BlockError::Malformed)
        );
    }
}
