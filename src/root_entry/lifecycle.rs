//! The package stop and uninstall purge steps. The executor runs them as
//! root with no session, after it ended this package's running jobs.

use crate::jobs::{self, JobKind, JobState, Request};
use std::path::Path;

/// The stop step: queued work the executor never started stays for the
/// next start as the old package stop left it. A queued scan pauses and
/// a queued apply is cancelled; pending launches go. Every job is tried;
/// the answer names those that could not be settled.
///
/// It also removes `privilege`, the setuid helper and grant builds before
/// the SDK executor left in the package `var`.
pub fn stop(state: &Path, privilege: &Path, uid: u32, now: u64) -> Vec<String> {
    let jobs = state.join("jobs");
    let mut unsettled = Vec::new();
    match jobs::list(&jobs) {
        Ok(found) => {
            for job in found.iter().filter(|job| job.state == JobState::Queued) {
                if let Err(e) = settle(&jobs, uid, &job.job_id, job.kind, now) {
                    unsettled.push(format!("{}: {e}", job.job_id));
                }
            }
        }
        Err(e) => unsettled.push(format!("{}: {e}", jobs.display())),
    }
    if let Err(e) = super::launch::clear(state) {
        unsettled.push(format!("launches: {e}"));
    }
    if let Err(e) = crate::uninstall::remove(privilege) {
        unsettled.push(format!("{}: {e}", privilege.display()));
    }
    unsettled
}

fn settle(jobs: &Path, uid: u32, id: &str, kind: JobKind, now: u64) -> Result<(), jobs::JobError> {
    if kind == JobKind::Scan {
        jobs::Live::launch(jobs, uid, id, now)?.finish(
            JobState::Paused,
            Some("package_stopped"),
            now,
        )?;
    } else {
        jobs::request(jobs, uid, id, Request::Cancel, now)?;
    }
    Ok(())
}

/// The purge step: with `delete_state`, the rootfs plans root wrote
/// outside the root state, which the executor removes itself.
pub fn purge(delete_state: bool, plans: &Path) -> Vec<String> {
    if !delete_state {
        return Vec::new();
    }
    match crate::uninstall::remove(plans) {
        Ok(_) => Vec::new(),
        Err(e) => vec![format!("{}: {e}", plans.display())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ScanLimits;
    use crate::jobs::{Job, JobFilesystem};
    use crate::resource::Profile;
    use crate::root_entry::launch::{self, Launch};

    fn state(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("synoreclaim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_stop_pauses_queued_scans_and_cancels_queued_applies() {
        let state = state("stop");
        let jobs = state.join("jobs");
        let uid = crate::sys::effective_uid();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let scan = Job::new(
            "s-1",
            &[b"/volume1".to_vec()],
            None,
            &ScanLimits::report_only(),
            now,
        );
        jobs::queue(&jobs, uid, scan).unwrap();
        let apply = Job::cleanup_apply(
            "c-1",
            &[b"/volume1/old".to_vec()],
            JobFilesystem {
                fsid: "f".repeat(32),
                mount: b"/volume1".to_vec(),
            },
            Profile::Normal,
            now,
        );
        jobs::queue(&jobs, uid, apply).unwrap();
        launch::save(
            &state,
            &Launch {
                job_id: "s-1".into(),
                args: vec!["scan".into()],
            },
        )
        .unwrap();

        assert_eq!(
            stop(&state, &state.join("privilege"), uid, now),
            Vec::<String>::new()
        );

        assert_eq!(jobs::load(&jobs, "s-1").unwrap().state, JobState::Paused);
        assert_eq!(jobs::load(&jobs, "c-1").unwrap().state, JobState::Cancelled);
        assert!(launch::take(&state, "s-1").is_err());
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn a_stop_removes_the_old_privilege_folder() {
        let state = state("stop-privilege");
        let privilege = state.join("var/privilege");
        std::fs::create_dir_all(privilege.join("releases/1550")).unwrap();
        std::fs::write(privilege.join("helper"), b"x").unwrap();
        std::fs::write(privilege.join("grant.json"), b"{}").unwrap();

        assert_eq!(
            stop(&state, &privilege, crate::sys::effective_uid(), 110),
            Vec::<String>::new()
        );

        assert!(std::fs::symlink_metadata(&privilege).is_err());
        assert_eq!(
            stop(&state, &privilege, crate::sys::effective_uid(), 110),
            Vec::<String>::new()
        );
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn a_stop_without_jobs_has_nothing_to_settle() {
        let state = state("stop-empty");
        assert_eq!(
            stop(
                &state,
                &state.join("privilege"),
                crate::sys::effective_uid(),
                110
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_purge_that_deletes_state_takes_the_rootfs_plans() {
        let plans = state("plans").join("rootfs-plans");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::write(plans.join("rootfs-1-2.json"), b"{}").unwrap();
        assert_eq!(purge(false, &plans), Vec::<String>::new());
        assert!(plans.exists());
        assert_eq!(purge(true, &plans), Vec::<String>::new());
        assert!(!plans.exists());
        assert_eq!(purge(true, &plans), Vec::<String>::new());
    }
}
