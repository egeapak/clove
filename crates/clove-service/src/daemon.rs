//! The daemon write coordinator shared by [`crate::Engine`] and
//! [`crate::StoreService`]: every write prefers the running `cloved`, which
//! serializes it and keeps its index and graph coherent, and falls back to a
//! direct `clove-core` op only when no daemon is reachable.

use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use clove_ipc::{ClientError, DaemonClient};

use crate::ToolError;

/// A cached daemon connection. A probe builds a tokio runtime, connects, and
/// pings — far too heavy to repeat on every call of a long-lived server — so
/// the client is kept and only its (cheap, on the open connection) `ping` is
/// repeated per call; a dead/restarted daemon drops the cache and re-probes.
#[derive(Clone)]
pub(crate) struct Daemon {
    clove_dir: Utf8PathBuf,
    client: Arc<Mutex<Option<DaemonClient>>>,
}

impl Daemon {
    pub(crate) fn new(clove_dir: Utf8PathBuf) -> Self {
        Daemon {
            clove_dir,
            client: Arc::new(Mutex::new(None)),
        }
    }

    /// Run `call` against the daemon, if one is reachable.
    ///
    /// `None` → no daemon (caller falls back to direct ops). The call itself
    /// is attempted exactly ONCE (a failed write RPC must surface, never be
    /// blindly retried — the daemon may have applied it before the response
    /// was lost); only the *liveness check* re-probes.
    pub(crate) fn with<T>(
        &self,
        call: impl FnOnce(&mut DaemonClient) -> Result<T, ClientError>,
    ) -> Option<Result<T, ToolError>> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        // Validate the cached connection first: one ping on the already-open
        // connection, no runtime construction.
        if let Some(client) = guard.as_mut() {
            if client.ping().is_err() {
                *guard = None;
            }
        }
        if guard.is_none() {
            *guard = DaemonClient::probe(&self.clove_dir);
        }
        let client = guard.as_mut()?;
        Some(call(client).map_err(ToolError::Daemon))
    }
}
