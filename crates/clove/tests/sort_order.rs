//! Read-path §1: one sort contract, every surface.
//!
//! The load-bearing test for `clove_core::view::Order` is a *triple* comparison:
//! the file path (`--no-index`), the index path (SQL `ORDER BY`), and the daemon
//! path (the same SQL, behind an RPC) must return **identical id sequences** for
//! every `SortField`. They are three independent implementations of one
//! comparator — a Rust `sort_by`, a SQLite clause, and a wire field — and any
//! divergence means `--no-index` silently changes results.
//!
//! The fixture is built so each field discriminates *differently*: no two sort
//! fields produce the same id sequence, so a clause that reads the wrong column
//! (or ignores the request entirely and answers in `rank` order, which is what a
//! dropped wire field looks like) fails rather than coincidentally passing.

use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;

fn clove() -> Command {
    Command::new(cargo_bin("clove"))
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    clove().current_dir(dir).args(args).output().unwrap()
}

/// Write one item file directly.
///
/// Deliberately *not* `clove new`: the timestamps have to be distinct and
/// controlled (two `clove new` calls in the same second tie on `created`, which
/// would make the `created`/`updated` rows of this test vacuous).
#[allow(clippy::too_many_arguments)]
fn write_item(
    root: &Path,
    id: &str,
    status: &str,
    priority: u8,
    item_type: &str,
    created_day: u8,
    updated_day: u8,
    deps: &[&str],
) {
    let mut s = format!(
        "---\nschema: 1\nid: {id}\ntitle: {id}\nstatus: {status}\ntype: {item_type}\n\
         priority: {priority}\ncreated: 2026-06-0{created_day}T10:00:00Z\n\
         updated: 2026-06-0{updated_day}T10:00:00Z\n"
    );
    if status == "closed" {
        s.push_str("closed: 2026-06-09T11:00:00Z\n");
    }
    if !deps.is_empty() {
        s.push_str("deps:\n");
        for d in deps {
            s.push_str(&format!("  - {d}\n"));
        }
    }
    s.push_str("---\nbody\n");
    std::fs::write(root.join(".clove/issues").join(format!("{id}.md")), s).unwrap();
}

const A: &str = "proj-AAAAAAAA";
const B: &str = "proj-BBBBBBBB";
const C: &str = "proj-CCCCCCCC";
const D: &str = "proj-DDDDDDDD";
const E: &str = "proj-EEEEEEEE";

/// A five-item store in which every sort field orders the items differently.
///
/// | id | priority | status      | type    | created | updated | deps |
/// |----|----------|-------------|---------|---------|---------|------|
/// | A  | 3        | closed      | chore   | 06-05   | 06-03   | —    |
/// | B  | 1        | open        | bug     | 06-04   | 06-05   | —    |
/// | C  | 4        | in_progress | epic    | 06-03   | 06-01   | —    |
/// | D  | 1        | open        | docs    | 06-02   | 06-04   | B    |
/// | E  | 0        | closed      | feature | 06-01   | 06-02   | —    |
///
/// `D → B` is what separates `rank` from `priority`: they tie at priority 1, and
/// the dependent (D) takes the lower topological rank.
fn build_fixture(root: &Path) {
    assert!(run_in(root, &["init", "--prefix", "proj"]).status.success());
    write_item(root, A, "closed", 3, "chore", 5, 3, &[]);
    write_item(root, B, "open", 1, "bug", 4, 5, &[]);
    write_item(root, C, "in_progress", 4, "epic", 3, 1, &[]);
    write_item(root, D, "open", 1, "docs", 2, 4, &[B]);
    write_item(root, E, "closed", 0, "feature", 1, 2, &[]);
}

/// The expected ascending id sequence for each sort field, spelled out rather
/// than derived — a bug that moves all three paths together is still caught.
fn expected() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        // priority, then topological rank (D before B, its dependency), then id.
        ("rank", vec![E, D, B, A, C]),
        // priority alone: B and D tie at p1 and fall through to the id tiebreak,
        // which is what makes this differ from `rank`.
        ("priority", vec![E, B, D, A, C]),
        ("created", vec![E, D, C, B, A]),
        ("updated", vec![C, E, A, D, B]),
        ("id", vec![A, B, C, D, E]),
        // Lifecycle order, not alphabetical: open → in_progress → closed.
        ("status", vec![B, D, C, A, E]),
        // Declaration order: bug → feature → chore → docs → epic.
        ("type", vec![B, E, A, D, C]),
    ]
}

/// `(ids, _meta.source)` from a `--format json` list response.
fn ids_and_source(out: &[u8]) -> (Vec<String>, String) {
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    let ids = v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("not a list response: {v}"))
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_owned())
        .collect();
    let source = v["_meta"]["source"].as_str().unwrap_or("").to_owned();
    (ids, source)
}

fn ls_args<'a>(field: &'a str, descending: bool, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["ls", "--sort", field, "-f", "json"];
    if descending {
        args.push("--desc");
    }
    args.extend_from_slice(extra);
    args
}

/// The fixture must actually discriminate: if two fields produced the same
/// sequence, a clause reading the wrong column would pass by accident.
#[test]
fn the_fixture_orders_differently_for_every_field() {
    let all: Vec<Vec<&str>> = expected().into_iter().map(|(_, ids)| ids).collect();
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(a, b, "two sort fields share a sequence — fixture is weak");
        }
        assert_eq!(a.len(), 5, "every field must order the whole store");
    }
}

/// The file path and the index path agree, for every field and both directions.
///
/// Runs everywhere (no daemon, so no skip): this is the half of the triple that
/// `--no-index` exposes directly to users.
#[test]
fn file_and_index_paths_agree_for_every_sort_field() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);
    assert!(run_in(root, &["reindex"]).status.success());

    for (field, want) in expected() {
        for descending in [false, true] {
            let mut want: Vec<String> = want.iter().map(|s| (*s).to_owned()).collect();
            if descending {
                want.reverse();
            }

            let (files, source) =
                ids_and_source(&run_in(root, &ls_args(field, descending, &["--no-index"])).stdout);
            assert_eq!(source, "files", "--no-index must take the file path");
            assert_eq!(files, want, "file path, --sort {field} desc={descending}");

            let (index, source) =
                ids_and_source(&run_in(root, &ls_args(field, descending, &[])).stdout);
            assert_eq!(
                source, "index",
                "the index must answer, or this comparison is vacuous"
            );
            assert_eq!(index, files, "index path, --sort {field} desc={descending}");

            // `ready` runs a *different* SQL predicate (and, on the file path, a
            // different code path: `GraphStore::ready_items` rather than a plain
            // scan), so it gets its own comparison rather than riding on `ls`.
            let ready_args = |extra: &[&'static str]| -> Vec<&str> {
                let mut a = vec!["ready", "--sort", field, "-f", "json"];
                if descending {
                    a.push("--desc");
                }
                a.extend_from_slice(extra);
                a
            };
            let (ready_files, source) =
                ids_and_source(&run_in(root, &ready_args(&["--no-index"])).stdout);
            assert_eq!(source, "files");
            assert!(!ready_files.is_empty(), "the ready set must be non-empty");
            let (ready_index, source) = ids_and_source(&run_in(root, &ready_args(&[])).stdout);
            assert_eq!(source, "index");
            assert_eq!(
                ready_index, ready_files,
                "ready index path, --sort {field} desc={descending}"
            );
        }
    }
}

/// Paging is stable under every order: consecutive windows tile the result set
/// exactly once. This is what the id tiebreak buys — over a partial order, ties
/// resolve to whatever the scan produced and rows repeat or vanish between
/// pages. Checked on the index path, where the window is pushed into SQL as
/// `LIMIT offset + limit`, so a wrong order returns the wrong *rows*.
#[test]
fn windows_tile_the_result_set_under_every_order() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);
    assert!(run_in(root, &["reindex"]).status.success());

    for (field, _) in expected() {
        let (all, _) = ids_and_source(&run_in(root, &ls_args(field, false, &[])).stdout);
        let mut paged: Vec<String> = Vec::new();
        for offset in [0, 2, 4] {
            let offset = offset.to_string();
            let args = ls_args(field, false, &["--limit", "2", "--offset", &offset]);
            paged.extend(ids_and_source(&run_in(root, &args).stdout).0);
        }
        assert_eq!(paged, all, "--sort {field}: windows must tile exactly once");
    }
}

/// `--sort` reaches `_meta`, so a client can read back the ordering that was
/// applied rather than assuming the surface default.
#[test]
fn meta_echoes_the_sort_and_direction() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);

    let meta = |args: &[&str]| -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_slice(&run_in(root, args).stdout).unwrap();
        v["_meta"].clone()
    };
    let m = meta(&["--no-index", "ls", "-f", "json"]);
    assert_eq!(m["sort"], "rank", "the default is echoed, not omitted");
    assert_eq!(m["dir"], "asc");

    let m = meta(&[
        "--no-index",
        "ls",
        "--sort",
        "updated",
        "--desc",
        "-f",
        "json",
    ]);
    assert_eq!(m["sort"], "updated");
    assert_eq!(m["dir"], "desc");

    // Search defaults to relevance, which is not a `SortField`.
    let m = meta(&["--no-index", "search", "proj", "-f", "json"]);
    assert_eq!(m["sort"], "relevance");
    let m = meta(&["--no-index", "search", "proj", "--sort", "id", "-f", "json"]);
    assert_eq!(m["sort"], "id");
}

/// An unknown `--sort` is a validation error, not a silent fall back to `rank`.
#[test]
fn an_unknown_sort_field_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_fixture(root);

    for args in [
        vec!["ls", "--sort", "nope", "-f", "json"],
        vec!["ready", "--sort", "nope", "-f", "json"],
        vec!["blocked", "--sort", "nope", "-f", "json"],
        vec!["search", "x", "--sort", "nope", "-f", "json"],
    ] {
        let out = run_in(root, &args);
        assert!(!out.status.success(), "{args:?} should have failed");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], false, "{args:?}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR", "{args:?}");
    }
}

/// `search` keeps relevance-first by default, and an explicit `--sort` replaces
/// that key entirely rather than tie-breaking within it.
#[test]
fn search_defaults_to_relevance_and_an_explicit_sort_replaces_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run_in(root, &["init", "--prefix", "proj"]).status.success());
    // A newer item whose hit is only in the *body*, and an older one whose hit
    // is in the title. Relevance ranks the title first; `--sort updated --desc`
    // must invert that.
    write_item(root, A, "open", 2, "bug", 1, 1, &[]);
    // B depends on A, so B takes the lower topological rank while A takes the
    // lower id — that is what separates `--sort rank` from `--sort priority`
    // here (both items share a priority).
    write_item(root, B, "open", 2, "bug", 2, 5, &[A]);
    let path = |id: &str| root.join(".clove/issues").join(format!("{id}.md"));
    let retitle = |id: &str, title: &str, body: &str| {
        let text = std::fs::read_to_string(path(id)).unwrap();
        let text = text
            .replace(&format!("title: {id}"), &format!("title: {title}"))
            .replace("---\nbody\n", &format!("---\n{body}\n"));
        std::fs::write(path(id), text).unwrap();
    };
    retitle(A, "widget rendering", "nothing here");
    retitle(B, "unrelated", "mentions widget in the body");

    let ids = |args: &[&str]| ids_and_source(&run_in(root, args).stdout).0;
    assert_eq!(
        ids(&["--no-index", "search", "widget", "-f", "json"]),
        vec![A.to_owned(), B.to_owned()],
        "default ranking is relevance: the title hit leads"
    );
    assert_eq!(
        ids(&[
            "--no-index",
            "search",
            "widget",
            "--sort",
            "updated",
            "--desc",
            "-f",
            "json"
        ]),
        vec![B.to_owned(), A.to_owned()],
        "an explicit sort replaces the relevance key, not just its tail"
    );
    // `--desc` with no `--sort` reverses relevance rather than being ignored.
    assert_eq!(
        ids(&["--no-index", "search", "widget", "--desc", "-f", "json"]),
        vec![B.to_owned(), A.to_owned()],
    );

    // `--sort rank` is the one field that needs the topological ranks, and a
    // search does not otherwise build the graph. Without them `rank` silently
    // degenerates to `(priority, id)` — i.e. to the `priority` answer below.
    assert!(run_in(root, &["reindex"]).status.success());
    for extra in [vec!["--no-index"], vec![]] {
        let with = |field: &str| {
            let mut a = vec!["search", "widget", "--sort", field, "-f", "json"];
            a.extend_from_slice(&extra);
            ids(&a)
        };
        assert_eq!(
            with("rank"),
            vec![B.to_owned(), A.to_owned()],
            "rank: the dependent (B) leads, {extra:?}"
        );
        assert_eq!(
            with("priority"),
            vec![A.to_owned(), B.to_owned()],
            "priority ties, so the id tiebreak decides, {extra:?}"
        );
    }
}

/// The third path: a live `cloved`. The sort rides `QueryRequest.order`, so a
/// dropped wire field shows up here as `rank` order for every request.
#[cfg(unix)]
// The daemon is reaped by `sigterm` + `wait` at the end of each test; clippy
// cannot see across the SIGTERM, so the lint is suppressed here as it is in
// `daemon_routing.rs`.
#[allow(clippy::zombie_processes)]
mod daemon {
    use super::*;
    use std::path::PathBuf;
    use std::process::Child;
    use std::time::{Duration, Instant};

    fn cloved_bin() -> Option<PathBuf> {
        let path = cargo_bin("clove").with_file_name("cloved");
        path.exists().then_some(path)
    }

    fn spawn_daemon(clove_dir: &Path, bin: &Path) -> Child {
        let child = Command::new(bin)
            .arg("run")
            .arg("--clove-dir")
            .arg(clove_dir)
            .spawn()
            .expect("spawn cloved");
        let pid = clove_dir.join("daemon.pid");
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if pid.exists() {
                return child;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon not ready");
    }

    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }
    fn sigterm(pid: u32) {
        unsafe {
            libc_kill(pid as i32, 15);
        }
    }

    #[test]
    fn daemon_path_agrees_with_the_file_path_for_every_sort_field() {
        let Some(bin) = cloved_bin() else {
            eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        build_fixture(root);
        assert!(run_in(root, &["reindex"]).status.success());

        // `ls` and `ready` are the two `QueryKind`s that ride the wire `order`.
        let cases: Vec<(&str, String, bool)> = ["ls", "ready"]
            .into_iter()
            .flat_map(|cmd| {
                expected()
                    .into_iter()
                    .flat_map(move |(field, _)| {
                        [false, true]
                            .into_iter()
                            .map(move |descending| (cmd, field.to_owned(), descending))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let args = |cmd: &str, field: &str, descending: bool, local: bool| -> Vec<String> {
            let mut a: Vec<String> = Vec::new();
            if local {
                a.push("--no-index".to_owned());
            }
            a.extend([cmd, "--sort", field, "-f", "json"].map(str::to_owned));
            if descending {
                a.push("--desc".to_owned());
            }
            a
        };
        let run = |a: &[String]| {
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            ids_and_source(&run_in(root, &a).stdout)
        };

        // Ground truth from the file path, gathered before the daemon exists.
        let truth: Vec<(&str, String, bool, Vec<String>)> = cases
            .iter()
            .map(|(cmd, field, descending)| {
                let (ids, source) = run(&args(cmd, field, *descending, true));
                assert_eq!(source, "files");
                assert!(!ids.is_empty(), "{cmd} --sort {field}: empty fixture");
                (*cmd, field.clone(), *descending, ids)
            })
            .collect();

        let clove_dir = root.join(".clove");
        let mut daemon = spawn_daemon(&clove_dir, &bin);

        let mut failures = Vec::new();
        for (cmd, field, descending, want) in &truth {
            let (ids, source) = run(&args(cmd, field, *descending, false));
            if source != "daemon" {
                failures.push(format!(
                    "{cmd} --sort {field}: served by `{source}`, not the daemon"
                ));
            } else if ids != *want {
                failures.push(format!(
                    "{cmd} --sort {field} desc={descending}: daemon {ids:?} != files {want:?}"
                ));
            }
        }

        sigterm(daemon.id());
        let _ = daemon.wait();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// `clove blocked` has no index tier — its daemon path returns *ids in rank
    /// order* over the wire and the CLI reorders locally. That local step is a
    /// second implementation of the same comparator, so it gets its own check.
    #[test]
    fn blocked_daemon_path_agrees_with_the_file_path() {
        let Some(bin) = cloved_bin() else {
            eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert!(run_in(root, &["init", "--prefix", "proj"]).status.success());
        // Three blocked items with differing keys, all waiting on open E.
        //
        // A and B tie on priority and B depends on A, so B takes the lower
        // topological rank while A takes the lower id: `rank` and `priority`
        // therefore disagree, which is what makes an ignored order detectable
        // (this path reorders in the CLI, using the daemon's rank sequence for
        // `rank` and a local sort for everything else).
        write_item(root, A, "open", 1, "chore", 5, 3, &[E]);
        write_item(root, B, "open", 1, "bug", 4, 5, &[A, E]);
        write_item(root, C, "in_progress", 4, "epic", 3, 1, &[E]);
        write_item(root, E, "open", 2, "feature", 1, 2, &[]);
        assert!(run_in(root, &["reindex"]).status.success());

        let fields = ["rank", "priority", "created", "updated", "id", "type"];
        let truth: Vec<(&str, bool, Vec<String>)> = fields
            .iter()
            .flat_map(|f| [false, true].map(|d| (*f, d)))
            .map(|(field, descending)| {
                let mut args = vec!["--no-index", "blocked", "--sort", field, "-f", "json"];
                if descending {
                    args.push("--desc");
                }
                let (ids, source) = ids_and_source(&run_in(root, &args).stdout);
                assert_eq!(source, "files");
                assert_eq!(ids.len(), 3, "--sort {field}: fixture must be non-empty");
                (field, descending, ids)
            })
            .collect();
        // The fixture separates `rank` from `priority`, so a path that answers
        // every request in rank order cannot pass by coincidence.
        let seq = |name: &str| {
            truth
                .iter()
                .find(|(f, d, _)| *f == name && !*d)
                .map(|(_, _, ids)| ids.clone())
                .unwrap()
        };
        assert_ne!(seq("rank"), seq("priority"), "blocked fixture is weak");

        let clove_dir = root.join(".clove");
        let mut daemon = spawn_daemon(&clove_dir, &bin);
        let mut failures = Vec::new();
        for (field, descending, want) in &truth {
            let mut args = vec!["blocked", "--sort", field, "-f", "json"];
            if *descending {
                args.push("--desc");
            }
            let (ids, source) = ids_and_source(&run_in(root, &args).stdout);
            if source != "daemon" {
                failures.push(format!("--sort {field}: served by `{source}`"));
            } else if ids != *want {
                failures.push(format!(
                    "--sort {field} desc={descending}: daemon {ids:?} != files {want:?}"
                ));
            }
        }
        sigterm(daemon.id());
        let _ = daemon.wait();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
