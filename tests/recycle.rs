#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn a_deleted_recycle_target_returns_its_resolution_error_as_json_without_scanning() {
    let base = std::env::temp_dir().join(format!("recycle-missing-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let target = base.join("removed-share");
    std::fs::create_dir(&target).unwrap();
    std::fs::remove_dir(&target).unwrap();
    let state = base.join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_synoreclaim"))
        .args([
            "recycle",
            "--json",
            "--no-scan",
            "--oldest",
            "200",
            "--state-dir",
        ])
        .arg(&state)
        .arg("--")
        .arg(&target)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["command"], "recycle");
    assert_eq!(doc["completion"], "failed");
    assert_eq!(doc["errors"][0]["code"], "not_found");
    assert_eq!(doc["errors"][0]["path"], target.to_str().unwrap());
    assert!(
        doc["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("os error 2")
    );
    assert!(doc["report_info"].is_null());
    assert_eq!(doc["shares"], serde_json::json!([]));
    assert_eq!(doc["scans"], serde_json::json!([]));
    assert!(!state.exists(), "a refused read must not create scan state");
    std::fs::remove_dir(base).unwrap();
}
