#![cfg(target_os = "linux")]
// Exercise the compiled root entry, not a CLI-parser or fake-executor shortcut.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use syno_reclaim::cli::ScanLimits;
use syno_reclaim::jobs::{self, Job, JobKind, JobState, Request};
use syno_reclaim::planning::PlanningRequest;
use syno_reclaim::root_entry::{self, launch};

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("reclaim-planning-{name}-{}", std::process::id()));
        let now = syno_reclaim::time::unix_seconds(std::time::SystemTime::now());
        let mut job = Job::new("plan-1", &[], None, &ScanLimits::report_only(), now);
        job.kind = JobKind::CleanupPlan;
        job.phase = "plan".into();
        job.requested_by = Some("admin".into());
        // Invalid selection: acceptance and worker dispatch must not be mistaken
        // for a completed plan, and this fixture never plans real user data.
        job.planning = Some(PlanningRequest {
            method: "cleanup.plan".into(),
            params: "{\"paths\":[]}".into(),
        });
        jobs::queue(&path.join("jobs"), syno_reclaim::sys::effective_uid(), job).unwrap();
        launch::save(
            &path,
            &launch::Launch {
                job_id: "plan-1".into(),
                args: vec![
                    "planning".into(),
                    "--job-id".into(),
                    "plan-1".into(),
                    "--state-dir".into(),
                    path.to_str().unwrap().into(),
                ],
            },
        )
        .unwrap();
        Self(path)
    }
    fn run(&self) -> std::process::Output {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_synoreclaim"))
            .env(root_entry::MODE_ENV, "job")
            .env(root_entry::REQUESTED_BY_ENV, "admin")
            .env(root_entry::ROOT_STATE_ENV, &self.0)
            .env_remove(root_entry::CONTROL_FD_ENV)
            .env_remove(root_entry::PROGRESS_FD_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"job_id\":\"plan-1\"}")
            .unwrap();
        child.wait_with_output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn compiled_root_job_runs_the_planning_worker_once_and_persists_its_failure() {
    let fixture = Fixture::new("failure");
    let output = fixture.run();
    assert!(!output.status.success());
    let saved = jobs::load(&fixture.0.join("jobs"), "plan-1").unwrap();
    assert_eq!(
        saved.state,
        JobState::Failed,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(saved.result.is_some());
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.0.join("jobs/plan-1/result.json")).unwrap())
            .unwrap();
    assert_eq!(document["job_id"], "plan-1");
    assert_eq!(document["completion"], "failed");
    assert!(!fixture.run().status.success());
}

#[test]
fn compiled_root_job_cannot_start_a_cancelled_plan_or_publish_its_result() {
    let fixture = Fixture::new("cancel");
    jobs::request(
        &fixture.0.join("jobs"),
        syno_reclaim::sys::effective_uid(),
        "plan-1",
        Request::Cancel,
        syno_reclaim::time::unix_seconds(std::time::SystemTime::now()),
    )
    .unwrap();
    assert!(!fixture.run().status.success());
    assert_eq!(
        jobs::load(&fixture.0.join("jobs"), "plan-1").unwrap().state,
        JobState::Cancelled
    );
    assert!(!fixture.0.join("jobs/plan-1/result.json").exists());
}
