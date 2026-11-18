//! DSM's own cleaners that `rootfs apply` may run after its moves: each a
//! fixed program and fixed arguments, never a shell, chosen by what the
//! DSM build ships and by whether syslog-ng holds deleted logs open.

use crate::why_not_freed::ProcessScan;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// A DSM tool run; its argv is fixed per variant, so a plan names only
/// the variant and can never carry a command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delegate {
    /// `synocleaner --rotate-log`: rotates DSM's logs once via logrotate.
    RotateLogs,
    /// DSM upgrade download API cleans its own downloaded nano patch.
    CleanDownloadedUpdate,
    /// DSM 7.1/7.1.1 releases its own temporary upgrade backing file.
    ReleaseUpdateReserve,
    /// `synocleaner --delete-journal`: deletes obsolete systemd journals.
    DeleteJournal,
    /// `logrotate -f /etc/logrotate.conf`, where synocleaner is absent.
    Logrotate,
    /// `synosystemctl restart syslog-ng`, only while it holds deleted logs.
    RestartSyslogNg,
    /// `synocleaner --delete-log`: deletes the logs DSM finds older than it
    /// keeps, as DSM itself does before an update. Deep only.
    DeleteLog,
}

pub const SYNOCLEANER: &str = "/usr/syno/bin/synocleaner";
pub const LOGROTATE: &str = "/usr/bin/logrotate";
pub const SYNOSYSTEMCTL: &str = "/usr/syno/bin/synosystemctl";

/// DSM 6.2.3, the first build whose synocleaner rotates logs and deletes
/// journals.
pub const SYNOCLEANER_MIN_BUILD: u32 = 25423;

impl Delegate {
    pub fn program(self) -> &'static str {
        match self {
            Delegate::RotateLogs | Delegate::DeleteJournal | Delegate::DeleteLog => SYNOCLEANER,
            Delegate::CleanDownloadedUpdate => "/usr/syno/bin/synowebapi",
            Delegate::ReleaseUpdateReserve => "/usr/syno/sbin/synoupgradepreserve",
            Delegate::Logrotate => LOGROTATE,
            Delegate::RestartSyslogNg => SYNOSYSTEMCTL,
        }
    }

    pub fn args(self) -> &'static [&'static str] {
        match self {
            Delegate::RotateLogs => &["--rotate-log"],
            Delegate::DeleteJournal => &["--delete-journal"],
            Delegate::DeleteLog => &["--delete-log"],
            Delegate::CleanDownloadedUpdate => &[
                "--exec",
                "api=SYNO.Core.Upgrade.Server.Download",
                "method=clean",
            ],
            Delegate::ReleaseUpdateReserve => &["--cleanup-backing-file"],
            Delegate::Logrotate => &["-f", "/etc/logrotate.conf"],
            Delegate::RestartSyslogNg => &["restart", "syslog-ng"],
        }
    }

    /// What the run does, for the plan.
    pub fn effect(self) -> &'static str {
        match self {
            Delegate::RotateLogs => {
                "rotates DSM's logs once; logs rotated out past their count are deleted"
            }
            Delegate::CleanDownloadedUpdate => {
                "cleans DSM downloaded nano-update cache using its own API; download again before updating"
            }
            Delegate::ReleaseUpdateReserve => {
                "releases DSM 7.1 upgrade backing space; update DSM soon; reboot may recreate the reserve"
            }
            Delegate::DeleteJournal => "deletes obsolete systemd journal files",
            Delegate::DeleteLog => {
                "deletes the logs DSM finds older than it keeps, as DSM does before an update; there is no copy"
            }
            Delegate::Logrotate => {
                "rotates the logs in /etc/logrotate.conf once; old rotations are deleted"
            }
            Delegate::RestartSyslogNg => {
                "restarts syslog-ng so the deleted logs it holds open are freed; logging pauses briefly"
            }
        }
    }

    /// The program followed by its arguments.
    pub fn argv(self) -> Vec<&'static str> {
        std::iter::once(self.program())
            .chain(self.args().iter().copied())
            .collect()
    }

    /// The same run with DSM's `--dry-run`, which lists what it would
    /// delete and deletes nothing; only for runs that list files.
    pub fn dry_run_argv(self) -> Option<Vec<&'static str>> {
        (self == Delegate::DeleteLog).then(|| {
            std::iter::once(self.program())
                .chain(std::iter::once("--dry-run"))
                .chain(self.args().iter().copied())
                .collect()
        })
    }
}

/// Deep runs DSM's own old-log deletion first, as DSM's own pre-update
/// cleanup does, where the build's synocleaner has it.
pub fn with_delete_log(
    runs: Vec<Delegate>,
    mode: crate::rootfs::plan::CleanupMode,
    build: Option<u32>,
    trusted: impl Fn(&str) -> bool,
) -> Vec<Delegate> {
    if mode.is_standard()
        || !build.is_some_and(|b| b >= SYNOCLEANER_MIN_BUILD)
        || !trusted(SYNOCLEANER)
    {
        return runs;
    }
    std::iter::once(Delegate::DeleteLog).chain(runs).collect()
}

/// What `stat` says about a path a dry run listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListedFile {
    pub device: u64,
    pub inode: u64,
    pub regular: bool,
    pub allocated_bytes: u64,
}

/// The allocated bytes of the regular files on `root_device` that a dry
/// run's `output` lists one absolute path per line, each inode once,
/// leaving out the paths the plan `moved` first. Other lines are DSM's
/// messages.
pub fn dry_run_bytes(
    output: &[u8],
    moved: &std::collections::BTreeSet<Vec<u8>>,
    root_device: u64,
    stat: impl Fn(&[u8]) -> Option<ListedFile>,
) -> u64 {
    let mut seen = std::collections::BTreeSet::new();
    output
        .split(|&b| b == b'\n')
        .filter(|line| line.starts_with(b"/") && !moved.contains(*line))
        .filter_map(stat)
        .filter(|f| f.regular && f.device == root_device && seen.insert(f.inode))
        .map(|f| f.allocated_bytes)
        .fold(0, u64::saturating_add)
}

/// What the tool's dry run would delete on `/` beyond the plan's `moved`
/// paths; `None` when the tool has no dry run or it did not succeed.
pub fn dry_run_estimate(
    tool: Delegate,
    moved: &std::collections::BTreeSet<Vec<u8>>,
) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let argv = tool.dry_run_argv()?;
    if !trusted_program(Path::new(argv[0]), 0) {
        return None;
    }
    let output = run_capture(argv[0], &argv[1..], DRY_RUN_OUTPUT_BYTES)?;
    let root = std::fs::metadata("/").ok()?.dev();
    Some(dry_run_bytes(&output, moved, root, |path| {
        let meta = std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(path)).ok()?;
        Some(ListedFile {
            device: meta.dev(),
            inode: meta.ino(),
            regular: meta.file_type().is_file(),
            allocated_bytes: meta.blocks().saturating_mul(512),
        })
    }))
}

/// A dry run's listing larger than this is cut off; on DSM it is a few
/// hundred paths.
const DRY_RUN_OUTPUT_BYTES: usize = 4 << 20;

/// The runs for a DSM build (`None` off DSM: none), given which programs
/// are trusted and whether syslog-ng holds deleted files on `/`.
pub fn select(
    dsm_build: Option<u32>,
    trusted: impl Fn(&str) -> bool,
    syslog_holds_deleted: bool,
) -> Vec<Delegate> {
    let Some(build) = dsm_build else {
        return Vec::new();
    };
    let mut runs = if build >= SYNOCLEANER_MIN_BUILD && trusted(SYNOCLEANER) {
        vec![Delegate::RotateLogs, Delegate::DeleteJournal]
    } else if trusted(LOGROTATE) {
        vec![Delegate::Logrotate]
    } else {
        Vec::new()
    };
    if syslog_holds_deleted && trusted(SYNOSYSTEMCTL) {
        runs.push(Delegate::RestartSyslogNg);
    }
    runs
}

/// Deep requests forced rotation using the existing, fixed logrotate argv.
/// Retain normal DSM cleanup when that trusted executable is unavailable.
pub fn for_mode(
    mut runs: Vec<Delegate>,
    mode: crate::rootfs::plan::CleanupMode,
    force_available: bool,
) -> Vec<Delegate> {
    if mode == crate::rootfs::plan::CleanupMode::Deep && force_available {
        for tool in &mut runs {
            if *tool == Delegate::RotateLogs {
                *tool = Delegate::Logrotate;
            }
        }
    }
    runs
}

pub const DOWNLOADED_PATCH: &str = "/var/update/downloaded_autonano.pat";
pub const UPDATE_RESERVE: &str = "/usr/syno/synoinstall/space-preserve";

/// Closed KB operations: presence and supported DSM version are required.
pub fn kb_tools(
    mode: crate::rootfs::plan::CleanupMode,
    build: Option<u32>,
    present: impl Fn(&str) -> bool,
    trusted: impl Fn(&str) -> bool,
) -> Vec<Delegate> {
    if mode.is_standard() || build.is_none() {
        return vec![];
    }
    let mut tools = Vec::new();
    if present(DOWNLOADED_PATCH) && trusted(Delegate::CleanDownloadedUpdate.program()) {
        tools.push(Delegate::CleanDownloadedUpdate);
    }
    if build.is_some_and(|b| (42661..64570).contains(&b))
        && present(UPDATE_RESERVE)
        && trusted(Delegate::ReleaseUpdateReserve.program())
    {
        tools.push(Delegate::ReleaseUpdateReserve);
    }
    tools
}

/// Only an unsymlinked root-owned regular file on rootfs is evidence for a
/// fixed DSM cleanup tool; never let a volume-backed path add an operation.
pub fn fixed_file_bytes(path: &str) -> Option<u64> {
    let file = crate::guard::open_absolute_no_follow(Path::new(path)).ok()?;
    let m = file.metadata().ok()?;
    let root = std::fs::metadata("/").ok()?;
    (m.is_file()
        && m.dev() == root.dev()
        && m.uid() == 0
        && m.nlink() == 1
        && m.mode() & 0o022 == 0)
        .then_some(m.blocks().saturating_mul(512))
}

pub fn estimated_bytes(tool: Delegate) -> Option<u64> {
    match tool {
        Delegate::CleanDownloadedUpdate => fixed_file_bytes(DOWNLOADED_PATCH),
        Delegate::ReleaseUpdateReserve => fixed_file_bytes(UPDATE_RESERVE),
        _ => None,
    }
}

pub fn available_mode(scan: &ProcessScan, mode: crate::rootfs::plan::CleanupMode) -> Vec<Delegate> {
    let version = std::fs::read_to_string(crate::cache::DSM_VERSION_FILE).ok();
    let build = version.as_deref().and_then(crate::cache::dsm_build);
    let mut runs = with_delete_log(
        for_mode(
            available(scan),
            mode,
            trusted_program(Path::new(LOGROTATE), 0),
        ),
        mode,
        build,
        |program| trusted_program(Path::new(program), 0),
    );
    runs.extend(kb_tools(
        mode,
        build,
        |path| fixed_file_bytes(path).is_some_and(|bytes| bytes > 0),
        |program| trusted_program(Path::new(program), 0),
    ));
    runs
}

/// Whether syslog-ng is among the holders of deleted files in `scan`.
pub fn syslog_ng_holds(scan: &ProcessScan) -> bool {
    scan.holders
        .iter()
        .any(|holder| holder.comm == "syslog-ng" && holder.inodes > 0)
}

/// The runs this host allows: programs trusted as root's, the build read
/// from DSM's version file.
pub fn available(scan: &ProcessScan) -> Vec<Delegate> {
    let version = std::fs::read_to_string(crate::cache::DSM_VERSION_FILE).ok();
    select(
        version.as_deref().and_then(crate::cache::dsm_build),
        |program| trusted_program(Path::new(program), 0),
        syslog_ng_holds(scan),
    )
}

/// A program is trusted only as a regular file, not a symlink, owned by
/// `owner`, executable and writable by no one else.
pub fn trusted_program(path: &Path, owner: u32) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_file()
            && meta.uid() == owner
            && meta.mode() & 0o022 == 0
            && meta.mode() & 0o100 != 0
    })
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ran {
    Exited(i32),
    Killed(i32),
    NotStarted,
}

/// Runs `program args` without a shell, from `/`, with no input, its output
/// discarded and only a fixed PATH and LANG=C for environment. Slowness is
/// not failure; an explicit stop kills the process group.
pub fn run(program: &str, args: &[&str]) -> Ran {
    run_until(program, args, || false)
}

fn run_until(program: &str, args: &[&str], mut cancelled: impl FnMut() -> bool) -> Ran {
    let started = Command::new(program)
        .args(args)
        .env_clear()
        .env(
            "PATH",
            "/usr/syno/sbin:/usr/syno/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("LANG", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    let Ok(mut child) = started else {
        return Ran::NotStarted;
    };
    match wait_child(&mut child, || {
        cancelled() || crate::sys::stop_signal().is_some()
    }) {
        Ok(status) => status
            .code()
            .map_or_else(|| Ran::Killed(status.signal().unwrap_or(0)), Ran::Exited),
        Err(_) => Ran::NotStarted,
    }
}

/// Runs `program args` as `run` does, but returns its standard output, at
/// most `max` bytes of it; `None` unless it exits 0.
pub fn run_capture(program: &str, args: &[&str], max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env(
            "PATH",
            "/usr/syno/sbin:/usr/syno/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("LANG", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut limited = stdout.take(max as u64);
        let _ = limited.read_to_end(&mut out);
        // Drain the rest so the tool never blocks on a full pipe.
        let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
        out
    });
    let status = wait_child(&mut child, || crate::sys::stop_signal().is_some()).ok();
    let out = reader.join().ok()?;
    status.filter(|s| s.success()).map(|_| out)
}

/// Poll for completion or an explicit stop, with no operation deadline.
pub(crate) fn wait_child(
    child: &mut std::process::Child,
    mut cancelled: impl FnMut() -> bool,
) -> std::io::Result<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if !cancelled() => std::thread::sleep(Duration::from_millis(50)),
            result => {
                let _ = crate::sys::signal_group(child.id(), libc::SIGKILL);
                let status = child.wait();
                return match result {
                    Err(error) => Err(error),
                    _ => status,
                };
            }
        }
    }
}

/// What became of one planned run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolOutcome {
    Succeeded,
    Failed {
        exit_code: i32,
    },
    Killed {
        signal: i32,
    },
    /// Historical reports may contain this; current runs have no deadline.
    TimedOut,
    NotStarted,
    /// Its program is no longer root's unwritable executable: not run.
    Untrusted,
    /// syslog-ng no longer holds deleted files: not restarted.
    NotNeeded,
}

impl ToolOutcome {
    pub fn succeeded(self) -> bool {
        matches!(self, ToolOutcome::Succeeded | ToolOutcome::NotNeeded)
    }
}

impl From<Ran> for ToolOutcome {
    fn from(ran: Ran) -> Self {
        match ran {
            Ran::Exited(0) => ToolOutcome::Succeeded,
            Ran::Exited(exit_code) => ToolOutcome::Failed { exit_code },
            Ran::Killed(signal) => ToolOutcome::Killed { signal },
            Ran::NotStarted => ToolOutcome::NotStarted,
        }
    }
}

/// Runs one planned tool; tests stand in a fake.
pub trait Tools {
    fn run(&mut self, tool: Delegate) -> ToolOutcome;
}

/// The host's tools, each checked again right before it runs: the
/// program must still be `owner`'s, and syslog-ng is restarted only if
/// it still holds deleted files on `root_device`.
pub struct HostTools<'a> {
    pub owner: u32,
    pub proc_root: &'a Path,
    pub root_device: u64,
    pub cancelled: Option<&'a dyn Fn() -> bool>,
}

impl Tools for HostTools<'_> {
    fn run(&mut self, tool: Delegate) -> ToolOutcome {
        if matches!(
            tool,
            Delegate::CleanDownloadedUpdate | Delegate::ReleaseUpdateReserve
        ) && estimated_bytes(tool).is_none()
        {
            return ToolOutcome::NotNeeded;
        }
        if tool == Delegate::ReleaseUpdateReserve {
            let version = std::fs::read_to_string(crate::cache::DSM_VERSION_FILE).ok();
            if !version
                .as_deref()
                .and_then(crate::cache::dsm_build)
                .is_some_and(|build| (42661..64570).contains(&build))
            {
                return ToolOutcome::NotNeeded;
            }
        }
        if tool == Delegate::RestartSyslogNg {
            let (found, unreadable) = crate::open_files::deleted_files(self.proc_root, |file| {
                file.metadata()
                    .is_ok_and(|meta| meta.dev() == self.root_device)
            });
            if !syslog_ng_holds(&crate::why_not_freed::process_scan(&found, unreadable)) {
                return ToolOutcome::NotNeeded;
            }
        }
        if !trusted_program(Path::new(tool.program()), self.owner) {
            return ToolOutcome::Untrusted;
        }
        run_until(tool.program(), tool.args(), || {
            self.cancelled.is_some_and(|cancelled| cancelled())
        })
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_trusted_only_as_its_owner_s_unwritable_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::spill::testing::scratch("delegate-trust");
        let program = dir.join("tool");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        let owner = std::fs::metadata(&program).unwrap().uid();
        let mode = |m| std::fs::set_permissions(&program, std::fs::Permissions::from_mode(m));
        mode(0o755).unwrap();
        assert!(trusted_program(&program, owner));
        assert!(!trusted_program(&program, owner + 1));
        mode(0o775).unwrap();
        assert!(!trusted_program(&program, owner));
        mode(0o644).unwrap();
        assert!(!trusted_program(&program, owner));
        mode(0o755).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&program, &link).unwrap();
        assert!(!trusted_program(&link, owner));
        assert!(!trusted_program(&dir, owner));
        assert!(!trusted_program(&dir.join("absent"), owner));
    }

    #[test]
    fn a_run_reports_its_exit_with_a_cleared_environment() {
        let script = "[ -z \"$HOME\" ] && [ \"$LANG\" = C ] && [ \"$(pwd)\" = / ] && exit 3";
        assert_eq!(run("/bin/sh", &["-c", script]), Ran::Exited(3));
        assert_eq!(run("/bin/sh", &["-c", "kill -9 $$"]), Ran::Killed(9));
        assert_eq!(run("/nonexistent/tool", &[]), Ran::NotStarted);
    }

    #[test]
    fn a_slow_tool_is_not_killed_by_elapsed_time() {
        assert_eq!(run("/bin/sh", &["-c", "sleep 1; exit 0"]), Ran::Exited(0));
        assert_eq!(
            run_capture("/bin/sh", &["-c", "sleep 1; echo data"], 1024),
            Some(b"data\n".to_vec())
        );
    }

    #[test]
    fn only_syslog_ng_holding_deleted_files_asks_for_its_restart() {
        use crate::why_not_freed::{OpenDeleted, process_scan};
        let held = |comm: &[u8]| {
            process_scan(
                &[OpenDeleted {
                    pid: 7,
                    comm: comm.to_vec(),
                    dev: 1,
                    ino: 2,
                    allocated_bytes: 4096,
                }],
                0,
            )
        };
        assert!(syslog_ng_holds(&held(b"syslog-ng")));
        assert!(!syslog_ng_holds(&held(b"smbd")));
        assert!(!syslog_ng_holds(&process_scan(&[], 0)));
    }

    #[test]
    fn an_exit_status_becomes_the_run_s_outcome() {
        assert_eq!(ToolOutcome::from(Ran::Exited(0)), ToolOutcome::Succeeded);
        assert_eq!(
            ToolOutcome::from(Ran::Exited(2)),
            ToolOutcome::Failed { exit_code: 2 }
        );
        assert_eq!(
            ToolOutcome::from(Ran::Killed(9)),
            ToolOutcome::Killed { signal: 9 }
        );
        assert_eq!(
            serde_json::to_value(ToolOutcome::Failed { exit_code: 2 }).unwrap(),
            serde_json::json!({"outcome": "failed", "exit_code": 2})
        );
    }

    // These never start a real DSM tool: each is refused before it runs.
    #[test]
    fn the_host_checks_each_tool_again_right_before_it_runs() {
        let proc_root = crate::spill::testing::scratch("delegate-no-proc");
        let mut host = HostTools {
            owner: u32::MAX,
            proc_root: &proc_root,
            root_device: 1,
            cancelled: None,
        };
        assert_eq!(host.run(Delegate::Logrotate), ToolOutcome::Untrusted);
        assert_eq!(host.run(Delegate::RotateLogs), ToolOutcome::Untrusted);
        host.owner = 0;
        assert_eq!(host.run(Delegate::RestartSyslogNg), ToolOutcome::NotNeeded);
    }

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn kb_update_cleaners_are_fixed_and_only_enabled_for_present_supported_files() {
        use crate::rootfs::plan::CleanupMode::{Deep, Standard};
        assert!(kb_tools(Standard, Some(42962), |_| true, |_| true).is_empty());
        assert!(kb_tools(Deep, None, |_| true, |_| true).is_empty());
        assert_eq!(
            kb_tools(Deep, Some(42962), |_| true, |_| true),
            [
                Delegate::CleanDownloadedUpdate,
                Delegate::ReleaseUpdateReserve
            ]
        );
        assert_eq!(
            kb_tools(Deep, Some(90069), |_| true, |_| true),
            [Delegate::CleanDownloadedUpdate]
        );
        assert!(kb_tools(Deep, Some(42962), |_| false, |_| true).is_empty());
        assert!(kb_tools(Deep, Some(42962), |_| true, |_| false).is_empty());
        assert_eq!(
            Delegate::CleanDownloadedUpdate.args(),
            [
                "--exec",
                "api=SYNO.Core.Upgrade.Server.Download",
                "method=clean"
            ]
        );
        assert_eq!(
            Delegate::ReleaseUpdateReserve.args(),
            ["--cleanup-backing-file"]
        );
    }

    #[test]
    fn deep_rotation_uses_only_the_existing_fixed_trusted_program() {
        use crate::rootfs::plan::CleanupMode::{Deep, Standard};
        let normal = select(Some(90069), all, true);
        assert_eq!(for_mode(normal.clone(), Standard, true), normal);
        assert_eq!(for_mode(normal.clone(), Deep, false), normal);
        assert_eq!(
            for_mode(normal, Deep, true),
            [
                Delegate::Logrotate,
                Delegate::DeleteJournal,
                Delegate::RestartSyslogNg
            ]
        );
        assert!(for_mode(vec![], Deep, true).is_empty(), "no tools off DSM");
    }

    #[test]
    fn each_run_is_a_fixed_argv() {
        assert_eq!(
            Delegate::RotateLogs.argv(),
            ["/usr/syno/bin/synocleaner", "--rotate-log"]
        );
        assert_eq!(
            Delegate::Logrotate.argv(),
            ["/usr/bin/logrotate", "-f", "/etc/logrotate.conf"]
        );
        assert_eq!(
            Delegate::RestartSyslogNg.argv(),
            ["/usr/syno/bin/synosystemctl", "restart", "syslog-ng"]
        );
    }

    #[test]
    fn a_captured_run_returns_its_output_only_when_it_succeeds() {
        assert_eq!(
            run_capture("/bin/sh", &["-c", "echo /var/log/a; echo $HOME"], 1024),
            Some(b"/var/log/a\n\n".to_vec())
        );
        assert_eq!(
            run_capture("/bin/sh", &["-c", "yes /x | head -c 100000"], 10),
            Some(b"/x\n/x\n/x\n/".to_vec())
        );
        assert_eq!(
            run_capture("/bin/sh", &["-c", "echo x; exit 1"], 1024),
            None
        );
        assert_eq!(run_capture("/nonexistent/tool", &[], 1024), None);
    }

    #[test]
    fn explicit_cancellation_stops_the_tool_and_its_children() {
        let dir = crate::spill::testing::scratch("delegate-cancel");
        let marker = dir.join("child-lived");
        let script = format!("(sleep 1; touch {}) & sleep 30", marker.display());
        let mut child = Command::new("/bin/sh")
            .args(["-c", &script])
            .process_group(0)
            .spawn()
            .unwrap();
        assert!(!wait_child(&mut child, || true).unwrap().success());
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "the child outlived cancellation");
    }

    #[test]
    fn deep_runs_dsm_s_own_old_log_cleaner_first_where_the_build_ships_it() {
        use crate::rootfs::plan::CleanupMode::{Deep, Standard};
        let runs = select(Some(90069), all, true);
        assert_eq!(
            with_delete_log(runs.clone(), Deep, Some(90069), all),
            [
                Delegate::DeleteLog,
                Delegate::RotateLogs,
                Delegate::DeleteJournal,
                Delegate::RestartSyslogNg
            ]
        );
        assert_eq!(
            with_delete_log(runs.clone(), Standard, Some(90069), all),
            runs
        );
        assert_eq!(
            with_delete_log(vec![], Deep, Some(SYNOCLEANER_MIN_BUILD - 1), all),
            []
        );
        assert_eq!(with_delete_log(vec![], Deep, None, all), []);
        assert_eq!(
            with_delete_log(vec![], Deep, Some(90069), |p| p != SYNOCLEANER),
            []
        );
        assert_eq!(
            Delegate::DeleteLog.argv(),
            ["/usr/syno/bin/synocleaner", "--delete-log"]
        );
        assert_eq!(
            Delegate::DeleteLog.dry_run_argv(),
            Some(vec![
                "/usr/syno/bin/synocleaner",
                "--dry-run",
                "--delete-log"
            ])
        );
        assert_eq!(Delegate::RotateLogs.dry_run_argv(), None);
    }

    #[test]
    fn the_dry_run_estimate_counts_listed_root_files_not_already_moved_once() {
        let out = b"/var/log/a.xz\n/var/log/samba/log.wb-X\nNo files to delete.\nrelative/x\n\
/var/log/moved.xz\n/var/log/link\n/var/log/elsewhere\n/var/log/a.xz\n/var/log/gone\n";
        let moved: std::collections::BTreeSet<Vec<u8>> = [b"/var/log/moved.xz".to_vec()].into();
        let stat = |path: &[u8]| -> Option<ListedFile> {
            Some(match path {
                b"/var/log/a.xz" => ListedFile {
                    device: 1,
                    inode: 10,
                    regular: true,
                    allocated_bytes: 4096,
                },
                b"/var/log/samba/log.wb-X" => ListedFile {
                    device: 1,
                    inode: 11,
                    regular: true,
                    allocated_bytes: 8192,
                },
                b"/var/log/moved.xz" => ListedFile {
                    device: 1,
                    inode: 12,
                    regular: true,
                    allocated_bytes: 1 << 20,
                },
                b"/var/log/link" => ListedFile {
                    device: 1,
                    inode: 13,
                    regular: false,
                    allocated_bytes: 0,
                },
                b"/var/log/elsewhere" => ListedFile {
                    device: 2,
                    inode: 14,
                    regular: true,
                    allocated_bytes: 1 << 20,
                },
                _ => return None,
            })
        };
        assert_eq!(dry_run_bytes(out, &moved, 1, stat), 4096 + 8192);
        assert_eq!(dry_run_bytes(b"No files to delete.\n", &moved, 1, stat), 0);
    }

    #[test]
    fn synocleaner_is_preferred_where_the_build_ships_it() {
        assert_eq!(
            select(Some(90069), all, false),
            [Delegate::RotateLogs, Delegate::DeleteJournal]
        );
        assert_eq!(
            select(Some(SYNOCLEANER_MIN_BUILD - 1), all, false),
            [Delegate::Logrotate]
        );
        assert_eq!(
            select(Some(90069), |p| p != SYNOCLEANER, false),
            [Delegate::Logrotate]
        );
        assert_eq!(select(Some(90069), |_| false, false), []);
        assert_eq!(select(None, all, true), []);
    }

    #[test]
    fn syslog_ng_is_restarted_last_and_only_while_it_holds_deleted_files() {
        assert_eq!(
            select(Some(90069), all, true),
            [
                Delegate::RotateLogs,
                Delegate::DeleteJournal,
                Delegate::RestartSyslogNg
            ]
        );
        assert_eq!(
            select(Some(90069), |p| p != SYNOSYSTEMCTL, true),
            [Delegate::RotateLogs, Delegate::DeleteJournal]
        );
    }
}
