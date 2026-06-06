//! Standalone file-watcher: turns `.clove/issues/` changes into **granular**
//! real-time events. Used by `clove serve` and the daemon's web server.
//!
//! On each debounced wake-up it recomputes the whole store's item snapshot and
//! diffs it against the last broadcast snapshot, emitting one `item.upserted` per
//! changed item and one `item.deleted` per removed item — plus a trailing `batch`
//! carrying a monotonic `seq` for client gap-detection. Diffing the *computed*
//! snapshot (not raw files) means a topology change that flips another item's
//! `ready`/`blocked_by` is pushed too, even though that item's file didn't change.
//!
//! Only `.clove/issues/` is watched, so `index.db*` churn never produces events
//! (the feedback-loop guard, DESIGN §8.5).

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde_json::Value;

use crate::dto::{frontmatter_value, GraphContext};
use crate::events::Event;
use crate::AppState;

/// Debounce window: coalesce a burst of file events into one diff.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Compute `id → item JSON` for the whole store (lean items + computed fields).
fn snapshot(state: &AppState) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    if let Ok((frontmatters, _errors)) = state.store.scan_frontmatter() {
        let ctx = GraphContext::build(&frontmatters);
        for fm in &frontmatters {
            map.insert(
                fm.id.to_string(),
                Value::Object(frontmatter_value(fm, &ctx)),
            );
        }
    }
    map
}

/// Start watching the repo's issues dir. The returned watcher must be kept alive
/// for the duration of the server; dropping it stops the watch.
pub fn spawn(state: AppState) -> Option<notify::RecommendedWatcher> {
    let issues_dir = state.issues_dir.clone();
    let (tx, rx) = mpsc::channel::<()>();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })
    .ok()?;
    watcher
        .watch(issues_dir.as_std_path(), RecursiveMode::Recursive)
        .ok()?;

    std::thread::spawn(move || {
        // Baseline so the first change diffs against the current state, not against
        // "nothing" (which would re-emit every item on the first edit).
        let mut last = snapshot(&state);

        loop {
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

            let current = snapshot(&state);
            let mut changed = Vec::new();
            let mut deleted = Vec::new();

            // Created or modified (any item whose computed JSON changed).
            for (id, value) in &current {
                if last.get(id) != Some(value) {
                    changed.push(id.clone());
                    let _ = state.events.send(Event::ItemUpserted {
                        id: id.clone(),
                        item: value.clone(),
                    });
                }
            }
            // Removed.
            for id in last.keys() {
                if !current.contains_key(id) {
                    deleted.push(id.clone());
                    let _ = state.events.send(Event::ItemDeleted { id: id.clone() });
                }
            }

            if !changed.is_empty() || !deleted.is_empty() {
                let seq = state.next_seq();
                let _ = state.events.send(Event::Batch {
                    changed,
                    deleted,
                    seq,
                });
            }
            last = current;
        }
    });

    Some(watcher)
}
