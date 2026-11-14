//! Whether this clone allowed the daemon to act on its own.
//!
//! `[daemon] git_sync` makes the daemon commit under the user's git identity,
//! and `github_sync_*` makes it talk to GitHub with the user's token. Both come
//! from `.clove/config.toml`, which arrives with the repository: taken at its
//! word, a fresh clone would start committing and syncing as soon as any
//! command loaded the project. So the daemon acts on those keys only when the
//! clone itself says so, in git configuration (`git config clove.automation
//! true`), which a clone never carries.

use camino::Utf8Path;

/// True when the git configuration of the repository at `repo_root` sets
/// `clove.automation` to true. Any level counts (the clone's `.git/config`, or
/// the user's global file): all of them are the user's, none is repository
/// content.
#[cfg(feature = "git-sync")]
pub fn allowed(repo_root: &Utf8Path) -> bool {
    crate::github_remote::open_work_tree(repo_root)
        .ok()
        .and_then(|repo| repo.config().ok())
        .and_then(|config| config.get_bool(clove_core::AUTOMATION_GIT_CONFIG_KEY).ok())
        .unwrap_or(false)
}

/// Without git support the configuration cannot be read, and neither kind of
/// automation can run.
#[cfg(not(feature = "git-sync"))]
pub fn allowed(_repo_root: &Utf8Path) -> bool {
    false
}

#[cfg(all(test, feature = "git-sync"))]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, camino::Utf8PathBuf, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, root, repo)
    }

    #[test]
    fn a_fresh_clone_has_not_allowed_automation() {
        let (_tmp, root, _repo) = repo();
        assert!(!allowed(&root));
    }

    #[test]
    fn the_clones_git_config_allows_it() {
        let (_tmp, root, repo) = repo();
        let mut config = repo
            .config()
            .unwrap()
            .open_level(git2::ConfigLevel::Local)
            .unwrap();
        config.set_bool("clove.automation", true).unwrap();
        assert!(allowed(&root));
        config.set_bool("clove.automation", false).unwrap();
        assert!(!allowed(&root));
    }

    /// A file in the work tree cannot grant it: only git configuration counts.
    #[test]
    fn repository_content_cannot_allow_it() {
        let (_tmp, root, _repo) = repo();
        std::fs::write(root.join(".gitconfig"), "[clove]\n\tautomation = true\n").unwrap();
        std::fs::create_dir_all(root.join(".clove")).unwrap();
        std::fs::write(
            root.join(".clove/config.toml"),
            "[daemon]\ngit_sync = true\nautomation = true\n",
        )
        .unwrap();
        assert!(!allowed(&root));
        assert!(!allowed(&root.join("no-such-dir")));
    }
}
