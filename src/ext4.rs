//! Read-only fd-relative namespace scan. No Btrfs ioctls, cache or content reads.

use crate::btrfs::walk::{BatchBoundary, StopReason, WalkStats};
use crate::model::namespace::FileKind;
use crate::model::tree::{InodeAllocation, InodeTree, VIRTUAL_ROOT};
use crate::sys::{self, DirectoryEntries, FileStat};
use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::ControlFlow;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

pub mod records;
pub mod spilling;

const DIRECTORY_FLAGS: i32 =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
const BATCH: u64 = 256;
const MAX_DEPTH: usize = 128;

pub const SUPER_MAGIC: u64 = 0xef53;

/// UUID only, from an ext4 superblock. Mounted free counters are never used.
pub fn superblock_uuid(superblock: &[u8]) -> io::Result<[u8; 16]> {
    if superblock.get(56..58) != Some(&[0x53, 0xef]) {
        return Err(io::Error::other("not an ext4 superblock"));
    }
    let uuid: [u8; 16] = superblock
        .get(104..120)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| io::Error::other("truncated ext4 UUID"))?;
    if uuid == [0; 16] {
        return Err(io::Error::other("ext4 UUID is empty"));
    }
    Ok(uuid)
}

/// Pin the mountinfo source and verify its block-device number against the
/// mounted filesystem before reading its UUID. Never accept an arbitrary file.
pub fn volume_uuid(mount: &crate::mounts::Mount) -> io::Result<[u8; 16]> {
    let source = std::fs::canonicalize(&mount.source)?;
    let device = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)?;
    let meta = device.metadata()?;
    let number = format!("{}:{}", libc::major(meta.rdev()), libc::minor(meta.rdev()));
    if !meta.file_type().is_block_device() || number != mount.device {
        return Err(io::Error::other(
            "ext4 source is not the mounted block device",
        ));
    }
    let root = File::open(&mount.mount_point)?;
    if root.metadata()?.dev() != meta.rdev() || sys::filesystem_type(root.as_fd())? != SUPER_MAGIC {
        return Err(io::Error::other("ext4 mount identity changed"));
    }
    let mut superblock = [0; 120];
    device.read_exact_at(&mut superblock, 1024)?;
    superblock_uuid(&superblock)
}

#[derive(Debug, Default)]
pub struct Walk {
    pub tree: InodeTree,
    pub entries: u64,
    pub unreadable: u64,
    pub changed: u64,
    pub skipped_mounts: u64,
    pub stopped: Option<StopReason>,
}

struct Frame {
    dir: File,
    entries: DirectoryEntries,
    path: Vec<u8>,
    node: u32,
    before: FileStat,
}

fn allocation(stat: &FileStat) -> io::Result<InodeAllocation> {
    Ok(InodeAllocation {
        device: stat.device,
        inode: stat.inode,
        kind: FileKind::from_mode(stat.mode),
        logical_bytes: stat.size,
        allocated_bytes: stat
            .blocks
            .checked_mul(512)
            .ok_or_else(|| io::Error::other("allocated bytes overflow"))?,
        links: u32::try_from(stat.links).map_err(|_| io::Error::other("link count overflow"))?,
        ctime: stat.ctime.max(0) as u64,
        uncertain: false,
    })
}

/// `mount_points` are byte paths relative to the selected root. Same-device
/// bind mounts are excluded as well as mounts on other devices.
pub fn walk(
    root: &Path,
    mount_points: &BTreeSet<Vec<u8>>,
    volume_root: bool,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<Walk> {
    let mut walked = Walk::default();
    walk_into(&mut walked, root, mount_points, volume_root, boundary)?;
    Ok(walked)
}

pub fn walk_into(
    walked: &mut Walk,
    root: &Path,
    mount_points: &BTreeSet<Vec<u8>>,
    volume_root: bool,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<()> {
    if let ControlFlow::Break(reason) = boundary.reserve(4096) {
        walked.stopped = Some(reason);
        return Ok(());
    }
    let pin = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    let meta = pin.metadata()?;
    let stat = FileStat {
        device: meta.dev(),
        inode: meta.ino(),
        mode: meta.mode(),
        links: meta.nlink(),
        size: meta.size(),
        blocks: meta.blocks(),
        mtime: meta.mtime(),
        mtime_nsec: meta.mtime_nsec(),
        ctime: meta.ctime(),
        ctime_nsec: meta.ctime_nsec(),
    };
    let node = walked.tree.add_inode_allocation(
        VIRTUAL_ROOT,
        root.as_os_str().as_bytes(),
        allocation(&stat)?,
        volume_root,
    );
    if FileKind::from_mode(stat.mode) != FileKind::Directory {
        return Ok(());
    }
    let dir = sys::openat_noatime(pin.as_fd(), OsStr::new("."), DIRECTORY_FLAGS, 0)?;
    let before = sys::statat(dir.as_fd(), OsStr::new("."))?;
    if (before.device, before.inode) != (stat.device, stat.inode) {
        return Err(io::Error::other("root changed before scan"));
    }
    let entries = DirectoryEntries::open(dir.as_fd())?;
    let mut visited = HashSet::from([stat.inode]);
    let mut stack = vec![Frame {
        dir,
        entries,
        node,
        path: Vec::new(),
        before,
    }];
    while !stack.is_empty() {
        let depth = stack.len();
        let frame = stack.last_mut().expect("nonempty");
        let name = match frame.entries.next() {
            None => {
                if !sys::statat(frame.dir.as_fd(), OsStr::new("."))
                    .is_ok_and(|stat| stat == frame.before)
                {
                    walked.tree.mark_uncertain(frame.node);
                    walked.changed += 1;
                }
                stack.pop();
                continue;
            }
            Some(Err(_)) => {
                walked.tree.mark_uncertain(frame.node);
                walked.unreadable += 1;
                stack.pop();
                continue;
            }
            Some(Ok(name)) => name,
        };
        if let ControlFlow::Break(reason) = boundary.reserve(
            4096 + walked.tree.tree().len() as u64 * 512
                + name.as_bytes().len() as u64
                + frame.path.len() as u64,
        ) {
            walked.stopped = Some(reason);
            break;
        }
        walked.entries += 1;
        boundary.took(&WalkStats {
            batches: u64::from(walked.entries % BATCH == 0),
            items: 1,
            dir_index_items: 1,
            ..WalkStats::default()
        });
        if (walked.entries % BATCH == 0)
            && let ControlFlow::Break(reason) = boundary.after_batch()
        {
            walked.stopped = Some(reason);
            break;
        }
        let mut path = frame.path.clone();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(name.as_bytes());
        if mount_points.contains(&path) {
            walked.skipped_mounts += 1;
            continue;
        }
        let stat = match sys::statat(frame.dir.as_fd(), &name) {
            Ok(stat) => stat,
            Err(_) => {
                walked.unreadable += 1;
                continue;
            }
        };
        if stat.device != before.device {
            walked.skipped_mounts += 1;
            continue;
        }
        let node = walked.tree.add_inode_allocation(
            frame.node,
            name.as_bytes(),
            allocation(&stat)?,
            false,
        );
        if FileKind::from_mode(stat.mode) != FileKind::Directory {
            if !sys::statat(frame.dir.as_fd(), &name).is_ok_and(|after| after == stat) {
                walked.tree.mark_uncertain(node);
                walked.changed += 1;
            }
            continue;
        }
        if depth >= MAX_DEPTH || !visited.insert(stat.inode) {
            walked.tree.mark_uncertain(node);
            walked.unreadable += 1;
            continue;
        }
        let child =
            sys::openat_noatime(frame.dir.as_fd(), &name, DIRECTORY_FLAGS, 0).and_then(|dir| {
                let opened = sys::statat(dir.as_fd(), OsStr::new("."))?;
                if opened != stat {
                    return Err(io::Error::other("directory changed before open"));
                }
                let entries = DirectoryEntries::open(dir.as_fd())?;
                Ok(Frame {
                    dir,
                    entries,
                    node,
                    path,
                    before: opened,
                })
            });
        match child {
            Ok(frame) => stack.push(frame),
            Err(_) => {
                walked.tree.mark_uncertain(node);
                walked.unreadable += 1;
            }
        }
    }
    if walked.stopped.is_none()
        && let ControlFlow::Break(reason) = boundary.after_batch()
    {
        walked.stopped = Some(reason);
    }
    Ok(())
}

/// Feed the existing report/query pipeline without Btrfs namespaces or cache.
pub fn scan(
    env: &crate::resolve::Environment,
    group: &crate::target::FilesystemTargets,
    state_root: &Path,
    spill: crate::spill::Spill,
    budget: u64,
    control: &mut dyn crate::scan::ScanControl,
) -> Result<crate::scan::ScanOutcome, crate::btrfs::walk::WalkError> {
    use crate::report::{Consistency, Coverage, ScanStats};
    use crate::scan::{ScanEvent, ScanOutcome};
    use crate::target::Scope;
    let inputs: Vec<_> = group
        .targets
        .iter()
        .map(|target| spilling::Input {
            path: target.path.clone(),
            volume_root: target.scope == Scope::Volume,
            excluded: env
                .mounts
                .iter()
                .map(|mount| &mount.mount_point)
                .chain(
                    crate::mounts::ecryptfs_mounts(&env.mounts)
                        .iter()
                        .map(|(lower, _)| *lower),
                )
                .filter_map(|path| {
                    path.strip_prefix(&target.path)
                        .ok()
                        .filter(|relative| !relative.as_os_str().is_empty())
                        .map(|relative| relative.as_os_str().as_bytes().to_vec())
                })
                .collect(),
        })
        .collect();
    // Keep the inventory fds pinned for the entire scan and verify that a
    // path still denotes the listed root before trusting its classification.
    let pinned_shares = crate::dsm_shares::list(&group.mount).unwrap_or_default();
    let shares = pinned_shares
        .iter()
        .map(|share| ((share.device, share.inode), share.name.clone()))
        .collect();
    let own_state = std::fs::metadata(state_root)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()));
    let walked = spilling::scan(
        &inputs,
        spill,
        budget,
        control.boundary(),
        &shares,
        own_state,
    )
    .map_err(records::walk_error)?;
    control.event(ScanEvent::Aggregating {
        nodes: walked.nodes.count() as usize,
    });
    let partial = walked.stopped.is_some() || walked.unreadable > 0 || walked.encrypted > 0;
    let consistency = if partial {
        Consistency::Partial
    } else if walked.changed > 0 {
        Consistency::ChangedDuringScan
    } else {
        Consistency::ObservedStable
    };
    let stats = ScanStats {
        entries: walked.totals.entries,
        inodes: walked.inodes,
        batches: walked.totals.entries.div_ceil(BATCH),
        ..ScanStats::default()
    };
    let coverage = Coverage {
        encrypted_directories: walked.encrypted,
        unreadable_roots: Some(walked.unreadable),
        dangling_entries: walked.unreadable,
        reference_accounting: if partial || walked.totals.unknown > 0 {
            "partial"
        } else {
            "complete"
        }
        .into(),
        ..Coverage::default()
    };
    Ok(ScanOutcome {
        candidates: Vec::new(),
        allocated_overview: group
            .targets
            .iter()
            .any(|target| target.scope == Scope::Volume)
            .then_some(walked.overview),
        stopped: walked.stopped,
        totals: walked.totals,
        accounting: walked.accounting,
        nodes: walked.nodes,
        subvolumes: Vec::new(),
        coverage,
        stats,
        consistency,
        change_reasons: Vec::new(),
        state_dir_writes: Vec::new(),
        root_errors: Vec::new(),
        snapshots: None,
        snapshot_error: None,
        space: None,
        overview: None,
        recycle_ctimes: Vec::new(),
        deletion: None,
        deletion_error: None,
    })
}
