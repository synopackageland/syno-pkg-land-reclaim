//! The published JSON Schemas against golden command outputs
//! (`tests/golden/json`, recorded from the loop fixture).

#[path = "support/schema.rs"]
mod support;

use serde_json::{Value, json};
use std::path::PathBuf;
use support::{Schemas, read_json, root};

const COMMANDS: [&str; 32] = [
    "config-init",
    "config-set",
    "config-show",
    "cleanup-plan",
    "cleanup-apply",
    "cleanup-delete",
    "scan",
    "top",
    "inspect",
    "simulate-delete",
    "shared",
    "volumes",
    "cache",
    "jobs",
    "history",
    "snapshots",
    "recycle",
    "overview",
    "why-not-freed",
    "pause",
    "cancel",
    "mode",
    "resume",
    "detach",
    "watch-free",
    "webapi",
    "job-result",
    "session",
    "rootfs",
    "rootfs-plan",
    "rootfs-apply",
    "rootfs-history",
];

fn goldens() -> Vec<(PathBuf, Value)> {
    let dir = root().join("tests/golden/json");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| (p.clone(), read_json(&p)))
        .collect()
}

fn golden(name: &str) -> Value {
    read_json(&root().join("tests/golden/json").join(name))
}

#[test]
fn every_command_has_a_schema_and_a_golden_output() {
    let schemas = Schemas::load();
    let outputs: Vec<String> = goldens()
        .iter()
        .map(|(_, doc)| doc["command"].as_str().unwrap().to_string())
        .collect();
    for command in COMMANDS {
        assert!(
            schemas
                .files
                .contains_key(&format!("{command}.schema.json")),
            "no schema for {command}"
        );
        assert!(
            outputs.iter().any(|c| c == command),
            "no golden for {command}"
        );
    }
}

#[test]
fn golden_outputs_match_their_command_schemas() {
    let schemas = Schemas::load();
    for (path, doc) in goldens() {
        let command = doc["command"].as_str().unwrap();
        let errors = schemas.validate(command, &doc);
        assert!(errors.is_empty(), "{path:?}:\n{}", errors.join("\n"));
    }
}

#[test]
fn rootfs_schemas_expose_destination_room_and_share_quota_limit_reason() {
    let schemas = Schemas::load();
    let rootfs = schemas.files.get("rootfs.schema.json").unwrap();
    assert!(
        rootfs
            .pointer(
                "/$defs/rootfs_report_fields/properties/cleanable/properties/required_destination_bytes"
            )
            .is_some()
    );

    let rootfs_plan = schemas.files.get("rootfs-plan.schema.json").unwrap();
    assert!(
        rootfs_plan
            .pointer(
                "/$defs/rootfs_plan_fields/properties/moves/properties/required_destination_bytes"
            )
            .is_some()
    );

    let common = schemas.files.get("common.schema.json").unwrap();
    let limit = common
        .pointer("/$defs/target_error/allOf/1/properties/limit/enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(limit.iter().any(|value| value == "share_quota"));
}

#[test]
fn doctor_uses_the_same_system_partition_schema_as_volumes() {
    let schemas = Schemas::load();
    let volumes = golden("volumes.json");
    let doctor = json!({
        "schema_version": 1,
        "target": "/volume1",
        "kernel": null,
        "filesystem_uuid": null,
        "subvolume_id": null,
        "checks": [],
        "system_partition": volumes["system_partition"].clone(),
        "subvolumes": null,
        "item_sample": null,
        "scheduling": null
    });
    assert!(schemas.files.contains_key("doctor.schema.json"));
    assert_eq!(schemas.validate("doctor", &doctor), Vec::<String>::new());
}

#[test]
fn rootfs_tools_and_advice_use_stable_codes() {
    let schemas = Schemas::load();
    let document = golden("rootfs.json");
    assert_eq!(
        document["report_only"][1]["advice"]["manage_in_id"],
        "package_center"
    );

    for code in [
        "user_tooling",
        "package_data",
        "live_logs",
        "log_center_db",
        "geoip_database",
        "own_state",
        "dsm_keep",
    ] {
        let mut document = golden("rootfs.json");
        document["report_only"][0]["advice"]["code"] = json!(code);
        let errors = schemas.validate("rootfs", &document);
        assert!(errors.is_empty(), "report_only {code}: {errors:?}");
    }
    for code in ["restart_syslog_ng", "restart_holder"] {
        let mut document = golden("rootfs.json");
        document["open_deleted"]["holders"][0]["advice"]["code"] = json!(code);
        let errors = schemas.validate("rootfs", &document);
        assert!(errors.is_empty(), "holder {code}: {errors:?}");
    }
    for tool in [
        "rotate_logs",
        "delete_journal",
        "logrotate",
        "restart_syslog_ng",
    ] {
        let mut document = golden("rootfs.json");
        document["cleanable"]["tools"] = json!([tool]);
        let errors = schemas.validate("rootfs", &document);
        assert!(errors.is_empty(), "tool {tool}: {errors:?}");
    }

    let mut invalid = golden("rootfs.json");
    invalid["report_only"][0]["advice"]["code"] = json!("unknown_advice");
    assert!(!schemas.validate("rootfs", &invalid).is_empty());
    let mut invalid = golden("rootfs.json");
    invalid["open_deleted"]["holders"][0]["advice"]["code"] = json!("package_data");
    assert!(!schemas.validate("rootfs", &invalid).is_empty());
    let mut invalid = golden("rootfs.json");
    invalid["cleanable"]["tools"] = json!(["unknown_tool"]);
    assert!(!schemas.validate("rootfs", &invalid).is_empty());
    let mut invalid = golden("rootfs.json");
    invalid["report_only"][1]["advice"]["manage_in_id"] = json!("settings");
    assert!(!schemas.validate("rootfs", &invalid).is_empty());
}

#[test]
fn rootfs_level_uses_the_shared_system_partition_enums() {
    let schemas = Schemas::load();
    for level in ["ok", "getting_full", "full"] {
        let mut document = golden("rootfs.json");
        document["filesystem"]["level"] = json!(level);
        let errors = schemas.validate("rootfs", &document);
        assert!(errors.is_empty(), "level {level}: {errors:?}");
    }
    for reason in [
        "used_over_recommended",
        "update_headroom_low",
        "update_may_fail",
        "nearly_full",
    ] {
        let mut document = golden("rootfs.json");
        document["filesystem"]["level_reasons"] = json!([reason]);
        let errors = schemas.validate("rootfs", &document);
        assert!(errors.is_empty(), "reason {reason}: {errors:?}");
    }
    let mut invalid = golden("rootfs.json");
    invalid["filesystem"]["level"] = json!("nearly_full");
    assert!(!schemas.validate("rootfs", &invalid).is_empty());
    let mut invalid = golden("rootfs.json");
    invalid["filesystem"]["level_reasons"] = json!(["unknown_reason"]);
    assert!(!schemas.validate("rootfs", &invalid).is_empty());
}

#[test]
fn rootfs_level_can_be_absent_when_partition_space_is_inconsistent() {
    let schemas = Schemas::load();
    let mut document = golden("rootfs.json");
    let filesystem = document["filesystem"].as_object_mut().unwrap();
    filesystem.remove("level");
    filesystem.remove("level_reasons");
    assert_eq!(schemas.validate("rootfs", &document), Vec::<String>::new());
}

#[test]
fn recycle_schema_allows_optional_age_rows() {
    let schemas = Schemas::load();
    let schema = schemas.files.get("recycle.schema.json").unwrap();
    let properties = schema
        .pointer("/allOf/1/properties/shares/items/properties")
        .unwrap()
        .as_object()
        .unwrap();
    assert!(properties.contains_key("oldest"));
    assert!(!properties.contains_key("ctime"));

    let doc = golden("recycle.json");
    let share = doc["shares"][0].as_object().unwrap();
    assert!(!share.contains_key("oldest"));
    assert!(!share.contains_key("ctime"));

    let mut with_oldest = doc.clone();
    with_oldest["shares"][0]["oldest"] = json!([
        {
            "path": {"path": "/volume1/photos/#recycle/old.bin"},
            "ctime": "2026-09-29T00:00:00Z",
            "bytes": 10,
            "reclaimable_bytes": 8
        }
    ]);
    assert_eq!(
        schemas.validate("recycle", &with_oldest),
        Vec::<String>::new()
    );
}

#[test]
fn recycle_refusals_fit_the_recycle_schema() {
    let schemas = Schemas::load();
    let doc = json!({
        "schema_version": 1,
        "command": "recycle",
        "tool_version": "0.1.0",
        "generated_at": "2026-09-29T00:00:00Z",
        "completion": "failed",
        "job_id": null,
        "errors": [{
            "path": "/volume1/photos/nested",
            "code": "not_a_shared_folder",
            "message": "…"
        }],
        "warnings": [],
        "target": {"path": "/volume1/photos/nested"},
        "report_info": null,
        "shares": [],
        "moved_by_reclaim": [],
        "scans": []
    });
    assert_eq!(schemas.validate("recycle", &doc), Vec::<String>::new());
    let mut wrong = doc.clone();
    wrong["errors"][0]["code"] = json!(7);
    assert!(!schemas.validate("recycle", &wrong).is_empty());
}

#[test]
fn why_not_freed_schema_allows_the_since_measurement_indicator() {
    let schemas = Schemas::load();
    let mut doc = golden("why-not-freed.json");
    doc["results"][0]["reasons"][0]["since_not_measured"] = json!(true);
    assert_eq!(
        schemas.validate("why-not-freed", &doc),
        Vec::<String>::new()
    );

    doc["results"][0]["reasons"][0]["since_not_measured"] = json!("yes");
    assert!(!schemas.validate("why-not-freed", &doc).is_empty());
}

type Mutation = (&'static str, fn(&mut Value));

#[test]
fn schemas_reject_missing_mistyped_unknown_and_out_of_set_fields() {
    let schemas = Schemas::load();
    let base = golden("inspect.json");
    let mutations: [Mutation; 5] = [
        ("missing", |d| {
            d.as_object_mut().unwrap().remove("completion");
        }),
        ("mistyped", |d| d["schema_version"] = json!("1")),
        ("unknown", |d| d["results"][0]["entry"]["bogus"] = json!(0)),
        ("out of set", |d| d["completion"] = json!("done")),
        ("negative", |d| {
            d["results"][0]["entry"]["metrics"]["logical"] = json!(-1)
        }),
    ];
    for (name, mutate) in mutations {
        let mut doc = base.clone();
        mutate(&mut doc);
        assert!(
            !schemas.validate("inspect", &doc).is_empty(),
            "{name} field accepted"
        );
    }
}

#[test]
fn state_dir_warnings_fit_the_scan_schema() {
    let schemas = Schemas::load();
    let mut doc = golden("scan.json");
    doc["filesystems"][0]["warnings"] = json!([
        {"code": "state_dir_in_selection", "message": "m"},
        {"code": "state_dir_changed", "message": "m"},
        {"code": "state_dir_writes_ignored", "message": "m"}
    ]);
    assert_eq!(schemas.validate("scan", &doc), Vec::<String>::new());
}

#[test]
fn ignored_state_dir_writes_fit_the_scan_schema() {
    let schemas = Schemas::load();
    let mut doc = golden("scan.json");
    doc["filesystems"][0]["state_dir_writes"] = json!([{
        "subvolume_id": 256,
        "generation_before": 20,
        "generation_after": 23,
        "inodes": 12
    }]);
    assert_eq!(schemas.validate("scan", &doc), Vec::<String>::new());
    doc["filesystems"][0]["state_dir_writes"][0]["inodes"] = json!(0);
    assert!(!schemas.validate("scan", &doc).is_empty());
}

#[test]
fn scan_hints_fit_every_answer_schema() {
    let schemas = Schemas::load();
    let hints = json!([
        {"path": "/volume1", "command": "synoreclaim scan /volume1", "last_scan": null},
        {
            "path": "/volume2",
            "command": "synoreclaim scan /volume2",
            "last_scan": {"finished": "2026-09-28T10:00:00Z", "elapsed_ms": 911000, "entries": 5510000}
        }
    ]);
    for command in ["top", "inspect", "shared", "simulate-delete"] {
        let mut doc = golden(&format!("{command}.json"));
        doc["scan_hints"] = hints.clone();
        assert_eq!(
            schemas.validate(command, &doc),
            Vec::<String>::new(),
            "{command}"
        );
    }
    let mut overview = golden("overview.json");
    overview["scan_hints"] = hints;
    assert_eq!(
        schemas.validate("overview", &overview),
        Vec::<String>::new(),
        "overview"
    );
    let mut doc = golden("top.json");
    doc["scan_hints"] = json!([{"path": "/volume1", "last_scan": null}]);
    assert!(!schemas.validate("top", &doc).is_empty());
}

#[test]
fn ranked_and_simulated_entries_may_name_their_package_data() {
    let schemas = Schemas::load();
    let data = json!({
        "service": "active_backup_data",
        "package": "Active Backup for Business (backup data)",
        "manage_in": "Active Backup for Business",
        "manage_in_id": "active_backup_business",
        "confirmed": true,
        "marker": "@ActiveBackup",
        "holds": false,
        "message": "package-managed data (@ActiveBackup): managed by …"
    });
    for (command, entries) in [("top", "entries"), ("simulate-delete", "selection")] {
        let mut doc = golden(&format!("{command}.json"));
        doc["results"][0][entries][0]["package_data"] = data.clone();
        assert_eq!(
            schemas.validate(command, &doc),
            Vec::<String>::new(),
            "{command}"
        );
        doc["results"][0][entries][0]["package_data"]["holds"] = json!("no");
        assert!(!schemas.validate(command, &doc).is_empty(), "{command}");
    }
}

#[test]
fn held_files_fit_the_snapshots_schema_named_or_not() {
    let schemas = Schemas::load();
    let mut doc = golden("snapshots.json");
    let file = |path: Value, live_path: Value| {
        json!({
            "snapshot_id": 257, "snapshot": {"path": "s1"}, "ino": 258,
            "path": path, "live_path": live_path, "in_live": false,
            "bytes": 4096, "unknown_bytes": 0
        })
    };
    doc["results"][0]["shares"][0]["held_files"] = json!({
        "complete": false,
        "count": 3,
        "files": [
            file(json!({"path": "a"}), json!({"path": "/v/data/a"})),
            file(Value::Null, Value::Null)
        ]
    });
    assert_eq!(schemas.validate("snapshots", &doc), Vec::<String>::new());
    doc["results"][0]["shares"][0]["held_files"] = Value::Null;
    assert_eq!(schemas.validate("snapshots", &doc), Vec::<String>::new());
    doc["results"][0]["shares"][0]["held_files"] = json!({"complete": true, "count": 0});
    assert!(!schemas.validate("snapshots", &doc).is_empty());
}

#[test]
fn a_bounded_snapshot_simulation_fits_the_schema_with_its_reasons() {
    let schemas = Schemas::load();
    let mut doc = golden("snapshots.json");
    let simulation = &mut doc["results"][0]["shares"][0]["simulation"];
    simulation["upper_bound_bytes"] = Value::Null;
    simulation["exact"] = json!(false);
    simulation["inexact_reasons"] = json!(["not_materialized", "cross_share_sharing"]);
    simulation["exact_command"] = json!("synoreclaim scan /v/data --snapshots all");
    assert_eq!(schemas.validate("snapshots", &doc), Vec::<String>::new());
    doc["results"][0]["shares"][0]["simulation"]["inexact_reasons"] = json!(["guess"]);
    assert!(!schemas.validate("snapshots", &doc).is_empty());
}

#[test]
fn why_not_freed_share_items_may_name_their_held_files() {
    let schemas = Schemas::load();
    let mut doc = golden("why-not-freed.json");
    let item = |files: Value| {
        json!({
            "subvolume_id": 261, "share": "photos", "bytes": 4096, "snapshots": 1,
            "newest_snapshot_otime": null, "manage_in": "Snapshot Replication > Snapshots",
            "manage_in_id": "snapshot_replication",
            "files": files
        })
    };
    let files = json!({"complete": true, "count": 1, "files": [{
        "snapshot_id": 1000, "snapshot": {"path": "s1"}, "ino": 257,
        "path": {"path": "a"}, "live_path": {"path": "/v/photos/a"},
        "in_live": false, "bytes": 4096, "unknown_bytes": 0
    }]});
    doc["results"][0]["reasons"][1]["items"] = json!([item(files), item(Value::Null)]);
    assert_eq!(
        schemas.validate("why-not-freed", &doc),
        Vec::<String>::new()
    );
    doc["results"][0]["reasons"][1]["items"][1]
        .as_object_mut()
        .unwrap()
        .remove("files");
    assert!(!schemas.validate("why-not-freed", &doc).is_empty());
}

#[test]
fn job_live_progress_carries_counts_and_a_previous_total() {
    let schemas = Schemas::load();
    let mut doc = golden("jobs.json");
    assert!(
        doc["jobs"][0].get("progress").is_none(),
        "the golden has no live progress: {doc:#}"
    );
    doc["jobs"][0]["progress"] = json!({
        "phase": "scan",
        "units_done": 12,
        "units_total": 190,
        "entries": 1200000,
        "items": 3400000,
        "bytes_read": 9437184,
        "elapsed_s": 148,
        "rss_bytes": 35651584,
        "subvolume_id": 256,
        "current_subvolume": {"path": "backup"},
        "basis": {"entries": 5510000, "elapsed_s": 900, "items": null},
        "updated": "2026-09-29T19:20:00Z"
    });
    assert_eq!(schemas.validate("jobs", &doc), Vec::<String>::new());
    doc["jobs"][0]["progress"]["basis"] = Value::Null;
    doc["jobs"][0]["progress"]["units_total"] = Value::Null;
    doc["jobs"][0]["progress"]["rss_bytes"] = Value::Null;
    doc["jobs"][0]["progress"]["subvolume_id"] = Value::Null;
    doc["jobs"][0]["progress"]["current_subvolume"] = Value::Null;
    assert_eq!(schemas.validate("jobs", &doc), Vec::<String>::new());
    doc["jobs"][0]["progress"]["bytes_read"] = json!(-1);
    assert!(!schemas.validate("jobs", &doc).is_empty());
}

#[test]
fn a_cleanup_job_carries_its_kind_and_item_counts() {
    let schemas = Schemas::load();
    let mut doc = golden("jobs.json");
    assert_eq!(doc["jobs"][0]["kind"], "scan");
    let job = &mut doc["jobs"][0];
    job["kind"] = json!("cleanup-apply");
    job["state"] = json!("running");
    job["phase"] = json!("execute");
    job["result"] = Value::Null;
    job["cleanup"] = json!({
        "plan_id": "cleanup-ab12-1",
        "items_total": 10,
        "items_done": 4,
        "moved": 2,
        "already_absent": 1,
        "partial": 1,
        "changed": 0,
        "failed": 0,
        "bytes_total": 4096000,
        "bytes_moved": 1630208,
        "calls_done": 7,
        "elapsed_s": 12,
        "updated": "2026-09-30T01:02:15Z"
    });
    assert_eq!(schemas.validate("jobs", &doc), Vec::<String>::new());
    doc["jobs"][0]["kind"] = json!("delete");
    assert!(!schemas.validate("jobs", &doc).is_empty());
    doc["jobs"][0]["kind"] = json!("cleanup-apply");
    doc["jobs"][0]["cleanup"]
        .as_object_mut()
        .unwrap()
        .remove("items_done");
    assert!(!schemas.validate("jobs", &doc).is_empty());
}

#[test]
fn a_rootfs_job_carries_moves_and_tools_progress() {
    let schemas = Schemas::load();
    let mut doc = golden("jobs.json");
    assert_eq!(schemas.validate("jobs", &doc), Vec::<String>::new());
    {
        let job = doc["jobs"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|job| job["kind"] == "rootfs-apply")
            .unwrap();
        assert_eq!(job["rootfs"]["phase"], "moves");
        job["rootfs"]["tool"] = json!("not_a_tool");
    }
    assert!(!schemas.validate("jobs", &doc).is_empty());
    {
        let job = doc["jobs"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|job| job["kind"] == "rootfs-apply")
            .unwrap();
        job["rootfs"]["tool"] = Value::Null;
        job["rootfs"]["bytes_done"] = json!(-1);
    }
    assert!(!schemas.validate("jobs", &doc).is_empty());
}

#[test]
fn non_utf8_paths_carry_hex() {
    let schemas = Schemas::load();
    let mut doc = golden("inspect.json");
    doc["results"][0]["entry"]["path_hex"] = json!("2f78ff");
    assert_eq!(schemas.validate("inspect", &doc), Vec::<String>::new());
    doc["results"][0]["entry"]["path_hex"] = json!("2F78FF");
    assert!(!schemas.validate("inspect", &doc).is_empty());
}

fn metrics_in<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(object) => {
            for (name, v) in object {
                if name == "metrics" || name == "totals" {
                    out.push(v);
                } else {
                    metrics_in(v, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| metrics_in(v, out)),
        _ => {}
    }
}

#[test]
fn reclaimable_is_null_exactly_when_references_are_unresolved() {
    let mut checked = 0;
    for (path, doc) in goldens() {
        let mut all = Vec::new();
        metrics_in(&doc, &mut all);
        for m in all.into_iter().filter(|m| !m.is_null()) {
            let n = |name: &str| m[name].as_u64().unwrap();
            let unknown = n("unknown");
            assert_eq!(m["reclaimable"].is_null(), unknown > 0, "{path:?}: {m}");
            if unknown == 0 {
                assert_eq!(n("reclaimable"), n("reclaimable_resolved"), "{path:?}");
            }
            assert_eq!(
                n("physical"),
                n("shared") + n("reclaimable_resolved") + unknown,
                "{path:?}: {m}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0);
}

fn every_change_reason() -> Value {
    json!([
        {
            "kind": "subvolume-generation-changed",
            "subvolume_id": 256,
            "selected": false,
            "generation_before": 20,
            "generation_after": 21
        },
        {"kind": "subvolume-appeared", "subvolume_id": 259, "generation": 40},
        {"kind": "subvolume-vanished", "subvolume_id": 257, "generation_before": 9},
        {"kind": "maintenance-running", "operation": "balance"},
        {"kind": "maintenance-probe-failed", "operation": "dev-replace", "errno": 1},
        {"kind": "maintenance-probe-failed", "operation": "balance", "errno": null},
        {"kind": "relocation-tree", "subvolume_id": 5}
    ])
}

#[test]
fn scans_and_reports_say_why_they_changed() {
    let schemas = Schemas::load();
    let mut scan = golden("scan.json");
    scan["filesystems"][0]["change_reasons"] = every_change_reason();
    assert_eq!(schemas.validate("scan", &scan), Vec::<String>::new());
    let mut inspect = golden("inspect.json");
    inspect["results"][0]["report"]["change_reasons"] = every_change_reason();
    assert_eq!(schemas.validate("inspect", &inspect), Vec::<String>::new());
}

#[test]
fn change_reasons_are_checked_by_kind() {
    let schemas = Schemas::load();
    let mutations: [Mutation; 4] = [
        ("unknown kind", |r| r[0]["kind"] = json!("bogus")),
        ("missing field", |r| {
            r[1].as_object_mut().unwrap().remove("generation");
        }),
        ("field of another kind", |r| r[3]["errno"] = json!(1)),
        ("unknown operation", |r| r[3]["operation"] = json!("scrub")),
    ];
    for (name, mutate) in mutations {
        let mut reasons = every_change_reason();
        mutate(&mut reasons);
        let mut scan = golden("scan.json");
        scan["filesystems"][0]["change_reasons"] = reasons;
        assert!(
            !schemas.validate("scan", &scan).is_empty(),
            "{name} accepted"
        );
    }
}

#[test]
fn a_cleanup_document_without_its_result_must_be_a_refusal() {
    let schemas = Schemas::load();
    for (name, command, field) in [
        ("cleanup-plan.json", "cleanup-plan", "shares"),
        ("cleanup-apply.json", "cleanup-apply", "moved"),
        ("cleanup-delete.json", "cleanup-delete", "targets"),
    ] {
        let mut doc = golden(name);
        doc.as_object_mut().unwrap().remove(field);
        assert!(!schemas.validate(command, &doc).is_empty(), "{name}");
        doc["completion"] = json!("busy");
        doc["errors"] = json!([{"path": "/p", "code": "busy", "message": "locked"}]);
        let errors = schemas.validate(command, &doc);
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
    let refused = golden("cleanup-plan-refused.json");
    assert_eq!(refused["errors"][0]["code"], "in_recycle_bin");
    let mut config = golden("config-show.json");
    config.as_object_mut().unwrap().remove("config");
    assert!(!schemas.validate("config-show", &config).is_empty());
    let refused = golden("config-show-refused.json");
    assert_eq!(refused["errors"][0]["code"], "config_untrusted");
}

/// Gives every `report_info`-shaped object (one with `stale` and
/// `scan_started`) the value `mark` as `cleanup_since_report`; returns
/// how many it changed.
fn mark_reports(value: &mut Value, mark: &Value) -> usize {
    match value {
        Value::Object(map) => {
            let own = usize::from(map.contains_key("stale") && map.contains_key("scan_started"));
            if own == 1 {
                map.insert("cleanup_since_report".into(), mark.clone());
            }
            own + map
                .values_mut()
                .map(|v| mark_reports(v, mark))
                .sum::<usize>()
        }
        Value::Array(items) => items.iter_mut().map(|v| mark_reports(v, mark)).sum(),
        _ => 0,
    }
}

#[test]
fn report_info_may_name_the_cleanup_that_made_it_stale() {
    let schemas = Schemas::load();
    let mark = json!({
        "plan_id": "cleanup-1",
        "execution_log": "cleanup-1-1.jsonl",
        "applied_at": "2026-09-29T12:00:00Z"
    });
    for name in ["top.json", "inspect.json", "overview.json"] {
        for mark in [&mark, &Value::Null] {
            let mut doc = golden(name);
            assert!(
                mark_reports(&mut doc, mark) > 0,
                "{name} has no report_info"
            );
            let command = doc["command"].as_str().unwrap().to_string();
            let errors = schemas.validate(&command, &doc);
            assert!(errors.is_empty(), "{name}: {errors:?}");
        }
    }
}

#[test]
fn a_rootfs_document_without_its_answer_must_be_a_refusal() {
    let schemas = Schemas::load();
    for (name, command, fields) in [
        (
            "rootfs.json",
            "rootfs",
            ["filesystem", "usage", "cleanable", "open_deleted"],
        ),
        (
            "rootfs-plan.json",
            "rootfs-plan",
            ["plan_file", "destination", "items", "apply"],
        ),
        (
            "rootfs-apply.json",
            "rootfs-apply",
            ["plan_id", "moved", "skipped", "root"],
        ),
    ] {
        let mut doc = golden(name);
        for field in fields {
            doc.as_object_mut().unwrap().remove(field);
        }
        assert!(!schemas.validate(command, &doc).is_empty(), "{name}");
        doc["completion"] = json!("failed");
        doc["errors"] =
            json!([{"path": "/", "code": "not_root", "message": "rootfs requires root"}]);
        let errors = schemas.validate(command, &doc);
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
}

#[test]
fn a_rootfs_apply_writer_lock_conflict_fits_its_schema() {
    let schemas = Schemas::load();
    let mut doc = golden("rootfs-apply.json");
    for field in [
        "plan_id",
        "plan_file",
        "move_dir",
        "manifest",
        "moved",
        "skipped",
        "root",
        "notice",
        "tools",
        "tools_notice",
    ] {
        doc.as_object_mut().unwrap().remove(field);
    }
    doc["completion"] = json!("busy");
    doc["errors"] = json!([{
        "path": "/",
        "code": "busy",
        "message": "another rootfs apply is running",
        "holder": {"job_id": "rootfs-job", "command": "rootfs-apply"}
    }]);

    let errors = schemas.validate("rootfs-apply", &doc);

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_rootfs_plan_with_nothing_to_move_has_no_plan_file() {
    let schemas = Schemas::load();
    let mut doc = golden("rootfs-plan.json");
    for field in ["plan_file", "plan_id", "expires_at", "apply"] {
        doc[field] = Value::Null;
    }
    doc["items"] = json!([]);
    let errors = schemas.validate("rootfs-plan", &doc);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_config_has_no_rootfs_switch() {
    let schemas = Schemas::load();
    let mut doc = golden("config-show.json");
    doc["config"]["rootfs_enabled"] = json!(true);
    assert!(!schemas.validate("config-show", &doc).is_empty());
}

#[test]
fn nas_scan_admission_refusal_names_the_existing_job_without_launching_another() {
    let mut value = read_json(&root().join("tests/golden/json/detach.json"));
    value["completion"] = json!("busy");
    value["job_id"] = Value::Null;
    value["outcome"] = json!("scan_in_progress");
    value["running_job_id"] = json!("s1");
    value["run"] = json!([]);
    value["result"] = Value::Null;
    assert!(Schemas::load().validate("detach", &value).is_empty());
    value["running_job_id"] = json!(42);
    assert!(!Schemas::load().validate("detach", &value).is_empty());
}

#[test]
fn cleanup_plan_params_accept_report_identities_and_reject_unsafe_shapes() {
    let schemas = Schemas::load();
    let valid = json!({"paths": ["/volume1/legacy", {"path":"/volume1/a", "expect": {
        "filesystem_uuid":"00112233-4455-6677-8899-aabbccddeeff", "tree":256, "objectid":300, "generation":9
    }}]});
    assert!(schemas.validate("cleanup-plan-params", &valid).is_empty());
    // No count bound: a larger selection is staged with cleanup.select.
    assert!(
        schemas
            .validate(
                "cleanup-plan-params",
                &json!({"paths": vec!["/volume1/a"; 1001]})
            )
            .is_empty()
    );
    for paths in [
        json!([]),
        json!([{"path":"/volume1/a","path_hex":"f"}]),
        json!([{"path":"/volume1/a","expect":{"tree":256,"objectid":300}}]),
    ] {
        assert!(
            !schemas
                .validate("cleanup-plan-params", &json!({"paths":paths}))
                .is_empty()
        );
    }
    for mut expected in [
        json!({"tree":256}),
        json!({"tree":-1,"objectid":300}),
        json!({"tree":256,"objectid":300,"bypass":true}),
    ] {
        expected["filesystem_uuid"] = json!("00112233-4455-6677-8899-aabbccddeeff");
        assert!(
            !schemas
                .validate(
                    "cleanup-plan-params",
                    &json!({"paths":[{"path":"/volume1/a","expect":expected}]})
                )
                .is_empty()
        );
    }
}

#[test]
fn ext4_cleanup_identity_shapes_are_explicit_and_strict() {
    let schemas = Schemas::load();
    let expected = json!({"filesystem_uuid":"00112233-4455-6677-8899-aabbccddeeff",
        "filesystem":"ext4", "tree":null, "inode_device":64769, "objectid":42});
    let value = json!({"paths":[{"path":"/volume1/a","expect":expected}]});
    assert!(schemas.validate("cleanup-plan-params", &value).is_empty());
    for (key, invalid) in [
        ("tree", json!(64769)),
        ("inode_device", json!(null)),
        ("filesystem", json!("btrfs")),
    ] {
        let mut value = value.clone();
        value["paths"][0]["expect"][key] = invalid;
        assert!(!schemas.validate("cleanup-plan-params", &value).is_empty());
    }
}

#[test]
fn a_plan_groups_items_by_shared_folder_with_its_recycle_bin() {
    let schemas = Schemas::load();
    let plan = golden("cleanup-plan.json");
    assert_eq!(plan["shares"][0]["recycle_bin"]["enabled"], true);
    // A shared folder without a Recycle Bin is planned too: its items are
    // removed for good, and the plan says why.
    assert_eq!(plan["shares"][1]["recycle_bin"]["enabled"], false);
    assert_eq!(plan["shares"][1]["delete"], true);
    assert_eq!(plan["shares"][1]["delete_reason"], "recycle_bin_off");
    assert_eq!(plan["warnings"][0]["code"], "deletes_for_good");
    assert!(schemas.validate("cleanup-plan", &plan).is_empty());
    let mut outside = plan.clone();
    outside["shares"][1]["delete_reason"] = json!("no_shared_folder");
    assert!(schemas.validate("cleanup-plan", &outside).is_empty());
    let mut unknown = plan.clone();
    unknown["shares"][1]["delete_reason"] = json!("shared_folder_off");
    assert!(!schemas.validate("cleanup-plan", &unknown).is_empty());
    // A plan always has an id now: nothing is left out for want of a Recycle Bin.
    for key in ["plan_id", "plan_file", "expires_at"] {
        let mut nothing = plan.clone();
        nothing[key] = Value::Null;
        assert!(!schemas.validate("cleanup-plan", &nothing).is_empty());
    }
    let mut bad = plan.clone();
    bad["shares"][0]["items"][0]["kind"] = json!("symlink");
    assert!(!schemas.validate("cleanup-plan", &bad).is_empty());
    let mut applied = golden("cleanup-apply.json");
    assert!(schemas.validate("cleanup-apply", &applied).is_empty());
    // An item in no shared folder is deleted for good: `deleted` is an outcome.
    applied["targets"][0]["outcome"] = json!("deleted");
    assert!(schemas.validate("cleanup-apply", &applied).is_empty());
    applied["targets"][0]["outcome"] = json!("erased");
    assert!(!schemas.validate("cleanup-apply", &applied).is_empty());
}

#[test]
fn top_metadata_summary_marks_bytes_already_in_folder_rows() {
    let schemas = Schemas::load();
    let schema = schemas.files.get("top.schema.json").unwrap();
    assert_eq!(
        schema.pointer("/allOf/1/properties/results/items/properties/services/properties/lines/items/properties/included_in_rows/type"),
        Some(&json!("boolean"))
    );
}

#[test]
fn top_nested_row_inclusion_has_a_lossless_optional_parent_path() {
    let schemas = Schemas::load();
    let schema = schemas.files.get("top.schema.json").unwrap();
    assert_eq!(schema.pointer("/allOf/1/properties/results/items/properties/entries/items/allOf/1/properties/within/$ref"),
        Some(&json!("common.schema.json#/$defs/path")));
    let mut doc = golden("top-volume.json");
    doc["results"][0]["entries"][0]["within"] =
        json!({"path": "/v/share", "path_hex": "2f762f7368617265"});
    assert!(schemas.validate("top", &doc).is_empty());
    doc["results"][0]["entries"][0]["within"] = json!(123);
    assert!(!schemas.validate("top", &doc).is_empty());
}

#[test]
#[ignore = "run after scripts/ext4-cleanup-integration.sh with EXT4_E2E_OUTPUT"]
fn loop_ext4_cleanup_envelopes_match_published_schemas() {
    let schemas = Schemas::load();
    let path = PathBuf::from(std::env::var_os("EXT4_E2E_OUTPUT").expect("fixture output"));
    let mut checked = 0;
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let value = read_json(&path);
        let Some(command) = value.get("command").and_then(Value::as_str) else {
            continue;
        };
        assert!(
            schemas.validate(command, &value).is_empty(),
            "{path:?}: {:?}",
            schemas.validate(command, &value)
        );
        checked += 1;
    }
    assert!(
        checked >= 15,
        "all successful and refusal paths must be exercised"
    );
}

#[test]
fn checkpoint_resume_refusal_keeps_the_reason_and_paused_state() {
    let schemas = Schemas::load();
    let mut doc = golden("resume.json");
    doc["outcome"] = json!("checkpoint_unusable");
    doc["state"] = json!("paused");
    doc["message"] = json!("Saved subvolume identity or generation changed; start a fresh scan");
    assert!(schemas.validate("resume", &doc).is_empty());
}
