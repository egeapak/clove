//! Running a plugin as a child process and exiting the way it exited
//! (`PLUGIN_SYSTEM.md` §6.1).
//!
//! The host used to `exec` the plugin on Unix, replacing its own image. It now
//! spawns the plugin and waits for it on every platform, so the host stays alive
//! to serve the plugin's IPC channel (§6.6). To the caller (a shell, a script,
//! `timeout`, an agent) the pair must still behave like one process:
//!
//! - **stdio** is inherited: the plugin gets the real terminal, unbuffered.
//! - **Ctrl-C / Ctrl-\\** reach the plugin directly (the terminal signals the
//!   whole foreground process group). The host only must not die first, so it
//!   catches SIGINT/SIGQUIT with a no-op handler. A *caught* signal resets to the
//!   default on exec, so the plugin keeps the default behavior; `SIG_IGN` would
//!   be inherited and make the plugin uninterruptible. A disposition that is
//!   already `SIG_IGN` (`nohup`, a background job) is left alone, as under exec.
//! - **SIGTERM / SIGHUP** sent to the host pid (`kill`, `timeout`, a service
//!   manager) are forwarded to the plugin, which no longer shares that pid.
//! - **Exit status**: the plugin's exit code becomes the host's; a plugin killed
//!   by a signal makes the host die by the same signal.
//!
//! On Windows the console sends Ctrl-C/Ctrl-Break to every attached process; the
//! host ignores them with a handler function (not `SetConsoleCtrlHandler(NULL,
//! TRUE)`, whose "ignore" flag the child would inherit) and propagates the
//! child's exit code.

use std::process::Command;

use camino::Utf8Path;
use clove_types::CloveError;

/// Spawn `cmd`, wait for it, run `cleanup`, and exit the host the way the child
/// exited. Returns only if the child could not be spawned.
pub fn run_and_exit(cmd: &mut Command, path: &Utf8Path, cleanup: impl FnOnce()) -> CloveError {
    let guard = imp::SignalGuard::install();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(source) => {
            drop(guard);
            return CloveError::Io {
                path: path.to_owned(),
                source,
            };
        }
    };
    imp::child_started(child.id());
    // `wait` retries on EINTR, so the no-op handlers do not cut it short.
    let status = child.wait();
    cleanup();
    match status {
        Ok(status) => imp::exit_like(status),
        Err(source) => CloveError::Io {
            path: path.to_owned(),
            source,
        },
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The plugin's pid once spawned; 0 before.
    static CHILD: AtomicI32 = AtomicI32::new(0);
    /// A SIGTERM/SIGHUP that arrived before the child existed.
    static PENDING: AtomicI32 = AtomicI32::new(0);

    /// Signals the host survives while the plugin runs (the terminal sends them
    /// to the plugin too).
    const ABSORB: [libc::c_int; 2] = [libc::SIGINT, libc::SIGQUIT];
    /// Signals aimed at the host pid alone, forwarded to the plugin.
    const FORWARD: [libc::c_int; 2] = [libc::SIGTERM, libc::SIGHUP];

    extern "C" fn absorb(_sig: libc::c_int) {}

    extern "C" fn forward(sig: libc::c_int) {
        // Async-signal-safe: an atomic load and kill(2).
        let pid = CHILD.load(Ordering::SeqCst);
        if pid > 0 {
            // SAFETY: kill is async-signal-safe; a stale pid is impossible
            // because the host reaps the child only after it stops forwarding.
            unsafe {
                libc::kill(pid, sig);
            }
        } else {
            PENDING.store(sig, Ordering::SeqCst);
        }
    }

    /// The handlers installed for the plugin's lifetime. Dropping it (only on a
    /// spawn failure) restores the previous dispositions.
    pub struct SignalGuard {
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl SignalGuard {
        pub fn install() -> SignalGuard {
            let mut previous = Vec::new();
            for (signals, handler) in [
                (&ABSORB, absorb as extern "C" fn(libc::c_int)),
                (&FORWARD, forward as extern "C" fn(libc::c_int)),
            ] {
                for &sig in signals.iter() {
                    if let Some(old) = set_handler(sig, handler as libc::sighandler_t) {
                        previous.push((sig, old));
                    }
                }
            }
            SignalGuard { previous }
        }
    }

    impl Drop for SignalGuard {
        fn drop(&mut self) {
            for (sig, old) in &self.previous {
                // SAFETY: restoring a disposition read back from sigaction.
                unsafe {
                    libc::sigaction(*sig, old, std::ptr::null_mut());
                }
            }
        }
    }

    /// Install `handler` for `sig` unless the signal is already ignored.
    /// Returns the previous disposition when it was replaced.
    fn set_handler(sig: libc::c_int, handler: libc::sighandler_t) -> Option<libc::sigaction> {
        // SAFETY: plain sigaction calls with zeroed, fully initialized structs.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut old) != 0 {
                return None;
            }
            if old.sa_sigaction == libc::SIG_IGN {
                return None; // inherited "ignore": keep it, the child inherits it too
            }
            let mut new: libc::sigaction = std::mem::zeroed();
            new.sa_sigaction = handler;
            new.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut new.sa_mask);
            if libc::sigaction(sig, &new, std::ptr::null_mut()) != 0 {
                return None;
            }
            Some(old)
        }
    }

    pub fn child_started(pid: u32) {
        CHILD.store(pid as i32, Ordering::SeqCst);
        let pending = PENDING.swap(0, Ordering::SeqCst);
        if pending != 0 {
            // SAFETY: kill on the child we just spawned.
            unsafe {
                libc::kill(pid as i32, pending);
            }
        }
    }

    /// Exit the host the way the child exited.
    pub fn exit_like(status: ExitStatus) -> ! {
        if let Some(code) = status.code() {
            std::process::exit(code);
        }
        if let Some(sig) = status.signal() {
            // SAFETY: resetting a disposition and re-raising a signal on
            // ourselves; nothing else runs after this point.
            unsafe {
                // The plugin already dumped core (or chose not to); the host
                // must not leave a second core when it re-raises.
                let none = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_CORE, &none);
                libc::signal(sig, libc::SIG_DFL);
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, sig);
                libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                libc::raise(sig);
            }
            std::process::exit(128 + sig);
        }
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod imp {
    use std::process::ExitStatus;

    use windows_sys::Win32::Foundation::{BOOL, FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    };

    /// Ctrl-C/Ctrl-Break go to every process on the console, the plugin
    /// included; the host survives them and waits. Close/logoff/shutdown fall
    /// through to the default handler.
    unsafe extern "system" fn ignore_interrupts(event: u32) -> BOOL {
        if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
            TRUE
        } else {
            FALSE
        }
    }

    pub struct SignalGuard;

    impl SignalGuard {
        pub fn install() -> SignalGuard {
            // SAFETY: registering a handler function; never NULL, so the
            // inheritable "ignore Ctrl-C" flag is not set for the child.
            unsafe {
                SetConsoleCtrlHandler(Some(ignore_interrupts), TRUE);
            }
            SignalGuard
        }
    }

    impl Drop for SignalGuard {
        fn drop(&mut self) {
            // SAFETY: removing the handler registered above.
            unsafe {
                SetConsoleCtrlHandler(Some(ignore_interrupts), FALSE);
            }
        }
    }

    pub fn child_started(_pid: u32) {}

    /// Exit with the child's code (NTSTATUS values pass through as negative
    /// i32 and are preserved by `process::exit`).
    pub fn exit_like(status: ExitStatus) -> ! {
        std::process::exit(status.code().unwrap_or(1));
    }
}
