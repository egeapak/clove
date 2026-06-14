//! Periodic two-way GitHub sync (T-M06 daemon integration).
//!
//! When `[daemon] github_sync_interval_min > 0` and `github_sync_repo` is set, a
//! running daemon reconciles the store with GitHub on that interval — the same
//! reconciliation `clove sync github` runs by hand, just unattended. It writes
//! item files through the normal store path, so the daemon's own file-watcher
//! then reindexes the result; nothing else here touches the index.
//!
//! Off by default and gated behind the `github-sync` feature, so a lean build
//! carries no octocrab weight. Best-effort: a failed sync (offline, no token,
//! rate-limited) is logged and retried next tick, never fatal to the daemon.

use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::{load_config, ItemStore};
use clove_import::{sync_net::sync_github, ConflictPolicy};

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

/// Run one sync of `repo_root` against `repo_spec`. Best-effort: returns `false`
/// (after logging) on any error rather than propagating. Default conflict policy
/// (newest wins) and comments on, matching the CLI defaults.
pub fn github_sync_once(repo_root: &Utf8Path, repo_spec: &str) -> bool {
    let config = match load_config(repo_root) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("github-sync: skipped (config load failed: {err})");
            return false;
        }
    };
    let store = ItemStore::new(repo_root.to_owned());
    match sync_github(
        repo_spec,
        &store,
        &config.id_prefix,
        ConflictPolicy::Newer,
        true,  // sync comments
        false, // not a dry run
    ) {
        Ok((_summary, Some(report))) => {
            eprintln!(
                "github-sync {repo_spec}: pulled {}/{}, pushed {}/{}, comments +{}/-{}, {} conflicts",
                report.pulled_created,
                report.pulled_updated,
                report.pushed_created,
                report.pushed_updated,
                report.comments_pulled,
                report.comments_pushed,
                report.conflicts,
            );
            true
        }
        Ok((_, None)) => true,
        Err(err) => {
            eprintln!("github-sync {repo_spec}: failed ({err})");
            false
        }
    }
}

/// Sync every `interval`, until cancelled. Never resolves when sync is disabled
/// (`interval`/`repo` absent). The first tick is consumed so no sync fires at
/// t=0 (the startup sweep already brought the index up to date). The network +
/// file IO runs on a blocking thread — `sync_github` spins up its own tokio
/// runtime, so it must not run on this async worker.
pub async fn github_sync_loop(
    repo_root: Utf8PathBuf,
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
        let repo_root = repo_root.clone();
        let repo_spec = repo_spec.clone();
        let _ = tokio::task::spawn_blocking(move || github_sync_once(&repo_root, &repo_spec)).await;
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
