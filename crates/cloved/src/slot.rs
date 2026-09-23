//! One project served by the hub (DESIGN §8.1).
//!
//! A slot is what used to be a whole daemon process — the index, the graph
//! cache, the watcher, the snapshot and GitHub-sync loops, the idle timer — now
//! loaded, supervised, and torn down by the hub on its own. It still takes the
//! project's `.clove/daemon.lock`, so a project is served by at most one daemon
//! of any version.

use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use clove_index::Index;
use clove_ipc::hub::codes;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::graph_cache::GraphCache;
use crate::ipc::Dispatcher;
use crate::state::{DaemonState, WatcherState};

/// A loaded project.
pub struct Slot {
    /// The canonical `.clove/` directory — the hub's key for this project.
    pub clove_dir: Utf8PathBuf,
    pub repo_root: Utf8PathBuf,
    pub dispatcher: Dispatcher,
    /// Cancelling it tears the slot down and closes every connection attached
    /// to it, so clients re-probe instead of talking to an unloaded project.
    pub cancel: CancellationToken,
    /// Cancelled by the supervisor once teardown has run.
    pub done: CancellationToken,
    /// The web mount's slug, when the project is on the hub's web listener.
    pub web_slug: Mutex<Option<String>>,
    /// Initialized by whoever starts the slot (web mount, tasks). Every other
    /// caller that attached while it loaded waits for that to finish, so none
    /// sees the project half-started.
    pub started: tokio::sync::OnceCell<()>,
    pub settings: Settings,
    /// `daemon.lock`, held until teardown closes it ([`Slot::release_lock`]).
    lock: Mutex<Option<File>>,
    /// The watch put on `issues/` before the startup sweep, until
    /// [`Slot::tasks`] hands it to the watcher task.
    armed: Mutex<Option<crate::watcher::Armed>>,
}

/// A project's daemon behaviour, from its `.clove/config.toml` (defaults when it
/// is missing or invalid — a bad config must not keep the project unserved).
pub struct Settings {
    pub debounce: Duration,
    pub idle: Option<Duration>,
    pub snapshot_interval: Option<Duration>,
    pub git_sync: bool,
    pub web_enabled: bool,
    pub web_port: u16,
    pub id_prefix: String,
    pub default_type: clove_types::ItemType,
    #[cfg_attr(not(feature = "github-sync"), allow(dead_code))]
    pub github_sync: (Option<String>, u64),
}

/// Why a project could not be loaded: a [`codes`] value and a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub code: &'static str,
    pub message: String,
}

impl LoadError {
    fn failed(message: impl Into<String>) -> LoadError {
        LoadError {
            code: codes::LOAD_FAILED,
            message: message.into(),
        }
    }
}

/// What ended a slot's task set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotTask {
    Watcher,
    Snapshot,
    #[cfg(feature = "github-sync")]
    GithubSync,
    /// The idle timer — the one task whose end is an ordinary eviction.
    Idle,
}

/// Resolve a caller's (absolute) `.clove/` path to the hub's key for it, so
/// `/tmp/x` and `/private/tmp/x`, or a symlinked checkout, name one slot.
pub fn canonical_key(clove_dir: &str) -> Result<Utf8PathBuf, LoadError> {
    let path = std::fs::canonicalize(clove_dir)
        .map_err(|e| LoadError::failed(format!("{clove_dir}: {e}")))?;
    Utf8PathBuf::from_path_buf(path)
        .map_err(|p| LoadError::failed(format!("{} is not UTF-8", p.display())))
}

/// Load the project at `clove_dir` (already canonical): take its lock, open the
/// index, and run the startup sweep (DESIGN §8.6) so it answers fresh from the
/// first query. Blocking — the hub runs it on the blocking pool.
pub fn open(clove_dir: &Utf8Path, cancel: CancellationToken) -> Result<Slot, LoadError> {
    let issues_dir = clove_dir.join("issues");
    // A symlinked issues/ would be served as "loaded" while every read and
    // write of it is refused (clove_core::fs_safe).
    clove_core::fs_safe::check_dirs(&issues_dir)
        .map_err(|e| LoadError::failed(format!("{clove_dir}: {e}")))?;
    if !issues_dir.is_dir() {
        return Err(LoadError::failed(format!(
            "{clove_dir} is not a clove store (no issues/ directory)"
        )));
    }
    // Symlink-safe: the lock ships with the repository, and a planted link
    // would otherwise have its target truncated.
    let lock_path = clove_ipc::lock_path(clove_dir);
    let lock = clove_core::fs_safe::open_lock_file(&lock_path)
        .map_err(|e| LoadError::failed(format!("opening {lock_path}: {e}")))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(LoadError {
                code: codes::PROJECT_LOCKED,
                message: format!(
                    "another process holds {lock_path}: some other daemon is serving \
                     this project; stop it to let this daemon serve it"
                ),
            })
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(LoadError::failed(format!("locking {clove_dir}: {e}")))
        }
    }

    let repo_root = clove_dir.parent().unwrap_or(clove_dir).to_owned();
    let config = clove_core::load_config(&repo_root).ok();
    let settings = settings(config.as_ref());

    let db_path = clove_dir.join("index.db");
    // A rebuild that cannot run right now — most often because another process
    // holds the reindex lock while upgrading the same index — must not keep the
    // project unserved. Fall back to opening whatever is there; the index is a
    // cache, and the next open retries the rebuild.
    let index = match Index::open_or_rebuild(&db_path, &issues_dir) {
        Ok(index) => index,
        Err(e) => {
            eprintln!(
                "cloved: {clove_dir}: could not rebuild the index ({e}); starting with what is there"
            );
            Index::open_or_create(&db_path)
                .map_err(|e| LoadError::failed(format!("opening {db_path}: {e}")))?
        }
    };
    let items = index.item_count().unwrap_or(0) as u64;
    let index = Arc::new(Mutex::new(index));
    let state = Arc::new(Mutex::new(DaemonState::new(items)));
    let graph = Arc::new(GraphCache::new(index.clone()));

    // Watch first, then sweep: a file written after the sweep has read it is
    // queued by the watch, so nothing slips between the two.
    let armed = crate::watcher::arm(&issues_dir)
        .map_err(|why| LoadError::failed(format!("{clove_dir}: {why}")))?;
    if let Ok(mut st) = state.lock() {
        st.set_watcher_state(WatcherState::Sweeping);
    }
    crate::reindexer::sync_once(&issues_dir, &index, &state);

    let dispatcher = Dispatcher {
        index,
        state,
        repo_root: repo_root.clone(),
        issues_dir,
        db_path,
        auto_refresh: config.as_ref().is_none_or(|c| c.index.auto_refresh),
        graph,
        id_prefix: settings.id_prefix.clone(),
        default_type: settings.default_type,
    };
    Ok(Slot {
        clove_dir: clove_dir.to_owned(),
        repo_root,
        dispatcher,
        cancel,
        done: CancellationToken::new(),
        web_slug: Mutex::new(None),
        started: tokio::sync::OnceCell::new(),
        settings,
        lock: Mutex::new(Some(lock)),
        armed: Mutex::new(Some(armed)),
    })
}

fn settings(config: Option<&clove_core::CloveConfig>) -> Settings {
    let defaults = clove_core::CloveConfig::default();
    let daemon = config.map_or(&defaults.daemon, |c| &c.daemon);
    let git_sync = daemon.git_sync;
    if git_sync && !cfg!(feature = "git-sync") {
        eprintln!(
            "cloved: [daemon] git_sync = true but this binary was built without \
             git-sync support; auto-commit is disabled"
        );
    }
    #[cfg(not(feature = "github-sync"))]
    if daemon.github_sync_repo.is_some() || daemon.github_sync_interval_min > 0 {
        eprintln!(
            "cloved: [daemon] github sync is configured but this binary was \
             built without github-sync support; periodic GitHub sync is disabled"
        );
    }
    Settings {
        debounce: Duration::from_millis(daemon.watch_debounce_ms),
        idle: idle_shutdown_duration(daemon.idle_shutdown_min),
        snapshot_interval: crate::snapshot::snapshot_interval(daemon.stats_snapshot_min),
        git_sync,
        web_enabled: config.is_none_or(|c| c.web.enabled),
        web_port: config.map_or(defaults.web.port, |c| c.web.port),
        id_prefix: config.map_or_else(|| defaults.id_prefix.clone(), |c| c.id_prefix.clone()),
        default_type: config.map_or(defaults.default_type, |c| c.default_type),
        github_sync: (
            daemon.github_sync_repo.clone(),
            daemon.github_sync_interval_min,
        ),
    }
}

/// Resolve the idle-eviction window (DESIGN §8.8). `idle_shutdown_min == 0` means
/// never. `CLOVED_IDLE_SHUTDOWN_MS` overrides it for every project (sub-minute
/// values for tests). `None` = never evict.
fn idle_shutdown_duration(idle_min: u64) -> Option<Duration> {
    if let Ok(ms) = std::env::var("CLOVED_IDLE_SHUTDOWN_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            return (ms > 0).then(|| Duration::from_millis(ms));
        }
    }
    (idle_min > 0).then(|| Duration::from_secs(idle_min * 60))
}

impl Slot {
    /// The project's background work. Each task runs until the slot is torn
    /// down; any of them ending is what tears it down (see [`supervise`]).
    pub fn tasks(&self) -> JoinSet<SlotTask> {
        let d = &self.dispatcher;
        let mut tasks = JoinSet::new();
        let armed = self.armed.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(armed) = armed {
            let watch = crate::watcher::watch(
                armed,
                d.issues_dir.clone(),
                d.index.clone(),
                d.state.clone(),
                self.settings.debounce,
                crate::watcher::WatchOptions {
                    repo_root: self.repo_root.clone(),
                    git_sync: self.settings.git_sync,
                },
                d.graph.clone(),
            );
            tasks.spawn(async move {
                watch.await;
                SlotTask::Watcher
            });
        }
        let snapshots = crate::snapshot::snapshot_loop(
            self.repo_root.clone(),
            d.index.clone(),
            self.settings.snapshot_interval,
        );
        tasks.spawn(async move {
            snapshots.await;
            SlotTask::Snapshot
        });
        #[cfg(feature = "github-sync")]
        {
            let (repo, interval_min) = self.settings.github_sync.clone();
            // The config ships with the repository, so it may only name one of
            // the project's own remotes (see `github_remote`).
            let repo = repo.and_then(|spec| {
                match crate::github_remote::check_sync_target(&self.repo_root, &spec) {
                    Ok(repo) => Some(repo),
                    Err(why) => {
                        eprintln!("cloved: {}: not syncing with GitHub: {why}", self.clove_dir);
                        None
                    }
                }
            });
            let sync = crate::github_sync::github_sync_loop(
                self.clove_dir.clone(),
                repo,
                crate::github_sync::github_sync_interval(interval_min),
            );
            tasks.spawn(async move {
                sync.await;
                SlotTask::GithubSync
            });
        }
        let idle = idle_watchdog(d.state.clone(), self.settings.idle);
        tasks.spawn(async move {
            idle.await;
            SlotTask::Idle
        });
        tasks
    }

    /// Flush the index WAL (DESIGN §8.9) during teardown.
    pub fn checkpoint(&self) {
        if let Ok(index) = self.dispatcher.index.lock() {
            let _ = index.checkpoint_truncate();
        }
    }

    /// Release `daemon.lock` now rather than when the last handle to the slot
    /// drops, so the project can be loaded again as soon as teardown ends.
    pub fn release_lock(&self) {
        self.lock.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Resolve once the project has been idle for `idle` (DESIGN §8.8); never when
/// `idle` is `None`. Any IPC call, web request, or watcher batch resets it.
async fn idle_watchdog(state: Arc<Mutex<DaemonState>>, idle: Option<Duration>) {
    let Some(idle) = idle else {
        std::future::pending::<()>().await;
        return;
    };
    // Check a few times per window so eviction fires within a fraction of it.
    let tick = (idle / 4).max(Duration::from_millis(25));
    loop {
        tokio::time::sleep(tick).await;
        let idle_for = state.lock().map(|s| s.idle_for()).unwrap_or_default();
        if idle_for >= idle {
            return;
        }
    }
}
