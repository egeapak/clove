//! Real-time push: the event protocol and the `/api/v1/events` WebSocket handler.
//!
//! A single `tokio::sync::broadcast` channel fans changes out to every connected
//! browser. The watcher (standalone or daemon) is the only producer; HTTP write
//! handlers just write files and let the watcher emit, so there is one event
//! source and no echo storms (DESIGN §8.5 feedback-loop guard).

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;

use crate::AppState;

/// Server → client frames, serialized as `{ "event": "...", "data": {...} }`.
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
    /// A single item was created or changed (carries the full item JSON).
    #[serde(rename = "item.upserted")]
    ItemUpserted { id: String, item: Value },
    /// A single item's file was removed.
    #[serde(rename = "item.deleted")]
    ItemDeleted { id: String },
    /// Aggregate stats changed (the client may refetch `/stats`).
    #[serde(rename = "stats.updated")]
    StatsUpdated {},
    /// Server heartbeat.
    #[serde(rename = "ping")]
    Ping { ts: i64 },
}

/// The `/api/v1/events` WebSocket upgrade handler.
pub async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
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
