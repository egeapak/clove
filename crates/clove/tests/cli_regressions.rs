//! End-to-end regression tests for CLI defects: each test pins one fixed bug
//! (argument handling, envelope contract, export/import, `--clove-dir`).

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A `clove` invocation rooted at `dir`, isolated from the developer's own
/// daemon and home (a private runtime dir, clove home, and `HOME`).
fn clove(dir: &Path) -> Command {
    let mut c = Command::cargo_bin("clove").unwrap();
    c.current_dir(dir);
    c.env_remove("CLOVE_FORMAT");
    c.env_remove("EDITOR");
    c.env_remove("CLOVE_EDITOR");
    c.env_remove("VISUAL");
    c.env("CLOVE_AUTHOR", "tester@example.com");
    c.env("CLOVE_RUNTIME_DIR", dir.join(".test-runtime"));
    c.env("CLOVE_HOME", dir.join(".test-clove-home"));
    c.env("HOME", dir);
    c
}

/// Initialize a repo in a fresh temp dir and return it.
fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    dir
}

/// Run a command expecting a JSON success envelope and return the parsed value.
fn json_ok(cmd: &mut Command) -> Value {
    let out = cmd.arg("--format").arg("json").output().unwrap();
    assert!(out.status.success(), "command failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["ok"], true, "envelope not ok: {v}");
    v
}

/// Create an item and return its id.
fn new_item(dir: &Path, title: &str, extra: &[&str]) -> String {
    let mut cmd = clove(dir);
    cmd.arg("new").arg(title).args(extra);
    let v = json_ok(&mut cmd);
    v["data"]["id"].as_str().unwrap().to_owned()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `--clove-dir` names the `.clove` directory itself. The store always lives in
/// `<parent>/.clove`, so a directory with any other name used to be
/// half-honored: `init` wrote its config there, and every later command then
/// read and wrote `<parent>/.clove` instead.
#[test]
fn clove_dir_must_be_a_dot_clove_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let out = clove(dir.path())
        .args(["--clove-dir", store.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert!(stderr(&out).contains("--clove-dir"), "{out:?}");
    assert!(!store.exists(), "init must not create the refused directory");
}

/// A sibling directory beside a real `.clove` must not be accepted as an
/// override: the item used to land in the real store while the index and the
/// plugin environment pointed at the sibling.
#[test]
fn clove_dir_sibling_of_a_real_store_is_refused() {
    let repo = init_repo();
    let snapshot = repo.path().join("snapshot-clove");
    std::fs::create_dir_all(snapshot.join("issues")).unwrap();
    let out = clove(repo.path())
        .args(["--clove-dir", snapshot.to_str().unwrap(), "new", "t"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    let issues = std::fs::read_dir(repo.path().join(".clove/issues"))
        .unwrap()
        .count();
    assert_eq!(issues, 0, "nothing may be written to the real store");
}

/// Item text is untrusted (item files, synced trackers). Human output must not
/// pass terminal control sequences through; JSON keeps the stored value.
#[test]
fn human_output_strips_terminal_control_sequences() {
    let repo = init_repo();
    let title = "safe\u{1b}[2K\rforged";
    let id = new_item(repo.path(), title, &["-b", "body\u{1b}]52;c;QUJD\u{7}end"]);

    for args in [vec!["ls"], vec!["show", id.as_str()], vec!["search", "safe"]] {
        let out = clove(repo.path()).args(&args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        let text = stdout(&out);
        assert!(text.contains("safe[2Kforged"), "{args:?}: {text:?}");
        assert!(
            !text.contains('\u{1b}') && !text.contains('\r') && !text.contains('\u{7}'),
            "{args:?} leaked a control character: {text:?}"
        );
    }

    let mut show = clove(repo.path());
    show.args(["show", id.as_str()]);
    assert_eq!(json_ok(&mut show)["data"]["title"], title);
}
