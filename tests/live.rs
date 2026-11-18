//! Layer-2 integration tests against a live loop-mounted Btrfs fixture.
//! Run through `scripts/integration.sh` as root on Linux; ignored otherwise.
#![cfg(target_os = "linux")]

#[path = "support/schema.rs"]
#[allow(dead_code)]
mod support;

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use syno_reclaim::model::namespace::{FileKind, ListingEntry, format_listing};

fn fixture() -> PathBuf {
    PathBuf::from(std::env::var("SYNORECLAIM_FIXTURE").expect("SYNORECLAIM_FIXTURE is set"))
}

/// Normal directory enumeration with std::fs, not stopping at but not
/// descending into other devices (nested subvolumes).
fn enumerate(root: &Path) -> Vec<ListingEntry> {
    let root_dev = std::fs::symlink_metadata(root).unwrap().dev();
    let mut out = Vec::new();
    let mut pending = vec![(root.to_path_buf(), Vec::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend(entry.file_name().as_encoded_bytes());
            let kind = FileKind::from_mode(meta.mode());
            let is_dir = kind == FileKind::Directory;
            if is_dir && meta.dev() == root_dev {
                pending.push((entry.path(), path.clone()));
            }
            out.push(ListingEntry {
                path,
                kind,
                ino: meta.ino(),
                size: (!is_dir).then_some(meta.size()),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn synoreclaim(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_synoreclaim"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_tree_matches_enumeration(path: &Path) {
    let out = synoreclaim(&["debug", "tree", path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = format_listing(&enumerate(path));
    assert_same_lines(&out.stdout, &expected);
}

/// Compares listings line by line, reporting only the differing lines.
fn assert_same_lines(actual: &[u8], expected: &[u8]) {
    let lines = |b: &[u8]| -> BTreeSet<String> {
        b.split(|&c| c == b'\n')
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect()
    };
    let (actual, expected) = (lines(actual), lines(expected));
    let missing: Vec<_> = expected.difference(&actual).collect();
    let unexpected: Vec<_> = actual.difference(&expected).collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "missing: {missing:#?}\nunexpected: {unexpected:#?}"
    );
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn subvolume_tree_matches_directory_enumeration() {
    assert_tree_matches_enumeration(&fixture().join("data"));
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn snapshot_tree_matches_directory_enumeration() {
    assert_tree_matches_enumeration(&fixture().join("snap"));
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn subtree_listing_matches_directory_enumeration() {
    assert_tree_matches_enumeration(&fixture().join("data/docs"));
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn doctor_passes_on_the_fixture() {
    let out = synoreclaim(&["doctor", fixture().join("data").to_str().unwrap(), "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(out.status.success(), "{report:#}");
    let failed: Vec<_> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "fail")
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
    let system_partition = &report["system_partition"];
    assert_eq!(
        system_partition["used_bytes"].as_u64().unwrap()
            + system_partition["free_bytes"].as_u64().unwrap(),
        system_partition["total_bytes"].as_u64().unwrap(),
        "{system_partition}"
    );
    assert_eq!(
        system_partition["free_bytes"], system_partition["available_bytes"],
        "{system_partition}"
    );
    assert!(system_partition["used_percent"].as_u64().unwrap() <= 100);
    assert!(matches!(
        system_partition["level"].as_str(),
        Some("ok" | "getting_full" | "full")
    ));
    assert!(system_partition["level_reasons"].is_array());
    assert_eq!(report["subvolumes"]["count"], 4, "{report:#}");
    assert_eq!(report["subvolumes"]["readonly"], 1);
    let partition = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "system_partition")
        .expect("a system_partition check");
    assert!(
        partition["detail"].as_str().unwrap().starts_with("/ ("),
        "{partition}"
    );
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn doctor_rejects_a_non_btrfs_target() {
    let out = synoreclaim(&["doctor", "/proc", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["checks"][0]["name"], "filesystem");
    assert_eq!(report["checks"][0]["status"], "fail");
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn maintenance_probes_see_an_idle_filesystem() {
    let dir = std::fs::File::open(fixture()).unwrap();
    let fd = std::os::fd::AsFd::as_fd(&dir);
    assert!(!syno_reclaim::sys::balance_running(fd).unwrap());
    assert!(!syno_reclaim::sys::dev_replace_running(fd).unwrap());
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_deleted_subvolumes_tree_search_fails_with_enoent() {
    use syno_reclaim::btrfs::keys::{FIRST_FREE_OBJECTID, Key};
    use syno_reclaim::btrfs::live::LiveSource;
    use syno_reclaim::btrfs::search::SearchRange;
    use syno_reclaim::btrfs::source::MetadataSource;
    let doomed = fixture().join(format!("doomed-{}", std::process::id()));
    btrfs_output(&["subvolume", "create", text(&doomed)]);
    let dir = std::fs::File::open(&doomed).unwrap();
    let tree = syno_reclaim::sys::subvolume_id(std::os::fd::AsFd::as_fd(&dir)).unwrap();
    let mut source = LiveSource::new(std::fs::File::open(fixture()).unwrap(), 4096);
    let everything = SearchRange::new(
        tree,
        Key::new(FIRST_FREE_OBJECTID, 0, 0),
        Key::new(u64::MAX, u8::MAX, u64::MAX),
    );
    assert!(source.search(&everything).is_ok());
    btrfs_output(&["subvolume", "delete", text(&doomed)]);
    let error = source.search(&everything).unwrap_err();
    assert!(
        matches!(&error, syno_reclaim::btrfs::source::SourceError::Io(e)
            if e.raw_os_error() == Some(libc::ENOENT)),
        "{error:?}"
    );
}

/// A private state directory for one test.
fn state_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("synoreclaim-live-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout {:?} stderr {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn run_json(args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let out = synoreclaim(args);
    (out.status.code(), json(&out))
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn write_entropy(path: &Path, size_mib: usize, seed: u64) {
    let mut state = seed;
    let mut block = vec![0u8; 1 << 20];
    for byte in &mut block {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = (state >> 24) as u8;
    }
    let mut file = std::fs::File::create(path).unwrap();
    for _ in 0..size_mib {
        file.write_all(&block).unwrap();
    }
    file.sync_all().unwrap();
}

fn btrfs_output(args: &[&str]) -> std::process::Output {
    let out = Command::new("btrfs").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "btrfs {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn btrfs_data_used(mount: &Path) -> u64 {
    let output = btrfs_output(&["filesystem", "df", "-b", text(mount)]);
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("Data"))
        .filter_map(|line| line.split_once("used=").map(|(_, value)| value))
        .map(|value| {
            let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u64>().unwrap()
        })
        .sum()
}

fn btrfs_data_used_after_sync(mount: &Path) -> u64 {
    btrfs(&["filesystem", "sync", text(mount)]);
    btrfs(&["filesystem", "sync", text(mount)]);
    btrfs_data_used(mount)
}

fn overview_item_reclaimable(doc: &serde_json::Value, kind: &str, share: &str) -> u64 {
    let parts = doc["results"][0]["parts"]
        .as_array()
        .expect("overview parts are an array");
    let part = parts
        .iter()
        .find(|part| part["kind"] == kind)
        .unwrap_or_else(|| panic!("overview omitted {kind} part: {doc:#}"));
    let items = part["items"]
        .as_array()
        .expect("overview part items are an array");
    let item = items
        .iter()
        .find(|item| item["share"] == share)
        .unwrap_or_else(|| panic!("overview omitted {kind} item for {share}: {doc:#}"));
    item["reclaimable_bytes"].as_u64().unwrap()
}

fn overview_part_reclaimable(doc: &serde_json::Value, kind: &str) -> u64 {
    doc["results"][0]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["kind"] == kind)
        .unwrap()["reclaimable_bytes"]
        .as_u64()
        .unwrap()
}

/// Independent oracle: `EXTENT_DATA` items of a subvolume tree as
/// printed by `btrfs inspect-internal dump-tree`, as
/// inode -> [(disk bytenr, disk bytes)], holes excluded.
fn dump_tree_extents(subvolume: &Path) -> std::collections::HashMap<u64, Vec<(u64, u64)>> {
    let output = |cmd: &str, args: &[&str]| {
        let out = Command::new(cmd).args(args).output().unwrap();
        assert!(out.status.success(), "{cmd} {args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let tree = output("btrfs", &["inspect-internal", "rootid", text(subvolume)]);
    let device = output("findmnt", &["-no", "SOURCE", "--target", text(&fixture())]);
    let dump = output(
        "btrfs",
        &["inspect-internal", "dump-tree", "-t", &tree, &device],
    );
    let mut extents: std::collections::HashMap<u64, Vec<(u64, u64)>> = Default::default();
    let mut inode = None;
    for line in dump.lines() {
        if let Some(key) = line.split("key (").nth(1) {
            let mut parts = key.split_whitespace();
            let objectid = parts.next().unwrap().parse::<u64>().ok();
            inode = objectid.filter(|_| parts.next() == Some("EXTENT_DATA"));
        } else if let (Some(ino), Some(rest)) = (inode, line.split("data disk byte ").nth(1)) {
            let words: Vec<&str> = rest.split_whitespace().collect();
            let (bytenr, bytes) = (words[0].parse().unwrap(), words[2].parse().unwrap());
            if bytenr != 0 {
                extents.entry(ino).or_default().push((bytenr, bytes));
            }
        }
    }
    extents
}

/// Expected `(logical, physical, extents)` of a path: distinct regular
/// inodes below it (not crossing into nested subvolumes) and the union
/// of their whole extents.
fn oracle_metrics(
    path: &Path,
    extents: &std::collections::HashMap<u64, Vec<(u64, u64)>>,
) -> (u64, u64, u64) {
    let dev = std::fs::symlink_metadata(path).unwrap().dev();
    let mut inodes = std::collections::BTreeMap::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(p) = pending.pop() {
        let meta = std::fs::symlink_metadata(&p).unwrap();
        if meta.dev() != dev {
            continue;
        }
        if meta.is_file() {
            inodes.insert(meta.ino(), meta.size());
        } else if meta.is_dir() {
            pending.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        }
    }
    let mut physical = std::collections::BTreeMap::new();
    for ino in inodes.keys() {
        for &(bytenr, bytes) in extents.get(ino).into_iter().flatten() {
            let e = physical.entry(bytenr).or_insert(0);
            *e = bytes.max(*e);
        }
    }
    (
        inodes.values().sum(),
        physical.values().sum(),
        physical.len() as u64,
    )
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn inspect_metrics_match_the_dump_tree_oracle() {
    let data = fixture().join("data");
    let state = state_dir("oracle");
    let (code, scan) = run_json(&[
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{scan:#}");
    let fs = &scan["filesystems"][0];
    assert_eq!(fs["completion"], "complete", "{scan:#}");
    assert_eq!(fs["consistency"], "observed-stable");
    assert_eq!(fs["targets"][0]["scope"], "path");

    let extents = dump_tree_extents(&data);
    let cases = [
        "extents",
        "extents/compressed/text.txt",
        "extents/reflink-src.bin",
        "extents/reflink-copy.bin",
        "extents/bookend.bin",
        "extents/prealloc.bin",
        "sparse.bin",
        "docs",
        "docs/hard-a",
        "many",
    ];
    let paths: Vec<PathBuf> = cases.iter().map(|c| data.join(c)).collect();
    let mut args = vec![
        "inspect",
        "--json",
        "--no-scan",
        "--state-dir",
        text(&state),
    ];
    args.extend(paths.iter().map(|p| text(p)));
    let (code, inspect) = run_json(&args);
    assert_eq!(code, Some(0), "{inspect:#}");
    for (case, (path, result)) in cases
        .iter()
        .zip(paths.iter().zip(inspect["results"].as_array().unwrap()))
    {
        let m = &result["entry"]["metrics"];
        let (logical, physical, count) = oracle_metrics(path, &extents);
        assert_eq!(
            (
                m["logical"].as_u64(),
                m["physical"].as_u64(),
                m["extents"].as_u64()
            ),
            (Some(logical), Some(physical), Some(count)),
            "{case}"
        );
        // The snapshot of `data` holds every extent written before it.
        assert_eq!(m["unknown"], 0, "{case}: {m}");
        assert_eq!(m["reclaimable"], 0, "{case}: {m}");
        assert_eq!(m["shared"].as_u64(), Some(physical), "{case}");
    }
    // The cases the oracle must actually exercise.
    let physical = |i: usize| {
        inspect["results"][i]["entry"]["metrics"]["physical"]
            .as_u64()
            .unwrap()
    };
    let logical = |i: usize| {
        inspect["results"][i]["entry"]["metrics"]["logical"]
            .as_u64()
            .unwrap()
    };
    assert!(
        physical(1) < logical(1),
        "compressed text must be smaller on disk"
    );
    assert_eq!(physical(2), physical(3), "reflink copy shares its source");
    assert_eq!(
        physical(4),
        512 * 1024 + 64 * 1024,
        "bookend pins the whole old extent"
    );
    assert!(physical(6) < logical(6), "sparse file has holes");
    assert_eq!(inspect["results"][8]["entry"]["nlink"], 3);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn volume_scan_attaches_nested_subvolumes_and_ranks_children() {
    let state = state_dir("volume");
    let mnt = fixture();
    let (code, scan) = run_json(&[
        "scan",
        text(&mnt),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{scan:#}");
    let fs = &scan["filesystems"][0];
    assert_eq!(fs["targets"][0]["scope"], "volume");
    assert_eq!(fs["stats"]["subvolumes_scanned"], 3, "{fs:#}");
    assert_eq!(fs["coverage"]["snapshots_not_materialized"], 1);
    // An idle fixture: the balance/replace probes succeed and see nothing.
    assert_eq!(fs["consistency"], "observed-stable", "{fs:#}");
    let report = fs["report"]["path"].as_str().unwrap();
    let mode = std::fs::metadata(report).unwrap().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let (code, top) = run_json(&["top", text(&mnt), "--json", "--state-dir", text(&state)]);
    assert_eq!(code, Some(0), "{top:#}");
    assert_eq!(top["ranked_by"], "reclaimable");
    assert_eq!(top["warnings"], serde_json::json!([]), "{top:#}");
    let entries = top["results"][0]["entries"].as_array().unwrap();
    // Each DSM service is one row ranked among the entries; a share's
    // #recycle is a row of its own.
    let rows: Vec<String> = entries
        .iter()
        .map(|e| match e["service"]["id"].as_str() {
            Some(id) => format!("[{id}]"),
            None => e["path"].as_str().unwrap().replace(text(&mnt), ""),
        })
        .collect();
    let expected = [
        "[synology_drive]",
        "[container_manager]",
        "[surveillance_recordings]",
        "[other_dsm_service_data]",
        "/data",
        "/data/#recycle",
        "[active_backup_data]",
        "[hyper_backup]",
        "/snap",
    ];
    assert_eq!(rows, expected, "{top:#}");
    let fixes: Vec<&str> = entries
        .iter()
        .map(|e| e["where_to_fix"]["token"].as_str().unwrap())
        .collect();
    assert_eq!(fixes[..4], ["package"; 4]);
    assert_eq!(
        fixes[4..],
        [
            "snapshot_replication",
            "recycle_bin",
            "package",
            "package",
            "snapshot_replication"
        ]
    );
    // Service data, the Recycle Bin itself and a share holding package
    // data are never cleanup targets.
    let eligible: Vec<&str> = rows
        .iter()
        .zip(entries)
        .filter(|(_, e)| e["cleanup_eligible"] == true)
        .map(|(row, _)| row.as_str())
        .collect();
    assert_eq!(eligible, ["/snap"], "{top:#}");
    let drive = &entries[0];
    assert_eq!(drive["metrics"]["reclaimable"], 262144, "{drive:#}");
    assert_eq!(drive["held_by"], serde_json::json!([]));
    assert_eq!(
        entries[4]["held_by"][0]["class"], "snapshot",
        "the share is held by its snapshot: {top:#}"
    );
    assert!(
        entries[8]["flags"]
            .as_array()
            .unwrap()
            .contains(&"snapshot_not_read".into())
    );

    let services = &top["results"][0]["services"];
    let lines: Vec<&str> = services["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["service"].as_str().unwrap())
        .collect();
    for code in [
        "synology_drive",
        "container_manager",
        "recycle_bin",
        "folder_metadata",
        "hyper_backup",
        "active_backup_data",
        "surveillance_recordings",
        "other_dsm_service_data",
    ] {
        assert!(lines.contains(&code), "{code}: {services:#}");
    }
    assert_eq!(services["other_snapshots_not_read"], 1, "{services:#}");

    // A share's #recycle is a ranked candidate, not hidden DSM data.
    let data = mnt.join("data");
    let args = [
        "top",
        text(&data),
        "--json",
        "--no-scan",
        "--state-dir",
        text(&state),
    ];
    let (code, share) = run_json(&args);
    assert_eq!(code, Some(0), "{share:#}");
    let recycle = mnt.join("data/#recycle");
    assert!(
        share["results"][0]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == text(&recycle)),
        "{share:#}"
    );
    // A folder holding @ActiveBackup is the service row's path, with all
    // it holds, and never ranks again as a row of its own.
    let abb = mnt.join("data/abb");
    let at_abb: Vec<&serde_json::Value> = share["results"][0]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["path"] == text(&abb))
        .collect();
    assert_eq!(at_abb.len(), 1, "{share:#}");
    assert_eq!(at_abb[0]["service"]["id"], "active_backup_data");

    let nested = mnt.join("data/nested/file");
    let (code, inspect) = run_json(&[
        "inspect",
        text(&nested),
        "--json",
        "--no-scan",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{inspect:#}");
    assert_eq!(inspect["results"][0]["entry"]["kind"], "file");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_second_scan_reuses_units_and_deleting_the_cache_changes_only_the_work() {
    let state = state_dir("cache");
    let mnt = fixture();
    let scan = || {
        let args = [
            "scan",
            text(&mnt),
            "--json",
            "--quiet",
            "--state-dir",
            text(&state),
        ];
        let (code, doc) = run_json(&args);
        assert_eq!(code, Some(0), "{doc:#}");
        doc["filesystems"][0].clone()
    };
    let cache = |action| {
        let args = [
            "cache",
            action,
            text(&mnt),
            "--json",
            "--state-dir",
            text(&state),
        ];
        let (code, doc) = run_json(&args);
        assert_eq!(code, Some(0), "{doc:#}");
        doc["filesystems"][0].clone()
    };
    let first = scan();
    let second = scan();
    assert_eq!(second["stats"]["subvolumes_scanned"], 0, "{second:#}");
    assert_eq!(second["stats"]["subvolumes_reused"], 3);
    assert_eq!(second["totals"], first["totals"]);
    assert_eq!(second["coverage"], first["coverage"]);
    let status = cache("status");
    let units = status["units"].as_array().unwrap();
    assert_eq!(units.len(), 3, "{status:#}");
    assert!(units.iter().all(|u| u["reusable"] == true));
    assert!(status["bytes"].as_u64().unwrap() > 0);
    assert_eq!(cache("clear")["cleared"], 3);
    assert_eq!(cache("status")["units"], serde_json::json!([]));
    let third = scan();
    assert_eq!(third["stats"]["subvolumes_reused"], 0);
    assert_eq!(third["stats"]["subvolumes_scanned"], 3);
    assert_eq!(third["totals"], first["totals"]);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_stopped_job_can_be_cancelled_but_not_paused() {
    let state = state_dir("cancel");
    let data = fixture().join("data");
    let s = ["--state-dir", text(&state)];
    let scan = [
        &[
            "scan",
            text(&data),
            "--json",
            "--quiet",
            "--max-runtime",
            "0s",
        ],
        &s[..],
    ]
    .concat();
    let (code, doc) = run_json(&scan);
    assert_eq!(code, Some(10), "{doc:#}");
    let id = doc["job_id"].as_str().unwrap();
    let pause = synoreclaim(&[&["pause", id], &s[..]].concat());
    assert_eq!(pause.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&pause.stderr).contains("not running"));
    let (code, pause) = run_json(&[&["pause", id, "--json"], &s[..]].concat());
    assert_eq!(code, Some(1), "{pause:#}");
    assert_eq!(
        (&pause["outcome"], &pause["state"]),
        (&"not_running".into(), &"paused".into())
    );
    let (code, cancel) = run_json(&[&["cancel", id, "--json"], &s[..]].concat());
    assert_eq!(code, Some(0), "{cancel:#}");
    assert_eq!(
        (&cancel["outcome"], &cancel["state"]),
        (&"cancelled".into(), &"cancelled".into())
    );
    assert_eq!(cancel["job_id"], id);
    let (_, jobs) = run_json(&[&["jobs", "--json"], &s[..]].concat());
    assert_eq!(jobs["jobs"][0]["state"], "cancelled", "{jobs:#}");
    let missing = synoreclaim(&[&["cancel", "no-such-job"], &s[..]].concat());
    assert_eq!(missing.status.code(), Some(1));
    let (code, missing) = run_json(&[&["cancel", "no-such-job", "--json"], &s[..]].concat());
    assert_eq!((code, &missing["outcome"]), (Some(1), &"no_job".into()));
    assert_eq!(missing["state"], serde_json::Value::Null);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_job_another_binary_started_is_refused_as_incompatible() {
    let state = state_dir("incompatible");
    let data = fixture().join("data");
    let s = ["--json", "--quiet", "--state-dir", text(&state)];
    let (_, doc) = run_json(&[&["scan", text(&data), "--max-runtime", "0s"], &s[..]].concat());
    let id = doc["job_id"].as_str().unwrap();
    // What an older binary leaves: another report format.
    let manifest = state.join("jobs").join(id).join("job.json");
    let mut job: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    job["versions"]["report_format"] = 1.into();
    std::fs::write(&manifest, serde_json::to_vec(&job).unwrap()).unwrap();
    let (_, jobs) = run_json(&["jobs", "--json", "--state-dir", text(&state)]);
    assert_eq!(jobs["jobs"][0]["compatible"], false, "{jobs:#}");
    let (code, refused) = run_json(&[&["resume", id], &s[..]].concat());
    assert_eq!(code, Some(4), "{refused:#}");
    assert_eq!(refused["command"], "resume");
    assert_eq!(
        (&refused["outcome"], &refused["state"]),
        (&"job_incompatible".into(), &"paused".into())
    );
    let human = synoreclaim(&["resume", id, "--quiet", "--state-dir", text(&state)]);
    assert_eq!(human.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&human.stderr).contains("start a new scan"),
        "{human:?}"
    );
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_job_whose_process_died_shows_as_interrupted_and_resumes() {
    let state = state_dir("crash");
    let data = fixture().join("data");
    let s = ["--state-dir", text(&state)];
    let scan = [
        &[
            "scan",
            text(&data),
            "--json",
            "--quiet",
            "--max-runtime",
            "0s",
        ],
        &s[..],
    ]
    .concat();
    let (_, doc) = run_json(&scan);
    let id = doc["job_id"].as_str().unwrap();
    // What a killed process leaves: a running manifest and no live lock.
    let manifest = state.join("jobs").join(id).join("job.json");
    let mut job: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    job["state"] = "running".into();
    std::fs::write(&manifest, serde_json::to_vec(&job).unwrap()).unwrap();
    let (_, jobs) = run_json(&[&["jobs", "--json"], &s[..]].concat());
    assert_eq!(jobs["jobs"][0]["state"], "interrupted", "{jobs:#}");
    let (code, resumed) = run_json(
        &[
            &["resume", id, "--max-runtime", "1h", "--json", "--quiet"],
            &s[..],
        ]
        .concat(),
    );
    assert_eq!(code, Some(0), "{resumed:#}");
    let (_, fresh) = run_json(
        &[
            &["scan", text(&data), "--json", "--quiet", "--no-cache"],
            &s[..],
        ]
        .concat(),
    );
    let totals = |d: &serde_json::Value| d["filesystems"][0]["totals"].clone();
    assert_eq!(
        totals(&resumed),
        totals(&fresh),
        "resuming changes only the work"
    );
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_paused_job_resumes_detached_through_its_launcher() {
    let state = state_dir("resume-detach");
    let data = fixture().join("data");
    let s = ["--state-dir", text(&state)];
    let scan = [
        &[
            "scan",
            text(&data),
            "--json",
            "--quiet",
            "--max-runtime",
            "0s",
        ],
        &s[..],
    ]
    .concat();
    let (_, doc) = run_json(&scan);
    let id = doc["job_id"].as_str().unwrap().to_string();
    let (code, refused) =
        run_json(&[&["resume", "nope", "--detach=none", "--json"], &s[..]].concat());
    assert_eq!(code, Some(1), "{refused:#}");
    assert_eq!(refused["outcome"], "no_job", "{refused:#}");
    let detach = [
        &["resume", &id, "--detach=none", "--profile", "fast"],
        &s[..],
    ]
    .concat();
    let out = synoreclaim(&detach);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let line = detach_line(&out);
    assert_eq!(line["job_id"], id.as_str(), "{line:#}");
    let run: Vec<&str> = line["run"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        run[1..],
        ["scan", "--job-id", &id, "--state-dir", text(&state)],
        "{line:#}"
    );
    assert!(
        line["result"]["path"]
            .as_str()
            .unwrap()
            .ends_with(&format!("jobs/{id}/result.json")),
        "{line:#}"
    );
    let job = only_job(&state);
    assert_eq!(
        (&job["state"], &job["resumes"]),
        (&"queued".into(), &1.into())
    );
    let ran = synoreclaim(&run[1..]);
    assert_eq!(
        ran.status.code(),
        Some(10),
        "the job keeps its 0s runtime: {ran:?}"
    );
    let job = only_job(&state);
    assert_eq!(
        (&job["state"], &job["resumes"], &job["profile"]),
        (&"paused".into(), &1.into(), &"fast".into()),
        "{job:#}"
    );
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_second_writer_is_refused_as_busy() {
    let state = state_dir("busy");
    let data = fixture().join("data");
    let (_, first) = run_json(&[
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    let fsid = first["filesystems"][0]["fsid"]
        .as_str()
        .unwrap()
        .to_string();
    let lock = format!("/run/synoreclaim/locks/{fsid}.lock");
    std::fs::write(&lock, br#"{"job_id":"holder-job","command":"scan"}"#).unwrap();
    // `-o`: only flock itself holds the lock, so killing it releases it.
    let mut holder = Command::new("flock")
        .args(["-o", &lock, "sleep", "10"])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (code, busy) = run_json(&[
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(code, Some(4), "{busy:#}");
    assert_eq!(busy["completion"], "busy");
    let error = &busy["filesystems"][0]["errors"][0];
    assert_eq!(error["code"], "busy");
    assert_eq!(
        error["holder"],
        serde_json::json!({"job_id": "holder-job", "command": "scan"}),
        "{busy:#}"
    );
    assert_eq!(
        busy["job_id"],
        serde_json::Value::Null,
        "a busy start keeps no job"
    );
    let (_, jobs) = run_json(&["jobs", "--json", "--state-dir", text(&state)]);
    let ids: Vec<&serde_json::Value> = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| &j["job_id"])
        .collect();
    assert_eq!(ids, [&first["job_id"]], "{jobs:#}");
    let _ = std::fs::remove_dir_all(&state);
}

/// The one line `scan --detach` prints, checked against its schema.
fn detach_line(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    assert_eq!(stdout.lines().count(), 1, "one JSON line: {out:?}");
    let line: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let errors = support::Schemas::load().validate("detach", &line);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    line
}

fn only_job(state: &Path) -> serde_json::Value {
    let (_, jobs) = run_json(&["jobs", "--json", "--state-dir", text(state)]);
    assert_eq!(jobs["jobs"].as_array().map(Vec::len), Some(1), "{jobs:#}");
    jobs["jobs"][0].clone()
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_queued_scan_runs_in_the_foreground_under_its_job_id() {
    let state = state_dir("queued");
    let data = fixture().join("data");
    let queue = [
        "scan",
        text(&data),
        "--detach=none",
        "--profile",
        "normal",
        "--requested-by=admin",
        "--state-dir",
        text(&state),
    ];
    let out = synoreclaim(&queue);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let line = detach_line(&out);
    assert_eq!(
        (&line["completion"], &line["launcher"]),
        (&"complete".into(), &"none".into())
    );
    assert_eq!(line["log"], serde_json::Value::Null, "{line:#}");
    let id = line["job_id"].as_str().unwrap().to_string();
    let run: Vec<&str> = line["run"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        run[1..],
        ["scan", "--job-id", &id, "--state-dir", text(&state)],
        "{line:#}"
    );
    let job = only_job(&state);
    assert_eq!(
        (&job["state"], &job["profile"], &job["requested_by"]),
        (&"queued".into(), &"normal".into(), &"admin".into())
    );
    assert_eq!(job["result"], serde_json::Value::Null, "{job:#}");
    let result = [
        "jobs",
        "--result",
        &id,
        "--json",
        "--state-dir",
        text(&state),
    ];
    let (code, none) = run_json(&result);
    assert_eq!(
        (code, &none["outcome"]),
        (Some(1), &"no_result".into()),
        "{none:#}"
    );
    let launch = [
        "scan",
        "--job-id",
        &id,
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ];
    let (code, doc) = run_json(&launch);
    assert_eq!(code, Some(0), "{doc:#}");
    assert_eq!(doc["job_id"], id.as_str());
    assert_eq!(doc["requested_targets"][0]["path"], text(&data));
    let job = only_job(&state);
    assert_eq!(job["state"], "completed", "{job:#}");
    assert_eq!(job["result"]["completion"], "complete", "{job:#}");
    assert_eq!(
        run_json(&result),
        (Some(0), doc.clone()),
        "the saved document"
    );
    let again = synoreclaim(&launch);
    assert_eq!(again.status.code(), Some(1), "{again:?}");
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("has finished"), "{stderr}");
    let missing = synoreclaim(&["scan", "--job-id", "nope", "--state-dir", text(&state)]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    let _ = std::fs::remove_dir_all(&state);
}

/// The command and exit code of every answer in the UI cache.
fn ui_cache(state: &Path) -> Vec<(String, i64)> {
    let Ok(entries) = std::fs::read_dir(state.join("ui/cache")) else {
        return Vec::new();
    };
    let mut answers: Vec<(String, i64)> = entries
        .map(|e| {
            let entry: serde_json::Value =
                serde_json::from_slice(&std::fs::read(e.unwrap().path()).unwrap()).unwrap();
            let doc: serde_json::Value =
                serde_json::from_str(entry["document"].as_str().unwrap()).unwrap();
            (
                doc["command"].as_str().unwrap().to_string(),
                entry["exit_code"].as_i64().unwrap(),
            )
        })
        .collect();
    answers.sort();
    answers
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_volume_scan_for_a_dsm_user_prewarms_the_ui_cache() {
    let state = state_dir("prewarm");
    let mount = fixture();
    let queue = [
        "scan",
        text(&mount),
        "--detach=none",
        "--requested-by=admin",
        "--state-dir",
        text(&state),
    ];
    let out = synoreclaim(&queue);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let id = detach_line(&out)["job_id"].as_str().unwrap().to_string();
    let launch = [
        "scan",
        "--job-id",
        &id,
        "--quiet",
        "--state-dir",
        text(&state),
    ];
    let run = synoreclaim(&launch);
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    let answers = ui_cache(&state);
    let commands: Vec<&str> = answers.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(commands, ["overview", "top"], "{answers:?}");
    assert!(
        answers.iter().all(|(_, code)| matches!(code, 0 | 3)),
        "{answers:?}"
    );
    let plain = state_dir("prewarm-plain");
    let scan = synoreclaim(&["scan", text(&mount), "--quiet", "--state-dir", text(&plain)]);
    assert_eq!(scan.status.code(), Some(0), "{scan:?}");
    assert_eq!(ui_cache(&plain), [], "only scans a DSM user asked for");
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&plain);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_foreground_job_run_by_id_writes_live_progress() {
    // Progress must reach a run started by an external launcher
    // (`scan --job-id`), not only a plain `scan`.
    let state = fixture().join("queued-progress-state");
    let data = fixture().join("data");
    let out = synoreclaim(&[
        "scan",
        text(&data),
        "--detach=none",
        "--min-free",
        "16MiB",
        "--max-runtime",
        "0s",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let id = detach_line(&out)["job_id"].as_str().unwrap().to_string();
    let (code, _) = run_json(&[
        "scan",
        "--job-id",
        &id,
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(10), "the runtime limit stops the run");
    let (_, jobs) = run_json(&["jobs", "--json", "--state-dir", text(&state)]);
    let job = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["job_id"] == id)
        .unwrap_or_else(|| panic!("{jobs:#}"));
    let progress = &job["progress"];
    assert!(progress.is_object(), "{jobs:#}");
    assert!(progress["items"].as_u64().unwrap() > 0, "{jobs:#}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_detached_scan_finishes_after_its_caller_returns_and_leaves_its_result() {
    let state = state_dir("detached");
    let data = fixture().join("data");
    let out = synoreclaim(&["scan", text(&data), "--detach", "--state-dir", text(&state)]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let line = detach_line(&out);
    assert_eq!(line["launcher"], "self", "{line:#}");
    let id = line["job_id"].as_str().unwrap().to_string();
    let dir = state.join("jobs").join(&id);
    assert_eq!(
        line["log"]["path"],
        text(&dir.join("output.log")),
        "{line:#}"
    );
    let mut job = only_job(&state);
    for _ in 0..300 {
        if !["queued", "running"].contains(&job["state"].as_str().unwrap()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        job = only_job(&state);
    }
    let log = std::fs::read_to_string(dir.join("output.log")).unwrap_or_default();
    assert_eq!(job["state"], "completed", "{job:#}\n{log}");
    assert_eq!(job["result"]["exit_code"], 0, "{job:#}");
    let document = &job["result"]["document"]["path"];
    assert_eq!(document, &line["result"]["path"], "{job:#}");
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(document.as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(
        (&kept["command"], &kept["job_id"]),
        (&"scan".into(), &id.as_str().into())
    );
    let mode = std::fs::metadata(dir.join("output.log")).unwrap().mode();
    assert_eq!(mode & 0o777, 0o600);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_detached_scan_of_a_busy_volume_is_refused_and_a_busy_launch_fails() {
    let state = state_dir("queued-busy");
    let data = fixture().join("data");
    let queue = [
        "scan",
        text(&data),
        "--detach=none",
        "--state-dir",
        text(&state),
    ];
    let out = synoreclaim(&queue);
    let id = detach_line(&out)["job_id"].as_str().unwrap().to_string();
    let fsid = only_job(&state)["filesystems"][0]["fsid"]
        .as_str()
        .unwrap()
        .to_string();
    let lock = format!("/run/synoreclaim/locks/{fsid}.lock");
    let mut holder = Command::new("flock")
        .args(["-o", &lock, "sleep", "10"])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let refused = synoreclaim(&queue);
    let launched = synoreclaim(&[
        "scan",
        "--job-id",
        &id,
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(refused.status.code(), Some(4), "{refused:?}");
    let line = detach_line(&refused);
    assert_eq!(line["completion"], "busy", "{line:#}");
    assert_eq!(line["job_id"], serde_json::Value::Null, "{line:#}");
    assert_eq!(line["outcome"], "scan_in_progress", "{line:#}");
    assert_eq!(line["running_job_id"], id.as_str(), "{line:#}");
    assert_eq!(line["errors"], serde_json::json!([]), "{line:#}");
    assert_eq!(launched.status.code(), Some(4), "{launched:?}");
    let job = only_job(&state);
    assert_eq!(job["job_id"], id.as_str(), "the handed-out job is kept");
    assert_eq!(
        (&job["state"], &job["reason"]),
        (&"failed".into(), &"busy".into())
    );
    assert_eq!(job["result"]["completion"], "busy", "{job:#}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_resumed_job_that_finds_the_volume_busy_stays_paused() {
    let state = state_dir("busy-resume");
    let data = fixture().join("data");
    let s = ["--json", "--quiet", "--state-dir", text(&state)];
    let (code, stopped) =
        run_json(&[&["scan", text(&data), "--max-runtime", "0s"], &s[..]].concat());
    assert_eq!(code, Some(10), "{stopped:#}");
    let id = stopped["job_id"].as_str().unwrap().to_string();
    let fsid = stopped["filesystems"][0]["fsid"]
        .as_str()
        .unwrap()
        .to_string();
    let lock = format!("/run/synoreclaim/locks/{fsid}.lock");
    let mut holder = Command::new("flock")
        .args(["-o", &lock, "sleep", "10"])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (code, busy) = run_json(&[&["resume", &id], &s[..]].concat());
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(code, Some(4), "{busy:#}");
    assert_eq!(busy["job_id"], id.as_str());
    let (_, jobs) = run_json(&["jobs", "--json", "--state-dir", text(&state)]);
    let job = &jobs["jobs"][0];
    assert_eq!(job["job_id"], id.as_str(), "{jobs:#}");
    assert_eq!(
        (&job["state"], &job["reason"]),
        (&"paused".into(), &"busy".into())
    );
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn the_runtime_limit_stops_without_publishing_and_a_resume_finishes() {
    let state = state_dir("runtime");
    let data = fixture().join("data");
    let (code, doc) = run_json(&[
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--max-runtime",
        "0s",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(10), "{doc:#}");
    assert_eq!(doc["completion"], "runtime_limit");
    assert_eq!(doc["filesystems"][0]["report"], serde_json::Value::Null);
    let stats = &doc["filesystems"][0]["stats"];
    assert!(stats["peak_rss_bytes"].as_u64() > Some(0), "{doc:#}");
    let (code, top) = run_json(&[
        "top",
        text(&data),
        "--json",
        "--no-scan",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(1), "{top:#}");
    assert_eq!(top["errors"][0]["code"], "no_report");
    let (code, jobs) = run_json(&["jobs", "--json", "--state-dir", text(&state)]);
    assert_eq!(code, Some(0), "{jobs:#}");
    let job = &jobs["jobs"][0];
    assert_eq!(job["job_id"], doc["job_id"], "{jobs:#}");
    let resolved = &job["filesystems"][0];
    assert_eq!(resolved["fsid"], doc["filesystems"][0]["fsid"], "{jobs:#}");
    assert_eq!(resolved["mount"]["path"], text(&fixture()), "{jobs:#}");
    assert_eq!(job["request"], serde_json::Value::Null);
    assert_eq!(
        (&job["state"], &job["reason"]),
        (&"paused".into(), &"runtime_limit".into())
    );
    assert_eq!(job["targets"][0]["path"], text(&data));
    let id = job["job_id"].as_str().unwrap();
    let result = &job["result"];
    assert_eq!(result["completion"], "runtime_limit", "{jobs:#}");
    assert_eq!(result["exit_code"], 10, "{jobs:#}");
    assert_eq!(result["reports"], serde_json::json!([]), "{jobs:#}");
    let document = state.join("jobs").join(id).join("result.json");
    assert_eq!(result["document"]["path"], text(&document), "{jobs:#}");
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    assert_eq!(kept, doc, "result.json is the run's document");
    // The job keeps its limits unless the resume overrides them.
    let resume = [
        "resume",
        id,
        "--max-runtime",
        "1h",
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ];
    let (code, resumed) = run_json(&resume);
    assert_eq!(code, Some(0), "{resumed:#}");
    assert_eq!(resumed["job_id"], id);
    assert!(
        resumed["filesystems"][0]["report"]["bytes"].as_u64() > Some(0),
        "{resumed:#}"
    );
    let (_, jobs) = run_json(&["jobs", "--json", "--state-dir", text(&state)]);
    assert_eq!(jobs["jobs"][0]["state"], "completed", "{jobs:#}");
    assert_eq!(jobs["jobs"][0]["resumes"], 1);
    let result = &jobs["jobs"][0]["result"];
    assert_eq!(
        (&result["completion"], &result["exit_code"]),
        (&"complete".into(), &0.into())
    );
    assert_eq!(
        result["reports"][0]["path"], resumed["filesystems"][0]["report"]["path"],
        "{jobs:#}"
    );
    let again = synoreclaim(&resume);
    assert_eq!(again.status.code(), Some(1), "a finished job: {again:?}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_memory_stop_publishes_where_it_stopped() {
    let state = state_dir("memory");
    let data = fixture().join("data");
    let (code, doc) = run_json(&[
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--max-memory",
        "1M",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(10), "{doc:#}");
    assert_eq!(doc["completion"], "resource_limit");
    let fs = &doc["filesystems"][0];
    let stop = &fs["resource_limit"];
    assert_eq!(stop["budget_bytes"], 1 << 20, "{doc:#}");
    assert!(stop["tree"].as_u64() > Some(0), "{doc:#}");
    let resident = stop["resident_bytes"].as_u64().unwrap();
    let reserved = stop["reserved_bytes"].as_u64().unwrap();
    assert_eq!(stop["projected_bytes"].as_u64(), Some(resident + reserved));
    assert!(resident + reserved > 1 << 20, "{doc:#}");
    assert!(fs["stats"]["peak_rss_bytes"].as_u64() > Some(0), "{doc:#}");
    let errors = support::Schemas::load().validate("scan", &doc);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn support_bundle_uses_existing_report_redacts_ids_and_writes_a_private_file() {
    let state = state_dir("support-bundle");
    let mount = fixture();
    let (code, scan) = run_json(&[
        "scan",
        text(&mount),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{scan:#}");
    let (overview_code, overview) = run_json(&[
        "overview",
        text(&mount),
        "--json",
        "--quiet",
        "--no-scan",
        "--state-dir",
        text(&state),
    ]);
    assert!(matches!(overview_code, Some(0 | 3)), "{overview:#}");
    let report_path = scan["filesystems"][0]["report"]["path"]
        .as_str()
        .expect("volume scan report path");
    let header =
        syno_reclaim::report::read_header(std::fs::File::open(report_path).unwrap()).unwrap();

    let output = state.join("support.json");
    let result = synoreclaim(&[
        "support-bundle",
        text(&mount),
        "--output",
        text(&output),
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(result.status.code(), Some(3));

    let bytes = std::fs::read(&output).unwrap();
    assert!(bytes.len() <= 1 << 20);
    assert_eq!(std::fs::metadata(&output).unwrap().mode() & 0o777, 0o600);
    let rendered = String::from_utf8(bytes).unwrap();
    assert!(!rendered.contains(text(&mount)));
    assert!(!rendered.contains(&header.fsid));
    let bundle: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(bundle["command"], "support-bundle");
    assert!(!bundle["report_missing"].as_bool().unwrap());
    assert_eq!(bundle["overview"]["completion"], overview["completion"]);
    assert_eq!(
        bundle["overview"]["reconciliation"],
        overview["results"][0]["reconciliation"]
    );
    let bundled_parts = bundle["overview"]["parts"].as_array().unwrap();
    let overview_parts = overview["results"][0]["parts"].as_array().unwrap();
    assert_eq!(bundled_parts.len(), overview_parts.len());
    for (bundled, part) in bundled_parts.iter().zip(overview_parts) {
        assert_eq!(bundled["kind"], part["kind"]);
        assert_eq!(bundled["bytes"], part["bytes"]);
        assert_eq!(bundled["reclaimable_bytes"], part["reclaimable_bytes"]);
        assert_eq!(bundled["held_bytes"], part["held_bytes"]);
        let reasons = bundled["reasons"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let source_reasons = part["reasons"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        assert_eq!(reasons.len(), source_reasons.len());
        for (reason, source_reason) in reasons.iter().zip(source_reasons) {
            assert_eq!(reason["code"], source_reason["code"]);
            assert_eq!(reason["bytes"], source_reason["bytes"]);
        }
    }
    let (_, why) = run_json(&[
        "why-not-freed",
        text(&mount),
        "--json",
        "--state-dir",
        text(&state),
    ]);
    let bundled_rows = bundle["why_not_freed"]["reasons"].as_array().unwrap();
    let rows = why["results"][0]["reasons"].as_array().unwrap();
    assert_eq!(bundle["why_not_freed"]["completion"], why["completion"]);
    assert_eq!(bundled_rows.len(), rows.len());
    for (bundled, row) in bundled_rows.iter().zip(rows) {
        assert_eq!(bundled["code"], row["code"]);
        assert_eq!(bundled["unknown"], row["unknown"]);
    }
    assert!(!rendered.contains("\"share\": \"data\""), "{rendered}");
    assert!(
        !bundle["missing_parts"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("why_not_freed"))
    );
    assert!(bundle["scan_summary"].is_object());
    assert!(bundle["top_rows"].as_array().unwrap().len() <= 50);
    assert!(
        bundle["missing_parts"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("snapshots"))
    );

    let _ = std::fs::remove_dir_all(state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_volume_without_a_report_gets_the_scan_command_and_the_last_scans_cost() {
    let state = state_dir("hint");
    let mnt = fixture();
    let top = |json: bool| {
        let mut args = vec!["top", text(&mnt), "--state-dir", text(&state)];
        if json {
            args.push("--json");
        }
        synoreclaim(&args)
    };
    let doc = json(&top(true));
    assert_eq!(doc["errors"][0]["code"], "no_report", "{doc:#}");
    let hint = &doc["scan_hints"][0];
    let command = format!(
        "synoreclaim scan {} --state-dir {}",
        text(&mnt),
        text(&state)
    );
    assert_eq!(hint["command"], command.as_str(), "{doc:#}");
    assert_eq!(hint["last_scan"], serde_json::Value::Null);
    let errors = support::Schemas::load().validate("top", &doc);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let (code, scan) = run_json(&[
        "scan",
        text(&mnt),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{scan:#}");
    std::fs::remove_dir_all(state.join("reports")).unwrap();
    let doc = json(&top(true));
    let last = &doc["scan_hints"][0]["last_scan"];
    assert_eq!(
        last["entries"], scan["filesystems"][0]["stats"]["entries"],
        "{doc:#}"
    );
    let human = String::from_utf8(top(false).stdout).unwrap();
    assert!(human.contains(&format!("hint: run `{command}`")), "{human}");
    assert!(human.contains("its last full scan took"), "{human}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn reports_that_cannot_be_read_are_shown_as_incompatible_not_missing() {
    let state = state_dir("incompatible");
    let mnt = fixture();
    let (code, scan) = run_json(&[
        "scan",
        text(&mnt),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{scan:#}");
    let fsid = scan["filesystems"][0]["fsid"].as_str().unwrap();
    for report in std::fs::read_dir(state.join("reports").join(fsid)).unwrap() {
        std::fs::write(report.unwrap().path(), b"an older report format").unwrap();
    }
    let (_, volumes) = run_json(&["volumes", "--json", "--state-dir", text(&state)]);
    let volume = volumes["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["fsid"] == fsid)
        .unwrap();
    assert_eq!(volume["reports"], 0, "{volume:#}");
    assert_eq!(volume["incompatible_reports"], 1, "{volume:#}");
    assert!(
        volume["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["code"] == "report_incompatible"),
        "{volume:#}"
    );
    let (code, top) = run_json(&[
        "top",
        text(&mnt),
        "--json",
        "--no-scan",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(1), "{top:#}");
    assert_eq!(top["errors"][0]["code"], "report_incompatible", "{top:#}");
    assert_eq!(
        top["scan_hints"].as_array().map(Vec::len),
        Some(1),
        "{top:#}"
    );
    let schemas = support::Schemas::load();
    for (command, doc) in [("volumes", &volumes), ("top", &top)] {
        let errors = schemas.validate(command, doc);
        assert!(errors.is_empty(), "{command}: {}", errors.join("\n"));
    }
    let _ = std::fs::remove_dir_all(&state);
}

/// Output of `synoreclaim args` run on a pseudo-terminal by `script`,
/// so its stderr is a terminal.
fn on_terminal(args: &[&str]) -> String {
    let command = std::iter::once(env!("CARGO_BIN_EXE_synoreclaim"))
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let out = Command::new("script")
        .args(["-qec", &command, "/dev/null"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn progress_lines_go_to_a_terminal_only_and_a_rescan_shows_an_eta() {
    let state = state_dir("progress");
    let mnt = fixture();
    let scan = ["scan", text(&mnt), "--state-dir", text(&state)];
    let piped = synoreclaim(&scan);
    assert_eq!(piped.status.code(), Some(0), "{piped:?}");
    let stderr = String::from_utf8_lossy(&piped.stderr);
    assert!(!stderr.contains("elapsed"), "{stderr}");
    let shown = on_terminal(&scan);
    assert!(shown.contains("subvolumes, "), "{shown}");
    assert!(shown.contains(" elapsed"), "{shown}");
    assert!(!shown.contains(", 0 items"), "{shown}");
    assert!(
        shown.contains("last scan"),
        "a rescan of the scope: {shown}"
    );
    let json = on_terminal(&[&scan[..], &["--json"]].concat());
    assert!(!json.contains(" elapsed"), "{json}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_published_scan_is_recorded_in_the_history() {
    let state = state_dir("history");
    let data = fixture().join("data");
    let (code, doc) = run_json(&[
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{doc:#}");
    let fs = &doc["filesystems"][0];
    let file = state
        .join("history")
        .join(format!("{}.json", fs["fsid"].as_str().unwrap()));
    let history: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let record = &history["scans"][0];
    assert_eq!(record["entries"], fs["stats"]["entries"], "{history:#}");
    assert_eq!(record["volume"], false);
    assert!(record["spill_peak_bytes"].as_u64() > Some(0), "{history:#}");
    let space = doctor_check(
        &["doctor", text(&data), "--state-dir", text(&state)],
        "state_space",
    );
    assert_eq!(space["status"], "pass", "{space:#}");
    assert!(
        space["detail"]
            .as_str()
            .unwrap()
            .contains("; largest recorded scan spill "),
        "{space:#}"
    );
    let full = doctor_check(
        &[
            "doctor",
            text(&data),
            "--state-dir",
            text(&state),
            "--min-free",
            "1024TiB",
        ],
        "state_space",
    );
    assert_eq!(full["status"], "warn", "{full:#}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_running_job_keeps_live_progress_in_its_manifest() {
    // The fixture volume is smaller than the default free-space reserve,
    // so keep the state on it with an explicit small reserve; /tmp is on
    // the system partition, whose 3% reserve can refuse the first scan.
    let state = fixture().join("live-progress-state");
    let data = fixture().join("data");
    let s = ["--state-dir", text(&state)];
    let scan = [
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--min-free",
        "16MiB",
    ];
    let (_, first) = run_json(&[&scan[..], &s[..]].concat());
    let entries = first["filesystems"][0]["stats"]["entries"]
        .as_u64()
        .unwrap();
    // A run stopped at the first batch boundary has already written one
    // progress update into its manifest.
    let (code, stopped) = run_json(&[&scan[..], &["--max-runtime", "0s"], &s[..]].concat());
    assert_eq!(code, Some(10), "{stopped:#}");
    let id = stopped["job_id"].as_str().unwrap();
    let (_, jobs) = run_json(&[&["jobs", "--json"], &s[..]].concat());
    let job = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["job_id"] == id)
        .unwrap_or_else(|| panic!("{jobs:#}"));
    let progress = &job["progress"];
    assert!(progress.is_object(), "{jobs:#}");
    assert!(progress["items"].as_u64().unwrap() > 0, "{jobs:#}");
    assert!(
        progress["basis"]["entries"].as_u64().unwrap() > 0,
        "{jobs:#}"
    );
    assert_eq!(progress["basis"]["entries"], entries, "{jobs:#}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn jobs_can_show_one_job_and_refuses_an_unknown_one() {
    let state = state_dir("one_job");
    let data = fixture().join("data");
    let s = ["--state-dir", text(&state)];
    let (_, doc) = run_json(
        &[
            &[
                "scan",
                text(&data),
                "--json",
                "--quiet",
                "--max-runtime",
                "0s",
            ],
            &s[..],
        ]
        .concat(),
    );
    let id = doc["job_id"].as_str().unwrap();
    let (_, one) = run_json(&[&["jobs", id, "--json"], &s[..]].concat());
    assert_eq!(one["jobs"].as_array().unwrap().len(), 1, "{one:#}");
    assert_eq!(one["jobs"][0]["job_id"], id, "{one:#}");
    let missing = synoreclaim(&[&["jobs", "no-such-job", "--json"], &s[..]].concat());
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    let _ = std::fs::remove_dir_all(&state);
}

/// The check `name` of the `doctor --json` run with `args`.
fn doctor_check(args: &[&str], name: &str) -> serde_json::Value {
    let out = synoreclaim(&[args, &["--json"]].concat());
    let report = json(&out);
    let checks = report["checks"].as_array().unwrap();
    let check = checks.iter().find(|c| c["name"] == name);
    check.unwrap_or_else(|| panic!("{report:#}")).clone()
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_scan_below_the_free_space_reserve_stops_as_a_paused_job() {
    let state = state_dir("reserve");
    let data = fixture().join("data");
    let (code, doc) = run_json(&[
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--no-cache",
        "--min-free",
        "1024TiB",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(10), "{doc:#}");
    assert_eq!(doc["completion"], "resource_limit");
    let fs = &doc["filesystems"][0];
    assert_eq!(fs["report"], serde_json::Value::Null);
    assert_eq!(fs["warnings"][0]["code"], "disk_limit", "{doc:#}");
    let (_, jobs) = run_json(&["jobs", "--json", "--state-dir", text(&state)]);
    assert_eq!(jobs["jobs"][0]["state"], "paused", "{jobs:#}");
    let _ = std::fs::remove_dir_all(&state);
}

/// Runs `btrfs args`, asserting success.
fn btrfs(args: &[&str]) {
    let out = Command::new("btrfs").args(args).output().unwrap();
    assert!(out.status.success(), "btrfs {args:?}: {out:?}");
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn bytes_a_deleted_snapshot_held_become_reclaimable() {
    let state = state_dir("unsnapshotted");
    let mnt = fixture();
    let lone = mnt.join("lone");
    let snap = mnt.join("lone-snap");
    btrfs(&["-q", "subvolume", "create", text(&lone)]);
    std::fs::write(lone.join("file"), vec![0x5a; 4 << 20]).unwrap();
    btrfs(&["filesystem", "sync", text(&mnt)]);
    btrfs(&[
        "-q",
        "subvolume",
        "snapshot",
        "-r",
        text(&lone),
        text(&snap),
    ]);
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let scan = [
        "scan",
        text(&lone),
        "--json",
        "--quiet",
        "--no-cache",
        "--state-dir",
        text(&state),
    ];
    let totals = |doc: &serde_json::Value| doc["filesystems"][0]["totals"].clone();
    let (code, held) = run_json(&scan);
    assert_eq!(code, Some(0), "{held:#}");
    assert_eq!(totals(&held)["reclaimable"], 0, "{held:#}");
    assert_eq!(totals(&held)["shared"], 4 << 20, "{held:#}");

    btrfs(&["-q", "subvolume", "delete", text(&snap)]);
    btrfs(&["subvolume", "sync", text(&mnt)]);
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let (code, freed) = run_json(&scan);
    btrfs(&["-q", "subvolume", "delete", text(&lone)]);
    btrfs(&["subvolume", "sync", text(&mnt)]);
    // Commit the removal: later tests expect an idle fixture.
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let _ = std::fs::remove_dir_all(&state);
    assert_eq!(code, Some(0), "{freed:#}");
    assert_eq!(totals(&freed)["reclaimable"], 4 << 20, "{freed:#}");
    assert_eq!(totals(&freed)["shared"], 0, "{freed:#}");
}

/// Data bytes the filesystem has allocated, from `btrfs filesystem df`.
fn data_used(mnt: &Path) -> u64 {
    let out = Command::new("btrfs")
        .args(["filesystem", "df", "-b", text(mnt)])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let df = String::from_utf8(out.stdout).unwrap();
    let line = df.lines().find(|l| l.starts_with("Data,")).unwrap();
    line.split("used=").nth(1).unwrap().trim().parse().unwrap()
}

/// Writes `mib` MiB to `path` and commits it.
fn write_mib(mnt: &Path, path: &Path, mib: usize) {
    std::fs::write(path, vec![0xa5; mib << 20]).unwrap();
    btrfs(&["filesystem", "sync", text(mnt)]);
}

/// Removes `path` and commits it.
fn remove_committed(mnt: &Path, path: &Path) {
    std::fs::remove_file(path).unwrap();
    btrfs(&["filesystem", "sync", text(mnt)]);
}

fn snapshot(mnt: &Path, share: &Path, name: &str) -> (PathBuf, u64) {
    let path = mnt.join(name);
    btrfs(&[
        "-q",
        "subvolume",
        "snapshot",
        "-r",
        text(share),
        text(&path),
    ]);
    btrfs(&["filesystem", "sync", text(mnt)]);
    let id = subvolume_id(&path);
    (path, id)
}

fn delete_subvolumes(mnt: &Path, paths: &[&Path]) {
    for path in paths {
        btrfs(&["-q", "subvolume", "delete", text(path)]);
    }
    btrfs(&["subvolume", "sync", text(mnt)]);
    btrfs(&["filesystem", "sync", text(mnt)]);
}

/// The `snapshots` row of subvolume `id`.
fn snapshot_row(doc: &serde_json::Value, id: u64) -> serde_json::Value {
    doc["results"][0]["shares"][0]["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["subvolume_id"] == id)
        .unwrap_or_else(|| panic!("no snapshot {id} in {doc:#}"))
        .clone()
}

const MIB: u64 = 1 << 20;

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn deleting_snapshots_frees_what_snapshots_computed() {
    let state = state_dir("snapshots");
    let mnt = fixture();
    let share = mnt.join("snapshare");
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    // s1: only1 + base; s2: base + mid; s3: mid + new; live: new.
    write_mib(&mnt, &share.join("only1"), 2);
    write_mib(&mnt, &share.join("base"), 4);
    let (s1, id1) = snapshot(&mnt, &share, "snapshare-s1");
    remove_committed(&mnt, &share.join("only1"));
    write_mib(&mnt, &share.join("mid"), 3);
    let (s2, id2) = snapshot(&mnt, &share, "snapshare-s2");
    remove_committed(&mnt, &share.join("base"));
    write_mib(&mnt, &share.join("new"), 1);
    let (s3, id3) = snapshot(&mnt, &share, "snapshare-s3");
    remove_committed(&mnt, &share.join("mid"));
    let s = ["--json", "--quiet", "--state-dir", text(&state)];
    let scan = |depth: &str| {
        let mut args = vec!["scan", text(&share), "--no-cache", "--snapshots", depth];
        args.extend(s);
        let (code, doc) = run_json(&args);
        assert_eq!(code, Some(0), "{doc:#}");
    };
    let selection = format!("id:{id1},id:{id2}");
    let mut args = vec!["snapshots", text(&share), "--simulate-delete", &selection];
    args.extend(s);

    scan("1");
    let (code, shallow) = run_json(&args);
    assert_eq!(code, Some(3), "{shallow:#}");
    let first = &shallow["results"][0]["shares"][0];
    assert_eq!(first["snapshots_not_materialized"], 2, "{shallow:#}");
    assert_eq!(
        snapshot_row(&shallow, id1)["exclusive_bytes"],
        serde_json::Value::Null
    );
    // mid is also in the unread s2, which changed after it was written.
    assert_eq!(
        snapshot_row(&shallow, id3)["unknown_bytes"],
        3 * MIB,
        "{shallow:#}"
    );
    let bounded = &first["simulation"];
    assert_eq!(bounded["exact"], false, "{shallow:#}");
    assert_eq!(bounded["upper_bound_bytes"], serde_json::Value::Null);
    assert_eq!(
        bounded["inexact_reasons"][0], "not_materialized",
        "{shallow:#}"
    );
    let command = bounded["exact_command"].as_str().unwrap();
    assert!(command.ends_with(" --snapshots all"), "{command}");

    scan("all");
    let (code, doc) = run_json(&args);
    assert_eq!(code, Some(0), "{doc:#}");
    let answer = &doc["results"][0]["shares"][0];
    assert_eq!(
        snapshot_row(&doc, id1)["exclusive_bytes"],
        2 * MIB,
        "{doc:#}"
    );
    assert_eq!(snapshot_row(&doc, id2)["exclusive_bytes"], 0, "{doc:#}");
    assert_eq!(snapshot_row(&doc, id3)["exclusive_bytes"], 0, "{doc:#}");
    assert_eq!(
        snapshot_row(&doc, id3)["shared_with_live_bytes"],
        MIB,
        "{doc:#}"
    );
    assert_eq!(answer["held_only_by_snapshots_bytes"], 9 * MIB, "{doc:#}");
    assert_eq!(answer["unknown_bytes"], 0, "{doc:#}");
    let freed = answer["simulation"]["freed_bytes"].as_u64().unwrap();
    assert_eq!(freed, 6 * MIB, "{doc:#}");
    assert_eq!(answer["simulation"]["exact"], true, "{doc:#}");
    assert_eq!(answer["simulation"]["upper_bound_bytes"], freed);
    assert_eq!(
        answer["simulation"]["exact_command"],
        serde_json::Value::Null
    );

    let mut roots = vec!["simulate-delete", text(&s1), text(&s2), "--no-scan"];
    roots.extend(s);
    let (code, sim) = run_json(&roots);
    assert_eq!(code, Some(0), "{sim:#}");
    let set = &sim["snapshot_sets"][0];
    assert_eq!(set["simulation"]["freed_bytes"], freed, "{sim:#}");
    assert_eq!(
        set["simulation"]["selected_ids"],
        serde_json::json!([id1, id2])
    );
    // new is also in s3: deleting it with s1 frees only what s1 alone holds.
    let new = share.join("new");
    let mut mixed = vec!["simulate-delete", text(&s1), text(&new), "--no-cache"];
    mixed.extend(s);
    let (code, together) = run_json(&mixed);
    assert_eq!(code, Some(0), "{together:#}");
    let with = &together["results"][0]["with_snapshots"];
    assert_eq!(
        with["snapshot_ids"],
        serde_json::json!([id1]),
        "{together:#}"
    );
    assert_eq!(with["freed_bytes"], 2 * MIB, "{together:#}");
    assert_eq!(with["unknown_bytes"], 0, "{together:#}");

    let before = data_used(&mnt);
    delete_subvolumes(&mnt, &[&s1, &s2]);
    let after = data_used(&mnt);
    delete_subvolumes(&mnt, &[&s3, &share]);
    let _ = std::fs::remove_dir_all(&state);
    assert_eq!(before - after, freed, "df used {before} -> {after}");
}

/// What deleting the snapshot roots `roots` frees, answered from the
/// report and by the deletion census of a scan that also deletes the
/// empty file `anchor`.
fn report_and_rescan(state: &Path, roots: &[&Path], anchor: &Path) -> (serde_json::Value, u64) {
    let s = ["--json", "--quiet", "--state-dir", text(state)];
    let mut args = vec!["simulate-delete"];
    args.extend(roots.iter().map(|r| text(r)));
    let mut from_report = args.clone();
    from_report.push("--no-scan");
    from_report.extend(s);
    let (code, report) = run_json(&from_report);
    assert_eq!(code, Some(0), "{report:#}");
    args.extend([text(anchor), "--no-cache"]);
    args.extend(s);
    let (code, rescan) = run_json(&args);
    assert_eq!(code, Some(0), "{rescan:#}");
    let with = &rescan["results"][0]["with_snapshots"];
    assert_eq!(with["unknown_bytes"], 0, "{rescan:#}");
    (
        report["snapshot_sets"][0]["simulation"].clone(),
        with["freed_bytes"].as_u64().unwrap(),
    )
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn the_report_answers_every_snapshot_set_as_the_rescan_does() {
    let state = state_dir("every-set");
    let mnt = fixture();
    let share = mnt.join("everyset");
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    let anchor = share.join("anchor");
    std::fs::write(&anchor, b"").unwrap();
    // s1: a + b; s2: b + c; s3: c + d; live: d.
    write_mib(&mnt, &share.join("a"), 1);
    write_mib(&mnt, &share.join("b"), 2);
    let (s1, _) = snapshot(&mnt, &share, "everyset-s1");
    remove_committed(&mnt, &share.join("a"));
    write_mib(&mnt, &share.join("c"), 3);
    let (s2, _) = snapshot(&mnt, &share, "everyset-s2");
    remove_committed(&mnt, &share.join("b"));
    write_mib(&mnt, &share.join("d"), 4);
    let (s3, _) = snapshot(&mnt, &share, "everyset-s3");
    remove_committed(&mnt, &share.join("c"));
    let mut args = vec!["scan", text(&share), "--no-cache", "--snapshots", "all"];
    args.extend(["--json", "--quiet", "--state-dir", text(&state)]);
    let (code, doc) = run_json(&args);
    assert_eq!(code, Some(0), "{doc:#}");

    let snapshots = [&s1, &s2, &s3];
    let mut answers = Vec::new();
    for mask in 1..8usize {
        let roots: Vec<&Path> = (0..3)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| snapshots[i].as_path())
            .collect();
        let (simulation, rescanned) = report_and_rescan(&state, &roots, &anchor);
        answers.push((mask, simulation, rescanned));
    }
    delete_subvolumes(&mnt, &[&s1, &s2, &s3, &share]);
    let _ = std::fs::remove_dir_all(&state);

    for (mask, simulation, rescanned) in answers {
        assert_eq!(simulation["exact"], true, "set {mask:03b}: {simulation:#}");
        assert_eq!(
            simulation["freed_bytes"], rescanned,
            "set {mask:03b}: {simulation:#}"
        );
        assert_eq!(simulation["upper_bound_bytes"], rescanned);
    }
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_reflink_kept_only_by_snapshots_of_two_shares_bounds_the_set() {
    let state = state_dir("cross-share");
    let mnt = fixture();
    let (one, two) = (mnt.join("crossone"), mnt.join("crosstwo"));
    for share in [&one, &two] {
        btrfs(&["-q", "subvolume", "create", text(share)]);
    }
    let anchor = one.join("anchor");
    std::fs::write(&anchor, b"").unwrap();
    write_mib(&mnt, &one.join("copy"), 2);
    let out = Command::new("cp")
        .args([
            "--reflink=always",
            text(&one.join("copy")),
            text(&two.join("copy")),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let (snap_one, _) = snapshot(&mnt, &one, "crossone-s1");
    let (snap_two, _) = snapshot(&mnt, &two, "crosstwo-s1");
    remove_committed(&mnt, &one.join("copy"));
    remove_committed(&mnt, &two.join("copy"));
    volume_scan(&mnt, &state, &["--snapshots", "all"]);

    let (simulation, rescanned) = report_and_rescan(&state, &[&snap_one, &snap_two], &anchor);
    let before = data_used(&mnt);
    delete_subvolumes(&mnt, &[&snap_one, &snap_two]);
    let after = data_used(&mnt);
    delete_subvolumes(&mnt, &[&one, &two]);
    let _ = std::fs::remove_dir_all(&state);

    assert_eq!(rescanned, 2 * MIB);
    assert_eq!(before - after, rescanned, "df used {before} -> {after}");
    assert_eq!(simulation["freed_bytes"], 0, "{simulation:#}");
    assert_eq!(simulation["exact"], false, "{simulation:#}");
    assert_eq!(
        simulation["inexact_reasons"],
        serde_json::json!(["cross_share_sharing"])
    );
    let upper = simulation["upper_bound_bytes"].as_u64().unwrap();
    assert!(upper >= rescanned, "{simulation:#}");
    assert_eq!(simulation["exact_command"], serde_json::Value::Null);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn deleting_the_snapshot_frees_the_files_listed_as_held() {
    const SHARE: &str = "heldshare";
    let state = state_dir("held-files");
    let mnt = fixture();
    let share = mnt.join(SHARE);
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    std::fs::create_dir(share.join("dir")).unwrap();
    write_entropy(&share.join("dir/gone"), 3, 0x6b1d_30e2_94ac_5f07);
    write_entropy(&share.join("rewritten"), 2, 0x13f7_a85c_02d9_e64b);
    write_mib(&mnt, &share.join("kept"), 1);
    let (snap, id) = snapshot(&mnt, &share, "heldshare-s1");
    remove_committed(&mnt, &share.join("dir/gone"));
    // Same inode, new extents: the old ones stay in the snapshot only.
    write_entropy(&share.join("rewritten"), 2, 0x4c02_9be7_d153_a86e);
    btrfs(&["filesystem", "sync", text(&mnt)]);

    let s = ["--json", "--quiet", "--state-dir", text(&state)];
    let mut args = vec!["scan", text(&share), "--no-cache", "--snapshots", "all"];
    args.extend(s);
    let (code, scan) = run_json(&args);
    assert_eq!(code, Some(0), "{scan:#}");
    let mut args = vec!["snapshots", text(&share)];
    args.extend(s);
    let (code, doc) = run_json(&args);
    assert_eq!(code, Some(0), "{doc:#}");
    volume_scan(&mnt, &state, &["--snapshots", "all"]);
    let (_, why) = why_not_freed(&mnt, &state, &[]);

    let before = data_used(&mnt);
    delete_subvolumes(&mnt, &[&snap]);
    let after = data_used(&mnt);
    delete_subvolumes(&mnt, &[&share]);
    let _ = std::fs::remove_dir_all(&state);

    let answer = &doc["results"][0]["shares"][0];
    let held = &answer["held_files"];
    assert_eq!(held["complete"], true, "{doc:#}");
    assert_eq!(held["count"], 2, "{doc:#}");
    let files = held["files"].as_array().unwrap();
    let live = |name: &str| format!("{}/{SHARE}/{name}", text(&mnt));
    assert_eq!(files[0]["path"]["path"], "dir/gone", "{doc:#}");
    assert_eq!(files[0]["live_path"]["path"], live("dir/gone"), "{doc:#}");
    assert_eq!(files[0]["in_live"], false, "{doc:#}");
    assert_eq!(files[0]["bytes"], 3 * MIB, "{doc:#}");
    assert_eq!(files[0]["snapshot_id"], id, "{doc:#}");
    assert_eq!(files[1]["path"]["path"], "rewritten", "{doc:#}");
    assert_eq!(files[1]["in_live"], true, "{doc:#}");
    assert_eq!(files[1]["bytes"], 2 * MIB, "{doc:#}");
    let listed: u64 = files.iter().map(|f| f["bytes"].as_u64().unwrap()).sum();
    assert_eq!(answer["held_only_by_snapshots_bytes"], listed, "{doc:#}");
    let row = why_item(&why_row(&why, "held_by_snapshots"), SHARE);
    assert_eq!(row["files"], *held, "{why:#}");
    assert_eq!(before - after, listed, "df used {before} -> {after}");
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn qgroup_exclusive_bytes_cross_check_the_snapshot_census() {
    let state = state_dir("qgroup");
    let mnt = fixture();
    let share = mnt.join("quotashare");
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    write_mib(&mnt, &share.join("old"), 5);
    write_mib(&mnt, &share.join("kept"), 2);
    let (snap, id) = snapshot(&mnt, &share, "quotashare-s1");
    remove_committed(&mnt, &share.join("old"));
    // Test-only: quotas on the loop fixture, never on a NAS volume.
    btrfs(&["quota", "enable", text(&mnt)]);
    btrfs(&["quota", "rescan", "-w", text(&mnt)]);
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let s = [
        "--json",
        "--quiet",
        "--no-cache",
        "--state-dir",
        text(&state),
    ];
    let mut scan = vec!["scan", text(&share), "--snapshots"];
    scan.extend(s);
    let (code, scanned) = run_json(&scan);
    let mut ask = vec!["snapshots", text(&share)];
    ask.extend(&s[..2]);
    ask.extend(&s[3..]);
    let (answered, doc) = run_json(&ask);
    btrfs(&["quota", "disable", text(&mnt)]);
    delete_subvolumes(&mnt, &[&snap, &share]);
    let _ = std::fs::remove_dir_all(&state);
    assert_eq!(code, Some(0), "{scanned:#}");
    assert_eq!(answered, Some(0), "{doc:#}");
    let row = snapshot_row(&doc, id);
    let exclusive = row["exclusive_bytes"].as_u64().unwrap();
    assert_eq!(exclusive, 5 * MIB, "{doc:#}");
    // qgroup excl also counts the snapshot's own tree blocks.
    let qgroup = row["qgroup_exclusive_bytes"]
        .as_u64()
        .expect("qgroup shown");
    assert!(
        (exclusive..exclusive + 256 * 1024).contains(&qgroup),
        "qgroup {qgroup} vs census {exclusive}: {doc:#}"
    );
}

/// Level-0 qgroup exclusive bytes of subvolume `id`.
fn qgroup_exclusive(mnt: &Path, id: u64) -> u64 {
    let out = Command::new("btrfs")
        .args(["qgroup", "show", "--raw", text(mnt)])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let listing = String::from_utf8(out.stdout).unwrap();
    let qgroup = format!("0/{id}");
    let line = listing
        .lines()
        .find(|l| l.split_whitespace().next() == Some(qgroup.as_str()))
        .unwrap_or_else(|| panic!("no qgroup {qgroup} in {listing}"));
    line.split_whitespace().nth(2).unwrap().parse().unwrap()
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn deleting_a_live_file_with_its_snapshot_frees_what_qgroups_and_df_show() {
    let state = state_dir("mixed");
    let mnt = fixture();
    let share = mnt.join("mixshare");
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    // s1 holds old alone and shares file and kept with the live share.
    write_mib(&mnt, &share.join("old"), 3);
    write_mib(&mnt, &share.join("file"), 2);
    write_mib(&mnt, &share.join("kept"), 1);
    let (s1, id) = snapshot(&mnt, &share, "mixshare-s1");
    remove_committed(&mnt, &share.join("old"));
    let file = share.join("file");
    let args = [
        "simulate-delete",
        text(&s1),
        text(&file),
        "--no-cache",
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ];
    let (code, doc) = run_json(&args);
    let result = &doc["results"][0];
    let with = &result["with_snapshots"];

    let before = data_used(&mnt);
    remove_committed(&mnt, &file);
    // Test-only: quotas on the loop fixture, never on a NAS volume.
    btrfs(&["quota", "enable", text(&mnt)]);
    btrfs(&["quota", "rescan", "-w", text(&mnt)]);
    let qgroup = qgroup_exclusive(&mnt, id);
    btrfs(&["quota", "disable", text(&mnt)]);
    delete_subvolumes(&mnt, &[&s1]);
    let after = data_used(&mnt);
    delete_subvolumes(&mnt, &[&share]);
    let _ = std::fs::remove_dir_all(&state);

    assert_eq!(code, Some(0), "{doc:#}");
    // Alone, the live file frees nothing: the snapshot holds it.
    assert_eq!(result["combined"]["reclaimable"], 0, "{doc:#}");
    assert_eq!(with["snapshot_ids"], serde_json::json!([id]), "{doc:#}");
    let freed = with["freed_bytes"].as_u64().unwrap();
    assert_eq!(freed, 5 * MIB, "{doc:#}");
    assert_eq!(with["unknown_bytes"], 0, "{doc:#}");
    // With the file gone, qgroup excl of s1 is what the pair frees, plus
    // the snapshot's own tree blocks.
    assert!(
        (freed..freed + 256 * 1024).contains(&qgroup),
        "qgroup {qgroup} vs census {freed}: {doc:#}"
    );
    assert_eq!(before - after, freed, "df used {before} -> {after}");
}

/// The subvolume id of the subvolume holding `path`.
fn subvolume_id(path: &Path) -> u64 {
    let out = Command::new("btrfs")
        .args(["inspect-internal", "rootid", text(path)])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Runs `body` while another thread keeps committing writes to `dir`,
/// so its subvolume's generation advances throughout.
fn while_writing<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let file = dir.join("churn");
    let writer = {
        let stop = stop.clone();
        let file = file.clone();
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                std::fs::write(&file, n.to_le_bytes()).unwrap();
                let synced = Command::new("sync").arg("-f").arg(&file).status();
                assert!(synced.unwrap().success());
                n += 1;
            }
        })
    };
    let result = body();
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    std::fs::remove_file(&file).unwrap();
    // Commit the removal: later tests read committed metadata only.
    assert!(
        Command::new("sync")
            .arg("-f")
            .arg(dir)
            .status()
            .unwrap()
            .success()
    );
    result
}

/// Whether `reasons` name a generation change of subvolume `id`.
fn names_generation_change(reasons: &serde_json::Value, id: u64) -> bool {
    reasons.as_array().unwrap().iter().any(|r| {
        r["kind"] == "subvolume-generation-changed"
            && r["subvolume_id"] == id
            && r["generation_after"].as_u64() > r["generation_before"].as_u64()
    })
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_scan_names_the_subvolume_written_during_it() {
    let state = state_dir("written");
    let data = fixture().join("data");
    let nested = data.join("nested");
    let id = subvolume_id(&nested);
    let scan = [
        "scan",
        text(&data),
        "--json",
        "--quiet",
        "--no-cache",
        "--state-dir",
        text(&state),
    ];
    // A scan window can fall between two commits: allow a few tries.
    let fs = while_writing(&nested, || {
        (0..5)
            .map(|_| run_json(&scan).1["filesystems"][0].clone())
            .find(|fs| fs["consistency"] == "changed-during-scan")
    })
    .expect("no scan saw the writes");
    assert!(names_generation_change(&fs["change_reasons"], id), "{fs:#}");
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_resumed_job_and_its_stored_report_keep_the_change_reasons() {
    let state = state_dir("resume-reasons");
    let data = fixture().join("data");
    let nested = data.join("nested");
    let id = subvolume_id(&nested);
    let s = ["--state-dir", text(&state)];
    let (fs, top) = while_writing(&nested, || {
        (0..5)
            .map(|_| {
                let pause = [
                    "scan",
                    text(&data),
                    "--json",
                    "--quiet",
                    "--max-runtime",
                    "0s",
                ];
                let (code, paused) = run_json(&[&pause[..], &s[..]].concat());
                assert_eq!(code, Some(10), "{paused:#}");
                let id = paused["job_id"].as_str().unwrap().to_string();
                let resume = ["resume", &id, "--max-runtime", "1h", "--json", "--quiet"];
                let (code, resumed) = run_json(&[&resume[..], &s[..]].concat());
                assert_eq!(code, Some(0), "{resumed:#}");
                let top = ["top", text(&data), "--json", "--no-scan"];
                let (_, top) = run_json(&[&top[..], &s[..]].concat());
                (resumed["filesystems"][0].clone(), top)
            })
            .find(|(fs, _)| fs["consistency"] == "changed-during-scan")
    })
    .expect("no resumed scan saw the writes");
    assert!(names_generation_change(&fs["change_reasons"], id), "{fs:#}");
    let report = &top["results"][0]["report"];
    assert_eq!(report["consistency"], "changed-during-scan", "{top:#}");
    assert_eq!(report["change_reasons"], fs["change_reasons"], "{top:#}");
    let _ = std::fs::remove_dir_all(&state);
}

/// The `state_dir` check of `doctor` on `target`.
fn doctor_state_check(target: &Path, state: &Path) -> serde_json::Value {
    doctor_check(
        &["doctor", text(target), "--state-dir", text(state)],
        "state_dir",
    )
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn doctor_warns_when_the_state_dir_is_on_the_target_filesystem() {
    let data = fixture().join("data");
    let inside = data.join("not-created/state");
    let check = doctor_state_check(&data, &inside);
    assert_eq!(check["status"], "warn", "{check:#}");
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .contains("inside selected subvolume"),
        "{check:#}"
    );
    assert!(
        !data.join("not-created").exists(),
        "doctor created the state dir"
    );
    let elsewhere = state_dir("doctor");
    assert_eq!(doctor_state_check(&data, &elsewhere)["status"], "pass");
}

/// The warning codes of a scan's first filesystem, and its stderr.
fn scan_warnings(target: &Path, state: &Path) -> (Vec<String>, String) {
    let out = synoreclaim(&[
        "scan",
        text(target),
        "--json",
        "--no-cache",
        "--min-free",
        "16MiB",
        "--state-dir",
        text(state),
    ]);
    let codes = json(&out)["filesystems"][0]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap().to_string())
        .collect();
    (codes, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Removes a state directory made on the fixture and commits that, so
/// later tests read the fixture as built.
fn remove_fixture_state(state: &Path) {
    std::fs::remove_dir_all(state).unwrap();
    assert!(
        Command::new("sync")
            .arg("-f")
            .arg(fixture())
            .status()
            .unwrap()
            .success()
    );
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_scan_warns_when_its_state_dir_is_on_the_scanned_filesystem() {
    let nested = fixture().join("data/nested");
    let inside = nested.join("state");
    let (codes, stderr) = scan_warnings(&nested, &inside);
    assert!(
        codes.contains(&"state_dir_in_selection".to_string()),
        "{codes:?}"
    );
    assert!(
        stderr.contains("warning: the state directory is inside selected subvolume"),
        "{stderr}"
    );
    remove_fixture_state(&inside);
    let beside = fixture().join("state");
    let (codes, _) = scan_warnings(&nested, &beside);
    assert!(
        codes.contains(&"state_dir_on_scanned_filesystem".to_string()),
        "{codes:?}"
    );
    remove_fixture_state(&beside);
    let elsewhere = state_dir("placement");
    let (codes, stderr) = scan_warnings(&nested, &elsewhere);
    assert!(
        !codes.iter().any(|c| c.starts_with("state_dir")),
        "{codes:?}"
    );
    assert!(!stderr.contains("state directory"), "{stderr}");
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_changed_scan_names_the_subvolume_holding_its_state_dir() {
    let nested = fixture().join("data/nested");
    let inside = nested.join("state");
    // A scan window can fall between two commits: allow a few tries.
    let codes = while_writing(&nested, || {
        (0..5)
            .map(|_| scan_warnings(&nested, &inside).0)
            .find(|codes| codes.contains(&"state_dir_changed".to_string()))
    });
    remove_fixture_state(&inside);
    assert!(codes.is_some(), "no scan named the state subvolume");
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn targets_are_deduplicated_and_unsupported_ones_reported() {
    let state = state_dir("targets");
    let data = fixture().join("data");
    let list = state.with_extension("list");
    let mut bytes = data.join("docs").as_os_str().as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(data.as_os_str().as_bytes());
    std::fs::write(&list, bytes).unwrap();
    let (code, doc) = run_json(&[
        "scan",
        "/proc",
        "--from-file",
        text(&list),
        "--null",
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(3), "{doc:#}");
    assert_eq!(doc["completion"], "partial");
    assert_eq!(doc["errors"][0]["code"], "not_btrfs");
    let fs = &doc["filesystems"][0];
    assert_eq!(fs["targets"].as_array().unwrap().len(), 1, "{fs:#}");
    assert_eq!(fs["covered"][0]["covered_by"]["path"], text(&data));
    let _ = std::fs::remove_file(&list);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn implicit_scans_only_for_path_targets() {
    let state = state_dir("implicit");
    let mnt = fixture();
    let (code, overview) = run_json(&[
        "overview",
        text(&mnt),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(1), "{overview:#}");
    assert_eq!(overview["errors"][0]["code"], "no_report");
    assert_eq!(overview["scan_hints"][0]["path"], text(&mnt));

    let (code, volume) = run_json(&[
        "top",
        text(&mnt),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(1), "{volume:#}");
    assert_eq!(volume["errors"][0]["code"], "no_report");
    let docs = mnt.join("data/docs");
    let (code, path) = run_json(&[
        "top",
        text(&docs),
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert_eq!(code, Some(0), "{path:#}");
    assert!(path["job_id"].is_string());
    assert_eq!(path["scans"][0]["completion"], "complete");
    assert!(!path["results"][0]["entries"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn live_outputs_match_the_published_schemas() {
    let schemas = support::Schemas::load();
    let state = state_dir("schema");
    let mnt = fixture();
    let data = mnt.join("data");
    let copy = data.join("extents/reflink-copy.bin");
    let s = ["--json", "--quiet", "--state-dir", text(&state)];
    let src = data.join("extents/reflink-src.bin");
    let snap = mnt.join("snap");
    let runs: [Vec<&str>; 15] = [
        vec!["scan", text(&mnt), "--scope", "volume"],
        vec!["overview", text(&mnt), "--no-scan"],
        vec!["why-not-freed", text(&mnt), "--processes", "--since", "1d"],
        vec!["scan", "/proc", text(&data)],
        vec!["top", text(&data), "--kind", "files", "--include-system"],
        vec!["inspect", text(&copy), "--explain"],
        vec!["simulate-delete", text(&src), text(&copy), "--explain"],
        vec!["shared", text(&data), "--kind", "files"],
        vec!["volumes", "--json", "--state-dir", text(&state)],
        vec!["cache", "status", text(&mnt)],
        vec!["jobs", "--json", "--state-dir", text(&state)],
        vec!["history", "--json", "--state-dir", text(&state)],
        vec!["scan", text(&data), "--snapshots"],
        vec!["snapshots", text(&data), "--simulate-delete", "all"],
        vec!["simulate-delete", text(&snap), "--no-scan"],
    ];
    for mut args in runs {
        if !["volumes", "jobs", "history"].contains(&args[0]) {
            args.extend(s);
        }
        let (_, doc) = run_json(&args);
        if args[0] == "volumes" {
            assert_eq!(doc["system_partition"]["mount"], "/", "{doc}");
        }
        if args[0] == "overview" {
            let data = &doc["results"][0]["reconciliation"]["data"];
            let residual = data["residual_bytes"].as_i64().unwrap();
            let tolerance = data["tolerance_bytes"].as_u64().unwrap();
            assert!(
                residual.unsigned_abs() <= tolerance,
                "fixture data reconciliation exceeds tolerance: {data}"
            );
        }
        let errors = schemas.validate(args[0], &doc);
        assert!(errors.is_empty(), "{args:?}:\n{}", errors.join("\n"));
    }
    let _ = std::fs::remove_dir_all(&state);
}

struct OverviewOracleCleanup {
    subvolume: PathBuf,
    state: PathBuf,
}

impl Drop for OverviewOracleCleanup {
    fn drop(&mut self) {
        let _ = Command::new("btrfs")
            .args(["subvolume", "delete", text(&self.subvolume)])
            .status();
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn overview_reclaimable_amounts_match_delete_and_measure() {
    const SHARE: &str = "overview-oracle-share";
    const MIB: u64 = 1 << 20;
    const SIZE_MIB: usize = 16;

    let mnt = fixture();
    let share = mnt.join(SHARE);
    assert!(
        !share.exists(),
        "fixture subvolume already exists: {share:?}"
    );
    btrfs(&["subvolume", "create", text(&share)]);
    let state = state_dir("overview-delete-oracle");
    let cleanup = OverviewOracleCleanup {
        subvolume: share.clone(),
        state: state.clone(),
    };

    let recycle = share.join("#recycle");
    std::fs::create_dir(&recycle).unwrap();
    let recycle_file = recycle.join("payload.bin");
    let live_file = share.join("live.bin");
    write_entropy(&recycle_file, SIZE_MIB, 0x7a31_d4c2_9915_0bef);
    write_entropy(&live_file, SIZE_MIB, 0xf104_a218_6c3d_8257);
    // Commit extent references and SPACE_INFO before comparing the report.
    btrfs(&["filesystem", "sync", text(&mnt)]);

    let (code, scan) = run_json(&[
        "scan",
        text(&mnt),
        "--scope",
        "volume",
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
        "--min-free",
        "16MiB",
    ]);
    assert!(matches!(code, Some(0 | 3)), "{scan:#}");
    let (code, doc) = run_json(&[
        "overview",
        text(&mnt),
        "--json",
        "--quiet",
        "--no-scan",
        "--state-dir",
        text(&state),
    ]);
    assert!(matches!(code, Some(0 | 3)), "{doc:#}");

    let recycle_bytes = overview_item_reclaimable(&doc, "recycle_bin", SHARE);
    let live_bytes = overview_item_reclaimable(&doc, "share_live", SHARE);
    assert_eq!(recycle_bytes, (SIZE_MIB as u64) * MIB, "{doc:#}");
    assert_eq!(live_bytes, (SIZE_MIB as u64) * MIB, "{doc:#}");
    assert_eq!(
        overview_part_reclaimable(&doc, "recycle_bin"),
        recycle_bytes
    );
    assert_eq!(overview_part_reclaimable(&doc, "share_live"), live_bytes);

    let actions = doc["results"][0]["actions"].as_array().unwrap();
    let recycle_action = actions
        .iter()
        .find(|action| action["kind"] == "recycle" && action["share"] == SHARE)
        .unwrap();
    let cleanup_action = actions
        .iter()
        .find(|action| action["kind"] == "cleanup" && action["path"]["path"] == text(&live_file))
        .unwrap();
    assert_eq!(
        recycle_action["reclaimable_bytes"].as_u64(),
        Some(recycle_bytes)
    );
    assert_eq!(
        cleanup_action["reclaimable_bytes"].as_u64(),
        Some(live_bytes)
    );

    let before_recycle = btrfs_data_used_after_sync(&mnt);
    std::fs::remove_file(&recycle_file).unwrap();
    let after_recycle = btrfs_data_used_after_sync(&mnt);
    assert_eq!(before_recycle - after_recycle, recycle_bytes);

    std::fs::remove_file(&live_file).unwrap();
    let after_live = btrfs_data_used_after_sync(&mnt);
    assert_eq!(after_recycle - after_live, live_bytes);

    drop(cleanup);
    btrfs(&["subvolume", "sync", text(&mnt)]);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn watch_free_observes_data_and_free_space_after_a_file_delete() {
    let mount = fixture();
    let file = mount.join(format!("watch-free-oracle-{}.bin", std::process::id()));
    write_entropy(&file, 32, 0x4e5b_7c13_89a2_d60f);
    btrfs(&["filesystem", "sync", text(&mount)]);

    let child = Command::new(env!("CARGO_BIN_EXE_synoreclaim"))
        .args([
            "watch-free",
            text(&mount),
            "--duration",
            "8s",
            "--interval",
            "2s",
            "--json",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    std::fs::remove_file(&file).unwrap();
    btrfs(&["filesystem", "sync", text(&mount)]);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        document["samples"].as_array().unwrap().len() >= 2,
        "{document:#}"
    );
    assert!(
        document["data_used_delta_bytes"].as_i64().unwrap() < 0,
        "{document:#}"
    );
    assert!(
        document["free_delta_bytes"].as_i64().unwrap() > 0,
        "{document:#}"
    );
    assert_eq!(document["concurrent_writers_may_affect_delta"], true);
    assert!(
        document["samples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|sample| { sample["pending_deletion_roots"].as_u64().is_some() })
    );
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn overview_attributes_fully_referenced_extents_behind_shared_leaves() {
    const SHARE: &str = "overview-shared-leaf-share";
    const SIZE_MIB: usize = 16;

    let mnt = fixture();
    let share = mnt.join(SHARE);
    let state = state_dir("overview-shared-leaf");
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    let file = share.join("payload.bin");
    write_entropy(&file, SIZE_MIB, 0x51d3_0c6e_a4f2_9b17);
    // Enough inodes for a tree above one leaf, so the snapshot shares
    // the payload's leaf instead of copying it with the root.
    for n in 0..600 {
        std::fs::File::create(share.join(format!("empty-{n:03}"))).unwrap();
    }
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let (snap, _) = snapshot(&mnt, &share, "overview-shared-leaf-snap");
    // Copy-on-write of the payload's shared leaf turns its data
    // references into shared (full) backrefs.
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
    std::fs::set_permissions(&file, permissions).unwrap();
    btrfs(&["filesystem", "sync", text(&mnt)]);

    let (code, scan) = run_json(&[
        "scan",
        text(&mnt),
        "--scope",
        "volume",
        "--json",
        "--quiet",
        "--no-cache",
        "--state-dir",
        text(&state),
        "--min-free",
        "16MiB",
    ]);
    assert!(matches!(code, Some(0 | 3)), "{scan:#}");
    let (code, doc) = run_json(&[
        "overview",
        text(&mnt),
        "--json",
        "--quiet",
        "--no-scan",
        "--state-dir",
        text(&state),
    ]);
    assert!(matches!(code, Some(0 | 3)), "{doc:#}");
    let before = data_used(&mnt);
    delete_subvolumes(&mnt, &[&snap, &share]);
    let freed = before - data_used(&mnt);
    let _ = std::fs::remove_dir_all(&state);

    let parts = doc["results"][0]["parts"].as_array().unwrap();
    let share_bytes = parts
        .iter()
        .filter(|part| part["kind"] == "share_live")
        .flat_map(|part| part["items"].as_array().unwrap())
        .find(|item| item["share"] == SHARE)
        .unwrap_or_else(|| panic!("no share_live item for {SHARE}: {doc:#}"))["bytes"]
        .as_u64()
        .unwrap();
    assert_eq!(share_bytes, SIZE_MIB as u64 * MIB, "{doc:#}");
    assert_eq!(freed, share_bytes);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn overview_measures_pinned_bytes_behind_unscanned_shared_leaves() {
    const SHARE: &str = "overview-pinned-leaf-share";
    let mnt = fixture();
    let share = mnt.join(SHARE);
    let state = state_dir("overview-pinned-leaf");
    let baseline = volume_overview(&mnt, &state);
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    // Enough inodes for a tree above one leaf, so the snapshot shares
    // the payload's leaf instead of copying it with the root.
    for n in 0..600 {
        std::fs::File::create(share.join(format!("empty-{n:03}"))).unwrap();
    }
    let file = share.join("payload.bin");
    write_entropy(&file, 4, 0x6b1e_93d0_27ac_f451);
    btrfs(&["filesystem", "sync", text(&mnt)]);
    // Rewriting the first MiB leaves it pinned in the payload's extent.
    let mut payload = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
    payload.write_all(&vec![0x5a; MIB as usize]).unwrap();
    drop(payload);
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let (snap, _) = snapshot(&mnt, &share, "overview-pinned-leaf-snap");
    // Copy-on-write of the shared leaf turns the snapshot's references
    // into shared backrefs; removing the live copy leaves only those.
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
    std::fs::set_permissions(&file, permissions).unwrap();
    btrfs(&["filesystem", "sync", text(&mnt)]);
    remove_committed(&mnt, &file);

    let doc = volume_overview(&mnt, &state);
    let fast = volume_overview_with(&mnt, &state, &["--profile", "fast"]);
    let before = btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&snap]);
    let freed = before - btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&share]);
    let _ = std::fs::remove_dir_all(&state);

    let part = |doc: &serde_json::Value, kind: &str| overview_part_bytes(doc, kind);
    let snapshot_only = overview_item_bytes(&doc, "snapshot_only", SHARE);
    let pinned = part(&doc, "pinned_unreferenced") - part(&baseline, "pinned_unreferenced");
    assert_eq!(part(&doc, "unknown"), part(&baseline, "unknown"), "{doc:#}");
    assert_eq!(snapshot_only, 4 * MIB, "{doc:#}");
    assert_eq!(pinned, MIB, "{doc:#}");
    assert_eq!(freed, snapshot_only + pinned);
    let parts = |doc: &serde_json::Value| doc["results"][0]["parts"].clone();
    assert_eq!(parts(&fast), parts(&doc), "{fast:#}");
}

/// A volume scan of `mnt`, then its overview.
fn volume_overview(mnt: &Path, state: &Path) -> serde_json::Value {
    volume_overview_with(mnt, state, &[])
}

/// A volume scan of `mnt` with `extra` arguments, then its overview.
fn volume_overview_with(mnt: &Path, state: &Path, extra: &[&str]) -> serde_json::Value {
    volume_scan(mnt, state, extra);
    let (code, doc) = run_json(&[
        "overview",
        text(mnt),
        "--json",
        "--quiet",
        "--no-scan",
        "--state-dir",
        text(state),
    ]);
    assert!(matches!(code, Some(0 | 3)), "{doc:#}");
    doc
}

/// The bytes of overview part `kind`.
fn overview_part_bytes(doc: &serde_json::Value, kind: &str) -> u64 {
    doc["results"][0]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} part: {doc:#}"))["bytes"]
        .as_u64()
        .unwrap()
}

/// The bytes of `share` in overview part `kind`.
fn overview_item_bytes(doc: &serde_json::Value, kind: &str, share: &str) -> u64 {
    doc["results"][0]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|part| part["kind"] == kind)
        .flat_map(|part| part["items"].as_array().unwrap())
        .find(|item| item["share"] == share)
        .unwrap_or_else(|| panic!("no {kind} item for {share}: {doc:#}"))["bytes"]
        .as_u64()
        .unwrap()
}

/// The `why-not-freed` row with `code`.
fn why_row(doc: &serde_json::Value, code: &str) -> serde_json::Value {
    doc["results"][0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"] == code)
        .unwrap_or_else(|| panic!("no {code} row in {doc:#}"))
        .clone()
}

/// The item of `share` in a row.
fn why_item(row: &serde_json::Value, share: &str) -> serde_json::Value {
    row["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["share"] == share)
        .unwrap_or_else(|| panic!("no {share} in {row:#}"))
        .clone()
}

fn volume_scan(mnt: &Path, state: &Path, extra: &[&str]) {
    let mut args = vec![
        "scan",
        text(mnt),
        "--scope",
        "volume",
        "--json",
        "--quiet",
        "--no-cache",
        "--state-dir",
        text(state),
        "--min-free",
        "16MiB",
    ];
    args.extend(extra);
    let (code, doc) = run_json(&args);
    assert!(matches!(code, Some(0 | 3)), "{doc:#}");
}

fn why_not_freed(mnt: &Path, state: &Path, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let mut args = vec![
        "why-not-freed",
        text(mnt),
        "--json",
        "--quiet",
        "--state-dir",
        text(state),
    ];
    args.extend(extra);
    run_json(&args)
}

/// Bytes of the unresolved part `code` of a why-not-freed document.
fn unresolved_part(doc: &serde_json::Value, code: &str) -> u64 {
    why_row(doc, "unresolved_references")["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"] == code)
        .map_or(0, |r| r["bytes"].as_u64().unwrap())
}

/// The unresolved bytes the rest of the fixture leaves, before a test
/// adds its own.
fn unresolved_baseline(mnt: &Path, state: &Path) -> serde_json::Value {
    volume_scan(mnt, state, &[]);
    why_not_freed(mnt, state, &[]).1
}

/// A process holding `path` open on its stdin, killed on drop.
struct Holder(std::process::Child);

impl Holder {
    fn open(path: &Path) -> Self {
        let file = std::fs::File::open(path).unwrap();
        Holder(
            Command::new("sleep")
                .arg("600")
                .stdin(file)
                .spawn()
                .unwrap(),
        )
    }

    fn release(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        self.release();
    }
}

/// B07/B22: a share created and scanned on its own after the volume scan is
/// listed by the volume's Recycle Bin answer from its own newer report, with
/// every one of its oldest items, not only the oldest.
#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn a_newer_share_scan_answers_for_its_share_in_the_volume_recycle_list() {
    const SHARE: &str = "recycle-newer-share";
    let mnt = fixture();
    let share = mnt.join(SHARE);
    let state = state_dir("recycle-newer-share");
    volume_scan(&mnt, &state, &[]);
    // Report times are whole seconds: the share scan must finish later.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    let bin = share.join("#recycle");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let kept = share.join("kept.bin");
    write_entropy(&kept, 2, 0x6b1e_93d2_04c7_5a18);
    std::fs::rename(&kept, bin.join("kept.bin")).unwrap();
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let (code, scan) = run_json(&[
        "scan",
        text(&share),
        "--json",
        "--quiet",
        "--no-cache",
        "--state-dir",
        text(&state),
        "--min-free",
        "16MiB",
    ]);
    let share_job = scan["job_id"].clone();
    let (recycle_code, recycle) = run_json(&[
        "recycle",
        text(&mnt),
        "--oldest",
        "200",
        "--no-scan",
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    delete_subvolumes(&mnt, &[&share]);
    let _ = std::fs::remove_dir_all(&state);

    assert!(matches!(code, Some(0 | 3)), "{scan:#}");
    assert!(matches!(recycle_code, Some(0 | 3)), "{recycle:#}");
    let row = recycle["shares"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["share"]["path"] == SHARE)
        .unwrap_or_else(|| panic!("the newer share is missing: {recycle:#}"));
    assert_eq!(row["items"], 2, "{recycle:#}");
    assert_eq!(row["report_info"]["job_id"], share_job, "{recycle:#}");
    assert_ne!(recycle["report_info"]["job_id"], share_job, "{recycle:#}");
    let oldest: Vec<_> = row["oldest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            item["path"]["path"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(oldest, ["desktop.ini", "kept.bin"], "{recycle:#}");
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn why_not_freed_amounts_match_delete_and_measure() {
    const SHARE: &str = "wnf-share";
    let mnt = fixture();
    let share = mnt.join(SHARE);
    let state = state_dir("why-not-freed");
    let baseline = unresolved_baseline(&mnt, &state);
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    write_entropy(&share.join("gone.bin"), 8, 0x51c3_77a0_19de_4b25);
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let (snap, _) = snapshot(&mnt, &share, "wnf-share-snap");
    remove_committed(&mnt, &share.join("gone.bin"));
    let recycle_dir = share.join("#recycle");
    std::fs::create_dir(&recycle_dir).unwrap();
    let recycled = share.join("#recycle/old.bin");
    let before_recycle = share.join("old.bin");
    write_entropy(&before_recycle, 4, 0x2e91_0c4d_a7b3_6f18);
    std::fs::rename(&before_recycle, &recycled).unwrap();
    let open = share.join("open.bin");
    write_entropy(&open, 2, 0x9d04_b6e1_35a2_c87f);
    let mut holder = Holder::open(&open);
    let holder_pid = holder.0.id();
    remove_committed(&mnt, &open);

    volume_scan(&mnt, &state, &[]);
    let (code, doc) = why_not_freed(&mnt, &state, &["--processes"]);
    let (since_code, since_doc) = why_not_freed(&mnt, &state, &["--since", "24h"]);
    assert!(matches!(since_code, Some(0 | 3)), "{since_doc:#}");
    let baseline_recycle_bytes = why_item(&why_row(&baseline, "recycle_bin"), "data")["bytes"]
        .as_u64()
        .unwrap();
    assert_eq!(
        why_row(&since_doc, "recycle_bin")["since_bytes"],
        baseline_recycle_bytes + 4 * MIB,
        "{since_doc:#}"
    );
    assert_eq!(
        why_row(&since_doc, "pending_subvolume_cleanup")["since_not_measured"],
        false,
        "{since_doc:#}"
    );
    assert!(
        !since_doc["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "since_not_measured")
    );
    let (recycle_code, recycle_doc) = run_json(&[
        "recycle",
        text(&mnt),
        "--oldest",
        "10",
        "--no-scan",
        "--json",
        "--quiet",
        "--state-dir",
        text(&state),
    ]);
    assert!(matches!(recycle_code, Some(0 | 3)), "{recycle_doc:#}");
    let recycle_share = recycle_doc["shares"]
        .as_array()
        .unwrap()
        .iter()
        .find(|share| share["share"]["path"] == SHARE)
        .unwrap_or_else(|| panic!("missing share in {recycle_doc:#}"));
    let oldest = &recycle_share["oldest"][0];
    assert_eq!(oldest["path"]["path"], text(&recycled));
    assert!(oldest["ctime"].is_string(), "{recycle_doc:#}");
    let snapshots = why_row(&doc, "held_by_snapshots");
    let recycle = why_row(&doc, "recycle_bin");
    let open_row = why_row(&doc, "open_unlinked");
    let holders = open_row["processes"]["holders"].as_array().unwrap().clone();

    let before = btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&snap]);
    let after_snapshot = btrfs_data_used_after_sync(&mnt);
    std::fs::remove_file(&recycled).unwrap();
    let after_recycle = btrfs_data_used_after_sync(&mnt);
    holder.release();
    let after_close = btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&share]);
    let _ = std::fs::remove_dir_all(&state);

    // The bytes only the snapshot holds are measured from its tree, but
    // those of the open file (no name to index) stay unresolved, so the
    // answer is partial.
    assert_eq!(code, Some(3), "{doc:#}");
    assert_eq!(why_row(&doc, "pending_subvolume_cleanup")["bytes"], 0);
    assert_eq!(why_row(&doc, "commit_pending")["interval_s"], 30);
    let unresolved = why_row(&doc, "unresolved_references");
    let part = |code: &str| unresolved_part(&doc, code) - unresolved_part(&baseline, code);
    assert_eq!(unresolved["unknown"], true, "{doc:#}");
    assert_eq!(snapshots["bytes"], 8 * MIB, "{doc:#}");
    assert_eq!(snapshots["lower_bound"], true, "{doc:#}");
    assert_eq!(part("pinned_ranges_unknown"), 0, "{doc:#}");
    assert_eq!(before - after_snapshot, 8 * MIB, "snapshot delete");
    let bin = why_item(&recycle, SHARE);
    assert_eq!(bin["reclaimable_bytes"], 4 * MIB, "{doc:#}");
    assert_eq!(after_snapshot - after_recycle, 4 * MIB, "recycle delete");
    assert_eq!(open_row["report_orphan_bytes"], 0, "{doc:#}");
    assert_eq!(part("direct_owner_unresolved"), 2 * MIB, "{doc:#}");
    assert_eq!(open_row["processes"]["bytes_estimate"], 2 * MIB, "{doc:#}");
    assert!(
        holders
            .iter()
            .any(|h| h["pid"] == holder_pid && h["comm"] == "sleep"),
        "{doc:#}"
    );
    assert_eq!(after_recycle - after_close, 2 * MIB, "close");
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn why_not_freed_measures_a_deleted_subvolume_awaiting_the_cleaner() {
    let mnt = fixture();
    let doomed = mnt.join("wnf-doomed");
    let state = state_dir("why-pending");
    let baseline = unresolved_baseline(&mnt, &state);
    btrfs(&["-q", "subvolume", "create", text(&doomed)]);
    for i in 0..64 {
        write_entropy(&doomed.join(format!("f{i}")), 1, 0x7710 + i);
    }
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let before = btrfs_data_used(&mnt);
    // An open inode keeps the cleaner from dropping the deleted root.
    let mut holder = Holder::open(&doomed.join("f0"));
    btrfs(&["-q", "subvolume", "delete", text(&doomed)]);
    // The root item shows zero refs once the deletion is committed.
    btrfs(&["filesystem", "sync", text(&mnt)]);
    volume_scan(&mnt, &state, &[]);
    let (code, doc) = why_not_freed(&mnt, &state, &[]);
    holder.release();
    btrfs(&["subvolume", "sync", text(&mnt)]);
    let after = btrfs_data_used_after_sync(&mnt);
    let _ = std::fs::remove_dir_all(&state);
    let row = why_row(&doc, "pending_subvolume_cleanup");
    let unresolved = why_row(&doc, "unresolved_references");
    assert_eq!(row["count"], 1, "{doc:#}");
    assert_eq!(row["appeared_after_report"], 0, "{doc:#}");
    // The pending root's extents have no pinned-range answer yet, so the
    // report attributes them to no part: the row is only a lower bound.
    assert_eq!(row["lower_bound"], true, "{doc:#}");
    assert_eq!(code, Some(3), "{doc:#}");
    let freed = before - after;
    assert_eq!(freed, 64 * MIB);
    assert_eq!(
        row["bytes"].as_u64().unwrap() + unresolved["bytes"].as_u64().unwrap()
            - why_row(&baseline, "unresolved_references")["bytes"]
                .as_u64()
                .unwrap(),
        freed,
        "{doc:#}"
    );
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn why_not_freed_is_partial_while_references_stay_unresolved() {
    let mnt = fixture();
    let share = mnt.join("wnf-budget");
    let state = state_dir("why-budget");
    btrfs(&["-q", "subvolume", "create", text(&share)]);
    for i in 0..32 {
        write_entropy(&share.join(format!("f{i}")), 1, 0x3301 + i);
    }
    btrfs(&["filesystem", "sync", text(&mnt)]);
    let (snap, _) = snapshot(&mnt, &share, "wnf-budget-snap");
    // Rewriting the live copy COWs its shared leaves into full backrefs.
    write_mib(&mnt, &share.join("f0"), 1);
    volume_scan(&mnt, &state, &["--owner-budget", "0"]);
    let (code, doc) = why_not_freed(&mnt, &state, &[]);
    delete_subvolumes(&mnt, &[&snap, &share]);
    let _ = std::fs::remove_dir_all(&state);
    let row = why_row(&doc, "unresolved_references");
    assert_eq!(code, Some(3), "{doc:#}");
    assert_eq!(row["unknown"], true, "{doc:#}");
    assert!(row["bytes"].as_u64().unwrap() > 0, "{doc:#}");
    assert_eq!(row["action"]["kind"], "doctor");
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn why_not_freed_fails_without_a_report() {
    let state = state_dir("why-no-report");
    let (code, doc) = why_not_freed(&fixture(), &state, &[]);
    let _ = std::fs::remove_dir_all(&state);
    assert_eq!(code, Some(1), "{doc:#}");
    assert_eq!(doc["completion"], "failed", "{doc:#}");
    assert!(doc["results"].as_array().unwrap().is_empty());
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn snapshots_lists_a_restored_shares_snapshot_under_its_share() {
    const SHARE: &str = "restored-snapshot-share";
    let mnt = fixture();
    let state = state_dir("snapshots-restored-share");
    // DSM layout: the share's snapshots live in `@sharesnap/<share>`.
    let sharesnap = mnt.join("@sharesnap");
    let holder = sharesnap.join(SHARE);
    let snap = holder.join("GMT+08-2026.09.29-12.00.00");
    let share = mnt.join(SHARE);
    btrfs(&["-q", "subvolume", "create", text(&sharesnap)]);
    btrfs(&["-q", "subvolume", "create", text(&holder)]);
    clone_of_orphaned_snapshot(&mnt, &snap, &share);
    let snap_id = subvolume_id(&snap);

    let s = ["--json", "--quiet", "--state-dir", text(&state)];
    let mut args = vec!["scan", text(&share), "--no-cache", "--snapshots", "all"];
    args.extend(s);
    let (code, doc) = run_json(&args);
    assert_eq!(code, Some(0), "{doc:#}");

    let mut args = vec!["snapshots", text(&share)];
    args.extend(s);
    let (code, doc) = run_json(&args);
    assert_eq!(code, Some(0), "{doc:#}");
    let answer = &doc["results"][0]["shares"][0];
    let row = snapshot_row(&doc, snap_id);
    // The restored share shares all of the snapshot's data.
    assert_eq!(row["shared_with_live_bytes"], 4 * MIB, "{doc:#}");
    assert_eq!(row["exclusive_bytes"], 0, "{doc:#}");
    assert_eq!(answer["held_only_by_snapshots_bytes"], 0, "{doc:#}");
    assert_eq!(answer["unknown_bytes"], 0, "{doc:#}");

    // A volume scan must not add the `@sharesnap/<share>` folder as a
    // second share of the same name.
    let mut args = vec!["scan", text(&mnt), "--no-cache", "--snapshots", "all"];
    args.extend(s);
    let (code, doc) = run_json(&args);
    assert_eq!(code, Some(0), "{doc:#}");
    let mut args = vec!["snapshots", text(&mnt)];
    args.extend(s);
    let (code, doc) = run_json(&args);
    assert_eq!(code, Some(0), "{doc:#}");
    let named = doc["results"][0]["shares"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|share| share["share"]["path"] == SHARE)
        .count();
    assert_eq!(named, 1, "{doc:#}");

    delete_subvolumes(&mnt, &[&snap, &share, &holder, &sharesnap]);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn overview_attributes_share_snapshots_whose_source_was_deleted() {
    const SHARE: &str = "restored-share";
    let mnt = fixture();
    let state = state_dir("overview-restored-share");
    let baseline = volume_overview(&mnt, &state);
    // DSM layout: the share's snapshots live in `@sharesnap/<share>`.
    let sharesnap = mnt.join("@sharesnap");
    let holder = sharesnap.join(SHARE);
    let snap = holder.join("GMT+08-2026.09.29-12.00.00");
    let share = mnt.join(SHARE);
    btrfs(&["-q", "subvolume", "create", text(&sharesnap)]);
    btrfs(&["-q", "subvolume", "create", text(&holder)]);
    clone_of_orphaned_snapshot(&mnt, &snap, &share);

    let doc = volume_overview(&mnt, &state);
    let before = btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&snap]);
    let freed_by_snapshot = before - btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&share]);
    let freed = before - btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&holder, &sharesnap]);
    let _ = std::fs::remove_dir_all(&state);

    let part = |doc: &serde_json::Value, kind: &str| overview_part_bytes(doc, kind);
    assert_eq!(part(&doc, "unknown"), part(&baseline, "unknown"), "{doc:#}");
    assert_eq!(part(&doc, "snapshot_only"), 0, "{doc:#}");
    assert_eq!(overview_item_bytes(&doc, "share_live", SHARE), 4 * MIB);
    assert_eq!(freed_by_snapshot, 0);
    assert_eq!(freed, 4 * MIB);
}

#[test]
#[ignore = "needs a live Btrfs fixture; run scripts/integration.sh"]
fn overview_attributes_lun_snapshots_whose_source_was_deleted() {
    let mnt = fixture();
    let state = state_dir("overview-restored-lun");
    let baseline = volume_overview(&mnt, &state);
    // DSM layout: LUN snapshots below `@iSCSI/Snapshot`, LUNs below
    // `@iSCSI/LUN`; a restored LUN is a writable snapshot of a snapshot.
    let iscsi = mnt.join("@iSCSI");
    let snap = iscsi.join("Snapshot/BLUN/lun-1/snap-1");
    let lun = iscsi.join("LUN/BLUN/lun-1");
    btrfs(&["-q", "subvolume", "create", text(&iscsi)]);
    std::fs::create_dir_all(snap.parent().unwrap()).unwrap();
    std::fs::create_dir_all(lun.parent().unwrap()).unwrap();
    clone_of_orphaned_snapshot(&mnt, &snap, &lun);

    let doc = volume_overview(&mnt, &state);
    let before = btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&snap]);
    let freed_by_snapshot = before - btrfs_data_used_after_sync(&mnt);
    delete_subvolumes(&mnt, &[&lun, &iscsi]);
    let _ = std::fs::remove_dir_all(&state);

    let part = |doc: &serde_json::Value, kind: &str| overview_part_bytes(doc, kind);
    assert_eq!(part(&doc, "unknown"), part(&baseline, "unknown"), "{doc:#}");
    let service = part(&doc, "dsm_service") - part(&baseline, "dsm_service");
    assert_eq!(service, 4 * MIB, "{doc:#}");
    assert_eq!(freed_by_snapshot, 0);
}

/// Makes read-only `snap` of a 4 MiB subvolume that is then deleted, so
/// its parent_uuid names nothing present, and writable `clone` of `snap`
/// sharing its data, as DSM leaves a restored share or LUN.
fn clone_of_orphaned_snapshot(mnt: &Path, snap: &Path, clone: &Path) {
    let source = mnt.join("orphaned-snapshot-source");
    btrfs(&["-q", "subvolume", "create", text(&source)]);
    write_entropy(&source.join("payload.bin"), 4, 0x2c47_e019_b83d_5a61);
    btrfs(&["filesystem", "sync", text(mnt)]);
    btrfs(&[
        "-q",
        "subvolume",
        "snapshot",
        "-r",
        text(&source),
        text(snap),
    ]);
    delete_subvolumes(mnt, &[&source]);
    btrfs(&["-q", "subvolume", "snapshot", text(snap), text(clone)]);
    btrfs(&["filesystem", "sync", text(mnt)]);
}
