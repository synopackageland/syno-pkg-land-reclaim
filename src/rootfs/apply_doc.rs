//! The `rootfs apply` answer: what moved, what stayed and why, and the
//! space `/` gained as statfs measured it.

use crate::btrfs::walk::StopReason;
use crate::document::{Completion, Envelope};
use crate::output::{JsonPath, Notice, escape_human, human_bytes};
use crate::rootfs::apply::{Done, Skip};
use crate::rootfs::delegate::{Delegate, ToolOutcome};
use crate::rootfs::plan::RootfsPlan;
use serde::Serialize;
use std::time::SystemTime;

pub struct ApplyInputs<'a> {
    pub plan: &'a RootfsPlan,
    pub plan_file: &'a [u8],
    pub done: &'a Done,
    /// statfs `f_bavail` bytes of `/` before and after the run.
    pub root_available_before: u64,
    pub root_available_after: u64,
    /// The DSM update marker that stopped the run, if one did.
    pub update_marker: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct ApplyDocument {
    #[serde(
        default,
        skip_serializing_if = "crate::rootfs::plan::CleanupMode::is_standard"
    )]
    pub mode: crate::rootfs::plan::CleanupMode,
    #[serde(flatten)]
    pub envelope: Envelope,
    pub plan_id: String,
    pub plan_file: JsonPath,
    pub move_dir: JsonPath,
    pub manifest: JsonPath,
    pub moved: MovedTotals,
    pub skipped: Vec<SkippedItem>,
    pub root: RootDelta,
    /// The planned DSM tools that ran, in order.
    pub tools: Vec<ToolRun>,
    pub notice: Notice,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub follow_up: Vec<crate::rootfs::plan_doc::FollowUp>,
    /// Present when DSM's tools ran.
    pub tools_notice: Option<Notice>,
}

#[derive(Debug, Serialize)]
pub struct ToolRun {
    pub tool: Delegate,
    pub argv: Vec<&'static str>,
    #[serde(flatten)]
    pub outcome: ToolOutcome,
}

#[derive(Debug, Serialize)]
pub struct MovedTotals {
    pub files: u64,
    /// Apparent bytes of the moved files.
    pub size_bytes: u64,
    pub removed_dirs: u64,
}

#[derive(Debug, Serialize)]
pub struct SkippedItem {
    #[serde(flatten)]
    pub path: JsonPath,
    pub reason: Skip,
}

#[derive(Debug, Serialize)]
pub struct RootDelta {
    pub available_before_bytes: u64,
    pub available_after_bytes: u64,
    /// after - before: what the run freed on `/`, as statfs measured it
    /// (other writers on `/` count too).
    pub freed_bytes: i64,
}

pub fn build(inputs: ApplyInputs<'_>, now: SystemTime) -> ApplyDocument {
    let plan = inputs.plan;
    let done = inputs.done;
    let move_dir = plan.move_dir();
    let mut manifest = move_dir.clone();
    manifest.extend_from_slice(b"/manifest.jsonl");
    let mut envelope = Envelope::new("rootfs-apply", now);
    if let Some(marker) = inputs.update_marker {
        envelope.completion = Completion::Paused;
        envelope.warnings.push(Notice::new(
            "dsm_update_active",
            format!(
                "a DSM update started (/{marker} exists), so apply stopped between items; \
                 run the same rootfs apply again once the update is done"
            ),
        ));
    } else if done.stopped == Some(StopReason::DiskLimit) {
        envelope.completion = Completion::Paused;
        envelope.warnings.push(Notice::new(
            "move_folder_full",
            "the move folder's filesystem has no room for the next file; \
             free space there, then run the same rootfs apply again to go on",
        ));
    } else if done.stopped.is_some() {
        envelope.completion = Completion::Paused;
        envelope.warnings.push(Notice::new(
            "apply_stopped",
            "stopped between items; run the same rootfs apply again to go on",
        ));
    } else if done.skipped > 0 || done.tools.iter().any(|(_, outcome)| !outcome.succeeded()) {
        envelope.completion = Completion::Partial;
    }
    ApplyDocument {
        follow_up: crate::rootfs::plan_doc::followups(done.moved_categories.iter().copied(), done.tools.iter().filter(|(_, outcome)| outcome.succeeded()).map(|(tool, _)| *tool)),
        envelope,
        mode: plan.payload.mode,
        plan_id: plan.payload.plan_id.clone(),
        plan_file: JsonPath::new(inputs.plan_file),
        move_dir: JsonPath::new(&move_dir),
        manifest: JsonPath::new(&manifest),
        moved: MovedTotals {
            files: done.moved_files,
            size_bytes: done.moved_size,
            removed_dirs: done.removed_dirs,
        },
        skipped: done
            .skips
            .iter()
            .map(|(path, reason)| SkippedItem {
                path: JsonPath::new(path),
                reason: *reason,
            })
            .collect(),
        root: RootDelta {
            available_before_bytes: inputs.root_available_before,
            available_after_bytes: inputs.root_available_after,
            freed_bytes: signed_delta(inputs.root_available_before, inputs.root_available_after),
        },
        tools: done
            .tools
            .iter()
            .map(|&(tool, outcome)| ToolRun {
                tool,
                argv: tool.argv(),
                outcome,
            })
            .collect(),
        notice: Notice::new(
            "no_restore_command",
            "synoreclaim has no restore command: to put a file back, copy it from the move folder yourself",
        ),
        tools_notice: (!done.tools.is_empty()).then(|| {
            Notice::new(
                "tools_no_undo",
                if plan.payload.mode.is_standard() { "DSM's tools deleted the logs and journals they cleaned up permanently; there is no undo" } else { "DSM's tools permanently deleted the logs, journals or update caches they cleaned up; there is no undo" },
            )
        }),
    }
}

fn signed_delta(before: u64, after: u64) -> i64 {
    if after >= before {
        i64::try_from(after - before).unwrap_or(i64::MAX)
    } else {
        i64::try_from(before - after).map_or(i64::MIN, |d| -d)
    }
}

fn skip_label(skip: Skip) -> &'static str {
    match skip {
        Skip::Missing => "gone since the plan",
        Skip::Changed => "changed since the plan",
        Skip::PathChanged => "a folder on its path changed",
        Skip::HeldOpen => "held open by a process",
        Skip::ChangedDuringCopy => "changed while it was copied",
        Skip::CopyMismatch => "its copy did not verify",
        Skip::CopyExists => "another file has its name in the move folder",
        Skip::UnlinkFailed => "copied, but removing the original failed; both are kept",
        Skip::NoRoom => "no room for it in the move folder",
        Skip::NotEmpty => "folder not empty",
    }
}

fn outcome_label(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Succeeded => "succeeded".into(),
        ToolOutcome::Failed { exit_code } => format!("failed (exit {exit_code})"),
        ToolOutcome::Killed { signal } => format!("killed (signal {signal})"),
        ToolOutcome::TimedOut => "timed out and was killed".into(),
        ToolOutcome::NotStarted => "could not start".into(),
        ToolOutcome::Untrusted => "not run: its program is not root's unwritable executable".into(),
        ToolOutcome::NotNeeded => "not needed any more".into(),
    }
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

pub fn render(doc: &ApplyDocument) -> String {
    let mut out = String::new();
    for action in &doc.follow_up {
        out.push_str(&format!("follow-up: {}\n", action.message()));
    }
    out.push_str(&format!(
        "rootfs apply {}: moved {} ({}) and removed {}\n  into {}\n",
        doc.plan_id,
        plural(doc.moved.files, "file", "files"),
        human_bytes(doc.moved.size_bytes),
        plural(doc.moved.removed_dirs, "folder", "folders"),
        escape_human(&doc.move_dir.bytes())
    ));
    if doc.mode == crate::rootfs::plan::CleanupMode::Deep {
        out.push_str("cleanup mode: deep\n");
    }
    if !doc.skipped.is_empty() {
        out.push_str(&format!(
            "\nleft on / ({}):\n",
            plural(doc.skipped.len() as u64, "item", "items")
        ));
        for item in &doc.skipped {
            out.push_str(&format!(
                "  {}  {}\n",
                escape_human(&item.path.bytes()),
                skip_label(item.reason)
            ));
        }
    }
    if !doc.tools.is_empty() {
        out.push_str("\nDSM's tools:\n");
        for run in &doc.tools {
            out.push_str(&format!(
                "  {}  {}\n",
                run.argv.join(" "),
                outcome_label(run.outcome)
            ));
        }
    }
    let freed = doc.root.freed_bytes;
    let delta = if freed >= 0 {
        format!("{} freed", human_bytes(freed.unsigned_abs()))
    } else {
        format!(
            "{} less (other writers on /)",
            human_bytes(freed.unsigned_abs())
        )
    };
    out.push_str(&format!(
        "\n/ available: {} before, {} after, {delta} (statfs)\n",
        human_bytes(doc.root.available_before_bytes),
        human_bytes(doc.root.available_after_bytes),
    ));
    out.push_str(&format!(
        "{} lists each moved file with its SHA-256; {}\n",
        escape_human(&doc.manifest.bytes()),
        doc.notice
            .message
            .replace("synoreclaim has no", "there is no")
    ));
    for warning in &doc.envelope.warnings {
        out.push_str(&format!("warning: {}\n", warning.message));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rootfs::apply::{Done, Skip};
    use crate::rootfs::plan::RootfsPlan;
    use crate::rootfs::plan::testing::payload;
    use std::time::{Duration, UNIX_EPOCH};

    const PLAN_FILE: &[u8] = b"/var/lib/synoreclaim/rootfs-plans/rootfs-18d75b8423f30000-4d.json";

    #[test]
    fn kb_followups_only_describe_completed_moves_and_successful_tools() {
        let mut done = Done::default();
        assert!(document(&done).follow_up.is_empty());
        done.moved_categories
            .insert(crate::rootfs::classify::Category::PreferenceImage);
        done.moved_categories
            .insert(crate::rootfs::classify::Category::SambaCache);
        done.tools.push((
            crate::rootfs::delegate::Delegate::ReleaseUpdateReserve,
            crate::rootfs::delegate::ToolOutcome::Succeeded,
        ));
        let doc = document(&done);
        assert!(
            doc.follow_up
                .contains(&crate::rootfs::plan_doc::FollowUp::ResetImages)
        );
        assert!(
            doc.follow_up
                .contains(&crate::rootfs::plan_doc::FollowUp::RestartWinbind)
        );
        assert!(
            doc.follow_up
                .contains(&crate::rootfs::plan_doc::FollowUp::UpdateDsm)
        );
    }

    fn done() -> Done {
        Done {
            moved_files: 2,
            moved_categories: Default::default(),
            moved_size: 3,
            moved_allocated_bytes: 12_288,
            removed_dirs: 1,
            skipped: 1,
            skips: vec![(b"/upd@te/c".to_vec(), Skip::Changed)],
            stopped: None,
            tools: Vec::new(),
        }
    }

    fn document(done: &Done) -> ApplyDocument {
        document_with(done, None)
    }

    fn document_with(done: &Done, update_marker: Option<&str>) -> ApplyDocument {
        let plan = RootfsPlan::seal(payload()).unwrap();
        build(
            ApplyInputs {
                plan: &plan,
                plan_file: PLAN_FILE,
                done,
                root_available_before: 1 << 30,
                root_available_after: (1 << 30) + 12_288,
                update_marker,
            },
            UNIX_EPOCH + Duration::from_secs(1_790_000_100),
        )
    }

    #[test]
    fn a_run_a_dsm_update_stopped_is_paused_and_says_so() {
        let mut stopped = done();
        stopped.stopped = Some(StopReason::Paused);

        let doc = document_with(&stopped, Some("var/run/dsmautoupdate.pid"));

        assert_eq!(doc.envelope.completion, Completion::Paused);
        assert_eq!(doc.envelope.warnings[0].code, "dsm_update_active");
        assert!(
            doc.envelope.warnings[0]
                .message
                .contains("/var/run/dsmautoupdate.pid"),
            "{}",
            doc.envelope.warnings[0].message
        );
    }

    #[test]
    fn the_answer_counts_moves_lists_skips_and_measures_the_freed_space() {
        let doc = serde_json::to_value(document(&done())).unwrap();
        assert_eq!(doc["command"], "rootfs-apply");
        assert_eq!(doc["completion"], "partial");
        assert_eq!(doc["plan_id"], "rootfs-18d75b8423f30000-4d");
        assert_eq!(
            doc["move_dir"]["path"],
            "/volume1/backup/rootfs/SynoReclaim-system-20260921-1413"
        );
        assert_eq!(
            doc["manifest"]["path"],
            "/volume1/backup/rootfs/SynoReclaim-system-20260921-1413/manifest.jsonl"
        );
        assert_eq!(doc["moved"]["files"], 2);
        assert_eq!(doc["moved"]["size_bytes"], 3);
        assert_eq!(doc["moved"]["removed_dirs"], 1);
        assert_eq!(doc["skipped"][0]["path"], "/upd@te/c");
        assert_eq!(doc["skipped"][0]["reason"], "changed");
        assert_eq!(doc["root"]["freed_bytes"], 12_288);
        assert_eq!(doc["notice"]["code"], "no_restore_command");
    }

    #[test]
    fn the_synthetic_answer_is_the_json_golden() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/json/rootfs-apply.json");
        let mut ran = done();
        ran.tools = vec![
            (
                crate::rootfs::delegate::Delegate::RotateLogs,
                crate::rootfs::delegate::ToolOutcome::Succeeded,
            ),
            (
                crate::rootfs::delegate::Delegate::DeleteJournal,
                crate::rootfs::delegate::ToolOutcome::Failed { exit_code: 1 },
            ),
        ];
        let mut rendered = serde_json::to_value(document(&ran)).unwrap();
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        rendered["tool_version"] = golden["tool_version"].clone();
        assert_eq!(rendered, golden);
    }

    #[test]
    fn a_clean_run_is_complete_and_a_stopped_one_paused() {
        let mut clean = done();
        clean.skipped = 0;
        clean.skips.clear();
        assert_eq!(document(&clean).envelope.completion, Completion::Complete);
        clean.stopped = Some(StopReason::Paused);
        let stopped = document(&clean);
        assert_eq!(stopped.envelope.completion, Completion::Paused);
        assert_eq!(stopped.envelope.warnings[0].code, "apply_stopped");
        clean.stopped = Some(StopReason::DiskLimit);
        let full = document(&clean);
        assert_eq!(full.envelope.completion, Completion::Paused);
        assert_eq!(full.envelope.warnings[0].code, "move_folder_full");
    }

    #[test]
    fn each_dsm_tool_run_is_reported_and_a_failed_one_makes_the_run_partial() {
        use crate::rootfs::delegate::{Delegate, ToolOutcome};
        let mut ran = done();
        ran.skipped = 0;
        ran.skips.clear();
        ran.tools = vec![
            (Delegate::RotateLogs, ToolOutcome::Succeeded),
            (
                Delegate::DeleteJournal,
                ToolOutcome::Failed { exit_code: 1 },
            ),
        ];
        let doc = document(&ran);
        assert_eq!(doc.envelope.completion, Completion::Partial);
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            json["tools"][1],
            serde_json::json!({
                "tool": "delete_journal",
                "argv": ["/usr/syno/bin/synocleaner", "--delete-journal"],
                "outcome": "failed",
                "exit_code": 1
            })
        );
        assert_eq!(json["tools_notice"]["code"], "tools_no_undo");
        let text = render(&doc);
        assert!(
            text.contains(
                "\nDSM's tools:\n  /usr/syno/bin/synocleaner --rotate-log  succeeded\n  \
                 /usr/syno/bin/synocleaner --delete-journal  failed (exit 1)\n"
            ),
            "{text}"
        );

        ran.tools = vec![(Delegate::RestartSyslogNg, ToolOutcome::NotNeeded)];
        let doc = document(&ran);
        assert_eq!(doc.envelope.completion, Completion::Complete);
        assert!(render(&doc).contains("syslog-ng  not needed any more\n"));
        let json = serde_json::to_value(document(&done())).unwrap();
        assert_eq!(json["tools"], serde_json::json!([]));
        assert_eq!(json["tools_notice"], serde_json::Value::Null);
    }

    #[test]
    fn the_human_answer_says_what_moved_what_stayed_and_what_was_freed() {
        let text = render(&document(&done()));
        let expected = [
            "rootfs apply rootfs-18d75b8423f30000-4d: moved 2 files (3 B) and removed 1 folder\n",
            "  into /volume1/backup/rootfs/SynoReclaim-system-20260921-1413\n",
            "left on / (1 item):\n",
            "  /upd@te/c  changed since the plan\n",
            "/ available: 1 GB before, 1 GB after, 12 KB freed (statfs)\n",
            "manifest.jsonl lists each moved file with its SHA-256",
            "no restore command",
        ];
        let mut rest = text.as_str();
        for part in expected {
            let at = rest
                .find(part)
                .unwrap_or_else(|| panic!("{part:?} not in order in:\n{text}"));
            rest = &rest[at + part.len()..];
        }
    }
}
