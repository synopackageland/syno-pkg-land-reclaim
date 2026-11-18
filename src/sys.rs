//! The ioctl/FFI module: the only place `unsafe` is allowed (SPEC 18).
//! Every function here is a thin, checked wrapper around one syscall.

use crate::btrfs::FeatureFlags;
use crate::btrfs::keys::FIRST_FREE_OBJECTID;
use crate::btrfs::search::SearchRange;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const BTRFS_IOCTL_MAGIC: u32 = 0x94;

const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    ioc_of(dir, BTRFS_IOCTL_MAGIC, nr, size)
}

const fn ioc_of(dir: u32, kind: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | (kind << 8) | nr
}

/// `FS_IOC_GETFLAGS`: `_IOR('f', 1, long)`, though the kernel writes an int.
const FS_IOC_GETFLAGS: u32 = ioc_of(IOC_READ, b'f' as u32, 1, size_of::<libc::c_long>());
/// `FS_APPEND_FL`: writes may only append; the file cannot be unlinked.
pub const FS_APPEND_FL: u32 = 0x20;
/// `FS_IMMUTABLE_FL`: the file cannot be changed, renamed or unlinked.
pub const FS_IMMUTABLE_FL: u32 = 0x10;

pub const BTRFS_IOC_TREE_SEARCH_V2: u32 = ioc(IOC_READ | IOC_WRITE, 17, size_of::<SearchArgsV2>());
pub const BTRFS_IOC_INO_LOOKUP: u32 = ioc(IOC_READ | IOC_WRITE, 18, size_of::<InoLookupArgs>());
pub const BTRFS_IOC_FS_INFO: u32 = ioc(IOC_READ, 31, size_of::<FsInfoArgs>());
pub const BTRFS_IOC_SPACE_INFO: u32 = ioc(IOC_READ | IOC_WRITE, 20, size_of::<SpaceArgs>());
pub const BTRFS_IOC_GET_FEATURES: u32 = ioc(IOC_READ, 57, size_of::<FeatureFlagsArgs>());
pub const BTRFS_IOC_BALANCE_PROGRESS: u32 = ioc(IOC_READ, 34, size_of::<BalanceArgs>());
pub const BTRFS_IOC_DEV_REPLACE: u32 = ioc(IOC_READ | IOC_WRITE, 53, size_of::<DevReplaceArgs>());
pub const BTRFS_IOC_DEV_INFO: u32 = ioc(IOC_READ | IOC_WRITE, 30, size_of::<DevInfoArgs>());
const FS_IOC_GETVERSION: u32 =
    (IOC_READ << 30) | ((size_of::<libc::c_long>() as u32) << 16) | ((b'v' as u32) << 8) | 1;

const FS_IOC_FSGETXATTR: u32 =
    (IOC_READ << 30) | ((size_of::<FsXattr>() as u32) << 16) | ((b'X' as u32) << 8) | 31;
/// `QCMD(Q_GETQUOTA, PRJQUOTA)` of `<linux/quota.h>`.
const Q_GETQUOTA_PROJECT: u32 = (0x80_0007 << 8) | 2;
/// `QIF_DQBLKSIZE`: quota limits count 1 KiB blocks.
const QUOTA_BLOCK: u64 = 1024;

const BALANCE_STATE_RUNNING: u64 = 1;
#[cfg(test)]
const BALANCE_STATE_PAUSE_REQ: u64 = 2;
const DEV_REPLACE_CMD_STATUS: u64 = 1;
const DEV_REPLACE_STATE_STARTED: u64 = 1;

const IOPRIO_WHO_PROCESS: libc::c_int = 1;
const IOPRIO_CLASS_SHIFT: libc::c_int = 13;

/// `struct btrfs_ioctl_search_key`.
#[repr(C)]
#[derive(Default)]
struct SearchKey {
    tree_id: u64,
    min_objectid: u64,
    max_objectid: u64,
    min_offset: u64,
    max_offset: u64,
    min_transid: u64,
    max_transid: u64,
    min_type: u32,
    max_type: u32,
    nr_items: u32,
    unused: u32,
    unused1: u64,
    unused2: u64,
    unused3: u64,
    unused4: u64,
}

/// `struct btrfs_ioctl_search_args_v2` without its flexible `buf[]`.
#[repr(C)]
struct SearchArgsV2 {
    key: SearchKey,
    buf_size: u64,
}

/// `struct btrfs_ioctl_space_args` before its flexible `spaces[]` array.
#[repr(C)]
struct SpaceArgs {
    space_slots: u64,
    total_spaces: u64,
}

/// One `struct btrfs_ioctl_space_info` returned by SPACE_INFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceInfo {
    pub flags: u64,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// `struct btrfs_ioctl_ino_lookup_args`.
#[repr(C)]
struct InoLookupArgs {
    treeid: u64,
    objectid: u64,
    name: [u8; 4080],
}

/// `struct btrfs_ioctl_fs_info_args`.
#[repr(C)]
struct FsInfoArgs {
    max_id: u64,
    num_devices: u64,
    fsid: [u8; 16],
    nodesize: u32,
    sectorsize: u32,
    clone_alignment: u32,
    reserved32: u32,
    reserved: [u64; 122],
}

/// `struct btrfs_ioctl_dev_info_args`.
#[repr(C)]
struct DevInfoArgs {
    devid: u64,
    uuid: [u8; 16],
    bytes_used: u64,
    total_bytes: u64,
    unused: [u64; 379],
    path: [u8; 1024],
}

/// `struct btrfs_ioctl_feature_flags`.
#[repr(C)]
#[derive(Default)]
struct FeatureFlagsArgs {
    compat_flags: u64,
    compat_ro_flags: u64,
    incompat_flags: u64,
}

/// `struct btrfs_ioctl_balance_args`; only `state` is read.
#[repr(C)]
struct BalanceArgs {
    flags: u64,
    state: u64,
    /// Data, system and metadata filters, progress, and reserved words.
    rest: [u64; 126],
}

/// `struct btrfs_ioctl_dev_replace_args` with the status member of its
/// start/status union.
#[repr(C)]
struct DevReplaceArgs {
    cmd: u64,
    result: u64,
    replace_state: u64,
    /// The rest of the status arguments.
    status: [u64; 5],
    /// Where the larger start arguments extend the union.
    start: [u8; 2024],
    spare: [u64; 64],
}

const ARGS_WORDS: usize = size_of::<SearchArgsV2>() / size_of::<u64>();

/// A reusable, 8-byte-aligned TREE_SEARCH_V2 argument block with room for
/// `capacity` result bytes after the fixed header.
pub struct SearchBuffer {
    words: Vec<u64>,
    capacity: usize,
}

impl SearchBuffer {
    pub fn new(capacity: usize) -> Self {
        let words = ARGS_WORDS + capacity.div_ceil(size_of::<u64>());
        SearchBuffer {
            words: vec![0; words],
            capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The result area after the header.
    pub fn results(&self) -> &[u8] {
        let bytes = self.words[ARGS_WORDS..].as_ptr().cast::<u8>();
        // SAFETY: the words after the header hold at least `capacity`
        // initialized bytes, and u8 has no alignment requirement.
        unsafe { std::slice::from_raw_parts(bytes, self.capacity) }
    }
}

/// Result of one TREE_SEARCH_V2 call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOutcome {
    /// This many items were written to the result area.
    Items(u32),
    /// The next item alone needs a result area of this many bytes.
    BufferTooSmall { needed: u64 },
}

pub fn tree_search_v2(
    fd: BorrowedFd<'_>,
    range: &SearchRange,
    buf: &mut SearchBuffer,
) -> io::Result<SearchOutcome> {
    let args = SearchArgsV2 {
        key: SearchKey {
            tree_id: range.tree_id,
            min_objectid: range.min.objectid,
            max_objectid: range.max.objectid,
            min_offset: range.min.offset,
            max_offset: range.max.offset,
            min_transid: range.min_transid,
            max_transid: u64::MAX,
            min_type: u32::from(range.min.item_type),
            max_type: u32::from(range.max.item_type),
            nr_items: u32::MAX,
            ..SearchKey::default()
        },
        buf_size: buf.capacity as u64,
    };
    let header = buf.words.as_mut_ptr().cast::<SearchArgsV2>();
    // SAFETY: `words` is 8-byte aligned and starts with ARGS_WORDS words,
    // exactly the size of SearchArgsV2.
    unsafe { header.write(args) };
    // SAFETY: the kernel writes at most `buf_size` bytes after the header,
    // and the allocation holds header + capacity bytes.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_TREE_SEARCH_V2 as _, header) };
    // SAFETY: header is still valid and aligned; the kernel updated it.
    let returned = unsafe { header.read() };
    if rc == 0 {
        return Ok(SearchOutcome::Items(returned.key.nr_items));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EOVERFLOW) {
        return Ok(SearchOutcome::BufferTooSmall {
            needed: returned.buf_size,
        });
    }
    Err(err)
}

/// Filesystem-wide identity from BTRFS_IOC_FS_INFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsInfo {
    /// Largest device id.
    pub max_id: u64,
    pub fsid: [u8; 16],
    pub num_devices: u64,
    pub nodesize: u32,
    pub sectorsize: u32,
}

pub fn fs_info(fd: BorrowedFd<'_>) -> io::Result<FsInfo> {
    // SAFETY: FsInfoArgs is plain old data; all-zero is a valid value.
    let mut args: FsInfoArgs = unsafe { std::mem::zeroed() };
    // SAFETY: args is a valid, writable FsInfoArgs of the size encoded in the request.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_FS_INFO as _, &mut args) })?;
    Ok(FsInfo {
        max_id: args.max_id,
        fsid: args.fsid,
        num_devices: args.num_devices,
        nodesize: args.nodesize,
        sectorsize: args.sectorsize,
    })
}

/// The data checksum size the kernel reports for the filesystem holding
/// `fd` (`BTRFS_FS_INFO_FLAG_CSUM_INFO`, Linux 5.5+). `None` when the
/// kernel predates it; such kernels only support CRC-32C (4 bytes).
pub fn fs_csum_size(fd: BorrowedFd<'_>) -> io::Result<Option<u16>> {
    const CSUM_INFO: u64 = 1;
    // SAFETY: FsInfoArgs is plain old data; all-zero is a valid value.
    let mut args: FsInfoArgs = unsafe { std::mem::zeroed() };
    // `flags` follows the checksum type and size, which take `reserved32`.
    args.reserved[0] = CSUM_INFO;
    // SAFETY: args is a valid, writable FsInfoArgs of the size encoded in the request.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_FS_INFO as _, &mut args) })?;
    Ok((args.reserved[0] & CSUM_INFO != 0).then_some((args.reserved32 >> 16) as u16))
}

/// The path of device `devid` of the filesystem holding the open file;
/// `None` when the filesystem has no such device.
pub fn device_path(fd: BorrowedFd<'_>, devid: u64) -> io::Result<Option<std::path::PathBuf>> {
    // SAFETY: DevInfoArgs is plain old data; all-zero is a valid value.
    let mut args: DevInfoArgs = unsafe { std::mem::zeroed() };
    args.devid = devid;
    // SAFETY: args is a valid, writable DevInfoArgs of the size encoded in the request.
    match check(unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_DEV_INFO as _, &mut args) }) {
        Err(e) if e.raw_os_error() == Some(libc::ENODEV) => return Ok(None),
        other => other?,
    };
    let len = args
        .path
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(args.path.len());
    Ok(Some(OsStr::from_bytes(&args.path[..len]).into()))
}

/// Superblock feature flags of the filesystem holding the open file.
pub fn feature_flags(fd: BorrowedFd<'_>) -> io::Result<FeatureFlags> {
    let mut args = FeatureFlagsArgs::default();
    // SAFETY: args is a valid, writable FeatureFlagsArgs of the size encoded in the request.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_GET_FEATURES as _, &mut args) })?;
    Ok(FeatureFlags {
        compat: args.compat_flags,
        compat_ro: args.compat_ro_flags,
        incompat: args.incompat_flags,
    })
}

/// `INODE_ITEM.generation` as exposed by `FS_IOC_GETVERSION`.
pub fn inode_generation(fd: BorrowedFd<'_>) -> io::Result<u64> {
    let mut generation: libc::c_long = 0;
    // SAFETY: generation is a writable long matching FS_IOC_GETVERSION.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_GETVERSION as _, &mut generation) })?;
    Ok(u64::from(generation as u32))
}

/// Opens one name relative to a directory descriptor without changing its
/// interpretation through the process working directory.
pub fn openat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    let name = c_name(name)?;
    // SAFETY: name is NUL-terminated and dirfd is a live directory descriptor.
    let fd = unsafe {
        libc::openat(
            dirfd.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_LARGEFILE,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor owned by this File.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Opens an inspection descriptor without updating atime where permitted.
/// If the caller does not own the entry, retry without only `O_NOATIME`.
pub fn openat_noatime(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    open_with_noatime(flags, |flags| openat(dirfd, name, flags, mode))
}

fn open_with_noatime(
    flags: libc::c_int,
    mut open: impl FnMut(libc::c_int) -> io::Result<File>,
) -> io::Result<File> {
    let fallback_flags = flags & !libc::O_NOATIME;
    match open(fallback_flags | libc::O_NOATIME) {
        Err(error) if error.raw_os_error() == Some(libc::EPERM) => open(fallback_flags),
        result => result,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub links: u64,
    pub size: u64,
    /// `st_blocks`: 512-byte units allocated.
    pub blocks: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    /// `st_ctime`: when the inode last changed (content, links, mode).
    pub ctime: i64,
    pub ctime_nsec: i64,
}

/// The inode flags of `fd` (`chattr`'s), from `FS_IOC_GETFLAGS`.
pub fn inode_flags(fd: BorrowedFd<'_>) -> io::Result<u32> {
    let mut flags: libc::c_int = 0;
    check(unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_GETFLAGS as _, &mut flags) })?;
    Ok(flags as u32)
}

/// Metadata of a directory entry without following a symlink.
pub fn statat(dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<FileStat> {
    let name = c_name(name)?;
    // SAFETY: stat is plain old data; all-zero is a valid writable value.
    let mut stat: libc::stat64 = unsafe { std::mem::zeroed() };
    // SAFETY: name is NUL-terminated and stat is valid; AT_SYMLINK_NOFOLLOW
    // reports the directory entry itself if it is a symbolic link.
    check(unsafe {
        libc::fstatat64(
            dirfd.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(FileStat {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
        mode: stat.st_mode,
        links: stat.st_nlink as u64,
        size: stat.st_size as u64,
        blocks: stat.st_blocks as u64,
        mtime: stat.st_mtime as i64,
        mtime_nsec: stat.st_mtime_nsec as i64,
        ctime: stat.st_ctime as i64,
        ctime_nsec: stat.st_ctime_nsec as i64,
    })
}

/// The offset of the first data at or after `offset` in the open file
/// (lseek SEEK_DATA); `None` when only a hole or the end follows.
pub fn seek_data(fd: BorrowedFd<'_>, offset: u64) -> io::Result<Option<u64>> {
    match lseek(fd, offset, libc::SEEK_DATA) {
        Err(e) if e.raw_os_error() == Some(libc::ENXIO) => Ok(None),
        result => result.map(Some),
    }
}

/// The offset of the first hole at or after `offset` in the open file
/// (lseek SEEK_HOLE); the end of the file counts as a hole.
pub fn seek_hole(fd: BorrowedFd<'_>, offset: u64) -> io::Result<u64> {
    lseek(fd, offset, libc::SEEK_HOLE)
}

fn lseek(fd: BorrowedFd<'_>, offset: u64, whence: libc::c_int) -> io::Result<u64> {
    let offset = libc::off_t::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset out of range"))?;
    // SAFETY: fd is a live descriptor; lseek only moves its offset.
    let rc = unsafe { libc::lseek(fd.as_raw_fd(), offset, whence) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc as u64)
}

/// Removes one name relative to a directory descriptor. Callers validate
/// its identity immediately before this operation.
pub fn unlinkat(dirfd: BorrowedFd<'_>, name: &OsStr, directory: bool) -> io::Result<()> {
    let name = c_name(name)?;
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: name is NUL-terminated and dirfd is a live directory descriptor.
    check(unsafe { libc::unlinkat(dirfd.as_raw_fd(), name.as_ptr(), flags) })
}

/// Creates one directory relative to a directory descriptor; the umask
/// can only narrow `mode`.
pub fn mkdirat(dirfd: BorrowedFd<'_>, name: &OsStr, mode: libc::mode_t) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: name is NUL-terminated and dirfd is a live directory descriptor.
    check(unsafe { libc::mkdirat(dirfd.as_raw_fd(), name.as_ptr(), mode) })
}

/// Sets the mode of an open file. On DSM this also turns an ACL-mode file
/// into a Linux-mode one, dropping the ACL it inherited.
pub fn set_mode(fd: BorrowedFd<'_>, mode: libc::mode_t) -> io::Result<()> {
    // SAFETY: fd is a live descriptor.
    check(unsafe { libc::fchmod(fd.as_raw_fd(), mode) })
}

/// Whether the open file has extended attribute `name`. A missing value
/// (ENODATA) and a namespace the filesystem does not handle (ENOTSUP) both
/// mean absent.
pub fn has_xattr(fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<bool> {
    let name = c_name(name)?;
    // SAFETY: name is NUL-terminated; a zero size asks only for the length
    // and writes nothing through the null value pointer.
    let rc = unsafe { libc::fgetxattr(fd.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if rc >= 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENODATA | libc::ENOTSUP) => Ok(false),
        _ => Err(error),
    }
}

/// Moves one name between directory descriptors; fails with EEXIST rather
/// than replacing an existing destination (renameat2 RENAME_NOREPLACE).
pub fn rename_noreplace(
    from_dir: BorrowedFd<'_>,
    from: &OsStr,
    to_dir: BorrowedFd<'_>,
    to: &OsStr,
) -> io::Result<()> {
    let from = c_name(from)?;
    let to = c_name(to)?;
    // The raw syscall: musl has no renameat2 wrapper.
    // SAFETY: both names are NUL-terminated and both descriptors are live;
    // renameat2 takes (int, const char *, int, const char *, unsigned int).
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from_dir.as_raw_fd(),
            from.as_ptr(),
            to_dir.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Gives the file `from` another name `to` (linkat, not following a
/// symlink); fails with EEXIST rather than replacing an existing `to`, and
/// with EXDEV across filesystems.
pub fn link_at(
    from_dir: BorrowedFd<'_>,
    from: &OsStr,
    to_dir: BorrowedFd<'_>,
    to: &OsStr,
) -> io::Result<()> {
    let from = c_name(from)?;
    let to = c_name(to)?;
    // SAFETY: both names are NUL-terminated and both descriptors are live.
    check(unsafe {
        libc::linkat(
            from_dir.as_raw_fd(),
            from.as_ptr(),
            to_dir.as_raw_fd(),
            to.as_ptr(),
            0,
        )
    })
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

/// Stream entries from a duplicated directory descriptor.
pub struct DirectoryEntries(*mut libc::DIR);

impl DirectoryEntries {
    pub fn open(fd: BorrowedFd<'_>) -> io::Result<DirectoryEntries> {
        // SAFETY: fcntl duplicates the live descriptor and returns an owned fd.
        let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fdopendir takes ownership of the duplicate on success.
        let dir = unsafe { libc::fdopendir(duplicate) };
        if dir.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir did not take ownership on failure.
            unsafe { libc::close(duplicate) };
            return Err(error);
        }
        Ok(DirectoryEntries(dir))
    }
}

impl Iterator for DirectoryEntries {
    type Item = io::Result<OsString>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // readdir reports end-of-directory and errors with a null pointer.
            // Clear errno to distinguish the two cases.
            // SAFETY: this thread's errno slot is writable.
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: self.0 remains a valid DIR until Drop.
            // The 64-bit interface also accepts large inode numbers on armv7.
            let entry = unsafe { libc::readdir64(self.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return (error.raw_os_error() != Some(0)).then_some(Err(error));
            }
            // SAFETY: readdir returns a dirent whose d_name is NUL-terminated.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            return Some(Ok(OsString::from_vec(name.to_bytes().to_vec())));
        }
    }
}

impl Drop for DirectoryEntries {
    fn drop(&mut self) {
        // SAFETY: this value uniquely owns the DIR returned by fdopendir.
        unsafe { libc::closedir(self.0) };
    }
}

/// Reserves a private execution log before cleanup starts.
pub fn reserve_file_space(fd: BorrowedFd<'_>, bytes: u64) -> io::Result<()> {
    let length = libc::off_t::try_from(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "reservation is too large"))?;
    // SAFETY: fallocate reserves blocks for the live fd; KEEP_SIZE preserves
    // the append position and does not expose zero-filled log records.
    let result = unsafe { libc::fallocate(fd.as_raw_fd(), 1, 0, length) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Requires both read-only kernel progress queries to succeed; unknown status
/// fails closed so cleanup does not run with an ambiguous maintenance state.
pub fn filesystem_maintenance_active(fd: BorrowedFd<'_>) -> io::Result<bool> {
    Ok(balance_running(fd)? || dev_replace_running(fd)?)
}

/// Whether a balance is relocating block groups on the filesystem of
/// the open file (read-only BTRFS_IOC_BALANCE_PROGRESS).
pub fn balance_running(fd: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: BalanceArgs is plain old data; all-zero is a valid value.
    let mut args: BalanceArgs = unsafe { std::mem::zeroed() };
    // SAFETY: args is a valid, writable BalanceArgs of the size encoded in the request.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_BALANCE_PROGRESS as _, &mut args) };
    balance_running_from(check(rc), args.state)
}

/// The kernel answers ENOTCONN when no balance exists; a paused one
/// relocates nothing.
fn balance_running_from(rc: io::Result<()>, state: u64) -> io::Result<bool> {
    match rc {
        Ok(()) => Ok(state & BALANCE_STATE_RUNNING != 0),
        Err(e) if e.raw_os_error() == Some(libc::ENOTCONN) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether a device replace is copying on the filesystem of the open
/// file (BTRFS_IOC_DEV_REPLACE with the read-only status command).
pub fn dev_replace_running(fd: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: DevReplaceArgs is plain old data; all-zero is a valid value.
    let mut args: DevReplaceArgs = unsafe { std::mem::zeroed() };
    args.cmd = DEV_REPLACE_CMD_STATUS;
    // SAFETY: args is a valid, writable DevReplaceArgs of the size encoded in the request.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_DEV_REPLACE as _, &mut args) })?;
    Ok(dev_replace_running_from(args.replace_state))
}

/// A suspended, finished or cancelled replace copies nothing.
fn dev_replace_running_from(replace_state: u64) -> bool {
    replace_state == DEV_REPLACE_STATE_STARTED
}

/// The subvolume (tree) ID containing the open file or directory.
pub fn subvolume_id(fd: BorrowedFd<'_>) -> io::Result<u64> {
    let mut args = InoLookupArgs {
        treeid: 0,
        objectid: FIRST_FREE_OBJECTID,
        name: [0; 4080],
    };
    // SAFETY: args is a valid, writable InoLookupArgs of the size encoded in the request.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_INO_LOOKUP as _, &mut args) })?;
    Ok(args.treeid)
}

/// Writes out everything cached for the filesystem holding `fd`
/// (`syncfs`). It reports no writeback error of a file; a later
/// `fdatasync` of that file does.
pub fn syncfs(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: syncfs takes a plain descriptor.
    check(unsafe { libc::syncfs(fd.as_raw_fd()) })
}

/// `statfs.f_type` of the filesystem holding the open file.
pub fn filesystem_type(fd: BorrowedFd<'_>) -> io::Result<u64> {
    // SAFETY: statfs is plain old data; all-zero is a valid value.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: st is a valid, writable statfs.
    check(unsafe { libc::fstatfs(fd.as_raw_fd(), &mut st) })?;
    Ok(st.f_type as u64 & 0xFFFF_FFFF)
}

/// Sets the file mode creation mask.
pub fn set_umask(mask: libc::mode_t) {
    // SAFETY: umask has no preconditions and cannot fail.
    unsafe { libc::umask(mask) };
}

/// A close-on-exec pipe: its read end, then its write end.
pub fn pipe() -> io::Result<(File, File)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two descriptors into the two-element array.
    check(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) })?;
    // SAFETY: both descriptors are new and owned by nothing else.
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

/// Points standard output at `fd`.
pub fn replace_stdout(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: dup2 on two descriptor numbers; 1 is replaced atomically.
    check(unsafe { libc::dup2(fd.as_raw_fd(), libc::STDOUT_FILENO) })
}

/// Takes ownership of descriptor `fd` a parent left open for this process,
/// closing it on exec; an error when no such descriptor is open.
pub fn inherited(fd: i32) -> io::Result<File> {
    if fd <= libc::STDERR_FILENO {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    // SAFETY: fcntl on a descriptor number only reads or sets its flags.
    check(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) })?;
    // SAFETY: fd is open (fcntl succeeded) and, inherited, owned by no one
    // else in this process; callers take each number once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Current nice value of the calling process.
pub fn nice_value() -> io::Result<i32> {
    // getpriority can legitimately return -1, so errno must be cleared first.
    // SAFETY: __errno_location returns this thread's errno slot.
    unsafe { *libc::__errno_location() = 0 };
    // SAFETY: getpriority has no memory-safety preconditions.
    let value = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(0) | None => Ok(value),
        Some(_) => Err(err),
    }
}

pub fn set_nice_value(value: i32) -> io::Result<()> {
    // SAFETY: setpriority has no memory-safety preconditions.
    check(unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, value) })
}

/// I/O priority as `(class, data)` for the calling thread.
pub fn io_priority() -> io::Result<(i32, i32)> {
    // SAFETY: ioprio_get takes two integers and has no memory preconditions.
    let raw = unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let raw = raw as i32;
    Ok((
        raw >> IOPRIO_CLASS_SHIFT,
        raw & ((1 << IOPRIO_CLASS_SHIFT) - 1),
    ))
}

pub fn set_io_priority(class: i32, data: i32) -> io::Result<()> {
    let value = (class << IOPRIO_CLASS_SHIFT) | data;
    // SAFETY: ioprio_set takes three integers and has no memory preconditions.
    let rc = unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, value) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn stdin_is_terminal() -> bool {
    // SAFETY: isatty only observes the process's standard input descriptor.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

pub fn stderr_is_terminal() -> bool {
    // SAFETY: isatty only observes the process's standard error descriptor.
    unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
}

/// Forwards a caught stop request to the packaged executor child.
pub fn signal_process(pid: u32, signal: i32) -> io::Result<()> {
    // SAFETY: kill only sends the requested signal to this child pid.
    check(unsafe { libc::kill(pid as libc::pid_t, signal) })
}

/// Sends `signal` to every process in the group `pgid`.
pub fn signal_group(pgid: u32, signal: i32) -> io::Result<()> {
    // SAFETY: killpg only sends the requested signal to that process group.
    check(unsafe { libc::killpg(pgid as libc::pid_t, signal) })
}

/// Asks for SIGTERM, a stop request, when the parent process dies, and
/// tells whether `parent` is still the parent: a parent that died before
/// the request was made sends nothing.
pub fn stop_with_parent(parent: u32) -> io::Result<bool> {
    // SAFETY: PR_SET_PDEATHSIG takes a signal number and no pointers.
    check(unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) })?;
    // SAFETY: getppid has no preconditions.
    Ok(unsafe { libc::getppid() } as u32 == parent)
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// An exclusive `flock` on a file, released when dropped. Closing the
/// file alone would not release it while a child that another thread
/// forked still holds a copy of the descriptor before its exec.
#[derive(Debug)]
pub struct ExclusiveLock(File);

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        // SAFETY: flock takes a plain descriptor and flags. A failed
        // unlock leaves the release to the close that follows.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl ExclusiveLock {
    /// Serializes a short admission transaction; this never waits for a scan.
    pub fn take(file: File) -> io::Result<ExclusiveLock> {
        loop {
            // SAFETY: flock takes a live descriptor and plain flags.
            match check(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }) {
                Ok(()) => return Ok(ExclusiveLock(file)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Takes the lock without waiting; gives `file` back if another open
    /// file description holds it.
    pub fn try_take(file: File) -> io::Result<Result<ExclusiveLock, File>> {
        // SAFETY: flock takes a plain descriptor and flags.
        match check(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }) {
            Ok(()) => Ok(Ok(ExclusiveLock(file))),
            Err(e) if e.raw_os_error() == Some(libc::EWOULDBLOCK) => Ok(Err(file)),
            Err(e) => Err(e),
        }
    }

    pub fn file(&self) -> &File {
        &self.0
    }
}

/// A shared ancestor or exclusive subtree lock, always non-blocking.
#[derive(Debug)]
pub struct PathLock(File);
impl PathLock {
    pub fn try_take(file: File, exclusive: bool) -> io::Result<Result<Self, File>> {
        let mode = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        // SAFETY: flock receives a live descriptor and plain flags.
        match check(unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) }) {
            Ok(()) => Ok(Ok(Self(file))),
            Err(e) if e.raw_os_error() == Some(libc::EWOULDBLOCK) => Ok(Err(file)),
            Err(e) => Err(e),
        }
    }
    pub fn file(&self) -> &File {
        &self.0
    }
}
impl Drop for PathLock {
    fn drop(&mut self) {
        // SAFETY: flock receives a live descriptor and plain flags.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Space of the filesystem holding the open file, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    pub free: u64,
    /// Available to unprivileged users.
    pub available: u64,
}

/// Inode counts of the filesystem holding the open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inodes {
    pub total: u64,
    pub free: u64,
}

pub fn filesystem_inodes(fd: BorrowedFd<'_>) -> io::Result<Inodes> {
    // SAFETY: statvfs is plain old data; all-zero is a valid value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: st is a valid, writable statvfs.
    check(unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut st) })?;
    Ok(Inodes {
        total: st.f_files as u64,
        free: st.f_ffree as u64,
    })
}

/// `struct fsxattr` of `<linux/fs.h>`.
#[repr(C)]
#[derive(Default)]
struct FsXattr {
    xflags: u32,
    extsize: u32,
    nextents: u32,
    projid: u32,
    cowextsize: u32,
    pad: [u8; 8],
}

/// The project id of the open file (ext4 `prjquota`, XFS).
pub fn project_id(fd: BorrowedFd<'_>) -> io::Result<u32> {
    let mut attr = FsXattr::default();
    // SAFETY: attr is a writable struct fsxattr matching FS_IOC_FSGETXATTR.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_FSGETXATTR as _, &mut attr) })?;
    Ok(attr.projid)
}

/// `struct if_dqblk` of `<linux/quota.h>`.
#[repr(C)]
#[derive(Default)]
struct IfDqblk {
    bhardlimit: u64,
    bsoftlimit: u64,
    curspace: u64,
    ihardlimit: u64,
    isoftlimit: u64,
    curinodes: u64,
    btime: u64,
    itime: u64,
    valid: u32,
}

/// A project's space quota, in bytes; a limit of 0 means none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectQuota {
    pub used: u64,
    pub soft_limit: u64,
    pub hard_limit: u64,
}

/// The space quota of project `id` on the filesystem of block `device`.
pub fn project_quota(device: &std::path::Path, id: u32) -> io::Result<ProjectQuota> {
    let device = c_name(device.as_os_str())?;
    let mut quota = IfDqblk::default();
    // The raw syscall: musl's quotactl wrapper is not in the libc crate.
    // SAFETY: device is NUL-terminated and quota is a writable if_dqblk,
    // as Q_GETQUOTA expects.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_quotactl,
            Q_GETQUOTA_PROJECT as libc::c_int,
            device.as_ptr(),
            id as libc::c_int,
            &mut quota as *mut IfDqblk,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ProjectQuota {
        used: quota.curspace,
        soft_limit: quota.bsoftlimit.saturating_mul(QUOTA_BLOCK),
        hard_limit: quota.bhardlimit.saturating_mul(QUOTA_BLOCK),
    })
}

pub fn filesystem_space(fd: BorrowedFd<'_>) -> io::Result<Space> {
    // SAFETY: statvfs is plain old data; all-zero is a valid value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: st is a valid, writable statvfs.
    check(unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut st) })?;
    let unit = st.f_frsize as u64;
    Ok(Space {
        total: (st.f_blocks as u64).saturating_mul(unit),
        free: (st.f_bfree as u64).saturating_mul(unit),
        available: (st.f_bavail as u64).saturating_mul(unit),
    })
}

/// SPACE_INFO rows for the filesystem holding `fd`, with room grown when
/// the kernel reports more space classes than the first allocation.
pub fn btrfs_space_info(fd: BorrowedFd<'_>) -> io::Result<Vec<SpaceInfo>> {
    let mut slots = 16usize;
    loop {
        let words_len = 2usize
            .checked_add(slots.checked_mul(3).ok_or_else(invalid_space_count)?)
            .ok_or_else(invalid_space_count)?;
        let mut words = vec![0u64; words_len];
        words[0] = u64::try_from(slots).map_err(|_| invalid_space_count())?;
        // SAFETY: the allocation begins with SpaceArgs and contains three
        // u64 words for every space slot after the header.
        let args = words.as_mut_ptr().cast::<SpaceArgs>();
        // SAFETY: args points into the live words allocation above.
        let rc = unsafe { libc::ioctl(fd.as_raw_fd(), BTRFS_IOC_SPACE_INFO as _, args) };
        if rc < 0 {
            let error = io::Error::last_os_error();
            let reported = words.get(1).copied().unwrap_or(0);
            if error.raw_os_error() == Some(libc::ENOSPC)
                && usize::try_from(reported).is_ok_and(|count| count > slots && count <= 4096)
            {
                slots = reported as usize;
                continue;
            }
            return Err(error);
        }
        let count = usize::try_from(words[1]).map_err(|_| invalid_space_count())?;
        if count > slots {
            if count > 4096 {
                return Err(invalid_space_count());
            }
            slots = count;
            continue;
        }
        return space_info_rows(&words);
    }
}

fn invalid_space_count() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid Btrfs SPACE_INFO slot count",
    )
}

fn space_info_rows(words: &[u64]) -> io::Result<Vec<SpaceInfo>> {
    let slots = usize::try_from(*words.first().ok_or_else(invalid_space_count)?)
        .map_err(|_| invalid_space_count())?;
    let count = usize::try_from(*words.get(1).ok_or_else(invalid_space_count)?)
        .map_err(|_| invalid_space_count())?;
    let needed = 2usize
        .checked_add(count.checked_mul(3).ok_or_else(invalid_space_count)?)
        .ok_or_else(invalid_space_count)?;
    if count > slots || words.len() < needed {
        return Err(invalid_space_count());
    }
    Ok(words[2..needed]
        .chunks_exact(3)
        .map(|row| SpaceInfo {
            flags: row[0],
            total_bytes: row[1],
            used_bytes: row[2],
        })
        .collect())
}

/// `statfs` space in bytes, for the overview's exact df calculation.
pub fn filesystem_space_statfs(fd: BorrowedFd<'_>) -> io::Result<Space> {
    // SAFETY: statfs is plain old data; all-zero is a valid value.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: st is a valid, writable statfs.
    check(unsafe { libc::fstatfs(fd.as_raw_fd(), &mut st) })?;
    let unit = st.f_bsize as u64;
    Ok(Space {
        total: (st.f_blocks as u64).saturating_mul(unit),
        free: (st.f_bfree as u64).saturating_mul(unit),
        available: (st.f_bavail as u64).saturating_mul(unit),
    })
}

static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STOP_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Whether a graceful stop was requested: a signal, or a job's cancel.
pub fn stop_requested() -> bool {
    STOP_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
}

extern "C" fn on_stop_signal(signal: libc::c_int) {
    // Only async-signal-safe atomic stores (SPEC 22.1).
    STOP_SIGNAL.store(signal, std::sync::atomic::Ordering::Relaxed);
    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Makes SIGINT, SIGTERM and SIGHUP request a graceful stop through the
/// returned flag. The handlers reset themselves, so a second signal
/// terminates the process immediately.
pub fn install_stop_handlers() -> io::Result<&'static std::sync::atomic::AtomicBool> {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: sigaction is plain old data; all-zero is a valid value.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESETHAND | libc::SA_RESTART;
        // SAFETY: action.sa_mask is a valid sigset_t to initialise.
        check(unsafe { libc::sigemptyset(&mut action.sa_mask) })?;
        // SAFETY: action is fully initialised; the old action is not needed.
        check(unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) })?;
    }
    Ok(&STOP_REQUESTED)
}

/// Starts `argv` in a new session, so it outlives its caller's terminal
/// and process group, with stdin from `/dev/null` and its output
/// appended to `log` (created 0600); does not wait for it.
pub fn spawn_session(argv: &[OsString], log: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(log)?;
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out);
    // SAFETY: the hook runs in the forked child before exec and only
    // calls setsid, which is async-signal-safe.
    unsafe {
        command.pre_exec(|| check(libc::setsid()));
    }
    command.spawn().map(drop)
}

/// The signal that requested a stop, if any.
pub fn stop_signal() -> Option<i32> {
    match STOP_SIGNAL.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        signal => Some(signal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn directory_metadata_preserves_large_sparse_file_sizes() {
        let dir = crate::spill::testing::scratch("statat-large-file");
        let file = std::fs::File::create(dir.join("sparse")).unwrap();
        file.set_len(64_u64 << 30).unwrap();
        let parent = std::fs::File::open(&dir).unwrap();
        let stat = statat(parent.as_fd(), OsStr::new("sparse")).unwrap();
        assert_eq!(stat.size, 64_u64 << 30);
        assert_eq!(stat.blocks, 0);
        let opened = openat(parent.as_fd(), OsStr::new("sparse"), libc::O_RDONLY, 0).unwrap();
        assert_eq!(opened.metadata().unwrap().len(), 64_u64 << 30);
        let names = DirectoryEntries::open(parent.as_fd())
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(names, [OsString::from("sparse")]);
    }

    #[test]
    fn statat_reports_the_inode_change_time() {
        use std::os::unix::fs::MetadataExt;
        let dir = crate::spill::testing::scratch("statat-ctime");
        std::fs::write(dir.join("file"), b"x").unwrap();
        let meta = std::fs::symlink_metadata(dir.join("file")).unwrap();

        let stat = statat(
            std::fs::File::open(&dir).unwrap().as_fd(),
            OsStr::new("file"),
        )
        .unwrap();

        assert_eq!(stat.ctime, meta.ctime());
    }

    #[test]
    fn data_and_holes_are_found_from_an_offset() {
        use std::os::unix::fs::FileExt;
        let path = crate::spill::testing::scratch("seek-data").join("sparse");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(4 << 20).unwrap();
        file.write_at(b"data", 1 << 20).unwrap();

        let data = seek_data(file.as_fd(), 0).unwrap().unwrap();
        assert!(data <= 1 << 20);
        let hole = seek_hole(file.as_fd(), data).unwrap();
        assert!(((1 << 20) + 4..=4 << 20).contains(&hole));
        assert_eq!(seek_data(file.as_fd(), 4 << 20).unwrap(), None);
    }

    #[test]
    fn a_dropped_lock_is_free_while_a_child_still_holds_its_descriptor() {
        let path = crate::spill::testing::scratch("flock").join("lock");
        let open = || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let lock = ExclusiveLock::try_take(open()).unwrap().unwrap();
        // A child forked by another thread keeps a copy of every
        // descriptor until it execs; this one keeps it for good.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(lock.file().try_clone().unwrap())
            .spawn()
            .unwrap();
        assert!(ExclusiveLock::try_take(open()).unwrap().is_err());
        drop(lock);
        let again = ExclusiveLock::try_take(open()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(again.is_ok(), "the lock outlived its guard");
    }

    #[test]
    fn an_extended_attribute_is_present_only_while_it_holds_a_value() {
        use std::os::fd::AsFd;
        let path = crate::spill::testing::scratch("xattr").join("file");
        std::fs::write(&path, b"").unwrap();
        let file = File::open(&path).unwrap();
        let name = OsStr::new("user.synoreclaim-test");
        assert!(!has_xattr(file.as_fd(), name).unwrap());
        let c_name = c_name(name).unwrap();
        // SAFETY: the name is NUL-terminated and the one-byte value is live.
        check(unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                c_name.as_ptr(),
                b"1".as_ptr().cast(),
                1,
                0,
            )
        })
        .unwrap();
        assert!(has_xattr(file.as_fd(), name).unwrap());
        // A namespace this filesystem does not know is absent, not an error.
        assert!(!has_xattr(file.as_fd(), OsStr::new("system.syno_acl_inherit")).unwrap());
    }

    #[test]
    fn a_mode_is_set_through_the_open_descriptor() {
        use std::os::fd::AsFd;
        use std::os::unix::fs::PermissionsExt;
        let directory = crate::spill::testing::scratch("fchmod").join("directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = File::open(&directory).unwrap();
        set_mode(file.as_fd(), 0o700).unwrap();
        assert_eq!(
            file.metadata().unwrap().permissions().mode() & 0o7777,
            0o700
        );
    }

    #[test]
    fn a_stop_is_requested_when_the_parent_dies_and_only_that_parent_counts() {
        // SAFETY: getppid has no preconditions.
        let parent = unsafe { libc::getppid() } as u32;
        assert!(!stop_with_parent(u32::MAX).unwrap());
        assert!(stop_with_parent(parent).unwrap());
        let mut signal: libc::c_int = 0;
        // SAFETY: PR_GET_PDEATHSIG writes one c_int to the valid pointer.
        check(unsafe { libc::prctl(libc::PR_GET_PDEATHSIG, &mut signal) }).unwrap();
        assert_eq!(signal, libc::SIGTERM);
    }

    #[test]
    fn a_spawned_session_leads_itself_and_writes_to_its_log() {
        let log = crate::spill::testing::scratch("session").join("out.log");
        let script = "read -r _ _ _ _ _ sid _ < /proc/$$/stat; echo \"$$ $sid\"";
        let argv = ["/bin/sh", "-c", script].map(OsString::from);
        spawn_session(&argv, &log).unwrap();
        let mut line = String::new();
        for _ in 0..100 {
            line = std::fs::read_to_string(&log).unwrap_or_default();
            if line.ends_with('\n') {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let ids: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(ids.len(), 2, "{line:?}");
        assert_eq!(ids[0], ids[1], "the child leads its own session");
        let mode = std::fs::metadata(&log).unwrap().permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
    }

    #[test]
    fn noatime_open_retries_without_only_that_flag_after_permission_denied() {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let mut seen = Vec::new();
        let file = open_with_noatime(flags, |attempt| {
            seen.push(attempt);
            if seen.len() == 1 {
                Err(io::Error::from_raw_os_error(libc::EPERM))
            } else {
                File::open("/dev/null")
            }
        })
        .unwrap();
        assert_eq!(seen, [flags | libc::O_NOATIME, flags]);
        drop(file);
    }

    #[test]
    fn a_private_directory_takes_a_name_only_where_none_exists() {
        let path = std::env::temp_dir().join(format!("synoreclaim-rename-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let directory = File::open(&path).unwrap();
        let fd = std::os::fd::AsFd::as_fd(&directory);
        mkdirat(fd, OsStr::new("private"), 0o700).unwrap();
        let mode = std::fs::metadata(path.join("private"))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o700
        );
        let private = openat(
            fd,
            OsStr::new("private"),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )
        .unwrap();
        std::fs::write(path.join("a"), b"a").unwrap();
        std::fs::write(path.join("b"), b"b").unwrap();

        let taken = rename_noreplace(fd, OsStr::new("a"), fd, OsStr::new("b")).unwrap_err();
        rename_noreplace(
            fd,
            OsStr::new("a"),
            std::os::fd::AsFd::as_fd(&private),
            OsStr::new("a"),
        )
        .unwrap();

        assert_eq!(taken.raw_os_error(), Some(libc::EEXIST));
        assert_eq!(std::fs::read(path.join("b")).unwrap(), b"b");
        assert_eq!(std::fs::read(path.join("private/a")).unwrap(), b"a");
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn a_link_takes_a_name_only_where_none_exists() {
        let path = std::env::temp_dir().join(format!("synoreclaim-link-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let directory = File::open(&path).unwrap();
        let fd = std::os::fd::AsFd::as_fd(&directory);
        std::fs::write(path.join("a"), b"a").unwrap();
        std::fs::write(path.join("b"), b"b").unwrap();

        let taken = link_at(fd, OsStr::new("a"), fd, OsStr::new("b")).unwrap_err();
        link_at(fd, OsStr::new("a"), fd, OsStr::new("c")).unwrap();

        assert_eq!(taken.raw_os_error(), Some(libc::EEXIST));
        assert_eq!(std::fs::read(path.join("b")).unwrap(), b"b");
        assert_eq!(std::fs::read(path.join("c")).unwrap(), b"a");
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn noatime_open_does_not_retry_other_errors() {
        let mut attempts = 0;
        let error = open_with_noatime(libc::O_RDONLY, |_| {
            attempts += 1;
            Err(io::Error::from_raw_os_error(libc::ENOENT))
        })
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    }

    #[test]
    fn signal_handler_checks_leave_inventory_cancellation_unchanged() {
        assert_eq!(stop_signal(), None);
        assert!(!STOP_REQUESTED.load(std::sync::atomic::Ordering::Relaxed));
        a_stop_signal_sets_the_flag_once_then_resets();
        assert_eq!(stop_signal(), None);
        assert!(!STOP_REQUESTED.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn a_stop_signal_sets_the_flag_once_then_resets() {
        // Signal dispositions and stop flags are process-wide. Keep this
        // check out of concurrent inventory and child-wait tests.
        if std::env::var_os("RECLAIM_STOP_SIGNAL_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sys::tests::a_stop_signal_sets_the_flag_once_then_resets",
                    "--nocapture",
                ])
                .env("RECLAIM_STOP_SIGNAL_CHILD", "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        let flag = install_stop_handlers().unwrap();
        // SAFETY: raising a signal whose handler was just installed.
        assert_eq!(unsafe { libc::raise(libc::SIGHUP) }, 0);
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(stop_signal(), Some(libc::SIGHUP));
        // SAFETY: querying the current disposition only.
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: old is a valid, writable sigaction.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut old) },
            0
        );
        assert_eq!(old.sa_sigaction, libc::SIG_DFL);
    }

    #[test]
    fn reports_space_of_the_root_filesystem() {
        let root = std::fs::File::open("/").unwrap();
        let space = filesystem_space(std::os::fd::AsFd::as_fd(&root)).unwrap();
        assert!(space.total > 0 && space.free <= space.total && space.available <= space.free);
    }

    #[test]
    fn ioctl_numbers_match_linux_uapi() {
        assert_eq!(BTRFS_IOC_TREE_SEARCH_V2, 0xC070_9411);
        assert_eq!(BTRFS_IOC_INO_LOOKUP, 0xD000_9412);
        assert_eq!(BTRFS_IOC_FS_INFO, 0x8400_941F);
        assert_eq!(BTRFS_IOC_SPACE_INFO, 0xC010_9414);
        assert_eq!(BTRFS_IOC_GET_FEATURES, 0x8018_9439);
        assert_eq!(BTRFS_IOC_BALANCE_PROGRESS, 0x8400_9422);
        assert_eq!(BTRFS_IOC_DEV_REPLACE, 0xCA28_9435);
        assert_eq!(BTRFS_IOC_DEV_INFO, 0xD000_941E);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(FS_IOC_GETFLAGS, 0x8008_6601);
    }

    /// START is 0 and CANCEL is 2: any other value would act on a replace.
    #[test]
    fn the_replace_probe_only_asks_for_status() {
        assert_eq!(DEV_REPLACE_CMD_STATUS, 1);
    }

    #[test]
    fn ioctl_structs_match_uapi_sizes() {
        assert_eq!(size_of::<SearchKey>(), 104);
        assert_eq!(size_of::<SearchArgsV2>(), 112);
        assert_eq!(size_of::<SpaceArgs>(), 16);
        assert_eq!(size_of::<InoLookupArgs>(), 4096);
        assert_eq!(size_of::<FsInfoArgs>(), 1024);
        assert_eq!(size_of::<FeatureFlagsArgs>(), 24);
        assert_eq!(size_of::<BalanceArgs>(), 1024);
        assert_eq!(std::mem::offset_of!(BalanceArgs, state), 8);
        assert_eq!(size_of::<DevReplaceArgs>(), 2600);
        assert_eq!(std::mem::offset_of!(DevReplaceArgs, replace_state), 16);
        assert_eq!(std::mem::offset_of!(DevReplaceArgs, spare), 2088);
        assert_eq!(size_of::<DevInfoArgs>(), 4096);
        assert_eq!(std::mem::offset_of!(DevInfoArgs, path), 3072);
    }

    #[test]
    fn parses_space_info_rows_without_losing_flags_or_byte_counts() {
        let rows = space_info_rows(&[2, 2, 1, 100, 70, 4, 50, 30]).unwrap();

        assert_eq!(
            rows,
            vec![
                SpaceInfo {
                    flags: 1,
                    total_bytes: 100,
                    used_bytes: 70,
                },
                SpaceInfo {
                    flags: 4,
                    total_bytes: 50,
                    used_bytes: 30,
                },
            ]
        );
    }

    #[test]
    fn a_balance_runs_only_while_its_running_bit_is_set() {
        let not_connected = || Err(io::Error::from_raw_os_error(libc::ENOTCONN));
        assert!(!balance_running_from(not_connected(), 0).unwrap());
        assert!(!balance_running_from(Ok(()), BALANCE_STATE_PAUSE_REQ).unwrap());
        assert!(balance_running_from(Ok(()), BALANCE_STATE_RUNNING).unwrap());
        let denied = Err(io::Error::from_raw_os_error(libc::EPERM));
        assert!(balance_running_from(denied, 0).is_err());
    }

    #[test]
    fn a_device_replace_runs_only_while_started() {
        assert!(dev_replace_running_from(DEV_REPLACE_STATE_STARTED));
        for idle in [0, 2, 3, 4] {
            assert!(!dev_replace_running_from(idle));
        }
    }

    #[test]
    fn fs_ioc_fsgetxattr_is_the_uapi_value() {
        assert_eq!(FS_IOC_FSGETXATTR, 0x801c_581f);
        assert_eq!(size_of::<IfDqblk>(), 72);
    }

    #[test]
    fn inode_counts_of_the_root_filesystem_are_consistent() {
        let root = File::open("/").unwrap();
        let inodes = filesystem_inodes(root.as_fd()).unwrap();
        assert!(inodes.total >= inodes.free);
    }

    #[test]
    fn a_missing_project_quota_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir();
        let _ = project_id(File::open(&dir).unwrap().as_fd());
        assert!(project_quota(std::path::Path::new("/dev/no-such-device"), 1001).is_err());
    }
}
