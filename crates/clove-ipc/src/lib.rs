//! clove daemon IPC: wire protocol, frame codec, and a synchronous client (M3).
//!
//! This crate is deliberately lean — `serde` + a blocking `interprocess`
//! client only. It is the seam between the lean `clove` CLI (client) and the
//! `cloved` daemon (server) so both share one definition of the wire format, while
//! keeping `tokio`/`notify`/`git2` out of the CLI (DESIGN §1; M3_PLAN §1.1).
//!
//! - [`protocol`] — the [`Request`]/[`Response`] types (DESIGN §8.4).
//! - [`frame`] — the 4-byte length-prefix JSON framing.
//! - [`client`] — [`DaemonClient`], a blocking connect-with-timeout client that
//!   probes liveness and cleans up a stale socket (DESIGN §8.3).
//! - path/name helpers for the socket, pid, and lock files (DESIGN §8.2).

pub mod client;
pub mod frame;
pub mod protocol;
pub mod service;

use std::hash::{Hash, Hasher};

use camino::{Utf8Path, Utf8PathBuf};

pub use client::{ClientError, DaemonClient};
pub use frame::{read_frame, read_message, write_frame, write_message, FrameError, MAX_FRAME};
pub use protocol::{
    ErrorResponse, GraphRequest, GraphResponse, LeanRow, QueryKind, QueryListResponse,
    QueryRequest, ReindexDone, Request, Response, SearchRequest, StatusResponse, PROTOCOL_VERSION,
};

/// The Unix socket filename inside `.clove/` (DESIGN §8.2).
pub const SOCK_FILE: &str = "daemon.sock";
/// The daemon PID filename inside `.clove/` (DESIGN §8.2).
pub const PID_FILE: &str = "daemon.pid";
/// The daemon single-instance lock filename inside `.clove/` (DESIGN §8.2).
pub const LOCK_FILE: &str = "daemon.lock";

/// Path to the Unix domain socket for this `.clove/` directory.
pub fn sock_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    clove_dir.join(SOCK_FILE)
}

/// Path to the daemon PID file for this `.clove/` directory.
pub fn pid_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    clove_dir.join(PID_FILE)
}

/// Path to the daemon lock file for this `.clove/` directory.
pub fn lock_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    clove_dir.join(LOCK_FILE)
}

/// Build the platform-specific local-socket name for a `.clove/` directory, used
/// identically by the client ([`DaemonClient`]) and the `cloved` listener so the
/// two always agree (DESIGN §8.2): a filesystem path on Unix (`daemon.sock`), a
/// namespaced pipe on Windows (`clove-<hash>`).
pub fn socket_name(
    clove_dir: &Utf8Path,
) -> std::io::Result<interprocess::local_socket::Name<'static>> {
    use interprocess::local_socket::prelude::*;
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        pipe_name(clove_dir).to_ns_name::<GenericNamespaced>()
    }
    #[cfg(not(windows))]
    {
        use interprocess::local_socket::GenericFilePath;
        sock_path(clove_dir)
            .into_string()
            .to_fs_name::<GenericFilePath>()
    }
}

/// The Windows named shutdown-event name for this `.clove/` directory
/// (DESIGN §8.9). `clove daemon stop` signals it; the daemon waits on it.
#[cfg(windows)]
pub fn event_name(clove_dir: &Utf8Path) -> String {
    format!("clove-shutdown-{}", repo_hash(clove_dir))
}

/// A short, stable hash of the `.clove/` directory path, used to derive the
/// Windows named-pipe name (`\\.\pipe\clove-<hash>`) and the Windows shutdown
/// event name (DESIGN §8.2/§8.9). Deterministic across processes so the CLI and
/// daemon agree; the standard-library [`std::collections::hash_map::DefaultHasher`]
/// is seeded with fixed keys (not randomized), which is sufficient here.
pub fn repo_hash(clove_dir: &Utf8Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    clove_dir.as_str().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The Windows named-pipe name for this `.clove/` directory.
#[cfg(windows)]
pub fn pipe_name(clove_dir: &Utf8Path) -> String {
    format!("clove-{}", repo_hash(clove_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_under_clove_dir() {
        let dir = Utf8Path::new("/repo/.clove");
        assert_eq!(
            sock_path(dir),
            Utf8PathBuf::from("/repo/.clove/daemon.sock")
        );
        assert_eq!(pid_path(dir), Utf8PathBuf::from("/repo/.clove/daemon.pid"));
        assert_eq!(
            lock_path(dir),
            Utf8PathBuf::from("/repo/.clove/daemon.lock")
        );
    }

    #[test]
    fn repo_hash_is_stable_and_path_specific() {
        let a = Utf8Path::new("/repo/.clove");
        let b = Utf8Path::new("/other/.clove");
        assert_eq!(repo_hash(a), repo_hash(a), "hash must be deterministic");
        assert_ne!(repo_hash(a), repo_hash(b), "distinct paths → distinct hash");
        assert_eq!(repo_hash(a).len(), 16);
    }
}
