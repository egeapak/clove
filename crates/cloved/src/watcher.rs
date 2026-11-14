//! File watcher with debounced batching (T-D04, DESIGN §8.5).
//!
//! `notify` runs the OS watch on its own thread and forwards `*.md` paths into a
//! Tokio channel. The async debounce loop coalesces a burst of events into a
//! single re-sync: it collects paths until the channel is quiet for the debounce
//! window, then applies **one** batch (one SQLite transaction). Each applied
//! batch bumps `DaemonState::batches_applied` — the M3-G05/G06 observable.
//!
//! **Feedback-loop prevention (M3-G05):** the watch is rooted at
//! `.clove/issues/`, so `.clove/index.db*` (a sibling, not under issues) is never
//! seen; the `*.md` filter is a second guard.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use clove_index::Index;
use notify::{recommended_watcher, Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::fresh::{Freshness, Sync};
use crate::graph_cache::GraphCache;
use crate::reindexer::{startup_sweep, sync_batch};
use crate::state::{DaemonState, WatcherState};

/// Per-batch options that depend on repo config (the git-sync opt-in).
#[derive(Clone)]
// Read only by the `git-sync` build; a lean build keeps them for a uniform API.
#[cfg_attr(not(feature = "git-sync"), allow(dead_code))]
pub struct WatchOptions {
    /// Repository root (parent of `.clove/`), for git-sync.
    pub repo_root: Utf8PathBuf,
    /// `[daemon] git_sync` — auto-commit clean edits (T-D06).
    pub git_sync: bool,
}

/// Only `*.md` files under the issues dir are item files; everything else
/// (including any stray `index.db*`) is ignored to prevent feedback loops.
fn is_item_file(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("md")
}

/// The longest a burst may keep collecting, however busy the directory is.
///
/// The quiet window alone has no upper bound: a writer that touches `issues/`
/// more often than the window (a shell loop of `clove new`) would restart it
/// forever and the batch would never be applied.
fn max_burst(debounce: Duration) -> Duration {
    debounce.saturating_mul(10).max(Duration::from_secs(1))
}

/// Collect one debounced burst: take `first`, then keep draining `rx` until it
/// has stayed quiet for `debounce`, or until `max_wait` has passed since
/// `first`. Returns the coalesced (deduplicated) path set that becomes exactly
/// one applied batch.
///
/// Split out of [`watch`] so the coalescing rule can be tested against a
/// **virtual** clock. The end-to-end watcher test can only observe batches by
/// writing files and waiting, which makes it a race between the OS scheduler and
/// the quiet window: under load a 10ms gap between writes can stretch past the
/// window, the burst flushes early, and the "exactly one batch" assertion fails
/// even though the logic is correct. The unit tests below drive this function
/// with `tokio::time` paused, so they assert the rule itself and cannot flake.
async fn collect_burst(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PathBuf>,
    first: PathBuf,
    debounce: Duration,
    max_wait: Duration,
) -> HashSet<PathBuf> {
    let mut pending: HashSet<PathBuf> = HashSet::new();
    pending.insert(first);
    let deadline = tokio::time::Instant::now() + max_wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break; // a busy directory must not starve the batch
        }
        match tokio::time::timeout(debounce.min(left), rx.recv()).await {
            Ok(Some(path)) => {
                pending.insert(path);
            }
            Ok(None) => break, // sender dropped → daemon shutting down
            Err(_) => break,   // quiet window elapsed → apply the batch
        }
    }
    pending
}

/// A watch on a project's `issues/` that is already in place: every change
/// from the moment [`arm`] returned is queued in `events`.
struct Armed {
    events: tokio::sync::mpsc::UnboundedReceiver<PathBuf>,
    _watcher: DropOffThread<RecommendedWatcher>,
}

/// Put the OS watch on `issues_dir`. Blocking, and on macOS at the mercy of
/// `fseventsd`: FSEvents setup can take seconds when it is busy.
///
/// Every report goes to `fresh` first (what changed, and whether the watch
/// still covers the directory); item-file paths are also queued in `events`,
/// which paces the batches.
fn arm(issues_dir: &Utf8Path, fresh: Arc<Freshness>) -> Result<Armed, String> {
    let (tx, events) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
    let noted = fresh.clone();
    // The notify handler runs on notify's own thread; forward only item-file
    // paths into the channel (non-blocking send, no runtime needed here).
    let mut watcher = recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) => {
            noted.note_event(&event);
            for path in event.paths {
                if is_item_file(&path) {
                    let _ = tx.send(path);
                }
            }
        }
        Err(_) => noted.note_error(),
    })
    .map_err(|e| format!("watcher init failed: {e}"))?;
    // Test knobs: a slow arm, as on a loaded machine — for a fixed time, or
    // until the project's `CLOVED_WATCH_ARM_HOLD_FILE` is removed.
    if let Some(delay) = crate::knobs::duration("CLOVED_WATCH_ARM_DELAY_MS") {
        std::thread::sleep(delay);
    }
    if let Some(clove_dir) = issues_dir.parent() {
        while crate::knobs::held("CLOVED_WATCH_ARM_HOLD_FILE", clove_dir.as_std_path()) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    watcher
        .watch(issues_dir.as_std_path(), RecursiveMode::Recursive)
        .map_err(|e| format!("watch({issues_dir}) failed: {e}"))?;
    fresh.armed();
    Ok(Armed {
        events,
        _watcher: DropOffThread(Some(watcher)),
    })
}

/// A loaded project's watcher task: arm the watch, catch up, then keep the
/// index fresh until the task is dropped (on teardown). Returns early if the
/// watch cannot be put in place, or once it no longer covers `issues/`; either
/// unloads the project, and the next call loads it again with a new watch.
///
/// The load has already swept `issues/` once, without waiting for this: the
/// project is served from the moment it is loaded. Until the watch is armed,
/// the index can miss a change, so reads are not answered from it (the state
/// says `Arming`; see `Dispatcher`). Once armed, a second sweep picks up
/// whatever changed between the first and the arming — anything later is
/// queued by the watch — and only then is the state `Watching`.
pub async fn run(
    fresh: Arc<Freshness>,
    issues_dir: Utf8PathBuf,
    index: Arc<Mutex<Index>>,
    state: Arc<Mutex<DaemonState>>,
    debounce: Duration,
    options: WatchOptions,
    graph: Arc<GraphCache>,
) {
    let arming = std::time::Instant::now();
    let armed = match arm_on_own_thread(issues_dir.clone(), fresh.clone()).await {
        Ok(armed) => armed,
        Err(why) => {
            status!("cloved: {issues_dir}: {why}");
            return;
        }
    };
    let took = arming.elapsed();
    if took > Duration::from_secs(1) {
        status!("cloved: {issues_dir}: the file watch took {took:.1?} to set up");
    }
    let (fresh_c, index_c, state_c, graph_c) =
        (fresh.clone(), index.clone(), state.clone(), graph.clone());
    let caught_up = tokio::task::spawn_blocking(move || {
        if startup_sweep(&fresh_c, &index_c, &state_c) {
            graph_c.mark_dirty();
        }
    })
    .await;
    if caught_up.is_err() {
        status!("cloved: {issues_dir}: the sweep after arming the watcher panicked");
        return;
    }
    watch(
        armed, fresh, issues_dir, index, state, debounce, options, graph,
    )
    .await;
}

/// [`arm`] on a thread of its own rather than the blocking pool: an arm stuck
/// behind `fseventsd` must neither hold a pool thread nor keep the runtime
/// from shutting down, and one that outlives its project (torn down while it
/// armed) just drops what it made.
async fn arm_on_own_thread(
    issues_dir: Utf8PathBuf,
    fresh: Arc<Freshness>,
) -> Result<Armed, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("cloved-arm".to_owned())
        .spawn(move || {
            let _ = tx.send(arm(&issues_dir, fresh));
        })
        .map_err(|e| format!("arming the watcher: {e}"))?;
    rx.await
        .map_err(|_| "arming the watcher: its thread died".to_owned())?
}

/// How often the watcher task checks that `issues/` is still the directory it
/// armed the watch on (one `stat`), for a loss the watch itself did not report.
const ROOT_CHECK: Duration = Duration::from_secs(1);

/// How long after a failed sync the watcher task tries again, and how many
/// failures in a row end the task (which reloads the project).
const RETRY_AFTER: Duration = Duration::from_secs(1);
const MAX_FAILURES: u32 = 5;

/// Keep the index fresh from `armed`'s changes until the task is dropped (on
/// shutdown) or the watch is lost. `debounce` is the per-burst quiet window
/// (DESIGN §8.5).
#[allow(clippy::too_many_arguments)]
async fn watch(
    armed: Armed,
    fresh: Arc<Freshness>,
    issues_dir: Utf8PathBuf,
    index: Arc<Mutex<Index>>,
    state: Arc<Mutex<DaemonState>>,
    debounce: Duration,
    options: WatchOptions,
    graph: Arc<GraphCache>,
) {
    let Armed {
        events: mut rx,
        _watcher,
    } = armed;
    if let Ok(mut st) = state.lock() {
        st.set_watcher_state(WatcherState::Watching);
    }
    let mut root_check = tokio::time::interval(ROOT_CHECK);
    root_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failures = 0u32;
    let mut retry: Option<tokio::time::Instant> = None;

    // Debounce loop: collect a burst, then apply exactly one batch.
    loop {
        let batch: Vec<Utf8PathBuf> = tokio::select! {
            first = rx.recv() => {
                let Some(first) = first else { return };
                collect_burst(&mut rx, first, debounce, max_burst(debounce))
                    .await
                    .into_iter()
                    .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
                    .collect()
            }
            // A sync is owed that no item-file event announces: reports were
            // dropped, or a request handler left a large catch-up to this task.
            _ = fresh.woken() => Vec::new(),
            _ = root_check.tick() => {
                if fresh.root_intact() {
                    continue;
                }
                Vec::new()
            }
            _ = async { tokio::time::sleep_until(retry.unwrap_or_else(tokio::time::Instant::now)).await },
                if retry.is_some() => Vec::new(),
        };
        retry = None;

        // Apply one index batch (one transaction) and record it — on the
        // blocking pool, like the IPC handlers: the sync (SQLite work while
        // holding the index mutex) and the git sync (libgit2 I/O, one commit
        // per file) can take seconds on a big batch (e.g. after a `git pull`),
        // and running them inline would park one of the daemon's two runtime
        // workers, starving concurrent `ping`s past the client's 50ms budget
        // exactly when the daemon is most needed.
        let fresh_b = fresh.clone();
        let index_b = index.clone();
        let state_b = state.clone();
        let graph_b = graph.clone();
        let options_b = options.clone();
        let done = tokio::task::spawn_blocking(move || {
            let outcome = sync_batch(&fresh_b, &index_b, &state_b);
            // The files changed → the cached dependency graph is now stale.
            graph_b.mark_dirty();
            if let Ok(mut st) = state_b.lock() {
                st.mark_event();
                st.inc_batches();
            }

            // Opt-in git auto-sync of the changed files (T-D06).
            maybe_git_sync(&options_b, batch, &index_b);
            outcome
        })
        .await;
        match done {
            Ok(Sync::Fresh { .. }) => failures = 0,
            Ok(Sync::Lost) => {
                status!(
                    "cloved: {issues_dir}: the file watch no longer covers this directory \
                     (it was removed or replaced)"
                );
                return;
            }
            // `Behind` cannot happen without a limit; treat it like a failure.
            Ok(Sync::Failed | Sync::Behind) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    status!("cloved: {issues_dir}: the index could not be brought up to date");
                    return;
                }
                retry = Some(tokio::time::Instant::now() + RETRY_AFTER);
            }
            Err(_) => status!("cloved: watcher batch task panicked"),
        }
    }
}

/// Drops its value on a thread of its own — the watcher, whose teardown
/// blocks, when its task is aborted on an async worker.
struct DropOffThread<T: Send + 'static>(Option<T>);

impl<T: Send + 'static> Drop for DropOffThread<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            std::thread::spawn(move || drop(value));
        }
    }
}

/// Auto-commit the batch's files when built with `git-sync` and enabled in config.
#[cfg(feature = "git-sync")]
fn maybe_git_sync(options: &WatchOptions, paths: Vec<Utf8PathBuf>, index: &Arc<Mutex<Index>>) {
    if options.git_sync && !paths.is_empty() {
        crate::git_sync::sync_files(&options.repo_root, &paths, index);
    }
}

/// No-op when the `git-sync` feature is disabled.
#[cfg(not(feature = "git-sync"))]
fn maybe_git_sync(_options: &WatchOptions, _paths: Vec<Utf8PathBuf>, _index: &Arc<Mutex<Index>>) {}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: Duration = Duration::from_millis(200);
    /// A burst cap far beyond any test's virtual time: the quiet window decides.
    const NO_CAP: Duration = Duration::from_secs(365 * 24 * 3600);

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/issues/{name}.md"))
    }

    /// Drive [`collect_burst`] once over an already-queued set of events.
    ///
    /// Every test here runs with `start_paused = true`, so `tokio::time` uses a
    /// virtual clock: the debounce timeout fires because the runtime is idle and
    /// auto-advances to the next deadline, not because any real time passed.
    /// These tests are therefore instant and deterministic — no sleeps, no
    /// dependence on the scheduler.
    async fn burst_of(events: &[&str], debounce: Duration) -> HashSet<PathBuf> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        for name in events {
            tx.send(path(name)).unwrap();
        }
        // Keep the sender alive so the burst ends on the quiet window, not on a
        // closed channel — that is the path the debounce rule is about.
        let first = rx.recv().await.expect("at least one event");
        let out = collect_burst(&mut rx, first, debounce, NO_CAP).await;
        drop(tx);
        out
    }

    /// M3-G05: `index.db*` is a sibling of the watched `issues/`, so writing it
    /// (a reindex) queues nothing. The watch delivers events in order, so a
    /// probe file written after the index writes arrives only after any event
    /// they would have produced: seeing the probe first proves there were none.
    #[tokio::test]
    async fn index_writes_outside_the_issues_dir_queue_no_events() {
        let tmp = tempfile::tempdir().unwrap();
        let clove_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let issues = clove_dir.join("issues");
        std::fs::create_dir_all(&issues).unwrap();
        let fresh = Arc::new(Freshness::new(issues.clone()));
        let mut armed = arm(&issues, fresh).expect("arm the watch");

        for name in ["index.db", "index.db-wal", "index.db-shm"] {
            std::fs::write(clove_dir.join(name), name).unwrap();
        }
        std::fs::write(issues.join("probe.md"), "probe").unwrap();

        let first = tokio::time::timeout(Duration::from_secs(30), armed.events.recv())
            .await
            .expect("the probe's event never arrived")
            .expect("the watch is open");
        assert_eq!(first.file_name().unwrap(), "probe.md", "{first:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_edits_coalesces_into_one_batch() {
        // The M3-G06 rule: many events arriving inside the quiet window become a
        // single batch. This is what the end-to-end test asserts by writing files
        // and counting applied batches; here it is asserted directly, so a slow
        // machine cannot turn a correct implementation into a failure.
        let names: Vec<String> = (0..10).map(|i| format!("item{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let burst = burst_of(&refs, DEBOUNCE).await;
        assert_eq!(burst.len(), 10, "one burst must carry every queued edit");
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_edits_to_one_file_collapse_to_a_single_path() {
        // Ten writes to the same file is the exact shape of the e2e test: the
        // batch is a path *set*, so the file is re-synced once, not ten times.
        let burst = burst_of(&["same"; 10], DEBOUNCE).await;
        assert_eq!(burst.len(), 1);
        assert!(burst.contains(&path("same")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_gap_longer_than_the_window_starts_a_new_burst() {
        // The other half of the rule: the window is what separates batches, so an
        // event arriving after it closes belongs to the next one. Draining an
        // empty channel makes the timeout fire immediately on the virtual clock.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();

        tx.send(path("first")).unwrap();
        let first = rx.recv().await.unwrap();
        let one = collect_burst(&mut rx, first, DEBOUNCE, NO_CAP).await;
        assert_eq!(one, HashSet::from([path("first")]));

        tx.send(path("second")).unwrap();
        let next = rx.recv().await.unwrap();
        let two = collect_burst(&mut rx, next, DEBOUNCE, NO_CAP).await;
        assert_eq!(
            two,
            HashSet::from([path("second")]),
            "an event after the quiet window is a separate batch"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_ends_the_burst_without_waiting() {
        // Daemon shutdown drops the sender mid-burst; the collected work must
        // still be returned rather than discarded or blocked on the window.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        tx.send(path("a")).unwrap();
        tx.send(path("b")).unwrap();
        let first = rx.recv().await.unwrap();
        drop(tx);

        let burst = collect_burst(&mut rx, first, DEBOUNCE, NO_CAP).await;
        assert_eq!(burst, HashSet::from([path("a"), path("b")]));
    }

    #[tokio::test(start_paused = true)]
    async fn the_window_length_does_not_change_the_outcome_for_a_queued_burst() {
        // Coalescing depends on the *gaps*, not on the window's magnitude, so a
        // queued burst behaves identically at 1ms and at an hour. This is the
        // property that makes the virtual clock sound here.
        for debounce in [
            Duration::from_millis(1),
            Duration::from_millis(200),
            Duration::from_secs(3600),
        ] {
            let burst = burst_of(&["x", "y", "z"], debounce).await;
            assert_eq!(burst.len(), 3, "debounce={debounce:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_directory_that_never_goes_quiet_still_flushes() {
        // A writer faster than the quiet window restarts it on every event.
        // Without a cap the burst never ends and the index is never updated.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let writer = tokio::spawn(async move {
            let mut n = 0u64;
            loop {
                if tx.send(path(&format!("item{n}"))).is_err() {
                    return;
                }
                n += 1;
                tokio::time::sleep(DEBOUNCE / 2).await;
            }
        });
        let first = rx.recv().await.unwrap();
        let cap = max_burst(DEBOUNCE);
        let started = tokio::time::Instant::now();
        let burst = tokio::time::timeout(cap * 3, collect_burst(&mut rx, first, DEBOUNCE, cap))
            .await
            .expect("the burst must end at the cap");
        assert!(
            started.elapsed() <= cap + DEBOUNCE,
            "{:?}",
            started.elapsed()
        );
        assert!(
            burst.len() > 1,
            "the burst carries what arrived before the cap"
        );
        writer.abort();
    }
}
