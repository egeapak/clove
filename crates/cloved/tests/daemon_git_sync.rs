//! Phase 5 (T-D06) git auto-sync tests. Feature-gated: only built/run with
//! `git-sync` (the default). Unit-level — they drive `git_sync::sync_files`
//! directly with a git2-built repo (so no `git` CLI / commit-signing hook).
#![cfg(feature = "git-sync")]

use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use clove_core::{ItemStore, NewItem};
use clove_index::Index;
use clove_types::{ItemType, Priority};
use git2::{Repository, Signature};

// `cloved` is a binary crate, so its modules aren't importable; include the real
// git_sync source directly to test the actual function the daemon runs.
#[path = "../src/git_sync.rs"]
mod git_sync;

struct Repo {
    _tmp: tempfile::TempDir,
    root: Utf8PathBuf,
    repo: Repository,
}

fn init_repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap().to_owned();
    let repo = Repository::init(root.as_std_path()).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
    }
    std::fs::create_dir_all(root.join(".clove/issues")).unwrap();
    std::fs::write(
        root.join(".clove/config.toml"),
        "schema = 1\nid_prefix = \"proj\"\n",
    )
    .unwrap();
    // Initial commit so HEAD has a parent. Scoped so all borrows of `repo` end
    // before it is moved into the returned struct.
    {
        std::fs::write(root.join("README"), b"init\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("README")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    Repo {
        _tmp: tmp,
        root,
        repo,
    }
}

impl Repo {
    fn add_item(&self, title: &str) -> Utf8PathBuf {
        let store = ItemStore::new(self.root.clone());
        let item = store
            .create(
                "proj",
                NewItem {
                    title: title.to_owned(),
                    item_type: ItemType::Feature,
                    priority: Priority(1),
                    labels: Vec::new(),
                    deps: Vec::new(),
                    parent: None,
                    assignee: None,
                    body: String::new(),
                },
                Utc::now(),
            )
            .unwrap();
        self.root
            .join(".clove/issues")
            .join(format!("{}.md", item.frontmatter.id))
    }

    fn commit_count(&self) -> usize {
        let mut walk = self.repo.revwalk().unwrap();
        walk.push_head().unwrap();
        walk.count()
    }

    fn head_message(&self) -> String {
        self.repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .message()
            .unwrap()
            .to_owned()
    }
}

fn throwaway_index(root: &Utf8Path) -> Arc<Mutex<Index>> {
    Arc::new(Mutex::new(
        Index::open_or_create(&root.join(".clove/index.db")).unwrap(),
    ))
}

#[test]
fn commits_a_new_item_and_does_not_recommit() {
    let repo = init_repo();
    let path = repo.add_item("alpha");
    let index = throwaway_index(&repo.root);
    let before = repo.commit_count();

    // First sync commits the new file.
    let n = git_sync::sync_files(&repo.root, std::slice::from_ref(&path), &index);
    assert_eq!(n, 1, "one file committed");
    assert_eq!(repo.commit_count(), before + 1);
    assert!(repo.head_message().contains("auto-sync"));
    assert!(repo.head_message().contains("created"));

    // Second sync with no change must NOT create another commit (re-commit guard).
    let n = git_sync::sync_files(&repo.root, &[path], &index);
    assert_eq!(n, 0, "clean file is not re-committed");
    assert_eq!(repo.commit_count(), before + 1);
}

#[test]
fn skips_during_merge() {
    let repo = init_repo();
    let path = repo.add_item("beta");
    let index = throwaway_index(&repo.root);
    // Simulate an in-progress merge.
    std::fs::write(repo.root.join(".git/MERGE_HEAD"), b"deadbeef\n").unwrap();
    let before = repo.commit_count();

    let n = git_sync::sync_files(&repo.root, &[path], &index);
    assert_eq!(n, 0, "no commit during a merge");
    assert_eq!(repo.commit_count(), before);
}

#[test]
fn skips_during_rebase() {
    let repo = init_repo();
    let path = repo.add_item("gamma");
    let index = throwaway_index(&repo.root);
    // Simulate an in-progress rebase.
    std::fs::create_dir_all(repo.root.join(".git/rebase-merge")).unwrap();
    let before = repo.commit_count();

    let n = git_sync::sync_files(&repo.root, &[path], &index);
    assert_eq!(n, 0, "no commit during a rebase");
    assert_eq!(repo.commit_count(), before);
}

#[test]
fn skips_malformed_then_commits_when_fixed() {
    let repo = init_repo();
    let path = repo.add_item("delta");
    let index = throwaway_index(&repo.root);

    // Corrupt the frontmatter (unterminated) → malformed-skip.
    std::fs::write(&path, b"---\nid: proj-BADBADBA\n").unwrap();
    let before = repo.commit_count();
    let n = git_sync::sync_files(&repo.root, std::slice::from_ref(&path), &index);
    assert_eq!(n, 0, "malformed file is not committed");
    assert_eq!(repo.commit_count(), before);

    // Restore a valid item → now it commits.
    let valid = repo.add_item("delta-fixed");
    let n = git_sync::sync_files(&repo.root, &[valid], &index);
    assert_eq!(n, 1);
    assert_eq!(repo.commit_count(), before + 1);
}

#[test]
fn never_configures_a_remote_or_pushes() {
    // Auto-sync only ever add+commit; it must not introduce a remote.
    let repo = init_repo();
    let path = repo.add_item("epsilon");
    let index = throwaway_index(&repo.root);
    git_sync::sync_files(&repo.root, &[path], &index);
    assert!(
        repo.repo.remotes().unwrap().is_empty(),
        "auto-sync must never add a remote / push"
    );
}
