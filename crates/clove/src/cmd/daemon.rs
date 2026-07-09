//! `clove daemon <start|stop|status>` (T-D05, DESIGN §7.2/§8).
//!
//! The daemon is an optional accelerator: it watches `.clove/issues/` and keeps
//! the index hot, but every read command works identically without it. `start`
//! spawns the sibling `cloved` binary detached and waits for its pid (readiness);
//! `stop` signals it and waits for teardown; `status` queries it over IPC.

use std::time::{Duration, Instant};

use camino::Utf8Path;
use clove_core::OutputFormat;
use clove_ipc::{pid_path, DaemonClient};
use clove_types::CloveError;
use serde_json::json;

use crate::cli::DaemonAction;
use crate::context::Ctx;
use crate::exit::ExitCode;
use crate::output::print_json_success;

/// How long `start`/`stop` wait for the pid file to appear/disappear.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(ctx: &Ctx, format: OutputFormat, action: DaemonAction) -> Result<ExitCode, CloveError> {
    let clove_dir = ctx
        .issues_dir
        .parent()
        .ok_or_else(|| daemon_err("cannot locate .clove directory"))?
        .to_owned();
    match action {
        DaemonAction::Start => start(&clove_dir, format),
        DaemonAction::Stop => stop(&clove_dir, format),
        DaemonAction::Status => status(&clove_dir, format),
    }
}

/// Start a detached `cloved` for this repository.
fn start(clove_dir: &Utf8Path, format: OutputFormat) -> Result<ExitCode, CloveError> {
    // Idempotent: a live daemon means we are already done.
    if DaemonClient::probe(clove_dir).is_some() {
        return emit(
            format,
            json!({ "started": false, "running": true }),
            &format!("daemon already running for {clove_dir}"),
        );
    }

    clove_ipc::spawn_daemon(clove_dir).map_err(|e| daemon_err(&format!("spawning cloved: {e}")))?;

    // Wait for readiness. The pid file appears after bind + the startup sweep, but
    // the daemon only *answers IPC* once its accept loop is actually polling — a
    // window a slow runner (or extra startup work) can widen. Gate readiness on a
    // real probe round-trip, not just the pid file's existence, so a status/read
    // issued right after `start` returns is guaranteed to connect; then read the
    // pid to report it.
    let pid_file = pid_path(clove_dir);
    let start = Instant::now();
    while start.elapsed() < WAIT_TIMEOUT {
        if DaemonClient::probe(clove_dir).is_some() {
            let pid = std::fs::read_to_string(&pid_file)
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            return emit(
                format,
                json!({ "started": true, "pid": pid }),
                &format!("daemon started (pid {pid})"),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(daemon_err("daemon did not become ready within 5s"))
}

/// Stop a running daemon and wait for it to tear down.
fn stop(clove_dir: &Utf8Path, format: OutputFormat) -> Result<ExitCode, CloveError> {
    let pid_file = pid_path(clove_dir);
    let pid = match std::fs::read_to_string(&pid_file) {
        Ok(s) => s.trim().parse::<u32>().ok(),
        Err(_) => None,
    };
    let Some(pid) = pid else {
        // Nothing to stop. Clean up any stray socket and report a no-op.
        clove_ipc::client::cleanup_stale(clove_dir);
        return emit(
            format,
            json!({ "stopped": false, "running": false }),
            "no daemon running",
        );
    };

    // Verify a live daemon actually answers on this repo's socket before
    // signalling the pid. A `daemon.pid` can outlive its daemon (SIGKILL / power
    // loss skip the clean-shutdown removal), and after OS pid recycling that
    // number may name an unrelated same-user process — SIGTERM-ing it blindly
    // would kill the wrong process and then hang waiting for a pid file nothing
    // removes. `probe` connects over IPC and only succeeds against our daemon
    // (D-daemon-6). If nothing answers, treat it as already stopped.
    //
    // Don't clean up the footprint here: `probe` already removes it in the
    // provably-dead (connection-refused) case, and deliberately preserves it when
    // the daemon may be alive-but-slow or on a mismatched protocol version. Doing
    // our own unconditional cleanup would unlink a *live* daemon's socket/pid,
    // orphaning it while we report "not running".
    if DaemonClient::probe(clove_dir).is_none() {
        return emit(
            format,
            json!({ "stopped": false, "running": false }),
            "no daemon running",
        );
    }

    signal_shutdown(clove_dir, pid)?;

    let start = Instant::now();
    while start.elapsed() < WAIT_TIMEOUT {
        if !pid_file.exists() {
            return emit(format, json!({ "stopped": true }), "daemon stopped");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(daemon_err("daemon did not stop within 5s"))
}

/// Query and print the running daemon's status.
fn status(clove_dir: &Utf8Path, format: OutputFormat) -> Result<ExitCode, CloveError> {
    let Some(mut client) = DaemonClient::probe(clove_dir) else {
        return emit(format, json!({ "running": false }), "daemon not running");
    };
    let status = client
        .status()
        .map_err(|e| daemon_err(&format!("status query failed: {e}")))?;

    let data = json!({
        "running": true,
        "uptime_s": status.uptime_s,
        "items_indexed": status.items_indexed,
        "watcher_state": status.watcher_state,
        "last_event_ms": status.last_event_ms,
        "batches_applied": status.batches_applied,
        "ping_count": status.ping_count,
        "last_ping_ms": status.last_ping_ms,
        "web_addr": status.web_addr,
    });
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(data, json!({})),
        OutputFormat::Human => {
            let web = status
                .web_addr
                .as_deref()
                .map(|a| format!("  web http://{a}"))
                .unwrap_or_default();
            println!(
                "running  uptime {}s  items {}  watcher {}  batches {}  pings {}{web}",
                status.uptime_s,
                status.items_indexed,
                status.watcher_state,
                status.batches_applied,
                status.ping_count,
            );
        }
    }
    Ok(ExitCode::Success)
}

/// Emit a small success envelope (`json`) or a one-line message (`human`).
fn emit(
    format: OutputFormat,
    data: serde_json::Value,
    human: &str,
) -> Result<ExitCode, CloveError> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(data, json!({})),
        OutputFormat::Human => println!("{human}"),
    }
    Ok(ExitCode::Success)
}

fn daemon_err(msg: &str) -> CloveError {
    CloveError::Io {
        path: camino::Utf8PathBuf::from("daemon"),
        source: std::io::Error::other(msg.to_owned()),
    }
}

/// Signal the daemon to shut down: SIGTERM (Unix) / named event (Windows).
#[cfg(unix)]
fn signal_shutdown(_clove_dir: &Utf8Path, pid: u32) -> Result<(), CloveError> {
    // SAFETY: kill(2) with a parsed pid and SIGTERM (15).
    let rc = unsafe { libc_kill(pid as i32, 15) };
    if rc == -1 {
        let err = std::io::Error::last_os_error();
        // ESRCH (no such process): treat as already-stopped.
        if err.raw_os_error() != Some(3) {
            return Err(daemon_err(&format!("sending SIGTERM: {err}")));
        }
    }
    Ok(())
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(windows)]
fn signal_shutdown(clove_dir: &Utf8Path, _pid: u32) -> Result<(), CloveError> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    let name = clove_ipc::event_name(clove_dir);
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: open the daemon's named shutdown event and signal it.
    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr());
        if handle.is_null() {
            return Err(daemon_err("daemon shutdown event not found"));
        }
        SetEvent(handle);
        CloseHandle(handle);
    }
    Ok(())
}
