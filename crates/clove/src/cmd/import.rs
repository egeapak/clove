//! `clove import <tk|beads|github> <src> [--dry-run]` (T-M01/T-M02/T-M03).
//!
//! Phase 0 scaffolding: this parses the source kind and arguments and routes to
//! the shared `clove-import` layer. The concrete importers land in later phases;
//! for now every source returns a clean [`CloveError::NotYetImplemented`].

use clove_core::{CloveError, OutputFormat};

use crate::cli::{ImportArgs, ImportSource};
use crate::context::Ctx;

pub fn run(_ctx: &Ctx, _format: OutputFormat, args: ImportArgs) -> Result<(), CloveError> {
    let feature = match args.source {
        ImportSource::Tk { .. } => "import tk",
        ImportSource::Beads { .. } => "import beads",
        ImportSource::Github { .. } => "import github",
    };
    Err(CloveError::NotYetImplemented {
        feature: feature.to_owned(),
    })
}
