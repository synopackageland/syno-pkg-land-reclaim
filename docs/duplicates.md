# Duplicate content (Btrfs, metadata only)

This is the P1 duplicates feature that SPEC section 6 describes: it finds files
whose content is the same but that are stored in different physical extents.
It reads Btrfs metadata only and never reads file data.

## Tier 0: the scan record

While a scan stores a unit, it writes one 24-byte record for each regular file
of at least 1 MiB (`duplicates::candidates::MIN_SIZE`) into the unit's
`candidates` file. The unit's tree and extents files sit beside it.

| Bytes | Field |
| --- | --- |
| 0–7 | inode objectid |
| 8–15 | size in the low 48 bits; flags in the top 16 (`1` compressed first extent, `2` more than one name) |
| 16–23 | first block: the first physical extent's `disk_bytenr` plus its `offset` |

The record comes from the extent references the scan already spilled for the
unit: it is the first reference of each inode, in tree order. The scan makes no
new search and reads no data. A sealed unit keeps its records, so a reused unit
still has them. The unit version includes whether the records were taken.

When the scan publishes a report, it also writes every unit's records into a
private sidecar beside the report: `<report>.srcand`. The layout is:

- the magic `SRCAND1\n`;
- for each unit: its tree id, its record count, then its records;
- `u64::MAX`, the total record count and `SRCEND1\n`;
- an FNV-1a 64 checksum of everything before it.

The sidecar is removed when its report is pruned. It is display data: a
missing or damaged sidecar means the duplicate analysis must wait for a new
scan; it never affects the report. The UI cache fingerprint ignores it.

Cost: the survey measured this record on a 1M-file loop-device fixture. A scan
took 12.47 s without it and 12.46 s with it; the difference is within the ±1%
noise. RSS grew by 1.3 MB. The ignored test
`scan::tests::duplicate_candidates_cost_no_more_than_noise` guards the scan
code itself. It scans 200,000 files with and without the record, and fails if
the median scan with the record is more than 5% (plus 20 ms) slower:

```sh
cargo test --release --lib -- --ignored duplicate_candidates_cost --nocapture
```

## Analysis

`synoreclaim duplicates analyze VOLUME` runs as its own job. It uses the newest
whole-volume report and its `.srcand` sidecar, reads metadata only, and saves
the result beside the report as `<report>.srdup`: private JSON, replaced
atomically. A new scan does not run it. Until the analysis runs again, the
answers say `stale` and show the older result.

The `.srdup` holds what the pages read (format 4). The identities that only
`cleanup.plan` needs (each folder member's folder identities and file stamps)
go to a `<report>.srstamps` sidecar: one JSON line per folder group, then a
trailer with its format and an id. Each folder group points at its line by
offset, length and checksum, and the `.srdup` holds the id. A plan reads
only the lines of the groups it selects; a missing, foreign or damaged line
refuses with `duplicates_stale`. A format-3 `.srdup`, which holds them
inline, is split on its first read; if that fails it is read whole.

### Cached answers

The page answers use Explore's UI cache (`src/ui_cache.rs`), with the same
binding, fallbacks and limits. After an analysis ends `completed` or
`partial`, the job stores two kinds of `duplicates.scope` answer:
- the volume root (`{volume, limit: 1}`);
- each share (`{volume, path, list: "both", limit: 200}`).

The root `cli webapi` answers such a request from the cache when its entry
still matches; it still starts, but does not read the analysis. An entry is
bound to the reports fingerprint plus the `.srdup` and the user keeps file
(`duplicates/<fsid>.json`, length, modification time and inode). It holds
only while the analysis is `ready` and not running or recalculating. A
missing or stale entry is computed live and stored again; a damaged one
falls back to the full read. A keep, a new analysis being queued or new
results therefore change the binding at once.

1. **Same size.** Candidates whose size no other candidate has are unique. At
   most 4,000,000 candidates are read (32 bytes each in memory); a larger set
   is cut and the analysis is `partial`.
2. **Names and exclusions.** One streamed walk of the report names each
   remaining inode. All of an inode's names (hard links) form one copy. An
   inode is not a copy if any of its names falls under one of these:
   - a node flagged as DSM system data, package data, this tool's own state,
     encrypted data, a snapshot or a placeholder;
   - a `#recycle` folder: a Recycle Bin copy is never a member;
   - any path component starting with `@`, which covers `@eaDir`,
     `@spamdata`, `@quarantine`, `@ChatWorking`, `@ActiveProtectVault` and
     `@TieringVaultData`;
   - a path `dsm::service_of` or `dsm::package_data_of` classifies.

   Synology Drive folders are user data. So are the destinations of Shared
   Folder Sync, USB Copy, Cloud Sync and Hyper Backup rsync. A size left with
   fewer than two copies drops out.
3. **Content signature.** Each copy's inode item and file extents are read
   again with one search. The copy is left out as `changed` when any of these
   holds:
   - its size or first block no longer matches the record;
   - it is no longer a regular file;
   - it has inline data.

   Otherwise its data checksums come from the checksum tree. The search starts
   one leaf item's reach before each extent; the checksum size comes from
   `BTRFS_IOC_FS_INFO` on 5.5+ kernels and is CRC-32C (4 bytes) before that.
   The signature is SHA-256 over these values:
   - the size;
   - for each 4 KiB sector in file order: a data byte and its checksum, or a
     zero byte for a hole or a preallocated range.

   Copies with the same extent layout (clones) reuse one signature. A copy is
   `unchecked` and is never grouped in two cases:
   - `nodatasum` (NOCOW) data, or a sector without a checksum;
   - a compressed extent.

   `unchecked` counts such files only when another copy of their size exists.
   No file data is read, so P1 cannot check them.
4. **Groups and clusters.** Copies with the same size and signature form a
   group of two or more. Copies that share any extent form one *cluster*
   (clones). A group's id is the first 16 bytes of its signature in hex, so it
   stays the same while the content does.
5. **What a cluster frees.** Every extent of a cluster is judged against the
   extent tree with `model::accounting::classify`, the normal accounting. The
   cluster's copies are the selection; other subvolumes are classified as the
   scan classifies them.
   - Reclaimable bytes count as `free`.
   - Bytes held only by snapshots (or snapshot-shared leaves) count as
     `later`: they free when those snapshots rotate.
   - Anything else holds the bytes: another file, a clone in another group, a
     hard link outside the report. Such bytes free nothing, so an extent
     shared across groups is never counted.
6. **Identical folders** and **partly identical folders** (next sections).

Lists, counts and one-click selection use groups and folders of at least
10 MiB (`model::LIST_MIN_SIZE`). They leave out groups whose copies are all
clones of one physical copy (`shared`): deleting any of them frees nothing.

## Identical folders

`duplicates::folders` finds folders of small files as well as large ones. It
reads no file data.

1. **Fingerprint.** The report already holds every entry's name, kind and
   logical size, so the fingerprint is computed from it during the analysis.
   The scan records nothing extra and its cost does not change. Per folder,
   in one streamed walk of the report:
   - SHA-256 over its entries in name order: for a file or symlink, its
     kind, name and size; for a subfolder, its name and that subfolder's
     fingerprint;
   - its file count and total bytes;
   - a *complete* bit.

   Regenerated system files are left out of all three: `.DS_Store`,
   `Thumbs.db`, `desktop.ini`, `._*` (AppleDouble) and `@eaDir`, listed once
   in `folders::JUNK_NAMES`. They never make two folders differ, and they are
   deleted with the folder. User dotfiles such as `.git` or `.env` count.

   A folder is complete only when nothing below it was left out. Any of
   these makes it and every folder above it incomplete:
   - another `@` folder or file, which is app or DSM data;
   - a node flagged as DSM, package or own-state data;
   - an encrypted folder, a subvolume boundary, a snapshot or a placeholder;
   - a name at the scan's depth limit;
   - a tree the report marks namespace-uncertain (read gaps).

   An incomplete folder is never identical. DSM's Btrfs scan reads metadata
   with TREE_SEARCH, so there is no permission-denied case.
2. **Candidates.** Complete folders that are not excluded themselves and
   hold at least 10 MiB, with the same fingerprint in two or more places. A
   set whose every member's parent is in one other set is dropped, so only
   the topmost folders are confirmed.
3. **Confirmation**, for candidate members only; it reads metadata, never
   file data.
   - A second report walk lists every file and folder below the candidate
     members, at most 2,000,000 files; beyond that the candidates are
     `unchecked`.
   - Inodes are read with one search per run of objectids less than 256
     apart in a tree (`INODE_ITEM` through `EXTENT_DATA`). Files of one
     folder are usually numbered close together, so a search covers many
     files.
   - Data stored inline in an `EXTENT_DATA` item is hashed from that item:
     small files and symlink targets. This is metadata, not a file data
     read.
   - Other data is identified by its sector checksums. These are read with
     one checksum-tree search per span of ranges less than 1 MiB apart
     (`csum::read_many`).
   - A file's signature covers its size and, per extent in file order: the
     inline hash, the checksums, or a zero marker for a hole or preallocated
     range. A member's signature covers its files' relative paths, kinds and
     signatures.
   - A member with a NOCOW (`nodatasum`) file, a sector without a checksum,
     or a compressed extent is `unchecked` (`unchecked.folders`,
     `unchecked.folder_bytes`).
4. **Groups and accounting.** Members of one candidate set with one
   signature form a folder group.
   - Members that share any extent (a reflinked copy) form one cluster.
   - Each cluster's extents are judged as for files: hard links inside the
     member stay internal, and links outside it hold the bytes.
   - A member's date (`mtime`) is its newest file's; the keep rule's age is
     its oldest file's.

   A file group whose every copy lies inside the members of one listed
   folder group is listed with the folder, not on its own. A file group with
   copies both inside and outside folder members is listed for its outside
   copies only. A copy inside a listed folder-group member has the role
   `in_folder`: the folder group removes and counts it, and its file group
   never does. It may still be that file group's keeper. Totals therefore
   count every byte once (`model::tests::files_inside_identical_folders_count_once`).

   The same holds between folder groups. What a listed folder group
   removes is its *parts*: each whole member, or each identical item of
   partly identical folders (`FolderGroup::parts`, `model::Covered`). A
   member of another folder group that is, or lies inside, such a part has
   the role `in_folder`. A folder group whose every member is `in_folder`,
   or is an identical item of similar folders (next section), is not
   listed (`in_folders`).

Cost: the analysis walks the report twice. Each walk is O(entries) in time,
but memory follows only the open folder stack and the fingerprints of folders
of at least 10 MiB. Confirmation costs one inode search per run of nearby
objectids and one checksum search per 1 MiB span. On a loop-mounted test
volume, three trees of 3,001 files each took 19 inode searches and 14
checksum searches.

Cleaning a folder group moves the whole member folder, junk included, into
its shared folder's Recycle Bin (SPEC 5.7); cleaning partly identical folders
moves only the identical items (below); a copy with no Recycle Bin is
deleted for good instead, once confirmed (below). The plan
and `cleanup apply` (before anything moves) check that each kept copy still
exists: a kept file with the size the analysis read, a kept folder copy as a
folder and each of its files (at most 500,000 per plan) with its size. A copy
gone or of another size refuses with `keeper_changed`. The moved copies are
not checked again: files added to or changed in a removed folder since the
analysis are removed with it: to the Recycle Bin, where they can be
restored, or for good where there is none. Review says so, with the scan's
time, and asks for a new scan first if those folders may have changed.

A plan keeps the copies of each selected group that it leaves alone: the
keeper, copies the user kept, copies no cleanup can move, copies in kept
folders.
It refuses (`keeper_removed`) when another selected group removes every
one of them, and (`keeper_conflict`) when a selected group removes a copy
the user kept in any other group: a copy the user chose to keep is never
removed with another group. Selecting a folder group and a file group with copies inside it
therefore works whenever one copy stays.

## Partly identical folders

A git clone and the same tree exported without `.git` differ only at the
top: every child is the same, so identical folders alone would list each
child as its own row, and two close versions of a tree thousands of rows.
Such a pair is one row instead (`duplicates::partial`, owner decision
2026-11-13). It reads no file data either.

1. **Pairs.** Two identical folders (same fingerprint, at least 1 MiB) or
   two copies of a file group with one name, in different folders, seed a
   pair. Walk up from them while the folder names stay the same: the
   topmost aligned folders P and Q are a pair. Only P and Q themselves may
   have different names (`linux-6.6` and `linux-6.6-copy`). The walk stops
   below the volume and where the two would be the same folder or one
   inside the other.
2. **Items.** Every file below P and Q is confirmed as above. A file is
   identical when Q has a file of the same kind at the same relative path
   with the same signature. The *identical items* are the topmost folders
   whose files, subfolders and names are all identical on both sides, and
   the identical files not inside one. Junk is left out as for identical
   folders.
3. **Threshold** (`partial::PARTIAL_PERCENT`, the one place it is set:
   80). A pair is one row when all of these hold:
   - both sides are complete (fully scanned, not excluded);
   - the identical items' bytes are at least 80% of the smaller side's
     bytes (`.git` of a clone does not count against the export);
   - they are at least 10 MiB;
   - there are at least 2 of them (`MIN_ITEMS`; one alone is an identical
     folder or file row) and at most 1,000 (`MAX_ITEMS`, a bound on the
     row's list, not on what a cleanup removes).

   Otherwise the pair's aligned subfolders that differ are tried, the
   topmost first. A row is never emitted for a pair inside a row, nor for
   an ancestor below the threshold.
4. **Similar folders.** A complete pair below the threshold whose identical
   items still cover at least 50% (`SIMILAR_PERCENT`), or one with more
   than 1,000 items, is *similar*: P2 handles it. Its identical items are
   not listed in P1 either: an identical folder whose every copy is such an
   item, or inside a listed row, is not listed. Pairs below 50% are
   unrelated folders; identical folders inside them are listed as before.
   File groups are always listed as before.
5. **Selection.** A folder is a side of at most one row; pairs with more
   identical bytes come first, then pairs with a side in an identical
   folder, then by path. Run the analysis again after a cleanup to see
   what else pairs up.
   - A side that is, or lies in, a listed identical folder brings that
     path in every copy of the identical folder. Those copies are
     `in_folder`: the identical folder removes them, so the row removes
     only from the other side and keeps the identical folder's copies.
   - A pair is skipped when an identical folder lies inside a side but not
     inside one of its identical items: two rows would remove the same
     files.
6. **Accounting.** The row's members are its sides; each side's identical
   items' files form its copy, clustered and judged as for identical
   folders. Group `files` and `bytes` are the identical items' (one side).
   Each member keeps its own `total_items`, `same_items` (entries directly
   in it), `total_files`, `total_bytes` and each item's inode.

Deleting the row removes only the identical items of each removed side,
each by its own path and expected inode (`FolderGroup::parts`). The side
folder itself, the items that differ and `.git` stay. A kept side is
checked like a kept identical folder (`keeper_changed`).

Expected on the dataset with Linux kernel trees (a v6.6 clone; an export of
its worktree without `.git`; a second export with junk; a third with a
`.env`; a v6.6.15 clone):
- the two exports are an identical folder (1.26 GiB);
- the v6.6 clone and the exports are one row: 37 of the clone's 39 items
  the same, `.git` and `drivers` left; `drivers` matches only in part, so
  its 462 identical items are in the row too (499 in all, 1.22 GiB of
  files). The row keeps the export the identical folder keeps and removes
  the clone's identical items (small files stored inline free no extent,
  so `can_free` is the extents' share). The exports are the identical
  folder's to remove;
- the export with `.env` and the exports are one row: 38 of 39 items,
  `.env` left;
- v6.6 against v6.6.15: 76% of the bytes are identical (a v6.6 clone
  measured against a v6.6.15 clone), below 80% and above 50%, so the top
  pair is similar and its identical children are not listed. The v6.6.15
  clone against the exports (no `.git`, 91%) has 39,297 identical items,
  more than one plan removes, so its subfolders are tried: 14 rows such
  as `drivers/gpu/drm/amd/include` (389 MiB) and `drivers/scsi`, 583 MiB
  in all.

### For P2 (similar folders)

P2 (folders whose names and sizes overlap by at least 90%) can reuse what
this walk builds:
- the per-folder entry list in name order;
- file counts and bytes;
- the complete bit;
- the per-file signatures from confirmation.

It would need:
- the entry lists kept per candidate (P1 keeps only the hash);
- a similarity index over (name, size) pairs, for example MinHash per
  folder;
- a diff per pair.

Partly identical folders already provide a cleanup plan mode that removes
only matching items, and mark similar pairs (50–80% identical bytes, or too
many items for one plan) that P1 leaves out.

Similar folders must never join one-click selection.

## Administrators only

Syno Reclaim, and so every duplicates answer, is for DSM administrators
only: the package's entry and WebAPI let only members of `administrators`
in. The answers therefore check no per-user folder
or file permission and hide nothing: every group shows every copy, with its
path and share, and the kept copy is always named.

## Keep rule

A group is always cleaned on its whole volume. A folder (a share, or one
below it) only picks which groups are listed: those with a copy in it. A
cleanup started from any folder keeps the same copy and moves the same
others, whichever shared folder they are in.

One copy is the *keeper*, chosen by the first rule that applies:

1. a copy the user kept (`reason: user`);
2. a copy in the first preferred share that has one (`preferred_share`);
3. a copy no cleanup can move (`unmovable`): a whole file or folder right
   below the volume, which is a shared folder itself or lies outside every
   shared folder. It stays anyway, so it is the one kept;
4. a copy inside a folder another listed folder group keeps
   (`in_kept_folder`): that group removes and counts the copies inside its
   folders, and this one stays with its folder, so the group removes the
   copies no folder group takes care of;
5. the oldest copy by modification time (`oldest`); when the oldest are tied,
   the shortest path wins (`shortest`). A copy inside a folder another group
   removes comes after every other one.

One place decides every group's keeper on the volume
(`model::Decisions`): the lists, the totals, a group's copies and the
cleanup plan all read it.

Copies in a `#recycle` folder are never members. If one ever were, it would
come after every other copy in each rule, so the copy that stays is never one
DSM empties on its own.

Each copy then gets one role:

- the keeper is `keep`;
- every other copy the user kept is `keep_user`;
- a copy inside a copy of a listed identical folder is `in_folder` (that
  folder group removes it);
- every other copy no cleanup can move is `unmovable`;
- every other copy, in any shared folder, is `remove`.

A copy is in a folder when all of its names lie below it.

`can_free` is the sum of `free` over the clusters whose every copy is
`remove`; `frees_later` is the same sum over `later`. A clone therefore frees
nothing while its twin stays. These numbers estimate what the cleanup plan
will show; the plan's own simulation is the figure that counts.

User keeps are stored per volume in `<state>/duplicates/<fsid>.json`, as a
list of kept paths for each group id. A keep is lost when the content changes,
because the group id changes with it. Changing a keep recomputes the tallies
at once. Changing the preferred shares marks the answers `recalculating`. They
keep the old numbers until `duplicates analyze` runs again.

In the UI a row opens its sheet: every identical copy on the volume, in any
shared folder, sortable by modification time, with a box per copy ticked for
the copies that move to the Recycle Bin. The keeper starts unticked and is
marked with why it is kept; the others start ticked. The user can untick
more to keep several; at least one copy stays, or Apply is disabled. A copy
no cleanup can move and one inside an identical folder have no box and say
why. A copy with no Recycle Bin to move to keeps its box and says that
removing it deletes it for good. Apply stores the unticked
copies as user keeps (`duplicates.keep`): when only the rule's keeper is
unticked nothing is stored; otherwise the first user keep replaces the
rule's keeper.

Rows of identical folders that keep the same folder (one folder matched by
several others, partly or wholly) are shown as one row. Its sheet lists the
kept folder and each member folder with how much of it matches and its
modification time; each member is a group of its own, and the row selects
the members ticked. This uses the groups as analysed; nothing is scanned
again.

A cleanup moves copies into their share's Recycle Bin. A removed copy with
no Recycle Bin to move to (in a share whose Recycle Bin is off, or in a
folder right below the volume that DSM does not list as a shared folder) is
removed for good instead: the plan marks its folder `delete`, and apply
removes it only once the user confirms that in Review (`confirm_delete`). Such a group stays selectable. The page reads
each share's Recycle Bin from DSM (`SYNO.Core.Share` `list` with
`additional: ["recyclebin"]`) each time it opens, not from
`duplicates.scope`, whose answers are cached. A row's `remove_shares` names
the shares it moves copies from. The shares table marks a share whose
Recycle Bin is off "Off · deleted for good", and a row's Removal column
says "To Recycle Bin" or, when it removes copies from such a share,
"Deletes for good". Both carry a settings icon (its tooltip names the
share; a grid row keeps one line per text, so the cell never outgrows the
row) that opens DSM Control Panel › Shared Folder with that share selected (the SDK's
`openControlPanel`; DSM has no launch parameter that opens its Edit
dialog), so the user can turn the Recycle Bin on first; Review's plan reads
it again. Review's line for each group says what happens to its copies, from
the plan's `delete` shares and the group's `remove_shares`: "Moves N to
Recycle Bin", "Deletes N for good", or, when only some of those shares have
no Recycle Bin or the group does not name them, "Removes N: to the Recycle
Bin, or for good where there is none". The group's sheet words its lead and
summary the same way, by the shares of the ticked copies, and marks only a
ticked copy "deletes it for good": the kept one is not removed. While the
cleanup runs and when it ends, Review titles it by what it does: moving,
deleting for good, or both; a delete-only result has no restore hint. When
the copies that move hold symlinks, Review's Recycle Bin note says how many:
DSM's Recycle Bin keeps no symlinks (nor folders holding only them), and a
restore does not bring them back, as with File Station's delete; Reclaim
keeps that behaviour. The folder itself, and `@…`, `#…`, hidden and package folders, are
never removed. Outside a DSM window nothing is marked; `cleanup.plan` still
decides.

Select all reads every page of the listed groups and checks each with the
same rule as a row's box, in a dialog that shows how many of how many were
checked and can be stopped. It selects the groups that can be selected and
then lists each group it skipped and each copy that stays, with why. It
only informs: nothing it selected waits on it.

## Tallies

For each directory holding a copy, the analysis keeps what deleting its
duplicates would do, for these kinds: listed file groups by category (photos,
videos, music, other) and folder groups. A kind's tally is the number of
copies removed, `can_free` and `frees_later`. Since a group is cleaned on its
whole volume, its outcome (the keep rule's `removes`, `can_free` and
`frees_later`) counts once at every directory holding one of its copies,
from the copy's folder up to the volume. Directories therefore overlap, as
the shares do, and the volume's tally is the sum of its groups. Stored
tallies carry the rule they follow (`tally_rule`); results with another rule
compute them again from the groups, with nothing scanned again.

At most 65,536 directories are kept, shallowest first. Any other directory is
computed from the groups when asked.

Category comes from the file name extension (`model::Category::of`).

## Commands

| Command | Does |
| --- | --- |
| `duplicates analyze VOLUME [--json]` | the analysis above |
| `duplicates scope VOLUME [--path=DIR] [--list=files\|folders\|both] [--category=photos\|videos\|music\|other] [--offset=N] [--limit=1..200]` | tallies of the scope and its child folders, then one page of the groups with a copy in the scope and at least one copy to remove on the volume, by `can_free` |
| `duplicates copies VOLUME --group=ID [--path=DIR] [--offset=N] [--limit=1..200]` | every copy of a group with its role, ordered by share: the scope's share, then the keeper's share, then the rest by name |
| `duplicates keep VOLUME --group=ID --copy=PATH --keep=on\|off [--path=DIR]` | keeps a copy, or stops keeping it |
| `duplicates preferred-shares VOLUME [--shares=JSON-LIST]` | shows or replaces the ordered preferred shares (at most 64; no `/`, no `@` names) |

Each command prints one JSON document with `--json`; its fields are the WebAPI contract.

## WebAPI contract

All methods need an administrator. Paths in params are text or
`{path, path_hex}`. Paths in answers are `{path, path_hex?}`. Times are
RFC 3339.

| Method | Params | Answer |
| --- | --- | --- |
| `duplicates.analyze` | `{volume}` | a planning job's detach line (`job_id`); a second request for the volume joins the running job |
| `duplicates.scope` | `{volume, path?, list?: "files"\|"folders"\|"both", category?: "photos"\|"videos"\|"music"\|"other", offset?, limit?: 1..200}` | see below |
| `duplicates.copies` | `{volume, group_id, scope?, offset?, limit?}` (`scope` sets `in_scope` and the share order) | `group` (a row), `copies {total, offset, items[] {path, share, mtime, role, in_scope}}`; for `folder_partial`, `items {total, offset, items[] {path, kind: "folder"\|"file", files, bytes}}` |
| `duplicates.keep` | `{volume, group_id, path, keep: bool, scope?}` | `group` (the row in the scope), `totals` (the scope's tally) |
| `duplicates.preferred_shares` | `{volume, shares?: [name]}` (≤ 64) | `shares`, `recalculating` |
| `cleanup.plan` | `{paths?, duplicates?: {volume, groups: [id]}}` | the plan document plus `duplicates {copies, groups, kept, scan_finished}`; a removed copy in no DSM shared folder is in a plan share marked `delete`; `scan_finished` is when the scan the analysis used finished (Unix seconds), which the delete confirmation names |

`duplicates.scope` answers with:

- `analysis {state: "none"|"stale"|"ready", running, job_id, analyzed_at,
  report_job_id, complete}`;
- `recalculating`: the preferred shares changed, or an analysis runs; the
  numbers shown are the previous ones;
- `preferred_shares`;
- `thresholds {record_bytes, list_bytes}`;
- `totals`: the scope's tally;
- `children[] {path, name, tally}`: child folders holding copies, by
  `all.can_free`. At the volume these are the shares; their sums overlap.
- `unchecked {files, bytes, no_checksums, compressed, folders, folder_bytes}`;
- `list`, `category`;
- `touches_drive`;
- `groups {total, offset, items[]}`: the rows with a copy in the scope and
  at least one copy to remove, largest `can_free` first. A row with nothing to
  remove stays listed when the user kept one of its copies, so the keep can
  be undone;
- `folders` (`list: "both"` only): the folders page, with the same
  `offset` and `limit`; `groups` is then the files page. `category`
  applies to `groups` only. A share opens with one request.

A tally is `{all, files, photos, videos, music, folders}`, each
`{copies, can_free, frees_later}`. `all` is `files` plus `folders`.

A row (`GroupView`) has these fields:

- `group {id, kind: "file"|"folder"|"folder_partial", name, size, files?
  (folders), copies_total, category? (files), same_items? (partial)}`. For
  `folder_partial`, `size` and `files` are the identical items' and
  `same_items` is how many identical items the row removes from a side;
- `keeper {path, share, mtime, reason:
  "user"|"preferred_share"|"unmovable"|"in_kept_folder"|"oldest"|"shortest",
  in_scope}`;
- `in_scope[]`: the scope's copies other than the keeper, at most five, each
  `{path, share, mtime, role: "remove"|"keep_user"|"unmovable"}`, and `in_scope_total`;
- `elsewhere_by_share[] {share, count}`;
- `removes`: how many copies (or sides) the row removes on the volume;
- `remove_shares[]`: the shares of those copies, by name;
- `unmovable[]`: copies right below the volume, which stay;
- `in_identical_folders`: copies that a listed folder group removes and
  counts instead of this row;
- `can_free`, `frees_later`;
- `touches_drive`: a removed copy lies in `homes/<user>/Drive`;
- `shared`: every copy is a clone of one extent set, so deleting frees
  nothing;
- `partial` (`folder_partial` only): `{path, same_items, total_items,
  same_files, total_files, same_bytes, total_bytes, counterpart}`. `path`
  is the first side the row removes from (in the scope first), `same_items`
  of its `total_items` entries are identical items, and `counterpart` is
  the kept side. For example
  "linux-6.6: 37 of 39 items same as /volume1/media/src/linux-6.6-copy ·
  can free 1.36 GiB".

Errors are envelope errors with code `duplicates_unavailable` (no Btrfs
volume, no report or candidates). `cleanup.plan` refuses a duplicates
selection with one of these codes:

| Code | When |
| --- | --- |
| `duplicates_stale` | no analysis, or a newer scan exists |
| `duplicates_unknown_group` | a selected group id is not in the analysis |
| `duplicate_changed` | a copy's inode, size or modification time changed since the analysis |
| `keeper_changed` | a kept copy, or a file of a kept folder, is gone or changed (size, modification or change time) since the analysis |
| `keeper_removed` | the selected groups together would leave no copy of a group |
| `keeper_conflict` | a selected group removes a copy the user kept in another group (or something inside it); the error's `path` is the copy and `conflict {kept_in, removed_by}` names both groups (`{id, name}` each). Every such pair of groups is one `errors[]` item, so Review can take every group that removes a kept copy out of the list at once, check again and go on |
| `duplicates_nothing` | the selection removes nothing |

`cleanup.apply` refuses with `keeper_changed` before any removal when a kept
copy, or a file of a kept folder, changed or went after the plan.

`duplicates.copies` items may have the role `in_folder`. For a
`folder_partial` group its answer also has `items`: the identical items,
largest first, paths relative to each side, paged by the same `offset` and
`limit` as the copies (a row has few sides, so the UI asks for the copies
with offset 0 and pages the items). The analysis format is now 4 (format 3
is split on its first read): results from formats 1 and 2 read as `none`
until the analysis runs again.

The `cleanup.plan` answer no longer carries the plan's internal `keep` and
`keep_files` lists; `cleanup.apply` reads them from the stored plan.

### Changes for the UI (partly identical folders)

- A new row kind `folder_partial` in the folders list (`list=folders`),
  beside `folder`. `DuplicateKind` gains it.
- Show the row from `partial`: the side's name, "`same_items` of
  `total_items` items same as `counterpart`", and `can_free`.
- The row's sheet pages `duplicates.copies` `items` (path, kind, files,
  bytes) with `offset` and `limit`; the copies come with offset 0.
- `group.size` and `group.files` are the identical items', not the whole
  folder's; `partial.total_bytes` is the whole side.
- The row's sides that lie in an identical folder have the role
  `in_folder` and count in `in_identical_folders`; the row removes only
  from the other side.
- Deleting removes only the identical items; the confirmation should say
  that the folder, the items that differ and `.git` stay.
- A member of an identical folder inside another listed folder group's
  part is now `in_folder` too, so nested identical folders no longer count
  twice.
- Keep (`duplicates.keep`) takes a side's folder path, as for `folder`.
- Select all and the cleanup plan take the group id as for `folder`; a row
  lists at most 1,000 identical items (`MAX_ITEMS`).
