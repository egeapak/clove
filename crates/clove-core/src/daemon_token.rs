//! The project's daemon token: `.clove/daemon.token`, a random secret that
//! every daemon call for the project carries (DESIGN §8.4).
//!
//! It scopes automated clients per project — a process that can read one
//! repository's `.clove/` can act on that project through the shared daemon,
//! and on no other. It gates automation; it is not a security boundary
//! against the local user, who can read every token they own.
//!
//! Only a token this user's clove issued for this project is trusted: a
//! regular file (never a symlink) — on Unix owned by the user with mode `0600`
//! — whose value clove recorded when it made it, in a per-user record under
//! the clove home ([`crate::home`]). A token that arrived any other way — one
//! committed to the repository, restored from an archive with its modes — has
//! no record, and is replaced by a client that loads the project. A lost
//! record only means a fresh token. Records count only in directories that are
//! private — real directories this user owns that no one else can write to —
//! checked on every read as well as when a record is made.
//!
//! Reading never writes: only [`read_or_create`], for a client that loads the
//! project, creates or replaces the token or touches `.clove/.gitignore`.

use std::io::{self, Read as _, Write as _};
use std::sync::OnceLock;

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest as _, Sha256};

/// The token's file name inside `.clove/`.
pub const TOKEN_FILE: &str = "daemon.token";

/// Random bytes in a fresh token (hex-encoded on disk).
const TOKEN_BYTES: usize = 32;

/// The shortest token accepted, in hex digits (128 bits).
const MIN_HEX_LEN: usize = 32;

/// The token file of the project whose `.clove/` directory is `clove_dir`.
pub fn token_path(clove_dir: &Utf8Path) -> Utf8PathBuf {
    clove_dir.join(TOKEN_FILE)
}

static RECORDS_DIR: OnceLock<Utf8PathBuf> = OnceLock::new();

/// Keep this process's token records in `dir` rather than under the clove
/// home — for tests, which must not touch the user's. The first call wins.
pub fn use_records_dir(dir: Utf8PathBuf) {
    let _ = RECORDS_DIR.set(dir);
}

/// Where clove records the tokens it issued: `<clove home>/daemon-tokens`.
pub fn records_dir() -> io::Result<Utf8PathBuf> {
    match RECORDS_DIR.get() {
        Some(dir) => Ok(dir.clone()),
        None => Ok(crate::home::clove_home()?.join("daemon-tokens")),
    }
}

fn hex_sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The directory of the project's records, named by the project path's hash.
fn project_records(clove_dir: &Utf8Path) -> io::Result<Utf8PathBuf> {
    Ok(records_dir()?.join(hex_sha256(clove_dir.as_str())))
}

/// The record of `token` as issued for the project at `clove_dir`: a file
/// named by the token's hash in the project's records directory.
fn record_path(clove_dir: &Utf8Path, token: &str) -> io::Result<Utf8PathBuf> {
    Ok(project_records(clove_dir)?.join(hex_sha256(token)))
}

/// Whether clove recorded issuing `token` for `clove_dir` — in a records
/// directory, and a project directory in it, that are both private: a record
/// anyone else could have put there vouches for nothing.
fn issued(clove_dir: &Utf8Path, token: &str) -> bool {
    let Ok(entry) = record_path(clove_dir, token) else {
        return false;
    };
    let project = entry.parent().unwrap_or(&entry);
    let records = project.parent().unwrap_or(project);
    is_private_dir(records)
        && is_private_dir(project)
        && std::fs::symlink_metadata(&entry).is_ok_and(|meta| meta.is_file())
}

/// Whether `dir` is a directory — itself, not a symlink to one — that on
/// Unix this user owns and no one else can write to: the daemon runtime
/// directory's rule, applied to the token records.
fn is_private_dir(dir: &Utf8Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.uid() == current_uid() && meta.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid takes no arguments, cannot fail, and touches no memory.
    unsafe { getuid() }
}

/// Record `token` as issued for `clove_dir` (`0700` directories, a `0600`
/// empty file, nothing followed through a symlink).
fn record(clove_dir: &Utf8Path, token: &str) -> io::Result<()> {
    let entry = record_path(clove_dir, token)?;
    let project = entry.parent().unwrap_or(&entry).to_owned();
    create_private_dir(&project)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    crate::fs_safe::no_follow(&mut options, &entry)?;
    match options.open(&entry) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Drop the records of the project's earlier tokens.
fn forget_others(clove_dir: &Utf8Path, token: &str) {
    let Ok(entry) = record_path(clove_dir, token) else {
        return;
    };
    let Some(project) = entry.parent() else {
        return;
    };
    for stale in std::fs::read_dir(project).into_iter().flatten().flatten() {
        if stale.file_name().to_str() != entry.file_name() {
            let _ = std::fs::remove_file(stale.path());
        }
    }
}

/// An exclusive lock on making the project's token, kept in the records
/// directory (never the repository): creating or replacing a token happens
/// one client at a time, so concurrent clients agree on one.
fn issue_lock(clove_dir: &Utf8Path) -> io::Result<std::fs::File> {
    let records = records_dir()?;
    create_private_dir(&records)?;
    let lock = crate::fs_safe::open_lock_file(
        &records.join(format!("{}.lock", hex_sha256(clove_dir.as_str()))),
    )?;
    lock.lock()?;
    Ok(lock)
}

fn create_private_dir(dir: &Utf8Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    if !is_private_dir(dir) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{dir} must be a directory (not a symlink) owned by the current user \
                 that no one else can write to"
            ),
        ));
    }
    Ok(())
}

/// What sits at a project's token path.
enum Found {
    Missing,
    Trusted(String),
    /// Something this clove did not issue, and why it is not trusted.
    Untrusted(String),
}

/// Read the project's token (`clove_dir` canonical). Refuses one that is
/// missing or not trusted (see the module docs); every error names the file.
/// Writes nothing.
pub fn read(clove_dir: &Utf8Path) -> io::Result<String> {
    let path = token_path(clove_dir);
    match inspect(clove_dir, &path)? {
        Found::Trusted(token) => Ok(token),
        Found::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{path}: the project has no daemon token"),
        )),
        Found::Untrusted(why) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{path}: not a daemon token clove can trust ({why})"),
        )),
    }
}

/// Read the project's token (`clove_dir` canonical), creating it if the
/// project has none and replacing one that is not trusted — for a client that
/// loads the project, never a read. It also puts `daemon.token` into a
/// `.clove/.gitignore` that lacks it (one written by an older clove), so the
/// secret never lands in a commit.
///
/// A fresh token is written to a private temp file and linked into place
/// without replacing anything, so a concurrent reader never sees a partial
/// token and concurrent creators — or replacers — agree on the winner's.
pub fn read_or_create(clove_dir: &Utf8Path) -> io::Result<String> {
    let path = token_path(clove_dir);
    let token = match inspect(clove_dir, &path)? {
        Found::Trusted(token) => token,
        Found::Missing | Found::Untrusted(_) => {
            let _issuing = issue_lock(clove_dir).map_err(with_path(&path, "creating"))?;
            // Where the new token would be recorded must be usable before the
            // one there now is removed.
            project_records(clove_dir)
                .and_then(|dir| create_private_dir(&dir))
                .map_err(with_path(&path, "recording"))?;
            // Another client may have made it while this one waited.
            match inspect(clove_dir, &path)? {
                Found::Trusted(token) => token,
                Found::Missing => create(clove_dir, &path)?,
                Found::Untrusted(_) => {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(with_path(&path, "replacing")(e)),
                    }
                    create(clove_dir, &path)?
                }
            }
        }
    };
    // Best effort: the token works either way.
    let _ = ensure_gitignored(clove_dir);
    Ok(token)
}

fn with_path<'a>(path: &'a Utf8Path, action: &str) -> impl FnOnce(io::Error) -> io::Error + 'a {
    let action = action.to_owned();
    move |e| io::Error::new(e.kind(), format!("{action} {path}: {e}"))
}

fn inspect(clove_dir: &Utf8Path, path: &Utf8Path) -> io::Result<Found> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Found::Missing),
        Err(e) => return Err(with_path(path, "reading")(e)),
    };
    if let Some(why) = untrusted(&meta) {
        return Ok(Found::Untrusted(why));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    crate::fs_safe::no_follow(&mut options, path)?;
    let mut file = options.open(path).map_err(with_path(path, "reading"))?;
    // Checked again on what was opened: the path may have been swapped since.
    let opened = file.metadata().map_err(with_path(path, "reading"))?;
    if let Some(why) = untrusted(&opened) {
        return Ok(Found::Untrusted(why));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(with_path(path, "reading"))?;
    let token = text.trim().to_ascii_lowercase();
    if token.len() < MIN_HEX_LEN || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Found::Untrusted("it does not hold a token".to_owned()));
    }
    if !issued(clove_dir, &token) {
        return Ok(Found::Untrusted(
            "clove has no record of issuing it for this project".to_owned(),
        ));
    }
    Ok(Found::Trusted(token))
}

/// Why a token file with this metadata is not trusted, if it is not. On
/// Windows only the file type is checked here (there is no mode, and a
/// checkout would be owned by the user anyway); the issue record is what
/// catches a token that arrived with the repository.
fn untrusted(meta: &std::fs::Metadata) -> Option<String> {
    if meta.file_type().is_symlink() {
        return Some("it is a symlink".to_owned());
    }
    if !meta.is_file() {
        return Some("it is not a regular file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != current_uid() {
            return Some(format!("it is owned by uid {}", meta.uid()));
        }
        if meta.mode() & 0o777 != 0o600 {
            return Some(format!("its mode is {:o}, not 600", meta.mode() & 0o777));
        }
    }
    None
}

/// Write a fresh token at `path`, recorded as issued, and linked into place
/// without replacing anything: should something else get there first, that is
/// used if it can be trusted. Called under [`issue_lock`].
fn create(clove_dir: &Utf8Path, path: &Utf8Path) -> io::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut staged = tempfile::Builder::new()
        .prefix(".daemon.token.")
        .tempfile_in(clove_dir.as_std_path())
        .map_err(with_path(path, "creating"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(with_path(path, "creating"))?;
    }
    writeln!(staged, "{token}").map_err(with_path(path, "creating"))?;
    staged
        .as_file()
        .sync_all()
        .map_err(with_path(path, "creating"))?;
    // Recorded before it appears, so no reader ever sees it unrecorded.
    record(clove_dir, &token).map_err(with_path(path, "recording"))?;
    match staged.persist_noclobber(path.as_std_path()) {
        Ok(_) => {
            forget_others(clove_dir, &token);
            Ok(token)
        }
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => match inspect(clove_dir, path)?
        {
            Found::Trusted(theirs) => Ok(theirs),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("creating {path}: something else was put there meanwhile"),
            )),
        },
        Err(e) => Err(with_path(path, "creating")(e.error)),
    }
}

/// Add `daemon.token` to `.clove/.gitignore` when it is not there (a list
/// written by a clove that predates the token).
fn ensure_gitignored(clove_dir: &Utf8Path) -> io::Result<()> {
    let gitignore = clove_dir.join(".gitignore");
    crate::fs_safe::refuse_symlink(&gitignore)?;
    let mut contents = match std::fs::read_to_string(&gitignore) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if contents.lines().any(|line| line.trim() == TOKEN_FILE) {
        return Ok(());
    }
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(TOKEN_FILE);
    contents.push('\n');
    crate::fs_safe::write_atomic(clove_dir, &gitignore, contents.as_bytes())
}

/// Compare two tokens without an early exit on the first differing byte.
pub fn matches(expected: &str, presented: &str) -> bool {
    let (a, b) = (expected.as_bytes(), presented.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records for these tests go in the build's target directory, never the
    /// user's clove home.
    fn isolate() {
        use_records_dir(Utf8PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/test-clove-home/daemon-tokens"
        )));
    }

    fn clove_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        isolate();
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path())
            .unwrap()
            .canonicalize_utf8()
            .unwrap()
            .join(".clove");
        std::fs::create_dir_all(&dir).unwrap();
        (tmp, dir)
    }

    #[cfg(unix)]
    fn write_with_mode(path: &Utf8Path, text: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::remove_file(path);
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    fn mode(path: &Utf8Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[cfg(unix)]
    const SOME_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn a_missing_token_is_created_private_and_then_kept() {
        let (_tmp, dir) = clove_dir();
        let token = read_or_create(&dir).unwrap();
        assert!(token.len() >= MIN_HEX_LEN, "{token}");
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        #[cfg(unix)]
        assert_eq!(mode(&token_path(&dir)), 0o600);
        assert_eq!(read_or_create(&dir).unwrap(), token);
        assert_eq!(read(&dir).unwrap(), token);
    }

    #[test]
    fn two_projects_get_different_tokens() {
        let (_a_tmp, a) = clove_dir();
        let (_b_tmp, b) = clove_dir();
        assert_ne!(read_or_create(&a).unwrap(), read_or_create(&b).unwrap());
    }

    /// Reading never writes: a missing token stays missing.
    #[test]
    fn reading_creates_nothing() {
        let (_tmp, dir) = clove_dir();
        assert!(read(&dir).is_err());
        assert!(!token_path(&dir).exists());
        assert!(!dir.join(".gitignore").exists());
    }

    /// A token clove did not issue here — committed to the repository, or
    /// restored from an archive with a trusted-looking `0600` — is never
    /// trusted, and a loading client replaces it.
    #[cfg(unix)]
    #[test]
    fn a_token_clove_did_not_issue_is_replaced_not_trusted() {
        let (_tmp, dir) = clove_dir();
        let path = token_path(&dir);
        for committed_mode in [0o644, 0o600] {
            write_with_mode(&path, SOME_TOKEN, committed_mode);
            let refused = read(&dir).unwrap_err().to_string();
            assert!(refused.contains("daemon.token"), "{refused}");
            let fresh = read_or_create(&dir).unwrap();
            assert_ne!(fresh, SOME_TOKEN);
            assert_eq!(mode(&path), 0o600);
            assert_eq!(read(&dir).unwrap(), fresh);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_token_is_replaced_and_its_target_untouched() {
        let (tmp, dir) = clove_dir();
        let victim = Utf8Path::from_path(tmp.path()).unwrap().join("victim");
        std::fs::write(&victim, format!("{SOME_TOKEN}\n")).unwrap();
        std::os::unix::fs::symlink(&victim, token_path(&dir)).unwrap();
        assert!(read(&dir).is_err(), "a symlinked token was read");
        let fresh = read_or_create(&dir).unwrap();
        assert_ne!(fresh, SOME_TOKEN);
        assert!(!std::fs::symlink_metadata(token_path(&dir))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            format!("{SOME_TOKEN}\n")
        );
    }

    /// A token this user can no longer read is replaced; where it cannot be
    /// (a read-only `.clove/`), the error names the file.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_token_is_replaced_or_named() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, dir) = clove_dir();
        let path = token_path(&dir);
        write_with_mode(&path, SOME_TOKEN, 0o000);
        assert_ne!(read_or_create(&dir).unwrap(), SOME_TOKEN);

        write_with_mode(&path, SOME_TOKEN, 0o000);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let failed = read_or_create(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let message = failed.unwrap_err().to_string();
        assert!(message.contains(path.as_str()), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn anything_but_a_token_is_refused() {
        let (_tmp, dir) = clove_dir();
        for junk in ["", "short", "zz89abcdef0123456789abcdef01234567"] {
            write_with_mode(&token_path(&dir), junk, 0o600);
            assert!(read(&dir).is_err(), "{junk:?}");
        }
    }

    /// Many clients replacing one untrusted token at once all end up with the
    /// same trusted token — none falls back.
    #[cfg(unix)]
    #[test]
    fn concurrent_replacers_agree_on_one_token() {
        let (_tmp, dir) = clove_dir();
        write_with_mode(&token_path(&dir), SOME_TOKEN, 0o644);
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || read_or_create(&dir))
            })
            .collect();
        let tokens: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().unwrap().expect("no client falls back"))
            .collect();
        let first = &tokens[0];
        assert!(tokens.iter().all(|t| t == first), "{tokens:?}");
        assert_eq!(&read(&dir).unwrap(), first);
    }

    /// A repository initialized by a clove that predates the token has a
    /// `.gitignore` without it: a loading client adds the line — also when the
    /// token already exists — keeping the rest and the file's mode.
    #[cfg(unix)]
    #[test]
    fn loading_git_ignores_the_token() {
        let (_tmp, dir) = clove_dir();
        let gitignore = dir.join(".gitignore");
        write_with_mode(&gitignore, "index.db\nsync/", 0o644);
        read_or_create(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&gitignore).unwrap(),
            "index.db\nsync/\ndaemon.token\n"
        );
        assert_eq!(mode(&gitignore), 0o644);
        // A token from an earlier build, with the entry missing.
        write_with_mode(&gitignore, "index.db\n", 0o644);
        read_or_create(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&gitignore).unwrap(),
            "index.db\ndaemon.token\n"
        );
        read_or_create(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&gitignore).unwrap(),
            "index.db\ndaemon.token\n",
            "the entry is added once"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_gitignore_is_not_written_through() {
        let (tmp, dir) = clove_dir();
        let victim = Utf8Path::from_path(tmp.path()).unwrap().join("precious");
        std::fs::write(&victim, "precious\n").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join(".gitignore")).unwrap();
        read_or_create(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious\n");
    }

    #[test]
    fn tokens_compare_exactly() {
        assert!(matches("abc123", "abc123"));
        assert!(!matches("abc123", "abc124"));
        assert!(!matches("abc123", "abc12"));
    }
}
