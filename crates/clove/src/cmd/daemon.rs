//! `clove daemon <start|stop|status>` (T-D05, DESIGN §7.2/§8).
//!
//! The daemon is an optional accelerator: one `cloved` per user (the hub) serves
//! every project that asks, watching each `.clove/issues/` and keeping its index
//! hot, but every read command works identically without it. `start` has the hub
//! serve this project (spawning the sibling `cloved` if none runs); `stop` has it
//! stop serving this project; `stop --all` stops the hub; `status` reports both.

use clove_plugin::outln;
use std::time::{Duration, Instant};

use camino::Utf8Path;
use clove_core::OutputFormat;
use clove_ipc::{ClientError, DaemonClient, HubClient, HubPaths};
use clove_types::CloveError;
use serde_json::{json, Value};

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
    let hub = HubPaths::resolve();
    match action {
        DaemonAction::Start => start(&hub, &clove_dir, format),
        DaemonAction::Stop { all: false } => stop(&hub, &clove_dir, format),
        DaemonAction::Stop { all: true } => stop_hub(&hub, format),
        DaemonAction::Status => status(&hub, &clove_dir, format),
    }
}

/// Have the hub serve this project, starting the hub if none runs.
fn start(
    hub: &HubPaths,
    clove_dir: &Utf8Path,
    format: OutputFormat,
) -> Result<ExitCode, CloveError> {
    // Idempotent: already served means we are already done.
    if DaemonClient::probe_at(hub, clove_dir).is_some() {
        return emit(
            format,
            json!({ "started": false, "running": true, "pid": hub.read_pid() }),
            &format!("daemon already serving {clove_dir}"),
        );
    }

    // The probe→spawn→attach semantics live in exactly one place — clove-ipc's
    // `ensure_daemon_at`, which the MCP auto-start also uses.
    match clove_ipc::ensure_daemon_at(hub, clove_dir) {
        Ok(_) => {}
        Err(ClientError::Refused { message, .. }) => {
            return Err(daemon_err(&match clove_ipc::legacy_daemon_pid(clove_dir) {
                Some(pid) => format!(
                    "a clove 0.1.0 daemon (pid {pid}) already serves this project; \
                     `clove daemon stop` stops it, then start again"
                ),
                None => format!("the daemon cannot serve this project: {message}"),
            }));
        }
        Err(ClientError::Connect(e) | ClientError::Name(e)) => {
            return Err(daemon_err(&format!("could not start the daemon: {e}")));
        }
        Err(_) => {
            return Err(daemon_err(
                "could not start the daemon (it did not become ready within 5s); \
                 is `cloved` installed next to `clove`?",
            ));
        }
    }

    let pid = hub.read_pid();
    emit(
        format,
        json!({ "started": true, "pid": pid }),
        &format!(
            "daemon serving {clove_dir} (pid {})",
            pid.map_or_else(|| "?".to_owned(), |p| p.to_string())
        ),
    )
}

/// Stop serving this project: detach it from the hub, or stop the clove 0.1.0
/// daemon that serves it.
fn stop(
    hub: &HubPaths,
    clove_dir: &Utf8Path,
    format: OutputFormat,
) -> Result<ExitCode, CloveError> {
    // A pre-hub daemon still running after an upgrade. `legacy_daemon_pid` only
    // answers when a clove daemon really listens on the project's old socket,
    // so the pid it left behind is safe to signal (D-daemon-6).
    if let Some(pid) = clove_ipc::legacy_daemon_pid(clove_dir) {
        signal_pid(pid)?;
        wait_gone(&clove_ipc::pid_path(clove_dir))?;
        return emit(
            format,
            json!({ "stopped": true, "legacy": true }),
            &format!("stopped the clove 0.1.0 daemon (pid {pid})"),
        );
    }
    // Anything left of one is a crashed daemon's corpse.
    clove_ipc::cleanup_legacy(clove_dir);

    let not_running = || {
        emit(
            format,
            json!({ "stopped": false, "running": false }),
            "no daemon serving this project",
        )
    };
    let mut control = match HubClient::connect(hub) {
        Ok(control) => control,
        Err(_) => return not_running(),
    };
    let detached = control
        .detach(clove_dir)
        .map_err(|e| daemon_err(&format!("detaching this project: {e}")))?;
    if !detached.detached {
        return not_running();
    }
    if detached.hub_exiting {
        wait_gone(&hub.pid())?;
    }
    emit(
        format,
        json!({ "stopped": true, "hub_stopped": detached.hub_exiting }),
        "daemon stopped serving this project",
    )
}

/// Stop the hub — every project it serves.
fn stop_hub(hub: &HubPaths, format: OutputFormat) -> Result<ExitCode, CloveError> {
    let not_running = || {
        emit(
            format,
            json!({ "stopped": false, "running": false }),
            "no daemon running",
        )
    };
    // Signal only a hub that proves itself alive: a leftover `hub.pid` can name
    // an unrelated process after pid reuse (D-daemon-6). An answer of any
    // protocol version is proof enough — that is exactly the old hub an upgrade
    // leaves behind.
    match HubClient::connect(hub) {
        Ok(_) | Err(ClientError::Refused { .. }) | Err(ClientError::Transport(_)) => {}
        Err(_) => return not_running(),
    }
    let Some(pid) = hub.read_pid() else {
        return not_running();
    };
    signal_hub(hub, pid)?;
    wait_gone(&hub.pid())?;
    emit(format, json!({ "stopped": true }), "daemon stopped")
}

/// Wait for a daemon's pid file to disappear (its last teardown step).
fn wait_gone(pid_file: &Utf8Path) -> Result<(), CloveError> {
    let start = Instant::now();
    while start.elapsed() < WAIT_TIMEOUT {
        if !pid_file.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(daemon_err("daemon did not stop within 5s"))
}

/// This project's daemon status, plus the hub serving it.
fn status(
    hub: &HubPaths,
    clove_dir: &Utf8Path,
    format: OutputFormat,
) -> Result<ExitCode, CloveError> {
    let hub_status = HubClient::connect(hub)
        .ok()
        .and_then(|mut c| c.status().ok());
    let project = match DaemonClient::probe_at(hub, clove_dir) {
        Some(mut client) => Some(
            client
                .status()
                .map_err(|e| daemon_err(&format!("status query failed: {e}")))?,
        ),
        None => None,
    };

    let hub_json = hub_status.as_ref().map(|h| {
        json!({
            "pid": h.pid,
            "uptime_s": h.uptime_s,
            "web_addr": h.web_addr,
            "projects": h.projects.iter().map(|p| json!({
                "clove_dir": p.clove_dir,
                "items_indexed": p.status.items_indexed,
                "watcher_state": p.status.watcher_state,
                "last_event_ms": p.status.last_event_ms,
                "web_url": p.status.web_url,
            })).collect::<Vec<Value>>(),
        })
    });
    let mut data = json!({ "running": project.is_some(), "hub": hub_json });
    if let Some(s) = &project {
        let fields = json!({
            "uptime_s": s.uptime_s,
            "items_indexed": s.items_indexed,
            "watcher_state": s.watcher_state,
            "last_event_ms": s.last_event_ms,
            "batches_applied": s.batches_applied,
            "ping_count": s.ping_count,
            "last_ping_ms": s.last_ping_ms,
            "web_addr": s.web_addr,
            "web_url": s.web_url,
        });
        if let (Some(data), Value::Object(fields)) = (data.as_object_mut(), fields) {
            data.extend(fields);
        }
    }

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(data, json!({})),
        OutputFormat::Human => {
            match &project {
                Some(s) => {
                    let web = s
                        .web_url
                        .as_deref()
                        .map(|url| format!("  web {url}"))
                        .unwrap_or_default();
                    outln!(
                        "running  uptime {}s  items {}  watcher {}  batches {}  pings {}{web}",
                        s.uptime_s,
                        s.items_indexed,
                        s.watcher_state,
                        s.batches_applied,
                        s.ping_count,
                    );
                }
                None if hub_status.is_some() => outln!("daemon not serving this project"),
                None => outln!("daemon not running"),
            }
            if let Some(h) = &hub_status {
                let web = h
                    .web_addr
                    .as_deref()
                    .map(|a| format!("  web http://{a}/"))
                    .unwrap_or_default();
                outln!(
                    "daemon pid {}  uptime {}s  projects {}{web}",
                    h.pid,
                    h.uptime_s,
                    h.projects.len()
                );
                for p in &h.projects {
                    outln!("  {}  items {}", p.clove_dir, p.status.items_indexed);
                }
            }
        }
    }
    Ok(ExitCode::Success)
}

/// Emit a small success envelope (`json`) or a one-line message (`human`).
fn emit(format: OutputFormat, data: Value, human: &str) -> Result<ExitCode, CloveError> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(data, json!({})),
        OutputFormat::Human => outln!("{human}"),
    }
    Ok(ExitCode::Success)
}

/// A daemon-communication failure.
///
/// These used to be reported as `CloveError::Io` against a fake `"daemon"`
/// path, which classified them as `IO_ERROR` / exit 5 — the code for a
/// filesystem problem. They are exactly what exit 7 (`DAEMON_ERROR`) is for;
/// it was published in the exit table from M0 but never actually produced.
fn daemon_err(msg: &str) -> CloveError {
    CloveError::Remote {
        code: "DAEMON_ERROR".to_owned(),
        exit: 7,
        message: msg.to_owned(),
    }
}

/// Signal the hub to shut down: SIGTERM (Unix) / its named event (Windows).
#[cfg(unix)]
fn signal_hub(_hub: &HubPaths, pid: u32) -> Result<(), CloveError> {
    signal_pid(pid)
}

#[cfg(windows)]
fn signal_hub(hub: &HubPaths, _pid: u32) -> Result<(), CloveError> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    let name = hub.event_name();
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: open the hub's named shutdown event and signal it.
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

/// SIGTERM a verified daemon pid.
#[cfg(unix)]
fn signal_pid(pid: u32) -> Result<(), CloveError> {
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

/// A clove 0.1.0 Windows daemon is never verified (see
/// `clove_ipc::legacy_daemon_pid`), so there is nothing to signal.
#[cfg(windows)]
fn signal_pid(_pid: u32) -> Result<(), CloveError> {
    Ok(())
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::daemon_err;

    /// Daemon-communication failures classify as `DAEMON_ERROR` / exit 7, not
    /// the `IO_ERROR` / exit 5 they used to borrow from a fabricated path.
    ///
    /// Pinned here rather than end-to-end because the call sites are a spawn
    /// timeout, a shutdown timeout, a signal failure, and an RPC failure against
    /// a *live* daemon — none reproducible cheaply or deterministically in a
    /// test. This asserts the mapping; the call sites are covered by inspection.
    #[test]
    fn daemon_failures_classify_as_exit_7() {
        let err = daemon_err("status query failed");
        assert_eq!(
            clove_types::error_code(&err),
            ("DAEMON_ERROR", 7),
            "daemon failures must not be reported as filesystem errors"
        );
        assert_eq!(crate::exit::classify(&err).0.code(), 7);
    }
}
