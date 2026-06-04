//! Shared incremental-apply helper used by the startup sweep and the watcher
//! (T-D04). Both reduce to: run the fast staleness check, and if anything
//! changed, apply it in one transaction (the encapsulated `clove_index`
//! write path), then refresh the daemon's item count.

use std::sync::{Arc, Mutex};

use camino::Utf8Path;
use clove_index::Index;

use crate::state::DaemonState;

/// Re-sync the index with the files under `issues_dir` in a single pass.
/// Returns `true` if any change was applied. Used for both the startup mtime
/// sweep (DESIGN §8.6) and each debounced watcher batch (DESIGN §8.5).
pub fn sync_once(
    issues_dir: &Utf8Path,
    index: &Arc<Mutex<Index>>,
    state: &Arc<Mutex<DaemonState>>,
) -> bool {
    let mut applied = false;
    let count = {
        let mut idx = match index.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if let Ok(report) = idx.check_staleness_fast(issues_dir) {
            if !report.is_clean() {
                let _ = idx.apply_staleness(&report, issues_dir);
                applied = true;
            }
        }
        idx.item_count().unwrap_or(0) as u64
    };
    if let Ok(mut st) = state.lock() {
        st.set_items_indexed(count);
    }
    applied
}
