//! A whole recording (`debug record` format) replayed through subvolume
//! discovery, namespace reconstruction and listing, as `debug tree` does.
//! Seed the corpus with `tests/golden/*.srbuf`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::ops::ControlFlow;
use syno_reclaim::btrfs::keys::FIRST_FREE_OBJECTID;
use syno_reclaim::btrfs::recording::ReplaySource;
use syno_reclaim::btrfs::subvolume::discover;
use syno_reclaim::btrfs::walk::{BatchBoundary, StopReason};
use syno_reclaim::model::namespace::{Namespace, format_listing};

struct NoStop;

impl BatchBoundary for NoStop {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        ControlFlow::Continue(())
    }
}

/// Subvolumes whose tree the recording may continue with.
const MAX_CANDIDATES: usize = 8;

fuzz_target!(|input: &[u8]| {
    let Ok(mut replay) = ReplaySource::parse(input) else {
        return;
    };
    let Ok(subvolumes) = discover(&mut replay, &mut NoStop) else {
        return;
    };
    for subvolume in subvolumes.subvolumes.iter().take(MAX_CANDIDATES) {
        let mut replay = ReplaySource::parse(input).unwrap();
        discover(&mut replay, &mut NoStop).unwrap();
        let Ok(ns) = Namespace::read(&mut replay, subvolume.id, &mut NoStop) else {
            continue;
        };
        if let Ok(entries) = ns.listing(FIRST_FREE_OBJECTID, &subvolumes) {
            let _ = format_listing(&entries);
        }
    }
});
