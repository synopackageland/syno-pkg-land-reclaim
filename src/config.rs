//! The cleanup opt-in configuration (SPEC 5.7, docs/ui GAP-12), kept in the
//! package's own folder and owned by the package user, so the unprivileged
//! WebAPI can read it. Root reads it as untrusted input (`read_trusted_config`).
//! Written atomically for the package's lifecycle and settings page.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::cli::ConfigAction;
use crate::document::Envelope;
use crate::guard::{
    CleanupConfig, CleanupSwitches, Refusal, read_trusted_config, switches_owned_by,
};

type Failure = Box<dyn std::error::Error>;

/// The package's user, which owns the configuration.
pub const PACKAGE_USER: &str = "SynoReclaim";
/// DSM's configuration folder of the package (`@appconf`): kept on upgrade,
/// removed on uninstall, already owned by the package user.
const PACKAGE_ETC: &str = "/var/packages/SynoReclaim/etc";
/// The configuration path as the package's root-owned alias names it, for
/// messages before `location` can resolve the real folder.
pub const CONFIG_ALIAS_PATH: &str = "/var/packages/SynoReclaim/etc/config.json";

/// Where the configuration is and who must own it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub owner: u32,
}

/// The uid of the passwd entry called `name`.
pub fn uid_in_passwd(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(name))
            .then(|| fields.nth(1)?.parse().ok())
            .flatten()
    })
}

/// The configuration's location: the package user from `/etc/passwd`, and
/// the real folder DSM's root-owned `etc` alias of the package points to.
pub fn location() -> Result<Location, Failure> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    let owner = uid_in_passwd(&passwd, PACKAGE_USER)
        .ok_or_else(|| format!("no package user {PACKAGE_USER}"))?;
    let alias = fs::symlink_metadata(PACKAGE_ETC)?;
    if alias.uid() != 0 {
        return Err(format!("{PACKAGE_ETC} is not root's").into());
    }
    Ok(Location {
        path: fs::canonicalize(PACKAGE_ETC)?.join("config.json"),
        owner,
    })
}

/// Whether `init` wrote the configuration, and what it now says.
#[derive(Debug)]
pub struct Initialized {
    pub created: bool,
    pub config: CleanupConfig,
}

/// Writes the default configuration (cleanup enabled, mutations allowed)
/// at `path` unless one is already there; `owner` must own it.
pub fn init(path: &Path, owner: u32) -> Result<Initialized, Failure> {
    if fs::symlink_metadata(path).is_ok() {
        return Ok(Initialized {
            created: false,
            config: read_existing(path, owner)?,
        });
    }
    let config = CleanupConfig {
        cleanup_enabled: true,
        ..CleanupConfig::default()
    };
    write(path, owner, &config)?;
    Ok(Initialized {
        created: true,
        config,
    })
}

/// What the package's install and upgrade do: write the default unless a
/// configuration is already there.
pub fn install(location: &Location) -> Result<Initialized, Failure> {
    init(&location.path, location.owner)
}

/// The cleanup switches a root `session.get` reports. The package has no
/// install step that writes the default any more, so the first session
/// writes it (cleanup allowed) when there is none; one already there,
/// cleanup turned off included, is left as is, and an untrusted one still
/// reads as both switches off.
pub fn session_switches(location: &Location) -> CleanupSwitches {
    let _ = install(location);
    switches_owned_by(&location.path, location.owner)
}

/// The `config` JSON document (SPEC 5.7).
#[derive(Debug, Serialize)]
pub struct ConfigDocument {
    #[serde(flatten)]
    pub envelope: Envelope,
    /// `config init` only: whether it wrote the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<bool>,
    /// Whether cleanup plan, apply and delete may mutate data now.
    pub enables_cleanup: bool,
    pub config: CleanupConfig,
}

/// Runs a `config` action against the configuration at `path`.
pub fn run(
    action: &ConfigAction,
    path: &Path,
    owner: u32,
    now: SystemTime,
) -> Result<ConfigDocument, Failure> {
    let (created, config) = match *action {
        ConfigAction::Init { .. } => {
            let initialized = init(path, owner)?;
            (Some(initialized.created), initialized.config)
        }
        ConfigAction::Set {
            mutations_disabled, ..
        } => (
            None,
            set_mutations_disabled(path, owner, mutations_disabled)?,
        ),
        ConfigAction::Show { .. } => (None, read_existing(path, owner)?),
    };
    Ok(ConfigDocument {
        envelope: Envelope::new(command_name(action), now),
        created,
        enables_cleanup: config.cleanup_enabled && !config.mutations_disabled,
        config,
    })
}

/// The envelope `command` of a `config` action.
pub fn command_name(action: &ConfigAction) -> &'static str {
    match action {
        ConfigAction::Init { .. } => "config-init",
        ConfigAction::Set { .. } => "config-set",
        ConfigAction::Show { .. } => "config-show",
    }
}

/// The human `config` answer.
pub fn render(doc: &ConfigDocument) -> String {
    let mut text = match doc.created {
        Some(true) => "configuration written\n".to_string(),
        Some(false) => "configuration already present; left as is\n".to_string(),
        None => String::new(),
    };
    let yes_no = |value| if value { "yes" } else { "no" };
    text.push_str(&format!(
        "cleanup enabled {}\nmutations disabled {}\ncleanup allowed now {}\n",
        yes_no(doc.config.cleanup_enabled),
        yes_no(doc.config.mutations_disabled),
        yes_no(doc.enables_cleanup),
    ));
    match &doc.config.allowed_roots {
        Some(roots) => {
            text.push_str("allowed roots\n");
            for root in roots {
                text.push_str(&format!(
                    "  {}\n",
                    crate::output::escape_human(root.as_bytes())
                ));
            }
        }
        None => text.push_str("allowed roots any\n"),
    }
    text
}

/// Turns every cleanup mutation off or back on in the configuration
/// already at `path`.
pub fn set_mutations_disabled(
    path: &Path,
    owner: u32,
    disabled: bool,
) -> Result<CleanupConfig, Failure> {
    let config = CleanupConfig {
        mutations_disabled: disabled,
        ..read_existing(path, owner)?
    };
    write(path, owner, &config)?;
    Ok(config)
}

/// The configuration at `path`, or `None` only when there is none: one
/// that exists but is untrusted or unreadable refuses, so a kill switch
/// in it can never be missed.
pub fn read_optional(path: &Path, owner: u32) -> Result<Option<CleanupConfig>, Refusal> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => read_existing(path, owner).map(Some),
    }
}

/// The configuration already at `path`, refused unless it is a valid
/// private file in a directory only `owner` can write.
fn read_existing(path: &Path, owner: u32) -> Result<CleanupConfig, Refusal> {
    read_trusted_config(path, owner).map_err(|_| {
        Refusal::new(
            "config_untrusted",
            format!(
                "{}: not a valid configuration only its owner can change; left as is",
                path.display()
            ),
        )
    })
}

/// Replaces the configuration: a synced 0600 temporary file, created
/// exclusively and owned by `owner` before it holds anything, renamed into
/// the folder, then the folder synced. The folder belongs to `owner`, who may
/// plant anything in it, so nothing is followed.
fn write(path: &Path, owner: u32, config: &CleanupConfig) -> Result<(), Failure> {
    let dir = path.parent().ok_or("configuration path has no directory")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("configuration file name is not UTF-8")?;
    DirBuilder::new().recursive(true).mode(0o755).create(dir)?;
    let tmp = dir.join(format!(".{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    std::os::unix::fs::fchown(&file, Some(owner), None)?;
    serde_json::to_writer(&mut file, config)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Location, init, read_optional, render, run, session_switches, set_mutations_disabled,
        uid_in_passwd,
    };
    use crate::cli::ConfigAction;
    use crate::guard::read_trusted_config;

    fn scratch_directory(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "synoreclaim-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn only_a_missing_config_reads_as_none_anything_untrusted_refuses() {
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::sys::effective_uid();
        let dir = scratch_directory("config-optional");
        let config = dir.join("synoreclaim/config.json");
        assert!(read_optional(&config, uid).unwrap().is_none());

        init(&config, uid).unwrap();
        assert!(read_optional(&config, uid).unwrap().is_some());

        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            read_optional(&config, uid).unwrap_err().code,
            "config_untrusted"
        );
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&config, b"not json").unwrap();
        assert_eq!(
            read_optional(&config, uid).unwrap_err().code,
            "config_untrusted"
        );
    }

    #[test]
    fn init_writes_a_trusted_config_that_enables_cleanup() {
        let uid = crate::sys::effective_uid();
        let config = scratch_directory("config-init").join("synoreclaim/config.json");

        let initialized = init(&config, uid).unwrap();

        assert!(initialized.created);
        let read = read_trusted_config(&config, uid).unwrap();
        assert!(read.cleanup_enabled && !read.mutations_disabled);
        assert!(read.allowed_roots.is_none());
    }

    #[test]
    fn a_session_writes_the_default_when_there_is_none_so_cleanup_is_allowed() {
        let location = Location {
            path: scratch_directory("config-session-new").join("config.json"),
            owner: crate::sys::effective_uid(),
        };

        let switches = session_switches(&location);

        assert!(switches.cleanup_enabled && !switches.mutations_disabled);
        let read = read_trusted_config(&location.path, location.owner).unwrap();
        assert!(read.cleanup_enabled && !read.mutations_disabled);
    }

    #[test]
    fn a_session_keeps_cleanup_turned_off_and_leaves_an_untrusted_config() {
        use std::os::unix::fs::PermissionsExt;
        let owner = crate::sys::effective_uid();
        let off = Location {
            path: scratch_directory("config-session-off").join("config.json"),
            owner,
        };
        let kept = br#"{"cleanup_enabled":true,"mutations_disabled":true}"#;
        std::fs::write(&off.path, kept).unwrap();
        std::fs::set_permissions(&off.path, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(session_switches(&off).mutations_disabled);
        assert_eq!(std::fs::read(&off.path).unwrap(), kept);

        let untrusted = Location {
            path: scratch_directory("config-session-untrusted").join("config.json"),
            owner,
        };
        std::fs::write(&untrusted.path, b"{}").unwrap();
        std::fs::set_permissions(&untrusted.path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let switches = session_switches(&untrusted);

        assert!(!switches.cleanup_enabled);
        assert_eq!(std::fs::read(&untrusted.path).unwrap(), b"{}");
    }

    #[test]
    fn init_keeps_a_trusted_config_already_there() {
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::sys::effective_uid();
        let config = scratch_directory("config-keep").join("config.json");
        let kept =
            br#"{"cleanup_enabled":true,"mutations_disabled":true,"allowed_roots":["/volume1/s"]}"#;
        std::fs::write(&config, kept).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();

        let initialized = init(&config, uid).unwrap();

        assert!(!initialized.created);
        assert!(initialized.config.mutations_disabled);
        assert_eq!(std::fs::read(&config).unwrap(), kept);
    }

    #[test]
    fn init_refuses_a_config_it_cannot_trust_and_leaves_it() {
        use crate::guard::{failure_code, failure_exit_code};
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::sys::effective_uid();
        let config = scratch_directory("config-untrusted").join("config.json");
        std::fs::write(&config, b"{}").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = init(&config, uid).unwrap_err();

        assert_eq!(failure_code(error.as_ref()), "config_untrusted");
        assert_eq!(failure_exit_code(error.as_ref()), 4);
        assert_eq!(std::fs::read(&config).unwrap(), b"{}");
    }

    #[test]
    fn set_switches_mutations_and_keeps_the_rest_of_the_config() {
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::sys::effective_uid();
        let config = scratch_directory("config-set").join("config.json");
        std::fs::write(
            &config,
            br#"{"cleanup_enabled":true,"allowed_roots":["/volume1/s"]}"#,
        )
        .unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();

        let set = set_mutations_disabled(&config, uid, true).unwrap();

        assert!(set.mutations_disabled);
        let read = read_trusted_config(&config, uid).unwrap();
        assert!(read.cleanup_enabled && read.mutations_disabled);
        assert_eq!(read.allowed_roots, Some(vec!["/volume1/s".to_string()]));
        let mode = std::fs::metadata(&config).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn each_config_command_answers_with_the_envelope_and_the_config() {
        let uid = crate::sys::effective_uid();
        let config = scratch_directory("config-run").join("config.json");
        let json = |action: ConfigAction| {
            serde_json::to_value(run(&action, &config, uid, std::time::UNIX_EPOCH).unwrap())
                .unwrap()
        };

        let init = json(ConfigAction::Init { json: true });
        assert_eq!(init["command"], "config-init");
        assert_eq!(init["schema_version"], 1);
        assert_eq!(init["completion"], "complete");
        assert_eq!(init["created"], true);
        assert_eq!(init["enables_cleanup"], true);
        assert_eq!(
            init["config"],
            serde_json::json!({"cleanup_enabled": true, "mutations_disabled": false})
        );
        let set = json(ConfigAction::Set {
            mutations_disabled: true,
            json: true,
        });
        assert_eq!(set["command"], "config-set");
        assert_eq!(set["enables_cleanup"], false);
        assert!(set.get("created").is_none());
        let show = json(ConfigAction::Show { json: true });
        assert_eq!(show["command"], "config-show");
        assert_eq!(show["config"]["mutations_disabled"], true);
    }

    #[test]
    fn set_refuses_without_a_config_and_writes_none() {
        use crate::guard::failure_code;
        let uid = crate::sys::effective_uid();
        let config = scratch_directory("config-missing").join("config.json");

        let error = set_mutations_disabled(&config, uid, true).unwrap_err();

        assert_eq!(failure_code(error.as_ref()), "config_untrusted");
        assert!(!config.exists());
    }

    #[test]
    fn the_package_user_is_the_passwd_entry_of_that_name() {
        let passwd = "root:x:0:0::/root:/bin/sh\nSynoReclaim:x:180818:180818::/var/empty:/sbin/nologin\nSynoReclaimX:x:5:5::/:/bin/false\n";
        assert_eq!(uid_in_passwd(passwd, "SynoReclaim"), Some(180818));
        assert_eq!(uid_in_passwd(passwd, "Syno"), None);
        assert_eq!(uid_in_passwd("SynoReclaim:x:abc:1", "SynoReclaim"), None);
    }

    #[test]
    fn the_human_answer_says_whether_cleanup_is_allowed_and_where() {
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::sys::effective_uid();
        let config = scratch_directory("config-render").join("config.json");
        std::fs::write(
            &config,
            br#"{"cleanup_enabled":true,"allowed_roots":["/volume1/s"]}"#,
        )
        .unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();

        let doc = run(
            &ConfigAction::Init { json: false },
            &config,
            uid,
            std::time::UNIX_EPOCH,
        );

        assert_eq!(
            render(&doc.unwrap()),
            "configuration already present; left as is\n\
             cleanup enabled yes\nmutations disabled no\ncleanup allowed now yes\n\
             allowed roots\n  /volume1/s\n"
        );
    }
}
