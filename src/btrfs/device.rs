//! Tree blocks read from the filesystem's device (SPEC 8.1): the
//! superblock proves the device is the filesystem's, and the chunk tree
//! maps each block's logical address to its place on the device.

use crate::btrfs::bytes::LeReader;
use crate::btrfs::chunk::ChunkMap;
use crate::btrfs::crc32c::crc32c;
use crate::btrfs::leaf_ranges::{BlockReader, Parse, ReadLeaf};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Where the primary superblock lies on every device.
pub const SUPERBLOCK_OFFSET: u64 = 64 << 10;
const SUPERBLOCK_LEN: usize = 4096;
const MAGIC: &[u8; 8] = b"_BHRfS_M";
const CSUM_TYPE_CRC32C: u16 = 0;
/// Reads are aligned for `O_DIRECT` on every logical block size.
const ALIGN: usize = 4096;

/// Why the device cannot serve tree blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("the device has no valid superblock")]
    Superblock,
    #[error("the device belongs to another filesystem")]
    Filesystem,
    #[error("the device has another device id")]
    Device,
    #[error("tree blocks use a checksum other than CRC-32C")]
    ChecksumType,
}

/// Reads at an offset, as `pread` does.
pub trait ReadAt {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
}

impl ReadAt for std::fs::File {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }
}

#[cfg(test)]
impl ReadAt for Vec<u8> {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let bytes = self
            .get(start..start + buf.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

/// What the device must belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub fsid: [u8; 16],
    pub devid: u64,
    pub nodesize: u32,
}

/// A buffer whose usable part starts on an `ALIGN` boundary.
struct Aligned {
    raw: Vec<u8>,
    start: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> Self {
        let raw = vec![0u8; len + ALIGN];
        let start = raw.as_ptr().align_offset(ALIGN);
        Aligned { raw, start, len }
    }

    fn bytes(&mut self) -> &mut [u8] {
        &mut self.raw[self.start..self.start + self.len]
    }
}

/// Tree blocks of a single-device filesystem, read from its device.
pub struct DeviceReader<D> {
    device: D,
    identity: Identity,
    chunks: ChunkMap,
    buf: Aligned,
}

impl<D: ReadAt> DeviceReader<D> {
    /// Accepts `device` once its superblock proves it is `identity`.
    pub fn new(device: D, identity: Identity, chunks: ChunkMap) -> Result<Self, DeviceError> {
        let mut sb = Aligned::new(SUPERBLOCK_LEN);
        device
            .read_exact_at(sb.bytes(), SUPERBLOCK_OFFSET)
            .map_err(|_| DeviceError::Superblock)?;
        check_superblock(sb.bytes(), &identity)?;
        Ok(DeviceReader {
            device,
            identity,
            chunks,
            buf: Aligned::new(identity.nodesize as usize),
        })
    }
}

impl<D: ReadAt + Sync> DeviceReader<D> {
    /// What `parse` makes of the block at each of `bytenrs`, given its
    /// index, read on up to `threads` threads at once.
    pub fn read_parallel<T: Send>(
        &self,
        bytenrs: &[u64],
        threads: usize,
        parse: impl Fn(usize, io::Result<&[u8]>) -> T + Sync,
    ) -> Vec<T> {
        let next = AtomicUsize::new(0);
        let worker = || {
            let mut buf = Aligned::new(self.identity.nodesize as usize);
            let mut done = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(&bytenr) = bytenrs.get(i) else {
                    return done;
                };
                let block = buf.bytes();
                let read = self
                    .offset(bytenr)
                    .and_then(|offset| self.device.read_exact_at(block, offset))
                    .map(|()| &*block);
                done.push((i, parse(i, read)));
            }
        };
        let mut done: Vec<(usize, T)> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads.clamp(1, bytenrs.len().max(1)))
                .map(|_| scope.spawn(worker))
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().expect("a block reader thread panicked"))
                .collect()
        });
        done.sort_unstable_by_key(|&(i, _)| i);
        done.into_iter().map(|(_, leaf)| leaf).collect()
    }

    /// Where the block at `bytenr` lies on the device.
    fn offset(&self, bytenr: u64) -> io::Result<u64> {
        let len = u64::from(self.identity.nodesize);
        self.chunks
            .physical(bytenr, len, self.identity.devid)
            .map_err(io::Error::other)
    }
}

impl<D: ReadAt + Sync> BlockReader for DeviceReader<D> {
    fn fsid(&self) -> [u8; 16] {
        self.identity.fsid
    }

    fn read(&mut self, bytenr: u64) -> io::Result<&[u8]> {
        let offset = self.offset(bytenr)?;
        let block = self.buf.bytes();
        self.device.read_exact_at(block, offset)?;
        Ok(block)
    }

    fn read_each(&mut self, bytenrs: &[u64], threads: usize, parse: &Parse<'_>) -> Vec<ReadLeaf> {
        self.read_parallel(bytenrs, threads, parse)
    }
}

fn check_superblock(sb: &[u8], identity: &Identity) -> Result<(), DeviceError> {
    let mut r = LeReader::new(sb);
    let stored = r.u32().map_err(|_| DeviceError::Superblock)?;
    let field = |at: usize, len: usize| sb.get(at..at + len).ok_or(DeviceError::Superblock);
    let le_u64 = |at| field(at, 8).map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")));
    if field(64, 8)? != MAGIC || stored != crc32c(&sb[32..]) {
        return Err(DeviceError::Superblock);
    }
    if field(32, 16)? != identity.fsid {
        return Err(DeviceError::Filesystem);
    }
    if le_u64(201)? != identity.devid {
        return Err(DeviceError::Device);
    }
    let csum_type = u16::from_le_bytes(field(196, 2)?.try_into().expect("2 bytes"));
    if csum_type != CSUM_TYPE_CRC32C {
        return Err(DeviceError::ChecksumType);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::chunk::testing::chunk_item;
    use crate::btrfs::chunk::{CHUNK_ITEM_KEY, CHUNK_TREE_OBJECTID, FIRST_CHUNK_TREE_OBJECTID};
    use crate::btrfs::keys::Key;
    use crate::btrfs::tree_block::testing::leaf;
    use crate::btrfs::tree_block::{Expected, parse_leaf};
    use crate::btrfs::walk::testing::{FakeTrees, NoStop};

    const MIB: u64 = 1 << 20;
    const NODESIZE: u32 = 4096;
    const FSID: [u8; 16] = [0x77; 16];
    const DEVID: u64 = 1;
    const LEAF: u64 = 30 * MIB + 8192;

    /// A device image with a valid superblock.
    fn disk(fsid: [u8; 16], devid: u64, csum_type: u16) -> Vec<u8> {
        let mut disk = vec![0u8; 8 * MIB as usize];
        let sb = &mut disk[SUPERBLOCK_OFFSET as usize..][..SUPERBLOCK_LEN];
        sb[32..48].copy_from_slice(&fsid);
        sb[48..56].copy_from_slice(&SUPERBLOCK_OFFSET.to_le_bytes());
        sb[64..72].copy_from_slice(b"_BHRfS_M");
        sb[148..152].copy_from_slice(&NODESIZE.to_le_bytes());
        sb[196..198].copy_from_slice(&csum_type.to_le_bytes());
        sb[201..209].copy_from_slice(&devid.to_le_bytes());
        let csum = crate::btrfs::crc32c::crc32c(&sb[32..]);
        sb[..4].copy_from_slice(&csum.to_le_bytes());
        disk
    }

    fn chunks() -> ChunkMap {
        let items = vec![(
            Key::new(FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM_KEY, 30 * MIB),
            chunk_item(MIB, 0, &[(DEVID, 4 * MIB)]),
        )];
        let mut source = FakeTrees::new([(CHUNK_TREE_OBJECTID, items)]);
        ChunkMap::load(&mut source, &mut NoStop).unwrap()
    }

    fn identity() -> Identity {
        Identity {
            fsid: FSID,
            devid: DEVID,
            nodesize: NODESIZE,
        }
    }

    #[test]
    fn reads_a_tree_block_where_the_chunk_tree_places_it() {
        let mut image = disk(FSID, DEVID, 0);
        let expected = Expected {
            bytenr: LEAF,
            generation: 9,
            fsid: FSID,
        };
        let block = leaf(&expected, NODESIZE as usize, &[]);
        let at = (4 * MIB + 8192) as usize;
        image[at..at + block.len()].copy_from_slice(&block);
        let mut reader = DeviceReader::new(image, identity(), chunks()).unwrap();

        let read = reader.read(LEAF).unwrap();

        assert!(parse_leaf(read, &expected).is_ok());
        assert_eq!(reader.fsid(), FSID);
    }

    #[test]
    fn reads_blocks_on_several_threads_as_it_does_one_at_a_time() {
        let mut image = disk(FSID, DEVID, 0);
        let bytenrs: Vec<u64> = (0..6).map(|i| LEAF + i * u64::from(NODESIZE)).collect();
        for (i, &bytenr) in bytenrs.iter().enumerate() {
            let block = leaf(
                &Expected {
                    bytenr,
                    generation: 9,
                    fsid: FSID,
                },
                NODESIZE as usize,
                &[],
            );
            let at = (4 * MIB + 8192) as usize + i * NODESIZE as usize;
            image[at..at + block.len()].copy_from_slice(&block);
        }
        let mut wanted = bytenrs.clone();
        wanted.push(40 * MIB);
        let reader = DeviceReader::new(image, identity(), chunks()).unwrap();
        let first_word = |at: usize, block: io::Result<&[u8]>| {
            block.map(|b| (at, b[48..56].to_vec())).map_err(|_| at)
        };

        let one = reader.read_parallel(&wanted, 1, first_word);
        let many = reader.read_parallel(&wanted, 3, first_word);

        assert_eq!(many, one);
        assert_eq!(many.len(), 7);
        assert_eq!(many[6], Err(6));
        assert_eq!(many[2], Ok((2, bytenrs[2].to_le_bytes().to_vec())));
    }

    #[test]
    fn a_block_outside_the_mapped_chunks_does_not_read() {
        let mut reader = DeviceReader::new(disk(FSID, DEVID, 0), identity(), chunks()).unwrap();

        assert!(reader.read(40 * MIB).is_err());
    }

    #[test]
    fn refuses_a_device_of_another_filesystem_or_device_id() {
        let other_fs = DeviceReader::new(disk([0x11; 16], DEVID, 0), identity(), chunks());
        let other_dev = DeviceReader::new(disk(FSID, 2, 0), identity(), chunks());

        assert_eq!(other_fs.err(), Some(DeviceError::Filesystem));
        assert_eq!(other_dev.err(), Some(DeviceError::Device));
    }

    #[test]
    fn refuses_a_damaged_superblock_or_another_checksum_type() {
        let mut damaged = disk(FSID, DEVID, 0);
        damaged[SUPERBLOCK_OFFSET as usize + 300] ^= 1;
        let mut blank = disk(FSID, DEVID, 0);
        blank[SUPERBLOCK_OFFSET as usize + 64] = 0;

        assert_eq!(
            DeviceReader::new(damaged, identity(), chunks()).err(),
            Some(DeviceError::Superblock)
        );
        assert_eq!(
            DeviceReader::new(blank, identity(), chunks()).err(),
            Some(DeviceError::Superblock)
        );
        assert_eq!(
            DeviceReader::new(disk(FSID, DEVID, 1), identity(), chunks()).err(),
            Some(DeviceError::ChecksumType)
        );
    }
}
