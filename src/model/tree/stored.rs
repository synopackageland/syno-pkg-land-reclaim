//! The stored form of a prepared scope tree: a magic line, varint
//! sections for nodes, names, inodes, links, roots, marked folders and
//! anomalies, then
//! an FNV-1a 64 checksum of everything before it. Extent uses are not
//! part of it: they stay in the unit's extents file (SPEC 9.1).

use super::{Anomalies, Node, ScopeTree, TreeInode};
use crate::codec::{self, Decoder, HashingWriter};
use crate::dsm;
use crate::model::namespace::FileKind;
use crate::model::tree::{NO_INODE, NO_PARENT, NodeFlags};
use std::collections::HashMap;
use std::io::{self, Write};

const MAGIC: &[u8; 8] = b"SRTREE3\n";

#[derive(Debug, thiserror::Error)]
#[error("stored scope tree is truncated or corrupt")]
pub struct Corrupt;

impl ScopeTree {
    /// Writes the prepared tree; hard-link lookups are not kept, so the
    /// stored tree cannot grow.
    pub fn store<W: Write>(&self, out: W) -> io::Result<u64> {
        let mut w = HashingWriter::new(out);
        w.put(MAGIC)?;
        w.varint(self.nodes.len() as u64)?;
        for n in &self.nodes {
            w.varint(u64::from(n.parent.wrapping_add(1)))?;
            w.varint(u64::from(n.name_len))?;
            w.varint(u64::from(n.inode.wrapping_add(1)))?;
            w.put(&[n.kind.letter() as u8, n.flags.bits()])?;
        }
        w.varint(self.names.len() as u64)?;
        w.put(&self.names)?;
        w.varint(self.inodes.len() as u64)?;
        for i in &self.inodes {
            for v in [i.tree, i.objectid, i.size, u64::from(i.nlink)] {
                w.varint(v)?;
            }
            w.put(&[i.kind.letter() as u8])?;
            w.varint(i.inline_bytes)?;
        }
        w.varint(self.links.len() as u64)?;
        for &(inode, node) in &self.links {
            w.varint(u64::from(inode))?;
            w.varint(u64::from(node))?;
        }
        w.varint(self.roots.len() as u64)?;
        for &r in &self.roots {
            w.varint(u64::from(r))?;
        }
        let mut markers: Vec<_> = self.markers.iter().collect();
        markers.sort_unstable();
        w.varint(markers.len() as u64)?;
        for (&node, marker) in markers {
            w.varint(u64::from(node))?;
            w.varint(dsm::marker_code(marker))?;
        }
        let a = &self.anomalies;
        for v in [
            a.dangling_entries,
            a.cycles,
            a.unsupported_locations,
            a.missing_subvolume_roots,
        ] {
            w.varint(v)?;
        }
        let checksum = w.checksum();
        w.put(&checksum.to_le_bytes())?;
        w.flush()?;
        Ok(w.written())
    }

    /// Reads a tree `store` wrote, checking its structure and checksum.
    pub fn load(bytes: &[u8]) -> Result<ScopeTree, Corrupt> {
        let body = bytes.len().checked_sub(8).ok_or(Corrupt)?;
        let stored = u64::from_le_bytes(bytes[body..].try_into().map_err(|_| Corrupt)?);
        if !bytes.starts_with(MAGIC) || codec::checksum(&bytes[..body]) != stored {
            return Err(Corrupt);
        }
        let mut d = Decoder::new(&bytes[..body], MAGIC.len());
        let tree = read(&mut d).ok_or(Corrupt)?;
        if !d.is_empty() || !tree.is_consistent() {
            return Err(Corrupt);
        }
        Ok(tree)
    }

    /// Indexes stay in range and parents precede their children.
    fn is_consistent(&self) -> bool {
        let nodes = self.nodes.len() as u32;
        let inodes = self.inodes.len() as u32;
        let names_fit = self
            .nodes
            .last()
            .is_some_and(|n| (n.name_offset + n.name_len) as usize == self.names.len());
        let nodes_fit = self
            .nodes
            .iter()
            .enumerate()
            .skip(1)
            .all(|(i, n)| n.parent < i as u32 && (n.inode == NO_INODE || n.inode < inodes));
        names_fit
            && self.nodes[0].parent == NO_PARENT
            && nodes_fit
            && self.links.iter().all(|&(i, n)| i < inodes && n < nodes)
            && self.links.is_sorted()
            && self.roots.iter().all(|&r| r > 0 && r < nodes)
            && self.markers.keys().all(|&n| n < nodes)
    }
}

fn read(d: &mut Decoder<'_>) -> Option<ScopeTree> {
    let mut name_offset = 0u32;
    let nodes = list(d, |d| {
        let parent = u32(d)?.wrapping_sub(1);
        let name_len = u32(d)?;
        let inode = u32(d)?.wrapping_sub(1);
        let kind = kind(d)?;
        let flags = NodeFlags::from_bits(d.byte()?);
        let node = Node {
            parent,
            name_offset,
            name_len,
            inode,
            kind,
            flags,
        };
        name_offset = name_offset.checked_add(name_len)?;
        Some(node)
    })?;
    let names_len = usize::try_from(d.varint()?).ok()?;
    let names = d.take(names_len)?.to_vec();
    let inodes = list(d, |d| {
        let tree = d.varint()?;
        let objectid = d.varint()?;
        let size = d.varint()?;
        let nlink = u32(d)?;
        Some(TreeInode {
            tree,
            objectid,
            size,
            nlink,
            kind: kind(d)?,
            inline_bytes: d.varint()?,
        })
    })?;
    let links = list(d, |d| Some((u32(d)?, u32(d)?)))?;
    let roots = list(d, u32)?;
    let markers = list(d, |d| Some((u32(d)?, dsm::marker_of_code(d.varint()?)?)))?;
    let anomalies = Anomalies {
        dangling_entries: d.varint()?,
        cycles: d.varint()?,
        unsupported_locations: d.varint()?,
        missing_subvolume_roots: d.varint()?,
    };
    if nodes.is_empty() {
        return None;
    }
    Some(ScopeTree {
        nodes,
        names,
        inodes,
        recycle_ctimes: Vec::new(),
        linked_inodes: HashMap::new(),
        links,
        uses: Vec::new(),
        roots,
        markers: markers.into_iter().collect(),
        anomalies,
    })
}

/// A varint count, then that many items; the count is bounded by the
/// bytes left, since every item takes at least one.
fn list<T>(
    d: &mut Decoder<'_>,
    mut item: impl FnMut(&mut Decoder<'_>) -> Option<T>,
) -> Option<Vec<T>> {
    let count = usize::try_from(d.varint()?).ok()?;
    if count > d.remaining() {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(item(d)?);
    }
    Some(out)
}

fn u32(d: &mut Decoder<'_>) -> Option<u32> {
    u32::try_from(d.varint()?).ok()
}

fn kind(d: &mut Decoder<'_>) -> Option<FileKind> {
    FileKind::from_letter(char::from(d.byte()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::subvolume::SubvolumeSet;
    use crate::model::testing::*;
    use crate::model::tree::{Marks, RootKind};

    const MIB: u64 = 1 << 20;

    /// Two roots, a hard link, a bookend extent, a placeholder and a
    /// folder a package marker marks.
    fn tree() -> ScopeTree {
        let ns = read_tree(
            257,
            vec![
                inode(256, DIR, 0, 1),
                entry(256, 2, "a", 257),
                entry(256, 3, "d", 258),
                subvolume_entry(256, 4, "sub", 300),
                inode(257, FILE, 2 * MIB, 2),
                whole_extent(257, 0, 30 * MIB, MIB),
                whole_extent(257, MIB, 10 * MIB, MIB),
                inode(258, DIR, 0, 1),
                entry(258, 2, "b", 257),
                entry(258, 3, "gone", 999),
                entry(258, 4, "@ActiveBackup", 259),
                inode(259, DIR, 0, 1),
            ],
        )
        .unwrap();
        let mut tree = ScopeTree::new();
        for (ino, name) in [(256, "/v"), (258, "/w")] {
            tree.add_root(
                &ns,
                ino,
                name.as_bytes(),
                RootKind::Volume,
                &SubvolumeSet::default(),
                &Marks::default(),
            )
            .unwrap();
        }
        tree.prepare();
        tree
    }

    fn stored(tree: &ScopeTree) -> Vec<u8> {
        let mut out = Vec::new();
        let written = tree.store(&mut out).unwrap();
        assert_eq!(written, out.len() as u64);
        out
    }

    #[test]
    fn a_stored_tree_loads_back_equal_without_its_lookups_and_uses() {
        let mut tree = tree();
        let loaded = ScopeTree::load(&stored(&tree)).unwrap();
        tree.linked_inodes.clear();
        assert!(!tree.uses.is_empty());
        // Uses live in the unit's extents file, never in memory with it.
        tree.uses.clear();
        assert_eq!(loaded, tree);
        assert!(!loaded.markers.is_empty());
        assert_eq!(loaded.anomalies().dangling_entries, 2, "once per root");
    }

    #[test]
    fn corrupt_or_truncated_trees_are_rejected() {
        let bytes = stored(&tree());
        for cut in [0, 7, 8, bytes.len() / 2, bytes.len() - 1] {
            assert!(ScopeTree::load(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut flipped = bytes.clone();
        flipped[20] ^= 1;
        assert!(ScopeTree::load(&flipped).is_err());
    }
}
