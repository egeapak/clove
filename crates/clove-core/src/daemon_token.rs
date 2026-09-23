//! The project's daemon token: `.clove/daemon.token`, a random secret that
//! every daemon call for the project carries (DESIGN §8.4).
//!
//! It scopes automated clients per project — a process that can read one
//! repository's `.clove/` can act on that project through the shared daemon,
//! and on no other. It gates automation; it is not a security boundary
//! against the local user, who can read every token they own.
//!
//! Only a token this user's clove made is trusted: a regular file (never a
//! symlink), and on Unix owned by this user with mode `0600`. Anything else —
//! a token committed to the repository, which a checkout writes `0644`, say —
//! is replaced with a fresh one rather than used.

use std::io::{self, Read as _, Write as _};

use camino::{Utf8Path, Utf8PathBuf};

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

/// What sits at a project's token path.
enum Found {
    Missing,
    Trusted(String),
    /// Something this clove did not make, and why it is not trusted.
    Untrusted(String),
}

/// Read the project's token. Refuses one that is missing or not trusted (see
/// the module docs); every error names the file.
pub fn read(clove_dir: &Utf8Path) -> io::Result<String> {
    let path = token_path(clove_dir);
    match inspect(&path)? {
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

/// Read the project's token, creating it if the project has none and
/// replacing one that is not trusted. A token it writes also gets
/// `daemon.token` into `.clove/.gitignore` if an older clove's list lacks it,
/// so the secret never lands in a commit.
///
/// A fresh token is written to a private temp file and moved into place —
/// linked without replacing anything when there was none, so a concurrent
/// reader never sees a partial token and two creators agree on the winner's.
pub fn read_or_create(clove_dir: &Utf8Path) -> io::Result<String> {
    let path = token_path(clove_dir);
    let token = match inspect(&path)? {
        Found::Trusted(token) => return Ok(token),
        Found::Missing => create(clove_dir, &path, false)?,
        Found::Untrusted(_) => create(clove_dir, &path, true)?,
    };
    // Best effort: the token works either way.
    let _ = ensure_gitignored(clove_dir);
    Ok(token)
}

fn with_path<'a>(path: &'a Utf8Path, action: &str) -> impl FnOnce(io::Error) -> io::Error + 'a {
    let action = action.to_owned();
    move |e| io::Error::new(e.kind(), format!("{action} {path}: {e}"))
}

fn inspect(path: &Utf8Path) -> io::Result<Found> {
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
    let token = text.trim();
    if token.len() < MIN_HEX_LEN || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Found::Untrusted("it does not hold a token".to_owned()));
    }
    Ok(Found::Trusted(token.to_ascii_lowercase()))
}

/// Why a token file with this metadata is not trusted, if it is not. On
/// Windows only the file type is checked: there is no mode, and a checkout
/// would be owned by the user anyway.
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
        extern "C" {
            fn getuid() -> u32;
        }
        // SAFETY: getuid takes no arguments, cannot fail, and touches no memory.
        let me = unsafe { getuid() };
        if meta.uid() != me {
            return Some(format!("it is owned by uid {}", meta.uid()));
        }
        if meta.mode() & 0o777 != 0o600 {
            return Some(format!("its mode is {:o}, not 600", meta.mode() & 0o777));
        }
    }
    None
}

/// Write a fresh token at `path`: linked into place if there was none, or —
/// `replace` — renamed over what is there (which replaces a symlink rather
/// than following it).
fn create(clove_dir: &Utf8Path, path: &Utf8Path, replace: bool) -> io::Result<String> {
    let action = if replace { "replacing" } else { "creating" };
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut staged = tempfile::Builder::new()
        .prefix(".daemon.token.")
        .tempfile_in(clove_dir.as_std_path())
        .map_err(with_path(path, action))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(with_path(path, action))?;
    }
    writeln!(staged, "{token}").map_err(with_path(path, action))?;
    staged
        .as_file()
        .sync_all()
        .map_err(with_path(path, action))?;
    if replace {
        staged
            .persist(path.as_std_path())
            .map_err(|e| with_path(path, action)(e.error))?;
        return Ok(token);
    }
    match staged.persist_noclobber(path.as_std_path()) {
        Ok(_) => Ok(token),
        // Another client created it first: use theirs, if it can be trusted.
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => read(path_parent(path)),
        Err(e) => Err(with_path(path, action)(e.error)),
    }
}

fn path_parent(path: &Utf8Path) -> &Utf8Path {
    path.parent().unwrap_or(path)
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
    crate::fs_safe::write_atomic(&gitignore, contents.as_bytes())
}

/// Compare two tokens without an early exit on the first differing byte.
pub fn matches(expected: &str, presented: &str) -> bool {
    let (a, b) = (expected.as_bytes(), presented.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clove_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap().join(".clove");
        std::fs::create_dir_all(&dir).unwrap();
        (tmp, dir)
    }

    #[cfg(unix)]
    fn write_with_mode(path: &Utf8Path, text: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
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

    /// A checkout writes a committed token `0644`: it is never trusted, and
    /// the client replaces it with a fresh private one.
    #[cfg(unix)]
    #[test]
    fn a_committed_token_is_replaced_not_trusted() {
        let (_tmp, dir) = clove_dir();
        let path = token_path(&dir);
        write_with_mode(&path, SOME_TOKEN, 0o644);
        let refused = read(&dir).unwrap_err().to_string();
        assert!(refused.contains("daemon.token"), "{refused}");
        let fresh = read_or_create(&dir).unwrap();
        assert_ne!(fresh, SOME_TOKEN);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(read(&dir).unwrap(), fresh);
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

    /// A repository initialized by a clove that predates the token has a
    /// `.gitignore` without it: creating the token adds the line, keeping the
    /// rest.
    #[test]
    fn creating_a_token_git_ignores_it() {
        let (_tmp, dir) = clove_dir();
        let gitignore = dir.join(".gitignore");
        std::fs::write(&gitignore, "index.db\nsync/").unwrap();
        read_or_create(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&gitignore).unwrap(),
            "index.db\nsync/\ndaemon.token\n"
        );
        read_or_create(&dir).unwrap();
        std::fs::remove_file(token_path(&dir)).unwrap();
        read_or_create(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&gitignore).unwrap(),
            "index.db\nsync/\ndaemon.token\n",
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
