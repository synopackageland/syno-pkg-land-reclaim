//! Answers prewarmed for the package UI: the JSON
//! document of one `webapi` argv, kept under `<state>/ui/cache/` and
//! served while the reports of its filesystem are unchanged.

use crate::report::hex_bytes;
use crate::state::{self, StateError};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Below the state directory.
pub const DIR: &str = "ui/cache";
pub const FORMAT: u32 = 7;

/// One cached answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub format: u32,
    /// The argv it answers, NUL-separated.
    #[serde(with = "hex_bytes")]
    pub key: Vec<u8>,
    pub fsid: String,
    #[serde(with = "hex_bytes")]
    pub mount: Vec<u8>,
    /// `reports_fingerprint` taken before the answer was computed.
    pub reports: String,
    /// Unix seconds until which the answer's reports are not stale.
    pub valid_until: u64,
    pub exit_code: i32,
    /// The command's stdout, byte for byte.
    pub document: String,
}

/// `--max-age` of the answer commands, in seconds: a cached answer is
/// served only while a fresh one would not call its reports stale.
pub const MAX_AGE_SECS: u64 = 24 * 3600;

/// Until when a document computed at `now` stays what a fresh run would
/// say about staleness: `None` if it has no report or one is stale.
pub fn valid_until(document: &[u8], now: u64) -> Option<u64> {
    let doc: serde_json::Value = serde_json::from_slice(document).ok()?;
    if doc["command"] == "duplicates-scope" {
        // Ready results with no analysis running or numbers recalculated:
        // stale once a report or a choice changes (`fingerprint`).
        let a = &doc["analysis"];
        let settled =
            a["state"] == "ready" && a["running"] == false && doc["recalculating"] == false;
        return settled.then_some(now + MAX_AGE_SECS);
    }
    let ages = doc["results"]
        .as_array()?
        .iter()
        .map(|r| r["report"]["age_seconds"].as_u64())
        .collect::<Option<Vec<u64>>>()?;
    let oldest = ages.into_iter().max()?;
    (oldest <= MAX_AGE_SECS).then(|| now + (MAX_AGE_SECS - oldest) + 1)
}

/// Runs `argv` with `exe` and caches its document for the filesystem
/// `fsid` mounted at `mount` when it answered (exit 0 or 3). Returns
/// whether it was cached.
pub fn prewarm(
    state_dir: &Path,
    uid: u32,
    exe: &Path,
    fsid: &str,
    mount: &[u8],
    argv: &[OsString],
    now: u64,
) -> Result<bool, StateError> {
    Ok(compute(state_dir, uid, exe, fsid, mount, argv, now)?.is_some_and(|(_, _, cached)| cached))
}

/// Like `prewarm`, and returns what `argv` printed and its exit code
/// whether or not it was cached; `None` if it printed no text.
pub fn compute(
    state_dir: &Path,
    uid: u32,
    exe: &Path,
    fsid: &str,
    mount: &[u8],
    argv: &[OsString],
    now: u64,
) -> Result<Option<(i32, String, bool)>, StateError> {
    let reports = fingerprint(state_dir, fsid, argv).map_err(|source| StateError::Io {
        path: state_dir.join("reports").join(fsid),
        source,
    })?;
    let output = command(exe, argv)
        .output()
        .map_err(|source| StateError::Io {
            path: exe.to_path_buf(),
            source,
        })?;
    let (Some(exit_code), Ok(document)) = (output.status.code(), String::from_utf8(output.stdout))
    else {
        return Ok(None);
    };
    let valid = valid_until(document.as_bytes(), now);
    let Some(valid_until) = valid.filter(|_| matches!(exit_code, 0 | 3)) else {
        return Ok(Some((exit_code, document, false)));
    };
    let entry = Entry {
        format: FORMAT,
        key: key(argv),
        fsid: fsid.to_string(),
        mount: mount.to_vec(),
        reports,
        valid_until,
        exit_code,
        document,
    };
    save(state_dir, uid, &entry)?;
    Ok(Some((entry.exit_code, entry.document, true)))
}

/// The command that runs `argv` with `exe` as the plain CLI. It runs
/// inside a root program the executor started, so the start mode is
/// removed: inherited, it makes the child refuse its arguments as a start
/// the executor never makes, and nothing is cached.
fn command(exe: &Path, argv: &[OsString]) -> Command {
    let mut command = Command::new(exe);
    command
        .args(&argv[1..])
        .env_remove(crate::root_entry::MODE_ENV)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// The cache key of an argv.
pub fn key(argv: &[OsString]) -> Vec<u8> {
    argv.iter()
        .map(|a| a.as_bytes())
        .collect::<Vec<_>>()
        .join(&0u8)
}

fn path_of(state_dir: &Path, key: &[u8]) -> PathBuf {
    state_dir.join(DIR).join(file_name(key))
}

fn file_name(key: &[u8]) -> String {
    format!("{:016x}.json", crate::guard::fnv1a64(key))
}

/// What the report store holds for a filesystem: the name, size,
/// modification time and inode of every visible report file in its
/// directory. Duplicate results count only for duplicates answers
/// ([`fingerprint`]): a keep choice rewrites them.
pub fn reports_fingerprint(state_dir: &Path, fsid: &str) -> io::Result<String> {
    files_fingerprint(state_dir, fsid, false)
}

fn files_fingerprint(state_dir: &Path, fsid: &str, duplicates: bool) -> io::Result<String> {
    let dir = state_dir.join("reports").join(fsid);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let extension = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_owned);
        if name.as_bytes().starts_with(b".")
            || matches!(
                extension.as_deref(),
                Some("srsummary" | "srindex" | "srcand" | "srstamps")
            )
            || (!duplicates && extension.as_deref() == Some("srdup"))
        {
            continue;
        }
        let meta = entry.metadata()?;
        files.push(format!(
            "{}:{}:{}.{}:{}",
            hex_bytes::encode(name.as_bytes()),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ino()
        ));
    }
    files.sort();
    Ok(files.join(","))
}

/// What a cached answer of `argv` depends on: the reports of `fsid`,
/// and for a duplicates answer also the volume's keep choices and
/// preferred shares (`duplicates/<fsid>.json`), which each choice, and
/// each analysis asked for, replaces.
pub fn fingerprint(state_dir: &Path, fsid: &str, argv: &[OsString]) -> io::Result<String> {
    let duplicates = argv.get(1).is_some_and(|a| a == "duplicates");
    let mut print = files_fingerprint(state_dir, fsid, duplicates)?;
    if duplicates {
        let settings = state_dir.join("duplicates").join(format!("{fsid}.json"));
        match std::fs::symlink_metadata(settings) {
            Ok(meta) => print.push_str(&format!(
                ";settings:{}:{}.{}:{}",
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ino()
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => print.push_str(";settings:none"),
            Err(e) => return Err(e),
        }
    }
    Ok(print)
}

/// Saves an entry (0600, atomic).
pub fn save(state_dir: &Path, uid: u32, entry: &Entry) -> Result<(), StateError> {
    let dir = state_dir.join(DIR);
    state::private_dir(&state_dir.join("ui"), uid)?;
    state::private_dir(&dir, uid)?;
    state::replace_file(&dir, &file_name(&entry.key), |out| {
        serde_json::to_writer(&mut *out, entry).map_err(io::Error::other)
    })
}

/// The exit code and document cached for `argv`, when its filesystem is
/// still mounted where it was (`fsid_of` maps a mount to its fsid), its
/// reports are unchanged and not stale at `now`.
pub fn lookup(
    state_dir: &Path,
    argv: &[OsString],
    fsid_of: impl Fn(&[u8]) -> Option<String>,
    now: u64,
) -> Option<(i32, String)> {
    let key = key(argv);
    let bytes = std::fs::read(path_of(state_dir, &key)).ok()?;
    let entry: Entry = serde_json::from_slice(&bytes).ok()?;
    let valid = entry.format == FORMAT
        && entry.key == key
        && now < entry.valid_until
        && fsid_of(&entry.mount).as_deref() == Some(entry.fsid.as_str())
        && fingerprint(state_dir, &entry.fsid, argv).ok()? == entry.reports;
    valid.then_some((entry.exit_code, entry.document))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A private directory removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "synoreclaim-ui-cache-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            state::private_dir(&path, uid()).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn argv() -> Vec<OsString> {
        ["synoreclaim", "top", "--kind=all", "--", "/volume1"]
            .map(OsString::from)
            .to_vec()
    }

    fn uid() -> u32 {
        crate::sys::effective_uid()
    }

    /// A state directory with one report of `fs1` and an entry for `argv`.
    fn cached() -> (Scratch, PathBuf) {
        let tmp = Scratch::new();
        let state = tmp.0.join("state");
        state::private_dir(&state, uid()).unwrap();
        let reports = state.join("reports/fs1");
        fs::create_dir_all(&reports).unwrap();
        fs::write(reports.join("a.srrep"), b"report").unwrap();
        let entry = Entry {
            format: FORMAT,
            key: key(&argv()),
            fsid: "fs1".into(),
            mount: b"/volume1".to_vec(),
            reports: reports_fingerprint(&state, "fs1").unwrap(),
            valid_until: 1000,
            exit_code: 3,
            document: "{\"command\":\"top\"}\n".into(),
        };
        save(&state, uid(), &entry).unwrap();
        (tmp, state)
    }

    fn mounted(mount: &[u8]) -> Option<String> {
        (mount == b"/volume1").then(|| "fs1".to_string())
    }

    #[test]
    fn the_max_age_is_the_answer_commands_default() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["synoreclaim", "overview", "/volume1"]).unwrap();
        let Command::Overview { answer, .. } = cli.command else {
            panic!("not overview");
        };
        assert_eq!(answer.max_age.as_secs(), MAX_AGE_SECS);
    }

    fn dup_argv() -> Vec<OsString> {
        [
            "synoreclaim",
            "duplicates",
            "scope",
            "--json",
            "--limit=1",
            "--",
            "/volume1",
        ]
        .map(OsString::from)
        .to_vec()
    }

    /// A duplicates answer turns stale when the volume's choices or its
    /// results change; other answers ignore both.
    #[test]
    fn a_keep_choice_or_new_results_invalidate_only_duplicates_answers() {
        let (_tmp, state) = cached();
        let reports = state.join("reports/fs1");
        fs::write(reports.join("a.srdup"), b"{}").unwrap();
        let mut entry: Entry =
            serde_json::from_slice(&fs::read(path_of(&state, &key(&argv()))).unwrap()).unwrap();
        entry.reports = fingerprint(&state, "fs1", &argv()).unwrap();
        save(&state, uid(), &entry).unwrap();
        entry.key = key(&dup_argv());
        entry.reports = fingerprint(&state, "fs1", &dup_argv()).unwrap();
        save(&state, uid(), &entry).unwrap();
        assert!(lookup(&state, &dup_argv(), mounted, 10).is_some());
        // A keep choice replaces the settings file.
        fs::create_dir_all(state.join("duplicates")).unwrap();
        fs::write(state.join("duplicates/fs1.json"), b"{}").unwrap();
        assert!(lookup(&state, &dup_argv(), mounted, 10).is_none());
        assert!(lookup(&state, &argv(), mounted, 10).is_some());
        entry.reports = fingerprint(&state, "fs1", &dup_argv()).unwrap();
        save(&state, uid(), &entry).unwrap();
        assert!(lookup(&state, &dup_argv(), mounted, 10).is_some());
        // Rewritten results: only the duplicates answer goes; the stamps
        // file counts for neither.
        fs::write(reports.join("a.srstamps"), b"x").unwrap();
        assert!(lookup(&state, &dup_argv(), mounted, 10).is_some());
        fs::remove_file(reports.join("a.srdup")).unwrap();
        fs::write(reports.join("a.srdup"), b"{\"x\":1}").unwrap();
        assert!(lookup(&state, &dup_argv(), mounted, 10).is_none());
        assert!(lookup(&state, &argv(), mounted, 10).is_some());
    }

    /// A duplicates answer is cached only for ready results with nothing
    /// running or recalculated.
    #[test]
    fn a_duplicates_answer_is_valid_only_when_settled() {
        let doc = |state: &str, running: bool, recalculating: bool| {
            serde_json::json!({
                "command": "duplicates-scope",
                "analysis": {"state": state, "running": running},
                "recalculating": recalculating,
            })
            .to_string()
        };
        assert_eq!(
            valid_until(doc("ready", false, false).as_bytes(), 10),
            Some(10 + MAX_AGE_SECS)
        );
        assert_eq!(valid_until(doc("ready", true, false).as_bytes(), 10), None);
        assert_eq!(valid_until(doc("ready", false, true).as_bytes(), 10), None);
        assert_eq!(valid_until(doc("stale", false, false).as_bytes(), 10), None);
        assert_eq!(valid_until(doc("none", false, false).as_bytes(), 10), None);
    }

    #[test]
    fn an_answer_is_valid_until_its_oldest_report_turns_stale() {
        let doc = br#"{"results":[{"report":{"age_seconds":100}},{"report":{"age_seconds":40}}]}"#;
        assert_eq!(valid_until(doc, 5000), Some(5000 + MAX_AGE_SECS - 100 + 1));
        let at_limit = format!(r#"{{"results":[{{"report":{{"age_seconds":{MAX_AGE_SECS}}}}}]}}"#);
        assert_eq!(valid_until(at_limit.as_bytes(), 7), Some(8));
        let stale = format!(
            r#"{{"results":[{{"report":{{"age_seconds":{}}}}}]}}"#,
            MAX_AGE_SECS + 1
        );
        assert_eq!(valid_until(stale.as_bytes(), 7), None);
        assert_eq!(valid_until(br#"{"results":[]}"#, 7), None);
        assert_eq!(valid_until(br#"{"results":[{"report":{}}]}"#, 7), None);
        assert_eq!(valid_until(b"not json", 7), None);
    }

    /// An executable that prints `document` and exits with `code`.
    fn fake_exe(dir: &Path, document: &str, code: i32) -> PathBuf {
        let exe = dir.join(format!("fake-{code}"));
        install_script(
            &exe,
            &format!("#!/bin/sh\ncat <<'EOF'\n{document}\nEOF\nexit {code}\n"),
        );
        exe
    }

    /// Writes `script` to `exe` through `cp`, so no process this test
    /// binary forks meanwhile inherits a write descriptor to it, which
    /// would make running it fail with ETXTBSY.
    fn install_script(exe: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let source = exe.with_extension("source");
        fs::write(&source, script).unwrap();
        let copied = Command::new("cp").arg(&source).arg(exe).status().unwrap();
        assert!(copied.success());
        fs::set_permissions(exe, fs::Permissions::from_mode(0o700)).unwrap();
    }

    const DOC: &str = r#"{"results":[{"report":{"age_seconds":10}}]}"#;

    #[test]
    fn the_answer_runs_as_the_plain_cli_not_a_root_start() {
        let command = command(Path::new("/x/cli"), &argv());
        let mode = std::ffi::OsStr::new(crate::root_entry::MODE_ENV);
        assert!(command.get_envs().any(|(k, v)| k == mode && v.is_none()));
        assert_eq!(command.get_args().count(), argv().len() - 1);
    }

    #[test]
    fn a_prewarmed_answer_is_served_with_its_exit_code() {
        let (tmp, state) = cached();
        let exe = fake_exe(&tmp.0, DOC, 3);
        let mut other = argv();
        other[4] = "/volume2".into();
        assert!(prewarm(&state, uid(), &exe, "fs1", b"/volume1", &other, 50).unwrap());
        assert_eq!(
            lookup(&state, &other, mounted, 50 + MAX_AGE_SECS - 10),
            Some((3, format!("{DOC}\n")))
        );
        assert_eq!(
            lookup(&state, &other, mounted, 51 + MAX_AGE_SECS - 10),
            None
        );
    }

    #[test]
    fn a_refused_or_stale_answer_is_not_prewarmed() {
        let (tmp, state) = cached();
        let mut other = argv();
        other[4] = "/volume2".into();
        let refused = fake_exe(&tmp.0, DOC, 1);
        assert!(!prewarm(&state, uid(), &refused, "fs1", b"/volume1", &other, 50).unwrap());
        let stale = fake_exe(&tmp.0, r#"{"results":[]}"#, 0);
        assert!(!prewarm(&state, uid(), &stale, "fs1", b"/volume1", &other, 50).unwrap());
        assert_eq!(lookup(&state, &other, mounted, 60), None);
    }

    #[test]
    fn an_answer_to_reports_published_while_it_ran_is_not_served() {
        let (tmp, state) = cached();
        let exe = tmp.0.join("publishing");
        install_script(
            &exe,
            &format!(
                "#!/bin/sh\necho new > {}/reports/fs1/b.srrep\necho '{DOC}'\n",
                state.display()
            ),
        );
        let mut other = argv();
        other[4] = "/volume2".into();
        assert!(prewarm(&state, uid(), &exe, "fs1", b"/volume1", &other, 50).unwrap());
        assert_eq!(lookup(&state, &other, mounted, 60), None);
    }

    #[test]
    fn an_answer_is_served_while_its_reports_are_unchanged() {
        let (_tmp, state) = cached();
        assert_eq!(
            lookup(&state, &argv(), mounted, 999),
            Some((3, "{\"command\":\"top\"}\n".to_string()))
        );
    }

    #[test]
    fn entries_are_private_files_below_the_cache_dir() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, state) = cached();
        let files: Vec<_> = fs::read_dir(state.join(DIR)).unwrap().collect();
        assert_eq!(files.len(), 1);
        let meta = files[0].as_ref().unwrap().metadata().unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_new_or_removed_report_invalidates_the_answer() {
        let (_tmp, state) = cached();
        fs::write(state.join("reports/fs1/b.srrep"), b"newer").unwrap();
        assert_eq!(lookup(&state, &argv(), mounted, 999), None);
        fs::remove_file(state.join("reports/fs1/b.srrep")).unwrap();
        fs::remove_file(state.join("reports/fs1/a.srrep")).unwrap();
        assert_eq!(lookup(&state, &argv(), mounted, 999), None);
    }

    #[test]
    fn old_ui_answers_with_metadata_double_counts_are_not_served() {
        let (_tmp, state) = cached();
        let file = path_of(&state, &key(&argv()));
        let mut entry: Entry = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        entry.format = 1;
        save(&state, uid(), &entry).unwrap();
        assert_eq!(lookup(&state, &argv(), mounted, 999), None);
    }

    #[test]
    fn old_ui_answers_without_nested_row_inclusion_are_not_served() {
        let (_tmp, state) = cached();
        let file = path_of(&state, &key(&argv()));
        let mut entry: Entry = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        entry.format = 2;
        save(&state, uid(), &entry).unwrap();
        assert_eq!(lookup(&state, &argv(), mounted, 999), None);
    }

    #[test]
    fn a_rewritten_report_invalidates_the_answer() {
        let (_tmp, state) = cached();
        fs::write(state.join("reports/fs1/a.srrep"), b"longer report").unwrap();
        assert_eq!(lookup(&state, &argv(), mounted, 999), None);
    }

    #[test]
    fn hidden_temporary_files_do_not_count_as_reports() {
        let (_tmp, state) = cached();
        fs::write(state.join("reports/fs1/.a.srrep.j.tmp"), b"partial").unwrap();
        assert!(lookup(&state, &argv(), mounted, 999).is_some());
    }

    #[test]
    fn a_stale_answer_is_not_served() {
        let (_tmp, state) = cached();
        assert_eq!(lookup(&state, &argv(), mounted, 1000), None);
    }

    #[test]
    fn another_filesystem_at_the_mount_is_not_answered() {
        let (_tmp, state) = cached();
        assert_eq!(lookup(&state, &argv(), |_| Some("fs2".into()), 999), None);
        assert_eq!(lookup(&state, &argv(), |_| None, 999), None);
    }

    #[test]
    fn other_argv_or_no_cache_is_a_miss() {
        let (_tmp, state) = cached();
        let mut other = argv();
        other.push("--limit=5".into());
        assert_eq!(lookup(&state, &other, mounted, 999), None);
        let empty = Scratch::new();
        assert_eq!(lookup(&empty.0, &argv(), mounted, 999), None);
    }

    #[test]
    fn an_entry_under_another_key_is_not_served() {
        let (_tmp, state) = cached();
        let name = file_name(&key(&argv()));
        let path = state.join(DIR).join(&name);
        let mut entry: Entry = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        entry.key = b"synoreclaim\0overview".to_vec();
        fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        assert_eq!(lookup(&state, &argv(), mounted, 999), None);
    }
}
