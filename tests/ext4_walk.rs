#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::fs;
use std::ops::ControlFlow;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, symlink};
use syno_reclaim::btrfs::walk::{BatchBoundary, StopReason};
use syno_reclaim::ext4::walk;

struct Boundary;
impl BatchBoundary for Boundary {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        ControlFlow::Continue(())
    }
}

#[test]
fn a_file_changed_after_its_visit_is_not_silently_reported_stable() {
    use syno_reclaim::ext4::spilling::{Input, scan};
    use syno_reclaim::spill::Spill;
    struct Mutate {
        path: std::path::PathBuf,
        items: u64,
        done: bool,
    }
    impl BatchBoundary for Mutate {
        fn took(&mut self, stats: &syno_reclaim::btrfs::walk::WalkStats) {
            self.items += stats.items;
        }
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            if self.items >= 2 && !self.done {
                fs::write(&self.path, [2; 16384]).unwrap();
                self.done = true;
            }
            ControlFlow::Continue(())
        }
    }
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("change-spill-{}", std::process::id()));
    let root = base.join("data");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a"), [1; 4096]).unwrap();
    fs::write(root.join("b"), [1; 4096]).unwrap();
    let mut boundary = Mutate {
        path: root.join("a"),
        items: 0,
        done: false,
    };
    let result = scan(
        &[Input {
            path: root,
            volume_root: true,
            excluded: BTreeSet::new(),
        }],
        Spill::create(base.join("spill")).unwrap(),
        256,
        &mut boundary,
        &Default::default(),
        None,
    )
    .unwrap();
    assert!(boundary.done);
    assert!(
        result.changed > 0,
        "an already visited file changed while aggregating"
    );
    drop(result);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn fd_walk_preserves_bytes_deduplicates_links_and_skips_mounts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("walk-{}", std::process::id()));
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    fs::create_dir(root.join("mounted")).unwrap();
    fs::write(root.join("mounted/hidden"), vec![0; 8192]).unwrap();
    fs::write(root.join("a/data"), vec![1; 8192]).unwrap();
    fs::hard_link(root.join("a/data"), root.join("b/link")).unwrap();
    let sparse = fs::File::create(root.join("a/sparse")).unwrap();
    sparse.set_len(8 << 30).unwrap();
    symlink("mounted/hidden", root.join("symlink")).unwrap();
    let name = std::ffi::OsString::from_vec(b"bytes-\xff".to_vec());
    fs::write(root.join(&name), b"x").unwrap();
    let mut scan = walk(
        &root,
        &BTreeSet::from([b"mounted".to_vec()]),
        true,
        &mut Boundary,
    )
    .unwrap();
    assert!(scan.stopped.is_none());
    assert_eq!(scan.unreadable, 0);
    let totals = scan.tree.account_inode_allocations();
    let expected = ["", "a", "b", "a/data", "a/sparse", "symlink"]
        .into_iter()
        .map(|name| fs::symlink_metadata(root.join(name)).unwrap().blocks() * 512)
        .sum::<u64>()
        + fs::metadata(root.join(&name)).unwrap().blocks() * 512;
    assert_eq!(totals.metrics[0].physical, expected);
    assert!(
        scan.tree
            .tree()
            .preorder()
            .iter()
            .any(|&(node, _)| scan.tree.tree().name(node) == b"bytes-\xff")
    );
    assert_eq!(totals.metrics[0].logical, (8 << 30) + 8193);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tiny_budget_spills_and_completes_with_the_same_inode_totals() {
    use syno_reclaim::ext4::spilling::{Input, scan};
    use syno_reclaim::spill::Spill;
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("spill-walk-{}", std::process::id()));
    let root = base.join("data");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    for i in 0..500 {
        fs::write(root.join("a").join(format!("f{i}")), [1; 1024]).unwrap();
    }
    fs::hard_link(root.join("a/f0"), root.join("b/link")).unwrap();
    fs::create_dir(root.join("a/package")).unwrap();
    fs::write(root.join("a/package/SynologyHyperBackup.bkpi"), b"marker").unwrap();
    fs::write(root.join("a/package/data"), [3; 8192]).unwrap();
    fs::hard_link(root.join("a/f1"), root.join("a/package/link")).unwrap();
    let mut oracle = walk(&root, &BTreeSet::new(), true, &mut Boundary).unwrap();
    let aggregate = oracle.tree.account_inode_allocations();
    let expected = aggregate.metrics[0];
    let expected_nodes: std::collections::BTreeMap<_, _> = oracle
        .tree
        .tree()
        .preorder()
        .iter()
        .map(|&(node, depth)| {
            let record = syno_reclaim::report::tree_record(
                oracle.tree.tree(),
                aggregate.totals(),
                node,
                depth,
            );
            (
                oracle.tree.tree().path(node),
                (record.metrics, record.blockers, record.held, record.outside),
            )
        })
        .collect();
    let input = Input {
        path: root.clone(),
        volume_root: true,
        excluded: BTreeSet::new(),
    };
    for (label, budget) in [("tiny", 256), ("large", 64 << 20)] {
        let spill = Spill::create(base.join(label)).unwrap();
        let result = scan(
            std::slice::from_ref(&input),
            spill,
            budget,
            &mut Boundary,
            &Default::default(),
            None,
        )
        .unwrap();
        assert_eq!(result.totals, expected);
        assert!(result.stopped.is_none());
        assert_eq!(result.nodes.count(), oracle.tree.tree().len() as u64 - 1);
        let mut bytes = Vec::new();
        result
            .nodes
            .write(&mut syno_reclaim::codec::HashingWriter::new(&mut bytes))
            .unwrap();
        let mut decoder = syno_reclaim::codec::Decoder::new(&bytes, 0);
        let mut parents = Vec::<Vec<u8>>::new();
        let mut actual = std::collections::BTreeMap::new();
        while let Some(record) = syno_reclaim::report::decode_record(&mut decoder) {
            parents.truncate(record.depth as usize);
            let mut path = parents.last().cloned().unwrap_or_default();
            if !path.is_empty() && !path.ends_with(b"/") {
                path.push(b'/');
            }
            path.extend_from_slice(record.name);
            parents.push(path.clone());
            actual.insert(
                path,
                (record.metrics, record.blockers, record.held, record.outside),
            );
        }
        assert_eq!(actual.len(), expected_nodes.len());
        for (path, expected) in &expected_nodes {
            assert_eq!(
                actual.get(path),
                Some(expected),
                "{}",
                String::from_utf8_lossy(path)
            );
        }
        drop(result);
    }
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn spilling_honours_disk_reserve_and_user_cancellation() {
    use syno_reclaim::ext4::spilling::{Input, scan};
    use syno_reclaim::spill::Spill;
    struct Stop(StopReason);
    impl BatchBoundary for Stop {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Break(self.0)
        }
    }
    for (index, reason) in [StopReason::DiskLimit, StopReason::Cancelled]
        .into_iter()
        .enumerate()
    {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(format!("stop-spill-{}-{index}", std::process::id()));
        let input = Input {
            path: std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf(),
            volume_root: false,
            excluded: BTreeSet::new(),
        };
        let spill = Spill::create(base.clone()).unwrap();
        let error = match scan(
            &[input],
            spill,
            256,
            &mut Stop(reason),
            &Default::default(),
            None,
        ) {
            Err(error) => error,
            Ok(_) => panic!("stop was ignored"),
        };
        assert!(
            matches!(syno_reclaim::ext4::records::walk_error(error), syno_reclaim::btrfs::walk::WalkError::Stopped(found) if found == reason)
        );
        assert!(!base.exists(), "spill must be removed on every exit");
    }
}

#[test]
#[ignore = "needs a disposable mounted ext4 loop image"]
fn loop_ext4_tiny_spill_budget_matches_unlimited_du_and_statvfs() {
    use std::os::fd::AsFd;
    use syno_reclaim::ext4::spilling::{Input, scan};
    use syno_reclaim::spill::Spill;
    let root = std::path::PathBuf::from(std::env::var_os("EXT4_FIXTURE").expect("fixture mount"));
    let file = fs::File::open(&root).unwrap();
    assert_eq!(
        syno_reclaim::sys::filesystem_type(file.as_fd()).unwrap(),
        syno_reclaim::ext4::SUPER_MAGIC
    );
    let before = syno_reclaim::sys::filesystem_space(file.as_fd()).unwrap();
    let du = std::process::Command::new("du")
        .args(["-x", "-B1", "-s"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(du.status.success());
    let expected: u64 = std::str::from_utf8(&du.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let mut totals = Vec::new();
    for (label, budget) in [("loop-tiny", 256), ("loop-unlimited", 64 << 20)] {
        let spill = Spill::create(root.parent().unwrap().join(label)).unwrap();
        let result = scan(
            &[Input {
                path: root.clone(),
                volume_root: true,
                excluded: BTreeSet::new(),
            }],
            spill,
            budget,
            &mut Boundary,
            &Default::default(),
            None,
        )
        .unwrap();
        assert_eq!(result.totals.physical, expected);
        assert_eq!(result.changed, 0);
        assert_eq!(result.unreadable, 0);
        totals.push(result.totals);
        drop(result);
    }
    assert_eq!(totals[0], totals[1]);
    assert_eq!(
        before,
        syno_reclaim::sys::filesystem_space(file.as_fd()).unwrap()
    );
}

#[test]
fn spilling_reports_aggregation_work_without_counting_verification_as_new_entries() {
    use syno_reclaim::btrfs::walk::WalkStats;
    use syno_reclaim::ext4::spilling::{Input, scan};
    use syno_reclaim::spill::Spill;
    #[derive(Default)]
    struct Progress {
        phase: &'static str,
        entries: u64,
        aggregate_items: u64,
        aggregate_bytes: u64,
    }
    impl BatchBoundary for Progress {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            ControlFlow::Continue(())
        }
        fn phase(&mut self, phase: &'static str) {
            self.phase = phase;
        }
        fn took(&mut self, stats: &WalkStats) {
            self.entries += stats.dir_index_items;
            if self.phase == "aggregate" {
                self.aggregate_items += stats.items;
                self.aggregate_bytes += stats.item_bytes;
                assert_eq!(
                    stats.dir_index_items, 0,
                    "verification must not double-count entries"
                );
            }
        }
    }
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("spill-progress-{}", std::process::id()));
    let root = base.join("data");
    fs::create_dir_all(&root).unwrap();
    for i in 0..300 {
        fs::write(root.join(format!("f{i}")), b"data").unwrap();
    }
    let mut progress = Progress::default();
    let result = scan(
        &[Input {
            path: root,
            volume_root: true,
            excluded: BTreeSet::new(),
        }],
        Spill::create(base.join("spill")).unwrap(),
        4096,
        &mut progress,
        &Default::default(),
        None,
    )
    .unwrap();
    assert_eq!(
        progress.entries, 300,
        "only the namespace walk counts entries"
    );
    assert_eq!(progress.phase, "aggregate");
    assert!(
        progress.aggregate_items > 300,
        "sort/merge and verification work must move progress"
    );
    assert!(
        progress.aggregate_bytes > 0,
        "encoded spill bytes must be observable"
    );
    assert_eq!(result.totals.entries, 301);
    drop(result);
    fs::remove_dir_all(base).unwrap();
}
