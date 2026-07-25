# Changelog

All notable changes to clove are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **MCP read tools take `fields` and `compact`.** `fields` projects each item to
  a named subset (`{"fields": ["id", "title"]}`); `compact` drops keys that are
  null or an empty list. Compaction is on by default — measured on a 12-item
  repo, a `clove_list` result went from 4595 to 2555 bytes (-44%), and to 764
  with a two-field projection (-83%). `compact: false` restores the previous
  shape exactly. A definite `false` or `0` is never dropped: `ready: false` is an
  answer, not an absence. Absent keys are v1-legal (only
  `id`/`title`/`status`/`type`/`priority`/`created`/`updated` are `required` in
  `item.json`), and the index-backed `clove ls` path has always returned a
  reduced shape. The CLI, web API, `export json`, and GitHub sync fingerprints
  are unaffected — shaping is applied at the MCP boundary only.
- **`clove_comments`** reads an item's comment thread. `clove_comment` could
  write comments that no MCP tool could read back; `clove_show` reported only a
  count.

### Changed

- **`clove show` no longer scans the whole store.** `ready`/`blocked_by` are
  computed from the item's own dependency closure, falling back to the
  whole-store graph only when that closure is very large. Measured 57.3ms ->
  0.06ms at 10k items, and flat rather than linear in store size. Affects
  `clove show`, the `clove_show` MCP tool, and the daemon's `show` RPC.
- **Daemon-reported errors now use clove's standard error codes.** `cloved`
  emitted a private code set (`not_found`, `self_loop`, `cycle`,
  `already_exists`, `invalid_field`, `op_failed`) that neither matched the
  documented `error.code` spellings nor covered them — every unrecognized
  failure collapsed into `op_failed`, merging distinct classes (I/O, exit 5,
  with parse failures, exit 4), so no client could recover the right exit code.
  It now emits the same `code`/`exit` pair as the CLI and web API, so a failure
  reported by the daemon is indistinguishable from the same failure raised
  locally.

  *User-visible:* MCP tool errors that route through the daemon now read
  `ITEM_NOT_FOUND: no item with id …` rather than `not_found: …` (likewise
  `CYCLE_DETECTED`, `VALIDATION_ERROR`, `SELF_LOOP`, `ALREADY_EXISTS`). Scripts
  matching the old lowercase strings need updating; the new spellings are the
  documented ones (DESIGN §7.3). `clove daemon` failures now report
  `daemon transport error: …` rather than `daemon protocol error: …`.

  The IPC wire is otherwise unchanged: the error reply gains a self-describing
  `exit` field that defaults compatibly in both directions, so there is no
  protocol bump and a mixed-version `clove`/`cloved` pair keeps working.

### Fixed

- **Silent lost updates in `clove status`/`start`/`close`, `clove set`, and
  `clove edit --field`.** These read the item without the store write lock and
  only took it for the write, so a concurrent writer — the web UI, an MCP agent,
  or the daemon — could commit in between and have its change silently
  overwritten, with no error. They now perform the whole read-modify-write under
  one lock via `ItemStore::update_with`, matching `clove_core::ops`. Only
  concurrent writers were affected; a single user running one command at a time
  never was. Each command also samples the clock once now, so a close writes the
  same timestamp to `closed` and `updated`.

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
