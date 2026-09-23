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
use clove_ipc::{ClientError, DaemonClient, HubClient, HubPaths, LegacyDaemon};
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
    let hub = HubPaths::resolve().map_err(|e| daemon_err(&e.to_string()))?;
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
            return Err(daemon_err(&match clove_ipc::legacy_daemon(clove_dir) {
                LegacyDaemon::Alive(pid) => format!(
                    "a clove 0.1.0 daemon (pid {pid}) already serves this project; \
                     `clove daemon stop` stops it, then start again"
                ),
                _ => format!("the daemon cannot serve this project: {message}"),
            }));
        }
        Err(ClientError::Connect(e) | ClientError::Name(e)) => {
            return Err(daemon_err(&format!("could not start the daemon: {e}")));
        }
        Err(ClientError::Timeout) => {
            return Err(daemon_err(
                "the daemon did not finish starting and loading this project within \
                 10s (a large project's first load can take longer); try again",
            ));
        }
        Err(e) => return Err(daemon_err(&format!("could not start the daemon: {e}"))),
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
    // A pre-hub daemon still running after an upgrade. Only one that answered
    // on the project's old socket is signalled (a stale pid can name an
    // unrelated process, D-daemon-6), only proven corpses are removed, and
    // anything in between is left exactly as it is.
    match legacy_step(clove_ipc::legacy_daemon(clove_dir), cfg!(windows)) {
        LegacyStep::Stop(pid) => {
            signal_pid(pid)?;
            wait_gone(&clove_ipc::pid_path(clove_dir))?;
            return emit(
                format,
                json!({ "stopped": true, "legacy": true }),
                &format!("stopped the clove 0.1.0 daemon (pid {pid})"),
            );
        }
        LegacyStep::Clean => clove_ipc::cleanup_legacy(clove_dir),
        LegacyStep::Refuse(pid) => {
            return Err(daemon_err(&format!(
                "a clove 0.1.0 daemon{} may still be serving this project, but it \
                 could not be verified (it did not answer, or its socket is not this \
                 user's); its files in .clove/ were left in place — check the process \
                 and stop it yourself",
                pid_note(pid)
            )));
        }
        LegacyStep::Note(pid) => eprintln!(
            "note: {} is left from a clove 0.1.0 daemon{}, which cannot be verified on \
             Windows; it was not stopped or removed — if no such daemon runs any more, \
             delete the file",
            clove_ipc::pid_path(clove_dir),
            pid_note(pid)
        ),
        LegacyStep::Continue => {}
    }

    let not_running = || {
        emit(
            format,
            json!({ "stopped": false, "running": false }),
            "no daemon serving this project",
        )
    };
    if !hub.footprint_present() {
        return not_running();
    }
    // Always ask the hub to stop the project, rather than probe first: a
    // project still loading answers a probe as not loaded, yet is about to be
    // served. The hub knows, and says whether it was serving (or loading) it.
    let exiting_pid = hub.read_pid();
    let detached = match DaemonClient::detach_at(hub, clove_dir) {
        Ok(detached) => detached,
        Err(ClientError::Connect(_)) if !hub.running() => return not_running(),
        // No token: no client ever loaded the project, so nothing serves it.
        Err(ClientError::Token(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return not_running()
        }
        Err(ClientError::Token(e)) => return Err(token_err(&e)),
        Err(e @ (ClientError::App(_) | ClientError::Transport(_))) => {
            return Err(daemon_err(&format!("detaching this project: {e}")))
        }
        Err(e) => {
            return Err(daemon_err(&format!(
                "a daemon is running but this clove cannot talk to it ({e}); stop it \
                 with `clove daemon stop --all`"
            )))
        }
    };
    if !detached.detached {
        return not_running();
    }
    if detached.stopping {
        return emit(
            format,
            json!({ "stopped": false, "stopping": true }),
            "the daemon is still stopping this project (flushing its index); it stops \
             serving it once that is done",
        );
    }
    if detached.hub_exiting {
        if let Some(pid) = exiting_pid {
            wait_hub_gone(&hub.pid(), pid)?;
        }
    }
    emit(
        format,
        json!({ "stopped": true, "hub_stopped": detached.hub_exiting }),
        "daemon stopped serving this project",
    )
}

/// The project's daemon token is unusable: the error names the file.
fn token_err(e: &std::io::Error) -> CloveError {
    daemon_err(&format!(
        "this project's daemon token cannot be used ({e}); make it readable, or \
         delete it so a fresh one is made"
    ))
}

/// What `clove daemon stop` does about a clove 0.1.0 daemon's footprint.
#[derive(Debug, PartialEq, Eq)]
enum LegacyStep {
    /// It answered: signal it.
    Stop(u32),
    /// Proven corpses: remove them, then stop this project's hub slot.
    Clean,
    /// Could not be verified here: leave it alone and say so.
    Refuse(Option<u32>),
    /// Cannot be verified on this platform at all (Windows): leave it alone,
    /// note it, and still stop this project's hub slot — a leftover pid file
    /// must not block `stop` forever.
    Note(Option<u32>),
    /// Nothing there.
    Continue,
}

fn legacy_step(legacy: LegacyDaemon, never_verifiable: bool) -> LegacyStep {
    match legacy {
        LegacyDaemon::Alive(pid) => LegacyStep::Stop(pid),
        LegacyDaemon::Dead => LegacyStep::Clean,
        LegacyDaemon::Unknown(pid) if never_verifiable => LegacyStep::Note(pid),
        LegacyDaemon::Unknown(pid) => LegacyStep::Refuse(pid),
        LegacyDaemon::Absent => LegacyStep::Continue,
    }
}

fn pid_note(pid: Option<u32>) -> String {
    pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
}

/// Stop the hub — every project it serves.
fn stop_hub(hub: &HubPaths, format: OutputFormat) -> Result<ExitCode, CloveError> {
    // Signal only a hub that proves itself alive: a leftover `hub.pid` can name
    // an unrelated process after pid reuse (D-daemon-6). An answer of any
    // protocol version is proof enough — that is exactly the old hub an upgrade
    // leaves behind — and so is a busy hub that did not answer in time, or one
    // holding its lock while it starts.
    let alive = match HubClient::connect(hub) {
        Ok(_)
        | Err(ClientError::Refused { .. })
        | Err(ClientError::Transport(_))
        | Err(ClientError::Timeout) => true,
        Err(_) => hub.running(),
    };
    if !alive {
        return emit(
            format,
            json!({ "stopped": false, "running": false }),
            "no daemon running",
        );
    }
    let Some(pid) = hub.read_pid() else {
        return Err(daemon_err(&format!(
            "a daemon holds {} but has no usable pid file; wait for it to finish \
             starting and try again",
            hub.dir()
        )));
    };
    signal_hub(hub, pid)?;
    match wait_hub_gone(&hub.pid(), pid)? {
        HubGone::Gone => emit(format, json!({ "stopped": true }), "daemon stopped"),
        HubGone::Replaced(new) => emit(
            format,
            json!({ "stopped": true, "restarted_by_another_client": new }),
            &format!("daemon stopped; another client has since started a new one (pid {new})"),
        ),
    }
}

/// How the hub a client signalled went away.
#[derive(Debug, PartialEq, Eq)]
enum HubGone {
    Gone,
    /// It stopped, and another client's start put up a new hub (this pid) in
    /// the meantime.
    Replaced(u32),
}

/// Wait for the hub `signalled` to go: its pid file removed, or naming
/// another hub — one a concurrent `clove daemon start` spawned — which is not
/// the hub this client stopped.
fn wait_hub_gone(pid_file: &Utf8Path, signalled: u32) -> Result<HubGone, CloveError> {
    let start = Instant::now();
    while start.elapsed() < WAIT_TIMEOUT {
        match std::fs::read_to_string(pid_file) {
            Err(_) if !pid_file.exists() => return Ok(HubGone::Gone),
            Ok(text) => match clove_ipc::parse_pid(&text) {
                Some(pid) if pid != signalled => return Ok(HubGone::Replaced(pid)),
                _ => {}
            },
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(daemon_err("daemon did not stop within 5s"))
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
    // With a hub there to ask, a token this client cannot use is the answer:
    // "not serving" would be wrong.
    if hub.footprint_present() {
        match clove_ipc::project(clove_dir, false) {
            // No token: never loaded, so not served.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(token_err(&e)),
            Ok(_) => {}
        }
    }
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
                    outln!(
                        "  {}  items {}  watcher {}",
                        p.clove_dir,
                        p.status.items_indexed,
                        p.status.watcher_state
                    );
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
    let name = hub
        .event_name()
        .map_err(|e| daemon_err(&format!("naming the shutdown event: {e}")))?;
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
    // `kill(0)`/`kill(-n)` signal whole process groups and pid 1 is init: only
    // an ordinary pid is ever signalled.
    let Some(pid) = signallable(pid) else {
        return Err(daemon_err(&format!("refusing to signal pid {pid}")));
    };
    // SAFETY: kill(2) with a validated positive pid and SIGTERM (15).
    let rc = unsafe { libc_kill(pid, 15) };
    if rc == -1 {
        let err = std::io::Error::last_os_error();
        // ESRCH (no such process): treat as already-stopped.
        if err.raw_os_error() != Some(3) {
            return Err(daemon_err(&format!("sending SIGTERM: {err}")));
        }
    }
    Ok(())
}

/// `pid` as a `kill(2)` target, if it names one ordinary process.
#[cfg(unix)]
fn signallable(pid: u32) -> Option<i32> {
    i32::try_from(pid).ok().filter(|&p| p > 1)
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
    use super::{daemon_err, legacy_step, LegacyDaemon, LegacyStep};

    /// `stop --all` while another client's start spawns a new hub: the hub
    /// this client signalled did stop, and a different pid in the pid file is
    /// the new one — not a hub that "did not stop" (item 4).
    #[test]
    fn a_hub_restarted_by_another_client_counts_as_stopped() {
        use super::{wait_hub_gone, HubGone};
        let tmp = tempfile::tempdir().unwrap();
        let pid = camino::Utf8PathBuf::from_path_buf(tmp.path().join("hub.pid")).unwrap();
        std::fs::write(&pid, "100\n").unwrap();
        let writer = {
            let pid = pid.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::fs::write(&pid, "200\n").unwrap();
            })
        };
        let waited = wait_hub_gone(&pid, 100);
        writer.join().unwrap();
        assert_eq!(waited.unwrap(), HubGone::Replaced(200));

        std::fs::remove_file(&pid).unwrap();
        assert_eq!(wait_hub_gone(&pid, 200).unwrap(), HubGone::Gone);
    }

    /// Where a clove 0.1.0 daemon can never be verified (Windows), its
    /// leftover pid file must not block stopping this project's hub slot for
    /// good; where it can, an unverifiable one still stops `stop`.
    #[test]
    fn an_unverifiable_old_daemon_blocks_stop_only_where_one_could_be_verified() {
        assert_eq!(
            legacy_step(LegacyDaemon::Unknown(Some(4242)), true),
            LegacyStep::Note(Some(4242))
        );
        assert_eq!(
            legacy_step(LegacyDaemon::Unknown(Some(4242)), false),
            LegacyStep::Refuse(Some(4242))
        );
        assert_eq!(
            legacy_step(LegacyDaemon::Alive(4242), true),
            LegacyStep::Stop(4242)
        );
    }

    /// Daemon-communication failures classify as `DAEMON_ERROR` / exit 7, not
    /// the `IO_ERROR` / exit 5 they used to borrow from a fabricated path.
    ///
    /// Pinned here rather than end-to-end because the call sites are a spawn
    /// timeout, a shutdown timeout, a signal failure, and an RPC failure against
    /// a *live* daemon — none reproducible cheaply or deterministically in a
    /// test. This asserts the mapping; the call sites are covered by inspection.
    #[cfg(unix)]
    #[test]
    fn only_ordinary_pids_are_signalled() {
        assert_eq!(super::signallable(4242), Some(4242));
        for pid in [0, 1, u32::MAX, i32::MAX as u32 + 1] {
            assert_eq!(super::signallable(pid), None, "{pid}");
        }
    }

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
