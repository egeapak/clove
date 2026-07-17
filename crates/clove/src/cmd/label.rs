//! `clove label <id> <add|rm> <label>` (T-CLI07).
//!
//! Thin shim over the unified [`EditRequest`] label-delta path (the same one
//! the web `PUT /labels` and MCP `add_labels`/`remove_labels` use), so the
//! normalize/sort/dedup semantics live in exactly one place.

use clove_core::{apply_edit, OutputFormat};
use clove_types::{CloveError, EditRequest, LabelEdit};
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

    let labels = match action.to_ascii_lowercase().as_str() {
        "add" => LabelEdit::Delta {
            add: vec![label.to_owned()],
            remove: Vec::new(),
        },
        "rm" | "remove" => LabelEdit::Delta {
            add: Vec::new(),
            remove: vec![label.to_owned()],
        },
        other => {
            return Err(CloveError::InvalidField {
                field: "action".to_owned(),
                reason: format!("expected add|rm, got `{other}`"),
            })
        }
    };

    let req = EditRequest {
        labels: Some(labels),
        ..EditRequest::default()
    };
    apply_edit(&ctx.store, &id, &req, now_seconds())?;

    let saved = ctx.store.get(&id)?;
    print_item(format, &saved, Map::new());
    Ok(())
}
