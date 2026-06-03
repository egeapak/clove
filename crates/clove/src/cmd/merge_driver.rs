//! `clove merge-driver <ancestor> <ours> <theirs> <marker-size>` (T-M05).
//!
//! Invoked by git via the `[merge "clove-item"]` driver that `clove init
//! --merge-driver` installs. Phase 0 scaffolding: this parses the four git
//! positionals (`%O %A %B %L`) and returns a clean
//! [`CloveError::NotYetImplemented`]. The real 3-way merge lands in Phase 2.

use clove_core::{CloveError, OutputFormat};

use crate::cli::MergeDriverArgs;

pub fn run(_format: OutputFormat, _args: MergeDriverArgs) -> Result<(), CloveError> {
    Err(CloveError::NotYetImplemented {
        feature: "merge-driver".to_owned(),
    })
}
