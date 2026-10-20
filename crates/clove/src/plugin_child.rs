//! Running a plugin as a child process and exiting the way it exited
//! (`PLUGIN_SYSTEM.md` §6.1).
//!
//! The host used to `exec` the plugin on Unix, replacing its own image. It now
//! spawns the plugin and waits for it on every platform, so the host stays alive
//! to serve the plugin's IPC channel (§6.6). To the caller (a shell, a script,
//! `timeout`, an agent) the pair must still behave like one process:
//!
//! - **stdio** is inherited: the plugin gets the real terminal, unbuffered.
//! - **Signals** are caught by the host with a handler, never `SIG_IGN`: a
//!   *caught* signal resets to the default on exec, so the plugin keeps the
//!   default behavior, while `SIG_IGN` would be inherited and make the plugin
//!   uninterruptible. A disposition that is already `SIG_IGN` (`nohup`, a
//!   background job) is left alone, as under exec.
//!   - **Ctrl-C / Ctrl-\\ / hang-up**: the terminal signals the whole
//!     foreground process group, so the plugin receives them directly; the
//!     host only must not die first.
//!   - **Sent by another process to the host pid** (`kill`, `timeout`, a
//!     service manager): forwarded to the plugin, which no longer shares that
//!     pid. On Linux this covers SIGINT/SIGQUIT/SIGTERM/SIGHUP (`si_code`
//!     tells a `kill` from a terminal). Elsewhere only SIGTERM/SIGHUP are
//!     forwarded, so Ctrl-C is never delivered twice.
//!   - A signal sent to the whole process group by a process (`kill -TERM
//!     -<pgid>`, `timeout` without `--foreground`) reaches the plugin twice:
//!     once directly and once forwarded.
//!   - Forwarding stops once the plugin has exited and before it is reaped,
//!     so a recycled pid is never signalled.
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
            cleanup();
            return CloveError::Io {
                path: path.to_owned(),
                source,
            };
        }
    };
    imp::child_started(child.id());
    // Wait without reaping, then stop forwarding, then reap: a signal that
    // arrives after the child is gone must never reach a recycled pid.
    imp::wait_exited(child.id());
    imp::child_finished();
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

    /// The plugin's pid once spawned; 0 before; -1 once it has exited (stop
    /// forwarding: the pid may be recycled).
    static CHILD: AtomicI32 = AtomicI32::new(0);
    /// A forwardable signal that arrived before the child existed.
    static PENDING: AtomicI32 = AtomicI32::new(0);

    /// The signals the host catches while the plugin runs. A terminal sends
    /// SIGINT/SIGQUIT (Ctrl-C, Ctrl-\\) and SIGHUP (hang-up) to the whole
    /// foreground process group, so the plugin receives those directly; a
    /// signal another process sent to the host pid alone (`kill`, `timeout`, a
    /// service manager) is forwarded — see [`sent_by_a_process`].
    const CAUGHT: [libc::c_int; 4] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP];

    // `si_code` of a signal sent with kill(2) / sigqueue(3). libc does not
    // export these for Apple targets.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const FROM_PROCESS: [libc::c_int; 2] = [0, -1]; // SI_USER, SI_QUEUE
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const FROM_PROCESS: [libc::c_int; 2] = [0x10001, 0x10002]; // SI_USER, SI_QUEUE

    /// Whether `sig` should be forwarded, judged from its siginfo.
    ///
    /// Linux marks terminal-generated signals `SI_KERNEL`, so anything sent by
    /// a process is forwarded and terminal signals (already delivered to the
    /// plugin) are not. Other systems do not mark terminal signals reliably,
    /// so there only SIGTERM/SIGHUP are forwarded (Ctrl-C is never doubled; a
    /// `kill -INT <host pid>` is absorbed rather than forwarded).
    fn sent_by_a_process(sig: libc::c_int, info: *const libc::siginfo_t) -> bool {
        if cfg!(any(target_os = "linux", target_os = "android")) {
            // SAFETY: the kernel passes a valid siginfo with SA_SIGINFO.
            let code = unsafe { (*info).si_code };
            FROM_PROCESS.contains(&code)
        } else {
            matches!(sig, libc::SIGTERM | libc::SIGHUP)
        }
    }

    extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
        // Async-signal-safe: atomics and kill(2) only.
        if info.is_null() || !sent_by_a_process(sig, info) {
            return;
        }
        let pid = CHILD.load(Ordering::SeqCst);
        if pid > 0 {
            // SAFETY: CHILD is positive only between spawn and the child's
            // exit (before it is reaped), so the pid is still ours.
            unsafe {
                libc::kill(pid, sig);
            }
        } else if pid == 0 {
            PENDING.store(sig, Ordering::SeqCst);
            // The child may have been published after the load above; if so,
            // deliver the pending signal ourselves (whoever swaps it sends it).
            let pid = CHILD.load(Ordering::SeqCst);
            if pid > 0 {
                let pending = PENDING.swap(0, Ordering::SeqCst);
                if pending != 0 {
                    // SAFETY: as above.
                    unsafe {
                        libc::kill(pid, pending);
                    }
                }
            }
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
            for &sig in CAUGHT.iter() {
                if let Some(old) = set_handler(sig) {
                    previous.push((sig, old));
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

    /// Install [`on_signal`] for `sig` unless the signal is already ignored.
    /// A caught signal resets to its default on exec, so the plugin keeps the
    /// default behavior. Returns the previous disposition when it was replaced.
    fn set_handler(sig: libc::c_int) -> Option<libc::sigaction> {
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
            new.sa_sigaction = on_signal
                as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)
                as libc::sighandler_t;
            new.sa_flags = libc::SA_RESTART | libc::SA_SIGINFO;
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
            // SAFETY: kill on the child we just spawned and have not reaped.
            unsafe {
                libc::kill(pid as i32, pending);
            }
        }
    }

    /// Block until the child has exited, leaving it unreaped (`WNOWAIT`) so
    /// its pid cannot be recycled yet.
    pub fn wait_exited(pid: u32) {
        loop {
            // SAFETY: waitid with a zeroed out-parameter; WNOWAIT keeps the
            // child waitable for the `Child::wait` that follows.
            let rc = unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if rc == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                return;
            }
        }
    }

    /// Stop forwarding signals: the child has exited.
    pub fn child_finished() {
        CHILD.store(-1, Ordering::SeqCst);
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

    pub fn wait_exited(_pid: u32) {}

    pub fn child_finished() {}

    /// Exit with the child's code (NTSTATUS values pass through as negative
    /// i32 and are preserved by `process::exit`).
    pub fn exit_like(status: ExitStatus) -> ! {
        std::process::exit(status.code().unwrap_or(1));
    }
}
