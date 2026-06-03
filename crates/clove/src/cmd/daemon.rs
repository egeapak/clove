//! `clove daemon <start|stop|status>` (DESIGN §7.2, §8).
//!
//! The daemon is an optional accelerator: it watches `.clove/issues/` and keeps
//! the index hot, but every read command works identically without it.
//!
//! **Phase 0 (this commit):** the subcommand surface and dispatch only — each
//! action is a wired stub. `start` (detached spawn) and `status` (IPC `STATUS`)
//! land in Phase 4 (T-D05), on top of the daemon lifecycle (P1) and IPC (P2).

use clove_core::{CloveError, OutputFormat};

use crate::cli::DaemonAction;
use crate::context::Ctx;
use crate::exit::ExitCode;

pub fn run(
    _ctx: &Ctx,
    _format: OutputFormat,
    action: DaemonAction,
) -> Result<ExitCode, CloveError> {
    let feature = match action {
        DaemonAction::Start => "daemon start",
        DaemonAction::Stop => "daemon stop",
        DaemonAction::Status => "daemon status",
    };
    Err(CloveError::NotYetImplemented {
        feature: feature.to_owned(),
    })
}
