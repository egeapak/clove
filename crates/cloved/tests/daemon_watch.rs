//! Phase 3 (T-D04) watcher tests: feedback-loop prevention (M3-G05), debounce
//! batching (M3-G06), watcher reflects new/edited/deleted items, and the startup
//! sweep picks up out-of-band changes. Unix-only (drives real signals).
#![cfg(unix)]

mod support;

use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use clove_core::{ItemStore, NewItem};
use clove_ipc::{DaemonClient, QueryKind, QueryRequest};
use clove_types::{ItemType, Priority};
use support::TestHub;

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

fn list_all() -> QueryRequest {
    QueryRequest {
        kind: QueryKind::List,
        filters: Default::default(),
        order: Default::default(),
        offset: 0,
        limit: None,
    }
}

fn count_via_daemon(hub: &TestHub, clove_dir: &Utf8Path) -> usize {
    let mut client: DaemonClient = hub.client(clove_dir);
    client.query_list(list_all()).unwrap().rows.len()
}

fn batches(hub: &TestHub, clove_dir: &Utf8Path) -> u64 {
    hub.client(clove_dir).status().unwrap().batches_applied
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

/// M4: a running daemon records `clove stats` history points on its interval.
#[test]
fn daemon_auto_snapshots_on_interval() {
    let repo = init_repo();
    repo.add_item("tracked work");
    repo.reindex();

    // Snapshot every 150ms; never idle-shut-down during the test.
    let hub = TestHub::spawn_with(
        Some(&repo.clove_dir),
        &[
            ("CLOVED_DISABLE_WEB", "1"),
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

    drop(hub);

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
    let hub = TestHub::spawn(Some(&repo.clove_dir));
    assert_eq!(
        count_via_daemon(&hub, &repo.clove_dir),
        1,
        "startup sweep indexed it"
    );
    drop(hub);
}

/// An item written the moment the project's load returns is indexed, however
/// slow the watcher is to arm: the load returns before the watch is in place,
/// and the sweep run once it is picks up what was written in between — it is
/// not lost until the next change.
#[test]
fn an_item_written_right_after_the_load_is_indexed() {
    let repo = init_repo();
    repo.reindex();
    // Only the watcher may bring the item in: no refresh on read.
    std::fs::write(
        repo.clove_dir.join("config.toml"),
        "config_schema = 1\nid_prefix = \"proj\"\n[index]\nauto_refresh = false\n",
    )
    .unwrap();
    let hub = TestHub::spawn_with(
        None,
        &[
            ("CLOVED_DISABLE_WEB", "1"),
            ("CLOVED_WATCH_ARM_DELAY_MS", "1500"),
        ],
    );
    let mut client = hub
        .load_arming(&repo.clove_dir)
        .expect("the hub serves the project");
    repo.add_item("written as the load returned");
    let ok = wait_until(Duration::from_secs(20), || {
        client
            .query_list(list_all())
            .is_ok_and(|page| page.rows.len() == 1)
    });
    assert!(
        ok,
        "the item written right after the load was never indexed"
    );
    drop(hub);
}

/// Until its watcher watches, the daemon does not answer reads from an index
/// that may be stale: it says so (`WATCHER_ARMING`), and the client reads the
/// index or the files itself. Writes are served throughout, and what was
/// written is served once the watcher is armed.
#[test]
fn reads_wait_for_the_watcher_but_writes_do_not() {
    let repo = init_repo();
    repo.reindex();
    let hub = TestHub::spawn_with(
        None,
        &[
            ("CLOVED_DISABLE_WEB", "1"),
            ("CLOVED_WATCH_ARM_DELAY_MS", "3000"),
        ],
    );
    let mut client = hub
        .load_arming(&repo.clove_dir)
        .expect("the hub serves the project");
    assert_eq!(client.status().unwrap().watcher_state, "arming");
    match client.query_list(list_all()) {
        Err(clove_ipc::ClientError::App(e)) => assert_eq!(e.code, "WATCHER_ARMING", "{e:?}"),
        other => panic!("a read while arming was answered: {other:?}"),
    }
    client
        .create(clove_types::NewSpec {
            title: "written while arming".to_owned(),
            ..Default::default()
        })
        .expect("a write while arming is served");
    let ok = wait_until(Duration::from_secs(20), || {
        client
            .query_list(list_all())
            .is_ok_and(|page| page.rows.len() == 1)
    });
    assert!(ok, "the write was never served once the watcher armed");
    assert_eq!(client.status().unwrap().watcher_state, "watching");
    drop(hub);
}

#[test]
fn watcher_reflects_new_item() {
    let repo = init_repo();
    repo.add_item("first");
    repo.reindex();
    let hub = TestHub::spawn(Some(&repo.clove_dir));
    assert_eq!(count_via_daemon(&hub, &repo.clove_dir), 1);

    // Add an item out-of-band; the watcher must pick it up.
    repo.add_item("second");
    let ok = wait_until(Duration::from_secs(3), || {
        count_via_daemon(&hub, &repo.clove_dir) == 2
    });
    assert!(ok, "watcher did not index the new item");

    drop(hub);
}

#[test]
fn reindex_does_not_trigger_watcher_batches() {
    // M3-G05: writing index.db (via `reindex`) must produce zero watcher batches,
    // because index.db lives outside the watched issues/ dir.
    let repo = init_repo();
    repo.add_item("one");
    repo.reindex();
    let hub = TestHub::spawn(Some(&repo.clove_dir));
    let before = batches(&hub, &repo.clove_dir);

    // Rebuild the index repeatedly — only touches .clove/index.db*.
    for _ in 0..3 {
        repo.reindex();
    }
    std::thread::sleep(Duration::from_millis(600));
    let after = batches(&hub, &repo.clove_dir);
    assert_eq!(
        after, before,
        "index.db writes must not be watched (feedback loop)"
    );

    drop(hub);
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
    let hub = TestHub::spawn(Some(&repo.clove_dir)); // returns once the pid (readiness) appears
    let ready = start.elapsed();
    assert!(
        ready < Duration::from_millis(500),
        "startup sweep + ready took {ready:?} (gate: < 500ms)"
    );
    assert_eq!(count_via_daemon(&hub, &repo.clove_dir), 1000);

    drop(hub);
}

#[test]
fn rapid_edits_debounce_into_fewer_batches_than_edits() {
    // M3-G06, end-to-end: a burst of edits to one file reaches the index as a
    // coalesced batch rather than one batch per write.
    //
    // **This test deliberately does not assert an exact batch count.** It used to
    // assert exactly one, and that made it flaky: the assertion held only while
    // every inter-write gap stayed under the debounce window, which the OS does
    // not guarantee. Under parallel load a 10ms sleep can stretch past the
    // window, the burst flushes early, and a correct implementation fails the
    // test. (Observed: `left: 2, right: 1`.)
    //
    // The exact rule — N events inside the window become exactly one batch, an
    // event after it starts a new one — is asserted deterministically against a
    // virtual clock by the `collect_burst` unit tests in `cloved::watcher`. What
    // is worth testing *here*, and only here, is the wiring: that real
    // filesystem events reach the debouncer and get coalesced at all. That is
    // expressible without depending on timing.
    let repo = init_repo();
    let id = repo.add_item("debounced");
    repo.reindex();

    // A generous quiet window, so the burst below is comfortably inside it even
    // on a loaded machine. This is belt-and-braces: the assertion no longer
    // depends on it.
    let config = repo.clove_dir.join("config.toml");
    let base_config = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        format!("{base_config}[daemon]\nwatch_debounce_ms = 1000\n"),
    )
    .unwrap();

    let hub = TestHub::spawn(Some(&repo.clove_dir));
    let before = batches(&hub, &repo.clove_dir);

    const EDITS: u64 = 10;
    let path = repo.clove_dir.join("issues").join(format!("{id}.md"));
    let base = std::fs::read_to_string(&path).unwrap();
    for i in 0..EDITS {
        // Append a line (keeps frontmatter valid), back-to-back: the burst is the
        // point, and sleeping between writes is what created the original race.
        std::fs::write(&path, format!("{base}\nedit {i}\n")).unwrap();
    }

    // Wait for the batch to land, then let any straggler batch land too, so the
    // count below cannot be read mid-burst.
    let ok = wait_until(Duration::from_secs(30), || {
        batches(&hub, &repo.clove_dir) > before
    });
    assert!(ok, "debounced batch never applied");
    std::thread::sleep(Duration::from_millis(2500));

    let delta = batches(&hub, &repo.clove_dir) - before;
    assert!(
        delta >= 1,
        "the edits must reach the index (got {delta} batches)"
    );
    assert!(
        delta < EDITS,
        "edits must coalesce: {delta} batches for {EDITS} rapid writes is no \
         coalescing at all"
    );

    drop(hub);
}
