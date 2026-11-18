//! Partly identical folders (docs/duplicates.md "Partly identical
//! folders"): two folders whose identical items, at the same relative
//! paths, cover most of their bytes are one row. Deleting it removes only
//! those items.
//!
//! 1. **Pairs.** Two identical folders or files with one name in
//!    different folders are aligned: walk up while the folder names stay
//!    the same. The topmost aligned folders are a pair.
//! 2. **Items.** With every file below both sides confirmed by content,
//!    the identical items are the topmost folders whose files, folders
//!    and names are the same on both sides, and the identical files not
//!    inside one.
//! 3. **Threshold.** A pair is one row when both sides were fully
//!    scanned and its identical items cover at least
//!    [`PARTIAL_PERCENT`] of the smaller side's bytes. Otherwise the
//!    pair's aligned subfolders are tried, topmost first. A pair of at
//!    least [`SIMILAR_PERCENT`] is *similar*: its identical items are left
//!    to P2 and not listed as identical folders either.
//! 4. **Selection.** A folder is in at most one row; the pair with most
//!    identical bytes wins. A side that is a listed identical folder
//!    brings every copy of it: those copies are that group's to remove,
//!    so the row only removes from the other side.

use super::analysis::{DupRoots, hex};
use super::folders::{Big, Confirmed, account, ages, last_name, parent, put_name};
use super::model::{
    Analysis, Covered, FileStamp, FolderGroup, FolderMember, Item, LIST_MIN_SIZE, Side, within,
};
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::{BatchBoundary, WalkError};
use crate::sha256::Sha256;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Identical items covering at least this share (percent) of the smaller
/// side's bytes make two folders one row. The one threshold of the rule.
pub const PARTIAL_PERCENT: u64 = 80;
/// Below [`PARTIAL_PERCENT`] but at least this: similar folders, left to
/// P2 (docs/duplicates.md "For P2"); their identical items are not
/// listed either.
pub const SIMILAR_PERCENT: u64 = 50;
const _: () = assert!(SIMILAR_PERCENT < PARTIAL_PERCENT);
/// A row holds at least two items: one alone is an identical folder or
/// file row.
pub const MIN_ITEMS: usize = 2;
/// A row lists at most this many identical items; a pair with more is
/// tried at its subfolders.
pub const MAX_ITEMS: usize = 1000;

fn related(a: &[u8], b: &[u8]) -> bool {
    a == b || within(a, b) || within(b, a)
}

fn join(root: &[u8], rel: &[u8]) -> Vec<u8> {
    if rel.is_empty() {
        root.to_vec()
    } else {
        [root, b"/", rel].concat()
    }
}

/// Step 1: the topmost aligned folders above each pair of `seeds` (paths
/// of two identical items), at least [`LIST_MIN_SIZE`] and not excluded.
pub(super) fn tops<'a>(
    seeds: impl Iterator<Item = (&'a [u8], &'a [u8])>,
    mount: &[u8],
    big: &HashMap<Vec<u8>, Big>,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = BTreeSet::new();
    for (a, b) in seeds {
        if last_name(a) != last_name(b) {
            continue;
        }
        let (mut p, mut q) = (parent(a), parent(b));
        if !within(p, mount) || !within(q, mount) || related(p, q) {
            continue;
        }
        loop {
            let (pp, qp) = (parent(p), parent(q));
            if last_name(p) != last_name(q)
                || !within(pp, mount)
                || !within(qp, mount)
                || related(pp, qp)
            {
                break;
            }
            (p, q) = (pp, qp);
        }
        let ok = |x: &[u8]| big.get(x).is_some_and(|b| !b.excluded);
        if ok(p) && ok(q) {
            out.insert(if p < q {
                (p.to_vec(), q.to_vec())
            } else {
                (q.to_vec(), p.to_vec())
            });
        }
    }
    out.into_iter().collect()
}

/// One side of a pair: every file and folder below `root`, by path
/// relative to it (no leading `/`).
struct Index {
    root: Vec<u8>,
    tree: u64,
    objectid: u64,
    files: BTreeMap<Vec<u8>, u32>,
    dirs: BTreeMap<Vec<u8>, u32>,
}

impl Index {
    fn of(tree: &Confirmed, m: usize, root: &[u8]) -> Option<Index> {
        let member = &tree.members[m];
        let (t, o) = if root == member.path.as_slice() {
            (member.tree, member.objectid)
        } else {
            let d = member
                .dirs
                .iter()
                .map(|&d| &tree.dirs[d as usize])
                .find(|d| d.path == root)?;
            (d.tree, d.objectid)
        };
        let prefix = [root, b"/"].concat();
        let rel = |path: &[u8]| path.strip_prefix(prefix.as_slice()).map(|r| r.to_vec());
        Some(Index {
            root: root.to_vec(),
            tree: t,
            objectid: o,
            files: member
                .files
                .iter()
                .filter_map(|&e| Some((rel(&tree.entries[e as usize].path)?, e)))
                .collect(),
            dirs: member
                .dirs
                .iter()
                .filter_map(|&d| Some((rel(&tree.dirs[d as usize].path)?, d)))
                .collect(),
        })
    }

    /// The files below `rel` (a folder), or `rel` itself (a file).
    fn files_of(&self, rel: &[u8], folder: bool) -> Vec<u32> {
        if !folder {
            return self.files.get(rel).copied().into_iter().collect();
        }
        let lo = [rel, b"/"].concat();
        let hi = [rel, b"0"].concat();
        self.files.range(lo..hi).map(|(_, &e)| e).collect()
    }

    /// The folder `rel` and every folder below it.
    fn dirs_of(&self, rel: &[u8]) -> Vec<u32> {
        let lo = [rel, b"/"].concat();
        let hi = [rel, b"0"].concat();
        self.dirs
            .get(rel)
            .copied()
            .into_iter()
            .chain(self.dirs.range(lo..hi).map(|(_, &d)| d))
            .collect()
    }

    fn bytes(&self, tree: &Confirmed) -> u64 {
        self.files
            .values()
            .map(|&e| tree.entries[e as usize].size)
            .sum()
    }
}

/// A pair of aligned folders whose identical items reach the threshold.
struct Pair {
    a: Vec<u8>,
    b: Vec<u8>,
    /// The members holding each side.
    ma: usize,
    mb: usize,
    /// The identical items, by path below the pair, and whether each is a
    /// folder.
    items: Vec<(Vec<u8>, bool)>,
    same_bytes: u64,
}

/// Every ancestor folder of `rel` (relative), the top (`""`) included.
fn ancestors(rel: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::once(&rel[..0]).chain(
        rel.iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'/')
            .map(move |(i, _)| &rel[..i]),
    )
}

/// Steps 2 and 3 for one top pair: the pairs that reach the threshold,
/// topmost only, and the identical items of similar pairs.
fn descend(
    tree: &Confirmed,
    big: &HashMap<Vec<u8>, Big>,
    (ma, mb): (usize, usize),
    pairs: &mut Vec<Pair>,
    similar: &mut Vec<Vec<u8>>,
) {
    let (Some(ia), Some(ib)) = (
        Index::of(tree, ma, &tree.members[ma].path),
        Index::of(tree, mb, &tree.members[mb].path),
    ) else {
        return;
    };
    let same_file = |rel: &[u8], ea: u32| {
        ib.files.get(rel).is_some_and(|&eb| {
            let (a, b) = (&tree.entries[ea as usize], &tree.entries[eb as usize]);
            a.kind == b.kind
                && tree.sig[ea as usize].is_some()
                && tree.sig[ea as usize] == tree.sig[eb as usize]
        })
    };
    let mut differ: HashSet<&[u8]> = HashSet::new();
    let mut same: HashSet<&[u8]> = HashSet::new();
    let mut bytes_a: HashMap<&[u8], u64> = HashMap::new();
    let mut bytes_b: HashMap<&[u8], u64> = HashMap::new();
    let mut same_bytes: HashMap<&[u8], u64> = HashMap::new();
    let mut children: HashMap<&[u8], Vec<(&[u8], bool)>> = HashMap::new();
    // Files and folders that differ; their folders differ too.
    let mut mark_all: Vec<&[u8]> = Vec::new();
    for (rel, &e) in &ia.files {
        let size = tree.entries[e as usize].size;
        for anc in ancestors(rel) {
            *bytes_a.entry(anc).or_default() += size;
        }
        children
            .entry(parent(rel))
            .or_default()
            .push((rel.as_slice(), false));
        if same_file(rel, e) {
            same.insert(rel.as_slice());
            for anc in ancestors(rel) {
                *same_bytes.entry(anc).or_default() += size;
            }
        } else {
            mark_all.push(rel.as_slice());
        }
    }
    for (rel, &e) in &ib.files {
        let size = tree.entries[e as usize].size;
        for anc in ancestors(rel) {
            *bytes_b.entry(anc).or_default() += size;
        }
        if !same.contains(rel.as_slice()) {
            mark_all.push(rel.as_slice());
        }
    }
    for rel in ia.dirs.keys() {
        children
            .entry(parent(rel))
            .or_default()
            .push((rel.as_slice(), true));
        if !ib.dirs.contains_key(rel) {
            mark_all.push(rel.as_slice());
        }
    }
    for rel in ib.dirs.keys() {
        if !ia.dirs.contains_key(rel) {
            mark_all.push(rel.as_slice());
        }
    }
    for rel in mark_all {
        differ.extend(ancestors(rel));
        differ.insert(rel);
    }
    if !differ.contains(&b""[..]) {
        // Identical as a whole: an identical folder, not a pair.
        return;
    }
    let complete = |path: &[u8]| big.get(path).is_some_and(|b| b.complete && !b.excluded);
    let mut stack: Vec<&[u8]> = vec![b""];
    while let Some(d) = stack.pop() {
        let small = bytes_a
            .get(d)
            .copied()
            .unwrap_or(0)
            .min(bytes_b.get(d).copied().unwrap_or(0));
        if small < LIST_MIN_SIZE {
            continue;
        }
        // The topmost identical items below d.
        let mut items: Vec<(&[u8], bool)> = Vec::new();
        let mut walk: Vec<&[u8]> = vec![d];
        while let Some(at) = walk.pop() {
            for &(c, folder) in children.get(at).map_or(&[][..], |v| v.as_slice()) {
                if !differ.contains(c) {
                    items.push((c, folder));
                } else if folder {
                    walk.push(c);
                }
            }
        }
        items.sort();
        let same = same_bytes.get(d).copied().unwrap_or(0);
        let (a, b) = (join(&ia.root, d), join(&ib.root, d));
        let whole = complete(&a) && complete(&b);
        if whole
            && same * 100 >= PARTIAL_PERCENT * small
            && same >= LIST_MIN_SIZE
            && (MIN_ITEMS..=MAX_ITEMS).contains(&items.len())
        {
            let below = |rel: &[u8]| {
                if d.is_empty() {
                    rel.to_vec()
                } else {
                    rel[d.len() + 1..].to_vec()
                }
            };
            pairs.push(Pair {
                a,
                b,
                ma,
                mb,
                items: items.iter().map(|&(r, f)| (below(r), f)).collect(),
                same_bytes: same,
            });
            continue;
        }
        if whole && same * 100 >= SIMILAR_PERCENT * small && items.len() >= MIN_ITEMS {
            for &(r, _) in &items {
                similar.push(join(&ia.root, r));
                similar.push(join(&ib.root, r));
            }
        }
        for &(c, folder) in children.get(d).map_or(&[][..], |v| v.as_slice()) {
            if folder && differ.contains(c) && ib.dirs.contains_key(c) {
                stack.push(c);
            }
        }
    }
}

/// What step 4 leaves.
pub(super) struct Partial {
    pub(super) groups: Vec<FolderGroup>,
    /// Identical items of similar folders: not listed in P1.
    pub(super) similar: Vec<Vec<u8>>,
}

/// Steps 2 to 4 for every top pair; `whole` are the identical folders.
#[allow(clippy::too_many_arguments)]
pub(super) fn find<S: MetadataSource + ?Sized>(
    source: &mut S,
    roots: &DupRoots<'_>,
    tree: &Confirmed,
    tops: &[(usize, usize)],
    big: &HashMap<Vec<u8>, Big>,
    mount: &[u8],
    whole: &[FolderGroup],
    boundary: &mut dyn BatchBoundary,
    analysis: &mut Analysis,
) -> Result<Partial, WalkError> {
    let mut pairs = Vec::new();
    let mut similar = Vec::new();
    for &top in tops {
        descend(tree, big, top, &mut pairs, &mut similar);
    }
    // A side that is, or lies in, a listed identical folder brings the
    // same path in every copy of it: the group and the path below.
    let mut class: HashMap<&[u8], usize> = HashMap::new();
    for (g, f) in whole.iter().enumerate().filter(|(_, f)| f.listed()) {
        for m in &f.members {
            class.insert(&m.path, g);
        }
    }
    let mut whole_paths: Vec<&[u8]> = class.keys().copied().collect();
    whole_paths.sort();
    let class_of = |path: &[u8]| -> Option<(usize, Vec<u8>)> {
        let mut at = path;
        while !at.is_empty() {
            if let Some(&g) = class.get(at) {
                return Some((g, path[at.len()..].to_vec()));
            }
            at = parent(at);
        }
        None
    };
    let has_class = |p: &Pair| class_of(&p.a).is_some() || class_of(&p.b).is_some();
    pairs.sort_by(|x, y| {
        (y.same_bytes, has_class(y))
            .cmp(&(x.same_bytes, has_class(x)))
            .then((&x.a, &x.b).cmp(&(&y.a, &y.b)))
    });
    let by_path: HashMap<&[u8], usize> = tree
        .members
        .iter()
        .enumerate()
        .map(|(i, m)| (m.path.as_slice(), i))
        .collect();
    let mut taken: Vec<Vec<u8>> = Vec::new();
    let mut groups = Vec::new();
    for pair in &pairs {
        let (ca, cb) = (class_of(&pair.a), class_of(&pair.b));
        if ca.is_some() && cb.is_some() {
            continue;
        }
        let free: Vec<(&[u8], usize)> = [(&pair.a, pair.ma, &ca), (&pair.b, pair.mb, &cb)]
            .into_iter()
            .filter(|(_, _, c)| c.is_none())
            .map(|(p, m, _)| (p.as_slice(), m))
            .collect();
        let fits = free.iter().all(|&(f, _)| {
            if taken.iter().any(|t| related(t, f)) {
                return false;
            }
            // An identical folder inside must be one of the items, or
            // inside one: else both rows would remove its files.
            let lo = [f, b"/"].concat();
            let start = whole_paths.partition_point(|w| *w < lo.as_slice());
            whole_paths[start..]
                .iter()
                .take_while(|w| w.starts_with(&lo))
                .all(|w| {
                    let rel = &w[lo.len()..];
                    pair.items.iter().any(|(item, folder)| {
                        *folder && (rel == item.as_slice() || within(rel, item))
                    })
                })
        });
        if !fits {
            continue;
        }
        let mut sides: Vec<(usize, Vec<u8>)> = free.iter().map(|&(p, m)| (m, p.to_vec())).collect();
        if let Some((g, rel)) = ca.or(cb) {
            for m in &whole[g].members {
                if let Some(&i) = by_path.get(m.path.as_slice()) {
                    sides.push((i, [m.path.as_slice(), &rel].concat()));
                }
            }
        }
        sides.sort_by(|x, y| x.1.cmp(&y.1));
        if let Some(group) = build(
            source,
            roots,
            tree,
            mount,
            &pair.items,
            &sides,
            free[0].0,
            boundary,
            analysis,
        )? {
            taken.extend(free.iter().map(|&(p, _)| p.to_vec()));
            groups.push(group);
        }
    }
    Ok(Partial { groups, similar })
}

/// The row of partly identical folders `sides` (member, path), with the
/// identical `items` below each.
#[allow(clippy::too_many_arguments)]
fn build<S: MetadataSource + ?Sized>(
    source: &mut S,
    roots: &DupRoots<'_>,
    tree: &Confirmed,
    mount: &[u8],
    items: &[(Vec<u8>, bool)],
    sides: &[(usize, Vec<u8>)],
    name: &[u8],
    boundary: &mut dyn BatchBoundary,
    analysis: &mut Analysis,
) -> Result<Option<FolderGroup>, WalkError> {
    let mut indexes = Vec::with_capacity(sides.len());
    for (m, root) in sides {
        match Index::of(tree, *m, root) {
            Some(i) => indexes.push(i),
            None => return Ok(None),
        }
    }
    let mut files: Vec<Vec<u32>> = Vec::with_capacity(sides.len());
    let mut ids: Vec<Vec<(u64, u64)>> = Vec::with_capacity(sides.len());
    let mut dirs: Vec<Vec<Vec<u8>>> = Vec::with_capacity(sides.len());
    for index in &indexes {
        let (mut f, mut i, mut d) = (Vec::new(), Vec::new(), Vec::new());
        for (rel, folder) in items {
            let objectid = if *folder {
                match index.dirs.get(rel) {
                    Some(&dir) => tree.dirs[dir as usize].objectid,
                    None => return Ok(None),
                }
            } else {
                match index.files.get(rel) {
                    Some(&e) => tree.entries[e as usize].objectid,
                    None => return Ok(None),
                }
            };
            let generation = tree.found(index.tree, objectid).map_or(0, |f| f.generation);
            i.push((objectid, generation));
            f.extend(index.files_of(rel, *folder));
            if *folder {
                d.extend(
                    index
                        .dirs_of(rel)
                        .into_iter()
                        .map(|dir| tree.dirs[dir as usize].path.clone()),
                );
            }
        }
        files.push(f);
        ids.push(i);
        dirs.push(d);
    }
    let first = &indexes[0];
    let group_items: Vec<Item> = items
        .iter()
        .map(|(rel, folder)| {
            let fs = first.files_of(rel, *folder);
            Item {
                rel: rel.clone(),
                folder: *folder,
                files: fs.len() as u64,
                bytes: fs.iter().map(|&e| tree.entries[e as usize].size).sum(),
            }
        })
        .collect();
    let (cluster_of, clusters) = account(source, roots, tree, &files, boundary, analysis)?;
    // Stable while the sides and the items' content are.
    let mut hash = Sha256::new();
    hash.update(b"partial");
    for (_, root) in sides {
        put_name(&mut hash, root);
    }
    for (rel, folder) in items {
        hash.update(&[u8::from(*folder)]);
        put_name(&mut hash, rel);
    }
    let mut order = files[0].clone();
    order.sort_by(|&a, &b| {
        tree.entries[a as usize]
            .path
            .cmp(&tree.entries[b as usize].path)
    });
    for e in order {
        hash.update(&tree.sig[e as usize].unwrap_or_default());
    }
    let id = format!("f{}", hex(&hash.finish()[..16]));
    let members = indexes
        .iter()
        .enumerate()
        .map(|(k, index)| {
            let bytes = files[k]
                .iter()
                .map(|&e| tree.entries[e as usize].size)
                .sum();
            let mut fm = FolderMember::new(&index.root, mount, files[k].len() as u64, bytes);
            let (newest, oldest) = ages(tree, &files[k]);
            fm.mtime = newest;
            fm.oldest = oldest;
            fm.tree = index.tree;
            fm.objectid = index.objectid;
            fm.generation = tree
                .found(index.tree, index.objectid)
                .map_or(0, |f| f.generation);
            fm.cluster = cluster_of[k];
            fm.dirs = std::mem::take(&mut dirs[k]);
            fm.file_stamps = files[k]
                .iter()
                .filter_map(|&e| {
                    let entry = &tree.entries[e as usize];
                    tree.found(entry.tree, entry.objectid).map(|f| FileStamp {
                        rel: entry.path[index.root.len()..].to_vec(),
                        stamp: f.stamp,
                    })
                })
                .collect();
            let direct = |rel: &Vec<u8>| !rel.contains(&b'/');
            fm.side = Some(Side {
                total_items: (index.files.keys().filter(|r| direct(r)).count()
                    + index.dirs.keys().filter(|r| direct(r)).count())
                    as u64,
                same_items: items.iter().filter(|(r, _)| direct(r)).count() as u64,
                total_files: index.files.len() as u64,
                total_bytes: index.bytes(tree),
                items: std::mem::take(&mut ids[k]),
            });
            fm
        })
        .collect();
    Ok(Some(FolderGroup {
        id,
        name: last_name(name).to_vec(),
        files: group_items.iter().map(|i| i.files).sum(),
        bytes: group_items.iter().map(|i| i.bytes).sum(),
        covered: 0,
        members,
        clusters,
        items: group_items,
        in_folders: false,
        stamps: None,
    }))
}

/// Takes out identical folders whose every copy lies in a part another
/// listed folder group removes, or is an identical item of `similar`
/// folders.
pub(super) fn settle(groups: &mut [FolderGroup], similar: &[Vec<u8>]) {
    let mut similar = similar.to_vec();
    similar.sort();
    similar.dedup();
    let in_similar = |path: &[u8]| {
        let mut at = path;
        while !at.is_empty() {
            if similar.binary_search_by(|s| s.as_slice().cmp(at)).is_ok() {
                return true;
            }
            at = parent(at);
        }
        false
    };
    let covered = Covered::of(groups);
    let out: Vec<bool> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            !g.partial()
                && g.listed()
                && g.members
                    .iter()
                    .all(|m| covered.holds(&m.path, Some(i)) || in_similar(&m.path))
        })
        .collect();
    for (g, out) in groups.iter_mut().zip(out) {
        g.in_folders |= out;
    }
}

#[cfg(test)]
mod tests {
    use super::super::analysis::{Inputs, analyze};
    use super::super::candidates::{Candidate, MIN_SIZE};
    use super::super::csum::testing::csum_items;
    use super::super::csum::{self, Layout};
    use super::super::model::{
        Amount, Decisions, Role, Settings, folder_members, mark_folder_members, view,
    };
    use super::*;
    use crate::btrfs::backref::testing::encode_data_extent;
    use crate::btrfs::backref::{DataBackref, EXTENT_ITEM_KEY, EXTENT_TREE_OBJECTID};
    use crate::btrfs::keys::{FS_TREE_OBJECTID, Key};
    use crate::btrfs::parser::testing::encode_inode;
    use crate::btrfs::parser::{BtrfsTimespec, InodeItem};
    use crate::btrfs::subvolume::SubvolumeSet;
    use crate::btrfs::walk::testing::{FakeTrees, NoStop};
    use crate::model::testing::*;
    use crate::model::tree::{RootKind, ScopeTree};
    use crate::report::{Report, RootInfo};
    use crate::target::Scope;

    const MIB: u64 = 1 << 20;

    /// A volume `/v` (tree 5) built file by file; files with one
    /// `content` have the same checksums, each in its own extent.
    #[derive(Default)]
    struct Volume {
        fs: Vec<TreeItem>,
        extents: Vec<TreeItem>,
        csums: Vec<TreeItem>,
        candidates: Vec<Candidate>,
        dirs: BTreeMap<String, u64>,
        next: u64,
        bytenr: u64,
    }

    impl Volume {
        fn new() -> Self {
            Volume {
                fs: vec![inode(256, DIR, 0, 1)],
                next: 300,
                bytenr: 1 << 30,
                ..Volume::default()
            }
        }

        fn dir(&mut self, path: &str) -> u64 {
            let mut parent = 256;
            let mut at = String::new();
            for part in path.split('/') {
                at.push('/');
                at.push_str(part);
                parent = match self.dirs.get(&at) {
                    Some(&d) => d,
                    None => {
                        self.next += 1;
                        let d = self.next;
                        self.fs.push(entry(parent, d, part, d));
                        self.fs.push(inode(d, DIR, 0, 1));
                        self.dirs.insert(at.clone(), d);
                        d
                    }
                };
            }
            parent
        }

        fn file(&mut self, path: &str, size: u64, content: u32) -> &mut Self {
            let (dir, name) = path.rsplit_once('/').unwrap();
            let parent = self.dir(dir);
            self.next += 1;
            let ino = self.next;
            self.fs.push(entry(parent, ino, name, ino));
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
                flags: 0,
                ctime: BtrfsTimespec::default(),
                mtime: BtrfsTimespec { sec: 10, nsec: 0 },
            };
            self.fs.push((
                Key::new(ino, crate::btrfs::keys::INODE_ITEM_KEY, 0),
                encode_inode(&item),
            ));
            let bytenr = self.bytenr;
            self.bytenr += size.next_multiple_of(MIB);
            self.fs.push(whole_extent(ino, 0, bytenr, size));
            let refs = [DataBackref::Direct {
                root: FS_TREE_OBJECTID,
                objectid: ino,
                offset: 0,
                count: 1,
            }];
            self.extents.push((
                Key::new(bytenr, EXTENT_ITEM_KEY, size),
                encode_data_extent(1, &refs),
            ));
            self.csums.extend(csum_items(
                bytenr,
                (size / 4096) as u32,
                content * 100_000,
                1024,
            ));
            if size >= MIN_SIZE {
                self.candidates.push(Candidate {
                    tree: FS_TREE_OBJECTID,
                    objectid: ino,
                    size,
                    flags: 0,
                    first_block: bytenr,
                });
            }
            self
        }

        /// The same files below `to` as below `from` (other extents).
        fn copy(&mut self, files: &[(&str, u64, u32)], to: &str) -> &mut Self {
            for &(rel, size, content) in files {
                self.file(&format!("{to}/{rel}"), size, content);
            }
            self
        }

        fn analyze(&self) -> Analysis {
            let ns = read_tree(FS_TREE_OBJECTID, self.fs.clone()).unwrap();
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
            let mut source = FakeTrees::new([
                (FS_TREE_OBJECTID, self.fs.clone()),
                (EXTENT_TREE_OBJECTID, self.extents.clone()),
                (csum::CSUM_TREE_OBJECTID, self.csums.clone()),
            ]);
            let mut a = analyze(
                &mut source,
                &mut &report,
                Inputs {
                    mount: b"/v",
                    candidates: self.candidates.clone(),
                    candidates_truncated: false,
                    subvolumes: &SubvolumeSet::default(),
                    layout: Layout::new(4096, 16384, 4),
                    uncertain: Some(HashSet::new()),
                },
                &mut NoStop,
            )
            .unwrap();
            a.mount = b"/v".to_vec();
            a.retally(&Settings::default());
            a
        }
    }

    /// A worktree: `a` (12 MiB, an identical folder on its own), `b`,
    /// `c` and `f1.txt`, 18 MiB.
    const TREE: &[(&str, u64, u32)] = &[
        ("a/x.bin", 6 * MIB, 1),
        ("a/y.bin", 6 * MIB, 2),
        ("b/z.bin", 3 * MIB, 3),
        ("c/w.bin", 2 * MIB, 4),
        ("f1.txt", MIB, 5),
    ];

    fn listed(a: &Analysis) -> Vec<&FolderGroup> {
        a.folders.iter().filter(|f| f.listed()).collect()
    }

    fn paths(f: &FolderGroup) -> Vec<String> {
        f.members
            .iter()
            .map(|m| String::from_utf8_lossy(&m.path).into_owned())
            .collect()
    }

    fn rels(f: &FolderGroup) -> Vec<String> {
        f.items
            .iter()
            .map(|i| String::from_utf8_lossy(&i.rel).into_owned())
            .collect()
    }

    /// A clone (the worktree and `.git`) and an export of it (the
    /// worktree alone): one row, not one per child.
    fn clone_and_export() -> Analysis {
        let mut v = Volume::new();
        v.copy(TREE, "s1/linux")
            .file("s1/linux/.git/pack", 3 * MIB, 6)
            .copy(TREE, "s2/copy");
        v.analyze()
    }

    #[test]
    fn folders_differing_by_one_subfolder_are_one_row() {
        let a = clone_and_export();
        let rows = listed(&a);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        let f = rows[0];
        assert!(f.partial());
        assert_eq!(paths(f), ["/v/s1/linux", "/v/s2/copy"]);
        assert_eq!(rels(f), ["a", "b", "c", "f1.txt"]);
        assert_eq!((f.files, f.bytes), (5, 18 * MIB));
        let sides: Vec<(u64, u64, u64)> = f
            .members
            .iter()
            .map(|m| {
                let s = m.side.as_ref().unwrap();
                (s.same_items, s.total_items, s.total_bytes)
            })
            .collect();
        assert_eq!(sides, [(4, 5, 21 * MIB), (4, 4, 18 * MIB)]);
        // `a` alone is an identical folder, but inside the row.
        let alone = a.folders.iter().find(|g| g.name == b"a").unwrap();
        assert!(!alone.listed() && alone.in_folders);
    }

    /// Totals count each identical byte once: the row frees the 18 MiB of
    /// the side it deletes from, and `a` adds nothing.
    #[test]
    fn totals_of_partly_identical_folders_are_exact() {
        let a = clone_and_export();
        let settings = Settings::default();
        let total = a.tally(b"/v", &settings).all();
        assert_eq!((total.copies, total.can_free), (1, 18 * MIB as i64));
        let covered = Covered::of(&a.folders);
        for scope in ["/v", "/v/s1", "/v/s2", "/v/s1/linux"] {
            let mut expected = Amount::default();
            for (i, f) in a.folders.iter().enumerate().filter(|(_, f)| f.listed()) {
                if !f
                    .members
                    .iter()
                    .any(|m| super::super::model::within(&m.path, scope.as_bytes()))
                {
                    continue;
                }
                let mut ms = folder_members(f);
                mark_folder_members(&mut ms, &covered, i);
                let v = view(&ms, &f.clusters, b"/v", &[], &[]);
                expected.copies += v.removes as i64;
                expected.can_free += v.can_free as i64;
            }
            let got = a.tally(scope.as_bytes(), &settings).all();
            assert_eq!(
                (got.copies, got.can_free),
                (expected.copies, expected.can_free),
                "{scope}"
            );
        }
        // The large files inside the items are the row's, not their own.
        assert!(a.groups.iter().all(|g| g.in_folders));
    }

    /// Deleting the row removes each identical item, by its inode, and
    /// never the folder itself, `.git` or anything that differs.
    #[test]
    fn deleting_the_row_removes_only_the_identical_items() {
        let a = clone_and_export();
        let f = listed(&a)[0];
        let clone = &f.members[0];
        let parts: Vec<String> = f
            .parts(clone)
            .iter()
            .map(|(p, _, _)| String::from_utf8_lossy(p).into_owned())
            .collect();
        assert_eq!(
            parts,
            [
                "/v/s1/linux/a",
                "/v/s1/linux/b",
                "/v/s1/linux/c",
                "/v/s1/linux/f1.txt"
            ]
        );
        let ids: HashSet<u64> = f.parts(clone).iter().map(|p| p.1).collect();
        assert_eq!(ids.len(), 4);
        // A kept side checks only its identical files again.
        let copy = &f.members[1];
        let kept: Vec<&[u8]> = copy.file_stamps.iter().map(|s| s.rel.as_slice()).collect();
        assert_eq!(
            kept,
            [
                b"/a/x.bin".as_slice(),
                b"/a/y.bin",
                b"/b/z.bin",
                b"/c/w.bin",
                b"/f1.txt"
            ]
        );
        assert!(
            clone
                .file_stamps
                .iter()
                .all(|s| !s.rel.starts_with(b"/.git"))
        );
        assert!(clone.dirs.iter().all(|d| !d.ends_with(b"/.git")));
    }

    /// Identical items covering half of each folder: similar folders,
    /// for P2. Nothing is listed, not even the identical subfolder `a`.
    #[test]
    fn a_pair_near_half_identical_lists_nothing() {
        let mut v = Volume::new();
        let same: &[(&str, u64, u32)] = &[
            ("a/x.bin", 5 * MIB, 1),
            ("a/y.bin", 5 * MIB, 2),
            ("b/z.bin", MIB, 3),
        ];
        // 11 of 22 and 23 MiB identical.
        v.copy(same, "s1/p")
            .file("s1/p/c/big.bin", 11 * MIB, 7)
            .copy(same, "s2/q")
            .file("s2/q/c/big.bin", 12 * MIB, 8);
        let a = v.analyze();
        assert!(listed(&a).is_empty(), "{:#?}", listed(&a));
        let alone = a.folders.iter().find(|g| g.name == b"a").unwrap();
        assert!(alone.in_folders);
        assert_eq!(a.tally(b"/v", &Settings::default()).all().copies, 0);
    }

    /// Nested pairs reaching the threshold: only the topmost is a row;
    /// below a pair that does not, its subfolders are tried.
    #[test]
    fn only_the_topmost_pair_is_a_row() {
        let tree: &[(&str, u64, u32)] = &[
            ("mid/a/x.bin", 5 * MIB, 1),
            ("mid/b/y.bin", 5 * MIB, 2),
            ("d/z.bin", 6 * MIB, 3),
        ];
        let mut v = Volume::new();
        v.copy(tree, "s1/top")
            .file("s1/top/mid/c.bin", MIB, 4)
            .copy(tree, "s2/top2")
            .file("s2/top2/mid/c.bin", MIB, 5);
        let a = v.analyze();
        let rows = listed(&a);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(paths(rows[0]), ["/v/s1/top", "/v/s2/top2"]);
        assert_eq!(rels(rows[0]), ["d", "mid/a", "mid/b"]);
        // A large folder that differs: the top pair is far below the
        // threshold, so the row is the subfolder pair.
        v.file("s1/top/e/big.bin", 40 * MIB, 8)
            .file("s2/top2/e/big.bin", 40 * MIB, 9);
        let a = v.analyze();
        let rows = listed(&a);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(paths(rows[0]), ["/v/s1/top/mid", "/v/s2/top2/mid"]);
        assert_eq!(rels(rows[0]), ["a", "b"]);
    }

    /// A clone beside two identical exports: the exports are an identical
    /// folder, and the clone's row brings both, removing only from the
    /// clone. Totals count each byte once.
    #[test]
    fn a_side_that_is_an_identical_folder_brings_its_copies() {
        let mut v = Volume::new();
        v.copy(TREE, "s1/linux")
            .file("s1/linux/.git/pack", 3 * MIB, 6)
            .copy(TREE, "s2/copy")
            .copy(TREE, "s3/copy");
        let a = v.analyze();
        let rows = listed(&a);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        // The shares hold only the exports: they are the identical folder.
        let whole = rows.iter().find(|f| !f.partial()).unwrap();
        assert_eq!(paths(whole), ["/v/s2", "/v/s3"]);
        let part = rows.iter().find(|f| f.partial()).unwrap();
        assert_eq!(paths(part), ["/v/s1/linux", "/v/s2/copy", "/v/s3/copy"]);
        let i = a.folders.iter().position(|f| f.id == part.id).unwrap();
        let settings = Settings::default();
        let (ms, v) = Decisions::new(&a, &settings).folder(i);
        let in_folder: Vec<bool> = ms.iter().map(|m| m.in_folder).collect();
        assert_eq!(in_folder, [false, true, true]);
        // The row keeps an export, which stays with its shared folder.
        assert_eq!(v.roles[0], Role::Remove);
        assert_eq!((v.removes, v.can_free), (1, 18 * MIB));
        // The identical folder is two shared folders themselves: neither
        // can move, so only the row's side goes.
        let total = a.tally(b"/v", &Settings::default()).all();
        assert_eq!((total.copies, total.can_free), (1, 18 * MIB as i64));
    }
}
