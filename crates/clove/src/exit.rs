//! Exit codes (DESIGN.md §7.6) and the mapping from [`CloveError`] to a code +
//! stable string error code for the JSON envelope.

use clove_core::CloveError;

/// The clove exit-code table. Values are stable and documented.
///
/// `Cycle`/`Index`/`Daemon` are part of the published table (§7.6) but are not
/// produced until their commands land (dep-cycle, index, daemon tasks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)]
pub enum ExitCode {
    /// Data returned (possibly empty).
    Success = 0,
    /// Bad flag, unknown subcommand, argument parse error.
    Usage = 1,
    /// Item does not exist.
    NotFound = 2,
    /// A `dep add` (or `--fail-on-cycle`) cycle was detected.
    Cycle = 3,
    /// Bad field value, id collision, invalid priority, config error.
    Validation = 4,
    /// `.clove/` missing, file unreadable, filesystem error.
    Io = 5,
    /// Stale index with `--strict`; index unrecoverable.
    Index = 6,
    /// Daemon communication failure.
    Daemon = 7,
}

impl ExitCode {
    pub fn code(self) -> u8 {
        self as u8
    }

    /// Convert to a process exit code.
    pub fn process(self) -> std::process::ExitCode {
        std::process::ExitCode::from(self as u8)
    }
}

/// Classify a [`CloveError`] into its exit code and stable string error code.
///
/// The string code is part of the agent-facing JSON contract (§7.3).
pub fn classify(error: &CloveError) -> (ExitCode, &'static str) {
    match error {
        CloveError::NotFound { .. } => (ExitCode::NotFound, "ITEM_NOT_FOUND"),

        CloveError::IdConflict { .. } | CloveError::CommentConflict { .. } => {
            (ExitCode::Validation, "ID_CONFLICT")
        }
        CloveError::InvalidId { .. } | CloveError::PathTraversal { .. } => {
            (ExitCode::Validation, "INVALID_ID")
        }
        CloveError::InvalidField { .. }
        | CloveError::EmptyLabel { .. }
        | CloveError::Invalid { .. } => (ExitCode::Validation, "VALIDATION_ERROR"),
        CloveError::HasDependents { .. } => (ExitCode::Validation, "HAS_DEPENDENTS"),
        CloveError::SelfDependency { .. } => (ExitCode::Validation, "SELF_LOOP"),
        CloveError::DependencyExists { .. } => (ExitCode::Validation, "ALREADY_EXISTS"),
        CloveError::DependencyCycle { .. } => (ExitCode::Cycle, "CYCLE_DETECTED"),
        CloveError::Config { .. } => (ExitCode::Validation, "CONFIG_ERROR"),

        // Malformed item files are data problems → validation, not I/O.
        CloveError::FrontmatterTooLarge { .. }
        | CloveError::BodyTooLarge { .. }
        | CloveError::AliasNotAllowed { .. }
        | CloveError::MissingFrontmatter { .. }
        | CloveError::UnterminatedFrontmatter { .. }
        | CloveError::IdMismatch { .. }
        | CloveError::InvalidYaml { .. } => (ExitCode::Validation, "PARSE_ERROR"),

        CloveError::NoRepo { .. } => (ExitCode::Io, "NO_REPO"),
        CloveError::Io { .. } => (ExitCode::Io, "IO_ERROR"),

        // `CloveError` is non_exhaustive; default unknown variants to I/O.
        _ => (ExitCode::Io, "ERROR"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn not_found_maps_to_exit_2() {
        let err = CloveError::NotFound {
            id: "proj-00000000".to_owned(),
        };
        let (code, name) = classify(&err);
        assert_eq!(code, ExitCode::NotFound);
        assert_eq!(code.code(), 2);
        assert_eq!(name, "ITEM_NOT_FOUND");
    }

    #[test]
    fn validation_errors_map_to_exit_4() {
        let err = CloveError::InvalidField {
            field: "priority".to_owned(),
            reason: "out of range".to_owned(),
        };
        assert_eq!(classify(&err).0, ExitCode::Validation);
    }

    #[test]
    fn io_maps_to_exit_5() {
        let err = CloveError::Io {
            path: Utf8PathBuf::from("/x"),
            source: std::io::Error::other("boom"),
        };
        assert_eq!(classify(&err).0, ExitCode::Io);
    }
}
