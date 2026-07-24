//! `clove plugin list` — enumerate installed subcommand plugins, enriched.
//!
//! A read-only view over [`crate::plugin::list_enriched`] (the pure `stat` walk of
//! the §5 search path, plus a bounded `--clove-plugin-info` probe of each binary,
//! `PLUGIN_REGISTRY.md` §3). Human mode renders a `NAME / VERSION / RUN AS /
//! ABOUT` table (with a compat note for an out-of-range plugin); JSON/JSONL emit
//! the standard `{ v, ok, data, _meta }` envelope with one additive object per
//! plugin (`{ name, binary, path, version, about, provides, commands, installed,
//! status }`). Needs no repository — a hung/old plugin can't wedge it (the probe
//! is time-bounded) and an unprobeable one still lists from its name.

use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::output::{print_json_list, print_jsonl_items};
use crate::plugin::{self, EnrichedPlugin, PluginStatus};

/// Render the installed plugins in the requested `format`.
pub fn run(format: OutputFormat) -> Result<(), CloveError> {
    let plugins = plugin::list_enriched();

    match format {
        OutputFormat::Human => render_human(&plugins),
        OutputFormat::Json => {
            let items: Vec<Value> = plugins.iter().map(to_json).collect();
            print_json_list(items, json!({ "count": plugins.len() }));
        }
        OutputFormat::Jsonl => {
            let items: Vec<Value> = plugins.iter().map(to_json).collect();
            print_jsonl_items(&items);
        }
    }
    Ok(())
}

/// The additive JSON object for one enriched plugin (§3): today's `{ name, path }`
/// plus `binary`, the probed `version`/`about`/`provides`, the derived
/// `commands`, `installed:true`, and the compat `status`.
fn to_json(plugin: &EnrichedPlugin) -> Value {
    let binary = format!("clove-{}", plugin.info.name);
    let version = plugin.probed.as_ref().map(|p| p.version.as_str());
    let about = plugin.probed.as_ref().map(|p| p.about.as_str());
    let provides: Vec<&str> = plugin
        .probed
        .as_ref()
        .map(|p| p.provides.iter().map(String::as_str).collect())
        .unwrap_or_default();

    json!({
        "name": plugin.info.name,
        "binary": binary,
        "path": plugin.info.path.as_str(),
        "version": version,
        "about": about,
        "provides": provides,
        "commands": plugin.commands,
        "installed": true,
        "status": plugin.status.as_str(),
    })
}

/// Render the human `NAME / VERSION / RUN AS / ABOUT` table (§3).
///
/// A plugin that failed the probe (`no_info`) shows `—` for version and
/// `(no metadata)` for about; an out-of-range plugin (`outdated` /
/// `needs_newer_clove`) gets a trailing compat note on its row.
fn render_human(plugins: &[EnrichedPlugin]) {
    if plugins.is_empty() {
        return;
    }

    struct Row {
        name: String,
        version: String,
        run_as: String,
        about: String,
    }

    let rows: Vec<Row> = plugins
        .iter()
        .map(|p| {
            let (version, about) = match &p.probed {
                Some(info) => (info.version.clone(), info.about.clone()),
                None => ("—".to_owned(), "(no metadata)".to_owned()),
            };
            let mut about = about;
            match p.status {
                PluginStatus::Outdated => {
                    about.push_str("  [outdated: predates this clove; runs with a warning]");
                }
                PluginStatus::NeedsNewerClove => {
                    about.push_str("  [needs a newer clove]");
                }
                PluginStatus::Ok | PluginStatus::NoInfo => {}
            }
            Row {
                name: p.info.name.clone(),
                version,
                run_as: p.commands.join(", "),
                about,
            }
        })
        .collect();

    let name_w = header_width("NAME", rows.iter().map(|r| r.name.as_str()));
    let version_w = header_width("VERSION", rows.iter().map(|r| r.version.as_str()));
    let run_as_w = header_width("RUN AS", rows.iter().map(|r| r.run_as.as_str()));

    println!(
        "{:<name_w$}  {:<version_w$}  {:<run_as_w$}  ABOUT",
        "NAME", "VERSION", "RUN AS"
    );
    for row in &rows {
        println!(
            "{:<name_w$}  {:<version_w$}  {:<run_as_w$}  {}",
            row.name, row.version, row.run_as, row.about
        );
    }
}

/// The column width: the wider of the header and the widest cell.
fn header_width<'a>(header: &str, cells: impl Iterator<Item = &'a str>) -> usize {
    cells
        .map(str::len)
        .chain(std::iter::once(header.len()))
        .max()
        .unwrap_or(header.len())
}
