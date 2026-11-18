//! `synoreclaim webapi`: the one entry point the DSM package's WebAPI
//! shim runs. It turns a method name and one
//! params JSON object into the argv of a CLI command, run in-process
//! with the package's fixed state directory.

use crate::document::{Completion, Envelope};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The longest path param, in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// The largest params object, in bytes: 1000 paths of 4096 bytes with
/// their hex fit.
pub const MAX_PARAMS_BYTES: usize = 16 << 20;

/// The most targets one `scan.start` takes.
pub const MAX_SCAN_TARGETS: usize = 256;

/// The longest job ID a job method takes.
const MAX_JOB_ID_BYTES: usize = 64;

/// The longest `--requested-by` name, in bytes.
pub const MAX_REQUESTER_BYTES: usize = 256;

/// What a call may use besides its params.
pub struct Context<'a> {
    pub state_dir: &'a Path,
    /// Mount points of the local Btrfs volumes (`volumes`).
    pub volumes: &'a [PathBuf],
    /// The DSM user the call runs for.
    pub requested_by: &'a str,
}

/// A method or param the allowlist rejects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid {
    pub message: String,
}

impl Invalid {
    fn new(message: impl Into<String>) -> Self {
        Invalid {
            message: message.into(),
        }
    }
}

/// The package's data folder (`$SYNOPKG_PKGVAR`); its state lives in
/// `state/` below where this link points.
pub const PACKAGE_VAR: &str = "/var/packages/SynoReclaim/var";

/// Why a call did not run.
#[derive(Debug)]
pub enum Refusal {
    /// The method or its params break the allowlist (exit 2).
    Invalid(Invalid),
    /// The package state cannot be found (exit 1).
    Failed(String),
}

impl From<Invalid> for Refusal {
    fn from(invalid: Invalid) -> Self {
        Refusal::Invalid(invalid)
    }
}

/// The argv that answers `method` for `requested_by` with the params
/// on `input`, using the state in `state_dir`, or its answer in the UI
/// cache at `now` (`fsid_of` maps a mount to its filesystem).
pub fn prepare(
    method: &str,
    requested_by: &str,
    input: &mut impl Read,
    state_dir: &Path,
    volumes: &[PathBuf],
    fsid_of: &dyn Fn(&[u8]) -> Option<String>,
    now: u64,
) -> Result<Call, Refusal> {
    requester(requested_by)?;
    let params = read_params(input)?;
    let ctx = Context {
        state_dir,
        volumes,
        requested_by,
    };
    Ok(match plan(method, &params, &ctx)? {
        Call::Run(argv) => match crate::ui_cache::lookup(state_dir, &argv, fsid_of, now) {
            Some((exit_code, document)) => Call::Cached(exit_code, document),
            None => Call::Run(argv),
        },
        Call::Planning { method, params } => {
            match saved_rootfs_report(&method, &params, state_dir, now) {
                Some((exit_code, document)) => Call::Cached(exit_code, document),
                None => Call::Planning { method, params },
            }
        }
        call => call,
    })
}

/// The saved `rootfs.report` answer within the call's `max_age_s`, so a
/// reader sees it at once instead of queueing a walk of `/`.
fn saved_rootfs_report(
    method: &str,
    params: &[u8],
    state_dir: &Path,
    now: u64,
) -> Option<(i32, String)> {
    if method != "rootfs.report" {
        return None;
    }
    let max_age_s = serde_json::from_slice::<RootfsReportParams>(params)
        .ok()?
        .max_age_s?;
    let uid = crate::sys::effective_uid();
    match crate::rootfs::report_cache::load(state_dir, uid, max_age_s, now) {
        Ok(cached) => cached.map(|cached| (cached.exit_code, cached.document)),
        Err(error) => {
            eprintln!("synoreclaim: warning: rootfs report cache ignored: {error}");
            None
        }
    }
}

/// The package's state directory: `state` in the real path of its data
/// folder `package_var`, the one every `webapi` call uses.
pub fn package_state_dir(package_var: &Path) -> std::io::Result<PathBuf> {
    Ok(std::fs::canonicalize(package_var)?.join("state"))
}

/// What answers a call.
#[derive(Debug, PartialEq, Eq)]
pub enum Call {
    /// The CLI command with this argv.
    Run(Vec<OsString>),
    /// `session.get`: this binary and its cleanup switches.
    Session,
    /// A validated planning request, queued for the existing root-job launcher.
    Planning { method: String, params: Vec<u8> },
    /// A prewarmed answer: its exit code and document.
    Cached(i32, String),
}

/// The answer to `method` with `params`.
pub fn plan(method: &str, params: &[u8], ctx: &Context) -> Result<Call, Invalid> {
    if method == "session.get" {
        parse::<NoParams>(params)?;
        return Ok(Call::Session);
    }
    let argv = argv(method, params, ctx)?;
    // Staging a chunk of a selection answers at once; only a plan queues.
    if method == "cleanup.plan" && staged_chunk(params).is_some() {
        return Ok(Call::Run(argv));
    }
    if matches!(
        method,
        "cleanup.plan" | "rootfs.plan" | "rootfs.report" | "duplicates.analyze"
    ) {
        return Ok(Call::Planning {
            method: method.into(),
            params: params.to_vec(),
        });
    }
    Ok(Call::Run(argv))
}

/// What `session.get` answers; the shim adds the DSM user and whether
/// they are an administrator.
#[derive(Debug, Serialize)]
pub struct SessionDocument {
    #[serde(flatten)]
    envelope: Envelope,
    requested_by: String,
    #[serde(flatten)]
    cleanup: crate::guard::CleanupSwitches,
}

impl SessionDocument {
    pub fn new(
        requested_by: &str,
        cleanup: crate::guard::CleanupSwitches,
        now: SystemTime,
    ) -> Self {
        SessionDocument {
            envelope: Envelope::new("session", now),
            requested_by: requested_by.to_string(),
            cleanup,
        }
    }
}

/// Computes `argv` into the UI cache when it is a duplicates answer kept
/// there, and prints what it answered; `None` when it is not one, or it
/// could not run (it then runs in-process).
#[cfg(target_os = "linux")]
fn rebuild_cached(
    state_dir: &Path,
    argv: &[OsString],
    fsid_of: &dyn Fn(&[u8]) -> Option<String>,
    now: u64,
) -> Option<i32> {
    use std::io::Write;
    let volume = duplicates_cached_volume(state_dir, argv)?;
    let mount = volume.as_os_str().as_bytes();
    let fsid = fsid_of(mount)?;
    let exe = std::env::current_exe().ok()?;
    let uid = crate::sys::effective_uid();
    let (code, document, _) =
        crate::ui_cache::compute(state_dir, uid, &exe, &fsid, mount, argv, now).ok()??;
    // A child that did not answer is not the answer: run it in-process.
    if document.trim().is_empty() {
        return None;
    }
    match std::io::stdout().write_all(document.as_bytes()) {
        Ok(()) => Some(code),
        Err(e) => Some(failed(&e.to_string())),
    }
}

/// Runs `method` with the params on `input` in-process, with the state in
/// `state_dir`, and returns its exit code; a refused call prints its
/// `ErrorDocument` and exits 2.
#[cfg(target_os = "linux")]
pub fn serve(method: &str, requested_by: &str, input: &mut impl Read, state_dir: &Path) -> i32 {
    use clap::Parser;
    let mounted: Vec<(PathBuf, Option<String>)> = match crate::resolve::Environment::load() {
        Ok(env) => env
            .volumes
            .into_iter()
            .map(|v| {
                let fsid = v.fsid.ok().map(|id| crate::resolve::fsid_text(&id));
                (v.primary.mount_point, fsid)
            })
            .collect(),
        Err(e) => return failed(&format!("cannot list volumes: {e}")),
    };
    let volumes: Vec<PathBuf> = mounted.iter().map(|(m, _)| m.clone()).collect();
    let fsid_of = |mount: &[u8]| {
        mounted
            .iter()
            .find(|(m, _)| m.as_os_str().as_bytes() == mount)
            .and_then(|(_, fsid)| fsid.clone())
    };
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match prepare(
        method,
        requested_by,
        input,
        state_dir,
        &volumes,
        &fsid_of,
        now,
    ) {
        Ok(Call::Planning { method, params }) => {
            let ctx = Context {
                state_dir,
                volumes: &volumes,
                requested_by,
            };
            match crate::planning::queue(&method, &params, &ctx) {
                Ok(code) => code,
                Err(error) => failed(&error.to_string()),
            }
        }
        Ok(Call::Run(argv)) => {
            // A duplicates answer missing from the cache is computed
            // into it, as after an analysis; the next ask is answered
            // from the cache.
            if let Some(code) = rebuild_cached(state_dir, &argv, &fsid_of, now) {
                return code;
            }
            match crate::cli::Cli::try_parse_from(argv) {
                Ok(cli) => crate::app::run(cli),
                Err(e) => refuse(method, &Invalid::new(e.to_string())),
            }
        }
        Ok(Call::Cached(exit_code, document)) => {
            use std::io::Write;
            match std::io::stdout().write_all(document.as_bytes()) {
                Ok(()) => exit_code,
                Err(e) => failed(&e.to_string()),
            }
        }
        Ok(Call::Session) => {
            let cleanup = crate::config::location()
                .map(|location| crate::config::session_switches(&location))
                .unwrap_or_default();
            print(
                &SessionDocument::new(requested_by, cleanup, SystemTime::now()),
                0,
            )
        }
        Err(Refusal::Invalid(invalid)) => refuse(method, &invalid),
        Err(Refusal::Failed(message)) => failed(&message),
    }
}

#[cfg(target_os = "linux")]
fn refuse(method: &str, invalid: &Invalid) -> i32 {
    let doc = ErrorDocument::new(method, invalid, SystemTime::now());
    print(&doc, EXIT_INVALID)
}

/// Prints `doc` and returns `exit_code`.
#[cfg(target_os = "linux")]
fn print(doc: &impl Serialize, exit_code: i32) -> i32 {
    use std::io::Write;
    let text = serde_json::to_string_pretty(doc).unwrap_or_default() + "\n";
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => exit_code,
        Err(e) => failed(&e.to_string()),
    }
}

#[cfg(target_os = "linux")]
fn failed(message: &str) -> i32 {
    eprintln!("synoreclaim: error: {message}");
    1
}

/// SPEC 22.7: 2 is an invalid argument.
const EXIT_INVALID: i32 = 2;

/// The params object on `input`, refused beyond `MAX_PARAMS_BYTES`.
pub fn read_params(input: &mut impl Read) -> Result<Vec<u8>, Invalid> {
    let mut params = Vec::new();
    input
        .take(MAX_PARAMS_BYTES as u64 + 1)
        .read_to_end(&mut params)
        .map_err(|e| Invalid::new(format!("params: {e}")))?;
    if params.len() > MAX_PARAMS_BYTES {
        return Err(Invalid::new(format!(
            "params exceed {MAX_PARAMS_BYTES} bytes"
        )));
    }
    Ok(params)
}

/// `name`, the DSM login the shim ran the call for, when it is 1 to
/// `MAX_REQUESTER_BYTES` bytes without control characters.
pub fn requester(name: &str) -> Result<&str, Invalid> {
    if name.is_empty() || name.len() > MAX_REQUESTER_BYTES || name.chars().any(char::is_control) {
        return Err(Invalid::new("--requested-by must be a DSM user name"));
    }
    Ok(name)
}

/// The answer to a call the allowlist rejects (exit 2).
#[derive(Debug, Serialize)]
pub struct ErrorDocument {
    #[serde(flatten)]
    envelope: Envelope,
    method: String,
    outcome: &'static str,
    message: String,
}

impl ErrorDocument {
    pub fn new(method: &str, invalid: &Invalid, now: SystemTime) -> Self {
        let mut envelope = Envelope::new("webapi", now);
        envelope.completion = Completion::Failed;
        ErrorDocument {
            envelope,
            method: method.to_string(),
            outcome: "invalid_param",
            message: invalid.message.clone(),
        }
    }
}

/// The argv of the CLI command that answers `method` with `params`.
pub fn argv(method: &str, params: &[u8], ctx: &Context) -> Result<Vec<OsString>, Invalid> {
    let argv = match method {
        "volumes.list" => listing("volumes", params)?.state_dir(ctx),
        "jobs.list" => listing("jobs", params)?.state_dir(ctx),
        "history.list" => listing("history", params)?.state_dir(ctx),
        "rootfs.report" => rootfs_report(params, ctx)?,
        "doctor.run" => on_volume(&["doctor", "--json"], params, ctx)?,
        "overview.get" => on_volume(&["overview", "--json", "--no-scan"], params, ctx)?,
        "cache.status" => on_volume(&["cache", "status", "--json"], params, ctx)?,
        "cache.clear" => on_volume(&["cache", "clear", "--json"], params, ctx)?,
        "support_bundle.get" => on_volume(&["support-bundle", "--output=-"], params, ctx)?,
        "why_not_freed.get" => why_not_freed(params, ctx)?,
        "watch_free.sample" => {
            let p: VolumeParams = parse(params)?;
            Argv::new(&["watch-free", "--duration=2s", "--interval=2s", "--json"])
                .operands([volume(&p.volume, ctx)?])
        }
        "top.list" => top(params, ctx)?,
        "inspect.get" => on_paths(
            &["inspect", "--explain", "--json", "--no-scan"],
            100,
            params,
            ctx,
        )?,
        "simulate.get" => on_paths(
            &["simulate-delete", "--json", "--no-scan"],
            1000,
            params,
            ctx,
        )?,
        // The App's `cleanup.select` runs as `cleanup.plan {stage}`: the
        // root program's methods, and so its approved policy, stay the same.
        "cleanup.plan" => match staged_chunk(params) {
            Some(chunk) => cleanup_select(&chunk, ctx)?,
            None => cleanup_plan(params, ctx)?,
        },
        "cleanup.apply" => cleanup_apply(params, ctx)?,
        "rootfs.plan" => rootfs_plan(params, ctx)?,
        // C4 rootfs apply and history mappings; no caller-supplied plan path.
        "rootfs.apply" => rootfs_apply(params, ctx)?,
        "rootfs.history" => rootfs_history(params, ctx)?,
        "setup.cleanup" => setup_cleanup(params)?,
        "snapshots.list" => snapshots(params, ctx)?,
        "recycle.list" => recycle(params, ctx)?,
        "scan.start" => scan_start(params, ctx)?,
        "job.pause" => on_job("pause", params, ctx)?,
        "job.cancel" => on_job("cancel", params, ctx)?,
        "job.mode" => job_mode(params, ctx)?,
        "job.resume" => job_resume(params, ctx)?,
        "job.result" => job_result(params, ctx)?,
        "duplicates.analyze" => on_volume(&["duplicates", "analyze", "--json"], params, ctx)?,
        "duplicates.scope" => duplicates_scope(params, ctx)?,
        "duplicates.copies" => duplicates_copies(params, ctx)?,
        "duplicates.keep" => duplicates_keep(params, ctx)?,
        "duplicates.preferred_shares" => duplicates_preferred(params, ctx)?,
        _ => return Err(Invalid::new(format!("unknown method {method:?}"))),
    };
    Ok(argv.0)
}

/// A command that takes no params.
fn listing(command: &str, params: &[u8]) -> Result<Argv, Invalid> {
    parse::<NoParams>(params)?;
    Ok(Argv::new(&[command, "--json"]))
}

fn rootfs_report(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let params: RootfsReportParams = parse(params)?;
    let mut argv = Argv::new(&["rootfs", "--json"]);
    if let Some(max_age_s) = params.max_age_s {
        if max_age_s > crate::cli::ROOTFS_REPORT_MAX_AGE_SECS {
            return Err(Invalid::new(format!(
                "max_age_s must be at most {}",
                crate::cli::ROOTFS_REPORT_MAX_AGE_SECS
            )));
        }
        argv = argv.flag("max-age-s", &max_age_s.to_string());
    }
    Ok(argv.state_dir(ctx))
}

/// `words` on `{volume}`.
fn on_volume(words: &[&str], params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: VolumeParams = parse(params)?;
    Ok(Argv::new(words)
        .state_dir(ctx)
        .operands([volume(&p.volume, ctx)?]))
}

/// `words` on `{paths}`: 1 to `limit` paths on volumes.
fn on_paths(words: &[&str], limit: usize, params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: PathsParams = parse(params)?;
    let paths = paths_on_volumes("paths", &p.paths, limit, ctx)?;
    Ok(Argv::new(words).state_dir(ctx).operands(paths))
}

/// A report row's identity of a selected path: its inode must still be
/// the path's, else the plan refuses `target_changed`.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExpectIdentity {
    filesystem_uuid: String,
    #[serde(default)]
    filesystem: Option<String>,
    #[serde(default)]
    tree: Option<u64>,
    #[serde(default)]
    inode_device: Option<u64>,
    objectid: u64,
    #[serde(default)]
    generation: Option<u64>,
}

#[derive(Serialize)]
struct ExpectedPath {
    #[serde(with = "crate::report::hex_bytes")]
    path: Vec<u8>,
    expect: ExpectIdentity,
}

/// Cleanup alone accepts a report identity on each printed path.
fn cleanup_plan(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(untagged, deny_unknown_fields)]
    enum Target {
        Text(String),
        Printed {
            path: String,
            path_hex: Option<String>,
            expect: Option<ExpectIdentity>,
        },
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Duplicates {
        volume: PathParam,
        #[serde(default)]
        groups: Vec<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        #[serde(default)]
        paths: Vec<Target>,
        duplicates: Option<Duplicates>,
        /// A selection `cleanup.select` staged: paths and groups of any number.
        selection: Option<String>,
    }
    let p: Params = parse(params)?;
    if let Some(id) = &p.selection
        && !crate::commands::valid_selection_id(id)
    {
        return Err(Invalid::new(
            "selection must be an ID cleanup.select returned",
        ));
    }
    if p.paths.is_empty() && p.duplicates.is_none() && p.selection.is_none() {
        return Err(Invalid::new(
            "select paths, duplicates or a staged selection",
        ));
    }
    let duplicates = match &p.duplicates {
        None => None,
        Some(d) => {
            if d.groups.is_empty() && p.selection.is_none() {
                return Err(Invalid::new("duplicates.groups must hold a group id"));
            }
            for id in &d.groups {
                group_id(id)?;
            }
            // Each group is cleaned on its whole volume: the selection
            // holds no scope, and `cleanup plan` refuses one.
            let volume = volume(&d.volume, ctx)?;
            Some(
                serde_json::to_string(&serde_json::json!({
                    "volume": crate::output::JsonPath::new(volume.as_os_str().as_bytes()),
                    "groups": d.groups,
                }))
                .map_err(|e| Invalid::new(e.to_string()))?,
            )
        }
    };
    let mut paths = Vec::new();
    let mut expected = Vec::new();
    for target in p.paths {
        let (path, expect) = match target {
            Target::Text(path) => (PathParam::Text(path), None),
            Target::Printed {
                path,
                path_hex,
                expect,
            } => (PathParam::Printed(PrintedPath { path, path_hex }), expect),
        };
        let path = on_a_volume(&path, ctx)?;
        if let Some(expect) = expect {
            expected.push(ExpectedPath {
                path: path.as_os_str().as_bytes().to_vec(),
                expect,
            });
        }
        paths.push(path);
    }
    let mut argv = Argv::new(&["cleanup", "plan"]).words(&["--json", "--quiet"]);
    if let Some(id) = &p.selection {
        argv = argv.flag("selection", id);
    }
    if !expected.is_empty() {
        argv = argv.flag(
            "expect",
            &serde_json::to_string(&expected).map_err(|e| Invalid::new(e.to_string()))?,
        );
    }
    if let Some(duplicates) = duplicates {
        argv = argv.flag("duplicates", &duplicates);
    }
    Ok(argv.state_dir(ctx).operands(paths))
}

/// The chunk of `cleanup.plan {stage: chunk}`: the App's `cleanup.select`.
fn staged_chunk(params: &[u8]) -> Option<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_slice(params).ok()?;
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    serde_json::to_vec(object.get("stage")?).ok()
}

/// `cleanup.select`: one chunk of a selection, staged in root's state so
/// a selection of any size reaches `cleanup.plan` (`selection`).
fn cleanup_select(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(untagged, deny_unknown_fields)]
    enum Target {
        Text(String),
        Printed {
            path: String,
            path_hex: Option<String>,
            expect: Option<ExpectIdentity>,
        },
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        selection: Option<String>,
        #[serde(default)]
        paths: Vec<Target>,
        #[serde(default)]
        groups: Vec<String>,
    }
    #[derive(Serialize)]
    struct Staged {
        #[serde(with = "crate::report::hex_bytes")]
        path: Vec<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        objectid: Option<u64>,
    }
    let p: Params = parse(params)?;
    if p.paths.is_empty() && p.groups.is_empty() {
        return Err(Invalid::new("a chunk holds paths or groups"));
    }
    if let Some(id) = &p.selection
        && !crate::commands::valid_selection_id(id)
    {
        return Err(Invalid::new(
            "selection must be an ID cleanup.select returned",
        ));
    }
    for id in &p.groups {
        group_id(id)?;
    }
    let mut paths = Vec::with_capacity(p.paths.len());
    for target in p.paths {
        let (path, expect) = match target {
            Target::Text(path) => (PathParam::Text(path), None),
            Target::Printed {
                path,
                path_hex,
                expect,
            } => (PathParam::Printed(PrintedPath { path, path_hex }), expect),
        };
        paths.push(Staged {
            path: on_a_volume(&path, ctx)?.as_os_str().as_bytes().to_vec(),
            objectid: expect.map(|e| e.objectid),
        });
    }
    let chunk = serde_json::to_string(&serde_json::json!({ "paths": paths, "groups": p.groups }))
        .map_err(|e| Invalid::new(e.to_string()))?;
    let mut argv = Argv::new(&["cleanup", "select", "--json"]);
    if let Some(id) = &p.selection {
        argv = argv.flag("selection", id);
    }
    Ok(argv.flag("chunk", &chunk).state_dir(ctx))
}

/// The 1 to `limit` paths of list param `name`, each on a volume.
fn paths_on_volumes(
    name: &str,
    list: &[PathParam],
    limit: usize,
    ctx: &Context,
) -> Result<Vec<PathBuf>, Invalid> {
    if list.is_empty() || list.len() > limit {
        return Err(Invalid::new(format!("{name} must hold 1 to {limit} paths")));
    }
    list.iter().map(|path| on_a_volume(path, ctx)).collect()
}

/// A scan of `targets`, queued for the package's launcher; its detach
/// line names the job and the argv that runs it (SPEC 5.3).
#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AllSnapshots {
    #[default]
    All,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SnapshotMode {
    Enabled(bool),
    All(AllSnapshots),
}
impl Default for SnapshotMode {
    fn default() -> Self {
        Self::Enabled(false)
    }
}

fn scan_start(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        targets: Vec<PathParam>,
        mode: Mode,
        #[serde(default)]
        snapshots: SnapshotMode,
        #[serde(default)]
        snapshot_report: Option<String>,
        #[serde(default)]
        attribution_report: Option<String>,
    }
    let p: Params = parse(params)?;
    if p.attribution_report.is_some() && p.snapshot_report.is_some() {
        return Err(Invalid::new("choose one additional analysis"));
    }
    let targets = paths_on_volumes("targets", &p.targets, MAX_SCAN_TARGETS, ctx)?;
    let argv = Argv::new(&["scan", "--detach=none"]).flag("profile", p.mode.profile());
    let argv = match p.snapshots {
        SnapshotMode::Enabled(true) => argv.words(&["--snapshots"]),
        SnapshotMode::All(AllSnapshots::All) => argv.words(&["--snapshots=all"]),
        SnapshotMode::Enabled(false) => argv,
    };
    let argv = if let Some(id) = p.snapshot_report {
        if id.is_empty()
            || id.len() > MAX_JOB_ID_BYTES
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Invalid::new("invalid snapshot report id"));
        }
        if !matches!(p.snapshots, SnapshotMode::All(_)) || targets.len() != 1 {
            return Err(Invalid::new(
                "snapshot analysis needs all snapshots and one volume",
            ));
        }
        argv.flag("snapshot-report", &id)
    } else {
        argv
    };
    let argv = if let Some(id) = p.attribution_report {
        if id.parse::<crate::cli::SnapshotReport>().is_err()
            || targets.len() != 1
            || !matches!(p.snapshots, SnapshotMode::Enabled(false))
        {
            return Err(Invalid::new("deep attribution needs one volume report"));
        }
        argv.flag("attribution-report", &id)
    } else {
        argv
    };
    Ok(argv
        .flag("requested-by", ctx.requested_by)
        .words(&["--json", "--quiet"])
        .state_dir(ctx)
        .operands(targets))
}

/// The kill switch in the cleanup configuration (SPEC 5.7): no state.
fn setup_cleanup(params: &[u8]) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        mutations_disabled: bool,
    }
    let p: Params = parse(params)?;
    let switch = if p.mutations_disabled { "on" } else { "off" };
    Ok(Argv::new(&["config", "set"])
        .flag("mutations-disabled", switch)
        .words(&["--json"]))
}

/// The apply of plan `plan_id` the user confirmed, detached for the
/// package's launcher as a cleanup job; its detach line names the job (SPEC 5.7).
fn cleanup_apply(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        plan_id: String,
        confirm_move: bool,
        /// Required for a plan that removes items in no shared folder for good.
        #[serde(default)]
        confirm_delete: bool,
    }
    let p: Params = parse(params)?;
    let id = job_id(&p.plan_id)
        .ok()
        .filter(|id| crate::commands::valid_plan_id(id))
        .ok_or_else(|| Invalid::new("plan_id is not a cleanup plan ID"))?;
    if !p.confirm_move {
        return Err(Invalid::new("confirm_move must be true"));
    }
    let plan = ctx
        .state_dir
        .join("cleanup")
        .join("plans")
        .join(format!("{id}.json"));
    let confirm: &[&str] = if p.confirm_delete {
        &["--confirm-delete", "--json"]
    } else {
        &["--json"]
    };
    Ok(Argv::new(&["cleanup", "apply", "--detach=none", "--yes"])
        .flag("requested-by", ctx.requested_by)
        .words(confirm)
        .state_dir(ctx)
        .operands([plan]))
}

/// Plans rootfs moves to a shared-folder root or a folder in one; the
/// CLI keeps the resulting plan in its private root-owned plan directory.
fn rootfs_plan(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        to: PathParam,
        #[serde(default)]
        mode: Option<crate::rootfs::plan::CleanupMode>,
    }
    let p: Params = parse(params)?;
    let mut argv = Argv::new(&["rootfs", "plan"]).path_flag("to", on_a_volume(&p.to, ctx)?);
    if let Some(mode) = p.mode {
        argv = argv.flag("mode", mode.as_str());
    }
    Ok(argv.words(&["--json"]))
}

/// C4: queue an apply of the private rootfs plan the user confirmed.
fn rootfs_apply(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        plan_id: String,
        confirm_no_restore: bool,
    }
    let p: Params = parse(params)?;
    if !crate::rootfs::plan::valid_plan_id(&p.plan_id) {
        return Err(Invalid::new("plan_id is not a rootfs plan ID"));
    }
    if !p.confirm_no_restore {
        return Err(Invalid::new("confirm_no_restore must be true"));
    }
    let plan = Path::new(crate::rootfs::plan::PLAN_DIR).join(format!("{}.json", p.plan_id));
    Ok(
        Argv::new(&["rootfs", "apply", "--detach=none", "--yes", "--json"])
            .flag("requested-by", ctx.requested_by)
            .state_dir(ctx)
            .operands([plan]),
    )
}

/// C4: list recorded rootfs applies; this read never takes a path.
fn rootfs_history(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    parse::<NoParams>(params)?;
    Ok(Argv::new(&["rootfs", "history", "--json"]).state_dir(ctx))
}

/// The UI's 快速 and 溫和.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Fast,
    Gentle,
}

impl Mode {
    fn profile(&self) -> &'static str {
        match self {
            Mode::Fast => "fast",
            Mode::Gentle => "low",
        }
    }
}

/// A stopped job, checked and handed to the package's launcher like
/// `scan.start`, optionally in another mode.
fn job_resume(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        job_id: String,
        mode: Option<Mode>,
        #[serde(default)]
        fresh: bool,
    }
    let p: Params = parse(params)?;
    let mut argv = Argv::new(&["resume", "--detach=none"]);
    if p.fresh {
        argv = argv.words(&["--fresh"]);
    } else if let Some(mode) = &p.mode {
        argv = argv.flag("profile", mode.profile());
    }
    Ok(argv
        .words(&["--json"])
        .state_dir(ctx)
        .operands([job_id(&p.job_id)?]))
}

fn job_mode(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        job_id: String,
        mode: Mode,
    }
    let p: Params = parse(params)?;
    Ok(Argv::new(&["mode"])
        .flag("profile", p.mode.profile())
        .words(&["--json"])
        .state_dir(ctx)
        .operands([job_id(&p.job_id)?]))
}

fn job_result(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: JobParams = parse(params)?;
    Ok(Argv::new(&["jobs"])
        .flag("result", job_id(&p.job_id)?)
        .words(&["--json"])
        .state_dir(ctx))
}

/// `command` on `{job_id}`.
fn on_job(command: &str, params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: JobParams = parse(params)?;
    Ok(Argv::new(&[command, "--json"])
        .state_dir(ctx)
        .operands([job_id(&p.job_id)?]))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobParams {
    job_id: String,
}

/// `id` when it is a plain job ID: 1 to `MAX_JOB_ID_BYTES` ASCII
/// letters, digits and `-`, not starting with `-`.
fn job_id(id: &str) -> Result<&str, Invalid> {
    let plain = id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !plain || id.is_empty() || id.len() > MAX_JOB_ID_BYTES || id.starts_with('-') {
        return Err(Invalid::new("job_id is not a job ID"));
    }
    Ok(id)
}

/// A chosen report's scan job ID, checked like any job ID.
fn report_job(id: &str) -> Result<&str, Invalid> {
    job_id(id).map_err(|_| Invalid::new("report is not a scan job ID"))
}

fn why_not_freed(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        volume: PathParam,
        #[serde(default)]
        processes: bool,
    }
    let p: Params = parse(params)?;
    let argv = Argv::new(&["why-not-freed"]);
    let argv = if p.processes {
        argv.words(&["--processes"])
    } else {
        argv
    };
    Ok(argv
        .words(&["--json"])
        .state_dir(ctx)
        .operands([volume(&p.volume, ctx)?]))
}

fn top(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        target: PathParam,
        kind: Kind,
        by: Option<By>,
        limit: Option<u32>,
        report: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Kind {
        All,
        Dirs,
        Files,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum By {
        Reclaimable,
        Logical,
        Physical,
        Shared,
        Held,
    }
    let p: Params = parse(params)?;
    let kind = match p.kind {
        Kind::All => "all",
        Kind::Dirs => "dirs",
        Kind::Files => "files",
    };
    let mut argv = Argv::new(&["top"]).flag("kind", kind);
    if let Some(by) = p.by {
        argv = argv.flag(
            "by",
            match by {
                By::Reclaimable => "reclaimable",
                By::Logical => "logical",
                By::Physical => "physical",
                By::Shared => "shared",
                By::Held => "held",
            },
        );
    }
    if let Some(limit) = p.limit {
        argv = argv.flag("limit", &bounded("limit", limit, 200)?.to_string());
    }
    if let Some(report) = &p.report {
        argv = argv.flag("report", report_job(report)?);
    }
    Ok(argv
        .words(&["--json", "--no-scan"])
        .state_dir(ctx)
        .operands([on_a_volume(&p.target, ctx)?]))
}

fn snapshots(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        target: PathParam,
        simulate: Option<String>,
    }
    let p: Params = parse(params)?;
    let mut argv = Argv::new(&["snapshots"]);
    if let Some(selection) = p.simulate {
        selection
            .parse::<crate::snapshots::answer::Selection>()
            .map_err(|e| Invalid::new(format!("simulate: {e}")))?;
        argv = argv.flag("simulate-delete", &selection);
    }
    Ok(argv
        .words(&["--json"])
        .state_dir(ctx)
        .operands([on_a_volume(&p.target, ctx)?]))
}

fn recycle(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        target: PathParam,
        oldest: Option<u32>,
        report: Option<String>,
    }
    let p: Params = parse(params)?;
    let mut argv = Argv::new(&["recycle"]);
    if let Some(oldest) = p.oldest {
        argv = argv.flag("oldest", &bounded("oldest", oldest, 1000)?.to_string());
    }
    if let Some(report) = &p.report {
        argv = argv.flag("report", report_job(report)?);
    }
    Ok(argv
        .words(&["--json", "--no-scan"])
        .state_dir(ctx)
        .operands([on_a_volume(&p.target, ctx)?]))
}

/// `value` of param `name` when it is 1 to `max`.
fn bounded(name: &str, value: u32, max: u32) -> Result<u32, Invalid> {
    if (1..=max).contains(&value) {
        Ok(value)
    } else {
        Err(Invalid::new(format!("{name} must be 1 to {max}")))
    }
}

/// `params` as the method's param struct: exactly one JSON object.
fn parse<T: DeserializeOwned>(params: &[u8]) -> Result<T, Invalid> {
    let value: serde_json::Value =
        serde_json::from_slice(params).map_err(|e| Invalid::new(format!("params: {e}")))?;
    if !value.is_object() {
        return Err(Invalid::new("params must be one JSON object"));
    }
    serde_json::from_value(value).map_err(|e| Invalid::new(format!("params: {e}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootfsReportParams {
    #[serde(default)]
    max_age_s: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeParams {
    volume: PathParam,
}

/// A duplicate group id: 32 hex digits, with an `f` before them for a folder
/// group. A file group's digits may start with `f` themselves, so the prefix
/// is told by the length, not by the first character.
fn group_id(id: &str) -> Result<&str, Invalid> {
    let hex = if id.len() == 33 {
        id.strip_prefix('f').unwrap_or(id)
    } else {
        id
    };
    if hex.len() == 32
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(id)
    } else {
        Err(Invalid::new("group_id must be a duplicate group id"))
    }
}

/// `param` as a folder on `volume`: the volume itself or below it.
fn on_the_volume(param: &PathParam, volume: &Path) -> Result<PathBuf, Invalid> {
    let path = absolute(param)?;
    if !path.starts_with(volume) {
        return Err(Invalid::new("the path must be on the volume"));
    }
    Ok(path)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicatesScopeParams {
    volume: PathParam,
    path: Option<PathParam>,
    list: Option<String>,
    category: Option<String>,
    offset: Option<u32>,
    limit: Option<u32>,
}

fn duplicates_scope(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: DuplicatesScopeParams = parse(params)?;
    let volume = volume(&p.volume, ctx)?;
    let mut argv = Argv::new(&["duplicates", "scope", "--json"]);
    if let Some(path) = &p.path {
        argv = argv.path_flag("path", on_the_volume(path, &volume)?);
    }
    if let Some(list) = &p.list {
        if !matches!(list.as_str(), "files" | "folders" | "both") {
            return Err(Invalid::new("list must be files, folders or both"));
        }
        argv = argv.flag("list", list);
    }
    if let Some(category) = &p.category {
        if !matches!(category.as_str(), "photos" | "videos" | "music" | "other") {
            return Err(Invalid::new(
                "category must be photos, videos, music or other",
            ));
        }
        argv = argv.flag("category", category);
    }
    argv = paging(argv, p.offset, p.limit)?;
    Ok(argv.state_dir(ctx).operands([volume]))
}

fn paging(mut argv: Argv, offset: Option<u32>, limit: Option<u32>) -> Result<Argv, Invalid> {
    if let Some(offset) = offset {
        argv = argv.flag("offset", &offset.to_string());
    }
    if let Some(limit) = limit {
        if !(1..=crate::duplicates::command::MAX_PAGE).contains(&limit) {
            return Err(Invalid::new("limit must be 1 to 200"));
        }
        argv = argv.flag("limit", &limit.to_string());
    }
    Ok(argv)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicatesCopiesParams {
    volume: PathParam,
    group_id: String,
    scope: Option<PathParam>,
    offset: Option<u32>,
    limit: Option<u32>,
}

fn duplicates_copies(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: DuplicatesCopiesParams = parse(params)?;
    let volume = volume(&p.volume, ctx)?;
    let mut argv =
        Argv::new(&["duplicates", "copies", "--json"]).flag("group", group_id(&p.group_id)?);
    if let Some(scope) = &p.scope {
        argv = argv.path_flag("path", on_the_volume(scope, &volume)?);
    }
    argv = paging(argv, p.offset, p.limit)?;
    Ok(argv.state_dir(ctx).operands([volume]))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicatesKeepParams {
    volume: PathParam,
    group_id: String,
    path: PathParam,
    keep: bool,
    scope: Option<PathParam>,
}

fn duplicates_keep(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: DuplicatesKeepParams = parse(params)?;
    let volume = volume(&p.volume, ctx)?;
    let mut argv = Argv::new(&["duplicates", "keep", "--json"])
        .flag("group", group_id(&p.group_id)?)
        .path_flag("copy", on_the_volume(&p.path, &volume)?)
        .flag("keep", if p.keep { "on" } else { "off" });
    if let Some(scope) = &p.scope {
        argv = argv.path_flag("path", on_the_volume(scope, &volume)?);
    }
    Ok(argv.state_dir(ctx).operands([volume]))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicatesPreferredParams {
    volume: PathParam,
    shares: Option<Vec<String>>,
}

fn duplicates_preferred(params: &[u8], ctx: &Context) -> Result<Argv, Invalid> {
    let p: DuplicatesPreferredParams = parse(params)?;
    let volume = volume(&p.volume, ctx)?;
    let mut argv = Argv::new(&["duplicates", "preferred-shares", "--json"]);
    if let Some(shares) = &p.shares {
        let text = serde_json::to_string(shares).map_err(|e| Invalid::new(e.to_string()))?;
        crate::duplicates::command::parse_shares(&text).map_err(Invalid::new)?;
        argv = argv.flag("shares", &text);
    }
    Ok(argv.state_dir(ctx).operands([volume]))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathsParams {
    paths: Vec<PathParam>,
}

/// A path as text, or as the CLI prints it: `{path, path_hex?}`, where
/// `path_hex` holds the bytes of a non-UTF-8 path.
#[derive(Deserialize)]
#[serde(untagged)]
enum PathParam {
    Text(String),
    Printed(PrintedPath),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrintedPath {
    path: String,
    path_hex: Option<String>,
}

impl PathParam {
    fn bytes(&self) -> Result<Vec<u8>, Invalid> {
        match self {
            PathParam::Text(path)
            | PathParam::Printed(PrintedPath {
                path,
                path_hex: None,
            }) => Ok(path.as_bytes().to_vec()),
            PathParam::Printed(PrintedPath {
                path_hex: Some(hex),
                ..
            }) => crate::report::hex_bytes::decode(hex)
                .ok_or_else(|| Invalid::new("path_hex is not hex")),
        }
    }
}

/// `param` as an absolute path of at most `MAX_PATH_BYTES` whose every
/// segment is a name: no NUL, no empty, `.` or `..` segment.
fn absolute(param: &PathParam) -> Result<PathBuf, Invalid> {
    let bytes = param.bytes()?;
    let named = |segment: &[u8]| !matches!(segment, b"" | b"." | b"..") && !segment.contains(&0);
    match bytes.strip_prefix(b"/") {
        Some(rest) if bytes.len() <= MAX_PATH_BYTES && rest.split(|b| *b == b'/').all(named) => {
            Ok(PathBuf::from(OsString::from_vec(bytes)))
        }
        _ => Err(Invalid::new(format!(
            "{} is not an absolute path",
            crate::output::escape_human(&bytes)
        ))),
    }
}

/// `param` as the mount point of a local Btrfs volume.
fn volume(param: &PathParam, ctx: &Context) -> Result<PathBuf, Invalid> {
    let path = absolute(param)?;
    if !ctx.volumes.contains(&path) {
        return Err(Invalid::new(format!(
            "{} is not a volume",
            crate::output::escape_human(path.as_os_str().as_bytes())
        )));
    }
    Ok(path)
}

/// `param` as a path on a local Btrfs volume: its mount or below it.
fn on_a_volume(param: &PathParam, ctx: &Context) -> Result<PathBuf, Invalid> {
    let path = absolute(param)?;
    if !ctx.volumes.iter().any(|v| path.starts_with(v)) {
        return Err(Invalid::new(format!(
            "{} is not on a volume",
            crate::output::escape_human(path.as_os_str().as_bytes())
        )));
    }
    Ok(path)
}

/// The argv of the calls prewarmed after a `webapi` scan of the volume
/// at `mount`: `top.list` of its first level and
/// `overview.get`.
pub fn prewarmed(state_dir: &Path, mount: &Path) -> Vec<Vec<OsString>> {
    let ctx = Context {
        state_dir,
        volumes: &[mount.to_path_buf()],
        requested_by: "",
    };
    let at = crate::output::JsonPath::new(mount.as_os_str().as_bytes());
    [
        (
            "top.list",
            serde_json::json!({"target": at, "kind": "all", "limit": 200}),
        ),
        ("overview.get", serde_json::json!({"volume": at})),
    ]
    .into_iter()
    .filter_map(|(method, params)| argv(method, params.to_string().as_bytes(), &ctx).ok())
    .collect()
}

/// The argv of a duplicates answer prewarmed after an analysis of the
/// volume at `mount`, and rebuilt into the cache when missing: the
/// volume's, as the UI asks for it first, or a shared folder's page
/// (`share`, directly on the volume) with both lists.
pub fn duplicates_prewarmed(
    state_dir: &Path,
    mount: &Path,
    share: Option<&Path>,
) -> Option<Vec<OsString>> {
    let ctx = Context {
        state_dir,
        volumes: &[mount.to_path_buf()],
        requested_by: "",
    };
    let at = crate::output::JsonPath::new(mount.as_os_str().as_bytes());
    let params = match share {
        None => serde_json::json!({"volume": at, "limit": 1}),
        Some(share) if share.parent() == Some(mount) => serde_json::json!({
            "volume": at,
            "path": crate::output::JsonPath::new(share.as_os_str().as_bytes()),
            "list": "both",
            "limit": 200,
        }),
        Some(_) => return None,
    };
    argv("duplicates.scope", params.to_string().as_bytes(), &ctx).ok()
}

/// The volume of `argv` when it is one of the duplicates answers kept
/// in the cache ([`duplicates_prewarmed`]).
pub fn duplicates_cached_volume(state_dir: &Path, argv: &[OsString]) -> Option<PathBuf> {
    if argv.get(1..3)? != ["duplicates", "scope"] {
        return None;
    }
    let dashes = argv.iter().position(|a| a == "--")?;
    let [volume] = &argv[dashes + 1..] else {
        return None;
    };
    let volume = PathBuf::from(volume);
    let share = argv[..dashes].iter().find_map(|a| {
        a.as_bytes()
            .strip_prefix(b"--path=")
            .map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)))
    });
    let wanted = duplicates_prewarmed(state_dir, &volume, share.as_deref())?;
    (wanted == argv).then_some(volume)
}

/// An argv under construction; user values only ever become operands
/// or `--flag=value` words, never flags of their own.
struct Argv(Vec<OsString>);

impl Argv {
    fn new(words: &[&str]) -> Self {
        Argv(vec!["synoreclaim".into()]).words(words)
    }

    fn words(mut self, words: &[&str]) -> Self {
        self.0.extend(words.iter().map(OsString::from));
        self
    }

    /// `--name=value`: a value can never become a flag of its own.
    fn flag(mut self, name: &str, value: &str) -> Self {
        self.0.push(format!("--{name}={value}").into());
        self
    }

    /// `--name=path`, retaining non-UTF-8 path bytes as one option.
    fn path_flag(mut self, name: &str, value: PathBuf) -> Self {
        let mut arg = OsString::from(format!("--{name}="));
        arg.push(value);
        self.0.push(arg);
        self
    }

    /// The operands after `--`, last.
    fn operands<T: Into<OsString>>(mut self, operands: impl IntoIterator<Item = T>) -> Self {
        self.0.push("--".into());
        self.0.extend(operands.into_iter().map(Into::into));
        self
    }

    fn state_dir(mut self, ctx: &Context) -> Self {
        self.0.push("--state-dir".into());
        self.0.push(ctx.state_dir.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    const STATE: &str = "/volume1/@appdata/SynoReclaim/state";

    /// A file group id is 32 hex digits and may start with `f`; a folder
    /// group id is `f` and 32 more. Neither is cut short by the other's prefix.
    #[test]
    fn group_ids_of_files_starting_with_f_are_accepted() {
        let file = "f9ee35a915338eefdfd50a417d91c1d2";
        let folder = "f9ee35a915338eefdfd50a417d91c1d2e";
        assert_eq!(group_id(file).ok(), Some(file));
        assert_eq!(group_id(folder).ok(), Some(folder));
        let plain = "0123456789abcdef0123456789abcdef";
        assert_eq!(group_id(plain).ok(), Some(plain));
        for bad in [
            "",
            "f",
            "f9ee35a915338eefdfd50a417d91c1d",
            "x9ee35a915338eefdfd50a417d91c1d2e",
            "F9ee35a915338eefdfd50a417d91c1d2",
            "f9ee35a915338eefdfd50a417d91c1d2ef",
        ] {
            assert!(group_id(bad).is_err(), "{bad} is not a group id");
        }
    }

    /// The volume's duplicates answer and each share's page with both
    /// lists are the cached ones; a deeper folder, a category or another
    /// page is computed live.
    #[test]
    fn duplicates_answers_cached_are_the_volume_and_share_pages() {
        let state = Path::new(STATE);
        let volume = Path::new("/volume1");
        let cached = |method: &str, params: &str| {
            let ctx = Context {
                state_dir: state,
                volumes: &[volume.to_path_buf()],
                requested_by: "admin",
            };
            let argv = argv(method, params.as_bytes(), &ctx).unwrap();
            duplicates_cached_volume(state, &argv)
        };
        let root = cached("duplicates.scope", r#"{"volume":"/volume1","limit":1}"#);
        assert_eq!(root.as_deref(), Some(volume));
        let share = r#"{"volume":"/volume1","path":"/volume1/photo","list":"both","limit":200}"#;
        assert_eq!(cached("duplicates.scope", share).as_deref(), Some(volume));
        for live in [
            r#"{"volume":"/volume1","path":"/volume1/photo/2024","list":"both","limit":200}"#,
            r#"{"volume":"/volume1","path":"/volume1/photo","list":"files","category":"photos","limit":200}"#,
            r#"{"volume":"/volume1","path":"/volume1/photo","list":"both","offset":200,"limit":200}"#,
            r#"{"volume":"/volume1","limit":200}"#,
        ] {
            assert_eq!(cached("duplicates.scope", live), None, "{live}");
        }
        assert_eq!(
            cached(
                "top.list",
                r#"{"target":"/volume1","kind":"all","limit":200}"#
            ),
            None
        );
        assert!(duplicates_prewarmed(state, volume, Some(Path::new("/volume1/a/b"))).is_none());
    }

    fn volumes() -> Vec<PathBuf> {
        vec![PathBuf::from("/volume1"), PathBuf::from("/volume2")]
    }

    fn call(method: &str, params: &str) -> Result<Vec<String>, Invalid> {
        let volumes = volumes();
        let ctx = Context {
            state_dir: Path::new(STATE),
            volumes: &volumes,
            requested_by: "admin",
        };
        argv(method, params.as_bytes(), &ctx)
            .map(|argv| argv.into_iter().map(|a| a.into_string().unwrap()).collect())
    }

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(String::from).collect()
    }

    #[test]
    fn planning_api_queues_instead_of_running_synchronously() {
        let volumes = vec![PathBuf::from("/volume1")];
        let ctx = Context {
            state_dir: Path::new("STATE"),
            volumes: &volumes,
            requested_by: "admin",
        };
        for (method, params) in [
            ("cleanup.plan", br#"{"paths":["/volume1/data"]}"#.as_slice()),
            ("rootfs.plan", br#"{"to":"/volume1/backup"}"#.as_slice()),
            ("rootfs.report", b"{}".as_slice()),
        ] {
            let call = plan(method, params, &ctx).unwrap();
            assert!(
                !matches!(call, Call::Run(_)),
                "planning remained synchronous: {method}"
            );
        }
        assert!(plan("cleanup.plan", br#"{"paths":[],"root":true}"#, &ctx).is_err());
    }

    #[test]
    fn a_staged_chunk_answers_at_once_as_cleanup_select_and_the_plan_takes_its_id() {
        let volumes = vec![PathBuf::from("/volume1")];
        let ctx = Context {
            state_dir: Path::new("STATE"),
            volumes: &volumes,
            requested_by: "admin",
        };
        let chunk = br#"{"stage":{"selection":"selection-18e-4f","paths":[{"path":"/volume1/s/a","expect":{"filesystem_uuid":"00112233-4455-6677-8899-aabbccddeeff","tree":256,"objectid":7}}],"groups":["0123456789abcdef0123456789abcdef"]}}"#;
        let Call::Run(argv) = plan("cleanup.plan", chunk, &ctx).unwrap() else {
            panic!("a chunk is staged at once, not queued");
        };
        let words: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(words[1..4], ["cleanup", "select", "--json"]);
        assert!(words.contains(&"--selection=selection-18e-4f".to_string()));
        let staged = words
            .iter()
            .find_map(|w| w.strip_prefix("--chunk="))
            .unwrap();
        let staged: serde_json::Value = serde_json::from_str(staged).unwrap();
        assert_eq!(staged["paths"][0]["path"], "2f766f6c756d65312f732f61");
        assert_eq!(staged["paths"][0]["objectid"], 7);
        for bad in [
            r#"{"stage":{"paths":["/tmp/x"]}}"#,
            r#"{"stage":{"selection":"../plans","paths":["/volume1/a"]}}"#,
            r#"{"stage":{}}"#,
            r#"{"stage":{"groups":["nothex"]}}"#,
        ] {
            assert!(plan("cleanup.plan", bad.as_bytes(), &ctx).is_err(), "{bad}");
        }
        assert!(matches!(
            plan("cleanup.plan", br#"{"selection":"selection-18e-4f"}"#, &ctx).unwrap(),
            Call::Planning { .. }
        ));
    }

    #[test]
    fn cleanup_plan_carries_expected_report_identity_without_shell_parsing() {
        let argv = call("cleanup.plan", r#"{"paths":[{"path":"/volume1/old name","expect":{"filesystem_uuid":"00112233-4455-6677-8899-aabbccddeeff","tree":256,"objectid":300,"generation":9}},"/volume1/legacy"]}"#).unwrap();
        let value = argv
            .iter()
            .find_map(|a| a.strip_prefix("--expect="))
            .unwrap();
        let expected: serde_json::Value = serde_json::from_str(value).unwrap();
        assert_eq!(expected[0]["expect"]["objectid"], 300);
        assert_eq!(expected[0]["path"], "2f766f6c756d65312f6f6c64206e616d65");
        assert_eq!(expected.as_array().unwrap().len(), 1);
        assert_eq!(argv.last().unwrap(), "/volume1/legacy");
        for expect in [
            r#"{"tree":256}"#,
            r#"{"tree":256,"objectid":300}"#,
            r#"{"tree":-1,"objectid":3}"#,
            r#"{"tree":256,"objectid":3,"bypass":true}"#,
        ] {
            assert!(
                call(
                    "cleanup.plan",
                    &format!(r#"{{"paths":[{{"path":"/volume1/a","expect":{expect}}}]}}"#)
                )
                .is_err()
            );
        }
        assert!(
            call(
                "simulate.get",
                r#"{"paths":[{"path":"/volume1/a","expect":{"tree":256,"objectid":3}}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn volumes_list_runs_volumes_with_the_package_state_dir() {
        assert_eq!(
            call("volumes.list", "{}").unwrap(),
            words(&format!("synoreclaim volumes --json --state-dir {STATE}"))
        );
    }

    #[test]
    fn params_are_exactly_one_object_with_only_known_fields() {
        for params in ["", "[]", "null", "{} {}", "{", r#"{"x":1}"#] {
            assert!(call("volumes.list", params).is_err(), "{params:?}");
        }
    }

    #[test]
    fn doctor_run_takes_a_volume_mount() {
        assert_eq!(
            call("doctor.run", r#"{"volume":"/volume2"}"#).unwrap(),
            words(&format!(
                "synoreclaim doctor --json --state-dir {STATE} -- /volume2"
            ))
        );
        for volume in [
            r#""/volume1/share""#,
            r#""/volume3""#,
            r#""volume1""#,
            r#""/volume1/""#,
            r#""/volume1/..""#,
            "1",
            "null",
        ] {
            let params = format!(r#"{{"volume":{volume}}}"#);
            assert!(call("doctor.run", &params).is_err(), "{params}");
        }
        assert!(call("doctor.run", "{}").is_err());
    }

    fn answer(method: &str, params: &str, expected: &str) {
        assert_eq!(
            call(method, params).unwrap(),
            words(&expected.replace("STATE", STATE)),
            "{method} {params}"
        );
    }

    #[test]
    fn listings_take_no_params() {
        answer(
            "jobs.list",
            "{}",
            "synoreclaim jobs --json --state-dir STATE",
        );
        answer(
            "history.list",
            "{}",
            "synoreclaim history --json --state-dir STATE",
        );
    }

    #[test]
    fn rootfs_plan_accepts_only_two_fixed_cleanup_modes() {
        answer(
            "rootfs.plan",
            r#"{"to":"/volume1/backup","mode":"deep"}"#,
            "synoreclaim rootfs plan --to=/volume1/backup --mode=deep --json",
        );
        answer(
            "rootfs.plan",
            r#"{"to":"/volume1/backup","mode":"standard"}"#,
            "synoreclaim rootfs plan --to=/volume1/backup --mode=standard --json",
        );
        for mode in ["all", "deep;reboot", "--help", "", "DEEP"] {
            let params = serde_json::json!({"to":"/volume1/backup", "mode":mode}).to_string();
            assert!(call("rootfs.plan", &params).is_err());
        }
    }

    #[test]
    fn rootfs_plan_maps_a_share_root_or_folder_to_the_destination_flag() {
        answer(
            "rootfs.plan",
            r#"{"to":"/volume1/backup"}"#,
            "synoreclaim rootfs plan --to=/volume1/backup --json",
        );
        answer(
            "rootfs.plan",
            r#"{"to":"/volume2/media/archive"}"#,
            "synoreclaim rootfs plan --to=/volume2/media/archive --json",
        );

        for params in [
            "{}",
            r#"{"to":"/volume3/share"}"#,
            r#"{"to":"/volume1/share/../other"}"#,
            r#"{"to":"/volume1/share/"}"#,
            r#"{"to":"/volume1/share","plan_file":"/tmp/plan.json"}"#,
        ] {
            assert!(call("rootfs.plan", params).is_err(), "{params}");
        }
    }

    #[test]
    fn duplicate_answers_stay_on_their_volume_with_checked_ids_and_pages() {
        let keep = call(
            "duplicates.keep",
            r#"{"volume":"/volume1","group_id":"0123456789abcdef0123456789abcdef","path":"/volume1/a/x","keep":false,"scope":"/volume1/a"}"#,
        )
        .unwrap();
        assert!(keep.contains(&"--keep=off".to_string()));
        assert!(keep.contains(&"--copy=/volume1/a/x".to_string()));
        assert_eq!(keep[keep.len() - 2..], ["--", "/volume1"]);
        // Reclaim is administrators-only: no answer depends on the caller.
        assert!(!keep.iter().any(|a| a.starts_with("--requested-by")));
        let plan = call(
            "cleanup.plan",
            r#"{"duplicates":{"volume":"/volume1","groups":["f0123456789abcdef0123456789abcdef"]}}"#,
        )
        .unwrap();
        let flag = plan
            .iter()
            .find_map(|a| a.strip_prefix("--duplicates="))
            .unwrap();
        let spec: serde_json::Value = serde_json::from_str(flag).unwrap();
        assert_eq!(spec["volume"]["path"], "/volume1");
        // `cleanup plan` refuses any other key, a null scope too.
        assert_eq!(
            spec.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["groups", "volume"]
        );
        // What the bridge sends, `cleanup plan` reads as a selection: no
        // analysis here, so it stops later, never on the selection itself.
        let missing = Path::new("/nonexistent/synoreclaim-state");
        let read = |spec: &str| {
            crate::duplicates::command::expand(spec, missing)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        };
        assert!(!read(flag).contains("duplicates_invalid"), "{}", read(flag));
        assert!(
            read(r#"{"volume":"/volume1","scope":null,"groups":[]}"#)
                .contains("duplicates_invalid")
        );
        assert!(spec.get("user").is_none());
        assert_eq!(plan.last().unwrap(), "--");
        for (method, params) in [
            ("duplicates.scope", r#"{"volume":"/volume1/photo"}"#),
            (
                "duplicates.scope",
                r#"{"volume":"/volume1","path":"/volume2/x"}"#,
            ),
            ("duplicates.scope", r#"{"volume":"/volume1","limit":201}"#),
            ("duplicates.scope", r#"{"volume":"/volume1","list":"all"}"#),
            (
                "duplicates.scope",
                r#"{"volume":"/volume1","category":"docs"}"#,
            ),
            (
                "duplicates.copies",
                r#"{"volume":"/volume1","group_id":"--path=/etc"}"#,
            ),
            (
                "duplicates.keep",
                r#"{"volume":"/volume1","group_id":"0123456789abcdef0123456789abcdef","path":"/volume2/x","keep":true}"#,
            ),
            (
                "duplicates.preferred_shares",
                r#"{"volume":"/volume1","shares":["a/b"]}"#,
            ),
            ("duplicates.analyze", r#"{"volume":"/volume1","x":1}"#),
            ("cleanup.plan", r#"{"paths":[]}"#),
            (
                "cleanup.plan",
                r#"{"duplicates":{"volume":"/volume1","groups":[]}}"#,
            ),
            (
                "cleanup.plan",
                r#"{"duplicates":{"volume":"/volume1","scope":"/volume2","groups":["0123456789abcdef0123456789abcdef"]}}"#,
            ),
        ] {
            assert!(call(method, params).is_err(), "{method} {params}");
        }
    }

    #[test]
    fn rootfs_report_accepts_max_age_and_uses_the_package_state_dir() {
        answer(
            "rootfs.report",
            "{}",
            "synoreclaim rootfs --json --state-dir STATE",
        );
        answer(
            "rootfs.report",
            r#"{"max_age_s":30}"#,
            "synoreclaim rootfs --json --max-age-s=30 --state-dir STATE",
        );
        answer(
            "rootfs.report",
            r#"{"max_age_s":0}"#,
            "synoreclaim rootfs --json --max-age-s=0 --state-dir STATE",
        );
        answer(
            "rootfs.report",
            r#"{"max_age_s":2592000}"#,
            "synoreclaim rootfs --json --max-age-s=2592000 --state-dir STATE",
        );
        assert!(call("rootfs.report", r#"{"max_age_s":2592001}"#).is_err());
        assert!(call("rootfs.report", r#"{"max_age_s":-1}"#).is_err());
        assert!(call("rootfs.report", r#"{"unknown":1}"#).is_err());
    }

    #[test]
    fn volume_methods_run_their_command_on_the_volume() {
        let v = r#"{"volume":"/volume1"}"#;
        answer(
            "overview.get",
            v,
            "synoreclaim overview --json --no-scan --state-dir STATE -- /volume1",
        );
        answer(
            "why_not_freed.get",
            v,
            "synoreclaim why-not-freed --json --state-dir STATE -- /volume1",
        );
        answer(
            "why_not_freed.get",
            r#"{"volume":"/volume1","processes":true}"#,
            "synoreclaim why-not-freed --processes --json --state-dir STATE -- /volume1",
        );
        answer(
            "watch_free.sample",
            v,
            "synoreclaim watch-free --duration=2s --interval=2s --json -- /volume1",
        );
        answer(
            "cache.status",
            v,
            "synoreclaim cache status --json --state-dir STATE -- /volume1",
        );
        answer(
            "cache.clear",
            v,
            "synoreclaim cache clear --json --state-dir STATE -- /volume1",
        );
        answer(
            "support_bundle.get",
            v,
            "synoreclaim support-bundle --output=- --state-dir STATE -- /volume1",
        );
        for method in [
            "overview.get",
            "why_not_freed.get",
            "watch_free.sample",
            "cache.status",
            "cache.clear",
            "support_bundle.get",
        ] {
            assert!(
                call(method, r#"{"volume":"/volume1/s"}"#).is_err(),
                "{method}"
            );
            assert!(call(method, "{}").is_err(), "{method}");
        }
        assert!(
            call(
                "why_not_freed.get",
                r#"{"volume":"/volume1","processes":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_volume_scan_prewarms_the_first_top_level_and_the_overview() {
        let as_text = |argv: Vec<OsString>| -> Vec<String> {
            argv.into_iter().map(|a| a.into_string().unwrap()).collect()
        };
        let prewarmed: Vec<Vec<String>> = prewarmed(Path::new(STATE), Path::new("/volume1"))
            .into_iter()
            .map(as_text)
            .collect();
        assert_eq!(
            prewarmed,
            vec![
                call(
                    "top.list",
                    r#"{"target":"/volume1","kind":"all","limit":200}"#
                )
                .unwrap(),
                call("overview.get", r#"{"volume":"/volume1"}"#).unwrap(),
            ]
        );
    }

    #[test]
    fn top_list_maps_kind_order_and_limit() {
        answer(
            "top.list",
            r#"{"target":"/volume1/share","kind":"all"}"#,
            "synoreclaim top --kind=all --json --no-scan --state-dir STATE -- /volume1/share",
        );
        answer(
            "top.list",
            r#"{"target":"/volume1","kind":"files","by":"physical","limit":200}"#,
            "synoreclaim top --kind=files --by=physical --limit=200 --json --no-scan --state-dir STATE -- /volume1",
        );
        for bad in [
            r#"{"target":"/volume1","kind":"all","limit":201}"#,
            r#"{"target":"/volume1","kind":"all","limit":0}"#,
            r#"{"target":"/volume1","kind":"all","limit":-1}"#,
            r#"{"target":"/volume1","kind":"everything"}"#,
            r#"{"target":"/volume1","kind":"all","by":"size"}"#,
            r#"{"target":"/volume1"}"#,
            r#"{"target":"/volume3/x","kind":"all"}"#,
            r#"{"target":"/volume10","kind":"all"}"#,
            r#"{"target":"/","kind":"all"}"#,
        ] {
            assert!(call("top.list", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn path_lists_are_bounded_and_each_path_lies_on_a_volume() {
        answer(
            "inspect.get",
            r#"{"paths":["/volume1/a","/volume2/b"]}"#,
            "synoreclaim inspect --explain --json --no-scan --state-dir STATE -- /volume1/a /volume2/b",
        );
        answer(
            "simulate.get",
            r#"{"paths":["/volume1/a"]}"#,
            "synoreclaim simulate-delete --json --no-scan --state-dir STATE -- /volume1/a",
        );
        answer(
            "cleanup.plan",
            r#"{"paths":["/volume1/a"]}"#,
            "synoreclaim cleanup plan --json --quiet --state-dir STATE -- /volume1/a",
        );
        let paths = |n: usize| {
            let list: Vec<String> = (0..n).map(|i| format!(r#""/volume1/{i}""#)).collect();
            format!(r#"{{"paths":[{}]}}"#, list.join(","))
        };
        // A cleanup selection has no bound of its own: its request is the
        // bound, and cleanup.select stages a larger one.
        assert!(call("cleanup.plan", &paths(5000)).is_ok());
        assert!(call("cleanup.plan", &paths(0)).is_err());
        for (method, limit) in [("inspect.get", 100), ("simulate.get", 1000)] {
            assert!(call(method, &paths(limit)).is_ok(), "{method}");
            assert!(call(method, &paths(limit + 1)).is_err(), "{method}");
            assert!(call(method, &paths(0)).is_err(), "{method}");
            assert!(
                call(method, r#"{"paths":["/volume1/a","/tmp"]}"#).is_err(),
                "{method}"
            );
            assert!(
                call(method, r#"{"paths":"/volume1/a"}"#).is_err(),
                "{method}"
            );
        }
    }

    #[test]
    fn paths_must_be_absolute_named_and_short() {
        let top = |target: &str| {
            call(
                "top.list",
                &format!(r#"{{"target":{target},"kind":"all"}}"#),
            )
        };
        let long = format!(r#""/volume1/{}""#, "x".repeat(4096 - 9));
        assert!(top(&long).is_ok());
        let longer = format!(r#""/volume1/{}""#, "x".repeat(4096 - 8));
        assert!(top(&longer).is_err());
        for bad in [
            r#""/volume1/a/../../etc""#,
            r#""/volume1/./a""#,
            r#""/volume1//a""#,
            r#""/volume1/a/""#,
            r#""/volume1/a\u0000b""#,
            r#""volume1/a""#,
            r#""""#,
            r#"{"path":"/volume1/a","path_hex":"zz"}"#,
            r#"{"path":"/volume1/a","extra":1}"#,
        ] {
            assert!(top(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_printed_path_passes_its_exact_bytes() {
        let hex = crate::report::hex_bytes::encode(b"/volume1/caf\xe9");
        let volumes = volumes();
        let ctx = Context {
            state_dir: Path::new(STATE),
            volumes: &volumes,
            requested_by: "admin",
        };
        let params = format!(r#"{{"paths":[{{"path":"/volume1/caf\ufffd","path_hex":"{hex}"}}]}}"#);
        let hexed = argv("inspect.get", params.as_bytes(), &ctx).unwrap();
        assert_eq!(hexed.last().unwrap().as_bytes(), b"/volume1/caf\xe9");
        let plain = r#"{"paths":[{"path":"/volume1/a"}]}"#;
        assert_eq!(
            argv("inspect.get", plain.as_bytes(), &ctx)
                .unwrap()
                .last()
                .unwrap(),
            "/volume1/a"
        );
    }

    #[test]
    fn snapshots_list_passes_a_selection_the_cli_accepts() {
        answer(
            "snapshots.list",
            r#"{"target":"/volume1/share"}"#,
            "synoreclaim snapshots --json --state-dir STATE -- /volume1/share",
        );
        answer(
            "snapshots.list",
            r#"{"target":"/volume1/share","simulate":"id:300,older-than:30d"}"#,
            "synoreclaim snapshots --simulate-delete=id:300,older-than:30d --json --state-dir STATE -- /volume1/share",
        );
        for bad in [r#""id:x""#, r#""""#, r#""--json""#, "1"] {
            let params = format!(r#"{{"target":"/volume1/share","simulate":{bad}}}"#);
            assert!(call("snapshots.list", &params).is_err(), "{params}");
        }
    }

    #[test]
    fn recycle_list_bounds_the_oldest_items() {
        answer(
            "recycle.list",
            r#"{"target":"/volume1"}"#,
            "synoreclaim recycle --json --no-scan --state-dir STATE -- /volume1",
        );
        answer(
            "recycle.list",
            r#"{"target":"/volume1/share","oldest":1000}"#,
            "synoreclaim recycle --oldest=1000 --json --no-scan --state-dir STATE -- /volume1/share",
        );
        assert!(call("recycle.list", r#"{"target":"/volume1","oldest":1001}"#).is_err());
        assert!(call("recycle.list", r#"{"target":"/volume1","oldest":0}"#).is_err());
    }

    /// A page can answer from a chosen report instead of the newest ones.
    #[test]
    fn top_and_recycle_lists_take_a_chosen_report_by_job_id() {
        answer(
            "top.list",
            r#"{"target":"/volume1","kind":"all","report":"20261105T153157Z-3f07"}"#,
            "synoreclaim top --kind=all --report=20261105T153157Z-3f07 --json --no-scan --state-dir STATE -- /volume1",
        );
        answer(
            "recycle.list",
            r#"{"target":"/volume1/share","oldest":200,"report":"20261105T153157Z-3f07"}"#,
            "synoreclaim recycle --oldest=200 --report=20261105T153157Z-3f07 --json --no-scan --state-dir STATE -- /volume1/share",
        );
        for id in ["", "-x", "../report", "a b", "--refresh"] {
            assert!(
                call(
                    "top.list",
                    &format!(r#"{{"target":"/volume1","kind":"all","report":"{id}"}}"#)
                )
                .is_err(),
                "{id}"
            );
            assert!(
                call(
                    "recycle.list",
                    &format!(r#"{{"target":"/volume1","report":"{id}"}}"#)
                )
                .is_err(),
                "{id}"
            );
        }
    }

    #[test]
    fn deep_attribution_is_gentle_and_report_ids_cannot_escape_state() {
        answer(
            "scan.start",
            r#"{"targets":["/volume1"],"mode":"gentle","attribution_report":"saved-scan"}"#,
            "synoreclaim scan --detach=none --profile=low --attribution-report=saved-scan --requested-by=admin --json --quiet --state-dir STATE -- /volume1",
        );
        for id in ["../report", "", "/tmp/report"] {
            assert!(
                call(
                    "scan.start",
                    &format!(
                        r#"{{"targets":["/volume1"],"mode":"gentle","attribution_report":"{id}"}}"#
                    )
                )
                .is_err()
            );
        }
        assert!(call("scan.start", r#"{"targets":["/volume1"],"mode":"gentle","attribution_report":"saved-scan","snapshots":"all"}"#).is_err());
        assert!(call("scan.start", r#"{"targets":["/volume1"],"mode":"gentle","attribution_report":"saved-scan","snapshot_report":"saved-scan"}"#).is_err());
    }

    #[test]
    fn scan_start_accepts_all_snapshots_and_a_saved_report() {
        answer(
            "scan.start",
            r#"{"targets":["/volume1"],"mode":"gentle","snapshots":"all","snapshot_report":"saved-scan"}"#,
            "synoreclaim scan --detach=none --profile=low --snapshots=all --snapshot-report=saved-scan --requested-by=admin --json --quiet --state-dir STATE -- /volume1",
        );
        for id in ["../report", "", "/tmp/report"] {
            assert!(call("scan.start", &format!(r#"{{"targets":["/volume1"],"mode":"gentle","snapshots":"all","snapshot_report":"{id}"}}"#)).is_err());
        }
    }

    #[test]
    fn scan_start_queues_a_scan_for_the_package_launcher() {
        answer(
            "scan.start",
            r#"{"targets":["/volume1"],"mode":"gentle"}"#,
            "synoreclaim scan --detach=none --profile=low --requested-by=admin --json --quiet --state-dir STATE -- /volume1",
        );
        answer(
            "scan.start",
            r#"{"targets":["/volume1/a","/volume1/b"],"mode":"fast","snapshots":true}"#,
            "synoreclaim scan --detach=none --profile=fast --snapshots --requested-by=admin --json --quiet --state-dir STATE -- /volume1/a /volume1/b",
        );
        let targets = |n: usize| {
            let list: Vec<String> = (0..n).map(|i| format!(r#""/volume1/{i}""#)).collect();
            format!(r#"{{"targets":[{}],"mode":"gentle"}}"#, list.join(","))
        };
        assert!(call("scan.start", &targets(MAX_SCAN_TARGETS)).is_ok());
        for bad in [
            targets(MAX_SCAN_TARGETS + 1),
            targets(0),
            r#"{"targets":["/volume1"]}"#.into(),
            r#"{"targets":["/volume1"],"mode":"normal"}"#.into(),
            r#"{"targets":["/volume1"],"mode":"fast","snapshots":"yes"}"#.into(),
            r#"{"targets":["/srv"],"mode":"fast"}"#.into(),
            r#"{"targets":["/volume1"],"mode":"fast","profile":"fast"}"#.into(),
        ] {
            assert!(call("scan.start", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn job_requests_name_one_plain_job_id() {
        answer(
            "job.pause",
            r#"{"job_id":"20260929T113051Z-2c1c46"}"#,
            "synoreclaim pause --json --state-dir STATE -- 20260929T113051Z-2c1c46",
        );
        answer(
            "job.cancel",
            r#"{"job_id":"j1"}"#,
            "synoreclaim cancel --json --state-dir STATE -- j1",
        );
        let long = format!(r#""{}""#, "a".repeat(65));
        for bad in [
            r#""""#,
            r#""../j1""#,
            r#""-j1""#,
            r#""j 1""#,
            r#""j/1""#,
            "1",
            &long,
        ] {
            let params = format!(r#"{{"job_id":{bad}}}"#);
            assert!(call("job.pause", &params).is_err(), "{params}");
            assert!(call("job.cancel", &params).is_err(), "{params}");
        }
    }

    #[test]
    fn job_mode_switches_a_running_scan_in_place() {
        answer(
            "job.mode",
            r#"{"job_id":"j1","mode":"fast"}"#,
            "synoreclaim mode --profile=fast --json --state-dir STATE -- j1",
        );
        answer(
            "job.mode",
            r#"{"job_id":"j1","mode":"gentle"}"#,
            "synoreclaim mode --profile=low --json --state-dir STATE -- j1",
        );
        for bad in [
            r#"{"job_id":"j1"}"#,
            r#"{"job_id":"j1","mode":"turbo"}"#,
            r#"{"job_id":"../j1","mode":"fast"}"#,
            r#"{"job_id":"j1","mode":"fast","x":1}"#,
        ] {
            assert!(call("job.mode", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn job_resume_detaches_for_the_launcher_and_job_result_prints_the_run() {
        answer(
            "job.resume",
            r#"{"job_id":"j1"}"#,
            "synoreclaim resume --detach=none --json --state-dir STATE -- j1",
        );
        answer(
            "job.resume",
            r#"{"job_id":"j1","mode":"fast"}"#,
            "synoreclaim resume --detach=none --profile=fast --json --state-dir STATE -- j1",
        );
        answer(
            "job.resume",
            r#"{"job_id":"j1","fresh":true,"mode":"fast"}"#,
            "synoreclaim resume --detach=none --fresh --json --state-dir STATE -- j1",
        );
        answer(
            "job.result",
            r#"{"job_id":"j1"}"#,
            "synoreclaim jobs --result=j1 --json --state-dir STATE",
        );
        for bad in [
            r#"{"job_id":"j1","mode":"low"}"#,
            r#"{"job_id":"-j1"}"#,
            r#"{"mode":"fast"}"#,
        ] {
            assert!(call("job.resume", bad).is_err(), "{bad}");
        }
        assert!(call("job.result", r#"{"job_id":"j1","mode":"fast"}"#).is_err());
    }

    #[test]
    fn setup_cleanup_sets_only_the_kill_switch() {
        answer(
            "setup.cleanup",
            r#"{"mutations_disabled":true}"#,
            "synoreclaim config set --mutations-disabled=on --json",
        );
        answer(
            "setup.cleanup",
            r#"{"mutations_disabled":false}"#,
            "synoreclaim config set --mutations-disabled=off --json",
        );
        for bad in [
            "{}",
            r#"{"mutations_disabled":"on"}"#,
            r#"{"mutations_disabled":true,"allowed_roots":["/volume1"]}"#,
        ] {
            assert!(call("setup.cleanup", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cleanup_apply_detaches_the_confirmed_plan_for_the_launcher() {
        answer(
            "cleanup.apply",
            r#"{"plan_id":"cleanup-18a2b-1f","confirm_move":true}"#,
            "synoreclaim cleanup apply --detach=none --yes --requested-by=admin --json --state-dir STATE -- STATE/cleanup/plans/cleanup-18a2b-1f.json",
        );
        // Removing items in no shared folder for good is its own confirmation.
        answer(
            "cleanup.apply",
            r#"{"plan_id":"cleanup-18a2b-1f","confirm_move":true,"confirm_delete":true}"#,
            "synoreclaim cleanup apply --detach=none --yes --requested-by=admin --confirm-delete --json --state-dir STATE -- STATE/cleanup/plans/cleanup-18a2b-1f.json",
        );
        let long = format!("cleanup-{}", "a".repeat(MAX_JOB_ID_BYTES));
        for bad in [
            r#"{"plan_id":"cleanup-a","confirm_move":false}"#.to_string(),
            r#"{"plan_id":"cleanup-a"}"#.into(),
            r#"{"plan_id":"cleanup-a","confirm_move":"yes"}"#.into(),
            r#"{"plan_id":"cleanup-a","confirm_move":true,"yes":true}"#.into(),
            r#"{"plan_id":"j1","confirm_move":true}"#.into(),
            r#"{"plan_id":"cleanup-","confirm_move":true}"#.into(),
            r#"{"plan_id":"cleanup-../a","confirm_move":true}"#.into(),
            r#"{"plan_id":"cleanup-a/b","confirm_move":true}"#.into(),
            format!(r#"{{"plan_id":"{long}","confirm_move":true}}"#),
            r#"{"confirm_move":true}"#.into(),
            r#"{"plan_id":"cleanup-a","confirm_move":true,"confirm_delete":"yes"}"#.into(),
        ] {
            assert!(call("cleanup.apply", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rootfs_apply_requires_confirmation_and_history_is_sync() {
        answer(
            "rootfs.apply",
            r#"{"plan_id":"rootfs-18d75b8423f30000-4d","confirm_no_restore":true}"#,
            "synoreclaim rootfs apply --detach=none --yes --json --requested-by=admin --state-dir STATE -- /var/lib/synoreclaim/rootfs-plans/rootfs-18d75b8423f30000-4d.json",
        );
        answer(
            "rootfs.history",
            "{}",
            "synoreclaim rootfs history --json --state-dir STATE",
        );
        for bad in [
            "{}",
            r#"{"plan_id":"rootfs-1-2"}"#,
            r#"{"plan_id":"rootfs-1-2","confirm_no_restore":false}"#,
            r#"{"plan_id":"rootfs-1-2","confirm_no_restore":"yes"}"#,
            r#"{"plan_id":"cleanup-a"}"#,
            r#"{"plan_id":"rootfs-1-2","path":"/var/lib/plan.json"}"#,
        ] {
            assert!(call("rootfs.apply", bad).is_err(), "{bad}");
        }
        assert!(call("rootfs.history", r#"{"path":"/volume1"}"#).is_err());
    }

    #[test]
    fn every_mapped_argv_parses_as_a_cli_command() {
        let calls = [
            ("volumes.list", "{}"),
            ("jobs.list", "{}"),
            ("history.list", "{}"),
            ("doctor.run", r#"{"volume":"/volume1"}"#),
            ("overview.get", r#"{"volume":"/volume1"}"#),
            ("cache.status", r#"{"volume":"/volume1"}"#),
            ("cache.clear", r#"{"volume":"/volume1"}"#),
            ("support_bundle.get", r#"{"volume":"/volume1"}"#),
            (
                "why_not_freed.get",
                r#"{"volume":"/volume1","processes":true}"#,
            ),
            ("watch_free.sample", r#"{"volume":"/volume1"}"#),
            (
                "top.list",
                r#"{"target":"/volume1","kind":"dirs","by":"held","limit":5}"#,
            ),
            ("inspect.get", r#"{"paths":["/volume1/a"]}"#),
            ("simulate.get", r#"{"paths":["/volume1/a","/volume1/b"]}"#),
            ("cleanup.plan", r#"{"paths":["/volume1/a"]}"#),
            (
                "cleanup.apply",
                r#"{"plan_id":"cleanup-a","confirm_move":true}"#,
            ),
            ("rootfs.plan", r#"{"to":"/volume1/backup"}"#),
            (
                "rootfs.apply",
                r#"{"plan_id":"rootfs-18d75b8423f30000-4d","confirm_no_restore":true}"#,
            ),
            ("rootfs.history", "{}"),
            ("setup.cleanup", r#"{"mutations_disabled":false}"#),
            (
                "snapshots.list",
                r#"{"target":"/volume1/s","simulate":"all"}"#,
            ),
            ("recycle.list", r#"{"target":"/volume1/s","oldest":3}"#),
            (
                "scan.start",
                r#"{"targets":["/volume1"],"mode":"fast","snapshots":true}"#,
            ),
            ("job.pause", r#"{"job_id":"j1"}"#),
            ("job.cancel", r#"{"job_id":"j1"}"#),
            ("job.resume", r#"{"job_id":"j1","mode":"gentle"}"#),
            ("job.result", r#"{"job_id":"j1"}"#),
            ("duplicates.analyze", r#"{"volume":"/volume1"}"#),
            (
                "cleanup.plan",
                r#"{"duplicates":{"volume":"/volume1","groups":["0123456789abcdef0123456789abcdef"]}}"#,
            ),
            (
                "duplicates.scope",
                r#"{"volume":"/volume1","path":"/volume1/photo","list":"folders","category":"videos","offset":50,"limit":50}"#,
            ),
            (
                "duplicates.copies",
                r#"{"volume":"/volume1","group_id":"0123456789abcdef0123456789abcdef","scope":"/volume1/a"}"#,
            ),
            (
                "duplicates.keep",
                r#"{"volume":"/volume1","group_id":"f0123456789abcdef0123456789abcdef","path":"/volume1/a/x","keep":true}"#,
            ),
            (
                "duplicates.preferred_shares",
                r#"{"volume":"/volume1","shares":["photo","video"]}"#,
            ),
            ("duplicates.preferred_shares", r#"{"volume":"/volume1"}"#),
        ];
        for (method, params) in calls {
            let argv = call(method, params).unwrap();
            if let Err(e) = crate::cli::Cli::try_parse_from(&argv) {
                panic!("{method}: {argv:?}: {e}");
            }
        }
    }

    #[test]
    fn params_are_read_up_to_the_size_limit() {
        assert_eq!(read_params(&mut &b"{}"[..]).unwrap(), b"{}");
        let most = vec![b' '; MAX_PARAMS_BYTES];
        assert!(read_params(&mut most.as_slice()).is_ok());
        let more = vec![b' '; MAX_PARAMS_BYTES + 1];
        assert!(read_params(&mut more.as_slice()).is_err());
    }

    #[test]
    fn the_requester_is_a_short_printable_name() {
        for name in [
            "admin",
            "DOMAIN\\alice",
            "bob@ldap.example",
            "jane doe",
            "李",
        ] {
            assert_eq!(requester(name).unwrap(), name);
        }
        let long = "x".repeat(MAX_REQUESTER_BYTES + 1);
        for name in ["", "a\nb", "a\u{7f}", "a\tb", long.as_str()] {
            assert!(requester(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn session_get_is_answered_here_from_no_params() {
        let volumes = volumes();
        let ctx = Context {
            state_dir: Path::new(STATE),
            volumes: &volumes,
            requested_by: "admin",
        };
        assert_eq!(plan("session.get", b"{}", &ctx).unwrap(), Call::Session);
        assert!(plan("session.get", br#"{"user":"x"}"#, &ctx).is_err());
        assert!(matches!(plan("jobs.list", b"{}", &ctx), Ok(Call::Run(_))));
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_640_000);
        let switches = crate::guard::CleanupSwitches {
            cleanup_enabled: true,
            mutations_disabled: false,
        };
        let doc = SessionDocument::new("admin", switches, now);
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/json/session.json");
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut rendered = serde_json::to_value(&doc).unwrap();
        rendered["tool_version"] = golden["tool_version"].clone();
        assert_eq!(rendered, golden);
    }

    #[test]
    fn a_rejection_is_an_invalid_param_document() {
        let invalid = Invalid::new("unknown method \"x\"");
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_640_000);
        let doc = ErrorDocument::new("x", &invalid, now);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/json/webapi-invalid-param.json");
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut rendered = serde_json::to_value(&doc).unwrap();
        rendered["tool_version"] = golden["tool_version"].clone();
        assert_eq!(rendered, golden);
    }

    fn package_var(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("synoreclaim-webapi-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_call_runs_with_its_state_dir() {
        let state = package_var("state").join("state");
        let Call::Run(argv) = prepare(
            "jobs.list",
            "admin",
            &mut &b"{}"[..],
            &state,
            &volumes(),
            &|_| None,
            0,
        )
        .unwrap() else {
            panic!("not run");
        };
        assert_eq!(argv[argv.len() - 1], state.into_os_string());
        std::fs::remove_dir(package_var("state")).unwrap();
    }

    #[test]
    fn a_prewarmed_answer_is_served_from_the_ui_cache() {
        let var = package_var("cached");
        let state = std::fs::canonicalize(&var).unwrap().join("state");
        let uid = crate::sys::effective_uid();
        crate::state::private_dir(&state, uid).unwrap();
        let overview = prewarmed(&state, Path::new("/volume1")).remove(1);
        let entry = crate::ui_cache::Entry {
            format: crate::ui_cache::FORMAT,
            key: crate::ui_cache::key(&overview),
            fsid: "fs1".into(),
            mount: b"/volume1".to_vec(),
            reports: crate::ui_cache::reports_fingerprint(&state, "fs1").unwrap(),
            valid_until: 100,
            exit_code: 3,
            document: "{}\n".into(),
        };
        crate::ui_cache::save(&state, uid, &entry).unwrap();
        let fs1 = |mount: &[u8]| (mount == b"/volume1").then(|| "fs1".to_string());
        let ask = |params: &[u8], now| {
            prepare(
                "overview.get",
                "admin",
                &mut &params[..],
                &state,
                &volumes(),
                &fs1,
                now,
            )
            .unwrap()
        };
        assert_eq!(
            ask(br#"{"volume":"/volume1"}"#, 99),
            Call::Cached(3, "{}\n".into())
        );
        assert_eq!(ask(br#"{"volume":"/volume1"}"#, 100), Call::Run(overview));
        assert!(matches!(ask(br#"{"volume":"/volume2"}"#, 99), Call::Run(_)));
        std::fs::remove_dir_all(&var).unwrap();
    }

    #[test]
    fn a_saved_rootfs_report_answers_at_once_within_its_max_age() {
        let var = package_var("rootfs-cached");
        let state = std::fs::canonicalize(&var).unwrap().join("state");
        let uid = crate::sys::effective_uid();
        crate::state::private_dir(&state, uid).unwrap();
        let document = "{\"schema_version\":1,\"command\":\"rootfs\",\"completion\":\"partial\"}\n";
        crate::rootfs::report_cache::save(&state, uid, 1000, 3, document).unwrap();
        let ask = |params: &[u8], now| {
            prepare(
                "rootfs.report",
                "admin",
                &mut &params[..],
                &state,
                &volumes(),
                &|_| None,
                now,
            )
            .unwrap()
        };

        assert_eq!(
            ask(br#"{"max_age_s":600}"#, 1600),
            Call::Cached(3, document.into())
        );
        assert!(matches!(
            ask(br#"{"max_age_s":600}"#, 1601),
            Call::Planning { .. }
        ));
        assert!(matches!(
            ask(br#"{"max_age_s":0}"#, 1000),
            Call::Planning { .. }
        ));
        assert!(matches!(ask(b"{}", 1000), Call::Planning { .. }));
        std::fs::remove_dir_all(&var).unwrap();
    }

    #[test]
    fn a_call_is_refused_before_it_runs() {
        let var = package_var("refused");
        let refused = |method: &str, user: &str, params: &[u8]| match prepare(
            method,
            user,
            &mut &params[..],
            &var,
            &volumes(),
            &|_| None,
            0,
        ) {
            Err(Refusal::Invalid(_)) => {}
            other => panic!("{method} {user:?}: {other:?}"),
        };
        refused("jobs.list", "", b"{}");
        refused("jobs.remove", "admin", b"{}");
        refused("jobs.list", "admin", b"{\"all\":true}");
        refused("jobs.list", "admin", &vec![b' '; MAX_PARAMS_BYTES + 1]);
        std::fs::remove_dir(&var).unwrap();
    }

    /// Every method the package's App forwards to this binary, as `area.verb`.
    fn package_methods() -> Vec<String> {
        let source = include_str!("../app/src/lib.rs");
        let list = source
            .split_once("pub const CLI_METHODS: &[&str] = &[")
            .unwrap()
            .1
            .split_once("];")
            .unwrap()
            .0;
        let mut methods = list
            .lines()
            .filter_map(|line| {
                let method = line.trim().strip_prefix('"')?;
                Some(method.split_once('"')?.0.to_string())
            })
            .collect::<Vec<_>>();
        methods.sort();
        methods
    }

    /// Method names accepted by the dispatcher in `argv`.
    fn argv_methods() -> Vec<String> {
        let source = include_str!("webapi.rs");
        let dispatcher = source
            .split_once("pub fn argv(")
            .unwrap()
            .1
            .split_once("_ => return Err(Invalid::new(format!(\"unknown method")
            .unwrap()
            .0;
        let mut methods = dispatcher
            .lines()
            .filter_map(|line| {
                let method = line.trim().strip_prefix('"')?;
                Some(method.split_once('"')?.0.to_string())
            })
            .collect::<Vec<_>>();
        methods.sort();
        methods
    }

    #[test]
    fn the_package_registers_exactly_the_ui_methods() {
        let mut spec = vec![
            "session.get",
            "setup.cleanup",
            "volumes.list",
            "doctor.run",
            "scan.start",
            "jobs.list",
            "job.pause",
            "job.resume",
            "job.cancel",
            "job.mode",
            "job.result",
            "history.list",
            "overview.get",
            "top.list",
            "inspect.get",
            "simulate.get",
            "snapshots.list",
            "why_not_freed.get",
            "recycle.list",
            "watch_free.sample",
            "cleanup.plan",
            "cleanup.apply",
            "rootfs.plan",
            "rootfs.apply",
            "rootfs.history",
            "cache.status",
            "cache.clear",
            "support_bundle.get",
            "rootfs.report",
            "duplicates.analyze",
            "duplicates.scope",
            "duplicates.copies",
            "duplicates.keep",
            "duplicates.preferred_shares",
        ];
        spec.sort();
        assert_eq!(package_methods(), spec);
    }

    #[test]
    fn argv_methods_match_package_registrations() {
        let accepted = argv_methods();
        // session.get is answered by `plan` and has no CLI argv.
        let mut registered = package_methods();
        registered.retain(|method| method != "session.get");
        assert_eq!(accepted, registered);
    }

    #[test]
    fn every_registered_method_is_mapped() {
        let volumes = volumes();
        let ctx = Context {
            state_dir: Path::new(STATE),
            volumes: &volumes,
            requested_by: "admin",
        };
        for method in package_methods() {
            let unknown = matches!(
                plan(&method, b"{}", &ctx),
                Err(invalid) if invalid.message.starts_with("unknown method")
            );
            assert!(!unknown, "{method}");
        }
    }

    #[test]
    fn an_unknown_method_is_invalid() {
        assert!(call("volumes.delete", "{}").is_err());
        assert!(call("", "{}").is_err());
    }
}
