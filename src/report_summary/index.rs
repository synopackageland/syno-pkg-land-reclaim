//! Seekable directory ranges and precomputed root/share top answers for display only.
use super::{Binding, atomic_write, open};
use crate::codec;
use crate::model::namespace::FileKind;
use crate::query::{KindFilter, Measure, TopAnswer, TopQuery, TopWalk};
use crate::report::{self, ReportError, ReportReader};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"SRIDX1\n\0";
// Version 7 separates this app from generic package/state paths.
const FORMAT: u32 = 7;
const WIDTH: u64 = 152;
const RUN: usize = 32_768;
const MAX_BLOCK: u64 = 64 << 20;

#[derive(Serialize, Deserialize)]
struct Header {
    format: u32,
    binding: Binding,
    count: u64,
    blob_bytes: u64,
}

/// A fixed record: SHA-256 path key, fourteen u64 fields, FNV checksum.
#[derive(Clone)]
struct Record([u8; WIDTH as usize]);
impl Record {
    #[cfg(test)]
    fn new(path: &[u8], start: u64, depth: u32, offset: u64) -> Self {
        let mut r = Self([0; WIDTH as usize]);
        r.0[..32].copy_from_slice(&key(path));
        r.set(0, start);
        r.set(2, u64::from(depth));
        r.set(3, offset);
        r.set(4, path.len() as u64);
        r
    }
    fn get(&self, n: usize) -> u64 {
        u64::from_le_bytes(self.0[32 + n * 8..40 + n * 8].try_into().expect("8 bytes"))
    }
    fn set(&mut self, n: usize, value: u64) {
        self.0[32 + n * 8..40 + n * 8].copy_from_slice(&value.to_le_bytes());
    }
    fn seal(&mut self) {
        let hash = codec::checksum(&self.0[..144]);
        self.0[144..].copy_from_slice(&hash.to_le_bytes());
    }
    fn valid(&self) -> bool {
        codec::checksum(&self.0[..144])
            == u64::from_le_bytes(self.0[144..].try_into().expect("8 bytes"))
    }
    fn read(input: &mut impl Read) -> io::Result<Option<Self>> {
        let mut r = Self([0; WIDTH as usize]);
        match input.read(&mut r.0[..1])? {
            0 => return Ok(None),
            1 => (),
            _ => unreachable!(),
        }
        input.read_exact(&mut r.0[1..])?;
        if !r.valid() {
            return Err(corrupt());
        }
        Ok(Some(r))
    }
}
fn key(path: &[u8]) -> [u8; 32] {
    let mut hash = crate::sha256::Sha256::new();
    hash.update(path);
    hash.finish()
}
fn corrupt() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid display directory index",
    )
}
pub fn path(report: &Path) -> PathBuf {
    report.with_extension("srindex")
}

/// Unlinked private scratch descriptors disappear even if a build is interrupted.
fn scratch(report: &Path) -> io::Result<File> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = report.parent().ok_or_else(corrupt)?;
    crate::state::private_dir(dir, crate::sys::effective_uid()).map_err(io::Error::other)?;
    let name = dir.join(format!(
        ".index-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&name)?;
    fs::remove_file(name)?;
    Ok(file)
}

/// Binary merge levels bound both sort memory and the number of open run descriptors.
struct Sorter<'a> {
    report: &'a Path,
    buf: Vec<Record>,
    levels: Vec<Option<File>>,
    count: u64,
}
impl<'a> Sorter<'a> {
    fn new(report: &'a Path) -> Self {
        Self {
            report,
            buf: Vec::new(),
            levels: Vec::new(),
            count: 0,
        }
    }
    fn push(&mut self, mut record: Record) -> io::Result<()> {
        record.seal();
        self.buf.push(record);
        self.count += 1;
        if self.buf.len() == RUN {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.buf.sort_unstable_by(|a, b| a.0[..32].cmp(&b.0[..32]));
        let mut out = BufWriter::new(scratch(self.report)?);
        for r in self.buf.drain(..) {
            out.write_all(&r.0)?;
        }
        out.flush()?;
        let mut run = out.into_inner().map_err(|e| e.into_error())?;
        run.rewind()?;
        let mut level = 0;
        while level < self.levels.len() {
            if let Some(old) = self.levels[level].take() {
                run = merge(self.report, old, run)?;
            } else {
                self.levels[level] = Some(run);
                return Ok(());
            }
            level += 1;
        }
        self.levels.push(Some(run));
        Ok(())
    }
    fn finish(mut self) -> io::Result<(File, u64)> {
        self.flush()?;
        let mut result = None;
        for run in self.levels.into_iter().flatten() {
            result = Some(match result {
                Some(old) => merge(self.report, old, run)?,
                None => run,
            });
        }
        Ok((
            match result {
                Some(run) => run,
                None => scratch(self.report)?,
            },
            self.count,
        ))
    }
}
fn merge(report: &Path, a: File, b: File) -> io::Result<File> {
    let mut a = BufReader::new(a);
    let mut b = BufReader::new(b);
    let mut x = Record::read(&mut a)?;
    let mut y = Record::read(&mut b)?;
    let mut out = BufWriter::new(scratch(report)?);
    while x.is_some() || y.is_some() {
        if y.is_none()
            || x.as_ref()
                .is_some_and(|x| x.0[..32] <= y.as_ref().expect("present").0[..32])
        {
            out.write_all(&x.take().expect("present").0)?;
            x = Record::read(&mut a)?;
        } else {
            out.write_all(&y.take().expect("present").0)?;
            y = Record::read(&mut b)?;
        }
    }
    out.flush()?;
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.rewind()?;
    Ok(file)
}

struct Index {
    file: File,
    header: Header,
    records: u64,
    blobs: u64,
}
impl Index {
    fn open(report: &Path) -> io::Result<Self> {
        let mut file = open(&path(report))?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != crate::sys::effective_uid() || meta.mode() & 0o077 != 0
        {
            return Err(corrupt());
        }
        let mut prefix = [0; 12];
        file.read_exact(&mut prefix)?;
        if &prefix[..8] != MAGIC {
            return Err(corrupt());
        }
        let len = u32::from_le_bytes(prefix[8..].try_into().expect("4 bytes")) as usize;
        if len > 4096 {
            return Err(corrupt());
        }
        let mut bytes = vec![0; len];
        file.read_exact(&mut bytes)?;
        let mut hash = [0; 8];
        file.read_exact(&mut hash)?;
        if codec::checksum(&bytes) != u64::from_le_bytes(hash) {
            return Err(corrupt());
        }
        let header: Header = serde_json::from_slice(&bytes)?;
        let records = 20 + len as u64;
        let blobs = records
            .checked_add(header.count.checked_mul(WIDTH).ok_or_else(corrupt)?)
            .ok_or_else(corrupt)?;
        if header.format != FORMAT
            || !header.binding.matches(report)
            || blobs.checked_add(header.blob_bytes) != Some(meta.len())
        {
            return Err(corrupt());
        }
        Ok(Self {
            file,
            header,
            records,
            blobs,
        })
    }
    fn record(&mut self, n: u64) -> io::Result<Record> {
        if n >= self.header.count {
            return Err(corrupt());
        }
        self.file.seek(SeekFrom::Start(self.records + n * WIDTH))?;
        Record::read(&mut self.file)?.ok_or_else(corrupt)
    }
    fn block(&mut self, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        if len > MAX_BLOCK
            || offset
                .checked_add(len)
                .is_none_or(|end| end > self.header.blob_bytes)
        {
            return Err(corrupt());
        }
        self.file.seek(SeekFrom::Start(self.blobs + offset))?;
        let mut bytes = vec![0; len as usize];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn find(&mut self, path: &[u8]) -> io::Result<Option<Record>> {
        let key = key(path);
        let mut lo = 0;
        let mut hi = self.header.count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let record = self.record(mid)?;
            if record.0[..32] < key[..] {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        while lo < self.header.count {
            let record = self.record(lo)?;
            if record.0[..32] != key {
                break;
            }
            let stored = self.block(record.get(3), record.get(4))?;
            if super::index::key(&stored) != key {
                return Err(corrupt());
            }
            if stored == path {
                return Ok(Some(record));
            }
            lo += 1;
        }
        Ok(None)
    }
    fn ancestor(&mut self, path: &[u8]) -> io::Result<Option<(Record, Vec<u8>)>> {
        let mut candidate = path;
        loop {
            if let Some(record) = self.find(candidate)? {
                return Ok(Some((record, candidate.to_vec())));
            }
            let Some(slash) = candidate.iter().rposition(|b| *b == b'/') else {
                return Ok(None);
            };
            if candidate.len() <= 1 {
                return Ok(None);
            }
            candidate = &candidate[..slash.max(1)];
        }
    }
}

pub fn ensure(report: &Path) -> Result<(), ReportError> {
    if Index::open(report).is_ok() {
        return Ok(());
    }
    build(report)
}

fn build(report: &Path) -> Result<(), ReportError> {
    let mut file = open(report)?;
    let binding = Binding::of(&mut file)?;
    file.rewind()?;
    let reader = ReportReader::open(&mut file)?;
    let mount = reader.header().mount.clone();
    let mut ranges = Ranges::build(report, reader, binding.size - 16)?;
    ranges.precompute(report, &mount)?;
    if Binding::of(&mut file)? != binding || !binding.matches(report) {
        return Err(corrupt().into());
    }
    ranges.publish(report, binding)
}

// A small directory is omitted only when its retained ancestor is at most 1 MiB.
const MIN_RANGE: u64 = 64 << 10;
const MAX_FALLBACK: u64 = 1 << 20;
const PENDING_BYTES: usize = 4 << 20;

/// LIFO candidates: each open directory owns the tail of its immediate children.
/// Most tails are discarded without I/O; wide trees spill to one unlinked file.
struct Pending {
    file: BufWriter<File>,
    buf: Vec<(Record, Vec<u8>)>,
    bytes: usize,
    disk_end: u64,
    end: u64,
}
impl Pending {
    fn new(report: &Path) -> io::Result<Self> {
        Ok(Self {
            file: BufWriter::new(scratch(report)?),
            buf: Vec::new(),
            bytes: 0,
            disk_end: 0,
            end: 0,
        })
    }
    fn push(&mut self, record: Record, path: Vec<u8>) -> io::Result<()> {
        let bytes = WIDTH as usize + path.len();
        self.end = self.end.checked_add(bytes as u64).ok_or_else(corrupt)?;
        self.bytes += bytes;
        self.buf.push((record, path));
        if self.bytes >= PENDING_BYTES {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        for (mut record, path) in self.buf.drain(..) {
            record.seal();
            self.file.write_all(&record.0)?;
            self.file.write_all(&path)?;
        }
        self.bytes = 0;
        self.disk_end = self.end;
        Ok(())
    }
    fn finish(
        &mut self,
        start: u64,
        keep: bool,
        mut retain: impl FnMut(Record, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if start > self.end {
            return Err(corrupt());
        }
        if start >= self.disk_end {
            let mut offset = self.end;
            let mut split = self.buf.len();
            while offset > start {
                split = split.checked_sub(1).ok_or_else(corrupt)?;
                let (_, path) = &self.buf[split];
                offset -= WIDTH + path.len() as u64;
            }
            if offset != start {
                return Err(corrupt());
            }
            if keep {
                for (record, path) in self.buf.drain(split..) {
                    retain(record, &path)?;
                }
            } else {
                self.buf.truncate(split);
            }
            self.bytes -= (self.end - start) as usize;
        } else {
            if keep {
                self.flush()?;
                self.file.flush()?;
                let mut input = BufReader::new(self.file.get_ref().try_clone()?);
                input.seek(SeekFrom::Start(start))?;
                let mut offset = start;
                while offset < self.end {
                    let record = Record::read(&mut input)?.ok_or_else(corrupt)?;
                    let len = record.get(4);
                    if len > 64 << 10 || offset + WIDTH + len > self.end {
                        return Err(corrupt());
                    }
                    let mut path = vec![0; len as usize];
                    input.read_exact(&mut path)?;
                    offset += WIDTH + len;
                    retain(record, &path)?;
                }
            }
            self.buf.clear();
            self.bytes = 0;
            self.file.seek(SeekFrom::Start(start))?;
            self.disk_end = start;
        }
        self.end = start;
        Ok(())
    }
}

struct Frame {
    start: u64,
    depth: u32,
    path_len: usize,
    pending: u64,
}
impl Frame {
    fn close(
        self,
        end: u64,
        path: &[u8],
        pending: &mut Pending,
        sort: &mut Sorter<'_>,
        blobs: &mut impl Write,
        blob_bytes: &mut u64,
    ) -> io::Result<()> {
        let mut retain = |mut record: Record, path: &[u8]| {
            record.0[..32].copy_from_slice(&key(path));
            record.set(3, *blob_bytes);
            blobs.write_all(path)?;
            *blob_bytes += path.len() as u64;
            sort.push(record)
        };
        pending.finish(self.pending, end - self.start > MAX_FALLBACK, &mut retain)?;
        // Delay hashing and storing paths until a record is actually retained.
        let mut record = Record([0; WIDTH as usize]);
        record.set(0, self.start);
        record.set(1, end);
        record.set(2, u64::from(self.depth));
        record.set(4, path.len() as u64);
        if self.depth <= 1 || end - self.start >= MIN_RANGE {
            retain(record, path)
        } else {
            pending.push(record, path.to_vec())
        }
    }
}

struct Ranges {
    records: File,
    count: u64,
    blobs: File,
    blob_bytes: u64,
}
impl Ranges {
    fn build(
        report: &Path,
        reader: ReportReader<impl Read>,
        body_end: u64,
    ) -> Result<Self, ReportError> {
        let mut sort = Sorter::new(report);
        let mut blobs = BufWriter::new(scratch(report)?);
        let mut blob_bytes = 0;
        let mut stack: Vec<Frame> = Vec::new();
        let mut ancestors = Vec::new();
        let mut pending = Pending::new(report)?;
        let mut error = None;
        reader.for_each_span(|path, node, span| {
            let result = (|| -> io::Result<()> {
                while stack.last().is_some_and(|r| r.depth >= node.depth) {
                    let frame = stack.pop().expect("present");
                    let len = frame.path_len;
                    frame.close(
                        span.start,
                        &ancestors[..len],
                        &mut pending,
                        &mut sort,
                        &mut blobs,
                        &mut blob_bytes,
                    )?;
                    ancestors.truncate(stack.last().map_or(0, |r| r.path_len));
                }
                if node.kind == FileKind::Directory {
                    ancestors.clear();
                    ancestors.extend_from_slice(path);
                    stack.push(Frame {
                        start: span.start,
                        depth: node.depth,
                        path_len: path.len(),
                        pending: pending.end,
                    });
                }
                Ok(())
            })();
            if let Err(e) = result {
                error = Some(e);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })?;
        if let Some(error) = error {
            return Err(error.into());
        }
        for frame in stack.into_iter().rev() {
            let len = frame.path_len;
            frame.close(
                body_end,
                &ancestors[..len],
                &mut pending,
                &mut sort,
                &mut blobs,
                &mut blob_bytes,
            )?;
        }
        blobs.flush()?;
        let blobs = blobs.into_inner().map_err(|e| e.into_error())?;
        let (records, count) = sort.finish()?;
        Ok(Ranges {
            records,
            count,
            blobs,
            blob_bytes,
        })
    }
    fn precompute(&mut self, report: &Path, mount: &[u8]) -> Result<(), ReportError> {
        // Bounded extra passes: one volume and one share at a time, three rankings together.
        for n in 0..self.count {
            self.records.seek(SeekFrom::Start(n * WIDTH))?;
            let mut r = Record::read(&mut self.records)?.ok_or_else(corrupt)?;
            if r.get(2) > 1 {
                continue;
            }
            self.blobs.seek(SeekFrom::Start(r.get(3)))?;
            let mut path = vec![0; r.get(4) as usize];
            self.blobs.read_exact(&mut path)?;
            let queries: Vec<_> = [KindFilter::All, KindFilter::Dirs, KindFilter::Files]
                .into_iter()
                .map(|kind| TopQuery {
                    under: &path,
                    kind,
                    services: true,
                    by: Measure::Reclaimable,
                    include_system: false,
                    limit: 200,
                    only_shared: false,
                })
                .collect();
            let mut walks: Vec<_> = queries.iter().map(|q| TopWalk::new(mount, q)).collect();
            report::display_subtree(
                open(report)?,
                r.get(0)..r.get(1),
                u32::try_from(r.get(2)).map_err(|_| corrupt())?,
                &path,
                |p, node| {
                    for walk in &mut walks {
                        let _ = walk.visit(p, node);
                    }
                    ControlFlow::Continue(())
                },
            )?;
            for (n, walk) in walks.into_iter().enumerate() {
                let bytes = super::top_codec::encode(&walk.finish().ok_or_else(corrupt)?)?;
                if bytes.len() as u64 > MAX_BLOCK {
                    continue;
                }
                r.set(5 + n * 3, self.blob_bytes);
                r.set(6 + n * 3, bytes.len() as u64);
                r.set(7 + n * 3, codec::checksum(&bytes));
                self.blobs.seek(SeekFrom::End(0))?;
                self.blobs.write_all(&bytes)?;
                self.blob_bytes += bytes.len() as u64;
            }
            r.seal();
            self.records.seek(SeekFrom::Start(n * WIDTH))?;
            self.records.write_all(&r.0)?;
        }
        Ok(())
    }
    fn publish(&mut self, report: &Path, binding: Binding) -> Result<(), ReportError> {
        let header = Header {
            format: FORMAT,
            binding,
            count: self.count,
            blob_bytes: self.blob_bytes,
        };
        let bytes = serde_json::to_vec(&header).map_err(io::Error::other)?;
        atomic_write(&path(report), |out| {
            let mut out = BufWriter::new(out);
            out.write_all(MAGIC)?;
            out.write_all(&(bytes.len() as u32).to_le_bytes())?;
            out.write_all(&bytes)?;
            out.write_all(&codec::checksum(&bytes).to_le_bytes())?;
            self.records.rewind()?;
            self.blobs.rewind()?;
            io::copy(&mut self.records, &mut out)?;
            io::copy(&mut self.blobs, &mut out)?;
            out.flush()
        })?;
        Ok(())
    }
}

fn answer(
    report: &Path,
    mount: &[u8],
    query: &TopQuery<'_>,
) -> Result<Option<TopAnswer>, ReportError> {
    let mut index = Index::open(report)?;
    let Some((r, root)) = index.ancestor(query.under)? else {
        return Ok(None);
    };
    if r.get(0) >= r.get(1) || r.get(1) > index.header.binding.size - 16 {
        return Err(corrupt().into());
    }
    let n = match query.kind {
        KindFilter::All => 0,
        KindFilter::Dirs => 1,
        KindFilter::Files => 2,
    };
    if root == query.under
        && query.services
        && query.by == Measure::Reclaimable
        && !query.include_system
        && !query.only_shared
        && query.limit <= 200
        && r.get(6 + n * 3) > 0
    {
        let bytes = index.block(r.get(5 + n * 3), r.get(6 + n * 3))?;
        if codec::checksum(&bytes) != r.get(7 + n * 3) {
            return Err(corrupt().into());
        }
        let mut answer = super::top_codec::decode(&bytes).ok_or_else(corrupt)?;
        answer.ranking.entries.truncate(query.limit);
        answer.overlaps.retain(|overlap| {
            answer
                .ranking
                .entries
                .iter()
                .any(|e| e.path == overlap.target)
        });
        if answer.ranking.target.path != query.under {
            return Err(corrupt().into());
        }
        return Ok(Some(answer));
    }
    let mut walk = TopWalk::new(mount, query);
    report::display_subtree(
        open(report)?,
        r.get(0)..r.get(1),
        u32::try_from(r.get(2)).map_err(|_| corrupt())?,
        &root,
        |p, node| walk.visit(p, node),
    )?;
    Ok(walk.finish())
}

pub fn top(
    report: &Path,
    mount: &[u8],
    query: &TopQuery<'_>,
) -> Result<Option<TopAnswer>, ReportError> {
    let mut answer = top_records(report, mount, query)?;
    if query.services
        && query.under == mount
        && let Some(answer) = &mut answer
        && let Some(data) = &ReportReader::open(open(report)?)?.header().overview
    {
        answer.include_own_state(data);
    }
    Ok(answer)
}

fn top_records(
    report: &Path,
    mount: &[u8],
    query: &TopQuery<'_>,
) -> Result<Option<TopAnswer>, ReportError> {
    if ensure(report).is_ok() {
        match answer(report, mount, query) {
            Ok(Some(answer)) => return Ok(Some(answer)),
            Ok(None) => (), // A target outside indexed directory roots uses the checked walk.
            Err(_) => {
                if build(report).is_ok()
                    && let Ok(answer) = answer(report, mount, query)
                {
                    return Ok(answer);
                }
            }
        }
    }
    let reader = ReportReader::open(open(report)?)?;
    let mut walk = TopWalk::new(mount, query);
    reader.for_each(|path, node| walk.visit(path, node))?;
    Ok(walk.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{KindFilter, Measure, TopWalk};
    use crate::report::ReportReader;
    use std::fs;
    fn fixture() -> std::path::PathBuf {
        let root = crate::spill::testing::scratch("directory-index").join("state");
        crate::state::private_dir(&root, crate::sys::effective_uid()).unwrap();
        let report = root.join("scan.srrep");
        fs::write(&report, crate::query::tests::package_report_bytes()).unwrap();
        report
    }
    /// Directory-heavy model: 50 shares, 100 groups/share, 100 small folders/group,
    /// ten files/folder. Unlike the original benchmark, 9.2% of nodes are directories.
    #[test]
    #[ignore = "directory-heavy performance evidence; run on a Linux build host"]
    fn benchmark_directory_heavy_report() {
        use crate::model::accounting::{Blockers, Held};
        use crate::model::tree::{Metrics, NodeFlags};
        use crate::report::{NodeRecord, RootInfo, encode_record, write_records};
        use std::time::Instant;
        let root = PathBuf::from(std::env::var_os("SYNORECLAIM_BENCH_DIR").unwrap());
        crate::state::private_dir(&root, crate::sys::effective_uid()).unwrap();
        let report = root.join("directories.srrep");
        let header = report::testing::header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: crate::target::Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        let write = Instant::now();
        let mut out = BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&report)
                .unwrap(),
        );
        write_records(&mut out, &header, 5_505_051, |out| {
            let mut emit = |depth, name: &[u8], kind| {
                encode_record(
                    out,
                    &NodeRecord {
                        depth,
                        name,
                        kind,
                        flags: NodeFlags::default(),
                        metrics: Metrics {
                            logical: 4096,
                            physical: 4096,
                            reclaimable: 4096,
                            entries: 1,
                            files: u64::from(kind == FileKind::Regular),
                            ..Metrics::default()
                        },
                        blockers: Blockers::default(),
                        held: Held::default(),
                        marker: None,
                        outside: None,
                        nlink: 1,
                        tree: 5,
                        objectid: 256,
                    },
                )
            };
            emit(0, b"/v", FileKind::Directory)?;
            for share in 0..50 {
                emit(
                    1,
                    format!("share{share:02}").as_bytes(),
                    FileKind::Directory,
                )?;
                for group in 0..100 {
                    emit(
                        2,
                        format!("group{group:03}").as_bytes(),
                        FileKind::Directory,
                    )?;
                    for dir in 0..100 {
                        emit(3, format!("folder{dir:03}").as_bytes(), FileKind::Directory)?;
                        for file in 0..10 {
                            emit(
                                4,
                                format!("file{file:03}.dat").as_bytes(),
                                FileKind::Regular,
                            )?;
                        }
                    }
                }
            }
            Ok(())
        })
        .unwrap();
        out.flush().unwrap();
        drop(out);
        println!(
            "model nodes=5505051 directories=505051 report_bytes={} write_ms={}",
            fs::metadata(&report).unwrap().len(),
            write.elapsed().as_millis()
        );
        let _ = fs::remove_file(path(&report));
        let start = Instant::now();
        ensure(&report).unwrap();
        let build_ms = start.elapsed().as_millis();
        let mut index = Index::open(&report).unwrap();
        let mut cached_bytes = 0;
        for n in 0..index.header.count {
            let r = index.record(n).unwrap();
            for kind in 0..3 {
                cached_bytes += r.get(6 + kind * 3);
            }
        }
        println!(
            "index format={} records={} fixed_bytes={} path_bytes={} cached_bytes={} total_bytes={} build_ms={build_ms}",
            FORMAT,
            index.header.count,
            index.header.count * WIDTH,
            index.header.blob_bytes - cached_bytes,
            cached_bytes,
            index.file.metadata().unwrap().len()
        );
        for under in [
            &b"/v"[..],
            b"/v/share25",
            b"/v/share25/group050",
            b"/v/share25/group050/folder099",
        ] {
            let q = TopQuery {
                under,
                kind: KindFilter::All,
                services: true,
                by: Measure::Reclaimable,
                include_system: false,
                only_shared: false,
                limit: 200,
            };
            let mut walk = TopWalk::new(b"/v", &q);
            ReportReader::open(open(&report).unwrap())
                .unwrap()
                .for_each(|p, r| walk.visit(p, r))
                .unwrap();
            let expected = walk.finish();
            let mut times = Vec::new();
            for _ in 0..5 {
                let start = Instant::now();
                let got = top(&report, b"/v", &q).unwrap();
                times.push(start.elapsed().as_secs_f64() * 1000.);
                assert_eq!(got, expected);
            }
            println!(
                "target={} warm_ms={times:?}",
                String::from_utf8_lossy(under)
            );
        }
    }

    fn directory_fixture(folders: usize) -> PathBuf {
        use crate::model::accounting::{Blockers, Held};
        use crate::model::tree::{Metrics, NodeFlags};
        use crate::report::{NodeRecord, RootInfo, encode_record, write_records};
        let report = fixture();
        let header = report::testing::header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: crate::target::Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        let mut out = BufWriter::new(File::create(&report).unwrap());
        write_records(&mut out, &header, 3 + folders as u64 * 11, |out| {
            let mut emit = |depth, name: &[u8], kind| {
                encode_record(
                    out,
                    &NodeRecord {
                        depth,
                        name,
                        kind,
                        flags: NodeFlags::default(),
                        metrics: Metrics {
                            physical: 4096,
                            reclaimable: 2048,
                            entries: 1,
                            ..Metrics::default()
                        },
                        blockers: Blockers::default(),
                        held: Held::default(),
                        marker: None,
                        outside: None,
                        nlink: 1,
                        tree: 5,
                        objectid: 256,
                    },
                )
            };
            emit(0, b"/v", FileKind::Directory)?;
            emit(1, b"share", FileKind::Directory)?;
            emit(2, b"group", FileKind::Directory)?;
            for dir in 0..folders {
                emit(3, format!("d{dir:05}").as_bytes(), FileKind::Directory)?;
                for file in 0..10 {
                    emit(
                        4,
                        format!("file{file:02}.dat").as_bytes(),
                        FileKind::Regular,
                    )?;
                }
            }
            Ok(())
        })
        .unwrap();
        out.flush().unwrap();
        report
    }

    #[test]
    fn small_directories_use_a_bounded_ancestor_with_streaming_parity() {
        let report = directory_fixture(300);
        ensure(&report).unwrap();
        let mut index = Index::open(&report).unwrap();
        assert!(index.find(b"/v/share/group/d00299").unwrap().is_none());
        assert_eq!(
            index.header.count, 3,
            "omit small leaf directory records and paths"
        );
        for under in [
            &b"/v/share/group/d00299"[..],
            b"/v/share/group/d00299/file09.dat",
            b"/v/share/group/missing",
            b"/v/share/group/d00299/missing",
        ] {
            for kind in [KindFilter::All, KindFilter::Dirs, KindFilter::Files] {
                for (by, services, include_system, only_shared) in [
                    (Measure::Reclaimable, true, false, false),
                    (Measure::Physical, false, true, false),
                    (Measure::Shared, true, true, true),
                ] {
                    let q = TopQuery {
                        under,
                        kind,
                        by,
                        services,
                        include_system,
                        only_shared,
                        limit: 5,
                    };
                    let mut walk = TopWalk::new(b"/v", &q);
                    ReportReader::open(open(&report).unwrap())
                        .unwrap()
                        .for_each(|p, r| walk.visit(p, r))
                        .unwrap();
                    assert_eq!(top(&report, b"/v", &q).unwrap(), walk.finish());
                }
            }
        }
    }

    #[test]
    fn wide_directories_retain_small_children_to_bound_fallback_reads() {
        let report = directory_fixture(5000);
        ensure(&report).unwrap();
        let mut index = Index::open(&report).unwrap();
        assert_eq!(index.header.count, 5003);
        let leaf = index.find(b"/v/share/group/d04999").unwrap().unwrap();
        assert!(leaf.get(1) - leaf.get(0) < 64 << 10);
    }

    #[test]
    fn deferred_candidates_spill_rewind_and_discard_with_bounded_memory() {
        let report = fixture();
        let mut pending = Pending::new(&report).unwrap();
        let count = RUN;
        for n in 0..count {
            let path = format!("/v/folder{n:05}").into_bytes();
            let record = Record::new(&path, n as u64, 2, 0);
            pending.push(record, path).unwrap();
            assert!(pending.bytes < PENDING_BYTES);
        }
        assert!(pending.disk_end > 0);
        let mut got = 0;
        pending
            .finish(0, true, |record, path| {
                assert_eq!(record.get(0), got);
                assert_eq!(path, format!("/v/folder{got:05}").as_bytes());
                got += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(got, count as u64);
        assert_eq!(pending.end, 0);
        // Reuse the same scratch range; stale tail bytes must not be read.
        let path = b"/v/new-\xff";
        pending
            .push(Record::new(path, 7, 2, 0), path.to_vec())
            .unwrap();
        pending.flush().unwrap();
        let mut got = Vec::new();
        pending
            .finish(0, true, |_, p| {
                got.push(p.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(got, vec![path.to_vec()]);
        for n in 0..count {
            let path = format!("/v/discard{n}").into_bytes();
            pending
                .push(Record::new(&path, n as u64, 2, 0), path)
                .unwrap();
        }
        pending
            .finish(0, false, |_, _| {
                panic!("discard must not decode candidates")
            })
            .unwrap();
        assert_eq!(pending.end, 0);
        assert_eq!(fs::read_dir(report.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn sparse_non_utf8_paths_and_failed_publication_keep_checked_fallback() {
        let report = directory_fixture(300);
        let original = report::Report::parse(fs::read(&report).unwrap()).unwrap();
        let mut records = Vec::new();
        original.for_each(|_, row| {
            let mut row = *row;
            if row.name == b"group" {
                row.name = b"group-\xff";
            }
            report::encode_record(&mut codec::HashingWriter::new(&mut records), &row).unwrap();
            ControlFlow::Continue(())
        });
        report::write_records(
            File::create(&report).unwrap(),
            &original.header,
            original.node_count(),
            |out| out.put(&records),
        )
        .unwrap();
        let q = TopQuery {
            under: b"/v/share/group-\xff/d00299",
            kind: KindFilter::All,
            services: true,
            by: Measure::Reclaimable,
            include_system: false,
            only_shared: false,
            limit: 200,
        };
        let mut walk = TopWalk::new(b"/v", &q);
        ReportReader::open(open(&report).unwrap())
            .unwrap()
            .for_each(|p, r| walk.visit(p, r))
            .unwrap();
        let expected = walk.finish();
        ensure(&report).unwrap();
        let mut index = Index::open(&report).unwrap();
        let (r, ancestor) = index.ancestor(q.under).unwrap().unwrap();
        assert_eq!(ancestor, b"/v/share/group-\xff");
        assert!(r.get(1) - r.get(0) <= MAX_FALLBACK);
        assert_eq!(top(&report, b"/v", &q).unwrap(), expected);
        fs::remove_file(path(&report)).unwrap();
        fs::create_dir(path(&report)).unwrap();
        assert_eq!(top(&report, b"/v", &q).unwrap(), expected);
        // Persistence failure must still validate the original report checksum.
        let mut bytes = fs::read(&report).unwrap();
        let len = bytes.len();
        bytes[len - 1] ^= 1;
        fs::write(&report, bytes).unwrap();
        assert!(top(&report, b"/v", &q).is_err());
    }

    #[test]
    fn indexed_top_preserves_rankings_packages_and_volume_service_rows() {
        let report = fixture();
        ensure(&report).unwrap();
        for under in [&b"/v"[..], b"/v/share", b"/v/share/abb"] {
            for kind in [KindFilter::All, KindFilter::Dirs, KindFilter::Files] {
                for (by, services, limit, include_system, only_shared) in [
                    (Measure::Reclaimable, true, 200, false, false),
                    (Measure::Physical, true, 1, true, false),
                    (Measure::Shared, false, 5, true, true),
                ] {
                    let q = TopQuery {
                        under,
                        kind,
                        by,
                        services,
                        limit,
                        include_system,
                        only_shared,
                    };
                    let reader = ReportReader::open(fs::File::open(&report).unwrap()).unwrap();
                    let mount = reader.header().mount.clone();
                    let mut walk = TopWalk::new(&mount, &q);
                    reader.for_each(|p, r| walk.visit(p, r)).unwrap();
                    let original = walk.finish();
                    if under == b"/v" && services {
                        assert!(!original.as_ref().unwrap().services.uses.is_empty());
                    }
                    assert_eq!(top(&report, &mount, &q).unwrap(), original);
                }
            }
        }
    }
    #[test]
    fn indexed_nested_full_rows_match_checked_precomputed_and_range_answers() {
        let report = fixture();
        fs::write(&report, crate::query::tests::nested_report_bytes()).unwrap();
        ensure(&report).unwrap();
        for under in [&b"/v"[..], b"/v/share", b"/v/share/deep"] {
            for kind in [KindFilter::All, KindFilter::Dirs, KindFilter::Files] {
                for by in [Measure::Reclaimable, Measure::Physical] {
                    for limit in [0, 1, 2, 3, 200] {
                        let q = TopQuery {
                            under,
                            kind,
                            by,
                            services: true,
                            include_system: false,
                            only_shared: false,
                            limit,
                        };
                        let mut walk = TopWalk::new(b"/v", &q);
                        ReportReader::open(File::open(&report).unwrap())
                            .unwrap()
                            .for_each(|p, node| walk.visit(p, node))
                            .unwrap();
                        let checked = walk.finish().unwrap();
                        let indexed = top(&report, b"/v", &q).unwrap().unwrap();
                        assert_eq!(indexed, checked);
                        let expected =
                            crate::query::full_rows(&checked.ranking, &checked.services, by, limit);
                        let cached =
                            crate::query::full_rows(&indexed.ranking, &indexed.services, by, limit);
                        assert_eq!(cached, expected);
                        assert_eq!(
                            crate::query::inclusions(&cached),
                            crate::query::inclusions(&expected)
                        );
                        if under != b"/v" {
                            // Share answers retain the original aggregated service rows.
                            assert_eq!(
                                cached,
                                crate::query::rows(&indexed.ranking, &indexed.services, by, limit)
                            );
                        }
                    }
                }
            }
        }
        let mut index = Index::open(&report).unwrap();
        for under in [&b"/v"[..], b"/v/share"] {
            let (r, root) = index.ancestor(under).unwrap().unwrap();
            assert_eq!(root, under);
            assert!(r.get(6) > 0, "test must exercise the precomputed answer");
        }
        // An unavailable index triggers the checked-walk fallback, not a stale answer.
        fs::remove_file(path(&report)).unwrap();
        fs::create_dir(path(&report)).unwrap();
        let q = TopQuery {
            under: b"/v",
            kind: KindFilter::All,
            by: Measure::Reclaimable,
            services: true,
            include_system: false,
            only_shared: false,
            limit: 200,
        };
        let answer = top(&report, b"/v", &q).unwrap().unwrap();
        let rows = crate::query::full_rows(&answer.ranking, &answer.services, q.by, q.limit);
        let represented: u64 = rows
            .iter()
            .zip(crate::query::inclusions(&rows))
            .filter(|(_, within)| within.is_none())
            .map(|(row, _)| row.full_metrics().physical)
            .sum();
        assert_eq!(represented, 13 << 20);
        fs::remove_dir(path(&report)).unwrap();
    }

    #[test]
    fn indexed_eadir_top_matches_the_checked_walk_and_preserves_once_only_totals() {
        let report = fixture();
        fs::write(&report, crate::summary::tests::eadir_report_bytes()).unwrap();
        ensure(&report).unwrap();
        for under in [
            &b"/v"[..],
            b"/v/share",
            b"/v/share/album",
            b"/v/share/album/@eaDir",
        ] {
            for kind in [KindFilter::All, KindFilter::Dirs, KindFilter::Files] {
                for (by, services, include_system, only_shared) in [
                    (Measure::Reclaimable, true, false, false),
                    (Measure::Physical, true, false, false),
                    (Measure::Physical, true, true, false),
                    (Measure::Shared, false, false, true),
                ] {
                    for limit in [0, 1, 2, 200] {
                        let q = TopQuery {
                            under,
                            kind,
                            by,
                            services,
                            include_system,
                            only_shared,
                            limit,
                        };
                        let mut walk = TopWalk::new(b"/v", &q);
                        ReportReader::open(File::open(&report).unwrap())
                            .unwrap()
                            .for_each(|p, node| walk.visit(p, node))
                            .unwrap();
                        let expected = walk.finish().unwrap();
                        let cached = top(&report, b"/v", &q).unwrap().unwrap();
                        assert_eq!(cached, expected);
                        // Public JSON and human ranking use the same subset after a codec round trip.
                        assert_eq!(
                            crate::query::rows(&cached.ranking, &cached.services, by, limit),
                            crate::query::rows(&expected.ranking, &expected.services, by, limit)
                        );
                        assert_eq!(
                            crate::document::ServicesJson::from(&cached.services),
                            crate::document::ServicesJson::from(&expected.services)
                        );
                        if under == b"/v" && kind == KindFilter::All && services && limit == 200 {
                            let rows =
                                crate::query::rows(&cached.ranking, &cached.services, by, limit);
                            assert_eq!(
                                rows.iter().map(|r| r.metrics().physical).sum::<u64>(),
                                22 << 20
                            );
                            assert_eq!(
                                rows.iter().map(|r| r.metrics().reclaimable).sum::<u64>(),
                                12 << 20
                            );
                        }
                    }
                }
            }
        }
        // Both root and share precomputed answers include the metadata distinction.
        let mut index = Index::open(&report).unwrap();
        for under in [&b"/v"[..], b"/v/share"] {
            let (r, root) = index.ancestor(under).unwrap().unwrap();
            assert_eq!(root, under);
            assert!(r.get(6) > 0);
        }
    }

    fn vmm_fixture() -> PathBuf {
        use crate::model::accounting::{Blockers, Held, HeldClass};
        use crate::model::tree::{Metrics, NodeFlags};
        use crate::report::{NodeRecord, RootInfo, encode_record, write_records};
        let path = fixture();
        let header = report::testing::header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: crate::target::Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        let rows = [
            (0, b"/v".as_slice(), 100, 60, 8, false),
            (1, b"@iSCSI", 50, 30, 5, false),
            (2, b"LUN", 50, 30, 5, false),
            (3, b"lun1", 20, 10, 2, false),
            (3, b"VDISK_a", 30, 20, 3, false),
            (4, b"disk.img", 30, 20, 3, false),
            (4, b"snap", 0, 0, 0, true),
            (3, b"lun2", 0, 0, 0, true),
        ];
        write_records(
            File::create(&path).unwrap(),
            &header,
            rows.len() as u64,
            |out| {
                for (n, (depth, name, physical, reclaimable, unknown, snapshot)) in
                    rows.into_iter().enumerate()
                {
                    let flags = if snapshot {
                        NodeFlags::SYSTEM.with(NodeFlags::SNAPSHOT)
                    } else if depth > 0 {
                        NodeFlags::SYSTEM
                    } else {
                        NodeFlags::default()
                    };
                    let record = NodeRecord {
                        depth,
                        name,
                        kind: if name == b"disk.img" {
                            FileKind::Regular
                        } else {
                            FileKind::Directory
                        },
                        flags,
                        metrics: Metrics {
                            physical,
                            reclaimable,
                            unknown,
                            ..Metrics::default()
                        },
                        blockers: Blockers::SNAPSHOT,
                        held: Held::of(HeldClass::Snapshot, physical - reclaimable - unknown),
                        marker: None,
                        outside: None,
                        nlink: 1,
                        tree: 5,
                        objectid: 256 + n as u64,
                    };
                    encode_record(out, &record)?;
                }
                Ok(())
            },
        )
        .unwrap();
        path
    }

    #[test]
    fn cached_volume_services_preserve_the_vmm_split_and_original_top_answer() {
        let report = vmm_fixture();
        ensure(&report).unwrap();
        for kind in [KindFilter::All, KindFilter::Dirs, KindFilter::Files] {
            let q = TopQuery {
                under: b"/v",
                kind,
                services: true,
                by: Measure::Reclaimable,
                limit: 200,
                include_system: false,
                only_shared: false,
            };
            let reader = ReportReader::open(File::open(&report).unwrap()).unwrap();
            let mut walk = TopWalk::new(b"/v", &q);
            reader.for_each(|p, r| walk.visit(p, r)).unwrap();
            let original = walk.finish().unwrap();
            let cached = top(&report, b"/v", &q).unwrap().unwrap();
            assert_eq!(cached, original);
            let line = |code| {
                cached
                    .services
                    .uses
                    .iter()
                    .find(|s| s.service.is_some_and(|x| x.code == code))
                    .unwrap()
            };
            let vmm = line("vmm_virtual_disks");
            let iscsi = line("iscsi_luns");
            assert_eq!(
                (
                    vmm.metrics.physical,
                    vmm.metrics.reclaimable,
                    vmm.metrics.unknown
                ),
                (30, 20, 3)
            );
            assert_eq!(
                (
                    iscsi.metrics.physical,
                    iscsi.metrics.reclaimable,
                    iscsi.metrics.unknown
                ),
                (20, 10, 2)
            );
            assert_eq!(vmm.snapshots_not_read, 1);
            assert_eq!(iscsi.snapshots_not_read, 1);
            assert_eq!(
                vmm.held.get(crate::model::accounting::HeldClass::Snapshot),
                7
            );
            assert_eq!(
                iscsi
                    .held
                    .get(crate::model::accounting::HeldClass::Snapshot),
                8
            );
            if kind == KindFilter::All {
                let rows =
                    crate::query::full_rows(&cached.ranking, &cached.services, q.by, q.limit);
                let within = crate::query::inclusions(&rows);
                assert!(
                    within.iter().all(Option::is_none),
                    "VMM is not included in iSCSI"
                );
                assert_eq!(
                    rows.iter()
                        .zip(within)
                        .filter(|(_, p)| p.is_none())
                        .map(|(r, _)| r.full_metrics().physical)
                        .sum::<u64>(),
                    50
                );
            }
            let rows = crate::query::rows(&cached.ranking, &cached.services, q.by, q.limit);
            assert!(rows.iter().any(|row| match row {
                crate::query::Row::Service(s) => {
                    s.service.is_some_and(|x| x.code == "vmm_virtual_disks")
                }
                _ => false,
            }));
        }
    }

    #[test]
    fn previous_index_formats_are_rejected_and_rebuilt() {
        let report = vmm_fixture();
        ensure(&report).unwrap();
        for old_format in [1, 2, 3, 4] {
            let mut index = Index::open(&report).unwrap();
            let before = index.file.metadata().unwrap().ino();
            index.header.format = old_format;
            let bytes = serde_json::to_vec(&index.header).unwrap();
            assert_eq!(bytes.len() as u64 + 20, index.records);
            let mut file = OpenOptions::new().write(true).open(path(&report)).unwrap();
            file.seek(SeekFrom::Start(12)).unwrap();
            file.write_all(&bytes).unwrap();
            file.write_all(&codec::checksum(&bytes).to_le_bytes())
                .unwrap();
            assert!(
                Index::open(&report).is_err(),
                "old classifier answers must never be reused"
            );
            ensure(&report).unwrap();
            let after = Index::open(&report).unwrap();
            assert_ne!(after.file.metadata().unwrap().ino(), before);
            assert_eq!(after.header.format, FORMAT);
            ensure(&report).unwrap();
            assert_eq!(
                fs::metadata(path(&report)).unwrap().ino(),
                after.file.metadata().unwrap().ino()
            );
        }
    }

    #[test]
    fn default_limits_keep_services_without_reading_the_report_body() {
        let report = fixture();
        ensure(&report).unwrap();
        let mut expected = Vec::new();
        for limit in [0, 1, 10, 200] {
            let q = TopQuery {
                under: b"/v",
                services: true,
                by: Measure::Reclaimable,
                kind: KindFilter::All,
                include_system: false,
                only_shared: false,
                limit,
            };
            let reader = ReportReader::open(fs::File::open(&report).unwrap()).unwrap();
            let mut walk = TopWalk::new(b"/v", &q);
            reader.for_each(|p, r| walk.visit(p, r)).unwrap();
            expected.push(walk.finish().unwrap());
        }
        // Display trusts the private derived answer + stat/trailer binding, not a body hash.
        let meta = fs::metadata(&report).unwrap();
        let mut idx = Index::open(&report).unwrap();
        let r = idx.find(b"/v").unwrap().unwrap();
        let mut f = OpenOptions::new().write(true).open(&report).unwrap();
        f.seek(SeekFrom::Start(r.get(0))).unwrap();
        f.write_all(&[0xff; 8]).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(meta.modified().unwrap()))
            .unwrap();
        assert!(crate::report::Report::parse(fs::read(&report).unwrap()).is_err());
        for (limit, expected) in [0, 1, 10, 200].into_iter().zip(expected) {
            let q = TopQuery {
                under: b"/v",
                services: true,
                by: Measure::Reclaimable,
                kind: KindFilter::All,
                include_system: false,
                only_shared: false,
                limit,
            };
            let got = top(&report, b"/v", &q).unwrap().unwrap();
            assert!(!got.services.uses.is_empty());
            assert_eq!(got, expected);
        }
    }
    #[test]
    fn spill_sort_merges_multiple_runs_with_bounded_descriptors() {
        let report = fixture();
        let mut sort = Sorter::new(&report);
        let count = RUN * 3 + 7;
        for n in (0..count).rev() {
            sort.push(Record::new(format!("/v/d{n}").as_bytes(), n as u64, 1, 0))
                .unwrap();
            assert!(sort.buf.len() < RUN);
            assert!(sort.levels.iter().flatten().count() <= 2);
        }
        let (mut sorted, got) = sort.finish().unwrap();
        assert_eq!(got, count as u64);
        let mut last = [0; 32];
        for _ in 0..count {
            let r = Record::read(&mut sorted).unwrap().unwrap();
            assert!(r.0[..32] >= last[..]);
            last.copy_from_slice(&r.0[..32]);
        }
        assert!(Record::read(&mut sorted).unwrap().is_none());
        assert_eq!(fs::read_dir(report.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn touched_record_header_and_cached_block_corruption_rebuild_once() {
        let report = fixture();
        ensure(&report).unwrap();
        let q = TopQuery {
            under: b"/v",
            services: true,
            by: Measure::Reclaimable,
            kind: KindFilter::All,
            include_system: false,
            only_shared: false,
            limit: 200,
        };
        let expected = top(&report, b"/v", &q).unwrap();
        for section in 0..3 {
            let mut index = Index::open(&report).unwrap();
            let r = index.find(b"/v").unwrap().unwrap();
            let root_position = (0..index.header.count)
                .find(|n| index.record(*n).unwrap().get(0) == r.get(0))
                .unwrap();
            let offset = match section {
                0 => 12,
                1 => index.records + root_position * WIDTH,
                _ => index.blobs + r.get(5),
            };
            let before = fs::metadata(path(&report)).unwrap().ino();
            let mut f = OpenOptions::new().write(true).open(path(&report)).unwrap();
            f.seek(SeekFrom::Start(offset)).unwrap();
            f.write_all(&[0xff; 8]).unwrap();
            assert_eq!(top(&report, b"/v", &q).unwrap(), expected);
            let after = fs::metadata(path(&report)).unwrap().ino();
            assert_ne!(after, before);
            assert_eq!(top(&report, b"/v", &q).unwrap(), expected);
            assert_eq!(fs::metadata(path(&report)).unwrap().ino(), after);
        }
    }

    #[test]
    fn deep_range_is_bounded_and_lossless_and_replacement_invalidates_binding() {
        let report = fixture();
        ensure(&report).unwrap();
        let mut index = Index::open(&report).unwrap();
        let (r, _) = index.ancestor(b"/v/share/abb").unwrap().unwrap();
        assert!(r.get(1) - r.get(0) <= MAX_FALLBACK);
        let old_inode = fs::metadata(path(&report)).unwrap().ino();
        let replacement = report.with_extension("replacement");
        fs::copy(&report, &replacement).unwrap();
        fs::rename(replacement, &report).unwrap();
        assert!(Index::open(&report).is_err());
        ensure(&report).unwrap();
        assert_ne!(fs::metadata(path(&report)).unwrap().ino(), old_inode);
        let non_utf8 = report.with_file_name("lossless.srrep");
        let original = crate::query::tests::package_report();
        let mut records = Vec::new();
        original.for_each(|_, r| {
            let mut r = *r;
            if r.name == b"share" {
                r.name = b"share-\xff";
            }
            report::encode_record(&mut crate::codec::HashingWriter::new(&mut records), &r).unwrap();
            ControlFlow::Continue(())
        });
        report::write_records(
            fs::File::create(&non_utf8).unwrap(),
            &original.header,
            original.node_count(),
            |w| w.put(&records),
        )
        .unwrap();
        ensure(&non_utf8).unwrap();
        let q = TopQuery {
            under: b"/v/share-\xff",
            services: true,
            by: Measure::Reclaimable,
            kind: KindFilter::All,
            include_system: false,
            only_shared: false,
            limit: 200,
        };
        let reader = ReportReader::open(fs::File::open(&non_utf8).unwrap()).unwrap();
        let mut walk = TopWalk::new(b"/v", &q);
        reader.for_each(|p, r| walk.visit(p, r)).unwrap();
        assert_eq!(top(&non_utf8, b"/v", &q).unwrap(), walk.finish());
    }
}
