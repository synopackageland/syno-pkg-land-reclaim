//! Tier-0 duplicate candidates (docs/duplicates.md "Tier 0"): while a
//! scan stores a unit, every regular file of at least [`MIN_SIZE`] gets
//! one fixed record of its size and the physical address of its first
//! data block. Nothing else is read: the record comes from the extent
//! references the scan already spilled. Records travel with the sealed
//! unit, so a reused unit keeps its records, and a published report gets
//! all of them in one sidecar ([`path`]).

use crate::codec::Fnv;
use crate::model::extent::ExtentUse;
use crate::model::namespace::FileKind;
use crate::model::tree::TreeInode;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Smaller files get no record.
pub const MIN_SIZE: u64 = 1 << 20;
/// A unit's records, beside its tree and extents.
pub const UNIT_FILE: &str = "candidates";
/// Bytes of one unit record: objectid, size with flags, first block.
pub const RECORD_LEN: usize = 24;
/// Sizes keep 48 bits (256 TiB); flags take the top 16.
const SIZE_BITS: u32 = 48;
const SIZE_MASK: u64 = (1 << SIZE_BITS) - 1;
const MAGIC: &[u8; 8] = b"SRCAND1\n";
const END: &[u8; 8] = b"SRCEND1\n";

/// The first extent is compressed: its block checksums cover
/// compressed bytes.
pub const COMPRESSED: u16 = 1;
/// The inode has more than one name.
pub const HARD_LINKED: u16 = 1 << 1;

/// One candidate file: an inode of `tree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Candidate {
    pub tree: u64,
    pub objectid: u64,
    pub size: u64,
    pub flags: u16,
    /// Filesystem-logical address of the file's first physical data
    /// block: the first extent's `disk_bytenr + offset`.
    pub first_block: u64,
}

impl Candidate {
    fn encode(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[..8].copy_from_slice(&self.objectid.to_le_bytes());
        let size = (self.size & SIZE_MASK) | (u64::from(self.flags) << SIZE_BITS);
        out[8..16].copy_from_slice(&size.to_le_bytes());
        out[16..].copy_from_slice(&self.first_block.to_le_bytes());
        out
    }

    fn decode(tree: u64, bytes: &[u8]) -> Candidate {
        let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"));
        let size = word(8);
        Candidate {
            tree,
            objectid: word(0),
            size: size & SIZE_MASK,
            flags: (size >> SIZE_BITS) as u16,
            first_block: word(16),
        }
    }
}

/// Writes one unit's records from its extent uses in tree order: each
/// inode's uses come together, its first one first.
pub struct UnitWriter {
    out: BufWriter<File>,
    last: Option<u32>,
    count: u64,
}

impl UnitWriter {
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(UnitWriter {
            out: BufWriter::new(File::create(path)?),
            last: None,
            count: 0,
        })
    }

    /// Takes `extent`, a use of scope inode `inode`; only the first use
    /// of a regular file of at least [`MIN_SIZE`] makes a record.
    pub(crate) fn extent(&mut self, inode: &TreeInode, extent: &ExtentUse) -> io::Result<()> {
        if self.last == Some(extent.inode) {
            return Ok(());
        }
        self.last = Some(extent.inode);
        if inode.kind != FileKind::Regular
            || inode.size < MIN_SIZE
            || inode.size > SIZE_MASK
            || extent.flags & ExtentUse::UNMAPPED != 0
        {
            return Ok(());
        }
        let mut flags = 0;
        if extent.flags & ExtentUse::COMPRESSED != 0 {
            flags |= COMPRESSED;
        }
        if inode.nlink > 1 {
            flags |= HARD_LINKED;
        }
        let record = Candidate {
            tree: inode.tree,
            objectid: inode.objectid,
            size: inode.size,
            flags,
            first_block: extent.bytenr.wrapping_add(u64::from(extent.start)),
        };
        self.out.write_all(&record.encode())?;
        self.count += 1;
        Ok(())
    }

    /// Flushes the file; returns how many records it holds.
    pub fn finish(mut self) -> io::Result<u64> {
        self.out.flush()?;
        Ok(self.count)
    }
}

/// The records of a unit of `tree`.
pub fn read_unit(path: &Path, tree: u64) -> io::Result<Vec<Candidate>> {
    let bytes = fs::read(path)?;
    if bytes.len() % RECORD_LEN != 0 {
        return Err(corrupt());
    }
    Ok(bytes
        .chunks_exact(RECORD_LEN)
        .map(|r| Candidate::decode(tree, r))
        .collect())
}

/// The candidates sidecar of the report at `report`.
pub fn path(report: &Path) -> PathBuf {
    report.with_extension("srcand")
}

/// Writes the sidecar of `report` from the unit files `units`, each
/// with its tree, as a private file replaced atomically. Layout: magic,
/// then per unit its tree, record count and records, then a unit count
/// of `u64::MAX`, the total record count, the end magic and an FNV-1a 64
/// checksum of everything before it. Returns the record count.
pub fn publish(report: &Path, units: &[(u64, PathBuf)]) -> io::Result<u64> {
    let target = path(report);
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(corrupt)?;
    let tmp = target.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut out = Hashed::new(BufWriter::new(file));
        out.put(MAGIC)?;
        let mut total = 0u64;
        for (tree, unit) in units {
            let bytes = match fs::read(unit) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if bytes.len() % RECORD_LEN != 0 {
                return Err(corrupt());
            }
            let count = (bytes.len() / RECORD_LEN) as u64;
            out.put(&tree.to_le_bytes())?;
            out.put(&count.to_le_bytes())?;
            out.put(&bytes)?;
            total += count;
        }
        out.put(&u64::MAX.to_le_bytes())?;
        out.put(&total.to_le_bytes())?;
        out.put(END)?;
        let sum = out.hash.value();
        out.inner.write_all(&sum.to_le_bytes())?;
        let file = out.inner.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(total)
    })();
    match written {
        Ok(total) => {
            fs::rename(&tmp, &target)?;
            Ok(total)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Every record of `report`'s sidecar, checked whole. At most `limit`
/// records are kept; `Ok((records, false))` says some were left out.
pub fn read(report: &Path, limit: usize) -> io::Result<(Vec<Candidate>, bool)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path(report))?;
    let mut input = BufReader::new(file);
    let mut hash = Fnv::new();
    let mut take = |len: usize, hash: &mut Fnv| -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        input.read_exact(&mut buf).map_err(|_| corrupt())?;
        hash.update(&buf);
        Ok(buf)
    };
    if take(MAGIC.len(), &mut hash)? != MAGIC {
        return Err(corrupt());
    }
    let mut out = Vec::new();
    let mut complete = true;
    let mut total = 0u64;
    loop {
        let word = |b: Vec<u8>| u64::from_le_bytes(b.try_into().expect("8 bytes"));
        let tree = word(take(8, &mut hash)?);
        let count = word(take(8, &mut hash)?);
        if tree == u64::MAX {
            if count != total || take(END.len(), &mut hash)? != END {
                return Err(corrupt());
            }
            let stored = word(take(8, &mut Fnv::new())?);
            if stored != hash.value() {
                return Err(corrupt());
            }
            return Ok((out, complete));
        }
        for _ in 0..count {
            let record = take(RECORD_LEN, &mut hash)?;
            if out.len() < limit {
                out.push(Candidate::decode(tree, &record));
            } else {
                complete = false;
            }
        }
        total += count;
    }
}

struct Hashed<W> {
    inner: W,
    hash: Fnv,
}

impl<W: Write> Hashed<W> {
    fn new(inner: W) -> Self {
        Hashed {
            inner,
            hash: Fnv::new(),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.hash.update(bytes);
        self.inner.write_all(bytes)
    }
}

fn corrupt() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid duplicate candidates")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::testing::scratch;

    fn inode(objectid: u64, size: u64, kind: FileKind, nlink: u32) -> TreeInode {
        TreeInode {
            tree: 256,
            objectid,
            size,
            nlink,
            kind,
            inline_bytes: 0,
        }
    }

    fn uses(inode: u32, bytenr: u64, start: u32, flags: u8) -> ExtentUse {
        ExtentUse {
            bytenr,
            inode,
            start,
            end: start + 4096,
            flags,
            disk_bytes: 1 << 20,
        }
    }

    #[test]
    fn records_the_first_block_of_large_regular_files_only() {
        let dir = scratch("cand-unit");
        let file = dir.join(UNIT_FILE);
        let mut w = UnitWriter::create(&file).unwrap();
        let big = inode(257, 3 << 20, FileKind::Regular, 1);
        let small = inode(258, MIN_SIZE - 1, FileKind::Regular, 1);
        let linked = inode(259, MIN_SIZE, FileKind::Regular, 2);
        let link = inode(260, 8 << 20, FileKind::Symlink, 1);
        // Two uses of the big file: only its first counts.
        w.extent(&big, &uses(0, 1 << 30, 8192, 0)).unwrap();
        w.extent(&big, &uses(0, 2 << 30, 0, 0)).unwrap();
        w.extent(&small, &uses(1, 3 << 30, 0, 0)).unwrap();
        w.extent(&linked, &uses(2, 4 << 30, 0, ExtentUse::COMPRESSED))
            .unwrap();
        w.extent(&link, &uses(3, 5 << 30, 0, 0)).unwrap();
        assert_eq!(w.finish().unwrap(), 2);
        assert_eq!(fs::metadata(&file).unwrap().len(), 2 * RECORD_LEN as u64);
        assert_eq!(
            read_unit(&file, 256).unwrap(),
            vec![
                Candidate {
                    tree: 256,
                    objectid: 257,
                    size: 3 << 20,
                    flags: 0,
                    first_block: (1 << 30) + 8192,
                },
                Candidate {
                    tree: 256,
                    objectid: 259,
                    size: MIN_SIZE,
                    flags: COMPRESSED | HARD_LINKED,
                    first_block: 4 << 30,
                },
            ]
        );
    }

    #[test]
    fn the_sidecar_joins_units_with_their_trees_and_checks_itself() {
        let dir = scratch("cand-side");
        let report = dir.join("r.srrep");
        let unit = |tree: u64, objectids: &[u64]| {
            let file = dir.join(format!("u{tree}"));
            let mut w = UnitWriter::create(&file).unwrap();
            for (i, &o) in objectids.iter().enumerate() {
                let mut node = inode(o, 2 << 20, FileKind::Regular, 1);
                node.tree = tree;
                w.extent(&node, &uses(i as u32, o << 20, 0, 0)).unwrap();
            }
            w.finish().unwrap();
            (tree, file)
        };
        let units = vec![
            unit(5, &[257, 258]),
            unit(256, &[300]),
            (257, dir.join("missing")),
        ];
        assert_eq!(publish(&report, &units).unwrap(), 3);
        let (all, complete) = read(&report, usize::MAX).unwrap();
        assert!(complete);
        let ids: Vec<(u64, u64)> = all.iter().map(|c| (c.tree, c.objectid)).collect();
        assert_eq!(ids, vec![(5, 257), (5, 258), (256, 300)]);
        let (some, complete) = read(&report, 2).unwrap();
        assert_eq!((some.len(), complete), (2, false));
        let mut bytes = fs::read(path(&report)).unwrap();
        bytes[12] ^= 1;
        fs::write(path(&report), bytes).unwrap();
        assert!(read(&report, usize::MAX).is_err());
    }
}
