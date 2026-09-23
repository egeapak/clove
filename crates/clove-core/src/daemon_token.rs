//! The project's daemon token: `.clove/daemon.token`, a random secret that
//! every daemon call for the project carries (DESIGN §8.4).
//!
//! It scopes automated clients per project — a process that can read one
//! repository's `.clove/` can act on that project through the shared daemon,
//! and on no other. It gates automation; it is not a security boundary
//! against the local user, who can read every token they own.

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

/// Read the project's token. Refuses a symlink and anything that is not a
/// token (too short, not hex).
pub fn read(clove_dir: &Utf8Path) -> io::Result<String> {
    let path = token_path(clove_dir);
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    crate::fs_safe::no_follow(&mut options, &path)?;
    let mut text = String::new();
    options.open(&path)?.read_to_string(&mut text)?;
    let token = text.trim();
    if token.len() < MIN_HEX_LEN || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{path} does not hold a daemon token"),
        ));
    }
    Ok(token.to_ascii_lowercase())
}

/// Read the project's token, creating it first if the project has none.
///
/// A fresh token is written to a private temp file and linked into place
/// without replacing anything, so a concurrent reader never sees a partial
/// token and two creators agree on the winner's.
pub fn read_or_create(clove_dir: &Utf8Path) -> io::Result<String> {
    match read(clove_dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        other => return other,
    }
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut staged = tempfile::Builder::new()
        .prefix(".daemon.token.")
        .tempfile_in(clove_dir.as_std_path())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    writeln!(staged, "{token}")?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(token_path(clove_dir).as_std_path()) {
        Ok(_) => Ok(token),
        // Another client created it first — or something already sits there
        // (a planted symlink), which the read refuses.
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => read(clove_dir),
        Err(e) => Err(e.error),
    }
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

    #[test]
    fn a_missing_token_is_created_private_and_then_kept() {
        let (_tmp, dir) = clove_dir();
        let token = read_or_create(&dir).unwrap();
        assert!(token.len() >= MIN_HEX_LEN, "{token}");
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_path(&dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(read_or_create(&dir).unwrap(), token);
        assert_eq!(read(&dir).unwrap(), token);
    }

    #[test]
    fn two_projects_get_different_tokens() {
        let (_a_tmp, a) = clove_dir();
        let (_b_tmp, b) = clove_dir();
        assert_ne!(read_or_create(&a).unwrap(), read_or_create(&b).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_token_is_refused_and_its_target_untouched() {
        let (tmp, dir) = clove_dir();
        let victim = Utf8Path::from_path(tmp.path()).unwrap().join("victim");
        std::fs::write(&victim, "0123456789abcdef0123456789abcdef\n").unwrap();
        std::os::unix::fs::symlink(&victim, token_path(&dir)).unwrap();
        assert!(read_or_create(&dir).is_err());
        assert!(read(&dir).is_err());
        let dangling = Utf8Path::from_path(tmp.path()).unwrap().join("nowhere");
        std::fs::remove_file(token_path(&dir)).unwrap();
        std::os::unix::fs::symlink(&dangling, token_path(&dir)).unwrap();
        assert!(read_or_create(&dir).is_err());
        assert!(!dangling.exists(), "a token was written through the link");
    }

    #[test]
    fn anything_but_a_token_is_refused() {
        let (_tmp, dir) = clove_dir();
        for junk in ["", "short", "zz89abcdef0123456789abcdef01234567"] {
            std::fs::write(token_path(&dir), junk).unwrap();
            assert!(read(&dir).is_err(), "{junk:?}");
        }
    }

    #[test]
    fn tokens_compare_exactly() {
        assert!(matches("abc123", "abc123"));
        assert!(!matches("abc123", "abc124"));
        assert!(!matches("abc123", "abc12"));
    }
}
