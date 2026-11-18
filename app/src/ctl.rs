//! Private ctl task adapters. Their fixed, typed projections never forward
//! backend text, paths, argv or future fields. UI methods retain their contract.

use super::{cli, no_document, status, valid_id};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use syno_pkg_land_app::{App, Context, Error, Params};

/// The source registry also supplies the adapter bindings, avoiding a second
/// read/write route inventory. Unknown root methods fail during construction.
pub fn registry() -> Value {
    serde_json::from_str(include_str!("../../operations.json")).expect("operation registry JSON")
}

pub fn register(app: &mut App, data: Arc<Option<PathBuf>>) {
    let registry = registry();
    let operations = registry["operations"].as_object().expect("operations");
    for (name, spec) in operations {
        if let Some(method) = name.strip_prefix("ctl.") {
            if method == "job.status" {
                let data = data.clone();
                app.method(name, move |ctx, params| {
                    job_status(ctx, params, data.as_deref(), false).map_err(safe_error)
                });
                continue;
            }
            assert!(super::CLI_METHODS.contains(&method) || method == "authorization.status");
            let method = method.to_owned();
            let data = data.clone();
            app.method(name, move |ctx, params| {
                if method == "doctor.run" {
                    return diagnostics(ctx, params, data.as_deref());
                }
                let result = if method == "authorization.status" {
                    status(ctx)
                } else {
                    cli(ctx, &method, params.get(), data.as_deref())
                };
                result
                    .map(|document| task_response(&method, &document))
                    .map_err(safe_error)
            });
        } else if spec["ai"] == "preview" {
            let (operation, target) = operations
                .iter()
                .find(|(_, op)| op["preview"] == *name)
                .expect("preview target");
            let operation = operation.clone();
            let effect = target["effect"].as_str().expect("effect").to_owned();
            app.method(name, move |ctx, params| {
                preview(ctx, params, &operation, &effect).map_err(safe_error)
            });
        }
    }
    let scan_data = data.clone();
    app.method("scans.start", move |ctx, params| {
        scan_start(ctx, params, scan_data.as_deref()).map_err(safe_error)
    });
    app.method("scans.status", move |ctx, params| {
        job_status(ctx, params, data.as_deref(), true).map_err(safe_error)
    });
}

/// Submission preview only: authorization evidence and sanitized selection,
/// never plan creation, root.run or root.start. Backend certification still
/// happens at human-confirmed execution using the original immutable plan.
fn preview(ctx: &Context, params: &Params, operation: &str, effect: &str) -> Result<Value, Error> {
    let input: Value = params.decode()?;
    let input = input
        .as_object()
        .ok_or_else(|| Error::bad_request("Expected an object"))?;
    let mut selection = Map::new();
    for key in ["job_id", "plan_id"] {
        if let Some(value) = input.get(key) {
            let id = value
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or_else(|| Error::bad_request("Invalid identity"))?;
            if key == "plan_id" && !id.starts_with("cleanup-") && !id.starts_with("rootfs-") {
                return Err(Error::bad_request("Invalid plan identity"));
            }
            selection.insert(key.into(), json!(id));
        }
    }
    for key in ["paths", "targets"] {
        if let Some(items) = input.get(key) {
            let items = items
                .as_array()
                .ok_or_else(|| Error::bad_request("Invalid selection"))?;
            selection.insert("target_count".into(), json!(items.len()));
        }
    }
    if let Some(mode) = input.get("mode") {
        if !["gentle", "fast", "standard", "deep"]
            .iter()
            .any(|v| mode == v)
        {
            return Err(Error::bad_request("Invalid mode"));
        }
        selection.insert("mode".into(), mode.clone());
    }
    if let Some(disabled) = input.get("mutations_disabled") {
        if !disabled.is_boolean() {
            return Err(Error::bad_request("Invalid setting"));
        }
        selection.insert("mutations_disabled".into(), disabled.clone());
    }
    Ok(
        json!({"operation":operation, "effect":effect, "confirmation_required":true,
        "selection":selection, "authorization":task_response("authorization.status", &status(ctx)?),
        "limitations":"Submission preview only; plan freshness, certification, current job eligibility and filesystem effects are checked by the backend at execution. Review the original sealed plan in the DSM UI before confirming deletion"}),
    )
}

/// Only the ctl entry supplies a default; explicit volumes still use the
/// root program's existing validation. Discovery uses the authenticated API.
fn diagnostics(ctx: &Context, params: &Params, data: Option<&Path>) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        volume: Option<String>,
    }
    let input: Input = params.decode().map_err(safe_error)?;
    let volume = match input.volume {
        Some(volume) => volume,
        None => {
            let document = cli(ctx, "volumes.list", "{}", data).map_err(safe_error)?;
            if document["exit"] != 0 {
                return Err(Error::new(
                    "volumes_unavailable",
                    "Could not discover volumes; run volumes list",
                ));
            }
            let rows = document["volumes"]
                .as_array()
                .ok_or_else(no_document)
                .map_err(safe_error)?;
            let mut volumes = Vec::new();
            for row in rows {
                let volume = row["mount"]["path"]
                    .as_str()
                    .filter(|v| public_volume(v))
                    .ok_or_else(|| {
                        Error::new(
                            "volumes_unavailable",
                            "Could not discover volumes; run volumes list",
                        )
                    })?;
                if !volumes.contains(&volume) {
                    volumes.push(volume);
                }
            }
            match volumes.as_slice() {
                [only] => (*only).to_owned(),
                [] => {
                    return Err(Error::new(
                        "no_volumes",
                        "No mounted data volumes are available",
                    ));
                }
                _ => {
                    return Err(Error::new(
                        "volume_required",
                        &format!("Choose --volume from: {}", volumes.join(", ")),
                    ));
                }
            }
        }
    };
    let document = cli(
        ctx,
        "doctor.run",
        &json!({"volume":volume}).to_string(),
        data,
    )
    .map_err(safe_error)?;
    Ok(task_response("doctor.run", &document))
}

fn public_volume(path: &str) -> bool {
    path.strip_prefix("/volume").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.len() <= 8 && suffix.bytes().all(|b| b.is_ascii_digit())
    })
}

fn scan_start(ctx: &Context, params: &Params, data: Option<&Path>) -> Result<Value, Error> {
    let accepted = cli(ctx, "scan.start", params.get(), data)?;
    if accepted["exit"] == 4
        && accepted["outcome"] == "scan_in_progress"
        && accepted["running_job_id"].as_str().is_some_and(valid_id)
    {
        return Ok(
            json!({"accepted":false,"outcome":"scan_in_progress","running_job_id":accepted["running_job_id"]}),
        );
    }
    if accepted["exit"] != 0
        || accepted["command"] != "detach"
        || !accepted["job_id"].as_str().is_some_and(valid_id)
    {
        return Err(Error::new("scan_not_started", "No new scan was accepted"));
    }
    Ok(json!({"accepted":true,"job_id":accepted["job_id"]}))
}

fn job_status(
    ctx: &Context,
    params: &Params,
    data: Option<&Path>,
    scan_only: bool,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JobParams {
        job_id: String,
    }
    let JobParams { job_id } = params.decode()?;
    if !valid_id(&job_id) {
        return Err(Error::bad_request("Invalid job ID"));
    }
    let document = cli(ctx, "jobs.list", "{}", data)?;
    if document["exit"] != 0 {
        return Err(Error::new(
            if scan_only {
                "scan_status_failed"
            } else {
                "job_status_failed"
            },
            "Could not read job inventory",
        ));
    }
    let scan = document["jobs"]
        .as_array()
        .ok_or_else(no_document)?
        .iter()
        .find(|job| job["job_id"] == job_id && (!scan_only || job["kind"] == "scan"))
        .ok_or_else(|| {
            Error::new(
                if scan_only {
                    "scan_not_found"
                } else {
                    "job_not_found"
                },
                "No matching job with this ID",
            )
        })?;
    Ok(statistics(scan, 0).unwrap_or_else(|| json!({})))
}

/// Refusal codes are protocol constants; backend messages may contain input.
fn safe_error(error: Error) -> Error {
    if [
        "checkpoint_unusable",
        "job_incompatible",
        "busy",
        "not_started",
        "finished",
        "no_job",
        "not_resumable",
        "scan_in_progress",
        "resume_failed",
    ]
    .contains(&error.code.as_str())
    {
        return Error::new(
            &error.code,
            error_message(&error.code, Some(&error.message)),
        );
    }
    let code = match error.code.as_str() {
        "AUTH_DENIED"
        | "ADMIN_REQUIRED"
        | "APP_DENIED"
        | "INELIGIBLE"
        | "POLICY_DENIED"
        | "UNKNOWN_OP"
        | "PERMISSION_DENIED"
        | "SCOPE_DENIED"
        | "INVALID_REQUEST"
        | "REQUEST_TOO_LARGE"
        | "STORAGE_ERROR"
        | "INTERNAL"
        | "ROOT_FAILED"
        | "ROOT_TIMEOUT"
        | "ROOT_BUSY"
        | "ROOT_STAGING_FULL"
        | "EXECUTOR_UNAVAILABLE"
        | "EXECUTOR_TIMEOUT"
        | "EXECUTOR_PROTOCOL"
        | "EXECUTOR_PAUSED"
        | "EXECUTOR_STARTING"
        | "EXECUTOR_TOO_LARGE"
        | "APP_STOPPED"
        | "APP_STARTING"
        | "WEBAPI_UNCERTAIN"
        | "credential_not_ready"
        | "bad_request"
        | "cli_error"
        | "internal"
        | "scan_not_started"
        | "scan_not_found"
        | "scan_status_failed"
        | "job_status_failed"
        | "job_not_found" => error.code.as_str(),
        _ => "ctl_failed",
    };
    Error::new(
        code,
        "Reclaim task failed; inspect authorization get or the DSM UI",
    )
}

fn task_response(method: &str, document: &Value) -> Value {
    if document["command"] == "detach"
        && document["exit"] == 0
        && document["job_id"].as_str().is_some_and(valid_id)
    {
        return json!({"accepted":true,"job_id":document["job_id"]});
    }
    project(method, document)
}

fn project(method: &str, document: &Value) -> Value {
    let groups: &[&str] = match method {
        "volumes.list" => &["volumes", "system_partition"],
        "jobs.list" => &["jobs"],
        "job.result" => &[
            "plan",
            "moved",
            "moves",
            "destination",
            "root",
            "tools",
            "items",
            "kept",
            "held_open",
            "filesystems",
            "scans",
            "results",
            "planned_estimate",
            "current_estimate",
            "certification",
            "filesystem",
            "log_quota",
            "cleanable",
            "open_deleted",
            "owners",
            "owners_small",
            "report_only",
            "report_only_small",
            "usage",
            "walk",
        ],
        "history.list" => &["scans"],
        "overview.get" | "top.list" | "inspect.get" | "simulate.get" | "snapshots.list" => {
            &["results", "scans"]
        }
        "recycle.list" => &[
            "report_info",
            "shares",
            "oldest",
            "scans",
            "moved_by_reclaim",
        ],
        "rootfs.report" => &[
            "filesystem",
            "log_quota",
            "cleanable",
            "open_deleted",
            "owners",
            "owners_small",
            "report_only",
            "report_only_small",
            "usage",
            "walk",
        ],
        "rootfs.history" => &["applies"],
        "cache.status" | "cache.clear" => &["filesystems"],
        "session.get" | "setup.cleanup" => &["config"],
        "doctor.run" => &[
            "checks",
            "system_partition",
            "subvolumes",
            "scheduling",
            "item_sample",
        ],
        "why_not_freed.get" => &["results"],
        "watch_free.sample" => &["samples"],
        "support_bundle.get" => &[
            "volumes",
            "doctor_checks",
            "active_jobs",
            "overview",
            "why_not_freed",
            "top",
            "scan",
            "dropped",
        ],
        "cleanup.plan" => &["size", "shares"],
        "cleanup.apply" => &["recycle_bins"],
        "rootfs.plan" => &[
            "moves",
            "destination",
            "root",
            "tools",
            "items",
            "kept",
            "held_open",
        ],
        "job.pause" | "job.resume" | "job.cancel" | "job.mode" | "rootfs.apply" => &[],
        "authorization.status" => &[],
        _ => return json!({}),
    };
    // Top-level fields are separate from nested statistics, so a future root
    // property cannot become public merely by using a known metric name.
    let mut out = Map::new();
    out.insert("command".into(), json!(method));
    out.insert("schema_version".into(), json!(1));
    for key in [
        "exit",
        "deleted",
        "already_absent",
        "failed",
        "changed",
        "indeterminate",
        "planned",
        "copied",
        "moved",
        "partial",
        "moved_bytes",
        "deletes",
        "deleted_bytes",
        "report_state_stale",
        "completion",
        "job_id",
        "state",
        "reason",
        "outcome",
        "plan_id",
        "expires_at",
        "permanent",
        "mode",
        "mutations_disabled",
        "cleanup_enabled",
        "enables_cleanup",
        "planning_attempts",
        "additional_bytes",
        "duration_seconds",
        "observed_duration_ms",
        "interval_seconds",
        "data_used_delta_bytes",
        "free_delta_bytes",
        "pending_deletion_roots_delta",
        "concurrent_writers_may_affect_delta",
        "interrupted",
        "follow_up",
    ] {
        if let Some(value) = document.get(key).and_then(|value| field(key, value, 0)) {
            out.insert(key.into(), value);
        }
    }
    counts(document, &mut out);

    for &key in groups {
        if let Some(value) = statistics(&document[key], 0) {
            out.insert(key.into(), value);
        }
    }
    if method == "volumes.list" {
        if let (Some(raw), Some(projected)) = (
            document["volumes"].as_array(),
            out.get_mut("volumes").and_then(Value::as_array_mut),
        ) {
            for (raw, projected) in raw.iter().zip(projected) {
                // Standard public volume identifiers support discovery. Share
                // names and arbitrary mount paths are never included.
                if let Some(suffix) = raw["mount"]["path"]
                    .as_str()
                    .and_then(|p| p.strip_prefix("/volume"))
                {
                    if !suffix.is_empty()
                        && suffix.len() <= 8
                        && suffix.bytes().all(|b| b.is_ascii_digit())
                    {
                        projected["volume"] = json!(format!("/volume{suffix}"));
                    }
                }
            }
        }
    }
    Value::Object(out)
}

/// Codes are generated by Reclaim, never backend diagnostic text. Keep new
/// snake_case codes without needing a second allowlist; malformed codes get
/// an explicit fallback so a nonzero error count always has a code.
fn error_code(value: &Value) -> &str {
    value
        .as_str()
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("ctl_failed")
}

pub(super) fn error_message(code: &str, detail: Option<&str>) -> &'static str {
    match code {
        "checkpoint_unusable" => {
            let reasons: &[(&[&str], &str)] = &[
                (
                    &[
                        "subvolume identity changed",
                        "namespace checkpoint identity is",
                    ],
                    "Saved subvolume identity changed; start a fresh scan",
                ),
                (
                    &["checkpoint format mismatch"],
                    "Saved checkpoint format is incompatible; start a fresh scan",
                ),
                (
                    &[
                        "a completed subvolume changed",
                        "subvolume identity or generation changed",
                    ],
                    "Saved subvolume identity or generation changed; start a fresh scan",
                ),
                (
                    &[
                        "no durable checkpoint",
                        "saved scan position is missing",
                        "no saved walk checkpoint",
                    ],
                    "No saved scan position is available; start a fresh scan",
                ),
                (
                    &["completed-unit manifest"],
                    "Saved completed-unit manifest is damaged or incompatible; start a fresh scan",
                ),
                (
                    &["filesystem identity changed", "volume is unavailable"],
                    "Saved filesystem identity changed or its volume is unavailable; start a fresh scan",
                ),
                (
                    &["no longer exists"],
                    "A saved subvolume no longer exists; start a fresh scan",
                ),
                (
                    &["did not finish saving", "partial extent stream is missing"],
                    "The saved namespace is incomplete; start a fresh scan",
                ),
                (
                    &[
                        "checksum mismatch",
                        "missing or damaged",
                        "damaged namespace",
                    ],
                    "Saved checkpoint data is missing or damaged; start a fresh scan",
                ),
                (
                    &["being relocated"],
                    "Saved checkpoint trees are being relocated; start a fresh scan after maintenance",
                ),
            ];
            reasons.iter().find(|(patterns, message)| detail.is_some_and(|text|
                text == *message || patterns.iter().any(|pattern| text.contains(pattern))))
                .map_or("Saved scan checkpoint is damaged, incompatible, or no longer matches this volume; start a fresh scan", |(_, message)| *message)
        }
        "job_incompatible" => "Saved job formats do not match this build; start a fresh scan",
        "not_started" => "The scan is queued but its worker has not started; check jobs and retry",
        "finished" => "The scan has already ended; start a fresh scan",
        "no_job" => "The saved scan no longer exists; start a fresh scan",
        "not_resumable" => "This job cannot be resumed; start a fresh scan",
        "resume_failed" => "Could not resume the scan; retry or start a fresh scan",
        "metadata_error" => "Could not read filesystem metadata; inspect diagnostics get",
        "no_report" => "No matching scan report is available; start a fresh scan",
        "busy" | "scan_in_progress" => "Another scan is using this volume; inspect jobs list",
        _ => "Reclaim task failed; inspect diagnostics get or the DSM UI",
    }
}

fn counts(value: &Value, out: &mut Map<String, Value>) {
    if let Some(errors) = value["errors"].as_array() {
        let errors: Vec<Value> = errors
            .iter()
            .map(|error| {
                let code = error_code(&error["code"]);
                json!({"code":code, "message":error_message(code, error["message"].as_str())})
            })
            .collect();
        out.insert("error_count".into(), json!(errors.len()));
        if !errors.is_empty() {
            out.insert(
                "error_codes".into(),
                json!(errors.iter().map(|e| &e["code"]).collect::<Vec<_>>()),
            );
            out.insert("errors".into(), json!(errors));
        }
    }
    for (key, count) in [
        ("warnings", "warning_count"),
        ("targets", "target_count"),
        ("selection", "selection_count"),
    ] {
        if let Some(items) = value[key].as_array() {
            out.insert(count.into(), json!(items.len()));
        }
    }
}

fn statistics(value: &Value, depth: usize) -> Option<Value> {
    if depth > 32 {
        return None;
    }
    if let Some(items) = value.as_array() {
        return Some(Value::Array(
            items
                .iter()
                .filter_map(|item| statistics(item, depth + 1))
                .collect(),
        ));
    }
    let object = value.as_object()?;
    let mut out = Map::new();
    for (key, value) in object {
        if let Some(value) = field(key, value, depth) {
            out.insert(key.clone(), value);
        }
    }
    counts(value, &mut out);
    Some(Value::Object(out))
}

fn field(key: &str, value: &Value, depth: usize) -> Option<Value> {
    const NUMBERS: &[&str] = &[
        "cleared",
        "exit",
        "exit_code",
        "live_subvolume_id",
        "held_only_by_snapshots_bytes",
        "exclusive_bytes",
        "shared_with_live_bytes",
        "unknown_bytes",
        "qgroup_exclusive_bytes",
        "pinned_unreferenced_bytes",
        "upper_bound_bytes",
        "estimated_bytes",
        "signal",
        "freed_bytes",
        "available_before_bytes",
        "removed_dirs",
        "appeared_after_report",
        "cleaned_since_report",
        "multi_share_bytes",
        "since_seconds",
        "since_bytes",
        "report_orphan_bytes",
        "elapsed_s",
        "units_done",
        "units_total",
        "bytes_read",
        "rss_bytes",
        "subvolume_id",
        "created_at",
        "expires_at",
        "scan_finished",
        "retained_bytes",
        "planning_attempts",
        "additional_bytes",
        "required_bytes",
        "available_after_bytes",
        "entries_total",
        "entries_done",
        "allocated_bytes_total",
        "allocated_bytes_done",
        "items_done",
        "items_total",
        "bytes_done",
        "bytes_total",
        "pending_deletion_roots",
        "duration_seconds",
        "observed_duration_ms",
        "interval_seconds",
        "data_used_delta_bytes",
        "free_delta_bytes",
        "pending_deletion_roots_delta",
        "bytes",
        "logical",
        "physical",
        "extents",
        "inline",
        "entries",
        "files",
        "dirs",
        "reclaimable",
        "unknown",
        "pinned",
        "retained",
        "together_bonus",
        "shared",
        "held",
        "total_bytes",
        "used_bytes",
        "free_bytes",
        "available_bytes",
        "used_percent",
        "size_bytes",
        "reclaimable_bytes",
        "reports",
        "incompatible_reports",
        "count",
        "items",
        "units",
        "resumes",
        "elapsed_ms",
        "total_elapsed_ms",
        "entries_read",
        "subvolumes",
        "reused_units",
        "full_read_ms",
        "full_read_entries",
        "peak_rss_bytes",
        "spill_peak_bytes",
        "report_bytes",
        "age_seconds",
        "df_total_bytes",
        "df_used_bytes",
        "df_free_bytes",
        "data_used_bytes",
        "budget_bytes",
        "resident_bytes",
        "reserved_bytes",
        "projected_bytes",
        "inodes_total",
        "inodes_used",
        "soft_limit_bytes",
        "hard_limit_bytes",
        "required_destination_bytes",
        "allocated_bytes",
        "bytes_estimate",
        "unreadable_processes",
        "unreadable_dirs",
        "percent_of_used",
        "below_bytes",
        "encrypted_directories",
        "snapshots_not_materialized",
        "snapshots_materialized",
        "unlinked_subvolume_entries",
        "dangling_entries",
        "cycles",
        "unsupported_locations",
        "missing_subvolume_roots",
        "unresolved_extents",
        "deleted",
        "already_absent",
        "failed",
        "changed",
        "indeterminate",
        "planned",
        "copied",
        "moved",
        "partial",
        "moved_bytes",
        "deletes",
        "deleted_bytes",
        "bytes_moved",
        "remaining_entries",
        "remaining_bytes",
        "skipped",
        "hidden_system",
        "reclassified",
        "reclaimable_lost",
        "pinned_lost",
        "unknown_added",
        "limit",
        "format",
    ];
    const BOOLS: &[&str] = &[
        "metadata_access",
        "materialized",
        "read_only",
        "pending_deletion",
        "exact",
        "mutations_disabled",
        "cleanup_enabled",
        "enables_cleanup",
        "lower_bound",
        "since_not_measured",
        "interrupted",
        "concurrent_writers_may_affect_delta",
        "compatible",
        "stale",
        "selection_stable",
        "nearly_full",
        "complete",
        "volume",
        "readonly",
        "reusable",
        "scan",
        "snapshots",
        "reflinks",
        "cleanup",
        "cleanup_eligible",
        "confirmed",
        "holds",
        "report_state_stale",
        "permanent",
        "enabled",
        "admin_only",
        "drive",
        "delete",
    ];
    const GROUPS: &[&str] = &[
        "df",
        "simulation",
        "plan",
        "moved",
        "size",
        "recycle_bins",
        "recycle_bin",
        "moved_by_reclaim",
        "estimate",
        "config",
        "basis",
        "reasons",
        "samples",
        "checks",
        "doctor_checks",
        "active_jobs",
        "overview",
        "why_not_freed",
        "top",
        "scan",
        "dropped",
        "subvolumes",
        "scheduling",
        "item_sample",
        "moves",
        "destination",
        "root",
        "tools",
        "kept",
        "held_open",
        "system_partition",
        "totals",
        "stats",
        "coverage",
        "resource_limit",
        "metrics",
        "without_package_data",
        "combined",
        "planned_estimate",
        "current_estimate",
        "certification",
        "recheck",
        "space",
        "data",
        "metadata",
        "system",
        "pending_deletion",
        "reconciliation",
        "parts",
        "entry",
        "entries",
        "report",
        "report_info",
        "filesystem",
        "filesystems",
        "jobs",
        "scans",
        "volumes",
        "results",
        "shares",
        "snapshots",
        "cleanable",
        "deep",
        "walk",
        "log_quota",
        "open_deleted",
        "owners",
        "owners_small",
        "report_only",
        "report_only_small",
        "usage",
        "children",
        "by_category",
        "applies",
        "result",
        "units",
        "capabilities",
        "services",
        "rows",
        "progress",
        "restart",
        "cleanup",
        "rootfs",
        "held_by",
        "oldest",
    ];
    if let Some(items) = value.as_array() {
        if [
            "selected_ids",
            "not_materialized_ids",
            "pending_deletion_ids",
        ]
        .contains(&key)
            && items.iter().all(|v| v.as_u64().is_some())
        {
            return Some(value.clone());
        }
        if key == "inexact_reasons" {
            return Some(json!(
                items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(
                        |s| ["not_materialized", "unknown_owners", "cross_share_sharing"]
                            .contains(s)
                    )
                    .collect::<Vec<_>>()
            ));
        }
        if key == "error_codes" {
            return Some(json!(items.iter().map(error_code).collect::<Vec<_>>()));
        }
    }
    // Reports use fixed tool-name strings; plans/applies use tool objects.
    // Keep both existing representations and drop unrecognized strings.
    if let Some(items) = value
        .as_array()
        .filter(|_| key == "tools" || key == "follow_up")
    {
        return Some(Value::Array(
            items
                .iter()
                .filter_map(|item| {
                    if item.is_string() {
                        field(
                            if key == "tools" { "tool" } else { "follow_up" },
                            item,
                            depth + 1,
                        )
                    } else if key == "tools" {
                        statistics(item, depth + 1)
                    } else {
                        None
                    }
                })
                .collect(),
        ));
    }
    if (NUMBERS.contains(&key) && (value.is_number() || value.is_null()))
        || (BOOLS.contains(&key) && (value.is_boolean() || value.is_null()))
    {
        return Some(value.clone());
    }
    if key == "plan_id"
        && value.as_str().is_some_and(|id| {
            valid_id(id) && (id.starts_with("cleanup-") || id.starts_with("rootfs-"))
        })
    {
        return Some(value.clone());
    }
    if ["updated", "created", "expires_at"].contains(&key)
        && value.as_str().is_some_and(|s| {
            s.len() == 20
                && s.ends_with('Z')
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || b"-:TZ".contains(&b))
        })
    {
        return Some(value.clone());
    }
    let allowed: &[&str] = match key {
        "name" => &[
            "scheduling",
            "item_types",
            "pending_deletion",
            "features",
            "filesystem",
            "privilege",
            "job_lock",
            "system_partition",
            "state_space",
            "state_space_estimate",
            "state_headroom",
            "tree_search_v2",
            "target",
            "fs_info",
            "subvolumes",
            "qgroups",
            "cache",
        ],
        "phase" => &[
            "discover",
            "scan",
            "resolve-refs",
            "aggregate",
            "publish",
            "execute",
            "plan",
            "report",
            "moves",
            "tools",
        ],
        "mode" => &["gentle", "fast", "standard", "deep"],
        "status" => &["pass", "warn", "fail", "skip"],
        "category" => &[
            "update_leftover",
            "log_archive",
            "crash_dump",
            "temp_debris",
            "geoip_database",
            "preference_image",
            "samba_cache",
            "tool_cache",
            "backup_installer",
        ],
        "tool" => &[
            "rotate_logs",
            "delete_journal",
            "restart_syslog_ng",
            "logrotate",
            "clean_downloaded_update",
            "release_update_reserve",
            "delete_log",
        ],
        "follow_up" => &[
            "refresh_geoip",
            "reset_images",
            "restart_winbind",
            "download_update",
            "update_dsm",
            "backup_installer",
        ],
        "code" => &[
            "pending_subvolume_cleanup",
            "held_by_snapshots",
            "recycle_bin",
            "open_unlinked",
            "shared_extents",
            "delayed_allocation",
            "metadata",
            "free_space",
        ],
        "state" => &[
            "queued",
            "running",
            "pausing",
            "cancelling",
            "paused",
            "completed",
            "partial",
            "failed",
            "cancelled",
            "interrupted",
            "ready",
            "authorization_required",
            "not_admin",
            "session_expired",
            "unavailable",
        ],
        "completion" => &[
            "complete",
            "partial",
            "busy",
            "paused",
            "runtime_limit",
            "resource_limit",
            "cancelled",
            "failed",
        ],
        "profile" => &[
            "low", "normal", "fast", "single", "dup", "raid0", "raid1", "raid10", "raid5", "raid6",
        ],
        "kind" => &[
            "scan",
            "cleanup-plan",
            "rootfs-plan",
            "rootfs-report",
            "cleanup-apply",
            "rootfs-apply",
            "btrfs",
            "ext4",
            "all",
            "dirs",
            "files",
            "dir",
            "file",
            "symlink",
            "service",
            "snapshot_only",
            "share_live",
            "shared_between_folders",
            "recycle_bin",
            "dsm_service",
            "package",
            "unknown",
            "system_other",
            "pending_cleanup",
            "pinned_unreferenced",
            "writable",
        ],
        "filesystem" | "fstype" => &["btrfs", "ext4"],
        "consistency" => &["observed-stable", "changed-during-scan", "partial"],
        "reference_accounting" => &["complete", "partial"],
        "level" => &["ok", "getting_full", "full"],
        "outcome" => &[
            "checkpoint_unusable",
            "job_incompatible",
            "invalid_param",
            "requested",
            "cancelled",
            "paused",
            "running",
            "succeeded",
            "killed",
            "timed_out",
            "not_started",
            "untrusted",
            "not_needed",
            "completed",
            "partial",
            "superseded",
            "failed",
            "deleted",
            "no_result",
        ],
        "reason" => &[
            "grant_missing",
            "upgrade_reapproval",
            "not_admin",
            "session_expired",
            "service_unavailable",
        ],
        _ => &[],
    };
    if let Some(text) = value.as_str() {
        if let Some(&fixed) = allowed.iter().find(|&&fixed| fixed == text) {
            return Some(json!(fixed));
        }
        if key == "job_id" && valid_id(text) {
            return Some(json!(text));
        }
    }
    if GROUPS.contains(&key) {
        return statistics(value, depth + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_keep_cache_clear_counts_and_raw_overview_df_without_private_fields() {
        let raw = json!({"filesystems":[{"cleared":0,"bytes":0,"private":"secret"}]});
        assert_eq!(project("cache.clear", &raw)["filesystems"][0]["cleared"], 0);
        let raw = json!({"results":[{"space":{"df":{"total_bytes":4096,"used_bytes":1024,"free_bytes":3072,"available_bytes":2048,"private":"secret"}}}]});
        let result = project("overview.get", &raw);
        assert_eq!(result["results"][0]["space"]["df"]["total_bytes"], 4096);
        assert_eq!(result["results"][0]["space"]["df"]["available_bytes"], 2048);
        assert!(!result.to_string().contains("secret"));
    }

    #[test]
    fn snapshot_projection_keeps_space_bounds_and_simulation_reasons() {
        let source: Value = serde_json::from_str(include_str!(
            "../../ui/fixtures/snapshots.list/volume1-unknown-owners-sim.json"
        ))
        .unwrap();
        let projected = project("snapshots.list", &source);
        let raw = &source["results"][0]["shares"][2];
        let share = &projected["results"][0]["shares"][2];
        for key in [
            "live_subvolume_id",
            "held_only_by_snapshots_bytes",
            "unknown_bytes",
            "snapshots_not_materialized",
        ] {
            assert_eq!(share[key], raw[key], "{key}");
        }
        for key in [
            "exclusive_bytes",
            "shared_with_live_bytes",
            "unknown_bytes",
            "qgroup_exclusive_bytes",
            "pinned_unreferenced_bytes",
            "materialized",
            "read_only",
            "pending_deletion",
        ] {
            assert_eq!(
                share["snapshots"][0][key], raw["snapshots"][0][key],
                "{key}"
            );
        }
        for key in [
            "selected_ids",
            "not_materialized_ids",
            "freed_bytes",
            "unknown_bytes",
            "upper_bound_bytes",
            "exact",
            "inexact_reasons",
        ] {
            assert_eq!(share["simulation"][key], raw["simulation"][key], "{key}");
        }
        assert!(share["simulation"].get("selector").is_none());
        assert!(share["simulation"].get("exact_command").is_none());
        assert_eq!(projected["completion"], "partial");
        let complete: Value = serde_json::from_str(include_str!(
            "../../ui/fixtures/snapshots.list/volume1-many-simulated-exact.json"
        ))
        .unwrap();
        assert_eq!(
            project("snapshots.list", &complete)["completion"],
            "complete"
        );
    }

    #[test]
    fn errors_always_keep_codes_and_safe_messages_including_nested_job_results() {
        let doc = json!({"errors":[
            {"code":"checkpoint_unusable", "message":"resume checkpoint cannot be used: a completed subvolume changed; start a fresh scan"},
            {"code":"metadata_error", "message":"private-file"},
            {"message":"private-file"}
        ], "filesystems":[{"errors":[{"code":"checkpoint_unusable","message":"private-file"}]}]});
        let projected = project("job.result", &doc);
        assert_eq!(
            projected["error_codes"],
            json!(["checkpoint_unusable", "metadata_error", "ctl_failed"])
        );
        assert_eq!(projected["errors"].as_array().unwrap().len(), 3);
        assert!(
            projected["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("start a fresh scan")
        );
        assert_eq!(
            projected["filesystems"][0]["error_codes"],
            json!(["checkpoint_unusable"])
        );
        assert!(!projected.to_string().contains("private-file"));
        let jobs = project(
            "jobs.list",
            &json!({"jobs":[{"result":{"error_codes":["checkpoint_unusable"]}}]}),
        );
        assert_eq!(
            jobs["jobs"][0]["result"]["error_codes"],
            json!(["checkpoint_unusable"])
        );
    }

    #[test]
    fn deep_report_and_plan_results_preserve_the_current_rootfs_contract() {
        let report: Value = serde_json::from_str(include_str!(
            "../../ui/fixtures/rootfs.report/deep-short.json"
        ))
        .unwrap();
        for method in ["rootfs.report", "job.result"] {
            assert_eq!(project(method, &report)["cleanable"], report["cleanable"]);
        }
        let plan: Value = serde_json::from_str(include_str!(
            "../../ui/fixtures/rootfs.plan/deep-short.json"
        ))
        .unwrap();
        for method in ["rootfs.plan", "job.result"] {
            let public = project(method, &plan);
            assert_eq!(public["moves"], plan["moves"]);
            assert_eq!(public["tools"][0]["tool"], "delete_log");
            assert_eq!(
                public["tools"][0]["estimated_bytes"],
                plan["tools"][0]["estimated_bytes"]
            );
            assert_eq!(public["follow_up"], plan["follow_up"]);
            assert!(public["tools"][0].get("argv").is_none());
            assert!(public["tools"][0].get("effect").is_none());
        }
        let unsafe_doc = json!({"cleanable":{"deep":{"tools":["delete_log","private-tool"],"secret":"private-path"}},"follow_up":["backup_installer","private-action"]});
        let safe = project("job.result", &unsafe_doc);
        assert_eq!(safe["cleanable"]["deep"]["tools"], json!(["delete_log"]));
        assert_eq!(safe["follow_up"], json!(["backup_installer"]));
        assert!(!safe.to_string().contains("private-"));
        for outcome in [
            "succeeded",
            "failed",
            "killed",
            "timed_out",
            "not_started",
            "untrusted",
            "not_needed",
        ] {
            let doc = json!({"tools":[{"tool":"delete_log","outcome":outcome,"signal":9}]});
            let public = project("job.result", &doc);
            assert_eq!(public["tools"][0]["outcome"], outcome);
            assert_eq!(public["tools"][0]["signal"], 9);
        }
    }

    #[test]
    fn completed_cleanup_results_keep_execution_totals_and_drop_private_details() {
        let result = project(
            "job.result",
            &json!({"completion":"partial","deleted":2,"changed":1,"report_state_stale":true,
            "moved":{"files":3,"size_bytes":42},"root":{"freed_bytes":40},"path":"private", "errors":[{"code":"plan_stale","message":"private"}]}),
        );
        assert_eq!(result["deleted"], 2);
        assert_eq!(result["changed"], 1);
        assert_eq!(result["moved"]["files"], 3);
        assert_eq!(result["root"]["freed_bytes"], 40);
        assert_eq!(result["error_codes"], json!(["plan_stale"]));
        assert!(!result.to_string().contains("private"));
    }

    #[test]
    fn projections_keep_plans_and_live_progress_without_private_fields() {
        let doc = json!({"exit":0,"completion":"complete","plan_id":"cleanup-aa-bb","deletes":1,
            "plan_file":"private","paths":["private"],"size":{"entries":3,"files":2,"allocated_bytes":42},
            "shares":[{"share":"private","path":"private","recycle_bin":{"enabled":false,"admin_only":false},
                "items":[{"path":"private","kind":"file","entries":1,"files":1,"allocated_bytes":42,"drive":false}],
                "entries":1,"files":1,"allocated_bytes":42,"delete":true,"delete_reason":"recycle_bin_off"}]});
        let projected = project("cleanup.plan", &doc);
        assert_eq!(projected["plan_id"], "cleanup-aa-bb");
        assert_eq!(projected["size"]["allocated_bytes"], 42);
        // Items with no Recycle Bin are deleted for good: an AI tool sees
        // how many, and which shared folders lack one.
        assert_eq!(projected["deletes"], 1);
        assert_eq!(projected["shares"][0]["recycle_bin"]["enabled"], false);
        assert_eq!(projected["shares"][0]["delete"], true);
        assert!(!projected.to_string().contains("private"));
        let applied = project(
            "cleanup.apply",
            &json!({"completion":"complete","moved":2,"partial":0,"moved_bytes":42,"deleted":1,"deleted_bytes":7,
                "recycle_bins":[{"share":"private","path":"private","admin_only":true,"items":2,"moved_bytes":42}],
                "targets":[{"path":"private"}]}),
        );
        assert_eq!(applied["moved"], 2);
        assert_eq!(applied["recycle_bins"][0]["moved_bytes"], 42);
        assert_eq!(applied["deleted"], 1);
        assert_eq!(applied["deleted_bytes"], 7);
        assert!(!applied.to_string().contains("private"));
        let jobs = project(
            "jobs.list",
            &json!({"jobs":[{"kind":"cleanup-plan","phase":"plan", "progress":{"elapsed_s":9,"units_done":2,"units_total":null,"updated":"2026-10-20T00:00:00Z"}}]}),
        );
        assert_eq!(jobs["jobs"][0]["kind"], "cleanup-plan");
        assert_eq!(jobs["jobs"][0]["phase"], "plan");
        assert_eq!(jobs["jobs"][0]["progress"]["elapsed_s"], 9);
    }

    #[test]
    fn projections_keep_metrics_and_partial_coverage_but_reject_type_changes() {
        let source = json!({"completion":"partial", "exit":3, "future":42,
            "errors":[{"code":"private-error", "message":"private-file"}],
            "results":[{"path":"private-file", "metrics":{"logical":456,"reclaimable":123,"unknown":null,"physical":"private-content"},
                "report":{"stale":true,"consistency":"partial","coverage":{"unresolved_extents":7,"reference_accounting":"partial","new_field":42}},
                "future_numeric":42}]});
        assert_eq!(
            project("top.list", &source),
            json!({
            "command":"top.list", "schema_version":1, "completion":"partial", "exit":3, "error_count":1,
            "error_codes":["ctl_failed"], "errors":[{"code":"ctl_failed","message":"Reclaim task failed; inspect diagnostics get or the DSM UI"}],
            "results":[{"metrics":{"logical":456,"reclaimable":123,"unknown":null},
                "report":{"stale":true,"consistency":"partial","coverage":{"unresolved_extents":7,"reference_accounting":"partial"}}}]})
        );
        assert_eq!(
            project(
                "volumes.list",
                &json!({"volumes":[
                    {"mount":{"path":"/volume1"},"used_bytes":100},
                    {"mount":{"path":"/private/passkey"},"used_bytes":200}
                ]})
            )["volumes"],
            json!([{"volume":"/volume1","used_bytes":100},{"used_bytes":200}])
        );
    }

    #[test]
    fn fixture_reports_do_not_leak_private_text_or_new_fields() {
        const PRIVATE: &str = "private-url-token-passkey-file-content";
        fn taint(value: &mut Value) {
            match value {
                Value::Object(object) => {
                    for (key, value) in object.iter_mut() {
                        if [
                            "path",
                            "path_hex",
                            "name",
                            "message",
                            "requested_by",
                            "source",
                            "argv",
                            "run",
                            "url",
                            "token",
                            "password",
                            "passkey",
                            "content",
                        ]
                        .contains(&key.as_str())
                        {
                            *value = json!(PRIVATE);
                        } else {
                            taint(value);
                        }
                    }
                    object.insert(
                        "unrecognized_future_field".into(),
                        json!({"content":PRIVATE}),
                    );
                }
                Value::Array(items) => {
                    for item in items {
                        taint(item);
                    }
                }
                _ => {}
            }
        }
        let mut checked = 0;
        for (name, spec) in registry()["operations"].as_object().unwrap() {
            if spec["ai"] != "read" || !name.starts_with("ctl.") {
                continue;
            }
            let method = name.trim_start_matches("ctl.");
            let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../ui/fixtures")
                .join(method);
            let Ok(files) = std::fs::read_dir(fixtures) else {
                continue;
            };
            for file in files {
                let path = file.unwrap().path();
                if path.extension().and_then(|v| v.to_str()) != Some("json")
                    || path.file_name().unwrap() == "index.json"
                {
                    continue;
                }
                let mut source: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                taint(&mut source);
                let output = project(method, &source);
                assert!(!output.to_string().contains(PRIVATE), "{}", path.display());
                assert!(
                    !output.to_string().contains("unrecognized_future_field"),
                    "{}",
                    path.display()
                );
                if source.get("completion").is_some() {
                    assert_eq!(
                        output["completion"],
                        source["completion"],
                        "{}",
                        path.display()
                    );
                }
                checked += 1;
            }
        }
        assert!(checked >= 80, "only {checked} fixtures checked");
    }
}
