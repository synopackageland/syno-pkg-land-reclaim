//! Resource profiles and cooperative pacing (SPEC 22.3). Pacing acts only
//! at batch boundaries: after each ioctl batch the scanner yields long
//! enough to keep its active share of wall time at the profile's duty.

use crate::btrfs::leaf_ranges::LeafReadPlan;
use crate::btrfs::walk::{BatchBoundary, StopReason};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Longest single sleep, so control requests are polled while yielding.
pub const YIELD_SLICE: Duration = Duration::from_millis(50);
const MAX_NICE: i32 = 19;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// Gentle on a busy NAS without waiting: nice +10, idle I/O, one leaf read at a time.
    #[default]
    Low,
    /// Unpaced at inherited priority, with the footprint of `low`.
    Normal,
    /// Finishes soonest: unpaced at inherited priority, pinned-byte leaves
    /// read in address-ordered batches on 4 threads, 64x the leaf cache.
    Fast,
}

/// Requested scheduling and pacing for a profile (SPEC 22.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileSettings {
    /// Added to the inherited nice value; never lowers it.
    pub nice_increment: i32,
    /// Request `IOPRIO_CLASS_IDLE`; otherwise keep the inherited class.
    pub idle_io: bool,
    /// Active share of wall time, in percent.
    pub duty_percent: u32,
    /// Initial TREE_SEARCH_V2 result buffer size in bytes.
    pub search_buffer: usize,
    /// How a volume overview batches its pinned-byte leaf reads.
    pub leaf_reads: LeafReadPlan,
}

impl Profile {
    /// The name as written on the command line and in JSON.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Low => "low",
            Profile::Normal => "normal",
            Profile::Fast => "fast",
        }
    }

    pub fn settings(self) -> ProfileSettings {
        match self {
            Profile::Low => ProfileSettings {
                nice_increment: 10,
                idle_io: true,
                duty_percent: 100,
                search_buffer: 256 * 1024,
                leaf_reads: LeafReadPlan::default(),
            },
            Profile::Normal => ProfileSettings {
                nice_increment: 0,
                idle_io: false,
                duty_percent: 100,
                search_buffer: 1024 * 1024,
                leaf_reads: LeafReadPlan::default(),
            },
            Profile::Fast => ProfileSettings {
                leaf_reads: LeafReadPlan {
                    window: 1024,
                    threads: 4,
                    cache_leaves: 16384,
                },
                ..Profile::Normal.settings()
            },
        }
    }
}

/// The nice value to request: inherited plus the increment, capped at 19,
/// and never below the inherited value.
pub fn nice_target(inherited: i32, increment: i32) -> i32 {
    inherited
        .saturating_add(increment.max(0))
        .min(MAX_NICE)
        .max(inherited)
}

/// Name of a Linux I/O priority class.
pub fn io_class_name(class: i32) -> &'static str {
    match class {
        0 => "none",
        1 => "realtime",
        2 => "best-effort",
        3 => "idle",
        _ => "unknown",
    }
}

/// Requested versus applied scheduling (SPEC 22.3). Accepted settings do
/// not prove reduced foreground impact, so effectiveness stays unknown.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SchedulingReport {
    pub profile: Profile,
    pub requested_nice: Option<i32>,
    pub applied_nice: Option<i32>,
    pub requested_io_class: &'static str,
    pub applied_io_class: Option<&'static str>,
    pub duty_percent: u32,
    pub warnings: Vec<String>,
    pub effectiveness_unknown: bool,
}

impl SchedulingReport {
    fn new(profile: Profile) -> Self {
        let settings = profile.settings();
        SchedulingReport {
            profile,
            requested_nice: None,
            applied_nice: None,
            requested_io_class: if settings.idle_io {
                "idle"
            } else {
                "inherited"
            },
            applied_io_class: None,
            duty_percent: settings.duty_percent,
            warnings: Vec::new(),
            effectiveness_unknown: true,
        }
    }

    /// Whether the kernel reports the requested settings as applied.
    pub fn applied_as_requested(&self) -> bool {
        let nice_ok = self.requested_nice.is_some() && self.applied_nice == self.requested_nice;
        let io_ok = self.requested_io_class == "inherited"
            || self.applied_io_class == Some(self.requested_io_class);
        nice_ok && io_ok
    }
}

/// The CPU and I/O priority this process started with, which a profile
/// adjusts and a later profile change goes back to.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inherited {
    pub nice: Option<i32>,
    pub io: Option<(i32, i32)>,
}

#[cfg(target_os = "linux")]
impl Inherited {
    /// The calling thread's priority now.
    pub fn read() -> Self {
        Inherited {
            nice: crate::sys::nice_value().ok(),
            io: crate::sys::io_priority().ok(),
        }
    }
}

/// Applies the profile's CPU and I/O priority to this process and reads
/// them back. Failures become warnings; pacing still applies. A later
/// call with another profile replaces it, from the priority the first
/// call found.
#[cfg(target_os = "linux")]
pub fn apply_profile(profile: Profile) -> SchedulingReport {
    static INHERITED: std::sync::OnceLock<Inherited> = std::sync::OnceLock::new();
    apply_profile_from(profile, INHERITED.get_or_init(Inherited::read))
}

/// [`apply_profile`] relative to `inherited`.
#[cfg(target_os = "linux")]
pub fn apply_profile_from(profile: Profile, inherited: &Inherited) -> SchedulingReport {
    use crate::sys;
    let settings = profile.settings();
    let mut report = SchedulingReport::new(profile);
    match inherited.nice {
        Some(inherited) => {
            let target = nice_target(inherited, settings.nice_increment);
            report.requested_nice = Some(target);
            if sys::nice_value().ok() != Some(target)
                && let Err(e) = sys::set_nice_value(target)
            {
                report
                    .warnings
                    .push(format!("setpriority({target}) failed: {e}"));
            }
            report.applied_nice = sys::nice_value().ok();
        }
        None => report.warnings.push("getpriority failed".to_string()),
    }
    let io = if settings.idle_io {
        Some((IOPRIO_CLASS_IDLE, 0))
    } else {
        inherited.io
    };
    if let Some((class, data)) = io
        && sys::io_priority().ok() != Some((class, data))
        && let Err(e) = sys::set_io_priority(class, data)
    {
        report
            .warnings
            .push(format!("ioprio_set({}) failed: {e}", io_class_name(class)));
    }
    report.applied_io_class = sys::io_priority()
        .ok()
        .map(|(class, _)| io_class_name(class));
    report
}

#[cfg(target_os = "linux")]
const IOPRIO_CLASS_IDLE: i32 = 3;

/// Monotonic time source, injectable for tests.
pub trait Clock {
    fn now(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
}

pub struct SystemClock(Instant);

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock(Instant::now())
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

static NEVER_STOP: AtomicBool = AtomicBool::new(false);

/// Batch boundary enforcing a duty cycle, a stop flag, and a runtime limit.
pub struct Pacer<C: Clock> {
    duty_percent: u32,
    clock: C,
    started: Duration,
    resumed: Duration,
    max_runtime: Option<Duration>,
    stop: &'static AtomicBool,
    memory: Option<MemoryWatch>,
}

/// Bytes the process uses now, or `None` when unknown.
pub type MemoryProbe = Box<dyn Fn() -> Option<u64>>;

struct MemoryWatch {
    limit: u64,
    probe: MemoryProbe,
    peak: Option<u64>,
    trip: Option<MemoryTrip>,
}

/// The memory check that stopped a walk (SPEC 22.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct MemoryTrip {
    pub budget_bytes: u64,
    pub resident_bytes: u64,
    /// Growth asked for; 0 when the resident set alone passed the budget.
    pub reserved_bytes: u64,
}

impl MemoryTrip {
    /// The use the check projected: resident plus the growth asked for.
    pub fn projected_bytes(&self) -> u64 {
        self.resident_bytes.saturating_add(self.reserved_bytes)
    }
}

impl MemoryWatch {
    /// Whether `used` plus `reserved` passes the limit, keeping the trip.
    fn trips(&mut self, used: u64, reserved: u64) -> bool {
        let trip = MemoryTrip {
            budget_bytes: self.limit,
            resident_bytes: used,
            reserved_bytes: reserved,
        };
        let tripped = trip.projected_bytes() > self.limit;
        if tripped {
            self.trip = Some(trip);
        }
        tripped
    }
}

impl<C: Clock> Pacer<C> {
    pub fn new(duty_percent: u32, clock: C, max_runtime: Option<Duration>) -> Self {
        let now = clock.now();
        Pacer {
            duty_percent: duty_percent.clamp(1, 100),
            clock,
            started: now,
            resumed: now,
            max_runtime,
            stop: &NEVER_STOP,
            memory: None,
        }
    }

    /// Stops the walk at the next boundary once `stop` becomes true. A
    /// static, so a signal handler can set it.
    pub fn with_stop_flag(mut self, stop: &'static AtomicBool) -> Self {
        self.stop = stop;
        self
    }

    /// Stops the walk once `probe` reports more than `limit` bytes.
    pub fn with_memory_limit(mut self, limit: u64, probe: MemoryProbe) -> Self {
        self.memory = Some(MemoryWatch {
            limit,
            probe,
            peak: None,
            trip: None,
        });
        self
    }

    /// The highest memory use observed at a boundary.
    pub fn peak_memory(&self) -> Option<u64> {
        self.memory.as_ref().and_then(|m| m.peak)
    }

    /// The memory check that stopped the walk, if one did.
    pub fn memory_trip(&self) -> Option<MemoryTrip> {
        self.memory.as_ref().and_then(|m| m.trip)
    }

    fn memory_exceeded(&mut self) -> bool {
        let Some(watch) = self.memory.as_mut() else {
            return false;
        };
        let Some(used) = (watch.probe)() else {
            return false;
        };
        watch.peak = watch.peak.max(Some(used));
        watch.trips(used, 0)
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn runtime_exceeded(&self) -> bool {
        self.max_runtime
            .is_some_and(|limit| self.clock.now() - self.started >= limit)
    }

    fn yield_for(&self, active: Duration) -> Duration {
        active.saturating_mul(100 - self.duty_percent) / self.duty_percent
    }

    /// Sleeps in slices; returns false if a stop request interrupted it.
    fn sleep_polling(&mut self, total: Duration) -> bool {
        let mut left = total;
        while !left.is_zero() {
            if self.stop_requested() {
                return false;
            }
            let slice = left.min(YIELD_SLICE);
            self.clock.sleep(slice);
            left -= slice;
        }
        true
    }
}

impl<C: Clock> BatchBoundary for Pacer<C> {
    fn reserve(&mut self, bytes: u64) -> ControlFlow<StopReason> {
        let Some(watch) = self.memory.as_mut() else {
            return ControlFlow::Continue(());
        };
        match (watch.probe)() {
            Some(used) if watch.trips(used, bytes) => ControlFlow::Break(StopReason::ResourceLimit),
            _ => ControlFlow::Continue(()),
        }
    }

    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        if self.stop_requested() {
            return ControlFlow::Break(StopReason::Paused);
        }
        if self.runtime_exceeded() {
            return ControlFlow::Break(StopReason::RuntimeLimit);
        }
        if self.memory_exceeded() {
            return ControlFlow::Break(StopReason::ResourceLimit);
        }
        let active = self.clock.now() - self.resumed;
        if !self.sleep_polling(self.yield_for(active)) {
            return ControlFlow::Break(StopReason::Paused);
        }
        self.resumed = self.clock.now();
        ControlFlow::Continue(())
    }
}

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
/// `--max-memory auto` ceiling (SPEC 22.4).
pub const AUTO_MEMORY_CAP: u64 = 512 * MIB;
/// Smallest budget a new full scan accepts (SPEC 22.4, initial value).
pub const MIN_MEMORY: u64 = 64 * MIB;

/// Parses a byte size: plain bytes or a K/M/G/T suffix (binary units,
/// optional `iB`/`B`), e.g. `512MiB`, `1G`, `65536`.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let unit = &text[digits.len()..];
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("invalid size {text:?}"))?;
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => KIB,
        "m" | "mb" | "mib" => MIB,
        "g" | "gb" | "gib" => 1024 * MIB,
        "t" | "tb" | "tib" => 1024 * 1024 * MIB,
        _ => return Err(format!("unknown size unit in {text:?}")),
    };
    value
        .checked_mul(multiplier)
        .ok_or_else(|| format!("size {text:?} is too large"))
}

/// Parses a duration with one unit: `90s`, `30m`, `2h`, `1d`.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("invalid duration {text:?}"))?;
    let seconds = match &text[digits.len()..] {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(format!("duration {text:?} needs a unit: s, m, h, or d")),
    };
    Ok(Duration::from_secs(value.saturating_mul(seconds)))
}

/// A `Name:   N kB` field of `/proc/self/status` or `/proc/meminfo`, in bytes.
pub fn proc_kib_field(text: &str, name: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix(name)?.strip_prefix(':')?;
        let kib = rest.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()?;
        kib.checked_mul(KIB)
    })
}

fn proc_field(path: &str, name: &str) -> Option<u64> {
    proc_kib_field(&std::fs::read_to_string(path).ok()?, name)
}

/// Resident set size of this process.
pub fn resident_bytes() -> Option<u64> {
    proc_field("/proc/self/status", "VmRSS")
}

/// Peak resident set size of this process.
pub fn peak_resident_bytes() -> Option<u64> {
    proc_field("/proc/self/status", "VmHWM")
}

/// Physical RAM of the host.
pub fn physical_ram() -> Option<u64> {
    proc_field("/proc/meminfo", "MemTotal")
}

/// `--max-memory`: the process working-set budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryLimit {
    #[default]
    Auto,
    Bytes(u64),
}

impl std::str::FromStr for MemoryLimit {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        if text == "auto" {
            Ok(MemoryLimit::Auto)
        } else {
            parse_size(text).map(MemoryLimit::Bytes)
        }
    }
}

impl MemoryLimit {
    /// The budget in bytes: `auto` is the smaller of 512 MiB and one eighth
    /// of physical RAM, and must reach the tested minimum. Explicit values
    /// are the operator's choice.
    pub fn resolve(self, physical_ram: u64) -> Result<u64, String> {
        match self {
            MemoryLimit::Bytes(bytes) => Ok(bytes),
            MemoryLimit::Auto => {
                let budget = AUTO_MEMORY_CAP.min(physical_ram / 8);
                if budget < MIN_MEMORY {
                    return Err(format!(
                        "automatic memory budget {} is below the {} minimum; pass --max-memory",
                        crate::output::human_bytes(budget),
                        crate::output::human_bytes(MIN_MEMORY)
                    ));
                }
                Ok(budget)
            }
        }
    }
}

/// A clock that only moves when told to or slept on, for pacing tests.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct FakeClock {
    now: std::rc::Rc<std::cell::RefCell<Duration>>,
    pub slept: std::rc::Rc<std::cell::RefCell<Vec<Duration>>>,
}

#[cfg(test)]
impl FakeClock {
    pub fn advance(&self, d: Duration) {
        *self.now.borrow_mut() += d;
    }
    pub fn total_slept(&self) -> Duration {
        self.slept.borrow().iter().sum()
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now(&self) -> Duration {
        *self.now.borrow()
    }
    fn sleep(&mut self, d: Duration) {
        self.slept.borrow_mut().push(d);
        self.advance(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::leaf_ranges::LeafReadPlan;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn parses_binary_sizes() {
        assert_eq!(parse_size("65536"), Ok(65536));
        assert_eq!(parse_size("512MiB"), Ok(512 * MIB));
        assert_eq!(parse_size("256M"), Ok(256 * MIB));
        assert_eq!(parse_size("1g"), Ok(1024 * MIB));
        assert_eq!(parse_size("4KB"), Ok(4096));
        assert!(parse_size("12XB").is_err());
        assert!(parse_size("MiB").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn parses_single_unit_durations() {
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1d"), Ok(Duration::from_secs(86_400)));
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("m").is_err());
    }

    #[test]
    fn automatic_memory_budget_is_an_eighth_of_ram_up_to_512_mib() {
        assert_eq!(MemoryLimit::Auto.resolve(16 * 1024 * MIB), Ok(512 * MIB));
        assert_eq!(MemoryLimit::Auto.resolve(1753 * MIB), Ok(1753 * MIB / 8));
        assert!(MemoryLimit::Auto.resolve(256 * MIB).is_err());
        assert_eq!(
            MemoryLimit::Bytes(32 * MIB).resolve(256 * MIB),
            Ok(32 * MIB)
        );
        assert_eq!("auto".parse(), Ok(MemoryLimit::Auto));
        assert_eq!("256MiB".parse(), Ok(MemoryLimit::Bytes(256 * MIB)));
    }

    #[test]
    fn low_profile_is_gentle_without_waiting() {
        let low = Profile::Low.settings();
        assert_eq!(low.nice_increment, 10);
        assert!(low.idle_io);
        assert_eq!(low.duty_percent, 100);
        assert_eq!(low.search_buffer, 256 * 1024);
    }

    #[test]
    fn normal_profile_keeps_inherited_priority_and_full_duty() {
        let normal = Profile::Normal.settings();
        assert_eq!(normal.nice_increment, 0);
        assert!(!normal.idle_io);
        assert_eq!(normal.duty_percent, 100);
        assert_eq!(normal.search_buffer, 1024 * 1024);
    }

    #[test]
    fn fast_profile_reads_pinned_leaves_in_parallel_batches_with_a_larger_cache() {
        let fast = Profile::Fast.settings();
        assert_eq!(fast.nice_increment, 0);
        assert!(!fast.idle_io);
        assert_eq!(fast.duty_percent, 100);
        assert_eq!(
            fast.leaf_reads,
            LeafReadPlan {
                window: 1024,
                threads: 4,
                cache_leaves: 16384,
            }
        );
        assert_eq!(Profile::Low.settings().leaf_reads, LeafReadPlan::default());
        assert_eq!(
            Profile::Normal.settings().leaf_reads,
            LeafReadPlan::default()
        );
        assert_eq!(Profile::Fast.name(), "fast");
    }

    #[test]
    fn nice_target_only_ever_lowers_priority() {
        assert_eq!(nice_target(0, 10), 10);
        assert_eq!(nice_target(15, 10), 19);
        assert_eq!(nice_target(-5, 0), -5);
    }

    #[test]
    fn names_io_priority_classes() {
        assert_eq!(io_class_name(3), "idle");
        assert_eq!(io_class_name(2), "best-effort");
        assert_eq!(io_class_name(0), "none");
        assert_eq!(io_class_name(9), "unknown");
    }

    #[test]
    fn scheduling_matches_request_only_when_nice_and_io_class_applied() {
        let mut report = SchedulingReport::new(Profile::Low);
        report.requested_nice = Some(10);
        report.applied_nice = Some(10);
        report.applied_io_class = Some("idle");
        assert!(report.applied_as_requested());
        report.applied_io_class = Some("best-effort");
        assert!(!report.applied_as_requested());
        let mut normal = SchedulingReport::new(Profile::Normal);
        normal.requested_nice = Some(0);
        normal.applied_nice = Some(0);
        assert!(normal.applied_as_requested());
    }

    #[cfg(target_os = "linux")]
    fn may_lower_nice() -> bool {
        const CAP_SYS_NICE: u32 = 23;
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                let hex = status.lines().find_map(|l| l.strip_prefix("CapEff:"))?;
                u64::from_str_radix(hex.trim(), 16).ok()
            })
            .is_some_and(|effective| effective & (1 << CAP_SYS_NICE) != 0)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_profile_change_goes_back_to_the_inherited_priority() {
        // Nice and I/O priority are per thread: this one is the test's own.
        std::thread::spawn(|| {
            let inherited = Inherited::read();
            let gentle = apply_profile_from(Profile::Low, &inherited);
            assert_eq!(gentle.applied_io_class, Some("idle"), "{gentle:?}");
            let fast = apply_profile_from(Profile::Fast, &inherited);
            // Lowering nice again takes CAP_SYS_NICE, which the executor's
            // root has and a default container drops.
            if may_lower_nice() {
                assert!(fast.applied_as_requested(), "{fast:?}");
                assert_eq!(fast.applied_nice, inherited.nice);
                assert_ne!(fast.applied_io_class, Some("idle"), "{fast:?}");
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    fn quarter_duty_yields_three_times_the_active_time() {
        let clock = FakeClock::default();
        let mut pacer = Pacer::new(25, clock.clone(), None);
        clock.advance(ms(40));
        assert!(pacer.after_batch().is_continue());
        assert_eq!(clock.total_slept(), ms(120));
    }

    #[test]
    fn no_profile_sleeps_between_batches() {
        for profile in [Profile::Low, Profile::Normal, Profile::Fast] {
            let clock = FakeClock::default();
            let mut pacer = Pacer::new(profile.settings().duty_percent, clock.clone(), None);
            clock.advance(ms(40));
            assert!(pacer.after_batch().is_continue());
            assert_eq!(clock.total_slept(), Duration::ZERO, "{}", profile.name());
        }
    }

    #[test]
    fn full_duty_never_sleeps() {
        let clock = FakeClock::default();
        let mut pacer = Pacer::new(100, clock.clone(), None);
        clock.advance(ms(40));
        assert!(pacer.after_batch().is_continue());
        assert_eq!(clock.total_slept(), Duration::ZERO);
    }

    #[test]
    fn active_time_is_measured_from_the_end_of_the_last_yield() {
        let clock = FakeClock::default();
        let mut pacer = Pacer::new(50, clock.clone(), None);
        clock.advance(ms(10));
        let _ = pacer.after_batch(); // sleeps 10
        clock.advance(ms(30));
        let _ = pacer.after_batch(); // sleeps 30
        assert_eq!(clock.total_slept(), ms(40));
    }

    #[test]
    fn yields_are_sliced_so_control_requests_are_polled() {
        let clock = FakeClock::default();
        let mut pacer = Pacer::new(25, clock.clone(), None);
        clock.advance(ms(100));
        let _ = pacer.after_batch();
        assert!(clock.slept.borrow().iter().all(|d| *d <= YIELD_SLICE));
        assert_eq!(clock.total_slept(), ms(300));
    }

    #[test]
    fn stop_request_ends_the_walk_at_the_next_boundary() {
        let clock = FakeClock::default();
        static STOP: AtomicBool = AtomicBool::new(false);
        let mut pacer = Pacer::new(25, clock.clone(), None).with_stop_flag(&STOP);
        assert!(pacer.after_batch().is_continue());
        STOP.store(true, Ordering::Relaxed);
        assert_eq!(pacer.after_batch(), ControlFlow::Break(StopReason::Paused));
    }

    #[test]
    fn reads_kib_fields_of_proc_status_and_meminfo() {
        let status = "Name:\tsynoreclaim\nVmHWM:\t   9000 kB\nVmRSS:\t   8192 kB\n";
        assert_eq!(proc_kib_field(status, "VmRSS"), Some(8192 * 1024));
        assert_eq!(proc_kib_field(status, "VmHWM"), Some(9000 * 1024));
        assert_eq!(
            proc_kib_field("MemTotal:  1795000 kB\n", "MemTotal"),
            Some(1795000 * 1024)
        );
        assert_eq!(proc_kib_field(status, "VmSwap"), None);
    }

    #[test]
    fn memory_over_budget_stops_with_a_resource_limit() {
        use std::cell::Cell;
        use std::rc::Rc;
        let clock = FakeClock::default();
        let used = Rc::new(Cell::new(10));
        let probe = used.clone();
        let mut pacer =
            Pacer::new(100, clock, None).with_memory_limit(10, Box::new(move || Some(probe.get())));
        assert!(pacer.after_batch().is_continue());
        used.set(11);
        assert_eq!(
            pacer.after_batch(),
            ControlFlow::Break(StopReason::ResourceLimit)
        );
        assert_eq!(pacer.peak_memory(), Some(11));
    }

    #[test]
    fn reserving_past_the_budget_stops_before_growing() {
        let clock = FakeClock::default();
        let mut pacer = Pacer::new(100, clock, None).with_memory_limit(10, Box::new(|| Some(6)));
        assert!(pacer.reserve(4).is_continue());
        assert_eq!(
            pacer.reserve(5),
            ControlFlow::Break(StopReason::ResourceLimit)
        );
        let mut unlimited = Pacer::new(100, FakeClock::default(), None);
        assert!(unlimited.reserve(u64::MAX).is_continue());
    }

    #[test]
    fn the_memory_check_that_stopped_is_kept() {
        let mut pacer =
            Pacer::new(100, FakeClock::default(), None).with_memory_limit(10, Box::new(|| Some(6)));
        assert!(pacer.reserve(4).is_continue());
        assert_eq!(pacer.memory_trip(), None);
        let _ = pacer.reserve(5);
        let trip = MemoryTrip {
            budget_bytes: 10,
            resident_bytes: 6,
            reserved_bytes: 5,
        };
        assert_eq!(pacer.memory_trip(), Some(trip));
        assert_eq!(trip.projected_bytes(), 11);

        let mut rss = Pacer::new(100, FakeClock::default(), None)
            .with_memory_limit(10, Box::new(|| Some(12)));
        let _ = rss.after_batch();
        assert_eq!(
            rss.memory_trip(),
            Some(MemoryTrip {
                budget_bytes: 10,
                resident_bytes: 12,
                reserved_bytes: 0,
            })
        );
    }

    #[test]
    fn runtime_limit_stops_between_batches() {
        let clock = FakeClock::default();
        let mut pacer = Pacer::new(100, clock.clone(), Some(ms(50)));
        clock.advance(ms(49));
        assert!(pacer.after_batch().is_continue());
        clock.advance(ms(1));
        assert_eq!(
            pacer.after_batch(),
            ControlFlow::Break(StopReason::RuntimeLimit)
        );
    }
}
