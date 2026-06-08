//! Exit codes (DESIGN.md §7.6) and the mapping from [`CloveError`] to a code +
//! stable string error code for the JSON envelope.

use clove_types::CloveError;

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
/// The `(code, exit)` pair comes from [`clove_types::error_code`] — the single
/// classification shared with the web API (§7.3 envelope / §7.6 exit table) — and
/// this maps the numeric exit to the [`ExitCode`] enum.
pub fn classify(error: &CloveError) -> (ExitCode, &'static str) {
    let (code, exit) = clove_types::error_code(error);
    let exit_code = match exit {
        0 => ExitCode::Success,
        1 => ExitCode::Usage,
        2 => ExitCode::NotFound,
        3 => ExitCode::Cycle,
        4 => ExitCode::Validation,
        5 => ExitCode::Io,
        6 => ExitCode::Index,
        7 => ExitCode::Daemon,
        _ => ExitCode::Io,
    };
    (exit_code, code)
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
