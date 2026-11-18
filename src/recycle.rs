//! Real shared-folder Recycle Bins in a report (SPEC 5.16).

use crate::btrfs::keys::FIRST_FREE_OBJECTID;
use crate::dsm::{self, Depth};
use crate::model::namespace::{FileKind, RecycleEntryCtime};
use crate::query::Entry;
#[cfg(test)]
use crate::report::Report;
use crate::report::{Header, NodeRecord};
use crate::target::Scope;
use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;

/// One real shared-folder Recycle Bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecycleBin {
    /// Share name relative to the volume, e.g. `photos` or `homes/alice`.
    pub share: Vec<u8>,
    pub path: Vec<u8>,
    pub entry: Entry,
}

impl RecycleBin {
    /// Same target classification as the streaming bin walker, including homes.
    pub fn under(&self, mount: &[u8], target: &[u8]) -> bool {
        target == mount
            || share_for_bin(&self.path, mount)
                .is_some_and(|location| location.share_root == target)
    }
}

/// One top-level item, ranked by its inode change time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldestItem {
    pub tree: u64,
    pub objectid: u64,
    pub path: Vec<u8>,
    pub ctime: Option<u64>,
    pub bytes: u64,
    pub reclaimable_bytes: Option<u64>,
}

type OldestKey = (bool, u64, Vec<u8>);
type OldestSelection = BTreeMap<OldestKey, OldestItem>;

struct Candidate {
    share: Vec<u8>,
    path: Vec<u8>,
    share_root: Vec<u8>,
    home: Option<Vec<u8>>,
    entry: Entry,
}

struct ShareLocation {
    share: Vec<u8>,
    share_root: Vec<u8>,
    home: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
struct Location {
    kind: FileKind,
    tree: u64,
    objectid: u64,
}

/// Real Recycle Bins under a volume mount or shared-folder root.
/// `homes/<user>/#recycle` belongs to the `homes` shared folder.
#[cfg(test)]
pub fn bins(report: &Report, under: &[u8]) -> Vec<RecycleBin> {
    let mut walk = Bins::new(&report.header.mount, under);
    report.for_each(|path, record| walk.visit(path, record));
    walk.finish()
}

/// Finds the real Recycle Bins under `under`, like [`bins`], in one
/// walk of records fed in preorder: it keeps each bin and every place
/// a shared folder or user home could be, and matches them at the end.
pub struct Bins<'a> {
    mount: &'a [u8],
    under: &'a [u8],
    candidates: Vec<Candidate>,
    locations: HashMap<Vec<u8>, Location>,
    filesystem: crate::filesystem::FilesystemKind,
}

impl<'a> Bins<'a> {
    pub fn new(mount: &'a [u8], under: &'a [u8]) -> Self {
        Self::for_filesystem(mount, under, crate::filesystem::FilesystemKind::Btrfs)
    }

    pub fn for_filesystem(
        mount: &'a [u8],
        under: &'a [u8],
        filesystem: crate::filesystem::FilesystemKind,
    ) -> Self {
        Bins {
            mount,
            under,
            candidates: Vec::new(),
            locations: HashMap::new(),
            filesystem,
        }
    }

    /// Feeds one record.
    pub fn visit(&mut self, path: &[u8], record: &NodeRecord<'_>) -> ControlFlow<()> {
        if is_share_or_home(path, self.mount) {
            self.locations.insert(path.to_vec(), location(record));
        }
        if record.name != b"#recycle" || record.kind != FileKind::Directory {
            return ControlFlow::Continue(());
        }
        let Some(location) = share_for_bin(path, self.mount) else {
            return ControlFlow::Continue(());
        };
        if self.under != self.mount && location.share_root != self.under {
            return ControlFlow::Continue(());
        }
        self.candidates.push(Candidate {
            share: location.share,
            path: path.to_vec(),
            share_root: location.share_root,
            home: location.home,
            entry: Entry::new(path, record),
        });
        ControlFlow::Continue(())
    }

    /// Largest first.
    pub fn finish(self) -> Vec<RecycleBin> {
        let locations = self.locations;
        let mut bins: Vec<RecycleBin> = self
            .candidates
            .into_iter()
            .filter_map(|candidate| real_bin(candidate, &locations, self.filesystem))
            .collect();
        bins.sort_by_key(|bin| std::cmp::Reverse(bin.entry.metrics.physical));
        bins
    }
}

/// The bin `candidate` is when its shared folder and home are real.
fn real_bin(
    candidate: Candidate,
    locations: &HashMap<Vec<u8>, Location>,
    filesystem: crate::filesystem::FilesystemKind,
) -> Option<RecycleBin> {
    let share = locations.get(&candidate.share_root)?;
    if share.kind != FileKind::Directory
        || (filesystem == crate::filesystem::FilesystemKind::Btrfs
            && share.objectid != FIRST_FREE_OBJECTID)
        || (filesystem == crate::filesystem::FilesystemKind::Ext4
            && !candidate
                .entry
                .flags
                .contains(crate::model::tree::NodeFlags::RECYCLE))
        || share.tree != candidate.entry.tree
        || (filesystem == crate::filesystem::FilesystemKind::Btrfs
            && candidate.entry.objectid == FIRST_FREE_OBJECTID)
    {
        return None;
    }
    if let Some(home_path) = &candidate.home {
        let home = locations.get(home_path)?;
        if home.kind != FileKind::Directory
            || (filesystem == crate::filesystem::FilesystemKind::Btrfs
                && home.objectid == FIRST_FREE_OBJECTID)
            || home.tree != share.tree
        {
            return None;
        }
    }
    Some(RecycleBin {
        share: candidate.share,
        path: candidate.path,
        entry: candidate.entry,
    })
}

/// Whether `path` is where a shared folder (`<mount>/<name>`) or a
/// user home (`<mount>/homes/<user>`) could be.
fn is_share_or_home(path: &[u8], mount: &[u8]) -> bool {
    let Some(relative) = path.strip_prefix(mount).and_then(|p| p.strip_prefix(b"/")) else {
        return false;
    };
    match relative.iter().position(|&byte| byte == b'/') {
        None => !relative.is_empty(),
        Some(at) => &relative[..at] == b"homes" && !relative[at + 1..].contains(&b'/'),
    }
}

/// A report a volume's Recycle Bin answer takes some shares from: the
/// newest report of each share scanned on its own after the volume report.
#[derive(Debug, PartialEq, Eq)]
pub struct ShareSupplement<'a, P> {
    pub report: &'a P,
    pub header: &'a Header,
    /// The share roots this report answers for; a newer report of the same
    /// share answers for it instead.
    pub roots: Vec<&'a [u8]>,
}

/// Which share reports supplement the volume report `volume` (SPEC 5.16):
/// each path is answered by the newest report covering it, so a share
/// scanned on its own after the volume scan lists its Recycle Bin from its
/// own report. `reports` is newest first, as the report store lists them.
/// Only shares directly on the volume count; snapshot-deletion answers
/// and reports of another mount never do.
pub fn newer_share_reports<'a, P>(
    reports: &'a [(P, Header)],
    volume: &Header,
) -> Vec<ShareSupplement<'a, P>> {
    let mut taken: Vec<&[u8]> = Vec::new();
    let mut supplements = Vec::new();
    for (report, header) in reports {
        if header.scan_finished <= volume.scan_finished
            || header.mount != volume.mount
            || header.fsid != volume.fsid
            || header.deletion.is_some()
            || header.roots.is_empty()
            || !header
                .roots
                .iter()
                .all(|root| root.scope == Scope::Path && is_share_root(&root.path, &volume.mount))
        {
            continue;
        }
        let roots: Vec<&[u8]> = header
            .roots
            .iter()
            .map(|root| root.path.as_slice())
            .filter(|root| !taken.contains(root))
            .collect();
        if roots.is_empty() {
            continue;
        }
        taken.extend(roots.iter().copied());
        supplements.push(ShareSupplement {
            report,
            header,
            roots,
        });
    }
    supplements
}

/// Whether `path` is a shared folder directly on the volume `mount`.
fn is_share_root(path: &[u8], mount: &[u8]) -> bool {
    path.strip_prefix(mount)
        .and_then(|p| p.strip_prefix(b"/"))
        .is_some_and(|name| !name.contains(&b'/') && valid_share_name(name))
}

/// Collects the oldest direct children of several bins in one streamed
/// report pass. Missing timestamps sort after measured timestamps.
pub struct OldestItems<'a> {
    ctimes: &'a [RecycleEntryCtime],
    limit: usize,
    selected: HashMap<Vec<u8>, OldestSelection>,
}

impl<'a> OldestItems<'a> {
    pub fn new(bins: &[RecycleBin], ctimes: &'a [RecycleEntryCtime], limit: usize) -> Self {
        OldestItems {
            ctimes,
            limit,
            selected: bins
                .iter()
                .map(|bin| (bin.path.clone(), BTreeMap::new()))
                .collect(),
        }
    }

    pub fn visit(&mut self, path: &[u8], record: &NodeRecord<'_>) -> ControlFlow<()> {
        if self.limit == 0 {
            return ControlFlow::Continue(());
        }
        let Some(separator) = path.iter().rposition(|&byte| byte == b'/') else {
            return ControlFlow::Continue(());
        };
        let parent = &path[..separator];
        let Some(selected) = self.selected.get_mut(parent) else {
            return ControlFlow::Continue(());
        };
        let name = &path[separator + 1..];
        if name.is_empty() {
            return ControlFlow::Continue(());
        }
        let ctime = ctime_for(self.ctimes, record.tree, record.objectid);
        let item = OldestItem {
            tree: record.tree,
            objectid: record.objectid,
            path: path.to_vec(),
            ctime,
            bytes: record.metrics.physical,
            reclaimable_bytes: (record.metrics.unknown == 0).then_some(record.metrics.reclaimable),
        };
        let key = (ctime.is_none(), ctime.unwrap_or(u64::MAX), name.to_vec());
        // Only a full list can turn an item away, and only one no older than its newest.
        if selected.len() >= self.limit
            && selected
                .last_key_value()
                .is_some_and(|(last, _)| key.cmp(last).is_ge())
        {
            return ControlFlow::Continue(());
        }
        selected.insert(key, item);
        if selected.len() > self.limit {
            selected.pop_last();
        }
        ControlFlow::Continue(())
    }

    pub fn finish(self) -> HashMap<Vec<u8>, Vec<OldestItem>> {
        self.selected
            .into_iter()
            .map(|(path, items)| (path, items.into_values().collect()))
            .collect()
    }
}

#[cfg(test)]
pub fn oldest(report: &Report, bin: &RecycleBin, limit: usize) -> Vec<OldestItem> {
    let mut items = OldestItems::new(
        std::slice::from_ref(bin),
        &report.header.recycle_ctimes,
        limit,
    );
    report.for_each(|path, record| items.visit(path, record));
    items.finish().remove(&bin.path).unwrap_or_default()
}

fn ctime_for(entries: &[RecycleEntryCtime], tree: u64, inode: u64) -> Option<u64> {
    entries
        .binary_search_by_key(&(tree, inode), |entry| (entry.tree, entry.inode))
        .ok()
        .map(|index| entries[index].ctime)
}

fn location(record: &NodeRecord<'_>) -> Location {
    Location {
        kind: record.kind,
        tree: record.tree,
        objectid: record.objectid,
    }
}

fn share_for_bin(path: &[u8], mount: &[u8]) -> Option<ShareLocation> {
    let relative = path.strip_prefix(mount)?.strip_prefix(b"/")?;
    let parts: Vec<&[u8]> = relative.split(|&byte| byte == b'/').collect();
    match parts.as_slice() {
        [name, b"#recycle"] if valid_share_name(name) => {
            if dsm::service(name, Depth::VolumeRoot).is_some() {
                return None;
            }
            Some(ShareLocation {
                share: name.to_vec(),
                share_root: join(mount, name),
                home: None,
            })
        }
        [b"homes", b"#recycle"] => {
            let root = join(mount, b"homes");
            Some(ShareLocation {
                share: b"homes".to_vec(),
                share_root: root,
                home: None,
            })
        }
        [b"homes", user, b"#recycle"] if valid_share_name(user) => {
            let root = join(mount, b"homes");
            let mut share = b"homes/".to_vec();
            share.extend_from_slice(user);
            let home = join(&root, user);
            Some(ShareLocation {
                share,
                share_root: root,
                home: Some(home),
            })
        }
        _ => None,
    }
}

fn valid_share_name(name: &[u8]) -> bool {
    !name.is_empty() && !name.starts_with(b"@") && !name.starts_with(b"#")
}

fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = parent.to_vec();
    if !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

#[cfg(test)]
mod tests {
    use crate::model::accounting::{Blockers, Held, HeldClass};
    use crate::model::namespace::FileKind;
    use crate::model::tree::{Metrics, NodeFlags};
    use crate::report::{
        NodeRecord, Report, ReportReader, RootInfo, encode_record, testing, write_records,
    };
    use crate::target::Scope;

    const MIB: u64 = 1 << 20;

    fn record(
        depth: u32,
        name: &'static [u8],
        kind: FileKind,
        tree: u64,
        objectid: u64,
        metrics: Metrics,
        held: Held,
    ) -> NodeRecord<'static> {
        NodeRecord {
            depth,
            kind,
            flags: NodeFlags::from_bits(0),
            name,
            metrics,
            blockers: Blockers::default(),
            held,
            marker: None,
            outside: None,
            nlink: 1,
            tree,
            objectid,
        }
    }

    fn dir(depth: u32, name: &'static [u8], tree: u64, objectid: u64) -> NodeRecord<'static> {
        record(
            depth,
            name,
            FileKind::Directory,
            tree,
            objectid,
            Metrics {
                entries: 1,
                ..Metrics::default()
            },
            Held::default(),
        )
    }

    fn bin(
        tree: u64,
        objectid: u64,
        physical: u64,
        reclaimable: u64,
        held: Held,
    ) -> NodeRecord<'static> {
        record(
            2,
            b"#recycle",
            FileKind::Directory,
            tree,
            objectid,
            Metrics {
                physical,
                reclaimable,
                entries: 3,
                ..Metrics::default()
            },
            held,
        )
    }

    fn report() -> Report {
        Report::parse(report_bytes()).unwrap()
    }

    fn report_bytes() -> Vec<u8> {
        let records = [
            dir(0, b"/v", 256, 256),
            dir(1, b"photos", 257, 256),
            bin(
                257,
                300,
                8 * MIB,
                4 * MIB,
                Held::of(HeldClass::HardLink, 4 * MIB),
            ),
            record(
                3,
                b"old.bin",
                FileKind::Regular,
                257,
                301,
                Metrics {
                    entries: 1,
                    files: 1,
                    physical: 10,
                    reclaimable: 8,
                    ..Metrics::default()
                },
                Held::default(),
            ),
            record(
                3,
                b"new.bin",
                FileKind::Regular,
                257,
                302,
                Metrics {
                    entries: 1,
                    files: 1,
                    physical: 20,
                    reclaimable: 19,
                    ..Metrics::default()
                },
                Held::default(),
            ),
            dir(1, b"homes", 258, 256),
            dir(2, b"alice", 258, 400),
            record(
                3,
                b"#recycle",
                FileKind::Directory,
                258,
                401,
                Metrics {
                    physical: 2 * MIB,
                    reclaimable: MIB,
                    entries: 2,
                    ..Metrics::default()
                },
                Held::of(HeldClass::Snapshot, MIB),
            ),
            record(
                4,
                b"note.txt",
                FileKind::Regular,
                258,
                402,
                Metrics {
                    entries: 1,
                    files: 1,
                    ..Metrics::default()
                },
                Held::default(),
            ),
            dir(1, b"plain", 256, 500),
            dir(2, b"nested", 256, 501),
            record(
                3,
                b"#recycle",
                FileKind::Directory,
                256,
                502,
                Metrics {
                    entries: 1,
                    ..Metrics::default()
                },
                Held::default(),
            ),
        ];
        let mut header = testing::header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: Scope::Volume,
            tree: 256,
            ino: 256,
        }]);
        header.recycle_ctimes = vec![
            crate::model::namespace::RecycleEntryCtime {
                tree: 256,
                inode: 301,
                ctime: 1,
            },
            crate::model::namespace::RecycleEntryCtime {
                tree: 257,
                inode: 301,
                ctime: 20,
            },
            crate::model::namespace::RecycleEntryCtime {
                tree: 257,
                inode: 302,
                ctime: 30,
            },
        ];
        let mut bytes = Vec::new();
        write_records(&mut bytes, &header, records.len() as u64, |writer| {
            records
                .iter()
                .try_for_each(|record| encode_record(writer, record))
        })
        .unwrap();
        bytes
    }

    #[test]
    fn ext4_bins_require_the_scanners_trusted_share_flag_not_inode_256() {
        let share = dir(1, b"photos", 88, 1234);
        let mut bin = dir(2, b"#recycle", 88, 1235);
        for trusted in [false, true] {
            bin.flags = if trusted {
                NodeFlags::RECYCLE
            } else {
                NodeFlags::default()
            };
            let mut walk =
                super::Bins::for_filesystem(b"/v", b"/v", crate::filesystem::FilesystemKind::Ext4);
            let _ = walk.visit(b"/v/photos", &share);
            let _ = walk.visit(b"/v/photos/#recycle", &bin);
            assert_eq!(walk.finish().len(), usize::from(trusted));
        }
    }

    #[test]
    fn cached_bins_match_streaming_target_selection_including_homes() {
        let report = report();
        let all = super::bins(&report, b"/v");
        for target in [
            b"/v".as_slice(),
            b"/v/photos",
            b"/v/homes",
            b"/v/homes/alice",
            b"/v/absent",
        ] {
            let cached: Vec<_> = all
                .iter()
                .filter(|b| b.under(b"/v", target))
                .cloned()
                .collect();
            assert_eq!(cached, super::bins(&report, target));
        }
    }

    #[test]
    fn only_real_share_and_user_home_bins_are_reported() {
        let report = report();
        let bins = super::bins(&report, b"/v");
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].share, b"photos");
        assert_eq!(bins[0].entry.metrics.physical, 8 * MIB);
        assert_eq!(bins[0].entry.metrics.entries.saturating_sub(1), 2);
        assert_eq!(bins[1].share, b"homes/alice");
        assert_eq!(bins[1].entry.metrics.physical, 2 * MIB);
    }

    #[test]
    fn oldest_lists_top_level_items_by_tree_and_inode_ctime() {
        let report = report();
        let bins = super::bins(&report, b"/v");

        let oldest = super::oldest(&report, &bins[0], 1);

        assert_eq!(oldest.len(), 1);
        assert_eq!(oldest[0].path, b"/v/photos/#recycle/old.bin");
        assert_eq!(oldest[0].ctime, Some(20));
        assert_eq!((oldest[0].tree, oldest[0].objectid), (257, 301));
        assert_eq!(
            (oldest[0].bytes, oldest[0].reclaimable_bytes),
            (10, Some(8))
        );
    }

    fn scanned(finished: u64, roots: &[&[u8]]) -> crate::report::Header {
        let mut header = testing::header(
            roots
                .iter()
                .map(|path| RootInfo {
                    path: path.to_vec(),
                    scope: if *path == b"/v" {
                        Scope::Volume
                    } else {
                        Scope::Path
                    },
                    tree: 257,
                    ino: 256,
                })
                .collect(),
        );
        header.scan_finished = finished;
        header
    }

    /// B22: a share scanned on its own after the volume scan answers for
    /// itself in the volume's Recycle Bin list; the newest report wins.
    #[test]
    fn newer_share_reports_answer_for_their_shares_newest_first() {
        let volume = scanned(100, &[b"/v"]);
        let reports = vec![
            ("both-newest", scanned(300, &[b"/v/photos", b"/v/docs"])),
            ("photos-older", scanned(200, &[b"/v/photos"])),
            ("music", scanned(150, &[b"/v/music"])),
            ("before-volume", scanned(90, &[b"/v/video"])),
            ("volume", volume.clone()),
        ];

        let supplements = super::newer_share_reports(&reports, &volume);

        let chosen: Vec<_> = supplements
            .iter()
            .map(|s| (*s.report, s.roots.clone()))
            .collect();
        assert_eq!(
            chosen,
            [
                ("both-newest", vec![b"/v/photos".as_slice(), b"/v/docs"]),
                ("music", vec![b"/v/music".as_slice()]),
            ]
        );
    }

    #[test]
    fn only_share_roots_of_the_same_volume_supplement_it() {
        let volume = scanned(100, &[b"/v"]);
        let mut deletion = scanned(200, &[b"/v/photos"]);
        deletion.deletion = Some(Default::default());
        let mut other_mount = scanned(200, &[b"/w/photos"]);
        other_mount.mount = b"/w".to_vec();
        let reports = vec![
            ("folder", scanned(200, &[b"/v/photos/2024"])),
            ("service", scanned(200, &[b"/v/@docker"])),
            ("mixed", scanned(200, &[b"/v/photos", b"/v/photos/2024"])),
            ("deletion", deletion),
            ("other-mount", other_mount),
            ("newer-volume", scanned(200, &[b"/v"])),
        ];

        assert!(super::newer_share_reports(&reports, &volume).is_empty());
    }

    /// B07: a bin listing an older item before a newer one returned only the
    /// older one, although the limit had room for both.
    #[test]
    fn oldest_keeps_newer_items_while_the_limit_has_room() {
        let report = report();
        let bins = super::bins(&report, b"/v");

        let oldest = super::oldest(&report, &bins[0], 200);

        let paths: Vec<_> = oldest.iter().map(|item| item.path.as_slice()).collect();
        assert_eq!(
            paths,
            [
                b"/v/photos/#recycle/old.bin".as_slice(),
                b"/v/photos/#recycle/new.bin"
            ]
        );
        assert_eq!(oldest[1].ctime, Some(30));
    }

    #[test]
    fn a_share_target_includes_its_user_home_bins_but_not_other_shares() {
        let report = report();
        let bins = super::bins(&report, b"/v/homes");
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0].share, b"homes/alice");
    }

    #[test]
    fn a_recycle_bin_in_a_nested_plain_folder_is_not_real() {
        let report = report();
        assert!(super::bins(&report, b"/v/plain").is_empty());
    }

    #[test]
    fn one_streamed_walk_finds_the_bins_of_every_target() {
        let report = report();
        let bytes = report_bytes();
        for under in [&b"/v"[..], b"/v/homes", b"/v/photos", b"/v/plain"] {
            let mut walk = super::Bins::new(&report.header.mount, under);
            let reader = ReportReader::open(bytes.as_slice()).unwrap();
            reader
                .for_each(|path, record| walk.visit(path, record))
                .unwrap();
            assert_eq!(walk.finish(), super::bins(&report, under));
        }
    }
}
