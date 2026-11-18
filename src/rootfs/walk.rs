//! A bounded, paced walk of one filesystem from a root, like `du -x`:
//! allocated bytes per first- and second-level path, and the entries the
//! fixed table lets `rootfs plan` move. Read-only; never follows symlinks
//! and never crosses into another mount.

use super::classify::{Category, Kind, classify, classify_deep, deepen, package_name};
use super::plan::CleanupMode;
use crate::btrfs::walk::{BatchBoundary, StopReason};
use crate::sys::{self, DirectoryEntries, FileStat};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::ops::ControlFlow;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Entries between two pacing boundaries.
pub const BATCH_ENTRIES: u64 = 256;
/// Paths the walk lists as not walked, at most.
const MAX_NOT_WALKED: usize = 64;

/// How far the walk may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_entries: u64,
    pub max_depth: usize,
}

impl Default for Limits {
    /// Four times the 524,288 inodes of a DSM 7.9 GiB system partition;
    /// deeper than any DSM or `node_modules` tree seen.
    fn default() -> Self {
        Limits {
            max_entries: 2_000_000,
            max_depth: 128,
        }
    }
}

/// The identity `rootfs apply` checks again before changing a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
    /// `st_mode & S_IFMT`.
    pub file_type: u32,
    pub size: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    /// `st_nlink`: a candidate file has exactly one name.
    pub nlink: u64,
    /// `st_ctime`: an update leftover changed since the last boot stays.
    pub ctime: i64,
}

impl Identity {
    pub fn of(stat: &FileStat) -> Self {
        Identity {
            device: stat.device,
            inode: stat.inode,
            file_type: stat.mode & libc::S_IFMT,
            size: stat.size,
            mtime: stat.mtime,
            mtime_nsec: stat.mtime_nsec,
            nlink: stat.links,
            ctime: stat.ctime,
        }
    }
}

/// An entry the fixed table lets `rootfs plan` move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(with = "crate::report::hex_bytes")]
    pub path: Vec<u8>,
    pub kind: Kind,
    pub category: Category,
    pub identity: Identity,
    /// `st_blocks × 512`; a directory's own blocks only.
    pub allocated_bytes: u64,
}

/// Why the kernel would refuse to remove an entry the table lets move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pinned {
    /// A named image resource whose bytes are not a recognized image.
    UnrecognisedImage,
    /// `chattr +a` (`FS_APPEND_FL`), as DSM sets on `/var/log/rm.log*`.
    AppendOnly,
    /// `chattr +i` (`FS_IMMUTABLE_FL`).
    Immutable,
}

/// A movable entry left out of the candidates because it is pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedEntry {
    pub path: Vec<u8>,
    pub reason: Pinned,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotWalkedReason {
    /// Another filesystem is mounted there (`/volume1`, tmpfs, USB).
    MountPoint,
    /// The directory could not be opened or read.
    Unreadable,
    /// Deeper than the walk's depth limit.
    TooDeep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotWalked {
    pub path: Vec<u8>,
    pub reason: NotWalkedReason,
}

/// Why the walk ended early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkStop {
    EntryLimit,
    Stopped(StopReason),
}

/// Who a byte on the system partition belongs to: a category, and for
/// package data its package, for user tooling its folder.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Owner {
    pub category: Category,
    pub name: Option<Vec<u8>>,
}

impl Owner {
    pub fn of(path: &[u8], category: Category) -> Self {
        let name = match category {
            Category::PackageData => package_name(path).map(<[u8]>::to_vec),
            Category::UserTooling if path.starts_with(b"/root") => Some(b"/root".to_vec()),
            Category::UserTooling => Some(b"/usr/local".to_vec()),
            _ => None,
        };
        Owner { category, name }
    }
}

#[derive(Debug, Default)]
pub struct Walk {
    /// Entries below the root, the root itself not included.
    pub entries: u64,
    /// Allocated bytes of the root and every entry, hard links once.
    pub allocated_bytes: u64,
    /// Allocated bytes below each first- and second-level path.
    pub rows: BTreeMap<Vec<u8>, u64>,
    /// Allocated bytes of the entries below the root per owner.
    pub owners: BTreeMap<Owner, u64>,
    /// Sorted by path.
    pub candidates: Vec<Candidate>,
    /// Entries the table lets move but the kernel would not let apply
    /// remove, sorted by path.
    pub pinned: Vec<PinnedEntry>,
    /// At most `MAX_NOT_WALKED`, in walk order.
    pub not_walked: Vec<NotWalked>,
    /// Directories that could not be read.
    pub unreadable: u64,
    pub stopped: Option<WalkStop>,
}

struct Frame {
    dir: File,
    path: Vec<u8>,
    entries: DirectoryEntries,
}

/// Reads an open entry's inode flags (`sys::inode_flags`).
pub type InodeFlags<'a> = &'a dyn Fn(BorrowedFd<'_>) -> io::Result<u32>;

/// Walks the filesystem of `root`, entering neither `mount_points`
/// (paths relative to `root`) nor any directory on another device.
pub fn walk(
    root: &Path,
    mount_points: &BTreeSet<Vec<u8>>,
    limits: Limits,
    boundary: &mut dyn BatchBoundary,
) -> io::Result<Walk> {
    walk_reading_flags(root, mount_points, limits, boundary, &sys::inode_flags)
}

/// `walk`, reading each movable entry's inode flags with `flags`.
pub fn walk_reading_flags(
    root: &Path,
    mount_points: &BTreeSet<Vec<u8>>,
    limits: Limits,
    boundary: &mut dyn BatchBoundary,
    flags: InodeFlags<'_>,
) -> io::Result<Walk> {
    walk_mode(
        root,
        mount_points,
        limits,
        boundary,
        flags,
        CleanupMode::Standard,
    )
}

pub fn walk_mode(
    root: &Path,
    mount_points: &BTreeSet<Vec<u8>>,
    limits: Limits,
    boundary: &mut dyn BatchBoundary,
    flags: InodeFlags<'_>,
    mode: CleanupMode,
) -> io::Result<Walk> {
    let dir = File::open(root)?;
    let meta = dir.metadata()?;
    let device = meta.dev();
    let mut walked = Walk {
        allocated_bytes: meta.blocks() * 512,
        ..Walk::default()
    };
    let mut linked = HashSet::new();
    let entries = DirectoryEntries::open(dir.as_fd())?;
    let mut stack = vec![Frame {
        dir,
        path: Vec::new(),
        entries,
    }];
    while let Some(frame) = stack.last_mut() {
        let Some(name) = frame.entries.next() else {
            stack.pop();
            continue;
        };
        let Ok(name) = name else {
            walked.unreadable_dir(&frame.path);
            stack.pop();
            continue;
        };
        let mut path = frame.path.clone();
        path.push(b'/');
        path.extend_from_slice(name.as_bytes());
        let Ok(stat) = sys::statat(frame.dir.as_fd(), &name) else {
            continue;
        };
        let is_dir = stat.mode & libc::S_IFMT == libc::S_IFDIR;
        if stat.device != device || mount_points.contains(&path) {
            walked.not_walked(path, NotWalkedReason::MountPoint);
            continue;
        }
        walked.entries += 1;
        let dir = &stack.last().expect("frame").dir;
        walked.count(&path, &stat, &mut linked, mode, || {
            if classify_deep(&path, if is_dir { Kind::Dir } else { Kind::File }).category
                == Category::PreferenceImage
            {
                use std::os::unix::fs::FileExt;
                let mut header = [0; 12];
                let image = sys::openat(
                    dir.as_fd(),
                    &name,
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    0,
                )
                .ok()
                .and_then(|file| file.read_at(&mut header, 0).ok())
                .is_some_and(|n| super::classify::image_header(&header[..n]));
                if !image {
                    return Some(Pinned::UnrecognisedImage);
                }
            }
            pinned(dir, &name, &stat, flags)
        });
        if let Some(stop) = walked.limit_reached(limits, boundary) {
            walked.stopped = Some(stop);
            break;
        }
        if !is_dir {
            continue;
        }
        if stack.len() > limits.max_depth {
            walked.not_walked(path, NotWalkedReason::TooDeep);
            continue;
        }
        match open_dir(&stack.last().expect("frame").dir, &name, &stat) {
            Ok((dir, entries)) => stack.push(Frame { dir, path, entries }),
            Err(_) => walked.unreadable_dir(&path),
        }
    }
    walked.candidates.sort_by(|a, b| a.path.cmp(&b.path));
    walked.pinned.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(walked)
}

/// Whether entry `name` of `parent`, still the object `stat` describes,
/// is append-only or immutable. An entry that cannot be opened or read is
/// not pinned here; apply checks it again.
fn pinned(parent: &File, name: &OsStr, stat: &FileStat, flags: InodeFlags<'_>) -> Option<Pinned> {
    let open =
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
    let entry = sys::openat(parent.as_fd(), name, open, 0).ok()?;
    let opened = entry.metadata().ok()?;
    if opened.dev() != stat.device || opened.ino() != stat.inode {
        return None;
    }
    let flags = flags(entry.as_fd()).ok()?;
    if flags & sys::FS_IMMUTABLE_FL != 0 {
        Some(Pinned::Immutable)
    } else if flags & sys::FS_APPEND_FL != 0 {
        Some(Pinned::AppendOnly)
    } else {
        None
    }
}

/// Opens directory `name` if it is still the object `stat` describes.
fn open_dir(parent: &File, name: &OsStr, stat: &FileStat) -> io::Result<(File, DirectoryEntries)> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let dir = sys::openat(parent.as_fd(), name, flags, 0)?;
    let opened = dir.metadata()?;
    if opened.dev() != stat.device || opened.ino() != stat.inode {
        return Err(io::Error::other("directory replaced during the walk"));
    }
    let entries = DirectoryEntries::open(dir.as_fd())?;
    Ok((dir, entries))
}

impl Walk {
    /// Adds an entry's bytes to the total and its rows, and keeps it as
    /// a candidate when the table lets it move and `pinned` finds it
    /// neither append-only nor immutable.
    fn count(
        &mut self,
        path: &[u8],
        stat: &FileStat,
        linked: &mut HashSet<u64>,
        mode: CleanupMode,
        pinned: impl FnOnce() -> Option<Pinned>,
    ) {
        let file_type = stat.mode & libc::S_IFMT;
        let repeated_link =
            file_type != libc::S_IFDIR && stat.links > 1 && !linked.insert(stat.inode);
        let bytes = if repeated_link { 0 } else { stat.blocks * 512 };
        self.allocated_bytes += bytes;
        for row in row_prefixes(path) {
            *self.rows.entry(row.to_vec()).or_default() += bytes;
        }
        let kind = match file_type {
            libc::S_IFDIR => Kind::Dir,
            _ => Kind::File,
        };
        // Owners stay Standard's, so one deep walk serves both modes' report.
        let standard = classify(path, kind);
        let class = if mode == CleanupMode::Deep {
            deepen(standard, path, kind)
        } else {
            standard
        };
        let owner = standard.category;
        *self.owners.entry(Owner::of(path, owner)).or_default() += bytes;
        // A file with another name elsewhere frees nothing when moved,
        // and that name may be one no one should see copied (/etc/shadow).
        let movable_type =
            file_type == libc::S_IFDIR || (file_type == libc::S_IFREG && stat.links == 1);
        if !(class.movable && movable_type) {
            return;
        }
        if let Some(reason) = pinned() {
            self.pinned.push(PinnedEntry {
                path: path.to_vec(),
                reason,
                allocated_bytes: bytes,
            });
        } else {
            self.candidates.push(Candidate {
                path: path.to_vec(),
                kind,
                category: class.category,
                identity: Identity::of(stat),
                allocated_bytes: bytes,
            });
        }
    }

    fn limit_reached(&self, limits: Limits, boundary: &mut dyn BatchBoundary) -> Option<WalkStop> {
        if self.entries >= limits.max_entries {
            return Some(WalkStop::EntryLimit);
        }
        if self.entries % BATCH_ENTRIES == 0 {
            if let ControlFlow::Break(reason) = boundary.after_batch() {
                return Some(WalkStop::Stopped(reason));
            }
        }
        None
    }

    fn unreadable_dir(&mut self, path: &[u8]) {
        self.unreadable += 1;
        self.not_walked(path.to_vec(), NotWalkedReason::Unreadable);
    }

    fn not_walked(&mut self, path: Vec<u8>, reason: NotWalkedReason) {
        if self.not_walked.len() < MAX_NOT_WALKED {
            self.not_walked.push(NotWalked { path, reason });
        }
    }
}

/// The first- and second-level paths `path` lies in or is.
fn row_prefixes(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    let slashes = path
        .iter()
        .enumerate()
        .filter(|&(_, &b)| b == b'/')
        .map(|(i, _)| i);
    slashes
        .skip(1)
        .take(2)
        .chain(std::iter::once(path.len()))
        .take(2)
        .map(|end| &path[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::walk::{BatchBoundary, StopReason};
    use std::ops::ControlFlow;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    struct Unpaced(u64);

    impl BatchBoundary for Unpaced {
        fn after_batch(&mut self) -> ControlFlow<StopReason> {
            self.0 += 1;
            ControlFlow::Continue(())
        }
    }

    fn fake_root(tag: &str) -> PathBuf {
        let root = crate::spill::testing::scratch(tag);
        for dir in ["var/log/disk-latency", "usr/syno/bin", "upd@te/sub", "root"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for (path, len) in [
            ("var/log/messages", 8192),
            ("var/log/disk-latency/a.xz", 16384),
            ("usr/syno/bin/synocleaner", 4096),
            ("upd@te/sub/hda1.tgz", 12288),
            (".SynoUpgrade.tar", 4096),
        ] {
            std::fs::write(root.join(path), vec![7u8; len]).unwrap();
        }
        root
    }

    fn allocated(path: &Path) -> u64 {
        std::fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    fn below(root: &Path, paths: &[&str]) -> u64 {
        paths.iter().map(|p| allocated(&root.join(p))).sum()
    }

    fn run(root: &Path) -> Walk {
        walk(
            root,
            &Default::default(),
            Limits::default(),
            &mut Unpaced(0),
        )
        .unwrap()
    }

    #[test]
    fn kb_deep_walk_keeps_symlinks_and_non_image_preferences() {
        let root = fake_root("kb-deep-walk");
        for dir in [
            "usr/syno/etc/preference/test/wallpaper_dir",
            "var/db/geoip-database",
            "var/lib/samba",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join("usr/syno/etc/preference/test/wallpaper"),
            b"\x89PNG\r\n\x1a\nimage",
        )
        .unwrap();
        std::fs::write(
            root.join("usr/syno/etc/preference/test/wallpaper_hd"),
            b"not an image",
        )
        .unwrap();
        std::fs::write(
            root.join("usr/syno/etc/preference/test/wallpaper_dir/0_hd.jpg"),
            b"\xff\xd8\xffimage",
        )
        .unwrap();
        std::fs::write(root.join("var/db/geoip-database/cache"), b"cache").unwrap();
        std::fs::create_dir_all(root.join("root/.npm/_cacache/index-v5")).unwrap();
        std::fs::write(root.join("root/.npm/_cacache/index-v5/x"), vec![1u8; 8192]).unwrap();
        std::os::unix::fs::symlink(
            "/volume1/cache",
            root.join("var/lib/samba/winbindd_cache.tdb"),
        )
        .unwrap();
        let deep = walk_mode(
            &root,
            &Default::default(),
            Limits::default(),
            &mut Unpaced(0),
            &|_| Ok(0),
            CleanupMode::Deep,
        )
        .unwrap();
        let paths: Vec<_> = deep.candidates.iter().map(|c| c.path.as_slice()).collect();
        assert!(paths.contains(&b"/usr/syno/etc/preference/test/wallpaper".as_slice()));
        assert!(
            paths.contains(&b"/usr/syno/etc/preference/test/wallpaper_dir/0_hd.jpg".as_slice())
        );
        assert!(paths.contains(&b"/var/db/geoip-database/cache".as_slice()));
        assert!(paths.contains(&b"/root/.npm/_cacache/index-v5/x".as_slice()));
        assert!(!paths.contains(&b"/root/.npm/_cacache".as_slice()));
        assert!(!paths.contains(&b"/var/lib/samba/winbindd_cache.tdb".as_slice()));
        // Owners keep Standard's table, so the report's rows do not change.
        assert!(
            deep.owners
                .keys()
                .all(|o| o.category != Category::ToolCache
                    && o.category != Category::PreferenceImage)
        );
        assert!(deep.owners.contains_key(&Owner {
            category: Category::UserTooling,
            name: Some(b"/root".to_vec()),
        }));
        assert!(
            deep.pinned
                .iter()
                .any(|p| p.reason == Pinned::UnrecognisedImage)
        );
        assert!(!run(&root).candidates.iter().any(|c| matches!(
            c.category,
            Category::PreferenceImage | Category::GeoipDatabase | Category::SambaCache
        )));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn allocated_bytes_add_up_per_first_and_second_level_path() {
        let root = fake_root("rootfs-walk-rows");
        let walked = run(&root);
        let var_log = [
            "var/log",
            "var/log/messages",
            "var/log/disk-latency",
            "var/log/disk-latency/a.xz",
        ];
        assert_eq!(walked.rows[&b"/var/log".to_vec()], below(&root, &var_log));
        let var: Vec<&str> = std::iter::once("var").chain(var_log).collect();
        assert_eq!(walked.rows[&b"/var".to_vec()], below(&root, &var));
        assert_eq!(
            walked.rows[&b"/.SynoUpgrade.tar".to_vec()],
            below(&root, &[".SynoUpgrade.tar"])
        );
        assert!(!walked.rows.contains_key(b"/var/log/messages".as_slice()));
        assert_eq!(walked.entries, 14);
        assert_eq!(
            walked.allocated_bytes,
            allocated(&root)
                + walked
                    .rows
                    .iter()
                    .filter(|(path, _)| path.iter().filter(|&&b| b == b'/').count() == 1)
                    .map(|(_, bytes)| bytes)
                    .sum::<u64>()
        );
        assert_eq!(walked.stopped, None);
    }

    #[test]
    fn movable_entries_are_candidates_with_their_identity() {
        let root = fake_root("rootfs-walk-candidates");
        let walked = run(&root);
        let paths: Vec<&[u8]> = walked
            .candidates
            .iter()
            .map(|c| c.path.as_slice())
            .collect();
        assert_eq!(
            paths,
            [
                &b"/.SynoUpgrade.tar"[..],
                b"/upd@te/sub",
                b"/upd@te/sub/hda1.tgz",
                b"/var/log/disk-latency/a.xz",
            ]
        );
        let archive = walked.candidates.last().unwrap();
        let meta = std::fs::symlink_metadata(root.join("var/log/disk-latency/a.xz")).unwrap();
        assert_eq!(archive.category, Category::LogArchive);
        assert_eq!(archive.kind, Kind::File);
        assert_eq!(archive.allocated_bytes, meta.blocks() * 512);
        assert_eq!(
            archive.identity,
            Identity {
                device: meta.dev(),
                inode: meta.ino(),
                file_type: meta.mode() & libc::S_IFMT,
                size: 16384,
                mtime: meta.mtime(),
                mtime_nsec: meta.mtime_nsec(),
                nlink: 1,
                ctime: meta.ctime(),
            }
        );
        assert_eq!(walked.candidates[1].kind, Kind::Dir);
    }

    #[test]
    fn symlinks_are_counted_but_never_followed_or_moved() {
        let root = fake_root("rootfs-walk-symlink");
        std::os::unix::fs::symlink(root.join("usr"), root.join("upd@te/link")).unwrap();
        std::os::unix::fs::symlink("/etc", root.join("var/crash")).unwrap();
        let walked = run(&root);
        assert!(walked.candidates.iter().all(|c| !c.path.ends_with(b"link")));
        assert_eq!(walked.entries, 16);
        assert!(!walked.rows.contains_key(b"/upd@te/link/syno".as_slice()));
    }

    #[test]
    fn hard_links_are_counted_once() {
        let root = fake_root("rootfs-walk-hardlink");
        let before = run(&root).allocated_bytes;
        std::fs::hard_link(root.join("var/log/messages"), root.join("root/messages")).unwrap();
        let after = run(&root);
        assert_eq!(after.allocated_bytes, before);
        assert_eq!(after.entries, 15);
    }

    #[test]
    fn an_append_only_or_immutable_entry_is_never_a_candidate_but_is_listed() {
        let root = fake_root("rootfs-walk-pinned");
        let append = std::fs::metadata(root.join("var/log/disk-latency/a.xz"))
            .unwrap()
            .ino();
        let immutable = std::fs::metadata(root.join("upd@te/sub")).unwrap().ino();
        let flags = |fd: std::os::fd::BorrowedFd<'_>| {
            let inode = File::from(fd.try_clone_to_owned()?).metadata()?.ino();
            Ok(match inode {
                i if i == append => sys::FS_APPEND_FL,
                i if i == immutable => sys::FS_IMMUTABLE_FL,
                _ => 0,
            })
        };
        let walked = walk_reading_flags(
            &root,
            &Default::default(),
            Limits::default(),
            &mut Unpaced(0),
            &flags,
        )
        .unwrap();
        let paths: Vec<&[u8]> = walked
            .candidates
            .iter()
            .map(|c| c.path.as_slice())
            .collect();
        assert_eq!(paths, [&b"/.SynoUpgrade.tar"[..], b"/upd@te/sub/hda1.tgz"]);
        let pinned: Vec<(&[u8], Pinned)> = walked
            .pinned
            .iter()
            .map(|p| (p.path.as_slice(), p.reason))
            .collect();
        assert_eq!(
            pinned,
            [
                (&b"/upd@te/sub"[..], Pinned::Immutable),
                (b"/var/log/disk-latency/a.xz", Pinned::AppendOnly),
            ]
        );
    }

    #[test]
    fn a_plain_file_reads_as_neither_append_only_nor_immutable() {
        let root = fake_root("rootfs-walk-flags");
        let file = File::open(root.join(".SynoUpgrade.tar")).unwrap();
        let flags = sys::inode_flags(file.as_fd()).unwrap();
        assert_eq!(flags & (sys::FS_APPEND_FL | sys::FS_IMMUTABLE_FL), 0);
    }

    #[test]
    fn a_file_with_another_hard_link_is_never_a_candidate() {
        let root = fake_root("rootfs-walk-hardlinked-candidate");
        std::fs::hard_link(root.join(".SynoUpgrade.tar"), root.join("root/upgrade")).unwrap();
        let walked = run(&root);
        let paths: Vec<&[u8]> = walked
            .candidates
            .iter()
            .map(|c| c.path.as_slice())
            .collect();
        assert!(!paths.contains(&&b"/.SynoUpgrade.tar"[..]), "{paths:?}");
        assert!(paths.contains(&&b"/upd@te/sub/hda1.tgz"[..]), "{paths:?}");
    }

    #[test]
    fn mount_points_are_listed_and_not_entered() {
        let root = fake_root("rootfs-walk-mounts");
        let skip = [b"/usr/syno".to_vec()].into_iter().collect();
        let walked = walk(&root, &skip, Limits::default(), &mut Unpaced(0)).unwrap();
        assert_eq!(
            walked.not_walked,
            [NotWalked {
                path: b"/usr/syno".to_vec(),
                reason: NotWalkedReason::MountPoint
            }]
        );
        assert_eq!(walked.rows[&b"/usr".to_vec()], allocated(&root.join("usr")));
        assert!(!walked.rows.contains_key(b"/usr/syno".as_slice()));
    }

    #[test]
    fn the_walk_is_bounded_and_yields_between_batches() {
        let root = fake_root("rootfs-walk-limits");
        let limits = Limits {
            max_entries: 3,
            ..Limits::default()
        };
        let walked = walk(&root, &Default::default(), limits, &mut Unpaced(0)).unwrap();
        assert_eq!(walked.entries, 3);
        assert_eq!(walked.stopped, Some(WalkStop::EntryLimit));

        for i in 0..600 {
            std::fs::write(root.join(format!("root/f{i}")), b"").unwrap();
        }
        let mut boundary = Unpaced(0);
        walk(&root, &Default::default(), Limits::default(), &mut boundary).unwrap();
        assert_eq!(boundary.0, 2);

        struct Stop;
        impl BatchBoundary for Stop {
            fn after_batch(&mut self) -> ControlFlow<StopReason> {
                ControlFlow::Break(StopReason::Paused)
            }
        }
        let stopped = walk(&root, &Default::default(), Limits::default(), &mut Stop).unwrap();
        assert_eq!(stopped.entries, BATCH_ENTRIES);
        assert_eq!(stopped.stopped, Some(WalkStop::Stopped(StopReason::Paused)));
    }

    #[test]
    fn unreadable_directories_are_counted_and_listed() {
        use std::os::unix::fs::PermissionsExt;
        let root = fake_root("rootfs-walk-unreadable");
        let locked = root.join("root/locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let walked = run(&root);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if crate::sys::effective_uid() != 0 {
            assert_eq!(walked.unreadable, 1);
            assert_eq!(walked.not_walked[0].reason, NotWalkedReason::Unreadable);
        }
    }

    #[test]
    fn bytes_are_totalled_per_owner() {
        let root = fake_root("rootfs-walk-owners");
        for dir in [
            "var/packages/SynoFinder",
            "usr/local/packages/@appstore/Python2",
            "usr/local/bin",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("var/packages/SynoFinder/db"), vec![1u8; 8192]).unwrap();
        std::fs::write(root.join("usr/local/bin/gs"), vec![1u8; 4096]).unwrap();
        let walked = run(&root);
        let owner = |category, name: Option<&str>| {
            walked.owners[&Owner {
                category,
                name: name.map(|n| n.as_bytes().to_vec()),
            }]
        };
        assert_eq!(
            owner(Category::PackageData, Some("SynoFinder")),
            below(
                &root,
                &["var/packages/SynoFinder", "var/packages/SynoFinder/db"]
            )
        );
        assert_eq!(
            owner(Category::PackageData, Some("Python2")),
            below(&root, &["usr/local/packages/@appstore/Python2"])
        );
        assert_eq!(
            owner(Category::UserTooling, Some("/usr/local")),
            below(&root, &["usr/local", "usr/local/bin", "usr/local/bin/gs"])
        );
        assert_eq!(
            owner(Category::UserTooling, Some("/root")),
            below(&root, &["root"])
        );
        assert_eq!(
            owner(Category::LogArchive, None),
            below(&root, &["var/log/disk-latency/a.xz"])
        );
        assert_eq!(
            walked.owners.values().sum::<u64>() + allocated(&root),
            walked.allocated_bytes
        );
    }
}
