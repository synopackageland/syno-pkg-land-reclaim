//! Live execution of `volumes`, `scan`, `overview`, `top` and `inspect`.

use crate::btrfs::keys::FIRST_FREE_OBJECTID;
use crate::btrfs::live::{LiveSource, OpenTarget};
use crate::btrfs::source::{BlockGroupSpace, FsSpace, SpaceWindow};
use crate::btrfs::subvolume::SubvolumeSet;
use crate::btrfs::walk::{BatchBoundary, StopReason, WalkError, WalkStats};
use crate::cache::{self, Budget, Cache, Reuse, Sealing};
use crate::cli::{
    AnswerArgs, CacheAction, CacheArgs, ConfigAction, LaunchArgs, Launcher, OutputArgs, RankBy,
    ResumeLimits, ScanLimits, ShareBy, TargetArgs,
};
use crate::document::*;
use crate::dsm::{self, Depth};
use crate::guard::{self, CleanupError, Refusal};
use crate::history::{self, ScanRecord};
use crate::jobs::{self, Job, JobLimits, JobState, Live, Poll, Request};
use crate::model::accounting::Blockers;
use crate::model::tree::{Marks, NodeFlags};
use crate::output::{JsonMetrics, JsonPath, Notice, TargetError};
use crate::overview::{OverviewData, PartKind};
use crate::placement;
use crate::progress::{EntryCount, JobBasis, JobProgress, Progress, UnitRestartProgress};
use crate::query::{self, KindFilter, Measure, TopQuery};
use crate::report::{self, FORMAT, Header, RootInfo, ScanStats};
use crate::resolve::{self, Environment, fsid_text};
use crate::resource::{self, Pacer, SchedulingReport, SystemClock};
use crate::scan::{
    NoCache, RootRequest, ScanControl, ScanEvent, ScanOptions, UnitCache, scan_filesystem,
};
use crate::snapshots::answer::{
    Listing, MANAGE_IN, Rescan, Selection, ShareJson, share_answer, simulate_set,
};
use crate::snapshots::{DEFAULT_NEWEST, DeletionAnswer, DeletionRequest, SnapshotScan};
use crate::space::{self, DiskWatch, SystemPartition};
use crate::spill::Spill;
use crate::state::{self, Store};
use crate::summary::ServiceSummary;
use crate::sys;
use crate::target::{
    Covered, FilesystemTargets, Resolved, Scope, TargetArg, operand, parse_operand, partition,
};
use crate::time::{job_id, rfc3339, unix_seconds};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::ops::ControlFlow;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

mod cleanup;
mod deep_attribution;
mod resume;
mod rootfs;
mod snapshot_analysis;
mod why_not_freed;
pub use cleanup::{
    cleanup_apply, cleanup_delete, cleanup_plan, cleanup_select, moved_since, valid_plan_id,
    valid_selection_id,
};
pub use rootfs::{rootfs_apply, rootfs_history, rootfs_plan, rootfs_report};
pub use why_not_freed::{WhyArgs, why_not_freed, why_not_freed_document};

type Failure = Box<dyn std::error::Error>;

/// How often a scan samples the state filesystem's free space.
const DISK_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Everything a scan needs besides its targets.
pub struct ScanContext {
    pub job_id: String,
    pub store: Store,
    pub uid: u32,
    pub limits: ScanLimits,
    pub scheduling: SchedulingReport,
    pub memory_budget: u64,
    /// Space scans leave available on the state filesystem.
    pub reserve: u64,
    pub stop: &'static AtomicBool,
    pub quiet: bool,
    /// Whether to print progress lines: not quiet, not JSON, and
    /// stderr is a terminal (SPEC 22.7).
    pub progress: bool,
    /// Identity of the state directory, excluded from rankings.
    pub state_identity: Option<([u8; 16], u64, u64)>,
    /// Where the state directory's writes land.
    pub state_locations: Vec<placement::Identity>,
    pub reuse: Reuse,
    /// The job this scan runs, for `scan` and `resume`.
    pub job: Option<RefCell<Live>>,
    /// Snapshot roots deleted with the selection (SPEC 5.14 mixed sets).
    pub deletion: Option<crate::snapshots::DeletionRequest>,
}

impl ScanContext {
    pub fn new(limits: ScanLimits, state_dir: &Path, output: OutputArgs) -> Result<Self, Failure> {
        let quiet = output.quiet;
        let stop = sys::install_stop_handlers()?;
        let ram = resource::physical_ram().ok_or("cannot read physical RAM from /proc/meminfo")?;
        let memory_budget = limits.max_memory.resolve(ram)?;
        let scheduling = resource::apply_profile(limits.profile);
        if !quiet {
            for w in &scheduling.warnings {
                eprintln!("synoreclaim: warning: {w}");
            }
        }
        let uid = sys::effective_uid();
        let store = Store::open(state_dir, uid)?;
        Ok(ScanContext {
            job_id: job_id(SystemTime::now(), std::process::id()),
            state_identity: resolve::identity(store.root()).ok(),
            state_locations: placement::locations(store.root()),
            reserve: space::reserve(limits.min_free, space::system_partition_total(store.root())),
            store,
            uid,
            limits,
            scheduling,
            memory_budget,
            stop,
            quiet,
            progress: !quiet && !output.json && sys::stderr_is_terminal(),
            reuse: Reuse::of_this_platform(),
            job: None,
            deletion: None,
        })
    }

    /// The unit store of a scan of `fsid`: its cache in the state
    /// directory, unless `--no-cache`.
    fn unit_store(&self, fsid: &str) -> Result<Option<Sealing>, WalkError> {
        if let Some(job) = &self.job {
            let root = self
                .store
                .root()
                .join("jobs")
                .join(&self.job_id)
                .join("checkpoint");
            if job.borrow().job.resumes > 0 && !root.is_dir() {
                return Err(WalkError::Checkpoint(
                    "this job has no durable checkpoint".into(),
                ));
            }
            if job.borrow().job.resumes > 0 {
                let saved = root.join(fsid);
                let progressed = job
                    .borrow()
                    .job
                    .progress
                    .as_ref()
                    .is_some_and(|p| phase_rank(&p.phase) > 0);
                if progressed
                    && !saved.join("manifest").is_file()
                    && !saved.join("namespace.json").is_file()
                {
                    return Err(io_checkpoint("the saved scan position is missing".into()));
                }
            }
            state::private_dir(&root, self.uid).map_err(|e| io_checkpoint(e.to_string()))?;
            if !Cache::checkpoint_manifest_valid(&root, fsid) {
                return Err(io_checkpoint("damaged completed-unit manifest".into()));
            }
            let cache = Cache::open(
                &root,
                fsid,
                self.uid,
                Reuse {
                    writable_max_age: u64::MAX,
                    trust_writable: true,
                },
                Budget {
                    max_bytes: u64::MAX,
                    min_free: self.reserve,
                },
            )
            .map_err(|e| io_checkpoint(e.to_string()))?;
            return Ok(Some(Sealing::checkpoint(cache)));
        }
        if self.limits.no_cache {
            return Ok(None);
        }
        let budget = Budget {
            max_bytes: self.limits.max_cache,
            min_free: self.reserve,
        };
        let root = self.store.root().join("cache");
        let cache = Cache::open(&root, fsid, self.uid, self.reuse, budget)
            .map_err(|e| WalkError::Spill(std::io::Error::other(e)))?;
        Ok(Some(Sealing::new(cache)))
    }

    pub fn resources(&self) -> Resources {
        Resources {
            scheduling: self.scheduling.clone(),
            memory_budget_bytes: self.memory_budget,
            max_runtime_seconds: self.limits.max_runtime.map(|d| d.as_secs()),
            peak_rss_bytes: resource::peak_resident_bytes(),
        }
    }

    fn marks(&self, env: &Environment, fsid: &[u8; 16]) -> Marks {
        let mut marks = env.encrypted_marks(fsid);
        if let Some((id, tree, ino)) = self.state_identity
            && &id == fsid
        {
            marks.own_state.insert((tree, ino));
        }
        marks
    }

    /// Warns, on stderr and in `fs`, when the state directory lies on
    /// `group`'s filesystem.
    fn warn_state_placement(&self, group: &FilesystemTargets, fs: &mut FilesystemScan) {
        let placement = placement::classify(&self.state_locations, &group.fsid, |id| {
            placement::selects(&group.targets, id)
        });
        if let Some(notice) = placement.notice() {
            if !self.quiet {
                eprintln!("synoreclaim: warning: {}", notice.message);
            }
            fs.warnings.push(notice);
        }
    }
}

fn phase_rank(phase: &str) -> u8 {
    match phase {
        "discover" => 0,
        "scan" => 1,
        "resolve-refs" => 2,
        "aggregate" => 3,
        "snapshots" => 4,
        "publish" => 5,
        _ => 0,
    }
}
fn phase_name(phase: &str) -> &'static str {
    match phase {
        "scan" => "scan",
        "resolve-refs" => "resolve-refs",
        "aggregate" => "aggregate",
        "snapshots" => "snapshots",
        "publish" => "publish",
        _ => "discover",
    }
}

fn io_checkpoint(message: String) -> WalkError {
    WalkError::Checkpoint(message)
}

/// Pacing, job requests and phase, plus progress lines on stderr at most
/// once a second.
struct Control<'a> {
    pacer: Pacer<SystemClock>,
    disk: DiskWatch,
    job: Option<&'a RefCell<Live>>,
    poll: Poll,
    /// Rate-limits job-manifest progress writes, which also write a file.
    progress_poll: Poll,
    quiet: bool,
    /// Whether progress lines are printed.
    progress: bool,
    fsid: String,
    started: Instant,
    elapsed_before: u64,
    progress_floor: Option<JobProgress>,
    /// Actual metadata I/O in this attempt; checkpoint replay is excluded.
    new_work: WalkStats,
    restart: Option<UnitRestartProgress>,
    last_line: Instant,
    phase: &'static str,
    tree: u64,
    /// The subvolume being read, with its name; `None` between
    /// subvolumes and in the phases that read none.
    subvolume: Option<(u64, JsonPath)>,
    /// Whether the scan reads whole volumes, so the writable subvolumes
    /// bound its units.
    volume: bool,
    writable: Option<u64>,
    units: u64,
    entries: EntryCount,
    items: u64,
    /// Payload bytes of the metadata items read so far.
    bytes: u64,
    /// The last completed scan of the same scope, for the ETA and the
    /// progress percentage.
    previous: Option<ScanRecord>,
    deletion: Option<crate::snapshots::DeletionRequest>,
}

impl Control<'_> {
    /// What another terminal asked the job to do, checked at most once
    /// a poll interval.
    fn requested(&mut self) -> Option<Request> {
        let job = self.job?;
        if !self.poll.due(Instant::now()) {
            return None;
        }
        self.follow_profile(job);
        job.borrow().requested()
    }

    /// Goes on in the mode another terminal switched the job to, without
    /// stopping: only the priority of the rest of the walk changes.
    fn follow_profile(&self, job: &RefCell<Live>) {
        let warnings = match job.borrow_mut().follow_profile() {
            Ok(Some(applied)) => applied.warnings,
            Ok(None) => return,
            Err(e) => vec![format!("mode change not recorded: {e}")],
        };
        if !self.quiet {
            for w in warnings {
                eprintln!("synoreclaim: warning: {w}");
            }
        }
    }

    /// Records the phase in the job manifest when it changes.
    fn phase(&mut self, phase: &'static str) {
        if self
            .progress_floor
            .as_ref()
            .is_some_and(|p| phase_rank(&p.phase) > phase_rank(phase))
        {
            return;
        }
        self.phase = phase;
        let Some(job) = self.job else {
            return;
        };
        let mut live = job.borrow_mut();
        if live.job.phase == phase {
            return;
        }
        live.job.phase = phase.to_string();
        if let Some(progress) = live.job.progress.as_mut() {
            progress.phase = phase.to_string();
        }
        if let Err(e) = live.save()
            && !self.quiet
        {
            eprintln!("synoreclaim: warning: job progress not saved: {e}");
        }
    }

    fn work_items(&self) -> u64 {
        self.progress_floor
            .as_ref()
            .map_or(self.items, |p| p.items.saturating_add(self.new_work.items))
    }

    fn work_bytes(&self) -> u64 {
        self.progress_floor.as_ref().map_or(self.bytes, |p| {
            p.bytes_read.saturating_add(self.new_work.item_bytes)
        })
    }

    fn progress(&mut self, force: bool) {
        self.save_progress(force);
        if !self.progress || (!force && self.last_line.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.last_line = Instant::now();
        let line = Progress {
            fsid: self.fsid.clone(),
            phase: self.phase.to_string(),
            units: self.units,
            writable: self.writable,
            entries: self
                .entries
                .total()
                .max(self.progress_floor.as_ref().map_or(0, |p| p.entries)),
            items: self.work_items(),
            elapsed: Duration::from_secs(self.elapsed_before)
                .saturating_add(self.started.elapsed()),
            rss: resource::resident_bytes(),
            previous: self
                .previous
                .as_ref()
                .map(|r| Duration::from_millis(r.eta_basis().0)),
        };
        eprintln!("{}", line.line());
    }

    /// Writes the running counts into the job manifest at most every two
    /// seconds (SPEC 22.1), so another process can follow the scan through
    /// `jobs` without measurable scan overhead.
    fn save_progress(&mut self, force: bool) {
        let Some(job) = self.job else {
            return;
        };
        if !force && !self.progress_poll.due(Instant::now()) {
            return;
        }
        let progress = JobProgress {
            phase: self.phase.to_string(),
            step: None,
            units_done: self
                .units
                .max(self.progress_floor.as_ref().map_or(0, |p| p.units_done)),
            units_total: self.writable,
            entries: self
                .entries
                .total()
                .max(self.progress_floor.as_ref().map_or(0, |p| p.entries)),
            items: self.work_items(),
            bytes_read: self.work_bytes(),
            elapsed_s: self
                .elapsed_before
                .saturating_add(self.started.elapsed().as_secs()),
            rss_bytes: resource::resident_bytes(),
            subvolume_id: self.subvolume.as_ref().map(|(id, _)| *id),
            current_subvolume: self.subvolume.as_ref().map(|(_, name)| name.clone()),
            restart: self.restart.clone(),
            basis: self.basis(),
            updated: rfc3339(SystemTime::now()),
        };
        job.borrow_mut().job.progress = Some(progress);
        if let Err(e) = job.borrow_mut().save_progress()
            && !self.quiet
        {
            eprintln!("synoreclaim: warning: job progress not saved: {e}");
        }
    }

    /// The last completed scan of the same target, as the percentage and
    /// ETA basis; `None` when none is recorded. The history keeps the last
    /// full read (not a reuse-heavy run), which is what a percentage has
    /// to compare against.
    fn basis(&self) -> Option<JobBasis> {
        if self
            .job
            .is_some_and(|j| j.borrow().job.limits.snapshot_report.is_some())
        {
            return None;
        }
        self.previous.as_ref().map(|r| {
            let (elapsed_ms, entries) = r.eta_basis();
            JobBasis {
                entries,
                elapsed_s: elapsed_ms / 1000,
                items: None,
            }
        })
    }

    /// A one-off progress line, such as a phase starting.
    fn note(&self, what: std::fmt::Arguments<'_>) {
        if self.progress {
            eprintln!(
                "synoreclaim: scan {}: {what}",
                &self.fsid[..8.min(self.fsid.len())]
            );
        }
    }
}

impl BatchBoundary for Control<'_> {
    fn phase(&mut self, phase: &'static str) {
        self.phase(phase);
    }

    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        self.progress(false);
        if let Some(request) = self.requested() {
            return ControlFlow::Break(stop_for(request));
        }
        self.disk.check(Instant::now())?;
        self.pacer.after_batch()
    }

    fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
        self.pacer.reserve(bytes)
    }

    fn restored(&mut self, batch: &WalkStats) {
        self.items += batch.items;
        self.bytes += batch.item_bytes;
        self.entries.took(batch.dir_index_items);
    }

    fn took(&mut self, batch: &WalkStats) {
        self.new_work.add(batch);
        if let Some(restart) = self.restart.as_mut() {
            restart.entries += batch.dir_index_items;
            restart.items += batch.items;
            restart.bytes_read += batch.item_bytes;
        }
        self.items += batch.items;
        self.bytes += batch.item_bytes;
        self.entries.took(batch.dir_index_items);
    }
}

impl ScanControl for Control<'_> {
    fn boundary(&mut self) -> &mut dyn BatchBoundary {
        self
    }

    fn deletion(&mut self) -> Option<crate::snapshots::DeletionRequest> {
        self.deletion.take()
    }

    fn event(&mut self, event: ScanEvent<'_>) {
        match event {
            ScanEvent::Discovered {
                subvolumes,
                writable,
            } => {
                self.phase("scan");
                self.subvolume = None;
                self.writable = self.volume.then_some(writable as u64);
                self.note(format_args!("discovered {subvolumes} subvolumes"));
            }
            ScanEvent::TreeStarted { tree, name } => {
                self.restart = self
                    .progress_floor
                    .as_ref()
                    .and_then(|p| p.restart.clone())
                    .filter(|restart| restart.subvolume_id == tree);
                self.tree = tree;
                self.subvolume = (!name.is_empty()).then(|| (tree, JsonPath::new(name)));
                self.entries.tree_started();
            }
            ScanEvent::UnitReused { stats, .. } => {
                self.units += 1;
                self.entries.tree_read(stats.dir_index_items);
                self.items += stats.walk.items;
                self.bytes += stats.walk.item_bytes;
            }
            ScanEvent::UnitRestarted { tree } => {
                self.restart = Some(UnitRestartProgress {
                    subvolume_id: tree,
                    ..UnitRestartProgress::default()
                });
                if let Some(floor) = self.progress_floor.as_mut() {
                    floor.phase = "scan".into();
                }
                self.phase("scan");
                self.progress(true);
            }
            ScanEvent::TreeRead { stats, .. } => {
                self.restart = None;
                self.units += 1;
                self.entries.tree_read(stats.dir_index_items);
            }
            ScanEvent::Resolving { extents } => {
                self.restart = None;
                self.phase("resolve-refs");
                self.subvolume = None;
                self.tree = crate::btrfs::backref::EXTENT_TREE_OBJECTID;
                self.note(format_args!("resolving references of {extents} extents"));
            }
            ScanEvent::Aggregating { .. } => {
                self.phase("aggregate");
                self.subvolume = None;
                self.progress(true);
            }
            ScanEvent::AnalyzingSnapshots { shares } => {
                self.phase("snapshots");
                self.subvolume = None;
                self.note(format_args!(
                    "reading the snapshots of {shares} subvolume(s)"
                ));
            }
        }
    }
}

fn scope_target(t: &Resolved) -> ScopeTarget {
    ScopeTarget {
        path: JsonPath::new(t.path.as_os_str().as_bytes()),
        scope: t.scope,
        tree: (t.filesystem == crate::filesystem::FilesystemKind::Btrfs).then_some(t.tree),
        inode_device: (t.filesystem == crate::filesystem::FilesystemKind::Ext4).then_some(t.tree),
        ino: t.ino,
    }
}

/// How a scan stops for a request from another terminal.
fn stop_for(request: Request) -> StopReason {
    match request {
        Request::Pause => StopReason::Paused,
        Request::Cancel => StopReason::Cancelled,
    }
}

fn stop_completion(reason: StopReason) -> (Completion, &'static str, &'static str) {
    match reason {
        StopReason::Cancelled => (
            Completion::Cancelled,
            "cancelled",
            "cancel requested; no report was published",
        ),
        StopReason::Paused => (
            Completion::Paused,
            "paused",
            "stop requested; no report was published",
        ),
        StopReason::RuntimeLimit => (
            Completion::RuntimeLimit,
            "runtime_limit",
            "--max-runtime reached; no report was published",
        ),
        StopReason::ResourceLimit => (
            Completion::ResourceLimit,
            "resource_limit",
            "memory budget reached; scan a narrower scope or raise --max-memory",
        ),
        StopReason::DiskLimit => (
            Completion::ResourceLimit,
            "disk_limit",
            "state or spill filesystem below its free-space reserve (--min-free, raised to 3% on the system partition); free space there or use a --state-dir elsewhere, then resume",
        ),
    }
}

/// Scans each filesystem group in fsid order, one at a time.
pub fn scan_groups(
    ctx: &ScanContext,
    env: &Environment,
    groups: Vec<FilesystemTargets>,
    covered: Vec<Covered>,
) -> Vec<FilesystemScan> {
    let mut results = Vec::new();
    let mut stopped: Option<StopReason> = None;
    for group in groups {
        let fsid = fsid_text(&group.fsid);
        let targets: Vec<ScopeTarget> = group.targets.iter().map(scope_target).collect();
        let mut fs = FilesystemScan::new(fsid.clone(), group.mount.as_os_str().as_bytes(), targets);
        fs.filesystem = group.targets[0].filesystem;
        fs.capabilities = fs.filesystem.capabilities();
        fs.covered = covered
            .iter()
            .filter(|c| c.target.fsid == group.fsid)
            .map(|c| CoveredTarget {
                path: JsonPath::new(c.target.path.as_os_str().as_bytes()),
                covered_by: JsonPath::new(c.by.as_os_str().as_bytes()),
            })
            .collect();
        if let Some(reason) = stopped {
            let (completion, code, _) = stop_completion(reason);
            fs.completion = completion;
            fs.warnings
                .push(Notice::new(code, "not started: the job stopped earlier"));
            results.push(fs);
            continue;
        }
        ctx.warn_state_placement(&group, &mut fs);
        match scan_one(ctx, env, &group, &mut fs) {
            Ok(()) => {
                fs.warnings
                    .extend(placement::writes_ignored(&fs.state_dir_writes));
                fs.warnings.extend(placement::change_hint(
                    &ctx.state_locations,
                    &group.fsid,
                    &fs.change_reasons,
                ));
            }
            Err(WalkError::Stopped(reason)) => {
                let (completion, code, message) = stop_completion(reason);
                fs.completion = completion;
                fs.warnings.push(Notice::new(code, message));
                stopped = Some(reason);
            }
            Err(e) => {
                fs.completion = Completion::Failed;
                fs.errors.push(TargetError::new(
                    fs.mount.clone(),
                    if matches!(e, WalkError::Checkpoint(_)) {
                        "checkpoint_unusable"
                    } else {
                        "metadata_error"
                    },
                    e.to_string(),
                ));
            }
        }
        results.push(fs);
    }
    results
}

/// `stats` with the time and memory the scan took since `started`.
fn measured(mut stats: ScanStats, started: Instant) -> ScanStats {
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    stats.peak_rss_bytes = resource::peak_resident_bytes();
    stats
}

/// The writer lock of filesystem `fsid`, mounted at `mount`; otherwise
/// the completion and error to report.
fn writer_lock(
    fsid: &str,
    uid: u32,
    mount: &JsonPath,
    holder: &state::LockHolder,
) -> Result<state::FsLock, Box<(Completion, TargetError)>> {
    state::lock_filesystem(Path::new(state::LOCK_DIR), fsid, uid, holder).map_err(|e| {
        let mut error = TargetError::new(mount.clone(), "lock_error", e.to_string());
        let completion = match e {
            state::StateError::Busy { holder, .. } => {
                error.code = "busy";
                error.holder = holder.map(Box::new);
                Completion::Busy
            }
            _ => Completion::Failed,
        };
        Box::new((completion, error))
    })
}

fn scan_one(
    ctx: &ScanContext,
    env: &Environment,
    group: &FilesystemTargets,
    fs: &mut FilesystemScan,
) -> Result<(), WalkError> {
    let fsid = fs.fsid.clone();
    let holder = state::LockHolder::new(Some(&ctx.job_id), "scan");
    let _lock = match writer_lock(&fsid, ctx.uid, &fs.mount, &holder).map_err(|e| *e) {
        Ok(lock) => lock,
        Err((completion, error)) => {
            fs.completion = completion;
            fs.errors.push(error);
            return Ok(());
        }
    };
    let opened = OpenTarget::open(&group.mount).map_err(|e| WalkError::Source(e.into()))?;
    let mut source = LiveSource::new(opened.file, ctx.limits.profile.settings().search_buffer);
    let ext4 = group.targets[0].filesystem == crate::filesystem::FilesystemKind::Ext4;
    if (ext4 || group.targets.iter().all(|t| t.scope == Scope::Path))
        && ctx
            .job
            .as_ref()
            .is_some_and(|job| job.borrow().job.resumes > 0)
    {
        return Err(WalkError::Checkpoint(
            "this filesystem has no saved walk checkpoint".into(),
        ));
    }
    let pacer = Pacer::new(
        ctx.limits.profile.settings().duty_percent,
        SystemClock::default(),
        ctx.limits.max_runtime,
    )
    .with_stop_flag(ctx.stop);
    let pacer = if ext4 {
        pacer
    } else {
        pacer.with_memory_limit(ctx.memory_budget, Box::new(resource::resident_bytes))
    };
    let spill_root = ctx.store.root().join("spill");
    let spill_dir = space::spill_dir(&spill_root, &ctx.job_id, &fsid);
    let state_root = ctx.store.root().to_path_buf();
    let spill_reserve = space::reserve(
        ctx.limits.min_free,
        space::system_partition_total(&spill_dir),
    );
    let disk = DiskWatch::new(
        spill_dir.clone(),
        state_root.clone(),
        ctx.reserve,
        spill_reserve,
        DISK_CHECK_INTERVAL,
        Box::new(space::available),
    );
    let now = Instant::now();
    // Snapshot-only updates reuse the namespace report, not its work or ETA.
    let previous = if ctx.limits.snapshot_report.is_some() {
        None
    } else {
        previous_scan(ctx.store.root(), &fsid, &group.targets)
    };
    let progress_floor = ctx.job.as_ref().and_then(|j| {
        let j = j.borrow();
        (j.job.resumes > 0)
            .then(|| j.job.progress.clone())
            .flatten()
    });
    let elapsed_before = progress_floor.as_ref().map_or(0, |p| p.elapsed_s);
    let initial_phase = progress_floor
        .as_ref()
        .map_or("discover", |p| phase_name(&p.phase));
    let mut control = Control {
        pacer,
        disk,
        job: ctx.job.as_ref(),
        poll: Poll::new(jobs::POLL_INTERVAL, now),
        progress_poll: Poll::new(jobs::PROGRESS_INTERVAL, now),
        quiet: ctx.quiet,
        progress: ctx.progress,
        fsid: fsid.clone(),
        started: now,
        elapsed_before,
        progress_floor,
        new_work: WalkStats::default(),
        restart: None,
        last_line: now,
        phase: initial_phase,
        // Discovery reads the root tree first.
        tree: crate::btrfs::keys::ROOT_TREE_OBJECTID,
        subvolume: None,
        volume: group.targets.iter().any(|t| t.scope == Scope::Volume),
        writable: None,
        units: 0,
        entries: EntryCount::default(),
        items: 0,
        bytes: 0,
        previous,
        deletion: ctx.deletion.clone(),
    };
    if let Some(id) = &ctx.limits.attribution_report {
        return deep_attribution::run(ctx, group, fs, &mut source, &mut control, id.as_str());
    }
    if let Some(id) = &ctx.limits.snapshot_report {
        return snapshot_analysis::run(ctx, group, fs, &mut source, &mut control, id.as_str());
    }
    let roots: Vec<RootRequest> = root_infos(&group.targets)
        .into_iter()
        .map(|r| RootRequest {
            path: r.path,
            scope: r.scope,
            tree: r.tree,
            ino: r.ino,
        })
        .collect();
    let started = SystemTime::now();
    let marks = ctx.marks(env, &group.fsid);
    let mut sealing = if ext4 { None } else { ctx.unit_store(&fsid)? };
    let cache: &mut dyn UnitCache = match sealing.as_mut() {
        Some(sealing) => sealing,
        None => &mut NoCache,
    };
    let scanned = if ext4 {
        control.phase("scan");
        fs.warnings.push(Notice::new(
            "ext4_full_rescan",
            "ext4 scans always walk the full selection; resume restarts the walk",
        ));
        crate::spill::clear_leftovers(&spill_root, &fsid)?;
        let spill = Spill::create(spill_dir)?;
        crate::ext4::scan(
            env,
            group,
            ctx.store.root(),
            spill,
            (ctx.memory_budget / 8)
                .min(resource::physical_ram().unwrap_or(ctx.memory_budget) / 32)
                .clamp(1, 32 << 20),
            &mut control,
        )
        .map_err(Box::<crate::scan::ScanStopped>::from)
    } else {
        crate::spill::clear_leftovers(&spill_root, &fsid)?;
        let spill = Spill::create(spill_dir)?;
        scan_filesystem(
            &mut source,
            &roots,
            &marks,
            ScanOptions {
                snapshots: ctx.limits.snapshot_scan(),
                owner_budget: ctx.limits.owner_budget,
                pinned_read_budget: ctx.limits.pinned_read_budget,
                leaf_reads: ctx.limits.profile.settings().leaf_reads,
                pending: Some(
                    ctx.store
                        .root()
                        .join("attribution")
                        .join(&fsid)
                        .join(&ctx.job_id),
                ),
                duplicate_candidates: true,
            },
            &mut control,
            spill,
            cache,
        )
    };
    if sealing.is_some_and(|s| s.over_budget) {
        fs.warnings.push(Notice::new(
            "cache_budget",
            "--max-cache reached: later units were not kept, so a resume reads them again",
        ));
    }
    let mut outcome = match scanned {
        Ok(outcome) => outcome,
        Err(stopped) => {
            control.progress(true);
            let mut stats = measured(stopped.stats, now);
            stats.elapsed_ms += elapsed_before.saturating_mul(1000);
            fs.stats = Some(stats);
            if matches!(stopped.error, WalkError::Stopped(StopReason::ResourceLimit)) {
                let trip = control.pacer.memory_trip();
                fs.resource_limit = Some(MemoryStop::new(control.tree, ctx.memory_budget, trip));
            }
            return Err(stopped.error);
        }
    };
    if outcome.stopped.is_some() {
        outcome.coverage.approximation = None;
    }
    let finished = SystemTime::now();
    control.disk.sample_spill();
    let mut stats = measured(outcome.stats.clone(), now);
    stats.elapsed_ms += elapsed_before.saturating_mul(1000);
    for (path, reason) in &outcome.root_errors {
        if outcome.coverage.approximation.is_some() {
            continue;
        }
        fs.errors.push(TargetError::new(
            JsonPath::new(path),
            "vanished",
            reason.clone(),
        ));
    }
    for error in outcome
        .snapshot_error
        .iter()
        .chain(outcome.deletion_error.iter())
    {
        fs.errors.push(TargetError::new(
            fs.mount.clone(),
            "metadata_error",
            error.clone(),
        ));
    }
    let header = Header {
        filesystem: group.targets[0].filesystem,
        capabilities: fs.capabilities.clone(),
        format: FORMAT,
        tool_version: env!("CARGO_PKG_VERSION").into(),
        fsid: fsid.clone(),
        mount: group.mount.as_os_str().as_bytes().to_vec(),
        job_id: ctx.job_id.clone(),
        scan_started: report::capture_started(unix_seconds(started), &outcome.subvolumes),
        scan_finished: unix_seconds(finished),
        profile: ctx.limits.profile.name().into(),
        roots: roots
            .iter()
            .map(|r| RootInfo {
                path: r.path.clone(),
                scope: r.scope,
                tree: r.tree,
                ino: r.ino,
            })
            .collect(),
        consistency: outcome.consistency,
        change_reasons: outcome.change_reasons.clone(),
        state_dir_writes: outcome.state_dir_writes.clone(),
        subvolumes: outcome.subvolumes.clone(),
        coverage: outcome.coverage.clone(),
        totals: outcome.totals,
        accounting: outcome.accounting.clone(),
        stats: stats.clone(),
        snapshots: outcome.snapshots.clone(),
        space: outcome.space.clone(),
        overview: outcome.overview.clone(),
        recycle_ctimes: outcome.recycle_ctimes,
        deletion: outcome.deletion.clone(),
        vfs_space: if ext4 {
            sys::filesystem_space(source.file().as_fd())
                .ok()
                .map(|space| crate::btrfs::source::FsSpace {
                    total_bytes: space.total,
                    free_bytes: space.free,
                    available_bytes: space.available,
                })
        } else {
            None
        },
        allocated_overview: outcome.allocated_overview.clone(),
    };
    control.phase(jobs::PUBLISH);
    let write = |out: &mut dyn Write| {
        report::write_records(out, &header, outcome.nodes.count(), |w| {
            outcome.nodes.write(w)
        })
    };
    match ctx.store.publish(&header, write) {
        Ok((path, bytes)) => {
            fs.report = Some(PublishedReport {
                file: JsonPath::new(path.as_os_str().as_bytes()),
                bytes,
            });
            if !outcome.candidates.is_empty()
                && let Err(e) = crate::duplicates::candidates::publish(&path, &outcome.candidates)
            {
                fs.warnings.push(Notice::new(
                    "duplicates_not_recorded",
                    format!("duplicate candidates not saved with the report: {e}"),
                ));
            }
            let space = sys::filesystem_space(source.file().as_fd()).ok();
            let mut record = ScanRecord::of(&header, control.disk.spill_peak_bytes, space);
            record.total_elapsed_ms = ctx
                .job
                .as_ref()
                .map(|live| {
                    header
                        .scan_finished
                        .saturating_sub(live.borrow().job.created)
                        .saturating_mul(1000)
                })
                .unwrap_or(0)
                .max(record.elapsed_ms);
            record.btrfs_data_used_bytes = header
                .space
                .as_ref()
                .and_then(|window| window.end.as_ref())
                .and_then(|snapshot| snapshot.block_groups.as_ref().ok())
                .map(|groups| crate::watch_free::data_used(groups));
            record.report = Some(JsonPath::new(path.as_os_str().as_bytes()));
            record.report_bytes = Some(bytes);
            // A scan with errors publishes a partial report even when its
            // consistency is not `partial` (SPEC 22.6).
            let partial =
                scan_completion(!fs.errors.is_empty(), header.consistency, &header.coverage)
                    == Completion::Partial;
            record.outcome = if partial {
                history::Outcome::Partial
            } else {
                history::Outcome::Completed
            };
            record.reason = if partial && header.consistency == report::Consistency::Partial {
                Some(header.consistency.name().to_string())
            } else {
                fs.errors.first().map(|error| error.code.to_string())
            };
            if let Err(e) = history::record(&history::dir(ctx.store.root()), ctx.uid, &fsid, record)
            {
                fs.warnings.push(Notice::new(
                    "history_not_saved",
                    format!("scan duration not recorded for later estimates: {e}"),
                ));
            }
        }
        Err(e) => fs.errors.push(TargetError::new(
            fs.mount.clone(),
            "publish_failed",
            format!("report not saved: {e}"),
        )),
    }
    fs.consistency = Some(outcome.consistency);
    fs.change_reasons = outcome.change_reasons.clone();
    fs.state_dir_writes = outcome.state_dir_writes.clone();
    fs.scan_started = Some(rfc3339(
        SystemTime::UNIX_EPOCH + Duration::from_secs(header.scan_started),
    ));
    fs.scan_finished = Some(rfc3339(finished));
    fs.totals = Some(JsonMetrics::from(&outcome.totals).with_filesystem(fs.filesystem));
    if ext4 {
        fs.inode_walk = Some(InodeWalkStats {
            entries: stats.entries,
            inodes: stats.inodes,
            elapsed_ms: stats.elapsed_ms,
            peak_rss_bytes: stats.peak_rss_bytes,
        });
        fs.subvolumes = None;
    } else {
        fs.stats = Some(stats);
        fs.subvolumes = Some(outcome.subvolumes.clone());
    }
    fs.coverage = Some(FilesystemCoverage {
        inner: outcome.coverage.clone(),
        filesystem: fs.filesystem,
    });
    if outcome.coverage.snapshots_not_materialized > 0 {
        fs.warnings.push(Notice::new(
            "snapshots_not_materialized",
            format!(
                "{} read-only snapshot(s) not read; their bytes are not in the totals",
                outcome.coverage.snapshots_not_materialized
            ),
        ));
    }
    if let Some(e) = &outcome.snapshot_error {
        fs.warnings.push(Notice::new(
            "snapshots_not_analyzed",
            format!("snapshots not analyzed: {e}"),
        ));
    }
    if let Some(e) = &outcome.deletion_error {
        fs.warnings.push(Notice::new(
            "deletion_not_analyzed",
            format!("snapshots deleted with the selection not analyzed: {e}"),
        ));
    }
    if outcome.coverage.encrypted_directories > 0 {
        fs.warnings.push(Notice::new(
            "unsupported_encrypted",
            format!(
                "{} eCryptfs director(y/ies): unsupported, names hidden; coverage may be incomplete",
                outcome.coverage.encrypted_directories
            ),
        ));
    }
    fs.completion = scan_completion(
        !fs.errors.is_empty(),
        outcome.consistency,
        &outcome.coverage,
    );
    if let Some(reason) = outcome.stopped {
        if reason == StopReason::ResourceLimit {
            fs.resource_limit = Some(MemoryStop::new(
                control.tree,
                ctx.memory_budget,
                control.pacer.memory_trip(),
            ));
            fs.resource_limit.as_mut().expect("set").tree = None;
        }
        return Err(WalkError::Stopped(reason));
    }
    Ok(())
}

pub(crate) fn scan_completion(
    has_errors: bool,
    consistency: report::Consistency,
    coverage: &report::Coverage,
) -> Completion {
    if has_errors
        || (consistency == report::Consistency::Partial && coverage.approximation.is_none())
    {
        Completion::Partial
    } else {
        Completion::Complete
    }
}

/// The last completed scan of `targets`, from the history.
fn previous_scan(state_dir: &Path, fsid: &str, targets: &[Resolved]) -> Option<ScanRecord> {
    let records = history::load(&history::dir(state_dir), fsid);
    let key = state::scope_key(&root_infos(targets));
    history::latest_of(&records, &key).cloned()
}

/// The report roots of scan targets.
fn root_infos(targets: &[Resolved]) -> Vec<RootInfo> {
    targets
        .iter()
        .map(|t| RootInfo {
            path: t.path.as_os_str().as_bytes().to_vec(),
            scope: t.scope,
            tree: t.tree,
            ino: t.ino,
        })
        .collect()
}

/// The overall completion of per-filesystem results plus extra errors.
pub fn overall(
    filesystems: &[FilesystemScan],
    errors: &[TargetError],
    answered: bool,
) -> Completion {
    if let Some(stop) = filesystems
        .iter()
        .map(|f| f.completion)
        .find(|c| c.is_stop())
    {
        return stop;
    }
    let useful = answered || filesystems.iter().any(|f| f.report.is_some());
    if !useful {
        if filesystems.iter().any(|f| f.completion == Completion::Busy) {
            return Completion::Busy;
        }
        return Completion::Failed;
    }
    let gaps = !errors.is_empty()
        || filesystems
            .iter()
            .any(|f| f.completion != Completion::Complete);
    if gaps {
        Completion::Partial
    } else {
        Completion::Complete
    }
}

fn emit(
    json: bool,
    doc: &impl serde::Serialize,
    human: impl FnOnce() -> String,
) -> Result<(), Failure> {
    let text = if json {
        serde_json::to_string_pretty(doc)? + "\n"
    } else {
        human()
    };
    std::io::stdout().write_all(text.as_bytes())?;
    Ok(())
}

fn resolve_targets(
    env: &Environment,
    targets: &TargetArgs,
) -> Result<(Vec<Resolved>, Vec<TargetError>), Failure> {
    let args = resolve::read_operands(&targets.operands, targets.from_file.as_ref(), targets.null)?;
    Ok(resolve::resolve_all(env, &args, targets.scope))
}

/// `scan`: a new job over the targets, run here or queued for a
/// launcher; or a queued job, run here (SPEC 5.3, 22.1).
pub fn scan(
    targets: TargetArgs,
    output: OutputArgs,
    limits: ScanLimits,
    launch: LaunchArgs,
    state_dir: &Path,
) -> Result<i32, Failure> {
    if let Some(id) = &launch.job_id {
        return answer_refusal(run_queued(id, output, state_dir), |e| {
            eprintln!("synoreclaim: error: {e}");
            Ok(())
        });
    }
    let ctx = ScanContext::new(limits, state_dir, output)?;
    let args = resolve::read_operands(&targets.operands, targets.from_file.as_ref(), targets.null)?;
    let operands: Vec<Vec<u8>> = args
        .iter()
        .map(|a| operand(a).as_bytes().to_vec())
        .collect();
    let now = unix_seconds(SystemTime::now());
    let mut job = Job::new(&ctx.job_id, &operands, targets.scope, &limits, now);
    job.requested_by = launch.requested_by.clone();
    let jobs_dir = jobs_dir(&ctx.store);
    jobs::prune(&jobs_dir, jobs::FINISHED_KEPT)?;
    if let Some(launcher) = launch.detach {
        return detach(ctx, job, &args, launcher);
    }
    let live = Live::start(&jobs_dir, ctx.uid, job)?;
    run_job(ctx, live, &args, output, OnBusy::Discard)
}

/// The output of a detached job's own process, under its directory.
const DETACHED_LOG: &str = "output.log";

/// `scan --detach`: queues `job` once its targets resolve and some
/// filesystem is free, prints one JSON line naming it, and has
/// `launcher` run it. A job whose ID was handed out is never discarded.
fn detach(
    ctx: ScanContext,
    mut job: Job,
    args: &[TargetArg],
    launcher: Launcher,
) -> Result<i32, Failure> {
    let env = Environment::load()?;
    let (resolved, errors) = resolve::resolve_all(&env, args, job.scope);
    let (groups, _) = partition(resolved);
    let mut doc = DetachDocument::new(Envelope::new("detach", SystemTime::now()), launcher);
    doc.envelope.errors = errors;
    if groups.is_empty() || !doc.envelope.errors.is_empty() {
        doc.envelope.completion = Completion::Failed;
        return print_detached(&doc, 1);
    }
    job.filesystems = job_filesystems(&groups);
    let jobs_dir = jobs_dir(&ctx.store);
    let id = match jobs::queue(&jobs_dir, ctx.uid, job) {
        Ok(job) => job.job_id,
        Err(jobs::JobError::ScanInProgress(id)) => {
            doc.envelope.completion = Completion::Busy;
            doc.outcome = Some("scan_in_progress");
            doc.running_job_id = Some(id);
            return print_detached(&doc, 4);
        }
        Err(error) => return Err(error.into()),
    };
    let holder = state::LockHolder::new(Some(&ctx.job_id), "scan");
    let busy: Vec<TargetError> = groups
        .iter()
        .filter_map(|g| {
            let mount = JsonPath::new(g.mount.as_os_str().as_bytes());
            writer_lock(&fsid_text(&g.fsid), ctx.uid, &mount, &holder)
                .err()
                .map(|e| *e)
        })
        .filter(|(completion, _)| *completion == Completion::Busy)
        .map(|(_, error)| error)
        .collect();
    if busy.len() == groups.len() {
        doc.envelope.completion = Completion::Busy;
        doc.envelope.errors = busy;
        Live::launch(&jobs_dir, ctx.uid, &id, unix_seconds(SystemTime::now()))?.discard()?;
        return print_detached(&doc, 4);
    }
    let mut run = vec![std::env::current_exe()?.into_os_string()];
    run.extend(["scan", "--job-id", &id, "--state-dir"].map(std::ffi::OsString::from));
    run.push(ctx.store.root().into());
    doc.envelope.job_id = Some(id.clone());
    if let Err(e) = hand_off(&mut doc, &jobs_dir.join(&id), &run, launcher)? {
        let now = unix_seconds(SystemTime::now());
        Live::launch(&jobs_dir, ctx.uid, &id, now)?.finish(
            JobState::Failed,
            Some("launch_failed"),
            now,
        )?;
        return Err(format!("could not launch job {id}: {e}").into());
    }
    Ok(0)
}

/// Completes `doc` with the job's `run` argv, result and, for `self`,
/// log under `dir`, prints it, and has `launcher` run `run`. The inner
/// result is the spawn's.
pub(crate) fn hand_off(
    doc: &mut DetachDocument,
    dir: &Path,
    run: &[std::ffi::OsString],
    launcher: Launcher,
) -> Result<std::io::Result<()>, Failure> {
    doc.run = run
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    doc.result = Some(JsonPath::new(dir.join(jobs::RESULT).as_os_str().as_bytes()));
    let log = dir.join(DETACHED_LOG);
    if launcher == Launcher::Process {
        doc.log = Some(JsonPath::new(log.as_os_str().as_bytes()));
    }
    print_detached(doc, 0)?;
    Ok(match launcher {
        Launcher::Process => sys::spawn_session(run, &log),
        Launcher::External => Ok(()),
    })
}

/// Prints `doc` as one JSON line, flushed before anything else happens.
fn print_detached(doc: &DetachDocument, exit_code: i32) -> Result<i32, Failure> {
    let mut out = std::io::stdout().lock();
    out.write_all((serde_json::to_string(doc)? + "\n").as_bytes())?;
    out.flush()?;
    Ok(exit_code)
}

/// `scan --job-id`: runs a queued job here with the targets and limits
/// it was queued with. The job's ID was handed out, so a run that finds
/// every filesystem busy fails a new job or leaves a resumed job paused.
fn run_queued(id: &str, output: OutputArgs, state_dir: &Path) -> Result<i32, Failure> {
    if !state_dir.is_dir() {
        return Err(jobs::JobError::NotFound(id.to_string()).into());
    }
    let job = jobs::load(&state_dir.join("jobs"), id)?;
    let mut ctx = ScanContext::new(job.limits.scan_limits(), state_dir, output)?;
    ctx.job_id = id.to_string();
    let now = unix_seconds(SystemTime::now());
    let live = Live::launch(&jobs_dir(&ctx.store), ctx.uid, id, now)?;
    let args = job_args(&live.job);
    let busy = queued_busy_policy(live.job.resumes);
    run_job(ctx, live, &args, output, busy)
}

/// `resume`: runs a paused or interrupted job again with its targets and,
/// unless overridden, its limits (SPEC 5.11, 22.1).
pub fn resume(
    id: &str,
    overrides: ResumeLimits,
    detach: Option<Launcher>,
    output: OutputArgs,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let run = if overrides.fresh {
        fresh_scan(id, detach, output, state_dir)
    } else {
        match detach {
            Some(launcher) => detach_resume(id, overrides.profile, launcher, state_dir),
            None => run_resume(id, overrides, output, state_dir),
        }
    };
    answer_refusal(run, |e| {
        if output.json {
            let doc = refusal("resume", id, e, job_state(state_dir, id), SystemTime::now());
            emit(true, &doc, String::new)
        } else {
            eprintln!("synoreclaim: error: {e}");
            Ok(())
        }
    })
}

/// An explicit recovery creates a new job from protected saved settings;
/// it does not need to deserialize or trust the old scan checkpoint.
fn fresh_scan(
    id: &str,
    detach: Option<Launcher>,
    output: OutputArgs,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let job = fresh_source(state_dir, id)?;
    let targets = TargetArgs {
        operands: job
            .operands()
            .into_iter()
            .map(std::ffi::OsString::from_vec)
            .collect(),
        from_file: None,
        null: false,
        scope: job.scope,
    };
    let launch = LaunchArgs {
        detach,
        job_id: None,
        requested_by: std::env::var(crate::root_entry::REQUESTED_BY_ENV)
            .ok()
            .or(job.requested_by.clone()),
    };
    scan(targets, output, job.limits.scan_limits(), launch, state_dir)
}

fn fresh_source(state_dir: &Path, id: &str) -> Result<Job, jobs::JobError> {
    let root = state_dir.join("jobs");
    let job = jobs::load(&root, id)?;
    if job.kind != jobs::JobKind::Scan {
        return Err(jobs::JobError::NotResumable(id.into()));
    }
    if matches!(
        job.state,
        JobState::Running | JobState::Pausing | JobState::Cancelling
    ) {
        return Err(jobs::JobError::Busy(id.into()));
    }
    if !job.state.is_finished()
        && jobs::request(
            &root,
            sys::effective_uid(),
            id,
            Request::Cancel,
            unix_seconds(SystemTime::now()),
        )? != JobState::Cancelled
    {
        return Err(jobs::JobError::Busy(id.into()));
    }
    Ok(job)
}

/// `run`'s exit code, answering a job refusal with `answer` and its
/// SPEC 22.7 exit code instead of failing.
fn answer_refusal(
    run: Result<i32, Failure>,
    answer: impl FnOnce(&jobs::JobError) -> Result<(), Failure>,
) -> Result<i32, Failure> {
    let refused = match run {
        Err(e) => e,
        done => return done,
    };
    let Some(e) = refused
        .downcast_ref::<jobs::JobError>()
        .filter(|e| e.code().is_some())
    else {
        return Err(refused);
    };
    answer(e)?;
    Ok(e.exit_code())
}

/// `resume --detach`: checks that job `id` can resume, prints one JSON
/// line naming what runs it (with `profile`, if given), and has
/// `launcher` run that. The job itself is left as it was.
fn detach_resume(
    id: &str,
    profile: Option<resource::Profile>,
    launcher: Launcher,
    state_dir: &Path,
) -> Result<i32, Failure> {
    if !state_dir.is_dir() {
        return Err(jobs::JobError::NotFound(id.to_string()).into());
    }
    let store = Store::open(state_dir, sys::effective_uid())?;
    let jobs_dir = jobs_dir(&store);
    let mut doc = DetachDocument::new(Envelope::new("detach", SystemTime::now()), launcher);
    let job = jobs::resumable(&jobs_dir, id)?;
    resume::preflight(&store, &job)?;
    let now = unix_seconds(SystemTime::now());
    match jobs::queue_resume(&jobs_dir, sys::effective_uid(), id, profile, now) {
        Ok(_) => {}
        Err(jobs::JobError::ScanInProgress(running)) => {
            doc.envelope.completion = Completion::Busy;
            doc.outcome = Some("scan_in_progress");
            doc.running_job_id = Some(running);
            return print_detached(&doc, 4);
        }
        Err(error) => return Err(error.into()),
    }
    let mut run = vec![
        std::env::current_exe()?.into_os_string(),
        "scan".into(),
        "--job-id".into(),
        id.into(),
    ];
    run.push("--state-dir".into());
    run.push(store.root().into());
    doc.envelope.job_id = Some(id.to_string());
    if let Err(error) = hand_off(&mut doc, &jobs_dir.join(id), &run, launcher)? {
        jobs::request(&jobs_dir, sys::effective_uid(), id, Request::Pause, now)?;
        return Err(format!("could not launch job {id}: {error}").into());
    }
    Ok(0)
}

fn run_resume(
    id: &str,
    overrides: ResumeLimits,
    output: OutputArgs,
    state_dir: &Path,
) -> Result<i32, Failure> {
    if !state_dir.is_dir() {
        return Err(jobs::JobError::NotFound(id.to_string()).into());
    }
    let job = jobs::resumable(&state_dir.join("jobs"), id)?;
    let store = Store::open(state_dir, sys::effective_uid())?;
    resume::preflight(&store, &job)?;
    let limits = overrides.apply(job.limits.scan_limits());
    let mut ctx = ScanContext::new(limits, state_dir, output)?;
    ctx.job_id = id.to_string();
    let now = unix_seconds(SystemTime::now());
    let mut live = Live::resume(&jobs_dir(&ctx.store), ctx.uid, id, now)?;
    live.job.limits = JobLimits::of(&limits);
    live.save()?;
    if !output.quiet {
        eprintln!(
            "synoreclaim: resuming job {id} from its saved namespace checkpoint{}",
            if limits.no_cache {
                " (--no-cache disables disposable caching, not the job checkpoint)"
            } else {
                ""
            }
        );
    }
    let args = job_args(&live.job);
    run_job(ctx, live, &args, output, OnBusy::Pause)
}

/// The targets `job` was started with.
fn job_args(job: &Job) -> Vec<TargetArg> {
    job.operands()
        .into_iter()
        .map(|o| parse_operand(std::ffi::OsString::from_vec(o)))
        .collect()
}

/// What a run that found every filesystem busy leaves of its job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnBusy {
    /// Nothing: the run never started.
    Discard,
    /// The job, paused as it was.
    Pause,
    /// The job, failed: its ID was handed out.
    Fail,
}

fn queued_busy_policy(resumes: u32) -> OnBusy {
    if resumes == 0 {
        OnBusy::Fail
    } else {
        OnBusy::Pause
    }
}

fn jobs_dir(store: &Store) -> PathBuf {
    store.root().join("jobs")
}

/// Scans the targets of `live`'s job, then records how the run ended.
fn run_job(
    mut ctx: ScanContext,
    live: Live,
    args: &[TargetArg],
    output: OutputArgs,
    busy: OnBusy,
) -> Result<i32, Failure> {
    let env = Environment::load()?;
    let requested: Vec<JsonPath> = live
        .job
        .operands()
        .iter()
        .map(|o| JsonPath::new(o))
        .collect();
    let (resolved, errors) = resolve::resolve_all(&env, args, live.job.scope);
    let volumes: Vec<String> = resolved
        .iter()
        .filter(|t| t.scope == Scope::Volume)
        .map(|t| fsid_text(&t.fsid))
        .collect();
    ctx.job = Some(RefCell::new(live));
    if !output.quiet {
        eprintln!("synoreclaim: job {}", ctx.job_id);
    }
    let (groups, covered) = partition(resolved);
    record_filesystems(ctx.job.as_ref().expect("set above"), &groups)?;
    let filesystems = scan_groups(&ctx, &env, groups, covered);
    let mut envelope = Envelope::new("scan", SystemTime::now());
    envelope.job_id = Some(ctx.job_id.clone());
    envelope.completion = overall(&filesystems, &errors, false);
    envelope.errors = errors;
    let mut doc = ScanDocument {
        envelope,
        resources: ctx.resources(),
        requested_targets: requested,
        filesystems,
    };
    let mut live = ctx.job.take().expect("set above").into_inner();
    if let Some(request) = live
        .requested()
        .filter(|_| !doc.envelope.completion.is_stop())
    {
        doc.envelope.warnings.push(Notice::new(
            "request_ignored",
            format!(
                "a {} request came after the last check; the scan finished anyway",
                request.name()
            ),
        ));
    }
    let exit_code = doc.envelope.completion.exit_code(sys::stop_signal());
    live.record_result(
        job_result(&doc, exit_code),
        (serde_json::to_string_pretty(&doc)? + "\n").as_bytes(),
    )?;
    let job = live.job.clone();
    if !finish_job(live, &doc, busy)? {
        doc.envelope.job_id = None;
    }
    emit(output.json, &doc, || render_scan(&doc))?;
    if job.requested_by.is_some() {
        prewarm_ui_cache(&ctx, &job, &volumes, doc.envelope.completion);
    }
    Ok(exit_code)
}

/// After a volume scan a DSM user asked for through `webapi`, caches
/// the answers the UI shows first. Best effort: a
/// failure leaves the answers to be computed when asked.
fn prewarm_ui_cache(ctx: &ScanContext, job: &Job, volumes: &[String], completion: Completion) {
    if !matches!(completion, Completion::Complete | Completion::Partial) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let state_dir = ctx.store.root();
    for fs in job.filesystems.iter().filter(|f| volumes.contains(&f.fsid)) {
        let mount = Path::new(std::ffi::OsStr::from_bytes(&fs.mount));
        for argv in crate::webapi::prewarmed(state_dir, mount) {
            let now = unix_seconds(SystemTime::now());
            let _ =
                crate::ui_cache::prewarm(state_dir, ctx.uid, &exe, &fs.fsid, &fs.mount, &argv, now);
        }
    }
}

/// How a scan ended, for its job.
fn job_result(doc: &ScanDocument, exit_code: i32) -> jobs::JobResult {
    let reports: Vec<Vec<u8>> = doc
        .filesystems
        .iter()
        .filter_map(|f| f.report.as_ref())
        .map(|r| r.file.bytes())
        .collect();
    let mut codes: Vec<String> = doc
        .envelope
        .errors
        .iter()
        .chain(doc.filesystems.iter().flat_map(|f| &f.errors))
        .map(|e| e.code.to_string())
        .collect();
    codes.sort();
    codes.dedup();
    jobs::JobResult::new(doc.envelope.completion.name(), exit_code, &reports, codes)
}

/// Records in the job manifest the filesystems the targets resolved to.
fn record_filesystems(job: &RefCell<Live>, groups: &[FilesystemTargets]) -> Result<(), Failure> {
    let mut live = job.borrow_mut();
    live.job.filesystems = job_filesystems(groups);
    live.save()?;
    Ok(())
}

fn job_filesystems(groups: &[FilesystemTargets]) -> Vec<jobs::JobFilesystem> {
    groups
        .iter()
        .map(|g| jobs::JobFilesystem {
            fsid: fsid_text(&g.fsid),
            mount: g.mount.as_os_str().as_bytes().to_vec(),
        })
        .collect()
}

/// Records in the job manifest how its run ended, or, for a run that
/// found every filesystem busy, what `busy` says. Returns whether the
/// job is kept.
fn finish_job(mut live: Live, doc: &ScanDocument, busy: OnBusy) -> Result<bool, Failure> {
    let completion = doc.envelope.completion;
    if completion == Completion::Busy && busy == OnBusy::Discard {
        live.discard()?;
        return Ok(false);
    }
    let (state, reason) = match completion {
        Completion::Complete => (JobState::Completed, None),
        Completion::Partial => (JobState::Partial, None),
        Completion::Failed => (JobState::Failed, Some(completion.name())),
        Completion::Busy if busy == OnBusy::Fail => (JobState::Failed, Some(completion.name())),
        Completion::Busy => (JobState::Paused, Some(completion.name())),
        Completion::Cancelled => (JobState::Cancelled, Some("cancelled")),
        _ if live.requested() == Some(Request::Cancel) => (JobState::Cancelled, Some("cancelled")),
        _ => (JobState::Paused, Some(completion.name())),
    };
    live.job.units = doc
        .filesystems
        .iter()
        .filter_map(|f| f.stats.as_ref())
        .map(|s| s.subvolumes_scanned + s.subvolumes_reused)
        .sum();
    live.finish(state, reason, unix_seconds(SystemTime::now()))?;
    Ok(true)
}

/// `pause` or `cancel` (SPEC 22.1): a request the running job observes
/// at its next safe boundary; cancelling a stopped job ends it at once.
/// With `json`, a refusal is a document with its code, not an error.
pub fn request_job(
    id: &str,
    request: Request,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let asked = ask_job(id, request, state_dir);
    if !json {
        let state = asked?;
        println!("job {id}: {}", request_text(request, state));
        return Ok(0);
    }
    if let Err(e) = &asked
        && e.code().is_none()
    {
        return Err(asked.unwrap_err().into());
    }
    let now = job_state(state_dir, id);
    let doc = request_answer(id, request, &asked, now, SystemTime::now());
    emit(true, &doc, String::new)?;
    Ok(asked.map_or_else(|e| e.exit_code(), |_| 0))
}

fn ask_job(id: &str, request: Request, state_dir: &Path) -> Result<JobState, jobs::JobError> {
    if !state_dir.is_dir() {
        return Err(jobs::JobError::NotFound(id.to_string()));
    }
    let uid = sys::effective_uid();
    let store = Store::open(state_dir, uid)?;
    let now = unix_seconds(SystemTime::now());
    jobs::request(&jobs_dir(&store), uid, id, request, now)
}

/// `mode`: asks running scan `id` to go on with `profile`, keeping its
/// progress.
pub fn change_mode(
    id: &str,
    profile: resource::Profile,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    const TEXT: &str =
        "mode change requested; the scan goes on in the new mode from its next check";
    let asked = if state_dir.is_dir() {
        let uid = sys::effective_uid();
        Store::open(state_dir, uid)
            .map_err(jobs::JobError::from)
            .and_then(|store| jobs::change_profile(&jobs_dir(&store), uid, id, profile))
    } else {
        Err(jobs::JobError::NotFound(id.to_string()))
    };
    if !json {
        asked?;
        println!("job {id}: {TEXT}");
        return Ok(0);
    }
    if let Err(e) = &asked
        && e.code().is_none()
    {
        return Err(asked.unwrap_err().into());
    }
    let state = job_state(state_dir, id);
    let now = SystemTime::now();
    let doc = match &asked {
        Ok(_) => {
            let mut envelope = Envelope::new("mode", now);
            envelope.job_id = Some(id.to_string());
            JobRequestDocument {
                envelope,
                outcome: "requested",
                message: TEXT.to_string(),
                state: state.map(JobState::name),
            }
        }
        Err(e) => refusal("mode", id, e, state, now),
    };
    emit(true, &doc, String::new)?;
    Ok(asked.map_or_else(|e| e.exit_code(), |_| 0))
}

/// Job `id`'s state as `jobs` shows it, if it exists.
fn job_state(state_dir: &Path, id: &str) -> Option<JobState> {
    jobs::load(&state_dir.join("jobs"), id)
        .ok()
        .map(|j| j.state)
}

fn request_text(request: Request, state: JobState) -> &'static str {
    match (request, state) {
        (_, JobState::Cancelled) => "cancelled; completed units and user files are kept",
        (Request::Pause, JobState::Paused) => "paused; the queued resume did not start",
        (Request::Pause, _) => {
            "pause requested; the scan checkpoints and exits at its next safe point"
        }
        (Request::Cancel, _) => "cancel requested; the scan stops at its next safe point",
    }
}

/// The `--json` answer to a request: `requested` or `cancelled`, or the
/// refusal's code, with the job's state afterwards.
fn request_answer(
    id: &str,
    request: Request,
    asked: &Result<JobState, jobs::JobError>,
    state: Option<JobState>,
    now: SystemTime,
) -> JobRequestDocument {
    let command = request.name();
    let mut envelope = Envelope::new(command, now);
    envelope.job_id = Some(id.to_string());
    let (outcome, message) = match asked {
        Ok(JobState::Cancelled) => (
            "cancelled",
            request_text(request, JobState::Cancelled).to_string(),
        ),
        Ok(after) => ("requested", request_text(request, *after).to_string()),
        Err(e) => return refusal(command, id, e, state, now),
    };
    JobRequestDocument {
        envelope,
        outcome,
        message,
        state: state.map(JobState::name),
    }
}

/// The `--json` refusal of a job command: the error's code as outcome,
/// with the job's state.
fn refusal(
    command: &'static str,
    id: &str,
    e: &jobs::JobError,
    state: Option<JobState>,
    now: SystemTime,
) -> JobRequestDocument {
    let mut envelope = Envelope::new(command, now);
    envelope.job_id = Some(id.to_string());
    envelope.completion = Completion::Failed;
    JobRequestDocument {
        envelope,
        outcome: e.code().unwrap_or("error"),
        message: e.to_string(),
        state: state.map(JobState::name),
    }
}

/// `jobs` (SPEC 5.11): lists jobs, or one named job, without starting
/// any work or creating the state directory.
pub fn jobs(json: bool, state_dir: &Path, job_id: Option<&str>) -> Result<i32, Failure> {
    let jobs_dir = state_dir.join("jobs");
    let listed = match job_id {
        Some(id) => {
            if !state_dir.is_dir() {
                return Err(jobs::JobError::NotFound(id.to_string()).into());
            }
            Store::open(state_dir, sys::effective_uid())?;
            vec![jobs::load(&jobs_dir, id)?]
        }
        None if state_dir.is_dir() => {
            Store::open(state_dir, sys::effective_uid())?;
            jobs::list(&jobs_dir)?
        }
        None => Vec::new(),
    };
    let doc = JobsDocument {
        envelope: Envelope::new("jobs", SystemTime::now()),
        jobs: listed.iter().map(|job| job_info(&jobs_dir, job)).collect(),
    };
    emit(json, &doc, || render_jobs(&doc))?;
    Ok(0)
}

/// `jobs --result`: the whole document of job `id`'s latest run, as
/// saved in `jobs/<id>/result.json`.
pub fn show_job_result(id: &str, json: bool, state_dir: &Path) -> Result<i32, Failure> {
    answer_refusal(print_job_result(id, state_dir), |e| {
        if json {
            let doc = refusal(
                "job-result",
                id,
                e,
                job_state(state_dir, id),
                SystemTime::now(),
            );
            emit(true, &doc, String::new)
        } else {
            eprintln!("synoreclaim: error: {e}");
            Ok(())
        }
    })
}

fn print_job_result(id: &str, state_dir: &Path) -> Result<i32, Failure> {
    if !state_dir.is_dir() {
        return Err(jobs::JobError::NotFound(id.to_string()).into());
    }
    let store = Store::open(state_dir, sys::effective_uid())?;
    let jobs_dir = jobs_dir(&store);
    if jobs::load(&jobs_dir, id)?.result.is_none() {
        return Err(jobs::JobError::NoResult(id.to_string()).into());
    }
    let document = std::fs::read(jobs_dir.join(id).join(jobs::RESULT))?;
    std::io::stdout().write_all(&document)?;
    Ok(0)
}

/// The configuration's location, and the path a refusal names: the real
/// file when it resolves, else the package's alias.
fn config_location() -> (std::path::PathBuf, Result<crate::config::Location, Failure>) {
    let location = crate::config::location();
    let target = location.as_ref().map_or_else(
        |_| crate::config::CONFIG_ALIAS_PATH.into(),
        |l| l.path.clone(),
    );
    (target, location)
}

/// The `--json` answer of `config set` and its exit code, for the package
/// CGI: it runs as the package user, who owns the configuration.
pub fn config_answer(mutations_disabled: bool) -> (serde_json::Value, i32) {
    let action = ConfigAction::Set {
        mutations_disabled,
        json: true,
    };
    let now = SystemTime::now();
    let (target, location) = config_location();
    let answer = location
        .and_then(|location| crate::config::run(&action, &location.path, location.owner, now))
        .map(|doc| (serde_json::to_value(doc), 0))
        .unwrap_or_else(|error| {
            let doc = cleanup_failure_document(
                crate::config::command_name(&action),
                now,
                target.as_os_str().as_bytes(),
                error.as_ref(),
            );
            (
                serde_json::to_value(doc),
                guard::failure_exit_code(error.as_ref()),
            )
        });
    (answer.0.unwrap_or_default(), answer.1)
}

/// `config init|set|show` (SPEC 5.7): the cleanup configuration in the
/// package folder (`config::location`); root only.
pub fn config(action: ConfigAction) -> Result<i32, Failure> {
    let json = match action {
        ConfigAction::Init { json }
        | ConfigAction::Set { json, .. }
        | ConfigAction::Show { json } => json,
    };
    let (target, location) = config_location();
    let result = if sys::effective_uid() != 0 {
        Err(Refusal::new("not_root", "config requires root privileges").into())
    } else {
        location
            .and_then(|location| {
                crate::config::run(&action, &location.path, location.owner, SystemTime::now())
            })
            .and_then(|doc| {
                emit(json, &doc, || crate::config::render(&doc))?;
                Ok(0)
            })
    };
    finish_cleanup(
        json,
        crate::config::command_name(&action),
        target.as_os_str().as_bytes(),
        result,
    )
}

/// `history` (SPEC 5.20): one entry per completed scan or finished job,
/// newest first; never starts work or creates the state directory.
pub fn history(json: bool, state_dir: &Path) -> Result<i32, Failure> {
    let doc = HistoryDocument {
        envelope: Envelope::new("history", SystemTime::now()),
        scans: history_entries(state_dir),
    };
    emit(json, &doc, || render_history(&doc))?;
    Ok(0)
}

/// The published scans of every filesystem under `state_dir`, plus the
/// finished scan jobs that published no report (cancelled, failed).
fn history_entries(state_dir: &Path) -> Vec<HistoryEntry> {
    if !state_dir.is_dir() {
        return Vec::new();
    }
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    let mut entries: Vec<(u64, HistoryEntry)> = history::load_all(&history::dir(state_dir))
        .iter()
        .map(|(fsid, record)| {
            recorded.insert(record.job_id.clone());
            (record.finished, record_entry(record, fsid))
        })
        .collect();
    for job in jobs::list(&state_dir.join("jobs")).unwrap_or_default() {
        if job.kind != jobs::JobKind::Scan {
            // An apply deletes, it does not scan: jobs.list shows cleanup
            // applies, and rootfs applies have their own history document.
            continue;
        }
        if recorded.contains(&job.job_id) {
            continue;
        }
        if job.reason.as_deref() == Some("busy") {
            // A busy attempt did no work and leaves no report; it is not a scan.
            continue;
        }
        let Some(outcome) = job_outcome(job.state) else {
            continue;
        };
        entries.push((job.updated, job_entry(&job, outcome)));
    }
    entries.sort_by_key(|(finished, _)| std::cmp::Reverse(*finished));
    entries.into_iter().map(|(_, entry)| entry).collect()
}

/// The outcome name of a job whose scan published no report; `None` for
/// a job still running (it is not yet a history entry).
fn job_outcome(state: JobState) -> Option<&'static str> {
    match state {
        JobState::Completed => Some("completed"),
        JobState::Partial => Some("partial"),
        JobState::Cancelled => Some("cancelled"),
        JobState::Failed => Some("failed"),
        JobState::Paused | JobState::Interrupted => Some("paused"),
        JobState::Queued | JobState::Running | JobState::Pausing | JobState::Cancelling => None,
    }
}

fn record_entry(record: &ScanRecord, fsid: &str) -> HistoryEntry {
    let report = record
        .report
        .as_ref()
        .filter(|path| Path::new(std::ffi::OsStr::from_bytes(&path.bytes())).is_file())
        .cloned();
    HistoryEntry {
        job_id: record.job_id.clone(),
        fsid: Some(fsid.to_string()),
        original_outcome: record.original_outcome.map(history::Outcome::name),
        targets: record.targets.clone(),
        volume: record.volume,
        profile: record.profile.clone(),
        started: at(record.started),
        finished: at(record.finished),
        elapsed_ms: record.elapsed_ms,
        total_elapsed_ms: record.total_elapsed_ms,
        entries: record.entries,
        subvolumes: record.subvolumes,
        reused_units: record.reused_units,
        full_read_ms: record.full_read_ms,
        full_read_entries: record.full_read_entries,
        outcome: record.outcome.name(),
        reason: record.reason.clone(),
        peak_rss_bytes: record.peak_rss_bytes,
        spill_peak_bytes: record.spill_peak_bytes,
        report_bytes: record.report_bytes,
        report,
        df_total_bytes: record.total_bytes,
        df_used_bytes: record.used_bytes,
        df_free_bytes: record.free_bytes,
        data_used_bytes: record.btrfs_data_used_bytes,
        attribution: record.attribution.clone(),
    }
}

/// What a finished job that published no report shows (SPEC 5.20): its
/// outcome and targets, without the scan statistics a report would have.
fn job_entry(job: &Job, outcome: &'static str) -> HistoryEntry {
    HistoryEntry {
        job_id: job.job_id.clone(),
        fsid: None,
        original_outcome: None,
        targets: job.operands().iter().map(|o| JsonPath::new(o)).collect(),
        volume: job.scope == Some(Scope::Volume),
        profile: Some(job.limits.profile.name().into()),
        started: at(job.created),
        finished: at(job.updated),
        elapsed_ms: job.updated.saturating_sub(job.created) * 1000,
        total_elapsed_ms: job.updated.saturating_sub(job.created) * 1000,
        entries: 0,
        subvolumes: job.units,
        reused_units: 0,
        full_read_ms: None,
        full_read_entries: None,
        outcome,
        reason: job.reason.clone(),
        peak_rss_bytes: None,
        spill_peak_bytes: 0,
        report: None,
        report_bytes: None,
        df_total_bytes: None,
        df_used_bytes: None,
        df_free_bytes: None,
        data_used_bytes: None,
        attribution: None,
    }
}

fn at(secs: u64) -> String {
    rfc3339(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn job_info(jobs: &Path, job: &Job) -> JobInfo {
    JobInfo {
        job_id: job.job_id.clone(),
        kind: job.kind.name(),
        plan_id: job.plan_id.clone(),
        elapsed_s: matches!(
            job.kind,
            jobs::JobKind::CleanupPlan | jobs::JobKind::RootfsPlan | jobs::JobKind::RootfsReport
        )
        .then(|| {
            if job.state.is_finished() {
                job.updated
            } else {
                unix_seconds(SystemTime::now())
            }
            .saturating_sub(job.created)
        }),
        state: job.state.name(),
        phase: job.phase.clone(),
        targets: job.operands().iter().map(|o| JsonPath::new(o)).collect(),
        scope: job.scope,
        filesystems: job
            .filesystems
            .iter()
            .map(|f| JobFilesystemInfo {
                fsid: f.fsid.clone(),
                mount: JsonPath::new(&f.mount),
            })
            .collect(),
        profile: job.limits.profile.name(),
        created: at(job.created),
        updated: at(job.updated),
        resumes: job.resumes,
        units: job.units,
        progress: job.progress.clone(),
        cleanup: job.cleanup.clone(),
        rootfs: job.rootfs.clone(),
        rootfs_plan: job.rootfs_plan.clone(),
        reason: job.reason.clone(),
        tool_version: job.versions.tool_version.clone(),
        compatible: job.compatible(),
        request: job.request.map(Request::name),
        request_ignored: job.request_ignored.map(Request::name),
        result: job.result.as_ref().map(|r| JobResultInfo {
            completion: r.completion.clone(),
            exit_code: r.exit_code,
            reports: r.reports().iter().map(|p| JsonPath::new(p)).collect(),
            error_codes: r.error_codes.clone(),
            document: JsonPath::new(
                jobs.join(&job.job_id)
                    .join(jobs::RESULT)
                    .as_os_str()
                    .as_bytes(),
            ),
        }),
        requested_by: job.requested_by.clone(),
    }
}

/// Finds, scanning if allowed, the newest report covering each target.
struct Answerer<'a> {
    env: &'a Environment,
    limits: ScanLimits,
    state_dir: &'a Path,
    answer: AnswerArgs,
    output: OutputArgs,
    scans: Vec<FilesystemScan>,
    errors: Vec<TargetError>,
    hints: Vec<ScanHint>,
    job_id: Option<String>,
    /// The report the user chose to answer from (`--report`), by its scan
    /// job; `None` answers from the newest report covering each target.
    report: Option<String>,
}

impl<'a> Answerer<'a> {
    fn store(&self) -> Result<Store, Failure> {
        Ok(Store::open(self.state_dir, sys::effective_uid())?)
    }

    fn covering(&self, store: &Store, target: &Resolved) -> Result<Option<PathBuf>, Failure> {
        let listing = store.display_reports(&fsid_text(&target.fsid))?;
        let path = target.path.as_os_str().as_bytes();
        Ok(listing
            .reports
            .into_iter()
            .find(|(_, h)| {
                query::covers(h, path) && self.report.as_ref().is_none_or(|job| &h.job_id == job)
            })
            .map(|(p, _)| p))
    }

    /// Returns `(target, report path)` for every answerable target.
    fn prepare(&mut self, targets: Vec<Resolved>) -> Result<Vec<(Resolved, PathBuf)>, Failure> {
        let store = self.store()?;
        let mut ready = Vec::new();
        let mut to_scan = Vec::new();
        for target in targets {
            let found = if self.answer.refresh {
                None
            } else {
                self.covering(&store, &target)?
            };
            match found {
                Some(path) => ready.push((target, path)),
                None if self.report.is_some() => {
                    self.refuse(&target, "the chosen report does not cover this target")?
                }
                None if self.answer.no_scan => self.refuse(&target, "--no-scan given")?,
                None if target.scope == Scope::Volume && !self.answer.refresh => self.refuse(
                    &target,
                    "volume targets are not scanned implicitly; run `synoreclaim scan` or pass --refresh",
                )?,
                None => to_scan.push(target),
            }
        }
        if !to_scan.is_empty() {
            let ctx = ScanContext::new(self.limits, self.state_dir, self.output)?;
            if !self.output.quiet {
                eprintln!(
                    "synoreclaim: job {} (scanning targets without a report)",
                    ctx.job_id
                );
            }
            self.job_id = Some(ctx.job_id.clone());
            let (groups, covered) = partition(to_scan.clone());
            self.scans = scan_groups(&ctx, self.env, groups, covered);
            for target in to_scan {
                match self.covering(&store, &target)? {
                    Some(path) => ready.push((target, path)),
                    None => {
                        let unreadable = unreadable_reports(&store, &target)?;
                        self.errors.push(no_report(
                            &target,
                            "the scan did not produce a report",
                            unreadable,
                        ));
                    }
                }
            }
        }
        Ok(ready)
    }

    /// The report whose root set is exactly `group`, deleted with the
    /// snapshots of `deletion` if any, scanning it when allowed; `None`
    /// after recording why.
    fn prepare_selection(
        &mut self,
        group: FilesystemTargets,
        deletion: Option<DeletionRequest>,
    ) -> Result<Option<PathBuf>, Failure> {
        let store = self.store()?;
        let key = state::scope_key(&root_infos(&group.targets));
        let fsid = fsid_text(&group.fsid);
        let deleted: Option<Vec<u64>> = deletion
            .as_ref()
            .map(|d| d.snapshots.iter().copied().collect());
        let exact = |store: &Store| -> Result<Option<PathBuf>, Failure> {
            Ok(store
                .reports(&fsid)?
                .reports
                .into_iter()
                .find(|(_, h)| {
                    state::scope_key(&h.roots) == key
                        && deleted.as_ref().is_none_or(|ids| {
                            h.deletion.as_ref().is_some_and(|d| &d.snapshots == ids)
                        })
                })
                .map(|(p, _)| p))
        };
        if !self.answer.refresh
            && let Some(path) = exact(&store)?
        {
            return Ok(Some(path));
        }
        if self.answer.no_scan {
            for target in &group.targets {
                let unreadable = unreadable_reports(&store, target)?;
                self.errors.push(no_report(
                    target,
                    "no report of exactly this selection and --no-scan given",
                    unreadable,
                ));
            }
            return Ok(None);
        }
        let mut ctx = ScanContext::new(self.limits, self.state_dir, self.output)?;
        ctx.deletion = deletion;
        if !self.output.quiet {
            eprintln!("synoreclaim: job {} (scanning the selection)", ctx.job_id);
        }
        self.job_id = Some(ctx.job_id.clone());
        let targets = group.targets.clone();
        self.scans
            .extend(scan_groups(&ctx, self.env, vec![group], Vec::new()));
        let found = exact(&store)?;
        if found.is_none() {
            for target in &targets {
                let unreadable = unreadable_reports(&store, target)?;
                self.errors.push(no_report(
                    target,
                    "the scan did not produce a report",
                    unreadable,
                ));
            }
        }
        Ok(found)
    }

    /// Records that no report answers `target`, with what to run for a
    /// volume.
    fn refuse(&mut self, target: &Resolved, why: &str) -> Result<(), Failure> {
        let unreadable = unreadable_reports(&self.store()?, target)?;
        self.errors.push(no_report(target, why, unreadable));
        if target.scope == Scope::Volume {
            self.hints.push(self.scan_hint(target));
        }
        Ok(())
    }

    fn scan_hint(&self, target: &Resolved) -> ScanHint {
        scan_hint(self.state_dir, target)
    }

    /// Puts how the answer went into `envelope`.
    fn close(&mut self, envelope: &mut Envelope, answered: bool) {
        envelope.completion = self.completion(answered);
        envelope.job_id = self.job_id.clone();
        envelope.errors = std::mem::take(&mut self.errors);
        envelope.scan_hints = std::mem::take(&mut self.hints);
    }

    fn completion(&self, answered: bool) -> Completion {
        let stopped = overall(&self.scans, &[], true);
        if stopped.is_stop() {
            return stopped;
        }
        if !answered {
            return Completion::Failed;
        }
        overall(&self.scans, &self.errors, true)
    }
}

/// What to run to scan `target`, with what its last volume scan took.
fn scan_hint(state_dir: &Path, target: &Resolved) -> ScanHint {
    let records = history::load(&history::dir(state_dir), &fsid_text(&target.fsid));
    let last_scan = history::latest_volume(&records).map(|r| {
        let (elapsed_ms, entries) = r.eta_basis();
        LastScan {
            finished: rfc3339(SystemTime::UNIX_EPOCH + Duration::from_secs(r.finished)),
            elapsed_ms,
            entries,
        }
    });
    ScanHint::new(target.path.as_os_str().as_bytes(), state_dir, last_scan)
}

/// How many of `target`'s filesystem's reports cannot be read.
fn unreadable_reports(store: &Store, target: &Resolved) -> Result<usize, Failure> {
    Ok(store.reports(&fsid_text(&target.fsid))?.unreadable.len())
}

/// That no report answers `target`; `report_incompatible` when
/// `unreadable` reports of its filesystem (damaged, or from another
/// report format) might have.
fn no_report(target: &Resolved, why: &str, unreadable: usize) -> TargetError {
    let path = JsonPath::new(target.path.as_os_str().as_bytes());
    if unreadable == 0 {
        return TargetError::new(
            path,
            "no_report",
            format!("no report covers this target: {why}"),
        );
    }
    TargetError::new(
        path,
        "report_incompatible",
        format!(
            "no readable report covers this target: {why}; {unreadable} reports of its filesystem \
             are unreadable or from an older synoreclaim, scan again"
        ),
    )
}

/// Whether `(tree, ino)` is the root of a snapshot some report analyzed.
fn is_snapshot_root(reports: &[(PathBuf, Header)], tree: u64, ino: u64) -> bool {
    ino == FIRST_FREE_OBJECTID
        && reports.iter().any(|(_, h)| {
            h.snapshots
                .as_ref()
                .is_some_and(|a| a.snapshot(tree).is_some())
        })
}

/// Splits the snapshot roots off a group that also holds live paths: the
/// roots some report analyzed, or read-only subvolumes. A group of only
/// live paths or only snapshot roots stays whole.
fn split_deleted(
    group: FilesystemTargets,
    reports: &[(PathBuf, Header)],
) -> (FilesystemTargets, Vec<Resolved>) {
    let read_only = if group.targets.iter().any(|t| t.ino == FIRST_FREE_OBJECTID) {
        read_only_roots(&group.mount)
    } else {
        BTreeSet::new()
    };
    let (deleted, live): (Vec<Resolved>, Vec<Resolved>) =
        group.targets.iter().cloned().partition(|t| {
            t.ino == FIRST_FREE_OBJECTID
                && (read_only.contains(&t.tree) || is_snapshot_root(reports, t.tree, t.ino))
        });
    if deleted.is_empty() || live.is_empty() {
        return (group, Vec::new());
    }
    (
        FilesystemTargets {
            targets: live,
            ..group
        },
        deleted,
    )
}

/// The read-only subvolumes of the filesystem mounted at `mount`; none
/// when its root tree cannot be read.
fn read_only_roots(mount: &Path) -> BTreeSet<u64> {
    let Ok(opened) = OpenTarget::open(mount) else {
        return BTreeSet::new();
    };
    let mut source = LiveSource::new(opened.file, 4096);
    discover_slowly(&mut source)
        .map(|set| {
            set.subvolumes
                .iter()
                .filter(|s| s.readonly && !s.pending_deletion)
                .map(|s| s.id)
                .collect()
        })
        .unwrap_or_default()
}

/// Discovers the Btrfs roots at the low profile's pace.
fn discover_slowly(source: &mut LiveSource) -> Result<SubvolumeSet, String> {
    crate::watch_free::discover_subvolumes(source, resource::Profile::Low, None)
}

/// What the scan of the live paths reads to delete the snapshot roots
/// `deleted` with them: `--snapshots` depth, the newest 64 by default.
fn deletion_request(deleted: &[Resolved], limits: &ScanLimits) -> Option<DeletionRequest> {
    if deleted.is_empty() {
        return None;
    }
    Some(DeletionRequest {
        snapshots: deleted.iter().map(|t| t.tree).collect(),
        scan: SnapshotScan {
            depth: limits
                .snapshots
                .unwrap_or(crate::snapshots::Depth::Newest(DEFAULT_NEWEST)),
            owner_budget: limits.owner_budget,
            global_owner_budget: limits.global_owner_budget,
            held_files_budget: limits.held_files_budget,
        },
    })
}

/// The combined answer of deleting the snapshot roots `deleted` with a
/// selection.
fn with_snapshots(deleted: &[Resolved], answer: &DeletionAnswer) -> WithSnapshotsJson {
    let path = |t: &Resolved| t.path.as_os_str().as_bytes().to_vec();
    WithSnapshotsJson {
        snapshots: deleted.iter().map(|t| JsonPath::new(&path(t))).collect(),
        snapshot_ids: answer.snapshots.clone(),
        freed_bytes: answer.freed_bytes,
        unknown_bytes: answer.unknown_bytes,
        unknown_extents: answer.unknown_extents,
        snapshots_not_materialized: answer.snapshots_not_materialized,
        manage_in: deleted
            .first()
            .map_or(MANAGE_IN, |t| crate::snapshots::manage_in(&path(t)))
            .into(),
        manage_in_ids: deleted
            .iter()
            .map(|t| crate::snapshots::manage_in_id(&path(t)))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}

/// Deleting the snapshot `roots` of one filesystem together.
fn snapshot_set(
    reports: &[(PathBuf, Header)],
    roots: &[&Resolved],
    now: SystemTime,
    max_age: Duration,
    state_dir: &Path,
) -> Option<SnapshotSetResult> {
    let ids = roots.iter().map(|t| t.tree).collect();
    let (file, header) = snapshot_report(reports, &ids)?;
    let analysis = header.snapshots.as_ref()?;
    let paths: Vec<JsonPath> = roots
        .iter()
        .map(|t| JsonPath::new(t.path.as_os_str().as_bytes()))
        .collect();
    let selector = roots
        .iter()
        .map(|t| t.path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(",");
    Some(SnapshotSetResult {
        report: answered_report(file, header, now, max_age),
        snapshots: paths,
        simulation: simulate_set(
            analysis,
            selector,
            &ids,
            &Rescan {
                mount: &header.mount,
                state_dir,
            },
        ),
        manage_in: snapshot_management_places(analysis, &ids)
            .into_iter()
            .collect::<Vec<_>>()
            .join("; "),
        manage_in_ids: snapshot_management_places(analysis, &ids)
            .into_iter()
            .map(crate::dsm::place_id)
            .collect(),
    })
}

/// The DSM places that delete the snapshots of `ids`, most compact first.
fn snapshot_management_places(
    analysis: &crate::snapshots::SnapshotAnalysis,
    ids: &BTreeSet<u64>,
) -> BTreeSet<&'static str> {
    let locations: BTreeSet<&'static str> = ids
        .iter()
        .filter_map(|&id| analysis.snapshot(id))
        .map(|(share, _)| crate::snapshots::manage_in_share(share))
        .collect();
    if locations.is_empty() {
        return BTreeSet::from([MANAGE_IN]);
    }
    locations
}

/// The newest report whose snapshot analysis holds every one of `ids`.
fn snapshot_report<'a>(
    reports: &'a [(PathBuf, Header)],
    ids: &BTreeSet<u64>,
) -> Option<&'a (PathBuf, Header)> {
    reports.iter().find(|(_, h)| {
        h.snapshots
            .as_ref()
            .is_some_and(|a| ids.iter().all(|&id| a.snapshot(id).is_some()))
    })
}

fn identity_warning(target: &Resolved, entry: &query::Entry) -> Option<Notice> {
    (entry.tree != target.tree || entry.objectid != target.ino).then(|| {
        Notice::new(
            "identity_changed",
            format!(
                "{} is now a different object than when scanned; use --refresh",
                target.path.display()
            ),
        )
    })
}

pub struct TopArgs {
    pub by: RankBy,
    pub kind: KindFilter,
    pub limit: usize,
    pub include_system: bool,
    pub columns: Vec<Column>,
    pub explain: bool,
    /// Answer from this report (scan job ID) instead of the newest one.
    pub report: Option<String>,
}

pub fn top(
    targets: TargetArgs,
    args: TopArgs,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let (measure, ranked_by) = match args.by {
        RankBy::Logical => (Measure::Logical, "logical"),
        RankBy::Physical => (Measure::Physical, "physical"),
        RankBy::Reclaimable => (Measure::Reclaimable, "reclaimable"),
        RankBy::Shared => (Measure::Shared, "shared"),
        RankBy::Held => (Measure::Held, "held"),
    };
    let rank = RankArgs {
        by: measure,
        kind: args.kind,
        limit: args.limit,
        include_system: args.include_system,
        only_shared: false,
        services: true,
        report: args.report.clone(),
    };
    let mut envelope = Envelope::new("top", SystemTime::now());
    let (ranked, scans) = rank_targets(
        &targets,
        &rank,
        answer,
        output,
        limits,
        state_dir,
        &mut envelope,
    )?;
    let results = ranked
        .into_iter()
        .map(
            |(report, ranking, services, entries, package_overlaps)| TopResult {
                report,
                target: EntryJson::from(&ranking.target),
                entries,
                hidden_system: ranking.hidden_system,
                services: ServicesJson::from(&services),
                package_overlaps,
            },
        )
        .map(TopResult::with_filesystem)
        .collect();
    let doc = TopDocument {
        envelope,
        requested_by: ranked_by,
        ranked_by,
        kind: kind_name(args.kind),
        limit: args.limit,
        include_system: args.include_system,
        results,
        scans,
        columns: args.columns,
        explain: args.explain,
    };
    emit(output.json, &doc, || render_top(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

/// Reconciles a volume's extent attribution with Btrfs SPACE_INFO and df.
pub fn overview(
    targets: TargetArgs,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let doc = overview_document(targets, answer, output, limits, state_dir)?;
    emit(output.json, &doc, || render_overview(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

/// Builds the same report-backed document as the overview command without
/// choosing an output format, so embedded callers use its accounting.
pub fn overview_document(
    targets: TargetArgs,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
) -> Result<OverviewDocument, Failure> {
    let env = Environment::load()?;
    let (resolved, mut errors) = resolve_targets(&env, &targets)?;
    let mut volume_targets = Vec::new();
    for target in resolved {
        if target.scope == Scope::Volume {
            volume_targets.push(target);
        } else {
            errors.push(TargetError::new(
                JsonPath::new(target.path.as_os_str().as_bytes()),
                "not_a_volume",
                "overview requires a whole supported volume",
            ));
        }
    }
    let mut answerer = Answerer {
        env: &env,
        limits,
        state_dir,
        answer,
        output,
        scans: Vec::new(),
        errors,
        hints: Vec::new(),
        job_id: None,
        report: None,
    };
    let prepared = answerer.prepare(volume_targets)?;
    let mut ready = Vec::new();
    for (target, _) in prepared {
        match newest_overview_report(&answerer, &target)? {
            Some(path) => ready.push((target, path)),
            None => answerer.refuse(
                &target,
                "no complete volume report contains overview and space data; run `synoreclaim scan`",
            )?,
        }
    }
    let now = SystemTime::now();
    let mut envelope = Envelope::new("overview", now);
    let mut results = Vec::new();
    for (target, path) in ready {
        let summary = crate::report_summary::ensure(&path)?;
        let header = summary.header;
        let result = if !header.roots.iter().any(|root| root.scope == Scope::Volume) {
            None
        } else if let (Some(data), Some(space)) = (&header.allocated_overview, &header.vfs_space) {
            let info = answered_report(&path, &header, now, answer.max_age);
            stale_warning(&mut envelope, &info);
            Some(build_allocation_overview(&header, data, space, info))
        } else if let (Some(data), Some(space)) = (&header.overview, &header.space) {
            let info = answered_report(&path, &header, now, answer.max_age);
            stale_warning(&mut envelope, &info);
            unresolved_warning(&mut envelope, &header);

            Some(build_overview_result(
                &header,
                data,
                space,
                info,
                summary.cleanup,
            ))
        } else {
            None
        };
        match result {
            Some(result) => results.push(result),
            None => answerer.refuse(
                &target,
                "the covering report predates volume overview data; run `synoreclaim scan`",
            )?,
        }
    }

    answerer.close(&mut envelope, !results.is_empty());
    if envelope.completion == Completion::Complete
        && results.iter().any(overview_result_needs_partial)
    {
        envelope.completion = Completion::Partial;
    }
    let doc = OverviewDocument {
        envelope,
        results,
        scans: std::mem::take(&mut answerer.scans),
    };
    Ok(doc)
}

fn overview_needs_partial(data: &DataReconciliation, df: &DfReconciliation) -> bool {
    data.coverage_gaps || data.status == "unknown" || df.status == "unknown"
}

fn overview_result_needs_partial(result: &OverviewResult) -> bool {
    overview_needs_partial(&result.reconciliation.data, &result.reconciliation.df)
        && (result.report.coverage.approximation.is_none()
            || result.reconciliation.data.status == "unknown"
            // Unknown copy factors/reserve rows are display uncertainty, not
            // a failed read. Missing observations remain partial.
            || result.reconciliation.df.used_bytes.is_none())
}

fn newest_overview_report(
    answerer: &Answerer<'_>,
    target: &Resolved,
) -> Result<Option<PathBuf>, Failure> {
    let store = answerer.store()?;
    let reports = store.display_reports(&fsid_text(&target.fsid))?;
    let mount = target.path.as_os_str().as_bytes();
    Ok(reports
        .reports
        .into_iter()
        .find(|(_, header)| {
            query::covers(header, mount)
                && header.roots.iter().any(|root| root.scope == Scope::Volume)
                && ((header.overview.is_some() && header.space.is_some())
                    || (header.allocated_overview.is_some() && header.vfs_space.is_some()))
        })
        .map(|(path, _)| path))
}

fn build_overview_result(
    header: &Header,
    data: &OverviewData,
    window: &SpaceWindow,
    info: ReportInfo,
    cleanup: Vec<CleanupCandidate>,
) -> OverviewResult {
    let coverage = &header.coverage;
    let coverage_gap = coverage.reference_accounting != "complete"
        || coverage.encrypted_directories > 0
        || coverage.dangling_entries > 0
        || coverage.unsupported_locations > 0
        || coverage.missing_subvolume_roots > 0
        || header.consistency == crate::report::Consistency::Partial
        || data.unknown_extents > 0
        || overview_parts_sum(data) != data.extent_bytes;
    let (space, data_space, df_space) = overview_space(window, data, coverage_gap);
    OverviewResult {
        report: info,
        filesystem: OverviewFilesystem {
            uuid: header.fsid.clone(),
            mount: JsonPath::new(&header.mount),
            kind: header.filesystem,
            capabilities: header.capabilities.clone(),
        },
        space,
        parts: overview_parts(data),
        reconciliation: OverviewReconciliation {
            data: data_space,
            df: df_space,
        },
        actions: overview_actions(&header.mount, data, cleanup),
        attribution: header
            .stats
            .pinned_read_plan
            .as_ref()
            .map(|plan| AttributionInfo {
                read_budget: Some(plan.clone()),
                deferred_bytes: data
                    .pinned_ranges_subcases
                    .get(&crate::overview::PinnedRangesSubcase::ReadBudgetExhausted)
                    .copied()
                    .unwrap_or(0),
                analysis: data.deep_analysis.clone(),
            }),
    }
}

/// st_blocks describes inode allocations, not all blocks counted by statvfs.
/// The difference includes filesystem metadata, journals and unlinked files;
/// it is deliberately neither a reclaim estimate nor a fabricated DATA group.
fn build_allocation_overview(
    header: &Header,
    data: &crate::model::tree::AllocationOverview,
    space: &FsSpace,
    info: ReportInfo,
) -> OverviewResult {
    let mut parts = BTreeMap::<PartKind, OverviewPart>::new();
    for row in &data.items {
        let part = parts.entry(row.kind).or_insert_with(|| OverviewPart {
            kind: row.kind,
            bytes: 0,
            reclaimable_bytes: matches!(
                row.kind,
                PartKind::ShareLive | PartKind::RecycleBin | PartKind::SharedBetweenFolders
            )
            .then_some(0),
            held_bytes: None,
            items: Vec::new(),
            reasons: Vec::new(),
        });
        part.bytes = part.bytes.saturating_add(row.bytes);
        if let Some(bytes) = &mut part.reclaimable_bytes {
            *bytes = bytes.saturating_add(row.reclaimable_bytes);
        }
        let item = if let Some(service) = row.service {
            service_overview_item(service, row.bytes)
        } else {
            let name = report::hex_bytes::decode(&row.name_hex).unwrap_or_default();
            let share = matches!(row.kind, PartKind::ShareLive | PartKind::RecycleBin);
            OverviewItem {
                label: (!share && !name.is_empty()).then(|| crate::output::escape_human(&name)),
                share: share.then(|| crate::output::escape_human(&name)),
                share_hex: share.then(|| row.name_hex.clone()),
                service: None,
                managed_by: None,
                manage_in: None,
                manage_in_id: None,
                explanation: None,
                bytes: row.bytes,
                reclaimable_bytes: part.reclaimable_bytes.map(|_| row.reclaimable_bytes),
            }
        };
        part.items.push(item);
    }
    let allocated = data
        .items
        .iter()
        .map(|row| row.bytes)
        .fold(0, u64::saturating_add);
    let df = df_space(space);
    let gaps = header.consistency == report::Consistency::Partial
        || header.coverage.dangling_entries > 0
        || header.coverage.encrypted_directories > 0;
    OverviewResult {
        report: info,
        filesystem: OverviewFilesystem {
            uuid: header.fsid.clone(),
            mount: JsonPath::new(&header.mount),
            kind: header.filesystem,
            capabilities: header.capabilities.clone(),
        },
        space: OverviewSpace {
            df: Some(df),
            space_info: None,
            space_info_error: None,
            inode_allocated_bytes: Some(allocated),
            unattributed_df_bytes: Some(signed_delta(df.used_bytes, allocated)),
        },
        parts: parts.into_values().collect(),
        reconciliation: OverviewReconciliation {
            data: DataReconciliation {
                parts_sum_bytes: allocated,
                space_info_used_bytes: None,
                residual_bytes: None,
                tolerance_bytes: None,
                status: "inode_allocations",
                coverage_gaps: gaps,
            },
            df: DfReconciliation {
                used_bytes: Some(df.used_bytes),
                metadata_system_bytes: None,
                data_metadata_system_bytes: None,
                residual_bytes: Some(signed_delta(df.used_bytes, allocated)),
                status: "display_only",
            },
        },
        actions: Vec::new(),
        attribution: None,
    }
}

fn overview_parts(data: &OverviewData) -> Vec<OverviewPart> {
    let pending = data.pending_cleanup_bytes;
    let snapshot_total = data
        .snapshot_only_bytes
        .values()
        .copied()
        .fold(data.snapshot_multi_share_bytes, u64::saturating_add);
    let snapshots = data
        .snapshot_only_bytes
        .iter()
        .map(|(&share, &bytes)| share_item(data, share, bytes, None))
        .chain((data.snapshot_multi_share_bytes > 0).then(|| OverviewItem {
            label: Some("multiple source shared folders".into()),
            share: None,
            share_hex: None,
            service: None,
            managed_by: None,
            manage_in: None,
            manage_in_id: None,
            explanation: None,
            bytes: data.snapshot_multi_share_bytes,
            reclaimable_bytes: None,
        }))
        .collect();
    let recycle_bytes = sum_values(data.recycle_bin.values().map(|row| row.bytes));
    let recycle_reclaimable =
        sum_values(data.recycle_bin.values().map(|row| row.reclaimable_bytes));
    let recycle = data
        .recycle_bin
        .iter()
        .map(|(&share, row)| share_item(data, share, row.bytes, Some(row.reclaimable_bytes)))
        .collect();
    let services = data
        .dsm_services
        .iter()
        .map(|(&id, &bytes)| service_overview_item(id, bytes))
        .collect::<Vec<_>>();
    let service_bytes = sum_values(data.dsm_services.values().copied());
    let share_bytes = sum_values(data.share_live.values().map(|row| row.bytes));
    let share_reclaimable = sum_values(data.share_live.values().map(|row| row.reclaimable_bytes));
    let shares = data
        .share_live
        .iter()
        .map(|(&share, row)| share_item(data, share, row.bytes, Some(row.reclaimable_bytes)))
        .collect();
    let unknown_bytes = sum_values(data.unknown.values().copied());
    let reasons = data
        .unknown
        .iter()
        .map(|(&code, &bytes)| OverviewReason {
            code,
            bytes,
            subcases: match code {
                crate::overview::UnknownReason::DirectOwnerUnresolved => data
                    .direct_owner_subcases
                    .iter()
                    .map(|(&code, summary)| OverviewReasonSubcase {
                        code: SubcaseCode::DirectOwner(code),
                        bytes: summary.bytes,
                        roots: summary
                            .roots
                            .iter()
                            .map(|(&root_id, root)| OverviewReasonRoot {
                                root_id,
                                kind: root.kind,
                                class: root.class,
                                at_directory: root.at_directory,
                                bytes: root.bytes,
                            })
                            .collect(),
                    })
                    .collect(),
                crate::overview::UnknownReason::PinnedRangesUnknown => data
                    .pinned_ranges_subcases
                    .iter()
                    .map(|(&code, &bytes)| OverviewReasonSubcase {
                        code: SubcaseCode::PinnedRanges(code),
                        bytes,
                        roots: Vec::new(),
                    })
                    .collect(),
                _ => Vec::new(),
            },
        })
        .collect();
    vec![
        OverviewPart {
            kind: PartKind::PendingCleanup,
            bytes: pending,
            reclaimable_bytes: None,
            held_bytes: Some(pending),
            items: Vec::new(),
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::SnapshotOnly,
            bytes: snapshot_total,
            reclaimable_bytes: None,
            held_bytes: Some(snapshot_total),
            items: snapshots,
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::RecycleBin,
            bytes: recycle_bytes,
            reclaimable_bytes: Some(recycle_reclaimable),
            held_bytes: None,
            items: recycle,
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::DsmService,
            bytes: service_bytes,
            reclaimable_bytes: None,
            held_bytes: Some(service_bytes),
            items: services,
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::SystemOther,
            bytes: data.system_other_bytes,
            reclaimable_bytes: None,
            held_bytes: None,
            items: Vec::new(),
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::ShareLive,
            bytes: share_bytes,
            reclaimable_bytes: Some(share_reclaimable),
            held_bytes: None,
            items: shares,
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::SharedBetweenFolders,
            bytes: data.shared_between_folders_bytes,
            reclaimable_bytes: None,
            held_bytes: Some(data.shared_between_folders_bytes),
            items: Vec::new(),
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::PinnedUnreferenced,
            bytes: data.pinned_unreferenced_bytes,
            reclaimable_bytes: None,
            held_bytes: None,
            items: Vec::new(),
            reasons: Vec::new(),
        },
        OverviewPart {
            kind: PartKind::Unknown,
            bytes: unknown_bytes,
            reclaimable_bytes: None,
            held_bytes: None,
            items: Vec::new(),
            reasons,
        },
    ]
}

fn share_item(
    data: &OverviewData,
    id: u64,
    bytes: u64,
    reclaimable_bytes: Option<u64>,
) -> OverviewItem {
    let encoded = data.share_names_hex.get(&id);
    let name_bytes = encoded.and_then(|hex| crate::report::hex_bytes::decode(hex));
    let share = name_bytes
        .as_deref()
        .map(String::from_utf8_lossy)
        .map(|value| value.into_owned());
    let share_hex = name_bytes
        .as_deref()
        .filter(|bytes| std::str::from_utf8(bytes).is_err())
        .map(crate::report::hex_bytes::encode);
    OverviewItem {
        label: None,
        share,
        share_hex,
        service: None,
        managed_by: None,
        manage_in: None,
        manage_in_id: None,
        explanation: None,
        bytes,
        reclaimable_bytes,
    }
}

fn service_overview_item(id: u16, bytes: u64) -> OverviewItem {
    let known = crate::dsm::service_by_id(id);
    OverviewItem {
        label: None,
        share: None,
        share_hex: None,
        service: Some(known.map_or("other_dsm_service_data", |service| service.code)),
        managed_by: Some(known.map_or("DSM or a package", |service| service.package)),
        manage_in: Some(known.map_or(crate::dsm::OWNING_PACKAGE, |service| service.manage_in)),
        manage_in_id: Some(known.map_or(
            crate::dsm::place_id(crate::dsm::OWNING_PACKAGE),
            |service| service.manage_in_id(),
        )),
        explanation: known.and_then(|service| service.explanation),
        bytes,
        reclaimable_bytes: None,
    }
}

fn overview_space(
    window: &SpaceWindow,
    data: &OverviewData,
    coverage_gap: bool,
) -> (OverviewSpace, DataReconciliation, DfReconciliation) {
    let end = window.end.as_ref();
    let start_data_used = window
        .start
        .as_ref()
        .and_then(|snapshot| snapshot.block_groups.as_ref().ok())
        .map(|groups| groups_data_used(groups));
    let groups = end.and_then(|snapshot| snapshot.block_groups.as_ref().ok());
    let end_data_used = groups.map(|rows| groups_data_used(rows));
    let df = end
        .and_then(|snapshot| snapshot.statfs.as_ref().ok())
        .map(df_space);
    let start_by_flags = window
        .start
        .as_ref()
        .and_then(|snapshot| snapshot.block_groups.as_ref().ok())
        .map(|rows| {
            let mut values = BTreeMap::new();
            for row in rows {
                let value = values.entry(row.flags).or_insert(0u64);
                *value = value.saturating_add(row.used_bytes);
            }
            values
        })
        .unwrap_or_default();
    let rows = groups.map_or_else(Vec::new, |groups| {
        groups
            .iter()
            .map(|row| {
                let (kind, _) = space_type(row.flags);
                let (profile, copies) = profile(row.flags);
                OverviewSpaceInfo {
                    kind,
                    profile,
                    copies,
                    total_bytes: row.total_bytes,
                    used_bytes: row.used_bytes,
                    captured: "scan_end",
                    used_bytes_at_scan_start: (kind == "data")
                        .then(|| start_by_flags.get(&row.flags).copied())
                        .flatten(),
                }
            })
            .collect()
    });
    let end_error = end.and_then(|snapshot| snapshot.block_groups.as_ref().err().cloned());
    let start_error = window
        .start
        .as_ref()
        .and_then(|snapshot| snapshot.block_groups.as_ref().err().cloned())
        .map(|error| format!("scan start: {error}"))
        .or_else(|| {
            window
                .start
                .is_none()
                .then(|| "scan start space data was not captured".into())
        });
    let space_info_error = end_error.or(start_error).or_else(|| {
        end.is_none()
            .then(|| "scan end space data was not captured".into())
    });
    let parts_sum = overview_parts_sum(data);
    let residual = end_data_used.map(|used| signed_delta(parts_sum, used));
    let tolerance = end_data_used.zip(start_data_used).map(|(used, start)| {
        (1u64 << 30)
            .max(used / 1000)
            .saturating_add(start.abs_diff(used))
    });
    let data_status = match (residual, tolerance) {
        (Some(_), _) if coverage_gap => "not_reconciled",
        (Some(residual), Some(tolerance)) if residual.unsigned_abs() <= tolerance => "reconciled",
        (Some(_), Some(_)) => "not_reconciled",
        _ => "unknown",
    };
    let data_reconciliation = DataReconciliation {
        parts_sum_bytes: parts_sum,
        space_info_used_bytes: end_data_used,
        residual_bytes: residual,
        tolerance_bytes: tolerance,
        status: if groups.is_none() {
            "unknown"
        } else {
            data_status
        },
        coverage_gaps: coverage_gap,
    };
    let metadata_system = groups.and_then(|rows| groups_metadata_system_used(rows));
    let calculated = groups.and_then(|rows| groups_df_used(rows));
    let df_used = df.map(|space| space.used_bytes);
    let df_reconciliation = DfReconciliation {
        used_bytes: df_used,
        metadata_system_bytes: metadata_system,
        data_metadata_system_bytes: calculated,
        residual_bytes: df_used
            .zip(calculated)
            .map(|(used, value)| signed_delta(used, value)),
        status: if df_used.is_some() && calculated.is_some() {
            "display_only"
        } else {
            "unknown"
        },
    };
    (
        OverviewSpace {
            df,
            space_info: Some(rows),
            inode_allocated_bytes: None,
            unattributed_df_bytes: None,
            space_info_error,
        },
        data_reconciliation,
        df_reconciliation,
    )
}

fn df_space(space: &FsSpace) -> OverviewDf {
    OverviewDf {
        total_bytes: space.total_bytes,
        used_bytes: space.total_bytes.saturating_sub(space.free_bytes),
        free_bytes: space.free_bytes,
        available_bytes: space.available_bytes,
    }
}

fn groups_data_used(groups: &[BlockGroupSpace]) -> u64 {
    crate::watch_free::data_used(groups)
}

fn groups_df_used(groups: &[BlockGroupSpace]) -> Option<u64> {
    groups.iter().try_fold(0u64, |used, row| {
        let (kind, _) = space_type(row.flags);
        if !matches!(kind, "data" | "metadata" | "system") {
            return (kind != "unknown" || row.used_bytes == 0).then_some(used);
        }
        let copies = profile(row.flags).1?;
        used.checked_add(row.used_bytes.checked_mul(u64::from(copies))?)
    })
}

fn groups_metadata_system_used(groups: &[BlockGroupSpace]) -> Option<u64> {
    groups.iter().try_fold(0u64, |used, row| {
        let (kind, _) = space_type(row.flags);
        if !matches!(kind, "metadata" | "system") {
            return (kind != "unknown" || row.used_bytes == 0).then_some(used);
        }
        let copies = profile(row.flags).1?;
        used.checked_add(row.used_bytes.checked_mul(u64::from(copies))?)
    })
}

fn space_type(flags: u64) -> (&'static str, u64) {
    match flags & 0x7 {
        1 => ("data", flags & !0x7),
        2 => ("system", flags & !0x7),
        4 => ("metadata", flags & !0x7),
        _ => ("unknown", flags & !0x7),
    }
}

fn profile(flags: u64) -> (&'static str, Option<u8>) {
    let profile = flags & 0x7f8;
    match profile {
        0 => ("single", Some(1)),
        8 => ("raid0", Some(1)),
        16 => ("raid1", Some(2)),
        32 => ("dup", Some(2)),
        64 => ("raid10", Some(2)),
        128 => ("raid5", None),
        256 => ("raid6", None),
        512 => ("raid1c3", Some(3)),
        1024 => ("raid1c4", Some(4)),
        _ => ("unknown", None),
    }
}

fn sum_values(values: impl Iterator<Item = u64>) -> u64 {
    values.fold(0u64, u64::saturating_add)
}

fn overview_parts_sum(data: &OverviewData) -> u64 {
    [
        data.pending_cleanup_bytes,
        data.snapshot_only_bytes
            .values()
            .copied()
            .fold(data.snapshot_multi_share_bytes, u64::saturating_add),
        sum_values(data.recycle_bin.values().map(|row| row.bytes)),
        sum_values(data.dsm_services.values().copied()),
        sum_values(data.share_live.values().map(|row| row.bytes)),
        data.shared_between_folders_bytes,
        data.system_other_bytes,
        data.pinned_unreferenced_bytes,
        sum_values(data.unknown.values().copied()),
    ]
    .into_iter()
    .fold(0u64, u64::saturating_add)
}

/// A share's top-level entry `cleanup delete` could free: its share,
/// path, reclaimable bytes and whether it is a directory.
pub(crate) type CleanupCandidate = (u64, Vec<u8>, u64, bool);

/// Finds the entries at the top of each share that overview suggests
/// deleting, in one walk of records fed in preorder.
pub(crate) struct CleanupCandidates<'a> {
    mount: &'a [u8],
    data: &'a OverviewData,
    share_ids: BTreeMap<Vec<u8>, u64>,
    found: Vec<CleanupCandidate>,
}

impl<'a> CleanupCandidates<'a> {
    pub(crate) fn new(mount: &'a [u8], data: &'a OverviewData) -> Self {
        let share_ids = data
            .share_names_hex
            .iter()
            .filter_map(|(&id, hex)| crate::report::hex_bytes::decode(hex).map(|name| (name, id)))
            .collect();
        CleanupCandidates {
            mount,
            data,
            share_ids,
            found: Vec::new(),
        }
    }

    /// Feeds one record.
    pub(crate) fn visit(
        &mut self,
        path: &[u8],
        record: &crate::report::NodeRecord<'_>,
    ) -> ControlFlow<()> {
        let mount = self.mount;
        let Some(relative) = path
            .strip_prefix(mount)
            .and_then(|suffix| suffix.strip_prefix(b"/"))
        else {
            return ControlFlow::Continue(());
        };
        let Some(separator) = relative.iter().position(|byte| *byte == b'/') else {
            return ControlFlow::Continue(());
        };
        let (share_name, child_with_slash) = relative.split_at(separator);
        let child = &child_with_slash[1..];
        if child.is_empty() || child.contains(&b'/') || child == b"#recycle" {
            return ControlFlow::Continue(());
        }
        let Some(&share) = self.share_ids.get(share_name) else {
            return ControlFlow::Continue(());
        };
        let Some(share_data) = self.data.share_live.get(&share) else {
            return ControlFlow::Continue(());
        };
        let reclaimable = record.metrics.reclaimable;
        if reclaimable == 0
            || record.metrics.unknown > 0
            || reclaimable > share_data.reclaimable_bytes
            || record.flags.contains(NodeFlags::SYSTEM)
            || crate::dsm::package_data_of(path, mount).is_some()
        {
            return ControlFlow::Continue(());
        }
        let directory = record.kind == crate::model::namespace::FileKind::Directory;
        self.found
            .push((share, path.to_vec(), reclaimable, directory));
        ControlFlow::Continue(())
    }

    /// In preorder.
    pub(crate) fn finish(self) -> Vec<CleanupCandidate> {
        self.found
    }
}

fn overview_actions(
    mount: &[u8],
    data: &OverviewData,
    cleanup: Vec<CleanupCandidate>,
) -> Vec<OverviewAction> {
    let mut candidates: Vec<(OverviewAction, u64)> = Vec::new();
    for (&share, row) in &data.recycle_bin {
        if row.reclaimable_bytes > 0 {
            candidates.push((
                share_action(
                    data,
                    share,
                    "recycle",
                    row.reclaimable_bytes,
                    None,
                    crate::dsm::service(b"#recycle", crate::dsm::Depth::Below)
                        .map(|service| service.manage_in),
                    None,
                ),
                row.reclaimable_bytes,
            ));
        }
    }
    for (share, path, bytes, directory) in cleanup {
        let command = String::from_utf8(path.clone()).ok().map(|path| {
            vec![
                "synoreclaim".into(),
                "cleanup".into(),
                "delete".into(),
                path,
            ]
        });
        let mut action = share_action(
            data,
            share,
            "cleanup",
            bytes,
            None,
            Some(crate::dsm::FILE_STATION),
            Some((JsonPath::new(&path), command)),
        );
        action.path_kind = Some(if directory { "dir" } else { "file" });
        candidates.push((action, bytes));
    }
    for (&share, &bytes) in &data.snapshot_only_bytes {
        if bytes > 0 {
            let mut action = share_action(
                data,
                share,
                "snapshot",
                0,
                Some(bytes),
                Some(crate::dsm::SNAPSHOT_REPLICATION_SHORT),
                None,
            );
            action.command = share_path(data, share, mount)
                .and_then(|path| String::from_utf8(path).ok())
                .map(|path| vec!["synoreclaim".into(), "snapshots".into(), path]);
            candidates.push((action, bytes));
        }
    }
    if data.snapshot_multi_share_bytes > 0 {
        let command = String::from_utf8(mount.to_vec())
            .ok()
            .map(|path| vec!["synoreclaim".into(), "snapshots".into(), path]);
        candidates.push((
            OverviewAction {
                rank: 0,
                kind: "snapshot",
                share: None,
                share_hex: None,
                service: None,
                reclaimable_bytes: None,
                held_bytes: Some(data.snapshot_multi_share_bytes),
                manage_in: Some(crate::dsm::SNAPSHOT_REPLICATION_SHORT),
                manage_in_id: Some(crate::dsm::place_id(crate::dsm::SNAPSHOT_REPLICATION_SHORT)),
                path: None,
                path_kind: None,
                command,
            },
            data.snapshot_multi_share_bytes,
        ));
    }
    for (&service, &bytes) in &data.dsm_services {
        if bytes == 0 {
            continue;
        }
        let metadata = crate::dsm::service_by_id(service);
        candidates.push((
            OverviewAction {
                rank: 0,
                kind: "package",
                share: None,
                share_hex: None,
                service: Some(metadata.map_or("other_dsm_service_data", |s| s.code)),
                reclaimable_bytes: None,
                held_bytes: Some(bytes),
                manage_in: Some(metadata.map_or(crate::dsm::OWNING_PACKAGE, |s| s.manage_in)),
                manage_in_id: Some(
                    metadata.map_or(crate::dsm::place_id(crate::dsm::OWNING_PACKAGE), |s| {
                        s.manage_in_id()
                    }),
                ),
                path: None,
                path_kind: None,
                command: None,
            },
            bytes,
        ));
    }
    if data.pending_cleanup_bytes > 0 {
        candidates.push((
            OverviewAction {
                rank: 0,
                kind: "wait",
                share: None,
                share_hex: None,
                service: None,
                reclaimable_bytes: None,
                held_bytes: Some(data.pending_cleanup_bytes),
                manage_in: Some(crate::dsm::BTRFS_CLEANUP),
                manage_in_id: Some(crate::dsm::place_id(crate::dsm::BTRFS_CLEANUP)),
                path: None,
                path_kind: None,
                command: None,
            },
            data.pending_cleanup_bytes,
        ));
    }
    candidates.sort_by(|(a, a_bytes), (b, b_bytes)| {
        let a_reclaimable = a.reclaimable_bytes.is_some();
        let b_reclaimable = b.reclaimable_bytes.is_some();
        b_reclaimable
            .cmp(&a_reclaimable)
            .then_with(|| b_bytes.cmp(a_bytes))
            .then_with(|| a.kind.cmp(b.kind))
    });
    candidates
        .into_iter()
        .take(5)
        .enumerate()
        .map(|(rank, (mut action, _))| {
            action.rank = rank + 1;
            action
        })
        .collect()
}

fn share_action(
    data: &OverviewData,
    share: u64,
    kind: &'static str,
    reclaimable_bytes: u64,
    held_bytes: Option<u64>,
    manage_in: Option<&'static str>,
    path_command: Option<(JsonPath, Option<Vec<String>>)>,
) -> OverviewAction {
    let item = share_item(data, share, 0, None);
    OverviewAction {
        rank: 0,
        kind,
        share: item.share,
        share_hex: item.share_hex,
        service: None,
        reclaimable_bytes: (reclaimable_bytes > 0).then_some(reclaimable_bytes),
        held_bytes,
        manage_in,
        manage_in_id: manage_in.map(crate::dsm::place_id),
        path: path_command.as_ref().map(|(path, _)| path.clone()),
        path_kind: None,
        command: path_command.and_then(|(_, command)| command),
    }
}

fn share_path(data: &OverviewData, share: u64, mount: &[u8]) -> Option<Vec<u8>> {
    let name = crate::report::hex_bytes::decode(data.share_names_hex.get(&share)?)?;
    let mut path = mount.to_vec();
    if !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(&name);
    Some(path)
}

#[cfg(test)]
mod overview_tests {
    use super::*;
    use crate::report::Coverage;

    fn approximate_header() -> Header {
        let mut header = crate::report::testing::header(Vec::new());
        header.coverage.approximation = Some(crate::report::Approximation {
            changed_subvolumes: 1,
            skipped_roots: 0,
            unaccounted_bytes: 16,
        });
        header.coverage.reference_accounting = "partial".into();
        header.accounting.unknown_extents = 1;
        header.totals.unknown = 16;
        header
    }

    #[test]
    fn detached_resume_refuses_missing_checkpoints_before_queueing_or_changing_the_job() {
        let state = crate::spill::testing::scratch("missing-resume-checkpoint").join("state");
        let jobs = state.join("jobs");
        let job = Job::new(
            "scan-one",
            &[b"/volume1".to_vec()],
            Some(Scope::Volume),
            &ScanLimits::report_only(),
            100,
        );
        Live::start(&jobs, sys::effective_uid(), job)
            .unwrap()
            .finish(JobState::Paused, Some("paused"), 200)
            .unwrap();
        let before = jobs::load(&jobs, "scan-one").unwrap();
        let error = detach_resume("scan-one", None, Launcher::External, &state).unwrap_err();
        let error = error.downcast_ref::<jobs::JobError>().unwrap();
        assert_eq!(error.code(), Some("checkpoint_unusable"));
        assert!(error.to_string().contains("start a fresh scan"));
        assert_eq!(jobs::load(&jobs, "scan-one").unwrap(), before);
        assert!(!state.join("launch").exists());
    }

    #[test]
    fn fresh_recovery_retains_saved_targets_and_every_limit_even_with_an_unusable_checkpoint() {
        let state = crate::spill::testing::scratch("fresh-saved-settings").join("state");
        let jobs = state.join("jobs");
        let mut limits = ScanLimits::report_only();
        limits.profile = resource::Profile::Fast;
        limits.snapshots = Some(crate::snapshots::Depth::Newest(3));
        limits.max_cache = 123456;
        limits.min_free = 7890;
        limits.no_cache = true;
        let mut original = Job::new(
            "saved-scan",
            &[b"/volume1/one".to_vec(), b"/volume1/two".to_vec()],
            Some(Scope::Path),
            &limits,
            100,
        );
        original.versions.tool_version = "0.0.0".into();
        Live::start(&jobs, sys::effective_uid(), original)
            .unwrap()
            .finish(JobState::Paused, Some("paused"), 200)
            .unwrap();
        let corrupt = jobs.join("saved-scan/checkpoint");
        std::fs::create_dir_all(&corrupt).unwrap();
        std::fs::write(corrupt.join("namespace.json"), b"damaged").unwrap();
        std::fs::create_dir_all(state.join("reports")).unwrap();
        std::fs::write(state.join("reports/retained.json"), b"retained report").unwrap();
        let source = fresh_source(&state, "saved-scan").unwrap();
        assert_eq!(
            source.operands(),
            [b"/volume1/one".to_vec(), b"/volume1/two".to_vec()]
        );
        assert_eq!(source.scope, Some(Scope::Path));
        assert_eq!(source.limits.scan_limits(), limits);
        assert_eq!(
            jobs::load(&jobs, "saved-scan").unwrap().state,
            JobState::Cancelled
        );
        assert!(
            !corrupt.exists(),
            "explicit fresh recovery cancels unfinished work"
        );
        assert_eq!(
            std::fs::read(state.join("reports/retained.json")).unwrap(),
            b"retained report"
        );
    }

    #[test]
    fn resumed_worker_retains_phase_counters_elapsed_and_flushes_on_pause() {
        let state = crate::spill::testing::scratch("resume-progress");
        let jobs = state.join("jobs");
        let limits = ScanLimits::report_only();
        let live = RefCell::new(
            Live::start(
                &jobs,
                sys::effective_uid(),
                Job::new(
                    "resume-progress",
                    &[b"/volume1".to_vec()],
                    Some(Scope::Volume),
                    &limits,
                    100,
                ),
            )
            .unwrap(),
        );
        let previous = JobProgress {
            phase: "scan".into(),
            units_done: 3,
            entries: 900,
            items: 1500,
            bytes_read: 12000,
            elapsed_s: 120,
            ..JobProgress::default()
        };
        let now = Instant::now();
        let mut control = Control {
            pacer: Pacer::new(100, SystemClock::default(), None),
            disk: DiskWatch::new(
                state.clone(),
                state.clone(),
                0,
                0,
                Duration::from_secs(1),
                Box::new(space::available),
            ),
            job: Some(&live),
            poll: Poll::new(Duration::from_secs(1), now),
            progress_poll: Poll::new(Duration::from_secs(1000), now),
            quiet: true,
            progress: false,
            fsid: "fs".into(),
            started: now,
            elapsed_before: 120,
            progress_floor: Some(previous),
            new_work: WalkStats::default(),
            restart: None,
            last_line: now,
            phase: "scan",
            tree: 5,
            subvolume: None,
            volume: true,
            writable: Some(500),
            units: 0,
            entries: EntryCount::default(),
            items: 0,
            bytes: 0,
            previous: None,
            deletion: None,
        };
        control.phase("discover");
        control.progress(true);
        let saved = crate::jobs::load(&jobs, "resume-progress")
            .unwrap()
            .progress
            .unwrap();
        assert_eq!(saved.phase, "scan");
        assert_eq!(
            (
                saved.entries,
                saved.items,
                saved.bytes_read,
                saved.units_done
            ),
            (900, 1500, 12000, 3)
        );
        assert!(saved.elapsed_s >= 120);
        control.entries.tree_read(900);
        control.took(&WalkStats {
            dir_index_items: 5,
            items: 1600,
            item_bytes: 13000,
            ..WalkStats::default()
        });
        control.progress(true);
        let saved = crate::jobs::load(&jobs, "resume-progress")
            .unwrap()
            .progress
            .unwrap();
        assert_eq!(
            (saved.entries, saved.items, saved.bytes_read),
            (905, 3100, 25000)
        );
        assert_eq!(saved.phase, "scan");
        assert!(saved.elapsed_s >= 120);

        control.event(ScanEvent::TreeStarted {
            tree: 256,
            name: b"share",
        });
        control.event(ScanEvent::UnitRestarted { tree: 256 });
        control.restored(&WalkStats {
            items: 10000,
            item_bytes: 80000,
            ..WalkStats::default()
        });
        control.took(&WalkStats {
            dir_index_items: 2,
            items: 8,
            item_bytes: 256,
            ..WalkStats::default()
        });
        control.progress(true);
        let restarted = jobs::load(&jobs, "resume-progress")
            .unwrap()
            .progress
            .unwrap();
        assert_eq!(
            (restarted.items, restarted.bytes_read),
            (3108, 25256),
            "replay does not double-count work"
        );
        assert_eq!(
            restarted.restart,
            Some(UnitRestartProgress {
                subvolume_id: 256,
                entries: 2,
                items: 8,
                bytes_read: 256
            })
        );

        // A snapshot-only update retains the volume scope but never reads
        // its namespace again, so its counts cannot use that scan's basis.
        let header = crate::report::testing::header(Vec::new());
        control.previous = Some(ScanRecord::of(&header, 0, None));
        assert!(control.basis().is_some());
        live.borrow_mut().job.limits.snapshot_report = Some("saved-scan".parse().unwrap());
        control.progress(true);
        assert!(
            crate::jobs::load(&jobs, "resume-progress")
                .unwrap()
                .progress
                .unwrap()
                .basis
                .is_none()
        );
    }

    #[test]
    fn approximate_overview_keeps_unknown_df_math_without_exit_three() {
        let header = approximate_header();
        let data = OverviewData::default();
        // DSM exposes a non-DATA reserve row: its df copy factor is unknown.
        let captured = sample(
            FsSpace {
                total_bytes: 1000,
                free_bytes: 900,
                available_bytes: 900,
            },
            vec![group(1, 100), group(0, 1)],
        );
        let window = SpaceWindow {
            start: Some(captured.clone()),
            end: Some(captured),
        };
        let info = ReportInfo::new(b"report", &header, SystemTime::now(), Duration::MAX);
        let mut result = build_overview_result(&header, &data, &window, info, Vec::new());
        assert_eq!(result.reconciliation.df.status, "unknown");
        assert_eq!(result.reconciliation.df.data_metadata_system_bytes, None);
        assert_eq!(result.reconciliation.df.used_bytes, Some(100));
        assert!(!overview_result_needs_partial(&result));
        result.reconciliation.df.used_bytes = None;
        assert!(overview_result_needs_partial(&result));
        result.reconciliation.df.used_bytes = Some(100);
        result.reconciliation.data.status = "unknown";
        assert!(overview_result_needs_partial(&result));
    }

    #[test]
    fn approximate_snapshot_listing_keeps_unknown_values_without_exit_three() {
        let mut header = approximate_header();
        let info = ReportInfo::new(b"report", &header, SystemTime::now(), Duration::MAX);
        let mut result = SnapshotsResult {
            snapshot_analysis: None,
            target: JsonPath::new(b"/volume1"),
            report: info,
            shares: vec![ShareJson {
                share: JsonPath::new(b"backup"),
                live_subvolume_id: 256,
                held_only_by_snapshots_bytes: 100,
                unknown_bytes: 16,
                snapshots_not_materialized: 1,
                snapshots: Vec::new(),
                pending_deletion_ids: Vec::new(),
                held_files: None,
                simulation: None,
                manage_in: MANAGE_IN,
                manage_in_id: "snapshot_replication",
            }],
        };
        assert!(result.shares[0].has_unknown());
        assert!(!snapshots_result_unknown(&result));
        assert_eq!(result.shares[0].held_only_by_snapshots_bytes, 100);
        assert_eq!(result.shares[0].unknown_bytes, 16);
        assert_eq!(result.shares[0].snapshots_not_materialized, 1);
        result.shares[0].simulation = Some(crate::snapshots::answer::SimulationJson {
            selector: "all".into(),
            selected_ids: vec![257],
            freed_bytes: 0,
            unknown_bytes: 16,
            not_materialized_ids: vec![257],
            upper_bound_bytes: None,
            exact: false,
            inexact_reasons: vec![crate::snapshots::answer::InexactReason::NotMaterialized],
            exact_command: None,
        });
        assert!(snapshots_result_unknown(&result));
        result.shares[0].simulation = None;
        header.coverage.approximation = None;
        result.report = ReportInfo::new(b"report", &header, SystemTime::now(), Duration::MAX);
        assert!(snapshots_result_unknown(&result));
    }

    #[test]
    fn approximate_reports_keep_unresolved_counts_out_of_warning_envelopes() {
        let mut header = approximate_header();
        let mut envelope = Envelope::new("overview", SystemTime::now());
        unresolved_warning(&mut envelope, &header);
        assert!(envelope.warnings.is_empty());
        assert_eq!(header.accounting.unknown_extents, 1);
        assert_eq!(header.totals.unknown, 16);
        header.coverage.approximation = None;
        unresolved_warning(&mut envelope, &header);
        assert_eq!(envelope.warnings[0].code, "unresolved_references");
    }

    fn group(flags: u64, used_bytes: u64) -> BlockGroupSpace {
        BlockGroupSpace {
            flags,
            total_bytes: 200,
            used_bytes,
        }
    }

    fn sample(
        statfs: FsSpace,
        groups: Vec<BlockGroupSpace>,
    ) -> crate::btrfs::source::SpaceSnapshot {
        crate::btrfs::source::SpaceSnapshot {
            statfs: Ok(statfs),
            block_groups: Ok(groups),
        }
    }

    #[test]
    fn a_cleanup_action_says_whether_it_deletes_a_file_or_a_folder() {
        let actions = overview_actions(
            b"/v",
            &OverviewData::default(),
            vec![
                (1, b"/v/share/disk.iso".to_vec(), 9, false),
                (1, b"/v/share/old".to_vec(), 5, true),
            ],
        );
        assert_eq!(
            actions.iter().map(|a| a.path_kind).collect::<Vec<_>>(),
            [Some("file"), Some("dir")]
        );
    }

    #[test]
    fn other_volume_data_reconciles_without_creating_a_cleanup_action() {
        let data = OverviewData {
            extent_bytes: 17,
            system_other_bytes: 17,
            ..OverviewData::default()
        };
        let parts = overview_parts(&data);
        let other = parts
            .iter()
            .find(|part| part.kind == PartKind::SystemOther)
            .unwrap();
        assert_eq!(overview_parts_sum(&data), 17);
        assert_eq!(parts.iter().map(|part| part.bytes).sum::<u64>(), 17);
        assert_eq!(other.reclaimable_bytes, None);
        assert_eq!(other.held_bytes, None);
        assert!(overview_actions(b"/v", &data, Vec::new()).is_empty());
    }

    #[test]
    fn display_attribution_keeps_scan_gaps_separate_from_unresolved_references() {
        let mut header = crate::report::testing::header(Vec::new());
        header.coverage.namespace_uncertain_subvolumes = Some(Vec::new());
        header.coverage.reference_accounting = "partial".into();
        header.coverage.unresolved_extents = 1;
        header.overview = Some(OverviewData {
            unknown: BTreeMap::from([(
                crate::overview::UnknownReason::SharedTreeOwnerUnresolved,
                10,
            )]),
            ..OverviewData::default()
        });
        header.space = Some(SpaceWindow {
            start: None,
            end: Some(sample(
                FsSpace {
                    total_bytes: 1200,
                    free_bytes: 200,
                    available_bytes: 100,
                },
                Vec::new(),
            )),
        });
        let evidence = crate::overview::Attribution::of(&header).unwrap();
        assert_eq!(evidence.unknown_bytes, 10);
        assert_eq!(evidence.used_bytes, Some(1000));
        assert!(!evidence.coverage_gaps);
        for coverage in [
            Coverage {
                namespace_uncertain_subvolumes: Some(vec![256]),
                ..header.coverage.clone()
            },
            Coverage {
                namespace_uncertain_subvolumes: None,
                ..header.coverage.clone()
            },
            Coverage {
                missing_subvolume_roots: 1,
                ..header.coverage.clone()
            },
            Coverage {
                dangling_entries: 1,
                ..header.coverage.clone()
            },
            Coverage {
                encrypted_directories: 1,
                ..header.coverage.clone()
            },
        ] {
            header.coverage = coverage;
            header.overview.as_mut().unwrap().unknown.clear();
            assert!(
                crate::overview::Attribution::of(&header)
                    .unwrap()
                    .coverage_gaps
            );
        }
    }

    #[test]
    fn data_reconciliation_uses_raw_bytes_and_start_end_change_tolerance() {
        let data = OverviewData {
            extent_bytes: 100,
            share_live: BTreeMap::from([(
                256,
                crate::overview::OwnerBytes {
                    bytes: 100,
                    reclaimable_bytes: 100,
                },
            )]),
            ..OverviewData::default()
        };
        let window = SpaceWindow {
            start: Some(sample(
                FsSpace {
                    total_bytes: 1000,
                    free_bytes: 900,
                    available_bytes: 850,
                },
                vec![group(1, 100)],
            )),
            end: Some(sample(
                FsSpace {
                    total_bytes: 1000,
                    free_bytes: 850,
                    available_bytes: 800,
                },
                vec![group(1, 110), group(4 | 32, 10), group(2 | 32, 10)],
            )),
        };

        let (space, data_check, df_check) = overview_space(&window, &data, false);

        assert_eq!(data_check.parts_sum_bytes, 100);
        assert_eq!(data_check.space_info_used_bytes, Some(110));
        assert_eq!(data_check.residual_bytes, Some(-10));
        assert_eq!(data_check.tolerance_bytes, Some((1 << 30) + 10));
        assert_eq!(data_check.status, "reconciled");
        assert_eq!(space.df.unwrap().used_bytes, 150);
        assert_eq!(df_check.metadata_system_bytes, Some(40));
        assert_eq!(df_check.data_metadata_system_bytes, Some(150));
        assert_eq!(df_check.residual_bytes, Some(0));
        assert_eq!(space.space_info.as_ref().unwrap()[1].copies, Some(2));
    }

    #[test]
    fn coverage_gaps_and_missing_samples_are_not_reported_as_reconciled() {
        let data = OverviewData::default();
        let window = SpaceWindow {
            start: None,
            end: Some(sample(
                FsSpace {
                    total_bytes: 1000,
                    free_bytes: 900,
                    available_bytes: 850,
                },
                vec![group(1, 100)],
            )),
        };
        let (_, gap, _) = overview_space(&window, &data, true);
        assert_eq!(gap.status, "not_reconciled");
        assert_eq!(gap.residual_bytes, Some(-100));

        let window = SpaceWindow {
            start: None,
            end: None,
        };
        let (_, missing, _) = overview_space(&window, &data, false);
        assert_eq!(missing.status, "unknown");
        assert_eq!(missing.residual_bytes, None);
    }

    #[test]
    fn unsupported_raid_copy_factors_keep_df_reconciliation_unknown() {
        assert_eq!(profile(1), ("single", Some(1)));
        assert_eq!(profile(4 | 32), ("dup", Some(2)));
        assert_eq!(profile(1 | 128), ("raid5", None));
        assert_eq!(groups_df_used(&[group(1 | 128, 100)]), None);

        let sample = sample(
            FsSpace {
                total_bytes: 1000,
                free_bytes: 900,
                available_bytes: 850,
            },
            vec![group(1 | 128, 100)],
        );
        let window = SpaceWindow {
            start: Some(sample.clone()),
            end: Some(sample),
        };
        let (_, data, df) = overview_space(&window, &OverviewData::default(), false);
        assert_eq!(df.status, "unknown");
        assert!(overview_needs_partial(&data, &df));
    }

    #[test]
    fn overview_parts_sum_counts_each_bucket_and_pinned_bytes_once() {
        let data = OverviewData {
            extent_bytes: 700,
            pending_cleanup_bytes: 100,
            snapshot_only_bytes: BTreeMap::from([(7, 100)]),
            recycle_bin: BTreeMap::from([(
                7,
                crate::overview::OwnerBytes {
                    bytes: 92,
                    reclaimable_bytes: 100,
                },
            )]),
            dsm_services: BTreeMap::from([(2, 100)]),
            share_live: BTreeMap::from([(
                7,
                crate::overview::OwnerBytes {
                    bytes: 100,
                    reclaimable_bytes: 100,
                },
            )]),
            shared_between_folders_bytes: 100,
            pinned_unreferenced_bytes: 8,
            unknown: BTreeMap::from([(crate::overview::UnknownReason::MissingOwner, 100)]),
            ..OverviewData::default()
        };

        assert_eq!(overview_parts_sum(&data), data.extent_bytes);
        assert_eq!(overview_parts(&data).len(), 9);
    }

    #[test]
    fn overview_iscsi_items_include_the_service_explanation() {
        let service = crate::dsm::service(b"@iSCSI", crate::dsm::Depth::VolumeRoot).unwrap();
        let item = service_overview_item(crate::dsm::service_id(service), 100);
        let json = serde_json::to_value(item).unwrap();
        assert!(
            json["explanation"]
                .as_str()
                .unwrap_or_default()
                .contains("UNMAP")
        );
    }

    #[test]
    fn overview_pinned_range_reason_lists_its_subcases() {
        use crate::overview::{PinnedRangesSubcase, UnknownReason};
        let data = OverviewData {
            unknown: BTreeMap::from([(UnknownReason::PinnedRangesUnknown, 9)]),
            pinned_ranges_subcases: BTreeMap::from([
                (PinnedRangesSubcase::NoScannedUse, 4),
                (PinnedRangesSubcase::SharedReference, 5),
            ]),
            ..OverviewData::default()
        };

        let parts = overview_parts(&data);
        let unknown = parts.last().expect("unknown part");
        let subcases: Vec<_> = unknown.reasons[0]
            .subcases
            .iter()
            .map(|subcase| (subcase.code, subcase.bytes))
            .collect();
        assert_eq!(
            subcases,
            vec![
                (
                    SubcaseCode::PinnedRanges(PinnedRangesSubcase::NoScannedUse),
                    4
                ),
                (
                    SubcaseCode::PinnedRanges(PinnedRangesSubcase::SharedReference),
                    5
                ),
            ]
        );
    }
}

pub struct SharedArgs {
    pub by: ShareBy,
    pub kind: KindFilter,
    pub limit: usize,
    pub include_system: bool,
}

pub fn shared(
    targets: TargetArgs,
    args: SharedArgs,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let (measure, ranked_by) = match args.by {
        ShareBy::Shared => (Measure::Shared, "shared"),
        ShareBy::Ratio => (Measure::SharedRatio, "ratio"),
        ShareBy::Logical => (Measure::Logical, "logical"),
    };
    let rank = RankArgs {
        by: measure,
        kind: args.kind,
        limit: args.limit,
        include_system: args.include_system,
        only_shared: true,
        services: false,
        report: None,
    };
    let mut envelope = Envelope::new("shared", SystemTime::now());
    let (ranked, scans) = rank_targets(
        &targets,
        &rank,
        answer,
        output,
        limits,
        state_dir,
        &mut envelope,
    )?;
    let results = ranked
        .into_iter()
        .map(|(report, ranking, _, _, _)| {
            let filesystem = report.filesystem;
            let normalize = |entry: &query::Entry| {
                let mut shared = SharedEntry::from(entry);
                shared.entry = shared.entry.with_filesystem(filesystem);
                shared
            };
            SharedResult {
                report,
                target: normalize(&ranking.target),
                entries: ranking.entries.iter().map(normalize).collect(),
                hidden_system: ranking.hidden_system,
            }
        })
        .collect();
    let doc = SharedDocument {
        envelope,
        ranked_by,
        kind: kind_name(args.kind),
        limit: args.limit,
        include_system: args.include_system,
        results,
        scans,
    };
    emit(output.json, &doc, || render_shared(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

fn kind_name(kind: KindFilter) -> &'static str {
    match kind {
        KindFilter::All => "all",
        KindFilter::Files => "files",
        KindFilter::Dirs => "dirs",
    }
}

/// How entries below each target are ranked.
struct RankArgs {
    by: Measure,
    kind: KindFilter,
    limit: usize,
    include_system: bool,
    only_shared: bool,
    /// Also add up the DSM service data below each target, and name the
    /// package data each entry is in or holds.
    services: bool,
    /// The chosen report (`--report`), if any.
    report: Option<String>,
}

/// Each target's report, ranking, service summary and, with
/// `RankArgs::services`, its `top` rows: the entries and services.
type Ranked = Vec<(
    ReportInfo,
    query::Ranking,
    ServiceSummary,
    Vec<TopRow>,
    Vec<crate::summary::PackageOverlap>,
)>;

/// Ranks the entries below each target from its covering report,
/// scanning if allowed, and completes `envelope`.
fn rank_targets(
    targets: &TargetArgs,
    rank: &RankArgs,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
    envelope: &mut Envelope,
) -> Result<(Ranked, Vec<FilesystemScan>), Failure> {
    let env = Environment::load()?;
    let (resolved, errors) = resolve_targets(&env, targets)?;
    let (groups, _) = partition(resolved);
    let resolved: Vec<Resolved> = groups.into_iter().flat_map(|g| g.targets).collect();
    let mut answerer = Answerer {
        env: &env,
        limits,
        state_dir,
        answer,
        output,
        scans: Vec::new(),
        errors,
        hints: Vec::new(),
        job_id: None,
        report: rank.report.clone(),
    };
    let ready = answerer.prepare(resolved)?;
    let now = SystemTime::now();
    let mut results = Vec::new();
    for (target, path) in ready {
        let header = crate::report_summary::ensure(&path)?.header;
        let under = target.path.as_os_str().as_bytes();
        let query = TopQuery {
            under,
            services: rank.services,
            by: rank.by,
            kind: rank.kind,
            include_system: rank.include_system,
            limit: rank.limit,
            only_shared: rank.only_shared,
        };
        let Some(found) = crate::report_summary::index::top(&path, &header.mount, &query)? else {
            let e = TargetError::new(
                JsonPath::new(under),
                "not_in_report",
                "not in the covering report; use --refresh",
            );
            answerer.errors.push(e);
            continue;
        };
        if let Some(w) = identity_warning(&target, &found.ranking.target) {
            envelope.warnings.push(w);
        }
        let info = answered_report(&path, &header, now, answer.max_age);
        stale_warning(envelope, &info);
        unresolved_warning(envelope, &header);
        let rows = if rank.services {
            top_rows(&header.mount, &found, rank, output.json)
        } else {
            Vec::new()
        };
        results.push((info, found.ranking, found.services, rows, found.overlaps));
    }
    answerer.close(envelope, !results.is_empty());
    Ok((results, std::mem::take(&mut answerer.scans)))
}

/// The best rows of `answer`'s ranking and, unless only files rank, of
/// its services, each entry naming the package data it is in or holds.
fn top_rows(mount: &[u8], answer: &query::TopAnswer, rank: &RankArgs, json: bool) -> Vec<TopRow> {
    let none = ServiceSummary::default();
    let services = match rank.kind {
        KindFilter::Files => &none,
        _ => &answer.services,
    };
    let full = json && answer.ranking.target.path == mount;
    let rows = if full {
        query::full_rows(&answer.ranking, services, rank.by, rank.limit)
    } else {
        query::rows(&answer.ranking, services, rank.by, rank.limit)
    };
    let included = if full {
        query::inclusions(&rows)
    } else {
        vec![None; rows.len()]
    };
    rows.into_iter()
        .zip(included)
        .map(|(row, within)| {
            let mut rendered = match row {
                query::Row::Entry(entry) => {
                    let package = crate::package::lookup(&entry.path, mount, &answer.roots);
                    TopRow::entry(&entry, mount, package.as_ref())
                }
                query::Row::Service(line) => TopRow::service(line),
            };
            rendered.within = within.as_deref().map(JsonPath::new);
            rendered
        })
        .collect()
}

/// `entries` as JSON, each naming the package data it is in or holds
/// (SPEC 5.7).
fn with_package_data(
    mount: &[u8],
    roots: &[crate::summary::PackageRoot],
    entries: &[query::Entry],
) -> Vec<EntryJson> {
    entries
        .iter()
        .map(|entry| {
            let package = crate::package::lookup(&entry.path, mount, roots);
            EntryJson::with_package(entry, package.as_ref())
        })
        .collect()
}

/// The report at `path`, its header read and its records to stream.
fn open_report(path: &Path) -> Result<(Header, report::ReportReader<File>), Failure> {
    let reader = report::ReportReader::open(File::open(path)?)?;
    Ok((reader.header().clone(), reader))
}

/// Reclaimable values of a report with unresolved references are
/// conservative, not exact (SPEC 3).
fn unresolved_warning(envelope: &mut Envelope, header: &crate::report::Header) {
    let unknown = header.accounting.unknown_extents;
    if unknown > 0 && header.coverage.approximation.is_none() {
        envelope.warnings.push(Notice::new(
            "unresolved_references",
            format!(
                "{unknown} extents ({}) have unresolved references; reclaimable values count only resolved extents",
                crate::output::human_bytes(header.totals.unknown)
            ),
        ));
    }
}

/// What an answer says about the report at `path` it read.
pub(crate) fn answered_report(
    path: &Path,
    header: &Header,
    now: SystemTime,
    max_age: Duration,
) -> ReportInfo {
    ReportInfo::new(path.as_os_str().as_bytes(), header, now, max_age)
        .with_cleanup(state::cleanup_marks_beside(path, sys::effective_uid()).of(&header.job_id))
}

fn stale_warning(envelope: &mut Envelope, info: &ReportInfo) {
    if !info.stale {
        return;
    }
    let job = &info.job_id;
    let (stale, cleanup) = match &info.cleanup_since_report {
        Some(mark) => (
            format!(
                "report {job} predates cleanup {} applied {}",
                mark.plan_id, mark.applied_at
            ),
            Some((
                "cleanup_since_report",
                format!(
                    "cleanup {} removed data after report {job}; rescan for current sizes",
                    mark.plan_id
                ),
            )),
        ),
        None if info.cleanup_record_unreadable => (
            format!("report {job} may predate a cleanup: the cleanup record is unreadable"),
            Some((
                "cleanup_record_unreadable",
                format!(
                    "the cleanup record of report {job} is unreadable; rescan for current sizes"
                ),
            )),
        ),
        None => (
            format!(
                "report {job} finished {} is older than --max-age",
                info.scan_finished
            ),
            None,
        ),
    };
    for (code, message) in std::iter::once(("stale_report", stale)).chain(cleanup) {
        if !envelope
            .warnings
            .iter()
            .any(|w| w.code == code && w.message.contains(job.as_str()))
        {
            envelope.warnings.push(Notice::new(code, message));
        }
    }
}

pub fn inspect(
    targets: TargetArgs,
    explain: bool,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let env = Environment::load()?;
    let (resolved, errors) = resolve_targets(&env, &targets)?;
    let mut answerer = Answerer {
        env: &env,
        limits,
        state_dir,
        answer,
        output,
        scans: Vec::new(),
        errors,
        hints: Vec::new(),
        job_id: None,
        report: None,
    };
    let ready = answerer.prepare(resolved)?;
    let now = SystemTime::now();
    let mut envelope = Envelope::new("inspect", now);
    let mut results = Vec::new();
    for (target, path) in ready {
        let (header, reader) = open_report(&path)?;
        let wanted = target.path.as_os_str().as_bytes();
        let mut find = query::Find::new(wanted);
        reader.for_each(|path, record| find.visit(path, record))?;
        let Some(entry) = find.finish() else {
            answerer.errors.push(TargetError::new(
                JsonPath::new(wanted),
                "not_in_report",
                "not in the covering report; use --refresh",
            ));
            continue;
        };
        if let Some(w) = identity_warning(&target, &entry) {
            envelope.warnings.push(w);
        }
        let info = answered_report(&path, &header, now, answer.max_age);
        stale_warning(&mut envelope, &info);
        results.push(InspectResult {
            explain: explain.then(|| crate::document::explain(&entry, &header)),
            report: info,
            entry: EntryJson::from(&entry).with_filesystem(header.filesystem),
        });
    }
    answerer.close(&mut envelope, !results.is_empty());
    let doc = InspectDocument {
        envelope,
        results,
        scans: std::mem::take(&mut answerer.scans),
    };
    emit(output.json, &doc, || render_inspect(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

pub fn simulate_delete(
    targets: TargetArgs,
    explain: bool,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let env = Environment::load()?;
    let (resolved, mut errors) = resolve_targets(&env, &targets)?;
    let (paths, volumes): (Vec<Resolved>, Vec<Resolved>) =
        resolved.into_iter().partition(|t| t.scope == Scope::Path);
    errors.extend(volumes.iter().map(|t| {
        TargetError::new(
            JsonPath::new(t.path.as_os_str().as_bytes()),
            "unsupported_target",
            "simulate-delete takes paths inside a volume, not whole volumes",
        )
    }));
    let now = SystemTime::now();
    let mut envelope = Envelope::new("simulate-delete", now);
    let (groups, covered) = partition(paths);
    for c in &covered {
        envelope.warnings.push(Notice::new(
            "covered",
            format!(
                "{} is inside {}; counted once",
                c.target.path.display(),
                c.by.display()
            ),
        ));
    }
    let mut answerer = Answerer {
        env: &env,
        limits,
        state_dir,
        answer,
        output,
        scans: Vec::new(),
        errors,
        hints: Vec::new(),
        job_id: None,
        report: None,
    };
    let mut results = Vec::new();
    let mut snapshot_sets = Vec::new();
    for group in groups {
        let listing = answerer.store()?.reports(&fsid_text(&group.fsid))?;
        let (roots, live): (Vec<&Resolved>, Vec<&Resolved>) = group
            .targets
            .iter()
            .partition(|t| is_snapshot_root(&listing.reports, t.tree, t.ino));
        if !roots.is_empty() && live.is_empty() {
            match snapshot_set(&listing.reports, &roots, now, answer.max_age, state_dir) {
                Some(set) => {
                    stale_warning(&mut envelope, &set.report);
                    snapshot_sets.push(set);
                }
                None => {
                    for t in &roots {
                        answerer.errors.push(no_report(
                            t,
                            "no report of a scan with --snapshots holds all these snapshots",
                            listing.unreadable.len(),
                        ));
                        answerer
                            .hints
                            .push(scan_hint(state_dir, t).with_flags("--snapshots"));
                    }
                }
            }
            continue;
        }
        let (group, deleted) = split_deleted(group, &listing.reports);
        let deletion = deletion_request(&deleted, &limits);
        let Some(path) = answerer.prepare_selection(group, deletion)? else {
            continue;
        };
        let (header, reader) = open_report(&path)?;
        let header = &header;
        let mut selection = query::SelectionWalk::new(header);
        let mut package_roots = crate::summary::PackageRoots::new(&header.mount);
        reader.for_each(|path, record| {
            package_roots.visit(path, record);
            selection.visit(path, record)
        })?;
        let selection = selection.finish();
        let info = answered_report(&path, header, now, answer.max_age);
        stale_warning(&mut envelope, &info);
        unresolved_warning(&mut envelope, header);
        results.push(SimulateResult {
            report: info,
            selection: with_package_data(&header.mount, &package_roots.finish(), &selection.roots).into_iter().map(|entry| entry.with_filesystem(header.filesystem)).collect(),
            combined: JsonMetrics::from(&selection.combined).with_filesystem(header.filesystem),
            retained: selection.combined.shared(),
            together_bonus: selection.together_bonus,
            blockers: blocker_summary(header),
            explain: explain.then(|| {
                let mut notes: Vec<Notice> = blocker_summary(header)
                    .iter()
                    .map(|b| Notice::new(b.blocker, blocker_message(b.blocker)))
                    .collect();
                let selected_paths: Vec<Vec<u8>> = selection
                    .roots
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect();
                notes.extend(selected_service_hints(&selected_paths, &header.mount));
                notes
            }),
            after_delete: if header.filesystem == crate::filesystem::FilesystemKind::Ext4 {
                vec![Notice::new("ext4_allocated_estimate", "st_blocks-based estimate for permanent deletion after the final hard link and open holder are gone; not a guaranteed immediate df increase")]
            } else { crate::release::after_delete(
                &selection
                    .roots
                    .iter()
                    .fold(Blockers::default(), |all, root| all.with(root.blockers))
                    .names(),
            ) },
            with_snapshots: header
                .deletion
                .as_ref()
                .filter(|_| !deleted.is_empty())
                .map(|d| with_snapshots(&deleted, d)),
        });
    }
    answerer.close(
        &mut envelope,
        !results.is_empty() || !snapshot_sets.is_empty(),
    );
    let unknown = snapshot_sets
        .iter()
        .any(|s| s.simulation.unknown_bytes > 0 || !s.simulation.not_materialized_ids.is_empty())
        || results
            .iter()
            .filter_map(|r| r.with_snapshots.as_ref())
            .any(WithSnapshotsJson::has_unknown);
    if unknown && envelope.completion == Completion::Complete {
        envelope.completion = Completion::Partial;
    }
    let doc = SimulateDocument {
        envelope,
        results,
        snapshot_sets,
        scans: std::mem::take(&mut answerer.scans),
    };
    emit(output.json, &doc, || render_simulate(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

/// Fixed explanations for selected paths whose service-table entry carries one.
fn selected_service_hints(paths: &[Vec<u8>], mount: &[u8]) -> Vec<Notice> {
    let mut seen = Vec::new();
    let mut hints = Vec::new();
    for path in paths {
        let Some(service) =
            crate::dsm::service_of(path, mount).filter(|service| service.explanation.is_some())
        else {
            continue;
        };
        if seen.contains(&service.code) {
            continue;
        }
        seen.push(service.code);
        hints.push(Notice::new("dsm_service", service.note()));
    }
    hints
}

/// What `snapshots` shows besides the snapshot list.
#[derive(Debug, Clone)]
pub struct SnapshotsView {
    pub selection: Option<Selection>,
    /// Warn when the report is older than this.
    pub max_age: Duration,
    /// Held files shown per share.
    pub limit: usize,
}

/// `snapshots` (SPEC 5.14): answered only from a report of a scan with
/// `--snapshots`; it never scans.
pub fn snapshots(
    targets: TargetArgs,
    view: SnapshotsView,
    output: OutputArgs,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let env = Environment::load()?;
    let (resolved, errors) = resolve_targets(&env, &targets)?;
    let now = SystemTime::now();
    let mut envelope = Envelope::new("snapshots", now);
    envelope.errors = errors;
    let store = Store::open(state_dir, sys::effective_uid())?;
    let mut results = Vec::new();
    for target in &resolved {
        let path = target.path.as_os_str().as_bytes();
        if !target.filesystem.capabilities().snapshots {
            envelope.errors.push(TargetError::new(
                JsonPath::new(path),
                "unsupported_capability",
                "snapshots are not available on ext4",
            ));
            continue;
        }
        let listing = if view.selection.is_some() {
            store.reports(&fsid_text(&target.fsid))?
        } else {
            store.display_reports(&fsid_text(&target.fsid))?
        };
        let volume = target.scope == Scope::Volume;
        let found = listing.reports.iter().find(|(_, h)| {
            query::covers(h, path)
                && h.snapshots
                    .as_ref()
                    .is_some_and(|a| volume || a.share(target.tree).is_some())
        });
        let Some((file, header)) = found else {
            envelope.errors.push(no_report(
                target,
                "no report of a scan with --snapshots holds its snapshots",
                listing.unreadable.len(),
            ));
            envelope
                .scan_hints
                .push(scan_hint(state_dir, target).with_flags("--snapshots"));
            continue;
        };
        let summary = view
            .selection
            .is_none()
            .then(|| crate::report_summary::ensure(file))
            .transpose()?;
        // A partial cache must fall back to the original holdings, never zeroed display metadata.
        let fallback_header = summary
            .as_ref()
            .filter(|s| {
                s.header.snapshots.as_ref().is_some_and(|a| {
                    a.shares
                        .iter()
                        .any(|share| !s.snapshots.contains_key(&share.live))
                })
            })
            .map(|_| report::read_header(std::io::BufReader::new(File::open(file)?)))
            .transpose()?;
        let header = fallback_header
            .as_ref()
            .unwrap_or_else(|| summary.as_ref().map_or(header, |s| &s.header));
        let info = answered_report(file, header, now, view.max_age);
        stale_warning(&mut envelope, &info);
        let analysis = header.snapshots.as_ref().expect("found with snapshots");
        let listing = Listing {
            mount: &header.mount,
            limit: view.limit,
        };
        let rescan = Rescan {
            mount: &header.mount,
            state_dir,
        };
        let shares: Vec<ShareJson> = analysis
            .shares_for(volume, target.tree)
            .into_iter()
            .map(|share| match &summary {
                Some(summary) => crate::snapshots::answer::share_answer_cached(
                    analysis,
                    share,
                    now,
                    &listing,
                    &rescan,
                    &summary.snapshots,
                ),
                None => share_answer(
                    analysis,
                    share,
                    view.selection.as_ref(),
                    now,
                    &listing,
                    &rescan,
                ),
            })
            .collect();
        if shares
            .iter()
            .any(|s| s.held_files.as_ref().is_some_and(|h| !h.complete))
        {
            envelope.warnings.push(Notice::new(
                "held_files_incomplete",
                "more files hold snapshot-only data than the scan tallied: the file list may miss some; rescan with a larger --held-files-budget",
            ));
        }
        if analysis.stats.owner_unresolved_extents > 0 {
            envelope.warnings.push(Notice::new(
                "owner_budget",
                format!(
                    "{} extent(s) could not be resolved within per-extent budget {} and global budget {}: their bytes are unknown",
                    analysis.stats.owner_unresolved_extents,
                    analysis.owner_budget,
                    analysis.global_owner_budget
                ),
            ));
        }
        results.push(SnapshotsResult {
            snapshot_analysis: analysis.stats.measurement.clone(),
            target: JsonPath::new(path),
            report: info,
            shares,
        });
    }
    let unknown = results.iter().any(snapshots_result_unknown);
    envelope.completion = match (results.is_empty(), envelope.errors.is_empty()) {
        (true, _) => Completion::Failed,
        (false, true) if !unknown => Completion::Complete,
        (false, _) => Completion::Partial,
    };
    let doc = SnapshotsDocument { envelope, results };
    emit(output.json, &doc, || render_snapshots(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

fn snapshots_result_unknown(result: &SnapshotsResult) -> bool {
    result.shares.iter().any(|share| {
        share.has_unknown()
            && (result.report.coverage.approximation.is_none() || share.simulation.is_some())
    })
}

/// `recycle` (SPEC 5.16): reports real shared-folder Recycle Bins only.
/// It never empties a bin or creates a cleanup plan.
pub fn recycle(
    path: PathBuf,
    oldest: Option<usize>,
    report: Option<String>,
    answer: AnswerArgs,
    output: OutputArgs,
    limits: ScanLimits,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let env = Environment::load()?;
    let now = SystemTime::now();
    let mut envelope = Envelope::new("recycle", now);
    let target = match resolve::resolve_path(&env, &path, None) {
        Ok(target) => target,
        // A saved scan can still list a share that has since been removed.
        // Preserve the resolver's code, path and cause in the JSON answer.
        Err(error) => {
            return refuse_recycle(output.json, envelope, &path, error.code, error.message);
        }
    };
    if !is_recycle_target(&target) {
        return refuse_recycle(
            output.json,
            envelope,
            &target.path,
            "not_a_shared_folder",
            format!(
                "{} is not a volume or shared-folder root; recycle requires a real shared-folder Recycle Bin",
                target.path.display()
            ),
        );
    }

    let mut answerer = Answerer {
        env: &env,
        limits,
        state_dir,
        answer,
        output,
        scans: Vec::new(),
        errors: Vec::new(),
        hints: Vec::new(),
        job_id: None,
        report: report.clone(),
    };
    let ready = answerer.prepare(vec![target.clone()])?;
    let mut report_info = None;
    let mut shares = Vec::new();
    let mut moved_by_reclaim = Vec::new();
    for (target, report_path) in ready {
        let summary = crate::report_summary::ensure(&report_path)?;
        // Optional age ranking still needs the original ctime index and its checked walk.
        let header = if oldest.is_some() {
            open_report(&report_path)?.0
        } else {
            summary.header
        };
        let under = target.path.as_os_str().as_bytes();
        let mut bins: Vec<_> = summary
            .bins
            .iter()
            .filter_map(|bin| bin.get())
            .filter(|bin| bin.under(&header.mount, under))
            .collect();
        if target.scope != Scope::Volume && bins.is_empty() {
            return refuse_recycle(
                output.json,
                envelope,
                &target.path,
                "no_recycle_bin",
                format!(
                    "{} has no real #recycle directory; empty it in DSM only if Recycle Bin is enabled",
                    target.path.display()
                ),
            );
        }
        // Shares scanned on their own after the volume scan answer from
        // their own, newer report (SPEC 5.16), unless a report was chosen.
        let supplements = if target.scope == Scope::Volume && report.is_none() {
            share_supplements(&answerer.store()?, &header, oldest, now, answer.max_age)
        } else {
            Vec::new()
        };
        bins.retain(|bin| {
            !supplements
                .iter()
                .any(|s| s.roots.iter().any(|root| bin.under(&header.mount, root)))
        });
        let info = answered_report(&report_path, &header, now, answer.max_age);
        stale_warning(&mut envelope, &info);
        unresolved_warning(&mut envelope, &header);
        let mut shares_for_report =
            recycle_shares(&mut envelope, &report_path, &header, &bins, oldest, None)?;
        for supplement in supplements {
            shares_for_report.extend(recycle_shares(
                &mut envelope,
                &supplement.path,
                &supplement.header,
                &supplement.bins,
                oldest,
                Some(supplement.info),
            )?);
        }
        // Largest first, as one report lists them.
        shares_for_report.sort_by_key(|share| std::cmp::Reverse(share.bytes));
        let under = target.path.as_os_str().as_bytes();
        moved_by_reclaim = moved_since(state_dir, &header.mount, header.scan_finished)
            .into_iter()
            .filter(|m| {
                let root = [header.mount.as_slice(), b"/", &m.share].concat();
                target.scope == Scope::Volume || root == under
            })
            .map(|m| MovedByReclaimJson {
                share: JsonPath::new(&m.share),
                items: m.items,
                bytes: m.allocated_bytes,
                last_moved: rfc3339(std::time::UNIX_EPOCH + Duration::from_secs(m.at)),
            })
            .collect();
        report_info = Some(info);
        shares = shares_for_report;
    }
    answerer.close(&mut envelope, report_info.is_some());
    let doc = RecycleDocument {
        envelope,
        target: JsonPath::new(target.path.as_os_str().as_bytes()),
        report_info,
        shares,
        moved_by_reclaim,
        scans: std::mem::take(&mut answerer.scans),
    };
    emit(output.json, &doc, || render_recycle(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

/// A newer share report a volume's Recycle Bin answer lists some shares from.
struct RecycleSupplement {
    path: PathBuf,
    header: Header,
    info: ReportInfo,
    roots: Vec<Vec<u8>>,
    bins: Vec<crate::recycle::RecycleBin>,
}

/// The newer share reports of the volume report `volume`, with their bins;
/// a share report that cannot be read leaves its shares to the volume report.
fn share_supplements(
    store: &Store,
    volume: &Header,
    oldest: Option<usize>,
    now: SystemTime,
    max_age: Duration,
) -> Vec<RecycleSupplement> {
    let Ok(listing) = store.display_reports(&volume.fsid) else {
        return Vec::new();
    };
    crate::recycle::newer_share_reports(&listing.reports, volume)
        .into_iter()
        .filter_map(|supplement| {
            let path = supplement.report.clone();
            let summary = crate::report_summary::ensure(&path).ok()?;
            let header = if oldest.is_some() {
                open_report(&path).ok()?.0
            } else {
                summary.header
            };
            let roots: Vec<Vec<u8>> = supplement.roots.iter().map(|r| r.to_vec()).collect();
            let bins = summary
                .bins
                .iter()
                .filter_map(|bin| bin.get())
                .filter(|bin| roots.iter().any(|root| bin.under(&header.mount, root)))
                .collect();
            let info = answered_report(&path, &header, now, max_age);
            Some(RecycleSupplement {
                path,
                header,
                info,
                roots,
                bins,
            })
        })
        .collect()
}

/// The Recycle Bin rows of `bins` from one report, with their oldest items
/// when asked; `from` names that report when it is not the answer's own.
fn recycle_shares(
    envelope: &mut Envelope,
    report_path: &Path,
    header: &Header,
    bins: &[crate::recycle::RecycleBin],
    oldest: Option<usize>,
    from: Option<ReportInfo>,
) -> Result<Vec<RecycleShareJson>, Failure> {
    let oldest_items = if let Some(limit) = oldest {
        let mut query = crate::recycle::OldestItems::new(bins, &header.recycle_ctimes, limit);
        if limit > 0 && !bins.is_empty() {
            let (_, reader) = open_report(report_path)?;
            reader.for_each(|path, record| query.visit(path, record))?;
        }
        Some(query.finish())
    } else {
        None
    };
    let mut shares = Vec::with_capacity(bins.len());
    for bin in bins {
        let mut share = RecycleShareJson::from_bin(bin);
        share.report_info = from.clone();
        if let Some(oldest_items) = &oldest_items {
            let items = oldest_items.get(&bin.path).map_or(&[][..], Vec::as_slice);
            if items.iter().any(|item| item.ctime.is_none()) {
                envelope.warnings.push(Notice::new(
                    "recycle_age_unavailable",
                    "Some Recycle Bin items have no recorded ctime; they are listed after measured items",
                ));
            }
            share.oldest = Some(
                items
                    .iter()
                    .map(|item| RecycleOldestJson::from_item(item, header.filesystem))
                    .collect(),
            );
        }
        shares.push(share);
    }
    Ok(shares)
}

/// A recycle target that cannot be answered (SPEC 5.16): the same text
/// on stderr as before, or, with `--json`, the standard envelope with a
/// stable `errors[].code` so the UI never parses CLI text.
fn refuse_recycle(
    json: bool,
    envelope: Envelope,
    target: &Path,
    code: &'static str,
    message: String,
) -> Result<i32, Failure> {
    if !json {
        return Err(CleanupError::Protected(message).into());
    }
    let doc = recycle_refusal_document(envelope, target, code, message);
    emit(true, &doc, String::new)?;
    Ok(4)
}

/// The `--json` document of a recycle refusal: a standard envelope with
/// the refusal in `errors[].code` and no report.
fn recycle_refusal_document(
    mut envelope: Envelope,
    target: &Path,
    code: &'static str,
    message: String,
) -> RecycleDocument {
    envelope.completion = Completion::Failed;
    let target = JsonPath::new(target.as_os_str().as_bytes());
    envelope
        .errors
        .push(TargetError::new(target.clone(), code, message));
    RecycleDocument {
        envelope,
        target,
        report_info: None,
        shares: Vec::new(),
        moved_by_reclaim: Vec::new(),
        scans: Vec::new(),
    }
}

fn is_recycle_target(target: &Resolved) -> bool {
    if target.scope == Scope::Volume {
        return true;
    }
    if target.filesystem == crate::filesystem::FilesystemKind::Ext4 {
        return crate::dsm_shares::list(&target.mount).is_ok_and(|shares| {
            shares.iter().any(|share| {
                share.device == target.tree
                    && share.inode == target.ino
                    && target.path
                        == target
                            .mount
                            .join(std::ffi::OsString::from_vec(share.name.clone()))
            })
        });
    }
    if target.ino != FIRST_FREE_OBJECTID {
        return false;
    }
    let path = target.path.as_os_str().as_bytes();
    let mount = target.mount.as_os_str().as_bytes();
    let Some(name) = path.strip_prefix(mount).and_then(|p| p.strip_prefix(b"/")) else {
        return false;
    };
    !name.is_empty()
        && !name.contains(&b'/')
        && !name.starts_with(b"@")
        && !name.starts_with(b"#")
        && dsm::service(name, Depth::VolumeRoot).is_none()
}

/// `cache status` or `cache clear` (SPEC 5.9) for the filesystems of the
/// targets; neither creates the state directory.
pub fn cache(action: CacheAction) -> Result<i32, Failure> {
    let (name, args) = match action {
        CacheAction::Status(args) => ("status", args),
        CacheAction::Clear(args) => ("clear", args),
    };
    let CacheArgs {
        targets,
        output,
        state,
    } = args;
    let env = Environment::load()?;
    let uid = sys::effective_uid();
    // Opening checks the directory is private; a missing one keeps nothing.
    let exists = state.state_dir.is_dir();
    if exists {
        Store::open(&state.state_dir, uid)?;
    }
    let root = state.state_dir.join("cache");
    let (resolved, errors) = resolve_targets(&env, &targets)?;
    let (groups, _) = partition(resolved);
    let filesystems: Vec<CacheInfo> = groups
        .iter()
        .map(|group| {
            let mut info = cache_info(&root, group);
            if name == "clear" {
                clear_cache(&root, uid, exists, &mut info);
            }
            info
        })
        .collect();
    let mut envelope = Envelope::new("cache", SystemTime::now());
    envelope.completion = cache_completion(&filesystems, &errors);
    envelope.errors = errors;
    let doc = CacheDocument {
        envelope,
        action: name,
        filesystems,
    };
    emit(output.json, &doc, || render_cache(&doc))?;
    Ok(doc.envelope.completion.exit_code(None))
}

/// What the cache under `root` keeps for `group`'s filesystem now.
fn cache_info(root: &Path, group: &FilesystemTargets) -> CacheInfo {
    let fsid = fsid_text(&group.fsid);
    CacheInfo {
        mount: JsonPath::new(group.mount.as_os_str().as_bytes()),
        completion: Completion::Complete,
        status: cache_status(root, &fsid),
        global_index: GLOBAL_INDEX,
        cleared: None,
        errors: Vec::new(),
    }
}

fn cache_status(root: &Path, fsid: &str) -> CacheStatus {
    let now = unix_seconds(SystemTime::now());
    cache::status(root, fsid, Reuse::of_this_platform(), now)
}

/// Deletes the cache `info` shows, under the filesystem's writer lock so
/// no scan is using its units; `exists` is whether the state directory
/// does.
fn clear_cache(root: &Path, uid: u32, exists: bool, info: &mut CacheInfo) {
    let fsid = info.status.fsid.clone();
    let holder = state::LockHolder::new(None, "cache-clear");
    let _lock = match writer_lock(&fsid, uid, &info.mount, &holder).map_err(|e| *e) {
        Ok(lock) => lock,
        Err((completion, error)) => {
            info.completion = completion;
            info.errors.push(error);
            return;
        }
    };
    let cleared = if exists {
        cache::clear(root, &fsid)
    } else {
        Ok(0)
    };
    match cleared {
        Ok(units) => {
            info.cleared = Some(units);
            info.status = cache_status(root, &fsid);
        }
        Err(e) => {
            info.completion = Completion::Failed;
            info.errors.push(TargetError::new(
                info.mount.clone(),
                "cache_error",
                e.to_string(),
            ));
        }
    }
}

/// Complete when every filesystem is; busy or failed when none is.
fn cache_completion(filesystems: &[CacheInfo], errors: &[TargetError]) -> Completion {
    let completions: Vec<Completion> = filesystems.iter().map(|f| f.completion).collect();
    let complete = completions
        .iter()
        .filter(|&&c| c == Completion::Complete)
        .count();
    match complete {
        0 if completions.contains(&Completion::Busy) => Completion::Busy,
        0 => Completion::Failed,
        n if n == completions.len() && errors.is_empty() => Completion::Complete,
        _ => Completion::Partial,
    }
}

/// Who an apply runs for (SPEC 5.7).
#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyOrigin<'a> {
    /// The DSM user a `webapi` call applied the plan for (audit).
    pub requested_by: Option<&'a str>,
    /// The queued cleanup job this apply runs as (`--job-id`).
    pub job_id: Option<&'a str>,
    /// The user confirmed that items in no shared folder are removed for
    /// good (`--confirm-delete`).
    pub confirm_delete: bool,
}

/// Ends queued cleanup job `id` under `jobs_dir`, whose apply was refused
/// before its executor started, with `doc`, the refusal, as its whole
/// document. A job that is no longer a queued cleanup job (cancelled,
/// or run) is left as it is.
fn end_queued_job(
    jobs_dir: &Path,
    id: &str,
    doc: &mut Envelope,
    exit_code: i32,
) -> Result<(), Failure> {
    match jobs::load(jobs_dir, id) {
        Ok(job) if job.kind == jobs::JobKind::CleanupApply && job.state == JobState::Queued => {}
        _ => return Ok(()),
    }
    let now = unix_seconds(SystemTime::now());
    let mut live = Live::launch(jobs_dir, sys::effective_uid(), id, now)?;
    doc.job_id = Some(id.to_string());
    let codes: Vec<String> = doc.errors.iter().map(|e| e.code.to_string()).collect();
    let reason = codes.first().cloned();
    live.record_result(
        jobs::JobResult::new(doc.completion.name(), exit_code, &[], codes),
        (serde_json::to_string_pretty(doc)? + "\n").as_bytes(),
    )?;
    live.finish(JobState::Failed, reason.as_deref(), now)?;
    Ok(())
}

/// Ends a cleanup command: with `--json` a refusal or failure is the
/// standard envelope with its code in `errors[]` (SPEC 5.7, 22.7), about
/// `subject` (the plan file, or the first selected path).
fn finish_cleanup(
    json: bool,
    command: &'static str,
    subject: &[u8],
    result: Result<i32, Failure>,
) -> Result<i32, Failure> {
    match result {
        Err(error) if json => {
            let doc = cleanup_failure_document(command, SystemTime::now(), subject, error.as_ref());
            emit(true, &doc, String::new)?;
            Ok(cleanup_exit_code(error.as_ref()))
        }
        result => report_refusal(result),
    }
}

/// The `--json` document of a cleanup refusal or failure: `busy` when
/// another writer holds the filesystem, else `failed`.
fn cleanup_failure_document(
    command: &'static str,
    now: SystemTime,
    subject: &[u8],
    error: &(dyn std::error::Error + 'static),
) -> Envelope {
    let mut envelope = Envelope::new(command, now);
    if let Some(errors) = error.downcast_ref::<cleanup::TargetErrors>() {
        envelope.completion = Completion::Failed;
        envelope.errors = errors.0.clone();
        return envelope;
    }
    let code = guard::failure_code(error);
    envelope.completion = if code == "busy" {
        Completion::Busy
    } else {
        Completion::Failed
    };
    let mut target = TargetError::new(
        JsonPath::new(guard::failure_path(error).unwrap_or(subject)),
        code,
        error.to_string(),
    );
    target.limit = guard::failure_limit(error).map(Box::new);
    if let Some(conflict) = guard::failure_conflict(error) {
        target.duplicate.get_or_insert_default().conflict = Some(conflict.clone());
    }
    envelope.errors.push(target);
    envelope
}

/// The exit code of a cleanup failure: refused paths exit 4, as every
/// refusal does (SPEC 22.7).
fn cleanup_exit_code(error: &(dyn std::error::Error + 'static)) -> i32 {
    if error.is::<cleanup::TargetErrors>() {
        4
    } else {
        guard::failure_exit_code(error)
    }
}

/// What a plan or delete refusal is about: the first selected path.
fn first_operand(targets: &TargetArgs) -> Vec<u8> {
    targets
        .operands
        .first()
        .map(|operand| operand.as_bytes().to_vec())
        .or_else(|| {
            let file = targets.from_file.as_ref()?;
            Some(file.as_os_str().as_bytes().to_vec())
        })
        .unwrap_or_default()
}

/// Prints an apply refusal as `synoreclaim: <text>` and exits 4 (SPEC
/// 22.7); other failures go on to the caller.
fn report_refusal(result: Result<i32, Failure>) -> Result<i32, Failure> {
    match result {
        Err(error) => match error.downcast::<Refusal>() {
            Ok(refusal) => {
                eprintln!("synoreclaim: {refusal}");
                Ok(4)
            }
            Err(error) => Err(error),
        },
        done => done,
    }
}

/// Counts the subvolumes awaiting the Btrfs cleaner.
fn pending_deletion(subvolumes: &SubvolumeSet) -> PendingDeletion {
    PendingDeletion {
        count: subvolumes
            .subvolumes
            .iter()
            .filter(|subvolume| subvolume.pending_deletion)
            .count(),
        bytes: None,
    }
}

/// The shared-folder roots of a volume (SPEC 4.6): the writable direct
/// children of its mounted subvolume with user names, sorted by name.
fn volume_shares(set: &SubvolumeSet, mount: &Path, volume_tree: u64) -> Vec<VolumeShare> {
    let mount = mount.as_os_str().as_bytes();
    let mut shares: Vec<VolumeShare> = set
        .subvolumes
        .iter()
        .filter(|subvolume| subvolume.is_share(volume_tree))
        .map(|subvolume| {
            let mut path = mount.to_vec();
            path.push(b'/');
            path.extend_from_slice(&subvolume.name);
            VolumeShare {
                name: JsonPath::new(&subvolume.name),
                path: JsonPath::new(&path),
                subvolume_id: Some(subvolume.id),
                inode: None,
                size_bytes: None,
            }
        })
        .collect();
    shares.sort_by_key(|share| share.name.bytes());
    shares
}

/// The share sizes the newest volume report already knows: its overview
/// attributes physical bytes to each share id (SPEC 5.13). `None` when
/// the volume has no volume-scope report; no scan is run to fill it.
fn share_sizes(reports: &[(PathBuf, Header)]) -> Option<BTreeMap<u64, u64>> {
    let overview = reports
        .iter()
        .map(|(_, header)| header)
        .find(|header| {
            header.overview.is_some() && header.roots.iter().any(|r| r.scope == Scope::Volume)
        })?
        .overview
        .as_ref()?;
    Some(
        overview
            .share_live
            .iter()
            .map(|(id, owner)| (*id, owner.bytes))
            .collect(),
    )
}

pub fn volumes(json: bool, state_dir: &Path) -> Result<i32, Failure> {
    let env = Environment::load()?;
    let uid = sys::effective_uid();
    // Listing reads reports but never creates the state directory.
    let store = state_dir
        .is_dir()
        .then(|| Store::open(state_dir, uid))
        .transpose()?;
    let mut volumes = Vec::new();
    for volume in &env.volumes {
        let mut info = VolumeInfo {
            filesystem: if volume.primary.fstype == "ext4" {
                crate::filesystem::FilesystemKind::Ext4
            } else {
                crate::filesystem::FilesystemKind::Btrfs
            },
            capabilities: if volume.primary.fstype == "ext4" {
                crate::filesystem::FilesystemKind::Ext4
            } else {
                crate::filesystem::FilesystemKind::Btrfs
            }
            .capabilities(),
            mount: JsonPath::new(volume.primary.mount_point.as_os_str().as_bytes()),
            fsid: volume.fsid.as_ref().ok().map(fsid_text),
            total_bytes: None,
            used_bytes: None,
            free_bytes: None,
            metadata_access: false,
            reports: 0,
            incompatible_reports: 0,
            last_scan: None,
            last_consistency: None,
            last_attribution: None,
            pending_deletion: None,
            shares: Vec::new(),
            notes: Vec::new(),
        };
        info.capabilities = info.filesystem.capabilities();
        if let Err(e) = &volume.fsid {
            info.notes.push(Notice::new("fs_info_failed", e.clone()));
        }
        match OpenTarget::open(&volume.primary.mount_point) {
            Ok(opened) => {
                use std::os::fd::AsFd;
                if let Ok(space) = sys::filesystem_space(opened.file.as_fd()) {
                    info.total_bytes = Some(space.total);
                    info.used_bytes = Some(space.total - space.free);
                    info.free_bytes = Some(space.available);
                }
                if info.filesystem == crate::filesystem::FilesystemKind::Ext4 {
                    info.metadata_access = volume.fsid.is_ok()
                        && sys::DirectoryEntries::open(opened.file.as_fd()).is_ok();
                    match crate::dsm_shares::list(&volume.primary.mount_point) {
                        Ok(shares) => {
                            info.shares = shares
                                .into_iter()
                                .map(|share| VolumeShare {
                                    path: JsonPath::new(
                                        volume
                                            .primary
                                            .mount_point
                                            .join(std::ffi::OsString::from_vec(share.name.clone()))
                                            .as_os_str()
                                            .as_bytes(),
                                    ),
                                    name: JsonPath::new(&share.name),
                                    subvolume_id: None,
                                    inode: Some(share.inode),
                                    size_bytes: None,
                                })
                                .collect()
                        }
                        Err(error) => info
                            .notes
                            .push(Notice::new("shares_unavailable", error.to_string())),
                    }
                } else {
                    let volume_tree = opened.subvolume_id();
                    let mut source = LiveSource::new(opened.file, 4096);
                    let probe = crate::doctor::probe_tree_search(&mut source);
                    info.metadata_access = probe.is_ok();
                    match probe {
                        Ok(_) => {
                            match crate::watch_free::discover_subvolumes(
                                &mut source,
                                resource::Profile::Low,
                                None,
                            ) {
                                Ok(subvolumes) => {
                                    info.pending_deletion = Some(pending_deletion(&subvolumes));
                                    match volume_tree {
                                        Ok(tree) => {
                                            info.shares = volume_shares(
                                                &subvolumes,
                                                &volume.primary.mount_point,
                                                tree,
                                            );
                                        }
                                        Err(e) => info.notes.push(Notice::new(
                                            "subvolume_id_failed",
                                            format!("cannot name the mounted subvolume: {e}"),
                                        )),
                                    }
                                }
                                Err(e) => info.notes.push(Notice::new("subvolumes_unavailable", e)),
                            }
                        }
                        Err(e) => info
                            .notes
                            .push(Notice::new("metadata_unavailable", e.to_string())),
                    }
                }
            }
            Err(e) => info.notes.push(Notice::new("open_failed", e.to_string())),
        }
        if let (Some(store), Some(fsid)) = (&store, &info.fsid) {
            let listing = store.display_reports(fsid)?;
            info.add_reports(&listing);
            if let Some(sizes) = share_sizes(&listing.reports) {
                for share in &mut info.shares {
                    share.size_bytes = share.subvolume_id.and_then(|id| sizes.get(&id).copied());
                }
            }
            if info.filesystem == crate::filesystem::FilesystemKind::Ext4
                && let Some((_, header)) = listing
                    .reports
                    .iter()
                    .filter(|(_, header)| {
                        header.roots.iter().any(|root| root.scope == Scope::Volume)
                    })
                    .max_by_key(|(_, header)| header.scan_finished)
                && let Some(overview) = &header.allocated_overview
            {
                for share in &mut info.shares {
                    let name = crate::report::hex_bytes::encode(&share.name.bytes());
                    share.size_bytes = Some(
                        overview
                            .items
                            .iter()
                            .filter(|item| {
                                item.name_hex == name
                                    && matches!(
                                        item.kind,
                                        crate::overview::PartKind::ShareLive
                                            | crate::overview::PartKind::RecycleBin
                                    )
                            })
                            .map(|item| item.bytes)
                            .sum(),
                    );
                }
            }
        }
        volumes.push(info);
    }
    let (_, excluded) = (Vec::<Resolved>::new(), unsupported(&env));
    let mut envelope = Envelope::new("volumes", SystemTime::now());
    envelope.completion = if volumes.is_empty() {
        Completion::Failed
    } else {
        Completion::Complete
    };
    if volumes.is_empty() {
        envelope.warnings.push(Notice::new(
            "no_volumes",
            "no mounted supported data volumes found",
        ));
    }
    let system_partition = space::system_partition(&env.mounts);
    envelope
        .warnings
        .extend(system_partition.as_ref().and_then(SystemPartition::notice));
    let doc = VolumesDocument {
        envelope,
        volumes,
        excluded,
        system_partition,
    };
    emit(json, &doc, || render_volumes(&doc))?;
    Ok(doc.envelope.completion.exit_code(None))
}

fn unsupported(env: &Environment) -> Vec<TargetError> {
    crate::mounts::unsupported_volumes(&env.mounts)
        .into_iter()
        .map(|m| {
            TargetError::new(
                JsonPath::new(m.mount_point.as_os_str().as_bytes()),
                "unsupported_filesystem",
                format!("{} volume, unsupported filesystem", m.fstype),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        FIRST_FREE_OBJECTID, cleanup_failure_document, history_entries, is_recycle_target,
        no_report, recycle_refusal_document, share_sizes, valid_plan_id, volume_shares,
    };
    use crate::btrfs::subvolume::{Link, Subvolume, SubvolumeSet};
    use crate::cli::ScanLimits;
    use crate::document::{Completion, Envelope};
    use crate::history::{self, Outcome, ScanRecord};
    use crate::jobs::{Job, JobState, Live};
    use crate::output::JsonPath;
    use crate::overview::{OverviewData, OwnerBytes};
    use crate::report::{Header, RootInfo, testing::header};
    use crate::target::Scope;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn queued_resume_preserves_the_paused_writer_busy_policy() {
        assert_eq!(super::queued_busy_policy(0), super::OnBusy::Fail);
        assert_eq!(super::queued_busy_policy(1), super::OnBusy::Pause);
    }

    fn queued_cleanup_job(jobs: &std::path::Path) {
        let job = crate::jobs::Job::cleanup_apply(
            "cleanup-1f-2a",
            &[b"/volume1/s/a".to_vec()],
            crate::jobs::JobFilesystem {
                fsid: "00112233-4455-6677-8899-aabbccddeeff".into(),
                mount: b"/volume1".to_vec(),
            },
            crate::resource::Profile::Low,
            crate::time::unix_seconds(std::time::SystemTime::now()),
        );
        crate::jobs::queue(jobs, crate::sys::effective_uid(), job).unwrap();
    }

    fn stale_refusal() -> Envelope {
        let refusal: Box<dyn std::error::Error> =
            Box::new(crate::guard::Refusal::stale("plan expired"));
        cleanup_failure_document(
            "cleanup-apply",
            std::time::UNIX_EPOCH,
            b"/state/cleanup/plans/cleanup-a.json",
            refusal.as_ref(),
        )
    }

    #[test]
    fn a_keeper_conflict_names_the_copy_and_both_groups() {
        let conflict = crate::duplicates::model::KeepConflict {
            kept_in: ("fenv".into(), b"linux-6.6-copy-env".to_vec()),
            copy: b"/volume1/media/src/linux-6.6-copy".to_vec(),
            removed_by: ("fjunk".into(), b"linux-6.6-copy-junk".to_vec()),
        };
        let error = crate::duplicates::command::keeper_conflict_refusal(&conflict);
        let doc = cleanup_failure_document(
            "cleanup-plan",
            std::time::UNIX_EPOCH,
            b"/volume1",
            error.as_ref(),
        );
        let doc = serde_json::to_value(doc).unwrap();
        let error_json = &doc["errors"][0];
        assert_eq!(error_json["code"], "keeper_conflict");
        assert_eq!(error_json["path"], "/volume1/media/src/linux-6.6-copy");
        assert_eq!(
            error_json["conflict"],
            serde_json::json!({
                "kept_in": {"id": "fenv", "name": {"path": "linux-6.6-copy-env"}},
                "removed_by": {"id": "fjunk", "name": {"path": "linux-6.6-copy-junk"}},
            })
        );
        assert_eq!(crate::guard::failure_exit_code(error.as_ref()), 4);
    }

    #[test]
    fn a_share_quota_refusal_names_its_limiting_resource() {
        let refusal =
            crate::guard::Refusal::new("destination_full", "the share quota has 1 MiB available")
                .with_limit("share_quota");
        let doc = cleanup_failure_document(
            "rootfs-plan",
            std::time::UNIX_EPOCH,
            b"/volume1/backup",
            &refusal,
        );

        assert_eq!(doc.errors[0].limit.as_deref(), Some(&"share_quota"));
        assert_eq!(
            serde_json::to_value(doc).unwrap()["errors"][0]["limit"],
            "share_quota"
        );
    }

    #[test]
    fn a_refused_queued_apply_ends_its_job_with_the_refusal() {
        let state = crate::spill::testing::scratch("apply-job-refused");
        let jobs = state.join("jobs");
        queued_cleanup_job(&jobs);
        let mut doc = stale_refusal();
        super::end_queued_job(&jobs, "cleanup-1f-2a", &mut doc, 4).unwrap();
        assert_eq!(doc.job_id.as_deref(), Some("cleanup-1f-2a"));
        let ended = crate::jobs::load(&jobs, "cleanup-1f-2a").unwrap();
        assert_eq!(ended.state, JobState::Failed);
        assert_eq!(ended.reason.as_deref(), Some("plan_stale"));
        let result = ended.result.unwrap();
        assert_eq!(
            (result.completion.as_str(), result.exit_code),
            ("failed", 4)
        );
        assert_eq!(result.error_codes, ["plan_stale"]);
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(jobs.join("cleanup-1f-2a").join(crate::jobs::RESULT)).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["command"], "cleanup-apply");
        assert_eq!(saved["job_id"], "cleanup-1f-2a");
        assert_eq!(saved["errors"][0]["code"], "plan_stale");
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_refusal_leaves_a_job_that_is_not_queued_alone() {
        let state = crate::spill::testing::scratch("apply-job-refused-late");
        let jobs = state.join("jobs");
        let uid = crate::sys::effective_uid();
        super::end_queued_job(&jobs, "cleanup-1f-2a", &mut stale_refusal(), 4).unwrap();
        queued_cleanup_job(&jobs);
        crate::jobs::request(&jobs, uid, "cleanup-1f-2a", crate::jobs::Request::Cancel, 5).unwrap();
        super::end_queued_job(&jobs, "cleanup-1f-2a", &mut stale_refusal(), 4).unwrap();
        let cancelled = crate::jobs::load(&jobs, "cleanup-1f-2a").unwrap();
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert!(cancelled.result.is_none());
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn history_merges_published_scans_with_finished_jobs_and_drops_vanished_reports() {
        let state = crate::spill::testing::scratch("history-entries");
        let uid = crate::sys::effective_uid();
        let report = state.join("reports").join("kept.srrep");
        std::fs::create_dir_all(report.parent().unwrap()).unwrap();
        std::fs::write(&report, b"kept").unwrap();
        let record = |job: &str, finished: u64, report: &std::path::Path| ScanRecord {
            scope_key: "scope".into(),
            volume: true,
            job_id: job.into(),
            targets: vec![JsonPath::new(b"/volume1")],
            profile: Some("low".into()),
            started: finished - 10,
            finished,
            elapsed_ms: 10_000,
            total_elapsed_ms: 10_000,
            scanned_units: 1,
            reused_units: 0,
            full_read_ms: Some(10_000),
            full_read_entries: Some(42),
            entries: 42,
            subvolumes: 1,
            spill_peak_bytes: 0,
            peak_rss_bytes: Some(1 << 20),
            outcome: Outcome::Completed,
            original_outcome: None,
            reason: None,
            report: Some(JsonPath::new(report.as_os_str().as_bytes())),
            report_bytes: Some(1234),
            total_bytes: Some(1 << 30),
            used_bytes: Some(1 << 20),
            free_bytes: Some(1 << 29),
            btrfs_data_used_bytes: Some(1 << 20),
            attribution: None,
        };
        history::record(&history::dir(&state), uid, "fs", record("j1", 100, &report)).unwrap();
        let first = super::history_entries(&state);
        assert_eq!(serde_json::to_value(&first).unwrap()[0]["fsid"], "fs");
        history::record(
            &history::dir(&state),
            uid,
            "fs",
            record("j0", 50, &state.join("gone.srrep")),
        )
        .unwrap();
        let live = Live::start(
            &state.join("jobs"),
            uid,
            Job::new(
                "j2",
                &[b"/volume2".to_vec()],
                Some(Scope::Volume),
                &ScanLimits::report_only(),
                200,
            ),
        )
        .unwrap();
        live.finish(JobState::Cancelled, Some("cancelled"), 260)
            .unwrap();
        let busy = Live::start(
            &state.join("jobs"),
            uid,
            Job::new(
                "j3",
                &[b"/volume1".to_vec()],
                Some(Scope::Volume),
                &ScanLimits::report_only(),
                300,
            ),
        )
        .unwrap();
        busy.finish(JobState::Failed, Some("busy"), 301).unwrap();
        let paused = Live::start(
            &state.join("jobs"),
            uid,
            Job::new(
                "j4",
                &[b"/volume3".to_vec()],
                Some(Scope::Volume),
                &ScanLimits::report_only(),
                400,
            ),
        )
        .unwrap();
        paused
            .finish(JobState::Paused, Some("paused"), 410)
            .unwrap();

        let entries = history_entries(&state);
        let ids: Vec<&str> = entries.iter().map(|e| e.job_id.as_str()).collect();
        assert_eq!(
            ids,
            ["j4", "j2", "j1", "j0"],
            "a busy attempt is not a scan; a paused job is"
        );
        assert_eq!(entries[0].outcome, "paused");
        assert_eq!(entries[0].targets[0].path, "/volume3");
        assert_eq!(entries[1].outcome, "cancelled");
        assert_eq!(entries[1].targets[0].path, "/volume2");
        assert!(entries[1].report.is_none());
        assert_eq!(entries[2].entries, 42);
        assert_eq!(entries[2].total_elapsed_ms, 10_000);
        assert_eq!(entries[2].full_read_ms, Some(10_000));
        assert_eq!(entries[2].df_used_bytes, Some(1 << 20));
        assert_eq!(entries[2].report_bytes, Some(1234));
        assert!(entries[2].report.is_some());
        assert!(entries[3].report.is_none(), "a vanished report is omitted");
        assert!(history_entries(&state.join("nowhere")).is_empty());
    }

    #[test]
    fn history_lists_no_cleanup_apply_as_a_scan() {
        let state = crate::spill::testing::scratch("history-no-cleanup");
        let uid = crate::sys::effective_uid();
        let job = crate::jobs::Job::cleanup_apply(
            "cleanup-1f-2a",
            &[b"/volume1/s/a".to_vec()],
            crate::jobs::JobFilesystem {
                fsid: "f".into(),
                mount: b"/volume1".to_vec(),
            },
            crate::resource::Profile::Low,
            100,
        );
        Live::start(&state.join("jobs"), uid, job)
            .unwrap()
            .finish(JobState::Completed, None, 110)
            .unwrap();
        assert!(
            history_entries(&state).is_empty(),
            "a cleanup apply moves items, it is not a scan; jobs.list shows it"
        );
    }

    fn subvolume(id: u64, name: &[u8], readonly: bool, parent_id: u64) -> Subvolume {
        Subvolume {
            id,
            parent: Some(Link {
                parent_id,
                dirid: FIRST_FREE_OBJECTID,
            }),
            name: name.to_vec(),
            generation: 1,
            readonly,
            pending_deletion: false,
            uuid: None,
            parent_uuid: None,
            last_snapshot: 0,
            otime: None,
        }
    }

    #[test]
    fn volume_shares_are_the_writable_direct_children_with_user_names() {
        let set = SubvolumeSet {
            subvolumes: vec![
                subvolume(257, b"photos", false, crate::btrfs::keys::FS_TREE_OBJECTID),
                subvolume(256, b"backup", false, crate::btrfs::keys::FS_TREE_OBJECTID),
                subvolume(258, b"@docker", false, crate::btrfs::keys::FS_TREE_OBJECTID),
                subvolume(
                    259,
                    b"#recycle",
                    false,
                    crate::btrfs::keys::FS_TREE_OBJECTID,
                ),
                subvolume(260, b"snap", true, crate::btrfs::keys::FS_TREE_OBJECTID),
                subvolume(261, b"nested", false, 256),
            ],
            ..SubvolumeSet::default()
        };
        let shares = volume_shares(
            &set,
            std::path::Path::new("/volume1"),
            crate::btrfs::keys::FS_TREE_OBJECTID,
        );
        let names: Vec<&str> = shares.iter().map(|s| s.name.path.as_str()).collect();
        assert_eq!(names, ["backup", "photos"]);
        assert_eq!(shares[0].path.path, "/volume1/backup");
        assert_eq!(shares[0].subvolume_id, Some(256));
        assert!(shares.iter().all(|s| s.size_bytes.is_none()));
    }

    #[test]
    fn shares_are_relative_to_the_mounted_subvolume_not_the_filesystem_root() {
        // DSM mounts /volume1 as @syno (id 256), so shares are its children.
        let set = SubvolumeSet {
            subvolumes: vec![
                subvolume(257, b"@syno", false, crate::btrfs::keys::FS_TREE_OBJECTID),
                subvolume(258, b"homes", false, 256),
                subvolume(259, b"@docker", false, 256),
                subvolume(256, b"photos", false, crate::btrfs::keys::FS_TREE_OBJECTID),
            ],
            ..SubvolumeSet::default()
        };
        let shares = volume_shares(&set, std::path::Path::new("/volume1"), 256);
        let names: Vec<&str> = shares.iter().map(|s| s.name.path.as_str()).collect();
        assert_eq!(names, ["homes"], "photos is outside the mounted subvolume");
        assert_eq!(shares[0].path.path, "/volume1/homes");
    }

    #[test]
    fn share_sizes_come_from_the_newest_volume_report() {
        let overview = |id: u64, bytes: u64| OverviewData {
            share_live: std::collections::BTreeMap::from([(
                id,
                OwnerBytes {
                    bytes,
                    reclaimable_bytes: 0,
                },
            )]),
            ..OverviewData::default()
        };
        let report = |scope, path: &[u8], tree, overview| {
            let mut header = header(vec![RootInfo {
                path: path.to_vec(),
                scope,
                tree,
                ino: FIRST_FREE_OBJECTID,
            }]);
            header.overview = Some(overview);
            (std::path::PathBuf::from("report"), header)
        };
        let path_report = report(Scope::Path, b"/v/one", 257, overview(257, 9));
        let volume_report = report(Scope::Volume, b"/v", 5, overview(256, 100));
        let sizes = share_sizes(&[path_report, volume_report]).unwrap();
        assert_eq!(sizes, std::collections::BTreeMap::from([(256, 100)]));
        assert!(share_sizes(&[]).is_none());
        assert!(share_sizes(&[(std::path::PathBuf::from("r"), header(Vec::new()))]).is_none());
    }

    #[test]
    fn a_json_request_answer_names_its_outcome_and_the_state_after() {
        use crate::jobs::{JobError, JobState, Request};
        use std::time::UNIX_EPOCH;
        let asked = super::request_answer(
            "j1",
            Request::Cancel,
            &Ok(JobState::Cancelling),
            Some(JobState::Cancelling),
            UNIX_EPOCH,
        );
        assert_eq!(asked.envelope.command, "cancel");
        assert_eq!(asked.envelope.job_id.as_deref(), Some("j1"));
        assert_eq!(
            (asked.outcome, asked.state),
            ("requested", Some("cancelling"))
        );
        assert_eq!(asked.envelope.completion, Completion::Complete);
        let ended = super::request_answer(
            "j1",
            Request::Cancel,
            &Ok(JobState::Cancelled),
            Some(JobState::Cancelled),
            UNIX_EPOCH,
        );
        assert_eq!(ended.outcome, "cancelled");
        let late = super::request_answer(
            "j1",
            Request::Pause,
            &Err(JobError::TooLate("j1".into())),
            Some(JobState::Running),
            UNIX_EPOCH,
        );
        assert_eq!((late.outcome, late.state), ("too_late", Some("running")));
        assert_eq!(late.envelope.completion, Completion::Failed);
        let missing = super::request_answer(
            "nope",
            Request::Pause,
            &Err(JobError::NotFound("nope".into())),
            None,
            UNIX_EPOCH,
        );
        assert_eq!((missing.outcome, missing.state), ("no_job", None));
    }

    #[test]
    fn a_cancel_request_stops_the_scan_as_cancelled() {
        use crate::btrfs::walk::StopReason;
        use crate::jobs::Request;
        assert_eq!(super::stop_for(Request::Pause), StopReason::Paused);
        assert_eq!(super::stop_for(Request::Cancel), StopReason::Cancelled);
        let (completion, code, _) = super::stop_completion(StopReason::Cancelled);
        assert_eq!((completion, code), (Completion::Cancelled, "cancelled"));
    }

    #[test]
    fn simulate_delete_service_hints_are_deduplicated() {
        let paths = vec![
            b"/volume1/@iSCSI/LUN/one".to_vec(),
            b"/volume1/@iSCSI/LUN/two".to_vec(),
        ];
        let hints = super::selected_service_hints(&paths, b"/volume1");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].code, "dsm_service");
        assert!(hints[0].message.contains("UNMAP"));
        assert!(hints[0].message.contains("Virtual Machine Manager"));
    }

    #[test]
    fn a_target_whose_reports_cannot_be_read_is_told_they_are_incompatible() {
        let target = crate::target::Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: "/volume1".into(),
            path: "/volume1".into(),
            scope: crate::target::Scope::Volume,
            fsid: [0; 16],
            mount: "/volume1".into(),
            tree: 256,
            ino: FIRST_FREE_OBJECTID,
        };
        assert_eq!(no_report(&target, "--no-scan given", 0).code, "no_report");
        let error = no_report(&target, "--no-scan given", 2);
        assert_eq!(error.code, "report_incompatible");
        assert!(error.message.contains("2 reports"), "{}", error.message);
    }

    #[test]
    fn recycle_requires_a_volume_or_a_direct_shared_folder_root() {
        let target = |path: &str, scope, tree, ino| crate::target::Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: path.into(),
            path: path.into(),
            scope,
            fsid: [0; 16],
            mount: "/volume1".into(),
            tree,
            ino,
        };
        assert!(is_recycle_target(&target(
            "/volume1",
            crate::target::Scope::Volume,
            256,
            FIRST_FREE_OBJECTID,
        )));
        assert!(is_recycle_target(&target(
            "/volume1/photos",
            crate::target::Scope::Path,
            257,
            FIRST_FREE_OBJECTID,
        )));
        for path in [
            "/volume1/photos/nested",
            "/volume1/@docker",
            "/volume1/#snapshot",
        ] {
            assert!(
                !is_recycle_target(&target(
                    path,
                    crate::target::Scope::Path,
                    257,
                    if path.ends_with("nested") {
                        300
                    } else {
                        FIRST_FREE_OBJECTID
                    },
                )),
                "accepted {path}"
            );
        }
    }

    #[test]
    fn a_recycle_refusal_answers_json_with_a_stable_code() {
        let target = crate::target::Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: "/volume1/photos/nested".into(),
            path: "/volume1/photos/nested".into(),
            scope: crate::target::Scope::Path,
            fsid: [0; 16],
            mount: "/volume1".into(),
            tree: 257,
            ino: 300,
        };
        let doc = recycle_refusal_document(
            Envelope::new("recycle", std::time::UNIX_EPOCH),
            &target.path,
            "not_a_shared_folder",
            "not a share".into(),
        );
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["command"], "recycle");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["completion"], "failed");
        assert_eq!(json["target"]["path"], "/volume1/photos/nested");
        assert_eq!(json["errors"][0]["code"], "not_a_shared_folder");
        assert_eq!(json["errors"][0]["path"], "/volume1/photos/nested");
        assert!(json["report_info"].is_null());
        assert_eq!(json["shares"], serde_json::json!([]));
        assert_eq!(json["scans"], serde_json::json!([]));
    }

    #[test]
    fn plan_ids_are_prefixed_ascii_names_without_separators() {
        assert!(valid_plan_id("cleanup-0123abcd-ff"));
        for bad in [
            "",
            "cleanup-",
            "cleanup-a/b",
            "cleanup-..",
            "../cleanup-a",
            "cleanup-a.b",
            "cleanup-é",
            "other-a",
            "cleanup-a b",
        ] {
            assert!(!valid_plan_id(bad), "{bad:?}");
        }
    }

    fn snapshot_header(ids: &[u64]) -> Header {
        use crate::snapshots::*;
        let mut header = crate::report::testing::header(Vec::new());
        header.snapshots = Some(SnapshotAnalysis {
            depth: Depth::All,
            owner_budget: DEFAULT_OWNER_BUDGET,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            shares: vec![ShareAnalysis {
                live: 256,
                name: b"share".to_vec(),
                subvolume_path: b"share".to_vec(),
                snapshots: ids
                    .iter()
                    .map(|&id| SnapshotInfo {
                        id,
                        name: Vec::new(),
                        subvolume_path: Vec::new(),
                        otime: None,
                        generation: 1,
                        materialized: true,
                    })
                    .collect(),
                pending: Vec::new(),
                held_files: None,
            }],
            holdings: Vec::new(),
            quota: None,
            stats: Default::default(),
        });
        header
    }

    #[test]
    fn snapshot_roots_are_answered_by_the_newest_report_holding_them_all() {
        use std::path::PathBuf;
        let reports = vec![
            (PathBuf::from("new"), snapshot_header(&[301])),
            (
                PathBuf::from("plain"),
                crate::report::testing::header(Vec::new()),
            ),
            (PathBuf::from("old"), snapshot_header(&[301, 302])),
        ];
        assert!(super::is_snapshot_root(&reports, 302, 256));
        assert!(!super::is_snapshot_root(&reports, 302, 257));
        assert!(!super::is_snapshot_root(&reports, 256, 256));
        let pick = |ids: &[u64]| {
            super::snapshot_report(&reports, &ids.iter().copied().collect()).map(|(p, _)| p.clone())
        };
        assert_eq!(pick(&[301]), Some(PathBuf::from("new")));
        assert_eq!(pick(&[301, 302]), Some(PathBuf::from("old")));
        assert_eq!(pick(&[303]), None);
    }

    #[test]
    fn a_cleanup_refusal_answers_json_with_its_code() {
        let refusal: super::Failure = Box::new(crate::guard::Refusal::stale("plan expired"));
        let doc = cleanup_failure_document(
            "cleanup-apply",
            std::time::UNIX_EPOCH,
            b"/state/p.json",
            refusal.as_ref(),
        );
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["command"], "cleanup-apply");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["completion"], "failed");
        assert_eq!(json["errors"][0]["code"], "plan_stale");
        assert_eq!(json["errors"][0]["path"], "/state/p.json");
        assert_eq!(json["errors"][0]["message"], "plan_stale: plan expired");
        let busy: super::Failure = Box::new(crate::guard::Refusal::new("busy", "locked"));
        let doc = cleanup_failure_document(
            "cleanup-plan",
            std::time::UNIX_EPOCH,
            b"/volume1/s/a",
            busy.as_ref(),
        );
        assert_eq!(doc.completion, Completion::Busy);
        let protected: super::Failure = Box::new(crate::guard::AtPath {
            path: b"/volume1/s/b".to_vec(),
            source: Box::new(crate::guard::CleanupError::Protected("DSM data".into())),
        });
        let doc = cleanup_failure_document(
            "cleanup-plan",
            std::time::UNIX_EPOCH,
            b"/volume1/s/a",
            protected.as_ref(),
        );
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["errors"][0]["path"], "/volume1/s/b");
        assert_eq!(json["errors"][0]["code"], "protected_target");
    }

    #[test]
    fn an_answer_from_a_report_a_cleanup_made_stale_says_so() {
        use crate::document::{CleanupMark, Envelope, Marked, ReportInfo};
        let header = crate::report::testing::header(Vec::new());
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(header.scan_finished);
        let info = |marked| {
            ReportInfo::new(b"/s/r", &header, now, std::time::Duration::from_secs(60))
                .with_cleanup(marked)
        };
        let codes = |info: &ReportInfo| {
            let mut envelope = Envelope::new("top", now);
            super::stale_warning(&mut envelope, info);
            envelope
                .warnings
                .iter()
                .map(|w| format!("{}: {}", w.code, w.message))
                .collect::<Vec<_>>()
        };
        let mark = CleanupMark {
            plan_id: "cleanup-1".into(),
            execution_log: "cleanup-1-1.jsonl".into(),
            applied_at: "2026-09-29T12:00:00Z".into(),
        };
        let job = &header.job_id;

        assert_eq!(
            codes(&info(Marked::By(mark))),
            [
                format!(
                    "stale_report: report {job} predates cleanup cleanup-1 applied 2026-09-29T12:00:00Z"
                ),
                format!(
                    "cleanup_since_report: cleanup cleanup-1 removed data after report {job}; rescan for current sizes"
                ),
            ]
        );
        assert_eq!(
            codes(&info(Marked::Unknown)),
            [
                format!(
                    "stale_report: report {job} may predate a cleanup: the cleanup record is unreadable"
                ),
                format!(
                    "cleanup_record_unreadable: the cleanup record of report {job} is unreadable; rescan for current sizes"
                ),
            ]
        );
        assert!(codes(&info(Marked::No)).is_empty());
    }

    #[test]
    fn an_answered_report_reads_the_cleanup_record_beside_it() {
        use crate::document::CleanupMark;
        let uid = crate::sys::effective_uid();
        let store = crate::state::Store::open(
            &crate::spill::testing::scratch("answered").join("state"),
            uid,
        )
        .unwrap();
        let header = crate::report::testing::header(Vec::new());
        let tree = crate::model::tree::ScopeTree::new();
        let (path, _) = store
            .publish(&header, |out| {
                crate::report::testing::write_footprint(out, &header, &tree)
            })
            .unwrap();
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(header.scan_finished);
        let max_age = std::time::Duration::from_secs(60);
        assert!(!super::answered_report(&path, &header, now, max_age).stale);
        let mark = CleanupMark {
            plan_id: "cleanup-1".into(),
            execution_log: "cleanup-1-1.jsonl".into(),
            applied_at: "2026-09-29T12:00:00Z".into(),
        };
        store.record_cleanup(&header.fsid, mark.clone()).unwrap();

        let info = super::answered_report(&path, &header, now, max_age);

        assert!(info.stale);
        assert_eq!(info.cleanup_since_report, Some(mark));
    }
}

#[cfg(test)]
mod nested_top_tests {
    use super::*;

    #[test]
    fn full_json_marks_the_bin_but_reduced_cli_metrics_do_not() {
        let report = report::Report::parse(query::tests::nested_report_bytes()).unwrap();
        let q = TopQuery {
            under: b"/v",
            services: true,
            by: Measure::Physical,
            kind: KindFilter::All,
            include_system: false,
            limit: 200,
            only_shared: false,
        };
        let mut walk = query::TopWalk::new(b"/v", &q);
        report.for_each(|p, node| walk.visit(p, node));
        let answer = walk.finish().unwrap();
        let rank = RankArgs {
            by: q.by,
            kind: q.kind,
            limit: q.limit,
            include_system: false,
            only_shared: false,
            services: true,
            report: None,
        };
        let json = top_rows(b"/v", &answer, &rank, true);
        let bin = json
            .iter()
            .find(|r| r.entry.path.path == "/v/share/#recycle")
            .unwrap();
        assert_eq!(bin.within.as_ref().unwrap().path, "/v/share");
        let cli = top_rows(b"/v", &answer, &rank, false);
        assert!(cli.iter().all(|r| r.within.is_none()));
        let share = cli
            .iter()
            .find(|r| r.entry.path.path == "/v/share")
            .unwrap();
        let cli_total: u64 = cli
            .iter()
            .map(|row| {
                row.entry
                    .without_package_data
                    .as_ref()
                    .unwrap_or(&row.entry.metrics)
                    .physical
            })
            .sum();
        let json_total: u64 = json
            .iter()
            .filter(|row| row.within.is_none())
            .map(|row| row.entry.metrics.physical)
            .sum();
        assert_eq!(cli_total, 13 << 20);
        assert_eq!(json_total, cli_total);
        assert_eq!(share.entry.metrics.physical, 8 << 20);
        assert_eq!(
            share.entry.without_package_data.as_ref().unwrap().physical,
            1 << 20
        );
        let share_query = TopQuery {
            under: b"/v/share",
            ..q
        };
        let mut share_walk = query::TopWalk::new(b"/v", &share_query);
        report.for_each(|p, node| share_walk.visit(p, node));
        let share_answer = share_walk.finish().unwrap();
        assert_eq!(
            top_rows(b"/v", &share_answer, &rank, true),
            top_rows(b"/v", &share_answer, &rank, false),
            "share answers stay unchanged"
        );
    }
}

#[cfg(test)]
mod config_answer_tests {
    use super::config_answer;

    #[test]
    fn an_unresolvable_configuration_answers_the_cli_failure_document() {
        // No SynoReclaim package here: the location cannot resolve.
        let (document, exit) = config_answer(true);
        assert_eq!(document["command"], "config-set");
        assert_eq!(document["completion"], "failed");
        assert_eq!(
            document["errors"][0]["path"],
            crate::config::CONFIG_ALIAS_PATH
        );
        assert_ne!(exit, 0);
    }
}
