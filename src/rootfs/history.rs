//! Private per-apply history for `rootfs apply` (SPEC 5.23).

use crate::document::Envelope;
use crate::output::JsonPath;
use crate::rootfs::delegate::{Delegate, ToolOutcome};
use crate::state::{self, StateError};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const ROOTFS_DIR: &str = "rootfs";
const HISTORY_DIR: &str = "history";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    pub files: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub tool: Delegate,
    pub outcome: String,
}

impl Tool {
    pub fn new(tool: Delegate, outcome: ToolOutcome) -> Tool {
        let outcome = match outcome {
            ToolOutcome::Succeeded => "succeeded",
            ToolOutcome::Failed { .. } => "failed",
            ToolOutcome::Killed { .. } => "killed",
            ToolOutcome::TimedOut => "timed_out",
            ToolOutcome::NotStarted => "not_started",
            ToolOutcome::Untrusted => "untrusted",
            ToolOutcome::NotNeeded => "not_needed",
        };
        Tool {
            tool,
            outcome: outcome.into(),
        }
    }
}

/// The durable summary of one apply attempt. The private file name is
/// the attempt ID, so another apply of the same plan gets its own record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Apply {
    #[serde(
        default,
        skip_serializing_if = "crate::rootfs::plan::CleanupMode::is_standard"
    )]
    pub mode: crate::rootfs::plan::CleanupMode,
    pub plan_id: String,
    pub requested_by: Option<String>,
    pub started: String,
    pub finished: String,
    pub move_dir: JsonPath,
    pub manifest: JsonPath,
    pub moved: Moved,
    pub freed_bytes: i64,
    pub completion: String,
    pub skipped_count: u64,
    pub tools: Vec<Tool>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: damaged rootfs history: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn history_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(ROOTFS_DIR).join(HISTORY_DIR)
}

/// Atomically saves one apply record as a synced 0600 JSON file.
pub fn record(state_dir: &Path, apply_id: &str, uid: u32, apply: &Apply) -> Result<(), Error> {
    if !valid_apply_id(apply_id) {
        return Err(io_at(&history_dir(state_dir))(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid apply id",
        )));
    }
    state::private_dir(state_dir, uid)?;
    let rootfs = state_dir.join(ROOTFS_DIR);
    state::private_dir(&rootfs, uid)?;
    let dir = history_dir(state_dir);
    state::private_dir(&dir, uid)?;
    let name = format!("{apply_id}.json");
    state::replace_file(&dir, &name, |out| {
        serde_json::to_writer(&mut *out, apply)?;
        out.write_all(b"\n")
    })?;
    Ok(())
}

/// Loads rootfs history, newest first. A missing state or history directory
/// is an empty history; directories and files are checked before reading.
pub fn load(state_dir: &Path, uid: u32) -> Result<Vec<Apply>, Error> {
    let mut dir = state_dir.to_path_buf();
    for part in [ROOTFS_DIR, HISTORY_DIR] {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_at(&dir)(error)),
            Ok(_) => state::private_dir(&dir, uid)?,
        }
    }
    let entries = fs::read_dir(&dir).map_err(io_at(&dir))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_at(&dir))?;
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
            continue;
        };
        if !valid_apply_id(stem) {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(io_at(&path))?;
        if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o777 != 0o600 {
            return Err(io_at(&path)(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "history record is not a private regular file",
            )));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(io_at(&path))?;
        let mut body = Vec::new();
        file.read_to_end(&mut body).map_err(io_at(&path))?;
        let apply: Apply = serde_json::from_slice(&body).map_err(|source| Error::Json {
            path: path.clone(),
            source,
        })?;
        found.push((path, apply));
    }
    found.sort_by(|(a_path, a), (b_path, b)| (&b.finished, b_path).cmp(&(&a.finished, a_path)));
    Ok(found.into_iter().map(|(_, record)| record).collect())
}

fn valid_apply_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Debug, Serialize)]
pub struct Document {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub applies: Vec<Apply>,
}

impl Document {
    pub fn new(applies: Vec<Apply>, now: SystemTime) -> Document {
        Document {
            envelope: Envelope::new("rootfs-history", now),
            applies,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::testing::scratch;
    use std::os::unix::fs::PermissionsExt;

    fn uid() -> u32 {
        fs::metadata(scratch("rootfs-history-uid")).unwrap().uid()
    }

    fn apply(started: &str, finished: &str) -> Apply {
        Apply {
            mode: crate::rootfs::plan::CleanupMode::Standard,
            plan_id: "rootfs-1-2".into(),
            requested_by: Some("admin".into()),
            started: started.into(),
            finished: finished.into(),
            move_dir: JsonPath::new(b"/volume1/backup/rootfs-1-2"),
            manifest: JsonPath::new(b"/volume1/backup/rootfs-1-2/manifest.jsonl"),
            moved: Moved {
                files: 2,
                size_bytes: 12,
            },
            freed_bytes: 10,
            completion: "complete".into(),
            skipped_count: 1,
            tools: vec![Tool::new(Delegate::DeleteJournal, ToolOutcome::Succeeded)],
        }
    }

    #[test]
    fn stores_one_atomic_record_per_apply_and_loads_newest_first() {
        let state = scratch("rootfs-history-records").join("state");
        let uid = uid();
        record(
            &state,
            "attempt-1",
            uid,
            &apply("2026-09-30T14:10:00Z", "2026-09-30T14:11:00Z"),
        )
        .unwrap();
        record(
            &state,
            "attempt-2",
            uid,
            &apply("2026-09-30T14:14:00Z", "2026-09-30T14:15:00Z"),
        )
        .unwrap();

        let dir = history_dir(&state);
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["attempt-1.json", "attempt-2.json"]);
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.join("attempt-1.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let loaded = load(&state, uid).unwrap();
        assert_eq!(
            loaded,
            [
                apply("2026-09-30T14:14:00Z", "2026-09-30T14:15:00Z"),
                apply("2026-09-30T14:10:00Z", "2026-09-30T14:11:00Z"),
            ]
        );
        assert_eq!(loaded[0].tools[0].outcome, "succeeded");
    }

    #[test]
    fn missing_history_is_empty_and_bad_ids_are_refused() {
        let state = scratch("rootfs-history-empty").join("missing");
        assert!(load(&state, uid()).unwrap().is_empty());
        let error = record(&state, "../escape", uid(), &apply("a", "b")).unwrap_err();
        assert!(error.to_string().contains("invalid apply id"));
    }
}
