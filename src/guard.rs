//! Checks every mutation shares: refusal codes, the opt-in configuration
//! and its kill switch, trusted state, no-follow opening, and the DSM
//! paths no cleanup may touch (SPEC 5.7, 5.23, 22.8).

use crate::sys;
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PLAN_LIFETIME_SECONDS: u64 = 24 * 60 * 60;

pub(crate) const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("protected cleanup target: {0}")]
    Protected(String),
    #[error("unsupported object in cleanup selection: {0}")]
    UnsupportedObject(String),
    #[error("cleanup selection crosses a mount or subvolume boundary at {0}")]
    Boundary(String),
    #[error("cleanup target identity is unavailable: {0}")]
    Identity(String),
    #[error("cleanup is turned off in Settings")]
    Disabled,
    #[error("cleanup target is outside the configured allowed roots")]
    OutOfScope,
    #[error("cleanup state directory is writable by other users: {0}")]
    UntrustedState(String),
    #[error("cleanup I/O: {0}")]
    Io(#[from] io::Error),
    #[error("cleanup JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// A cleanup refusal (exit 4) with its symbolic code; its text is the
/// whole stderr message after `synoreclaim: `.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
    /// The limiting resource when a destination is full.
    pub limit: Option<&'static str>,
    /// Why a `protected_target` is protected ([`PROTECTED_REASONS`]).
    pub reason: Option<&'static str>,
    /// The groups of a `keeper_conflict`.
    pub conflict: Option<Box<crate::output::KeeperConflict>>,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Refusal {
            code,
            message: message.into(),
            limit: None,
            reason: None,
            conflict: None,
        }
    }

    pub fn with_conflict(mut self, conflict: crate::output::KeeperConflict) -> Self {
        self.conflict = Some(Box::new(conflict));
        self
    }

    pub fn with_limit(mut self, limit: &'static str) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The plan no longer matches the filesystem: make a new one.
    pub fn stale(detail: impl std::fmt::Display) -> Self {
        Refusal::new("plan_stale", format!("plan_stale: {detail}"))
    }
}

/// A failure about one selected path: the text, code and exit code are
/// its cause's; JSON names the path.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct AtPath {
    pub path: Vec<u8>,
    pub source: Box<dyn std::error::Error>,
}

/// The selected path a failure is about, when it names one.
pub fn failure_path<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a [u8]> {
    error.downcast_ref::<AtPath>().map(|at| at.path.as_slice())
}

/// The failure itself, without the path it is about.
pub(crate) fn cause<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> &'a (dyn std::error::Error + 'static) {
    error
        .downcast_ref::<AtPath>()
        .map_or(error, |at| cause(at.source.as_ref()))
}

/// SPEC 22.7: protected-target and disabled-cleanup refusals exit 4;
/// every other failure is fatal (1).
pub fn failure_exit_code(error: &(dyn std::error::Error + 'static)) -> i32 {
    let error = cause(error);
    if error.is::<Refusal>() {
        return 4;
    }
    match error.downcast_ref::<CleanupError>() {
        Some(
            CleanupError::Protected(_)
            | CleanupError::Boundary(_)
            | CleanupError::Disabled
            | CleanupError::OutOfScope
            | CleanupError::UntrustedState(_),
        ) => 4,
        _ => 1,
    }
}

/// The stable JSON `errors[].code` of a cleanup failure (SPEC 5.7);
/// `error` for failures without a symbolic reason.
pub fn failure_code(error: &(dyn std::error::Error + 'static)) -> &'static str {
    let error = cause(error);
    if let Some(refusal) = error.downcast_ref::<Refusal>() {
        return refusal.code;
    }
    match error.downcast_ref::<CleanupError>() {
        Some(CleanupError::Protected(_)) => "protected_target",
        Some(CleanupError::UnsupportedObject(_)) => "unsupported_object",
        Some(CleanupError::Boundary(_)) => "selection_boundary",
        Some(CleanupError::Identity(_)) => "identity_unavailable",
        Some(CleanupError::Disabled) => "cleanup_disabled",
        Some(CleanupError::OutOfScope) => "outside_allowed_roots",
        Some(CleanupError::UntrustedState(_)) => "untrusted_state",
        Some(CleanupError::Io(_)) => "io_error",
        Some(CleanupError::Json(_)) => "json_error",
        None => "error",
    }
}

/// The limiting resource attached to a refusal, when it has one.
/// The groups of a `keeper_conflict` refusal.
pub fn failure_conflict<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a crate::output::KeeperConflict> {
    cause(error)
        .downcast_ref::<Refusal>()
        .and_then(|refusal| refusal.conflict.as_deref())
}

pub fn failure_limit(error: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    cause(error)
        .downcast_ref::<Refusal>()
        .and_then(|refusal| refusal.limit)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupConfig {
    #[serde(default)]
    pub cleanup_enabled: bool,
    #[serde(default)]
    pub mutations_disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_roots: Option<Vec<String>>,
}

/// The package configuration, if it enables cleanup. The package user
/// owns it, so it is untrusted input here: read through the same strict
/// checks, failing closed on any doubt.
pub fn read_cleanup_config() -> Result<CleanupConfig, CleanupError> {
    let location = crate::config::location().map_err(|_| CleanupError::Disabled)?;
    read_config_owned_by(&location.path, location.owner)
}

/// The two cleanup switches, as `cleanup apply` would see them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct CleanupSwitches {
    pub cleanup_enabled: bool,
    pub mutations_disabled: bool,
}

/// The switches of the configuration at `path`; both off when it is
/// missing or untrusted. Readable by the package user itself.
pub(crate) fn switches_owned_by(path: &Path, owner: u32) -> CleanupSwitches {
    read_trusted_config(path, owner)
        .map(|c| CleanupSwitches {
            cleanup_enabled: c.cleanup_enabled,
            mutations_disabled: c.mutations_disabled,
        })
        .unwrap_or_default()
}

/// Reads the opt-in configuration only if `owner` alone can change it
/// and it enables cleanup.
pub(crate) fn read_config_owned_by(path: &Path, owner: u32) -> Result<CleanupConfig, CleanupError> {
    let config = read_trusted_config(path, owner)?;
    if !config.cleanup_enabled || config.mutations_disabled {
        return Err(CleanupError::Disabled);
    }
    Ok(config)
}

/// The configuration as written, only if `owner` alone can change it;
/// whether it enables cleanup is the caller's question.
pub fn read_trusted_config(path: &Path, owner: u32) -> Result<CleanupConfig, CleanupError> {
    let parent = path.parent().ok_or(CleanupError::Disabled)?;
    let parent_meta = fs::symlink_metadata(parent)?;
    if !parent_meta.is_dir() || parent_meta.uid() != owner || parent_meta.mode() & 0o022 != 0 {
        return Err(CleanupError::Disabled);
    }
    let meta = fs::symlink_metadata(path).map_err(|_| CleanupError::Disabled)?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
        return Err(CleanupError::Disabled);
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(CleanupError::Disabled);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| CleanupError::Disabled)?;
    let opened = file.metadata()?;
    if opened.ino() != meta.ino() || opened.uid() != owner || opened.mode() & 0o077 != 0 {
        return Err(CleanupError::Disabled);
    }
    serde_json::from_reader(&mut file).map_err(|_| CleanupError::Disabled)
}

/// A directory above private cleanup state: only root or `uid` can write
/// it, so no other user can swap the manifest or plan below it.
pub(crate) fn trusted_state_ancestor(mode: u32, owner: u32, uid: u32) -> bool {
    mode & libc::S_IFMT == libc::S_IFDIR && (owner == 0 || owner == uid) && mode & 0o022 == 0
}

/// Refuses a state directory that has an ancestor, or is itself, writable
/// by another user. DSM share ACLs are not visible to this mode check.
pub fn check_state_ancestors(state_dir: &Path, uid: u32) -> Result<(), CleanupError> {
    for ancestor in absolute_lexical(state_dir)?.ancestors() {
        let metadata = fs::metadata(ancestor)?;
        if !trusted_state_ancestor(metadata.mode(), metadata.uid(), uid) {
            return Err(CleanupError::UntrustedState(crate::output::escape_human(
                ancestor.as_os_str().as_bytes(),
            )));
        }
    }
    Ok(())
}

/// Keeps an opened file only if it is a regular file that `owner` owns
/// and nobody else can read or write, so later reads use checked content.
pub fn private_file(file: File, owner: u32) -> Result<File, CleanupError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(CleanupError::Identity(
            "cleanup state file is not a private regular file".into(),
        ));
    }
    Ok(file)
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// A stop requested before the executor starts ends apply with the
/// signal's exit code; nothing has been deleted yet.
pub fn stop_before_executor(signal: Option<i32>) -> Option<i32> {
    signal.map(|signal| crate::document::Completion::Paused.exit_code(Some(signal)))
}

/// Unique enough across invocations without a dependency on DSM's RNG API.
pub fn new_plan_id(now: SystemTime, pid: u32) -> String {
    let nanos = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("cleanup-{nanos:x}-{pid:x}")
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Converts relative operands to a normalized absolute path without
/// resolving symlinks. The executor opens each component with O_NOFOLLOW.
pub fn absolute_lexical(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an empty path is not a cleanup target",
        ));
    }
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::from("/");
    for component in source.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "path escapes the filesystem root",
                    ));
                }
            }
            Component::Normal(name) => result.push(name),
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-Unix path prefix is unsupported",
                ));
            }
        }
    }
    Ok(result)
}

/// Opens an absolute path by walking from `/`; no path component may be a
/// symbolic link. Only regular files and directories are supported.
pub fn open_absolute_no_follow(path: &Path) -> Result<File, CleanupError> {
    let path = absolute_lexical(path)?;
    let mut current = File::open("/")?;
    let components: Vec<OsString> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_os_string()),
            _ => None,
        })
        .collect();
    for (index, name) in components.iter().enumerate() {
        let stat = sys::statat(current.as_fd(), name)?;
        let kind = stat.mode & libc::S_IFMT;
        if kind != libc::S_IFDIR && kind != libc::S_IFREG {
            return Err(CleanupError::UnsupportedObject(format!(
                "{} has unsupported file type {kind:#o}",
                crate::output::escape_human(name.as_bytes())
            )));
        }
        let final_component = index + 1 == components.len();
        let flags = if kind == libc::S_IFDIR {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        } else {
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        let opened = sys::openat_noatime(current.as_fd(), name, flags, 0)?;
        if !names_descriptor(&stat, &opened)? {
            return Err(CleanupError::Identity(format!(
                "{} changed while opening",
                crate::output::escape_human(path.as_os_str().as_bytes())
            )));
        }
        current = opened;
        if final_component {
            return Ok(current);
        }
        if kind != libc::S_IFDIR {
            return Err(CleanupError::UnsupportedObject(format!(
                "{} has a non-directory ancestor",
                crate::output::escape_human(path.as_os_str().as_bytes())
            )));
        }
    }
    Ok(current)
}

pub fn is_protected_name(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    bytes.starts_with(b"@") || bytes.starts_with(b"#")
}

pub fn protected_target_reason(path: &Path, volume: &Path) -> Option<String> {
    if path == volume {
        return Some("volume root".into());
    }
    if [
        "/proc", "/sys", "/dev", "/run", "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/boot",
    ]
    .iter()
    .any(|root| path == Path::new(root) || path.starts_with(root))
    {
        return Some("system path".into());
    }
    if path
        .as_os_str()
        .as_bytes()
        .split(|byte| *byte == b'/')
        .any(|name| name == b"#recycle")
    {
        return Some(
            "a Recycle Bin or an item in it: Reclaim never removes anything from a Recycle Bin; empty it in DSM (Control Panel > Shared Folder > Action > Empty Recycle Bin)".into(),
        );
    }
    if let Some(service) =
        crate::dsm::service_of(path.as_os_str().as_bytes(), volume.as_os_str().as_bytes())
    {
        return Some(service.refusal());
    }
    protected_names(path.as_os_str().as_bytes())
}

/// The reasons a `protected_target` refusal names, so a reader can say
/// exactly why: the volume itself, a system path, a Recycle Bin, a
/// package's data, a DSM-managed `@` folder, a shared folder itself, or
/// a path outside every shared folder.
pub const PROTECTED_REASONS: &[&str] = &[
    "volume",
    "system",
    "recycle_bin",
    "package",
    "dsm_folder",
    "share_root",
    "home",
    "outside_share",
];

/// Why [`target_refusal`] refused `path` on `volume`: one of
/// [`PROTECTED_REASONS`], and the package for `package`. A shared folder
/// itself is `share_root`, whatever told it.
pub fn protected_reason(path: &Path, volume: &Path) -> (&'static str, Option<&'static str>) {
    if path == volume {
        return ("volume", None);
    }
    if protected_target_reason(path, volume).is_some() {
        let bytes = path.as_os_str().as_bytes();
        if bytes.split(|b| *b == b'/').any(|name| name == b"#recycle") {
            return ("recycle_bin", None);
        }
        if let Some(service) = crate::dsm::service_of(bytes, volume.as_os_str().as_bytes()) {
            return ("package", Some(service.package));
        }
        if protected_names(bytes).is_some() {
            return ("dsm_folder", None);
        }
        return ("system", None);
    }
    if path.parent() == Some(volume) {
        return ("share_root", None);
    }
    match marker_above(path, volume) {
        Ok(Some(marker)) => ("package", crate::dsm::marked_by(marker).map(|s| s.package)),
        _ => ("share_root", None),
    }
}

/// The generic refusal of a path through an `@`/`#` name.
pub(crate) fn protected_names(path: &[u8]) -> Option<String> {
    path.split(|byte| *byte == b'/')
        .any(|name| is_protected_name(OsStr::from_bytes(name)))
        .then(|| "DSM-managed directory".into())
}

/// Why `path` on `volume` cannot be a cleanup target: a protected path
/// (see [`protected_target_reason`]), or a path in a folder that a
/// package marker marks as package data (SPEC 5.7).
pub fn target_refusal(path: &Path, volume: &Path) -> Result<Option<String>, CleanupError> {
    if let Some(reason) = protected_target_reason(path, volume) {
        return Ok(Some(reason));
    }
    let ext4_dsm = Path::new("/etc.defaults/VERSION").exists()
        && sys::filesystem_type(open_absolute_no_follow(volume)?.as_fd())?
            == crate::ext4::SUPER_MAGIC;
    let shares = if ext4_dsm {
        Some(crate::dsm_shares::list(volume)?)
    } else {
        None
    };
    if let Some(shares) = &shares {
        let selected = open_absolute_no_follow(path)?;
        let meta = selected.metadata()?;
        if shares
            .iter()
            .any(|share| share.device == meta.dev() && share.inode == meta.ino())
        {
            return Ok(Some(
                "DSM shared-folder root: select items inside it".into(),
            ));
        }
    }
    marker_refusal_above(path, volume)
}

/// Why a folder between `stop` (exclusive) and `path` marks `path` as
/// package data (SPEC 5.7 rule 3), if one does.
pub(crate) fn marker_refusal_above(
    path: &Path,
    stop: &Path,
) -> Result<Option<String>, CleanupError> {
    Ok(marker_above(path, stop)?.and_then(crate::dsm::marker_refusal))
}

/// The package marker held by the nearest folder between `stop`
/// (exclusive) and `path`, if any.
pub fn marker_above(path: &Path, stop: &Path) -> io::Result<Option<&'static [u8]>> {
    for folder in path.ancestors().take_while(|folder| *folder != stop) {
        for marker in crate::dsm::markers() {
            match std::fs::symlink_metadata(folder.join(OsStr::from_bytes(marker))) {
                Ok(_) => return Ok(Some(marker)),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(None)
}

/// A subvolume root, or the empty placeholder directory (inode 2) a
/// snapshot shows for a nested subvolume. SPEC 15 makes both boundaries:
/// removing a placeholder unlinks the nested subvolume's entry.
pub fn is_subvolume_boundary(file_type: u32, inode: u64) -> bool {
    file_type == libc::S_IFDIR
        && (inode == crate::btrfs::keys::EMPTY_SUBVOL_DIR_OBJECTID
            || inode == crate::btrfs::keys::FIRST_FREE_OBJECTID)
}

/// Only the mutation's own user may reach a private folder it made (rootfs
/// apply's move folder); a DSM ACL can open it
/// to others whatever its mode bits say.
pub(crate) fn private_to_cleanup(owner: u32, mode: u32, dsm_acl: bool) -> bool {
    owner == sys::effective_uid() && mode & 0o077 == 0 && !dsm_acl
}

/// Whether DSM applies an ACL of its own or an inherited one to `file`.
pub(crate) fn dsm_acl(file: &File) -> io::Result<bool> {
    for name in crate::dsm::ACL_XATTRS {
        if sys::has_xattr(file.as_fd(), OsStr::new(name))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a name lookup and an open descriptor are one object. Inode
/// numbers repeat across Btrfs subvolumes, so the device must match too.
pub(crate) fn names_descriptor(stat: &sys::FileStat, file: &File) -> io::Result<bool> {
    let metadata = file.metadata()?;
    Ok(stat.device == metadata.dev()
        && stat.inode == metadata.ino()
        && stat.mode == metadata.mode())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsm_paths_volume_roots_and_recycle_bins_are_protected() {
        let volume = Path::new("/volume1");
        let reason = |path: &str| protected_target_reason(Path::new(path), volume);
        assert_eq!(reason("/volume1").as_deref(), Some("volume root"));
        assert_eq!(reason("/etc/passwd").as_deref(), Some("system path"));
        for path in [
            "/volume1/photos/#recycle",
            "/volume1/photos/#recycle/old.jpg",
            "/volume1/homes/alice/#recycle/a",
        ] {
            assert!(reason(path).unwrap().contains("Recycle Bin"), "{path}");
        }
        assert!(reason("/volume1/@appstore/x").is_some());
        assert!(reason("/volume1/photos/x/@eaDir").is_some());
        assert!(reason("/volume1/photos/2023/a.jpg").is_none());
    }

    /// Each protected path says exactly why, never a catch-all.
    #[test]
    fn a_protected_path_names_its_reason() {
        let volume = Path::new("/volume1");
        let reason = |path: &str| protected_reason(Path::new(path), volume);
        assert_eq!(reason("/volume1"), ("volume", None));
        assert_eq!(
            reason("/volume1/photos/#recycle/old.jpg"),
            ("recycle_bin", None)
        );
        let (why, package) = reason("/volume1/@appstore/x");
        assert_eq!(why, "package");
        assert!(package.is_some());
        assert_eq!(reason("/volume1/photos"), ("share_root", None));
        for path in ["/volume1", "/volume1/@appstore/x", "/volume1/photos"] {
            assert!(PROTECTED_REASONS.contains(&reason(path).0), "{path}");
        }
    }

    #[test]
    fn refusals_exit_four_and_keep_their_code() {
        let refusal = Refusal::new("in_recycle_bin", "in a Recycle Bin");
        assert_eq!(failure_exit_code(&refusal), 4);
        assert_eq!(failure_code(&refusal), "in_recycle_bin");
        let at = AtPath {
            path: b"/volume1/s/a".to_vec(),
            source: Box::new(CleanupError::Protected("x".into())),
        };
        assert_eq!(failure_path(&at), Some(&b"/volume1/s/a"[..]));
        assert_eq!(failure_code(&at), "protected_target");
        assert_eq!(
            failure_exit_code(&CleanupError::Io(io::Error::other("x"))),
            1
        );
    }

    #[test]
    fn an_absolute_path_is_normalized_without_following_links() {
        assert_eq!(
            absolute_lexical(Path::new("/volume1/a/../b/./c")).unwrap(),
            Path::new("/volume1/b/c")
        );
        assert!(absolute_lexical(Path::new("")).is_err());
        assert!(absolute_lexical(Path::new("/..")).is_err());
    }
}
