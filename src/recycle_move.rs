//! Shared-folder cleanup moves each selected item into its shared
//! folder's DSM Recycle Bin with DSM's own tool (SPEC 5.7):
//! `synorecycle --rmdir|--unlink share=<name> rpath=<path>`, the library
//! File Station deletes with. Nothing in a shared folder is removed for
//! good: the user restores or empties the Recycle Bin in DSM.
//!
//! An item with no Recycle Bin to move to (in a folder right below the
//! volume that DSM does not list as a shared folder, or in a shared folder
//! whose Recycle Bin is off) is removed for good instead, once the user
//! confirms that ([`delete_target`]; user decisions 2026-11-14).
//!
//! The tool does not check that the Recycle Bin is enabled (it then exits 0
//! doing nothing) and removes an item for good when its path lies in
//! `#recycle` or `@eaDir`; both are refused here first, and every move is
//! checked afterwards with `lstat`.

use crate::filesystem::FilesystemKind;
use crate::guard::{self, Refusal};
use crate::output::escape_human;
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// 5: the targets are rows of a shared-folder table, not hex paths.
pub const PLAN_FORMAT: u32 = 5;
pub const PLAN_LIFETIME_SECONDS: u64 = guard::PLAN_LIFETIME_SECONDS;
/// Paths a partly moved item lists of what stayed behind.
pub const LISTED_REMAINING: usize = 50;
/// Where DSM's Recycle Bin is turned on, for the refusal text.
pub const ENABLE_ROUTE: &str = "Control Panel > Shared Folder > Edit > Enable Recycle Bin";
/// Where DSM empties a Recycle Bin.
pub const EMPTY_ROUTE: &str = "Control Panel > Shared Folder > Action > Empty Recycle Bin, or a Recycle Bin task in Task Scheduler";
const RECYCLE_BIN: &[u8] = b"#recycle";
const HOMES: &[u8] = b"homes";

/// DSM's programs, as absolute paths run without a shell. Each must be
/// a regular file `owner` owns, executable and writable by no one else.
#[derive(Debug, Clone)]
pub struct Tools {
    pub synorecycle: PathBuf,
    pub synoshare: PathBuf,
    pub systemctl: PathBuf,
    pub owner: u32,
    /// Tests: move in this process, as DSM does, below this shared folder,
    /// instead of running `synorecycle`: a test of many items needs no
    /// run of a program per item.
    #[cfg(test)]
    pub in_process: Option<PathBuf>,
}

impl Tools {
    pub fn dsm() -> Tools {
        Tools {
            synorecycle: "/usr/syno/bin/synorecycle".into(),
            synoshare: "/usr/syno/sbin/synoshare".into(),
            systemctl: "/usr/bin/systemctl".into(),
            owner: 0,
            #[cfg(test)]
            in_process: None,
        }
    }

    fn trusted(&self, program: &Path) -> Result<(), Refusal> {
        if crate::rootfs::delegate::trusted_program(program, self.owner) {
            return Ok(());
        }
        Err(Refusal::new(
            "recycle_bin_unavailable",
            format!(
                "recycle_bin_unavailable: {} is not DSM's own program",
                escape_human(program.as_os_str().as_bytes())
            ),
        ))
    }
}

/// A shared folder's Recycle Bin settings, as `synoshare --get` shows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinStatus {
    pub enabled: bool,
    pub admin_only: bool,
}

/// `RecycleBin....[yes]` and `RecycleBinAdminOnly....[yes]` in
/// `synoshare --get` output; `None` without a `RecycleBin` line.
pub fn parse_bin_status(output: &[u8]) -> Option<BinStatus> {
    let mut enabled = None;
    let mut admin_only = false;
    for line in output.split(|b| *b == b'\n') {
        let line = line.trim_ascii();
        let value = |key: &[u8]| {
            let rest = line.strip_prefix(key)?;
            let rest = &rest[rest.iter().take_while(|b| **b == b'.').count()..];
            let rest = std::str::from_utf8(rest).ok()?;
            Some(rest.trim() == "[yes]")
        };
        if let Some(v) = value(b"RecycleBinAdminOnly") {
            admin_only = v;
        } else if let Some(v) = value(b"RecycleBin") {
            enabled = Some(v);
        }
    }
    enabled.map(|enabled| BinStatus {
        enabled,
        admin_only,
    })
}

/// The Recycle Bin settings of shared folder `share`.
pub fn bin_status(tools: &Tools, share: &[u8]) -> Result<BinStatus, Refusal> {
    tools.trusted(&tools.synoshare)?;
    let unknown = |detail: String| {
        Refusal::new(
            "recycle_bin_unavailable",
            format!(
                "recycle_bin_unavailable: the Recycle Bin of {} could not be read: {detail}",
                escape_human(share)
            ),
        )
    };
    let output = run_capture(
        &tools.synoshare,
        &[OsStr::new("--get"), OsStr::from_bytes(share)],
        1 << 16,
    )
    .ok_or_else(|| unknown("synoshare --get failed".into()))?;
    parse_bin_status(&output).ok_or_else(|| unknown("no RecycleBin setting".into()))
}

/// Whether DSM knows `name` as a shared folder (`synoshare --get` exits
/// 0), asked before anything in a folder its mount list lacks is removed
/// for good: a share missing from that list must never lose items for
/// good. An error when DSM's tool cannot be run.
pub fn dsm_knows_share(tools: &Tools, name: &[u8]) -> Result<bool, Refusal> {
    tools.trusted(&tools.synoshare)?;
    match run(
        &tools.synoshare,
        &[OsStr::new("--get"), OsStr::from_bytes(name)],
    ) {
        Some(code) => Ok(code == 0),
        None => Err(Refusal::new(
            "recycle_bin_unavailable",
            format!(
                "recycle_bin_unavailable: DSM could not say whether {} is a shared folder",
                escape_human(name)
            ),
        )),
    }
}

/// Whether DSM's `recyclebind`, which moves deleted items from their
/// hidden staging folder into `#recycle`, is running.
pub fn recyclebind_running(tools: &Tools) -> bool {
    tools.trusted(&tools.systemctl).is_ok()
        && run(
            &tools.systemctl,
            &[
                OsStr::new("is-active"),
                OsStr::new("--quiet"),
                OsStr::new("recyclebind.service"),
            ],
        ) == Some(0)
}

/// Where a selected path lies: its shared folder and its path inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub share: Vec<u8>,
    pub rpath: Vec<u8>,
}

fn protected(reason: &'static str, message: String) -> Refusal {
    let mut refusal = Refusal::new(
        "protected_target",
        format!("protected cleanup target: {message}"),
    );
    refusal.reason = Some(reason);
    refusal
}

/// The shared folder of `path` on the volume mounted at `mount`, and the
/// path inside it. Refuses the volume and shared-folder roots, anything in
/// or being a Recycle Bin, and every `@…`/`#…` name: DSM's tool would
/// remove an item in `#recycle` or `@eaDir` for good.
pub fn locate(path: &[u8], mount: &[u8]) -> Result<Located, Refusal> {
    let below = path
        .strip_prefix(mount)
        .and_then(|rest| rest.strip_prefix(b"/"))
        .filter(|rest| !rest.is_empty())
        .ok_or_else(|| {
            protected(
                "outside_share",
                format!("{} is not inside a shared folder", escape_human(path)),
            )
        })?;
    let parts: Vec<&[u8]> = below.split(|b| *b == b'/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == b"." || *part == b"..")
    {
        return Err(protected(
            "system",
            format!("{} is not a normal path", escape_human(path)),
        ));
    }
    if parts.contains(&RECYCLE_BIN) {
        return Err(Refusal::new(
            "in_recycle_bin",
            format!(
                "in_recycle_bin: {} is in a Recycle Bin; Reclaim never removes items from it: {EMPTY_ROUTE}",
                escape_human(path)
            ),
        ));
    }
    if let Some(name) = parts
        .iter()
        .find(|part| part.starts_with(b"@") || part.starts_with(b"#"))
    {
        return Err(protected(
            "dsm_folder",
            format!(
                "{} passes through the DSM-managed {}",
                escape_human(path),
                escape_human(name)
            ),
        ));
    }
    let (share, rest) = parts
        .split_first()
        .expect("a path below the volume has a name");
    if rest.is_empty() {
        // A folder right below the volume: a shared folder itself, or
        // one outside every shared folder (the plan tells them apart).
        return Err(protected(
            "share_root",
            format!(
                "{} is a shared-folder root: select items inside it",
                escape_human(path)
            ),
        ));
    }
    if *share == HOMES && rest.len() == 1 {
        return Err(protected(
            "home",
            format!(
                "{} is a user home: select items inside it",
                escape_human(path)
            ),
        ));
    }
    Ok(Located {
        share: share.to_vec(),
        rpath: rest.join(&b'/'),
    })
}

/// Whether `path` lies in a user's Synology Drive folder,
/// `homes/<user>/Drive`, whose clients sync the move.
pub fn in_drive(path: &[u8], mount: &[u8]) -> bool {
    let rest = path
        .strip_prefix(mount)
        .and_then(|r| r.strip_prefix(b"/"))
        .unwrap_or(b"");
    let parts: Vec<&[u8]> = rest.split(|&b| b == b'/').collect();
    parts.len() > 2 && parts[0] == HOMES && parts[2] == b"Drive"
}

/// What a selected item holds: itself and everything below it on its
/// filesystem, and the space those entries allocate (`st_blocks` × 512).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub entries: u64,
    pub files: u64,
    pub allocated_bytes: u64,
}

impl Size {
    fn add(&mut self, meta: &std::fs::Metadata) {
        self.entries += 1;
        self.files += u64::from(meta.is_file());
        self.allocated_bytes += meta.blocks() * 512;
    }

    pub fn plus(self, other: Size) -> Size {
        Size {
            entries: self.entries + other.entries,
            files: self.files + other.files,
            allocated_bytes: self.allocated_bytes + other.allocated_bytes,
        }
    }
}

/// Entries a post-move check visits at most to list what stayed behind.
pub const MAX_REMAINING_VISIT: u64 = 100_000;
/// A selected folder holding more entries than this, by the scan, moves
/// child by child so that a cancel and the progress reach inside it.
pub const SPLIT_ENTRIES: u64 = 100_000;

/// What a walk of an item found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Walked {
    pub size: Size,
    /// Symlinks among the entries: a Recycle Bin does not keep them.
    pub symlinks: u64,
    /// Up to `list` of its paths.
    pub paths: Vec<Vec<u8>>,
    /// False when the walk stopped at its bound: `size` is then a lower bound.
    pub complete: bool,
}

/// The size of `path`, up to `list` of its paths, visiting at most
/// `max_visit` entries. Symlinks are not followed and other filesystems
/// not entered. A stop signal ends the walk like the bound.
pub fn measure(path: &Path, list: usize, max_visit: u64) -> io::Result<Walked> {
    let top = std::fs::symlink_metadata(path)?;
    let device = top.dev();
    let mut walked = Walked {
        complete: true,
        ..Walked::default()
    };
    let note = |p: &Path, walked: &mut Walked| {
        if walked.paths.len() < list {
            walked.paths.push(p.as_os_str().as_bytes().to_vec());
        }
    };
    walked.size.add(&top);
    walked.symlinks += u64::from(top.file_type().is_symlink());
    note(path, &mut walked);
    if !top.is_dir() {
        return Ok(walked);
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            if walked.size.entries >= max_visit || crate::sys::stop_signal().is_some() {
                walked.complete = false;
                return Ok(walked);
            }
            let entry = entry?;
            let child = entry.path();
            let meta = match std::fs::symlink_metadata(&child) {
                Ok(meta) => meta,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            walked.size.add(&meta);
            walked.symlinks += u64::from(meta.file_type().is_symlink());
            note(&child, &mut walked);
            if meta.is_dir() && meta.dev() == device {
                stack.push(child);
            }
        }
    }
    Ok(walked)
}

/// The kind of a selected item: DSM's tool moves a file with `--unlink`
/// and a folder with `--rmdir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Directory,
}

/// One selected item of a plan, with its identity when planned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    #[serde(with = "crate::report::hex_bytes")]
    pub path: Vec<u8>,
    #[serde(with = "crate::report::hex_bytes")]
    pub share: Vec<u8>,
    #[serde(with = "crate::report::hex_bytes")]
    pub rpath: Vec<u8>,
    pub kind: Kind,
    pub device: u64,
    pub inode: u64,
    /// The Btrfs subvolume holding it, for the unit cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<u64>,
    pub size: Size,
    /// Symlinks it holds, itself included: DSM's Recycle Bin drops them
    /// (and folders holding only symlinks) on a move, as File Station's
    /// delete does, so Review says how many (user decision 2026-11-15).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub symlinks: u64,
    /// Folders, by path below the item (empty: the item itself), that the
    /// scan found holding more than [`SPLIT_ENTRIES`] entries: they move
    /// child by child, sorted by name, then go with a final `--rmdir`.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "hex_list")]
    pub split: Vec<Vec<u8>>,
    /// No Recycle Bin takes it (`share` is no DSM shared folder, or its
    /// Recycle Bin was off at the plan), so apply removes it for good, and
    /// only when the user confirmed that.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete: bool,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// A list of byte strings as hex.
mod hex_list {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(list: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(list.iter().map(|b| crate::report::hex_bytes::encode(b)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
        let text: Vec<String> = Vec::deserialize(d)?;
        text.iter()
            .map(|t| {
                (0..t.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(t.get(i..i + 2).unwrap_or("x"), 16))
                    .collect::<Result<Vec<u8>, _>>()
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

/// A copy a duplicates cleanup keeps: it must still exist when the
/// others move, as a folder, or as a file of the same size. A kept folder
/// lists its symlinks too, sized by their target path, as the report has them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keeper {
    #[serde(with = "crate::report::hex_bytes")]
    pub path: Vec<u8>,
    /// A file's size; `None` for a folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl Keeper {
    pub fn holds(&self) -> bool {
        match std::fs::symlink_metadata(OsStr::from_bytes(&self.path)) {
            Ok(meta) => match self.size {
                Some(size) => {
                    (meta.is_file() || meta.file_type().is_symlink()) && meta.len() == size
                }
                None => meta.is_dir(),
            },
            Err(_) => false,
        }
    }
}

/// Refuses before anything moves unless every kept copy still exists
/// with the same size: moving the others could leave none.
pub fn check_keepers(keep: &[Keeper]) -> Result<(), Refusal> {
    let total = keep.len() as u64;
    let changed = keep.iter().enumerate().find_map(|(done, k)| {
        if done % 256 == 0 {
            crate::planning::progress(done as u64, total);
            if crate::sys::stop_requested() {
                return Some(Err(()));
            }
        }
        (!k.holds()).then_some(Ok(k))
    });
    crate::planning::progress(total, total);
    match changed {
        None => Ok(()),
        Some(Err(())) => Err(Refusal::new(
            "cancelled",
            "cancelled: the check was stopped",
        )),
        Some(Ok(k)) => Err(Refusal::new(
            "keeper_changed",
            format!(
                "keeper_changed: {}, kept for a duplicate, is gone or changed size; review the duplicates again",
                escape_human(&k.path)
            ),
        )),
    }
}

/// A sealed plan: what `cleanup apply` moves, nothing else (SPEC 5.7).
/// On disk it is a [`PlanFile`]: a selection of any size stays small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "PlanFile", try_from = "PlanFile")]
pub struct Plan {
    pub format: u32,
    pub plan_id: String,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(with = "crate::report::hex_bytes")]
    pub volume: Vec<u8>,
    pub filesystem_uuid: String,
    pub filesystem: FilesystemKind,
    pub targets: Vec<Target>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keep: Vec<Keeper>,
    pub checksum: String,
}

impl Plan {
    pub fn seal(mut self) -> Plan {
        self.checksum = self.digest();
        self
    }

    fn digest(&self) -> String {
        let mut unsealed = self.clone();
        unsealed.checksum.clear();
        let bytes = serde_json::to_vec(&unsealed).unwrap_or_default();
        format!("{:016x}", guard::fnv1a64(&bytes))
    }

    /// A plan of this format, intact (the checksum detects corruption,
    /// not tampering: plans live in root's private state) and in time.
    pub fn validate(&self, now: u64) -> Result<(), Refusal> {
        if self.format != PLAN_FORMAT {
            return Err(Refusal::stale("plan of another format: make a new plan"));
        }
        if self.checksum != self.digest() {
            return Err(Refusal::stale("plan checksum mismatch"));
        }
        if self.targets.is_empty() {
            return Err(Refusal::stale("plan holds no targets"));
        }
        if now >= self.expires_at || now + 300 < self.created_at {
            return Err(Refusal::stale("plan expired: make a new plan"));
        }
        Ok(())
    }

    pub fn size(&self) -> Size {
        self.targets
            .iter()
            .fold(Size::default(), |sum, t| sum.plus(t.size))
    }

    /// How many items apply removes for good, and their size.
    pub fn deletes(&self) -> (usize, Size) {
        self.targets
            .iter()
            .filter(|t| t.delete)
            .fold((0, Size::default()), |(n, sum), t| {
                (n + 1, sum.plus(t.size))
            })
    }
}

/// A name as JSON: text when it is UTF-8, else `{"hex": …}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum Name {
    Text(String),
    Hex {
        #[serde(with = "crate::report::hex_bytes")]
        hex: Vec<u8>,
    },
}

impl Name {
    fn of(bytes: &[u8]) -> Name {
        match std::str::from_utf8(bytes) {
            Ok(text) => Name::Text(text.to_string()),
            Err(_) => Name::Hex {
                hex: bytes.to_vec(),
            },
        }
    }

    fn bytes(self) -> Vec<u8> {
        match self {
            Name::Text(text) => text.into_bytes(),
            Name::Hex { hex } => hex,
        }
    }
}

/// One target of a [`PlanFile`]: its shared folder (an index into
/// `shares`), its path in it, kind, device, inode, subvolume, size and
/// the folders the move splits, as one JSON array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row(
    u32,
    Name,
    Kind,
    u64,
    u64,
    Option<u64>,
    u64,
    u64,
    u64,
    Vec<Name>,
);

/// A [`Plan`] as saved: each target's path is its volume, shared folder
/// and path in it, so the folder names are kept once.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanFile {
    format: u32,
    plan_id: String,
    created_at: u64,
    expires_at: u64,
    #[serde(with = "crate::report::hex_bytes")]
    volume: Vec<u8>,
    filesystem_uuid: String,
    filesystem: FilesystemKind,
    shares: Vec<Name>,
    /// Indexes into `shares` of the folders with no Recycle Bin: their
    /// targets are removed for good.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    delete: Vec<u32>,
    targets: Vec<Row>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    keep: Vec<Keeper>,
    checksum: String,
}

impl From<Plan> for PlanFile {
    fn from(plan: Plan) -> PlanFile {
        let mut shares: Vec<Vec<u8>> = Vec::new();
        let mut delete: Vec<u32> = Vec::new();
        let mut index = std::collections::HashMap::<Vec<u8>, u32>::new();
        let targets = plan
            .targets
            .into_iter()
            .map(|t| {
                let share = *index.entry(t.share.clone()).or_insert_with(|| {
                    shares.push(t.share.clone());
                    let at = (shares.len() - 1) as u32;
                    if t.delete {
                        delete.push(at);
                    }
                    at
                });
                Row(
                    share,
                    Name::of(&t.rpath),
                    t.kind,
                    t.device,
                    t.inode,
                    t.tree,
                    t.size.entries,
                    t.size.files,
                    t.size.allocated_bytes,
                    t.split.iter().map(|s| Name::of(s)).collect(),
                )
            })
            .collect();
        PlanFile {
            format: plan.format,
            plan_id: plan.plan_id,
            created_at: plan.created_at,
            expires_at: plan.expires_at,
            volume: plan.volume,
            filesystem_uuid: plan.filesystem_uuid,
            filesystem: plan.filesystem,
            shares: shares.iter().map(|s| Name::of(s)).collect(),
            delete,
            targets,
            keep: plan.keep,
            checksum: plan.checksum,
        }
    }
}

impl TryFrom<PlanFile> for Plan {
    type Error = String;

    fn try_from(file: PlanFile) -> Result<Plan, String> {
        let shares: Vec<Vec<u8>> = file.shares.into_iter().map(Name::bytes).collect();
        let targets = file
            .targets
            .into_iter()
            .map(
                |Row(share, rpath, kind, device, inode, tree, entries, files, bytes, split)| {
                    let delete = file.delete.contains(&share);
                    let share = shares
                        .get(share as usize)
                        .ok_or("plan target names no shared folder")?
                        .clone();
                    let rpath = rpath.bytes();
                    Ok(Target {
                        path: [file.volume.as_slice(), b"/", &share, b"/", &rpath].concat(),
                        share,
                        rpath,
                        kind,
                        device,
                        inode,
                        tree,
                        size: Size {
                            entries,
                            files,
                            allocated_bytes: bytes,
                        },
                        symlinks: 0,
                        split: split.into_iter().map(Name::bytes).collect(),
                        delete,
                    })
                },
            )
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Plan {
            format: file.format,
            plan_id: file.plan_id,
            created_at: file.created_at,
            expires_at: file.expires_at,
            volume: file.volume,
            filesystem_uuid: file.filesystem_uuid,
            filesystem: file.filesystem,
            targets,
            keep: file.keep,
            checksum: file.checksum,
        })
    }
}

/// What became of one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// In the Recycle Bin now.
    Moved,
    /// Removed for good: no Recycle Bin took it (see [`delete_target`]).
    Deleted,
    /// Already gone before the move.
    AlreadyAbsent,
    /// Some of it moved; the rest stays where it was.
    Partial,
    /// Another item is at its path now: left alone.
    Changed,
    /// DSM's tool could not run or moved nothing.
    Failed,
}

/// One item's move and what stayed behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub outcome: Outcome,
    /// The space now in the Recycle Bin, as allocated when measured.
    pub moved: Size,
    pub remaining: Size,
    /// False when the walk of what stayed stopped at its bound.
    pub remaining_complete: bool,
    /// Up to [`LISTED_REMAINING`] paths still at the item's place.
    pub remaining_paths: Vec<Vec<u8>>,
    /// A cancel stopped it between DSM's runs.
    pub cancelled: bool,
    pub detail: Option<String>,
    /// Removed for good ([`delete_target`]), not moved: `moved` is what
    /// is gone for good.
    pub for_good: bool,
}

impl Moved {
    fn just(outcome: Outcome, detail: Option<String>) -> Moved {
        Moved {
            outcome,
            moved: Size::default(),
            remaining: Size::default(),
            remaining_complete: true,
            remaining_paths: Vec::new(),
            cancelled: false,
            detail,
            for_good: false,
        }
    }
}

/// The identity at `path` now: `None` when nothing is there.
fn identity(path: &Path) -> io::Result<Option<(u64, u64, bool)>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), meta.ino(), meta.is_dir()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The Recycle Bin settings of each shared folder, read once per apply.
#[derive(Debug, Default)]
pub struct BinCache {
    read: std::collections::HashMap<Vec<u8>, Result<BinStatus, String>>,
    /// `synoshare --get` runs so far.
    pub reads: u64,
}

impl BinCache {
    pub fn status(&mut self, tools: &Tools, share: &[u8]) -> Result<BinStatus, String> {
        if let Some(read) = self.read.get(share) {
            return read.clone();
        }
        self.reads += 1;
        let read = bin_status(tools, share).map_err(|refusal| refusal.message);
        self.read.insert(share.to_vec(), read.clone());
        read
    }
}

/// What a move asks of its caller between DSM's runs.
pub trait MoveControl {
    /// Whether to stop before the next run: a cancel or a stop signal.
    fn cancelled(&mut self) -> bool;
    /// One run of DSM's tool finished.
    fn stepped(&mut self);
}

/// A move nobody watches.
pub struct Unwatched;

impl MoveControl for Unwatched {
    fn cancelled(&mut self) -> bool {
        crate::sys::stop_signal().is_some()
    }
    fn stepped(&mut self) {}
}

/// Moves `target` into its shared folder's Recycle Bin with DSM's tool,
/// after checking it is still the planned item and the Recycle Bin is on,
/// then checks with `lstat` what moved. A folder the plan splits moves
/// child by child, sorted by name, then goes with a final `--rmdir`; a
/// cancel stops between runs. DSM's tool itself is never stopped half-way:
/// it would leave items in its hidden staging folder.
pub fn move_target(
    tools: &Tools,
    target: &Target,
    bins: &mut BinCache,
    control: &mut dyn MoveControl,
) -> Moved {
    let path = Path::new(OsStr::from_bytes(&target.path));
    match identity(path) {
        Ok(None) => return Moved::just(Outcome::AlreadyAbsent, None),
        Ok(Some((device, inode, dir)))
            if device == target.device
                && inode == target.inode
                && dir == (target.kind == Kind::Directory) => {}
        Ok(Some(_)) => {
            return Moved::just(
                Outcome::Changed,
                Some("another item is at this path now".into()),
            );
        }
        Err(e) => return Moved::just(Outcome::Failed, Some(e.to_string())),
    }
    match bins.status(tools, &target.share) {
        Ok(status) if status.enabled => {}
        // Turned off since the plan: no Recycle Bin takes it, so it goes
        // for good, as an item the plan found without one would (user
        // decision 2026-11-14).
        Ok(_) => return delete_target(target, control),
        Err(message) => return Moved::just(Outcome::Failed, Some(message)),
    }
    if let Err(refusal) = tools.trusted(&tools.synorecycle) {
        return Moved::just(Outcome::Failed, Some(refusal.message));
    }
    let mut run = Runner {
        tools,
        target,
        control,
        last_exit: None,
        cancelled: false,
    };
    if target.kind == Kind::Directory && target.split.iter().any(|s| s.is_empty()) {
        run.split(b"");
    } else {
        run.one(&target.rpath, target.kind == Kind::Directory);
    }
    let (exit, cancelled) = (run.last_exit, run.cancelled);
    let after = match identity(path) {
        Ok(after) => after,
        Err(e) => return Moved::just(Outcome::Failed, Some(e.to_string())),
    };
    match after {
        Some((device, inode, _)) if device == target.device && inode == target.inode => {
            // Only an item DSM left in place is walked, and only so far.
            let walked = measure(path, LISTED_REMAINING, MAX_REMAINING_VISIT).unwrap_or_default();
            let moved = Size {
                entries: target.size.entries.saturating_sub(walked.size.entries),
                files: target.size.files.saturating_sub(walked.size.files),
                allocated_bytes: target
                    .size
                    .allocated_bytes
                    .saturating_sub(walked.size.allocated_bytes),
            };
            let detail = if cancelled {
                "cancelled: the rest stays where it was".to_string()
            } else {
                match exit {
                    Some(0) => "DSM's synorecycle left these items in place".to_string(),
                    Some(code) => format!("DSM's synorecycle exited {code}"),
                    None => "DSM's synorecycle could not run".to_string(),
                }
            };
            Moved {
                outcome: if moved.entries == 0 && !cancelled && target.split.is_empty() {
                    Outcome::Failed
                } else {
                    Outcome::Partial
                },
                moved,
                remaining: walked.size,
                remaining_complete: walked.complete,
                remaining_paths: walked.paths,
                cancelled,
                detail: Some(detail),
                for_good: false,
            }
        }
        // Gone, or something else took the name since: it moved.
        _ => Moved {
            moved: target.size,
            ..Moved::just(Outcome::Moved, None)
        },
    }
}

/// Removes `target` for good: there is no Recycle Bin to move it into, as
/// its folder is no shared folder or its shared folder's Recycle Bin is off
/// (user decisions 2026-11-14). It must still be the planned item. A folder is emptied depth first through descriptors: no
/// symlink is followed, and nothing on another device (a mount, or a Btrfs
/// subvolume below it) is touched, so that part and the folders holding it
/// stay. A cancel stops between entries.
pub fn delete_target(target: &Target, control: &mut dyn MoveControl) -> Moved {
    Moved {
        for_good: true,
        ..remove(target, control)
    }
}

fn remove(target: &Target, control: &mut dyn MoveControl) -> Moved {
    let path = Path::new(OsStr::from_bytes(&target.path));
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Moved::just(Outcome::Failed, Some("not a path below a folder".into()));
    };
    let parent = match guard::open_absolute_no_follow(parent) {
        Ok(parent) => parent,
        Err(e) => return Moved::just(Outcome::Failed, Some(e.to_string())),
    };
    let stat = match crate::sys::statat(parent.as_fd(), name) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Moved::just(Outcome::AlreadyAbsent, None);
        }
        Err(e) => return Moved::just(Outcome::Failed, Some(e.to_string())),
    };
    let dir = stat.mode & libc::S_IFMT == libc::S_IFDIR;
    if (stat.device, stat.inode, dir)
        != (target.device, target.inode, target.kind == Kind::Directory)
    {
        return Moved::just(
            Outcome::Changed,
            Some("another item is at this path now".into()),
        );
    }
    let mut removal = Removal {
        device: target.device,
        removed: Size::default(),
        kept: false,
        cancelled: false,
    };
    let done = if dir {
        removal.tree(&parent, name, &stat, control)
    } else {
        crate::sys::unlinkat(parent.as_fd(), name, false).map(|()| {
            removal.count(&stat);
            true
        })
    };
    match done {
        Ok(true) => Moved {
            moved: removal.removed,
            ..Moved::just(Outcome::Deleted, None)
        },
        Ok(false) | Err(_) => {
            let walked = measure(path, LISTED_REMAINING, MAX_REMAINING_VISIT).unwrap_or_default();
            let detail = match done {
                Err(e) => format!("removing it stopped: {e}"),
                Ok(_) if removal.cancelled => "cancelled: the rest stays where it was".into(),
                Ok(_) => {
                    "a mount or Btrfs subvolume below it stays, with the folders holding it".into()
                }
            };
            Moved {
                outcome: if removal.removed.entries == 0 && !removal.cancelled {
                    Outcome::Failed
                } else {
                    Outcome::Partial
                },
                moved: removal.removed,
                remaining: walked.size,
                remaining_complete: walked.complete,
                remaining_paths: walked.paths,
                cancelled: removal.cancelled,
                detail: Some(detail),
                for_good: false,
            }
        }
    }
}

/// One removal for good: what it removed, and whether it left anything.
struct Removal {
    device: u64,
    removed: Size,
    /// Something on another device stays, so its folders do too.
    kept: bool,
    cancelled: bool,
}

impl Removal {
    fn count(&mut self, stat: &crate::sys::FileStat) {
        self.removed.entries += 1;
        if stat.mode & libc::S_IFMT == libc::S_IFREG {
            self.removed.files += 1;
            self.removed.allocated_bytes += stat.blocks * 512;
        }
    }

    /// Empties and removes folder `name` in `parent`, `stat` as checked.
    /// True when it is gone.
    fn tree(
        &mut self,
        parent: &std::fs::File,
        name: &OsStr,
        stat: &crate::sys::FileStat,
        control: &mut dyn MoveControl,
    ) -> io::Result<bool> {
        let dir = crate::sys::openat(
            parent.as_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )?;
        let opened = dir.metadata()?;
        if (opened.dev(), opened.ino()) != (stat.device, stat.inode) {
            return Err(io::Error::other("the folder changed while opening it"));
        }
        let names =
            crate::sys::DirectoryEntries::open(dir.as_fd())?.collect::<io::Result<Vec<_>>>()?;
        let mut whole = true;
        for child in names {
            if control.cancelled() {
                self.cancelled = true;
                return Ok(false);
            }
            let entry = match crate::sys::statat(dir.as_fd(), &child) {
                Ok(entry) => entry,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if entry.device != self.device {
                self.kept = true;
                whole = false;
                continue;
            }
            if entry.mode & libc::S_IFMT == libc::S_IFDIR {
                whole &= self.tree(&dir, &child, &entry, control)?;
                if self.cancelled {
                    return Ok(false);
                }
            } else {
                crate::sys::unlinkat(dir.as_fd(), &child, false)?;
                self.count(&entry);
            }
        }
        if !whole {
            return Ok(false);
        }
        crate::sys::unlinkat(parent.as_fd(), name, true)?;
        self.count(stat);
        Ok(true)
    }
}

/// DSM's runs for one target.
struct Runner<'a> {
    tools: &'a Tools,
    target: &'a Target,
    control: &'a mut dyn MoveControl,
    last_exit: Option<i32>,
    cancelled: bool,
}

impl Runner<'_> {
    /// One run: `--rmdir` for a folder, `--unlink` for anything else,
    /// on `rpath` inside the share.
    fn one(&mut self, rpath: &[u8], dir: bool) {
        let mut share = OsString::from("share=");
        share.push(OsStr::from_bytes(&self.target.share));
        let mut arg = OsString::from("rpath=");
        arg.push(OsStr::from_bytes(rpath));
        let verb = if dir { "--rmdir" } else { "--unlink" };
        #[cfg(test)]
        if let Some(root) = &self.tools.in_process {
            let from = root.join(OsStr::from_bytes(rpath));
            let to = root.join("#recycle").join(OsStr::from_bytes(rpath));
            let moved = to
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::rename(&from, &to));
            self.last_exit = Some(i32::from(moved.is_err()));
            self.control.stepped();
            return;
        }
        self.last_exit = run(&self.tools.synorecycle, &[OsStr::new(verb), &share, &arg]);
        self.control.stepped();
    }

    /// Moves the folder `rel` below the target folder by folder, then the
    /// folder itself with the files directly in it: one run per file costs
    /// about 13 ms of DSM's start-up (a test NAS), forty times one run's
    /// rate, so files go together in the final run. A child folder the
    /// plan splits too goes the same way.
    fn split(&mut self, rel: &[u8]) {
        let join = |base: &[u8], name: &[u8]| -> Vec<u8> {
            match (base.is_empty(), name.is_empty()) {
                (true, _) => name.to_vec(),
                (_, true) => base.to_vec(),
                _ => [base, b"/", name].concat(),
            }
        };
        let dir = join(&self.target.path, rel);
        let mut children: Vec<(Vec<u8>, bool)> = match std::fs::read_dir(OsStr::from_bytes(&dir)) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|e| {
                    let dir = e.file_type().is_ok_and(|t| t.is_dir());
                    (e.file_name().into_vec(), dir)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        children.sort();
        for (name, is_dir) in children {
            if self.cancelled || self.control.cancelled() {
                self.cancelled = true;
                return;
            }
            if !is_dir {
                continue;
            }
            let child = join(rel, &name);
            if self.target.split.contains(&child) {
                self.split(&child);
            } else {
                self.one(&join(&self.target.rpath, &child), true);
            }
        }
        if self.cancelled || self.control.cancelled() {
            self.cancelled = true;
            return;
        }
        // The files directly in it, the emptied folder, and whatever
        // appeared in it meanwhile.
        self.one(&join(&self.target.rpath, rel), true);
    }
}

/// Runs `program args` without a shell, from `/`, with no input and its
/// output discarded, a fixed PATH and LANG=C; its exit code, `None` when
/// it could not run or was killed. It is never stopped.
fn run(program: &Path, args: &[&OsStr]) -> Option<i32> {
    let mut child = command(program, args).stdout(Stdio::null()).spawn().ok()?;
    let status = crate::rootfs::delegate::wait_child(&mut child, || false).ok()?;
    status
        .code()
        .or_else(|| status.signal().map(|_| -1))
        .filter(|c| *c >= 0)
}

fn command(program: &Path, args: &[&OsStr]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env(
            "PATH",
            "/usr/syno/sbin:/usr/syno/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("LANG", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

/// `run`, returning at most `max` bytes of standard output when it exits 0.
fn run_capture(program: &Path, args: &[&OsStr], max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = command(program, args).stdout(Stdio::piped()).spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut limited = stdout.take(max as u64);
        let _ = limited.read_to_end(&mut out);
        let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
        out
    });
    let status = crate::rootfs::delegate::wait_child(&mut child, || false).ok()?;
    let out = reader.join().ok()?;
    status.success().then_some(out)
}

/// Bytes of a path as an OS path.
pub fn os_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Scratch(PathBuf);
    impl Scratch {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch() -> Scratch {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("reclaim-recycle-move-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }

    #[test]
    fn synoshare_output_gives_the_recycle_bin_settings() {
        let out = b"SYNOSHARE data dump:\n\t Name .......[photos]\n\t RecycleBin....[yes]\n\t RecycleBinAdminOnly....[no]\n";
        assert_eq!(
            parse_bin_status(out),
            Some(BinStatus {
                enabled: true,
                admin_only: false
            })
        );
        let out = b"\t RecycleBin....[no]\n\t RecycleBinAdminOnly....[yes]\n";
        assert_eq!(
            parse_bin_status(out),
            Some(BinStatus {
                enabled: false,
                admin_only: true
            })
        );
        assert_eq!(parse_bin_status(b"\t Name ....[x]\n"), None);
    }

    #[test]
    fn only_items_inside_a_shared_folder_and_outside_dsm_folders_are_located() {
        let ok = locate(b"/volume1/photos/2023/a.jpg", b"/volume1").unwrap();
        assert_eq!(ok.share, b"photos");
        assert_eq!(ok.rpath, b"2023/a.jpg");
        let home = locate(b"/volume1/homes/alice/old", b"/volume1").unwrap();
        assert_eq!(
            (home.share.as_slice(), home.rpath.as_slice()),
            (&b"homes"[..], &b"alice/old"[..])
        );
        for (path, code) in [
            (&b"/volume1"[..], "protected_target"),
            (b"/volume1/photos", "protected_target"),
            (b"/volume1/homes/alice", "protected_target"),
            (b"/volume1/photos/#recycle/a", "in_recycle_bin"),
            (b"/volume1/photos/#recycle", "in_recycle_bin"),
            (b"/volume1/homes/alice/#recycle/x", "in_recycle_bin"),
            (b"/volume1/photos/@eaDir/a", "protected_target"),
            (b"/volume1/photos/x/#snapshot", "protected_target"),
            (b"/volume1/@appstore/x", "protected_target"),
            (b"/volume2/photos/a", "protected_target"),
        ] {
            assert_eq!(
                locate(path, b"/volume1").unwrap_err().code,
                code,
                "{path:?}"
            );
        }
    }

    #[test]
    fn a_users_drive_folder_is_recognized() {
        assert!(in_drive(b"/volume1/homes/alice/Drive/a", b"/volume1"));
        assert!(!in_drive(b"/volume1/homes/alice/Docs/a", b"/volume1"));
        assert!(!in_drive(b"/volume1/photos/alice/Drive/a", b"/volume1"));
    }

    #[test]
    fn a_keeper_must_still_exist_with_its_size() {
        let dir = scratch();
        let file = dir.path().join("keep.bin");
        std::fs::write(&file, b"12345").unwrap();
        let bytes = |p: &Path| p.as_os_str().as_bytes().to_vec();
        let mut keep = vec![
            Keeper {
                path: bytes(&file),
                size: Some(5),
            },
            Keeper {
                path: bytes(dir.path()),
                size: None,
            },
        ];
        assert!(check_keepers(&keep).is_ok());
        keep[0].size = Some(6);
        assert_eq!(check_keepers(&keep).unwrap_err().code, "keeper_changed");
        keep[0].size = Some(5);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(check_keepers(&keep).unwrap_err().code, "keeper_changed");
    }

    #[test]
    fn a_kept_symlink_holds_with_its_target_length() {
        let dir = scratch();
        let link = dir.path().join("Changes");
        std::os::unix::fs::symlink("process/changes.rst", &link).unwrap();
        let mut keep = vec![Keeper {
            path: link.as_os_str().as_bytes().to_vec(),
            size: Some(19),
        }];
        assert!(check_keepers(&keep).is_ok());
        keep[0].size = Some(20);
        assert_eq!(check_keepers(&keep).unwrap_err().code, "keeper_changed");
    }

    #[test]
    fn a_plan_is_sealed_and_expires() {
        let plan = Plan {
            format: PLAN_FORMAT,
            plan_id: "cleanup-1-2".into(),
            created_at: 100,
            expires_at: 100 + PLAN_LIFETIME_SECONDS,
            volume: b"/volume1".to_vec(),
            filesystem_uuid: "f".into(),
            filesystem: FilesystemKind::Btrfs,
            targets: vec![Target {
                path: b"/volume1/s/a".to_vec(),
                share: b"s".to_vec(),
                rpath: b"a".to_vec(),
                kind: Kind::File,
                device: 1,
                inode: 2,
                tree: Some(256),
                size: Size::default(),
                symlinks: 0,
                split: vec![Vec::new(), b"x/y".to_vec()],
                delete: false,
            }],
            keep: Vec::new(),
            checksum: String::new(),
        }
        .seal();
        assert!(plan.validate(200).is_ok());
        assert_eq!(
            plan.validate(100 + PLAN_LIFETIME_SECONDS).unwrap_err().code,
            "plan_stale"
        );
        let mut changed = plan.clone();
        changed.targets[0].rpath = b"b".to_vec();
        assert_eq!(changed.validate(200).unwrap_err().code, "plan_stale");
        let text = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<Plan>(&text).unwrap(), plan);
    }

    #[test]
    fn a_plan_file_keeps_each_shared_folder_once_and_any_name() {
        let target = |share: &[u8], rpath: &[u8]| Target {
            path: [b"/volume1/".as_slice(), share, b"/", rpath].concat(),
            share: share.to_vec(),
            rpath: rpath.to_vec(),
            kind: Kind::File,
            device: 41,
            inode: 7,
            tree: None,
            size: Size {
                entries: 1,
                files: 1,
                allocated_bytes: 4096,
            },
            symlinks: 0,
            split: Vec::new(),
            delete: false,
        };
        let mut targets: Vec<Target> = (0..50_000)
            .map(|i| target(b"photos", format!("2026/IMG_{i:05}.jpg").as_bytes()))
            .collect();
        targets.push(target(b"m\xfasica", b"caf\xe9/\xff.mp3"));
        let plan = Plan {
            format: PLAN_FORMAT,
            plan_id: "cleanup-1-2".into(),
            created_at: 100,
            expires_at: 100 + PLAN_LIFETIME_SECONDS,
            volume: b"/volume1".to_vec(),
            filesystem_uuid: "f".into(),
            filesystem: FilesystemKind::Btrfs,
            targets,
            keep: Vec::new(),
            checksum: String::new(),
        }
        .seal();
        let text = serde_json::to_string(&plan).unwrap();
        assert_eq!(
            text.matches("photos").count(),
            1,
            "a shared folder is named once"
        );
        assert!(text.contains(r#"[0,"2026/IMG_00000.jpg","file",41,7,null,1,1,4096,[]]"#));
        // 50,000 items fit in a few MB: about 50 bytes each beyond the name.
        assert!(text.len() < 50_000 * 70, "{} bytes", text.len());
        let read: Plan = serde_json::from_str(&text).unwrap();
        assert_eq!(read, plan);
        assert!(read.validate(200).is_ok());
        assert_eq!(
            read.targets[50_000].path,
            b"/volume1/m\xfasica/caf\xe9/\xff.mp3"
        );
    }

    #[test]
    fn a_plan_file_marks_the_folders_whose_items_are_removed_for_good() {
        let target = |share: &[u8], delete: bool| Target {
            path: [b"/volume1/".as_slice(), share, b"/a.tar"].concat(),
            share: share.to_vec(),
            rpath: b"a.tar".to_vec(),
            kind: Kind::File,
            device: 41,
            inode: 7,
            tree: None,
            size: Size {
                entries: 1,
                files: 1,
                allocated_bytes: 4096,
            },
            symlinks: 0,
            split: Vec::new(),
            delete,
        };
        let plan = Plan {
            format: PLAN_FORMAT,
            plan_id: "cleanup-1-2".into(),
            created_at: 100,
            expires_at: 100 + PLAN_LIFETIME_SECONDS,
            volume: b"/volume1".to_vec(),
            filesystem_uuid: "f".into(),
            filesystem: FilesystemKind::Btrfs,
            targets: vec![target(b"photos", false), target(b"restore-test", true)],
            keep: Vec::new(),
            checksum: String::new(),
        }
        .seal();
        let text = serde_json::to_string(&plan).unwrap();
        assert!(text.contains(r#""delete":[1]"#), "{text}");
        let read: Plan = serde_json::from_str(&text).unwrap();
        assert_eq!(read, plan);
        assert_eq!(read.deletes(), (1, target(b"x", true).size));
        // A plan without the mark never removes anything for good.
        let mut moved = plan.clone();
        moved.targets[1].delete = false;
        assert!(
            !serde_json::to_string(&moved.seal())
                .unwrap()
                .contains(r#""delete""#)
        );
        // The mark is sealed: dropping it from the file makes the plan stale.
        let tampered: Plan = serde_json::from_str(&text.replace(r#""delete":[1],"#, "")).unwrap();
        assert!(!tampered.targets[1].delete);
        assert_eq!(tampered.validate(200).unwrap_err().code, "plan_stale");
    }

    #[test]
    fn an_item_in_no_shared_folder_is_removed_for_good_only_while_it_is_the_planned_one() {
        let dir = scratch();
        let folder = dir.path().join("restore-test");
        std::fs::create_dir_all(folder.join("tree/deep")).unwrap();
        std::fs::write(folder.join("tree/deep/a"), b"12345").unwrap();
        std::fs::write(folder.join("tree/b"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), folder.join("tree/link")).unwrap();
        std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
        std::fs::write(dir.path().join("elsewhere/kept"), b"k").unwrap();
        std::fs::write(folder.join("package-meta.tar"), b"tar").unwrap();
        let mark = |mut t: Target| {
            t.delete = true;
            t
        };
        let tree = mark(planned(&folder.join("tree"), &folder));
        let moved = delete_target(&tree, &mut Unwatched);
        assert_eq!(moved.outcome, Outcome::Deleted, "{:?}", moved.detail);
        assert_eq!((moved.moved.entries, moved.moved.files), (5, 2));
        assert!(!folder.join("tree").exists());
        // The symlink went, not what it pointed at.
        assert!(dir.path().join("elsewhere/kept").exists());
        assert_eq!(
            delete_target(&tree, &mut Unwatched).outcome,
            Outcome::AlreadyAbsent
        );
        // Another file at the planned path stays.
        let file = mark(planned(&folder.join("package-meta.tar"), &folder));
        std::fs::write(folder.join("other"), b"new").unwrap();
        std::fs::rename(folder.join("other"), folder.join("package-meta.tar")).unwrap();
        let changed = delete_target(&file, &mut Unwatched);
        assert_eq!(changed.outcome, Outcome::Changed);
        assert!(folder.join("package-meta.tar").exists());
        let file = mark(planned(&folder.join("package-meta.tar"), &folder));
        assert_eq!(
            delete_target(&file, &mut Unwatched).outcome,
            Outcome::Deleted
        );
        assert!(!folder.join("package-meta.tar").exists());
        assert!(folder.exists(), "the folder below the volume stays");
    }

    #[test]
    fn a_folder_dsm_knows_as_a_share_is_never_treated_as_outside_every_share() {
        let dir = scratch();
        let tools = fake_tools(dir.path(), dir.path(), "yes", false);
        assert!(dsm_knows_share(&tools, b"photos").unwrap());
        std::fs::write(&tools.synoshare, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(!dsm_knows_share(&tools, b"restore-test").unwrap());
    }

    #[test]
    fn a_cancel_stops_a_removal_for_good_between_entries() {
        struct Stop;
        impl MoveControl for Stop {
            fn cancelled(&mut self) -> bool {
                true
            }
            fn stepped(&mut self) {}
        }
        let dir = scratch();
        std::fs::create_dir_all(dir.path().join("tree")).unwrap();
        std::fs::write(dir.path().join("tree/a"), b"a").unwrap();
        let mut tree = planned(&dir.path().join("tree"), dir.path());
        tree.delete = true;
        let moved = delete_target(&tree, &mut Stop);
        assert_eq!(moved.outcome, Outcome::Partial);
        assert!(moved.cancelled);
        assert!(dir.path().join("tree/a").exists());
    }

    /// Fake DSM tools under `dir`, owned by the test's user: `synoshare`
    /// prints `bin` and `synorecycle` moves the item into `#recycle` the
    /// way DSM does, or does nothing when `moves` is false.
    pub(crate) fn fake_tools(dir: &Path, share_root: &Path, bin: &str, moves: bool) -> Tools {
        let write = |name: &str, body: String| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let synoshare = write(
            "synoshare",
            format!("printf '\\t RecycleBin....[{bin}]\\n\\t RecycleBinAdminOnly....[no]\\n'"),
        );
        let root = share_root.display();
        let log = dir.join("runs").display().to_string();
        let synorecycle = write(
            "synorecycle",
            if moves {
                format!(
                    "r=\"${{3#rpath=}}\"; echo \"$1 $r\" >> {log}; s=\"{root}/$r\"; d=\"{root}/#recycle/$r\"\nif [ -d \"$s\" ] && [ -d \"$d\" ]; then cp -R \"$s\"/. \"$d\"/ && rm -rf \"$s\"; exit $?; fi\nmkdir -p \"$(dirname \"$d\")\" && mv \"$s\" \"$d\""
                )
            } else {
                format!("r=\"${{3#rpath=}}\"; echo \"$1 $r\" >> {log}; exit 0")
            },
        );
        let systemctl = write("systemctl", "exit 0".into());
        Tools {
            synorecycle,
            synoshare,
            systemctl,
            owner: crate::sys::effective_uid(),
            in_process: None,
        }
    }

    pub(crate) fn planned(path: &Path, share_root: &Path) -> Target {
        let meta = std::fs::symlink_metadata(path).unwrap();
        let rpath = path.strip_prefix(share_root).unwrap();
        Target {
            path: path.as_os_str().as_bytes().to_vec(),
            share: b"s".to_vec(),
            rpath: rpath.as_os_str().as_bytes().to_vec(),
            kind: if meta.is_dir() {
                Kind::Directory
            } else {
                Kind::File
            },
            device: meta.dev(),
            inode: meta.ino(),
            tree: None,
            size: measure(path, 0, u64::MAX).unwrap().size,
            symlinks: 0,
            split: Vec::new(),
            delete: false,
        }
    }

    fn runs(tools_dir: &Path) -> Vec<String> {
        std::fs::read_to_string(tools_dir.join("runs"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn go(tools: &Tools, target: &Target) -> Moved {
        move_target(tools, target, &mut BinCache::default(), &mut Unwatched)
    }

    #[test]
    fn a_planned_folder_lands_in_the_recycle_bin() {
        let tmp = scratch();
        let (tools_dir, share) = (tmp.path().join("tools"), tmp.path().join("s"));
        std::fs::create_dir_all(share.join("a/b")).unwrap();
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(share.join("a/b/f"), b"data").unwrap();
        let tools = fake_tools(&tools_dir, &share, "yes", true);
        let target = planned(&share.join("a"), &share);
        let moved = go(&tools, &target);
        assert_eq!(moved.outcome, Outcome::Moved);
        assert_eq!(moved.moved, target.size);
        assert!(share.join("#recycle/a/b/f").exists());
        assert_eq!(go(&tools, &target).outcome, Outcome::AlreadyAbsent);
    }

    #[test]
    fn an_item_whose_recycle_bin_went_off_goes_for_good_and_a_changed_one_stays() {
        let tmp = scratch();
        let (tools_dir, share) = (tmp.path().join("tools"), tmp.path().join("s"));
        std::fs::create_dir_all(share.join("a")).unwrap();
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(share.join("a/f"), b"data").unwrap();
        let target = planned(&share.join("a"), &share);
        // Turned off after the plan: removed for good, nothing in #recycle.
        let off = fake_tools(&tools_dir, &share, "no", true);
        let gone = go(&off, &target);
        assert_eq!(gone.outcome, Outcome::Deleted);
        assert!(gone.for_good);
        assert_eq!(
            (gone.moved.entries, gone.moved.files),
            (target.size.entries, target.size.files)
        );
        assert!(!share.join("a").exists());
        assert!(!share.join("#recycle").exists());
        std::fs::create_dir_all(share.join("a")).unwrap();
        std::fs::write(share.join("a/f"), b"data").unwrap();
        let target = planned(&share.join("a"), &share);
        let on = fake_tools(&tools_dir, &share, "yes", true);
        std::fs::rename(share.join("a"), share.join("old")).unwrap();
        std::fs::create_dir(share.join("a")).unwrap();
        assert_eq!(go(&on, &target).outcome, Outcome::Changed);
        assert!(share.join("a").exists() && share.join("old/f").exists());
    }

    #[test]
    fn what_dsm_left_in_place_is_reported_partial_or_failed() {
        let tmp = scratch();
        let (tools_dir, share) = (tmp.path().join("tools"), tmp.path().join("s"));
        std::fs::create_dir_all(share.join("a")).unwrap();
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(share.join("a/f"), b"data").unwrap();
        std::fs::write(share.join("a/g"), b"more").unwrap();
        let target = planned(&share.join("a"), &share);
        let idle = fake_tools(&tools_dir, &share, "yes", false);
        let moved = go(&idle, &target);
        assert_eq!(moved.outcome, Outcome::Failed);
        assert_eq!(moved.remaining, target.size);
        assert_eq!(moved.remaining_paths.len(), 3);
        assert!(moved.remaining_complete);
        // DSM moved one file, not the other.
        std::fs::create_dir_all(share.join("#recycle/a")).unwrap();
        std::fs::rename(share.join("a/f"), share.join("#recycle/a/f")).unwrap();
        let moved = go(&idle, &target);
        assert_eq!(moved.outcome, Outcome::Partial);
        assert_eq!(moved.remaining.files, 1);
        assert_eq!(moved.moved.files, 1);
    }

    #[test]
    fn a_program_dsm_does_not_own_is_not_run() {
        let tmp = scratch();
        let share = tmp.path().join("s");
        std::fs::create_dir_all(share.join("a")).unwrap();
        let tools_dir = tmp.path().join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        let mut tools = fake_tools(&tools_dir, &share, "yes", true);
        tools.owner += 1;
        let target = planned(&share.join("a"), &share);
        let moved = go(&tools, &target);
        assert_eq!(moved.outcome, Outcome::Failed);
        assert!(share.join("a").exists());
        assert!(!recyclebind_running(&tools));
    }

    #[test]
    fn a_large_folder_moves_folder_by_folder_in_name_order_and_then_goes() {
        let tmp = scratch();
        let (tools_dir, share) = (tmp.path().join("tools"), tmp.path().join("s"));
        std::fs::create_dir_all(share.join("big/b-small")).unwrap();
        std::fs::create_dir_all(share.join("big/a-big/x")).unwrap();
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(share.join("big/c.bin"), b"c").unwrap();
        std::fs::write(share.join("big/b-small/f"), b"f").unwrap();
        std::fs::write(share.join("big/a-big/x/g"), b"g").unwrap();
        std::fs::write(share.join("big/a-big/h"), b"h").unwrap();
        let tools = fake_tools(&tools_dir, &share, "yes", true);
        let mut target = planned(&share.join("big"), &share);
        // As the scan would mark them: the folder and its large child.
        target.split = vec![Vec::new(), b"a-big".to_vec()];
        let mut steps = 0;
        struct Count<'a>(&'a mut u32);
        impl MoveControl for Count<'_> {
            fn cancelled(&mut self) -> bool {
                false
            }
            fn stepped(&mut self) {
                *self.0 += 1;
            }
        }
        let moved = move_target(
            &tools,
            &target,
            &mut BinCache::default(),
            &mut Count(&mut steps),
        );
        assert_eq!(moved.outcome, Outcome::Moved);
        assert_eq!(
            runs(&tools_dir),
            [
                "--rmdir big/a-big/x",
                "--rmdir big/a-big",
                "--rmdir big/b-small",
                "--rmdir big",
            ]
        );
        assert_eq!(steps, 4);
        assert!(!share.join("big").exists());
        assert!(share.join("#recycle/big/a-big/x/g").exists());
        assert!(share.join("#recycle/big/c.bin").exists());
    }

    #[test]
    fn a_cancel_stops_between_runs_and_reports_what_stayed() {
        let tmp = scratch();
        let (tools_dir, share) = (tmp.path().join("tools"), tmp.path().join("s"));
        std::fs::create_dir_all(&tools_dir).unwrap();
        for name in ["a", "b", "c"] {
            std::fs::create_dir_all(share.join("big").join(name)).unwrap();
            std::fs::write(share.join("big").join(name).join("f"), name).unwrap();
        }
        let tools = fake_tools(&tools_dir, &share, "yes", true);
        let mut target = planned(&share.join("big"), &share);
        target.split = vec![Vec::new()];
        struct AfterOne(u32);
        impl MoveControl for AfterOne {
            fn cancelled(&mut self) -> bool {
                self.0 >= 1
            }
            fn stepped(&mut self) {
                self.0 += 1;
            }
        }
        let moved = move_target(&tools, &target, &mut BinCache::default(), &mut AfterOne(0));
        assert_eq!(moved.outcome, Outcome::Partial);
        assert!(moved.cancelled);
        assert_eq!(runs(&tools_dir), ["--rmdir big/a"]);
        // The folder, b, c and their files stay: 5 entries of 7.
        assert_eq!(moved.remaining.entries, 5);
        assert_eq!(moved.moved.entries, 2);
        assert!(share.join("big/b/f").exists() && share.join("#recycle/big/a/f").exists());
    }

    #[test]
    fn the_recycle_bin_settings_are_read_once_per_shared_folder() {
        let tmp = scratch();
        let (tools_dir, share) = (tmp.path().join("tools"), tmp.path().join("s"));
        std::fs::create_dir_all(&share).unwrap();
        std::fs::create_dir_all(&tools_dir).unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(share.join(name), name).unwrap();
        }
        let tools = fake_tools(&tools_dir, &share, "yes", true);
        let mut bins = BinCache::default();
        for name in ["a", "b", "c"] {
            let target = planned(&share.join(name), &share);
            assert_eq!(
                move_target(&tools, &target, &mut bins, &mut Unwatched).outcome,
                Outcome::Moved
            );
        }
        assert_eq!(bins.reads, 1);
    }

    /// A walk counts the symlinks it meets, without following them,
    /// those in a folder holding only symlinks too.
    #[test]
    fn a_walk_counts_symlinks() {
        let tmp = scratch();
        std::fs::write(tmp.path().join("a.c"), "a").unwrap();
        std::os::unix::fs::symlink("a.c", tmp.path().join("b.c")).unwrap();
        std::fs::create_dir(tmp.path().join("only-links")).unwrap();
        std::os::unix::fs::symlink("../a.c", tmp.path().join("only-links/x")).unwrap();
        std::os::unix::fs::symlink("/nowhere", tmp.path().join("only-links/y")).unwrap();
        let walked = measure(tmp.path(), 0, u64::MAX).unwrap();
        assert!(walked.complete);
        assert_eq!(walked.symlinks, 3);
        assert_eq!(walked.size.entries, 6);
        assert_eq!(walked.size.files, 1);
        let link = measure(&tmp.path().join("b.c"), 0, u64::MAX).unwrap();
        assert_eq!(link.symlinks, 1);
    }

    #[test]
    fn a_walk_stops_at_its_bound() {
        let tmp = scratch();
        for name in ["a", "b", "c", "d"] {
            std::fs::write(tmp.path().join(name), name).unwrap();
        }
        let walked = measure(tmp.path(), 2, 3).unwrap();
        assert!(!walked.complete);
        assert_eq!(walked.size.entries, 3);
        assert_eq!(walked.paths.len(), 2);
        assert!(measure(tmp.path(), 0, u64::MAX).unwrap().complete);
    }
}
