//! In-memory filesystem model built from Btrfs metadata.

pub mod accounting;
pub mod extent;
pub mod namespace;
#[cfg(test)]
pub(crate) mod testing;
pub mod tree;
