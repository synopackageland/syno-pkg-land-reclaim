//! The queued job a detach answer describes, kept in the root state until
//! one `root.start` consumes it, so a job runs only the argv its own CLI
//! queued and runs it once.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// A queued job and the argv (without the program) that runs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub job_id: String,
    pub args: Vec<String>,
}

/// A launch that is not one this CLI queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Untrusted;

/// A job ID a launch may name.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The launch of a detach `document` with state in `state`, `None` for any
/// other answer.
pub fn from_document(document: &Value, state: &Path) -> Result<Option<Launch>, Untrusted> {
    if document["command"] != "detach" {
        return Ok(None);
    }
    let run = document["run"].as_array().ok_or(Untrusted)?;
    if run.is_empty() {
        return Ok(None);
    }
    let job_id = document["job_id"]
        .as_str()
        .filter(|id| valid_id(id))
        .ok_or(Untrusted)?;
    let args: Vec<String> = run[1..]
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or(Untrusted))
        .collect::<Result<_, _>>()?;
    if document["launcher"] != "none"
        || args.len() > 64
        || args.iter().map(String::len).sum::<usize>() > 64 * 1024
        || args
            .iter()
            .any(|s| s.contains('\0') || s.starts_with("--detach"))
    {
        return Err(Untrusted);
    }
    let state = state.to_str().ok_or(Untrusted)?;
    // The one flag of its name, with exactly this value.
    let once = |arg: String| {
        let name = arg.split('=').next().unwrap_or_default();
        args.iter()
            .filter(|s| s.split('=').next() == Some(name))
            .count()
            == 1
            && args.contains(&arg)
    };
    let scan = args == ["scan", "--job-id", job_id, "--state-dir", state];
    let apply = args.len() > 4
        && matches!(args[0].as_str(), "cleanup" | "rootfs")
        && args[1] == "apply"
        && once(format!("--job-id={job_id}"))
        && once(format!("--state-dir={state}"))
        && args.iter().any(|s| s == "--yes")
        && args.iter().any(|s| s == "--json");
    let planning = args == ["planning", "--job-id", job_id, "--state-dir", state];
    if !scan && !apply && !planning {
        return Err(Untrusted);
    }
    Ok(Some(Launch {
        job_id: job_id.into(),
        args,
    }))
}

/// Keeps `launch` in `<state>/launch` until [`take`] consumes it.
#[cfg(target_os = "linux")]
pub fn save(state: &Path, launch: &Launch) -> Result<(), Untrusted> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_id(&launch.job_id) {
        return Err(Untrusted);
    }
    let dir = directory(state)?;
    let body = serde_json::to_vec(launch).map_err(|_| Untrusted)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir.join(format!("{}.json", launch.job_id)))
        .map_err(|_| Untrusted)?;
    file.write_all(&body)
        .and_then(|()| file.sync_all())
        .map_err(|_| Untrusted)
}

/// Consumes the launch of job `id` in `state`, so no later start replays it.
#[cfg(target_os = "linux")]
pub fn take(state: &Path, id: &str) -> Result<Launch, Untrusted> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_id(id) {
        return Err(Untrusted);
    }
    let dir = directory(state)?;
    // The rename claims it: of two starts of one job, only one finds it.
    let taken = dir.join(format!("{id}.taken"));
    std::fs::rename(dir.join(format!("{id}.json")), &taken).map_err(|_| Untrusted)?;
    let read = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&taken)
        .and_then(|file| {
            let mut body = Vec::new();
            file.take(64 * 1024).read_to_end(&mut body).map(|_| body)
        });
    let _ = std::fs::remove_file(&taken);
    let launch: Launch =
        serde_json::from_slice(&read.map_err(|_| Untrusted)?).map_err(|_| Untrusted)?;
    if launch.job_id != id {
        return Err(Untrusted);
    }
    Ok(launch)
}

/// `<state>/launch`, private to the root programs.
#[cfg(target_os = "linux")]
/// Drops every pending launch.
#[cfg(target_os = "linux")]
pub fn clear(state: &Path) -> std::io::Result<()> {
    let dir = state.join("launch");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&dir),
        Ok(_) => std::fs::remove_file(&dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn directory(state: &Path) -> Result<std::path::PathBuf, Untrusted> {
    let dir = state.join("launch");
    crate::state::private_dir(&dir, crate::sys::effective_uid()).map_err(|_| Untrusted)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document() -> Value {
        json!({"command":"detach", "launcher":"none", "job_id":"j-123", "run":["/any/cli", "scan", "--job-id", "j-123", "--state-dir", "/trusted/state"]})
    }

    fn parse(doc: &Value) -> Result<Option<Launch>, Untrusted> {
        from_document(doc, Path::new("/trusted/state"))
    }

    #[test]
    fn planning_launch_is_bound_to_its_job_and_state() {
        let mut doc = document();
        doc["run"] = json!([
            "/any/cli",
            "planning",
            "--job-id",
            "j-123",
            "--state-dir",
            "/trusted/state"
        ]);
        assert!(parse(&doc).unwrap().is_some());
        doc["run"][3] = json!("other");
        assert_eq!(parse(&doc), Err(Untrusted));
    }

    #[test]
    fn a_queued_scan_runs_its_own_job() {
        assert_eq!(
            parse(&document()),
            Ok(Some(Launch {
                job_id: "j-123".into(),
                args: ["scan", "--job-id", "j-123", "--state-dir", "/trusted/state"]
                    .map(String::from)
                    .to_vec(),
            }))
        );
    }

    #[test]
    fn a_queued_apply_runs_its_own_job_once_confirmed() {
        let mut doc = document();
        doc["run"] = json!([
            "/any/cli",
            "cleanup",
            "apply",
            "--yes",
            "--json",
            "--max-cache=1",
            "--profile=low",
            "--requested-by=admin",
            "--job-id=j-123",
            "--state-dir=/trusted/state",
            "--",
            "/trusted/state/cleanup/plans/p.json"
        ]);
        assert_eq!(
            parse(&doc).unwrap().unwrap().args[0..2],
            ["cleanup", "apply"]
        );
        doc["run"] = json!([
            "/any/cli",
            "rootfs",
            "apply",
            "--yes",
            "--json",
            "--profile=low",
            "--job-id=j-123",
            "--state-dir=/trusted/state",
            "--",
            "/var/lib/synoreclaim/rootfs-plans/r.json"
        ]);
        assert_eq!(
            parse(&doc).unwrap().unwrap().args[0..2],
            ["rootfs", "apply"]
        );
    }

    #[test]
    fn other_answers_launch_nothing() {
        assert_eq!(parse(&json!({"command":"volumes"})), Ok(None));
        assert_eq!(
            parse(&json!({"command":"detach","launcher":"none","run":[]})),
            Ok(None)
        );
    }

    #[test]
    fn a_launch_must_run_its_own_job_in_the_root_state() {
        for (field, value) in [
            ("job_id", json!("../escape")),
            ("launcher", json!("self")),
            (
                "run",
                json!([
                    "/c",
                    "scan",
                    "--job-id",
                    "other",
                    "--state-dir",
                    "/trusted/state"
                ]),
            ),
            (
                "run",
                json!([
                    "/c",
                    "scan",
                    "--job-id",
                    "j-123",
                    "--state-dir",
                    "/mutable/state"
                ]),
            ),
            (
                "run",
                json!([
                    "/c",
                    "scan",
                    "--job-id",
                    "j-123",
                    "--state-dir",
                    "/trusted/state",
                    "--detach=self"
                ]),
            ),
            (
                "run",
                json!([
                    "/c",
                    "doctor",
                    "--job-id=j-123",
                    "--state-dir=/trusted/state"
                ]),
            ),
            (
                "run",
                json!([
                    "/c",
                    "cleanup",
                    "apply",
                    "--json",
                    "--job-id=j-123",
                    "--state-dir=/trusted/state",
                    "--",
                    "p"
                ]),
            ),
            (
                "run",
                json!([
                    "/c",
                    "cleanup",
                    "apply",
                    "--yes",
                    "--json",
                    "--job-id=j-123",
                    "--job-id=j-123",
                    "--state-dir=/trusted/state",
                    "--",
                    "p"
                ]),
            ),
            (
                "run",
                json!([
                    "/c",
                    "cleanup",
                    "apply",
                    "--yes",
                    "--json",
                    "--job-id=j-123",
                    "--state-dir=/trusted/state",
                    "--state-dir=/x",
                    "--",
                    "p"
                ]),
            ),
            ("run", json!(["/c", "scan", 1])),
        ] {
            let mut doc = document();
            doc[field] = value;
            assert_eq!(parse(&doc), Err(Untrusted), "{field}={}", doc[field]);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_launch_is_kept_once_and_consumed_once() {
        let state = std::env::temp_dir().join(format!("synoreclaim-launch-{}", std::process::id()));
        let launch = parse(&document()).unwrap().unwrap();
        save(&state, &launch).unwrap();
        assert_eq!(save(&state, &launch), Err(Untrusted));
        assert_eq!(take(&state, "j-123"), Ok(launch));
        assert_eq!(take(&state, "j-123"), Err(Untrusted));
        assert_eq!(take(&state, "../j-123"), Err(Untrusted));
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_linked_launch_is_not_followed() {
        let state = std::env::temp_dir().join(format!("synoreclaim-linked-{}", std::process::id()));
        let launch = parse(&document()).unwrap().unwrap();
        save(&state, &launch).unwrap();
        let outside = state.join("outside.json");
        std::fs::rename(state.join("launch/j-123.json"), &outside).unwrap();
        std::os::unix::fs::symlink(&outside, state.join("launch/j-123.json")).unwrap();
        assert_eq!(take(&state, "j-123"), Err(Untrusted));
        assert!(outside.exists());
        std::fs::remove_dir_all(&state).unwrap();
    }
}
