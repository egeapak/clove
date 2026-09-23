//! Phase 2 (T-D03) IPC tests: PING round-trip latency (M3-G01), QUERY parity with
//! the direct index read, REINDEX, STATUS, and stale-socket recovery (M3-G04) at
//! the client level. Unix-only (drives real signals).
#![cfg(unix)]

mod support;

use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use clove_core::{ItemStore, NewItem};
use clove_index::{Filter, Index, QueryMode};
use clove_ipc::{DaemonClient, QueryKind, QueryRequest};
use clove_types::{ItemType, Priority};
use support::{TestHub, SIGKILL};

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

fn list_request(kind: QueryKind) -> QueryRequest {
    QueryRequest {
        kind,
        filters: Default::default(),
        order: Default::default(),
        offset: 0,
        limit: None,
    }
}

#[test]
fn ping_round_trip_is_fast() {
    let (_tmp, clove_dir) = init_repo_with_items(3);
    let hub = TestHub::spawn(Some(&clove_dir));

    let mut client = hub.client(&clove_dir);
    // Warm one round-trip, then measure (M3-G01: PING/PONG < 5ms). The best
    // of several samples is the hub's latency; a single one on a loaded
    // machine is mostly the scheduler's.
    client.ping().unwrap();
    let best = (0..20)
        .map(|_| {
            let start = Instant::now();
            client.ping().unwrap();
            start.elapsed()
        })
        .min()
        .unwrap();
    assert!(
        best < Duration::from_millis(5),
        "best PING round-trip {best:?} exceeds 5ms gate"
    );
}

#[test]
fn query_matches_direct_index_read() {
    let (_tmp, clove_dir) = init_repo_with_items(5);
    let hub = TestHub::spawn(Some(&clove_dir));

    // Daemon-served rows.
    let mut client = hub.client(&clove_dir);
    let via_daemon = client.query_list(list_request(QueryKind::List)).unwrap();

    // Direct index read of the same db.
    let index = Index::open(&clove_dir.join("index.db")).unwrap();
    let direct = index
        .query_list(&Filter {
            mode: QueryMode::List,
            order: Default::default(),
            ..Default::default()
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
}

#[test]
fn status_and_reindex_round_trip() {
    let (_tmp, clove_dir) = init_repo_with_items(2);
    let hub = TestHub::spawn(Some(&clove_dir));

    let mut client = hub.client(&clove_dir);
    let status = client.status().unwrap();
    assert_eq!(status.items_indexed, 2);

    let report = client.reindex().unwrap();
    assert_eq!(report.items_indexed, 2);
}

#[test]
fn mutations_round_trip_through_daemon() {
    // Topology B: writes go through the single daemon, which performs them on the
    // file store and keeps itself coherent. Verify create → edit → comment →
    // dep_add → show all round-trip and land on disk.
    use clove_types::{ItemStatus, NewSpec};

    let (_tmp, clove_dir) = init_repo_with_items(0);
    let hub = TestHub::spawn(Some(&clove_dir));
    let mut client = hub.client(&clove_dir);

    // create
    let created = client
        .create(NewSpec {
            title: "via daemon".to_owned(),
            priority: Some(1),
            ..Default::default()
        })
        .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    assert!(created["path"].as_str().unwrap().contains(&id));
    // The file actually exists on disk.
    assert!(clove_dir.join("issues").join(format!("{id}.md")).exists());

    // edit: set status + add a label atomically.
    let edited = client
        .edit(
            id.clone(),
            vec!["assignee=alice".to_owned(), "labels+=urgent".to_owned()],
        )
        .unwrap();
    assert_eq!(edited["assignee"], "alice");
    assert_eq!(edited["labels"], serde_json::json!(["urgent"]));

    // set_status → closed.
    let closed = client.set_status(id.clone(), ItemStatus::Closed).unwrap();
    assert_eq!(closed["status"], "closed");

    // a second item + a dependency edge (cycle-checked daemon-side).
    let dep = client
        .create(NewSpec {
            title: "dependency".to_owned(),
            ..Default::default()
        })
        .unwrap();
    let dep_id = dep["id"].as_str().unwrap().to_owned();
    let with_dep = client.dep_add(id.clone(), dep_id.clone()).unwrap();
    assert_eq!(with_dep["deps"], serde_json::json!([dep_id]));
    // Negative: a self-loop is rejected by the daemon's validation pipeline.
    assert!(client.dep_add(id.clone(), id.clone()).is_err());

    // apply_edit: a structured edit including a body (the new v3 capability),
    // proving the EditRequest rides the wire and the body lands on disk.
    let renamed = client
        .apply_edit(
            id.clone(),
            clove_types::EditRequest {
                title: Some("renamed via daemon".to_owned()),
                body: Some("a fresh body".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(renamed["title"], "renamed via daemon");
    assert_eq!(client.show(id.clone()).unwrap()["body"], "a fresh body\n");

    // dep_remove then re-add (keeps later assertions stable).
    let undone = client.dep_remove(id.clone(), dep_id.clone()).unwrap();
    assert_eq!(undone["deps"], serde_json::json!([]));
    client.dep_add(id.clone(), dep_id.clone()).unwrap();

    // set_parent: make `id` a child of `dep`, then clear it.
    let parented = client.set_parent(id.clone(), Some(dep_id.clone())).unwrap();
    assert_eq!(parented["parent"], dep_id);
    assert!(client.set_parent(id.clone(), None).unwrap()["parent"].is_null());

    // comment + show reflect the accumulated state.
    client
        .add_comment(id.clone(), "me@example.com".to_owned(), "done".to_owned())
        .unwrap();
    let shown = client.show(id.clone()).unwrap();
    assert_eq!(shown["status"], "closed");
    assert_eq!(shown["comment_count"], 1);
    assert_eq!(shown["deps"], serde_json::json!([dep_id]));

    // stats sees both items.
    let stats = client.stats(10, true).unwrap();
    assert_eq!(stats["total"], 2);
}

#[test]
fn concurrent_daemon_writes_serialize() {
    // Regression (D-daemon-3): concurrent write RPCs to the daemon must not lose
    // updates. Each `dep_add` is a read-modify-write; without the store-wide
    // write lock (held across the whole window by `update_with`), parallel adds
    // would clobber each other and silently drop deps. Fire N concurrent adds of
    // distinct deps to one root and assert all survive.
    use clove_types::NewSpec;

    let (_tmp, clove_dir) = init_repo_with_items(0);
    let hub = TestHub::spawn(Some(&clove_dir));

    let mut client = hub.client(&clove_dir);
    let mk = |c: &mut DaemonClient, title: &str| -> String {
        c.create(NewSpec {
            title: title.to_owned(),
            ..Default::default()
        })
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let root = mk(&mut client, "root");
    let n = 6usize;
    let deps: Vec<String> = (0..n).map(|i| mk(&mut client, &format!("d{i}"))).collect();
    drop(client);

    let handles: Vec<_> = deps
        .into_iter()
        .map(|dep| {
            let cd = clove_dir.clone();
            let paths = hub.paths.clone();
            let root = root.clone();
            std::thread::spawn(move || {
                let mut c = DaemonClient::probe_at(&paths, &cd).expect("daemon alive");
                c.dep_add(root, dep).unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let mut client = hub.client(&clove_dir);
    let shown = client.show(root).unwrap();
    assert_eq!(
        shown["deps"].as_array().unwrap().len(),
        n,
        "every concurrent dep add must survive (writes serialize in the daemon)"
    );
}

#[test]
fn socket_and_runtime_dir_are_owner_only() {
    // Regression (D-daemon-SEC-1): the mutating control socket — every project's
    // write path now — and the runtime dir holding it must be owner-only, not
    // default-umask, on a shared machine.
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, clove_dir) = init_repo_with_items(1);
    let hub = TestHub::spawn(Some(&clove_dir));

    let mode = |path: &Utf8Path| {
        std::fs::metadata(path.as_std_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(
        mode(&hub.paths.sock()),
        0o600,
        "control socket must be 0600"
    );
    assert_eq!(mode(&hub.paths.pid()), 0o600, "pid file must be 0600");
    assert_eq!(
        mode(hub.paths.dir()) & 0o022,
        0,
        "runtime dir must not be group/other-writable"
    );
}

#[test]
fn stale_socket_recovery_is_fast() {
    let (_tmp, clove_dir) = init_repo_with_items(1);
    let mut hub = TestHub::spawn(Some(&clove_dir));
    // Hard-kill leaves a corpse socket + pid.
    hub.signal(SIGKILL);
    hub.wait_exit(Duration::from_secs(5)).expect("killed");
    assert!(hub.paths.sock().exists());

    // The next probe must fail fast (connect timeout + cleanup) and clean up
    // (DESIGN §8.3; M3-G04 measures the resulting `clove ls` < 200ms).
    let start = Instant::now();
    let client = DaemonClient::probe_at(&hub.paths, &clove_dir);
    let elapsed = start.elapsed();
    assert!(client.is_none(), "no live daemon");
    assert!(
        elapsed < Duration::from_millis(200),
        "stale-socket probe {elapsed:?} exceeds 200ms"
    );
    assert!(!hub.paths.sock().exists(), "stale sock cleaned");
    assert!(!hub.paths.pid().exists(), "stale pid cleaned");
}

/// The daemon serves graph queries over IPC — and, deliberately, **not** search.
///
/// Protocol v5 had a `search` RPC that ran the index's FTS5 query; v6 removed it
/// with the FTS table itself (read-path roadmap §6.1). There is nothing to call
/// here any more, which is the point: `clove search` scans files on every
/// surface, so a live daemon cannot change its answer.
#[test]
fn graph_over_ipc_and_no_search_rpc() {
    use clove_ipc::{GraphRequest, GraphResponse};
    let (_tmp, clove_dir) = init_repo_with_items(3);
    let hub = TestHub::spawn(Some(&clove_dir));
    let mut client = hub.client(&clove_dir);

    // GRAPH: no deps yet → no cycles, nothing blocked.
    match client.graph(GraphRequest::Cycles).unwrap() {
        GraphResponse::Cycles { cycles } => assert!(cycles.is_empty()),
        other => panic!("expected Cycles, got {other:?}"),
    }
    match client
        .graph(GraphRequest::Blocked {
            order: Default::default(),
        })
        .unwrap()
    {
        GraphResponse::Blocked { ids } => assert!(ids.is_empty(), "nothing blocked"),
        other => panic!("expected Blocked, got {other:?}"),
    }
}

/// A filter residue must not ship the whole match set over the wire.
///
/// `q` is the one filter SQL cannot express (SQLite case-folds ASCII only, where
/// `str::to_lowercase` is full Unicode), so `query_filtered` applies it in memory
/// — and therefore cannot push the `LIMIT` down, because slicing before the
/// residue removes rows returns too few. Locally that is right: it returns every
/// match and the caller windows. Across a socket it was not: `clove ls --q x
/// --limit 1` transferred the entire match set for one row (read-path roadmap §5,
/// left open by §2).
///
/// The fix is to window on the daemon side of the wire *after* the residue. The
/// observable is the frame: `rows` is capped at `offset + limit` while `total`
/// still reports every match, so nothing about the caller's answer changes.
#[test]
fn a_residue_does_not_ship_the_whole_match_set() {
    // 12 items, all titled "item N" — so `q: "item"` matches every one and a
    // truncation bug cannot hide behind a small fixture.
    let (_tmp, clove_dir) = init_repo_with_items(12);
    let hub = TestHub::spawn(Some(&clove_dir));
    let mut client = hub.client(&clove_dir);

    let residue =
        clove_core::view::Filters::parse_multi(&[], &[], &[], None, &[], Some("item")).unwrap();

    // Baseline: unwindowed, the residue path returns everything.
    let all = client
        .query_list(QueryRequest {
            kind: QueryKind::List,
            filters: residue.clone(),
            order: Default::default(),
            offset: 0,
            limit: None,
        })
        .unwrap();
    assert_eq!(all.total, 12, "the residue matches every item");
    assert_eq!(all.rows.len(), 12, "no window, so no truncation");

    // Windowed: only what the window can reach crosses the wire.
    for (offset, limit) in [(0, 1), (0, 3), (5, 2)] {
        let resp = client
            .query_list(QueryRequest {
                kind: QueryKind::List,
                filters: residue.clone(),
                order: Default::default(),
                offset,
                limit: Some(limit),
            })
            .unwrap();
        assert_eq!(
            resp.total, 12,
            "total stays the pre-window match count (offset={offset} limit={limit})"
        );
        assert_eq!(
            resp.rows.len(),
            offset + limit,
            "only `offset + limit` rows cross the wire (offset={offset} limit={limit})"
        );
        // …and they are the *right* rows: the caller skips `offset` locally, so
        // the frame must start at row 0 of the ordered match set.
        let ids: Vec<&str> = resp.rows.iter().map(|r| r.id.as_str()).collect();
        let want: Vec<&str> = all.rows[..offset + limit]
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, want, "offset={offset} limit={limit}");
    }
}
