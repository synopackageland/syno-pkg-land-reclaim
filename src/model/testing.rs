//! Hand-built subvolume tree items for model tests.

use crate::btrfs::keys::*;
use crate::btrfs::parser::testing::{
    encode_dir, encode_disk_extent, encode_extent_header, encode_inode, encode_inode_extrefs,
    encode_root_item, encode_root_ref,
};
use crate::btrfs::parser::{BtrfsTimespec, DiskExtent, InodeItem, ROOT_SUBVOL_RDONLY, RootItem};
use crate::btrfs::subvolume::{Link, Subvolume};
use crate::btrfs::walk::WalkError;
use crate::btrfs::walk::testing::{FakeSource, NoStop};
use crate::model::namespace::Namespace;

pub const DIR: u32 = 0o040755;
pub const FILE: u32 = 0o100644;
pub const LINK: u32 = 0o120777;
pub const REGULAR: u8 = 1;
pub const PREALLOC: u8 = 2;

pub type TreeItem = (Key, Vec<u8>);

pub fn inode(ino: u64, mode: u32, size: u64, nlink: u32) -> TreeItem {
    inode_with_ctime(ino, mode, size, nlink, BtrfsTimespec::default())
}

/// An inode item last written in transaction `transid`.
pub fn inode_written(ino: u64, mode: u32, transid: u64) -> TreeItem {
    let (key, mut data) = inode(ino, mode, 0, 1);
    data[8..16].copy_from_slice(&transid.to_le_bytes());
    (key, data)
}

/// An `INODE_REF` linking `ino` into directory `parent`.
pub fn inode_ref(ino: u64, parent: u64, name: &str) -> TreeItem {
    let mut data = 2u64.to_le_bytes().to_vec();
    data.extend((name.len() as u16).to_le_bytes());
    data.extend(name.as_bytes());
    (Key::new(ino, INODE_REF_KEY, parent), data)
}

/// An `INODE_EXTREF` linking `ino` into each directory of `parents`.
pub fn inode_extref(ino: u64, hash: u64, parents: &[u64]) -> TreeItem {
    let links: Vec<(u64, &[u8])> = parents.iter().map(|&p| (p, b"link".as_slice())).collect();
    (
        Key::new(ino, INODE_EXTREF_KEY, hash),
        encode_inode_extrefs(&links),
    )
}

pub fn inode_with_ctime(
    ino: u64,
    mode: u32,
    size: u64,
    nlink: u32,
    ctime: BtrfsTimespec,
) -> TreeItem {
    let item = InodeItem {
        generation: 1,
        transid: 1,
        size,
        nbytes: size,
        nlink,
        uid: 0,
        gid: 0,
        mode,
        rdev: 0,
        flags: 0,
        ctime,
        mtime: BtrfsTimespec::default(),
    };
    (Key::new(ino, INODE_ITEM_KEY, 0), encode_inode(&item))
}

pub fn entry(parent: u64, index: u64, name: &str, child: u64) -> TreeItem {
    let location = Key::new(child, INODE_ITEM_KEY, 0);
    (
        Key::new(parent, DIR_INDEX_KEY, index),
        encode_dir(location, 1, name.as_bytes(), &[]),
    )
}

/// A Synology caseless index item: a `DIR_ITEM` payload under type 91.
pub fn caseless(parent: u64, hash: u64, name: &str, child: u64) -> TreeItem {
    let location = Key::new(child, INODE_ITEM_KEY, 0);
    (
        Key::new(parent, SYNO_DIR_ITEM_CASELESS_KEY, hash),
        encode_dir(location, 1, name.as_bytes(), &[]),
    )
}

pub fn subvolume_entry(parent: u64, index: u64, name: &str, subvol: u64) -> TreeItem {
    let location = Key::new(subvol, ROOT_ITEM_KEY, u64::MAX);
    (
        Key::new(parent, DIR_INDEX_KEY, index),
        encode_dir(location, 2, name.as_bytes(), &[]),
    )
}

pub fn extent_item(ino: u64, file_offset: u64, extent_type: u8, disk: DiskExtent) -> TreeItem {
    let mut data = encode_extent_header(extent_type);
    data.extend(encode_disk_extent(&disk));
    (Key::new(ino, EXTENT_DATA_KEY, file_offset), data)
}

/// `item`, an `EXTENT_DATA` item, with compression turned off.
pub fn uncompressed(mut item: TreeItem) -> TreeItem {
    item.1[16] = 0; // after generation and ram_bytes
    item
}

/// A regular, uncompressed extent referencing all of `[bytenr, +len)`.
pub fn whole_extent(ino: u64, file_offset: u64, bytenr: u64, len: u64) -> TreeItem {
    let disk = DiskExtent {
        disk_bytenr: bytenr,
        disk_num_bytes: len,
        offset: 0,
        num_bytes: len,
    };
    uncompressed(extent_item(ino, file_offset, REGULAR, disk))
}

pub fn read_tree(tree_id: u64, items: Vec<TreeItem>) -> Result<Namespace, WalkError> {
    Namespace::read(&mut FakeSource::new(items, 3), tree_id, &mut NoStop)
}

/// Subvolume `id` linked into directory `dirid` of tree `parent_id`.
pub fn linked(id: u64, parent_id: u64, dirid: u64, name: &str) -> Subvolume {
    Subvolume {
        id,
        parent: Some(Link { parent_id, dirid }),
        name: name.as_bytes().to_vec(),
        generation: 1,
        readonly: false,
        pending_deletion: false,
        uuid: None,
        parent_uuid: None,
        last_snapshot: 0,
        otime: None,
    }
}

/// A root-tree `ROOT_ITEM` of subvolume `id`.
pub fn root_item(id: u64, generation: u64, readonly: bool) -> TreeItem {
    root_item_with_refs(id, generation, readonly, 1)
}

/// A root-tree `ROOT_ITEM` of subvolume `id` deleted and awaiting the
/// Btrfs cleaner (`refs == 0`).
pub fn pending_root_item(id: u64, generation: u64) -> TreeItem {
    root_item_with_refs(id, generation, false, 0)
}

fn root_item_with_refs(id: u64, generation: u64, readonly: bool, refs: u32) -> TreeItem {
    let item = RootItem {
        generation,
        root_dirid: FIRST_FREE_OBJECTID,
        bytenr: 0,
        last_snapshot: 0,
        flags: if readonly { ROOT_SUBVOL_RDONLY } else { 0 },
        refs,
        uuid: Some([id as u8; 16]),
        parent_uuid: None,
        otime: None,
    };
    (
        Key::new(id, ROOT_ITEM_KEY, 0),
        encode_root_item(&item, true),
    )
}

/// A distinct, non-nil uuid for subvolume `id`.
pub fn uuid_of(id: u64) -> [u8; 16] {
    let mut uuid = [0xab; 16];
    uuid[..8].copy_from_slice(&id.to_be_bytes());
    uuid
}

/// A writable subvolume with a uuid, last snapshotted at `last_snapshot`.
pub fn snapshotted_root_item(id: u64, generation: u64, last_snapshot: u64) -> TreeItem {
    encode_root(
        id,
        RootItem {
            generation,
            root_dirid: FIRST_FREE_OBJECTID,
            bytenr: 0,
            last_snapshot,
            flags: 0,
            refs: 1,
            uuid: Some(uuid_of(id)),
            parent_uuid: None,
            otime: None,
        },
    )
}

/// A read-only snapshot `id` of subvolume `source`.
pub fn snapshot_root_item(id: u64, generation: u64, source: u64) -> TreeItem {
    encode_root(
        id,
        RootItem {
            generation,
            root_dirid: FIRST_FREE_OBJECTID,
            bytenr: 0,
            last_snapshot: generation,
            flags: ROOT_SUBVOL_RDONLY,
            refs: 1,
            uuid: Some(uuid_of(id)),
            parent_uuid: Some(uuid_of(source)),
            otime: None,
        },
    )
}

fn encode_root(id: u64, item: RootItem) -> TreeItem {
    (
        Key::new(id, ROOT_ITEM_KEY, 0),
        encode_root_item(&item, true),
    )
}

/// A root-tree `ROOT_BACKREF`: `id` is linked as `name` in `dirid` of `parent`.
pub fn root_backref(id: u64, parent: u64, dirid: u64, name: &str) -> TreeItem {
    (
        Key::new(id, ROOT_BACKREF_KEY, parent),
        encode_root_ref(dirid, 1, name.as_bytes()),
    )
}
