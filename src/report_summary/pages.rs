//! Compact page figures; the source classifiers and snapshot algebra stay authoritative.
use crate::codec::{Decoder, HashingWriter};
use crate::query::Entry;
use crate::recycle::RecycleBin;
use crate::report::{NodeRecord, decode_record, encode_record, hex_bytes};
use serde::{Deserialize, Serialize};
use std::io;

pub use crate::snapshots::answer::SnapshotFigures;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bin {
    #[serde(with = "hex_bytes")]
    share: Vec<u8>,
    #[serde(with = "hex_bytes")]
    entry: Vec<u8>,
}

impl Bin {
    pub fn new(bin: &RecycleBin) -> io::Result<Self> {
        Ok(Self {
            share: bin.share.clone(),
            entry: encode_entry(&bin.entry)?,
        })
    }

    pub fn get(&self) -> Option<RecycleBin> {
        let entry = decode_entry(&self.entry)?;
        Some(RecycleBin {
            share: self.share.clone(),
            path: entry.path.clone(),
            entry,
        })
    }
}

pub fn encode_entry(entry: &Entry) -> io::Result<Vec<u8>> {
    let record = NodeRecord {
        depth: 0,
        name: &entry.path,
        kind: entry.kind,
        flags: entry.flags,
        metrics: entry.metrics,
        blockers: entry.blockers,
        held: entry.held,
        marker: None,
        outside: entry.outside.as_deref().copied(),
        nlink: entry.nlink,
        tree: entry.tree,
        objectid: entry.objectid,
    };
    let mut bytes = Vec::new();
    encode_record(&mut HashingWriter::new(&mut bytes), &record)?;
    Ok(bytes)
}

pub fn decode_entry(bytes: &[u8]) -> Option<Entry> {
    let mut decoder = Decoder::new(bytes, 0);
    let record = decode_record(&mut decoder)?;
    (record.depth == 0 && decoder.at() == bytes.len()).then(|| Entry::new(record.name, &record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::accounting::{Blockers, Held};
    use crate::model::namespace::FileKind;
    use crate::model::tree::{Metrics, NodeFlags};

    #[test]
    fn stored_entries_keep_lossless_paths_identity_flags_and_retained_values() {
        let record = NodeRecord {
            depth: 0,
            name: b"/v/a/#recycle/\xff",
            kind: FileKind::Directory,
            flags: NodeFlags::PACKAGE,
            metrics: Metrics {
                logical: 19,
                physical: 12,
                reclaimable: 7,
                unknown: 2,
                ..Metrics::default()
            },
            blockers: Blockers::SNAPSHOT,
            held: Held([1, 2, 3, 4, 5, 6]),
            marker: None,
            outside: None,
            nlink: 2,
            tree: 257,
            objectid: 300,
        };
        let entry = Entry::new(record.name, &record);
        let bytes = encode_entry(&entry).unwrap();
        assert_eq!(decode_entry(&bytes), Some(entry));
        let mut bad = bytes;
        bad.push(0);
        assert!(decode_entry(&bad).is_none());
    }
}
