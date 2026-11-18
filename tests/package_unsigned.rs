//! The package is released unsigned and runs as its own package user: no
//! Synology signing tooling anywhere in the dev/test/CI flow, and no lifecycle
//! code of its own: `synopkgland build` writes the scripts and only the SDK
//! executor runs as root.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn files_under(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn tooling_files() -> Vec<PathBuf> {
    let mut files = vec![repo("README.md")];
    for dir in [".github", "scripts", "package", "ui/ci"] {
        files_under(&repo(dir), &mut files);
    }
    files
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_none_or(|ext| ext != "png" && ext != "pem")
        })
        .collect()
}

#[test]
fn no_tooling_refers_to_synology_signing() {
    let banned = ["syno-sign", "package/sign.sh", "Sign Server"];
    for path in tooling_files() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for word in banned {
            assert!(
                !text.contains(word),
                "{} mentions {word}",
                path.strip_prefix(repo("")).unwrap().display()
            );
        }
    }
    assert!(!repo("package/sign.sh").exists());
}

#[test]
fn dsm_install_takes_only_the_spk_and_a_thread_id() {
    let output = Command::new("sh")
        .arg(repo("scripts/dsm/install-spk.sh"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let usage = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(usage.contains("<spk> <thread-id>"), "{usage}");
    assert!(
        !usage.contains("serial") && !usage.contains("signed"),
        "{usage}"
    );
}

#[test]
fn no_root_capable_legacy_lifecycle_code_remains() {
    for legacy in [
        "package/src/launch.cpp",
        "package/src/SYNO.SynoReclaim.cpp",
        "package/scripts",
        "package/conf",
        "package/builder",
        "package/prepare.py",
        "tests/package_state.rs",
    ] {
        assert!(!repo(legacy).exists(), "{legacy} must be removed");
    }
}
