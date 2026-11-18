//! The chunk tree (tree 3): where a logical address lies on a device.
//! Only profiles that keep a whole copy in every stripe (single, DUP,
//! RAID1 and its variants) are mapped; striped profiles are refused.

use crate::btrfs::bytes::LeReader;
use crate::btrfs::keys::Key;
use crate::btrfs::parser::ItemError;
use crate::btrfs::search::SearchRange;
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_tree};
use std::collections::BTreeMap;

pub const CHUNK_TREE_OBJECTID: u64 = 3;
pub const FIRST_CHUNK_TREE_OBJECTID: u64 = 256;
pub const CHUNK_ITEM_KEY: u8 = 228;
pub const BLOCK_GROUP_METADATA: u64 = 1 << 2;
pub const BLOCK_GROUP_RAID0: u64 = 1 << 3;
pub const BLOCK_GROUP_RAID1: u64 = 1 << 4;
pub const BLOCK_GROUP_DUP: u64 = 1 << 5;
pub const BLOCK_GROUP_RAID10: u64 = 1 << 6;
pub const BLOCK_GROUP_RAID5: u64 = 1 << 7;
pub const BLOCK_GROUP_RAID6: u64 = 1 << 8;
/// Profiles that spread one copy over several stripes.
const STRIPED: u64 = BLOCK_GROUP_RAID0 | BLOCK_GROUP_RAID10 | BLOCK_GROUP_RAID5 | BLOCK_GROUP_RAID6;
/// `struct btrfs_chunk` before its stripes.
const CHUNK_LEN: usize = 48;
/// Size of `struct btrfs_stripe`.
const STRIPE_LEN: usize = 32;

/// Why a logical range has no device offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    #[error("no chunk holds the whole range")]
    Unmapped,
    #[error("the chunk spreads its copy over several stripes")]
    Striped,
    #[error("the chunk has no stripe on the device")]
    NoStripe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunk {
    length: u64,
    striped: bool,
    /// `(devid, offset)` of each stripe.
    stripes: Vec<(u64, u64)>,
}

/// Every chunk of a filesystem by logical start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkMap {
    chunks: BTreeMap<u64, Chunk>,
}

impl ChunkMap {
    /// Reads every `CHUNK_ITEM` of the chunk tree.
    pub fn load<S: MetadataSource + ?Sized>(
        source: &mut S,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Self, WalkError> {
        let range = SearchRange::new(
            CHUNK_TREE_OBJECTID,
            Key::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, 0),
            Key::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, u64::MAX),
        );
        let mut chunks = BTreeMap::new();
        walk_tree(source, range, boundary, |item| {
            let chunk = parse_chunk(item.data).map_err(|source| WalkError::Item {
                key: item.key,
                source,
            })?;
            chunks.insert(item.key.offset, chunk);
            Ok(())
        })?;
        Ok(ChunkMap { chunks })
    }

    /// The offset on device `devid` of `len` bytes at `logical`.
    pub fn physical(&self, logical: u64, len: u64, devid: u64) -> Result<u64, MapError> {
        let (&start, chunk) = self
            .chunks
            .range(..=logical)
            .next_back()
            .ok_or(MapError::Unmapped)?;
        let within = logical - start;
        if within.checked_add(len).is_none_or(|end| end > chunk.length) {
            return Err(MapError::Unmapped);
        }
        if chunk.striped {
            return Err(MapError::Striped);
        }
        let &(_, offset) = chunk
            .stripes
            .iter()
            .find(|&&(id, _)| id == devid)
            .ok_or(MapError::NoStripe)?;
        offset.checked_add(within).ok_or(MapError::Unmapped)
    }
}

fn parse_chunk(data: &[u8]) -> Result<Chunk, ItemError> {
    let mut r = LeReader::new(data);
    let length = r.u64()?;
    r.skip(16)?;
    let profile = r.u64()?;
    r.skip(12)?;
    let num_stripes = r.u16()?;
    r.skip(2)?;
    let expected = CHUNK_LEN + usize::from(num_stripes) * STRIPE_LEN;
    if data.len() != expected {
        return Err(ItemError::Length {
            expected,
            actual: data.len(),
        });
    }
    let stripes = (0..num_stripes)
        .map(|_| {
            let devid = r.u64()?;
            let offset = r.u64()?;
            r.skip(16)?;
            Ok((devid, offset))
        })
        .collect::<Result<_, ItemError>>()?;
    Ok(Chunk {
        length,
        striped: profile & STRIPED != 0,
        stripes,
    })
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A `btrfs_chunk` payload of `profile` with `stripes` as
    /// `(devid, offset)`.
    pub fn chunk_item(length: u64, profile: u64, stripes: &[(u64, u64)]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(length.to_le_bytes());
        data.extend(2u64.to_le_bytes());
        data.extend((64u64 << 10).to_le_bytes());
        data.extend((BLOCK_GROUP_METADATA | profile).to_le_bytes());
        data.extend([0u8; 12]);
        data.extend((stripes.len() as u16).to_le_bytes());
        data.extend(0u16.to_le_bytes());
        for &(devid, offset) in stripes {
            data.extend(devid.to_le_bytes());
            data.extend(offset.to_le_bytes());
            data.extend([0u8; 16]);
        }
        data
    }
}

#[cfg(test)]
mod tests {
    use super::testing::chunk_item;
    use super::*;
    use crate::btrfs::walk::testing::{FakeTrees, NoStop};

    const MIB: u64 = 1 << 20;

    fn map(chunks: Vec<(u64, Vec<u8>)>) -> ChunkMap {
        let items = chunks
            .into_iter()
            .map(|(logical, data)| {
                (
                    Key::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, logical),
                    data,
                )
            })
            .collect();
        let mut source = FakeTrees::new([(CHUNK_TREE_OBJECTID, items)]);
        ChunkMap::load(&mut source, &mut NoStop).unwrap()
    }

    #[test]
    fn maps_an_address_to_its_offset_in_the_chunk_stripe() {
        let map = map(vec![
            (30 * MIB, chunk_item(8 * MIB, 0, &[(1, 100 * MIB)])),
            (
                38 * MIB,
                chunk_item(8 * MIB, BLOCK_GROUP_DUP, &[(1, 200 * MIB), (1, 300 * MIB)]),
            ),
        ]);

        assert_eq!(map.physical(31 * MIB, 16384, 1), Ok(101 * MIB));
        assert_eq!(map.physical(40 * MIB, 16384, 1), Ok(202 * MIB));
    }

    #[test]
    fn uses_the_stripe_on_the_requested_device() {
        let map = map(vec![(
            30 * MIB,
            chunk_item(
                8 * MIB,
                BLOCK_GROUP_RAID1,
                &[(2, 500 * MIB), (1, 100 * MIB)],
            ),
        )]);

        assert_eq!(map.physical(30 * MIB, 16384, 1), Ok(100 * MIB));
        assert_eq!(map.physical(30 * MIB, 16384, 3), Err(MapError::NoStripe));
    }

    #[test]
    fn refuses_addresses_outside_every_chunk_or_across_its_end() {
        let map = map(vec![(30 * MIB, chunk_item(8 * MIB, 0, &[(1, 100 * MIB)]))]);

        assert_eq!(map.physical(29 * MIB, 16384, 1), Err(MapError::Unmapped));
        assert_eq!(
            map.physical(38 * MIB - 4096, 16384, 1),
            Err(MapError::Unmapped)
        );
    }

    #[test]
    fn refuses_striped_profiles() {
        for profile in [
            BLOCK_GROUP_RAID0,
            BLOCK_GROUP_RAID10,
            BLOCK_GROUP_RAID5,
            BLOCK_GROUP_RAID6,
        ] {
            let map = map(vec![(
                30 * MIB,
                chunk_item(8 * MIB, profile, &[(1, 100 * MIB), (1, 200 * MIB)]),
            )]);

            assert_eq!(map.physical(30 * MIB, 16384, 1), Err(MapError::Striped));
        }
    }

    #[test]
    fn a_malformed_chunk_item_is_an_error() {
        let items = vec![(
            Key::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, 0),
            chunk_item(MIB, 0, &[(1, 0)])[..60].to_vec(),
        )];
        let mut source = FakeTrees::new([(CHUNK_TREE_OBJECTID, items)]);

        assert!(ChunkMap::load(&mut source, &mut NoStop).is_err());
    }
}
