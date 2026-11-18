//! A scan decomposed into units (SPEC 19 Phase 3): one unit per
//! subvolume tree's selected scope, nested where a subvolume is linked.
//! Each unit is materialized and spilled on its own; extents are then
//! judged across all units in one merged pass, and each unit is
//! aggregated from its judgements, children first, into a report
//! fragment that the report splices in preorder.

use crate::codec::{Decoder, Fnv, HashingWriter, push_varint};
use crate::model::accounting::{Blockers, HeldUse, Holder, Judgement, Verdict};
use crate::model::extent::ExtentUse;
use crate::model::tree::{Metrics, Owner};
use crate::report::{NodeRecord, decode_record, encode_record};
use crate::spill::{Appender, ChunkReader, MAX_RECORD};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

/// What a sealed unit was built from: equal keys build equal units.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnitKey {
    pub subvolume: u64,
    /// Subvolume UUID in hex; empty when its root item has none.
    pub uuid: String,
    pub generation: u64,
    pub readonly: bool,
    /// Which scope of the subvolume: one bundle is kept per slot.
    pub slot: u64,
    /// Hash of everything else the unit is built from.
    pub inputs: u64,
}

impl UnitKey {
    /// A file name unique to the key.
    pub fn name(&self) -> String {
        let mut hash = Fnv::new();
        hash.update(&self.subvolume.to_le_bytes());
        hash.update(self.uuid.as_bytes());
        hash.update(&self.generation.to_le_bytes());
        hash.update(&[u8::from(self.readonly)]);
        hash.update(&self.slot.to_le_bytes());
        hash.update(&self.inputs.to_le_bytes());
        format!("{:016x}", hash.value())
    }
}

/// Where a unit sits in the unit forest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placement {
    /// The parent unit and the boundary node there; `None` for a unit of
    /// selected roots.
    pub parent: Option<(usize, u32)>,
}

const EXTERNAL_NAMES: u8 = 1 << 5;
const SEVERAL_NAMES: u8 = 1 << 6;

/// Appends one held use to a unit's extents file.
fn put_held(out: &mut Vec<u8>, last: &mut u64, u: &HeldUse) {
    push_varint(out, u.extent.bytenr.wrapping_sub(*last));
    *last = u.extent.bytenr;
    push_varint(out, u.extent.disk_bytes);
    push_varint(out, u64::from(u.extent.start));
    push_varint(out, u64::from(u.extent.end));
    let mut flags = u.extent.flags;
    if u.holder.external_names {
        flags |= EXTERNAL_NAMES;
    }
    if u.several_names {
        flags |= SEVERAL_NAMES;
    }
    out.push(flags);
    push_varint(out, u.holder.tree);
    push_varint(out, u.holder.objectid);
    push_varint(out, u64::from(u.extent.inode));
}

fn get_held(d: &mut Decoder<'_>, last: &mut u64) -> Option<HeldUse> {
    let bytenr = last.wrapping_add(d.varint()?);
    *last = bytenr;
    let disk_bytes = d.varint()?;
    let start = u32::try_from(d.varint()?).ok()?;
    let end = u32::try_from(d.varint()?).ok()?;
    let flags = d.byte()?;
    let tree = d.varint()?;
    let objectid = d.varint()?;
    let inode = u32::try_from(d.varint()?).ok()?;
    let use_flags = flags & !(EXTERNAL_NAMES | SEVERAL_NAMES);
    Some(HeldUse {
        extent: ExtentUse {
            bytenr,
            inode,
            start,
            end,
            flags: use_flags,
            disk_bytes,
        },
        holder: Holder {
            tree,
            objectid,
            external_names: flags & EXTERNAL_NAMES != 0,
            shared_leaf: use_flags & ExtentUse::SHARED_LEAF != 0,
        },
        several_names: flags & SEVERAL_NAMES != 0,
    })
}

/// Writes a unit's held uses, which must be in extent order.
pub fn write_extents(path: PathBuf, uses: impl Iterator<Item = HeldUse>) -> io::Result<()> {
    let mut out = Appender::new(path);
    let mut last = 0;
    for u in uses {
        put_held(out.buf(), &mut last, &u);
        out.commit()?;
    }
    out.flush()
}

/// Uses a sorter holds before writing them out as a sorted run.
pub const RUN_USES: usize = 1 << 16;

/// Sorts a unit's held uses into its extents file through sorted runs
/// of bounded size, so no unit holds all its uses at once (SPEC 9.1).
pub struct Sorter {
    dir: PathBuf,
    capacity: usize,
    buf: Vec<HeldUse>,
    runs: Vec<PathBuf>,
}

impl Sorter {
    /// Keeps up to `capacity` uses in memory; runs go in `dir`.
    pub fn new(dir: PathBuf, capacity: usize) -> Self {
        Sorter {
            dir,
            capacity,
            buf: Vec::new(),
            runs: Vec::new(),
        }
    }

    pub fn push(&mut self, u: HeldUse) -> io::Result<()> {
        self.buf.push(u);
        if self.buf.len() >= self.capacity {
            self.write_run()?;
        }
        Ok(())
    }

    fn write_run(&mut self) -> io::Result<()> {
        let path = self.dir.join(format!("run.{}", self.runs.len()));
        self.buf.sort_unstable_by_key(|u| u.extent);
        write_extents(path.clone(), self.buf.drain(..))?;
        self.runs.push(path);
        Ok(())
    }

    /// Writes every use pushed to `path` in extent order and removes the
    /// runs.
    pub fn finish(mut self, path: PathBuf) -> io::Result<()> {
        if self.runs.is_empty() {
            self.buf.sort_unstable_by_key(|u| u.extent);
            return write_extents(path, self.buf.into_iter());
        }
        if !self.buf.is_empty() {
            self.write_run()?;
        }
        let mut merge = Merge::open(self.runs.clone())?;
        write_extents(path, merge.by_ref().flat_map(|g| g.uses).map(|(_, u)| u))?;
        merge.finish()?;
        self.runs.iter().try_for_each(fs::remove_file)
    }
}

/// Every unit's uses of one extent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub bytenr: u64,
    /// `(unit, use)`, by unit.
    pub uses: Vec<(usize, HeldUse)>,
}

impl Group {
    /// The inodes of `unit`'s uses.
    fn inodes_of(&self, unit: usize) -> Vec<u32> {
        let uses = self.uses.iter().filter(|&&(u, _)| u == unit);
        uses.map(|(_, h)| h.extent.inode).collect()
    }
}

/// Merges the extents files of all units into groups in bytenr order.
/// A read error ends the iteration and is kept for `finish`.
pub struct Merge {
    readers: Vec<(ChunkReader, u64)>,
    heads: Vec<Option<HeldUse>>,
    heap: BinaryHeap<Reverse<(u64, usize)>>,
    error: Option<io::Error>,
}

impl Merge {
    pub fn open(paths: Vec<PathBuf>) -> io::Result<Merge> {
        let mut merge = Merge {
            readers: paths
                .into_iter()
                .map(|p| (ChunkReader::new(p), 0))
                .collect(),
            heads: Vec::new(),
            heap: BinaryHeap::new(),
            error: None,
        };
        merge.heads = vec![None; merge.readers.len()];
        for unit in 0..merge.readers.len() {
            merge.read_head(unit)?;
            merge.queue(unit);
        }
        Ok(merge)
    }

    fn read_head(&mut self, unit: usize) -> io::Result<()> {
        let (reader, last) = &mut self.readers[unit];
        self.heads[unit] = reader.next(|d| get_held(d, last))?;
        Ok(())
    }

    fn queue(&mut self, unit: usize) {
        if let Some(u) = &self.heads[unit] {
            self.heap.push(Reverse((u.extent.bytenr, unit)));
        }
    }

    /// Units come off the heap in order, so the uses are by unit.
    fn next_group(&mut self) -> io::Result<Option<Group>> {
        let Some(&Reverse((bytenr, _))) = self.heap.peek() else {
            return Ok(None);
        };
        let mut uses = Vec::new();
        while let Some(&Reverse((b, unit))) = self.heap.peek() {
            if b != bytenr {
                break;
            }
            self.heap.pop();
            while let Some(u) = self.heads[unit].filter(|u| u.extent.bytenr == bytenr) {
                uses.push((unit, u));
                self.read_head(unit)?;
            }
            self.queue(unit);
        }
        Ok(Some(Group { bytenr, uses }))
    }

    /// The read error that ended the iteration, if any.
    pub fn finish(self) -> io::Result<()> {
        self.error.map_or(Ok(()), Err)
    }
}

impl Iterator for Merge {
    type Item = Group;

    fn next(&mut self) -> Option<Group> {
        match self.next_group() {
            Ok(group) => group,
            Err(e) => {
                self.error.get_or_insert(e);
                self.heap.clear();
                None
            }
        }
    }
}

/// One extent's judgement as one unit applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub bytenr: u64,
    pub judgement: Judgement,
    pub owner: Owner,
    /// The unit's inodes holding the extent, one per use.
    pub inodes: Vec<u32>,
    /// Boundary nodes of the unit's child units that hold the extent.
    pub children: Vec<u32>,
}

fn put_judged(out: &mut Vec<u8>, last: &mut u64, j: &Judged) {
    push_varint(out, j.bytenr.wrapping_sub(*last));
    *last = j.bytenr;
    let g = &j.judgement;
    push_varint(out, g.disk_bytes);
    let (tag, blockers) = match g.verdict {
        None => (0, Blockers::default()),
        Some(Verdict::Reclaimable) => (1, Blockers::default()),
        Some(Verdict::Retained(b)) => (2, b),
        Some(Verdict::Unknown) => (3, Blockers::default()),
    };
    let owner = match j.owner {
        Owner::Here => 0,
        Owner::Above => 1,
    };
    out.extend_from_slice(&[tag, blockers.bits(), g.local.bits(), owner]);
    push_varint(out, g.pinned);
    put_u32s(out, &j.inodes);
    put_u32s(out, &j.children);
}

fn put_u32s(out: &mut Vec<u8>, values: &[u32]) {
    push_varint(out, values.len() as u64);
    for &v in values {
        push_varint(out, u64::from(v));
    }
}

/// A count, then that many values; the count is bounded by the bytes
/// left, since every value takes at least one.
fn get_u32s(d: &mut Decoder<'_>) -> Option<Vec<u32>> {
    let count = usize::try_from(d.varint()?).ok()?;
    if count > d.remaining() {
        return None;
    }
    (0..count)
        .map(|_| u32::try_from(d.varint()?).ok())
        .collect()
}

fn get_judged(d: &mut Decoder<'_>, last: &mut u64) -> Option<Judged> {
    let bytenr = last.wrapping_add(d.varint()?);
    *last = bytenr;
    let disk_bytes = d.varint()?;
    let tag = d.byte()?;
    let blockers = Blockers::from_bits(d.byte()?);
    let verdict = match tag {
        0 => None,
        1 => Some(Verdict::Reclaimable),
        2 => Some(Verdict::Retained(blockers)),
        3 => Some(Verdict::Unknown),
        _ => return None,
    };
    let local = Blockers::from_bits(d.byte()?);
    let owner = match d.byte()? {
        0 => Owner::Here,
        1 => Owner::Above,
        _ => return None,
    };
    let pinned = d.varint()?;
    let inodes = get_u32s(d)?;
    let children = get_u32s(d)?;
    Some(Judged {
        bytenr,
        judgement: Judgement {
            disk_bytes,
            verdict,
            local,
            pinned,
        },
        owner,
        inodes,
        children,
    })
}

/// Reads a unit's judgements in bytenr order.
pub struct JudgedReader {
    reader: ChunkReader,
    last: u64,
}

impl JudgedReader {
    pub fn new(path: PathBuf) -> Self {
        JudgedReader {
            reader: ChunkReader::new(path),
            last: 0,
        }
    }

    pub fn read(&mut self) -> io::Result<Option<Judged>> {
        let last = &mut self.last;
        self.reader.next(|d| get_judged(d, last))
    }
}

/// Routes each judged extent to the units holding it and to the units
/// between them and their lowest common unit (SPEC 9.1, 11).
pub struct Router {
    placement: Vec<Placement>,
    out: Vec<(Appender, u64)>,
    /// Corrections to the sum of the top-level units' totals.
    pub added: Metrics,
    pub removed: Metrics,
}

impl Router {
    pub fn new(placement: Vec<Placement>, paths: Vec<PathBuf>) -> Self {
        Router {
            placement,
            out: paths.into_iter().map(|p| (Appender::new(p), 0)).collect(),
            added: Metrics::default(),
            removed: Metrics::default(),
        }
    }

    fn parent(&self, unit: usize) -> Option<(usize, u32)> {
        self.placement[unit].parent
    }

    pub fn route(&mut self, group: &Group, judgement: Judgement) -> io::Result<()> {
        let mut holders: Vec<usize> = group.uses.iter().map(|&(u, _)| u).collect();
        holders.dedup();
        let lca = self.common_unit(&holders);
        let mut visits: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        for &h in &holders {
            visits.entry(h).or_default();
            let mut at = h;
            while Some(at) != lca {
                let Some((parent, boundary)) = self.parent(at) else {
                    break;
                };
                visits.entry(parent).or_default().push(boundary);
                at = parent;
            }
        }
        if lca.is_none() {
            let tops = visits.keys().filter(|&&u| self.parent(u).is_none()).count();
            self.correct_top(&judgement, tops as u64);
        }
        for (unit, mut children) in visits {
            children.sort_unstable();
            children.dedup();
            let owner = if Some(unit) == lca {
                Owner::Here
            } else {
                Owner::Above
            };
            let record = Judged {
                bytenr: group.bytenr,
                judgement,
                owner,
                inodes: group.inodes_of(unit),
                children,
            };
            self.push(unit, &record)?;
        }
        Ok(())
    }

    /// The lowest unit whose subtree holds every unit of `holders`;
    /// `None` when they lie below different top-level units.
    fn common_unit(&self, holders: &[usize]) -> Option<usize> {
        let mut path = vec![holders[0]];
        while let Some((parent, _)) = self.parent(*path.last().expect("non-empty")) {
            path.push(parent);
        }
        let mut deepest_common = 0;
        for &h in &holders[1..] {
            let mut at = h;
            let index = loop {
                if let Some(i) = path.iter().position(|&p| p == at) {
                    break i;
                }
                at = self.parent(at)?.0;
            };
            deepest_common = deepest_common.max(index);
        }
        Some(path[deepest_common])
    }

    /// Each of `tops` top-level units counts the extent once; the
    /// filesystem totals count it once, reclaimable there if at all.
    fn correct_top(&mut self, judgement: &Judgement, tops: u64) {
        let extra = tops.saturating_sub(1);
        let disk_bytes = judgement.disk_bytes;
        self.removed.physical += extra * disk_bytes;
        self.removed.extents += extra;
        match judgement.verdict {
            Some(Verdict::Unknown) => self.removed.unknown += extra * disk_bytes,
            Some(Verdict::Reclaimable) => {
                self.added.reclaimable += disk_bytes;
                self.added.pinned += judgement.pinned;
            }
            _ => {}
        }
    }

    fn push(&mut self, unit: usize, record: &Judged) -> io::Result<()> {
        let (out, last) = &mut self.out[unit];
        let before = out.buf().len();
        put_judged(out.buf(), last, record);
        if out.buf().len() - before > MAX_RECORD {
            return Err(io::Error::other("judgement record too large"));
        }
        out.commit()
    }

    pub fn finish(mut self) -> io::Result<()> {
        for (out, _) in &mut self.out {
            out.flush()?;
        }
        Ok(())
    }
}

const RECORD: u8 = 0;
const SPLICE: u8 = 1;

/// A unit's report records in preorder, with child units spliced in
/// where their boundaries are.
pub struct Fragment {
    out: Appender,
    records: u64,
}

impl Fragment {
    pub fn new(path: PathBuf) -> Self {
        Fragment {
            out: Appender::new(path),
            records: 0,
        }
    }

    pub fn record(&mut self, record: &NodeRecord) -> io::Result<()> {
        let buf = self.out.buf();
        buf.push(RECORD);
        let mut w = HashingWriter::new(buf);
        encode_record(&mut w, record)?;
        self.records += 1;
        self.out.commit()
    }

    /// Child unit `unit`'s records go here, `depth` below this unit's.
    pub fn splice(&mut self, unit: usize, depth: u32) -> io::Result<()> {
        let buf = self.out.buf();
        buf.push(SPLICE);
        push_varint(buf, unit as u64);
        push_varint(buf, u64::from(depth));
        self.out.commit()
    }

    /// Returns the records written, not counting spliced ones.
    pub fn finish(mut self) -> io::Result<u64> {
        self.out.flush()?;
        Ok(self.records)
    }
}

/// Writes unit `unit`'s fragment with its spliced children, every depth
/// raised by `depth`, reading fragments from `path`.
pub fn splice<W: Write>(
    w: &mut HashingWriter<W>,
    path: &dyn Fn(usize) -> PathBuf,
    unit: usize,
    depth: u32,
) -> io::Result<()> {
    let file = path(unit);
    let bytes = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let corrupt = || io::Error::new(io::ErrorKind::InvalidData, "corrupt report fragment");
    let mut d = Decoder::new(&bytes, 0);
    while !d.is_empty() {
        match d.byte() {
            Some(RECORD) => {
                let mut record = decode_record(&mut d).ok_or_else(corrupt)?;
                record.depth += depth;
                encode_record(w, &record)?;
            }
            Some(SPLICE) => {
                let child = d.varint().and_then(|v| usize::try_from(v).ok());
                let at = d.varint().and_then(|v| u32::try_from(v).ok());
                let (Some(child), Some(at)) = (child, at) else {
                    return Err(corrupt());
                };
                splice(w, path, child, depth + at)?;
            }
            _ => return Err(corrupt()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::Spill;
    use crate::spill::testing::scratch;

    fn held(bytenr: u64, tree: u64, objectid: u64) -> HeldUse {
        HeldUse {
            extent: ExtentUse {
                bytenr,
                inode: (objectid - 256) as u32,
                start: 0,
                end: 4096,
                flags: ExtentUse::SHARED_LEAF,
                disk_bytes: 4096,
            },
            holder: Holder {
                tree,
                objectid,
                external_names: true,
                shared_leaf: true,
            },
            several_names: true,
        }
    }

    #[test]
    fn merged_extents_group_every_units_uses_by_bytenr() {
        let spill = Spill::create(scratch("merge").join("run")).unwrap();
        let a = [held(10, 5, 257), held(10, 5, 258), held(30, 5, 257)];
        let b = [held(10, 256, 300), held(20, 256, 300)];
        write_extents(spill.path("a"), a.iter().copied()).unwrap();
        write_extents(spill.path("b"), b.iter().copied()).unwrap();
        let mut merge = Merge::open(vec![spill.path("a"), spill.path("b")]).unwrap();
        let groups: Vec<Group> = merge.by_ref().collect();
        merge.finish().unwrap();
        assert_eq!(
            groups,
            [
                Group {
                    bytenr: 10,
                    uses: vec![(0, a[0]), (0, a[1]), (1, b[0])]
                },
                Group {
                    bytenr: 20,
                    uses: vec![(1, b[1])]
                },
                Group {
                    bytenr: 30,
                    uses: vec![(0, a[2])]
                },
            ]
        );
    }

    #[test]
    fn sorted_runs_merge_into_one_extents_file_in_bytenr_order() {
        let dir = scratch("sort");
        let uses: Vec<HeldUse> = [50, 10, 40, 10, 30, 20, 60]
            .into_iter()
            .zip(256..)
            .map(|(bytenr, objectid)| held(bytenr, 5, objectid))
            .collect();
        let mut sorter = Sorter::new(dir.clone(), 2);
        for &u in &uses {
            sorter.push(u).unwrap();
        }
        sorter.finish(dir.join("extents")).unwrap();
        let mut merge = Merge::open(vec![dir.join("extents")]).unwrap();
        let mut read: Vec<HeldUse> = merge
            .by_ref()
            .flat_map(|g| g.uses)
            .map(|(_, u)| u)
            .collect();
        merge.finish().unwrap();
        let bytenrs: Vec<u64> = read.iter().map(|u| u.extent.bytenr).collect();
        assert_eq!(bytenrs, [10, 10, 20, 30, 40, 50, 60]);
        let mut sorted = uses.clone();
        sorted.sort_by_key(|u| u.extent);
        read.sort_by_key(|u| u.extent);
        assert_eq!(read, sorted);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "runs are removed");
    }

    fn judgement(verdict: Verdict) -> Judgement {
        Judgement {
            disk_bytes: 4096,
            verdict: Some(verdict),
            local: Blockers::LIVE_REFLINK,
            pinned: 100,
        }
    }

    fn judged(spill: &Spill, unit: usize) -> Vec<Judged> {
        let mut reader = JudgedReader::new(spill.path(&format!("j{unit}")));
        let mut out = Vec::new();
        while let Some(j) = reader.read().unwrap() {
            out.push(j);
        }
        out
    }

    /// Units: 0 and 1 top-level; 2 below 0 at node 7; 3 below 2 at
    /// node 4; 4 below 0 at node 9.
    fn router(spill: &Spill) -> Router {
        let placement = [None, None, Some((0, 7)), Some((2, 4)), Some((0, 9))]
            .into_iter()
            .map(|parent| Placement { parent })
            .collect();
        Router::new(
            placement,
            (0..5).map(|u| spill.path(&format!("j{u}"))).collect(),
        )
    }

    fn group(bytenr: u64, units: &[usize]) -> Group {
        Group {
            bytenr,
            uses: units.iter().map(|&u| (u, held(bytenr, 5, 257))).collect(),
        }
    }

    #[test]
    fn extents_go_to_holders_and_the_units_up_to_their_common_one() {
        let spill = Spill::create(scratch("route").join("run")).unwrap();
        let mut router = router(&spill);
        let j = judgement(Verdict::Reclaimable);
        router.route(&group(10, &[3]), j).unwrap();
        router.route(&group(20, &[3, 4]), j).unwrap();
        router.route(&group(30, &[0, 2]), j).unwrap();
        router.route(&group(40, &[1, 3]), j).unwrap();
        assert_eq!(router.added.reclaimable, 4096);
        assert_eq!((router.removed.physical, router.removed.extents), (4096, 1));
        router.finish().unwrap();
        // Holders get their uses' inodes (1, from `group`); units on the
        // way to the common unit only their boundaries.
        let record = |bytenr, owner, children: &[u32], inodes: &[u32]| Judged {
            bytenr,
            judgement: j,
            owner,
            inodes: inodes.to_vec(),
            children: children.to_vec(),
        };
        use Owner::{Above, Here};
        assert_eq!(
            judged(&spill, 0),
            [
                record(20, Here, &[7, 9], &[]),
                record(30, Here, &[7], &[1]),
                record(40, Above, &[7], &[])
            ]
        );
        assert_eq!(judged(&spill, 1), [record(40, Above, &[], &[1])]);
        assert_eq!(
            judged(&spill, 2),
            [
                record(20, Above, &[4], &[]),
                record(30, Above, &[], &[1]),
                record(40, Above, &[4], &[])
            ]
        );
        assert_eq!(
            judged(&spill, 3),
            [
                record(10, Here, &[], &[1]),
                record(20, Above, &[], &[1]),
                record(40, Above, &[], &[1])
            ]
        );
        assert_eq!(judged(&spill, 4), [record(20, Above, &[], &[1])]);
    }

    #[test]
    fn an_unknown_extent_across_top_units_is_counted_once() {
        let spill = Spill::create(scratch("route").join("run")).unwrap();
        let mut router = router(&spill);
        router
            .route(&group(10, &[0, 1]), judgement(Verdict::Unknown))
            .unwrap();
        router
            .route(
                &group(20, &[2, 1]),
                judgement(Verdict::Retained(Blockers::SNAPSHOT)),
            )
            .unwrap();
        assert_eq!(router.added, Metrics::default());
        assert_eq!(
            (
                router.removed.physical,
                router.removed.extents,
                router.removed.unknown
            ),
            (8192, 2, 4096)
        );
    }
}
