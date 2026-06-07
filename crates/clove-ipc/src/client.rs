//! A blocking client for talking to a running `cloved` (DESIGN §8.3).
//!
//! Internally this drives the async [`crate::service::CloveRpcClient`] (tarpc) on
//! a small owned tokio runtime, exposing a synchronous API so callers (the CLI,
//! the MCP shim's fallback) need not be async themselves.
//!
//! The entry point is [`DaemonClient::probe`]: it builds the platform socket
//! name, connects with a short timeout, sends `ping`, and only returns a live
//! client on success. On any failure it removes the stale `daemon.sock`/
//! `daemon.pid` (the §8.3 cleanup) and returns `None`, so the caller falls back
//! to direct index/file reads.

use std::time::Duration;

use camino::Utf8Path;
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use tarpc::context;
use thiserror::Error;
use tokio::runtime::Runtime;
use tokio::time::timeout;

use crate::protocol::{
    GraphRequest, GraphResponse, QueryListResponse, QueryRequest, ReindexDone, SearchRequest,
    StatusResponse,
};
use crate::service::{CloveRpcClient, RpcError};
use crate::transport::build_transport;
use crate::{pid_path, sock_path, socket_name, PROTOCOL_VERSION};

/// Liveness/connect timeout (DESIGN §8.3: "Attempt connect with 50ms timeout").
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// A client-side IPC failure.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Could not build the platform socket name from the `.clove/` path.
    #[error("invalid socket name: {0}")]
    Name(std::io::Error),

    /// Transport could not connect (no daemon, refused, stale socket), or the
    /// owned runtime could not be created.
    #[error("could not connect to daemon: {0}")]
    Connect(std::io::Error),

    /// The connect/handshake did not complete within [`CONNECT_TIMEOUT`].
    #[error("daemon connect timed out")]
    Timeout,

    /// The daemon replied, but with an error or an unexpected shape.
    #[error("daemon protocol error: {0}")]
    Protocol(String),
}

/// A connected, handshaken daemon client backed by an owned tokio runtime.
pub struct DaemonClient {
    rt: Runtime,
    client: CloveRpcClient,
}

impl DaemonClient {
    /// Connect to the daemon for `clove_dir` and verify it answers `ping`, all
    /// within [`CONNECT_TIMEOUT`]. Returns the live client, or `None` when no
    /// healthy daemon is present — in which case any stale `daemon.sock`/
    /// `daemon.pid` left by a crashed daemon is removed first (DESIGN §8.3).
    pub fn probe(clove_dir: &Utf8Path) -> Option<DaemonClient> {
        // Fast path: no socket file at all → definitely no daemon, nothing to clean.
        if !sock_path(clove_dir).exists() {
            return None;
        }
        match Self::connect_and_ping(clove_dir) {
            Ok(client) => Some(client),
            Err(_) => {
                cleanup_stale(clove_dir);
                None
            }
        }
    }

    /// Liveness check that does **not** mutate the filesystem (unlike
    /// [`DaemonClient::probe`]). Returns `true` only if a daemon answers `ping`.
    /// Used by `clove doctor` (T-D07) to distinguish a live daemon from a
    /// dead-daemon footprint before deciding whether to clean up.
    pub fn is_alive(clove_dir: &Utf8Path) -> bool {
        sock_path(clove_dir).exists() && Self::connect_and_ping(clove_dir).is_ok()
    }

    /// Connect + `ping`, bounded by [`CONNECT_TIMEOUT`].
    fn connect_and_ping(clove_dir: &Utf8Path) -> Result<DaemonClient, ClientError> {
        let name = socket_name(clove_dir).map_err(ClientError::Name)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(ClientError::Connect)?;

        let client = rt.block_on(async {
            let stream = timeout(CONNECT_TIMEOUT, Stream::connect(name))
                .await
                .map_err(|_| ClientError::Timeout)?
                .map_err(ClientError::Connect)?;
            let transport = build_transport(stream);
            let client = CloveRpcClient::new(tarpc::client::Config::default(), transport).spawn();

            let version = timeout(CONNECT_TIMEOUT, client.ping(context::current()))
                .await
                .map_err(|_| ClientError::Timeout)?
                .map_err(|e| ClientError::Protocol(e.to_string()))?;
            if version != PROTOCOL_VERSION {
                return Err(ClientError::Protocol(format!(
                    "daemon protocol version {version} != {PROTOCOL_VERSION}"
                )));
            }
            Ok::<_, ClientError>(client)
        })?;

        Ok(DaemonClient { rt, client })
    }

    /// Round-trip `ping`; `Ok(())` means the daemon is alive.
    pub fn ping(&mut self) -> Result<(), ClientError> {
        let version = self
            .rt
            .block_on(self.client.ping(context::current()))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        if version == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(ClientError::Protocol(format!(
                "daemon protocol version {version} != {PROTOCOL_VERSION}"
            )))
        }
    }

    /// Run a lean list query; returns the rows + total the CLI shapes itself.
    pub fn query_list(&mut self, req: QueryRequest) -> Result<QueryListResponse, ClientError> {
        self.app(self.client.query(context::current(), req))
    }

    /// Run a full-text search; returns matched ids in FTS-rank order.
    pub fn search(&mut self, req: SearchRequest) -> Result<Vec<String>, ClientError> {
        self.app(self.client.search(context::current(), req))
    }

    /// Run a dependency-graph query against the daemon's cached graph.
    pub fn graph(&mut self, req: GraphRequest) -> Result<GraphResponse, ClientError> {
        self.app(self.client.graph(context::current(), req))
    }

    /// Trigger a full reindex inside the daemon; returns its report.
    pub fn reindex(&mut self) -> Result<ReindexDone, ClientError> {
        self.app(self.client.reindex(context::current()))
    }

    /// Fetch the daemon's operational status.
    pub fn status(&mut self) -> Result<StatusResponse, ClientError> {
        self.rt
            .block_on(self.client.status(context::current()))
            .map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// Drive a fallible RPC call to completion, flattening the transport-level
    /// error (`tarpc::client::RpcError`) and the application-level [`RpcError`]
    /// into a single [`ClientError`].
    fn app<T, F>(&self, fut: F) -> Result<T, ClientError>
    where
        F: std::future::Future<Output = Result<Result<T, RpcError>, tarpc::client::RpcError>>,
    {
        match self.rt.block_on(fut) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(app_err)) => Err(ClientError::Protocol(app_err.to_string())),
            Err(transport_err) => Err(ClientError::Protocol(transport_err.to_string())),
        }
    }
}

/// Remove a stale `daemon.sock` and `daemon.pid` (best effort). Called when a
/// connect/handshake fails, so a crashed daemon's corpse files do not linger
/// (DESIGN §8.3).
pub fn cleanup_stale(clove_dir: &Utf8Path) {
    let _ = std::fs::remove_file(sock_path(clove_dir));
    let _ = std::fs::remove_file(pid_path(clove_dir));
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn probe_returns_none_when_no_socket() {
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        assert!(DaemonClient::probe(&clove_dir).is_none());
    }

    #[test]
    fn is_alive_false_when_no_socket() {
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        assert!(!DaemonClient::is_alive(&clove_dir));
    }

    #[test]
    fn probe_cleans_up_stale_socket_and_pid() {
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        // A leftover socket file + pid with nothing listening (a crashed daemon).
        std::fs::write(sock_path(&clove_dir), b"").unwrap();
        std::fs::write(pid_path(&clove_dir), b"4242").unwrap();
        assert!(DaemonClient::probe(&clove_dir).is_none());
        assert!(!sock_path(&clove_dir).exists(), "stale sock removed");
        assert!(!pid_path(&clove_dir).exists(), "stale pid removed");
    }
}
