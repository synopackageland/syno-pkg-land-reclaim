//! App planning uses the same protected product jobs and SDK root launcher as
//! scans/applies. Only the worker runs the existing synchronous CLI operation.
use crate::cli::{Launcher, ScanLimits};
use crate::document::{Completion, DetachDocument, Envelope};
use crate::jobs::{self, Job, JobKind, JobResult, JobState, Live, Request};
use crate::state::Store;
use crate::time::{job_id, unix_seconds};
use crate::webapi::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

type Failure = Box<dyn Error>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanningRequest {
    pub method: String,
    pub params: String,
}

/// Validates parameters without inventorying the selection or computing a plan.
/// The usual detach result is captured and bound to a one-use root launch.
/// A `rootfs.report` asked for while one walks `/` follows that walk: its
/// job, with outcome `joined` and no launch, so nothing starts twice.
pub fn queue(method: &str, params: &[u8], ctx: &Context<'_>) -> Result<i32, Failure> {
    let (job, joined) = enqueue(method, params, ctx)?;
    let id = job.job_id;
    if joined {
        let mut doc = DetachDocument::new(
            Envelope::new("detach", SystemTime::now()),
            Launcher::External,
        );
        doc.envelope.job_id = Some(id.clone());
        doc.outcome = Some("joined");
        doc.running_job_id = Some(id);
        println!("{}", serde_json::to_string(&doc)?);
        return Ok(0);
    }
    let run = vec![
        std::env::current_exe()?.into_os_string(),
        "planning".into(),
        "--job-id".into(),
        id.clone().into(),
        "--state-dir".into(),
        ctx.state_dir.into(),
    ];
    let mut doc = DetachDocument::new(
        Envelope::new("detach", SystemTime::now()),
        Launcher::External,
    );
    doc.envelope.job_id = Some(id.clone());
    crate::commands::hand_off(
        &mut doc,
        &ctx.state_dir.join("jobs").join(&id),
        &run,
        Launcher::External,
    )??;
    Ok(0)
}

fn enqueue(method: &str, params: &[u8], ctx: &Context<'_>) -> Result<(Job, bool), Failure> {
    crate::webapi::argv(method, params, ctx).map_err(|e| e.message)?;
    let kind = match method {
        "cleanup.plan" => JobKind::CleanupPlan,
        "rootfs.plan" => JobKind::RootfsPlan,
        "rootfs.report" => JobKind::RootfsReport,
        "duplicates.analyze" => JobKind::DuplicatesAnalyze,
        _ => return Err("not a planning operation".into()),
    };
    let now = SystemTime::now();
    let id = job_id(now, std::process::id());
    let store = Store::open(ctx.state_dir, crate::sys::effective_uid())?;
    let mut job = Job::new(
        &id,
        &[],
        None,
        &ScanLimits::report_only(),
        unix_seconds(now),
    );
    job.kind = kind;
    job.phase = match method {
        "rootfs.report" => "report",
        "duplicates.analyze" => "analyze",
        _ => "plan",
    }
    .into();
    job.requested_by = Some(ctx.requested_by.into());
    job.planning = Some(PlanningRequest {
        method: method.into(),
        params: std::str::from_utf8(params)?.into(),
    });
    let jobs = store.root().join("jobs");
    jobs::prune(&jobs, jobs::FINISHED_KEPT)?;
    // Each walk of `/` reads the whole partition, and each analysis a
    // volume's metadata; one at a time is enough.
    if matches!(kind, JobKind::RootfsReport | JobKind::DuplicatesAnalyze) {
        let queued = jobs::queue_or_join(&jobs, crate::sys::effective_uid(), job)?;
        #[cfg(target_os = "linux")]
        if kind == JobKind::DuplicatesAnalyze && !queued.1 {
            crate::duplicates::command::invalidate_answers(
                ctx.state_dir,
                std::str::from_utf8(params)?,
            );
        }
        return Ok(queued);
    }
    Ok((jobs::queue(&jobs, crate::sys::effective_uid(), job)?, false))
}

/// No total yet.
const NONE: u64 = u64::MAX;
static UNITS_DONE: AtomicU64 = AtomicU64::new(0);
static UNITS_TOTAL: AtomicU64 = AtomicU64::new(NONE);
thread_local! {
    /// Set on the thread that runs a planning job's work.
    static REPORTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What a planning job has done of its work so far, such as the items
/// of a cleanup plan checked: its job shows it as progress.
/// Only the thread running a planning job reports: the same checks also run
/// outside a job (a CLI plan, tests in parallel), and must not move its numbers.
pub fn progress(done: u64, total: u64) {
    if !REPORTING.with(std::cell::Cell::get) {
        return;
    }
    UNITS_DONE.store(done, Ordering::Relaxed);
    UNITS_TOTAL.store(total, Ordering::Relaxed);
}

/// The step a planning job's units belong to, when it has several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    None = 0,
    /// The copies a duplicates cleanup keeps, checked again.
    Kept = 1,
    /// The items a cleanup moves.
    Items = 2,
}
static STEP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Starts step `step`: its units count from zero.
pub fn step(step: Step) {
    if !REPORTING.with(std::cell::Cell::get) {
        return;
    }
    STEP.store(step as u8, Ordering::Relaxed);
    progress(0, NONE);
}

fn step_name() -> Option<String> {
    match STEP.load(Ordering::Relaxed) {
        1 => Some("kept".into()),
        2 => Some("items".into()),
        _ => None,
    }
}

/// Runs one queued planning job once. The result remains in the ordinary
/// job result file after this process and the ctl connection have ended.
pub fn run(state: &Path, id: &str) -> Result<i32, Failure> {
    run_with(state, id, |request, user| {
        crate::root_entry::serve::capture(|| execute(request, state, user))
    })
}

fn run_with(
    state: &Path,
    id: &str,
    work: impl FnOnce(&PlanningRequest, &str) -> std::io::Result<(i32, Vec<u8>)>,
) -> Result<i32, Failure> {
    let state_dir = state;
    let jobs = state.join("jobs");
    let saved = jobs::load(&jobs, id)?;
    let request = saved
        .planning
        .clone()
        .ok_or("job has no planning request")?;
    if !matches!(
        (saved.kind, request.method.as_str()),
        (JobKind::CleanupPlan, "cleanup.plan")
            | (JobKind::RootfsPlan, "rootfs.plan")
            | (JobKind::RootfsReport, "rootfs.report")
            | (JobKind::DuplicatesAnalyze, "duplicates.analyze")
    ) {
        return Err("planning job kind does not match its request".into());
    }
    let mut live = Live::launch(
        &jobs,
        crate::sys::effective_uid(),
        id,
        unix_seconds(SystemTime::now()),
    )?;
    let stop = crate::sys::install_stop_handlers()?;
    let done = AtomicBool::new(false);
    let user = live.job.requested_by.clone().unwrap_or_default();
    UNITS_DONE.store(0, Ordering::Relaxed);
    UNITS_TOTAL.store(NONE, Ordering::Relaxed);
    STEP.store(Step::None as u8, Ordering::Relaxed);
    // The collectors already check this stop flag at their existing safe
    // boundaries. SDK cancellation arrives through the ordinary job request.
    let outcome = std::thread::scope(|scope| {
        let live = &mut live;
        let done = &done;
        let stop = &stop;
        scope.spawn(move || {
            let started = std::time::Instant::now();
            let mut shown = (0, NONE, 0);
            let mut saved = started;
            while !done.load(Ordering::Acquire) {
                if live.requested() == Some(Request::Cancel) {
                    stop.store(true, Ordering::Relaxed);
                    return;
                }
                let now = (
                    UNITS_DONE.load(Ordering::Relaxed),
                    UNITS_TOTAL.load(Ordering::Relaxed),
                    STEP.load(Ordering::Relaxed),
                );
                if now != shown && saved.elapsed() >= jobs::PROGRESS_INTERVAL {
                    shown = now;
                    saved = std::time::Instant::now();
                    live.job.progress = Some(crate::progress::JobProgress {
                        phase: live.job.phase.clone(),
                        units_done: now.0,
                        units_total: (now.1 != NONE).then_some(now.1),
                        elapsed_s: started.elapsed().as_secs(),
                        updated: crate::time::rfc3339(SystemTime::now()),
                        step: step_name(),
                        ..Default::default()
                    });
                    if let Err(error) = live.save_progress() {
                        eprintln!("synoreclaim: warning: planning progress not updated: {error}");
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        // Set even when the work panics: the scope waits for the watcher.
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                REPORTING.with(|reporting| reporting.set(false));
                self.0.store(true, Ordering::Release);
            }
        }
        let _done = Done(done);
        REPORTING.with(|reporting| reporting.set(true));
        work(&request, &user)
    })?;
    live.job.phase = jobs::PUBLISH.into();
    live.save()?;
    let cancelled = live.requested() == Some(Request::Cancel);
    let (code, document) = terminal(&request.method, id, outcome, cancelled);
    let completion = document["completion"].as_str().unwrap_or("failed");
    let codes = document["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|error| error["code"].as_str().map(str::to_owned))
        .collect();
    if !cancelled {
        live.record_result(
            JobResult::new(completion, code, &[], codes),
            &serde_json::to_vec(&document)?,
        )?;
    }
    let state = match completion {
        "complete" => JobState::Completed,
        "partial" => JobState::Partial,
        "cancelled" => JobState::Cancelled,
        _ => JobState::Failed,
    };
    live.finish(
        state,
        (state == JobState::Cancelled).then_some("cancelled"),
        unix_seconds(SystemTime::now()),
    )?;
    println!("{document}");
    #[cfg(target_os = "linux")]
    if request.method == "duplicates.analyze"
        && matches!(state, JobState::Completed | JobState::Partial)
    {
        crate::duplicates::command::prewarm_answers(state_dir, &request.params);
    }
    Ok(code)
}

fn execute(request: &PlanningRequest, state: &Path, user: &str) -> i32 {
    use clap::Parser;
    let result = (|| -> Result<_, Failure> {
        let env = crate::resolve::Environment::load()?;
        let volumes = env
            .volumes
            .into_iter()
            .map(|volume| volume.primary.mount_point)
            .collect::<Vec<_>>();
        let ctx = Context {
            state_dir: state,
            volumes: &volumes,
            requested_by: user,
        };
        let argv = crate::webapi::argv(&request.method, request.params.as_bytes(), &ctx)
            .map_err(|e| e.message)?;
        Ok(crate::cli::Cli::try_parse_from(argv)?)
    })();
    match result {
        Ok(cli) => crate::app::run(cli),
        Err(error) => {
            let mut doc = Envelope::new("job-result", SystemTime::now());
            doc.completion = Completion::Failed;
            doc.errors.push(crate::output::TargetError::new(
                crate::output::JsonPath::new(b""),
                "planning_failed",
                error.to_string(),
            ));
            let mut doc = serde_json::to_value(doc).expect("envelope serializes");
            doc["outcome"] = serde_json::json!("no_result");
            doc["state"] = serde_json::json!("failed");
            doc["message"] = serde_json::json!("Planning could not finish");
            println!("{doc}");
            1
        }
    }
}

fn terminal(_method: &str, id: &str, captured: (i32, Vec<u8>), cancelled: bool) -> (i32, Value) {
    let (code, bytes) = captured;
    let mut doc: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if cancelled || !doc.is_object() {
        let mut envelope = Envelope::new("job-result", SystemTime::now());
        envelope.completion = if cancelled {
            Completion::Cancelled
        } else {
            Completion::Failed
        };
        doc = serde_json::to_value(envelope).expect("envelope serializes");
        doc["outcome"] = serde_json::json!(if cancelled { "cancelled" } else { "no_result" });
        doc["state"] = serde_json::json!(if cancelled { "cancelled" } else { "failed" });
        doc["message"] = serde_json::json!("Planning did not publish a result");
    }
    doc["job_id"] = serde_json::json!(id);
    (
        if cancelled {
            4
        } else if code == 0 && doc["completion"] != "complete" {
            1
        } else {
            code
        },
        doc,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn background_results_keep_deep_report_and_plan_documents_intact() {
        for (method, params, fixture) in [
            (
                "rootfs.report",
                "{}",
                include_str!("../ui/fixtures/rootfs.report/deep-short.json"),
            ),
            (
                "rootfs.plan",
                r#"{"to":"/volume1/backup","mode":"deep"}"#,
                include_str!("../ui/fixtures/rootfs.plan/deep-short.json"),
            ),
        ] {
            let state = std::env::temp_dir().join(format!(
                "reclaim-deep-{}-{}",
                method,
                std::process::id()
            ));
            let volumes = vec!["/volume1".into()];
            let ctx = Context {
                state_dir: &state,
                volumes: &volumes,
                requested_by: "admin",
            };
            let job = enqueue(method, params.as_bytes(), &ctx).unwrap().0;
            let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            run_with(&state, &job.job_id, |request, _| {
                assert_eq!(request.params, params);
                Ok((0, fixture.as_bytes().to_vec()))
            })
            .unwrap();
            let mut result: Value = serde_json::from_slice(
                &std::fs::read(state.join("jobs").join(&job.job_id).join(jobs::RESULT)).unwrap(),
            )
            .unwrap();
            let expected: Value = serde_json::from_str(fixture).unwrap();
            result["job_id"] = expected["job_id"].clone();
            assert_eq!(result, expected, "{method} dropped fields");
            std::fs::remove_dir_all(state).unwrap();
        }
    }

    /// The tests that run a planning job: its progress is one per process.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_cleanup_plan_of_a_staged_selection_shows_its_items_checked_as_it_runs() {
        let state =
            std::env::temp_dir().join(format!("reclaim-plan-progress-{}", std::process::id()));
        let ctx = Context {
            state_dir: &state,
            volumes: &[],
            requested_by: "admin",
        };
        // However large the selection, the request names only its ID.
        let job = enqueue("cleanup.plan", br#"{"selection":"selection-18e-4f"}"#, &ctx)
            .unwrap()
            .0;
        assert_eq!(job.kind, JobKind::CleanupPlan);
        let jobs_dir = state.join("jobs");
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        run_with(&state, &job.job_id, |_, _| {
            progress(12_000, 50_000);
            std::thread::sleep(jobs::PROGRESS_INTERVAL + Duration::from_millis(400));
            let running = jobs::load(&jobs_dir, &job.job_id).unwrap();
            let shown = running.progress.expect("progress while planning");
            assert_eq!(
                (shown.units_done, shown.units_total),
                (12_000, Some(50_000))
            );
            assert_eq!(shown.phase, "plan");
            Ok((
                0,
                br#"{"command":"cleanup-plan","completion":"complete"}"#.to_vec(),
            ))
        })
        .unwrap();
        assert_eq!(
            jobs::load(&jobs_dir, &job.job_id).unwrap().state,
            JobState::Completed
        );
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn rootfs_report_uses_a_durable_job_and_preserves_partial_results() {
        let state = std::env::temp_dir().join(format!("reclaim-report-{}", std::process::id()));
        let ctx = Context {
            state_dir: &state,
            volumes: &[],
            requested_by: "admin",
        };
        let job = enqueue("rootfs.report", br#"{"max_age_s":0}"#, &ctx)
            .unwrap()
            .0;
        assert_eq!(job.kind, JobKind::RootfsReport);
        assert_eq!(job.phase, "report");
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        run_with(&state, &job.job_id, |request, user| {
            assert_eq!(request.method, "rootfs.report");
            assert_eq!(user, "admin");
            Ok((
                2,
                br#"{"command":"rootfs","completion":"partial","walk":{"entries":12}}"#.to_vec(),
            ))
        })
        .unwrap();
        let saved = jobs::load(&state.join("jobs"), &job.job_id).unwrap();
        assert_eq!(saved.state, JobState::Partial);
        assert_eq!(saved.result.unwrap().completion, "partial");
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_second_rootfs_report_joins_the_walk_already_queued_or_running() {
        let state =
            std::env::temp_dir().join(format!("reclaim-report-join-{}", std::process::id()));
        let ctx = Context {
            state_dir: &state,
            volumes: &[],
            requested_by: "admin",
        };
        let (first, joined) = enqueue("rootfs.report", b"{}", &ctx).unwrap();
        assert!(!joined);
        // Queued, then running: a refresh asked for meanwhile follows the same walk.
        let (again, joined) = enqueue("rootfs.report", br#"{"max_age_s":0}"#, &ctx).unwrap();
        assert!(joined);
        assert_eq!(again.job_id, first.job_id);
        let live = Live::launch(
            &state.join("jobs"),
            crate::sys::effective_uid(),
            &first.job_id,
            unix_seconds(SystemTime::now()),
        )
        .unwrap();
        let (again, joined) = enqueue("rootfs.report", b"{}", &ctx).unwrap();
        assert!(joined);
        assert_eq!(again.job_id, first.job_id);
        let listed = jobs::list(&state.join("jobs")).unwrap();
        assert_eq!(listed.len(), 1);
        drop(live);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_planning_worker_killed_after_cancel_stays_cancelled_without_a_result() {
        let state =
            std::env::temp_dir().join(format!("reclaim-plan-killed-{}", std::process::id()));
        let volumes = vec!["/volume1".into()];
        let ctx = Context {
            state_dir: &state,
            volumes: &volumes,
            requested_by: "admin",
        };
        let job = enqueue("rootfs.plan", br#"{"to":"/volume1/backup"}"#, &ctx)
            .unwrap()
            .0;
        let now = unix_seconds(SystemTime::now());
        let live = Live::launch(
            &state.join("jobs"),
            crate::sys::effective_uid(),
            &job.job_id,
            now,
        )
        .unwrap();
        jobs::request(
            &state.join("jobs"),
            crate::sys::effective_uid(),
            &job.job_id,
            Request::Cancel,
            now,
        )
        .unwrap();
        drop(live); // models executor grace/kill, before the worker saved an end
        let saved = jobs::load(&state.join("jobs"), &job.job_id).unwrap();
        assert_eq!(saved.state, JobState::Cancelled);
        assert!(saved.result.is_none());
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn planning_queue_is_durable_and_inspects_no_target_before_acceptance() {
        let state = std::env::temp_dir().join(format!("reclaim-plan-queue-{}", std::process::id()));
        let volumes = vec!["/volume1".into()];
        let ctx = Context {
            state_dir: &state,
            volumes: &volumes,
            requested_by: "admin",
        };
        let params = br#"{"paths":[{"path":"/volume1/does-not-exist","expect":{"filesystem_uuid":"00112233-4455-6677-8899-aabbccddeeff","tree":256,"objectid":10}}]}"#;
        let job = enqueue("cleanup.plan", params, &ctx).unwrap().0;
        let saved = jobs::load(&state.join("jobs"), &job.job_id).unwrap();
        assert_eq!(saved.state, JobState::Queued);
        assert_eq!(saved.planning.unwrap().params.as_bytes(), params);
        let invoked = AtomicBool::new(false);
        let id = job.job_id.clone();
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(run_with(&state,&id,|request,user| {
            invoked.store(true,Ordering::Relaxed);
            assert_eq!(user,"admin");
            assert_eq!(request.params.as_bytes(),params);
            Ok((0,serde_json::to_vec(&json!({"command":"cleanup-plan","completion":"complete","plan":{"plan_id":"cleanup-aa-bb","estimate":{"reclaimable":42}}})).unwrap()))
        }).unwrap(),0);
        assert!(invoked.load(Ordering::Relaxed));
        let saved = jobs::load(&state.join("jobs"), &id).unwrap();
        assert_eq!(saved.state, JobState::Completed);
        let result: Value = serde_json::from_slice(
            &std::fs::read(state.join("jobs").join(&id).join(jobs::RESULT)).unwrap(),
        )
        .unwrap();
        assert_eq!(result["plan"]["plan_id"], "cleanup-aa-bb");
        assert!(run_with(&state, &id, |_, _| panic!("job replayed")).is_err());
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn cancelling_a_queued_plan_prevents_the_worker_and_unknown_params_are_refused() {
        let state =
            std::env::temp_dir().join(format!("reclaim-plan-cancel-{}", std::process::id()));
        let volumes = vec!["/volume1".into()];
        let ctx = Context {
            state_dir: &state,
            volumes: &volumes,
            requested_by: "admin",
        };
        assert!(
            enqueue(
                "rootfs.plan",
                br#"{"to":"/volume1/backup","run":"rm"}"#,
                &ctx
            )
            .is_err()
        );
        let job = enqueue(
            "rootfs.plan",
            br#"{"to":"/volume1/backup","mode":"deep"}"#,
            &ctx,
        )
        .unwrap()
        .0;
        jobs::request(
            &state.join("jobs"),
            crate::sys::effective_uid(),
            &job.job_id,
            Request::Cancel,
            unix_seconds(SystemTime::now()),
        )
        .unwrap();
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        assert!(run_with(&state, &job.job_id, |_, _| panic!("cancelled planning ran")).is_err());
        assert_eq!(
            jobs::load(&state.join("jobs"), &job.job_id).unwrap().state,
            JobState::Cancelled
        );
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_cancelled_plan_never_returns_an_applicable_plan() {
        let captured = (
            0,
            serde_json::to_vec(
                &json!({"completion":"complete","plan":{"plan_id":"cleanup-aa-bb"}}),
            )
            .unwrap(),
        );
        let (code, doc) = terminal("cleanup.plan", "j-1", captured, true);
        assert_ne!(code, 0);
        assert_eq!(doc["completion"], "cancelled");
        assert_eq!(doc["job_id"], "j-1");
        assert!(doc.get("plan").is_none());
    }
    #[test]
    fn malformed_output_cannot_be_a_completed_plan() {
        let (code, doc) = terminal("rootfs.plan", "j-2", (0, b"not JSON".to_vec()), false);
        assert_ne!(code, 0);
        assert_eq!(doc["completion"], "failed");
    }
}
