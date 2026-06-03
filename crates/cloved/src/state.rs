//! Shared daemon runtime state (DESIGN §8.4 `STATUS`).
//!
//! This is the daemon's *operational* telemetry — uptime, indexed-item count,
//! watcher state, last-event recency — surfaced by the `STATUS` IPC command and
//! `clove daemon status`. It is not work-item analytics (that is the deferred M4
//! `clove stats`, M3_PLAN §1.1). Shared across the accept loop and (from P3) the
//! watcher behind an `Arc<Mutex<_>>`.

use std::time::Instant;

use clove_ipc::StatusResponse;

/// What the file watcher is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Sweeping/Watching are set by the watcher in Phase 3 (T-D04)
pub enum WatcherState {
    /// Performing the startup mtime sweep (P3), before the daemon is ready.
    Sweeping,
    /// Watching for changes (the steady state once ready).
    Watching,
    /// No watcher running (e.g. watcher disabled or not yet started).
    Idle,
}

impl WatcherState {
    /// The wire string used in the `STATUS` payload.
    fn as_str(self) -> &'static str {
        match self {
            WatcherState::Sweeping => "sweeping",
            WatcherState::Watching => "watching",
            WatcherState::Idle => "idle",
        }
    }
}

/// Mutable daemon state shared between tasks.
#[derive(Debug)]
pub struct DaemonState {
    started_at: Instant,
    items_indexed: u64,
    watcher_state: WatcherState,
    last_event_at: Option<Instant>,
}

impl DaemonState {
    /// New state at daemon start, with the initial indexed-item count.
    pub fn new(items_indexed: u64) -> DaemonState {
        DaemonState {
            started_at: Instant::now(),
            items_indexed,
            watcher_state: WatcherState::Idle,
            last_event_at: None,
        }
    }

    /// Update the indexed-item count (after a sweep/watch batch).
    #[allow(dead_code)] // called by the watcher in Phase 3 (T-D04)
    pub fn set_items_indexed(&mut self, n: u64) {
        self.items_indexed = n;
    }

    /// Set the watcher state.
    #[allow(dead_code)] // called by the watcher in Phase 3 (T-D04)
    pub fn set_watcher_state(&mut self, state: WatcherState) {
        self.watcher_state = state;
    }

    /// Record that a watcher/IPC event just happened (for `last_event_ms`).
    pub fn mark_event(&mut self) {
        self.last_event_at = Some(Instant::now());
    }

    /// Build the `STATUS` IPC payload from the current state.
    pub fn snapshot(&self) -> StatusResponse {
        StatusResponse {
            uptime_s: self.started_at.elapsed().as_secs(),
            items_indexed: self.items_indexed,
            watcher_state: self.watcher_state.as_str().to_owned(),
            last_event_ms: self.last_event_at.map(|t| t.elapsed().as_millis() as u64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reflects_state() {
        let mut s = DaemonState::new(7);
        assert_eq!(s.snapshot().items_indexed, 7);
        assert_eq!(s.snapshot().watcher_state, "idle");
        assert_eq!(s.snapshot().last_event_ms, None);

        s.set_watcher_state(WatcherState::Watching);
        s.set_items_indexed(9);
        s.mark_event();
        let snap = s.snapshot();
        assert_eq!(snap.items_indexed, 9);
        assert_eq!(snap.watcher_state, "watching");
        assert!(snap.last_event_ms.is_some());
    }
}
