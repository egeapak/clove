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

    /// Could not generate a collision-free comment filename after the retry
    /// budget.
    #[error("could not create a unique comment file after {attempts} attempts")]
    CommentConflict { attempts: u32 },

    /// A field value failed validation (range, format, …).
    #[error("invalid {field}: {reason}")]
    InvalidField { field: String, reason: String },

    /// A label was empty after normalization (DESIGN.md §2.2).
    #[error("label is empty after normalization: {raw:?}")]
    EmptyLabel { raw: String },

    /// The frontmatter block exceeds [`crate::limits::MAX_FRONTMATTER_BYTES`].
    #[error("frontmatter exceeds {limit} bytes in `{path}`")]
    FrontmatterTooLarge { path: Utf8PathBuf, limit: usize },

    /// The body exceeds [`crate::limits::MAX_BODY_BYTES`].
    #[error("body exceeds {limit} bytes in `{path}`")]
    BodyTooLarge { path: Utf8PathBuf, limit: usize },

    /// YAML anchors/aliases were found in the frontmatter (bomb guard, §12.2).
    #[error("YAML anchors/aliases are not allowed in `{path}`")]
    AliasNotAllowed { path: Utf8PathBuf },

    /// The file does not begin with a `---` frontmatter fence.
    #[error("missing `---` frontmatter fence in `{path}`")]
    MissingFrontmatter { path: Utf8PathBuf },

    /// The frontmatter block has no closing `---` fence.
    #[error("unterminated frontmatter (no closing `---`) in `{path}`")]
    UnterminatedFrontmatter { path: Utf8PathBuf },

    /// The `id` field does not match the file name stem.
    #[error("id `{id}` does not match filename stem `{stem}` in `{path}`")]
    IdMismatch {
        path: Utf8PathBuf,
        id: String,
        stem: String,
    },

    /// The frontmatter YAML failed to deserialize.
    #[error("failed to parse frontmatter in `{path}`: {message}")]
    InvalidYaml { path: Utf8PathBuf, message: String },

    /// One or more field-level validations failed (see [`crate::validate`]).
    #[error("{count} validation error(s) in `{path}`: {summary}")]
    Invalid {
        path: Utf8PathBuf,
        count: usize,
        summary: String,
    },

    /// No item exists with the requested id.
    #[error("no item with id `{id}`")]
    NotFound { id: String },

    /// Deletion was refused because other items depend on this one.
    #[error("`{id}` has {} dependent(s): {}", dependents.len(), dependents.join(", "))]
    HasDependents { id: String, dependents: Vec<String> },

    /// A filesystem operation failed.
    #[error("io error at `{path}`: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
}
