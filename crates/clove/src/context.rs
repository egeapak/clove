//! Repository context shared by every command: locate `.clove/`, open the file
//! store, and load config (DESIGN.md §7, §10).

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::repo::find_repo_root;
use clove_core::{load_config, CloveConfig, CloveError, ItemStore};

/// Everything a command needs to act on a repository.
pub struct Ctx {
    /// The repository root (the directory containing `.clove/`).
    pub root: Utf8PathBuf,
    /// The `.clove/issues/` directory.
    pub issues_dir: Utf8PathBuf,
    /// The (possibly absent) `.clove/index.db` path.
    pub db_path: Utf8PathBuf,
    /// The file store.
    pub store: ItemStore,
    /// Loaded, validated configuration.
    pub config: CloveConfig,
}

/// The current working directory as a UTF-8 path.
pub fn current_dir() -> Result<Utf8PathBuf, CloveError> {
    let cwd = std::env::current_dir().map_err(|source| CloveError::Io {
        path: Utf8PathBuf::from("."),
        source,
    })?;
    Utf8PathBuf::from_path_buf(cwd).map_err(|p| CloveError::Io {
        path: Utf8PathBuf::from(p.to_string_lossy().into_owned()),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, "cwd is not valid UTF-8"),
    })
}

/// Discover the repository context.
///
/// With `--clove-dir PATH`, `PATH` is taken to be the `.clove/` directory and
/// the repo root is its parent. Otherwise the nearest ancestor containing
/// `.clove/` is used (with linked-worktree fallback in `clove-core`).
pub fn discover(clove_dir_override: Option<&Utf8Path>) -> Result<Ctx, CloveError> {
    let (root, clove_dir) = match clove_dir_override {
        Some(dir) => {
            let root = dir
                .parent()
                .map(Utf8Path::to_owned)
                .unwrap_or_else(|| Utf8PathBuf::from("."));
            (root, dir.to_owned())
        }
        None => {
            let cwd = current_dir()?;
            let root = find_repo_root(&cwd).ok_or(CloveError::NoRepo { searched: cwd })?;
            let clove_dir = root.join(".clove");
            (root, clove_dir)
        }
    };

    let issues_dir = clove_dir.join("issues");
    let db_path = clove_dir.join("index.db");
    let config = load_config(&root)?;
    let store = ItemStore::new(root.clone());

    Ok(Ctx {
        root,
        issues_dir,
        db_path,
        store,
        config,
    })
}

/// A path made relative to the repo root for display (falls back to the input).
pub fn rel_to_root(root: &Utf8Path, path: &Utf8Path) -> Utf8PathBuf {
    path.strip_prefix(root)
        .map(Utf8Path::to_owned)
        .unwrap_or_else(|_| path.to_owned())
}

/// Surface a `clove-index` error through `CloveError` so the CLI's exit-code
/// mapping applies (index errors map to the I/O class).
pub fn index_error(err: clove_index::IndexError, path: &Utf8Path) -> CloveError {
    CloveError::Io {
        path: path.to_owned(),
        source: std::io::Error::other(err.to_string()),
    }
}
