//! Shared daemon runtime state (DESIGN §8.4 `STATUS`).
//!
//! This is the daemon's *operational* telemetry — uptime, indexed-item count,
//! watcher state, last-event recency — surfaced by the `STATUS` IPC command and
//! `clove daemon status`. It is not work-item analytics (that is the deferred M4
//! `clove stats`). Shared across the accept loop and the
//! watcher behind an `Arc<Mutex<_>>`.

use std::time::Instant;

use clove_ipc::StatusResponse;

/// What the file watcher is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    batches_applied: u64,
    ping_count: u64,
    last_ping_at: Option<Instant>,
    web_addr: Option<String>,
}

impl DaemonState {
    /// New state at daemon start, with the initial indexed-item count.
    pub fn new(items_indexed: u64) -> DaemonState {
        DaemonState {
            started_at: Instant::now(),
            items_indexed,
            watcher_state: WatcherState::Idle,
            last_event_at: None,
            batches_applied: 0,
            ping_count: 0,
            last_ping_at: None,
            web_addr: None,
        }
    }

    /// Record the address the daemon is serving the web UI on (M4).
    pub fn set_web_addr(&mut self, addr: Option<String>) {
        self.web_addr = addr;
    }

    /// Update the indexed-item count (after a sweep/watch batch).
    pub fn set_items_indexed(&mut self, n: u64) {
        self.items_indexed = n;
    }

    /// Set the watcher state.
    pub fn set_watcher_state(&mut self, state: WatcherState) {
        self.watcher_state = state;
    }

    /// Record that the watcher applied one debounced batch (the M3-G05/G06
    /// observable: feedback-loop prevention and debounce coalescing).
    pub fn inc_batches(&mut self) {
        self.batches_applied += 1;
    }

    /// How long since the last watcher/IPC activity (or since startup if none).
    /// Drives idle self-shutdown (DESIGN §8.8 `idle_shutdown_min`).
    pub fn idle_for(&self) -> std::time::Duration {
        self.last_event_at.unwrap_or(self.started_at).elapsed()
    }

    /// Record that a watcher/IPC event just happened (for `last_event_ms`).
    pub fn mark_event(&mut self) {
        self.last_event_at = Some(Instant::now());
    }

    /// Record a served `ping`: bump the counter, stamp the time, and count it as
    /// activity (so a client/MCP heartbeat resets the idle-shutdown window).
    pub fn record_ping(&mut self) {
        self.ping_count += 1;
        let now = Instant::now();
        self.last_ping_at = Some(now);
        self.last_event_at = Some(now);
    }

    /// Build the `STATUS` IPC payload from the current state.
    pub fn snapshot(&self) -> StatusResponse {
        StatusResponse {
            uptime_s: self.started_at.elapsed().as_secs(),
            items_indexed: self.items_indexed,
            watcher_state: self.watcher_state.as_str().to_owned(),
            last_event_ms: self.last_event_at.map(|t| t.elapsed().as_millis() as u64),
            batches_applied: self.batches_applied,
            ping_count: self.ping_count,
            last_ping_ms: self.last_ping_at.map(|t| t.elapsed().as_millis() as u64),
            web_addr: self.web_addr.clone(),
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

    #[test]
    fn record_ping_counts_and_marks_activity() {
        let mut s = DaemonState::new(0);
        assert_eq!(s.snapshot().ping_count, 0);
        assert_eq!(s.snapshot().last_ping_ms, None);
        // A ping bumps the counter, stamps last-ping, and counts as activity.
        s.record_ping();
        s.record_ping();
        let snap = s.snapshot();
        assert_eq!(snap.ping_count, 2);
        assert!(snap.last_ping_ms.is_some(), "last ping stamped");
        assert!(snap.last_event_ms.is_some(), "ping resets idle window");
    }
}
