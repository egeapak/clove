//! The hub: where it lives, and the version handshake that opens every
//! connection (DESIGN §8.2/§8.4).
//!
//! One `cloved` serves every project of a user and is tied to none of them.
//! The first length-delimited JSON frame on a connection is a [`Hello`] naming
//! only the client's protocol version; the hub answers exactly one [`Welcome`];
//! after an `ok` the *same* framed stream carries tarpc ([`crate::CloveRpc`]),
//! where every project-scoped call names its caller's own
//! [`crate::service::Project`].

use camino::{Utf8Path, Utf8PathBuf};
use futures::{SinkExt, StreamExt};
use interprocess::local_socket::tokio::Stream;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tarpc::tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::protocol::StatusResponse;

/// Where a hub lives: its socket, pid, and lock inside one per-user runtime
/// directory (DESIGN §8.2). Two `HubPaths` over different directories are two
/// independent hubs — which is how tests keep from sharing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubPaths {
    dir: Utf8PathBuf,
}

impl HubPaths {
    /// The hub rooted at `dir`, made absolute against this process's working
    /// directory — the hub itself runs from its runtime directory, so a
    /// relative one would name a different place there.
    pub fn at(dir: impl Into<Utf8PathBuf>) -> HubPaths {
        HubPaths {
            dir: crate::absolute(&dir.into()),
        }
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

    /// Whether a hub process holds this directory's lock — alive, whether
    /// starting, serving, or on its way out. The socket answers for a hub only
    /// once it is bound; the lock is taken first and dropped last.
    pub fn running(&self) -> bool {
        if !crate::runtime_dir_is_private(&self.dir) || !self.lock().exists() {
            return false;
        }
        match clove_core::fs_safe::open_lock_file(&self.lock()) {
            Ok(file) => matches!(
                file.try_lock_shared(),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            Err(_) => false,
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

    /// The pid the hub advertised, if its pid file is readable and names a
    /// process that could be signalled (never 0 or 1, never negative as an
    /// `i32` — `kill(0)` and `kill(-1)` address process groups).
    pub fn read_pid(&self) -> Option<u32> {
        crate::parse_pid(&std::fs::read_to_string(self.pid()).ok()?)
    }

    /// The local-socket name clients and the hub agree on: the socket file on
    /// Unix, a per-user pipe on Windows.
    pub fn socket_name(&self) -> std::io::Result<interprocess::local_socket::Name<'static>> {
        use interprocess::local_socket::prelude::*;
        #[cfg(windows)]
        {
            use interprocess::local_socket::GenericNamespaced;
            self.pipe_name()?.to_ns_name::<GenericNamespaced>()
        }
        #[cfg(not(windows))]
        {
            use interprocess::local_socket::GenericFilePath;
            let path = self.sock();
            crate::check_sock_len(&path)?;
            path.into_string().to_fs_name::<GenericFilePath>()
        }
    }

    /// The Windows pipe name: keyed on the user's SID and the runtime
    /// directory, so no two users (or test hubs) ever meet on one pipe.
    #[cfg(windows)]
    pub fn pipe_name(&self) -> std::io::Result<String> {
        Ok(format!("clove-hub-{}", self.user_key()?))
    }

    /// The Windows named event `clove daemon stop --all` signals (DESIGN §8.9).
    #[cfg(windows)]
    pub fn event_name(&self) -> std::io::Result<String> {
        Ok(format!("clove-hub-shutdown-{}", self.user_key()?))
    }

    #[cfg(windows)]
    fn user_key(&self) -> std::io::Result<String> {
        let sid = crate::win::current_user_sid()?;
        Ok(crate::repo_hash(Utf8Path::new(&format!(
            "{sid}|{}",
            self.dir
        ))))
    }
}

/// The first frame a client sends: only its protocol version. The project each
/// call is about travels with the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "hello", rename_all = "snake_case")]
pub enum Hello {
    Clove { protocol: u32 },
}

impl Hello {
    /// The hello this build sends.
    pub fn current() -> Hello {
        Hello::Clove {
            protocol: crate::PROTOCOL_VERSION,
        }
    }

    /// The protocol version the client speaks.
    pub fn protocol(&self) -> u32 {
        match self {
            Hello::Clove { protocol } => *protocol,
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

/// The `code`s a [`Welcome::Err`] or a per-call [`crate::RpcError`] carries.
pub mod codes {
    /// The client's protocol version is not the hub's. Still proof of life:
    /// a hub answered, it just cannot be used by this client.
    pub const PROTOCOL_MISMATCH: &str = "PROTOCOL_MISMATCH";
    /// The first frame was not a `Hello`.
    pub const BAD_HELLO: &str = "BAD_HELLO";
    /// A call's project path is not absolute.
    pub const BAD_PROJECT: &str = "BAD_PROJECT";
    /// `load: false` and the hub is not serving that project.
    pub const NOT_LOADED: &str = "NOT_LOADED";
    /// Another daemon holds the project's `daemon.lock` — a pre-hub daemon, or a
    /// hub under a different runtime dir.
    pub const PROJECT_LOCKED: &str = "PROJECT_LOCKED";
    /// The project could not be loaded (unreadable index, missing directory…).
    pub const LOAD_FAILED: &str = "LOAD_FAILED";
    /// The hub has decided to exit; start a new one once it is gone.
    pub const SHUTTING_DOWN: &str = "SHUTTING_DOWN";
}

/// The reply to `hub_status`.
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
    /// The project's own status — the same payload `status` returns.
    pub status: StatusResponse,
}

/// The reply to `detach`.
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

/// Whether the process at the other end of `stream` runs as this user. Checked
/// by the client (is this really my hub?) and by the hub (is this one of my
/// user's clients?): the runtime directory is private, but a check on the
/// connection itself does not depend on that.
pub fn peer_is_this_user(stream: &Stream) -> std::io::Result<bool> {
    use interprocess::local_socket::traits::StreamCommon as _;
    let creds = stream.peer_creds()?;
    #[cfg(unix)]
    {
        Ok(creds.euid() == Some(crate::current_uid()))
    }
    #[cfg(windows)]
    {
        let pid = creds
            .pid()
            .ok_or_else(|| std::io::Error::other("the pipe peer has no process id"))?;
        Ok(crate::win::process_user_sid(pid)? == crate::win::current_user_sid()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_and_welcome_have_a_stable_wire_shape() {
        assert_eq!(
            serde_json::to_value(Hello::Clove { protocol: 7 }).unwrap(),
            serde_json::json!({"hello":"clove","protocol":7})
        );
        let refused = Welcome::Err {
            protocol: 7,
            code: codes::PROTOCOL_MISMATCH.into(),
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

    #[test]
    fn a_relative_hub_dir_is_made_absolute() {
        assert!(HubPaths::at("rel/run").dir().is_absolute());
    }
}
