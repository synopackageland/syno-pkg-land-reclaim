//! An ext4 walk and hard-link merge whose memory does not grow with the volume.
//! Temporary records use the existing report codec, Appender and ChunkReader;
//! they are never sealed as scan cache units.

use super::records::{self, Order, Reader, Record, Sorter};
use super::{DIRECTORY_FLAGS, MAX_DEPTH, allocation};
use crate::btrfs::walk::{BatchBoundary, StopReason, WalkStats};
use crate::model::accounting::{AccountingSummary, Blockers, Held, Verdict};
use crate::model::namespace::FileKind;
use crate::model::tree::{AllocationOverview, InodeAllocation, Metrics, NodeFlags, Outside};
use crate::overview::PartKind;
use crate::scan::ReportNodes;
use crate::spill::{Appender, Spill};
use crate::sys::{self, DirectoryEntries, FileStat};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::ControlFlow;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

#[derive(Clone)]
pub struct Input {
    pub path: PathBuf,
    pub volume_root: bool,
    pub excluded: BTreeSet<Vec<u8>>,
}

pub struct Result {
    pub nodes: ReportNodes,
    pub totals: Metrics,
    pub accounting: AccountingSummary,
    pub overview: AllocationOverview,
    pub inodes: u64,
    pub unreadable: u64,
    pub changed: u64,
    pub encrypted: u64,
    pub stopped: Option<StopReason>,
}

struct Frame {
    dir: File,
    entries: DirectoryEntries,
    before: FileStat,
    record: Record,
    share: bool,
    volume_root: bool,
}

#[derive(Default)]
struct Counts {
    unreadable: u64,
    changed: u64,
    encrypted: u64,
    entries: u64,
    stopped: Option<StopReason>,
    observed_shares: BTreeSet<Vec<u8>>,
    fingerprint: u64,
}

impl Counts {
    fn observe(&mut self, path: &[u8], stat: FileStat) {
        let mut hash = crate::codec::Fnv::new();
        hash.update(path);
        for value in [
            stat.device,
            stat.inode,
            u64::from(stat.mode),
            stat.links,
            stat.size,
            stat.blocks,
            stat.mtime as u64,
            stat.mtime_nsec as u64,
            stat.ctime as u64,
            stat.ctime_nsec as u64,
        ] {
            hash.update(&value.to_le_bytes());
        }
        // Directory iteration order can change; this observational checksum
        // is order independent and is never a cleanup certificate.
        self.fingerprint = self.fingerprint.wrapping_add(hash.value());
    }
}

type Sink<'a> = dyn FnMut(Record, &mut dyn BatchBoundary) -> io::Result<()> + 'a;

fn raw(path: Vec<u8>, depth: u32, stat: FileStat, flags: NodeFlags) -> io::Result<Record> {
    let alloc = allocation(&stat)?;
    Ok(Record {
        name: path,
        depth,
        kind: alloc.kind,
        flags,
        metrics: Metrics {
            logical: if alloc.kind == FileKind::Regular {
                alloc.logical_bytes
            } else {
                0
            },
            physical: alloc.allocated_bytes,
            ..Metrics::default()
        },
        blockers: Blockers::default(),
        held: Held::default(),
        marker: None,
        outside: None,
        nlink: if alloc.kind == FileKind::Directory {
            1
        } else {
            alloc.links
        },
        device: alloc.device,
        inode: alloc.inode,
    })
}

fn uncertain(record: &mut Record) {
    record.blockers = record.blockers.with(Blockers::UNKNOWN);
    record.metrics.unknown = record.metrics.physical;
}

fn stat_file(file: &File) -> io::Result<FileStat> {
    let meta = file.metadata()?;
    Ok(FileStat {
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
    })
}

fn frame(
    dir: File,
    stat: FileStat,
    mut record: Record,
    share: bool,
    volume_root: bool,
    counts: &mut Counts,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<Frame> {
    if sys::statat(dir.as_fd(), OsStr::new("."))? != stat {
        return Err(io::Error::other("directory changed before open"));
    }
    // Inspect directory markers before visiting children; a marker encountered
    // after a child must still protect that child's package data.
    let check = sys::openat_noatime(dir.as_fd(), OsStr::new("."), DIRECTORY_FLAGS, 0)?;
    for entry in DirectoryEntries::open(check.as_fd())? {
        records::checkpoint(boundary)?;
        let entry = entry?;
        if entry.as_bytes().starts_with(b"ECRYPTFS_FNEK_ENCRYPTED.") {
            record.flags = record.flags.with(NodeFlags::ENCRYPTED);
            uncertain(&mut record);
            counts.encrypted += 1;
        }
        if let Some(marker) = crate::model::tree::inode_marker(entry.as_bytes(), volume_root) {
            record.flags = record.flags.with(NodeFlags::PACKAGE);
            record.marker = Some(marker);
            record.metrics.extents = u64::from(record.depth) + 1;
        }
    }
    let entries = DirectoryEntries::open(dir.as_fd())?;
    Ok(Frame {
        dir,
        entries,
        before: stat,
        record,
        share,
        volume_root,
    })
}

fn walk(
    input: &Input,
    count_entries: bool,
    push: &mut Sink<'_>,
    boundary: &mut dyn BatchBoundary,
    shares: &BTreeMap<(u64, u64), Vec<u8>>,
    own_state: Option<(u64, u64)>,
    counts: &mut Counts,
) -> io::Result<()> {
    let pin = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&input.path)?;
    let stat = stat_file(&pin)?;
    if let Some(name) = shares.get(&(stat.device, stat.inode)) {
        counts.observed_shares.insert(name.clone());
    }
    let mut flags = NodeFlags::default();
    if own_state == Some((stat.device, stat.inode)) {
        flags = flags.with(NodeFlags::OWN_STATE);
    }
    let record = raw(input.path.as_os_str().as_bytes().to_vec(), 0, stat, flags)?;
    counts.observe(&record.name, stat);
    counts.entries += 1;
    if record.kind != FileKind::Directory {
        push(record, boundary)?;
        return Ok(());
    }
    let dir = sys::openat_noatime(pin.as_fd(), OsStr::new("."), DIRECTORY_FLAGS, 0)?;
    let mut stack = vec![frame(
        dir,
        stat,
        record,
        shares.contains_key(&(stat.device, stat.inode)),
        input.volume_root,
        counts,
        boundary,
    )?];
    while !stack.is_empty() {
        if let ControlFlow::Break(reason) = boundary.after_batch() {
            if reason != StopReason::ResourceLimit {
                counts.stopped = Some(reason);
                break;
            }
        }
        let depth = stack.len();
        let parent = stack.last_mut().expect("nonempty");
        let next = if parent.record.flags.contains(NodeFlags::ENCRYPTED)
            || parent.record.flags.contains(NodeFlags::OWN_STATE)
        {
            None
        } else {
            parent.entries.next()
        };
        let name = match next {
            None => {
                let mut done = stack.pop().expect("nonempty");
                if !sys::statat(done.dir.as_fd(), OsStr::new("."))
                    .is_ok_and(|now| now == done.before)
                {
                    uncertain(&mut done.record);
                    counts.changed += 1;
                }
                push(done.record, boundary)?;
                continue;
            }
            Some(Err(_)) => {
                uncertain(&mut parent.record);
                counts.unreadable += 1;
                let done = stack.pop().expect("nonempty");
                push(done.record, boundary)?;
                continue;
            }
            Some(Ok(name)) => name,
        };
        let mut path = parent.record.name.clone();
        if !path.ends_with(b"/") {
            path.push(b'/');
        }
        path.extend_from_slice(name.as_bytes());
        let relative = path
            .strip_prefix(input.path.as_os_str().as_bytes())
            .unwrap_or(&path)
            .strip_prefix(b"/")
            .unwrap_or(&path);
        if input.excluded.contains(relative) {
            continue;
        }
        let stat = match sys::statat(parent.dir.as_fd(), &name) {
            Ok(stat) => stat,
            Err(_) => {
                counts.unreadable += 1;
                counts.changed += 1;
                uncertain(&mut parent.record);
                continue;
            }
        };
        if stat.device != parent.before.device {
            continue;
        }
        if let Some(name) = shares.get(&(stat.device, stat.inode)) {
            counts.observed_shares.insert(name.clone());
        }
        // Also refuse a directory inode already pinned in this ancestry.
        let cycle = stack.iter().any(|frame| frame.before.inode == stat.inode);
        let parent = stack.last_mut().expect("nonempty");
        let mut flags = crate::model::tree::inode_child_flags(
            parent.record.flags,
            name.as_bytes(),
            parent.volume_root,
        );
        flags = NodeFlags::from_bits(flags.bits() & !NodeFlags::RECYCLE.bits());
        if parent.record.flags.contains(NodeFlags::RECYCLE)
            || (parent.share && name.as_bytes() == b"#recycle")
        {
            flags = flags.with(NodeFlags::RECYCLE);
        }
        if own_state == Some((stat.device, stat.inode)) {
            flags = flags.with(NodeFlags::OWN_STATE);
        }
        let mut record = raw(path, depth as u32, stat, flags)?;
        // Scratch field in the existing codec: deepest package/recycle cut.
        // Cleared before publishing report metrics; no extent is fabricated.
        record.metrics.extents = parent.record.metrics.extents;
        if flags.contains(NodeFlags::PACKAGE)
            || (flags.contains(NodeFlags::RECYCLE)
                && !parent.record.flags.contains(NodeFlags::RECYCLE))
        {
            record.metrics.extents = depth as u64 + 1;
        }
        counts.observe(&record.name, stat);
        counts.entries += 1;
        boundary.took(&WalkStats {
            items: 1,
            dir_index_items: u64::from(count_entries),
            ..WalkStats::default()
        });
        if record.kind != FileKind::Directory {
            if !sys::statat(parent.dir.as_fd(), &name).is_ok_and(|now| now == stat) {
                uncertain(&mut record);
                counts.changed += 1;
            }
            push(record, boundary)?;
            continue;
        }
        if depth >= MAX_DEPTH || cycle {
            uncertain(&mut record);
            counts.unreadable += 1;
            push(record, boundary)?;
            continue;
        }
        let opened = sys::openat_noatime(parent.dir.as_fd(), &name, DIRECTORY_FLAGS, 0);
        match opened.and_then(|dir| {
            frame(
                dir,
                stat,
                record.clone(),
                shares.contains_key(&(stat.device, stat.inode)),
                false,
                counts,
                boundary,
            )
        }) {
            Ok(frame) => stack.push(frame),
            Err(_) => {
                uncertain(&mut record);
                counts.unreadable += 1;
                push(record, boundary)?;
            }
        }
    }
    // A cancelled prefix still has every ancestor needed by the report.
    for mut frame in stack.into_iter().rev() {
        uncertain(&mut frame.record);
        push(frame.record, boundary)?;
    }
    Ok(())
}

/// Full paths of this node and its selected ancestors, root first.
fn prefixes(record: &Record) -> Vec<(Vec<u8>, u32)> {
    let mut path = record.name.clone();
    let mut result = vec![(path.clone(), record.depth)];
    for depth in (0..record.depth).rev() {
        let end = path
            .iter()
            .rposition(|&byte| byte == b'/')
            .expect("child path");
        path.truncate(end.max(1));
        result.push((path.clone(), depth));
    }
    result.reverse();
    result
}

struct Prefix {
    path: Vec<u8>,
    depth: u32,
    count: u64,
    non_recycle: u64,
    visible: u64,
    visible_non_recycle: u64,
    directory: bool,
}

fn finish_prefix(
    prefix: Prefix,
    allocation: &InodeAllocation,
    seen: u64,
    non_recycle: u64,
    sorter: &mut Sorter,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<()> {
    let (metrics, held, blockers) = allocation.subtree_totals(
        seen,
        prefix.count,
        prefix.non_recycle == non_recycle,
        prefix.directory,
    );
    let mut record = Record::contribution(prefix.path, prefix.depth, metrics);
    record.blockers = blockers;
    record.held = held;
    let (metrics, held, _) = if prefix.visible > 0 {
        allocation.subtree_totals(
            seen,
            prefix.visible,
            prefix.visible_non_recycle == non_recycle,
            prefix.directory,
        )
    } else {
        (Metrics::default(), Held::default(), Blockers::default())
    };
    record.outside = Some(Outside { metrics, held });
    sorter.push(record, boundary)
}

fn owner(
    record: &Record,
    shares: &BTreeSet<Vec<u8>>,
    inputs: &[Input],
) -> (PartKind, Vec<u8>, Option<u16>) {
    let mount = inputs.iter().find(|input| {
        input.volume_root && record.name.starts_with(input.path.as_os_str().as_bytes())
    });
    let Some(mount) = mount else {
        return (PartKind::SystemOther, Vec::new(), None);
    };
    let relative = record
        .name
        .strip_prefix(mount.path.as_os_str().as_bytes())
        .unwrap_or_default()
        .strip_prefix(b"/")
        .unwrap_or_default();
    let parts: Vec<_> = relative.split(|&byte| byte == b'/').collect();
    for (index, name) in parts.iter().enumerate() {
        if let Some(service) = crate::dsm::service(
            name,
            if index == 0 {
                crate::dsm::Depth::VolumeRoot
            } else {
                crate::dsm::Depth::Below
            },
        ) {
            return (
                PartKind::DsmService,
                Vec::new(),
                Some(crate::dsm::service_id(service)),
            );
        }
    }
    let name = parts.first().copied().unwrap_or_default().to_vec();
    if shares.contains(&name) {
        (
            if record.flags.contains(NodeFlags::RECYCLE) {
                PartKind::RecycleBin
            } else {
                PartKind::ShareLive
            },
            name,
            None,
        )
    } else {
        (PartKind::SystemOther, Vec::new(), None)
    }
}

pub fn scan(
    inputs: &[Input],
    spill: Spill,
    budget: u64,
    boundary: &mut dyn BatchBoundary,
    shares: &BTreeMap<(u64, u64), Vec<u8>>,
    own_state: Option<(u64, u64)>,
) -> io::Result<Result> {
    boundary.phase("scan");
    let mut inode_sorter = Sorter::new(spill.dir().to_path_buf(), "inodes", Order::Inode, budget);
    let mut counts = Counts::default();
    for input in inputs {
        walk(
            input,
            true,
            &mut |record, boundary| inode_sorter.push(record, boundary),
            boundary,
            shares,
            own_state,
            &mut counts,
        )?;
        if counts.stopped.is_some() {
            break;
        }
    }
    boundary.phase("aggregate");
    inode_sorter.finish(spill.path("inodes"), boundary)?;
    let mut reader = Reader::new(spill.path("inodes"));
    let mut pending = reader.read_tracked(boundary)?;
    let mut nodes = Sorter::new(spill.dir().to_path_buf(), "nodes", Order::Path, budget);
    let mut totals = Metrics::default();
    let mut accounting = AccountingSummary::default();
    let mut inodes = 0;
    let mut overview = BTreeMap::<(PartKind, Vec<u8>, Option<u16>), (u64, u64, u64)>::new();
    while let Some(first) = pending.take() {
        records::checkpoint(boundary)?;
        let mut alloc = InodeAllocation {
            device: first.device,
            inode: first.inode,
            kind: first.kind,
            logical_bytes: first.metrics.logical,
            allocated_bytes: first.metrics.physical,
            links: first.nlink,
            ctime: 0,
            uncertain: (first.blockers.bits() & Blockers::UNKNOWN.bits() != 0),
        };
        let path = spill.path("inode-group");
        // Most inodes have one name. Keep that one record in memory; only
        // multi-link groups need their own bounded, rewindable spill file.
        let mut single = None;
        let mut group: Option<Appender> = None;
        let mut seen = 0;
        let mut non_recycle = 0;
        let mut owners = BTreeSet::new();
        let mut record = first;
        loop {
            if seen % 256 == 0 {
                records::checkpoint(boundary)?;
            }
            alloc.uncertain |= (record.blockers.bits() & Blockers::UNKNOWN.bits() != 0)
                || record.nlink != alloc.links
                || record.metrics.physical != alloc.allocated_bytes
                || record.metrics.logical != alloc.logical_bytes
                || record.kind != alloc.kind;
            alloc.allocated_bytes = alloc.allocated_bytes.max(record.metrics.physical);
            alloc.logical_bytes = alloc.logical_bytes.max(record.metrics.logical);
            seen += 1;
            non_recycle += u64::from(!record.flags.contains(NodeFlags::RECYCLE));
            owners.insert(owner(&record, &counts.observed_shares, inputs));
            if seen == 1 {
                single = Some(record);
            } else {
                if group.is_none() {
                    File::create(&path)?;
                    let mut out = Appender::new(path.clone());
                    records::append(&mut out, &single.take().expect("first name"))?;
                    group = Some(out);
                }
                records::append(group.as_mut().expect("multi-link group"), &record)?;
            }
            pending = reader.read_tracked(boundary)?;
            match pending.take() {
                Some(next) if (next.device, next.inode) == (alloc.device, alloc.inode) => {
                    record = next
                }
                next => {
                    pending = next;
                    break;
                }
            }
        }
        if let Some(group) = &mut group {
            group.flush()?;
        }
        inodes += 1;
        let judgement = alloc.judgement(seen);
        accounting.count(&judgement);
        totals.logical = totals
            .logical
            .saturating_add(if alloc.kind == FileKind::Regular {
                alloc.logical_bytes
            } else {
                0
            });
        totals.physical = totals.physical.saturating_add(alloc.allocated_bytes);
        if judgement.verdict == Some(Verdict::Reclaimable) {
            totals.reclaimable = totals.reclaimable.saturating_add(alloc.allocated_bytes);
        }
        if judgement.verdict == Some(Verdict::Unknown) {
            totals.unknown = totals.unknown.saturating_add(alloc.allocated_bytes);
        }
        let key = if alloc.uncertain {
            (PartKind::Unknown, Vec::new(), None)
        } else if owners.len() > 1 {
            (PartKind::SharedBetweenFolders, Vec::new(), None)
        } else {
            owners.into_iter().next().expect("observed inode")
        };
        let row = overview.entry(key).or_default();
        row.0 += alloc.allocated_bytes;
        if judgement.verdict == Some(Verdict::Reclaimable) {
            row.1 += alloc.allocated_bytes;
        }
        if judgement.verdict == Some(Verdict::Unknown) {
            row.2 += alloc.allocated_bytes;
        }
        let mut group = group.map(|_| Reader::new(path));
        let mut stack: Vec<Prefix> = Vec::new();
        loop {
            let next = match &mut group {
                Some(reader) => reader.read_tracked(boundary)?,
                None => single.take(),
            };
            let Some(mut record) = next else { break };
            records::checkpoint(boundary)?;
            let paths = prefixes(&record);
            let common = stack
                .iter()
                .zip(&paths)
                .take_while(|(a, b)| a.path == b.0)
                .count();
            while stack.len() > common {
                finish_prefix(
                    stack.pop().expect("nonempty"),
                    &alloc,
                    seen,
                    non_recycle,
                    &mut nodes,
                    boundary,
                )?;
            }
            for (name, depth) in paths.iter().skip(common) {
                stack.push(Prefix {
                    path: name.clone(),
                    depth: *depth,
                    count: 0,
                    non_recycle: 0,
                    visible: 0,
                    visible_non_recycle: 0,
                    directory: *depth < record.depth || record.kind == FileKind::Directory,
                });
            }
            let cut = record.metrics.extents.checked_sub(1);
            for prefix in &mut stack {
                prefix.visible_non_recycle += u64::from(
                    !record.flags.contains(NodeFlags::RECYCLE)
                        && cut.is_none_or(|cut| cut <= u64::from(prefix.depth)),
                );
                prefix.visible += u64::from(cut.is_none_or(|cut| cut <= u64::from(prefix.depth)));
                prefix.count += 1;
                prefix.non_recycle += u64::from(!record.flags.contains(NodeFlags::RECYCLE));
            }
            let own = Metrics {
                entries: 1,
                files: u64::from(record.kind == FileKind::Regular),
                ..Metrics::default()
            };
            totals.entries += 1;
            totals.files += own.files;
            for (name, depth) in paths {
                let mut contribution = Record::contribution(name, depth, own);
                contribution.outside = Some(Outside {
                    metrics: if cut.is_none_or(|cut| cut <= u64::from(depth)) {
                        own
                    } else {
                        Metrics::default()
                    },
                    held: Held::default(),
                });
                nodes.push(contribution, boundary)?;
            }
            record.metrics = Metrics::default();
            record.outside = (record.kind == FileKind::Directory).then(Outside::default);
            record.blockers = Blockers::default();
            nodes.push(record, boundary)?;
        }
        while let Some(prefix) = stack.pop() {
            finish_prefix(prefix, &alloc, seen, non_recycle, &mut nodes, boundary)?;
        }
    }
    nodes.finish(spill.path("nodes"), boundary)?;
    // Re-read metadata after aggregation: writes to an already visited file
    // need not change its parent directory's timestamps. A bounded second
    // walk catches those changes without storing an inode table in RAM.
    let mut final_counts = Counts::default();
    for input in inputs {
        walk(
            input,
            false,
            &mut |_, boundary| records::checkpoint(boundary),
            boundary,
            shares,
            own_state,
            &mut final_counts,
        )?;
    }
    if final_counts.fingerprint != counts.fingerprint || final_counts.entries != counts.entries {
        counts.changed += 1;
    }
    counts.changed += final_counts.changed;
    counts.unreadable = counts.unreadable.max(final_counts.unreadable);
    let overview = AllocationOverview {
        items: overview
            .into_iter()
            .map(
                |((kind, name, service), (bytes, reclaimable_bytes, unknown_bytes))| {
                    crate::model::tree::AllocationItem {
                        kind,
                        name_hex: crate::report::hex_bytes::encode(&name),
                        service,
                        bytes,
                        reclaimable_bytes,
                        unknown_bytes,
                    }
                },
            )
            .collect(),
    };
    Ok(Result {
        nodes: ReportNodes::Stream {
            spill,
            count: counts.entries,
        },
        totals,
        accounting,
        overview,
        inodes,
        unreadable: counts.unreadable,
        changed: counts.changed,
        encrypted: counts.encrypted,
        stopped: counts.stopped,
    })
}
