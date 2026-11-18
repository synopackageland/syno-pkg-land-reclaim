//! Namespace reconstruction for one subvolume tree: `DIR_INDEX` entries
//! joined to `INODE_ITEM`s (SPEC 4.1), held in compact arenas (SPEC 9):
//! inodes sorted by objectid, entries grouped by parent directory in tree
//! key order, and all names in one byte table.

use crate::btrfs::keys::*;
use crate::btrfs::parser::{
    is_inline_extent, parse_dir_index, parse_file_extent, parse_inode_item,
};
use crate::btrfs::search::{Item, SearchRange};
use crate::btrfs::source::MetadataSource;
use crate::btrfs::subvolume::{LeafSharing, Link, SubvolumeSet};
use crate::btrfs::walk::{
    BatchBoundary, WalkError, WalkStats, growth_bytes, try_grow, walk_batches,
};
use crate::codec::{Decoder, push_varint};
use crate::model::extent::{ExtentRecord, ExtentUse, FileExtentRef, classify};
use crate::spill::{Appender, ChunkReader};
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::ops::ControlFlow;
use std::path::PathBuf;

fn checkpoint_replace(path: &std::path::Path, value: &impl serde::Serialize) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let mut out = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut out, value).map_err(io::Error::other)?;
    out.flush()?;
    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    std::fs::rename(tmp, path)?;
    std::fs::File::open(path.parent().expect("checkpoint directory"))?.sync_all()
}

fn checkpoint_hash(value: &impl serde::Serialize) -> io::Result<u64> {
    struct Hash(crate::codec::Fnv);
    impl std::io::Write for Hash {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut hash = Hash(crate::codec::Fnv::new());
    serde_json::to_writer(&mut hash, value).map_err(io::Error::other)?;
    Ok(hash.0.value())
}

fn checkpoint_file_hash(path: &std::path::Path) -> io::Result<u64> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = crate::codec::Fnv::new();
    let mut bytes = [0; crate::spill::CHUNK];
    loop {
        let len = file.read(&mut bytes)?;
        if len == 0 {
            break;
        }
        hash.update(&bytes[..len]);
    }
    Ok(hash.value())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedNamespace {
    version: u32,
    key: crate::units::UnitKey,
    next: Option<Key>,
    namespace: Namespace,
    refs_bytes: u64,
    refs_checksum: u64,
    checksum: u64,
}

impl SavedNamespace {
    fn load(checkpoint: &std::path::Path, refs: &std::path::Path) -> Result<Self, WalkError> {
        let saved: Self =
            serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(checkpoint)?))
                .map_err(|_| WalkError::Checkpoint("damaged namespace checkpoint".into()))?;
        if saved.version != 1 || saved.namespace.tree_id != saved.key.subvolume {
            return Err(WalkError::Checkpoint(
                "namespace checkpoint format mismatch".into(),
            ));
        }
        if checkpoint_hash(&(
            saved.version,
            &saved.key,
            saved.next,
            &saved.namespace,
            saved.refs_bytes,
            saved.refs_checksum,
        ))? != saved.checksum
        {
            return Err(WalkError::Checkpoint(
                "namespace checkpoint checksum mismatch".into(),
            ));
        }
        if saved
            .namespace
            .spill
            .as_ref()
            .is_none_or(|spill| spill.out.path() != refs)
            || std::fs::metadata(refs).map(|m| m.len()).ok() != Some(saved.refs_bytes)
            || checkpoint_file_hash(refs).ok() != Some(saved.refs_checksum)
        {
            return Err(WalkError::Checkpoint(
                "the partial extent stream is missing or incomplete".into(),
            ));
        }
        Ok(saved)
    }
}

/// What a directory entry points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryTarget {
    Inode(u64),
    /// A nested subvolume/snapshot root: a boundary, not a child.
    Subvolume(u64),
    Other(Key),
}

/// The `INODE_ITEM` fields the namespace keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Inode {
    pub objectid: u64,
    /// Logical size (`st_size`).
    pub size: u64,
    pub mode: u32,
    pub nlink: u32,
    pub ctime: u64,
}

/// The change time of one inode below a Recycle Bin path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecycleEntryCtime {
    pub tree: u64,
    pub inode: u64,
    pub ctime: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum TargetKind {
    Inode,
    Subvolume,
    /// The location key is kept in `Namespace::other_locations`.
    Other,
}

/// One `DIR_INDEX` entry; its name lives in `Namespace::names`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct DirEntry {
    parent: u64,
    target: u64,
    name_offset: u32,
    name_len: u16,
    kind: TargetKind,
}

/// Item counts observed while reading one tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TreeStats {
    pub inode_items: u64,
    pub dir_index_items: u64,
    pub extent_data_items: u64,
    /// `EXTENT_DATA` items that are holes (no allocation).
    pub holes: u64,
    /// Item types outside the known set, e.g. Synology-private type 163.
    pub unknown_types: BTreeMap<u8, u64>,
    pub walk: WalkStats,
}

#[derive(Debug, thiserror::Error)]
pub enum NamespaceError {
    #[error("inode {0} has no INODE_ITEM")]
    MissingInode(u64),
    #[error("inode {0} is not a directory")]
    NotADirectory(u64),
    #[error("entry {name:?} in directory {parent} points to missing inode {ino}", name = String::from_utf8_lossy(name))]
    DanglingEntry {
        parent: u64,
        name: Vec<u8>,
        ino: u64,
    },
    #[error("entry in directory {parent} points to unsupported location {location:?}")]
    UnsupportedLocation { parent: u64, location: Key },
    #[error("directory {0} is reachable twice")]
    Cycle(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    Fifo,
    Socket,
    CharDevice,
    BlockDevice,
    Unknown,
}

impl FileKind {
    pub fn from_mode(mode: u32) -> Self {
        match mode & 0o170000 {
            0o100000 => FileKind::Regular,
            0o040000 => FileKind::Directory,
            0o120000 => FileKind::Symlink,
            0o010000 => FileKind::Fifo,
            0o140000 => FileKind::Socket,
            0o020000 => FileKind::CharDevice,
            0o060000 => FileKind::BlockDevice,
            _ => FileKind::Unknown,
        }
    }

    /// The `find -printf %y` letter.
    pub fn letter(self) -> char {
        match self {
            FileKind::Regular => 'f',
            FileKind::Directory => 'd',
            FileKind::Symlink => 'l',
            FileKind::Fifo => 'p',
            FileKind::Socket => 's',
            FileKind::CharDevice => 'c',
            FileKind::BlockDevice => 'b',
            FileKind::Unknown => 'U',
        }
    }

    pub fn from_letter(letter: char) -> Option<Self> {
        Some(match letter {
            'f' => FileKind::Regular,
            'd' => FileKind::Directory,
            'l' => FileKind::Symlink,
            'p' => FileKind::Fifo,
            's' => FileKind::Socket,
            'c' => FileKind::CharDevice,
            'b' => FileKind::BlockDevice,
            'U' => FileKind::Unknown,
            _ => return None,
        })
    }

    /// The name used in JSON output.
    pub fn name(self) -> &'static str {
        match self {
            FileKind::Regular => "file",
            FileKind::Directory => "dir",
            FileKind::Symlink => "symlink",
            FileKind::Fifo => "fifo",
            FileKind::Socket => "socket",
            FileKind::CharDevice => "char-device",
            FileKind::BlockDevice => "block-device",
            FileKind::Unknown => "unknown",
        }
    }
}

/// One reconstructed path below a listing root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingEntry {
    pub path: Vec<u8>,
    pub kind: FileKind,
    pub ino: u64,
    /// Logical size; `None` for directories and subvolume boundaries.
    pub size: Option<u64>,
}

/// Sizes of a namespace's parts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelCounts {
    pub entries: u64,
    pub name_bytes: u64,
    pub inodes: u64,
    pub extents: u64,
    pub recycle_entries: u64,
}

/// Directory entries and inodes of one subvolume tree.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Namespace {
    tree_id: u64,
    /// Sorted by objectid.
    inodes: Vec<Inode>,
    /// Sorted by parent directory, as `DIR_INDEX` keys are.
    entries: Vec<DirEntry>,
    names: Vec<u8>,
    other_locations: BTreeMap<usize, Key>,
    /// Sorted by inode, then file offset, as `EXTENT_DATA` keys are.
    extents: Vec<FileExtentRef>,
    /// Inline data bytes per inode, sorted by inode.
    inline: Vec<(u64, u64)>,
    /// Where physical extent refs go instead of `extents`, if anywhere.
    spill: Option<RefSpill>,
    stats: TreeStats,
}

/// A physical file extent ref a spilling read wrote out: its inode and
/// its use, whose scope inode is not known yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpilledRef {
    pub objectid: u64,
    pub extent: ExtentUse,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RefSpill {
    out: Appender,
    last: u64,
}

impl RefSpill {
    fn put(&mut self, r: &SpilledRef) -> io::Result<()> {
        let out = self.out.buf();
        push_varint(out, r.objectid.wrapping_sub(self.last));
        self.last = r.objectid;
        let e = &r.extent;
        push_varint(out, e.bytenr);
        push_varint(out, u64::from(e.start));
        push_varint(out, u64::from(e.end));
        out.push(e.flags);
        push_varint(out, e.disk_bytes);
        self.out.commit()
    }
}

/// Reads back the refs a spilling read wrote, in tree order.
pub struct SpilledRefs {
    reader: ChunkReader,
    last: u64,
}

impl SpilledRefs {
    pub fn new(path: PathBuf) -> Self {
        SpilledRefs {
            reader: ChunkReader::new(path),
            last: 0,
        }
    }

    pub fn read(&mut self) -> io::Result<Option<SpilledRef>> {
        let last = &mut self.last;
        self.reader.next(|d| get_ref(d, last))
    }
}

fn get_ref(d: &mut Decoder<'_>, last: &mut u64) -> Option<SpilledRef> {
    let objectid = last.wrapping_add(d.varint()?);
    *last = objectid;
    let bytenr = d.varint()?;
    let start = u32::try_from(d.varint()?).ok()?;
    let end = u32::try_from(d.varint()?).ok()?;
    let flags = d.byte()?;
    let disk_bytes = d.varint()?;
    Some(SpilledRef {
        objectid,
        extent: ExtentUse {
            bytenr,
            inode: 0,
            start,
            end,
            flags,
            disk_bytes,
        },
    })
}

impl Namespace {
    /// Reads every item of subvolume tree `tree_id`.
    pub fn read<S: MetadataSource + ?Sized>(
        source: &mut S,
        tree_id: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Namespace, WalkError> {
        Self::read_shared(source, tree_id, LeafSharing::default(), boundary)
    }

    /// Reads tree `tree_id`, flagging file extents in leaves `sharing`
    /// says a snapshot may share.
    pub fn read_shared<S: MetadataSource + ?Sized>(
        source: &mut S,
        tree_id: u64,
        sharing: LeafSharing,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<Namespace, WalkError> {
        Self::read_with(source, tree_id, sharing, boundary, None)
    }

    /// Verify a retained partial unit before deciding to reuse or reread it.
    pub(crate) fn checkpoint_key(
        checkpoint: &std::path::Path,
    ) -> Result<crate::units::UnitKey, WalkError> {
        Ok(SavedNamespace::load(checkpoint, &checkpoint.with_extension("refs"))?.key)
    }

    /// A safe pause retains the partial namespace and its flushed extent
    /// stream. The next process starts at the first key not yet consumed.
    pub(crate) fn read_checkpointed<S: MetadataSource + ?Sized>(
        source: &mut S,
        tree_id: u64,
        sharing: LeafSharing,
        boundary: &mut dyn BatchBoundary,
        refs: PathBuf,
        checkpoint: &std::path::Path,
        key: &crate::units::UnitKey,
    ) -> Result<Self, WalkError> {
        let active = checkpoint.with_extension("active");
        if active.exists() && !checkpoint.exists() {
            return Err(WalkError::Checkpoint(
                "the previous process did not finish saving its namespace".into(),
            ));
        }
        if !active.exists() {
            checkpoint_replace(&active, key)?;
        }
        let saved = if checkpoint.exists() {
            let saved = SavedNamespace::load(checkpoint, &refs)?;
            let active_key: crate::units::UnitKey =
                serde_json::from_slice(&std::fs::read(&active)?).map_err(|_| {
                    WalkError::Checkpoint("damaged namespace checkpoint identity".into())
                })?;
            if active_key != saved.key
                || saved.key.subvolume != tree_id
                || saved.key.uuid != key.uuid
            {
                return Err(WalkError::Checkpoint(
                    "the subvolume identity changed".into(),
                ));
            }
            Some(saved)
        } else {
            None
        };
        let (mut ns, mut next) = if let Some(saved) = saved.filter(|saved| saved.key == *key) {
            // Restoring saved work does not count as new metadata I/O.
            boundary.restored(&saved.namespace.stats.walk);
            (saved.namespace, saved.next)
        } else {
            // find-new has no deletion tombstones. Until a complete namespace
            // delta join is available, reread this unit instead of mixing epochs.
            checkpoint_replace(&active, key)?;
            let ns = Namespace {
                tree_id,
                spill: Some(RefSpill {
                    out: Appender::new(refs.clone()),
                    last: 0,
                }),
                ..Namespace::default()
            };
            std::fs::File::create(&refs)?;
            (ns, Some(Key::MIN))
        };
        let walked = if let Some(min) = next {
            let range = SearchRange::new(tree_id, min, Key::MAX);
            walk_batches(source, range, boundary, |items, boundary| {
                ns.make_room(items, boundary)?;
                for &item in items {
                    ns.add(item, &sharing)?;
                }
                ns.stats
                    .walk
                    .add(&WalkStats::batch(items.len() as u32, items));
                next = items.last().and_then(|item| item.key.successor());
                Ok(())
            })
            .map(|_| ())
        } else {
            Ok(())
        };
        match walked {
            Err(error @ WalkError::Stopped(reason))
                if reason != crate::btrfs::walk::StopReason::Cancelled =>
            {
                let spill = ns.spill.as_mut().expect("spilling namespace");
                spill.out.flush()?;
                std::fs::File::open(&refs)?.sync_all()?;
                let refs_bytes = std::fs::metadata(&refs)?.len();
                let refs_checksum = checkpoint_file_hash(&refs)?;
                let checksum = checkpoint_hash(&(1u32, key, next, &ns, refs_bytes, refs_checksum))?;
                let saved = SavedNamespace {
                    version: 1,
                    key: key.clone(),
                    next,
                    namespace: ns,
                    refs_bytes,
                    refs_checksum,
                    checksum,
                };
                checkpoint_replace(checkpoint, &saved).map_err(|_| {
                    WalkError::Checkpoint("the pause position could not be saved".into())
                })?;
                Err(error)
            }
            Err(error) => Err(error),
            Ok(()) => {
                if let Some(mut spill) = ns.spill.take() {
                    spill.out.flush()?;
                }
                if checkpoint.exists() {
                    std::fs::remove_file(checkpoint)?;
                }
                Ok(ns)
            }
        }
    }

    /// Like `read_shared`, but writes physical extent refs to `refs`
    /// instead of holding them (SPEC 9.1).
    pub fn read_spilling<S: MetadataSource + ?Sized>(
        source: &mut S,
        tree_id: u64,
        sharing: LeafSharing,
        boundary: &mut dyn BatchBoundary,
        refs: PathBuf,
    ) -> Result<Namespace, WalkError> {
        let spill = RefSpill {
            out: Appender::new(refs),
            last: 0,
        };
        Self::read_with(source, tree_id, sharing, boundary, Some(spill))
    }

    /// Reads just the selected inode ranges when every root is a file.
    /// Directories and hard-linked files retain the whole-tree walk.
    /// Link counts and all selected file extents are still read;
    /// external holders are resolved from the extent tree by the caller.
    pub fn read_file_roots_spilling<S: MetadataSource + ?Sized>(
        source: &mut S,
        tree_id: u64,
        sharing: LeafSharing,
        boundary: &mut dyn BatchBoundary,
        refs: PathBuf,
        roots: impl ExactSizeIterator<Item = u64>,
    ) -> Result<Namespace, WalkError> {
        let mut inodes = Vec::new();
        if let ControlFlow::Break(reason) = boundary.reserve(growth_bytes(&inodes, roots.len())) {
            return Err(WalkError::Stopped(reason));
        }
        try_grow(&mut inodes, roots.len())?;
        inodes.extend(roots);
        inodes.sort_unstable();
        inodes.dedup();
        if inodes.contains(&FIRST_FREE_OBJECTID) {
            return Self::read_spilling(source, tree_id, sharing, boundary, refs);
        }
        let mut ns = Namespace {
            tree_id,
            ..Namespace::default()
        };
        for &ino in &inodes {
            let key = Key::new(ino, INODE_ITEM_KEY, 0);
            ns.read_range(
                source,
                SearchRange::new(tree_id, key, key),
                &sharing,
                boundary,
            )?;
        }
        if ns
            .inodes
            .iter()
            .any(|i| FileKind::from_mode(i.mode) == FileKind::Directory || i.nlink > 1)
        {
            return Self::read_spilling(source, tree_id, sharing, boundary, refs);
        }
        ns.spill = Some(RefSpill {
            out: Appender::new(refs),
            last: 0,
        });
        // Sorted, distinct inode ranges preserve the spill's tree-key order.
        for ino in inodes {
            let min = Key::new(ino, INODE_ITEM_KEY, 0)
                .successor()
                .expect("inode key has a successor");
            ns.read_range(
                source,
                SearchRange::new(tree_id, min, Key::new(ino, u8::MAX, u64::MAX)),
                &sharing,
                boundary,
            )?;
        }
        if let Some(mut spill) = ns.spill.take() {
            spill.out.flush()?;
        }
        Ok(ns)
    }

    fn read_with<S: MetadataSource + ?Sized>(
        source: &mut S,
        tree_id: u64,
        sharing: LeafSharing,
        boundary: &mut dyn BatchBoundary,
        spill: Option<RefSpill>,
    ) -> Result<Namespace, WalkError> {
        let mut ns = Namespace {
            tree_id,
            spill,
            ..Namespace::default()
        };
        let range = SearchRange::new(tree_id, Key::MIN, Key::MAX);
        ns.read_range(source, range, &sharing, boundary)?;
        if let Some(mut spill) = ns.spill.take() {
            spill.out.flush()?;
        }
        Ok(ns)
    }

    fn read_range<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        range: SearchRange,
        sharing: &LeafSharing,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(), WalkError> {
        let stats = walk_batches(source, range, boundary, |items, boundary| {
            self.make_room(items, boundary)?;
            items.iter().try_for_each(|&item| self.add(item, sharing))
        })?;
        self.stats.walk.add(&stats);
        Ok(())
    }

    /// Charges the growth `items` may cause, then allocates it: the model
    /// never grows past the budget unchecked, nor aborts when memory runs
    /// out (SPEC 22.4).
    fn make_room(
        &mut self,
        items: &[Item<'_>],
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(), WalkError> {
        let count = |wanted: fn(&Item<'_>) -> bool| items.iter().filter(|i| wanted(i)).count();
        let inodes = count(|i| i.key.item_type == INODE_ITEM_KEY);
        let entries = count(|i| i.key.item_type == DIR_INDEX_KEY);
        let extents = match self.spill {
            Some(_) => 0,
            None => count(|i| i.key.item_type == EXTENT_DATA_KEY),
        };
        let inline = count(|i| i.key.item_type == EXTENT_DATA_KEY && is_inline_extent(i.data));
        // A DIR_INDEX name is shorter than its item.
        let names = items
            .iter()
            .filter(|i| i.key.item_type == DIR_INDEX_KEY)
            .map(|i| i.data.len())
            .sum();
        let bytes = [
            growth_bytes(&self.inodes, inodes),
            growth_bytes(&self.entries, entries),
            growth_bytes(&self.names, names),
            growth_bytes(&self.extents, extents),
            growth_bytes(&self.inline, inline),
        ];
        if let ControlFlow::Break(reason) = boundary.reserve(bytes.iter().sum()) {
            return Err(WalkError::Stopped(reason));
        }
        try_grow(&mut self.inodes, inodes)?;
        try_grow(&mut self.entries, entries)?;
        try_grow(&mut self.names, names)?;
        try_grow(&mut self.extents, extents)?;
        try_grow(&mut self.inline, inline)
    }

    pub fn tree_id(&self) -> u64 {
        self.tree_id
    }

    /// Name and target of every entry of directory `dir`, in index order.
    pub fn child_entries(&self, dir: u64) -> impl Iterator<Item = (&[u8], EntryTarget)> {
        self.children(dir)
            .map(|index| (self.name(&self.entries[index]), self.target(index)))
    }

    pub fn stats(&self) -> &TreeStats {
        &self.stats
    }

    /// How many entries, name bytes, inodes and file extents it holds.
    pub fn counts(&self) -> ModelCounts {
        let recycle_entries = self
            .subtree_counts_with_recycle(FIRST_FREE_OBJECTID, false)
            .recycle_entries;
        ModelCounts {
            entries: self.entries.len() as u64,
            name_bytes: self.names.len() as u64,
            inodes: self.inodes.len() as u64,
            extents: self.extents.len() as u64,
            recycle_entries,
        }
    }

    /// Sizes of the parts below inode `root`, itself included: the
    /// entries of every directory reached without crossing into a nested
    /// subvolume, and the distinct inodes they name with their extents.
    pub fn subtree_counts(&self, root: u64) -> ModelCounts {
        self.subtree_counts_with_recycle(root, false)
    }

    /// Sizes below `root`, where its path already lies inside a Recycle Bin.
    pub fn subtree_counts_with_recycle(&self, root: u64, root_is_recycled: bool) -> ModelCounts {
        let mut counts = ModelCounts::default();
        let mut seen = HashSet::new();
        let mut stack = vec![(root, root_is_recycled)];
        counts.recycle_entries = u64::from(root_is_recycled);
        while let Some((ino, in_recycle)) = stack.pop() {
            let Some(inode) = self.inode(ino).filter(|_| seen.insert(ino)) else {
                continue;
            };
            counts.inodes += 1;
            counts.extents += self.extents(ino).len() as u64;
            if FileKind::from_mode(inode.mode) != FileKind::Directory {
                continue;
            }
            for index in self.children(ino) {
                counts.entries += 1;
                counts.name_bytes += u64::from(self.entries[index].name_len);
                let name = self.name(&self.entries[index]);
                let recycled = in_recycle || name == b"#recycle";
                if let EntryTarget::Inode(child) = self.target(index) {
                    if recycled {
                        counts.recycle_entries += 1;
                    }
                    stack.push((child, recycled));
                }
            }
        }
        counts
    }

    fn add(&mut self, item: Item<'_>, sharing: &LeafSharing) -> Result<(), WalkError> {
        let key = item.key;
        let invalid = |source| WalkError::Item { key, source };
        match key.item_type {
            INODE_ITEM_KEY => {
                let inode = parse_inode_item(item.data).map_err(invalid)?;
                self.insert_inode(Inode {
                    objectid: key.objectid,
                    size: inode.size,
                    mode: inode.mode,
                    nlink: inode.nlink,
                    ctime: inode.ctime.sec,
                });
                self.stats.inode_items += 1;
            }
            DIR_INDEX_KEY => {
                let entry = parse_dir_index(item.data).map_err(invalid)?;
                self.push_entry(key, entry.name, entry.location)?;
                self.stats.dir_index_items += 1;
            }
            EXTENT_DATA_KEY => {
                let extent = parse_file_extent(item.data).map_err(invalid)?;
                let mut record = classify(key.objectid, key.offset, &extent);
                if let ExtentRecord::Physical(r) = &mut record {
                    r.shared_leaf = sharing.may_share(item.transid);
                }
                self.add_extent(key.objectid, record)?;
                self.stats.extent_data_items += 1;
            }
            t if is_known_fs_tree_type(t) => {}
            t => *self.stats.unknown_types.entry(t).or_default() += 1,
        }
        Ok(())
    }

    /// Tree walks return keys in order, so this normally appends.
    fn insert_inode(&mut self, inode: Inode) {
        match self.inodes.last() {
            Some(last) if last.objectid >= inode.objectid => {
                match self
                    .inodes
                    .binary_search_by_key(&inode.objectid, |i| i.objectid)
                {
                    Ok(pos) => self.inodes[pos] = inode,
                    Err(pos) => self.inodes.insert(pos, inode),
                }
            }
            _ => self.inodes.push(inode),
        }
    }

    fn push_entry(&mut self, key: Key, name: &[u8], location: Key) -> Result<(), WalkError> {
        let name_offset = u32::try_from(self.names.len())
            .map_err(|_| WalkError::Capacity("name table exceeds 4 GiB"))?;
        let (kind, target) = match entry_target(location) {
            EntryTarget::Inode(ino) => (TargetKind::Inode, ino),
            EntryTarget::Subvolume(id) => (TargetKind::Subvolume, id),
            EntryTarget::Other(location) => {
                self.other_locations.insert(self.entries.len(), location);
                (TargetKind::Other, 0)
            }
        };
        self.names.extend_from_slice(name);
        self.entries.push(DirEntry {
            parent: key.objectid,
            target,
            name_offset,
            // DIR_INDEX names are at most u16::MAX bytes by format.
            name_len: name.len() as u16,
            kind,
        });
        Ok(())
    }

    fn add_extent(&mut self, inode: u64, record: ExtentRecord) -> io::Result<()> {
        match record {
            ExtentRecord::Physical(r) => match &mut self.spill {
                Some(spill) => spill.put(&SpilledRef {
                    objectid: inode,
                    extent: ExtentUse::new(&r, 0),
                })?,
                None => self.extents.push(r),
            },
            ExtentRecord::Hole { .. } => self.stats.holes += 1,
            ExtentRecord::Inline { bytes } => match self.inline.last_mut() {
                Some((ino, total)) if *ino == inode => *total += bytes,
                _ => self.inline.push((inode, bytes)),
            },
        }
        Ok(())
    }

    /// Physical extent references of inode `objectid`, in file order.
    pub fn extents(&self, objectid: u64) -> &[FileExtentRef] {
        let start = self.extents.partition_point(|r| r.inode < objectid);
        let end = self.extents.partition_point(|r| r.inode <= objectid);
        &self.extents[start..end]
    }

    /// Bytes of data stored inline in inode `objectid`'s metadata.
    pub fn inline_bytes(&self, objectid: u64) -> u64 {
        self.inline
            .binary_search_by_key(&objectid, |(ino, _)| *ino)
            .map_or(0, |pos| self.inline[pos].1)
    }

    pub fn inode(&self, objectid: u64) -> Option<&Inode> {
        self.inodes
            .binary_search_by_key(&objectid, |i| i.objectid)
            .ok()
            .map(|pos| &self.inodes[pos])
    }

    fn name(&self, entry: &DirEntry) -> &[u8] {
        let start = entry.name_offset as usize;
        &self.names[start..start + usize::from(entry.name_len)]
    }

    fn target(&self, index: usize) -> EntryTarget {
        let entry = &self.entries[index];
        match entry.kind {
            TargetKind::Inode => EntryTarget::Inode(entry.target),
            TargetKind::Subvolume => EntryTarget::Subvolume(entry.target),
            TargetKind::Other => EntryTarget::Other(self.other_locations[&index]),
        }
    }

    /// Indexes of the entries of directory `dir`, in index order.
    fn children(&self, dir: u64) -> std::ops::Range<usize> {
        let start = self.entries.partition_point(|e| e.parent < dir);
        let end = self.entries.partition_point(|e| e.parent <= dir);
        start..end
    }

    /// All paths below directory inode `root`, sorted bytewise.
    /// `subvolumes` tells linked subvolume boundaries from the empty
    /// placeholders that snapshots keep for nested subvolumes.
    pub fn listing(
        &self,
        root: u64,
        subvolumes: &SubvolumeSet,
    ) -> Result<Vec<ListingEntry>, NamespaceError> {
        self.expect_directory(root)?;
        let mut out = Vec::new();
        let mut visited = HashSet::from([root]);
        let mut pending = vec![(root, Vec::new())];
        while let Some((dir, prefix)) = pending.pop() {
            for index in self.children(dir) {
                let listed = self.list_entry(index, &prefix, subvolumes)?;
                let boundary = matches!(self.target(index), EntryTarget::Subvolume(_));
                if listed.kind == FileKind::Directory && !boundary {
                    if !visited.insert(listed.ino) {
                        return Err(NamespaceError::Cycle(listed.ino));
                    }
                    pending.push((listed.ino, listed.path.clone()));
                }
                out.push(listed);
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn expect_directory(&self, ino: u64) -> Result<(), NamespaceError> {
        let inode = self.inode(ino).ok_or(NamespaceError::MissingInode(ino))?;
        if FileKind::from_mode(inode.mode) != FileKind::Directory {
            return Err(NamespaceError::NotADirectory(ino));
        }
        Ok(())
    }

    fn list_entry(
        &self,
        index: usize,
        prefix: &[u8],
        subvolumes: &SubvolumeSet,
    ) -> Result<ListingEntry, NamespaceError> {
        let entry = &self.entries[index];
        let name = self.name(entry);
        let path = join_path(prefix, name);
        match self.target(index) {
            EntryTarget::Inode(ino) => {
                let inode = self
                    .inode(ino)
                    .ok_or_else(|| NamespaceError::DanglingEntry {
                        parent: entry.parent,
                        name: name.to_vec(),
                        ino,
                    })?;
                let kind = FileKind::from_mode(inode.mode);
                let size = (kind != FileKind::Directory).then_some(inode.size);
                Ok(ListingEntry {
                    path,
                    kind,
                    ino,
                    size,
                })
            }
            EntryTarget::Subvolume(id) => Ok(ListingEntry {
                path,
                kind: FileKind::Directory,
                ino: if self.links_subvolume(entry.parent, name, id, subvolumes) {
                    FIRST_FREE_OBJECTID
                } else {
                    EMPTY_SUBVOL_DIR_OBJECTID
                },
                size: None,
            }),
            EntryTarget::Other(location) => Err(NamespaceError::UnsupportedLocation {
                parent: entry.parent,
                location,
            }),
        }
    }

    /// Whether subvolume `id` is linked as `name` in directory `dirid` of
    /// this tree, i.e. its ROOT_BACKREF names this tree, directory and name.
    pub fn links_subvolume(
        &self,
        dirid: u64,
        name: &[u8],
        id: u64,
        subvolumes: &SubvolumeSet,
    ) -> bool {
        let here = Link {
            parent_id: self.tree_id,
            dirid,
        };
        subvolumes
            .get(id)
            .is_some_and(|s| s.parent == Some(here) && s.name == name)
    }
}

fn entry_target(location: Key) -> EntryTarget {
    match location.item_type {
        INODE_ITEM_KEY => EntryTarget::Inode(location.objectid),
        ROOT_ITEM_KEY => EntryTarget::Subvolume(location.objectid),
        _ => EntryTarget::Other(location),
    }
}

fn join_path(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = Vec::with_capacity(prefix.len() + 1 + name.len());
    if !prefix.is_empty() {
        path.extend_from_slice(prefix);
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

/// Renders entries as `path<TAB>type<TAB>ino<TAB>size` lines, matching
/// `find -printf '%P\t%y\t%i\t%s\n'` with `-` as the size of directories.
/// Control bytes and backslashes in paths are escaped as `\xNN`.
pub fn format_listing(entries: &[ListingEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        escape_path(&e.path, &mut out);
        let size = e.size.map_or_else(|| "-".to_string(), |s| s.to_string());
        out.extend(format!("\t{}\t{}\t{}\n", e.kind.letter(), e.ino, size).bytes());
    }
    out
}

fn escape_path(path: &[u8], out: &mut Vec<u8>) {
    for &b in path {
        if b < 0x20 || b == 0x7f || b == b'\\' {
            out.extend(format!("\\x{b:02x}").bytes());
        } else {
            out.push(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::btrfs::parser::DiskExtent;
    use crate::btrfs::parser::testing::{encode_extent_header, encode_regular_extent};
    use crate::btrfs::subvolume::SubvolumeSet;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};
    use crate::btrfs::walk::{StopReason, WalkError};
    use crate::model::testing::*;
    use crate::spill::testing::scratch;

    const TREE: u64 = 257;

    #[test]
    fn kinds_round_trip_through_letters_and_have_json_names() {
        for kind in [
            FileKind::Regular,
            FileKind::Directory,
            FileKind::Symlink,
            FileKind::Fifo,
            FileKind::Socket,
            FileKind::CharDevice,
            FileKind::BlockDevice,
            FileKind::Unknown,
        ] {
            assert_eq!(FileKind::from_letter(kind.letter()), Some(kind));
        }
        assert_eq!(FileKind::from_letter('x'), None);
        assert_eq!(FileKind::Regular.name(), "file");
        assert_eq!(FileKind::Directory.name(), "dir");
    }

    fn read(items: Vec<TreeItem>) -> Result<Namespace, WalkError> {
        read_tree(TREE, items)
    }

    fn listing_text(ns: &Namespace, root: u64) -> String {
        listing_text_with(ns, root, &SubvolumeSet::default())
    }

    fn listing_text_with(ns: &Namespace, root: u64, subvolumes: &SubvolumeSet) -> String {
        String::from_utf8(format_listing(&ns.listing(root, subvolumes).unwrap())).unwrap()
    }

    /// Subvolume 300 linked into TREE's directory `dirid` as `name`.
    fn linked_subvolume(dirid: u64, name: &str) -> SubvolumeSet {
        SubvolumeSet {
            subvolumes: vec![linked(300, TREE, dirid, name)],
            ..SubvolumeSet::default()
        }
    }

    fn sample_tree() -> Vec<TreeItem> {
        vec![
            inode(256, DIR, 12, 1),
            entry(256, 2, "docs", 257),
            entry(256, 3, "café.txt", 258),
            entry(256, 4, "link", 259),
            inode(257, DIR, 6, 1),
            entry(257, 2, "b.bin", 260),
            inode(258, FILE, 5, 1),
            inode(259, LINK, 8, 1),
            inode(260, FILE, 4096, 1),
        ]
    }

    #[test]
    fn reconstructs_sorted_paths_with_types_inodes_and_sizes() {
        let ns = read(sample_tree()).unwrap();
        assert_eq!(
            listing_text(&ns, FIRST_FREE_OBJECTID),
            "café.txt\tf\t258\t5\n\
             docs\td\t257\t-\n\
             docs/b.bin\tf\t260\t4096\n\
             link\tl\t259\t8\n"
        );
    }

    #[test]
    fn retains_inode_ctime_for_later_recycle_bin_queries() {
        let ctime = crate::btrfs::parser::BtrfsTimespec {
            sec: 1_790_000_123,
            nsec: 456_789_012,
        };
        let ns = read(vec![inode_with_ctime(256, DIR, 0, 1, ctime)]).unwrap();

        assert_eq!(ns.inode(256).unwrap().ctime, ctime.sec);
    }

    #[test]
    fn a_subtree_counts_only_what_lies_below_its_root() {
        let mut items = sample_tree();
        items.push(whole_extent(260, 0, 1 << 20, 4096));
        items.push(whole_extent(258, 0, 2 << 20, 4096));
        let ns = read(items).unwrap();
        let docs = ns.subtree_counts(257);
        assert_eq!(
            docs,
            ModelCounts {
                entries: 1,
                name_bytes: "b.bin".len() as u64,
                inodes: 2,
                extents: 1,
                recycle_entries: 0,
            }
        );
        let file = ns.subtree_counts(258);
        assert_eq!((file.entries, file.inodes, file.extents), (0, 1, 1));
        let whole = ns.counts();
        let all = ns.subtree_counts(FIRST_FREE_OBJECTID);
        assert_eq!((all.entries, all.extents), (whole.entries, whole.extents));
        assert_eq!(all.inodes, whole.inodes);
    }

    #[test]
    fn subtree_budget_counts_recycle_entries_only_with_scope_context() {
        let ns = read(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "share", 257),
            inode(257, DIR, 0, 1),
            entry(257, 2, "#recycle", 258),
            inode(258, DIR, 0, 1),
            entry(258, 2, "old", 259),
            inode(259, FILE, 1, 1),
        ])
        .unwrap();

        assert_eq!(ns.subtree_counts(256).recycle_entries, 2);
        assert_eq!(ns.subtree_counts_with_recycle(258, true).recycle_entries, 2);
        assert_eq!(ns.subtree_counts(258).recycle_entries, 0);
    }

    #[test]
    fn caseless_index_items_are_never_names_or_entries() {
        let plain = read(sample_tree()).unwrap();
        let mut items = sample_tree();
        items.insert(1, caseless(256, 7, "DOCS", 257));
        items.insert(2, caseless(256, 9, "CAFÉ.TXT", 258));
        let ns = read(items).unwrap();
        assert_eq!(
            listing_text(&ns, FIRST_FREE_OBJECTID),
            listing_text(&plain, FIRST_FREE_OBJECTID)
        );
        assert_eq!(ns.stats().dir_index_items, plain.stats().dir_index_items);
        assert_eq!(ns.stats().unknown_types, BTreeMap::from([(91, 2)]));
    }

    #[test]
    fn lists_a_subtree_relative_to_its_root() {
        let ns = read(sample_tree()).unwrap();
        assert_eq!(listing_text(&ns, 257), "b.bin\tf\t260\t4096\n");
    }

    #[test]
    fn hard_links_appear_under_every_name() {
        let ns = read(vec![
            inode(256, DIR, 4, 1),
            entry(256, 2, "a", 257),
            entry(256, 3, "b", 257),
            inode(257, FILE, 1, 2),
        ])
        .unwrap();
        assert_eq!(listing_text(&ns, 256), "a\tf\t257\t1\nb\tf\t257\t1\n");
    }

    #[test]
    fn nested_subvolumes_are_boundaries_not_children() {
        let ns = read(vec![
            inode(256, DIR, 6, 1),
            subvolume_entry(256, 2, "sub", 300),
        ])
        .unwrap();
        let linked = linked_subvolume(256, "sub");
        assert_eq!(listing_text_with(&ns, 256, &linked), "sub\td\t256\t-\n");
    }

    #[test]
    fn unlinked_subvolume_entries_are_empty_placeholders() {
        // In a snapshot, nested subvolumes are not carried over; the kernel
        // shows an empty directory with inode 2 instead.
        let ns = read(vec![
            inode(256, DIR, 6, 1),
            subvolume_entry(256, 2, "sub", 300),
        ])
        .unwrap();
        for subvolumes in [
            SubvolumeSet::default(),
            linked_subvolume(999, "sub"),
            linked_subvolume(256, "other"),
        ] {
            assert_eq!(listing_text_with(&ns, 256, &subvolumes), "sub\td\t2\t-\n");
        }
    }

    #[test]
    fn entry_without_inode_is_an_error() {
        let ns = read(vec![inode(256, DIR, 2, 1), entry(256, 2, "x", 999)]).unwrap();
        assert!(matches!(
            ns.listing(256, &SubvolumeSet::default()),
            Err(NamespaceError::DanglingEntry { ino: 999, .. })
        ));
    }

    #[test]
    fn listing_root_must_be_a_directory_inode() {
        let ns = read(sample_tree()).unwrap();
        assert!(matches!(
            ns.listing(258, &SubvolumeSet::default()),
            Err(NamespaceError::NotADirectory(258))
        ));
        assert!(matches!(
            ns.listing(4242, &SubvolumeSet::default()),
            Err(NamespaceError::MissingInode(4242))
        ));
    }

    #[test]
    fn control_bytes_in_names_are_escaped() {
        let ns = read(vec![
            inode(256, DIR, 2, 1),
            entry(256, 2, "a\nb\\", 257),
            inode(257, FILE, 0, 1),
        ])
        .unwrap();
        assert_eq!(listing_text(&ns, 256), "a\\x0ab\\x5c\tf\t257\t0\n");
    }

    #[test]
    fn counts_item_kinds_and_unknown_types() {
        let extent = DiskExtent {
            disk_bytenr: 1 << 20,
            disk_num_bytes: 4096,
            offset: 0,
            num_bytes: 4096,
        };
        let mut items = sample_tree();
        items.push((
            Key::new(260, EXTENT_DATA_KEY, 0),
            encode_regular_extent(&extent),
        ));
        items.push((Key::new(260, 163, 0), vec![1, 2, 3]));
        items.push((Key::new(260, XATTR_ITEM_KEY, 0), vec![0; 40]));
        let ns = read(items).unwrap();
        let stats = ns.stats();
        assert_eq!(stats.inode_items, 5);
        assert_eq!(stats.dir_index_items, 4);
        assert_eq!(stats.extent_data_items, 1);
        assert_eq!(stats.unknown_types.get(&163), Some(&1));
        assert_eq!(stats.unknown_types.len(), 1);
    }

    #[test]
    fn keeps_physical_extent_refs_per_inode_in_file_order() {
        let a = DiskExtent {
            disk_bytenr: 1 << 20,
            disk_num_bytes: 4096,
            offset: 0,
            num_bytes: 4096,
        };
        let b = DiskExtent {
            disk_bytenr: 2 << 20,
            ..a
        };
        let mut items = sample_tree();
        items.push(extent_item(260, 4096, REGULAR, b));
        items.push(extent_item(260, 0, PREALLOC, a));
        items.push(extent_item(258, 0, REGULAR, a));
        let ns = read(items).unwrap();
        let refs = ns.extents(260);
        assert_eq!(refs.len(), 2);
        assert_eq!((refs[0].file_offset, refs[0].bytenr), (0, 1 << 20));
        assert!(refs[0].prealloc);
        assert_eq!((refs[1].file_offset, refs[1].bytenr), (4096, 2 << 20));
        assert_eq!(ns.extents(258).len(), 1);
        assert!(ns.extents(257).is_empty());
    }

    #[test]
    fn extents_in_leaves_a_snapshot_may_share_are_flagged() {
        let mut items = sample_tree();
        items.push(whole_extent(260, 0, 1 << 20, 4096));
        items.push(whole_extent(260, 4096, 2 << 20, 4096));
        let mut source = FakeSource::new(items, 3);
        source.leaf_generation = |key| if key.offset == 0 { 40 } else { 41 };
        let sharing = LeafSharing {
            up_to_generation: 40,
            with_snapshot: true,
        };
        let ns = Namespace::read_shared(&mut source, TREE, sharing, &mut NoStop).unwrap();
        let flags: Vec<bool> = ns.extents(260).iter().map(|r| r.shared_leaf).collect();
        assert_eq!(flags, vec![true, false]);
        assert!(read(sample_tree()).unwrap().extents(260).is_empty());
    }

    #[test]
    fn holes_and_inline_data_are_not_physical_extents() {
        let hole = DiskExtent {
            disk_bytenr: 0,
            disk_num_bytes: 0,
            offset: 0,
            num_bytes: 8192,
        };
        let mut inline = encode_extent_header(0);
        inline.extend(b"hello");
        let mut items = sample_tree();
        items.push(extent_item(260, 0, REGULAR, hole));
        items.push((Key::new(258, EXTENT_DATA_KEY, 0), inline));
        let ns = read(items).unwrap();
        assert!(ns.extents(260).is_empty());
        assert!(ns.extents(258).is_empty());
        assert_eq!(ns.inline_bytes(258), 5);
        assert_eq!(ns.inline_bytes(260), 0);
        assert_eq!(ns.stats().holes, 1);
    }

    /// Logs reservations (`Some`) and batch ends (`None`); refuses any
    /// reservation above `allowance`.
    struct Charges {
        allowance: u64,
        log: Vec<Option<u64>>,
    }

    impl Charges {
        fn new(allowance: u64) -> Self {
            Charges {
                allowance,
                log: Vec::new(),
            }
        }
    }

    impl BatchBoundary for Charges {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            self.log.push(None);
            ControlFlow::Continue(())
        }

        fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
            self.log.push(Some(bytes));
            if bytes > self.allowance {
                return ControlFlow::Break(StopReason::ResourceLimit);
            }
            ControlFlow::Continue(())
        }
    }

    #[test]
    fn each_batch_is_charged_before_the_model_takes_it() {
        let mut roomy = Charges::new(u64::MAX);
        let mut source = FakeSource::new(sample_tree(), 100);
        let ns = Namespace::read(&mut source, TREE, &mut roomy).unwrap();
        assert!(
            matches!(roomy.log[..], [Some(_), None, ..]),
            "{:?}",
            roomy.log
        );
        let c = ns.counts();
        let held = c.inodes * size_of::<Inode>() as u64
            + c.entries * size_of::<DirEntry>() as u64
            + c.name_bytes;
        let charged: u64 = roomy.log.iter().flatten().sum();
        assert!(charged >= held, "charged {charged} for {held} held bytes");

        let mut tight = Charges::new(charged - 1);
        let mut source = FakeSource::new(sample_tree(), 100);
        let err = Namespace::read(&mut source, TREE, &mut tight).unwrap_err();
        assert!(matches!(err, WalkError::Stopped(StopReason::ResourceLimit)));
        assert!(
            !tight.log.contains(&None),
            "stopped before taking the batch"
        );
    }

    #[test]
    fn damaged_or_replaced_partial_checkpoint_never_restarts_the_tree() {
        use crate::units::UnitKey;
        struct Pause;
        impl BatchBoundary for Pause {
            fn after_batch(&mut self) -> ControlFlow<crate::btrfs::walk::StopReason> {
                ControlFlow::Break(crate::btrfs::walk::StopReason::Paused)
            }
        }
        for damage in 0..4 {
            let dir = crate::spill::testing::scratch("invalid-namespace-resume");
            let mut key = UnitKey {
                subvolume: TREE,
                uuid: String::new(),
                generation: 10,
                readonly: false,
                slot: 1,
                inputs: 1,
            };
            let checkpoint = dir.join("namespace.json");
            let refs = dir.join("namespace.refs");
            let mut source = FakeSource::new(sample_tree(), 2);
            Namespace::read_checkpointed(
                &mut source,
                TREE,
                LeafSharing::default(),
                &mut Pause,
                refs.clone(),
                &checkpoint,
                &key,
            )
            .unwrap_err();
            match damage {
                0 => std::fs::write(&checkpoint, b"{}").unwrap(),
                1 => key.uuid = "replaced-subvolume".into(),
                2 => {
                    let mut saved: serde_json::Value =
                        serde_json::from_reader(std::fs::File::open(&checkpoint).unwrap()).unwrap();
                    saved["next"]["objectid"] = serde_json::json!(9999);
                    serde_json::to_writer(std::fs::File::create(&checkpoint).unwrap(), &saved)
                        .unwrap();
                }
                _ => std::fs::write(&refs, b"corrupt extent stream").unwrap(),
            }
            let calls = source.calls.len();
            let error = Namespace::read_checkpointed(
                &mut source,
                TREE,
                LeafSharing::default(),
                &mut NoStop,
                refs,
                &checkpoint,
                &key,
            )
            .unwrap_err();
            assert!(matches!(error, WalkError::Checkpoint(_)));
            assert_eq!(
                source.calls.len(),
                calls,
                "an invalid checkpoint must not start a fresh read"
            );
        }
    }

    #[test]
    fn paused_namespace_resumes_after_the_saved_key_without_rereading() {
        use crate::units::UnitKey;
        struct Pause;
        impl BatchBoundary for Pause {
            fn after_batch(&mut self) -> ControlFlow<crate::btrfs::walk::StopReason> {
                ControlFlow::Break(crate::btrfs::walk::StopReason::Paused)
            }
        }
        let dir = crate::spill::testing::scratch("namespace-resume");
        let key = UnitKey {
            subvolume: TREE,
            uuid: String::new(),
            generation: 10,
            readonly: false,
            slot: 1,
            inputs: 1,
        };
        let checkpoint = dir.join("namespace.json");
        let refs = dir.join("namespace.refs");
        let mut source = FakeSource::new(sample_tree(), 2);
        let error = Namespace::read_checkpointed(
            &mut source,
            TREE,
            LeafSharing::default(),
            &mut Pause,
            refs.clone(),
            &checkpoint,
            &key,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            WalkError::Stopped(crate::btrfs::walk::StopReason::Paused)
        ));
        let before = source.calls.len();
        let saved_max = source.items[1].0;
        let ns = Namespace::read_checkpointed(
            &mut source,
            TREE,
            LeafSharing::default(),
            &mut NoStop,
            refs,
            &checkpoint,
            &key,
        )
        .unwrap();
        assert_eq!(source.calls[before].min, saved_max.successor().unwrap());
        let expected = Namespace::read_shared(
            &mut FakeSource::new(sample_tree(), 2),
            TREE,
            LeafSharing::default(),
            &mut NoStop,
        )
        .unwrap();
        assert_eq!(ns.inodes, expected.inodes);
        assert_eq!(ns.entries, expected.entries);
        assert_eq!(ns.names, expected.names);
        assert_eq!(ns.stats, expected.stats);
    }

    #[test]
    fn a_spilling_read_writes_extent_refs_out_instead_of_holding_them() {
        let mut items = sample_tree();
        items.push(whole_extent(260, 4096, 2 << 20, 4096));
        items.push(whole_extent(258, 0, 1 << 20, 4096));
        items.push(whole_extent(260, 0, 3 << 20, 8192));
        let held = read(items.clone()).unwrap();
        let path = scratch("refs").join("refs");
        let mut source = FakeSource::new(items, 3);
        let sharing = LeafSharing::default();
        let ns = Namespace::read_spilling(&mut source, TREE, sharing, &mut NoStop, path.clone())
            .unwrap();
        assert_eq!(ns.counts().extents, 0);
        assert!(ns.extents(260).is_empty());
        assert_eq!(ns.stats(), held.stats());
        let mut refs = SpilledRefs::new(path);
        let mut spilled = Vec::new();
        while let Some(r) = refs.read().unwrap() {
            spilled.push(r);
        }
        let expected: Vec<SpilledRef> = [258, 260]
            .into_iter()
            .flat_map(|objectid| {
                let refs = held.extents(objectid).iter();
                refs.map(move |r| SpilledRef {
                    objectid,
                    extent: ExtentUse::new(r, 0),
                })
            })
            .collect();
        assert_eq!(spilled, expected);
    }

    #[test]
    fn file_roots_spill_all_extents_once_in_inode_order() {
        let mut items = sample_tree();
        items.push(whole_extent(260, 4096, 2 << 20, 4096));
        items.push(whole_extent(258, 0, 1 << 20, 4096));
        items.push(whole_extent(260, 0, 3 << 20, 8192));
        let held = read(items.clone()).unwrap();
        let path = scratch("file-refs").join("refs");
        let mut source = FakeSource::new(items, 1);
        let ns = Namespace::read_file_roots_spilling(
            &mut source,
            TREE,
            LeafSharing::default(),
            &mut NoStop,
            path.clone(),
            [260, 258, 260].into_iter(),
        )
        .unwrap();
        assert_eq!(ns.stats().inode_items, 2);
        assert_eq!(ns.stats().extent_data_items, 3);
        assert_eq!(ns.stats().dir_index_items, 0);
        for ino in [258, 260] {
            assert_eq!(ns.inode(ino), held.inode(ino));
        }
        let mut refs = SpilledRefs::new(path);
        let mut spilled = Vec::new();
        while let Some(r) = refs.read().unwrap() {
            spilled.push(r);
        }
        let expected: Vec<SpilledRef> = [258, 260]
            .into_iter()
            .flat_map(|objectid| {
                held.extents(objectid).iter().map(move |r| SpilledRef {
                    objectid,
                    extent: ExtentUse::new(r, 0),
                })
            })
            .collect();
        assert_eq!(spilled, expected);
    }

    #[test]
    fn a_directory_root_keeps_the_full_namespace() {
        let mut source = FakeSource::new(sample_tree(), 1);
        let ns = Namespace::read_file_roots_spilling(
            &mut source,
            TREE,
            LeafSharing::default(),
            &mut NoStop,
            scratch("dir-refs").join("refs"),
            [257, 258].into_iter(),
        )
        .unwrap();
        assert_eq!(
            listing_text(&ns, 256),
            listing_text(&read(sample_tree()).unwrap(), 256)
        );
    }

    #[test]
    fn file_root_work_is_stopped_before_exceeding_the_memory_budget() {
        let mut source = FakeSource::new(sample_tree(), 1);
        let error = Namespace::read_file_roots_spilling(
            &mut source,
            TREE,
            LeafSharing::default(),
            &mut Charges::new(0),
            scratch("budget-refs").join("refs"),
            [258].into_iter(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            WalkError::Stopped(StopReason::ResourceLimit)
        ));
        assert!(source.calls.is_empty());
    }

    #[test]
    fn malformed_known_items_fail_the_read() {
        let mut items = sample_tree();
        items.push((Key::new(260, EXTENT_DATA_KEY, 0), vec![0; 7]));
        let err = read(items).unwrap_err();
        assert!(matches!(err, WalkError::Item { key, .. } if key.item_type == EXTENT_DATA_KEY));
    }
}
