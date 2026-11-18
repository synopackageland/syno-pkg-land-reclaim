//! Additional snapshot census over a retained report. Never walks the live
//! namespace or changes its totals, capture time, or cleanup certification.
use super::*;
use crate::snapshots::census::{Request as CensusRequest, analyze_checkpointed};
use crate::snapshots::{AnalysisMeasurement, Depth as SnapshotDepth};
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
            "snapshot analysis needs one whole Btrfs volume".into(),
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
    // Validate its checksum before spending time on the analysis.
    report::ReportReader::open(BufReader::new(File::open(&path)?))
        .and_then(|r| r.checked_header())
        .map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    control.phase("snapshots");
    let now = Instant::now();
    let started = ctx.job.as_ref().map_or_else(
        || unix_seconds(SystemTime::now()),
        |j| j.borrow().job.created,
    );
    let mut set = crate::btrfs::subvolume::discover(source, control)?;
    set.relocating = crate::btrfs::subvolume::relocation_trees(source, control)?;
    if !set.relocating.is_empty() {
        return Err(WalkError::Checkpoint(
            "snapshot trees are being relocated".into(),
        ));
    }
    let lives: Vec<u64> = header
        .subvolumes
        .iter()
        .filter(|s| s.role.is_materialized())
        .filter_map(|s| set.get(s.id).filter(|s| !s.readonly).map(|s| s.id))
        .collect();
    let scan = ctx
        .limits
        .snapshot_scan()
        .ok_or_else(|| WalkError::Checkpoint("snapshot depth is missing".into()))?;
    let request = CensusRequest {
        lives,
        depth: SnapshotDepth::All,
        owner_budget: scan.owner_budget,
        global_owner_budget: scan.global_owner_budget,
        held_files_budget: scan.held_files_budget,
    };
    let checkpoint = ctx
        .store
        .root()
        .join("jobs")
        .join(&ctx.job_id)
        .join("checkpoint")
        .join("snapshots");
    state::private_dir(checkpoint.parent().expect("checkpoint parent"), ctx.uid)
        .map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    state::private_dir(&checkpoint, ctx.uid).map_err(|e| WalkError::Checkpoint(e.to_string()))?;
    let spill = Spill::create(space::spill_dir(
        &ctx.store.root().join("spill"),
        &ctx.job_id,
        &fs.fsid,
    ))?;
    let mut analysis = analyze_checkpointed(
        source,
        &set,
        &request,
        control,
        spill.dir(),
        Some(&checkpoint),
    )?;
    let after = crate::btrfs::subvolume::discover(source, control)?;
    let mut changed_live = BTreeSet::new();
    for share in &analysis.shares {
        for root in share.materialized().map(|s| s.id) {
            if set.get(root) != after.get(root) {
                return Err(WalkError::Checkpoint(
                    "a read-only snapshot changed; start the analysis again".into(),
                ));
            }
        }
        if set.get(share.live) != after.get(share.live) {
            changed_live.insert(share.live);
        }
    }
    analysis.invalidate_live_comparison(&changed_live);
    analysis.stats.measurement = Some(AnalysisMeasurement {
        changed_live_subvolumes: changed_live.into_iter().collect(),
        started,
        finished: unix_seconds(SystemTime::now()),
        elapsed_ms: now.elapsed().as_millis() as u64 + control.elapsed_before.saturating_mul(1000),
        peak_rss_bytes: resource::peak_resident_bytes(),
    });
    let read: BTreeSet<u64> = analysis
        .shares
        .iter()
        .flat_map(|s| s.materialized().map(|s| s.id))
        .collect();
    header.coverage.snapshots_materialized = read.len() as u64;
    header.coverage.snapshots_not_materialized = header
        .subvolumes
        .iter()
        .filter(|s| s.role == report::SubvolumeRole::SnapshotSkipped && !read.contains(&s.id))
        .count() as u64;
    header.snapshots = Some(analysis);
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
    let mut stats = measured(ScanStats::default(), now);
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
