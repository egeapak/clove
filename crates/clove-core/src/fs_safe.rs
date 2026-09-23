//! Opening files that a cloned repository could have planted as symlinks.
//!
//! Everything under `.clove/` arrives with the repository, so a malicious clone
//! can ship `.clove/daemon.lock` (or `index.db`, …) as a symlink to a file the
//! user cares about. A plain `File::create` follows the link and truncates the
//! target. Every file clove creates or writes there by a *fixed* name goes
//! through here instead.

use std::fs::{File, OpenOptions};
use std::io;

use camino::Utf8Path;

/// Open a lock file for writing, creating it if missing — without following a
/// symlink and without truncating. A lock's content is never read or written;
/// only its advisory lock matters.
pub fn open_lock_file(path: &Utf8Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    no_follow(&mut options, path)?;
    options.open(path)
}

/// Create (or replace the content of) an owner-only file without following a
/// symlink at `path`.
pub fn create_private_file(path: &Utf8Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    no_follow(&mut options, path)?;
    options.open(path)
}

/// Fail if `path` is a symlink (a missing path is fine).
pub fn refuse_symlink(path: &Utf8Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to follow the symlink {path}"),
        )),
        _ => Ok(()),
    }
}

/// `O_NOFOLLOW` on Unix, where the kernel refuses the link atomically. Windows
/// has no such open flag here, so the check is done up front instead.
#[cfg(unix)]
fn no_follow(options: &mut OpenOptions, _path: &Utf8Path) -> io::Result<()> {
    std::os::unix::fs::OpenOptionsExt::custom_flags(options, libc::O_NOFOLLOW);
    Ok(())
}

#[cfg(not(unix))]
fn no_follow(_options: &mut OpenOptions, path: &Utf8Path) -> io::Result<()> {
    refuse_symlink(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_lock_file_symlink_is_refused_and_its_target_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let victim = root.join("precious.txt");
        std::fs::write(&victim, "precious").unwrap();
        let link = root.join("daemon.lock");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(open_lock_file(&link).is_err());
        assert!(create_private_file(&link).is_err());
        assert!(refuse_symlink(&link).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    }

    #[test]
    fn a_plain_lock_file_opens_and_keeps_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("x.lock");
        std::fs::write(&path, "kept").unwrap();
        open_lock_file(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept");
        assert!(refuse_symlink(&path).is_ok());
    }
}
