//! Which GitHub repository the daemon may sync a project with: only one of that
//! project's own git remotes.
//!
//! `[daemon] github_sync_repo` comes from the project's `.clove/config.toml`,
//! which arrives with the repository. Taken at its word, a cloned repo could
//! point the daemon — running with the user's GitHub credentials — at any
//! repository the user can reach and two-way sync it. So the daemon only syncs
//! with a repository the project itself names as a remote. (`clove sync github
//! <owner/repo>` typed by the user is the user's own choice and is not gated.)
#![cfg_attr(not(feature = "github-sync"), allow(dead_code))]

/// `owner/repo`, lowercased, when `url` is a GitHub remote — `https://`,
/// `http://`, `git://`, `ssh://git@github.com/…`, or scp-like
/// `git@github.com:…` — with or without a trailing `.git`.
pub fn github_repo_of(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        let (_, rest) = url.split_once("://")?;
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host.split(':').next().unwrap_or(host);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };
    owner_repo(path)
}

/// A `github_sync_repo` setting as `owner/repo`, lowercased: either that form
/// already, or a GitHub URL.
pub fn normalize_spec(spec: &str) -> Option<String> {
    github_repo_of(spec).or_else(|| owner_repo(spec.trim()))
}

fn owner_repo(path: &str) -> Option<String> {
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (valid(owner) && valid(repo)).then(|| format!("{owner}/{repo}").to_ascii_lowercase())
}

/// Check that `spec` names one of the GitHub remotes of the repository at
/// `repo_root`; returns the normalized `owner/repo`, or why it may not be synced.
pub fn check_sync_target(repo_root: &camino::Utf8Path, spec: &str) -> Result<String, String> {
    let wanted =
        normalize_spec(spec).ok_or_else(|| format!("{spec:?} is not an owner/repo spec"))?;
    let remotes = project_github_repos(repo_root)?;
    if remotes.contains(&wanted) {
        Ok(wanted)
    } else if remotes.is_empty() {
        Err(format!(
            "{wanted} is not a remote of {repo_root} (it has no GitHub remotes)"
        ))
    } else {
        Err(format!(
            "{wanted} is not a remote of {repo_root} (its GitHub remotes: {})",
            remotes.join(", ")
        ))
    }
}

/// The repository whose work tree is exactly `repo_root`, through its own
/// `.git` (a directory, or a gitfile for a linked work tree). Nothing else is
/// trusted: a bare repository, or any directory of repository files a clone
/// can carry, would let the project's content choose its remotes.
#[cfg(feature = "git-sync")]
fn open_work_tree(repo_root: &camino::Utf8Path) -> Result<git2::Repository, String> {
    let not_a_root = || format!("{repo_root} is not the root of a git work tree");
    let dot_git = repo_root.join(".git");
    let dot_git_kind = std::fs::symlink_metadata(&dot_git).map_err(|_| not_a_root())?;
    if !dot_git_kind.is_dir() && !dot_git_kind.is_file() {
        return Err(not_a_root());
    }
    let repo = git2::Repository::open_ext(
        repo_root.as_std_path(),
        git2::RepositoryOpenFlags::NO_SEARCH,
        std::iter::empty::<&std::ffi::OsStr>(),
    )
    .map_err(|e| format!("{} ({})", not_a_root(), e.message()))?;
    let same = |a: &std::path::Path, b: &std::path::Path| {
        let (a, b) = (a.canonicalize(), b.canonicalize());
        a.is_ok() && a.ok() == b.ok()
    };
    let work_tree_is_root = repo
        .workdir()
        .is_some_and(|workdir| same(workdir, repo_root.as_std_path()));
    let git_dir_is_own = !dot_git_kind.is_dir() || same(repo.path(), dot_git.as_std_path());
    if repo.is_bare() || !work_tree_is_root || !git_dir_is_own {
        return Err(not_a_root());
    }
    Ok(repo)
}

/// The GitHub repositories the git repository at `repo_root` has as remotes.
#[cfg(feature = "git-sync")]
fn project_github_repos(repo_root: &camino::Utf8Path) -> Result<Vec<String>, String> {
    let repo = open_work_tree(repo_root)?;
    let names = repo.remotes().map_err(|e| e.message().to_owned())?;
    let mut repos: Vec<String> = names
        .iter()
        .filter_map(|name| name.ok().flatten())
        .filter_map(|name| repo.find_remote(name).ok())
        .flat_map(|remote| {
            [remote.url().ok(), remote.pushurl().ok().flatten()]
                .into_iter()
                .flatten()
                .filter_map(github_repo_of)
                .collect::<Vec<_>>()
        })
        .collect();
    repos.sort();
    repos.dedup();
    Ok(repos)
}

/// Without git support the remotes cannot be read, so nothing is allowed.
#[cfg(not(feature = "git-sync"))]
fn project_github_repos(_repo_root: &camino::Utf8Path) -> Result<Vec<String>, String> {
    Err(
        "this cloved was built without git support, so it cannot read the \
         project's remotes"
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_remote_urls_in_every_form() {
        for url in [
            "https://github.com/Owner/Repo.git",
            "https://github.com/owner/repo",
            "http://github.com/owner/repo/",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "ssh://git@github.com:22/owner/repo",
            "git://github.com/owner/repo.git",
            "https://token@github.com/owner/repo.git",
        ] {
            assert_eq!(github_repo_of(url).as_deref(), Some("owner/repo"), "{url}");
        }
    }

    #[test]
    fn other_hosts_and_shapes_are_not_github_remotes() {
        for url in [
            "https://gitlab.com/owner/repo.git",
            "git@gitlab.com:owner/repo.git",
            "https://github.com.evil.example/owner/repo",
            "https://github.com/owner",
            "/local/path/repo.git",
        ] {
            assert_eq!(github_repo_of(url), None, "{url}");
        }
    }

    #[test]
    fn specs_normalize_like_remotes() {
        assert_eq!(normalize_spec("Owner/Repo").as_deref(), Some("owner/repo"));
        assert_eq!(
            normalize_spec("https://github.com/owner/repo.git").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(normalize_spec("owner/repo/extra"), None);
        assert_eq!(normalize_spec("nope"), None);
    }

    #[cfg(feature = "git-sync")]
    fn repo_with_remotes(remotes: &[(&str, &str)]) -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        for (name, url) in remotes {
            repo.remote(name, url).unwrap();
        }
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, root)
    }

    /// The daemon syncs a project only with a GitHub repository the project
    /// itself names as a remote.
    #[cfg(feature = "git-sync")]
    #[test]
    fn only_the_projects_own_remote_may_be_synced() {
        let (_tmp, root) = repo_with_remotes(&[
            ("origin", "git@github.com:me/project.git"),
            ("upstream", "https://github.com/org/project"),
        ]);
        assert_eq!(
            check_sync_target(&root, "me/project").unwrap(),
            "me/project"
        );
        assert_eq!(
            check_sync_target(&root, "https://github.com/ORG/project.git").unwrap(),
            "org/project"
        );
        let refused = check_sync_target(&root, "victim/private-repo").unwrap_err();
        assert!(refused.contains("not a remote"), "{refused}");
    }

    /// A repository that ships a directory holding `HEAD`, `objects/`, `refs/`
    /// and a `config` naming another remote must not be read as the project's
    /// repository: only the work tree's own `.git` counts.
    #[cfg(feature = "git-sync")]
    #[test]
    fn a_committed_repository_directory_is_not_the_projects_repository() {
        let (_tmp, outer) = repo_with_remotes(&[("origin", "git@github.com:me/project.git")]);
        let planted = outer.join("sub");
        let bare = git2::Repository::init_bare(planted.as_std_path()).unwrap();
        bare.remote("origin", "https://github.com/victim/private")
            .unwrap();
        let refused = check_sync_target(&planted, "victim/private").unwrap_err();
        assert!(
            refused.contains("not the root of a git work tree"),
            "{refused}"
        );

        // The same directory claiming to be its own work tree.
        let mut config = bare.config().unwrap();
        config.set_bool("core.bare", false).unwrap();
        config.set_str("core.worktree", planted.as_str()).unwrap();
        let refused = check_sync_target(&planted, "victim/private").unwrap_err();
        assert!(
            refused.contains("not the root of a git work tree"),
            "{refused}"
        );
    }

    #[cfg(feature = "git-sync")]
    #[test]
    fn a_linked_work_tree_reads_its_repositorys_remotes() {
        let (_tmp, root) = repo_with_remotes(&[("origin", "git@github.com:me/project.git")]);
        let repo = git2::Repository::open(root.as_std_path()).unwrap();
        let signature = git2::Signature::now("t", "t@example.com").unwrap();
        let tree = repo
            .find_tree(repo.index().unwrap().write_tree().unwrap())
            .unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .unwrap();
        let linked = tempfile::tempdir().unwrap();
        let linked_root = camino::Utf8PathBuf::from_path_buf(linked.path().join("wt")).unwrap();
        repo.worktree("wt", linked_root.as_std_path(), None)
            .unwrap();
        assert_eq!(
            check_sync_target(&linked_root, "me/project").unwrap(),
            "me/project"
        );
    }

    #[cfg(feature = "git-sync")]
    #[test]
    fn a_project_outside_git_syncs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        assert!(check_sync_target(&root, "me/project").is_err());
    }
}
