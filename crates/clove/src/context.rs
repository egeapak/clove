//! Repository context shared by every command: locate `.clove/`, open the file
//! store, and load config (DESIGN.md §7, §10).

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::repo::find_repo_root;
use clove_core::{load_config, CloveConfig, ItemStore};
use clove_types::CloveError;

/// Everything a command needs to act on a repository.
pub struct Ctx {
    /// The repository root (the directory containing `.clove/`).
    pub root: Utf8PathBuf,
    /// The resolved `.clove/` directory. Normally `root/.clove`, but honors the
    /// `--clove-dir PATH` override — so it is the authoritative source for
    /// `CLOVE_DIR` / `CLOVE_SYNC_DIR` / `CLOVE_CONFIG_PATH` exported to plugins
    /// (never re-derive it as `root.join(".clove")`).
    pub clove_dir: Utf8PathBuf,
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
        Some(dir) => clove_dir_override_root(dir)?,
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
        clove_dir,
        issues_dir,
        db_path,
        store,
        config,
    })
}

/// Split a `--clove-dir PATH` override into `(repo root, clove dir)`.
///
/// The store, the config, and the write lock always live in `<root>/.clove`
/// (`ItemStore::new(root)`), while the index, the daemon key, and the plugin
/// environment follow the override. The two only agree when `PATH` *is*
/// `<parent>/.clove` — by name, or as a symlink to it — so any other path is
/// refused instead of being half-honored (items written to a sibling `.clove`
/// while the index and plugins look at `PATH`).
pub fn clove_dir_override_root(dir: &Utf8Path) -> Result<(Utf8PathBuf, Utf8PathBuf), CloveError> {
    let root = match dir.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent.to_owned(),
        _ => Utf8PathBuf::from("."),
    };
    let named = dir.file_name() == Some(".clove");
    let aliased = || {
        let own = std::fs::canonicalize(dir).ok();
        own.is_some() && own == std::fs::canonicalize(root.join(".clove")).ok()
    };
    if named || aliased() {
        return Ok((root, dir.to_owned()));
    }
    Err(CloveError::InvalidField {
        field: "--clove-dir".to_owned(),
        reason: format!(
            "`{dir}` is not a `.clove` directory: the path must end in `.clove` \
             (or be a symlink to the `.clove` beside it)"
        ),
    })
}

impl Ctx {
    /// The read engine for this repository.
    ///
    /// `no_index` is the `--no-index` flag, and it disables **both** accelerator
    /// tiers: the flag promises a file scan, and a daemon answering from its hot
    /// index would be no more of a file scan than the local one. `deep` is
    /// `--deep`, the thorough per-file staleness pass.
    ///
    /// Built per command rather than stored on `Ctx` because the two flags are
    /// per-invocation and the CLI is a short-lived process — there is nothing to
    /// amortize across calls, unlike the MCP server and the web, which hold one
    /// engine (and one cached daemon connection) for their lifetime.
    pub fn engine(&self, no_index: bool, deep: bool) -> clove_engine::Engine {
        clove_engine::Engine::new(
            self.store.clone(),
            self.clove_dir.clone(),
            clove_engine::Tiers {
                daemon: !no_index,
                index: !no_index,
                deep,
                auto_refresh: self.config.index.auto_refresh,
            },
        )
    }
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
