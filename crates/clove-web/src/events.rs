//! Real-time push: the event protocol and the `/api/v1/events` WebSocket handler.
//!
//! A single `tokio::sync::broadcast` channel fans changes out to every connected
//! browser. The watcher (standalone or daemon) is the only producer; HTTP write
//! handlers just write files and let the watcher emit, so there is one event
//! source and no echo storms (DESIGN §8.5 feedback-loop guard).

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;

use crate::AppState;

/// Server → client frames, serialized as `{ "event": "...", "data": {...} }`.
///
/// The protocol is deliberately minimal: `hello` on connect, then `batch` for
/// every change (per-id `item.*`/`stats.*`/`ping` variants once existed but
/// were never emitted, so both sides carried dead plumbing — the client always
/// resyncs from a batch).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum Event {
    /// Sent immediately on connect.
    #[serde(rename = "hello")]
    Hello {
        protocol: u32,
        source: String,
        seq: u64,
    },
    /// A debounced batch of file changes. `changed`/`deleted` may be empty, in
    /// which case the client should refetch (the standalone watcher reports
    /// "something changed" rather than diffing ids).
    #[serde(rename = "batch")]
    Batch {
        changed: Vec<String>,
        deleted: Vec<String>,
        seq: u64,
    },
}

/// Whether a browser `Origin` header names a loopback origin. WS handshakes are
/// not subject to the same-origin policy, so without this any web page could open
/// `ws://127.0.0.1:<port>/api/v1/events` and read full item data (cross-origin WS
/// read / DNS-rebinding exfiltration). An absent Origin (non-browser clients) is
/// allowed; a present, non-local Origin (including the literal `"null"`) is not.
fn origin_is_local(origin: &str) -> bool {
    let after = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
    let authority = after.split(['/', '?', '#']).next().unwrap_or(after);
    crate::host_is_local(authority)
}

/// The `/api/v1/events` WebSocket upgrade handler.
pub async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Reject cross-origin handshakes before upgrading (the Host middleware covers
    // the Host header; Origin is the browser-controlled cross-origin signal).
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if !origin_is_local(origin) {
            return (StatusCode::FORBIDDEN, "forbidden: cross-origin WebSocket").into_response();
        }
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.events.subscribe();

    // Greet the client with the current sequence and serving mode.
    let hello = Event::Hello {
        protocol: 1,
        source: state.source.clone(),
        seq: state.current_seq(),
    };
    if let Ok(text) = serde_json::to_string(&hello) {
        if sender.send(Message::Text(text)).await.is_err() {
            return;
        }
    }

    // Forward broadcast events; stop when the client disconnects.
    let forward = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let Ok(text) = serde_json::to_string(&event) else {
                        continue;
                    };
                    if sender.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                // Lagged: the client fell behind; tell it to resync via a batch.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let resync = Event::Batch {
                        changed: Vec::new(),
                        deleted: Vec::new(),
                        seq: 0,
                    };
                    if let Ok(text) = serde_json::to_string(&resync) {
                        if sender.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Drain inbound frames (client pings/close) until the socket closes.
    while let Some(Ok(msg)) = receiver.next().await {
        if matches!(msg, Message::Close(_)) {
            break;
        }
    }
    forward.abort();
}

#[cfg(test)]
mod tests {
    use super::origin_is_local;

    #[test]
    fn origin_is_local_accepts_loopback_origins() {
        for o in [
            "http://localhost",
            "http://localhost:5173",
            "http://127.0.0.1:7373",
            "https://[::1]:7373",
        ] {
            assert!(origin_is_local(o), "should be local: {o}");
        }
    }

    #[test]
    fn origin_is_local_rejects_cross_origin() {
        for o in [
            "http://evil.example.com",
            "https://evil.example.com:443",
            "null",
        ] {
            assert!(!origin_is_local(o), "should be rejected: {o}");
        }
    }
}
