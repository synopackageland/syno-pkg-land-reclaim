//! Who holds the bytes that deleting an entry would not free, and where
//! the user fixes that (SPEC 5.4 `HELD BY`, `WHERE TO FIX`). One unit
//! for every command that shows them.

use crate::dsm::Service;
use crate::model::accounting::{Blockers, Held, HeldClass};
use crate::model::namespace::FileKind;
use crate::model::tree::{Metrics, NodeFlags};
use crate::output::human_bytes;
use crate::query::Entry;

/// A holder of retained bytes: a class, or references not resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    Class(HeldClass),
    Unknown,
}

impl Holder {
    /// The JSON name.
    pub fn name(self) -> &'static str {
        match self {
            Holder::Class(class) => class.name(),
            Holder::Unknown => "unknown",
        }
    }

    /// The human name.
    pub fn label(self) -> &'static str {
        match self {
            Holder::Class(HeldClass::Snapshot) => "snapshot",
            Holder::Class(HeldClass::RecycleBin) => "recycle bin",
            Holder::Class(HeldClass::HardLink) => "hard link",
            Holder::Class(HeldClass::Reflink) => "reflink",
            Holder::Class(HeldClass::PendingCleaner) => "pending cleaner",
            Holder::Class(HeldClass::OpenFile) => "open file",
            Holder::Unknown => "unknown",
        }
    }
}

/// An entry's retained bytes per class: a directory's as the report
/// keeps them, a file's all in the class of its blockers.
pub fn retained(kind: FileKind, metrics: &Metrics, blockers: Blockers, held: Held) -> Held {
    if kind == FileKind::Directory {
        return held;
    }
    HeldClass::of(blockers).map_or_else(Held::default, |c| Held::of(c, metrics.shared()))
}

/// The holders of `held` and of `unknown` bytes, largest first, ties in
/// class order; unknown bytes are never merged into a class.
pub fn held_by(held: &Held, unknown: u64) -> Vec<(Holder, u64)> {
    let classes = HeldClass::ALL
        .iter()
        .map(|&c| (Holder::Class(c), held.get(c)));
    let mut holders: Vec<(Holder, u64)> = classes
        .chain([(Holder::Unknown, unknown)])
        .filter(|&(_, bytes)| bytes > 0)
        .collect();
    holders.sort_by_key(|&(_, bytes)| std::cmp::Reverse(bytes));
    holders
}

/// The largest holder and its bytes, `+N` when the N others hold at
/// least a tenth of the retained bytes; `—` when nothing is retained.
pub fn held_by_text(holders: &[(Holder, u64)]) -> String {
    let Some(&(largest, bytes)) = holders.first() else {
        return "—".to_string();
    };
    let others: u64 = holders[1..].iter().map(|&(_, b)| b).sum();
    let total = bytes + others;
    let mut text = format!("{} {}", largest.label(), human_bytes(bytes));
    if others > 0 && others.saturating_mul(10) >= total {
        text.push_str(&format!(" +{}", holders.len() - 1));
    }
    text
}

/// Where the user acts on a row (SPEC 5.4 `WHERE TO FIX`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    Cleanup,
    SnapshotReplication,
    RecycleBin,
    Shared,
    InspectExplain,
    Wait,
    WhyNotFreed,
    Doctor,
    /// In the package managing the data.
    Package(&'static Service),
}

impl Fix {
    /// From the largest share of a row's bytes: its reclaimable bytes
    /// (which win ties) or one of its `holders`, largest first.
    pub fn from_bytes(metrics: &Metrics, holders: &[(Holder, u64)]) -> Fix {
        match holders.first() {
            Some(&(holder, bytes)) if bytes > metrics.reclaimable => Fix::for_holder(holder),
            _ => Fix::Cleanup,
        }
    }

    /// For a ranked entry below `mount`: in the package whose data it
    /// is (`package`, or the service path it lies in), in the Recycle
    /// Bin it lies in, Snapshot Replication for a snapshot not read,
    /// `doctor` when its names cannot be read, else from its bytes.
    pub fn for_entry(entry: &Entry, mount: &[u8], package: Option<&'static Service>) -> Fix {
        if let Some(service) = package.or_else(|| crate::dsm::service_of(&entry.path, mount)) {
            return match service.code {
                "recycle_bin" => Fix::RecycleBin,
                _ => Fix::Package(service),
            };
        }
        if entry.flags.contains(NodeFlags::SNAPSHOT) {
            return Fix::SnapshotReplication;
        }
        if entry.flags.contains(NodeFlags::ENCRYPTED) {
            return Fix::Doctor;
        }
        let holders = held_by(&entry.retained(), entry.metrics.unknown);
        Fix::from_bytes(&entry.metrics, &holders)
    }

    fn for_holder(holder: Holder) -> Fix {
        match holder {
            Holder::Class(HeldClass::Snapshot) => Fix::SnapshotReplication,
            Holder::Class(HeldClass::RecycleBin) => Fix::RecycleBin,
            Holder::Class(HeldClass::HardLink) => Fix::InspectExplain,
            Holder::Class(HeldClass::Reflink) => Fix::Shared,
            Holder::Class(HeldClass::PendingCleaner) => Fix::Wait,
            Holder::Class(HeldClass::OpenFile) => Fix::WhyNotFreed,
            Holder::Unknown => Fix::Doctor,
        }
    }

    /// The JSON token.
    pub fn token(self) -> &'static str {
        match self {
            Fix::Cleanup => "cleanup",
            Fix::SnapshotReplication => "snapshot_replication",
            Fix::RecycleBin => "recycle_bin",
            Fix::Shared => "shared",
            Fix::InspectExplain => "inspect_explain",
            Fix::Wait => "wait",
            Fix::WhyNotFreed => "why_not_freed",
            Fix::Doctor => "doctor",
            Fix::Package(_) => "package",
        }
    }

    /// The short human text of the column.
    pub fn label(self) -> &'static str {
        match self {
            Fix::Cleanup => "cleanup",
            Fix::SnapshotReplication => "Snapshot Replication",
            Fix::RecycleBin => "Recycle Bin",
            Fix::Shared => "shared (see shared)",
            Fix::InspectExplain => "inspect --explain",
            Fix::Wait => "wait",
            Fix::WhyNotFreed => "why-not-freed",
            Fix::Doctor => "doctor",
            Fix::Package(service) => service.place(),
        }
    }

    /// The full DSM place to act in, when the fix is in DSM.
    pub fn manage_in(self) -> Option<&'static str> {
        match self {
            Fix::SnapshotReplication => Some(crate::dsm::SNAPSHOT_REPLICATION_SNAPSHOTS),
            Fix::RecycleBin => crate::dsm::service(b"#recycle", crate::dsm::Depth::Below)
                .map(|service| service.manage_in),
            Fix::Package(service) => Some(service.manage_in),
            _ => None,
        }
    }

    /// The stable id of that place (SPEC 4.6, GAP-14).
    pub fn manage_in_id(self) -> Option<&'static str> {
        match self {
            Fix::SnapshotReplication => Some(crate::dsm::place_id(
                crate::dsm::SNAPSHOT_REPLICATION_SNAPSHOTS,
            )),
            Fix::RecycleBin => crate::dsm::service(b"#recycle", crate::dsm::Depth::Below)
                .map(Service::manage_in_id),
            Fix::Package(service) => Some(service.manage_in_id()),
            _ => None,
        }
    }

    /// Fixed service-table context for package-managed paths.
    pub fn explanation(self) -> Option<&'static str> {
        match self {
            Fix::Package(service) => service.explanation,
            _ => None,
        }
    }

    /// The synoreclaim command that acts on or tells more about `path`,
    /// below the volume `mount` and a directory when `dir`; none for a
    /// path that is not text.
    pub fn command(self, path: &[u8], mount: &[u8], dir: bool) -> Option<Vec<String>> {
        let share;
        let text = std::str::from_utf8(path).ok()?;
        let args: &[&str] = match self {
            Fix::Cleanup => &["cleanup", "plan", text],
            Fix::Shared => &["shared", text],
            Fix::InspectExplain => &["inspect", text, "--explain"],
            Fix::Doctor => &["doctor", text],
            Fix::SnapshotReplication => {
                share = std::str::from_utf8(share_of(path, mount, dir)?).ok()?;
                &["snapshots", share]
            }
            Fix::RecycleBin => {
                share = std::str::from_utf8(share_of(path, mount, dir)?).ok()?;
                &["recycle", share]
            }
            Fix::WhyNotFreed => {
                share = std::str::from_utf8(mount).ok()?;
                &["why-not-freed", share, "--processes"]
            }
            _ => return None,
        };
        let argv = std::iter::once("synoreclaim").chain(args.iter().copied());
        Some(argv.map(str::to_string).collect())
    }

    /// The full hint `--explain` prints.
    pub fn hint(self) -> String {
        let text = match self {
            Fix::Cleanup => "mostly reclaimable: review its deletion with cleanup plan",
            Fix::SnapshotReplication => {
                "mostly held by snapshots: it is freed when they are deleted in Snapshot Replication > Snapshots"
            }
            Fix::RecycleBin => "in a Recycle Bin: empty it from the share's Recycle Bin",
            Fix::Shared => {
                "mostly shared with reflinked copies or other subvolumes: shared names what shares it"
            }
            Fix::InspectExplain => {
                "mostly held by hard links outside it: inspect --explain shows its blockers"
            }
            Fix::Wait => {
                "held by a deleted subvolume the Btrfs cleaner has not freed yet: the space returns by itself"
            }
            Fix::WhyNotFreed => {
                "held by deleted files still open: the space returns when their processes close them"
            }
            Fix::Doctor => {
                "its references or names could not be read: doctor explains the coverage gap"
            }
            Fix::Package(service) => return service.hint(),
        };
        text.to_string()
    }
}

/// The shared folder below `mount` holding `path` (a directory when
/// `dir`), or `mount` for the volume and the files at its root.
fn share_of<'p>(path: &'p [u8], mount: &'p [u8], dir: bool) -> Option<&'p [u8]> {
    let rest = path.strip_prefix(mount)?;
    if rest.is_empty() {
        return Some(mount);
    }
    let rest = rest.strip_prefix(b"/")?;
    match rest.iter().position(|&b| b == b'/') {
        Some(end) => Some(&path[..mount.len() + 1 + end]),
        None if dir => Some(path),
        None => Some(mount),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tree::NodeFlags;

    const GIB: u64 = 1 << 30;

    fn held(pairs: &[(HeldClass, u64)]) -> Held {
        let mut held = Held::default();
        for &(class, bytes) in pairs {
            held.add(&Held::of(class, bytes));
        }
        held
    }

    #[test]
    fn a_file_holds_its_retained_bytes_in_its_blockers_class() {
        let metrics = Metrics {
            physical: 5 * GIB,
            reclaimable: GIB,
            unknown: GIB,
            ..Metrics::default()
        };
        let blockers = Blockers::LIVE_REFLINK.with(Blockers::SNAPSHOT);
        let file = retained(FileKind::Regular, &metrics, blockers, Held::default());
        assert_eq!(file, Held::of(HeldClass::Snapshot, 3 * GIB));
        let dir_held = Held::of(HeldClass::Reflink, 2 * GIB);
        assert_eq!(
            retained(FileKind::Directory, &metrics, blockers, dir_held),
            dir_held
        );
    }

    #[test]
    fn holders_are_listed_largest_first_with_unknown_apart() {
        let h = held(&[(HeldClass::Reflink, GIB), (HeldClass::Snapshot, 3 * GIB)]);
        assert_eq!(
            held_by(&h, 2 * GIB),
            [
                (Holder::Class(HeldClass::Snapshot), 3 * GIB),
                (Holder::Unknown, 2 * GIB),
                (Holder::Class(HeldClass::Reflink), GIB),
            ]
        );
        assert_eq!(held_by(&Held::default(), 0), []);
    }

    #[test]
    fn the_text_names_the_largest_holder_and_counts_the_others_that_matter() {
        let text =
            |pairs: &[(HeldClass, u64)], unknown| held_by_text(&held_by(&held(pairs), unknown));
        assert_eq!(text(&[], 0), "—");
        assert_eq!(
            text(&[(HeldClass::Snapshot, 93 * GIB)], 0),
            "snapshot 93 GB"
        );
        assert_eq!(
            text(
                &[(HeldClass::Snapshot, 9 * GIB), (HeldClass::HardLink, GIB)],
                0
            ),
            "snapshot 9 GB +1"
        );
        assert_eq!(
            text(&[(HeldClass::Snapshot, 95 * GIB)], 4 * GIB),
            "snapshot 95 GB",
            "others under a tenth are left out"
        );
        assert_eq!(text(&[], GIB), "unknown 1 GB");
    }

    #[test]
    fn where_to_fix_follows_the_largest_share_of_the_bytes() {
        let fix = |reclaimable, pairs: &[(HeldClass, u64)], unknown| {
            let metrics = Metrics {
                reclaimable,
                ..Metrics::default()
            };
            Fix::from_bytes(&metrics, &held_by(&held(pairs), unknown)).token()
        };
        assert_eq!(fix(5 * GIB, &[(HeldClass::Snapshot, GIB)], 0), "cleanup");
        assert_eq!(
            fix(GIB, &[(HeldClass::Snapshot, GIB)], 0),
            "cleanup",
            "ties"
        );
        assert_eq!(fix(0, &[], 0), "cleanup");
        let cases = [
            (HeldClass::Snapshot, "snapshot_replication"),
            (HeldClass::RecycleBin, "recycle_bin"),
            (HeldClass::HardLink, "inspect_explain"),
            (HeldClass::Reflink, "shared"),
            (HeldClass::PendingCleaner, "wait"),
            (HeldClass::OpenFile, "why_not_freed"),
        ];
        for (class, token) in cases {
            assert_eq!(fix(GIB, &[(class, 2 * GIB)], 0), token);
        }
        assert_eq!(fix(0, &[(HeldClass::Reflink, GIB)], 2 * GIB), "doctor");
    }

    #[test]
    fn package_data_is_fixed_in_its_package() {
        let drive = crate::dsm::service(b"@synologydrive", crate::dsm::Depth::VolumeRoot).unwrap();
        let fix = Fix::Package(drive);
        assert_eq!(
            (fix.token(), fix.label()),
            ("package", "Synology Drive Admin Console")
        );
    }

    fn entry(path: &str, flags: NodeFlags, reclaimable: u64) -> Entry {
        Entry {
            path: path.as_bytes().to_vec(),
            kind: FileKind::Directory,
            flags,
            metrics: Metrics {
                physical: 4 * GIB,
                reclaimable,
                ..Metrics::default()
            },
            blockers: Blockers::default(),
            held: Held::of(HeldClass::Snapshot, 4 * GIB - reclaimable),
            nlink: 1,
            tree: 5,
            objectid: 300,
            outside: None,
        }
    }

    #[test]
    fn an_entry_is_fixed_where_its_data_is_managed_before_its_bytes_count() {
        let fix =
            |path, flags, package| Fix::for_entry(&entry(path, flags, 0), b"/v", package).token();
        let none = NodeFlags::default();
        assert_eq!(fix("/v/share/a", none, None), "snapshot_replication");
        assert_eq!(
            fix("/v/share/#recycle", NodeFlags::RECYCLE, None),
            "recycle_bin"
        );
        assert_eq!(
            fix("/v/share/#recycle/a", NodeFlags::RECYCLE, None),
            "recycle_bin"
        );
        assert_eq!(fix("/v/@synologydrive/x", none, None), "package");
        assert_eq!(fix("/v/share/secret", NodeFlags::ENCRYPTED, None), "doctor");
        let abb = crate::dsm::service(b"@ActiveBackup", crate::dsm::Depth::Below);
        assert_eq!(fix("/v/share/a", none, abb), "package");
        let cleanup = Fix::for_entry(&entry("/v/share/a", none, 4 * GIB), b"/v", None);
        assert_eq!(cleanup, Fix::Cleanup);
        let snapshot = entry("/v/snap", NodeFlags::SNAPSHOT, 4 * GIB);
        assert_eq!(
            Fix::for_entry(&snapshot, b"/v", None),
            Fix::SnapshotReplication,
            "a snapshot not read"
        );
    }

    #[test]
    fn each_fix_names_where_to_act_and_the_command_that_shows_more() {
        let command = |fix: Fix| fix.command(b"/v/a", b"/v", true).map(|argv| argv.join(" "));
        assert_eq!(
            command(Fix::Cleanup).unwrap(),
            "synoreclaim cleanup plan /v/a"
        );
        assert_eq!(command(Fix::Shared).unwrap(), "synoreclaim shared /v/a");
        assert_eq!(
            command(Fix::InspectExplain).unwrap(),
            "synoreclaim inspect /v/a --explain"
        );
        assert_eq!(command(Fix::Doctor).unwrap(), "synoreclaim doctor /v/a");
        assert_eq!(
            command(Fix::WhyNotFreed).unwrap(),
            "synoreclaim why-not-freed /v --processes",
            "the volume holding the path"
        );
        assert_eq!(
            Fix::Cleanup.command(b"/v/\xff", b"/v", true),
            None,
            "not text"
        );
        let snapshots = |path: &[u8]| {
            Fix::SnapshotReplication
                .command(path, b"/v", true)
                .map(|argv| argv.join(" "))
        };
        assert_eq!(
            snapshots(b"/v/share/vm/disk").unwrap(),
            "synoreclaim snapshots /v/share",
            "the share holding the path"
        );
        let recycle = Fix::RecycleBin.command(b"/v/share/#recycle", b"/v", true);
        assert_eq!(
            recycle.unwrap().join(" "),
            "synoreclaim recycle /v/share",
            "the share holding the Recycle Bin"
        );
        assert_eq!(snapshots(b"/v").unwrap(), "synoreclaim snapshots /v");
        assert_eq!(snapshots(b"/w/share"), None, "not below the volume");
        let file = Fix::SnapshotReplication.command(b"/v/big.iso", b"/v", false);
        assert_eq!(
            file.unwrap().join(" "),
            "synoreclaim snapshots /v",
            "a file at the volume root is in no share"
        );
        assert!(
            Fix::SnapshotReplication
                .manage_in()
                .unwrap()
                .starts_with("Snapshot Replication")
        );
        assert!(
            Fix::RecycleBin
                .manage_in()
                .unwrap()
                .contains("Empty Recycle Bin")
        );
        assert_eq!(Fix::Cleanup.manage_in(), None);
        for fix in [
            Fix::Cleanup,
            Fix::SnapshotReplication,
            Fix::RecycleBin,
            Fix::Shared,
            Fix::InspectExplain,
            Fix::Wait,
            Fix::WhyNotFreed,
            Fix::Doctor,
        ] {
            assert!(!fix.hint().is_empty(), "{fix:?}");
        }
    }
}
