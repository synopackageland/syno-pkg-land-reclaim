//! What the completed scans of a filesystem took, one record per scan
//! (SPEC 5.1, 22.4): the estimates of the no-report hint, the progress
//! ETA, doctor's spill projection, and the scan history (`history`
//! command). Reports can be pruned or invalidated; this small file
//! outlives them.

use crate::output::JsonPath;
use crate::report::Header;
use crate::state::{StateError, private_dir, replace_file, scope_key};
use crate::sys::Space;
use crate::target::Scope;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bumped when the record layout changes; older files are ignored.
pub const FORMAT: u32 = 2;
/// Records kept per filesystem; the oldest beyond this are dropped.
pub const KEPT: usize = 64;

/// How a scan ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Completed,
    Partial,
    Superseded,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Partial => "partial",
            Outcome::Superseded => "superseded",
        }
    }
}

/// One completed scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRecord {
    /// `state::scope_key` of the scan's roots.
    pub scope_key: String,
    /// Whether the scan read the whole volume.
    pub volume: bool,
    pub job_id: String,
    /// The scan's roots.
    #[serde(default)]
    pub targets: Vec<JsonPath>,
    /// The resource profile the scan ran with.
    pub profile: Option<String>,
    /// Unix seconds.
    pub started: u64,
    pub finished: u64,
    /// This run's elapsed time.
    pub elapsed_ms: u64,
    /// All runs of the job so far, including earlier resumed runs: the
    /// job's wall time from its creation to this publish.
    pub total_elapsed_ms: u64,
    /// Units the run read (not reused).
    pub scanned_units: u64,
    /// Units the run reused from the cache.
    pub reused_units: u64,
    /// The last full read of this scope: a run that read without reusing
    /// anything. Reuse-heavy and resumed runs inherit it instead of
    /// replacing it, so the ETA basis stays a whole read (SPEC 22.7).
    #[serde(default)]
    pub full_read_ms: Option<u64>,
    #[serde(default)]
    pub full_read_entries: Option<u64>,
    pub entries: u64,
    pub subvolumes: u64,
    /// Largest size the scan's spill directory reached.
    pub spill_peak_bytes: u64,
    #[serde(default)]
    pub peak_rss_bytes: Option<u64>,
    pub outcome: Outcome,
    /// The outcome before the report was superseded; absent in older histories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_outcome: Option<Outcome>,
    /// Why a partial or superseded scan is not complete.
    #[serde(default)]
    pub reason: Option<String>,
    /// The report the scan published, while its file still exists.
    #[serde(default)]
    pub report: Option<JsonPath>,
    /// Bytes of the published report.
    #[serde(default)]
    pub report_bytes: Option<u64>,
    /// Filesystem bytes around the scan (statvfs, as `volumes` reports
    /// them); `None` when the scan could not read them.
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub used_bytes: Option<u64>,
    #[serde(default)]
    pub free_bytes: Option<u64>,
    /// Btrfs DATA block-group bytes in use at the scan's end, the
    /// overview reconciliation number; `None` when unavailable.
    #[serde(default)]
    pub btrfs_data_used_bytes: Option<u64>,
    /// Additive display evidence; legacy histories keep their original outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<crate::overview::Attribution>,
}

impl ScanRecord {
    /// The ETA basis: the last full read of the scope, or this run when
    /// no full read is recorded. Returns `(elapsed_ms, entries)`.
    pub fn eta_basis(&self) -> (u64, u64) {
        (
            self.full_read_ms.unwrap_or(self.elapsed_ms),
            self.full_read_entries.unwrap_or(self.entries),
        )
    }

    /// The record of the scan that published `header`.
    pub fn of(header: &Header, spill_peak_bytes: u64, space: Option<Space>) -> Self {
        let stats = &header.stats;
        let outcome = if header.consistency == crate::report::Consistency::Partial {
            Outcome::Partial
        } else {
            Outcome::Completed
        };
        let read_all = stats.subvolumes_scanned > 0 && stats.subvolumes_reused == 0;
        ScanRecord {
            scope_key: scope_key(&header.roots),
            volume: header.roots.iter().any(|r| r.scope == Scope::Volume),
            job_id: header.job_id.clone(),
            targets: header
                .roots
                .iter()
                .map(|root| JsonPath::new(&root.path))
                .collect(),
            profile: Some(header.profile.clone()),
            started: header.scan_started,
            finished: header.scan_finished,
            elapsed_ms: stats.elapsed_ms,
            total_elapsed_ms: stats.elapsed_ms,
            scanned_units: stats.subvolumes_scanned,
            reused_units: stats.subvolumes_reused,
            full_read_ms: read_all.then_some(stats.elapsed_ms),
            full_read_entries: read_all.then_some(stats.entries),
            entries: stats.entries,
            subvolumes: stats.subvolumes_scanned + stats.subvolumes_reused,
            spill_peak_bytes,
            peak_rss_bytes: stats.peak_rss_bytes,
            outcome,
            original_outcome: None,
            reason: (outcome == Outcome::Partial).then(|| header.consistency.name().to_string()),
            report: None,
            report_bytes: None,
            total_bytes: space.map(|space| space.total),
            used_bytes: space.map(|space| space.total - space.free),
            free_bytes: space.map(|space| space.available),
            btrfs_data_used_bytes: None,
            attribution: crate::overview::Attribution::of(header),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HistoryFile {
    format: u32,
    scans: Vec<ScanRecord>,
}

/// The history directory of a state directory.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("history")
}

fn file_name(fsid: &str) -> String {
    format!("{fsid}.json")
}

/// The records of `fsid` under `dir`, newest first; none when the file
/// is missing, unreadable or of another format.
pub fn load(dir: &Path, fsid: &str) -> Vec<ScanRecord> {
    std::fs::read(dir.join(file_name(fsid)))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HistoryFile>(&bytes).ok())
        .filter(|file| file.format == FORMAT)
        .map(|file| file.scans)
        .unwrap_or_default()
}

/// Every filesystem's records under `dir`, newest first.
pub fn load_all(dir: &Path) -> Vec<(String, ScanRecord)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut all: Vec<(String, ScanRecord)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let fsid = name.to_str()?.strip_suffix(".json")?;
            Some(
                load(dir, fsid)
                    .into_iter()
                    .map(|scan| (fsid.to_string(), scan))
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    all.sort_by_key(|(_, scan)| std::cmp::Reverse(scan.finished));
    all
}

/// Saves `scan` as the newest record of its filesystem. Older records of
/// the same scope are marked superseded: their report was replaced by
/// this scan's. A run that reused units inherits the scope's last
/// full-read basis instead of replacing it.
pub fn record(dir: &Path, uid: u32, fsid: &str, mut scan: ScanRecord) -> Result<(), StateError> {
    let mut scans = load(dir, fsid);
    if scan.full_read_ms.is_none()
        && let Some(basis) = scans
            .iter()
            .find(|s| s.scope_key == scan.scope_key && s.full_read_ms.is_some())
            .or_else(|| scans.iter().find(|s| s.full_read_ms.is_some()))
    {
        scan.full_read_ms = basis.full_read_ms;
        scan.full_read_entries = basis.full_read_entries;
    }
    for older in scans
        .iter_mut()
        .filter(|s| s.scope_key == scan.scope_key && s.finished <= scan.finished)
    {
        if older.outcome != Outcome::Superseded {
            older.original_outcome = Some(older.outcome);
        }
        older.outcome = Outcome::Superseded;
        older.reason = Some("a newer scan of the same target replaced this report".into());
        older.report = None;
    }
    scans.push(scan);
    scans.sort_by_key(|scan| std::cmp::Reverse(scan.finished));
    scans.truncate(KEPT);
    let file = HistoryFile {
        format: FORMAT,
        scans,
    };
    private_dir(dir, uid)?;
    replace_file(dir, &file_name(fsid), |out| {
        serde_json::to_writer(out, &file).map_err(std::io::Error::other)
    })
}

/// The newest record of a whole-volume scan.
pub fn latest_volume(records: &[ScanRecord]) -> Option<&ScanRecord> {
    records.iter().find(|r| r.volume)
}

/// The newest record of the scope `scope_key`.
pub fn latest_of<'a>(records: &'a [ScanRecord], scope_key: &str) -> Option<&'a ScanRecord> {
    records.iter().find(|r| r.scope_key == scope_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::testing::scratch;

    fn scan(scope_key: &str, finished: u64) -> ScanRecord {
        ScanRecord {
            scope_key: scope_key.into(),
            volume: scope_key == "vol",
            job_id: format!("job-{finished}"),
            targets: vec![JsonPath::new(b"/v")],
            profile: Some("low".into()),
            started: finished.saturating_sub(5),
            finished,
            elapsed_ms: finished * 1000,
            total_elapsed_ms: finished * 1000,
            scanned_units: 2,
            reused_units: 0,
            full_read_ms: Some(finished * 1000),
            full_read_entries: Some(10),
            entries: 10,
            subvolumes: 2,
            spill_peak_bytes: 4096,
            peak_rss_bytes: Some(1 << 20),
            outcome: Outcome::Completed,
            original_outcome: None,
            reason: None,
            report: Some(JsonPath::new(b"/state/report")),
            report_bytes: Some(4096),
            total_bytes: Some(1 << 30),
            used_bytes: Some(1 << 20),
            free_bytes: Some(1 << 29),
            btrfs_data_used_bytes: Some(1 << 20),
            attribution: None,
        }
    }

    fn uid() -> u32 {
        crate::sys::effective_uid()
    }

    #[test]
    fn a_record_takes_the_scope_and_work_of_a_published_scan() {
        use crate::report::{RootInfo, testing::header};
        use crate::target::Scope;
        let root = |scope, path: &[u8]| RootInfo {
            path: path.to_vec(),
            scope,
            tree: 5,
            ino: 256,
        };
        let mut volume = header(vec![root(Scope::Volume, b"/v")]);
        volume.stats.elapsed_ms = 911_000;
        volume.stats.entries = 5_510_000;
        volume.stats.subvolumes_scanned = 190;
        volume.stats.subvolumes_reused = 7;
        volume.stats.peak_rss_bytes = Some(40 << 20);
        let record = ScanRecord::of(
            &volume,
            700 << 20,
            Some(Space {
                total: 1 << 40,
                free: 1 << 39,
                available: 1 << 39,
            }),
        );
        assert_eq!(
            record,
            ScanRecord {
                scope_key: scope_key(&volume.roots),
                volume: true,
                job_id: volume.job_id.clone(),
                targets: vec![JsonPath::new(b"/v")],
                profile: Some("low".into()),
                started: volume.scan_started,
                finished: volume.scan_finished,
                elapsed_ms: 911_000,
                total_elapsed_ms: 911_000,
                scanned_units: 190,
                reused_units: 7,
                full_read_ms: None,
                full_read_entries: None,
                entries: 5_510_000,
                subvolumes: 197,
                spill_peak_bytes: 700 << 20,
                peak_rss_bytes: Some(40 << 20),
                outcome: Outcome::Completed,
                original_outcome: None,
                reason: None,
                report: None,
                report_bytes: None,
                total_bytes: Some(1 << 40),
                used_bytes: Some(1 << 39),
                free_bytes: Some(1 << 39),
                btrfs_data_used_bytes: None,
                attribution: None,
            }
        );
        let path = header(vec![root(Scope::Path, b"/v/share")]);
        assert!(!ScanRecord::of(&path, 0, None).volume);
        assert_eq!(ScanRecord::of(&path, 0, None).total_bytes, None);
    }

    #[test]
    fn a_partial_scan_says_why() {
        use crate::report::testing::header;
        let mut partial = header(Vec::new());
        partial.consistency = crate::report::Consistency::Partial;
        let record = ScanRecord::of(&partial, 0, None);
        assert_eq!(record.outcome, Outcome::Partial);
        assert_eq!(record.reason.as_deref(), Some("partial"));
    }

    #[test]
    fn superseding_preserves_whether_the_scan_was_complete() {
        let dir = scratch("history-complete").join("history");
        record(&dir, uid(), "fs", scan("vol", 1)).unwrap();
        let mut partial = scan("vol", 2);
        partial.outcome = Outcome::Partial;
        record(&dir, uid(), "fs", partial).unwrap();
        record(&dir, uid(), "fs", scan("vol", 3)).unwrap();
        let records = load(&dir, "fs");
        let json = serde_json::to_value(records).unwrap();
        assert_eq!(json[1]["original_outcome"], "partial");
        assert_eq!(json[2]["original_outcome"], "completed");
    }

    #[test]
    fn a_missing_or_corrupt_history_has_no_records() {
        let dir = scratch("history");
        assert!(load(&dir, "fs").is_empty());
        assert!(load_all(&dir).is_empty());
        std::fs::write(dir.join(file_name("fs")), b"{not json").unwrap();
        assert!(load(&dir, "fs").is_empty());
    }

    #[test]
    fn a_new_scan_supersedes_older_records_of_the_same_scope() {
        let dir = scratch("history").join("history");
        record(&dir, uid(), "fs", scan("vol", 1)).unwrap();
        record(&dir, uid(), "fs", scan("path", 2)).unwrap();
        record(&dir, uid(), "fs", scan("vol", 3)).unwrap();
        let records = load(&dir, "fs");
        assert_eq!(records.len(), 3);
        assert_eq!(records[0], scan("vol", 3));
        let superseded = &records[2];
        assert_eq!(superseded.outcome, Outcome::Superseded);
        assert_eq!(superseded.report, None);
        assert!(superseded.reason.as_deref().unwrap().contains("newer scan"));
        assert!(load(&dir, "other").is_empty());
    }

    #[test]
    fn every_filesystem_is_read_newest_first() {
        let dir = scratch("history").join("history");
        record(&dir, uid(), "fs", scan("path", 4)).unwrap();
        record(&dir, uid(), "other", scan("vol", 7)).unwrap();
        let all = load_all(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "other");
        assert_eq!(all[1].0, "fs");
        assert_eq!(all[0].1.finished, 7);
        assert_eq!(all[1].1.finished, 4);
    }

    #[test]
    fn only_the_newest_records_are_kept() {
        let dir = scratch("history").join("history");
        for n in 0..KEPT as u64 + 3 {
            record(&dir, uid(), "fs", scan(&format!("s{n}"), n)).unwrap();
        }
        let records = load(&dir, "fs");
        assert_eq!(records.len(), KEPT);
        assert_eq!(records[0].finished, KEPT as u64 + 2);
    }

    #[test]
    fn a_reuse_heavy_run_inherits_the_last_full_read_basis() {
        let dir = scratch("history-eta").join("history");
        let mut full = scan("vol", 10);
        full.elapsed_ms = 900_000;
        full.entries = 5_000_000;
        full.full_read_ms = Some(900_000);
        full.full_read_entries = Some(5_000_000);
        record(&dir, uid(), "fs", full).unwrap();
        let mut reuse = scan("vol", 20);
        reuse.scanned_units = 0;
        reuse.reused_units = 3;
        reuse.elapsed_ms = 2_000;
        reuse.entries = 0;
        reuse.full_read_ms = None;
        reuse.full_read_entries = None;
        record(&dir, uid(), "fs", reuse).unwrap();
        let records = load(&dir, "fs");
        assert_eq!(records[0].elapsed_ms, 2_000, "this run's time is kept");
        assert_eq!(records[0].eta_basis(), (900_000, 5_000_000));
        let mut fresh = scan("vol", 30);
        fresh.elapsed_ms = 800_000;
        fresh.entries = 5_100_000;
        fresh.full_read_ms = Some(800_000);
        fresh.full_read_entries = Some(5_100_000);
        record(&dir, uid(), "fs", fresh).unwrap();
        assert_eq!(load(&dir, "fs")[0].eta_basis(), (800_000, 5_100_000));
    }

    #[test]
    fn the_eta_falls_back_to_the_run_when_no_full_read_is_recorded() {
        let record = scan("vol", 5);
        assert_eq!(record.eta_basis(), (5_000, 10));
    }

    #[test]
    fn lookups_find_the_newest_volume_scan_or_scope() {
        let records = vec![scan("path", 5), scan("vol", 4), scan("path", 1)];
        assert_eq!(latest_volume(&records), Some(&records[1]));
        assert_eq!(latest_of(&records, "path"), Some(&records[0]));
        assert_eq!(latest_of(&records, "none"), None);
        assert_eq!(latest_volume(&records[..1]), None);
    }
}
