//! clove-web: the HTTP/WebSocket server and embedded SPA for the clove web UI.
//!
//! Shared by both binaries: the `clove serve` CLI subcommand (standalone) and the
//! `cloved` daemon (in-process). Reads are served from the file store + the
//! in-memory dependency graph (always correct — files are truth, DESIGN §4); the
//! SQLite index/daemon are accelerators that can be layered on later. Writes go
//! through `clove_core::ItemStore` (atomic rename + advisory lock), so web edits
//! are concurrency-safe with the CLI and re-enter the same watcher → push loop.

mod assets;
mod dto;
mod error;
mod events;
mod read;
mod watch;
mod write;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use axum::Router;
use camino::Utf8PathBuf;
use clove_core::ItemStore;
use tokio::sync::broadcast;

pub use error::ApiError;
pub use events::Event;

/// Maximum accepted request-body size (matches the item body cap, DESIGN §4).
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Broadcast backlog before slow WS clients are told to resync.
const EVENT_CHANNEL_CAPACITY: usize = 512;

/// Shared, cheaply-cloneable server state.
#[derive(Clone)]
pub struct AppState {
    /// The file store (the single write path).
    pub store: ItemStore,
    /// The `.clove/issues/` directory.
    pub issues_dir: Utf8PathBuf,
    /// The configured id prefix (for the create form / `/meta`).
    pub id_prefix: String,
    /// Serving mode label surfaced to clients: `"standalone"` or `"daemon"`.
    pub source: String,
    /// Whether a daemon is known to be running for this repo.
    pub daemon_running: bool,
    /// The real-time event fan-out channel.
    pub events: broadcast::Sender<Event>,
    seq: Arc<AtomicU64>,
}

impl AppState {
    /// Build server state for a discovered repository.
    pub fn new(
        store: ItemStore,
        issues_dir: Utf8PathBuf,
        id_prefix: String,
        source: impl Into<String>,
        daemon_running: bool,
    ) -> Self {
        let (events, _rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            store,
            issues_dir,
            id_prefix,
            source: source.into(),
            daemon_running,
            events,
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The current event sequence number (advances on every published batch).
    pub fn current_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Allocate the next sequence number for a published batch.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Build the axum router (API + WebSocket + embedded-SPA fallback).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/v1/items",
            get(read::list_items).post(write::create_item),
        )
        .route(
            "/api/v1/items/:id",
            get(read::get_item)
                .patch(write::patch_item)
                .delete(write::delete_item),
        )
        .route(
            "/api/v1/items/:id/comments",
            get(read::get_comments).post(write::add_comment),
        )
        .route("/api/v1/items/:id/deptree", get(read::get_deptree))
        .route("/api/v1/items/:id/labels", put(write::put_labels))
        .route("/api/v1/items/:id/deps", post(write::add_dep))
        .route("/api/v1/items/:id/deps/:dep", delete(write::remove_dep))
        .route("/api/v1/board", get(read::get_board))
        .route("/api/v1/stats", get(read::get_stats))
        .route("/api/v1/stats/history", get(read::get_stats_history))
        .route("/api/v1/meta", get(read::get_meta))
        .route("/api/v1/cycles", get(read::get_cycles))
        .route("/api/v1/events", get(events::ws_handler))
        .fallback(assets::static_handler)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Serve the web UI on `addr` until the process is terminated.
pub async fn serve(state: AppState, addr: SocketAddr) -> std::io::Result<()> {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

/// Serve the web UI with a standalone file-watcher that pushes real-time updates.
pub async fn serve_with_watch(state: AppState, addr: SocketAddr) -> std::io::Result<()> {
    let _watcher = watch::spawn(state.clone());
    serve(state, addr).await
}
