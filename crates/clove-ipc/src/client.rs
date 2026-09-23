//! Blocking clients for the `cloved` hub (DESIGN §8.3).
//!
//! Internally these drive the async tarpc clients on a small owned tokio
//! runtime, exposing a synchronous API so callers (the CLI, the MCP shim's
//! fallback) need not be async themselves.
//!
//! The entry point is [`DaemonClient::probe`]: it connects to the user's hub
//! with a short timeout and attaches to one project without loading it, so it
//! returns a live client only when the hub is already serving that project.
//! When nothing is listening it removes the hub's stale socket/pid (the §8.3
//! cleanup) and returns `None`, so the caller falls back to direct index/file
//! reads. [`HubClient`] speaks the hub's control service (status, detach).

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

use crate::hub::{frame, recv_frame, send_frame, Detached, Hello, HubPaths, HubStatus, Welcome};
use crate::protocol::{
    GraphRequest, GraphResponse, QueryListResponse, QueryRequest, ReindexDone, StatusResponse,
};
use crate::service::{CloveRpcClient, RpcError};
use crate::transport::transport_from_framed;
use crate::{legacy_sock_path, pid_path, PROTOCOL_VERSION};

/// Liveness/connect timeout (DESIGN §8.3: "Attempt connect with 50ms timeout").
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// How long an attach that loads the project may take: the hub opens (and may
/// rebuild) its index and sweeps it before answering.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// A client-side IPC failure.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Could not build the platform socket name.
    #[error("invalid socket name: {0}")]
    Name(std::io::Error),

    /// Transport could not connect (no daemon, refused, stale socket), or the
    /// owned runtime could not be created.
    #[error("could not connect to daemon: {0}")]
    Connect(std::io::Error),

    /// The connect/handshake did not complete within [`CONNECT_TIMEOUT`].
    #[error("daemon connect timed out")]
    Timeout,

    /// The hub answered the handshake and turned the connection down; `code` is
    /// one of [`crate::hub::codes`]. Proof that a hub is alive.
    #[error("{message}")]
    Refused { code: String, message: String },

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

/// The diagnostic state of the hub's footprint, as classified by
/// [`DaemonClient::health`] (non-mutating). Used by `clove doctor` to decide
/// whether the socket/pid files are safe to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonHealth {
    /// No socket and no pid file: no footprint.
    Absent,
    /// A hub answered the handshake with the matching protocol version.
    Healthy,
    /// A hub answered but speaks a different protocol version — it is alive
    /// (e.g. an old hub still running after a `clove` upgrade), so its files
    /// must not be removed; a restart is the remedy.
    Incompatible,
    /// Socket/pid present but nothing answered: corpse files from a crash, safe
    /// to clean up.
    Dead,
}

/// A connected client attached to one project on the hub.
pub struct DaemonClient {
    rt: Runtime,
    client: CloveRpcClient,
}

impl DaemonClient {
    /// A client for `clove_dir` on the user's hub, if the hub is already
    /// serving that project. Never starts a hub or loads a project — see
    /// [`crate::ensure_daemon`] for that.
    pub fn probe(clove_dir: &Utf8Path) -> Option<DaemonClient> {
        DaemonClient::probe_at(&HubPaths::resolve(), clove_dir)
    }

    /// [`DaemonClient::probe`] against an explicit hub.
    pub fn probe_at(hub: &HubPaths, clove_dir: &Utf8Path) -> Option<DaemonClient> {
        // Fast path: no footprint at all → definitely no hub, nothing to clean.
        if !hub.footprint_present() {
            return None;
        }
        match DaemonClient::attach(hub, clove_dir, false) {
            Ok(client) => Some(client),
            Err(ClientError::Connect(_)) => {
                // Connection refused / no listener: the hub is provably gone,
                // so clean up its crashed-hub corpse files.
                cleanup_stale_hub(hub);
                None
            }
            // Not serving this project, a timeout, a protocol mismatch: the hub
            // may well be alive (a ping can miss the 50ms budget under load) —
            // unlinking its socket here would orphan it. Fall back to direct
            // ops and leave the footprint alone.
            Err(_) => None,
        }
    }

    /// Connect to the hub and attach to `clove_dir`, loading the project first
    /// when `load` is set (bounded by [`LOAD_TIMEOUT`] then, by
    /// [`CONNECT_TIMEOUT`] otherwise).
    pub fn attach(
        hub: &HubPaths,
        clove_dir: &Utf8Path,
        load: bool,
    ) -> Result<DaemonClient, ClientError> {
        let hello = Hello::Attach {
            protocol: PROTOCOL_VERSION,
            clove_dir: clove_dir.to_string(),
            load,
        };
        let budget = if load { LOAD_TIMEOUT } else { CONNECT_TIMEOUT };
        let (rt, stream) = handshake(hub, &hello, budget)?;
        let client = {
            let _guard = rt.enter();
            CloveRpcClient::new(tarpc::client::Config::default(), stream).spawn()
        };
        Ok(DaemonClient { rt, client })
    }

    /// The hub's health, without touching the filesystem (unlike
    /// [`DaemonClient::probe`]). A live-but-incompatible hub — which a protocol
    /// bump produces after a `clove` upgrade — is told apart from dead corpse
    /// files: the former must be left alone (and a restart advised), the latter
    /// is safe to remove.
    pub fn health(hub: &HubPaths) -> DaemonHealth {
        if !hub.sock().exists() && !hub.pid().exists() {
            return DaemonHealth::Absent;
        }
        if !hub.footprint_present() {
            return DaemonHealth::Dead;
        }
        match HubClient::connect(hub) {
            Ok(_) => DaemonHealth::Healthy,
            Err(ClientError::Refused { .. }) | Err(ClientError::Transport(_)) => {
                DaemonHealth::Incompatible
            }
            // Could not connect at all (no listener / refused / stale socket):
            // corpse files from a crashed hub. A timeout lands here too, which
            // is why `doctor` re-checks before it deletes anything.
            Err(_) => DaemonHealth::Dead,
        }
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

/// A connection to the hub's control service ([`crate::HubRpc`]).
pub struct HubClient {
    rt: Runtime,
    client: crate::hub::HubRpcClient,
}

impl HubClient {
    /// Connect to the hub at `hub` and open its control service.
    pub fn connect(hub: &HubPaths) -> Result<HubClient, ClientError> {
        if !hub.footprint_present() {
            return Err(ClientError::Connect(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no daemon is running",
            )));
        }
        let hello = Hello::Control {
            protocol: PROTOCOL_VERSION,
        };
        let (rt, stream) = handshake(hub, &hello, CONNECT_TIMEOUT)?;
        let client = {
            let _guard = rt.enter();
            crate::hub::HubRpcClient::new(tarpc::client::Config::default(), stream).spawn()
        };
        Ok(HubClient { rt, client })
    }

    /// The hub and every project it serves.
    pub fn status(&mut self) -> Result<HubStatus, ClientError> {
        self.rt
            .block_on(self.client.hub_status(context::current()))
            .map_err(|e| ClientError::Transport(e.to_string()))
    }

    /// Stop serving `clove_dir`; returns once the hub has torn it down.
    pub fn detach(&mut self, clove_dir: &Utf8Path) -> Result<Detached, ClientError> {
        match self.rt.block_on(
            self.client
                .detach(context::current(), clove_dir.to_string()),
        ) {
            Ok(Ok(detached)) => Ok(detached),
            Ok(Err(app)) => Err(ClientError::App(app)),
            Err(e) => Err(ClientError::Transport(e.to_string())),
        }
    }
}

type HubTransport<Item, SinkItem> = tarpc::serde_transport::Transport<
    Stream,
    Item,
    SinkItem,
    tarpc::tokio_serde::formats::Json<Item, SinkItem>,
>;

/// Connect to the hub, send `hello`, and return the connection ready for tarpc
/// once the hub welcomes it — all within `budget`.
fn handshake<Item, SinkItem>(
    hub: &HubPaths,
    hello: &Hello,
    budget: Duration,
) -> Result<(Runtime, HubTransport<Item, SinkItem>), ClientError>
where
    Item: serde::de::DeserializeOwned,
    SinkItem: serde::Serialize,
{
    let name = hub.socket_name().map_err(ClientError::Name)?;
    // A current-thread runtime: the client is synchronous (every call is a
    // `block_on`, which also drives the tarpc dispatch task), so spawning a
    // dedicated worker thread per client — for every CLI command that probes —
    // is pure overhead.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ClientError::Connect)?;
    let transport = rt.block_on(async {
        let stream = timeout(CONNECT_TIMEOUT, Stream::connect(name))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(ClientError::Connect)?;
        let mut framed = frame(stream);
        let welcome = timeout(budget, async {
            send_frame(&mut framed, hello).await?;
            recv_frame::<Welcome>(&mut framed).await
        })
        .await
        .map_err(|_| ClientError::Timeout)?
        .map_err(|e| ClientError::Transport(e.to_string()))?;
        match welcome {
            Some(Welcome::Ok { protocol }) if protocol == PROTOCOL_VERSION => {
                Ok(transport_from_framed(framed))
            }
            Some(Welcome::Ok { protocol }) => Err(ClientError::Transport(format!(
                "daemon protocol version {protocol} != {PROTOCOL_VERSION}"
            ))),
            Some(Welcome::Err { code, message, .. }) => Err(ClientError::Refused { code, message }),
            None => Err(ClientError::Transport(
                "the daemon closed the connection during the handshake".to_owned(),
            )),
        }
    })?;
    Ok((rt, transport))
}

/// Remove a crashed hub's socket and pid (best effort) — only once a connect
/// was refused, so a live hub's files are never touched (DESIGN §8.3).
fn cleanup_stale_hub(hub: &HubPaths) {
    #[cfg(not(windows))]
    let _ = std::fs::remove_file(hub.sock());
    let _ = std::fs::remove_file(hub.pid());
}

/// [`cleanup_stale_hub`] for `clove doctor --fix`, which has classified the
/// footprint [`DaemonHealth::Dead`] first.
pub fn cleanup_hub(hub: &HubPaths) {
    cleanup_stale_hub(hub);
}

/// Remove a pre-hub (clove 0.1.0) daemon's corpse `daemon.sock`/`daemon.pid`
/// from `.clove/`.
pub fn cleanup_legacy(clove_dir: &Utf8Path) {
    let _ = std::fs::remove_file(legacy_sock_path(clove_dir));
    let _ = std::fs::remove_file(pid_path(clove_dir));
}

/// The pid of a live pre-hub (clove 0.1.0) daemon serving `clove_dir`, if one
/// answers on `.clove/daemon.sock`.
///
/// Any answer to `ping` counts, whatever its protocol version: the question is
/// only whether a clove daemon is really listening before `clove daemon stop`
/// signals the pid it left behind — a stale pid can name an unrelated process
/// after pid reuse. The v6 `ping` has the same wire shape as today's.
pub fn legacy_daemon_pid(clove_dir: &Utf8Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_path(clove_dir))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    #[cfg(windows)]
    {
        // A 0.1.0 Windows daemon listened on a pipe named after the repo; it
        // cannot be verified from here, so it is never signalled.
        let _ = pid;
        None
    }
    #[cfg(not(windows))]
    {
        use interprocess::local_socket::prelude::*;
        use interprocess::local_socket::GenericFilePath;
        let sock = legacy_sock_path(clove_dir);
        if !sock.exists() {
            return None;
        }
        use crate::transport::build_transport;
        let name = sock.into_string().to_fs_name::<GenericFilePath>().ok()?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            let stream = timeout(Duration::from_millis(200), Stream::connect(name))
                .await
                .ok()?
                .ok()?;
            let client =
                CloveRpcClient::new(tarpc::client::Config::default(), build_transport(stream))
                    .spawn();
            timeout(Duration::from_millis(500), client.ping(context::current()))
                .await
                .ok()?
                .ok()
        })?;
        Some(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

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

    /// A private temp dir to root a test hub in (tempdirs are 0700 on Unix).
    fn hub_dir() -> (tempfile::TempDir, HubPaths) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, HubPaths::at(path))
    }

    #[test]
    fn probe_returns_none_when_no_hub() {
        let (_tmp, hub) = hub_dir();
        assert!(DaemonClient::probe_at(&hub, Utf8Path::new("/repo/.clove")).is_none());
    }

    #[test]
    fn health_classifies_absent_and_dead() {
        let (_tmp, hub) = hub_dir();
        assert_eq!(DaemonClient::health(&hub), DaemonHealth::Absent);

        std::fs::write(hub.pid(), b"4242").unwrap();
        #[cfg(not(windows))]
        assert_eq!(
            DaemonClient::health(&hub),
            DaemonHealth::Dead,
            "a lone pid file with no socket is a dead footprint"
        );

        // Socket file present but nothing listening (a crashed hub) is also
        // Dead — and health() must NOT mutate the filesystem (unlike probe()).
        std::fs::write(hub.sock(), b"").unwrap();
        assert_eq!(DaemonClient::health(&hub), DaemonHealth::Dead);
        assert!(hub.sock().exists(), "health() left the sock in place");
        assert!(hub.pid().exists(), "health() left the pid in place");
    }

    #[cfg(unix)]
    #[test]
    fn probe_cleans_up_a_stale_hub_socket_and_pid() {
        let (_tmp, hub) = hub_dir();
        // A leftover socket file + pid with nothing listening (a crashed hub).
        std::fs::write(hub.sock(), b"").unwrap();
        std::fs::write(hub.pid(), b"4242").unwrap();
        assert!(DaemonClient::probe_at(&hub, Utf8Path::new("/repo/.clove")).is_none());
        assert!(!hub.sock().exists(), "stale sock removed");
        assert!(!hub.pid().exists(), "stale pid removed");
    }

    /// A stand-in hub on `hub` that answers each connection's first frame with
    /// `reply` (or never, when `None`) for as long as `window` lasts.
    #[cfg(unix)]
    fn fake_hub(
        hub: &HubPaths,
        reply: Option<Welcome>,
        window: Duration,
    ) -> std::thread::JoinHandle<()> {
        use interprocess::local_socket::traits::tokio::Listener as _;
        use interprocess::local_socket::ListenerOptions;
        use std::sync::{Arc, Barrier};

        let name = hub.socket_name().unwrap();
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
                let _ = timeout(window, async {
                    loop {
                        if let Ok(stream) = listener.accept().await {
                            let mut framed = frame(stream);
                            if let Some(reply) = &reply {
                                let _ = recv_frame::<Hello>(&mut framed).await;
                                let _ = send_frame(&mut framed, reply).await;
                            }
                            held.push(framed);
                        }
                    }
                })
                .await;
            });
        });
        bound.wait();
        handle
    }

    /// Regression (D-daemon-1): a live-but-slow hub that accepts the connection
    /// but does not answer within the budget must NOT have its socket unlinked
    /// — doing so orphans a running hub. A timeout is "alive but busy", unlike a
    /// refused connection.
    #[cfg(unix)]
    #[test]
    fn probe_keeps_socket_when_hub_is_alive_but_slow() {
        let (_tmp, hub) = hub_dir();
        let handle = fake_hub(&hub, None, Duration::from_secs(2));
        assert!(hub.sock().exists(), "listener created the socket file");
        assert!(
            DaemonClient::probe_at(&hub, Utf8Path::new("/repo/.clove")).is_none(),
            "a non-answering hub yields no usable client"
        );
        assert!(
            hub.sock().exists(),
            "live-but-slow hub's socket must survive a timeout"
        );
        handle.join().unwrap();
    }

    /// A hub that is up but not serving the project turns the attach down; the
    /// probe falls back without touching the hub's files.
    #[cfg(unix)]
    #[test]
    fn a_refused_attach_names_its_code_and_keeps_the_hub() {
        let (_tmp, hub) = hub_dir();
        let refusal = Welcome::Err {
            protocol: PROTOCOL_VERSION,
            code: crate::hub::codes::NOT_LOADED.to_owned(),
            message: "not serving /repo/.clove".to_owned(),
        };
        let handle = fake_hub(&hub, Some(refusal), Duration::from_millis(500));
        match DaemonClient::attach(&hub, Utf8Path::new("/repo/.clove"), false) {
            Err(ClientError::Refused { code, .. }) => {
                assert_eq!(code, crate::hub::codes::NOT_LOADED)
            }
            Err(other) => panic!("expected a refusal, got {other}"),
            Ok(_) => panic!("expected a refusal, got a client"),
        }
        assert!(DaemonClient::probe_at(&hub, Utf8Path::new("/repo/.clove")).is_none());
        assert!(hub.sock().exists(), "a refusing hub is alive");
        handle.join().unwrap();
    }

    #[test]
    fn no_legacy_daemon_without_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        assert_eq!(legacy_daemon_pid(&clove_dir), None);
        // A pid with nothing answering on the socket is not proof of a daemon.
        std::fs::write(pid_path(&clove_dir), b"4242").unwrap();
        std::fs::write(legacy_sock_path(&clove_dir), b"").unwrap();
        assert_eq!(legacy_daemon_pid(&clove_dir), None);
    }
}
