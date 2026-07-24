//! `clove agent-doc` (T-CLI17): emit a self-contained, deterministic usage
//! document for agents, with an embedded schema-version marker that `--check`
//! can validate.

use clove_core::OutputFormat;
use clove_types::model::CURRENT_SCHEMA_VERSION;
use clove_types::CloveError;
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
- `clove setup [--global] [--dry-run]` — register the `clove mcp` server (+ tool\n\
  permissions) with Claude Code and write `CLOVE.md` agent directives.\n\
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
- `clove stats [--top N] [--no-epics] [--snapshot] [--history [--since RFC3339] [--limit N]]` — work-item analytics (counts by status/type/priority/assignee/label, ready/blocked, cycles, epic rollups, throughput) plus daemon/index telemetry. `--snapshot` persists to the index's durable history (`.clove/index.db`); `--history` replays the series.\n\
- `clove reindex` — rebuild the SQLite index. `clove doctor [--fix] [--strict]` — health check.\n\
- `clove version` — `{{ clove, schema, git_hash, build_date }}`.\n\
\n\
## Interop (import / export / merge)\n\
\n\
- `clove export json` / `clove export jsonl [--out FILE]` — dump all items as a\n\
  JSON envelope (`data` array) or one item per line (NDJSON), in clove's native\n\
  item schema — the exact inverse of `import json|jsonl`. (A Beads-native export\n\
  is the `beads` plugin, `clove export beads`, not this built-in.)\n\
- `clove import json <file>` / `clove import jsonl <file> [--dry-run]\n\
  [--overwrite]` — built-in native restore, the inverse of `export json|jsonl`:\n\
  recreates items preserving their ids (existing ids skipped unless\n\
  `--overwrite`). A full `export → import` round-trip.\n\
- `clove import tk <.tickets-dir> [--dry-run]` — import tk tickets (needs the\n\
  `clove-import-tk` plugin; `cargo install clove-import-tk`).\n\
- `clove import beads <issues.jsonl> [--dry-run]` — import a Beads JSONL export\n\
  (needs the `clove-import-beads` plugin).\n\
- `clove sync github <owner/repo> [--dry-run] [--prefer P] [--no-comments]` —\n\
  two-way GitHub sync (pull + push + comments in one pass; conflict policy\n\
  `newer|local|remote|manual`). Needs the `clove-sync-github` plugin\n\
  (`cargo install clove-sync-github`) + a token via `GITHUB_TOKEN` or\n\
  `gh auth token`; without the plugin it exits 4 with an install hint. The\n\
  single GitHub path (replaces the old\n\
  one-way `import github` / `export github`).\n\
- File imports are idempotent on `external_ref`: re-running skips already-imported\n\
  items. `--dry-run` reports `{{ would_create, would_skip, conflicts }}` and\n\
  writes nothing.\n\
- `clove init --merge-driver` installs a git merge driver for\n\
  `.clove/issues/*.md`. On `git merge`, same-value scalar edits and dependency/\n\
  label set-unions auto-resolve; only genuinely divergent edits conflict.\n\
\n\
## Git integration\n\
\n\
- Files are the source of truth and travel with the repo. After a `git merge` or\n\
  `git pull` the SQLite index refreshes automatically on the next command\n\
  (staleness is detected and the index reindexed transparently), so reads stay\n\
  correct without a manual `clove reindex`.\n\
\n\
## Daemon (optional)\n\
\n\
- `clove daemon start|stop|status` runs an optional background process that keeps\n\
  the index hot (file-watch incremental indexing). It is never required — every\n\
  command works identically without it; when it is running, reads are served from\n\
  its hot index and report `_meta.source = \"daemon\"`.\n\
- Opt-in `[daemon] git_sync = true` auto-commits clean item edits (never pushes).\n\
- A running daemon auto-records `clove stats` history points on a timer\n\
  (`[daemon] stats_snapshot_min`, default 60; `0` disables) — replay with\n\
  `clove stats --history`.\n\
- `clove doctor --fix` cleans up a stale daemon socket/pid left by a crash.\n\
\n\
## MCP server (for agents)\n\
\n\
- `clove mcp` runs a Model Context Protocol server over stdio (newline-delimited\n\
  JSON-RPC), exposing clove as native tools so an agent need not shell out:\n\
  `clove_ready`, `clove_blocked`, `clove_list`, `clove_show`, `clove_search`,\n\
  `clove_dep_tree`, `clove_stats` (reads) and `clove_new`, `clove_status`,\n\
  `clove_edit`, `clove_comment`, `clove_dep_add` (writes). Tool results carry the\n\
  same item JSON as the CLI. Configure it as an MCP server with command `clove`\n\
  and arg `mcp`, launched in the repository.\n\
- Writes are coordinated through a running daemon when present (so concurrent\n\
  agents share one writer) and fall back to direct file writes otherwise.\n\
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
