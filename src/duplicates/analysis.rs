//! The duplicate analysis (docs/duplicates.md "Analysis"): from a
//! volume report and its tier-0 candidates, finds files with the same
//! content in different extents, using metadata only.
//!
//! 1. Group candidates by size; a size with one candidate is unique.
//! 2. Name each remaining candidate from the report; drop app-side and
//!    DSM copies ([`excluded`]).
//! 3. Read each candidate's inode and file extents again, and the data
//!    checksums of its extents: the per-sector checksum sequence, hashed,
//!    is its content signature. Files without checksums (NOCOW,
//!    `nodatasum`) or with compressed extents are "unchecked".
//! 4. Group by signature. Copies sharing an extent form a physical
//!    cluster: a clone frees nothing while its twin stays.
//! 5. Judge every cluster's extents against the extent tree with the
//!    normal accounting (`model::accounting::classify`): reclaimable bytes
//!    free now, bytes only snapshots hold free later, anything else held
//!    elsewhere (another file, another group) frees nothing.
//! 6. Derive identical folders from the report: a folder's signature
//!    covers its names, its small files' sizes and its large files'
//!    content groups.

use super::candidates::Candidate;
use super::csum::{self, Layout};
use super::model::{Analysis, Category, Cluster, CopyRecord, FolderGroup, Group, Unchecked};
use crate::btrfs::extent_refs::{ExtentRefs, Plan, resolve};
use crate::btrfs::keys::{EXTENT_DATA_KEY, INODE_ITEM_KEY, Key};
use crate::btrfs::parser::{ExtentKind, parse_file_extent, parse_inode_item};
use crate::btrfs::search::SearchRange;
use crate::btrfs::source::MetadataSource;
use crate::btrfs::subvolume::SubvolumeSet;
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_tree};
use crate::model::accounting::{Blockers, Holder, RootClass, Verdict, classify};
use crate::model::namespace::FileKind;
use crate::model::tree::NodeFlags;
use crate::report::NodeRecord;
use crate::sha256::Sha256;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::ops::ControlFlow;

/// `BTRFS_INODE_NODATASUM`.
const INODE_NODATASUM: u64 = 1;
/// Extents judged per batch of the extent tree.
const EXTENT_BATCH: usize = 1 << 16;
/// Copies of one file group paired as seeds of partly identical folders,
/// at most.
const SEED_COPIES: usize = 16;
/// Folders named by DSM apps inside shares; any `@` folder is left out
/// anyway, these are the ones the survey found holding copies.
pub const APP_MARKERS: [&[u8]; 5] = [
    b"@spamdata",
    b"@quarantine",
    b"@ChatWorking",
    b"@ActiveProtectVault",
    b"@TieringVaultData",
];

/// A report's nodes, walked in preorder with full paths, as often as
/// asked.
pub trait Nodes {
    fn walk(
        &mut self,
        visit: &mut dyn FnMut(&[u8], &NodeRecord<'_>) -> ControlFlow<()>,
    ) -> io::Result<()>;
}

impl Nodes for &crate::report::Report {
    fn walk(
        &mut self,
        visit: &mut dyn FnMut(&[u8], &NodeRecord<'_>) -> ControlFlow<()>,
    ) -> io::Result<()> {
        self.for_each(|path, record| visit(path, record));
        Ok(())
    }
}

/// The report file at a path, read record by record each walk.
pub struct ReportFile(pub std::path::PathBuf);

impl Nodes for ReportFile {
    fn walk(
        &mut self,
        visit: &mut dyn FnMut(&[u8], &NodeRecord<'_>) -> ControlFlow<()>,
    ) -> io::Result<()> {
        let file = std::fs::File::open(&self.0)?;
        crate::report::ReportReader::open(io::BufReader::new(file))
            .map_err(io::Error::other)?
            .for_each(|path, record| visit(path, record))
            .map_err(io::Error::other)
    }
}

/// What one analysis reads.
pub struct Inputs<'a> {
    pub mount: &'a [u8],
    pub candidates: Vec<Candidate>,
    /// Some candidates were left out of `candidates`.
    pub candidates_truncated: bool,
    pub subvolumes: &'a SubvolumeSet,
    pub layout: Layout,
    /// Trees whose namespace the scan could not read whole: no folder in
    /// them is complete. `None`: none can be trusted, so no folders.
    pub uncertain: Option<HashSet<u64>>,
}

/// Whether the node at `path` (below `mount`), with `flags`, is DSM,
/// package or app data that is never a copy: any `@` folder, a
/// classified service or package path, this tool's state, encrypted or
/// snapshot content.
pub fn excluded(path: &[u8], mount: &[u8], flags: NodeFlags) -> bool {
    let hidden = NodeFlags::SYSTEM
        .with(NodeFlags::RECYCLE)
        .with(NodeFlags::PACKAGE)
        .with(NodeFlags::OWN_STATE)
        .with(NodeFlags::ENCRYPTED)
        .with(NodeFlags::SNAPSHOT)
        .with(NodeFlags::PLACEHOLDER);
    if flags.bits() & hidden.bits() != 0 {
        return true;
    }
    if path == mount {
        return false;
    }
    let Some(rest) = path.strip_prefix(mount).and_then(|r| r.strip_prefix(b"/")) else {
        return true;
    };
    rest.split(|&b| b == b'/')
        .any(|part| part.first() == Some(&b'@'))
        || crate::dsm::service_of(path, mount).is_some()
        || crate::dsm::package_data_of(path, mount).is_some()
}

/// One file extent of a candidate, in file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileExtent {
    file_offset: u64,
    /// 0 for a hole.
    bytenr: u64,
    disk_bytes: u64,
    offset: u64,
    len: u64,
    prealloc: bool,
    compressed: bool,
    shared_leaf: bool,
}

/// A candidate still standing after its names were read.
#[derive(Debug, Clone)]
struct Inode {
    candidate: Candidate,
    names: Vec<Vec<u8>>,
    excluded: bool,
    nlink: u32,
    mtime: u64,
    stamp: super::model::Stamp,
    generation: u64,
    transid: u64,
    extents: Vec<FileExtent>,
}

/// Why a candidate was left out after its lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signature {
    Content([u8; 32]),
    NoChecksums,
    Compressed,
    /// Changed since the scan, or gone.
    Changed,
}

pub(crate) struct DupRoots<'a>(pub(crate) &'a SubvolumeSet);

impl RootClass for DupRoots<'_> {
    fn external_root(&self, root: u64) -> Blockers {
        match self.0.get(root) {
            None => Blockers::UNKNOWN,
            Some(s) if s.pending_deletion => Blockers::PENDING_DELETION,
            Some(s) if s.readonly => Blockers::SNAPSHOT,
            Some(_) => Blockers::LIVE_REFLINK,
        }
    }

    fn shared_leaf(&self, tree: u64) -> Blockers {
        if self.0.with_snapshot(tree) {
            Blockers::SNAPSHOT
        } else {
            Blockers::SHARED_TREE
        }
    }
}

/// Runs the analysis; `nodes` is walked twice.
pub fn analyze<S: MetadataSource + ?Sized>(
    source: &mut S,
    nodes: &mut dyn Nodes,
    inputs: Inputs<'_>,
    boundary: &mut dyn BatchBoundary,
) -> Result<Analysis, AnalysisError> {
    let mut analysis = Analysis {
        candidates: inputs.candidates.len() as u64,
        complete: !inputs.candidates_truncated,
        ..Analysis::default()
    };
    // 1. Sizes with more than one candidate.
    let mut by_size: HashMap<u64, u32> = HashMap::new();
    for c in &inputs.candidates {
        *by_size.entry(c.size).or_default() += 1;
    }
    let mut wanted: HashMap<(u64, u64), usize> = HashMap::new();
    let mut inodes: Vec<Inode> = Vec::new();
    for c in &inputs.candidates {
        if by_size[&c.size] > 1 {
            wanted.insert((c.tree, c.objectid), inodes.len());
            inodes.push(Inode {
                candidate: *c,
                names: Vec::new(),
                excluded: false,
                nlink: 0,
                mtime: 0,
                stamp: Default::default(),
                generation: 0,
                transid: 0,
                extents: Vec::new(),
            });
        }
    }
    drop(by_size);
    analysis.same_size = inodes.len() as u64;
    // 2. Names from the report.
    let mount = inputs.mount;
    let mut excluded_at: Vec<bool> = Vec::new();
    nodes.walk(&mut |path, record| {
        let depth = record.depth as usize;
        excluded_at.truncate(depth);
        let above = depth > 0 && excluded_at.get(depth - 1).copied().unwrap_or(true);
        let here = above || excluded(path, mount, record.flags);
        excluded_at.push(here);
        if record.kind == FileKind::Regular
            && let Some(&i) = wanted.get(&(record.tree, record.objectid))
        {
            let inode = &mut inodes[i];
            inode.names.push(path.to_vec());
            inode.excluded |= here;
            inode.nlink = record.nlink;
        }
        ControlFlow::Continue(())
    })?;
    // A copy is a user-side inode with every name in the report.
    inodes.retain(|i| !i.names.is_empty() && !i.excluded);
    analysis.excluded = analysis.same_size - inodes.len() as u64;
    let mut sizes: HashMap<u64, u32> = HashMap::new();
    for i in &inodes {
        *sizes.entry(i.candidate.size).or_default() += 1;
    }
    inodes.retain(|i| sizes[&i.candidate.size] > 1);
    drop(sizes);
    inodes.sort_by_key(|i| (i.candidate.tree, i.candidate.objectid));
    // 3. Lookups and content signatures.
    let mut signatures = Vec::with_capacity(inodes.len());
    let mut by_layout: HashMap<Vec<FileExtent>, Signature> = HashMap::new();
    for inode in &mut inodes {
        let sharing = inputs.subvolumes.leaf_sharing(inode.candidate.tree);
        let fresh = lookup(source, inode, &sharing, boundary, &mut analysis.searches)?;
        let signature = if !fresh {
            Signature::Changed
        } else if let Some(&known) = by_layout.get(&inode.extents) {
            known
        } else {
            let s = signature(
                source,
                &inputs.layout,
                inode,
                boundary,
                &mut analysis.csum_searches,
            )?;
            by_layout.insert(inode.extents.clone(), s);
            s
        };
        signatures.push(signature);
    }
    drop(by_layout);
    let mut by_content: BTreeMap<(u64, [u8; 32]), Vec<usize>> = BTreeMap::new();
    let mut unchecked_sizes: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, s) in signatures.iter().enumerate() {
        let size = inodes[i].candidate.size;
        match s {
            Signature::Content(sig) => by_content.entry((size, *sig)).or_default().push(i),
            Signature::NoChecksums | Signature::Compressed => {
                unchecked_sizes.entry(size).or_default().push(i)
            }
            Signature::Changed => analysis.changed += 1,
        }
    }
    // An unchecked file counts when another copy of its size exists.
    let mut unchecked = Unchecked::default();
    for (size, members) in &unchecked_sizes {
        let checked = by_content
            .range((*size, [0; 32])..=(*size, [u8::MAX; 32]))
            .count();
        if members.len() + checked > 1 {
            for &i in members {
                unchecked.files += 1;
                unchecked.bytes += size;
                match signatures[i] {
                    Signature::Compressed => unchecked.compressed += 1,
                    _ => unchecked.no_checksums += 1,
                }
            }
        }
    }
    analysis.unchecked = unchecked;
    // 4. Groups and clusters.
    let mut groups = Vec::new();
    let mut members_of: Vec<Vec<usize>> = Vec::new();
    for ((size, sig), mut members) in by_content {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|&a, &b| {
            inodes[a]
                .names
                .iter()
                .min()
                .cmp(&inodes[b].names.iter().min())
        });
        let mut copies = Vec::with_capacity(members.len());
        let clusters = clusters(&members, &inodes);
        for (k, &i) in members.iter().enumerate() {
            let inode = &inodes[i];
            let mut names = inode.names.clone();
            names.sort();
            let mut copy = CopyRecord::new(
                names,
                mount,
                inode.mtime,
                inode.candidate.tree,
                inode.candidate.objectid,
                inode.generation,
                clusters[k],
            );
            copy.stamp = inode.stamp;
            copies.push(copy);
        }
        // The name most copies have; the first by path on a tie.
        let mut names: Vec<&[u8]> = copies.iter().map(|c| c.file_name()).collect();
        names.sort();
        let name = names
            .chunk_by(|a, b| a == b)
            .max_by(|a, b| a.len().cmp(&b.len()).then(b[0].cmp(a[0])))
            .map_or_else(Vec::new, |n| n[0].to_vec());
        let count = clusters.iter().max().map_or(0, |m| m + 1);
        groups.push(Group {
            id: hex(&sig[..16]),
            category: Category::of(&name),
            name,
            size,
            copies,
            clusters: vec![Cluster::default(); count as usize],
            in_folders: false,
        });
        members_of.push(members);
    }
    // 5. What each cluster frees.
    judge(
        source,
        inputs.subvolumes,
        &inodes,
        &members_of,
        &mut groups,
        boundary,
        &mut analysis,
    )?;
    // 6. Identical and partly identical folders; copies with one name in
    // different folders show where partly identical folders may be.
    let mut seeds: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for g in &groups {
        let copies = &g.copies[..g.copies.len().min(SEED_COPIES)];
        for (i, a) in copies.iter().enumerate() {
            for b in &copies[i + 1..] {
                if a.file_name() == b.file_name() {
                    seeds.push((a.path().to_vec(), b.path().to_vec()));
                }
            }
        }
    }
    analysis.folders = match &inputs.uncertain {
        Some(uncertain) => super::folders::find(
            source,
            nodes,
            mount,
            inputs.subvolumes,
            uncertain,
            &inputs.layout,
            &seeds,
            boundary,
            &mut analysis,
        )?,
        None => Vec::new(),
    };
    let mut folders = std::mem::take(&mut analysis.folders);
    for folder in &mut folders {
        mark_covered(folder, &mut groups);
    }
    analysis.folders = folders;
    analysis.groups = groups;
    analysis.finish();
    Ok(analysis)
}

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Walk(#[from] WalkError),
    #[error("report: {0}")]
    Report(#[from] io::Error),
}

/// Reads `inode`'s inode item and file extents; false when it no longer
/// matches its tier-0 record.
fn lookup<S: MetadataSource + ?Sized>(
    source: &mut S,
    inode: &mut Inode,
    sharing: &crate::btrfs::subvolume::LeafSharing,
    boundary: &mut dyn BatchBoundary,
    searches: &mut u64,
) -> Result<bool, WalkError> {
    let c = inode.candidate;
    let range = SearchRange::new(
        c.tree,
        Key::new(c.objectid, INODE_ITEM_KEY, 0),
        Key::new(c.objectid, EXTENT_DATA_KEY, u64::MAX),
    );
    let mut item_seen = false;
    let mut valid = true;
    let mut extents = Vec::new();
    let mut flags = 0;
    let stats = walk_tree(source, range, boundary, |item| {
        match item.key.item_type {
            INODE_ITEM_KEY => match parse_inode_item(item.data) {
                Ok(i) => {
                    item_seen = true;
                    valid &= i.size == c.size && FileKind::from_mode(i.mode) == FileKind::Regular;
                    inode.mtime = i.mtime.sec;
                    inode.stamp = super::model::Stamp::of(&i);
                    inode.generation = i.generation;
                    inode.transid = i.transid;
                    inode.nlink = i.nlink;
                    flags = i.flags;
                }
                Err(_) => valid = false,
            },
            EXTENT_DATA_KEY => match parse_file_extent(item.data) {
                Ok(e) => {
                    let (disk, prealloc) = match e.kind {
                        ExtentKind::Inline { .. } => {
                            valid = false;
                            return Ok(());
                        }
                        ExtentKind::Regular(d) => (d, false),
                        ExtentKind::Prealloc(d) => (d, true),
                    };
                    extents.push(FileExtent {
                        file_offset: item.key.offset,
                        bytenr: disk.disk_bytenr,
                        disk_bytes: disk.disk_num_bytes,
                        offset: disk.offset,
                        len: disk.num_bytes,
                        prealloc,
                        compressed: e.compression != 0,
                        shared_leaf: sharing.may_share(item.transid),
                    });
                }
                Err(_) => valid = false,
            },
            _ => {}
        }
        Ok(())
    })?;
    *searches += stats.batches.max(1);
    let first = extents
        .iter()
        .find(|e| e.bytenr != 0)
        .map(|e| e.bytenr.wrapping_add(e.offset));
    valid &= item_seen && first == Some(c.first_block);
    if flags & INODE_NODATASUM != 0 {
        // Recorded as a layout no checksum can match.
        extents.iter_mut().for_each(|e| e.prealloc = e.bytenr != 0);
        inode.extents = extents;
        inode.extents.push(FileExtent {
            file_offset: u64::MAX,
            bytenr: 0,
            disk_bytes: 0,
            offset: 0,
            len: 0,
            prealloc: true,
            compressed: false,
            shared_leaf: false,
        });
        return Ok(valid);
    }
    inode.extents = extents;
    Ok(valid)
}

/// The content signature of `inode`: SHA-256 over its size and, per
/// sector in file order, a kind byte and the data checksum (data) or
/// nothing (a hole or preallocated range, which read as zeros).
fn signature<S: MetadataSource + ?Sized>(
    source: &mut S,
    layout: &Layout,
    inode: &Inode,
    boundary: &mut dyn BatchBoundary,
    searches: &mut u64,
) -> Result<Signature, WalkError> {
    const DATA: u8 = 1;
    const ZERO: u8 = 2;
    if inode.extents.iter().any(|e| e.file_offset == u64::MAX) {
        return Ok(Signature::NoChecksums);
    }
    if inode.extents.iter().any(|e| e.compressed && e.bytenr != 0) {
        return Ok(Signature::Compressed);
    }
    let sector = layout.sector;
    let size = inode.candidate.size;
    let sectors = size.div_ceil(sector);
    let mut hash = Sha256::new();
    hash.update(&size.to_le_bytes());
    let mut at = 0u64;
    let zeros = |hash: &mut Sha256, n: u64| {
        for _ in 0..n {
            hash.update(&[ZERO]);
        }
    };
    for e in &inode.extents {
        if e.file_offset % sector != 0 || e.offset % sector != 0 {
            return Ok(Signature::NoChecksums);
        }
        let first = e.file_offset / sector;
        if first >= sectors {
            break;
        }
        if first < at {
            return Ok(Signature::Changed);
        }
        zeros(&mut hash, first - at);
        let count = e.len.div_ceil(sector).min(sectors - first);
        if e.bytenr == 0 || e.prealloc {
            zeros(&mut hash, count);
        } else {
            let start = e.bytenr + e.offset;
            let Some(csums) =
                csum::read(source, layout, start, count * sector, boundary, searches)?
            else {
                return Ok(Signature::NoChecksums);
            };
            for c in csums.chunks_exact(layout.csum_size) {
                hash.update(&[DATA]);
                hash.update(c);
            }
        }
        at = first + count;
    }
    zeros(&mut hash, sectors.saturating_sub(at));
    Ok(Signature::Content(hash.finish()))
}

/// The cluster of each of `members`: copies sharing a physical extent
/// share a cluster, numbered from 0 in member order.
fn clusters(members: &[usize], inodes: &[Inode]) -> Vec<u32> {
    let mut parent: Vec<usize> = (0..members.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut owner: HashMap<u64, usize> = HashMap::new();
    for (k, &i) in members.iter().enumerate() {
        for e in inodes[i].extents.iter().filter(|e| e.bytenr != 0) {
            match owner.get(&e.bytenr) {
                Some(&other) => {
                    let (a, b) = (root(&mut parent, k), root(&mut parent, other));
                    parent[a.max(b)] = a.min(b);
                }
                None => {
                    owner.insert(e.bytenr, k);
                }
            }
        }
    }
    let mut number: HashMap<usize, u32> = HashMap::new();
    (0..members.len())
        .map(|k| {
            let r = root(&mut parent, k);
            let next = number.len() as u32;
            *number.entry(r).or_insert(next)
        })
        .collect()
}

/// Judges each cluster's extents: bytes deleting the whole cluster frees
/// now, and bytes only snapshots keep, which free when they go.
fn judge<S: MetadataSource + ?Sized>(
    source: &mut S,
    subvolumes: &SubvolumeSet,
    inodes: &[Inode],
    members_of: &[Vec<usize>],
    groups: &mut [Group],
    boundary: &mut dyn BatchBoundary,
    analysis: &mut Analysis,
) -> Result<(), WalkError> {
    let roots = DupRoots(subvolumes);
    let snapshot_only = Blockers::SNAPSHOT.with(Blockers::SHARED_TREE);
    let mut start = 0;
    while start < groups.len() {
        // A batch of groups whose extents are resolved together.
        let mut end = start;
        let mut wanted: Vec<u64> = Vec::new();
        while end < groups.len() && (end == start || wanted.len() < EXTENT_BATCH) {
            for &i in &members_of[end] {
                wanted.extend(
                    inodes[i]
                        .extents
                        .iter()
                        .filter(|e| e.bytenr != 0)
                        .map(|e| e.bytenr),
                );
            }
            end += 1;
        }
        wanted.sort_unstable();
        wanted.dedup();
        let mut refs: HashMap<u64, Option<ExtentRefs>> = HashMap::with_capacity(wanted.len());
        let stats = resolve(
            source,
            wanted.iter().copied(),
            Plan::Coalesce {
                max_gap: crate::scan::COALESCE_GAP,
            },
            boundary,
            |bytenr, r| {
                refs.insert(bytenr, r.cloned());
            },
        )?;
        analysis.reference_searches += stats.ranges;
        for g in start..end {
            let group = &mut groups[g];
            for (c, cluster) in group.clusters.iter_mut().enumerate() {
                let mut extents: BTreeMap<u64, (u64, Vec<Holder>)> = BTreeMap::new();
                for (k, &i) in members_of[g].iter().enumerate() {
                    if group.copies[k].cluster as usize != c {
                        continue;
                    }
                    let inode = &inodes[i];
                    let names = inode.names.len() as u64;
                    for e in inode.extents.iter().filter(|e| e.bytenr != 0) {
                        let entry = extents
                            .entry(e.bytenr)
                            .or_insert((e.disk_bytes, Vec::new()));
                        let holder = Holder {
                            tree: inode.candidate.tree,
                            objectid: inode.candidate.objectid,
                            external_names: u64::from(inode.nlink) > names,
                            shared_leaf: e.shared_leaf,
                        };
                        if !entry.1.contains(&holder) {
                            entry.1.push(holder);
                        }
                    }
                }
                for (bytenr, (disk_bytes, holders)) in extents {
                    let r = refs.get(&bytenr).and_then(|r| r.as_ref());
                    match classify(disk_bytes, &holders, r, &roots) {
                        Verdict::Reclaimable => cluster.free += disk_bytes,
                        Verdict::Retained(b) if b.bits() & !snapshot_only.bits() == 0 => {
                            cluster.later += disk_bytes
                        }
                        Verdict::Retained(_) => cluster.held += disk_bytes,
                        Verdict::Unknown => cluster.unknown += disk_bytes,
                    }
                }
            }
        }
        start = end;
    }
    Ok(())
}

/// Marks file groups whose copies all lie inside members of `folder`:
/// they are listed with the folder, not again on their own.
fn mark_covered(folder: &mut FolderGroup, groups: &mut [Group]) {
    if !folder.listed() {
        return;
    }
    let parts: Vec<Vec<u8>> = folder
        .members
        .iter()
        .flat_map(|m| folder.parts(m))
        .map(|(p, _, _)| p)
        .collect();
    let inside = |path: &[u8]| {
        parts.iter().any(|m| {
            path == m.as_slice() || path.starts_with(m) && path.get(m.len()) == Some(&b'/')
        })
    };
    for g in groups.iter_mut() {
        if g.copies.iter().all(|c| c.names().iter().all(|n| inside(n))) {
            g.in_folders = true;
            folder.covered += 1;
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::backref::DataBackref;
    use crate::btrfs::backref::testing::encode_data_extent;
    use crate::btrfs::backref::{EXTENT_ITEM_KEY, EXTENT_TREE_OBJECTID};
    use crate::btrfs::keys::FS_TREE_OBJECTID;
    use crate::btrfs::parser::testing::encode_inode;
    use crate::btrfs::parser::{BtrfsTimespec, InodeItem};
    use crate::btrfs::walk::testing::{FakeTrees, NoStop};
    use crate::duplicates::csum::testing::csum_items;
    use crate::model::testing::*;
    use crate::model::tree::{RootKind, ScopeTree};
    use crate::report::{Report, RootInfo};
    use crate::target::Scope;

    const MIB: u64 = 1 << 20;
    const SIZE: u64 = 20 * MIB;

    /// A file inode with a modification time.
    fn file(ino: u64, size: u64, mtime: u64, flags: u64) -> TreeItem {
        let item = InodeItem {
            generation: 1,
            transid: 1,
            size,
            nbytes: size,
            nlink: 1,
            uid: 0,
            gid: 0,
            mode: FILE,
            rdev: 0,
            flags,
            ctime: BtrfsTimespec::default(),
            mtime: BtrfsTimespec {
                sec: mtime,
                nsec: 0,
            },
        };
        (Key::new(ino, INODE_ITEM_KEY, 0), encode_inode(&item))
    }

    fn extent(bytenr: u64, len: u64, owners: &[u64]) -> TreeItem {
        let refs: Vec<DataBackref> = owners
            .iter()
            .map(|&o| DataBackref::Direct {
                root: FS_TREE_OBJECTID,
                objectid: o,
                offset: 0,
                count: 1,
            })
            .collect();
        (
            Key::new(bytenr, EXTENT_ITEM_KEY, len),
            encode_data_extent(owners.len() as u64, &refs),
        )
    }

    struct Fixture {
        source: FakeTrees,
        report: Report,
        candidates: Vec<Candidate>,
    }

    /// `/v` (tree 5):
    /// - `photo/a.mov` and `archive/a.mov`: real copies (A, B);
    /// - `archive/a-clone.mov`: a clone of `archive/a.mov`;
    /// - `archive/b.mov`: same size, other content;
    /// - `@eaDir/a.mov`: an app-side copy;
    /// - `nocow/n1.bin`, `nocow/n2.bin`: no checksums;
    /// - `f1` and `f2`: identical folders, each with `big.bin`,
    ///   `small.txt` and `sub/c.bin`.
    fn fixture() -> Fixture {
        fixture_with(b"0123456789", false)
    }

    /// An inline extent holding `data`.
    fn inline(ino: u64, data: &[u8]) -> TreeItem {
        let mut item = crate::btrfs::parser::testing::encode_extent_header(0);
        item.extend_from_slice(data);
        (Key::new(ino, EXTENT_DATA_KEY, 0), item)
    }

    /// `fixture`, with `f2/small.txt` holding `small` (`f1`'s holds
    /// `0123456789`) and a `.DS_Store` in `f1` only.
    fn fixture_with(small: &[u8], at_folder_in_f2: bool) -> Fixture {
        let mut fs = vec![inode(256, DIR, 0, 1)];
        let mut extents = Vec::new();
        let mut csums = Vec::new();
        let mut candidates = Vec::new();
        let mut next_dir = 300;
        let mut dirs: BTreeMap<String, u64> = BTreeMap::new();
        let mut index = 2;
        let mut dir_of = |path: &str, fs: &mut Vec<TreeItem>| -> u64 {
            let mut parent = 256;
            let mut at = String::new();
            for part in path.split('/') {
                at.push('/');
                at.push_str(part);
                parent = match dirs.get(&at) {
                    Some(&d) => d,
                    None => {
                        next_dir += 1;
                        fs.push(entry(parent, next_dir, part, next_dir));
                        fs.push(inode(next_dir, DIR, 0, 1));
                        dirs.insert(at.clone(), next_dir);
                        next_dir
                    }
                };
            }
            parent
        };
        // (path, ino, size, mtime, bytenr, csum seed or None, flags)
        type Spec = (&'static str, u64, u64, u64, u64, Option<u32>, u64);
        let files: &[Spec] = &[
            ("photo/a.mov", 1001, SIZE, 50, 1 << 30, Some(1), 0),
            ("archive/a.mov", 1002, SIZE, 10, 2 << 30, Some(1), 0),
            ("archive/a-clone.mov", 1003, SIZE, 60, 2 << 30, Some(1), 0),
            ("archive/b.mov", 1004, SIZE, 20, 3 << 30, Some(2), 0),
            ("@eaDir/a.mov", 1005, SIZE, 5, 4 << 30, Some(1), 0),
            (
                "nocow/n1.bin",
                1006,
                SIZE,
                1,
                5 << 30,
                None,
                INODE_NODATASUM,
            ),
            (
                "nocow/n2.bin",
                1007,
                SIZE,
                1,
                6 << 30,
                None,
                INODE_NODATASUM,
            ),
            ("f1/big.bin", 1008, 2 * MIB, 30, 7 << 30, Some(3), 0),
            ("f2/big.bin", 1009, 2 * MIB, 40, 8 << 30, Some(3), 0),
            ("f1/sub/c.bin", 1010, 9 * MIB, 30, 9 << 30, Some(4), 0),
            ("f2/sub/c.bin", 1011, 9 * MIB, 40, 10 << 30, Some(4), 0),
        ];
        for &(path, ino, size, mtime, bytenr, seed, flags) in files {
            let (dir, name) = path.rsplit_once('/').unwrap();
            let parent = dir_of(dir, &mut fs);
            index += 1;
            fs.push(entry(parent, index, name, ino));
            fs.push(file(ino, size, mtime, flags));
            fs.push(whole_extent(ino, 0, bytenr, size));
            let owners: Vec<u64> = files
                .iter()
                .filter(|f| f.4 == bytenr)
                .map(|f| f.1)
                .collect();
            if owners[0] == ino {
                extents.push(extent(bytenr, size, &owners));
                if let Some(seed) = seed {
                    csums.extend(csum_items(bytenr, (size / 4096) as u32, seed, 1024));
                }
            }
            candidates.push(Candidate {
                tree: FS_TREE_OBJECTID,
                objectid: ino,
                size,
                flags: 0,
                first_block: bytenr,
            });
        }
        for folder in ["f1", "f2"] {
            let parent = dir_of(folder, &mut fs);
            let (ino, data) = if folder == "f1" {
                (1020, b"0123456789".as_slice())
            } else {
                (1021, small)
            };
            index += 1;
            fs.push(entry(parent, index, "small.txt", ino));
            fs.push(file(ino, data.len() as u64, 1, 0));
            fs.push(inline(ino, data));
        }
        let parent = dir_of("f1", &mut fs);
        index += 1;
        fs.push(entry(parent, index, ".DS_Store", 1022));
        fs.push(file(1022, 6, 1, 0));
        fs.push(inline(1022, b"junk!!"));
        if at_folder_in_f2 {
            // An app folder inside, as an app or package keeps there:
            // excluded from the scan's user data, so f2 is incomplete.
            dir_of("f2/@cache", &mut fs);
        }
        let ns = read_tree(FS_TREE_OBJECTID, fs.clone()).unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v",
            RootKind::Volume,
            &SubvolumeSet::default(),
            &Default::default(),
        )
        .unwrap();
        tree.prepare();
        let header = crate::report::testing::header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: Scope::Volume,
            tree: FS_TREE_OBJECTID,
            ino: 256,
        }]);
        let report = Report::parse(crate::report::testing::encode(&header, &tree)).unwrap();
        Fixture {
            source: FakeTrees::new([
                (FS_TREE_OBJECTID, fs),
                (EXTENT_TREE_OBJECTID, extents),
                (csum::CSUM_TREE_OBJECTID, csums),
            ]),
            report,
            candidates,
        }
    }

    fn run(f: &mut Fixture) -> Analysis {
        run_with(f, &SubvolumeSet::default())
    }

    fn run_with(f: &mut Fixture, subvolumes: &SubvolumeSet) -> Analysis {
        analyze(
            &mut f.source,
            &mut &f.report,
            Inputs {
                mount: b"/v",
                candidates: f.candidates.clone(),
                candidates_truncated: false,
                subvolumes,
                layout: Layout::new(4096, 16384, 4),
                uncertain: Some(HashSet::new()),
            },
            &mut NoStop,
        )
        .unwrap()
    }

    fn paths(g: &Group) -> Vec<String> {
        g.copies
            .iter()
            .map(|c| String::from_utf8_lossy(c.path()).into_owned())
            .collect()
    }

    #[test]
    fn finds_real_copies_and_clones_by_checksum_and_leaves_app_copies_out() {
        let mut f = fixture();
        let a = run(&mut f);
        let movie = a.groups.iter().find(|g| g.name == b"a.mov").unwrap();
        assert_eq!(
            paths(movie),
            [
                "/v/archive/a-clone.mov",
                "/v/archive/a.mov",
                "/v/photo/a.mov"
            ]
        );
        // The clone and its twin are one cluster.
        let clusters: Vec<u32> = movie.copies.iter().map(|c| c.cluster).collect();
        assert_eq!(clusters[0], clusters[1]);
        assert_ne!(clusters[0], clusters[2]);
        // Deleting the clone pair frees its extent, the photo copy its own.
        let mut frees: Vec<u64> = movie.clusters.iter().map(|c| c.free).collect();
        frees.sort();
        assert_eq!(frees, [SIZE, SIZE]);
        assert_eq!(movie.category, Category::Videos);
        // Same size, other content: no group with b.mov.
        assert!(a.groups.iter().all(|g| g.name != b"b.mov"));
        assert_eq!(a.excluded, 1);
        assert_eq!(a.unchecked.files, 2);
        assert_eq!(a.unchecked.no_checksums, 2);
    }

    #[test]
    fn identical_folders_are_reported_at_the_topmost_level() {
        let mut f = fixture();
        let a = run(&mut f);
        let names: Vec<&[u8]> = a.folders.iter().map(|f| f.name.as_slice()).collect();
        assert_eq!(names, [b"f1".as_slice()]);
        let folder = &a.folders[0];
        let members: Vec<&[u8]> = folder.members.iter().map(|m| m.path.as_slice()).collect();
        assert_eq!(members, [b"/v/f1".as_slice(), b"/v/f2".as_slice()]);
        // .DS_Store neither counts nor makes the folders differ.
        assert_eq!((folder.files, folder.bytes), (3, 11 * MIB + 10));
        let frees: Vec<u64> = folder.clusters.iter().map(|c| c.free).collect();
        assert_eq!(frees, [11 * MIB, 11 * MIB]);
        assert_eq!((folder.members[0].oldest, folder.members[1].mtime), (1, 40));
        // Its files are listed with the folder, not again on their own.
        let big = a.groups.iter().find(|g| g.name == b"big.bin").unwrap();
        assert!(big.in_folders);
    }

    #[test]
    fn one_small_file_with_other_content_makes_folders_differ() {
        let mut f = fixture_with(b"0123456780", false);
        let a = run(&mut f);
        // The same names and sizes, so candidates; the inline data differs.
        assert!(a.folders.iter().all(|g| g.partial() || g.name != b"f1"));
        // The other items are the same and hold nearly every byte: partly
        // identical folders, whose row keeps small.txt.
        assert_eq!(a.folders.len(), 1);
        let rels: Vec<&[u8]> = a.folders[0]
            .items
            .iter()
            .map(|i| i.rel.as_slice())
            .collect();
        assert_eq!(rels, [b"big.bin".as_slice(), b"sub"]);
    }

    #[test]
    fn a_folder_with_an_at_folder_inside_is_never_identical() {
        let mut f = fixture_with(b"0123456789", true);
        let a = run(&mut f);
        assert!(a.folders.iter().all(|g| g.name != b"f1"));
        // `@eaDir` is junk, not an excluded folder; user dotfiles count.
        assert!(super::super::folders::is_junk(b"@eaDir"));
        assert!(super::super::folders::is_junk(b"._IMG_1.JPG"));
        assert!(!super::super::folders::is_junk(b".git"));
        assert!(!super::super::folders::is_junk(b"._"));
    }

    #[test]
    fn a_file_changed_since_the_scan_is_left_out() {
        let mut f = fixture();
        // The photo copy now starts elsewhere: rewritten after the scan.
        f.candidates[0].first_block += 4096;
        let a = run(&mut f);
        let movie = a.groups.iter().find(|g| g.size == SIZE).unwrap();
        assert_eq!(movie.copies.len(), 2);
        assert_eq!(a.changed, 1);
    }

    #[test]
    fn a_copy_a_snapshot_also_holds_frees_later() {
        let mut f = fixture();
        // Snapshot 400 (read-only) also references the photo copy.
        let extents = &mut f.source.trees.get_mut(&EXTENT_TREE_OBJECTID).unwrap().items;
        let at = extents
            .iter()
            .position(|(k, _)| k.objectid == 1 << 30)
            .unwrap();
        let refs = [
            DataBackref::Direct {
                root: FS_TREE_OBJECTID,
                objectid: 1001,
                offset: 0,
                count: 1,
            },
            DataBackref::Direct {
                root: 400,
                objectid: 1001,
                offset: 0,
                count: 1,
            },
        ];
        extents[at].1 = encode_data_extent(2, &refs);
        let mut snapshot = linked(400, FS_TREE_OBJECTID, 256, "snap");
        snapshot.readonly = true;
        let subvolumes = SubvolumeSet {
            subvolumes: vec![snapshot],
            ..SubvolumeSet::default()
        };
        let a = run_with(&mut f, &subvolumes);
        let movie = a.groups.iter().find(|g| g.size == SIZE).unwrap();
        let photo = movie
            .copies
            .iter()
            .find(|c| c.path() == b"/v/photo/a.mov")
            .unwrap();
        let cluster = movie.clusters[photo.cluster as usize];
        assert_eq!((cluster.free, cluster.later), (0, SIZE));
    }
}
