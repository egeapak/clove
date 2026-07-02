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
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::Router;
use camino::Utf8PathBuf;
use clove_core::ItemStore;
use clove_types::ItemType;
use tokio::sync::broadcast;
use tower_http::compression::{CompressionLayer, CompressionLevel};

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
    /// The configured default item type, honored by `POST /items` when the
    /// request omits a type (matches every other surface's `config.default_type`).
    pub default_type: ItemType,
    /// Serving mode label surfaced to clients: `"standalone"` or `"daemon"`.
    pub source: String,
    /// Whether a daemon is known to be running for this repo.
    pub daemon_running: bool,
    /// The real-time event fan-out channel.
    pub events: broadcast::Sender<Event>,
    seq: Arc<AtomicU64>,
    /// Optional per-request hook (the daemon uses it to reset idle-shutdown).
    heartbeat: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl AppState {
    /// Build server state for a discovered repository.
    pub fn new(
        store: ItemStore,
        issues_dir: Utf8PathBuf,
        id_prefix: String,
        source: impl Into<String>,
        daemon_running: bool,
        default_type: ItemType,
    ) -> Self {
        let (events, _rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            store,
            issues_dir,
            id_prefix,
            default_type,
            source: source.into(),
            daemon_running,
            events,
            seq: Arc::new(AtomicU64::new(0)),
            heartbeat: None,
        }
    }

    /// Attach a per-request hook (the daemon passes one that resets its
    /// idle-shutdown timer so an actively-used web session keeps it alive).
    pub fn with_heartbeat(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.heartbeat = Some(hook);
        self
    }

    /// Invoke the heartbeat hook, if any.
    fn beat(&self) {
        if let Some(hook) = &self.heartbeat {
            hook();
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

/// Whether an HTTP `Host`/authority (or `Origin` authority) names the loopback
/// interface. Accepts an optional port and bracketed IPv6. This is the guard
/// against DNS-rebinding: a rebound hostname (e.g. `evil.com`) never matches.
pub(crate) fn host_is_local(host: &str) -> bool {
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6 literal: take up to the closing ']'.
        rest.split_once(']').map(|(h, _)| h).unwrap_or(rest)
    } else {
        // `host` or `host:port` — strip a trailing `:port` if present.
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    };
    matches!(hostname, "localhost" | "127.0.0.1" | "::1")
}

/// Middleware rejecting requests whose `Host` header is not loopback. Loopback
/// binding alone doesn't stop a malicious page from using DNS rebinding to reach
/// `127.0.0.1:<port>` under an attacker-controlled name; validating `Host` does.
/// An absent `Host` (HTTP/2 uses `:authority`; some non-browser clients) is
/// allowed — browsers always send a `Host`, which is the rebinding vector.
async fn host_guard(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let ok = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(host_is_local)
        .unwrap_or(true);
    if !ok {
        return (StatusCode::FORBIDDEN, "forbidden: non-local Host header").into_response();
    }
    next.run(request).await
}

/// Middleware that fires the per-request heartbeat hook before handling.
async fn heartbeat_layer(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    state.beat();
    next.run(request).await
}

/// Build the axum router (API + WebSocket + embedded-SPA fallback).
pub fn build_router(state: AppState) -> Router {
    // Decompress the embedded gzip assets into memory once, up front.
    assets::warm();
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
        .route("/api/v1/items/:id/parent", put(write::put_parent))
        .route("/api/v1/items/:id/deps", post(write::add_dep))
        .route("/api/v1/items/:id/deps/:dep", delete(write::remove_dep))
        .route("/api/v1/board", get(read::get_board))
        .route("/api/v1/stats", get(read::get_stats))
        .route("/api/v1/stats/history", get(read::get_stats_history))
        .route("/api/v1/meta", get(read::get_meta))
        .route("/api/v1/cycles", get(read::get_cycles))
        .route("/api/v1/events", get(events::ws_handler))
        .fallback(assets::static_handler)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            heartbeat_layer,
        ))
        // DNS-rebinding guard: reject any request (API + WS + assets) whose Host
        // header isn't loopback. Outermost so it runs before everything else.
        .layer(axum::middleware::from_fn(host_guard))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        // gzip-only compression for the *dynamic* API responses (e.g. /items at
        // 10k items). Static SPA assets are already served pre-gzipped from
        // memory with their own Content-Encoding, so this layer skips them. No
        // brotli/zstd library is linked (gzip via pure-Rust flate2).
        .layer(CompressionLayer::new().quality(CompressionLevel::Best))
        .with_state(state)
}

/// Serve the web UI on `addr` until the process is terminated.
pub async fn serve(state: AppState, addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_on(state, listener).await
}

/// Serve the web UI on an already-bound `listener`. Splitting the bind out lets a
/// caller (the daemon) learn whether the bind succeeded *before* committing to
/// serve — e.g. to only advertise its web address once it truly holds the port.
pub async fn serve_on(state: AppState, listener: tokio::net::TcpListener) -> std::io::Result<()> {
    let app = build_router(state);
    axum::serve(listener, app).await
}

/// Serve the web UI with a standalone file-watcher that pushes real-time updates.
pub async fn serve_with_watch(state: AppState, addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_with_watch_on(state, listener).await
}

/// [`serve_with_watch`] on an already-bound `listener` (see [`serve_on`]).
pub async fn serve_with_watch_on(
    state: AppState,
    listener: tokio::net::TcpListener,
) -> std::io::Result<()> {
    let _watcher = watch::spawn(state.clone());
    serve_on(state, listener).await
}

#[cfg(test)]
mod tests {
    use super::host_is_local;

    #[test]
    fn host_is_local_accepts_loopback_with_and_without_port() {
        for h in [
            "localhost",
            "localhost:7373",
            "127.0.0.1",
            "127.0.0.1:7373",
            "[::1]",
            "[::1]:7373",
        ] {
            assert!(host_is_local(h), "should be local: {h}");
        }
    }

    #[test]
    fn host_is_local_rejects_non_loopback() {
        for h in [
            "evil.example.com",
            "evil.example.com:7373",
            "10.0.0.5:7373",
            "example.com",
            "127.0.0.1.evil.com",
            "",
        ] {
            assert!(!host_is_local(h), "should be rejected: {h}");
        }
    }
}
