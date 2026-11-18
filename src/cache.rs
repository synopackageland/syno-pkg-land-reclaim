//! The disposable unit cache (SPEC 12): sealed scan units of one
//! filesystem, kept as immutable bundles under `cache/<fsid>/bundles`
//! and named by a small manifest that is replaced atomically. All of it
//! may be deleted: a miss costs a rescan, never a different answer.

use crate::btrfs::walk::{StopReason, WalkError};
use crate::codec::{self, Fnv};
use crate::document::{CacheStatus, CacheUnit};
use crate::scan::UnitCache;
use crate::state::{self, StateError};
use crate::units::UnitKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Bumped when the bundle or manifest layout changes, or what a unit
/// records does (SPEC 12): 2 keeps a link per name and drops the
/// shared-leaf bound of subvolumes no snapshot shares; 3 flags where
/// package data starts; 4 excludes this app state from folder display subsets.
pub const FORMAT: u32 = 4;
const MANIFEST: &str = "manifest";
const METADATA: &str = "metadata";
const BUNDLES: &str = "bundles";

/// Oldest reusable writable unit, in seconds (SPEC 12 initial default).
pub const WRITABLE_MAX_AGE: u64 = 5 * 60;

/// DSM builds whose generations `scripts/dsm-mutation.sh` showed to
/// advance on every kind of namespace change (SPEC 12): 90069 is DSM
/// 7.4 (2026-09-28, 15 kinds, commit interval about 30 s).
pub const VALIDATED_DSM_BUILDS: &[u32] = &[90069];

/// Where DSM records its version.
pub const DSM_VERSION_FILE: &str = "/etc.defaults/VERSION";

/// The build number in the text of DSM's version file.
pub fn dsm_build(version: &str) -> Option<u32> {
    version.lines().find_map(|line| {
        let value = line.strip_prefix("buildnumber=")?;
        value.strip_prefix('"')?.strip_suffix('"')?.parse().ok()
    })
}

/// Whether writable units may be reused on this platform: always on
/// upstream kernels (`None`), only on validated DSM builds.
pub fn trusts_writable_generations(dsm_build: Option<u32>) -> bool {
    dsm_build.is_none_or(|build| VALIDATED_DSM_BUILDS.contains(&build))
}

/// When a sealed unit may stand in for a fresh read (SPEC 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reuse {
    /// Oldest capture, in seconds, of a reusable writable unit.
    pub writable_max_age: u64,
    /// Whether this platform's generations were validated to advance on
    /// every namespace change; if not, writable units are always read.
    pub trust_writable: bool,
}

impl Reuse {
    /// The policy on the running system, which may be a DSM build.
    pub fn of_this_platform() -> Reuse {
        let dsm = fs::read_to_string(DSM_VERSION_FILE).ok();
        Reuse {
            writable_max_age: WRITABLE_MAX_AGE,
            trust_writable: trusts_writable_generations(dsm.as_deref().and_then(dsm_build)),
        }
    }

    /// Whether the unit of `key`, read at unix time `captured`, may stand
    /// in for a fresh read at `now`.
    fn allows(&self, key: &UnitKey, captured: u64, now: u64) -> bool {
        if now < captured {
            return false;
        }
        key.readonly || (self.trust_writable && now - captured <= self.writable_max_age)
    }
}

/// Disk the cache may use (SPEC 22.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Bytes of sealed bundles kept (`--max-cache`).
    pub max_bytes: u64,
    /// Bytes left available on the state filesystem (`--min-free`).
    pub min_free: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("{available} bytes available on the state filesystem, below --min-free {min_free}")]
    NoSpace { available: u64, min_free: u64 },
}

/// Where a sealed unit's files went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sealed {
    Kept(PathBuf),
    /// Past `--max-cache`: the files stay in staging, not kept.
    OverBudget,
}

impl Sealed {
    pub fn kept(self) -> Option<PathBuf> {
        match self {
            Sealed::Kept(dir) => Some(dir),
            Sealed::OverBudget => None,
        }
    }
}

/// A bundle's own record of its key and files.
#[derive(Debug, Serialize, Deserialize)]
struct Metadata {
    format: u32,
    key: UnitKey,
    captured: u64,
    files: Vec<FileSum>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileSum {
    name: String,
    bytes: u64,
    checksum: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    fsid: String,
    bundles: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    key: UnitKey,
    /// Unix seconds when the unit's namespace was read.
    captured: u64,
    bytes: u64,
}

/// The cache of one filesystem, opened by the holder of its writer lock.
#[derive(Debug)]
pub struct Cache {
    dir: PathBuf,
    manifest: Manifest,
    reuse: Reuse,
    budget: Budget,
    /// Bundles this run found or sealed: never evicted by it.
    used: BTreeSet<String>,
    available: fn(&Path) -> io::Result<u64>,
}

impl Cache {
    /// Opens `root/<fsid>`, creating it if missing.
    pub fn open(
        root: &Path,
        fsid: &str,
        uid: u32,
        reuse: Reuse,
        budget: Budget,
    ) -> Result<Cache, StateError> {
        let dir = root.join(fsid);
        for d in [root, &dir, &dir.join(BUNDLES)] {
            state::private_dir(d, uid)?;
        }
        let manifest = Manifest {
            format: FORMAT,
            fsid: fsid.to_string(),
            bundles: Vec::new(),
        };
        let manifest = read_manifest(&dir.join(MANIFEST)).unwrap_or(manifest);
        let cache = Cache {
            dir,
            manifest,
            reuse,
            budget,
            used: BTreeSet::new(),
            available: crate::space::available,
        };
        cache.recover()?;
        Ok(cache)
    }

    pub(crate) fn checkpoint_manifest_valid(root: &Path, fsid: &str) -> bool {
        let path = root.join(fsid).join(MANIFEST);
        !path.exists() || read_manifest(&path).is_some()
    }

    /// Read saved identities without opening/recovering the cache or changing
    /// any checkpoint. Payload checksums are still rechecked by the worker.
    pub(crate) fn checkpoint_keys(root: &Path, fsid: &str) -> Result<Vec<UnitKey>, WalkError> {
        let dir = root.join(fsid);
        let invalid = |reason: &str| WalkError::Checkpoint(reason.into());
        let path = dir.join(MANIFEST);
        let mut keys = if path.exists() {
            let manifest = read_manifest(&path)
                .ok_or_else(|| invalid("damaged or incompatible completed-unit manifest"))?;
            if manifest.fsid != fsid {
                return Err(invalid("checkpoint filesystem identity changed"));
            }
            let mut saved = Vec::new();
            for entry in manifest.bundles {
                if !is_intact(&dir.join(BUNDLES).join(&entry.name), &entry.key) {
                    return Err(invalid(
                        "a completed subvolume checkpoint is missing or damaged",
                    ));
                }
                saved.push(entry.key);
            }
            saved
        } else {
            Vec::new()
        };
        for namespace in namespace_paths(&dir)? {
            let active = namespace.with_extension("active");
            if !active.is_file() {
                return Err(invalid("namespace checkpoint identity is missing"));
            }
            let key: UnitKey = fs::read(&active)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or_else(|| invalid("damaged namespace checkpoint identity"))?;
            if !namespace.is_file() {
                return Err(invalid(
                    "the previous process did not finish saving its namespace",
                ));
            }
            if crate::model::namespace::Namespace::checkpoint_key(&namespace)? != key {
                return Err(invalid("namespace checkpoint identity is damaged"));
            }
            keys.push(key);
        }
        Ok(keys)
    }

    /// Removes what an interrupted run left: staging files and bundles
    /// the manifest does not name.
    fn recover(&self) -> Result<(), StateError> {
        let _ = fs::remove_file(self.dir.join(format!(".{MANIFEST}.tmp")));
        let bundles = self.dir.join(BUNDLES);
        let entries = fs::read_dir(&bundles).map_err(|source| StateError::Io {
            path: bundles.clone(),
            source,
        })?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !self.manifest.bundles.iter().any(|e| *e.name == *name) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Ok(())
    }

    fn bundle(&self, name: &str) -> PathBuf {
        self.dir.join(BUNDLES).join(name)
    }

    /// The directory of a sealed unit that may stand in for `key` at
    /// unix time `now`.
    pub fn find(&mut self, key: &UnitKey, now: u64) -> Option<PathBuf> {
        let entry = self.manifest.bundles.iter().find(|e| e.key == *key)?;
        if !self.reuse.allows(key, entry.captured, now) {
            return None;
        }
        let name = entry.name.clone();
        let dir = self.bundle(&name);
        if !is_intact(&dir, key) {
            self.discard(&name);
            return None;
        }
        self.used.insert(name);
        Some(dir)
    }

    /// Drops a bundle; the manifest no longer names it.
    fn discard(&mut self, name: &str) {
        self.manifest.bundles.retain(|e| e.name != name);
        let _ = self.save();
        let _ = fs::remove_dir_all(self.bundle(name));
    }

    /// Seals the files of `staging` as the unit of `key` read at unix
    /// time `captured`, replacing the bundle of the same slot. Returns
    /// where the files are now.
    pub fn seal(
        &mut self,
        key: &UnitKey,
        captured: u64,
        staging: &Path,
    ) -> Result<Sealed, CacheError> {
        let at = |path: &Path| {
            let path = path.to_path_buf();
            move |source| StateError::Io { path, source }
        };
        let sums = sum_files(staging).map_err(at(staging))?;
        let bytes = sums.iter().map(|f| f.bytes).sum();
        if !self.make_room(bytes)? {
            return Ok(Sealed::OverBudget);
        }
        let metadata = Metadata {
            format: FORMAT,
            key: key.clone(),
            captured,
            files: sums,
        };
        let path = staging.join(METADATA);
        write_json(&path, &metadata).map_err(at(&path))?;
        sync_dir(staging).map_err(at(staging))?;
        let name = key.name();
        let target = self.bundle(&name);
        let _ = fs::remove_dir_all(&target);
        fs::rename(staging, &target).map_err(at(&target))?;
        sync_dir(&self.dir.join(BUNDLES)).map_err(at(&target))?;
        let superseded = self.replace_slot(Entry {
            name: name.clone(),
            key: key.clone(),
            captured,
            bytes,
        });
        self.save()?;
        for old in superseded.iter().filter(|old| old.name != name) {
            let _ = fs::remove_dir_all(self.bundle(&old.name));
        }
        self.used.insert(name);
        Ok(Sealed::Kept(target))
    }

    /// Frees unused bundles while the state filesystem is below
    /// `--min-free`; an error if that is not enough.
    pub fn ensure_free(&mut self) -> Result<(), CacheError> {
        self.make_room(0).map(|_| ())
    }

    /// Evicts bundles this run does not use, oldest first, until `bytes`
    /// more fit `--max-cache` and the state filesystem has `--min-free`
    /// available. Returns whether they fit the cache budget.
    fn make_room(&mut self, bytes: u64) -> Result<bool, CacheError> {
        loop {
            let kept: u64 = self.manifest.bundles.iter().map(|e| e.bytes).sum();
            let over = kept.saturating_add(bytes) > self.budget.max_bytes;
            let available = (self.available)(&self.dir).map_err(|source| StateError::Io {
                path: self.dir.clone(),
                source,
            })?;
            let low = available < self.budget.min_free;
            if !over && !low {
                return Ok(true);
            }
            let unused = self
                .manifest
                .bundles
                .iter()
                .filter(|e| !self.used.contains(&e.name))
                .min_by_key(|e| e.captured);
            match unused {
                Some(e) => {
                    let name = e.name.clone();
                    self.discard(&name);
                }
                None if low => {
                    return Err(CacheError::NoSpace {
                        available,
                        min_free: self.budget.min_free,
                    });
                }
                None => return Ok(false),
            }
        }
    }

    /// Puts `entry` in the manifest in place of its slot's; returns the
    /// entries it replaced.
    fn replace_slot(&mut self, entry: Entry) -> Vec<Entry> {
        let same_slot =
            |e: &Entry| e.key.subvolume == entry.key.subvolume && e.key.slot == entry.key.slot;
        let (old, kept) = std::mem::take(&mut self.manifest.bundles)
            .into_iter()
            .partition(same_slot);
        self.manifest.bundles = kept;
        self.manifest.bundles.push(entry);
        old
    }

    /// Atomically replaces the manifest.
    fn save(&self) -> Result<(), StateError> {
        let body = serde_json::to_vec(&self.manifest).expect("manifest serializes");
        state::replace_file(&self.dir, MANIFEST, |out| {
            out.write_all(&body)?;
            writeln!(out, "\n{:016x}", codec::checksum(&body))
        })
    }
}

/// The cache of `fsid` under `root` as of unix time `now`, read without
/// changing anything; empty if there is none or its manifest is damaged.
pub fn status(root: &Path, fsid: &str, reuse: Reuse, now: u64) -> CacheStatus {
    let bundles =
        read_manifest(&root.join(fsid).join(MANIFEST)).map_or_else(Vec::new, |m| m.bundles);
    let mut units: Vec<CacheUnit> = bundles
        .into_iter()
        .map(|e| CacheUnit {
            reusable: reuse.allows(&e.key, e.captured, now),
            subvolume: e.key.subvolume,
            uuid: e.key.uuid,
            slot: format!("{:016x}", e.key.slot),
            generation: e.key.generation,
            readonly: e.key.readonly,
            captured: e.captured,
            bytes: e.bytes,
        })
        .collect();
    units.sort_by_key(|u| (u.subvolume, u.captured));
    CacheStatus {
        format: FORMAT,
        fsid: fsid.to_string(),
        bytes: units.iter().map(|u| u.bytes).sum(),
        units,
    }
}

/// Deletes the cache of `fsid` under `root`, manifest first so no reader
/// sees a partial one; the caller holds the writer lock. Returns how many
/// units it named.
pub fn clear(root: &Path, fsid: &str) -> io::Result<usize> {
    let dir = root.join(fsid);
    let units = read_manifest(&dir.join(MANIFEST)).map_or(0, |m| m.bundles.len());
    match fs::remove_file(dir.join(MANIFEST)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        other => other?,
    }
    match fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(units),
        other => other.map(|()| units),
    }
}

/// Drops the cached units of `subvolumes`, which a mutation changed, so
/// no scan reuses them; the caller holds the writer lock. Returns how
/// many it dropped.
pub fn mark_stale(
    root: &Path,
    fsid: &str,
    uid: u32,
    subvolumes: &[u64],
) -> Result<usize, StateError> {
    if !root.join(fsid).join(MANIFEST).exists() {
        return Ok(0);
    }
    let unlimited = Budget {
        max_bytes: u64::MAX,
        min_free: 0,
    };
    let never = Reuse {
        writable_max_age: 0,
        trust_writable: false,
    };
    let mut cache = Cache::open(root, fsid, uid, never, unlimited)?;
    let stale: Vec<String> = cache
        .manifest
        .bundles
        .iter()
        .filter(|e| subvolumes.contains(&e.key.subvolume))
        .map(|e| e.name.clone())
        .collect();
    stale.iter().for_each(|name| cache.discard(name));
    Ok(stale.len())
}

/// A manifest body followed by a line with its checksum.
fn read_manifest(path: &Path) -> Option<Manifest> {
    let bytes = fs::read(path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?.strip_suffix('\n')?;
    let (body, sum) = text.rsplit_once('\n')?;
    if u64::from_str_radix(sum, 16).ok()? != codec::checksum(body.as_bytes()) {
        return None;
    }
    let manifest: Manifest = serde_json::from_str(body).ok()?;
    (manifest.format == FORMAT).then_some(manifest)
}

/// Whether the bundle in `dir` is `key`'s and its files match their sums.
fn is_intact(dir: &Path, key: &UnitKey) -> bool {
    let Some(metadata) = fs::read(dir.join(METADATA))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Metadata>(&bytes).ok())
    else {
        return false;
    };
    metadata.format == FORMAT
        && metadata.key == *key
        && metadata.files.iter().all(|f| {
            File::open(dir.join(&f.name))
                .is_ok_and(|file| sum(file, &f.name).ok() == Some(f.clone()))
        })
}

/// Sizes and checksums of the files of `dir`, after syncing them.
fn sum_files(dir: &Path) -> io::Result<Vec<FileSum>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            names.push(entry.file_name().into_string().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 unit file name")
            })?);
        }
    }
    names.sort();
    names
        .iter()
        .map(|name| sum_synced(&dir.join(name), name))
        .collect()
}

/// Size and checksum of a file, after syncing it.
fn sum_synced(path: &Path, name: &str) -> io::Result<FileSum> {
    let file = File::open(path)?;
    file.sync_all()?;
    sum(file, name)
}

fn sum(mut file: File, name: &str) -> io::Result<FileSum> {
    let mut hash = Fnv::new();
    let mut bytes = 0u64;
    let mut buf = vec![0u8; crate::spill::CHUNK];
    loop {
        match file.read(&mut buf)? {
            0 => break,
            n => {
                hash.update(&buf[..n]);
                bytes += n as u64;
            }
        }
    }
    Ok(FileSum {
        name: name.to_string(),
        bytes,
        checksum: hash.value(),
    })
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let body = serde_json::to_vec(value).expect("metadata serializes");
    state::write_synced(path, |out| out.write_all(&body).map(|()| 0))?;
    Ok(())
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Retained partial units, including legacy single-namespace checkpoints.
fn namespace_paths(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name
            .strip_suffix(".active")
            .or_else(|| name.strip_suffix(".json"))
        else {
            continue;
        };
        if stem == "namespace"
            || stem
                .strip_prefix("namespace-")
                .is_some_and(|id| id.parse::<u64>().is_ok())
        {
            names.insert(stem.to_string());
        }
    }
    Ok(names
        .into_iter()
        .map(|stem| dir.join(stem).with_extension("json"))
        .collect())
}

/// A filesystem's cache as the unit store of a scan, on the wall clock.
#[derive(Debug)]
pub struct Sealing {
    cache: Cache,
    /// A unit was left unsealed: past `--max-cache`.
    pub over_budget: bool,
    checkpoint: bool,
}

impl Sealing {
    fn clear_namespace(&self, key: &UnitKey) -> io::Result<()> {
        if !self.checkpoint {
            return Ok(());
        }
        for namespace in namespace_paths(&self.cache.dir)? {
            let active = namespace.with_extension("active");
            let saved: Option<UnitKey> = fs::read(&active)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            if saved.as_ref() == Some(key) {
                for path in [active, namespace.with_extension("refs"), namespace] {
                    match fs::remove_file(path) {
                        Ok(()) => (),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        File::open(&self.cache.dir)?.sync_all()?;
        Ok(())
    }

    pub(crate) fn checkpoint(cache: Cache) -> Self {
        Self {
            cache,
            over_budget: false,
            checkpoint: true,
        }
    }
    pub fn new(cache: Cache) -> Self {
        Sealing {
            cache,
            over_budget: false,
            checkpoint: false,
        }
    }
}

impl UnitCache for Sealing {
    fn checkpoint_key(&self, tree: u64) -> Option<UnitKey> {
        if !self.checkpoint {
            return None;
        }
        if let Ok(paths) = namespace_paths(&self.cache.dir) {
            for path in paths {
                let key = fs::read(path.with_extension("active"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<UnitKey>(&b).ok());
                if let Some(key) = key.filter(|key| key.subvolume == tree) {
                    return Some(key);
                }
            }
        }
        self.cache
            .manifest
            .bundles
            .iter()
            .find(|entry| entry.key.subvolume == tree)
            .map(|entry| entry.key.clone())
    }

    fn checkpoint_keys(&self) -> Result<Vec<UnitKey>, WalkError> {
        if !self.checkpoint {
            return Ok(Vec::new());
        }
        Cache::checkpoint_keys(
            self.cache.dir.parent().expect("filesystem checkpoint"),
            &self.cache.manifest.fsid,
        )
    }

    fn namespace_checkpoint_for(&self, tree: u64) -> Option<PathBuf> {
        let legacy = self.namespace_checkpoint()?;
        let alternate = self.cache.dir.join(format!("namespace-{tree}.json"));
        if alternate.exists() || alternate.with_extension("active").exists() {
            return Some(alternate);
        }
        let saved = fs::read(legacy.with_extension("active"))
            .ok()
            .and_then(|b| serde_json::from_slice::<UnitKey>(&b).ok());
        Some(if saved.is_some_and(|key| key.subvolume != tree) {
            alternate
        } else {
            legacy
        })
    }

    fn find_checked(&mut self, key: &UnitKey) -> Result<Option<PathBuf>, WalkError> {
        if !self.checkpoint {
            return Ok(self.find(key));
        }
        let Some(entry) = self
            .cache
            .manifest
            .bundles
            .iter()
            .find(|e| e.key.subvolume == key.subvolume && e.key.slot == key.slot)
        else {
            return Ok(None);
        };
        let name = entry.name.clone();
        let dir = self.cache.bundle(&name);
        if !is_intact(&dir, &entry.key) {
            return Err(WalkError::Checkpoint(
                "a completed subvolume checkpoint is missing or damaged".into(),
            ));
        }
        if entry.key.uuid != key.uuid {
            return Err(WalkError::Checkpoint(
                "the subvolume identity changed".into(),
            ));
        }
        if entry.key != *key {
            return Ok(None);
        }
        self.cache.used.insert(name);
        self.clear_namespace(key)?;
        Ok(Some(dir))
    }

    fn namespace_checkpoint(&self) -> Option<PathBuf> {
        self.checkpoint
            .then(|| self.cache.dir.join("namespace.json"))
    }

    fn now(&self) -> u64 {
        crate::time::unix_seconds(std::time::SystemTime::now())
    }

    fn find(&mut self, key: &UnitKey) -> Option<PathBuf> {
        let now = self.now();
        self.cache.find(key, now)
    }

    fn seal(&mut self, key: &UnitKey, captured: u64, staging: &Path) -> Result<PathBuf, WalkError> {
        match self.cache.seal(key, captured, staging) {
            Ok(Sealed::Kept(dir)) => {
                self.clear_namespace(key)?;
                Ok(dir)
            }
            Ok(Sealed::OverBudget) => {
                self.over_budget = true;
                Ok(staging.to_path_buf())
            }
            Err(CacheError::NoSpace { .. }) => Err(WalkError::Stopped(StopReason::DiskLimit)),
            Err(CacheError::State(e)) => Err(WalkError::Spill(io::Error::other(e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::testing::scratch;

    const TRUSTED: Reuse = Reuse {
        writable_max_age: 300,
        trust_writable: true,
    };

    const ROOMY: Budget = Budget {
        max_bytes: u64::MAX,
        min_free: 0,
    };

    fn open(root: &Path, reuse: Reuse) -> Cache {
        open_with(root, reuse, ROOMY)
    }

    fn open_with(root: &Path, reuse: Reuse, budget: Budget) -> Cache {
        let uid = crate::sys::effective_uid();
        Cache::open(&root.join("cache"), "fs", uid, reuse, budget).unwrap()
    }

    #[test]
    fn checkpoint_admission_refuses_damaged_or_incompatible_manifests_without_recovering_them() {
        let dir = scratch("checkpoint-admission");
        fs::create_dir_all(dir.join("fs")).unwrap();
        let path = dir.join("fs/manifest");
        for bytes in [
            b"damaged".as_slice(),
            br#"{"format":0,"fsid":"fs","bundles":[]}"#,
        ] {
            fs::write(&path, bytes).unwrap();
            let error = Cache::checkpoint_keys(&dir, "fs").unwrap_err();
            assert!(matches!(error, WalkError::Checkpoint(_)));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    fn key(subvolume: u64, generation: u64) -> UnitKey {
        UnitKey {
            subvolume,
            uuid: "00ff".into(),
            generation,
            readonly: false,
            slot: 7,
            inputs: 9,
        }
    }

    /// A staging directory holding `tree` and `extents`.
    fn staged(root: &Path, tag: &str, content: &[u8]) -> PathBuf {
        let dir = root.join(format!("staging-{tag}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tree"), content).unwrap();
        fs::write(dir.join("extents"), [content, b"!"].concat()).unwrap();
        dir
    }

    #[test]
    fn a_sealed_unit_is_found_by_its_key_after_reopening() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        let staging = staged(&root, "a", b"tree bytes");
        let sealed = cache
            .seal(&key(256, 10), 1000, &staging)
            .unwrap()
            .kept()
            .unwrap();
        assert!(!staging.exists());
        let mut cache = open(&root, TRUSTED);
        assert_eq!(cache.find(&key(256, 10), 1060), Some(sealed.clone()));
        assert_eq!(fs::read(sealed.join("tree")).unwrap(), b"tree bytes");
        assert_eq!(cache.find(&key(256, 11), 1060), None, "another generation");
    }

    #[test]
    fn writable_units_expire_and_read_only_ones_do_not() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        let mut snapshot = key(300, 5);
        snapshot.readonly = true;
        for k in [key(256, 10), snapshot.clone()] {
            let staging = staged(&root, &k.subvolume.to_string(), b"t");
            cache.seal(&k, 1000, &staging).unwrap();
        }
        assert!(cache.find(&key(256, 10), 1300).is_some());
        assert!(cache.find(&key(256, 10), 1301).is_none(), "expired");
        assert!(
            cache.find(&key(256, 10), 999).is_none(),
            "captured in the future"
        );
        assert!(cache.find(&snapshot, 1_000_000).is_some());
        let mut untrusted = open(
            &root,
            Reuse {
                trust_writable: false,
                ..TRUSTED
            },
        );
        assert!(untrusted.find(&key(256, 10), 1001).is_none());
        assert!(untrusted.find(&snapshot, 1001).is_some());
    }

    #[test]
    fn sealing_a_slot_again_replaces_its_bundle() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        let first = cache
            .seal(&key(256, 10), 1000, &staged(&root, "1", b"old"))
            .unwrap()
            .kept()
            .unwrap();
        let mut other_slot = key(256, 10);
        other_slot.slot = 8;
        cache
            .seal(&other_slot, 1000, &staged(&root, "2", b"other"))
            .unwrap();
        cache
            .seal(&key(256, 11), 1010, &staged(&root, "3", b"new"))
            .unwrap();
        assert!(!first.exists());
        assert!(cache.find(&key(256, 10), 1020).is_none());
        assert!(cache.find(&key(256, 11), 1020).is_some());
        assert!(cache.find(&other_slot, 1020).is_some());
    }

    #[test]
    fn resealing_an_expired_unit_of_the_same_generation_keeps_the_new_bundle() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        cache
            .seal(&key(256, 10), 1000, &staged(&root, "1", b"old"))
            .unwrap();
        assert!(cache.find(&key(256, 10), 1400).is_none(), "expired");
        let sealed = cache
            .seal(&key(256, 10), 1400, &staged(&root, "2", b"new"))
            .unwrap()
            .kept()
            .unwrap();
        assert_eq!(fs::read(sealed.join("tree")).unwrap(), b"new");
        assert_eq!(cache.find(&key(256, 10), 1500), Some(sealed));
    }

    #[test]
    fn a_corrupt_or_incomplete_bundle_is_discarded() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        let mut sealed = Vec::new();
        for subvolume in [256, 257, 258] {
            let staging = staged(&root, &subvolume.to_string(), b"tree bytes");
            sealed.push(
                cache
                    .seal(&key(subvolume, 10), 1000, &staging)
                    .unwrap()
                    .kept()
                    .unwrap(),
            );
        }
        let mut flipped = fs::read(sealed[0].join("tree")).unwrap();
        flipped[3] ^= 1;
        fs::write(sealed[0].join("tree"), flipped).unwrap();
        fs::remove_file(sealed[1].join("extents")).unwrap();
        fs::write(sealed[2].join(METADATA), b"{").unwrap();
        for (subvolume, dir) in [256, 257, 258].into_iter().zip(&sealed) {
            assert_eq!(cache.find(&key(subvolume, 10), 1001), None);
            assert!(!dir.exists(), "{subvolume} discarded");
        }
        let cache = open(&root, TRUSTED);
        assert!(cache.manifest.bundles.is_empty());
    }

    #[test]
    fn opening_discards_bundles_the_manifest_does_not_name() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        let kept = cache
            .seal(&key(256, 10), 1000, &staged(&root, "a", b"t"))
            .unwrap()
            .kept()
            .unwrap();
        let bundles = root.join("cache/fs").join(BUNDLES);
        let torn = bundles.join("0123456789abcdef");
        fs::create_dir(&torn).unwrap();
        fs::write(root.join("cache/fs/.manifest.tmp"), b"half").unwrap();
        let mut cache = open(&root, TRUSTED);
        assert!(!torn.exists());
        assert!(!root.join("cache/fs/.manifest.tmp").exists());
        assert_eq!(cache.find(&key(256, 10), 1001), Some(kept.clone()));
        fs::write(root.join("cache/fs").join(MANIFEST), b"garbage").unwrap();
        let mut cache = open(&root, TRUSTED);
        assert_eq!(cache.find(&key(256, 10), 1001), None);
        assert!(!kept.exists(), "a corrupt manifest empties the cache");
    }

    #[test]
    fn status_lists_the_cached_units_without_changing_the_cache() {
        let root = scratch("cache");
        let cache_root = root.join("cache");
        assert_eq!(status(&cache_root, "fs", TRUSTED, 0).units, []);
        assert!(!cache_root.exists(), "no cache is created");
        let mut cache = open(&root, TRUSTED);
        let mut snapshot = key(300, 5);
        snapshot.readonly = true;
        for k in [snapshot, key(256, 10)] {
            let staging = staged(&root, &k.subvolume.to_string(), b"tree");
            cache.seal(&k, 1000, &staging).unwrap();
        }
        let torn = cache_root.join("fs").join(BUNDLES).join("0123456789abcdef");
        fs::create_dir(&torn).unwrap();
        let s = status(&cache_root, "fs", TRUSTED, 1400);
        assert_eq!((s.format, s.fsid.as_str()), (FORMAT, "fs"));
        assert_eq!(s.bytes, 2 * 9, "tree and extents of both units");
        let units: Vec<_> = s
            .units
            .iter()
            .map(|u| {
                (
                    u.subvolume,
                    u.generation,
                    u.readonly,
                    u.captured,
                    u.reusable,
                )
            })
            .collect();
        assert_eq!(
            units,
            [(256, 10, false, 1000, false), (300, 5, true, 1000, true)],
            "by subvolume; the writable one has expired"
        );
        assert_eq!(s.units[0].slot, "0000000000000007");
        assert!(torn.exists(), "status changes nothing");
    }

    #[test]
    fn clearing_removes_every_unit() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        cache
            .seal(&key(256, 10), 1000, &staged(&root, "a", b"t"))
            .unwrap();
        assert_eq!(clear(&root.join("cache"), "fs").unwrap(), 1);
        assert!(!root.join("cache/fs").exists());
        assert_eq!(
            clear(&root.join("cache"), "fs").unwrap(),
            0,
            "already clear"
        );
        let mut cache = open(&root, TRUSTED);
        assert!(cache.find(&key(256, 10), 1001).is_none());
    }

    #[test]
    fn marking_subvolumes_stale_drops_only_their_units() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        for subvolume in [256, 257] {
            let staging = staged(&root, &subvolume.to_string(), b"t");
            cache.seal(&key(subvolume, 10), 1000, &staging).unwrap();
        }
        let uid = crate::sys::effective_uid();
        let stale = mark_stale(&root.join("cache"), "fs", uid, &[257, 999]).unwrap();
        assert_eq!(stale, 1);
        let mut cache = open(&root, TRUSTED);
        assert!(cache.find(&key(256, 10), 1001).is_some());
        assert!(cache.find(&key(257, 10), 1001).is_none());
        let none = root.join("none");
        assert_eq!(mark_stale(&none, "fs", uid, &[256]).unwrap(), 0);
        assert!(!none.exists(), "no cache is created");
    }

    fn available_100(_: &Path) -> io::Result<u64> {
        Ok(100)
    }

    #[test]
    fn over_the_cache_budget_unused_bundles_go_first_then_units_stay_unsealed() {
        let root = scratch("cache");
        // Each staged unit is 21 bytes: room for two.
        let budget = Budget {
            max_bytes: 50,
            min_free: 0,
        };
        let mut cache = open_with(&root, TRUSTED, budget);
        let old = cache.seal(&key(256, 10), 1000, &staged(&root, "a", b"0123456789"));
        let old = old.unwrap().kept().unwrap();
        let mut cache = open_with(&root, TRUSTED, budget);
        let b = staged(&root, "b", b"0123456789");
        let sealed = cache.seal(&key(257, 10), 1000, &b).unwrap();
        assert!(sealed.kept().is_some());
        let c = staged(&root, "c", b"0123456789");
        assert!(old.exists(), "room for both");
        let sealed = cache.seal(&key(258, 10), 1000, &c).unwrap();
        assert!(!old.exists(), "unused by this run: evicted");
        assert!(sealed.kept().is_some());
        let d = staged(&root, "d", b"0123456789");
        let sealed = cache.seal(&key(259, 10), 1000, &d).unwrap();
        assert!(sealed.kept().is_none(), "never evicts what this run uses");
        assert!(d.join("tree").exists(), "left in staging");
    }

    #[test]
    fn below_the_free_space_reserve_unused_bundles_go_first_then_it_is_an_error() {
        let root = scratch("cache");
        let mut cache = open(&root, TRUSTED);
        let old = cache.seal(&key(256, 10), 1000, &staged(&root, "a", b"t"));
        let old = old.unwrap().kept().unwrap();
        let budget = Budget {
            max_bytes: u64::MAX,
            min_free: 200,
        };
        let mut cache = open_with(&root, TRUSTED, budget);
        cache.available = available_100;
        let staging = staged(&root, "b", b"t");
        let sealed = cache.seal(&key(257, 10), 1000, &staging);
        assert!(matches!(
            sealed,
            Err(CacheError::NoSpace { available: 100, .. })
        ));
        assert!(!old.exists(), "freed first");
        assert!(matches!(
            cache.ensure_free(),
            Err(CacheError::NoSpace { .. })
        ));
    }

    #[test]
    fn only_validated_dsm_builds_trust_writable_generations() {
        let version = "majorversion=\"7\"\nbuildnumber=\"90069\"\nsmallfixnumber=\"0\"\n";
        assert_eq!(dsm_build(version), Some(90069));
        assert_eq!(dsm_build("buildnumber=90069"), None);
        assert!(trusts_writable_generations(None), "upstream kernels");
        assert!(!trusts_writable_generations(Some(1)));
        assert!(trusts_writable_generations(Some(90069)), "DSM 7.4");
    }

    #[test]
    fn as_a_unit_store_a_full_cache_leaves_units_unsealed_and_no_space_stops() {
        let root = scratch("cache");
        let budget = Budget {
            max_bytes: 1,
            min_free: 0,
        };
        let mut store = Sealing::new(open_with(&root, TRUSTED, budget));
        let staging = staged(&root, "a", b"tree");
        let kept = store.seal(&key(256, 10), 1000, &staging).unwrap();
        assert_eq!(kept, staging);
        assert!(store.over_budget);
        let mut cache = open_with(&root, TRUSTED, ROOMY);
        cache.budget.min_free = 200;
        cache.available = available_100;
        let mut store = Sealing::new(cache);
        let stopped = store.seal(&key(256, 10), 1000, &staging);
        assert!(matches!(
            stopped,
            Err(WalkError::Stopped(StopReason::DiskLimit))
        ));
    }
}
