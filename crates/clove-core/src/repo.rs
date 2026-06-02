//! Discovery of the `.clove/` root for the current repository.
//!
//! The common case — find the nearest ancestor directory containing `.clove/` —
//! is a pure-Rust ancestor walk with no subprocess, keeping per-command latency
//! within the targets in DESIGN.md §13.1. Only when no local `.clove/` is found
//! *and* we appear to be inside a git repository do we fall back to delegating
//! to the `git` binary (`git rev-parse --git-common-dir`) to resolve the main
//! worktree's root — the linked-worktree case, where `.clove/` lives in the
//! main checkout rather than the linked one.
//!
//! `git2` is intentionally not used here: its vendored libgit2 C build blocks
//! macOS→Windows cross-compilation, and a one-shot `git` invocation on the rare
//! worktree-fallback path is sufficient. (git2 returns in M3 for the daemon's
//! git auto-sync, where richer git access is genuinely needed.)

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};

/// Find the repository root: the nearest ancestor of `start` that contains a
/// `.clove/` directory. Returns the directory that *contains* `.clove/`.
///
/// If no ancestor has `.clove/` but we are inside a git worktree, resolves the
/// main worktree root via `git` and checks there (linked-worktree support).
/// Returns `None` if no `.clove/` can be located.
pub fn find_repo_root(start: &Utf8Path) -> Option<Utf8PathBuf> {
    // Canonicalize so `ancestors()` walks real directories (and so the worktree
    // fallback compares canonical paths). Fall back to the literal path if the
    // start does not exist on disk.
    let start_abs = start
        .canonicalize_utf8()
        .unwrap_or_else(|_| start.to_owned());

    let mut inside_git = false;
    for ancestor in start_abs.ancestors() {
        if ancestor.join(".clove").is_dir() {
            return Some(ancestor.to_owned());
        }
        // A `.git` entry (directory in a normal checkout, file in a linked
        // worktree) tells us a git fallback is worth attempting.
        if ancestor.join(".git").exists() {
            inside_git = true;
        }
    }

    if inside_git {
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

        let git = |args: &[&str], cwd: &Utf8Path| {
            let status = Command::new("git")
                .args(args)
                .current_dir(cwd.as_std_path())
                .output()
                .expect("git runs");
            assert!(
                status.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&status.stderr)
            );
        };

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
}
