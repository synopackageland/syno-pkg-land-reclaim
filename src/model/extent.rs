//! File-to-physical-extent references (SPEC 3 "Physical footprint", 9).
//! A physical extent is identified by its filesystem-logical `disk_bytenr`
//! within one filesystem; accounting is whole-extent granular.

use crate::btrfs::leaf_ranges::FileRange;
use crate::btrfs::parser::{ExtentKind, FileExtentItem};

/// One `EXTENT_DATA` item that references a physical data extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileExtentRef {
    pub inode: u64,
    pub file_offset: u64,
    /// Filesystem-logical allocation identity, not a drive sector.
    pub bytenr: u64,
    /// Allocated (possibly compressed) size of the whole extent.
    pub disk_bytes: u64,
    /// Offset of the referenced range inside the extent's decoded data.
    pub extent_offset: u64,
    /// Logical bytes of the file this item covers.
    pub logical_len: u64,
    pub compression: u8,
    pub prealloc: bool,
    /// The item's leaf may be shared with a snapshot (SPEC 8).
    pub shared_leaf: bool,
}

/// What one `EXTENT_DATA` item contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentRecord {
    Physical(FileExtentRef),
    /// A regular extent with `disk_bytenr == 0`: no allocation.
    Hole {
        logical_len: u64,
    },
    /// Data stored inside the metadata item (SPEC 4.5).
    Inline {
        bytes: u64,
    },
}

pub fn classify(inode: u64, file_offset: u64, item: &FileExtentItem) -> ExtentRecord {
    let (disk, prealloc) = match item.kind {
        ExtentKind::Inline { inline_len } => {
            return ExtentRecord::Inline {
                bytes: inline_len as u64,
            };
        }
        ExtentKind::Regular(disk) => (disk, false),
        ExtentKind::Prealloc(disk) => (disk, true),
    };
    if disk.disk_bytenr == 0 {
        return ExtentRecord::Hole {
            logical_len: disk.num_bytes,
        };
    }
    ExtentRecord::Physical(FileExtentRef {
        inode,
        file_offset,
        bytenr: disk.disk_bytenr,
        disk_bytes: disk.disk_num_bytes,
        extent_offset: disk.offset,
        logical_len: disk.num_bytes,
        compression: item.compression,
        prealloc,
        shared_leaf: false,
    })
}

/// One file extent item of a scope inode (an index into the scope's
/// inode table), ordered by extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExtentUse {
    pub(crate) bytenr: u64,
    pub(crate) inode: u32,
    /// Referenced range `[start, end)` of the extent's decoded data.
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) flags: u8,
    pub(crate) disk_bytes: u64,
}

impl ExtentUse {
    pub(crate) const SHARED_LEAF: u8 = 1;
    pub(crate) const COMPRESSED: u8 = 1 << 1;
    /// The range does not fit the compact fields.
    pub(crate) const UNMAPPED: u8 = 1 << 2;

    pub(crate) fn new(r: &FileExtentRef, inode: u32) -> Self {
        let range = r.extent_offset.checked_add(r.logical_len).and_then(|end| {
            Some((
                u32::try_from(r.extent_offset).ok()?,
                u32::try_from(end).ok()?,
            ))
        });
        let mut flags = 0;
        if r.shared_leaf {
            flags |= Self::SHARED_LEAF;
        }
        if r.compression != 0 {
            flags |= Self::COMPRESSED;
        }
        if range.is_none() {
            flags |= Self::UNMAPPED;
        }
        let (start, end) = range.unwrap_or_default();
        ExtentUse {
            bytenr: r.bytenr,
            inode,
            start,
            end,
            flags,
            disk_bytes: r.disk_bytes,
        }
    }

    /// The range of the extent the use references.
    pub(crate) fn file_range(&self) -> FileRange {
        FileRange {
            start: u64::from(self.start),
            end: u64::from(self.end),
            opaque: self.flags & (Self::COMPRESSED | Self::UNMAPPED) != 0,
        }
    }
}

/// Allocated bytes no valid file range covers. Ranges must be sorted.
pub(crate) fn unreferenced_range_bytes_sorted(
    ranges: impl IntoIterator<Item = (u64, u64)>,
    disk_bytes: u64,
) -> Option<u64> {
    let mut covered = 0;
    let mut reach = 0;
    for (start, end) in ranges {
        if start > end || end > disk_bytes {
            return None;
        }
        if end > reach {
            covered += end - start.max(reach);
            reach = end;
        }
    }
    disk_bytes.checked_sub(covered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::parser::DiskExtent;

    fn item(kind: ExtentKind, compression: u8) -> FileExtentItem {
        FileExtentItem {
            generation: 1,
            ram_bytes: 131072,
            compression,
            encryption: 0,
            other_encoding: 0,
            kind,
        }
    }

    const DISK: DiskExtent = DiskExtent {
        disk_bytenr: 0x10_0000,
        disk_num_bytes: 16384,
        offset: 4096,
        num_bytes: 65536,
    };

    #[test]
    fn regular_extent_keeps_identity_offsets_and_compression() {
        assert_eq!(
            classify(260, 8192, &item(ExtentKind::Regular(DISK), 3)),
            ExtentRecord::Physical(FileExtentRef {
                inode: 260,
                file_offset: 8192,
                bytenr: 0x10_0000,
                disk_bytes: 16384,
                extent_offset: 4096,
                logical_len: 65536,
                compression: 3,
                prealloc: false,
                shared_leaf: false,
            })
        );
    }

    #[test]
    fn prealloc_extent_is_physical_and_flagged() {
        let ExtentRecord::Physical(r) = classify(260, 0, &item(ExtentKind::Prealloc(DISK), 0))
        else {
            panic!("prealloc is allocated space");
        };
        assert!(r.prealloc);
    }

    #[test]
    fn zero_bytenr_is_a_hole() {
        let hole = DiskExtent {
            disk_bytenr: 0,
            ..DISK
        };
        assert_eq!(
            classify(260, 0, &item(ExtentKind::Regular(hole), 0)),
            ExtentRecord::Hole { logical_len: 65536 }
        );
    }

    #[test]
    fn inline_data_is_not_a_physical_extent() {
        assert_eq!(
            classify(260, 0, &item(ExtentKind::Inline { inline_len: 5 }, 0)),
            ExtentRecord::Inline { bytes: 5 }
        );
    }
}
