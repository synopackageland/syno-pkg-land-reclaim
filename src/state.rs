//! Private on-disk state (SPEC 15, 22.4, 22.5): the report store under
//! `--state-dir` and the canonical per-filesystem writer lock.

pub use crate::document::{CleanupMark, Marked};
use crate::report::{self, Header, ReportError, RootInfo};
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Fixed, independent of `--state-dir`, so two state locations cannot
/// bypass writer exclusion.
pub const LOCK_DIR: &str = "/run/synoreclaim/locks";
const REPORT_SUFFIX: &str = ".srrep";
/// Reports kept per filesystem; the oldest beyond this are pruned.
pub const REPORTS_KEPT: usize = 32;
/// Per filesystem, next to its reports (SPEC 5.7 "Reports after cleanup").
const CLEANUP_RECORD: &str = "cleanups.json";
const CLEANUPS_KEPT: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{0}: state directory must be owned by the current user with mode 0700")]
    Insecure(PathBuf),
    #[error("{0}: not a directory")]
    NotDirectory(PathBuf),
    #[error("another synoreclaim process is writing filesystem {fsid}{}", held_by(holder.as_ref()))]
    Busy {
        fsid: String,
        /// Who holds the lock, when its file says so.
        holder: Option<LockHolder>,
    },
    #[error("another synoreclaim process is cleaning the system partition{}", held_by(holder.as_ref()))]
    RootfsBusy {
        /// Who holds the NAS-wide rootfs lock, when its file says so.
        holder: Option<LockHolder>,
    },
}

/// What a writer lock's holder records in the lock file, so a refused
/// writer can name it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHolder {
    pub job_id: Option<String>,
    /// `scan`, `cache-clear`, `cleanup-apply`, `rootfs-apply` or `doctor`.
    pub command: String,
}

impl LockHolder {
    pub fn new(job_id: Option<&str>, command: &str) -> Self {
        LockHolder {
            job_id: job_id.map(str::to_string),
            command: command.to_string(),
        }
    }
}

fn held_by(holder: Option<&LockHolder>) -> String {
    match holder {
        Some(LockHolder {
            job_id: Some(id),
            command,
        }) => format!(" ({command}, job {id})"),
        Some(LockHolder { command, .. }) => format!(" ({command})"),
        None => String::new(),
    }
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> StateError + '_ {
    move |source| StateError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Creates `path` (0700) if missing and checks that it is a real
/// directory owned by `uid` that only its owner can access (SPEC 15).
pub fn private_dir(path: &Path, uid: u32) -> Result<(), StateError> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(io_at(path))?,
        Err(e) => return Err(io_at(path)(e)),
        Ok(_) => {}
    }
    let meta = fs::symlink_metadata(path).map_err(io_at(path))?;
    if !meta.is_dir() {
        return Err(StateError::NotDirectory(path.to_path_buf()));
    }
    if meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(StateError::Insecure(path.to_path_buf()));
    }
    Ok(())
}

/// The report store of one state directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    uid: u32,
}

impl Store {
    pub fn open(root: &Path, uid: u32) -> Result<Store, StateError> {
        private_dir(root, uid)?;
        Ok(Store {
            root: root.to_path_buf(),
            uid,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn reports_dir(&self, fsid: &str) -> PathBuf {
        self.root.join("reports").join(fsid)
    }

    /// Atomically replaces the report of `header`'s scope: written to a
    /// 0600 temporary file, synced, then renamed. Returns its path and size.
    pub fn publish(
        &self,
        header: &Header,
        write: impl FnOnce(&mut dyn Write) -> io::Result<u64>,
    ) -> Result<(PathBuf, u64), StateError> {
        private_dir(&self.root.join("reports"), self.uid)?;
        let dir = self.reports_dir(&header.fsid);
        private_dir(&dir, self.uid)?;
        let name = format!("{}{REPORT_SUFFIX}", scope_key(&header.roots));
        let path = dir.join(&name);
        let tmp = dir.join(format!(".{name}.{}.tmp", header.job_id));
        let result = write_synced(&tmp, write);
        let size = match result {
            Ok(size) => size,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(io_at(&tmp)(e));
            }
        };
        fs::rename(&tmp, &path).map_err(io_at(&path))?;
        File::open(&dir)
            .and_then(|d| d.sync_all())
            .map_err(io_at(&dir))?;
        // Derived display data is disposable; a good report survives a summary failure.
        let _ = crate::report_summary::ensure(&path);
        let _ = crate::report_summary::index::ensure(&path);
        self.prune(&header.fsid)?;
        Ok((path, size))
    }

    /// Reports of a filesystem with readable headers, newest first.
    /// Unreadable reports are returned separately.
    pub fn reports(&self, fsid: &str) -> Result<Listing, StateError> {
        self.list_reports(fsid, false)
    }

    /// Header metadata for display selection, without decoding snapshot holdings on a hit.
    pub fn display_reports(&self, fsid: &str) -> Result<Listing, StateError> {
        self.list_reports(fsid, true)
    }

    fn list_reports(&self, fsid: &str, display: bool) -> Result<Listing, StateError> {
        let dir = self.reports_dir(fsid);
        let mut listing = Listing::default();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(listing),
            Err(e) => return Err(io_at(&dir)(e)),
        };
        for entry in entries {
            let path = entry.map_err(io_at(&dir))?.path();
            if !is_report_name(&path) {
                continue;
            }
            if display && let Some(summary) = crate::report_summary::load(&path) {
                listing.reports.push((path, summary.header));
                continue;
            }
            match File::open(&path)
                .map_err(ReportError::from)
                .and_then(|f| report::read_header(io::BufReader::new(f)))
            {
                Ok(header) => listing.reports.push((path, header)),
                Err(e) => listing.unreadable.push((path, e)),
            }
        }
        listing
            .reports
            .sort_by(|a, b| (b.1.scan_finished, &b.0).cmp(&(a.1.scan_finished, &a.0)));
        Ok(listing)
    }

    /// Removes every cached report for a filesystem after a cleanup
    /// mutation, so later answers cannot reuse pre-mutation state.
    /// Marks every report of `fsid` as made stale by the cleanup `mark`
    /// (SPEC 5.7 "Reports after cleanup"). Reports published later stay
    /// unmarked.
    pub fn record_cleanup(&self, fsid: &str, mark: CleanupMark) -> Result<(), StateError> {
        private_dir(&self.root.join("reports"), self.uid)?;
        let dir = self.reports_dir(fsid);
        private_dir(&dir, self.uid)?;
        let current: Vec<String> = self
            .reports(fsid)?
            .reports
            .into_iter()
            .map(|(_, header)| header.job_id)
            .collect();
        let mut cleanups = match self.cleanup_marks(fsid) {
            CleanupMarks::Read(records) => records,
            CleanupMarks::Unreadable => Vec::new(),
        };
        cleanups.retain(|record| {
            record.mark.execution_log != mark.execution_log
                && record.reports.iter().any(|id| current.contains(id))
        });
        cleanups.push(CleanupRecord {
            mark,
            reports: current,
        });
        cleanups.drain(..cleanups.len().saturating_sub(CLEANUPS_KEPT));
        replace_file(&dir, CLEANUP_RECORD, |out| {
            serde_json::to_writer(&mut *out, &CleanupFile { cleanups })?;
            out.write_all(b"\n")
        })
    }

    /// The cleanups recorded for the reports of `fsid`.
    pub fn cleanup_marks(&self, fsid: &str) -> CleanupMarks {
        read_cleanup_marks(&self.reports_dir(fsid), self.uid)
    }

    fn prune(&self, fsid: &str) -> Result<(), StateError> {
        let listing = self.reports(fsid)?;
        for (path, _) in listing.reports.iter().skip(REPORTS_KEPT) {
            fs::remove_file(path).map_err(io_at(path))?;
            let _ = fs::remove_file(crate::report_summary::path(path));
            let _ = fs::remove_file(crate::report_summary::index::path(path));
            let _ = fs::remove_file(crate::duplicates::candidates::path(path));
        }
        Ok(())
    }
}

/// One recorded cleanup and the job ids of the reports it made stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupRecord {
    #[serde(flatten)]
    pub mark: CleanupMark,
    pub reports: Vec<String>,
}

/// The cleanups recorded beside the report at `report`, a file only
/// `uid` may change.
pub fn cleanup_marks_beside(report: &Path, uid: u32) -> CleanupMarks {
    report
        .parent()
        .map_or(CleanupMarks::Unreadable, |dir| read_cleanup_marks(dir, uid))
}

fn read_cleanup_marks(dir: &Path, uid: u32) -> CleanupMarks {
    let path = dir.join(CLEANUP_RECORD);
    let read = || -> io::Result<CleanupFile> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::other("cleanup record is not a private file"));
        }
        Ok(serde_json::from_reader(io::BufReader::new(file))?)
    };
    match read() {
        Ok(file) => CleanupMarks::Read(file.cleanups),
        Err(error) if error.kind() == io::ErrorKind::NotFound => CleanupMarks::Read(Vec::new()),
        Err(_) => CleanupMarks::Unreadable,
    }
}

#[derive(Serialize, Deserialize)]
struct CleanupFile {
    cleanups: Vec<CleanupRecord>,
}

/// The cleanup record of one filesystem, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupMarks {
    Read(Vec<CleanupRecord>),
    /// The record exists but cannot be trusted or parsed.
    Unreadable,
}

impl CleanupMarks {
    /// The newest cleanup that made the report of `job_id` stale.
    pub fn of(&self, job_id: &str) -> Marked {
        match self {
            CleanupMarks::Read(records) => records
                .iter()
                .rev()
                .find(|record| record.reports.iter().any(|id| id == job_id))
                .map_or(Marked::No, |record| Marked::By(record.mark.clone())),
            CleanupMarks::Unreadable => Marked::Unknown,
        }
    }
}

#[derive(Debug, Default)]
pub struct Listing {
    pub reports: Vec<(PathBuf, Header)>,
    pub unreadable: Vec<(PathBuf, ReportError)>,
}

fn is_report_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(REPORT_SUFFIX) && !n.starts_with('.'))
}

/// Writes a new 0600 file through `write` and syncs it.
pub(crate) fn write_synced(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> io::Result<u64>,
) -> io::Result<u64> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut out = BufWriter::new(file);
    let size = write(&mut out)?;
    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(size)
}

/// Atomically replaces `dir/name` with what `write` writes: a synced
/// 0600 temporary file renamed over it, then the directory synced.
pub(crate) fn replace_file(
    dir: &Path,
    name: &str,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<(), StateError> {
    replace_file_with(dir, name, true, write)
}

/// Like `replace_file`, but without either sync: the rename is still
/// atomic, so a reader never sees a half-written file, while a crash may
/// lose the write. For disposable and frequent data such as live scan
/// progress, making the filesystem commit every couple of seconds costs
/// far more than the data is worth (SPEC 22.1).
pub(crate) fn replace_file_unflushed(
    dir: &Path,
    name: &str,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<(), StateError> {
    replace_file_with(dir, name, false, write)
}

fn replace_file_with(
    dir: &Path,
    name: &str,
    sync: bool,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<(), StateError> {
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp"));
    let written = (|| {
        let _ = fs::remove_file(&tmp);
        if sync {
            write_synced(&tmp, |out| write(out).map(|()| 0))?;
        } else {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            let mut out = BufWriter::new(file);
            write(&mut out)?;
            out.flush()?;
        }
        fs::rename(&tmp, &path)?;
        if sync {
            File::open(dir)?.sync_all()?;
        }
        Ok(())
    })();
    written.map_err(io_at(&path))
}

/// Names the report file of a root set: order-independent FNV-1a 64 of
/// each root's scope, identity, and path.
pub fn scope_key(roots: &[RootInfo]) -> String {
    let mut keys: Vec<Vec<u8>> = roots
        .iter()
        .map(|r| {
            let mut k = format!("{:?}:{}:{}:", r.scope, r.tree, r.ino).into_bytes();
            k.extend_from_slice(&r.path);
            k
        })
        .collect();
    keys.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for k in keys {
        for b in k.into_iter().chain([0]) {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// A held writer lock; released when dropped or when the process exits.
#[derive(Debug)]
pub struct FsLock {
    _lock: crate::sys::ExclusiveLock,
    pub path: PathBuf,
}

/// Takes the exclusive writer lock of filesystem `fsid` under `dir`
/// without waiting, and records `holder` in it.
pub fn lock_filesystem(
    dir: &Path,
    fsid: &str,
    uid: u32,
    holder: &LockHolder,
) -> Result<FsLock, StateError> {
    writer_lock(
        dir,
        &format!("{fsid}.lock"),
        uid,
        holder,
        LockScope::Filesystem(fsid.to_string()),
    )
}

/// Locks mutation subtrees independently of read-only scans. Every ancestor
/// is shared and every selected root is exclusive, so siblings can proceed
/// but a remover of a parent can never race an incoming copy below it.
/// Paths must use the filesystem's primary mount, just as the sealed plans do.
pub fn lock_mutation_paths(
    dir: &Path,
    fsid: &str,
    mount: &Path,
    paths: &[PathBuf],
    uid: u32,
    holder: &LockHolder,
) -> Result<Vec<crate::sys::PathLock>, StateError> {
    use std::collections::BTreeMap;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    private_dir(dir, uid)?;
    let mut wanted = BTreeMap::<PathBuf, bool>::new();
    for path in paths {
        let relative = path.strip_prefix(mount).map_err(|_| {
            io_at(path)(io::Error::other(
                "mutation path is outside its primary mount",
            ))
        })?;
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(io_at(path)(io::Error::other(
                "mutation path is not normalized",
            )));
        }
        let mut key = PathBuf::new();
        wanted.entry(key.clone()).or_insert(false);
        for part in relative.components() {
            key.push(part.as_os_str());
            wanted.entry(key.clone()).or_insert(false);
        }
        wanted.insert(key, true);
    }
    let mut held = Vec::new();
    for (relative, exclusive) in wanted {
        // A collision only serializes unrelated paths; it cannot permit overlap.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in fsid
            .bytes()
            .chain([0])
            .chain(relative.as_os_str().as_bytes().iter().copied())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        let path = dir.join(format!("mutation-{hash:016x}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(io_at(&path))?;
        match crate::sys::PathLock::try_take(file, exclusive).map_err(io_at(&path))? {
            Ok(lock) => {
                if exclusive {
                    record_holder(lock.file(), holder).map_err(io_at(&path))?;
                }
                held.push(lock);
            }
            Err(file) => {
                return Err(StateError::Busy {
                    fsid: fsid.to_string(),
                    holder: read_holder(&file),
                });
            }
        }
    }
    Ok(held)
}

/// Takes the NAS-wide rootfs writer lock under the same canonical lock
/// directory as per-filesystem locks, without waiting.
pub fn lock_rootfs(dir: &Path, uid: u32, holder: &LockHolder) -> Result<FsLock, StateError> {
    writer_lock(dir, "rootfs.lock", uid, holder, LockScope::Rootfs)
}

#[derive(Debug)]
enum LockScope {
    Filesystem(String),
    Rootfs,
}

fn writer_lock(
    dir: &Path,
    name: &str,
    uid: u32,
    holder: &LockHolder,
    scope: LockScope,
) -> Result<FsLock, StateError> {
    private_dir(dir, uid)?;
    let path = dir.join(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(io_at(&path))?;
    match crate::sys::ExclusiveLock::try_take(file) {
        Ok(Ok(lock)) => {
            record_holder(lock.file(), holder).map_err(io_at(&path))?;
            Ok(FsLock { _lock: lock, path })
        }
        Ok(Err(file)) => {
            let holder = read_holder(&file);
            Err(match scope {
                LockScope::Filesystem(fsid) => StateError::Busy { fsid, holder },
                LockScope::Rootfs => StateError::RootfsBusy { holder },
            })
        }
        Err(e) => Err(io_at(&path)(e)),
    }
}

fn record_holder(file: &File, holder: &LockHolder) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let body = serde_json::to_vec(holder).expect("holder serializes");
    file.set_len(0)?;
    file.write_all_at(&body, 0)
}

/// The holder a lock file names; none while it is being written.
fn read_holder(mut file: &File) -> Option<LockHolder> {
    use std::io::Read;
    let mut body = Vec::new();
    file.read_to_end(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tree::ScopeTree;
    use crate::report::testing::header;
    use crate::spill::testing::scratch;
    use crate::target::Scope;

    #[test]
    fn mutation_paths_allow_scans_and_disjoint_cleanup_but_refuse_overlap() {
        let dir = scratch("mutation-paths").join("locks");
        let uid = crate::sys::effective_uid();
        let mount = Path::new("/volume1");
        let rootfs = LockHolder::new(Some("rootfs"), "rootfs-apply");
        let cleanup = LockHolder::new(Some("cleanup"), "cleanup-apply");
        let copied = PathBuf::from("/volume1/share/SynoReclaim-system-date");
        let scan = lock_filesystem(&dir, "fs", uid, &LockHolder::new(None, "scan")).unwrap();
        let incoming = lock_mutation_paths(
            &dir,
            "fs",
            mount,
            std::slice::from_ref(&copied),
            uid,
            &rootfs,
        )
        .unwrap();
        let unrelated = lock_mutation_paths(
            &dir,
            "fs",
            mount,
            &[PathBuf::from("/volume1/share/old")],
            uid,
            &cleanup,
        )
        .unwrap();
        assert!(
            lock_mutation_paths(
                &dir,
                "fs",
                mount,
                std::slice::from_ref(&copied),
                uid,
                &cleanup
            )
            .is_err()
        );
        assert!(
            lock_mutation_paths(&dir, "fs", mount, &[copied.join("file")], uid, &cleanup).is_err()
        );
        assert!(
            lock_mutation_paths(
                &dir,
                "fs",
                mount,
                &[PathBuf::from("/volume1/share")],
                uid,
                &cleanup
            )
            .is_err()
        );
        drop(incoming);
        let incoming = lock_mutation_paths(
            &dir,
            "fs",
            mount,
            std::slice::from_ref(&copied),
            uid,
            &cleanup,
        )
        .unwrap();
        assert!(
            lock_mutation_paths(
                &dir,
                "fs",
                mount,
                std::slice::from_ref(&copied),
                uid,
                &rootfs
            )
            .is_err()
        );
        drop((scan, unrelated, incoming));
        assert!(lock_mutation_paths(&dir, "fs", mount, &[copied], uid, &rootfs).is_ok());
    }

    fn uid() -> u32 {
        fs::metadata(std::env::temp_dir().join("."))
            .map(|_| ())
            .unwrap();
        let probe = scratch("uid");
        fs::metadata(probe).unwrap().uid()
    }

    fn root(path: &str, tree: u64) -> RootInfo {
        RootInfo {
            path: path.as_bytes().to_vec(),
            scope: Scope::Path,
            tree,
            ino: 256,
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn creates_private_directories_and_refuses_shared_ones() {
        let base = scratch("private");
        let state = base.join("a/b");
        private_dir(&state, uid()).unwrap();
        assert_eq!(mode(&state), 0o700);
        for readable in [0o770, 0o750, 0o705] {
            fs::set_permissions(&state, fs::Permissions::from_mode(readable)).unwrap();
            assert!(matches!(
                private_dir(&state, uid()),
                Err(StateError::Insecure(_))
            ));
        }
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            private_dir(&state, uid().wrapping_add(1)),
            Err(StateError::Insecure(_))
        ));
        let file = base.join("file");
        fs::write(&file, b"").unwrap();
        assert!(matches!(
            private_dir(&file, uid()),
            Err(StateError::NotDirectory(_))
        ));
    }

    #[test]
    fn publishes_reports_atomically_as_private_files() {
        let store = Store::open(&scratch("publish").join("state"), uid()).unwrap();
        let header = header(vec![root("/v/s", 257)]);
        let tree = ScopeTree::new();
        let (path, size) = store
            .publish(&header, |out| {
                report::testing::write_footprint(out, &header, &tree)
            })
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        let listing = store.reports(&header.fsid).unwrap();
        assert_eq!(listing.reports, [(path.clone(), header.clone())]);
        assert_eq!(mode(&crate::report_summary::path(&path)), 0o600);
        assert_eq!(mode(&crate::report_summary::index::path(&path)), 0o600);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 3);
    }

    #[test]
    fn a_failed_write_keeps_the_previous_report() {
        let store = Store::open(&scratch("failed").join("state"), uid()).unwrap();
        let header = header(vec![root("/v/s", 257)]);
        let (path, _) = store
            .publish(&header, |out| out.write_all(b"old").map(|()| 3))
            .unwrap();
        let err = store.publish(&header, |_| Err(io::Error::other("disk full")));
        assert!(err.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn lists_newest_first_and_separates_unreadable_reports() {
        let store = Store::open(&scratch("list").join("state"), uid()).unwrap();
        let mut old = header(vec![root("/v/a", 257)]);
        old.scan_finished = 100;
        let mut new = header(vec![root("/v/b", 258)]);
        new.scan_finished = 200;
        let tree = ScopeTree::new();
        for h in [&old, &new] {
            store
                .publish(h, |out| report::testing::write_footprint(out, h, &tree))
                .unwrap();
        }
        let dir = store.root().join("reports").join(&old.fsid);
        fs::write(dir.join("junk.srrep"), b"junk").unwrap();
        let listing = store.reports(&old.fsid).unwrap();
        let finished: Vec<u64> = listing
            .reports
            .iter()
            .map(|(_, h)| h.scan_finished)
            .collect();
        assert_eq!(finished, [200, 100]);
        assert_eq!(listing.unreadable.len(), 1);
        assert!(store.reports("other").unwrap().reports.is_empty());
    }

    #[test]
    fn scope_keys_ignore_root_order_but_not_identity() {
        let a = [root("/v/a", 257), root("/v/b", 258)];
        let b = [root("/v/b", 258), root("/v/a", 257)];
        assert_eq!(scope_key(&a), scope_key(&b));
        assert_ne!(
            scope_key(&a),
            scope_key(&[root("/v/a", 259), root("/v/b", 258)])
        );
        assert_eq!(scope_key(&a).len(), 16);
    }

    #[test]
    fn a_second_writer_is_busy_until_the_first_releases() {
        let dir = scratch("lock").join("locks");
        let scan = LockHolder::new(Some("j1"), "scan");
        let first = lock_filesystem(&dir, "fs1", uid(), &scan).unwrap();
        assert!(matches!(
            lock_filesystem(&dir, "fs1", uid(), &scan),
            Err(StateError::Busy { ref fsid, .. }) if fsid == "fs1"
        ));
        let other = lock_filesystem(&dir, "fs2", uid(), &scan).unwrap();
        assert_eq!(mode(&first.path), 0o600);
        drop(first);
        lock_filesystem(&dir, "fs1", uid(), &scan).unwrap();
        drop(other);
    }

    #[test]
    fn a_busy_writer_is_told_who_holds_the_lock() {
        let dir = scratch("lock").join("locks");
        let old = lock_filesystem(
            &dir,
            "fs1",
            uid(),
            &LockHolder::new(Some("old-job"), "scan"),
        );
        drop(old);
        let apply = LockHolder::new(None, "cleanup-apply");
        let _held = lock_filesystem(&dir, "fs1", uid(), &apply).unwrap();
        let busy =
            lock_filesystem(&dir, "fs1", uid(), &LockHolder::new(Some("j2"), "scan")).unwrap_err();
        assert!(
            matches!(busy, StateError::Busy { holder: Some(ref h), .. } if *h == apply),
            "{busy:?}"
        );
        assert!(busy.to_string().ends_with("fs1 (cleanup-apply)"), "{busy}");
    }

    #[test]
    fn rootfs_lock_is_nas_wide_and_reports_its_holder() {
        let dir = scratch("rootfs-lock").join("locks");
        let uid = uid();
        let holder = LockHolder::new(Some("rootfs-job"), "rootfs-apply");
        let first = lock_rootfs(&dir, uid, &holder).unwrap();
        let busy = lock_rootfs(&dir, uid, &LockHolder::new(Some("other"), "rootfs-apply"));

        assert!(
            matches!(busy, Err(StateError::RootfsBusy { holder: Some(found) }) if found == holder)
        );
        drop(first);
        assert!(lock_rootfs(&dir, uid, &holder).is_ok());
    }

    fn mark(plan_id: &str) -> CleanupMark {
        CleanupMark {
            plan_id: plan_id.into(),
            execution_log: format!("{plan_id}-1.jsonl"),
            applied_at: "2026-09-29T12:00:00Z".into(),
        }
    }

    #[test]
    fn a_recorded_cleanup_marks_the_reports_it_found_and_no_later_one() {
        let store = Store::open(&scratch("mark").join("state"), uid()).unwrap();
        let tree = ScopeTree::new();
        let mut before = header(vec![root("/v/a", 257)]);
        before.job_id = "before".into();
        store
            .publish(&before, |out| {
                report::testing::write_footprint(out, &before, &tree)
            })
            .unwrap();

        store
            .record_cleanup(&before.fsid, mark("cleanup-1"))
            .unwrap();

        let mut after = header(vec![root("/v/b", 258)]);
        after.job_id = "after".into();
        store
            .publish(&after, |out| {
                report::testing::write_footprint(out, &after, &tree)
            })
            .unwrap();
        let marks = store.cleanup_marks(&before.fsid);
        assert_eq!(marks.of("before"), Marked::By(mark("cleanup-1")));
        assert_eq!(marks.of("after"), Marked::No);
        assert_eq!(store.reports(&before.fsid).unwrap().reports.len(), 2);
        let record = store
            .root()
            .join("reports")
            .join(&before.fsid)
            .join("cleanups.json");
        assert_eq!(mode(&record), 0o600);
    }

    #[test]
    fn an_unreadable_cleanup_record_leaves_every_report_possibly_stale() {
        let store = Store::open(&scratch("mark-junk").join("state"), uid()).unwrap();
        let header = header(vec![root("/v/a", 257)]);
        let dir = store.root().join("reports").join(&header.fsid);
        private_dir(&store.root().join("reports"), uid()).unwrap();
        private_dir(&dir, uid()).unwrap();
        fs::write(dir.join("cleanups.json"), b"junk").unwrap();
        fs::set_permissions(dir.join("cleanups.json"), fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(store.cleanup_marks(&header.fsid).of("any"), Marked::Unknown);
        assert_eq!(store.cleanup_marks("other").of("any"), Marked::No);
    }

    #[test]
    fn a_cleanup_record_forgets_cleanups_whose_reports_are_gone() {
        let store = Store::open(&scratch("mark-prune").join("state"), uid()).unwrap();
        let tree = ScopeTree::new();
        let mut first = header(vec![root("/v/a", 257)]);
        first.job_id = "first".into();
        store
            .publish(&first, |out| {
                report::testing::write_footprint(out, &first, &tree)
            })
            .unwrap();
        store
            .record_cleanup(&first.fsid, mark("cleanup-1"))
            .unwrap();
        let mut second = first.clone();
        second.job_id = "second".into();
        store
            .publish(&second, |out| {
                report::testing::write_footprint(out, &second, &tree)
            })
            .unwrap();

        store
            .record_cleanup(&first.fsid, mark("cleanup-2"))
            .unwrap();

        let CleanupMarks::Read(records) = store.cleanup_marks(&first.fsid) else {
            panic!("record unreadable");
        };
        let plans: Vec<&str> = records.iter().map(|r| r.mark.plan_id.as_str()).collect();
        assert_eq!(plans, ["cleanup-2"]);
        assert_eq!(records[0].reports, ["second"]);
    }

    #[test]
    fn recording_the_same_cleanup_again_keeps_one_record() {
        let store = Store::open(&scratch("mark-again").join("state"), uid()).unwrap();
        let header = header(vec![root("/v/a", 257)]);
        let tree = ScopeTree::new();
        store
            .publish(&header, |out| {
                report::testing::write_footprint(out, &header, &tree)
            })
            .unwrap();

        store
            .record_cleanup(&header.fsid, mark("cleanup-1"))
            .unwrap();
        store
            .record_cleanup(&header.fsid, mark("cleanup-1"))
            .unwrap();

        let CleanupMarks::Read(records) = store.cleanup_marks(&header.fsid) else {
            panic!("record unreadable");
        };
        assert_eq!(records.len(), 1);
    }
}
