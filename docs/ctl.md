# AI and CLI tasks

> Toolchain: CLI `0.48.11`; Rust and Go SDK `v0.50.5`; templates `0.40.1`.

Use the pinned Syno Package Land CLI. Register the NAS with
`synopkgland host add nas --url https://nas.example:5001 --username admin`.
Enter credentials interactively or through `--password-stdin`, never in command
arguments or task JSON. Discover the installed version with
`synopkgland ctl list --host nas --app SynoReclaim --json` and approve its
fixed authenticated description endpoint when prompted. The App alias is `reclaim`.

Reclaim remains administrator-only. The existing SDK executor must already have
its digest approval from the DSM UI. ctl cannot authorize it, change its protected
policy, or bypass cleanup's checks. A normal user is refused before the
App invokes the executor. Description approval and `--yes` are not root grants.

## Commands

The paths follow `synopkgland ctl reclaim`. The single source is
[`operations.json`](../operations.json); the CLI emits the complete
[operation reference](../generated/operations.md), command schemas, AI tools,
route bindings and permissions. Use discovery or `--help` for typed inputs.
Private paths and selections use schema-marked redaction.

Run `npm run generate:operations` after editing the registry, then
`npm run check:operations` after preparing the root payloads. The App gate runs
`ctl generate --check` before package validation. The small repository generator
derives the Rust manifest's required `backend.methods` build input; actual Rust
handlers are tested for equality in both directions. The pinned CLI's Rust builder
requires this explicit method input for cross-compilation.

All existing mutations are AI `draft` operations with a matching `preview.*`
handler accepting exactly the same parameters. A preview checks authorization
and exposes only sanitized identifiers/counts and fixed submission risks. It
does not create a sealed plan, certify its freshness or simulate deletion.
Review the original sealed plan in the DSM UI, then use the separate human
`--yes` command. The AI tool interface never executes a draft mutation.

`settings update` sets only the existing mutation kill switch. Clearing it does
not enable cleanup or authorize the executor. `cache clear` deletes App cache,
not user files. Snapshot deletion remains in DSM Snapshot Replication; there is
no new empty-bin, restore, arbitrary-command or filesystem API.

## Start long operations without waiting

Scan start/resume, cleanup planning, system cleanup planning and both applies
use `job` flows with SDK-owned request budgets and no total wait limit.
They submit once, poll `jobs get` for the exact product
ID and read `jobs result` when completed or partial. `--no-wait` returns the
admission in `job`; inspect `job.accepted` and retain its `job_id`. Acceptance
is not completion. Busy scan admission preserves `accepted:false` and the
existing `running_job_id`; it never launches another scan. Polling failures,
timeouts and terminal refusals never resubmit the start.
Each request uses the shared SDK default budget; no App-specific `timeoutMs`
is declared. A live job keeps polling until a terminal state or cancellation.

The existing system-report endpoint can either return a cached document or
accept a collection job. Its read-only `invoke` flow preserves both real shapes;
this conditional protocol cannot use the CLI's unconditional job flow. On
`accepted:true`, use the returned ID with `jobs get` and `jobs result`. This is
an explicit boundary of the current SDK flow contract, not a completed report.

Planning and system reports from the DSM UI use the same asynchronous App APIs.

```sh
synopkgland ctl reclaim scans start --input scan.json --host nas --dry-run
synopkgland ctl reclaim scans start --input scan.json --host nas --yes --no-wait
synopkgland ctl reclaim jobs list --host nas
synopkgland ctl reclaim scans get --job-id JOB_ID --host nas
synopkgland ctl reclaim jobs result --job-id JOB_ID --host nas
synopkgland ctl reclaim jobs cancel --job-id JOB_ID --host nas --dry-run
synopkgland ctl reclaim jobs cancel --job-id JOB_ID --host nas --yes
```

`scan.json` is `{"targets":["/volume1/example"],"mode":"gentle","snapshots":false}`.
All path parameters are NAS paths; the `--input` file is local. Scan start writes
job/report state but does not delete selected files. `gentle` is the low-load
profile; `fast` uses more resources. Only scans support pause, resume and mode.

A newly accepted scan returns `{"accepted":true,"job_id":"..."}`. If the existing
NAS-wide admission finds a queued/running scan, it returns
`{"accepted":false,"outcome":"scan_in_progress","running_job_id":"..."}`.
That ID belongs to the existing scan, whose targets may differ from this request.
No additional scan is launched and the new parameters were not accepted.

Keep the returned product ID across ctl restarts and disconnections. Subsequent
calls authenticate again and read the same persisted App job. Do not confuse this
ID with the SDK executor's internal 64-hex ID. On an uncertain start response,
inspect the existing jobs before submitting again; ctl does not retry a start.

Use `jobs list` to inspect planning and apply progress as well as scans. Planning
kinds are `cleanup-plan` and `rootfs-plan`, with phase `plan`. System report
collection uses kind `rootfs-report` and phase `report`; all three expose
top-level `elapsed_s` wall seconds. Unknown totals do not imply a percentage or ETA. Scan progress exposes
its existing numeric counters, elapsed time, optional previous-run basis and
update timestamp. A previous-run basis is an estimate, not a completion promise.

`rootfs report` with `max_age_s` above zero answers with the saved report when
it is at most that old (30 days at most), without a job. Otherwise it returns an
ID: use `jobs list` and `jobs result`. `max_age_s: 0` always walks `/` again,
and a rootfs apply drops the saved report. A partial report remains partial.
While a `rootfs-report` job is queued or running, `rootfs report` returns that
job's ID instead of starting a second one.

`completed`/`partial` jobs can have results; failed planning jobs retain their
existing product diagnostic document. A missing result is not success. Check
`exit`, `completion`, `error_count`, `error_codes`, coverage and null
statistics. ctl's outer `ok` indicates request delivery; it does not mean the
underlying operation completed successfully. A partial scan is partial coverage.

`job.result` has no pagination/sorting parameters. `top list` provides the existing
saved-report ranking with `by` and a bounded `limit`; it is a separate target-based
read. No new result pagination protocol is introduced.

## Resume after a package upgrade

`jobs pause` checkpoints the scan before it exits. After an upgrade and any
required executor digest approval, use `jobs resume --job-id JOB_ID --yes` to
continue that same product job. Reclaim owns the job manifests, live locks,
pause requests and checkpoints; the SDK owns process admission, root execution
and package-stop signalling.

Resume validates checkpoint formats, checksums and volume/subvolume identities
before it queues a worker. An unchanged unit reuses its sealed result or partial
namespace search position. Ordinary writes never refuse resume: changed units
are reread from their beginning, while every other unit keeps its checkpoint.
The current `find-new` data does not establish a complete namespace delta for
deletions and renames, so this per-unit reread avoids mixing old and new data.
Job metadata items/bytes count actual I/O across attempts without counting saved
checkpoint replay again; `progress.restart` reports the current unit's separate
reread counters. The whole job does not reset to zero.

Missing/damaged/incompatible checkpoints, deleted or replaced subvolumes,
changed filesystem identity and relocation still return `checkpoint_unusable`;
incompatible job formats return `job_incompatible`. Refusal keeps the stopped
job and does not start a worker. The UI offers one-click recovery; the equivalent
ctl command is `jobs resume --job-id JOB_ID --fresh --yes --no-wait`. This explicit
recovery cancels the stopped job and creates a new scan from all original saved
targets, scope, snapshot choices and resource limits, retaining existing reports.
It returns the new job ID, rather than treating the old checkpoint as valid.

An upgrade may change a unit through package writes. Only changes proven to lie
entirely inside Reclaim's own state directory may retain its saved generation;
other writes trigger the same per-unit reread as business data. The worker
rechecks identities and integrity at use time; later unsafe changes remain
visible through explicit codes in `jobs result` and `jobs list`.

## Snapshot space and simulations

```sh
synopkgland ctl reclaim snapshots list --target /volume1/homes --host nas
synopkgland ctl reclaim snapshots list --target /volume1/homes --simulate older-than:30d --host nas
```

`results[].shares[]` retains `live_subvolume_id`,
`held_only_by_snapshots_bytes`, `unknown_bytes` and `snapshots_not_materialized`.
Each `snapshots[]` entry retains `subvolume_id`, `exclusive_bytes`,
`shared_with_live_bytes`, `unknown_bytes`, `qgroup_exclusive_bytes`,
`pinned_unreferenced_bytes`, `materialized`, `read_only` and `pending_deletion`.
Null byte values mean unmeasured, never zero.

`--simulate all` or a supported selector adds each share's `simulation` with
`selected_ids`, `not_materialized_ids`, `freed_bytes`, `unknown_bytes`,
`upper_bound_bytes`, `exact` and `inexact_reasons`. `freed_bytes` is the lower
bound for deleting the whole selected set together; `upper_bound_bytes` is null
when selected snapshots were not read. Do not sum individual exclusive sizes
as a set estimate. ctl omits private names, selector text and suggested commands.

The backend's completion is preserved: exact, fully measured answers complete;
partial answers exit 3 rather than promising complete accounting. Inspect share
`unknown_bytes` and `snapshots_not_materialized`, and simulation reasons
`unknown_owners`, `not_materialized` or `cross_share_sharing`. Even an exact
selected-set simulation can be part of a partial listing if other snapshots of
a returned share have unknown bytes. Re-scan with all snapshots and larger
owner budgets when that is the missing evidence; unsupported sharing can remain
inexact. Snapshot deletion stays in DSM Snapshot Replication.

## Environment diagnostics

```sh
synopkgland ctl reclaim diagnostics get --host nas
synopkgland ctl reclaim diagnostics get --volume /volume1 --host nas
```

With exactly one mounted data volume, omitting `--volume` selects it. With
several, `volume_required` lists the available volume roots and asks for
`--volume`; no diagnostic checks run until one is selected. No mounted volumes
returns `no_volumes`; failed discovery returns `volumes_unavailable`. An explicit
volume uses the existing backend validation without an extra discovery call.

## Plan and apply remain separate decisions

`--dry-run` previews the CLI submission and does not call the App's planning API.
To create the actual immutable cleanup plan, put the selection in `selection.json`
(`{"paths":["/volume1/example"]}`; report-selected identity objects are also accepted)
and explicitly submit:

```sh
synopkgland ctl reclaim cleanup plan --input selection.json --host nas --dry-run
synopkgland ctl reclaim cleanup plan --input selection.json --host nas --yes --no-wait
synopkgland ctl reclaim jobs list --host nas
synopkgland ctl reclaim jobs result --job-id PLANNING_JOB_ID --host nas
```

Read the completed plan's exact `plan_id`, size, expiry, shared folders (each
with its Recycle Bin settings) and refusals. A standard system-partition plan instead uses `rootfs plan` with input
`{"to":"/volume1/backup","mode":"standard"}` and returns its `plan_id` in the
saved result. `deep` is an explicit existing mode with additional cache/preference
effects. Review the plan in the DSM UI when private paths or detailed effects
are needed. No command automatically plans and applies.

To move the plan's items into their shared folders' Recycle Bins, prepare
`apply.json` with the exact ID and `{"plan_id":"cleanup-aa-bb","confirm_move":true}`. For system cleanup use
`{"plan_id":"rootfs-aa-bb","confirm_no_restore":true}` and `rootfs apply`.
A plan whose `deletes` is not 0 removes items with no Recycle Bin for good:
items in a folder that is no shared folder, and items of a shared folder whose
Recycle Bin is off (`shares[]` marked `delete`; such a shared folder has
`recycle_bin.enabled: false`). Turning that Recycle Bin on in DSM and planning
again moves them instead. Add `"confirm_delete":true` only after reading which. These booleans are required in addition to CLI `--yes`:

```sh
synopkgland ctl reclaim cleanup apply --input apply.json --host nas --dry-run
synopkgland ctl reclaim cleanup apply --input apply.json --host nas --yes --no-wait
synopkgland ctl reclaim jobs list --host nas
synopkgland ctl reclaim jobs result --job-id APPLY_JOB_ID --host nas
```

The App checks each item again, the plan's freshness, the cleanup configuration,
the kill switch and the existing system/destination/update guards. Cleanup moves
items into the Recycle Bin, which the user empties or restores in DSM; space is
freed only after it is emptied. Items with no Recycle Bin are deleted for
good: the plan result counts them in `deletes`, and the apply result counts
`deleted` and `deleted_bytes` apart from `moved` and `moved_bytes`. An item
whose shared folder's Recycle Bin is turned off after the plan is deleted for
good too, when apply reaches it. Cancellation uses `jobs cancel`, stops between
DSM's runs and does not bring back items already moved or deleted. A late request can be refused as too late.
Executor/package stop or crash can interrupt work; saved IDs do not promise reboot
continuation. A cancelled planning job does not return an applicable plan through
its result. Private sealed artifacts follow the App's existing retention policy.

## Sensitive data and result boundary

Private path/selection/destination parameters declare JSON Schema `writeOnly`.
CLI plans, dry-run, errors and output use the installed generic redaction. Every
ctl binding also uses Reclaim's fixed typed projection: numeric statistics, known
enum states/phases, product job IDs and sealed plan IDs. It excludes arbitrary
paths/path hex, share/file/process names, requester names, argv, run/result file
locations, credentials, free-form messages and unknown future fields.

Support and environment tasks expose reviewed statistics/check statuses, not an
unrestricted export of diagnostic text. Ranked rows preserve their order and
metrics; use the DSM UI for private names. Every backend error exposes its Reclaim `code` in `error_codes` and an `errors`
entry with a fixed safe message, including nested filesystem errors and stored
job-result codes. A malformed or missing code becomes `ctl_failed`; a positive
`error_count` always has a code. Raw diagnostic text remains private.

The pinned SDK's `root.jobs` JSON result limit is 16 KiB and stdout is diagnostic,
limited to 4 KiB. Reclaim continues to use its existing protected product
`jobs/<id>/result.json` and `job.result` for complete reports/plans; fd 5 sends the
existing small job summary. This does not add SDK result storage, opaque file
capabilities or a new paging layer. Reclaim's legacy product IDs/busy shape and
failure documents differ from ordinary SDK background jobs; its admin/executor
policy remains unchanged.

Remove the local login when finished:
`synopkgland host remove nas --yes`.


Deep system results retain `cleanable.deep` totals, per-category accounting,
fixed DSM tool names (including `delete_log`) and `estimated_bytes` where the
backend provides it. `tool_cache` and `backup_installer` remain distinct
categories. `delete_log` invokes DSM's existing log retention cleanup and has
no copied log backup. Manual `follow_up: backup_installer` means update DSM or
copy the moved restore installer back before full-system Hyper Backup restore
is available. The existing backend decides when that installer is eligible;
ctl does not override the update-room guard. `jobs result` declares these
public fields in its result schema while omitting paths, argv and free text.
