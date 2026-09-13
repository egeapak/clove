//! The clove-managed home directory — where installed plugin binaries live
//! (`PLUGIN_REGISTRY.md` §7, as revised by the Stage-1 implementation plan).
//!
//! This is *not* a repository's `.clove/` directory. It is a per-user root that
//! holds `bin/` (plugin binaries installed by clove) and the registry cache.
//!
//! # Why not `~/.clove`
//!
//! The superseded design put this root at `~/.clove`. That is unsafe:
//! [`clove_core::discover`]'s `find_repo_root` accepts **any** ancestor
//! directory containing a `.clove/` *directory*, with no marker file and no
//! content check. So the moment clove created `~/.clove/bin`, `$HOME` itself
//! would resolve as a clove repository — every command run anywhere beneath the
//! home directory would silently target `$HOME/.clove/issues` instead of
//! reporting "no clove repository found", and `clove new` would write items
//! there. Using an XDG-style path avoids the collision entirely, without
//! changing repository-discovery semantics for existing repositories.

use camino::Utf8PathBuf;
use clove_types::CloveError;

/// The user's home directory (`$HOME`, or `%USERPROFILE%` on Windows). clove has
/// no `dirs`-style dependency, so this is resolved by hand.
///
/// `field` names the caller's context for the error message (e.g. `--global`),
/// so a failure reads naturally at each call site rather than always blaming a
/// flag the current command may not even have.
pub fn home_dir(field: &str) -> Result<Utf8PathBuf, CloveError> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let raw = std::env::var_os(var).ok_or_else(|| CloveError::InvalidField {
        field: field.to_owned(),
        reason: format!("cannot resolve the home directory: ${var} is not set"),
    })?;
    Utf8PathBuf::from_path_buf(std::path::PathBuf::from(raw)).map_err(|path| {
        CloveError::InvalidField {
            field: field.to_owned(),
            reason: format!("home directory is not valid UTF-8: {}", path.display()),
        }
    })
}

/// Resolve the clove home directory:
///
/// 1. `$CLOVE_HOME` — an explicit override, always wins;
/// 2. `$XDG_DATA_HOME/clove` when `XDG_DATA_HOME` is set;
/// 3. `~/.local/share/clove` on Unix;
/// 4. `%APPDATA%\clove` on Windows (falling back to `~/clove` if unset).
///
/// Deliberately never `~/.clove` — see the module docs.
pub fn clove_home() -> Result<Utf8PathBuf, CloveError> {
    if let Some(raw) = non_empty_var("CLOVE_HOME") {
        return to_utf8(raw, "CLOVE_HOME");
    }
    if let Some(raw) = non_empty_var("XDG_DATA_HOME") {
        return Ok(to_utf8(raw, "XDG_DATA_HOME")?.join("clove"));
    }
    #[cfg(windows)]
    {
        if let Some(raw) = non_empty_var("APPDATA") {
            return Ok(to_utf8(raw, "APPDATA")?.join("clove"));
        }
        Ok(home_dir("CLOVE_HOME")?.join("clove"))
    }
    #[cfg(not(windows))]
    {
        Ok(home_dir("CLOVE_HOME")?
            .join(".local")
            .join("share")
            .join("clove"))
    }
}

/// The directory installed plugin binaries land in (`cargo install --root
/// <clove-home>` puts them in `<clove-home>/bin`).
pub fn bin_dir() -> Option<Utf8PathBuf> {
    // A failure here (no `$HOME`) must never be fatal for the callers that only
    // want to *search* the directory — an unresolvable home simply contributes
    // no search entry.
    clove_home().ok().map(|home| home.join("bin"))
}

/// An environment variable, treated as absent when empty. An empty `$CLOVE_HOME`
/// must not resolve the root to `""` (which would join to a relative path).
fn non_empty_var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn to_utf8(raw: std::ffi::OsString, field: &str) -> Result<Utf8PathBuf, CloveError> {
    Utf8PathBuf::from_path_buf(std::path::PathBuf::from(raw)).map_err(|path| {
        CloveError::InvalidField {
            field: field.to_owned(),
            reason: format!("path is not valid UTF-8: {}", path.display()),
        }
    })
}

/// True when `path` is the `.clove` directory name that repository discovery
/// keys on. Used by the unit test below to pin the invariant that the clove home
/// can never *be* a repository marker.
#[cfg(test)]
fn ends_with_dot_clove(path: &camino::Utf8Path) -> bool {
    path.file_name() == Some(".clove")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize env mutation across the tests in this module — `std::env::set_var`
    /// is process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, Option<&str>)]) -> Self {
            let saved = vars
                .iter()
                .map(|(name, _)| (*name, std::env::var_os(name)))
                .collect();
            for (name, value) in vars {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
            EnvGuard { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn clove_home_override_wins() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[
            ("CLOVE_HOME", Some("/tmp/explicit-root")),
            ("XDG_DATA_HOME", Some("/tmp/xdg")),
        ]);
        assert_eq!(
            clove_home().unwrap(),
            Utf8PathBuf::from("/tmp/explicit-root")
        );
    }

    #[test]
    fn clove_home_uses_xdg_when_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[("CLOVE_HOME", None), ("XDG_DATA_HOME", Some("/tmp/xdg"))]);
        assert_eq!(clove_home().unwrap(), Utf8PathBuf::from("/tmp/xdg/clove"));
    }

    #[test]
    fn empty_env_var_is_treated_as_absent() {
        // An empty `$CLOVE_HOME` must not resolve the root to a relative path.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[
            ("CLOVE_HOME", Some("")),
            ("XDG_DATA_HOME", Some("/tmp/xdg")),
        ]);
        assert_eq!(clove_home().unwrap(), Utf8PathBuf::from("/tmp/xdg/clove"));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_default_is_xdg_style_never_dot_clove() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[
            ("CLOVE_HOME", None),
            ("XDG_DATA_HOME", None),
            ("HOME", Some("/home/someone")),
        ]);
        let home = clove_home().unwrap();
        assert_eq!(home, Utf8PathBuf::from("/home/someone/.local/share/clove"));
        assert_eq!(
            bin_dir().unwrap(),
            Utf8PathBuf::from("/home/someone/.local/share/clove/bin")
        );
    }

    #[test]
    fn clove_home_is_never_a_repository_marker() {
        // The load-bearing invariant: `find_repo_root` treats any ancestor with a
        // `.clove/` *directory* as a repository root, so the clove home must never
        // be named `.clove` — otherwise installing a plugin would turn the user's
        // home directory into a clove repository.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[
            ("CLOVE_HOME", None),
            ("XDG_DATA_HOME", None),
            ("HOME", Some("/home/someone")),
            ("APPDATA", Some(r"C:\Users\someone\AppData\Roaming")),
        ]);
        let home = clove_home().unwrap();
        assert!(
            !ends_with_dot_clove(&home),
            "clove home {home} must not be named `.clove` (it would be picked up \
             as a repository root by find_repo_root)"
        );
        for ancestor in home.ancestors() {
            assert!(
                !ends_with_dot_clove(ancestor),
                "no component of the clove home may be `.clove`, found in {home}"
            );
        }
    }

    #[test]
    fn home_dir_error_names_the_callers_field() {
        // The `--global` field name was hardcoded when this lived in `cmd/setup.rs`;
        // a registry caller must not report a flag it does not have.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set(&[("HOME", None), ("USERPROFILE", None)]);
        let err = home_dir("CLOVE_HOME").unwrap_err();
        match err {
            CloveError::InvalidField { field, .. } => assert_eq!(field, "CLOVE_HOME"),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }
}
