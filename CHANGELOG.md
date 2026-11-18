# Changelog

## 0.1.1-1678 — Every keeper conflict at once

- Duplicates cleanup plan: when several selected groups would remove a copy you chose to keep, the plan now refuses with one `keeper_conflict` error for every pair of groups in conflict instead of only the first, so Review's "Unable to start" dialog names them all and one click takes them all out of the list before checking again. A single conflict reads as before
- Rust build of the App and root programs from this source (the root programs change, so administrator reapproval is needed after upgrading from 1677); no UI kit or SDK version change: UI ~0.25.30, SDK/App 0.53.52, executor 0.53.6, CLI ~0.54.6 and templates 0.42.1 unchanged
- Validated with the cleanup plan Rust tests, rustfmt, clippy, the Review UI tests, the SPK build of each architecture, inspection with the catalog, an archive privacy scan and the release checksums after download

## 0.1.1-1677 — Duplicates review keeps the list

- Review: when a duplicates cleanup is refused because a selected group would remove a copy you chose to keep, "Remove <group> from the list" takes out only that group (several at once when there are more), checks again and keeps the cleanup list open, saying which groups it removed; two groups that remove each other's kept copy are offered as a choice in the same dialog. Previously the button took every duplicate group out of the list
- UI-only update: the App, CGI, executor and root programs are the 0.1.1-1676 binaries, reused with `package/repackage-ui.mjs` (no Rust build); permissions and task templates are unchanged, so no new approval is needed after upgrading from 1676
- Validated with the UI build, inspection of each SPK with the catalog, and the release checksums after download

## 0.1.1-1676 — UI kit and SDK update

- Updated to the latest UI kit (~0.25.30) and SDK/App (0.53.52); executor 0.53.6, CLI ~0.54.6 and templates 0.42.1 unchanged

## 0.1.1-1675 — Compact phone toolbar

- Pin UI ~0.25.29: on a phone, a page's toolbar takes two thin rows at the top edge (Home beside the view switch, counts beside the actions, secondary actions with an icon drawn as icons); wider layouts and a DSM window are unchanged. No Reclaim code changes. SDK/App 0.53.49, CLI ~0.54.6, executor 0.53.6 and templates 0.42.1 unchanged
- UI-only update: the App, CGI, executor and root programs are the 0.1.1-1674 binaries, reused with `package/repackage-ui.mjs` (no Rust build); permissions and task templates are unchanged, so no new approval is needed after upgrading from 1674
- Validate with the UI build, inspection with the catalog and against 1674 (approval retention eligible, permissions and policy unchanged); catalog screenshots are reused from 1672

## 0.1.1-1674 — Phone look matches the desktop

- Pin UI ~0.25.28: on a phone, the page background, cards and nested panels use the same layers as on the desktop (a CSS-only UI fix). No Reclaim code changes. SDK/App 0.53.49, CLI ~0.54.6, executor 0.53.6 and templates 0.42.1 unchanged
- UI-only update: the App, CGI, executor and root programs are the 0.1.1-1673 binaries, reused with `package/repackage-ui.mjs` (no Rust build); permissions and task templates are unchanged, so no new approval is needed after upgrading from 1673
- Validate with the UI build, inspection with the catalog and against 1673 (approval retention eligible, permissions and policy unchanged); catalog screenshots are reused from 1673

## 0.1.1-1673 — Fits a phone screen

- Pin UI ~0.25.27: on a phone, the Overview's "Where the space went" tabs keep each label on one line and scroll, with the next tab showing cut at the edge; the chart legend (now the UI `ChartLegend`) never cuts an amount or share and moves them under a long name; lists line up row by row; the Duplicates summary keeps its width and puts "Select all duplicates" on the next line; the page's status button sits in the top bar (in its status colour) instead of a row of its own. SDK/App 0.53.49, CLI ~0.54.6, executor 0.53.6 and templates 0.42.1 unchanged
- UI-only update: the App, CGI, executor and root programs are the 0.1.1-1672 binaries, reused with `package/repackage-ui.mjs` (no Rust build); permissions and task templates are unchanged, so no new approval is needed after upgrading from 1672
- Validate with the UI build, inspection with the catalog and against 1672 (approval retention eligible, permissions and policy unchanged); catalog screenshots are reused from 1672

## 0.1.1-1672 — UI component update

- Pin UI ~0.25.25: it only keeps "+N" visible in one-line tag lists, which Reclaim does not draw, so the pages look and work the same (the cut-text tooltip of UI 0.25.24 shipped in 1671). SDK/App 0.53.49, CLI ~0.54.6, executor 0.53.6 and templates 0.42.1 unchanged
- UI-only update: the App, CGI, executor and root programs are the 0.1.1-1671 binaries, reused with `package/repackage-ui.mjs` (no Rust build); permissions and task templates are unchanged, so no new approval is needed after upgrading from 1671
- Validate with the UI build, inspection with the catalog and against 1671 (approval retention eligible, permissions and policy unchanged); catalog screenshots are reused from 1671

## 0.1.1-1671 — Opens in the DSM display language

- `app.status` answers in the caller's DSM display language: the App uses the Rust SDK's `app_language` (the entry page's `dsmLanguage` hint, else the browser's language), so the phone entry opens in the language set in DSM's Personal settings. The operation declares the optional `dsmLanguage` parameter and `lang` is a DSM code or empty
- Rust SDK tag v0.53.49 and npm SDK ~0.53.49, CLI ~0.54.6 (integrity pin refreshed), UI ~0.25.24; executor 0.53.6 and templates 0.42.1 unchanged
- A full Rust build of the App, CGI, executor and root programs (not a UI-only repackage)
- The gate's e2e launcher opens the archived source without the gate's own `target/` and `.tmp/` scratch and skips symlinks, so the default parallel gate no longer fails when another job's compiler or test files come and go (or a test leaves a dangling `/proc` link)
- Validate with the fast gate on Docker at its default parallelism (UI, Rust checks, e2e, both musl builds and the package part for all three architectures, three identical SPK builds), and inspection with the catalog and against 1670 (approval retention eligible, permissions and policy unchanged); catalog screenshots are reused from 1670

## 0.1.1-1670 — Snapshot page separator fix

- The Snapshot page no longer shows "Shared folder ·" for a shared folder: `joinParts` drops empty fragments, so the middle dot only stands between two fragments that both have content (also scan progress without a phase, job strips without an elapsed time and similar labels)
- Pin UI ~0.25.22: the UI SDK now ships as separate ES modules; its exports and the pages look the same
- UI-only update: the App, CGI, executor and root programs are the 0.1.1-1669 binaries, reused with `package/repackage-ui.mjs` (no Rust build); permissions and task templates are unchanged, so no new approval is needed after upgrading from 1669
- Validate with the related UI tests (red, then green), lint, typecheck, the UI test suite and builds, inspection with the catalog and against 1669, and a test NAS install with English catalog screenshots

## 0.1.1-1669 — Duplicates page and cleanup into the Recycle Bin

- New Duplicates page (Btrfs volumes): identical folders and big duplicate files (1 MiB or more), found from the scan's metadata without reading file contents. The scan records the candidates, so a volume scanned by an earlier version needs a new scan before Duplicates can list it
- Each group keeps one copy and says why: the user's choice, a preferred shared folder (set in Settings), staying with its kept folder, the oldest copy or the shortest path; the details list every copy, mark the kept one, show how much the group frees, and let the user keep another copy
- Partly identical folders show as one row that removes only their identical items; a row that merges several groups says "Includes N groups"
- Select all on a volume or a shared folder shows its progress, can be stopped, and lists what it could not select and why
- Duplicates are added to the cleanup list and go through Review like Explore's items; kept copies are checked again right before the cleanup, and a group whose copy is gone leaves Review
- A copy that one group keeps is never removed by another group: the second group keeps the same copy, and a plan that would still remove a copy kept elsewhere is refused (`keeper_conflict`), naming the copy and both groups
- A running cleanup says what it is doing (checking kept copies, moving or deleting, finishing after a cancel), follows the job reliably and counts dropped symlinks
- @ folders inside a shared folder are refused with their own reason
- Choose the volume in Explore's page bar
- Cleaning up a shared folder (Explore's selected items, Duplicates' copies) moves each item into that shared folder's Recycle Bin with DSM's own tool, the one File Station deletes with. Restore it in File Station (#recycle > Restore; DSM before 7.2.1: move it out of #recycle), or empty the Recycle Bin to free the space
- A shared folder whose Recycle Bin is off can be cleaned too: its items are deleted for good, only after Review's confirmation names how many cannot be restored. Review says the shared folder has no Recycle Bin turned on and opens it in Control Panel; turning the Recycle Bin on and selecting Recheck moves the items to the Recycle Bin instead. Duplicates lets such groups be selected and marks them "Deletes for good"
- Review's duplicates lines say whether each group's copies move to the Recycle Bin or are deleted for good, Recheck sits beside the confirm button, and the approval screen says that items with no Recycle Bin are deleted for good
- A Recycle Bin turned off after Review's check deletes the items of that shared folder for good when the cleanup reaches them; the result counts them as deleted
- Review groups the items by shared folder, notes when only administrators can restore them or when every user of the shared folder can see them, and marks Synology Drive folders
- The Recycle Bin page shows what Syno Reclaim moved since the scan and where DSM empties it; Syno Reclaim never removes anything from a Recycle Bin, so items in #recycle can no longer be selected
- Large folders move folder by folder with progress, and a cancel stops between DSM's runs; item sizes come from the scan report
- A cleanup has no item limit: the 1,000-item limit of the cleanup list and the plan is gone. Review sends a large list in chunks the NAS keeps on disk, checks it in a background job that shows how many items it checked, and shows the totals with the first 100 items of each shared folder; the move reports progress and can be cancelled as before. Closing the browser leaves a running check or move alone
- An item in a folder directly under the volume that DSM does not list as a shared folder has no Recycle Bin either: it is deleted for good the same way. System folders (@ and # folders, hidden folders, lost+found, package and DSM folders) stay refused, and so does such a folder itself
- System partition cleanup is unchanged
- Pin UI ~0.25.19: a button label wraps instead of ending in an ellipsis
- After upgrading, an administrator approves the system tasks once more: the `cli` and `lifecycle` root programs changed, the root method list gained `duplicates.analyze`, `duplicates.copies`, `duplicates.keep`, `duplicates.preferred_shares` and `duplicates.scope`, and root jobs get a 7-day timeout (`timeout_s` 604800), so `synopkgland inspect` against 1641 reports `permissions: review-required`; task templates are unchanged
- Pin SDK/App ~0.53.43 (executor/v0.53.6); CLI 0.53.15 and templates 0.42.1 are unchanged; rebuilt every architecture from source
- Retain existing settings, saved scan reports and all three package architectures
- Validate with one reproducible build of each architecture (`--verify-reproducible always`, identical) with the package unit tests, inspection with the catalog and against 1641, the internal-names check and archive privacy scans; the changes since 1641 were each tested before merging, and a 0.1.1-1669 build of the same source was checked on a test NAS (upgrade approval, Duplicates, Review up to the confirmation, nothing applied); no full gate or qemu run

## 0.1.1-1641 — System partition cleanup to ext4 shared folders

- System partition cleanup lists the shared folders of every data volume, ext4 included, as destinations: a NAS with only ext4 data volumes no longer shows "No shared folder found"
- System partition cleanup removes its empty `.partial` staging folder at the end of each batch, so no empty hidden folder is left beside the moved files, the manifest and the README
- `aquota.user`, `aquota.group` and `synoquota.db` at the root of an ext4 volume are DSM quota files: Explore and Packages show them as DSM data to keep, and cleanup refuses them like any service path
- After upgrading, an administrator approves the system tasks once more: the `cli` and `lifecycle` root programs changed, so `synopkgland inspect` against 1640 requires reapproval (`permissions: unchanged`, no policy changes)
- Rebuilt every architecture from source (runtime change); SDK/App 0.53.37 (executor/v0.53.6), UI 0.25.12, CLI 0.53.15 and templates 0.42.1 are unchanged
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the x86_64 Rust tests (1,403 unit tests and the integration tests, all pass), fmt and clippy, the UI gate (lint, typecheck, 1,910 unit tests), one build of each architecture with inspection with the catalog and against 1640, the internal-names check and archive privacy scans; no full gate, qemu run, reproducibility check or NAS test (the 1641 changes were checked on a test NAS before merging)

## 0.1.1-1640 — Finish or continue an interrupted delete

- If a delete is interrupted (a restart, a power cut, or the process being killed), Overview, Explore and History show where it stopped; "Delete the rest" confirms the same plan again and deletes what is left, and when the interrupted run left its hidden folder behind, "Finish the interrupted cleanup" (`cleanup.recover`, SPEC §5.7) finishes or undoes each removal first
- After upgrading, an administrator approves the system tasks once more: the root program changed and its method list gained `cleanup.recover`, so `synopkgland inspect` against 1639 reports `permissions: review-required`
- Fix "Delete the rest" after an interrupted run whose hidden folder still held an item: `cleanup apply` reported a partial delete with no reason, so the page showed "Some items weren't deleted" instead of "Finish the interrupted cleanup"; it now refuses as `quarantine_kept` before anything runs and names the selected item
- Fix "Delete the rest" after "Finish the interrupted cleanup": the confirmation stayed disabled, so the rest of the plan could not be deleted from that dialog
- The full gate runs the armv7 qemu tests without `QEMU_LD_PREFIX`, which redirected every walk from `/` and failed the recover tests only under emulation
- Rebuilt every architecture from source (runtime change); SDK/App 0.53.37 (executor/v0.53.6), UI 0.25.12, CLI 0.53.15 and templates 0.42.1 are unchanged
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the x86_64 cleanup and commands tests (160), fmt and clippy, the armv7 qemu recover tests (13), the UI gate (lint, generated types, typecheck, 1,909 unit tests, UI and DSM builds), one build of each architecture with inspection with the catalog and against 1639, and archive privacy scans; on a test NAS: the upgrade approval screen, then deletes in a test folder of about 60,000 small files killed with SIGKILL, one of them while an entry sat in its cleanup folder, "Delete the rest" refused as `quarantine_kept`, "Finish the interrupted cleanup", and "Delete the rest" to completion, with no hidden folder left and nothing outside the test folder changed; no full gate, aarch64 qemu run or reproducibility check

## 0.1.1-1639 — Shorter text when deleting

- Deleting now shows shorter, clearer text, and every confirmation is still there: the permanent-deletion warning, the "I understand this can't be undone" checkbox and the red delete button stay; supporting details move into info buttons, and the confirm screen lists how much each selected item can free
- UI-only change: repackaged from the 0.1.1-1638 SPKs with their CLI, App, CGI, executor and root programs reused unchanged (no Rust build); SDK/App 0.53.37 (executor/v0.53.6), UI 0.25.12, CLI 0.53.15 and templates 0.42.1 are unchanged
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the Review, Explore and History unit tests, typecheck, lint, the UI and DSM builds, the catalog check, inspection of every architecture with the catalog and against 1638, and an archive privacy scan; no NAS tests

## 0.1.1-1638 — Scan status no longer covers the list on a phone

- Move each page's scan status ("last month · Not fully read") out of the page actions into the page bar's info button (SDK `PageInfo`), with its details and the page's notes in one place: on a phone the status no longer floats as a pill over the list, and the button is tinted while the status warns. A paused scan's status on Recycle Bin moves there too; its "View" button stays an action
- Pin UI ~0.25.12: the soft keyboard stays up on Android, phone pages keep equal margins, a list in a split view keeps its full width, and toolbar actions wrap instead of floating
- Includes the unreleased 0.1.1-1637 changes: clear reasons and next steps when a cleanup cannot delete, and no "Temporary folder during cleanup" note before deleting
- UI-only change: repackaged from the 0.1.1-1636 SPKs with their CLI, App, CGI, executor and root programs reused unchanged (no Rust build); SDK/App 0.53.37 (executor/v0.53.6), CLI 0.53.15 and templates 0.42.1 are unchanged
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the affected page and component unit tests, typecheck, lint, the UI and DSM builds and a 390 × 844 phone check of the fixture App; no NAS tests

## 0.1.1-1637 — Clear reasons when a cleanup cannot delete

- Stop showing the "Temporary folder during cleanup" note before deleting: some users read it as a folder they had to create. The hidden .synoreclaim folders are made and removed automatically; while cleanup runs, an info icon explains them
- Name the cause and the next step when a cleanup is refused for an item that is protected, a mount point, on a volume that cannot be cleaned, outside the allowed folders, holding unsupported files, or left behind by an interrupted cleanup, and let the user remove that item from the cleanup list to clean up the rest
- Explain refusals caused by an unconfirmed item identity, Syno Reclaim's own records, its cleanup log, a read or write error or the maintenance check, instead of the generic "The cleanup plan was refused"
- List the entries a partial cleanup left, with their status, and suggest scanning the shared folder again
- UI-only change: the Rust CLI, SDK/App 0.53.37 (executor/v0.53.6), UI 0.25.9, CLI 0.53.15 and templates 0.42.1 are unchanged
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the Review unit tests, i18n catalog tests, typecheck, lint and fixture screenshots; no NAS tests

## 0.1.1-1636 — Install no longer ends with "The operation failed"

- Fix a fresh install that ended with "The operation failed. Please sign in to DSM again and retry. undefined is not an object (evaluating 't.indexOf')" and an App window that would not open until the DSM page was reloaded: CLI 0.53.15 no longer asks DSM to reload nginx at install and uninstall for an executor-registered Portal entry, so the reload no longer breaks Package Center's refresh of the DSM API list; the executor still reloads nginx when it removes the entry
- Pin CLI ~0.53.15 with locked dependencies; SDK/App 0.53.37 (executor/v0.53.6), UI 0.25.9 and templates 0.42.1 are unchanged
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the fast gate's SPK build and inspection of every architecture with the catalog; released without a final NAS install check

## 0.1.1-1635 — Wizard confirm dialogs stay in the app window

- Cover only the App window, not the whole DSM desktop, with confirm dialogs opened from a floating dialog or wizard: UI 0.25.9 scopes their backdrop to the App
- Keep "No data" off the header of an empty table that is only as tall as its header (UI 0.25.9)
- Pin UI ~0.25.9 and CLI ~0.53.14 with locked dependencies; SDK/App 0.53.37 (executor/v0.53.6) and templates 0.42.1 are unchanged
- CLI 0.53.14 estimates the installed INFO size, icons included, in check, build and inspect: about 28.6 KB for every architecture, under the 56 KiB warning, so the icons stay as they are
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the catalog check, one build of each architecture, SPK inspection with the catalog and against 1634, and archive privacy scans; no unit, browser, qemu or NAS tests; screenshots are reused from 1634

## 0.1.1-1634 — Bottom tab bar on a phone

- Move phone navigation to a bottom tab bar: SDK 0.53.37's AppShell lists the pages as icon-only tabs, up to four plus More, which holds the rest and the navigation drawer; desktop widths and DSM windows are unchanged
- Pin SDK/App 0.53.37 (executor/v0.53.6, unchanged), UI ~0.25.8, CLI ~0.53.13 and templates 0.42.1 with locked dependencies
- No App-side phone navigation workaround to remove: the App fills no tabbar slot and uses the SDK default
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the catalog check, one build of each architecture, SPK inspection with the catalog and against 1633, and archive privacy scans; no unit, browser, qemu or NAS tests; screenshots are reused from 1633

## 0.1.1-1633 — Full tables on a phone

- Show every row of Explore and other tables on a phone, with the whole page scrolling: UI 0.25.8 lays lists out at their content height there instead of in a short inner scroll area
- Pin SDK/App 0.53.36 (executor/v0.53.6), UI ~0.25.8, CLI ~0.53.13 and templates 0.42.1 with locked dependencies
- No App-side phone height workaround to remove: the pages' fill wrappers serve the DSM window, and UI 0.25.8 overrides them on a phone
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the catalog check, one build of each architecture, SPK inspection with the catalog and against 1632, and archive privacy scans; no unit, browser, qemu or NAS tests; screenshots are reused from 1632

## 0.1.1-1632 — Smoother Explore and phone approval fix

- Make dragging the Explore divider and resizing the window smoother; tables stay responsive while their width changes
- Fix an occasional "Task not exist" error when approving from a phone: the CLI reads back a scheduler write DSM refused before failing the approval
- Pin SDK/App 0.53.34 (executor/v0.53.6), UI 0.25.7, CLI ~0.53.12 and templates 0.42.0 with locked dependencies
- Store descriptions and release notes come only from the catalog (`SYNOPKGLAND_CATALOG`); the manifest no longer carries them
- Retain existing settings, saved scan reports and all three package architectures
- Validate with the catalog check, one build of each architecture, SPK inspection with the catalog and against 1631, and archive privacy scans; no unit, browser, qemu or NAS tests; screenshots are reused from 1631
- Permissions, root programs and task templates are unchanged from 1631; only the executor differs, so approval carryover is eligible when the protected runtime and exact DSM task readback match

## 0.1.1-1631 — Reliable sign-in and busy requests

- Improve sign-in handling when connection checks fail, and make selected items clearer on phones
- Use the SDK for executor admission and safe busy retry; retain the UI queue for polling and stop actions
- Pin SDK/App 0.53.19, UI 0.25.2 and CLI ~0.52.11 with locked dependencies
- Retain existing settings, saved scan reports and all three package architectures
- Validate with builds, SPK inspection, archive privacy scans and focused unit tests for the removed retry workaround
- Permissions, root program bytes and task templates are unchanged from 1630. Approval carryover is conditional on matching protected runtime and exact DSM task readback

## 0.1.1-1630 — Shared interface and startup update

- Refresh desktop and phone corners, focus styles, overlays and touch feedback
- Improve startup feedback and authorization messages through the shared SDK
- Pin SDK/App 0.53.15, UI 0.25.1 and CLI ~0.52.10 with locked dependencies
- Retain existing settings, scan reports and all three package architectures
- Keep the existing root program bytes and executor pins; approval carryover still requires matching protected runtime and exact DSM task readback
- Prepare this dependency-only release with builds, SPK inspection and archive privacy scans; unit, browser, qemu and NAS tests are outside its scope

## 0.1.1-1629 — Reliable phone scrolling

- Fix phone scrolling when the hidden header reappears, and keep the header above page content
- Improve the phone toolbar and settings action buttons
- Keep read-only system task checks in memory to avoid waking disks
- Update SDK/App to 0.53.6 and UI to 0.23.7; retain settings and scan reports
- System task permissions, root programs and task templates are unchanged from 1628; approval carryover requires a matching protected runtime and exact DSM task readback

## 0.1.1-1626 — Newest scan per shared folder and Explore layout fixes

- Explore and Recycle Bin answer each shared folder from its newest scan: a shared folder scanned after the whole-volume scan shows its own newer results, marked with its scan time
- The page header names the scan's scope, and a report picker switches to one saved scan
- Recycle Bin lists every oldest item of a shared folder, not only the oldest one
- Explore fits a maximized 1440 px window: full columns, wrapped text and treemap labels fitted after resizing
- The cleanup review keeps its name and offers Cancel while you confirm
- `top` and `recycle` (CLI, ctl) take `--report`/`report` to answer from one saved scan; a volume `recycle` lists newer shared-folder scans with their own `report_info`
- Rebuild Rust payloads for x86_64, aarch64 and armv7; retain settings and reports
- Changed root programs require administrator approval after upgrading from 0.1.1-1622

## 0.1.1-1622 — Reliable scan resume and complete command results

- Resume paused scans by reusing unchanged units and rereading changed units; retain progress and start a fresh job only when requested
- Return complete ctl/MCP outcomes and preserve saved reports
- Pin SDK/App 0.50.15, UI 0.20.12 and CLI 0.48.21; retain templates 0.40.1
- Rebuild Rust payloads for x86_64, aarch64 and armv7; retain settings and reports
- Changed root programs require administrator approval after upgrading

## 0.1.1-1621 — Shared UI update

- Pin UI 0.20.9
- Use the shared full-page typography, stable nested layouts and corrected card and tab overflow
- Keep SDK/App 0.50.12, CLI 0.48.18 and templates 0.40.1
- Reuse the unchanged runtime, executor and root programs for x86_64, aarch64 and armv7
- Preserve settings, saved scan reports and the protected authorization lifecycle

## 0.1.1-1619 — Resizable details and approval diagnostics

- Rebuild with SDK/App 0.50.5, UI 0.20.3, CLI 0.48.11 and templates 0.40.1
- Preserve approval failure diagnostics and complete App and AI command responses
- Resize Recycle Bin and Packages detail panes with the pointer or keyboard, and remember each page's width
- Remember table column widths separately for each App
- Show compact, single-line recycle dates with full timestamps in tooltips
- Retain settings, scan reports and unchanged root programs on x86_64, aarch64 and armv7

## 0.1.1-1617 — Detail panels and callouts

- Update UI to 0.20.1 to keep the page undimmed when a selected row opens its detail panel
- Use the UI library's corrected callout width, dismissal behavior and centered close icon
- Reuse the unchanged runtime and protected payloads from 0.1.1-1616 for all three architectures

## 0.1.1-1616 — Compact pages and shared request budgets

- Remove large page titles while keeping page actions in the toolbar
- Keep slow DSM reads loading until the shared five-minute SDK budget or cancellation; preserve live job IDs without a total polling deadline
- Pin SDK/App 0.50.0, UI 0.20.0 and CLI 0.48.4; rebuild x86_64, aarch64 and armv7
- Changed root payloads and policy require administrator reapproval after upgrading

## 0.1.1-1614 — DSM window WebAPI

- Read snapshot names and lock states through the SDK window’s `ctx.webApi`; remove the direct session-token fetch
- Keep report-name fallbacks and unknown lock states when DSM refuses a read; keep the local development fixtures
- Update CLI to 0.47.4 and frontend/native SDK to 0.46.2; UI remains at the latest 0.19.4
- Rebuild x86_64, aarch64 and armv7 packages; retain settings, reports and existing screenshots

## 0.1.1-1613 — VMM snapshot status and management links

- Read local VMM snapshot locks through the administrator's DSM session, including scheduled snapshots; missing or ambiguous evidence stays unknown
- Open Virtual Machine Manager for VM disks, SAN Manager for LUNs and Snapshot Replication for shared folders
- Update native and frontend SDK to 0.46.1, UI to 0.19.4 and CLI to 0.47.3; rebuild all three architectures
- Keep all snapshot inventory reads behind one DSM WebAPI adapter until the generated window exposes `ctx.webApi`

## 0.1.1-1604 — Calm default motion

- Smoother transitions for dialogs, menus, panels and notifications (UI 0.19.0)
- Reuse the exact native binaries from build 1603 on x86_64, aarch64 and armv7; scanning and cleanup behavior is unchanged
- 對話框、選單、面板與通知的轉場更順暢（UI 0.19.0）
- 沿用 build 1603 的 x86_64、aarch64 與 armv7 原生執行檔，掃描與清理行為不變

## 0.1.1-1603 — SDK security and supervision updates

- Rebuild with SDK/App 0.41.0, UI 0.18.3, CLI 0.41.13 and templates 0.36.0
- Inherit the SDK security fixes: public-network-only outbound requests, bounded executor admission, gateway pre-authentication locking and owner-bound staging prefixes, with executor v0.39.0
- Inherit the SDK supervision policy for package processes
- No feature changes; existing settings and reports are retained
- Existing approval can be retained when the protected runtime and exact DSM tasks still match the previous release; root programs, permissions and task templates are unchanged


## 0.1.1-1595: SDK entry-gate migration

Built with SDK/App/templates 0.32.0, CLI 0.34.0 and executor v0.32.0.
The DSM frontend uses ES modules and lazy page chunks; missing chunks reload only after the SDK confirms a newer installed UI.
Protected executor payloads changed: administrator approval is required again after upgrading.
No NAS installation or device acceptance was performed for this rebuild.

## 0.1.1-1594 — Adaptive space attribution

- Scale metadata read allowances to used space, extent count and snapshot sharing, independently of the 512 MiB memory limit
- Record the selected allowance and its inputs in scan results
- Refine remaining space from a saved volume scan, prioritising larger extents without walking live folders again
- Pause and resume the extra reads from durable checkpoints with low I/O priority
- Keep changed references and unreadable metadata explicitly unknown, and show the extra analysis time
- Administrator approval is required again after this upgrade
- 依使用容量、extent 數與快照共用情形調整中繼資料讀取預算，維持 512 MiB 記憶體上限
- 掃描結果記錄選定的讀取預算與依據
- 沿用磁碟區掃描追加分析剩餘空間，優先處理較大的 extent，不重新走訪目前的資料夾
- 以低 I/O 優先權追加讀取，支援耐久檢查點與暫停後繼續
- 已變動的參照與無法讀取的中繼資料仍明列為未知，並顯示追加耗時
- 升級後須由管理員重新核准
