use super::*;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// A service answer: a result, or a refusal code.
type Answer = Result<Value, &'static str>;

struct Served {
    /// Each service the App asked for, with its params.
    services: Vec<(String, Value)>,
    /// The `result` frame.
    result: Value,
}

/// Plays the Go runtime for one call of `method`, answering each service
/// with `answer`.
fn serve(
    app: &App,
    method: &str,
    params: Value,
    answer: impl FnMut(&str, &Value) -> Answer,
) -> Served {
    serve_as(
        app,
        method,
        params,
        json!({"user": "admin", "admin": true, "locale": "en",
            "request_id": "r1", "deadline_ms": 60000}),
        answer,
    )
}

fn serve_as(
    app: &App,
    method: &str,
    params: Value,
    context: Value,
    mut answer: impl FnMut(&str, &Value) -> Answer,
) -> Served {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let (r, w) = (theirs.try_clone().unwrap(), theirs);
    std::thread::scope(|scope| {
        scope.spawn(|| app.serve(r, w));
        let mut out = ours.try_clone().unwrap();
        let mut lines = BufReader::new(ours);
        let mut read = || {
            let mut line = String::new();
            lines.read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        };
        writeln!(out, r#"{{"t":"hello","versions":[1]}}"#).unwrap();
        assert_eq!(read()["t"], "hello");
        let call = json!({"t": "call", "id": "1", "method": method, "params": params,
            "ctx": context});
        writeln!(out, "{call}").unwrap();
        let mut services = Vec::new();
        loop {
            let frame = read();
            if frame["t"] == "result" {
                return Served {
                    services,
                    result: frame,
                };
            }
            let (op, params) = (
                frame["op"].as_str().unwrap().to_string(),
                frame["params"].clone(),
            );
            let reply = match answer(&op, &params) {
                Ok(result) => {
                    json!({"t": "svc_result", "id": frame["id"], "ok": true, "result": result})
                }
                Err(code) => {
                    json!({"t": "svc_result", "id": frame["id"], "ok": false, "error": {"code": code, "message": "refused"}})
                }
            };
            services.push((op, params));
            writeln!(out, "{reply}").unwrap();
        }
    })
}

#[test]
fn app_status_uses_verified_identity_without_executor_access() {
    for locale in ["de", "zh-TW", ""] {
        let served = serve_as(
            &app(None),
            "app.status",
            json!({"user": "spoofed", "admin": false, "locale": "ja"}),
            json!({"user": "operator", "admin": true, "locale": locale,
                "request_id": "r1", "deadline_ms": 60000}),
            |_, _| panic!("entry metadata must not use the executor"),
        );
        let manifest: Value = serde_json::from_str(include_str!("../../dsm-package.yaml")).unwrap();
        assert_eq!(served.result["ok"], true, "{}", served.result);
        assert_eq!(
            served.result["data"],
            json!({
                "package": "SynoReclaim",
                "version": format!("{}-{}", manifest["version"].as_str().unwrap(), manifest["build"]),
                "user": "operator", "admin": true, "lang": "", "locale": locale,
            })
        );
        assert!(served.services.is_empty());
    }
}

/// The entry page's DSM display-language hint decides lang/locale, as the
/// SDK's app_language does; "def" follows the browser again.
#[test]
fn app_status_answers_in_the_hinted_dsm_display_language() {
    for (params, lang, locale) in [
        (json!({"dsmLanguage": "cht"}), "cht", "zh-TW"),
        (json!({"dsmLanguage": "jpn"}), "jpn", "ja"),
        (json!({"dsmLanguage": "def"}), "", "de"),
    ] {
        let served = serve_as(
            &app(None),
            "app.status",
            params,
            json!({"user": "translator", "admin": true, "locale": "de",
                "request_id": "r1", "deadline_ms": 60000}),
            |_, _| panic!("entry metadata must not use the executor"),
        );
        assert_eq!(served.result["ok"], true, "{}", served.result);
        assert_eq!(served.result["data"]["lang"], lang);
        assert_eq!(served.result["data"]["locale"], locale);
        assert!(served.services.is_empty());
    }
}

#[test]
fn app_status_requires_an_authenticated_administrator() {
    for (user, admin, code) in [
        ("", false, "session_required"),
        ("operator", false, "admin_required"),
    ] {
        let served = serve_as(
            &app(None),
            "app.status",
            json!({}),
            json!({"user": user, "admin": admin, "locale": "en",
                "request_id": "r1", "deadline_ms": 60000}),
            |_, _| panic!("refused entry must not use the executor"),
        );
        assert_eq!(served.result["ok"], false);
        assert_eq!(served.result["error"]["code"], code);
        assert!(served.result.get("data").is_none());
        assert!(served.services.is_empty());
    }
}

fn output(exit_code: i32, document: Value) -> Answer {
    Ok(
        json!({"request_id": "f".repeat(64), "exit_code": exit_code, "stderr": "", "output": document.to_string()}),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("synoreclaim-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn registered_business_methods_equal_the_source_registry_and_generated_bridge() {
    let actual = app(None).describe();
    let mut all: Vec<&str> = actual["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    all.sort();
    let source = ctl::registry();
    let mut expected: Vec<&str> = source["operations"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    expected.sort();
    assert_eq!(
        all, expected,
        "source must equal actual handlers in both directions"
    );
    let manifest: Value = serde_json::from_str(include_str!("../../dsm-package.yaml")).unwrap();
    let declared: Vec<&str> = manifest["backend"]["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(all, declared, "generated Rust bridge methods drifted");
}

#[test]
fn ctl_scan_status_selects_the_exact_scan_and_keeps_its_identity() {
    let scan = json!({"job_id":"j-2", "kind":"scan", "state":"partial",
        "targets":[{"path":"/volume1/example"}], "profile":"low", "requested_by":"admin"});
    let served = serve(
        &app(None),
        "scans.status",
        json!({"job_id":"j-2"}),
        |_, _| {
            output(
                0,
                json!({"jobs":[{"job_id":"j-1","kind":"scan","state":"completed"},scan]}),
            )
        },
    );
    assert_eq!(served.services.len(), 1);
    assert_eq!(served.services[0].1["params"]["method"], "jobs.list");
    assert_eq!(served.services[0].1["params"]["params"], "{}");
    assert_eq!(served.result["ok"], true);
    assert_eq!(
        served.result["data"],
        json!({"job_id":"j-2", "kind":"scan", "state":"partial", "profile":"low", "target_count":1})
    );
}

#[test]
fn ctl_commands_declare_private_paths_and_selectors_write_only() {
    let registry = ctl::registry();
    for command in registry["operations"].as_object().unwrap().values() {
        if command["ai"] == "hidden" {
            continue;
        }
        for (name, field) in command["paramsSchema"]["properties"].as_object().unwrap() {
            if command["ai"] != "hidden"
                && ["target", "targets", "paths", "simulate"].contains(&name.as_str())
            {
                assert_eq!(field["writeOnly"], true, "{name}");
            }
        }
    }
}

#[test]
fn every_ctl_read_projects_fields_and_omits_private_payloads() {
    let registry = ctl::registry();
    let private = "private-url-token-passkey-file-content";
    for command in registry["operations"].as_object().unwrap().values() {
        if command["ai"] != "read" {
            continue;
        }
        let method = command["flow"]["method"].as_str().unwrap();
        let params = command["examples"][0].clone();
        let document = json!({"job_id":"j-example", "state":"partial", "kind":"scan", "profile":"low",
            "completion":"partial", "targets":[{"path":private,"path_hex":"736563726574"}],
            "jobs":[{"job_id":"j-example", "kind":"scan", "state":"partial", "profile":"low", "targets":[private],"requested_by":private,"token":private}],
            "results":[{"metrics":{"reclaimable":123,"logical":456,"token":private},"path":private,"message":private,"future_numeric":123}],
            "errors":[{"message":private,"code":private,"path":private}],
            "volumes":[{"mount":{"path":"/volume1"},"used_bytes":100,"shares":[{"name":private,"path":private}]}],
            "token":private,"url":private,"password":private,"content":private,"future":{"value":private}});
        let served = serve(&app(None), method, params, |op, _| match op {
            "root.run" => output(0, document.clone()),
            "root.jobs.list" => Ok(json!({"jobs":[]})),
            _ => panic!("unexpected service"),
        });
        assert_eq!(served.result["ok"], true, "{method}");
        let text = served.result["data"].to_string();
        assert!(!text.contains(private), "{method} leaked a private value");
        for forbidden in [
            "path_hex",
            "future_numeric",
            "future",
            "password",
            "token",
            "content",
            "url",
            "requested_by",
        ] {
            assert!(
                !text.contains(&format!("\"{forbidden}\"")),
                "{method}: {forbidden}"
            );
        }
    }
}

#[test]
fn ctl_scan_acceptance_exposes_only_accepted_and_job_id() {
    let served = serve(
        &app(None),
        "scans.start",
        json!({"targets":["/volume1/private"]}),
        |op, _| match op {
            "root.run" => output(
                0,
                json!({"command":"detach","job_id":"j-2","run":["/root/private","private-token"],"result":{"path":"/private"}}),
            ),
            "root.start" => Ok(json!({"job_id":"executor-job"})),
            _ => panic!("unexpected service"),
        },
    );
    assert_eq!(
        served.result["data"],
        json!({"accepted":true,"job_id":"j-2"})
    );
}

#[test]
fn ctl_errors_keep_known_refusal_codes_and_replace_messages_or_unknown_codes() {
    for code in ["ROOT_TIMEOUT", "private-url-token-passkey-file-content"] {
        let served = serve(&app(None), "scans.start", json!({}), |_, _| Err(code));
        assert_eq!(served.result["ok"], false);
        assert_eq!(
            served.result["error"]["message"],
            "Reclaim task failed; inspect authorization get or the DSM UI"
        );
        assert_eq!(
            served.result["error"]["code"],
            if code == "ROOT_TIMEOUT" {
                "ROOT_TIMEOUT"
            } else {
                "ctl_failed"
            }
        );
    }
}

#[test]
fn ctl_scan_status_rejects_invalid_ids_before_executor_access() {
    for params in [
        json!({"job_id":"../x"}),
        json!({"job_id":"j-1","extra":true}),
        json!({}),
    ] {
        let served = serve(&app(None), "scans.status", params, |_, _| {
            panic!("invalid params")
        });
        assert_eq!(served.result["ok"], false);
        assert!(served.services.is_empty());
    }
}

#[test]
fn ctl_scan_status_refuses_missing_non_scan_or_failed_inventory() {
    for (exit, document, code) in [
        (0, json!({"jobs":[]}), "scan_not_found"),
        (
            0,
            json!({"jobs":[{"job_id":"j-2","kind":"cleanup-apply","state":"completed"}]}),
            "scan_not_found",
        ),
        (2, json!({"jobs":[]}), "scan_status_failed"),
        (0, json!({}), "cli_error"),
    ] {
        let served = serve(
            &app(None),
            "scans.status",
            json!({"job_id":"j-2"}),
            |_, _| output(exit, document.clone()),
        );
        assert_eq!(served.result["error"]["code"], code);
    }
}

#[test]
fn ctl_scan_start_preserves_params_and_starts_only_once() {
    let params = json!({"targets":["/volume1/example"],"mode":"gentle","snapshots":false});
    let served = serve(
        &app(None),
        "scans.start",
        params.clone(),
        |op, _| match op {
            "root.run" => output(0, json!({"command":"detach","job_id":"j-2"})),
            "root.start" => Ok(json!({"job_id":"e".repeat(64)})),
            _ => panic!("unexpected service"),
        },
    );
    assert_eq!(served.result["ok"], true);
    assert_eq!(served.result["data"]["job_id"], "j-2");
    assert_eq!(served.services.len(), 2);
    assert_eq!(served.services[0].1["params"]["method"], "scan.start");
    assert_eq!(
        serde_json::from_str::<Value>(served.services[0].1["params"]["params"].as_str().unwrap())
            .unwrap(),
        params
    );
}

#[test]
fn ctl_scan_start_refusals_do_not_become_success_or_restart_an_existing_job() {
    for (exit, document) in [
        (2, json!({"command":"webapi","outcome":"invalid_param"})),
        (
            3,
            json!({"command":"detach","outcome":"scan_in_progress","running_job_id":"j-old"}),
        ),
        (0, json!({"command":"scan"})),
    ] {
        let served = serve(&app(None), "scans.start", json!({}), |op, _| {
            assert_eq!(op, "root.run");
            output(exit, document.clone())
        });
        assert_eq!(served.result["ok"], false);
        assert_eq!(served.services.len(), 1);
    }
}

#[test]
fn the_package_stop_and_purge_programs_are_not_the_sessions() {
    // The executor refuses a session the program its policy names for stop
    // or purge, so those steps need a program of their own.
    let manifest: Value = serde_json::from_str(include_str!("../../dsm-package.yaml")).unwrap();
    let programs = &manifest["root"]["programs"];
    let session = &programs[CLI];
    assert!(session["run"].is_object() && session["job"].is_object());
    for step in [
        &manifest["root"]["stop"],
        &manifest["uninstall"]["purge"]["root"],
    ] {
        let name = step.as_str().unwrap();
        assert_ne!(name, CLI);
        let program = &programs[name];
        assert_eq!(program["kind"], "binary", "{name}");
        assert!(
            program["run"].is_null() && program["job"].is_null(),
            "{name}"
        );
    }
}

#[test]
fn a_method_runs_the_root_program_and_answers_its_document_with_its_exit() {
    let served = serve(&app(None), "volumes.list", json!({}), |_, _| {
        output(0, json!({"command": "volumes", "volumes": []}))
    });
    assert_eq!(
        served.services,
        [(
            "root.run".to_string(),
            json!({"name": "cli", "params": {"method": "volumes.list", "params": "{}"}})
        )]
    );
    assert_eq!(served.result["ok"], true);
    assert_eq!(
        served.result["data"],
        json!({"command": "volumes", "volumes": [], "exit": 0})
    );
}

#[test]
fn a_refused_run_is_still_its_document() {
    let served = serve(&app(None), "cleanup.plan", json!({"x": 1}), |_, _| {
        output(2, json!({"command": "cleanup", "outcome": "invalid_param"}))
    });
    assert_eq!(served.services[0].1["params"]["params"], r#"{"x":1}"#);
    assert_eq!(served.result["data"]["exit"], 2);
}

#[test]
fn an_executor_refusal_reaches_the_browser_with_its_code() {
    let served = serve(&app(None), "rootfs.report", json!({}), |_, _| {
        Err("ROOT_TIMEOUT")
    });
    assert_eq!(served.result["ok"], false);
    assert_eq!(served.result["error"]["code"], "ROOT_TIMEOUT");
}

#[test]
fn output_that_is_no_document_is_a_cli_error() {
    let served = serve(&app(None), "volumes.list", json!({}), |_, _| {
        Ok(json!({"request_id": "f", "exit_code": 1, "stderr": "boom", "output": ""}))
    });
    assert_eq!(served.result["error"]["code"], "cli_error");
}

#[test]
fn a_queued_job_starts_as_a_root_job_and_its_id_is_kept() {
    let data = scratch("start");
    let detach = json!({"command": "detach", "launcher": "none", "job_id": "j-1"});
    let served = serve(
        &app(Some(data.clone())),
        "scan.start",
        json!({"targets": ["/volume1"]}),
        |op, _| match op {
            "root.run" => output(0, detach.clone()),
            "root.start" => Ok(json!({"job_id": "e".repeat(64), "state": "running"})),
            _ => Err("UNKNOWN_OP"),
        },
    );
    assert_eq!(
        served.services[1],
        (
            "root.start".to_string(),
            json!({"name": "cli", "params": {"job_id": "j-1"}})
        )
    );
    assert_eq!(served.result["data"]["job_id"], "j-1");
    assert_eq!(
        std::fs::read_to_string(data.join("root-jobs/j-1")).unwrap(),
        "e".repeat(64)
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_busy_executor_refuses_the_queued_job() {
    let detach = json!({"command": "detach", "launcher": "none", "job_id": "j-1"});
    let served = serve(
        &app(Some(scratch("busy"))),
        "cleanup.apply",
        json!({}),
        |op, _| match op {
            "root.run" => output(0, detach.clone()),
            _ => Err("ROOT_BUSY"),
        },
    );
    assert_eq!(served.result["error"]["code"], "ROOT_BUSY");
}

#[test]
fn a_joined_walk_starts_nothing_and_answers_the_running_job() {
    let detach = json!({"command": "detach", "launcher": "none", "job_id": "j-1", "outcome": "joined", "running_job_id": "j-1"});
    let served = serve(
        &app(Some(scratch("joined"))),
        "rootfs.report",
        json!({}),
        |op, _| match op {
            "root.run" => output(0, detach.clone()),
            _ => Err("UNKNOWN_OP"),
        },
    );
    assert_eq!(served.services.len(), 1);
    assert_eq!(served.result["data"]["job_id"], "j-1");
}

#[test]
fn a_refused_queue_starts_nothing() {
    let served = serve(
        &app(Some(scratch("refused"))),
        "scan.start",
        json!({}),
        |_, _| output(2, json!({"command": "scan"})),
    );
    assert_eq!(served.services.len(), 1);
}

#[test]
fn an_accepted_cancel_stops_the_root_job() {
    let data = scratch("cancel");
    std::fs::create_dir_all(data.join("root-jobs")).unwrap();
    std::fs::write(data.join("root-jobs/j-1"), "e".repeat(64)).unwrap();
    let served = serve(
        &app(Some(data.clone())),
        "job.cancel",
        json!({"job_id": "j-1"}),
        |op, _| match op {
            "root.run" => output(0, json!({"command": "job", "state": "cancelling"})),
            "root.jobs.stop" => Err("ROOT_JOB_NOT_FOUND"),
            _ => Err("UNKNOWN_OP"),
        },
    );
    assert_eq!(
        served.services[1],
        (
            "root.jobs.stop".to_string(),
            json!({"job_id": "e".repeat(64)})
        )
    );
    assert_eq!(served.result["data"]["exit"], 0);
    let refused = serve(
        &app(Some(data.clone())),
        "job.cancel",
        json!({"job_id": "j-1"}),
        |_, _| output(1, json!({"command": "job"})),
    );
    assert_eq!(refused.services.len(), 1);
    let unknown = serve(
        &app(Some(data.clone())),
        "job.cancel",
        json!({"job_id": "../x"}),
        |_, _| output(0, json!({"command": "job"})),
    );
    assert_eq!(unknown.services.len(), 1);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_cancelled_move_is_never_stopped_by_the_executor() {
    let data = scratch("cancel-move");
    std::fs::create_dir_all(data.join("root-jobs")).unwrap();
    std::fs::write(data.join("root-jobs/cleanup-18e7-1694"), "e".repeat(64)).unwrap();
    let served = serve(
        &app(Some(data.clone())),
        "job.cancel",
        json!({"job_id": "cleanup-18e7-1694"}),
        |op, _| match op {
            "root.run" => output(
                0,
                json!({"command": "job", "state": "cancelling", "outcome": "requested"}),
            ),
            _ => Err("UNKNOWN_OP"),
        },
    );
    // Only the job's own cancel request: it stops between DSM's runs.
    assert_eq!(served.services.len(), 1, "{:?}", served.services);
    assert_eq!(served.result["data"]["exit"], 0);
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_mode_change_reaches_the_running_job_without_restarting_it() {
    let data = scratch("mode");
    let served = serve(
        &app(Some(data.clone())),
        "job.mode",
        json!({"job_id": "j-1", "mode": "fast"}),
        |op, _| match op {
            "root.run" => output(0, json!({"command": "mode", "outcome": "requested"})),
            _ => Err("UNKNOWN_OP"),
        },
    );
    assert_eq!(served.services.len(), 1, "{:?}", served.services);
    assert_eq!(
        served.services[0],
        (
            "root.run".to_string(),
            json!({"name": "cli", "params": {"method": "job.mode", "params": json!({"job_id": "j-1", "mode": "fast"}).to_string()}})
        )
    );
    assert_eq!(served.result["data"]["outcome"], "requested");
    std::fs::remove_dir_all(&data).unwrap();
}

/// Plays the executor keeping `text` as a staging file: it creates the
/// staging folder when it spills and answers the bare file name.
fn stage(data: &Path, text: &str) -> String {
    let dir = data.join("executor-staging");
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!("root-{}", "a".repeat(32));
    std::fs::write(dir.join(&name), text).unwrap();
    name
}

#[test]
fn staged_output_is_read_by_name_and_removed() {
    let data = scratch("staging");
    let served = serve(
        &app(Some(data.clone())),
        "support_bundle.get",
        json!({}),
        |_, _| {
            let name = stage(&data, &json!({"command": "support_bundle"}).to_string());
            Ok(
                json!({"request_id": "f", "exit_code": 0, "stderr": "", "staging": name, "size": 28}),
            )
        },
    );
    assert_eq!(served.result["data"]["command"], "support_bundle");
    assert!(
        !data
            .join(format!("executor-staging/root-{}", "a".repeat(32)))
            .exists()
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_top_list_larger_than_a_reply_arrives_staged() {
    let data = scratch("staging-large");
    let entries: Vec<Value> = (0..2000)
        .map(|i| json!({"path": format!("/volume1/share/{i:0>40}"), "bytes": i}))
        .collect();
    let text = json!({"command": "top", "entries": entries}).to_string();
    assert!(text.len() > 64 << 10);
    let served = serve(
        &app(Some(data.clone())),
        "top.list",
        json!({"target": "/volume1", "kind": "all", "limit": 200}),
        |_, _| {
            let name = stage(&data, &text);
            Ok(
                json!({"request_id": "f", "exit_code": 0, "stderr": "", "staging": name, "size": text.len()}),
            )
        },
    );
    assert_eq!(
        served.result["data"]["entries"].as_array().unwrap().len(),
        2000
    );
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn staging_that_is_not_a_bare_staging_name_is_refused() {
    let data = scratch("staging-outside");
    let name = stage(&data, "{}");
    for staging in [
        "/etc/passwd".to_string(),
        "../x".to_string(),
        format!("../executor-staging/{name}"),
        format!("{}/executor-staging/{name}", data.display()),
        "root-xyz".to_string(),
        String::new(),
    ] {
        let served = serve(
            &app(Some(data.clone())),
            "support_bundle.get",
            json!({}),
            |_, _| {
                Ok(
                    json!({"request_id": "f", "exit_code": 0, "stderr": "", "staging": staging, "size": 1}),
                )
            },
        );
        assert_eq!(served.result["error"]["code"], "internal", "{staging}");
    }
    assert!(data.join("executor-staging").join(&name).exists());
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn a_missing_staging_folder_is_an_internal_error() {
    let data = scratch("staging-missing");
    let served = serve(
        &app(Some(data.clone())),
        "support_bundle.get",
        json!({}),
        |_, _| {
            Ok(
                json!({"request_id": "f", "exit_code": 0, "stderr": "", "staging": format!("root-{}", "a".repeat(32)), "size": 1}),
            )
        },
    );
    assert_eq!(served.result["error"]["code"], "internal");
    std::fs::remove_dir_all(&data).unwrap();
}

#[test]
fn the_status_is_ready_once_the_executor_answers() {
    let status = |code: Option<&'static str>| {
        serve(
            &app(None),
            "authorization.status",
            json!({}),
            move |_, _| code.map_or(Ok(json!({"jobs": []})), Err),
        )
        .result["data"]
            .clone()
    };
    let doc = |state: &str, reason: Value| json!({"command": "authorization", "schema_version": 1, "state": state, "reason": reason});
    assert_eq!(status(None), doc("ready", Value::Null));
    assert_eq!(
        status(Some("AUTH_DENIED")),
        doc("session_expired", json!("session_expired"))
    );
    assert_eq!(
        status(Some("ADMIN_REQUIRED")),
        doc("not_admin", json!("not_admin"))
    );
    assert_eq!(
        status(Some("APP_DENIED")),
        doc("not_admin", json!("not_admin"))
    );
    assert_eq!(
        status(Some("EXECUTOR_UNAVAILABLE")),
        doc("authorization_required", json!("grant_missing"))
    );
    assert_eq!(
        status(Some("EXECUTOR_PAUSED")),
        doc("authorization_required", json!("upgrade_reapproval"))
    );
    assert_eq!(
        status(Some("EXECUTOR_TIMEOUT")),
        doc("unavailable", json!("service_unavailable"))
    );
}

// Approval adds no operation to the policy or to the executor: the refusal
// reaches the browser instead of asking for approval again.
#[test]
fn the_status_fails_with_a_refusal_approval_cannot_fix() {
    for code in ["POLICY_DENIED", "UNKNOWN_OP"] {
        let served = serve(&app(None), "authorization.status", json!({}), |_, _| {
            Err(code)
        });
        assert_eq!(served.result["error"]["code"], code);
    }
}

// The SDK's Gate names a page without DSM's SynoToken and a stopped
// or starting package, and checks again on its own; the status asks once and
// leaves the refusal to it.
#[test]
fn the_status_leaves_a_refusal_the_sdk_names_to_the_browser() {
    for code in [
        "credential_not_ready",
        "APP_STOPPED",
        "APP_STARTING",
        "EXECUTOR_STARTING",
    ] {
        let served = serve(&app(None), "authorization.status", json!({}), |_, _| {
            Err(code)
        });
        assert_eq!(served.services.len(), 1, "{code}");
        assert_eq!(served.result["error"]["code"], code);
    }
}

// The window's call repeats a request the SDK refuses while the App starts;
// the App asks the executor once.
#[test]
fn any_other_call_refused_while_the_executor_starts_reaches_the_browser() {
    let detach = json!({"command": "detach", "launcher": "none", "job_id": "j-1"});
    let served = serve(
        &app(Some(scratch("starting"))),
        "scan.start",
        json!({}),
        |op, _| match op {
            "root.run" => output(0, detach.clone()),
            _ => Err("EXECUTOR_STARTING"),
        },
    );
    assert_eq!(served.services.len(), 2);
    assert_eq!(served.result["error"]["code"], "EXECUTOR_STARTING");
}

#[test]
fn the_uninstall_hook_has_nothing_of_the_package_account_to_remove() {
    let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(crate::hook(&args(&["uninstall"])), Some(0));
    assert_eq!(
        crate::hook(&args(&["uninstall", "--delete-state"])),
        Some(0)
    );
    assert_eq!(crate::hook(&args(&[])), None);
    assert_eq!(crate::hook(&args(&["scan"])), Some(2));
}

// One cleanup plan took 30.8 s instead of about 1 s and left no
// trace of where; a slow root call is noted for the DSM log.
#[test]
fn a_slow_root_call_is_noted_with_its_planning_attempts() {
    let plan = json!({"command": "cleanup-plan", "planning_attempts": 2});
    assert_eq!(
        slow_call("cleanup.plan", Duration::from_millis(4999), &plan),
        None
    );
    assert_eq!(
        slow_call("cleanup.plan", Duration::from_millis(30_812), &plan).as_deref(),
        Some("cleanup.plan took 30812 ms, planning attempts 2")
    );
    assert_eq!(
        slow_call(
            "top.list",
            Duration::from_secs(6),
            &json!({"command": "top"})
        )
        .as_deref(),
        Some("top.list took 6000 ms")
    );
}

#[test]
fn ctl_diagnostics_selects_one_volume_and_reports_ambiguity_without_running_checks() {
    for (volumes, selected, code) in [
        (
            json!([{"mount":{"path":"/volume2"}}]),
            Some("/volume2"),
            None,
        ),
        (
            json!([{"mount":{"path":"/volume1"}},{"mount":{"path":"/volume2"}}]),
            None,
            Some("volume_required"),
        ),
        (json!([]), None, Some("no_volumes")),
    ] {
        let served = serve(&app(None), "ctl.doctor.run", json!({}), |op, params| {
            assert_eq!(op, "root.run");
            match params["params"]["method"].as_str().unwrap() {
                "volumes.list" => output(0, json!({"volumes":volumes})),
                "doctor.run" => {
                    assert_eq!(
                        Some(params["params"]["params"].as_str().unwrap()),
                        selected.map(|_| "{\"volume\":\"/volume2\"}")
                    );
                    output(0, json!({"checks":[], "completion":"complete"}))
                }
                other => panic!("unexpected method {other}"),
            }
        });
        if let Some(code) = code {
            assert_eq!(served.result["error"]["code"], code);
            assert_eq!(served.services.len(), 1);
            if code == "volume_required" {
                for volume in ["/volume1", "/volume2"] {
                    assert!(
                        served.result["error"]["message"]
                            .as_str()
                            .unwrap()
                            .contains(volume)
                    );
                }
            }
        } else {
            assert_eq!(served.result["ok"], true, "{}", served.result);
            assert_eq!(served.services.len(), 2);
        }
    }
    let served = serve(
        &app(None),
        "ctl.doctor.run",
        json!({"volume":"/volume1"}),
        |_, params| {
            assert_eq!(params["params"]["method"], "doctor.run");
            output(0, json!({"checks":[]}))
        },
    );
    assert_eq!(served.services.len(), 1);
    assert_eq!(served.result["ok"], true);
}

#[test]
fn unusable_resume_is_an_actionable_error_for_ui_and_ctl_without_starting_a_worker() {
    for (reason, expected) in [
        (
            "the subvolume identity changed (subvolume 256)",
            "Saved subvolume identity changed",
        ),
        (
            "namespace checkpoint format mismatch",
            "Saved checkpoint format is incompatible",
        ),
        ("saved subvolume 256 no longer exists", "no longer exists"),
        (
            "namespace checkpoint checksum mismatch",
            "missing or damaged",
        ),
        ("checkpoint trees are being relocated", "being relocated"),
        ("a completed subvolume changed", "generation changed"),
        (
            "this job has no durable checkpoint",
            "No saved scan position",
        ),
        (
            "damaged or incompatible completed-unit manifest",
            "completed-unit manifest",
        ),
    ] {
        for method in ["job.resume", "ctl.job.resume"] {
            let served = serve(&app(None), method, json!({"job_id":"scan-one"}), |op, _| {
                assert_eq!(op, "root.run");
                output(
                    4,
                    json!({"command":"resume", "outcome":"checkpoint_unusable", "message":format!("resume checkpoint cannot be used: {reason}; start a fresh scan")}),
                )
            });
            assert_eq!(served.result["ok"], false);
            assert_eq!(served.result["error"]["code"], "checkpoint_unusable");
            let message = served.result["error"]["message"].as_str().unwrap();
            assert!(message.contains(expected), "{message}");
            assert!(message.contains("start a fresh scan"), "{message}");
            assert!(!message.ends_with('.'));
            assert_eq!(served.services.len(), 1);
        }
    }
}

#[test]
fn every_resume_refusal_is_an_error_and_never_starts_a_worker() {
    for (outcome, exit, expected) in [
        ("busy", 4, "busy"),
        ("not_started", 1, "not_started"),
        ("finished", 1, "finished"),
        ("no_job", 1, "no_job"),
        ("unexpected/private-path", 2, "resume_failed"),
    ] {
        for method in ["job.resume", "ctl.job.resume"] {
            let served = serve(&app(None), method, json!({"job_id":"scan-one"}), |op, _| {
                assert_eq!(op, "root.run");
                output(
                    exit,
                    json!({"command":"resume", "outcome":outcome, "message":"private-path"}),
                )
            });
            assert_eq!(served.result["ok"], false, "{outcome}: {}", served.result);
            assert_eq!(served.result["error"]["code"], expected);
            assert!(!served.result.to_string().contains("private-path"));
            assert_eq!(served.services.len(), 1);
        }
    }
}

#[test]
fn a_paused_scan_resume_starts_a_new_executor_worker_for_the_original_job() {
    let data = scratch("resume-worker");
    std::fs::create_dir_all(data.join("root-jobs")).unwrap();
    std::fs::write(data.join("root-jobs/scan-one"), "old-worker").unwrap();
    for method in ["job.resume", "ctl.job.resume"] {
        let served = serve(
            &app(Some(data.clone())),
            method,
            json!({"job_id":"scan-one", "mode":"gentle"}),
            |op, params| match op {
                "root.run" => {
                    assert_eq!(params["params"]["method"], "job.resume");
                    assert_eq!(
                        serde_json::from_str::<Value>(params["params"]["params"].as_str().unwrap())
                            .unwrap(),
                        json!({"job_id":"scan-one", "mode":"gentle"})
                    );
                    output(
                        0,
                        json!({"command":"detach", "job_id":"scan-one", "launcher":"none"}),
                    )
                }
                "root.start" => {
                    assert_eq!(
                        params,
                        &json!({"name":"cli", "params":{"job_id":"scan-one"}})
                    );
                    Ok(json!({"job_id":"resumed-worker", "state":"running"}))
                }
                _ => panic!("unexpected service {op}"),
            },
        );
        assert_eq!(served.result["ok"], true, "{}", served.result);
        assert_eq!(served.services.len(), 2);
        assert_eq!(
            std::fs::read_to_string(data.join("root-jobs/scan-one")).unwrap(),
            "resumed-worker"
        );
    }
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn explicit_fresh_recovery_starts_the_replacement_job_with_saved_settings() {
    let data = scratch("fresh-worker");
    for method in ["job.resume", "ctl.job.resume"] {
        let served = serve(
            &app(Some(data.clone())),
            method,
            json!({"job_id":"old-scan", "fresh":true}),
            |op, params| match op {
                "root.run" => {
                    assert_eq!(params["params"]["method"], "job.resume");
                    assert_eq!(
                        serde_json::from_str::<Value>(params["params"]["params"].as_str().unwrap())
                            .unwrap(),
                        json!({"job_id":"old-scan", "fresh":true})
                    );
                    output(
                        0,
                        json!({"command":"detach", "job_id":"fresh-scan", "launcher":"none"}),
                    )
                }
                "root.start" => {
                    assert_eq!(
                        params,
                        &json!({"name":"cli", "params":{"job_id":"fresh-scan"}})
                    );
                    Ok(json!({"job_id":"fresh-worker", "state":"running"}))
                }
                _ => panic!("unexpected service {op}"),
            },
        );
        assert_eq!(served.result["ok"], true, "{}", served.result);
        assert_eq!(served.result["data"]["job_id"], "fresh-scan");
        assert_eq!(served.services.len(), 2);
        assert_eq!(
            std::fs::read_to_string(data.join("root-jobs/fresh-scan")).unwrap(),
            "fresh-worker"
        );
        assert!(!data.join("root-jobs/old-scan").exists());
    }
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn destructive_previews_only_check_authorization_and_never_submit_root_work() {
    for (method, input) in [
        (
            "preview.cleanup_apply",
            json!({"plan_id":"cleanup-aa-bb","confirm_move":true}),
        ),
        (
            "preview.rootfs_apply",
            json!({"plan_id":"rootfs-aa-bb","confirm_no_restore":true}),
        ),
        ("preview.cache_clear", json!({"volume":"/volume1"})),
    ] {
        let served = serve(&app(None), method, input, |op, _| {
            assert_eq!(op, "root.jobs.list", "preview dispatched root work");
            Ok(json!({"jobs":[]}))
        });
        assert_eq!(served.result["ok"], true, "{method}");
        assert_eq!(served.services.len(), 1);
        assert_eq!(served.result["data"]["confirmation_required"], true);
        assert_eq!(served.result["data"]["effect"], "destructive");
        assert_eq!(served.result["data"]["authorization"]["state"], "ready");
        assert!(
            served.result["data"]["limitations"]
                .as_str()
                .unwrap()
                .contains("freshness")
        );
    }
}

/// Export real bridge handler results for the CLI's JSON Schema validator.
/// These are generated test outputs, never a second response contract.
#[test]
fn registry_handler_response_corpus() {
    let registry = ctl::registry();
    let mut corpus = Vec::new();
    for (method, op) in registry["operations"].as_object().unwrap() {
        if op["ai"] == "hidden" {
            continue;
        }
        let backend = method.trim_start_matches("ctl.");
        let mut documents = vec![
            json!({"completion":"complete", "jobs":[{"job_id":"j-example","kind":"scan","state":"partial"}]}),
        ];
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ui/fixtures")
            .join(backend);
        if let Ok(files) = std::fs::read_dir(fixtures) {
            for file in files {
                let path = file.unwrap().path();
                if path.extension().and_then(|v| v.to_str()) == Some("json")
                    && path.file_name().unwrap() != "index.json"
                {
                    documents.push(serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap());
                }
            }
        }
        if op["longRunning"] == true || backend == "rootfs.report" {
            documents.push(json!({"command":"detach","job_id":"j-example"}));
        }
        for doc in documents {
            let params = op["examples"][0].clone();
            let served = serve(&app(None), method, params, |service, input| match service {
                "root.run"
                    if input["params"]["method"] == "volumes.list" && backend == "doctor.run" =>
                {
                    output(0, json!({"volumes":[{"mount":{"path":"/volume1"}}]}))
                }
                "root.run" if method == "scans.start" => {
                    output(0, json!({"command":"detach","job_id":"j-example"}))
                }
                "root.run" => output(0, doc.clone()),
                "root.start" => Ok(json!({"job_id":"root-job"})),
                "root.jobs.list" => Ok(json!({"jobs":[]})),
                _ => panic!("unexpected service: {service}"),
            });
            assert_eq!(served.result["ok"], true, "{method}: {}", served.result);
            corpus.push(json!({"method":method,"result":served.result["data"]}));
        }
    }
    // Capture the real App/ctl adapter across a stateful executor fixture. The
    // Node CLI tests replay these checked handler results, not hand-written replies.
    let data = scratch("ctl-scan-controls");
    std::fs::create_dir_all(data.join("root-jobs")).unwrap();
    std::fs::write(data.join("root-jobs/scan-one"), "first-worker").unwrap();
    let mut state = "running";
    let mut profile = "fast";
    let mut worker = true;
    for (method, params, expected_state, expected_profile, expected_worker) in [
        (
            "ctl.job.pause",
            json!({"job_id":"scan-one"}),
            "paused",
            "fast",
            false,
        ),
        ("ctl.jobs.list", json!({}), "paused", "fast", false),
        (
            "ctl.job.resume",
            json!({"job_id":"scan-one","mode":"gentle"}),
            "running",
            "low",
            true,
        ),
        ("ctl.jobs.list", json!({}), "running", "low", true),
        (
            "ctl.job.mode",
            json!({"job_id":"scan-one","mode":"fast"}),
            "running",
            "fast",
            true,
        ),
        ("ctl.jobs.list", json!({}), "running", "fast", true),
        (
            "ctl.job.mode",
            json!({"job_id":"scan-one","mode":"gentle"}),
            "running",
            "low",
            true,
        ),
        ("ctl.jobs.list", json!({}), "running", "low", true),
        (
            "ctl.job.cancel",
            json!({"job_id":"scan-one"}),
            "cancelled",
            "low",
            false,
        ),
        ("ctl.jobs.list", json!({}), "cancelled", "low", false),
    ] {
        let served = serve(
            &app(Some(data.clone())),
            method,
            params.clone(),
            |service, input| match service {
                "root.run" => match input["params"]["method"].as_str().unwrap() {
                    "jobs.list" => output(
                        0,
                        json!({"command":"jobs", "jobs":[{
                        "job_id":"scan-one", "kind":"scan", "state":state, "phase":"scan",
                        "profile":profile, "progress":{"entries":123,"items":456}}]}),
                    ),
                    "job.pause" => {
                        assert_eq!(state, "running");
                        state = "paused";
                        worker = false;
                        output(0, json!({"outcome":"requested", "state":state}))
                    }
                    "job.resume" => {
                        assert_eq!(state, "paused");
                        output(0, json!({"command":"detach", "job_id":"scan-one"}))
                    }
                    "job.mode" => {
                        assert_eq!(state, "running");
                        assert!(worker);
                        profile = if params["mode"] == "gentle" {
                            "low"
                        } else {
                            "fast"
                        };
                        output(0, json!({"outcome":"requested", "state":state}))
                    }
                    "job.cancel" => {
                        state = "cancelled";
                        output(0, json!({"outcome":"requested", "state":state}))
                    }
                    other => panic!("unexpected root method {other}"),
                },
                "root.start" => {
                    assert_eq!(input["params"]["job_id"], "scan-one");
                    profile = if params["mode"] == "gentle" {
                        "low"
                    } else {
                        "fast"
                    };
                    state = "running";
                    worker = true;
                    Ok(json!({"job_id":"resumed-worker"}))
                }
                "root.jobs.stop" => {
                    assert_eq!(input["job_id"], "resumed-worker");
                    worker = false;
                    Ok(json!({}))
                }
                other => panic!("unexpected service {other}"),
            },
        );
        assert_eq!(served.result["ok"], true, "{}", served.result);
        assert_eq!(
            (state, profile, worker),
            (expected_state, expected_profile, expected_worker)
        );
        corpus.push(
            json!({"scenario":"scan-controls", "method":method, "params":params,
            "result":served.result["data"], "worker_running":worker}),
        );
    }
    std::fs::remove_dir_all(data).unwrap();
    // Root-worker responses for a scan whose business data changed while paused.
    let mut running = false;
    for (method, params) in [
        ("ctl.job.pause", json!({"job_id":"scan-written"})),
        ("ctl.jobs.list", json!({})),
        (
            "ctl.job.resume",
            json!({"job_id":"scan-written","mode":"gentle"}),
        ),
        ("ctl.jobs.list", json!({})),
    ] {
        let served = serve(
            &app(None),
            method,
            params.clone(),
            |service, input| match service {
                "root.run" => match input["params"]["method"].as_str().unwrap() {
                    "job.pause" => output(0, json!({"outcome":"requested", "state":"paused"})),
                    "job.resume" => output(0, json!({"command":"detach", "job_id":"scan-written"})),
                    "jobs.list" => output(
                        0,
                        json!({"jobs":[{"job_id":"scan-written", "kind":"scan", "state":if running {"running"} else {"paused"},
                    "progress": if running { json!({"entries":123,"items":480,"bytes_read":9000,
                        "restart":{"subvolume_id":256,"entries":12,"items":24,"bytes_read":768}}) }
                    else { json!({"entries":123,"items":456,"bytes_read":8232}) }}]}),
                    ),
                    other => panic!("unexpected root method {other}"),
                },
                "root.start" => {
                    assert_eq!(input["params"]["job_id"], "scan-written");
                    running = true;
                    Ok(json!({"job_id":"resumed-worker"}))
                }
                other => panic!("unexpected service {other}"),
            },
        );
        assert_eq!(served.result["ok"], true);
        corpus.push(json!({"scenario":"live-writes", "method":method,"params":params,"result":served.result["data"]}));
    }
    if let Some(path) = std::env::var_os("RECLAIM_CTL_RESULTS") {
        std::fs::write(path, serde_json::to_vec(&corpus).unwrap()).unwrap();
    }
    assert!(corpus.len() > 80);
}

#[test]
fn previews_drop_private_input_and_preserve_refusal_evidence() {
    let result = serve(
        &app(None),
        "preview.cleanup_plan",
        json!({"paths":["private-path"]}),
        |op, _| {
            assert_eq!(op, "root.jobs.list");
            Err("EXECUTOR_PAUSED")
        },
    );
    assert_eq!(result.result["data"]["selection"]["target_count"], 1);
    assert_eq!(
        result.result["data"]["authorization"]["state"],
        "authorization_required"
    );
    assert!(!result.result.to_string().contains("private-path"));
    for params in [
        json!({"plan_id":"../private"}),
        json!({"plan_id":"arbitrary"}),
    ] {
        let result = serve(&app(None), "preview.cleanup_apply", params, |_, _| {
            panic!("invalid identity reached executor")
        });
        assert_eq!(result.result["ok"], false);
    }
}

#[test]
fn product_job_status_supports_every_async_kind_without_resubmitting_work() {
    for kind in [
        "scan",
        "cleanup-plan",
        "rootfs-plan",
        "rootfs-report",
        "cleanup-apply",
        "rootfs-apply",
    ] {
        let result = serve(
            &app(None),
            "ctl.job.status",
            json!({"job_id":"j-example"}),
            |op, params| {
                assert_eq!(op, "root.run");
                assert_eq!(params["params"]["method"], "jobs.list");
                output(
                    0,
                    json!({"jobs":[{"job_id":"j-other","kind":kind,"state":"completed"},
                {"job_id":"j-example","kind":kind,"state":"running","requested_by":"private"}]}),
                )
            },
        );
        assert_eq!(
            result.result["data"],
            json!({"job_id":"j-example","kind":kind,"state":"running"})
        );
        assert_eq!(result.services.len(), 1);
    }
}
