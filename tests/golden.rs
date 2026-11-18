//! Layer-1 golden tests: replay TREE_SEARCH_V2 buffers recorded from Btrfs
//! fixtures (`scripts/record-golden.sh`) and compare the reconstructed tree
//! with the `find` enumeration captured at recording time. No root, no
//! Btrfs, any host.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use syno_reclaim::btrfs::recording::ReplaySource;
use syno_reclaim::btrfs::subvolume::discover;
use syno_reclaim::btrfs::walk::BatchBoundary;
use syno_reclaim::btrfs::walk::StopReason;
use syno_reclaim::model::namespace::{Namespace, format_listing};

struct NoStop;

impl BatchBoundary for NoStop {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        ControlFlow::Continue(())
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn read_meta(path: &Path) -> BTreeMap<String, u64> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let (k, v) = line.split_once('=').unwrap();
            (k.to_string(), v.parse().unwrap())
        })
        .collect()
}

fn check_recording(meta_path: &Path) {
    let meta = read_meta(meta_path);
    let bytes = std::fs::read(meta_path.with_extension("srbuf")).unwrap();
    let expected = std::fs::read(meta_path.with_extension("listing")).unwrap();
    let mut replay = ReplaySource::parse(&bytes).unwrap();
    let subvolumes = discover(&mut replay, &mut NoStop).unwrap();
    let ns = Namespace::read(&mut replay, meta["tree_id"], &mut NoStop).unwrap();
    assert_eq!(replay.remaining(), 0, "unreplayed calls in {meta_path:?}");
    assert_eq!(subvolumes.subvolumes.len() as u64, meta["subvolumes"]);
    let listing = format_listing(&ns.listing(meta["root_ino"], &subvolumes).unwrap());
    assert!(
        listing == expected,
        "{meta_path:?}: reconstructed tree differs from find:\n{}",
        String::from_utf8_lossy(&listing)
    );
}

#[test]
fn recorded_fixtures_reconstruct_to_their_find_listings() {
    let mut metas: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "meta"))
        .collect();
    metas.sort();
    assert!(!metas.is_empty(), "no golden recordings in tests/golden");
    for meta in &metas {
        check_recording(meta);
    }
}
