//! Files a scan spills its units to (SPEC 19 Phase 3). They are read
//! and appended in bounded chunks and reopened for each chunk, so
//! neither memory nor open descriptors grow with the number of units.

use crate::codec::Decoder;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Bytes read or buffered per file at a time.
pub const CHUNK: usize = 64 << 10;
/// Largest record a chunk reader decodes.
pub const MAX_RECORD: usize = 16 << 10;

/// A private directory of one run's files, removed when dropped.
#[derive(Debug)]
pub struct Spill {
    dir: PathBuf,
}

impl Spill {
    /// Creates `dir` (0700), which must not exist yet.
    pub fn create(dir: PathBuf) -> io::Result<Spill> {
        if let Some(parent) = dir.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(Spill { dir })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Removes the run directories under `root` that scans of `fsid` left
/// when they died; the caller holds that filesystem's writer lock, so no
/// live scan owns them. Returns how many it removed.
pub fn clear_leftovers(root: &Path, fsid: &str) -> io::Result<usize> {
    let entries = match fs::read_dir(root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        other => other?,
    };
    let suffix = format!("-{fsid}");
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .ends_with(suffix.as_bytes())
        {
            fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Bytes of the files below `dir`; what cannot be read counts as none.
pub fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&entry.path()),
            Ok(t) if t.is_file() => entry.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

fn corrupt(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: truncated or corrupt record", path.display()),
    )
}

/// Decodes records from a file one chunk at a time.
#[derive(Debug)]
pub struct ChunkReader {
    path: PathBuf,
    /// File offset of the end of `buf`.
    offset: u64,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    max_record: usize,
}

impl ChunkReader {
    pub fn new(path: PathBuf) -> Self {
        ChunkReader {
            path,
            offset: 0,
            buf: Vec::new(),
            pos: 0,
            eof: false,
            max_record: MAX_RECORD,
        }
    }

    /// A larger existing-codec record, still bounded by one chunk. Used by
    /// ext4's temporary full paths, not by the final component-name report.
    pub fn with_max_record(path: PathBuf, max_record: usize) -> Self {
        let mut reader = Self::new(path);
        reader.max_record = max_record.clamp(MAX_RECORD, CHUNK);
        reader
    }

    /// The next record, or `None` at the end of the file. Records must
    /// be at most `MAX_RECORD` bytes.
    pub fn next<T>(
        &mut self,
        decode: impl FnOnce(&mut Decoder<'_>) -> Option<T>,
    ) -> io::Result<Option<T>> {
        self.fill()?;
        if self.pos == self.buf.len() {
            return Ok(None);
        }
        let mut d = Decoder::new(&self.buf, self.pos);
        let record = decode(&mut d).ok_or_else(|| corrupt(&self.path))?;
        self.pos = d.at();
        Ok(Some(record))
    }

    fn fill(&mut self) -> io::Result<()> {
        if self.eof || self.buf.len() - self.pos >= self.max_record {
            return Ok(());
        }
        self.buf.drain(..self.pos);
        self.pos = 0;
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let start = self.buf.len();
        self.buf.resize(CHUNK, 0);
        let mut got = 0;
        while start + got < CHUNK {
            match file.read(&mut self.buf[start + got..])? {
                0 => {
                    self.eof = true;
                    break;
                }
                n => got += n,
            }
        }
        self.buf.truncate(start + got);
        self.offset += got as u64;
        Ok(())
    }
}

/// Appends records to a file through a buffer of about one chunk.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Appender {
    path: PathBuf,
    buf: Vec<u8>,
}

impl Appender {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub fn new(path: PathBuf) -> Self {
        Appender {
            path,
            buf: Vec::new(),
        }
    }

    /// The buffer to encode the next record into; `commit` it after.
    pub fn buf(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }

    /// Writes the buffer out once it holds a chunk.
    pub fn commit(&mut self) -> io::Result<()> {
        if self.buf.len() >= CHUNK {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)?;
        file.write_all(&self.buf)?;
        self.buf.clear();
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh directory under the system temporary directory.
    pub fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("synoreclaim-test-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::testing::scratch;
    use super::*;
    use crate::codec::push_varint;

    #[test]
    fn leftovers_of_one_filesystem_are_cleared_and_others_kept() {
        let root = scratch("spill");
        for name in ["job1-fsa", "job2-fsa", "job1-fsb"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(root.join(name).join("refs"), b"x").unwrap();
        }
        assert_eq!(clear_leftovers(&root, "fsa").unwrap(), 2);
        assert!(root.join("job1-fsb").exists());
        assert!(!root.join("job1-fsa").exists());
        assert_eq!(clear_leftovers(&root.join("none"), "fsa").unwrap(), 0);
    }

    #[test]
    fn a_directory_size_sums_its_files_at_any_depth() {
        let root = scratch("spill");
        fs::write(root.join("a"), [0; 100]).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("b"), [0; 23]).unwrap();
        assert_eq!(dir_bytes(&root), 123);
        assert_eq!(dir_bytes(&root.join("missing")), 0);
    }

    #[test]
    fn records_round_trip_across_chunk_boundaries() {
        let spill = Spill::create(scratch("spill").join("run")).unwrap();
        let mut out = Appender::new(spill.path("values"));
        let values: Vec<u64> = (0..100_000u64).map(|i| i * 7919).collect();
        for &v in &values {
            push_varint(out.buf(), v);
            out.commit().unwrap();
        }
        out.flush().unwrap();
        let mut reader = ChunkReader::new(spill.path("values"));
        let mut read = Vec::new();
        while let Some(v) = reader.next(|d| d.varint()).unwrap() {
            read.push(v);
        }
        assert_eq!(read, values);
    }

    #[test]
    fn a_truncated_record_is_an_error_and_dropping_removes_the_files() {
        let spill = Spill::create(scratch("spill").join("run")).unwrap();
        fs::write(spill.path("bad"), [0x80]).unwrap();
        let mut reader = ChunkReader::new(spill.path("bad"));
        assert!(reader.next(|d| d.varint()).is_err());
        let dir = spill.dir().to_path_buf();
        drop(spill);
        assert!(!dir.exists());
    }
}
