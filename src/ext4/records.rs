//! Bounded external sorting using the existing report record codec and spill
//! chunks. Two-way, levelled merges keep both readers and run names bounded.

use crate::btrfs::walk::{BatchBoundary, StopReason, WalkError};
use crate::codec::HashingWriter;
use crate::model::accounting::{Blockers, Held};
use crate::model::namespace::FileKind;
use crate::model::tree::{Metrics, NodeFlags, Outside};
use crate::report::{NodeRecord, decode_record, encode_record};
use crate::spill::{Appender, ChunkReader};
use std::cmp::Ordering;
use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Stopped(StopReason);
impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ext4 scan stopped: {:?}", self.0)
    }
}
impl std::error::Error for Stopped {}

pub fn checkpoint(boundary: &mut dyn BatchBoundary) -> io::Result<()> {
    match boundary.after_batch() {
        ControlFlow::Break(StopReason::ResourceLimit) => Ok(()), // memory triggers flushing, never stopping
        ControlFlow::Break(reason) => Err(io::Error::other(Stopped(reason))),
        ControlFlow::Continue(()) => Ok(()),
    }
}

pub fn walk_error(error: io::Error) -> WalkError {
    if let Some(stopped) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<Stopped>())
    {
        WalkError::Stopped(stopped.0)
    } else {
        WalkError::Spill(error)
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub depth: u32,
    pub kind: FileKind,
    pub flags: NodeFlags,
    pub name: Vec<u8>,
    pub metrics: Metrics,
    pub blockers: Blockers,
    pub held: Held,
    pub marker: Option<&'static [u8]>,
    pub outside: Option<Outside>,
    pub nlink: u32,
    pub device: u64,
    pub inode: u64,
}

impl Record {
    pub fn borrowed(&self) -> NodeRecord<'_> {
        NodeRecord {
            depth: self.depth,
            kind: self.kind,
            flags: self.flags,
            name: &self.name,
            metrics: self.metrics,
            blockers: self.blockers,
            held: self.held,
            marker: self.marker,
            outside: self.outside,
            nlink: self.nlink,
            tree: self.device,
            objectid: self.inode,
        }
    }

    fn owned(record: NodeRecord<'_>) -> Self {
        Self {
            depth: record.depth,
            kind: record.kind,
            flags: record.flags,
            name: record.name.to_vec(),
            metrics: record.metrics,
            blockers: record.blockers,
            held: record.held,
            marker: record.marker,
            outside: record.outside,
            nlink: record.nlink,
            device: record.tree,
            inode: record.objectid,
        }
    }

    pub fn contribution(name: Vec<u8>, depth: u32, metrics: Metrics) -> Self {
        Self {
            depth,
            kind: FileKind::Directory,
            flags: NodeFlags::default(),
            name,
            metrics,
            blockers: Blockers::default(),
            held: Held::default(),
            marker: None,
            outside: None,
            nlink: 0,
            device: 0,
            inode: 0,
        }
    }

    fn combine(&mut self, other: Self) {
        self.metrics.add(&other.metrics);
        self.held.add(&other.held);
        self.blockers = self.blockers.with(other.blockers);
        if let Some(outside) = other.outside {
            let total = self.outside.get_or_insert_with(Outside::default);
            total.metrics.add(&outside.metrics);
            total.held.add(&outside.held);
        }
        if self.device == 0 && other.device != 0 {
            self.kind = other.kind;
            self.flags = other.flags;
            self.marker = other.marker;
            self.nlink = other.nlink;
            self.device = other.device;
            self.inode = other.inode;
        }
    }
}

pub struct Reader(ChunkReader);
impl Reader {
    pub fn new(path: PathBuf) -> Self {
        Self(ChunkReader::with_max_record(path, 40 << 10))
    }
    pub fn read(&mut self) -> io::Result<Option<Record>> {
        self.0.next(|d| decode_record(d).map(Record::owned))
    }

    /// Count spill work separately from namespace entries. In particular a
    /// long merge must keep moving even when the directory walk has finished.
    pub fn read_tracked(&mut self, boundary: &mut dyn BatchBoundary) -> io::Result<Option<Record>> {
        let next = self.0.next(|d| {
            let start = d.at();
            let record = decode_record(d).map(Record::owned)?;
            Some((record, (d.at() - start) as u64))
        })?;
        Ok(next.map(|(record, bytes)| {
            boundary.took(&crate::btrfs::walk::WalkStats {
                items: 1,
                item_bytes: bytes,
                ..Default::default()
            });
            record
        }))
    }
}

pub fn append(out: &mut Appender, record: &Record) -> io::Result<()> {
    encode_record(&mut HashingWriter::new(out.buf()), &record.borrowed())?;
    out.commit()
}

/// Compare components, so `a/child` stays below `a` and before `a-sibling`.
pub fn path_order(a: &[u8], b: &[u8]) -> Ordering {
    a.split(|&byte| byte == b'/')
        .cmp(b.split(|&byte| byte == b'/'))
}

#[derive(Clone, Copy)]
pub enum Order {
    Inode,
    Path,
}
impl Order {
    fn compare(self, a: &Record, b: &Record) -> Ordering {
        match self {
            Self::Inode => (a.device, a.inode)
                .cmp(&(b.device, b.inode))
                .then_with(|| path_order(&a.name, &b.name)),
            Self::Path => path_order(&a.name, &b.name),
        }
    }
    fn combine(self, a: &Record, b: &Record) -> bool {
        matches!(self, Self::Path) && a.name == b.name
    }
}

pub struct Sorter {
    dir: PathBuf,
    tag: &'static str,
    order: Order,
    budget: u64,
    bytes: u64,
    buffer: Vec<Record>,
    levels: Vec<Option<PathBuf>>,
    serial: u64,
}

impl Sorter {
    pub fn new(dir: PathBuf, tag: &'static str, order: Order, budget: u64) -> Self {
        Self {
            dir,
            tag,
            order,
            budget: budget.max(1),
            bytes: 0,
            buffer: Vec::new(),
            levels: Vec::new(),
            serial: 0,
        }
    }

    pub fn push(&mut self, record: Record, boundary: &mut dyn BatchBoundary) -> io::Result<()> {
        self.bytes = self
            .bytes
            .saturating_add(record.name.capacity() as u64 + std::mem::size_of::<Record>() as u64);
        self.buffer.push(record);
        if self.bytes >= self.budget {
            self.flush(boundary)?;
        }
        Ok(())
    }

    fn path(&mut self) -> PathBuf {
        let path = self.dir.join(format!("{}.run.{}", self.tag, self.serial));
        self.serial += 1;
        path
    }

    fn flush(&mut self, boundary: &mut dyn BatchBoundary) -> io::Result<()> {
        checkpoint(boundary)?;
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer
            .sort_unstable_by(|a, b| self.order.compare(a, b));
        let mut path = self.path();
        let mut out = Appender::new(path.to_path_buf());
        let mut pending: Option<Record> = None;
        for record in self.buffer.drain(..) {
            if let Some(last) = &mut pending
                && self.order.combine(last, &record)
            {
                last.combine(record);
            } else {
                if let Some(last) = pending.take() {
                    append(&mut out, &last)?;
                }
                pending = Some(record);
            }
        }
        if let Some(last) = pending {
            append(&mut out, &last)?;
        }
        out.flush()?;
        self.bytes = 0;
        // Drop the arena as well as its contents before allocating merge readers.
        self.buffer = Vec::new();
        let mut level = 0;
        loop {
            if level == self.levels.len() {
                self.levels.push(None);
            }
            let Some(previous) = self.levels[level].take() else {
                self.levels[level] = Some(path);
                break;
            };
            let merged = self.path();
            merge(&previous, &path, &merged, self.order, boundary)?;
            std::fs::remove_file(previous)?;
            std::fs::remove_file(path)?;
            path = merged;
            level += 1;
        }
        Ok(())
    }

    pub fn finish(
        mut self,
        destination: PathBuf,
        boundary: &mut dyn BatchBoundary,
    ) -> io::Result<()> {
        self.flush(boundary)?;
        let paths: Vec<_> = self.levels.iter_mut().filter_map(Option::take).collect();
        let mut pending: Option<PathBuf> = None;
        for path in paths {
            pending = Some(match pending {
                None => path,
                Some(previous) => {
                    let merged = self.path();
                    merge(&previous, &path, &merged, self.order, boundary)?;
                    std::fs::remove_file(previous)?;
                    std::fs::remove_file(path)?;
                    merged
                }
            });
        }
        match pending {
            Some(path) => std::fs::rename(path, destination),
            None => Appender::new(destination).flush(),
        }
    }
}

fn merge(
    a: &Path,
    b: &Path,
    path: &Path,
    order: Order,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<()> {
    let (mut left, mut right) = (Reader::new(a.to_path_buf()), Reader::new(b.to_path_buf()));
    let (mut x, mut y) = (left.read_tracked(boundary)?, right.read_tracked(boundary)?);
    let mut out = Appender::new(path.to_path_buf());
    let mut steps = 0u64;
    while x.is_some() || y.is_some() {
        steps += 1;
        if steps % 256 == 0 {
            checkpoint(boundary)?;
        }
        if let (Some(a), Some(b)) = (&x, &y)
            && order.combine(a, b)
        {
            let mut record = x.take().expect("present");
            record.combine(y.take().expect("present"));
            append(&mut out, &record)?;
            x = left.read_tracked(boundary)?;
            y = right.read_tracked(boundary)?;
        } else if x.as_ref().is_some_and(|a| {
            y.as_ref()
                .is_none_or(|b| order.compare(a, b) != Ordering::Greater)
        }) {
            append(&mut out, &x.take().expect("present"))?;
            x = left.read_tracked(boundary)?;
        } else {
            append(&mut out, &y.take().expect("present"))?;
            y = right.read_tracked(boundary)?;
        }
    }
    out.flush()
}
