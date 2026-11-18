//! JSON and human output for `overview` (SPEC 5.13).

use super::{Envelope, ReportInfo, notices, report_line, shell_word, signed_bytes};
use crate::output::{JsonPath, escape_human, human_bytes};
use crate::overview::{
    DirectOwnerRootClass, DirectOwnerRootKind, DirectOwnerSubcase, PartKind, PinnedRangesSubcase,
    UnknownReason,
};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub results: Vec<OverviewResult>,
    pub scans: Vec<super::FilesystemScan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewResult {
    pub report: ReportInfo,
    pub filesystem: OverviewFilesystem,
    pub space: OverviewSpace,
    pub parts: Vec<OverviewPart>,
    pub reconciliation: OverviewReconciliation,
    pub actions: Vec<OverviewAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AttributionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttributionInfo {
    pub read_budget: Option<crate::btrfs::read_budget::ReadBudget>,
    pub deferred_bytes: u64,
    pub analysis: Option<crate::scan::attribution::Measurement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewFilesystem {
    pub uuid: String,
    pub mount: JsonPath,
    pub kind: crate::filesystem::FilesystemKind,
    pub capabilities: crate::filesystem::Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewSpace {
    pub df: Option<OverviewDf>,
    pub space_info: Option<Vec<OverviewSpaceInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode_allocated_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unattributed_df_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_info_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OverviewDf {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewSpaceInfo {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub profile: &'static str,
    pub copies: Option<u8>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub captured: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_bytes_at_scan_start: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewPart {
    pub kind: PartKind,
    pub bytes: u64,
    pub reclaimable_bytes: Option<u64>,
    pub held_bytes: Option<u64>,
    pub items: Vec<OverviewItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<OverviewReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_by: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manage_in: Option<&'static str>,
    /// Stable id of that DSM place (SPEC 4.6, GAP-14); null with `manage_in`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manage_in_id: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<&'static str>,
    pub bytes: u64,
    pub reclaimable_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewReason {
    pub code: UnknownReason,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subcases: Vec<OverviewReasonSubcase>,
}

/// The subcase code of one unknown reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SubcaseCode {
    DirectOwner(DirectOwnerSubcase),
    PinnedRanges(PinnedRangesSubcase),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewReasonSubcase {
    pub code: SubcaseCode,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<OverviewReasonRoot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OverviewReasonRoot {
    pub root_id: u64,
    pub kind: DirectOwnerRootKind,
    pub class: DirectOwnerRootClass,
    pub at_directory: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewReconciliation {
    pub data: DataReconciliation,
    pub df: DfReconciliation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataReconciliation {
    pub parts_sum_bytes: u64,
    pub space_info_used_bytes: Option<u64>,
    pub residual_bytes: Option<i64>,
    pub tolerance_bytes: Option<u64>,
    pub status: &'static str,
    pub coverage_gaps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DfReconciliation {
    pub used_bytes: Option<u64>,
    pub metadata_system_bytes: Option<u64>,
    pub data_metadata_system_bytes: Option<u64>,
    pub residual_bytes: Option<i64>,
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewAction {
    pub rank: usize,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<&'static str>,
    pub reclaimable_bytes: Option<u64>,
    pub held_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manage_in: Option<&'static str>,
    /// Stable id of that DSM place (SPEC 4.6, GAP-14); null with `manage_in`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manage_in_id: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<JsonPath>,
    /// What `path` is, as top.list names it ("file", "dir"); cleanup actions only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

pub fn render_overview(doc: &OverviewDocument) -> String {
    let mut out = String::new();
    for result in &doc.results {
        let mount = super::escape_path(&result.filesystem.mount);
        match result.space.df {
            Some(df) => out.push_str(&format!(
                "{mount}  Btrfs  {} total, {} used (df), {} free\n",
                human_bytes(df.total_bytes),
                human_bytes(df.used_bytes),
                human_bytes(df.free_bytes)
            )),
            None => out.push_str(&format!("{mount}  Btrfs  df space unknown\n")),
        }
        out.push_str(&format!("report {}\n\n", report_line(&result.report)));
        out.push_str("WHERE THE USED SPACE IS\n");
        out.push_str(&format!(
            "{:<43} {:>12}  {:>12}  {}\n",
            "PART", "BYTES", "CAN FREE", "WHERE TO FIX"
        ));
        for part in &result.parts {
            render_part(&mut out, part);
        }
        out.push_str(&format!(
            "File data, sum of the above                 {}\n",
            human_bytes(result.reconciliation.data.parts_sum_bytes)
        ));
        render_data_reconciliation(&mut out, &result.reconciliation.data);
        render_df_reconciliation(&mut out, &result.reconciliation.df);
        out.push_str("\nTOP ACTIONS\n");
        if result.actions.is_empty() {
            out.push_str("No actionable amounts were proven by this report.\n");
        }
        for action in &result.actions {
            render_action(&mut out, action);
        }
        out.push('\n');
    }
    for fs in &doc.scans {
        super::render_filesystem(&mut out, fs);
    }
    notices(&mut out, &doc.envelope);
    out
}

fn render_part(out: &mut String, part: &OverviewPart) {
    let label = part_label(part.kind);
    let can_free = match part.kind {
        PartKind::PendingCleanup => "frees by itself".to_string(),
        PartKind::SnapshotOnly => "held".to_string(),
        PartKind::RecycleBin | PartKind::ShareLive => part
            .reclaimable_bytes
            .map_or_else(|| "unknown".to_string(), human_bytes),
        PartKind::Unknown => "unknown".to_string(),
        PartKind::DsmService
        | PartKind::SystemOther
        | PartKind::SharedBetweenFolders
        | PartKind::PinnedUnreferenced => "—".to_string(),
    };
    out.push_str(&format!(
        "{label:<43} {:>12}  {:>12}  {}\n",
        human_bytes(part.bytes),
        can_free,
        where_to_fix(part.kind)
    ));
    let mut items = part.items.clone();
    items.sort_by_key(|item| std::cmp::Reverse(item.bytes));
    for item in items.iter().take(3) {
        let name = item_label(item);
        let reclaimable = item.reclaimable_bytes.map_or("—".to_string(), human_bytes);
        let detail = match (item.manage_in, item.explanation) {
            (Some(place), Some(explanation)) => format!(" — {place}; {explanation}"),
            (Some(place), None) => format!(" — {place}"),
            (None, Some(explanation)) => format!(" — {explanation}"),
            (None, None) => String::new(),
        };
        out.push_str(&format!(
            "  {name:<41} {:>12}  {:>12}{detail}\n",
            human_bytes(item.bytes),
            reclaimable
        ));
    }
    if items.len() > 3 {
        let remaining = &items[3..];
        let count = remaining.len();
        let bytes = remaining.iter().map(|item| item.bytes).sum();
        out.push_str(&format!(
            "  … {count} more                                  {:>12}\n",
            human_bytes(bytes)
        ));
    }
    for reason in &part.reasons {
        out.push_str(&format!(
            "  {}                                      {:>12}\n",
            reason.code_name(),
            human_bytes(reason.bytes)
        ));
    }
}

fn part_label(kind: PartKind) -> &'static str {
    match kind {
        PartKind::PendingCleanup => "Deleted, waiting for Btrfs cleanup",
        PartKind::SnapshotOnly => "Held only by snapshots",
        PartKind::RecycleBin => "Recycle bins (#recycle)",
        PartKind::DsmService => "DSM services",
        PartKind::SystemOther => "Other volume data",
        PartKind::ShareLive => "Shared folders (live data)",
        PartKind::SharedBetweenFolders => "Shared between folders (reflinks)",
        PartKind::PinnedUnreferenced => "Allocated but unused parts of extents",
        PartKind::Unknown => "Unknown (references not resolved)",
    }
}

fn item_label(item: &OverviewItem) -> String {
    if let Some(label) = &item.label {
        return escape_human(label.as_bytes());
    }
    match (&item.share, item.managed_by, item.service) {
        (_, Some(package), _) => escape_human(package.as_bytes()),
        (Some(share), _, _) => escape_human(share.as_bytes()),
        (_, _, Some(service)) => escape_human(service.as_bytes()),
        _ => "unknown owner".to_string(),
    }
}

fn where_to_fix(kind: PartKind) -> &'static str {
    match kind {
        PartKind::PendingCleanup => "wait for Btrfs cleanup",
        PartKind::SnapshotOnly => "Snapshot Replication",
        PartKind::RecycleBin => "DSM Recycle Bin settings",
        PartKind::DsmService => "manage in DSM packages below",
        PartKind::SystemOther => "inspect the volume-root paths",
        PartKind::ShareLive => "synoreclaim cleanup delete",
        PartKind::SharedBetweenFolders => "synoreclaim shared",
        PartKind::PinnedUnreferenced => "synoreclaim inspect --explain",
        PartKind::Unknown => "coverage gaps",
    }
}

fn render_data_reconciliation(out: &mut String, data: &DataReconciliation) {
    let used = data
        .space_info_used_bytes
        .map_or("unknown".to_string(), human_bytes);
    let residual = data
        .residual_bytes
        .map_or("unknown".to_string(), signed_bytes);
    let tolerance = data
        .tolerance_bytes
        .map_or("unknown".to_string(), human_bytes);
    let status = if data.coverage_gaps {
        "not reconciled: coverage gaps"
    } else {
        match data.status {
            "reconciled" => "reconciled",
            "not_reconciled" => "not reconciled",
            _ => "unknown",
        }
    };
    out.push_str(&format!(
        "File data, Btrfs data used (SPACE_INFO)      {used}  residual {residual}  {status} (tolerance {tolerance})\n"
    ));
}

fn render_df_reconciliation(out: &mut String, df: &DfReconciliation) {
    let metadata_system = df
        .metadata_system_bytes
        .map_or("unknown".to_string(), human_bytes);
    let total = df
        .data_metadata_system_bytes
        .map_or("unknown".to_string(), human_bytes);
    let used = df.used_bytes.map_or("unknown".to_string(), human_bytes);
    let residual = df
        .residual_bytes
        .map_or("unknown".to_string(), signed_bytes);
    out.push_str(&format!(
        "Btrfs data + metadata + system               {total}  residual {residual}\n"
    ));
    out.push_str(&format!(
        "Btrfs metadata and system (not reclaimable)  {metadata_system}  display only\n"
    ));
    out.push_str(&format!(
        "Used (df)                                   {used}  display only\n"
    ));
}

fn render_action(out: &mut String, action: &OverviewAction) {
    let share = action.share.as_deref().unwrap_or("the volume");
    let name = match action.kind {
        "cleanup" => "Delete".to_string(),
        "recycle" => format!("Empty the Recycle Bin of {share:?}"),
        "snapshot" => format!("Review snapshots of {share:?}"),
        "package" => {
            let package = action
                .service
                .map(crate::dsm::service_id_by_code)
                .and_then(crate::dsm::service_by_id)
                .map_or("DSM or package data", |service| service.package);
            format!("Review {package}")
        }
        "wait" => "Wait for Btrfs cleanup".to_string(),
        _ => "Review volume data".to_string(),
    };
    let amount = if action.kind == "wait" {
        action
            .held_bytes
            .map(|bytes| format!("{}; no action needed", human_bytes(bytes)))
            .unwrap_or_else(|| "amount unknown".to_string())
    } else {
        action
            .reclaimable_bytes
            .map(|bytes| format!("frees {}", human_bytes(bytes)))
            .or_else(|| {
                action
                    .held_bytes
                    .map(|bytes| format!("up to {} held", human_bytes(bytes)))
            })
            .unwrap_or_else(|| "amount unknown".to_string())
    };
    let place = action.manage_in.unwrap_or("synoreclaim");
    let command = action.command.as_ref().map_or(String::new(), |tokens| {
        let words = tokens
            .iter()
            .map(|token| shell_word(token.as_bytes()))
            .collect::<Vec<_>>()
            .join(" ");
        format!("; run {words}")
    });
    let path = action
        .path
        .as_ref()
        .map_or(String::new(), |p| format!(" {}", super::escape_path(p)));
    out.push_str(&format!(
        "{}. {}{} ({place}) — {amount}{command}\n",
        action.rank, name, path
    ));
}

impl OverviewReason {
    fn code_name(&self) -> &'static str {
        match self.code {
            UnknownReason::MissingOwner => "missing_owner",
            UnknownReason::IncompleteReferences => "incomplete_references",
            UnknownReason::DirectOwnerUnresolved => "direct_owner_unresolved",
            UnknownReason::SharedTreeOwnerUnresolved => "shared_tree_owner_unresolved",
            UnknownReason::RelocationTree => "relocation_tree",
            UnknownReason::CoverageGap => "coverage_gap",
            UnknownReason::OwnerBudgetExhausted => "owner_budget_exhausted",
            UnknownReason::PinnedRangesUnknown => "pinned_ranges_unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_json_keeps_signed_residual_and_unknown_values() {
        let doc = OverviewDocument {
            envelope: Envelope::new("overview", std::time::UNIX_EPOCH),
            results: Vec::new(),
            scans: Vec::new(),
        };
        let json = serde_json::to_value(doc).unwrap();
        assert_eq!(json["command"], "overview");
    }

    #[test]
    fn direct_owner_subcases_are_additive_and_contain_only_root_structure_facts() {
        let reason = OverviewReason {
            code: UnknownReason::DirectOwnerUnresolved,
            bytes: 19,
            subcases: vec![OverviewReasonSubcase {
                code: SubcaseCode::DirectOwner(DirectOwnerSubcase::WritableRootInodeUnindexed),
                bytes: 19,
                roots: vec![OverviewReasonRoot {
                    root_id: 301,
                    kind: DirectOwnerRootKind::Writable,
                    class: DirectOwnerRootClass::ActiveBackup,
                    at_directory: true,
                    bytes: 19,
                }],
            }],
        };
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            serde_json::json!({
                "code": "direct_owner_unresolved",
                "bytes": 19,
                "subcases": [{
                    "code": "writable_root_inode_unindexed",
                    "bytes": 19,
                    "roots": [{
                        "root_id": 301,
                        "kind": "writable",
                        "class": "active_backup",
                        "at_directory": true,
                        "bytes": 19
                    }]
                }]
            })
        );

        let legacy_reason = OverviewReason {
            code: UnknownReason::MissingOwner,
            bytes: 7,
            subcases: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(legacy_reason).unwrap(),
            serde_json::json!({"code": "missing_owner", "bytes": 7})
        );
    }

    #[test]
    fn pinned_range_subcases_name_only_the_mechanism() {
        let reason = OverviewReason {
            code: UnknownReason::PinnedRangesUnknown,
            bytes: 12,
            subcases: vec![
                OverviewReasonSubcase {
                    code: SubcaseCode::PinnedRanges(PinnedRangesSubcase::NoScannedUse),
                    bytes: 5,
                    roots: Vec::new(),
                },
                OverviewReasonSubcase {
                    code: SubcaseCode::PinnedRanges(PinnedRangesSubcase::SharedReference),
                    bytes: 4,
                    roots: Vec::new(),
                },
                OverviewReasonSubcase {
                    code: SubcaseCode::PinnedRanges(PinnedRangesSubcase::RangeUnmappable),
                    bytes: 3,
                    roots: Vec::new(),
                },
            ],
        };
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            serde_json::json!({
                "code": "pinned_ranges_unknown",
                "bytes": 12,
                "subcases": [
                    {"code": "no_scanned_use", "bytes": 5},
                    {"code": "shared_reference", "bytes": 4},
                    {"code": "range_unmappable", "bytes": 3}
                ]
            })
        );
    }

    #[test]
    fn human_output_names_unknown_bytes_and_quotes_cleanup_commands() {
        let mut out = String::new();
        render_part(
            &mut out,
            &OverviewPart {
                kind: PartKind::Unknown,
                bytes: 10,
                reclaimable_bytes: None,
                held_bytes: None,
                items: Vec::new(),
                reasons: Vec::new(),
            },
        );
        render_action(
            &mut out,
            &OverviewAction {
                rank: 1,
                kind: "cleanup",
                share: Some("photos".into()),
                share_hex: None,
                service: None,
                reclaimable_bytes: Some(10),
                held_bytes: None,
                manage_in: Some("File Station (permanent delete)"),
                manage_in_id: Some(crate::dsm::place_id(crate::dsm::FILE_STATION)),
                path: Some(JsonPath::new(b"/volume1/photos/a b")),
                path_kind: Some("dir"),
                command: Some(vec![
                    "synoreclaim".into(),
                    "cleanup".into(),
                    "delete".into(),
                    "/volume1/photos/a b".into(),
                ]),
            },
        );

        assert!(out.contains("Unknown (references not resolved)"));
        assert!(out.contains("Delete /volume1/photos/a b"));
        assert!(out.contains("run synoreclaim cleanup delete '/volume1/photos/a b'"));
    }

    #[test]
    fn human_output_shows_the_fixed_service_explanation() {
        let mut out = String::new();
        render_part(
            &mut out,
            &OverviewPart {
                kind: PartKind::DsmService,
                bytes: 10,
                reclaimable_bytes: None,
                held_bytes: Some(10),
                items: vec![OverviewItem {
                    label: None,
                    share: None,
                    share_hex: None,
                    service: Some("iscsi_luns"),
                    managed_by: Some("SAN Manager (iSCSI LUNs)"),
                    manage_in: Some("SAN Manager"),
                    manage_in_id: Some(crate::dsm::place_id(crate::dsm::SAN_MANAGER_ISCSI)),
                    explanation: crate::dsm::service(b"@iSCSI", crate::dsm::Depth::VolumeRoot)
                        .and_then(|service| service.explanation),
                    bytes: 10,
                    reclaimable_bytes: None,
                }],
                reasons: Vec::new(),
            },
        );
        assert!(out.contains("UNMAP"), "{out}");
        assert!(out.contains("Virtual Machine Manager > Image"), "{out}");
    }
}
