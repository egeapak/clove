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

/// Open an owner-only log file for appending, creating it if missing, without
/// following a symlink at `path`; anything but a regular file is refused.
pub fn open_private_log(path: &Utf8Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    no_follow(&mut options, path)?;
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{path} is not a regular file"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
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

/// The directories strictly below `root` (a store's `.clove/`) down to `dir`
/// itself, outermost first. `root` is given, not guessed from the path: a
/// canonical path no longer shows which of its components was `.clove`.
fn dirs_below<'a>(root: &Utf8Path, dir: &'a Utf8Path) -> io::Result<Vec<&'a Utf8Path>> {
    if !dir.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{dir} is not under {root}"),
        ));
    }
    let mut below: Vec<&Utf8Path> = dir.ancestors().take_while(|p| *p != root).collect();
    below.reverse();
    Ok(below)
}

fn symlinked_dir(path: &Utf8Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "refusing to use {path}: it is a symlink, not a directory \
             (replace the link with the directory it points to)"
        ),
    )
}

/// Fail if any existing directory below the store root `root` on the way to
/// `dir` (itself included) is a symlink. Missing directories are fine.
pub fn check_dirs(root: &Utf8Path, dir: &Utf8Path) -> io::Result<()> {
    for step in dirs_below(root, dir)? {
        match std::fs::symlink_metadata(step) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(symlinked_dir(step)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `create_dir_all` that never follows a symlink planted below the store root
/// `root`: every directory from there down to `dir` must be a real directory
/// or is created as one.
pub fn create_dirs(root: &Utf8Path, dir: &Utf8Path) -> io::Result<()> {
    let below = dirs_below(root, dir)?;
    std::fs::create_dir_all(root)?;
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

/// Replace `path` (below the store root `root`) with `bytes` atomically — a
/// fresh sibling temp file, fsynced, then renamed over it — never writing
/// through a symlink: not one at `path`, nor a symlinked directory on the way
/// there. The file keeps its permissions (a new one gets `0644` on Unix).
pub fn write_atomic(root: &Utf8Path, path: &Utf8Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    check_dirs(root, parent)?;
    refuse_symlink(path)?;
    // The temp name is random and created exclusively, so nothing planted can
    // stand in for it; the rename replaces the directory entry, not a target.
    let mut temp = tempfile::NamedTempFile::new_in(parent.as_std_path())?;
    let permissions = match std::fs::symlink_metadata(path) {
        Ok(meta) => Some(meta.permissions()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => new_file_permissions(),
        Err(e) => return Err(e),
    };
    if let Some(permissions) = permissions {
        temp.as_file().set_permissions(permissions)?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path.as_std_path()).map_err(|e| e.error)?;
    Ok(())
}

/// What an ordinary new file gets (`tempfile` makes its own `0600`).
#[cfg(unix)]
fn new_file_permissions() -> Option<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;
    Some(std::fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
fn new_file_permissions() -> Option<std::fs::Permissions> {
    None
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

    /// Rewriting a file keeps its mode — `.gitignore` stays `0644` — and a new
    /// file is an ordinary `0644`, not the temp file's `0600`.
    #[test]
    fn an_atomic_write_keeps_the_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap().join(".clove");
        std::fs::create_dir(&root).unwrap();
        let mode = |p: &Utf8Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fresh = root.join("fresh");
        write_atomic(&root, &fresh, b"a").unwrap();
        assert_eq!(mode(&fresh), 0o644);
        let kept = root.join(".gitignore");
        std::fs::write(&kept, "x\n").unwrap();
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&root, &kept, b"x\ny\n").unwrap();
        assert_eq!(mode(&kept), 0o640);
    }

    /// The store root is given, not guessed: under a canonical path (no
    /// component named `.clove`) a symlinked directory is still refused.
    #[test]
    fn directories_are_checked_below_an_explicit_root() {
        let dir = tempfile::tempdir().unwrap();
        let base = Utf8Path::from_path(dir.path()).unwrap();
        let root = base.join("store");
        std::fs::create_dir(&root).unwrap();
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("issues")).unwrap();
        assert!(check_dirs(&root, &root.join("issues")).is_err());
        assert!(create_dirs(&root, &root.join("issues").join("x")).is_err());
        assert!(check_dirs(&root, &base.join("outside")).is_err());
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
