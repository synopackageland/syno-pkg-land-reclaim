//! What DSM service and hidden data uses below a target (survey item A1):
//! the known service paths of [`crate::dsm`], other `@…` volume-root
//! entries, and snapshots that were not read. Rankings keep hiding DSM
//! data (SPEC 4.6); this summary shows it.

use crate::dsm::{self, Depth, Service};
use crate::model::accounting::Held;
use crate::model::tree::{Metrics, NodeFlags};
use crate::query::Entry;
use crate::report::NodeRecord;
#[cfg(test)]
use crate::report::Report;
use std::ops::ControlFlow;

/// Paths a line of several paths names.
pub const LISTED: usize = 20;

/// One summary line: one path, or several paths of one service added up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceUse {
    /// `None` for `@…` volume-root entries the table does not know.
    pub service: Option<&'static Service>,
    /// The path, when the service lists each path.
    pub path: Option<Vec<u8>>,
    /// Paths added up.
    pub paths: u64,
    /// Sums of each path's own values: a footprint shared by two paths
    /// counts twice, and reclaimable bytes are each path deleted alone.
    pub metrics: Metrics,
    /// Sums of each path's retained bytes per holder class.
    pub held: Held,
    /// The entry, when the service lists each path.
    pub entry: Option<Entry>,
    /// The first `LISTED` paths added up.
    pub listed: Vec<Vec<u8>>,
    /// Snapshot boundaries inside whose trees were not read.
    pub snapshots_not_read: u64,
    /// Some metadata paths are already counted by eligible ranking entries.
    pub included_in_rows: bool,
    /// Metadata paths without a covering ranking entry. Used for ranking
    /// when the summary also includes covered paths; never contains a subset.
    pub standalone: Option<Box<ServiceUse>>,
    /// Complete path coverage, unlike the bounded public path sample.
    pub common_path: Vec<u8>,
    /// Service subtrees whose bytes were subtracted from this line.
    pub excluded_paths: Vec<Vec<u8>>,
    /// Full-metric ranking partitions by eligible containing folder, computed
    /// before the limit. The public service summary remains aggregated.
    pub full_parts: Vec<ServiceUse>,
    pub covering_folder: Option<Vec<u8>>,
}

impl ServiceUse {
    fn exclude(&mut self, path: &[u8], entry: &Entry) {
        self.metrics.sub(&entry.metrics);
        self.held.sub(&entry.retained());
        self.excluded_paths.push(path.to_vec());
    }

    fn add_path(&mut self, path: &[u8], record: &NodeRecord<'_>) {
        let entry = Entry::new(path, record);
        if self.paths == 0 {
            self.common_path = path.to_vec();
        } else {
            while !is_within(&self.common_path, path) {
                let cut = self
                    .common_path
                    .iter()
                    .rposition(|b| *b == b'/')
                    .unwrap_or(0);
                self.common_path.truncate(cut);
            }
        }
        self.paths += 1;
        self.metrics.add(&record.metrics);
        if self.service.is_some_and(|s| s.code == "syno_reclaim") {
            self.metrics.reclaimable = 0;
        }
        self.held.add(&entry.retained());
        self.snapshots_not_read += u64::from(record.flags.contains(NodeFlags::SNAPSHOT));
        if self.listed.len() < LISTED {
            self.listed.push(path.to_vec());
        }
        if self.path.is_some() {
            self.entry = Some(entry);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceSummary {
    /// Largest footprint first.
    pub uses: Vec<ServiceUse>,
    /// Snapshot boundaries below the target outside every line.
    pub other_snapshots_not_read: u64,
}

/// Where an entry below a target counts: each path in its topmost
/// service only, except that a service nested in another (a VMM virtual
/// disk in `@iSCSI`) takes its own path out of it. `None` services are other `@…` volume-root entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The entry starts a service path.
    Starts(Option<&'static Service>),
    /// The entry lies inside the service path started above it.
    Inside(Option<&'static Service>),
    Outside,
}

/// Classifies the entries below a target, fed in preorder, by the
/// service path each lies in. With `marked`, a folder holding a package
/// marker is a path of the marked service, so all it holds counts there
/// once.
pub struct ServiceWalk<'a> {
    mount: &'a [u8],
    marked: bool,
    /// The depth and service of the open service path.
    open: Option<(u32, Option<&'static Service>)>,
    /// The depth and service of the nested service path open inside it.
    nested: Option<(u32, &'static Service)>,
}

impl<'a> ServiceWalk<'a> {
    pub fn new(mount: &'a [u8], marked: bool) -> Self {
        ServiceWalk {
            mount,
            marked,
            open: None,
            nested: None,
        }
    }

    /// `Starts` of the service nested in the open one that `path` is.
    fn start_nested(&mut self, path: &[u8], record: &NodeRecord<'_>) -> Option<Step> {
        let rest = path.strip_prefix(self.mount)?.strip_prefix(b"/")?;
        let service = if record.flags.contains(NodeFlags::OWN_STATE) {
            dsm::syno_reclaim()
        } else {
            dsm::service_within(rest)?
        };
        self.nested = Some((record.depth, service));
        Some(Step::Starts(Some(service)))
    }

    pub fn step(&mut self, path: &[u8], record: &NodeRecord<'_>) -> Step {
        if self.open.is_some_and(|(d, _)| record.depth <= d) {
            self.open = None;
        }
        if self.nested.is_some_and(|(d, _)| record.depth <= d) {
            self.nested = None;
        }
        if let Some((_, service)) = self.nested {
            return Step::Inside(Some(service));
        }
        if let Some((_, service)) = self.open {
            if service.is_some_and(|s| s.code == "syno_reclaim") {
                return Step::Inside(service);
            }
            return self
                .start_nested(path, record)
                .unwrap_or(Step::Inside(service));
        }
        let at = depth_of(path, self.mount);
        let found = record
            .flags
            .contains(NodeFlags::OWN_STATE)
            .then_some(dsm::syno_reclaim())
            .or_else(|| dsm::service_within(path.strip_prefix(self.mount)?.strip_prefix(b"/")?))
            .or_else(|| {
                record
                    .marker
                    .filter(|_| self.marked)
                    .and_then(dsm::marked_by)
                    .or_else(|| dsm::service(record.name, at))
            });
        let other_service_root = at == Depth::VolumeRoot && record.name.starts_with(b"@");
        if found.is_none() && !other_service_root {
            return Step::Outside;
        }
        self.open = Some((record.depth, found));
        Step::Starts(found)
    }
}

/// The entries below a target, in records fed in preorder, each with the
/// service path it lies in.
pub struct Below<'a> {
    under: &'a [u8],
    /// The target and its depth, once found.
    target: Option<(Entry, u32)>,
    walk: ServiceWalk<'a>,
}

impl<'a> Below<'a> {
    /// `marked` as for [`ServiceWalk::new`].
    pub fn new(mount: &'a [u8], under: &'a [u8], marked: bool) -> Self {
        Below {
            under,
            target: None,
            walk: ServiceWalk::new(mount, marked),
        }
    }

    /// Feeds one record; `entry` gets each entry below the target, with
    /// the target's depth and where the entry lies. Breaks once past
    /// the target.
    pub fn visit(
        &mut self,
        path: &[u8],
        record: &NodeRecord<'_>,
        entry: impl FnOnce(u32, Step),
    ) -> ControlFlow<()> {
        let Some((_, depth)) = &self.target else {
            if path == self.under {
                self.target = Some((Entry::new(path, record), record.depth));
            }
            return ControlFlow::Continue(());
        };
        if record.depth <= *depth {
            return ControlFlow::Break(());
        }
        entry(*depth, self.walk.step(path, record));
        ControlFlow::Continue(())
    }

    /// The target, when the records held it.
    pub fn target(self) -> Option<Entry> {
        self.target.map(|(entry, _)| entry)
    }
}

/// Adds up the service paths below a target, fed as [`Below`] finds them.
#[derive(Debug, Default)]
pub struct ServiceTally {
    summary: ServiceSummary,
    /// The lines of the open service path and of the service paths nested
    /// in it, outermost first.
    open: Vec<(usize, Option<&'static Service>, bool, Option<usize>)>,
    /// Each service path started that ranked folders leave out, with its
    /// footprint.
    packages: Vec<(Vec<u8>, &'static Service, u64)>,
}

impl ServiceTally {
    #[cfg(test)]
    pub fn add(&mut self, path: &[u8], record: &NodeRecord<'_>, step: Step) {
        self.add_with_coverage(path, record, step, false);
    }

    pub fn add_with_coverage(
        &mut self,
        path: &[u8],
        record: &NodeRecord<'_>,
        step: Step,
        covered: bool,
    ) {
        self.add_for_display(path, record, step, covered, None);
    }

    /// `folder` partitions volume full-metric rows by eligible ancestors;
    /// `None` keeps the original summary/ranking behavior.
    pub fn add_for_display(
        &mut self,
        path: &[u8],
        record: &NodeRecord<'_>,
        step: Step,
        covered: bool,
        folder: Option<Option<&[u8]>>,
    ) {
        let summary = &mut self.summary;
        let snapshot = u64::from(record.flags.contains(NodeFlags::SNAPSHOT));
        match step {
            Step::Inside(inside) => {
                // Leaving a nested service path returns to the one holding it.
                while self.open.len() > 1
                    && self.open.last().map(|(_, s, _, _)| s.map(|s| s.code))
                        != Some(inside.map(|s| s.code))
                {
                    self.open.pop();
                }
                let (open, _, standalone, part) = self.open[self.open.len() - 1];
                summary.uses[open].snapshots_not_read += snapshot;
                if let Some(part) = part {
                    summary.uses[open].full_parts[part].snapshots_not_read += snapshot;
                }
                if standalone {
                    summary.uses[open]
                        .standalone
                        .as_mut()
                        .unwrap()
                        .snapshots_not_read += snapshot;
                }
            }
            Step::Outside => summary.other_snapshots_not_read += snapshot,
            Step::Starts(found) => {
                let open = line(&mut summary.uses, found, path, None);
                let entry = Entry::new(path, record);
                if found.is_some_and(|s| s.is_nested()) && !self.open.is_empty() {
                    // Its bytes are counted here, not in the service holding it.
                    let (outer, _, _, part) = self.open[self.open.len() - 1];
                    summary.uses[outer].exclude(path, &entry);
                    if let Some(part) = part {
                        summary.uses[outer].full_parts[part].exclude(path, &entry);
                    }
                } else {
                    self.open.clear();
                }
                let metadata = found.is_some_and(|s| s.code == "folder_metadata");
                let standalone = metadata && !covered;
                let part = folder.map(|parent| {
                    let part = line(&mut summary.uses[open].full_parts, found, path, parent);
                    summary.uses[open].full_parts[part].add_path(path, record);
                    part
                });
                self.open.push((open, found, standalone, part));
                let used = &mut summary.uses[open];
                used.add_path(path, record);
                used.included_in_rows |= metadata && covered;
                if standalone {
                    used.standalone
                        .get_or_insert_with(|| {
                            Box::new(ServiceUse {
                                service: found,
                                ..ServiceUse::default()
                            })
                        })
                        .add_path(path, record);
                }
                if let Some(service) = found.filter(|service| service.left_out_of_folders()) {
                    self.packages
                        .push((path.to_vec(), service, record.metrics.physical));
                }
            }
        }
    }

    /// The summary, and the package data below it that `ranked` entries
    /// leave out.
    pub fn finish(self, ranked: &[Entry]) -> (ServiceSummary, Vec<PackageOverlap>) {
        let mut summary = self.summary;
        summary
            .uses
            .sort_by_key(|u| std::cmp::Reverse(u.metrics.physical));
        let mut overlaps = Vec::new();
        for (path, service, bytes) in &self.packages {
            add_package_overlap(&mut overlaps, ranked, path, service, *bytes);
        }
        (summary, overlaps)
    }
}

/// The service summary of the entries below `under`, or `None` when the
/// report lacks it. Each path counts in its topmost service only.
#[cfg(test)]
pub fn service_summary(report: &Report, under: &[u8]) -> Option<ServiceSummary> {
    service_summary_with_overlaps(report, under, &[]).map(|(summary, _)| summary)
}

/// The service summary and package data below it that a ranked entry
/// leaves out.
#[cfg(test)]
pub fn service_summary_with_overlaps(
    report: &Report,
    under: &[u8],
    ranked: &[Entry],
) -> Option<(ServiceSummary, Vec<PackageOverlap>)> {
    let mut below = Below::new(&report.header.mount, under, true);
    let mut tally = ServiceTally::default();
    report.for_each(|path, record| {
        below.visit(path, record, |_, step| tally.add(path, record, step))
    });
    below.target()?;
    Some(tally.finish(ranked))
}

/// Where package-managed data (SPEC 5.7) starts in a report: a guarded
/// service path, or a folder holding a package marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot {
    pub path: Vec<u8>,
    pub service: &'static Service,
    /// The marker entry, when a marker made the folder package data.
    pub marker: Option<&'static [u8]>,
}

/// Package data below a ranked directory that its row leaves out, as a
/// service row counts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageOverlap {
    pub target: Vec<u8>,
    pub service: &'static Service,
    /// Present when each service path has its own row.
    pub service_path: Option<Vec<u8>>,
    pub bytes: u64,
}

/// The package roots of `report`, in preorder.
#[cfg(test)]
pub fn package_roots(report: &Report) -> Vec<PackageRoot> {
    let mut walk = PackageRoots::new(&report.header.mount);
    report.for_each(|path, record| {
        walk.visit(path, record);
        ControlFlow::Continue(())
    });
    walk.finish()
}

/// Gathers the package roots below `mount` from records fed in preorder.
pub struct PackageRoots<'m> {
    mount: &'m [u8],
    roots: Vec<PackageRoot>,
}

impl<'m> PackageRoots<'m> {
    pub fn new(mount: &'m [u8]) -> Self {
        PackageRoots {
            mount,
            roots: Vec::new(),
        }
    }

    /// Feeds one record.
    pub fn visit(&mut self, path: &[u8], record: &NodeRecord<'_>) {
        self.roots.extend(package_root(self.mount, path, record));
    }

    /// In preorder.
    pub fn finish(self) -> Vec<PackageRoot> {
        self.roots
    }
}

/// The package root `record` at `path`, below `mount`, is, if any.
/// Its own name names it before a marker it holds does.
pub fn package_root(mount: &[u8], path: &[u8], record: &NodeRecord<'_>) -> Option<PackageRoot> {
    let at = depth_of(path, mount);
    let by_name = record
        .flags
        .contains(NodeFlags::OWN_STATE)
        .then_some(dsm::syno_reclaim())
        .or_else(|| dsm::service_of(path, mount).filter(|s| s.code == "syno_reclaim"))
        .or_else(|| dsm::service(record.name, at))
        .filter(|s| s.guarded)
        .filter(|_| at == Depth::VolumeRoot || dsm::marked_by(record.name).is_none())
        .map(|service| (service, None));
    let by_marker = || {
        record.marker.map(|marker| {
            let service = dsm::marked_by(marker).expect("a marker names its service");
            (service, Some(marker))
        })
    };
    let (service, marker) = by_name.or_else(by_marker)?;
    Some(PackageRoot {
        path: path.to_vec(),
        service,
        marker,
    })
}

fn add_package_overlap(
    overlaps: &mut Vec<PackageOverlap>,
    ranked: &[Entry],
    path: &[u8],
    service: &'static Service,
    bytes: u64,
) {
    for entry in ranked
        .iter()
        .filter(|entry| strict_descendant(&entry.path, path))
    {
        let service_path = service.list_each.then(|| path.to_vec());
        let existing = overlaps.iter_mut().find(|overlap| {
            overlap.target == entry.path
                && overlap.service.code == service.code
                && overlap.service_path == service_path
        });
        if let Some(overlap) = existing {
            overlap.bytes = overlap.bytes.saturating_add(bytes);
        } else {
            overlaps.push(PackageOverlap {
                target: entry.path.clone(),
                service,
                service_path,
                bytes,
            });
        }
    }
}

pub(crate) fn is_within(parent: &[u8], child: &[u8]) -> bool {
    child == parent || strict_descendant(parent, child)
}

fn strict_descendant(parent: &[u8], child: &[u8]) -> bool {
    child.len() > parent.len()
        && child.starts_with(parent)
        && (parent.ends_with(b"/") || child.get(parent.len()) == Some(&b'/'))
}

fn depth_of(path: &[u8], mount: &[u8]) -> Depth {
    let cut = path.iter().rposition(|b| *b == b'/').unwrap_or(0);
    let parent = &path[..cut];
    if parent == mount {
        Depth::VolumeRoot
    } else {
        Depth::Below
    }
}

/// The index of the line `path` of `service` adds to, created if new.
fn line(
    uses: &mut Vec<ServiceUse>,
    service: Option<&'static Service>,
    path: &[u8],
    folder: Option<&[u8]>,
) -> usize {
    let each = service.is_some_and(|s| s.list_each);
    let existing = uses.iter().position(|u| {
        !each
            && u.path.is_none()
            && u.service.map(|s| s.code) == service.map(|s| s.code)
            && u.covering_folder.as_deref() == folder
    });
    existing.unwrap_or_else(|| {
        uses.push(ServiceUse {
            service,
            covering_folder: folder.map(Vec::from),
            path: each.then(|| path.to_vec()),
            paths: 0,
            metrics: Metrics::default(),
            held: Held::default(),
            entry: None,
            listed: Vec::new(),
            snapshots_not_read: 0,
            ..ServiceUse::default()
        });
        uses.len() - 1
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::accounting::{Blockers, Held, HeldClass};
    use crate::model::namespace::FileKind;
    use crate::report::testing::header;
    use crate::report::{RootInfo, encode_record, write_records};
    use crate::target::Scope;

    #[test]
    fn this_app_state_is_its_own_line_at_custom_and_package_paths() {
        for nodes in [
            vec![
                (0, "/v", NodeFlags::default(), 7, 7),
                (1, "state", NodeFlags::OWN_STATE, 7, 7),
            ],
            vec![
                (0, "/v", NodeFlags::default(), 10, 10),
                (1, "@appdata", NodeFlags::SYSTEM, 10, 10),
                (2, "SynoReclaim", NodeFlags::SYSTEM, 7, 7),
                (3, "state", NodeFlags::OWN_STATE, 7, 7),
            ],
        ] {
            let summary = service_summary(&report(&nodes), b"/v").unwrap();
            let own = summary
                .uses
                .iter()
                .find(|u| u.service.is_some_and(|s| s.code == "syno_reclaim"))
                .unwrap();
            assert_eq!(own.metrics.physical, 7 * MIB);
            if let Some(other) = summary
                .uses
                .iter()
                .find(|u| u.service.is_some_and(|s| s.code == "package_data"))
            {
                assert_eq!(other.metrics.physical, 3 * MIB);
            }
        }
    }

    const MIB: u64 = 1 << 20;

    /// `(depth, name, flags, physical MiB, reclaimable MiB)` in preorder;
    /// the first is the root `/v`.
    fn report(nodes: &[(u32, &str, NodeFlags, u64, u64)]) -> Report {
        let records: Vec<NodeRecord> = nodes
            .iter()
            .map(|&(depth, name, flags, physical, reclaimable)| {
                dir(depth, name, flags, physical, reclaimable)
            })
            .collect();
        encode(&records)
    }

    fn dir(
        depth: u32,
        name: &str,
        flags: NodeFlags,
        physical: u64,
        reclaimable: u64,
    ) -> NodeRecord<'_> {
        NodeRecord {
            depth,
            kind: FileKind::Directory,
            flags,
            name: name.as_bytes(),
            metrics: Metrics {
                physical: physical * MIB,
                reclaimable: reclaimable * MIB,
                ..Metrics::default()
            },
            blockers: Blockers::default(),
            held: Held::default(),
            marker: None,
            outside: None,
            nlink: 1,
            tree: 5,
            objectid: 256,
        }
    }

    /// A report of `records` in preorder below the root `/v`.
    fn encode(records: &[NodeRecord]) -> Report {
        Report::parse(encode_bytes(records)).unwrap()
    }

    fn encode_bytes(records: &[NodeRecord]) -> Vec<u8> {
        let header = header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        let mut out = Vec::new();
        write_records(&mut out, &header, records.len() as u64, |w| {
            records.iter().try_for_each(|r| encode_record(w, r))
        })
        .unwrap();
        out
    }

    const NONE: NodeFlags = NodeFlags::from_bits(0);
    const SYSTEM: NodeFlags = NodeFlags::SYSTEM;

    fn lines(summary: &ServiceSummary) -> Vec<(String, Option<String>, u64, u64)> {
        summary
            .uses
            .iter()
            .map(|u| {
                (
                    u.service.map_or("other", |s| s.code).to_string(),
                    u.path
                        .as_ref()
                        .map(|p| String::from_utf8(p.clone()).unwrap()),
                    u.paths,
                    u.metrics.physical / MIB,
                )
            })
            .collect()
    }

    pub(crate) fn eadir_report_bytes() -> Vec<u8> {
        let nodes = [
            (0, "/v", NONE, 22, 12),
            (1, "@eaDir", SYSTEM, 2, 1),
            (2, "thumb", NONE, 2, 1),
            (1, "share", NodeFlags::SUBVOLUME_ROOT, 20, 11),
            (2, "@eaDir", SYSTEM, 3, 2),
            (3, "thumb", NONE, 3, 2),
            (2, "album", NONE, 10, 5),
            (3, "@eaDir", SYSTEM, 4, 2),
            (4, "thumb", NONE, 4, 2),
            (3, "photo", NONE, 6, 3),
            (2, "docs", NONE, 7, 4),
        ];
        let records: Vec<_> = nodes
            .iter()
            .map(|&(depth, name, flags, physical, reclaimable)| {
                let mut r = dir(depth, name, flags, physical, reclaimable);
                r.metrics.logical = physical * MIB;
                if matches!(name, "thumb" | "photo" | "docs") {
                    r.kind = FileKind::Regular;
                    r.blockers = Blockers::SNAPSHOT;
                } else {
                    r.held = Held::of(HeldClass::Snapshot, (physical - reclaimable) * MIB);
                }
                r
            })
            .collect();
        encode_bytes(&records)
    }

    #[test]
    fn eadir_rankings_count_nested_and_root_metadata_once() {
        let r = Report::parse(eadir_report_bytes()).unwrap();
        for (under, physical, reclaimable) in [
            (&b"/v"[..], 22, 12),
            (b"/v/share", 20, 11),
            (b"/v/share/album", 10, 5),
            (b"/v/share/album/@eaDir", 4, 2),
        ] {
            for include_system in [false, true] {
                let q = crate::query::TopQuery {
                    under,
                    services: true,
                    by: crate::query::Measure::Physical,
                    kind: crate::query::KindFilter::All,
                    include_system,
                    limit: 200,
                    only_shared: false,
                };
                let mut walk = crate::query::TopWalk::new(b"/v", &q);
                r.for_each(|p, record| walk.visit(p, record));
                let answer = walk.finish().unwrap();
                let rows = crate::query::rows(&answer.ranking, &answer.services, q.by, q.limit);
                assert_eq!(
                    rows.iter().map(|row| row.metrics().physical).sum::<u64>(),
                    physical * MIB,
                    "physical at {under:?}, include_system={include_system}"
                );
                assert_eq!(
                    rows.iter()
                        .map(|row| row.metrics().reclaimable)
                        .sum::<u64>(),
                    reclaimable * MIB
                );
                assert_eq!(
                    rows.iter().map(|row| row.metrics().shared()).sum::<u64>(),
                    (physical - reclaimable) * MIB
                );
                if under == b"/v" {
                    assert_eq!(
                        answer
                            .ranking
                            .entries
                            .iter()
                            .find(|e| e.path == b"/v/share")
                            .unwrap()
                            .metrics
                            .physical,
                        20 * MIB
                    );
                    assert_eq!(
                        answer
                            .services
                            .uses
                            .iter()
                            .find(|u| u.service.is_some_and(|s| s.code == "folder_metadata"))
                            .unwrap()
                            .metrics
                            .physical,
                        9 * MIB
                    );
                }
            }
        }
    }

    #[test]
    fn a_truncated_ranking_does_not_promote_metadata_from_omitted_folders() {
        let r = Report::parse(eadir_report_bytes()).unwrap();
        for by in [
            crate::query::Measure::Physical,
            crate::query::Measure::Reclaimable,
        ] {
            for limit in [0, 1, 2, 200] {
                let q = crate::query::TopQuery {
                    under: b"/v",
                    services: true,
                    by,
                    kind: crate::query::KindFilter::All,
                    include_system: false,
                    limit,
                    only_shared: false,
                };
                let mut walk = crate::query::TopWalk::new(b"/v", &q);
                r.for_each(|p, node| walk.visit(p, node));
                let answer = walk.finish().unwrap();
                let meta = &answer.services.uses[0];
                assert!(meta.included_in_rows);
                let standalone = meta.standalone.as_ref().unwrap();
                assert_eq!(standalone.metrics.physical, 2 * MIB);
                assert_eq!(standalone.listed, [b"/v/@eaDir".to_vec()]);
                let rows = crate::query::rows(&answer.ranking, &answer.services, by, limit);
                if limit > 0 {
                    assert_eq!(rows[0].metrics().physical, 20 * MIB);
                }
            }
        }
    }

    #[test]
    fn nested_metadata_is_information_and_shared_keeps_folder_values() {
        let r = Report::parse(eadir_report_bytes()).unwrap();
        for (kind, services, only_shared) in [
            (crate::query::KindFilter::Dirs, true, false),
            (crate::query::KindFilter::All, false, true),
        ] {
            let q = crate::query::TopQuery {
                under: b"/v",
                services,
                by: crate::query::Measure::Shared,
                kind,
                include_system: false,
                limit: 200,
                only_shared,
            };
            let mut walk = crate::query::TopWalk::new(b"/v", &q);
            r.for_each(|p, node| walk.visit(p, node));
            let answer = walk.finish().unwrap();
            let rows = crate::query::rows(&answer.ranking, &answer.services, q.by, q.limit);
            let metadata_rows: Vec<_> = rows
                .iter()
                .filter_map(|row| match row {
                    crate::query::Row::Service(line) => Some(line),
                    _ => None,
                })
                .collect();
            if services {
                assert_eq!(metadata_rows.len(), 1);
                assert_eq!(metadata_rows[0].metrics.physical, 2 * MIB);
            } else {
                assert!(metadata_rows.is_empty());
            }
            assert_eq!(rows[0].metrics().physical, 20 * MIB);
            assert_eq!(rows[0].metrics().reclaimable, 11 * MIB);
            if services {
                let json = crate::document::ServicesJson::from(&answer.services);
                assert!(json.lines[0].included_in_rows);
                assert_eq!(json.lines[0].metrics.physical, 9 * MIB);
            }
        }
    }

    #[test]
    fn volume_root_services_are_listed_by_path_largest_first() {
        let r = report(&[
            (0, "/v", NONE, 100, 0),
            (1, "@docker", SYSTEM, 5, 5),
            (1, "@synologydrive", SYSTEM, 40, 10),
            (2, "@sync", SYSTEM, 40, 10),
            (1, "photos", NONE, 55, 55),
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        assert_eq!(
            lines(&s),
            [
                (
                    "synology_drive".into(),
                    Some("/v/@synologydrive".into()),
                    1,
                    40
                ),
                ("container_manager".into(), Some("/v/@docker".into()), 1, 5),
            ]
        );
        assert_eq!(s.uses[0].metrics.reclaimable, 10 * MIB);
    }

    #[test]
    fn lines_add_up_what_holds_each_path_and_keep_a_single_paths_entry() {
        let mut drive = dir(1, "@synologydrive", SYSTEM, 40, 10);
        drive.held = Held::of(HeldClass::Snapshot, 30 * MIB);
        let mut a = dir(2, "@eaDir", SYSTEM, 2, 1);
        a.held = Held::of(HeldClass::HardLink, MIB);
        let mut thumb = dir(2, "@eaDir", SYSTEM, 1, 0);
        thumb.kind = FileKind::Regular;
        thumb.blockers = Blockers::SNAPSHOT;
        let r = encode(&[
            dir(0, "/v", NONE, 100, 0),
            drive,
            dir(1, "a", NONE, 10, 0),
            a,
            dir(1, "b", NONE, 10, 0),
            thumb,
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        let drive = &s.uses[0];
        assert_eq!(drive.held, Held::of(HeldClass::Snapshot, 30 * MIB));
        assert_eq!(drive.entry.as_ref().unwrap().path, b"/v/@synologydrive");
        let metadata = &s.uses[1];
        let mut both = Held::of(HeldClass::HardLink, MIB);
        both.add(&Held::of(HeldClass::Snapshot, MIB));
        assert_eq!(metadata.held, both);
        assert!(metadata.entry.is_none());
        assert_eq!(
            metadata.listed,
            [b"/v/a/@eaDir".to_vec(), b"/v/b/@eaDir".to_vec()]
        );
    }

    #[test]
    fn unknown_volume_root_service_entries_are_added_up_as_other() {
        let r = report(&[
            (0, "/v", NONE, 10, 0),
            (1, "@SYNO.a.core.gz", SYSTEM, 1, 1),
            (1, "@SYNO.b.core.gz", SYSTEM, 2, 2),
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        assert_eq!(lines(&s), [("other".into(), None, 2, 3)]);
    }

    #[test]
    fn omitted_outer_folder_does_not_make_a_service_aggregate_overlap_visible_inner_folders() {
        let r = report(&[
            (0, "/v", NONE, 20, 20),
            (1, "share", NONE, 20, 20),
            (2, "a", NONE, 10, 0),
            (3, "vault.hbk", SYSTEM, 6, 0),
            (2, "b", NONE, 10, 0),
            (3, "vault.hbk", SYSTEM, 6, 0),
        ]);
        let q = crate::query::TopQuery {
            under: b"/v",
            kind: crate::query::KindFilter::Dirs,
            by: crate::query::Measure::Held,
            services: true,
            limit: 2,
            include_system: false,
            only_shared: false,
        };
        let mut walk = crate::query::TopWalk::new(b"/v", &q);
        r.for_each(|p, n| walk.visit(p, n));
        let answer = walk.finish().unwrap();
        assert!(
            answer
                .ranking
                .entries
                .iter()
                .all(|entry| entry.path != b"/v/share")
        );
        let rows = crate::query::full_rows(&answer.ranking, &answer.services, q.by, q.limit);
        let represented: u64 = rows
            .iter()
            .zip(crate::query::inclusions(&rows))
            .filter(|(_, p)| p.is_none())
            .map(|(row, _)| row.full_metrics().physical)
            .sum();
        assert_eq!(represented, 20 * MIB);
    }

    #[test]
    fn mixed_service_paths_partition_before_limits_without_changing_the_summary() {
        let r = report(&[
            (0, "/v", NONE, 17, 17),
            (1, "share", NONE, 10, 10),
            (2, "doc", NONE, 7, 7),
            (2, "vault.hbk", SYSTEM, 3, 3),
            (1, "standalone.hbk", SYSTEM, 7, 7),
        ]);
        let q = crate::query::TopQuery {
            under: b"/v",
            kind: crate::query::KindFilter::All,
            by: crate::query::Measure::Physical,
            services: true,
            limit: 200,
            include_system: false,
            only_shared: false,
        };
        let mut walk = crate::query::TopWalk::new(b"/v", &q);
        r.for_each(|p, n| walk.visit(p, n));
        let answer = walk.finish().unwrap();
        assert_eq!(answer.services.uses[0].metrics.physical, 10 * MIB);
        assert_eq!(answer.services.uses[0].paths, 2);
        let rows = crate::query::full_rows(&answer.ranking, &answer.services, q.by, q.limit);
        let within = crate::query::inclusions(&rows);
        assert_eq!(within.iter().filter(|p| p.is_some()).count(), 1);
        let represented: u64 = rows
            .iter()
            .zip(within)
            .filter(|(_, p)| p.is_none())
            .map(|(row, _)| row.full_metrics().physical)
            .sum();
        assert_eq!(represented, 17 * MIB);
        assert_eq!(rows.len(), 3);
        // The reduced CLI ranking still has one aggregated service line.
        assert_eq!(
            crate::query::rows(&answer.ranking, &answer.services, q.by, q.limit).len(),
            2
        );
    }

    #[test]
    fn each_share_recycle_bin_is_its_own_line_and_metadata_is_summed() {
        let r = report(&[
            (0, "/v", NONE, 100, 0),
            (1, "a", NONE, 50, 0),
            (2, "#recycle", NONE, 20, 20),
            (3, "@eaDir", SYSTEM, 1, 1),
            (2, "@eaDir", SYSTEM, 2, 2),
            (1, "b", NONE, 50, 0),
            (2, "#recycle", NONE, 7, 7),
            (2, "album", NONE, 40, 0),
            (3, "@eaDir", SYSTEM, 3, 3),
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        assert_eq!(
            lines(&s),
            [
                ("recycle_bin".into(), Some("/v/a/#recycle".into()), 1, 20),
                ("recycle_bin".into(), Some("/v/b/#recycle".into()), 1, 7),
                ("folder_metadata".into(), None, 2, 5),
            ]
        );
    }

    #[test]
    fn snapshots_not_read_are_counted_where_they_sit() {
        let snapshot = NodeFlags::SNAPSHOT.with(NodeFlags::SUBVOLUME_ROOT);
        let r = report(&[
            (0, "/v", NONE, 10, 0),
            (1, "@sharesnap", SYSTEM, 0, 0),
            (2, "a", SYSTEM, 0, 0),
            (3, "GMT+08-2026.09.28", snapshot.with(SYSTEM), 0, 0),
            (1, "snaps", NONE, 0, 0),
            (2, "s1", snapshot, 0, 0),
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        assert_eq!(s.uses[0].service.unwrap().code, "share_snapshots");
        assert_eq!(s.uses[0].snapshots_not_read, 1);
        assert_eq!(s.other_snapshots_not_read, 1);
    }

    #[test]
    fn a_path_target_summarises_only_below_itself() {
        let r = report(&[
            (0, "/v", NONE, 100, 0),
            (1, "a", NONE, 50, 0),
            (2, "#recycle", NONE, 20, 20),
            (1, "b", NONE, 50, 0),
            (2, "#recycle", NONE, 7, 7),
        ]);
        let s = service_summary(&r, b"/v/b").unwrap();
        assert_eq!(
            lines(&s),
            [("recycle_bin".into(), Some("/v/b/#recycle".into()), 1, 7)]
        );
        assert!(service_summary(&r, b"/v/c").is_none());
    }

    /// `dir` of a folder `marker` makes package data.
    fn marked<'a>(depth: u32, name: &'a str, physical: u64, marker: &str) -> NodeRecord<'a> {
        let mut folder = dir(depth, name, NodeFlags::PACKAGE, physical, 0);
        folder.marker = dsm::markers().find(|m| *m == marker.as_bytes());
        folder
    }

    #[test]
    fn a_folder_holding_a_marker_is_one_path_of_its_service_with_all_it_holds() {
        let r = encode(&[
            dir(0, "/v", NONE, 100, 0),
            marked(1, "abb", 60, "@ActiveBackup"),
            dir(2, "@ActiveBackup", NONE, 50, 0),
            dir(2, "#recycle", NONE, 4, 4),
            dir(1, "@ActiveBackup", SYSTEM, 1, 1),
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        assert_eq!(
            lines(&s),
            [
                ("active_backup_data".into(), None, 1, 60),
                (
                    "active_backup".into(),
                    Some("/v/@ActiveBackup".into()),
                    1,
                    1
                ),
            ]
        );
        assert_eq!(s.uses[0].listed, [b"/v/abb".to_vec()]);
    }

    #[test]
    fn package_roots_are_guarded_service_paths_and_folders_holding_a_marker() {
        let r = encode(&[
            dir(0, "/v", NONE, 100, 0),
            dir(1, "@ActiveBackup", SYSTEM, 1, 1),
            dir(1, "surveillance", NONE, 5, 5),
            marked(1, "abb", 50, "@ActiveBackup"),
            dir(2, "@ActiveBackup", NONE, 50, 0),
            dir(1, "docs", NONE, 45, 0),
            dir(2, "x.hbk", NONE, 20, 0),
            dir(2, "@eaDir", SYSTEM, 1, 1),
        ]);
        let roots: Vec<(String, &str, Option<&[u8]>)> = package_roots(&r)
            .iter()
            .map(|root| {
                (
                    String::from_utf8(root.path.clone()).unwrap(),
                    root.service.code,
                    root.marker,
                )
            })
            .collect();
        assert_eq!(
            roots,
            [
                ("/v/surveillance".into(), "surveillance_recordings", None),
                (
                    "/v/abb".into(),
                    "active_backup_data",
                    Some(&b"@ActiveBackup"[..])
                ),
                ("/v/docs/x.hbk".into(), "hyper_backup", None),
            ]
        );
    }

    #[test]
    fn a_package_folder_holding_its_own_marker_is_named_by_its_name() {
        let r = encode(&[
            dir(0, "/v", NONE, 10, 0),
            marked(1, "task.hbk", 10, "SynologyHyperBackup.bkpi"),
        ]);
        let roots = package_roots(&r);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].service.code, "hyper_backup");
        assert_eq!(roots[0].marker, None);
    }

    #[test]
    fn ranked_parents_note_package_data_counted_in_a_service_row() {
        let r = report(&[
            (0, "/v", NONE, 100, 0),
            (1, "photos", NONE, 50, 0),
            (2, "vault.hbk", NONE, 8, 0),
            (2, "@eaDir", SYSTEM, 2, 2),
        ]);
        let ranked = crate::query::find(&r, b"/v/photos").unwrap();
        let (_, overlaps) = service_summary_with_overlaps(&r, b"/v", &[ranked]).unwrap();
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].target, b"/v/photos");
        assert_eq!(overlaps[0].service.code, "hyper_backup");
        assert_eq!(overlaps[0].bytes, 8 * MIB);
    }

    #[test]
    fn vmm_virtual_disks_are_their_own_line_and_leave_the_iscsi_line_what_is_not_theirs() {
        let r = report(&[
            (0, "/v", NONE, 100, 0),
            (1, "@iSCSI", SYSTEM, 50, 50),
            (2, "LUN", SYSTEM, 50, 50),
            (3, "lun1", SYSTEM, 20, 20),
            (3, "VDISK_a", SYSTEM, 30, 30),
            (4, "disk.img", SYSTEM, 30, 30),
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        assert_eq!(
            lines(&s),
            [
                (
                    "vmm_virtual_disks".into(),
                    Some("/v/@iSCSI/LUN/VDISK_a".into()),
                    1,
                    30
                ),
                ("iscsi_luns".into(), Some("/v/@iSCSI".into()), 1, 20),
            ]
        );
        assert_eq!(s.uses[0].metrics.reclaimable, 30 * MIB);
        assert_eq!(s.uses[1].metrics.reclaimable, 20 * MIB);
    }

    #[test]
    fn snapshots_not_read_count_in_the_line_that_holds_them() {
        let mut inside = dir(4, "snap", SYSTEM, 0, 0);
        inside.flags = NodeFlags::SNAPSHOT;
        let mut beside = dir(3, "lun2", SYSTEM, 0, 0);
        beside.flags = NodeFlags::SNAPSHOT;
        let r = encode(&[
            dir(0, "/v", NONE, 100, 0),
            dir(1, "@iSCSI", SYSTEM, 50, 50),
            dir(2, "LUN", SYSTEM, 50, 50),
            dir(3, "VDISK_a", SYSTEM, 30, 30),
            inside,
            beside,
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        let of = |code: &str| {
            s.uses
                .iter()
                .find(|u| u.service.is_some_and(|x| x.code == code))
                .unwrap()
                .snapshots_not_read
        };
        assert_eq!(of("vmm_virtual_disks"), 1);
        assert_eq!(of("iscsi_luns"), 1);
    }

    #[test]
    fn a_virtual_disk_name_elsewhere_under_iscsi_stays_in_the_iscsi_line() {
        let r = report(&[
            (0, "/v", NONE, 100, 0),
            (1, "@iSCSI", SYSTEM, 50, 50),
            (2, "other", SYSTEM, 50, 50),
            (3, "VDISK_a", SYSTEM, 30, 30),
        ]);
        let s = service_summary(&r, b"/v").unwrap();
        assert_eq!(
            lines(&s),
            [("iscsi_luns".into(), Some("/v/@iSCSI".into()), 1, 50)]
        );
    }
}
