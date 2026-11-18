//! `rootfs apply`: moves each planned item off `/` into the plan's move
//! folder. A file is copied, synced and verified (size and SHA-256)
//! before its original is unlinked, and only while the original is still
//! the file planned (dev, inode, type, size, mtime), reached from the
//! root without following a symlink.

use crate::btrfs::walk::{BatchBoundary, StopReason};
use crate::guard::{self, CleanupConfig, Refusal};
use crate::output::JsonPath;
use crate::output::escape_human;
use crate::progress::RootfsProgress;
use crate::rootfs::classify::{Category, Kind};
use crate::rootfs::delegate::{Delegate, ToolOutcome, Tools};
use crate::rootfs::plan::{DESTINATION_MARGIN_BYTES, RootfsPlan, check_space};
use crate::rootfs::walk::{Candidate, Identity};
use crate::sha256::Sha256;
use crate::sys;
use serde::Serialize;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::ops::ControlFlow;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

const COPY_CHUNK: usize = 1 << 20;
/// The folder in the move folder where each copy is written before it
/// takes its name; no allowlisted path on `/` is named so.
const STAGING: &str = ".partial";
/// The one copy in `STAGING`.
const STAGED: &str = "copy";
const DIR_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Why an item stayed on `/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    /// Gone since the plan.
    Missing,
    /// Another file, or changed (size or mtime), since the plan.
    Changed,
    /// A folder on its path is now a symlink or not a folder.
    PathChanged,
    /// A process holds it open.
    HeldOpen,
    /// It changed while it was copied; the copy was removed.
    ChangedDuringCopy,
    /// The copy read back differs; the copy was removed.
    CopyMismatch,
    /// Another file already has its name in the move folder; both were left.
    CopyExists,
    /// The move folder's filesystem has no room for it; apply stopped.
    NoRoom,
    /// A folder that still holds entries.
    NotEmpty,
    /// Its verified copy is in the move folder (the manifest's moved line
    /// before this one), but removing the original failed: both are kept.
    UnlinkFailed,
}

/// A file moved off `/`: its copy has this size and digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub size: u64,
    pub mtime: i64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Moved(Moved),
    RemovedDir,
    Skipped(Skip),
}

/// Whether a process held the file `(st_dev, st_ino)` open when apply
/// started.
pub type Held = Box<dyn Fn(u64, u64) -> bool>;

/// How many files, and how many of their bytes, one batch moves: its
/// copies are synced, and its moves logged, together before its unlinks.
#[derive(Debug, Clone, Copy)]
pub struct BatchLimits {
    pub files: usize,
    pub bytes: u64,
}

impl BatchLimits {
    pub const APPLY: BatchLimits = BatchLimits {
        files: 64,
        bytes: 64 << 20,
    };
}

/// How apply makes a batch of copies durable: the move folder's whole
/// filesystem once, then each copy's data, which reports a write error
/// the first did not; a stand-in in tests.
#[derive(Clone, Copy)]
pub struct Syncs {
    pub filesystem: fn(BorrowedFd<'_>) -> io::Result<()>,
    pub data: fn(&File) -> io::Result<()>,
}

impl Syncs {
    pub const KERNEL: Syncs = Syncs {
        filesystem: sys::syncfs,
        data: File::sync_data,
    };
}

/// Gives a name in one folder a name in another, failing with EEXIST
/// rather than replacing a file that has it.
type Rename = fn(BorrowedFd<'_>, &OsStr, BorrowedFd<'_>, &OsStr) -> io::Result<()>;

/// How a verified staged copy takes its final name without replacing
/// another file: a no-replace rename, else a hard link; stand-ins in tests.
#[derive(Clone, Copy)]
struct Renames {
    noreplace: Rename,
    link: Rename,
}

impl Renames {
    const KERNEL: Renames = Renames {
        noreplace: sys::rename_noreplace,
        link: sys::link_at,
    };
}

/// A planned file whose verified copy has its final name in the move
/// folder, while its original is still on `/`: the open original and its
/// folders.
struct Copied<'a> {
    item: &'a Candidate,
    name: &'a OsStr,
    parent: File,
    copy_dir: File,
    /// The copy at its final name, open to sync its data.
    copy: File,
    moved: Moved,
}

/// Moves items from below `root` (the planned `/`) to the same paths
/// below `target` (the plan's move folder).
pub struct Mover {
    pub root: File,
    pub target: File,
    /// The files other processes held open at the start, from one
    /// `/proc` snapshot; a file opened later is not seen.
    pub held: Held,
    pub syncs: Syncs,
    pub batch: BatchLimits,
    /// Bytes to leave available on the move folder's filesystem.
    pub reserve: u64,
}

impl Mover {
    /// Moves one planned regular file, calling `before_unlink` with the
    /// verified copy before the last check of its original, which is then
    /// removed at once (a skip at that check comes after the moved line
    /// in the manifest and overrides it); `Err` only for a
    /// failure that should stop apply (the destination or `before_unlink`
    /// cannot be written).
    pub fn move_file(
        &self,
        item: &Candidate,
        before_unlink: &mut dyn FnMut(&Moved) -> io::Result<()>,
    ) -> io::Result<Outcome> {
        let copied = match self.copy_file(item)? {
            Ok(copied) => copied,
            Err(skip) => return Ok(Outcome::Skipped(skip)),
        };
        self.sync_copies(std::slice::from_ref(&copied))?;
        before_unlink(&copied.moved)?;
        self.finish(copied)
    }

    /// Removes the staging folder once a batch has left nothing in it, so
    /// a finished or paused run leaves only the moved files, the manifest
    /// and the README in the move folder; the next copy makes it again.
    /// A folder that still holds something stays.
    fn remove_staging(&self) {
        let _ = sys::unlinkat(self.target.as_fd(), OsStr::new(STAGING), true);
    }

    /// Makes `copied` durable: their filesystem written out once, then
    /// each copy's data synced. A copy whose sync fails is removed, so a
    /// rerun cannot take it for a good one, and the run stops.
    fn sync_copies(&self, copied: &[Copied<'_>]) -> io::Result<()> {
        if copied.is_empty() {
            return Ok(());
        }
        (self.syncs.filesystem)(self.target.as_fd())?;
        for copy in copied {
            if let Err(e) = (self.syncs.data)(&copy.copy) {
                let _ = sys::unlinkat(copy.copy_dir.as_fd(), copy.name, false);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Copies one planned regular file into the move folder and verifies
    /// the copy under its final name, the original still on `/` and
    /// watched until `finish`; or why it stays.
    fn copy_file<'a>(&self, item: &'a Candidate) -> io::Result<Result<Copied<'a>, Skip>> {
        let (parents, name) = split(&item.path)?;
        let parent = match open_below(&self.root, &parents) {
            Ok(parent) => parent,
            Err(e) => return Ok(Err(path_skip(&e))),
        };
        if let Some(skip) = self.check_source(&parent, name, &item.identity) {
            return Ok(Err(skip));
        }
        if (self.held)(item.identity.device, item.identity.inode) {
            return Ok(Err(Skip::HeldOpen));
        }
        let (source, opened) = match open_source(&parent, name) {
            Ok(opened) => opened,
            Err(skip) => return Ok(Err(skip)),
        };
        if (opened.dev(), opened.ino()) != (item.identity.device, item.identity.inode) {
            return Ok(Err(Skip::Changed));
        }
        if item.category == Category::PreferenceImage {
            use std::os::unix::fs::FileExt;
            let mut header = [0; 12];
            let n = source.read_at(&mut header, 0)?;
            if !super::classify::image_header(&header[..n]) {
                return Ok(Err(Skip::Changed));
            }
        }
        let copy_dir = create_below(&self.target, &parents)?;
        let (digest, copy) = match sys::statat(copy_dir.as_fd(), name) {
            // An earlier run copied it and stopped before removing it.
            Ok(_) => {
                let (_, digest) = digest_of(&source)?;
                let copy = open_copy(&copy_dir, name)?;
                if digest_of(&copy)? != (item.identity.size, digest.clone()) {
                    return Ok(Err(Skip::CopyExists));
                }
                (digest, copy)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !self.has_room(item.identity.size.max(item.allocated_bytes))? {
                    return Ok(Err(Skip::NoRoom));
                }
                let staging = create_below(&self.target, &[OsStr::new(STAGING)])?;
                let (digest, copy) = match stage(&source, &staging) {
                    Err(e) if no_room(&e) => return Ok(Err(Skip::NoRoom)),
                    result => result?,
                };
                let after = identity_of(&source.metadata()?);
                let staged = verify(&staging, OsStr::new(STAGED))?;
                if !is_staged(&staging, &copy)? {
                    return Ok(Err(Skip::CopyMismatch));
                }
                if let Some(skip) = settle(&item.identity, &after, &digest, staged) {
                    sys::unlinkat(staging.as_fd(), OsStr::new(STAGED), false)?;
                    return Ok(Err(skip));
                }
                place(&staging, &copy_dir, name, Renames::KERNEL)?;
                (digest, copy)
            }
            Err(e) => return Err(e),
        };
        Ok(Ok(Copied {
            item,
            name,
            parent,
            copy_dir,
            copy,
            moved: Moved {
                size: item.identity.size,
                mtime: item.identity.mtime,
                sha256: digest,
            },
        }))
    }

    /// Removes the original of `copied` if it is still the planned file;
    /// otherwise removes the copy and says why the original stays.
    fn finish(&self, copied: Copied<'_>) -> io::Result<Outcome> {
        let Copied {
            item,
            name,
            parent,
            copy_dir,
            moved,
            ..
        } = copied;
        // The last check sits right before the unlink; a skip here follows
        // the moved line in the manifest, which the later line overrides.
        if let Some(skip) = self.check_source(&parent, name, &item.identity) {
            sys::unlinkat(copy_dir.as_fd(), name, false)?;
            return Ok(Outcome::Skipped(skip));
        }
        if let Some(skip) = remove_original(&parent, name) {
            return Ok(Outcome::Skipped(skip));
        }
        Ok(Outcome::Moved(moved))
    }

    /// Removes one planned folder if it is still the planned folder (dev,
    /// inode, type; its size and mtime change as it empties) and empty.
    pub fn remove_dir(&self, item: &Candidate) -> io::Result<Outcome> {
        let (parents, name) = split(&item.path)?;
        let parent = match open_below(&self.root, &parents) {
            Ok(parent) => parent,
            Err(e) => return Ok(Outcome::Skipped(path_skip(&e))),
        };
        let stat = match sys::statat(parent.as_fd(), name) {
            Ok(stat) => stat,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Outcome::Skipped(Skip::Missing));
            }
            Err(_) => return Ok(Outcome::Skipped(Skip::Changed)),
        };
        let planned = &item.identity;
        if (stat.device, stat.inode, stat.mode & libc::S_IFMT)
            != (planned.device, planned.inode, planned.file_type)
        {
            return Ok(Outcome::Skipped(Skip::Changed));
        }
        match sys::unlinkat(parent.as_fd(), name, true) {
            Ok(()) => Ok(Outcome::RemovedDir),
            Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => {
                Ok(Outcome::Skipped(Skip::NotEmpty))
            }
            Err(e) => Err(e),
        }
    }

    /// Whether the move folder's filesystem has `bytes` available beyond
    /// the reserve.
    fn has_room(&self, bytes: u64) -> io::Result<bool> {
        let available = sys::filesystem_space(self.target.as_fd())?.available;
        Ok(available >= bytes.saturating_add(self.reserve))
    }

    /// Why the entry `name` in `parent` is not the planned file, if it is not.
    fn check_source(&self, parent: &File, name: &OsStr, planned: &Identity) -> Option<Skip> {
        let stat = match sys::statat(parent.as_fd(), name) {
            Ok(stat) => stat,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(Skip::Missing),
            Err(_) => return Some(Skip::Changed),
        };
        (Identity::of(&stat) != *planned).then_some(Skip::Changed)
    }
}

/// What must hold before apply touches anything: root, `--yes`, the
/// kill switch off in the root-owned configuration (`None` only when
/// there is none; an untrusted one has already refused), and a
/// still-valid plan at `now`.
pub fn gate(
    plan: &RootfsPlan,
    now: u64,
    config: Option<&CleanupConfig>,
    yes: bool,
    uid: u32,
) -> Result<(), Refusal> {
    if uid != 0 {
        return Err(Refusal::new(
            "not_root",
            "rootfs apply requires root privileges",
        ));
    }
    if !yes {
        return Err(Refusal::new(
            "yes_required",
            "rootfs apply moves files off / and removes the originals; pass --yes to confirm",
        ));
    }
    if config.is_some_and(|config| config.mutations_disabled) {
        return Err(Refusal::new(
            "mutations_disabled",
            "every cleanup mutation is off: an administrator turns them back on with \
             synoreclaim config set --mutations-disabled off",
        ));
    }
    plan.validate(now)
}

/// Opens (creating it 0700 if new) the plan's move folder `name` in the
/// destination, and its `manifest.jsonl` for appending: a rerun of an
/// interrupted plan resumes in the same folder. Refuses a symlink or a
/// folder of another user, and (`apply_running`) a manifest another apply
/// holds locked: two runs of one plan would share its `.partial` copy.
pub fn open_move_dir(destination: &File, name: &OsStr) -> Result<(File, SyncedLog), ExecuteError> {
    let dir = create_below(destination, &[name])?;
    let manifest = sys::openat(
        dir.as_fd(),
        OsStr::new("manifest.jsonl"),
        libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    )?;
    sys::set_mode(manifest.as_fd(), 0o600)?;
    require_private(&manifest, "the manifest")?;
    let Ok(manifest) = sys::ExclusiveLock::try_take(manifest)? else {
        return Err(Refusal::new(
            "apply_running",
            "another rootfs apply of this plan is running; nothing was changed",
        )
        .into());
    };
    dir.sync_all()?;
    Ok((dir, SyncedLog(manifest)))
}

#[derive(Serialize)]
struct PlanMarker<'a> {
    event: &'static str,
    plan_id: &'a str,
}

/// Records the plan id before any originals are removed. A minute-named
/// folder can collide with another plan, so never append to another plan's
/// manifest.
fn ensure_manifest_plan(
    dir: &File,
    manifest: &mut SyncedLog,
    plan_id: &str,
) -> Result<(), ExecuteError> {
    let file = sys::openat(
        dir.as_fd(),
        OsStr::new("manifest.jsonl"),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )?;
    require_private(&file, "the manifest")?;
    let mut first_line = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut first_line)?;
    if first_line.is_empty() {
        let marker = PlanMarker {
            event: "plan",
            plan_id,
        };
        serde_json::to_writer(&mut *manifest, &marker).map_err(io::Error::other)?;
        manifest.write_all(b"\n")?;
        manifest.flush()?;
        return Ok(());
    }

    let marker: serde_json::Value =
        serde_json::from_slice(&first_line).map_err(io::Error::other)?;
    if marker.get("event").and_then(serde_json::Value::as_str) != Some("plan")
        || marker.get("plan_id").and_then(serde_json::Value::as_str) != Some(plan_id)
    {
        return Err(Refusal::new(
            "destination_changed",
            "the timestamped move folder already belongs to another plan; no items were moved",
        )
        .into());
    }
    Ok(())
}

const README_TITLE: &[u8] = b"Syno Reclaim planned these original paths on the system partition:\n";

/// Adds a private README listing the planned source paths. On a rerun,
/// verify the same list instead of replacing a file in the move folder.
fn write_readme(dir: &File, items: &[Candidate]) -> io::Result<()> {
    let readme = match sys::openat(
        dir.as_fd(),
        OsStr::new("README.txt"),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return validate_readme(dir, items);
        }
        Err(error) => return Err(error),
    };
    sys::set_mode(readme.as_fd(), 0o600)?;
    require_private(&readme, "README.txt")?;
    let mut readme = readme;
    write_readme_contents(&mut readme, items)?;
    readme.sync_all()?;
    dir.sync_all()?;
    Ok(())
}

fn write_readme_contents(output: &mut impl Write, items: &[Candidate]) -> io::Result<()> {
    output.write_all(README_TITLE)?;
    output.write_all(b"\n")?;
    for item in items {
        output.write_all(b"- ")?;
        output.write_all(escape_human(&item.path).as_bytes())?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

fn validate_readme(dir: &File, items: &[Candidate]) -> io::Result<()> {
    let file = sys::openat(
        dir.as_fd(),
        OsStr::new("README.txt"),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )?;
    require_private(&file, "README.txt")?;
    let mut input = BufReader::new(file);
    if !readme_line_matches(&mut input, README_TITLE)? || !readme_line_matches(&mut input, b"\n")? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "README.txt does not list this plan's original paths",
        ));
    }
    for item in items {
        let mut expected = b"- ".to_vec();
        expected.extend_from_slice(escape_human(&item.path).as_bytes());
        expected.push(b'\n');
        if !readme_line_matches(&mut input, &expected)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "README.txt does not list this plan's original paths",
            ));
        }
    }
    let mut extra = [0u8; 1];
    if input.read(&mut extra)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "README.txt contains paths outside this plan",
        ));
    }
    Ok(())
}

fn readme_line_matches(input: &mut impl BufRead, expected: &[u8]) -> io::Result<bool> {
    let mut line = Vec::new();
    input.read_until(b'\n', &mut line)?;
    Ok(line == expected)
}

/// Why `execute` did not run: a refusal (nothing was touched) or an
/// I/O failure on the way.
#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("apply stopped after partial work: {source}")]
    Partial { source: io::Error, done: Done },
}

impl ExecuteError {
    pub fn code(&self) -> &'static str {
        match self {
            ExecuteError::Refused(refusal) => refusal.code,
            ExecuteError::Io(_) | ExecuteError::Partial { .. } => "io_error",
        }
    }
}

/// Carries out a gated plan: `root` must still be the planned `/`
/// device and `destination` the planned folder with room for the moves;
/// then the items move into the plan's move folder (resumed if it
/// exists), each logged and synced, skipping files `held` open when
/// apply started.
pub fn execute(
    plan: &RootfsPlan,
    root: &File,
    destination: &File,
    held: Held,
    tools: &mut dyn Tools,
    boundary: &mut dyn BatchBoundary,
) -> Result<Done, ExecuteError> {
    execute_with_progress(plan, root, destination, held, tools, boundary, &mut |_| {})
}

/// Carries out a gated plan and reports durable move/tool boundaries. The
/// callback is observational: its failure cannot interrupt the apply.
pub fn execute_with_progress(
    plan: &RootfsPlan,
    root: &File,
    destination: &File,
    held: Held,
    tools: &mut dyn Tools,
    boundary: &mut dyn BatchBoundary,
    progress: &mut dyn FnMut(RootfsProgress),
) -> Result<Done, ExecuteError> {
    let payload = &plan.payload;
    if root.metadata()?.dev() != payload.root_device {
        return Err(Refusal::new(
            "root_changed",
            "/ is another filesystem than when the plan was made; make a new plan",
        )
        .into());
    }
    let here = destination.metadata()?;
    if (here.dev(), here.ino()) != (payload.destination.device, payload.destination.inode) {
        return Err(Refusal::new(
            "destination_changed",
            format!(
                "{} is not the folder the plan was made for; make a new plan",
                escape_human(&payload.destination.path)
            ),
        )
        .into());
    }
    check_space(
        remaining_bytes(root, &payload.items),
        sys::filesystem_space(destination.as_fd())?.available,
    )?;
    let (target, mut manifest) = open_move_dir(destination, OsStr::new(&plan.move_dir_name()))?;
    ensure_manifest_plan(&target, &mut manifest, &payload.plan_id)?;
    write_readme(&target, &payload.items)?;
    let mover = Mover {
        root: root.try_clone()?,
        target,
        held,
        syncs: Syncs::KERNEL,
        batch: BatchLimits::APPLY,
        reserve: DESTINATION_MARGIN_BYTES,
    };
    let mut done = match run_with_progress_with_done(
        &mover,
        &payload.items,
        &mut manifest,
        boundary,
        progress,
    ) {
        Ok(done) => done,
        Err((source, done)) => return Err(ExecuteError::Partial { source, done }),
    };
    if done.stopped.is_none() {
        if let Err(source) = run_tools_with_progress(
            &payload.delegates,
            tools,
            &mut manifest,
            boundary,
            &mut done,
            progress,
        ) {
            progress(RootfsProgress {
                phase: "tools".into(),
                items_done: done.tools.len() as u64,
                items_total: payload.delegates.len() as u64,
                ..RootfsProgress::default()
            });
            return Err(ExecuteError::Partial { source, done });
        }
    }
    Ok(done)
}

/// What the files of `items` still on `/` as planned need in the move
/// folder: an interrupted run already moved the others.
fn remaining_bytes(root: &File, items: &[Candidate]) -> u64 {
    items
        .iter()
        .filter(|item| item.kind == Kind::File && still_planned(root, item))
        .map(|item| item.identity.size.max(item.allocated_bytes))
        .fold(0, u64::saturating_add)
}

/// Whether the planned file `item` is still at its path below `root`.
fn still_planned(root: &File, item: &Candidate) -> bool {
    let Ok((parents, name)) = split(&item.path) else {
        return false;
    };
    open_below(root, &parents)
        .and_then(|parent| sys::statat(parent.as_fd(), name))
        .is_ok_and(|stat| Identity::of(&stat) == item.identity)
}

/// A log file whose every flush reaches the disk.
#[derive(Debug)]
pub struct SyncedLog(sys::ExclusiveLock);

impl Write for SyncedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.file().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.file().sync_data()
    }
}

/// What a run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub moved_files: u64,
    pub moved_categories: Box<std::collections::BTreeSet<Category>>,
    /// Apparent bytes of the moved files.
    pub moved_size: u64,
    /// Allocated bytes of the moved files and removed folders, as planned.
    pub moved_allocated_bytes: u64,
    pub removed_dirs: u64,
    pub skipped: u64,
    pub skips: Vec<(Vec<u8>, Skip)>,
    pub stopped: Option<StopReason>,
    /// The planned DSM tools run so far, in order.
    pub tools: Vec<(Delegate, ToolOutcome)>,
}

/// One `manifest.jsonl` line.
#[derive(Serialize)]
struct Record<'a> {
    #[serde(flatten)]
    path: JsonPath,
    kind: Kind,
    category: Category,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<Skip>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    moved: Option<&'a Moved>,
}

/// One `manifest.jsonl` line for a DSM tool run.
#[derive(Serialize)]
struct ToolRecord {
    tool: Delegate,
    argv: Vec<&'static str>,
    #[serde(flatten)]
    outcome: ToolOutcome,
}

/// Runs the `planned` tools in order, logging each to `manifest`;
/// `boundary` runs before every tool and may stop the rest.
pub fn run_tools(
    planned: &[Delegate],
    tools: &mut dyn Tools,
    manifest: &mut dyn Write,
    boundary: &mut dyn BatchBoundary,
    done: &mut Done,
) -> io::Result<()> {
    run_tools_with_progress(planned, tools, manifest, boundary, done, &mut |_| {})
}

/// Runs planned tools and reports the active tool and completed tools.
pub fn run_tools_with_progress(
    planned: &[Delegate],
    tools: &mut dyn Tools,
    manifest: &mut dyn Write,
    boundary: &mut dyn BatchBoundary,
    done: &mut Done,
    progress: &mut dyn FnMut(RootfsProgress),
) -> io::Result<()> {
    progress(RootfsProgress {
        phase: "tools".into(),
        items_total: planned.len() as u64,
        ..RootfsProgress::default()
    });
    for &tool in planned {
        if let ControlFlow::Break(reason) = boundary.after_batch() {
            done.stopped = Some(reason);
            break;
        }
        progress(RootfsProgress {
            phase: "tools".into(),
            items_done: done.tools.len() as u64,
            items_total: planned.len() as u64,
            tool: Some(tool),
            ..RootfsProgress::default()
        });
        let outcome = tools.run(tool);
        let record = ToolRecord {
            tool,
            argv: tool.argv(),
            outcome,
        };
        done.tools.push((tool, outcome));
        serde_json::to_writer(&mut *manifest, &record)?;
        manifest.write_all(b"\n")?;
        manifest.flush()?;
        progress(RootfsProgress {
            phase: "tools".into(),
            items_done: done.tools.len() as u64,
            items_total: planned.len() as u64,
            ..RootfsProgress::default()
        });
        if let ControlFlow::Break(reason) = boundary.after_batch() {
            done.stopped = Some(reason);
            break;
        }
    }
    Ok(())
}

/// Moves the files of `items` in order, batch by batch, then removes its
/// folders deepest first, logging each to `manifest`. A batch's moved lines
/// are flushed together after its copies are synced and before any of its
/// originals is removed. `boundary` runs after every batch and folder and
/// may stop the run there.
pub fn run(
    mover: &Mover,
    items: &[Candidate],
    manifest: &mut dyn Write,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<Done> {
    run_with_progress(mover, items, manifest, boundary, &mut |_| {})
}

/// Moves planned entries and reports progress after each completed batch
/// or directory boundary.
pub fn run_with_progress(
    mover: &Mover,
    items: &[Candidate],
    manifest: &mut dyn Write,
    boundary: &mut dyn BatchBoundary,
    progress: &mut dyn FnMut(RootfsProgress),
) -> io::Result<Done> {
    run_with_progress_with_done(mover, items, manifest, boundary, progress)
        .map_err(|(error, _)| error)
}

/// As `run_with_progress`, retaining completed work when an I/O error
/// stops the next batch or directory boundary.
fn run_with_progress_with_done(
    mover: &Mover,
    items: &[Candidate],
    manifest: &mut dyn Write,
    boundary: &mut dyn BatchBoundary,
    progress: &mut dyn FnMut(RootfsProgress),
) -> Result<Done, (io::Error, Done)> {
    let files: Vec<&Candidate> = items
        .iter()
        .filter(|item| item.kind == Kind::File)
        .collect();
    let mut dirs: Vec<&Candidate> = items.iter().filter(|item| item.kind == Kind::Dir).collect();
    dirs.sort_by(|a, b| b.path.cmp(&a.path));
    let mut done = Done::default();
    let items_total = items.len() as u64;
    let bytes_total = files
        .iter()
        .map(|item| item.identity.size)
        .fold(0, u64::saturating_add);
    progress(move_progress(&done, items_total, bytes_total));
    for batch in batches(&files, mover.batch) {
        let full = match move_batch(mover, batch, manifest, &mut done) {
            Ok(full) => full,
            Err(error) => {
                progress(move_progress(&done, items_total, bytes_total));
                return Err((error, done));
            }
        };
        progress(move_progress(&done, items_total, bytes_total));
        if full {
            done.stopped = Some(StopReason::DiskLimit);
            return Ok(done);
        }
        if let ControlFlow::Break(reason) = boundary.after_batch() {
            done.stopped = Some(reason);
            return Ok(done);
        }
    }
    for item in dirs {
        let outcome = match mover.remove_dir(item) {
            Ok(outcome) => outcome,
            Err(error) => {
                progress(move_progress(&done, items_total, bytes_total));
                return Err((error, done));
            }
        };
        if let Err(error) = log(manifest, item, &outcome) {
            progress(move_progress(&done, items_total, bytes_total));
            return Err((error, done));
        }
        if let Err(error) = manifest.flush() {
            progress(move_progress(&done, items_total, bytes_total));
            return Err((error, done));
        }
        done.count(item, outcome);
        progress(move_progress(&done, items_total, bytes_total));
        if let ControlFlow::Break(reason) = boundary.after_batch() {
            done.stopped = Some(reason);
            break;
        }
    }
    Ok(done)
}

fn move_progress(done: &Done, items_total: u64, bytes_total: u64) -> RootfsProgress {
    RootfsProgress {
        phase: "moves".into(),
        items_done: done
            .moved_files
            .saturating_add(done.removed_dirs)
            .saturating_add(done.skipped),
        items_total,
        bytes_done: done.moved_size,
        bytes_total,
        tool: None,
    }
}

/// Moves one batch of files: copies each, syncs the copies' folders once,
/// flushes the moved lines once, then removes the originals still as
/// planned (a later line overrides a moved one whose original stays).
/// True when the move folder had no room, which stops the run.
fn move_batch(
    mover: &Mover,
    batch: &[&Candidate],
    manifest: &mut dyn Write,
    done: &mut Done,
) -> io::Result<bool> {
    let mut copied = Vec::new();
    let mut full = false;
    for item in batch {
        match mover.copy_file(item)? {
            Ok(copy) => copied.push(copy),
            Err(skip) => {
                log(manifest, item, &Outcome::Skipped(skip))?;
                done.count(item, Outcome::Skipped(skip));
                if skip == Skip::NoRoom {
                    full = true;
                    break;
                }
            }
        }
    }
    mover.sync_copies(&copied)?;
    for copy in &copied {
        log(manifest, copy.item, &Outcome::Moved(copy.moved.clone()))?;
    }
    manifest.flush()?;
    let mut overridden = false;
    for copy in copied {
        let item = copy.item;
        let outcome = mover.finish(copy)?;
        if !matches!(outcome, Outcome::Moved(_)) {
            log(manifest, item, &outcome)?;
            overridden = true;
        }
        done.count(item, outcome);
    }
    if overridden {
        manifest.flush()?;
    }
    mover.remove_staging();
    Ok(full)
}

/// `files` in order, cut into batches of at most `limits.files` files and,
/// unless a single file is larger, `limits.bytes` bytes.
fn batches<'a, 'b>(files: &'b [&'a Candidate], limits: BatchLimits) -> Vec<&'b [&'a Candidate]> {
    let mut cut = Vec::new();
    let (mut start, mut bytes) = (0, 0u64);
    for (i, item) in files.iter().enumerate() {
        let size = item.identity.size.max(item.allocated_bytes);
        if i > start && (i - start == limits.files || bytes.saturating_add(size) > limits.bytes) {
            cut.push(&files[start..i]);
            (start, bytes) = (i, 0);
        }
        bytes = bytes.saturating_add(size);
    }
    if start < files.len() {
        cut.push(&files[start..]);
    }
    cut
}

impl Done {
    fn count(&mut self, item: &Candidate, outcome: Outcome) {
        match outcome {
            Outcome::Moved(moved) => {
                self.moved_categories.insert(item.category);
                self.moved_files += 1;
                self.moved_size = self.moved_size.saturating_add(moved.size);
                self.moved_allocated_bytes = self
                    .moved_allocated_bytes
                    .saturating_add(item.allocated_bytes);
            }
            Outcome::RemovedDir => {
                self.removed_dirs += 1;
                self.moved_allocated_bytes = self
                    .moved_allocated_bytes
                    .saturating_add(item.allocated_bytes);
            }
            Outcome::Skipped(skip) => {
                self.skipped += 1;
                self.skips.push((item.path.clone(), skip));
            }
        }
    }
}

/// Writes one manifest line; the caller flushes.
fn log(manifest: &mut dyn Write, item: &Candidate, outcome: &Outcome) -> io::Result<()> {
    let (label, reason, moved) = match outcome {
        Outcome::Moved(moved) => ("moved", None, Some(moved)),
        Outcome::RemovedDir => ("removed", None, None),
        Outcome::Skipped(skip) => ("skipped", Some(*skip), None),
    };
    let record = Record {
        path: JsonPath::new(&item.path),
        kind: item.kind,
        category: item.category,
        outcome: label,
        reason,
        moved,
    };
    serde_json::to_writer(&mut *manifest, &record)?;
    manifest.write_all(b"\n")
}

/// `path` (absolute, as planned) as its folder names and final name.
fn split(path: &[u8]) -> io::Result<(Vec<&OsStr>, &OsStr)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "not a plain absolute path");
    let relative = path.strip_prefix(b"/").ok_or_else(invalid)?;
    let mut parts: Vec<&OsStr> = relative
        .split(|b| *b == b'/')
        .map(OsStr::from_bytes)
        .collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || p.as_bytes() == b"." || p.as_bytes() == b"..")
    {
        return Err(invalid());
    }
    let name = parts.pop().ok_or_else(invalid)?;
    Ok((parts, name))
}

/// Opens the folder `parts` below `dir`, never through a symlink.
fn open_below(dir: &File, parts: &[&OsStr]) -> io::Result<File> {
    let mut current = dir.try_clone()?;
    for part in parts {
        current = sys::openat(current.as_fd(), part, DIR_FLAGS, 0)?;
    }
    Ok(current)
}

/// Whether `error` says the filesystem is full or its quota reached.
fn no_room(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT))
}

/// A symlink or non-folder on the path: the path changed. Missing: gone.
/// Opens the checked source `name` in `parent` without following a
/// symlink or its atime; one swapped or removed since the check is the
/// item's skip, not the run's end.
fn open_source(parent: &File, name: &OsStr) -> Result<(File, std::fs::Metadata), Skip> {
    let source = sys::openat_noatime(
        parent.as_fd(),
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )
    .map_err(|e| path_skip(&e))?;
    let meta = source.metadata().map_err(|e| path_skip(&e))?;
    Ok((source, meta))
}

fn path_skip(error: &io::Error) -> Skip {
    match error.kind() {
        io::ErrorKind::NotFound => Skip::Missing,
        _ => Skip::PathChanged,
    }
}

/// Opens the folder `parts` below `dir`, creating what is missing as
/// 0700 folders of this user, each synced with its parent; refuses a symlink, and a folder that is not
/// private to this user (its mode or a DSM ACL lets others in).
fn create_below(dir: &File, parts: &[&OsStr]) -> io::Result<File> {
    let mut current = dir.try_clone()?;
    for part in parts {
        let created = match sys::mkdirat(current.as_fd(), part, 0o700) {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e),
        };
        let parent = current;
        current = sys::openat(parent.as_fd(), part, DIR_FLAGS, 0)?;
        if created {
            // DSM gives a new folder the ACL it inherits from the share,
            // whatever mode mkdirat asked for; setting the mode drops it.
            sys::set_mode(current.as_fd(), 0o700)?;
            current.sync_all()?;
            parent.sync_all()?;
        }
        require_private(&current, "a folder in the move folder")?;
    }
    Ok(current)
}

/// Refuses `file` unless only this user may reach it.
fn require_private(file: &File, what: &str) -> io::Result<()> {
    let meta = file.metadata()?;
    if !guard::private_to_cleanup(meta.uid(), meta.mode(), guard::dsm_acl(file)?) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{what} is not private to this user"),
        ));
    }
    Ok(())
}

/// Copies `source` to a new 0600 file `name` in `dir`, its data synced
/// (its folder is synced once per batch), leaving its holes holes;
/// returns the SHA-256 of its content and the copy's (device, inode).
fn copy(source: &File, dir: &File, name: &OsStr) -> io::Result<(String, File)> {
    let copy = sys::openat(
        dir.as_fd(),
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    )?;
    sys::set_mode(copy.as_fd(), 0o600)?;
    require_private(&copy, "a copy in the move folder")?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut offset = 0;
    while let Some(data) = sys::seek_data(source.as_fd(), offset)? {
        hash_zeros(&mut hasher, data - offset);
        let end = sys::seek_hole(source.as_fd(), data)?;
        offset = copy_range(source, &copy, data, end, &mut buffer, &mut hasher)?;
    }
    let size = source.metadata()?.len().max(offset);
    hash_zeros(&mut hasher, size - offset);
    copy.set_len(size)?;
    Ok((hasher.finish_hex(), copy))
}

/// Copies the bytes `from..to` of `source` to the same offsets of `copy`,
/// or up to the end of `source` if that comes first; returns the offset
/// reached.
fn copy_range(
    source: &File,
    copy: &File,
    from: u64,
    to: u64,
    buffer: &mut [u8],
    hasher: &mut Sha256,
) -> io::Result<u64> {
    use std::os::unix::fs::FileExt;
    let mut offset = from;
    while offset < to {
        let want = buffer
            .len()
            .min(usize::try_from(to - offset).unwrap_or(usize::MAX));
        let read = source.read_at(&mut buffer[..want], offset)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        copy.write_all_at(&buffer[..read], offset)?;
        offset += read as u64;
    }
    Ok(offset)
}

/// Hashes `count` zero bytes: a hole reads as zeros.
fn hash_zeros(hasher: &mut Sha256, mut count: u64) {
    static ZEROS: [u8; 1 << 16] = [0; 1 << 16];
    while count > 0 {
        let take = count.min(ZEROS.len() as u64) as usize;
        hasher.update(&ZEROS[..take]);
        count -= take as u64;
    }
}

/// Copies `source` into `STAGED` in `staging`, first removing a copy an
/// interrupted run left there; a failed copy is removed.
fn stage(source: &File, staging: &File) -> io::Result<(String, File)> {
    let staged = OsStr::new(STAGED);
    match sys::unlinkat(staging.as_fd(), staged, false) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    copy(source, staging, staged).inspect_err(|_| {
        let _ = sys::unlinkat(staging.as_fd(), staged, false);
    })
}

/// Gives the staged copy in `staging` the name `name` in `copy_dir`; a
/// copy that cannot is removed, so `.partial` keeps nothing of a failure.
/// Without renameat2 RENAME_NOREPLACE (ENOSYS: a kernel before 3.15, as
/// on DSM's 3.10; EINVAL: a filesystem without the flag) the copy is
/// hard-linked to `name` instead, which fails with EEXIST just as the
/// rename would, and its staged name then removed. Both folders are in
/// the move folder, on one filesystem; another fails with EXDEV.
fn place(staging: &File, copy_dir: &File, name: &OsStr, renames: Renames) -> io::Result<()> {
    let staged = OsStr::new(STAGED);
    match (renames.noreplace)(staging.as_fd(), staged, copy_dir.as_fd(), name) {
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) => {
            link_into_place(staging, copy_dir, name, renames.link)
        }
        renamed => renamed,
    }
    .inspect_err(|_| {
        let _ = sys::unlinkat(staging.as_fd(), staged, false);
    })
}

/// Links the staged copy to `name`, then removes its staged name; if that
/// fails, the new name goes too, so the copy is never left with two.
fn link_into_place(staging: &File, copy_dir: &File, name: &OsStr, link: Rename) -> io::Result<()> {
    let staged = OsStr::new(STAGED);
    link(staging.as_fd(), staged, copy_dir.as_fd(), name)?;
    sys::unlinkat(staging.as_fd(), staged, false).inspect_err(|_| {
        let _ = sys::unlinkat(copy_dir.as_fd(), name, false);
    })
}

/// Removes the moved file's original `name` from `parent`; a failure is
/// the item's skip, not the run's end.
fn remove_original(parent: &File, name: &OsStr) -> Option<Skip> {
    sys::unlinkat(parent.as_fd(), name, false)
        .err()
        .map(|_| Skip::UnlinkFailed)
}

/// Whether the staged name in `staging` is still the file `written`
/// (device, inode): only that file may take the copy's final name.
fn is_staged(staging: &File, written: &File) -> io::Result<bool> {
    let stat = sys::statat(staging.as_fd(), OsStr::new(STAGED))?;
    let written = written.metadata()?;
    Ok((stat.device, stat.inode) == (written.dev(), written.ino()))
}

/// Why a copy of `planned` may not replace it: the source changed while
/// it was read (`after`), or the copy read back (`copied`: size and
/// SHA-256) differs from what was read (`digest`).
fn settle(
    planned: &Identity,
    after: &Identity,
    digest: &str,
    copied: (u64, String),
) -> Option<Skip> {
    if after != planned {
        return Some(Skip::ChangedDuringCopy);
    }
    (copied != (planned.size, digest.to_string())).then_some(Skip::CopyMismatch)
}

/// The size and SHA-256 of the file `name` in `dir`, read back.
fn verify(dir: &File, name: &OsStr) -> io::Result<(u64, String)> {
    digest_of(&open_copy(dir, name)?)
}

/// Opens the copy `name` in `dir` to read it back or sync it.
fn open_copy(dir: &File, name: &OsStr) -> io::Result<File> {
    sys::openat(
        dir.as_fd(),
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
}

/// The size and SHA-256 of what `file` reads from its current offset.
fn digest_of(mut file: &File) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((size, hasher.finish_hex()))
}

fn identity_of(meta: &std::fs::Metadata) -> Identity {
    Identity {
        device: meta.dev(),
        inode: meta.ino(),
        file_type: meta.mode() & libc::S_IFMT,
        size: meta.size(),
        mtime: meta.mtime(),
        mtime_nsec: meta.mtime_nsec(),
        nlink: meta.nlink(),
        ctime: meta.ctime(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct Fixture {
        root: PathBuf,
        moves: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let base = crate::spill::testing::scratch(name);
            let root = base.join("root");
            let moves = base.join("moves");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&moves).unwrap();
            Fixture { root, moves }
        }

        fn write(&self, path: &str, data: &[u8]) -> Candidate {
            let full = self.root.join(path.trim_start_matches('/'));
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, data).unwrap();
            self.planned(path, Kind::File)
        }

        fn planned(&self, path: &str, kind: Kind) -> Candidate {
            let full = self.root.join(path.trim_start_matches('/'));
            let meta = std::fs::symlink_metadata(&full).unwrap();
            Candidate {
                path: path.as_bytes().to_vec(),
                kind,
                category: Category::CrashDump,
                identity: Identity {
                    device: meta.dev(),
                    inode: meta.ino(),
                    file_type: meta.mode() & libc::S_IFMT,
                    size: meta.size(),
                    mtime: meta.mtime(),
                    mtime_nsec: meta.mtime_nsec(),
                    nlink: meta.nlink(),
                    ctime: meta.ctime(),
                },
                allocated_bytes: meta.blocks() * 512,
            }
        }

        fn mover(&self) -> Mover {
            Mover {
                root: File::open(&self.root).unwrap(),
                target: File::open(&self.moves).unwrap(),
                held: nothing_held(),
                syncs: Syncs::KERNEL,
                batch: BatchLimits::APPLY,
                reserve: 0,
            }
        }
    }

    fn nothing_held() -> Held {
        Box::new(|_, _| false)
    }

    fn no_log(_: &Moved) -> io::Result<()> {
        Ok(())
    }

    fn exists(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    #[test]
    fn kb_image_backup_is_verified_before_removing_original_and_rejects_non_images() {
        let fx = Fixture::new("kb-image-backup");
        let data = b"\x89PNG\r\n\x1a\npayload";
        let mut image = fx.write("/usr/syno/etc/preference/example/wallpaper", data);
        image.category = Category::PreferenceImage;
        let mut checked = false;
        let source = fx.root.join("usr/syno/etc/preference/example/wallpaper");
        let mut before_remove = |_: &Moved| {
            assert_eq!(std::fs::read(&source)?, data);
            checked = true;
            Ok(())
        };
        assert!(matches!(
            fx.mover().move_file(&image, &mut before_remove).unwrap(),
            Outcome::Moved(_)
        ));
        assert!(checked);
        assert!(!exists(&source));
        assert_eq!(
            std::fs::read(fx.moves.join("usr/syno/etc/preference/example/wallpaper")).unwrap(),
            data
        );
        let mut settings = fx.write(
            "/usr/syno/etc/preference/example/wallpaper_hd",
            b"not an image",
        );
        settings.category = Category::PreferenceImage;
        assert_eq!(
            fx.mover().move_file(&settings, &mut no_log).unwrap(),
            Outcome::Skipped(Skip::Changed)
        );
        assert!(exists(
            &fx.root.join("usr/syno/etc/preference/example/wallpaper_hd")
        ));
    }

    #[test]
    fn a_file_is_copied_verified_and_then_removed_from_root() {
        let fx = Fixture::new("rootfs-apply-move");
        let item = fx.write("/var/crash/core.1", &vec![7u8; 300_000]);

        let outcome = fx.mover().move_file(&item, &mut no_log).unwrap();

        let Outcome::Moved(moved) = outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(moved.size, 300_000);
        let copy = fx.moves.join("var/crash/core.1");
        assert_eq!(std::fs::read(&copy).unwrap(), vec![7u8; 300_000]);
        let mut hasher = crate::sha256::Sha256::new();
        hasher.update(&vec![7u8; 300_000]);
        assert_eq!(moved.sha256, hasher.finish_hex());
        assert!(!exists(&fx.root.join("var/crash/core.1")));
        assert!(exists(&fx.root.join("var/crash")));
    }

    #[test]
    fn a_move_is_logged_before_its_original_goes_and_a_failed_log_keeps_the_original() {
        let fx = Fixture::new("rootfs-apply-intent");
        let item = fx.write("/var/crash/core.1", b"core");
        let original = fx.root.join("var/crash/core.1");
        let mut logged = Vec::new();

        let outcome = fx
            .mover()
            .move_file(&item, &mut |moved| {
                assert!(exists(&original));
                logged.push(moved.clone());
                Ok(())
            })
            .unwrap();

        assert_eq!(outcome, Outcome::Moved(logged[0].clone()));
        assert!(!exists(&original));
        let item = fx.write("/var/crash/core.2", b"core two");
        let error = fx
            .mover()
            .move_file(&item, &mut |_| Err(io::Error::other("manifest full")))
            .unwrap_err();
        assert_eq!(error.to_string(), "manifest full");
        assert!(exists(&fx.root.join("var/crash/core.2")));
        assert!(exists(&fx.moves.join("var/crash/core.2")));
    }

    #[test]
    fn totals_saturate_rather_than_wrap() {
        let fx = Fixture::new("rootfs-apply-saturate");
        let mut item = fx.write("/var/crash/core.1", b"x");
        item.allocated_bytes = u64::MAX;
        let moved = || {
            Outcome::Moved(Moved {
                size: u64::MAX,
                mtime: 0,
                sha256: String::new(),
            })
        };
        let mut done = Done::default();

        done.count(&item, moved());
        done.count(&item, moved());

        assert_eq!(
            (done.moved_size, done.moved_allocated_bytes),
            (u64::MAX, u64::MAX)
        );
        assert_eq!(done.moved_files, 2);
    }

    #[test]
    fn a_file_changed_since_the_plan_is_left_and_not_copied() {
        let fx = Fixture::new("rootfs-apply-changed");
        let item = fx.write("/var/crash/core.1", b"planned");
        std::fs::write(fx.root.join("var/crash/core.1"), b"rewritten later").unwrap();

        let outcome = fx.mover().move_file(&item, &mut no_log).unwrap();

        assert_eq!(outcome, Outcome::Skipped(Skip::Changed));
        assert!(exists(&fx.root.join("var/crash/core.1")));
        assert!(!exists(&fx.moves.join("var/crash/core.1")));
    }

    #[test]
    fn a_missing_file_is_skipped() {
        let fx = Fixture::new("rootfs-apply-missing");
        let item = fx.write("/var/crash/core.1", b"x");
        std::fs::remove_file(fx.root.join("var/crash/core.1")).unwrap();
        assert_eq!(
            fx.mover().move_file(&item, &mut no_log).unwrap(),
            Outcome::Skipped(Skip::Missing)
        );
    }

    #[test]
    fn a_symlink_anywhere_on_the_path_is_never_followed() {
        let fx = Fixture::new("rootfs-apply-symlink");
        let elsewhere = fx.root.join("etc");
        let item = fx.write("/etc/shadow", b"secret");
        std::fs::create_dir_all(fx.root.join("var")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, fx.root.join("var/crash")).unwrap();
        let mut planted = item.clone();
        planted.path = b"/var/crash/shadow".to_vec();

        let outcome = fx.mover().move_file(&planted, &mut no_log).unwrap();

        assert_eq!(outcome, Outcome::Skipped(Skip::PathChanged));
        assert!(exists(&fx.root.join("etc/shadow")));
        assert!(!exists(&fx.moves.join("var")));
    }

    #[test]
    fn a_file_that_gained_a_hard_link_since_the_plan_is_left() {
        let fx = Fixture::new("rootfs-apply-hardlink");
        let item = fx.write("/var/tmp/convert_thumbnail_1", b"x");
        std::fs::hard_link(
            fx.root.join("var/tmp/convert_thumbnail_1"),
            fx.root.join("var/tmp/other_name"),
        )
        .unwrap();

        assert_eq!(
            fx.mover().move_file(&item, &mut no_log).unwrap(),
            Outcome::Skipped(Skip::Changed)
        );
        assert!(!exists(&fx.moves.join("var")));
    }

    #[test]
    fn a_symlink_planted_in_the_move_folder_is_never_followed() {
        let fx = Fixture::new("rootfs-apply-move-symlink");
        let item = fx.write("/var/crash/core.1", b"core");
        let elsewhere = crate::spill::testing::scratch("rootfs-apply-move-symlink-target");
        std::os::unix::fs::symlink(&elsewhere, fx.moves.join("var")).unwrap();

        assert!(fx.mover().move_file(&item, &mut no_log).is_err());
        assert!(exists(&fx.root.join("var/crash/core.1")));
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_file_a_process_holds_open_at_apply_is_skipped() {
        let fx = Fixture::new("rootfs-apply-held");
        let item = fx.write("/var/crash/core.1", b"x");
        let planned = (item.identity.device, item.identity.inode);
        let mover = Mover {
            held: Box::new(move |device, inode| (device, inode) == planned),
            ..fx.mover()
        };

        assert_eq!(
            mover.move_file(&item, &mut no_log).unwrap(),
            Outcome::Skipped(Skip::HeldOpen)
        );
        assert!(exists(&fx.root.join("var/crash/core.1")));
    }

    #[test]
    fn a_file_opened_after_the_start_snapshot_is_moved() {
        let fx = Fixture::new("rootfs-apply-opened-later");
        let item = fx.write("/var/crash/core.1", b"x");
        let original = fx.root.join("var/crash/core.1");
        let _holder = File::open(&original).unwrap();

        let outcome = fx.mover().move_file(&item, &mut no_log).unwrap();

        assert!(matches!(outcome, Outcome::Moved(_)), "{outcome:?}");
        assert!(!exists(&original));
        assert!(exists(&fx.moves.join("var/crash/core.1")));
    }

    #[test]
    fn files_move_in_batches_of_at_most_so_many_files_and_bytes_a_big_file_alone() {
        let fx = Fixture::new("rootfs-apply-batches");
        let sized = |name: &str, size: usize| {
            let mut item = fx.write(&format!("/upd@te/{name}"), b"x");
            item.identity.size = size as u64;
            item.allocated_bytes = 0;
            item
        };
        let items = [
            sized("a", 10),
            sized("b", 10),
            sized("c", 100),
            sized("d", 10),
        ];
        let files: Vec<&Candidate> = items.iter().collect();
        let limits = BatchLimits {
            files: 2,
            bytes: 50,
        };

        let sizes: Vec<Vec<u64>> = batches(&files, limits)
            .iter()
            .map(|batch| batch.iter().map(|item| item.identity.size).collect())
            .collect();

        assert_eq!(sizes, [vec![10, 10], vec![100], vec![10]]);
    }

    /// A manifest that records, at each flush, which originals are still
    /// on `/`.
    struct Watched<'a> {
        lines: Vec<u8>,
        originals: Vec<PathBuf>,
        flushes: &'a std::cell::RefCell<Vec<usize>>,
    }

    impl Write for Watched<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.lines.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            let left = self.originals.iter().filter(|path| exists(path)).count();
            self.flushes.borrow_mut().push(left);
            Ok(())
        }
    }

    #[test]
    fn a_batch_logs_all_its_moves_in_one_flush_before_any_original_goes() {
        let fx = Fixture::new("rootfs-apply-batch-log");
        let items = vec![
            fx.write("/var/log/a.xz", b"a"),
            fx.write("/var/log/b.xz", b"b"),
            fx.write("/var/log/sub/c.xz", b"c"),
        ];
        let flushes = std::cell::RefCell::new(Vec::new());
        let mut manifest = Watched {
            lines: Vec::new(),
            originals: ["var/log/a.xz", "var/log/b.xz", "var/log/sub/c.xz"]
                .iter()
                .map(|path| fx.root.join(path))
                .collect(),
            flushes: &flushes,
        };

        let done = run(
            &fx.mover(),
            &items,
            &mut manifest,
            &mut StopAfter(usize::MAX),
        )
        .unwrap();

        assert_eq!(done.moved_files, 3);
        assert_eq!(flushes.borrow()[0], 3, "{:?}", flushes.borrow());
        assert_eq!(
            String::from_utf8(manifest.lines).unwrap().lines().count(),
            3
        );
        assert!(!exists(&fx.root.join("var/log/sub/c.xz")));
    }

    thread_local! {
        static SYNCED: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
        static DATA_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn filesystem_logged(_: BorrowedFd<'_>) -> io::Result<()> {
        SYNCED.with(|synced| synced.borrow_mut().push("filesystem"));
        Ok(())
    }

    fn data_logged(_: &File) -> io::Result<()> {
        SYNCED.with(|synced| synced.borrow_mut().push("data"));
        Ok(())
    }

    fn data_fails(_: &File) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EIO))
    }

    fn data_fails_second(file: &File) -> io::Result<()> {
        let fail = DATA_CALLS.with(|calls| {
            let fail = calls.get() == 1;
            calls.set(calls.get() + 1);
            fail
        });
        if fail { data_fails(file) } else { Ok(()) }
    }

    #[test]
    fn a_batch_syncs_its_filesystem_once_then_the_data_of_each_copy() {
        let fx = Fixture::new("rootfs-apply-batch-syncs");
        let items: Vec<Candidate> = (0..3)
            .map(|i| fx.write(&format!("/var/log/{i}.xz"), b"x"))
            .collect();
        let mover = Mover {
            syncs: Syncs {
                filesystem: filesystem_logged,
                data: data_logged,
            },
            ..fx.mover()
        };

        let done = run(&mover, &items, &mut Vec::new(), &mut StopAfter(usize::MAX)).unwrap();

        assert_eq!(done.moved_files, 3);
        assert_eq!(
            SYNCED.with(|synced| synced.borrow().clone()),
            ["filesystem", "data", "data", "data"]
        );
    }

    #[test]
    fn a_copy_whose_data_sync_fails_is_removed_and_its_original_kept() {
        let fx = Fixture::new("rootfs-apply-sync-fails");
        let items = vec![fx.write("/var/log/a.xz", b"a")];
        let mover = Mover {
            syncs: Syncs {
                filesystem: |_| Ok(()),
                data: data_fails,
            },
            ..fx.mover()
        };
        let mut manifest = Vec::new();

        let error = run(&mover, &items, &mut manifest, &mut StopAfter(usize::MAX)).unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert!(exists(&fx.root.join("var/log/a.xz")));
        assert!(!exists(&fx.moves.join("var/log/a.xz")));
        assert!(manifest.is_empty());
    }

    #[test]
    fn a_batch_error_keeps_counts_from_completed_batches() {
        DATA_CALLS.with(|calls| calls.set(0));
        let fx = Fixture::new("rootfs-apply-partial-error");
        let items = vec![
            fx.write("/var/log/a.xz", b"one"),
            fx.write("/var/log/b.xz", b"two"),
        ];
        let mover = Mover {
            batch: BatchLimits {
                files: 1,
                bytes: u64::MAX,
            },
            syncs: Syncs {
                filesystem: |_| Ok(()),
                data: data_fails_second,
            },
            ..fx.mover()
        };

        let (error, done) = run_with_progress_with_done(
            &mover,
            &items,
            &mut Vec::new(),
            &mut StopAfter(usize::MAX),
            &mut |_| {},
        )
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(done.moved_files, 1);
        assert_eq!(done.moved_size, 3);
        assert!(exists(&fx.root.join("var/log/b.xz")));
    }

    #[test]
    fn a_planned_folder_is_removed_only_when_empty_and_still_the_same() {
        let fx = Fixture::new("rootfs-apply-dirs");
        std::fs::create_dir_all(fx.root.join("upd@te/empty")).unwrap();
        std::fs::create_dir_all(fx.root.join("upd@te/full")).unwrap();
        std::fs::write(fx.root.join("upd@te/full/kept"), b"x").unwrap();
        std::fs::create_dir_all(fx.root.join("upd@te/swapped")).unwrap();
        let empty = fx.planned("/upd@te/empty", Kind::Dir);
        let full = fx.planned("/upd@te/full", Kind::Dir);
        let swapped = fx.planned("/upd@te/swapped", Kind::Dir);
        // The planned folder stays alive elsewhere, so the new one cannot
        // reuse its inode.
        std::fs::rename(fx.root.join("upd@te/swapped"), fx.root.join("upd@te/other")).unwrap();
        std::fs::create_dir(fx.root.join("upd@te/swapped")).unwrap();
        let mover = fx.mover();

        assert_eq!(mover.remove_dir(&empty).unwrap(), Outcome::RemovedDir);
        assert_eq!(
            mover.remove_dir(&full).unwrap(),
            Outcome::Skipped(Skip::NotEmpty)
        );
        assert_eq!(
            mover.remove_dir(&swapped).unwrap(),
            Outcome::Skipped(Skip::Changed)
        );
        assert!(!exists(&fx.root.join("upd@te/empty")));
        assert!(exists(&fx.root.join("upd@te/full/kept")));
    }

    struct StopAfter(usize);

    impl BatchBoundary for StopAfter {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            if self.0 == 0 {
                return ControlFlow::Break(StopReason::Paused);
            }
            self.0 -= 1;
            ControlFlow::Continue(())
        }
    }

    #[test]
    fn a_run_moves_files_then_removes_folders_deepest_first_and_logs_each_skips_first_in_a_batch() {
        let fx = Fixture::new("rootfs-apply-run");
        let a = fx.write("/upd@te/a/one", b"1");
        let b = fx.write("/upd@te/b", b"22");
        let changed = fx.write("/upd@te/c", b"3");
        std::fs::write(fx.root.join("upd@te/c"), b"changed").unwrap();
        let items = vec![
            fx.planned("/upd@te", Kind::Dir),
            fx.planned("/upd@te/a", Kind::Dir),
            a,
            b,
            changed,
        ];
        let mut manifest = Vec::new();

        let done = run(
            &fx.mover(),
            &items,
            &mut manifest,
            &mut StopAfter(usize::MAX),
        )
        .unwrap();

        assert_eq!(done.moved_files, 2);
        assert_eq!(done.moved_size, 3);
        assert_eq!(done.removed_dirs, 1);
        assert_eq!(done.skipped, 2);
        assert_eq!(done.stopped, None);
        assert!(!exists(&fx.root.join("upd@te/a")));
        assert!(exists(&fx.root.join("upd@te/c")));
        let lines: Vec<serde_json::Value> = String::from_utf8(manifest)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let outcomes: Vec<(&str, &str)> = lines
            .iter()
            .map(|l| (l["path"].as_str().unwrap(), l["outcome"].as_str().unwrap()))
            .collect();
        assert_eq!(
            outcomes,
            [
                ("/upd@te/c", "skipped"),
                ("/upd@te/a/one", "moved"),
                ("/upd@te/b", "moved"),
                ("/upd@te/a", "removed"),
                ("/upd@te", "skipped"),
            ]
        );
        assert_eq!(lines[1]["size"], 1);
        assert_eq!(lines[1]["sha256"].as_str().unwrap().len(), 64);
        assert_eq!(lines[0]["reason"], "changed");
        assert_eq!(lines[4]["reason"], "not_empty");
    }

    #[test]
    fn a_run_leaves_no_empty_staging_folder_in_the_move_folder() {
        let fx = Fixture::new("rootfs-apply-staging");
        let items = [fx.write("/upd@te/a", b"1"), fx.write("/upd@te/b", b"2")];
        let mover = Mover {
            batch: BatchLimits {
                files: 1,
                bytes: u64::MAX,
            },
            ..fx.mover()
        };

        let paused = run(&mover, &items[..1], &mut Vec::new(), &mut StopAfter(0)).unwrap();
        assert_eq!(paused.stopped, Some(StopReason::Paused));
        assert!(!exists(&fx.moves.join(STAGING)));

        let done = run(
            &mover,
            &items[1..],
            &mut Vec::new(),
            &mut StopAfter(usize::MAX),
        )
        .unwrap();
        assert_eq!(done.moved_files, 1);
        assert!(!exists(&fx.moves.join(STAGING)));
        assert!(exists(&fx.moves.join("upd@te/a")) && exists(&fx.moves.join("upd@te/b")));
    }

    #[test]
    fn a_stop_between_batches_leaves_the_rest_untouched() {
        let fx = Fixture::new("rootfs-apply-stop");
        let items = vec![fx.write("/upd@te/a", b"1"), fx.write("/upd@te/b", b"2")];
        let mut manifest = Vec::new();
        let mover = Mover {
            batch: BatchLimits {
                files: 1,
                bytes: u64::MAX,
            },
            ..fx.mover()
        };

        let done = run(&mover, &items, &mut manifest, &mut StopAfter(0)).unwrap();

        assert_eq!(done.moved_files, 1);
        assert_eq!(done.stopped, Some(StopReason::Paused));
        assert!(exists(&fx.root.join("upd@te/b")));
    }

    #[test]
    fn progress_reports_completed_move_batches_and_pauses_at_the_batch_boundary() {
        let fx = Fixture::new("rootfs-apply-progress");
        let items = vec![fx.write("/upd@te/a", b"one"), fx.write("/upd@te/b", b"two")];
        let mover = Mover {
            batch: BatchLimits {
                files: 1,
                bytes: u64::MAX,
            },
            ..fx.mover()
        };
        let mut updates = Vec::new();
        let mut report = |update| updates.push(update);

        let done = run_with_progress(
            &mover,
            &items,
            &mut Vec::new(),
            &mut StopAfter(0),
            &mut report,
        )
        .unwrap();

        assert_eq!(done.moved_files, 1);
        assert_eq!(done.stopped, Some(StopReason::Paused));
        assert_eq!(updates.last().unwrap().phase, "moves");
        assert_eq!(updates.last().unwrap().items_done, 1);
        assert_eq!(updates.last().unwrap().items_total, 2);
        assert_eq!(updates.last().unwrap().bytes_done, 3);
        assert_eq!(updates.last().unwrap().bytes_total, 6);
    }

    /// Stands in for DSM's tools: records each run, answers `outcome`.
    struct FakeTools {
        ran: Vec<Delegate>,
        outcome: ToolOutcome,
    }

    impl FakeTools {
        fn new() -> Self {
            FakeTools {
                ran: Vec::new(),
                outcome: ToolOutcome::Succeeded,
            }
        }
    }

    impl Tools for FakeTools {
        fn run(&mut self, tool: Delegate) -> ToolOutcome {
            self.ran.push(tool);
            self.outcome
        }
    }

    #[test]
    fn planned_tools_run_in_order_each_logged_with_its_argv() {
        let mut tools = FakeTools::new();
        tools.outcome = ToolOutcome::Failed { exit_code: 1 };
        let mut manifest = Vec::new();
        let mut done = Done::default();
        let planned = [Delegate::RotateLogs, Delegate::DeleteJournal];

        run_tools(
            &planned,
            &mut tools,
            &mut manifest,
            &mut StopAfter(usize::MAX),
            &mut done,
        )
        .unwrap();

        assert_eq!(tools.ran, planned);
        assert_eq!(done.tools.len(), 2);
        let first: serde_json::Value =
            serde_json::from_str(String::from_utf8(manifest).unwrap().lines().next().unwrap())
                .unwrap();
        assert_eq!(
            first,
            serde_json::json!({
                "tool": "rotate_logs",
                "argv": ["/usr/syno/bin/synocleaner", "--rotate-log"],
                "outcome": "failed",
                "exit_code": 1
            })
        );

        for (checks, ran) in [(0, &planned[..0]), (1, &planned[..1])] {
            let mut tools = FakeTools::new();
            let mut stopped = Done::default();
            run_tools(
                &planned,
                &mut tools,
                &mut Vec::new(),
                &mut StopAfter(checks),
                &mut stopped,
            )
            .unwrap();
            assert_eq!(tools.ran, ran, "a stop at check {checks}");
            assert_eq!(stopped.stopped, Some(StopReason::Paused));
        }
    }

    #[test]
    fn tool_progress_names_the_active_tool_and_completed_tools() {
        let mut tools = FakeTools::new();
        let mut manifest = Vec::new();
        let mut done = Done::default();
        let planned = [Delegate::RotateLogs, Delegate::DeleteJournal];
        let mut updates = Vec::new();
        let mut report = |update| updates.push(update);

        run_tools_with_progress(
            &planned,
            &mut tools,
            &mut manifest,
            &mut StopAfter(usize::MAX),
            &mut done,
            &mut report,
        )
        .unwrap();

        let active: Vec<_> = updates.iter().filter_map(|update| update.tool).collect();
        assert_eq!(active, planned);
        assert_eq!(updates.last().unwrap().phase, "tools");
        assert_eq!(updates.last().unwrap().items_done, 2);
        assert_eq!(updates.last().unwrap().items_total, 2);
        assert_eq!(updates.last().unwrap().tool, None);
    }

    fn gate_code(uid: u32, yes: bool, config: Option<CleanupConfig>) -> Result<(), &'static str> {
        let plan =
            crate::rootfs::plan::RootfsPlan::seal(crate::rootfs::plan::testing::payload()).unwrap();
        gate(&plan, 1_790_000_001, config.as_ref(), yes, uid).map_err(|r| r.code)
    }

    #[test]
    fn apply_needs_root_and_yes_and_refuses_only_under_the_kill_switch() {
        assert_eq!(gate_code(0, true, None), Ok(()));
        assert_eq!(gate_code(0, true, Some(CleanupConfig::default())), Ok(()));
        assert_eq!(gate_code(1000, true, None), Err("not_root"));
        assert_eq!(gate_code(0, false, None), Err("yes_required"));
        let killed = CleanupConfig {
            mutations_disabled: true,
            ..CleanupConfig::default()
        };
        assert_eq!(gate_code(0, true, Some(killed)), Err("mutations_disabled"));
    }

    #[test]
    fn apply_refuses_a_plan_that_is_no_longer_valid() {
        let mut plan =
            crate::rootfs::plan::RootfsPlan::seal(crate::rootfs::plan::testing::payload()).unwrap();
        plan.payload.items[0].path = b"/etc/passwd".to_vec();
        let refusal = gate(&plan, 1_790_000_001, None, true, 0).unwrap_err();
        assert_eq!(refusal.code, "plan_invalid");
    }

    #[test]
    fn the_move_folder_is_made_private_resumed_if_ours_and_never_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new("rootfs-apply-move-dir");
        let destination = File::open(&fx.moves).unwrap();
        let name = OsStr::new("rootfs-1-2");

        let (dir, mut manifest) = open_move_dir(&destination, name).unwrap();
        manifest.write_all(b"first\n").unwrap();
        let mode = dir.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        drop(manifest);
        let (_, mut again) = open_move_dir(&destination, name).unwrap();
        again.write_all(b"second\n").unwrap();
        assert_eq!(
            std::fs::read(fx.moves.join("rootfs-1-2/manifest.jsonl")).unwrap(),
            b"first\nsecond\n"
        );
        let manifest_mode = std::fs::metadata(fx.moves.join("rootfs-1-2/manifest.jsonl"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(manifest_mode & 0o777, 0o600);

        std::os::unix::fs::symlink(&fx.root, fx.moves.join("rootfs-3-4")).unwrap();
        assert!(open_move_dir(&destination, OsStr::new("rootfs-3-4")).is_err());
    }

    #[test]
    fn a_second_apply_into_the_same_move_folder_is_refused_while_the_first_runs() {
        let fx = Fixture::new("rootfs-apply-running");
        let destination = File::open(&fx.moves).unwrap();
        let name = OsStr::new("rootfs-1-2");
        let first = open_move_dir(&destination, name).unwrap();

        match open_move_dir(&destination, name) {
            Err(ExecuteError::Refused(refusal)) => assert_eq!(refusal.code, "apply_running"),
            other => panic!("{other:?}"),
        }
        drop(first);
        assert!(open_move_dir(&destination, name).is_ok());
    }

    #[test]
    fn the_move_folder_and_its_copies_are_private_and_an_open_folder_in_it_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new("rootfs-apply-private");
        let item = fx.write("/var/crash/core.1", b"core");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        std::fs::create_dir(fx.moves.join("var")).unwrap();
        std::fs::set_permissions(fx.moves.join("var"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        let refused = fx.mover().move_file(&item, &mut no_log).unwrap_err();

        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(exists(&fx.root.join("var/crash/core.1")));
        std::fs::set_permissions(fx.moves.join("var"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(matches!(
            fx.mover().move_file(&item, &mut no_log),
            Ok(Outcome::Moved(_))
        ));
        assert_eq!(mode(&fx.moves.join("var/crash")), 0o700);
        assert_eq!(mode(&fx.moves.join("var/crash/core.1")), 0o600);
    }

    /// Writes `data` as a private file at `path` below `fx.moves`, as an
    /// earlier interrupted run would have left it.
    fn left_in_moves(fx: &Fixture, path: &str, data: &[u8]) {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let full = fx.moves.join(path);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(full.parent().unwrap())
            .unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(full)
            .unwrap();
        file.write_all(data).unwrap();
    }

    #[test]
    fn a_copy_an_interrupted_run_left_staged_never_blocks_a_rerun() {
        let fx = Fixture::new("rootfs-apply-staged-leftover");
        let item = fx.write("/var/crash/core.1", b"core");
        left_in_moves(&fx, ".partial/copy", b"half a co");

        let outcome = fx.mover().move_file(&item, &mut no_log).unwrap();

        assert!(matches!(outcome, Outcome::Moved(_)), "{outcome:?}");
        assert_eq!(
            std::fs::read(fx.moves.join("var/crash/core.1")).unwrap(),
            b"core"
        );
        assert!(!exists(&fx.moves.join(".partial/copy")));
        assert!(!exists(&fx.root.join("var/crash/core.1")));
    }

    #[test]
    fn a_finished_copy_of_a_file_still_on_root_counts_only_if_identical() {
        let fx = Fixture::new("rootfs-apply-final-leftover");
        let same = fx.write("/var/crash/core.1", b"core");
        let other = fx.write("/var/crash/core.2", b"core two");
        left_in_moves(&fx, "var/crash/core.1", b"core");
        left_in_moves(&fx, "var/crash/core.2", b"not core two");
        let mover = fx.mover();

        assert!(matches!(
            mover.move_file(&same, &mut no_log),
            Ok(Outcome::Moved(_))
        ));
        assert!(!exists(&fx.root.join("var/crash/core.1")));
        assert_eq!(
            mover.move_file(&other, &mut no_log).unwrap(),
            Outcome::Skipped(Skip::CopyExists)
        );
        assert_eq!(
            std::fs::read(fx.root.join("var/crash/core.2")).unwrap(),
            b"core two"
        );
        assert_eq!(
            std::fs::read(fx.moves.join("var/crash/core.2")).unwrap(),
            b"not core two"
        );
    }

    #[test]
    fn a_staged_copy_is_renamed_only_while_its_name_is_still_the_file_written() {
        let fx = Fixture::new("rootfs-apply-staged-swap");
        let staging =
            create_below(&File::open(&fx.moves).unwrap(), &[OsStr::new(STAGING)]).unwrap();
        fx.write("/upd@te/a", b"a");
        let source = File::open(fx.root.join("upd@te/a")).unwrap();

        let (_, written) = stage(&source, &staging).unwrap();
        assert!(is_staged(&staging, &written).unwrap());

        let swapped = fx.moves.join(".partial/other");
        std::fs::write(&swapped, b"a").unwrap();
        std::fs::rename(&swapped, fx.moves.join(".partial/copy")).unwrap();
        assert!(!is_staged(&staging, &written).unwrap());
    }

    /// A staged copy of `data` and the folder its final name goes in.
    fn staged(fx: &Fixture, data: &[u8]) -> (File, File) {
        left_in_moves(fx, ".partial/copy", data);
        let moves = File::open(&fx.moves).unwrap();
        let staging = create_below(&moves, &[OsStr::new(STAGING)]).unwrap();
        let copy_dir = create_below(&moves, &[OsStr::new("var")]).unwrap();
        (staging, copy_dir)
    }

    #[test]
    fn a_staged_copy_that_cannot_take_its_name_is_removed_and_the_name_kept() {
        let fx = Fixture::new("rootfs-apply-place-taken");
        let (staging, copy_dir) = staged(&fx, b"copy");
        left_in_moves(&fx, "var/core.1", b"other");

        let error = place(&staging, &copy_dir, OsStr::new("core.1"), Renames::KERNEL).unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        assert_eq!(
            std::fs::read(fx.moves.join("var/core.1")).unwrap(),
            b"other"
        );
        assert!(!exists(&fx.moves.join(".partial/copy")));
    }

    fn no_renameat2(_: BorrowedFd<'_>, _: &OsStr, _: BorrowedFd<'_>, _: &OsStr) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn no_noreplace_flag(
        _: BorrowedFd<'_>,
        _: &OsStr,
        _: BorrowedFd<'_>,
        _: &OsStr,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EINVAL))
    }

    fn cross_device(_: BorrowedFd<'_>, _: &OsStr, _: BorrowedFd<'_>, _: &OsStr) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EXDEV))
    }

    fn no_links(_: BorrowedFd<'_>, _: &OsStr, _: BorrowedFd<'_>, _: &OsStr) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EPERM))
    }

    fn without_renameat2(noreplace: Rename) -> Renames {
        Renames {
            noreplace,
            ..Renames::KERNEL
        }
    }

    #[test]
    fn without_renameat2_a_staged_copy_is_linked_to_its_name_and_unstaged() {
        for (case, noreplace) in [
            ("enosys", no_renameat2 as Rename),
            ("einval", no_noreplace_flag),
        ] {
            let fx = Fixture::new(&format!("rootfs-apply-place-link-{case}"));
            let (staging, copy_dir) = staged(&fx, b"copy");

            place(
                &staging,
                &copy_dir,
                OsStr::new("core.1"),
                without_renameat2(noreplace),
            )
            .unwrap();

            let copy = fx.moves.join("var/core.1");
            assert_eq!(std::fs::read(&copy).unwrap(), b"copy", "{case}");
            assert_eq!(std::fs::metadata(&copy).unwrap().nlink(), 1, "{case}");
            assert!(!exists(&fx.moves.join(".partial/copy")), "{case}");
        }
    }

    #[test]
    fn the_link_fallback_never_replaces_a_file_that_has_the_name() {
        let fx = Fixture::new("rootfs-apply-place-link-taken");
        let (staging, copy_dir) = staged(&fx, b"copy");
        left_in_moves(&fx, "var/core.1", b"other");

        let error = place(
            &staging,
            &copy_dir,
            OsStr::new("core.1"),
            without_renameat2(no_renameat2),
        )
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        assert_eq!(
            std::fs::read(fx.moves.join("var/core.1")).unwrap(),
            b"other"
        );
        assert!(!exists(&fx.moves.join(".partial/copy")));
    }

    #[test]
    fn a_failed_link_fallback_removes_the_staged_copy() {
        let fx = Fixture::new("rootfs-apply-place-link-failed");
        let (staging, copy_dir) = staged(&fx, b"copy");
        let renames = Renames {
            noreplace: no_renameat2,
            link: no_links,
        };

        let error = place(&staging, &copy_dir, OsStr::new("core.1"), renames).unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        assert!(!exists(&fx.moves.join("var/core.1")));
        assert!(!exists(&fx.moves.join(".partial/copy")));
    }

    #[test]
    fn only_a_missing_renameat2_falls_back_to_a_link() {
        let fx = Fixture::new("rootfs-apply-place-no-fallback");
        let (staging, copy_dir) = staged(&fx, b"copy");

        let error = place(
            &staging,
            &copy_dir,
            OsStr::new("core.1"),
            without_renameat2(cross_device),
        )
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EXDEV));
        assert!(!exists(&fx.moves.join("var/core.1")));
        assert!(!exists(&fx.moves.join(".partial/copy")));
    }

    #[test]
    fn a_failed_unlink_after_the_moved_line_is_recorded_not_fatal() {
        let fx = Fixture::new("rootfs-apply-unlink-failed");
        std::fs::create_dir_all(fx.root.join("upd@te/a")).unwrap();
        let parent = File::open(fx.root.join("upd@te")).unwrap();

        assert_eq!(
            remove_original(&parent, OsStr::new("a")),
            Some(Skip::UnlinkFailed)
        );
        assert!(exists(&fx.root.join("upd@te/a")));
    }

    #[test]
    fn a_source_that_cannot_be_opened_is_skipped_not_fatal() {
        let fx = Fixture::new("rootfs-apply-open-source");
        std::fs::create_dir_all(fx.root.join("var/tmp")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", fx.root.join("var/tmp/etilqs_1")).unwrap();
        let parent = File::open(fx.root.join("var/tmp")).unwrap();

        assert_eq!(
            open_source(&parent, OsStr::new("etilqs_0")).unwrap_err(),
            Skip::Missing
        );
        assert_eq!(
            open_source(&parent, OsStr::new("etilqs_1")).unwrap_err(),
            Skip::PathChanged
        );
    }

    #[test]
    fn a_copy_that_fails_midway_is_removed() {
        let fx = Fixture::new("rootfs-apply-staged-failure");
        let staging =
            create_below(&File::open(&fx.moves).unwrap(), &[OsStr::new(STAGING)]).unwrap();
        // Reading a folder fails (EISDIR) after the staged copy exists.
        let unreadable = File::open(&fx.root).unwrap();

        let error = stage(&unreadable, &staging).unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EISDIR));
        assert!(!exists(&fx.moves.join(".partial/copy")));
    }

    #[test]
    fn a_copy_replaces_its_file_only_if_the_file_held_still_and_the_copy_reads_back_the_same() {
        let fx = Fixture::new("rootfs-apply-settle");
        let planned = fx.write("/var/crash/core.1", b"core").identity;
        let digest = "d".repeat(64);
        let copied = || (planned.size, digest.clone());

        assert_eq!(settle(&planned, &planned, &digest, copied()), None);
        let grown = Identity {
            size: planned.size + 1,
            ..planned
        };
        assert_eq!(
            settle(&planned, &grown, &digest, copied()),
            Some(Skip::ChangedDuringCopy)
        );
        let short = (planned.size - 1, digest.clone());
        assert_eq!(
            settle(&planned, &planned, &digest, short),
            Some(Skip::CopyMismatch)
        );
        let other = (planned.size, "e".repeat(64));
        assert_eq!(
            settle(&planned, &planned, &digest, other),
            Some(Skip::CopyMismatch)
        );
    }

    #[test]
    fn a_sparse_file_is_copied_with_its_holes() {
        use std::os::unix::fs::FileExt;
        let fx = Fixture::new("rootfs-apply-sparse");
        let path = fx.root.join("var/crash/core.1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let sparse = File::create(&path).unwrap();
        sparse.set_len(16 << 20).unwrap();
        sparse.write_at(b"middle", 8 << 20).unwrap();
        drop(sparse);
        let (_, expected) = digest_of(&File::open(&path).unwrap()).unwrap();
        let item = fx.planned("/var/crash/core.1", Kind::File);

        let outcome = fx.mover().move_file(&item, &mut no_log).unwrap();

        let Outcome::Moved(moved) = outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(moved.sha256, expected);
        let copy = std::fs::metadata(fx.moves.join("var/crash/core.1")).unwrap();
        assert_eq!(copy.len(), 16 << 20);
        assert!(
            copy.blocks() * 512 < 4 << 20,
            "{} bytes allocated",
            copy.blocks() * 512
        );
    }

    #[test]
    fn a_file_the_move_folder_has_no_room_for_stays_and_the_run_stops() {
        let fx = Fixture::new("rootfs-apply-no-room");
        let items = vec![fx.write("/upd@te/a", b"1"), fx.write("/upd@te/b", b"2")];
        let mut mover = fx.mover();
        mover.reserve = u64::MAX / 2;
        let mut manifest = Vec::new();

        let done = run(&mover, &items, &mut manifest, &mut StopAfter(usize::MAX)).unwrap();

        assert_eq!(done.skips, vec![(b"/upd@te/a".to_vec(), Skip::NoRoom)]);
        assert_eq!(done.stopped, Some(StopReason::DiskLimit));
        assert!(exists(&fx.root.join("upd@te/a")) && exists(&fx.root.join("upd@te/b")));
        assert!(!exists(&fx.moves.join("upd@te/a")));
    }

    #[test]
    fn a_full_or_over_quota_filesystem_means_no_room() {
        assert!(no_room(&io::Error::from_raw_os_error(libc::ENOSPC)));
        assert!(no_room(&io::Error::from_raw_os_error(libc::EDQUOT)));
        assert!(!no_room(&io::Error::from_raw_os_error(libc::EIO)));
    }

    /// A plan of `items` in `fx`'s fake root, moving into `fx.moves`.
    fn fake_plan(fx: &Fixture, items: Vec<Candidate>) -> RootfsPlan {
        let root = std::fs::metadata(&fx.root).unwrap();
        let destination = std::fs::metadata(&fx.moves).unwrap();
        let mut payload = crate::rootfs::plan::testing::payload();
        payload.root_device = root.dev();
        payload.destination.path = fx.moves.as_os_str().as_bytes().to_vec();
        payload.destination.device = destination.dev();
        payload.destination.inode = destination.ino();
        payload.items = items;
        RootfsPlan::seal(payload).unwrap()
    }

    #[test]
    fn executing_a_plan_on_a_fake_root_moves_its_items_and_logs_them() {
        let fx = Fixture::new("rootfs-apply-execute");
        let items = vec![
            fx.write("/.SynoUpgrade.tar", &vec![1u8; 70_000]),
            fx.write("/var/log/disk-latency/a.xz", b"xz"),
            fx.write("/var/log/disk-latency/new\nline.xz", b"newline"),
        ];
        std::fs::write(fx.root.join("var/log/messages"), b"live").unwrap();
        let plan = fake_plan(&fx, items);

        let done = execute(
            &plan,
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut FakeTools::new(),
            &mut StopAfter(usize::MAX),
        )
        .unwrap();

        assert_eq!((done.moved_files, done.skipped), (3, 0));
        let moved = fx.moves.join("SynoReclaim-system-20260921-1413");
        assert_eq!(
            std::fs::read(moved.join(".SynoUpgrade.tar")).unwrap().len(),
            70_000
        );
        assert!(moved.join("var/log/disk-latency/a.xz").exists());
        assert!(moved.join("var/log/disk-latency/new\nline.xz").exists());
        let readme = std::fs::read_to_string(moved.join("README.txt")).unwrap();
        assert!(readme.contains("/.SynoUpgrade.tar"));
        assert!(readme.contains("/var/log/disk-latency/a.xz"));
        assert!(readme.contains("/var/log/disk-latency/new\\nline.xz"));
        assert!(!exists(&fx.root.join(".SynoUpgrade.tar")));
        assert!(exists(&fx.root.join("var/log/messages")));
        let manifest = std::fs::read_to_string(moved.join("manifest.jsonl")).unwrap();
        let marker: serde_json::Value =
            serde_json::from_str(manifest.lines().next().unwrap()).unwrap();
        assert_eq!(
            marker,
            serde_json::json!({"event": "plan", "plan_id": plan.payload.plan_id})
        );
        assert_eq!(manifest.lines().count(), 4);
        assert_eq!(
            std::fs::metadata(moved.join("README.txt")).unwrap().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_minute_folder_refuses_a_different_plan_id() {
        let fx = Fixture::new("rootfs-apply-minute-collision");
        let first = fake_plan(&fx, vec![fx.write("/.SynoUpgrade.tar", b"first")]);
        execute(
            &first,
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut FakeTools::new(),
            &mut StopAfter(usize::MAX),
        )
        .unwrap();

        let mut payload = crate::rootfs::plan::testing::payload();
        payload.plan_id = "rootfs-18d75b8423f30000-4e".into();
        payload.root_device = std::fs::metadata(&fx.root).unwrap().dev();
        let destination = std::fs::metadata(&fx.moves).unwrap();
        payload.destination.path = fx.moves.as_os_str().as_bytes().to_vec();
        payload.destination.device = destination.dev();
        payload.destination.inode = destination.ino();
        payload.items = vec![fx.write("/var/crash/core.2", b"second")];
        let second = RootfsPlan::seal(payload).unwrap();

        let error = execute(
            &second,
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut FakeTools::new(),
            &mut StopAfter(usize::MAX),
        )
        .unwrap_err();

        assert_eq!(error.code(), "destination_changed");
        assert!(exists(&fx.root.join("var/crash/core.2")));
    }

    #[test]
    fn executing_needs_room_only_for_the_files_still_to_move() {
        let fx = Fixture::new("rootfs-apply-remaining");
        let mut gone = fx.write("/var/crash/core.1", b"moved by an earlier run");
        std::fs::remove_file(fx.root.join("var/crash/core.1")).unwrap();
        gone.identity.size = u64::MAX / 4;
        let plan = fake_plan(&fx, vec![gone, fx.write("/var/crash/core.2", b"core")]);

        let done = execute(
            &plan,
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut FakeTools::new(),
            &mut StopAfter(usize::MAX),
        )
        .unwrap();

        assert_eq!((done.moved_files, done.skipped), (1, 1));
        let mut big = fx.write("/var/crash/core.3", b"core");
        big.allocated_bytes = u64::MAX / 4;
        let error = execute(
            &fake_plan(&fx, vec![big]),
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut FakeTools::new(),
            &mut StopAfter(usize::MAX),
        )
        .unwrap_err();
        assert_eq!(error.code(), "destination_full");
        assert!(exists(&fx.root.join("var/crash/core.3")));
    }

    #[test]
    fn a_plan_s_tools_run_after_its_moves_and_not_after_a_stop() {
        let fx = Fixture::new("rootfs-apply-tools");
        let mut plan = fake_plan(&fx, vec![fx.write("/.SynoUpgrade.tar", b"x")]);
        plan.payload.delegates = vec![Delegate::Logrotate];
        let plan = RootfsPlan::seal(plan.payload).unwrap();
        let mut tools = FakeTools::new();

        let done = execute(
            &plan,
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut tools,
            &mut StopAfter(0),
        )
        .unwrap();

        assert_eq!(done.moved_files, 1);
        assert_eq!(done.stopped, Some(StopReason::Paused));
        assert!(tools.ran.is_empty());

        let done = execute(
            &plan,
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut tools,
            &mut StopAfter(usize::MAX),
        )
        .unwrap();
        assert_eq!(tools.ran, [Delegate::Logrotate]);
        assert_eq!(done.tools, [(Delegate::Logrotate, ToolOutcome::Succeeded)]);
        let manifest =
            std::fs::read_to_string(fx.moves.join(plan.move_dir_name()).join("manifest.jsonl"))
                .unwrap();
        assert!(
            manifest
                .lines()
                .last()
                .unwrap()
                .contains(r#""tool":"logrotate""#),
            "{manifest}"
        );
    }

    #[test]
    fn executing_refuses_another_root_or_destination_than_planned() {
        let fx = Fixture::new("rootfs-apply-identity");
        let plan = fake_plan(&fx, vec![fx.write("/.SynoUpgrade.tar", b"x")]);
        let code = |root: &Path, destination: &Path| {
            execute(
                &plan,
                &File::open(root).unwrap(),
                &File::open(destination).unwrap(),
                nothing_held(),
                &mut FakeTools::new(),
                &mut StopAfter(usize::MAX),
            )
            .unwrap_err()
            .code()
        };
        let elsewhere = crate::spill::testing::scratch("rootfs-apply-elsewhere");
        assert_eq!(code(&fx.root, &elsewhere), "destination_changed");
        let mut other_root = plan.clone();
        other_root.payload.root_device += 1;
        let refusal = execute(
            &other_root,
            &File::open(&fx.root).unwrap(),
            &File::open(&fx.moves).unwrap(),
            nothing_held(),
            &mut FakeTools::new(),
            &mut StopAfter(usize::MAX),
        )
        .unwrap_err();
        assert_eq!(refusal.code(), "root_changed");
        assert!(exists(&fx.root.join(".SynoUpgrade.tar")));
    }
}
