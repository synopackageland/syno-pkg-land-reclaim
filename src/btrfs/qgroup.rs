//! The quota tree (tree 8), read only: status, usage and limits for
//! level-0 qgroups. Nothing here enables, rescans or changes quotas.

use crate::btrfs::bytes::LeReader;
use crate::btrfs::keys::{Key, QUOTA_TREE_OBJECTID};
use crate::btrfs::parser::ItemError;
use crate::btrfs::search::{Item, SearchRange};
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_tree};
use std::collections::BTreeMap;

pub const QGROUP_STATUS_KEY: u8 = 240;
pub const QGROUP_INFO_KEY: u8 = 242;
pub const QGROUP_LIMIT_KEY: u8 = 244;
pub const QGROUP_STATUS_FLAG_ON: u64 = 1;
pub const QGROUP_STATUS_FLAG_RESCAN: u64 = 1 << 1;
pub const QGROUP_STATUS_FLAG_INCONSISTENT: u64 = 1 << 2;
pub const QGROUP_LIMIT_MAX_RFER: u64 = 1;
/// A qgroup id holds its level above this many bits.
const QGROUP_LEVEL_SHIFT: u32 = 48;

/// What the quota tree says about level-0 qgroups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Qgroups {
    /// Status flags; 0 without a status item.
    pub flags: u64,
    /// Referenced bytes of `0/<subvolid>`, by subvolume ID.
    pub referenced: BTreeMap<u64, u64>,
    /// Exclusive bytes of qgroup `0/<subvolid>`, by subvolume id.
    pub exclusive: BTreeMap<u64, u64>,
    /// Maximum referenced bytes, only when the qgroup has that limit.
    reference_limits: BTreeMap<u64, u64>,
}

/// A share's current referenced bytes and its hard referenced-byte limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareQuota {
    pub referenced_bytes: u64,
    pub limit_bytes: u64,
}

impl ShareQuota {
    pub fn available_bytes(self) -> u64 {
        self.limit_bytes.saturating_sub(self.referenced_bytes)
    }
}

impl Qgroups {
    pub fn enabled(&self) -> bool {
        self.flags & QGROUP_STATUS_FLAG_ON != 0
    }

    /// Quotas are on, no rescan runs and the kernel does not flag the
    /// numbers as inconsistent: only then are they worth showing.
    pub fn consistent(&self) -> bool {
        self.flags & QGROUP_STATUS_FLAG_ON != 0
            && self.flags & (QGROUP_STATUS_FLAG_RESCAN | QGROUP_STATUS_FLAG_INCONSISTENT) == 0
    }

    /// A share quota, when the qgroup has a max referenced-byte limit.
    pub fn share_quota(&self, subvolume_id: u64) -> Option<ShareQuota> {
        Some(ShareQuota {
            referenced_bytes: *self.referenced.get(&subvolume_id)?,
            limit_bytes: *self.reference_limits.get(&subvolume_id)?,
        })
    }

    pub fn has_share_quota(&self, subvolume_id: u64) -> bool {
        self.reference_limits.contains_key(&subvolume_id)
    }

    fn add(&mut self, item: Item<'_>) -> Result<(), ItemError> {
        let mut r = LeReader::new(item.data);
        match item.key.item_type {
            QGROUP_STATUS_KEY => {
                let _version = r.u64()?;
                let _generation = r.u64()?;
                self.flags = r.u64()?;
            }
            QGROUP_INFO_KEY if item.key.offset >> QGROUP_LEVEL_SHIFT == 0 => {
                let _generation = r.u64()?;
                let referenced = r.u64()?;
                let _referenced_compressed = r.u64()?;
                self.exclusive.insert(item.key.offset, r.u64()?);
                self.referenced.insert(item.key.offset, referenced);
            }
            QGROUP_LIMIT_KEY if item.key.offset >> QGROUP_LEVEL_SHIFT == 0 => {
                let flags = r.u64()?;
                let max_referenced = r.u64()?;
                if flags & QGROUP_LIMIT_MAX_RFER != 0 {
                    self.reference_limits
                        .insert(item.key.offset, max_referenced);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Reads quota status, level-0 qgroup infos and their optional limits.
pub fn read_qgroups<S: MetadataSource + ?Sized>(
    source: &mut S,
    boundary: &mut dyn BatchBoundary,
) -> Result<Qgroups, WalkError> {
    let range = SearchRange::new(
        QUOTA_TREE_OBJECTID,
        Key::new(0, QGROUP_STATUS_KEY, 0),
        Key::new(0, QGROUP_LIMIT_KEY, u64::MAX),
    );
    let mut qgroups = Qgroups::default();
    let mut saw_item = false;
    let walked = walk_tree(source, range, boundary, |item| {
        saw_item = true;
        let key = item.key;
        qgroups
            .add(item)
            .map_err(|source| WalkError::Item { key, source })
    });
    // With quotas disabled, Btrfs may have no quota tree at all. Only an
    // initial ENOENT means absent; a tree lost after reading, malformed
    // metadata, and other I/O failures must still fail quota checks.
    match walked {
        Err(WalkError::Source(crate::btrfs::source::SourceError::Io(error)))
            if !saw_item && error.raw_os_error() == Some(libc::ENOENT) => {}
        other => {
            other?;
        }
    }
    Ok(qgroups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};

    fn words(values: &[u64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn status(flags: u64) -> (Key, Vec<u8>) {
        (Key::new(0, QGROUP_STATUS_KEY, 0), words(&[1, 9, flags, 0]))
    }

    fn info(qgroup: u64, exclusive: u64) -> (Key, Vec<u8>) {
        (
            Key::new(0, QGROUP_INFO_KEY, qgroup),
            words(&[9, 3 * exclusive, 3 * exclusive, exclusive, exclusive]),
        )
    }

    fn reference_limit(qgroup: u64, limit: u64) -> (Key, Vec<u8>) {
        (
            Key::new(0, QGROUP_LIMIT_KEY, qgroup),
            words(&[QGROUP_LIMIT_MAX_RFER, limit, 0, 0, 0]),
        )
    }

    fn read(items: Vec<(Key, Vec<u8>)>) -> Qgroups {
        read_qgroups(&mut FakeSource::new(items, 2), &mut NoStop).unwrap()
    }

    #[test]
    fn reads_the_exclusive_bytes_of_level_zero_qgroups() {
        let q = read(vec![
            status(QGROUP_STATUS_FLAG_ON),
            info(257, 4096),
            info(258, 8192),
            info((1 << 48) | 100, 1),
        ]);
        assert!(q.consistent());
        assert_eq!(q.exclusive, BTreeMap::from([(257, 4096), (258, 8192)]));
    }

    #[test]
    fn reads_a_share_qgroups_reference_usage_and_limit() {
        let q = read(vec![
            status(QGROUP_STATUS_FLAG_ON),
            (
                Key::new(0, QGROUP_INFO_KEY, 257),
                words(&[9, 7_000, 7_000, 5_000, 5_000]),
            ),
            reference_limit(257, 10_000),
        ]);

        assert_eq!(
            q.share_quota(257),
            Some(ShareQuota {
                referenced_bytes: 7_000,
                limit_bytes: 10_000,
            })
        );
        assert_eq!(q.share_quota(258), None);
    }

    #[test]
    fn numbers_are_consistent_only_when_on_without_rescan_or_inconsistency() {
        let flags = |f| Qgroups {
            flags: f,
            ..Qgroups::default()
        };
        assert!(!flags(0).consistent());
        assert!(!flags(QGROUP_STATUS_FLAG_ON | QGROUP_STATUS_FLAG_RESCAN).consistent());
        assert!(!flags(QGROUP_STATUS_FLAG_ON | QGROUP_STATUS_FLAG_INCONSISTENT).consistent());
        assert!(read(vec![info(257, 1)]).exclusive.contains_key(&257));
        assert!(!read(vec![info(257, 1)]).consistent());
    }

    #[test]
    fn a_truncated_item_is_an_error() {
        let items = vec![(Key::new(0, QGROUP_INFO_KEY, 257), vec![0; 20])];
        let err = read_qgroups(&mut FakeSource::new(items, 2), &mut NoStop).unwrap_err();
        assert!(matches!(err, WalkError::Item { .. }));
    }

    struct FailingSource {
        first: Option<FakeSource>,
        errno: i32,
    }

    impl MetadataSource for FailingSource {
        fn search(
            &mut self,
            range: &SearchRange,
        ) -> Result<crate::btrfs::source::RawBatch<'_>, crate::btrfs::source::SourceError> {
            assert_eq!(range.tree_id, QUOTA_TREE_OBJECTID);
            if self
                .first
                .as_ref()
                .is_some_and(|source| source.calls.is_empty())
            {
                return self.first.as_mut().unwrap().search(range);
            }
            Err(std::io::Error::from_raw_os_error(self.errno).into())
        }
    }

    #[test]
    fn an_absent_quota_tree_means_quotas_are_disabled() {
        let mut source = FailingSource {
            first: None,
            errno: libc::ENOENT,
        };
        let groups = read_qgroups(&mut source, &mut NoStop).unwrap();
        assert_eq!(groups, Qgroups::default());
        assert!(!groups.enabled());
        assert!(!groups.has_share_quota(257));
    }

    #[test]
    fn quota_io_and_permission_errors_are_not_treated_as_disabled() {
        for errno in [libc::EIO, libc::EPERM, libc::EACCES] {
            let mut source = FailingSource { first: None, errno };
            assert!(read_qgroups(&mut source, &mut NoStop).is_err());
        }
    }

    #[test]
    fn a_quota_tree_that_disappears_after_reading_items_is_an_error() {
        let mut source = FailingSource {
            first: Some(FakeSource::new(vec![status(QGROUP_STATUS_FLAG_ON)], 1)),
            errno: libc::ENOENT,
        };
        assert!(read_qgroups(&mut source, &mut NoStop).is_err());
    }
}
