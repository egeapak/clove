//! A synchronous, blocking IPC client for talking to a running `cloved`
//! (DESIGN §8.3). No async runtime — this is what the lean `clove` CLI uses.
//!
//! The entry point is [`DaemonClient::probe`]: it builds the platform socket
//! name, connects with a short timeout, sends `PING`, and only returns a live
//! client on `PONG`. On any failure it removes the stale `daemon.sock`/`daemon.pid`
//! (the §8.3 cleanup) and returns `None`, so the caller falls back to direct
//! index/file reads. Every read therefore degrades gracefully when the daemon is
//! absent — the PRD's "nothing required but the binary and the files" guarantee.

use std::io::BufReader;
use std::sync::mpsc;
use std::time::Duration;

use camino::Utf8Path;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{Name, Stream};
use thiserror::Error;

use crate::frame::{self, FrameError};
use crate::protocol::{QueryRequest, Request, Response};
use crate::{pid_path, sock_path};

/// Liveness/connect timeout (DESIGN §8.3: "Attempt connect with 50ms timeout").
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// A client-side IPC failure.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Could not build the platform socket name from the `.clove/` path.
    #[error("invalid socket name: {0}")]
    Name(std::io::Error),

    /// Transport could not connect (no daemon, refused, stale socket).
    #[error("could not connect to daemon: {0}")]
    Connect(std::io::Error),

    /// The connect/handshake did not complete within [`CONNECT_TIMEOUT`].
    #[error("daemon connect timed out")]
    Timeout,

    /// A framing or (de)serialization error on the wire.
    #[error(transparent)]
    Frame(#[from] FrameError),

    /// The daemon replied, but not with the expected response shape.
    #[error("unexpected daemon response: {0}")]
    Protocol(String),
}

/// Build the platform-specific socket name for a `.clove/` directory: a
/// filesystem path on Unix (`daemon.sock`), a namespaced pipe on Windows
/// (`clove-<hash>`), matching the listener side in `cloved` (DESIGN §8.2).
fn socket_name(clove_dir: &Utf8Path) -> Result<Name<'static>, ClientError> {
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        crate::pipe_name(clove_dir)
            .to_ns_name::<GenericNamespaced>()
            .map_err(ClientError::Name)
    }
    #[cfg(not(windows))]
    {
        use interprocess::local_socket::GenericFilePath;
        sock_path(clove_dir)
            .into_string()
            .to_fs_name::<GenericFilePath>()
            .map_err(ClientError::Name)
    }
}

/// A connected, handshaken daemon client.
pub struct DaemonClient {
    stream: BufReader<Stream>,
}

impl DaemonClient {
    /// Connect to the daemon for `clove_dir` and verify it answers `PING` with
    /// `PONG`, all within [`CONNECT_TIMEOUT`]. Returns the live client, or `None`
    /// when no healthy daemon is present — in which case any stale
    /// `daemon.sock`/`daemon.pid` left by a crashed daemon is removed first
    /// (DESIGN §8.3) so the next run starts clean.
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

    /// Connect + `PING`/`PONG`, bounded by [`CONNECT_TIMEOUT`]. The whole
    /// handshake runs on a worker thread so a hung peer cannot block the CLI past
    /// the timeout (the worker is abandoned on timeout; the CLI is short-lived).
    fn connect_and_ping(clove_dir: &Utf8Path) -> Result<DaemonClient, ClientError> {
        let name = socket_name(clove_dir)?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| {
                let stream = Stream::connect(name).map_err(ClientError::Connect)?;
                let mut client = DaemonClient {
                    stream: BufReader::new(stream),
                };
                match client.request(&Request::Ping)? {
                    Response::Pong => Ok(client),
                    other => Err(ClientError::Protocol(format!(
                        "expected PONG, got {other:?}"
                    ))),
                }
            })();
            let _ = tx.send(result);
        });
        match rx.recv_timeout(CONNECT_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ClientError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ClientError::Timeout),
        }
    }

    /// Send one request and read exactly one response frame.
    pub fn request(&mut self, req: &Request) -> Result<Response, ClientError> {
        frame::write_message(self.stream.get_mut(), req)?;
        let resp: Response = frame::read_message(&mut self.stream)?;
        Ok(resp)
    }

    /// Round-trip `PING` → `PONG`; `Ok(())` means the daemon is alive.
    pub fn ping(&mut self) -> Result<(), ClientError> {
        match self.request(&Request::Ping)? {
            Response::Pong => Ok(()),
            other => Err(ClientError::Protocol(format!(
                "expected PONG, got {other:?}"
            ))),
        }
    }

    /// Run a query; returns the standard CLI envelope the daemon built.
    pub fn query(&mut self, req: QueryRequest) -> Result<serde_json::Value, ClientError> {
        match self.request(&Request::Query(req))? {
            Response::Query { envelope } => Ok(envelope),
            Response::Error(e) => Err(ClientError::Protocol(format!("{}: {}", e.code, e.message))),
            other => Err(ClientError::Protocol(format!(
                "expected QUERY reply, got {other:?}"
            ))),
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
