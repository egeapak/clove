//! Standalone file-watcher: turns `.clove/issues/` changes into debounced
//! real-time `batch` events. Used by `clove serve` and the daemon's web server.
//!
//! On any debounced change it emits one `batch{seq}` event and the SPA refetches
//! the (lean) item list — a full rescan of even 10k items is sub-second, so a
//! per-id diff is unnecessary complexity. A monotonic `seq` still lets a
//! reconnecting client detect a missed batch.
//!
//! Only `.clove/issues/` is watched, so `index.db*` churn never produces events
//! (the feedback-loop guard, DESIGN §8.5). A change from any source — the web
//! UI's own writes, the CLI, an editor, or `git pull` — flows through here.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::events::Event;
use crate::AppState;

/// Debounce window: coalesce a burst of file events into one batch.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Start watching the repo's issues dir. The returned watcher must be kept alive
/// for the duration of the server; dropping it stops the watch.
pub fn spawn(state: AppState) -> Option<notify::RecommendedWatcher> {
    let issues_dir = state.issues_dir.clone();
    let (tx, rx) = mpsc::channel::<()>();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            // Coalesced downstream; we only need to know *something* changed.
            let _ = tx.send(());
        }
    })
    .ok()?;
    watcher
        .watch(issues_dir.as_std_path(), RecursiveMode::Recursive)
        .ok()?;

    std::thread::spawn(move || loop {
        // Block until the first event of a burst.
        if rx.recv().is_err() {
            break;
        }
        // Drain until the directory is quiet for the debounce window.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let seq = state.next_seq();
        let _ = state.events.send(Event::Batch {
            changed: Vec::new(),
            deleted: Vec::new(),
            seq,
        });
    });

    Some(watcher)
}
