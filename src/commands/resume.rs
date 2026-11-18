//! Read-only resume admission. No queued state or launcher is written until
//! the durable checkpoint and current filesystem identities pass these checks.
use super::*;

pub(super) fn preflight(store: &Store, job: &Job) -> Result<(), jobs::JobError> {
    let check = || -> Result<(), Failure> {
        let root = store
            .root()
            .join("jobs")
            .join(&job.job_id)
            .join("checkpoint");
        if !root.is_dir() {
            return Err("this job has no durable checkpoint".into());
        }
        let env = Environment::load()?;
        for filesystem in &job.filesystems {
            let volume = env
                .volumes
                .iter()
                .find(|v| {
                    v.primary.mount_point.as_os_str().as_bytes() == filesystem.mount
                        && v.fsid
                            .as_ref()
                            .is_ok_and(|id| fsid_text(id) == filesystem.fsid)
                })
                .ok_or("checkpoint filesystem identity changed or volume is unavailable")?;
            if volume.primary.fstype != "btrfs" || job.scope == Some(Scope::Path) {
                return Err("this filesystem has no saved walk checkpoint".into());
            }
            // These phases have their own report-bound checkpoint validators.
            // They do not consume a saved live namespace.
            if job.limits.attribution_report.is_some() || job.limits.snapshot_report.is_some() {
                continue;
            }
            let keys = Cache::checkpoint_keys(&root, &filesystem.fsid)?;
            if keys.is_empty()
                && job
                    .progress
                    .as_ref()
                    .is_some_and(|p| phase_rank(&p.phase) > 0)
            {
                return Err("the saved scan position is missing".into());
            }
            let mut marks = env.encrypted_marks(
                &volume
                    .fsid
                    .as_ref()
                    .copied()
                    .map_err(|_| "volume identity is unavailable")?,
            );
            if let Ok((id, tree, ino)) = resolve::identity(store.root())
                && fsid_text(&id) == filesystem.fsid
            {
                marks.own_state.insert((tree, ino));
            }
            let opened = OpenTarget::open(&volume.primary.mount_point)?;
            let mut source =
                LiveSource::new(opened.file, job.limits.profile.settings().search_buffer);
            crate::scan::validate_checkpoint_roots(&mut source, &marks, &keys, &mut Boundary)?;
        }
        Ok(())
    };
    check().map_err(|e| jobs::JobError::Checkpoint {
        reason: e.to_string(),
    })
}

struct Boundary;
impl BatchBoundary for Boundary {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        ControlFlow::Continue(())
    }
}
