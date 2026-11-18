//! The mount table (`/proc/self/mountinfo`): Btrfs volume discovery for
//! `system`/`volumes` and eCryptfs share detection (SPEC 4.6, 22.6).

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// `major:minor`; all mounts of one Btrfs filesystem share it.
    pub device: String,
    /// Path inside the filesystem that is mounted (`/@syno`, `/`).
    pub root: Vec<u8>,
    pub mount_point: PathBuf,
    pub fstype: String,
    /// Device, or for eCryptfs the lower directory.
    pub source: PathBuf,
    /// Mount and superblock options, retained for read-only timing hints.
    pub options: Vec<Vec<u8>>,
}

/// Parses mountinfo, skipping lines it cannot read.
pub fn parse_mountinfo(text: &[u8]) -> Vec<Mount> {
    text.split(|&b| b == b'\n').filter_map(parse_line).collect()
}

fn parse_line(line: &[u8]) -> Option<Mount> {
    let fields: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
    let separator = fields.iter().position(|f| *f == b"-")?;
    if separator < 6 || fields.len() < separator + 3 {
        return None;
    }
    Some(Mount {
        device: String::from_utf8(fields[2].to_vec()).ok()?,
        root: unescape(fields[3]),
        mount_point: path(fields[4]),
        fstype: String::from_utf8(fields[separator + 1].to_vec()).ok()?,
        source: path(fields[separator + 2]),
        options: fields[5]
            .split(|&byte| byte == b',')
            .chain(
                fields
                    .get(separator + 3)
                    .into_iter()
                    .flat_map(|options| options.split(|&byte| byte == b',')),
            )
            .map(<[u8]>::to_vec)
            .collect(),
    })
}

fn path(field: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(unescape(field)))
}

/// Undoes the kernel's octal escapes (`\\040` for space, and so on).
fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        let octal = field
            .get(i + 1..i + 4)
            .filter(|d| field[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match octal {
            Some(d) => {
                out.push(d.iter().fold(0u8, |v, c| v.wrapping_mul(8) + (c - b'0')));
                i += 4;
            }
            None => {
                out.push(field[i]);
                i += 1;
            }
        }
    }
    out
}

/// One mounted Btrfs filesystem and its primary mount: the one exposing
/// the highest subvolume path (then the shortest mount point), so bind
/// mounts and subvolume aliases are not listed as separate volumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedVolume {
    pub primary: Mount,
    pub aliases: usize,
}

/// Compatibility name for the existing Btrfs discovery callers.
pub type BtrfsVolume = MountedVolume;

pub fn btrfs_volumes(mounts: &[Mount]) -> Vec<BtrfsVolume> {
    grouped_volumes(mounts, false)
}

/// Mounted data filesystems; the primary system-partition mount `/` is
/// excluded together with all of its same-device bind aliases.
pub fn supported_volumes(mounts: &[Mount]) -> Vec<MountedVolume> {
    grouped_volumes(mounts, true)
        .into_iter()
        .filter(|volume| volume.primary.mount_point != std::path::Path::new("/"))
        .collect()
}

/// DSM ext4 data volumes are mounted directly at `/volumeN`; its private
/// service images belong to packages, not to the data-volume picker.
pub fn dsm_data_volumes(mounts: &[Mount]) -> Vec<MountedVolume> {
    supported_volumes(mounts)
        .into_iter()
        .filter(|volume| {
            volume.primary.fstype != "ext4" || is_dsm_volume(&volume.primary.mount_point)
        })
        .collect()
}

fn grouped_volumes(mounts: &[Mount], ext4: bool) -> Vec<BtrfsVolume> {
    let mut btrfs: Vec<&Mount> = mounts
        .iter()
        .filter(|m| m.fstype == "btrfs" || (ext4 && m.fstype == "ext4"))
        .collect();
    btrfs.sort_by(|a, b| {
        (
            &a.device,
            a.root.len(),
            a.mount_point.as_os_str().len(),
            &a.mount_point,
        )
            .cmp(&(
                &b.device,
                b.root.len(),
                b.mount_point.as_os_str().len(),
                &b.mount_point,
            ))
    });
    let mut volumes: Vec<BtrfsVolume> = Vec::new();
    for mount in btrfs {
        match volumes.last_mut() {
            Some(v) if v.primary.device == mount.device => v.aliases += 1,
            _ => volumes.push(BtrfsVolume {
                primary: mount.clone(),
                aliases: 0,
            }),
        }
    }
    volumes.sort_by(|a, b| a.primary.mount_point.cmp(&b.primary.mount_point));
    volumes
}

/// DSM data volumes that are not Btrfs, reported as unsupported by
/// `system` discovery instead of being silently left out.
pub fn unsupported_volumes(mounts: &[Mount]) -> Vec<&Mount> {
    mounts
        .iter()
        .filter(|m| m.fstype != "btrfs" && m.fstype != "ext4" && is_dsm_volume(&m.mount_point))
        .collect()
}

fn is_dsm_volume(path: &std::path::Path) -> bool {
    let Some(name) = path.to_str().and_then(|p| p.strip_prefix("/volume")) else {
        return false;
    };
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

/// Mounted eCryptfs shares: `(lower directory, upper mount point)`.
pub fn ecryptfs_mounts(mounts: &[Mount]) -> Vec<(&PathBuf, &PathBuf)> {
    mounts
        .iter()
        .filter(|m| m.fstype == "ecryptfs")
        .map(|m| (&m.source, &m.mount_point))
        .collect()
}

#[cfg(target_os = "linux")]
pub fn read_mounts() -> std::io::Result<Vec<Mount>> {
    Ok(parse_mountinfo(&std::fs::read("/proc/self/mountinfo")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like a test NAS (DSM 7.4) plus a Linux loop fixture.
    const MOUNTINFO: &str = "\
22 1 9:0 / / rw,noatime - ext4 /dev/md0 rw,data=ordered
45 17 0:36 /@syno /volume1 rw,nodev,noatime - btrfs /dev/mapper/cachedev_0 rw,ssd,subvolid=256,subvol=/@syno,commit=12
49 45 0:36 /@syno/@docker /volume1/@docker rw,nodev,noatime shared:1 - btrfs /dev/mapper/cachedev_0 rw,subvolid=256,subvol=/@syno
80 45 0:36 /@syno/backup /volume1/@appdata/all_shares/backup rw,nodev - btrfs /dev/mapper/cachedev_0 rw,subvolid=261,subvol=/@syno/backup
90 17 8:3 / /volume2 rw - ext4 /dev/sdb3 rw
91 45 0:50 / /volume1/secret rw - ecryptfs /volume1/@secret@ rw,ecryptfs_fnek_sig=abc
99 1 7:0 / /root/fix\\040ture/mnt rw - btrfs /dev/loop0 rw,subvolid=5,subvol=/
";

    fn mounts() -> Vec<Mount> {
        parse_mountinfo(MOUNTINFO.as_bytes())
    }

    #[test]
    fn parses_fields_and_octal_escapes() {
        let m = mounts();
        assert_eq!(m.len(), 7);
        assert_eq!(m[1].device, "0:36");
        assert_eq!(m[1].root, b"/@syno");
        assert_eq!(m[1].mount_point, PathBuf::from("/volume1"));
        assert_eq!(m[1].fstype, "btrfs");
        assert!(m[1].options.contains(&b"commit=12".to_vec()));
        assert_eq!(m[6].mount_point, PathBuf::from("/root/fix ture/mnt"));
    }

    #[test]
    fn skips_malformed_lines() {
        assert!(parse_mountinfo(b"garbage\n1 2 3\n").is_empty());
    }

    #[test]
    fn groups_btrfs_mounts_into_volumes_with_primary_mounts() {
        let volumes = btrfs_volumes(&mounts());
        let points: Vec<_> = volumes
            .iter()
            .map(|v| v.primary.mount_point.clone())
            .collect();
        assert_eq!(
            points,
            [
                PathBuf::from("/root/fix ture/mnt"),
                PathBuf::from("/volume1")
            ]
        );
        assert_eq!(volumes[1].aliases, 2);
    }

    #[test]
    fn ext4_dsm_volumes_are_supported() {
        let m = mounts();
        let unsupported: Vec<_> = unsupported_volumes(&m)
            .iter()
            .map(|m| m.mount_point.clone())
            .collect();
        assert!(unsupported.is_empty());
    }

    #[test]
    fn dsm_discovery_excludes_internal_ext4_images_and_preserves_btrfs() {
        let mut m = mounts();
        m.extend(parse_mountinfo(b"44 22 7:1 / /tmp/SynologyAuthService rw - ext4 /dev/loop1 rw\n46 45 7:2 / /volume1/@accountdb/@accountcache rw - ext4 /dev/loop2 rw\n47 22 7:3 / /root/ext4-fixture/mnt rw - ext4 /dev/loop3 rw\n"));
        let points: Vec<_> = dsm_data_volumes(&m)
            .into_iter()
            .map(|v| v.primary.mount_point)
            .collect();
        assert_eq!(
            points,
            [
                PathBuf::from("/root/fix ture/mnt"),
                PathBuf::from("/volume1"),
                PathBuf::from("/volume2")
            ]
        );
        let native = supported_volumes(&m);
        assert!(
            native
                .iter()
                .any(|v| v.primary.mount_point == std::path::Path::new("/root/ext4-fixture/mnt")),
            "native Linux loop integration remains supported"
        );
        assert!(
            !native
                .iter()
                .any(|v| v.primary.mount_point == std::path::Path::new("/"))
        );
    }

    #[test]
    fn finds_ecryptfs_lower_directories() {
        let m = mounts();
        assert_eq!(
            ecryptfs_mounts(&m),
            [(
                &PathBuf::from("/volume1/@secret@"),
                &PathBuf::from("/volume1/secret")
            )]
        );
    }
}
