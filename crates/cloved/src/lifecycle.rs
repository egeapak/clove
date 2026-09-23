//! Hub lifecycle (DESIGN §8.1/§8.2/§8.9): single-instance lock, socket bind,
//! pid-after-ready, signal-driven shutdown, and clean teardown.
//!
//! Ordering invariants:
//! - The `hub.lock` advisory flock is taken first; a second hub gives up once
//!   the lock has stayed taken for a second (a client's liveness poll holds it
//!   only for an instant).
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
    if !take_hub_lock(&lock).with_context(|| format!("locking {}", paths.lock()))? {
        eprintln!("cloved: a daemon is already running in {}", paths.dir());
        std::process::exit(1);
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
        let listener = owner_only(ListenerOptions::new().name(name))?
            .create_tokio()
            .with_context(|| format!("binding {}", paths.sock()))?;
        // The control socket is a mutating RPC channel for every project the
        // user has — restrict it to the owner (D-daemon-SEC-1).
        restrict_to_owner(&paths.sock(), 0o600);

        let hub = Hub::new(std::env::var_os("CLOVED_DISABLE_WEB").is_none(), grace())
            .with_load_delay(env_ms("CLOVED_LOAD_DELAY_MS").unwrap_or_default());

        // Register the shutdown-signal handler BEFORE advertising readiness (the
        // pid file): "pid present ⇒ ready to shut down cleanly" (DESIGN §8.9).
        let mut shutdown =
            ShutdownSignal::install(paths).context("installing the shutdown signal")?;
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

    // Every served project is torn down by now. What may remain is a load
    // still running on the blocking pool (a large project's open, holding its
    // lock): dropping the runtime would wait for it while `hub.pid` says the
    // hub is gone. Abandon it instead — the process exits right after, which
    // releases its locks — and remove the files only then, so "stopped" is
    // true when a client sees them go.
    runtime.shutdown_background();
    #[cfg(not(windows))]
    let _ = std::fs::remove_file(paths.sock());
    let _ = std::fs::remove_file(paths.pid());
    result
}

/// Take the hub lock exclusively; `false` when another hub holds it.
///
/// Clients learn whether a hub is alive by briefly taking the same lock shared
/// ([`HubPaths::running`]), so one refusal may be a poll in flight rather than
/// a hub. Only a lock that stays taken for about a second means a hub.
fn take_hub_lock(lock: &std::fs::File) -> std::io::Result<bool> {
    const PATIENCE: Duration = Duration::from_secs(1);
    let start = std::time::Instant::now();
    let mut backoff = Duration::from_millis(1);
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(true),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < PATIENCE => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
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
    env_ms("CLOVED_HUB_GRACE_MS").unwrap_or(DEFAULT_GRACE)
}

/// A millisecond test knob from the environment.
fn env_ms(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Create the runtime directory owner-only and refuse one another user can
/// write to (a planted socket would receive every project's RPCs).
fn ensure_dir(paths: &HubPaths) -> std::io::Result<()> {
    clove_ipc::ensure_private_dir(paths.dir())
}

/// On Windows, give the hub's pipe a protected DACL that admits only this
/// user's SID (the default grants read access to Everyone), so no other local
/// user can reach a channel that writes to every project, and make that SID
/// its owner — the owner is what a client checks. The SID is explicit rather
/// than `OW`/the token default, which mean Administrators under an elevated
/// token. Without such a descriptor the hub does not start.
/// Unix gets the same from the socket's mode and the private runtime directory.
#[cfg(windows)]
fn owner_only(options: ListenerOptions<'_>) -> anyhow::Result<ListenerOptions<'_>> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    let sid = clove_ipc::win::current_user_sid().context("reading this user's SID")?;
    let sddl = widestring::U16CString::from_str(format!("O:{sid}D:P(A;;GA;;;{sid})"))
        .context("building the pipe's security descriptor")?;
    let descriptor = SecurityDescriptor::deserialize(&sddl)
        .context("building the pipe's security descriptor")?;
    Ok(options.security_descriptor(descriptor))
}

#[cfg(not(windows))]
fn owner_only(options: ListenerOptions<'_>) -> anyhow::Result<ListenerOptions<'_>> {
    Ok(options)
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
    fn install(_paths: &HubPaths) -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(
            match (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
            ) {
                (Ok(term), Ok(interrupt)) => ShutdownSignal::Signals { term, interrupt },
                _ => ShutdownSignal::Failed,
            },
        )
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
/// event that `clove daemon stop --all` signals (DESIGN §8.9). The event is
/// created at install, before the pid file advertises readiness; a hub that
/// cannot create it would be one nothing can stop, so it does not start. It
/// is created owner-only, and one that already exists is refused: another
/// process that made it first could signal it — stopping this hub — at will.
#[cfg(windows)]
struct ShutdownSignal {
    event: NamedEvent,
}

/// An owned handle to the shutdown event.
#[cfg(windows)]
struct NamedEvent(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: an event handle may be waited on from any thread.
#[cfg(windows)]
unsafe impl Send for NamedEvent {}
#[cfg(windows)]
unsafe impl Sync for NamedEvent {}

#[cfg(windows)]
impl Drop for NamedEvent {
    fn drop(&mut self) {
        // SAFETY: the handle was created by CreateEventW and is closed once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
impl ShutdownSignal {
    fn install(paths: &HubPaths) -> std::io::Result<Self> {
        use windows_sys::Win32::Foundation::{
            CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS,
        };
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
        use windows_sys::Win32::System::Threading::CreateEventW;

        let name = paths.event_name()?;
        if name.is_empty() {
            return Err(std::io::Error::other("the shutdown event has no name"));
        }
        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
        let sid = clove_ipc::win::current_user_sid()?;
        let sddl = wide(&format!("O:{sid}D:P(A;;GA;;;{sid})"));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a valid NUL-terminated SDDL string; the descriptor is
        // LocalAlloc'd by the API and freed below.
        let built = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if built == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name = wide(&name);
        // SAFETY: owner-only security, manual reset, initially unsignaled, and
        // a valid NUL-terminated UTF-16 name. GetLastError is read at once.
        let (handle, existed) = unsafe {
            let handle = CreateEventW(&attributes, 1, 0, name.as_ptr());
            (handle, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        let error = std::io::Error::last_os_error();
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(descriptor) };
        if handle.is_null() {
            return Err(error);
        }
        if existed {
            // SAFETY: a handle this call opened.
            unsafe { CloseHandle(handle) };
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "the shutdown event already exists: some other process created it",
            ));
        }
        Ok(ShutdownSignal {
            event: NamedEvent(handle),
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = wait_signaled(&self.event) => {},
        }
    }
}

/// Resolve once `event` is signaled. Waits in short slices on the blocking
/// pool, so a hub exiting for another reason never leaves a thread blocked on
/// it for good.
#[cfg(windows)]
async fn wait_signaled(event: &NamedEvent) {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    loop {
        let handle = event.0 as usize;
        // SAFETY: the handle outlives this wait: `event` is borrowed across it.
        let waited = tokio::task::spawn_blocking(move || unsafe {
            WaitForSingleObject(handle as windows_sys::Win32::Foundation::HANDLE, 250)
        })
        .await;
        match waited {
            Ok(status) if status == WAIT_OBJECT_0 => return,
            Ok(_) => {}
            Err(_) => return,
        }
    }
}
