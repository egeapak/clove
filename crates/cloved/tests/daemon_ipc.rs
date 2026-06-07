//! Phase 2 (T-D03) IPC tests: PING round-trip latency (M3-G01), QUERY parity with
//! the direct index read, REINDEX, STATUS, and stale-socket recovery (M3-G04) at
//! the client level. Unix-only (drives real signals).
#![cfg(unix)]
#![allow(clippy::zombie_processes)]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use clove_core::{ItemStore, ItemType, NewItem, Priority};
use clove_index::{Filter, Index, QueryMode};
use clove_ipc::{DaemonClient, QueryKind, QueryRequest};

fn cloved_bin() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_BIN_EXE_cloved"))
}

/// Build a `.clove/` with `n` items and a freshly reindexed `index.db`.
fn init_repo_with_items(n: usize) -> (tempfile::TempDir, Utf8PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap().to_owned();
    let clove_dir = root.join(".clove");
    std::fs::create_dir_all(clove_dir.join("issues")).unwrap();
    std::fs::write(
        clove_dir.join("config.toml"),
        "schema = 1\nid_prefix = \"proj\"\n",
    )
    .unwrap();

    let store = ItemStore::new(root.clone());
    for i in 0..n {
        store
            .create(
                "proj",
                NewItem {
                    title: format!("item {i}"),
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
    }
    clove_index::reindex(&clove_dir.join("issues"), &clove_dir.join("index.db")).unwrap();
    (dir, clove_dir)
}

fn spawn_ready(clove_dir: &Utf8Path) -> Child {
    let child = Command::new(cloved_bin())
        .env("CLOVED_DISABLE_WEB", "1") // avoid all test daemons contending for port 7373
        .arg("run")
        .arg("--clove-dir")
        .arg(clove_dir.as_str())
        .spawn()
        .expect("spawn cloved");
    let pid_file = clove_dir.join("daemon.pid");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if pid_file.exists() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon not ready");
}

fn list_request(kind: QueryKind) -> QueryRequest {
    QueryRequest {
        kind,
        status: None,
        item_type: None,
        priority: None,
        assignee: None,
        label: None,
        offset: 0,
        limit: None,
    }
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
fn sigkill(pid: u32) {
    unsafe {
        libc_kill(pid as i32, 9);
    }
}

#[test]
fn ping_round_trip_is_fast() {
    let (_tmp, clove_dir) = init_repo_with_items(3);
    let mut child = spawn_ready(&clove_dir);

    let mut client = DaemonClient::probe(&clove_dir).expect("daemon alive");
    // Warm one round-trip, then measure (M3-G01: PING/PONG < 5ms).
    client.ping().unwrap();
    let start = Instant::now();
    client.ping().unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(5),
        "PING round-trip {elapsed:?} exceeds 5ms gate"
    );

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn query_matches_direct_index_read() {
    let (_tmp, clove_dir) = init_repo_with_items(5);
    let mut child = spawn_ready(&clove_dir);

    // Daemon-served rows.
    let mut client = DaemonClient::probe(&clove_dir).expect("daemon alive");
    let via_daemon = client.query_list(list_request(QueryKind::List)).unwrap();

    // Direct index read of the same db.
    let index = Index::open(&clove_dir.join("index.db")).unwrap();
    let direct = index
        .query_list(&Filter {
            mode: QueryMode::List,
            status: None,
            item_type: None,
            priority: None,
            assignee: None,
            label: None,
            parent: None,
            limit: None,
        })
        .unwrap();

    assert_eq!(via_daemon.total, 5);
    assert_eq!(via_daemon.rows.len(), direct.len());
    let daemon_ids: Vec<&str> = via_daemon.rows.iter().map(|r| r.id.as_str()).collect();
    let direct_ids: Vec<&str> = direct.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        daemon_ids, direct_ids,
        "daemon order must match index order"
    );

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn status_and_reindex_round_trip() {
    let (_tmp, clove_dir) = init_repo_with_items(2);
    let mut child = spawn_ready(&clove_dir);

    let mut client = DaemonClient::probe(&clove_dir).expect("daemon alive");
    let status = client.status().unwrap();
    assert_eq!(status.items_indexed, 2);

    let report = client.reindex().unwrap();
    assert_eq!(report.items_indexed, 2);

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn stale_socket_recovery_is_fast() {
    let (_tmp, clove_dir) = init_repo_with_items(1);
    let mut child = spawn_ready(&clove_dir);
    // Hard-kill leaves a corpse socket + pid.
    sigkill(child.id());
    let _ = child.wait();
    assert!(clove_dir.join("daemon.sock").exists());

    // The next probe must fail fast (connect timeout + cleanup) and clean up
    // (DESIGN §8.3; M3-G04 measures the resulting `clove ls` < 200ms).
    let start = Instant::now();
    let client = DaemonClient::probe(&clove_dir);
    let elapsed = start.elapsed();
    assert!(client.is_none(), "no live daemon");
    assert!(
        elapsed < Duration::from_millis(200),
        "stale-socket probe {elapsed:?} exceeds 200ms"
    );
    assert!(
        !clove_dir.join("daemon.sock").exists(),
        "stale sock cleaned"
    );
    assert!(!clove_dir.join("daemon.pid").exists(), "stale pid cleaned");
}

#[test]
fn search_and_graph_over_ipc() {
    use clove_ipc::{GraphRequest, GraphResponse, SearchRequest};
    let (_tmp, clove_dir) = init_repo_with_items(3);
    let mut child = spawn_ready(&clove_dir);
    let mut client = DaemonClient::probe(&clove_dir).expect("daemon alive");

    // SEARCH returns ids for a matching title token ("item" is in every title).
    let ids = client
        .search(SearchRequest {
            text: "item".to_owned(),
            limit: None,
        })
        .unwrap();
    assert_eq!(ids.len(), 3, "search matches all three items");

    // GRAPH: no deps yet → no cycles, nothing blocked.
    match client.graph(GraphRequest::Cycles).unwrap() {
        GraphResponse::Cycles { cycles } => assert!(cycles.is_empty()),
        other => panic!("expected Cycles, got {other:?}"),
    }
    match client
        .graph(GraphRequest::Blocked {
            include_warnings: false,
        })
        .unwrap()
    {
        GraphResponse::Blocked { ids } => assert!(ids.is_empty(), "nothing blocked"),
        other => panic!("expected Blocked, got {other:?}"),
    }

    sigterm(child.id());
    let _ = child.wait();
}
