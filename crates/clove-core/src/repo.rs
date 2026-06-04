//! Discovery of the `.clove/` root for the current repository.
//!
//! `clove` is a **per-project** tracker: all of a project's git worktrees share a
//! single `.clove/` (the main worktree's) and therefore a single index/daemon —
//! work items belong to the project, not to a branch (DESIGN §8.1). So discovery
//! resolves a linked worktree to the **main worktree's** `.clove/`, even when the
//! linked worktree has its own checked-out `.clove/`.
//!
//! The common case — the main worktree — is a pure-Rust ancestor walk with no
//! subprocess (`.git` is a directory there), keeping per-command latency within
//! the targets in DESIGN.md §13.1. Only inside a *linked* worktree (where `.git`
//! is a file) do we shell out to `git rev-parse --git-common-dir` to find the
//! main worktree root.
//!
//! `git2` is intentionally not used here: its vendored libgit2 C build blocks
//! macOS→Windows cross-compilation, and a one-shot `git` invocation on the rare
//! worktree path is sufficient. (git2 returns in M3 for the daemon's git
//! auto-sync, where richer git access is genuinely needed.)

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

/// Find the repository root: the directory that contains the project's `.clove/`.
///
/// In a **linked git worktree**, resolves to the **main worktree's** root so all
/// worktrees of a project share one `.clove/` (and one index/daemon) — the main
/// worktree's `.clove/` wins over any branch-local checkout. Otherwise returns the
/// nearest ancestor of `start` that contains a `.clove/` directory. Returns `None`
/// if no `.clove/` can be located.
pub fn find_repo_root(start: &Utf8Path) -> Option<Utf8PathBuf> {
    // Canonicalize so `ancestors()` walks real directories (and so the worktree
    // resolution compares canonical paths). Fall back to the literal path if the
    // start does not exist on disk.
    let start_abs = start
        .canonicalize_utf8()
        .unwrap_or_else(|_| start.to_owned());

    let mut local_clove: Option<Utf8PathBuf> = None;
    let mut linked_worktree = false; // `.git` is a *file* → a linked worktree
    let mut inside_git = false;
    for ancestor in start_abs.ancestors() {
        if local_clove.is_none() && ancestor.join(".clove").is_dir() {
            local_clove = Some(ancestor.to_owned());
        }
        let dotgit = ancestor.join(".git");
        if dotgit.is_file() {
            linked_worktree = true;
            inside_git = true;
        } else if dotgit.is_dir() {
            inside_git = true;
        }
    }

    // A linked worktree shares the main worktree's `.clove/` (per-project tool):
    // prefer the main worktree's `.clove/` over any branch-local checkout.
    if linked_worktree {
        if let Some(main_root) = main_worktree_root_via_git(&start_abs) {
            if main_root.join(".clove").is_dir() {
                return Some(main_root);
            }
        }
    }

    // Otherwise the locally-discovered `.clove/` is the project root.
    if let Some(root) = local_clove {
        return Some(root);
    }

    // No local `.clove/` but inside a (non-worktree) git repo: the project's
    // `.clove/` may live in the main worktree (e.g. a branch without it checked
    // out) — resolve there.
    if inside_git && !linked_worktree {
        if let Some(main_root) = main_worktree_root_via_git(&start_abs) {
            if main_root.join(".clove").is_dir() {
                return Some(main_root);
            }
        }
    }

    None
}

/// Find the `.clove/issues/` directory for the repository containing `start`.
pub fn find_issues_dir(start: &Utf8Path) -> Option<Utf8PathBuf> {
    find_repo_root(start).map(|root| root.join(".clove").join("issues"))
}

/// Resolve the main worktree root by asking `git` for the common git directory.
///
/// `git rev-parse --git-common-dir` yields the shared `.git` directory
/// (`<main-root>/.git`) regardless of whether `cwd` is the main checkout or a
/// linked worktree; its parent is the main worktree root. Returns `None` if
/// `git` is unavailable, errors, or `cwd` is not in a repository.
fn main_worktree_root_via_git(cwd: &Utf8Path) -> Option<Utf8PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd.as_std_path())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // The reported common dir may be relative to `cwd`.
    let common_dir = {
        let candidate = Utf8PathBuf::from(trimmed);
        if candidate.is_absolute() {
            candidate
        } else {
            cwd.join(candidate)
        }
    };
    let common_dir = common_dir.canonicalize_utf8().ok()?;

    // `<main-root>/.git` → `<main-root>`.
    common_dir.parent().map(Utf8Path::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(path: &std::path::Path) -> Utf8PathBuf {
        Utf8Path::from_path(path)
            .unwrap()
            .canonicalize_utf8()
            .unwrap()
    }

    /// Run `git` in `cwd`, asserting success. Disables commit signing so the
    /// commits these tests make do not depend on a configured signing key
    /// (CI/sandboxes may force `commit.gpgsign=true`).
    fn git(args: &[&str], cwd: &Utf8Path) {
        let out = Command::new("git")
            .arg("-c")
            .arg("commit.gpgsign=false")
            .args(args)
            .current_dir(cwd.as_std_path())
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn finds_root_when_cwd_is_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();

        assert_eq!(find_repo_root(&root), Some(root.clone()));
        assert_eq!(
            find_issues_dir(&root),
            Some(root.join(".clove").join("issues"))
        );
    }

    #[test]
    fn finds_root_from_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
        let nested = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_repo_root(&nested), Some(root));
    }

    #[test]
    fn returns_none_without_clove_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let nested = root.join("x").join("y");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_repo_root(&nested), None);
        assert_eq!(find_issues_dir(&nested), None);
    }

    #[test]
    fn linked_worktree_resolves_to_main_worktree() {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping: git binary not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path());
        let main = base.join("main");
        std::fs::create_dir_all(&main).unwrap();

        git(&["init", "-q"], &main);
        git(&["config", "user.email", "test@example.com"], &main);
        git(&["config", "user.name", "Test"], &main);
        git(&["commit", "-q", "--allow-empty", "-m", "init"], &main);

        // `.clove/` lives only in the main worktree.
        std::fs::create_dir_all(main.join(".clove").join("issues")).unwrap();

        // Add a linked worktree alongside it.
        let linked = base.join("linked");
        git(&["worktree", "add", "-q", linked.as_str()], &main);
        assert!(
            linked.join(".git").is_file(),
            "linked worktree has a .git file"
        );

        // From inside the linked worktree (no local .clove/), discovery must
        // resolve to the main worktree root.
        let main_canonical = main.canonicalize_utf8().unwrap();
        assert_eq!(find_repo_root(&linked), Some(main_canonical));
    }

    /// Worktrees of a project share the main worktree's `.clove/` — even a linked
    /// worktree that has its OWN checked-out `.clove/` must resolve to main, so
    /// all worktrees use one index/daemon (clove is per-project, not per-branch).
    #[test]
    fn linked_worktree_with_local_clove_still_resolves_to_main() {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping: git binary not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let base = canonical(tmp.path());
        let main = base.join("main");
        std::fs::create_dir_all(&main).unwrap();

        git(&["init", "-q"], &main);
        git(&["config", "user.email", "test@example.com"], &main);
        git(&["config", "user.name", "Test"], &main);
        // Commit a `.clove/` so it is checked out in every worktree.
        std::fs::create_dir_all(main.join(".clove").join("issues")).unwrap();
        std::fs::write(main.join(".clove").join("config.toml"), "schema = 1\n").unwrap();
        git(&["add", "-A"], &main);
        git(&["commit", "-q", "-m", "add clove"], &main);

        let linked = base.join("linked");
        git(&["worktree", "add", "-q", linked.as_str()], &main);
        // The linked worktree DOES have its own checked-out `.clove/`...
        assert!(linked.join(".clove").is_dir(), "linked worktree has .clove");

        // ...but discovery still resolves to the main worktree's `.clove/`.
        let main_canonical = main.canonicalize_utf8().unwrap();
        assert_eq!(find_repo_root(&linked), Some(main_canonical));
    }
}
