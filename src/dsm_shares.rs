//! DSM's authoritative share inventory; ordinary volume folders are not shares.

use crate::sys;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const PROGRAM: &str = "/usr/syno/sbin/synoshare";
const MAX_OUTPUT: usize = 1 << 20;
const MAX_SHARES: usize = 4096;

pub fn parse_inventory(bytes: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let mut lines = bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .split(|&byte| byte == b'\n');
    let count = lines
        .next()
        .and_then(|header| header.strip_suffix(b" Listed:"))
        .and_then(|count| std::str::from_utf8(count).ok())
        .and_then(|count| count.parse::<usize>().ok())
        .filter(|&count| count <= MAX_SHARES)
        .ok_or_else(|| io::Error::other("invalid DSM share inventory"))?;
    let names: Vec<_> = lines.map(<[u8]>::to_vec).collect();
    if names.len() != count
        || names.iter().any(|name| {
            name.is_empty()
                || name == b"."
                || name == b".."
                || name.contains(&b'/')
                || name.contains(&0)
        })
        || names.iter().collect::<BTreeSet<_>>().len() != names.len()
    {
        return Err(io::Error::other("invalid DSM share names or count"));
    }
    Ok(names)
}

/// The names of the shares DSM registers on the volume mounted at `mount`.
pub fn inventory(mount: &Path) -> io::Result<Vec<Vec<u8>>> {
    for ancestor in Path::new(PROGRAM)
        .parent()
        .expect("absolute program")
        .ancestors()
    {
        let meta = std::fs::symlink_metadata(ancestor)?;
        if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(io::Error::other(
                "DSM share inventory program has untrusted ancestors",
            ));
        }
    }
    if !crate::rootfs::delegate::trusted_program(Path::new(PROGRAM), 0) {
        return Err(io::Error::other(
            "DSM share inventory program is unavailable or untrusted",
        ));
    }
    let child = Command::new(PROGRAM)
        .arg("--enummnt")
        .arg(mount)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()?;
    read_inventory(child)
}

fn read_inventory(child: std::process::Child) -> io::Result<Vec<Vec<u8>>> {
    read_inventory_until(child, || crate::sys::stop_signal().is_some())
}

fn read_inventory_until(
    mut child: std::process::Child,
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<Vec<Vec<u8>>> {
    let stdout = child.stdout.take().expect("piped");
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = send.send(result);
    });
    // This only spaces cancellation checks; it never expires the operation.
    let output = loop {
        match receive.recv_timeout(Duration::from_millis(50)) {
            Ok(output) => break output,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !cancelled() => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                break Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "DSM share inventory cancelled",
                ));
            }
            Err(error) => break Err(io::Error::other(error)),
        }
    };
    let result = match output {
        Ok(bytes) if bytes.len() <= MAX_OUTPUT => {
            crate::rootfs::delegate::wait_child(&mut child, &mut cancelled).and_then(|status| {
                if status.success() {
                    parse_inventory(&bytes)
                } else {
                    Err(io::Error::other("DSM share inventory failed"))
                }
            })
        }
        output => {
            let _ = crate::sys::signal_group(child.id(), libc::SIGKILL);
            match output {
                Ok(_) => Err(io::Error::other("DSM share inventory too large")),
                Err(error) => Err(error),
            }
        }
    };
    let _ = child.wait();
    let _ = reader.join();
    result
}

pub struct Share {
    pub name: Vec<u8>,
    pub root: File,
    pub device: u64,
    pub inode: u64,
}

/// Keep root fds pinned while their identities are consumed. No inventory
/// failure grants share-root trust or cleanup permission.
pub fn list(mount: &Path) -> io::Result<Vec<Share>> {
    let root = File::open(mount)?;
    let device = root.metadata()?.dev();
    inventory(mount)?
        .into_iter()
        .map(|name| {
            let root = sys::openat_noatime(
                root.as_fd(),
                OsStr::new(&OsString::from_vec(name.clone())),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )?;
            let meta = root.metadata()?;
            if meta.dev() != device {
                return Err(io::Error::other(
                    "DSM share is mounted on another filesystem",
                ));
            }
            Ok(Share {
                name,
                root,
                device,
                inode: meta.ino(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_inventory_output_and_exit_still_return_shares() {
        for script in [
            "sleep 11; printf '1 Listed:\nshared\n'",
            "printf '1 Listed:\nshared\n'; exec 1>&-; sleep 11",
        ] {
            let child = Command::new("/bin/sh")
                .args(["-c", script])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            assert_eq!(read_inventory(child).unwrap(), vec![b"shared".to_vec()]);
        }
    }
    #[test]
    fn inventory_can_be_cancelled_while_waiting_for_output() {
        let child = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        assert_eq!(
            read_inventory_until(child, || true).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn oversized_or_unsuccessful_inventory_is_still_refused() {
        for script in [
            "head -c 1048577 /dev/zero; sleep 30",
            "printf '0 Listed:\n'; exit 1",
        ] {
            let child = Command::new("/bin/sh")
                .args(["-c", script])
                .stdout(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap();
            assert!(read_inventory(child).is_err());
        }
    }
}
