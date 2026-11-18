//! Reclaimability of one physical extent (SPEC 8.1, 10): whether deleting
//! every selected name frees it, which blockers retain it, or whether
//! its references cannot be resolved.

use crate::btrfs::backref::DataBackref;
use crate::btrfs::extent_refs::ExtentRefs;
use crate::btrfs::leaf_ranges::FileRange;
use crate::model::extent::ExtentUse;

/// Why selected data stays allocated (SPEC 3 "External blockers").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Blockers(u8);

impl Blockers {
    pub const SNAPSHOT: Blockers = Blockers(1);
    pub const LIVE_REFLINK: Blockers = Blockers(1 << 1);
    pub const HARD_LINK: Blockers = Blockers(1 << 2);
    pub const OTHER_SUBVOLUME: Blockers = Blockers(1 << 3);
    /// Held by a tree block shared through snapshotting (conservative).
    pub const SHARED_TREE: Blockers = Blockers(1 << 4);
    pub const PENDING_DELETION: Blockers = Blockers(1 << 5);
    /// An inode without names, such as an unlinked-but-open file.
    pub const ORPHAN_INODE: Blockers = Blockers(1 << 6);
    pub const UNKNOWN: Blockers = Blockers(1 << 7);
    const NAMES: [&'static str; 8] = [
        "snapshot",
        "live_reflink",
        "hard_link",
        "other_subvolume",
        "shared_tree",
        "pending_subvolume_deletion",
        "orphan_inode",
        "unknown",
    ];

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn from_bits(bits: u8) -> Self {
        Blockers(bits)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn with(self, other: Blockers) -> Self {
        Blockers(self.0 | other.0)
    }

    pub fn names(self) -> Vec<&'static str> {
        (0..8)
            .filter(|i| self.0 & (1 << i) != 0)
            .map(|i| Self::NAMES[i])
            .collect()
    }
}

/// Who holds bytes that deleting a subtree would not free (SPEC 5.4
/// `HELD BY`). Unknown bytes are `Metrics::unknown`, never a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldClass {
    Snapshot,
    RecycleBin,
    HardLink,
    Reflink,
    PendingCleaner,
    OpenFile,
}

impl HeldClass {
    pub const ALL: [HeldClass; 6] = [
        HeldClass::Snapshot,
        HeldClass::RecycleBin,
        HeldClass::HardLink,
        HeldClass::Reflink,
        HeldClass::PendingCleaner,
        HeldClass::OpenFile,
    ];

    pub fn name(self) -> &'static str {
        match self {
            HeldClass::Snapshot => "snapshot",
            HeldClass::RecycleBin => "recycle_bin",
            HeldClass::HardLink => "hard_link",
            HeldClass::Reflink => "reflink",
            HeldClass::PendingCleaner => "pending_cleaner",
            HeldClass::OpenFile => "open_file",
        }
    }

    /// The one class an extent retained by `blockers` counts in, the
    /// first by this order: snapshot, pending cleaner, open file, hard
    /// link, reflink. `None` when only unknown references hold it.
    pub fn of(blockers: Blockers) -> Option<HeldClass> {
        let order = [
            (
                Blockers::SNAPSHOT.with(Blockers::SHARED_TREE),
                HeldClass::Snapshot,
            ),
            (Blockers::PENDING_DELETION, HeldClass::PendingCleaner),
            (Blockers::ORPHAN_INODE, HeldClass::OpenFile),
            (Blockers::HARD_LINK, HeldClass::HardLink),
            (
                Blockers::LIVE_REFLINK.with(Blockers::OTHER_SUBVOLUME),
                HeldClass::Reflink,
            ),
        ];
        order
            .iter()
            .find(|(bits, _)| blockers.bits() & bits.bits() != 0)
            .map(|(_, class)| *class)
    }
}

/// Retained bytes per [`HeldClass`], indexed like `HeldClass::ALL`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held(pub [u64; 6]);

impl Held {
    pub fn of(class: HeldClass, bytes: u64) -> Self {
        let mut held = Held::default();
        held.0[class as usize] = bytes;
        held
    }

    pub fn get(&self, class: HeldClass) -> u64 {
        self.0[class as usize]
    }

    pub fn add(&mut self, other: &Held) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a = a.wrapping_add(b);
        }
    }

    pub fn sub(&mut self, other: &Held) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a = a.wrapping_sub(b);
        }
    }
}

/// One selected file extent item referencing the extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Holder {
    pub tree: u64,
    pub objectid: u64,
    /// The inode keeps a name outside the selection.
    pub external_names: bool,
    /// The item's leaf may be shared with a snapshot.
    pub shared_leaf: bool,
}

/// What an extent's references outside the selection mean.
pub trait RootClass {
    /// The blocker a direct reference from tree `root` represents.
    fn external_root(&self, root: u64) -> Blockers;
    /// The blocker a possibly shared leaf of tree `tree` represents.
    fn shared_leaf(&self, tree: u64) -> Blockers;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every reference goes away with the selection.
    Reclaimable,
    Retained(Blockers),
    /// The reference set cannot be proven complete.
    Unknown,
}

/// One selected use of an extent with what classifying it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldUse {
    pub(crate) extent: ExtentUse,
    pub(crate) holder: Holder,
    /// The inode has several selected names.
    pub(crate) several_names: bool,
}

/// What the selection means for one extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Judgement {
    pub disk_bytes: u64,
    /// `None` when references are not classified.
    pub verdict: Option<Verdict>,
    /// Blockers of the holders below the owning subtree of a reclaimable
    /// extent.
    pub local: Blockers,
    /// Bytes of a reclaimable extent no use references.
    pub pinned: u64,
}

/// Judges one extent from all its selected uses, which must be non-empty.
pub(crate) fn judge(
    uses: &[HeldUse],
    refs: Option<&ExtentRefs>,
    roots: Option<&dyn RootClass>,
) -> Judgement {
    let disk_bytes = uses.iter().map(|u| u.extent.disk_bytes).max().unwrap_or(0);
    let mut judgement = Judgement {
        disk_bytes,
        verdict: None,
        local: Blockers::default(),
        pinned: 0,
    };
    let Some(roots) = roots else {
        return judgement;
    };
    let holders: Vec<Holder> = uses.iter().map(|u| u.holder).collect();
    let verdict = classify(disk_bytes, &holders, refs, roots);
    judgement.verdict = Some(verdict);
    if verdict == Verdict::Reclaimable {
        judgement.local = local_blockers(uses);
        let extents: Vec<ExtentUse> = uses.iter().map(|u| u.extent).collect();
        judgement.pinned = pinned_bytes(&extents, disk_bytes).unwrap_or(0);
    }
    judgement
}

/// Links inside the selection that deleting only part of it keeps.
fn local_blockers(uses: &[HeldUse]) -> Blockers {
    let first = &uses[0].holder;
    let mut local = Blockers::default();
    if uses
        .iter()
        .any(|u| (u.holder.tree, u.holder.objectid) != (first.tree, first.objectid))
    {
        local = local.with(Blockers::LIVE_REFLINK);
    }
    if uses.iter().any(|u| u.several_names) {
        local = local.with(Blockers::HARD_LINK);
    }
    local
}

/// Bytes of an uncompressed extent that no use's range covers; zero
/// when that cannot be computed.
pub(crate) fn pinned_bytes(group: &[ExtentUse], disk_bytes: u64) -> Option<u64> {
    unreferenced_file_bytes(group.iter().map(ExtentUse::file_range), disk_bytes)
}

/// Bytes of an extent that no range covers; zero once an opaque range
/// shows that cannot be computed.
pub(crate) fn unreferenced_file_bytes(
    ranges: impl IntoIterator<Item = FileRange>,
    disk_bytes: u64,
) -> Option<u64> {
    let mut ranges: Vec<FileRange> = ranges.into_iter().collect();
    if ranges.iter().any(|r| r.opaque) {
        return Some(0);
    }
    ranges.sort_unstable();
    crate::model::extent::unreferenced_range_bytes_sorted(
        ranges.iter().map(|r| (r.start, r.end)),
        disk_bytes,
    )
}

/// Per-scope accounting totals (SPEC 5.6 blocker summary).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountingSummary {
    pub extents: u64,
    pub reclaimable_extents: u64,
    pub retained_extents: u64,
    pub unknown_extents: u64,
    /// Retained bytes per blocker bit; an extent counts in each class.
    pub class_bytes: [u64; 8],
}

impl AccountingSummary {
    /// Retained bytes of the classes in `blockers`, summed per class.
    pub fn blocker_bytes(&self, blockers: Blockers) -> u64 {
        (0..8)
            .filter(|i| blockers.bits() & (1 << i) != 0)
            .map(|i| self.class_bytes[i])
            .sum()
    }

    pub fn count(&mut self, judgement: &Judgement) {
        self.extents += 1;
        match judgement.verdict {
            None => {}
            Some(Verdict::Reclaimable) => self.reclaimable_extents += 1,
            Some(Verdict::Retained(blockers)) => {
                self.retained_extents += 1;
                for i in 0..8 {
                    if blockers.bits() & (1 << i) != 0 {
                        self.class_bytes[i] += judgement.disk_bytes;
                    }
                }
            }
            Some(Verdict::Unknown) => self.unknown_extents += 1,
        }
    }
}

/// Judges an extent of `disk_bytes` whose selected file extent items are
/// `holders` against its extent-tree references.
pub fn classify(
    disk_bytes: u64,
    holders: &[Holder],
    refs: Option<&ExtentRefs>,
    roots: &dyn RootClass,
) -> Verdict {
    let mut blockers = Blockers::default();
    for h in holders {
        if h.external_names {
            blockers = blockers.with(Blockers::HARD_LINK);
        }
        if h.shared_leaf {
            blockers = blockers.with(roots.shared_leaf(h.tree));
        }
    }
    let Some(refs) = refs else {
        return retained_or_unknown(blockers);
    };
    let mut declared = 0u64;
    for backref in &refs.backrefs {
        declared += u64::from(backref.count());
        match *backref {
            DataBackref::Shared { .. } => blockers = blockers.with(Blockers::SHARED_TREE),
            DataBackref::Direct { root, objectid, .. } => {
                if !holders
                    .iter()
                    .any(|h| h.tree == root && h.objectid == objectid)
                {
                    blockers = blockers.with(roots.external_root(root));
                }
            }
        }
    }
    if blockers == Blockers::UNKNOWN {
        return Verdict::Unknown;
    }
    if !blockers.is_empty() {
        return Verdict::Retained(blockers);
    }
    let complete = !refs.malformed
        && refs.num_bytes == disk_bytes
        && declared == refs.refs
        && holders
            .iter()
            .all(|h| direct_count(refs, h) == item_count(holders, h));
    if complete {
        Verdict::Reclaimable
    } else {
        Verdict::Unknown
    }
}

/// References the backrefs attribute to `h`'s inode.
fn direct_count(refs: &ExtentRefs, h: &Holder) -> u64 {
    refs.backrefs
        .iter()
        .filter_map(|b| match *b {
            DataBackref::Direct {
                root,
                objectid,
                count,
                ..
            } if root == h.tree && objectid == h.objectid => Some(u64::from(count)),
            _ => None,
        })
        .sum()
}

/// Selected file extent items of `h`'s inode.
fn item_count(holders: &[Holder], h: &Holder) -> u64 {
    holders
        .iter()
        .filter(|o| o.tree == h.tree && o.objectid == h.objectid)
        .count() as u64
}

fn retained_or_unknown(blockers: Blockers) -> Verdict {
    if blockers.is_empty() {
        Verdict::Unknown
    } else {
        Verdict::Retained(blockers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: u64 = 4096;

    struct Roots;

    impl RootClass for Roots {
        fn external_root(&self, root: u64) -> Blockers {
            match root {
                257 => Blockers::LIVE_REFLINK,
                300 => Blockers::SNAPSHOT,
                301 => Blockers::PENDING_DELETION,
                302 => Blockers::UNKNOWN,
                _ => Blockers::OTHER_SUBVOLUME,
            }
        }

        fn shared_leaf(&self, _tree: u64) -> Blockers {
            Blockers::SNAPSHOT
        }
    }

    fn holder(objectid: u64) -> Holder {
        Holder {
            tree: 257,
            objectid,
            external_names: false,
            shared_leaf: false,
        }
    }

    #[test]
    fn pinned_extent_ranges_return_a_known_once_only_unreferenced_count() {
        let uses = [ExtentUse {
            bytenr: 10,
            inode: 256,
            start: 10,
            end: 40,
            flags: 0,
            disk_bytes: 100,
        }];
        assert_eq!(pinned_bytes(&uses, 100), Some(70));
    }

    fn direct(root: u64, objectid: u64, count: u32) -> DataBackref {
        DataBackref::Direct {
            root,
            objectid,
            offset: 0,
            count,
        }
    }

    fn refs(refs: u64, backrefs: Vec<DataBackref>) -> ExtentRefs {
        ExtentRefs {
            num_bytes: LEN,
            refs,
            generation: 1,
            backrefs,
            malformed: false,
        }
    }

    fn verdict(holders: &[Holder], refs: Option<&ExtentRefs>) -> Verdict {
        classify(LEN, holders, refs, &Roots)
    }

    #[test]
    fn all_references_internal_is_reclaimable() {
        let r = refs(3, vec![direct(257, 300, 1), direct(257, 301, 2)]);
        let holders = [holder(300), holder(301), holder(301)];
        assert_eq!(verdict(&holders, Some(&r)), Verdict::Reclaimable);
    }

    /// A file referencing one extent at two file offsets has one backref
    /// per `file_offset - extent_offset`.
    #[test]
    fn backrefs_of_one_inode_at_several_offsets_add_up() {
        let at = |offset| DataBackref::Direct {
            root: 257,
            objectid: 300,
            offset,
            count: 1,
        };
        let r = refs(2, vec![at(0), at(8192)]);
        assert_eq!(
            verdict(&[holder(300), holder(300)], Some(&r)),
            Verdict::Reclaimable
        );
    }

    #[test]
    fn a_reference_from_outside_the_selection_retains_the_extent() {
        let r = refs(2, vec![direct(257, 300, 1), direct(257, 999, 1)]);
        assert_eq!(
            verdict(&[holder(300)], Some(&r)),
            Verdict::Retained(Blockers::LIVE_REFLINK)
        );
        let r = refs(2, vec![direct(257, 300, 1), direct(300, 300, 1)]);
        assert_eq!(
            verdict(&[holder(300)], Some(&r)),
            Verdict::Retained(Blockers::SNAPSHOT)
        );
    }

    #[test]
    fn shared_tree_blocks_hard_links_and_shared_leaves_retain() {
        let shared = DataBackref::Shared {
            parent: 1 << 30,
            count: 1,
        };
        let r = refs(2, vec![direct(257, 300, 1), shared]);
        assert_eq!(
            verdict(&[holder(300)], Some(&r)),
            Verdict::Retained(Blockers::SHARED_TREE)
        );
        let r = refs(1, vec![direct(257, 300, 1)]);
        let linked = Holder {
            external_names: true,
            ..holder(300)
        };
        assert_eq!(
            verdict(&[linked], Some(&r)),
            Verdict::Retained(Blockers::HARD_LINK)
        );
        let old = Holder {
            shared_leaf: true,
            ..holder(300)
        };
        assert_eq!(
            verdict(&[old], Some(&r)),
            Verdict::Retained(Blockers::SNAPSHOT)
        );
    }

    #[test]
    fn blockers_accumulate() {
        let r = refs(
            3,
            vec![direct(301, 5, 1), direct(257, 9, 1), direct(257, 300, 1)],
        );
        assert_eq!(
            verdict(&[holder(300)], Some(&r)),
            Verdict::Retained(Blockers::PENDING_DELETION.with(Blockers::LIVE_REFLINK))
        );
    }

    #[test]
    fn unresolved_references_are_unknown_never_reclaimable() {
        let holders = [holder(300)];
        assert_eq!(verdict(&holders, None), Verdict::Unknown);
        // The declared total exceeds the backrefs found.
        let r = refs(2, vec![direct(257, 300, 1)]);
        assert_eq!(verdict(&holders, Some(&r)), Verdict::Unknown);
        let malformed = ExtentRefs {
            malformed: true,
            ..refs(1, vec![direct(257, 300, 1)])
        };
        assert_eq!(verdict(&holders, Some(&malformed)), Verdict::Unknown);
        let other_length = ExtentRefs {
            num_bytes: 2 * LEN,
            ..refs(1, vec![direct(257, 300, 1)])
        };
        assert_eq!(verdict(&holders, Some(&other_length)), Verdict::Unknown);
        // Fewer file extent items than the backref counts.
        let r = refs(2, vec![direct(257, 300, 2)]);
        assert_eq!(verdict(&holders, Some(&r)), Verdict::Unknown);
    }

    /// SPEC 3: unresolved ownership is unknown, not proven sharing.
    #[test]
    fn a_reference_only_from_an_unresolved_root_is_unknown() {
        let r = refs(2, vec![direct(257, 300, 1), direct(302, 5, 1)]);
        assert_eq!(verdict(&[holder(300)], Some(&r)), Verdict::Unknown);
        let r = refs(
            3,
            vec![direct(257, 300, 1), direct(302, 5, 1), direct(300, 5, 1)],
        );
        assert_eq!(
            verdict(&[holder(300)], Some(&r)),
            Verdict::Retained(Blockers::UNKNOWN.with(Blockers::SNAPSHOT))
        );
    }

    #[test]
    fn blockers_have_stable_names() {
        let all = Blockers::from_bits(0xff);
        assert_eq!(
            all.names(),
            vec![
                "snapshot",
                "live_reflink",
                "hard_link",
                "other_subvolume",
                "shared_tree",
                "pending_subvolume_deletion",
                "orphan_inode",
                "unknown",
            ]
        );
        assert!(Blockers::default().is_empty());
    }
}
