//! Daemon lifecycle (T-D02, DESIGN §8.1/§8.2/§8.9): single-instance lock, socket
//! bind, pid-after-ready, signal-driven shutdown, and clean teardown.
//!
//! Ordering invariants:
//! - The `daemon.lock` advisory flock is taken first; a second daemon fails fast.
//! - `daemon.pid` is written **only after** the socket is bound (DESIGN §8.2), so
//!   a reader that sees a pid is guaranteed a usable socket. (From P3 the startup
//!   sweep also completes before the pid is written.)
//! - Shutdown flushes the index (`wal_checkpoint(TRUNCATE)`), then removes the
//!   socket and pid, then releases the lock (DESIGN §8.9).

use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use camino::Utf8Path;
use clove_index::Index;
use clove_ipc::{lock_path, pid_path, sock_path, socket_name};
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::Listener as TokioListener;
use interprocess::local_socket::ListenerOptions;

use crate::ipc::{handle_connection, Dispatcher};
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
    let graph = Arc::new(crate::graph_cache::GraphCache::new(repo_root));

    // 3. Tokio runtime — 2 workers (IPC + watcher), per DESIGN §8.1.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    let dispatcher = Dispatcher {
        index: Arc::clone(&index),
        state: Arc::clone(&state),
        issues_dir: issues_dir.clone(),
        db_path: db_path.clone(),
        auto_refresh,
        graph: Arc::clone(&graph),
    };

    let serve_result: anyhow::Result<()> = runtime.block_on(async {
        // A corpse socket from a crashed daemon: safe to remove — we hold the lock.
        let _ = std::fs::remove_file(sock_path(clove_dir));
        let name = socket_name(clove_dir).context("building socket name")?;
        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .with_context(|| format!("binding {}", sock_path(clove_dir)))?;

        // 4. Startup mtime sweep (DESIGN §8.6): re-index anything changed while
        //    the daemon was down (e.g. a `git pull`), BEFORE advertising
        //    readiness. Only then write the pid → "pid present" ⇒ "swept & ready".
        if let Ok(mut st) = state.lock() {
            st.set_watcher_state(WatcherState::Sweeping);
        }
        crate::reindexer::sync_once(&issues_dir, &index, &state);
        write_pid(clove_dir).context("writing pid file")?;

        // 5. Serve IPC + watch for changes until a shutdown signal (or idle
        //    timeout) fires.
        tokio::select! {
            _ = accept_loop(listener, dispatcher) => {},
            _ = crate::watcher::watch(issues_dir.clone(), Arc::clone(&index), Arc::clone(&state), debounce, watch_options.clone(), Arc::clone(&graph)) => {},
            _ = idle_watchdog(Arc::clone(&state), idle_shutdown) => {},
            _ = shutdown_signal(clove_dir) => {},
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

/// Write the current process id to `daemon.pid` (DESIGN §8.2).
fn write_pid(clove_dir: &Utf8Path) -> std::io::Result<()> {
    let mut file = File::create(pid_path(clove_dir))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

/// Accept connections forever, serving each on its own task.
async fn accept_loop(listener: TokioListener, dispatcher: Dispatcher) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let dispatcher = dispatcher.clone();
                tokio::spawn(async move {
                    // EOF / connection reset are normal client teardown.
                    let _ = handle_connection(stream, dispatcher).await;
                });
            }
            Err(e) => {
                eprintln!("cloved: accept error: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// Resolve once a shutdown signal arrives (DESIGN §8.9).
#[cfg(unix)]
async fn shutdown_signal(_clove_dir: &Utf8Path) {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => return,
    };
    tokio::select! {
        _ = term.recv() => {},
        _ = interrupt.recv() => {},
    }
}

/// Windows has no SIGTERM: wait on Ctrl-C (interactive) or the named shutdown
/// event that `clove daemon stop` signals (DESIGN §8.9).
#[cfg(windows)]
async fn shutdown_signal(clove_dir: &Utf8Path) {
    let event = clove_ipc::event_name(clove_dir);
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = wait_named_event(event) => {},
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
