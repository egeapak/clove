//! `clove plugin list` — enumerate installed subcommand plugins.
//!
//! A read-only view over [`crate::plugin::list`] (a pure `stat` walk of the §5
//! search path): one `name<TAB>path` line per plugin in human mode, or the
//! standard `{ v, ok, data, _meta }` envelope with a `data` array of
//! `{ name, path }` in JSON/JSONL. Needs no repository.

use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::output::{print_json_list, print_jsonl_items};
use crate::plugin;

/// Render the installed plugins in the requested `format`.
pub fn run(format: OutputFormat) -> Result<(), CloveError> {
    let plugins = plugin::list();
    let items: Vec<Value> = plugins
        .iter()
        .map(|p| json!({ "name": p.name, "path": p.path.as_str() }))
        .collect();

    match format {
        OutputFormat::Human => {
            for p in &plugins {
                println!("{}\t{}", p.name, p.path);
            }
        }
        OutputFormat::Json => print_json_list(items, json!({ "count": plugins.len() })),
        OutputFormat::Jsonl => print_jsonl_items(&items),
    }
    Ok(())
}
