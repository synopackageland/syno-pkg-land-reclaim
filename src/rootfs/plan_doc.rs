//! The `rootfs plan` answer: what apply will move, where to, what it
//! frees, and what stays on `/`.

use crate::document::{Completion, Envelope, shell_word};
use crate::output::{JsonPath, Notice, escape_human, human_bytes};
use crate::rootfs::classify::{Category, Kind};
use crate::rootfs::delegate::Delegate;
use crate::rootfs::plan::{DESTINATION_MARGIN_BYTES, Held, Kept, KeptReason, RootfsPlan};
use crate::rootfs::report::{Cleanable, cleanable, render_category_totals};
use crate::rootfs::walk::WalkStop;
use crate::time::rfc3339;
use serde::Serialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Items the human plan lists one by one; JSON lists them all.
const HUMAN_ITEMS: usize = 20;

pub struct PlanInputs<'a> {
    pub plan: &'a RootfsPlan,
    /// Where the plan was written; `None` when it has nothing to move.
    pub plan_file: Option<&'a [u8]>,
    pub held: &'a [Held],
    pub kept: &'a [Kept],
    pub root_available: u64,
    pub destination_available: u64,
    pub walk_stop: Option<WalkStop>,
}

#[derive(Debug, Serialize)]
pub struct PlanDocument {
    #[serde(
        default,
        skip_serializing_if = "crate::rootfs::plan::CleanupMode::is_standard"
    )]
    pub mode: crate::rootfs::plan::CleanupMode,
    #[serde(flatten)]
    pub envelope: Envelope,
    pub plan_file: Option<JsonPath>,
    pub plan_id: Option<String>,
    pub expires_at: Option<String>,
    pub destination: DestinationJson,
    pub root: RootSpace,
    pub moves: Cleanable,
    /// Additional allocated movable bytes compared with standard mode.
    pub additional_bytes: u64,
    pub items: Vec<ItemRow>,
    pub held_open: Vec<HeldRow>,
    /// Movable files left on `/` by their size or age.
    pub kept: Vec<KeptRow>,
    /// DSM's own tools apply runs after the moves, in order.
    pub tools: Vec<ToolRow>,
    pub apply: Option<Apply>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub follow_up: Vec<FollowUp>,
}

/// Manual actions only: no service restart or reboot is added to execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUp {
    RefreshGeoip,
    ResetImages,
    RestartWinbind,
    DownloadUpdate,
    UpdateDsm,
    BackupInstaller,
}

pub fn followups(
    categories: impl IntoIterator<Item = Category>,
    tools: impl IntoIterator<Item = Delegate>,
) -> Vec<FollowUp> {
    let mut actions = std::collections::BTreeSet::new();
    for category in categories {
        match category {
            Category::GeoipDatabase => {
                actions.insert(FollowUp::RefreshGeoip);
            }
            Category::PreferenceImage => {
                actions.insert(FollowUp::ResetImages);
            }
            Category::SambaCache => {
                actions.insert(FollowUp::RestartWinbind);
            }
            Category::BackupInstaller => {
                actions.insert(FollowUp::BackupInstaller);
            }
            _ => {}
        }
    }
    for tool in tools {
        match tool {
            Delegate::CleanDownloadedUpdate => {
                actions.insert(FollowUp::DownloadUpdate);
            }
            Delegate::ReleaseUpdateReserve => {
                actions.insert(FollowUp::UpdateDsm);
            }
            _ => {}
        }
    }
    actions.into_iter().collect()
}

impl FollowUp {
    pub fn message(self) -> &'static str {
        match self {
            Self::RefreshGeoip => {
                "Check location-based firewall rules; ask Synology support to download the GeoIP database again before relying on them."
            }
            Self::ResetImages => {
                "Users may need to upload and select their desktop wallpaper or login background again; settings files were kept."
            }
            Self::RestartWinbind => {
                "Restart pkg-synosamba-winbindd manually on DSM 7 to rebuild the Winbind cache (DSM 6: restart Samba); domain access may be interrupted. Reclaim does not restart it."
            }
            Self::DownloadUpdate => {
                "Download the DSM update again in Control Panel before updating."
            }
            Self::UpdateDsm => {
                "Update DSM and packages soon. Restarting DSM may recreate the update reserve; a restart is not required by this cleanup. Reclaim does not reboot the NAS."
            }
            Self::BackupInstaller => {
                "Update DSM now. DSM recreates Hyper Backup's restore installer after a successful update; until then Hyper Backup cannot restore the entire system unless you copy the moved file back."
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ToolRow {
    pub tool: Delegate,
    pub argv: Vec<&'static str>,
    pub effect: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct DestinationJson {
    #[serde(flatten)]
    pub path: JsonPath,
    pub share: JsonPath,
    /// The folder apply creates and moves the items into.
    pub move_dir: JsonPath,
    pub available_bytes: u64,
    /// The moved bytes plus the margin apply keeps free.
    pub required_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct RootSpace {
    pub available_bytes: u64,
    /// Estimated: available plus the moved items' allocated bytes.
    pub available_after_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ItemRow {
    #[serde(flatten)]
    pub path: JsonPath,
    pub kind: Kind,
    pub category: Category,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct HeldRow {
    #[serde(flatten)]
    pub path: JsonPath,
    pub bytes: u64,
    pub holder: String,
}

#[derive(Debug, Serialize)]
pub struct KeptRow {
    #[serde(flatten)]
    pub path: JsonPath,
    pub bytes: u64,
    pub reason: KeptReason,
}

#[derive(Debug, Serialize)]
pub struct Apply {
    pub command: Vec<String>,
    pub notice: Notice,
    /// Present when the plan runs DSM's tools.
    pub tools_notice: Option<Notice>,
}

pub fn build(inputs: PlanInputs<'_>, now: SystemTime) -> PlanDocument {
    let plan = inputs.plan;
    let payload = &plan.payload;
    let bytes = plan.bytes();
    let mut envelope = Envelope::new("rootfs-plan", now);
    if inputs.walk_stop.is_some() {
        envelope.completion = Completion::Partial;
        envelope.warnings.push(Notice::new(
            "walk_incomplete",
            "the walk of / stopped early, so this plan may miss items; run rootfs plan again later",
        ));
    }
    let written = inputs.plan_file.is_some();
    PlanDocument {
        follow_up: followups(payload.items.iter().map(|item| item.category), payload.delegates.iter().copied()),
        envelope,
        mode: payload.mode,
        plan_file: inputs.plan_file.map(JsonPath::new),
        plan_id: written.then(|| payload.plan_id.clone()),
        expires_at: written
            .then(|| rfc3339(UNIX_EPOCH + Duration::from_secs(payload.expires_at))),
        destination: DestinationJson {
            path: JsonPath::new(&payload.destination.path),
            share: JsonPath::new(&payload.destination.share),
            move_dir: JsonPath::new(&plan.move_dir()),
            available_bytes: inputs.destination_available,
            required_bytes: bytes.saturating_add(DESTINATION_MARGIN_BYTES),
        },
        root: RootSpace {
            available_bytes: inputs.root_available,
            available_after_bytes: inputs.root_available.saturating_add(bytes),
        },
        moves: cleanable(&payload.items),
        additional_bytes: {
            let standard_candidates: Vec<_> = payload.items.iter().filter(|item| crate::rootfs::classify::classify(&item.path, item.kind).movable).cloned().collect();
            let standard = crate::rootfs::plan::select(&standard_candidates, &std::collections::BTreeMap::new(), i64::try_from(payload.created_at).unwrap_or(i64::MAX), i64::MAX);
            cleanable(&payload.items).bytes.saturating_sub(cleanable(&standard.items).bytes)
        },
        items: payload
            .items
            .iter()
            .map(|item| ItemRow {
                path: JsonPath::new(&item.path),
                kind: item.kind,
                category: item.category,
                bytes: item.allocated_bytes,
            })
            .collect(),
        held_open: inputs
            .held
            .iter()
            .map(|held| HeldRow {
                path: JsonPath::new(&held.path),
                bytes: held.bytes,
                holder: String::from_utf8_lossy(&held.holder).into_owned(),
            })
            .collect(),
        kept: inputs
            .kept
            .iter()
            .map(|kept| KeptRow {
                path: JsonPath::new(&kept.path),
                bytes: kept.bytes,
                reason: kept.reason,
            })
            .collect(),
        tools: payload
            .delegates
            .iter()
            .map(|&tool| ToolRow {
                tool,
                argv: tool.argv(),
                effect: tool.effect(),
                estimated_bytes: payload.tool_estimates.get(&tool).copied(),
            })
            .collect(),
        apply: inputs.plan_file.map(|file| Apply {
            command: ["synoreclaim", "rootfs", "apply"]
                .into_iter()
                .map(String::from)
                .chain([String::from_utf8_lossy(file).into_owned(), "--yes".into()])
                .collect(),
            notice: Notice::new(
                "no_restore_command",
                format!(
                    "apply copies each item into {}, checks its size and SHA-256, then removes it from /; \
                     synoreclaim has no restore command: copy files back yourself if you need them",
                    escape_human(&plan.move_dir())
                ),
            ),
            tools_notice: (!payload.delegates.is_empty()).then(|| {
                Notice::new(
                    "tools_no_undo",
                    if payload.mode.is_standard() { "DSM's tools delete the logs and journals they clean up permanently; there is no undo" } else { "DSM's tools permanently delete the logs, journals or update caches they clean up, old logs included; there is no undo" },
                )
            }),
        }),
    }
}

pub fn render(doc: &PlanDocument) -> String {
    let mut out = String::new();
    if doc.mode == crate::rootfs::plan::CleanupMode::Deep {
        out.push_str(&format!(
            "cleanup mode: deep; additional movable space: {}\n",
            human_bytes(doc.additional_bytes)
        ));
        out.push_str("KB caches and image resources may require manual follow-up; forced rotation may delete old logs\n");
    }
    if let (Some(id), Some(expires)) = (&doc.plan_id, &doc.expires_at) {
        out.push_str(&format!("rootfs plan {id}, valid until {expires}\n"));
    }
    match (doc.items.is_empty(), doc.plan_file.is_some()) {
        (false, _) => render_moves(doc, &mut out),
        (true, true) => out.push_str("nothing to move off /\n"),
        (true, false) => out.push_str("rootfs plan: nothing to move off /; no plan was written\n"),
    }
    for action in &doc.follow_up {
        out.push_str(&format!("follow-up: {}\n", action.message()));
    }
    render_tools(doc, &mut out);
    render_space(doc, &mut out);
    render_held(doc, &mut out);
    render_kept(doc, &mut out);
    if let (Some(apply), Some(file)) = (&doc.apply, &doc.plan_file) {
        out.push_str(&format!("\n{}\n", apply.notice.message));
        if let Some(notice) = &apply.tools_notice {
            out.push_str(&format!("{}\n", notice.message));
        }
        out.push_str(&format!(
            "to apply: synoreclaim rootfs apply {} --yes\n",
            shell_word(&file.bytes())
        ));
    }
    for warning in &doc.envelope.warnings {
        out.push_str(&format!("warning: {}\n", warning.message));
    }
    out
}

fn render_moves(doc: &PlanDocument, out: &mut String) {
    out.push_str(&format!(
        "move {} in {} items off / into {}\n",
        human_bytes(doc.moves.bytes),
        doc.moves.items,
        escape_human(&doc.destination.move_dir.bytes())
    ));
    render_category_totals(&doc.moves, out);
    for item in doc.items.iter().take(HUMAN_ITEMS) {
        let slash = if item.kind == Kind::Dir { "/" } else { "" };
        out.push_str(&format!(
            "  {:>9}  {}{slash}\n",
            human_bytes(item.bytes),
            escape_human(&item.path.bytes())
        ));
    }
    if doc.items.len() > HUMAN_ITEMS {
        out.push_str(&format!(
            "  ... and {} more (--json lists them all)\n",
            doc.items.len() - HUMAN_ITEMS
        ));
    }
}

fn render_tools(doc: &PlanDocument, out: &mut String) {
    if doc.tools.is_empty() {
        return;
    }
    out.push_str("then run DSM's own tools:\n");
    for tool in &doc.tools {
        out.push_str(&format!("  {}  ({})\n", tool.argv.join(" "), tool.effect));
    }
}

fn render_space(doc: &PlanDocument, out: &mut String) {
    out.push_str(&format!(
        "\n/ has {} available, about {} after apply\n",
        human_bytes(doc.root.available_bytes),
        human_bytes(doc.root.available_after_bytes)
    ));
    out.push_str(&format!(
        "the destination has {} available; this plan needs {} with the margin\n",
        human_bytes(doc.destination.available_bytes),
        human_bytes(doc.destination.required_bytes)
    ));
}

fn render_held(doc: &PlanDocument, out: &mut String) {
    if doc.held_open.is_empty() {
        return;
    }
    out.push_str("\nleft on / because a process holds them open:\n");
    for held in &doc.held_open {
        out.push_str(&format!(
            "  {:>9}  {}  ({})\n",
            human_bytes(held.bytes),
            escape_human(&held.path.bytes()),
            escape_human(held.holder.as_bytes())
        ));
    }
}

fn render_kept(doc: &PlanDocument, out: &mut String) {
    if doc.kept.is_empty() {
        return;
    }
    out.push_str("\nleft on / for support:\n");
    for kept in &doc.kept {
        let why = match kept.reason {
            KeptReason::RecentLogArchive => "a log archive from the last 7 days",
            KeptReason::UnrecognisedImage => {
                "named image resource has no recognized image signature"
            }
            KeptReason::SmallJuniorLog => "a junior-mode log under 1 MB",
            KeptReason::UpdatedSinceBoot => {
                "an update leftover changed since the last boot (an update may wait for the restart)"
            }
            KeptReason::TooLarge => "larger than 8 GB",
            KeptReason::UpdateRoomEnough => {
                "Hyper Backup needs it to restore the entire system; / has room for a DSM update without it"
            }
            KeptReason::AppendOnly => "append-only; the kernel refuses to remove it",
            KeptReason::Immutable => "immutable; the kernel refuses to remove it",
        };
        out.push_str(&format!(
            "  {:>9}  {}  ({why})\n",
            human_bytes(kept.bytes),
            escape_human(&kept.path.bytes())
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rootfs::plan::testing::payload;
    use crate::rootfs::plan::{DESTINATION_MARGIN_BYTES, Held, Kept, KeptReason, RootfsPlan};
    use crate::rootfs::walk::WalkStop;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn moving_the_backup_installer_says_how_it_comes_back_and_the_delete_log_run_has_none() {
        assert_eq!(
            followups(
                [Category::BackupInstaller, Category::ToolCache],
                [Delegate::DeleteLog]
            ),
            [FollowUp::BackupInstaller]
        );
        assert!(
            FollowUp::BackupInstaller
                .message()
                .contains("entire system")
        );
    }

    #[test]
    fn deep_preview_reports_only_the_additional_archives_allocated_bytes() {
        use crate::rootfs::plan::{
            CleanupMode,
            testing::{candidate, payload},
        };
        let mut payload = payload();
        payload.mode = CleanupMode::Deep;
        let mut log = candidate("/var/log/messages.1.xz", Kind::File, 5);
        log.category = Category::LogArchive;
        log.allocated_bytes = 8192;
        log.identity.mtime = payload.created_at as i64;
        payload.items.push(log);
        let plan = RootfsPlan::seal(payload).unwrap();
        let doc = build(
            PlanInputs {
                plan: &plan,
                plan_file: None,
                held: &[],
                kept: &[],
                root_available: 0,
                destination_available: 1 << 30,
                walk_stop: None,
            },
            UNIX_EPOCH,
        );
        assert_eq!(doc.mode, CleanupMode::Deep);
        assert_eq!(doc.additional_bytes, 8192);
    }

    const PLAN_FILE: &[u8] = b"/var/lib/synoreclaim/rootfs-plans/rootfs-18d75b8423f30000-4d.json";

    fn held() -> Vec<Held> {
        vec![Held {
            path: b"/upd@te/b".to_vec(),
            bytes: 4096,
            holder: b"synoupgrade".to_vec(),
        }]
    }

    fn kept() -> Vec<Kept> {
        vec![Kept {
            path: b"/var/log/messages.1.xz".to_vec(),
            bytes: 4096,
            reason: KeptReason::RecentLogArchive,
        }]
    }

    fn inputs<'a>(plan: &'a RootfsPlan, held: &'a [Held]) -> PlanInputs<'a> {
        PlanInputs {
            plan,
            plan_file: Some(PLAN_FILE),
            held,
            kept: &[],
            root_available: 1 << 30,
            destination_available: 5 << 40,
            walk_stop: None,
        }
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_790_000_000)
    }

    #[test]
    fn the_plan_says_what_moves_where_and_what_it_frees() {
        let plan = RootfsPlan::seal(payload()).unwrap();
        let (held, kept) = (held(), kept());
        let with_kept = PlanInputs {
            kept: &kept,
            ..inputs(&plan, &held)
        };
        let doc = serde_json::to_value(build(with_kept, now())).unwrap();
        assert_eq!(doc["command"], "rootfs-plan");
        assert_eq!(doc["completion"], "complete");
        assert_eq!(doc["plan_id"], "rootfs-18d75b8423f30000-4d");
        assert_eq!(
            doc["plan_file"]["path"],
            std::str::from_utf8(PLAN_FILE).unwrap()
        );
        assert_eq!(doc["expires_at"], "2026-09-22T14:13:20Z");
        let destination = &doc["destination"];
        assert_eq!(destination["path"], "/volume1/backup/rootfs");
        assert_eq!(destination["share"]["path"], "/volume1/backup");
        assert_eq!(
            destination["move_dir"]["path"],
            "/volume1/backup/rootfs/SynoReclaim-system-20260921-1413"
        );
        assert_eq!(
            destination["required_bytes"],
            8192 + DESTINATION_MARGIN_BYTES
        );
        assert_eq!(doc["root"]["available_bytes"], 1u64 << 30);
        assert_eq!(doc["root"]["available_after_bytes"], (1u64 << 30) + 8192);
        assert_eq!(doc["moves"]["bytes"], 8192);
        assert_eq!(
            doc["moves"]["by_category"][0]["category"],
            "update_leftover"
        );
        assert_eq!(doc["items"][1]["path"], "/upd@te/old");
        assert_eq!(doc["items"][1]["kind"], "dir");
        assert_eq!(doc["held_open"][0]["holder"], "synoupgrade");
        assert_eq!(doc["kept"][0]["path"], "/var/log/messages.1.xz");
        assert_eq!(doc["kept"][0]["reason"], "recent_log_archive");
        assert_eq!(
            doc["apply"]["command"],
            serde_json::json!(["synoreclaim", "rootfs", "apply", PLAN_FILE_STR, "--yes"])
        );
        assert_eq!(doc["apply"]["notice"]["code"], "no_restore_command");
    }

    #[test]
    fn dsm_tool_runs_are_listed_with_their_argv_and_no_undo() {
        use crate::rootfs::delegate::Delegate;
        let mut runs = payload();
        runs.delegates = vec![Delegate::RotateLogs, Delegate::RestartSyslogNg];
        let plan = RootfsPlan::seal(runs).unwrap();
        let doc = build(inputs(&plan, &[]), now());
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["tools"][0]["tool"], "rotate_logs");
        assert_eq!(
            json["tools"][0]["argv"],
            serde_json::json!(["/usr/syno/bin/synocleaner", "--rotate-log"])
        );
        assert_eq!(json["tools"][1]["tool"], "restart_syslog_ng");
        assert_eq!(json["apply"]["tools_notice"]["code"], "tools_no_undo");
        let text = render(&doc);
        assert!(
            text.contains("then run DSM's own tools:\n  /usr/syno/bin/synocleaner --rotate-log  "),
            "{text}"
        );
        assert!(text.contains("there is no undo"), "{text}");

        let plan = RootfsPlan::seal(payload()).unwrap();
        let json = serde_json::to_value(build(inputs(&plan, &[]), now())).unwrap();
        assert_eq!(json["tools"], serde_json::json!([]));
        assert_eq!(json["apply"]["tools_notice"], serde_json::Value::Null);
    }

    #[test]
    fn a_plan_with_only_tools_to_run_moves_nothing_but_is_written() {
        use crate::rootfs::delegate::Delegate;
        let mut runs = payload();
        runs.items.clear();
        runs.delegates = vec![Delegate::Logrotate];
        let plan = RootfsPlan::seal(runs).unwrap();
        let text = render(&build(inputs(&plan, &[]), now()));
        assert!(
            text.starts_with(
                "rootfs plan rootfs-18d75b8423f30000-4d, valid until 2026-09-22T14:13:20Z\n\
                 nothing to move off /\n\
                 then run DSM's own tools:\n  /usr/bin/logrotate -f /etc/logrotate.conf  ("
            ),
            "{text}"
        );
        assert!(
            text.contains("to apply: synoreclaim rootfs apply"),
            "{text}"
        );
    }

    const PLAN_FILE_STR: &str = "/var/lib/synoreclaim/rootfs-plans/rootfs-18d75b8423f30000-4d.json";

    #[test]
    fn the_synthetic_plan_is_the_json_golden() {
        let mut runs = payload();
        runs.delegates = vec![
            crate::rootfs::delegate::Delegate::RotateLogs,
            crate::rootfs::delegate::Delegate::DeleteJournal,
        ];
        let plan = RootfsPlan::seal(runs).unwrap();
        let (held, kept) = (held(), kept());
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/json/rootfs-plan.json");
        let with_kept = PlanInputs {
            kept: &kept,
            ..inputs(&plan, &held)
        };
        let mut rendered = serde_json::to_value(build(with_kept, now())).unwrap();
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        rendered["tool_version"] = golden["tool_version"].clone();
        assert_eq!(rendered, golden);
    }

    #[test]
    fn with_nothing_to_move_no_plan_is_written() {
        let mut nothing = payload();
        nothing.items.clear();
        let plan = RootfsPlan::seal(nothing).unwrap();
        let mut empty = inputs(&plan, &[]);
        empty.plan_file = None;
        let doc = build(empty, now());
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["plan_file"], serde_json::Value::Null);
        assert_eq!(json["plan_id"], serde_json::Value::Null);
        assert_eq!(json["apply"], serde_json::Value::Null);
        assert_eq!(json["moves"]["items"], 0);
        assert!(
            render(&doc).contains("nothing to move off /"),
            "{}",
            render(&doc)
        );
    }

    #[test]
    fn a_walk_cut_short_warns_that_the_plan_may_miss_items() {
        let plan = RootfsPlan::seal(payload()).unwrap();
        let mut cut = inputs(&plan, &[]);
        cut.walk_stop = Some(WalkStop::EntryLimit);
        let doc = serde_json::to_value(build(cut, now())).unwrap();
        assert_eq!(doc["completion"], "partial");
        assert_eq!(doc["warnings"][0]["code"], "walk_incomplete");
    }

    #[test]
    fn the_human_plan_lists_moves_then_space_then_what_stays_then_how_to_apply() {
        let plan = RootfsPlan::seal(payload()).unwrap();
        let (held, kept) = (held(), kept());
        let text = render(&build(
            PlanInputs {
                kept: &kept,
                ..inputs(&plan, &held)
            },
            now(),
        ));
        let expected = [
            "rootfs plan rootfs-18d75b8423f30000-4d, valid until 2026-09-22T14:13:20Z\n",
            "move 8 KB in 2 items off / into /volume1/backup/rootfs/SynoReclaim-system-20260921-1413\n",
            "  update leftovers            8 KB  2 items\n",
            "       4 KB  /upd@te/a\n",
            "       4 KB  /upd@te/old/\n",
            "/ has 1 GB available, about 1 GB after apply\n",
            "the destination has 5 TB available; this plan needs 256 MB with the margin\n",
            "left on / because a process holds them open:\n",
            "       4 KB  /upd@te/b  (synoupgrade)\n",
            "left on / for support:\n",
            "       4 KB  /var/log/messages.1.xz  (a log archive from the last 7 days)\n",
            "no restore command",
            "to apply: synoreclaim rootfs apply /var/lib/synoreclaim/rootfs-plans/rootfs-18d75b8423f30000-4d.json --yes\n",
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
