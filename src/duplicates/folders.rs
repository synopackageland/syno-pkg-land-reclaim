//! Identical folders (docs/duplicates.md "Identical folders"), for folders
//! of small files as much as large ones.
//!
//! 1. **Fingerprint** from the report, which already holds every entry's
//!    name, kind and size: per folder, SHA-256 over its entries in name
//!    order (kind, name, size; a subfolder's own fingerprint), leaving out
//!    regenerated system files ([`is_junk`]). A folder is *complete* only
//!    when nothing below it was left out of the scan or excluded.
//! 2. **Candidates**: complete folders of at least 10 MiB with the same
//!    fingerprint in two or more places; only the topmost of nested ones.
//! 3. **Confirmation**, candidates only: every file's inode and extents
//!    are read again in objectid-range searches; inline data (small files,
//!    symlinks) is hashed from the extent item itself, other data by its
//!    sector checksums read in 1 MiB-coalesced spans. No file data is read.
//!    A folder with a file lacking checksums is `unchecked`.
//! 4. **Accounting**: copies sharing extents form clusters, judged like
//!    file clusters.
//! 5. **Partly identical folders** ([`super::partial`]): pairs of folders
//!    whose identical items, at the same relative paths, cover most of
//!    their bytes.

use super::analysis::{DupRoots, Nodes, excluded, hex};
use super::csum::{self, Layout};
use super::model::{Analysis, Cluster, FolderGroup, FolderMember, LIST_MIN_SIZE};
use crate::btrfs::extent_refs::{ExtentRefs, Plan, resolve};
use crate::btrfs::keys::{EXTENT_DATA_KEY, INODE_ITEM_KEY, Key};
use crate::btrfs::parser::{ExtentKind, parse_file_extent, parse_inode_item};
use crate::btrfs::search::SearchRange;
use crate::btrfs::source::MetadataSource;
use crate::btrfs::subvolume::SubvolumeSet;
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_tree};
use crate::model::accounting::{Blockers, Holder, Verdict, classify};
use crate::model::namespace::FileKind;
use crate::model::tree::{MAX_DEPTH, NodeFlags};
use crate::sha256::Sha256;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::ops::ControlFlow;

/// Regenerated system files: they never make two folders differ, and
/// they go with a folder that is deleted. User dotfiles (`.git`, `.env`)
/// are content and count.
pub const JUNK_NAMES: [&[u8]; 4] = [b".DS_Store", b"Thumbs.db", b"desktop.ini", b"@eaDir"];
/// AppleDouble resource forks: `._name`.
pub const JUNK_PREFIX: &[u8] = b"._";

pub fn is_junk(name: &[u8]) -> bool {
    JUNK_NAMES.contains(&name) || (name.starts_with(JUNK_PREFIX) && name.len() > JUNK_PREFIX.len())
}

/// Files confirmed per analysis at most; candidates beyond are unchecked.
pub const MAX_FILES: usize = 2_000_000;
/// Same-fingerprint folders of at least this size seed the search for
/// partly identical folders (docs/duplicates.md).
const SEED_MIN: u64 = 1 << 20;
/// Members of one same-content set paired as seeds, at most.
const SEED_MEMBERS: usize = 16;
/// Objectids closer than this share one inode search.
const INODE_GAP: u64 = 256;
/// `BTRFS_INODE_NODATASUM`.
const INODE_NODATASUM: u64 = 1;

/// One folder as the fingerprint walk leaves it.
#[derive(Debug, Clone)]
struct Folder {
    path: Vec<u8>,
    tree: u64,
    objectid: u64,
    files: u64,
    bytes: u64,
}

/// A folder of at least [`LIST_MIN_SIZE`] as the fingerprint walk saw
/// it: only complete folders that are not excluded can be partly
/// identical.
#[derive(Debug, Clone, Copy)]
pub(super) struct Big {
    pub(super) complete: bool,
    pub(super) excluded: bool,
}

struct Frame {
    depth: u32,
    folder: Folder,
    hash: Sha256,
    complete: bool,
    /// The folder itself is DSM, package or app data.
    excluded: bool,
}

/// What a report node does to the folder above it.
fn incomplete(flags: NodeFlags, name: &[u8], depth: u32) -> bool {
    let unread = NodeFlags::SUBVOLUME_ROOT
        .with(NodeFlags::SNAPSHOT)
        .with(NodeFlags::PLACEHOLDER)
        .with(NodeFlags::ENCRYPTED)
        .with(NodeFlags::OWN_STATE)
        .with(NodeFlags::SYSTEM)
        .with(NodeFlags::PACKAGE);
    flags.bits() & unread.bits() != 0
        || name.first() == Some(&b'@')
        || depth as usize + 1 >= MAX_DEPTH
}

/// What the fingerprint walk finds.
struct Fingerprints {
    /// Sets of candidate identical folders with one fingerprint, topmost.
    sets: Vec<Vec<Folder>>,
    /// Same-fingerprint folders with one name in different parents, of at
    /// least [`SEED_MIN`]: where partly identical folders may be.
    seeds: Vec<(Vec<u8>, Vec<u8>)>,
    big: HashMap<Vec<u8>, Big>,
}

/// Steps 1 and 2: sets of candidate folders with one fingerprint.
fn candidates(
    nodes: &mut dyn Nodes,
    mount: &[u8],
    uncertain: &HashSet<u64>,
) -> io::Result<Fingerprints> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut found: HashMap<[u8; 32], Vec<Folder>> = HashMap::new();
    let mut big: HashMap<Vec<u8>, Big> = HashMap::new();
    // A junk folder's subtree is skipped down to this depth.
    let mut skip_below: Option<u32> = None;
    let close = |frame: Frame,
                 stack: &mut Vec<Frame>,
                 found: &mut HashMap<[u8; 32], Vec<Folder>>,
                 big: &mut HashMap<Vec<u8>, Big>| {
        let digest = frame.hash.finish();
        let name = last_name(&frame.folder.path).to_vec();
        if let Some(parent) = stack.last_mut() {
            parent.hash.update(b"d");
            put_name(&mut parent.hash, &name);
            parent.hash.update(&digest);
            parent.complete &= frame.complete;
            parent.folder.files += frame.folder.files;
            parent.folder.bytes += frame.folder.bytes;
        }
        if frame.folder.bytes >= LIST_MIN_SIZE {
            big.insert(
                frame.folder.path.clone(),
                Big {
                    complete: frame.complete,
                    excluded: frame.excluded,
                },
            );
        }
        if frame.complete
            && !frame.excluded
            && frame.folder.files > 0
            && frame.folder.bytes >= SEED_MIN
        {
            found.entry(digest).or_default().push(frame.folder);
        }
    };
    nodes.walk(&mut |path, record| {
        if skip_below.is_some_and(|d| record.depth > d) {
            return ControlFlow::Continue(());
        }
        skip_below = None;
        while stack.last().is_some_and(|f| f.depth >= record.depth) {
            let frame = stack.pop().expect("checked");
            close(frame, &mut stack, &mut found, &mut big);
        }
        let name = last_name(path);
        if record.depth > 0 && is_junk(name) {
            if record.kind == FileKind::Directory {
                skip_below = Some(record.depth);
            }
            return ControlFlow::Continue(());
        }
        let gap = incomplete(record.flags, name, record.depth) || uncertain.contains(&record.tree);
        if record.kind == FileKind::Directory {
            if let Some(parent) = stack.last_mut() {
                parent.complete &= !gap;
            }
            stack.push(Frame {
                depth: record.depth,
                folder: Folder {
                    path: path.to_vec(),
                    tree: record.tree,
                    objectid: record.objectid,
                    files: 0,
                    bytes: 0,
                },
                hash: Sha256::new(),
                complete: !gap,
                excluded: excluded(path, mount, record.flags),
            });
            return ControlFlow::Continue(());
        }
        let Some(parent) = stack.last_mut() else {
            return ControlFlow::Continue(());
        };
        parent.complete &= !gap;
        parent.hash.update(&[record.kind.letter() as u8]);
        put_name(&mut parent.hash, name);
        parent.hash.update(&record.metrics.logical.to_le_bytes());
        parent.folder.files += 1;
        parent.folder.bytes += record.metrics.logical;
        ControlFlow::Continue(())
    })?;
    while let Some(frame) = stack.pop() {
        close(frame, &mut stack, &mut found, &mut big);
    }
    let mut all: Vec<([u8; 32], Vec<Folder>)> =
        found.into_iter().filter(|(_, m)| m.len() > 1).collect();
    all.sort_by_key(|s| s.0);
    let mut seeds = Vec::new();
    for (_, members) in &all {
        let members = &members[..members.len().min(SEED_MEMBERS)];
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                if last_name(&a.path) == last_name(&b.path) {
                    seeds.push((a.path.clone(), b.path.clone()));
                }
            }
        }
    }
    let sets: Vec<([u8; 32], Vec<Folder>)> = all
        .into_iter()
        .filter(|(_, m)| m[0].bytes >= LIST_MIN_SIZE)
        .collect();
    let mut set_of: HashMap<Vec<u8>, usize> = HashMap::new();
    for (i, (_, members)) in sets.iter().enumerate() {
        for m in members {
            set_of.insert(m.path.clone(), i);
        }
    }
    let sets = sets
        .into_iter()
        .filter(|(_, members)| {
            let parents: HashSet<Option<usize>> = members
                .iter()
                .map(|m| set_of.get(parent(&m.path)).copied())
                .collect();
            !(parents.len() == 1 && parents.iter().next().is_some_and(|p| p.is_some()))
        })
        .map(|(_, members)| members)
        .collect();
    Ok(Fingerprints { sets, seeds, big })
}

/// One file (or symlink) below a member.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub(super) path: Vec<u8>,
    pub(super) kind: FileKind,
    pub(super) tree: u64,
    pub(super) objectid: u64,
    /// Logical size, as the report has it.
    pub(super) size: u64,
}

/// One folder below a member.
#[derive(Debug, Clone)]
pub(super) struct Dir {
    pub(super) path: Vec<u8>,
    pub(super) tree: u64,
    pub(super) objectid: u64,
}

/// A folder whose files are read: a candidate identical folder, or one
/// side of a pair that may be partly identical.
#[derive(Debug, Clone)]
pub(super) struct Member {
    pub(super) path: Vec<u8>,
    pub(super) tree: u64,
    pub(super) objectid: u64,
    /// The candidate set, for an identical-folder candidate.
    set: Option<usize>,
    /// Indexes into the entries and dirs below it.
    pub(super) files: Vec<u32>,
    pub(super) dirs: Vec<u32>,
}

/// What the inode search found of one inode.
#[derive(Debug, Clone, Default)]
pub(super) struct Found {
    pub(super) generation: u64,
    pub(super) mtime: u64,
    pub(super) stamp: super::model::Stamp,
    pub(super) nlink: u32,
    pub(super) size: u64,
    pub(super) nodatasum: bool,
    /// File offset, then the extent: (bytenr, disk bytes, offset, len,
    /// prealloc, compressed, shared leaf), or the inline data's hash.
    pub(super) extents: Vec<(u64, Part)>,
}

#[derive(Debug, Clone)]
pub(super) enum Part {
    Data {
        bytenr: u64,
        disk_bytes: u64,
        offset: u64,
        len: u64,
        prealloc: bool,
        compressed: bool,
        shared_leaf: bool,
    },
    Inline([u8; 32]),
}

/// Every file and folder below the members, read again.
pub(super) struct Confirmed {
    pub(super) members: Vec<Member>,
    pub(super) entries: Vec<Entry>,
    pub(super) dirs: Vec<Dir>,
    pub(super) found: HashMap<(u64, u64), Found>,
    /// Per entry: its content signature, `None` without checksums.
    pub(super) sig: Vec<Option<[u8; 32]>>,
}

impl Confirmed {
    pub(super) fn found(&self, tree: u64, objectid: u64) -> Option<&Found> {
        self.found.get(&(tree, objectid))
    }

    /// The path of entry `e` below member `m`, with its leading `/`.
    pub(super) fn rel(&self, m: usize, e: u32) -> &[u8] {
        &self.entries[e as usize].path[self.members[m].path.len()..]
    }
}

/// Steps 3 and 4: confirmed identical folders, partly identical folders,
/// and the bytes of folders that could not be confirmed. `seeds` are
/// pairs of files with one name and one content in different folders.
#[allow(clippy::too_many_arguments)]
pub fn find<S: MetadataSource + ?Sized>(
    source: &mut S,
    nodes: &mut dyn Nodes,
    mount: &[u8],
    subvolumes: &SubvolumeSet,
    uncertain: &HashSet<u64>,
    layout: &Layout,
    seeds: &[(Vec<u8>, Vec<u8>)],
    boundary: &mut dyn BatchBoundary,
    analysis: &mut Analysis,
) -> Result<Vec<FolderGroup>, super::analysis::AnalysisError> {
    let fp = candidates(nodes, mount, uncertain)?;
    let tops = super::partial::tops(
        fp.seeds
            .iter()
            .chain(seeds)
            .map(|(a, b)| (a.as_slice(), b.as_slice())),
        mount,
        &fp.big,
    );
    if fp.sets.is_empty() && tops.is_empty() {
        return Ok(Vec::new());
    }
    // Members: every candidate, then the sides of the pairs.
    let mut members: Vec<Member> = Vec::new();
    let mut by_path: HashMap<Vec<u8>, usize> = HashMap::new();
    for (s, set) in fp.sets.iter().enumerate() {
        for m in set {
            by_path.insert(m.path.clone(), members.len());
            members.push(Member {
                path: m.path.clone(),
                tree: m.tree,
                objectid: m.objectid,
                set: Some(s),
                files: Vec::new(),
                dirs: Vec::new(),
            });
        }
    }
    let whole = members.len();
    for (a, b) in &tops {
        for side in [a, b] {
            if !by_path.contains_key(side) {
                by_path.insert(side.clone(), members.len());
                members.push(Member {
                    path: side.clone(),
                    tree: 0,
                    objectid: 0,
                    set: None,
                    files: Vec::new(),
                    dirs: Vec::new(),
                });
            }
        }
    }
    let (mut entries, mut dirs) = match below(nodes, &by_path, &mut members)? {
        Some(read) => read,
        None => {
            // Too many files: the pairs give way to identical folders.
            members.truncate(whole);
            by_path.retain(|_, m| *m < whole);
            match below(nodes, &by_path, &mut members)? {
                Some(read) => read,
                None => {
                    for set in &fp.sets {
                        for m in set {
                            analysis.unchecked.folders += 1;
                            analysis.unchecked.folder_bytes += m.bytes;
                        }
                    }
                    return Ok(Vec::new());
                }
            }
        }
    };
    let tops: Vec<(usize, usize)> = tops
        .iter()
        .filter_map(|(a, b)| Some((*by_path.get(a)?, *by_path.get(b)?)))
        .collect();
    entries.shrink_to_fit();
    dirs.shrink_to_fit();
    // Inodes: files, folders and the members themselves.
    let mut wanted: Vec<(u64, u64)> = entries.iter().map(|e| (e.tree, e.objectid)).collect();
    wanted.extend(dirs.iter().map(|d| (d.tree, d.objectid)));
    wanted.extend(members.iter().map(|m| (m.tree, m.objectid)));
    wanted.sort_unstable();
    wanted.dedup();
    let found = lookup(
        source,
        subvolumes,
        &wanted,
        boundary,
        &mut analysis.searches,
    )?;
    // Data checksums of every data part, in one batched read.
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for f in found.values() {
        if f.nodatasum {
            continue;
        }
        for (_, part) in &f.extents {
            if let Part::Data {
                bytenr,
                offset,
                len,
                prealloc: false,
                compressed: false,
                ..
            } = *part
                && bytenr != 0
            {
                ranges.push((bytenr + offset, len.div_ceil(layout.sector) * layout.sector));
            }
        }
    }
    ranges.sort_unstable();
    ranges.dedup();
    let csums = csum::read_many(
        source,
        layout,
        &ranges,
        boundary,
        &mut analysis.csum_searches,
    )?;
    let csum_of: HashMap<(u64, u64), Option<Vec<u8>>> = ranges.into_iter().zip(csums).collect();
    let sig: Vec<Option<[u8; 32]>> = entries
        .iter()
        .map(|e| {
            found
                .get(&(e.tree, e.objectid))
                .and_then(|f| file_signature(f, layout, &csum_of))
        })
        .collect();
    drop(csum_of);
    let tree = Confirmed {
        members,
        entries,
        dirs,
        found,
        sig,
    };
    // Member signatures: every file's relative path, kind and signature.
    let mut member_sig: Vec<Option<[u8; 32]>> = Vec::with_capacity(whole);
    for (i, m) in tree.members[..whole].iter().enumerate() {
        let mut order = m.files.clone();
        order.sort_by(|&a, &b| tree.rel(i, a).cmp(tree.rel(i, b)));
        let mut hash = Sha256::new();
        let mut signed = true;
        for &e in &order {
            match tree.sig[e as usize] {
                Some(sig) => {
                    hash.update(&[tree.entries[e as usize].kind.letter() as u8]);
                    put_name(&mut hash, tree.rel(i, e));
                    hash.update(&sig);
                }
                None => {
                    signed = false;
                    break;
                }
            }
        }
        member_sig.push(signed.then(|| hash.finish()));
    }
    // Groups: members of one candidate set with one signature.
    let roots = DupRoots(subvolumes);
    let mut out = Vec::new();
    for s in 0..fp.sets.len() {
        let mut by_sig: BTreeMap<[u8; 32], Vec<usize>> = BTreeMap::new();
        for (i, m) in tree.members[..whole].iter().enumerate() {
            if m.set != Some(s) {
                continue;
            }
            match member_sig[i] {
                Some(h) => by_sig.entry(h).or_default().push(i),
                None => {
                    analysis.unchecked.folders += 1;
                    analysis.unchecked.folder_bytes += fp.sets[s][0].bytes;
                }
            }
        }
        for (sig, list) in by_sig {
            if list.len() < 2 {
                continue;
            }
            let sides: Vec<Vec<u32>> = list
                .iter()
                .map(|&i| tree.members[i].files.clone())
                .collect();
            let (cluster_of, clusters) =
                account(source, &roots, &tree, &sides, boundary, analysis)?;
            let mut list_members: Vec<FolderMember> = list
                .iter()
                .enumerate()
                .map(|(k, &i)| {
                    let m = &tree.members[i];
                    let bytes = m.files.iter().map(|&e| tree.entries[e as usize].size).sum();
                    let mut fm = FolderMember::new(&m.path, mount, m.files.len() as u64, bytes);
                    let (newest, oldest) = ages(&tree, &m.files);
                    fm.mtime = newest;
                    fm.oldest = oldest;
                    fm.tree = m.tree;
                    fm.objectid = m.objectid;
                    fm.generation = tree.found(m.tree, m.objectid).map_or(0, |f| f.generation);
                    fm.cluster = cluster_of[k];
                    fm.dirs = m
                        .dirs
                        .iter()
                        .map(|&d| tree.dirs[d as usize].path.clone())
                        .collect();
                    fm.file_stamps = stamps(&tree, i, &m.files);
                    fm
                })
                .collect();
            list_members.sort_by(|a, b| a.path.cmp(&b.path));
            let first = &fp.sets[s][0];
            out.push(FolderGroup {
                id: format!("f{}", hex(&sig[..16])),
                name: last_name(&tree.members[list[0]].path).to_vec(),
                files: first.files,
                bytes: first.bytes,
                covered: 0,
                members: list_members,
                clusters,
                items: Vec::new(),
                in_folders: false,
                stamps: None,
            });
        }
    }
    // Partly identical folders.
    let partial = super::partial::find(
        source, &roots, &tree, &tops, &fp.big, mount, &out, boundary, analysis,
    )?;
    out.extend(partial.groups);
    super::partial::settle(&mut out, &partial.similar);
    out.sort_by(|a, b| {
        let free = |f: &FolderGroup| f.clusters.iter().map(|c| c.free).sum::<u64>();
        free(b).cmp(&free(a)).then(a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Lists every file and folder below the members, junk left out; `None`
/// beyond [`MAX_FILES`] files. A file below nested members belongs to
/// each.
fn below(
    nodes: &mut dyn Nodes,
    by_path: &HashMap<Vec<u8>, usize>,
    members: &mut [Member],
) -> io::Result<Option<(Vec<Entry>, Vec<Dir>)>> {
    for m in members.iter_mut() {
        m.files.clear();
        m.dirs.clear();
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut dirs: Vec<Dir> = Vec::new();
    let mut inside: Vec<(u32, usize)> = Vec::new();
    let mut skip_below: Option<u32> = None;
    let mut overflow = false;
    nodes.walk(&mut |path, record| {
        if skip_below.is_some_and(|d| record.depth > d) {
            return ControlFlow::Continue(());
        }
        skip_below = None;
        while inside.last().is_some_and(|&(d, _)| d >= record.depth) {
            inside.pop();
        }
        let member = if record.kind == FileKind::Directory {
            by_path.get(path).copied()
        } else {
            None
        };
        if inside.is_empty() && member.is_none() {
            return ControlFlow::Continue(());
        }
        if member.is_none() && is_junk(last_name(path)) {
            if record.kind == FileKind::Directory {
                skip_below = Some(record.depth);
            }
            return ControlFlow::Continue(());
        }
        if record.kind == FileKind::Directory {
            if let Some(m) = member {
                members[m].tree = record.tree;
                members[m].objectid = record.objectid;
            }
            if !inside.is_empty() {
                let d = dirs.len() as u32;
                dirs.push(Dir {
                    path: path.to_vec(),
                    tree: record.tree,
                    objectid: record.objectid,
                });
                for &(_, o) in &inside {
                    members[o].dirs.push(d);
                }
            }
            if let Some(m) = member {
                inside.push((record.depth, m));
            }
            return ControlFlow::Continue(());
        }
        if entries.len() >= MAX_FILES {
            overflow = true;
            return ControlFlow::Break(());
        }
        let e = entries.len() as u32;
        entries.push(Entry {
            path: path.to_vec(),
            kind: record.kind,
            tree: record.tree,
            objectid: record.objectid,
            size: record.metrics.logical,
        });
        for &(_, o) in &inside {
            members[o].files.push(e);
        }
        ControlFlow::Continue(())
    })?;
    Ok((!overflow).then_some((entries, dirs)))
}

/// The newest and oldest modification of `files`, Unix seconds.
pub(super) fn ages(tree: &Confirmed, files: &[u32]) -> (u64, u64) {
    let (mut newest, mut oldest) = (0, u64::MAX);
    for &e in files {
        let e = &tree.entries[e as usize];
        if let Some(f) = tree.found(e.tree, e.objectid) {
            newest = newest.max(f.mtime);
            oldest = oldest.min(f.mtime);
        }
    }
    (newest, oldest.min(newest))
}

/// `files` of member `m` as the analysis read them.
pub(super) fn stamps(tree: &Confirmed, m: usize, files: &[u32]) -> Vec<super::model::FileStamp> {
    files
        .iter()
        .filter_map(|&e| {
            let entry = &tree.entries[e as usize];
            tree.found(entry.tree, entry.objectid)
                .map(|f| super::model::FileStamp {
                    rel: tree.rel(m, e).to_vec(),
                    stamp: f.stamp,
                })
        })
        .collect()
}

/// Step 4 for one group: `sides` are each member's files. Members sharing
/// an extent form one cluster; each cluster's extents are judged with the
/// normal accounting. Returns each side's cluster and the clusters.
pub(super) fn account<S: MetadataSource + ?Sized>(
    source: &mut S,
    roots: &DupRoots<'_>,
    tree: &Confirmed,
    sides: &[Vec<u32>],
    boundary: &mut dyn BatchBoundary,
    analysis: &mut Analysis,
) -> Result<(Vec<u32>, Vec<Cluster>), WalkError> {
    let parts = |k: usize| {
        sides[k].iter().filter_map(|&e| {
            let e = &tree.entries[e as usize];
            tree.found(e.tree, e.objectid).map(|f| (e, f))
        })
    };
    let mut owner: HashMap<u64, usize> = HashMap::new();
    let mut parent_of: Vec<usize> = (0..sides.len()).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for k in 0..sides.len() {
        for (_, f) in parts(k) {
            for (_, part) in &f.extents {
                if let Part::Data { bytenr, .. } = part
                    && *bytenr != 0
                {
                    match owner.get(bytenr) {
                        Some(&o) => {
                            let (a, b) = (root(&mut parent_of, k), root(&mut parent_of, o));
                            parent_of[a.max(b)] = a.min(b);
                        }
                        None => {
                            owner.insert(*bytenr, k);
                        }
                    }
                }
            }
        }
    }
    let mut number: HashMap<usize, u32> = HashMap::new();
    let cluster_of: Vec<u32> = (0..sides.len())
        .map(|k| {
            let r = root(&mut parent_of, k);
            let next = number.len() as u32;
            *number.entry(r).or_insert(next)
        })
        .collect();
    let mut clusters = vec![Cluster::default(); number.len()];
    // Extents per cluster, with their holders.
    let mut per: Vec<BTreeMap<u64, (u64, Vec<Holder>)>> = vec![BTreeMap::new(); clusters.len()];
    for k in 0..sides.len() {
        let mut names: HashMap<(u64, u64), u64> = HashMap::new();
        for (e, _) in parts(k) {
            *names.entry((e.tree, e.objectid)).or_default() += 1;
        }
        for (e, f) in parts(k) {
            for (_, part) in &f.extents {
                let Part::Data {
                    bytenr,
                    disk_bytes,
                    shared_leaf,
                    ..
                } = *part
                else {
                    continue;
                };
                if bytenr == 0 {
                    continue;
                }
                let holder = Holder {
                    tree: e.tree,
                    objectid: e.objectid,
                    external_names: u64::from(f.nlink) > names[&(e.tree, e.objectid)],
                    shared_leaf,
                };
                let entry = per[cluster_of[k] as usize]
                    .entry(bytenr)
                    .or_insert((disk_bytes, Vec::new()));
                if !entry.1.contains(&holder) {
                    entry.1.push(holder);
                }
            }
        }
    }
    let wanted: Vec<u64> = {
        let mut w: Vec<u64> = per.iter().flat_map(|m| m.keys().copied()).collect();
        w.sort_unstable();
        w.dedup();
        w
    };
    let mut refs: HashMap<u64, Option<ExtentRefs>> = HashMap::new();
    let stats = resolve(
        source,
        wanted.iter().copied(),
        Plan::Coalesce {
            max_gap: crate::scan::COALESCE_GAP,
        },
        boundary,
        |b, r| {
            refs.insert(b, r.cloned());
        },
    )?;
    analysis.reference_searches += stats.ranges;
    let snapshot_only = Blockers::SNAPSHOT.with(Blockers::SHARED_TREE);
    for (c, extents) in per.into_iter().enumerate() {
        for (bytenr, (disk_bytes, holders)) in extents {
            let r = refs.get(&bytenr).and_then(|r| r.as_ref());
            let cluster = &mut clusters[c];
            match classify(disk_bytes, &holders, r, roots) {
                Verdict::Reclaimable => cluster.free += disk_bytes,
                Verdict::Retained(b) if b.bits() & !snapshot_only.bits() == 0 => {
                    cluster.later += disk_bytes
                }
                Verdict::Retained(_) => cluster.held += disk_bytes,
                Verdict::Unknown => cluster.unknown += disk_bytes,
            }
        }
    }
    Ok((cluster_of, clusters))
}

/// A file's content signature: its size and, per data part in file
/// order, the inline data's hash or the sector checksums; `None` when a
/// part has none (NOCOW, compressed).
fn file_signature(
    f: &Found,
    layout: &Layout,
    csum_of: &HashMap<(u64, u64), Option<Vec<u8>>>,
) -> Option<[u8; 32]> {
    if f.nodatasum {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(&f.size.to_le_bytes());
    for (offset, part) in &f.extents {
        hash.update(&offset.to_le_bytes());
        match part {
            Part::Inline(h) => {
                hash.update(b"i");
                hash.update(h);
            }
            Part::Data { bytenr: 0, len, .. }
            | Part::Data {
                prealloc: true,
                len,
                ..
            } => {
                hash.update(b"z");
                hash.update(&len.to_le_bytes());
            }
            Part::Data {
                compressed: true, ..
            } => return None,
            Part::Data {
                bytenr,
                offset: o,
                len,
                ..
            } => {
                let key = (bytenr + o, len.div_ceil(layout.sector) * layout.sector);
                let sums = csum_of.get(&key)?.as_ref()?;
                hash.update(b"c");
                hash.update(sums);
            }
        }
    }
    Some(hash.finish())
}

/// Every inode of `wanted` (sorted), in searches over runs of nearby
/// objectids per tree.
fn lookup<S: MetadataSource + ?Sized>(
    source: &mut S,
    subvolumes: &SubvolumeSet,
    wanted: &[(u64, u64)],
    boundary: &mut dyn BatchBoundary,
    searches: &mut u64,
) -> Result<HashMap<(u64, u64), Found>, WalkError> {
    let mut out: HashMap<(u64, u64), Found> = HashMap::with_capacity(wanted.len());
    let want: HashSet<(u64, u64)> = wanted.iter().copied().collect();
    let mut at = 0;
    while at < wanted.len() {
        let (tree, lo) = wanted[at];
        let mut hi = lo;
        let mut next = at + 1;
        while next < wanted.len() && wanted[next].0 == tree && wanted[next].1 <= hi + INODE_GAP {
            hi = wanted[next].1;
            next += 1;
        }
        let sharing = subvolumes.leaf_sharing(tree);
        let range = SearchRange::new(
            tree,
            Key::new(lo, INODE_ITEM_KEY, 0),
            Key::new(hi, EXTENT_DATA_KEY, u64::MAX),
        );
        let stats = walk_tree(source, range, boundary, |item| {
            let id = (tree, item.key.objectid);
            if !want.contains(&id) {
                return Ok(());
            }
            match item.key.item_type {
                INODE_ITEM_KEY => {
                    if let Ok(i) = parse_inode_item(item.data) {
                        let f = out.entry(id).or_default();
                        f.generation = i.generation;
                        f.mtime = i.mtime.sec;
                        f.stamp = super::model::Stamp::of(&i);
                        f.nlink = i.nlink;
                        f.size = i.size;
                        f.nodatasum = i.flags & INODE_NODATASUM != 0;
                    }
                }
                EXTENT_DATA_KEY => {
                    let f = out.entry(id).or_default();
                    match parse_file_extent(item.data) {
                        Ok(e) => {
                            let part = match e.kind {
                                ExtentKind::Inline { inline_len } => {
                                    let data = &item.data[item.data.len() - inline_len..];
                                    let mut h = Sha256::new();
                                    h.update(&[e.compression]);
                                    h.update(&e.ram_bytes.to_le_bytes());
                                    h.update(data);
                                    Part::Inline(h.finish())
                                }
                                ExtentKind::Regular(d) | ExtentKind::Prealloc(d) => Part::Data {
                                    bytenr: d.disk_bytenr,
                                    disk_bytes: d.disk_num_bytes,
                                    offset: d.offset,
                                    len: d.num_bytes,
                                    prealloc: matches!(e.kind, ExtentKind::Prealloc(_)),
                                    compressed: e.compression != 0,
                                    shared_leaf: sharing.may_share(item.transid),
                                },
                            };
                            f.extents.push((item.key.offset, part));
                        }
                        Err(_) => f.nodatasum = true,
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        *searches += stats.batches.max(1);
        at = next;
    }
    Ok(out)
}

pub(super) fn put_name(hash: &mut Sha256, name: &[u8]) {
    hash.update(&(name.len() as u64).to_le_bytes());
    hash.update(name);
}

pub(super) fn last_name(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}

pub(super) fn parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[..i],
        None => b"",
    }
}
