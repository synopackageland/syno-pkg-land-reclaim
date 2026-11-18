//! Command-line surface (SPEC section 5).

use crate::btrfs::leaf_ranges::DEFAULT_READ_BUDGET;
use crate::query::KindFilter;
use crate::resource::{MemoryLimit, Profile, parse_duration, parse_size};
use crate::snapshots::answer::Selection;
use crate::snapshots::{
    DEFAULT_GLOBAL_OWNER_BUDGET, DEFAULT_HELD_FILES_BUDGET, DEFAULT_OWNER_BUDGET, Depth,
    SnapshotScan,
};
use crate::target::Scope;
use clap::builder::{OsStringValueParser, TypedValueParser};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// `--state-dir` default: durable and local, not a RAM-backed `/tmp`.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/synoreclaim";
/// Rootfs reports may be reused for at most one day.
pub const ROOTFS_REPORT_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Parser)]
#[command(name = "synoreclaim", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// List local Btrfs volumes and their report state (SPEC 5.2).
    Volumes {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Watch live free-space and Btrfs cleaner counters on a volume (SPEC 5.19).
    WatchFree {
        /// Exact mount point of a Btrfs volume.
        #[arg(value_name = "VOLUME", value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        target: PathBuf,
        /// How long to observe, from 2 seconds to 1 hour.
        #[arg(long, value_parser = parse_watch_duration, default_value = "10m")]
        duration: Duration,
        /// How often to sample, from 2 to 60 seconds.
        #[arg(long, value_parser = parse_watch_interval, default_value = "10s")]
        interval: Duration,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        resources: ResourceArgs,
    },
    /// Scan targets and publish a report (SPEC 5.3).
    Scan {
        #[command(flatten)]
        targets: TargetArgs,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        limits: ScanLimits,
        #[command(flatten)]
        launch: LaunchArgs,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Rank the entries below targets (SPEC 5.4).
    Top {
        #[command(flatten)]
        targets: TargetArgs,
        /// Metric to rank by.
        #[arg(long, value_enum, default_value_t = RankBy::Reclaimable)]
        by: RankBy,
        /// `all`: direct children; `files`/`dirs`: at any depth.
        #[arg(long, value_enum, default_value_t = KindFilter::All)]
        kind: KindFilter,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Also list the detail rows below each DSM service row.
        #[arg(long)]
        include_system: bool,
        /// Human columns after PATH; JSON always has every value.
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            default_values_t = crate::document::Column::DEFAULT
        )]
        columns: Vec<crate::document::Column>,
        /// Print each row's full hint and synoreclaim command under it.
        #[arg(long)]
        explain: bool,
        /// Answer from the report of this scan job instead of the newest
        /// report covering each target.
        #[arg(long, value_name = "JOB_ID")]
        report: Option<String>,
        #[command(flatten)]
        answer: AnswerArgs,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        limits: ScanLimits,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Reconcile where a volume's used space is held (SPEC 5.13).
    Overview {
        #[command(flatten)]
        targets: TargetArgs,
        #[command(flatten)]
        answer: AnswerArgs,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        limits: ScanLimits,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Show metrics of individual paths (SPEC 5.5).
    Inspect {
        #[command(flatten)]
        targets: TargetArgs,
        /// Explain limits and blockers of the values shown.
        #[arg(long)]
        explain: bool,
        #[command(flatten)]
        answer: AnswerArgs,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        limits: ScanLimits,
        #[command(flatten)]
        state: StateArgs,
    },
    /// What deleting all selected paths together would free (SPEC 5.6);
    /// never deletes anything.
    SimulateDelete {
        #[command(flatten)]
        targets: TargetArgs,
        /// Describe the blockers that retain bytes.
        #[arg(long)]
        explain: bool,
        #[command(flatten)]
        answer: AnswerArgs,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        limits: ScanLimits,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Which snapshot holds the space, and what deleting a set of
    /// snapshots frees (SPEC 5.14); never deletes anything.
    Snapshots {
        #[command(flatten)]
        targets: TargetArgs,
        /// Snapshots to delete together, comma-separated: `id:<subvolid>`,
        /// `name:<snapshot>`, `older-than:<duration>` or `all`.
        #[arg(long, value_name = "SELECTORS")]
        simulate_delete: Option<Selection>,
        /// Files held only by snapshots to show per share.
        #[arg(long, default_value_t = 10, value_parser = held_files_limit())]
        limit: usize,
        /// Warn when the report is older than this.
        #[arg(long, value_parser = parse_duration, default_value = "24h")]
        max_age: Duration,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Show the size and reclaimable bytes of real shared-folder Recycle Bins (SPEC 5.16).
    Recycle {
        /// A volume mount or shared-folder root.
        #[arg(value_name = "TARGET", value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        target: PathBuf,
        /// List this many oldest top-level Recycle Bin items by ctime.
        #[arg(long)]
        oldest: Option<usize>,
        /// Answer from the report of this scan job instead of the newest
        /// reports; a volume answer then leaves out newer share reports.
        #[arg(long, value_name = "JOB_ID")]
        report: Option<String>,
        #[command(flatten)]
        answer: AnswerArgs,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        limits: ScanLimits,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Why deleting files did not free space yet (SPEC 5.15); read-only,
    /// from the newest volume report plus cheap live probes.
    WhyNotFreed {
        /// Btrfs volume mount.
        #[arg(value_name = "VOLUME", value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        target: PathBuf,
        /// Ask for what changed within this window; rows say whether they
        /// could filter by it.
        #[arg(long, value_parser = parse_duration)]
        since: Option<Duration>,
        /// Also scan /proc once for deleted files processes still hold.
        #[arg(long)]
        processes: bool,
        /// Files held only by snapshots to name per share.
        #[arg(long, default_value_t = 10, value_parser = held_files_limit())]
        limit: usize,
        /// Warn when the report is older than this.
        #[arg(long, value_parser = parse_duration, default_value = "24h")]
        max_age: Duration,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Move explicitly selected items into their shared folder's Recycle
    /// Bin (SPEC 5.7).
    Cleanup {
        #[command(subcommand)]
        command: CleanupCommand,
    },
    /// Rank the entries below targets whose footprint is shared, and
    /// what shares it (SPEC 5.8); shared data is not waste.
    Shared {
        #[command(flatten)]
        targets: TargetArgs,
        /// Metric to rank by.
        #[arg(long, value_enum, default_value_t = ShareBy::Shared)]
        by: ShareBy,
        /// `all`: direct children; `files`/`dirs`: at any depth.
        #[arg(long, value_enum, default_value_t = KindFilter::All)]
        kind: KindFilter,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Also rank DSM-managed entries.
        #[arg(long)]
        include_system: bool,
        #[command(flatten)]
        answer: AnswerArgs,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        limits: ScanLimits,
        #[command(flatten)]
        state: StateArgs,
    },
    /// List scan jobs: running, paused, interrupted and recent finished
    /// ones (SPEC 5.11, 22.1). Never starts work. With a job ID, shows
    /// only that job, including its live progress.
    Jobs {
        job_id: Option<String>,
        #[arg(long)]
        json: bool,
        /// Print the whole document of job ID's latest run instead.
        #[arg(long, value_name = "ID", conflicts_with = "job_id")]
        result: Option<String>,
        #[command(flatten)]
        state: StateArgs,
    },
    /// One entry per completed scan or finished job, newest first
    /// (SPEC 5.20). Never starts work.
    History {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Run a paused or interrupted job again in the foreground (SPEC
    /// 22.1): completed units and the saved search position of a partial
    /// Btrfs tree continue under the same job ID.
    Resume {
        job_id: String,
        #[command(flatten)]
        limits: ResumeLimits,
        /// Check the job can resume, print one JSON line naming what
        /// runs it, and have LAUNCHER (`self`, or `none` for an external
        /// one) run that; only `--profile` may change.
        #[arg(
            long,
            value_enum,
            value_name = "LAUNCHER",
            num_args = 0..=1,
            default_missing_value = "self",
            require_equals = true,
            conflicts_with_all = ["max_memory", "max_runtime", "min_free"]
        )]
        detach: Option<Launcher>,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Ask a running job to checkpoint and exit (SPEC 22.1).
    Pause {
        job_id: String,
        /// Answer with a JSON document, refusals included.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Switch a running scan to another profile from its next check; it
    /// keeps its progress and only its priority changes.
    Mode {
        job_id: String,
        #[arg(long, value_enum)]
        profile: Profile,
        /// Answer with a JSON document, refusals included.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Stop a job and discard its unfinished work; completed units and
    /// user files stay (SPEC 22.1).
    Cancel {
        job_id: String,
        /// Answer with a JSON document, refusals included.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Write, change or show the root-owned cleanup configuration
    /// (SPEC 5.7); root only.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Show or delete kept scan units (SPEC 5.9); deleting them changes
    /// only the work later scans do.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Read-only environment diagnostics for a Btrfs target (SPEC 5.10).
    Doctor {
        /// Volume mount or path on the Btrfs filesystem to diagnose.
        target: PathBuf,
        /// Print one JSON document instead of text.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        resources: ResourceArgs,
        /// Checked for whether its writes land on the target's filesystem.
        #[command(flatten)]
        state: StateArgs,
        /// The --min-free of the scans to check the state filesystem for.
        #[arg(long, value_parser = parse_size, default_value = "512MiB")]
        min_free: u64,
    },
    /// Write a privacy-preserving ticket bundle from an existing volume report (SPEC 5.17).
    SupportBundle {
        /// Btrfs volume mount.
        #[arg(value_name = "VOLUME", value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        target: PathBuf,
        /// New output file, or `-` for stdout.
        #[arg(long, required = true)]
        output: PathBuf,
        /// Keep real path names in the bundle.
        #[arg(long)]
        include_paths: bool,
        /// Maximum JSON size, default 1 MiB.
        #[arg(long, value_parser = parse_size, default_value = "1MiB")]
        max_bytes: u64,
        #[command(flatten)]
        state: StateArgs,
    },
    /// What fills DSM's system partition (`/`, md0), ranked, and what
    /// could be moved off it; read-only and no Btrfs scan.
    #[command(args_conflicts_with_subcommands = true)]
    Rootfs {
        #[command(subcommand)]
        command: Option<RootfsCommand>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        resources: ResourceArgs,
        /// Reuse a recent JSON report, at most 30 days old; 0 forces a fresh walk.
        #[arg(long, default_value_t = 0, value_parser = parse_rootfs_max_age_s)]
        max_age_s: u64,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Files with the same content stored twice on a Btrfs volume, from
    /// metadata only (docs/duplicates.md).
    #[command(subcommand)]
    Duplicates(DuplicatesCommand),
    /// Developer tools for metadata scanning (not a stable interface).
    #[command(subcommand, hide = true)]
    Debug(DebugCommand),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum DuplicatesCommand {
    /// Analyse the newest whole-volume report: group same-content files
    /// by their data checksums and measure what deleting copies frees.
    /// Reads metadata only; saves the result beside the report.
    Analyze {
        #[arg(value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        volume: PathBuf,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Totals, child folders and one page of duplicate groups of a scope.
    Scope {
        #[arg(value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        volume: PathBuf,
        /// A folder on the volume; the whole volume when left out.
        #[arg(long, require_equals = true)]
        path: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "files", require_equals = true)]
        list: crate::duplicates::command::List,
        #[arg(long, value_enum, require_equals = true)]
        category: Option<DuplicateCategory>,
        #[arg(long, default_value_t = 0, require_equals = true)]
        offset: u32,
        #[arg(long, default_value_t = 50, require_equals = true,
              value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Every copy of one group with its role in a scope.
    Copies {
        #[arg(value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        volume: PathBuf,
        #[arg(long, require_equals = true)]
        group: String,
        #[arg(long, require_equals = true)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = 0, require_equals = true)]
        offset: u32,
        #[arg(long, default_value_t = 50, require_equals = true,
              value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Keep one copy of a group (or stop keeping it); kept on the NAS.
    Keep {
        #[arg(value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        volume: PathBuf,
        #[arg(long, require_equals = true)]
        group: String,
        /// The copy's path (a folder's for a folder group).
        #[arg(long, require_equals = true)]
        copy: PathBuf,
        #[arg(long, required = true, action = clap::ArgAction::Set, value_name = "on|off",
              require_equals = true, value_parser = clap::builder::BoolishValueParser::new())]
        keep: bool,
        /// The scope the answer shows the group in.
        #[arg(long, require_equals = true)]
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Show or replace the ordered shares whose copies are kept first.
    PreferredShares {
        #[arg(value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        volume: PathBuf,
        /// A JSON list of share names, first preferred first.
        #[arg(long, require_equals = true)]
        shares: Option<String>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
}

/// A file category of the duplicates lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DuplicateCategory {
    Photos,
    Videos,
    Music,
    Other,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum CacheAction {
    /// List the kept units of the targets' filesystems.
    Status(CacheArgs),
    /// Delete the kept units of the targets' filesystems.
    Clear(CacheArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct CacheArgs {
    #[command(flatten)]
    pub targets: TargetArgs,
    #[command(flatten)]
    pub output: OutputArgs,
    #[command(flatten)]
    pub state: StateArgs,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum ConfigAction {
    /// Write the default configuration (cleanup enabled) unless a trusted
    /// one exists; the package's postinst runs it.
    Init {
        #[arg(long)]
        json: bool,
    },
    /// Turn every cleanup mutation off (on) or back on (off).
    Set {
        #[arg(long, required = true, action = clap::ArgAction::Set, value_name = "on|off",
              value_parser = clap::builder::BoolishValueParser::new())]
        mutations_disabled: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show the configuration and whether it enables cleanup.
    Show {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum RootfsCommand {
    /// Plan moving the allowlisted leftovers off `/` into a shared
    /// folder; writes only the plan, into root's private
    /// /var/lib/synoreclaim/rootfs-plans, and moves nothing.
    Plan {
        /// A DSM shared folder root or folder below one on a data volume,
        /// such as /volume1/backup or /volume1/backup/rootfs.
        #[arg(long, value_name = "FOLDER")]
        to: PathBuf,
        /// Standard retention, or explicitly include recent diagnostic archives.
        #[arg(long, value_enum, default_value = "standard")]
        mode: crate::rootfs::plan::CleanupMode,
        /// Print one JSON document instead of text.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        resources: ResourceArgs,
    },
    /// Carry out a rootfs plan: move each item into the plan's folder,
    /// verified, then remove it from `/`, then run the DSM tools it
    /// lists. Needs --yes; refused while `config set --mutations-disabled
    /// on` holds.
    Apply {
        /// A plan file that `rootfs plan` wrote into
        /// /var/lib/synoreclaim/rootfs-plans; a plan anywhere else is refused.
        #[arg(value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        plan: PathBuf,
        #[arg(long)]
        yes: bool,
        /// Print one JSON document instead of text.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        resources: ResourceArgs,
        #[command(flatten)]
        launch: RootfsLaunchArgs,
        #[command(flatten)]
        state: StateArgs,
    },
    /// List previous rootfs applies; never starts work.
    History {
        /// Print one JSON document instead of text.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
}

/// Launcher options for a rootfs apply job (SPEC 5.21, 5.23).
#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
pub struct RootfsLaunchArgs {
    /// Queue the apply as a job; `none` leaves it to the DSM package launcher.
    #[arg(
        long,
        value_enum,
        value_name = "LAUNCHER",
        num_args = 0..=1,
        default_missing_value = "self",
        require_equals = true,
        requires = "yes",
        conflicts_with = "job_id"
    )]
    pub detach: Option<Launcher>,
    /// Run this already-queued rootfs apply here.
    #[arg(
        long,
        hide = true,
        value_name = "ID",
        requires = "yes",
        conflicts_with = "detach"
    )]
    pub job_id: Option<String>,
    /// DSM login for the audit record; set by the WebAPI mapper.
    #[arg(long, hide = true, value_name = "USER")]
    pub requested_by: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum CleanupCommand {
    /// Stage one chunk of a selection in root's state, for `plan
    /// --selection`: a selection of any size, chunk by chunk.
    Select {
        /// The selection to add to; a new one when left out.
        #[arg(long, value_name = "ID")]
        selection: Option<String>,
        /// Paths and duplicate groups as JSON
        /// `{paths: [{path: hex bytes, objectid?}], groups: [id…]}`.
        #[arg(long, value_name = "JSON")]
        chunk: String,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Check the selection and save a plan; this command moves nothing.
    Plan {
        #[command(flatten)]
        targets: TargetArgs,
        /// A selection `cleanup select` staged; the plan uses it up.
        #[arg(long, value_name = "ID")]
        selection: Option<String>,
        /// Report identities as a JSON array of {path: hex bytes, expect: identity}.
        #[arg(long, value_name = "JSON")]
        expect: Option<String>,
        /// Duplicate groups to clean, each on its whole volume, as JSON
        /// `{volume, groups: [id…]}` (docs/duplicates.md).
        #[arg(long, value_name = "JSON")]
        duplicates: Option<String>,
        /// Also save the private plan to this new file.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        quiet: bool,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Check a saved plan again and move its items into their shared
    /// folders' Recycle Bins.
    Apply {
        #[arg(value_parser = OsStringValueParser::new().try_map(non_empty_path))]
        plan: PathBuf,
        #[arg(long)]
        yes: bool,
        /// Also remove for good the plan's items in no shared folder, which
        /// have no Recycle Bin; without it such a plan is refused.
        #[arg(long)]
        confirm_delete: bool,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        resources: ResourceArgs,
        /// The DSM user a `webapi` call applied the plan for (audit).
        #[arg(long, hide = true, value_name = "USER")]
        requested_by: Option<String>,
        /// Queue the apply as a cleanup job, print one JSON line naming what
        /// runs it, and have LAUNCHER run that: `self` (default) or `none`
        /// (SPEC 5.7).
        #[arg(
            long,
            value_enum,
            value_name = "LAUNCHER",
            num_args = 0..=1,
            default_missing_value = "self",
            require_equals = true,
            requires = "yes",
            conflicts_with = "job_id"
        )]
        detach: Option<Launcher>,
        /// Run as the queued cleanup job `--detach` made (SPEC 5.7).
        #[arg(long, hide = true, value_name = "ID", requires = "yes")]
        job_id: Option<String>,
        #[command(flatten)]
        state: StateArgs,
    },
    /// Plan, confirm and apply in one step.
    Delete {
        #[command(flatten)]
        targets: TargetArgs,
        #[arg(long)]
        yes: bool,
        /// Also save the plan to this new file.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        quiet: bool,
        #[command(flatten)]
        resources: ResourceArgs,
        #[command(flatten)]
        state: StateArgs,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum DebugCommand {
    /// Reconstruct the namespace below a directory from raw metadata and
    /// print it in `scripts/fixture/listing.sh` format.
    Tree {
        path: PathBuf,
        #[command(flatten)]
        resources: ResourceArgs,
    },
    /// Record the raw TREE_SEARCH_V2 buffers of subvolume discovery plus
    /// the walk of the subvolume containing a path, as `debug tree` does.
    Record {
        /// A path inside the subvolume to record.
        path: PathBuf,
        /// Recording file to create.
        #[arg(long, short)]
        output: PathBuf,
        #[command(flatten)]
        resources: ResourceArgs,
    },
    /// Rewrite a recording without host-identifying root-tree metadata
    /// (subvolume names, UUIDs, times), for checking it in as golden data.
    Sanitise {
        /// Recording made by `debug record`.
        input: PathBuf,
        /// Sanitised recording to create.
        #[arg(long, short)]
        output: PathBuf,
    },
}

/// An empty operand would resolve to the working directory (SPEC 22.7).
fn non_empty_operand(value: OsString) -> Result<OsString, &'static str> {
    if value.is_empty() {
        return Err("an empty path is not a target");
    }
    Ok(value)
}

fn parse_watch_duration(text: &str) -> Result<Duration, String> {
    bounded_duration(text, 2, 3600, "duration", "2s to 1h")
}

fn parse_rootfs_max_age_s(text: &str) -> Result<u64, String> {
    let seconds = text
        .parse::<u64>()
        .map_err(|_| "--max-age-s must be an integer number of seconds".to_string())?;
    if seconds <= ROOTFS_REPORT_MAX_AGE_SECS {
        Ok(seconds)
    } else {
        Err(format!(
            "--max-age-s must be at most {ROOTFS_REPORT_MAX_AGE_SECS}"
        ))
    }
}

fn parse_watch_interval(text: &str) -> Result<Duration, String> {
    bounded_duration(text, 2, 60, "interval", "2s to 60s")
}

fn bounded_duration(
    text: &str,
    minimum: u64,
    maximum: u64,
    name: &str,
    range: &str,
) -> Result<Duration, String> {
    let duration = parse_duration(text)?;
    if (minimum..=maximum).contains(&duration.as_secs()) {
        Ok(duration)
    } else {
        Err(format!("--{name} must be within {range}"))
    }
}

fn non_empty_path(value: OsString) -> Result<PathBuf, &'static str> {
    non_empty_operand(value).map(PathBuf::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct ResourceArgs {
    /// Scheduling and pacing profile (SPEC 22.3).
    #[arg(long, value_enum, default_value_t = Profile::Low)]
    pub profile: Profile,
}

/// Targets (SPEC 5.1): `system`, volume mounts, or paths.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct TargetArgs {
    /// `system`, a volume such as `/volume1`, or paths; a path named
    /// `system` is written `./system`.
    #[arg(value_name = "TARGET", value_parser = OsStringValueParser::new().try_map(non_empty_operand))]
    pub operands: Vec<OsString>,
    /// Read more paths from a file (`-` for stdin), one per line.
    #[arg(long, value_name = "PATH")]
    pub from_file: Option<PathBuf>,
    /// Paths in `--from-file` are NUL-separated.
    #[arg(long, requires = "from_file")]
    pub null: bool,
    /// Require each target to be a whole volume or a subtree.
    #[arg(long, value_enum)]
    pub scope: Option<Scope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct OutputArgs {
    /// Print one JSON document.
    #[arg(long)]
    pub json: bool,
    /// No progress on stderr.
    #[arg(long)]
    pub quiet: bool,
}

/// A byte-safe saved scan identifier; never a user-supplied filesystem path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotReport([u8; 64]);
impl SnapshotReport {
    pub fn as_str(&self) -> &str {
        let len = self.0.iter().position(|&b| b == 0).unwrap_or(64);
        std::str::from_utf8(&self.0[..len]).expect("validated ASCII id")
    }
}
impl std::str::FromStr for SnapshotReport {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid saved scan id".into());
        }
        let mut bytes = [0; 64];
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }
}

impl serde::Serialize for SnapshotReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> serde::Deserialize<'de> for SnapshotReport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Where a scan runs (SPEC 5.3): here, or as a job another process runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
pub struct LaunchArgs {
    /// Queue the scan as a job, print its ID as one JSON line and return;
    /// `self` runs it in a new session, `none` leaves it to a launcher
    /// that runs `scan --job-id`.
    #[arg(
        long,
        value_enum,
        value_name = "LAUNCHER",
        num_args = 0..=1,
        default_missing_value = "self",
        require_equals = true
    )]
    pub detach: Option<Launcher>,
    /// Run a queued job here, with the targets and limits it was queued with.
    #[arg(
        long,
        value_name = "ID",
        conflicts_with_all = [
            "operands", "from_file", "scope", "detach", "profile", "max_memory",
            "max_runtime", "no_cache", "max_cache", "min_free", "snapshots", "snapshot_report", "attribution_report",
            "owner_budget", "global_owner_budget", "held_files_budget",
            "pinned_read_budget", "requested_by",
        ]
    )]
    pub job_id: Option<String>,
    /// The DSM user a `webapi` call started the job for (audit).
    #[arg(long, hide = true, value_name = "USER")]
    pub requested_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Launcher {
    /// This binary, in a new session.
    #[value(name = "self")]
    Process,
    /// An external launcher, such as a service manager.
    #[value(name = "none")]
    External,
}

/// Scan resources (SPEC 5.11, 22.3, 22.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct ScanLimits {
    /// Scheduling, pacing and leaf-read parallelism (SPEC 22.3). Every
    /// profile gives the same result while `--pinned-read-budget` lasts.
    #[arg(long, value_enum, default_value_t = Profile::Low)]
    pub profile: Profile,
    /// Working-set budget: `auto` or a size such as `256MiB`.
    #[arg(long, default_value = "auto")]
    pub max_memory: MemoryLimit,
    /// Stop cooperatively after this long, such as `30m`.
    #[arg(long, value_parser = parse_duration)]
    pub max_runtime: Option<Duration>,
    /// Keep no scanned units for later scans or resumes.
    #[arg(long)]
    pub no_cache: bool,
    /// Disk for kept scan units per filesystem, such as `1GiB`.
    #[arg(long, value_parser = parse_size, default_value = "1GiB")]
    pub max_cache: u64,
    /// Space to leave available on the state filesystem.
    #[arg(long, value_parser = parse_size, default_value = "512MiB")]
    pub min_free: u64,
    /// Also read each share's newest N snapshots, or `all` (SPEC 5.14).
    #[arg(
        long,
        value_name = "N|all",
        num_args = 0..=1,
        default_missing_value = "64",
        value_parser = parse_depth
    )]
    pub snapshots: Option<Depth>,
    /// Add snapshot analysis to this saved report without walking its live namespace.
    #[arg(long, requires = "snapshots")]
    pub snapshot_report: Option<SnapshotReport>,
    /// Refine a completed volume report using its deferred metadata work
    #[arg(long, conflicts_with_all = ["snapshots", "snapshot_report"])]
    pub attribution_report: Option<SnapshotReport>,
    /// Tree-block lookups the owner walk of one extent may make.
    #[arg(long, default_value_t = DEFAULT_OWNER_BUDGET)]
    pub owner_budget: u32,
    /// Total tree-block lookups allowed in one snapshot analysis.
    #[arg(long = "owner-budget-global", default_value_t = DEFAULT_GLOBAL_OWNER_BUDGET)]
    pub global_owner_budget: u64,
    /// Files with snapshot-only data tallied per share at most (SPEC 5.14).
    #[arg(long, default_value_t = DEFAULT_HELD_FILES_BUDGET)]
    pub held_files_budget: u64,
    /// Tree-block reads and root searches a volume overview may make to
    /// measure the pinned bytes of references it did not scan.
    #[arg(long, default_value = "auto", value_parser = parse_pinned_read_budget)]
    pub pinned_read_budget: u64,
}

impl ScanLimits {
    /// The defaults, for callers that answer from existing reports only
    /// and never scan.
    pub fn report_only() -> Self {
        ScanLimits {
            profile: Profile::Low,
            max_memory: MemoryLimit::Auto,
            max_runtime: None,
            no_cache: true,
            max_cache: 1 << 30,
            min_free: 512 << 20,
            snapshots: None,
            snapshot_report: None,
            attribution_report: None,
            owner_budget: DEFAULT_OWNER_BUDGET,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            held_files_budget: DEFAULT_HELD_FILES_BUDGET,
            pinned_read_budget: DEFAULT_READ_BUDGET,
        }
    }

    /// What `--snapshots` asks the scan to read.
    pub fn snapshot_scan(&self) -> Option<SnapshotScan> {
        self.snapshots.map(|depth| SnapshotScan {
            depth,
            owner_budget: self.owner_budget,
            global_owner_budget: self.global_owner_budget,
            held_files_budget: self.held_files_budget,
        })
    }
}

fn parse_pinned_read_budget(value: &str) -> Result<u64, String> {
    if value == "auto" {
        return Ok(DEFAULT_READ_BUDGET);
    }
    value
        .parse()
        .map_err(|_| "expected auto or a nonnegative metadata-read count".into())
}

/// 1 to the number of held files a report keeps.
fn held_files_limit() -> clap::builder::RangedU64ValueParser<usize> {
    clap::builder::RangedU64ValueParser::new().range(1..=crate::snapshots::HELD_FILES_KEPT as u64)
}

fn parse_depth(text: &str) -> Result<Depth, String> {
    match text {
        "all" => Ok(Depth::All),
        _ => match text.parse::<u32>() {
            Ok(n) if n > 0 => Ok(Depth::Newest(n)),
            _ => Err(format!("expected a positive count or `all`, not {text:?}")),
        },
    }
}

/// Limits a resume may change; the job keeps the others and its targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct ResumeLimits {
    /// Start a new scan with this job's saved targets and limits, without its checkpoint.
    #[arg(long)]
    pub fresh: bool,
    #[arg(long, value_enum)]
    pub profile: Option<Profile>,
    #[arg(long)]
    pub max_memory: Option<MemoryLimit>,
    #[arg(long, value_parser = parse_duration)]
    pub max_runtime: Option<Duration>,
    /// Space to leave available on the state filesystem.
    #[arg(long, value_parser = parse_size)]
    pub min_free: Option<u64>,
}

impl ResumeLimits {
    pub fn apply(self, limits: ScanLimits) -> ScanLimits {
        ScanLimits {
            profile: self.profile.unwrap_or(limits.profile),
            max_memory: self.max_memory.unwrap_or(limits.max_memory),
            max_runtime: self.max_runtime.or(limits.max_runtime),
            min_free: self.min_free.unwrap_or(limits.min_free),
            ..limits
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct StateArgs {
    /// Private directory for reports.
    #[arg(long, default_value = DEFAULT_STATE_DIR)]
    pub state_dir: PathBuf,
}

/// Which report answers (SPEC 5.1 "Common report behavior").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Args)]
pub struct AnswerArgs {
    /// Scan before answering.
    #[arg(long, conflicts_with = "no_scan")]
    pub refresh: bool,
    /// Never scan; answer only from an existing report.
    #[arg(long)]
    pub no_scan: bool,
    /// Warn when the report is older than this.
    #[arg(long, value_parser = parse_duration, default_value = "24h")]
    pub max_age: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RankBy {
    Reclaimable,
    Logical,
    Physical,
    Shared,
    /// Bytes deleting the entry would not free.
    Held,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShareBy {
    /// Shared bytes.
    Shared,
    /// Shared bytes per footprint byte.
    Ratio,
    /// Logical size.
    Logical,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(args: &[&str]) -> Command {
        Cli::try_parse_from(args).unwrap().command
    }

    #[test]
    fn parses_rootfs_plan_with_its_destination() {
        assert_eq!(
            parse(&[
                "synoreclaim",
                "rootfs",
                "plan",
                "--to",
                "/volume1/backup/rootfs",
                "--json"
            ]),
            Command::Rootfs {
                command: Some(RootfsCommand::Plan {
                    to: PathBuf::from("/volume1/backup/rootfs"),
                    mode: crate::rootfs::plan::CleanupMode::Standard,
                    json: true,
                    resources: ResourceArgs {
                        profile: Profile::Low
                    },
                }),
                json: false,
                resources: ResourceArgs {
                    profile: Profile::Low
                },
                max_age_s: 0,
                state: StateArgs {
                    state_dir: PathBuf::from(DEFAULT_STATE_DIR)
                },
            }
        );
        assert!(Cli::try_parse_from(["synoreclaim", "rootfs", "plan"]).is_err());
        assert!(
            Cli::try_parse_from(["synoreclaim", "rootfs", "--json", "plan", "--to", "/x"]).is_err()
        );
    }

    #[test]
    fn parses_rootfs_apply_with_its_plan_and_confirmation() {
        assert!(matches!(
            parse(&["synoreclaim", "rootfs", "apply", "/volume1/b/p.json", "--yes", "--json"]),
            Command::Rootfs {
                command: Some(RootfsCommand::Apply {
                    yes: true,
                    json: true,
                    ref plan,
                    ..
                }),
                ..
            } if plan == Path::new("/volume1/b/p.json")
        ));
        assert!(matches!(
            parse(&["synoreclaim", "rootfs", "apply", "p.json"]),
            Command::Rootfs {
                command: Some(RootfsCommand::Apply { yes: false, .. }),
                ..
            }
        ));
        assert!(Cli::try_parse_from(["synoreclaim", "rootfs", "apply"]).is_err());
    }

    #[test]
    fn parses_detached_rootfs_apply_and_history_state_directory() {
        assert!(matches!(
            parse(&[
                "synoreclaim",
                "rootfs",
                "apply",
                "/var/lib/synoreclaim/rootfs-plans/rootfs-1-2.json",
                "--yes",
                "--detach=none",
                "--requested-by=admin",
                "--state-dir=/state"
            ]),
            Command::Rootfs {
                command: Some(RootfsCommand::Apply {
                    launch: RootfsLaunchArgs {
                        detach: Some(Launcher::External),
                        requested_by: Some(ref user),
                        ..
                    },
                    state: StateArgs { ref state_dir },
                    ..
                }),
                ..
            } if user == "admin" && state_dir == Path::new("/state")
        ));
        assert!(matches!(
            parse(&["synoreclaim", "rootfs", "history", "--json", "--state-dir", "/state"]),
            Command::Rootfs {
                command: Some(RootfsCommand::History {
                    json: true,
                    state: StateArgs { ref state_dir },
                }),
                ..
            } if state_dir == Path::new("/state")
        ));
    }

    #[test]
    fn parses_the_rootfs_report_with_low_profile_by_default() {
        assert_eq!(
            parse(&["synoreclaim", "rootfs"]),
            Command::Rootfs {
                command: None,
                json: false,
                resources: ResourceArgs {
                    profile: Profile::Low
                },
                max_age_s: 0,
                state: StateArgs {
                    state_dir: PathBuf::from(DEFAULT_STATE_DIR)
                },
            }
        );
        assert!(matches!(
            parse(&["synoreclaim", "rootfs", "--json", "--profile", "normal"]),
            Command::Rootfs {
                json: true,
                resources: ResourceArgs {
                    profile: Profile::Normal
                },
                ..
            }
        ));
    }

    #[test]
    fn parses_doctor_with_low_profile_by_default() {
        assert_eq!(
            parse(&["synoreclaim", "doctor", "/volume1"]),
            Command::Doctor {
                target: PathBuf::from("/volume1"),
                json: false,
                resources: ResourceArgs {
                    profile: Profile::Low
                },
                state: StateArgs {
                    state_dir: PathBuf::from(DEFAULT_STATE_DIR)
                },
                min_free: 512 << 20,
            }
        );
        let cmd = parse(&["synoreclaim", "doctor", "/volume1", "--min-free", "1G"]);
        assert!(matches!(cmd, Command::Doctor { min_free, .. } if min_free == 1 << 30));
    }

    #[test]
    fn parses_doctor_state_dir() {
        let cmd = parse(&["synoreclaim", "doctor", "/volume1", "--state-dir", "/s"]);
        assert!(matches!(
            cmd,
            Command::Doctor { state, .. } if state.state_dir.as_os_str() == "/s"
        ));
    }

    #[test]
    fn parses_doctor_json_and_profile() {
        let cmd = parse(&[
            "synoreclaim",
            "doctor",
            "/volume1",
            "--json",
            "--profile",
            "normal",
        ]);
        assert!(matches!(
            cmd,
            Command::Doctor {
                json: true,
                resources: ResourceArgs {
                    profile: Profile::Normal
                },
                ..
            }
        ));
    }

    #[test]
    fn parses_support_bundle_with_private_defaults_and_path_opt_in() {
        assert!(matches!(
            parse(&[
                "synoreclaim",
                "support-bundle",
                "/volume1",
                "--output",
                "bundle.json"
            ]),
            Command::SupportBundle {
                target,
                output,
                include_paths: false,
                max_bytes: 1_048_576,
                state: StateArgs { ref state_dir },
            } if target == Path::new("/volume1")
                && output == Path::new("bundle.json")
                && state_dir == &PathBuf::from(DEFAULT_STATE_DIR)
        ));

        assert!(matches!(
            parse(&[
                "synoreclaim",
                "support-bundle",
                "/volume1",
                "--output",
                "-",
                "--include-paths",
                "--max-bytes",
                "512K"
            ]),
            Command::SupportBundle {
                output,
                include_paths: true,
                max_bytes,
                ..
            } if output == Path::new("-") && max_bytes == (512 << 10)
        ));
        assert!(Cli::try_parse_from(["synoreclaim", "support-bundle", "/volume1"]).is_err());
    }

    #[test]
    fn parses_debug_tree() {
        assert!(matches!(
            parse(&["synoreclaim", "debug", "tree", "/volume1/x"]),
            Command::Debug(DebugCommand::Tree { .. })
        ));
    }

    #[test]
    fn parses_debug_record() {
        let cmd = parse(&["synoreclaim", "debug", "record", "/v/x", "-o", "out.srbuf"]);
        assert_eq!(
            cmd,
            Command::Debug(DebugCommand::Record {
                path: PathBuf::from("/v/x"),
                output: PathBuf::from("out.srbuf"),
                resources: ResourceArgs {
                    profile: Profile::Low
                },
            })
        );
    }

    #[test]
    fn rootfs_report_accepts_a_bounded_cache_age_and_state_dir() {
        assert!(
            Cli::try_parse_from([
                "synoreclaim",
                "rootfs",
                "--json",
                "--max-age-s",
                "30",
                "--state-dir",
                "/state",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["synoreclaim", "rootfs", "--max-age-s", "2592001",]).is_err());
    }

    #[test]
    fn parses_debug_sanitise() {
        let cmd = parse(&[
            "synoreclaim",
            "debug",
            "sanitise",
            "in.srbuf",
            "-o",
            "out.srbuf",
        ]);
        assert_eq!(
            cmd,
            Command::Debug(DebugCommand::Sanitise {
                input: PathBuf::from("in.srbuf"),
                output: PathBuf::from("out.srbuf"),
            })
        );
    }

    #[test]
    fn parses_scan_targets_and_limits() {
        let cmd = parse(&[
            "synoreclaim",
            "scan",
            "/volume1/a",
            "--max-memory",
            "256MiB",
            "--max-runtime",
            "30m",
            "--json",
            "--",
            "system",
        ]);
        let Command::Scan {
            targets,
            output,
            limits,
            state,
            ..
        } = cmd
        else {
            panic!("not scan: {cmd:?}");
        };
        assert_eq!(
            targets.operands,
            [OsString::from("/volume1/a"), OsString::from("system")]
        );
        assert!(output.json && !output.quiet);
        assert_eq!(limits.profile, Profile::Low);
        assert_eq!(limits.max_memory, MemoryLimit::Bytes(256 << 20));
        assert_eq!(limits.max_runtime, Some(Duration::from_secs(1800)));
        assert_eq!(state.state_dir, PathBuf::from("/var/lib/synoreclaim"));
    }

    fn scan_limits(args: &[&str]) -> ScanLimits {
        match parse(&[&["synoreclaim", "scan", "/volume1"], args].concat()) {
            Command::Scan { limits, .. } => limits,
            cmd => panic!("not scan: {cmd:?}"),
        }
    }

    #[test]
    fn scans_keep_units_in_a_bounded_cache_unless_told_not_to() {
        let defaults = scan_limits(&[]);
        assert!(!defaults.no_cache);
        assert_eq!(defaults.max_cache, 1 << 30);
        assert_eq!(defaults.min_free, 512 << 20);
        let set = scan_limits(&["--no-cache", "--max-cache", "2GiB", "--min-free", "1G"]);
        assert!(set.no_cache);
        assert_eq!(set.max_cache, 2 << 30);
        assert_eq!(set.min_free, 1 << 30);
    }

    #[test]
    fn snapshots_are_read_only_when_asked_newest_64_by_default() {
        assert_eq!(scan_limits(&[]).snapshot_scan(), None);
        assert_eq!(scan_limits(&[]).owner_budget, DEFAULT_OWNER_BUDGET);
        assert_eq!(scan_limits(&["--owner-budget", "0"]).owner_budget, 0);
        let default = scan_limits(&["--snapshots"]).snapshot_scan().unwrap();
        assert_eq!(default.depth, Depth::Newest(64));
        assert_eq!(default.owner_budget, 64);
        assert_eq!(default.global_owner_budget, DEFAULT_GLOBAL_OWNER_BUDGET);
        let all = scan_limits(&[
            "--snapshots",
            "all",
            "--owner-budget",
            "8",
            "--owner-budget-global",
            "42",
            "--held-files-budget",
            "7",
        ]);
        assert_eq!(
            all.snapshot_scan(),
            Some(SnapshotScan {
                depth: Depth::All,
                owner_budget: 8,
                global_owner_budget: 42,
                held_files_budget: 7,
            })
        );
        assert_eq!(default.held_files_budget, DEFAULT_HELD_FILES_BUDGET);
        assert_eq!(
            scan_limits(&["--snapshots=5"]).snapshots,
            Some(Depth::Newest(5))
        );
        let parsed = Cli::try_parse_from(["synoreclaim", "scan", "/v", "--snapshots", "0"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn a_scan_detaches_or_runs_a_queued_job() {
        let launch = |args: &[&str]| match parse(&[&["synoreclaim", "scan"], args].concat()) {
            Command::Scan { launch, .. } => launch,
            other => panic!("{other:?}"),
        };
        assert_eq!(launch(&["/v"]), LaunchArgs::default());
        assert_eq!(launch(&["/v", "--detach"]).detach, Some(Launcher::Process));
        assert_eq!(
            launch(&["/v", "--detach=none"]).detach,
            Some(Launcher::External)
        );
        assert_eq!(launch(&["--job-id", "j1"]).job_id.as_deref(), Some("j1"));
        assert_eq!(
            launch(&["/v", "--requested-by=admin"])
                .requested_by
                .as_deref(),
            Some("admin")
        );
        for bad in [
            &["--job-id", "j1", "/v"][..],
            &["--job-id", "j1", "--requested-by=admin"],
            &["--job-id", "j1", "--profile", "fast"],
            &["--job-id", "j1", "--detach"],
            &["--job-id", "j1", "--scope", "volume"],
        ] {
            let args = [&["synoreclaim", "scan"], bad].concat();
            assert!(Cli::try_parse_from(args).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_scan_takes_the_fast_profile() {
        assert_eq!(scan_limits(&["--profile", "fast"]).profile, Profile::Fast);
    }

    #[test]
    fn the_pinned_read_budget_defaults_and_is_configurable() {
        assert_eq!(scan_limits(&[]).pinned_read_budget, DEFAULT_READ_BUDGET);
        assert_eq!(
            scan_limits(&["--pinned-read-budget", "0"]).pinned_read_budget,
            0
        );
    }

    #[test]
    fn snapshots_take_targets_and_an_optional_selection() {
        let cmd = parse(&[
            "synoreclaim",
            "snapshots",
            "/volume1/backup",
            "--simulate-delete",
            "older-than:90d,id:1040",
        ]);
        let Command::Snapshots {
            targets,
            simulate_delete,
            max_age,
            limit,
            ..
        } = cmd
        else {
            panic!("not snapshots: {cmd:?}");
        };
        assert_eq!(targets.operands, [OsString::from("/volume1/backup")]);
        assert_eq!(simulate_delete.unwrap().selectors.len(), 2);
        assert_eq!(max_age, Duration::from_secs(86_400));
        assert_eq!(limit, 10);
        let bad = Cli::try_parse_from(["synoreclaim", "snapshots", "/v", "--simulate-delete", "x"]);
        assert!(bad.is_err());
        for limit in ["0", "101"] {
            let bad = Cli::try_parse_from(["synoreclaim", "snapshots", "/v", "--limit", limit]);
            assert!(bad.is_err(), "--limit {limit}");
        }
    }

    #[test]
    fn why_not_freed_takes_one_volume_and_opt_in_probes() {
        let cmd = parse(&["synoreclaim", "why-not-freed", "/volume1"]);
        let Command::WhyNotFreed {
            target,
            since,
            processes,
            max_age,
            limit,
            ..
        } = cmd
        else {
            panic!("not why-not-freed: {cmd:?}");
        };
        assert_eq!(target, Path::new("/volume1"));
        assert_eq!((since, processes), (None, false));
        assert_eq!(limit, 10);
        assert_eq!(max_age, Duration::from_secs(86_400));

        let cmd = parse(&[
            "synoreclaim",
            "why-not-freed",
            "/volume1",
            "--since",
            "24h",
            "--processes",
        ]);
        let Command::WhyNotFreed {
            since, processes, ..
        } = cmd
        else {
            panic!("not why-not-freed: {cmd:?}");
        };
        assert_eq!(since, Some(Duration::from_secs(86_400)));
        assert!(processes);
        assert!(Cli::try_parse_from(["synoreclaim", "why-not-freed", ""]).is_err());
    }

    #[test]
    fn why_not_freed_leaves_watching_to_watch_free() {
        let watch = ["synoreclaim", "why-not-freed", "/volume1", "--watch"];
        assert!(Cli::try_parse_from(watch).is_err());
    }

    #[test]
    fn recycle_takes_one_target_and_analysis_options() {
        let parsed = Cli::try_parse_from([
            "synoreclaim",
            "recycle",
            "/volume1",
            "--oldest",
            "10",
            "--no-scan",
            "--json",
        ]);
        assert!(parsed.is_ok(), "recycle CLI was rejected: {parsed:?}");
        let Command::Recycle { oldest, .. } = parsed.unwrap().command else {
            panic!("not recycle command");
        };
        assert_eq!(oldest, Some(10));
        let empty = Cli::try_parse_from(["synoreclaim", "recycle", ""]);
        assert!(empty.is_err(), "an empty recycle target was accepted");
    }

    #[test]
    fn resume_detaches_with_at_most_a_new_profile() {
        let detach = |args: &[&str]| match parse(&[&["synoreclaim", "resume", "j"], args].concat())
        {
            Command::Resume { detach, .. } => detach,
            other => panic!("{other:?}"),
        };
        assert_eq!(detach(&[]), None);
        assert_eq!(detach(&["--detach"]), Some(Launcher::Process));
        assert_eq!(
            detach(&["--detach=none", "--profile", "fast"]),
            Some(Launcher::External)
        );
        for bad in [
            &["--detach", "--max-memory", "1GiB"][..],
            &["--detach", "--max-runtime", "1h"],
            &["--detach", "--min-free", "1GiB"],
        ] {
            let args = [&["synoreclaim", "resume", "j"], bad].concat();
            assert!(Cli::try_parse_from(args).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn resume_overrides_only_the_limits_given() {
        let cmd = parse(&[
            "synoreclaim",
            "resume",
            "j",
            "--max-memory",
            "512MiB",
            "--min-free",
            "3GiB",
            "--json",
        ]);
        let Command::Resume {
            job_id,
            limits,
            output,
            ..
        } = cmd
        else {
            panic!("not resume: {cmd:?}");
        };
        assert_eq!(job_id, "j");
        assert!(output.json);
        assert_eq!(limits.min_free, Some(3 << 30));
        let original = ScanLimits {
            profile: Profile::Normal,
            max_memory: MemoryLimit::Auto,
            max_runtime: Some(Duration::from_secs(60)),
            no_cache: true,
            max_cache: 1,
            min_free: 2,
            snapshots: Some(Depth::All),
            snapshot_report: None,
            attribution_report: None,
            owner_budget: 3,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            held_files_budget: DEFAULT_HELD_FILES_BUDGET,
            pinned_read_budget: DEFAULT_READ_BUDGET,
        };
        let resumed = limits.apply(original);
        assert_eq!(resumed.max_memory, MemoryLimit::Bytes(512 << 20));
        assert_eq!(resumed.min_free, 3 << 30);
        assert_eq!(
            ScanLimits {
                max_memory: MemoryLimit::Auto,
                min_free: 2,
                ..resumed
            },
            original
        );
        let Command::Resume { limits, .. } = parse(&["synoreclaim", "resume", "j"]) else {
            panic!("not resume");
        };
        assert_eq!(limits.apply(original), original);
    }

    #[test]
    fn parses_pause_and_cancel_of_a_job() {
        let cmd = parse(&["synoreclaim", "pause", "20260928T010203Z-1f"]);
        let Command::Pause { job_id, state, .. } = cmd else {
            panic!("not pause: {cmd:?}");
        };
        assert_eq!(job_id, "20260928T010203Z-1f");
        assert_eq!(state.state_dir, PathBuf::from(DEFAULT_STATE_DIR));
        let cmd = parse(&["synoreclaim", "cancel", "j", "--json", "--state-dir", "/s"]);
        assert!(matches!(cmd, Command::Cancel { job_id, json: true, .. } if job_id == "j"));
    }

    #[test]
    fn parses_a_mode_change_of_a_job() {
        let cmd = parse(&["synoreclaim", "mode", "j", "--profile", "fast", "--json"]);
        assert!(matches!(
            cmd,
            Command::Mode { job_id, profile: Profile::Fast, json: true, .. } if job_id == "j"
        ));
        assert!(Cli::try_parse_from(["synoreclaim", "mode", "j"]).is_err());
    }

    #[test]
    fn parses_jobs() {
        let cmd = parse(&["synoreclaim", "jobs", "--json", "--state-dir", "/s"]);
        let Command::Jobs {
            job_id,
            json,
            result,
            state,
        } = cmd
        else {
            panic!("not jobs: {cmd:?}");
        };
        assert!(json);
        assert_eq!(job_id, None);
        assert_eq!(result, None);
        assert_eq!(state.state_dir, PathBuf::from("/s"));
        let cmd = parse(&["synoreclaim", "jobs", "j1", "--json"]);
        let Command::Jobs { job_id, .. } = cmd else {
            panic!("not jobs: {cmd:?}");
        };
        assert_eq!(job_id.as_deref(), Some("j1"));
        let cmd = parse(&["synoreclaim", "jobs", "--result", "j1"]);
        assert!(matches!(cmd, Command::Jobs { result: Some(id), .. } if id == "j1"));
        assert!(Cli::try_parse_from(["synoreclaim", "jobs", "j1", "--result", "j2"]).is_err());
    }

    #[test]
    fn parses_config() {
        let cmd = parse(&["synoreclaim", "config", "init", "--json"]);
        assert!(matches!(
            cmd,
            Command::Config {
                action: ConfigAction::Init { json: true }
            }
        ));
        let cmd = parse(&[
            "synoreclaim",
            "config",
            "set",
            "--mutations-disabled",
            "true",
        ]);
        assert!(matches!(
            cmd,
            Command::Config {
                action: ConfigAction::Set {
                    mutations_disabled: true,
                    json: false
                }
            }
        ));
        let cmd = parse(&[
            "synoreclaim",
            "config",
            "set",
            "--mutations-disabled",
            "off",
        ]);
        assert!(matches!(
            cmd,
            Command::Config {
                action: ConfigAction::Set {
                    mutations_disabled: false,
                    ..
                }
            }
        ));
        let cmd = parse(&["synoreclaim", "config", "show"]);
        assert!(matches!(
            cmd,
            Command::Config {
                action: ConfigAction::Show { json: false }
            }
        ));
        assert!(Cli::try_parse_from(["synoreclaim", "config", "set"]).is_err());
    }

    #[test]
    fn parses_history() {
        let cmd = parse(&["synoreclaim", "history", "--json", "--state-dir", "/s"]);
        let Command::History { json, state } = cmd else {
            panic!("not history: {cmd:?}");
        };
        assert!(json);
        assert_eq!(state.state_dir, PathBuf::from("/s"));
    }

    #[test]
    fn parses_cache_status_and_clear() {
        let cmd = parse(&["synoreclaim", "cache", "status", "/volume1", "--json"]);
        let Command::Cache {
            action: CacheAction::Status(args),
        } = cmd
        else {
            panic!("not cache status: {cmd:?}");
        };
        assert_eq!(args.targets.operands, [OsString::from("/volume1")]);
        assert!(args.output.json);
        assert_eq!(args.state.state_dir, PathBuf::from(DEFAULT_STATE_DIR));
        let cmd = parse(&["synoreclaim", "cache", "clear", "system"]);
        assert!(matches!(
            cmd,
            Command::Cache {
                action: CacheAction::Clear(_)
            }
        ));
    }

    #[test]
    fn top_defaults_to_reclaimable_children_and_a_day_of_freshness() {
        let Command::Top {
            by,
            kind,
            limit,
            include_system,
            answer,
            ..
        } = parse(&["synoreclaim", "top", "/volume1"])
        else {
            panic!("not top");
        };
        assert_eq!(
            (by, kind, limit, include_system),
            (RankBy::Reclaimable, KindFilter::All, 20, false)
        );
        assert_eq!(answer.max_age, Duration::from_secs(86_400));
        assert!(!answer.refresh && !answer.no_scan);
    }

    #[test]
    fn parses_overview_with_a_volume_target() {
        let Command::Overview {
            targets,
            answer,
            output,
            ..
        } = parse(&["synoreclaim", "overview", "/volume1", "--json"])
        else {
            panic!("expected overview command");
        };

        assert_eq!(targets.operands, ["/volume1"]);
        assert_eq!(answer.max_age, Duration::from_secs(86_400));
        assert!(output.json);
    }

    #[test]
    fn watch_free_is_a_command() {
        let Command::WatchFree {
            target,
            duration,
            interval,
            json,
            ..
        } = parse(&["synoreclaim", "watch-free", "/volume1"])
        else {
            panic!("expected watch-free");
        };
        assert_eq!(target, PathBuf::from("/volume1"));
        assert_eq!(duration, Duration::from_secs(600));
        assert_eq!(interval, Duration::from_secs(10));
        assert!(!json);

        let Command::WatchFree {
            duration,
            interval,
            json,
            ..
        } = parse(&[
            "synoreclaim",
            "watch-free",
            "/volume1",
            "--duration",
            "30s",
            "--interval",
            "5s",
            "--json",
        ])
        else {
            panic!("expected watch-free");
        };
        assert_eq!(duration, Duration::from_secs(30));
        assert_eq!(interval, Duration::from_secs(5));
        assert!(json);

        for args in [
            &["synoreclaim", "watch-free", "/volume1", "--duration", "1s"][..],
            &["synoreclaim", "watch-free", "/volume1", "--duration", "2h"][..],
            &["synoreclaim", "watch-free", "/volume1", "--interval", "1s"][..],
            &["synoreclaim", "watch-free", ""][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_simulate_delete_with_a_path_list() {
        let Command::SimulateDelete {
            targets, explain, ..
        } = parse(&[
            "synoreclaim",
            "simulate-delete",
            "/v/a",
            "/v/b",
            "--from-file",
            "paths.txt",
            "--explain",
        ])
        else {
            panic!("not simulate-delete");
        };
        assert_eq!(targets.operands.len(), 2);
        assert_eq!(targets.from_file, Some(PathBuf::from("paths.txt")));
        assert!(explain);
    }

    #[test]
    fn parses_cleanup_plan_and_apply() {
        assert!(matches!(
            Cli::try_parse_from(["synoreclaim", "cleanup", "plan", "/v/a", "--output", "p.json"])
                .unwrap()
                .command,
            Command::Cleanup {
                command: CleanupCommand::Plan { output: Some(path), .. }
            } if path == std::path::Path::new("p.json")
        ));
        assert!(matches!(
            Cli::try_parse_from(["synoreclaim", "cleanup", "apply", "p.json", "--yes"])
                .unwrap()
                .command,
            Command::Cleanup {
                command: CleanupCommand::Apply { yes: true, .. }
            }
        ));
        for removed in [
            &[
                "synoreclaim",
                "cleanup",
                "apply",
                "p.json",
                "--yes",
                "--measure",
            ][..],
            &["synoreclaim", "cleanup", "recover", "cleanup-1-2"],
            &[
                "synoreclaim",
                "__cleanup-execute",
                "m",
                "p",
                "l",
                "--parent=1",
            ],
        ] {
            assert!(Cli::try_parse_from(removed).is_err(), "{removed:?}");
        }
    }

    #[test]
    fn a_webapi_apply_names_its_requester() {
        let requested = |args: &[&str]| match parse(
            &[&["synoreclaim", "cleanup", "apply", "p.json"], args].concat(),
        ) {
            Command::Cleanup {
                command: CleanupCommand::Apply { requested_by, .. },
            } => requested_by,
            other => panic!("{other:?}"),
        };
        assert_eq!(requested(&[]), None);
        assert_eq!(
            requested(&["--requested-by=admin"]).as_deref(),
            Some("admin")
        );
    }

    #[test]
    fn only_a_confirmed_apply_detaches() {
        let detach = |args: &[&str]| {
            let args = [&["synoreclaim", "cleanup", "apply", "p.json"], args].concat();
            match Cli::try_parse_from(args).map(|cli| cli.command) {
                Ok(Command::Cleanup {
                    command: CleanupCommand::Apply { detach, .. },
                }) => Ok(detach),
                Ok(other) => panic!("{other:?}"),
                Err(e) => Err(e.kind()),
            }
        };
        assert_eq!(detach(&["--yes"]), Ok(None));
        assert_eq!(detach(&["--yes", "--detach"]), Ok(Some(Launcher::Process)));
        assert_eq!(
            detach(&["--yes", "--detach=none"]),
            Ok(Some(Launcher::External))
        );
        assert!(detach(&["--detach"]).is_err());
        assert!(detach(&["--yes", "--detach", "--measure"]).is_err());
        assert!(detach(&["--yes", "--detach", "--job-id=cleanup-1f-2a"]).is_err());
    }

    #[test]
    fn a_confirmed_apply_runs_a_queued_job() {
        let apply = ["synoreclaim", "cleanup", "apply", "p.json"];
        let queued = [&apply[..], &["--yes", "--job-id=cleanup-1f-2a"]].concat();
        match parse(&queued) {
            Command::Cleanup {
                command: CleanupCommand::Apply { job_id, .. },
            } => assert_eq!(job_id.as_deref(), Some("cleanup-1f-2a")),
            other => panic!("{other:?}"),
        }
        let unconfirmed = [&apply[..], &["--job-id=cleanup-1f-2a"]].concat();
        assert!(Cli::try_parse_from(unconfirmed).is_err());
    }

    #[test]
    fn empty_path_operands_are_usage_errors() {
        for args in [
            &["synoreclaim", "cleanup", "delete", "", "--yes"][..],
            &["synoreclaim", "cleanup", "plan", "/v/a", ""],
            &["synoreclaim", "cleanup", "apply", ""],
            &["synoreclaim", "scan", ""],
        ] {
            let error = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn shared_ranks_by_shared_bytes_by_default() {
        let Command::Shared { by, limit, .. } = parse(&["synoreclaim", "shared", "/volume1"])
        else {
            panic!("not shared");
        };
        assert_eq!((by, limit), (ShareBy::Shared, 20));
        let Command::Shared { by, .. } = parse(&["synoreclaim", "shared", "/v", "--by", "ratio"])
        else {
            panic!("not shared");
        };
        assert_eq!(by, ShareBy::Ratio);
    }

    #[test]
    fn refresh_and_no_scan_conflict() {
        assert!(
            Cli::try_parse_from(["synoreclaim", "inspect", "/v/a", "--refresh", "--no-scan"])
                .is_err()
        );
    }

    #[test]
    fn null_requires_a_path_list() {
        assert!(Cli::try_parse_from(["synoreclaim", "scan", "--null"]).is_err());
        assert!(Cli::try_parse_from(["synoreclaim", "scan", "--from-file", "-", "--null"]).is_ok());
    }

    #[test]
    fn rejects_unknown_profiles() {
        assert!(
            Cli::try_parse_from(["synoreclaim", "doctor", "/v", "--profile", "turbo"]).is_err()
        );
    }
}
