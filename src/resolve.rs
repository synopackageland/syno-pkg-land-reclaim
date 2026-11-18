//! Live target resolution (SPEC 5.1, 4.6, 22.6): operands to filesystem
//! objects, using the mount table and Btrfs identity ioctls.

use crate::btrfs::BTRFS_SUPER_MAGIC;
use crate::btrfs::live::OpenTarget;
use crate::doctor::format_uuid;
use crate::filesystem::FilesystemKind;
use crate::model::tree::Marks;
use crate::mounts::{self, BtrfsVolume, Mount};
use crate::output::{JsonPath, TargetError};
use crate::sys;
use crate::target::{Resolved, Scope, TargetArg};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// `statfs.f_type` of eCryptfs.
pub const ECRYPTFS_SUPER_MAGIC: u64 = 0xf15f;

/// A mounted Btrfs filesystem and its identity.
#[derive(Debug)]
pub struct Volume {
    pub primary: Mount,
    pub fsid: Result<[u8; 16], String>,
}

/// The mount table as read once per command.
#[derive(Debug)]
pub struct Environment {
    pub mounts: Vec<Mount>,
    pub volumes: Vec<Volume>,
}

impl Environment {
    pub fn load() -> io::Result<Self> {
        let mounts = mounts::read_mounts()?;
        let mut volumes: Vec<Volume> = Vec::new();
        let mounted = if Path::new(crate::cache::DSM_VERSION_FILE).is_file() {
            mounts::dsm_data_volumes(&mounts)
        } else {
            mounts::supported_volumes(&mounts)
        };
        for BtrfsVolume { primary, .. } in mounted {
            let fsid = if primary.fstype == "ext4" {
                crate::ext4::volume_uuid(&primary)
            } else {
                OpenTarget::open(&primary.mount_point)
                    .and_then(|t| sys::fs_info(t.file.as_fd()))
                    .map(|info| info.fsid)
            }
            .map_err(|e| e.to_string());
            // Mounts of one filesystem with different device numbers.
            if let Ok(id) = &fsid
                && volumes.iter().any(|v| v.fsid.as_ref() == Ok(id))
            {
                continue;
            }
            volumes.push(Volume { primary, fsid });
        }
        Ok(Environment { mounts, volumes })
    }

    fn volume_of(&self, fsid: &[u8; 16]) -> Option<&Volume> {
        self.volumes.iter().find(|v| v.fsid.as_ref() == Ok(fsid))
    }

    /// Targets for `system`, plus volumes it cannot include.
    pub fn system_targets(&self) -> (Vec<Resolved>, Vec<TargetError>) {
        let mut targets = Vec::new();
        let mut excluded = Vec::new();
        for volume in &self.volumes {
            match resolve_path(self, &volume.primary.mount_point, Some(Scope::Volume)) {
                Ok(t) => targets.push(t),
                Err(e) => excluded.push(e),
            }
        }
        for mount in mounts::unsupported_volumes(&self.mounts) {
            excluded.push(error(
                &mount.mount_point,
                "unsupported_filesystem",
                format!("{} volume, unsupported filesystem", mount.fstype),
            ));
        }
        (targets, excluded)
    }

    /// eCryptfs lower directories on filesystem `fsid`, as identities.
    pub fn encrypted_marks(&self, fsid: &[u8; 16]) -> Marks {
        let mut marks = Marks::default();
        for (lower, _) in mounts::ecryptfs_mounts(&self.mounts) {
            if let Ok((id, tree, ino)) = identity(lower)
                && &id == fsid
            {
                marks.encrypted.insert((tree, ino));
            }
        }
        marks
    }
}

/// `(fsid, tree, inode)` of a Btrfs path.
pub fn identity(path: &Path) -> io::Result<([u8; 16], u64, u64)> {
    let target = OpenTarget::open(path)?;
    let fsid = sys::fs_info(target.file.as_fd())?.fsid;
    Ok((fsid, target.subvolume_id()?, target.ino))
}

pub fn fsid_text(fsid: &[u8; 16]) -> String {
    format_uuid(fsid)
}

fn error(path: &Path, code: &'static str, message: impl Into<String>) -> TargetError {
    TargetError::new(JsonPath::new(path.as_os_str().as_bytes()), code, message)
}

fn io_error(path: &Path, e: &io::Error) -> TargetError {
    let code = match e.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "io_error",
    };
    error(path, code, e.to_string())
}

/// Resolves one operand path; `scope` forces or validates the scope.
pub fn resolve_path(
    env: &Environment,
    requested: &Path,
    scope: Option<Scope>,
) -> Result<Resolved, TargetError> {
    let path = std::fs::canonicalize(requested).map_err(|e| io_error(requested, &e))?;
    let target = OpenTarget::open(&path).map_err(|e| io_error(requested, &e))?;
    let fs_type = sys::filesystem_type(target.file.as_fd()).map_err(|e| io_error(requested, &e))?;
    if fs_type == ECRYPTFS_SUPER_MAGIC {
        return Err(error(
            requested,
            "unsupported_encrypted",
            "eCryptfs shared folder: names are encrypted in Btrfs metadata; not supported",
        ));
    }
    if fs_type == crate::ext4::SUPER_MAGIC {
        return resolve_ext4(env, requested, path, target, scope);
    }
    if fs_type != BTRFS_SUPER_MAGIC {
        return Err(error(
            requested,
            "not_btrfs",
            format!("not on Btrfs (filesystem type {fs_type:#x})"),
        ));
    }
    let fsid = sys::fs_info(target.file.as_fd())
        .map_err(|e| io_error(requested, &e))?
        .fsid;
    let volume = env.volume_of(&fsid).ok_or_else(|| {
        error(
            requested,
            "no_mount",
            "Btrfs filesystem not found in the mount table",
        )
    })?;
    let is_mount = path == volume.primary.mount_point;
    let scope = match scope {
        Some(Scope::Volume) if !is_mount => {
            return Err(error(
                requested,
                "not_a_volume",
                format!(
                    "not a volume mount; the volume is {}",
                    volume.primary.mount_point.display()
                ),
            ));
        }
        Some(scope) => scope,
        None if is_mount => Scope::Volume,
        None => Scope::Path,
    };
    let tree = target.subvolume_id().map_err(|e| io_error(requested, &e))?;
    Ok(Resolved {
        filesystem: FilesystemKind::Btrfs,
        requested: requested.to_path_buf(),
        path,
        scope,
        fsid,
        mount: volume.primary.mount_point.clone(),
        tree,
        ino: target.ino,
    })
}

fn resolve_ext4(
    env: &Environment,
    requested: &Path,
    path: PathBuf,
    target: OpenTarget,
    scope: Option<Scope>,
) -> Result<Resolved, TargetError> {
    use std::os::unix::fs::MetadataExt;
    let device = target
        .file
        .metadata()
        .map_err(|e| io_error(requested, &e))?
        .dev();
    let volume = env
        .volumes
        .iter()
        .find(|volume| {
            volume.primary.fstype == "ext4"
                && path.starts_with(&volume.primary.mount_point)
                && std::fs::metadata(&volume.primary.mount_point)
                    .is_ok_and(|meta| meta.dev() == device)
        })
        .ok_or_else(|| {
            error(
                requested,
                "unsupported_filesystem",
                "ext4 system partition or unrecognized data volume",
            )
        })?;
    // A same-device bind alias is not another namespace to walk.
    if env.mounts.iter().any(|mount| {
        mount.mount_point != volume.primary.mount_point
            && path.starts_with(&mount.mount_point)
            && mount.mount_point.starts_with(&volume.primary.mount_point)
    }) {
        return Err(error(
            requested,
            "mount_boundary",
            "target is below an excluded mount",
        ));
    }
    let is_mount = path == volume.primary.mount_point;
    if scope == Some(Scope::Volume) && !is_mount {
        return Err(error(requested, "not_a_volume", "not an ext4 volume mount"));
    }
    let fsid = *volume.fsid.as_ref().map_err(|message| {
        error(
            requested,
            "filesystem_identity_unavailable",
            message.clone(),
        )
    })?;
    Ok(Resolved {
        filesystem: FilesystemKind::Ext4,
        requested: requested.to_path_buf(),
        path,
        scope: scope.unwrap_or(if is_mount { Scope::Volume } else { Scope::Path }),
        fsid,
        mount: volume.primary.mount_point.clone(),
        tree: device,
        ino: target.ino,
    })
}

/// Resolves all operands; failures become per-target errors.
pub fn resolve_all(
    env: &Environment,
    args: &[TargetArg],
    scope: Option<Scope>,
) -> (Vec<Resolved>, Vec<TargetError>) {
    let mut resolved = Vec::new();
    let mut errors = Vec::new();
    for arg in args {
        match arg {
            TargetArg::System => {
                let (targets, excluded) = env.system_targets();
                resolved.extend(targets);
                errors.extend(excluded);
            }
            TargetArg::Path(path) => match resolve_path(env, path, scope) {
                Ok(t) => resolved.push(t),
                Err(e) => errors.push(e),
            },
        }
    }
    (resolved, errors)
}

/// Operands followed by `--from-file` paths.
pub fn read_operands(
    operands: &[std::ffi::OsString],
    from_file: Option<&PathBuf>,
    null: bool,
) -> Result<Vec<TargetArg>, String> {
    use std::io::Read;
    let mut args: Vec<TargetArg> = operands
        .iter()
        .cloned()
        .map(crate::target::parse_operand)
        .collect();
    if let Some(file) = from_file {
        let mut data = Vec::new();
        let read = if file.as_os_str() == "-" {
            io::stdin().read_to_end(&mut data)
        } else {
            std::fs::File::open(file).and_then(|mut f| f.read_to_end(&mut data))
        };
        read.map_err(|e| format!("{}: {e}", file.display()))?;
        let paths = crate::target::read_path_list(&data, null).map_err(|e| e.to_string())?;
        args.extend(paths.into_iter().map(TargetArg::Path));
    }
    if args.is_empty() {
        return Err("no targets: give `system`, a volume, or paths".into());
    }
    Ok(args)
}
