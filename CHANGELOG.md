# Changelog

All notable changes to clove are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-20

The initial feature set (milestones M0–M4). First tagged public release.

### Added

- **Core CLI (`clove`)** — git-native work-item tracker over Markdown +
  YAML-frontmatter files under `.clove/issues/` as the single source of truth.
  Create/edit/transition items, labels, assignees, priorities, comments, and a
  cycle-validated dependency graph (`dep add/remove`, `dep tree`, `ready`,
  `blocked`). Stable `{ v, ok, data, _meta }` JSON envelope on every command with
  documented exit codes; `clove agent-doc` describes the agent-facing surface.
- **SQLite index (`clove-index`)** — optional FTS5 search, fast staleness
  checks, incremental derived state, and analytics history. Never required:
  delete `.clove/index.db` and nothing is lost.
- **Daemon (`cloved`)** — optional background file-watcher that keeps the index +
  dependency graph hot, serves reads over IPC, records analytics snapshots, and
  can run GitHub sync on a timer.
- **Analytics** — `clove stats` (counts, ready/blocked, epics, throughput) with
  recorded history snapshots.
- **Terminal UI** — `clove tui`, a read-only ratatui browser (master-detail,
  tabs, filters).
- **Web UI (`clove-web`)** — `clove serve` serves a SvelteKit SPA (Kanban / list
  / detail / timeline) with live file-watch updates; loopback-only by default.
  The SPA is embedded in the binary (no Node needed at runtime).
- **MCP server** — `clove mcp` exposes items to AI agents as native MCP tools
  over stdio (read: list/ready/blocked/show/search/dep_tree/stats; write:
  new/edit/status/comment/dep_add/dep_remove/set_parent). Writes prefer the
  auto-started `cloved` daemon and fall back to direct file access.
- **Claude Code plugin** — this repo is a plugin marketplace
  (`.claude-plugin/`); install with `/plugin marketplace add egeapak/clove` and
  `/plugin install clove@clove`. The MCP server ships instructions that nudge
  agents to use clove as the source of truth for work items by default, and a
  root `CLOVE.md` provides `@CLOVE.md` standing directives for projects.
- **`clove setup`** — one command to wire clove into Claude Code: registers the
  `clove mcp` server (and its tool permissions) in `settings.json`, writes
  `CLOVE.md`, and adds an `@CLOVE.md` import to `CLAUDE.md`. Supports `--global`
  vs project scope and `--dry-run`; idempotent.
- **GitHub sync** — `clove sync github <owner/repo>`, two-way (pull + push in one
  pass) with policy-based conflict resolution and bidirectional comments.
- **Interop** — import from tk/beads, export to json/jsonl, and a 3-way git merge
  driver (`clove init --merge-driver`).
- **Quality gates** — workspace tests, clippy `-D warnings`, fuzz targets, perf
  gates, render snapshots, and `cargo deny`, all in CI.

### Notes

- Dual-licensed under MIT OR Apache-2.0.
- Release binaries for Linux, macOS (arm64 + x86_64), and Windows are published
  via `.github/workflows/release.yml` on `v*` tags.
