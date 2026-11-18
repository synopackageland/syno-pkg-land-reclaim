# syno-reclaim CLI Specification

Status: CLI design with two production-readiness reviews (2026-09-27), covering the backref strategy, memory shape, Synology namespaces, cleanup load, test infrastructure, and release staging. Implementation and release gates remain unverified.

## 1. Product goal

Build a fast, robust Synology Btrfs/ext4 CLI that answers the questions that ordinary storage analyzers do not answer well on copy-on-write filesystems:

1. If I delete this file, how much space will actually be freed?
2. If I delete this folder, how much space will actually be freed?
3. If I delete several paths together, how much space will actually be freed?
4. Why is a large file/folder not reclaiming the amount suggested by its logical size?
5. Which data is shared by reflinks, snapshots, hard links, or other Btrfs references?
6. Which paths are the best cleanup candidates by **reclaimable bytes**, not merely logical size?

Support tickets add four volume-level questions, answered by the question-centred commands (sections 5.12–5.18):

7. Where did my volume's used space go, and does it add up to what Storage Manager shows?
8. Which snapshot holds my space, and what would deleting snapshots free?
9. I deleted files; why is the space not back?
10. Who holds the bytes that deleting a path would not free, and where in DSM do I fix that?

The filesystem is always the source of truth. Every cache and report is disposable.

## 2. V1 scope

### Included

- Linux/Synology DSM, Btrfs and ext4 data volumes.
- Rust CLI.
- On-demand scanning. No daemon.
- `BTRFS_IOC_TREE_SEARCH_V2` based metadata scanning.
- File and folder tree reconstruction.
- Hard-link-aware namespace model.
- Reflink/snapshot/shared-extent-aware accounting.
- Point-in-time/best-effort reports for live filesystems.
- Disposable, scope-aware cache with bounded-age generation-assisted reuse.
- Foreground jobs with graceful pause/resume and crash recovery from completed scan units.
- Low-impact scanning by default, with bounded memory, disk use, and concurrency, and low CPU and I/O priority.
- Human-readable and JSON output.
- Targeted scans for the whole NAS, one supported volume, or one path subtree.
- Read-only analysis and deletion simulation.
- Actual cleanup through fixed, packaged cleanup scripts invoked only from a validated cleanup plan.
- Read-only volume reconciliation from `BTRFS_IOC_SPACE_INFO` and `statfs` (display only; section 5.13).
- Reading qgroup counters from the quota tree as a cross-check when quotas are already enabled (section 5.14). Enabling, rescanning, or changing qgroups stays excluded.
- A read-only report of DSM's system partition `/`, and moving a fixed allowlist of DSM's leftovers off it into a shared folder through an immutable plan, then DSM's own log cleaners (section 5.23).

### Explicitly excluded from V1

- Continuous indexing or inotify/fanotify watchers.
- SQLite or any authoritative secondary database.
- Historical/time-series scan storage.
- Full-content duplicate hashing.
- Automatic dedupe/reflink creation.
- qgroup enablement or management.
- Reliance on Synology private Btrfs key type 163.
- A job daemon, automatic scheduler, cross-host coordinator, or mandatory cgroup/systemd service.
- Exact byte-cursor continuation across mutations of a writable Btrfs tree.

## 3. User-facing metric semantics

For ext4, physical footprint is allocated inode space (`st_blocks * 512`),
including directories and symlinks, deduplicated by `(st_dev, st_ino)`. Sparse
logical lengths do not imply allocation. Hard links share one allocation and
retain the existing hard-link holding rules. This is neither Btrfs DATA used
nor physical drive consumption. Extents, inline, pinned, snapshots, reflinks,
and cleaner measurements are unavailable (`null` or absent), never fake zeros.

The CLI must avoid presenting `st_size`, inode `nbytes`, or a sum of file sizes as "space that will be freed".

For every selectable path, expose these concepts:

### Logical size

The apparent file data size. Sparse files, compression, reflinks, and snapshots can make this very different from real on-disk use.

### Physical footprint

The union of physical data extents referenced by the selected path/subtree. A shared extent is counted once in the footprint even if multiple selected files reference it.

This is **not** necessarily reclaimable because references may exist outside the selection.

Here, extent identity is a Btrfs filesystem-logical allocation identity, namespaced by filesystem UUID. `disk_bytenr` is not a raw drive sector address. Report allocated data bytes before RAID/mirror replication, not total physical drive consumption. Preserve file-extent offsets, logical ranges, compression, holes, and preallocation.

Accounting is **extent-granular**. Btrfs frees a data extent only when its last reference is dropped; a file that references only part of an extent (bookend after overwrite, partial clone) still pins the whole `disk_num_bytes`. Therefore footprint and reclaimable values count whole extents, and the part of a pinned extent that no selected or external file range still references is reported separately as `pinned_unreferenced_bytes` (space that deletion may free but that no file "owns"). Deleting a file never frees part of an extent.

### Shared bytes

Physical extent bytes referenced by the selection that are also referenced outside the selection.

Unresolved ownership is reported separately as `unknown_bytes`, not silently classified as proven sharing. Unknown reclaimable values display as `unknown`/JSON `null`, not as a confident zero. A resolved portion may be reported as a conservative estimate with explicit coverage, but it is not a guaranteed lower bound on future `df` changes on a live NAS.

### Reclaimable bytes

The estimated unique physical data bytes that become unreferenced if all selected directory entries/subvolumes are removed.

This is the primary product metric.

The number is an estimate of reclaimable file data. It is not a promise that `df` will immediately increase by exactly the same amount because Btrfs metadata, block-group allocation, delayed references, compression, allocator behavior, and concurrent writes can affect the observed filesystem free-space delta.

The estimate assumes the data is gone for good (a cleanup moved to the Recycle Bin frees it once the Recycle Bin is emptied) and eventual release of remaining open file handles. Moving data to a recycle bin alone frees nothing. Open-but-unlinked files and mapped files can retain data after pathname deletion. V1 explains these limits without scanning every process continuously or claiming application-level duplicate detection inside backup/VM/container formats.

### External blockers

When possible, explain why selected data is not reclaimable:

- referenced by a snapshot;
- referenced by another live file/reflink;
- inode still has a hard link outside the selection;
- referenced from another subvolume;
- referenced through a shared (snapshot-shared) tree block whose owner is not resolved;
- held by a deleted subvolume/snapshot still awaiting the Btrfs cleaner (space frees later without further action);
- held by an unlinked-but-open or orphan inode (no directory entry, extents still referenced);
- unknown/unresolved Btrfs reference.

When accounting cannot prove that an extent becomes unreferenced, V1 must be conservative: do not count it as reclaimable.

## 4. Important filesystem semantics

### ext4 capabilities

Volumes and report headers identify `filesystem: btrfs|ext4` and capabilities
`scan`, `snapshots`, `reflinks`, `cleanup`, `cleanup_unavailable_reason`. ext4
has no subvolumes, snapshots or reflinks. A DSM shared folder is identified
from the trusted root-owned `synoshare --enummnt MOUNT` inventory and a pinned,
no-follow root fd; an ordinary directory is never promoted to a share. Recycle
Bins are classified only beneath those trusted share roots. Report targets use
an inode device identity and `tree: null`; internal namespace IDs are not
presented as Btrfs subvolume IDs or generations.

ext4 cleanup is available only after a real `RENAME_NOREPLACE` probe in private
state on the same volume. Unsupported kernels (including tested DSM kernel
3.10) expose `rename_noreplace_unavailable`; missing/untrusted state exposes
`cleanup_state_unavailable`, and other probe failures expose
`cleanup_probe_failed`. No link/unlink or replacement fallback is permitted.

### 4.1 Paths and inodes are different objects

The model must separate directory entries from inodes.

A hard-linked file may have multiple names pointing to one inode. Deleting one pathname does not reclaim the inode's extents while another hard link remains.

Conceptually:

```text
DirEntry(path/name) -> Inode -> Extent references -> Physical extents
```

### 4.2 Reflinks are not duplicates in the cleanup sense

Two files may each report 100 GiB while sharing almost all physical extents. Deleting one can reclaim nearly zero bytes.

Do not label shared reflink data as wasted duplicate data.

### 4.3 Snapshots are external references

Deleting a live file can reclaim little or nothing when snapshots retain its extents. The report should explain this instead of merely showing a smaller number.

### 4.4 Compression and sparse files

Logical bytes and physical data bytes must remain separate. Physical extent accounting must use the physical allocation identity/length rather than logical file length.

### 4.5 Inline/metadata-only data

V1 focuses on reclaimable file-data extents. Inline data and Btrfs metadata-space reclamation should not be overclaimed as immediately reclaimable user-data bytes.

Metadata and system space may be *displayed* (from `BTRFS_IOC_SPACE_INFO`) so that a volume view adds up to the filesystem's used space (section 5.13). Displayed metadata is never counted as reclaimable, never attributed to a path, and never included in any `reclaimable` or `held` value.

### 4.6 Synology namespace specifics

Indexed paths inside the volume but outside known shares and DSM service paths are
classified as `system_other` (Other volume data) in overview, rather than unknown
ownership. Unrecognised volume-root `@…` paths remain other DSM service data.
The mixed volume root is not a uniform service owner: an unindexed inode or an
unresolved shared-leaf owner there remains unknown. Root name `@syno` alone is
not evidence that all its data belongs to DSM. This display classification adds
no reclaimable or held estimate and changes no cleanup certification rule.

When a shared-data backref reaches the mixed volume root, overview may resolve
its individual owners through the referenced metadata leaf: verify the leaf's
filesystem identity, logical address and extent-tree generation, require exactly
the declared reference count and matching allocation size, then classify each
inode through that root's existing path index. Every shared leaf keeps its own
root anchors; roots from a different leaf are not owners of its inodes. Unreadable,
rewritten, over-budget or mismatching leaves and unindexed inodes stay unknown.
This uses the bounded leaf-read budget and does not change extent reclaimability.

Raw Btrfs metadata is not the same as the user-visible DSM namespace. The following must be detected and handled explicitly; the concrete names and layouts are initial assumptions to confirm per DSM build with fixtures, not verified facts:

- **Shared folders are subvolumes.** A share root is a subvolume boundary; its snapshots (Snapshot Replication, e.g. under `@sharesnap`, exposed through `#snapshot`) are separate read-only roots that participate in reference accounting but are never generic cleanup targets.
- **Encrypted shared folders (eCryptfs).** Btrfs stores ciphertext names (`ECRYPTFS_FNEK_ENCRYPTED.*`) and lower-file sizes that include the eCryptfs header. Raw DIR_INDEX names cannot be mapped to user paths, and logical sizes differ. V1 must detect eCryptfs lower directories and either (a) map names through the mounted upper filesystem with readdir/`fstatat`, joining by lower inode identity, or (b) report the share as `unsupported_encrypted` coverage. Never display ciphertext names as user paths, and never let cleanup resolve upper paths through raw lower names. Locked encrypted shares are coverage gaps, never unlocked. DSM volume-level encryption (block layer) is transparent and needs no special handling.
- **Recycle bin and service directories.** `#recycle` contents are ordinary, often high-value cleanup candidates and are surfaced as such: rankings show each share's `#recycle` as a normal entry, and the `top` service summary lists each one as its own line. Reclaim's cleanup moves items into a share's `#recycle` and never removes anything from it: `#recycle` and everything in it are refused as cleanup targets (section 5.7), and DSM empties it. `@eaDir` (indexing/thumbnail metadata), `@`-prefixed volume-root directories (`@appstore`, `@docker`, `@database`, `@tmp`, …), and `#snapshot` are DSM-managed; they are counted in accounting and protected from generic cleanup. In `top` rankings each known service appears as one aggregated row by default; `@eaDir` already included in folder rows is summary information only, while uncovered metadata paths keep a separate row. Service detail rows (individual directories and files) are hidden unless `--include-system` is given (section 5.4). Volume JSON rankings retain nested service rows with a lossless `within` path when their entire full footprint is already represented by another row in the same answer; the list remains visible, while treemaps and full-footprint sums exclude those rows. Path containment alone is insufficient: VMM's footprint is subtracted from iSCSI and keeps a separate tile.
- **Known service paths.** One data table (`src/dsm.rs`, the only place it lives) names each known service path, the package that manages it, where in DSM to manage it, and any fixed explanation. Its rules, first match wins:

  | Match | Where | Package | Notes |
  |---|---|---|---|
  | `@synologydrive` | volume root | Synology Drive Server | |
  | `@SynologyDriveShareSync` | volume root | Synology Drive ShareSync | |
  | `@ActiveBackup` | volume root | Active Backup for Business (database and settings) | |
  | `@ActiveBackup` | below the volume root | Active Backup for Business (backup data) | cleanup refuses it |
  | `@ActiveBackupVolume` | volume root | Active Backup for Business (volume data) | |
  | `ActiveBackupForMicrosoft365` | below the volume root | Active Backup for Microsoft 365 (backup data) | cleanup refuses it |
  | `ActiveBackupForGoogleWorkspace`, `ActiveBackupForGSuite` | below the volume root | Active Backup for Google Workspace (backup data) | cleanup refuses it |
  | `@ActiveBackup-Office365` | volume root | Active Backup for Microsoft 365 (database) | |
  | `@ActiveBackup-GSuite` | volume root | Active Backup for Google Workspace (database) | |
  | `@ActiveBackup-*` | anywhere | an Active Backup package | not confirmed |
  | `@docker` | volume root | Container Manager | |
  | `@appstore`, `@appdata`, `@apphome`, `@appconf`, `@appshare`, `@apptemp` | volume root | installed packages (Package Center) | |
  | `@database` | volume root | DSM and package databases | not confirmed |
  | `@sharesnap` | volume root | Snapshot Replication (shared folder snapshots) | |
  | `#snapshot` | anywhere | Snapshot Replication (visible snapshot folder) | not confirmed |
  | `@SnapshotReplication` | volume root | Snapshot Replication (package data) | |
  | `@surveillance` | volume root | Surveillance Station (package data) | |
  | `surveillance` | volume root (the share it creates) | Surveillance Station (recordings) | cleanup refuses it |
  | `@iSCSI` | volume root | SAN Manager (iSCSI LUNs) | Thin-provisioned LUNs return deleted space only when space reclamation is enabled and the initiator sends UNMAP; thick-provisioned LUNs have their configured capacity allocated up front. VMM image storage is shown in Virtual Machine Manager > Image. |
  | `VDISK_*` | directly in `@iSCSI/LUN` | Virtual Machine Manager (virtual disks) | Managed in Virtual Machine Manager. Their storage is checked in Virtual Machine Manager > Virtual Machine, not inferred from the Btrfs path. |
  | `@cloudsync`, `@USBCopy` | volume root | Cloud Sync, USB Copy | |
  | `@tmp` | volume root | DSM temporary files | not confirmed |
  | `#recycle` | anywhere | shared folder Recycle Bin | listed per share |
  | `@eaDir` | anywhere | DSM indexing (thumbnails and file metadata) | not confirmed |
  | `*.hbk` | anywhere | Hyper Backup (backup data) | cleanup refuses it |
  | `@img_bkp_cache`, `@img_bkp_mount` | volume root | Hyper Backup (image and cloud cache) | not confirmed |
  | `SynoReclaim` | directly in `@appdata`, plus the resolved state directory by `(fsid, subvolume, inode)` | Syno Reclaim (this app) | `syno_reclaim`; Scan history > Clear cache; cleanup refuses it |
  | `aquota.user`, `aquota.group`, `synoquota.db` | volume root | DSM quota files (an ext4 volume's user, group and shared folder quotas) | `dsm_quota`; managed in DSM, so cleanup refuses them like any service path |

  "Not confirmed" layouts are marked as such in output: seen on DSM 7.4 but not documented by Synology, or not seen yet. Other `@…` volume-root entries (core dumps, caches of other packages) have no row and show as "other" service data. Surveillance Station recordings assigned to another shared folder cannot be recognized by name.

  Read-only checks on 2026-09-29 confirmed `/volume1/@iSCSI/LUN/BLUN` and `BLUN_THICK` on one test NAS (DS420+, DSM 7.4 build 90069). On another (DS918+, DSM 7.4 build 90069), checks also found `VDISK_BLUN*` and `VDISK_ULUN` under `/volume1/@iSCSI/LUN` (they are the `vmm_virtual_disks` service: matched by their parent folder, last in the table because stored reports index it, and taking their own bytes out of the `@iSCSI` line in the service summary; owner classification and `service_of` still see them as part of `@iSCSI`), plus `/volume1/@appstore/Virtualization` and `/volume1/@appdata/Virtualization`. The DS420+ returned no VMM images or repositories. These names identify service data, but the selected VMM storage must be checked in Virtual Machine Manager; do not infer it from a Btrfs path. Synology documents that thin LUN space reclamation depends on initiator UNMAP, while thick LUN capacity is pre-allocated ([SAN Manager LUN help](https://kb.synology.com/DSM/help/ScsiTarget/lun?version=7)); VMM shows image storage in its Image page ([VMM Image help](https://kb.synology.com/en-us/DSM/help/Virtualization/image?version=7)).

  The `syno_reclaim` row is appended after all existing service rows, preserving stored
  report indices. The package handler resolves `/var/packages/SynoReclaim/var/state`
  through the real package var path (`@appdata/SynoReclaim/state`); CLI `--state-dir`
  is recognized by filesystem/subvolume/inode identity, including symlink resolution.
  Its scan cache, reports, sidecars and jobs belong to this app. The nested package
  subtree is removed from the generic `package_data` summary so it is counted once.
  Files appearing after the namespace read require inode ancestry evidence (8.1),
  not a guessed path. Overview includes the service in largest packages when eligible.

  **Stable place ids.** Every JSON document that names a DSM place by the service table's English `manage_in` text also names it by a stable snake_case `manage_in_id`, so a GUI localizes the id instead of parsing the text. An id identifies a DSM place, not a service: several services and several `manage_in` texts share one id, and the ids are unique. `src/dsm.rs` (`PLACES`) is the only source; a test checks that every service-table row has one and that ids are unique. When one `manage_in` names more than one place (deleting snapshot roots of more than one service together) the field is `manage_in_ids`, a list.

  | `manage_in_id` | DSM place |
  |---|---|
  | `syno_reclaim` | Syno Reclaim > Scan history > Clear cache |
  | `drive_admin_console` | Synology Drive Admin Console |
  | `drive_sharesync` | Synology Drive ShareSync |
  | `active_backup_business` | Active Backup for Business |
  | `active_backup_microsoft_365` | Active Backup for Microsoft 365 |
  | `active_backup_google_workspace` | Active Backup for Google Workspace |
  | `active_backup_package` | the Active Backup package that created the data |
  | `container_manager` | Container Manager |
  | `package_center` | Package Center |
  | `owning_package` | the package that owns the data |
  | `snapshot_replication` | Snapshot Replication |
  | `surveillance_station` | Surveillance Station |
  | `san_manager` | SAN Manager |
  | `virtual_machine_manager` | Virtual Machine Manager |
  | `cloud_sync` | Cloud Sync |
  | `usb_copy` | USB Copy |
  | `dsm` | DSM itself |
  | `shared_folder_recycle_bin` | Control Panel > Shared Folder |
  | `owning_folder` | the folder the data belongs to |
  | `hyper_backup` | Hyper Backup or Hyper Backup Vault |
  | `file_station` | File Station |
  | `btrfs_cleanup` | the Btrfs background cleaner |
- **Background data movers.** Btrfs balance/relocation, defragmentation (breaks reflinks), DSM data deduplication, snapshot creation/deletion, and Snapshot Replication receive change extent identities or sharing while a report exists. See section 8 for detection.

## 5. CLI surface

ext4 supports `volumes`, `scan`, `overview`, `top`, `inspect`, `shared`, and
`simulate-delete` estimates through the existing report/query paths. Overview
separates share live allocations, Recycle Bins, cross-folder hard links, DSM
services and other allocations. `statvfs` used/free is a separate live reading;
its difference from inode allocations is unattributed filesystem overhead,
not an additional reclaimable estimate. Btrfs-specific snapshot/reflink/cleaner
panels are hidden. ext4 cleanup is gated by the verified per-volume atomic rename capability and private state; see §4 and §5.7.
`rootfs plan --to` and `rootfs apply` accept a shared folder on an ext4 data volume as they do on Btrfs: the move is a plain copy, sync and unlink, and only the share quota check reads Btrfs qgroups (an ext4 share has none); see 5.23.

Binary name: `synoreclaim`

### 5.1 Target model

Every command that operates on filesystem content uses the same target model.

Targets can be one or more of:

- `system`: all supported local Btrfs data volumes on the NAS;
- a volume mount such as `/volume1`;
- a shared folder, directory, or file path such as `/volume1/share/Movies`.

Multiple path targets are first-class. The CLI must accept repeated positional paths in one invocation and treat them as one target set when the command semantics require combined accounting.

The CLI should auto-resolve a normal path target and expose `--scope` only when the caller wants explicit validation/override.

Examples:

```bash
synoreclaim scan system
synoreclaim scan /volume1
synoreclaim scan /volume1/share
synoreclaim scan /volume1/share/Movies
synoreclaim scan /volume1/share/Movies /volume1/homes/alice /volume1/backup/old

synoreclaim scan /volume1 --scope volume
synoreclaim scan /volume1/share/Movies --scope path
```

Rules:

- `system` scans each Btrfs filesystem independently; physical extents are never shared across different filesystems.
- a volume target builds a complete report for that volume;
- a path target builds/report-ranks only that subtree, while still querying enough global Btrfs reference metadata to calculate external sharing/reclaimability correctly;
- multiple path targets may span multiple subvolumes on the same filesystem; their selected namespaces are materialized independently but accounting is combined where meaningful;
- targets spanning different filesystems/volumes are scanned independently because physical extents cannot be shared across filesystems; combined output may sum the final per-filesystem results but must never merge physical extent identities across filesystems;
- a path scan must not silently claim exact reclaimable values from subtree-only information when external references cannot be resolved;
- non-Btrfs targets fail clearly in V1.

Common report behavior:

- analysis commands (`top`, `inspect`, `simulate-delete`, `shared`, `cleanup plan`) should accept multiple path targets where applicable and automatically use the newest compatible cached report;
- all path-set commands support `--from-file <path>` (one literal path per line) and `--null` for NUL-delimited input, including `--from-file -` for stdin; no glob expansion, trimming, shell evaluation, or comment interpretation is performed by the program;
- report answer freshness is separate from cache-unit reuse (section 12). An analysis command answers from the newest complete report that covers the requested target, regardless of age, always printing its capture window; it warns when older than `--max-age` (initial default 24h). It does not silently rescan a large target minutes after a multi-hour scan merely because the 5-minute unit-reuse window expired;
- when no covering report exists, an implicit scan is allowed only for file/directory path targets; `system` and volume targets require an explicit `scan` or `--refresh`, so a quick `top /volume1` never becomes an unannounced multi-hour job. Noninteractive implicit scans print the job ID to stderr like `scan`;
- a `system` or volume target without a covering report gets a hint (`scan_hints` in JSON, a `hint:` line in human output): the exact `synoreclaim scan` command to run (with `--state-dir` when not the default) and, from the scan history (section 22.4), how long the last completed whole-volume scan of that filesystem took and how many entries it read, such as "its last full scan took ~15 min". With no recorded scan it says so and gives no estimate;
- `top` and `recycle` take `--report <JOB_ID>`: answer from the report of that scan job (a `history` job ID) instead of the newest covering one, never scanning; a target that report does not cover answers `no_report`. The UI uses it to let the user switch reports;
- `--refresh` forces a fresh scan before answering;
- `--no-scan` reads an existing structurally valid report without refreshing data; stale age/coverage remain visible and this flag cannot bypass cleanup preflight;
- every answer must show/report the scan timestamp so callers know the estimate is point-in-time.

### 5.2 `volumes`

```bash
synoreclaim volumes
synoreclaim volumes --json
```

List supported local Btrfs volumes and basic state:

- mount path;
- filesystem UUID;
- total/used/free filesystem space where available;
- the volume's shared-folder roots (`shares`): each writable direct child of the volume's mounted subvolume (on DSM `/volume1` is the `@syno` subvolume, so shares are its children) with a user name (not a `@…`/`#…` DSM name), with its name, path, subvolume id, and the physical bytes of its live namespace from the newest volume report's overview when one exists. Listing shares reads the root tree and existing report headers only; it never starts a scan, so `size_bytes` is omitted for a volume that has never been scanned;
- cache/report state: readable `reports`, and `incompatible_reports`, the reports that cannot be read (damaged, or written by another report format, typically after an upgrade). A non-zero count adds the volume note `report_incompatible`: the volume looks unscanned until it is scanned again, and nothing migrates the old reports;
- last scan time;
- whether raw Btrfs metadata access is available;
- subvolumes deleted but not yet freed by the Btrfs cleaner (`pending_deletion`): a count, and bytes `null`. The bytes are not derivable cheaply: a root item counts only its tree blocks, and qgroup numbers are neither always enabled nor trusted during deletion. The output says that this space will be freed later without any action.
- the system partition (`/`; on DSM md0, ext4), listed apart from the data volumes. Below 5% available it warns `system_partition_nearly_full`: that partition is DSM's own, not a data volume, and outside what `scan` and `cleanup` read or change. The warning points at `synoreclaim rootfs`, which shows what fills it, and `rootfs plan --to <folder in a shared folder>` (5.23), which plans moving DSM's leftovers off it.

This is the normal discovery entry point before choosing a scan target.

### 5.3 `scan`

```bash
synoreclaim scan system
synoreclaim scan /volume1
synoreclaim scan /volume1/share/Movies
synoreclaim scan /volume1/share/Movies /volume1/homes/alice
synoreclaim scan /volume1 --refresh
synoreclaim scan /volume1 --no-cache
synoreclaim scan /volume1 --json
```

Purpose:

- discover the Btrfs filesystem and subvolumes;
- reuse unchanged subvolume caches;
- rescan changed/missing/corrupt caches;
- rebuild derived global extent accounting as needed;
- write one latest immutable report;
- print scan/cache/consistency summary.

Expected summary fields:

- filesystem UUID;
- scan timestamp;
- subvolumes discovered/scanned/reused;
- namespace entries;
- inode count;
- physical extent count;
- changed-during-scan subvolumes;
- cache bytes;
- elapsed time.

Default scan behavior is best-effort on a live filesystem. It must not freeze or sync the filesystem.

For path targets, the user-visible namespace/materialized report should be scoped to the selected path set. The implementation may inspect filesystem-global extent/back-reference metadata as needed for correctness without building unrelated directory trees.

Detached scans (for a UI or other caller that must not wait for the scan):

```bash
synoreclaim scan /volume1 --detach                        # launcher self
synoreclaim scan /volume1 --detach=none                   # an external launcher runs it
synoreclaim scan --job-id JOB_ID [--json] [--quiet]       # what the launcher runs
```

- `--detach[=self|none]` resolves the targets and probes the writer locks, then queues the scan as a job (state `queued`, with its filesystems) under the given targets and limits. It prints one JSON line and flushes it before doing anything else: `command: "detach"`, `job_id`, `launcher`, `run` (the argv that runs the job: this binary's `scan --job-id ID --state-dir DIR`), `result` (`jobs/<id>/result.json`) and `log`. It exits 0 without waiting, so a caller that reads just one line gets the job ID. A target that does not resolve refuses the whole detach (`completion: "failed"`, exit 1). If every filesystem is locked by another writer, the detach is refused as `busy` with the lock holder (exit 4). In both cases no job is kept and `job_id` is null.
- `--detach` (`self`) runs `run` itself, in a new session (`setsid`) with stdin from `/dev/null` and stdout and stderr appended to `jobs/<id>/output.log` (0600, `log`), so the scan outlives the caller and its terminal. A new session does not survive a service manager that kills the caller's whole cgroup (DSM's `synoscgi.service` uses `KillMode=control-group`). A package should therefore use `--detach=none` and run `run` under its own unit, e.g. an instance of its own template unit (DSM has no `systemd-run`). synoreclaim does not do that itself. If the spawn fails, the job ends `failed` with reason `launch_failed`.
- `scan --job-id ID` runs a queued job in the foreground with the targets and limits it was queued with; target, scope and limit flags are rejected. A job that is not queued is refused: `not_queued`, `finished`, `busy` (exit 4, it is running) or `job_incompatible` (exit 4, queued by another binary). Unlike a plain scan, a launch that finds every filesystem busy ends the job `failed` with reason `busy` instead of discarding it, because its ID was already handed out.
- Every job run writes `jobs/<id>/result.json` and `jobs --json` `result` (section 22.1). A caller polls `jobs --json` for `state` and `result`, and uses `pause`/`cancel` as for any job; cancelling a queued job ends it before it starts.

### 5.4 `top`

The informational `@eaDir` line stays outside Packages' size ranking with its
included-in-folders note. Overview's `system_other` does not add another top row:
those indexed paths are already ordinary ranking entries. Report format 4 remains
compatible; new attribution requires a fresh scan, since old reports do not retain
per-extent inode ownership. Rebuilding a display cache cannot reconstruct it.

Minor namespace churn (display only). `coverage.unreadable_roots` counts requested roots the scan could not read; it is absent on older reports (unknown, not zero). When the only coverage gap is `dangling_entries > 0`, every namespace-uncertain tree has a `subvolume_generation_changed` change reason (the scan saw the tree change), `unreadable_roots` is 0, and no other gap applies (encrypted entries, placeholders, cycles, unsupported locations, missing roots, unavailable reference accounting, a coverage-gap unknown), the display evidence `attribution.namespace_churn.dangling_entries` carries the count. It is absent otherwise, and on older answers, which stay conservative. `attribution.coverage_gaps` stays true and cleanup certification, Review/apply and the stored completion are unchanged: namespace-uncertain trees still refuse certification. The UI shows a muted line instead of the partial banner when the count is at most 10. The UI answer cache format is 6.

The `syno_reclaim` summary is a summed service row. On a volume query, its size
uses the measured once-only Overview service bucket, which includes state files
created after the namespace read and before the extent pass reached their allocations.
It never adds those bytes to the old namespace's folder metrics. Full-metric top
rows retain the known namespace partitions and put measured growth in an unplaced
service partition: the old folder row cannot claim it already includes those new
bytes. The Packages summary still has one summed row. Its generic cleanup
estimate is unavailable (`metrics.reclaimable: null`, resolved value 0); the UI reads
`cache.status` separately for the clearable scan-cache size. Reports and job history
are retained by `cache clear`. Writes after the extent measurement, including final
report publication, are observed by the next scan, not predicted into this report.
Display summary/index/UI-cache formats 4/7/5 and unit-cache format 4 invalidate old
derived answers; immutable report format 4 stays readable. Old unknown inode bytes
cannot be retroactively identified from an old report; a fresh scan supplies the proof.

```bash
synoreclaim top /volume1
synoreclaim top /volume1/share
synoreclaim top /volume1/share/Movies /volume1/homes/alice
synoreclaim top /volume1/share --by reclaimable --limit 50
synoreclaim top /volume1/share --by logical
synoreclaim top /volume1/share --by physical
synoreclaim top /volume1/share --by shared
synoreclaim top /volume1/share --kind files
synoreclaim top /volume1/share --kind dirs
synoreclaim top /volume1/share --json
```

Default sort is `reclaimable`.

Human output should prioritize:

```text
PATH                              SIZE       CAN FREE     SHARED
/volume1/share/Movies             420 GB     395 GB       5 GB
/volume1/share/OldBackups         1.8 TB     260 GB       1.5 TB
```

Internal/system subvolumes may be hidden from the default presentation but must still participate in reference accounting. Add `--include-system` for visibility.

Below the ranking, `top` sums the DSM service and hidden data below each target (JSON `services`): one line per known service path of section 4.6, per share `#recycle`, and for other `@…` volume-root entries, and summed lines for `@eaDir`, `*.hbk` folders, Active Backup data and `#snapshot` views. Each line shows physical, can free, retained and unknown bytes, and the snapshots below it that were not read. Each path counts in its topmost service only; a line's values are the sum of each path deleted alone. One hint per service says which package manages it and where to manage it in DSM. The summary itself is informational and is never added to folder totals. Service rows below add each service to the ranking once, except `@eaDir` paths already included in eligible folder rows: their summed summary stays intact with `included_in_rows: true`. If a summary mixes covered and uncovered metadata paths, only the uncovered paths form a service ranking row, with their own metrics, holders and path list. `included_in_rows: false` means none of that line is covered. The field is absent in older answers. Packages shows the full metadata summary separately with an “included in folders” note; it does not rank or add it to package sizes. Overview uses mutually exclusive per-extent parts, not this summary, and counts metadata once in its service part.

#### Service rows, `HELD BY`, `WHERE TO FIX`

This replaces the rule that hides internal/system data from the default ranking (the `--include-system` paragraph above). The service summary stays as it is. DSM service data is the most common root cause in support tickets, so hiding it by default hides the answer. `top` on a volume or share therefore always shows each known DSM service as **one aggregated row**, ranked with the other rows by the chosen sort key. `--include-system` no longer decides whether service data is visible; it decides only whether the *detail rows* below a service (its individual directories and files) are listed.

```text
$ synoreclaim top /volume1 --limit 8
PATH                                   SIZE      CAN FREE   HELD BY               WHERE TO FIX
/volume1/photos/2019-raw               96 GB      96 GB     —                     cleanup
[DSM] Synology Drive (@synologydrive)  905 GB     812 GB*   snapshot 93 GB        Drive Admin Console
/volume1/photos/#recycle               188 GB     186 GB    hard link 2 GB        Recycle Bin
/volume1/backup/vm                     210 GB      12 GB    snapshot 198 GB       Snapshot Replication
[DSM] Active Backup for Business       512 GB     301 GB*   reflink 211 GB        Active Backup for Business
/volume1/media/Movies                  420 GB       5 GB    reflink 415 GB        shared (see shared)
/volume1/homes/user-07/old.iso          4 GB        0 B     pending cleaner 4 GB   wait
/volume1/share/locked                    —        unknown   unknown               doctor
* frees only through the managing package; synoreclaim cleanup refuses service data.
report 2026-09-28 10:02–10:17 (2h ago), observed-stable
```

- **Service rows.** One row per entry of the service table (section 4.6) found on the volume (`@synologydrive`, `@ActiveBackup*`, ABB destination folders, Hyper Backup vaults, `@docker`, `@appstore`, …; section 4.6), labelled `[DSM] <package> (<path>)`. Values are computed exactly like a directory row (sections 10–11) over all of that service's paths. `CAN FREE` shows the section 10 value with a `*` meaning the bytes can be freed only through the package; the JSON row has `cleanup_eligible: false`. A path target inside a service lists that service's detail rows without `--include-system`, because the user asked for them. When present, the fixed `explanation` from the service table is included in JSON and shown with the service hint; path rows expose it under `where_to_fix.explanation` and print it with `--explain`.
- **`#recycle`** rows appear by default per share (section 4.6), with `WHERE TO FIX` = Recycle Bin.
- **`HELD BY`** names who holds the bytes of the row that deleting the row would *not* free (`SIZE − CAN FREE`). Classes, from the section 3 blockers: `snapshot` (snapshot, `shared_tree`), `recycle bin` (only other references are inside a `#recycle`), `hard link`, `reflink` (live reflink or other subvolume), `pending cleaner` (`pending_subvolume_deletion`), `open file` (`orphan_inode`), `unknown`. Human output shows the largest class and its bytes, plus `+N` when other classes hold at least 10% of the retained bytes; `—` when nothing is retained. Unknown bytes are never merged into another class.
- **`WHERE TO FIX`** is a short fixed token, one per row: for service rows the service table's "manage in" place; otherwise derived from the largest share of the row's bytes: `cleanup` (mostly reclaimable), `Snapshot Replication`, `Recycle Bin`, `shared (see shared)` (reflink), `inspect --explain` (hard link), `wait` (pending cleaner), `why-not-freed` (open file), `doctor` (unknown/coverage gap). `--explain` prints the full service-table hint and the synoreclaim command under each row.
- `--by held` sorts by retained bytes. `--columns` may drop `HELD BY`/`WHERE TO FIX` for narrow terminals; JSON always has them.

JSON (additive fields per row):

```json
{ "path": {"display": "/volume1/backup/vm"}, "kind": "dir",
  "logical_bytes": 0, "physical_bytes": 0, "reclaimable_bytes": 0, "shared_bytes": 0,
  "service": null, "cleanup_eligible": true,
  "held_by": [ {"class": "snapshot", "bytes": 0}, {"class": "unknown", "bytes": 0} ],
  "where_to_fix": {"token": "snapshot_replication", "manage_in": "Snapshot Replication > Snapshots",
                   "command": ["synoreclaim", "snapshots", "/volume1/backup"]} }
```

Service rows carry `"kind": "service"`, `"service": {"id": "synology_drive", "managed_by": "Synology Drive", "manage_in": "…", "paths": [...]}` and `"cleanup_eligible": false`.

As built (2026-09-28):

- Rows are the `entries` of each result. A service row's `path` is its first path; `service` also has `names`, `confirmed` and `path_count`, and `paths` lists at most the first 20. `where_to_fix.command` for `snapshot_replication` is `snapshots` on the share holding the row, for `recycle_bin` is `recycle` on the share holding the row, and for `why_not_freed` is `why-not-freed <volume> --processes`; it is `null` for `wait` and `package`, and for a path that is not text.
- A service row's values are the sums of each of its ranked paths deleted alone (for a mixed `@eaDir` summary, only the uncovered subset), not one calculation over all its paths: `CAN FREE` is a lower bound when its paths share extents with each other, and `SIZE`/`PHYSICAL` count such a footprint once per path. Checked by the delete-and-measure oracle on paths that share nothing.
- A file's retained bytes are all in one class, by priority `snapshot` > `pending cleaner` > `open file` > `hard link` > `reflink`; a directory's retained bytes are split per extent (six u64 per directory in the report since format 3; unknown bytes come from the unknown metric). `recycle bin` holds bytes whose other references are all inside a `#recycle` within the report's scope.
- Overview's path attribution (including verified shared leaves, section 4.6) is independent of these reclaimability verdicts. Attributing an extent to `system_other` does not remove a row's unresolved-reference or shared-tree blocker, and does not make it reclaimable. Report sidecars retain parity with the checked top stream; existing reports keep their recorded attribution until an explicitly requested new scan.
- A folder holding a package marker (`@ActiveBackup`, `SynologyHyperBackup.bkpi`; section 5.7) is itself the path of the marked service, in the service rows and in the service summary: its row adds up everything the folder holds, a `#recycle` inside it included (cleanup refuses that as the package's data too), and the folder never ranks as a row of its own, so an Active Backup destination share is listed once. A ranked folder that holds `@eaDir` further down keeps those bytes in its size and can-free values, because deleting the folder also deletes its metadata (section 5.7 F8). The full `@eaDir` summary is information, marked `included_in_rows: true`, and those covered paths are not added as another service ranking row. `@eaDir` directly below the query target (share or volume root included), with no eligible folder row holding it, keeps a separate row. Coverage follows the requested kind and shared filter before `--limit`, so truncation does not promote omitted folders' metadata back into the ranking. With `--include-system`, a visible metadata entry covers itself; the aggregate does not duplicate it. A target inside `@eaDir` lists its detail rows normally. `shared` keeps its existing folder values and adds no service summary rows.
- A ranked folder holding deeper package-managed data (section 5.7: a guarded service path such as a `*.hbk`, a folder holding a marker, or a nested subvolume that is package data) ranks and shows its values without that data, which its service row counts (report format 4 keeps these values per directory above package data). A footprint shared with package data counts in the folder only where a holder outside package data reaches it; an extent shared across a subvolume boundary with package data counts as package data. A `#recycle` further down is left out the same way, since it ranks as a row of its own; a `#recycle` inside another one or inside package data lies in that row. A human note names the physical bytes left out and the row counting them: the service row for package data, the `#recycle` row for Recycle Bin data. In JSON, `metrics` stays the full values and `without_package_data` holds the values shown; `held_by` stays that of the full values, while the `HELD BY` column shows the values without.
- `top` and `shared` read their report as a stream, record by record, and answer ranking, service summary, package overlaps and package roots in one walk, so their memory does not grow with the report. So do `inspect`, `simulate-delete`, `recycle`, `overview` and `support-bundle`, each in one walk; `why-not-freed` answers from the header and reads the records only to check the checksum. The others check the structure and checksum to the report's end before their answer stands.
- **Full-footprint volume JSON inclusion (2026-10-01).** Consumer metrics remain unchanged: JSON / WebAPI `top.list` and Explore use full folder `metrics`; CLI human ranking uses `without_package_data` when present. Volume JSON entries may carry `within: {path, path_hex?}` naming another row in that same answer whose displayed full footprint includes all their bytes. Included rows retain their full size, can-free and management information in the list, but never add to treemap area or full-footprint sums. A Recycle Bin under a visible share is included for JSON; it is not included in the human CLI's already-disjoint subset metrics. VMM / iSCSI stays partitioned: a subtracted disk is not included merely because its path is nested. Eligibility and service coverage partitions are established before the limit; inclusion is attached only if the covering row survives in the final answer. Summed service ranking rows may be partitioned by eligible containing folder for volume JSON so mixed covered / uncovered paths and omitted parents retain their own footprints; the `services` summary and human CLI aggregation stay unchanged. Whole-volume shared extents are still nonadditive. Share answers, report accounting and cleanup are unchanged. Disposable top index format 5 and UI answer-cache format 3 reject older answers; persisted report format is unchanged.

- A share `#recycle` ranks as its own entry, once. An entry that is or lies in package data is fixed in its package and marked `*` like a service row; one that only holds package data is fixed by its own bytes; neither is `cleanup_eligible`. A snapshot not read is fixed in Snapshot Replication, an encrypted directory by `doctor`.
- `--kind files` ranks no service rows. `--columns` takes a comma list of `size`, `physical`, `can-free`, `shared`, `held-by`, `where-to-fix` (default all but `shared`). `--by held` ranks by physical bytes minus reclaimable bytes (`held` in `ranked_by`).

Data it needs: per-directory retained bytes per holder class. Today the volume scan keeps blocker totals, not a per-directory split; the design adds one u64 per class (7) for each *directory* only (directories are roughly 10% of entries, so about 6 B per entry on average against the 48 B target in section 9.1), attributed while streaming the extent tree. `recycle bin` needs the Recycle Bin map (section 5.7). Relies on: 3, 4.6 (service table, `#recycle`), 5.4 service summary, 5.7 (package-data refusal, Recycle Bin items), 9.1, 10, 11. Changes: the default visibility rule above.

A ranked entry that is, lies in or holds package-managed data (the rules of section 5.7: a guarded service path by name, a folder marked by `@ActiveBackup` or `SynologyHyperBackup.bkpi` at or above it, checked live, or a package root the report holds below it) gets a line under the table, `path: package-managed data (names): managed by X; manage it in Y`, or `path: holds package-managed data (...)`, and in JSON an additive `package_data` object (`service`, `package`, `manage_in`, `confirmed`, `marker` when a marker matched, `holds`, `message`). An Active Backup destination share that ranks first is thus shown as the package's data to free in the package, never by deleting its files.

### 5.5 `inspect`

```bash
synoreclaim inspect /volume1/share/Movies
synoreclaim inspect /volume1/share/movie.mkv
synoreclaim inspect /volume1/share/movie.mkv /volume1/homes/alice/large.bin
synoreclaim inspect /volume1/share/movie.mkv --explain
synoreclaim inspect /volume1/share/movie.mkv --json
```

For multiple paths, show one object section/result per path plus an optional combined summary. Per-path reclaimable values remain independent unless the output explicitly labels a combined target-set calculation.

Show:

- object type;
- report timestamp/age;
- logical size;
- physical footprint;
- shared bytes;
- reclaimable bytes if this path alone is deleted;
- hard-link count when relevant;
- extent count;
- known blockers/reasons when `--explain` is requested, and for a known service path (section 4.6) the package that manages it, where to manage it in DSM, and whether cleanup refuses it (`dsm_service`);
- consistency state (`observed-stable`, `changed-during-scan`, `partial`) and accounting coverage; none means an atomic filesystem snapshot.

### 5.6 `simulate-delete`

```bash
synoreclaim simulate-delete /volume1/share/old.iso
synoreclaim simulate-delete /volume1/share/A /volume1/share/B /volume1/homes/alice/old
synoreclaim simulate-delete --from-file paths.txt
synoreclaim simulate-delete /volume1/share/A /volume1/share/B --explain
synoreclaim simulate-delete /volume1/share/A /volume1/share/B --json
```

This is a first-class feature, not a sum of per-path values.

For a selection set `S`, calculate the result for deleting **all paths in S together**. This matters because two files can mutually keep one shared extent alive when considered individually but free it when both are deleted.

Show:

- selected logical bytes;
- selected physical footprint;
- reclaimable bytes for the combined selection;
- bytes retained by external references;
- optional "together bonus": bytes reclaimed only because multiple selected paths are deleted together;
- blocker summary grouped by snapshot / live reflink / hard link / other subvolume / unknown;
- what still holds the data after the delete, in DSM terms (`after_delete`): snapshots (Snapshot Replication), other hard links, reflink copies, a deleted subvolume the Btrfs cleaner has not freed yet, files still open; then that a File Station, SMB or AFP delete only moves files into the share's `#recycle` while its Recycle Bin is enabled, and that Storage Manager and `df` show freed space only after the Btrfs commit and cleaner.

Each selected path that is, lies in or holds package-managed data gets the same line and `package_data` object as `top` (section 5.4).

With `--explain`, selected paths in services with a fixed service-table explanation also show that explanation once per service. For `@iSCSI`, it explains thin versus thick provisioning and directs VMM image questions to the selected storage in Virtual Machine Manager; for `VDISK_*` it directs virtual-disk questions there. This is guidance only; the command never sends UNMAP or invokes a package operation.

This command never deletes paths. It is the analysis primitive used by cleanup planning.

For path selections consisting only of non-directory inodes with at most one link within a tree,
read each distinct selected inode's `INODE_ITEM` and remaining inode-key range
(including every `EXTENT_DATA`) instead of its whole namespace. Preserve
`nlink`, inline/hole/preallocated/compressed extent handling, leaf-sharing
flags and the complete extent/backref lookup. A selection containing a
directory, an inode with `nlink > 1`, a subvolume root or a volume still reads the full namespace.
Discovery before/after the read, selected-subvolume generation and snapshot
certification, live path/identity checks and cleanup protection checks remain
mandatory. This shortens the instability window; it does not certify a
changed selected subvolume or eliminate bounded plan retries.
File-only namespace fragments reserve scope metadata from the selected roots,
including Recycle Bin ctimes; they cannot use an ancestor-dependent whole-tree
cap for those records.

`simulate-delete` also accepts snapshot roots (share snapshots under `#snapshot`, or other read-only subvolume roots) as targets, alone or mixed with live paths, and computes one combined result with the engine of section 5.14 (a set of only snapshot roots needs a report with materialized snapshots; a mixed set scans its live paths). A snapshot root in the set makes `cleanup plan` refuse the set, as before; the output points to Snapshot Replication.

### 5.7 `cleanup`

Cleanup moves explicitly selected items in shared folders into their shared folder's DSM Recycle Bin: the user restores the items, or empties the Recycle Bin, in DSM. An item with no Recycle Bin, in a folder right below the volume that DSM does not list as a shared folder (left over from a restore, say) or in a shared folder whose Recycle Bin is off, is removed for good, only after the user confirms exactly that ("Removing for good" below). Space is freed only after the Recycle Bin is emptied (and, on Btrfs, after the transaction commit and cleaner). The system partition is cleaned by `rootfs` (5.23), which keeps its own copy-and-remove executor.

#### The move

Each item moves with DSM's own tool, the library File Station deletes with:

```text
/usr/syno/bin/synorecycle --rmdir  share=<shared folder> rpath=<path inside it>   (a folder)
/usr/syno/bin/synorecycle --unlink share=<shared folder> rpath=<path inside it>   (anything else)
```

It runs with a fixed argv, no shell, from `/`, an empty environment except a fixed PATH and `LANG=C`, and only when the program is a regular file root owns, executable and writable by no one else. DSM's tool moves every regular file into a hidden staging folder (`<share>/@eaDir/@recycle/<task>`) and its `recyclebind` daemon moves it into `<share>/#recycle/<path inside the share>`, keeping the relative path, the file's inode, owner and modification time, and setting the deletion time (a `dtime` extended attribute) the Recycle Bin's retention policy reads. Folders are recreated inside `#recycle` (owner root, ACL inherited from `#recycle`); empty folders and symlinks are not kept. Reclaim keeps that behaviour, as File Station's delete has it: the plan counts the symlinks each item holds (`symlinks` on its item and shared folder, absent when none; from the scan report's records, or the walk of an item no report covers), and Review says how many a move drops before it is confirmed. A name already taken in `#recycle` gets DSM's `_HHMMSS[_n]` suffix. A user home's items, selected as `homes/<user>/…`, go to `homes/#recycle/<user>/…`, as when an administrator deletes them in File Station. DSM 7.2.1 and later restore an item with File Station's Restore (`#recycle` removed from its path); earlier DSM has no Restore button, so the item is moved out of `#recycle` by hand. A folder's explicit ACL is not restored: the restored folders inherit the destination's.

DSM's tool does not check that the Recycle Bin is enabled (it then exits 0 doing nothing) and removes an item for good when its path inside the share starts with `#recycle/` or `@eaDir/`. Cleanup therefore never runs it on such a path and checks every move afterwards.

#### `cleanup plan`

```bash
synoreclaim cleanup plan /volume1/share/old.iso /volume1/share/A [--expect JSON] [--duplicates JSON] [--output FILE] [--json]
```

Checks each selected path and saves a plan in root's private state (`<state-dir>/cleanup/plans/<plan id>.json`, 0600); it moves nothing. A path is refused (each refused path is one `errors[]` item, exit 4, no plan) when it:

- is not inside a folder below the volume (the volume root, a shared folder root, a user home itself): `protected_target`; a folder right below the volume that DSM does not list (`synoshare --enum ALL`) is refused itself (`reason: outside_share`), while the items inside it are planned to be removed for good, except in a hidden or file-system folder (`.…`, `lost+found`: `reason: system`);
- is, or lies in, a Recycle Bin (`#recycle`): `in_recycle_bin`. Reclaim never removes anything from a Recycle Bin;
- passes through any other `@…`/`#…` name, or is package data (the service table of 4.6, a folder holding a package marker, rule 2 and 3 of package data below): `protected_target`;
- is a Btrfs subvolume root: `selection_boundary`; all paths must lie on one volume: `selection_boundary`;
- is a symlink or a special file, or reached through a symlink: `unsupported_object`;
- carries a report identity (`expect`, from Explore or Recycle Bin rows) whose inode is no longer the path's: `target_changed`;
- lies outside the configuration's `allowed_roots`: `outside_allowed_roots`.

A plan has no item limit. A selection too large for one request is staged first: `cleanup select --chunk '{paths: [{path: hex, objectid?}], groups: [id…]}' [--selection ID]` appends one chunk to `<state-dir>/cleanup/selections/<ID>` (root's private state on disk, never `/tmp`; JSON lines; a selection untouched for a day is removed when another starts) and prints `cleanup-select` with the `selection` ID and the counts so far; `cleanup plan --selection ID` reads it and removes it, so a recheck stages it again. Each step is linear in the selection: one `synoshare --enummnt` per volume, one listing of reports per filesystem, one streaming read per report with the items looked up by path, and the inodes the selection carried by path. The WebAPI runs `cleanup.plan` as a planning job (22.1) whose `progress` counts the items checked (`units_done` of `units_total`); it has no time limit. Package data rules: (1) a known service path of 4.6; (2) a selected folder holding a `*.hbk` folder, a `SynologyHyperBackup.bkpi` file, an `@ActiveBackup` folder or a Microsoft 365 / Google Workspace backup folder at any depth; (3) a path inside a folder holding `SynologyHyperBackup.bkpi` or `@ActiveBackup`.

The plan groups the items by shared folder (`shares[]`, each with `item_count` and `drive_items` and the first 100 of its `items`) with each one's Recycle Bin settings from `synoshare --get` (`recycle_bin: {enabled, admin_only}`). Items of a shared folder whose Recycle Bin is off are planned to be removed for good (below). An item in a user's Synology Drive folder (`homes/<user>/Drive/…`) is marked `drive`. DSM's `recyclebind` must be running (`recycle_bin_unavailable` otherwise).

Each item's size (`entries`, `files`, `allocated_bytes`) comes from the newest report covering it whose entry still has the item's inode, in one streaming read per report; only an item no report covers is walked. The same read lists the folders below an item holding more than 100,000 entries (`split`): those move folder by folder (below). A plan expires after 24 hours (`plan_stale`); its checksum detects corruption, not tampering, which root's private state prevents.

A duplicates selection (`--duplicates {volume, groups}`, docs/duplicates.md) adds the copies the keep rule moves and the copies it keeps. Each kept copy must still exist, a file with the size the analysis read (`keeper_changed`), and the groups together must leave a copy of each (`keeper_removed`).

#### Removing for good

An item with no Recycle Bin to move to is removed for good. That is an item in a folder right below the volume that is no DSM shared folder, planned with that folder as its `share` and no Recycle Bin read for it (`delete_reason: no_shared_folder`), and an item of a shared folder whose Recycle Bin is off (`delete_reason: recycle_bin_off`). Its share is marked `delete: true` in `shares[]` with the `delete_reason`, and the item is counted in the plan's `deletes` with a `deletes_for_good` warning. Turning the Recycle Bin on (Control Panel > Shared Folder > Edit > Enable Recycle Bin) and making a new plan moves the items of that shared folder to its Recycle Bin instead. Apply refuses such a plan without `--confirm-delete` (`delete_not_confirmed`, nothing moves; the WebAPI's `confirm_delete: true`), which the Review dialog sends only with a confirmation box naming how many items are deleted for good. `cleanup delete` refuses a plan that removes for good. A planned removal stays one even when the Recycle Bin is turned on before apply: the user confirmed it; a new plan moves the items instead. An item planned to move whose shared folder's Recycle Bin is turned off after the plan is removed for good when apply reaches it, with no new plan or confirmation, and counts as `deleted`. Apply checks the item is still the planned one (device, inode, kind: `target_changed` otherwise), then removes it through directory descriptors: no symlink is followed, nothing on another device (a mount or a Btrfs subvolume below it) is touched, and those parts and the folders holding them stay (`partial`). The outcome is `deleted`; the result counts `deleted` and `deleted_bytes` apart from what moved, and the job progress counts `deleted`. The space is freed at once (on Btrfs once no snapshot or reflink holds it).

#### `cleanup apply`

```bash
synoreclaim cleanup apply PLAN --yes [--confirm-delete] [--detach[=self|none]] [--json] [--profile …]
synoreclaim cleanup delete PATH… [--yes] [--json]     # plan, confirm and apply in one command
```

Apply requires root, a valid plan, cleanup enabled in the configuration (`cleanup_disabled`, the kill switch `mutations_disabled` included) and `--yes` or an interactive confirmation; it then checks every kept duplicate (`keeper_changed`, nothing moves) and that `recyclebind` runs. It holds the filesystem writer lock and mutation locks on the selected paths (as `rootfs apply` does), or on their shared folders when the plan holds more than 256 items, so a plan of any size holds a few locks. It marks every report of the filesystem stale before the first move (`<state-dir>/reports/<fsid>/cleanups.json`, read by every answer as `report_info.cleanup_since_report`) and the selected subvolumes' cached units stale.

For each item, in plan order:

1. the path must still be the planned item (same device, inode and kind): else `changed`, left alone; gone already: `already_absent`;
2. its shared folder's Recycle Bin is read again (once per shared folder per apply): when it is off now, the item is removed for good as above (`deleted`) instead of the next steps;
3. DSM's tool moves it. A folder the plan splits moves folder by folder, sorted by name bytes: a child folder that is itself split recurses, any other child folder is one run; then a final `--rmdir` takes the files directly in it, what appeared meanwhile and the folder itself. Files are not moved one run each: DSM's start-up costs about 13 ms a run, so one run per file moved 79 files/s against about 3,000 for a folder run (a test NAS, DSM 7.4). A huge flat folder therefore still moves in one run, which a cancel waits for. Selected files are one run each (DSM's tool takes one `rpath` a run): 50,000 selected files take about 11 minutes at 13 ms a run, in the background job;
4. `lstat` checks the result: the item gone is `moved`; still there is `partial` (some moved) or `failed` (nothing moved), and a walk of what stayed, at most 100,000 entries (`remaining_complete: false` beyond), lists up to 50 paths.

A cancel (`jobs cancel`) or a stop signal ends the apply between DSM's runs, never inside one: a run stopped half-way would leave items in DSM's hidden staging folder. The item being moved then ends `partial` with what stayed, and the items after it stay untouched. Its only time limit is the executor's longest one, 7 days (`job.timeout_s`, 604800, for every root job); a cancel is the job's own request, never the executor's stop, which kills a job 10 s later, inside a DSM run. DSM moves a few thousand files per second (a test NAS, DSM 7.4: 100,000 small files in 100 folders took 32 s split into 101 runs and 46 s as one `--rmdir`, until all were in `#recycle`), so a folder of 10^8 files takes hours, split into runs the cancel and the progress reach.

Exit 0 when every item moved, was removed for good or was already gone, 3 when one moved partly or failed, 4 when one changed, 10 cancelled, 129/130/143 for a signal. Refusals before anything moves exit 4 (`plan_stale`, `cleanup_disabled`, `keeper_changed`, `recycle_bin_unavailable`, `busy`, `not_confirmed`, …), other failures 1.

The apply document (`schemas/v1/cleanup-apply.schema.json`) has the counts (`moved`, `deleted`, `already_absent`, `partial`, `changed`, `failed`), `moved_bytes` (allocated size now in Recycle Bins), `deleted_bytes` (allocated size removed for good), `items` (in the plan), the first 1,000 targets that did not move (`targets[]`; the counts cover every item) with their outcome, `moved_bytes`, `remaining_entries`, `remaining_bytes`, `remaining_complete`, `remaining` and `detail`, `recycle_bins[]` (per shared folder: `admin_only`, `items`, `moved_bytes`), `report_state_stale`, `rescan` (the shared folders) and `space_release` (space is freed after the Recycle Bin is emptied). It is kept as `<state-dir>/cleanup/logs/<plan id>-<attempt>.json`. Each move is also recorded per shared folder in `<state-dir>/cleanup/moved.json` (newest 256), which the Recycle Bin answer shows (5.16).

`--detach` queues the apply as a `cleanup-apply` job (section 22.1; the WebAPI always detaches) and prints one `detach` line. The job names the shared folders it moves from, not each item. Its `cleanup` progress (`schemas/v1/jobs.schema.json`) is rewritten at most every two seconds, and once more at the end: `items_total`, `items_done`, the outcome counts, `bytes_total`, `bytes_moved`, `calls_done` (DSM's runs), `elapsed_s`, `updated`. `pause` is refused; `cancel` stops it as above (`completion: cancelled`). Applying the same plan again moves what is left: moved items are `already_absent`.

#### Cleanup JSON contract

`cleanup plan`, `apply` and `delete` print one document with the standard envelope; `command` is `cleanup-plan`, `cleanup-apply` or `cleanup-delete` (`schemas/v1/cleanup-{plan,apply,delete}.schema.json`). A plan has `plan_id`, `plan_file`, `expires_at`, `items` (how many it moves or removes), `deletes` (how many it removes for good), `paths` (the first 100), `size`, `shares[]` (`share`, `path`, `recycle_bin`, `item_count`, `drive_items`, the first 100 `items[]` with `path`, `kind`, `drive` and size, the shared folder's size, and `delete` with `delete_reason` when its items are removed for good) and `duplicates` when a duplicates selection expanded. A refusal is the envelope alone with `completion` `failed` or `busy` and an `errors[]` item per refused path. Consumers tolerate new codes.

#### `config`

```bash
synoreclaim config init [--json]
synoreclaim config set --mutations-disabled on|off [--json]
synoreclaim config show [--json]
```

Root only (else `not_root`, exit 4). They manage `config.json` in the package's configuration folder, `/var/packages/SynoReclaim/etc` (owned by the package user `SynoReclaim`): `{"cleanup_enabled": bool, "mutations_disabled": bool, "allowed_roots": [absolute path, ...]}`. Cleanup moves items only when `cleanup_enabled` is true and `mutations_disabled` is false; `allowed_roots` only narrows which paths may move. The package user writes this file, so root reads it only as a regular file (`O_NOFOLLOW`, at most 64 KiB) the package user owns with no group or other access, in a folder only the package user can write; anything else refuses with `config_untrusted`. `init` writes `{"cleanup_enabled":true,"mutations_disabled":false}` when no file exists (the package's `lifecycle.update` runs it); `set` rewrites the kill switch atomically keeping the other keys; `show` reads it. Schemas: `schemas/v1/config-{init,set,show}.schema.json`.

### 5.8 `shared`

```bash
synoreclaim shared /volume1
synoreclaim shared /volume1/share
synoreclaim shared /volume1/share/Movies /volume1/homes/alice
synoreclaim shared --from-file paths.txt
synoreclaim shared /volume1/share --limit 50
synoreclaim shared /volume1/share --json
```

Find paths whose physical footprint is heavily shared.

This command explains storage behavior; it does not imply the data is waste.

Sort options should include:

- shared bytes;
- shared ratio;
- logical size.

Where possible classify sharing as:

- snapshot;
- live file/reflink;
- hard link;
- mixed;
- unknown.

### 5.9 `cache`

```bash
synoreclaim cache status /volume1
synoreclaim cache clear /volume1
```

Cache operations never affect user data.

`cache status` should show cache format version, filesystem UUID, subvolume generations, cache size, and whether any derived index is stale.

### 5.10 `doctor`

```bash
synoreclaim doctor /volume1
```

Read-only environment diagnostics:

- filesystem is Btrfs;
- required ioctl works;
- privilege level is sufficient;
- subvolume discovery works;
- cache location is writable;
- cache format/checksum is valid;
- report references the same filesystem UUID;
- the state directory (`--state-dir`) is not on the target's filesystem (section 22.4);
- subvolumes deleted but not yet freed by the Btrfs cleaner (`pending_deletion` check, with `subvolumes.pending_deletion_bytes` always `null`, see section 5.2): the space will be freed later;
- the system partition is not nearly full (`system_partition`, the same 5% rule as `volumes`; reported, never fixed by `doctor`; see 5.23 `rootfs`);
- the state filesystem keeps the scan's free-space reserve (`state_space`, section 22.4) for the given `--min-free`, and the largest spill recorded for the target's filesystem fits above it; if spill uses another filesystem, `spill_space` checks that location separately. Otherwise `doctor` warns that the scan would stop and pause, resumable;
- unsupported/unknown Synology Btrfs items are summarized, not treated as fatal unless required for correctness.

Also report actual scheduling settings, scheduler visibility/support caveats, state-directory permissions/free space, resource budgets, supported accounting features, and the active job lock. `doctor` never changes system-wide schedulers, mount options, quotas, or services.

### 5.11 Job control and scan resource options

```bash
synoreclaim scan /volume1/share /volume1/homes --profile low
synoreclaim jobs
synoreclaim jobs JOB_ID
synoreclaim pause JOB_ID
synoreclaim mode JOB_ID --profile low|normal|fast
synoreclaim resume JOB_ID
synoreclaim resume JOB_ID --max-memory 512MiB --profile low
synoreclaim resume JOB_ID --detach=none --profile fast   # an external launcher runs it
synoreclaim cancel JOB_ID
synoreclaim scan /volume1 --max-memory 256MiB --max-runtime 30m
```

`scan` runs in the foreground and prints a job ID. Ctrl+C is a graceful scan pause. `pause --json` and `cancel --json` answer with the standard envelope (`job_id` = the job asked about) plus `outcome` and `state` (the job's state afterwards, as `jobs` shows it; null when there is no such job). `outcome` is `requested` (the running job stops at its next safe point; poll `jobs` for the end), `cancelled` (a stopped job ended at once), or a refusal with exit 1: `too_late` (the job is publishing its report and no longer checks for requests), `not_running`, `finished`, `no_job`, or `not_pausable` (pause of a cleanup apply job, section 5.7). `resume` of a cleanup apply job is refused as `not_resumable`. Without `--json`, refusals stay error text with exit 1. `pause` requests checkpoint-and-exit from another terminal; `resume` starts a new foreground process. `resume --detach[=self|none]` is `scan --detach` for a stopped job (section 5.3): it checks that the job can resume now (paused or interrupted, from this binary; otherwise the same refusals as `resume`, e.g. `busy`, `finished`, `not_started`, `job_incompatible`, `no_job`), atomically reserves NAS-wide scan admission as `queued`, saves the optional profile override and increments `resumes` once, then prints one `detach` JSON line whose `run` is `scan --job-id JOB_ID --state-dir DIR`; `self` runs it in a new session with output appended to `jobs/<id>/output.log`, `none` leaves it to the caller's launcher. Only `--profile` may change with `--detach`. No process must stay alive while paused. `cancel` stops the scan and discards unfinished work, not user files or valid reusable cache. `jobs` lists recent jobs newest first; `jobs JOB_ID` shows only that job. `jobs --result JOB_ID` prints the whole document of the job's latest run exactly as saved in `jobs/<id>/result.json` (exit 0); a job with no finished run yet is refused as `no_result`, an unknown one as `no_job` (exit 1; with `--json` the job-command refusal document with `command: "job-result"`). See section 22 for lifecycle, resource defaults, checkpoint granularity, and failure behavior. These commands control scan/analysis jobs, not destructive cleanup replay.

### 5.12 Question-centred commands

Accepted design; not implemented yet. Sections 5.12–5.18 and the related parts of 5.4, 5.6, 5.7, 8.1 and 13 are the target behaviour for Phase 5. Items marked *verify first* depend on a read-only check on a test NAS or a fixture done before building on the assumption.

Support tickets ask four questions far more often than "which file is largest": *where did my space go*, *why didn't deleting free space*, *which snapshot holds it*, and *why don't the numbers match*. Each question gets one command that answers it from a scan report, so a user does not need to know Btrfs:

| Question | Command |
| --- | --- |
| Where did my space go? Do the numbers add up? | `overview <volume>` (5.13) |
| What should I delete, and who holds the rest? | `top` (5.4, with `HELD BY` / `WHERE TO FIX`) |
| Which snapshot holds it? What would deleting snapshots free? | `snapshots <share\|volume>` (5.14), `simulate-delete` on snapshot roots (5.6) |
| I deleted files, why is the space not back? | `why-not-freed <volume>` (5.15) |
| How big is the recycle bin, and can I empty old items? | `recycle <volume\|share>` (5.16) |
| What do I attach to a support ticket? | `support-bundle <volume>` (5.17) |
| Which Drive folder's versions use the space? | `drive <volume>` (5.18, lower priority) |

Rules shared by all of them:

- They are analysis commands (section 15): read-only with respect to user data. A cleanup plan file is created separately with `cleanup plan` and still needs `cleanup apply` with all its checks.
- They answer from the newest covering report under the 5.1 rules (no unannounced volume scan; `--refresh`, `--no-scan`, `--max-age`; capture window always printed). Where a command needs data that the report does not hold yet (for example per-snapshot sets), it says which scan produces it instead of starting one.
- Unknown stays `unknown`/`null`, never zero (section 3). A byte count that an action *might* free is labelled `held` (an upper bound on what that action can free), never `reclaimable`. Only bytes proven under section 10 are `reclaimable`.
- Human output uses DSM terms (Storage Manager, shared folder, Snapshot Replication, Recycle Bin) and names the DSM place where the user acts. The service table (section 4.6) is the single source of "managed by X, manage it in Y" text.
- JSON follows section 14: one document, integer bytes, `schema_version`, `report_info` (capture window, consistency, `change_reasons`, coverage), symbolic reason codes, additive changes only.
- Exit codes are the section 22.7 set: 0 complete; 1 fatal; 2 usage; 3 usable partial answer (some values unknown because a budget or coverage gap stopped resolution); 4 refusal (for example a `recycle` target that is not a real Recycle Bin); 10 cooperative stop. A volume target without a covering report ends like `top` today: the section 5.1 scan hint, the target error `no_report`, and no implicit volume scan.

### 5.13 `overview`

```bash
synoreclaim overview /volume1
synoreclaim overview /volume1 --json
synoreclaim overview system
```

The first command a new user runs. One screen that splits the filesystem's used space into parts that add up, names the residual, and lists the five actions that free the most, with where to do each.

DSM-service items include the optional fixed `explanation` from the service table (section 4.6) in JSON and human output. This lets the `@iSCSI` item explain provisioning and direct VMM image location questions to Virtual Machine Manager.

Input: the newest complete volume report plus the space figures captured with it (below). `overview` needs a volume scan; a path report does not cover it. With no covering report it behaves like `top`: the section 5.1 scan hint (`scan_hints`, with the last scan's duration) and the target error `no_report`.

Human output (mock-up, sanitised):

```text
/volume1  Btrfs  7.21 TiB total, 5.79 TiB used (df), 1.42 TiB free
report 2026-09-28 10:02–10:17 (2h ago), changed-during-scan: 3 subvolumes outside any share you selected

WHERE THE USED SPACE IS                          BYTES     CAN FREE   WHERE TO FIX
Shared folders (live data)                     3.10 TiB    2.71 TiB   delete files: synoreclaim top /volume1
  photos                                       1.42 TiB    1.39 TiB
  backup                                       0.98 TiB    0.61 TiB
  … 9 more shared folders                      0.70 TiB    0.71 TiB
Recycle bins (#recycle, 4 shared folders)       212 GiB     209 GiB   Recycle Bin: synoreclaim recycle /volume1
DSM services                                   1.58 TiB    —          managed by the package (see below)
  Synology Drive versions and database          905 GiB    —          Synology Drive Admin Console > versions
  Active Backup for Business                    512 GiB    —          Active Backup for Business > versions
  Container Manager (@docker)                    63 GiB    —          Container Manager
  … 5 more                                      140 GiB    —
Held only by snapshots                          688 GiB    held       Snapshot Replication: synoreclaim snapshots /volume1
Shared between folders (reflinks)               41 GiB     —          synoreclaim shared /volume1
Deleted, waiting for Btrfs cleanup               18 GiB    frees by itself  synoreclaim why-not-freed /volume1
Allocated but unused parts of extents           9.6 GiB    —          (explained: synoreclaim inspect --explain)
Unknown (references not resolved)               2.1 GiB    unknown
─────────────────────────────────────────────────────────────
File data, sum of the above                    5.63 TiB
File data, Btrfs data used (SPACE_INFO)        5.63 TiB   residual +0.4 GiB  reconciled (tolerance 5.8 GiB)
Btrfs metadata and system (not reclaimable)     160 GiB   display only
Used (df)                                      5.79 TiB   residual +0.1 GiB

TOP ACTIONS
1. Empty Recycle Bin of "photos" (DSM: shared folder settings)      frees 186 GiB
2. Delete old snapshots of "backup" (Snapshot Replication)          up to 402 GiB held; see synoreclaim snapshots /volume1/backup
3. Lower Synology Drive version retention (Drive Admin Console)     up to 905 GiB held; not predictable per version
4. Delete /volume1/photos/2019-raw (synoreclaim cleanup delete …)   frees 96 GiB
5. Wait for Btrfs cleanup of 2 deleted snapshots                    18 GiB, no action needed
```

Parts (each allocated data extent is attributed to exactly one part, in this order of precedence, so the parts add up without double counting):

1. **Deleted, waiting for Btrfs cleanup**: every reference comes from a root classified `pending_subvolume_deletion` (section 8.1 step 5).
2. **Held only by snapshots**: every reference comes from read-only snapshot roots (read-only subvolumes whose `parent_uuid` chain reaches a present share, and read-only subvolumes linked in the root directory of `@sharesnap/<share>/`, even when their `parent_uuid` names nothing present: a restored share or a promoted Snapshot Replication copy is a writable snapshot of the snapshot, and the subvolume the snapshot was taken from is gone) or pending roots, with at least one snapshot. Split per source shared folder. Unresolved `shared_tree` owners stop an extent from being placed here; it goes to **Unknown** (section 8.1 step 3; resolution budget as in 5.14).
3. **Recycle bins**: every live reference comes from paths inside one share's real `#recycle` (Recycle Bin rules, section 5.7), per share.
4. **DSM services**: every live reference comes from one entry of the service table in section 4.6 (for example `@synologydrive`, `@ActiveBackup*`, `@docker`, `@appstore`, ABB destination folders, Hyper Backup vaults), per service. This includes data a package keeps inside ordinary shared folders when the service table marks it (for example `*.hbk`, Active Backup data, Surveillance recordings). A read-only subvolume belongs to a service when its `parent_uuid` chain reaches that service's root, or, when no rule of part 2 places it, when it is stored inside that service's root: a LUN snapshot below `@iSCSI` whose source LUN is gone, or a snapshot in the `@sharesnap` folder of no present share (Snapshot Replication).
5. **Shared folders (live data)**: every live reference comes from one shared folder's live namespace (outside `#recycle` and service entries), per share.
6. **Shared between folders**: live references from more than one of parts 3–5 (reflinks or hard links across folders or services). Not split further in this view; `shared` explains them.
7. **Allocated but unused parts of extents**: the `pinned_unreferenced_bytes` of all extents (section 3), taken out of the part the rest of the extent went to.
8. **Unknown**: missing extent items, unresolved `shared_tree` owners, data-relocation-tree references, unknown ref types, coverage gaps (locked eCryptfs shares, unscanned subvolumes).
9. **Other volume data** (`system_other`): indexed volume paths outside known shares/services. Can-free and held values remain null; missing inode owners never enter this bucket.

Each part shows `bytes` (physical, section 3) and, where section 10 proves it for deleting that part's paths, `reclaimable`. Parts 1, 2, 4 and 6 show no reclaimable value: part 1 frees without action, part 2 is freed only by deleting snapshots (5.14 computes that), part 4 only through the package (cleanup refuses package data, section 5.7), part 6 depends on which copy goes.

Reconciliation (the core promise of this view):

- **Data level (certified).** The sum of all parts equals the sum of `disk_num_bytes` over the data extents the scan streamed from the extent tree. Compare it with the data `bytes_used` from `BTRFS_IOC_SPACE_INFO` (read-only; single-copy bytes, before the RAID/DUP factor). The report records SPACE_INFO at scan start and scan end. `reconciled` requires `|sum − data_used| ≤ tolerance` for the scan-end value, where tolerance is the larger of 1 GiB and 0.1% of data used, plus the absolute change of data used between scan start and end. The residual is always printed with its sign; it is never folded into a part.
- **df level (display only).** `Used (df)` is `(f_blocks − f_bfree) × f_bsize` from `statfs` at scan end. Show `data used + metadata used + system used` (from SPACE_INFO, each multiplied by its profile's copy factor) next to it and print the residual. Metadata and system bytes are display only, never reclaimable (section 4.5).
- DSM Storage Manager's "Used" is assumed to equal df `Used` for a Btrfs volume on one md/LVM device, within the larger of 1 GiB and 0.1%. *Verified first*: read-only on a test NAS (DS420+, DSM 7.4 build 90069) on 2026-09-29, Storage Manager `volume_1` reported total 939,051,651,072 bytes and used 42,948,796,416 bytes; `df -B1 /volume1` reported the same total and used values (zero difference). Confidence is high for this volume/build; other DSM builds remain unverified.
- A `changed-during-scan` report still reconciles, but the output says the data moved during the window and the tolerance includes the start/end change. `partial` coverage prints `not reconciled: coverage gaps` and the gap bytes, if known.

Top actions: at most five, ranked by proven `reclaimable` first, then by `held` bytes. Each action has a kind (`cleanup`, `recycle`, `snapshot`, `package`, `wait`), a DSM place, an optional synoreclaim command, and either `reclaimable_bytes` or `held_bytes` (never both as the same number). `wait` actions come from part 1. No action ever names a DSM service path as a cleanup target.

JSON (sketch):

```json
{
  "schema_version": 1,
  "command": "overview",
  "report_info": { "...": "section 14" },
  "filesystem": { "uuid": "…", "mount": "/volume1" },
  "space": {
    "df": { "total_bytes": 0, "used_bytes": 0, "free_bytes": 0 },
    "space_info": [
      { "type": "data", "profile": "single", "copies": 1, "total_bytes": 0, "used_bytes": 0,
        "captured": "scan_end", "used_bytes_at_scan_start": 0 },
      { "type": "metadata", "profile": "dup", "copies": 2, "total_bytes": 0, "used_bytes": 0 },
      { "type": "system", "profile": "dup", "copies": 2, "total_bytes": 0, "used_bytes": 0 }
    ]
  },
  "parts": [
    { "kind": "pending_cleanup", "bytes": 0, "reclaimable_bytes": null, "held_bytes": 0, "items": [] },
    { "kind": "snapshot_only", "bytes": 0, "held_bytes": 0,
      "items": [ { "share": "backup", "bytes": 0 } ] },
    { "kind": "recycle_bin", "bytes": 0, "reclaimable_bytes": 0, "items": [ { "share": "photos", "bytes": 0, "reclaimable_bytes": 0 } ] },
    { "kind": "dsm_service", "bytes": 0, "items": [ { "service": "synology_drive", "managed_by": "Synology Drive", "manage_in": "Synology Drive Admin Console > version settings", "bytes": 0 } ] },
    { "kind": "share_live", "bytes": 0, "reclaimable_bytes": 0, "items": [ { "share": "photos", "bytes": 0, "reclaimable_bytes": 0 } ] },
    { "kind": "shared_between_folders", "bytes": 0 },
    { "kind": "pinned_unreferenced", "bytes": 0 },
    { "kind": "unknown", "bytes": 0, "reasons": [ { "code": "shared_tree_owner_unresolved", "bytes": 0 } ] }
  ],
  "reconciliation": {
    "data": { "parts_sum_bytes": 0, "space_info_used_bytes": 0, "residual_bytes": 0,
              "tolerance_bytes": 0, "status": "reconciled" },
    "df": { "used_bytes": 0, "data_metadata_system_bytes": 0, "residual_bytes": 0, "status": "display_only" }
  },
  "actions": [
    { "rank": 1, "kind": "recycle", "share": "photos", "reclaimable_bytes": 0, "held_bytes": null,
      "manage_in": "…", "command": ["synoreclaim", "recycle", "/volume1/photos"] }
  ]
}
```

`status` is `reconciled`, `not_reconciled`, or `unknown` (a SPACE_INFO read failed). `residual_bytes` is a signed integer. Share and service names are real names in normal output; `support-bundle` redacts them (5.17).

A reason may carry additive `subcases` (`code`, `bytes`) that split its bytes by mechanism; names and paths are never included. `pinned_ranges_unknown` splits into `no_scanned_use` (no scanned file range references the extent, e.g. only unmaterialized snapshots do), `shared_reference` (a shared tree block references it and the scanned ranges leave part of it unreferenced, so unscanned ranges may cover that part; scanned ranges that cover the whole extent prove zero pinned bytes), `range_unmappable` (a scanned range does not fit the extent), `read_budget_exhausted` and `leaf_unverified`. When every owner of such an extent is known, a volume overview reads the ranges the scan did not see (section 8.3): the leaf each `SHARED_DATA_REF` parent names, and for a direct reference to an unscanned root a bounded search of that root. The pinned bytes are then measured over the scanned and read ranges together. Reads and root searches are limited per scan by `--pinned-read-budget` (default 500,000; parsed leaves cached in a bounded set charged to `--max-memory`; `--profile fast` batches and parallelises these reads, section 22.3); an extent the budget leaves unread is `read_budget_exhausted`, and one whose leaf fails verification or whose references its leaf does not account for is `leaf_unverified`. A direct reference to a deleted root awaiting the cleaner cannot be searched and keeps `no_scanned_use`. Without a block reader (section 8.3) the earlier subcases stay, and `scan_stats.pinned_leaf_reader_unavailable` says why. `scan_stats` counts `pinned_leaf_reads`, `pinned_leaf_read_bytes`, `pinned_leaf_cache_hits` and `pinned_root_searches`.

Data it needs beyond today's volume report: per-extent part attribution during the extent-tree stream (a fixed set of per-share/per-service counters, not per extent); SPACE_INFO and statfs at scan start and end in the report header; the share list (share-root subvolumes, section 4.6) and the service table (section 4.6); part 2 needs `shared_tree` owner resolution (5.14). A whole-volume scan keeps an inode-to-owner index for exact direct references: its on-disk payload is 16 bytes per regular inode with external data (an inode id and owner code), with one shared 1 MiB page cache and small per-tree metadata in memory. It is per file inode, not per physical extent. The extent-tree pass attributes one extent at a time and folds its answer into the fixed counters. SPACE_INFO needs no new privileges (same root-only model).

Relies on: sections 3, 4.5, 4.6 (service table, `#recycle`), 5.4 (service summary), 5.7 (package-data refusal, Recycle Bin items), 8.1, 9.1 (extent streaming), 11, 13. Changes: section 4.5 now allows metadata to be *displayed*; it stays excluded from reclaimable.

### 5.14 `snapshots`

```bash
synoreclaim snapshots /volume1                   # every share with snapshots
synoreclaim snapshots /volume1/backup            # one share
synoreclaim snapshots /volume1/backup --json
synoreclaim snapshots /volume1/backup --simulate-delete older-than:90d
synoreclaim snapshots /volume1/backup --simulate-delete id:1040,id:1051
synoreclaim simulate-delete /volume1/backup/#snapshot/GMT+08_2026-06-01-00-00
```

Answers "which snapshot holds my space" and "what does deleting snapshot set S free". Read-only. It never deletes, creates, or modifies snapshots, and never enables or rescans quotas. Deletion happens in DSM Snapshot Replication (or Storage Manager for LUN snapshots); the output says so.

Human output (mock-up):

```text
/volume1/backup  12 snapshots, report 2026-09-28 10:02–10:17 (2h ago), observed-stable

SNAPSHOT (Snapshot Replication)      TAKEN              EXCLUSIVE    SHARED W/ LIVE   QGROUP EXCL
GMT+08_2026-09-27-00-00              2026-09-27 00:00      1.2 GiB        210 GiB     1.2 GiB
GMT+08_2026-09-20-00-00              2026-09-20 00:00     18.4 GiB        205 GiB     18.4 GiB
GMT+08_2026-06-01-00-00  (oldest)    2026-06-01 00:00     96.0 GiB        150 GiB     96.1 GiB
…
Held only by snapshots of this share: 402 GiB (sum of EXCLUSIVE is 131 GiB; the rest is shared between snapshots)

FILES HELD ONLY BY SNAPSHOTS (largest 3 of 1,204)   BYTES      IN SNAPSHOT
/volume1/backup/vm/disk.img  (deleted)              88.0 GiB   GMT+08_2026-06-01-00-00
/volume1/backup/db/dump.sql  (rewritten)            12.4 GiB   GMT+08_2026-09-27-00-00
/volume1/backup/iso/old.iso  (deleted)               4.1 GiB   GMT+08_2026-09-20-00-00

Deleting all snapshots older than 90d (5 snapshots) frees 311 GiB.
Delete snapshots in DSM: Snapshot Replication > Snapshots > backup.
```

Definitions (per filesystem, snapshot roots = read-only subvolumes whose `parent_uuid` chain leads to the share's live subvolume, plus read-only subvolumes linked in the root directory of `@sharesnap/<share>/` when that chain is broken — a restored share or a promoted Snapshot Replication copy is a writable snapshot of the snapshot, and the subvolume the snapshot was taken from is gone — plus `pending_subvolume_deletion` roots shown separately):

- **exclusive** of snapshot `s`: bytes of extents whose every reference resolves to `s`. This is what deleting `s` alone frees (plus its `pinned_unreferenced` part).
- **freed by deleting set S**: bytes of extents whose every reference resolves to roots in `S` ∪ pending roots, with at least one in `S`. Computed as one set operation, never as a sum of exclusives (section 10 rule, applied to roots).
- **held only by snapshots** of a share: extents whose references all resolve to that share's snapshot roots (and pending roots); equals overview part 2 for the share.
- **shared with live**: extents referenced by `s` and by the live share. Deleting snapshots never frees these.
- **qgroup excl**: the kernel's `excl` from the quota tree's `QGROUP_INFO` item for level-0 qgroup `0/<subvolid>`, shown only when quotas are already enabled, the quota status item says consistent, and no rescan is running. It is a cross-check column, never the answer, and only `snapshots` shows it (`overview` does not). Missing or inconsistent qgroups show `—`, not zero. This reads the quota tree with TREE_SEARCH only; section 2's exclusion of qgroup enablement/management stands.

Algorithm (bounded, read-only):

1. Materialize each snapshot root's file-extent items (EXTENT_DATA only; names are not needed for the numbers). For snapshots of one source, use the generation bound to skip unchanged leaves: a leaf in snapshot `s` whose generation is not newer than the creation transid of the next-older present snapshot `p` of the same source is also referenced by `p` (it existed in the source when `p` was taken), so it cannot hold extents exclusive to any set that excludes `p`. Read those leaves only when `p` is itself in `S`. *Verify first*: until loop fixtures and the qgroup oracle confirm this bound on the DSM kernel families, read every leaf of each materialized snapshot.
2. Snapshot scan references are sorted on the scan spill filesystem in 32,768-record runs, combined through binary merge levels, and joined in batches bounded by 16,384 retained references (one extent group stays together and its excess growth is checked). Equal inode ranges across snapshots keep one newest-root range while retaining every owner. This preserves union, histogram and held-file accounting without retaining every snapshot reference. Scratch descriptors are private and unlinked, and merges honor pause/cancel and disk reserves. The separate mixed-deletion census retains its existing model. For each candidate extent, resolve references by section 8.1, and resolve `SHARED_DATA_REF` owners by walking tree-block backrefs (`TREE_BLOCK_REF`, `SHARED_BLOCK_REF`, skinny `METADATA_ITEM`) up to roots. The default per-extent limit is 64 tree-block lookups (`--owner-budget`); the default global limit is 100,000 (`--owner-budget-global`). Snapshot census entries, file-range mappings, and the owner histogram are charged to `--max-memory`. Exceeding either lookup limit makes that extent `owner_unresolved`: its bytes go to `unknown`, never to exclusive or freed. Stale full-backref blocks after a deleted snapshot resolve to a pending or missing root; a missing root is `unknown`.
3. Attribute each extent to its owner-root set. Keep counters per root and per small root set; do not store per-extent owner sets for a volume (memory, section 9.1). `pinned_unreferenced_bytes` is the allocated part of a freed extent outside the union of its uncompressed file ranges. It is `null` when any relevant owner is unresolved or a range cannot be mapped exactly.

`--simulate-delete` selectors: `id:<subvolid>`, `name:<snapshot name>`, `older-than:<duration>` (by ROOT_ITEM `otime`), `all`. `simulate-delete <snapshot root path>` accepts snapshot roots as targets and uses the same engine; mixing snapshot roots and live paths in one set is allowed and computes one combined result (a live path's extents count as freed only if every reference is in the selection). `cleanup plan` keeps refusing snapshot roots (section 5.7); the refusal points to Snapshot Replication.

Scan cost: materializing snapshots multiplies fs-tree reads by up to the number of snapshots. `snapshots` therefore needs a scan with snapshot materialization, `scan <volume|share> --snapshots` (new flag, off by default; the 5.1 no-implicit-volume-scan rule applies). `--snapshots` materializes the newest 64 snapshots of each share (by `otime`); `--snapshots all` reads every one. Extents that an unmaterialized snapshot may reference cannot be proven exclusive or freed: they are `unknown`, and the output names how many snapshots per share were skipped and the `--snapshots all` command. Coverage records `snapshots_materialized` next to `snapshots_not_materialized`. `snapshots` on a report without them prints the `scan --snapshots` command and fails with the target error `no_report`, as `top` does without a covering report. Budget exhaustion yields exit 3 with the unresolved bytes shown.

JSON (sketch):

```json
{
  "command": "snapshots",
  "report_info": { "...": "section 14" },
  "shares": [
    { "share": "backup", "live_subvolume_id": 261,
      "held_only_by_snapshots_bytes": 0, "unknown_bytes": 0,
      "snapshots": [
        { "subvolume_id": 1040, "name": "GMT+08_2026-06-01-00-00", "otime": "2026-06-01T00:00:00Z",
          "read_only": true, "pending_deletion": false,
          "exclusive_bytes": 0, "pinned_unreferenced_bytes": 0, "shared_with_live_bytes": 0,
          "unknown_bytes": 0, "qgroup_exclusive_bytes": null }
      ],
      "simulation": { "selector": "older-than:90d", "selected_ids": [1040],
                      "freed_bytes": 0, "unknown_bytes": 0 },
      "manage_in": "Snapshot Replication > Snapshots" }
  ]
}
```

Implementation status: `scan --snapshots [N|all]`, `--owner-budget` (default 64), `--owner-budget-global` (default 100,000), `snapshots`, and snapshot roots in `simulate-delete` are implemented. The census and owner histogram count toward `--max-memory`; pinned bytes are measured for mappable uncompressed ranges and remain `null` when ownership or mapping is unknown. Snapshot management hints use the captured subvolume path, including SAN Manager for roots under `@iSCSI`. Every leaf of each materialized snapshot is read; the generation bound is not used. `simulate-delete` answers a set of snapshot roots in `snapshot_sets`, using the newest report of a `--snapshots` scan that holds them all. It never scans for them. A set that mixes snapshot roots and live paths is answered by scanning the live paths with a deletion census: the live trees of the selection and of each selected snapshot's source are each read as two roots, the selected inodes and the rest. The selected snapshots, and the newest `--snapshots` depth (default 64) of the others, are also read. An extent is freed only when all its references lie in the deleted roots and no unread snapshot may share its leaf. The combined answer is `results[].with_snapshots` (`freed_bytes`, `unknown_bytes`, `unknown_extents`, `snapshots_not_materialized`). Any unknown value exits 3. The snapshot roots in such a set are the read-only subvolume roots and the roots some report analyzed. A snapshot root alone that no `--snapshots` report holds is answered as a path, as before. JSON output nests the per-share answers under `results[].shares` for each target. Its `simulation` also lists `not_materialized_ids`.

#### Answering any snapshot set from the report

A GUI recomputes the freed bytes each time the user toggles a snapshot. A set of only snapshot roots (`snapshots --simulate-delete`, or `simulate-delete` on snapshot roots) is therefore answered from the covering `--snapshots` report alone, never by a scan per selection. The report already holds what that needs, and no report data is added for it:

- **What the report keeps.** Per share, the owner histogram of step 3: bytes and extent counts per (owner set among the roots read, hold), where hold is `resolved` (no other root references the extent), `external` (a root outside the share's census that is not one of its snapshots does) or `unknown`. Per-snapshot `exclusive_bytes` and `held_files` are derived views of the same census; they add nothing for sets. The histogram lives in the report header, so an answer reads only the header (a test NAS, 2026-09-29: `simulate-delete` of snapshot roots from a 301.6 MB `--snapshots` volume report answers in 0.00 s at 3.2 MB RSS; the deletion-census rescan of the same sets agreed byte for byte).
- **Lower bound.** `freed_bytes` = resolved bytes whose owners all lie in the set plus the pending deletions of its shares (the rule above). Deleting the set frees at least that much.
- **Upper bound.** `upper_bound_bytes` = `freed_bytes` + `unknown_bytes` (unknown bytes whose owners all lie in the set) + the `external` bytes whose owners all lie in the set when the set also holds a snapshot of another share. That other share's snapshot may be the external root; a set within one share can never contain it, since the share's own snapshots are never `external`. The same cross-share extent sits in both shares' histograms, so this bound may count it twice; it stays an upper bound. `upper_bound_bytes` is `null` when a selected snapshot was not materialized: what it alone holds was never read.
- **`exact`** is `true` when the two bounds meet and nothing in the set is unmeasured: every selected snapshot was materialized, and no `unknown` or cross-share `external` histogram bucket (by extent count, so extents whose size is not known count too) has all its owners in the set. `inexact_reasons` lists why not, as stable codes: `not_materialized`, `unknown_owners`, `cross_share_sharing`. Exit 3 still follows `unknown_bytes` and `not_materialized_ids` as before; a set that is inexact only through `cross_share_sharing` exits 0, because its `freed_bytes` are proven.
- **`exact_command`** is the rescan that removes what made the answer inexact, or `null` when no scan option does (cross-share sharing, or unknown bytes from missing or relocating roots). It rescans the one share of the set, or the volume when the set spans shares, with `--snapshots all` when a share of the set has unread snapshots, and with `--owner-budget` and `--owner-budget-global` raised fourfold when the report records owner-budget failures. After that scan, `exact` says whether it worked.
- **Why not exact for every set.** Making cross-share sharing exact needs, per `external` bucket, the other roots that reference it, merged across the shares' histograms so an extent is counted once: a volume-wide census keyed by extent, which section 9.1 rules out. It only arises for extents that snapshots of two shares, and no live root, reference (a reflink copy across shares, deleted from both live shares). The mixed-set scan above reads every selected snapshot in one census and answers such a set exactly.

The JSON fields are additive members of `simulation` (both `shares[].simulation` of `snapshots` and `snapshot_sets[].simulation` of `simulate-delete`):

```json
"simulation": { "selector": "id:1040,id:1051", "selected_ids": [1040, 1051],
                "freed_bytes": 0, "unknown_bytes": 0, "not_materialized_ids": [],
                "upper_bound_bytes": 0, "exact": true, "inexact_reasons": [],
                "exact_command": null }
```

#### Files held only by snapshots

`snapshots` also names the files behind "held only by snapshots": per share, the files whose data extents are referenced only by that share's snapshots, largest first. The byte totals above stay the answer; the list says which files they are.

- **What a file is.** A (share, inode) pair. Snapshots of one source keep the source's inode numbers, so one file seen in several snapshots is listed once. Its `path` is its name in the newest read snapshot that references it, relative to the snapshot root. That is where the file was in the share when the snapshot was taken. `live_path` is the same relative path under the share's live root. `in_live` says whether the live share still has that inode: `false` means the file was deleted (or renamed away and then deleted) after the snapshot, and `true` means it still exists but was rewritten, so its old extents stay in the snapshots. A name that cannot be resolved (an ambiguous or broken `INODE_REF` chain) is `null` and the output shows the inode number. Hard-linked files show their first name.
- **Bytes.** `bytes`: the extents of the file whose read owners are all snapshots of the share and whose other references are resolved (SPEC 5.14 steps 2 and 3, `Hold::Resolved`). Each extent counts once per file. `unknown_bytes`: extents of the file referenced by the snapshots alone among the roots read, whose other owners could not be ruled out (owner budget, unread snapshots, missing roots). They are never added to `bytes`. Extents that another root also references (the live share, another share) are not counted. Files share extents through reflinks, so the listed bytes can add up to more than `held_only_by_snapshots_bytes`. On files that share no extents, deleting every snapshot of the share frees at least the sum of the files' `bytes`.
- **Bounded.** The census already keeps one file range per snapshot `EXTENT_DATA` item. Tallying files adds one entry per distinct file with snapshot-only data. At most `--held-files-budget` files are tallied per share (default 100,000; the entries count toward `--max-memory`). Past that limit, files first seen later are not tallied, the list is marked `complete: false`, and the output says to raise the limit. Files that are tallied keep exact bytes. The report keeps the largest 100 per share, ordered by `bytes`, then `unknown_bytes`, then inode number. `snapshots --limit N` (default 10, at most 100) shows the first N. Resolving names reads `INODE_REF` items of those files and their directories only.
- **Where it shows.** `snapshots` shows it per share, human output below the table and JSON `shares[].held_files` = `{"complete": true, "files": [{"snapshot_id": 1040, "snapshot": "GMT+08_2026-06-01-00-00", "path": "photos/a.jpg", "live_path": "/volume1/backup/photos/a.jpg", "in_live": false, "bytes": 0, "unknown_bytes": 0}], "count": 1204}`, where `count` is the number of files tallied. `held_files` is `null` for a report written before the list existed. `why-not-freed` shows the same files under its `held_by_snapshots` row.
- **Privacy.** Paths are local output only. `support-bundle` never includes them. It keeps only its tokenised share and snapshot names.

Report format: the list is an additive field of each share's analysis (`held_files`, serde default), so format-4 readers still load reports without it; no format bump.

Relies on: 4.3, 4.6 (share snapshots are never generic cleanup targets), 8.1 (steps 3 and 5), 10, 13, 22.4. Changes: section 8.1 step 3 owner resolution becomes part of accounting for snapshot targets (it stays optional for `--explain` on live paths); `simulate-delete` accepts snapshot roots (5.6).

### 5.15 `why-not-freed`

```bash
synoreclaim why-not-freed /volume1
synoreclaim why-not-freed /volume1 --since 24h
synoreclaim why-not-freed /volume1 --processes
synoreclaim why-not-freed /volume1 --json
```

Answers "I deleted files (or emptied the recycle bin), why did the free space not grow". It lists every mechanism that keeps deleted data allocated, with bytes where derivable, and says what the user can do about each. It reads the latest volume report plus a few live, cheap probes; it never deletes, syncs, or triggers space reclamation.

Human output (mock-up):

```text
/volume1  free now 1.42 TiB; report 2026-09-28 10:02–10:17 (2h ago)

WHY SPACE IS NOT BACK                                BYTES        WHAT TO DO
Deleted snapshots still being cleaned (2)            18 GiB       nothing: Btrfs frees it in the background
Files deleted from shares, still in snapshots        688 GiB      Snapshot Replication (synoreclaim snapshots /volume1)
  photos: 3 snapshots hold 1,204 deleted files      402 GiB
Recycle bins (moved there in the last 24h: 31 GiB)    212 GiB      Empty Recycle Bin (synoreclaim recycle /volume1)
Deleted but still open (--processes)                    6.1 GiB    restart or stop: synoindexd (pid 8123) holds 2 files
Other names (hard links) of deleted files            unknown      synoreclaim inspect --explain <path> for files that still exist
Btrfs commit not yet done                            —            wait one commit interval (30 s)

For live free-space changes, run `synoreclaim watch-free /volume1 --duration 10m`.
```

Rows and data sources:

| Row | Source | Bytes |
| --- | --- | --- |
| Pending Btrfs cleanup | Root tree: roots with zero refs / orphan items (section 8.1 step 5), live probe at run time; bytes from the report's part 1 (5.13) | report value; roots that appeared after the report show count only, bytes `unknown` |
| Deleted from shares, still in snapshots | For each share, the extents held only by its snapshots (5.13 part 2, 5.14). Named files: entries present in the share's newest snapshot but absent from the live share, which were therefore deleted (or renamed away) after that snapshot was taken; top N by bytes. Needs `scan --snapshots` (5.14). `--since` does not filter this row: the newest snapshot's time is the only bound the data gives, and the row prints it | report value, or `unknown` without snapshot materialization |
| Recycle bins | Each share's `#recycle` (5.4 service summary); `--since D` adds the part whose entry `ctime` is newer than now−D | report value |
| Deleted but open (only with `--processes`) | One-shot best-effort scan of `/proc/*/fd`: links ending in ` (deleted)` whose `fstat` `st_dev` belongs to the volume; `/proc/*/maps` for mapped files. Also orphan-item inodes from the report (`orphan_inode` blocker). Root only; processes shown by pid and command name | `st_blocks × 512` per inode, labelled "allocated, estimate"; joined with the report's orphan inode reclaimable bytes when the inode matches |
| Hard links | Not derivable for names that no longer exist (no history, section 2). For paths that still exist, `inspect --explain` | always `unknown` here |
| Commit not yet done | Mount option `commit=` (default 30 s) | none |

`--since DURATION` is a filter, not history: V1 keeps no time series (section 2). It narrows the recycle-bin row to entries whose ctime falls in the window. Pending subvolume deletions remain unmeasured because the root item has no deletion time; the output marks that row unknown when pending roots exist.

Live free-space observation is the standalone `watch-free` command (section 5.19); `why-not-freed` does not start a second polling loop.

JSON (sketch): `{"command": "why-not-freed", "report_info": …, "reasons": [{"code": "pending_subvolume_cleanup", "count": 2, "bytes": 0, "action": {"kind": "wait"}}, {"code": "held_by_snapshots", "bytes": 0, "items": [...]}, {"code": "recycle_bin", "bytes": 0, "since_bytes": 0}, {"code": "open_unlinked", "bytes_estimate": 0, "holders": [{"pid": 0, "comm": "…", "inodes": 2}]}, {"code": "hard_link", "bytes": null}]}`.

Implemented (V1, 2026-09-29). Where the implementation differs from the table above, this paragraph is what ships:

- Source: the newest complete volume report with overview data (`scan --scope volume`); without one the command fails (exit 1) and names the scan. JSON is `results[].reasons[]` (`schemas/v1/why-not-freed.schema.json`); each row has `code`, its values, `unknown` and `action` (`kind`, `manage_in`, `command`). A seventh row, `unresolved_references`, carries the overview's unknown part by reason: those bytes may belong to any other row.
- Pending cleanup: roots whose root item has zero refs, probed live; a deletion shows only once committed. Reports record their pending root ids (`pending_roots`), so `bytes` is the report's part 1 only while the live set equals the report's; otherwise `bytes` is null with `appeared_after_report` / `cleaned_since_report`.
- `lower_bound`: when the report has unresolved bytes, the pending, snapshot and Recycle Bin rows are minimums (human output prints `≥`). Measured on a loop fixture: bytes held only by a snapshot whose leaves were COWed, and a deleted subvolume's extents, both land in `pinned_ranges_unknown`, so those rows then show 0 with `lower_bound`.
- Snapshot row: the per-share bytes of part 2, snapshot counts, the newest snapshot time when a `--snapshots` scan read it, and where to manage them. Named files (2026-09-29) are the share's files only snapshots hold (5.14 "Files held only by snapshots"), not a namespace diff of the newest snapshot against the live share. They include rewritten files (`in_live: true`) as well as deleted ones, and they appear only when the volume report is itself a `scan --scope volume --snapshots` report: each share item's `files` (`null` otherwise), at most `--limit` per share (default 10, at most 100). A share whose snapshot-held bytes the volume report left unattributed (COWed leaves, see `lower_bound`) still gets an item, with `bytes: 0`, when the census named its files. Human output lists them under the share with `(deleted)` or `(rewritten)`, and says how many more were tallied and whether the tally was complete. `support-bundle` omits them.
- `--since` measures the Recycle Bin portion from `INODE_ITEM.ctime`; any Recycle Bin bytes without ctime keep `since_bytes` null. Pending subvolume deletion time is not measured, so a non-empty pending row is unknown for `--since` and warning `since_not_measured` is emitted.
- Deleted but open: `report_orphan_bytes` is the `orphan_inode` blocker class, which counts only extents shared with a scanned scope; extents only an unlinked inode references show as `direct_owner_unresolved` in the unresolved row. `--processes` reads `/proc/*/fd` and `/proc/*/map_files`: links ending in ` (deleted)` to regular files with `st_nlink == 0`, opened `O_NONBLOCK`, whose `FS_INFO` fsid is the volume's; `st_blocks × 512` per distinct inode. Unreadable processes are counted and make the row unknown.
- Exit: 0 when every row is known, 3 when any row is unknown (hard links excepted), 1 without a report.

Relies on: sections 3 (external blockers), 5.7 (`/proc` scan, `--measure`), 5.13 part 1, 5.14, 8.1 step 5. Changes: the `/proc` scan becomes available outside cleanup, opt-in with `--processes` (still one-shot, advisory). Without the flag the row shows the report's `orphan_inode` bytes only and names the flag.

### 5.16 `recycle`

```bash
synoreclaim recycle /volume1
synoreclaim recycle /volume1/photos
synoreclaim recycle /volume1/photos --oldest 20
```

Per-share recycle-bin size and reclaimable bytes, with an optional list of oldest items.

```text
RECYCLE BIN (#recycle)      ITEMS     SIZE      CAN FREE   OLDEST       HELD BY
photos                      8,412   188 GiB     186 GiB   2025-11-03   hard link 2 GiB
backup                        311    22 GiB       1 GiB   2026-02-17   snapshot 21 GiB
Emptying in DSM: Control Panel > Shared Folder > Action > Empty Recycle Bin, or a Recycle Bin task in Task Scheduler. Reclaim never empties a Recycle Bin.
```

- Sizes come from the volume or share report (the `#recycle` lines of the 5.4 service summary); `CAN FREE` is section 10 reclaimable for deleting the whole `#recycle` content of that share together; `HELD BY` as in 5.4.
- `--oldest N` lists up to N oldest top-level items (direct children of `#recycle`) by entry `ctime`, with their full paths, size and reclaimable bytes. Entry age uses `INODE_ITEM.ctime`; the format-4 report stores it only for entries inside `#recycle`. On a test NAS, read-only observations of existing Recycle Bin items showed ctime later than mtime; a Btrfs rename fixture on a Linux build host advanced ctime while preserving inode and mtime. This supports the rename-based age estimate, but it is not an authoritative DSM delete timestamp. Items missing ctime sort last and return `null` with a warning.
- Which report answers: each share's row comes from the newest report that covers that share. For a volume target the answer is the newest volume report, and a share scanned on its own after it (a report whose roots are only direct shared-folder roots of the volume, newer than the volume report; snapshot-deletion answers never count) lists its Recycle Bin from that newer share report instead, including a share the volume report does not know. Such a row carries its own `report_info`; rows from the volume report carry none. `--report <JOB_ID>` answers from the report of that scan job only (it must cover the target, else `no_report`), so a volume target then lists exactly what the volume report saw. A share target always answers from the newest report covering it, which is the share report after a later share scan.
- `--older-than` and cleanup-plan generation are not implemented.
- Refuses (exit 4) when the target is not a volume or a direct shared-folder root, or the share has no `#recycle` or it is not the share's real Recycle Bin (section 5.7 rule). With `--json` the refusal is the standard error envelope (`completion: "failed"`, `errors[].code`, empty `shares`) and exit 4, with nothing on stderr: `not_a_shared_folder` (not a volume or direct shared-folder root) or `no_recycle_bin` (no real `#recycle`). Without `--json` the same text goes to stderr.

- `moved_by_reclaim` lists, per shared folder, what Reclaim's cleanup moved into its Recycle Bin after the answering report's scan (`items`, `bytes`, `last_moved`), from `<state-dir>/cleanup/moved.json`: not yet in the rows' sizes until a rescan.

JSON: `{"command": "recycle", "report_info": …, "shares": [{"share": "photos", "items": 0, "bytes": 0, "reclaimable_bytes": 0, "held_by": [...], "oldest": [{"path": {...}, "ctime": "…", "bytes": 0, "reclaimable_bytes": 0}], "report_info": …}], "moved_by_reclaim": [{"share": {...}, "items": 0, "bytes": 0, "last_moved": "…"}]}` (`oldest` only with `--oldest`; a row's `report_info` only when a newer share report answers for it); a refusal instead carries `report_info: null` and the error code above.

Relies on: 4.6, 5.7 (Recycle Bin items), 10. Changes: the report keeps ctime for `#recycle` entries.

### 5.17 `support-bundle`

```bash
synoreclaim support-bundle /volume1 --output bundle.json
synoreclaim support-bundle /volume1 --include-paths --output bundle.json
```

One JSON document to attach to a ticket, replacing the usual SSH-plus-`du` session. Read-only; it uses the existing report and runs no scan (it records `report_missing` instead).

Contents, in order: tool version, schema version, DSM version/build, model family, kernel, architecture; `volumes`; `doctor` checks; the `overview` document; the top 50 of `top --by reclaimable` including aggregated service rows; `why-not-freed` without `--processes` (per row: `code`, `unknown`, `lower_bound`, `bytes`, pending `count`, share items as tokens without held-file names, unresolved `parts`); `snapshots` summaries if materialized; `change_reasons`, coverage, warnings, the last scan's timings and peak memory, and the active job states.

Redaction (default on):

- Share, directory, file, snapshot, and process names become stable tokens within the bundle (`share-01`, `dir-0042`, `file-0107`), assigned by first appearance; the mapping is never written anywhere. The same name always gets the same token inside one bundle, and tokens do not correlate across bundles.
- Names from the fixed DSM table (service-table names, `#recycle`, `#snapshot`, `@eaDir`, `@sharesnap`) stay readable, because they are the diagnosis.
- Never included: hostnames, IP or MAC addresses, serial numbers, user names, uids beyond 0, file contents, xattr values, `syno.*` metadata, kernel logs (`dmesg`). Filesystem and subvolume UUIDs are replaced by tokens too. Device paths (`/dev/...`, such as `/dev/mapper/cachedev_0`, `/dev/sda1` or `/dev/md2`) in any string of the bundle, for instance in `pinned_leaf_reader_unavailable`, become `device-01` tokens, even with `--include-paths`.
- `--include-paths` keeps real names (opt-in; prints a warning that the file then contains path names).

Size bound: `--max-bytes` (default 1 MiB). When the bundle would exceed it, drop detail in a fixed order (top rows beyond 20, snapshot rows beyond 10 per share, doctor detail text) and record what was dropped; never truncate mid-document. The file is created 0600; `--output -` writes to stdout.

Exit codes: 0 written; 3 written with missing parts (no report, no snapshots); 1 cannot write.

Relies on: 5.2, 5.4, 5.10, 5.13–5.15, 14, the existing sanitiser rules for golden recordings. Changes: none to other commands.

### 5.18 `drive` (lower priority)

```bash
synoreclaim drive /volume1
synoreclaim drive /volume1 --json
```

Splits Synology Drive's storage (`@synologydrive`, the Drive version repository) by the shared folder or team folder it belongs to, using directory structure only: no Drive database reads, no package API, no file contents.

```text
SYNOLOGY DRIVE (@synologydrive)   905 GiB, can free — (manage it in Drive Admin Console)
FOLDER (team folder / My Drive)       VERSIONS DATA    CAN FREE BY DSM
photos (team folder)                      612 GiB     up to 612 GiB held: lower version count
homes: user-07 (My Drive)                 201 GiB     up to 201 GiB held
database and index                         48 GiB     —
unattributed                               44 GiB     unknown
```

- The layout (for example `@synologydrive/@sync/repo/…` and how repo entries map to team folders or home folders) is **unverified** and differs between Drive versions. Mapping rules live in one versioned table keyed by the Drive package version read from its package metadata; an unknown version shows only the total and `unattributed`.
- All values are `held`: deleting versions is done in Drive Admin Console, and cleanup refuses Drive data (section 5.7). No reclaimable value is claimed for Drive data.
- *Verify first*, in the last round: a read-only listing of the `@synologydrive` directory structure on a test NAS (approved by the user, 2026-09-28; no file contents, names sanitised in anything saved) confirms the layout for the installed Drive version before implementation.

Relies on: 4.6 (service table), 5.4 (service summary), 5.7. Changes: none.

### 5.19 `watch-free`

```bash
synoreclaim watch-free /volume1
synoreclaim watch-free /volume1 --duration 15m --interval 5s
synoreclaim watch-free /volume1 --duration 30s --interval 5s --json
```

Observe a Btrfs volume after a delete or other change. The target must be an exact mounted Btrfs volume root, not a share or arbitrary path. The command opens it read-only and takes an immediate sample, then samples at `--interval` (default 10 s, range 2–60 s) until `--duration` (default 10 m, range 2 s–1 h). The interval cannot exceed the duration. `--profile low|normal` controls root-tree probe pacing and defaults to `low`. A monotonic clock bounds the watch window; Ctrl+C prints the samples collected so far and exits 130.

Each sample contains:

- `data_used_bytes`: the sum of `used_bytes` for `BTRFS_IOC_SPACE_INFO` block groups with the DATA flag;
- `free_bytes`: `statfs` free bytes for the filesystem;
- `pending_deletion_roots`: the number of root-tree entries with zero references, which are pending Btrfs cleaner work. This is a count only; the command cannot tell whether the cleaner is actively running or how many bytes those roots will release.

Human output shows the first and last successful values, their signed deltas, and the pending-root status across the window. JSON emits one final document with the standard envelope (`schema_version`, `command`, `tool_version`, `generated_at`, `completion`, `job_id`, `errors`, `warnings`), sample `elapsed_ms`, the three counters, `data_used_delta_bytes`, `free_delta_bytes`, `pending_deletion_roots_delta`, `concurrent_writers_may_affect_delta: true`, and per-field probe errors where a value is unavailable. `completion` is `complete` (exit 0), `partial` when a failed initial or final space probe leaves a delta unknown (exit 3), or `paused` on a stop request (exit 130). Missing deltas remain `null`; unknown cleaner status is not presented as zero. The concurrent-writer label means the observation does not attribute a change to the preceding delete.

`watch-free` never deletes, syncs, starts reclamation, scans namespace contents, reads a report, writes state, or takes the filesystem writer lock. It uses the same sampler and delta representation as cleanup's optional `--measure` (section 5.7). Probe errors are reported as unknown; a failed initial or final space probe leaves that delta unknown and exits 3.

### 5.20 `history`

```bash
synoreclaim history
synoreclaim history --json
```

One entry per completed scan or finished scan job, newest first, from `<state-dir>/history/` and the job manifests. A cleanup or rootfs apply is not a scan and is not listed; `jobs` lists it. `history` never starts work, creates the state directory, or reads a report's records; a missing or empty state directory yields no scans.

Each scan has:

- `job_id`, `targets` (the scan's roots: a volume mount or share paths), and `volume`;
- `profile` (the resource profile the run used; null when not recorded);
- `started`, `finished`, and this run's `elapsed_ms`;
- `total_elapsed_ms`: all runs of the job so far, earlier resumed runs included;
- `entries` (namespace entries read), `subvolumes`, and `reused_units` (units this run answered from the cache);
- `full_read_ms` and `full_read_entries`: the last run of this scope that read without reusing anything, the basis an ETA should use. A reuse-heavy or resumed run records its own duration but inherits this basis instead of replacing it, so it cannot skew the ETA (C9 of an earlier survey). Both are null until a full read is recorded;
- `outcome`: `completed`, `partial` (with `reason`), `cancelled`, `failed`, `paused` (a paused or interrupted job), or `superseded`;
- `peak_rss_bytes` and `spill_peak_bytes`;
- `report`: the report path, only while its file still exists, and `report_bytes`. Cleanup deleting reports keeps the scan and its space numbers; the report path is then omitted;
- `fsid`: filesystem identity from the persisted history filename; omitted for job-only entries. `original_outcome` preserves `completed` or `partial` before a record is marked `superseded`; omitted on legacy superseded records, whose original completeness is unknown. These additive v1 fields let the UI retain the last complete whole-volume df baseline even after a partial scan replaces its report.
- `df_total_bytes`, `df_used_bytes`, and `df_free_bytes`: the filesystem's bytes at the scan's end (statvfs, as `volumes` reports them; `df_used_bytes` is `total - free`), null when the scan could not read them. `data_used_bytes` is the Btrfs DATA block-group usage at the same instant, the overview reconciliation number, null when unavailable. They feed the UI's used-space trend (GAP-3).

The records of a scan are written under `<state-dir>/history/<fsid>.json` when it publishes. The newest 64 scans per filesystem are kept. A newer scan of the same scope marks the older records `superseded` (their report was replaced) and drops their `report` path. A job that ends without publishing a report (cancelled, failed or paused) contributes a job-derived scan from its manifest, without scan statistics. A job that failed with reason `busy` left no work and is not listed.

### 5.21 `webapi` (DSM package entry point)

```bash
printf '%s' '{"target":"/volume1/share","kind":"all","limit":200}' \
  | synoreclaim webapi top.list --requested-by admin
```

The only command the DSM package's WebAPI shim runs. The shim passes a fixed argv, `webapi <method> --requested-by <DSM login>`, and the params object unchanged on stdin; all validation and argv building happen here.

- stdin holds exactly one JSON object, at most 16 MiB. `--requested-by` is 1 to 256 bytes without control characters.
- The state directory is fixed: `state/` below the real path of `/var/packages/SynoReclaim/var` (`$SYNOPKG_PKGVAR`). If that link does not resolve, the call fails with exit 1 before anything runs.
- Each method has an allowlist of params with `deny_unknown_fields`, types, ranges and list limits. A path param is a string or the `{path, path_hex?}` object the CLI prints (`path_hex` wins when present). It must be absolute, at most 4096 bytes, with no NUL and no empty, `.` or `..` segment. A `volume` param must be the mount of a local Btrfs volume (as `volumes` lists them); any other path must be that mount or below it. Job IDs are 1 to 64 ASCII letters, digits and `-`, not starting with `-`.
- The call runs the mapped command in-process and prints that command's own JSON document with its own exit code. User values only ever become operands after `--` or `--flag=value` words, never flags of their own; no shell is involved.
- An unknown method or a param that breaks the allowlist prints a `webapi` document (`schemas/v1/webapi.schema.json`: envelope with `completion: "failed"`, plus `method`, `outcome: "invalid_param"`, `message`) and exits 2.

| Method | Params | Runs |
| --- | --- | --- |
| `session.get` | – | answered in-process: `session` document with `requested_by`, `cleanup_enabled`, `mutations_disabled` from the trusted package config of 5.7, read by the unprivileged package user (both false without one) |
| `volumes.list`, `jobs.list`, `history.list` | – | `volumes` / `jobs` / `history --json` |
| `doctor.run`, `overview.get`, `cache.status`, `cache.clear` | `{volume}` | `doctor --json`, `overview --json --no-scan`, `cache status\|clear --json` |
| `why_not_freed.get` | `{volume, processes?: bool}` | `why-not-freed [--processes] --json` |
| `watch_free.sample` | `{volume}` | `watch-free --duration=2s --interval=2s --json` |
| `support_bundle.get` | `{volume}` | `support-bundle --output=-` (paths redacted) |
| `top.list` | `{target, kind: all\|dirs\|files, by?: reclaimable\|logical\|physical\|shared\|held, limit?: 1..200}` | `top --kind= [--by=] [--limit=] --json --no-scan` |
| `inspect.get` | `{paths: 1..100}` | `inspect --explain --json --no-scan` |
| `simulate.get` | `{paths: 1..1000}` | `simulate-delete --json --no-scan` |
| `cleanup.plan {stage}` | `{stage: {selection?, paths?, groups?}}`: one chunk of a selection, per-path `expect` (5.7); the App's `cleanup.select` sends it, so the root program's methods stay the same | `cleanup select --json [--selection=] --chunk=`, answered at once |
| `cleanup.plan` | `{paths?, duplicates?, selection?}`, optional per-path `expect` (5.7); no count limit beyond the request size | `cleanup plan [--selection=] --json --quiet` in a planning job |
| `snapshots.list` | `{target, simulate?: selectors}` | `snapshots [--simulate-delete=] --json`; the selectors must parse as for the CLI |
| `recycle.list` | `{target, oldest?: 1..1000}` | `recycle [--oldest=] --json --no-scan` |
| `scan.start` | `{targets: 1..256, mode: fast\|gentle, snapshots?: bool}` | `scan --detach=none --profile=fast\|low [--snapshots] --requested-by=<user> --json --quiet` |
| `job.pause`, `job.cancel` | `{job_id}` | `pause` / `cancel --json` |
| `job.resume` | `{job_id, mode?: fast\|gentle}` | `resume --detach=none [--profile=] --json` |
| `job.result` | `{job_id}` | `jobs --result=<id> --json` |
| `cleanup.apply` | `{plan_id, confirm_move: true, confirm_delete?: true}` | `cleanup apply --detach=none --yes --requested-by=<user> [--confirm-delete] --json -- <state>/cleanup/plans/<plan_id>.json` |
| `rootfs.apply` | `{plan_id}` | `rootfs apply --detach=none --yes --requested-by=<user> --json -- /var/lib/synoreclaim/rootfs-plans/<plan_id>.json` (the UI never sends a path) |
| `rootfs.history` | – | `rootfs history --json` |
| `duplicates.analyze` | `{volume}` | `duplicates analyze --json -- <volume>` as a planning job (one per volume; a second request joins it) |
| `duplicates.scope` | `{volume, path?, list?: files\|folders, category?: photos\|videos\|music\|other, offset?, limit?: 1..200}` | `duplicates scope [--path=] [--list=] [--category=] [--offset=] [--limit=] --json -- <volume>` |
| `duplicates.copies` | `{volume, group_id, scope?, offset?, limit?: 1..200}` | `duplicates copies --group= [--path=] [--offset=] [--limit=] --json -- <volume>` |
| `duplicates.keep` | `{volume, group_id, path, keep: bool, scope?}` | `duplicates keep --group= --copy= --keep=on\|off [--path=] --json -- <volume>` |
| `duplicates.preferred_shares` | `{volume, shares?: [name…] (≤ 64)}` | `duplicates preferred-shares [--shares=<JSON list>] --json -- <volume>` |
| `setup.cleanup` | `{mutations_disabled: bool}` | `config set --mutations-disabled=on\|off --json` (no state directory) |

Every command also gets `--state-dir` (except `watch-free`, which has none). Sync answers never scan: answer commands run with `--no-scan`.

Launcher contract (`scan.start`, `job.resume`, `cleanup.apply`, `rootfs.apply`): the call returns within the time it takes to resolve the targets and check the job, and prints one `detach` line (`schemas/v1/detach.schema.json`) with `job_id`, `launcher: "none"`, `run` and `result`. The package's launcher must then:

1. run `run` exactly as given, as argv without a shell, as the package's job unit (package user; the helper raises privilege only for an authorized grant), outside the caller's cgroup (synoscgi kills its cgroup on restart), e.g. as an instance of a template unit (the DSM package's `pkg-SynoReclaim-job@<job_id>`; DSM has no `systemd-run`), with stdin from `/dev/null` and its output discarded or logged; it does not wait for it;
2. take the job's progress and end from `jobs.list` (`state`, `result`) and its whole document from `job.result`;
3. if it cannot start `run`: `job.cancel` a queued scan or apply (it would otherwise stay `queued`; an apply never started deletes nothing); a resumed job simply stays paused or interrupted.

A refused start prints that command's refusal instead and no job is queued or changed: `scan.start` a failed or busy detach line (exit 1 or 4, `job_id` null), `job.resume` the job-command refusal (`busy`, `finished`, `not_started`, `job_incompatible`, `no_job`) or a busy detach line with `outcome: scan_in_progress` and `running_job_id`, `cleanup.apply` the `cleanup-apply` failure document. `--requested-by` is kept in the queued job's `job.json` and shown as `jobs --json` `requested_by`.

UI cache. When a job that has `requested_by` and is ending `completed` or `partial` scanned a whole volume, it runs two more commands before it exits (`scan --job-id` and `resume` alike). They are `top --kind=all --limit=200 --json --no-scan` and `overview --json --no-scan` of that volume, with the argv `top.list {target: <mount>, kind: "all", limit: 200}` and `overview.get {volume: <mount>}` map to. Each run's stdout is saved when it exits 0 or 3, as a private file in `<state>/ui/cache/`. Later, a `webapi` call whose mapped argv is byte-for-byte the same prints that saved stdout and exit code instead of running the command, as long as all of these hold:
- the volume at that mount is still the same filesystem;
- the report directory of the filesystem (names, sizes, modification times, inodes) is what it was before the answer was computed;
- none of its reports would yet be stale under the default `--max-age` of 24 h.

A new scan, a pruned report or a cleanup therefore invalidates the entry. A served answer is the document as it was printed: its `generated_at` and `age_seconds` are from the time it was computed. Prewarming is best effort; a failure leaves the call to be computed when asked.

### 5.22 `uninstall-cleanup`

```bash
synoreclaim uninstall-cleanup --json
synoreclaim uninstall-cleanup --delete-state --json
```

For the package's `preuninst` script, run after every job is stopped (as the package account). It removes the package config `config.json`, always. With `--delete-state` it also removes the package state directory `state/` in the real path of `/var/packages/SynoReclaim/var` (5.21): reports, cache, jobs, plans, logs and the UI cache. The package data folder itself is left for DSM. A path that is a symlink loses only the link. Every path is tried even when an earlier one fails.

`--json` prints an `uninstall-cleanup` document (`schemas/v1/uninstall-cleanup.schema.json`):
- `removed`: the paths removed;
- `absent`: paths that were already gone (for a missing package data folder, the folder's path);
- `delete_state`;
- a `remove_failed` error for each path it could not remove (`completion: "failed"`, exit 1).

Otherwise the exit code is 0. The script logs a failure and goes on; it never fails the uninstall.

### 5.23 `rootfs`

```bash
synoreclaim rootfs [--json] [--profile low|normal|fast]
synoreclaim rootfs plan --to FOLDER [--json] [--profile …]
synoreclaim rootfs apply PLAN --yes [--detach[=self|none]] [--json] [--profile …]
synoreclaim rootfs history [--json] [--state-dir DIR]
```

What fills DSM's system partition `/` (md0, ext4), and moving DSM's own leftovers off it into a shared folder. All three commands require root (else `not_root`, exit 4). They never touch a data volume except the `--to` folder and never scan Btrfs. Their only state is root's plan folder `/var/lib/synoreclaim/rootfs-plans` (on `/`, 0700). There are no aliases. There is no switch to turn them on: they are always available to root, and only the kill switch `mutations_disabled` stops `rootfs apply`.

#### `rootfs`: the report

This command is read-only. It reports:
- the df-style space and inodes of `/`;
- DSM's ext4 project quota on `/var/log` (`log_quota`, null when there is none);
- the largest first- and second-level paths (`usage`) and the bytes by owner (`owners`);
- what `rootfs plan` could move (`cleanable`), and what `rootfs plan --mode deep` could move (`cleanable.deep`, Standard's items included, with `additional_bytes` beyond them and the tools a deep plan would run). These are estimates, not promises: each is its mode's allowlist after plan's size, age, boot, pinned-file and (deep) Hyper Backup limits (below), but before its held-open check, so a plan may move less, and apply less again (what changed or was opened since the plan stays). The report walks once with the deep table; its owners and advice keep the Standard table. `required_destination_bytes` adds the 256 MiB margin;
- the recognised owners it never selects, each with advice (`report_only`): `/root`, `/usr/local`, package data (`@appstore`, `@appdata`, `/var/packages`), Log Center's `.SYNO*DB`, and the GeoIP database;
- deleted files that processes still hold open (`open_deleted`), with advice per holder.

The walk covers `/` alone: every other mount point is `not_walked` (`mount_point`). It is paced by `--profile` (default `low`), stops at 2,000,000 entries or depth 128, and does not follow symlinks. A walk cut short reports `walk.complete: false` with a `stop_reason`.

#### `rootfs plan --to FOLDER`: the plan

This command reads FOLDER and `/` and changes neither. FOLDER may be a shared folder root or a folder below one on a data volume. It writes one file, the plan `<plan_id>.json`, into root's plan folder `/var/lib/synoreclaim/rootfs-plans`, never into FOLDER: a plan in a shared folder could be edited or forged by the share's users. The plan folder is created 0700 if missing and must be a folder on `/`, owned by root, that no one else may enter, reached through no symlink (else `plan_dir_untrusted`). The plan file is created new (never replacing one, never through a symlink), set to 0600 through its descriptor, and synced. Refusals (exit 4):
- `destination_not_directory`: FOLDER is not an existing folder.
- `destination_not_in_share`: FOLDER is not a shared folder root or a folder below one, on a mounted data volume. Shared folders are the ones DSM registers for that volume (`synoshare --enummnt`), on Btrfs and ext4 alike.
- `destination_on_system_partition`: FOLDER is on the device of `/` itself, for example a volume that is not mounted.
- `destination_protected`: FOLDER is below a `@…` or `#…` folder, such as the Recycle Bin or snapshots.
- `destination_full`: the moved bytes plus a 256 MiB margin do not fit in the volume or the share quota. A file counts as its apparent or allocated size, whichever is larger. When the share quota is the limiting resource, the error has `limit: "share_quota"`.
- `destination_quota_unavailable`: a configured share quota could not be read consistently, so no plan was written.
- `plan_dir_untrusted`: the plan folder is not root's private folder on `/`.
- `walk_stopped`: the walk stopped early (paused, cancelled or a limit).
- `processes_unreadable`: open files are unknown.
- `dsm_update_active`: see below.

When Btrfs quota is enabled and the destination share has a maximum referenced-byte limit, planning reads its qgroup usage and limit without changing quota settings. It refuses if the quota scan is inconsistent or the share quota has less room than the moved bytes plus the margin. A share without a quota is checked against volume free space.

The plan is immutable. It carries:
- its id, created and expiry times (24 hours);
- the device of `/`;
- the destination (path, share, device, inode);
- each item's path, kind, category, allocated bytes and identity (device, inode, type, size, mtime with nanoseconds, link count, and inode change time);
- the DSM tools to run;
- a checksum. The checksum guards against corruption, not tampering; what guards against tampering is that apply reads plans only from root's private plan folder.

With nothing to move and no tool to run, no plan is written. `plan_file`, `plan_id`, `expires_at` and `apply` are then null.

**Allowlist.** Only these paths are ever planned. Each is a case where support found removal both safe and effective:
- `/upd@te/**`, `/.SynoUpgrade.tar`, `/.SynoUpgradeBootupSmallUpdateFiles/**`, `/@autoupdate/**`: leftovers of an interrupted DSM update (KI#48627).
- `/.log.junior/logs.tar.*` (KI#83238).
- `/var/log/**/*.xz` and `/var/log/healthtest/dhm*`, and the disk latency archives `/var/log/disk-latency/.SYNODISKLATENCYDB.<n>.xz` (only these exact names; the live `.SYNODISKLATENCYDB` stays).
- `/var/crash/**`.
- `/var/tmp/etilqs_*` and `/var/tmp/convert_thumbnail_*` (KI#46779, KI#78323).

**Never touched.** The following are never planned. The names among them (`.upd@te`, `space-preserve-for-configs`, `@appdata`, `.SYNO*DB` and the like) are refused at any depth, even below an allowlisted path:
- `/usr`, `/etc`, `/bin`, `/sbin`, `/lib*` and `/boot`;
- `.upd@te` and `.upd@te_sub`, the user's original DSM configuration;
- `/SynoUpgrade*.tar.gz` and `/SynoUpgrade*.txz`, developer evidence of a failed junior-mode update;
- `space-preserve-for-configs` and `winbindd_idmap.tdb`;
- `@appstore`, `@appdata` and `.SYNO*DB` (other than the disk latency archives above);
- the GeoIP database (report only);
- `/var/lib/synoreclaim`.

The plan also leaves out symlinks, anything that is not a regular file or folder, a regular file with more than one hard link (removing one name would not free it, and the other name may be anywhere on `/`), and a folder above a file that must stay. The top update folders themselves (`/upd@te`, `/.SynoUpgradeBootupSmallUpdateFiles`, `/@autoupdate`) are never planned, only what is in them: DSM's update code expects them to exist.

**Size and age limits.** These allowlisted entries stay on `/`. They are listed in `kept[]` with a `reason`, not in the plan:
- `updated_since_boot`: an update leftover (file or folder) other than a junior log whose mtime or inode change time is at or after the last boot (`btime` in `/proc/stat`). It may be an installed update waiting for the restart, which DSM applies from these folders. Junior logs are exempt: DSM renames every `logs.tar.N` at each boot, so all of them change then (a test NAS: all 24, ctime btime + 46 s).
- `too_large`: a file larger than 8 GiB. Apply copies each file before removing it, and one file that size is better moved by the administrator.
- `small_junior_log`: a `/.log.junior/logs.tar.*` under 1 MiB (apparent size). It frees little, and support reads it when an update failed.
- `recent_log_archive`: a `/var/log/**/*.xz` modified in the last 7 days. The last week of rotated history is what support asks for first; the old archives are the ones that pile up. Archives under `disk-latency/` and `healthtest/` are exempt, as an earlier survey had them.
- `append_only` and `immutable`: an entry with `chattr +a` or `+i` (`FS_IOC_GETFLAGS`), and anything below an immutable folder. The kernel refuses to unlink it, so apply could only copy it and fail (`unlink_failed`). DSM sets `+a` on `/var/log/rm.log*` (a test NAS: `rm.log.1.xz` to `.3.xz`).

**Held open.** A planned file that a process holds open is left on `/` and listed in `held_open[]` with its holder. The folders above it stay too.

**DSM's tools.** The plan also names the DSM tools that apply runs after the moves. Each has a fixed argv, runs without a shell, and is chosen when the plan is made:
- `rotate_logs` (`/usr/syno/bin/synocleaner --rotate-log`) and `delete_journal` (`… --delete-journal`), on DSM builds from 25423 (DSM 6.2.3) on;
- otherwise `logrotate` (`/usr/bin/logrotate -f /etc/logrotate.conf`);
- then `restart_syslog_ng` (`/usr/syno/bin/synosystemctl restart syslog-ng`), only while syslog-ng holds deleted files on `/`.

`--mode deep` adds what docs/rootfs-cleanup-modes.md lists, among them `delete_log` (`/usr/syno/bin/synocleaner --delete-log`) first, where synocleaner rotates logs: it permanently deletes the logs DSM finds older than it keeps, as DSM does before an update. Its plan estimate is what `synocleaner --dry-run --delete-log` lists (it deletes nothing) on `/`, less the paths the plan moves.

A program is used only if it is a regular file owned by root, executable, and writable by no one else. `synocleaner --delete-old-core --volume` is left out: `--volume` deletes core dumps on the data volumes, so it frees nothing on `/`.

#### `rootfs apply PLAN --yes`

Apply is gated, in this order:
1. `not_root`.
2. `plan_untrusted`: PLAN is not `<plan_id>.json` directly in `/var/lib/synoreclaim/rootfs-plans`, is a symlink, is not a regular file of root's that no one else may read or write, or holds another plan than its name says. `plan_dir_untrusted`: the plan folder is not root's private folder on `/`. `plan_invalid`: the file does not parse.
3. `yes_required`: `--yes` is missing.
4. `mutations_disabled`: the kill switch is on in the package configuration. There is no other switch. The kill switch fails closed: only a missing package `config.json` counts as off, and one that exists but is not the package user's private file in a folder only the package user can write, or does not parse, refuses with `config_untrusted`.
5. `plan_invalid` or `plan_stale`: a bad checksum or format, an item no longer on the allowlist, or an expired plan.
6. `dsm_update_active`.
7. The destination checks again (`destination_not_in_share`, `destination_on_system_partition`, `destination_protected`).
8. `processes_unreadable`: apply reads `/proc` once (see **Held open** below) and a process could not be read.
9. `root_changed`: `/` is another device. `destination_changed`: the destination is not the planned folder. `destination_full`: the files still to move (those still at their planned path with their planned identity) plus the 256 MiB margin no longer fit, so a rerun after a stop needs room only for what is left.

After the destination checks, Apply takes the NAS-wide rootfs writer lock and a mutation subtree lock on its dated move folder. It holds both through the entire apply. Read-only scans can run concurrently; moved files may only appear in the next scan result. Cleanup apply keeps its existing filesystem writer lock and additionally holds mutation subtree locks on every selected root. Ancestor locks are shared, selected roots are exclusive, and all locks are non-blocking and acquired in sorted order. Disjoint cleanup targets can run while rootfs apply copies into a new folder; equal, parent or descendant targets conflict in either admission order. The rootfs writer lock, private sealed plans, manifest lock, no-follow destination descriptors, quotas, free-space reserve and per-file identity checks are unchanged. Locks are non-blocking: a conflict is `busy` (exit 4) and its error includes `holder: {command, job_id}` when recorded. The CLI checks locks before queuing a detached job and the worker checks them again before changing anything. Apply also holds an exclusive lock (`flock`) on the folder's `manifest.jsonl` for the whole run; a second apply of the same plan meanwhile refuses with `apply_running` before changing anything, since the two would share one `.partial/copy`.

Apply moves each item into `<destination>/SynoReclaim-system-YYYYMMDD-HHMM/`, using the plan creation time in UTC to the minute. It writes the plan id as the first line of `manifest.jsonl` and a `README.txt` listing the escaped planned original paths before moving anything. Items stay at their paths below `/`: files first in path order, in batches of at most 64 files or 64 MiB, then folders deepest first. It runs at `--profile`'s CPU and I/O priority (for `low`, nice +10 on the inherited value, at most 19, and the idle I/O class) but never sleeps to pace itself: it moves at most a few thousand small files, and the 25% duty cycle of the other commands made it take about 16 s per file on a test NAS.

With `--detach` (the WebAPI always uses `--detach=none`), Apply is a `rootfs-apply` job started by the package launcher in its restart-safe systemd unit, outside synoscgi's cgroup. Closing the UI window or restarting synoscgi does not stop it. Its `jobs --json` progress is `rootfs: {phase:"moves"|"tools", items_done, items_total, bytes_done, bytes_total, tool}`. Progress is written at most every two seconds and at the end. `cancel` stops at the next completed move batch or DSM-tool boundary and the result has `completion:"paused"`; rerun the same plan to continue in its existing move folder. The job itself cannot be resumed.

**Privacy.** On a DSM ACL share a new folder or file inherits the share's ACL whatever mode it is created with (a test NAS, DSM 7.4). Apply therefore sets each folder it creates to 0700, and each copy, `README.txt` and `manifest.jsonl` to 0600, through the descriptor (`fchmod`), which makes it a Linux-mode entry without that ACL. It then checks each one: owned by root, no group or other mode bits, and neither `system.syno_acl_self` nor `system.syno_acl_inherit` reads back. If one fails, apply stops. Each folder it creates is synced with its parent. Residual limit: a member of the share's groups can open a new folder in the moment between `mkdirat` and `fchmod`. What they can do there is limited: anything they create is not root's, so the privacy check on the next copy or folder stops the run; they cannot make apply replace or remove a file of theirs.

For each file of a batch it:
1. opens the file's parent folders and the file without following symlinks;
2. skips it as `held_open` if the start's `/proc` snapshot lists it (see **Held open** below), and checks the planned identity (skip `missing`, `changed` or `path_changed`), including the link count and inode change time;
3. checks that the move folder's filesystem has the file's bytes plus the 256 MiB margin free (else `no_room`);
4. copies it into `.partial/copy` in the move folder, hashing it on the way. Only the file's data ranges are copied (`SEEK_DATA`/`SEEK_HOLE`), so its holes stay holes; they hash as zeros. The copy is not synced yet (step 7). A copy that runs out of space (ENOSPC, EDQUOT) is removed and the item skipped as `no_room`;
5. checks that the source's identity did not change during the copy (`changed_during_copy`), and re-reads the copy and checks its size and SHA-256 (`copy_mismatch`); a failing copy is removed and the original kept;
6. renames the copy to its final name without replacing anything (`renameat2(RENAME_NOREPLACE)`). Where that rename is missing (ENOSYS: Linux before 3.15, such as DSM's 3.10; EINVAL: a filesystem without the flag) it hard-links the copy to its final name instead (`linkat`, which also fails with EEXIST rather than replacing) and removes `.partial/copy`; both names are in the move folder, on one filesystem. A copy that cannot take its final name is removed from `.partial/` and the run stops (`io_error`), the original kept. Cleanup has no such fallback (section 5.7): it also moves folders, which cannot be hard-linked, so there it stays `rename_noreplace_unavailable`.

When every file of the batch is copied or skipped, apply:
7. syncs the move folder's filesystem once (`syncfs`), which writes out the batch's copies, their names and folders, then each copy's data (`fdatasync`), which reports a write error of that copy that `syncfs` does not. A copy whose `fdatasync` fails is removed, its original kept, and the run stops;
8. appends the batch's moved lines to `manifest.jsonl` and syncs it once;
9. for each file, checks the source's identity once more and at once removes the original.

So the manifest lists a move before its original is gone. When the last check or the removal fails, a second line for the same path follows the moved line and overrides it: the skip reason of that check, with the copy removed, or `unlink_failed`, with the verified copy kept and the original still on `/` (both exist). The last line for a path is its outcome.

Before a copy takes its final name, apply checks that `.partial/copy` is still the file it wrote (device and inode); if not, it skips the item as `copy_mismatch` and renames nothing. A source that vanishes or is swapped for a symlink between its check and its open is skipped (`missing` or `path_changed`); the run goes on.

A file already at its final name in the move folder, left by a run that stopped before step 9, counts only if its size and SHA-256 match the source; otherwise the item is skipped as `copy_exists` and neither file is changed. If such a matching copy is kept but the source then fails its last check (it changed), that copy is removed and the original stays. A run that skips an item as `no_room` stops there (`move_folder_full` warning); the remaining items stay on `/`.

**Leftovers of a killed run.** A run killed (SIGKILL, power loss) between steps 4 and 9 can leave `.partial/copy`, and a copy at its final name whose original is still on `/`. Running the same apply again cleans both up while the plan is valid (24 hours). After the plan has expired they stay: the administrator removes `.partial/` and, for each copy with no moved line in `manifest.jsonl` (or whose original is still on `/`), the copy, by hand. Nothing else in the move folder needs care.

**Held open.** After the gates and before the first batch, apply reads `/proc` once: the descriptors and memory mappings of every process, as the plan does. A planned file listed there is skipped as `held_open`. Nothing is checked again per file or per batch, and not before the removal. A process `/proc` cannot read refuses the run (`processes_unreadable`) rather than moving blindly. The accepted race: a file a process opens after the snapshot can still be moved. For the allowlisted files (rotated log archives, crash dumps, update leftovers, stale thumbnails) that is rare, and its only effect is that the file's space is freed when that process closes it, not at once; the process keeps reading and writing its open file, and the copy holds the file as it was when copied.

**Crash cases.** An original is removed only after the batch's copies and names (step 7) and its moved line (step 8) are on disk. A copy takes its final name (step 6) before its data is synced, so that one sync per batch replaces one per file. A run killed during a batch leaves one of these states, each of which the same apply, run again while the plan is valid, completes or leaves safe:
- killed (SIGKILL) before step 8's sync: no moved line of the batch is on disk and every original is still on `/`. A copy is at its final name, complete (the kernel still writes it out), or in `.partial`. The rerun reuses a final copy whose size and SHA-256 match and replaces `.partial`;
- a power loss or kernel crash before step 7 ends: as above, but a copy at its final name may be missing, short or garbled, since its data was not yet on disk. The rerun reads it back: one that does not match is skipped as `copy_exists` with both files kept, so the original stays on `/` and the administrator removes that copy (see **Leftovers of a killed run**). Nothing is lost;
- after step 8's sync, during step 9: every file of the batch has a moved line and a synced copy; an original already removed is `missing` on the rerun, and one still on `/` (or back after a power loss, since the removals are not synced) is removed then, its copy reused as it matches.

After the plan has expired, a moved line whose original is still on `/` means both exist; see **Leftovers of a killed run** above.

**Speed.** Measured with `run` on a Linux build host (ext4, 363–371 processes, Linux 6.8), small `*.xz`-like files of 4–80 KiB, the one `/proc` snapshot included (about 125 ms): 2,000 files in 2.53–2.56 s (1.27 ms per file), 160 files in 0.31–0.32 s. Before, with a `/proc` scan per check and the 25% duty cycle: 1,629 ms per file on the build host, and about 16 s per file on a test NAS (2.4 s of it the scans of 542 processes, 11.3 s asleep, 1.3 s in five syncs per file).

A folder is removed only once it is empty (else `not_empty`). Each move is appended to `manifest.jsonl` in the move folder (path, kind, category, outcome, and for a moved file its size, mtime and SHA-256). Each tool run is appended too. The answer's totals saturate rather than wrap.

After the moves, the plan's tools run in order:
- Before each tool, apply checks for a stop (a signal, a limit or a DSM update, below); after a stop the remaining tools do not run and are left out of `tools[]` (`not_started` means a tool could not be spawned).
- Each tool is checked again right before it runs (`untrusted` if the check fails).
- `restart_syslog_ng` is `not_needed` once syslog-ng no longer holds deleted files.
- Each runs with a cleared environment (a fixed PATH and `LANG=C`), `/` as its working directory, null stdio, and its own process group. There is no per-tool wall-clock deadline; a slow tool remains running until it exits or an explicit stop kills its process group. The historical `timed_out` outcome remains readable in retained reports.

The answer reports:
- `moved` (files, bytes, removed folders);
- `skipped[]` with the reasons above;
- `tools[]` with each outcome (`succeeded`, `failed` + `exit_code`, `killed` + `signal`, `timed_out`, `not_started`, `untrusted`, `not_needed`);
- the space of `/` before and after, as statfs measured it (`freed_bytes` may be negative when other writers used more).

Completion:
- `partial` (exit 3) when an item was left or a tool did not succeed.
- `paused` when the run stopped between batches or folders, or before a tool: by a signal (exit 130, 143 or 129), or because a DSM update started (exit 10, warning `dsm_update_active`). Running the same apply again resumes in the same move folder; items already moved count as `missing`, and a copy left in `.partial` is replaced.

Every started apply writes one atomic, private 0600 history record under `<state>/rootfs/history/`. `rootfs history --json` returns these records newest first; missing history is empty. A record contains `plan_id, requested_by, started, finished, move_dir, manifest, moved:{files,size_bytes}, freed_bytes, completion, skipped_count, tools:[{tool,outcome}]`.

**No undo.** There is no restore command. The moved files stay in the move folder, listed in the manifest, and the administrator copies back whatever they need. What DSM's tools delete is gone for good. The plan and the answer both say so (`no_restore_command`, `tools_no_undo`).

#### DSM update guard

`rootfs plan` and `rootfs apply` refuse with `dsm_update_active` while DSM has an update staged or running. The allowlisted update folders are DSM's working set at that time. `plan` checks before the walk and again before writing the plan; `apply` checks before its first change, and again after each batch of files, after each folder and before each tool: a marker that appears mid-run stops the run there as `paused` with the warning `dsm_update_active`, which names the marker. The markers are DSM's own (libsynoinstall):
- **Staged:** `/autoupdate.dsm.info`, `/manualupdate.dsm.info`, `/autoupdate.small.info`, `/manualupdate.small.info`. `UpdateUtil::CheckDownloadedPatchType` reads them to find a staged patch, and `_LaunchSystemUpdate` unlinks them when the update starts.
- **Running:** `/var/run/dsmautoupdate.pid`, `/var/run/dsmautoupdate_prepare.pid`, `/var/run/downloadDSM.pid`.

Any entry at one of these paths counts, including a symlink or a folder. The message names the marker and says to finish or cancel the update in Control Panel > Update & Restore.

#### JSON contract

The three documents use the standard envelope (section 14) with `command` `rootfs`, `rootfs-plan` or `rootfs-apply`. Schemas: `schemas/v1/rootfs.schema.json`, `rootfs-plan.schema.json` and `rootfs-apply.schema.json`, each tested against a golden file in `tests/golden/json/`. The `$defs/tool` enum in `rootfs-plan.schema.json` is shared with apply.

A refusal is the envelope alone, with `completion` `failed` and `errors[0].code` set to one of the codes above. A writer-lock conflict has `completion:"busy"`, `errors[0].code:"busy"`, and the holder object when known. Each schema's description lists its codes. `rootfs history` has its own `rootfs-history` envelope and `applies[]` schema.

Paths are the common `path` object, so non-UTF-8 names survive. Byte counts are integers.

The DSM UI builds on these documents, so the section 14 policy applies strictly: fields and reason codes are only ever added.

## 6. V2 candidate: true duplicate cleanup

Do not call reflink-shared files duplicates that waste space.

The useful duplicate feature is:

> Same content, but stored in different physical extents.

Possible pipeline:

1. same logical size candidate grouping;
2. exclude files already fully/mostly sharing physical extents;
3. cheap sampled fingerprint (for example beginning/middle/end chunks);
4. full-content hash only for surviving candidates;
5. report only duplicate groups with real additional physical cost;
6. estimate reclaimable bytes for deleting one or more duplicate members using the normal deletion simulator.

This remains outside V1 because it reads file contents rather than metadata only.

The P1 duplicates feature (docs/duplicates.md) is the metadata-only part of this
pipeline on Btrfs: step 1 from a record each scan keeps, steps 2 to 4 from the
extent and checksum trees instead of file data, and step 6 with the normal
reference accounting. It reads no file data.

## 7. Scan architecture

The ext4 backend walks fd-relative directory entries without following symlinks,
stays on the pinned device, and excludes mountinfo boundaries. Inode records
feed the existing accounting and report codec through bounded external sorts.
Both sorts reuse `spill::Appender`/`ChunkReader`, private `state/spill` runs and
the existing disk-reserve watch; there is no new persistent spill format.
Multi-link records are merged by device/inode before path aggregation. The
single-name fast path retains only one record. A second bounded metadata walk
checks an order-independent observational checksum (path, device, inode, mode,
links, size, blocks and nanosecond mtime/ctime). Differences mark the scan changed;
read gaps mark it partial. This observation is never a cleanup certificate.
The active state subtree is excluded from ext4 traversal to avoid scanning its
own growing spill. It remains represented and protected as own state.

```text
                    Btrfs filesystem
                          |
                          v
                 enumerate subvolumes
                          |
                  id / UUID / generation
                          |
             +------------+-------------+
             |                          |
      unchanged cache               changed/new
             |                          |
             |                  TREE_SEARCH_V2
             |                          |
             |          +---------------+---------------+
             |          |               |               |
             |      DIR_INDEX       INODE_ITEM      EXTENT_DATA
             |          |               |               |
             +----------+---------------+---------------+
                                |
                                v
                    per-subvolume immutable cache
                                |
                                v
                      global extent accounting
                                |
                                v
                         directory aggregation
                                |
                                v
                       point-in-time report
```

### Scan scope

The **presentation/materialization scope** and **accounting scope** are intentionally different.

- System scan: enumerate and scan supported Btrfs volumes independently.
- Volume scan: materialize the full namespace/accounting needed for one filesystem.
- Path-set scan: materialize only the selected subtrees plus the inode/extent references needed for those subtrees.
- Reclaimable accounting: regardless of presentation scope, consult filesystem-global physical extent/back-reference information as needed to determine whether selected extents remain referenced elsewhere.

A multi-path scan should therefore be able to stay relatively small without pretending the rest of the filesystem does not exist.

If global reference state cannot be resolved for a selected extent, V1 must treat that extent as retained/unknown instead of reclaimable.

## 8. Btrfs data sources

Use `BTRFS_IOC_TREE_SEARCH_V2` with large reusable buffers. The one exception is the tree blocks a volume overview reads by address (section 8.3).

Required V1 concepts include:

- `INODE_ITEM`: inode metadata, logical size, link count, mode;
- `DIR_INDEX`: parent/name/child relationships for namespace reconstruction;
- `EXTENT_DATA`: file-to-physical-extent mapping;
- extent/back-reference data required to establish total physical references and external ownership.

`INODE_ITEM.nbytes` must **not** be used as authoritative physical/reclaimable accounting because reflinked files each report referenced bytes even when they point at the same physical extent.

Physical identity and global references are authoritative for reclaimable calculations.

The Btrfs search range is lexicographic `(objectid, type, offset)`, not three independent filters; always inspect the returned item type and validate monotonic keys, item lengths, offsets, and integer bounds. `min_transid` filters metadata-leaf generations, not a complete file-change/deletion journal. V1 must not implement incremental deletion detection by treating it as such.

Snapshot sharing can be represented indirectly through shared metadata blocks. A scalar extent refcount is not necessarily a count of all visible files or snapshot owners. Resolve supported direct/indirect backrefs completely, or mark affected extents unknown. Merely counting selected `EXTENT_DATA` records is insufficient proof of reclaimability. Runtime capability probes plus fixture tests determine support on each DSM kernel; kernel version or successful TREE_SEARCH alone does not establish it.

### 8.1 Reference resolution strategy

The reclaimability decision is made from the extent tree (tree 2), read with TREE_SEARCH_V2, keyed by `disk_bytenr`:

1. Look up the `EXTENT_ITEM` for each candidate extent and read its total `refs` plus all inline and keyed data backrefs.
2. `EXTENT_DATA_REF (root, objectid, offset, count)`: a direct reference. It is internal to the selection only if `(root, objectid)` is an inode that loses its last name under the selection (section 10). Any direct ref to another inode or root is an external blocker with a precise classification.
3. `SHARED_DATA_REF (parent)`: a reference from a tree block shared through snapshotting. V1 treats it as **retained**, class `shared_tree`. A deletion in a live subvolume never frees an extent held by a shared tree block. The owner can optionally be named for `--explain` by walking tree-block backrefs (`TREE_BLOCK_REF`/`SHARED_BLOCK_REF`, `METADATA_ITEM` with skinny metadata) up to roots, with a bounded depth/fan-out budget; exceeding it yields `owner_unresolved`, not reclaimable. Note that stale full-backref blocks can remain after a snapshot is deleted, so `shared_tree` is conservative and can under-report reclaimable bytes; label that in output. For snapshot targets (sections 5.13 part 2, 5.14) owner resolution becomes part of accounting, not only `--explain`: an extent whose resolved owners all lie in the selected root set counts as freed by that set, under the same budget; an unresolved or budget-exceeded extent is `unknown`. For live path targets `shared_tree` stays retained. File extents found in fs-tree leaves no newer than the root's `ROOT_ITEM.last_snapshot` may be shared implicitly through a shared ancestor block and count as retained. That bound is dropped only when the root items prove no other root can share the tree: the subvolume has a uuid, no present subvolume is linked to it through a chain of `parent_uuid`s (a snapshot awaiting the cleaner still counts), no subvolume's `parent_uuid` names a missing uuid (a deleted intermediate snapshot hides its descendants), and no other subvolume without a uuid was ever snapshotted, and no relocation tree copies the subvolume (a running balance shares its blocks with a reloc root). So a subvolume whose snapshots were all deleted and cleaned (`btrfs subvolume sync`) is accounted as never snapshotted.
4. Reclaimable requires that the sum of internal direct-ref counts equals the item's total `refs`. Any mismatch, unknown ref type, or missing item is `unknown`.
5. Roots with zero refs / orphan roots awaiting the cleaner are classified `pending_subvolume_deletion`, not as live snapshots.

`BTRFS_IOC_LOGICAL_INO` is a test oracle and optional explanation aid, not the accounting engine. `LOGICAL_INO_V2` with `IGNORE_OFFSET` exists only from Linux 4.15; on 4.4 the v1 ioctl returns only references that cover the queried byte, so it misses partial/bookend references. The kernel backref walk can also be very expensive with many snapshots and cannot be paced by this program.

Lookup cost is the main performance risk for path scans: one extent-tree lookup per extent is random metadata I/O on a cold HDD/RAID cache. Sort candidate bytenrs, coalesce nearby ones into range searches, and switch to a streaming range read of the extent tree when the selected density makes that cheaper. Volume scans stream the extent tree once in bytenr order instead of point lookups (section 9).

Balance/relocation rewrites `disk_bytenr` values and creates shared refs. Detect a running balance/device replace with the read-only progress ioctls. While one is active, scans are labeled `changed-during-scan`, the extent-keyed global index is not certified for reuse, and cleanup is refused (section 5.7). Defrag and dedupe are ordinary mutations detected through generation changes. A balance can also be caught between progress probes: a relocation root (objectid `TREE_RELOC`, offset the fs root it copies) seen by either discovery marks the scan `changed-during-scan` with a `relocation-tree` reason, and that subvolume keeps the conservative leaf-sharing rule (section 8.1). Extents referenced by the data relocation tree are owned by no discovered subvolume, so their bytes are unknown, never reclaimable.

For display ownership of this app's writes, an unindexed writable inode is assigned
`syno_reclaim` only when every `INODE_REF`/`INODE_EXTREF` parent chain reaches a
state directory identified on that filesystem by `(subvolume, inode)`. This also
works if unrelated files changed in the same subvolume: it does not depend on the all-changes-inside generation exemption.
Each scan memoizes at most 65,536 decisions and performs at most 65,536 parent
lookups, with the existing 4,096-inode ancestry depth limit. Missing references,
external hard links, cycles, failed lookups or exhausted budgets keep the original
unknown classification (`writable_root_inode_unindexed` for a missing index entry).
Read-only, pending-deletion and relocation roots keep their existing classifications.
Extent ref completeness, verified file ranges, pinned-byte accounting and unknown
precedence still apply. This ownership evidence is display-only and never certifies
reclaimability or relaxes a cleanup guard or generation-change rule.

### 8.2 Synology-private metadata

Synology-specific xattrs (`syno.archive_bit`, `syno.create_time`, `syno.archive_version`, ACL metadata) and private Btrfs key type 163 are not required for V1 correctness. Unknown private items should be safely skipped and observable in debug/doctor output.

### 8.3 Tree blocks read by address

TREE_SEARCH_V2 reads a tree by key and cannot return the block at a given address. The volume overview (section 5.13) needs exactly that for a `SHARED_DATA_REF` parent in an unscanned root, so it reads tree blocks from the filesystem's device, read-only:

1. `BTRFS_IOC_FS_INFO` must report one device; `BTRFS_IOC_DEV_INFO` names its path. The device is opened `O_RDONLY | O_DIRECT`, so reads bypass and do not grow the page cache.
2. The primary superblock (64 KiB) must carry the magic and a valid CRC-32C, the filesystem's fsid and the device id, and checksum type CRC-32C. Any other checksum type, device count or failed check disables the reader for the scan.
3. The chunk tree (tree 3), read with TREE_SEARCH_V2, maps a logical address to a device offset. Profiles that keep a whole copy per stripe (single, DUP, RAID1 variants) are mapped; striped profiles (RAID0/10/5/6) are refused per block.
4. A block is used only when its CRC-32C, header address, fsid and generation match, the generation being that of its tree-block item in the extent tree read in the same scan, and it is a leaf with increasing keys. Any mismatch (for instance a block rewritten since the extent tree was read) makes the extent `leaf_unverified`, never measured.

The device is never written. A multi-device filesystem (Synology volumes are one device on md/LVM) keeps the conservative subcases.

## 9. In-memory model

Avoid one heap allocation per file/path.

Use compact arenas/string tables.

Conceptual model:

```text
DirEntry {
    parent_entry,
    name_offset,
    name_len,
    inode_id,
    flags,
}

Inode {
    subvol_id,
    object_id,
    mode,
    nlink,
    logical_bytes,
    extent_ref_range,
}

FileExtentRef {
    inode_id,
    file_offset,
    physical_extent_id,
    extent_offset,
    logical_len,
    compression,
    flags,
}

PhysicalExtent {
    filesystem_id,
    bytenr,
    physical_len,
    total_ref_state,
}
```

Paths are reconstructed from `DirEntry` parent chains only when needed for output. Do not store a full string path for every entry.

### 9.1 Sizing and the volume-scan memory shape

Holding every `FileExtentRef` and `PhysicalExtent` in memory does not fit the default budget on a large NAS. Rough design arithmetic (not a measurement): 10 M entries × ~40 B for namespace + inode records, ~200 MB of names, and 15 M extent refs × ~40 B comes to about 1.2 GiB, which exceeds the 512 MiB `--max-memory auto` default.

Volume scans therefore use an extent-driven shape:

1. Build only the namespace arena (DirEntry/Inode plus names) and per-inode counters from the fs trees.
2. Stream the extent tree once in bytenr order. For each extent, resolve its refs (section 8.1) and attribute its bytes immediately to one of these: the single orphan-able inode, the minimal owning subtree (section 11), or an external/unknown bucket. Then drop it.
3. Retain only per-inode/per-directory aggregates, blocker totals, and a bounded sample of top blockers for `--explain`.

Path-set scans and `simulate-delete` keep the selection's extent set in memory, which is bounded by the selection rather than the volume.

Implementation targets to verify in Phase 4: at most 48 B per namespace entry excluding name bytes, and no per-extent residency for volume scans. Publish the measured entries-per-GiB capacity. When the namespace alone exceeds the budget, stop with `resource_limit` and name the per-subvolume scope that fits. Spill-to-disk remains deferred.

## 10. Reclaimable calculation

### Single path

For a selected file/folder:

1. expand selected directory entries to affected namespace entries;
2. determine which inodes would lose their final directory reference, accounting for hard links outside the selection;
3. gather the physical extents referenced by those inodes;
4. determine how many references to each physical extent are removed by the selection;
5. compare with authoritative total known references/backrefs;
6. count a physical extent as reclaimable only if no reference remains outside the selection;
7. union physical extents before summing, so each physical allocation is counted once.

### Multi-select

Run the above against the union of selected namespace entries. This same target-set model is used by multi-path `scan`, `simulate-delete`, and cleanup planning where combined accounting is requested. Never calculate:

```text
reclaimable(A + B) = reclaimable(A) + reclaimable(B)
```

That identity is false for shared extents.

The CLI must also deduplicate overlapping path selections. For example:

```text
/volume1/share/Movies
/volume1/share/Movies/old
```

must behave as one selection rooted at `/volume1/share/Movies` for namespace deletion/accounting purposes, not double-count the nested path.

### Conservative correctness

If reference ownership cannot be completely resolved, classify the bytes as retained/unknown rather than reclaimable.

## 11. Efficient per-folder ranking

`top --by reclaimable` must not run a full deletion simulation independently for every directory.

For each physical extent whose complete reference set is known, determine the smallest namespace subtree that contains all deletable references (conceptually the lowest common ancestor of all relevant path owners when they belong to the same tree and there are no external references).

Attribute that extent's reclaimable bytes to that minimal owning subtree, then aggregate upward once.

Extents with references in snapshots/other subvolumes/outside the subtree do not contribute to a folder's individually reclaimable value.

Multi-select simulation remains a separate set-based calculation because it can reclaim extents that no individual selected folder can reclaim alone.

## 12. Cache model

ext4 never reuses sealed scan units: every scan is a full walk and paused resume
restarts the walk. Immutable report query caching still applies. Report format
5 carries explicit filesystem kind/capabilities; older binary reports require a
new scan. Existing Btrfs cache certification and generation rules are unchanged.

No SQLite.

No daemon-maintained mirror of current filesystem state.

The filesystem is truth; cache is an optimization.

Conceptual layout:

```text
state/
    jobs/<job-id>/job.json
    ui/cache/<fnv64 of argv>.json   # prewarmed webapi answers (5.21)
    cache/<filesystem-uuid>/
        manifest
        bundles/<unit-id-and-version>/
            tree
            extents
            metadata    # scope, identity, capture interval, checksum
        global.extents  # derived, freshness-bound reference evidence
        latest.report   # immutable report selected by manifest
```

### Cache key/invalidation

At minimum include:

- cache format version;
- filesystem UUID;
- subvolume UUID;
- subvolume ID;
- subvolume generation;
- scan scope/coverage hash, parser/accounting version, feature flags, and scan-unit ID;
- source capture interval, observed consistency, and cache age;
- checksum.

Behavior:

- unchanged generation is only a reuse hint, not a proof of an unchanged live namespace;
- for writable subvolumes, reuse complete, compatible, observed-stable units only within a bounded age (initial default 5 minutes); after expiry, recheck/rescan even when generation appears unchanged;
- read-only subvolume namespace caches may use identity/generation validation without that short age limit, but their external sharing results still require fresh reference evidence;
- generation-assisted reuse on a DSM build is enabled only after delayed-item/write/rename/delete fixture validation; an unvalidated build rescans writable units rather than trusting generation alone;
- changed generation -> rescan that subvolume;
- missing/corrupt subvolume cache -> rescan only that subvolume;
- changed subvolume/snapshot set or reference-evidence freshness -> refresh affected global accounting;
- cached per-subvolume extent mappings can rebuild an index, but cannot alone prove current outside ownership; refresh filesystem-global backref evidence for a new scan/resume or label the answer historical/unknown;
- partial path caches never stand in for a whole-subvolume cache; a whole cache may answer narrower coverage;
- changed-during-scan/partial results may be displayed but are not certified fresh cache units;
- everything missing/corrupt -> full scan.

Do not attempt complex cache repair.

### Writes

Publish a complete immutable bundle, not independently replaced `.tree` and `.extents` files. Write both into a versioned staging directory, validate counts/checksums, then atomically replace a small manifest pointing to the complete bundle. Readers pin one manifest generation. Missing/torn bundles fall back to the previous complete report or rebuild.

Durable checkpoint boundaries flush only the program's files plus the containing directory as required. Atomic rename alone is not a power-loss durability guarantee. Never call filesystem-wide `sync`/`syncfs` for scanning. Incomplete `.tmp` files are discarded on recovery; keep the previous complete report until its replacement is published.

### Cache retention

V1 only needs the latest generation. History is not required.

A V1 disk budget is required; evict only unreferenced obsolete cache bundles, never an active checkpoint or a report currently being read. Resource and free-space failure behavior is defined in section 22. Cache misses cost rescans, not correctness.

Exceeding `--max-cache` is not a scan failure. The scan continues in memory, stops persisting further units (so pause/resume loses those units), warns once, and still publishes the final report if the report itself fits. Only running out of space for the report or for a cleanup execution log is fatal.

### Versioning and time

A binary upgrade that changes cache, parser, or accounting version invalidates the old cache units by key; it never migrates them in place. Paused jobs created by a different binary version are not resumed and require a fresh scan, with an explicit message. A job manifest records `versions: {tool_version, report_format, cache_format}`; `resume` of a job whose versions differ from the running binary's (or that has none) refuses with `job_incompatible`, exit 4 (`resume --json`: the job-command refusal document, as for `pause`/`cancel --json`, with `command: "resume"`; a job running elsewhere is `busy`, exit 4). `cancel` still ends such a job. `jobs --json` shows each job's `tool_version` and `compatible`. Reports of another format are never migrated or silently ignored: `volumes` counts them (`incompatible_reports`, note `report_incompatible`), and an answer that finds no readable covering report while its filesystem has unreadable reports fails with the target error `report_incompatible` (with the same scan hint) instead of `no_report`. Plans are rejected when their format version is unknown. Age checks (unit reuse, report `--max-age`, plan expiry) use wall-clock timestamps for display. A timestamp in the future or a backward clock jump is treated as expired, never as fresh.

### 12.1 Display summary and directory index

See [report-summary.md](docs/report-summary.md) for the versioned sidecar design,
identity binding, atomic publication, compatibility rebuilds, page payloads,
subtree offsets and the integrity/safety boundary. Public JSON output is unchanged.
Overview and why-not-freed use `.srsummary` instead of decoding or checksumming the
report body on each display query. A missing/stale/corrupt summary is rebuilt once
from the checked streaming reader; a failure to persist disposable derived state
never discards a usable scan. The summary's own checksum and report trailer/stat
binding are checked on hits. The old full-file FNV check detected accidental
corruption and partial files; atomic publication, private state and DSM Btrfs data
checksums already cover those display needs. It never validated live file safety.
Cleanup and apply continue to read original/private fresh estimates, certify them
and recheck live data; they never consume this display sidecar.

Snapshot default listings and Recycle Bin rows also use the display summary. The
summary stores per-share/per-snapshot figures (including unknown/null and qgroup
cross-checks) and only bins accepted by the original real-bin classifier. Held-file
limits, pending ids, management hints, stale/cleanup notices and coverage remain
dynamic. Explicit snapshot simulations read the original analysis; optional oldest
Recycle Bin item ranking retains its original checked streaming path. Summary
format changes invalidate only derived state; supported reports need no rescan.

#### Precomputed Top and indexed Explore

Display `top.list` uses a report-bound `.srindex`. Root/share rankings for default
reclaimable order, services enabled, all/dirs/files, system details hidden, limits
up to 200 are computed at publication. Cached answers include the complete DSM
services summary and service rows, including physical, reclaimable, retained,
unknown and snapshots-not-read values. A Packages page can read the default
volume answer with the same JSON shape and no report-body traversal. The
`service_within` split keeps `vmm_virtual_disks` separate from iSCSI, including
its service rows and snapshots-not-read count. Index format 5 retains full-footprint
coverage partitions and excluded service paths; it rejects formats 1–4 (including
older classifiers, dense indexes and metadata-only coverage) and rebuilds them
from the original report without a rescan.

Other directory parameters seek the selected retained subtree or its nearest
indexed ancestor. Retain roots at depths 0/1, subtrees at least 64 KiB, and small
immediate children of parents above 1 MiB; omit other small directory records and
paths. Every omitted directory is answered by streaming at most a 1 MiB ancestor
range, stopping after the target. Large explicit queries, absent paths and regular
files directly inside large directories may still require large reads. Each touched fixed
record and cached answer verifies its own checksum; stat/trailer/schema binding
validates identity without hashing the entire report/index. Non-UTF-8 paths,
ranking ties, package exclusions/overlaps, unknowns and output limits retain
existing semantics. Missing/stale/corrupt derived state is rebuilt with the
original checked reader once; a persistence failure keeps the checked streaming
fallback available. Original readers still serve cleanup and simulations.

Directory sort memory is bounded through sorted spill runs and binary merging.
Pending small-child records/paths buffer about 4 MiB plus one entry, then spill to
one private unlinked scratch descriptor; only retained paths are hashed/sorted.
Root/share precomputation makes bounded extra subtree passes, at most three
ranking heaps per target, one target at a time; its measured publication overhead
is recorded with the large-report benchmark. See `docs/report-summary.md`.


Directory-heavy evidence on a Linux build host (Rust 1.97.1 release, 5,505,051 nodes,
505,051 directories, 179,188,137-byte report): format 2 uses 95,976,849 bytes
(53.56%), format 3 uses 5,476,847 bytes (3.06%, below the measured 5% goal).
Index build falls from 15.820 s to 9.015 s; five warm queries are 0.138–0.255 ms
for the volume root, 0.225–0.335 ms for a share, 0.855–0.954 ms for a group,
and 0.663–0.714 ms for an omitted leaf folder. Rankings skip allocation for
strictly worse heap candidates while preserving value/path ties and package
metrics. These are synthetic build-host measurements; NAS timing is not remeasured,
and wide directory fanout can retain more index records.

## 13. Live-filesystem consistency model

DSM volumes remain online and mutable during scans.

V1 intentionally uses best-effort scan-window analysis, not an atomic point-in-time snapshot:

1. record subvolume generation before scanning;
2. scan without `sync`, filesystem freeze, or write blocking;
3. record generation after scanning;
4. if unchanged and structural checks pass, mark the subvolume `observed-stable` (no change detected, not proof of consistency);
5. if changed, keep the scan result but mark it `changed-during-scan`/approximate.

A successful Btrfs read finishes the job `completed` and the document `complete`
(exit 0), even with changing generations, dangling entries, vanished requested
roots, bounded unresolved references, or unmaterialized snapshots. Coverage carries
an additive `approximation` object: distinct changed subvolumes, skipped requested
roots, and known unaccounted bytes. Existing coverage counters and change reasons
remain intact. Unaccounted byte counts do not include unknown sizes of skipped
namespaces and are never reclaimable. Each namespace is read once; there is no
whole-scan restart or unbounded retry. A vanished snapshot discards its share's
incomplete census and marks its snapshots unmaterialized, then continues to the
next share. I/O, permission, parsing, resource-limit and cancellation outcomes
remain errors or cooperative stops. Cycles, unsupported and encrypted namespaces
retain partial status.

Queries of an approximate report keep unresolved bytes and unmaterialized
snapshots in their normal result fields, without an unresolved-reference warning
or exit 3. Overview keeps unknown df copy-factor/reserve-row arithmetic nullable;
it does not pretend that arithmetic reconciled. Missing DATA/statfs observations
and real query errors remain partial. Snapshot deletion simulations retain their
strict unknown-selection outcome even when the ordinary listing is approximate.

Completion is distinct from report consistency: namespace gaps may still record
`consistency: partial` for fail-closed certification. Private cleanup estimates
never opt into this completion policy. Plan/apply still require fresh, stable,
complete evidence for the selection, live inode/owner/permission checks and the
existing rootfs protections.

Each detected change is listed in the report and in JSON as `change_reasons`, keyed by `kind`: `subvolume-generation-changed` (with `selected` telling whether the subvolume's namespace is in the report), `subvolume-appeared` (with `has_uuid` and, when its `parent_uuid` names a known subvolume, `snapshot_of`), `subvolume-vanished`, `subvolume-snapshotted` (a selected subvolume's `ROOT_ITEM.last_snapshot` moved), `subvolume-uuid-changed` (a selected subvolume id now has another uuid), `maintenance-running`, and `maintenance-probe-failed` (with `errno` when known), and `relocation-tree` (a balance left a relocation root for that subvolume in the root tree). They carry subvolume ids and generations, not names. Each report subvolume also records its `uuid`, `last_snapshot` and the ids of its `snapshots` (subvolumes whose `parent_uuid` names it) at the final discovery, which cleanup certification compares (section 5.7). The list is empty when nothing changed; cleanup refusals cite it.

A subvolume deleted after discovery is a change, not a scan failure. The kernel then fails every tree search of it with `ENOENT` (the error DSM showed as `tree search failed: No such file or directory`, observed on a test NAS, 2026-10-05, at a Docker subvolume; confirmed on kernel 6.8 by a loop-Btrfs test). When reading the namespace of a nested subvolume gets `ENOENT`, the scan skips that unit, leaves the link node unexpanded, lists the tree in `namespace_uncertain_subvolumes` and reports `subvolume-vanished` (with the generation seen at discovery) even if the final discovery still lists the root while the cleaner has not removed it. The scan finishes `changed-during-scan`, and certification stays fail-closed for that tree (section 5.7). When the missing tree holds a requested path root, that root is also an `unreadable_roots` entry, so consistency remains `partial` but the read completes with approximation. Other tree searches by tree id (owner resolution, leaf ranges, find-new, snapshot analysis) already tolerate an `ENOENT` by leaving their answer unknown, never by failing the scan.

A generation change made only by the tool's own state directory is no change reason (section 22.4). For each subvolume holding the state directory whose generation moved, the scan runs one find-new pass after its final discovery: a tree search of that subvolume reading only tree blocks newer than the first generation (`min_transid`, what `btrfs subvolume find-new` searches), keeping every inode whose `INODE_ITEM.transid` is newer. Btrfs rewrites an inode's item in the writing transaction whenever its data, attributes or directory entries change, and a deleted inode leaves its parent directory changed. Each changed inode is traced up through its `INODE_REF`/`INODE_EXTREF` parents; when every link of every changed inode reaches the state directory, that subvolume's `subvolume-generation-changed` reason is dropped and listed in `state_dir_writes` instead (`subvolume_id`, `generation_before`, `generation_after`, `inodes`: how many changed). Each subvolume is judged on its own: other subvolumes' reasons stay. The reason stays (fail closed) when any changed inode lies outside the state directory, when no changed inode is found (the change is unexplained), when a lookup fails or finds no parent, when an inode is reached twice or its ancestry exceeds 4096 inodes, or when more than 65,536 inodes changed. The pass does not see a relocation that rewrote file extent items without touching their inodes; balance and relocation roots have their own reasons (section 8.1), and the apply-time backref recheck (section 5.7) still refuses moved extents. Items of no inode (orphan items, DSM type 163) are not read: they hold no file data.


Default behavior should not endlessly retry a busy NAS.

Do not infer the amount of filesystem change from the numerical generation delta. Delayed items, uncommitted changes, concurrent ref updates, and multiple ioctl calls prevent generation equality from proving an atomic view. Missing/contradictory required metadata downgrades accounting coverage instead of being silently accepted as precise reclaimable bytes.

Reports include actual `scan_started_at`, `scan_finished_at`, earliest/latest source capture times (including reused units), `generated_at`, and consistency/coverage. Resuming tomorrow must not relabel yesterday's reused data as captured today. A valid partial report is useful; fake freshness is not.

## 14. Report and JSON contract

Human-readable output should remain simple. JSON is the stable integration surface for a future GUI/API.

Every JSON result should include:

- schema version;
- filesystem UUID;
- report timestamp;
- report consistency state;
- whether values come from reused cache or current rescan where useful;
- byte values as integer bytes, never localized strings.
- requested/effective scope, unsupported/skipped targets, incomplete units, unknown bytes/refs, and per-target errors;
- job ID, completion state, requested/effective resource profile, and degradation warnings.

The future GUI should be able to use the CLI/library without changing accounting semantics.

Compatibility policy: within a schema major version, changes are additive only (new optional fields, new reason codes that consumers must tolerate). Renaming or removing a field, or changing a field's meaning or units, requires a major version bump. Publish a machine-readable JSON Schema per command and test it against golden outputs.

## 15. Safety

All analysis commands are read-only with respect to user data. Cleanup is the only user-data-mutating surface. `rootfs apply` is the only command that changes DSM's system partition: it moves allowlisted DSM leftovers off `/` and runs DSM's own fixed-argv cleaners, with no switch of its own; the kill switch `mutations_disabled` stops it (5.23).

Allowed writes:

- cache files;
- temporary scan files;
- report files.
- explicit user-requested cleanup through packaged fixed scripts.

`doctor`, `scan`, `top`, `inspect`, `simulate-delete`, and `shared` must not alter the filesystem being analyzed.

`cleanup apply` must revalidate selected objects against current filesystem identity rather than trusting an old report, and reject detected replacement objects. The operation-boundary/concurrent-rename limitations and release gate in section 5.7 apply; do not claim that a one-time preflight check eliminates the race.

Cleanup scripts must be fixed package assets. The CLI must not execute arbitrary user-provided cleanup scripts in V1.

Generic recursive cleanup must refuse protected targets such as `/`, a volume mount root, DSM internal roots, and subvolume/shared-folder root deletion unless a future dedicated operation explicitly supports them.

Mountpoints, bind mounts, and nested subvolume/snapshot roots encountered inside a selected directory are boundaries, not ordinary recursive children. Enforce identity/mount-map checks, not just pathname prefixes or `st_dev`. Do not descend symlinks. Root/admin-only operation is the V1 permission model: raw Btrfs metadata may bypass user-level pathname access, so reports/checkpoints/logs are private (directories 0700, files 0600) and never exposed as a general unprivileged search index.

## 16. Privileges and platform

The CLI should detect required privileges and fail with a clear message when raw Btrfs metadata access is unavailable.

Initial targets:

- DSM/Linux x86_64;
- DSM/Linux aarch64.

Do not depend on `statx` or `io_uring`; DSM kernels in the target range may not provide them.

## 17. Suggested Rust module layout

```text
src/
    main.rs
    cli.rs
    scan.rs
    accounting.rs
    cleanup.rs
    job.rs
    resource.rs
    cache.rs
    report.rs
    output.rs
    model/
        mod.rs
        namespace.rs
        inode.rs
        extent.rs
    btrfs/
        mod.rs
        ioctl.rs
        keys.rs
        parser.rs
        subvolume.rs
```

Keep V1 as one binary/crate unless a real need for a workspace appears.

Likely dependencies:

- `clap` for CLI;
- `libc` or minimal direct FFI for ioctl/open calls;
- `serde`/`serde_json` for machine-readable output;
- `thiserror` or `anyhow` for errors during the prototype phase.

Do not choose a cache serialization crate until the in-memory model stabilizes. Cache format should be explicitly versioned and must not rely on Rust struct ABI layout.

## 18. Validation strategy

For ext4 cleanup, run `scripts/ext4-cleanup-integration.sh` in the disposable
Linux build workspace. Its private mount namespace supplies package config
stand-ins without changing host accounts, /etc or /var. Loop checks cover
plan/apply, hard links inside and outside selections, sparse files, stale
writes/nlink/symlink replacement and mount boundaries. Real DSM kernel 3.10
must display the per-volume no-replace refusal; actual deletion requires the
newer build-host kernel. Record explicitly when no newer real ext4 NAS is available.

ext4 validation includes a disposable loop image on a Linux build host and real DS416 armv7
hardware (a test NAS: 1 GB RAM, kernel 3.10). Use sparse files, cross-folder hard links,
symlinks, byte names, unreadable directories, bind boundaries and concurrent
writers. Compare allocations with `du -x -B1` and a device/inode-aware oracle;
compare filesystem usage separately with statvfs. A fixture larger than a tiny
spill budget must complete with the same totals and nodes as an unlimited run.
Real DSM UI/CLI testing must scan at least one million files across 2–3 trusted
shared folders with Recycle Bin content and record wall time, RSS, spill size,
responsiveness, pause/resume/cancel/reload and changes during scan. Loop cleanup
unmounts and detaches every image. No real newer-kernel ext4 NAS is assumed.

### Test infrastructure

Development is test-driven, so most logic must be testable without root or a Btrfs mount. Define three layers:

1. **Unit, no privileges (any host, including macOS).** The parser, accounting, LCA attribution, plan validation, and output layers consume a `MetadataSource` trait, never the ioctl directly. Tests feed hand-built items and **recorded TREE_SEARCH_V2 buffers** captured from fixture filesystems and sanitized DSM volumes, checked in as golden data.
2. **Integration, Linux VM with root.** Loop-mounted Btrfs images, built by scripts, on at least two kernel families: 4.4 (DSM 7.x baseline) and 5.10 (newer DSM platforms; confirm the exact list). Run them in CI under QEMU/KVM, because GitHub-hosted containers cannot mount loop filesystems reliably.
3. **DSM hardware lab.** Real x86_64 (and later aarch64) units for Synology-specific behavior (section 4.6), performance, and the acceptance matrix.

Also required:

- **Fuzzing.** Run coverage-guided fuzzing (`cargo fuzz`) over item/buffer parsing and plan parsing. Malformed, truncated, and hostile inputs must produce errors, never panics, UB, or unbounded allocation. `unsafe` stays confined to the ioctl/FFI module.
- **Ground-truth oracle.** Only on disposable fixtures with no other writers: record the Btrfs data-allocated bytes, delete the selection, force a commit (`btrfs filesystem sync`, test-only), wait for the cleaner, and compare the delta with `reclaimable_bytes`. Mismatch beyond a documented metadata tolerance fails the test. Cross-check with `btrfs filesystem du` (FIEMAP shared flag), `btrfs inspect-internal logical-resolve`, and qgroup exclusive counts where enabled.

### Functional fixtures

Create temporary Btrfs subvolumes covering:

- ordinary files/directories;
- empty files;
- sparse files;
- compressed files;
- reflink clone pair;
- partial reflink/range clone if supported;
- hard-linked files in same and different directories;
- deleted hard-link name with another link remaining;
- read-only snapshot retaining old extents;
- live file modified after snapshot;
- multiple snapshots;
- files with holes/prealloc extents;
- nested directories;
- filenames with non-ASCII bytes/UTF-8;
- concurrent mutation during scan.

For each fixture compare logical and physical expectations with independent Btrfs tools where available.

### Key invariant tests

- namespace DIR_INDEX count matches visible entries for stable fixture trees;
- every live directory entry resolves to an inode or a recognized subvolume/mount boundary;
- hard links do not reclaim data until the last selected name is removed;
- two full reflink clones share physical identity;
- deleting one clone does not reclaim shared extent bytes;
- deleting all references in a selection does reclaim the physical extent exactly once;
- snapshot-held extents are not individually reclaimable from the live file;
- multi-select reclaimable can be greater than the sum of individual reclaimable values;
- cache deletion never changes semantic results, only scan time;
- corrupt cache triggers automatic rebuild;
- unknown/private Btrfs item types do not crash the scanner.

### Production acceptance matrix

These are release gates, not optional follow-up work. Use isolated temporary Btrfs fixtures, never production user data for destructive tests.

| Area | Test and required outcome |
| --- | --- |
| Pause at each phase | Ctrl+C/control request during scan, reference resolution, aggregation, and publication; worker exits/releases locks, completed units survive, no incomplete result is promoted |
| Resume | Resume an unchanged fixture and match an uninterrupted report; modify/rename/delete files or create/delete a snapshot while paused and verify affected work/evidence is refreshed, not joined blindly with old refs |
| Writable-unit restart | Interrupt a giant single-subvolume scan; resume explicitly restarts that unit and displays redo progress instead of pretending to continue an atomic old view |
| Crash/durable publish | Kill during staged writes or manifest replacement; reboot-style interruption leaves a previous valid bundle or a clean rebuild path, never mixed-generation tree/extent files |
| Cache correctness | Same apparent generation with delayed changes, expired caches, wrong scope hash, partial path cache used for a wider query, truncated files, version mismatch; no false fresh/full-coverage result |
| Resource pressure | Set small memory/cache/runtime limits; fill state filesystem/trigger ENOSPC or EDQUOT; observe graceful stop/partial status without deleting user data or falsely reporting a saved checkpoint |
| Multiple processes | Two scans/cleanup writers for the same FS (including different state-dir overrides) cannot run concurrently; immutable report reads work; stale PID/lock files do not cause unrelated processes to be killed |
| Real scheduling impact | On supported HDD/RAID and SSD reference systems, compare no-scan baseline, low, and normal profiles with concurrent SMB/NFS or backup-like workloads; record foreground p95 latency, throughput, scanner CPU/RSS, scan time, and applied scheduler settings |
| Mount/permission changes | Unmount/remount replacement, bind aliases, nested subvolumes, locked shares, and EACCES; no unintended traversal or false complete result |
| Accounting correctness | Direct/indirect snapshot backrefs, external hard links, partial clones, compressed/sparse/preallocated data, and unresolved refs; unsupported cases are unknown, not confidently reclaimable |
| Cleanup confinement | New/replaced files, symlink/ancestor/final-name swaps, nested mounts, shell metacharacters, newlines, non-UTF-8 bytes, root targets, edited plans, and untrusted wrappers; no out-of-plan deletion or shell execution; unsupported hostile-race cases are refused |
| Cleanup interruption | Terminate after some removals; distinguish success/failure/already-absent/indeterminate, stop further work, and require fresh validation before retry; do not automatically replay an unfinished deletion |
| Synology namespaces | Mounted and locked eCryptfs shares, `#recycle`, `@eaDir`, `#snapshot`/share snapshots, share-root subvolumes; no ciphertext paths, no cleanup through lower names, and correct protected/hidden classification |
| Background data movers | Balance, defrag, dedupe, snapshot create/delete, and pending snapshot cleanup during/after scan; results downgrade or refresh, cleanup refuses during balance, and `pending_subvolume_deletion` is classified correctly |
| Accounting oracle | Delete-and-measure on disposable fixtures matches `reclaimable_bytes` within the documented metadata tolerance, including bookend/partial-extent cases |
| Scale | At least 10 M entries and one file with at least 1 M extents on HDD/RAID; memory stays within budget or stops with an actionable `resource_limit`; published capacity numbers come from this run |
| Cleanup load | Delete at least 1 M entries with qgroups on and off under concurrent SMB load; record foreground latency and commit stalls per profile; the `low` priority default is judged from this data |
| Upgrade | Cache, job, and plan from version N read by N+1: invalidated or refused cleanly, never misread |
| Machine interface | Multiple paths/stdin/NUL/-- operands; no progress contamination of JSON stdout; meaningful exit codes; control characters escaped in human output; reports remain private |

Before production labeling, record the tested resource limits and a foreground-latency/throughput acceptance envelope for each reference platform. Low-priority settings being accepted by the kernel is not a passing performance test. Optimize the default pacing only from those results. This spec does not claim a measured percentage improvement or universal zero-impact scanning.

### Performance tests

Measure separately:

- raw ioctl scan floor;
- parsing/building arenas;
- physical extent accounting;
- report aggregation;
- cold filesystem metadata cache;
- warm filesystem metadata cache;
- cache-hit report load.

Do not optimize against only one synthetic tree. Include many-small-files, deep-tree, large-file/reflink, snapshot-heavy, SSD, and HDD/RAID cases.

## 19. Implementation phases

### Phase 0 - ioctl/parser foundation

- Rust project skeleton and CLI.
- Btrfs filesystem/subvolume discovery.
- `TREE_SEARCH_V2` wrapper.
- parse `INODE_ITEM`, `DIR_INDEX`, `EXTENT_DATA`.
- `doctor` command.
- ABI bounds/overflow checks, capability detection, and low-profile cooperative batch boundaries.
- `MetadataSource` abstraction, buffer recording tool, golden-buffer unit tests, parser fuzz target, and the Linux 4.4/5.10 VM integration harness (section 18).
- raw benchmark command gated behind debug/development feature if needed.

Exit condition: stable fixture tree can be reconstructed and matches normal directory enumeration; parser fuzzing runs in CI without findings.

### Phase 1 - namespace + basic report

- compact DirEntry/Inode arenas;
- logical size aggregation;
- physical extent identity parsing;
- target-set resolution (`system`, volume, one or more paths), including overlap deduplication and cross-filesystem partitioning;
- `volumes`, `scan`, `top`, `inspect`;
- JSON schema v1.
- resource budgets, stderr progress, per-target coverage/errors, and private state paths.
- canonical per-filesystem advisory writer lock (section 22.5); it is needed by scan publication and by cleanup in Phase 2.5, so it cannot wait for Phase 3.
- Synology namespace detection (section 4.6): eCryptfs shares reported as unsupported coverage at minimum.

Exit condition: ordinary, sparse, compressed, hard-link, and reflink fixtures report correct basic metrics.

### Phase 2 - reclaimable accounting

- authoritative/global physical reference accounting per section 8.1 (direct refs, conservative shared-tree refs, orphan roots, sorted/coalesced lookups);
- extent-tree streaming for volume scans (section 9.1);
- snapshot/subvolume external reference handling;
- per-path reclaimable aggregation;
- `simulate-delete` multi-select;
- `shared` and blocker explanations.

Exit condition: reflink/hard-link/snapshot fixture matrix passes reclaimability invariants and the delete-and-measure oracle.

### Phase 2.5 - cleanup plan/apply

- immutable cleanup plan format;
- target identity revalidation;
- each selected item moved into its shared folder's Recycle Bin with DSM's own `synorecycle`, run with a fixed argv and no shell;
- `cleanup plan`, `cleanup apply`, and the `cleanup delete` convenience flow;
- protected-target denylist/guardrails;
- per-item results, partial and cancelled moves with what stayed, and an explicit confirmation that the items move to the Recycle Bin;
- mark affected report/cache state stale after mutation.

Exit condition: every refused path class is refused, and a move ends moved, partial or untouched as reported, with nothing removed for good.

### Phase 3 - disposable cache

- manifest/version/checksum;
- per-subvolume immutable caches;
- generation-assisted bounded-age/scope-aware reuse with DSM mutation tests;
- whole-bundle atomic manifest publication and durable completed-unit checkpoints;
- derived global index rebuild;
- corruption/rebuild tests;
- `cache status/clear`.
- foreground job lifecycle, `jobs/pause/resume/cancel`, and graceful signals (reusing the Phase 1 writer lock);
- restart current writable unit / refresh global reference evidence on resume;
- disk/memory/time-budget recovery and stable exit codes.

Exit condition: pause/resume/crash/corruption/resource-pressure tests pass; cache deletion changes work needed, not semantics on unchanged fixtures. Live reports preserve actual capture windows and incomplete-coverage labels.

### Phase 4 - DSM benchmark/hardening

- x86_64 and aarch64 builds;
- real DSM volumes;
- busy-volume consistency reporting;
- memory profiling on millions of files;
- HDD/RAID cold-cache benchmark;
- identify whether any Synology-specific Btrfs behavior requires special handling.
- run the production acceptance matrix and publish tested limits/profile impact, not just raw scan throughput;
- packaging ownership, dependency lockfile, non-UTF-8 paths, and privilege checks.

### Phase 5 - question-centred commands

Proposed order:

- next round: `top` service rows and `HELD BY`/`WHERE TO FIX` (5.4); cleanup certification (5.7, implemented); `overview` with data-level reconciliation (5.13); `snapshots` and snapshot targets in `simulate-delete` (5.14, needs `scan --snapshots` and owner resolution); `recycle` (5.16);
- after those: `why-not-freed` (5.15, reuses 5.13/5.14), `support-bundle` (5.17, reuses everything above);
- later: `drive` (5.18, after a read-only layout survey).

Exit condition per command: fixture tests for every part/row including unknown and budget-exhaustion cases; the delete-and-measure oracle for every `reclaimable`/`freed` value (snapshot deletion measured on loop fixtures with `btrfs subvolume delete` + `subvolume sync`, test-only); qgroup cross-check where enabled on fixtures; `overview` reconciles on vtw fixtures and on a test NAS's `/volume1` within tolerance.

## 20. Empirical findings already validated

On a DSM 7.4 / Linux 4.4.302+ x86_64 Btrfs test NAS:

- `BTRFS_IOC_TREE_SEARCH_V2` works.
- `statx` returns `ENOSYS`, so V1 must not depend on it.
- `DIR_INDEX` counts matched normal directory enumeration on tested stable subvolumes.
- `INODE_ITEM` count matched entries plus the subvolume root inode.
- a 16 MiB reflink clone pair reported 16 MiB `INODE_ITEM.nbytes` for each file while both `EXTENT_DATA` records pointed to the same physical `disk_bytenr`/`disk_num_bytes`; therefore inode byte summation double-counts shared physical data.
- Synology stores private xattrs such as `syno.archive_bit`, `syno.create_time`, and `syno.archive_version` in Btrfs metadata; they are not required for V1 cleanup accounting.
- a Synology-private key type 163 exists in tested metadata. Its semantics are currently unknown and it is intentionally not part of the V1 correctness model.

## 21. Product principle

The CLI should optimize for this answer:

> "If you delete this selection now, the latest scan estimates that about **X GiB** of file-data space becomes reclaimable. **Y GiB** remains referenced by snapshots, hard links, or other files."

Everything else exists to calculate or explain that number.

## 22. Production operation: minimum required behavior

These are V1 requirements, not a separate orchestration platform. Keep one foreground Rust process, private files, advisory locks, and a small fixed cleanup wrapper. Numerical limits below are initial design defaults to validate on DSM, not measured safe limits for every NAS.

### 22.1 Pause/resume without a resident service

A queued launcher has 60 seconds from its manifest's `updated` timestamp to acquire the live lock. On job load/list, an older queued job with an idle live lock is re-read under that lock and durably cancelled with reason `not_started`; no work was executed and no final result or counts are invented. A recent queue or held live lock is left alone. This also fences a delayed launcher: `launch` refuses the cancelled manifest. A load never reruns a job, including destructive applies.

An interrupted or failed job need not have `result.json`. Clients leave the progress view when the job is no longer active; they attempt the final result at most once per terminal transition and show "Operation interrupted; some work may have completed" when it is absent. Persisted progress is a last recorded checkpoint, not a final total. Only scans use `resume`. Cleanup apply requires a new `cleanup plan`; rootfs apply explicitly reuses the original plan through `rootfs apply`, whose existing validity and destination checks can refuse it and require a new check. New rootfs job manifests retain optional `rootfs_plan` (`plan_id`, `move_dir`) before queueing and expose it in `jobs --json`; older manifests remain readable and offer recheck when this information is unavailable.

Lifecycle: `[queued ->] running -> pausing -> paused -> running -> completed | partial | failed`, with `cancelled` as an explicit terminal state. `queued` is a job `scan --detach` created that waits for its launcher to run it (`scan --job-id`); `pause` of a fresh queued scan is `not_running`; an idle queued detached resume can be returned to `paused` after launcher failure. `cancel` ends either queued scan at once, and `resume` refuses it as `not_started`. A killed process is discovered as `interrupted` when its live lock is absent; a PID alone never proves liveness.

- `scan` allocates a job ID and stores targets, options, schema version, source identities, completed unit references, and phase in a private job manifest.
- `jobs` lists active/paused/interrupted jobs, current phase, completed units, actual source age, and the reason work stopped; `jobs JOB_ID` shows only that job. It does not launch work. A running job with a pending request shows `state: "pausing"` or `"cancelling"` and `request: "pause"` or `"cancel"` (`request` is null otherwise). Each job lists `filesystems: [{fsid, mount}]`, what its targets resolved to in the latest run (empty until a run resolves them), so a caller can find the job of a volume without re-resolving the operands. A request that arrives after a run's last check (late in `aggregate`, or just before `publish`) cannot stop it: the run finishes, its scan document warns `request_ignored`, and the job keeps `request_ignored: "pause"` or `"cancel"` (null otherwise). A request sent while the job's phase is already `publish` is refused as `too_late` instead. Every run of a job keeps its whole scan document, as `scan --json` prints it, in `jobs/<job_id>/result.json` (0600, replaced by the next run), and `jobs --json` shows its summary as `result: {completion, exit_code, reports: [path], error_codes, document}` (null before a run has ended, e.g. `queued` or `interrupted` before its first end); a caller that did not start the run, such as a detached one, reads the outcome there.
- A running scan rewrites its live progress into the job manifest at most every two seconds, atomically (a reader sees either manifest, never a partial one) but without the filesystem sync a durable manifest write uses, since a forced commit that often measurably slows the scan and progress is disposable. Another process can follow it through `jobs --json` without reading progress text: the phase, units done out of the writable-subvolume total, the entries read (those of the trees fully read plus the directory entries read so far in the tree being read, so the count moves within one large share; a reused unit does not count), the metadata items and their payload bytes read so far, elapsed seconds, resident memory, the subvolume being read by id and name (never a file path), a `basis` of the last completed scan of the same target (its entries, duration, and its items once history records them; `null` when none is recorded) for a percentage and an ETA, and the time of the update so a consumer can detect a stalled scan. The write is additive in schema version 1; a manifest from an older binary simply has no progress.
- `pause JOB_ID` writes an authenticated-by-file-ownership control request; the active process observes it at safe boundaries, checkpoints completed work, releases FDs/locks/memory, and exits. A request receipt is not proof that pausing has finished.
- `mode JOB_ID --profile P` writes the same kind of request, kept apart from pause and cancel, for a running scan only (an apply job is `not_adjustable`, a stopped one `not_running`). At its next request check the scan applies P's CPU and I/O priority, measured from the priority the process started with, and records P in the job, so a later resume keeps it; it does not stop, checkpoint or restart, and its progress keeps counting. Batch sizes chosen when a filesystem's walk started stay until that walk ends. Lowering nice again needs CAP_SYS_NICE (the executor's root has it); without it the change is a warning. `--json` answers like `pause --json` with `command: "mode"`.
- Ctrl+C, SIGHUP (where delivered on SSH closure), and SIGTERM request the same graceful scan shutdown. Signal handlers set a flag only; normal code performs filesystem writes. A second interrupt may terminate immediately, losing only unsealed work.
- `resume JOB_ID` reacquires locks, validates identities/coverage/checksums/versions and the cache freshness policy, then starts a new foreground process. It can work after SSH disconnect or reboot; it does not run automatically on startup.
- Resume may override resource budgets/profile (including the `--min-free` reserve) or force refresh, but cannot silently change the original target set. Missing disposable unit files may trigger a fresh scan; missing or changed job checkpoint data explicitly requires a new scan. All job commands honor the same `--state-dir`; filesystem writer locks still use the independent canonical runtime directory.
- `cancel JOB_ID` requests termination and discards unfinished work. It never cancels NAS services, deletes targets, or invalidates otherwise valid cache. Completed/cancelled job summaries may be pruned to a small bounded count. A scan stopped by a cancel request reports `completion: "cancelled"` (exit 10) with warning `cancelled`, not `paused`, and its job ends `cancelled`.
- A cooperative `--max-runtime 30m` limit uses the same pause path. It is checked between batches; it is not a promise to interrupt a blocked kernel call at an exact deadline.

Control files live in a root-owned job directory; no network endpoint, polling daemon, blind `kill <saved-pid>`, PID-reuse assumption, or dependence on SIGSTOP/SIGCONT. Use existing terminal/session supervision such as `tmux` when detachment is desired; V1 does not invent a background scheduler.

### 22.2 Checkpoint granularity and restart semantics

A work unit is one selected namespace scope within one subvolume (group overlapping roots), identified by filesystem/subvolume identity and scope hash. V1 seals a unit only after all required namespace/extent records for that scope have passed structural validation. Whole-subvolume scans normally have one such unit per subvolume.

Persist completed units and a versioned partial namespace/extent stream, never Rust heap pointers or live lock state. Namespace checkpoint encoding and checksumming stream to disk with bounded buffers. Check for pause between ioctl batches and bounded parsing/accounting chunks; do not wait for a giant subvolume to finish before acknowledging pause.

On resume of a Btrfs volume job:

1. validate saved subvolume identities, generations, scope inputs and checkpoint checksums;
2. reuse the job's completed units without rereading their namespaces;
3. restore the partial namespace and flushed extent stream, then search after its last consumed key;
4. preserve the job ID, completed phases, counters and accumulated active elapsed time;
5. refresh global reference evidence, recomputing an interrupted derived phase from complete input units.

The checkpoint lives under the private job directory and is independent of disposable cache age and `--no-cache`. Its own writes may advance the containing subvolume's generation: retain the saved baseline only when the existing changed-inode/ancestry proof establishes that every changed inode lies inside the App's state directory. Unexplained changes, changed identity or scope inputs, missing files and damaged checksums still return `checkpoint_unusable` and explicitly offer a fresh scan; never silently restart. A process killed before saving its partial namespace also requires a fresh scan. Current path-only and ext4 jobs have no persistent walk checkpoint and explicitly refuse resume rather than restarting silently.

A last B-tree key alone is not a safe continuation token for a changed writable tree. A resumed result retains the existing before/after consistency checks and does not certify cleanup authority; cleanup still performs its fresh selection/reference checks. Paused jobs keep no resident process. Final and cancelled jobs discard their private checkpoint files.

### 22.3 Low-impact scheduling is the default

| Profile | CPU scheduling | I/O scheduling request | Metadata concurrency | Cooperative scan duty (active share of wall time) |
| --- | --- | --- | --- | --- |
| `low` (default) | nice +10, never above inherited priority | `IOPRIO_CLASS_IDLE` when available | 1 active metadata/backref worker globally | 100%: no sleeps (before 2026-09-30: 25% active / 75% yield) |
| `normal` | preserve normal/inherited priority; no realtime policy | best-effort normal priority when available | 1 by default; explicit `--workers N` may increase to at most 4 | 100% |
| `fast` | as `normal` | as `normal` | as `normal`, plus up to 4 concurrent pinned-leaf device reads | 100% |

`fast` finishes a volume overview soonest by spending memory and device queue depth, not priority: it never raises CPU or I/O priority above what the caller gave it, because the tool does not outrank the NAS's own work. Its volume overview holds up to 1024 judged extents, reads the leaves their `SHARED_DATA_REF`s name (section 5.13) once each, in address order and 4 at a time, and keeps up to 16,384 parsed leaves (`low` and `normal`: one extent and one read at a time, 256 leaves), still charged to `--max-memory`. The profiles measure the same bytes; they differ only in which extents `--pinned-read-budget`, which applies to every profile, leaves `read_budget_exhausted` when it runs out. Skipping the leaf reads is not a profile: `--pinned-read-budget 0` does that with any profile. Cleanup treats `fast` as `normal`. Measured on DSM (a test NAS's `/volume2`, DS918+, 938 GB of data, 2026-09-29), each a volume scan with the same result: `low` 2075 s, `normal` 532 s, `fast` 451 s; peak RSS 172–173 MiB and about 2.2 GB read in all three. That difference was `low`'s pacing, since removed; `fast`'s batching and larger cache save a further 15% and 1,062 of 58,692 leaf reads.

Apply settings to every worker, not just the main thread. No profile sleeps to hold a duty cycle: `low` is gentle through `nice` and the idle I/O class and one leaf read at a time, not through waiting. Stop and control requests are polled at every batch boundary, so they do not depend on sleeps, and `--max-runtime` still applies. `duty_percent` stays in the scheduling report (100 for every profile) so existing readers keep working. All implicit scans from `inspect`/`top` use the same default profile and budgets.

Start with a reusable 256 KiB ioctl buffer for `low`, 1 MiB for `normal`; grow only when a single valid returned item requires it, with a tested upper cap. Do not equate returned metadata bytes with physical I/O. A huge buffer/call can delay yielding even with low priority. Smaller buffers and a single worker are the fallback when scheduler enforcement is unknown.

`nice` affects CPU preference, not a CPU percentage limit. `ioprio_set` is scheduler/device-stack dependent and does not cover every asynchronous kernel write. A successful setting is not proof of no SMB/NFS latency impact. Read back settings, inspect available underlying scheduler information without changing it, and expose `requested`, `applied`, and `effectiveness_unknown` separately in `doctor`/JSON. Failed priority requests warn and keep the single worker and small buffers; never silently fall back to an unrestricted high-concurrency scan.

Do not infer SSD/HDD topology from a Btrfs `ssd` mount option, change disk schedulers/mount options, disable other tasks, clear global page cache, or require cgroups/PSI/io_uring. V1 does not implement an adaptive system-wide I/O controller. Explicit `--workers` overrides are enough; operators can stop a scan and resume it after backup/rebuild/scrub activity.

### 22.4 Memory, disk, and resource exhaustion

- **ext4 spill budget:** memory exhaustion is never a reason to stop an ext4
  scan. Each sort buffer uses `min(max-memory / 8, MemTotal / 32, 32 MiB)`
  (minimum one byte for explicit tiny tests), with bounded merge readers and
  directory depth. On a 1 GB box auto gives approximately 16 MiB of buffer;
  arbitrary volume size increases disk/time, not inode-table memory. Flush
  and continue at the budget. Disk-reserve exhaustion and explicit user
  cancellation retain the existing clear stop reasons; pause and runtime
  limits retain existing lifecycle semantics.
- `--max-memory auto`: initial own-process working-set budget is the smaller of 512 MiB and one eighth of detected physical RAM. If that leaves less than the tested minimum (initially 64 MiB), refuse a new full scan with an actionable message. Explicit `--max-memory` overrides are permitted. These are application budgets, not hard OS-enforced RSS/page-cache guarantees.
- Charge arenas, strings, inode maps, extent/backref sets, sorting workspaces, and output buffers to that budget. Use bounded queues, checked/fallible growth, and periodic RSS/available-memory checks. Do not exhaust the NAS because a fragmented file has millions of extents or a snapshot has many owners.
- For whole-volume overview attribution, charge both the inode-to-owner index described in 5.13 and the shared-tree owner memo before they grow. On 64-bit targets the memo's base estimate is about 48 bytes per resolved tree block plus 8 bytes for each cached root; one extent may cause up to 64 tree-block lookups by default, and the default volume-scan global lookup budget is 100,000. This owner-walk memory is shared across extents, not retained as one owner list per extent.
- At the budget, stop safely with `resource_limit`, preserving completed units. The stopped filesystem still publishes its scan stats and a `resource_limit` object: the budget, the resident set and the growth requested at the check that tripped (their sum is the projection), and the tree being read. Explain whether a narrower scope or a larger memory limit is required; do not auto-resume into the same failure repeatedly. Unknown output is preferable to silently dropping refs. The volume ownership index and snapshot scan census spill to disk. Snapshot extent-tree joins consume one extent's backrefs at a time, including references from snapshots outside the materialized selection; they never clone all backrefs for a candidate chunk. Other transient models remain checked against the memory budget.
- A path scan reserves the scope tree of its path roots' subtrees only, but V1 still reads the whole namespace of each subvolume it enters (inodes, entries and names, with extent refs spilled to disk) before expanding the path. That transient read sets the memory floor of a narrow scan in a huge subvolume: on a test NAS (DSM 7.4, a 2.3M-item namespace read) a 50k-entry path scan completes at 16 MiB and stops at 14 MiB. A directed read of only the subtree is deferred: it would cost one search per directory and needs a cache rule for partial units. This floor is a known V1 limit.
- `--max-cache` initially limits cache plus resumable scan data/staging to 1 GiB per filesystem; `--min-free` initially reserves 512 MiB available space on the state filesystem. Both are operator-overridable, including when resuming a paused job; an omitted resume option keeps the job's saved value. Budget peak old+new bundles during publication, not just final file size.
- When the state directory or its spill directory resolves to the system partition (the device of `/`; the default `/var/lib/synoreclaim` on DSM md0), the effective reserve on that filesystem is the larger of the current `--min-free` value (including a resume override) and 3% of that partition, because DSM itself needs the space. The separate 5% threshold controls the general `system_partition_nearly_full` warning. A scan checks free space on both the state and spill filesystems at most once per second between batches, and stops below either location's reserve as a paused, resumable job (`resource_limit`, warning `disk_limit`, exit 10) instead of filling it.
- Each published scan is recorded in `<state-dir>/history/<fsid>.json` (0700/0600, the newest 64 scans per filesystem): scope, job, targets, profile, start/finish, this run's elapsed, the job's total elapsed across resumes, entries and subvolume counts, reused units, the full-read ETA basis, peak RSS, the report path and bytes, the filesystem and Btrfs data space at the end, and the outcome (section 5.20). It outlives report pruning and invalidation and feeds the `history` command, the no-report hint (section 5.1), the progress ETA (section 22.7) and `doctor`'s spill projection. The ETA uses the last full read so a reuse-heavy or resumed run cannot shorten it; a missing or unreadable history only removes those estimates, and failing to write it warns `history_not_saved`.
- Choose/configure a durable local `--state-dir`, not an assumed-unlimited RAM-backed `/tmp`. Pin it by identity, exclude the tool's own state subtree from user rankings/cleanup, and do not create a hidden subvolume or change filesystem settings automatically. Private state inside a scanned filesystem can itself advance generations; it must not trigger endless rescans. `doctor` warns (`state_dir` check) when the state directory, or where it would be created, lies on the target's filesystem. It recommends the default state directory only when both its state and spill locations have room for the reserve and the largest spill recorded for the target's filesystem. Otherwise it gives a concrete `--state-dir /volumeN/synoreclaim` on a different DSM data volume when one has enough space; if none is available, it says to provide a data-volume path outside the scan selection. It never recommends `/var/lib/synoreclaim` when the system partition lacks room. When spill is separately mounted, `doctor` reports its reserve and projection in `spill_space`. A scan whose state directory lies on the scanned filesystem ignores a generation change that only the state directory's own writes made (section 13: find-new pass, `state_dir_writes`), so such a scan can stay `observed-stable` and cleanup can use it; this is how the SPK keeps its state in the package data folder on the volume. The scan still says where the state lies, on stderr and in its filesystem `warnings`: `state_dir_in_selection` (inside a selected subvolume: a volume target selects all, a path target its own subvolume) or `state_dir_on_scanned_filesystem`, without telling the user to move it. It adds `state_dir_writes_ignored` for each subvolume whose change it ignored, and the hint `state_dir_changed` when a change reason is still the subvolume holding the state directory (some changed inode lies outside it or could not be traced). It never refuses the scan. The state directory's own files are read as ordinary content, not skipped: their nodes carry the `own_state` flag (the state directory, from its identity), which rankings and cleanup already exclude, so totals include the state's bytes and every consumer can tell them apart. A directory entry below the state directory whose inode is gone when the scan reads it (the scan replaces its own job file and removes its spill runs during the read) is not counted in `dangling_entries`, so it does not make the scan partial; a dangling entry anywhere else still does. Measured on DSM (2026-09-28): with the state directory in the scanned `@syno` subvolume, 5/5 path scans were `changed-during-scan`; with it on tmpfs, 4/5 were `observed-stable`. Measured on DSM (a test NAS, 2026-09-30, state in `/volume1/@appdata/…/state`, subvolume 256): three `cleanup plan` estimates of one test file took 80 s, 9.7 s and 5.5 s. One stayed `observed-stable` although 256 changed through 12 inodes, all in the state directory (`state_dir_writes`). The other two kept the change because files outside the state directory had changed: the test's own diagnostic output, and DSM's `@userpreference/admin` settings. A whole-volume low-profile scan (27:50, 91 MB RSS) kept 256's change, as DSM wrote `MonitorDataPool/QCMonitor/Current` and `@userpreference` there during the window; 261 and 262 kept theirs too.
- A whole-volume report's state is large, and the default `/var/lib/synoreclaim` sits on DSM's system partition (`/dev/md0`), not on a data volume. Measured on DSM 7.4 (2026-09-28, 5.5 million entries, 204 subvolumes): `top /volume1 --refresh` left about 620 MiB in the state directory, a 284 MiB report plus kept scan units (bounded by `--max-cache`), on a 7.9 GiB system partition with 4.5 GiB free. `--min-free`, raised to 3% of the partition as above, guards the space left there; pass the recommended `--state-dir` on another data volume when the system partition is short of room.
- Cleanup keeps plans, apply results and the record of moves under `<state-dir>/cleanup/` (directories 0700, files 0600) and refuses a state directory that it, or any ancestor, lets another user write by owner and mode bits. Mode checks cannot see DSM share ACLs: a state directory inside a shared folder (`/volume*/<share>/...`) whose ACL grants other users access is not detected. Such state dirs are not refused; keep the state directory outside shared folders, such as the default `/var/lib/synoreclaim`.
- Check space before and during writing. Free obsolete unreferenced cache first; if still insufficient, pause/fail with `ENOSPC`/`EDQUOT` and keep the previous report. If even the final checkpoint cannot be written, say that recent progress was not saved. A free-byte check cannot guarantee Btrfs metadata allocation succeeds.
- Small scans may run with `--no-cache`; the tool must not require gigabytes of free disk merely to inspect one file. A useful in-memory/partial answer may be emitted when persistence fails, with a non-success completion status and no false durable report ID.

### 22.5 Concurrent invocations and publication

The backend admits only one queued or running scan on the NAS, regardless of volume, administrator, or `--state-dir`. A short `flock` transaction on canonical `/run/synoreclaim/locks/scan-admission.lock` checks and records `scan-owner.json` (`jobs`: canonical absolute jobs directory, `job_id`) before publishing the manifest. It waits only for another admission transaction, never for a scan to finish. A persistent reservation bridges the detached queue/launcher gap; start, launch, and resume all use the same admission path. Detached resume reserves `queued` with the selected profile before returning `scan --job-id` argv, and increments `resumes` once. The same job can launch its reservation once. If the external launcher fails, `job.pause` returns an idle queued resume to paused, retaining its checkpoint and releasing admission; a fresh queued scan is cancelled. Queued/running/pausing/cancelling scans occupy admission; paused, interrupted and terminal scans do not. Apply jobs do not occupy scan admission and keep their existing destructive writer locks.

A second `scan.start` returns exit 4, `completion: busy`, `outcome: scan_in_progress`, and `running_job_id` in its detach document (`job_id: null`, empty `run`, null `result`/`log`). The CGI returns this document as data and the UI raises `scan_in_progress`; it shows “已有掃描進行中” and a link to that job. It creates no second job and never silently queues it. An unreadable/corrupt reservation or referenced manifest fails closed; a stopped/missing owner can be replaced. If the runtime registry is missing after reboot, queued scans in the configured state directory still block admission. Queued work is not automatically relaunched at startup; administrators can cancel it explicitly, and the 60-second orphan recovery in section 22.1 also releases admission.

Use one canonical host-local advisory lock per filesystem for scan/cache publication/cleanup apply. Key it by resolved filesystem identity under a fixed private runtime directory, independent of user-selected `--state-dir`, so two state locations cannot bypass exclusion. Multi-filesystem jobs acquire identities in deterministic order or process one filesystem at a time; `low` processes them sequentially.

Immutable report readers remain usable during a scan. Another writer gets a clear `busy` result; no hidden queue or duplicate background job. The holder records itself in the lock file (`{"job_id", "command"}`; `command` is `scan`, `cache-clear`, `cleanup-apply` or `doctor`, and `job_id` is null except for scans), and a `busy` error carries it as `holder` when the file names one (a lock taken by another tool, or one read while being written, has none). A new scan job that finds every filesystem busy did no work: it is removed, so `jobs` does not list it and the result's `job_id` is null. A resumed job that finds every filesystem busy stays `paused` with reason `busy`. These locks coordinate this tool only, not SMB/NFS writers or DSM services. Never infer filesystem consistency from holding the job lock.

`cache clear` refuses active writer/checkpoint use instead of deleting files another job needs. Pause releases writer locks; resume refreshes evidence after reacquiring them. Lock-file existence is not lock ownership and stale PID files are not a reason to kill processes. A crash releases the live OS lock; recovery validates the saved manifests.

### 22.6 Partial results, storage errors, and freshness

Distinguish errors; do not retry everything or silently present a partial scan as complete.

| Condition | Required behavior |
| --- | --- |
| File removed/renamed during scan | Record mutation; skip/reconcile the affected scope, mark approximation |
| Permission denied, locked share, unsupported filesystem/feature | Report coverage gap explicitly; unknown refs never count as reclaimable |
| Kernel metadata I/O/corruption error (`EIO`, `EUCLEAN`) | Stop the affected filesystem scan and warn; no repair, scrub, remount, raw-device fallback, or retry storm |
| Device disappeared, mount changed, filesystem became unavailable | Stop/revalidate identity; never continue against a new mount at the same path |
| Cache/checkpoint corruption | Discard only the affected derived layer and rebuild once; repeated failure surfaces as an error |
| Memory/disk/time limit or user pause | Preserve complete units, report incomplete job, do not overwrite the last complete report |

`scan system` discovers local data filesystems without crawling `/proc`, `/sys`, remote mounts, or every bind alias. Explicit unsupported targets fail; system discovery lists excluded unsupported volumes instead of pretending to include them. Locked/encrypted or app-managed storage is never unlocked/stopped automatically. A system report summarizes each filesystem separately and reports failed/skipped ones.

For partial results show requested/effective scope, errors, unresolved references, and observed bytes. Unknown is not zero. Do not call a partial ranking the complete set of largest cleanup candidates. Partial/mutation-heavy reports can aid inspection, but cleanup must perform fresh target preflight and cannot turn incomplete evidence into a confident estimate.

### 22.7 CLI output and input contract

- Results go to stdout; progress/warnings go to stderr, at most about once per second. `--json` produces one parseable result document, not mixed progress text. `--quiet` suppresses progress, not fatal errors.
- Show phase (`discover`, `scan`, `resolve-refs`, `aggregate`, `publish`), completed units, observed entries/extents, elapsed time, effective profile, cache reuse, and job ID. If no reliable total exists, show counts rather than a fabricated percentage/ETA.
- Human progress lines go to stderr only when it is a terminal and neither `--quiet` nor `--json` is given, at most once per second, such as `synoreclaim: scan 1a2b3c4d: scan, 12/190 subvolumes, 1.20M entries, 3.40M items (23.0k/s), 2:28 elapsed, rss 34 MiB, ETA ~13 min (last scan ~15 min)`. The subvolume total (writable subvolumes) is shown for volume scans only; entries count the trees fully read plus the directory entries read so far in the current tree, not units reused from the cache. Items are metadata items read so far, counted per search batch, so the line and its rate keep moving while one large subvolume namespace or the extent tree is read (measured on a test NAS: a path scan whose entries counted only fully read trees showed 0 entries for its first 2.5 minutes). The ETA is the last completed scan of the same scope minus the time elapsed, and becomes "past the last scan's ~15 min" once exceeded; with no recorded scan there is no ETA.
- Filenames remain byte-safe internally and round-trip through NUL input. Accept `--` before operands; a literal path named `system` is written `./system`. Human output escapes controls/terminal sequences. JSON uses display text plus a lossless encoding for non-UTF-8 paths; no lossy path is ever used for deletion.
- Initial exit contract: 0 complete success; 1 fatal/environment failure; 2 CLI usage; 3 usable partial result or partial cleanup failure; 4 busy/stale-plan/protected-target/filesystem-maintenance refusal; 10 cooperative pause/resource stop or cancel. Signal exits use 130/143/129 for INT/TERM/HUP, with saved job state indicating whether pause completed. JSON has stable symbolic reason codes as well.
- Keep bounded recent diagnostic summaries; do not log every scanned pathname or xattr value by default. Cleanup execution records are separate, private, and never evicted while unfinished. Handle broken output pipes without a panic or hidden ongoing scan; stop/checkpoint and return a documented failure.

### 22.8 Production release gates, not extra services

Ship bounded behavior and clear limits before claiming universal NAS support. Pin Cargo.lock/dependency versions and cache/plan formats; publish reproducible target builds without running remote install scripts. Validate x86_64 DSM first; aarch64 has its own ABI/runtime gate and is not supported merely because cross-compilation succeeds. Install binary/wrapper as root-owned files; unsupported required features fail closed.

Build statically linked against musl for each target, because DSM's glibc version varies and must not be assumed. Validate the DSM platform/kernel list explicitly (at least the 4.4 and 5.10 families) rather than inferring support from architecture.

Staged release:

1. **Analysis-only.** `volumes`, `doctor`, `scan`, `top`, `inspect`, `simulate-delete`, `shared`, and `cleanup plan`.
2. **Cleanup preview.** `cleanup apply/delete` are enabled only when a root-owned configuration file opts in.
3. **General availability.** Cleanup is on by default after the confinement, interruption, and load gates pass on the reference platforms.

At every stage a root-owned kill switch (a config key) disables all mutating commands fleet-wide without a reinstall. Uninstalling leaves no system configuration changes; private state is removable with `cache clear` or by deleting the state directory.

Open decisions before GA (owner: product/platform, not resolved by this spec):

- distribution channel: decided 2026-10-05, a third-party SPK released unsigned. It runs as its own package user; root is obtained only through the helper after the user's one-time DSM-password authorization;
- install paths that survive DSM updates;
- the supported DSM version/model matrix;

No daemon, network port, task scheduler, telemetry backend, live watcher, persistent per-inode change journal, automatic dedupe, or compulsory cgroups are required. Exact-record resume, general disk-backed database indexing, adaptive I/O management, and hostile-writer transactional deletion are not promises of V1.

## 23. Technical references and review limits

The following primary references inform ABI/scheduling/failure semantics; product defaults and scope choices above are design decisions. Upstream Linux documentation is not proof that a Synology-patched kernel implements every path identically.

- Linux 4.4 Btrfs ioctl implementation: `search_ioctl`, `copy_to_sk`, `btrfs_ioctl_tree_search_v2`; compound key bounds, per-call traversal, generation filtering, and buffer handling: https://raw.githubusercontent.com/torvalds/linux/v4.4/fs/btrfs/ioctl.c
- Btrfs on-disk format (extent identities, file-extent records, and backrefs): https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html
- Btrfs qgroups (snapshot/shared ownership and exclusive accounting): https://btrfs.readthedocs.io/en/latest/Qgroups.html
- Linux `ioprio_set(2)` (I/O classes, scheduler dependence, and asynchronous-write limits): https://man7.org/linux/man-pages/man2/ioprio_set.2.html
- Linux `setpriority(2)` (CPU nice scheduling): https://man7.org/linux/man-pages/man2/setpriority.2.html
- Linux `open(2)` / `unlink(2)` (directory FDs, pathname races, and open-file lifetime): https://man7.org/linux/man-pages/man2/open.2.html and https://man7.org/linux/man-pages/man2/unlink.2.html
- Linux `fsync(2)` (file and directory durability): https://man7.org/linux/man-pages/man2/fsync.2.html

- Btrfs backref walking and `LOGICAL_INO` (v1 on 4.4; v2/`IGNORE_OFFSET` added in 4.15): https://raw.githubusercontent.com/torvalds/linux/v4.4/fs/btrfs/backref.c and https://raw.githubusercontent.com/torvalds/linux/v4.15/fs/btrfs/ioctl.c

This review changed the specification only. Pause/resume, priority effectiveness, cache correctness under delayed writes, authoritative snapshot backrefs, eCryptfs mapping, memory capacity, cleanup load impact, and deletion race protection still require implementation and the tests in section 18. Earlier scan prototypes did not validate them. Section 4.6 layouts and the kernel-family list are assumptions to confirm on DSM (confidence: moderate).



## System-partition cleanup modes

See [the mode contract](docs/rootfs-cleanup-modes.md) for standard/deep selection, effects, estimates, approval and compatibility.
