//! Daemon lifecycle (T-D02, DESIGN §8.1/§8.2/§8.9): single-instance lock, socket
//! bind, pid-after-ready, signal-driven shutdown, and clean teardown.
//!
//! Ordering invariants:
//! - The `daemon.lock` advisory flock is taken first; a second daemon fails fast.
//! - `daemon.pid` is written **only after** the socket is bound (DESIGN §8.2), so
//!   a reader that sees a pid is guaranteed a usable socket. (From P3 the startup
//!   sweep also completes before the pid is written.)
//! - The shutdown-signal handler is installed **before** the pid is written, so a
//!   SIGTERM racing the daemon's readiness is caught (clean teardown) rather than
//!   hitting the kernel default disposition (abrupt kill, stale socket/pid).
//! - Shutdown flushes the index (`wal_checkpoint(TRUNCATE)`), then removes the
//!   socket and pid, then releases the lock (DESIGN §8.9).

use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use camino::Utf8Path;
use clove_index::Index;
use clove_ipc::{build_transport, lock_path, pid_path, sock_path, socket_name, CloveRpc};
use futures::StreamExt;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::Listener as TokioListener;
use interprocess::local_socket::ListenerOptions;
use tarpc::server::{BaseChannel, Channel};

use crate::ipc::Dispatcher;
use crate::state::{DaemonState, WatcherState};

/// Run the daemon for `clove_dir` until a shutdown signal arrives. Blocks the
/// calling thread (it owns the Tokio runtime). Exits the process with a non-zero
/// code if another daemon already holds the lock.
pub fn run(clove_dir: &Utf8Path) -> anyhow::Result<()> {
    // 1. Single-instance advisory lock. Held for the whole lifetime via `_guard`.
    let lock_file = File::create(lock_path(clove_dir))
        .with_context(|| format!("creating {}", lock_path(clove_dir)))?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = match lock.try_write() {
        Ok(guard) => guard,
        Err(_) => {
            eprintln!("cloved: daemon already running for {clove_dir}");
            std::process::exit(1);
        }
    };

    // Harden the runtime/state dir to owner-only (Unix): it holds the mutating
    // control socket + pid + write lock, which under a default umask would be
    // reachable by other local users on a shared machine (D-daemon-SEC-1).
    restrict_to_owner(clove_dir, 0o700);

    // 2. Open the index (rebuilt if stale/corrupt — it is a cache).
    let db_path = clove_dir.join("index.db");
    let issues_dir = clove_dir.join("issues");
    let index = Index::open_or_create(&db_path).context("opening index")?;
    let items = index.item_count().unwrap_or(0) as u64;
    let index = Arc::new(Mutex::new(index));
    let state = Arc::new(Mutex::new(DaemonState::new(items)));

    // Repo config (auto-refresh, debounce, and — from P5 — git_sync). A
    // missing/invalid config falls back to defaults; the daemon must still run.
    let config = clove_dir
        .parent()
        .and_then(|root| clove_core::load_config(root).ok());
    let auto_refresh = config.as_ref().is_none_or(|c| c.index.auto_refresh);
    let debounce =
        Duration::from_millis(config.as_ref().map_or(200, |c| c.daemon.watch_debounce_ms));
    // Falls back to the `DaemonConfig` default (non-zero) when no config loads,
    // so an idle daemon never lingers indefinitely.
    let idle_min = config.as_ref().map_or_else(
        || clove_core::DaemonConfig::default().idle_shutdown_min,
        |c| c.daemon.idle_shutdown_min,
    );
    let idle_shutdown = idle_shutdown_duration(idle_min);
    // Auto-snapshot interval (M4): records a `clove stats` history point on a timer
    // so trends accrue without manual `--snapshot`. Falls back to the config default.
    let snapshot_min = config.as_ref().map_or_else(
        || clove_core::DaemonConfig::default().stats_snapshot_min,
        |c| c.daemon.stats_snapshot_min,
    );
    let snapshot_interval = crate::snapshot::snapshot_interval(snapshot_min);
    let git_sync = config.as_ref().is_some_and(|c| c.daemon.git_sync);
    if git_sync && !cfg!(feature = "git-sync") {
        eprintln!(
            "cloved: [daemon] git_sync = true but this binary was built without \
             git-sync support; auto-commit is disabled"
        );
    }
    let repo_root = clove_dir.parent().unwrap_or(clove_dir).to_owned();
    let watch_options = crate::watcher::WatchOptions {
        repo_root: repo_root.clone(),
        git_sync,
    };
    let graph = Arc::new(crate::graph_cache::GraphCache::new(index.clone()));

    // Web UI (M4): served by the daemon by default ([web] enabled, port 7373), so
    // `clove serve` hands off to the daemon instead of binding its own server.
    // `CLOVED_DISABLE_WEB` turns it off (used by the daemon tests to avoid all
    // instances contending for the fixed port); `CLOVED_WEB_PORT` overrides the port.
    let web_enabled = config.as_ref().is_none_or(|c| c.web.enabled)
        && std::env::var_os("CLOVED_DISABLE_WEB").is_none();
    let web_port = std::env::var("CLOVED_WEB_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or_else(|| config.as_ref().map_or(7373, |c| c.web.port));
    let id_prefix = config
        .as_ref()
        .map_or_else(|| "proj".to_owned(), |c| c.id_prefix.clone());
    let web_state = web_enabled.then(|| {
        let hb = Arc::clone(&state);
        let heartbeat: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Ok(mut s) = hb.lock() {
                s.mark_event();
            }
        });
        clove_web::AppState::new(
            clove_core::ItemStore::new(repo_root.clone()),
            issues_dir.clone(),
            id_prefix,
            "daemon",
            true,
            config.as_ref().map_or_else(
                || clove_core::CloveConfig::default().default_type,
                |c| c.default_type,
            ),
        )
        .with_heartbeat(heartbeat)
    });
    // The web address is advertised to clients only *after* a successful bind
    // (inside `serve_web`), never up front: per-project daemons share one fixed
    // port, so a second daemon that loses the bind must not claim it serves the
    // web UI — else `clove serve` would hand the user off to another project's
    // tracker (D-daemon-5).
    let web_addr: std::net::SocketAddr = (std::net::Ipv4Addr::LOCALHOST, web_port).into();

    // 3. Tokio runtime — 2 workers (IPC + watcher), per DESIGN §8.1.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    // Id prefix + default type for daemon-side `create` (topology B writes).
    let id_prefix = config.as_ref().map_or_else(
        || clove_core::CloveConfig::default().id_prefix,
        |c| c.id_prefix.clone(),
    );
    let default_type = config.as_ref().map_or_else(
        || clove_core::CloveConfig::default().default_type,
        |c| c.default_type,
    );

    let dispatcher = Dispatcher {
        index: Arc::clone(&index),
        state: Arc::clone(&state),
        repo_root: repo_root.clone(),
        issues_dir: issues_dir.clone(),
        db_path: db_path.clone(),
        auto_refresh,
        graph: Arc::clone(&graph),
        id_prefix,
        default_type,
    };

    let serve_result: anyhow::Result<()> = runtime.block_on(async {
        // A corpse socket from a crashed daemon: safe to remove — we hold the lock.
        let _ = std::fs::remove_file(sock_path(clove_dir));
        let name = socket_name(clove_dir).context("building socket name")?;
        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .with_context(|| format!("binding {}", sock_path(clove_dir)))?;
        // The control socket is a mutating RPC channel — restrict it to the owner
        // (Unix; the Windows named pipe is unaffected). (D-daemon-SEC-1)
        restrict_to_owner(&sock_path(clove_dir), 0o600);

        // 4. Startup mtime sweep (DESIGN §8.6): re-index anything changed while
        //    the daemon was down (e.g. a `git pull`), BEFORE advertising
        //    readiness. Only then write the pid → "pid present" ⇒ "swept & ready".
        if let Ok(mut st) = state.lock() {
            st.set_watcher_state(WatcherState::Sweeping);
        }
        crate::reindexer::sync_once(&issues_dir, &index, &state);
        // Register the shutdown-signal handler BEFORE advertising readiness (the
        // pid file). Otherwise a SIGTERM delivered in the window between the pid
        // write and the `select!` below (where the handler used to be installed)
        // hits the kernel default "terminate" disposition — killing the daemon
        // without the cleanup sequence and leaving a stale socket/pid behind.
        // "pid present ⇒ ready to shut down cleanly." (DESIGN §8.9)
        let mut shutdown = ShutdownSignal::install(clove_dir);
        write_pid(clove_dir).context("writing pid file")?;
        restrict_to_owner(&pid_path(clove_dir), 0o600);

        // Opt-in periodic two-way GitHub sync (T-M06). When the feature is off,
        // or the interval/repo aren't configured, this future never resolves.
        #[cfg(feature = "github-sync")]
        let github_sync_fut = {
            let interval_min = config
                .as_ref()
                .map_or(0, |c| c.daemon.github_sync_interval_min);
            let repo = config
                .as_ref()
                .and_then(|c| c.daemon.github_sync_repo.clone());
            crate::github_sync::github_sync_loop(
                repo_root.clone(),
                repo,
                crate::github_sync::github_sync_interval(interval_min),
            )
        };
        #[cfg(not(feature = "github-sync"))]
        let github_sync_fut = {
            // Symmetric with the git_sync warning above: if the config asks for
            // periodic GitHub sync but this binary lacks the feature, say so once
            // instead of silently never running it.
            let wants_github_sync = config.as_ref().is_some_and(|c| {
                c.daemon.github_sync_repo.is_some() || c.daemon.github_sync_interval_min > 0
            });
            if wants_github_sync {
                eprintln!(
                    "cloved: [daemon] github sync is configured but this binary was \
                     built without github-sync support; periodic GitHub sync is disabled"
                );
            }
            std::future::pending::<()>()
        };

        // 5. Serve IPC + watch for changes until a shutdown signal (or idle
        //    timeout) fires.
        tokio::select! {
            _ = accept_loop(listener, dispatcher) => {},
            _ = crate::watcher::watch(issues_dir.clone(), Arc::clone(&index), Arc::clone(&state), debounce, watch_options.clone(), Arc::clone(&graph)) => {},
            _ = serve_web(web_state, web_addr, Arc::clone(&state)) => {},
            _ = idle_watchdog(Arc::clone(&state), idle_shutdown) => {},
            _ = crate::snapshot::snapshot_loop(repo_root.clone(), Arc::clone(&index), snapshot_interval) => {},
            _ = github_sync_fut => {},
            _ = shutdown.recv() => {},
        }
        Ok(())
    });

    // 6. Shutdown sequence (DESIGN §8.9): flush WAL, drop the connection, remove
    //    the socket + pid. The lock is released when `_guard` drops on return.
    if let Ok(idx) = index.lock() {
        let _ = idx.checkpoint_truncate();
    }
    drop(index);
    let _ = std::fs::remove_file(sock_path(clove_dir));
    let _ = std::fs::remove_file(pid_path(clove_dir));

    serve_result
}

/// Resolve the idle-shutdown window (DESIGN §8.8). `idle_shutdown_min == 0` means
/// never. A `CLOVED_IDLE_SHUTDOWN_MS` env var overrides it (sub-minute values for
/// tests). `None` = never self-terminate.
fn idle_shutdown_duration(idle_min: u64) -> Option<Duration> {
    if let Ok(ms) = std::env::var("CLOVED_IDLE_SHUTDOWN_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            return (ms > 0).then(|| Duration::from_millis(ms));
        }
    }
    (idle_min > 0).then(|| Duration::from_secs(idle_min * 60))
}

/// Resolve once the daemon has been idle for `idle` (DESIGN §8.8). Never resolves
/// when `idle` is `None`. Idle resets on any watcher/IPC activity (`mark_event`).
async fn idle_watchdog(state: Arc<Mutex<DaemonState>>, idle: Option<Duration>) {
    let Some(idle) = idle else {
        std::future::pending::<()>().await;
        return;
    };
    // Check a few times per window so shutdown fires within a fraction of it.
    let tick = (idle / 4).max(Duration::from_millis(25));
    loop {
        tokio::time::sleep(tick).await;
        let idle_for = state.lock().map(|s| s.idle_for()).unwrap_or_default();
        if idle_for >= idle {
            return;
        }
    }
}

/// Restrict a runtime file or directory to owner-only access (Unix). A no-op on
/// other platforms (Windows named pipes carry their own ACLs). Best-effort:
/// a chmod failure must not bring the daemon down (D-daemon-SEC-1).
#[cfg(unix)]
fn restrict_to_owner(path: &Utf8Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// No-op on non-Unix platforms.
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Utf8Path, _mode: u32) {}

/// Write the current process id to `daemon.pid` (DESIGN §8.2).
fn write_pid(clove_dir: &Utf8Path) -> std::io::Result<()> {
    let mut file = File::create(pid_path(clove_dir))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

/// Serve the web UI (with its own debounced file-watcher for real-time push).
/// A bind/serve failure is logged but does not bring the daemon down — the web UI
/// is an optional accelerator like the rest of the daemon. Resolves never on the
/// success path so the `select!` arm only completes if serving truly ends.
///
/// The daemon's `web_addr` (surfaced by `STATUS` and trusted by `clove serve`) is
/// advertised **only after the bind succeeds** and cleared if serving later
/// errors, so a daemon that lost the shared port never claims to serve the UI
/// (D-daemon-5).
async fn serve_web(
    state: Option<clove_web::AppState>,
    addr: std::net::SocketAddr,
    daemon_state: Arc<Mutex<DaemonState>>,
) {
    if let Some(state) = state {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                // We hold the port: now it is safe to advertise the address.
                if let Ok(mut s) = daemon_state.lock() {
                    s.set_web_addr(Some(addr.to_string()));
                }
                if let Err(e) = clove_web::serve_with_watch_on(state, listener).await {
                    eprintln!("cloved: web server error ({addr}): {e}");
                    // Serving ended in error — stop advertising a UI we no longer serve.
                    if let Ok(mut s) = daemon_state.lock() {
                        s.set_web_addr(None);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Expected when another daemon or `clove serve` already holds the
                // port (per-project daemons share one web port). The daemon keeps
                // running without the web UI and does NOT advertise `web_addr`.
                eprintln!(
                    "cloved: web UI port {addr} in use; this daemon will not serve the web UI"
                );
            }
            Err(e) => {
                eprintln!("cloved: web server bind error ({addr}): {e}");
            }
        }
    }
    std::future::pending::<()>().await
}

/// Accept connections forever, serving each as a tarpc channel on its own task.
async fn accept_loop(listener: TokioListener, dispatcher: Dispatcher) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let dispatcher = dispatcher.clone();
                tokio::spawn(async move {
                    // One tarpc channel per connection; each request is served on
                    // its own task. EOF / reset are normal client teardown.
                    let transport = build_transport(stream);
                    BaseChannel::with_defaults(transport)
                        .execute(dispatcher.serve())
                        .for_each(|response| async move {
                            tokio::spawn(response);
                        })
                        .await;
                });
            }
            Err(e) => {
                eprintln!("cloved: accept error: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
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
    fn install(_clove_dir: &Utf8Path) -> Self {
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
/// event that `clove daemon stop` signals (DESIGN §8.9).
#[cfg(windows)]
struct ShutdownSignal {
    clove_dir: camino::Utf8PathBuf,
}

#[cfg(windows)]
impl ShutdownSignal {
    fn install(clove_dir: &Utf8Path) -> Self {
        ShutdownSignal {
            clove_dir: clove_dir.to_owned(),
        }
    }

    async fn recv(&mut self) {
        let event = clove_ipc::event_name(&self.clove_dir);
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = wait_named_event(event) => {},
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
