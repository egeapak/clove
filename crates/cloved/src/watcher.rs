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
    let mut pending: HashSet<PathBuf> = HashSet::new();
    while let Some(first) = rx.recv().await {
        pending.insert(first);
        // Keep draining until the channel is quiet for `debounce`.
        loop {
            match tokio::time::timeout(debounce, rx.recv()).await {
                Ok(Some(path)) => {
                    pending.insert(path);
                }
                Ok(None) => break, // sender dropped → daemon shutting down
                Err(_) => break,   // quiet window elapsed → apply the batch
            }
        }
        let batch: Vec<Utf8PathBuf> = pending
            .drain()
            .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
            .collect();

        // Apply one index batch (one transaction) and record it.
        sync_once(&issues_dir, &index, &state);
        // The files changed → the cached dependency graph is now stale.
        graph.mark_dirty();
        if let Ok(mut st) = state.lock() {
            st.mark_event();
            st.inc_batches();
        }

        // Opt-in git auto-sync of the changed files (T-D06).
        maybe_git_sync(&options, batch, &index);
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
