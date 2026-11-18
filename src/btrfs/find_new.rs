//! What changed in a subvolume tree after a transaction (what `btrfs
//! subvolume find-new` searches), and where a changed inode is linked.

use crate::btrfs::keys::{
    FIRST_FREE_OBJECTID, INODE_EXTREF_KEY, INODE_ITEM_KEY, INODE_REF_KEY, Key, LAST_FREE_OBJECTID,
};
use crate::btrfs::parser::{parse_inode_extref_parents, parse_inode_item};
use crate::btrfs::search::SearchRange;
use crate::btrfs::source::MetadataSource;
use crate::btrfs::walk::{BatchBoundary, WalkError, walk_tree};
use std::collections::BTreeSet;

/// The inodes of `tree` whose inode item was written after transaction
/// `since`. Btrfs rewrites an inode's item, with the writing
/// transaction's id, whenever its data, its attributes or, for a
/// directory, its entries change; a deleted inode leaves its parent
/// directory changed. Only tree blocks written after `since` are read.
/// More than `limit` changed inodes is a capacity error.
pub fn changed_inodes<S: MetadataSource + ?Sized>(
    source: &mut S,
    tree: u64,
    since: u64,
    limit: usize,
    boundary: &mut dyn BatchBoundary,
) -> Result<BTreeSet<u64>, WalkError> {
    let range = SearchRange::new(
        tree,
        Key::new(FIRST_FREE_OBJECTID, INODE_ITEM_KEY, 0),
        Key::new(LAST_FREE_OBJECTID, u8::MAX, u64::MAX),
    )
    .since(since.saturating_add(1));
    let mut changed = BTreeSet::new();
    walk_tree(source, range, boundary, |item| {
        if item.key.item_type != INODE_ITEM_KEY {
            return Ok(());
        }
        let inode = parse_inode_item(item.data).map_err(|source| WalkError::Item {
            key: item.key,
            source,
        })?;
        if inode.transid > since {
            changed.insert(item.key.objectid);
        }
        if changed.len() > limit {
            return Err(WalkError::Capacity("changed inodes"));
        }
        Ok(())
    })?;
    Ok(changed)
}

/// The directories `ino` of `tree` is linked in, from its `INODE_REF`
/// and `INODE_EXTREF` items; empty when it has none.
pub fn parents<S: MetadataSource + ?Sized>(
    source: &mut S,
    tree: u64,
    ino: u64,
    boundary: &mut dyn BatchBoundary,
) -> Result<BTreeSet<u64>, WalkError> {
    let range = SearchRange::new(
        tree,
        Key::new(ino, INODE_REF_KEY, 0),
        Key::new(ino, INODE_EXTREF_KEY, u64::MAX),
    );
    let mut parents = BTreeSet::new();
    walk_tree(source, range, boundary, |item| {
        match item.key.item_type {
            INODE_REF_KEY => {
                parents.insert(item.key.offset);
            }
            INODE_EXTREF_KEY => {
                let found =
                    parse_inode_extref_parents(item.data).map_err(|source| WalkError::Item {
                        key: item.key,
                        source,
                    })?;
                parents.extend(found);
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(parents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::walk::testing::{FakeSource, NoStop};
    use crate::model::testing::*;

    const TREE: u64 = 256;

    /// Items in leaves written by transaction 20, except those of inodes
    /// 300 and above, which transaction 25 rewrote.
    fn tree() -> FakeSource {
        let mut source = FakeSource::new(
            vec![
                inode_written(256, DIR, 20),
                inode_ref(256, 256, ".."),
                inode_written(257, FILE, 12),
                whole_extent(257, 0, 1 << 20, 4096),
                inode_written(300, FILE, 20),
                inode_written(301, FILE, 25),
                inode_ref(301, 256, "new"),
            ],
            2,
        );
        source.leaf_generation = |key| if key.objectid >= 300 { 25 } else { 20 };
        source
    }

    #[test]
    fn the_inodes_written_after_a_transaction_changed() {
        let mut source = tree();

        let changed = changed_inodes(&mut source, TREE, 20, 10, &mut NoStop).unwrap();

        assert_eq!(changed, BTreeSet::from([301]));
        assert!(source.calls.iter().all(|range| range.min_transid == 21));
        assert!(source.calls.iter().all(|range| range.tree_id == TREE));
    }

    #[test]
    fn an_older_transaction_finds_the_older_changes_too() {
        let changed = changed_inodes(&mut tree(), TREE, 11, 10, &mut NoStop).unwrap();

        assert_eq!(changed, BTreeSet::from([256, 257, 300, 301]));
    }

    #[test]
    fn more_changed_inodes_than_the_limit_is_a_capacity_error() {
        let result = changed_inodes(&mut tree(), TREE, 11, 3, &mut NoStop);

        assert!(matches!(result, Err(WalkError::Capacity(_))), "{result:?}");
    }

    #[test]
    fn an_inode_lies_in_every_directory_its_refs_name() {
        let mut source = FakeSource::new(
            vec![
                inode_written(400, FILE, 1),
                inode_ref(400, 300, "a"),
                inode_ref(400, 310, "b"),
                inode_extref(400, 7, &[320, 330]),
                inode_ref(401, 999, "other"),
            ],
            2,
        );

        let found = parents(&mut source, TREE, 400, &mut NoStop).unwrap();

        assert_eq!(found, BTreeSet::from([300, 310, 320, 330]));
    }

    #[test]
    fn an_unlinked_inode_has_no_parents() {
        let mut source = FakeSource::new(vec![inode_written(400, FILE, 1)], 2);

        assert!(
            parents(&mut source, TREE, 400, &mut NoStop)
                .unwrap()
                .is_empty()
        );
    }
}
