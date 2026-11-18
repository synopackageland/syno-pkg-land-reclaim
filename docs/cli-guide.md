# CLI usage and operational limits

Use `synoreclaim --help` and each subcommand's `--help` for current options.
For installed-app automation, use the [ctl guide](ctl.md). The
[specification](../SPEC.md) defines accounting and cleanup certification.

## Scans and saved answers

Scans accept a system, volume, single path or set of paths. Overlapping targets
are deduplicated; combined reclaimable space is computed from shared extent
references, not by summing independent estimates.

```sh
synoreclaim scan /volume1/share /volume1/homes --profile low
synoreclaim jobs
synoreclaim pause JOB_ID
synoreclaim resume JOB_ID
synoreclaim resume JOB_ID --min-free 1GiB
synoreclaim scan /volume1 --max-memory 256MiB --max-runtime 30m
```

`overview <volume>` reads the latest volume report. Run a volume scan first;
the answer does not silently start one. It attributes allocated data extents to
snapshots, Recycle Bins, DSM services, live shared folders and unknown references,
then reconciles the result with Btrfs allocation and filesystem usage.

`top` includes service and hidden data, with retained/unknown bytes and the DSM
location that manages each category. `inspect --explain` identifies known
service paths. `simulate-delete` explains remaining references, including
snapshots, hard links, reflinks, the Recycle Bin and pending Btrfs cleanup.
`volumes` and `doctor` also report deleted subvolumes awaiting the Btrfs cleaner.

The default `low` profile paces CPU and I/O use; `normal` is unpaced and `fast`
uses more parallel metadata reads. Profiles preserve the accounting rules.
`--pinned-read-budget auto` scales metadata I/O independently of the memory
guard. A completed report can be refined with
`scan <volume> --attribution-report <saved-scan-id>` without walking live folders
again. See [adaptive attribution](adaptive-attribution.md).

Scans record coverage, consistency and resource-limit reasons. A partial answer
is not complete coverage. A path scan still reads its subvolume's namespace,
so its minimum memory requirement depends on that namespace, not just the path.
Terminal progress goes to stderr; `--quiet`, `--json` and non-terminal output
disable it. Saved-job resume preserves resource limits unless overridden.
See the ctl guide for checkpoint validation and explicit fresh-scan recovery.

## Snapshots

A volume scan lists snapshots by shared folder; detailed snapshot analysis is
optional. `scan --snapshots` reads a bounded selection and `--snapshots all`
reads all snapshots. `scan <volume> --snapshots all --snapshot-report <saved-scan-id>`
adds analysis to a saved full-volume scan without walking live folders again. Analysis has its own
elapsed time and checkpoints completed shared folders for pause/resume.

`snapshots <volume|share>` reports exclusive and shared space from an analysed
report. Its `--simulate-delete` option estimates a snapshot set as one combined
operation. Unread snapshots and unresolved owners remain unknown. Owner lookup
budgets and the memory guard bound the work; existing consistent quotas provide
an optional cross-check without enabling or rescanning quotas.

Delete shared-folder snapshots in DSM Snapshot Replication and LUN snapshots
in SAN Manager. Reclaim does not delete snapshots.

## Cleanup boundaries

`cleanup plan` checks the selection and groups it by shared folder; `cleanup
apply` checks each item again, then moves it into its shared folder's Recycle
Bin with DSM's own `synorecycle` (fixed argv, no shell) and reports what moved
and what stayed. `cleanup delete` combines planning, confirmation and apply.
Apply runs as a job with progress and cancel; a large folder is handled folder
by folder so progress and a cancel reach inside it. Destructive work never
resumes automatically. DSM's Recycle Bin keeps no symlinks, nor folders that
hold only symlinks, and a restore does not bring them back, as with File
Station's delete: the plan counts them (`symlinks` per shared folder and
item, from the scan report) and Review says how many before the move.

An item with no Recycle Bin, in a shared folder whose Recycle Bin is off or in a
folder directly under the volume that DSM does not list as a shared folder, is
deleted for good instead, only after the user confirms it (`--confirm-delete`).
Space is freed once the Recycle Bin is emptied in DSM, then by Btrfs after its
transaction commit; snapshots and open handles can keep it.

Cleanup uses a fixed packaged executor, explicit opt-in and a protected mutation
kill switch. Volume and shared-folder roots, Btrfs subvolumes, Recycle Bins,
system folders, DSM-managed directories and protected package data are refused
with guidance to the owning DSM application. Reclaim never removes anything from
a Recycle Bin. Encrypted eCryptfs shares are unsupported for cleanup until name
mapping is validated.

A changed scan records `changed-during-scan` and its reasons. Cleanup requires
the selected subvolumes to be stable; unrelated subvolume changes do not alone
invalidate a selection. Apply rereads reclaimable extents before removal and
counts changed references as retained.

## State storage and free-space guards

The default state directory is `/var/lib/synoreclaim`, on DSM's system partition.
Keep a custom `--state-dir` private and outside shared folders: ownership and
mode checks do not detect DSM share ACLs that grant access to other users.

Keep state and spill writes off the filesystem being scanned. They advance the
containing subvolume's generation, can mark the scan as changed and can prevent
cleanup certification for that selection. `doctor` and scan warnings identify
these conflicts; the scan itself is not refused solely for that placement.

Scans check free space on both state and spill filesystems. On the system
partition they retain at least the larger of `--min-free` and 3% of capacity,
pausing resumably with `disk_limit` when needed. `doctor` reports capacity and
may suggest another volume. A data-volume scan never scans the system partition.

Use `rootfs` for its separate read-only system-partition report and planned
cleanup. Moved files are copied and verified before removal; DSM's own cleaners
can permanently delete logs or caches. There is no automatic restore command.
See [system-partition cleanup modes](rootfs-cleanup-modes.md) for exact policies,
effects, approvals and manual follow-up.
