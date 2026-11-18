//! Known DSM service paths (SPEC 4.6): which package manages the data
//! and where in DSM to manage it. One table, read by the service summary,
//! `--explain` and the cleanup guard.

/// Where a name sits relative to its volume mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// A direct child of the volume mount: `@…` service roots and shares.
    VolumeRoot,
    /// Anywhere deeper.
    Below,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pattern {
    Exact(&'static [u8]),
    OneOf(&'static [&'static [u8]]),
    Prefix(&'static [u8]),
    Suffix(&'static [u8]),
}

impl Pattern {
    fn matches(self, name: &[u8]) -> bool {
        match self {
            Pattern::Exact(p) => name == p,
            Pattern::OneOf(names) => names.contains(&name),
            Pattern::Prefix(p) => name.starts_with(p) && name.len() > p.len(),
            Pattern::Suffix(p) => name.ends_with(p) && name.len() > p.len(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    VolumeRoot,
    Below,
    Anywhere,
    /// Directly in the folder at this path below the volume root. Only
    /// [`service_within`] looks for these: the name-and-depth lookups never
    /// find them, so a path stays in its topmost service everywhere else.
    Under(&'static [u8]),
}

impl At {
    fn includes(self, depth: Depth) -> bool {
        matches!(
            (self, depth),
            (At::Anywhere, _) | (At::VolumeRoot, Depth::VolumeRoot) | (At::Below, Depth::Below)
        )
    }
}

/// One known DSM service path.
#[derive(Debug, PartialEq, Eq)]
pub struct Service {
    /// Stable JSON code.
    pub code: &'static str,
    /// What the data is and who writes it.
    pub package: &'static str,
    /// Where in DSM to manage or free it.
    pub manage_in: &'static str,
    /// Additional fixed context for data paths handled by this service.
    pub explanation: Option<&'static str>,
    /// The layout was seen on DSM 7.4 or is in Synology's documentation;
    /// otherwise it is an assumption, and output says so.
    pub confirmed: bool,
    /// Cleanup refuses it, and any selection holding it (SPEC 5.7).
    pub guarded: bool,
    /// The summary lists each path; otherwise it adds them up.
    pub list_each: bool,
    /// Rankings list its entries as ordinary ones instead of hiding
    /// them behind one service row (SPEC 5.4).
    pub ranks_contents: bool,
    at: At,
    pattern: Pattern,
}

const fn service_at(
    code: &'static str,
    at: At,
    pattern: Pattern,
    package: &'static str,
    manage_in: &'static str,
) -> Service {
    Service {
        code,
        package,
        manage_in,
        explanation: None,
        confirmed: true,
        guarded: false,
        list_each: true,
        ranks_contents: false,
        at,
        pattern,
    }
}

/// A direct child of the volume mount named exactly `name`.
const fn root(
    code: &'static str,
    name: &'static [u8],
    package: &'static str,
    manage_in: &'static str,
) -> Service {
    service_at(
        code,
        At::VolumeRoot,
        Pattern::Exact(name),
        package,
        manage_in,
    )
}

const fn unconfirmed(mut service: Service) -> Service {
    service.confirmed = false;
    service
}

const fn explained(mut service: Service, explanation: &'static str) -> Service {
    service.explanation = Some(explanation);
    service
}

const fn guarded(mut service: Service) -> Service {
    service.guarded = true;
    service
}

/// Ranked entry by entry, like any folder.
const fn ranked(mut service: Service) -> Service {
    service.ranks_contents = true;
    service
}

/// Added up in the summary instead of listed path by path.
const fn summed(mut service: Service) -> Service {
    service.list_each = false;
    service
}

const PACKAGE_CENTER: &str = "Package Center (the package's own settings)";
const SNAPSHOT_REPLICATION: &str =
    "Snapshot Replication > Snapshots (delete snapshots or change the retention policy)";
const DRIVE_ADMIN_CONSOLE: &str =
    "Synology Drive Admin Console > Team Folder (version settings); Drive's own recycle bin";
const DRIVE_SHARESYNC: &str = "Synology Drive ShareSync";
const ACTIVE_BACKUP_OTHER_PACKAGE: &str = "the Active Backup package that created it";
const CONTAINER_MANAGER: &str = "Container Manager (remove unused images and containers)";
const DATABASES: &str = "the package that owns each database";
const SURVEILLANCE_STATION: &str = "Surveillance Station";
const SURVEILLANCE_RECORDINGS: &str =
    "Surveillance Station > Recording > Settings (rotation and retention)";
const SAN_MANAGER: &str = "SAN Manager";
const CLOUD_SYNC: &str = "Cloud Sync";
const USB_COPY: &str = "USB Copy";
const DSM_TEMP: &str = "DSM (the system clears it)";
const DSM_QUOTA: &str =
    "DSM (user and shared folder quotas, in Control Panel > User and Shared Folder)";
const RECYCLE_BIN: &str = "Control Panel > Shared Folder > Action > Empty Recycle Bin, or the share's Recycle Bin settings";
const FOLDER_METADATA: &str =
    "the folder it belongs to (it is deleted with its folder, and DSM indexing recreates it)";
const HYPER_BACKUP: &str = "Hyper Backup or Hyper Backup Vault (delete versions or the task there; never delete its files directly)";
const HYPER_BACKUP_CACHE: &str = "Hyper Backup (the backup task that uses it)";
pub const OWNING_PACKAGE: &str = "the package that created it";

/// `manage_in` texts the JSON documents use outside the service table;
/// [`PLACES`] maps each to its stable id.
pub const SNAPSHOT_REPLICATION_SHORT: &str = "Snapshot Replication";
pub const SAN_MANAGER_ISCSI: &str = "SAN Manager > iSCSI LUNs";
pub const SNAPSHOT_REPLICATION_SNAPSHOTS: &str = "Snapshot Replication > Snapshots";
pub const RECYCLE_BIN_EDIT: &str = "Control Panel > Shared Folder > Edit > Recycle Bin";
pub const FILE_STATION: &str = "File Station (permanent delete)";
pub const BTRFS_CLEANUP: &str = "Btrfs background cleanup";

/// Extended attributes a DSM ACL-mode file or directory reads back: its
/// own ACL or the one it inherits. A Linux-mode one, and any file off DSM,
/// has neither (a test NAS, DSM 7.4). The mode bits of an ACL-mode directory
/// read 000 while its ACL can grant any group access.
pub const ACL_XATTRS: [&str; 2] = ["system.syno_acl_self", "system.syno_acl_inherit"];

/// Where to manage a backup package's versions: in the package, never
/// by deleting its files.
macro_rules! backup_versions_in {
    ($package:literal) => {
        concat!(
            $package,
            " (delete versions or tasks there; never delete its files directly)"
        )
    };
}

const ACTIVE_BACKUP: &str = backup_versions_in!("Active Backup for Business");
const ACTIVE_BACKUP_MICROSOFT_365: &str = backup_versions_in!("Active Backup for Microsoft 365");
const ACTIVE_BACKUP_GOOGLE_WORKSPACE: &str =
    backup_versions_in!("Active Backup for Google Workspace");
const ISCSI_EXPLANATION: &str = "Thin-provisioned LUNs return deleted space to the volume only when space reclamation is enabled and the initiator sends UNMAP; thick-provisioned LUNs have their configured capacity allocated up front. VMM images are managed in Virtual Machine Manager > Image, where their selected storage is shown.";
const VMM_EXPLANATION: &str = "Virtual Machine Manager stores virtual disks under @iSCSI/LUN/VDISK_*. Their storage is checked in Virtual Machine Manager > Virtual Machine, not inferred from the Btrfs path.";
pub const SYNO_RECLAIM_MANAGE: &str = "Syno Reclaim > Scan history > Clear cache";
pub const VIRTUAL_MACHINE_MANAGER: &str = "Virtual Machine Manager";

/// First match wins. `@…` names at the volume root not listed here are
/// summed up as other DSM service data.
const SERVICES: &[Service] = &[
    root(
        "synology_drive",
        b"@synologydrive",
        "Synology Drive Server",
        DRIVE_ADMIN_CONSOLE,
    ),
    root(
        "synology_drive_sharesync",
        b"@SynologyDriveShareSync",
        "Synology Drive ShareSync",
        DRIVE_SHARESYNC,
    ),
    root(
        "active_backup",
        b"@ActiveBackup",
        "Active Backup for Business (database and settings)",
        ACTIVE_BACKUP,
    ),
    service_at(
        "active_backup_volume",
        At::VolumeRoot,
        Pattern::OneOf(&[b"@ActiveBackupVolume", b"@ABM_Encrypted@"]),
        "Active Backup for Business (volume data)",
        ACTIVE_BACKUP,
    ),
    guarded(summed(service_at(
        "active_backup_data",
        At::Below,
        Pattern::Exact(b"@ActiveBackup"),
        "Active Backup for Business (backup data)",
        ACTIVE_BACKUP,
    ))),
    guarded(summed(service_at(
        "active_backup_microsoft_365_data",
        At::Below,
        Pattern::Exact(b"ActiveBackupForMicrosoft365"),
        "Active Backup for Microsoft 365 (backup data)",
        ACTIVE_BACKUP_MICROSOFT_365,
    ))),
    guarded(summed(service_at(
        "active_backup_google_workspace_data",
        At::Below,
        Pattern::OneOf(&[b"ActiveBackupForGoogleWorkspace", b"ActiveBackupForGSuite"]),
        "Active Backup for Google Workspace (backup data)",
        ACTIVE_BACKUP_GOOGLE_WORKSPACE,
    ))),
    root(
        "active_backup_microsoft_365",
        b"@ActiveBackup-Office365",
        "Active Backup for Microsoft 365 (database)",
        ACTIVE_BACKUP_MICROSOFT_365,
    ),
    root(
        "active_backup_google_workspace",
        b"@ActiveBackup-GSuite",
        "Active Backup for Google Workspace (database)",
        ACTIVE_BACKUP_GOOGLE_WORKSPACE,
    ),
    unconfirmed(service_at(
        "active_backup_other",
        At::Anywhere,
        Pattern::Prefix(b"@ActiveBackup-"),
        "an Active Backup package",
        ACTIVE_BACKUP_OTHER_PACKAGE,
    )),
    root(
        "container_manager",
        b"@docker",
        "Container Manager",
        CONTAINER_MANAGER,
    ),
    root(
        "packages",
        b"@appstore",
        "installed packages",
        PACKAGE_CENTER,
    ),
    root(
        "package_data",
        b"@appdata",
        "installed packages (data)",
        PACKAGE_CENTER,
    ),
    root(
        "package_home",
        b"@apphome",
        "installed packages (home)",
        PACKAGE_CENTER,
    ),
    root(
        "package_config",
        b"@appconf",
        "installed packages (settings)",
        PACKAGE_CENTER,
    ),
    root(
        "package_share",
        b"@appshare",
        "installed packages (shared data)",
        PACKAGE_CENTER,
    ),
    root(
        "package_temp",
        b"@apptemp",
        "installed packages (temporary files)",
        PACKAGE_CENTER,
    ),
    unconfirmed(root(
        "databases",
        b"@database",
        "DSM and package databases",
        DATABASES,
    )),
    root(
        "share_snapshots",
        b"@sharesnap",
        "Snapshot Replication (shared folder snapshots)",
        SNAPSHOT_REPLICATION,
    ),
    unconfirmed(summed(service_at(
        "snapshot_view",
        At::Anywhere,
        Pattern::Exact(b"#snapshot"),
        "Snapshot Replication (visible snapshot folder)",
        SNAPSHOT_REPLICATION,
    ))),
    root(
        "snapshot_replication",
        b"@SnapshotReplication",
        "Snapshot Replication (package data)",
        SNAPSHOT_REPLICATION,
    ),
    root(
        "surveillance_data",
        b"@surveillance",
        "Surveillance Station (package data)",
        SURVEILLANCE_STATION,
    ),
    guarded(root(
        "surveillance_recordings",
        b"surveillance",
        "Surveillance Station (recordings)",
        SURVEILLANCE_RECORDINGS,
    )),
    explained(
        root(
            "iscsi_luns",
            b"@iSCSI",
            "SAN Manager (iSCSI LUNs)",
            SAN_MANAGER,
        ),
        ISCSI_EXPLANATION,
    ),
    root("cloud_sync", b"@cloudsync", "Cloud Sync", CLOUD_SYNC),
    root("usb_copy", b"@USBCopy", "USB Copy", USB_COPY),
    unconfirmed(root("dsm_temp", b"@tmp", "DSM temporary files", DSM_TEMP)),
    ranked(service_at(
        "recycle_bin",
        At::Anywhere,
        Pattern::Exact(b"#recycle"),
        "shared folder Recycle Bin",
        RECYCLE_BIN,
    )),
    unconfirmed(summed(service_at(
        "folder_metadata",
        At::Anywhere,
        Pattern::Exact(b"@eaDir"),
        "DSM indexing (thumbnails and file metadata)",
        FOLDER_METADATA,
    ))),
    guarded(summed(service_at(
        "hyper_backup",
        At::Anywhere,
        Pattern::Suffix(b".hbk"),
        "Hyper Backup (backup data)",
        HYPER_BACKUP,
    ))),
    unconfirmed(service_at(
        "hyper_backup_cache",
        At::VolumeRoot,
        Pattern::OneOf(&[b"@img_bkp_cache", b"@img_bkp_mount"]),
        "Hyper Backup (image and cloud cache)",
        HYPER_BACKUP_CACHE,
    )),
    // Stored reports index this table, so new rows go at the end.
    explained(
        service_at(
            "vmm_virtual_disks",
            At::Under(b"@iSCSI/LUN"),
            Pattern::Prefix(b"VDISK_"),
            "Virtual Machine Manager (virtual disks)",
            VIRTUAL_MACHINE_MANAGER,
        ),
        VMM_EXPLANATION,
    ),
    guarded(summed(service_at(
        "syno_reclaim",
        At::Under(b"@appdata"),
        Pattern::Exact(b"SynoReclaim"),
        "Syno Reclaim (this app)",
        SYNO_RECLAIM_MANAGE,
    ))),
    // An ext4 volume's quota files: the kernel's user and group quotas
    // and DSM's share quota database, directly in the volume root.
    service_at(
        "dsm_quota",
        At::VolumeRoot,
        Pattern::OneOf(&[b"aquota.user", b"aquota.group", b"synoquota.db"]),
        "DSM quota files",
        DSM_QUOTA,
    ),
];

/// A DSM place JSON names by a stable id (SPEC 4.6, GAP-14): the UI
/// localizes the id instead of parsing the English `manage_in` text.
/// Several services and `manage_in` texts may share one place.
#[derive(Debug, PartialEq, Eq)]
pub struct Place {
    /// Stable snake_case id.
    pub id: &'static str,
    /// Every English `manage_in` text that names this place.
    pub manage_in: &'static [&'static str],
}

/// The id of a place no `manage_in` text of the table names.
pub const OTHER_PLACE_ID: &str = "other";

/// Every stable DSM-place id JSON may carry. `manage_in` texts that
/// name the same place share its id; the ids are unique.
pub const PLACES: &[Place] = &[
    Place {
        id: "syno_reclaim",
        manage_in: &[SYNO_RECLAIM_MANAGE],
    },
    Place {
        id: "virtual_machine_manager",
        manage_in: &[VIRTUAL_MACHINE_MANAGER],
    },
    Place {
        id: "drive_admin_console",
        manage_in: &[DRIVE_ADMIN_CONSOLE],
    },
    Place {
        id: "drive_sharesync",
        manage_in: &[DRIVE_SHARESYNC],
    },
    Place {
        id: "active_backup_business",
        manage_in: &[ACTIVE_BACKUP],
    },
    Place {
        id: "active_backup_microsoft_365",
        manage_in: &[ACTIVE_BACKUP_MICROSOFT_365],
    },
    Place {
        id: "active_backup_google_workspace",
        manage_in: &[ACTIVE_BACKUP_GOOGLE_WORKSPACE],
    },
    Place {
        id: "active_backup_package",
        manage_in: &[ACTIVE_BACKUP_OTHER_PACKAGE],
    },
    Place {
        id: "container_manager",
        manage_in: &[CONTAINER_MANAGER],
    },
    Place {
        id: "package_center",
        manage_in: &[PACKAGE_CENTER],
    },
    Place {
        id: "owning_package",
        manage_in: &[DATABASES, OWNING_PACKAGE],
    },
    Place {
        id: "snapshot_replication",
        manage_in: &[
            SNAPSHOT_REPLICATION,
            SNAPSHOT_REPLICATION_SNAPSHOTS,
            SNAPSHOT_REPLICATION_SHORT,
        ],
    },
    Place {
        id: "surveillance_station",
        manage_in: &[SURVEILLANCE_STATION, SURVEILLANCE_RECORDINGS],
    },
    Place {
        id: "san_manager",
        manage_in: &[SAN_MANAGER, SAN_MANAGER_ISCSI],
    },
    Place {
        id: "cloud_sync",
        manage_in: &[CLOUD_SYNC],
    },
    Place {
        id: "usb_copy",
        manage_in: &[USB_COPY],
    },
    Place {
        id: "dsm",
        manage_in: &[DSM_TEMP, DSM_QUOTA],
    },
    Place {
        id: "shared_folder_recycle_bin",
        manage_in: &[RECYCLE_BIN, RECYCLE_BIN_EDIT],
    },
    Place {
        id: "owning_folder",
        manage_in: &[FOLDER_METADATA],
    },
    Place {
        id: "hyper_backup",
        manage_in: &[HYPER_BACKUP, HYPER_BACKUP_CACHE],
    },
    Place {
        id: "file_station",
        manage_in: &[FILE_STATION],
    },
    Place {
        id: "btrfs_cleanup",
        manage_in: &[BTRFS_CLEANUP],
    },
];

/// The stable id of the place `manage_in` names, or [`OTHER_PLACE_ID`].
pub fn place_id(manage_in: &str) -> &'static str {
    PLACES
        .iter()
        .find(|place| place.manage_in.contains(&manage_in))
        .map_or(OTHER_PLACE_ID, |place| place.id)
}

/// Stable compact index for a service, stored by volume overview reports.
pub fn service_id(service: &Service) -> u16 {
    SERVICES
        .iter()
        .position(|known| known.code == service.code)
        .and_then(|index| u16::try_from(index).ok())
        .expect("service table fits a u16")
}

/// The service at a compact index stored by a volume overview report.
pub fn service_by_id(id: u16) -> Option<&'static Service> {
    SERVICES.get(id as usize)
}

/// The bucket id used for DSM-managed paths absent from the service table.
pub const OTHER_SERVICE_ID: u16 = u16::MAX;

/// The stable id for a known code, or the `other` bucket for unknown code.
pub fn service_id_by_code(code: &str) -> u16 {
    SERVICES
        .iter()
        .find(|service| service.code == code)
        .map(service_id)
        .unwrap_or(OTHER_SERVICE_ID)
}

/// The appended service row, also used for identity-proven custom state paths.
pub fn syno_reclaim() -> &'static Service {
    service_by_id(service_id_by_code("syno_reclaim")).expect("service table includes this app")
}

fn own_package_path(rest: &[u8]) -> bool {
    rest == b"@appdata/SynoReclaim" || rest.starts_with(b"@appdata/SynoReclaim/")
}

/// Entries whose presence inside a directory marks that directory as
/// package data (SPEC 5.7): an Active Backup destination folder holds
/// `@ActiveBackup`, a Hyper Backup target holds its `.bkpi` file.
const MARKERS: &[(&[u8], &str)] = &[
    (b"@ActiveBackup", "active_backup_data"),
    (b"SynologyHyperBackup.bkpi", "hyper_backup"),
];

/// The known service a directory entry named `name` at `depth` is.
pub fn service(name: &[u8], depth: Depth) -> Option<&'static Service> {
    SERVICES
        .iter()
        .find(|s| s.at.includes(depth) && s.pattern.matches(name))
}

/// Whether `name` is the fixed name of a known service at some depth,
/// such as `@eaDir`: a pattern such as `@ActiveBackup-*` or `*.hbk` is
/// not, since the rest of such a name is chosen by whoever made it.
pub fn is_fixed_service_name(name: &[u8]) -> bool {
    SERVICES.iter().any(|s| {
        !s.is_nested()
            && matches!(s.pattern, Pattern::Exact(_) | Pattern::OneOf(_))
            && s.pattern.matches(name)
    })
}

/// The service nested in another one that the relative path `rest`
/// (below the volume root) is directly: a VMM virtual disk in `@iSCSI`.
/// Everything else keeps to its topmost service ([`service_of`]).
pub fn service_within(rest: &[u8]) -> Option<&'static Service> {
    let cut = rest.iter().rposition(|b| *b == b'/')?;
    let (parent, name) = (&rest[..cut], &rest[cut + 1..]);
    SERVICES
        .iter()
        .find(|s| matches!(s.at, At::Under(under) if under == parent) && s.pattern.matches(name))
}

/// The topmost known service holding `path`, a path below `mount`.
pub fn service_of(path: &[u8], mount: &[u8]) -> Option<&'static Service> {
    let rest = path.strip_prefix(mount)?.strip_prefix(b"/")?;
    if own_package_path(rest) {
        return Some(syno_reclaim());
    }
    service_along(rest, Depth::VolumeRoot, |_| true)
}

/// The topmost guarded service holding `path`, a path below `mount`:
/// package data by name (SPEC 5.7).
pub fn package_data_of(path: &[u8], mount: &[u8]) -> Option<&'static Service> {
    let rest = path.strip_prefix(mount)?.strip_prefix(b"/")?;
    if own_package_path(rest) {
        return Some(syno_reclaim());
    }
    service_along(rest, Depth::VolumeRoot, |s| s.guarded)
}

/// The topmost known service holding `rest`, a relative path whose
/// first entry is below the volume root.
pub fn service_below(rest: &[u8]) -> Option<&'static Service> {
    service_along(rest, Depth::Below, |_| true)
}

fn service_along(
    rest: &[u8],
    first: Depth,
    accept: impl Fn(&Service) -> bool,
) -> Option<&'static Service> {
    rest.split(|b| *b == b'/')
        .enumerate()
        .find_map(|(i, name)| {
            let depth = if i == 0 { first } else { Depth::Below };
            service(name, depth).filter(|s| accept(s))
        })
}

/// The package whose data an entry named `name` below a share is, or
/// marks its directory as: what cleanup never deletes (SPEC 5.7).
pub fn package_data(name: &[u8]) -> Option<&'static Service> {
    service(name, Depth::Below)
        .filter(|s| s.guarded)
        .or_else(|| marked_by(name))
}

/// Why cleanup refuses an entry named `name` below a share, or the
/// directory it marks as package data (SPEC 5.7).
pub fn package_data_refusal(name: &[u8]) -> Option<String> {
    service(name, Depth::Below)
        .filter(|s| s.guarded)
        .map(Service::refusal)
        .or_else(|| marker_refusal(name))
}

/// Why cleanup refuses a directory holding an entry named `marker`,
/// naming the marker when the service's own names do not.
pub fn marker_refusal(marker: &[u8]) -> Option<String> {
    let service = marked_by(marker)?;
    let marker = String::from_utf8_lossy(marker);
    let names = service.names();
    Some(if names == marker {
        service.refusal()
    } else {
        service.refusal_of(&format!("{names}, marked by {marker}"))
    })
}

/// The entry names that mark their directory as package data.
pub fn markers() -> impl Iterator<Item = &'static [u8]> {
    MARKERS.iter().map(|(marker, _)| *marker)
}

/// A stored code of `marker`: one more than its place among the
/// markers, so 0 names none.
pub fn marker_code(marker: &[u8]) -> u64 {
    markers()
        .position(|m| m == marker)
        .map_or(0, |i| i as u64 + 1)
}

/// The marker a stored `code` names; `None` for 0 or an unknown code.
pub fn marker_of_code(code: u64) -> Option<&'static [u8]> {
    markers().nth(usize::try_from(code.checked_sub(1)?).ok()?)
}

/// The service a directory holding an entry named `name` belongs to.
pub fn marked_by(name: &[u8]) -> Option<&'static Service> {
    let (_, code) = MARKERS.iter().find(|(marker, _)| *marker == name)?;
    SERVICES.iter().find(|s| s.code == *code)
}

impl Service {
    /// The names it matches: `@eaDir`, `*.hbk`, `@ActiveBackup-*`.
    pub fn names(&self) -> String {
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        match self.pattern {
            Pattern::Exact(name) => text(name),
            Pattern::OneOf(names) => names.iter().map(|n| text(n)).collect::<Vec<_>>().join(", "),
            Pattern::Prefix(prefix) => format!("{}*", text(prefix)),
            Pattern::Suffix(suffix) => format!("*{}", text(suffix)),
        }
    }

    /// It lies inside another service ([`service_within`]).
    pub fn is_nested(&self) -> bool {
        matches!(self.at, At::Under(_))
    }

    /// Whether a ranked folder holding a path of it leaves that path out
    /// of its row (SPEC 5.4): package data, and a Recycle Bin, whose own
    /// rows count them.
    pub fn left_out_of_folders(&self) -> bool {
        self.guarded || self.ranks_contents
    }

    /// "names: hint", saying so when cleanup refuses its data.
    pub fn note(&self) -> String {
        let refused = if self.guarded {
            "; cleanup refuses it"
        } else {
            ""
        };
        format!("{}: {}{refused}", self.names(), self.hint())
    }

    /// Why cleanup refuses a path of this service.
    pub fn refusal(&self) -> String {
        self.refusal_of(&self.names())
    }

    fn refusal_of(&self, names: &str) -> String {
        format!("package-managed data ({names}): {}", self.hint())
    }

    /// The DSM place to manage it: `manage_in` up to its first detail.
    pub fn place(&self) -> &'static str {
        let cut = [" > ", " (", ";", ","]
            .iter()
            .filter_map(|sep| self.manage_in.find(sep))
            .min()
            .unwrap_or(self.manage_in.len());
        &self.manage_in[..cut]
    }

    /// The stable id of the place to manage it (SPEC 4.6, GAP-14).
    pub fn manage_in_id(&self) -> &'static str {
        place_id(self.manage_in)
    }

    /// "managed by X; manage it in Y", marked when unconfirmed.
    pub fn hint(&self) -> String {
        let base = hint(self.package, self.manage_in, self.confirmed);
        match self.explanation {
            Some(explanation) => format!("{base}; {explanation}"),
            None => base,
        }
    }
}

/// "managed by `package`; manage it in `place`", marked when the layout
/// is not `confirmed`.
pub fn hint(package: &str, place: &str, confirmed: bool) -> String {
    format!(
        "managed by {package}; manage it in {place}{}",
        if confirmed {
            ""
        } else {
            " (layout not confirmed on this DSM version)"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_root_service_directories_name_their_package() {
        let drive = service(b"@synologydrive", Depth::VolumeRoot).unwrap();
        assert_eq!(drive.package, "Synology Drive Server");
        assert!(drive.manage_in.contains("Synology Drive Admin Console"));
        assert!(service(b"@synologydrive", Depth::Below).is_none());
    }

    #[test]
    fn active_backup_data_inside_a_share_is_guarded_but_its_database_is_not_listed_as_guarded() {
        let data = service(b"@ActiveBackup", Depth::Below).unwrap();
        assert_eq!(data.code, "active_backup_data");
        assert!(data.guarded);
        let db = service(b"@ActiveBackup", Depth::VolumeRoot).unwrap();
        assert_eq!(db.code, "active_backup");
        assert_eq!(
            service(b"@ActiveBackup-Unknown", Depth::VolumeRoot).map(|s| s.code),
            Some("active_backup_other")
        );
    }

    #[test]
    fn the_active_backup_volume_folder_at_the_volume_root_names_its_package() {
        let volume = service(b"@ActiveBackupVolume", Depth::VolumeRoot).unwrap();
        assert_eq!(volume.code, "active_backup_volume");
        assert!(volume.package.starts_with("Active Backup for Business ("));
        assert_eq!(volume.manage_in, ACTIVE_BACKUP);
        assert!(volume.confirmed && !volume.guarded);
        assert_eq!(
            service_of(b"/v/@ActiveBackupVolume/share/@ActiveBackup", b"/v").map(|s| s.code),
            Some("active_backup_volume")
        );
    }

    #[test]
    fn the_encrypted_active_backup_volume_alias_names_the_same_service() {
        let volume = service(b"@ABM_Encrypted@", Depth::VolumeRoot).unwrap();
        assert_eq!(volume.code, "active_backup_volume");
        assert_eq!(
            service_of(b"/v/@ABM_Encrypted@/data", b"/v").map(|s| s.code),
            Some("active_backup_volume")
        );
    }

    #[test]
    fn saas_backup_databases_at_the_volume_root_name_their_package() {
        let m365 = service(b"@ActiveBackup-Office365", Depth::VolumeRoot).unwrap();
        assert_eq!(m365.code, "active_backup_microsoft_365");
        assert_eq!(m365.package, "Active Backup for Microsoft 365 (database)");
        assert!(m365.confirmed && !m365.guarded);
        let gws = service(b"@ActiveBackup-GSuite", Depth::VolumeRoot).unwrap();
        assert_eq!(gws.code, "active_backup_google_workspace");
        assert_eq!(gws.package, "Active Backup for Google Workspace (database)");
        assert!(gws.confirmed && !gws.guarded);
    }

    #[test]
    fn saas_backup_destination_folders_inside_a_share_are_guarded() {
        let code = |n: &[u8]| package_data(n).map(|s| s.code);
        assert_eq!(
            code(b"ActiveBackupForMicrosoft365"),
            Some("active_backup_microsoft_365_data")
        );
        for name in [
            &b"ActiveBackupForGoogleWorkspace"[..],
            b"ActiveBackupForGSuite",
        ] {
            assert_eq!(code(name), Some("active_backup_google_workspace_data"));
        }
        let data = service(b"ActiveBackupForMicrosoft365", Depth::Below).unwrap();
        assert!(data.confirmed && !data.list_each);
        assert!(
            data.manage_in
                .starts_with("Active Backup for Microsoft 365 ")
        );
        assert!(service(b"ActiveBackupForMicrosoft365", Depth::VolumeRoot).is_none());
    }

    #[test]
    fn hyper_backup_image_caches_at_the_volume_root_are_unconfirmed() {
        for name in [&b"@img_bkp_cache"[..], b"@img_bkp_mount"] {
            let cache = service(name, Depth::VolumeRoot).unwrap();
            assert_eq!(cache.code, "hyper_backup_cache");
            assert_eq!(cache.package, "Hyper Backup (image and cloud cache)");
            assert!(!cache.confirmed && !cache.guarded && cache.list_each);
        }
        assert!(service(b"@img_bkp_cache", Depth::Below).is_none());
    }

    #[test]
    fn hyper_backup_targets_are_matched_by_suffix_at_any_depth() {
        for depth in [Depth::VolumeRoot, Depth::Below] {
            let hb = service(b"nas_1.hbk", depth).unwrap();
            assert_eq!(hb.code, "hyper_backup");
            assert!(hb.guarded);
        }
        assert!(service(b".hbk", Depth::Below).is_none());
        assert!(service(b"notes.hbk.txt", Depth::Below).is_none());
    }

    #[test]
    fn the_surveillance_share_is_guarded_only_at_the_volume_root() {
        assert!(service(b"surveillance", Depth::VolumeRoot).unwrap().guarded);
        assert!(service(b"surveillance", Depth::Below).is_none());
    }

    #[test]
    fn recycle_bins_are_listed_one_by_one_and_metadata_is_summed() {
        let recycle = service(b"#recycle", Depth::Below).unwrap();
        assert!(recycle.list_each && !recycle.guarded);
        let ea = service(b"@eaDir", Depth::Below).unwrap();
        assert!(!ea.list_each);
        assert_eq!(service(b"@eaDir", Depth::VolumeRoot), Some(ea));
    }

    #[test]
    fn services_name_the_entries_they_match() {
        let name = |n: &[u8], d| service(n, d).unwrap().names();
        assert_eq!(name(b"@eaDir", Depth::Below), "@eaDir");
        assert_eq!(name(b"a.hbk", Depth::Below), "*.hbk");
        assert_eq!(
            name(b"@ActiveBackup-Unknown", Depth::VolumeRoot),
            "@ActiveBackup-*"
        );
        assert_eq!(
            name(b"ActiveBackupForGSuite", Depth::Below),
            "ActiveBackupForGoogleWorkspace, ActiveBackupForGSuite"
        );
    }

    #[test]
    fn a_path_belongs_to_its_topmost_service_below_the_mount() {
        let code = |p: &[u8]| service_of(p, b"/v").map(|s| s.code);
        assert_eq!(
            code(b"/v/@synologydrive/@sync/repo"),
            Some("synology_drive")
        );
        assert_eq!(code(b"/v/share/x.hbk/Pool/@eaDir"), Some("hyper_backup"));
        assert_eq!(
            code(b"/v/surveillance/cam"),
            Some("surveillance_recordings")
        );
        assert_eq!(code(b"/v/share/surveillance"), None);
        assert_eq!(code(b"/v/share/file"), None);
        assert_eq!(code(b"/v"), None);
        assert_eq!(code(b"/other/@docker"), None);
    }

    #[test]
    fn package_data_is_guarded_service_data_or_its_marker() {
        let code = |n: &[u8]| package_data(n).map(|s| s.code);
        assert_eq!(code(b"x.hbk"), Some("hyper_backup"));
        assert_eq!(code(b"SynologyHyperBackup.bkpi"), Some("hyper_backup"));
        assert_eq!(code(b"@ActiveBackup"), Some("active_backup_data"));
        assert_eq!(code(b"@eaDir"), None);
        assert_eq!(code(b"#recycle"), None);
        assert_eq!(code(b"surveillance"), None);
    }

    #[test]
    fn unknown_names_have_no_service() {
        assert!(service(b"@SYNO.core.gz", Depth::VolumeRoot).is_none());
        assert!(service(b"photos", Depth::VolumeRoot).is_none());
        assert!(service(b"@ActiveBackup-", Depth::VolumeRoot).is_none());
    }

    #[test]
    fn quota_files_at_an_ext4_volume_root_are_dsm_data() {
        for name in [&b"aquota.user"[..], b"aquota.group", b"synoquota.db"] {
            let quota = service(name, Depth::VolumeRoot).unwrap();
            assert_eq!(quota.code, "dsm_quota");
            assert_eq!(quota.manage_in_id(), "dsm");
            assert!(service(name, Depth::Below).is_none());
        }
        assert_eq!(
            service_of(b"/volume1/aquota.user", b"/volume1").map(|s| s.code),
            Some("dsm_quota")
        );
        assert_eq!(service_of(b"/volume1/share/aquota.user", b"/volume1"), None);
    }

    #[test]
    fn marker_entries_name_the_service_of_their_directory() {
        assert_eq!(
            marked_by(b"@ActiveBackup").map(|s| s.code),
            Some("active_backup_data")
        );
        assert_eq!(
            marked_by(b"SynologyHyperBackup.bkpi").map(|s| s.code),
            Some("hyper_backup")
        );
        assert!(marked_by(b"Pool").is_none());
    }

    #[test]
    fn places_are_the_first_part_of_where_to_manage_the_data() {
        let place = |name: &[u8], depth| service(name, depth).unwrap().place();
        assert_eq!(place(b"@docker", Depth::VolumeRoot), "Container Manager");
        assert_eq!(place(b"@appstore", Depth::VolumeRoot), "Package Center");
        assert_eq!(
            place(b"@ActiveBackup", Depth::Below),
            "Active Backup for Business"
        );
        assert_eq!(
            place(b"@sharesnap", Depth::VolumeRoot),
            "Snapshot Replication"
        );
        assert_eq!(place(b"#recycle", Depth::Below), "Control Panel");
        for s in SERVICES {
            assert!(!s.place().is_empty() && s.place().len() <= 44, "{}", s.code);
        }
    }

    #[test]
    fn hints_say_who_manages_the_data_and_flag_unconfirmed_layouts() {
        let drive = service(b"@synologydrive", Depth::VolumeRoot).unwrap();
        assert!(
            drive
                .hint()
                .starts_with("managed by Synology Drive Server; manage it in ")
        );
        assert!(!drive.hint().contains("not confirmed"));
        let db = service(b"@database", Depth::VolumeRoot).unwrap();
        assert!(
            db.hint()
                .ends_with("(layout not confirmed on this DSM version)")
        );
    }

    #[test]
    fn iscsi_hint_explains_provisioning_and_vmm_image_storage() {
        let iscsi = service(b"@iSCSI", Depth::VolumeRoot).unwrap();
        let hint = iscsi.hint();
        for detail in [
            "UNMAP",
            "allocated up front",
            "Virtual Machine Manager > Image",
        ] {
            assert!(hint.contains(detail), "missing {detail:?} from {hint:?}");
        }
    }

    #[test]
    fn vmm_virtual_disks_are_their_own_service_under_the_lun_folder() {
        let disks = service_within(b"@iSCSI/LUN/VDISK_BLUN1").unwrap();
        assert_eq!(disks.code, "vmm_virtual_disks");
        assert_eq!(disks.package, "Virtual Machine Manager (virtual disks)");
        assert!(disks.confirmed);
        assert_eq!(place_id(disks.manage_in), "virtual_machine_manager");
        let hint = disks.hint();
        for detail in ["Virtual Machine Manager > Virtual Machine", "not inferred"] {
            assert!(hint.contains(detail), "missing {detail:?} from {hint:?}");
        }
    }

    #[test]
    fn only_virtual_disk_names_directly_under_the_lun_folder_are_vmm_disks() {
        for path in [
            &b"@iSCSI/LUN/BLUN"[..],
            b"@iSCSI/LUN/VDISK_",
            b"@iSCSI/other/VDISK_a",
            b"@iSCSI/LUN/BLUN/VDISK_a",
            b"VDISK_a",
            b"@iSCSI",
        ] {
            assert!(service_within(path).is_none(), "{path:?}");
        }
    }

    #[test]
    fn a_path_in_a_virtual_disk_is_still_topmost_in_iscsi_for_the_owner_and_guard_lookups() {
        let found = service_of(b"/v/@iSCSI/LUN/VDISK_a/x", b"/v").unwrap();
        assert_eq!(found.code, "iscsi_luns");
    }

    #[test]
    fn this_app_is_appended_and_guards_only_its_canonical_package_path() {
        assert_eq!(service_id_by_code("vmm_virtual_disks"), 31);
        assert_eq!(service_id(syno_reclaim()), 32);
        assert_eq!(syno_reclaim().manage_in_id(), "syno_reclaim");
        assert!(syno_reclaim().guarded);
        for path in [
            b"/v/@appdata/SynoReclaim".as_slice(),
            b"/v/@appdata/SynoReclaim/state/cache",
        ] {
            assert_eq!(service_of(path, b"/v"), Some(syno_reclaim()));
            assert_eq!(package_data_of(path, b"/v"), Some(syno_reclaim()));
        }
        assert_eq!(
            service_of(b"/v/@appdata/SynoReclaimOther/state", b"/v")
                .unwrap()
                .code,
            "package_data"
        );
        assert_eq!(service_of(b"/v/share/SynoReclaim/state", b"/v"), None);
    }

    #[test]
    fn every_service_has_a_unique_code_and_a_place_to_manage_it() {
        let mut codes: Vec<&str> = SERVICES.iter().map(|s| s.code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), SERVICES.len());
        assert!(SERVICES.iter().all(|s| !s.manage_in.is_empty()));
        for (_, code) in MARKERS {
            assert!(SERVICES.iter().any(|s| s.code == *code), "{code}");
        }
    }

    fn is_snake_case(id: &str) -> bool {
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    #[test]
    fn every_place_id_is_unique_snake_case_and_names_its_texts() {
        let mut ids: Vec<&str> = PLACES.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(count, ids.len(), "duplicate place id");
        let mut texts: Vec<&str> = PLACES
            .iter()
            .flat_map(|p| p.manage_in.iter().copied())
            .collect();
        texts.sort_unstable();
        let count = texts.len();
        texts.dedup();
        assert_eq!(count, texts.len(), "a manage_in text names two places");
        for place in PLACES {
            assert!(is_snake_case(place.id), "{}", place.id);
            assert!(!place.manage_in.is_empty(), "{}", place.id);
            for text in place.manage_in {
                assert_eq!(place_id(text), place.id, "{text:?}");
            }
        }
        assert_eq!(place_id("nowhere"), OTHER_PLACE_ID);
    }

    #[test]
    fn every_service_row_names_a_known_place() {
        for service in SERVICES {
            assert_ne!(service.manage_in_id(), OTHER_PLACE_ID, "{}", service.code);
        }
    }
}
