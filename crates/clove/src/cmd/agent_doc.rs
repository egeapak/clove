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
`_meta` is described by the published JSON Schemas under\n\
`docs/json-schema/v1/` (`item-list.json` for every list command,\n\
`comment-list.json`, `stats.json`, and `{{ \"warnings\": [] }}` for the\n\
single-object responses), so a key like `_meta.limit` — where `0` means\n\
*unlimited* — is readable from the schema rather than by experiment. Those\n\
schemas set `additionalProperties: false`: a new `_meta` key is documented\n\
before it ships.\n\
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
| 5 | i/o or missing `.clove/`; also `REGISTRY_ERROR` — the plugin registry, `cargo` or `git` could not be reached |\n\
| 6 | index error |\n\
| 7 | daemon error |\n\
\n\
## Commands\n\
\n\
- `clove init [--prefix STR] [--merge-driver]` — create `.clove/`.\n\
- `clove setup [--global] [--dry-run]` — register the `clove mcp` server (+ tool\n\
  permissions) with Claude Code and write `CLOVE.md` agent directives.\n\
- `clove new <title> [--type T] [-p N] [-l LABEL]... [--dep ID]... [--parent ID] [-a WHO] [-b TEXT]`\n\
- `clove show <id> [--fields LIST] [--compact]` — one item. `ready`/`blocked_by` are always computed (`-v` is accepted but no longer needed for them).\n\
- `clove edit <id> [--field KEY=VALUE]...` / `clove set <id> KEY=VALUE...`\n\
- `clove status <id> <open|in_progress|closed>` (aliases `start`, `close`).\n\
- `clove label <id> <add|rm> <label>`, `clove assign <id> <who|--clear>`, `clove priority <id> <0-4>`.\n\
- `clove dep add <id> <dep-id>` / `dep rm` / `dep tree <id> [--depth N|--full] [--flat]` / `dep cycle [--fail-on-cycle]`.\n\
- `clove ready` / `clove blocked` — work queues (filters: `--status --type --label --assignee --priority --q`; also `--sort --desc --limit --offset --fields --compact`).\n\
- `clove ls` / `clove query [--filter JSON]` — list/query (`--sort`, `--desc`, `--fields`, `--limit`, `--offset`).\n\
- `ls`, `ready` and `blocked` take the same repeatable filter flags. `--status`, `--type` and `--priority` repeat as **any of** (`--status open --status in_progress`); `--label` repeats as **all of** (`--label area:core --label area:ios` needs both); `--assignee` is an exact match; `--q TEXT` is a case-insensitive substring over the **id, title, and labels** only — it never reads the body, so it is a filter, not a search. Omitting a filter does not constrain. `_meta.filters` echoes the parsed set, canonicalized (`--status started` comes back as `in_progress`). `clove query` takes the same filter *set* but only as JSON (`--filter '{{\"status\":[\"open\",\"in_progress\"]}}'`), not as flags. An unknown filter value is a `VALIDATION_ERROR` (exit 4) rather than an empty list (`--priority 9`, `--status bogus`); a value of the wrong type is still an argument-parse error (exit 1), so `--priority abc` never reaches clove.\n\
- `clove query --filter JSON` takes the same filters as JSON, each accepting one value or a list: `{{\"status\": [\"open\", \"in_progress\"], \"label\": [\"area:core\", \"area:ios\"], \"priority\": [0, 1], \"q\": \"login\"}}`.\n\
- Every list read (`ls`, `query`, `ready`, `blocked`, `search`) takes the same `--limit`/`--offset`: no flag caps at 100, `--limit 0` returns everything, `_meta.total` is always the full match count and `_meta.limit` echoes the one in force.\n\
- They also take the same `--sort <rank|priority|created|updated|id|status|type>` and `--desc`. The default is `rank` — priority, then dependency order, then id — except on `search`, whose default is relevance (title hits, then labels, then body); naming a field there replaces that ranking entirely. `status` sorts open → in_progress → closed and `type` sorts bug → feature → chore → docs → epic (declared order, not alphabetical). Every order ends in an id tiebreak, so paging with `--offset` is stable. `_meta.sort`/`_meta.dir` echo what was applied. Prefer `--sort updated --desc --limit N` over pulling the store and sorting yourself.\n\
- `clove comment <id> <message>` / `clove comments <id> [--limit N] [--skip-newest N]` — `--limit` keeps the *newest* N (default 100, `--limit 0` for all); `--skip-newest` pages back into older ones. `_meta.total` is the full thread length.\n\
- `clove search <text> [--sort FIELD] [--desc] [--limit N] [--offset N] [--fields LIST] [--compact]` — searches **titles, labels, and bodies**, relevance-ranked by default (title hits, then labels, then body).\n\
- Search matches a **case-insensitive substring**, not whole words: `clove search core` finds a body reading `the corepart word` and a label `area:core` alike, and `clove search icode` finds the label `ünicode-tag`. Case folding is full Unicode, so `Ünicode` finds `ünicode-tag`. The text is a literal — there is no query language, so `clove search '\"a b\" OR c*'` looks for that exact character sequence, and quoting/wildcards buy you nothing.\n\
- `clove search` always scans the item files: it has no index or daemon fast path, so `_meta.source` is always `files`, `--no-index` changes nothing, and the same query gives the same ids whether or not `.clove/index.db` exists or a daemon is running. It reads every body, so prefer `--q` on `ls`/`ready` when id/title/labels are enough.\n\
- `clove stats [--top N] [--no-epics] [--snapshot] [--history [--since RFC3339] [--limit N] [--offset N]]` — work-item analytics (counts by status/type/priority/assignee/label, ready/blocked, cycles, epic rollups, throughput) plus daemon/index telemetry. `--snapshot` persists to the index's durable history (`.clove/index.db`); `--history` replays the series.\n\
- `--since`/`--limit`/`--offset` window the **history series** and require `--history`: a live report is a single object with nothing to page, so passing one without it is a usage error (exit 1) rather than a flag that is silently dropped.\n\
- `clove reindex` — rebuild the SQLite index. `clove doctor [--fix] [--strict]` — health check.\n\
- `--no-index` (force a file scan) and `--deep` (thorough index staleness check) are global flags acted on only by the commands that choose a read tier — `ls`, `ready`, `blocked`, `query`, `stats`, `doctor`, `dep`, `serve` — and by plugins (`$CLOVE_NO_INDEX`/`$CLOVE_DEEP`). They are accepted and inert everywhere else, which their `--help` says; `--no-index` never changes an *answer*, only which tier produced it.\n\
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
  `clove-import-tk` plugin; `clove plugin install import-tk`).\n\
- `clove import beads <issues.jsonl> [--dry-run]` — import a Beads JSONL export\n\
  (needs the `clove-import-beads` plugin).\n\
- `clove sync github <owner/repo> [--dry-run] [--prefer P] [--no-comments]` —\n\
  two-way GitHub sync (pull + push + comments in one pass; conflict policy\n\
  `newer|local|remote|manual`). Needs the `clove-sync-github` plugin\n\
  (`clove plugin install sync-github`) + a token via `GITHUB_TOKEN` or\n\
  `gh auth token`; without the plugin it exits 4 with an install hint. The\n\
  single GitHub path (replaces the old\n\
  one-way `import github` / `export github`).\n\
- File imports are idempotent on `external_ref`: re-running skips already-imported\n\
  items. `--dry-run` reports `{{ would_create, would_skip, conflicts }}` and\n\
  writes nothing.\n\
- `clove plugin list [--all] [--refresh]` — installed plugins (name, version,\n\
  what they run as, host<->plugin compat `status`). Plain `list` is a pure\n\
  filesystem walk and never uses the network; `--all` additionally lists plugins\n\
  published to crates.io (discovered as the reverse dependencies of\n\
  `clove-plugin`), cached for 24h. If discovery fails or the registry is not\n\
  published yet, the installed list still prints and the reason appears in\n\
  `_meta.warnings` and the command still exits 0 — a registry outage is\n\
  never an error, so do not treat a missing Available section as a failure.\n\
- `clove plugin install <name> [--yes] [--force] [--strict] [--allow-yanked]` —\n\
  build and install a published plugin. This compiles and runs third-party code,\n\
  so it always requires an explicit decision: a non-interactive run (no TTY, or\n\
  `--format json`) **refuses** unless `--yes` is given. A bare provider name is\n\
  resolved by constructing the candidate crate names; if several exist the\n\
  command refuses and asks for the exact crate rather than guessing.\n\
- `clove plugin install --git <url> [--tag T | --rev R | --branch B] [--package P]`\n\
  — install from any git forge (plain `git`, not `gh`). The repository is cloned\n\
  shallowly and inspected: a package counts as a plugin only if it depends on\n\
  `clove-plugin`, builds a `clove-*` binary, AND is publishable. Several\n\
  candidates means `--package` is required rather than clove guessing. Without\n\
  `--tag`/`--rev` it warns that the default branch moves.\n\
- `clove plugin uninstall <name>` — remove a plugin clove installed. Needs no\n\
  network: the package is resolved from cargo's own bookkeeping. A plugin\n\
  installed by something else (e.g. `cargo install` into ~/.cargo/bin) is\n\
  reported as unmanaged rather than failing obscurely.\n\
- `clove plugin update [<name>] [--all] [--yes]` — re-resolve installed plugins.\n\
  Shows each old -> new version before changing anything, and only re-resolves\n\
  crates.io installs (a git-sourced plugin is left alone, not silently swapped\n\
  for a same-named crate — reinstall it with `plugin install --git <url> --force`\n\
  to update it). The payload separates `checked` from `skipped` for that reason,\n\
  and moves only to a strictly greater STABLE version: a pre-release is reported\n\
  and held, and a registry offering an older version than the installed one is\n\
  never treated as an upgrade. Anything refused (yanked, no longer a\n\
  `clove-plugin` dependent, failed build, failed rollback) is reported in\n\
  `_meta.warnings`, so exit 0 with an empty `updated` is not by itself a\n\
  clean bill of health.\n\
- `clove plugin search <text> [--refresh]` — filter published plugins by\n\
  name/description. When that filter matches nothing, the candidate crate\n\
  names are constructed and probed directly, so a published plugin is found\n\
  even when the cached set is stale or discovery is unavailable.\n\
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
  `clove_comments`, `clove_dep_tree`, `clove_stats` (reads) and `clove_new`, `clove_status`,\n\
  `clove_edit`, `clove_comment`, `clove_dep_add`, `clove_dep_remove`, `clove_set_parent`\n\
  (writes). Configure it as an MCP server with command `clove` and arg `mcp`,\n\
  launched in the repository.\n\
- Tool results are the same item JSON as the CLI's, with two differences worth\n\
  knowing: reads are **compacted by default** (null and empty-list keys, plus\n\
  `schema`, are omitted — pass `compact: false` for the full shape), and the\n\
  read tools' default `limit` is **50**, not the CLI's 100. `limit: 0` is\n\
  unlimited on both, and every list result carries `total`/`returned`/`limit`.\n\
- `clove_list`, `clove_ready`, `clove_blocked`, `clove_search` and\n\
  `clove_comments` advertise an **`outputSchema`** in `tools/list` (published as\n\
  `docs/json-schema/v1/mcp-item-page.json` / `mcp-comment-page.json`), so the\n\
  page shape — `{{total, returned, offset, limit, sort, dir, filters?, source,\n\
  items}}`, with `skip_newest` in place of `offset` on comments — can be read\n\
  rather than discovered. `structuredContent` validates against it; the identical\n\
  JSON is also in `content[0].text` for clients that do not read structured\n\
  results, so parse whichever you prefer, not both.\n\
- `fields` and `compact` are accepted by every read tool, and by `clove ls`,\n\
  `ready`, `blocked`, `query`, `search`, and `show` as `--fields`/`--compact`.\n\
  Use them: a two-field projection cuts a list result by roughly 80%.\n\
- `sort` and `desc` are accepted by `clove_ready`, `clove_blocked`, `clove_list`,\n\
  and `clove_search`, with the same vocabulary and defaults as the CLI's\n\
  `--sort`/`--desc`. `{{\"sort\": \"updated\", \"desc\": true, \"limit\": 10}}` answers\n\
  \"what changed most recently\" in one call instead of pulling the store.\n\
- `status`, `type`, `label` and `priority` on `clove_ready`/`clove_blocked`/\n\
  `clove_list` each accept **one value or a list**, with the CLI's meaning:\n\
  status/type/priority are any-of, labels are all-of. `q` is there too.\n\
  `{{\"status\": [\"open\", \"in_progress\"], \"label\": [\"area:core\", \"area:ios\"]}}`\n\
  is one call for a question that used to need several. The result object\n\
  carries a `filters` key echoing the parsed set.\n\
- `q` (on the filtering tools) and `clove_search` are different questions, not\n\
  two spellings of one. `q` is a filter over **id, title, and labels**, composes\n\
  with the other filters, and never reads a body; `clove_search` reads **titles,\n\
  labels, and bodies**, ranks the hits, and takes no field filters. Both match a\n\
  case-insensitive Unicode *substring*. Reach for `q` to narrow a list and\n\
  `clove_search` to find where something was written.\n\
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
