//! Phase 2 (T-D03) end-to-end: `clove ls`/`ready`/`query` route through a running
//! `cloved` and produce output identical (bar `_meta.source`) to the local path.
//! Unix-only (drives real signals). Spawns the sibling `cloved` binary from the
//! same target dir; skips cleanly if it is not built (only happens outside the
//! `cargo test --workspace` gate).
#![cfg(unix)]
#![allow(clippy::zombie_processes)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;

fn cloved_bin() -> Option<PathBuf> {
    let path = cargo_bin("clove").with_file_name("cloved");
    path.exists().then_some(path)
}

fn clove() -> Command {
    Command::new(cargo_bin("clove"))
}

fn run_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    clove().current_dir(dir).args(args).output().unwrap()
}

fn spawn_daemon(clove_dir: &std::path::Path, bin: &std::path::Path) -> Child {
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

/// Parse the `data` ids and `_meta.source` from a `--format json` list output.
fn ids_and_source(out: &[u8]) -> (Vec<String>, String) {
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    let ids = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_owned())
        .collect();
    let source = v["_meta"]["source"].as_str().unwrap_or("").to_owned();
    (ids, source)
}

#[test]
fn ls_ready_query_route_through_daemon_with_parity() {
    let Some(bin) = cloved_bin() else {
        eprintln!("skipping: cloved binary not built (run via `cargo test --workspace`)");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run_in(root, &["init"]).status.success());
    for title in ["alpha", "beta", "gamma"] {
        assert!(run_in(root, &["new", title]).status.success());
    }
    assert!(run_in(root, &["reindex"]).status.success());

    let clove_dir = root.join(".clove");
    let mut daemon = spawn_daemon(&clove_dir, &bin);

    // Ground truth: the file-scan path (no index, no daemon).
    let (mut want_ls, _) =
        ids_and_source(&run_in(root, &["--no-index", "ls", "-f", "json"]).stdout);
    want_ls.sort();

    for (cmd, args) in [
        ("ls", vec!["ls", "-f", "json"]),
        ("ready", vec!["ready", "-f", "json"]),
        ("query", vec!["query", "-f", "json"]),
    ] {
        let (mut ids, source) = ids_and_source(&run_in(root, &args).stdout);
        assert_eq!(source, "daemon", "{cmd} must be served by the daemon");
        ids.sort();
        assert_eq!(
            ids, want_ls,
            "{cmd} via daemon must match the file-scan id set"
        );
    }

    sigterm(daemon.id());
    let _ = daemon.wait();

    // With the daemon gone, the same read falls back cleanly (index path) and the
    // stale socket/pid are cleaned up by the liveness probe.
    let (_, source) = ids_and_source(&run_in(root, &["ls", "-f", "json"]).stdout);
    assert_ne!(source, "daemon", "no daemon → fall back");
    assert!(
        !clove_dir.join("daemon.sock").exists(),
        "stale sock cleaned"
    );
}

/// Parse the new-item id from `clove new ... -f json`.
fn new_id(out: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    v["data"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn tier1_tier2_commands_route_through_daemon_with_parity() {
    let Some(bin) = cloved_bin() else {
        eprintln!("skipping: cloved binary not built (run via `cargo test --workspace`)");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run_in(root, &["init"]).status.success());
    let a = new_id(&run_in(root, &["new", "alpha apple", "-f", "json"]).stdout);
    let b = new_id(&run_in(root, &["new", "beta banana", "-f", "json"]).stdout);
    let c = new_id(&run_in(root, &["new", "gamma grape", "-f", "json"]).stdout);
    // a depends on b (b open → a blocked); c depends on a (→ c blocked too).
    assert!(run_in(root, &["dep", "add", &a, &b]).status.success());
    assert!(run_in(root, &["dep", "add", &c, &a]).status.success());
    assert!(run_in(root, &["reindex"]).status.success());

    // Ground truth (no daemon, file scan).
    let mut want_blocked = list_ids(&run_in(root, &["--no-index", "blocked", "-f", "json"]).stdout);
    want_blocked.sort();
    let want_cycle_count = cycle_count(&run_in(root, &["dep", "cycle", "-f", "json"]).stdout);
    let want_tree = tree_shape(&run_in(root, &["dep", "tree", &c, "-f", "json"]).stdout);

    let clove_dir = root.join(".clove");
    let mut daemon = spawn_daemon(&clove_dir, &bin);

    // search does NOT route to the daemon — see
    // `search_is_a_file_scan_even_with_a_live_daemon` for why, and for the
    // needles that make it observable.
    let (search_ids, search_src) =
        ids_and_source(&run_in(root, &["search", "apple", "-f", "json"]).stdout);
    assert_eq!(search_src, "files", "search has no daemon tier");
    assert_eq!(search_ids, vec![a.clone()], "search finds 'alpha apple'");

    // blocked → daemon, same set as the file path.
    let (mut blocked_ids, blocked_src) =
        ids_and_source(&run_in(root, &["blocked", "-f", "json"]).stdout);
    assert_eq!(blocked_src, "daemon", "blocked routes to the daemon");
    blocked_ids.sort();
    assert_eq!(
        blocked_ids, want_blocked,
        "blocked set matches the file path"
    );

    // dep cycle → daemon, same count.
    assert_eq!(
        cycle_count(&run_in(root, &["dep", "cycle", "-f", "json"]).stdout),
        want_cycle_count
    );

    // dep tree → daemon, same shape.
    assert_eq!(
        tree_shape(&run_in(root, &["dep", "tree", &c, "-f", "json"]).stdout),
        want_tree
    );

    // reindex delegates to the daemon (still reports the item count).
    let rv: serde_json::Value =
        serde_json::from_slice(&run_in(root, &["reindex", "-f", "json"]).stdout).unwrap();
    assert_eq!(rv["data"]["items_indexed"], serde_json::json!(3));

    sigterm(daemon.id());
    let _ = daemon.wait();
}

fn list_ids(out: &[u8]) -> Vec<String> {
    ids_and_source(out).0
}

fn cycle_count(out: &[u8]) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    v["_meta"]["count"].as_u64().unwrap()
}

/// A stable "shape" string of a dep tree: root id + the ids of its children.
fn tree_shape(out: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    let root = v["data"]["id"].as_str().unwrap();
    let kids: Vec<&str> = v["data"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    format!("{root}:{}", kids.join(","))
}

fn sigkill(pid: u32) {
    unsafe {
        libc_kill(pid as i32, 9);
    }
}

/// If the daemon crashes mid-session, the next routed read must transparently
/// fall back to the local path with identical results, cleaning up the corpse
/// socket on the way (DESIGN §8.3).
#[test]
fn routed_reads_fall_back_after_daemon_crash() {
    let Some(bin) = cloved_bin() else {
        eprintln!("skipping: cloved binary not built (run via `cargo test --workspace`)");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run_in(root, &["init"]).status.success());
    let a = new_id(&run_in(root, &["new", "alpha apple", "-f", "json"]).stdout);
    let b = new_id(&run_in(root, &["new", "beta", "-f", "json"]).stdout);
    assert!(run_in(root, &["dep", "add", &a, &b]).status.success());
    assert!(run_in(root, &["reindex"]).status.success());

    let mut want_blocked = list_ids(&run_in(root, &["--no-index", "blocked", "-f", "json"]).stdout);
    want_blocked.sort();

    let clove_dir = root.join(".clove");
    let daemon = spawn_daemon(&clove_dir, &bin);

    // Confirm it routes while alive.
    let (_, src) = ids_and_source(&run_in(root, &["blocked", "-f", "json"]).stdout);
    assert_eq!(src, "daemon", "blocked routes while the daemon is alive");

    // Hard-kill (no clean shutdown → corpse socket/pid left behind).
    sigkill(daemon.id());
    let mut daemon = daemon;
    let _ = daemon.wait();
    assert!(
        clove_dir.join("daemon.sock").exists(),
        "corpse socket remains"
    );

    // Next routed reads fall back, return identical results, and clean up.
    let (mut blocked_ids, blocked_src) =
        ids_and_source(&run_in(root, &["blocked", "-f", "json"]).stdout);
    assert_ne!(blocked_src, "daemon", "fell back to the local path");
    blocked_ids.sort();
    assert_eq!(
        blocked_ids, want_blocked,
        "fallback blocked set is identical"
    );

    let (search_ids, search_src) =
        ids_and_source(&run_in(root, &["search", "apple", "-f", "json"]).stdout);
    assert_ne!(search_src, "daemon", "search fell back");
    assert_eq!(search_ids, vec![a], "fallback search is correct");

    assert!(
        !clove_dir.join("daemon.sock").exists(),
        "corpse socket cleaned by the liveness probe"
    );
    assert!(!clove_dir.join("daemon.pid").exists(), "corpse pid cleaned");
}

/// `clove search` answers identically with a live daemon, with only a local
/// index, and with neither — the third leg of the read-path §6.1 invariant
/// (`crates/clove/tests/cli_commands.rs::search_agrees_across_the_file_and_index_paths`
/// covers the first two).
///
/// Protocol v5 had a `search` RPC that ran the daemon's hot FTS5 index and
/// returned matched ids. FTS matches whole tokens with ASCII-only case folding,
/// so the daemon's match set was strictly narrower than the file scan's, and
/// merely *starting* a daemon changed what `clove search` found. v6 removed the
/// RPC with the FTS table itself.
///
/// The needles below are the ones that make the difference visible: a whole
/// token agreed on both paths even when the bug was live, which is why the old
/// test could not see it.
#[test]
fn search_is_a_file_scan_even_with_a_live_daemon() {
    let Some(bin) = cloved_bin() else {
        eprintln!("skipping: cloved binary not built (run via `cargo test --workspace`)");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run_in(root, &["init", "--prefix", "proj"]).status.success());
    let issues = root.join(".clove/issues");
    // `corepart` holds `core` mid-token; `ünicode-tag` holds `icode` mid-token
    // and is only reachable by a case-folded non-ASCII needle.
    for (suffix, title, body, label) in [
        ("AAAAAAAA", "Payments gateway", "unrelated prose", ""),
        ("BBBBBBBB", "Plain title", "the corepart word", ""),
        ("CCCCCCCC", "Plain title two", "plain body", "ünicode-tag"),
    ] {
        let id = format!("proj-{suffix}");
        let labels = if label.is_empty() {
            String::new()
        } else {
            format!("labels:\n  - {label}\n")
        };
        std::fs::write(
            issues.join(format!("{id}.md")),
            format!(
                "---\nschema: 1\nid: {id}\ntitle: {title}\nstatus: open\ntype: feature\n\
                 priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n\
                 {labels}---\n{body}\n"
            ),
        )
        .unwrap();
    }
    assert!(run_in(root, &["reindex"]).status.success());

    let needles = ["gateway", "core", "icode", "Ünicode"];
    let ground: Vec<Vec<String>> = needles
        .iter()
        .map(|n| list_ids(&run_in(root, &["search", n, "--no-index", "-f", "json"]).stdout))
        .collect();
    // The fixture must actually discriminate: without hits on the mid-token and
    // non-ASCII needles this test would pass against the bug it targets.
    assert_eq!(ground[1], vec!["proj-BBBBBBBB"], "mid-token `core`");
    assert_eq!(ground[2], vec!["proj-CCCCCCCC"], "mid-token `icode`");
    assert_eq!(ground[3], vec!["proj-CCCCCCCC"], "non-ASCII `Ünicode`");

    let clove_dir = root.join(".clove");
    let mut daemon = spawn_daemon(&clove_dir, &bin);

    // The daemon has to be genuinely live, or "search did not use it" is vacuous.
    let (_, ls_src) = ids_and_source(&run_in(root, &["ls", "-f", "json"]).stdout);
    assert_eq!(ls_src, "daemon", "the daemon must be answering something");

    for (needle, want) in needles.iter().zip(&ground) {
        let (ids, src) = ids_and_source(&run_in(root, &["search", needle, "-f", "json"]).stdout);
        assert_eq!(src, "files", "search must not route to the daemon");
        assert_eq!(
            &ids, want,
            "{needle:?} must answer identically with a daemon live"
        );
    }

    sigterm(daemon.id());
    let _ = daemon.wait();
}
