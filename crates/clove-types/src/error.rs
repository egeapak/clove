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

    /// The plugin registry (crates.io) could not be reached or understood —
    /// offline, rate-limited, a TLS/proxy failure, or a malformed response.
    ///
    /// Never raised for "this plugin does not exist": a 404 is an authoritative
    /// answer, not an error. Classified at exit 5 alongside the other
    /// environment failures, since the command was well-formed and the world
    /// was not cooperating.
    #[error("registry error: {message}")]
    Registry { message: String },

    /// A failure the daemon reported over IPC, carrying the classification it
    /// already computed.
    ///
    /// This exists because the original variant cannot be faithfully rebuilt on
    /// the client side — reconstructing e.g. `DependencyCycle` would mean
    /// inventing its `from`/`to`/`cycle` fields. Carrying `code`/`exit` across
    /// the wire instead means a write rejected by the daemon exits with the same
    /// code it would have locally. Only ever constructed client-side, from a
    /// `clove_ipc::ClientError`.
    #[error("{message}")]
    Remote {
        code: String,
        exit: u8,
        message: String,
    },

    /// A plugin binary was dispatched (structurally, probe-free — PLUGIN_SYSTEM.md
    /// §4.2) for a capability it does not implement. Raised by a multi-capability
    /// plugin's own `main` when it is handed a `<mux> <provider>` outside its
    /// `provides` set (e.g. an import-only binary reached via the export
    /// cross-sibling fallback). Maps to NotFound's exit code (2).
    #[error("{plugin} does not provide `{capability}`")]
    UnsupportedCapability { plugin: String, capability: String },
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
        // A capability miss is a "not found" at the plugin boundary — same exit (2)
        // as an unknown item, but a distinct wire code so tooling can tell them apart.
        CloveError::UnsupportedCapability { .. } => ("UNSUPPORTED_CAPABILITY", 2),

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
        CloveError::Registry { .. } => ("REGISTRY_ERROR", 5),

        // A remote failure names its class on the wire; resolve that name
        // against the local table rather than trusting the numeric `exit` that
        // came with it. Otherwise a daemon that is newer, older, or simply
        // buggy could hand back `("ITEM_NOT_FOUND", 200)` and steer the caller's
        // exit code — including into `error.json`'s `maximum: 7` on the web
        // envelope. An unrecognized code degrades to the generic daemon error.
        CloveError::Remote { code, .. } => canonical_code(code).unwrap_or(DAEMON_ERROR),
    }
}

/// The generic classification for a daemon failure this build cannot name.
const DAEMON_ERROR: (&str, u8) = ("DAEMON_ERROR", 7);

/// Every `(code, exit)` pair [`error_code`] can produce.
///
/// This is the lookup used to resolve a code that arrives off the wire (see the
/// [`CloveError::Remote`] arm). It must stay in step with the match above;
/// `every_error_code_is_canonical` fails if a variant's code is missing here.
const CODES: &[(&str, u8)] = &[
    ("ITEM_NOT_FOUND", 2),
    ("UNSUPPORTED_CAPABILITY", 2),
    ("ID_CONFLICT", 4),
    ("INVALID_ID", 4),
    ("VALIDATION_ERROR", 4),
    ("HAS_DEPENDENTS", 4),
    ("SELF_LOOP", 4),
    ("ALREADY_EXISTS", 4),
    ("CYCLE_DETECTED", 3),
    ("CONFIG_ERROR", 4),
    ("PARSE_ERROR", 4),
    ("NO_REPO", 5),
    ("IO_ERROR", 5),
    ("NOT_YET_IMPLEMENTED", 1),
    ("REGISTRY_ERROR", 5),
    DAEMON_ERROR,
];

/// Resolve a wire code to its canonical `(code, exit)`, if this build knows it.
fn canonical_code(code: &str) -> Option<(&'static str, u8)> {
    CODES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(known, exit)| (*known, *exit))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One instance of every [`CloveError`] variant.
    ///
    /// `exhaustive_variant_check` below is what keeps this honest: it matches on
    /// `CloveError` without a wildcard, so adding a variant fails to compile
    /// there and points here.
    fn one_of_each() -> Vec<CloveError> {
        let path = || Utf8PathBuf::from("/x");
        vec![
            CloveError::InvalidId {
                value: "x".into(),
                reason: "r".into(),
            },
            CloveError::PathTraversal { id: "x".into() },
            CloveError::IdConflict { attempts: 1 },
            CloveError::CommentConflict { attempts: 1 },
            CloveError::InvalidField {
                field: "f".into(),
                reason: "r".into(),
            },
            CloveError::EmptyLabel { raw: "".into() },
            CloveError::Config {
                path: path(),
                message: "m".into(),
            },
            CloveError::FrontmatterTooLarge {
                path: path(),
                limit: 1,
            },
            CloveError::BodyTooLarge {
                path: path(),
                limit: 1,
            },
            CloveError::AliasNotAllowed { path: path() },
            CloveError::MissingFrontmatter { path: path() },
            CloveError::UnterminatedFrontmatter { path: path() },
            CloveError::IdMismatch {
                path: path(),
                id: "a".into(),
                stem: "b".into(),
            },
            CloveError::InvalidYaml {
                path: path(),
                message: "m".into(),
            },
            CloveError::Invalid {
                path: path(),
                count: 1,
                summary: "s".into(),
            },
            CloveError::NoRepo { searched: path() },
            CloveError::NotFound { id: "x".into() },
            CloveError::HasDependents {
                id: "x".into(),
                dependents: vec![],
            },
            CloveError::SelfDependency { id: "x".into() },
            CloveError::DependencyCycle {
                from: "a".into(),
                to: "b".into(),
                cycle: vec![],
            },
            CloveError::DependencyExists {
                from: "a".into(),
                to: "b".into(),
            },
            CloveError::ScanFailed {
                path: path(),
                count: 1,
                message: "m".into(),
            },
            CloveError::Io {
                path: path(),
                source: std::io::Error::other("e"),
            },
            CloveError::NotYetImplemented {
                feature: "f".into(),
            },
            CloveError::Registry {
                message: "m".into(),
            },
            CloveError::Remote {
                code: "IO_ERROR".into(),
                exit: 5,
                message: "m".into(),
            },
            CloveError::UnsupportedCapability {
                plugin: "p".into(),
                capability: "c".into(),
            },
        ]
    }

    /// Adding a `CloveError` variant must fail to compile here until it is added
    /// to `one_of_each`, so the coverage of the tests below cannot silently rot.
    #[allow(dead_code)]
    fn exhaustive_variant_check(e: &CloveError) {
        match e {
            CloveError::InvalidId { .. }
            | CloveError::PathTraversal { .. }
            | CloveError::IdConflict { .. }
            | CloveError::CommentConflict { .. }
            | CloveError::InvalidField { .. }
            | CloveError::EmptyLabel { .. }
            | CloveError::Config { .. }
            | CloveError::FrontmatterTooLarge { .. }
            | CloveError::BodyTooLarge { .. }
            | CloveError::AliasNotAllowed { .. }
            | CloveError::MissingFrontmatter { .. }
            | CloveError::UnterminatedFrontmatter { .. }
            | CloveError::IdMismatch { .. }
            | CloveError::InvalidYaml { .. }
            | CloveError::Invalid { .. }
            | CloveError::NoRepo { .. }
            | CloveError::NotFound { .. }
            | CloveError::HasDependents { .. }
            | CloveError::SelfDependency { .. }
            | CloveError::DependencyCycle { .. }
            | CloveError::DependencyExists { .. }
            | CloveError::ScanFailed { .. }
            | CloveError::Io { .. }
            | CloveError::NotYetImplemented { .. }
            | CloveError::Registry { .. }
            | CloveError::Remote { .. }
            | CloveError::UnsupportedCapability { .. } => {}
        }
    }

    /// Every code `error_code` can emit must resolve through `CODES`, or a
    /// failure reported by a same-build daemon would silently degrade to
    /// `DAEMON_ERROR`.
    #[test]
    fn every_error_code_is_canonical() {
        for error in one_of_each() {
            let (code, exit) = error_code(&error);
            assert_eq!(
                canonical_code(code),
                Some((code, exit)),
                "`{code}` is missing from CODES, or its exit disagrees with the match arm"
            );
        }
    }

    /// A code that survives the wire classifies exactly as the local error does.
    /// This is the property the daemon seam exists for: a rejected write must
    /// exit the same whether the daemon or the local store refused it.
    #[test]
    fn remote_round_trips_every_code_to_the_local_classification() {
        for local in one_of_each() {
            let (code, exit) = error_code(&local);
            let remote = CloveError::Remote {
                code: code.to_owned(),
                exit,
                message: "over the wire".to_owned(),
            };
            assert_eq!(
                error_code(&remote),
                (code, exit),
                "`{code}` must round-trip"
            );
        }
    }

    /// The numeric `exit` on the wire is informational: classification resolves
    /// the *code* against the local table, so a daemon cannot push a caller to an
    /// arbitrary exit status (`error.json` caps `exit` at 7).
    #[test]
    fn a_bogus_remote_exit_is_ignored() {
        let hostile = CloveError::Remote {
            code: "ITEM_NOT_FOUND".to_owned(),
            exit: 200,
            message: "boom".to_owned(),
        };
        assert_eq!(error_code(&hostile), ("ITEM_NOT_FOUND", 2));
    }

    /// An unrecognized code (an older or newer daemon) degrades to the generic
    /// daemon error rather than being trusted.
    #[test]
    fn an_unknown_remote_code_degrades_to_daemon_error() {
        let future = CloveError::Remote {
            code: "SOME_FUTURE_CODE".to_owned(),
            exit: 3,
            message: "boom".to_owned(),
        };
        assert_eq!(error_code(&future), ("DAEMON_ERROR", 7));
    }
}
