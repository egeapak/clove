//! Locating and spawning the `cloved` hub, and an `ensure_daemon` helper that
//! attaches to it — starting it first if needed. Shared by `clove daemon start`
//! and the MCP server's auto-start (topology B), so the spawn semantics are
//! defined in exactly one place.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use camino::Utf8Path;

use crate::client::{cleanup_stale_hub, ClientError};
use crate::hub::{codes, HubPaths};
use crate::DaemonClient;

/// How long [`ensure_daemon`] keeps trying to reach a serving hub — spawning
/// one, or waiting out one that is exiting — before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait before spawning again when no hub has come up.
const RESPAWN_INTERVAL: Duration = Duration::from_millis(500);

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
    // The hub runs from its runtime directory, which must exist and be private
    // before anything is spawned into it.
    hub.preflight()?;
    spawn_detached(&bin, hub)
}

/// A client for `clove_dir` on the user's hub, loading the project — and
/// starting the hub — if needed.
pub fn ensure_daemon(clove_dir: &Utf8Path) -> Option<DaemonClient> {
    ensure_daemon_at(&HubPaths::resolve().ok()?, clove_dir).ok()
}

/// [`ensure_daemon`] against an explicit hub, reporting why it failed. Callers
/// fall back to direct file access on an error, so this never has to succeed.
///
/// The hub's lock, not its socket, says whether one is alive: a hub takes the
/// lock before it binds and releases it after it has unbound, so a hub that is
/// starting or on its way out holds it while its socket says nothing useful. A
/// hub that has decided to exit refuses new projects (`SHUTTING_DOWN`); this
/// waits for it to go and then starts another.
pub fn ensure_daemon_at(hub: &HubPaths, clove_dir: &Utf8Path) -> Result<DaemonClient, ClientError> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_spawn: Option<Instant> = None;
    let mut last_error = ClientError::Timeout;
    loop {
        if hub.footprint_present() {
            match DaemonClient::attach(hub, clove_dir, true) {
                Ok(client) => return Ok(client),
                Err(e) if e.refusal_code() == Some(codes::SHUTTING_DOWN) => last_error = e,
                // Refused before a socket is bound, or after it is unbound; or
                // cut off by a hub on its way out.
                Err(e @ (ClientError::Connect(_) | ClientError::Transport(_))) => last_error = e,
                // Alive and answering, but not for this project (locked by an
                // older daemon, unloadable, another protocol): spawning cannot
                // fix that.
                Err(other) => return Err(other),
            }
        }
        if !hub.running() && last_spawn.is_none_or(|t| t.elapsed() >= RESPAWN_INTERVAL) {
            // No process holds the lock: whatever is on disk is a corpse.
            cleanup_stale_hub(hub);
            spawn_hub(hub).map_err(ClientError::Connect)?;
            last_spawn = Some(Instant::now());
        }
        if Instant::now() >= deadline {
            return Err(match last_error {
                ClientError::Connect(_) => ClientError::Timeout,
                other => other,
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The variables a spawned hub keeps from its spawner. The hub serves every
/// project of the user and belongs to none of them, so it must not inherit one
/// client's shell — its secrets (`GITHUB_TOKEN`), its locale quirks, its
/// working directory. What it keeps is what it needs to find its runtime
/// directory, run tools, and honour the test knobs.
fn keep_var(name: &str) -> bool {
    #[cfg(windows)]
    let name = name.to_ascii_uppercase();
    #[cfg(windows)]
    let name = name.as_str();
    const KEEP: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "XDG_DATA_HOME",
        // Where `gh auth token` finds a non-default GitHub CLI config.
        "XDG_CONFIG_HOME",
        "GH_CONFIG_DIR",
        "CLOVE_HOME",
        "CLOVE_GITHUB_API_URL",
        "CLOVE_GITHUB_RETRY_MS",
        "LANG",
        // Windows.
        "SYSTEMROOT",
        "WINDIR",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
        "TEMP",
        "TMP",
        "PATHEXT",
    ];
    KEEP.contains(&name) || name.starts_with("LC_") || name.starts_with("CLOVED_")
}

/// Spawn `cloved run` detached from this process and terminal. The runtime
/// directory is passed explicitly so the hub binds where this client looks.
#[cfg(unix)]
fn spawn_detached(bin: &Path, hub: &HubPaths) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;

    let mut cmd = hub_command(bin, hub);
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

/// `cloved run` — no project, the scrubbed environment, and the runtime
/// directory as both its working directory and `CLOVE_RUNTIME_DIR`, so the hub
/// binds where this client looks and inherits nothing of the client's place.
/// Its stderr goes to [`HubPaths::log`] (nowhere, if that cannot be opened).
fn hub_command(bin: &Path, hub: &HubPaths) -> std::process::Command {
    use std::process::{Command, Stdio};
    let stderr = open_hub_log(hub).map_or_else(|_| Stdio::null(), Stdio::from);
    let mut cmd = Command::new(bin);
    cmd.arg("run")
        .current_dir(hub.dir().as_std_path())
        .env_clear()
        .envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(keep_var)))
        .env("CLOVE_RUNTIME_DIR", hub.dir().as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    // The hub works from its runtime directory: a relative clove home would
    // name another place there, and every token check would fail.
    if let Some(home) = std::env::var_os("CLOVE_HOME").filter(|home| !home.is_empty()) {
        if let Ok(home) = std::path::absolute(&home) {
            cmd.env("CLOVE_HOME", home);
        }
    }
    cmd
}

/// A log past this size is set aside (as `hub.log.1`) when a hub starts.
const HUB_LOG_LIMIT: u64 = 1024 * 1024;

/// Open the hub's log for appending: owner-only, never through a symlink,
/// and set aside first once it has grown past [`HUB_LOG_LIMIT`] — so it is
/// bounded at two files of about that size.
fn open_hub_log(hub: &HubPaths) -> std::io::Result<std::fs::File> {
    let log = hub.log();
    if std::fs::symlink_metadata(&log)
        .is_ok_and(|meta| meta.is_file() && meta.len() > HUB_LOG_LIMIT)
    {
        // Renaming replaces whatever is at `hub.log.1` itself, a link included,
        // without following it.
        let _ = std::fs::rename(&log, hub.dir().join("hub.log.1"));
    }
    clove_core::fs_safe::open_private_log(&log)
}

#[cfg(unix)]
extern "C" {
    #[link_name = "setsid"]
    fn libc_setsid() -> i32;
}

#[cfg(windows)]
fn spawn_detached(bin: &Path, hub: &HubPaths) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = hub_command(bin, hub);
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_child| ())
}

#[cfg(test)]
mod tests {
    use super::keep_var;

    #[test]
    fn the_hub_keeps_only_what_it_needs_from_its_spawner() {
        // `gh auth token` (the daemon's GitHub sync) looks in XDG_CONFIG_HOME
        // or GH_CONFIG_DIR for a non-default gh config.
        for kept in [
            "PATH",
            "HOME",
            "CLOVED_WEB_PORT",
            "LC_ALL",
            "XDG_CONFIG_HOME",
            "GH_CONFIG_DIR",
        ] {
            assert!(keep_var(kept), "{kept}");
        }
        for dropped in [
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "PWD",
            "CLOVE_FORMAT",
        ] {
            assert!(!keep_var(dropped), "{dropped}");
        }
    }
}
