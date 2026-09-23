//! The per-user clove home directory (not a repository's `.clove/`): where
//! installed plugins live, and where clove records the daemon tokens it
//! issued. One resolver, so the CLI and the daemon agree on it.
//!
//! 1. `$CLOVE_HOME` — an explicit override, always wins;
//! 2. `$XDG_DATA_HOME/clove` when `XDG_DATA_HOME` is set;
//! 3. `~/.local/share/clove` on Unix;
//! 4. `%APPDATA%\clove` on Windows (falling back to `~/clove` if unset).
//!
//! Deliberately never `~/.clove`, which repository discovery would take for a
//! repository at `$HOME`.

use std::io;

use camino::Utf8PathBuf;

/// The clove home directory.
pub fn clove_home() -> io::Result<Utf8PathBuf> {
    if let Some(dir) = non_empty_var("CLOVE_HOME")? {
        return Ok(dir);
    }
    if let Some(dir) = non_empty_var("XDG_DATA_HOME")? {
        return Ok(dir.join("clove"));
    }
    #[cfg(windows)]
    {
        if let Some(dir) = non_empty_var("APPDATA")? {
            return Ok(dir.join("clove"));
        }
        Ok(home_dir()?.join("clove"))
    }
    #[cfg(not(windows))]
    {
        Ok(home_dir()?.join(".local").join("share").join("clove"))
    }
}

/// `$HOME` (`%USERPROFILE%` on Windows).
fn home_dir() -> io::Result<Utf8PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    non_empty_var(var)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("cannot resolve the home directory: ${var} is not set"),
        )
    })
}

/// An environment variable as a path, absent when unset or empty (an empty
/// `$CLOVE_HOME` must not resolve to a relative path).
fn non_empty_var(name: &str) -> io::Result<Option<Utf8PathBuf>> {
    match std::env::var_os(name).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(raw) => Utf8PathBuf::from_path_buf(raw.into())
            .map(Some)
            .map_err(|path| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("${name} is not valid UTF-8: {}", path.display()),
                )
            }),
    }
}
