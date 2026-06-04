//! Phase 3 (T-D04) watcher tests: feedback-loop prevention (M3-G05), debounce
//! batching (M3-G06), watcher reflects new/edited/deleted items, and the startup
//! sweep picks up out-of-band changes. Unix-only (drives real signals).
#![cfg(unix)]
#![allow(clippy::zombie_processes)]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use clove_core::{ItemStore, ItemType, NewItem, Priority};
use clove_ipc::{DaemonClient, QueryKind, QueryRequest};

fn cloved_bin() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_BIN_EXE_cloved"))
}

struct Repo {
    _tmp: tempfile::TempDir,
    root: Utf8PathBuf,
    clove_dir: Utf8PathBuf,
}

fn init_repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap().to_owned();
    let clove_dir = root.join(".clove");
    std::fs::create_dir_all(clove_dir.join("issues")).unwrap();
    std::fs::write(
        clove_dir.join("config.toml"),
        "schema = 1\nid_prefix = \"proj\"\n",
    )
    .unwrap();
    Repo {
        _tmp: tmp,
        root,
        clove_dir,
    }
}

impl Repo {
    fn add_item(&self, title: &str) -> String {
        let store = ItemStore::new(self.root.clone());
        let item = store
            .create(
                "proj",
                NewItem {
                    title: title.to_owned(),
                    item_type: ItemType::Feature,
                    priority: Priority(1),
                    labels: Vec::new(),
                    deps: Vec::new(),
                    parent: None,
                    assignee: None,
                    body: String::new(),
                },
                Utc::now(),
            )
            .unwrap();
        item.frontmatter.id.to_string()
    }

    fn reindex(&self) {
        clove_index::reindex(
            &self.clove_dir.join("issues"),
            &self.clove_dir.join("index.db"),
        )
        .unwrap();
    }
}

fn spawn_ready(clove_dir: &Utf8Path) -> Child {
    let child = Command::new(cloved_bin())
        .arg("run")
        .arg("--clove-dir")
        .arg(clove_dir.as_str())
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

fn list_all() -> QueryRequest {
    QueryRequest {
        kind: QueryKind::List,
        status: None,
        item_type: None,
        priority: None,
        assignee: None,
        label: None,
        offset: 0,
        limit: None,
    }
}

fn count_via_daemon(clove_dir: &Utf8Path) -> usize {
    let mut client = DaemonClient::probe(clove_dir).expect("daemon alive");
    client.query_list(list_all()).unwrap().rows.len()
}

fn batches(clove_dir: &Utf8Path) -> u64 {
    let mut client = DaemonClient::probe(clove_dir).expect("daemon alive");
    client.status().unwrap().batches_applied
}

/// Poll until `f()` holds or `timeout` elapses.
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    f()
}

/// Spawn the daemon with extra env (e.g. the snapshot/idle overrides) and wait
/// until its pid file appears.
fn spawn_ready_env(clove_dir: &Utf8Path, env: &[(&str, &str)]) -> Child {
    let mut cmd = Command::new(cloved_bin());
    cmd.arg("run").arg("--clove-dir").arg(clove_dir.as_str());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn cloved");
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

/// M4: a running daemon records `clove stats` history points on its interval.
#[test]
fn daemon_auto_snapshots_on_interval() {
    let repo = init_repo();
    repo.add_item("tracked work");
    repo.reindex();

    // Snapshot every 150ms; never idle-shut-down during the test.
    let mut child = spawn_ready_env(
        &repo.clove_dir,
        &[
            ("CLOVED_STATS_SNAPSHOT_MS", "150"),
            ("CLOVED_IDLE_SHUTDOWN_MS", "0"),
        ],
    );

    let db = repo.clove_dir.join("index.db");
    let recorded = wait_until(Duration::from_secs(5), || {
        clove_index::Index::open(&db)
            .ok()
            .and_then(|i| i.snapshot_count().ok())
            .unwrap_or(0)
            >= 1
    });

    sigterm(child.id());
    let _ = child.wait();

    assert!(
        recorded,
        "daemon must auto-record at least one stats snapshot"
    );
    // The recorded snapshot reflects the one tracked item.
    let index = clove_index::Index::open(&db).unwrap();
    let hist = index.snapshot_history(None, Some(1)).unwrap();
    assert_eq!(hist[0].report.total, 1);
}

#[test]
fn startup_sweep_picks_up_out_of_band_items() {
    let repo = init_repo();
    repo.add_item("created before daemon");
    // No reindex: the index.db doesn't exist yet, so the startup sweep is what
    // must index this item before the daemon serves it.
    let mut child = spawn_ready(&repo.clove_dir);
    assert_eq!(
        count_via_daemon(&repo.clove_dir),
        1,
        "startup sweep indexed it"
    );
    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn watcher_reflects_new_item() {
    let repo = init_repo();
    repo.add_item("first");
    repo.reindex();
    let mut child = spawn_ready(&repo.clove_dir);
    assert_eq!(count_via_daemon(&repo.clove_dir), 1);

    // Add an item out-of-band; the watcher must pick it up.
    repo.add_item("second");
    let ok = wait_until(Duration::from_secs(3), || {
        count_via_daemon(&repo.clove_dir) == 2
    });
    assert!(ok, "watcher did not index the new item");

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn reindex_does_not_trigger_watcher_batches() {
    // M3-G05: writing index.db (via `reindex`) must produce zero watcher batches,
    // because index.db lives outside the watched issues/ dir.
    let repo = init_repo();
    repo.add_item("one");
    repo.reindex();
    let mut child = spawn_ready(&repo.clove_dir);
    let before = batches(&repo.clove_dir);

    // Rebuild the index repeatedly — only touches .clove/index.db*.
    for _ in 0..3 {
        repo.reindex();
    }
    std::thread::sleep(Duration::from_millis(600));
    let after = batches(&repo.clove_dir);
    assert_eq!(
        after, before,
        "index.db writes must not be watched (feedback loop)"
    );

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn startup_sweep_1k_50_modified_under_500ms() {
    // M3-G02: with 1k items already indexed and 50 changed out-of-band, the
    // daemon must complete its startup sweep and become ready in < 500ms.
    let repo = init_repo();
    let mut ids = Vec::new();
    for i in 0..1000 {
        ids.push(repo.add_item(&format!("item {i}")));
    }
    repo.reindex();

    // Modify 50 files out-of-band (so the sweep has real work).
    for id in ids.iter().take(50) {
        let path = repo.clove_dir.join("issues").join(format!("{id}.md"));
        let body = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{body}\nswept\n")).unwrap();
    }

    let start = Instant::now();
    let mut child = spawn_ready(&repo.clove_dir); // returns once the pid (readiness) appears
    let ready = start.elapsed();
    assert!(
        ready < Duration::from_millis(500),
        "startup sweep + ready took {ready:?} (gate: < 500ms)"
    );
    assert_eq!(count_via_daemon(&repo.clove_dir), 1000);

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn rapid_edits_debounce_into_one_batch() {
    // M3-G06: 10 chunks 10ms apart to one file → exactly one applied batch.
    let repo = init_repo();
    let id = repo.add_item("debounced");
    repo.reindex();
    let mut child = spawn_ready(&repo.clove_dir);
    let before = batches(&repo.clove_dir);

    let path = repo.clove_dir.join("issues").join(format!("{id}.md"));
    let base = std::fs::read_to_string(&path).unwrap();
    for i in 0..10 {
        // Append a comment line (keeps frontmatter valid) 10ms apart.
        std::fs::write(&path, format!("{base}\nedit {i}\n")).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }

    // Wait past the debounce window for the single batch to land.
    let ok = wait_until(Duration::from_secs(3), || batches(&repo.clove_dir) > before);
    assert!(ok, "debounced batch never applied");
    std::thread::sleep(Duration::from_millis(400));
    let delta = batches(&repo.clove_dir) - before;
    assert_eq!(delta, 1, "rapid edits must coalesce into exactly one batch");

    sigterm(child.id());
    let _ = child.wait();
}
