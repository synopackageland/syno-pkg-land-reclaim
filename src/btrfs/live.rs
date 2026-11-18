//! The live `MetadataSource`: TREE_SEARCH_V2 on an open Btrfs file.

use crate::btrfs::chunk::ChunkMap;
use crate::btrfs::device::{DeviceReader, Identity};
use crate::btrfs::leaf_ranges::BlockReader;
use crate::btrfs::search::SearchRange;
use crate::btrfs::source::{
    BlockGroupSpace, FsSpace, Maintenance, MetadataSource, Probe, RawBatch, ReaderResult,
    SourceError, SpaceSnapshot,
};
use crate::btrfs::walk::BatchBoundary;
use crate::sys::{self, SearchBuffer, SearchOutcome};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// The kernel clamps TREE_SEARCH_V2 buffers to 16 MiB.
pub const MAX_SEARCH_BUFFER: usize = 16 * 1024 * 1024;

/// An opened scan target and its inode number within its subvolume.
pub struct OpenTarget {
    pub file: File,
    pub ino: u64,
}

impl OpenTarget {
    /// Opens `path` read-only without blocking on FIFOs.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let ino = file.metadata()?.ino();
        Ok(OpenTarget { file, ino })
    }

    /// The subvolume (tree) containing the target.
    pub fn subvolume_id(&self) -> io::Result<u64> {
        sys::subvolume_id(self.file.as_fd())
    }
}

/// Issues TREE_SEARCH_V2 with one reusable buffer, growing it only when a
/// single item does not fit, up to the kernel cap.
pub struct LiveSource {
    file: File,
    buf: SearchBuffer,
}

impl LiveSource {
    pub fn new(file: File, initial_capacity: usize) -> Self {
        LiveSource {
            file,
            buf: SearchBuffer::new(initial_capacity),
        }
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    /// The device of a single-device filesystem, read without the page
    /// cache once its superblock proves it is this filesystem's.
    fn open_device(&self, chunks: ChunkMap) -> Result<Box<dyn BlockReader>, String> {
        let fd = self.file.as_fd();
        let info = sys::fs_info(fd).map_err(|e| format!("filesystem info: {e}"))?;
        if info.num_devices != 1 {
            return Err(format!("the filesystem has {} devices", info.num_devices));
        }
        let (devid, path) = (1..=info.max_id)
            .find_map(|devid| {
                sys::device_path(fd, devid)
                    .transpose()
                    .map(|path| path.map(|path| (devid, path)))
            })
            .ok_or("the filesystem names no device")?
            .map_err(|e| format!("device info: {e}"))?;
        let device = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| format!("device {}: {e}", path.display()))?;
        let identity = Identity {
            fsid: info.fsid,
            devid,
            nodesize: info.nodesize,
        };
        let reader = DeviceReader::new(device, identity, chunks)
            .map_err(|e| format!("device {}: {e}", path.display()))?;
        Ok(Box::new(reader))
    }
}

impl MetadataSource for LiveSource {
    fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
        loop {
            match sys::tree_search_v2(self.file.as_fd(), range, &mut self.buf)? {
                SearchOutcome::Items(nr_items) => {
                    return Ok(RawBatch {
                        nr_items,
                        buf: self.buf.results(),
                    });
                }
                SearchOutcome::BufferTooSmall { needed } => {
                    let capacity = grown_capacity(self.buf.capacity(), needed)?;
                    self.buf = SearchBuffer::new(capacity);
                }
            }
        }
    }

    fn maintenance_probes(&mut self) -> Vec<Probe> {
        let fd = self.file.as_fd();
        vec![
            (Maintenance::Balance, sys::balance_running(fd)),
            (Maintenance::DevReplace, sys::dev_replace_running(fd)),
        ]
    }

    fn block_reader(&mut self, boundary: &mut dyn BatchBoundary) -> ReaderResult {
        let chunks = ChunkMap::load(self, boundary)?;
        Ok(self.open_device(chunks))
    }

    fn space_snapshot(&mut self) -> Option<SpaceSnapshot> {
        let fd = self.file.as_fd();
        let statfs = sys::filesystem_space_statfs(fd)
            .map(|space| FsSpace {
                total_bytes: space.total,
                free_bytes: space.free,
                available_bytes: space.available,
            })
            .map_err(|e| e.to_string());
        let block_groups = sys::btrfs_space_info(fd)
            .map(|rows| {
                rows.into_iter()
                    .map(|row| BlockGroupSpace {
                        flags: row.flags,
                        total_bytes: row.total_bytes,
                        used_bytes: row.used_bytes,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string());
        Some(SpaceSnapshot {
            statfs,
            block_groups,
        })
    }
}

/// The result-area size to retry with after the kernel reported `needed`.
fn grown_capacity(current: usize, needed: u64) -> Result<usize, SourceError> {
    let cap = MAX_SEARCH_BUFFER as u64;
    if needed > cap {
        return Err(SourceError::BufferCap { needed, cap });
    }
    let needed = needed as usize;
    if needed <= current {
        return Err(SourceError::NoProgress {
            needed: needed as u64,
            current,
        });
    }
    Ok(needed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_to_exactly_the_needed_size() {
        assert_eq!(grown_capacity(4096, 70_000).unwrap(), 70_000);
    }

    #[test]
    fn refuses_growth_beyond_the_kernel_cap() {
        let needed = MAX_SEARCH_BUFFER as u64 + 1;
        assert!(matches!(
            grown_capacity(4096, needed),
            Err(SourceError::BufferCap { needed: n, cap }) if n == needed && cap == MAX_SEARCH_BUFFER as u64
        ));
    }

    #[test]
    fn refuses_a_needed_size_that_does_not_grow_the_buffer() {
        assert!(matches!(
            grown_capacity(4096, 4096),
            Err(SourceError::NoProgress { .. })
        ));
    }
}
