//! Btrfs B-tree keys. Search ranges are lexicographic over
//! `(objectid, type, offset)`, not three independent filters (SPEC 8).

// Item types (`btrfs_tree.h`).
pub const INODE_ITEM_KEY: u8 = 1;
pub const INODE_REF_KEY: u8 = 12;
pub const INODE_EXTREF_KEY: u8 = 13;
pub const XATTR_ITEM_KEY: u8 = 24;
pub const VERITY_DESC_ITEM_KEY: u8 = 36;
pub const VERITY_MERKLE_ITEM_KEY: u8 = 37;
pub const ORPHAN_ITEM_KEY: u8 = 48;
pub const DIR_LOG_ITEM_KEY: u8 = 60;
pub const DIR_LOG_INDEX_KEY: u8 = 72;
pub const DIR_ITEM_KEY: u8 = 84;
pub const DIR_INDEX_KEY: u8 = 96;
pub const EXTENT_DATA_KEY: u8 = 108;
pub const ROOT_ITEM_KEY: u8 = 132;
pub const ROOT_BACKREF_KEY: u8 = 144;
pub const ROOT_REF_KEY: u8 = 156;

/// Item types expected in a subvolume (fs) tree. Anything else, such as
/// Synology-private type 163, is unknown and reported, never fatal.
const FS_TREE_ITEM_TYPES: [u8; 12] = [
    INODE_ITEM_KEY,
    INODE_REF_KEY,
    INODE_EXTREF_KEY,
    XATTR_ITEM_KEY,
    VERITY_DESC_ITEM_KEY,
    VERITY_MERKLE_ITEM_KEY,
    ORPHAN_ITEM_KEY,
    DIR_LOG_ITEM_KEY,
    DIR_LOG_INDEX_KEY,
    DIR_ITEM_KEY,
    DIR_INDEX_KEY,
    EXTENT_DATA_KEY,
];

pub fn is_known_fs_tree_type(item_type: u8) -> bool {
    FS_TREE_ITEM_TYPES.contains(&item_type)
}

/// Synology's caseless directory-name index (`CONFIG_SYNO_BTRFS_CASELESS_STAT`):
/// a secondary copy of `DIR_ITEM` keyed by the upper-cased name hash. It
/// carries no extents and is never a source of names or entry counts.
pub const SYNO_DIR_ITEM_CASELESS_KEY: u8 = 91;

/// A display label for an item type outside the mainline fs-tree set.
pub fn describe_item_type(item_type: u8) -> String {
    match item_type {
        SYNO_DIR_ITEM_CASELESS_KEY => format!("BTRFS_DIR_ITEM_CASELESS_KEY ({item_type})"),
        t => format!("type {t}"),
    }
}

/// `label xCOUNT` for each type, comma-separated.
pub fn describe_type_counts(counts: &std::collections::BTreeMap<u8, u64>) -> String {
    let parts: Vec<String> = counts
        .iter()
        .map(|(t, n)| format!("{} x{n}", describe_item_type(*t)))
        .collect();
    parts.join(", ")
}

// Well-known tree and object IDs.
pub const ROOT_TREE_OBJECTID: u64 = 1;
pub const FS_TREE_OBJECTID: u64 = 5;
pub const QUOTA_TREE_OBJECTID: u64 = 8;
/// First objectid for inodes and subvolumes; also the root directory inode.
pub const FIRST_FREE_OBJECTID: u64 = 256;
/// Inode number of the empty directory standing in for a nested subvolume
/// that a snapshot did not carry over.
pub const EMPTY_SUBVOL_DIR_OBJECTID: u64 = 2;
/// Last objectid usable by subvolumes (`-256` as u64).
pub const LAST_FREE_OBJECTID: u64 = u64::MAX - 255;
/// Root items of relocation trees (`-8`), keyed by the root they copy.
pub const TREE_RELOC_OBJECTID: u64 = u64::MAX - 7;

/// A B-tree key. Field order gives the Btrfs lexicographic ordering.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Key {
    pub objectid: u64,
    pub item_type: u8,
    pub offset: u64,
}

impl Key {
    pub const MIN: Key = Key::new(0, 0, 0);
    pub const MAX: Key = Key::new(u64::MAX, u8::MAX, u64::MAX);

    pub const fn new(objectid: u64, item_type: u8, offset: u64) -> Self {
        Key {
            objectid,
            item_type,
            offset,
        }
    }

    /// The smallest key strictly greater than `self`, if any.
    pub fn successor(self) -> Option<Key> {
        if let Some(offset) = self.offset.checked_add(1) {
            return Some(Key { offset, ..self });
        }
        if let Some(item_type) = self.item_type.checked_add(1) {
            return Some(Key::new(self.objectid, item_type, 0));
        }
        self.objectid
            .checked_add(1)
            .map(|objectid| Key::new(objectid, 0, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_lexicographically() {
        let a = Key::new(256, 1, u64::MAX);
        let b = Key::new(256, 2, 0);
        let c = Key::new(257, 0, 0);
        assert!(a < b && b < c);
    }

    #[test]
    fn successor_increments_offset() {
        assert_eq!(Key::new(5, 1, 7).successor(), Some(Key::new(5, 1, 8)));
    }

    #[test]
    fn successor_carries_offset_into_type() {
        assert_eq!(
            Key::new(5, 1, u64::MAX).successor(),
            Some(Key::new(5, 2, 0))
        );
    }

    #[test]
    fn successor_carries_type_into_objectid() {
        assert_eq!(
            Key::new(5, u8::MAX, u64::MAX).successor(),
            Some(Key::new(6, 0, 0))
        );
    }

    #[test]
    fn names_the_synology_caseless_index_type() {
        assert_eq!(describe_item_type(91), "BTRFS_DIR_ITEM_CASELESS_KEY (91)");
        assert_eq!(describe_item_type(163), "type 163");
    }

    #[test]
    fn largest_key_has_no_successor() {
        assert_eq!(Key::MAX.successor(), None);
    }
}
