//! Git auto-sync (T-D06, DESIGN §8.7). Compiled only under the default-on
//! `git-sync` feature so a `--no-default-features` build is verifiably free of
//! vendored libgit2.
//!
//! Opt-in via `[daemon] git_sync = true`. After a watcher batch, each changed
//! item file that (a) parses cleanly, (b) has uncommitted changes, and (c) is not
//! mid-merge/mid-rebase, is `git add`-ed and committed via `git2` (no subprocess,
//! so it bypasses any commit-signing hook). **Never pushes** — push stays the
//! user's job.
//!
//! Re-commit guard: committing a file leaves it unmodified versus `HEAD`, so the
//! next batch's status check skips it. The schema-v3 `file_mtimes.synced_at`
//! column records the sync for observability (DESIGN §8.7).

use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use clove_index::Index;
use git2::{Repository, RepositoryState, Signature, Status};

/// True when this binary was built with git auto-sync support.
#[allow(dead_code)] // surfaced via config diagnostics; reserved
pub fn available() -> bool {
    true
}

/// Auto-commit any of `files` that are clean-to-sync. Returns the number
/// committed. Best-effort: every failure mode is a silent skip (the file store is
/// the source of truth; a missed auto-commit just means the user commits later).
pub fn sync_files(repo_root: &Utf8Path, files: &[Utf8PathBuf], index: &Arc<Mutex<Index>>) -> usize {
    let repo = match Repository::open(repo_root.as_std_path()) {
        Ok(repo) => repo,
        Err(_) => return 0, // not a git repo → nothing to sync
    };

    // Skip the whole batch during an in-progress merge or rebase (DESIGN §8.7).
    if repo.state() != RepositoryState::Clean {
        return 0;
    }

    let mut committed = 0;
    for file in files {
        if commit_one(&repo, repo_root, file, index) {
            committed += 1;
        }
    }
    committed
}

/// Try to commit a single changed item file. Returns `true` if a commit was made.
fn commit_one(
    repo: &Repository,
    repo_root: &Utf8Path,
    file: &Utf8Path,
    index: &Arc<Mutex<Index>>,
) -> bool {
    // Malformed-skip guard: only commit files whose frontmatter parses (avoids
    // committing a half-written mid-edit autosave).
    if clove_core::parse_item_file(file).is_err() {
        return false;
    }
    let Ok(rel) = file.strip_prefix(repo_root) else {
        return false;
    };
    let rel_std = rel.as_std_path();

    // Only commit modified-but-uncommitted files. This is also the re-commit
    // guard: after a commit the file is no longer dirty, so the next batch skips.
    let status = match repo.status_file(rel_std) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let created = status.intersects(Status::WT_NEW | Status::INDEX_NEW);
    let modified = status.intersects(Status::WT_MODIFIED | Status::INDEX_MODIFIED);
    if !created && !modified {
        return false;
    }

    let change = if created { "created" } else { "modified" };
    let id = file.file_stem().unwrap_or("item");
    let message = format!("clove: auto-sync {id} [{change}]");

    if commit_file(repo, file, rel_std, &message).is_err() {
        return false;
    }

    // Record the sync (best-effort; schema-v3 file_mtimes.synced_at, §8.7).
    if let (Ok(idx), Some(name)) = (index.lock(), file.file_name()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let _ = idx.set_synced_at(name, now);
    }
    true
}

/// Commit `rel_path`'s current worktree content on `HEAD` with `message`.
///
/// The commit tree is built from HEAD's tree plus ONLY this file's blob —
/// never from the repository index, whose `write_tree()` snapshots *every*
/// staged entry: with unrelated user-staged changes present, the old
/// implementation silently swept them into the auto-sync commit (attributing
/// them to the daemon and emptying the user's staging area).
fn commit_file(
    repo: &Repository,
    abs_path: &Utf8Path,
    rel_path: &std::path::Path,
    message: &str,
) -> Result<(), git2::Error> {
    let parent = repo.head()?.peel_to_commit()?;
    let head_tree = parent.tree()?;

    // HEAD's tree + this one blob, assembled in an in-memory index.
    let blob = repo.blob_path(abs_path.as_std_path())?;
    let mut staging = git2::Index::new()?;
    staging.read_tree(&head_tree)?;
    let path_bytes = rel_path.to_string_lossy().replace('\\', "/").into_bytes();
    staging.add(&git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100_644,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: blob,
        flags: 0,
        flags_extended: 0,
        path: path_bytes,
    })?;
    let tree = repo.find_tree(staging.write_tree_to(repo)?)?;

    let signature = repo
        .signature()
        .or_else(|_| Signature::now("clove-daemon", "clove-daemon@localhost"))?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &[&parent],
    )?;

    // Refresh the real index entry for THIS path only, so the file reads clean
    // against the new HEAD (the re-commit guard checks status). Other staged
    // entries are untouched.
    let mut real_index = repo.index()?;
    real_index.add_path(rel_path)?;
    real_index.write()?;
    Ok(())
}
