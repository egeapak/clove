//! The index sync shared by a project's load and its watcher (T-D04): bring
//! the index up to date with the files in one transaction, then refresh the
//! daemon's item count.

use std::sync::{Arc, Mutex};

use clove_index::Index;

use crate::fresh::{Freshness, Sync};
use crate::state::DaemonState;

/// The sweep of a project's load (DESIGN §8.6), run once before the watch is
/// armed and once after: the directory-level check, and the per-file pass
/// only if the directory moved. Returns `true` if any change was applied.
pub fn startup_sweep(
    fresh: &Freshness,
    index: &Arc<Mutex<Index>>,
    state: &Arc<Mutex<DaemonState>>,
) -> bool {
    let Ok(mut idx) = index.lock() else {
        return false;
    };
    let applied = fresh.startup_sweep(&mut idx);
    let count = idx.item_count().unwrap_or(0) as u64;
    drop(idx);
    if let Ok(mut st) = state.lock() {
        st.set_items_indexed(count);
    }
    applied
}

/// One watcher batch (DESIGN §8.5): apply everything the watch has reported,
/// with no limit on the number of changed items.
pub fn sync_batch(
    fresh: &Freshness,
    index: &Arc<Mutex<Index>>,
    state: &Arc<Mutex<DaemonState>>,
) -> Sync {
    let Ok(mut idx) = index.lock() else {
        return Sync::Failed;
    };
    let outcome = fresh.sync(&mut idx, None);
    if let Sync::Fresh { changed: true } = outcome {
        let count = idx.item_count().unwrap_or(0) as u64;
        drop(idx);
        if let Ok(mut st) = state.lock() {
            st.set_items_indexed(count);
        }
    }
    outcome
}
