//! Runs one root program start: a WebAPI call for `root.run`, a queued job
//! for `root.start`, or a lifecycle step.

use super::launch::{self, Untrusted};
use super::{Invocation, JobRequest, QUEUEING_METHODS, RunRequest};
use crate::jobs::{self, Job, Request};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A `root.run` or `root.start` request is at most 1 MiB.
const MAX_REQUEST: u64 = 1 << 20;
/// How often a job reports progress.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);
/// The setuid helper and grants of the builds before the SDK executor.
const OLD_PRIVILEGE_DIR: &str = "/var/packages/SynoReclaim/var/privilege";

/// Serves one start the executor made; the process exit code.
pub fn start(invocation: Invocation) -> i32 {
    crate::sys::set_umask(0o077);
    let var = |name: &str| std::env::var_os(name);
    let served = match invocation {
        Invocation::Run => run(&var),
        Invocation::Job => job(&var),
        Invocation::Stop { .. } => super::root_state(var).map_err(|e| e.0).map(|state| {
            settled(super::lifecycle::stop(
                &state,
                Path::new(OLD_PRIVILEGE_DIR),
                crate::sys::effective_uid(),
                now(),
            ))
        }),
        Invocation::Purge { delete_state } => Ok(settled(super::lifecycle::purge(
            delete_state,
            Path::new(crate::rootfs::plan::PLAN_DIR),
        ))),
    };
    served.unwrap_or_else(|message| {
        eprintln!("synoreclaim: error: {message}");
        1
    })
}

fn run(var: &impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<i32, String> {
    let request: RunRequest = super::request(&stdin()?).map_err(|e| e.0)?;
    let caller = super::caller(var).map_err(|e| e.0)?;
    let call = || {
        crate::webapi::serve(
            &request.method,
            &caller.requested_by,
            &mut request.params.as_bytes(),
            &caller.state_dir,
        )
    };
    if !QUEUEING_METHODS.contains(&request.method.as_str()) {
        return Ok(call());
    }
    let (code, out) = capture(call).map_err(|e| format!("stdout: {e}"))?;
    keep_launch(&out, &caller.state_dir)
        .map_err(|Untrusted| "the queued job's launch is not one to keep".to_string())?;
    std::io::stdout()
        .write_all(&out)
        .map_err(|e| format!("stdout: {e}"))?;
    Ok(code)
}

fn job(var: &impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<i32, String> {
    use clap::Parser;
    let request: JobRequest = super::request(&stdin()?).map_err(|e| e.0)?;
    let caller = super::caller(var).map_err(|e| e.0)?;
    let launch = launch::take(&caller.state_dir, &request.job_id)
        .map_err(|Untrusted| format!("job {} has no launch", request.job_id))?;
    let apply = launch.args.first().is_some_and(|command| command != "scan");
    let planning = launch
        .args
        .first()
        .is_some_and(|command| command == "planning");
    let cli = if planning {
        None
    } else {
        Some(
            crate::cli::Cli::try_parse_from(
                std::iter::once("synoreclaim".to_string()).chain(launch.args),
            )
            .map_err(|e| e.to_string())?,
        )
    };
    let jobs = caller.state_dir.join("jobs");
    let done = Arc::new(AtomicBool::new(false));
    if let Some(cancel) = inherited(var, super::CONTROL_FD_ENV) {
        let (jobs, id) = (jobs.clone(), request.job_id.clone());
        std::thread::spawn(move || follow_cancels(cancel, &jobs, &id, apply));
    }
    let reporter = inherited(var, super::PROGRESS_FD_ENV).map(|out| {
        let (jobs, id, done) = (jobs.clone(), request.job_id.clone(), done.clone());
        std::thread::spawn(move || report_progress(out, &jobs, &id, &done))
    });
    let code = if planning {
        crate::planning::run(&caller.state_dir, &request.job_id).unwrap_or_else(|error| {
            eprintln!("synoreclaim: planning job failed: {error}");
            1
        })
    } else {
        crate::app::run(cli.expect("non-planning launch parsed"))
    };
    done.store(true, Ordering::Release);
    if let Some(reporter) = reporter {
        let _ = reporter.join();
    }
    Ok(code)
}

/// Turns each cancel the executor sends into a request of the job; the
/// executor's end (EOF) is a cancel of its own.
fn follow_cancels(cancel: std::fs::File, jobs: &Path, id: &str, apply: bool) {
    let uid = crate::sys::effective_uid();
    for line in std::io::BufReader::new(cancel).lines() {
        let Ok(line) = line else { break };
        if let Some(request) = control(&line, apply) {
            let _ = jobs::request(jobs, uid, id, request, now());
        }
    }
    let request = control(r#"{"cancel":"executor-stop"}"#, apply).unwrap_or(Request::Cancel);
    let _ = jobs::request(jobs, uid, id, request, now());
}

/// Writes the job's progress until `done`, then once more.
fn report_progress(mut out: std::fs::File, jobs: &Path, id: &str, done: &AtomicBool) {
    loop {
        let finished = done.load(Ordering::Acquire);
        if let Ok(job) = jobs::load(jobs, id) {
            let line = progress(&job).to_string() + "\n";
            if out.write_all(line.as_bytes()).is_err() {
                return;
            }
        }
        if finished {
            return;
        }
        let mut waited = Duration::ZERO;
        while waited < PROGRESS_EVERY && !done.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(100));
            waited += Duration::from_millis(100);
        }
    }
}

fn inherited(
    var: &impl Fn(&str) -> Option<std::ffi::OsString>,
    name: &str,
) -> Option<std::fs::File> {
    let fd = var(name)?.to_str()?.parse().ok()?;
    crate::sys::inherited(fd).ok()
}

fn stdin() -> Result<Vec<u8>, String> {
    let mut request = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST + 1)
        .read_to_end(&mut request)
        .map_err(|e| format!("stdin: {e}"))?;
    if request.len() as u64 > MAX_REQUEST {
        return Err("the request is over 1 MiB".into());
    }
    Ok(request)
}

/// Exit 0 when every step settled; the rest go to the executor log.
fn settled(unsettled: Vec<String>) -> i32 {
    for message in &unsettled {
        eprintln!("synoreclaim: error: {message}");
    }
    i32::from(!unsettled.is_empty())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// After a queueing method answered `output`, keeps the launch its detach
/// document names so `root.start` can run that job and nothing else.
pub fn keep_launch(output: &[u8], state: &Path) -> Result<(), Untrusted> {
    let Ok(document) = serde_json::from_slice::<Value>(output) else {
        return Ok(());
    };
    match launch::from_document(&document, state)? {
        Some(launch) => launch::save(state, &launch),
        None => Ok(()),
    }
}

/// The job request a cancel line on fd 4 makes; `None` for anything else.
pub fn control(line: &str, apply: bool) -> Option<Request> {
    let line: Value = serde_json::from_str(line).ok()?;
    let reason = line.get("cancel")?.as_str()?;
    Some(if super::cancels(reason, apply) {
        Request::Cancel
    } else {
        Request::Pause
    })
}

/// A progress line for fd 5 from the job's record.
pub fn progress(job: &Job) -> Value {
    json!({
        "phase": job.phase,
        "message": job.state.name(),
        "result": {"job_id": job.job_id, "kind": job.kind.name(), "state": job.state.name()},
    })
}

/// Runs `call` with standard output going to a pipe and answers its exit
/// code with what it wrote.
pub fn capture(call: impl FnOnce() -> i32) -> std::io::Result<(i32, Vec<u8>)> {
    use std::os::fd::AsFd;
    let saved = std::io::stdout().as_fd().try_clone_to_owned()?;
    let (mut reader, writer) = crate::sys::pipe()?;
    let drain = std::thread::spawn(move || {
        let mut out = Vec::new();
        Read::by_ref(&mut reader)
            .take(MAX_OUTPUT)
            .read_to_end(&mut out)?;
        // Past the limit the writer must not block on a full pipe.
        std::io::copy(&mut reader, &mut std::io::sink())?;
        Ok::<_, std::io::Error>(out)
    });
    crate::sys::replace_stdout(writer.as_fd())?;
    drop(writer);
    let code = call();
    let flushed = std::io::stdout().flush();
    let restored = crate::sys::replace_stdout(saved.as_fd());
    let out = drain
        .join()
        .map_err(|_| std::io::Error::other("stdout reader panicked"))??;
    flushed?;
    restored?;
    Ok((code, out))
}

/// The policy's `run.max_output`.
const MAX_OUTPUT: u64 = 64 << 20;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ScanLimits;

    fn state(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("synoreclaim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn detach(state: &Path) -> Value {
        json!({
            "command": "detach",
            "launcher": "none",
            "job_id": "j-1",
            "run": ["/x/synoreclaim", "scan", "--job-id", "j-1", "--state-dir", state],
        })
    }

    #[test]
    fn a_queued_job_keeps_its_launch() {
        let state = state("keep");
        let out = serde_json::to_vec(&detach(&state)).unwrap();
        assert_eq!(keep_launch(&out, &state), Ok(()));
        assert_eq!(launch::take(&state, "j-1").unwrap().job_id, "j-1");
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn a_refusal_keeps_nothing_and_a_forged_launch_is_refused() {
        let state = state("refusal");
        assert_eq!(keep_launch(br#"{"command":"error"}"#, &state), Ok(()));
        assert_eq!(keep_launch(b"not json", &state), Ok(()));
        let mut forged = detach(&state);
        forged["run"] = json!([
            "/x/synoreclaim",
            "scan",
            "--job-id",
            "j-1",
            "--state-dir",
            "/tmp"
        ]);
        assert_eq!(
            keep_launch(&serde_json::to_vec(&forged).unwrap(), &state),
            Err(Untrusted)
        );
        assert!(launch::take(&state, "j-1").is_err());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_cancel_line_asks_the_job() {
        assert_eq!(
            control(r#"{"cancel":"user"}"#, false),
            Some(Request::Cancel)
        );
        assert_eq!(control(r#"{"cancel":"stop"}"#, false), Some(Request::Pause));
        assert_eq!(
            control(r#"{"cancel":"upgrade"}"#, true),
            Some(Request::Cancel)
        );
        assert_eq!(control(r#"{"cancel":1}"#, false), None);
        assert_eq!(control("junk", false), None);
    }

    #[test]
    fn progress_names_the_job_and_its_state() {
        let job = Job::new(
            "j-1",
            &[b"/volume1".to_vec()],
            None,
            &ScanLimits::report_only(),
            1,
        );
        assert_eq!(
            progress(&job),
            json!({
                "phase": "discover",
                "message": "running",
                "result": {"job_id": "j-1", "kind": "scan", "state": "running"},
            })
        );
    }

    #[test]
    fn a_call_is_captured_and_stdout_comes_back() {
        use std::io::Write;
        // capture replaces fd 1 for the whole process, including libtest's
        // concurrent progress output. Give it its own process, as root.run has.
        if std::env::var_os("RECLAIM_CAPTURE_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "root_entry::serve::tests::a_call_is_captured_and_stdout_comes_back",
                    "--nocapture",
                ])
                .env("RECLAIM_CAPTURE_CHILD", "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains("stdout before capture\nstdout after capture\n"),
                "stdout was not restored: {output:?}"
            );
            return;
        }
        std::io::stdout()
            .write_all(b"stdout before capture\n")
            .unwrap();
        let (code, out) = capture(|| {
            std::io::stdout()
                .write_all(b"{\"command\":\"detach\"}")
                .unwrap();
            7
        })
        .unwrap();
        assert_eq!((code, out.as_slice()), (7, &br#"{"command":"detach"}"#[..]));
        std::io::stdout()
            .write_all(b"stdout after capture\n")
            .unwrap();
        assert!(crate::sys::inherited(1).is_err());
        assert!(crate::sys::inherited(987).is_err());
    }
}
