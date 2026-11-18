//! JSON and human output for `why-not-freed` (SPEC 5.15).

use super::{Envelope, OverviewFilesystem, ReportInfo, notices, report_line, shell_word};
use crate::output::{escape_human, human_bytes};
use crate::snapshots::HELD_FILES_KEPT;
use crate::snapshots::answer::HeldFilesJson;
use crate::why_not_freed::{OpenUnlinked, Reason, ShareName};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WhyNotFreedDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub results: Vec<WhyNotFreedResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WhyNotFreedResult {
    pub report: ReportInfo,
    pub filesystem: OverviewFilesystem,
    /// statfs free bytes now; `None` when it could not be read.
    pub free_bytes: Option<u64>,
    pub reasons: Vec<ReasonEntry>,
}

/// One row: its measured values, whether one it should give is unknown,
/// and what the user can do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReasonEntry {
    #[serde(flatten)]
    pub reason: Reason,
    pub unknown: bool,
    pub action: Action,
}

impl ReasonEntry {
    pub fn new(reason: Reason, mount: &str) -> Self {
        ReasonEntry {
            unknown: reason.is_unknown(),
            action: Action::for_reason(&reason, mount),
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// `wait`, `snapshot`, `recycle`, `processes`, `restart`, `inspect`
    /// or `doctor`.
    pub kind: &'static str,
    pub manage_in: Option<&'static str>,
    /// Stable id of that DSM place (SPEC 4.6, GAP-14); null with `manage_in`.
    pub manage_in_id: Option<&'static str>,
    pub command: Option<Vec<String>>,
}

/// Where DSM empties a shared folder's Recycle Bin.
const RECYCLE_BIN: &str = crate::dsm::RECYCLE_BIN_EDIT;

impl Action {
    fn for_reason(reason: &Reason, mount: &str) -> Self {
        let run = |args: &[&str]| {
            let mut command = vec!["synoreclaim".to_string()];
            command.extend(args.iter().map(|a| a.to_string()));
            Some(command)
        };
        let (kind, manage_in, command) = match reason {
            Reason::PendingSubvolumeCleanup(_) | Reason::CommitPending { .. } => {
                ("wait", None, None)
            }
            Reason::HeldBySnapshots(row) => (
                "snapshot",
                Some(
                    row.items
                        .first()
                        .map_or(crate::snapshots::SNAPSHOT_REPLICATION, |i| i.manage_in),
                ),
                run(&["snapshots", mount]),
            ),
            Reason::RecycleBin(_) => ("recycle", Some(RECYCLE_BIN), run(&["recycle", mount])),
            Reason::OpenUnlinked(OpenUnlinked {
                processes: None, ..
            }) => (
                "processes",
                None,
                run(&["why-not-freed", mount, "--processes"]),
            ),
            Reason::OpenUnlinked(_) => ("restart", None, None),
            Reason::HardLink { .. } => ("inspect", None, None),
            Reason::UnresolvedReferences(_) => ("doctor", None, run(&["doctor", mount])),
        };
        Action {
            kind,
            manage_in_id: manage_in.map(crate::dsm::place_id),
            manage_in,
            command,
        }
    }
}

pub fn render_why_not_freed(doc: &WhyNotFreedDocument) -> String {
    let mut out = String::new();
    for result in &doc.results {
        let free = result.free_bytes.map_or("unknown".into(), human_bytes);
        out.push_str(&format!(
            "{}  free now {free}; report {}\n\n",
            super::escape_path(&result.filesystem.mount),
            report_line(&result.report)
        ));
        out.push_str(&row("WHY SPACE IS NOT BACK", "BYTES", "WHAT TO DO"));
        for entry in &result.reasons {
            render_reason(&mut out, entry);
        }
        out.push_str(&format!(
            "\nFor live free-space changes, run `synoreclaim watch-free {} --duration 10m`.\n\n",
            shell_word(&result.filesystem.mount.bytes())
        ));
    }
    notices(&mut out, &doc.envelope);
    out
}

fn row(label: &str, bytes: &str, what: &str) -> String {
    format!("{label:<48} {bytes:>10}  {what}\n")
}

/// `≥ bytes` when unresolved bytes may add to the row.
fn at_least(lower_bound: bool, bytes: &str) -> String {
    if lower_bound {
        format!("≥ {bytes}")
    } else {
        bytes.to_string()
    }
}

fn bytes_text(bytes: Option<u64>) -> String {
    bytes.map_or("unknown".into(), human_bytes)
}

fn what_to_do(entry: &ReasonEntry, text: &str) -> String {
    match (&entry.action.manage_in, &entry.action.command) {
        (Some(place), Some(command)) => format!("{place} ({})", command.join(" ")),
        (None, Some(command)) => format!("{text}: {}", command.join(" ")),
        (Some(place), None) => place.to_string(),
        (None, None) => text.to_string(),
    }
}

/// A share's largest held files below its row, and what was left out.
fn render_held_files(out: &mut String, held: &HeldFilesJson, entry: &ReasonEntry) {
    for file in &held.files {
        out.push_str(&format!(
            "    {:>10}  {}\n",
            super::unknown_cell(file.bytes, file.unknown_bytes),
            super::held_file_name(file)
        ));
    }
    let more = held.count.saturating_sub(held.files.len() as u64);
    if more > 0 {
        let list = entry.action.command.as_ref().map_or(String::new(), |c| {
            format!(
                ": `{} --limit {HELD_FILES_KEPT}` lists up to {HELD_FILES_KEPT}",
                c.join(" ")
            )
        });
        out.push_str(&format!("    and {more} more file(s){list}\n"));
    }
    if !held.complete {
        out.push_str(
            "    only some files were tallied: rescan with a larger --held-files-budget\n",
        );
    }
}

fn share_label(name: &Option<ShareName>, id: u64) -> String {
    name.as_ref().map_or_else(
        || format!("subvolume {id}"),
        |n| escape_human(n.share.as_bytes()),
    )
}

fn render_reason(out: &mut String, entry: &ReasonEntry) {
    match &entry.reason {
        Reason::PendingSubvolumeCleanup(pending) => {
            let count = pending.count.map_or("unknown".into(), |n| n.to_string());
            out.push_str(&row(
                &format!("Deleted snapshots still being cleaned ({count})"),
                &at_least(pending.lower_bound, &bytes_text(pending.bytes)),
                &what_to_do(entry, "nothing: Btrfs frees it in the background"),
            ));
        }
        Reason::HeldBySnapshots(held) => {
            out.push_str(&row(
                "Files deleted from shares, still in snapshots",
                &at_least(held.lower_bound, &human_bytes(held.bytes)),
                &what_to_do(entry, "delete snapshots"),
            ));
            for item in &held.items {
                let count = item
                    .snapshots
                    .map_or(String::new(), |n| format!(": {n} snapshots"));
                out.push_str(&row(
                    &format!("  {}{count}", share_label(&item.name, item.subvolume_id)),
                    &human_bytes(item.bytes),
                    "",
                ));
                if let Some(files) = &item.files {
                    render_held_files(out, files, entry);
                }
            }
        }
        Reason::RecycleBin(bins) => {
            let since = bins.since_seconds.map_or(String::new(), |s| {
                format!(
                    " (moved there in the last {}: {})",
                    crate::time::human_age(std::time::Duration::from_secs(s)),
                    bytes_text(bins.since_bytes)
                )
            });
            out.push_str(&row(
                &format!("Recycle bins{since}"),
                &at_least(bins.lower_bound, &human_bytes(bins.bytes)),
                &what_to_do(entry, "Empty Recycle Bin"),
            ));
            for item in &bins.items {
                out.push_str(&row(
                    &format!("  {}", share_label(&item.name, item.subvolume_id)),
                    &human_bytes(item.bytes),
                    "",
                ));
            }
        }
        Reason::OpenUnlinked(open) => render_open(out, entry, open),
        Reason::HardLink { bytes } => out.push_str(&row(
            "Other names (hard links) of deleted files",
            &bytes_text(*bytes),
            "synoreclaim inspect --explain <path> for files that still exist",
        )),
        Reason::CommitPending { interval_s } => out.push_str(&row(
            "Btrfs commit not yet done",
            "—",
            &format!("wait one commit interval ({interval_s} s)"),
        )),
        Reason::UnresolvedReferences(unresolved) => {
            out.push_str(&row(
                "Unresolved references",
                &human_bytes(unresolved.bytes),
                &what_to_do(entry, "may belong to any row above"),
            ));
            for part in &unresolved.reasons {
                let code = serde_json::to_value(part.code).expect("a unit variant");
                out.push_str(&row(
                    &format!("  {}", code.as_str().unwrap_or_default()),
                    &human_bytes(part.bytes),
                    "",
                ));
            }
        }
    }
}

fn render_open(out: &mut String, entry: &ReasonEntry, open: &OpenUnlinked) {
    let Some(scan) = &open.processes else {
        out.push_str(&row(
            "Deleted but still open (inodes without names)",
            &human_bytes(open.report_orphan_bytes),
            &what_to_do(entry, "find the processes"),
        ));
        return;
    };
    out.push_str(&row(
        "Deleted but still open (allocated, estimate)",
        &human_bytes(scan.bytes_estimate),
        "restart or stop the holding processes",
    ));
    for holder in &scan.holders {
        out.push_str(&row(
            &format!(
                "  {} (pid {}) holds {} files",
                holder.comm, holder.pid, holder.inodes
            ),
            &human_bytes(holder.bytes_estimate),
            "",
        ));
    }
    if scan.unreadable_processes > 0 {
        out.push_str(&format!(
            "  {} processes could not be read\n",
            scan.unreadable_processes
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::JsonPath;
    use crate::why_not_freed::{
        HeldBySnapshots, OpenHolder, PendingCleanup, ProcessScan, RecycleBins, RecycleShare,
        SnapshotShare, Unresolved,
    };

    fn command(action: &Action) -> Option<String> {
        action.command.as_ref().map(|c| c.join(" "))
    }

    fn pending(count: Option<usize>, bytes: Option<u64>) -> Reason {
        Reason::PendingSubvolumeCleanup(PendingCleanup {
            count,
            bytes,
            appeared_after_report: Some(0),
            cleaned_since_report: Some(0),
            lower_bound: false,
            since_not_measured: false,
        })
    }

    fn open(processes: Option<ProcessScan>) -> Reason {
        Reason::OpenUnlinked(OpenUnlinked {
            report_orphan_bytes: 0,
            processes,
        })
    }

    #[test]
    fn each_row_names_what_to_do() {
        let snapshots = Reason::HeldBySnapshots(HeldBySnapshots {
            bytes: 0,
            multi_share_bytes: 0,
            items: Vec::new(),
            lower_bound: false,
        });
        let recycle = Reason::RecycleBin(RecycleBins {
            bytes: 0,
            reclaimable_bytes: 0,
            since_seconds: None,
            since_bytes: None,
            items: Vec::new(),
            lower_bound: false,
        });
        let unresolved = Reason::UnresolvedReferences(Unresolved {
            bytes: 1,
            reasons: Vec::new(),
        });
        let scanned = ProcessScan {
            bytes_estimate: 0,
            holders: Vec::new(),
            unreadable_processes: 0,
        };
        let cases = [
            (pending(Some(1), Some(1)), "wait", None),
            (snapshots, "snapshot", Some("synoreclaim snapshots /v")),
            (recycle, "recycle", Some("synoreclaim recycle /v")),
            (
                open(None),
                "processes",
                Some("synoreclaim why-not-freed /v --processes"),
            ),
            (open(Some(scanned)), "restart", None),
            (Reason::HardLink { bytes: None }, "inspect", None),
            (Reason::CommitPending { interval_s: 30 }, "wait", None),
            (unresolved, "doctor", Some("synoreclaim doctor /v")),
        ];
        for (reason, kind, expected) in cases {
            let action = Action::for_reason(&reason, "/v");
            assert_eq!(action.kind, kind, "{reason:?}");
            assert_eq!(command(&action).as_deref(), expected, "{reason:?}");
        }
        let recycle = Action::for_reason(
            &Reason::RecycleBin(RecycleBins {
                bytes: 0,
                reclaimable_bytes: 0,
                since_seconds: None,
                since_bytes: None,
                items: Vec::new(),
                lower_bound: false,
            }),
            "/v",
        );
        assert_eq!(recycle.manage_in, Some(RECYCLE_BIN));
    }

    #[test]
    fn a_row_serializes_its_code_values_unknown_flag_and_action() {
        let entry = ReasonEntry::new(pending(Some(2), None), "/v");
        let json = serde_json::to_value(&entry).unwrap();

        assert_eq!(json["code"], "pending_subvolume_cleanup");
        assert_eq!(json["count"], 2);
        assert_eq!(json["bytes"], serde_json::Value::Null);
        assert_eq!(json["unknown"], true);
        assert_eq!(json["action"]["kind"], "wait");
    }

    fn share(name: &str) -> Option<ShareName> {
        Some(ShareName {
            share: name.into(),
            share_hex: None,
        })
    }

    fn document(reasons: Vec<Reason>) -> WhyNotFreedDocument {
        let header = crate::report::testing::header(Vec::new());
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_100);
        WhyNotFreedDocument {
            envelope: Envelope::new("why-not-freed", now),
            results: vec![WhyNotFreedResult {
                report: ReportInfo::new(b"/r", &header, now, std::time::Duration::from_secs(60)),
                filesystem: OverviewFilesystem {
                    kind: header.filesystem,
                    capabilities: header.capabilities.clone(),
                    uuid: header.fsid.clone(),
                    mount: crate::output::JsonPath::new(b"/v"),
                },
                free_bytes: Some(3 << 30),
                reasons: reasons
                    .into_iter()
                    .map(|r| ReasonEntry::new(r, "/v"))
                    .collect(),
            }],
        }
    }

    #[test]
    fn the_human_table_lists_every_row_with_bytes_or_unknown() {
        let doc = document(vec![
            pending(Some(2), Some(18 << 30)),
            Reason::HeldBySnapshots(HeldBySnapshots {
                bytes: 5 << 30,
                multi_share_bytes: 0,
                items: vec![SnapshotShare {
                    subvolume_id: 261,
                    name: share("photos"),
                    bytes: 5 << 30,
                    snapshots: Some(3),
                    newest_snapshot_otime: None,
                    manage_in: crate::snapshots::SNAPSHOT_REPLICATION,
                    manage_in_id: crate::dsm::place_id(crate::snapshots::SNAPSHOT_REPLICATION),
                    files: Some(crate::snapshots::answer::HeldFilesJson {
                        complete: false,
                        count: 3,
                        files: vec![crate::snapshots::answer::HeldFileJson {
                            snapshot_id: 1000,
                            snapshot: JsonPath::new(b"s1"),
                            ino: 257,
                            path: Some(JsonPath::new(b"a.jpg")),
                            live_path: Some(JsonPath::new(b"/v/photos/a.jpg")),
                            in_live: false,
                            bytes: 4 << 30,
                            unknown_bytes: 0,
                        }],
                    }),
                }],
                lower_bound: false,
            }),
            Reason::RecycleBin(RecycleBins {
                bytes: 2 << 30,
                reclaimable_bytes: 2 << 30,
                since_seconds: Some(86_400),
                since_bytes: None,
                items: vec![RecycleShare {
                    subvolume_id: 261,
                    name: share("photos"),
                    bytes: 2 << 30,
                    reclaimable_bytes: 2 << 30,
                }],
                lower_bound: false,
            }),
            open(Some(ProcessScan {
                bytes_estimate: 6 << 20,
                holders: vec![OpenHolder {
                    pid: 8123,
                    comm: "synoindexd".into(),
                    inodes: 2,
                    bytes_estimate: 6 << 20,
                }],
                unreadable_processes: 0,
            })),
            Reason::HardLink { bytes: None },
            Reason::CommitPending { interval_s: 30 },
            Reason::UnresolvedReferences(Unresolved {
                bytes: 0,
                reasons: Vec::new(),
            }),
        ]);

        let text = render_why_not_freed(&doc);

        assert!(text.starts_with("/v  free now 3 GB; report "), "{text}");
        for line in [
            "Deleted snapshots still being cleaned (2)",
            "18 GB",
            "nothing: Btrfs frees it in the background",
            "Files deleted from shares, still in snapshots",
            "  photos: 3 snapshots",
            "          4 GB  /v/photos/a.jpg  (deleted)\n",
            "    and 2 more file(s): `synoreclaim snapshots /v --limit 100` lists up to 100",
            "    only some files were tallied: rescan with a larger --held-files-budget",
            "synoreclaim snapshots /v",
            "Recycle bins (moved there in the last 1d: unknown)",
            "synoreclaim recycle /v",
            "Deleted but still open",
            "  synoindexd (pid 8123) holds 2 files",
            "Other names (hard links) of deleted files",
            "unknown",
            "Btrfs commit not yet done",
            "wait one commit interval (30 s)",
            "Unresolved references",
        ] {
            assert!(text.contains(line), "missing {line:?} in\n{text}");
        }
    }

    #[test]
    fn unresolved_parts_are_listed_as_possibly_any_row_above() {
        let doc = document(vec![Reason::UnresolvedReferences(Unresolved {
            bytes: 10 << 20,
            reasons: vec![
                crate::why_not_freed::UnresolvedPart {
                    code: crate::overview::UnknownReason::PinnedRangesUnknown,
                    bytes: 8 << 20,
                },
                crate::why_not_freed::UnresolvedPart {
                    code: crate::overview::UnknownReason::DirectOwnerUnresolved,
                    bytes: 2 << 20,
                },
            ],
        })]);

        let text = render_why_not_freed(&doc);

        for line in [
            "may belong to any row above",
            "  pinned_ranges_unknown",
            "8 MB",
            "  direct_owner_unresolved",
            "2 MB",
        ] {
            assert!(text.contains(line), "missing {line:?} in\n{text}");
        }
    }

    #[test]
    fn lower_bound_rows_show_at_least_their_bytes() {
        let mut row = pending(Some(1), Some(0));
        if let Reason::PendingSubvolumeCleanup(p) = &mut row {
            p.lower_bound = true;
        }
        let doc = document(vec![row]);

        let text = render_why_not_freed(&doc);

        assert!(
            text.contains("Deleted snapshots still being cleaned (1)") && text.contains("≥ 0 B"),
            "{text}"
        );
    }

    #[test]
    fn the_table_points_at_watch_free_for_live_changes() {
        let text = render_why_not_freed(&document(vec![pending(Some(0), Some(0))]));

        assert!(
            text.contains(
                "\nFor live free-space changes, run `synoreclaim watch-free /v --duration 10m`.\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn row_labels_fit_their_column() {
        let doc = document(vec![open(None), pending(None, None)]);

        let text = render_why_not_freed(&doc);

        let rows = text
            .lines()
            .skip_while(|l| !l.starts_with("WHY SPACE"))
            .take_while(|l| !l.is_empty());
        for line in rows {
            let label = line.split("  ").next().unwrap();
            assert!(label.chars().count() <= 48, "{line:?}");
        }
    }

    #[test]
    fn unknown_pending_bytes_and_a_skipped_process_scan_say_so() {
        let doc = document(vec![pending(None, None), open(None)]);

        let text = render_why_not_freed(&doc);

        assert!(
            text.contains("Deleted snapshots still being cleaned (unknown)"),
            "{text}"
        );
        assert!(
            text.contains("synoreclaim why-not-freed /v --processes"),
            "{text}"
        );
    }
}
