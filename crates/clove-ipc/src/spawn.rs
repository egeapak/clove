//! Locating and spawning the `cloved` daemon, and an `ensure_daemon` helper that
//! probes-or-starts one. Shared by `clove daemon start` and the MCP server's
//! auto-start (topology B), so the spawn semantics are defined in exactly one
//! place.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use camino::Utf8Path;

use crate::{pid_path, DaemonClient};

/// How long [`ensure_daemon`] waits for a freshly-spawned daemon to become ready
/// (its pid file appears only after socket bind + startup sweep).
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

/// Spawn a detached `cloved run --clove-dir <dir>` for this repository. Returns
/// once spawned (not once ready — use [`ensure_daemon`] to wait for readiness).
pub fn spawn_daemon(clove_dir: &Utf8Path) -> std::io::Result<()> {
    let bin = cloved_path()?;
    spawn_detached(&bin, clove_dir)
}

/// Return a live daemon client for `clove_dir`, starting `cloved` if none is
/// running and waiting (up to [`READY_TIMEOUT`]) for it to become ready. Returns
/// `None` if no daemon could be reached or started — callers then fall back to
/// direct file access, so this never hard-fails.
pub fn ensure_daemon(clove_dir: &Utf8Path) -> Option<DaemonClient> {
    if let Some(client) = DaemonClient::probe(clove_dir) {
        return Some(client);
    }
    if spawn_daemon(clove_dir).is_err() {
        return None;
    }
    let pid_file = pid_path(clove_dir);
    let start = Instant::now();
    while start.elapsed() < READY_TIMEOUT {
        if pid_file.exists() {
            if let Some(client) = DaemonClient::probe(clove_dir) {
                return Some(client);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Spawn `cloved run --clove-dir <dir>` detached from this process and terminal.
#[cfg(unix)]
fn spawn_detached(bin: &Path, clove_dir: &Utf8Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(bin);
    cmd.arg("run")
        .arg("--clove-dir")
        .arg(clove_dir.as_str())
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
fn spawn_detached(bin: &Path, clove_dir: &Utf8Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new(bin)
        .arg("run")
        .arg("--clove-dir")
        .arg(clove_dir.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_child| ())
}
