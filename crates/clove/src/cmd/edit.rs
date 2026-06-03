//! `clove edit` (T-CLI05) and the shared non-interactive field application used
//! by `clove set`.

use clove_core::{normalize_label, parse_item_file, CloveError, ItemFrontmatter, OutputFormat};
use serde_json::Map;

use crate::cli::EditArgs;
use crate::cmd::status::set_status;
use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id, parse_priority, parse_status, parse_type};

/// Apply a list of `KEY=VALUE` (and `labels+=`/`labels-=`) edits to frontmatter.
/// All edits are applied to the in-memory copy before a single write.
pub fn apply_assignments(
    fm: &mut ItemFrontmatter,
    assignments: &[String],
) -> Result<(), CloveError> {
    for token in assignments {
        apply_one(fm, token)?;
    }
    Ok(())
}

fn apply_one(fm: &mut ItemFrontmatter, token: &str) -> Result<(), CloveError> {
    let (raw_key, value) = token
        .split_once('=')
        .ok_or_else(|| CloveError::InvalidField {
            field: "edit".to_owned(),
            reason: format!("expected KEY=VALUE, got `{token}`"),
        })?;

    // labels+=val / labels-=val
    if let Some(key) = raw_key.strip_suffix('+') {
        require_labels(key)?;
        let canonical = normalize_label(value)?;
        if !fm.labels.contains(&canonical) {
            fm.labels.push(canonical);
            fm.labels.sort();
            fm.labels.dedup();
        }
        return Ok(());
    }
    if let Some(key) = raw_key.strip_suffix('-') {
        require_labels(key)?;
        let canonical = normalize_label(value)?;
        fm.labels.retain(|l| l != &canonical);
        return Ok(());
    }

    match raw_key {
        "status" => set_status(fm, parse_status(value)?),
        "priority" => {
            let n: u8 = value.parse().map_err(|_| CloveError::InvalidField {
                field: "priority".to_owned(),
                reason: format!("expected 0–4, got `{value}`"),
            })?;
            fm.priority = parse_priority(n)?;
        }
        "type" => fm.item_type = parse_type(value)?,
        "assignee" => {
            fm.assignee = if value.trim().is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
        }
        "title" => {
            if value.trim().is_empty() {
                return Err(CloveError::InvalidField {
                    field: "title".to_owned(),
                    reason: "title cannot be empty".to_owned(),
                });
            }
            fm.title = value.to_owned();
        }
        other => {
            return Err(CloveError::InvalidField {
                field: other.to_owned(),
                reason:
                    "unknown editable field (status|priority|type|assignee|title|labels+=|labels-=)"
                        .to_owned(),
            })
        }
    }
    Ok(())
}

fn require_labels(key: &str) -> Result<(), CloveError> {
    if key == "labels" {
        Ok(())
    } else {
        Err(CloveError::InvalidField {
            field: key.to_owned(),
            reason: "only `labels` supports += / -=".to_owned(),
        })
    }
}

pub fn run(ctx: &Ctx, format: OutputFormat, args: EditArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;

    if args.fields.is_empty() {
        return open_in_editor(ctx, &id);
    }

    let mut item = ctx.store.get(&id)?;
    apply_assignments(&mut item.frontmatter, &args.fields)?;
    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}

/// Open the item file in `$EDITOR`/`$VISUAL`, then re-parse to validate it.
fn open_in_editor(ctx: &Ctx, id: &clove_core::CloveId) -> Result<(), CloveError> {
    let path = ctx.store.path_for(id);
    if !path.exists() {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let editor = std::env::var("CLOVE_EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    let status = std::process::Command::new(&editor)
        .arg(path.as_str())
        .status()
        .map_err(|source| CloveError::Io {
            path: path.clone(),
            source,
        })?;
    if !status.success() {
        return Err(CloveError::Io {
            path: path.clone(),
            source: std::io::Error::other(format!("editor `{editor}` exited with failure")),
        });
    }

    // Validate that the result still parses (surfaces a corrupt hand-edit).
    parse_item_file(&path)?;
    Ok(())
}
