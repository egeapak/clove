//! The error type for `clove-import`.
//!
//! Wraps [`clove_types::CloveError`] for store/validation failures and adds
//! import-specific variants (source parsing, unsupported sources). The CLI maps
//! these to exit codes at its boundary.

use camino::Utf8PathBuf;
use thiserror::Error;

/// Errors produced while planning or applying an import.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImportError {
    /// A failure originating in `clove-core` (store I/O, validation, label
    /// normalization, …).
    #[error(transparent)]
    Core(#[from] clove_types::CloveError),

    /// The import source file/directory could not be read or did not parse.
    #[error("failed to read import source `{path}`: {message}")]
    Source { path: Utf8PathBuf, message: String },

    /// A single source record was malformed (line/entry-level).
    #[error("malformed source record: {message}")]
    Record { message: String },

    /// The import source was produced by a newer clove than this build can read
    /// (the export container `format` or a per-item `schema` is too new).
    #[error("{message}")]
    Incompatible { message: String },

    /// A GitHub API call failed. `operation` names the call and the item it was
    /// for (e.g. `issue update of #3 (gh-3) for proj-…`); `reason` is GitHub's
    /// status, message, and validation errors, or the transport failure.
    #[error("GitHub {operation} failed: {reason}")]
    GitHub { operation: String, reason: String },
}
