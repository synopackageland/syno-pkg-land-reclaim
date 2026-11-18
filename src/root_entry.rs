//! The binary as a root program of the SDK executor (`spec/protocol/v1`,
//! Root programs). The executor runs it as root, without arguments for
//! `root.run` and `root.start` and with one argument for the package stop
//! and uninstall steps, and says which in `SYNOPKGLAND_ROOT_MODE`.

use serde::Deserialize;
use std::ffi::OsString;
use std::path::PathBuf;

pub mod launch;
#[cfg(target_os = "linux")]
pub mod lifecycle;
#[cfg(target_os = "linux")]
pub mod serve;

/// The verified DSM user a run or job is for.
pub const REQUESTED_BY_ENV: &str = "SYNOPKGLAND_REQUESTED_BY";
/// The root-only state directory (policy `root_state`).
pub const ROOT_STATE_ENV: &str = "SYNOPKGLAND_ROOT_STATE";
/// The job's cancel pipe, executor to job.
pub const CONTROL_FD_ENV: &str = "SYNOPKGLAND_CONTROL_FD";
/// The job's progress pipe, job to executor.
pub const PROGRESS_FD_ENV: &str = "SYNOPKGLAND_PROGRESS_FD";

/// The methods that queue a job and answer its detach document; the App
/// then starts the job with `root.start`.
pub const QUEUEING_METHODS: &[&str] = &[
    "rootfs.report",
    "duplicates.analyze",
    "scan.start",
    "cleanup.plan",
    "rootfs.plan",
    "cleanup.apply",
    "rootfs.apply",
    "job.resume",
];

/// A `root.run` call: the WebAPI method and its params, as JSON text so the
/// executor's closed schema need not know each method's params.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    pub method: String,
    pub params: String,
}

/// A `root.start` call: the queued job to run.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobRequest {
    pub job_id: String,
}

/// Decodes the request on stdin.
pub fn request<T: serde::de::DeserializeOwned>(stdin: &[u8]) -> Result<T, Refused> {
    serde_json::from_slice(stdin).map_err(|e| Refused(format!("request: {e}")))
}

/// What the executor passes a run or job besides its params.
#[derive(Debug, PartialEq, Eq)]
pub struct Caller {
    pub requested_by: String,
    pub state_dir: PathBuf,
}

/// The caller from the executor's environment (`var` reads one variable).
pub fn caller(var: impl Fn(&str) -> Option<OsString>) -> Result<Caller, Refused> {
    let requested_by = var(REQUESTED_BY_ENV)
        .and_then(|user| user.into_string().ok())
        .filter(|user| !user.is_empty())
        .ok_or_else(|| Refused(format!("{REQUESTED_BY_ENV} is not a user")))?;
    Ok(Caller {
        requested_by,
        state_dir: root_state(var)?,
    })
}

/// The root state directory from the executor's environment.
pub fn root_state(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, Refused> {
    var(ROOT_STATE_ENV)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .ok_or_else(|| Refused(format!("{ROOT_STATE_ENV} is not an absolute path")))
}

/// Whether a cancel with `reason` stops job work for good (`true`) or
/// pauses a scan for a later resume. An apply cannot pause.
pub fn cancels(reason: &str, apply: bool) -> bool {
    apply || reason == "user"
}

/// Serves one start the executor made, or refuses one it does not make;
/// the process exit code.
pub fn start(invocation: Result<Invocation, Refused>) -> i32 {
    match invocation {
        #[cfg(target_os = "linux")]
        Ok(invocation) => serve::start(invocation),
        #[cfg(not(target_os = "linux"))]
        Ok(_) => {
            eprintln!("synoreclaim: error: synoreclaim requires Linux");
            1
        }
        Err(Refused(message)) => {
            eprintln!("synoreclaim: error: {message}");
            2
        }
    }
}

/// How the executor started the program.
pub const MODE_ENV: &str = "SYNOPKGLAND_ROOT_MODE";

/// One root program start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    /// `root.run`: `{"method", "params"}` on stdin.
    Run,
    /// `root.start`: `{"job_id"}` of a queued job on stdin.
    Job,
    /// The package stop step; `upgrade` for `prepare-upgrade`.
    Stop { upgrade: bool },
    /// The uninstall purge step.
    Purge { delete_state: bool },
}

/// Why a start is not one the executor makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// The root program `mode` and `args` (without the program name) start,
/// or `None` when the executor did not start this process.
pub fn invocation(
    mode: Option<OsString>,
    args: &[OsString],
) -> Option<Result<Invocation, Refused>> {
    let mode = mode?;
    let arg = match args {
        [] => None,
        [one] => one.to_str(),
        _ => {
            return Some(Err(Refused(
                "a root program takes at most one argument".into(),
            )));
        }
    };
    Some(match (mode.to_str(), arg, args.is_empty()) {
        (Some("run"), None, true) => Ok(Invocation::Run),
        (Some("job"), None, true) => Ok(Invocation::Job),
        (Some("stop"), Some("stop"), _) => Ok(Invocation::Stop { upgrade: false }),
        (Some("stop"), Some("prepare-upgrade"), _) => Ok(Invocation::Stop { upgrade: true }),
        (Some("purge"), Some("keep-state"), _) => Ok(Invocation::Purge {
            delete_state: false,
        }),
        (Some("purge"), Some("delete-state"), _) => Ok(Invocation::Purge { delete_state: true }),
        _ => Err(Refused(format!(
            "{MODE_ENV}={} with {args:?} is not a start the executor makes",
            mode.to_string_lossy()
        ))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(mode: Option<&str>, args: &[&str]) -> Option<Result<Invocation, Refused>> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        invocation(mode.map(OsString::from), &args)
    }

    #[test]
    fn without_the_mode_it_is_the_cli() {
        assert_eq!(start(None, &["scan", "/volume1"]), None);
    }

    #[test]
    fn run_and_job_take_no_argument() {
        assert_eq!(start(Some("run"), &[]), Some(Ok(Invocation::Run)));
        assert_eq!(start(Some("job"), &[]), Some(Ok(Invocation::Job)));
        assert!(matches!(start(Some("run"), &["scan"]), Some(Err(_))));
        assert!(matches!(start(Some("job"), &["x"]), Some(Err(_))));
    }

    #[test]
    fn the_lifecycle_steps_take_their_one_argument() {
        assert_eq!(
            start(Some("stop"), &["stop"]),
            Some(Ok(Invocation::Stop { upgrade: false }))
        );
        assert_eq!(
            start(Some("stop"), &["prepare-upgrade"]),
            Some(Ok(Invocation::Stop { upgrade: true }))
        );
        assert_eq!(
            start(Some("purge"), &["keep-state"]),
            Some(Ok(Invocation::Purge {
                delete_state: false
            }))
        );
        assert_eq!(
            start(Some("purge"), &["delete-state"]),
            Some(Ok(Invocation::Purge { delete_state: true }))
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for (mode, args) in [
            ("stop", &[][..]),
            ("stop", &["delete-state"][..]),
            ("purge", &["stop"][..]),
            ("purge", &["keep-state", "x"][..]),
            ("shell", &[][..]),
            ("", &[][..]),
        ] {
            assert!(
                matches!(start(Some(mode), args), Some(Err(_))),
                "{mode} {args:?}"
            );
        }
    }

    #[test]
    fn a_request_is_one_strict_object() {
        assert_eq!(
            request::<RunRequest>(br#"{"method":"jobs.list","params":"{}"}"#),
            Ok(RunRequest {
                method: "jobs.list".into(),
                params: "{}".into()
            })
        );
        for bad in [
            &br#"{"method":"jobs.list","params":{}}"#[..],
            br#"{"method":"jobs.list","params":"{}","user":"x"}"#,
            br#"{"method":"jobs.list"}"#,
            br#"{"method":"jobs.list","params":"{}"} {}"#,
            b"",
        ] {
            assert!(
                request::<RunRequest>(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(
            request::<JobRequest>(br#"{"job_id":"j-1"}"#),
            Ok(JobRequest {
                job_id: "j-1".into()
            })
        );
    }

    #[test]
    fn the_caller_is_the_verified_user_with_the_root_state() {
        fn env<'a>(
            user: Option<&'a str>,
            state: Option<&'a str>,
        ) -> impl Fn(&str) -> Option<OsString> + 'a {
            move |name| match name {
                REQUESTED_BY_ENV => user.map(OsString::from),
                ROOT_STATE_ENV => state.map(OsString::from),
                _ => None,
            }
        }
        assert_eq!(
            caller(env(
                Some("admin"),
                Some("/volume1/@synopkgland/SynoReclaim")
            )),
            Ok(Caller {
                requested_by: "admin".into(),
                state_dir: "/volume1/@synopkgland/SynoReclaim".into()
            })
        );
        for (user, state) in [
            (None, Some("/s")),
            (Some(""), Some("/s")),
            (Some("admin"), None),
            (Some("admin"), Some("relative")),
            (Some("admin"), Some("")),
        ] {
            assert!(caller(env(user, state)).is_err(), "{user:?} {state:?}");
        }
    }

    #[test]
    fn a_user_cancel_ends_the_work_and_a_package_stop_pauses_a_scan() {
        assert!(cancels("user", false));
        assert!(cancels("user", true));
        for reason in ["stop", "upgrade", "purge", "executor-stop", "unknown"] {
            assert!(!cancels(reason, false), "{reason}");
            assert!(cancels(reason, true), "{reason}");
        }
    }
}
