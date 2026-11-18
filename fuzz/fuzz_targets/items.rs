//! A TREE_SEARCH_V2 result buffer, validated and then fed item by item to
//! every item parser. First byte: `nr_items`; the rest: the buffer.
#![no_main]

use libfuzzer_sys::fuzz_target;
use syno_reclaim::btrfs::keys::Key;
use syno_reclaim::btrfs::search::{SearchRange, batch_len, parse_batch};
use syno_reclaim::btrfs::{backref, parser};

fuzz_target!(|input: &[u8]| {
    let Some((&nr_items, buf)) = input.split_first() else {
        return;
    };
    let nr_items = u32::from(nr_items);
    let range = SearchRange::new(5, Key::MIN, Key::MAX);
    let parsed = parse_batch(buf, nr_items, &range);
    if parsed.is_ok() {
        assert!(batch_len(buf, nr_items).unwrap() <= buf.len());
    }
    for item in parsed.into_iter().flatten() {
        let data = item.data;
        let _ = parser::parse_inode_item(data);
        let _ = parser::parse_dir_index(data);
        let _ = parser::parse_file_extent(data);
        let _ = parser::parse_root_item(data);
        let _ = parser::parse_root_ref(data);
        let _ = backref::parse_extent_item(data);
        let _ = backref::parse_keyed_backref(item.key, data);
    }
});
