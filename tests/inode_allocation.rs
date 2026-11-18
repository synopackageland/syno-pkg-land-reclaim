use syno_reclaim::model::accounting::{Blockers, HeldClass};
use syno_reclaim::model::namespace::FileKind;
use syno_reclaim::model::tree::{InodeAllocation, InodeTree, VIRTUAL_ROOT};

fn allocation(ino: u64, links: u32, bytes: u64) -> InodeAllocation {
    InodeAllocation {
        device: 7,
        inode: ino,
        kind: FileKind::Regular,
        logical_bytes: 8 << 30,
        allocated_bytes: bytes,
        links,
        ctime: 1,
        uncertain: false,
    }
}

#[test]
fn inode_allocations_share_existing_hard_link_accounting() {
    let mut tree = InodeTree::default();
    let a = tree.add_inode_allocation(VIRTUAL_ROOT, b"/a", allocation(1, 2, 5 << 30), false);
    let b = tree.add_inode_allocation(VIRTUAL_ROOT, b"/b", allocation(1, 2, 5 << 30), false);
    let totals = tree.account_inode_allocations();
    assert_eq!(totals.metrics[VIRTUAL_ROOT as usize].physical, 5 << 30);
    assert_eq!(totals.metrics[VIRTUAL_ROOT as usize].reclaimable, 5 << 30);
    assert_eq!(totals.metrics[a as usize].reclaimable, 0);
    assert_eq!(totals.metrics[b as usize].physical, 5 << 30);
    assert!(totals.blockers[a as usize].bits() & Blockers::HARD_LINK.bits() != 0);
}

#[test]
fn unseen_links_and_changed_inodes_are_conservative() {
    let mut tree = InodeTree::default();
    let a = tree.add_inode_allocation(VIRTUAL_ROOT, b"/a", allocation(1, 2, 4096), false);
    let mut changed = allocation(2, 1, 8192);
    changed.uncertain = true;
    tree.add_inode_allocation(VIRTUAL_ROOT, b"/changed", changed, false);
    let totals = tree.account_inode_allocations();
    assert_eq!(totals.metrics[0].reclaimable, 0);
    assert_eq!(totals.metrics[0].unknown, 8192);
    assert!(totals.external[a as usize].bits() & Blockers::HARD_LINK.bits() != 0);
}

#[test]
fn directory_link_count_is_not_a_file_hard_link_count() {
    let mut tree = InodeTree::default();
    let mut root = allocation(2, 12, 4096);
    root.kind = FileKind::Directory;
    let root = tree.add_inode_allocation(VIRTUAL_ROOT, b"/volume", root, true);
    tree.add_inode_allocation(root, b"sparse", allocation(3, 1, 0), false);
    let totals = tree.account_inode_allocations();
    assert_eq!(totals.metrics[0].physical, 4096);
    assert_eq!(totals.metrics[0].reclaimable, 4096);
    assert_eq!(totals.metrics[0].logical, 8 << 30);
    assert_eq!(totals.held.get(root).get(HeldClass::HardLink), 0);
}

#[test]
fn overview_attributes_inode_bytes_once_and_trusts_only_listed_share_roots() {
    use std::collections::BTreeMap;
    use syno_reclaim::model::tree::NodeFlags;
    use syno_reclaim::overview::PartKind;
    let mut tree = InodeTree::default();
    let mut directory = allocation(2, 2, 4096);
    directory.kind = FileKind::Directory;
    let root = tree.add_inode_allocation(0, b"/volume", directory, true);
    directory.inode = 10;
    let a = tree.add_inode_allocation(root, b"a", directory, false);
    directory.inode = 20;
    let b = tree.add_inode_allocation(root, b"b", directory, false);
    tree.add_inode_allocation(a, b"data", allocation(30, 2, 8192), false);
    tree.add_inode_allocation(b, b"link", allocation(30, 2, 8192), false);
    directory.inode = 40;
    let ordinary = tree.add_inode_allocation(root, b"ordinary", directory, false);
    directory.inode = 41;
    let fake_bin = tree.add_inode_allocation(ordinary, b"#recycle", directory, false);
    let shares = BTreeMap::from([((7, 10), b"a".to_vec()), ((7, 20), b"b".to_vec())]);
    tree.classify_shares(&shares, None);
    assert!(
        !tree
            .tree()
            .node(fake_bin)
            .flags
            .contains(NodeFlags::RECYCLE)
    );
    let overview = tree.allocation_overview(&shares);
    assert_eq!(
        overview.items.iter().map(|item| item.bytes).sum::<u64>(),
        28672
    );
    assert_eq!(
        overview
            .items
            .iter()
            .find(|item| item.kind == PartKind::SharedBetweenFolders)
            .unwrap()
            .bytes,
        8192
    );
}
