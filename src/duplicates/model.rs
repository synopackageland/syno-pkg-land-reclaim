//! What a duplicate analysis keeps (`<report>.srdup`), and the answers
//! derived from it for a scope: which copy stays, which go, and what that
//! frees (docs/duplicates.md "Keep rule", "Scope").

use crate::report::hex_bytes;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Version 4 keeps the kept-copy stamps (`dirs`, `file_stamps`) in the
/// `.srstamps` file beside the results, which only `cleanup.plan` reads.
pub const FORMAT: u32 = 4;
/// Results that still hold the stamps inline; read once and split.
pub const FORMAT_INLINE_STAMPS: u32 = 3;
/// The rule of stored tallies: 1, every group cleaned on its whole volume
/// (none before); 2, a copy another listed folder group keeps is the one
/// kept. Results with tallies of another rule compute them
/// again; nothing is scanned or analysed again.
pub const TALLY_RULE: u32 = 2;
/// Groups and folders smaller than this are not listed, counted or
/// selected; smaller files still make folders identical.
pub const LIST_MIN_SIZE: u64 = 10 << 20;
/// Directories whose tallies the analysis keeps; others are computed
/// when asked.
pub const TALLY_CAP: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Photos,
    Videos,
    Music,
    #[default]
    Other,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Photos,
        Category::Videos,
        Category::Music,
        Category::Other,
    ];

    /// By file name extension, case-insensitive.
    pub fn of(name: &[u8]) -> Category {
        let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
            return Category::Other;
        };
        let ext = String::from_utf8_lossy(&name[dot + 1..]).to_ascii_lowercase();
        const PHOTOS: &[&str] = &[
            "jpg", "jpeg", "png", "heic", "heif", "gif", "bmp", "tif", "tiff", "webp", "raw",
            "arw", "cr2", "cr3", "nef", "orf", "rw2", "dng", "raf", "srw", "psd",
        ];
        const VIDEOS: &[&str] = &[
            "mp4", "mov", "m4v", "mkv", "avi", "wmv", "flv", "webm", "mts", "m2ts", "ts", "3gp",
            "mpg", "mpeg", "vob", "insv",
        ];
        const MUSIC: &[&str] = &[
            "mp3", "flac", "aac", "m4a", "wav", "ogg", "opus", "wma", "aiff", "aif", "ape", "dsf",
        ];
        if PHOTOS.contains(&ext.as_str()) {
            Category::Photos
        } else if VIDEOS.contains(&ext.as_str()) {
            Category::Videos
        } else if MUSIC.contains(&ext.as_str()) {
            Category::Music
        } else {
            Category::Other
        }
    }
}

/// Large same-size files whose content could not be compared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unchecked {
    pub files: u64,
    pub bytes: u64,
    /// No data checksums: NOCOW (`nodatasum`) data.
    pub no_checksums: u64,
    /// Compressed extents: their checksums cover compressed bytes.
    pub compressed: u64,
    /// Candidate identical folders with a file that has no checksum.
    #[serde(default)]
    pub folders: u64,
    #[serde(default)]
    pub folder_bytes: u64,
}

/// Copies sharing extents (clones): deleting all of them frees `free`
/// now and `later` once the snapshots holding the rest go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cluster {
    pub free: u64,
    pub later: u64,
    /// Held by references outside the cluster (another file, another
    /// group): never freed by deleting it.
    pub held: u64,
    pub unknown: u64,
}

/// What a file's inode said when the analysis read it: a kept file whose
/// size or times differ later may have other content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
}

impl Stamp {
    pub fn of(i: &crate::btrfs::parser::InodeItem) -> Self {
        Stamp {
            size: i.size,
            mtime: i.mtime.sec as i64,
            mtime_nsec: i64::from(i.mtime.nsec),
            ctime: i.ctime.sec as i64,
            ctime_nsec: i64::from(i.ctime.nsec),
        }
    }

    /// The stamp of the file at `meta`.
    pub fn of_metadata(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Stamp {
            size: meta.size(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
        }
    }
}

/// A file of a kept folder copy, by its path below the folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    #[serde(with = "hex_bytes")]
    pub rel: Vec<u8>,
    pub stamp: Stamp,
}

/// One copy: an inode with every name it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyRecord {
    /// Sorted; the first is the one shown.
    #[serde(with = "hex_list")]
    pub names: Vec<Vec<u8>>,
    pub share: String,
    /// Unix seconds.
    pub mtime: u64,
    pub tree: u64,
    pub objectid: u64,
    /// Inode generation, for the cleanup plan's identity check.
    pub generation: u64,
    pub cluster: u32,
    /// A name lies in a `#recycle` folder.
    pub recycle: bool,
    /// A name lies in a Synology Drive folder (`homes/<user>/Drive`).
    pub drive: bool,
    pub stamp: Stamp,
}

impl CopyRecord {
    pub fn new(
        names: Vec<Vec<u8>>,
        mount: &[u8],
        mtime: u64,
        tree: u64,
        objectid: u64,
        generation: u64,
        cluster: u32,
    ) -> Self {
        let share = share_of(&names[0], mount);
        let recycle = names.iter().any(|n| has_component(n, b"#recycle"));
        let drive = names.iter().any(|n| in_drive(n, mount));
        CopyRecord {
            names,
            share,
            mtime,
            tree,
            objectid,
            generation,
            cluster,
            recycle,
            drive,
            stamp: Stamp::default(),
        }
    }

    pub fn path(&self) -> &[u8] {
        &self.names[0]
    }

    pub fn names(&self) -> &[Vec<u8>] {
        &self.names
    }

    pub fn file_name(&self) -> &[u8] {
        last_name(self.path())
    }
}

/// Files with the same content in two or more places.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// Stable while the content is: from its checksum signature.
    pub id: String,
    #[serde(with = "hex_bytes")]
    pub name: Vec<u8>,
    pub size: u64,
    pub category: Category,
    pub copies: Vec<CopyRecord>,
    pub clusters: Vec<Cluster>,
    /// Every copy lies in a listed identical folder.
    pub in_folders: bool,
}

/// One place an identical folder is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderMember {
    #[serde(with = "hex_bytes")]
    pub path: Vec<u8>,
    pub share: String,
    pub files: u64,
    pub bytes: u64,
    /// Newest modification of its files, Unix seconds.
    pub mtime: u64,
    /// Oldest modification of its files: the keep rule's age.
    pub oldest: u64,
    pub recycle: bool,
    pub drive: bool,
    /// The folder's own inode, for the cleanup plan's identity check.
    pub tree: u64,
    pub objectid: u64,
    pub generation: u64,
    /// Members sharing extents (a reflinked copy) share a cluster.
    pub cluster: u32,
    /// Every folder below it (junk folders left out): what a cleanup
    /// that keeps this member checks again. In the `.srstamps` file, not
    /// the results: empty until `cleanup.plan` loads it.
    #[serde(with = "hex_list", default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<Vec<u8>>,
    /// Every file below it (junk left out) as the analysis read it: what a
    /// cleanup that keeps this member checks again. For partly identical
    /// folders, the files of the identical items only. Kept like `dirs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_stamps: Vec<FileStamp>,
    /// Partly identical folders only: the member's own counts and each
    /// identical item's inode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
}

/// One identical item of partly identical folders: a folder or a file at
/// the same relative path in every member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Below the member, without a leading `/`.
    #[serde(with = "hex_bytes")]
    pub rel: Vec<u8>,
    pub folder: bool,
    /// Files and bytes of the item, junk left out.
    pub files: u64,
    pub bytes: u64,
}

/// A member of partly identical folders: what it holds besides the
/// identical items, and each item's inode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    /// Entries directly in the member, junk left out.
    pub total_items: u64,
    /// Those of them that are identical items.
    pub same_items: u64,
    /// Files and bytes below the member, junk left out.
    pub total_files: u64,
    pub total_bytes: u64,
    /// Per item of the group, in its order: the item's objectid and
    /// generation in this member, for the cleanup plan's identity check.
    pub items: Vec<(u64, u64)>,
}

impl FolderMember {
    pub fn new(path: &[u8], mount: &[u8], files: u64, bytes: u64) -> Self {
        FolderMember {
            path: path.to_vec(),
            share: share_of(path, mount),
            files,
            bytes,
            mtime: 0,
            oldest: u64::MAX,
            recycle: has_component(path, b"#recycle"),
            drive: in_drive(path, mount),
            tree: 0,
            objectid: 0,
            generation: 0,
            cluster: 0,
            dirs: Vec::new(),
            file_stamps: Vec::new(),
            side: None,
        }
    }
}

/// The same folder content in two or more places; only the topmost of
/// nested identical folders is a group. With `items`, partly identical
/// folders: only those items, at the same relative paths, are the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderGroup {
    pub id: String,
    #[serde(with = "hex_bytes")]
    pub name: Vec<u8>,
    /// Files and bytes of one member, junk left out; of its identical
    /// items for partly identical folders.
    pub files: u64,
    pub bytes: u64,
    /// File groups whose every copy lies in its members: listed with it
    /// and not on their own.
    pub covered: u64,
    pub members: Vec<FolderMember>,
    pub clusters: Vec<Cluster>,
    /// Partly identical folders: the identical items, topmost only, by
    /// relative path. Empty for identical folders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Item>,
    /// Every member lies in a part another listed folder group removes,
    /// or is an identical item of similar folders: not listed.
    #[serde(default)]
    pub in_folders: bool,
    /// Where its members' `dirs` and `file_stamps` are in `.srstamps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamps: Option<StampsAt>,
}

/// One folder group's record in the `.srstamps` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StampsAt {
    pub offset: u64,
    pub len: u64,
    /// FNV-1a 64 of the record's bytes.
    pub checksum: u64,
}

impl FolderGroup {
    /// At least 10 MiB, not only clones of one copy (those free nothing),
    /// and not inside other listed folder groups.
    pub fn listed(&self) -> bool {
        self.bytes >= LIST_MIN_SIZE && self.clusters.len() > 1 && !self.in_folders
    }

    /// Partly identical folders.
    pub fn partial(&self) -> bool {
        !self.items.is_empty()
    }

    /// What removing `member` removes: the whole folder, or each
    /// identical item of partly identical folders, with the inode
    /// (objectid, generation) the analysis read.
    pub fn parts(&self, member: &FolderMember) -> Vec<(Vec<u8>, u64, u64)> {
        match &member.side {
            Some(side) if self.partial() => self
                .items
                .iter()
                .zip(&side.items)
                .map(|(item, &(objectid, generation))| {
                    (
                        [member.path.as_slice(), b"/", &item.rel].concat(),
                        objectid,
                        generation,
                    )
                })
                .collect(),
            _ => vec![(member.path.clone(), member.objectid, member.generation)],
        }
    }
}

/// The parts listed folder groups remove ([`FolderGroup::parts`]): a copy
/// inside one is that folder group's to remove and count.
#[derive(Debug, Clone, Default)]
pub struct Covered {
    /// Sorted: the path, the folder group's index and its member's.
    parts: Vec<(Vec<u8>, usize, usize)>,
}

impl Covered {
    pub fn of(folders: &[FolderGroup]) -> Self {
        let mut parts = Vec::new();
        for (i, f) in folders.iter().enumerate().filter(|(_, f)| f.listed()) {
            for (k, m) in f.members.iter().enumerate() {
                parts.extend(f.parts(m).into_iter().map(|(p, _, _)| (p, i, k)));
            }
        }
        parts.sort();
        Covered { parts }
    }

    /// Whether `path` is, or lies inside, a part of a listed folder group
    /// other than `except`.
    pub fn holds(&self, path: &[u8], except: Option<usize>) -> bool {
        !self.holders(path, except).is_empty()
    }

    /// The folder groups other than `except`, each with its member, whose
    /// parts are `path` or lie above it: each removes `path` with its
    /// member, or keeps it.
    pub fn holders(&self, path: &[u8], except: Option<usize>) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut at = path;
        while !at.is_empty() {
            let start = self.parts.partition_point(|(p, _, _)| p.as_slice() < at);
            out.extend(
                self.parts[start..]
                    .iter()
                    .take_while(|(p, _, _)| p.as_slice() == at)
                    .filter(|&&(_, g, _)| Some(g) != except)
                    .map(|&(_, g, m)| (g, m)),
            );
            at = parent(at);
        }
        out
    }
}

/// One analysis of one volume report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Analysis {
    pub format: u32,
    /// The report analysed, by its scan job.
    pub report_job_id: String,
    #[serde(with = "hex_bytes")]
    pub mount: Vec<u8>,
    /// Unix seconds.
    pub analyzed_at: u64,
    /// The preferred shares the tallies were computed with.
    pub preferred_shares: Vec<String>,
    /// Every candidate was read; false when the record limit cut some.
    pub complete: bool,
    pub candidates: u64,
    pub same_size: u64,
    pub excluded: u64,
    pub changed: u64,
    pub searches: u64,
    pub csum_searches: u64,
    pub reference_searches: u64,
    pub unchecked: Unchecked,
    pub groups: Vec<Group>,
    pub folders: Vec<FolderGroup>,
    /// Per-directory tallies, at most [`TALLY_CAP`], shallowest first.
    #[serde(with = "tally_map")]
    pub tallies: BTreeMap<Vec<u8>, Tally>,
    /// Every directory holding a copy has its tally.
    pub tallies_complete: bool,
    /// The `.srstamps` file these results go with: the checksum of its
    /// records. Empty when no folder group needs one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stamps_id: String,
    /// The rule the stored tallies follow; other tallies are not used
    /// ([`TALLY_RULE`]).
    #[serde(default)]
    pub tally_rule: u32,
}

impl Analysis {
    /// Orders groups and folders by what they free on the whole volume.
    pub fn finish(&mut self) {
        self.format = FORMAT;
        let total = |g: &Group| g.clusters.iter().map(|c| c.free).sum::<u64>();
        self.groups
            .sort_by(|a, b| total(b).cmp(&total(a)).then(a.id.cmp(&b.id)));
    }

    /// Recomputes the per-directory tallies with `settings`.
    pub fn retally(&mut self, settings: &Settings) {
        let all = tallies(self, settings, None);
        let complete = all.len() <= TALLY_CAP;
        let mut dirs: Vec<(Vec<u8>, Tally)> = all.into_iter().collect();
        if !complete {
            dirs.sort_by_key(|(d, _)| (depth(d), d.clone()));
            dirs.truncate(TALLY_CAP);
        }
        self.tallies = dirs.into_iter().collect();
        self.tallies_complete = complete;
        self.tally_rule = TALLY_RULE;
        self.preferred_shares = settings.preferred_shares.clone();
    }

    /// The tally of directory `dir`: kept, or computed now.
    pub fn tally(&self, dir: &[u8], settings: &Settings) -> Tally {
        if let Some(t) = self.tallies.get(dir) {
            return *t;
        }
        if self.tallies_complete {
            return Tally::default();
        }
        tallies(self, settings, Some(dir))
            .remove(dir)
            .unwrap_or_default()
    }

    /// The tallies of `dir`'s child directories holding copies.
    pub fn child_tallies(&self, dir: &[u8], settings: &Settings) -> Vec<(Vec<u8>, Tally)> {
        let is_child = |d: &[u8]| {
            d.strip_prefix(dir)
                .and_then(|r| r.strip_prefix(b"/"))
                .is_some_and(|r| !r.is_empty() && !r.contains(&b'/'))
        };
        let kept: Vec<(Vec<u8>, Tally)> = self
            .tallies
            .iter()
            .filter(|(d, _)| is_child(d))
            .map(|(d, t)| (d.clone(), *t))
            .collect();
        if self.tallies_complete {
            return kept;
        }
        tallies(self, settings, None)
            .into_iter()
            .filter(|(d, _)| is_child(d))
            .collect()
    }
}

/// What the user chose: copies to keep per group, and shares whose
/// copies stay first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// By group id: the kept copies' paths (folders' for a folder group).
    #[serde(default, with = "keep_map")]
    pub keep: BTreeMap<String, Vec<Vec<u8>>>,
    #[serde(default)]
    pub preferred_shares: Vec<String>,
}

impl Settings {
    pub fn kept(&self, group: &str) -> &[Vec<u8>] {
        self.keep.get(group).map_or(&[], |v| v.as_slice())
    }
}

/// Copies, bytes freed now and bytes freed once snapshots go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    pub copies: i64,
    pub can_free: i64,
    pub frees_later: i64,
}

impl Amount {
    fn add(&mut self, other: &Amount, sign: i64) {
        self.copies += sign * other.copies;
        self.can_free += sign * other.can_free;
        self.frees_later += sign * other.frees_later;
    }
}

/// What deleting the duplicates a directory holds would do, per kind:
/// listed file groups by category, and folder groups.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub photos: Amount,
    pub videos: Amount,
    pub music: Amount,
    pub other: Amount,
    pub folders: Amount,
}

impl Tally {
    fn slot(&mut self, kind: Option<Category>) -> &mut Amount {
        match kind {
            Some(Category::Photos) => &mut self.photos,
            Some(Category::Videos) => &mut self.videos,
            Some(Category::Music) => &mut self.music,
            Some(Category::Other) => &mut self.other,
            None => &mut self.folders,
        }
    }

    /// Files of every category and folders.
    pub fn all(&self) -> Amount {
        let mut all = Amount::default();
        for a in [
            &self.photos,
            &self.videos,
            &self.music,
            &self.other,
            &self.folders,
        ] {
            all.add(a, 1);
        }
        all
    }

    pub fn files(&self, category: Option<Category>) -> Amount {
        match category {
            Some(c) => {
                let mut t = *self;
                *t.slot(Some(c))
            }
            None => {
                let mut all = self.all();
                all.add(&self.folders, -1);
                all
            }
        }
    }
}

/// Why a copy is the one kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    User,
    PreferredShare,
    /// A copy no cleanup can move: a folder right below the volume, which
    /// is a shared folder itself or lies outside every shared folder. It
    /// stays anyway, so it is the one kept.
    Unmovable,
    /// A copy inside a part another listed folder group keeps: it stays
    /// with that folder, so this group removes the others.
    InKeptFolder,
    Oldest,
    Shortest,
}

/// What happens to one copy in a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The copy that stays.
    Keep,
    /// The user kept it too.
    KeepUser,
    Remove,
    /// Another copy no cleanup can move (see [`Reason::Unmovable`]).
    Unmovable,
    /// Inside a copy of a listed identical folder: the folder group
    /// removes it, never this file group.
    InFolder,
}

/// A copy or a folder member, as the keep rule sees it.
#[derive(Debug, Clone, Copy)]
pub struct Member<'a> {
    pub names: &'a [Vec<u8>],
    pub share: &'a str,
    pub mtime: u64,
    pub recycle: bool,
    pub cluster: u32,
    /// A file copy inside a copy of a listed identical folder: that folder
    /// group removes and counts it, so its file group never does. It may
    /// still be the kept copy.
    pub in_folder: bool,
    /// In a folder: every listed folder group holding it keeps it, so it
    /// stays whatever this group does ([`Decisions`]).
    pub stays: bool,
    /// Removing it removes this path itself: a file, or a whole folder. A
    /// side of partly identical folders removes only items inside it.
    pub whole: bool,
}

/// Marks the file copies lying inside a part of a listed folder group.
pub fn mark_in_folders(members: &mut [Member<'_>], covered: &Covered) {
    for m in members {
        m.in_folder = m.names.iter().all(|n| covered.holds(n, None));
    }
}

/// Marks the members of folder group `group` that are, or lie inside, a
/// part another listed folder group removes: that group removes and
/// counts them.
pub fn mark_folder_members(members: &mut [Member<'_>], covered: &Covered, group: usize) {
    for m in members {
        m.in_folder = covered.holds(m.path(), Some(group));
    }
}

/// The keep decision of every listed group on its volume, made in this one
/// place: the lists, the totals, a group's copies and the cleanup plan all
/// read it.
///
/// A copy inside a part of another listed folder group is that group's to
/// remove (`in_folder`). It stays when every such group keeps the part
/// holding it, and then it is the copy this group keeps: the group removes
/// the copies no folder takes care of, instead of keeping one of them and
/// removing nothing.
pub struct Decisions<'a> {
    analysis: &'a Analysis,
    settings: &'a Settings,
    covered: Covered,
    /// Per folder group: its view once decided.
    folders: Vec<Decided>,
}

#[derive(Clone)]
enum Decided {
    Not,
    /// Being decided: a group that needs it back takes the copy as one
    /// that goes, so groups never keep a copy on each other's word.
    Busy,
    Done(View),
}

impl<'a> Decisions<'a> {
    pub fn new(analysis: &'a Analysis, settings: &'a Settings) -> Self {
        Decisions {
            analysis,
            settings,
            covered: Covered::of(&analysis.folders),
            folders: vec![Decided::Not; analysis.folders.len()],
        }
    }

    pub fn analysis(&self) -> &'a Analysis {
        self.analysis
    }

    /// File group `index`: its copies as the keep rule sees them, and what
    /// a cleanup of it keeps and removes.
    pub fn file(&mut self, index: usize) -> (Vec<Member<'a>>, View) {
        let a: &'a Analysis = self.analysis;
        let g = &a.groups[index];
        let mut ms = members(g);
        mark_in_folders(&mut ms, &self.covered);
        for m in &mut ms {
            m.stays = m.in_folder && m.names.iter().all(|n| self.stays(n, None));
        }
        let v = self.view(&ms, &g.clusters, &g.id);
        (ms, v)
    }

    /// Folder group `index`, as [`Decisions::file`].
    pub fn folder(&mut self, index: usize) -> (Vec<Member<'a>>, View) {
        let a: &'a Analysis = self.analysis;
        let f = &a.folders[index];
        let mut ms = folder_members(f);
        mark_folder_members(&mut ms, &self.covered, index);
        let decided = match &self.folders[index] {
            Decided::Done(v) => Some(v.clone()),
            _ => None,
        };
        if decided.is_none() {
            self.folders[index] = Decided::Busy;
        }
        for m in &mut ms {
            m.stays = m.in_folder && self.stays(m.names[0].as_slice(), Some(index));
        }
        let v = match decided {
            Some(v) => v,
            None => self.view(&ms, &f.clusters, &f.id),
        };
        self.folders[index] = Decided::Done(v.clone());
        (ms, v)
    }

    /// Whether `path`, inside parts of listed folder groups other than
    /// `except`, stays: each of them keeps the part holding it.
    fn stays(&mut self, path: &[u8], except: Option<usize>) -> bool {
        let holders = self.covered.holders(path, except);
        !holders.is_empty() && holders.into_iter().all(|(g, m)| self.member_stays(g, m))
    }

    /// Whether member `m` of folder group `g` stays.
    fn member_stays(&mut self, g: usize, m: usize) -> bool {
        let role = match &self.folders[g] {
            Decided::Busy => return false,
            Decided::Done(v) => v.roles[m],
            Decided::Not => self.folder(g).1.roles[m],
        };
        match role {
            Role::Remove => false,
            Role::InFolder => {
                let a: &'a Analysis = self.analysis;
                self.stays(&a.folders[g].members[m].path, Some(g))
            }
            Role::Keep | Role::KeepUser | Role::Unmovable => true,
        }
    }

    fn view(&self, ms: &[Member<'_>], clusters: &[Cluster], id: &str) -> View {
        view(
            ms,
            clusters,
            &self.analysis.mount,
            self.settings.kept(id),
            &self.settings.preferred_shares,
        )
    }
}

/// A copy the user kept in one group that a cleanup of another group
/// removes, or removes something inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepConflict {
    /// The group the user kept the copy in, by id and name.
    pub kept_in: (String, Vec<u8>),
    pub copy: Vec<u8>,
    /// The group whose cleanup removes it.
    pub removed_by: (String, Vec<u8>),
}

/// What a cleanup removes for one group.
#[derive(Debug, Clone)]
pub struct Removal<'a> {
    pub id: &'a str,
    pub name: &'a [u8],
    /// The paths it removes.
    pub paths: Vec<Vec<u8>>,
}

/// Every copy the user kept in a group of `analysis` that one of
/// `removing` removes: one per pair of groups, naming the first such copy,
/// so the user sees every group to take out at once. A cleanup never
/// removes a copy the user chose to keep, even for another group whose
/// copy it also is.
pub fn keep_conflicts(
    analysis: &Analysis,
    settings: &Settings,
    removing: &[Removal<'_>],
) -> Vec<KeepConflict> {
    let name_of = |id: &str| {
        analysis
            .groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .or_else(|| {
                analysis
                    .folders
                    .iter()
                    .find(|f| f.id == id)
                    .map(|f| f.name.clone())
            })
    };
    let mut conflicts: Vec<KeepConflict> = Vec::new();
    for (kept_id, copies) in &settings.keep {
        let Some(kept_name) = name_of(kept_id) else {
            continue;
        };
        for copy in copies {
            for Removal { id, name, paths } in removing {
                if id == kept_id
                    || conflicts
                        .iter()
                        .any(|c| c.kept_in.0 == *kept_id && c.removed_by.0 == *id)
                {
                    continue;
                }
                if paths
                    .iter()
                    .any(|p| p == copy || within(copy, p) || within(p, copy))
                {
                    conflicts.push(KeepConflict {
                        kept_in: (kept_id.clone(), kept_name.clone()),
                        copy: copy.clone(),
                        removed_by: (id.to_string(), name.to_vec()),
                    });
                }
            }
        }
    }
    conflicts
}

impl Member<'_> {
    pub fn path(&self) -> &[u8] {
        &self.names[0]
    }

    /// Oldest first; a copy in a recycle bin only after every other one,
    /// then the shortest path.
    fn order(&self) -> (bool, u64, usize, &[u8]) {
        (self.recycle, self.mtime, self.path().len(), self.path())
    }
}

pub fn members(group: &Group) -> Vec<Member<'_>> {
    group
        .copies
        .iter()
        .map(|c| Member {
            names: &c.names,
            share: &c.share,
            mtime: c.mtime,
            recycle: c.recycle,
            cluster: c.cluster,
            in_folder: false,
            stays: false,
            whole: true,
        })
        .collect()
}

pub fn folder_members(folder: &FolderGroup) -> Vec<Member<'_>> {
    folder
        .members
        .iter()
        .map(|m| Member {
            names: std::slice::from_ref(&m.path),
            share: &m.share,
            mtime: m.oldest,
            recycle: m.recycle,
            cluster: m.cluster,
            in_folder: false,
            stays: false,
            whole: !folder.partial(),
        })
        .collect()
}

pub fn folder_clusters(folder: &FolderGroup) -> Vec<Cluster> {
    folder.clusters.clone()
}

/// Whether `path` lies below directory `scope` (not `scope` itself).
pub fn within(path: &[u8], scope: &[u8]) -> bool {
    path.strip_prefix(scope)
        .is_some_and(|rest| rest.first() == Some(&b'/') && rest.len() > 1)
}

/// One group on its volume: what a cleanup of it keeps and removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub keeper: usize,
    pub reason: Reason,
    pub roles: Vec<Role>,
    pub can_free: u64,
    pub frees_later: u64,
    pub removes: usize,
}

/// Whether no cleanup can move member `m` of a group on `mount`: a folder
/// right below the volume is a shared folder itself, or lies outside
/// every shared folder (which has no Recycle Bin).
pub fn unmovable(m: &Member<'_>, mount: &[u8]) -> bool {
    m.whole
        && m.names.iter().any(|n| {
            n.strip_prefix(mount)
                .and_then(|r| r.strip_prefix(b"/"))
                .is_some_and(|r| !r.is_empty() && !r.contains(&b'/'))
        })
}

/// The keep rule (docs/duplicates.md): a copy the user kept, else the
/// first preferred share's, else one no cleanup can move, else one
/// another listed folder group keeps, else the oldest; ties go to the
/// shortest path. Every copy the user kept stays
/// and every other copy on the volume goes, but those no cleanup can
/// move. A group is always seen on its whole volume: a folder only picks
/// which groups are listed. A cluster frees its bytes only when every
/// copy in it goes.
pub fn view(
    members: &[Member<'_>],
    clusters: &[Cluster],
    mount: &[u8],
    kept: &[Vec<u8>],
    preferred: &[String],
) -> View {
    let fixed: Vec<bool> = members.iter().map(|m| unmovable(m, mount)).collect();
    let user: Vec<bool> = members
        .iter()
        .map(|m| kept.iter().any(|k| k == m.path()))
        .collect();
    let best = |pick: &dyn Fn(usize) -> bool| -> Option<usize> {
        (0..members.len())
            .filter(|&i| pick(i))
            .min_by(|&a, &b| members[a].order().cmp(&members[b].order()))
    };
    let preferred_rank = |i: usize| preferred.iter().position(|s| s == members[i].share);
    let (keeper, reason) = if let Some(k) = best(&|i| user[i]) {
        (k, Reason::User)
    } else if let Some(k) = (0..members.len())
        .filter(|&i| preferred_rank(i).is_some())
        .min_by(|&a, &b| {
            (preferred_rank(a), members[a].order()).cmp(&(preferred_rank(b), members[b].order()))
        })
    {
        (k, Reason::PreferredShare)
    } else if let Some(k) = best(&|i| fixed[i] && !members[i].in_folder) {
        (k, Reason::Unmovable)
    } else if let Some(k) = best(&|i| members[i].in_folder && members[i].stays) {
        (k, Reason::InKeptFolder)
    } else {
        // A copy in a folder that goes is never the one kept while
        // another can be.
        let k = best(&|i| !members[i].in_folder)
            .or_else(|| best(&|_| true))
            .expect("a group has copies");
        let tied = members
            .iter()
            .filter(|m| (m.recycle, m.mtime) == (members[k].recycle, members[k].mtime))
            .count();
        (
            k,
            if tied > 1 {
                Reason::Shortest
            } else {
                Reason::Oldest
            },
        )
    };
    let roles: Vec<Role> = (0..members.len())
        .map(|i| {
            if i == keeper {
                Role::Keep
            } else if user[i] {
                Role::KeepUser
            } else if members[i].in_folder {
                Role::InFolder
            } else if fixed[i] {
                Role::Unmovable
            } else {
                Role::Remove
            }
        })
        .collect();
    let (mut can_free, mut frees_later) = (0, 0);
    for (c, cluster) in clusters.iter().enumerate() {
        if (0..members.len())
            .filter(|&i| members[i].cluster as usize == c)
            .all(|i| roles[i] == Role::Remove)
        {
            can_free += cluster.free;
            frees_later += cluster.later;
        }
    }
    View {
        keeper,
        reason,
        can_free,
        frees_later,
        removes: roles.iter().filter(|&&r| r == Role::Remove).count(),
        roles,
    }
}

/// Per-directory tallies, every directory or only `only`. A group is
/// always cleaned on its whole volume ([`view`]): its outcome counts once
/// at every directory holding one of its copies, from the copy's folder
/// up to the volume. Directories therefore overlap, and the volume's
/// tally is the sum of its groups.
fn tallies(
    analysis: &Analysis,
    settings: &Settings,
    only: Option<&[u8]>,
) -> HashMap<Vec<u8>, Tally> {
    let mount = analysis.mount.as_slice();
    let mut out: HashMap<Vec<u8>, Tally> = HashMap::new();
    let mut group = |members: &[Member<'_>], v: &View, kind: Option<Category>| {
        let amount = Amount {
            copies: v.removes as i64,
            can_free: v.can_free as i64,
            frees_later: v.frees_later as i64,
        };
        let mut dirs: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
        for name in members.iter().flat_map(|m| m.names.iter()) {
            let mut dir = parent(name);
            while dir.len() >= mount.len() && (dir == mount || within(dir, mount)) {
                if !dirs.insert(dir) {
                    break;
                }
                if dir == mount {
                    break;
                }
                dir = parent(dir);
            }
        }
        for dir in dirs {
            if only.is_none_or(|o| o == dir) {
                out.entry(dir.to_vec())
                    .or_default()
                    .slot(kind)
                    .add(&amount, 1);
            }
        }
    };
    let mut decisions = Decisions::new(analysis, settings);
    for (i, g) in analysis
        .groups
        .iter()
        .enumerate()
        .filter(|(_, g)| listed(g))
    {
        let (ms, v) = decisions.file(i);
        group(&ms, &v, Some(g.category));
    }
    for (i, _) in analysis
        .folders
        .iter()
        .enumerate()
        .filter(|(_, f)| f.listed())
    {
        let (ms, v) = decisions.folder(i);
        group(&ms, &v, None);
    }
    out.retain(|_, t| *t != Tally::default());
    out
}

/// Whether a file group is listed on its own.
pub fn listed(group: &Group) -> bool {
    group.size >= LIST_MIN_SIZE && !group.in_folders && group.clusters.len() > 1
}

/// The share of `path`: its first component below `mount`.
pub fn share_of(path: &[u8], mount: &[u8]) -> String {
    let rest = path
        .strip_prefix(mount)
        .and_then(|r| r.strip_prefix(b"/"))
        .unwrap_or(b"");
    let share = rest.split(|&b| b == b'/').next().unwrap_or(b"");
    String::from_utf8_lossy(share).into_owned()
}

fn has_component(path: &[u8], name: &[u8]) -> bool {
    path.split(|&b| b == b'/').any(|part| part == name)
}

/// Synology Drive keeps a user's files in `homes/<user>/Drive`.
fn in_drive(path: &[u8], mount: &[u8]) -> bool {
    let rest = path
        .strip_prefix(mount)
        .and_then(|r| r.strip_prefix(b"/"))
        .unwrap_or(b"");
    let parts: Vec<&[u8]> = rest.split(|&b| b == b'/').collect();
    parts.len() > 3 && parts[0] == b"homes" && parts[2] == b"Drive"
}

pub fn last_name(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}

pub fn parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[..i],
        None => b"",
    }
}

fn depth(path: &[u8]) -> usize {
    path.iter().filter(|&&b| b == b'/').count()
}

pub(crate) mod hex_list {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
        let hex: Vec<String> = v
            .iter()
            .map(|b| crate::report::hex_bytes::encode(b))
            .collect();
        hex.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|h| {
                crate::report::hex_bytes::decode(h)
                    .ok_or_else(|| serde::de::Error::custom("invalid hex"))
            })
            .collect()
    }
}

mod keep_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(
        v: &BTreeMap<String, Vec<Vec<u8>>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let hex: BTreeMap<&String, Vec<String>> = v
            .iter()
            .map(|(k, paths)| {
                (
                    k,
                    paths
                        .iter()
                        .map(|p| crate::report::hex_bytes::encode(p))
                        .collect(),
                )
            })
            .collect();
        hex.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<String, Vec<Vec<u8>>>, D::Error> {
        BTreeMap::<String, Vec<String>>::deserialize(d)?
            .into_iter()
            .map(|(k, paths)| {
                let paths = paths
                    .iter()
                    .map(|h| {
                        crate::report::hex_bytes::decode(h)
                            .ok_or_else(|| serde::de::Error::custom("invalid hex"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((k, paths))
            })
            .collect()
    }
}

mod tally_map {
    use super::Tally;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(v: &BTreeMap<Vec<u8>, Tally>, s: S) -> Result<S::Ok, S::Error> {
        let hex: BTreeMap<String, &Tally> = v
            .iter()
            .map(|(k, t)| (crate::report::hex_bytes::encode(k), t))
            .collect();
        hex.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<Vec<u8>, Tally>, D::Error> {
        BTreeMap::<String, Tally>::deserialize(d)?
            .into_iter()
            .map(|(k, t)| {
                crate::report::hex_bytes::decode(&k)
                    .map(|k| (k, t))
                    .ok_or_else(|| serde::de::Error::custom("invalid hex"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn copy(path: &str, mtime: u64, cluster: u32) -> CopyRecord {
        CopyRecord::new(
            vec![path.as_bytes().to_vec()],
            b"/v",
            mtime,
            256,
            mtime,
            1,
            cluster,
        )
    }

    fn group(id: &str, size: u64, copies: Vec<CopyRecord>, frees: &[u64]) -> Group {
        Group {
            id: id.into(),
            name: last_name(copies[0].path()).to_vec(),
            size,
            category: Category::of(copies[0].path()),
            copies,
            clusters: frees
                .iter()
                .map(|&free| Cluster {
                    free,
                    later: 0,
                    ..Cluster::default()
                })
                .collect(),
            in_folders: false,
        }
    }

    fn analysis(groups: Vec<Group>) -> Analysis {
        Analysis {
            mount: b"/v".to_vec(),
            groups,
            ..Analysis::default()
        }
    }

    #[test]
    fn categories_come_from_the_extension() {
        assert_eq!(Category::of(b"IMG_1.HEIC"), Category::Photos);
        assert_eq!(Category::of(b"a.MOV"), Category::Videos);
        assert_eq!(Category::of(b"song.flac"), Category::Music);
        assert_eq!(Category::of(b"disk.vmdk"), Category::Other);
        assert_eq!(Category::of(b"noext"), Category::Other);
    }

    #[test]
    fn the_oldest_copy_stays_when_every_copy_is_in_scope() {
        let g = group(
            "g",
            20 * MIB,
            vec![
                copy("/v/a/x.iso", 30, 0),
                copy("/v/b/x.iso", 10, 1),
                copy("/v/c/x.iso", 20, 2),
            ],
            &[20 * MIB, 20 * MIB, 20 * MIB],
        );
        let v = view(&members(&g), &g.clusters, b"/v", &[], &[]);
        assert_eq!((v.keeper, v.reason), (1, Reason::Oldest));
        assert_eq!(v.roles, [Role::Remove, Role::Keep, Role::Remove]);
        assert_eq!((v.can_free, v.removes), (40 * MIB, 2));
    }

    /// A group is cleaned on its whole volume: one copy stays, the
    /// copies in every other shared folder go too.
    #[test]
    fn every_copy_but_the_kept_one_goes_whatever_its_shared_folder() {
        let g = group(
            "g",
            20 * MIB,
            vec![
                copy("/v/a/x", 30, 0),
                copy("/v/a/y", 40, 1),
                copy("/v/b/x", 50, 2),
            ],
            &[MIB, 2 * MIB, 4 * MIB],
        );
        let v = view(&members(&g), &g.clusters, b"/v", &[], &[]);
        assert_eq!((v.keeper, v.reason), (0, Reason::Oldest));
        assert_eq!(v.roles, [Role::Keep, Role::Remove, Role::Remove]);
        assert_eq!(v.can_free, 6 * MIB);
    }

    /// A folder right below the volume (a shared folder itself, or one
    /// outside every shared folder) cannot move: it is the copy kept, and
    /// every other copy goes.
    #[test]
    fn a_folder_right_below_the_volume_stays_and_is_the_one_kept() {
        let member = |path: &str, oldest: u64, cluster: u32| {
            let mut m = FolderMember::new(path.as_bytes(), b"/v", 3, 25 * MIB);
            m.oldest = oldest;
            m.cluster = cluster;
            m
        };
        let f = FolderGroup {
            id: "ftop".into(),
            name: b"x".to_vec(),
            files: 3,
            bytes: 25 * MIB,
            covered: 0,
            members: vec![
                member("/v/a/x", 1, 0),
                member("/v/upgrade_test", 9, 1),
                member("/v/b/x", 5, 2),
                member("/v/c", 7, 3),
            ],
            clusters: vec![
                Cluster {
                    free: 25 * MIB,
                    ..Cluster::default()
                };
                4
            ],
            items: Vec::new(),
            in_folders: false,
            stamps: None,
        };
        let v = view(&folder_members(&f), &f.clusters, b"/v", &[], &[]);
        assert_eq!((v.keeper, v.reason), (3, Reason::Unmovable));
        assert_eq!(
            v.roles,
            [Role::Remove, Role::Unmovable, Role::Remove, Role::Keep]
        );
        assert_eq!((v.removes, v.can_free), (2, 50 * MIB));
        // A copy the user kept still comes first; the unmovable ones stay.
        let kept = vec![b"/v/a/x".to_vec()];
        let v = view(&folder_members(&f), &f.clusters, b"/v", &kept, &[]);
        assert_eq!(
            v.roles,
            [Role::Keep, Role::Unmovable, Role::Remove, Role::Unmovable]
        );
    }

    #[test]
    fn user_keeps_and_preferred_shares_come_before_age() {
        let g = group(
            "g",
            20 * MIB,
            vec![
                copy("/v/photo/x", 30, 0),
                copy("/v/archive/x", 10, 1),
                copy("/v/archive/y", 20, 2),
            ],
            &[MIB, MIB, MIB],
        );
        let pref = vec!["photo".to_string()];
        let v = view(&members(&g), &g.clusters, b"/v", &[], &pref);
        assert_eq!((v.keeper, v.reason), (0, Reason::PreferredShare));
        let kept = vec![b"/v/archive/y".to_vec()];
        let v = view(&members(&g), &g.clusters, b"/v", &kept, &pref);
        assert_eq!((v.keeper, v.reason), (2, Reason::User));
        assert_eq!(v.roles, [Role::Remove, Role::Remove, Role::Keep]);
    }

    #[test]
    fn a_clone_frees_nothing_until_its_twin_goes_too() {
        // Copies 0 and 1 share extents (cluster 0); copy 2 is its own.
        let g = group(
            "g",
            20 * MIB,
            vec![
                copy("/v/a/x", 10, 0),
                copy("/v/a/y", 20, 0),
                copy("/v/b/x", 5, 1),
            ],
            &[8 * MIB, 8 * MIB],
        );
        let v = view(&members(&g), &g.clusters, b"/v", &[], &[]);
        assert_eq!(v.keeper, 2);
        assert_eq!(v.can_free, 8 * MIB);
        let kept = vec![b"/v/a/x".to_vec()];
        let v = view(&members(&g), &g.clusters, b"/v", &kept, &[]);
        // Removing only the clone and the other copy frees the other copy.
        assert_eq!(v.roles, [Role::Keep, Role::Remove, Role::Remove]);
        assert_eq!(v.can_free, 8 * MIB);
    }

    #[test]
    fn a_copy_in_a_recycle_bin_is_kept_only_when_nothing_else_is() {
        let g = group(
            "g",
            20 * MIB,
            vec![copy("/v/a/#recycle/x", 1, 0), copy("/v/a/x", 9, 1)],
            &[MIB, MIB],
        );
        let v = view(&members(&g), &g.clusters, b"/v", &[], &[]);
        assert_eq!(v.keeper, 1);
    }

    /// A directory's tally is the volume view of every group with a copy
    /// below it.
    #[test]
    fn directory_tallies_add_the_groups_with_a_copy_below() {
        let groups = vec![
            group(
                "a",
                20 * MIB,
                vec![
                    copy("/v/s1/d/x.mp4", 30, 0),
                    copy("/v/s1/e/x.mp4", 40, 1),
                    copy("/v/s2/x.mp4", 50, 2),
                ],
                &[MIB, 2 * MIB, 4 * MIB],
            ),
            group(
                "b",
                30 * MIB,
                vec![
                    copy("/v/s1/d/y.bin", 9, 0),
                    copy("/v/s1/d/z.bin", 8, 0),
                    copy("/v/s2/y.bin", 7, 1),
                ],
                &[16 * MIB, 32 * MIB],
            ),
            // Too small to list.
            group(
                "c",
                MIB,
                vec![copy("/v/s1/q", 1, 0), copy("/v/s2/q", 2, 1)],
                &[MIB, MIB],
            ),
        ];
        for settings in [
            Settings::default(),
            Settings {
                preferred_shares: vec!["s1".into()],
                ..Settings::default()
            },
            Settings {
                keep: BTreeMap::from([("a".into(), vec![b"/v/s1/e/x.mp4".to_vec()])]),
                ..Settings::default()
            },
        ] {
            let mut a = analysis(groups.clone());
            a.retally(&settings);
            for scope in ["/v", "/v/s1", "/v/s1/d", "/v/s1/e", "/v/s2"] {
                let mut expected = Amount::default();
                for g in a.groups.iter().filter(|g| listed(g)) {
                    if !g
                        .copies
                        .iter()
                        .any(|c| c.names.iter().any(|n| within(n, scope.as_bytes())))
                    {
                        continue;
                    }
                    let v = view(
                        &members(g),
                        &g.clusters,
                        b"/v",
                        settings.kept(&g.id),
                        &settings.preferred_shares,
                    );
                    expected.copies += v.removes as i64;
                    expected.can_free += v.can_free as i64;
                }
                let got = a.tally(scope.as_bytes(), &settings).all();
                assert_eq!(
                    (got.copies, got.can_free),
                    (expected.copies, expected.can_free),
                    "{scope} with {settings:?}"
                );
            }
        }
    }

    /// A file copy inside a copy of a listed identical folder is the
    /// folder group's: totals count its bytes once, as the set the
    /// cleanup plan would delete frees them.
    #[test]
    fn files_inside_identical_folders_count_once() {
        let mut a = analysis(vec![group(
            "g",
            20 * MIB,
            vec![
                copy("/v/photo/trip/a.mov", 10, 0),
                copy("/v/old/trip/a.mov", 20, 1),
                copy("/v/archive/a.mov", 30, 2),
            ],
            &[20 * MIB, 20 * MIB, 20 * MIB],
        )]);
        let member = |path: &str, oldest: u64, cluster: u32| {
            let mut m = FolderMember::new(path.as_bytes(), b"/v", 3, 25 * MIB);
            m.oldest = oldest;
            m.cluster = cluster;
            m
        };
        a.folders.push(FolderGroup {
            id: "ffolder".into(),
            name: b"trip".to_vec(),
            files: 3,
            bytes: 25 * MIB,
            covered: 0,
            members: vec![member("/v/old/trip", 5, 0), member("/v/photo/trip", 1, 1)],
            clusters: vec![
                Cluster {
                    free: 25 * MIB,
                    ..Cluster::default()
                },
                Cluster {
                    free: 25 * MIB,
                    ..Cluster::default()
                },
            ],
            items: Vec::new(),
            in_folders: false,
            stamps: None,
        });
        let settings = Settings::default();
        a.retally(&settings);
        // The folder group keeps photo/trip and removes old/trip (25 MiB);
        // the file group removes only archive/a.mov (20 MiB), never a.mov
        // inside the folders.
        let total = a.tally(b"/v", &settings).all();
        assert_eq!((total.copies, total.can_free), (2, 45 * MIB as i64));
        // photo/trip/a.mov stays with the folder the folder group keeps.
        let (_, v) = Decisions::new(&a, &settings).file(0);
        assert_eq!(v.roles, [Role::Keep, Role::InFolder, Role::Remove]);
        assert_eq!((v.keeper, v.reason), (0, Reason::InKeptFolder));
        assert_eq!(v.can_free, 20 * MIB);
        // Each directory adds the groups with a copy below it.
        let fv = view(
            &folder_members(&a.folders[0]),
            &a.folders[0].clusters,
            b"/v",
            &[],
            &[],
        );
        let below =
            |paths: &[&[u8]], scope: &str| paths.iter().any(|p| within(p, scope.as_bytes()));
        for scope in ["/v", "/v/photo", "/v/old", "/v/old/trip", "/v/archive"] {
            let mut expected = Amount::default();
            let folder_here = below(&[b"/v/old/trip", b"/v/photo/trip"], scope);
            let files_here = below(
                &[
                    b"/v/photo/trip/a.mov",
                    b"/v/old/trip/a.mov",
                    b"/v/archive/a.mov",
                ],
                scope,
            );
            for (here, v) in [(folder_here, &fv), (files_here, &v)] {
                if here {
                    expected.copies += v.removes as i64;
                    expected.can_free += v.can_free as i64;
                }
            }
            let got = a.tally(scope.as_bytes(), &settings).all();
            assert_eq!(
                (got.copies, got.can_free),
                (expected.copies, expected.can_free),
                "{scope}"
            );
        }
    }

    /// A folder group of `paths`, each 37 MiB and its own cluster, all of
    /// one age; partly identical folders with `items`.
    fn folder(id: &str, paths: &[&str], items: &[&str]) -> FolderGroup {
        FolderGroup {
            id: id.into(),
            name: last_name(paths[0].as_bytes()).to_vec(),
            files: 19,
            bytes: 37 * MIB,
            covered: 0,
            members: paths
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    let mut m = FolderMember::new(p.as_bytes(), b"/v", 19, 37 * MIB);
                    m.oldest = 1;
                    m.cluster = k as u32;
                    if !items.is_empty() {
                        m.side = Some(Side {
                            items: vec![(0, 0); items.len()],
                            ..Side::default()
                        });
                    }
                    m
                })
                .collect(),
            clusters: vec![
                Cluster {
                    free: 37 * MIB,
                    ..Cluster::default()
                };
                paths.len()
            ],
            items: items
                .iter()
                .map(|r| Item {
                    rel: r.as_bytes().to_vec(),
                    folder: true,
                    files: 1,
                    bytes: MIB,
                })
                .collect(),
            in_folders: false,
            stamps: None,
        }
    }

    /// A folder set in three places, two of them inside identical parent
    /// folders: the parent group keeps one copy, so this group keeps that
    /// copy and removes the third, which no other group takes care of.
    /// Keeping the third instead (the shortest path) removed nothing and
    /// left the group off every list.
    #[test]
    fn a_copy_another_group_keeps_is_the_one_kept() {
        let mut a = analysis(Vec::new());
        a.folders = vec![
            folder("fphones", &["/v/main/Phones", "/v/archive/phones"], &[]),
            folder(
                "falice",
                &[
                    "/v/archive/phones/phone-alice",
                    "/v/main/Phones/phone-alice",
                    "/v/media/phone-alice",
                ],
                &[],
            ),
        ];
        let settings = Settings::default();
        {
            let mut d = Decisions::new(&a, &settings);
            let (_, phones) = d.folder(0);
            assert_eq!(phones.roles, [Role::Keep, Role::Remove]);
            let (ms, alice) = d.folder(1);
            assert_eq!((alice.keeper, alice.reason), (1, Reason::InKeptFolder));
            assert_eq!(alice.roles, [Role::InFolder, Role::Keep, Role::Remove]);
            assert_eq!((alice.removes, alice.can_free), (1, 37 * MIB));
            // The copy in the removed parent goes with it, never stays.
            assert!(!ms[0].stays && ms[1].stays && !ms[2].stays);
        }
        // The totals come from the same decisions.
        a.retally(&settings);
        let total = a.tally(b"/v", &settings).all();
        assert_eq!((total.copies, total.can_free), (2, 74 * MIB as i64));
        let media = a.tally(b"/v/media", &settings).all();
        assert_eq!((media.copies, media.can_free), (1, 37 * MIB as i64));
    }

    /// Kernel trees: two exports are identical folders; a clone and an
    /// export with `.env` are each partly identical to them. Each row keeps
    /// the export the identical folder keeps and removes its own side; the
    /// clone's row kept the clone (the shortest path) and removed nothing.
    #[test]
    fn partly_identical_folders_keep_the_copy_their_identical_folder_keeps() {
        let exports = [
            "/v/archive/src/linux-6.6-copy-junk",
            "/v/media/src/linux-6.6-copy",
        ];
        let mut a = analysis(Vec::new());
        a.folders = vec![
            folder("fexports", &exports, &[]),
            folder(
                "fclone",
                &[exports[0], "/v/main/src/linux-6.6", exports[1]],
                &["arch", "drivers"],
            ),
            folder(
                "fenv",
                &[exports[0], "/v/main/src/linux-6.6-copy-env", exports[1]],
                &["arch", "drivers"],
            ),
        ];
        let settings = Settings::default();
        {
            let mut d = Decisions::new(&a, &settings);
            let (_, both) = d.folder(0);
            assert_eq!(both.roles, [Role::Remove, Role::Keep]);
            for i in [1, 2] {
                let (_, row) = d.folder(i);
                assert_eq!((row.keeper, row.reason), (2, Reason::InKeptFolder), "{i}");
                assert_eq!(row.roles, [Role::InFolder, Role::Remove, Role::Keep]);
                assert_eq!((row.removes, row.can_free), (1, 37 * MIB));
            }
        }
        a.retally(&settings);
        let total = a.tally(b"/v", &settings).all();
        assert_eq!((total.copies, total.can_free), (3, 3 * 37 * MIB as i64));
        // The user's keep still comes first: keeping the archive export
        // removes the media one, and the rows then keep the archive one.
        let kept = Settings {
            keep: BTreeMap::from([("fexports".into(), vec![exports[0].as_bytes().to_vec()])]),
            ..Settings::default()
        };
        let mut d = Decisions::new(&a, &kept);
        assert_eq!(d.folder(0).1.roles, [Role::Keep, Role::Remove]);
        let (_, row) = d.folder(1);
        assert_eq!((row.keeper, row.reason), (0, Reason::InKeptFolder));
        assert_eq!(row.roles, [Role::Keep, Role::Remove, Role::InFolder]);
    }

    /// The user kept the media export in the `.env` row and the archive
    /// one in the exports: the exports' cleanup would remove the copy the
    /// row keeps. That is a conflict naming both groups, never a silent
    /// removal; without the row's keep there is none.
    #[test]
    fn a_copy_the_user_kept_in_one_group_that_another_removes_conflicts() {
        let exports = [
            "/v/archive/src/linux-6.6-copy-junk",
            "/v/media/src/linux-6.6-copy",
        ];
        let mut a = analysis(Vec::new());
        a.folders = vec![
            folder("fexports", &exports, &[]),
            folder(
                "fenv",
                &[exports[0], "/v/main/src/linux-6.6-copy-env", exports[1]],
                &["arch", "drivers"],
            ),
        ];
        let settings = Settings {
            keep: BTreeMap::from([
                ("fexports".into(), vec![exports[0].as_bytes().to_vec()]),
                ("fenv".into(), vec![exports[1].as_bytes().to_vec()]),
            ]),
            ..Settings::default()
        };
        let mut d = Decisions::new(&a, &settings);
        let (_, v) = d.folder(0);
        assert_eq!(v.roles, [Role::Keep, Role::Remove]);
        let removes = |v: &View, f: &FolderGroup| -> Vec<Vec<u8>> {
            f.members
                .iter()
                .zip(&v.roles)
                .filter(|(_, r)| **r == Role::Remove)
                .flat_map(|(m, _)| f.parts(m).into_iter().map(|(p, _, _)| p))
                .collect()
        };
        let exports_go = removes(&v, &a.folders[0]);
        let removing = [Removal {
            id: "fexports",
            name: b"linux-6.6-copy-junk",
            paths: exports_go,
        }];
        let conflicts = keep_conflicts(&a, &settings, &removing);
        assert_eq!(conflicts.len(), 1);
        let c = &conflicts[0];
        assert_eq!(c.kept_in.0, "fenv");
        assert_eq!(c.copy, exports[1].as_bytes());
        assert_eq!(c.removed_by.0, "fexports");
        // The row's own removal is not a conflict with its own keep.
        let (_, row) = d.folder(1);
        let row_goes = removes(&row, &a.folders[1]);
        assert!(!row_goes.is_empty());
        let removing = [Removal {
            id: "fenv",
            name: b"linux-6.6-copy-env",
            paths: row_goes,
        }];
        assert_eq!(keep_conflicts(&a, &settings, &removing), []);
        let only_exports = Settings {
            keep: BTreeMap::from([("fexports".into(), vec![exports[0].as_bytes().to_vec()])]),
            ..Settings::default()
        };
        let removing = [Removal {
            id: "fexports",
            name: b"linux-6.6-copy-junk",
            paths: vec![exports[1].as_bytes().to_vec()],
        }];
        assert_eq!(keep_conflicts(&a, &only_exports, &removing), []);
        // Two groups removing the kept copy: both are named at once, each
        // pair once however many of its paths conflict.
        let removing = [
            Removal {
                id: "fexports",
                name: b"linux-6.6-copy-junk",
                paths: vec![
                    exports[1].as_bytes().to_vec(),
                    format!("{}/drivers", exports[1]).into_bytes(),
                ],
            },
            Removal {
                id: "fscsi",
                name: b"scsi",
                paths: vec![format!("{}/drivers/scsi", exports[1]).into_bytes()],
            },
        ];
        let conflicts = keep_conflicts(&a, &settings, &removing);
        let pairs: Vec<(&str, &str)> = conflicts
            .iter()
            .map(|c| (c.kept_in.0.as_str(), c.removed_by.0.as_str()))
            .collect();
        assert_eq!(pairs, [("fenv", "fexports"), ("fenv", "fscsi")]);
    }

    #[test]
    fn analyses_round_trip_through_json() {
        let mut a = analysis(vec![group(
            "g",
            20 * MIB,
            vec![copy("/v/a/x", 1, 0), copy("/v/b/x", 2, 1)],
            &[MIB, MIB],
        )]);
        a.retally(&Settings::default());
        let json = serde_json::to_vec(&a).unwrap();
        assert_eq!(serde_json::from_slice::<Analysis>(&json).unwrap(), a);
        let s = Settings {
            keep: BTreeMap::from([("g".into(), vec![b"/v/\xffx".to_vec()])]),
            preferred_shares: vec!["a".into()],
        };
        let json = serde_json::to_vec(&s).unwrap();
        assert_eq!(serde_json::from_slice::<Settings>(&json).unwrap(), s);
    }
}
