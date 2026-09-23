//! clove daemon IPC: the typed `tarpc` service, payload types, and a synchronous
//! client (M3/M4).
//!
//! This crate is the seam between the `clove` CLI / MCP server (clients) and the
//! `cloved` daemon (server) so all share one definition of the wire format. As of
//! M4 the transport is `tarpc` over a local socket (replacing the hand-rolled
//! frame/protocol).
//!
//! - [`service`] — the `#[tarpc::service]` contract ([`CloveRpc`]) + [`RpcError`].
//! - [`protocol`] — the request/response payload types (DESIGN §8.4).
//! - [`client`] — [`DaemonClient`], a blocking connect-with-timeout client that
//!   probes liveness and cleans up a stale socket (DESIGN §8.3), and
//!   [`HubClient`] for the hub's control service.
//! - [`hub`] — the hub handshake, control service, and [`HubPaths`].
//! - the per-user runtime directory the hub lives in (DESIGN §8.2).

pub mod client;
pub mod hub;
pub mod protocol;
pub mod service;
pub mod spawn;
pub mod transport;

use camino::{Utf8Path, Utf8PathBuf};

pub use client::{
    cleanup_hub, cleanup_legacy, legacy_daemon_pid, ClientError, DaemonClient, DaemonHealth,
    HubClient,
};
pub use hub::{Detached, HubPaths, HubRpc, HubRpcClient, HubStatus, ProjectInfo};
pub use protocol::{
    GraphRequest, GraphResponse, LeanRow, QueryKind, QueryListResponse, QueryRequest, ReindexDone,
    StatusResponse, PROTOCOL_VERSION,
};
pub use service::{CloveRpc, CloveRpcClient, RpcError};
pub use spawn::{cloved_path, ensure_daemon, ensure_daemon_at, spawn_hub};
pub use transport::{build_transport, transport_from_framed};

/// The Unix socket filename a clove 0.1.0 daemon binds inside `.clove/`. The hub
/// lives in the per-user [`runtime_dir`]; the name is kept so a pre-hub daemon
/// can be recognized (and its leftovers stay git-ignored).
pub const SOCK_FILE: &str = "daemon.sock";
/// The pid filename a clove 0.1.0 daemon writes inside `.clove/`.
pub const PID_FILE: &str = "daemon.pid";
/// The per-project lock filename inside `.clove/` (DESIGN §8.2): whichever daemon
/// serves the project holds it — a hub slot, or a pre-hub daemon.
pub const LOCK_FILE: &str = "daemon.lock";

/// The longest socket path the platform accepts, excluding the trailing NUL
/// (`sizeof(sun_path) - 1`).
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub const SUN_PATH_MAX: usize = 103;
/// The longest socket path the platform accepts, excluding the trailing NUL
/// (`sizeof(sun_path) - 1`).
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
pub const SUN_PATH_MAX: usize = 107;

/// The per-user directory holding daemon sockets:
///
/// 1. `$CLOVE_RUNTIME_DIR`, verbatim — an explicit override (tests use it);
/// 2. `$XDG_RUNTIME_DIR/clove`;
/// 3. `$TMPDIR/clove-<uid>`, else `/tmp/clove-<uid>`.
///
/// Only the daemon creates it ([`ensure_runtime_dir`]); clients connect only if
/// [`runtime_dir_is_private`] holds, so another user can't plant a socket there.
pub fn runtime_dir() -> Utf8PathBuf {
    resolve_runtime_dir(utf8_var, current_uid())
}

fn resolve_runtime_dir(var: impl Fn(&str) -> Option<Utf8PathBuf>, uid: u32) -> Utf8PathBuf {
    if let Some(dir) = var("CLOVE_RUNTIME_DIR") {
        return dir;
    }
    if let Some(dir) = var("XDG_RUNTIME_DIR") {
        return dir.join("clove");
    }
    let tmp = var("TMPDIR").unwrap_or_else(|| Utf8PathBuf::from("/tmp"));
    tmp.join(format!("clove-{uid}"))
}

/// Create the [`runtime_dir`] owner-only if missing, and refuse one that another
/// user owns or can write to.
pub fn ensure_runtime_dir() -> std::io::Result<Utf8PathBuf> {
    let dir = runtime_dir();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// [`ensure_runtime_dir`] for an explicit directory (a [`HubPaths`] root).
pub fn ensure_private_dir(dir: &Utf8Path) -> std::io::Result<()> {
    if !dir.exists() {
        create_private_dir(dir)?;
    }
    if !runtime_dir_is_private(dir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "daemon runtime directory {dir} must be owned by the current user and \
                 not writable by others (set CLOVE_RUNTIME_DIR to use another directory)"
            ),
        ));
    }
    Ok(())
}

/// Whether `dir` is a directory owned by the current user that no one else can
/// write to — the precondition for trusting a socket inside it.
#[cfg(unix)]
pub fn runtime_dir_is_private(dir: &Utf8Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir)
        .is_ok_and(|m| m.is_dir() && m.uid() == current_uid() && m.mode() & 0o022 == 0)
}

/// Windows daemons use named pipes, not files in the runtime directory.
#[cfg(not(unix))]
pub fn runtime_dir_is_private(_dir: &Utf8Path) -> bool {
    true
}

#[cfg(unix)]
fn create_private_dir(dir: &Utf8Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Utf8Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid takes no arguments, cannot fail, and touches no memory.
    unsafe { getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// An environment variable as a UTF-8 path, treated as absent when empty or not
/// UTF-8.
fn utf8_var(name: &str) -> Option<Utf8PathBuf> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .map(Utf8PathBuf::from)
}

/// The pid file of a clove 0.1.0 daemon for this `.clove/` directory.
pub fn pid_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    clove_dir.join(PID_FILE)
}

/// The socket of a clove 0.1.0 daemon for this `.clove/` directory.
pub fn legacy_sock_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    clove_dir.join(SOCK_FILE)
}

/// The per-project lock for this `.clove/` directory.
pub fn lock_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    clove_dir.join(LOCK_FILE)
}

/// Reject a socket path the kernel would refuse, naming the cause and the fix.
#[cfg(not(windows))]
pub(crate) fn check_sock_len(path: &Utf8Path) -> std::io::Result<()> {
    if path.as_str().len() <= SUN_PATH_MAX {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "daemon socket path {path} is {} bytes, over the platform limit of \
             {SUN_PATH_MAX}; set CLOVE_RUNTIME_DIR to a shorter directory",
            path.as_str().len()
        ),
    ))
}

/// A short, stable hash of a path, used to derive the hub's Windows named-pipe
/// and shutdown-event names from its runtime directory (DESIGN §8.2/§8.9). Must be deterministic *across processes and
/// across builds* so independently-compiled binaries (`clove`, `cloved`,
/// `clove-mcp`) — possibly built with different toolchains (a distro-packaged
/// `clove` alongside a `cargo install`ed `cloved`) — always agree on the name.
///
/// This uses an inlined, explicitly-versioned FNV-1a rather than
/// `std::collections::hash_map::DefaultHasher`, whose algorithm std documents as
/// unspecified and *not* stable across releases — relying on it across a binary
/// boundary is a latent contract violation.
pub fn repo_hash(clove_dir: &Utf8Path) -> String {
    // FNV-1a, 64-bit (offset basis + prime are the published FNV constants).
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in clove_dir.as_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_files_are_under_clove_dir() {
        let dir = Utf8Path::new("/repo/.clove");
        assert_eq!(pid_path(dir), Utf8PathBuf::from("/repo/.clove/daemon.pid"));
        assert_eq!(
            legacy_sock_path(dir),
            Utf8PathBuf::from("/repo/.clove/daemon.sock")
        );
        assert_eq!(
            lock_path(dir),
            Utf8PathBuf::from("/repo/.clove/daemon.lock")
        );
    }

    #[test]
    fn the_hub_lives_in_the_runtime_dir() {
        let hub = HubPaths::resolve();
        assert_eq!(hub.dir(), runtime_dir());
        assert_eq!(hub.sock(), runtime_dir().join("hub.sock"));
        assert_eq!(hub.pid(), runtime_dir().join("hub.pid"));
        assert_eq!(hub.lock(), runtime_dir().join("hub.lock"));
    }

    #[cfg(not(windows))]
    #[test]
    fn an_over_long_hub_socket_is_refused_with_the_fix() {
        let deep = HubPaths::at(format!("/{}", "nested-directory/".repeat(8)));
        let message = deep.socket_name().unwrap_err().to_string();
        assert!(message.contains("CLOVE_RUNTIME_DIR"), "{message}");
    }

    #[test]
    fn runtime_dir_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| Utf8PathBuf::from(*value))
            }
        };
        let all = env(&[
            ("CLOVE_RUNTIME_DIR", "/explicit"),
            ("XDG_RUNTIME_DIR", "/run/user/7"),
            ("TMPDIR", "/var/tmp"),
        ]);
        assert_eq!(resolve_runtime_dir(all, 7), Utf8PathBuf::from("/explicit"));
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/7"), ("TMPDIR", "/var/tmp")]);
        assert_eq!(
            resolve_runtime_dir(xdg, 7),
            Utf8PathBuf::from("/run/user/7/clove")
        );
        let tmpdir = env(&[("TMPDIR", "/var/tmp")]);
        assert_eq!(
            resolve_runtime_dir(tmpdir, 7),
            Utf8PathBuf::from("/var/tmp/clove-7")
        );
        assert_eq!(
            resolve_runtime_dir(env(&[]), 7),
            Utf8PathBuf::from("/tmp/clove-7")
        );
    }

    #[test]
    fn over_long_socket_path_names_the_limit_and_the_fix() {
        let long = Utf8PathBuf::from(format!("/{}.sock", "x".repeat(SUN_PATH_MAX)));
        let message = check_sock_len(&long).unwrap_err().to_string();
        assert!(message.contains(&SUN_PATH_MAX.to_string()), "{message}");
        assert!(message.contains("CLOVE_RUNTIME_DIR"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn runtime_dir_must_not_be_writable_by_others() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(runtime_dir_is_private(path));
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!runtime_dir_is_private(path), "world-writable dir trusted");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(!runtime_dir_is_private(path), "group-writable dir trusted");
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
