# UI fixtures

Recorded CLI JSON that the UI's fixture backend serves in place of the DSM
WebAPI (dev server outside DSM, and every test). The UI calls
`api(method, params)`; the fixture backend answers it
from this folder.

## Layout

```
ui/fixtures/
  <method>/                 one folder per WebAPI method, e.g. volumes.list/, top.list/
    index.json              which file answers which params in which scenario
    <name>.json             one CLI document, exactly as `synoreclaim webapi <method>` printed it
```

- `<method>` is the WebAPI method name from SPEC.md 5.21
  (`session.get`, `volumes.list`, `jobs.list`, `overview.get`, `top.list`,
  `rootfs.report`, `rootfs.plan`, …).
- `<name>.json` is the command's stdout document unchanged (the `data` the
  shim returns). Lower-case, `[a-z0-9_-]`, describe the content:
  `volume1.json`, `volume1-share-docs.json`, `no-report.json`.
- Sanitize before committing: share, user and file names from real NASes are
  replaced by neutral ones (`share01`, `admin`, `file0001.bin`), and path_hex
  values are recomputed from the replaced path. Keep numbers as recorded.

The recorded `recycle.list` set has real `/volume1` aggregates and a sampled
share's oldest items. The real scan exposed only six oldest rows for that share;
the synthetic `share01` fixtures keep the full 200/1000-row pagination case,
where the 1000 rows add up to the share aggregate and the first 200 match
`oldest:200`.

`job.pause` and `job.cancel` are not recorded. Their answers follow the
`job_request` schema for the job of `jobs.list/running.json`: `requested` with
state `pausing` for the running job (normal), and for the `paused` scenario
`not_running` (pause) and `cancelled` (cancel, which ends a stopped job at once).

For `rootfs-apply`, `job.cancel` requests a stop after the current batch; the
job then finishes with `completion: paused`.

## index.json

A JSON array. Each entry answers one call:

```json
[
  { "file": "volume1.json", "params": { "volume": "/volume1" } },
  { "file": "volume1-partial.json", "params": { "volume": "/volume1" }, "scenario": "partial", "exit": 3 },
  { "scenario": "busy", "document": { "completion": "busy", "exit": 4 } }
]
```

| Field | Meaning |
| --- | --- |
| `file` | The document to return (relative to the method folder). |
| `params` | Optional. The entry matches when every listed key equals the call's param (deep equality); keys not listed match anything. Omit it to match every call. |
| `scenario` | Optional. The entry is used only in that scenario. Entries without it are the `normal` scenario. |
| `exit` | Optional, default 0. The CLI exit code recorded with the document (informational: a JSON document resolves `api()` whatever its exit; pages read `completion`). |
| `document` | Instead of `file`: the CLI document inline. As over the CGI, a refusal document (`exit` 2 with `outcome: invalid_param`, `exit` 4 with `completion: busy`) fails the call. |
| `error` | Instead of `file`: the call fails with this SDK envelope error, `{ "code": <SDK code>, "message"? }`. |

Lookup: the entries of the current scenario are tried first, in file order,
then the `normal` entries; the first whose `params` match wins. No match is a
test failure (the backend throws `fixture_missing`).

## Scenarios

Selected in dev with `?scenario=<name>` in the dev server URL (or the dev-only
select in the toolbar); tests call `setBackend(fixtureBackend({ scenario }))` (`src/api`).

| Scenario | Shows |
| --- | --- |
| `normal` | Sanitized current test-NAS capture set. Its `/volume1` report remains visibly partial; the recorded real running job is available separately as `scanning`. |
| `no_report` | Volumes listed, nothing scanned yet (`no_report` target errors, `scan_hints`). |
| `scanning` | The real test-NAS whole-volume job capture with progress. |
| `interaction` | Stable test-only interaction data retained from the previous fixtures, including complete recycle pagination. UI interaction tests select it explicitly; it is not the normal/dev fixture set. |
| `scan_test` | Stable test-only running-job data for detailed progress behavior tests. It does not replace the real `scanning` capture. |
| `share_scanning` | A `running` share scan of `share04` whose entries count within the tree it reads. |
| `paused` | A `paused` (or `interrupted`) job. |
| `paused_real` | The recorded short test-NAS selection scan paused, then resumed successfully. |
| `partial` | Reports with `completion: partial` / `consistency: partial`, `null` byte values. |
| `partial_stale` | As `partial`, with `stale: true`: Explore shows the partial and stale notices together. |
| `busy` | Calls answer `busy` (`completion: busy`). |
| `invalid_param` | A call is refused with `outcome: invalid_param` for an invalid request. |
| `error` | Calls fail (`completion: failed`, or the `cli_error` error). |
| `not_admin` | `session.get` fails with 105. |
| `days` | `history.list` with the recorded scans one day apart, newest first (numbers as recorded), for the History trend. |
| `many_snapshots` | `snapshots.list` with two shares and several snapshots (read, unread, pending deletion), held files and inexact / exact simulations; not recorded, built to the schema's rules: held ≥ the sum of exclusive bytes, a set frees at least its members' exclusive bytes, unread snapshots have null bytes. |
| `no_snapshots` | Volumes scanned, but no report was made with `--snapshots` (`snapshots.list` answers `no_report` with a scan hint). |
| `share_scan` | Only share scans finished on `/volume1` (the recorded history without its volume scan): volume answers are `no_report`, `share04` answers from its share report. Every answer describes the one scan `fixture-job0011` (history, jobs, report headers, volume `last_scan`). Its `recycle.list` documents for `share04` are synthetic, built from `share06`'s recorded one; its `top.list` and `jobs.list` documents are the recorded ones with that scan's header. |
| `packages` | `top.list {target:/volume1, kind:all}` with 12 service lines (summed and per-path lines, a layout that is not confirmed, an unknown size, snapshots not read, an iSCSI line with its UNMAP explanation and a Hyper Backup line with 17 paths) for the Packages page; derived from `volume1-all.json`. |
| `review_cleanup` | A sanitized plan, detached cleanup-apply job progress, and its final apply document. |
| `review_cleanup_refused` | A queued cleanup-apply job that ends failed with a refusal document from `job.result`. |
| `review_long_refusal` | A synthetic 40-entry `target_changed` refusal list, long enough to scroll the Review dialog body; used for the 1100×720 dialog-layout screenshots. |
| `changed_system` | `/volume1` is `changed-during-scan` from `subvolume-generation-changed` of subvolume 256 (a system area): the report chip tooltip and the quiet note under the header say the change was only in system areas. Its `volume1` shares use subvolume 262, so 256 is not a share. Overview, Explore (`top.list`), Recycle Bin (`recycle.list`) and Snapshots (`snapshots.list`) all answer from such a report. |
| `changed_share` | As `changed_system`, but the changed subvolume (262) is a shared folder: the plain wording and no system-areas note stay. |
| `sysvol_getting_full` | `volumes.list` whose `system_partition` is 89 % used with 930 MB free (the §2 8.5 GB layout): Overview shows the warning banner and the System partition card, and no Full actions row. `rootfs.report` answers the golden `rootfs.json` with that filesystem. |
| `sysvol_full` | As `sysvol_getting_full`, but 380 MB free (below the update room): the danger banner, the Full card and the Full actions row, and the Full System partition view. |
| `sysvol_partial` | As `sysvol_getting_full`, with a `rootfs.report` whose walk stopped early (`walk.complete=false`, `completion: partial`): the System partition view warns that sizes may be low. |
| `sysvol_small` | A 2.5 GB system partition getting full where moving everything it can would still leave it getting full: the System partition view adds the small-partition note. |
| `sysvol_nothing_movable` | As `sysvol_getting_full`, with `rootfs.cleanable.items=0`: the System partition view names the largest owner instead of offering a cleanup. |
| `small_items` | A deep level, `/volume1/share62/folder0176/folder0177`, holding one 5 MB file beside six small folders (one of 0 B) and 150 small files, some held by snapshots; its `logs-archive` folder opens to a few files. Synthetic, built from `folder-depth-1-all.json`, for the Explore treemap's folder and file tiles and its grouped small items. |
| `rootfs_cleanup` | Rootfs report and plan previews with eligible, low-space and busy destination shares. |
| `rootfs_no_room` | All listed shares have less free space than the plan requires. |
| `rootfs_no_shares` | No shared folders are available as a destination. |
| `rootfs_destination_full`, `rootfs_busy`, `rootfs_walk_stopped`, `rootfs_dsm_update_active`, `rootfs_stale`, `rootfs_nothing`, `rootfs_tools_only` | Plan refusals and edge cases for the cleanup dialog. |

The hand-written rootfs WebAPI methods without a JSON schema yet are in
`rootfs.apply/` and `rootfs.history/`. The history fixtures include populated
and empty `applies`. `jobs.list` rootfs progress
and `job.result` rootfs documents are supplied by the dev-only apply fixture
page and tests until the rootfs job schema exists. They are not inserted
into the older base jobs schema here.

A scenario only needs entries for the methods whose answer differs from
`normal`. New scenarios may be added; list them here.

## Adding a fixture

1. Record it on a test NAS: `printf '%s' '<params json>' | synoreclaim webapi <method> --requested-by admin > <name>.json`
   (read-only methods only; no new NAS scans for UI work, reuse existing reports).
2. Sanitize (above), put it in `ui/fixtures/<method>/`, add an `index.json` entry.
3. `pnpm -C ui test` validates every fixture against `schemas/v1/*.schema.json`
   where a schema exists for the method. Cleanup job fixtures also assert the
   !68 `kind`, `execute` phase and cleanup-progress contract while the branch's
   base jobs schema awaits that upstream change.

The cleanup review plans are local previews derived from
`tests/golden/json/cleanup-plan.json`; they are not new NAS recordings. The job
fixtures follow the cleanup-apply `kind`, `execute` phase and `cleanup` progress
object. `job.result` carries the apply document, including the queued
job refusal path.

The `rootfs.report` set retains the original `rootfs.json` golden unchanged and
adds `getting-full`, `full`, `partial`, `small`, and `nothing-movable` variants
for the System partition view. The cleanup `report.json` and `rootfs.plan` fixtures are
dev-only previews shaped from the rootfs goldens. That
report includes `cleanable.required_destination_bytes` and the expected
rootfs level; these WebAPI additions have no schema yet. Their documents use
hand-written UI types and fixture contract assertions until the schemas and
API methods exist. `volumes.list` and `jobs.list` also have rootfs cleanup
scenarios for eligible shares, low-space shares, busy volumes and no shared
folders. The fixture contract checks that `report.json`; the other
`rootfs.report` documents remain validated by their page tests until schemas
land.

For a batch of saved CLI outputs, `scripts/ui-fixtures/record.sh --captures <dir>`
also reads an optional `manifest.json` in that directory. Each array entry maps a
`source` JSON file to a fixture `method` and `file`, with optional `params`,
`scenario`, and `raw_only`. Inputs are sanitized together so names stay
consistent; sanitized copies are written under `<dir>/sanitized/`. `raw_only`
keeps an output out of `ui/fixtures` when the UI contract has no schema yet.

`job_recovery` is a synthetic dev scenario derived from the scan and cleanup
recordings: one running cleanup on the NAS. It verifies
job-kind links and follow-only Review with empty browser storage. Its counts
are fixture data, not a live NAS measurement.

The cleanup identity variants and `review_target_changed` refusal are
local dev previews. Oldest-item fixture tree/inode fields are synthetic
identity examples, not new NAS measurements. Legacy path-only plan requests
remain covered.

The plan-cert scenarios `review_partial_unrelated`, `review_selected_changed`,
`review_snapshot_missing` and `review_selection_partial` are synthetic dev previews.
The partial plan's scope evidence is a safety-contract example, not a NAS measurement.
They exercise confirmation with unrelated partiality and three actionable refusal
reasons, in addition to the legacy identity fixtures.

`usage_change` is synthetic UX data derived from `sysvol-full`/`interaction`:
volume1 is +50 decimal GB and volume2 is −100 GB against complete 1 TB df baselines.
It keeps the independent Full system-partition warning. Open once, then reload
(the persisted first crossing becomes the confirming sample). Close/snooze are
real localStorage records; clear only `synoreclaim.usage.v1:*` to repeat this demo.
These capacities and deltas are not NAS measurements.

The synthetic cleanup.plan partial-unrelated and selected-changed scenarios set
planning_attempts=2 to represent a successful bounded re-plan and two final
generation refusals. These counters are diagnostic; they are not NAS measurements.
