//! Integration tests for the CLI foundation (T-CLI01): output format
//! resolution, the JSON envelope, and exit codes.

use assert_cmd::Command;
use serde_json::Value;

fn clove() -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    // Isolate from any ambient CLOVE_FORMAT in the developer's environment.
    cmd.env_remove("CLOVE_FORMAT");
    cmd
}

fn stdout_string(args: &[&str], env: Option<(&str, &str)>) -> (bool, String) {
    let mut cmd = clove();
    if let Some((key, value)) = env {
        cmd.env(key, value);
    }
    let output = cmd.args(args).output().unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

#[test]
fn version_human_prints_plain_line() {
    let (ok, out) = stdout_string(&["version"], None);
    assert!(ok);
    assert!(out.starts_with("clove "), "got: {out:?}");
}

#[test]
fn version_json_is_a_valid_envelope() {
    let output = clove()
        .args(["version", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(value["v"], 1);
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["schema"], 1);
    assert!(value["data"]["clove"].is_string());
}

#[test]
fn clove_format_env_selects_json_without_flag() {
    // The T-CLI01 contract: CLOVE_FORMAT=json yields JSON with no --format flag.
    let output = clove()
        .env("CLOVE_FORMAT", "json")
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(value["ok"], true);
}

#[test]
fn flag_overrides_env() {
    // --format human beats CLOVE_FORMAT=json.
    let (ok, out) = stdout_string(
        &["version", "--format", "human"],
        Some(("CLOVE_FORMAT", "json")),
    );
    assert!(ok);
    assert!(out.starts_with("clove "), "got: {out:?}");
}

#[test]
fn unknown_subcommand_exits_usage_1() {
    clove().arg("frobnicate").assert().code(1);
}

#[test]
fn invalid_format_value_exits_usage_1() {
    clove()
        .args(["version", "--format", "xml"])
        .assert()
        .code(1);
}
