//! Locating and spawning the `cloved` hub, and an `ensure_daemon` helper that
//! attaches to it — starting it first if needed. Shared by `clove daemon start`
//! and the MCP server's auto-start (topology B), so the spawn semantics are
//! defined in exactly one place.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use camino::Utf8Path;

use crate::client::ClientError;
use crate::hub::HubPaths;
use crate::DaemonClient;

/// How long [`ensure_daemon`] waits for a freshly-spawned hub to become ready
/// (its pid file appears only after the socket is bound).
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Locate the `cloved` binary next to the running executable (the install layout,
/// and the cargo target dir in tests). `CLOVED_PATH` overrides it (tests / unusual
/// installs).
pub fn cloved_path() -> std::io::Result<PathBuf> {
    if let Ok(p) = std::env::var("CLOVED_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("executable has no parent directory"))?;
    let name = if cfg!(windows) {
        "cloved.exe"
    } else {
        "cloved"
    };
    let path = dir.join(name);
    if path.exists() {
        Ok(path)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("cloved binary not found at {}", path.display()),
        ))
    }
}

/// Spawn a detached `cloved run` hub rooted at `hub`. Returns once spawned (not
/// once ready — use [`ensure_daemon`] to wait for readiness).
pub fn spawn_hub(hub: &HubPaths) -> std::io::Result<()> {
    let bin = cloved_path()?;
    spawn_detached(&bin, hub)
}

/// A client attached to `clove_dir` on the user's hub, loading the project —
/// and starting the hub — if needed.
pub fn ensure_daemon(clove_dir: &Utf8Path) -> Option<DaemonClient> {
    ensure_daemon_at(&HubPaths::resolve(), clove_dir).ok()
}

/// [`ensure_daemon`] against an explicit hub, reporting why it failed. Callers
/// fall back to direct file access on an error, so this never has to succeed.
pub fn ensure_daemon_at(hub: &HubPaths, clove_dir: &Utf8Path) -> Result<DaemonClient, ClientError> {
    if hub.footprint_present() {
        match DaemonClient::attach(hub, clove_dir, true) {
            // No listener behind the footprint: a crashed hub. Start another.
            Err(ClientError::Connect(_)) => {}
            // Attached — or a live hub that refused this project (locked by an
            // older daemon, unloadable). Spawning cannot fix either.
            other => return other,
        }
    }
    // A hub that cannot bind would only surface as the readiness timeout.
    hub.preflight().map_err(ClientError::Connect)?;
    spawn_hub(hub).map_err(ClientError::Connect)?;
    let start = Instant::now();
    while start.elapsed() < READY_TIMEOUT {
        if hub.pid().exists() {
            match DaemonClient::attach(hub, clove_dir, true) {
                // Readiness races a concurrent spawner's hub: the loser exits and
                // the winner's socket may not be there yet. Keep waiting.
                Err(ClientError::Connect(_)) => {}
                other => return other,
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ClientError::Timeout)
}

/// Spawn `cloved run` detached from this process and terminal. The runtime
/// directory is passed explicitly so the hub binds where this client looks.
#[cfg(unix)]
fn spawn_detached(bin: &Path, hub: &HubPaths) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(bin);
    cmd.arg("run")
        .env("CLOVE_RUNTIME_DIR", hub.dir().as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // New session leader → detached from the controlling terminal. The parent
    // exits after readiness, so the daemon reparents to init.
    unsafe {
        cmd.pre_exec(|| {
            // SAFETY: setsid in the forked child before exec; no allocation.
            if libc_setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map(|_child| ())
}

#[cfg(unix)]
extern "C" {
    #[link_name = "setsid"]
    fn libc_setsid() -> i32;
}

#[cfg(windows)]
fn spawn_detached(bin: &Path, hub: &HubPaths) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new(bin)
        .arg("run")
        .env("CLOVE_RUNTIME_DIR", hub.dir().as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_child| ())
}
