//! Refines retained extent attribution without walking the live namespace.
use super::*;
use crate::btrfs::source::MetadataSource;
use crate::scan::attribution;
use std::io::{self, BufReader};

pub(super) fn run(
    ctx: &ScanContext,
    group: &FilesystemTargets,
    fs: &mut FilesystemScan,
    source: &mut LiveSource,
    control: &mut Control<'_>,
    id: &str,
) -> Result<(), WalkError> {
    if group.targets.len() != 1 || group.targets[0].scope != Scope::Volume {
        return Err(WalkError::Checkpoint(
            "deep attribution needs one whole Btrfs volume".into(),
        ));
    }
    let reports = ctx
        .store
        .reports(&fs.fsid)
        .map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    let (path, mut header) = reports
        .reports
        .into_iter()
        .find(|(_, h)| {
            h.job_id == id
                && h.mount == group.mount.as_os_str().as_bytes()
                && h.roots.len() == 1
                && h.roots[0].scope == Scope::Volume
        })
        .ok_or_else(|| {
            WalkError::Checkpoint("the saved full-volume report is no longer available".into())
        })?;
    report::ReportReader::open(BufReader::new(File::open(&path)?))
        .and_then(|r| r.checked_header())
        .map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    // This phase has different work units from the namespace scan.
    control.previous = None;
    control.phase("resolve-refs");
    let now = Instant::now();
    let mut set = crate::btrfs::subvolume::discover(source, control)?;
    set.relocating = crate::btrfs::subvolume::relocation_trees(source, control)?;
    let queue = ctx.store.root().join("attribution").join(&fs.fsid).join(id);
    let manifest = attribution::open(&queue, &set)?;
    let checkpoint = ctx
        .store
        .root()
        .join("jobs")
        .join(&ctx.job_id)
        .join("checkpoint")
        .join("attribution");
    state::private_dir(checkpoint.parent().expect("checkpoint parent"), ctx.uid)
        .map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    state::private_dir(&checkpoint, ctx.uid).map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    let reader = source
        .block_reader(control)?
        .map_err(WalkError::Checkpoint)?;
    let (mut overview, measurement) = attribution::process(
        source,
        reader,
        &manifest,
        &queue,
        &checkpoint,
        ctx.limits.pinned_read_budget,
        control,
    )?;
    let mut after = crate::btrfs::subvolume::discover(source, control)?;
    after.relocating = crate::btrfs::subvolume::relocation_trees(source, control)?;
    if attribution::identity(&set) != attribution::identity(&after) {
        return Err(WalkError::Checkpoint(
            "attribution roots changed during analysis; run a fresh volume scan".into(),
        ));
    }
    overview.deep_analysis = Some(measurement);
    // Overview evidence is display-only. Keep namespace totals, times and all
    // cleanup certification facts unchanged; attribution reads cannot certify
    // the old namespace as current or authorize deletion.
    if let Some(approximation) = &mut header.coverage.approximation {
        approximation.unaccounted_bytes = overview
            .unknown
            .values()
            .copied()
            .sum::<u64>()
            .max(header.totals.unknown);
    }
    header.overview = Some(overview);
    // Keep the original job id and scan times: freshness and cleanup marks
    // describe the original namespace, not this extra metadata-only read.
    let reader = report::ReportReader::open(BufReader::new(File::open(&path)?))
        .map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    let count = reader.node_count();
    control.phase(jobs::PUBLISH);
    let (file, bytes) = ctx
        .store
        .publish(&header, |out| {
            report::write_records(out, &header, count, |out| {
                let mut failure = None;
                reader
                    .for_each(|_, record| {
                        if let Err(e) = report::encode_record(out, record) {
                            failure = Some(e);
                            return ControlFlow::Break(());
                        }
                        ControlFlow::Continue(())
                    })
                    .map_err(io::Error::other)?;
                failure.map_or(Ok(()), Err)
            })
        })
        .map_err(|e| WalkError::Spill(io::Error::other(e)))?;
    fs.report = Some(PublishedReport {
        file: JsonPath::new(file.as_os_str().as_bytes()),
        bytes,
    });
    fs.consistency = Some(header.consistency);
    fs.change_reasons = header.change_reasons.clone();
    fs.coverage = Some(FilesystemCoverage {
        inner: header.coverage.clone(),
        filesystem: fs.filesystem,
    });
    let mut stats = measured(
        ScanStats {
            pinned_read_plan: header
                .overview
                .as_ref()
                .and_then(|overview| overview.deep_analysis.as_ref())
                .and_then(|analysis| analysis.plan.clone()),
            ..ScanStats::default()
        },
        now,
    );
    stats.elapsed_ms += control.elapsed_before.saturating_mul(1000);
    fs.stats = Some(stats);
    fs.scan_started = Some(rfc3339(
        SystemTime::UNIX_EPOCH + Duration::from_secs(header.scan_started),
    ));
    fs.scan_finished = Some(rfc3339(
        SystemTime::UNIX_EPOCH + Duration::from_secs(header.scan_finished),
    ));
    fs.completion = Completion::Complete;
    Ok(())
}
