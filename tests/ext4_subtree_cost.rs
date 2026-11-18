//! A volume with millions of single-name inodes must not build a tree for
//! every ancestor contribution. Count allocations, rather than timing the CPU.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use syno_reclaim::model::namespace::FileKind;
use syno_reclaim::model::tree::InodeAllocation;

struct CountingAllocator;
thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNT.try_with(|count| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            }
        });
        // SAFETY: forward the caller's allocation layout unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forward the allocation and its original layout unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn complete_inode_subtree_accounting_does_not_allocate_per_ancestor() {
    for kind in [FileKind::Regular, FileKind::Directory, FileKind::Symlink] {
        let inode = InodeAllocation {
            device: 1,
            inode: 2,
            kind,
            logical_bytes: 123,
            allocated_bytes: 4096,
            links: 1,
            ctime: 0,
            uncertain: false,
        };
        COUNT.with(|count| count.set(Some(0)));
        let (metrics, held, blockers) =
            std::hint::black_box(inode).subtree_totals(1, 1, true, true);
        let allocations = COUNT.with(|count| count.replace(None).unwrap());
        assert_eq!(metrics.physical, 4096);
        assert_eq!(metrics.reclaimable, 4096);
        assert_eq!(
            metrics.logical,
            if kind == FileKind::Regular { 123 } else { 0 }
        );
        assert_eq!(
            (
                metrics.entries,
                metrics.files,
                metrics.extents,
                metrics.unknown
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(held, Default::default());
        assert!(blockers.is_empty());
        assert_eq!(
            allocations, 0,
            "each inode/ancestor must avoid a temporary tree"
        );
    }
}
