//! `clove agent-doc` (T-CLI17): emit a self-contained, deterministic usage
//! document for agents, with an embedded schema-version marker that `--check`
//! can validate.

use clove_core::model::CURRENT_SCHEMA_VERSION;
use clove_core::{CloveError, OutputFormat};
use serde_json::json;

use crate::cli::AgentDocArgs;
use crate::output::print_json_success;

pub fn run(format: OutputFormat, args: AgentDocArgs) -> Result<(), CloveError> {
    if args.check {
        return check(args.file.as_deref());
    }

    let doc = generate();
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({ "schema": CURRENT_SCHEMA_VERSION, "markdown": doc }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => match &args.out {
            Some(path) => std::fs::write(path, &doc).map_err(|source| CloveError::Io {
                path: path.clone(),
                source,
            })?,
            None => print!("{doc}"),
        },
    }
    Ok(())
}

/// The marker line embedded at the top of generated docs.
fn marker() -> String {
    format!(
        "<!-- generated-by: clove v{} schema:{} -->",
        env!("CARGO_PKG_VERSION"),
        CURRENT_SCHEMA_VERSION
    )
}

/// Generate the document. Deterministic: identical bytes on every run for a
/// given binary (no timestamps), so `--check` and idempotency tests are stable.
fn generate() -> String {
    let schema = CURRENT_SCHEMA_VERSION;
    format!(
        "{marker}\n\
# clove for agents\n\
\n\
clove is a git-native, dependency-aware work-item tracker. Plain Markdown +\n\
YAML-frontmatter files under `.clove/issues/` are the source of truth. Pass\n\
`--format json` (or set `CLOVE_FORMAT=json`) to get machine output on stdout.\n\
\n\
## JSON envelope\n\
\n\
Every JSON response is `{{ \"v\": 1, \"ok\": <bool>, ... }}`:\n\
- success: `{{ \"v\":1, \"ok\":true, \"data\": <value>, \"_meta\": {{...}} }}`\n\
- error:   `{{ \"v\":1, \"ok\":false, \"error\": {{ \"code\": <STR>, \"message\": <STR>, \"exit\": <N> }} }}`\n\
\n\
The item `schema` version is currently **{schema}**. Re-read this document if it\n\
changes (`clove agent-doc --check --file <path>` verifies a saved copy).\n\
\n\
## Exit codes\n\
\n\
| code | meaning |\n\
|------|---------|\n\
| 0 | success |\n\
| 1 | usage / bad arguments |\n\
| 2 | item not found |\n\
| 3 | dependency cycle |\n\
| 4 | validation error |\n\
| 5 | i/o or missing `.clove/` |\n\
| 6 | index error |\n\
| 7 | daemon error |\n\
\n\
## Commands\n\
\n\
- `clove init [--prefix STR] [--merge-driver]` — create `.clove/`.\n\
- `clove new <title> [--type T] [-p N] [-l LABEL]... [--dep ID]... [--parent ID] [-a WHO] [-b TEXT]`\n\
- `clove show <id> [--fields LIST] [-v]` — one item (`-v`/json compute `ready`/`blocked_by`).\n\
- `clove edit <id> [--field KEY=VALUE]...` / `clove set <id> KEY=VALUE...`\n\
- `clove status <id> <open|in_progress|closed>` (aliases `start`, `close`).\n\
- `clove label <id> <add|rm> <label>`, `clove assign <id> <who|--clear>`, `clove priority <id> <0-4>`.\n\
- `clove dep add <id> <dep-id>` / `dep rm` / `dep tree <id> [--depth N|--full] [--flat]` / `dep cycle [--fail-on-cycle]`.\n\
- `clove ready` / `clove blocked` — work queues (filters: `--status --type --label --assignee --priority`).\n\
- `clove ls` / `clove query [--filter JSON]` — list/query (`--fields`, `--limit`, `--offset`). Lists are capped at 100 by default (`_meta.total` is the full count; `--limit 0` for all).\n\
- `clove comment <id> <message>` / `clove comments <id> [--limit N]`.\n\
- `clove search <text> [--limit N]` — full-text (index) or substring (files) search.\n\
- `clove reindex` — rebuild the SQLite index. `clove doctor [--fix] [--strict]` — health check.\n\
- `clove version` — `{{ clove, schema, git_hash, build_date }}`.\n\
\n\
## Conventions\n\
\n\
- Labels are case-insensitive and canonicalized (`Area:iOS` → `area:ios`).\n\
- Priority is 0 (highest) – 4, default 2. Types: bug, feature, chore, docs, epic.\n\
- Dependencies are hard/blocking; `ready` = open with all deps closed and none missing.\n",
        marker = marker(),
        schema = schema,
    )
}

/// Verify a saved doc's embedded schema version matches this binary.
fn check(file: Option<&camino::Utf8Path>) -> Result<(), CloveError> {
    let path = file.ok_or_else(|| CloveError::InvalidField {
        field: "file".to_owned(),
        reason: "--check requires --file PATH".to_owned(),
    })?;
    let contents = std::fs::read_to_string(path).map_err(|source| CloveError::Io {
        path: path.to_owned(),
        source,
    })?;

    let found = extract_schema(&contents).ok_or_else(|| CloveError::InvalidField {
        field: "agent-doc".to_owned(),
        reason: "no `generated-by: clove ... schema:N` marker found".to_owned(),
    })?;

    if found != CURRENT_SCHEMA_VERSION {
        return Err(CloveError::InvalidField {
            field: "agent-doc".to_owned(),
            reason: format!("stale: doc schema {found}, binary schema {CURRENT_SCHEMA_VERSION}"),
        });
    }
    Ok(())
}

/// Parse the `schema:N` value out of the marker line.
fn extract_schema(contents: &str) -> Option<u32> {
    let marker_line = contents
        .lines()
        .find(|l| l.contains("generated-by: clove"))?;
    let after = marker_line.split("schema:").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}
