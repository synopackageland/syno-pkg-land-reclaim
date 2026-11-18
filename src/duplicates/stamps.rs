//! The kept-copy stamps of folder groups (`<report>.srstamps`): every
//! folder and file stamp below each folder member, which only
//! `cleanup.plan` reads. Kept out of the results so a page reads a small
//! file (docs/duplicates.md).
//!
//! One JSON line per folder group, then a trailer line naming the file:
//! `{"format":1,"id":"…"}`. The results hold each group's offset, length
//! and FNV-1a checksum, and the id: a line that does not match, or a file
//! of other results, is refused rather than read.

use super::model::{Analysis, FileStamp, FolderGroup, StampsAt};
use crate::codec::{self, Fnv};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const FORMAT: u32 = 1;
/// The trailer line is far shorter.
const TRAILER_MAX: u64 = 256;

/// The stamps beside `report`.
pub fn path_of(report: &Path) -> PathBuf {
    report.with_extension("srstamps")
}

/// One member's stamps in a group's line.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Member {
    #[serde(with = "super::model::hex_list", default)]
    dirs: Vec<Vec<u8>>,
    #[serde(default)]
    file_stamps: Vec<FileStamp>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Trailer {
    format: u32,
    id: String,
}

fn has_inline(g: &FolderGroup) -> bool {
    g.members
        .iter()
        .any(|m| !m.dirs.is_empty() || !m.file_stamps.is_empty())
}

/// Whether any folder member still holds its stamps.
pub fn inline(analysis: &Analysis) -> bool {
    analysis.folders.iter().any(has_inline)
}

/// Moves every folder member's stamps out of `analysis` into `out`, one
/// line per group, and points each group and the results at them. The
/// same stamps always write the same file and id.
pub fn write(analysis: &mut Analysis, out: &mut dyn Write) -> io::Result<()> {
    let mut offset = 0u64;
    let mut id = Fnv::new();
    for g in &mut analysis.folders {
        g.stamps = None;
        if !has_inline(g) {
            continue;
        }
        let members: Vec<Member> = g
            .members
            .iter_mut()
            .map(|m| Member {
                dirs: std::mem::take(&mut m.dirs),
                file_stamps: std::mem::take(&mut m.file_stamps),
            })
            .collect();
        let mut line = serde_json::to_vec(&members).map_err(io::Error::other)?;
        line.push(b'\n');
        let at = StampsAt {
            offset,
            len: line.len() as u64,
            checksum: codec::checksum(&line),
        };
        out.write_all(&line)?;
        offset += at.len;
        for v in [at.offset, at.len, at.checksum] {
            id.update(&v.to_le_bytes());
        }
        g.stamps = Some(at);
    }
    analysis.stamps_id = format!("{:016x}", id.value());
    let trailer = Trailer {
        format: FORMAT,
        id: analysis.stamps_id.clone(),
    };
    serde_json::to_writer(&mut *out, &trailer).map_err(io::Error::other)?;
    out.write_all(b"\n")
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// An open stamps file checked against the results it goes with.
pub struct Stamps {
    file: File,
    /// Where the trailer starts: no line reaches past it.
    end: u64,
}

impl Stamps {
    /// Opens the stamps of `report` and checks they are those of results
    /// with `id`.
    pub fn open(report: &Path, id: &str) -> io::Result<Stamps> {
        let mut file = File::open(path_of(report))?;
        let size = file.metadata()?.len();
        let tail = size.min(TRAILER_MAX);
        file.seek(SeekFrom::Start(size - tail))?;
        let mut bytes = Vec::with_capacity(tail as usize);
        (&mut file).take(tail).read_to_end(&mut bytes)?;
        let body = bytes
            .strip_suffix(b"\n")
            .ok_or_else(|| invalid("stamps have no trailer"))?;
        let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        if start == 0 && size > tail {
            return Err(invalid("stamps trailer too long"));
        }
        let trailer: Trailer =
            serde_json::from_slice(&body[start..]).map_err(|_| invalid("stamps trailer"))?;
        if trailer.format != FORMAT || trailer.id != id {
            return Err(invalid("stamps of other results"));
        }
        Ok(Stamps {
            file,
            end: size - tail + start as u64,
        })
    }

    /// Puts back the stamps of `group`'s members. A group without stamps
    /// (none needed, or still inline) is left as it is.
    pub fn attach(&mut self, group: &mut FolderGroup) -> io::Result<()> {
        let Some(at) = group.stamps else {
            return Ok(());
        };
        if at.offset.checked_add(at.len).is_none_or(|e| e > self.end) {
            return Err(invalid("stamps line out of range"));
        }
        self.file.seek(SeekFrom::Start(at.offset))?;
        let mut line = vec![0u8; at.len as usize];
        self.file.read_exact(&mut line)?;
        if codec::checksum(&line) != at.checksum {
            return Err(invalid("stamps line checksum"));
        }
        let members: Vec<Member> =
            serde_json::from_slice(&line).map_err(|_| invalid("stamps line"))?;
        if members.len() != group.members.len() {
            return Err(invalid("stamps of other members"));
        }
        for (m, s) in group.members.iter_mut().zip(members) {
            m.dirs = s.dirs;
            m.file_stamps = s.file_stamps;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duplicates::model::{FolderMember, Stamp};

    fn analysis() -> Analysis {
        let mut a = Analysis::default();
        for (id, n) in [("fa", 2), ("fb", 3)] {
            let members = (0..n)
                .map(|k| {
                    let mut m = FolderMember::new(format!("/v/s{k}/{id}").as_bytes(), b"/v", 1, 10);
                    m.dirs = vec![format!("/v/s{k}/{id}/d").into_bytes()];
                    m.file_stamps = vec![FileStamp {
                        rel: b"/d/x".to_vec(),
                        stamp: Stamp {
                            size: 10,
                            mtime: k,
                            ..Stamp::default()
                        },
                    }];
                    m
                })
                .collect();
            a.folders.push(FolderGroup {
                id: id.into(),
                name: id.as_bytes().to_vec(),
                files: 1,
                bytes: 10,
                covered: 0,
                members,
                clusters: Vec::new(),
                items: Vec::new(),
                in_folders: false,
                stamps: None,
            });
        }
        a
    }

    fn written(dir: &Path) -> (PathBuf, Analysis, Analysis) {
        let report = dir.join("r.srrep");
        let original = analysis();
        let mut split = original.clone();
        let mut out = Vec::new();
        write(&mut split, &mut out).unwrap();
        std::fs::write(path_of(&report), &out).unwrap();
        (report, original, split)
    }

    /// The results lose the stamps and get them back, group by group.
    #[test]
    fn stamps_round_trip_by_group() {
        let dir = crate::spill::testing::scratch("dup-stamps");
        let (report, original, mut split) = written(&dir);
        assert!(!inline(&split) && inline(&original));
        let json = serde_json::to_string(&split).unwrap();
        assert!(!json.contains("file_stamps") && !json.contains("\"dirs\""));
        let mut stamps = Stamps::open(&report, &split.stamps_id).unwrap();
        stamps.attach(&mut split.folders[1]).unwrap();
        assert_eq!(split.folders[1].members, original.folders[1].members);
        assert!(split.folders[0].members[0].file_stamps.is_empty());
        // The same stamps write the same id.
        let mut again = original.clone();
        write(&mut again, &mut Vec::new()).unwrap();
        assert_eq!(again.stamps_id, split.stamps_id);
    }

    /// Stamps of other results, a damaged line or a missing file are
    /// refused, never read as another group's.
    #[test]
    fn mismatched_or_corrupt_stamps_are_refused() {
        let dir = crate::spill::testing::scratch("dup-stamps-bad");
        let (report, _, mut split) = written(&dir);
        assert!(Stamps::open(&report, "0000000000000000").is_err());
        let mut bytes = std::fs::read(path_of(&report)).unwrap();
        bytes[2] ^= 1;
        std::fs::write(path_of(&report), &bytes).unwrap();
        let mut stamps = Stamps::open(&report, &split.stamps_id).unwrap();
        assert!(stamps.attach(&mut split.folders[0]).is_err());
        let mut other = split.folders[1].clone();
        other.members.pop();
        assert!(stamps.attach(&mut other).is_err());
        std::fs::remove_file(path_of(&report)).unwrap();
        assert!(Stamps::open(&report, &split.stamps_id).is_err());
    }
}
