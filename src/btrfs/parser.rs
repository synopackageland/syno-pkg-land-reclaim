//! Parsers for the item payloads Phase 0 needs: `INODE_ITEM`, `DIR_INDEX`,
//! and `EXTENT_DATA`. Layouts follow the Btrfs on-disk format.

use crate::btrfs::bytes::{LeReader, Truncated};
use crate::btrfs::keys::Key;

/// Size of `struct btrfs_inode_item`.
pub const INODE_ITEM_LEN: usize = 160;
const FILE_EXTENT_INLINE: u8 = 0;
const FILE_EXTENT_REGULAR: u8 = 1;
const FILE_EXTENT_PREALLOC: u8 = 2;

/// Malformed item payloads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ItemError {
    #[error("item is {actual} bytes, expected {expected}")]
    Length { expected: usize, actual: usize },
    #[error("item truncated: needed {} bytes, {} available", .0.needed, .0.available)]
    Truncated(Truncated),
    #[error("{count} unexpected trailing bytes")]
    TrailingBytes { count: usize },
    #[error("directory entry has an empty name")]
    EmptyName,
    #[error("directory index entry carries {len} data bytes")]
    UnexpectedData { len: u16 },
    #[error("unknown file extent type {0}")]
    UnknownExtentType(u8),
}

impl From<Truncated> for ItemError {
    fn from(t: Truncated) -> Self {
        ItemError::Truncated(t)
    }
}

/// The `btrfs_inode_item` fields the scanner uses. `nbytes` is never
/// authoritative for physical accounting (SPEC 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeItem {
    pub generation: u64,
    pub transid: u64,
    pub size: u64,
    pub nbytes: u64,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub rdev: u64,
    pub flags: u64,
    pub ctime: BtrfsTimespec,
    pub mtime: BtrfsTimespec,
}

/// A Btrfs on-disk time: Unix seconds and nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BtrfsTimespec {
    pub sec: u64,
    pub nsec: u32,
}

pub fn parse_inode_item(data: &[u8]) -> Result<InodeItem, ItemError> {
    if data.len() != INODE_ITEM_LEN {
        return Err(ItemError::Length {
            expected: INODE_ITEM_LEN,
            actual: data.len(),
        });
    }
    let mut r = LeReader::new(data);
    let generation = r.u64()?;
    let transid = r.u64()?;
    let size = r.u64()?;
    let nbytes = r.u64()?;
    let _block_group = r.u64()?;
    let nlink = r.u32()?;
    let uid = r.u32()?;
    let gid = r.u32()?;
    let mode = r.u32()?;
    let rdev = r.u64()?;
    let flags = r.u64()?;
    // sequence, four reserved u64s and atime precede ctime.
    r.skip(8 + 4 * 8 + 12)?;
    let ctime = BtrfsTimespec {
        sec: r.u64()?,
        nsec: r.u32()?,
    };
    let mtime = BtrfsTimespec {
        sec: r.u64()?,
        nsec: r.u32()?,
    };
    Ok(InodeItem {
        generation,
        transid,
        size,
        nbytes,
        nlink,
        uid,
        gid,
        mode,
        rdev,
        flags,
        ctime,
        mtime,
    })
}

/// One `DIR_INDEX` entry: a name in its parent directory and the key of the
/// inode (`INODE_ITEM`) or subvolume root (`ROOT_ITEM`) it points to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry<'a> {
    pub location: Key,
    pub transid: u64,
    pub file_type: u8,
    pub name: &'a [u8],
}

pub fn parse_dir_index(data: &[u8]) -> Result<DirEntry<'_>, ItemError> {
    let mut r = LeReader::new(data);
    let location = read_disk_key(&mut r)?;
    let transid = r.u64()?;
    let data_len = r.u16()?;
    let name_len = r.u16()?;
    let file_type = r.u8()?;
    let name = r.take(usize::from(name_len))?;
    r.skip(usize::from(data_len))?;
    expect_end(&r)?;
    if name.is_empty() {
        return Err(ItemError::EmptyName);
    }
    if data_len != 0 {
        return Err(ItemError::UnexpectedData { len: data_len });
    }
    Ok(DirEntry {
        location,
        transid,
        file_type,
        name,
    })
}

/// A `btrfs_file_extent_item`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileExtentItem {
    pub generation: u64,
    pub ram_bytes: u64,
    pub compression: u8,
    pub encryption: u8,
    pub other_encoding: u16,
    pub kind: ExtentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentKind {
    /// Data stored in the metadata item itself; not a data extent.
    Inline {
        inline_len: usize,
    },
    Regular(DiskExtent),
    Prealloc(DiskExtent),
}

/// Reference to a data extent. `disk_bytenr == 0` denotes a hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskExtent {
    pub disk_bytenr: u64,
    pub disk_num_bytes: u64,
    pub offset: u64,
    pub num_bytes: u64,
}

/// Whether `EXTENT_DATA` item `data` holds its file data inline.
pub fn is_inline_extent(data: &[u8]) -> bool {
    // generation, ram_bytes, compression, encryption, other_encoding
    const TYPE_OFFSET: usize = 8 + 8 + 1 + 1 + 2;
    data.get(TYPE_OFFSET) == Some(&FILE_EXTENT_INLINE)
}

pub fn parse_file_extent(data: &[u8]) -> Result<FileExtentItem, ItemError> {
    let mut r = LeReader::new(data);
    let generation = r.u64()?;
    let ram_bytes = r.u64()?;
    let compression = r.u8()?;
    let encryption = r.u8()?;
    let other_encoding = r.u16()?;
    let extent_type = r.u8()?;
    let kind = match extent_type {
        FILE_EXTENT_INLINE => ExtentKind::Inline {
            inline_len: r.remaining(),
        },
        FILE_EXTENT_REGULAR => ExtentKind::Regular(read_disk_extent(&mut r)?),
        FILE_EXTENT_PREALLOC => ExtentKind::Prealloc(read_disk_extent(&mut r)?),
        other => return Err(ItemError::UnknownExtentType(other)),
    };
    Ok(FileExtentItem {
        generation,
        ram_bytes,
        compression,
        encryption,
        other_encoding,
        kind,
    })
}

fn read_disk_extent(r: &mut LeReader<'_>) -> Result<DiskExtent, ItemError> {
    let extent = DiskExtent {
        disk_bytenr: r.u64()?,
        disk_num_bytes: r.u64()?,
        offset: r.u64()?,
        num_bytes: r.u64()?,
    };
    expect_end(r)?;
    Ok(extent)
}

/// `btrfs_root_item` up to and including `level`, as written by old kernels.
pub const LEGACY_ROOT_ITEM_LEN: usize = 239;
/// Full `btrfs_root_item` with UUIDs, transids, and times.
pub const ROOT_ITEM_LEN: usize = 439;
/// `BTRFS_ROOT_SUBVOL_RDONLY`.
pub const ROOT_SUBVOL_RDONLY: u64 = 1;
const UUID_LEN: usize = 16;
/// byte_limit, bytes_used.
const ROOT_ITEM_UNUSED_COUNTERS: usize = 16;

/// The `btrfs_root_item` fields used for subvolume discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootItem {
    pub generation: u64,
    pub root_dirid: u64,
    pub bytenr: u64,
    /// Transaction of the newest snapshot taken of (or copied into) this
    /// root: tree blocks from later transactions are never shared.
    pub last_snapshot: u64,
    pub flags: u64,
    /// Zero for a deleted subvolume awaiting the cleaner.
    pub refs: u32,
    /// Present only when the item carries a current extended section.
    pub uuid: Option<[u8; 16]>,
    pub parent_uuid: Option<[u8; 16]>,
    /// Creation time, Unix seconds; with the extended section only.
    pub otime: Option<u64>,
}

impl RootItem {
    pub fn is_readonly(&self) -> bool {
        self.flags & ROOT_SUBVOL_RDONLY != 0
    }
}

pub fn parse_root_item(data: &[u8]) -> Result<RootItem, ItemError> {
    let mut r = LeReader::new(data);
    r.skip(INODE_ITEM_LEN)?;
    let generation = r.u64()?;
    let root_dirid = r.u64()?;
    let bytenr = r.u64()?;
    r.skip(ROOT_ITEM_UNUSED_COUNTERS)?;
    let last_snapshot = r.u64()?;
    let flags = r.u64()?;
    let refs = r.u32()?;
    r.skip(LEGACY_ROOT_ITEM_LEN - r.position())?;
    let extended = read_root_extended(&mut r, generation)?;
    let (uuid, parent_uuid, otime) = extended.map_or((None, None, None), |e| e);
    Ok(RootItem {
        generation,
        root_dirid,
        bytenr,
        last_snapshot,
        flags,
        refs,
        uuid,
        parent_uuid,
        otime,
    })
}

/// uuid, parent_uuid and otime seconds of the extended section.
type RootExtended = (Option<[u8; 16]>, Option<[u8; 16]>, Option<u64>);

/// received_uuid, ctransid, otransid, stransid, rtransid, ctime.
const ROOT_BEFORE_OTIME: usize = UUID_LEN + 4 * 8 + ROOT_TIMESPEC_LEN;
/// `btrfs_timespec`: u64 seconds, u32 nanoseconds.
const ROOT_TIMESPEC_LEN: usize = 12;

/// The extended section is valid only when `generation_v2 == generation`.
fn read_root_extended(
    r: &mut LeReader<'_>,
    generation: u64,
) -> Result<Option<RootExtended>, ItemError> {
    if r.remaining() < ROOT_ITEM_LEN - LEGACY_ROOT_ITEM_LEN {
        return Ok(None);
    }
    let generation_v2 = r.u64()?;
    let uuid = read_uuid(r)?;
    let parent_uuid = read_uuid(r)?;
    r.skip(ROOT_BEFORE_OTIME)?;
    let otime = r.u64()?;
    Ok((generation_v2 == generation).then_some((uuid, parent_uuid, Some(otime))))
}

/// Reads a UUID; the nil UUID means "none".
fn read_uuid(r: &mut LeReader<'_>) -> Result<Option<[u8; 16]>, Truncated> {
    let mut uuid = [0; UUID_LEN];
    uuid.copy_from_slice(r.take(UUID_LEN)?);
    Ok((uuid != [0; UUID_LEN]).then_some(uuid))
}

/// A `ROOT_BACKREF`/`ROOT_REF` payload: where a subvolume is linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootRef<'a> {
    /// Directory inode in the parent subvolume holding the link.
    pub dirid: u64,
    pub sequence: u64,
    pub name: &'a [u8],
}

/// One packed `INODE_REF` entry: a directory inode's parent and name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeRef<'a> {
    pub index: u64,
    pub name: &'a [u8],
}

/// Reads the packed entries in an `INODE_REF` item.
pub fn parse_inode_refs(data: &[u8]) -> Result<Vec<InodeRef<'_>>, ItemError> {
    let mut r = LeReader::new(data);
    let mut refs = Vec::new();
    while r.remaining() > 0 {
        let index = r.u64()?;
        let name_len = usize::from(r.u16()?);
        let name = r.take(name_len)?;
        if name.is_empty() {
            return Err(ItemError::EmptyName);
        }
        refs.push(InodeRef { index, name });
    }
    Ok(refs)
}

/// Reads the parent directory of each packed entry in an `INODE_EXTREF`
/// item: the hard links an inode's `INODE_REF` items have no room for.
pub fn parse_inode_extref_parents(data: &[u8]) -> Result<Vec<u64>, ItemError> {
    let mut r = LeReader::new(data);
    let mut parents = Vec::new();
    while r.remaining() > 0 {
        let parent = r.u64()?;
        let _index = r.u64()?;
        let name_len = usize::from(r.u16()?);
        if r.take(name_len)?.is_empty() {
            return Err(ItemError::EmptyName);
        }
        parents.push(parent);
    }
    Ok(parents)
}

pub fn parse_root_ref(data: &[u8]) -> Result<RootRef<'_>, ItemError> {
    let mut r = LeReader::new(data);
    let dirid = r.u64()?;
    let sequence = r.u64()?;
    let name_len = r.u16()?;
    let name = r.take(usize::from(name_len))?;
    expect_end(&r)?;
    Ok(RootRef {
        dirid,
        sequence,
        name,
    })
}

/// Reads a packed 17-byte `btrfs_disk_key`.
pub(crate) fn read_disk_key(r: &mut LeReader<'_>) -> Result<Key, Truncated> {
    Ok(Key::new(r.u64()?, r.u8()?, r.u64()?))
}

fn expect_end(r: &LeReader<'_>) -> Result<(), ItemError> {
    match r.remaining() {
        0 => Ok(()),
        count => Err(ItemError::TrailingBytes { count }),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn encode_inode(inode: &InodeItem) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [inode.generation, inode.transid, inode.size, inode.nbytes, 0] {
            b.extend(v.to_le_bytes()); // ... block_group
        }
        for v in [inode.nlink, inode.uid, inode.gid, inode.mode] {
            b.extend(v.to_le_bytes());
        }
        for v in [inode.rdev, inode.flags, 3, 0, 0, 0, 0] {
            b.extend(v.to_le_bytes()); // sequence and reserved fields
        }
        for time in [
            BtrfsTimespec::default(), // atime
            inode.ctime,
            inode.mtime,
            BtrfsTimespec::default(), // otime
        ] {
            b.extend(time.sec.to_le_bytes());
            b.extend(time.nsec.to_le_bytes());
        }
        assert_eq!(b.len(), INODE_ITEM_LEN);
        b
    }

    /// Packed `INODE_EXTREF` entries: `(parent, name)` each.
    pub fn encode_inode_extrefs(links: &[(u64, &[u8])]) -> Vec<u8> {
        let mut b = Vec::new();
        for (index, (parent, name)) in links.iter().enumerate() {
            b.extend(parent.to_le_bytes());
            b.extend((index as u64 + 2).to_le_bytes());
            b.extend((name.len() as u16).to_le_bytes());
            b.extend(*name);
        }
        b
    }

    pub fn encode_dir(location: Key, file_type: u8, name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(location.objectid.to_le_bytes());
        b.push(location.item_type);
        b.extend(location.offset.to_le_bytes());
        b.extend(77u64.to_le_bytes()); // transid
        b.extend((data.len() as u16).to_le_bytes());
        b.extend((name.len() as u16).to_le_bytes());
        b.push(file_type);
        b.extend(name);
        b.extend(data);
        b
    }

    /// A full-size root item; `uuid_valid` controls `generation_v2`.
    pub fn encode_root_item(root: &RootItem, uuid_valid: bool) -> Vec<u8> {
        let mut b = vec![0; INODE_ITEM_LEN];
        for v in [
            root.generation,
            root.root_dirid,
            root.bytenr,
            0,
            0,
            root.last_snapshot,
            root.flags,
        ] {
            b.extend(v.to_le_bytes()); // ... byte_limit bytes_used
        }
        b.extend(root.refs.to_le_bytes());
        b.resize(LEGACY_ROOT_ITEM_LEN, 0); // drop_progress, drop_level, level
        let generation_v2 = if uuid_valid { root.generation } else { 0 };
        b.extend(generation_v2.to_le_bytes());
        b.extend(root.uuid.unwrap_or_default());
        b.extend(root.parent_uuid.unwrap_or_default());
        b.resize(b.len() + ROOT_BEFORE_OTIME, 0); // received_uuid, transids, ctime
        b.extend(root.otime.unwrap_or_default().to_le_bytes());
        b.resize(ROOT_ITEM_LEN, 0); // otime nsec, stime, rtime, reserved
        b
    }

    pub fn encode_root_ref(dirid: u64, sequence: u64, name: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(dirid.to_le_bytes());
        b.extend(sequence.to_le_bytes());
        b.extend((name.len() as u16).to_le_bytes());
        b.extend(name);
        b
    }

    pub fn encode_extent_header(extent_type: u8) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(21u64.to_le_bytes()); // generation
        b.extend(1_000_000u64.to_le_bytes()); // ram_bytes
        b.extend([2, 0]); // compression, encryption
        b.extend(0u16.to_le_bytes()); // other_encoding
        b.push(extent_type);
        b
    }

    pub fn encode_disk_extent(extent: &DiskExtent) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [
            extent.disk_bytenr,
            extent.disk_num_bytes,
            extent.offset,
            extent.num_bytes,
        ] {
            b.extend(v.to_le_bytes());
        }
        b
    }

    pub fn encode_regular_extent(extent: &DiskExtent) -> Vec<u8> {
        let mut b = encode_extent_header(FILE_EXTENT_REGULAR);
        b.extend(encode_disk_extent(extent));
        b
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const SAMPLE_INODE: InodeItem = InodeItem {
        generation: 11,
        transid: 12,
        size: 4096,
        nbytes: 8192,
        nlink: 2,
        uid: 1000,
        gid: 100,
        mode: 0o100644,
        rdev: 0,
        flags: 0x10,
        ctime: BtrfsTimespec {
            sec: 1_790_000_123,
            nsec: 456_789_012,
        },
        mtime: BtrfsTimespec {
            sec: 1_780_000_000,
            nsec: 7,
        },
    };

    fn inode_bytes() -> Vec<u8> {
        encode_inode(&SAMPLE_INODE)
    }

    #[test]
    fn parses_inode_item() {
        assert_eq!(parse_inode_item(&inode_bytes()).unwrap(), SAMPLE_INODE);
    }

    #[test]
    fn reads_ctime_from_the_inode_item_timespec() {
        let inode = parse_inode_item(&inode_bytes()).unwrap();
        assert_eq!(inode.ctime.sec, 1_790_000_123);
        assert_eq!(inode.ctime.nsec, 456_789_012);
    }

    #[test]
    fn inode_item_with_wrong_length_is_rejected() {
        let mut b = inode_bytes();
        b.push(0);
        assert_eq!(
            parse_inode_item(&b),
            Err(ItemError::Length {
                expected: INODE_ITEM_LEN,
                actual: INODE_ITEM_LEN + 1
            })
        );
        assert_eq!(
            parse_inode_item(&b[..100]),
            Err(ItemError::Length {
                expected: INODE_ITEM_LEN,
                actual: 100
            })
        );
    }

    #[test]
    fn parses_dir_index_entry() {
        let b = encode_dir(Key::new(257, 1, 0), 1, "café".as_bytes(), &[]);
        let entry = parse_dir_index(&b).unwrap();
        assert_eq!(
            entry,
            DirEntry {
                location: Key::new(257, 1, 0),
                transid: 77,
                file_type: 1,
                name: "café".as_bytes(),
            }
        );
    }

    #[test]
    fn dir_index_name_must_fit_in_item() {
        let mut b = encode_dir(Key::new(257, 1, 0), 1, b"abc", &[]);
        b.truncate(b.len() - 1);
        assert!(matches!(parse_dir_index(&b), Err(ItemError::Truncated(_))));
    }

    #[test]
    fn dir_index_holds_exactly_one_entry() {
        let mut b = encode_dir(Key::new(257, 1, 0), 1, b"abc", &[]);
        b.push(0);
        assert_eq!(
            parse_dir_index(&b),
            Err(ItemError::TrailingBytes { count: 1 })
        );
    }

    #[test]
    fn dir_index_rejects_empty_name() {
        let b = encode_dir(Key::new(257, 1, 0), 1, b"", &[]);
        assert_eq!(parse_dir_index(&b), Err(ItemError::EmptyName));
    }

    #[test]
    fn dir_index_rejects_attached_data() {
        let b = encode_dir(Key::new(257, 1, 0), 1, b"abc", b"xattr-value");
        assert_eq!(
            parse_dir_index(&b),
            Err(ItemError::UnexpectedData { len: 11 })
        );
    }

    #[test]
    fn parses_packed_inode_refs() {
        let mut data = Vec::new();
        for (index, name) in [(1_u64, b"Snapshot".as_slice()), (2, b"Alias")] {
            data.extend(index.to_le_bytes());
            data.extend((name.len() as u16).to_le_bytes());
            data.extend(name);
        }

        let refs = parse_inode_refs(&data).unwrap();

        assert_eq!(
            refs.iter().map(|r| (r.index, r.name)).collect::<Vec<_>>(),
            vec![(1, b"Snapshot".as_slice()), (2, b"Alias".as_slice())]
        );
    }

    #[test]
    fn parses_the_parents_of_packed_inode_extrefs() {
        let data = testing::encode_inode_extrefs(&[(300, b"a"), (301, b"bb")]);

        assert_eq!(parse_inode_extref_parents(&data).unwrap(), [300, 301]);
    }

    #[test]
    fn an_inode_extref_needs_a_name() {
        let data = testing::encode_inode_extrefs(&[(300, b"")]);

        assert_eq!(parse_inode_extref_parents(&data), Err(ItemError::EmptyName));
        assert!(matches!(
            parse_inode_extref_parents(&data[..10]),
            Err(ItemError::Truncated(_))
        ));
    }

    const SAMPLE_EXTENT: DiskExtent = DiskExtent {
        disk_bytenr: 0x1340_0000,
        disk_num_bytes: 131072,
        offset: 4096,
        num_bytes: 65536,
    };

    #[test]
    fn parses_regular_file_extent() {
        assert_eq!(
            parse_file_extent(&encode_regular_extent(&SAMPLE_EXTENT)).unwrap(),
            FileExtentItem {
                generation: 21,
                ram_bytes: 1_000_000,
                compression: 2,
                encryption: 0,
                other_encoding: 0,
                kind: ExtentKind::Regular(SAMPLE_EXTENT),
            }
        );
    }

    #[test]
    fn parses_prealloc_file_extent() {
        let mut b = encode_extent_header(2);
        b.extend(encode_disk_extent(&SAMPLE_EXTENT));
        assert!(matches!(
            parse_file_extent(&b).unwrap().kind,
            ExtentKind::Prealloc(_)
        ));
    }

    #[test]
    fn parses_inline_file_extent() {
        let mut b = encode_extent_header(0);
        b.extend(b"hello");
        assert_eq!(
            parse_file_extent(&b).unwrap().kind,
            ExtentKind::Inline { inline_len: 5 }
        );
        assert!(is_inline_extent(&b));
        assert!(!is_inline_extent(&encode_extent_header(1)));
        assert!(!is_inline_extent(&b[..10]));
    }

    #[test]
    fn regular_extent_must_have_exact_length() {
        let mut b = encode_extent_header(1);
        b.extend(encode_disk_extent(&SAMPLE_EXTENT));
        b.push(0);
        assert_eq!(
            parse_file_extent(&b),
            Err(ItemError::TrailingBytes { count: 1 })
        );
        assert!(matches!(
            parse_file_extent(&b[..40]),
            Err(ItemError::Truncated(_))
        ));
    }

    const SAMPLE_ROOT: RootItem = RootItem {
        generation: 90,
        root_dirid: 256,
        bytenr: 0x4000,
        last_snapshot: 88,
        flags: ROOT_SUBVOL_RDONLY,
        refs: 1,
        uuid: Some([7; 16]),
        parent_uuid: Some([9; 16]),
        otime: Some(1_780_000_000),
    };

    #[test]
    fn parses_full_root_item() {
        let root = parse_root_item(&encode_root_item(&SAMPLE_ROOT, true)).unwrap();
        assert_eq!(root, SAMPLE_ROOT);
        assert!(root.is_readonly());
    }

    #[test]
    fn parses_the_creation_time_of_the_extended_section() {
        let root = parse_root_item(&encode_root_item(&SAMPLE_ROOT, true)).unwrap();
        assert_eq!(root.otime, Some(1_780_000_000));
        let stale = parse_root_item(&encode_root_item(&SAMPLE_ROOT, false)).unwrap();
        assert_eq!(stale.otime, None);
    }

    #[test]
    fn uuids_are_absent_when_generation_v2_is_stale() {
        let root = parse_root_item(&encode_root_item(&SAMPLE_ROOT, false)).unwrap();
        assert_eq!(root.uuid, None);
        assert_eq!(root.parent_uuid, None);
    }

    #[test]
    fn nil_parent_uuid_means_no_parent() {
        let original = RootItem {
            parent_uuid: None,
            ..SAMPLE_ROOT
        };
        let root = parse_root_item(&encode_root_item(&original, true)).unwrap();
        assert_eq!(root.parent_uuid, None);
    }

    #[test]
    fn legacy_root_item_has_no_uuids() {
        let bytes = encode_root_item(&SAMPLE_ROOT, true);
        let root = parse_root_item(&bytes[..LEGACY_ROOT_ITEM_LEN]).unwrap();
        assert_eq!(root.generation, 90);
        assert_eq!(root.uuid, None);
    }

    #[test]
    fn root_item_shorter_than_legacy_layout_is_rejected() {
        let bytes = encode_root_item(&SAMPLE_ROOT, true);
        assert!(matches!(
            parse_root_item(&bytes[..200]),
            Err(ItemError::Truncated(_))
        ));
    }

    #[test]
    fn parses_root_ref() {
        let bytes = encode_root_ref(256, 4, b"share");
        let r = parse_root_ref(&bytes).unwrap();
        assert_eq!(
            r,
            RootRef {
                dirid: 256,
                sequence: 4,
                name: b"share"
            }
        );
    }

    #[test]
    fn root_ref_length_must_match_name() {
        let mut b = encode_root_ref(256, 4, b"share");
        b.push(0);
        assert_eq!(
            parse_root_ref(&b),
            Err(ItemError::TrailingBytes { count: 1 })
        );
        assert!(matches!(
            parse_root_ref(&b[..20]),
            Err(ItemError::Truncated(_))
        ));
    }

    #[test]
    fn unknown_extent_type_is_rejected() {
        let mut b = encode_extent_header(9);
        b.extend(encode_disk_extent(&SAMPLE_EXTENT));
        assert_eq!(parse_file_extent(&b), Err(ItemError::UnknownExtentType(9)));
    }
}
