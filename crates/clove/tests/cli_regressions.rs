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

/// A human-format list that stops at the window must say so: without the note
/// a capped `clove ls` reads as the whole list.
#[test]
fn human_list_reports_a_truncated_page() {
    let repo = init_repo();
    for title in ["one", "two", "three"] {
        new_item(repo.path(), title, &[]);
    }

    let capped = clove(repo.path())
        .args(["ls", "--limit", "2"])
        .output()
        .unwrap();
    assert_eq!(stdout(&capped).lines().count(), 2, "{capped:?}");
    assert!(
        stderr(&capped).contains("showing 2 of 3 items"),
        "{capped:?}"
    );

    for args in [vec!["ls"], vec!["ls", "--limit", "2", "--offset", "1"]] {
        let whole = clove(repo.path()).args(&args).output().unwrap();
        assert!(!stderr(&whole).contains("showing"), "{args:?}: {whole:?}");
    }

    // `ready` and `search` share the renderer.
    for args in [
        vec!["ready", "--limit", "1"],
        vec!["search", "t", "--limit", "1"],
    ] {
        let out = clove(repo.path()).args(&args).output().unwrap();
        assert!(stderr(&out).contains("showing 1 of"), "{args:?}: {out:?}");
    }
}

/// An export is a backup. An item file that does not parse used to be left
/// out with exit 0 and no word; it must be named on stderr and, for `json`, in
/// `_meta.warnings`.
#[test]
fn export_names_item_files_it_could_not_export() {
    let repo = init_repo();
    let good = new_item(repo.path(), "good", &[]);
    let bad = new_item(repo.path(), "bad", &[]);
    let bad_path = repo.path().join(format!(".clove/issues/{bad}.md"));
    let text = std::fs::read_to_string(&bad_path).unwrap();
    std::fs::write(&bad_path, text.replace("status: open", "status: nonsense")).unwrap();

    let jsonl = clove(repo.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap();
    assert!(jsonl.status.success(), "{jsonl:?}");
    assert_eq!(stdout(&jsonl).lines().count(), 1);
    assert!(stdout(&jsonl).contains(&good));
    assert!(
        stderr(&jsonl).contains("warning: skipped") && stderr(&jsonl).contains(&bad),
        "{jsonl:?}"
    );

    let json = clove(repo.path())
        .args(["export", "json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    let warnings = v["_meta"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{v}");
    assert!(warnings[0].as_str().unwrap().contains(&bad), "{v}");
    assert!(stderr(&json).contains(&bad), "{json:?}");

    // `--out` keeps the same contract and still writes the good item.
    let out_path = repo.path().join("backup.jsonl");
    let to_file = clove(repo.path())
        .args(["export", "jsonl", "--out", "backup.jsonl"])
        .output()
        .unwrap();
    assert!(to_file.status.success(), "{to_file:?}");
    assert!(stderr(&to_file).contains("wrote 1 items"), "{to_file:?}");
    assert!(stderr(&to_file).contains(&bad), "{to_file:?}");
    assert_eq!(std::fs::read_to_string(out_path).unwrap().lines().count(), 1);
}

/// `import --dry-run` and the real import must agree. An invalid item used to
/// pass the dry run ("would create 3") and then stop the real run half-way,
/// with the items before it already written.
#[test]
fn import_validates_every_item_before_writing_any() {
    let source = init_repo();
    for title in ["first", "second", "third"] {
        new_item(source.path(), title, &[]);
    }
    let export = clove(source.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap();
    let mut lines: Vec<Value> = stdout(&export)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    let bad_id = lines[1]["id"].as_str().unwrap().to_owned();
    lines[1]["title"] = Value::String(String::new());
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();

    let target = init_repo();
    let file = target.path().join("in.jsonl");
    std::fs::write(&file, text).unwrap();

    for extra in [&["--dry-run"][..], &[][..]] {
        let out = clove(target.path())
            .args(["import", "jsonl", "in.jsonl"])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4), "{extra:?}: {out:?}");
        assert!(stderr(&out).contains(&bad_id), "{extra:?}: {out:?}");
        let written = std::fs::read_dir(target.path().join(".clove/issues"))
            .unwrap()
            .count();
        assert_eq!(written, 0, "{extra:?}: nothing may be written");
    }
}

/// The body is editable without an editor: `edit --body` / `--body-file`.
#[test]
fn edit_sets_the_body_from_a_flag_or_a_file() {
    let repo = init_repo();
    let id = new_item(repo.path(), "has a body", &["-b", "old"]);
    let body_of = |dir: &Path| {
        let mut show = clove(dir);
        show.args(["show", id.as_str()]);
        json_ok(&mut show)["data"]["body"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    let mut edit = clove(repo.path());
    edit.args(["edit", id.as_str(), "--body", "new text", "--field", "priority=1"]);
    let v = json_ok(&mut edit);
    assert_eq!(v["data"]["priority"], 1);
    assert_eq!(body_of(repo.path()), "new text\n");

    std::fs::write(repo.path().join("body.md"), "from a file\nline 2\n").unwrap();
    clove(repo.path())
        .args(["edit", id.as_str(), "--body-file", "body.md"])
        .assert()
        .success();
    assert_eq!(body_of(repo.path()), "from a file\nline 2\n");

    let mut stdin_edit = clove(repo.path());
    stdin_edit.args(["edit", id.as_str(), "--body-file", "-"]);
    let out = {
        use std::io::Write;
        let mut child = stdin_edit
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"piped").unwrap();
        child.wait_with_output().unwrap()
    };
    assert!(out.status.success(), "{out:?}");
    assert_eq!(body_of(repo.path()), "piped\n");

    // `--body ""` clears the body.
    clove(repo.path())
        .args(["edit", id.as_str(), "--body", ""])
        .assert()
        .success();
    assert_eq!(body_of(repo.path()), "");
}

/// An "editor" that rewrites the file in place (no rename), like nano or VS
/// Code: it applies one `sed` expression to the file it is given.
#[cfg(unix)]
fn in_place_editor(dir: &Path, sed_expr: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("editor.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nsed '{sed_expr}' \"$1\" > \"$1.edit\" && cat \"$1.edit\" > \"$1\" && rm \"$1.edit\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// `clove edit` in an editor is a store write like any other: validated,
/// `updated` stamped, the `closed` invariant kept, a JSON envelope printed, and
/// visible to the index even when the editor wrote the file in place.
#[cfg(unix)]
#[test]
fn editor_edit_is_saved_through_the_store() {
    let repo = init_repo();
    let id = new_item(repo.path(), "edit me", &[]);
    clove(repo.path()).arg("reindex").assert().success();
    let mut show = clove(repo.path());
    show.args(["show", id.as_str()]);
    let before = json_ok(&mut show)["data"].clone();
    std::thread::sleep(std::time::Duration::from_millis(20));

    let editor = in_place_editor(repo.path(), "s/^status: open$/status: closed/");
    let mut edit = clove(repo.path());
    edit.env("CLOVE_EDITOR", &editor).args(["edit", id.as_str()]);
    let saved = json_ok(&mut edit);
    assert_eq!(saved["data"]["status"], "closed");
    assert!(saved["data"]["closed"].is_string(), "{saved}");
    assert_ne!(saved["data"]["updated"], before["updated"]);

    // The index tier sees the change: no open item is left.
    let mut ls = clove(repo.path());
    ls.args(["ls", "--status", "open"]);
    let listed = json_ok(&mut ls);
    assert_eq!(listed["_meta"]["total"], 0, "{listed}");
    assert_eq!(listed["data"].as_array().unwrap().len(), 0, "{listed}");

    // An editor that changes nothing leaves the file byte-identical.
    let path = repo.path().join(format!(".clove/issues/{id}.md"));
    let bytes = std::fs::read(&path).unwrap();
    let noop = in_place_editor(repo.path(), "s/^zzz$/zzz/");
    clove(repo.path())
        .env("CLOVE_EDITOR", &noop)
        .args(["edit", id.as_str()])
        .assert()
        .success();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// An editor session that leaves an invalid item must not leave it in the
/// store: the original file is restored and the rejected text is kept.
#[cfg(unix)]
#[test]
fn editor_edit_that_does_not_validate_is_rolled_back() {
    let repo = init_repo();
    let id = new_item(repo.path(), "edit me", &[]);
    let path = repo.path().join(format!(".clove/issues/{id}.md"));
    let original = std::fs::read(&path).unwrap();

    let editor = in_place_editor(repo.path(), "s/^status: open$/status: nonsense/");
    let out = clove(repo.path())
        .env("CLOVE_EDITOR", &editor)
        .env("TMPDIR", repo.path())
        .args(["edit", id.as_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert!(stderr(&out).contains("clove-edit-"), "{out:?}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let kept = std::fs::read_dir(repo.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().starts_with("clove-edit-"))
        .expect("the rejected text is kept");
    assert!(std::fs::read_to_string(kept.path())
        .unwrap()
        .contains("status: nonsense"));
}
