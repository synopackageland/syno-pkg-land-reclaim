//! Supported filesystem kinds and capabilities advertised to callers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemKind {
    #[default]
    Btrfs,
    Ext4,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub scan: bool,
    pub snapshots: bool,
    pub reflinks: bool,
    pub cleanup: bool,
    pub cleanup_unavailable_reason: Option<String>,
}

impl FilesystemKind {
    pub fn is_btrfs(&self) -> bool {
        *self == Self::Btrfs
    }

    pub fn capabilities(self) -> Capabilities {
        let btrfs = self == Self::Btrfs;
        Capabilities {
            scan: true,
            snapshots: btrfs,
            reflinks: btrfs,
            // Cleanup moves items into their shared folder's Recycle Bin
            // with DSM's own tool, on either filesystem (SPEC 5.7).
            cleanup: true,
            cleanup_unavailable_reason: None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Btrfs => "btrfs",
            Self::Ext4 => "ext4",
        }
    }
}
