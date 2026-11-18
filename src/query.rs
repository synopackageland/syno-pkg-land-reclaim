//! Answers from a report (SPEC 5.4, 5.5): locating a path and ranking
//! the entries below it.

use crate::model::accounting::{Blockers, Held};
use crate::model::namespace::FileKind;
use crate::model::tree::{Metrics, NodeFlags, Outside};
#[cfg(test)]
use crate::report::Report;
use crate::report::{Header, NodeRecord};
use crate::summary::{
    Below, PackageOverlap, PackageRoot, ServiceSummary, ServiceTally, ServiceUse, Step,
};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::ControlFlow;

/// A node copied out of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: Vec<u8>,
    pub kind: FileKind,
    pub flags: NodeFlags,
    pub metrics: Metrics,
    pub blockers: Blockers,
    /// Retained bytes per holder class; directories only.
    pub held: Held,
    pub nlink: u32,
    pub tree: u64,
    pub objectid: u64,
    /// A directory's values without the package data below it, when it
    /// holds some.
    pub outside: Option<Box<Outside>>,
}

impl Entry {
    /// Retained bytes per holder class (SPEC 5.4 `HELD BY`).
    pub fn retained(&self) -> Held {
        crate::holders::retained(self.kind, &self.metrics, self.blockers, self.held)
    }

    pub fn new(path: &[u8], record: &NodeRecord<'_>) -> Self {
        Entry {
            path: path.to_vec(),
            kind: record.kind,
            flags: record.flags,
            metrics: record.metrics,
            blockers: record.blockers,
            held: record.held,
            nlink: record.nlink,
            tree: record.tree,
            objectid: record.objectid,
            outside: record.outside.map(Box::new),
        }
    }

    /// Values without the package data below the entry, which service
    /// rows count.
    pub fn metrics_without_package_data(&self) -> &Metrics {
        self.outside.as_ref().map_or(&self.metrics, |o| &o.metrics)
    }

    /// Retained bytes per holder class without the package data below
    /// the entry.
    pub fn retained_without_package_data(&self) -> Held {
        match &self.outside {
            Some(o) => crate::holders::retained(self.kind, &o.metrics, self.blockers, o.held),
            None => self.retained(),
        }
    }
}

/// Whether `path` is one of the report's roots or inside one.
pub fn covers(header: &Header, path: &[u8]) -> bool {
    header.roots.iter().any(|root| is_within(&root.path, path))
}

fn is_within(outer: &[u8], path: &[u8]) -> bool {
    path == outer
        || (path.starts_with(outer)
            && (outer.ends_with(b"/") || path.get(outer.len()) == Some(&b'/')))
}

#[cfg(test)]
pub fn find(report: &Report, path: &[u8]) -> Option<Entry> {
    let mut find = Find::new(path);
    report.for_each(|p, record| find.visit(p, record));
    find.finish()
}

/// Finds the entry at one path in records fed in preorder.
pub struct Find<'p> {
    path: &'p [u8],
    found: Option<Entry>,
}

impl<'p> Find<'p> {
    pub fn new(path: &'p [u8]) -> Self {
        Find { path, found: None }
    }

    /// Feeds one record; breaks once the path is found.
    pub fn visit(&mut self, path: &[u8], record: &NodeRecord<'_>) -> ControlFlow<()> {
        if path != self.path {
            return ControlFlow::Continue(());
        }
        self.found = Some(Entry::new(path, record));
        ControlFlow::Break(())
    }

    /// `None` when the records did not hold the path.
    pub fn finish(self) -> Option<Entry> {
        self.found
    }
}

/// Deleting all roots of a report together (SPEC 5.6, 10 multi-select).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Each root alone, in report order.
    pub roots: Vec<Entry>,
    /// All roots together.
    pub combined: Metrics,
    /// Resolved bytes freed only because the roots go together.
    pub together_bonus: u64,
}

/// The report's root set as one selection; its totals are the combined
/// deletion, not a sum of per-root values.
#[cfg(test)]
pub fn selection(report: &Report) -> Selection {
    let mut walk = SelectionWalk::new(&report.header);
    report.for_each(|path, record| walk.visit(path, record));
    walk.finish()
}

/// Gathers a report's root set as one selection from records fed in
/// preorder.
pub struct SelectionWalk<'h> {
    header: &'h Header,
    roots: Vec<Entry>,
}

impl<'h> SelectionWalk<'h> {
    pub fn new(header: &'h Header) -> Self {
        SelectionWalk {
            header,
            roots: Vec::new(),
        }
    }

    /// Feeds one record.
    pub fn visit(&mut self, path: &[u8], record: &NodeRecord<'_>) -> ControlFlow<()> {
        if self.header.roots.iter().any(|root| root.path == path) {
            self.roots.push(Entry::new(path, record));
        }
        ControlFlow::Continue(())
    }

    pub fn finish(self) -> Selection {
        let combined = self.header.totals;
        let alone: u64 = self.roots.iter().map(|e| e.metrics.reclaimable).sum();
        Selection {
            roots: self.roots,
            combined,
            together_bonus: combined.reclaimable.saturating_sub(alone),
        }
    }
}

/// The metric a ranking orders by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Logical,
    Physical,
    /// Resolved reclaimable bytes.
    Reclaimable,
    Shared,
    /// Bytes deleting the entry would not free: shared and unknown.
    Held,
    /// Shared bytes per million footprint bytes.
    SharedRatio,
}

impl Measure {
    pub fn of(self, metrics: &Metrics) -> u64 {
        match self {
            Measure::Logical => metrics.logical,
            Measure::Physical => metrics.physical,
            Measure::Reclaimable => metrics.reclaimable,
            Measure::Shared => metrics.shared(),
            Measure::Held => metrics.physical.saturating_sub(metrics.reclaimable),
            Measure::SharedRatio => match metrics.physical {
                0 => 0,
                physical => {
                    (u128::from(metrics.shared()) * 1_000_000 / u128::from(physical)) as u64
                }
            },
        }
    }
}

/// Which entries a ranking considers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum KindFilter {
    /// Direct children of the target.
    All,
    /// Regular files at any depth.
    Files,
    /// Directories at any depth.
    Dirs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranking {
    /// The target itself.
    pub target: Entry,
    /// Best first.
    pub entries: Vec<Entry>,
    /// Candidates left out because they are detail rows of a DSM
    /// service path.
    pub hidden_system: u64,
}

pub struct TopQuery<'a> {
    pub under: &'a [u8],
    /// Service rows stand for DSM service paths: a folder holding a
    /// package marker is one, and its entries are detail rows. Ranked
    /// folders leave out the package data below them.
    pub services: bool,
    pub by: Measure,
    pub kind: KindFilter,
    pub include_system: bool,
    pub limit: usize,
    /// Consider only entries with proven shared bytes.
    pub only_shared: bool,
}

/// Ranks entries below `query.under`; `None` if the report lacks it.
#[cfg(test)]
pub fn top(report: &Report, query: &TopQuery<'_>) -> Option<Ranking> {
    let mut walk = TopWalk::new(&report.header.mount, query);
    report.for_each(|path, record| walk.visit(path, record));
    walk.finish().map(|answer| answer.ranking)
}

/// What `top` answers for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopAnswer {
    pub ranking: Ranking,
    /// Empty unless the query asks for service rows.
    pub services: ServiceSummary,
    /// Package data below ranked entries that service rows also count.
    pub overlaps: Vec<PackageOverlap>,
    /// Package roots below the target, in preorder; empty unless the
    /// query asks for service rows.
    pub roots: Vec<PackageRoot>,
}

impl TopAnswer {
    /// The extent pass also sees our cache files created after the namespace
    /// read. Its once-only bucket is the authoritative volume display size.
    pub fn include_own_state(&mut self, data: &crate::overview::OverviewData) {
        let service = crate::dsm::syno_reclaim();
        let Some(&bytes) = data.dsm_services.get(&crate::dsm::service_id(service)) else {
            return;
        };
        let at = self
            .services
            .uses
            .iter()
            .position(|u| u.service == Some(service));
        let at = at.unwrap_or_else(|| {
            self.services.uses.push(crate::summary::ServiceUse {
                service: Some(service),
                ..Default::default()
            });
            self.services.uses.len() - 1
        });
        let line = &mut self.services.uses[at];
        let growth = bytes.saturating_sub(line.metrics.physical);
        if growth > 0 {
            if line.full_parts.is_empty() && line.paths > 0 {
                line.full_parts.push(line.clone());
            }
            // These bytes were not in the namespace's containing folder.
            // An unplaced partition must not claim that old row covers them.
            line.full_parts.push(crate::summary::ServiceUse {
                service: Some(service),
                metrics: Metrics {
                    physical: growth,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        line.metrics.physical = bytes;
        line.metrics.reclaimable = 0;
        line.metrics.unknown = 0;
        line.metrics.pinned = 0;
        self.services
            .uses
            .sort_by_key(|u| std::cmp::Reverse(u.metrics.physical));
    }
}

/// Ranks the entries below a target and, for service rows, adds up its
/// services and finds its package roots, in one walk of records fed in
/// preorder. The tool's own state is never ranked.
pub struct TopWalk<'a> {
    mount: &'a [u8],
    below: Below<'a>,
    ranker: Ranker<'a>,
    tally: Option<ServiceTally>,
    roots: Vec<PackageRoot>,
}

impl<'a> TopWalk<'a> {
    pub fn new(mount: &'a [u8], query: &'a TopQuery<'a>) -> Self {
        TopWalk {
            mount,
            below: Below::new(mount, query.under, query.services),
            ranker: Ranker::new(query),
            tally: query.services.then(ServiceTally::default),
            roots: Vec::new(),
        }
    }

    /// Feeds one record; breaks once past the target.
    pub fn visit(&mut self, path: &[u8], record: &NodeRecord<'_>) -> ControlFlow<()> {
        let TopWalk {
            mount,
            below,
            ranker,
            tally,
            roots,
        } = self;
        below.visit(path, record, |depth, step| {
            let covered = ranker.metadata_covered(depth, path, record, step);
            ranker.add(depth, path, record, step);
            if let Some(tally) = tally {
                let folder = (ranker.query.under == *mount)
                    .then_some(ranker.full_folders.last().map(|(_, path)| path.as_slice()));
                tally.add_for_display(path, record, step, covered, folder);
                roots.extend(crate::summary::package_root(mount, path, record));
            }
        })
    }

    /// `None` when the records did not hold the target.
    pub fn finish(self) -> Option<TopAnswer> {
        let ranking = self.ranker.finish(self.below.target()?);
        let (services, overlaps) = self
            .tally
            .map(|tally| tally.finish(&ranking.entries))
            .unwrap_or_default();
        Some(TopAnswer {
            ranking,
            services,
            overlaps,
            roots: self.roots,
        })
    }
}

/// Keeps the best entries below a target, fed as [`Below`] finds them.
struct Ranker<'q> {
    query: &'q TopQuery<'q>,
    heap: BinaryHeap<Reverse<Candidate>>,
    hidden_system: u64,
    /// Outermost eligible directory; independent of the heap's limit.
    covering_depth: Option<u32>,
    /// Nearest eligible ancestors for full-metric service partitions. Unlike
    /// metadata coverage, grouping by the outermost row can mix paths whose
    /// only surviving parents are different inner folders.
    full_folders: Vec<(u32, Vec<u8>)>,
}

impl<'q> Ranker<'q> {
    fn new(query: &'q TopQuery<'q>) -> Self {
        Ranker {
            query,
            heap: BinaryHeap::new(),
            hidden_system: 0,
            covering_depth: None,
            full_folders: Vec::new(),
        }
    }

    /// Metadata remains in eligible folders even when the limit omits
    /// them. Only paths with no such row get a separate service row.
    fn metadata_covered(
        &mut self,
        depth: u32,
        path: &[u8],
        record: &NodeRecord<'_>,
        step: Step,
    ) -> bool {
        while self
            .full_folders
            .last()
            .is_some_and(|(d, _)| record.depth <= *d)
        {
            self.full_folders.pop();
        }
        if self.covering_depth.is_some_and(|d| record.depth <= d) {
            self.covering_depth = None;
        }
        let eligible =
            self.wanted(depth, record) && (!Self::is_detail(step) || self.query.include_system);
        let covered = self.covering_depth.is_some() || eligible;
        if eligible && record.kind == FileKind::Directory {
            self.covering_depth.get_or_insert(record.depth);
            if self.query.services {
                self.full_folders.push((record.depth, path.to_vec()));
            }
        }
        covered || (self.query.include_system && matches!(self.query.kind, KindFilter::Files))
    }

    fn wanted(&self, depth: u32, record: &NodeRecord<'_>) -> bool {
        let kind = match self.query.kind {
            KindFilter::All => record.depth == depth + 1,
            KindFilter::Files => record.kind == FileKind::Regular,
            KindFilter::Dirs => record.kind == FileKind::Directory,
        };
        kind && (!self.query.only_shared || record.metrics.shared() > 0)
            && !record.flags.contains(NodeFlags::OWN_STATE)
    }

    fn is_detail(step: Step) -> bool {
        matches!(step, Step::Starts(s) | Step::Inside(s)
            if !s.is_some_and(|s| s.ranks_contents))
    }

    /// Considers the entry `record` at `path`, below a target at `depth`.
    fn add(&mut self, depth: u32, path: &[u8], record: &NodeRecord<'_>, step: Step) {
        let query = self.query;
        let detail = Self::is_detail(step);
        if !self.wanted(depth, record) {
            return;
        }
        if detail && !query.include_system {
            self.hidden_system += 1;
            return;
        }
        let metrics = if query.services {
            record
                .outside
                .as_ref()
                .map_or(&record.metrics, |o| &o.metrics)
        } else {
            &record.metrics
        };
        let value = query.by.of(metrics);
        // Reject strictly worse rank keys before copying paths or allocating package data.
        // Equal keys keep the original heap behavior, including repeated report paths.
        if query.limit == 0
            || (self.heap.len() == query.limit
                && self.heap.peek().is_some_and(|Reverse(worst)| {
                    value < worst.value || (value == worst.value && path > &worst.entry.path[..])
                }))
        {
            return;
        }
        let candidate = Candidate {
            value,
            entry: Entry::new(path, record),
        };
        self.heap.push(Reverse(candidate));
        if self.heap.len() > query.limit {
            self.heap.pop();
        }
    }

    fn finish(self, target: Entry) -> Ranking {
        let mut best: Vec<Candidate> = self.heap.into_iter().map(|Reverse(c)| c).collect();
        best.sort_unstable_by(|a, b| b.cmp(a));
        let entries = best.into_iter().map(|c| c.entry).collect();
        Ranking {
            target,
            entries,
            hidden_system: self.hidden_system,
        }
    }
}

/// One row of a `top` ranking (SPEC 5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row<'a> {
    Entry(Entry),
    /// A DSM service's paths below the target as one row.
    Service(&'a ServiceUse),
}

impl Row<'_> {
    pub fn metrics(&self) -> &Metrics {
        match self {
            Row::Entry(e) => e.metrics_without_package_data(),
            Row::Service(u) => &u.metrics,
        }
    }

    /// Full footprints used by the JSON/UI, not the CLI's package-free subset.
    pub fn full_metrics(&self) -> &Metrics {
        match self {
            Row::Entry(e) => &e.metrics,
            Row::Service(u) => &u.metrics,
        }
    }

    fn covered_path(&self) -> &[u8] {
        match self {
            Row::Entry(e) => &e.path,
            Row::Service(u) => &u.common_path,
        }
    }

    /// Whether the other row's entire full footprint is in this row. A service
    /// partition is a genuine subtraction, not path-based duplication.
    fn includes(&self, other: &Row<'_>) -> bool {
        let child = other.covered_path();
        match self {
            Row::Entry(e) if e.kind == FileKind::Directory => {
                is_within(&e.path, child) && (e.path != child || matches!(other, Row::Service(_)))
            }
            Row::Service(u) => u.path.as_deref().is_some_and(|parent| {
                parent != child
                    && is_within(parent, child)
                    && !u
                        .excluded_paths
                        .iter()
                        .any(|p| is_within(p, child) || is_within(child, p))
            }),
            _ => false,
        }
    }

    /// The path rows of equal value are ordered by.
    fn path(&self) -> &[u8] {
        match self {
            Row::Entry(e) => &e.path,
            Row::Service(u) => u.listed.first().map_or(&[][..], |p| p),
        }
    }
}

/// The best `limit` rows by `by`: the ranked entries and one row per
/// line of `services`. Metadata already counted by eligible entries is
/// informational; only its standalone paths rank as a service row.
/// A Recycle Bin is a row of its entry, once.
pub fn rows<'a>(
    ranking: &Ranking,
    services: &'a ServiceSummary,
    by: Measure,
    limit: usize,
) -> Vec<Row<'a>> {
    ranked_rows(ranking, services, by, limit, false)
}

/// Full-footprint JSON rows keep service groups with different containing
/// folders separate, so a limit cannot make a partially covered aggregate
/// count some of its bytes twice.
pub fn full_rows<'a>(
    ranking: &Ranking,
    services: &'a ServiceSummary,
    by: Measure,
    limit: usize,
) -> Vec<Row<'a>> {
    ranked_rows(ranking, services, by, limit, true)
}

/// Classify the candidates before the ranking limit; only parents still in
/// this answer may be named as covering rows. Bytes do not disappear when a
/// small limit omits a covering parent.
pub fn inclusions(rows: &[Row<'_>]) -> Vec<Option<Vec<u8>>> {
    rows.iter()
        .map(|child| {
            rows.iter()
                .filter(|parent| parent.includes(child))
                .min_by_key(|parent| parent.path().len())
                .map(|parent| parent.path().to_vec())
        })
        .collect()
}

fn ranked_rows<'a>(
    ranking: &Ranking,
    services: &'a ServiceSummary,
    by: Measure,
    limit: usize,
    full: bool,
) -> Vec<Row<'a>> {
    let mut rows: Vec<Row<'a>> = ranking.entries.iter().cloned().map(Row::Entry).collect();
    for line in &services.uses {
        let line = if line.included_in_rows {
            let Some(standalone) = line.standalone.as_deref() else {
                continue;
            };
            standalone
        } else {
            line
        };
        let parts = if full && !line.full_parts.is_empty() {
            &line.full_parts[..]
        } else {
            std::slice::from_ref(line)
        };
        for line in parts {
            match (&line.entry, line.service) {
                (Some(entry), Some(s)) if s.ranks_contents => {
                    if !ranking.entries.iter().any(|e| e.path == entry.path) {
                        rows.push(Row::Entry(entry.clone()));
                    }
                }
                _ => rows.push(Row::Service(line)),
            }
        }
    }
    rows.sort_by(|a, b| {
        by.of(b.metrics())
            .cmp(&by.of(a.metrics()))
            .then_with(|| a.path().cmp(b.path()))
    });
    rows.truncate(limit);
    rows
}

/// Ranking order: larger values are better, then earlier paths.
#[derive(Debug, PartialEq, Eq)]
struct Candidate {
    value: u64,
    entry: Entry,
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value
            .cmp(&other.value)
            .then_with(|| other.entry.path.cmp(&self.entry.path))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::btrfs::subvolume::SubvolumeSet;
    use crate::model::accounting::{Blockers, Held, HeldClass};
    use crate::model::testing::*;
    use crate::model::tree::{Marks, RootKind, ScopeTree};
    use crate::report::testing::{encode, encode_accounted, header};
    use crate::report::{Report, ReportReader, RootInfo};
    use crate::target::Scope;

    const MIB: u64 = 1 << 20;

    /// `/v`: `big` (3 MiB), `@eaDir/thumb` (5 MiB, system), `dir/small`
    /// (1 MiB) and `dir/mid` (2 MiB), `state` (own state, 9 MiB).
    fn report_tree() -> ScopeTree {
        let ns = read_tree(
            5,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "big", 257),
                entry(256, 3, "@eaDir", 258),
                entry(256, 4, "dir", 260),
                entry(256, 5, "state", 263),
                inode(257, FILE, 3 * MIB, 1),
                whole_extent(257, 0, 10 * MIB, 3 * MIB),
                inode(258, DIR, 0, 1),
                entry(258, 2, "thumb", 259),
                inode(259, FILE, 5 * MIB, 1),
                whole_extent(259, 0, 20 * MIB, 5 * MIB),
                inode(260, DIR, 0, 1),
                entry(260, 2, "small", 261),
                entry(260, 3, "mid", 262),
                inode(261, FILE, MIB, 1),
                whole_extent(261, 0, 30 * MIB, MIB),
                inode(262, FILE, 2 * MIB, 1),
                whole_extent(262, 0, 40 * MIB, 2 * MIB),
                inode(263, FILE, 9 * MIB, 1),
                whole_extent(263, 0, 50 * MIB, 9 * MIB),
            ],
        )
        .unwrap();
        let mut marks = Marks::default();
        marks.own_state.insert((5, 263));
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v",
            RootKind::Volume,
            &SubvolumeSet::default(),
            &marks,
        )
        .unwrap();
        tree
    }

    fn report() -> Report {
        Report::parse(report_bytes()).unwrap()
    }

    fn report_bytes() -> Vec<u8> {
        let header = header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        encode(&header, &report_tree())
    }

    fn query(under: &[u8], kind: KindFilter) -> TopQuery<'_> {
        TopQuery {
            under,
            services: false,
            by: Measure::Physical,
            kind,
            include_system: false,
            limit: 10,
            only_shared: false,
        }
    }

    fn ranked(ranking: &Ranking) -> Vec<(String, u64)> {
        ranking
            .entries
            .iter()
            .map(|e| {
                (
                    String::from_utf8(e.path.clone()).unwrap(),
                    e.metrics.physical / MIB,
                )
            })
            .collect()
    }

    #[test]
    fn ranks_direct_children_hiding_system_and_own_state() {
        let r = top(&report(), &query(b"/v", KindFilter::All)).unwrap();
        assert_eq!(
            ranked(&r),
            [("/v/big".to_string(), 3), ("/v/dir".to_string(), 3)]
        );
        assert_eq!(r.hidden_system, 1);
        assert_eq!(r.target.metrics.physical, 20 * MIB);
    }

    /// `/v`: `@synologydrive/x` (4 MiB), `s/@ActiveBackup/v1` (3 MiB),
    /// `s/#recycle/old` (2 MiB) and `s/doc` (1 MiB).
    fn service_report() -> Report {
        let ns = read_tree(
            5,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "@synologydrive", 257),
                entry(256, 3, "s", 259),
                inode(257, DIR, 0, 1),
                entry(257, 2, "x", 258),
                inode(258, FILE, 4 * MIB, 1),
                whole_extent(258, 0, 10 * MIB, 4 * MIB),
                inode(259, DIR, 0, 1),
                entry(259, 2, "@ActiveBackup", 260),
                entry(259, 3, "#recycle", 262),
                entry(259, 4, "doc", 264),
                inode(260, DIR, 0, 1),
                entry(260, 2, "v1", 261),
                inode(261, FILE, 3 * MIB, 1),
                whole_extent(261, 0, 20 * MIB, 3 * MIB),
                inode(262, DIR, 0, 1),
                entry(262, 2, "old", 263),
                inode(263, FILE, 2 * MIB, 1),
                whole_extent(263, 0, 30 * MIB, 2 * MIB),
                inode(264, FILE, MIB, 1),
                whole_extent(264, 0, 40 * MIB, MIB),
            ],
        )
        .unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v",
            RootKind::Volume,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        let header = header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        Report::parse(encode(&header, &tree)).unwrap()
    }

    #[test]
    fn service_paths_hide_their_detail_rows_but_recycle_bins_do_not() {
        let report = service_report();
        let r = top(&report, &query(b"/v", KindFilter::Files)).unwrap();
        assert_eq!(
            ranked(&r),
            [
                ("/v/s/#recycle/old".to_string(), 2),
                ("/v/s/doc".to_string(), 1)
            ]
        );
        assert_eq!(r.hidden_system, 2);
        let r = top(&report, &query(b"/v/s", KindFilter::All)).unwrap();
        assert_eq!(
            ranked(&r),
            [
                ("/v/s/#recycle".to_string(), 2),
                ("/v/s/doc".to_string(), 1)
            ]
        );
        let mut all = query(b"/v", KindFilter::Files);
        all.include_system = true;
        assert_eq!(top(&report, &all).unwrap().entries.len(), 4);
    }

    fn labels(rows: &[Row<'_>]) -> Vec<(String, u64)> {
        rows.iter()
            .map(|row| {
                let label = match row {
                    Row::Entry(e) => String::from_utf8(e.path.clone()).unwrap(),
                    Row::Service(u) => format!("[{}]", u.service.map_or("other", |s| s.code)),
                };
                (label, row.metrics().physical / MIB)
            })
            .collect()
    }

    fn service_rows(report: &Report, under: &[u8], limit: usize) -> Vec<(String, u64)> {
        let q = TopQuery {
            services: true,
            ..query(under, KindFilter::All)
        };
        let ranking = top(report, &q).unwrap();
        let services = crate::summary::service_summary(report, under).unwrap();
        labels(&rows(&ranking, &services, Measure::Physical, limit))
    }

    #[test]
    fn a_folder_marked_by_active_backup_ranks_only_in_its_service_row() {
        let report = service_report();
        assert_eq!(
            service_rows(&report, b"/v", 10),
            [
                ("[active_backup_data]".into(), 6),
                ("[synology_drive]".into(), 4),
            ]
        );
        assert_eq!(service_rows(&report, b"/v", 1).len(), 1);
    }

    #[test]
    fn a_ranked_recycle_bin_is_not_listed_twice() {
        assert_eq!(
            service_rows(&service_report(), b"/v/s", 10),
            [
                ("[active_backup_data]".into(), 3),
                ("/v/s/#recycle".into(), 2),
                ("/v/s/doc".into(), 1),
            ]
        );
    }

    #[test]
    fn one_walk_ranks_adds_up_services_and_finds_package_roots() {
        let report = service_report();
        for under in [&b"/v"[..], b"/v/s", b"/v/nope"] {
            let q = TopQuery {
                services: true,
                ..query(under, KindFilter::Dirs)
            };
            let mut walk = TopWalk::new(&report.header.mount, &q);
            report.for_each(|path, record| walk.visit(path, record));
            let mut answer = walk.finish();
            // Standalone summary walks do not know eligible ranking ancestors.
            if let Some(answer) = &mut answer {
                for line in &mut answer.services.uses {
                    line.full_parts.clear();
                }
            }
            let apart = top(&report, &q).map(|ranking| {
                let (services, overlaps) =
                    crate::summary::service_summary_with_overlaps(&report, under, &ranking.entries)
                        .unwrap();
                let mut roots = crate::summary::package_roots(&report);
                roots.retain(|root| root.path != under && is_within(under, &root.path));
                TopAnswer {
                    ranking,
                    services,
                    overlaps,
                    roots,
                }
            });
            assert_eq!(answer, apart, "{}", String::from_utf8_lossy(under));
        }
    }

    /// `/v`: `big` (3 MiB), `share/doc` (1 MiB) and `share/abb`, a
    /// folder `@ActiveBackup` marks (5 MiB), accounted.
    pub(crate) fn package_report() -> Report {
        Report::parse(package_report_bytes()).unwrap()
    }

    /// [`package_report`] as stored.
    pub(crate) fn package_report_bytes() -> Vec<u8> {
        accounted_report_bytes(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "big", 257),
            entry(256, 3, "share", 258),
            inode(257, FILE, 3 * MIB, 1),
            inode(258, DIR, 0, 1),
            entry(258, 2, "doc", 259),
            entry(258, 3, "abb", 260),
            inode(259, FILE, MIB, 1),
            inode(260, DIR, 0, 1),
            entry(260, 2, "@ActiveBackup", 261),
            inode(261, FILE, 5 * MIB, 1),
        ])
    }

    /// Distinct extents: share (doc 1, bin 4, deep vault 3), other (5).
    pub(crate) fn nested_report_bytes() -> Vec<u8> {
        accounted_with_refs(
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "share", 257),
                entry(256, 3, "other", 264),
                inode(257, DIR, 0, 1),
                entry(257, 2, "doc", 258),
                entry(257, 3, "#recycle", 259),
                entry(257, 4, "deep", 261),
                inode(258, FILE, MIB, 1),
                whole_extent(258, 0, 10 * MIB, MIB),
                inode(259, DIR, 0, 1),
                entry(259, 2, "old", 260),
                inode(260, FILE, 4 * MIB, 1),
                whole_extent(260, 0, 20 * MIB, 4 * MIB),
                inode(261, DIR, 0, 1),
                entry(261, 2, "vault.hbk", 262),
                inode(262, DIR, 0, 1),
                entry(262, 2, "data", 263),
                inode(263, FILE, 3 * MIB, 1),
                whole_extent(263, 0, 30 * MIB, 3 * MIB),
                inode(264, FILE, 5 * MIB, 1),
                whole_extent(264, 0, 40 * MIB, 5 * MIB),
            ],
            &[
                (10 * MIB, MIB, 258),
                (20 * MIB, 4 * MIB, 260),
                (30 * MIB, 3 * MIB, 263),
                (40 * MIB, 5 * MIB, 264),
            ],
        )
    }

    #[test]
    fn volume_nested_rows_keep_full_folders_and_count_distinct_bytes_once() {
        let report = Report::parse(nested_report_bytes()).unwrap();
        let q = TopQuery {
            services: true,
            limit: 200,
            ..query(b"/v", KindFilter::All)
        };
        let mut walk = TopWalk::new(b"/v", &q);
        report.for_each(|p, node| walk.visit(p, node));
        let answer = walk.finish().unwrap();
        let rows = full_rows(&answer.ranking, &answer.services, q.by, q.limit);
        let included = inclusions(&rows);
        for (row, parent) in rows.iter().zip(&included) {
            if row.path() == b"/v/share" {
                assert_eq!(row.full_metrics().physical, 8 * MIB);
                assert_eq!(row.full_metrics().reclaimable, 8 * MIB);
                assert!(parent.is_none());
                // CLI still uses the disjoint subset, unchanged.
                assert_eq!(row.metrics().physical, MIB);
            } else if row.path().starts_with(b"/v/share/") {
                assert_eq!(parent.as_deref(), Some(&b"/v/share"[..]));
            }
        }
        let represented: u64 = rows
            .iter()
            .zip(included)
            .filter(|(_, parent)| parent.is_none())
            .map(|(r, _)| r.full_metrics().physical)
            .sum();
        assert_eq!(represented, 13 * MIB);
        assert_eq!(represented, answer.ranking.target.metrics.physical);
        assert_eq!(
            rows.iter().map(|r| r.full_metrics().physical).sum::<u64>(),
            20 * MIB
        );
        // No marker references a parent omitted by the answer's limit.
        for limit in [0, 1, 2, 3] {
            let limited = full_rows(&answer.ranking, &answer.services, q.by, limit);
            for parent in inclusions(&limited).into_iter().flatten() {
                assert!(limited.iter().any(|r| r.path() == parent));
            }
        }
    }

    /// The accounted report of the volume `/v` holding `items`.
    fn accounted_report(items: Vec<TreeItem>) -> Report {
        Report::parse(accounted_report_bytes(items)).unwrap()
    }

    fn accounted_report_bytes(items: Vec<TreeItem>) -> Vec<u8> {
        accounted_with_refs(items, &[])
    }

    fn accounted_with_refs(items: Vec<TreeItem>, extents: &[(u64, u64, u64)]) -> Vec<u8> {
        struct Roots;
        impl crate::model::accounting::RootClass for Roots {
            fn external_root(&self, _: u64) -> Blockers {
                Blockers::UNKNOWN
            }
            fn shared_leaf(&self, _: u64) -> Blockers {
                Blockers::UNKNOWN
            }
        }
        let ns = read_tree(5, items).unwrap();
        let mut tree = ScopeTree::new();
        tree.add_root(
            &ns,
            256,
            b"/v",
            RootKind::Volume,
            &SubvolumeSet::default(),
            &Marks::default(),
        )
        .unwrap();
        tree.prepare();
        let mut accountant = crate::model::tree::Accountant::new(&tree, Some(&Roots));
        for &(bytenr, num_bytes, objectid) in extents {
            let refs = crate::btrfs::extent_refs::ExtentRefs {
                num_bytes,
                refs: 1,
                generation: 1,
                malformed: false,
                backrefs: vec![crate::btrfs::backref::DataBackref::Direct {
                    root: 5,
                    objectid,
                    offset: 0,
                    count: 1,
                }],
            };
            accountant.extent(bytenr, Some(&refs));
        }
        let aggregate = accountant.finish();
        let header = header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        let mut bytes = Vec::new();
        crate::report::write(&mut bytes, &header, &tree, aggregate.totals()).unwrap();
        bytes
    }

    #[test]
    fn with_service_rows_folders_rank_without_the_recycle_bin_they_hold() {
        let report = accounted_report(vec![
            inode(256, DIR, 0, 1),
            entry(256, 2, "share", 257),
            inode(257, DIR, 0, 1),
            entry(257, 2, "doc", 258),
            entry(257, 3, "#recycle", 259),
            inode(258, FILE, MIB, 1),
            whole_extent(258, 0, 10 * MIB, MIB),
            inode(259, DIR, 0, 1),
            entry(259, 2, "old", 260),
            inode(260, FILE, 4 * MIB, 1),
            whole_extent(260, 0, 20 * MIB, 4 * MIB),
        ]);
        let q = TopQuery {
            services: true,
            by: Measure::Logical,
            ..query(b"/v", KindFilter::All)
        };
        let mut walk = TopWalk::new(&report.header.mount, &q);
        report.for_each(|path, record| walk.visit(path, record));
        let answer = walk.finish().unwrap();
        let rows = rows(&answer.ranking, &answer.services, Measure::Logical, 10);
        let shown: Vec<(String, u64)> = rows
            .iter()
            .map(|row| match row {
                Row::Entry(e) => (
                    String::from_utf8(e.path.clone()).unwrap(),
                    row.metrics().logical / MIB,
                ),
                Row::Service(u) => (format!("{:?}", u.service.map(|s| s.code)), 0),
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("/v/share/#recycle".to_string(), 4),
                ("/v/share".to_string(), 1)
            ]
        );
        let recycle = answer.services.uses[0].metrics.physical;
        assert_eq!(
            answer.overlaps,
            [PackageOverlap {
                target: b"/v/share".to_vec(),
                service: crate::dsm::service(b"#recycle", crate::dsm::Depth::Below).unwrap(),
                service_path: Some(b"/v/share/#recycle".to_vec()),
                bytes: recycle,
            }]
        );
    }

    #[test]
    fn with_service_rows_folders_rank_without_the_package_data_they_hold() {
        let report = package_report();
        let ranking = |services| {
            let q = TopQuery {
                services,
                by: Measure::Logical,
                ..query(b"/v", KindFilter::All)
            };
            top(&report, &q).unwrap()
        };
        let logical = |entries: &[Entry], without: bool| {
            entries
                .iter()
                .map(|e| {
                    let m = if without {
                        e.metrics_without_package_data()
                    } else {
                        &e.metrics
                    };
                    (String::from_utf8(e.path.clone()).unwrap(), m.logical / MIB)
                })
                .collect::<Vec<_>>()
        };
        let with_rows = ranking(true);
        assert_eq!(
            logical(&with_rows.entries, true),
            [("/v/big".to_string(), 3), ("/v/share".to_string(), 1)]
        );
        assert_eq!(
            logical(&with_rows.entries, false),
            [("/v/big".to_string(), 3), ("/v/share".to_string(), 6)],
            "entries keep their full values"
        );
        let services = ServiceSummary::default();
        let rows = rows(&with_rows, &services, Measure::Logical, 10);
        let order: Vec<u64> = rows.iter().map(|r| r.metrics().logical / MIB).collect();
        assert_eq!(order, [3, 1]);
        assert_eq!(
            logical(&ranking(false).entries, false),
            [("/v/share".to_string(), 6), ("/v/big".to_string(), 3)]
        );
    }

    #[test]
    fn a_target_inside_a_service_lists_its_detail_rows() {
        let report = service_report();
        let r = top(&report, &query(b"/v/@synologydrive", KindFilter::All)).unwrap();
        assert_eq!(ranked(&r), [("/v/@synologydrive/x".to_string(), 4)]);
        assert_eq!(r.hidden_system, 0);
    }

    #[test]
    fn bounded_ranking_matches_exhaustive_sort_for_values_ties_and_package_metrics() {
        let report = report();
        let mut records = Vec::new();
        report.for_each(|path, row| {
            if path != b"/v" {
                records.push((path.to_vec(), *row));
            }
            ControlFlow::Continue(())
        });
        for by in [
            Measure::Logical,
            Measure::Physical,
            Measure::Reclaimable,
            Measure::Shared,
            Measure::Held,
            Measure::SharedRatio,
        ] {
            for services in [false, true] {
                for reverse in [false, true] {
                    for limit in [0, 1, 2, 5, 20] {
                        let q = TopQuery {
                            by,
                            services,
                            limit,
                            include_system: true,
                            ..query(b"/v", KindFilter::Files)
                        };
                        let mut ranker = Ranker::new(&q);
                        let mut candidates = Vec::new();
                        for n in 0..1000 {
                            let slot = if reverse { 999 - n } else { n };
                            let (original, row) = &records[slot % records.len()];
                            let mut row = *row;
                            row.kind = FileKind::Regular;
                            row.flags = NodeFlags::default();
                            row.metrics.physical = (slot % 7) as u64;
                            row.metrics.reclaimable = (slot % 5) as u64;
                            row.outside = (slot % 3 == 0).then_some(Outside {
                                metrics: Metrics {
                                    logical: (slot % 11) as u64,
                                    physical: (slot % 13) as u64,
                                    reclaimable: (slot % 7) as u64,
                                    ..Metrics::default()
                                },
                                held: Held::default(),
                            });
                            let mut path = original.clone();
                            path.extend_from_slice(format!("/candidate{slot:04}").as_bytes());
                            let entry = Entry::new(&path, &row);
                            let metrics = if services {
                                entry.metrics_without_package_data()
                            } else {
                                &entry.metrics
                            };
                            candidates.push(Candidate {
                                value: by.of(metrics),
                                entry,
                            });
                            ranker.add(0, &path, &row, Step::Outside);
                        }
                        candidates.sort_unstable_by(|a, b| b.cmp(a));
                        let expected: Vec<_> = candidates
                            .into_iter()
                            .take(limit)
                            .map(|c| c.entry)
                            .collect();
                        let target = Entry::new(b"/v", &records[0].1);
                        let got = ranker.finish(target);
                        assert_eq!(got.entries, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn ties_are_ordered_by_path() {
        let r = top(&report(), &query(b"/v", KindFilter::All)).unwrap();
        // Both 3 MiB: `/v/big` sorts before `/v/dir`.
        assert_eq!(r.entries[0].path, b"/v/big");
    }

    #[test]
    fn ranks_files_at_any_depth_and_honours_the_limit() {
        let mut q = query(b"/v", KindFilter::Files);
        q.include_system = true;
        q.limit = 2;
        let r = top(&report(), &q).unwrap();
        assert_eq!(
            ranked(&r),
            [
                ("/v/@eaDir/thumb".to_string(), 5),
                ("/v/big".to_string(), 3)
            ]
        );
        assert_eq!(r.hidden_system, 0);
    }

    #[test]
    fn ranks_below_a_subdirectory_only() {
        let r = top(&report(), &query(b"/v/dir", KindFilter::All)).unwrap();
        assert_eq!(
            ranked(&r),
            [
                ("/v/dir/mid".to_string(), 2),
                ("/v/dir/small".to_string(), 1)
            ]
        );
        assert!(top(&report(), &query(b"/v/nope", KindFilter::All)).is_none());
    }

    #[test]
    fn measures_reclaimable_and_shared_bytes() {
        let m = Metrics {
            logical: 9,
            physical: 10,
            reclaimable: 6,
            unknown: 3,
            ..Metrics::default()
        };
        assert_eq!(Measure::Reclaimable.of(&m), 6);
        assert_eq!(Measure::Shared.of(&m), 1);
        assert_eq!(Measure::Held.of(&m), 4, "shared and unknown bytes");
        assert_eq!(Measure::SharedRatio.of(&m), 100_000);
        assert_eq!(Measure::SharedRatio.of(&Metrics::default()), 0);
    }

    #[test]
    fn only_shared_entries_can_be_ranked() {
        let tree = report_tree();
        let mut metrics = tree.aggregate();
        for (n, m) in metrics.iter_mut().enumerate() {
            if tree.path(n as u32) != b"/v/big" {
                m.unknown = m.physical;
            }
        }
        let blockers = vec![Blockers::default(); tree.len()];
        let header = report().header;
        let report = Report::parse(encode_accounted(&header, &tree, &metrics, &blockers)).unwrap();
        let mut q = query(b"/v", KindFilter::Files);
        q.only_shared = true;
        assert_eq!(
            ranked(&top(&report, &q).unwrap()),
            [("/v/big".to_string(), 3)]
        );
    }

    #[test]
    fn entries_carry_their_blockers() {
        let report = report();
        let blockers = vec![Blockers::SNAPSHOT; report_tree().len()];
        let report = Report::parse(encode_accounted(
            &report.header,
            &report_tree(),
            &report_tree().aggregate(),
            &blockers,
        ))
        .unwrap();
        let found = find(&report, b"/v/big").unwrap();
        assert_eq!(found.blockers, Blockers::SNAPSHOT);
        assert_eq!(found.retained(), Held::of(HeldClass::Snapshot, 3 * MIB));
        assert_eq!(
            find(&report, b"/v/dir").unwrap().retained(),
            Held::default()
        );
    }

    #[test]
    fn a_selection_combines_its_roots_and_reports_the_together_bonus() {
        let tree = report_tree();
        let mut metrics = tree.aggregate();
        let roots: Vec<usize> = (0..tree.len() as u32)
            .filter(|&n| [&b"/v/big"[..], b"/v/dir"].contains(&tree.path(n).as_slice()))
            .map(|n| n as usize)
            .collect();
        metrics[roots[0]].reclaimable = MIB;
        metrics[roots[1]].reclaimable = 2 * MIB;
        let mut header = header(
            [&b"/v/big"[..], b"/v/dir"]
                .iter()
                .map(|p| RootInfo {
                    path: p.to_vec(),
                    scope: Scope::Path,
                    tree: 5,
                    ino: 0,
                })
                .collect(),
        );
        header.totals = Metrics {
            reclaimable: 5 * MIB,
            ..metrics[0]
        };
        let blockers = vec![Blockers::default(); tree.len()];
        let report = Report::parse(encode_accounted(&header, &tree, &metrics, &blockers)).unwrap();
        let s = selection(&report);
        let paths: Vec<&[u8]> = s.roots.iter().map(|e| e.path.as_slice()).collect();
        assert_eq!(paths, [&b"/v/big"[..], b"/v/dir"]);
        assert_eq!(s.combined.reclaimable, 5 * MIB);
        assert_eq!(s.together_bonus, 2 * MIB);
    }

    #[test]
    fn finds_paths_and_reports_coverage() {
        let report = report();
        let found = find(&report, b"/v/dir/mid").unwrap();
        assert_eq!((found.kind, found.objectid), (FileKind::Regular, 262));
        assert!(find(&report, b"/v/di").is_none());
        assert!(covers(&report.header, b"/v"));
        assert!(covers(&report.header, b"/v/dir/mid"));
        assert!(!covers(&report.header, b"/vx"));
    }

    #[test]
    fn finds_a_path_in_a_streamed_report() {
        let bytes = report_bytes();
        for (path, objectid) in [(&b"/v/dir/mid"[..], Some(262)), (b"/v/di", None)] {
            let mut find = Find::new(path);
            let reader = ReportReader::open(bytes.as_slice()).unwrap();
            reader.for_each(|p, record| find.visit(p, record)).unwrap();
            assert_eq!(find.finish().map(|entry| entry.objectid), objectid);
        }
    }
}
