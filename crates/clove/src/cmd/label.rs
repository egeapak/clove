//! `clove label <id> <add|rm> <label>` (T-CLI07).

use clove_core::OutputFormat;
use clove_types::{normalize_label, CloveError};
use serde_json::Map;

use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id};

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    action: &str,
    label: &str,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    let mut item = ctx.store.get(&id)?;
    let canonical = normalize_label(label)?;

    match action.to_ascii_lowercase().as_str() {
        "add" => {
            if !item.frontmatter.labels.contains(&canonical) {
                item.frontmatter.labels.push(canonical);
                item.frontmatter.labels.sort();
                item.frontmatter.labels.dedup();
            }
        }
        "rm" | "remove" => item.frontmatter.labels.retain(|l| l != &canonical),
        other => {
            return Err(CloveError::InvalidField {
                field: "action".to_owned(),
                reason: format!("expected add|rm, got `{other}`"),
            })
        }
    }

    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}
