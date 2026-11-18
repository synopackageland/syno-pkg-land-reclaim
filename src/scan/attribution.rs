//! Disk-backed, size-prioritised work left by a bounded overview read.
//! At most 64 output buffers and one bounded record are resident, regardless
//! of volume capacity or extent count. This is display attribution only.
use super::*;
use crate::btrfs::read_budget::ReadBudget;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

#[cfg(test)]
std::thread_local! {
    static CHECKPOINT_DELAY: std::cell::Cell<std::time::Duration> = const { std::cell::Cell::new(std::time::Duration::ZERO) };
    /// Virtual time the test checkpoint delay has charged on this thread.
    static VIRTUAL_TIME: std::cell::Cell<std::time::Duration> = const { std::cell::Cell::new(std::time::Duration::ZERO) };
}
/// Tests only: each durable `progress.json` save then takes `delay` more
/// time, on a virtual clock that `Stopwatch` adds to the real one. It does not
/// sleep, so what the measurement includes can be checked exactly, however
/// slowly the host (an emulated aarch64 under qemu, say) runs the real work.
#[cfg(test)]
pub(super) fn checkpoint_test_delay(delay: std::time::Duration) {
    CHECKPOINT_DELAY.set(delay);
}

/// Measures `elapsed_ms`: real time (plus, in tests, the virtual checkpoint
/// time charged since it started).
struct Stopwatch {
    start: std::time::Instant,
    #[cfg(test)]
    virtual_start: std::time::Duration,
}

impl Stopwatch {
    fn start() -> Self {
        Self {
            start: std::time::Instant::now(),
            #[cfg(test)]
            virtual_start: VIRTUAL_TIME.get(),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        let elapsed = self.start.elapsed();
        #[cfg(test)]
        let elapsed = elapsed + VIRTUAL_TIME.get().saturating_sub(self.virtual_start);
        elapsed.as_millis() as u64
    }
}

const MAX_RECORD: u64 = 2 << 20;
const CHUNK: u64 = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub bytenr: u64,
    pub refs: ExtentRefs,
    pub owners: Vec<OverviewOwner>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bucket {
    records: u64,
    bytes: u64,
    reads: u64,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    identity: String,
    pub buckets: Vec<Bucket>,
    pub base: OverviewData,
    pub plan: ReadBudget,
}

/// Leaves and root identities can change while an active live tree advances.
/// Names/parents/UUIDs/roles and readonly generations must stay identical;
/// every deferred allocation is separately revalidated before attribution.
pub fn identity(set: &SubvolumeSet) -> String {
    let mut hash = crate::sha256::Sha256::new();
    for root in &set.subvolumes {
        hash.update(
            format!(
                "{:?}",
                (
                    root.id,
                    &root.parent,
                    &root.name,
                    root.uuid,
                    root.parent_uuid,
                    root.readonly,
                    root.pending_deletion,
                    root.readonly.then_some(root.generation)
                )
            )
            .as_bytes(),
        );
    }
    hash.update(format!("{:?}", set.relocating).as_bytes());
    hash.finish_hex()
}

pub(super) struct Capture {
    dir: PathBuf,
    writers: Vec<Option<BufWriter<File>>>,
    buckets: Vec<Bucket>,
}

impl Capture {
    pub(super) fn new(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        for b in 0..64 {
            let _ = fs::remove_file(dir.join(format!("{b}.jsonl")));
        }
        let _ = fs::remove_file(dir.join("manifest.json"));
        Ok(Self {
            dir: dir.to_path_buf(),
            writers: (0..64).map(|_| None).collect(),
            buckets: vec![Bucket::default(); 64],
        })
    }

    pub(super) fn push(&mut self, task: &Pending) -> io::Result<()> {
        let b = task.refs.num_bytes.max(1).ilog2() as usize;
        if self.writers[b].is_none() {
            use std::os::unix::fs::OpenOptionsExt;
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(self.dir.join(format!("{b}.jsonl")))?;
            self.writers[b] = Some(BufWriter::new(file));
        }
        let out = self.writers[b].as_mut().expect("opened");
        serde_json::to_writer(&mut *out, task).map_err(io::Error::other)?;
        out.write_all(b"\n")?;
        self.buckets[b].records += 1;
        self.buckets[b].bytes = self.buckets[b].bytes.saturating_add(task.refs.num_bytes);
        self.buckets[b].reads = self.buckets[b]
            .reads
            .saturating_add(task.refs.backrefs.len() as u64);
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        set: &SubvolumeSet,
        base: &OverviewData,
        plan: ReadBudget,
    ) -> io::Result<()> {
        for (b, writer) in self.writers.iter_mut().enumerate() {
            if let Some(out) = writer {
                out.flush()?;
                out.get_ref().sync_all()?;
                self.buckets[b].digest = file_digest(&self.dir.join(format!("{b}.jsonl")))?;
            }
        }
        save(
            &self.dir,
            "manifest.json",
            &Manifest {
                identity: identity(set),
                buckets: self.buckets,
                base: base.clone(),
                plan,
            },
        )
    }
}

fn file_digest(path: &Path) -> io::Result<String> {
    let mut file = BufReader::new(File::open(path)?);
    let mut hash = crate::sha256::Sha256::new();
    let mut bytes = [0; 32 << 10];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hash.finish_hex())
}

fn invalid(message: impl Into<String>) -> WalkError {
    WalkError::Checkpoint(message.into())
}

fn save<T: Serialize>(dir: &Path, name: &str, value: &T) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut hash = crate::sha256::Sha256::new();
    hash.update(&data);
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    serde_json::to_writer(
        &mut out,
        &(
            hash.finish_hex(),
            serde_json::from_slice::<serde_json::Value>(&data).map_err(io::Error::other)?,
        ),
    )
    .map_err(io::Error::other)?;
    out.sync_all()?;
    fs::rename(tmp, dir.join(name))?;
    File::open(dir)?.sync_all()?;
    #[cfg(test)]
    if name == "progress.json" {
        VIRTUAL_TIME.set(VIRTUAL_TIME.get() + CHECKPOINT_DELAY.get());
    }
    Ok(())
}

fn load<T: serde::de::DeserializeOwned + Serialize>(path: &Path) -> Result<T, WalkError> {
    let file = File::open(path)?;
    if file.metadata()?.len() > 32 << 20 {
        return Err(invalid("attribution checkpoint is too large"));
    }
    let (digest, value): (String, serde_json::Value) =
        serde_json::from_reader(BufReader::new(file))
            .map_err(|e| invalid(format!("damaged attribution checkpoint: {e}")))?;
    let decoded: T = serde_json::from_value(value.clone()).map_err(|e| invalid(e.to_string()))?;
    let mut hash = crate::sha256::Sha256::new();
    hash.update(&serde_json::to_vec(&decoded).map_err(io::Error::other)?);
    if hash.finish_hex() != digest {
        return Err(invalid("attribution checkpoint checksum mismatch"));
    }
    Ok(decoded)
}

pub fn open(dir: &Path, set: &SubvolumeSet) -> Result<Manifest, WalkError> {
    let saved: Manifest = load(&dir.join("manifest.json"))?;
    if saved.identity != identity(set) || !set.relocating.is_empty() {
        return Err(invalid(
            "attribution roots changed; run a fresh volume scan",
        ));
    }
    for (b, bucket) in saved.buckets.iter().enumerate() {
        if bucket.records > 0 && file_digest(&dir.join(format!("{b}.jsonl")))? != bucket.digest {
            return Err(invalid("attribution queue checksum mismatch"));
        }
    }
    Ok(saved)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measurement {
    pub elapsed_ms: u64,
    pub peak_rss_bytes: Option<u64>,
    pub unknown_before: u64,
    pub unknown_after: u64,
    pub processed_extents: u64,
    pub changed_extents: u64,
    pub reads: u64,
    pub plan: Option<ReadBudget>,
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    key: String,
    bucket: i32,
    offset: u64,
    overview: OverviewData,
    measurement: Measurement,
}

/// A record is capped before JSON decoding; a corrupt line cannot allocate
/// memory proportional to a whole volume's queue.
fn next_record(
    reader: &mut BufReader<File>,
    bytes: &mut Vec<u8>,
) -> Result<Option<Pending>, WalkError> {
    bytes.clear();
    let n = reader.take(MAX_RECORD + 1).read_until(b'\n', bytes)?;
    if n == 0 {
        return Ok(None);
    }
    if n as u64 > MAX_RECORD || bytes.last() != Some(&b'\n') {
        return Err(invalid("invalid attribution record length"));
    }
    serde_json::from_slice(bytes)
        .map(Some)
        .map_err(|e| invalid(e.to_string()))
}

pub fn process<S: MetadataSource + ?Sized, R: BlockReader>(
    source: &mut S,
    leaves: R,
    manifest: &Manifest,
    dir: &Path,
    checkpoint: &Path,
    requested: u64,
    boundary: &mut dyn BatchBoundary,
) -> Result<(OverviewData, Measurement), WalkError> {
    let reads = manifest
        .buckets
        .iter()
        .map(|b| b.reads)
        .fold(0, u64::saturating_add);
    let plan = ReadBudget::select(
        requested,
        manifest.plan.used_bytes,
        manifest.plan.extent_count,
        manifest.plan.snapshot_count,
        reads,
    );
    let key = format!("{}:{:?}:{:?}", manifest.identity, manifest.buckets, plan);
    fs::create_dir_all(checkpoint)?;
    let path = checkpoint.join("progress.json");
    let mut saved: Checkpoint = if path.exists() {
        load(&path)?
    } else {
        Checkpoint {
            key: key.clone(),
            bucket: 63,
            offset: 0,
            overview: manifest.base.clone(),
            measurement: Measurement {
                unknown_before: manifest.base.unknown.values().sum(),
                plan: Some(plan.clone()),
                ..Default::default()
            },
        }
    };
    if saved.key != key {
        return Err(invalid("attribution checkpoint inputs changed"));
    }
    let mut leaves = LeafRanges::new(
        leaves,
        plan.selected_reads.saturating_sub(saved.measurement.reads),
    );
    let elapsed_before = saved.measurement.elapsed_ms;
    let tick = Stopwatch::start();
    let mut line = Vec::new();
    while saved.bucket >= 0 {
        let b = saved.bucket as usize;
        if manifest.buckets[b].records == 0 {
            saved.bucket -= 1;
            continue;
        }
        let mut reader = BufReader::new(File::open(dir.join(format!("{b}.jsonl")))?);
        reader.seek(SeekFrom::Start(saved.offset))?;
        let mut chunk = 0;
        while let Some(task) = next_record(&mut reader, &mut line)? {
            let mut current = None;
            crate::btrfs::extent_refs::resolve(
                source,
                std::iter::once(task.bytenr),
                Plan::Stream,
                boundary,
                |_, refs| current = refs.cloned(),
            )?;
            let pinned = if current.as_ref() != Some(&task.refs) {
                saved.measurement.changed_extents += 1;
                Err(PinnedRangesSubcase::ExtentChanged)
            } else {
                let read = leaves.all_ranges(source, task.bytenr, &task.refs, boundary)?;
                match read {
                    Ok(ranges) => unreferenced_file_bytes(ranges, task.refs.num_bytes)
                        .ok_or(PinnedRangesSubcase::RangeUnmappable),
                    Err(Unresolved::BudgetExhausted) => {
                        Err(PinnedRangesSubcase::ReadBudgetExhausted)
                    }
                    Err(Unresolved::Unverified) => Err(PinnedRangesSubcase::LeafUnverified),
                    Err(Unresolved::RootUnsearchable) => Err(PinnedRangesSubcase::RootUnsearchable),
                }
            };
            replace_pending(&mut saved.overview, &task, pinned)?;
            saved.measurement.processed_extents += 1;
            saved.offset = reader.stream_position()?;
            chunk += 1;
            if chunk == CHUNK {
                saved.measurement.reads += leaves.stats.leaf_reads + leaves.stats.direct_searches;
                leaves.stats = LeafRangeStats::default();
                saved.measurement.elapsed_ms = elapsed_before.saturating_add(tick.elapsed_ms());
                saved.measurement.peak_rss_bytes = saved
                    .measurement
                    .peak_rss_bytes
                    .into_iter()
                    .chain(crate::resource::peak_resident_bytes())
                    .max();
                save(checkpoint, "progress.json", &saved)?;
                // Only the last interrupted chunk is repeated on resume.
                chunk = 0;
            }
        }
        saved.bucket -= 1;
        saved.offset = 0;
    }
    saved.measurement.reads += leaves.stats.leaf_reads + leaves.stats.direct_searches;
    saved.measurement.unknown_after = saved.overview.unknown.values().sum();
    saved.measurement.peak_rss_bytes = saved
        .measurement
        .peak_rss_bytes
        .into_iter()
        .chain(crate::resource::peak_resident_bytes())
        .max();
    saved.measurement.elapsed_ms = elapsed_before.saturating_add(tick.elapsed_ms());
    save(checkpoint, "progress.json", &saved)?;
    Ok((saved.overview, saved.measurement))
}

fn replace_pending(
    overview: &mut OverviewData,
    task: &Pending,
    pinned: Result<u64, PinnedRangesSubcase>,
) -> Result<(), WalkError> {
    let n = task.refs.num_bytes;
    for value in [
        overview
            .unknown
            .get_mut(&UnknownReason::PinnedRangesUnknown),
        overview
            .pinned_ranges_subcases
            .get_mut(&PinnedRangesSubcase::ReadBudgetExhausted),
    ] {
        let value =
            value.ok_or_else(|| invalid("attribution queue differs from the volume report"))?;
        *value = value
            .checked_sub(n)
            .ok_or_else(|| invalid("attribution byte underflow"))?;
    }
    overview.extent_count = overview
        .extent_count
        .checked_sub(1)
        .ok_or_else(|| invalid("attribution extent underflow"))?;
    overview.extent_bytes = overview
        .extent_bytes
        .checked_sub(n)
        .ok_or_else(|| invalid("attribution extent byte underflow"))?;
    overview.unknown_extents = overview
        .unknown_extents
        .checked_sub(1)
        .ok_or_else(|| invalid("attribution unknown underflow"))?;
    let part = attribute_extent(&ExtentInput {
        bytes: n,
        refs_complete: true,
        owners: Some(task.owners.clone()),
        pinned_bytes: pinned,
        reclaimable: false,
    });
    overview.add(&part, n);
    overview.unknown.retain(|_, v| *v != 0);
    overview.pinned_ranges_subcases.retain(|_, v| *v != 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "attribution-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn damaged_checkpoint_is_never_reused() {
        let dir = temp();
        save(&dir, "value", &vec![1u64, 2, 3]).unwrap();
        let path = dir.join("value");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value[1][0] = serde_json::json!(99);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load::<Vec<u64>>(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use crate::btrfs::read_budget::ReadBudget;
    fn task(bytes: u64, bytenr: u64) -> Pending {
        Pending {
            bytenr,
            refs: ExtentRefs {
                num_bytes: bytes,
                refs: 1,
                backrefs: vec![DataBackref::Shared {
                    parent: 4096,
                    count: 1,
                }],
                ..Default::default()
            },
            owners: vec![OverviewOwner::Snapshot { share: 1 }],
        }
    }
    #[test]
    fn larger_byte_buckets_are_read_first_and_corruption_is_rejected() {
        let dir = super::tests::temp();
        let mut out = Capture::new(&dir).unwrap();
        for (bytes, address) in [(4096, 1), (1 << 30, 2), (1 << 20, 3)] {
            out.push(&task(bytes, address)).unwrap();
        }
        out.finish(
            &SubvolumeSet::default(),
            &OverviewData::default(),
            ReadBudget::select(DEFAULT_READ_BUDGET, 1 << 40, 3, 1, 0),
        )
        .unwrap();
        let manifest = open(&dir, &SubvolumeSet::default()).unwrap();
        let mut addresses = Vec::new();
        for (b, row) in manifest.buckets.iter().enumerate().rev() {
            if row.records == 0 {
                continue;
            }
            let mut reader = BufReader::new(File::open(dir.join(format!("{b}.jsonl"))).unwrap());
            let mut line = Vec::new();
            while let Some(task) = next_record(&mut reader, &mut line).unwrap() {
                addresses.push(task.bytenr);
            }
        }
        assert_eq!(addresses, [2, 3, 1]);
        fs::write(dir.join("30.jsonl"), b"changed\n").unwrap();
        assert!(open(&dir, &SubvolumeSet::default()).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_500_tib_model_keeps_a_fixed_working_set_and_linear_read_cost() {
        if std::env::var_os("RECLAIM_ATTRIBUTION_MODEL_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "scan::attribution::queue_tests::a_500_tib_model_keeps_a_fixed_working_set_and_linear_read_cost", "--nocapture", "--test-threads=1"])
                .env("RECLAIM_ATTRIBUTION_MODEL_CHILD", "1").output().unwrap();
            eprintln!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "isolated memory model failed");
            return;
        }
        // Model 64 million extents on a 500 TiB volume. Exercise the actual
        // spill writer/reader with a representative 64K-record stream rather
        // than materialising 64 million allocations in the test process.
        let plan = ReadBudget::select(DEFAULT_READ_BUDGET, 500 << 40, 64_000_000, 4096, 0);
        let smaller = ReadBudget::select(DEFAULT_READ_BUDGET, 250 << 40, 32_000_000, 4096, 0);
        assert!(plan.selected_reads <= smaller.selected_reads * 2 + 1);
        let dir = super::tests::temp();
        let before = crate::resource::peak_resident_bytes().unwrap_or(0);
        let mut capture = Capture::new(&dir).unwrap();
        for address in 0..65_536 {
            capture.push(&task(8 << 20, address * (8 << 20))).unwrap();
        }
        capture
            .finish(
                &SubvolumeSet::default(),
                &OverviewData::default(),
                plan.clone(),
            )
            .unwrap();
        let manifest = open(&dir, &SubvolumeSet::default()).unwrap();
        let mut reader = BufReader::new(File::open(dir.join("23.jsonl")).unwrap());
        let mut line = Vec::new();
        let mut count = 0;
        while next_record(&mut reader, &mut line).unwrap().is_some() {
            count += 1;
            assert!(line.capacity() <= MAX_RECORD as usize);
        }
        assert_eq!(count, 65_536);
        assert_eq!(manifest.buckets[23].records, count);
        let peak = crate::resource::peak_resident_bytes().unwrap_or(0);
        assert!(
            peak.saturating_sub(before) < 32 << 20,
            "stream RSS grew by {}",
            peak.saturating_sub(before)
        );
        assert!(peak < 512 << 20, "RSS {peak}");
        // An I/O-cost model, not a promised hardware duration: upper admission
        // divided by a measured throughput is O(extents), not O(extents*N snapshots).
        eprintln!(
            "500 TiB model: {} admitted reads; at 2000 read units/s {:.1} hours, at 20000/s {:.1} hours; peak RSS {} bytes; fixed leaf cache 256, 64 spill buffers",
            plan.selected_reads,
            plan.selected_reads as f64 / 2000.0 / 3600.0,
            plan.selected_reads as f64 / 20000.0 / 3600.0,
            peak
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
