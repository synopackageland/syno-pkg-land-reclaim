# System-partition cleanup modes

`rootfs plan --to <folder-in-share> [--mode standard|deep]` defaults to
`standard`. WebAPI `rootfs.plan` accepts the same closed enum. Apply accepts
only the protected plan ID. It executes the sealed policy through the existing
approved root binary, with no shell, command arguments or service selector from
the caller. The changed binary requires a new payload digest approval. SDK/UI
pins, bootstrap, authentication, revocation and retained settings are preserved.

## Policy

Standard keeps the existing allowlist, diagnostic retention and DSM tools.
Deep follows DSM's guidance for a full system partition, plus narrowly
verified uploaded images:

| Item | Standard | Deep | Effect / manual follow-up |
| --- | --- | --- | --- |
| Existing eligible update leftovers, crashes, fixed temporary debris | Existing rules | Same guards | Copies remain available for diagnosis; inactive update files may need downloading again |
| Compressed log / junior boot archives | Existing age/size retention | Recent and small archives too | Diagnostic history leaves its original path |
| `/var/log/dms.log.*.gz` | Kept | Move closed archives | Media Server diagnostic history moves |
| `/var/db/geoip-database/*` | Report only | Move rootfs-local cache contents | Check location firewall rules; arrange GeoIP re-download with support |
| `/var/lib/samba/winbindd_cache.tdb` | Kept | Move only closed, regular rootfs file | Manually restart `pkg-synosamba-winbindd` on DSM 7 (Samba on DSM 6); domain access may pause |
| Uploaded preference / login images | Kept | Move only exact resource patterns and recognized JPEG/PNG/GIF/WebP headers | Users may need to upload and select wallpaper/login images again; settings remain |
| `/var/update/downloaded_autonano.pat` | Kept | DSM's own fixed download-clean API, if present/trusted | Download again before updating |
| `/usr/syno/synoinstall/space-preserve` | Kept | DSM's own fixed cleanup on builds 42661–64569 (7.1/7.1.1) only | Update DSM/packages soon; reboot may recreate reserve, but cleanup does not require reboot |
| `/var/log/disk-latency/.SYNODISKLATENCYDB.<n>.xz` | Move (no age limit) | Move | Archived disk latency history; the live `.SYNODISKLATENCYDB` stays |
| `/root/.npm/_cacache/*`, `/root/.bun/install/cache/*`, `/root/.cache/{pip,yarn}/*` | Report only | Move contents; cache folders stay | The administrator's package managers download again when next used |
| `/var/synobackup/HyperBackup-ED/HyperBackup-ED.latest.spk` | Kept | Move only while `/`, after the other moves, would still have less than a typical DSM update's room (519 MB); else `kept` as `update_room_enough` | DSM recreates it after a successful update; until then Hyper Backup cannot restore the entire system unless the file is copied back |
| Logs DSM finds older than it keeps | DSM rotates them | DSM's own `synocleaner --delete-log`, first among the tools | Permanently deleted, as DSM does before an update; the preview counts `--dry-run`'s list |

The backing-file operation follows DSM 7.1's own update preserve-space
behaviour. It never changes
support configuration or `space-preserve-for-configs`.

Image patterns are `/usr/syno/etc/preference/<user>/wallpaper`, `wallpaper_hd`,
and `wallpaper_dir/[0-7]{,_hd,thumb}.{jpg,jpeg,png,gif,webp}`. Local users have
one path component; domain/LDAP layouts are `@DH-*/*/*` or `@LH-*/*/*`.
Login history uses `/usr/syno/etc/{login_image,fbsharing_login_image}/` with
`{logo,background}[0-7]{,_hd,thumb}` and the same image extensions. Fixed legacy
resources `login_background.jpg`, `fbsharing_login_background.jpg`, and
`fbsharing_login_logo.png` are also recognized directly under `/usr/syno/etc`.
Directory entries, unrecognized images, usersettings and arbitrary image names
are retained. Symlinks to volume-backed preference directories are never
followed. Rules were checked against libdsm `image_selector_utils.cpp` and
`include/dsm/{image_selector_utils,webman}.h`, and libsynoHtmlHandler
`SynoSettings.cpp` (DSM 6.2 / 7.4).

## Execution and estimates

All moved resources use the existing private (0700, ACL checked) dated folder
under the user's selected destination: copy → size/SHA-256 verification → remove.
Existing shared-folder content is never a source. Rootfs mount, symlink,
hardlink, identity, open-file, inode-flag, size and destination quota/margin
checks remain. Active updates and pending updates stay protected.

Deep requests trusted `/usr/bin/logrotate -f /etc/logrotate.conf` in place of
normal rotation when available. This may permanently delete oldest rotations.
Existing obsolete-machine-ID journal cleanup and conditional syslog-ng restart
remain. New closed operations are exactly:

- `clean_downloaded_update`: `/usr/syno/bin/synowebapi --exec api=SYNO.Core.Upgrade.Server.Download method=clean`
- `release_update_reserve`: `/usr/syno/sbin/synoupgradepreserve --cleanup-backing-file`
- `delete_log`: `/usr/syno/bin/synocleaner --delete-log`, on builds whose synocleaner rotates logs (25423+). Plan runs `synocleaner --dry-run --delete-log` (it deletes nothing) and estimates the regular files it lists on `/`, each once, less the paths the plan moves first.

Presence, root-local file identity properties and trusted executables are
checked again at execution; the reserve build restriction is also rechecked.
The tools delete their own cache without backup, as disclosed in preview. No
new auto-restart or reboot is introduced. Result follow-ups are emitted only
for successfully moved categories or successful DSM tools.

The `rootfs` report's `cleanable` and `cleanable.deep` apply the same size,
age, boot, pinned-file and Hyper Backup limits as each mode's plan (only
held-open files are left to the plan), so Overview shows what each plan would
move.

Preview lists allocated bytes by category, exact paths, effects and tool input
estimates where measured. `additional_bytes` compares movable allocated bytes
with Standard; it excludes tool effects. Known tool input sizes are shown
separately, other savings are unknown. Actual `freed_bytes` remains a signed
statfs delta affected by concurrent writers. Deep is opt-in and requires a
second unchecked impact consent; back/mode/reopen resets consent. Plan, job,
result and history retain the mode. Legacy mode omission means Standard;
default fields are omitted from sealed payloads to retain old checksums.

## Exclusions and verification

This is a bounded subset of the KB, not its manual support SOP executed blindly.
Authentication/domain databases, settings, scheduler DBs, package binaries,
HA cluster operations, user tooling, hidden mount-point contents, broad temp
scans, live-log truncation and active-journal vacuum are excluded. No package
or shared data is moved. No automatic factory reset, restart or reboot occurs.

Development uses red/green tests and the build host's fast gate, plus all three SPK
architectures. No NAS cleanup or installation is part of this change. A future
approved device acceptance should compare previews, verify a newly approved
digest, exercise disposable rootfs fixtures, verify copies/manifests and test
manual follow-ups. Run the full release gate before a future release.
