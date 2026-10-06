//! The project's sync targets, as the SPA needs them.
//!
//! `clove sync github <owner/repo>` records its per-repo state under
//! `.clove/sync/github/<owner>_<repo>.json` (see `clove_import::sync::SyncState`),
//! and that file is the only local record of *which* repository a project syncs
//! with. The web reads just its `repo` field — this crate does not link
//! `clove-import`, since `cloved` hosts it and deliberately carries no importer —
//! so the SPA can turn an item's `external_ref: gh-67` into a link to the issue
//! and tell synced items from unsynced ones.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

/// One repository the project syncs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncTarget {
    /// The provider, which is also the `source_system` its items carry.
    pub provider: &'static str,
    /// `owner/name`.
    pub repo: String,
    /// The repository's web URL.
    pub url: String,
    /// Prefix for an issue link: append the issue number.
    pub issue_url: String,
}

#[derive(Deserialize)]
struct StateFile {
    #[serde(default)]
    repo: String,
}

/// A GitHub `owner/name`, checked before it is spliced into a URL: the state
/// file arrives with the working tree, so its contents are not trusted.
fn valid_repo(repo: &str) -> bool {
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(repo.split_once('/'), Some((owner, name)) if segment_ok(owner) && segment_ok(name))
}

/// The sync targets recorded under `clove_dir` (a store's `.clove/`), sorted by
/// repo. Unreadable, unparseable, or symlinked state is skipped, never an error:
/// this only decorates the UI.
pub fn targets(clove_dir: &Utf8Path) -> Vec<SyncTarget> {
    let dir = clove_dir.join("sync").join("github");
    if clove_core::fs_safe::check_dirs(clove_dir, &dir).is_err() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut repos: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str::<StateFile>(&text).ok())
        .map(|state| state.repo.trim().to_owned())
        .filter(|repo| valid_repo(repo))
        .collect();
    repos.sort();
    repos.dedup();
    repos
        .into_iter()
        .map(|repo| {
            let url = format!("https://github.com/{repo}");
            SyncTarget {
                provider: "github",
                issue_url: format!("{url}/issues/"),
                url,
                repo,
            }
        })
        .collect()
}

/// Whether an `external_ref` links the item to a GitHub issue (`gh-<number>`),
/// the spelling `clove sync github` writes.
pub fn is_synced(external_ref: Option<&str>) -> bool {
    external_ref
        .and_then(|r| r.trim().strip_prefix("gh-"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn clove_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(tmp.path().join(".clove")).unwrap();
        std::fs::create_dir_all(dir.join("sync").join("github")).unwrap();
        (tmp, dir)
    }

    #[test]
    fn reads_the_repo_from_each_state_file() {
        let (_tmp, dir) = clove_dir();
        let github = dir.join("sync").join("github");
        std::fs::write(
            github.join("egeapak_clove.json"),
            r#"{"version":1,"repo":"egeapak/clove","entries":{}}"#,
        )
        .unwrap();
        std::fs::write(github.join("egeapak_clove.lock"), "").unwrap();
        std::fs::write(github.join("broken.json"), "{not json").unwrap();
        std::fs::write(github.join("evil.json"), r#"{"repo":"a/b?x=<script>"}"#).unwrap();
        std::fs::write(github.join("dots.json"), r#"{"repo":"../.."}"#).unwrap();

        let targets = targets(&dir);
        assert_eq!(
            targets,
            vec![SyncTarget {
                provider: "github",
                repo: "egeapak/clove".to_owned(),
                url: "https://github.com/egeapak/clove".to_owned(),
                issue_url: "https://github.com/egeapak/clove/issues/".to_owned(),
            }]
        );
    }

    #[test]
    fn no_sync_dir_means_no_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(tmp.path().join(".clove")).unwrap();
        assert!(targets(&dir).is_empty());
    }

    #[test]
    fn only_a_github_issue_ref_counts_as_synced() {
        assert!(is_synced(Some("gh-67")));
        assert!(!is_synced(None));
        assert!(!is_synced(Some("gh-")));
        assert!(!is_synced(Some("gh-6x")));
        assert!(!is_synced(Some("tk:abc-123")));
    }
}
