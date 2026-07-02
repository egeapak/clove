# clove

A fast, git-native, **dependency-aware** work-item tracker for AI coding agents and humans.

Plain Markdown + YAML-frontmatter files under `.clove/issues/` are the **single source
of truth** — grep-able, diffable, and they travel with the repo. An optional SQLite
index and an optional background daemon add speed and features but are never required:
delete them and nothing is lost. Written in Rust as a single cross-platform binary.

## Status

M0–M3 complete and gated; M4 in progress: `clove stats` + analytics history, an
exact-incremental index/daemon graph, the `clove tui` terminal browser/editor,
and the **`clove serve` web UI** (Kanban / list / detail / timeline). See
`HANDOFF.md` for the current state and `docs/` for the full design
(`docs/M4_WEB_UI_PLAN.md` for the web UI).

## Build & test

```sh
cargo build --release          # binaries: clove (CLI), cloved (daemon)
cargo test --workspace         # unit + integration + doctests
cargo clippy --workspace --all-targets -- -D warnings
```

## Quick start

```sh
clove init                                   # create .clove/ in the repo
clove new "Wire up auth" --type feature -p 1 # create an item
clove dep add <id> <dep-id>                  # declare a hard dependency
clove ready                                  # items with all deps closed
clove blocked                                # items waiting on open deps
clove dep tree <id>                          # cargo-tree-style dependency view
clove ls --status open --label area:core     # filter/list
clove stats                                  # analytics (counts, ready/blocked, epics, throughput)
clove search "login"                         # full-text search
```

Every command supports `--format json|jsonl` with a stable `{ v, ok, data, _meta }`
envelope and documented exit codes — see `clove agent-doc` for the agent-facing surface.

### Optional accelerators

```sh
clove reindex                 # build/refresh the SQLite index (.clove/index.db, gitignored)
clove daemon start            # watch files, keep the index + graph hot, serve reads over IPC
clove stats --snapshot        # record an analytics history point
clove stats --history         # replay recorded snapshots (a running daemon also auto-records)
```

### Browse: terminal & web UI

```sh
clove tui                     # terminal browser + add/edit form (master-detail, tabs, filters)
clove serve                   # serve the web UI on http://127.0.0.1:7373 (loopback)
clove serve --open            # …and open it in the browser
```

`clove serve` runs an HTTP/WebSocket server with a Kanban board, a filterable
list, an item detail view (Markdown body, dependency tree, comments, inline
edits), and a timeline — with live updates via a file-watcher. The SPA is built
to a single binary (no Node needed to run). When a daemon is running it serves the
web UI itself (port `7373` by default), and `clove serve` hands off to it instead
of starting a second server. The web API mirrors the CLI under `/api/v1` with the
same JSON envelope and exit-code semantics.

### Interop

```sh
clove import tk|beads <src>             # import from a file-based tracker
clove export json|jsonl                 # export to a file / stdout
clove sync github <owner/repo>          # two-way GitHub sync (pull + push, one pass)
clove init --merge-driver               # install the 3-way git merge driver for item files
```

**Two-way GitHub sync.** `clove sync github <owner/repo>` reconciles both
directions in a single pass: it pulls remote issue changes *and* pushes local
ones, and when the same issue changed on both sides since the last sync it
resolves the conflict by policy (`--prefer newer|local|remote|manual`; default
*newest edit wins*, every conflict reported). Issue comments sync bidirectionally
too (`--no-comments` to skip). `--dry-run` plans without touching either side. A
per-repo last-sync clock lives under `.clove/sync/` (git-ignored), and a running
daemon can run the sync on a timer (`[daemon] github_sync_interval_min` +
`github_sync_repo`). Auth via `GITHUB_TOKEN` or the `gh` CLI.

### AI agents: MCP server & Claude Code plugin

`clove mcp` runs an MCP server (stdio) that exposes clove's items to AI agents as
native tools — list/search/show, dependency tree and ready/blocked, stats, and
create/edit/transition plus dep/parent/comment writes. Reads compute from the
file store; writes prefer the auto-started `cloved` daemon (and fall back to
direct file access), so concurrent agents stay coherent.

For **Claude Code**, this repo is also a plugin marketplace — install it with:

```sh
/plugin marketplace add egeapak/clove
/plugin install clove@clove
```

The plugin wires up the `clove mcp` server automatically (tools surface as
`mcp__plugin_clove_tracker__*`); it needs `clove` (and, for coordinated writes,
`cloved`) on your `PATH` and an initialized repo (`clove init`). See
[`.claude-plugin/README.md`](.claude-plugin/README.md) for details.

## Layout

| Path | What |
|------|------|
| `crates/clove-core` | model, file store, dependency-graph engine, IDs (pure; no SQLite) |
| `crates/clove-index` | optional SQLite index (FTS5, staleness, incremental derived state, stats history) |
| `crates/clove` | the `clove` CLI |
| `crates/cloved` | the optional `cloved` daemon (file-watch, IPC, optional git sync, web serving) |
| `crates/clove-import` | import/export + merge driver |
| `crates/clove-ipc` | CLI↔daemon wire protocol |
| `crates/clove-tui` | terminal browser + add/edit form (`clove tui`, ratatui) |
| `crates/clove-web` | web UI server + embedded SvelteKit SPA (`clove serve`); see `web/README.md` |
| `docs/DESIGN.md` | authoritative, implementation-ready spec |
| `docs/IMPLEMENTATION_PLAN.md` | phased M0–M4 task plan |
| `docs/M4_WEB_UI_PLAN.md` | web UI plan + status; `docs/web-ui-mockups/` the design themes |
| `docs/*_ACCEPTANCE_GATES.md` | per-milestone acceptance gates |

## License

MIT OR Apache-2.0.
