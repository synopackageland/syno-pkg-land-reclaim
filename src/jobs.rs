//! Scan and cleanup jobs (SPEC 22.1, 5.7): a private manifest per job, a live lock its
//! running process holds, and pause or cancel requests from other
//! terminals. No process stays alive while a job is paused.

use crate::btrfs::leaf_ranges::DEFAULT_READ_BUDGET;
use crate::cli::ScanLimits;
use crate::progress::{CleanupProgress, JobProgress, RootfsPlanDetails, RootfsProgress};
use crate::report::hex_bytes;
use crate::resource::{MemoryLimit, Profile, SchedulingReport};
use crate::snapshots::{
    DEFAULT_GLOBAL_OWNER_BUDGET, DEFAULT_HELD_FILES_BUDGET, DEFAULT_OWNER_BUDGET, Depth,
};
use crate::state::{self, StateError, private_dir};
use crate::sys::ExclusiveLock;
use crate::target::Scope;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const JOB_FORMAT: u32 = 1;
const MANIFEST: &str = "job.json";
const LIVE: &str = "live.lock";
const CONTROL: &str = "control";
/// The profile a running scan is asked to go on with.
const PROFILE: &str = "profile";
/// A launcher must acquire the live lock within a minute of queueing.
const QUEUE_START_GRACE_S: u64 = 60;
/// The whole document of a job's latest run.
pub const RESULT: &str = "result.json";
/// The last phase: a running job no longer looks for requests.
pub const PUBLISH: &str = "publish";
/// How often a running job looks for pause and cancel requests.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How often a running job rewrites its live progress (SPEC 22.1).
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(2);
/// Finished jobs kept when a new one starts (SPEC 22.1 "bounded count").
pub const FINISHED_KEPT: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: damaged job manifest: {source}")]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("no job {0}")]
    NotFound(String),
    #[error("job {0} is running in another process")]
    Busy(String),
    #[error("scan {0} is already queued or running on this NAS")]
    ScanInProgress(String),
    #[error("job {0} is not running")]
    NotRunning(String),
    #[error("job {0} has finished")]
    Finished(String),
    #[error("job {0} is publishing its report and can no longer stop")]
    TooLate(String),
    #[error("job {0} has not started: run `synoreclaim scan --job-id {0}`")]
    NotStarted(String),
    #[error("job {0} has no finished run yet")]
    NoResult(String),
    #[error("job {0} has already been launched: `resume` runs it again")]
    NotQueued(String),
    #[error("job {0} is a non-scan job and cannot pause: `cancel` stops it at a safe boundary")]
    NotPausable(String),
    #[error("job {0} is a non-scan job: use its original planning or apply operation")]
    NotResumable(String),
    #[error("job {0} is a non-scan job and keeps the speed it started with")]
    NotAdjustable(String),
    #[error(
        "job {id} was started by synoreclaim {} (report format {}, cache format {}); this binary cannot resume it: start a new scan",
        started.tool_version, started.report_format, started.cache_format
    )]
    Incompatible { id: String, started: Versions },
    #[error("resume checkpoint cannot be used: {reason}; start a fresh scan")]
    Checkpoint { reason: String },
}

impl JobError {
    /// SPEC 22.7: a job that is running or from another binary is a
    /// refusal (4); every other answer is a failure (1).
    pub fn exit_code(&self) -> i32 {
        match self {
            JobError::Busy(_)
            | JobError::ScanInProgress(_)
            | JobError::Incompatible { .. }
            | JobError::Checkpoint { .. } => 4,
            _ => 1,
        }
    }

    /// The stable code of an answer to a job request, if it is one.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            JobError::NotFound(_) => Some("no_job"),
            JobError::Busy(_) => Some("busy"),
            JobError::ScanInProgress(_) => Some("scan_in_progress"),
            JobError::NotRunning(_) => Some("not_running"),
            JobError::Finished(_) => Some("finished"),
            JobError::TooLate(_) => Some("too_late"),
            JobError::NotStarted(_) => Some("not_started"),
            JobError::NotQueued(_) => Some("not_queued"),
            JobError::NoResult(_) => Some("no_result"),
            JobError::NotPausable(_) => Some("not_pausable"),
            JobError::NotResumable(_) => Some("not_resumable"),
            JobError::NotAdjustable(_) => Some("not_adjustable"),
            JobError::Incompatible { .. } => Some("job_incompatible"),
            JobError::Checkpoint { .. } => Some("checkpoint_unusable"),
            _ => None,
        }
    }
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> JobError + '_ {
    move |source| JobError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// SPEC 22.1 lifecycle. `Pausing`, `Cancelling` and `Interrupted` are
/// only derived: a running job asked to pause or cancel, and a running
/// job whose process died. `Queued` waits for a launcher to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Pausing,
    Cancelling,
    Paused,
    Completed,
    Partial,
    Failed,
    Cancelled,
    Interrupted,
}

impl JobState {
    pub fn name(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Pausing => "pausing",
            JobState::Cancelling => "cancelling",
            JobState::Paused => "paused",
            JobState::Completed => "completed",
            JobState::Partial => "partial",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
            JobState::Interrupted => "interrupted",
        }
    }

    pub fn is_finished(self) -> bool {
        matches!(
            self,
            JobState::Completed | JobState::Partial | JobState::Failed | JobState::Cancelled
        )
    }
}

/// What another terminal asks of a running job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    Pause,
    Cancel,
}

impl Request {
    pub fn name(self) -> &'static str {
        match self {
            Request::Pause => "pause",
            Request::Cancel => "cancel",
        }
    }
}

/// Rate-limits request checks, which read a file, at batch boundaries.
#[derive(Debug, Clone, Copy)]
pub struct Poll {
    every: Duration,
    next: Instant,
}

impl Poll {
    pub fn new(every: Duration, now: Instant) -> Poll {
        Poll { every, next: now }
    }

    /// Whether to check at `now`; if so, the next check is due `every`
    /// later.
    pub fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + self.every;
        true
    }
}

/// The scan limits a job was started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLimits {
    pub profile: Profile,
    /// `None`: `auto`.
    pub max_memory: Option<u64>,
    pub max_runtime_seconds: Option<u64>,
    pub no_cache: bool,
    pub max_cache: u64,
    pub min_free: u64,
    #[serde(default)]
    pub snapshots: Option<Depth>,
    #[serde(default)]
    pub snapshot_report: Option<crate::cli::SnapshotReport>,
    #[serde(default)]
    pub attribution_report: Option<crate::cli::SnapshotReport>,
    #[serde(default = "default_owner_budget")]
    pub owner_budget: u32,
    #[serde(default = "default_global_owner_budget")]
    pub global_owner_budget: u64,
    #[serde(default = "default_held_files_budget")]
    pub held_files_budget: u64,
    #[serde(default = "default_pinned_read_budget")]
    pub pinned_read_budget: u64,
}

fn default_held_files_budget() -> u64 {
    DEFAULT_HELD_FILES_BUDGET
}

fn default_pinned_read_budget() -> u64 {
    DEFAULT_READ_BUDGET
}

fn default_owner_budget() -> u32 {
    DEFAULT_OWNER_BUDGET
}

fn default_global_owner_budget() -> u64 {
    DEFAULT_GLOBAL_OWNER_BUDGET
}

impl JobLimits {
    pub fn of(limits: &ScanLimits) -> Self {
        JobLimits {
            profile: limits.profile,
            max_memory: match limits.max_memory {
                MemoryLimit::Auto => None,
                MemoryLimit::Bytes(bytes) => Some(bytes),
            },
            max_runtime_seconds: limits.max_runtime.map(|d| d.as_secs()),
            no_cache: limits.no_cache,
            max_cache: limits.max_cache,
            min_free: limits.min_free,
            snapshots: limits.snapshots,
            snapshot_report: limits.snapshot_report,
            attribution_report: limits.attribution_report,
            owner_budget: limits.owner_budget,
            global_owner_budget: limits.global_owner_budget,
            held_files_budget: limits.held_files_budget,
            pinned_read_budget: limits.pinned_read_budget,
        }
    }

    pub fn scan_limits(&self) -> ScanLimits {
        ScanLimits {
            profile: self.profile,
            max_memory: self
                .max_memory
                .map_or(MemoryLimit::Auto, MemoryLimit::Bytes),
            max_runtime: self.max_runtime_seconds.map(Duration::from_secs),
            no_cache: self.no_cache,
            max_cache: self.max_cache,
            min_free: self.min_free,
            snapshots: self.snapshots,
            snapshot_report: self.snapshot_report,
            attribution_report: self.attribution_report,
            owner_budget: self.owner_budget,
            global_owner_budget: self.global_owner_budget,
            held_files_budget: self.held_files_budget,
            pinned_read_budget: self.pinned_read_budget,
        }
    }
}

/// What a job runs (SPEC 5.7, 22.1). A manifest without `kind` is a
/// scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobKind {
    #[default]
    Scan,
    CleanupApply,
    RootfsApply,
    CleanupPlan,
    RootfsPlan,
    RootfsReport,
    DuplicatesAnalyze,
}

fn is_non_scan(kind: JobKind) -> bool {
    kind != JobKind::Scan
}

impl JobKind {
    pub fn name(self) -> &'static str {
        match self {
            JobKind::Scan => "scan",
            JobKind::CleanupApply => "cleanup-apply",
            JobKind::RootfsApply => "rootfs-apply",
            JobKind::CleanupPlan => "cleanup-plan",
            JobKind::RootfsPlan => "rootfs-plan",
            JobKind::RootfsReport => "rootfs-report",
            JobKind::DuplicatesAnalyze => "duplicates-analyze",
        }
    }
}

/// One target operand, hex-encoded so any name round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Operand(#[serde(with = "hex_bytes")] Vec<u8>);

/// How a job's latest run ended; its whole document is [`RESULT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub completion: String,
    pub exit_code: i32,
    /// The reports it published.
    reports: Vec<Operand>,
    /// The distinct codes of its errors.
    pub error_codes: Vec<String>,
}

impl JobResult {
    pub fn new(
        completion: &str,
        exit_code: i32,
        reports: &[Vec<u8>],
        error_codes: Vec<String>,
    ) -> Self {
        JobResult {
            completion: completion.to_string(),
            exit_code,
            reports: reports.iter().cloned().map(Operand).collect(),
            error_codes,
        }
    }

    pub fn reports(&self) -> Vec<Vec<u8>> {
        self.reports.iter().map(|r| r.0.clone()).collect()
    }
}

/// What started a job. Only the same tool and formats resume it
/// (SPEC 12); a manifest without them is from an older binary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    pub tool_version: String,
    pub report_format: u32,
    pub cache_format: u32,
}

impl Versions {
    pub fn current() -> Self {
        Versions {
            tool_version: env!("CARGO_PKG_VERSION").into(),
            report_format: crate::report::FORMAT,
            cache_format: crate::cache::FORMAT,
        }
    }
}

/// A filesystem a job's targets resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFilesystem {
    pub fsid: String,
    #[serde(with = "hex_bytes")]
    pub mount: Vec<u8>,
}

/// A job manifest (SPEC 12 `jobs/<job-id>/job.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub format: u32,
    #[serde(default)]
    pub versions: Versions,
    pub job_id: String,
    #[serde(default)]
    pub kind: JobKind,
    /// The sealed apply plan, available even while the job is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    /// Validated App planning request; private root state, never an argv supplied by ctl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<crate::planning::PlanningRequest>,
    pub state: JobState,
    /// `discover`, `scan`, `resolve-refs`, `aggregate` or `publish`; a
    /// cleanup job's is `execute`.
    pub phase: String,
    operands: Vec<Operand>,
    pub scope: Option<Scope>,
    /// What the targets resolved to in the latest run, in lock order.
    #[serde(default)]
    pub filesystems: Vec<JobFilesystem>,
    pub limits: JobLimits,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
    pub resumes: u32,
    /// Units scanned or reused by the latest run.
    pub units: u64,
    /// Live progress of the run; `None` before its first update and for
    /// a manifest written by an older binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<JobProgress>,
    /// Live progress of a cleanup job; `None` for a scan and before the
    /// executor starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<CleanupProgress>,
    /// Live progress of a rootfs apply; absent for scans and cleanup jobs.
    /// Rewritten at most every two seconds while it runs, then once more
    /// when the executor ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootfs: Option<RootfsProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootfs_plan: Option<RootfsPlanDetails>,
    /// Why the latest run stopped.
    pub reason: Option<String>,
    /// A pending pause or cancel request of a running job; derived.
    #[serde(skip)]
    pub request: Option<Request>,
    /// A request that came after the latest run's last check, so the run
    /// finished anyway.
    #[serde(default)]
    pub request_ignored: Option<Request>,
    /// How the latest run ended; `None` before one has.
    #[serde(default)]
    pub result: Option<JobResult>,
    /// The DSM user a `webapi` call started the job for (audit).
    #[serde(default)]
    pub requested_by: Option<String>,
}

impl Job {
    pub fn new(
        id: &str,
        operands: &[Vec<u8>],
        scope: Option<Scope>,
        limits: &ScanLimits,
        now: u64,
    ) -> Job {
        Job {
            format: JOB_FORMAT,
            versions: Versions::current(),
            job_id: id.to_string(),
            kind: JobKind::Scan,
            plan_id: None,
            planning: None,
            state: JobState::Running,
            phase: "discover".into(),
            operands: operands.iter().cloned().map(Operand).collect(),
            scope,
            filesystems: Vec::new(),
            limits: JobLimits::of(limits),
            created: now,
            updated: now,
            resumes: 0,
            units: 0,
            progress: None,
            cleanup: None,
            rootfs: None,
            rootfs_plan: None,
            reason: None,
            request: None,
            request_ignored: None,
            result: None,
            requested_by: None,
        }
    }

    /// A running `cleanup apply` of the plan selecting `targets` on
    /// `filesystem` (SPEC 5.7): no scope, units or scan progress.
    pub fn cleanup_apply(
        id: &str,
        targets: &[Vec<u8>],
        filesystem: JobFilesystem,
        profile: Profile,
        now: u64,
    ) -> Job {
        let limits = ScanLimits {
            profile,
            ..ScanLimits::report_only()
        };
        Job {
            kind: JobKind::CleanupApply,
            phase: "execute".into(),
            filesystems: vec![filesystem],
            ..Job::new(id, targets, None, &limits, now)
        }
    }

    /// A `rootfs apply` of its private system-partition plan, queued for
    /// the DSM package's launcher.
    pub fn rootfs_apply(
        id: &str,
        plan_path: &[u8],
        filesystem: JobFilesystem,
        profile: Profile,
        now: u64,
    ) -> Job {
        let limits = ScanLimits {
            profile,
            ..ScanLimits::report_only()
        };
        let operands = [plan_path.to_vec()];
        Job {
            kind: JobKind::RootfsApply,
            plan_id: std::str::from_utf8(plan_path)
                .ok()
                .and_then(|path| Path::new(path).file_stem())
                .and_then(|stem| stem.to_str())
                .map(str::to_owned),
            phase: "execute".into(),
            filesystems: vec![filesystem],
            ..Job::new(id, &operands, None, &limits, now)
        }
    }

    /// Whether this binary can resume the job.
    pub fn compatible(&self) -> bool {
        self.format == JOB_FORMAT && self.versions == Versions::current()
    }

    pub fn operands(&self) -> Vec<Vec<u8>> {
        self.operands.iter().map(|o| o.0.clone()).collect()
    }
}

/// A job this process runs, holding its live lock until dropped.
#[derive(Debug)]
pub struct Live {
    dir: PathBuf,
    pub job: Job,
    _lock: ExclusiveLock,
}

impl Live {
    /// Starts `job` under `jobs`.
    pub fn start(jobs: &Path, uid: u32, job: Job) -> Result<Live, JobError> {
        let dir = job_dir(jobs, &job.job_id)?;
        private_dir(jobs, uid)?;
        let _admission = admit_scan(jobs, uid, &job, false)?;
        private_dir(&dir, uid)?;
        let lock = take_live_lock(&dir, &job.job_id)?;
        let mut live = Live {
            dir,
            job,
            _lock: lock,
        };
        live.clear_request()?;
        live.save()?;
        Ok(live)
    }

    /// Runs a paused or interrupted job again in this process.
    pub fn resume(jobs: &Path, uid: u32, id: &str, now: u64) -> Result<Live, JobError> {
        let dir = existing_dir(jobs, id)?;
        private_dir(&dir, uid)?;
        let lock = take_live_lock(&dir, id)?;
        let mut job = read_manifest(&dir)?;
        if is_non_scan(job.kind) {
            return Err(JobError::NotResumable(id.to_string()));
        }
        if job.state.is_finished() {
            return Err(JobError::Finished(id.to_string()));
        }
        if job.state == JobState::Queued {
            return Err(JobError::NotStarted(id.to_string()));
        }
        if !job.compatible() {
            return Err(JobError::Incompatible {
                id: id.to_string(),
                started: job.versions,
            });
        }
        let _admission = admit_scan(jobs, uid, &job, true)?;
        job.state = JobState::Running;
        job.resumes += 1;
        job.updated = now;
        job.reason = None;
        let mut live = Live {
            dir,
            job,
            _lock: lock,
        };
        live.clear_request()?;
        live.save()?;
        Ok(live)
    }

    /// Runs queued job `id` in this process, keeping a request made
    /// while it waited to launch.
    pub fn launch(jobs: &Path, uid: u32, id: &str, now: u64) -> Result<Live, JobError> {
        let dir = existing_dir(jobs, id)?;
        private_dir(&dir, uid)?;
        let lock = take_live_lock(&dir, id)?;
        let mut job = read_manifest(&dir)?;
        if job.state.is_finished() {
            return Err(JobError::Finished(id.to_string()));
        }
        if job.state != JobState::Queued {
            return Err(JobError::NotQueued(id.to_string()));
        }
        if !job.compatible() {
            return Err(JobError::Incompatible {
                id: id.to_string(),
                started: job.versions,
            });
        }
        let _admission = admit_scan(jobs, uid, &job, true)?;
        job.state = JobState::Running;
        job.updated = now;
        let mut live = Live {
            dir,
            job,
            _lock: lock,
        };
        live.save()?;
        Ok(live)
    }

    /// A pause or cancel request from another terminal, if any.
    pub fn requested(&self) -> Option<Request> {
        read_request(&self.dir)
    }

    /// A profile another terminal asked the run to go on with, when it
    /// is not the one the run has.
    pub fn requested_profile(&self) -> Option<Profile> {
        read_profile(&self.dir).filter(|asked| *asked != self.job.limits.profile)
    }

    /// Goes on with the profile another terminal asked for, if any: its
    /// priority applies to this thread from now on and the job records it.
    #[cfg(target_os = "linux")]
    pub fn follow_profile(&mut self) -> Result<Option<SchedulingReport>, JobError> {
        let Some(profile) = self.requested_profile() else {
            return Ok(None);
        };
        let applied = crate::resource::apply_profile(profile);
        self.adopt_profile(profile)?;
        Ok(Some(applied))
    }

    /// Records that the run goes on with `profile`; a resume keeps it.
    pub fn adopt_profile(&mut self, profile: Profile) -> Result<(), JobError> {
        self.job.limits.profile = profile;
        self.save()
    }

    pub fn save(&mut self) -> Result<(), JobError> {
        let body = serde_json::to_vec_pretty(&self.job).expect("job serializes");
        state::replace_file(&self.dir, MANIFEST, |out| out.write_all(&body))?;
        Ok(())
    }

    /// Keeps how the run ended, with its whole `document`, for
    /// [`Live::finish`] to record.
    pub fn record_result(&mut self, result: JobResult, document: &[u8]) -> Result<(), JobError> {
        state::replace_file(&self.dir, RESULT, |out| out.write_all(document))?;
        self.job.result = Some(result);
        Ok(())
    }

    /// Rewrites the manifest for a live progress update without waiting
    /// for the disk (SPEC 22.1). The rename is still atomic, so a reader
    /// sees either the old or the new manifest, never a partial one; a
    /// crash may lose the last update, which is the point: progress is
    /// disposable, and making the filesystem commit every couple of
    /// seconds costs far more than the update is worth.
    pub fn save_progress(&mut self) -> Result<(), JobError> {
        let body = serde_json::to_vec_pretty(&self.job).expect("job serializes");
        state::replace_file_unflushed(&self.dir, MANIFEST, |out| out.write_all(&body))?;
        Ok(())
    }

    /// Records how the run ended and releases the job, with a request
    /// that came too late for a run that finished anyway.
    pub fn finish(
        mut self,
        state: JobState,
        reason: Option<&str>,
        now: u64,
    ) -> Result<Job, JobError> {
        self.job.request_ignored = match state {
            JobState::Completed | JobState::Partial | JobState::Failed
                if reason != Some("cancelled") =>
            {
                self.requested()
            }
            _ => None,
        };
        self.job.state = state;
        self.job.reason = reason.map(str::to_string);
        self.job.updated = now;
        self.save()?;
        self.clear_request()?;
        if state.is_finished() && self.job.kind == JobKind::Scan {
            let checkpoint = self.dir.join("checkpoint");
            match fs::remove_dir_all(&checkpoint) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                other => other.map_err(io_at(&checkpoint))?,
            }
        }
        Ok(self.job)
    }

    /// Removes the job without a trace, for a run that never started.
    pub fn discard(self) -> Result<(), JobError> {
        fs::remove_dir_all(&self.dir).map_err(io_at(&self.dir))
    }

    fn clear_request(&self) -> Result<(), JobError> {
        for name in [CONTROL, PROFILE] {
            let path = self.dir.join(name);
            match fs::remove_file(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                other => other.map_err(io_at(&path))?,
            }
        }
        Ok(())
    }
}

/// Records `job` under `jobs` for a launcher to run later.
pub fn queue(jobs: &Path, uid: u32, mut job: Job) -> Result<Job, JobError> {
    job.state = JobState::Queued;
    Ok(Live::start(jobs, uid, job)?.job)
}

/// Records `job` like [`queue`] unless a job of its kind is already queued or
/// running: then that job, and `true`, so a second request follows the work
/// already under way instead of repeating it. One lock per kind keeps two
/// simultaneous requests from both queueing.
pub fn queue_or_join(jobs: &Path, uid: u32, job: Job) -> Result<(Job, bool), JobError> {
    private_dir(jobs, uid)?;
    let lock_path = jobs.join(format!(".{}.lock", job.kind.name()));
    let _lock = ExclusiveLock::take(open_lock(&lock_path)?).map_err(io_at(&lock_path))?;
    if let Some(active) = list(jobs)?.into_iter().find(|candidate| {
        candidate.kind == job.kind
            && matches!(candidate.state, JobState::Queued | JobState::Running)
            && (job.kind != JobKind::DuplicatesAnalyze || same_volume(candidate, &job))
    }) {
        return Ok((active, true));
    }
    Ok((queue(jobs, uid, job)?, false))
}

/// Two duplicate analyses of one volume: the second joins the first.
fn same_volume(a: &Job, b: &Job) -> bool {
    let volume = |job: &Job| {
        job.planning.as_ref().and_then(|p| {
            serde_json::from_str::<serde_json::Value>(&p.params)
                .ok()
                .map(|v| v["volume"].clone())
        })
    };
    volume(a).is_some() && volume(a) == volume(b)
}

/// A persistent reservation bridges queue publication and launcher startup.
/// Production uses one canonical host directory even with another --state-dir.
#[derive(Serialize, Deserialize)]
struct ScanOwner {
    jobs: PathBuf,
    job_id: String,
}

fn admission_dir(jobs: &Path) -> PathBuf {
    #[cfg(test)]
    {
        jobs.join(".admission")
    }
    #[cfg(not(test))]
    {
        let _ = jobs;
        PathBuf::from(state::LOCK_DIR)
    }
}

fn admit_scan(
    jobs: &Path,
    uid: u32,
    job: &Job,
    own: bool,
) -> Result<Option<ExclusiveLock>, JobError> {
    if job.kind != JobKind::Scan {
        return Ok(None);
    }
    admit_scan_in(&admission_dir(jobs), jobs, uid, &job.job_id, own).map(Some)
}

fn admit_scan_in(
    dir: &Path,
    jobs: &Path,
    uid: u32,
    id: &str,
    own: bool,
) -> Result<ExclusiveLock, JobError> {
    let jobs = fs::canonicalize(jobs).map_err(io_at(jobs))?;
    let jobs = jobs.as_path();
    private_dir(dir, uid)?;
    let lock_path = dir.join("scan-admission.lock");
    let file = open_lock(&lock_path)?;
    let lock = ExclusiveLock::take(file).map_err(io_at(&lock_path))?;
    let owner_path = dir.join("scan-owner.json");
    match fs::read(&owner_path) {
        Ok(body) => {
            let owner: ScanOwner =
                serde_json::from_slice(&body).map_err(|source| JobError::Corrupt {
                    path: owner_path.clone(),
                    source,
                })?;
            if !(own && owner.job_id == id && owner.jobs == jobs) {
                match load(&owner.jobs, &owner.job_id) {
                    Ok(active)
                        if matches!(
                            active.state,
                            JobState::Queued
                                | JobState::Running
                                | JobState::Pausing
                                | JobState::Cancelling
                        ) =>
                    {
                        return Err(if owner.job_id == id && owner.jobs == jobs {
                            JobError::Busy(owner.job_id)
                        } else {
                            JobError::ScanInProgress(owner.job_id)
                        });
                    }
                    Ok(_) | Err(JobError::NotFound(_)) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_at(&owner_path)(error)),
    }
    // A runtime-directory reset must not forget persisted queued jobs.
    if let Some(active) = list(jobs)?.into_iter().find(|candidate| {
        candidate.kind == JobKind::Scan
            && !(own && candidate.job_id == id)
            && matches!(
                candidate.state,
                JobState::Queued | JobState::Running | JobState::Pausing | JobState::Cancelling
            )
    }) {
        return Err(if active.job_id == id {
            JobError::Busy(active.job_id)
        } else {
            JobError::ScanInProgress(active.job_id)
        });
    }
    let owner = ScanOwner {
        jobs: jobs.to_path_buf(),
        job_id: id.into(),
    };
    let body = serde_json::to_vec(&owner).expect("scan owner serializes");
    state::replace_file(dir, "scan-owner.json", |out| out.write_all(&body))?;
    Ok(lock)
}

/// Reserves a detached resume before returning its launcher argv.
pub fn queue_resume(
    jobs: &Path,
    uid: u32,
    id: &str,
    profile: Option<Profile>,
    now: u64,
) -> Result<Job, JobError> {
    let mut live = Live::resume(jobs, uid, id, now)?;
    live.job.state = JobState::Queued;
    if let Some(profile) = profile {
        live.job.limits.profile = profile;
    }
    live.save()?;
    Ok(live.job)
}

/// Job IDs name directories: plain names only.
fn job_dir(jobs: &Path, id: &str) -> Result<PathBuf, JobError> {
    let plain = !id.is_empty()
        && !id.starts_with('.')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !plain {
        return Err(JobError::NotFound(id.to_string()));
    }
    Ok(jobs.join(id))
}

fn existing_dir(jobs: &Path, id: &str) -> Result<PathBuf, JobError> {
    let dir = job_dir(jobs, id)?;
    if !dir.join(MANIFEST).exists() {
        return Err(JobError::NotFound(id.to_string()));
    }
    Ok(dir)
}

fn open_live_lock(dir: &Path) -> Result<File, JobError> {
    open_lock(&dir.join(LIVE))
}

fn open_lock(path: &Path) -> Result<File, JobError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(io_at(path))
}

fn take_live_lock(dir: &Path, id: &str) -> Result<ExclusiveLock, JobError> {
    match ExclusiveLock::try_take(open_live_lock(dir)?) {
        Ok(Ok(lock)) => Ok(lock),
        Ok(Err(_)) => Err(JobError::Busy(id.to_string())),
        Err(e) => Err(io_at(&dir.join(LIVE))(e)),
    }
}

/// The live lock if no process holds it: proof the job is not running.
fn lock_if_idle(dir: &Path, id: &str) -> Result<Option<ExclusiveLock>, JobError> {
    match take_live_lock(dir, id) {
        Ok(file) => Ok(Some(file)),
        Err(JobError::Busy(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_manifest(dir: &Path) -> Result<Job, JobError> {
    let path = dir.join(MANIFEST);
    let body = fs::read(&path).map_err(io_at(&path))?;
    serde_json::from_slice(&body).map_err(|source| JobError::Corrupt { path, source })
}

fn read_request(dir: &Path) -> Option<Request> {
    match fs::read(dir.join(CONTROL)).ok()?.trim_ascii() {
        b"pause" => Some(Request::Pause),
        b"cancel" => Some(Request::Cancel),
        _ => None,
    }
}

fn read_profile(dir: &Path) -> Option<Profile> {
    match fs::read(dir.join(PROFILE)).ok()?.trim_ascii() {
        b"low" => Some(Profile::Low),
        b"normal" => Some(Profile::Normal),
        b"fast" => Some(Profile::Fast),
        _ => None,
    }
}

/// Job `id` as it stands, with a running job whose process died shown
/// as interrupted and one asked to stop as pausing or cancelling.
pub fn load(jobs: &Path, id: &str) -> Result<Job, JobError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    load_at(jobs, id, now)
}

fn load_at(jobs: &Path, id: &str, now: u64) -> Result<Job, JobError> {
    let dir = existing_dir(jobs, id)?;
    let mut job = read_manifest(&dir)?;
    if job.state == JobState::Queued
        && now.saturating_sub(job.updated) >= QUEUE_START_GRACE_S
        && let Some(_lock) = lock_if_idle(&dir, id)?
    {
        // A launcher may have started between the first read and the lock.
        // Re-read while excluding launch(), then persist cancellation so a
        // delayed destructive launcher cannot run it later.
        job = read_manifest(&dir)?;
        if job.state == JobState::Queued && now.saturating_sub(job.updated) >= QUEUE_START_GRACE_S {
            job.state = JobState::Cancelled;
            job.reason = Some("not_started".into());
            job.updated = now;
            let body = serde_json::to_vec_pretty(&job).expect("job serializes");
            state::replace_file(&dir, MANIFEST, |out| out.write_all(&body))?;
        }
    }
    if job.state == JobState::Running {
        if lock_if_idle(&dir, id)?.is_some() {
            if matches!(
                job.kind,
                JobKind::CleanupPlan | JobKind::RootfsPlan | JobKind::RootfsReport
            ) && read_request(&dir) == Some(Request::Cancel)
            {
                job.state = JobState::Cancelled;
                job.reason = Some("cancelled".into());
                job.result = None;
            } else {
                job.state = JobState::Interrupted;
            }
        } else {
            job.request = read_request(&dir);
            job.state = asked(job.request);
        }
    }
    Ok(job)
}

/// Job `id` when `resume` can run it now: paused or interrupted, and
/// started by this binary. Nothing is locked or changed.
pub fn resumable(jobs: &Path, id: &str) -> Result<Job, JobError> {
    let job = load(jobs, id)?;
    let id = id.to_string();
    if is_non_scan(job.kind) {
        return Err(JobError::NotResumable(id));
    }
    match job.state {
        JobState::Paused | JobState::Interrupted => {}
        JobState::Queued => return Err(JobError::NotStarted(id)),
        state if state.is_finished() => return Err(JobError::Finished(id)),
        _ => return Err(JobError::Busy(id)),
    }
    if !job.compatible() {
        return Err(JobError::Incompatible {
            id,
            started: job.versions,
        });
    }
    Ok(job)
}

/// The state of a live job with `request` pending.
fn asked(request: Option<Request>) -> JobState {
    match request {
        Some(Request::Pause) => JobState::Pausing,
        Some(Request::Cancel) => JobState::Cancelling,
        None => JobState::Running,
    }
}

/// Every job under `jobs`, newest first; unreadable ones are skipped.
pub fn list(jobs: &Path) -> Result<Vec<Job>, JobError> {
    let entries = match fs::read_dir(jobs) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other.map_err(io_at(jobs))?,
    };
    let mut found = Vec::new();
    for entry in entries {
        let name = entry.map_err(io_at(jobs))?.file_name();
        if let Some(job) = name.to_str().and_then(|id| load(jobs, id).ok()) {
            found.push(job);
        }
    }
    found.sort_by(|a, b| (b.created, &b.job_id).cmp(&(a.created, &a.job_id)));
    Ok(found)
}

/// Asks running job `id` to pause or cancel at its next safe boundary;
/// cancelling a stopped job ends it at once. Only the owner of the state
/// directory may ask. Returns the job's state after the request.
pub fn request(
    jobs: &Path,
    uid: u32,
    id: &str,
    request: Request,
    now: u64,
) -> Result<JobState, JobError> {
    private_dir(jobs, uid)?;
    let dir = existing_dir(jobs, id)?;
    let mut job = read_manifest(&dir)?;
    if job.state.is_finished() {
        return Err(JobError::Finished(id.to_string()));
    }
    if is_non_scan(job.kind) && request == Request::Pause {
        return Err(JobError::NotPausable(id.to_string()));
    }
    let Some(lock) = lock_if_idle(&dir, id)? else {
        if job.phase == PUBLISH {
            return Err(JobError::TooLate(id.to_string()));
        }
        state::replace_file(&dir, CONTROL, |out| writeln!(out, "{}", request.name()))?;
        return Ok(asked(Some(request)));
    };
    let state = if request == Request::Pause {
        if job.kind != JobKind::Scan || job.state != JobState::Queued || job.resumes == 0 {
            return Err(JobError::NotRunning(id.to_string()));
        }
        JobState::Paused
    } else {
        JobState::Cancelled
    };
    job.updated = now;
    let live = Live {
        dir,
        job,
        _lock: lock,
    };
    let reason = if state == JobState::Paused {
        "paused"
    } else {
        "cancelled"
    };
    Ok(live.finish(state, Some(reason), now)?.state)
}

/// Asks running scan `id` to go on with `profile` from its next check:
/// it keeps its progress and only its priority changes. Only the owner
/// of the state directory may ask. Returns the job's state.
pub fn change_profile(
    jobs: &Path,
    uid: u32,
    id: &str,
    profile: Profile,
) -> Result<JobState, JobError> {
    private_dir(jobs, uid)?;
    let dir = existing_dir(jobs, id)?;
    let job = read_manifest(&dir)?;
    if job.state.is_finished() {
        return Err(JobError::Finished(id.to_string()));
    }
    if is_non_scan(job.kind) {
        return Err(JobError::NotAdjustable(id.to_string()));
    }
    if lock_if_idle(&dir, id)?.is_some() {
        return Err(JobError::NotRunning(id.to_string()));
    }
    if job.phase == PUBLISH {
        return Err(JobError::TooLate(id.to_string()));
    }
    state::replace_file(&dir, PROFILE, |out| writeln!(out, "{}", profile.name()))?;
    Ok(asked(read_request(&dir)))
}

/// Removes finished jobs beyond the newest `keep`.
pub fn prune(jobs: &Path, keep: usize) -> Result<(), JobError> {
    for job in list(jobs)?
        .into_iter()
        .filter(|j| j.state.is_finished())
        .skip(keep)
    {
        let dir = jobs.join(&job.job_id);
        fs::remove_dir_all(&dir).map_err(io_at(&dir))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_only_jobs_keep_the_saved_report_and_memory_limit_on_resume() {
        let mut limits = ScanLimits::report_only();
        limits.snapshots = Some(Depth::All);
        limits.snapshot_report = Some("saved-scan".parse().unwrap());
        let saved = JobLimits::of(&limits);
        let encoded = serde_json::to_vec(&saved).unwrap();
        let decoded: JobLimits = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.scan_limits(), limits);
        assert_eq!(decoded.scan_limits().max_memory, MemoryLimit::Auto);
        let mut damaged = serde_json::to_value(&saved).unwrap();
        damaged["snapshot_report"] = serde_json::json!("../report");
        assert!(serde_json::from_value::<JobLimits>(damaged).is_err());
    }

    #[test]
    fn planning_jobs_refuse_scan_only_controls() {
        for kind in ["cleanup-plan", "rootfs-plan", "rootfs-report"] {
            let decoded: JobKind = serde_json::from_value(serde_json::json!(kind)).unwrap();
            assert_eq!(decoded.name(), kind);
            assert!(is_non_scan(decoded));
        }
    }
    use crate::output::JsonPath;
    use crate::progress::JobBasis;
    use crate::spill::testing::scratch;
    use std::os::unix::fs::MetadataExt;

    fn uid() -> u32 {
        std::fs::metadata(scratch("uid")).unwrap().uid()
    }

    fn job(id: &str) -> Job {
        let limits = ScanLimits {
            profile: Profile::Normal,
            max_memory: MemoryLimit::Bytes(256 << 20),
            max_runtime: Some(Duration::from_secs(1800)),
            no_cache: false,
            max_cache: 1 << 30,
            min_free: 512 << 20,
            snapshots: Some(Depth::Newest(3)),
            snapshot_report: None,
            attribution_report: None,
            owner_budget: 64,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            held_files_budget: 5,
            pinned_read_budget: DEFAULT_READ_BUDGET,
        };
        Job::new(
            id,
            &[b"/volume1".to_vec(), b"/v/\xff".to_vec()],
            None,
            &limits,
            100,
        )
    }

    fn cleanup_job(id: &str) -> Job {
        Job::cleanup_apply(
            id,
            &[b"/volume1/share/old".to_vec()],
            JobFilesystem {
                fsid: "f".repeat(32),
                mount: b"/volume1".to_vec(),
            },
            Profile::Low,
            100,
        )
    }

    fn current_time() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn recent_job(id: &str) -> Job {
        let mut job = job(id);
        job.updated = current_time();
        job
    }

    #[test]
    fn cancelling_a_paused_scan_removes_its_private_checkpoint() {
        let jobs = scratch("cancel-checkpoint").join("jobs");
        let live = Live::start(&jobs, uid(), recent_job("scan1")).unwrap();
        let checkpoint = jobs.join("scan1/checkpoint");
        fs::create_dir(&checkpoint).unwrap();
        fs::write(checkpoint.join("saved"), b"checkpoint").unwrap();
        live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        assert!(checkpoint.is_dir(), "pause retains the checkpoint");
        request(&jobs, uid(), "scan1", Request::Cancel, 300).unwrap();
        assert!(
            !checkpoint.exists(),
            "cancel releases the job's checkpoint files"
        );
        assert_eq!(load(&jobs, "scan1").unwrap().state, JobState::Cancelled);
    }

    #[test]
    fn queue_or_join_follows_a_queued_or_running_job_of_the_same_kind_only() {
        let jobs = scratch("queue-or-join").join("jobs");
        let report = |id: &str| Job {
            kind: JobKind::RootfsReport,
            ..recent_job(id)
        };
        let (first, joined) = queue_or_join(&jobs, uid(), report("r1")).unwrap();
        assert_eq!((first.job_id.as_str(), joined), ("r1", false));
        let (same, joined) = queue_or_join(&jobs, uid(), report("r2")).unwrap();
        assert_eq!((same.job_id.as_str(), joined), ("r1", true));
        assert!(!jobs.join("r2").exists());
        // Another kind of work is never joined to it.
        let plan = Job {
            kind: JobKind::RootfsPlan,
            ..recent_job("p1")
        };
        assert!(!queue_or_join(&jobs, uid(), plan).unwrap().1);
        let live = Live::launch(&jobs, uid(), "r1", current_time()).unwrap();
        assert_eq!(
            queue_or_join(&jobs, uid(), report("r3")).unwrap().0.job_id,
            "r1"
        );
        // An ended or cancelled job is not followed: the next request starts anew.
        live.finish(JobState::Completed, None, current_time())
            .unwrap();
        let (next, joined) = queue_or_join(&jobs, uid(), report("r4")).unwrap();
        assert_eq!((next.job_id.as_str(), joined), ("r4", false));
        request(&jobs, uid(), "r4", Request::Cancel, current_time()).unwrap();
        assert!(!queue_or_join(&jobs, uid(), report("r5")).unwrap().1);
    }

    #[test]
    fn admission_recovers_an_expired_queue_before_accepting_a_new_scan() {
        let jobs = scratch("expired-admission").join("jobs");
        queue(&jobs, uid(), job("old")).unwrap();
        queue(&jobs, uid(), recent_job("new")).unwrap();
        assert_eq!(
            load(&jobs, "old").unwrap().reason.as_deref(),
            Some("not_started")
        );
        assert_eq!(load(&jobs, "new").unwrap().state, JobState::Queued);
        assert!(Live::launch(&jobs, uid(), "old", current_time()).is_err());
    }

    #[test]
    fn scan_admission_refuses_other_volumes_while_queued_or_running() {
        for queued in [false, true] {
            let jobs = scratch("scan-admission").join("jobs");
            let first = recent_job("s1");
            let live = if queued {
                queue(&jobs, uid(), first).unwrap();
                None
            } else {
                Some(Live::start(&jobs, uid(), first).unwrap())
            };
            let mut second = recent_job("s2");
            second.operands = vec![Operand(b"/volume2".to_vec())];
            let refused = queue(&jobs, uid(), second).unwrap_err();
            assert_eq!(refused.code(), Some("scan_in_progress"));
            assert_eq!(refused.exit_code(), 4);
            assert!(refused.to_string().contains("s1"));
            assert!(!jobs.join("s2").exists());
            drop(live);
        }
    }

    #[test]
    fn scan_admission_is_atomic_for_simultaneous_admins() {
        let jobs = scratch("scan-race").join("jobs");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|n| {
                    let jobs = &jobs;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        queue(jobs, uid(), recent_job(&format!("s{n}")))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(list(&jobs).unwrap().len(), 1);
    }

    #[test]
    fn scan_admission_allows_interrupted_and_paused_scans_and_keeps_apply_separate() {
        let jobs = scratch("scan-release").join("jobs");
        drop(Live::start(&jobs, uid(), recent_job("s1")).unwrap());
        let live = Live::start(&jobs, uid(), recent_job("s2")).unwrap();
        live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        let apply = Live::start(&jobs, uid(), cleanup_job("c1")).unwrap();
        let scan = Live::start(&jobs, uid(), recent_job("s3")).unwrap();
        assert!(Live::resume(&jobs, uid(), "s2", current_time()).is_err());
        drop(scan);
        Live::resume(&jobs, uid(), "s2", current_time()).unwrap();
        drop(apply);
    }

    #[test]
    fn admission_registry_covers_different_state_directories_and_fails_closed() {
        let root = scratch("cross-state");
        let shared = root.join("locks");
        let first = root.join("state1/jobs");
        let second = root.join("state2/jobs");
        private_dir(&root.join("state1"), uid()).unwrap();
        private_dir(&root.join("state2"), uid()).unwrap();
        private_dir(&second, uid()).unwrap();
        queue(&first, uid(), recent_job("s1")).unwrap();
        // Register the first state directory in the shared host registry.
        drop(admit_scan_in(&shared, &first, uid(), "s1", true).unwrap());
        let refused = admit_scan_in(&shared, &second, uid(), "s2", false).unwrap_err();
        assert_eq!(refused.code(), Some("scan_in_progress"));
        request(&first, uid(), "s1", Request::Cancel, 200).unwrap();
        drop(admit_scan_in(&shared, &second, uid(), "s2", false).unwrap());
        fs::write(shared.join("scan-owner.json"), b"broken").unwrap();
        assert!(matches!(
            admit_scan_in(&shared, &second, uid(), "s3", false),
            Err(JobError::Corrupt { .. })
        ));
    }

    #[test]
    fn detached_resume_reserves_admission_until_its_one_launch() {
        let jobs = scratch("resume-admission").join("jobs");
        Live::start(&jobs, uid(), recent_job("s1"))
            .unwrap()
            .finish(JobState::Paused, Some("paused"), 200)
            .unwrap();
        let queued = queue_resume(&jobs, uid(), "s1", Some(Profile::Fast), current_time()).unwrap();
        assert_eq!(
            (queued.state, queued.resumes, queued.limits.profile),
            (JobState::Queued, 1, Profile::Fast)
        );
        assert_eq!(
            queue(&jobs, uid(), recent_job("s2")).unwrap_err().code(),
            Some("scan_in_progress")
        );
        assert!(queue_resume(&jobs, uid(), "s1", None, current_time()).is_err());
        let live = Live::launch(&jobs, uid(), "s1", current_time()).unwrap();
        assert_eq!(live.job.resumes, 1);
        live.finish(JobState::Completed, None, 400).unwrap();
        queue(&jobs, uid(), recent_job("s2")).unwrap();
    }

    #[test]
    fn losing_the_runtime_registry_does_not_admit_over_a_queued_job() {
        let jobs = scratch("registry-reset").join("jobs");
        queue(&jobs, uid(), recent_job("s1")).unwrap();
        fs::remove_file(admission_dir(&jobs).join("scan-owner.json")).unwrap();
        assert_eq!(
            queue(&jobs, uid(), recent_job("s2")).unwrap_err().code(),
            Some("scan_in_progress")
        );
    }

    #[test]
    fn queued_apply_jobs_expose_their_plan_before_live_progress() {
        let jobs = scratch("queued-apply-plan").join("jobs");
        let mut cleanup = cleanup_job("c1");
        cleanup.plan_id = Some("cleanup-plan1".into());
        queue(&jobs, uid(), cleanup).unwrap();
        let queued = load(&jobs, "c1").unwrap();
        assert_eq!(queued.plan_id.as_deref(), Some("cleanup-plan1"));
        assert!(queued.cleanup.is_none());
        let rootfs = Job::rootfs_apply(
            "r1",
            b"/state/rootfs-plans/rootfs-plan1.json",
            JobFilesystem {
                fsid: "f".repeat(32),
                mount: b"/".to_vec(),
            },
            Profile::Low,
            100,
        );
        queue(&jobs, uid(), rootfs).unwrap();
        let queued = load(&jobs, "r1").unwrap();
        assert_eq!(queued.plan_id.as_deref(), Some("rootfs-plan1"));
        assert!(queued.rootfs.is_none());
    }

    #[test]
    fn a_failed_launcher_can_put_a_queued_resume_back_in_paused_state() {
        let jobs = scratch("failed-resume-launch").join("jobs");
        Live::start(&jobs, uid(), recent_job("s1"))
            .unwrap()
            .finish(JobState::Paused, Some("paused"), 200)
            .unwrap();
        queue_resume(&jobs, uid(), "s1", None, current_time()).unwrap();
        assert_eq!(
            request(&jobs, uid(), "s1", Request::Pause, current_time()).unwrap(),
            JobState::Paused
        );
        assert_eq!(
            resumable(&jobs, "s1").unwrap().reason.as_deref(),
            Some("paused")
        );
        queue(&jobs, uid(), recent_job("s2")).unwrap();
    }

    #[test]
    fn a_manifest_without_a_kind_is_a_scan() {
        let mut value = serde_json::to_value(job("j1")).unwrap();
        assert_eq!(value["kind"], "scan");
        value.as_object_mut().unwrap().remove("kind");
        let job: Job = serde_json::from_value(value).unwrap();
        assert_eq!(job.kind, JobKind::Scan);
    }

    #[test]
    fn a_cleanup_job_names_its_targets_filesystem_and_profile() {
        let jobs = scratch("cleanup-job").join("jobs");
        Live::start(&jobs, uid(), cleanup_job("c1")).unwrap();
        let job = load(&jobs, "c1").unwrap();
        assert_eq!(job.kind, JobKind::CleanupApply);
        assert_eq!(job.kind.name(), "cleanup-apply");
        assert_eq!(job.phase, "execute");
        assert_eq!(job.operands(), [b"/volume1/share/old".to_vec()]);
        assert_eq!(job.filesystems[0].mount, b"/volume1".to_vec());
        assert_eq!(job.limits.profile, Profile::Low);
        assert_eq!((job.scope, job.units, job.progress), (None, 0, None));
        assert_eq!(job.state, JobState::Interrupted, "its process let go");
    }

    #[test]
    fn cleanup_progress_round_trips_and_a_scan_has_none() {
        let jobs = scratch("cleanup-progress").join("jobs");
        let mut live = Live::start(&jobs, uid(), cleanup_job("c1")).unwrap();
        assert!(
            serde_json::to_value(&live.job)
                .unwrap()
                .get("cleanup")
                .is_none()
        );
        live.job.cleanup = Some(CleanupProgress {
            plan_id: "plan-1".into(),
            items_total: 10,
            items_done: 4,
            moved: 3,
            already_absent: 1,
            bytes_total: 40 << 10,
            bytes_moved: 12 << 10,
            elapsed_s: 7,
            updated: "2026-09-30T01:02:03Z".into(),
            ..CleanupProgress::default()
        });
        live.save_progress().unwrap();
        let cleanup = load(&jobs, "c1").unwrap().cleanup.unwrap();
        assert_eq!((cleanup.items_done, cleanup.items_total), (4, 10));
        assert_eq!((cleanup.moved, cleanup.already_absent), (3, 1));
        assert_eq!(cleanup.bytes_moved, 12 << 10);
        assert_eq!(load_scan_cleanup(&jobs), None);
    }

    #[test]
    fn rootfs_progress_round_trips_with_its_job() {
        use crate::progress::RootfsProgress;
        use crate::rootfs::delegate::Delegate;

        let jobs = scratch("rootfs-progress").join("jobs");
        let mut job = Job::rootfs_apply(
            "rootfs-20260930T141500Z-7b",
            b"/var/lib/synoreclaim/rootfs-plans/rootfs-1-2.json",
            JobFilesystem {
                fsid: "11111111-2222-3333-4444-555555555555".into(),
                mount: b"/volume1".to_vec(),
            },
            Profile::Low,
            10,
        );
        job.requested_by = Some("admin".into());
        job.rootfs = Some(RootfsProgress {
            phase: "tools".into(),
            items_done: 3,
            items_total: 3,
            bytes_done: 12,
            bytes_total: 12,
            tool: Some(Delegate::DeleteJournal),
        });
        let live = Live::start(&jobs, uid(), job).unwrap();
        let loaded = load(&jobs, &live.job.job_id).unwrap();

        assert_eq!(loaded.kind.name(), "rootfs-apply");
        assert_eq!(
            loaded.rootfs.as_ref().unwrap().tool,
            Some(Delegate::DeleteJournal)
        );
        let value = serde_json::to_value(&loaded).unwrap();
        assert_eq!(value["rootfs"]["phase"], "tools");
        assert_eq!(value["rootfs"]["items_done"], 3);
        assert_eq!(value["rootfs"]["items_total"], 3);
        assert_eq!(value["rootfs"]["bytes_done"], 12);
        assert_eq!(value["rootfs"]["bytes_total"], 12);
        assert_eq!(value["rootfs"]["tool"], "delete_journal");
        assert_eq!(value["requested_by"], "admin");
    }

    fn load_scan_cleanup(jobs: &Path) -> Option<CleanupProgress> {
        Live::start(jobs, uid(), job("s1")).unwrap();
        load(jobs, "s1").unwrap().cleanup
    }

    #[test]
    fn a_cleanup_job_cancels_but_neither_pauses_nor_resumes() {
        let jobs = scratch("cleanup-control").join("jobs");
        let live = Live::start(&jobs, uid(), cleanup_job("c1")).unwrap();
        let paused = request(&jobs, uid(), "c1", Request::Pause, 150);
        assert!(matches!(paused, Err(JobError::NotPausable(_))));
        assert_eq!(paused.unwrap_err().code(), Some("not_pausable"));
        assert_eq!(live.requested(), None, "no pause request is left");
        assert_eq!(
            request(&jobs, uid(), "c1", Request::Cancel, 160).unwrap(),
            JobState::Cancelling
        );
        assert_eq!(live.requested(), Some(Request::Cancel));
        drop(live);
        let refused = resumable(&jobs, "c1");
        assert!(matches!(refused, Err(JobError::NotResumable(_))));
        assert_eq!(refused.unwrap_err().code(), Some("not_resumable"));
        assert!(matches!(
            Live::resume(&jobs, uid(), "c1", 200),
            Err(JobError::NotResumable(_))
        ));
        assert_eq!(load(&jobs, "c1").unwrap().state, JobState::Interrupted);
    }

    #[test]
    fn a_job_keeps_its_targets_and_limits_byte_for_byte() {
        let j = job("j1");
        assert_eq!(j.operands(), [b"/volume1".to_vec(), b"/v/\xff".to_vec()]);
        let limits = j.limits.scan_limits();
        assert_eq!(limits.profile, Profile::Normal);
        assert_eq!(limits.max_memory, MemoryLimit::Bytes(256 << 20));
        assert_eq!(limits.max_runtime, Some(Duration::from_secs(1800)));
        assert_eq!((limits.max_cache, limits.min_free), (1 << 30, 512 << 20));
        assert_eq!(limits.held_files_budget, 5);
    }

    #[test]
    fn a_job_written_before_the_held_files_budget_resumes_with_the_default() {
        let mut value = serde_json::to_value(&job("j1").limits).unwrap();
        value.as_object_mut().unwrap().remove("held_files_budget");
        let limits: JobLimits = serde_json::from_value(value).unwrap();
        assert_eq!(limits.held_files_budget, DEFAULT_HELD_FILES_BUDGET);
    }

    #[test]
    fn a_job_from_before_the_pinned_read_budget_uses_the_default() {
        let mut manifest = serde_json::to_value(job("j1").limits).unwrap();
        manifest
            .as_object_mut()
            .unwrap()
            .remove("pinned_read_budget");
        let limits: JobLimits = serde_json::from_value(manifest).unwrap();
        assert_eq!(limits.scan_limits().pinned_read_budget, DEFAULT_READ_BUDGET);
    }

    #[test]
    fn a_job_written_before_live_progress_loads_without_it() {
        let mut value = serde_json::to_value(job("j1")).unwrap();
        assert!(value.get("progress").is_none(), "{value}");
        value.as_object_mut().unwrap().remove("progress");
        let job: Job = serde_json::from_value(value).unwrap();
        assert_eq!(job.progress, None);
    }

    #[test]
    fn live_progress_round_trips_through_the_manifest() {
        let jobs = scratch("progress").join("jobs");
        let mut j = job("j1");
        j.progress = Some(JobProgress {
            phase: "scan".into(),
            step: None,
            units_done: 12,
            units_total: Some(190),
            entries: 1_200_000,
            items: 3_400_000,
            bytes_read: 9 << 20,
            elapsed_s: 148,
            rss_bytes: Some(34 << 20),
            subvolume_id: Some(256),
            restart: None,
            current_subvolume: Some(JsonPath::new(b"backup")),
            basis: Some(JobBasis {
                entries: 5_510_000,
                elapsed_s: 900,
                items: None,
            }),
            updated: "2026-09-29T19:20:00Z".into(),
        });
        Live::start(&jobs, uid(), j).unwrap();
        let progress = load(&jobs, "j1").unwrap().progress.unwrap();
        assert_eq!(progress.phase, "scan");
        assert_eq!((progress.units_done, progress.units_total), (12, Some(190)));
        assert_eq!(progress.entries, 1_200_000);
        assert_eq!(progress.bytes_read, 9 << 20);
        assert_eq!(progress.elapsed_s, 148);
        assert_eq!(progress.subvolume_id, Some(256));
        assert_eq!(
            progress.current_subvolume.unwrap().bytes(),
            b"backup".to_vec()
        );
        let basis = progress.basis.unwrap();
        assert_eq!((basis.entries, basis.elapsed_s), (5_510_000, 900));
        assert_eq!(basis.items, None);
        assert_eq!(progress.updated, "2026-09-29T19:20:00Z");
    }

    #[test]
    fn a_progress_update_is_written_atomically_without_a_temporary_left_behind() {
        let jobs = scratch("progress_light").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        live.job.progress = Some(JobProgress {
            phase: "scan".into(),
            items: 7,
            ..JobProgress::default()
        });
        live.save_progress().unwrap();
        assert!(!jobs.join("j1").join(".job.json.tmp").exists());
        assert_eq!(load(&jobs, "j1").unwrap().progress.unwrap().items, 7);
    }

    #[test]
    fn a_started_job_runs_until_its_process_lets_go_and_is_then_interrupted() {
        let jobs = scratch("jobs").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(load(&jobs, "j1").unwrap().state, JobState::Running);
        assert!(matches!(
            Live::start(&jobs, uid(), job("j1")),
            Err(JobError::Busy(_))
        ));
        drop(live);
        assert_eq!(load(&jobs, "j1").unwrap().state, JobState::Interrupted);
    }

    #[test]
    fn a_job_remembers_the_filesystems_its_targets_resolved_to() {
        let jobs = scratch("jobs").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert!(load(&jobs, "j1").unwrap().filesystems.is_empty());
        let resolved = vec![JobFilesystem {
            fsid: "0f".into(),
            mount: b"/volume\xff".to_vec(),
        }];
        live.job.filesystems = resolved.clone();
        live.save().unwrap();
        assert_eq!(load(&jobs, "j1").unwrap().filesystems, resolved);
    }

    #[test]
    fn a_job_written_before_filesystems_were_recorded_still_loads() {
        let mut value = serde_json::to_value(job("j1")).unwrap();
        value.as_object_mut().unwrap().remove("filesystems");
        let old: Job = serde_json::from_value(value).unwrap();
        assert!(old.filesystems.is_empty());
    }

    #[test]
    fn an_orphaned_queue_is_cancelled_once_without_running_any_kind() {
        for kind in [JobKind::Scan, JobKind::CleanupApply, JobKind::RootfsApply] {
            let jobs = scratch("orphaned-queue").join("jobs");
            let mut queued = job("j1");
            queued.kind = kind;
            queue(&jobs, uid(), queued).unwrap();
            let recovered = load(&jobs, "j1").unwrap();
            assert_eq!(recovered.state, JobState::Cancelled);
            assert_eq!(recovered.reason.as_deref(), Some("not_started"));
            assert!(recovered.result.is_none());
            assert!(recovered.cleanup.is_none());
            assert!(recovered.rootfs.is_none());
            assert_eq!(read_manifest(&jobs.join("j1")).unwrap(), recovered);
            assert!(matches!(
                Live::launch(&jobs, uid(), "j1", 300),
                Err(JobError::Finished(_))
            ));
        }
    }

    #[test]
    fn queue_recovery_keeps_recent_and_locked_launches() {
        let jobs = scratch("queue-grace").join("jobs");
        queue(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(load_at(&jobs, "j1", 159).unwrap().state, JobState::Queued);
        let lock = take_live_lock(&jobs.join("j1"), "j1").unwrap();
        assert_eq!(load_at(&jobs, "j1", 160).unwrap().state, JobState::Queued);
        drop(lock);
        assert_eq!(
            load_at(&jobs, "j1", 160).unwrap().state,
            JobState::Cancelled
        );
        assert_eq!(load_at(&jobs, "j1", 500).unwrap().updated, 160);
    }

    #[test]
    fn rootfs_recovery_retains_the_original_plan_and_destination() {
        let jobs = scratch("rootfs-recovery").join("jobs");
        let mut queued = job("j1");
        queued.kind = JobKind::RootfsApply;
        queued.rootfs_plan = Some(crate::progress::RootfsPlanDetails {
            mode: crate::rootfs::plan::CleanupMode::Standard,
            plan_id: "rootfs-1-2".into(),
            move_dir: crate::output::JsonPath::new(b"/volume1/recovered"),
        });
        queue(&jobs, uid(), queued.clone()).unwrap();
        let recovered = load(&jobs, "j1").unwrap();
        assert_eq!(recovered.rootfs_plan, queued.rootfs_plan);
    }

    #[test]
    fn a_queued_job_waits_for_one_launch() {
        let jobs = scratch("jobs").join("jobs");
        queue(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(load_at(&jobs, "j1", 100).unwrap().state, JobState::Queued);
        assert!(matches!(
            request(&jobs, uid(), "j1", Request::Pause, 150),
            Err(JobError::NotRunning(_))
        ));
        assert!(matches!(
            Live::resume(&jobs, uid(), "j1", 150),
            Err(JobError::NotStarted(_))
        ));
        let live = Live::launch(&jobs, uid(), "j1", 200).unwrap();
        assert_eq!((live.job.state, live.job.updated), (JobState::Running, 200));
        assert!(matches!(
            Live::launch(&jobs, uid(), "j1", 210),
            Err(JobError::Busy(_))
        ));
        live.finish(JobState::Paused, Some("paused"), 250).unwrap();
        let again = Live::launch(&jobs, uid(), "j1", 300).unwrap_err();
        assert_eq!(again.code(), Some("not_queued"));
    }

    #[test]
    fn a_queued_job_keeps_a_request_made_as_it_launches() {
        let jobs = scratch("jobs").join("jobs");
        queue(&jobs, uid(), job("j1")).unwrap();
        let dir = jobs.join("j1");
        let lock = take_live_lock(&dir, "j1").unwrap();
        request(&jobs, uid(), "j1", Request::Cancel, 150).unwrap();
        drop(lock);
        let live = Live::launch(&jobs, uid(), "j1", 200).unwrap();
        assert_eq!(live.requested(), Some(Request::Cancel));
    }

    #[test]
    fn cancelling_a_queued_job_ends_it_before_it_launches() {
        let jobs = scratch("jobs").join("jobs");
        queue(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(
            request(&jobs, uid(), "j1", Request::Cancel, 150).unwrap(),
            JobState::Cancelled
        );
        assert!(matches!(
            Live::launch(&jobs, uid(), "j1", 200),
            Err(JobError::Finished(_))
        ));
    }

    #[test]
    fn a_stopped_job_keeps_its_state_and_reason() {
        let jobs = scratch("jobs").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        live.job.units = 7;
        live.finish(JobState::Paused, Some("runtime_limit"), 200)
            .unwrap();
        let j = load(&jobs, "j1").unwrap();
        assert_eq!(j.state, JobState::Paused);
        assert_eq!(j.reason.as_deref(), Some("runtime_limit"));
        assert_eq!((j.units, j.updated), (7, 200));
    }

    #[test]
    fn a_finished_run_keeps_its_result_and_whole_document() {
        let jobs = scratch("jobs").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(load(&jobs, "j1").unwrap().result, None);
        let result = JobResult::new(
            "partial",
            3,
            &[b"/r/\xff.srrep".to_vec()],
            vec!["metadata_error".into()],
        );
        live.record_result(result.clone(), b"{\"command\":\"scan\"}\n")
            .unwrap();
        live.finish(JobState::Partial, None, 200).unwrap();
        let j = load(&jobs, "j1").unwrap();
        assert_eq!(j.result, Some(result));
        assert_eq!(j.result.unwrap().reports(), [b"/r/\xff.srrep".to_vec()]);
        let file = jobs.join("j1").join(RESULT);
        assert_eq!(std::fs::read(&file).unwrap(), b"{\"command\":\"scan\"}\n");
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_discarded_job_leaves_nothing_behind() {
        let jobs = scratch("jobs").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        live.discard().unwrap();
        assert!(matches!(load(&jobs, "j1"), Err(JobError::NotFound(_))));
        assert!(!jobs.join("j1").exists());
    }

    #[test]
    fn pause_reaches_only_a_live_job() {
        let jobs = scratch("jobs").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(live.requested(), None);
        assert_eq!(
            request(&jobs, uid(), "j1", Request::Pause, 150).unwrap(),
            JobState::Pausing
        );
        assert_eq!(live.requested(), Some(Request::Pause));
        let asked = load(&jobs, "j1").unwrap();
        assert_eq!(
            (asked.state, asked.request),
            (JobState::Pausing, Some(Request::Pause))
        );
        live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        assert!(matches!(
            request(&jobs, uid(), "j1", Request::Pause, 250),
            Err(JobError::NotRunning(_))
        ));
    }

    #[test]
    fn a_mode_change_reaches_a_live_scan_without_stopping_it() {
        let jobs = scratch("jobs").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(live.job.limits.profile, Profile::Normal);
        assert_eq!(live.requested_profile(), None);
        change_profile(&jobs, uid(), "j1", Profile::Normal).unwrap();
        assert_eq!(live.requested_profile(), None, "the mode it already has");
        assert_eq!(
            change_profile(&jobs, uid(), "j1", Profile::Fast).unwrap(),
            JobState::Running
        );
        assert_eq!(live.requested(), None, "a mode change is no pause");
        assert_eq!(live.requested_profile(), Some(Profile::Fast));
        live.adopt_profile(Profile::Fast).unwrap();
        assert_eq!(live.requested_profile(), None);
        assert_eq!(load(&jobs, "j1").unwrap().limits.profile, Profile::Fast);
        let paused = live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        assert_eq!(
            paused.limits.profile,
            Profile::Fast,
            "a resume keeps the mode"
        );
        assert!(!jobs.join("j1").join(PROFILE).exists());
        assert!(matches!(
            change_profile(&jobs, uid(), "j1", Profile::Low),
            Err(JobError::NotRunning(_))
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_running_scan_follows_a_mode_change_at_its_check() {
        let jobs = scratch("jobs").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(live.follow_profile().unwrap(), None);
        change_profile(&jobs, uid(), "j1", Profile::Low).unwrap();
        let applied = live.follow_profile().unwrap().unwrap();
        assert_eq!(applied.profile, Profile::Low);
        assert_eq!(live.job.state, JobState::Running);
        assert_eq!(load(&jobs, "j1").unwrap().limits.profile, Profile::Low);
        assert_eq!(live.follow_profile().unwrap(), None, "applied once");
    }

    #[test]
    fn a_mode_change_is_only_for_a_running_scan() {
        let jobs = scratch("jobs").join("jobs");
        assert!(matches!(
            change_profile(&jobs, uid(), "nope", Profile::Fast),
            Err(JobError::NotFound(_))
        ));
        let apply = Live::start(&jobs, uid(), cleanup_job("c1")).unwrap();
        assert!(matches!(
            change_profile(&jobs, uid(), "c1", Profile::Fast),
            Err(JobError::NotAdjustable(_))
        ));
        apply.finish(JobState::Completed, None, 200).unwrap();
        let live = Live::start(&jobs, uid(), job("j2")).unwrap();
        live.finish(JobState::Completed, None, 200).unwrap();
        assert!(matches!(
            change_profile(&jobs, uid(), "j2", Profile::Fast),
            Err(JobError::Finished(_))
        ));
    }

    #[test]
    fn a_stale_mode_change_does_not_outlive_its_run() {
        let jobs = scratch("jobs").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        change_profile(&jobs, uid(), "j1", Profile::Fast).unwrap();
        live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        let resumed = Live::resume(&jobs, uid(), "j1", 300).unwrap();
        assert_eq!(resumed.requested_profile(), None);
        assert_eq!(resumed.job.limits.profile, Profile::Normal);
    }

    #[test]
    fn cancel_asks_a_live_job_and_ends_a_stopped_one() {
        let jobs = scratch("jobs").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert_eq!(
            request(&jobs, uid(), "j1", Request::Cancel, 150).unwrap(),
            JobState::Cancelling
        );
        assert_eq!(live.requested(), Some(Request::Cancel));
        let asked = load(&jobs, "j1").unwrap();
        assert_eq!(
            (asked.state, asked.request),
            (JobState::Cancelling, Some(Request::Cancel))
        );
        live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        assert_eq!(
            request(&jobs, uid(), "j1", Request::Cancel, 250).unwrap(),
            JobState::Cancelled
        );
        assert_eq!(load(&jobs, "j1").unwrap().state, JobState::Cancelled);
        assert!(matches!(
            request(&jobs, uid(), "j1", Request::Cancel, 300),
            Err(JobError::Finished(_))
        ));
    }

    #[test]
    fn a_job_publishing_its_report_refuses_requests_as_too_late() {
        let jobs = scratch("jobs").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        live.job.phase = PUBLISH.into();
        live.save().unwrap();
        for asked in [Request::Pause, Request::Cancel] {
            let refused = request(&jobs, uid(), "j1", asked, 150).unwrap_err();
            assert_eq!(refused.code(), Some("too_late"), "{refused}");
        }
        assert_eq!(live.requested(), None, "nothing was asked");
    }

    #[test]
    fn a_mode_change_while_publishing_is_too_late_and_writes_no_request() {
        let jobs = scratch("publish-mode").join("jobs");
        let mut live = Live::start(&jobs, uid(), job("j1")).unwrap();
        live.job.phase = PUBLISH.into();
        live.save().unwrap();
        let refused = change_profile(&jobs, uid(), "j1", Profile::Fast).unwrap_err();
        assert_eq!(refused.code(), Some("too_late"));
        assert_eq!(live.requested_profile(), None);
    }

    #[test]
    fn a_run_that_finished_despite_a_request_records_it_as_ignored() {
        let jobs = scratch("jobs").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        request(&jobs, uid(), "j1", Request::Pause, 150).unwrap();
        live.finish(JobState::Completed, None, 200).unwrap();
        let done = load(&jobs, "j1").unwrap();
        assert_eq!(done.request_ignored, Some(Request::Pause));
        let live = Live::start(&jobs, uid(), job("j2")).unwrap();
        request(&jobs, uid(), "j2", Request::Pause, 150).unwrap();
        live.finish(JobState::Paused, Some("paused"), 200).unwrap();
        assert_eq!(load(&jobs, "j2").unwrap().request_ignored, None);
    }

    #[test]
    fn a_job_started_by_another_binary_is_not_resumed() {
        let jobs = scratch("jobs").join("jobs");
        let mut old = job("j1");
        old.versions.report_format -= 1;
        Live::start(&jobs, uid(), old)
            .unwrap()
            .finish(JobState::Paused, Some("paused"), 200)
            .unwrap();
        assert!(!load(&jobs, "j1").unwrap().compatible());
        let refused = Live::resume(&jobs, uid(), "j1", 300).unwrap_err();
        assert_eq!(refused.code(), Some("job_incompatible"), "{refused}");
        assert_eq!(refused.exit_code(), 4);
        assert_eq!(load(&jobs, "j1").unwrap().state, JobState::Paused);
        assert_eq!(
            request(&jobs, uid(), "j1", Request::Cancel, 400).unwrap(),
            JobState::Cancelled,
            "it can still be cancelled"
        );
    }

    #[test]
    fn a_job_written_without_versions_is_from_an_older_binary() {
        let mut value = serde_json::to_value(job("j1")).unwrap();
        value.as_object_mut().unwrap().remove("versions");
        let old: Job = serde_json::from_value(value).unwrap();
        assert!(!old.compatible());
        assert!(job("j2").compatible());
    }

    #[test]
    fn a_job_keeps_who_requested_it_and_older_jobs_name_nobody() {
        let mut requested = job("j1");
        requested.requested_by = Some("admin".into());
        let value = serde_json::to_value(&requested).unwrap();
        assert_eq!(value["requested_by"], "admin");
        let mut old = value;
        old.as_object_mut().unwrap().remove("requested_by");
        let old: Job = serde_json::from_value(old).unwrap();
        assert_eq!(old.requested_by, None);
    }

    #[test]
    fn a_job_without_a_finished_run_has_no_result() {
        let e = JobError::NoResult("j1".into());
        assert_eq!((e.code(), e.exit_code()), (Some("no_result"), 1));
    }

    #[test]
    fn only_a_stopped_or_interrupted_job_is_resumable() {
        let jobs = scratch("resumable").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        assert!(matches!(resumable(&jobs, "j1"), Err(JobError::Busy(_))));
        drop(live);
        assert_eq!(resumable(&jobs, "j1").unwrap().state, JobState::Interrupted);
        let mut paused = Live::resume(&jobs, uid(), "j1", 200).unwrap();
        paused.job.state = JobState::Paused;
        paused.save().unwrap();
        drop(paused);
        assert_eq!(resumable(&jobs, "j1").unwrap().state, JobState::Paused);
        let mut recent = job("j2");
        recent.updated = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        queue(&jobs, uid(), recent).unwrap();
        assert!(matches!(
            resumable(&jobs, "j2"),
            Err(JobError::NotStarted(_))
        ));
        request(&jobs, uid(), "j2", Request::Cancel, 250).unwrap();
        let mut old = job("j3");
        old.versions.tool_version = "0.0.1".into();
        drop(Live::start(&jobs, uid(), old).unwrap());
        assert!(matches!(
            resumable(&jobs, "j3"),
            Err(JobError::Incompatible { .. })
        ));
        Live::resume(&jobs, uid(), "j1", 300)
            .unwrap()
            .finish(JobState::Completed, None, 400)
            .unwrap();
        assert!(matches!(resumable(&jobs, "j1"), Err(JobError::Finished(_))));
        assert!(matches!(resumable(&jobs, "j9"), Err(JobError::NotFound(_))));
    }

    #[test]
    fn resume_restarts_a_stopped_or_interrupted_job_only() {
        let jobs = scratch("jobs").join("jobs");
        let live = Live::start(&jobs, uid(), job("j1")).unwrap();
        request(&jobs, uid(), "j1", Request::Pause, 150).unwrap();
        assert!(matches!(
            Live::resume(&jobs, uid(), "j1", 160),
            Err(JobError::Busy(_))
        ));
        drop(live);
        let resumed = Live::resume(&jobs, uid(), "j1", 300).unwrap();
        assert_eq!(resumed.job.state, JobState::Running);
        assert_eq!((resumed.job.resumes, resumed.job.updated), (1, 300));
        assert_eq!(resumed.requested(), None, "the old request is spent");
        resumed.finish(JobState::Completed, None, 400).unwrap();
        assert!(matches!(
            Live::resume(&jobs, uid(), "j1", 500),
            Err(JobError::Finished(_))
        ));
    }

    #[test]
    fn requests_are_polled_at_most_once_an_interval() {
        let start = Instant::now();
        let mut poll = Poll::new(Duration::from_secs(1), start);
        assert!(poll.due(start), "the first check is immediate");
        assert!(!poll.due(start + Duration::from_millis(500)));
        assert!(poll.due(start + Duration::from_millis(1000)));
        assert!(!poll.due(start + Duration::from_millis(1500)));
    }

    #[test]
    fn unknown_and_unsafe_job_ids_are_refused() {
        let jobs = scratch("jobs").join("jobs");
        assert!(matches!(load(&jobs, "nope"), Err(JobError::NotFound(_))));
        for id in ["../x", "a/b", "", "."] {
            assert!(
                matches!(load(&jobs, id), Err(JobError::NotFound(_))),
                "{id}"
            );
        }
    }

    #[test]
    fn jobs_list_newest_first_and_only_a_few_finished_ones_are_kept() {
        let jobs = scratch("jobs").join("jobs");
        for (i, id) in ["a", "b", "c", "d"].iter().enumerate() {
            let mut j = job(id);
            j.created = i as u64;
            let live = Live::start(&jobs, uid(), j).unwrap();
            let state = if *id == "b" {
                JobState::Paused
            } else {
                JobState::Completed
            };
            live.finish(state, None, 10).unwrap();
        }
        prune(&jobs, 1).unwrap();
        let ids: Vec<String> = list(&jobs).unwrap().into_iter().map(|j| j.job_id).collect();
        assert_eq!(
            ids,
            ["d", "b"],
            "the paused job and the newest finished one"
        );
    }
}
