//! Typed errors for the clove core libraries (`clove-types` / `clove-core`).
//!
//! These crates never use `anyhow` (that belongs to the CLI/daemon). Every
//! fallible operation returns a `CloveError` carrying enough context to map to
//! the exit-code table in DESIGN.md §7.6.

use camino::Utf8PathBuf;
use thiserror::Error;

/// The error type for all of `clove-types` / `clove-core`.
///
/// Variants are added as tasks need them; each maps to a stable error code +
/// exit code via [`error_code`] (DESIGN.md §7.6) — the single mapping shared by
/// the CLI exit table and the web API's HTTP-status mapping.
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

    /// The repository configuration is invalid or unreadable.
    #[error("config error in `{path}`: {message}")]
    Config { path: Utf8PathBuf, message: String },

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

    /// No `.clove/` directory was found in the current directory or any ancestor.
    #[error("no clove repository found in `{searched}` or any parent (run `clove init`)")]
    NoRepo { searched: Utf8PathBuf },

    /// No item exists with the requested id.
    #[error("no item with id `{id}`")]
    NotFound { id: String },

    /// Deletion was refused because other items depend on this one.
    #[error("`{id}` has {} dependent(s): {}", dependents.len(), dependents.join(", "))]
    HasDependents { id: String, dependents: Vec<String> },

    /// `dep add` was given an item as its own dependency.
    #[error("`{id}` cannot depend on itself")]
    SelfDependency { id: String },

    /// `dep add` would introduce a hard-dependency cycle.
    #[error("adding `{from}` → `{to}` would create a cycle: {}", cycle.join(" → "))]
    DependencyCycle {
        from: String,
        to: String,
        cycle: Vec<String>,
    },

    /// `dep add` for a dependency that is already present.
    #[error("`{from}` already depends on `{to}`")]
    DependencyExists { from: String, to: String },

    /// A store-wide validation (cycle check, ancestry walk, dependents check)
    /// could not be performed because one or more item files failed to parse.
    /// Validating against the partial graph would silently let invalid edges
    /// (real cycles, hidden dependents) through, so the mutation is refused
    /// until the broken file(s) are repaired (`clove doctor` lists them).
    #[error("cannot validate against the store: {count} item file(s) failed to parse (first: `{path}`: {message})")]
    ScanFailed {
        path: Utf8PathBuf,
        count: usize,
        message: String,
    },

    /// A filesystem operation failed.
    #[error("io error at `{path}`: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A command (or feature) exists in the CLI surface but its behavior has not
    /// been implemented yet. Used by M2 scaffolding stubs (import/export/
    /// merge-driver) until each phase lands.
    #[error("not yet implemented: {feature}")]
    NotYetImplemented { feature: String },
}

/// The stable string error code and numeric exit code for a [`CloveError`]
/// (DESIGN.md §7.3 envelope `error.code` / §7.6 exit table).
///
/// This is the **single** classification shared by the CLI's exit-code mapping
/// and the web API's HTTP-status mapping, so both report the same `code`/`exit`
/// for the same failure. The CLI maps the `exit` to its `ExitCode` enum; the web
/// layer maps it (with a few variant-specific refinements) to an HTTP status.
pub fn error_code(error: &CloveError) -> (&'static str, u8) {
    match error {
        CloveError::NotFound { .. } => ("ITEM_NOT_FOUND", 2),

        CloveError::IdConflict { .. } | CloveError::CommentConflict { .. } => ("ID_CONFLICT", 4),
        CloveError::InvalidId { .. } | CloveError::PathTraversal { .. } => ("INVALID_ID", 4),
        CloveError::InvalidField { .. }
        | CloveError::EmptyLabel { .. }
        | CloveError::Invalid { .. } => ("VALIDATION_ERROR", 4),
        CloveError::HasDependents { .. } => ("HAS_DEPENDENTS", 4),
        CloveError::SelfDependency { .. } => ("SELF_LOOP", 4),
        CloveError::DependencyExists { .. } => ("ALREADY_EXISTS", 4),
        CloveError::DependencyCycle { .. } => ("CYCLE_DETECTED", 3),
        CloveError::Config { .. } => ("CONFIG_ERROR", 4),

        // Malformed item files are data problems → validation, not I/O.
        CloveError::FrontmatterTooLarge { .. }
        | CloveError::BodyTooLarge { .. }
        | CloveError::AliasNotAllowed { .. }
        | CloveError::MissingFrontmatter { .. }
        | CloveError::UnterminatedFrontmatter { .. }
        | CloveError::IdMismatch { .. }
        | CloveError::InvalidYaml { .. }
        | CloveError::ScanFailed { .. } => ("PARSE_ERROR", 4),

        CloveError::NoRepo { .. } => ("NO_REPO", 5),
        CloveError::Io { .. } => ("IO_ERROR", 5),
        CloveError::NotYetImplemented { .. } => ("NOT_YET_IMPLEMENTED", 1),
    }
}
