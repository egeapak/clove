//! Hub lifecycle (DESIGN §8.1/§8.2/§8.9): single-instance lock, socket bind,
//! pid-after-ready, signal-driven shutdown, and clean teardown.
//!
//! Ordering invariants:
//! - The `hub.lock` advisory flock is taken first; a second hub fails fast.
//! - `hub.pid` is written **only after** the socket is bound, so a reader that
//!   sees a pid is guaranteed a usable socket.
//! - The shutdown-signal handler is installed **before** the pid is written, so a
//!   SIGTERM racing the hub's readiness is caught (clean teardown) rather than
//!   hitting the kernel default disposition (abrupt kill, stale socket/pid).
//! - Shutdown tears every project down (each flushes its index), then removes the
//!   socket and pid, then releases the lock (DESIGN §8.9).

use std::io::Write;
use std::time::Duration;

use anyhow::Context;
use camino::Utf8Path;
use clove_ipc::HubPaths;
use interprocess::local_socket::ListenerOptions;

use crate::hub::Hub;

/// How long the hub lingers with no project before exiting, unless
/// `CLOVED_HUB_GRACE_MS` says otherwise.
const DEFAULT_GRACE: Duration = Duration::from_secs(60);

/// Run the hub rooted at `paths` until a shutdown signal arrives or it has
/// served nothing for its grace period. Blocks the calling thread (it owns the
/// Tokio runtime). Exits the process with a non-zero code if another hub
/// already holds the lock.
///
/// The hub belongs to no project and to no client: it works from its own
/// private runtime directory — never `/`, never the directory of whichever
/// client happened to start it — so nothing it does can depend on a place
/// someone else controls. A runtime directory that cannot be created or
/// validated fails the start.
pub fn run(paths: &HubPaths) -> anyhow::Result<()> {
    ensure_dir(paths).context("preparing the daemon runtime directory")?;
    std::env::set_current_dir(paths.dir().as_std_path())
        .with_context(|| format!("entering the runtime directory {}", paths.dir()))?;

    // 1. Single-instance advisory lock, held for the whole lifetime.
    let lock = clove_core::fs_safe::open_lock_file(&paths.lock())
        .with_context(|| format!("opening {}", paths.lock()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            eprintln!("cloved: a daemon is already running in {}", paths.dir());
            std::process::exit(1);
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(e).with_context(|| format!("locking {}", paths.lock()));
        }
    }

    // 2. Tokio runtime — 2 workers, per DESIGN §8.1. Every project's blocking
    //    work (loads, sweeps, queries) runs on the blocking pool.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    let result: anyhow::Result<()> = runtime.block_on(async {
        // A corpse socket from a crashed hub: safe to remove — we hold the lock.
        #[cfg(not(windows))]
        let _ = std::fs::remove_file(paths.sock());
        let name = paths.socket_name().context("building socket name")?;
        let listener = owner_only(ListenerOptions::new().name(name))
            .create_tokio()
            .with_context(|| format!("binding {}", paths.sock()))?;
        // The control socket is a mutating RPC channel for every project the
        // user has — restrict it to the owner (D-daemon-SEC-1).
        restrict_to_owner(&paths.sock(), 0o600);

        let hub = Hub::new(std::env::var_os("CLOVED_DISABLE_WEB").is_none(), grace());

        // Register the shutdown-signal handler BEFORE advertising readiness (the
        // pid file): "pid present ⇒ ready to shut down cleanly" (DESIGN §8.9).
        let mut shutdown = ShutdownSignal::install(paths);
        write_pid(paths).context("writing pid file")?;

        tokio::select! {
            _ = hub.accept_loop(listener) => {},
            _ = hub.idle_exit() => {},
            _ = orphaned(paths) => {},
            _ = shutdown.recv() => {},
            _ = hub.shutdown().cancelled() => {},
        }
        hub.shutdown().cancel();
        hub.unload_all().await;
        Ok(())
    });

    #[cfg(not(windows))]
    let _ = std::fs::remove_file(paths.sock());
    let _ = std::fs::remove_file(paths.pid());
    drop(runtime);
    result
}

/// Resolve once the hub's own files are gone — its runtime directory deleted
/// from under it — leaving it unreachable: no client can find it, and nothing
/// could ever stop it. It exits instead of lingering until idle.
async fn orphaned(paths: &HubPaths) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        #[cfg(not(windows))]
        let gone = !paths.pid().exists() || !paths.sock().exists();
        #[cfg(windows)]
        let gone = !paths.pid().exists();
        if gone {
            eprintln!("cloved: {} is gone; exiting", paths.dir());
            return;
        }
    }
}

/// The hub's grace period with no project (`CLOVED_HUB_GRACE_MS` for tests).
fn grace() -> Duration {
    std::env::var("CLOVED_HUB_GRACE_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .map_or(DEFAULT_GRACE, Duration::from_millis)
}

/// Create the runtime directory owner-only and refuse one another user can
/// write to (a planted socket would receive every project's RPCs).
fn ensure_dir(paths: &HubPaths) -> std::io::Result<()> {
    clove_ipc::ensure_private_dir(paths.dir())
}

/// On Windows, give the hub's pipe a protected DACL that admits only this
/// user's SID (the default grants read access to Everyone), so no other local
/// user can reach a channel that writes to every project. The SID is explicit
/// rather than `OW`, which maps to Administrators under an elevated token.
/// Unix gets the same from the socket's mode and the private runtime directory.
#[cfg(windows)]
fn owner_only(options: ListenerOptions<'_>) -> ListenerOptions<'_> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    let owner_only = clove_ipc::win::current_user_sid()
        .ok()
        .and_then(|sid| widestring::U16CString::from_str(format!("D:P(A;;GA;;;{sid})")).ok())
        .and_then(|sddl| SecurityDescriptor::deserialize(&sddl).ok());
    match owner_only {
        Some(sd) => options.security_descriptor(sd),
        None => {
            eprintln!("cloved: could not build an owner-only pipe descriptor; using the default");
            options
        }
    }
}

#[cfg(not(windows))]
fn owner_only(options: ListenerOptions<'_>) -> ListenerOptions<'_> {
    options
}

/// Restrict a runtime file to owner-only access (Unix). A no-op on other
/// platforms (Windows named pipes carry their own ACLs). Best-effort: a chmod
/// failure must not bring the daemon down (D-daemon-SEC-1).
#[cfg(unix)]
fn restrict_to_owner(path: &Utf8Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// No-op on non-Unix platforms.
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Utf8Path, _mode: u32) {}

/// Write the current process id to `hub.pid` (DESIGN §8.2). Written aside and
/// renamed into place, owner-only from the start: the pid file's appearance is
/// the readiness signal, so it must never be seen empty or world-readable.
fn write_pid(paths: &HubPaths) -> std::io::Result<()> {
    let staged = paths.dir().join("hub.pid.tmp");
    let mut file = clove_core::fs_safe::create_private_file(&staged)?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()?;
    std::fs::rename(&staged, paths.pid())
}

/// A shutdown-signal source whose OS handler is installed at construction, so it
/// can be set up *before* the pid file advertises readiness (DESIGN §8.9). Await
/// [`ShutdownSignal::recv`] to block until a shutdown signal arrives.
#[cfg(unix)]
enum ShutdownSignal {
    Signals {
        term: tokio::signal::unix::Signal,
        interrupt: tokio::signal::unix::Signal,
    },
    /// Registration failed — resolve immediately, matching the prior behaviour
    /// (an unusable signal handler shouldn't leave the daemon un-stoppable).
    Failed,
}

#[cfg(unix)]
impl ShutdownSignal {
    fn install(_paths: &HubPaths) -> Self {
        use tokio::signal::unix::{signal, SignalKind};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(term), Ok(interrupt)) => ShutdownSignal::Signals { term, interrupt },
            _ => ShutdownSignal::Failed,
        }
    }

    async fn recv(&mut self) {
        match self {
            ShutdownSignal::Signals { term, interrupt } => {
                tokio::select! {
                    _ = term.recv() => {},
                    _ = interrupt.recv() => {},
                }
            }
            ShutdownSignal::Failed => {}
        }
    }
}

/// Windows has no SIGTERM: wait on Ctrl-C (interactive) or the named shutdown
/// event that `clove daemon stop --all` signals (DESIGN §8.9).
#[cfg(windows)]
struct ShutdownSignal {
    event: String,
}

#[cfg(windows)]
impl ShutdownSignal {
    fn install(paths: &HubPaths) -> Self {
        ShutdownSignal {
            event: paths.event_name().unwrap_or_default(),
        }
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = wait_named_event(self.event.clone()) => {},
        }
    }
}

/// Block (off-runtime) on a named manual-reset Windows event until it is signaled.
#[cfg(windows)]
async fn wait_named_event(name: String) {
    let _ = tokio::task::spawn_blocking(move || {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: standard Win32 named-event create/wait/close. A null name attr
        // and a valid null-terminated UTF-16 name are passed.
        unsafe {
            let handle = CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr());
            if handle.is_null() {
                return;
            }
            WaitForSingleObject(handle, INFINITE);
            CloseHandle(handle);
        }
    })
    .await;
}
