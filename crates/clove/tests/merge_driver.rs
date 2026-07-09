//! T-M05: `clove merge-driver` end-to-end tests driven by **real `git merge`**.
//!
//! Each test builds a real git repo in a tempdir, installs the merge driver via
//! `clove init --merge-driver`, then **rewrites the `.git/config` driver line to
//! the absolute path of the binary under test** (`assert_cmd::cargo::cargo_bin`)
//! so that `git merge` invokes *this* build's `clove merge-driver`, not whatever
//! `clove` might be on `$PATH`. The auto-resolution tests (V-I14/V-I15) prove the
//! driver actually ran: without it those merges would conflict.
//!
//! Maps to VERIFICATION_PLAN.md V-I13–V-I16.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use tempfile::TempDir;

/// A `clove` command rooted at `dir`, with a deterministic environment.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

/// A `git` command rooted at `dir` with identity + signing disabled so commits
/// succeed in a bare sandbox.
fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    cmd.env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

fn git_ok(dir: &Path, args: &[&str]) {
    let status = git(dir).args(args).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Init a git repo, install the clove merge driver, and pin the driver to the
/// binary under test (absolute path).
fn setup_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git_ok(p, &["init", "-q", "-b", "main"]);
    git_ok(p, &["config", "commit.gpgsign", "false"]);
    git_ok(p, &["config", "user.name", "T"]);
    git_ok(p, &["config", "user.email", "t@example.com"]);
    // Keep line endings byte-for-byte across platforms. Windows CI runners set
    // core.autocrlf=true globally, which would rewrite item files to CRLF on
    // checkout — desyncing the base/ours/theirs the merge driver diffs and
    // breaking these exact-content assertions. clove item files are always LF.
    git_ok(p, &["config", "core.autocrlf", "false"]);
    git_ok(p, &["config", "core.eol", "lf"]);

    clove(p)
        .args(["init", "--prefix", "proj", "--merge-driver"])
        .assert()
        .success();

    // Repoint the installed driver at THIS build's binary (absolute path), so
    // `git merge` runs the version we just compiled regardless of $PATH.
    let bin = assert_cmd::cargo::cargo_bin("clove");
    let bin = bin.to_str().unwrap();
    git_ok(
        p,
        &[
            "config",
            "merge.clove-item.driver",
            &format!("{bin} merge-driver %O %A %B %L"),
        ],
    );

    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "init"]);
    dir
}

/// Like [`setup_repo`] but also creates one item and commits it, so tests that
/// edit a shared item on two branches have a base to diverge from.
fn setup_repo_with_item() -> TempDir {
    let dir = setup_repo();
    let p = dir.path();
    clove(p).args(["new", "Item one"]).assert().success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "item"]);
    dir
}

/// The id of the single item in the repo.
fn only_item_id(dir: &Path) -> String {
    let out = clove(dir)
        .args(["ls", "--format", "json", "--limit", "0"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"][0]["id"].as_str().unwrap().to_owned()
}

fn item_path(dir: &Path, id: &str) -> std::path::PathBuf {
    dir.join(".clove").join("issues").join(format!("{id}.md"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Replace the whole `deps: [...]` line in an item file.
fn set_deps_line(path: &Path, new_line: &str) {
    let text = read(path);
    let replaced: String = text
        .lines()
        .map(|l| {
            if l.starts_with("deps:") {
                new_line.to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{replaced}\n")).unwrap();
}

/// Run `git merge <branch>` and return whether it exited cleanly (0).
fn git_merge(dir: &Path, branch: &str) -> bool {
    git(dir)
        .args(["merge", "--no-edit", branch])
        .status()
        .unwrap()
        .success()
}

fn has_conflict_markers(text: &str) -> bool {
    text.contains("<<<<<<<") || text.contains("=======") || text.contains(">>>>>>>")
}

// --------------------------------------------------------------------------
// V-I13 — parallel new items (sanity; no driver needed).
// --------------------------------------------------------------------------

#[test]
fn v_i13_parallel_new_items_no_conflict() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    // main already has one item from setup; remove that noise by branching first.
    git_ok(p, &["checkout", "-qb", "a"]);
    clove(p).args(["new", "Item A"]).assert().success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    clove(p).args(["new", "Item B"]).assert().success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(git_merge(p, "b"), "parallel new items must merge cleanly");

    // Both new items present; setup item + A + B = 3 distinct ids.
    let out = clove(p)
        .args(["ls", "--format", "json", "--limit", "0"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ids: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "all three items present: {ids:?}");
    let unique: std::collections::HashSet<&&str> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "ids are distinct: {ids:?}");
}

// --------------------------------------------------------------------------
// V-I14 — same-value status edit auto-resolves.
// --------------------------------------------------------------------------

#[test]
fn v_i14_same_value_status_auto_resolves() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);

    git_ok(p, &["checkout", "-qb", "a"]);
    clove(p).args(["close", &id]).assert().success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "close on a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    clove(p).args(["close", &id]).assert().success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "close on b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        git_merge(p, "b"),
        "same-value status edit must auto-resolve (exit 0) — proves the driver ran"
    );

    let text = read(&item_path(p, &id));
    assert!(!has_conflict_markers(&text), "no markers:\n{text}");
    assert!(text.contains("status: closed"), "status closed:\n{text}");
    // The closed timestamp must be a valid RFC3339 instant.
    let closed_line = text
        .lines()
        .find(|l| l.starts_with("closed:"))
        .expect("closed timestamp present");
    let ts = closed_line.trim_start_matches("closed:").trim();
    chrono::DateTime::parse_from_rfc3339(ts).expect("valid RFC3339 closed timestamp");
}

// --------------------------------------------------------------------------
// V-I15 — dep union merge.
// --------------------------------------------------------------------------

#[test]
fn v_i15_dep_union_merge() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);
    let path = item_path(p, &id);

    git_ok(p, &["checkout", "-qb", "a"]);
    set_deps_line(&path, "deps: [proj-AAAAAAAA]");
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "add AAAA on a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    set_deps_line(&path, "deps: [proj-BBBBBBBB]");
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "add BBBB on b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(git_merge(p, "b"), "dep union must auto-merge (exit 0)");

    let text = read(&path);
    assert!(!has_conflict_markers(&text), "no markers:\n{text}");
    assert!(
        text.contains("deps: [proj-AAAAAAAA, proj-BBBBBBBB]"),
        "deps union, sorted:\n{text}"
    );
}

// --------------------------------------------------------------------------
// V-I16 — dep removal conflict.
// --------------------------------------------------------------------------

#[test]
fn v_i16_dep_removal_conflict() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);
    let path = item_path(p, &id);

    // Base: deps = [proj-OLDELEM0].
    set_deps_line(&path, "deps: [proj-OLDELEM0]");
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "base dep"]);

    git_ok(p, &["checkout", "-qb", "a"]);
    set_deps_line(&path, "deps: []"); // A removes OLD.
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "remove OLD on a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    set_deps_line(&path, "deps: [proj-NEWELEM0, proj-OLDELEM0]"); // B keeps OLD, adds NEW.
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "keep OLD add NEW on b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        !git_merge(p, "b"),
        "dep removal/add on same element must conflict (nonzero)"
    );

    let text = read(&path);
    // The conflict block names the deps field and carries markers.
    assert!(has_conflict_markers(&text), "markers present:\n{text}");
    assert!(text.contains("deps:"), "deps field referenced:\n{text}");
    // Isolated to deps: no other field appears inside the conflict markers.
    for forbidden in ["status:", "title:", "priority:"] {
        let in_marker_region = text.split("<<<<<<<").skip(1).any(|seg| {
            seg.split(">>>>>>>")
                .next()
                .unwrap_or("")
                .contains(forbidden)
        });
        assert!(
            !in_marker_region,
            "{forbidden} leaked into conflict:\n{text}"
        );
    }
}

// --------------------------------------------------------------------------
// Divergent scalar conflict.
// --------------------------------------------------------------------------

#[test]
fn divergent_scalar_status_conflicts() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);

    git_ok(p, &["checkout", "-qb", "a"]);
    clove(p).args(["start", &id]).assert().success(); // in_progress
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "start on a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    clove(p).args(["close", &id]).assert().success(); // closed
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "close on b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        !git_merge(p, "b"),
        "divergent status (in_progress vs closed) must conflict"
    );
    let text = read(&item_path(p, &id));
    assert!(has_conflict_markers(&text), "markers:\n{text}");
    assert!(text.contains("status:"), "status conflict shown:\n{text}");
}

// --------------------------------------------------------------------------
// Clean disjoint scalar merge (different fields edited).
// --------------------------------------------------------------------------

#[test]
fn clean_disjoint_scalar_merge() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);

    git_ok(p, &["checkout", "-qb", "a"]);
    clove(p)
        .args(["set", &id, "title=Renamed title"])
        .assert()
        .success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "title on a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    clove(p).args(["set", &id, "priority=0"]).assert().success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "priority on b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(git_merge(p, "b"), "disjoint scalar edits must auto-merge");
    let text = read(&item_path(p, &id));
    assert!(!has_conflict_markers(&text), "no markers:\n{text}");
    assert!(text.contains("title: Renamed title"), "title kept:\n{text}");
    assert!(text.contains("priority: 0"), "priority kept:\n{text}");
}

// --------------------------------------------------------------------------
// Body merges: one-sided clean; both-sided conflict.
// --------------------------------------------------------------------------

#[test]
fn one_sided_body_edit_merges_clean() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);
    let path = item_path(p, &id);

    // Give the base a body.
    let base = read(&path);
    std::fs::write(&path, format!("{base}Original body line.\n")).unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "base body"]);

    git_ok(p, &["checkout", "-qb", "a"]);
    let t = read(&path);
    std::fs::write(
        &path,
        t.replace("Original body line.", "Original body line.\nAdded by A."),
    )
    .unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "body a"]);

    git_ok(p, &["checkout", "-q", "main"]); // main unchanged body
    git_ok(p, &["checkout", "-qb", "b"]);
    clove(p).args(["set", &id, "priority=1"]).assert().success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "field b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(git_merge(p, "b"), "one-sided body edit must merge clean");
    let text = read(&path);
    assert!(!has_conflict_markers(&text), "no markers:\n{text}");
    assert!(
        text.contains("Added by A."),
        "body change preserved:\n{text}"
    );
    assert!(
        text.contains("priority: 1"),
        "field change preserved:\n{text}"
    );
}

#[test]
fn conflicting_body_edits_produce_markers() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);
    let path = item_path(p, &id);

    let base = read(&path);
    std::fs::write(&path, format!("{base}Shared body line.\n")).unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "base body"]);

    git_ok(p, &["checkout", "-qb", "a"]);
    let t = read(&path);
    std::fs::write(&path, t.replace("Shared body line.", "Body edited by A.")).unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "body a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    let t = read(&path);
    std::fs::write(&path, t.replace("Shared body line.", "Body edited by B.")).unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "body b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        !git_merge(p, "b"),
        "conflicting body edits must conflict (nonzero)"
    );
    let text = read(&path);
    assert!(has_conflict_markers(&text), "body markers present:\n{text}");
    assert!(
        text.contains("Body edited by A."),
        "ours body present:\n{text}"
    );
    assert!(
        text.contains("Body edited by B."),
        "theirs body present:\n{text}"
    );
}

// --------------------------------------------------------------------------
// Unparseable side → nonzero, ours not destroyed.
// --------------------------------------------------------------------------

// --------------------------------------------------------------------------
// add/add: same item id created on two branches with no common ancestor (%O
// empty). The frontmatter merge must run against base=None: same-value fields
// stay, divergent scalars conflict, dep lists union.
// --------------------------------------------------------------------------

/// Write a minimal valid clove item file at `<id>.md` with the given title and
/// deps line, so two branches can independently "add" the same id.
fn write_item(path: &Path, id: &str, title: &str, deps: &str) {
    // The `.clove/issues` dir can vanish on a branch with no items (git prunes
    // empty dirs on checkout); recreate it so the add/add write always lands.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let body = format!(
        "---\n\
schema: 1\n\
id: {id}\n\
title: {title}\n\
status: open\n\
type: feature\n\
priority: 2\n\
created: 2026-01-01T00:00:00Z\n\
updated: 2026-01-01T00:00:00Z\n\
deps: {deps}\n\
---\n\
Body for {id}.\n"
    );
    std::fs::write(path, body).unwrap();
}

#[test]
fn add_add_same_id_unions_deps_no_ancestor() {
    // Base has NO item, so when both branches add the same id git invokes the
    // driver with an EMPTY ancestor (`%O`). The dep lists must union (base=None
    // means every element is an addition, never a removal), and the disjoint
    // titles/same scalars must not spuriously clobber.
    let dir = setup_repo();
    let p = dir.path();
    let id = "proj-ADDADD01";
    let path = item_path(p, id);

    git_ok(p, &["checkout", "-qb", "a"]);
    write_item(&path, id, "Shared title", "[proj-AAAAAAAA]");
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "add on a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    write_item(&path, id, "Shared title", "[proj-BBBBBBBB]");
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "add on b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        git_merge(p, "b"),
        "add/add with same title + disjoint deps must auto-merge (union)"
    );
    let text = read(&path);
    assert!(!has_conflict_markers(&text), "no markers:\n{text}");
    assert!(
        text.contains("deps: [proj-AAAAAAAA, proj-BBBBBBBB]"),
        "deps union under empty ancestor:\n{text}"
    );
    assert!(text.contains("title: Shared title"), "title kept:\n{text}");
}

#[test]
fn add_add_divergent_scalar_conflicts_without_clobbering_ours() {
    // Same id added on both branches (empty ancestor) but with a divergent
    // scalar (priority). That must conflict, and ours must survive intact.
    let dir = setup_repo();
    let p = dir.path();
    let id = "proj-ADDADD02";
    let path = item_path(p, id);

    git_ok(p, &["checkout", "-qb", "a"]);
    write_item(&path, id, "Ours title", "[]");
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "add ours"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    // Same id, different title (divergent scalar) → conflict.
    write_item(&path, id, "Theirs title", "[]");
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "add theirs"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        !git_merge(p, "b"),
        "add/add with divergent title must conflict (empty ancestor, base=None)"
    );
    let text = read(&path);
    // Canonical frontmatter still carries ours; the conflict block makes the
    // divergence explicit. Ours must not be clobbered.
    assert!(
        text.contains("title: Ours title"),
        "ours preserved on add/add conflict:\n{text}"
    );
    assert!(text.contains("title:"), "title divergence shown:\n{text}");
}

// --------------------------------------------------------------------------
// Simultaneous frontmatter + body conflict in one file: both the field
// conflict block and the git body markers appear, exit nonzero.
// --------------------------------------------------------------------------

#[test]
fn simultaneous_field_and_body_conflict() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);
    let path = item_path(p, &id);

    // Give the base a body line both sides will edit divergently.
    let base = read(&path);
    std::fs::write(&path, format!("{base}Shared body line.\n")).unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "base body"]);

    git_ok(p, &["checkout", "-qb", "a"]);
    clove(p).args(["start", &id]).assert().success(); // status → in_progress
    let t = read(&path);
    std::fs::write(&path, t.replace("Shared body line.", "Body edited by A.")).unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "field+body a"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    clove(p).args(["close", &id]).assert().success(); // status → closed (divergent)
    let t = read(&path);
    std::fs::write(&path, t.replace("Shared body line.", "Body edited by B.")).unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "field+body b"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        !git_merge(p, "b"),
        "divergent field AND divergent body must conflict (nonzero)"
    );
    let text = read(&path);
    // Body conflict: git markers present, both sides shown.
    assert!(has_conflict_markers(&text), "body markers present:\n{text}");
    assert!(text.contains("Body edited by A."), "ours body:\n{text}");
    assert!(text.contains("Body edited by B."), "theirs body:\n{text}");
    // Field conflict: the dedicated frontmatter conflict block + status field.
    assert!(
        text.contains("clove: frontmatter merge conflict"),
        "field conflict block present:\n{text}"
    );
    assert!(
        text.contains("status:"),
        "status field conflict shown:\n{text}"
    );
}

#[test]
fn unparseable_side_conflicts_without_clobbering_ours() {
    let dir = setup_repo_with_item();
    let p = dir.path();
    let id = only_item_id(p);
    let path = item_path(p, &id);

    git_ok(p, &["checkout", "-qb", "a"]);
    clove(p)
        .args(["set", &id, "title=Valid ours title"])
        .assert()
        .success();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "ours valid"]);

    git_ok(p, &["checkout", "-q", "main"]);
    git_ok(p, &["checkout", "-qb", "b"]);
    // Corrupt theirs into a non-clove file (still tracked by gitattributes).
    std::fs::write(&path, "this is not frontmatter at all\n").unwrap();
    git_ok(p, &["add", "-A"]);
    git_ok(p, &["commit", "-qm", "theirs broken"]);

    git_ok(p, &["checkout", "-q", "a"]);
    assert!(
        !git_merge(p, "b"),
        "unparseable theirs must conflict (nonzero)"
    );
    // Our valid content must survive — the driver must not have clobbered it.
    let text = read(&path);
    assert!(
        text.contains("title: Valid ours title"),
        "ours preserved on unparseable conflict:\n{text}"
    );
}
