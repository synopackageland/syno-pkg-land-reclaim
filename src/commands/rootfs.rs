//! `rootfs`: what fills DSM's system partition, and the plan to move its
//! allowlisted leftovers into a shared folder. Root only; no Btrfs scan
//! and no state directory.

use super::{Failure, emit, finish_cleanup};
use crate::btrfs::keys::FIRST_FREE_OBJECTID;
use crate::btrfs::live::{LiveSource, OpenTarget};
use crate::btrfs::walk::{BatchBoundary, StopReason};
use crate::cli::Launcher;
use crate::document::{Completion, DetachDocument, Envelope};
use crate::guard::{self, Refusal};
use crate::jobs::{self, Job, JobFilesystem, JobResult, JobState, Live, Request};
use crate::output::{JsonPath, Notice, TargetError};
use crate::progress::RootfsProgress;
use crate::resolve::{Environment, Volume};
use crate::resource::{self, Pacer, Profile, SystemClock};
use crate::rootfs::apply::{self, ExecuteError};
use crate::rootfs::apply_doc::{self, ApplyInputs};
use crate::rootfs::delegate;
use crate::rootfs::history::{
    self, Apply as HistoryApply, Moved as HistoryMoved, Tool as HistoryTool,
};
use crate::rootfs::live::{self, Sources};
use crate::rootfs::plan::{self, CleanupMode, Destination, PlanPayload, RootfsPlan};
use crate::rootfs::plan_doc::{self, PlanInputs};
use crate::rootfs::report::render;
use crate::rootfs::update_guard;
use crate::rootfs::walk::{Limits, WalkStop};
use crate::state::{self, LockHolder, Store};
use crate::sys;
use crate::time::{rfc3339, unix_seconds};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::ops::ControlFlow;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// `rootfs [--json]`: the read-only report of `/`.
pub fn rootfs_report(
    json: bool,
    profile: Profile,
    state_dir: &Path,
    max_age_s: u64,
) -> Result<i32, Failure> {
    let result = require_root().and_then(|()| report(json, profile, state_dir, max_age_s));
    finish_cleanup(json, "rootfs", b"/", result)
}

/// `rootfs plan --to FOLDER [--json]`: plans the moves and writes the plan
/// into FOLDER; nothing on `/` changes.
pub fn rootfs_plan(
    to: &Path,
    mode: CleanupMode,
    json: bool,
    profile: Profile,
) -> Result<i32, Failure> {
    let result = require_root().and_then(|()| make_plan(to, mode, json, profile));
    finish_cleanup(json, "rootfs-plan", to.as_os_str().as_bytes(), result)
}

fn require_root() -> Result<(), Failure> {
    if sys::effective_uid() != 0 {
        return Err(Refusal::new("not_root", "rootfs requires root privileges").into());
    }
    Ok(())
}

fn report(json: bool, profile: Profile, state_dir: &Path, max_age_s: u64) -> Result<i32, Failure> {
    let uid = sys::effective_uid();
    if json && max_age_s > 0 {
        match crate::rootfs::report_cache::load(
            state_dir,
            uid,
            max_age_s,
            crate::time::unix_seconds(SystemTime::now()),
        ) {
            Ok(Some(cached)) => {
                std::io::stdout().write_all(cached.document.as_bytes())?;
                return Ok(cached.exit_code);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("synoreclaim: warning: rootfs report cache ignored: {error}");
            }
        }
    }
    let mounts = crate::mounts::read_mounts()?;
    let sources = live_sources(&mounts)?;
    let mut pacer = pacer(profile)?;
    let generated_at = SystemTime::now();
    let doc = live::report(&sources, Limits::default(), &mut pacer, generated_at)?;
    let exit_code = doc.envelope.completion.exit_code(sys::stop_signal());
    let document = serde_json::to_string_pretty(&doc)? + "\n";
    if let Err(error) = crate::rootfs::report_cache::save(
        state_dir,
        uid,
        crate::time::unix_seconds(generated_at),
        exit_code,
        &document,
    ) {
        eprintln!("synoreclaim: warning: rootfs report cache not saved: {error}");
    }
    if json {
        std::io::stdout().write_all(document.as_bytes())?;
    } else {
        emit(false, &doc, || render(&doc))?;
    }
    Ok(exit_code)
}

fn make_plan(to: &Path, mode: CleanupMode, json: bool, profile: Profile) -> Result<i32, Failure> {
    update_guard::check(Path::new("/"), "no plan was written")?;
    let to = guard::absolute_lexical(to)?;
    let to_bytes = to.as_os_str().as_bytes();
    let destination = guard::open_absolute_no_follow(&to)?;
    let destination_meta = destination.metadata()?;
    if !destination_meta.is_dir() {
        return Err(Refusal::new(
            "destination_not_directory",
            "--to must be an existing folder",
        )
        .into());
    }
    let env = Environment::load()?;
    let shares = destination_volume_shares(&env, &to)?;
    let root_device = std::fs::metadata("/")?.dev();
    let share = plan::destination_share(to_bytes, &shares, destination_meta.dev(), root_device)?;
    let volume = destination_volume(&env, &to)
        .ok_or_else(|| Refusal::new("destination_not_in_share", "data volume was not found"))?;
    let mut pacer = pacer(profile)?;
    let share_quota_available = match share_quota_subvolume(volume, &share)? {
        Some(id) => destination_share_quota(&volume.primary.mount_point, id, &mut pacer)?,
        None => None,
    };

    let sources = live_sources(&env.mounts)?;
    let survey = live::survey_mode(&sources, Limits::default(), &mut pacer, mode)?;
    if let Some(code) = guard::stop_before_executor(sys::stop_signal()) {
        return Ok(code);
    }
    if let Some(WalkStop::Stopped(reason)) = survey.walk.stopped {
        return Err(Refusal::new(
            "walk_stopped",
            format!("the walk of / stopped ({reason:?}); no plan was written"),
        )
        .into());
    }
    let open = open_files_on(survey.device, "no plan was written")?;
    let now = SystemTime::now();
    let created_at = crate::time::unix_seconds(now);
    let mut selection = plan::select_mode(
        &survey.walk.candidates,
        &open,
        i64::try_from(created_at).unwrap_or(i64::MAX),
        update_guard::boot_time(Path::new("/proc"))?,
        mode,
    );
    plan::keep_pinned(&mut selection, &survey.walk.pinned);
    if mode == CleanupMode::Deep {
        plan::keep_backup_installer(&mut selection, survey.header.available_bytes);
    }

    let delegates = delegate::available_mode(&survey.open_deleted, mode);
    let moved: BTreeSet<Vec<u8>> = selection.items.iter().map(|c| c.path.clone()).collect();
    let tool_estimates = delegates
        .iter()
        .filter_map(|&tool| {
            delegate::estimated_bytes(tool)
                .or_else(|| delegate::dry_run_estimate(tool, &moved))
                .map(|bytes| (tool, bytes))
        })
        .collect();
    let sealed = RootfsPlan::seal(PlanPayload {
        format: plan::PLAN_FORMAT,
        mode,
        plan_id: plan::new_plan_id(now, std::process::id()),
        created_at,
        expires_at: created_at + plan::PLAN_LIFETIME_SECONDS,
        root_device,
        destination: Destination {
            path: to_bytes.to_vec(),
            share,
            device: destination_meta.dev(),
            inode: destination_meta.ino(),
        },
        items: selection.items,
        delegates,
        tool_estimates,
    })?;
    // An update may have started during the walk.
    update_guard::check(Path::new("/"), "no plan was written")?;
    let destination_available = sys::filesystem_space(destination.as_fd())?.available;
    let destination_available_after_quota = share_quota_available
        .map_or(destination_available, |available| {
            destination_available.min(available)
        });
    let plan_file = if sealed.payload.items.is_empty() && sealed.payload.delegates.is_empty() {
        None
    } else {
        plan::check_space_with_quota(sealed.bytes(), destination_available, share_quota_available)?;
        let dir = Path::new(plan::PLAN_DIR);
        let plans = plan::open_plan_dir(dir, root_device, 0)?;
        let name = plan::write_plan(&plans, &sealed)?;
        Some(dir.join(name).as_os_str().as_bytes().to_vec())
    };
    let doc = plan_doc::build(
        PlanInputs {
            plan: &sealed,
            plan_file: plan_file.as_deref(),
            held: &selection.held,
            kept: &selection.kept,
            root_available: survey.header.available_bytes,
            destination_available: destination_available_after_quota,
            walk_stop: survey.walk.stopped,
        },
        now,
    );
    emit(json, &doc, || plan_doc::render(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

/// Applies a rootfs plan directly or queues/runs it through the detached
/// launcher contract (SPEC 5.21).
#[allow(clippy::too_many_arguments)]
pub fn rootfs_apply(
    plan: &Path,
    yes: bool,
    json: bool,
    profile: Profile,
    detach: Option<Launcher>,
    job_id: Option<&str>,
    requested_by: Option<&str>,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let subject = plan.as_os_str().as_bytes();
    if !yes {
        return finish_rootfs_apply(
            json,
            subject,
            Err(Refusal::new("yes_required", "rootfs apply requires --yes").into()),
        );
    }
    if let Err(error) = require_root() {
        return finish_rootfs_apply(json, subject, Err(error));
    }
    if let Some(id) = job_id {
        return run_queued_rootfs_apply(plan, json, id, state_dir);
    }
    if let Some(launcher) = detach {
        return finish_rootfs_apply(
            true,
            subject,
            detach_rootfs_apply(plan, profile, requested_by, launcher, state_dir),
        );
    }
    let result = Store::open(state_dir, sys::effective_uid())
        .map_err(Failure::from)
        .and_then(|_| apply_once(plan, profile, requested_by, None, None, state_dir))
        .map(|run| run.doc);
    finish_apply_document(json, subject, result)
}

struct PreparedApply {
    plan_path: PathBuf,
    plan: RootfsPlan,
    root: File,
    destination: File,
    filesystem: JobFilesystem,
    _rootfs_lock: state::FsLock,
    _destination_locks: Vec<crate::sys::PathLock>,
}

fn prepare_apply(
    plan_path: &Path,
    yes: bool,
    holder: &LockHolder,
) -> Result<PreparedApply, Failure> {
    require_root()?;
    let plan_path = guard::absolute_lexical(plan_path)?;
    let dir = Path::new(plan::PLAN_DIR);
    let name = plan::plan_name(&plan_path, dir)?;
    let root = File::open("/")?;
    let root_device = root.metadata()?.dev();
    let plan = plan::read_plan(&plan::open_plan_dir(dir, root_device, 0)?, name, 0)?;
    let location = crate::config::location()
        .map_err(|error| Refusal::new("config_untrusted", error.to_string()))?;
    let config = crate::config::read_optional(&location.path, location.owner)?;
    let now = SystemTime::now();
    apply::gate(
        &plan,
        crate::time::unix_seconds(now),
        config.as_ref(),
        yes,
        sys::effective_uid(),
    )?;
    update_guard::check(Path::new("/"), "nothing was changed")?;
    let to = &plan.payload.destination.path;
    let to_path = Path::new(OsStr::from_bytes(to));
    let destination = guard::open_absolute_no_follow(to_path)?;
    let env = Environment::load()?;
    let shares = destination_volume_shares(&env, to_path)?;
    plan::destination_share(to, &shares, destination.metadata()?.dev(), root_device)?;
    let volume = destination_volume(&env, to_path).ok_or_else(|| {
        Refusal::new(
            "destination_not_in_volume",
            "destination is not on a data volume",
        )
    })?;
    let fsid = volume
        .fsid
        .as_ref()
        .map_err(|error| std::io::Error::other(error.clone()))?;
    let filesystem = JobFilesystem {
        fsid: crate::resolve::fsid_text(fsid),
        mount: volume.primary.mount_point.as_os_str().as_bytes().to_vec(),
    };
    let rootfs_lock = state::lock_rootfs(Path::new(state::LOCK_DIR), sys::effective_uid(), holder)?;
    let destination_locks = state::lock_mutation_paths(
        Path::new(state::LOCK_DIR),
        &filesystem.fsid,
        &volume.primary.mount_point,
        &[to_path.join(plan.move_dir_name())],
        sys::effective_uid(),
        holder,
    )?;
    Ok(PreparedApply {
        plan_path,
        plan,
        root,
        destination,
        filesystem,
        _rootfs_lock: rootfs_lock,
        _destination_locks: destination_locks,
    })
}

struct ApplyRun {
    doc: apply_doc::ApplyDocument,
    stopped: Option<StopReason>,
}

fn apply_once(
    plan_path: &Path,
    profile: Profile,
    requested_by: Option<&str>,
    job_id: Option<&str>,
    job: Option<&RootfsApplyJob>,
    state_dir: &Path,
) -> Result<ApplyRun, Failure> {
    let uid = sys::effective_uid();
    let attempt = job_id.map(str::to_owned).unwrap_or_else(new_rootfs_attempt);
    let holder = LockHolder::new(job_id, "rootfs-apply");
    let prepared = prepare_apply(plan_path, true, &holder)?;
    // The saved report no longer describes `/` once anything moves.
    if let Err(error) = crate::rootfs::report_cache::forget(state_dir) {
        eprintln!("synoreclaim: warning: rootfs report cache not dropped: {error}");
    }
    let open = open_files_on(prepared.root.metadata()?.dev(), "nothing was changed")?;
    let before = sys::filesystem_space(prepared.root.as_fd())?.available;
    let started_at = SystemTime::now();
    let mut pacer = unpaced(profile)?;
    let mut watch = update_guard::UpdateWatch::new(Path::new("/"), &mut pacer);
    let cancelled = || job.is_some_and(|job| job.requested() == Some(Request::Cancel));
    let mut tools = delegate::HostTools {
        owner: uid,
        proc_root: Path::new("/proc"),
        root_device: prepared.root.metadata()?.dev(),
        cancelled: Some(&cancelled),
    };
    let done_result = {
        let mut boundary = RootfsBoundary {
            inner: &mut watch,
            job,
        };
        let mut publish = |progress| {
            if let Some(job) = job {
                job.progress(progress);
            }
        };
        apply::execute_with_progress(
            &prepared.plan,
            &prepared.root,
            &prepared.destination,
            Box::new(move |device, inode| open.contains_key(&(device, inode))),
            &mut tools,
            &mut boundary,
            &mut publish,
        )
    };
    let update_marker = watch.marker;
    let after = sys::filesystem_space(prepared.root.as_fd())?.available;
    let done = match done_result {
        Ok(done) => done,
        Err(error) => {
            let (files, size_bytes, skipped_count, tools) = match &error {
                ExecuteError::Partial { done, .. } => (
                    done.moved_files,
                    done.moved_size,
                    done.skipped,
                    done.tools
                        .iter()
                        .map(|&(tool, outcome)| HistoryTool::new(tool, outcome))
                        .collect(),
                ),
                ExecuteError::Refused(_) | ExecuteError::Io(_) => (0, 0, 0, Vec::new()),
            };
            let move_dir = prepared.plan.move_dir();
            let mut manifest = move_dir.clone();
            manifest.extend_from_slice(b"/manifest.jsonl");
            let record = HistoryApply {
                mode: prepared.plan.payload.mode,
                plan_id: prepared.plan.payload.plan_id.clone(),
                requested_by: requested_by.map(str::to_string),
                started: rfc3339(started_at),
                finished: rfc3339(SystemTime::now()),
                move_dir: JsonPath::new(&move_dir),
                manifest: JsonPath::new(&manifest),
                moved: HistoryMoved { files, size_bytes },
                freed_bytes: signed_delta(before, after),
                completion: "failed".into(),
                skipped_count,
                tools,
            };
            if let Err(history_error) = history::record(state_dir, &attempt, uid, &record) {
                eprintln!("synoreclaim: warning: rootfs apply history not saved: {history_error}");
            }
            return Err(match error {
                ExecuteError::Refused(refusal) => Failure::from(refusal),
                ExecuteError::Io(error) | ExecuteError::Partial { source: error, .. } => {
                    Failure::from(error)
                }
            });
        }
    };
    let doc = apply_doc::build(
        ApplyInputs {
            plan: &prepared.plan,
            plan_file: prepared.plan_path.as_os_str().as_bytes(),
            done: &done,
            root_available_before: before,
            root_available_after: after,
            update_marker,
        },
        SystemTime::now(),
    );
    let mut doc = doc;
    doc.envelope.job_id = job_id.map(str::to_string);
    let record = HistoryApply {
        mode: prepared.plan.payload.mode,
        plan_id: prepared.plan.payload.plan_id.clone(),
        requested_by: requested_by.map(str::to_string),
        started: rfc3339(started_at),
        finished: rfc3339(SystemTime::now()),
        move_dir: doc.move_dir.clone(),
        manifest: doc.manifest.clone(),
        moved: HistoryMoved {
            files: done.moved_files,
            size_bytes: done.moved_size,
        },
        freed_bytes: doc.root.freed_bytes,
        completion: doc.envelope.completion.name().into(),
        skipped_count: done.skipped,
        tools: done
            .tools
            .iter()
            .map(|&(tool, outcome)| HistoryTool::new(tool, outcome))
            .collect(),
    };
    if let Err(error) = history::record(state_dir, &attempt, uid, &record) {
        doc.envelope.warnings.push(Notice::new(
            "history_not_saved",
            format!("could not save rootfs apply history: {error}"),
        ));
    }
    let stopped = done.stopped;
    if let Some(job) = job {
        job.progress_force(job.last_progress());
    }
    Ok(ApplyRun { doc, stopped })
}

fn new_rootfs_attempt() -> String {
    plan::new_plan_id(SystemTime::now(), std::process::id())
}

fn signed_delta(before: u64, after: u64) -> i64 {
    if after >= before {
        i64::try_from(after - before).unwrap_or(i64::MAX)
    } else {
        i64::try_from(before - after).map_or(i64::MIN, |delta| -delta)
    }
}

struct RootfsBoundary<'a> {
    inner: &'a mut dyn BatchBoundary,
    job: Option<&'a RootfsApplyJob>,
}

impl BatchBoundary for RootfsBoundary<'_> {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        if self
            .job
            .is_some_and(|job| job.requested() == Some(Request::Cancel))
        {
            return ControlFlow::Break(StopReason::Cancelled);
        }
        self.inner.after_batch()
    }
}

struct RootfsApplyJob {
    live: RefCell<Live>,
    saved: Cell<Option<Instant>>,
    last: RefCell<RootfsProgress>,
    warned: Cell<bool>,
}

impl RootfsApplyJob {
    fn new(live: Live) -> RootfsApplyJob {
        RootfsApplyJob {
            live: RefCell::new(live),
            saved: Cell::new(None),
            last: RefCell::new(RootfsProgress {
                phase: "moves".into(),
                ..RootfsProgress::default()
            }),
            warned: Cell::new(false),
        }
    }

    fn requested(&self) -> Option<Request> {
        self.live.borrow().requested()
    }

    fn progress(&self, progress: RootfsProgress) {
        *self.last.borrow_mut() = progress.clone();
        self.publish(progress, false);
    }

    fn last_progress(&self) -> RootfsProgress {
        self.last.borrow().clone()
    }

    fn progress_force(&self, progress: RootfsProgress) {
        *self.last.borrow_mut() = progress.clone();
        self.publish(progress, true);
    }

    fn publish(&self, progress: RootfsProgress, force: bool) {
        let now = Instant::now();
        if !force
            && self
                .saved
                .get()
                .is_some_and(|saved| now.duration_since(saved) < jobs::PROGRESS_INTERVAL)
        {
            return;
        }
        self.saved.set(Some(now));
        let mut live = self.live.borrow_mut();
        live.job.phase = progress.phase.clone();
        live.job.rootfs = Some(progress);
        if let Err(error) = live.save_progress()
            && !self.warned.replace(true)
        {
            eprintln!("synoreclaim: warning: rootfs apply progress not saved: {error}");
        }
    }

    fn finish(self, run: &ApplyRun) -> Result<(), Failure> {
        let mut live = self.live.into_inner();
        let doc = &run.doc;
        let exit_code = doc.envelope.completion.exit_code(sys::stop_signal());
        let errors = doc
            .envelope
            .errors
            .iter()
            .map(|error| error.code.to_string())
            .collect::<Vec<_>>();
        live.record_result(
            JobResult::new(doc.envelope.completion.name(), exit_code, &[], errors),
            (serde_json::to_string_pretty(doc)? + "\n").as_bytes(),
        )?;
        let (state, reason) = match doc.envelope.completion {
            Completion::Complete => (JobState::Completed, None),
            Completion::Partial => (JobState::Partial, None),
            Completion::Paused => (
                JobState::Partial,
                Some(match run.stopped {
                    Some(StopReason::Cancelled) => "cancelled",
                    Some(StopReason::Paused) => "paused",
                    Some(StopReason::RuntimeLimit) => "runtime_limit",
                    Some(StopReason::ResourceLimit) => "resource_limit",
                    Some(StopReason::DiskLimit) => "disk_limit",
                    None => "paused",
                }),
            ),
            Completion::Busy => (JobState::Failed, Some("busy")),
            _ => (JobState::Failed, Some("failed")),
        };
        live.finish(state, reason, unix_seconds(SystemTime::now()))?;
        Ok(())
    }

    fn fail(self, doc: &Envelope, exit_code: i32) -> Result<(), Failure> {
        let progress = self.last_progress();
        self.progress_force(progress);
        let mut live = self.live.into_inner();
        let codes = doc
            .errors
            .iter()
            .map(|error| error.code.to_string())
            .collect::<Vec<_>>();
        live.record_result(
            JobResult::new(doc.completion.name(), exit_code, &[], codes),
            (serde_json::to_string_pretty(doc)? + "\n").as_bytes(),
        )?;
        live.finish(
            JobState::Failed,
            Some("failed"),
            unix_seconds(SystemTime::now()),
        )?;
        Ok(())
    }
}

fn detach_rootfs_apply(
    plan_path: &Path,
    profile: Profile,
    requested_by: Option<&str>,
    launcher: Launcher,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let uid = sys::effective_uid();
    let store = Store::open(state_dir, uid)?;
    let id = new_rootfs_attempt();
    let holder = LockHolder::new(Some(&id), "rootfs-apply");
    let prepared = prepare_apply(plan_path, true, &holder)?;
    let plan_path = prepared.plan_path.clone();
    let filesystem = prepared.filesystem.clone();
    let plan_details = crate::progress::RootfsPlanDetails {
        mode: prepared.plan.payload.mode,
        plan_id: prepared.plan.payload.plan_id.clone(),
        move_dir: JsonPath::new(&prepared.plan.move_dir()),
    };
    drop(prepared);

    let now = SystemTime::now();
    let mut job = Job::rootfs_apply(
        &id,
        plan_path.as_os_str().as_bytes(),
        filesystem,
        profile,
        unix_seconds(now),
    );
    job.requested_by = requested_by.map(str::to_string);
    job.rootfs_plan = Some(plan_details);
    let jobs_dir = store.root().join("jobs");
    jobs::prune(&jobs_dir, jobs::FINISHED_KEPT)?;
    jobs::queue(&jobs_dir, uid, job)?;

    let mut run = vec![std::env::current_exe()?.into_os_string()];
    run.extend(["rootfs", "apply", "--yes", "--json"].map(std::ffi::OsString::from));
    run.push(format!("--profile={}", profile.name()).into());
    if let Some(user) = requested_by {
        run.push(format!("--requested-by={user}").into());
    }
    run.push(format!("--job-id={id}").into());
    let mut state = std::ffi::OsString::from("--state-dir=");
    state.push(store.root());
    run.push(state);
    run.push("--".into());
    run.push(plan_path.into());

    let jobs_dir = store.root().join("jobs");
    let job_dir = jobs_dir.join(&id);
    let mut doc = DetachDocument::new(Envelope::new("detach", now), launcher);
    doc.envelope.job_id = Some(id);
    if let Err(error) = super::hand_off(&mut doc, &job_dir, &run, launcher)? {
        let now = unix_seconds(SystemTime::now());
        Live::launch(&jobs_dir, uid, doc.envelope.job_id.as_deref().unwrap(), now)?.finish(
            JobState::Failed,
            Some("launch_failed"),
            now,
        )?;
        return Err(format!("could not launch rootfs apply job: {error}").into());
    }
    Ok(0)
}

fn run_queued_rootfs_apply(
    plan_path: &Path,
    json: bool,
    id: &str,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let uid = sys::effective_uid();
    let store = Store::open(state_dir, uid)?;
    let jobs_dir = store.root().join("jobs");
    let loaded = jobs::load(&jobs_dir, id)?;
    if loaded.kind != jobs::JobKind::RootfsApply {
        return Err(format!("job {id} is not a rootfs apply job").into());
    }
    let plan_path = guard::absolute_lexical(plan_path)?;
    if loaded.operands() != [plan_path.as_os_str().as_bytes().to_vec()] {
        return Err(format!("job {id} names a different rootfs plan").into());
    }
    let requested_by = loaded.requested_by.clone();
    let profile = loaded.limits.profile;
    let live = Live::launch(&jobs_dir, uid, id, unix_seconds(SystemTime::now()))?;
    let job = RootfsApplyJob::new(live);
    match apply_once(
        &plan_path,
        profile,
        requested_by.as_deref(),
        Some(id),
        Some(&job),
        store.root(),
    ) {
        Ok(run) => {
            job.finish(&run)?;
            let subject = plan_path.as_os_str().as_bytes();
            finish_apply_document(json, subject, Ok(run.doc))
        }
        Err(error) => {
            let mut doc = rootfs_failure_document(
                plan_path.as_os_str().as_bytes(),
                error.as_ref(),
                SystemTime::now(),
            );
            doc.job_id = Some(id.to_string());
            let exit = rootfs_failure_exit(error.as_ref());
            job.fail(&doc, exit)?;
            if json {
                emit(true, &doc, String::new)?;
                Ok(exit)
            } else {
                finish_rootfs_apply(json, plan_path.as_os_str().as_bytes(), Err(error))
            }
        }
    }
}

fn finish_apply_document(
    json: bool,
    subject: &[u8],
    result: Result<apply_doc::ApplyDocument, Failure>,
) -> Result<i32, Failure> {
    match result {
        Ok(doc) => {
            let exit_code = doc.envelope.completion.exit_code(sys::stop_signal());
            emit(json, &doc, || apply_doc::render(&doc))?;
            Ok(exit_code)
        }
        Err(error) => finish_rootfs_apply(json, subject, Err(error)),
    }
}

fn finish_rootfs_apply(
    json: bool,
    subject: &[u8],
    result: Result<i32, Failure>,
) -> Result<i32, Failure> {
    match result {
        Ok(code) => Ok(code),
        Err(error) if json => {
            let doc = rootfs_failure_document(subject, error.as_ref(), SystemTime::now());
            let code = rootfs_failure_exit(error.as_ref());
            emit(true, &doc, String::new)?;
            Ok(code)
        }
        Err(error) if rootfs_failure_exit(error.as_ref()) == 4 => {
            eprintln!("synoreclaim: {error}");
            Ok(4)
        }
        Err(error) => Err(error),
    }
}

fn rootfs_failure_exit(error: &(dyn std::error::Error + 'static)) -> i32 {
    if error.downcast_ref::<Refusal>().is_some()
        || matches!(
            error.downcast_ref::<state::StateError>(),
            Some(state::StateError::Busy { .. } | state::StateError::RootfsBusy { .. })
        )
    {
        4
    } else {
        1
    }
}

fn rootfs_failure_document(
    subject: &[u8],
    error: &(dyn std::error::Error + 'static),
    now: SystemTime,
) -> Envelope {
    let busy = matches!(
        error.downcast_ref::<state::StateError>(),
        Some(state::StateError::Busy { .. } | state::StateError::RootfsBusy { .. })
    );
    let code = if busy {
        "busy"
    } else if let Some(refusal) = error.downcast_ref::<Refusal>() {
        refusal.code
    } else {
        "io_error"
    };
    let mut doc = Envelope::new("rootfs-apply", now);
    doc.completion = if busy {
        Completion::Busy
    } else {
        Completion::Failed
    };
    let target = if matches!(
        error.downcast_ref::<state::StateError>(),
        Some(state::StateError::RootfsBusy { .. })
    ) {
        b"/".as_slice()
    } else {
        subject
    };
    let mut target_error = TargetError::new(JsonPath::new(target), code, error.to_string());
    target_error.holder = match error.downcast_ref::<state::StateError>() {
        Some(state::StateError::Busy { holder, .. } | state::StateError::RootfsBusy { holder }) => {
            holder.clone().map(Box::new)
        }
        _ => None,
    };
    doc.errors.push(target_error);
    doc
}

/// `rootfs history --json`: per-apply records from the private state dir.
pub fn rootfs_history(json: bool, state_dir: &Path) -> Result<i32, Failure> {
    let applies = history::load(state_dir, sys::effective_uid())?;
    let doc = history::Document::new(applies, SystemTime::now());
    emit(json, &doc, || {
        if doc.applies.is_empty() {
            "No rootfs applies recorded.\n".into()
        } else {
            doc.applies
                .iter()
                .map(|apply| {
                    format!(
                        "{}  {}  {} files / {} bytes  {}\n",
                        apply.finished,
                        apply.plan_id,
                        apply.moved.files,
                        apply.moved.size_bytes,
                        apply.completion
                    )
                })
                .collect()
        }
    })?;
    Ok(0)
}

/// The files other processes hold open on `device`; refused when some
/// process could not be read, since a held file must never be moved.
fn open_files_on(
    device: u64,
    consequence: &str,
) -> Result<std::collections::BTreeMap<(u64, u64), Vec<u8>>, Failure> {
    let (open, unreadable) =
        crate::open_files::open_inodes(Path::new("/proc"), |dev| dev == device);
    if unreadable > 0 {
        return Err(Refusal::new(
            "processes_unreadable",
            format!(
                "{unreadable} processes could not be read, so open files are unknown; {consequence}"
            ),
        )
        .into());
    }
    Ok(open)
}

/// The shared-folder roots of the volume `to` lies on, as DSM registers
/// them on Btrfs and ext4 alike; none when it is on no volume.
fn destination_volume_shares(env: &Environment, to: &Path) -> Result<Vec<Vec<u8>>, Failure> {
    let Some(volume) = destination_volume(env, to) else {
        return Ok(Vec::new());
    };
    let mount = &volume.primary.mount_point;
    Ok(share_paths(mount, crate::dsm_shares::inventory(mount)?))
}

/// The root path of each share `names` registers below `mount`.
fn share_paths(mount: &Path, names: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    names
        .into_iter()
        .map(|name| {
            let mut path = mount.as_os_str().as_bytes().to_vec();
            path.push(b'/');
            path.extend_from_slice(&name);
            path
        })
        .collect()
}

/// The subvolume that holds the quota of `share` on `volume`: a Btrfs
/// share root is its own subvolume. None on ext4, which has no share quota.
fn share_quota_subvolume(volume: &Volume, share: &[u8]) -> Result<Option<u64>, Failure> {
    if volume.primary.fstype != "btrfs" {
        return Ok(None);
    }
    let root = guard::open_absolute_no_follow(Path::new(OsStr::from_bytes(share)))?;
    if root.metadata()?.ino() != FIRST_FREE_OBJECTID {
        return Ok(None);
    }
    Ok(Some(sys::subvolume_id(root.as_fd())?))
}

fn destination_volume<'a>(env: &'a Environment, to: &Path) -> Option<&'a Volume> {
    env.volumes
        .iter()
        .filter(|volume| to.starts_with(&volume.primary.mount_point))
        .max_by_key(|volume| volume.primary.mount_point.as_os_str().len())
}

fn destination_share_quota(
    mount: &Path,
    share_subvolume_id: u64,
    boundary: &mut dyn BatchBoundary,
) -> Result<Option<u64>, Failure> {
    let opened = OpenTarget::open(mount)?;
    let mut source = LiveSource::new(opened.file, 4096);
    let qgroups = crate::btrfs::qgroup::read_qgroups(&mut source, boundary)?;
    if !qgroups.enabled() || !qgroups.has_share_quota(share_subvolume_id) {
        return Ok(None);
    }
    if !qgroups.consistent() {
        return Err(Refusal::new(
            "destination_quota_unavailable",
            "the shared folder quota could not be checked; no plan was written",
        )
        .into());
    }
    let quota = qgroups.share_quota(share_subvolume_id).ok_or_else(|| {
        Refusal::new(
            "destination_quota_unavailable",
            "the shared folder quota could not be checked; no plan was written",
        )
    })?;
    Ok(Some(quota.available_bytes()))
}

/// The live `/`, `/proc`, and every other mount point, which the walk
/// never enters.
fn live_sources(mounts: &[crate::mounts::Mount]) -> Result<Sources<'static>, Failure> {
    let root = crate::space::root_mount(mounts).ok_or("no mount of / found")?;
    Ok(Sources {
        root: Path::new("/"),
        mount_points: other_mount_points(mounts),
        source: root.source.display().to_string(),
        fstype: root.fstype.clone(),
        proc_root: Path::new("/proc"),
    })
}

/// Applies `profile` and paces the walk by it; a signal stops it.
fn pacer(profile: Profile) -> Result<Pacer<SystemClock>, Failure> {
    paced(profile, profile.settings().duty_percent)
}

/// Applies `profile`'s nice value and I/O class but never sleeps: apply
/// moves at most a few thousand small files, so it does not pace them. A
/// signal stops it between batches.
fn unpaced(profile: Profile) -> Result<Pacer<SystemClock>, Failure> {
    paced(profile, 100)
}

fn paced(profile: Profile, duty_percent: u32) -> Result<Pacer<SystemClock>, Failure> {
    resource::apply_profile(profile);
    let stop = sys::install_stop_handlers()?;
    Ok(Pacer::new(duty_percent, SystemClock::default(), None).with_stop_flag(stop))
}

/// Every mount point but `/`: the walk enters none of them.
fn other_mount_points(mounts: &[crate::mounts::Mount]) -> BTreeSet<Vec<u8>> {
    mounts
        .iter()
        .map(|mount| mount.mount_point.as_os_str().as_bytes().to_vec())
        .filter(|point| point.as_slice() != b"/")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_volume(mount_point: &str, fstype: &str) -> Volume {
        Volume {
            primary: crate::mounts::Mount {
                device: "0:1".into(),
                root: b"/".to_vec(),
                mount_point: PathBuf::from(mount_point),
                fstype: fstype.into(),
                source: PathBuf::from("/dev/md2"),
                options: Vec::new(),
            },
            fsid: Ok([0; 16]),
        }
    }

    #[test]
    fn the_destination_volume_is_found_on_btrfs_and_ext4() {
        let env = Environment {
            mounts: Vec::new(),
            volumes: vec![
                data_volume("/volume1", "btrfs"),
                data_volume("/volume2", "ext4"),
            ],
        };

        let mount = |to: &str| {
            destination_volume(&env, Path::new(to)).map(|volume| volume.primary.mount_point.clone())
        };

        assert_eq!(mount("/volume1/share/a"), Some(PathBuf::from("/volume1")));
        assert_eq!(mount("/volume2/share/a"), Some(PathBuf::from("/volume2")));
        assert_eq!(mount("/volume20/share/a"), None);
    }

    #[test]
    fn a_folder_in_a_registered_share_is_a_destination() {
        let shares = share_paths(
            Path::new("/volume1"),
            vec![b"archive".to_vec(), b"photo".to_vec()],
        );

        assert_eq!(
            plan::destination_share(b"/volume1/photo/moved", &shares, 2, 1).unwrap(),
            b"/volume1/photo"
        );
        assert_eq!(
            plan::destination_share(b"/volume1/plain/moved", &shares, 2, 1)
                .unwrap_err()
                .code,
            "destination_not_in_share"
        );
    }

    #[test]
    fn an_ext4_share_has_no_quota_subvolume() {
        let volume = data_volume("/volume1", "ext4");

        assert_eq!(
            share_quota_subvolume(&volume, b"/volume1/archive").unwrap(),
            None
        );
    }

    #[test]
    fn a_rootfs_cancel_request_stops_at_the_next_apply_boundary() {
        struct Continue;
        impl BatchBoundary for Continue {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                ControlFlow::Continue(())
            }
        }

        let jobs_dir = crate::spill::testing::scratch("rootfs-cancel-boundary").join("jobs");
        let uid = sys::effective_uid();
        let live = Live::start(
            &jobs_dir,
            uid,
            Job::rootfs_apply(
                "rootfs-job",
                b"/var/lib/synoreclaim/rootfs-plans/rootfs-1-2.json",
                JobFilesystem {
                    fsid: "f".repeat(32),
                    mount: b"/volume1".to_vec(),
                },
                Profile::Low,
                100,
            ),
        )
        .unwrap();
        let job = RootfsApplyJob::new(live);
        jobs::request(&jobs_dir, uid, "rootfs-job", Request::Cancel, 101).unwrap();
        let mut inner = Continue;
        let mut boundary = RootfsBoundary {
            inner: &mut inner,
            job: Some(&job),
        };

        assert_eq!(
            boundary.after_batch(),
            ControlFlow::Break(StopReason::Cancelled)
        );
    }

    #[test]
    fn busy_rootfs_apply_documents_include_the_recorded_holder() {
        let holder = LockHolder::new(Some("scan-1"), "scan");
        let error = state::StateError::RootfsBusy {
            holder: Some(holder.clone()),
        };

        let doc = rootfs_failure_document(b"/plan.json", &error, SystemTime::UNIX_EPOCH);
        let value = serde_json::to_value(doc).unwrap();

        assert_eq!(value["completion"], "busy");
        assert_eq!(value["errors"][0]["path"], "/");
        assert_eq!(value["errors"][0]["code"], "busy");
        assert_eq!(value["errors"][0]["holder"]["job_id"], "scan-1");
        assert_eq!(value["errors"][0]["holder"]["command"], "scan");
    }

    #[test]
    fn a_failed_rootfs_job_saves_its_final_progress() {
        let jobs_dir = crate::spill::testing::scratch("rootfs-failure-progress").join("jobs");
        let uid = sys::effective_uid();
        let live = Live::start(
            &jobs_dir,
            uid,
            Job::rootfs_apply(
                "rootfs-failure",
                b"/var/lib/synoreclaim/rootfs-plans/rootfs-1-2.json",
                JobFilesystem {
                    fsid: "f".repeat(32),
                    mount: b"/volume1".to_vec(),
                },
                Profile::Low,
                100,
            ),
        )
        .unwrap();
        let job = RootfsApplyJob::new(live);
        let document = rootfs_failure_document(
            b"/plan.json",
            &std::io::Error::other("executor failed"),
            SystemTime::UNIX_EPOCH,
        );

        job.fail(&document, 1).unwrap();

        let failed = jobs::load(&jobs_dir, "rootfs-failure").unwrap();
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(
            failed.rootfs,
            Some(RootfsProgress {
                phase: "moves".into(),
                ..RootfsProgress::default()
            })
        );
    }
}
