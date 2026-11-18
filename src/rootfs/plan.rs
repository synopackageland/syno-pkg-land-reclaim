//! `rootfs plan`: which walked candidates to move off `/`, and the
//! sealed plan `rootfs apply` carries out.

use crate::guard::Refusal;
use crate::output::{escape_human, human_bytes};
use crate::rootfs::classify::{Category, Kind, classify, classify_deep};
use crate::rootfs::delegate::Delegate;
use crate::rootfs::walk::{Candidate, Pinned, PinnedEntry};
use crate::sys;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const PLAN_FORMAT: u32 = 1;
/// Explicit, closed cleanup policy. Omitted in old plans means standard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    #[default]
    Standard,
    Deep,
}

impl CleanupMode {
    pub fn is_standard(&self) -> bool {
        *self == Self::Standard
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }
}

/// How long `rootfs apply` accepts a plan; each item is re-checked anyway.
pub const PLAN_LIFETIME_SECONDS: u64 = crate::guard::PLAN_LIFETIME_SECONDS;

/// Kept free on the destination volume beyond the moved bytes.
pub const DESTINATION_MARGIN_BYTES: u64 = 256 << 20;

/// Folder names below a share that are never a destination: its Recycle
/// Bin and snapshots, and DSM's or packages' `@` and `#` folders.
fn protected_folder(name: &[u8]) -> bool {
    name.starts_with(b"@") || name.starts_with(b"#")
}

/// A plan `rootfs apply` carries out as made: its checksum guards
/// against corruption, not tampering (the file is root's, 0600).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsPlan {
    #[serde(flatten)]
    pub payload: PlanPayload,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPayload {
    pub format: u32,
    #[serde(default, skip_serializing_if = "CleanupMode::is_standard")]
    pub mode: CleanupMode,
    pub plan_id: String,
    pub created_at: u64,
    pub expires_at: u64,
    /// `st_dev` of `/` when planned.
    pub root_device: u64,
    pub destination: Destination,
    /// Sorted by path; folders come before what they hold.
    pub items: Vec<Candidate>,
    /// DSM's own cleaners to run after the moves, in order.
    pub delegates: Vec<Delegate>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_estimates: BTreeMap<Delegate, u64>,
}

/// The `--to` folder, as opened when planned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Destination {
    #[serde(with = "crate::report::hex_bytes")]
    pub path: Vec<u8>,
    #[serde(with = "crate::report::hex_bytes")]
    pub share: Vec<u8>,
    pub device: u64,
    pub inode: u64,
}

impl RootfsPlan {
    pub fn seal(payload: PlanPayload) -> Result<RootfsPlan, serde_json::Error> {
        let checksum = checksum(&payload)?;
        Ok(RootfsPlan { payload, checksum })
    }

    /// Refuses a plan whose payload no longer matches its checksum, of
    /// another format, or outside its lifetime at `now` (Unix seconds).
    pub fn validate(&self, now: u64) -> Result<(), Refusal> {
        let invalid =
            |detail: &str| Refusal::new("plan_invalid", format!("plan_invalid: {detail}"));
        if checksum(&self.payload).ok().as_ref() != Some(&self.checksum) {
            return Err(invalid("its checksum does not match; make a new plan"));
        }
        if self.payload.format != PLAN_FORMAT {
            return Err(invalid(
                "another synoreclaim version made it; make a new plan",
            ));
        }
        if !valid_plan_id(&self.payload.plan_id) {
            return Err(invalid("its plan id is not one rootfs plan makes"));
        }
        if !is_normalised_absolute(&self.payload.destination.path) {
            return Err(invalid("its destination is not a normalised absolute path"));
        }
        if let Some(item) = self
            .payload
            .items
            .iter()
            .find(|item| !allowlisted(item, self.payload.mode))
        {
            return Err(invalid(&format!(
                "{} is not on the rootfs allowlist of this synoreclaim",
                escape_human(&item.path)
            )));
        }
        if self.payload.mode.is_standard()
            && self.payload.delegates.iter().any(|tool| {
                matches!(
                    tool,
                    Delegate::CleanDownloadedUpdate
                        | Delegate::ReleaseUpdateReserve
                        | Delegate::DeleteLog
                )
            })
        {
            return Err(invalid("KB cleaners require deep mode"));
        }
        if self.payload.created_at > now || self.payload.expires_at < now {
            return Err(Refusal::stale("the plan expired; make a new one"));
        }
        Ok(())
    }

    /// What the items take on the destination: each one's apparent size
    /// or its allocated bytes, whichever is larger (a sparse file may
    /// copy at its full size), saturating instead of wrapping.
    pub fn bytes(&self) -> u64 {
        self.payload
            .items
            .iter()
            .map(|c| c.identity.size.max(c.allocated_bytes))
            .fold(0, u64::saturating_add)
    }

    /// The readable, minute-resolution folder name derived from plan time.
    pub fn move_dir_name(&self) -> String {
        crate::time::rootfs_move_dir_name(UNIX_EPOCH + Duration::from_secs(self.payload.created_at))
    }

    /// Where apply moves the items, holding each item at its path on `/`.
    pub fn move_dir(&self) -> Vec<u8> {
        let mut dir = self.payload.destination.path.clone();
        dir.push(b'/');
        dir.extend_from_slice(self.move_dir_name().as_bytes());
        dir
    }
}

/// Where plans are kept: root's own folder on `/`, never in a share,
/// where share users could edit or forge one.
pub const PLAN_DIR: &str = "/var/lib/synoreclaim/rootfs-plans";

/// Opens the plan folder `dir`, creating it (and its parents) 0700 if
/// missing. It must be a folder on `root_device` owned by `owner` that
/// no one else may enter, reached through no symlink.
pub fn open_plan_dir(dir: &Path, root_device: u64, owner: u32) -> Result<File, Refusal> {
    let untrusted = |detail: String| {
        Refusal::new(
            "plan_dir_untrusted",
            format!("{}: {detail}", escape_human(dir.as_os_str().as_bytes())),
        )
    };
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| untrusted(e.to_string()))?;
    let file = crate::guard::open_absolute_no_follow(dir).map_err(|e| untrusted(e.to_string()))?;
    let meta = file.metadata().map_err(|e| untrusted(e.to_string()))?;
    if !meta.is_dir() || meta.dev() != root_device {
        return Err(untrusted("not a folder on the system partition".into()));
    }
    if meta.uid() != owner || meta.mode() & 0o077 != 0 {
        return Err(untrusted(
            "others may reach it; it must be root's, mode 0700".into(),
        ));
    }
    Ok(file)
}

/// Writes `plan` as `<plan id>.json` into the open plan folder: a new
/// file (never replacing one, never through a symlink), mode 0600,
/// synced. Returns its name. A plan `read_plan` would refuse as too
/// large is not written.
pub fn write_plan(destination: &File, plan: &RootfsPlan) -> io::Result<String> {
    let text = encode_plan(plan, MAX_PLAN_BYTES)?;
    let name = format!("{}.json", plan.payload.plan_id);
    let mut file = sys::openat(
        destination.as_fd(),
        OsStr::new(&name),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    )?;
    sys::set_mode(file.as_fd(), 0o600)?;
    file.write_all(&text)?;
    file.sync_all()?;
    Ok(name)
}

/// `plan` as the text of its file, refused when longer than `limit`.
fn encode_plan(plan: &RootfsPlan, limit: u64) -> io::Result<Vec<u8>> {
    let mut text = serde_json::to_vec_pretty(plan)?;
    text.push(b'\n');
    if text.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the plan lists {} items, more than one plan can hold; free some space by hand or narrow the cleanup and plan again",
                plan.payload.items.len()
            ),
        ));
    }
    Ok(text)
}

/// The fixed table still lets `rootfs plan` move `item`, as the kind of
/// file it was planned as.
fn allowlisted(item: &Candidate, mode: CleanupMode) -> bool {
    let file_type = match item.kind {
        Kind::File => libc::S_IFREG,
        Kind::Dir => libc::S_IFDIR,
    };
    item.identity.file_type == file_type
        && if mode == CleanupMode::Deep {
            classify_deep(&item.path, item.kind).movable
        } else {
            classify(&item.path, item.kind).movable
        }
}

/// Plans larger than this are refused unread.
const MAX_PLAN_BYTES: u64 = 16 << 20;

/// The name of the plan file `path` in the plan folder `dir`: only a
/// `<plan id>.json` directly in it names a plan (a plan anywhere else,
/// such as a shared folder, may have been written by anyone).
pub fn plan_name<'a>(path: &'a Path, dir: &Path) -> Result<&'a OsStr, Refusal> {
    let name = path
        .file_name()
        .filter(|_| path.parent() == Some(dir))
        .filter(|name| {
            name.to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .is_some_and(valid_plan_id)
        });
    name.ok_or_else(|| {
        Refusal::new(
            "plan_untrusted",
            format!(
                "{}: plans are read only from {}; make a new plan with rootfs plan",
                escape_human(path.as_os_str().as_bytes()),
                escape_human(dir.as_os_str().as_bytes())
            ),
        )
    })
}

/// Reads the plan `name` in the open plan folder `plans`: a regular file
/// reached without a symlink, owned by `owner` and private to it (as
/// `write_plan` leaves it), holding the plan of that name. Whether it is
/// still valid is `validate`'s question.
pub fn read_plan(plans: &File, name: &OsStr, owner: u32) -> Result<RootfsPlan, Refusal> {
    let untrusted = |detail: String| {
        Refusal::new(
            "plan_untrusted",
            format!(
                "{}: {detail}; make a new plan with rootfs plan",
                escape_human(name.as_bytes())
            ),
        )
    };
    let file = sys::openat(
        plans.as_fd(),
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )
    .map_err(|e| untrusted(format!("cannot open it without following a symlink ({e})")))?;
    let meta = file.metadata().map_err(|e| untrusted(e.to_string()))?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
        return Err(untrusted(format!(
            "not a regular file private to uid {owner}"
        )));
    }
    if meta.len() > MAX_PLAN_BYTES {
        return Err(untrusted("larger than any plan".into()));
    }
    let mut text = Vec::new();
    (&file)
        .read_to_end(&mut text)
        .map_err(|e| untrusted(e.to_string()))?;
    let plan: RootfsPlan = serde_json::from_slice(&text).map_err(|e| {
        Refusal::new(
            "plan_invalid",
            format!("plan_invalid: not a rootfs plan ({e}); make a new plan"),
        )
    })?;
    if format!("{}.json", plan.payload.plan_id).as_bytes() != name.as_bytes() {
        return Err(untrusted("it holds another plan than its name".into()));
    }
    Ok(plan)
}

fn checksum(payload: &PlanPayload) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(payload)?;
    Ok(format!("{:016x}", crate::guard::fnv1a64(&bytes)))
}

/// Unique across invocations: the time in nanoseconds and the pid.
pub fn new_plan_id(now: SystemTime, pid: u32) -> String {
    let nanos = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("rootfs-{nanos:x}-{pid:x}")
}

/// A movable file that a process still holds open: left on `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub path: Vec<u8>,
    pub bytes: u64,
    pub holder: Vec<u8>,
}

/// Junior-mode log archives smaller than this are left: they free
/// little, and support reads them when an update failed.
pub const JUNIOR_LOG_MIN_BYTES: u64 = 1 << 20;
/// `/var/log` archives younger than this are left: the recent history is
/// what support asks for first. `disk-latency` archives are exempt.
pub const LOG_ARCHIVE_MIN_AGE_SECONDS: i64 = 7 * 24 * 60 * 60;

/// A file larger than this stays: DSM's whole system partition is
/// smaller (8 GiB on DSM 7), so only a sparse file can be, and reading
/// its holes to hash them would take long.
pub const MAX_ITEM_BYTES: u64 = 8 << 30;

/// Why a movable file is left on `/` by choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptReason {
    /// A `/var/log` `*.xz` archive modified in the last 7 days.
    RecentLogArchive,
    /// A `/.log.junior/logs.tar.*` smaller than 1 MiB.
    SmallJuniorLog,
    /// An update leftover (not a junior log) modified or changed since
    /// the last boot: it may be an installed update waiting for the restart.
    UpdatedSinceBoot,
    /// A file larger than `MAX_ITEM_BYTES`.
    TooLarge,
    /// Append-only (`chattr +a`): the kernel refuses to unlink it.
    AppendOnly,
    /// Immutable (`chattr +i`), or below an immutable folder: the kernel
    /// refuses to unlink it.
    Immutable,
    UnrecognisedImage,
    /// Hyper Backup's restore installer while `/` would have room for a DSM
    /// update without moving it.
    UpdateRoomEnough,
}

impl From<Pinned> for KeptReason {
    fn from(pinned: Pinned) -> Self {
        match pinned {
            Pinned::UnrecognisedImage => KeptReason::UnrecognisedImage,
            Pinned::AppendOnly => KeptReason::AppendOnly,
            Pinned::Immutable => KeptReason::Immutable,
        }
    }
}

/// A movable file the plan leaves on `/` by its size or age.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub path: Vec<u8>,
    pub bytes: u64,
    pub reason: KeptReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub items: Vec<Candidate>,
    pub held: Vec<Held>,
    pub kept: Vec<Kept>,
}

/// Splits `candidates` (sorted by path, as the walk leaves them) into the
/// items to move, the files a process holds open (`open`, keyed by
/// `(st_dev, st_ino)`), and the files kept by their size or age at `now`
/// or by a change since the last `boot` (Unix seconds). A directory above
/// a held file is left too: it cannot be emptied.
pub fn select(
    candidates: &[Candidate],
    open: &BTreeMap<(u64, u64), Vec<u8>>,
    now: i64,
    boot: i64,
) -> Selection {
    select_mode(candidates, open, now, boot, CleanupMode::Standard)
}

/// Deep only relaxes diagnostic retention; never boot/update, size, open-file
/// or pinned-file safeguards. The walker selects each mode’s fixed path table.
pub fn select_mode(
    candidates: &[Candidate],
    open: &BTreeMap<(u64, u64), Vec<u8>>,
    now: i64,
    boot: i64,
    mode: CleanupMode,
) -> Selection {
    let held: Vec<Held> = candidates
        .iter()
        .filter_map(|c| {
            let holder = open.get(&(c.identity.device, c.identity.inode))?;
            Some(Held {
                path: c.path.clone(),
                bytes: c.allocated_bytes,
                holder: holder.clone(),
            })
        })
        .collect();
    let mut kept = Vec::new();
    let mut items = Vec::new();
    for c in candidates {
        if held
            .iter()
            .any(|h| h.path == c.path || is_below(&h.path, &c.path))
        {
            continue;
        }
        match kept_reason(c, now, boot).filter(|reason| {
            mode != CleanupMode::Deep
                || !matches!(
                    reason,
                    KeptReason::RecentLogArchive | KeptReason::SmallJuniorLog
                )
        }) {
            Some(reason) => kept.push(Kept {
                path: c.path.clone(),
                bytes: c.allocated_bytes,
                reason,
            }),
            None => items.push(c.clone()),
        }
    }
    Selection { items, held, kept }
}

/// Keeps each `pinned` entry, with the same reason for the items below
/// it, and leaves the folders above it: apply could not unlink them.
pub fn keep_pinned(selection: &mut Selection, pinned: &[PinnedEntry]) {
    for p in pinned {
        selection.kept.push(Kept {
            path: p.path.clone(),
            bytes: p.allocated_bytes,
            reason: p.reason.into(),
        });
    }
    let items = std::mem::take(&mut selection.items);
    for c in items {
        if let Some(p) = pinned.iter().find(|p| is_below(&c.path, &p.path)) {
            selection.kept.push(Kept {
                path: c.path.clone(),
                bytes: c.allocated_bytes,
                reason: p.reason.into(),
            });
        } else if !pinned.iter().any(|p| is_below(&p.path, &c.path)) {
            selection.items.push(c);
        }
    }
    selection.kept.sort_by(|a, b| a.path.cmp(&b.path));
}

/// Keeps Hyper Backup's restore installer unless `/`, with `root_available`
/// bytes and the other selected items moved, would still lack the room a
/// typical DSM update needs: support removes it only then, since restoring
/// the entire system needs it until DSM recreates it after an update.
pub fn keep_backup_installer(selection: &mut Selection, root_available: u64) {
    let installer = |c: &Candidate| c.category == Category::BackupInstaller;
    let others = selection
        .items
        .iter()
        .filter(|c| !installer(c))
        .map(|c| c.allocated_bytes)
        .fold(root_available, u64::saturating_add);
    if others < crate::space::SYSTEM_PARTITION_TYPICAL_UPDATE_ROOM_BYTES {
        return;
    }
    let (kept, items) = std::mem::take(&mut selection.items)
        .into_iter()
        .partition::<Vec<_>, _>(installer);
    selection.items = items;
    selection.kept.extend(kept.into_iter().map(|c| Kept {
        path: c.path,
        bytes: c.allocated_bytes,
        reason: KeptReason::UpdateRoomEnough,
    }));
    selection.kept.sort_by(|a, b| a.path.cmp(&b.path));
}

/// Update leftovers changed since `boot`, the size cap, and the survey's
/// size and age limits on two allowlisted kinds of file.
fn kept_reason(c: &Candidate, now: i64, boot: i64) -> Option<KeptReason> {
    let parts: Vec<&[u8]> = c
        .path
        .split(|&b| b == b'/')
        .filter(|p| !p.is_empty())
        .collect();
    // Junior logs are renamed at every boot: only update payloads count.
    if c.category == Category::UpdateLeftover
        && parts.first() != Some(&&b".log.junior"[..])
        && (c.identity.mtime >= boot || c.identity.ctime >= boot)
    {
        return Some(KeptReason::UpdatedSinceBoot);
    }
    if c.kind != Kind::File {
        return None;
    }
    if c.identity.size > MAX_ITEM_BYTES {
        return Some(KeptReason::TooLarge);
    }
    match parts.as_slice() {
        [b".log.junior", _] if c.identity.size < JUNIOR_LOG_MIN_BYTES => {
            Some(KeptReason::SmallJuniorLog)
        }
        [b"var", b"log", b"disk-latency" | b"healthtest", ..] => None,
        [b"var", b"log", .., name]
            if name.ends_with(b".xz")
                && now.saturating_sub(c.identity.mtime) < LOG_ARCHIVE_MIN_AGE_SECONDS =>
        {
            Some(KeptReason::RecentLogArchive)
        }
        _ => None,
    }
}

/// The shared folder (one of `shares`, from the volumes' subvolumes) that
/// the destination `path` is or lies in. `device` is the destination's
/// `st_dev`; a folder on the system partition itself (`root_device`), as
/// when a volume is not mounted, is refused.
pub fn destination_share(
    path: &[u8],
    shares: &[Vec<u8>],
    device: u64,
    root_device: u64,
) -> Result<Vec<u8>, Refusal> {
    if device == root_device {
        return Err(Refusal::new(
            "destination_on_system_partition",
            format!(
                "{} is on the system partition; --to must be in a shared folder on a data volume",
                escape_human(path)
            ),
        ));
    }
    let share = shares
        .iter()
        .find(|share| share.as_slice() == path || is_below(path, share))
        .ok_or_else(|| {
            Refusal::new(
                "destination_not_in_share",
                format!(
                    "{} is not in a DSM shared folder; pass --to /volumeN/<shared folder>/<folder>",
                    escape_human(path)
                ),
            )
        })?;
    if path[share.len()..]
        .split(|b| *b == b'/')
        .any(protected_folder)
    {
        return Err(Refusal::new(
            "destination_protected",
            format!(
                "{} is in a Recycle Bin, a snapshot or a DSM folder; choose a plain folder",
                escape_human(path)
            ),
        ));
    }
    Ok(share.clone())
}

/// Refuses a destination whose volume or share quota has less than the
/// moved bytes plus `DESTINATION_MARGIN_BYTES` available.
pub fn check_space(needed: u64, available: u64) -> Result<(), Refusal> {
    check_space_with_quota(needed, available, None)
}

pub fn check_space_with_quota(
    needed: u64,
    volume_available: u64,
    share_quota_available: Option<u64>,
) -> Result<(), Refusal> {
    let wanted = needed.saturating_add(DESTINATION_MARGIN_BYTES);
    if let Some(available) = share_quota_available
        && available < wanted
    {
        return Err(Refusal::new(
            "destination_full",
            format!(
                "the share quota has {} available; moving needs {} with the margin",
                human_bytes(available),
                human_bytes(wanted)
            ),
        )
        .with_limit("share_quota"));
    }
    if volume_available < wanted {
        return Err(Refusal::new(
            "destination_full",
            format!(
                "the destination has {} available; moving needs {} with the margin",
                human_bytes(volume_available),
                human_bytes(wanted)
            ),
        ));
    }
    Ok(())
}

/// `rootfs-<hex>-<hex>`, as [`new_plan_id`] makes it: safe as a file and
/// folder name.
pub fn valid_plan_id(id: &str) -> bool {
    let hex = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    match id
        .strip_prefix("rootfs-")
        .map(|rest| rest.split('-').collect::<Vec<_>>())
    {
        Some(parts) => parts.len() == 2 && parts.iter().all(|p| hex(p)),
        None => false,
    }
}

/// An absolute path with no empty, `.` or `..` component and no
/// trailing slash, other than `/` itself.
fn is_normalised_absolute(path: &[u8]) -> bool {
    match path.strip_prefix(b"/") {
        Some(rest) if !rest.is_empty() => rest
            .split(|b| *b == b'/')
            .all(|part| !matches!(part, b"" | b"." | b"..")),
        _ => false,
    }
}

/// Whether `path` lies strictly below `dir`.
fn is_below(path: &[u8], dir: &[u8]) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.first() == Some(&b'/'))
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::rootfs::classify::{Category, Kind};
    use crate::rootfs::walk::Identity;

    pub fn candidate(path: &str, kind: Kind, inode: u64) -> Candidate {
        Candidate {
            path: path.as_bytes().to_vec(),
            kind,
            category: Category::UpdateLeftover,
            identity: Identity {
                device: 2049,
                inode,
                file_type: match kind {
                    Kind::Dir => libc::S_IFDIR,
                    Kind::File => libc::S_IFREG,
                },
                size: 100,
                mtime: 1_790_000_000,
                mtime_nsec: 0,
                nlink: 1,
                ctime: 0,
            },
            allocated_bytes: 4096,
        }
    }

    pub fn payload() -> PlanPayload {
        PlanPayload {
            mode: CleanupMode::Standard,
            tool_estimates: BTreeMap::new(),
            format: PLAN_FORMAT,
            plan_id: new_plan_id(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000),
                77,
            ),
            created_at: 1_790_000_000,
            expires_at: 1_790_000_000 + PLAN_LIFETIME_SECONDS,
            root_device: 2049,
            destination: Destination {
                path: b"/volume1/backup/rootfs".to_vec(),
                share: b"/volume1/backup".to_vec(),
                device: 44,
                inode: 300,
            },
            items: vec![
                candidate("/upd@te/a", Kind::File, 11),
                candidate("/upd@te/old", Kind::Dir, 13),
            ],
            delegates: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{candidate, payload};
    use super::*;
    use crate::rootfs::classify::{Category, Kind};

    #[test]
    fn hyper_backup_s_installer_moves_only_while_a_dsm_update_would_not_fit() {
        const ROOM: u64 = crate::space::SYSTEM_PARTITION_TYPICAL_UPDATE_ROOM_BYTES;
        let mut spk = candidate(
            "/var/synobackup/HyperBackup-ED/HyperBackup-ED.latest.spk",
            Kind::File,
            40,
        );
        spk.category = Category::BackupInstaller;
        spk.allocated_bytes = 19 << 20;
        let mut log = candidate("/var/log/a.xz", Kind::File, 41);
        log.category = Category::LogArchive;
        log.allocated_bytes = 100 << 20;
        let selection = || Selection {
            items: vec![log.clone(), spk.clone()],
            held: vec![],
            kept: vec![],
        };

        let mut short = selection();
        keep_backup_installer(&mut short, ROOM - (100 << 20) - 1);
        assert_eq!(short.items.len(), 2, "still short after the other moves");
        assert!(short.kept.is_empty());

        let mut enough = selection();
        keep_backup_installer(&mut enough, ROOM - (100 << 20));
        assert_eq!(enough.items, [log.clone()]);
        assert_eq!(
            enough.kept,
            [Kept {
                path: spk.path.clone(),
                bytes: 19 << 20,
                reason: KeptReason::UpdateRoomEnough,
            }]
        );
    }

    #[test]
    fn kb_items_cannot_be_authorized_as_standard_items() {
        let mut payload = testing::payload();
        let mut item = payload.items[0].clone();
        item.path = b"/var/db/geoip-database/cache".to_vec();
        item.kind = Kind::File;
        item.category = crate::rootfs::classify::Category::GeoipDatabase;
        assert!(!allowlisted(&item, CleanupMode::Standard));
        assert!(allowlisted(&item, CleanupMode::Deep));
        payload.items = vec![item];
    }

    #[test]
    fn a_standard_plan_cannot_carry_dsm_s_old_log_deletion() {
        let mut payload = testing::payload();
        payload.delegates = vec![Delegate::DeleteLog];
        let now = payload.created_at;
        let standard = RootfsPlan::seal(payload.clone()).unwrap();
        assert_eq!(standard.validate(now).unwrap_err().code, "plan_invalid");
        payload.mode = CleanupMode::Deep;
        assert!(RootfsPlan::seal(payload).unwrap().validate(now).is_ok());
    }

    #[test]
    fn held_open_files_and_the_directories_above_them_stay_on_root() {
        let candidates = [
            candidate("/upd@te", Kind::Dir, 10),
            candidate("/upd@te/a", Kind::File, 11),
            candidate("/upd@te/b", Kind::File, 12),
            candidate("/upd@te2", Kind::Dir, 13),
            candidate("/var/crash/core", Kind::File, 14),
        ];
        let open = BTreeMap::from([((2049, 12), b"synoupgrade".to_vec())]);
        let selection = select(&candidates, &open, 1_790_000_000, NEVER_BOOTED);
        let paths: Vec<&[u8]> = selection.items.iter().map(|c| c.path.as_slice()).collect();
        assert_eq!(paths, [&b"/upd@te/a"[..], b"/upd@te2", b"/var/crash/core"]);
        assert_eq!(
            selection.held,
            [Held {
                path: b"/upd@te/b".to_vec(),
                bytes: 4096,
                holder: b"synoupgrade".to_vec(),
            }]
        );
    }

    #[test]
    fn recent_log_archives_and_small_junior_logs_stay_on_root_with_a_reason() {
        let now = 1_790_000_000;
        let aged = |path: &str, inode, age: i64, size| {
            let mut c = candidate(path, Kind::File, inode);
            c.identity.mtime = now - age;
            c.identity.size = size;
            c
        };
        let candidates = [
            aged("/.log.junior/logs.tar.1", 1, 0, JUNIOR_LOG_MIN_BYTES - 1),
            aged("/.log.junior/logs.tar.2", 2, 0, JUNIOR_LOG_MIN_BYTES),
            aged("/var/log/disk-latency/a.xz", 3, 0, 1),
            aged("/var/log/healthtest/dhm1/b.xz", 4, 0, 1),
            aged(
                "/var/log/messages.1.xz",
                5,
                LOG_ARCHIVE_MIN_AGE_SECONDS - 1,
                1,
            ),
            aged("/var/log/messages.2.xz", 6, LOG_ARCHIVE_MIN_AGE_SECONDS, 1),
            aged("/var/log/samba/c.xz", 7, -60, 1),
        ];
        let selection = select(&candidates, &BTreeMap::new(), now, NEVER_BOOTED);
        let paths: Vec<&[u8]> = selection.items.iter().map(|c| c.path.as_slice()).collect();
        assert_eq!(
            paths,
            [
                &b"/.log.junior/logs.tar.2"[..],
                b"/var/log/disk-latency/a.xz",
                b"/var/log/healthtest/dhm1/b.xz",
                b"/var/log/messages.2.xz",
            ]
        );
        let kept: Vec<(&[u8], KeptReason)> = selection
            .kept
            .iter()
            .map(|k| (k.path.as_slice(), k.reason))
            .collect();
        assert_eq!(
            kept,
            [
                (&b"/.log.junior/logs.tar.1"[..], KeptReason::SmallJuniorLog),
                (b"/var/log/messages.1.xz", KeptReason::RecentLogArchive),
                (b"/var/log/samba/c.xz", KeptReason::RecentLogArchive),
            ]
        );
        assert_eq!(selection.kept[0].bytes, 4096);
    }

    #[test]
    fn append_only_or_immutable_entries_are_kept_with_what_they_pin() {
        // The walk lists a pinned entry in `pinned`, never in candidates.
        let candidates = [
            candidate("/upd@te", Kind::Dir, 1),
            candidate("/upd@te/locked/a", Kind::File, 3),
            candidate("/upd@te/free", Kind::File, 4),
        ];
        let mut selection = select(&candidates, &BTreeMap::new(), 0, NEVER_BOOTED);
        let pinned = [
            PinnedEntry {
                path: b"/upd@te/locked".to_vec(),
                reason: Pinned::Immutable,
                allocated_bytes: 4096,
            },
            PinnedEntry {
                path: b"/var/log/rm.log.1.xz".to_vec(),
                reason: Pinned::AppendOnly,
                allocated_bytes: 8192,
            },
        ];

        keep_pinned(&mut selection, &pinned);

        let paths: Vec<&[u8]> = selection.items.iter().map(|c| c.path.as_slice()).collect();
        assert_eq!(paths, [&b"/upd@te/free"[..]]);
        let kept: Vec<(&[u8], u64, KeptReason)> = selection
            .kept
            .iter()
            .map(|k| (k.path.as_slice(), k.bytes, k.reason))
            .collect();
        assert_eq!(
            kept,
            [
                (&b"/upd@te/locked"[..], 4096, KeptReason::Immutable),
                (b"/upd@te/locked/a", 4096, KeptReason::Immutable),
                (b"/var/log/rm.log.1.xz", 8192, KeptReason::AppendOnly),
            ]
        );
    }

    #[test]
    fn mode_is_sealed_and_old_standard_plans_keep_their_checksum() {
        let old = RootfsPlan::seal(testing::payload()).unwrap();
        let json = serde_json::to_value(&old).unwrap();
        assert!(json.get("mode").is_none());
        let restored: RootfsPlan = serde_json::from_value(json).unwrap();
        assert_eq!(restored.payload.mode, CleanupMode::Standard);
        assert_eq!(restored, old);
        let mut deep = old.clone();
        deep.payload.mode = CleanupMode::Deep;
        assert_ne!(deep.checksum, checksum(&deep.payload).unwrap());
        assert!(deep.validate(deep.payload.created_at).is_err());
        let sealed = RootfsPlan::seal(deep.payload).unwrap();
        assert!(sealed.validate(sealed.payload.created_at).is_ok());
        assert_eq!(serde_json::to_value(&sealed).unwrap()["mode"], "deep");
    }

    #[test]
    fn deep_mode_adds_recent_archives_and_small_junior_logs_but_keeps_safety_limits() {
        let now = 1_790_000_000;
        let boot = now - 100;
        let mut recent = candidate("/var/log/messages.1.xz", Kind::File, 1);
        recent.category = Category::LogArchive;
        recent.identity.mtime = now - 10;
        let mut junior = candidate("/.log.junior/logs.tar.1", Kind::File, 2);
        junior.identity.size = 512;
        let mut update = candidate("/upd@te/payload", Kind::File, 3);
        update.identity.mtime = now;
        let mut huge = candidate("/var/crash/core", Kind::File, 4);
        huge.category = Category::CrashDump;
        huge.identity.size = MAX_ITEM_BYTES + 1;
        let candidates = [recent, junior, update, huge];
        let standard = select(&candidates, &BTreeMap::new(), now, boot);
        assert!(standard.items.is_empty());
        let deep = select_mode(&candidates, &BTreeMap::new(), now, boot, CleanupMode::Deep);
        assert_eq!(deep.items.len(), 2);
        assert_eq!(deep.kept.len(), 2);
        let open = BTreeMap::from([(
            (candidates[0].identity.device, candidates[0].identity.inode),
            b"writer".to_vec(),
        )]);
        let deep = select_mode(&candidates, &open, now, boot, CleanupMode::Deep);
        assert_eq!(deep.items.len(), 1);
        assert_eq!(deep.held.len(), 1);
    }

    /// A boot time after every test candidate's times.
    const NEVER_BOOTED: i64 = i64::MAX;

    #[test]
    fn update_leftovers_changed_since_boot_and_files_over_the_cap_stay_on_root_with_a_reason() {
        let boot = 1_790_000_000;
        let at = |path: &str, inode, mtime, ctime| {
            let mut c = candidate(path, Kind::File, inode);
            c.identity.mtime = mtime;
            c.identity.ctime = ctime;
            c
        };
        let mut crash = at("/var/crash/core", 4, boot + 5, boot + 5);
        crash.category = Category::CrashDump;
        let mut huge = at("/var/crash/huge", 5, 0, 0);
        huge.identity.size = MAX_ITEM_BYTES + 1;
        let mut limit = at("/var/crash/limit", 6, 0, 0);
        limit.identity.size = MAX_ITEM_BYTES;
        let mut new_dir = candidate("/upd@te/newdir", Kind::Dir, 7);
        new_dir.identity.mtime = boot;
        let candidates = [
            at("/upd@te/new", 1, boot, boot),
            at("/upd@te/old", 2, boot - 1, boot - 1),
            at("/upd@te/touched", 3, boot - 10, boot + 5),
            new_dir,
            crash,
            huge,
            limit,
        ];

        let selection = select(&candidates, &BTreeMap::new(), boot + 60, boot);

        let paths: Vec<&[u8]> = selection.items.iter().map(|c| c.path.as_slice()).collect();
        assert_eq!(
            paths,
            [&b"/upd@te/old"[..], b"/var/crash/core", b"/var/crash/limit"]
        );
        let kept: Vec<(&[u8], KeptReason)> = selection
            .kept
            .iter()
            .map(|k| (k.path.as_slice(), k.reason))
            .collect();
        assert_eq!(
            kept,
            [
                (&b"/upd@te/new"[..], KeptReason::UpdatedSinceBoot),
                (b"/upd@te/touched", KeptReason::UpdatedSinceBoot),
                (b"/upd@te/newdir", KeptReason::UpdatedSinceBoot),
                (b"/var/crash/huge", KeptReason::TooLarge),
            ]
        );
    }

    #[test]
    fn junior_logs_rotated_at_boot_are_still_planned() {
        // DSM renames every logs.tar.N to N+1 while it boots (a test NAS: all
        // 24 had ctime btime + 46 s, mtimes back to 2024): they are logs,
        // not an update waiting for the restart.
        let boot = 1_790_000_000;
        let mut rotated = candidate("/.log.junior/logs.tar.12", Kind::File, 1);
        rotated.identity.size = JUNIOR_LOG_MIN_BYTES;
        rotated.identity.mtime = boot - 86_400;
        rotated.identity.ctime = boot + 46;

        let selection = select(&[rotated], &BTreeMap::new(), boot + 60, boot);

        assert_eq!(selection.kept, []);
        assert_eq!(selection.items.len(), 1);
    }

    const SHARES: [&[u8]; 2] = [b"/volume1/backup", b"/volume1/photos"];

    fn share_of(path: &str, device: u64) -> Result<Vec<u8>, &'static str> {
        let shares: Vec<Vec<u8>> = SHARES.iter().map(|s| s.to_vec()).collect();
        destination_share(path.as_bytes(), &shares, device, 2049).map_err(|r| r.code)
    }

    #[test]
    fn a_destination_is_a_shared_folder_or_below_it_on_a_data_volume() {
        assert_eq!(
            share_of("/volume1/backup/rootfs", 44),
            Ok(b"/volume1/backup".to_vec())
        );
        assert_eq!(
            share_of("/volume1/photos", 44),
            Ok(b"/volume1/photos".to_vec())
        );
        assert_eq!(
            share_of("/volume1/backupx/a", 44),
            Err("destination_not_in_share")
        );
        assert_eq!(share_of("/volume1", 44), Err("destination_not_in_share"));
        assert_eq!(
            share_of("/root/rootfs", 44),
            Err("destination_not_in_share")
        );
    }

    #[test]
    fn a_destination_on_the_system_partition_is_refused_even_below_a_share_path() {
        // /volume1 not mounted: the share path is a directory on `/`.
        assert_eq!(
            share_of("/volume1/backup/rootfs", 2049),
            Err("destination_on_system_partition")
        );
    }

    #[test]
    fn recycle_bins_snapshots_and_dsm_folders_are_not_destinations() {
        for path in [
            "/volume1/backup/#recycle",
            "/volume1/backup/#recycle/x",
            "/volume1/backup/#snapshot/x",
            "/volume1/backup/a/@eaDir",
            "/volume1/backup/@tmp",
        ] {
            assert_eq!(share_of(path, 44), Err("destination_protected"), "{path}");
        }
    }

    #[test]
    fn a_destination_needs_room_for_the_items_and_a_margin() {
        let needed = 100 << 20;
        assert!(check_space(needed, needed + DESTINATION_MARGIN_BYTES).is_ok());
        let refusal = check_space(needed, needed + DESTINATION_MARGIN_BYTES - 1).unwrap_err();
        assert_eq!(refusal.code, "destination_full");
        assert!(refusal.message.contains("356 MB"), "{}", refusal.message);
    }

    #[test]
    fn a_share_quota_that_is_tighter_than_the_volume_sets_the_limit_reason() {
        let needed = 100 << 20;
        let refusal = check_space_with_quota(
            needed,
            20 << 30,
            Some(needed + DESTINATION_MARGIN_BYTES - 1),
        )
        .unwrap_err();

        assert_eq!(refusal.code, "destination_full");
        assert_eq!(refusal.limit, Some("share_quota"));
        assert!(refusal.message.contains("share quota"));
    }

    #[test]
    fn a_share_root_is_a_valid_plan_destination() {
        assert_eq!(
            destination_share(b"/volume1/backup", &[b"/volume1/backup".to_vec()], 44, 1).unwrap(),
            b"/volume1/backup"
        );
    }

    #[test]
    fn a_sealed_plan_round_trips_and_names_where_its_files_go() {
        let plan = RootfsPlan::seal(payload()).unwrap();
        assert_eq!(plan.payload.plan_id, "rootfs-18d75b8423f30000-4d");
        assert_eq!(plan.bytes(), 8192);
        assert_eq!(
            plan.move_dir(),
            b"/volume1/backup/rootfs/SynoReclaim-system-20260921-1413".to_vec()
        );
        let text = serde_json::to_string(&plan).unwrap();
        assert!(text.contains(&crate::report::hex_bytes::encode(b"/upd@te/a")));
        let back: RootfsPlan = serde_json::from_str(&text).unwrap();
        assert_eq!(back, plan);
        assert!(back.validate(1_790_000_001).is_ok());
    }

    #[test]
    fn a_plan_names_its_dsm_tool_runs_only_by_their_fixed_variant() {
        use crate::rootfs::delegate::Delegate;
        let mut runs = payload();
        runs.delegates = vec![Delegate::RotateLogs, Delegate::RestartSyslogNg];
        let plan = RootfsPlan::seal(runs).unwrap();
        let text = serde_json::to_string(&plan).unwrap();
        assert!(
            text.contains(r#""delegates":["rotate_logs","restart_syslog_ng"]"#),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<RootfsPlan>(&text).unwrap(), plan);
        let argv = text.replace("rotate_logs", "/bin/sh");
        assert!(serde_json::from_str::<RootfsPlan>(&argv).is_err());
    }

    #[test]
    fn a_changed_expired_or_foreign_plan_is_refused() {
        let code = |plan: &RootfsPlan, now| plan.validate(now).unwrap_err().code;
        let plan = RootfsPlan::seal(payload()).unwrap();
        assert_eq!(
            code(&plan, 1_790_000_000 + PLAN_LIFETIME_SECONDS + 1),
            "plan_stale"
        );
        assert_eq!(code(&plan, 1_789_999_999), "plan_stale");
        let mut edited = plan.clone();
        edited.payload.items[0].path = b"/etc/passwd".to_vec();
        assert_eq!(code(&edited, 1_790_000_001), "plan_invalid");
        let mut outside = payload();
        outside.items[0].path = b"/etc/shadow".to_vec();
        assert_eq!(
            code(&RootfsPlan::seal(outside).unwrap(), 1_790_000_001),
            "plan_invalid"
        );
        let mut kind = payload();
        kind.items[0].kind = Kind::Dir;
        assert_eq!(
            code(&RootfsPlan::seal(kind).unwrap(), 1_790_000_001),
            "plan_invalid"
        );
        let mut future = payload();
        future.format = PLAN_FORMAT + 1;
        assert_eq!(
            code(&RootfsPlan::seal(future).unwrap(), 1_790_000_001),
            "plan_invalid"
        );
    }

    #[test]
    fn a_plan_id_other_than_the_generated_form_is_invalid() {
        for id in [
            "../../../../root",
            "rootfs-1-2/../../x",
            "rootfs-1-2/x",
            "rootfs--2",
            "rootfs-1-",
            "rootfs-1-2-3",
            "rootfs-1G-2",
            "",
        ] {
            let mut forged = payload();
            forged.plan_id = id.to_string();
            let plan = RootfsPlan::seal(forged).unwrap();
            assert_eq!(
                plan.validate(1_790_000_001).unwrap_err().code,
                "plan_invalid",
                "{id:?}"
            );
        }
    }

    #[test]
    fn a_destination_path_that_is_not_already_normalised_is_invalid() {
        for path in [
            "/volume1/backup/../../volume2/x",
            "/volume1/backup/./rootfs",
            "/volume1//backup",
            "/volume1/backup/",
            "volume1/backup",
            "/",
            "",
        ] {
            let mut forged = payload();
            forged.destination.path = path.as_bytes().to_vec();
            let plan = RootfsPlan::seal(forged).unwrap();
            assert_eq!(
                plan.validate(1_790_000_001).unwrap_err().code,
                "plan_invalid",
                "{path:?}"
            );
        }
    }

    #[test]
    fn a_sparse_item_counts_its_apparent_size_and_the_total_saturates() {
        let mut sparse = payload();
        sparse.items[0].identity.size = 1 << 40;
        sparse.items[0].allocated_bytes = 0;
        assert_eq!(
            RootfsPlan::seal(sparse.clone()).unwrap().bytes(),
            (1 << 40) + 4096
        );
        sparse.items[1].identity.size = u64::MAX;
        assert_eq!(RootfsPlan::seal(sparse).unwrap().bytes(), u64::MAX);
    }

    #[test]
    fn the_plan_folder_is_created_private_on_root_and_refused_otherwise() {
        use std::os::unix::fs::PermissionsExt;
        let base = crate::spill::testing::scratch("rootfs-plan-dir");
        let device = std::fs::metadata(&base).unwrap().dev();
        let me = sys::effective_uid();
        let dir = base.join("state/rootfs-plans");

        open_plan_dir(&dir, device, me).unwrap();
        let meta = std::fs::metadata(&dir).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        open_plan_dir(&dir, device, me).unwrap();

        let code = |dir: &Path, device, owner| open_plan_dir(dir, device, owner).unwrap_err().code;
        assert_eq!(code(&dir, device + 1, me), "plan_dir_untrusted");
        assert_eq!(code(&dir, device, me + 1), "plan_dir_untrusted");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(code(&dir, device, me), "plan_dir_untrusted");
        let link = base.join("link");
        std::os::unix::fs::symlink(base.join("state"), &link).unwrap();
        assert_eq!(
            code(&link.join("rootfs-plans"), device, me),
            "plan_dir_untrusted"
        );
    }

    #[test]
    fn a_plan_too_large_to_read_back_is_not_written() {
        let plan = RootfsPlan::seal(payload()).unwrap();
        let text = encode_plan(&plan, MAX_PLAN_BYTES).unwrap();
        let back: RootfsPlan = serde_json::from_slice(&text).unwrap();
        assert_eq!(back, plan);

        let error = encode_plan(&plan, text.len() as u64 - 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("more than one plan can hold"),
            "{error}"
        );
        assert!(encode_plan(&plan, text.len() as u64).is_ok());
    }

    #[test]
    fn the_plan_file_is_written_once_into_the_destination_readable_by_root_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::spill::testing::scratch("rootfs-plan-file");
        let plan = RootfsPlan::seal(payload()).unwrap();
        let destination = std::fs::File::open(&dir).unwrap();

        let name = write_plan(&destination, &plan).unwrap();

        assert_eq!(name, "rootfs-18d75b8423f30000-4d.json");
        let path = dir.join(&name);
        let back: RootfsPlan = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, plan);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let again = write_plan(&destination, &plan).unwrap_err();
        assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_plan_file_is_read_only_if_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::spill::testing::scratch("rootfs-plan-read");
        let plans = std::fs::File::open(&dir).unwrap();
        let plan = RootfsPlan::seal(payload()).unwrap();
        let name = write_plan(&plans, &plan).unwrap();
        let name = OsStr::new(&name);
        let path = dir.join(name);
        let uid = crate::sys::effective_uid();

        assert_eq!(read_plan(&plans, name, uid).unwrap(), plan);
        assert_eq!(
            read_plan(&plans, name, uid + 1).unwrap_err().code,
            "plan_untrusted"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            read_plan(&plans, name, uid).unwrap_err().code,
            "plan_untrusted"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = OsStr::new("rootfs-1-2.json");
        std::os::unix::fs::symlink(&path, dir.join(link)).unwrap();
        assert_eq!(
            read_plan(&plans, link, uid).unwrap_err().code,
            "plan_untrusted"
        );
        let junk = OsStr::new("rootfs-3-4.json");
        std::fs::write(dir.join(junk), b"{").unwrap();
        std::fs::set_permissions(dir.join(junk), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            read_plan(&plans, junk, uid).unwrap_err().code,
            "plan_invalid"
        );
    }

    #[test]
    fn a_plan_is_read_only_under_its_own_id() {
        let dir = crate::spill::testing::scratch("rootfs-plan-renamed");
        let plans = std::fs::File::open(&dir).unwrap();
        let plan = RootfsPlan::seal(payload()).unwrap();
        let name = write_plan(&plans, &plan).unwrap();
        let other = OsStr::new("rootfs-5-6.json");
        std::fs::rename(dir.join(&name), dir.join(other)).unwrap();
        let uid = crate::sys::effective_uid();

        assert_eq!(
            read_plan(&plans, other, uid).unwrap_err().code,
            "plan_untrusted"
        );
    }

    #[test]
    fn a_plan_is_named_only_by_a_file_directly_in_the_plan_folder() {
        let dir = Path::new(PLAN_DIR);
        let name = |path: &str| {
            plan_name(Path::new(path), dir)
                .map(OsStr::to_os_string)
                .map_err(|r| r.code)
        };

        assert_eq!(
            name("/var/lib/synoreclaim/rootfs-plans/rootfs-1a-2b.json"),
            Ok("rootfs-1a-2b.json".into())
        );
        let untrusted = Err("plan_untrusted");
        assert_eq!(name("/volume1/share/rootfs-1a-2b.json"), untrusted);
        assert_eq!(
            name("/var/lib/synoreclaim/rootfs-plans/x/rootfs-1a-2b.json"),
            untrusted
        );
        assert_eq!(
            name("/var/lib/synoreclaim/rootfs-plans/notes.json"),
            untrusted
        );
        assert_eq!(
            name("/var/lib/synoreclaim/rootfs-plans/rootfs-1a-2b"),
            untrusted
        );
        assert_eq!(name("/var/lib/synoreclaim/rootfs-plans"), untrusted);
    }
}
