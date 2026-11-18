//! Extent-tree (tree 2) items that establish global references to a data
//! extent (SPEC 8.1): `EXTENT_ITEM` with its inline backrefs, and keyed
//! `EXTENT_DATA_REF`/`SHARED_DATA_REF` items.

use crate::btrfs::bytes::LeReader;
use crate::btrfs::keys::Key;
use crate::btrfs::parser::ItemError;

pub const EXTENT_TREE_OBJECTID: u64 = 2;
pub const EXTENT_ITEM_KEY: u8 = 168;
pub const METADATA_ITEM_KEY: u8 = 169;
/// Simple-quota owner of a data extent; not counted in `refs`.
pub const EXTENT_OWNER_REF_KEY: u8 = 172;
pub const TREE_BLOCK_REF_KEY: u8 = 176;
pub const EXTENT_DATA_REF_KEY: u8 = 178;
pub const SHARED_BLOCK_REF_KEY: u8 = 182;
pub const SHARED_DATA_REF_KEY: u8 = 184;
pub const EXTENT_FLAG_DATA: u64 = 1;
pub const EXTENT_FLAG_TREE_BLOCK: u64 = 2;

/// One backref to a data extent and the references it holds.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum DataBackref {
    /// `EXTENT_DATA_REF`: file extent items of inode `objectid` in tree
    /// `root` whose file offset minus extent offset is `offset`.
    Direct {
        root: u64,
        objectid: u64,
        offset: u64,
        count: u32,
    },
    /// `SHARED_DATA_REF`: file extent items in tree block `parent`, which
    /// is shared through snapshotting; its owner is not named.
    Shared { parent: u64, count: u32 },
}

impl DataBackref {
    pub fn count(&self) -> u32 {
        match *self {
            DataBackref::Direct { count, .. } | DataBackref::Shared { count, .. } => count,
        }
    }
}

/// An `EXTENT_ITEM` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentItem {
    /// Total references, inline and keyed.
    pub refs: u64,
    /// Transaction that allocated the extent.
    pub generation: u64,
    pub flags: u64,
    /// Inline data backrefs; empty for tree blocks.
    pub inline: Vec<DataBackref>,
}

impl ExtentItem {
    pub fn is_data(&self) -> bool {
        self.flags & EXTENT_FLAG_DATA != 0
    }
}

/// One reference to a tree block (SPEC 8.1 owner resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockBackref {
    /// `TREE_BLOCK_REF`: tree `root` reaches the block through its own
    /// path, and so may every root sharing that path.
    Root(u64),
    /// `SHARED_BLOCK_REF`: tree block `parent` points at it.
    Parent(u64),
}

/// `btrfs_tree_block_info` (first key, level) in a non-skinny item.
pub const TREE_BLOCK_INFO_LEN: usize = 18;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackrefError {
    #[error(transparent)]
    Item(#[from] ItemError),
    #[error("unexpected inline backref type {0}")]
    UnexpectedInline(u8),
    #[error("item type {0} is not a data backref")]
    NotABackref(u8),
    #[error("extent item is not a tree block")]
    NotATreeBlock,
}

impl From<crate::btrfs::bytes::Truncated> for BackrefError {
    fn from(t: crate::btrfs::bytes::Truncated) -> Self {
        BackrefError::Item(ItemError::Truncated(t))
    }
}

/// Parses an `EXTENT_ITEM`; only data extents' inline backrefs are read.
pub fn parse_extent_item(data: &[u8]) -> Result<ExtentItem, BackrefError> {
    let mut r = LeReader::new(data);
    let refs = r.u64()?;
    let generation = r.u64()?;
    let flags = r.u64()?;
    let mut item = ExtentItem {
        refs,
        generation,
        flags,
        inline: Vec::new(),
    };
    if !item.is_data() {
        return Ok(item);
    }
    while r.remaining() > 0 {
        match r.u8()? {
            EXTENT_DATA_REF_KEY => item.inline.push(read_data_ref(&mut r)?),
            SHARED_DATA_REF_KEY => {
                let parent = r.u64()?;
                let count = r.u32()?;
                item.inline.push(DataBackref::Shared { parent, count });
            }
            EXTENT_OWNER_REF_KEY => r.skip(8)?,
            other => return Err(BackrefError::UnexpectedInline(other)),
        }
    }
    Ok(item)
}

/// Parses the inline backrefs of a tree block's `EXTENT_ITEM`, or of
/// its skinny `METADATA_ITEM`, which has no `btrfs_tree_block_info`.
pub fn parse_tree_block_item(key: Key, data: &[u8]) -> Result<Vec<BlockBackref>, BackrefError> {
    let mut r = LeReader::new(data);
    let _refs = r.u64()?;
    let _generation = r.u64()?;
    if r.u64()? & EXTENT_FLAG_TREE_BLOCK == 0 {
        return Err(BackrefError::NotATreeBlock);
    }
    if key.item_type == EXTENT_ITEM_KEY {
        r.skip(TREE_BLOCK_INFO_LEN)?;
    }
    let mut inline = Vec::new();
    while r.remaining() > 0 {
        let kind = r.u8()?;
        let offset = r.u64()?;
        inline.push(block_backref(kind, offset)?);
    }
    Ok(inline)
}

/// Parses a keyed `TREE_BLOCK_REF` or `SHARED_BLOCK_REF` item; both are
/// named entirely by their key.
pub fn parse_keyed_block_backref(key: Key, data: &[u8]) -> Result<BlockBackref, BackrefError> {
    if !data.is_empty() {
        return Err(ItemError::TrailingBytes { count: data.len() }.into());
    }
    block_backref(key.item_type, key.offset).map_err(|_| BackrefError::NotABackref(key.item_type))
}

fn block_backref(kind: u8, offset: u64) -> Result<BlockBackref, BackrefError> {
    match kind {
        TREE_BLOCK_REF_KEY => Ok(BlockBackref::Root(offset)),
        SHARED_BLOCK_REF_KEY => Ok(BlockBackref::Parent(offset)),
        other => Err(BackrefError::UnexpectedInline(other)),
    }
}

/// Parses a keyed `EXTENT_DATA_REF` or `SHARED_DATA_REF` item.
pub fn parse_keyed_backref(key: Key, data: &[u8]) -> Result<DataBackref, BackrefError> {
    let mut r = LeReader::new(data);
    let backref = match key.item_type {
        EXTENT_DATA_REF_KEY => read_data_ref(&mut r)?,
        SHARED_DATA_REF_KEY => DataBackref::Shared {
            parent: key.offset,
            count: r.u32()?,
        },
        other => return Err(BackrefError::NotABackref(other)),
    };
    match r.remaining() {
        0 => Ok(backref),
        count => Err(ItemError::TrailingBytes { count }.into()),
    }
}

fn read_data_ref(r: &mut LeReader<'_>) -> Result<DataBackref, BackrefError> {
    Ok(DataBackref::Direct {
        root: r.u64()?,
        objectid: r.u64()?,
        offset: r.u64()?,
        count: r.u32()?,
    })
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A data `EXTENT_ITEM` payload with `inline` backrefs.
    pub fn encode_data_extent(refs: u64, inline: &[DataBackref]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [refs, 7, EXTENT_FLAG_DATA] {
            b.extend(v.to_le_bytes()); // refs, generation, flags
        }
        for r in inline {
            b.extend(encode_inline(r));
        }
        b
    }

    pub fn encode_inline(backref: &DataBackref) -> Vec<u8> {
        match *backref {
            DataBackref::Direct { .. } => {
                let mut b = vec![EXTENT_DATA_REF_KEY];
                b.extend(encode_data_ref(backref));
                b
            }
            DataBackref::Shared { parent, count } => {
                let mut b = vec![SHARED_DATA_REF_KEY];
                b.extend(parent.to_le_bytes());
                b.extend(count.to_le_bytes());
                b
            }
        }
    }

    /// A tree block's `EXTENT_ITEM` (`with_info`) or skinny
    /// `METADATA_ITEM` payload with `inline` backrefs.
    pub fn encode_tree_block(refs: u64, with_info: bool, inline: &[BlockBackref]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [refs, 7, EXTENT_FLAG_TREE_BLOCK] {
            b.extend(v.to_le_bytes());
        }
        if with_info {
            b.extend([0u8; TREE_BLOCK_INFO_LEN]);
        }
        for r in inline {
            let (kind, offset) = match *r {
                BlockBackref::Root(root) => (TREE_BLOCK_REF_KEY, root),
                BlockBackref::Parent(parent) => (SHARED_BLOCK_REF_KEY, parent),
            };
            b.push(kind);
            b.extend(offset.to_le_bytes());
        }
        b
    }

    /// The `btrfs_extent_data_ref` body of a direct backref.
    pub fn encode_data_ref(backref: &DataBackref) -> Vec<u8> {
        let DataBackref::Direct {
            root,
            objectid,
            offset,
            count,
        } = *backref
        else {
            panic!("not a direct backref");
        };
        let mut b = Vec::new();
        for v in [root, objectid, offset] {
            b.extend(v.to_le_bytes());
        }
        b.extend(count.to_le_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const DIRECT: DataBackref = DataBackref::Direct {
        root: 257,
        objectid: 300,
        offset: 0,
        count: 2,
    };
    const SHARED: DataBackref = DataBackref::Shared {
        parent: 30_000_000,
        count: 1,
    };

    #[test]
    fn parses_a_data_extent_item_with_inline_backrefs() {
        let item = parse_extent_item(&encode_data_extent(3, &[DIRECT, SHARED])).unwrap();
        assert_eq!(item.refs, 3);
        assert_eq!(item.generation, 7);
        assert!(item.is_data());
        assert_eq!(item.inline, vec![DIRECT, SHARED]);
    }

    #[test]
    fn tree_block_items_carry_no_data_backrefs() {
        let mut b = Vec::new();
        for v in [1u64, 7, EXTENT_FLAG_TREE_BLOCK] {
            b.extend(v.to_le_bytes());
        }
        b.extend([0u8; 18]); // btrfs_tree_block_info
        b.extend([TREE_BLOCK_REF_KEY]);
        b.extend(5u64.to_le_bytes());
        let item = parse_extent_item(&b).unwrap();
        assert!(!item.is_data());
        assert!(item.inline.is_empty());
    }

    #[test]
    fn skips_simple_quota_owner_refs() {
        let mut b = encode_data_extent(1, &[]);
        b.push(EXTENT_OWNER_REF_KEY);
        b.extend(257u64.to_le_bytes());
        b.extend(encode_inline(&DIRECT));
        let item = parse_extent_item(&b).unwrap();
        assert_eq!(item.inline, vec![DIRECT]);
    }

    #[test]
    fn unknown_inline_types_and_truncation_are_errors() {
        let mut unknown = encode_data_extent(1, &[]);
        unknown.push(TREE_BLOCK_REF_KEY);
        unknown.extend(5u64.to_le_bytes());
        assert_eq!(
            parse_extent_item(&unknown),
            Err(BackrefError::UnexpectedInline(TREE_BLOCK_REF_KEY))
        );
        let mut truncated = encode_data_extent(1, &[DIRECT]);
        truncated.pop();
        assert!(matches!(
            parse_extent_item(&truncated),
            Err(BackrefError::Item(ItemError::Truncated(_)))
        ));
        // EXTENT_ITEM_V0 is a bare u32 reference count.
        assert!(parse_extent_item(&1u32.to_le_bytes()).is_err());
    }

    #[test]
    fn parses_keyed_backrefs() {
        let direct = Key::new(12_582_912, EXTENT_DATA_REF_KEY, 0xabcd);
        assert_eq!(
            parse_keyed_backref(direct, &encode_data_ref(&DIRECT)),
            Ok(DIRECT)
        );
        let shared = Key::new(12_582_912, SHARED_DATA_REF_KEY, 30_000_000);
        assert_eq!(parse_keyed_backref(shared, &1u32.to_le_bytes()), Ok(SHARED));
        assert!(parse_keyed_backref(shared, &[0; 5]).is_err());
    }

    #[test]
    fn parses_the_inline_backrefs_of_a_tree_block() {
        let backrefs = [BlockBackref::Root(257), BlockBackref::Parent(40 << 20)];
        let skinny = Key::new(30 << 20, METADATA_ITEM_KEY, 0);
        let item = encode_tree_block(2, false, &backrefs);
        assert_eq!(parse_tree_block_item(skinny, &item), Ok(backrefs.to_vec()));
        let fat = Key::new(30 << 20, EXTENT_ITEM_KEY, 16384);
        let item = encode_tree_block(2, true, &backrefs);
        assert_eq!(parse_tree_block_item(fat, &item), Ok(backrefs.to_vec()));
    }

    #[test]
    fn a_data_extent_or_unknown_inline_type_is_not_a_tree_block() {
        let key = Key::new(30 << 20, EXTENT_ITEM_KEY, 16384);
        assert_eq!(
            parse_tree_block_item(key, &encode_data_extent(1, &[DIRECT])),
            Err(BackrefError::NotATreeBlock)
        );
        let mut item = encode_tree_block(1, false, &[]);
        item.push(EXTENT_DATA_REF_KEY);
        item.extend(5u64.to_le_bytes());
        let skinny = Key::new(30 << 20, METADATA_ITEM_KEY, 0);
        assert_eq!(
            parse_tree_block_item(skinny, &item),
            Err(BackrefError::UnexpectedInline(EXTENT_DATA_REF_KEY))
        );
    }

    #[test]
    fn keyed_tree_block_backrefs_are_named_by_their_key() {
        let root = Key::new(30 << 20, TREE_BLOCK_REF_KEY, 257);
        assert_eq!(
            parse_keyed_block_backref(root, &[]),
            Ok(BlockBackref::Root(257))
        );
        let parent = Key::new(30 << 20, SHARED_BLOCK_REF_KEY, 40 << 20);
        assert_eq!(
            parse_keyed_block_backref(parent, &[]),
            Ok(BlockBackref::Parent(40 << 20))
        );
        assert!(parse_keyed_block_backref(root, &[0]).is_err());
        let data = Key::new(30 << 20, EXTENT_DATA_REF_KEY, 1);
        assert!(parse_keyed_block_backref(data, &[]).is_err());
    }

    #[test]
    fn counts_references_of_each_backref() {
        assert_eq!(DIRECT.count(), 2);
        assert_eq!(SHARED.count(), 1);
    }
}
