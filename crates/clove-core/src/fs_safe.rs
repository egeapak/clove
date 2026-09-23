//! Opening files that a cloned repository could have planted as symlinks.
//!
//! Everything under `.clove/` arrives with the repository, so a malicious clone
//! can ship `.clove/daemon.lock` (or `index.db`, …) as a symlink to a file the
//! user cares about. A plain `File::create` follows the link and truncates the
//! target. Every file clove creates or writes there by a *fixed* name goes
//! through here instead, and so does every directory it writes into: a
//! symlinked `issues/` or `sync/` would redirect even freshly named files.

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

/// The `.clove` directory a path lies under: its nearest ancestor so named.
fn clove_dir_of(path: &Utf8Path) -> Option<&Utf8Path> {
    path.ancestors().find(|p| p.file_name() == Some(".clove"))
}

/// The directories from just below `.clove/` down to `dir` itself, outermost
/// first; empty when `dir` is not under a `.clove` directory.
fn dirs_below_clove(dir: &Utf8Path) -> Vec<&Utf8Path> {
    let Some(clove_dir) = clove_dir_of(dir) else {
        return Vec::new();
    };
    let mut below: Vec<&Utf8Path> = dir.ancestors().take_while(|p| *p != clove_dir).collect();
    below.reverse();
    below
}

fn symlinked_dir(path: &Utf8Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("refusing to use {path}: it is a symlink, not a directory"),
    )
}

/// Fail if any existing directory under `.clove/` on the way to `dir` (itself
/// included) is a symlink. Missing directories are fine.
pub fn check_dirs(dir: &Utf8Path) -> io::Result<()> {
    for step in dirs_below_clove(dir) {
        match std::fs::symlink_metadata(step) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(symlinked_dir(step)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `create_dir_all` that never follows a symlink planted under `.clove/`:
/// every directory from there down to `dir` must be a real directory or is
/// created as one.
pub fn create_dirs(dir: &Utf8Path) -> io::Result<()> {
    let below = dirs_below_clove(dir);
    let Some(first) = below.first() else {
        return std::fs::create_dir_all(dir);
    };
    if let Some(above) = first.parent() {
        std::fs::create_dir_all(above)?;
    }
    for step in below {
        match std::fs::create_dir(step) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let meta = std::fs::symlink_metadata(step)?;
                if meta.file_type().is_symlink() {
                    return Err(symlinked_dir(step));
                }
                if !meta.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{step} is not a directory"),
                    ));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Replace `path` with `bytes` atomically (a fresh sibling temp file, fsynced,
/// then renamed over it), never writing through a symlink: not one at `path`,
/// nor a symlinked directory under `.clove/` on the way there.
pub fn write_atomic(path: &Utf8Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    check_dirs(parent)?;
    refuse_symlink(path)?;
    // The temp name is random and created exclusively, so nothing planted can
    // stand in for it; the rename replaces the directory entry, not a target.
    let mut temp = tempfile::NamedTempFile::new_in(parent.as_std_path())?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path.as_std_path()).map_err(|e| e.error)?;
    Ok(())
}

/// `O_NOFOLLOW` on Unix, where the kernel refuses the link atomically. Windows
/// has no such open flag here, so the check is done up front instead.
#[cfg(unix)]
pub(crate) fn no_follow(options: &mut OpenOptions, _path: &Utf8Path) -> io::Result<()> {
    std::os::unix::fs::OpenOptionsExt::custom_flags(options, libc::O_NOFOLLOW);
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn no_follow(_options: &mut OpenOptions, path: &Utf8Path) -> io::Result<()> {
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
