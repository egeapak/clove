//! File watcher with debounced batching (T-D04, DESIGN §8.5).
//!
//! `notify` runs the OS watch on its own thread and forwards `*.md` paths into a
//! Tokio channel. The async debounce loop coalesces a burst of events into a
//! single re-sync: it collects paths until the channel is quiet for the debounce
//! window, then applies **one** batch (one SQLite transaction). Each applied
//! batch bumps `DaemonState::batches_applied` — the M3-G05/G06 observable.
//!
//! **Feedback-loop prevention (M3-G05):** the watch is rooted at
//! `.clove/issues/`, so `.clove/index.db*` (a sibling, not under issues) is never
//! seen; the `*.md` filter is a second guard.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::Utf8PathBuf;
use clove_index::Index;
use notify::{recommended_watcher, Event, RecursiveMode, Watcher};

use crate::graph_cache::GraphCache;
use crate::reindexer::sync_once;
use crate::state::{DaemonState, WatcherState};

/// Per-batch options that depend on repo config (the git-sync opt-in).
#[derive(Clone)]
// Read only by the `git-sync` build; a lean build keeps them for a uniform API.
#[cfg_attr(not(feature = "git-sync"), allow(dead_code))]
pub struct WatchOptions {
    /// Repository root (parent of `.clove/`), for git-sync.
    pub repo_root: Utf8PathBuf,
    /// `[daemon] git_sync` — auto-commit clean edits (T-D06).
    pub git_sync: bool,
}

/// Only `*.md` files under the issues dir are item files; everything else
/// (including any stray `index.db*`) is ignored to prevent feedback loops.
fn is_item_file(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("md")
}

/// Collect one debounced burst: take `first`, then keep draining `rx` until it
/// has stayed quiet for `debounce`. Returns the coalesced (deduplicated) path
/// set that becomes exactly one applied batch.
///
/// Split out of [`watch`] so the coalescing rule can be tested against a
/// **virtual** clock. The end-to-end watcher test can only observe batches by
/// writing files and waiting, which makes it a race between the OS scheduler and
/// the quiet window: under load a 10ms gap between writes can stretch past the
/// window, the burst flushes early, and the "exactly one batch" assertion fails
/// even though the logic is correct. The unit tests below drive this function
/// with `tokio::time` paused, so they assert the rule itself and cannot flake.
async fn collect_burst(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PathBuf>,
    first: PathBuf,
    debounce: Duration,
) -> HashSet<PathBuf> {
    let mut pending: HashSet<PathBuf> = HashSet::new();
    pending.insert(first);
    loop {
        match tokio::time::timeout(debounce, rx.recv()).await {
            Ok(Some(path)) => {
                pending.insert(path);
            }
            Ok(None) => break, // sender dropped → daemon shutting down
            Err(_) => break,   // quiet window elapsed → apply the batch
        }
    }
    pending
}

/// Watch `issues_dir` and keep the index fresh until the task is dropped (on
/// shutdown). `debounce` is the per-burst quiet window (DESIGN §8.5).
pub async fn watch(
    issues_dir: Utf8PathBuf,
    index: Arc<Mutex<Index>>,
    state: Arc<Mutex<DaemonState>>,
    debounce: Duration,
    options: WatchOptions,
    graph: Arc<GraphCache>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();

    // The notify handler runs on notify's own thread; forward only item-file
    // paths into the channel (non-blocking send, no runtime needed here).
    let mut watcher = match recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                if is_item_file(&path) {
                    let _ = tx.send(path);
                }
            }
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cloved: watcher init failed: {e}");
            return;
        }
    };

    if let Err(e) = watcher.watch(issues_dir.as_std_path(), RecursiveMode::Recursive) {
        eprintln!("cloved: watch({issues_dir}) failed: {e}");
        return;
    }
    if let Ok(mut st) = state.lock() {
        st.set_watcher_state(WatcherState::Watching);
    }

    // Debounce loop: collect a burst, then apply exactly one batch.
    while let Some(first) = rx.recv().await {
        let batch: Vec<Utf8PathBuf> = collect_burst(&mut rx, first, debounce)
            .await
            .into_iter()
            .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
            .collect();

        // Apply one index batch (one transaction) and record it — on the
        // blocking pool, like the IPC handlers: `sync_once` (SQLite work while
        // holding the index mutex) and the git sync (libgit2 I/O, one commit
        // per file) can take seconds on a big batch (e.g. after a `git pull`),
        // and running them inline would park one of the daemon's two runtime
        // workers, starving concurrent `ping`s past the client's 50ms budget
        // exactly when the daemon is most needed.
        let issues_dir_b = issues_dir.clone();
        let index_b = index.clone();
        let state_b = state.clone();
        let graph_b = graph.clone();
        let options_b = options.clone();
        let done = tokio::task::spawn_blocking(move || {
            sync_once(&issues_dir_b, &index_b, &state_b);
            // The files changed → the cached dependency graph is now stale.
            graph_b.mark_dirty();
            if let Ok(mut st) = state_b.lock() {
                st.mark_event();
                st.inc_batches();
            }

            // Opt-in git auto-sync of the changed files (T-D06).
            maybe_git_sync(&options_b, batch, &index_b);
        })
        .await;
        if done.is_err() {
            eprintln!("cloved: watcher batch task panicked");
        }
    }

    drop(watcher);
}

/// Auto-commit the batch's files when built with `git-sync` and enabled in config.
#[cfg(feature = "git-sync")]
fn maybe_git_sync(options: &WatchOptions, paths: Vec<Utf8PathBuf>, index: &Arc<Mutex<Index>>) {
    if options.git_sync && !paths.is_empty() {
        crate::git_sync::sync_files(&options.repo_root, &paths, index);
    }
}

/// No-op when the `git-sync` feature is disabled.
#[cfg(not(feature = "git-sync"))]
fn maybe_git_sync(_options: &WatchOptions, _paths: Vec<Utf8PathBuf>, _index: &Arc<Mutex<Index>>) {}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: Duration = Duration::from_millis(200);

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/issues/{name}.md"))
    }

    /// Drive [`collect_burst`] once over an already-queued set of events.
    ///
    /// Every test here runs with `start_paused = true`, so `tokio::time` uses a
    /// virtual clock: the debounce timeout fires because the runtime is idle and
    /// auto-advances to the next deadline, not because any real time passed.
    /// These tests are therefore instant and deterministic — no sleeps, no
    /// dependence on the scheduler.
    async fn burst_of(events: &[&str], debounce: Duration) -> HashSet<PathBuf> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        for name in events {
            tx.send(path(name)).unwrap();
        }
        // Keep the sender alive so the burst ends on the quiet window, not on a
        // closed channel — that is the path the debounce rule is about.
        let first = rx.recv().await.expect("at least one event");
        let out = collect_burst(&mut rx, first, debounce).await;
        drop(tx);
        out
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_edits_coalesces_into_one_batch() {
        // The M3-G06 rule: many events arriving inside the quiet window become a
        // single batch. This is what the end-to-end test asserts by writing files
        // and counting applied batches; here it is asserted directly, so a slow
        // machine cannot turn a correct implementation into a failure.
        let names: Vec<String> = (0..10).map(|i| format!("item{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let burst = burst_of(&refs, DEBOUNCE).await;
        assert_eq!(burst.len(), 10, "one burst must carry every queued edit");
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_edits_to_one_file_collapse_to_a_single_path() {
        // Ten writes to the same file is the exact shape of the e2e test: the
        // batch is a path *set*, so the file is re-synced once, not ten times.
        let burst = burst_of(&["same"; 10], DEBOUNCE).await;
        assert_eq!(burst.len(), 1);
        assert!(burst.contains(&path("same")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_gap_longer_than_the_window_starts_a_new_burst() {
        // The other half of the rule: the window is what separates batches, so an
        // event arriving after it closes belongs to the next one. Draining an
        // empty channel makes the timeout fire immediately on the virtual clock.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();

        tx.send(path("first")).unwrap();
        let first = rx.recv().await.unwrap();
        let one = collect_burst(&mut rx, first, DEBOUNCE).await;
        assert_eq!(one, HashSet::from([path("first")]));

        tx.send(path("second")).unwrap();
        let next = rx.recv().await.unwrap();
        let two = collect_burst(&mut rx, next, DEBOUNCE).await;
        assert_eq!(
            two,
            HashSet::from([path("second")]),
            "an event after the quiet window is a separate batch"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_ends_the_burst_without_waiting() {
        // Daemon shutdown drops the sender mid-burst; the collected work must
        // still be returned rather than discarded or blocked on the window.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        tx.send(path("a")).unwrap();
        tx.send(path("b")).unwrap();
        let first = rx.recv().await.unwrap();
        drop(tx);

        let burst = collect_burst(&mut rx, first, DEBOUNCE).await;
        assert_eq!(burst, HashSet::from([path("a"), path("b")]));
    }

    #[tokio::test(start_paused = true)]
    async fn the_window_length_does_not_change_the_outcome_for_a_queued_burst() {
        // Coalescing depends on the *gaps*, not on the window's magnitude, so a
        // queued burst behaves identically at 1ms and at an hour. This is the
        // property that makes the virtual clock sound here.
        for debounce in [
            Duration::from_millis(1),
            Duration::from_millis(200),
            Duration::from_secs(3600),
        ] {
            let burst = burst_of(&["x", "y", "z"], debounce).await;
            assert_eq!(burst.len(), 3, "debounce={debounce:?}");
        }
    }
}
