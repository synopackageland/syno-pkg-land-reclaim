//! The target model (SPEC 5.1): operands, path lists, overlap removal and
//! partitioning by filesystem.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A command operand before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetArg {
    /// Every supported local Btrfs volume.
    System,
    Path(PathBuf),
}

/// The literal operand `system` selects all volumes; a path named
/// `system` is written `./system`.
pub fn parse_operand(operand: OsString) -> TargetArg {
    if operand == "system" {
        TargetArg::System
    } else {
        TargetArg::Path(PathBuf::from(operand))
    }
}

/// The operand that parses back to `arg`.
pub fn operand(arg: &TargetArg) -> OsString {
    match arg {
        TargetArg::System => "system".into(),
        TargetArg::Path(path) if path.as_os_str() == "system" => "./system".into(),
        TargetArg::Path(path) => path.clone().into_os_string(),
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathListError {
    #[error("empty path at entry {0} of the path list")]
    EmptyPath(usize),
}

/// Paths of `--from-file`: one per line, or NUL-separated with `--null`.
/// Bytes are taken literally: no trimming, comments or globbing. Only a
/// final terminator is optional.
pub fn read_path_list(data: &[u8], null: bool) -> Result<Vec<PathBuf>, PathListError> {
    use std::os::unix::ffi::OsStringExt;
    let separator = if null { 0 } else { b'\n' };
    let data = data.strip_suffix(&[separator]).unwrap_or(data);
    if data.is_empty() {
        return Ok(Vec::new());
    }
    data.split(|&b| b == separator)
        .enumerate()
        .map(|(i, entry)| {
            if entry.is_empty() {
                Err(PathListError::EmptyPath(i + 1))
            } else {
                Ok(PathBuf::from(OsString::from_vec(entry.to_vec())))
            }
        })
        .collect()
}

/// Whether a target is a whole volume or a subtree.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Volume,
    Path,
}

/// A target resolved to a filesystem object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub filesystem: crate::filesystem::FilesystemKind,
    /// The operand as given.
    pub requested: PathBuf,
    /// Canonical absolute path.
    pub path: PathBuf,
    pub scope: Scope,
    pub fsid: [u8; 16],
    /// Primary mount point of the filesystem.
    pub mount: PathBuf,
    /// Subvolume tree and inode number of the object.
    pub tree: u64,
    pub ino: u64,
}

/// A target left out because another target already contains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Covered {
    pub target: Resolved,
    pub by: PathBuf,
}

/// Targets of one filesystem, without overlaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemTargets {
    pub fsid: [u8; 16],
    pub mount: PathBuf,
    pub targets: Vec<Resolved>,
}

/// Groups targets by filesystem (ordered by fsid, the lock order of
/// SPEC 22.5) and drops targets inside another target of the group.
pub fn partition(targets: Vec<Resolved>) -> (Vec<FilesystemTargets>, Vec<Covered>) {
    let mut targets = targets;
    targets.sort_by(|a, b| (a.fsid, &a.path, a.scope).cmp(&(b.fsid, &b.path, b.scope)));
    let mut groups: Vec<FilesystemTargets> = Vec::new();
    let mut covered = Vec::new();
    for target in targets {
        let group = match groups.last_mut() {
            Some(g) if g.fsid == target.fsid => g,
            _ => {
                groups.push(FilesystemTargets {
                    fsid: target.fsid,
                    mount: target.mount.clone(),
                    targets: Vec::new(),
                });
                groups.last_mut().expect("just pushed")
            }
        };
        // Sorted by path, so a containing target always comes first.
        match group
            .targets
            .iter()
            .find(|t| contains(&t.path, &target.path))
        {
            Some(outer) => covered.push(Covered {
                by: outer.path.clone(),
                target,
            }),
            None => group.targets.push(target),
        }
    }
    (groups, covered)
}

fn contains(outer: &Path, inner: &Path) -> bool {
    inner.starts_with(outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(fs: u8, path: &str, scope: Scope) -> Resolved {
        Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: PathBuf::from(path),
            path: PathBuf::from(path),
            scope,
            fsid: [fs; 16],
            mount: PathBuf::from(format!("/volume{fs}")),
            tree: 256,
            ino: 256,
        }
    }

    fn paths(group: &FilesystemTargets) -> Vec<&str> {
        group
            .targets
            .iter()
            .map(|t| t.path.to_str().unwrap())
            .collect()
    }

    #[test]
    fn system_is_a_keyword_and_dot_system_a_path() {
        assert_eq!(parse_operand("system".into()), TargetArg::System);
        assert_eq!(
            parse_operand("./system".into()),
            TargetArg::Path(PathBuf::from("./system"))
        );
    }

    #[test]
    fn operands_written_back_parse_to_the_same_targets() {
        let system = || TargetArg::Path(PathBuf::from("system"));
        assert_eq!(operand(&TargetArg::System), "system");
        assert_eq!(operand(&system()), "./system");
        let path = TargetArg::Path(PathBuf::from("/volume1/a"));
        assert_eq!(parse_operand(operand(&path)), path);
    }

    #[test]
    fn path_lists_are_literal_lines() {
        let list = read_path_list(b"/v/a b\n /v/#c\n", false).unwrap();
        assert_eq!(list, [PathBuf::from("/v/a b"), PathBuf::from(" /v/#c")]);
    }

    #[test]
    fn nul_path_lists_keep_newlines_in_names() {
        let list = read_path_list(b"/v/a\nb\0/v/c", true).unwrap();
        assert_eq!(list, [PathBuf::from("/v/a\nb"), PathBuf::from("/v/c")]);
    }

    #[test]
    fn empty_path_list_entries_are_rejected() {
        assert_eq!(
            read_path_list(b"/v/a\n\n/v/b\n", false),
            Err(PathListError::EmptyPath(2))
        );
        assert_eq!(read_path_list(b"", false), Ok(Vec::new()));
    }

    #[test]
    fn partitions_by_filesystem_in_fsid_order() {
        let (groups, covered) = partition(vec![
            target(2, "/volume2/b", Scope::Path),
            target(1, "/volume1/a", Scope::Path),
            target(2, "/volume2/a", Scope::Path),
        ]);
        assert!(covered.is_empty());
        assert_eq!(groups.len(), 2);
        assert_eq!(paths(&groups[0]), ["/volume1/a"]);
        assert_eq!(paths(&groups[1]), ["/volume2/a", "/volume2/b"]);
    }

    #[test]
    fn drops_targets_inside_other_targets_and_duplicates() {
        let (groups, covered) = partition(vec![
            target(1, "/volume1/share/Movies", Scope::Path),
            target(1, "/volume1/share", Scope::Path),
            target(1, "/volume1/share", Scope::Path),
            target(1, "/volume1/shared", Scope::Path),
        ]);
        assert_eq!(paths(&groups[0]), ["/volume1/share", "/volume1/shared"]);
        let dropped: Vec<_> = covered
            .iter()
            .map(|c| (c.target.path.to_str().unwrap(), c.by.to_str().unwrap()))
            .collect();
        assert_eq!(
            dropped,
            [
                ("/volume1/share", "/volume1/share"),
                ("/volume1/share/Movies", "/volume1/share")
            ]
        );
    }

    #[test]
    fn a_volume_target_covers_paths_on_it() {
        let (groups, covered) = partition(vec![
            target(1, "/volume1/share", Scope::Path),
            target(1, "/volume1", Scope::Volume),
        ]);
        assert_eq!(paths(&groups[0]), ["/volume1"]);
        assert_eq!(groups[0].targets[0].scope, Scope::Volume);
        assert_eq!(covered.len(), 1);
    }
}
