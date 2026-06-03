//! `clove export <json|jsonl|github> [--out FILE] [--dry-run]` (T-M03/T-M04).
//!
//! Phase 0 scaffolding: this parses the format and arguments. The JSON/JSONL
//! writers (Phase 1) and GitHub push (Phase 5) land later; for now every format
//! returns a clean [`CloveError::NotYetImplemented`].

use clove_core::{CloveError, OutputFormat};

use crate::cli::{ExportArgs, ExportFormat};
use crate::context::Ctx;

pub fn run(_ctx: &Ctx, _format: OutputFormat, args: ExportArgs) -> Result<(), CloveError> {
    let feature = match args.export_format {
        ExportFormat::Json => "export json",
        ExportFormat::Jsonl => "export jsonl",
        ExportFormat::Github => "export github",
    };
    Err(CloveError::NotYetImplemented {
        feature: feature.to_owned(),
    })
}
