//! Fixed-width owner records on the scan's private spill directory. Only a
//! bounded page cache is resident, shared by all trees and scope fragments.
use super::owners::{OwnerRecord, combine_owner};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

const RECORD: usize = 16;
const PAGE: usize = 4096;
const CACHE_PAGES: usize = 256;

#[derive(Debug)]
struct Table {
    path: PathBuf,
    records: usize,
}

#[derive(Debug)]
struct Page {
    used: u64,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct Cache {
    pages: HashMap<(usize, usize), Page>,
    tick: u64,
}

#[derive(Debug, Default)]
pub(super) struct OwnerTables {
    trees: BTreeMap<u64, Vec<usize>>,
    tables: Vec<Table>,
    cache: RefCell<Cache>,
}

impl OwnerTables {
    /// Each fragment is already sorted and collapsed. Keep fragments separate:
    /// overlapping scopes use the same conflict rule as the in-memory index.
    pub fn insert(&mut self, root: u64, records: &[OwnerRecord], path: &Path) -> io::Result<()> {
        let mut out = BufWriter::new(File::create(path)?);
        for record in records {
            out.write_all(&record.inode.to_le_bytes())?;
            out.write_all(&record.owner.to_le_bytes())?;
        }
        out.flush()?;
        self.trees.entry(root).or_default().push(self.tables.len());
        self.tables.push(Table {
            path: path.to_path_buf(),
            records: records.len(),
        });
        Ok(())
    }

    pub fn lookup(&self, root: u64, inode: u64) -> io::Result<Option<u64>> {
        let Some(tables) = self.trees.get(&root) else {
            return Ok(None);
        };
        let mut owner = None;
        for &id in tables {
            let mut low = 0;
            let mut high = self.tables[id].records;
            while low < high {
                let mid = low + (high - low) / 2;
                let record = self.record(id, mid)?;
                match record.inode.cmp(&inode) {
                    std::cmp::Ordering::Less => low = mid + 1,
                    std::cmp::Ordering::Greater => high = mid,
                    std::cmp::Ordering::Equal => {
                        owner = Some(
                            owner.map_or(record.owner, |prior| combine_owner(prior, record.owner)),
                        );
                        break;
                    }
                }
            }
        }
        Ok(owner)
    }

    fn record(&self, table: usize, record: usize) -> io::Result<OwnerRecord> {
        let offset = record * RECORD;
        let page = offset / PAGE;
        let mut cache = self.cache.borrow_mut();
        cache.tick += 1;
        let tick = cache.tick;
        let key = (table, page);
        if !cache.pages.contains_key(&key) {
            let source = &self.tables[table];
            let length = PAGE.min(source.records * RECORD - page * PAGE);
            // Evict before allocating: even the refill stays within the cap.
            if cache.pages.len() == CACHE_PAGES {
                let oldest = *cache
                    .pages
                    .iter()
                    .min_by_key(|(_, p)| p.used)
                    .expect("full cache")
                    .0;
                cache.pages.remove(&oldest);
            }
            let mut bytes = vec![0; length];
            File::open(&source.path)?.read_exact_at(&mut bytes, (page * PAGE) as u64)?;
            cache.pages.insert(key, Page { used: tick, bytes });
        }
        let entry = cache.pages.get_mut(&key).expect("cached page");
        entry.used = tick;
        let at = offset % PAGE;
        let record = OwnerRecord {
            inode: u64::from_le_bytes(entry.bytes[at..at + 8].try_into().expect("inode bytes")),
            owner: u64::from_le_bytes(
                entry.bytes[at + 8..at + RECORD]
                    .try_into()
                    .expect("owner bytes"),
            ),
        };
        Ok(record)
    }

    #[cfg(test)]
    pub fn retained_bytes(&self) -> usize {
        self.cache
            .borrow()
            .pages
            .values()
            .map(|p| p.bytes.capacity())
            .sum::<usize>()
            + self.tables.capacity() * std::mem::size_of::<Table>()
            + self
                .tables
                .iter()
                .map(|t| t.path.as_os_str().len())
                .sum::<usize>()
            + self
                .trees
                .values()
                .map(|v| v.capacity() * std::mem::size_of::<usize>())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::owners::{OWNER_CONFLICT, OWNER_RECYCLE, OWNER_SHARE, OWNER_SHARED};
    use crate::spill::{Spill, testing::scratch};

    #[test]
    fn disk_lookup_preserves_missing_inodes_and_conflicting_fragments() {
        let spill = Spill::create(scratch("owner-pages").join("run")).unwrap();
        let mut tables = OwnerTables::default();
        tables
            .insert(
                5,
                &[
                    OwnerRecord {
                        inode: 1,
                        owner: OWNER_SHARE | 256,
                    },
                    OwnerRecord {
                        inode: 3,
                        owner: OWNER_RECYCLE | 256,
                    },
                    OwnerRecord {
                        inode: 5,
                        owner: OWNER_SHARE | 256,
                    },
                ],
                &spill.path("a"),
            )
            .unwrap();
        tables
            .insert(
                5,
                &[
                    OwnerRecord {
                        inode: 3,
                        owner: OWNER_SHARE | 256,
                    },
                    OwnerRecord {
                        inode: 5,
                        owner: OWNER_CONFLICT,
                    },
                ],
                &spill.path("b"),
            )
            .unwrap();
        assert_eq!(tables.lookup(5, 1).unwrap(), Some(OWNER_SHARE | 256));
        assert_eq!(tables.lookup(5, 2).unwrap(), None);
        assert_eq!(tables.lookup(5, 3).unwrap(), Some(OWNER_SHARED));
        assert_eq!(tables.lookup(5, 5).unwrap(), Some(OWNER_CONFLICT));
        assert_eq!(tables.lookup(5, 6).unwrap(), None);
        assert_eq!(tables.lookup(6, 1).unwrap(), None);
    }

    #[test]
    fn evicted_pages_reload_and_io_errors_are_not_missing_owners() {
        let spill = Spill::create(scratch("owner-eviction").join("run")).unwrap();
        let mut tables = OwnerTables::default();
        let path = spill.path("table");
        let records: Vec<_> = (0..(CACHE_PAGES + 1) * PAGE / RECORD)
            .map(|inode| OwnerRecord {
                inode: inode as u64 * 2,
                owner: inode as u64,
            })
            .collect();
        tables.insert(5, &records, &path).unwrap();
        for record in &records {
            assert_eq!(tables.lookup(5, record.inode).unwrap(), Some(record.owner));
            assert_eq!(tables.lookup(5, record.inode + 1).unwrap(), None);
        }
        assert!(tables.cache.borrow().pages.len() <= CACHE_PAGES);
        assert!(tables.retained_bytes() < 2 << 20);
        // The first page has been evicted by the forward walk.
        std::fs::write(path, []).unwrap();
        assert_eq!(
            tables.lookup(5, 0).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
