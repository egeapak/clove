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
use clove_types::{EditRequest, ItemStatus, NewSpec};
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use serde_json::Value;
use tarpc::context;
use thiserror::Error;
use tokio::runtime::Runtime;
use tokio::time::timeout;

use crate::protocol::{
    GraphRequest, GraphResponse, QueryListResponse, QueryRequest, ReindexDone, StatusResponse,
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

    /// The daemon received the call and **reported a decision**, carrying its
    /// own error classification.
    ///
    /// This means the request was processed and answered — *not* that a write
    /// left the store untouched. Several failures are raised after the mutation
    /// is already durable: `atomic_write` renames the file and only then fsyncs
    /// the parent directory, `add_comment_at` creates the comment file before
    /// writing it, and a panic in the daemon's blocking worker is reported as an
    /// app-level `internal` error even if it happened after the write.
    ///
    /// So a caller must not treat this as "safe to retry locally". What it does
    /// mean is that the daemon's classification is authoritative and should be
    /// reported verbatim, rather than reinterpreted.
    #[error("{0}")]
    App(RpcError),

    /// The transport failed, or the reply had an unexpected shape or protocol
    /// version. The call never produced an answer, so its fate is unknown: the
    /// daemon may have applied a write before the response was lost.
    ///
    /// A write that fails this way must surface as an error rather than fall
    /// back to direct ops, because re-applying is not universally safe — a
    /// second `add_comment` appends a duplicate comment file rather than
    /// erroring, so the fallback would silently duplicate data.
    #[error("daemon transport error: {0}")]
    Transport(String),
}

impl From<ClientError> for clove_types::CloveError {
    /// Carry a daemon failure into the shared error type so callers classify it
    /// through the one taxonomy (`clove_types::error_code`) rather than
    /// re-deriving one per surface.
    ///
    /// An [`ClientError::App`] carries the daemon's `code` across, so a failure
    /// it reported classifies exactly as the same failure raised locally. The
    /// `exit` rides along for clients that do not share the taxonomy (and for
    /// logs), but `clove_types::error_code` resolves the *code* against its own
    /// table rather than trusting that number — see its `Remote` arm. Every
    /// other variant is a communication failure the daemon never classified, and
    /// becomes `DAEMON_ERROR` / exit 7.
    fn from(err: ClientError) -> Self {
        match err {
            ClientError::App(rpc) => clove_types::CloveError::Remote {
                code: rpc.code,
                exit: rpc.exit,
                message: rpc.message,
            },
            other => clove_types::CloveError::Remote {
                code: "DAEMON_ERROR".to_owned(),
                exit: 7,
                message: other.to_string(),
            },
        }
    }
}

/// The diagnostic state of a daemon footprint, as classified by
/// [`DaemonClient::health`] (non-mutating). Used by `clove doctor` to decide
/// whether socket/pid files are safe to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonHealth {
    /// No `daemon.sock` and no `daemon.pid`: no footprint.
    Absent,
    /// A daemon answered `ping` with the matching protocol version.
    Healthy,
    /// A daemon answered but speaks a different/incompatible protocol version —
    /// it is alive (e.g. an old daemon still running after a `clove` upgrade),
    /// so its socket/pid must not be removed; a restart is the remedy.
    Incompatible,
    /// Socket/pid present but nothing answered: corpse files from a crash, safe
    /// to clean up.
    Dead,
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
        // Fast path: no footprint at all → definitely no daemon, nothing to clean.
        // The footprint is the socket file on Unix, but on Windows the transport
        // is a named pipe with no filesystem entry, so there the pid file is the
        // liveness signal (see `footprint_present`).
        if !footprint_present(clove_dir) {
            return None;
        }
        match Self::connect_and_ping(clove_dir) {
            Ok(client) => Some(client),
            Err(ClientError::Connect(_)) => {
                // Connection refused / no listener: the daemon is provably gone,
                // so clean up its crashed-daemon corpse files.
                cleanup_stale(clove_dir);
                None
            }
            Err(_) => {
                // Timeout, protocol mismatch, or name error: the daemon may well
                // be *alive but slow* (a ping can miss the 50ms budget during the
                // startup sweep or under load) — unlinking the socket here would
                // orphan a live daemon. Leave the footprint in place and fall back
                // to direct ops. (A truly dead socket is force-removed by the next
                // daemon's own startup, so this cannot deadlock.)
                None
            }
        }
    }

    /// Liveness check that does **not** mutate the filesystem (unlike
    /// [`DaemonClient::probe`]). Returns `true` only if a daemon answers `ping`
    /// with a compatible protocol version.
    pub fn is_alive(clove_dir: &Utf8Path) -> bool {
        matches!(Self::health(clove_dir), DaemonHealth::Healthy)
    }

    /// Diagnostic daemon liveness for `clove doctor` — richer than [`is_alive`].
    /// Does **not** mutate the filesystem. Mirrors the exact classification
    /// [`DaemonClient::probe`] uses, so a live-but-incompatible daemon (which a
    /// protocol bump produces after a `clove` upgrade) is distinguished from
    /// dead corpse files: the former must be left alone (and a restart advised),
    /// the latter is safe to remove.
    pub fn health(clove_dir: &Utf8Path) -> DaemonHealth {
        let sock = sock_path(clove_dir).exists();
        let pid = pid_path(clove_dir).exists();
        if !sock && !pid {
            return DaemonHealth::Absent;
        }
        // Decide whether there is anything to connect to. On Unix, the socket
        // file is the connect target, so a lone `daemon.pid` (no socket) is a
        // dead footprint. On Windows the transport is a named pipe with no
        // filesystem entry, so the pid file is the liveness signal instead —
        // gating on the (never-created) socket would misreport every live
        // Windows daemon as Dead.
        if !footprint_present(clove_dir) {
            return DaemonHealth::Dead;
        }
        match Self::connect_and_ping(clove_dir) {
            Ok(_) => DaemonHealth::Healthy,
            // Answered, but with a mismatched protocol version (or an otherwise
            // incompatible reply): it is alive — do not touch its socket/pid.
            // `App` is unreachable today (`ping` is infallible at the app level),
            // but it is *proof of life*, so it belongs here rather than falling
            // into `Dead` below — where `doctor --fix` would unlink the socket of
            // a running daemon.
            Err(ClientError::Transport(_) | ClientError::App(_)) => DaemonHealth::Incompatible,
            // Could not connect at all (no listener / refused / stale socket):
            // corpse files from a crashed daemon.
            Err(_) => DaemonHealth::Dead,
        }
    }

    /// Connect + `ping`, bounded by [`CONNECT_TIMEOUT`].
    fn connect_and_ping(clove_dir: &Utf8Path) -> Result<DaemonClient, ClientError> {
        let name = socket_name(clove_dir).map_err(ClientError::Name)?;
        // A current-thread runtime: the client is synchronous (every call is a
        // `block_on`, which also drives the tarpc dispatch task), so spawning
        // a dedicated worker thread per client — for every CLI command that
        // probes — is pure overhead.
        let rt = tokio::runtime::Builder::new_current_thread()
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
                .map_err(|e| ClientError::Transport(e.to_string()))?;
            if version != PROTOCOL_VERSION {
                return Err(ClientError::Transport(format!(
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
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        if version == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(ClientError::Transport(format!(
                "daemon protocol version {version} != {PROTOCOL_VERSION}"
            )))
        }
    }

    /// Run a lean list query; returns the rows + total the CLI shapes itself.
    pub fn query_list(&mut self, req: QueryRequest) -> Result<QueryListResponse, ClientError> {
        self.app(self.client.query(context::current(), req))
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
            .map_err(|e| ClientError::Transport(e.to_string()))
    }

    /// Read the daemon's monotonic graph change-generation counter. Used by the
    /// MCP server's notifier to detect changes and push `resources/updated`.
    pub fn change_generation(&mut self) -> Result<u64, ClientError> {
        self.rt
            .block_on(self.client.change_generation(context::current()))
            .map_err(|e| ClientError::Transport(e.to_string()))
    }

    // ---- M4 mutations + reads (topology B). Each returns the §7.4 item JSON
    // (or `{id, path}`); the daemon serializes writes and keeps itself coherent.

    /// Create an item; returns `{ id, path }`.
    pub fn create(&mut self, spec: NewSpec) -> Result<Value, ClientError> {
        self.app(self.client.create(context::current(), spec))
    }

    /// Transition an item's status; returns the updated item object.
    pub fn set_status(&mut self, id: String, status: ItemStatus) -> Result<Value, ClientError> {
        self.app(self.client.set_status(context::current(), id, status))
    }

    /// Apply `KEY=VALUE` edits atomically; returns the updated item object.
    pub fn edit(&mut self, id: String, assignments: Vec<String>) -> Result<Value, ClientError> {
        self.app(self.client.edit(context::current(), id, assignments))
    }

    /// Apply a structured [`EditRequest`] atomically; returns the updated item object.
    pub fn apply_edit(&mut self, id: String, req: EditRequest) -> Result<Value, ClientError> {
        self.app(self.client.apply_edit(context::current(), id, req))
    }

    /// Append a comment; returns `{ id, path }`.
    pub fn add_comment(
        &mut self,
        id: String,
        author: String,
        body: String,
    ) -> Result<Value, ClientError> {
        self.app(
            self.client
                .add_comment(context::current(), id, author, body),
        )
    }

    /// Add a hard dependency `id → dep_id`; returns the updated item object.
    pub fn dep_add(&mut self, id: String, dep_id: String) -> Result<Value, ClientError> {
        self.app(self.client.dep_add(context::current(), id, dep_id))
    }

    /// Remove a hard dependency `id → dep_id`; returns the updated item object.
    pub fn dep_remove(&mut self, id: String, dep_id: String) -> Result<Value, ClientError> {
        self.app(self.client.dep_remove(context::current(), id, dep_id))
    }

    /// Set (or clear) an item's parent; returns the updated item object.
    pub fn set_parent(&mut self, id: String, parent: Option<String>) -> Result<Value, ClientError> {
        self.app(self.client.set_parent(context::current(), id, parent))
    }

    /// Full item detail (frontmatter + body + comment_count + ready/blocked_by).
    pub fn show(&mut self, id: String) -> Result<Value, ClientError> {
        self.app(self.client.show(context::current(), id))
    }

    /// Work-item analytics (`clove stats`) as JSON.
    pub fn stats(&mut self, top: u32, include_epics: bool) -> Result<Value, ClientError> {
        self.app(self.client.stats(context::current(), top, include_epics))
    }

    /// Drive a fallible RPC call to completion, keeping the application-level
    /// [`RpcError`] (the daemon answered) distinct from the transport-level
    /// `tarpc::client::RpcError` (no answer; the call's fate is unknown).
    ///
    /// These were previously flattened into one stringly-typed variant, which
    /// lost both the daemon's error classification and the difference between
    /// "the daemon decided" and "we never heard back".
    fn app<T, F>(&self, fut: F) -> Result<T, ClientError>
    where
        F: std::future::Future<Output = Result<Result<T, RpcError>, tarpc::client::RpcError>>,
    {
        match self.rt.block_on(fut) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(app_err)) => Err(ClientError::App(app_err)),
            Err(transport_err) => Err(ClientError::Transport(transport_err.to_string())),
        }
    }
}

/// Whether a daemon footprint exists that is worth attempting a connect to.
///
/// The transport differs by platform, so the "is anything there" signal does
/// too: on Unix the daemon binds a filesystem socket (`daemon.sock`), so its
/// presence gates the connect; on Windows the daemon binds a namespaced named
/// pipe that leaves no filesystem entry, so the `daemon.pid` file — written on
/// both platforms only after a successful bind — is the liveness signal instead.
fn footprint_present(clove_dir: &Utf8Path) -> bool {
    #[cfg(windows)]
    {
        pid_path(clove_dir).exists()
    }
    #[cfg(not(windows))]
    {
        sock_path(clove_dir).exists()
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

    /// A failure the daemon reports classifies exactly as the same failure
    /// raised locally — the property the whole seam exists for.
    ///
    /// The expected pair is taken from the *local* classifier rather than
    /// written out here: hardcoding it would only prove that the wire value is
    /// echoed back, which is true of any implementation.
    #[test]
    fn app_error_matches_the_local_classification() {
        let locals = [
            clove_types::CloveError::NotFound {
                id: "proj-0000000A".into(),
            },
            clove_types::CloveError::DependencyCycle {
                from: "a".into(),
                to: "b".into(),
                cycle: vec![],
            },
            clove_types::CloveError::InvalidField {
                field: "priority".into(),
                reason: "out of range".into(),
            },
            clove_types::CloveError::Io {
                path: "/x".into(),
                source: std::io::Error::other("disk"),
            },
        ];
        for local in locals {
            let (code, exit) = clove_types::error_code(&local);
            // What `cloved` would put on the wire for this failure.
            let remote: clove_types::CloveError =
                ClientError::App(RpcError::with_exit(code, local.to_string(), exit)).into();
            assert_eq!(
                clove_types::error_code(&remote),
                (code, exit),
                "`{code}` must classify identically whether local or remote"
            );
        }
    }

    /// A code this build does not recognize must not steer the exit code; it
    /// degrades to the generic daemon error rather than being trusted.
    #[test]
    fn unknown_remote_code_falls_back_to_daemon_error() {
        let err = ClientError::App(RpcError::with_exit("SOME_FUTURE_CODE", "boom", 42));
        let core: clove_types::CloveError = err.into();
        assert_eq!(clove_types::error_code(&core), ("DAEMON_ERROR", 7));
    }

    /// A known code carrying a bogus exit must not reach the caller. Exit 0 is
    /// the dangerous one: it would make a failed command report success.
    #[test]
    fn a_hostile_remote_exit_cannot_force_success() {
        let err = ClientError::App(RpcError::with_exit("ITEM_NOT_FOUND", "gone", 0));
        let core: clove_types::CloveError = err.into();
        assert_eq!(clove_types::error_code(&core), ("ITEM_NOT_FOUND", 2));
    }

    /// Every non-`App` variant is a communication failure the daemon never
    /// classified — exit 7, which is otherwise unreachable.
    #[test]
    fn transport_failures_classify_as_daemon_error() {
        for err in [
            ClientError::Transport("connection reset".to_owned()),
            ClientError::Timeout,
            ClientError::Connect(std::io::Error::other("refused")),
        ] {
            let core: clove_types::CloveError = err.into();
            assert_eq!(clove_types::error_code(&core), ("DAEMON_ERROR", 7));
        }
    }

    /// The wire is self-describing JSON, so a reply from a daemon that predates
    /// the `exit` field still deserializes — defaulting to the daemon error.
    #[test]
    fn rpc_error_without_exit_deserializes_to_daemon_error() {
        let legacy = r#"{"code":"not_found","message":"gone"}"#;
        let parsed: RpcError = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.exit, 7);
        assert_eq!(parsed.code, "not_found");

        // And a current reply round-trips its exit unchanged.
        let current = RpcError::with_exit("ITEM_NOT_FOUND", "gone", 2);
        let wire = serde_json::to_string(&current).unwrap();
        assert_eq!(serde_json::from_str::<RpcError>(&wire).unwrap(), current);
    }

    #[test]
    fn is_alive_false_when_no_socket() {
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        assert!(!DaemonClient::is_alive(&clove_dir));
    }

    #[test]
    fn health_classifies_absent_and_dead() {
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        // No footprint at all.
        assert_eq!(DaemonClient::health(&clove_dir), DaemonHealth::Absent);

        // A lone pid file with no socket is a dead footprint.
        std::fs::write(pid_path(&clove_dir), b"4242").unwrap();
        assert_eq!(DaemonClient::health(&clove_dir), DaemonHealth::Dead);

        // Socket file present but nothing listening (a crashed daemon) is also
        // Dead — and health() must NOT mutate the filesystem (unlike probe()).
        std::fs::write(sock_path(&clove_dir), b"").unwrap();
        assert_eq!(DaemonClient::health(&clove_dir), DaemonHealth::Dead);
        assert!(
            sock_path(&clove_dir).exists(),
            "health() left the sock in place"
        );
        assert!(
            pid_path(&clove_dir).exists(),
            "health() left the pid in place"
        );
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

    /// Regression (D-daemon-1): a live-but-slow daemon that accepts the
    /// connection but does not answer `ping` within the budget must NOT have its
    /// socket unlinked — doing so orphans a running daemon. A ping *timeout* is
    /// treated as "alive but busy", unlike a connection *refusal*.
    #[cfg(unix)]
    #[test]
    fn probe_keeps_socket_when_daemon_is_alive_but_slow() {
        use interprocess::local_socket::traits::tokio::Listener as _;
        use interprocess::local_socket::ListenerOptions;
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let name = socket_name(&clove_dir).unwrap();

        // A "daemon" that binds the socket and accepts connections but never
        // replies — modelling a daemon whose workers are all busy past the 50ms
        // ping budget (a startup sweep, or saturated blocking I/O).
        let bound = Arc::new(Barrier::new(2));
        let bound_srv = Arc::clone(&bound);
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = ListenerOptions::new().name(name).create_tokio().unwrap();
                bound_srv.wait();
                let mut held = Vec::new();
                // Accept (so connect() succeeds) but answer nothing, for a bounded
                // window that outlives the client's probe.
                let _ = timeout(Duration::from_secs(2), async {
                    loop {
                        if let Ok(stream) = listener.accept().await {
                            held.push(stream);
                        }
                    }
                })
                .await;
            });
        });

        bound.wait();
        assert!(
            sock_path(&clove_dir).exists(),
            "listener created the socket file"
        );

        // The probe connects, pings, times out → returns None (fall back to
        // direct ops) but leaves the live daemon's socket untouched.
        assert!(
            DaemonClient::probe(&clove_dir).is_none(),
            "a non-answering daemon yields no usable client"
        );
        assert!(
            sock_path(&clove_dir).exists(),
            "live-but-slow daemon's socket must survive a ping timeout"
        );

        handle.join().unwrap();
    }
}
