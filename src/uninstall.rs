//! What the root program's uninstall purge removes.

use std::io;
use std::path::Path;

/// Removes `path` and everything below it without following a symlink
/// at `path` itself; `false` when there was nothing.
pub(crate) fn remove(path: &Path) -> io::Result<bool> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("synoreclaim-remove-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_directory_goes_with_everything_below_it() {
        let dir = scratch("tree");
        fs::create_dir_all(dir.join("plans/a")).unwrap();
        fs::write(dir.join("plans/a/plan.json"), b"{}").unwrap();
        assert!(remove(&dir.join("plans")).unwrap());
        assert!(!dir.join("plans").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_is_already_gone_is_no_removal() {
        let dir = scratch("absent");
        assert!(!remove(&dir.join("plans")).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_link_loses_only_itself() {
        let dir = scratch("link");
        fs::create_dir_all(dir.join("elsewhere")).unwrap();
        fs::write(dir.join("elsewhere/kept"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join("plans")).unwrap();
        assert!(remove(&dir.join("plans")).unwrap());
        assert!(fs::symlink_metadata(dir.join("plans")).is_err());
        assert!(dir.join("elsewhere/kept").is_file());
        fs::remove_dir_all(dir).unwrap();
    }
}
