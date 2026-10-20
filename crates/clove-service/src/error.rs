//! [`ToolError`] — why a tool call failed, keeping where it failed.
//!
//! The MCP server renders it as text (`Display`); the plugin host IPC needs the
//! machine code and exit status (`code()`, `into_clove_error()`). Keeping the
//! three origins apart lets each transport render exactly what it promised
//! without re-deriving the classification.

use clove_ipc::ClientError;
use clove_types::CloveError;

/// A failed tool call.
#[derive(Debug)]
pub enum ToolError {
    /// A local failure (the store, validation, an id, a filter), classified by
    /// [`clove_types::error_code`].
    Core(CloveError),
    /// The daemon reported or transported a failure. Rendered verbatim: the
    /// daemon's own classification is authoritative (see
    /// [`ClientError::App`]).
    Daemon(ClientError),
    /// The request itself is unusable (for example an edit with no fields).
    Usage(String),
}

impl ToolError {
    /// The stable machine code and exit status of this failure: the same pair a
    /// built-in CLI command reports for the same failure.
    pub fn code(&self) -> (&'static str, u8) {
        match self {
            ToolError::Core(e) => clove_types::error_code(e),
            // Same mapping as `From<ClientError> for CloveError`: the daemon's
            // code is resolved against the local table; anything it never
            // classified (connect, transport, refusal) is a daemon error.
            ToolError::Daemon(ClientError::App(rpc)) => {
                clove_types::error_code(&CloveError::Remote {
                    code: rpc.code.clone(),
                    exit: rpc.exit,
                    message: String::new(),
                })
            }
            ToolError::Daemon(_) => ("DAEMON_ERROR", 7),
            ToolError::Usage(_) => ("VALIDATION_ERROR", 4),
        }
    }

    /// The failure as the shared error type.
    pub fn into_clove_error(self) -> CloveError {
        match self {
            ToolError::Core(e) => e,
            ToolError::Daemon(e) => e.into(),
            ToolError::Usage(reason) => CloveError::InvalidField {
                field: "arguments".to_owned(),
                reason,
            },
        }
    }
}

impl std::fmt::Display for ToolError {
    /// The tool-result text. A local `CloveError` renders the way the daemon
    /// renders a remote one, `CODE: message`: read tools never reach the daemon
    /// and write tools fall back to local ops when none is running, so without
    /// this the *same* failure read `ITEM_NOT_FOUND: no item with id X` or
    /// `no item with id X` depending on whether a daemon happened to be up.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::Core(e) => {
                let (code, _exit) = clove_types::error_code(e);
                write!(f, "{code}: {e}")
            }
            ToolError::Daemon(e) => write!(f, "{e}"),
            ToolError::Usage(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<CloveError> for ToolError {
    fn from(e: CloveError) -> Self {
        ToolError::Core(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clove_ipc::RpcError;

    #[test]
    fn a_local_failure_renders_and_classifies_like_the_cli() {
        let e = ToolError::Core(CloveError::NotFound {
            id: "proj-0000000A".to_owned(),
        });
        assert_eq!(e.code(), ("ITEM_NOT_FOUND", 2));
        assert!(e.to_string().starts_with("ITEM_NOT_FOUND: "), "{e}");
    }

    #[test]
    fn a_daemon_failure_keeps_its_own_code_and_text() {
        let rpc = RpcError::with_exit("CYCLE_DETECTED", "would create a cycle", 3);
        let e = ToolError::Daemon(ClientError::App(rpc));
        assert_eq!(e.code(), ("CYCLE_DETECTED", 3));
        assert_eq!(e.to_string(), "CYCLE_DETECTED: would create a cycle");

        let lost = ToolError::Daemon(ClientError::Transport("reset".to_owned()));
        assert_eq!(lost.code(), ("DAEMON_ERROR", 7));
    }

    #[test]
    fn a_usage_failure_is_a_validation_error() {
        let e = ToolError::Usage("no fields to edit".to_owned());
        assert_eq!(e.code(), ("VALIDATION_ERROR", 4));
        assert_eq!(e.to_string(), "no fields to edit");
        assert_eq!(
            clove_types::error_code(&e.into_clove_error()),
            ("VALIDATION_ERROR", 4)
        );
    }
}
