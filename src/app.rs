//! Command dispatch: turns parsed CLI arguments into work and exit codes.

use crate::cli::{CleanupCommand, Cli, Command, DebugCommand, RootfsCommand};

/// SPEC 22.7: 1 is a fatal/environment failure.
const EXIT_FAILURE: i32 = 1;

type AppResult = Result<(), Box<dyn std::error::Error>>;

pub fn run(cli: Cli) -> i32 {
    match cli.command {
        Command::Doctor {
            target,
            json,
            resources,
            state,
            min_free,
        } => doctor(target, json, resources.profile, &state.state_dir, min_free),
        Command::Debug(command) => exit_code(debug(command)),
        command => analysis(command),
    }
}

#[cfg(target_os = "linux")]
fn analysis(command: Command) -> i32 {
    use crate::commands;
    let result = match command {
        Command::Volumes { json, state } => commands::volumes(json, &state.state_dir),
        Command::History { json, state } => commands::history(json, &state.state_dir),
        Command::WatchFree {
            target,
            duration,
            interval,
            json,
            resources,
        } => crate::watch_free::run(target, duration, interval, json, resources.profile),
        Command::Scan {
            targets,
            output,
            limits,
            launch,
            state,
        } => commands::scan(targets, output, limits, launch, &state.state_dir),
        Command::Top {
            targets,
            by,
            kind,
            limit,
            include_system,
            columns,
            explain,
            report,
            answer,
            output,
            limits,
            state,
        } => commands::top(
            targets,
            commands::TopArgs {
                by,
                kind,
                limit,
                include_system,
                columns,
                explain,
                report,
            },
            answer,
            output,
            limits,
            &state.state_dir,
        ),
        Command::Overview {
            targets,
            answer,
            output,
            limits,
            state,
        } => commands::overview(targets, answer, output, limits, &state.state_dir),
        Command::Inspect {
            targets,
            explain,
            answer,
            output,
            limits,
            state,
        } => commands::inspect(targets, explain, answer, output, limits, &state.state_dir),
        Command::SimulateDelete {
            targets,
            explain,
            answer,
            output,
            limits,
            state,
        } => commands::simulate_delete(targets, explain, answer, output, limits, &state.state_dir),
        Command::Snapshots {
            targets,
            simulate_delete,
            max_age,
            limit,
            output,
            state,
        } => commands::snapshots(
            targets,
            commands::SnapshotsView {
                selection: simulate_delete,
                max_age,
                limit,
            },
            output,
            &state.state_dir,
        ),
        Command::Recycle {
            target,
            oldest,
            report,
            answer,
            output,
            limits,
            state,
        } => commands::recycle(
            target,
            oldest,
            report,
            answer,
            output,
            limits,
            &state.state_dir,
        ),
        Command::WhyNotFreed {
            target,
            since,
            processes,
            max_age,
            limit,
            output,
            state,
        } => commands::why_not_freed(
            target,
            commands::WhyArgs {
                since,
                processes,
                max_age,
                limit,
            },
            output,
            &state.state_dir,
        ),
        Command::Cleanup { command } => cleanup(command),
        Command::Duplicates(command) => duplicates(command),
        Command::Rootfs {
            command: None,
            json,
            resources,
            max_age_s,
            state,
        } => commands::rootfs_report(json, resources.profile, &state.state_dir, max_age_s),
        Command::Rootfs {
            command:
                Some(RootfsCommand::Plan {
                    to,
                    mode,
                    json,
                    resources,
                }),
            ..
        } => commands::rootfs_plan(&to, mode, json, resources.profile),
        Command::Rootfs {
            command:
                Some(RootfsCommand::Apply {
                    plan,
                    yes,
                    json,
                    resources,
                    launch,
                    state,
                }),
            ..
        } => commands::rootfs_apply(
            &plan,
            yes,
            json,
            resources.profile,
            launch.detach,
            launch.job_id.as_deref(),
            launch.requested_by.as_deref(),
            &state.state_dir,
        ),
        Command::Rootfs {
            command: Some(RootfsCommand::History { json, state }),
            ..
        } => commands::rootfs_history(json, &state.state_dir),
        Command::Shared {
            targets,
            by,
            kind,
            limit,
            include_system,
            answer,
            output,
            limits,
            state,
        } => commands::shared(
            targets,
            commands::SharedArgs {
                by,
                kind,
                limit,
                include_system,
            },
            answer,
            output,
            limits,
            &state.state_dir,
        ),
        Command::Cache { action } => commands::cache(action),
        Command::Config { action } => commands::config(action),
        Command::Jobs {
            job_id,
            json,
            result: None,
            state,
        } => commands::jobs(json, &state.state_dir, job_id.as_deref()),
        Command::Jobs {
            json,
            result: Some(id),
            state,
            ..
        } => commands::show_job_result(&id, json, &state.state_dir),
        Command::Resume {
            job_id,
            limits,
            detach,
            output,
            state,
        } => commands::resume(&job_id, limits, detach, output, &state.state_dir),
        Command::Pause {
            job_id,
            json,
            state,
        } => commands::request_job(&job_id, crate::jobs::Request::Pause, json, &state.state_dir),
        Command::Mode {
            job_id,
            profile,
            json,
            state,
        } => commands::change_mode(&job_id, profile, json, &state.state_dir),
        Command::Cancel {
            job_id,
            json,
            state,
        } => commands::request_job(
            &job_id,
            crate::jobs::Request::Cancel,
            json,
            &state.state_dir,
        ),
        Command::SupportBundle {
            target,
            output,
            include_paths,
            max_bytes,
            state,
        } => {
            crate::support_bundle::run(&target, &output, include_paths, max_bytes, &state.state_dir)
        }
        Command::Doctor { .. } | Command::Debug(_) => {
            unreachable!("dispatched by run")
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("synoreclaim: error: {e}");
            crate::guard::failure_exit_code(e.as_ref())
        }
    }
}

#[cfg(target_os = "linux")]
fn cleanup(command: CleanupCommand) -> Result<i32, Box<dyn std::error::Error>> {
    use crate::commands;
    match command {
        CleanupCommand::Select {
            selection,
            chunk,
            json,
            state,
        } => commands::cleanup_select(selection, &chunk, json, &state.state_dir),
        CleanupCommand::Plan {
            targets,
            selection,
            expect,
            duplicates,
            output,
            json,
            quiet: _,
            state,
        } => commands::cleanup_plan(
            (targets, expect, duplicates, selection),
            output,
            json,
            &state.state_dir,
        ),
        CleanupCommand::Apply {
            plan,
            yes,
            confirm_delete,
            json,
            resources,
            requested_by,
            detach,
            job_id,
            state,
        } => commands::cleanup_apply(
            &plan,
            yes,
            json,
            resources.profile,
            detach,
            commands::ApplyOrigin {
                requested_by: requested_by.as_deref(),
                job_id: job_id.as_deref(),
                confirm_delete,
            },
            &state.state_dir,
        ),
        CleanupCommand::Delete {
            targets,
            yes,
            output,
            json,
            quiet,
            resources,
            state,
        } => commands::cleanup_delete(
            targets,
            output,
            yes,
            json,
            quiet,
            resources.profile,
            &state.state_dir,
        ),
    }
}

#[cfg(not(target_os = "linux"))]
fn cleanup(_: CleanupCommand) -> i32 {
    exit_code(Err("cleanup requires Linux".into()))
}

#[cfg(target_os = "linux")]
fn duplicates(command: crate::cli::DuplicatesCommand) -> Result<i32, Box<dyn std::error::Error>> {
    use crate::cli::{DuplicateCategory, DuplicatesCommand as D};
    use crate::duplicates::{command as dup, model::Category};
    match command {
        D::Analyze {
            volume,
            json,
            state,
        } => dup::analyze(&volume, json, &state.state_dir),
        D::Scope {
            volume,
            path,
            list,
            category,
            offset,
            limit,
            json,
            state,
        } => dup::scope(
            dup::ScopeQuery {
                volume: &volume,
                path: path.as_deref(),
                list,
                category: category.map(|c| match c {
                    DuplicateCategory::Photos => Category::Photos,
                    DuplicateCategory::Videos => Category::Videos,
                    DuplicateCategory::Music => Category::Music,
                    DuplicateCategory::Other => Category::Other,
                }),
                offset,
                limit,
            },
            json,
            &state.state_dir,
        ),
        D::Copies {
            volume,
            group,
            path,
            offset,
            limit,
            json,
            state,
        } => dup::copies(
            &volume,
            &group,
            path.as_deref(),
            offset,
            limit,
            json,
            &state.state_dir,
        ),
        D::Keep {
            volume,
            group,
            copy,
            keep,
            path,
            json,
            state,
        } => dup::keep(
            &volume,
            &group,
            &copy,
            keep,
            path.as_deref(),
            json,
            &state.state_dir,
        ),
        D::PreferredShares {
            volume,
            shares,
            json,
            state,
        } => dup::preferred_shares(&volume, shares.as_deref(), json, &state.state_dir),
    }
}

#[cfg(not(target_os = "linux"))]
fn analysis(_: Command) -> i32 {
    exit_code(Err("synoreclaim requires Linux".into()))
}

fn exit_code(result: AppResult) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("synoreclaim: error: {e}");
            EXIT_FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn doctor(
    target: std::path::PathBuf,
    json: bool,
    profile: crate::resource::Profile,
    state_dir: &std::path::Path,
    min_free: u64,
) -> i32 {
    use std::io::Write;
    let report = crate::doctor::run(target, profile, state_dir, min_free);
    let text = if json {
        report.render_json() + "\n"
    } else {
        report.render_human()
    };
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => report.exit_code(),
        Err(e) => exit_code(Err(e.into())),
    }
}

#[cfg(target_os = "linux")]
fn debug(command: DebugCommand) -> AppResult {
    match command {
        DebugCommand::Tree { path, resources } => debug::tree(&path, resources.profile),
        DebugCommand::Record {
            path,
            output,
            resources,
        } => debug::record(&path, &output, resources.profile),
        DebugCommand::Sanitise { input, output } => debug::sanitise(&input, &output),
    }
}

#[cfg(not(target_os = "linux"))]
fn doctor(
    _: std::path::PathBuf,
    _: bool,
    _: crate::resource::Profile,
    _: &std::path::Path,
    _: u64,
) -> i32 {
    exit_code(Err("synoreclaim requires Linux".into()))
}

#[cfg(not(target_os = "linux"))]
fn debug(_: DebugCommand) -> AppResult {
    Err("synoreclaim requires Linux".into())
}

#[cfg(target_os = "linux")]
mod debug {
    use super::AppResult;
    use crate::btrfs::keys::describe_type_counts;
    use crate::btrfs::live::{LiveSource, OpenTarget};
    use crate::btrfs::recording::RecordingSource;
    use crate::btrfs::sanitise::sanitise_recording;
    use crate::btrfs::source::MetadataSource;
    use crate::btrfs::subvolume::{SubvolumeSet, discover};
    use crate::btrfs::walk::{BatchBoundary, WalkError};
    use crate::model::namespace::{Namespace, format_listing};
    use crate::resource::{Pacer, Profile, SystemClock, apply_profile};
    use std::fs::OpenOptions;
    use std::io::{BufWriter, Write};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    use std::time::Instant;

    fn pacer(profile: Profile) -> Pacer<SystemClock> {
        for warning in apply_profile(profile).warnings {
            eprintln!("synoreclaim: warning: {warning}");
        }
        Pacer::new(
            profile.settings().duty_percent,
            SystemClock::default(),
            None,
        )
    }

    pub fn tree(path: &Path, profile: Profile) -> AppResult {
        let target = OpenTarget::open(path)?;
        let tree_id = target.subvolume_id()?;
        let root = target.ino;
        let mut pacer = pacer(profile);
        let mut source = LiveSource::new(target.file, profile.settings().search_buffer);
        let started = Instant::now();
        let (subvolumes, ns) = read_tree(&mut source, tree_id, &mut pacer)?;
        let listing = ns.listing(root, &subvolumes)?;
        std::io::stdout().write_all(&format_listing(&listing))?;
        let stats = ns.stats();
        eprintln!(
            "tree {tree_id}: {} items in {} batches ({} inodes, {} dir entries, {} extents, other item types (skipped): [{}]), {} listed, {:.3}s",
            stats.walk.items,
            stats.walk.batches,
            stats.inode_items,
            stats.dir_index_items,
            stats.extent_data_items,
            describe_type_counts(&stats.unknown_types),
            listing.len(),
            started.elapsed().as_secs_f64()
        );
        Ok(())
    }

    /// Subvolume discovery followed by one subvolume tree walk: the calls
    /// `tree` makes, and the order a recording replays them in.
    fn read_tree<S: MetadataSource>(
        source: &mut S,
        tree_id: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<(SubvolumeSet, Namespace), WalkError> {
        let subvolumes = discover(source, boundary)?;
        let ns = Namespace::read(source, tree_id, boundary)?;
        Ok((subvolumes, ns))
    }

    pub fn sanitise(input: &Path, output: &Path) -> AppResult {
        let (clean, report) = sanitise_recording(&std::fs::read(input)?)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?
            .write_all(&clean)?;
        eprintln!(
            "sanitised {} root items ({} distinct UUIDs), replaced {} link names, kept {} inside recorded trees",
            report.root_items, report.distinct_uuids, report.names_replaced, report.names_kept
        );
        Ok(())
    }

    pub fn record(path: &Path, output: &Path, profile: Profile) -> AppResult {
        let target = OpenTarget::open(path)?;
        let tree_id = target.subvolume_id()?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)?;
        let mut pacer = pacer(profile);
        let live = LiveSource::new(target.file, profile.settings().search_buffer);
        let mut recorder = RecordingSource::new(live, BufWriter::new(file))?;
        let (subvolumes, _) = read_tree(&mut recorder, tree_id, &mut pacer)?;
        recorder.into_writer().flush()?;
        // Replay metadata for golden tests (tests/golden/*.meta).
        let meta = format!(
            "tree_id={tree_id}\nroot_ino={}\nsubvolumes={}\n",
            target.ino,
            subvolumes.subvolumes.len()
        );
        std::io::stdout().write_all(meta.as_bytes())?;
        Ok(())
    }
}
