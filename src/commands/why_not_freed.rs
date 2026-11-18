//! `why-not-freed` (SPEC 5.15): the newest volume report with overview
//! data plus cheap live probes. It never scans, syncs, or frees anything.

use super::{
    Answerer, Failure, discover_slowly, emit, newest_overview_report, stale_warning,
    unresolved_warning,
};
use crate::btrfs::live::{LiveSource, OpenTarget};
use crate::cli::{AnswerArgs, OutputArgs, ScanLimits};
use crate::document::{
    Completion, Envelope, OverviewFilesystem, ReasonEntry, WhyNotFreedDocument, WhyNotFreedResult,
    render_why_not_freed,
};
use crate::output::{JsonPath, Notice};
use crate::resolve::{self, Environment};
use crate::sys;
use crate::target::{Resolved, Scope};
use crate::why_not_freed::{self, Probes};
use std::collections::BTreeSet;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// What to ask besides the report rows.
#[derive(Debug, Clone, Copy)]
pub struct WhyArgs {
    pub since: Option<Duration>,
    pub processes: bool,
    pub max_age: Duration,
    /// Held files named per share.
    pub limit: usize,
}

pub fn why_not_freed(
    path: PathBuf,
    args: WhyArgs,
    output: OutputArgs,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let (doc, _) = why_not_freed_document(&path, args, output, state_dir)?;
    emit(output.json, &doc, || render_why_not_freed(&doc))?;
    Ok(doc.envelope.completion.exit_code(sys::stop_signal()))
}

/// The rows for the volume of `path`, without the watch, and the volume
/// when it resolved.
pub fn why_not_freed_document(
    path: &Path,
    args: WhyArgs,
    output: OutputArgs,
    state_dir: &Path,
) -> Result<(WhyNotFreedDocument, Option<Resolved>), Failure> {
    let env = Environment::load()?;
    let now = SystemTime::now();
    let mut envelope = Envelope::new("why-not-freed", now);
    let target = match resolve::resolve_path(&env, path, Some(Scope::Volume)) {
        Ok(target) => target,
        Err(error) => {
            envelope.errors.push(error);
            envelope.completion = Completion::Failed;
            let doc = WhyNotFreedDocument {
                envelope,
                results: Vec::new(),
            };
            return Ok((doc, None));
        }
    };
    let mut answerer = Answerer {
        env: &env,
        limits: ScanLimits::report_only(),
        state_dir,
        answer: AnswerArgs {
            refresh: false,
            no_scan: true,
            max_age: args.max_age,
        },
        output,
        scans: Vec::new(),
        errors: Vec::new(),
        hints: Vec::new(),
        job_id: None,
        report: None,
    };
    if !target.filesystem.capabilities().reflinks {
        envelope.errors.push(crate::output::TargetError::new(
            crate::output::JsonPath::new(path.as_os_str().as_encoded_bytes()),
            "unsupported_capability",
            "Btrfs snapshot, reflink and cleaner diagnostics are not available on ext4",
        ));
        envelope.completion = Completion::Failed;
        return Ok((
            WhyNotFreedDocument {
                envelope,
                results: Vec::new(),
            },
            Some(target),
        ));
    }
    let mut results = Vec::new();
    let mut since_not_measured = false;
    match newest_overview_report(&answerer, &target)? {
        None => answerer.refuse(
            &target,
            "no complete volume report contains overview data; run `synoreclaim scan`",
        )?,
        Some(report_path) => {
            let header = crate::report_summary::ensure(&report_path)?.header;
            let data = header
                .overview
                .as_ref()
                .expect("an overview report has overview data");
            let info = super::answered_report(&report_path, &header, now, args.max_age);
            stale_warning(&mut envelope, &info);
            unresolved_warning(&mut envelope, &header);
            let live = live_probes(&env, &target, args.processes);
            let mount = JsonPath::new(&header.mount);
            let reasons =
                why_not_freed::reasons(&header, data, live.probes, args.since, args.limit);
            since_not_measured |= reasons
                .iter()
                .any(why_not_freed::Reason::since_not_measured);
            results.push(WhyNotFreedResult {
                report: info,
                filesystem: OverviewFilesystem {
                    kind: header.filesystem,
                    capabilities: header.capabilities.clone(),
                    uuid: header.fsid.clone(),
                    mount: mount.clone(),
                },
                free_bytes: live.free_bytes,
                reasons: reasons
                    .into_iter()
                    .map(|r| ReasonEntry::new(r, &mount.path))
                    .collect(),
            });
            envelope.warnings.extend(live.warnings);
        }
    }
    if args.since.is_some() && since_not_measured {
        envelope.warnings.push(Notice::new(
            "since_not_measured",
            "--since could not filter pending deletions or some Recycle Bin bytes because their times were not recorded",
        ));
    }
    answerer.close(&mut envelope, !results.is_empty());
    if envelope.completion == Completion::Complete
        && results.iter().any(|result| {
            result.reasons.iter().any(|entry| {
                entry.unknown
                    && !(result.report.coverage.approximation.is_some()
                        && matches!(entry.reason, why_not_freed::Reason::UnresolvedReferences(_)))
            })
        })
    {
        envelope.completion = Completion::Partial;
    }
    let doc = WhyNotFreedDocument { envelope, results };
    Ok((doc, Some(target)))
}

struct Live {
    probes: Probes,
    free_bytes: Option<u64>,
    warnings: Vec<Notice>,
}

/// Pending roots, free space, the commit interval and, when asked, the
/// processes holding deleted files of the volume.
fn live_probes(env: &Environment, target: &Resolved, processes: bool) -> Live {
    let mut warnings = Vec::new();
    let (pending, free_bytes) = match OpenTarget::open(&target.mount) {
        Ok(opened) => {
            let free = sys::filesystem_space(opened.file.as_fd())
                .ok()
                .map(|space| space.free);
            let mut source = LiveSource::new(opened.file, 4096);
            let pending = match pending_roots(&mut source) {
                Ok(ids) => Some(ids),
                Err(e) => {
                    warnings.push(Notice::new("pending_probe_failed", e));
                    None
                }
            };
            (pending, free)
        }
        Err(e) => {
            warnings.push(Notice::new("open_failed", e.to_string()));
            (None, None)
        }
    };
    let options = env
        .mounts
        .iter()
        .rev()
        .find(|m| m.mount_point == target.mount)
        .map_or(&[][..], |m| m.options.as_slice());
    let fsid = target.fsid;
    let processes = processes.then(|| {
        let (found, unreadable) = crate::open_files::deleted_files(Path::new("/proc"), |file| {
            sys::fs_info(file.as_fd()).is_ok_and(|info| info.fsid == fsid)
        });
        why_not_freed::process_scan(&found, unreadable)
    });
    Live {
        probes: Probes {
            pending,
            processes,
            commit_interval_s: why_not_freed::commit_interval(options),
        },
        free_bytes,
        warnings,
    }
}

/// Subvolumes awaiting the Btrfs cleaner now.
fn pending_roots(source: &mut LiveSource) -> Result<BTreeSet<u64>, String> {
    let set = discover_slowly(source)?;
    Ok(set
        .subvolumes
        .iter()
        .filter(|s| s.pending_deletion)
        .map(|s| s.id)
        .collect())
}
