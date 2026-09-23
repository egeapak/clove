//! A reader that exits early (`clove ls | head -1`) must end clove's output
//! quietly: exit 0, no panic. The read end is closed *before* the spawn so every
//! write deterministically hits EPIPE instead of landing in the pipe buffer.

use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::prelude::*;

fn run_with_closed_stdout(bin: &str, dir: &Path, args: &[&str]) -> Output {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    Command::cargo_bin(bin)
        .unwrap()
        .current_dir(dir)
        .env_remove("CLOVE_FORMAT")
        .env("CLOVE_AUTHOR", "tester@example.com")
        .args(args)
        .stdout(writer)
        .output()
        .unwrap()
}

fn assert_quiet_exit(output: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{what}: expected exit 0, got {:?}; stderr: {stderr}",
        output.status
    );
    assert!(!stderr.contains("panicked"), "{what}: panicked: {stderr}");
    assert!(
        !stderr.to_lowercase().contains("broken pipe"),
        "{what}: reported the broken pipe as an error: {stderr}"
    );
}

#[test]
fn closed_stdout_ends_output_quietly() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("clove")
        .unwrap()
        .current_dir(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    Command::cargo_bin("clove")
        .unwrap()
        .current_dir(dir.path())
        .env("CLOVE_AUTHOR", "tester@example.com")
        .args(["new", "An item"])
        .assert()
        .success();

    for args in [
        &["agent-doc"][..],
        &["ls"],
        &["--format", "json", "ls"],
        &["stats"],
        &["export", "json"],
        &["export", "jsonl"],
    ] {
        let output = run_with_closed_stdout("clove", dir.path(), args);
        assert_quiet_exit(&output, &args.join(" "));
    }
}
