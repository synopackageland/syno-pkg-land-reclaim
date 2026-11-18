//! External sorting of snapshot references. Binary merge levels keep one
//! descriptor per level and a fixed record buffer, independent of tree count.
use super::{ExtentUse, JOIN_CHUNK, reserve};
use crate::btrfs::walk::{BatchBoundary, WalkError};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::ops::ControlFlow;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const WIDTH: usize = 49;
const RUN: usize = 32_768;
type Record = [u8; WIDTH];

fn encode(u: ExtentUse, add: bool) -> Record {
    let mut r = [0; WIDTH];
    for (i, value) in [
        u.bytenr,
        u.start,
        u.end.unwrap_or(u64::MAX),
        u.ino,
        u.disk_num_bytes,
    ]
    .into_iter()
    .enumerate()
    {
        r[i * 8..i * 8 + 8].copy_from_slice(&value.to_be_bytes());
    }
    r[40] = u8::from(u.mappable) | (u8::from(u.end.is_some()) << 1) | (u8::from(add) << 2);
    r[41..].copy_from_slice(&(u.root as u64).to_be_bytes());
    r
}

fn decode(r: Record) -> io::Result<(ExtentUse, bool)> {
    let word = |i: usize| u64::from_be_bytes(r[i * 8..i * 8 + 8].try_into().expect("word"));
    if r[40] > 7 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid snapshot reference",
        ));
    }
    Ok((
        ExtentUse {
            root: usize::try_from(u64::from_be_bytes(r[41..].try_into().expect("root")))
                .map_err(io::Error::other)?,
            ino: word(3),
            bytenr: word(0),
            disk_num_bytes: word(4),
            start: word(1),
            end: (r[40] & 2 != 0).then(|| word(2)),
            mappable: r[40] & 1 != 0,
        },
        r[40] & 4 != 0,
    ))
}

fn read(input: &mut impl Read) -> io::Result<Option<Record>> {
    let mut r = [0; WIDTH];
    if input.read(&mut r[..1])? == 0 {
        return Ok(None);
    }
    input.read_exact(&mut r[1..])?;
    Ok(Some(r))
}

fn scratch(dir: &Path) -> io::Result<File> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = dir.join(format!(
        ".snapshot-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    fs::remove_file(path)?;
    Ok(file)
}

fn check(boundary: &mut dyn BatchBoundary) -> Result<(), WalkError> {
    match boundary.after_batch() {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(reason) => Err(WalkError::Stopped(reason)),
    }
}

pub(super) struct Sorter<'a> {
    dir: &'a Path,
    buf: Vec<Record>,
    levels: Vec<Option<File>>,
}

impl<'a> Sorter<'a> {
    pub(super) fn new(dir: &'a Path) -> Self {
        Self {
            dir,
            buf: Vec::new(),
            levels: Vec::new(),
        }
    }

    pub(super) fn push(
        &mut self,
        usage: ExtentUse,
        add: bool,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(), WalkError> {
        if self.buf.capacity() == 0 {
            reserve(boundary, (RUN * WIDTH) as u64)?;
            self.buf
                .try_reserve_exact(RUN)
                .map_err(|_| WalkError::Capacity("snapshot sort buffer"))?;
        }
        self.buf.push(encode(usage, add));
        #[cfg(test)]
        super::observe_census_memory(
            self.buf.capacity() * WIDTH
                + self.levels.capacity() * std::mem::size_of::<Option<File>>()
                + 3 * crate::spill::CHUNK,
        );
        if self.buf.len() == RUN {
            self.flush(boundary)?;
        }
        Ok(())
    }

    fn flush(&mut self, boundary: &mut dyn BatchBoundary) -> Result<(), WalkError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.buf.sort_unstable();
        let mut out = BufWriter::with_capacity(crate::spill::CHUNK, scratch(self.dir)?);
        for record in self.buf.drain(..) {
            out.write_all(&record)?;
        }
        out.flush()?;
        let mut run = out.into_inner().map_err(|e| e.into_error())?;
        run.rewind()?;
        let mut level = 0;
        while level < self.levels.len() {
            if let Some(old) = self.levels[level].take() {
                run = merge(self.dir, old, run, boundary)?;
            } else {
                self.levels[level] = Some(run);
                return check(boundary);
            }
            level += 1;
        }
        self.levels.push(Some(run));
        check(boundary)
    }

    pub(super) fn finish(mut self, boundary: &mut dyn BatchBoundary) -> Result<Reader, WalkError> {
        self.flush(boundary)?;
        let mut result = None;
        for run in self.levels.into_iter().flatten() {
            result = Some(match result {
                Some(old) => merge(self.dir, old, run, boundary)?,
                None => run,
            });
        }
        let mut input = BufReader::with_capacity(
            crate::spill::CHUNK,
            match result {
                Some(run) => run,
                None => scratch(self.dir)?,
            },
        );
        let head = read(&mut input)?;
        Ok(Reader {
            input,
            head,
            read: 0,
        })
    }
}

fn merge(
    dir: &Path,
    a: File,
    b: File,
    boundary: &mut dyn BatchBoundary,
) -> Result<File, WalkError> {
    let mut a = BufReader::with_capacity(crate::spill::CHUNK, a);
    let mut b = BufReader::with_capacity(crate::spill::CHUNK, b);
    let mut x = read(&mut a)?;
    let mut y = read(&mut b)?;
    let mut out = BufWriter::with_capacity(crate::spill::CHUNK, scratch(dir)?);
    let mut count = 0;
    while x.is_some() || y.is_some() {
        if y.is_none()
            || x.as_ref()
                .is_some_and(|x| x <= y.as_ref().expect("present"))
        {
            out.write_all(&x.take().expect("present"))?;
            x = read(&mut a)?;
        } else {
            out.write_all(&y.take().expect("present"))?;
            y = read(&mut b)?;
        }
        count += 1;
        if count % RUN == 0 {
            check(boundary)?;
        }
    }
    out.flush()?;
    let mut file = out.into_inner().map_err(|e| e.into_error())?;
    file.rewind()?;
    Ok(file)
}

pub(super) struct Group {
    pub bytenr: u64,
    pub roots: BTreeSet<usize>,
    pub uses: Vec<ExtentUse>,
}

pub(super) struct Reader {
    input: BufReader<File>,
    head: Option<Record>,
    read: usize,
}

impl Reader {
    pub(super) fn chunk(
        &mut self,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Vec<Group>, WalkError> {
        reserve(
            boundary,
            (JOIN_CHUNK * (std::mem::size_of::<ExtentUse>() + 96)) as u64,
        )?;
        let mut out = Vec::new();
        let mut count = 0;
        while let Some(record) = self.head {
            let (first, _) = decode(record)?;
            let mut group = Group {
                bytenr: first.bytenr,
                roots: BTreeSet::new(),
                uses: Vec::new(),
            };
            while let Some(record) = self.head {
                let (usage, add) = decode(record)?;
                if usage.bytenr != group.bytenr {
                    break;
                }
                group.roots.insert(usage.root);
                // Equal inode ranges of different snapshots have the same
                // union and file tally. Keep their newest root, while the
                // owner set above still retains every snapshot.
                let repeated = group.uses.last().is_some_and(|prior: &ExtentUse| {
                    prior.ino == usage.ino
                        && prior.start == usage.start
                        && prior.end == usage.end
                        && prior.disk_num_bytes == usage.disk_num_bytes
                        && prior.mappable == usage.mappable
                });
                if add && !repeated {
                    if group.uses.len() >= JOIN_CHUNK && group.uses.len() == group.uses.capacity() {
                        reserve(boundary, crate::btrfs::walk::growth_bytes(&group.uses, 1))?;
                    }
                    group.uses.push(usage);
                }
                self.head = read(&mut self.input)?;
                self.read += 1;
                if self.read % JOIN_CHUNK == 0 {
                    check(boundary)?;
                }
            }
            // Live-only extents were never candidates in the census.
            if group.uses.is_empty() {
                continue;
            }
            count += group.uses.len().max(group.roots.len());
            out.push(group);
            #[cfg(test)]
            super::observe_census_memory(
                out.iter()
                    .map(|g| {
                        g.uses.capacity() * std::mem::size_of::<ExtentUse>() + g.roots.len() * 48
                    })
                    .sum::<usize>()
                    + out.capacity() * std::mem::size_of::<Group>()
                    + crate::spill::CHUNK,
            );
            if count >= JOIN_CHUNK {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::walk::{StopReason, testing::NoStop};

    #[test]
    fn a_truncated_snapshot_spill_fails_instead_of_losing_a_reference() {
        let bytes = [0; WIDTH - 1];
        assert_eq!(
            read(&mut bytes.as_slice()).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn pausing_a_sort_leaves_no_temporary_paths() {
        struct Pause;
        impl BatchBoundary for Pause {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                ControlFlow::Break(StopReason::Paused)
            }
        }
        let dir = crate::spill::testing::scratch("snapshot-sort-pause");
        let mut sorter = Sorter::new(&dir);
        for i in 0..RUN * 3 + 1 {
            sorter
                .push(
                    ExtentUse {
                        root: 0,
                        ino: i as u64,
                        bytenr: (RUN * 4 - i) as u64,
                        disk_num_bytes: 4096,
                        start: 0,
                        end: Some(4096),
                        mappable: true,
                    },
                    true,
                    &mut NoStop,
                )
                .unwrap();
        }
        assert!(matches!(
            sorter.finish(&mut Pause),
            Err(WalkError::Stopped(StopReason::Paused))
        ));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir(dir).unwrap();
    }
}
