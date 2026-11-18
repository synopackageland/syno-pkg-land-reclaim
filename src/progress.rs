//! Progress of a scan (SPEC 22.1, 22.7): the human line on stderr,
//! counts not a fabricated percentage, and the same counts persisted in
//! the job manifest for another process to follow. Both use a previous
//! completed scan of the same scope as their only basis.

use crate::output::{JsonPath, human_bytes, human_count};
use crate::time::human_duration;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The confirmed rootfs plan, retained even if execution never writes a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsPlanDetails {
    #[serde(
        default,
        skip_serializing_if = "crate::rootfs::plan::CleanupMode::is_standard"
    )]
    pub mode: crate::rootfs::plan::CleanupMode,
    pub plan_id: String,
    pub move_dir: JsonPath,
}

/// Work reread from the beginning of a changed unit, separate from retained job totals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRestartProgress {
    pub subvolume_id: u64,
    pub entries: u64,
    pub items: u64,
    pub bytes_read: u64,
}

/// Live progress of a scan's current run, rewritten in the job manifest
/// at most every two seconds (SPEC 22.1) so another process can follow a
/// scan that is still running through `jobs --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    /// The phase the run is in, one of the scan phases.
    pub phase: String,
    /// Subvolume units read or reused so far.
    pub units_done: u64,
    /// Writable subvolumes discovered: the most units a volume scan can
    /// have; `None` for a path scan.
    pub units_total: Option<u64>,
    /// Entries of the trees fully read so far, plus the directory entries
    /// read so far in the tree being read. A reused unit does not
    /// count.
    pub entries: u64,
    /// Metadata items read so far, counted per batch, so the value moves
    /// while one large tree is read. It, not `entries`, is the measure
    /// that advances within one large share.
    pub items: u64,
    /// Payload bytes of those metadata items.
    pub bytes_read: u64,
    /// Seconds since this run started.
    pub elapsed_s: u64,
    /// Resident set size when the update was written.
    pub rss_bytes: Option<u64>,
    /// The subvolume being read; `None` between subvolumes and in the
    /// phases that read none.
    pub subvolume_id: Option<u64>,
    /// That subvolume's name: one entry, never a directory path. The UI
    /// shows it as the current item.
    pub current_subvolume: Option<JsonPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<UnitRestartProgress>,
    /// The last completed scan of the same target, so a consumer can show
    /// a percentage and an ETA; `None` when none is recorded.
    pub basis: Option<JobBasis>,
    /// When this update was written, RFC 3339 UTC. A consumer older than
    /// the staleness window knows the values are old.
    pub updated: String,
    /// What a planning job's units are, when it has several steps: a
    /// cleanup plan checks `kept` copies, then the `items` it moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

/// The last completed scan of the same scope, as far as the job manifest
/// needs it (SPEC 22.1). Only the fields a consumer needs to judge
/// progress; the full history record stays in `history/<fsid>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobBasis {
    /// Entries that scan read.
    pub entries: u64,
    /// How long that scan took.
    pub elapsed_s: u64,
    /// Metadata items that scan read; `None` until history records them
    /// (a later change), so a consumer must fall back to `entries`.
    #[serde(default)]
    pub items: Option<u64>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Live progress of a `cleanup apply` job (SPEC 5.7 "Apply progress in
/// a job"): the selected items moved into their Recycle Bins so far,
/// rewritten in the job manifest after each item and once more at the end.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupProgress {
    pub plan_id: String,
    /// Selected items in the plan.
    pub items_total: u64,
    /// Items with a result.
    pub items_done: u64,
    pub moved: u64,
    /// Items with no Recycle Bin, removed for good (absent before
    /// removing for good existed).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub deleted: u64,
    pub already_absent: u64,
    pub partial: u64,
    pub changed: u64,
    pub failed: u64,
    /// Allocated size (`st_blocks` × 512) of the planned items. Moving
    /// them frees nothing until their Recycle Bin is emptied.
    pub bytes_total: u64,
    /// The same, over what moved.
    pub bytes_moved: u64,
    /// Runs of DSM's tool so far: a large folder moves child by child.
    #[serde(default)]
    pub calls_done: u64,
    /// Seconds since the first move started.
    pub elapsed_s: u64,
    /// When this update was written, RFC 3339 UTC.
    pub updated: String,
}

/// Live progress of a `rootfs apply` job (SPEC 5.23), rewritten at most
/// every two seconds while the executor runs, then once more when it ends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsProgress {
    /// `moves` while moving planned entries, then `tools` for DSM cleaners.
    pub phase: String,
    pub items_done: u64,
    pub items_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// The DSM cleaner currently running, or null outside a tool run.
    pub tool: Option<crate::rootfs::delegate::Delegate>,
}

/// Entries a run has read so far: those of the trees fully read plus the
/// directory entries read so far in the current tree, so the count moves
/// while one large tree is read. A tree that starts again drops what its
/// earlier attempt counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryCount {
    finished: u64,
    current: u64,
}

impl EntryCount {
    pub fn tree_started(&mut self) {
        self.current = 0;
    }

    /// Adds the directory entries of a batch of the current tree.
    pub fn took(&mut self, dir_index_items: u64) {
        self.current += dir_index_items;
    }

    /// Replaces the current tree's running count with its final one.
    pub fn tree_read(&mut self, entries: u64) {
        self.finished += entries;
        self.current = 0;
    }

    pub fn total(&self) -> u64 {
        self.finished + self.current
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The filesystem's id, shortened when shown.
    pub fsid: String,
    pub phase: String,
    /// Subvolume units read or reused so far.
    pub units: u64,
    /// Writable subvolumes discovered: the most units a scan can have.
    pub writable: Option<u64>,
    pub entries: u64,
    /// Metadata items read so far, counted per batch, so the line moves
    /// while one large tree is read.
    pub items: u64,
    pub elapsed: Duration,
    pub rss: Option<u64>,
    /// How long the last completed scan of the same scope took.
    pub previous: Option<Duration>,
}

impl Progress {
    pub fn line(&self) -> String {
        let mut parts = vec![self.phase.clone(), self.subvolumes()];
        parts.push(format!("{} entries", human_count(self.entries)));
        parts.push(format!("{} items", human_count(self.items)));
        let seconds = self.elapsed.as_secs();
        if let Some(rate) = self.items.checked_div(seconds) {
            let last = parts.last_mut().expect("items pushed");
            last.push_str(&format!(" ({}/s)", human_count(rate)));
        }
        parts.push(format!("{}:{:02} elapsed", seconds / 60, seconds % 60));
        if let Some(rss) = self.rss {
            parts.push(format!("rss {}", human_bytes(rss)));
        }
        parts.extend(self.eta());
        format!(
            "synoreclaim: scan {}: {}",
            &self.fsid[..8.min(self.fsid.len())],
            parts.join(", ")
        )
    }

    fn subvolumes(&self) -> String {
        match self.writable {
            Some(total) => format!("{}/{total} subvolumes", self.units),
            None => format!("{} subvolumes", self.units),
        }
    }

    fn eta(&self) -> Option<String> {
        let previous = self.previous?;
        let last = human_duration(previous);
        Some(match previous.checked_sub(self.elapsed) {
            Some(left) if !left.is_zero() => {
                format!("ETA {} (last scan {last})", human_duration(left))
            }
            _ => format!("past the last scan's {last}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> Progress {
        Progress {
            fsid: "1a2b3c4d-0000-0000-0000-000000000000".into(),
            phase: "scan".into(),
            units: 12,
            writable: Some(190),
            entries: 1_200_000,
            items: 3_400_000,
            elapsed: Duration::from_secs(148),
            rss: Some(34 << 20),
            previous: None,
        }
    }

    #[test]
    fn entries_move_while_a_tree_is_read() {
        let mut entries = EntryCount::default();
        entries.tree_started();
        entries.took(3);
        entries.took(4);
        assert_eq!(entries.total(), 7);
        entries.tree_read(7);
        entries.tree_started();
        entries.took(2);
        assert_eq!(entries.total(), 9);
    }

    #[test]
    fn a_tree_read_again_counts_its_entries_once() {
        let mut entries = EntryCount::default();
        entries.tree_started();
        entries.took(5);
        entries.tree_started();
        entries.took(5);
        entries.tree_read(6);
        assert_eq!(entries.total(), 6);
    }

    #[test]
    fn job_progress_round_trips_with_a_non_utf8_subvolume_name() {
        let progress = JobProgress {
            phase: "scan".into(),
            step: None,
            units_done: 2,
            units_total: Some(3),
            entries: 3,
            items: 4,
            bytes_read: 5,
            elapsed_s: 6,
            rss_bytes: Some(7 << 20),
            subvolume_id: Some(256),
            current_subvolume: Some(JsonPath::new(b"ba\xffckup")),
            restart: None,
            basis: Some(JobBasis {
                entries: 8,
                elapsed_s: 9,
                items: None,
            }),
            updated: "2026-09-29T19:20:00Z".into(),
        };
        let text = serde_json::to_string(&progress).unwrap();
        assert!(text.contains("\"basis\":{\"entries\":8"), "{text}");
        assert!(text.contains("\"path_hex\":\"6261ff636b7570\""), "{text}");
        let back: JobProgress = serde_json::from_str(&text).unwrap();
        assert_eq!(back, progress);
    }

    #[test]
    fn without_a_previous_scan_the_line_shows_counts_and_no_eta() {
        assert_eq!(
            progress().line(),
            "synoreclaim: scan 1a2b3c4d: scan, 12/190 subvolumes, 1.20M entries, 3.40M items (23.0k/s), 2:28 elapsed, rss 34 MB"
        );
        let early = Progress {
            writable: None,
            elapsed: Duration::ZERO,
            rss: None,
            ..progress()
        };
        assert_eq!(
            early.line(),
            "synoreclaim: scan 1a2b3c4d: scan, 12 subvolumes, 1.20M entries, 3.40M items, 0:00 elapsed"
        );
    }

    #[test]
    fn a_previous_scan_of_the_scope_gives_an_eta_until_it_is_exceeded() {
        let within = Progress {
            previous: Some(Duration::from_secs(911)),
            ..progress()
        };
        assert!(
            within.line().ends_with(", ETA ~13 min (last scan ~15 min)"),
            "{}",
            within.line()
        );
        let past = Progress {
            elapsed: Duration::from_secs(1000),
            previous: Some(Duration::from_secs(911)),
            ..progress()
        };
        assert!(
            past.line()
                .ends_with(", 16:40 elapsed, rss 34 MB, past the last scan's ~15 min"),
            "{}",
            past.line()
        );
    }
}
