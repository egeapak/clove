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

use crate::reindexer::sync_once;
use crate::state::{DaemonState, WatcherState};

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
        pending.clear();

        // Apply one batch (one transaction) and record it.
        sync_once(&issues_dir, &index, &state);
        if let Ok(mut st) = state.lock() {
            st.mark_event();
            st.inc_batches();
        }
    }

    drop(watcher);
}
