//! Recorded TREE_SEARCH_V2 buffers for golden tests (SPEC 18 layer 1).
//!
//! Format, all integers little-endian:
//! `MAGIC`, then per search call: tree_id u64, min key, max key (each
//! objectid u64, type u8, offset u64), nr_items u32, byte length u32, and
//! the used bytes of the result buffer.

use crate::btrfs::bytes::{LeReader, Truncated};
use crate::btrfs::keys::Key;
use crate::btrfs::parser::read_disk_key;
use crate::btrfs::search::{SearchRange, batch_len};
use crate::btrfs::source::{MetadataSource, Probe, RawBatch, SourceError, SpaceSnapshot};
use std::io::{self, Write};
use std::ops::Range;

pub const MAGIC: &[u8; 8] = b"SRBUF01\n";
/// Upper bound on one recorded buffer: the kernel's TREE_SEARCH_V2 cap.
const MAX_RECORD_LEN: u32 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("not a syno-reclaim buffer recording")]
    BadMagic,
    #[error("recording truncated: needed {} bytes, {} available", .0.needed, .0.available)]
    Truncated(Truncated),
    #[error("recorded buffer of {len} bytes exceeds the TREE_SEARCH_V2 cap")]
    Oversized { len: u32 },
}

impl From<Truncated> for RecordingError {
    fn from(t: Truncated) -> Self {
        RecordingError::Truncated(t)
    }
}

/// Wraps a source and appends every search call it serves to `writer`.
pub struct RecordingSource<S, W> {
    inner: S,
    writer: W,
}

impl<S: MetadataSource, W: Write> RecordingSource<S, W> {
    pub fn new(inner: S, mut writer: W) -> io::Result<Self> {
        writer.write_all(MAGIC)?;
        Ok(RecordingSource { inner, writer })
    }

    pub fn into_writer(self) -> W {
        self.writer
    }
}

impl<S: MetadataSource, W: Write> MetadataSource for RecordingSource<S, W> {
    fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
        let batch = self.inner.search(range)?;
        // A malformed batch is recorded whole so replay reproduces the error.
        let used = batch_len(batch.buf, batch.nr_items).unwrap_or(batch.buf.len());
        let batch = RawBatch {
            nr_items: batch.nr_items,
            buf: &batch.buf[..used],
        };
        write_record(&mut self.writer, range, batch)?;
        Ok(batch)
    }

    fn maintenance_probes(&mut self) -> Vec<Probe> {
        self.inner.maintenance_probes()
    }

    fn space_snapshot(&mut self) -> Option<SpaceSnapshot> {
        self.inner.space_snapshot()
    }
}

fn write_record(w: &mut impl Write, range: &SearchRange, batch: RawBatch<'_>) -> io::Result<()> {
    let len = u32::try_from(batch.buf.len()).map_err(io::Error::other)?;
    w.write_all(&range.tree_id.to_le_bytes())?;
    write_key(w, range.min)?;
    write_key(w, range.max)?;
    w.write_all(&batch.nr_items.to_le_bytes())?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(batch.buf)
}

fn write_key(w: &mut impl Write, key: Key) -> io::Result<()> {
    w.write_all(&key.objectid.to_le_bytes())?;
    w.write_all(&[key.item_type])?;
    w.write_all(&key.offset.to_le_bytes())
}

struct Record<'a> {
    range: SearchRange,
    batch: RawBatch<'a>,
}

/// Where one recorded search call sits in a recording: its request and the
/// byte range of its result buffer.
pub(crate) struct RecordSpan {
    pub range: SearchRange,
    pub nr_items: u32,
    pub buf: Range<usize>,
}

/// Splits a recording into its search calls without interpreting the buffers.
pub(crate) fn record_spans(bytes: &[u8]) -> Result<Vec<RecordSpan>, RecordingError> {
    let mut r = LeReader::new(bytes);
    if r.take(MAGIC.len()).ok() != Some(&MAGIC[..]) {
        return Err(RecordingError::BadMagic);
    }
    let mut spans = Vec::new();
    while r.remaining() > 0 {
        spans.push(read_record(&mut r)?);
    }
    Ok(spans)
}

/// Serves recorded search calls in order, refusing any request that differs
/// from the recorded one.
pub struct ReplaySource<'a> {
    records: Vec<Record<'a>>,
    next: usize,
}

impl<'a> ReplaySource<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RecordingError> {
        let records = record_spans(bytes)?
            .into_iter()
            .map(|span| Record {
                range: span.range,
                batch: RawBatch {
                    nr_items: span.nr_items,
                    buf: &bytes[span.buf],
                },
            })
            .collect();
        Ok(ReplaySource { records, next: 0 })
    }

    /// Recorded calls not yet served.
    pub fn remaining(&self) -> usize {
        self.records.len() - self.next
    }
}

fn read_record(r: &mut LeReader<'_>) -> Result<RecordSpan, RecordingError> {
    let tree_id = r.u64()?;
    let min = read_disk_key(r)?;
    let max = read_disk_key(r)?;
    let nr_items = r.u32()?;
    let len = r.u32()?;
    if len > MAX_RECORD_LEN {
        return Err(RecordingError::Oversized { len });
    }
    let start = r.position();
    r.skip(len as usize)?;
    Ok(RecordSpan {
        range: SearchRange::new(tree_id, min, max),
        nr_items,
        buf: start..r.position(),
    })
}

impl MetadataSource for ReplaySource<'_> {
    fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError> {
        let index = self.next;
        let record = self
            .records
            .get(index)
            .ok_or(SourceError::ReplayExhausted { index })?;
        if record.range != *range {
            return Err(SourceError::ReplayMismatch {
                index,
                requested: *range,
            });
        }
        self.next += 1;
        Ok(record.batch)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::{MAGIC, write_record};
    use crate::btrfs::keys::Key;
    use crate::btrfs::search::SearchRange;
    use crate::btrfs::search::testing::encode_item;
    use crate::btrfs::source::RawBatch;

    /// One search call: its request and the items it returned.
    pub type Call = (SearchRange, Vec<(Key, Vec<u8>)>);

    /// A recording of `calls`, in order.
    pub fn encode_recording(calls: &[Call]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        for (range, items) in calls {
            let buf: Vec<u8> = items
                .iter()
                .flat_map(|(key, data)| encode_item(1, *key, data))
                .collect();
            let batch = RawBatch {
                nr_items: items.len() as u32,
                buf: &buf,
            };
            write_record(&mut out, range, batch).unwrap();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::keys::Key;
    use crate::btrfs::search::SearchRange;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};
    use crate::btrfs::walk::walk_tree;

    fn fake() -> FakeSource {
        let items = (0..5u64)
            .map(|i| (Key::new(256 + i, 1, 0), vec![i as u8; 3]))
            .collect();
        FakeSource::new(items, 2)
    }

    fn range() -> SearchRange {
        SearchRange::new(5, Key::MIN, Key::MAX)
    }

    fn collect<S: MetadataSource>(source: &mut S) -> Vec<(Key, Vec<u8>)> {
        let mut out = Vec::new();
        walk_tree(source, range(), &mut NoStop, |item| {
            out.push((item.key, item.data.to_vec()));
            Ok(())
        })
        .unwrap();
        out
    }

    fn record_fake() -> Vec<u8> {
        let mut recorder = RecordingSource::new(fake(), Vec::new()).unwrap();
        collect(&mut recorder);
        recorder.into_writer()
    }

    #[test]
    fn replay_reproduces_the_recorded_walk() {
        let expected = collect(&mut fake());
        let bytes = record_fake();
        let mut replay = ReplaySource::parse(&bytes).unwrap();
        assert_eq!(collect(&mut replay), expected);
        assert_eq!(replay.remaining(), 0);
    }

    #[test]
    fn replay_rejects_a_different_request() {
        let bytes = record_fake();
        let mut replay = ReplaySource::parse(&bytes).unwrap();
        let other = SearchRange::new(1, Key::MIN, Key::MAX);
        assert!(matches!(
            replay.search(&other),
            Err(SourceError::ReplayMismatch { index: 0, .. })
        ));
    }

    #[test]
    fn replay_reports_exhaustion() {
        let bytes = record_fake();
        let mut replay = ReplaySource::parse(&bytes).unwrap();
        collect(&mut replay);
        assert!(matches!(
            replay.search(&range()),
            Err(SourceError::ReplayExhausted { index: 4 })
        ));
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(matches!(
            ReplaySource::parse(b"NOTABUF!"),
            Err(RecordingError::BadMagic)
        ));
    }

    #[test]
    fn rejects_truncated_records() {
        let bytes = record_fake();
        for cut in [MAGIC.len() + 1, bytes.len() - 1] {
            assert!(matches!(
                ReplaySource::parse(&bytes[..cut]),
                Err(RecordingError::Truncated(_))
            ));
        }
    }

    #[test]
    fn rejects_records_larger_than_the_kernel_buffer_cap() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend([0u8; 8 + 17 + 17 + 4]);
        bytes.extend(u32::MAX.to_le_bytes());
        assert!(matches!(
            ReplaySource::parse(&bytes),
            Err(RecordingError::Oversized { len: u32::MAX })
        ));
    }
}
