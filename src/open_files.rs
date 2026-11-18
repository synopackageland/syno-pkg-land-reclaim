//! One-shot, best-effort `/proc` scan for deleted files that processes
//! still hold open or mapped (SPEC 5.15 `--processes`). Advisory: a file
//! opened or closed during the scan may be missed.

use crate::why_not_freed::OpenDeleted;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Which of a process's mapped files `holder_links` visits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mapped {
    All,
    /// Only those `maps` marks ` (deleted)`: a DSM process maps hundreds
    /// of files, and reading every link costs about a second per scan.
    Deleted,
}

/// What processes under `proc_root` hold: deleted regular files for which
/// `on_volume` is true, and how many processes could not be read.
pub fn deleted_files(
    proc_root: &Path,
    on_volume: impl Fn(&File) -> bool,
) -> (Vec<OpenDeleted>, u64) {
    let mut found = Vec::new();
    let unreadable = holder_links(proc_root, Mapped::Deleted, |pid, comm, link| {
        if let Some((dev, ino, allocated_bytes)) = deleted_file(link, &on_volume) {
            found.push(OpenDeleted {
                pid,
                comm: comm.to_vec(),
                dev,
                ino,
                allocated_bytes,
            });
        }
    });
    (found, unreadable)
}

/// The regular files other processes hold open or mapped, as
/// `(st_dev, st_ino)` with the first holder's name, on the devices for
/// which `on_device` is true; and how many processes could not be read.
pub fn open_inodes(
    proc_root: &Path,
    on_device: impl Fn(u64) -> bool,
) -> (BTreeMap<(u64, u64), Vec<u8>>, u64) {
    let mut held = BTreeMap::new();
    let unreadable = holder_links(proc_root, Mapped::All, |_, comm, link| {
        if let Ok(meta) = fs::metadata(link)
            && meta.is_file()
            && on_device(meta.dev())
        {
            held.entry((meta.dev(), meta.ino()))
                .or_insert_with(|| comm.to_vec());
        }
    });
    (held, unreadable)
}

/// Calls `visit(pid, comm, link)` for every open descriptor and mapped
/// file of every other process under `proc_root`; returns how many
/// processes could not be read.
fn holder_links(proc_root: &Path, mapped: Mapped, mut visit: impl FnMut(u32, &[u8], &Path)) -> u64 {
    let mut unreadable = 0;
    let Ok(entries) = fs::read_dir(proc_root) else {
        return 1;
    };
    let own = std::process::id();
    for pid in entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
    {
        if pid == own {
            continue;
        }
        let dir = proc_root.join(pid.to_string());
        let comm = fs::read(dir.join("comm")).unwrap_or_default();
        let comm = comm.strip_suffix(b"\n").unwrap_or(&comm);
        let fds = visit_links(&dir.join("fd"), |link| visit(pid, comm, link));
        let maps = match mapped {
            Mapped::All => visit_links(&dir.join("map_files"), |link| visit(pid, comm, link)),
            Mapped::Deleted => fs::read(dir.join("maps")).map(|maps| {
                for range in deleted_ranges(&maps) {
                    visit(pid, comm, &dir.join("map_files").join(range));
                }
            }),
        };
        // NotFound: the process exited during the scan.
        let blocked = [fds, maps].iter().any(|result| {
            result
                .as_ref()
                .is_err_and(|e| e.kind() != io::ErrorKind::NotFound)
        });
        unreadable += u64::from(blocked);
    }
    unreadable
}

fn visit_links(dir: &Path, mut visit: impl FnMut(&Path)) -> io::Result<()> {
    for link in fs::read_dir(dir)?.flatten() {
        visit(&link.path());
    }
    Ok(())
}

/// The `map_files` names of the ranges `maps` lists as a deleted file:
/// `maps` pads addresses with zeros, `map_files` does not.
fn deleted_ranges(maps: &[u8]) -> impl Iterator<Item = String> + '_ {
    maps.split(|&b| b == b'\n')
        .filter(|line| line.ends_with(b" (deleted)"))
        .filter_map(|line| {
            let range = std::str::from_utf8(line.split(|&b| b == b' ').next()?).ok()?;
            let (start, end) = range.split_once('-')?;
            let start = u64::from_str_radix(start, 16).ok()?;
            let end = u64::from_str_radix(end, 16).ok()?;
            Some(format!("{start:x}-{end:x}"))
        })
}

/// `(st_dev, st_ino, allocated bytes)` when `link` leads to an unlinked
/// regular file on the volume.
fn deleted_file(link: &Path, on_volume: &impl Fn(&File) -> bool) -> Option<(u64, u64, u64)> {
    let target = fs::read_link(link).ok()?;
    if !target
        .as_os_str()
        .as_encoded_bytes()
        .ends_with(b" (deleted)")
    {
        return None;
    }
    let meta = fs::metadata(link).ok()?;
    if !meta.is_file() || meta.nlink() != 0 {
        return None;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(link)
        .ok()?;
    on_volume(&file).then(|| (meta.dev(), meta.ino(), meta.blocks() * 512))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Child, Command, Stdio};

    fn ready_holder(file: File) -> (Child, UnixStream) {
        let (control, child_control) = UnixStream::pair().unwrap();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("printf 'ready\\n'; read -r line <&2")
            .stdin(Stdio::from(file))
            .stdout(Stdio::piped())
            .stderr(Stdio::from(OwnedFd::from(child_control)))
            .spawn()
            .unwrap();
        // The message proves the holder has executed its shell before /proc
        // is sampled. Keep it blocked on a socket without spawning descendants.
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        (child, control)
    }

    #[test]
    fn finds_a_deleted_file_a_child_process_holds_open() {
        let dir = crate::spill::testing::scratch("open-files");
        let path = dir.join("held");
        std::fs::write(&path, vec![7u8; 64 << 10]).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let (mut child, _control) = ready_holder(file);
        std::fs::remove_file(&path).unwrap();

        let (found, _) = deleted_files(Path::new("/proc"), |_| true);
        let (none, _) = deleted_files(Path::new("/proc"), |_| false);
        child.kill().unwrap();
        child.wait().unwrap();

        let held: Vec<&OpenDeleted> = found.iter().filter(|f| f.pid == child.id()).collect();
        assert_eq!(held.len(), 1, "{found:?}");
        assert_eq!(held[0].comm, b"sh");
        assert!(held[0].allocated_bytes >= 64 << 10, "{held:?}");
        assert!(!none.iter().any(|f| f.pid == child.id()));
    }

    #[test]
    fn deleted_ranges_name_the_map_files_links_of_deleted_mappings() {
        let maps = b"00400000-00452000 r-xp 00000000 08:02 173521 /usr/bin/dbus-daemon\n\
7f2c4e5f1000-7f2c4e5f3000 rw-s 00000000 00:05 1234 /SYSV00000000 (deleted)\n\
00651000-00652000 r--p 00051000 08:02 173521 /tmp/gone (deleted)\n\
7ffd8a1fe000-7ffd8a21f000 rw-p 00000000 00:00 0 [stack]\n";
        assert_eq!(
            deleted_ranges(maps).collect::<Vec<_>>(),
            ["7f2c4e5f1000-7f2c4e5f3000", "651000-652000"]
        );
    }

    #[test]
    fn finds_a_deleted_program_a_process_still_runs() {
        let dir = crate::spill::testing::scratch("open-mapped");
        let program = dir.join("sleeper");
        std::fs::copy("/bin/sleep", &program).unwrap();
        // A parallel test's fork can briefly hold the copy open for writing.
        let mut child = (0..50)
            .find_map(|_| match Command::new(&program).arg("30").spawn() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                }
                started => Some(started.unwrap()),
            })
            .unwrap();
        let exe = Path::new("/proc").join(child.id().to_string()).join("exe");
        for _ in 0..250 {
            if fs::read_link(&exe).is_ok_and(|target| target == program) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::fs::remove_file(&program).unwrap();
        // map_files needs CAP_SYS_ADMIN, which DSM's root has and a container's may not.
        let maps = Path::new("/proc")
            .join(child.id().to_string())
            .join("map_files");
        if !fs::read_dir(&maps)
            .is_ok_and(|mut links| links.any(|l| l.is_ok_and(|l| fs::metadata(l.path()).is_ok())))
        {
            child.kill().unwrap();
            child.wait().unwrap();
            eprintln!("skipped: {} is not readable", maps.display());
            return;
        }

        let (found, _) = deleted_files(Path::new("/proc"), |_| true);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(found.iter().any(|f| f.pid == child.id()), "{found:?}");
    }

    #[test]
    fn names_the_holder_of_each_file_other_processes_hold_open() {
        let dir = crate::spill::testing::scratch("open-inodes");
        let path = dir.join("held");
        std::fs::write(&path, b"x").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let (mut child, _control) = ready_holder(file);

        // Parallel tests can fork while `file` is open and briefly inherit its
        // descriptor before exec. Limit this fixture to the intended holder,
        // while still reading its real kernel fd/map_files and comm entries.
        let proc_root = dir.join("proc");
        fs::create_dir(&proc_root).unwrap();
        std::os::unix::fs::symlink(
            format!("/proc/{}", child.id()),
            proc_root.join(child.id().to_string()),
        )
        .unwrap();
        let (held, _) = open_inodes(&proc_root, |dev| dev == meta.dev());
        let (elsewhere, _) = open_inodes(&proc_root, |_| false);
        child.kill().unwrap();
        child.wait().unwrap();

        assert_eq!(
            held.get(&(meta.dev(), meta.ino())).map(Vec::as_slice),
            Some(&b"sh"[..])
        );
        assert!(elsewhere.is_empty());
    }
}
