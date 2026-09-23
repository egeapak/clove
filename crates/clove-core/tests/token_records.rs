//! The daemon-token records directory must be private to be believed — on
//! every read, not just when a record is made (DESIGN §8.4). Its own test
//! binary: the records directory is set once per process, and this one needs
//! one of its own to take apart.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::daemon_token;

fn chmod(path: &Utf8Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A record in a records directory someone else could have written to — or
/// reached through a symlink someone else could have pointed — vouches for
/// nothing: the token it records is not trusted, and no token is issued there.
#[test]
fn records_are_believed_only_in_a_private_directory() {
    let home = tempfile::tempdir().unwrap();
    let home = Utf8Path::from_path(home.path())
        .unwrap()
        .canonicalize_utf8()
        .unwrap();
    let records = home.join("daemon-tokens");
    daemon_token::use_records_dir(records.clone());

    let repo = tempfile::tempdir().unwrap();
    let clove_dir = Utf8Path::from_path(repo.path())
        .unwrap()
        .canonicalize_utf8()
        .unwrap()
        .join(".clove");
    std::fs::create_dir_all(&clove_dir).unwrap();
    let token = daemon_token::read_or_create(&clove_dir).unwrap();
    assert_eq!(daemon_token::read(&clove_dir).unwrap(), token);

    let project_records: Utf8PathBuf = std::fs::read_dir(&records)
        .unwrap()
        .flatten()
        .map(|entry| Utf8PathBuf::from_path_buf(entry.path()).unwrap())
        .find(|path| path.is_dir())
        .expect("the project's records directory");

    for (dir, why) in [
        (&records, "a group-writable records directory"),
        (
            &project_records,
            "a group-writable project records directory",
        ),
    ] {
        chmod(dir, 0o770);
        let refused = daemon_token::read(&clove_dir);
        let issued = daemon_token::read_or_create(&clove_dir);
        chmod(dir, 0o700);
        assert!(refused.is_err(), "a token was trusted from {why}");
        assert!(issued.is_err(), "a token was issued into {why}");
        assert_eq!(daemon_token::read(&clove_dir).unwrap(), token);
    }

    // The records directory swapped for a link to a copy of itself.
    let moved = home.join("elsewhere");
    std::fs::rename(&records, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &records).unwrap();
    assert!(
        daemon_token::read(&clove_dir).is_err(),
        "a token was trusted through a symlinked records directory"
    );
    assert!(
        daemon_token::read_or_create(&clove_dir).is_err(),
        "a token was issued through a symlinked records directory"
    );
}
