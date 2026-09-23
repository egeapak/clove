//! The hub handshake and control service (DESIGN §8.4).
//!
//! One `cloved` serves every project of a user, so a connection has to say which
//! project it is about before any [`crate::CloveRpc`] call can mean anything. The
//! first length-delimited JSON frame on a connection is a [`Hello`]; the hub
//! answers exactly one [`Welcome`]; after an `ok` welcome the *same* framed
//! stream carries tarpc — [`crate::CloveRpc`] bound to the attached project, or
//! [`HubRpc`] for a control connection. Binding the project per connection is
//! what lets the sixteen `CloveRpc` methods stay exactly as they were.

use camino::{Utf8Path, Utf8PathBuf};
use futures::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::Stream;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tarpc::tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::StatusResponse;
use crate::service::RpcError;

/// Where a hub lives: its socket, pid, and lock inside one per-user runtime
/// directory (DESIGN §8.2). Two `HubPaths` over different directories are two
/// independent hubs — which is how tests keep from sharing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubPaths {
    dir: Utf8PathBuf,
}

impl HubPaths {
    /// The hub rooted at `dir`.
    pub fn at(dir: impl Into<Utf8PathBuf>) -> HubPaths {
        HubPaths { dir: dir.into() }
    }

    /// The hub for this user: the one in [`crate::runtime_dir`].
    pub fn resolve() -> HubPaths {
        HubPaths::at(crate::runtime_dir())
    }

    /// Check, before spawning a hub, that it will be able to bind: the runtime
    /// directory exists and is private, and the socket path fits the platform's
    /// limit. The error names the actual cause, where a failed spawn would only
    /// time out.
    pub fn preflight(&self) -> std::io::Result<()> {
        #[cfg(not(windows))]
        crate::ensure_private_dir(&self.dir)?;
        self.socket_name().map(|_| ())
    }

    /// Whether a hub footprint exists that is worth a connect: the socket on
    /// Unix — inside a directory only this user controls, so no one else can
    /// have planted it — and the pid file on Windows, where the pipe leaves
    /// nothing on disk.
    pub fn footprint_present(&self) -> bool {
        #[cfg(windows)]
        {
            self.pid().exists()
        }
        #[cfg(not(windows))]
        {
            self.sock().exists() && crate::runtime_dir_is_private(&self.dir)
        }
    }

    /// The runtime directory.
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// The Unix socket (unused on Windows, where the transport is a named pipe).
    pub fn sock(&self) -> Utf8PathBuf {
        self.dir.join("hub.sock")
    }

    /// The pid file, written once the hub is ready.
    pub fn pid(&self) -> Utf8PathBuf {
        self.dir.join("hub.pid")
    }

    /// The single-instance lock.
    pub fn lock(&self) -> Utf8PathBuf {
        self.dir.join("hub.lock")
    }

    /// The pid the hub advertised, if its pid file is readable.
    pub fn read_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.pid())
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// The local-socket name clients and the hub agree on: the socket file on
    /// Unix, a pipe named after the runtime directory on Windows.
    pub fn socket_name(&self) -> std::io::Result<interprocess::local_socket::Name<'static>> {
        use interprocess::local_socket::prelude::*;
        #[cfg(windows)]
        {
            use interprocess::local_socket::GenericNamespaced;
            self.pipe_name().to_ns_name::<GenericNamespaced>()
        }
        #[cfg(not(windows))]
        {
            use interprocess::local_socket::GenericFilePath;
            let path = self.sock();
            crate::check_sock_len(&path)?;
            path.into_string().to_fs_name::<GenericFilePath>()
        }
    }

    /// The Windows pipe name. Keyed on the runtime directory, which is per user,
    /// so two users (or two test hubs) never meet on one pipe.
    #[cfg(windows)]
    pub fn pipe_name(&self) -> String {
        format!("clove-hub-{}", crate::repo_hash(&self.dir))
    }

    /// The Windows named event `clove daemon stop --all` signals (DESIGN §8.9).
    #[cfg(windows)]
    pub fn event_name(&self) -> String {
        format!("clove-hub-shutdown-{}", crate::repo_hash(&self.dir))
    }
}

/// The first frame a client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "hello", rename_all = "snake_case")]
pub enum Hello {
    /// Serve [`crate::CloveRpc`] for the project at `clove_dir`.
    Attach {
        protocol: u32,
        clove_dir: String,
        /// Load the project if the hub is not serving it yet. A read probe sends
        /// `false`: asking whether a daemon can answer must not start one.
        load: bool,
    },
    /// Serve [`HubRpc`].
    Control { protocol: u32 },
}

impl Hello {
    /// The protocol version the client speaks.
    pub fn protocol(&self) -> u32 {
        match self {
            Hello::Attach { protocol, .. } | Hello::Control { protocol } => *protocol,
        }
    }
}

/// The hub's one reply to a [`Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "welcome", rename_all = "snake_case")]
pub enum Welcome {
    /// Accepted: tarpc follows on this stream.
    Ok { protocol: u32 },
    /// Refused; the hub closes the connection. `code` is one of [`codes`].
    Err {
        protocol: u32,
        code: String,
        message: String,
    },
}

/// The `code`s a [`Welcome::Err`] carries.
pub mod codes {
    /// The client's protocol version is not the hub's. Still proof of life:
    /// a hub answered, it just cannot be used by this client.
    pub const PROTOCOL_MISMATCH: &str = "PROTOCOL_MISMATCH";
    /// `load: false` and the hub is not serving that project.
    pub const NOT_LOADED: &str = "NOT_LOADED";
    /// Another daemon holds the project's `daemon.lock` — a pre-hub daemon, or a
    /// hub under a different runtime dir.
    pub const PROJECT_LOCKED: &str = "PROJECT_LOCKED";
    /// The project could not be loaded (unreadable index, missing directory…).
    pub const LOAD_FAILED: &str = "LOAD_FAILED";
    /// The first frame was not a `Hello`.
    pub const BAD_HELLO: &str = "BAD_HELLO";
    /// The hub is shutting down.
    pub const SHUTTING_DOWN: &str = "SHUTTING_DOWN";
}

/// The control service a [`Hello::Control`] connection speaks.
#[tarpc::service]
pub trait HubRpc {
    /// Liveness: returns [`crate::PROTOCOL_VERSION`].
    async fn ping() -> u32;
    /// The hub and every project it serves.
    async fn hub_status() -> HubStatus;
    /// Stop serving the project at `clove_dir`. When it was the last one the
    /// hub exits right after replying.
    async fn detach(clove_dir: String) -> Result<Detached, RpcError>;
}

/// The reply to [`HubRpc::hub_status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubStatus {
    pub pid: u32,
    pub uptime_s: u64,
    /// `host:port` of the shared web listener, when it is serving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_addr: Option<String>,
    pub projects: Vec<ProjectInfo>,
}

/// One loaded project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// The canonical `.clove/` directory the project is keyed on.
    pub clove_dir: String,
    /// The project's own status — the same payload `CloveRpc::status` returns.
    pub status: StatusResponse,
}

/// The reply to [`HubRpc::detach`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detached {
    /// Whether the project was being served.
    pub detached: bool,
    /// Whether the hub is exiting because nothing is left to serve.
    pub hub_exiting: bool,
}

/// A connection before and after the handshake: the length-delimited frames
/// tarpc's JSON transport also uses, so the stream is handed over as is.
pub type HubFramed = Framed<Stream, LengthDelimitedCodec>;

/// Frame a freshly connected or accepted stream.
pub fn frame(stream: Stream) -> HubFramed {
    LengthDelimitedCodec::builder().new_framed(stream)
}

/// Send one JSON frame.
pub async fn send_frame<T: Serialize>(framed: &mut HubFramed, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    framed.send(bytes.into()).await
}

/// Receive one JSON frame; `Ok(None)` when the peer closed first.
pub async fn recv_frame<T: DeserializeOwned>(framed: &mut HubFramed) -> std::io::Result<Option<T>> {
    match framed.next().await {
        None => Ok(None),
        Some(frame) => serde_json::from_slice(&frame?)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_and_welcome_have_a_stable_wire_shape() {
        let attach = Hello::Attach {
            protocol: 7,
            clove_dir: "/r/.clove".into(),
            load: false,
        };
        assert_eq!(
            serde_json::to_value(&attach).unwrap(),
            serde_json::json!({"hello":"attach","protocol":7,"clove_dir":"/r/.clove","load":false})
        );
        assert_eq!(
            serde_json::to_value(Hello::Control { protocol: 7 }).unwrap(),
            serde_json::json!({"hello":"control","protocol":7})
        );
        let refused = Welcome::Err {
            protocol: 7,
            code: codes::NOT_LOADED.into(),
            message: "x".into(),
        };
        let wire = serde_json::to_string(&refused).unwrap();
        assert_eq!(serde_json::from_str::<Welcome>(&wire).unwrap(), refused);
    }

    /// A pre-hub client opens with a tarpc request, not a `Hello`; the hub must
    /// be able to tell, so it can refuse instead of misreading it.
    #[test]
    fn a_tarpc_request_is_not_a_hello() {
        let tarpc_frame = r#"{"Request":{"context":{},"id":0,"message":{"Ping":{}}}}"#;
        assert!(serde_json::from_str::<Hello>(tarpc_frame).is_err());
    }
}
