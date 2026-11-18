//! Strips host-identifying metadata from a buffer recording so recordings
//! from real machines (e.g. a DSM volume) can be checked in as golden data.
//!
//! Only root-tree records change, and only in place: every item keeps its
//! key, size and position, so replay, key order and item types are exactly
//! those of the original. Replaced:
//! - `ROOT_REF`/`ROOT_BACKREF` names, unless the link lives inside a tree
//!   that is itself recorded (its directory entries already carry the name),
//!   by same-length names derived from the child subvolume ID;
//! - `ROOT_ITEM` UUIDs (uuid, parent_uuid, received_uuid) by synthetic UUIDs,
//!   one per distinct original, so snapshot relations survive;
//! - `ROOT_ITEM` times and `bytes_used`, zeroed.
//!
//! Fs-tree records are left alone: golden recordings are of synthetic
//! fixture trees.

use crate::btrfs::keys::{Key, ROOT_BACKREF_KEY, ROOT_ITEM_KEY, ROOT_REF_KEY, ROOT_TREE_OBJECTID};
use crate::btrfs::parser::{
    ItemError, LEGACY_ROOT_ITEM_LEN, ROOT_ITEM_LEN, parse_root_item, parse_root_ref,
};
use crate::btrfs::recording::{RecordingError, record_spans};
use crate::btrfs::search::{BatchError, item_ranges};
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

/// `atime`..`otime` of the inode item embedded at the start of a root item.
const ROOT_INODE_TIMES: Range<usize> = 112..160;
const ROOT_BYTES_USED: Range<usize> = 192..200;
/// uuid, parent_uuid, received_uuid, right after `generation_v2`.
const ROOT_UUIDS: [usize; 3] = [
    LEGACY_ROOT_ITEM_LEN + 8,
    LEGACY_ROOT_ITEM_LEN + 24,
    LEGACY_ROOT_ITEM_LEN + 40,
];
const UUID_LEN: usize = 16;
/// ctime, otime, stime, rtime, after the four transids.
const ROOT_TIMES: Range<usize> = LEGACY_ROOT_ITEM_LEN + 88..LEGACY_ROOT_ITEM_LEN + 136;
/// dirid, sequence, name_len.
const ROOT_REF_NAME_OFFSET: usize = 18;
/// Marks synthetic UUIDs; the rest is a counter.
const SYNTHETIC_UUID_PREFIX: &[u8; 8] = b"sanitise";

#[derive(Debug, thiserror::Error)]
pub enum SanitiseError {
    #[error(transparent)]
    Recording(#[from] RecordingError),
    #[error("malformed root-tree buffer: {0}")]
    Batch(#[from] BatchError),
    #[error("root-tree item {key:?}: {source}")]
    Item { key: Key, source: ItemError },
}

/// What a sanitising pass changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SanitiseReport {
    pub names_replaced: usize,
    pub names_kept: usize,
    pub root_items: usize,
    pub distinct_uuids: usize,
}

pub fn sanitise_recording(bytes: &[u8]) -> Result<(Vec<u8>, SanitiseReport), SanitiseError> {
    let spans = record_spans(bytes)?;
    let mut sanitiser = Sanitiser {
        recorded_trees: spans
            .iter()
            .map(|span| span.range.tree_id)
            .filter(|&tree| tree != ROOT_TREE_OBJECTID)
            .collect(),
        uuids: HashMap::new(),
        report: SanitiseReport::default(),
    };
    let mut out = bytes.to_vec();
    for span in spans
        .iter()
        .filter(|s| s.range.tree_id == ROOT_TREE_OBJECTID)
    {
        let buf = &mut out[span.buf.clone()];
        for (key, data) in item_ranges(buf, span.nr_items)? {
            sanitiser
                .item(key, &mut buf[data])
                .map_err(|source| SanitiseError::Item { key, source })?;
        }
    }
    sanitiser.report.distinct_uuids = sanitiser.uuids.len();
    Ok((out, sanitiser.report))
}

struct Sanitiser {
    recorded_trees: BTreeSet<u64>,
    uuids: HashMap<[u8; UUID_LEN], [u8; UUID_LEN]>,
    report: SanitiseReport,
}

impl Sanitiser {
    fn item(&mut self, key: Key, data: &mut [u8]) -> Result<(), ItemError> {
        match key.item_type {
            ROOT_ITEM_KEY => self.root_item(data),
            ROOT_REF_KEY => self.link(key.objectid, key.offset, data),
            ROOT_BACKREF_KEY => self.link(key.offset, key.objectid, data),
            _ => Ok(()),
        }
    }

    fn root_item(&mut self, data: &mut [u8]) -> Result<(), ItemError> {
        parse_root_item(data)?;
        self.report.root_items += 1;
        data[ROOT_INODE_TIMES].fill(0);
        data[ROOT_BYTES_USED].fill(0);
        if data.len() >= ROOT_ITEM_LEN {
            for offset in ROOT_UUIDS {
                let field = &mut data[offset..offset + UUID_LEN];
                let synthetic = self.synthetic_uuid(field.try_into().expect("16-byte field"));
                field.copy_from_slice(&synthetic);
            }
            data[ROOT_TIMES].fill(0);
        }
        Ok(())
    }

    /// The nil UUID means "none" and stays nil.
    fn synthetic_uuid(&mut self, original: [u8; UUID_LEN]) -> [u8; UUID_LEN] {
        if original == [0; UUID_LEN] {
            return original;
        }
        let next = self.uuids.len() as u64 + 1;
        *self.uuids.entry(original).or_insert_with(|| {
            let mut uuid = [0; UUID_LEN];
            uuid[..8].copy_from_slice(SYNTHETIC_UUID_PREFIX);
            uuid[8..].copy_from_slice(&next.to_be_bytes());
            uuid
        })
    }

    fn link(&mut self, parent: u64, child: u64, data: &mut [u8]) -> Result<(), ItemError> {
        parse_root_ref(data)?;
        if self.recorded_trees.contains(&parent) {
            self.report.names_kept += 1;
        } else {
            synthetic_name(child, &mut data[ROOT_REF_NAME_OFFSET..]);
            self.report.names_replaced += 1;
        }
        Ok(())
    }
}

/// Fills `name` with `subvol<child>-` repeated, so equal-length names of
/// different subvolumes differ whenever the length allows.
fn synthetic_name(child: u64, name: &mut [u8]) {
    let pattern = format!("subvol{child}-");
    for (byte, fill) in name.iter_mut().zip(pattern.bytes().cycle()) {
        *byte = fill;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::keys::*;
    use crate::btrfs::parser::testing::{encode_root_item, encode_root_ref};
    use crate::btrfs::parser::{ROOT_SUBVOL_RDONLY, RootItem};
    use crate::btrfs::recording::testing::{Call, encode_recording};
    use crate::btrfs::recording::{MAGIC, ReplaySource};
    use crate::btrfs::search::SearchRange;
    use crate::btrfs::subvolume::discover;
    use crate::btrfs::walk::testing::NoStop;

    type Items = Vec<(Key, Vec<u8>)>;

    const DATA: u64 = 300;
    const NESTED: u64 = 301;
    const SNAP: u64 = 302;
    const DATA_UUID: [u8; 16] = [0xd1; 16];
    const SNAP_UUID: [u8; 16] = [0x51; 16];

    fn root_range() -> SearchRange {
        SearchRange::new(
            ROOT_TREE_OBJECTID,
            Key::new(FS_TREE_OBJECTID, 0, 0),
            Key::new(LAST_FREE_OBJECTID, u8::MAX, u64::MAX),
        )
    }

    fn fs_range(tree: u64) -> SearchRange {
        SearchRange::new(tree, Key::MIN, Key::MAX)
    }

    fn root_item(
        id: u64,
        flags: u64,
        uuid: [u8; 16],
        parent_uuid: Option<[u8; 16]>,
    ) -> (Key, Vec<u8>) {
        let root = RootItem {
            generation: 10,
            root_dirid: FIRST_FREE_OBJECTID,
            bytenr: 4096 * id,
            last_snapshot: 0,
            flags,
            refs: 1,
            uuid: Some(uuid),
            parent_uuid,
            otime: None,
        };
        (
            Key::new(id, ROOT_ITEM_KEY, 0),
            encode_root_item(&root, true),
        )
    }

    fn link(parent: u64, child: u64, name: &[u8]) -> Items {
        let data = encode_root_ref(FIRST_FREE_OBJECTID, 2, name);
        vec![
            (Key::new(parent, ROOT_REF_KEY, child), data.clone()),
            (Key::new(child, ROOT_BACKREF_KEY, parent), data),
        ]
    }

    /// data (secret share name) holding nested, and a read-only snapshot of
    /// data, all directly under the top-level tree.
    fn root_tree() -> Items {
        let mut items = vec![
            root_item(DATA, 0, DATA_UUID, None),
            root_item(NESTED, 0, [0x4e; 16], None),
            root_item(SNAP, ROOT_SUBVOL_RDONLY, SNAP_UUID, Some(DATA_UUID)),
        ];
        items.extend(link(FS_TREE_OBJECTID, DATA, b"secret-share"));
        items.extend(link(DATA, NESTED, b"nested"));
        items.extend(link(FS_TREE_OBJECTID, SNAP, b"secret-snap"));
        items.sort_by_key(|(k, _)| *k);
        items
    }

    fn fs_tree() -> Items {
        vec![(Key::new(FIRST_FREE_OBJECTID, INODE_ITEM_KEY, 0), vec![7; 4])]
    }

    /// The search calls of a whole walk: one batch, then the empty search
    /// past its last key.
    fn walk(range: SearchRange, items: Items) -> [Call; 2] {
        let last = items.last().unwrap().0;
        let rest = SearchRange::new(range.tree_id, last.successor().unwrap(), range.max);
        [(range, items), (rest, Vec::new())]
    }

    fn recording(fs_trees: &[u64]) -> Vec<u8> {
        let mut calls = walk(root_range(), root_tree()).to_vec();
        for &tree in fs_trees {
            calls.extend(walk(fs_range(tree), fs_tree()));
        }
        encode_recording(&calls)
    }

    fn sanitise(bytes: &[u8]) -> Vec<u8> {
        sanitise_recording(bytes).unwrap().0
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn subvolumes(bytes: &[u8]) -> crate::btrfs::subvolume::SubvolumeSet {
        discover(&mut ReplaySource::parse(bytes).unwrap(), &mut NoStop).unwrap()
    }

    fn name_of(bytes: &[u8], id: u64) -> Vec<u8> {
        subvolumes(bytes).get(id).unwrap().name.clone()
    }

    #[test]
    fn link_names_outside_recorded_trees_become_same_length_synthetic_names() {
        let original = recording(&[]);
        let clean = sanitise(&original);
        assert_eq!(clean.len(), original.len());
        assert!(!contains(&clean, b"secret"));
        let data = name_of(&clean, DATA);
        assert_eq!(data.len(), b"secret-share".len());
        assert!(data.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-'));
    }

    #[test]
    fn ref_and_backref_of_one_link_get_the_same_name() {
        let clean = sanitise(&recording(&[]));
        let renamed = name_of(&clean, DATA);
        let root_ref = encode_root_ref(FIRST_FREE_OBJECTID, 2, &renamed);
        assert_eq!(
            clean
                .windows(root_ref.len())
                .filter(|w| *w == &root_ref[..])
                .count(),
            2
        );
    }

    #[test]
    fn link_names_inside_a_recorded_tree_are_kept() {
        let clean = sanitise(&recording(&[DATA]));
        assert_eq!(name_of(&clean, NESTED), b"nested");
        assert!(!contains(&clean, b"secret"));
        let (_, report) = sanitise_recording(&recording(&[DATA])).unwrap();
        assert_eq!((report.names_replaced, report.names_kept), (4, 2));
    }

    #[test]
    fn uuids_are_replaced_consistently() {
        let clean = sanitise(&recording(&[]));
        assert!(!contains(&clean, &DATA_UUID));
        assert!(!contains(&clean, &SNAP_UUID));
        let set = subvolumes(&clean);
        let data = set.get(DATA).unwrap();
        let snap = set.get(SNAP).unwrap();
        assert!(data.uuid.is_some());
        assert_eq!(snap.parent_uuid, data.uuid);
        assert_ne!(snap.uuid, data.uuid);
        assert_eq!(data.parent_uuid, None);
        let (_, report) = sanitise_recording(&recording(&[])).unwrap();
        assert_eq!((report.root_items, report.distinct_uuids), (3, 3));
    }

    #[test]
    fn root_item_times_and_usage_are_zeroed() {
        let (key, mut item) = root_item(DATA, 0, DATA_UUID, None);
        let marked = [112..160, 192..200, 327..375];
        for range in marked.clone() {
            item[range].fill(0xab);
        }
        let clean = sanitise(&encode_recording(&[(root_range(), vec![(key, item)])]));
        assert!(!contains(&clean, &[0xab; 8]));
    }

    #[test]
    fn subvolume_structure_survives_sanitising() {
        let original = recording(&[DATA]);
        let shape = |bytes: &[u8]| {
            let set = subvolumes(bytes);
            let rows: Vec<_> = set
                .subvolumes
                .iter()
                .map(|s| (s.id, s.parent, s.readonly, s.pending_deletion, s.generation))
                .collect();
            (rows, set.quota_enabled)
        };
        assert_eq!(shape(&sanitise(&original)), shape(&original));
    }

    #[test]
    fn only_root_tree_bytes_change_and_output_is_deterministic() {
        let original = recording(&[DATA]);
        let clean = sanitise(&original);
        let fs_call = encode_recording(&walk(fs_range(DATA), fs_tree()));
        let fs_bytes = &fs_call[MAGIC.len()..];
        assert!(original.ends_with(fs_bytes));
        assert!(clean.ends_with(fs_bytes));
        assert_eq!(sanitise(&original), clean);
        assert_eq!(sanitise(&clean), clean);
    }

    #[test]
    fn rejects_malformed_root_tree_items() {
        let bad = vec![(Key::new(DATA, ROOT_BACKREF_KEY, 5), vec![0; 5])];
        let bytes = encode_recording(&[(root_range(), bad)]);
        assert!(matches!(
            sanitise_recording(&bytes),
            Err(SanitiseError::Item { .. })
        ));
    }
}
