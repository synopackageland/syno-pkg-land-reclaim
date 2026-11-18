//! Btrfs metadata access: keys, TREE_SEARCH_V2 buffers, and item parsers.

/// `statfs.f_type` of a Btrfs filesystem.
pub const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;

/// Superblock feature flags as returned by `BTRFS_IOC_GET_FEATURES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureFlags {
    pub compat: u64,
    pub compat_ro: u64,
    pub incompat: u64,
}

/// Synology `BTRFS_FEATURE_COMPAT_SYNO_CASELESS`: type-91 items exist.
pub const COMPAT_SYNO_CASELESS: u64 = 1 << 63;

pub mod backref;
pub mod bytes;
pub mod chunk;
pub mod crc32c;
pub mod device;
pub mod extent_refs;
pub mod find_new;
pub mod keys;
pub mod leaf_ranges;
#[cfg(target_os = "linux")]
pub mod live;
pub mod owners;
pub mod parser;
pub mod qgroup;
pub mod read_budget;
pub mod recording;
pub mod sanitise;
pub mod search;
pub mod source;
pub mod subvolume;
pub mod tree_block;
pub mod walk;
