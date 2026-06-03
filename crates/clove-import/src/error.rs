//! The error type for `clove-import`.
//!
//! Wraps [`clove_core::CloveError`] for store/validation failures and adds
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
    Core(#[from] clove_core::CloveError),

    /// The import source file/directory could not be read or did not parse.
    #[error("failed to read import source `{path}`: {message}")]
    Source { path: Utf8PathBuf, message: String },

    /// A single source record was malformed (line/entry-level).
    #[error("malformed source record: {message}")]
    Record { message: String },

    /// The requested capability is not yet implemented.
    #[error("not yet implemented: {feature}")]
    NotYetImplemented { feature: String },
}
