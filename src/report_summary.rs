//! Disposable display answers bound to one immutable report. Cleanup never reads these.
use crate::codec;
use crate::commands::{CleanupCandidate, CleanupCandidates};
use crate::report::{self, Header, ReportError, ReportReader};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const FORMAT: u32 = 5;
pub mod index;
pub mod pages;
mod top_codec;
const MAX_BYTES: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub report_format: u32,
    pub checksum: u64,
    pub size: u64,
    pub device: u64,
    pub inode: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
}

impl Binding {
    fn of(file: &mut File) -> io::Result<Self> {
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() < 16 {
            return Err(io::Error::other("not a report"));
        }
        file.seek(SeekFrom::End(-16))?;
        let mut trailer = [0u8; 16];
        file.read_exact(&mut trailer)?;
        if &trailer[..8] != b"SRREND1\n" {
            return Err(io::Error::other("invalid report trailer"));
        }
        Ok(Self {
            report_format: report::FORMAT,
            checksum: u64::from_le_bytes(trailer[8..].try_into().expect("8 bytes")),
            size: meta.len(),
            device: meta.dev(),
            inode: meta.ino(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
        })
    }

    fn matches(&self, path: &Path) -> bool {
        open(path)
            .and_then(|mut file| Self::of(&mut file))
            .is_ok_and(|now| now == *self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub format: u32,
    pub binding: Binding,
    pub header: Header,
    pub cleanup: Vec<CleanupCandidate>,
    pub snapshots: std::collections::BTreeMap<u64, pages::SnapshotFigures>,
    pub bins: Vec<pages::Bin>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    checksum: u64,
    summary: Summary,
}

pub fn path(report: &Path) -> PathBuf {
    report.with_extension("srsummary")
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// A valid hit reads only the small summary and the report trailer/stat identity.
pub fn load(report: &Path) -> Option<Summary> {
    let file = open(&path(report)).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != crate::sys::effective_uid()
        || meta.mode() & 0o077 != 0
        || meta.len() > MAX_BYTES
    {
        return None;
    }
    let saved: Saved =
        serde_json::from_reader(io::BufReader::new(file.take(MAX_BYTES + 1))).ok()?;
    let summary = saved.summary;
    let encoded = serde_json::to_vec(&summary).ok()?;
    (saved.checksum == codec::checksum(&encoded)
        && summary.format == FORMAT
        && summary.header.format == report::FORMAT
        && summary.binding.report_format == report::FORMAT
        && summary.binding.matches(report)
        && summary.bins.iter().all(|bin| bin.get().is_some())
        && summary.header.snapshots.as_ref().is_none_or(|a| {
            a.shares.iter().all(|s| {
                summary.snapshots.get(&s.live).is_some_and(|f| {
                    f.snapshots
                        .iter()
                        .map(|x| x.subvolume_id)
                        .eq(s.snapshots.iter().map(|x| x.id))
                })
            })
        }))
    .then_some(summary)
}

/// Old reports are streamed once; persisting disposable derived state is best effort.
pub fn ensure(report: &Path) -> Result<Summary, ReportError> {
    if let Some(summary) = load(report) {
        return Ok(summary);
    }
    let summary = build(report)?;
    let _ = save(report, &summary);
    Ok(summary)
}

pub fn build(path: &Path) -> Result<Summary, ReportError> {
    let mut file = open(path)?;
    let binding = Binding::of(&mut file)?;
    file.rewind()?;
    let reader = ReportReader::open(&mut file)?;
    let mut header = reader.header().clone();
    let data = header.overview.clone().unwrap_or_default();
    let snapshots = header
        .snapshots
        .as_ref()
        .map(|analysis| {
            analysis
                .shares
                .iter()
                .map(|share| {
                    (
                        share.live,
                        crate::snapshots::answer::snapshot_figures(analysis, share),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut cleanup = CleanupCandidates::new(&header.mount, &data);
    let mut bins =
        crate::recycle::Bins::for_filesystem(&header.mount, &header.mount, header.filesystem);
    reader.for_each(|path, record| {
        let _ = cleanup.visit(path, record);
        bins.visit(path, record)
    })?;
    let bins = bins
        .finish()
        .iter()
        .map(pages::Bin::new)
        .collect::<io::Result<Vec<_>>>()?;
    let mut candidates = cleanup.finish();
    // Only five actions are displayed; stable ties preserve report preorder.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.2));
    candidates.truncate(5);
    if Binding::of(&mut file)? != binding || !binding.matches(path) {
        return Err(io::Error::other("report replaced while building summary").into());
    }
    header.recycle_ctimes.clear();
    header.deletion = None;
    if let Some(snapshots) = &mut header.snapshots {
        snapshots.holdings.clear();
        snapshots.quota = None;
    }
    Ok(Summary {
        format: FORMAT,
        binding,
        header,
        cleanup: candidates,
        snapshots,
        bins,
    })
}

pub fn save(report: &Path, summary: &Summary) -> io::Result<()> {
    if !summary.binding.matches(report) {
        return Err(io::Error::other("summary no longer matches report"));
    }
    let checksum = codec::checksum(&serde_json::to_vec(summary)?);
    atomic_write(&path(report), |file| {
        let mut out = io::BufWriter::new(file);
        serde_json::to_writer(
            &mut out,
            &Saved {
                checksum,
                summary: summary.clone(),
            },
        )?;
        out.flush()
    })
}

/// Unique temporary names let first readers build concurrently without interference.
fn atomic_write(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("summary has no parent"))?;
    crate::state::private_dir(dir, crate::sys::effective_uid()).map_err(io::Error::other)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("summary has no name"))?;
    let tmp = dir.join(format!(
        ".{}.{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        write(&mut file)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::testing;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (crate::state::Store, PathBuf) {
        let root = crate::spill::testing::scratch("summary").join("state");
        let store = crate::state::Store::open(&root, crate::sys::effective_uid()).unwrap();
        let header = testing::header(Vec::new());
        let tree = crate::model::tree::ScopeTree::new();
        let (path, _) = store
            .publish(&header, |out| testing::write_footprint(out, &header, &tree))
            .unwrap();
        (store, path)
    }

    #[test]
    fn old_report_builds_once_and_reuses_its_private_summary() {
        let (_, report) = fixture();
        let _ = fs::remove_file(path(&report));
        assert!(load(&report).is_none());
        let expected = ensure(&report).unwrap();
        assert_eq!(load(&report), Some(expected.clone()));
        assert_eq!(
            fs::metadata(path(&report)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let inode = fs::metadata(path(&report)).unwrap().ino();
        assert_eq!(ensure(&report).unwrap(), expected);
        assert_eq!(fs::metadata(path(&report)).unwrap().ino(), inode);
    }

    #[test]
    fn corrupt_stale_and_unsupported_summaries_rebuild_from_the_report() {
        let (_, report) = fixture();
        let expected = ensure(&report).unwrap();
        fs::write(path(&report), b"broken").unwrap();
        assert_eq!(ensure(&report).unwrap(), expected);
        let mut wrong = expected.clone();
        wrong.format += 1;
        save(&report, &wrong).unwrap();
        assert!(load(&report).is_none());
        assert_eq!(ensure(&report).unwrap(), expected);
        wrong = expected.clone();
        wrong.binding.inode += 1;
        let encoded = serde_json::to_vec(&wrong).unwrap();
        fs::write(
            path(&report),
            serde_json::to_vec(&Saved {
                checksum: codec::checksum(&encoded),
                summary: wrong,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(load(&report).is_none());
        assert_eq!(ensure(&report).unwrap(), expected);
    }

    #[test]
    fn a_hit_does_not_checksum_the_report_body_but_the_original_parser_still_does() {
        let (_, report) = fixture();
        let expected = ensure(&report).unwrap();
        let meta = fs::metadata(&report).unwrap();
        let mut file = OpenOptions::new().write(true).open(&report).unwrap();
        file.seek(SeekFrom::Start(20)).unwrap();
        file.write_all(b"X").unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(meta.modified().unwrap()))
            .unwrap();
        assert_eq!(load(&report), Some(expected));
        assert!(crate::report::Report::parse(fs::read(&report).unwrap()).is_err());
    }

    #[test]
    fn report_publication_writes_a_summary_bound_to_the_new_inode() {
        let (store, report) = fixture();
        let before = load(&report).expect("summary at publication");
        let mut header = before.header.clone();
        header.scan_finished += 1;
        let tree = crate::model::tree::ScopeTree::new();
        store
            .publish(&header, |out| testing::write_footprint(out, &header, &tree))
            .unwrap();
        let after = load(&report).unwrap();
        assert_ne!(after.binding.inode, before.binding.inode);
        assert_eq!(after.header.scan_finished, header.scan_finished);
    }
    #[test]
    fn summary_precomputes_snapshot_figures_and_real_recycle_bin_rows() {
        use crate::model::accounting::{Blockers, Held};
        use crate::model::namespace::FileKind;
        use crate::model::tree::{Metrics, NodeFlags};
        use crate::report::{NodeRecord, RootInfo, encode_record, write_records};
        use crate::snapshots::{
            AnalysisStats, Depth, Hold, Holding, ShareAnalysis, SnapshotAnalysis, SnapshotInfo,
        };
        let (store, _) = fixture();
        let mut header = testing::header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: crate::target::Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        header.snapshots = Some(SnapshotAnalysis {
            depth: Depth::All,
            owner_budget: 64,
            global_owner_budget: 100,
            shares: vec![ShareAnalysis {
                live: 257,
                name: b"a".to_vec(),
                subvolume_path: b"a".to_vec(),
                snapshots: vec![SnapshotInfo {
                    id: 258,
                    name: b"s1".to_vec(),
                    subvolume_path: b"@sharesnap/a/s1".to_vec(),
                    otime: Some(1),
                    generation: 3,
                    materialized: true,
                }],
                pending: vec![],
                held_files: None,
            }],
            holdings: vec![Holding {
                owners: vec![258],
                hold: Hold::Resolved,
                bytes: 8192,
                extents: 1,
                pinned_unreferenced_bytes: Some(1024),
            }],
            quota: None,
            stats: AnalysisStats::default(),
        });
        let make = |depth, name, tree, objectid| NodeRecord {
            depth,
            name,
            tree,
            objectid,
            kind: FileKind::Directory,
            flags: NodeFlags::from_bits(0),
            metrics: Metrics {
                physical: 8192,
                reclaimable: 8192,
                ..Metrics::default()
            },
            blockers: Blockers::default(),
            held: Held::default(),
            marker: None,
            outside: None,
            nlink: 1,
        };
        let (report, _) = store
            .publish(&header, |out| {
                write_records(out, &header, 3, |w| {
                    encode_record(w, &make(0, b"/v", 5, 256))?;
                    encode_record(w, &make(1, b"a", 257, 256))?;
                    encode_record(w, &make(2, b"#recycle", 257, 300))
                })
            })
            .unwrap();
        let summary = ensure(&report).unwrap();
        assert_eq!(summary.snapshots[&257].held_bytes, 8192);
        assert_eq!(
            summary.snapshots[&257].snapshots[0].exclusive_bytes,
            Some(8192)
        );
        assert_eq!(
            summary.snapshots[&257].snapshots[0].pinned_unreferenced_bytes,
            Some(1024)
        );
        assert!(summary.header.snapshots.unwrap().holdings.is_empty());
        let bins: Vec<_> = summary.bins.iter().map(|b| b.get().unwrap()).collect();
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0].path, b"/v/a/#recycle");
        assert_eq!(bins[0].entry.metrics.reclaimable, 8192);
    }

    #[test]
    #[ignore = "large synthetic performance evidence; run on a Linux build host"]
    fn benchmark_five_million_nodes() {
        use crate::model::accounting::{Blockers, Held};
        use crate::model::namespace::FileKind;
        use crate::model::tree::{Metrics, NodeFlags};
        use crate::report::{NodeRecord, RootInfo, encode_record, write_records};
        use std::time::Instant;
        let root = std::env::var_os("SYNORECLAIM_BENCH_DIR")
            .map(PathBuf::from)
            .expect("SYNORECLAIM_BENCH_DIR is set to the thread build directory");
        crate::state::private_dir(&root, crate::sys::effective_uid()).unwrap();
        let report = root.join("large.srrep");
        let mut header = testing::header(vec![RootInfo {
            path: b"/v".to_vec(),
            scope: crate::target::Scope::Volume,
            tree: 5,
            ino: 256,
        }]);
        header.overview = Some(crate::overview::OverviewData::default());
        use crate::snapshots::{
            AnalysisStats, Depth, Hold, Holding, ShareAnalysis, SnapshotAnalysis, SnapshotInfo,
        };
        header.snapshots = Some(SnapshotAnalysis {
            depth: Depth::All,
            owner_budget: 64,
            global_owner_budget: 100,
            shares: vec![ShareAnalysis {
                live: 5,
                name: b"a".to_vec(),
                subvolume_path: b"a".to_vec(),
                pending: vec![],
                held_files: None,
                snapshots: (0..64)
                    .map(|i| SnapshotInfo {
                        id: 258 + i,
                        name: format!("snapshot-{i}").into_bytes(),
                        subvolume_path: vec![],
                        otime: Some(i),
                        generation: 1,
                        materialized: i < 63,
                    })
                    .collect(),
            }],
            holdings: (0..5000)
                .map(|i| Holding {
                    owners: vec![258 + i % 63],
                    hold: if i % 13 == 0 {
                        Hold::Unknown
                    } else {
                        Hold::Resolved
                    },
                    bytes: 4096,
                    extents: 1,
                    pinned_unreferenced_bytes: Some(0),
                })
                .collect(),
            quota: None,
            stats: AnalysisStats::default(),
        });
        fn make(depth: u32, name: &[u8], kind: FileKind) -> NodeRecord<'_> {
            NodeRecord {
                depth,
                name,
                kind,
                flags: NodeFlags::from_bits(0),
                metrics: Metrics {
                    logical: 4096,
                    physical: 4096,
                    reclaimable: 4096,
                    entries: 1,
                    files: u64::from(kind == FileKind::Regular),
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
        let start = Instant::now();
        let out = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&report)
            .unwrap();
        let mut out = io::BufWriter::new(out);
        write_records(&mut out, &header, 5_002_005, |w| {
            encode_record(w, &make(0, b"/v", FileKind::Directory))?;
            encode_record(w, &make(1, b"a", FileKind::Directory))?;
            for d in 0..2000 {
                let name = format!("d{d:04}");
                encode_record(w, &make(2, name.as_bytes(), FileKind::Directory))?;
                for f in 0..2500 {
                    let name = format!("file{f:06}");
                    encode_record(w, &make(3, name.as_bytes(), FileKind::Regular))?;
                }
            }
            let mut bin = make(2, b"#recycle", FileKind::Directory);
            bin.objectid = 300;
            encode_record(w, &bin)?;
            let mut drive = make(1, b"@synologydrive", FileKind::Directory);
            drive.flags = NodeFlags::PACKAGE.with(NodeFlags::SYSTEM);
            drive.metrics.unknown = 1024;
            drive.held = Held([0, 0, 2048, 0, 0, 0]);
            encode_record(w, &drive)?;
            encode_record(w, &make(2, b"data", FileKind::Regular))?;
            Ok(())
        })
        .unwrap();
        out.flush().unwrap();
        drop(out);
        println!(
            "synthetic nodes=5002005 bytes={} write_ms={}",
            fs::metadata(&report).unwrap().len(),
            start.elapsed().as_millis()
        );
        let mut baseline_overview = Vec::new();
        let mut baseline_why = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let reader = ReportReader::open(File::open(&report).unwrap()).unwrap();
            let mut cleanup =
                CleanupCandidates::new(&header.mount, header.overview.as_ref().unwrap());
            reader.for_each(|p, r| cleanup.visit(p, r)).unwrap();
            std::hint::black_box(cleanup.finish());
            baseline_overview.push(start.elapsed().as_secs_f64() * 1000.);
            let start = Instant::now();
            std::hint::black_box(
                ReportReader::open(File::open(&report).unwrap())
                    .unwrap()
                    .checked_header()
                    .unwrap(),
            );
            baseline_why.push(start.elapsed().as_secs_f64() * 1000.);
        }
        let mut before_snapshots = Vec::new();
        let mut before_recycle = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let h = report::read_header(io::BufReader::new(File::open(&report).unwrap())).unwrap();
            let analysis = h.snapshots.as_ref().unwrap();
            std::hint::black_box(crate::snapshots::answer::share_answer(
                analysis,
                &analysis.shares[0],
                None,
                std::time::UNIX_EPOCH,
                &crate::snapshots::answer::Listing {
                    mount: &h.mount,
                    limit: 100,
                },
                &crate::snapshots::answer::Rescan {
                    mount: &h.mount,
                    state_dir: &root,
                },
            ));
            before_snapshots.push(start.elapsed().as_secs_f64() * 1000.);
            let start = Instant::now();
            let reader = ReportReader::open(File::open(&report).unwrap()).unwrap();
            let mut bins = crate::recycle::Bins::new(&h.mount, &h.mount);
            reader.for_each(|p, r| bins.visit(p, r)).unwrap();
            assert_eq!(bins.finish().len(), 1);
            before_recycle.push(start.elapsed().as_secs_f64() * 1000.);
        }
        let start = Instant::now();
        let summary = build(&report).unwrap();
        save(&report, &summary).unwrap();
        println!(
            "summary build_ms={} bytes={}",
            start.elapsed().as_millis(),
            fs::metadata(path(&report)).unwrap().len()
        );
        let mut hits = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            std::hint::black_box(ensure(&report).unwrap());
            hits.push(start.elapsed().as_secs_f64() * 1000.);
        }
        println!(
            "baseline overview_ms={baseline_overview:?} why_ms={baseline_why:?} snapshots_ms={before_snapshots:?} recycle_ms={before_recycle:?}; summary hit_ms={hits:?}"
        );
        use crate::query::{KindFilter, Measure, TopQuery, TopWalk};
        let targets: [&[u8]; 3] = [b"/v", b"/v/a", b"/v/a/d1000"];
        let mut before_top = vec![Vec::new(); 3];
        let mut expected = Vec::new();
        for (n, under) in targets.iter().enumerate() {
            let q = TopQuery {
                under,
                services: true,
                by: Measure::Reclaimable,
                kind: KindFilter::All,
                include_system: false,
                only_shared: false,
                limit: 200,
            };
            for run in 0..3 {
                let start = Instant::now();
                let reader = ReportReader::open(File::open(&report).unwrap()).unwrap();
                let mut walk = TopWalk::new(&header.mount, &q);
                reader.for_each(|p, r| walk.visit(p, r)).unwrap();
                let answer = walk.finish();
                before_top[n].push(start.elapsed().as_secs_f64() * 1000.);
                if run == 0 {
                    expected.push(answer);
                }
            }
        }
        let _ = fs::remove_file(index::path(&report));
        let start = Instant::now();
        index::ensure(&report).unwrap();
        println!(
            "index build_ms={} bytes={}",
            start.elapsed().as_millis(),
            fs::metadata(index::path(&report)).unwrap().len()
        );
        let mut after_top = vec![Vec::new(); 3];
        for (n, under) in targets.iter().enumerate() {
            let q = TopQuery {
                under,
                services: true,
                by: Measure::Reclaimable,
                kind: KindFilter::All,
                include_system: false,
                only_shared: false,
                limit: 200,
            };
            for _ in 0..3 {
                let start = Instant::now();
                let answer = index::top(&report, &header.mount, &q).unwrap();
                after_top[n].push(start.elapsed().as_secs_f64() * 1000.);
                assert_eq!(answer, expected[n]);
            }
        }
        assert!(!expected[0].as_ref().unwrap().services.uses.is_empty());
        println!("top root/share/deep before_ms={before_top:?} after_ms={after_top:?}");
    }
}
