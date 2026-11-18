//! Syno Reclaim's App. The SDK's Go runtime checks the DSM session and the
//! admin-only entry; business methods run the root program `cli` through the
//! executor (`root.run`), and queued jobs run as its root jobs.

use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use syno_pkg_land_app::refusals::{APP_STARTING, APP_STOPPED, EXECUTOR_STARTING};
use syno_pkg_land_app::{App, Context, Error, Params, app_language};

mod ctl;
mod staging;

/// The WebAPI methods the root program answers; its policy schema allows
/// exactly these.
pub const CLI_METHODS: &[&str] = &[
    "cache.clear",
    "cache.status",
    "cleanup.apply",
    "cleanup.plan",
    "doctor.run",
    "duplicates.analyze",
    "duplicates.copies",
    "duplicates.keep",
    "duplicates.preferred_shares",
    "duplicates.scope",
    "history.list",
    "inspect.get",
    "job.cancel",
    "job.mode",
    "job.pause",
    "job.result",
    "job.resume",
    "jobs.list",
    "overview.get",
    "recycle.list",
    "rootfs.apply",
    "rootfs.history",
    "rootfs.plan",
    "rootfs.report",
    "scan.start",
    "session.get",
    "setup.cleanup",
    "simulate.get",
    "snapshots.list",
    "support_bundle.get",
    "top.list",
    "volumes.list",
    "watch_free.sample",
    "why_not_freed.get",
];

/// The methods that queue a job; the App then starts it with `root.start`.
const QUEUEING: &[&str] = &[
    "rootfs.report",
    "duplicates.analyze",
    "scan.start",
    "cleanup.plan",
    "rootfs.plan",
    "cleanup.apply",
    "rootfs.apply",
    "job.resume",
];

/// The policy's root program for runs and jobs.
const CLI: &str = "cli";

/// Where each queued job's executor job id is kept, below the data directory.
const ROOT_JOBS: &str = "root-jobs";

/// A root call at least this slow is noted in the DSM log.
const SLOW_CALL: Duration = Duration::from_secs(5);

/// The App, keeping its executor job ids below `data_dir`.
pub fn app(data_dir: Option<PathBuf>) -> App {
    let data_dir = Arc::new(data_dir);
    let mut app = App::new();
    app.method("app.status", entry_status);
    ctl::register(&mut app, data_dir.clone());
    for &method in CLI_METHODS {
        let data_dir = data_dir.clone();
        app.method(method, move |ctx, params| {
            cli(ctx, method, params.get(), data_dir.as_deref())
        });
    }
    // A chunk of a large selection: the root program stages it as
    // `cleanup.plan {stage}`, so its methods and approved policy stay as
    // they are.
    let staging = data_dir.clone();
    app.method("cleanup.select", move |ctx, params| {
        let stage = format!(r#"{{"stage":{}}}"#, params.get());
        cli(ctx, "cleanup.plan", &stage, staging.as_deref())
    });
    app.method("authorization.status", |ctx, _| status(ctx));
    app
}

/// Phone-entry metadata from the verified bridge context, available before
/// executor approval. No settings, root calls or browser-supplied identity.
/// lang/locale are the caller's DSM display language as the SDK decides it:
/// the entry page's dsmLanguage hint, else the browser's language.
fn entry_status(ctx: &Context, params: &Params) -> Result<Value, Error> {
    if ctx.user.is_empty() {
        return Err(Error::new("session_required", "Sign in to DSM again"));
    }
    if !ctx.admin {
        return Err(Error::new("admin_required", "An administrator is required"));
    }
    let manifest: Value =
        serde_json::from_str(include_str!("../../dsm-package.yaml")).map_err(|_| internal())?;
    let version = manifest["version"].as_str().ok_or_else(internal)?;
    let build = manifest["build"].as_u64().ok_or_else(internal)?;
    let (lang, locale) = app_language(ctx, params);
    Ok(json!({
        "package": "SynoReclaim",
        "version": format!("{version}-{build}"),
        "user": ctx.user,
        "admin": ctx.admin,
        "lang": lang,
        "locale": locale,
    }))
}

/// The exit code of a start with `args` (without the program) that is not
/// a request, `None` for the runtime's start. `uninstall` is the package's
/// uninstall hook: the package account keeps nothing of its own to remove,
/// as everything Reclaim keeps belongs to root and the root purge.
pub fn hook(args: &[OsString]) -> Option<i32> {
    match args.first()?.to_str() {
        Some("uninstall") => Some(0),
        _ => {
            eprintln!("synoreclaim-app: the runtime starts this App; it takes no arguments");
            Some(2)
        }
    }
}

/// A `root.run` result (protocol v1, Root programs).
#[derive(Deserialize)]
struct RootRun {
    exit_code: i32,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    staging: Option<String>,
}

/// Runs `method` as root and answers its document with its exit code; a
/// queued job then starts, and an accepted cancel stops its root job.
fn cli(ctx: &Context, method: &str, params: &str, data_dir: Option<&Path>) -> Result<Value, Error> {
    let started = Instant::now();
    let run: RootRun = ctx.runtime(
        "root.run",
        &json!({"name": CLI, "params": {"method": method, "params": params}}),
    )?;
    let text = match (run.output, run.staging) {
        (Some(output), _) => output,
        (None, Some(staging)) => staging::take(data_dir, &staging)?,
        (None, None) => return Err(internal()),
    };
    let mut document: Value = serde_json::from_str(&text).map_err(|_| no_document())?;
    if let Some(note) = slow_call(method, started.elapsed(), &document) {
        eprintln!("synoreclaim-app: {note}");
    }
    let object = document.as_object_mut().ok_or_else(no_document)?;
    // A refused resume must also be a UI error; otherwise callers treat the
    // delivered refusal document as acceptance and open a scan that never ran.
    if method == "job.resume" && run.exit_code != 0 {
        let code = object
            .get("outcome")
            .and_then(Value::as_str)
            .filter(|code| {
                matches!(
                    *code,
                    "checkpoint_unusable"
                        | "job_incompatible"
                        | "busy"
                        | "not_started"
                        | "finished"
                        | "no_job"
                        | "not_resumable"
                        | "scan_in_progress"
                )
            })
            .unwrap_or("resume_failed");
        return Err(Error::new(
            code,
            ctl::error_message(code, object.get("message").and_then(Value::as_str)),
        ));
    }
    // A detach with an outcome queued nothing: it joined a job already started.
    if run.exit_code == 0
        && QUEUEING.contains(&method)
        && object.get("command") == Some(&json!("detach"))
        && object.get("outcome").is_none()
    {
        let job_id = object
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or_else(no_document)?;
        start(ctx, job_id, data_dir)?;
    }
    if run.exit_code == 0 && method == "job.cancel" {
        stop(ctx, params, data_dir);
    }
    object.insert("exit".into(), json!(run.exit_code));
    Ok(document)
}

/// The note a root call of `method` that took `elapsed` leaves in the DSM
/// log, with the planning attempts its answer counts; none when it was quick.
fn slow_call(method: &str, elapsed: Duration, document: &Value) -> Option<String> {
    if elapsed < SLOW_CALL {
        return None;
    }
    let took = format!("{method} took {} ms", elapsed.as_millis());
    Some(match document.get("planning_attempts") {
        Some(attempts) => format!("{took}, planning attempts {attempts}"),
        None => took,
    })
}

/// Starts queued job `job_id` as a root job and keeps its executor job id.
fn start(ctx: &Context, job_id: &str, data_dir: Option<&Path>) -> Result<(), Error> {
    let started: Value = ctx.runtime(
        "root.start",
        &json!({"name": CLI, "params": {"job_id": job_id}}),
    )?;
    if let (Some(dir), Some(root_job), true) =
        (data_dir, started["job_id"].as_str(), valid_id(job_id))
    {
        let dir = dir.join(ROOT_JOBS);
        let kept =
            std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join(job_id), root_job));
        if let Err(e) = kept {
            eprintln!("root job of {job_id}: {e}");
        }
    }
    Ok(())
}

/// Stops the root job of a job whose cancel the root program accepted, so
/// the executor ends it should it not stop by itself. Best effort: the
/// cancel already stands.
fn stop(ctx: &Context, params: &str, data_dir: Option<&Path>) {
    #[derive(Deserialize)]
    struct JobParams {
        job_id: String,
    }
    let Some(dir) = data_dir else { return };
    let Ok(JobParams { job_id }) = serde_json::from_str(params) else {
        return;
    };
    // A move into the Recycle Bin stops by itself between DSM's runs, and
    // must: killed inside one, DSM leaves items in its hidden staging folder,
    // and the executor kills a job its grace (10 s) after the stop, shorter
    // than one run of a large folder. Its cancel is the job's own request.
    if !valid_id(&job_id) || job_id.starts_with("cleanup-") {
        return;
    }
    let Ok(root_job) = std::fs::read_to_string(dir.join(ROOT_JOBS).join(&job_id)) else {
        return;
    };
    if let Err(e) = ctx.runtime::<Value>("root.jobs.stop", &json!({"job_id": root_job.trim()})) {
        eprintln!("root job of {job_id}: {e}");
    }
}

/// The approval status the UI's gate reads: whether the executor answers
/// for this administrator. A refusal that approval cannot fix, an operation
/// outside the policy or unknown to the executor, fails the call instead, as
/// does one the SDK's Gate names itself.
fn status(ctx: &Context) -> Result<Value, Error> {
    let (state, reason) = match ctx.runtime::<Value>("root.jobs.list", &json!({})) {
        Ok(_) => ("ready", None),
        Err(e) => match e.code.as_str() {
            "AUTH_DENIED" => ("session_expired", Some("session_expired")),
            "ADMIN_REQUIRED" | "APP_DENIED" | "INELIGIBLE" => ("not_admin", Some("not_admin")),
            "EXECUTOR_UNAVAILABLE" => ("authorization_required", Some("grant_missing")),
            "POLICY_DENIED" | "UNKNOWN_OP" => return Err(e),
            // The SDK's Gate names a page without DSM's SynoToken and a
            // stopped or starting package, and checks again on its own.
            "credential_not_ready" | APP_STOPPED | APP_STARTING | EXECUTOR_STARTING => {
                return Err(e);
            }
            "EXECUTOR_PAUSED" => ("authorization_required", Some("upgrade_reapproval")),
            _ => ("unavailable", Some("service_unavailable")),
        },
    };
    Ok(json!({"command": "authorization", "schema_version": 1, "state": state, "reason": reason}))
}

/// A Reclaim job id: what the root program accepts, never a path.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn no_document() -> Error {
    Error::new("cli_error", "the CLI returned no document")
}

fn internal() -> Error {
    Error::new("internal", "internal error")
}

#[cfg(test)]
mod tests;
