//! The fixed system-partition table: what each path on `/` is, and
//! whether `rootfs plan` may move it to a shared folder. Pure: paths are
//! absolute byte strings relative to the root, and nothing is read.

use serde::{Deserialize, Serialize};

/// What a path on the system partition holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// What an interrupted DSM update left behind (KI#48627, KI#83238).
    UpdateLeftover,
    /// Rotated or archived logs (`*.xz`, `healthtest/dhm*`).
    LogArchive,
    /// Core dumps and crash reports under `/var/crash`.
    CrashDump,
    /// Named temporary files daemons leave in `/var/tmp` (KI#46779, KI#78323).
    TempDebris,
    /// Live logs, capped by DSM's `/var/log` quota.
    Logs,
    /// Log Center's live databases (`.SYNO*DB`).
    LogCenterDb,
    /// The firewall's GeoIP database (KI#70873).
    GeoipDatabase,
    /// Re-uploadable DSM wallpaper/login image resources, never usersettings.
    PreferenceImage,
    /// Rootfs-local Winbind cache; rebuilding it requires the documented service restart.
    SambaCache,
    /// Package managers' download caches in root's home (npm, Bun, Yarn,
    /// pip): the administrator's tools download them again when needed.
    ToolCache,
    /// Hyper Backup's copy of itself for restoring the entire system, which
    /// DSM recreates after its next successful update.
    BackupInstaller,
    /// Package programs and data (`@appstore`, `@appdata`, `/var/packages`).
    PackageData,
    /// What the administrator installed: `/root` and `/usr/local`.
    UserTooling,
    /// synoreclaim's own state directory.
    OwnState,
    /// Names Synology says never to delete (the md0-full SOP).
    DsmKeep,
    /// DSM's programs, libraries and configuration.
    DsmSystem,
    /// Anything else.
    Other,
}

/// The object type at a path: the table treats some names differently
/// for files and directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Dir,
}

/// A path's category, and whether `rootfs plan` may move it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Class {
    pub category: Category,
    pub movable: bool,
}

impl Class {
    pub fn movable(category: Category) -> Self {
        Class {
            category,
            movable: true,
        }
    }

    pub fn kept(category: Category) -> Self {
        Class {
            category,
            movable: false,
        }
    }
}

/// Directories below which everything is DSM's own.
const DSM_ROOTS: [&[u8]; 11] = [
    b"usr",
    b"etc",
    b"etc.defaults",
    b"bin",
    b"sbin",
    b"lib",
    b"lib32",
    b"lib64",
    b"libx32",
    b"boot",
    b"initrd",
];

/// The class of `path` (absolute, relative to the root) of type `kind`.
/// Names Synology says to keep win anywhere, then package data, then
/// the place a path lies in; only the fixed places below are movable.
pub fn classify(path: &[u8], kind: Kind) -> Class {
    let parts: Vec<&[u8]> = components(path).collect();
    if kind == Kind::File && is_disk_latency_archive(&parts) {
        return Class::movable(Category::LogArchive);
    }
    if let Some(category) = parts.iter().find_map(|name| kept_name(name)) {
        return Class::kept(category);
    }
    if is_kept_upgrade_file(&parts) {
        return Class::kept(Category::DsmKeep);
    }
    match parts.as_slice() {
        [b"var", b"packages", ..] => Class::kept(Category::PackageData),
        [b"usr", b"local", b"packages", ..] => Class::kept(Category::PackageData),
        [b"root", ..] | [b"usr", b"local", ..] => Class::kept(Category::UserTooling),
        [top, ..] if DSM_ROOTS.contains(top) => Class::kept(Category::DsmSystem),
        [b"var", b"lib", b"synoreclaim", ..] => Class::kept(Category::OwnState),
        [b"var", b"db", b"geoip-database", ..] => Class::kept(Category::GeoipDatabase),
        [b"var", b"log", rest @ ..] => log_class(rest, kind),
        [b"var", b"crash"] => Class::kept(Category::CrashDump),
        [b"var", b"crash", _, ..] => Class::movable(Category::CrashDump),
        [b"var", b"tmp", name, ..] if is_temp_debris(name) => Class::movable(Category::TempDebris),
        // Whether DSM recreates these before it next updates is unknown,
        // so the folders themselves stay.
        [b"upd@te" | b".SynoUpgradeBootupSmallUpdateFiles" | b"@autoupdate"]
            if kind == Kind::Dir =>
        {
            Class::kept(Category::UpdateLeftover)
        }
        [
            b"upd@te" | b".SynoUpgradeBootupSmallUpdateFiles" | b"@autoupdate",
            ..,
        ]
        | [b".SynoUpgrade.tar"] => Class::movable(Category::UpdateLeftover),
        [b".log.junior", name] if name.starts_with(b"logs.tar.") => {
            Class::movable(Category::UpdateLeftover)
        }
        _ => Class::kept(Category::Other),
    }
}

/// Additional fixed md0 SOP resources. Standard classification stays unchanged.
pub fn classify_deep(path: &[u8], kind: Kind) -> Class {
    deepen(classify(path, kind), path, kind)
}

/// [`classify_deep`] from `base`, the Standard class of `path`: a walk
/// that needs both classes classifies each entry once.
pub fn deepen(base: Class, path: &[u8], kind: Kind) -> Class {
    if matches!(
        base.category,
        Category::DsmKeep | Category::PackageData | Category::LogCenterDb | Category::OwnState
    ) {
        return base;
    }
    let parts: Vec<&[u8]> = components(path).collect();
    match parts.as_slice() {
        [b"var", b"db", b"geoip-database", _, ..] => Class::movable(Category::GeoipDatabase),
        [b"var", b"lib", b"samba", b"winbindd_cache.tdb"] if kind == Kind::File => {
            Class::movable(Category::SambaCache)
        }
        [b"root", rest @ ..] if tool_cache(rest) => Class::movable(Category::ToolCache),
        [
            b"var",
            b"synobackup",
            b"HyperBackup-ED",
            b"HyperBackup-ED.latest.spk",
        ] if kind == Kind::File => Class::movable(Category::BackupInstaller),
        [
            b"usr",
            b"syno",
            b"etc",
            b"preference",
            user @ ..,
            b"wallpaper" | b"wallpaper_hd",
        ] if kind == Kind::File && preference_user(user) => {
            Class::movable(Category::PreferenceImage)
        }
        [
            b"usr",
            b"syno",
            b"etc",
            b"preference",
            user @ ..,
            b"wallpaper_dir",
            name,
        ] if kind == Kind::File && preference_user(user) && history_image(name, b"") => {
            Class::movable(Category::PreferenceImage)
        }
        [
            b"usr",
            b"syno",
            b"etc",
            b"login_image" | b"fbsharing_login_image",
            name,
        ] if kind == Kind::File
            && (history_image(name, b"logo") || history_image(name, b"background")) =>
        {
            Class::movable(Category::PreferenceImage)
        }
        [
            b"usr",
            b"syno",
            b"etc",
            b"login_background.jpg"
            | b"fbsharing_login_background.jpg"
            | b"fbsharing_login_logo.png",
        ] if kind == Kind::File => Class::movable(Category::PreferenceImage),
        [b"var", b"log", name]
            if kind == Kind::File && name.starts_with(b"dms.log.") && name.ends_with(b".gz") =>
        {
            Class::movable(Category::LogArchive)
        }
        _ => base,
    }
}

/// What lies in a package manager's download cache below `/root`; the
/// cache folder itself stays, as the tool expects it.
fn tool_cache(below_root: &[&[u8]]) -> bool {
    matches!(
        below_root,
        [b".npm", b"_cacache", _, ..]
            | [b".bun", b"install", b"cache", _, ..]
            | [b".cache", b"pip" | b"yarn", _, ..]
    )
}

fn preference_user(parts: &[&[u8]]) -> bool {
    matches!(parts, [_])
        || matches!(parts, [domain, _, _] if domain.starts_with(b"@LH-") || domain.starts_with(b"@DH-"))
}

// libdsm AddToHistory: eight slots, optional generated thumbnail/HD copy.
fn history_image(name: &[u8], prefix: &[u8]) -> bool {
    let Some(name) = name.strip_prefix(prefix) else {
        return false;
    };
    let Some((&index, rest)) = name.split_first() else {
        return false;
    };
    if !(b'0'..=b'7').contains(&index) {
        return false;
    }
    let rest = rest
        .strip_prefix(b"_hd")
        .or_else(|| rest.strip_prefix(b"thumb"))
        .unwrap_or(rest);
    [b".jpg".as_slice(), b".jpeg", b".png", b".gif", b".webp"].contains(&rest)
}

/// Recognize image payloads as well as their fixed resource names.
pub fn image_header(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

/// The package whose data `path` is, when it lies in one of the folders
/// DSM keeps for each package.
pub fn package_name(path: &[u8]) -> Option<&[u8]> {
    let parts: Vec<&[u8]> = components(path).collect();
    match parts.as_slice() {
        [b"var", b"packages", name, ..]
        | [
            b"usr",
            b"local",
            b"packages",
            b"@appdata" | b"@appstore" | b"@apphome" | b"@appshare" | b"@apptemp",
            name,
            ..,
        ] => Some(name),
        _ => None,
    }
}

/// The category of a name kept wherever it appears.
fn kept_name(name: &[u8]) -> Option<Category> {
    match name {
        b".upd@te" | b".upd@te_sub" | b"space-preserve-for-configs" | b"winbindd_idmap.tdb" => {
            Some(Category::DsmKeep)
        }
        b"@appstore" | b"@appdata" => Some(Category::PackageData),
        _ if name.starts_with(b".SYNO") && contains(name, b"DB") => Some(Category::LogCenterDb),
        _ => None,
    }
}

/// `/var/log/disk-latency/.SYNODISKLATENCYDB.<n>.xz`: the disk latency
/// history DSM archives off its live database, which support moves off
/// a full system partition. The live database itself stays.
fn is_disk_latency_archive(parts: &[&[u8]]) -> bool {
    let [b"var", b"log", b"disk-latency", name] = parts else {
        return false;
    };
    name.strip_prefix(b".SYNODISKLATENCYDB.")
        .and_then(|rest| rest.strip_suffix(b".xz"))
        .is_some_and(|n| !n.is_empty() && n.iter().all(u8::is_ascii_digit))
}

/// `/SynoUpgrade*.tar.gz` and `/SynoUpgrade*.txz`: evidence developers
/// need when a junior-mode update failed.
fn is_kept_upgrade_file(parts: &[&[u8]]) -> bool {
    matches!(parts, [name] if name.starts_with(b"SynoUpgrade")
        && (name.ends_with(b".tar.gz") || name.ends_with(b".txz")))
}

/// Below `/var/log`: archives are movable, everything else is a live log.
fn log_class(rest: &[&[u8]], kind: Kind) -> Class {
    match rest {
        [b"healthtest", name, ..] if name.starts_with(b"dhm") => {
            Class::movable(Category::LogArchive)
        }
        [.., name] if kind == Kind::File && name.ends_with(b".xz") => {
            Class::movable(Category::LogArchive)
        }
        _ => Class::kept(Category::Logs),
    }
}

fn is_temp_debris(name: &[u8]) -> bool {
    name.starts_with(b"etilqs_") || name.starts_with(b"convert_thumbnail_")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The non-empty components of `path`.
fn components(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.split(|&b| b == b'/').filter(|part| !part.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> Class {
        classify(path.as_bytes(), Kind::File)
    }

    #[test]
    fn kb_deep_paths_expand_only_the_fixed_cache_and_image_allowlist() {
        for path in [
            "/var/db/geoip-database/xt_geoip/LE/TW.iv4",
            "/var/lib/samba/winbindd_cache.tdb",
            "/usr/syno/etc/preference/alice/wallpaper",
            "/usr/syno/etc/preference/alice/wallpaper_hd",
            "/var/log/dms.log.1.gz",
        ] {
            assert!(
                !classify(path.as_bytes(), Kind::File).movable,
                "standard: {path}"
            );
            assert!(
                classify_deep(path.as_bytes(), Kind::File).movable,
                "deep: {path}"
            );
        }
        for path in [
            "/usr/syno/etc/preference/alice/usersettings",
            "/usr/syno/etc/preference/alice/secrets.jpg",
            "/usr/syno/etc/preference.tmp/alice/wallpaper",
            "/var/lib/samba/winbindd_idmap.tdb",
            "/var/db/geoip-database/@appdata/config",
            "/volume1/share/wallpaper",
            "/root/archive.tar",
            "/usr/local/bin/gs",
        ] {
            assert!(
                !classify_deep(path.as_bytes(), Kind::File).movable,
                "protected: {path}"
            );
        }
        assert!(!classify_deep(b"/usr/syno/etc/preference/alice/wallpaper", Kind::Dir).movable);
        assert!(!classify_deep(b"/var/db/geoip-database", Kind::Dir).movable);
    }

    #[test]
    fn update_leftovers_are_movable() {
        for path in [
            "/upd@te",
            "/upd@te/hda1.tgz",
            "/.SynoUpgrade.tar",
            "/.SynoUpgradeBootupSmallUpdateFiles/x",
            "/@autoupdate/a/b",
            "/.log.junior/logs.tar.gz",
        ] {
            assert_eq!(
                file(path),
                Class::movable(Category::UpdateLeftover),
                "{path}"
            );
        }
        // DSM's download folder has one `@` (SZF_DOWNLOADDIR in
        // juniorinstaller-ng synoupgrade.c; KB system_partition_full).
        assert_eq!(file("/@@autoupdate/x"), Class::kept(Category::Other));
    }

    fn dir(path: &str) -> Class {
        classify(path.as_bytes(), Kind::Dir)
    }

    #[test]
    fn the_top_folders_dsm_updates_into_stay_while_what_they_hold_moves() {
        for path in [
            "/upd@te",
            "/.SynoUpgradeBootupSmallUpdateFiles",
            "/@autoupdate",
        ] {
            assert_eq!(dir(path), Class::kept(Category::UpdateLeftover), "{path}");
            let inside = format!("{path}/x");
            assert_eq!(
                dir(&inside),
                Class::movable(Category::UpdateLeftover),
                "{inside}"
            );
        }
    }

    #[test]
    fn names_synology_says_never_to_delete_are_kept_even_inside_a_movable_tree() {
        for path in [
            "/.upd@te",
            "/.upd@te_sub/x",
            "/upd@te/.upd@te",
            "/SynoUpgrade_90069.tar.gz",
            "/SynoUpgrade.txz",
            "/usr/syno/synoinstall/space-preserve-for-configs",
            "/var/crash/space-preserve-for-configs",
        ] {
            assert_eq!(file(path), Class::kept(Category::DsmKeep), "{path}");
        }
        assert_eq!(
            file("/var/lib/samba/winbindd_idmap.tdb"),
            Class::kept(Category::DsmKeep)
        );
    }

    #[test]
    fn rotated_log_archives_are_movable_and_live_logs_are_not() {
        for path in [
            "/var/log/messages.1.xz",
            "/var/log/disk-latency/2026-09-01.xz",
            "/var/log/healthtest/dhm_sda.log",
            "/var/log/healthtest/dhm_sda/x",
        ] {
            assert_eq!(file(path), Class::movable(Category::LogArchive), "{path}");
        }
        assert_eq!(
            dir("/var/log/healthtest/dhm_sda"),
            Class::movable(Category::LogArchive)
        );
        assert_eq!(dir("/var/log/old.xz"), Class::kept(Category::Logs));
        for path in ["/var/log", "/var/log/messages", "/var/log/healthtest/other"] {
            assert_eq!(file(path), Class::kept(Category::Logs), "{path}");
        }
    }

    #[test]
    fn log_center_databases_are_kept_even_when_archived() {
        for path in [
            "/var/log/synolog/.SYNOSYSDB",
            "/var/log/synolog/.SYNOCONNDB-wal",
            "/var/log/synolog/.SYNODISKDB.xz",
        ] {
            assert_eq!(file(path), Class::kept(Category::LogCenterDb), "{path}");
        }
    }

    #[test]
    fn disk_latency_archives_move_but_its_live_database_stays() {
        for path in [
            "/var/log/disk-latency/.SYNODISKLATENCYDB.1.xz",
            "/var/log/disk-latency/.SYNODISKLATENCYDB.20.xz",
        ] {
            assert_eq!(file(path), Class::movable(Category::LogArchive), "{path}");
        }
        for path in [
            "/var/log/disk-latency/.SYNODISKLATENCYDB",
            "/var/log/disk-latency/.SYNODISKLATENCYDB-wal",
            "/var/log/disk-latency/.SYNODISKLATENCYDB.xz",
            "/var/log/disk-latency/.SYNODISKLATENCYDB.1a.xz",
            "/var/log/disk-latency/x/.SYNODISKLATENCYDB.1.xz",
            "/var/log/synolog/.SYNODISKLATENCYDB.1.xz",
        ] {
            assert_eq!(file(path), Class::kept(Category::LogCenterDb), "{path}");
        }
        assert!(!dir("/var/log/disk-latency/.SYNODISKLATENCYDB.1.xz").movable);
    }

    #[test]
    fn deep_moves_root_s_package_manager_caches_but_not_the_rest_of_root() {
        for path in [
            "/root/.npm/_cacache/content-v2/sha512/ab/cd",
            "/root/.npm/_cacache/index-v5/00/11",
            "/root/.bun/install/cache/react@18.3.1@@@1/index.js",
            "/root/.cache/pip/http-v2/a/b",
            "/root/.cache/yarn/v6/npm-x/package.json",
        ] {
            assert_eq!(
                file(path),
                Class::kept(Category::UserTooling),
                "standard: {path}"
            );
            assert_eq!(
                classify_deep(path.as_bytes(), Kind::File),
                Class::movable(Category::ToolCache),
                "deep: {path}"
            );
        }
        assert_eq!(
            classify_deep(b"/root/.npm/_cacache/content-v2", Kind::Dir),
            Class::movable(Category::ToolCache)
        );
        for path in [
            "/root/.npm/_cacache",
            "/root/.bun/install/cache",
            "/root/.cache/pip",
            "/root/.npm/_npx/x/node_modules/a.js",
            "/root/.npm/_logs/debug.log",
            "/root/.npmrc",
            "/root/.bun/bin/bun",
            "/root/.cache/other/x",
            "/root/archive/.npm/_cacache/x",
            "/home/alice/.npm/_cacache/x",
            "/usr/local/lib/node_modules/npm/x",
        ] {
            assert!(
                !classify_deep(path.as_bytes(), Kind::File).movable,
                "kept: {path}"
            );
        }
        for top in [
            "/root/.npm/_cacache",
            "/root/.bun/install/cache",
            "/root/.cache/pip",
        ] {
            assert!(!classify_deep(top.as_bytes(), Kind::Dir).movable, "{top}");
        }
    }

    #[test]
    fn deep_classifies_hyper_backup_s_restore_installer_as_its_own_category() {
        let spk = b"/var/synobackup/HyperBackup-ED/HyperBackup-ED.latest.spk";
        assert!(!classify(spk, Kind::File).movable);
        assert_eq!(
            classify_deep(spk, Kind::File),
            Class::movable(Category::BackupInstaller)
        );
        assert!(!classify_deep(spk, Kind::Dir).movable);
        for path in [
            "/var/synobackup/HyperBackup-ED",
            "/var/synobackup/HyperBackup-ED/HyperBackup-ED.spk",
            "/var/synobackup/HyperBackup-ED/config",
            "/var/synobackup/other.latest.spk",
        ] {
            assert!(
                !classify_deep(path.as_bytes(), Kind::File).movable,
                "{path}"
            );
        }
    }

    #[test]
    fn crash_dumps_and_named_temp_debris_are_movable_but_not_their_dsm_folders() {
        assert_eq!(
            file("/var/crash/core.synoscgi"),
            Class::movable(Category::CrashDump)
        );
        assert_eq!(dir("/var/crash/2026"), Class::movable(Category::CrashDump));
        assert_eq!(dir("/var/crash"), Class::kept(Category::CrashDump));
        for path in ["/var/tmp/etilqs_4f2a", "/var/tmp/convert_thumbnail_9/x.jpg"] {
            assert_eq!(file(path), Class::movable(Category::TempDebris), "{path}");
        }
        assert_eq!(dir("/var/tmp"), Class::kept(Category::Other));
        assert_eq!(file("/var/tmp/other"), Class::kept(Category::Other));
    }

    #[test]
    fn package_data_user_tooling_and_databases_are_report_only() {
        for path in [
            "/var/packages/ActiveInsight/target/x",
            "/usr/local/packages/@appdata/ActiveInsight/statistic.sqlite",
            "/usr/local/packages/@appstore/SynoFinder/x",
            "/volume1/@appstore/x",
        ] {
            assert_eq!(file(path), Class::kept(Category::PackageData), "{path}");
        }
        for path in [
            "/root/.cache/x",
            "/usr/local/lib/node_modules/x",
            "/usr/local/bin/gs",
        ] {
            assert_eq!(file(path), Class::kept(Category::UserTooling), "{path}");
        }
        assert_eq!(
            file("/var/db/geoip-database/GeoLite2-City.mmdb"),
            Class::kept(Category::GeoipDatabase)
        );
        assert_eq!(
            file("/var/lib/synoreclaim/reports/x"),
            Class::kept(Category::OwnState)
        );
    }

    #[test]
    fn dsm_programs_and_configuration_are_kept() {
        for path in [
            "/usr/syno/bin/synocleaner",
            "/usr/lib/x.so",
            "/etc/synoinfo.conf",
            "/etc.defaults/VERSION",
            "/bin/sh",
            "/sbin/init",
            "/lib/libc.so",
            "/lib64/ld.so",
            "/boot/x",
            "/usr/crash/core.x",
            "/etc/var/crash/core.x",
            "/usr/upd@te",
        ] {
            assert_eq!(file(path), Class::kept(Category::DsmSystem), "{path}");
        }
    }

    #[test]
    fn a_name_only_matches_at_its_own_place() {
        for path in [
            "/var/log/x/upd@te",
            "/root/upd@te",
            "/var/crash.xz",
            "/.log.junior/other.tar.gz",
            "/.log.junior",
            "/x/var/crash/core",
            "/",
        ] {
            assert!(!file(path).movable, "{path}");
        }
        assert_eq!(file("/root/upd@te"), Class::kept(Category::UserTooling));
    }

    #[test]
    fn package_data_names_its_package() {
        fn name(path: &str) -> Option<&[u8]> {
            package_name(path.as_bytes())
        }
        assert_eq!(
            name("/var/packages/ActiveInsight/etc"),
            Some(&b"ActiveInsight"[..])
        );
        assert_eq!(
            name("/usr/local/packages/@appdata/SynoFinder/x"),
            Some(&b"SynoFinder"[..])
        );
        assert_eq!(
            name("/usr/local/packages/@appstore/Python2"),
            Some(&b"Python2"[..])
        );
        // DSM's per-package home, share and temp folders name it too.
        for folder in ["@apphome", "@appshare", "@apptemp"] {
            assert_eq!(
                name(&format!("/usr/local/packages/{folder}/ActiveInsight/x")),
                Some(&b"ActiveInsight"[..]),
                "{folder}"
            );
        }
        assert_eq!(name("/usr/local/packages/@appstore"), None);
        assert_eq!(name("/usr/local/packages/@tmp/synopkg.tmp"), None);
        assert_eq!(name("/var/packages"), None);
        assert_eq!(name("/usr/local/bin/x"), None);
    }
}
