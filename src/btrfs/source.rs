//! `MetadataSource`: the only way parser and logic code reach Btrfs
//! metadata (SPEC 18 layer 1). The live implementation issues
//! TREE_SEARCH_V2; tests replay recorded buffers or hand-built items.

use crate::btrfs::leaf_ranges::BlockReader;
use crate::btrfs::search::SearchRange;
use crate::btrfs::walk::{BatchBoundary, WalkError};
use serde::{Deserialize, Serialize};
use std::io;

/// Failure of one search call.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("tree search failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("a single item needs a {needed}-byte buffer, above the {cap}-byte cap")]
    BufferCap { needed: u64, cap: u64 },
    #[error("kernel asked for a {needed}-byte buffer while {current} bytes were offered")]
    NoProgress { needed: u64, current: usize },
    #[error("recorded search {index} does not match the request {requested:?}")]
    ReplayMismatch {
        index: usize,
        requested: SearchRange,
    },
    #[error("recording ended before request {index}")]
    ReplayExhausted { index: usize },
}

/// The raw result of one search call, borrowed from the source's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawBatch<'a> {
    /// Number of items the buffer holds; zero means the range is exhausted.
    pub nr_items: u32,
    pub buf: &'a [u8],
}

/// `statfs` byte counts captured alongside Btrfs space information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSpace {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
}

/// One raw block-group class returned by `BTRFS_IOC_SPACE_INFO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockGroupSpace {
    pub flags: u64,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// Read-only space measurements from one instant in the scan window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceSnapshot {
    pub statfs: Result<FsSpace, String>,
    pub block_groups: Result<Vec<BlockGroupSpace>, String>,
}

/// Space measurements on both sides of a volume scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceWindow {
    pub start: Option<SpaceSnapshot>,
    pub end: Option<SpaceSnapshot>,
}

/// Performs single TREE_SEARCH_V2-shaped calls.
pub trait MetadataSource {
    /// Searches `range` starting at `range.min`.
    fn search(&mut self, range: &SearchRange) -> Result<RawBatch<'_>, SourceError>;

    /// Probes whether each balance and device replace is running
    /// (SPEC 8.1). A source without a live filesystem probes nothing.
    fn maintenance_probes(&mut self) -> Vec<Probe> {
        Vec::new()
    }

    /// Captures space counters when the source is a live filesystem.
    fn space_snapshot(&mut self) -> Option<SpaceSnapshot> {
        None
    }

    /// Reads tree blocks by address (SPEC 8.1), or says why it cannot.
    fn block_reader(&mut self, _boundary: &mut dyn BatchBoundary) -> ReaderResult {
        Ok(Err("the metadata source is not a live filesystem".into()))
    }
}

/// A block reader, why there is none, or why looking for one stopped.
pub type ReaderResult = Result<Result<Box<dyn BlockReader>, String>, WalkError>;

/// A filesystem operation that rewrites extent mappings (SPEC 8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Maintenance {
    Balance,
    DevReplace,
}

impl Maintenance {
    /// The JSON name.
    pub fn name(self) -> &'static str {
        match self {
            Maintenance::Balance => "balance",
            Maintenance::DevReplace => "dev-replace",
        }
    }
}

/// One maintenance probe: whether the operation is running.
pub type Probe = (Maintenance, io::Result<bool>);
