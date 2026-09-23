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
//!   probes liveness and cleans up a stale socket (DESIGN §8.3).
//! - path/name helpers for the socket, pid, and lock files (DESIGN §8.2).

pub mod client;
pub mod protocol;
pub mod service;
pub mod spawn;
pub mod transport;

use camino::{Utf8Path, Utf8PathBuf};

pub use client::{cleanup_stale, ClientError, DaemonClient, DaemonHealth};
pub use protocol::{
    GraphRequest, GraphResponse, LeanRow, QueryKind, QueryListResponse, QueryRequest, ReindexDone,
    StatusResponse, PROTOCOL_VERSION,
};
pub use service::{CloveRpc, CloveRpcClient, RpcError};
pub use spawn::{cloved_path, ensure_daemon, spawn_daemon};
pub use transport::build_transport;

/// The Unix socket filename clove 0.1.0 used inside `.clove/`. Sockets now live
/// in the per-user [`runtime_dir`]; the name is kept so leftovers are git-ignored.
pub const SOCK_FILE: &str = "daemon.sock";
/// The daemon PID filename inside `.clove/` (DESIGN §8.2).
pub const PID_FILE: &str = "daemon.pid";
/// The daemon single-instance lock filename inside `.clove/` (DESIGN §8.2).
pub const LOCK_FILE: &str = "daemon.lock";

/// Path to the Unix domain socket for this `.clove/` directory: a short name in
/// the per-user [`runtime_dir`], not a file inside `.clove/`, because a socket
/// path is capped at [`SUN_PATH_MAX`] bytes and a deeply nested repository would
/// exceed it (DESIGN §8.2). Keyed by the canonical path so every spelling of the
/// same repository (symlinks, `/tmp` vs `/private/tmp`) reaches one daemon.
pub fn sock_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    let canonical = clove_dir
        .canonicalize_utf8()
        .unwrap_or_else(|_| clove_dir.to_owned());
    runtime_dir().join(format!("{}.sock", repo_hash(&canonical)))
}

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
    if !dir.exists() {
        create_private_dir(&dir)?;
    }
    if !runtime_dir_is_private(&dir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "daemon runtime directory {dir} must be owned by the current user and \
                 not writable by others (set CLOVE_RUNTIME_DIR to use another directory)"
            ),
        ));
    }
    Ok(dir)
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

/// Check, before spawning a daemon, that it will be able to bind: the runtime
/// directory exists and is private, and the socket path fits [`SUN_PATH_MAX`].
/// The error names the actual cause, where a failed spawn would only time out.
pub fn preflight(clove_dir: &Utf8Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    ensure_runtime_dir()?;
    socket_name(clove_dir).map(|_| ())
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
        let path = sock_path(clove_dir);
        check_sock_len(&path)?;
        path.into_string().to_fs_name::<GenericFilePath>()
    }
}

/// Reject a socket path the kernel would refuse, naming the cause and the fix.
fn check_sock_len(path: &Utf8Path) -> std::io::Result<()> {
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

/// The Windows named shutdown-event name for this `.clove/` directory
/// (DESIGN §8.9). `clove daemon stop` signals it; the daemon waits on it.
#[cfg(windows)]
pub fn event_name(clove_dir: &Utf8Path) -> String {
    format!("clove-shutdown-{}", repo_hash(clove_dir))
}

/// A short, stable hash of the `.clove/` directory path, used to derive the
/// Windows named-pipe name (`\\.\pipe\clove-<hash>`) and the Windows shutdown
/// event name (DESIGN §8.2/§8.9). Must be deterministic *across processes and
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

/// The Windows named-pipe name for this `.clove/` directory.
#[cfg(windows)]
pub fn pipe_name(clove_dir: &Utf8Path) -> String {
    format!("clove-{}", repo_hash(clove_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_and_lock_are_under_clove_dir() {
        let dir = Utf8Path::new("/repo/.clove");
        assert_eq!(pid_path(dir), Utf8PathBuf::from("/repo/.clove/daemon.pid"));
        assert_eq!(
            lock_path(dir),
            Utf8PathBuf::from("/repo/.clove/daemon.lock")
        );
    }

    #[test]
    fn socket_lives_in_the_runtime_dir_whatever_the_repo_depth() {
        let deep = Utf8PathBuf::from(format!("/{}/.clove", "nested-directory/".repeat(12)));
        let sock = sock_path(&deep);
        assert_eq!(sock.parent().unwrap(), runtime_dir());
        assert_eq!(
            sock.file_name().unwrap(),
            format!("{}.sock", repo_hash(&deep))
        );
        assert!(check_sock_len(&sock).is_ok(), "{sock} fits sun_path");
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
