//! The last `rootfs` JSON report, kept below the configured state directory.

use crate::state::{self, StateError};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const DIR: &str = "rootfs";
pub const FILE: &str = "last-report.json";
pub const FORMAT: u32 = 1;
const MAX_CACHE_BYTES: u64 = 16 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedReport {
    pub created_at_s: u64,
    pub exit_code: i32,
    pub document: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskReport {
    format: u32,
    created_at_s: u64,
    exit_code: i32,
    document: String,
}

pub fn load(
    state_dir: &Path,
    uid: u32,
    max_age_s: u64,
    now_s: u64,
) -> Result<Option<CachedReport>, StateError> {
    if max_age_s == 0 {
        return Ok(None);
    }
    state::private_dir(state_dir, uid)?;
    let dir = state_dir.join(DIR);
    match fs::symlink_metadata(&dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_at(&dir, error)),
        Ok(_) => {}
    }
    state::private_dir(&dir, uid)?;

    let path = dir.join(FILE);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_at(&path, error)),
        Ok(file) => file,
    };
    let metadata = file.metadata().map_err(|error| io_at(&path, error))?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MAX_CACHE_BYTES
    {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_at(&path, error))?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Ok(None);
    }
    let Ok(entry) = serde_json::from_slice::<DiskReport>(&bytes) else {
        return Ok(None);
    };
    if entry.format != FORMAT || !matches!(entry.exit_code, 0 | 3) {
        return Ok(None);
    }
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&entry.document) else {
        return Ok(None);
    };
    let completion_matches = match document["completion"].as_str() {
        Some("complete") => entry.exit_code == 0,
        Some("partial") => entry.exit_code == 3,
        _ => false,
    };
    if document["command"] != "rootfs" || !completion_matches {
        return Ok(None);
    }
    let Some(age_s) = now_s.checked_sub(entry.created_at_s) else {
        return Ok(None);
    };
    if age_s > max_age_s {
        return Ok(None);
    }
    Ok(Some(CachedReport {
        created_at_s: entry.created_at_s,
        exit_code: entry.exit_code,
        document: entry.document,
    }))
}

pub fn save(
    state_dir: &Path,
    uid: u32,
    created_at_s: u64,
    exit_code: i32,
    document: &str,
) -> Result<(), StateError> {
    if !matches!(exit_code, 0 | 3) || document.len() as u64 > MAX_CACHE_BYTES {
        return Ok(());
    }
    state::private_dir(state_dir, uid)?;
    let dir = state_dir.join(DIR);
    state::private_dir(&dir, uid)?;
    let entry = DiskReport {
        format: FORMAT,
        created_at_s,
        exit_code,
        document: document.to_string(),
    };
    state::replace_file(&dir, FILE, |out| {
        serde_json::to_writer(&mut *out, &entry).map_err(io::Error::other)?;
        out.write_all(b"\n")
    })
}

/// Drops the saved report, so the next read walks `/` again.
pub fn forget(state_dir: &Path) -> Result<(), StateError> {
    let path = state_dir.join(DIR).join(FILE);
    match fs::remove_file(&path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(&path, error)),
        _ => Ok(()),
    }
}

fn io_at(path: &Path, source: io::Error) -> StateError {
    StateError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "synoreclaim-rootfs-cache-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            state::private_dir(&path, crate::sys::effective_uid()).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saved_report_is_private_and_reused_only_within_its_max_age() {
        let scratch = Scratch::new();
        let state_dir = scratch.0.join("state");
        let document =
            "{\"schema_version\":1,\"command\":\"rootfs\",\"completion\":\"complete\"}\n";
        save(&state_dir, crate::sys::effective_uid(), 1000, 0, document).unwrap();

        let path = state_dir.join(DIR).join(FILE);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            load(&state_dir, crate::sys::effective_uid(), 30, 1030).unwrap(),
            Some(CachedReport {
                created_at_s: 1000,
                exit_code: 0,
                document: document.into(),
            })
        );
        assert!(
            load(&state_dir, crate::sys::effective_uid(), 30, 1031)
                .unwrap()
                .is_none()
        );
        assert!(
            load(&state_dir, crate::sys::effective_uid(), 0, 1000)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn malformed_and_future_dated_cache_entries_are_not_reused() {
        let scratch = Scratch::new();
        let state_dir = scratch.0.join("state");
        let uid = crate::sys::effective_uid();
        let document =
            "{\"schema_version\":1,\"command\":\"rootfs\",\"completion\":\"complete\"}\n";
        save(&state_dir, uid, 2000, 0, document).unwrap();
        assert!(load(&state_dir, uid, 30, 1999).unwrap().is_none());

        let path = state_dir.join(DIR).join(FILE);
        fs::write(path, b"not json").unwrap();
        assert!(load(&state_dir, uid, 30, 2001).unwrap().is_none());
    }

    #[test]
    fn a_forgotten_report_is_not_reused() {
        let scratch = Scratch::new();
        let state_dir = scratch.0.join("state");
        let uid = crate::sys::effective_uid();
        forget(&state_dir).unwrap();
        let document =
            "{\"schema_version\":1,\"command\":\"rootfs\",\"completion\":\"complete\"}\n";
        save(&state_dir, uid, 1000, 0, document).unwrap();

        forget(&state_dir).unwrap();

        assert!(load(&state_dir, uid, 30, 1001).unwrap().is_none());
        forget(&state_dir).unwrap();
    }
}
