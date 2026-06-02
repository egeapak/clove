//! Typed errors for clove-core.
//!
//! `clove-core` never uses `anyhow` (that belongs to the CLI/daemon). Every
//! fallible operation returns a `CloveError` carrying enough context to map to
//! the exit-code table in DESIGN.md §7.6.

use camino::Utf8PathBuf;
use thiserror::Error;

/// The error type for all of `clove-core`.
///
/// Variants are added as tasks need them; each maps to an exit code at the CLI
/// boundary (see DESIGN.md §7.6). Keep the mapping in one place there, not here.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CloveError {
    /// An ID string failed `CloveId` validation.
    #[error("invalid id `{value}`: {reason}")]
    InvalidId { value: String, reason: String },

    /// A resolved path escaped the `.clove/issues/` root.
    #[error("path traversal rejected for id `{id}`")]
    PathTraversal { id: String },

    /// Could not generate a collision-free ID after the retry budget.
    #[error("could not generate a unique id after {attempts} attempts")]
    IdConflict { attempts: u32 },

    /// A filesystem operation failed.
    #[error("io error at `{path}`: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
}
