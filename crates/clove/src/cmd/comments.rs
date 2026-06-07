//! `clove comment` / `clove comments` (T-CLI12).

use clove_core::{add_comment, list_comments, OutputFormat};
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::context::{rel_to_root, Ctx};
use crate::output::print_json_success;
use crate::util::parse_id;

/// Resolve the comment author: `CLOVE_AUTHOR`, then `GIT_AUTHOR_EMAIL`, else
/// `unknown`.
fn author() -> String {
    std::env::var("CLOVE_AUTHOR")
        .ok()
        .or_else(|| std::env::var("GIT_AUTHOR_EMAIL").ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub fn add(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    message: &str,
    quiet: bool,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    if !ctx.store.exists(&id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let path = add_comment(&ctx.issues_dir, &id, &author(), message)?;
    let rel = rel_to_root(&ctx.root, &path);
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({ "id": id.as_str(), "path": rel.as_str() }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => {
            if !quiet {
                println!("added comment to {}", id.as_str());
            }
        }
    }
    Ok(())
}

pub fn list(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    limit: Option<usize>,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    if !ctx.store.exists(&id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let mut comments = list_comments(&ctx.issues_dir, &id)?;
    if let Some(n) = limit {
        if comments.len() > n {
            comments = comments.split_off(comments.len() - n);
        }
    }

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let values: Vec<Value> = comments
                .iter()
                .map(|c| {
                    json!({
                        "author": c.author,
                        "timestamp": c.timestamp.to_rfc3339(),
                        "body": c.body,
                    })
                })
                .collect();
            print_json_success(Value::Array(values), json!({ "warnings": [] }));
        }
        OutputFormat::Human => {
            for c in &comments {
                println!("{}  {}", c.timestamp.to_rfc3339(), c.author);
                println!("{}\n", c.body.trim_end());
            }
        }
    }
    Ok(())
}
