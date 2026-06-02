//! `clove version` (T-CLI13).

use clove_core::model::CURRENT_SCHEMA_VERSION;
use clove_core::{CloveError, OutputFormat};
use serde_json::json;

use crate::output::print_json_success;

/// Print version and schema information. The `schema` field lets agents detect
/// schema bumps at startup.
pub fn run(format: OutputFormat) -> Result<(), CloveError> {
    let version = env!("CARGO_PKG_VERSION");
    let data = json!({
        "clove": version,
        "schema": CURRENT_SCHEMA_VERSION,
        "git_hash": option_env!("CLOVE_GIT_HASH"),
        "build_date": option_env!("CLOVE_BUILD_DATE"),
    });

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_success(data, json!({ "warnings": [] }));
        }
        OutputFormat::Human => {
            println!("clove {version}");
        }
    }
    Ok(())
}
