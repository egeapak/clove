//! Periodic two-way GitHub sync (T-M06 daemon integration).
//!
//! When `[daemon] github_sync_interval_min > 0` and `github_sync_repo` is set, a
//! running daemon reconciles the store with GitHub on that interval — the same
//! reconciliation `clove sync github` runs by hand, just unattended. It does this
//! by **spawning the `clove` CLI** (`clove sync github <repo>`), which resolves
//! the external `clove-sync-github` plugin (PLUGIN_SYSTEM.md §4.2/§8) — the exact
//! path a user would run. Unattended sync and manual sync share one code path,
//! and the daemon carries **no** octocrab/clove-import weight of its own. The
//! spawned `clove` writes item files through the normal store path, so the
//! daemon's own file-watcher then reindexes the result.
//!
//! Best-effort: a failed sync (offline, no token, rate-limited, plugin not
//! installed) is logged and retried next tick, never fatal to the daemon.

use std::process::Command;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};

/// Resolve the auto-sync interval. `0` minutes disables it (returns `None`).
/// `CLOVED_GITHUB_SYNC_MS` overrides it with sub-minute values for tests; `0`
/// there also disables.
pub fn github_sync_interval(interval_min: u64) -> Option<Duration> {
    if let Ok(ms) = std::env::var("CLOVED_GITHUB_SYNC_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            return (ms > 0).then(|| Duration::from_millis(ms));
        }
    }
    (interval_min > 0).then(|| Duration::from_secs(interval_min * 60))
}

/// Locate the sibling `clove` binary next to the running `cloved`, falling back
/// to a bare `clove` on `$PATH`. The two ship together (`cargo install clove-cli`
/// installs both into the same dir), so `current_exe`'s directory is the reliable
/// place to find it — matching the plugin search path's "adjacent binary" rule.
fn clove_binary() -> Utf8PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Utf8PathBuf::from_path_buf(exe).ok())
        .map(|exe| exe.with_file_name(format!("clove{}", std::env::consts::EXE_SUFFIX)))
        .filter(|candidate| candidate.exists())
        .unwrap_or_else(|| Utf8PathBuf::from("clove"))
}

/// Run one sync of the repository at `clove_dir` against `repo_spec` by spawning
/// `clove --clove-dir <clove_dir> sync github <repo_spec>` (which resolves the
/// `clove-sync-github` plugin). Best-effort: returns `false` (after logging) on a
/// spawn failure or a non-zero exit rather than propagating. The child inherits
/// this process's environment (so a `GITHUB_TOKEN` set for the daemon is passed
/// through) and streams its own stdout/stderr.
///
/// `--clove-dir` pins the child to the *exact* directory the daemon manages —
/// the daemon may have been started with a non-standard `--clove-dir` (a CLI
/// flag, not inherited into the child's env), so relying on cwd rediscovery could
/// aim the child at a different repository. Global flags precede the subcommand,
/// per the `clove` CLI contract.
pub fn github_sync_once(clove_dir: &Utf8Path, repo_spec: &str) -> bool {
    let clove = clove_binary();
    let status = Command::new(clove.as_std_path())
        .args([
            "--clove-dir",
            clove_dir.as_str(),
            "sync",
            "github",
            repo_spec,
        ])
        .status();
    match status {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!("github-sync {repo_spec}: `clove sync github` exited with {status}");
            false
        }
        Err(err) => {
            eprintln!("github-sync {repo_spec}: failed to spawn `{clove}` ({err})");
            false
        }
    }
}

/// Sync every `interval`, until cancelled. Never resolves when sync is disabled
/// (`interval`/`repo` absent). The first tick is consumed so no sync fires at
/// t=0 (the startup sweep already brought the index up to date). The spawn +
/// wait for the `clove` subprocess runs on a blocking thread, so it never stalls
/// this async worker.
pub async fn github_sync_loop(
    clove_dir: Utf8PathBuf,
    repo_spec: Option<String>,
    interval: Option<Duration>,
) {
    let (Some(interval), Some(repo_spec)) = (interval, repo_spec) else {
        std::future::pending::<()>().await;
        return;
    };
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await; // skip the immediate t=0 tick
    loop {
        ticker.tick().await;
        let clove_dir = clove_dir.clone();
        let repo_spec = repo_spec.clone();
        let _ = tokio::task::spawn_blocking(move || github_sync_once(&clove_dir, &repo_spec)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_zero_disables() {
        if std::env::var("CLOVED_GITHUB_SYNC_MS").is_err() {
            assert!(github_sync_interval(0).is_none());
            assert_eq!(github_sync_interval(30), Some(Duration::from_secs(1800)));
        }
    }
}
