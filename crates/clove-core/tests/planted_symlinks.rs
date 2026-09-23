//! Everything under `.clove/` arrives with the repository, so a clone can plant
//! a symlink where clove expects a file or a directory. Writing through one
//! would land outside the project; reading through one would serve another
//! directory's files as the project's. Each test plants one and checks the
//! target is left alone.
#![cfg(unix)]

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use clove_core::{ItemStore, NewItem};
use clove_types::{ItemType, Priority};

struct Fixture {
    _tmp: tempfile::TempDir,
    root: Utf8PathBuf,
    /// A directory outside the project that a planted link points at.
    elsewhere: Utf8PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = Utf8Path::from_path(tmp.path()).unwrap().to_owned();
    let root = base.join("repo");
    std::fs::create_dir_all(root.join(".clove")).unwrap();
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    Fixture {
        _tmp: tmp,
        root,
        elsewhere,
    }
}

fn spec(title: &str) -> NewItem {
    NewItem {
        title: title.to_owned(),
        item_type: ItemType::Feature,
        priority: Priority::DEFAULT,
        labels: Vec::new(),
        deps: Vec::new(),
        parent: None,
        assignee: None,
        body: String::new(),
    }
}

fn entries(dir: &Utf8Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn doctor_fix_never_appends_through_a_symlinked_gitignore() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join(".clove/issues")).unwrap();
    let victim = fx.elsewhere.join("precious");
    std::fs::write(&victim, "precious\n").unwrap();
    std::os::unix::fs::symlink(&victim, fx.root.join(".clove/.gitignore")).unwrap();
    let store = ItemStore::new(fx.root.clone());
    let _ = clove_core::doctor_fix(&store);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious\n");
}

#[test]
fn a_symlinked_issues_directory_is_neither_written_nor_read() {
    let fx = fixture();
    std::os::unix::fs::symlink(&fx.elsewhere, fx.root.join(".clove/issues")).unwrap();
    let store = ItemStore::new(fx.root.clone());
    assert!(store.create("proj", spec("planted"), Utc::now()).is_err());
    assert!(
        entries(&fx.elsewhere).is_empty(),
        "{:?}",
        entries(&fx.elsewhere)
    );
    assert!(store.scan().is_err(), "a symlinked issues dir was scanned");
}

#[test]
fn a_symlinked_comment_directory_is_not_written_through() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join(".clove/issues")).unwrap();
    let store = ItemStore::new(fx.root.clone());
    let item = store.create("proj", spec("real"), Utc::now()).unwrap();
    let id = item.frontmatter.id;
    std::os::unix::fs::symlink(&fx.elsewhere, store.item_dir(&id)).unwrap();
    assert!(clove_core::add_comment(store.issues_dir(), &id, "me@example.com", "hi").is_err());
    assert!(
        entries(&fx.elsewhere).is_empty(),
        "{:?}",
        entries(&fx.elsewhere)
    );
}
