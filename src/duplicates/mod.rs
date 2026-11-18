//! Duplicate content on Btrfs from metadata only (docs/duplicates.md).

#[cfg(target_os = "linux")]
pub mod analysis;
pub mod candidates;
#[cfg(target_os = "linux")]
pub mod command;
pub mod csum;
pub mod folders;
pub mod model;
pub mod partial;
pub mod stamps;
