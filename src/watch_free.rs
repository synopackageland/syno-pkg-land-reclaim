//! Read-only, bounded observations of Btrfs data space and cleaner backlog.

use crate::btrfs::live::{LiveSource, OpenTarget};
use crate::btrfs::source::{BlockGroupSpace, MetadataSource};
use crate::document::{Completion, Envelope};
use crate::output::{JsonPath, escape_human, human_bytes};
use crate::resolve::Environment;
use crate::resource::{Pacer, Profile, SystemClock};
use crate::sys;
use serde::Serialize;
use std::error::Error;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const DATA_FLAG: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sample {
    pub elapsed_ms: u64,
    pub data_used_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_used_error: Option<String>,
    pub free_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_error: Option<String>,
    pub pending_deletion_roots: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_deletion_error: Option<String>,
}

/// Common output from `watch-free` and cleanup's optional `--measure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub interval_seconds: u64,
    pub samples: Vec<Sample>,
    pub data_used_delta_bytes: Option<i64>,
    pub free_delta_bytes: Option<i64>,
    pub pending_deletion_roots_delta: Option<i64>,
    pub concurrent_writers_may_affect_delta: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchFreeDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub volume: JsonPath,
    #[serde(skip)]
    raw_volume: PathBuf,
    pub duration_seconds: u64,
    pub observed_duration_ms: u64,
    pub interrupted: bool,
    #[serde(flatten)]
    pub observation: Observation,
}

pub(crate) struct Watcher {
    target: PathBuf,
    source: LiveSource,
    profile: Profile,
    interval: Duration,
    started: Instant,
    stop: &'static AtomicBool,
    samples: Vec<Sample>,
}

impl Watcher {
    pub(crate) fn open(
        target: &Path,
        profile: Profile,
        interval: Duration,
        stop: &'static AtomicBool,
    ) -> Result<Self, Box<dyn Error>> {
        let env = Environment::load()?;
        let volume = env
            .volumes
            .iter()
            .find(|volume| volume.primary.mount_point == target)
            .ok_or("target must be an exact mounted Btrfs volume root")?;
        let opened = OpenTarget::open(&volume.primary.mount_point)?;
        if volume.primary.fstype != "btrfs" {
            return Err("unsupported_capability: Btrfs cleaner/reflink diagnostics are not available on ext4".into());
        }
        Ok(Self {
            target: volume.primary.mount_point.clone(),
            source: LiveSource::new(opened.file, profile.settings().search_buffer),
            profile,
            interval,
            started: Instant::now(),
            stop,
            samples: Vec::new(),
        })
    }

    pub(crate) fn capture(&mut self) {
        if self.samples.is_empty() {
            self.started = Instant::now();
        }
        let space = self.source.space_snapshot();
        let (data_used_bytes, data_used_error, free_bytes, free_error) = match space {
            Some(space) => {
                let (data, data_error) = match space.block_groups {
                    Ok(groups) => (Some(data_used(&groups)), None),
                    Err(error) => (None, Some(error)),
                };
                let (free, free_error) = match space.statfs {
                    Ok(space) => (Some(space.free_bytes), None),
                    Err(error) => (None, Some(error)),
                };
                (data, data_error, free, free_error)
            }
            None => (
                None,
                Some("Btrfs space counters are unavailable".into()),
                None,
                Some("statfs space counters are unavailable".into()),
            ),
        };
        let (pending_deletion_roots, pending_deletion_error) =
            match pending_deletion_count(&mut self.source, self.profile, Some(self.stop)) {
                Ok(count) => (Some(count), None),
                Err(error) => (None, Some(error)),
            };
        self.samples.push(Sample {
            elapsed_ms: elapsed_millis(self.started.elapsed()),
            data_used_bytes,
            data_used_error,
            free_bytes,
            free_error,
            pending_deletion_roots,
            pending_deletion_error,
        });
    }

    /// Samples to the monotonic deadline. `next_sample` allows cleanup to
    /// include executor time before it starts the post-delete wait.
    pub(crate) fn observe_until(&mut self, deadline: Instant, next_sample: Instant) -> bool {
        let mut next = next_sample.min(deadline);
        loop {
            if self.stop.load(std::sync::atomic::Ordering::Relaxed) {
                return true;
            }
            sleep_until(next, self.stop);
            if self.stop.load(std::sync::atomic::Ordering::Relaxed) {
                return true;
            }
            self.capture();
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            while next <= now {
                next = next.checked_add(self.interval).unwrap_or(deadline);
            }
            next = next.min(deadline);
        }
    }

    pub(crate) fn observation(&self) -> Observation {
        summarize(self.interval, &self.samples)
    }

    pub(crate) fn document(
        &self,
        duration: Duration,
        interrupted: bool,
        completion: Completion,
    ) -> WatchFreeDocument {
        let mut envelope = Envelope::new("watch-free", SystemTime::now());
        envelope.completion = completion;
        WatchFreeDocument {
            envelope,
            volume: JsonPath::new(self.target.as_os_str().as_bytes()),
            raw_volume: self.target.clone(),
            duration_seconds: duration.as_secs(),
            observed_duration_ms: elapsed_millis(self.started.elapsed()),
            interrupted,
            observation: self.observation(),
        }
    }

    pub(crate) fn endpoint_space_available(&self) -> bool {
        if self.samples.len() < 2 {
            return false;
        }
        let Some(first) = self.samples.first() else {
            return false;
        };
        let Some(last) = self.samples.last() else {
            return false;
        };
        first.data_used_bytes.is_some()
            && first.free_bytes.is_some()
            && last.data_used_bytes.is_some()
            && last.free_bytes.is_some()
    }
}

pub fn run(
    target: PathBuf,
    duration: Duration,
    interval: Duration,
    json: bool,
    profile: Profile,
) -> Result<i32, Box<dyn Error>> {
    if interval > duration {
        eprintln!("synoreclaim: --interval cannot exceed --duration");
        return Ok(2);
    }
    let stop = sys::install_stop_handlers()?;
    let mut watcher = match Watcher::open(&target, profile, interval, stop) {
        Ok(watcher) => watcher,
        Err(error) => {
            eprintln!("synoreclaim: watch-free refused: {error}");
            return Ok(4);
        }
    };
    for warning in crate::resource::apply_profile(profile).warnings {
        eprintln!("synoreclaim: warning: {warning}");
    }
    watcher.capture();
    let deadline = watcher
        .started
        .checked_add(duration)
        .unwrap_or(watcher.started);
    let next_sample = watcher.started + interval;
    let interrupted = watcher.observe_until(deadline, next_sample);
    let endpoint_space = watcher.endpoint_space_available();
    let completion = if interrupted {
        Completion::Paused
    } else if endpoint_space {
        Completion::Complete
    } else {
        Completion::Partial
    };
    let document = watcher.document(duration, interrupted, completion);
    emit(json, &document)?;
    if interrupted {
        Ok(130)
    } else if endpoint_space {
        Ok(0)
    } else {
        Ok(3)
    }
}

pub(crate) fn pending_deletion_count(
    source: &mut LiveSource,
    profile: Profile,
    stop: Option<&'static AtomicBool>,
) -> Result<usize, String> {
    discover_subvolumes(source, profile, stop).map(|set| {
        set.subvolumes
            .iter()
            .filter(|subvolume| subvolume.pending_deletion)
            .count()
    })
}

pub(crate) fn discover_subvolumes(
    source: &mut LiveSource,
    profile: Profile,
    stop: Option<&'static AtomicBool>,
) -> Result<crate::btrfs::subvolume::SubvolumeSet, String> {
    let pacer = Pacer::new(
        profile.settings().duty_percent,
        SystemClock::default(),
        None,
    );
    let mut pacer = match stop {
        Some(stop) => pacer.with_stop_flag(stop),
        None => pacer,
    };
    crate::btrfs::subvolume::discover(source, &mut pacer).map_err(|error| error.to_string())
}

pub(crate) fn data_used(groups: &[BlockGroupSpace]) -> u64 {
    groups
        .iter()
        .filter(|group| group.flags & DATA_FLAG != 0)
        .fold(0, |used, group| used.saturating_add(group.used_bytes))
}

pub(crate) fn signed_delta(start: Option<u64>, end: Option<u64>) -> Option<i64> {
    match (start, end) {
        (Some(start), Some(end)) if end >= start => {
            Some(i64::try_from(end - start).unwrap_or(i64::MAX))
        }
        (Some(start), Some(end)) => Some(
            i64::try_from(start - end)
                .unwrap_or(i64::MAX)
                .saturating_neg(),
        ),
        _ => None,
    }
}

fn summarize(interval: Duration, samples: &[Sample]) -> Observation {
    let (first, last) = if samples.len() >= 2 {
        (samples.first(), samples.last())
    } else {
        (None, None)
    };
    Observation {
        interval_seconds: interval.as_secs(),
        samples: samples.to_vec(),
        data_used_delta_bytes: signed_delta(
            first.and_then(|sample| sample.data_used_bytes),
            last.and_then(|sample| sample.data_used_bytes),
        ),
        free_delta_bytes: signed_delta(
            first.and_then(|sample| sample.free_bytes),
            last.and_then(|sample| sample.free_bytes),
        ),
        pending_deletion_roots_delta: signed_delta(
            first
                .and_then(|sample| sample.pending_deletion_roots)
                .map(|count| count as u64),
            last.and_then(|sample| sample.pending_deletion_roots)
                .map(|count| count as u64),
        ),
        concurrent_writers_may_affect_delta: true,
    }
}

fn elapsed_millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn sleep_until(deadline: Instant, stop: &AtomicBool) {
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let now = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            return;
        };
        if remaining.is_zero() {
            return;
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

fn emit(json: bool, document: &WatchFreeDocument) -> Result<(), Box<dyn Error>> {
    let output = if json {
        serde_json::to_string_pretty(document)? + "\n"
    } else {
        render(document)
    };
    std::io::stdout().write_all(output.as_bytes())?;
    Ok(())
}

pub(crate) fn render(document: &WatchFreeDocument) -> String {
    render_observation(
        &document.raw_volume,
        &document.observation,
        document.observed_duration_ms,
        "watch-free",
    )
}

fn render_observation(
    volume: &Path,
    observation: &Observation,
    observed_duration_ms: u64,
    label: &str,
) -> String {
    let first = observation.samples.first();
    let last = observation.samples.last();
    let target = escape_human(volume.as_os_str().as_bytes());
    let mut output = format!(
        "{label} on {target} observed for {:.1}s ({} samples)\n",
        observed_duration_ms as f64 / 1000.0,
        observation.samples.len()
    );
    output.push_str(&render_metric(
        "Btrfs data used",
        first.and_then(|sample| sample.data_used_bytes),
        last.and_then(|sample| sample.data_used_bytes),
        observation.data_used_delta_bytes,
    ));
    output.push_str(&render_metric(
        "Filesystem free",
        first.and_then(|sample| sample.free_bytes),
        last.and_then(|sample| sample.free_bytes),
        observation.free_delta_bytes,
    ));
    output.push_str(&render_count_metric(
        "Pending deletion roots",
        first.and_then(|sample| sample.pending_deletion_roots),
        last.and_then(|sample| sample.pending_deletion_roots),
        observation.pending_deletion_roots_delta,
    ));
    let cleaner = last
        .and_then(|sample| sample.pending_deletion_roots)
        .map_or_else(
            || "pending-deletion status unknown".to_string(),
            |count| {
                if count == 0 {
                    "no pending-deletion roots visible".into()
                } else {
                    format!("{count} pending-deletion root(s); cleaner work remains")
                }
            },
        );
    output.push_str(&format!("Btrfs cleaner: {cleaner}\n"));
    output.push_str("Observed deltas may be affected by concurrent writers.\n");
    output
}

fn render_metric(label: &str, start: Option<u64>, end: Option<u64>, delta: Option<i64>) -> String {
    let start = start.map_or_else(|| "unknown".into(), human_bytes);
    let end = end.map_or_else(|| "unknown".into(), human_bytes);
    let delta = delta.map_or_else(|| "unknown".into(), signed_bytes);
    format!("{label}: {start} -> {end} ({delta})\n")
}

fn render_count_metric(
    label: &str,
    start: Option<usize>,
    end: Option<usize>,
    delta: Option<i64>,
) -> String {
    let start = start.map_or_else(|| "unknown".into(), |value| value.to_string());
    let end = end.map_or_else(|| "unknown".into(), |value| value.to_string());
    let delta = delta.map_or_else(|| "unknown".into(), signed_count);
    format!("{label}: {start} -> {end} ({delta})\n")
}

fn signed_bytes(delta: i64) -> String {
    let sign = if delta >= 0 { "+" } else { "-" };
    format!("{sign}{}", human_bytes(delta.unsigned_abs()))
}

fn signed_count(delta: i64) -> String {
    if delta >= 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::source::BlockGroupSpace;

    #[test]
    fn data_used_includes_combined_groups_with_the_data_flag_only() {
        let groups = [
            BlockGroupSpace {
                flags: 1,
                total_bytes: 100,
                used_bytes: 60,
            },
            BlockGroupSpace {
                flags: 4,
                total_bytes: 100,
                used_bytes: 50,
            },
            BlockGroupSpace {
                flags: 5,
                total_bytes: 100,
                used_bytes: 30,
            },
        ];

        assert_eq!(data_used(&groups), 90);
    }

    #[test]
    fn signed_delta_is_end_minus_start_and_preserves_unknown() {
        assert_eq!(signed_delta(Some(40), Some(67)), Some(27));
        assert_eq!(signed_delta(Some(67), Some(40)), Some(-27));
        assert_eq!(signed_delta(None, Some(67)), None);
        assert_eq!(signed_delta(Some(40), None), None);
    }

    #[test]
    fn a_single_sample_has_unknown_deltas_instead_of_claiming_zero_change() {
        let observation = summarize(Duration::from_secs(10), &[sample(0, 80, 500, 2)]);

        assert_eq!(observation.data_used_delta_bytes, None);
        assert_eq!(observation.free_delta_bytes, None);
        assert_eq!(observation.pending_deletion_roots_delta, None);
    }

    #[test]
    fn deltas_use_the_first_and_last_samples_and_keep_the_writer_label() {
        let observation = summarize(
            Duration::from_secs(2),
            &[sample(0, 100, 500, 2), sample(2000, 80, 520, 1)],
        );

        assert_eq!(observation.data_used_delta_bytes, Some(-20));
        assert_eq!(observation.free_delta_bytes, Some(20));
        assert_eq!(observation.pending_deletion_roots_delta, Some(-1));
        assert!(observation.concurrent_writers_may_affect_delta);
    }

    #[test]
    fn the_json_document_carries_the_standard_envelope() {
        let document = WatchFreeDocument {
            envelope: Envelope::new("watch-free", std::time::UNIX_EPOCH),
            volume: JsonPath::new(b"/volume1"),
            raw_volume: PathBuf::from("/volume1"),
            duration_seconds: 2,
            observed_duration_ms: 2000,
            interrupted: false,
            observation: summarize(
                Duration::from_secs(2),
                &[sample(0, 100, 500, 2), sample(2000, 80, 520, 1)],
            ),
        };
        let json = serde_json::to_value(&document).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["command"], "watch-free");
        assert_eq!(json["completion"], "complete");
        assert_eq!(json["tool_version"], env!("CARGO_PKG_VERSION"));
        assert!(json["errors"].as_array().unwrap().is_empty());
        assert!(json["warnings"].as_array().unwrap().is_empty());
        assert_eq!(json["samples"].as_array().unwrap().len(), 2);
        assert_eq!(json["volume"]["path"], "/volume1");
    }

    fn sample(elapsed_ms: u64, data_used: u64, free: u64, pending: usize) -> Sample {
        Sample {
            elapsed_ms,
            data_used_bytes: Some(data_used),
            data_used_error: None,
            free_bytes: Some(free),
            free_error: None,
            pending_deletion_roots: Some(pending),
            pending_deletion_error: None,
        }
    }
}
