# clove

[![CI](https://github.com/egeapak/clove/actions/workflows/ci.yml/badge.svg)](https://github.com/egeapak/clove/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](rust-toolchain.toml)

A fast, git-native, **dependency-aware** work-item tracker for AI coding agents and humans.

Plain Markdown + YAML-frontmatter files under `.clove/issues/` are the **single
source of truth** — grep-able, diffable, and they travel with the repo. An
optional SQLite index and an optional background daemon add speed and features
but are never required: delete them and nothing is lost. Written in Rust as a
single, dependency-light, cross-platform binary.

## Contents

- [Features](#features)
- [Install](#install)
- [Quick start](#quick-start)
- [Accelerators: index & daemon](#accelerators-index--daemon)
- [Browse: terminal & web UI](#browse-terminal--web-ui)
- [Interop & GitHub sync](#interop--github-sync)
- [AI agents: MCP server & Claude Code plugin](#ai-agents-mcp-server--claude-code-plugin)
- [Build & test](#build--test)
- [Workspace layout](#workspace-layout)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)

## Features

- **Git-native, plain-text store** — every item is a Markdown file with
  YAML frontmatter under `.clove/issues/`. No database of record; it diffs,
  greps, and merges like code, and a 3-way merge driver resolves conflicts.
- **Dependency-aware** — a cycle-validated hard-dependency graph with
  `ready` / `blocked` queries and a `cargo tree`-style `dep tree` view.
- **One write path, many surfaces** — CLI, web, MCP, daemon, and the TUI form
  all funnel through a single validated mutation path, so behavior never
  diverges between them.
- **Agent-first output** — every command speaks a stable
  `{ v, ok, data, _meta }` JSON envelope with documented exit codes;
  `clove agent-doc` describes the whole agent-facing surface.
- **Optional accelerators, never required** — an FTS5 SQLite index for fast
  search/staleness and a `cloved` daemon that keeps the index + graph hot and
  serves reads over IPC. Delete them and nothing is lost.
- **Analytics** — `clove stats` (counts, ready/blocked, epics, throughput) with
  recorded history snapshots.
- **Two UIs** — a `ratatui` terminal browser (`clove tui`) and an embedded
  SvelteKit web UI (`clove serve`: Kanban / list / detail / timeline, live
  file-watch updates, no Node needed at runtime).
- **Two-way GitHub sync** — `clove sync github` reconciles issues *and* comments
  in both directions in a single pass, with policy-based conflict resolution.
- **AI-native** — a built-in MCP server (`clove mcp`) and a Claude Code plugin
  expose items to agents as native tools.
- **Cross-platform** — a single stripped, LTO'd binary for Linux, macOS
  (arm64 + x86_64), and Windows.

## Install

```sh
cargo install --locked --git https://github.com/egeapak/clove clove-cli cloved
clove version   # the installed command is `clove` (crate: clove-cli)

# ...then add integrations as plugins, only the ones you want:
cargo install --locked --git https://github.com/egeapak/clove clove-sync-github   # GitHub sync (~3.5 MB of TLS/HTTP)
cargo install --locked --git https://github.com/egeapak/clove clove-import-tk clove-import-beads   # import from tk / Beads
```

This installs the `clove` CLI and the optional `cloved` daemon onto your `PATH`.
A Rust (stable) toolchain compiles them; no Node is required (the web UI embeds a
placeholder unless built with Node — see
[`crates/clove-web/web/README.md`](crates/clove-web/web/README.md)).

**Integrations are cargo-style plugins.** The core `clove` binary carries only
its own native surface; every foreign-tracker integration is a
separately-installed binary resolved on your `PATH` (or next to `clove`), exactly
as `cargo nextest` runs `cargo-nextest`: `clove sync github` →
**`clove-sync-github`**, `clove import tk`/`beads` →
**`clove-import-tk`**/**`clove-import-beads`**. (`clove export json`/`jsonl` stays
built-in — that's clove's own serialization, not a foreign integration.) Without
the plugin, the command prints a clean `unknown <mux> provider; install
clove-<mux>-<provider>` error (exit 4); installing it lights the command up with
no core rebuild. The daemon's periodic sync spawns the same `clove sync github`,
so it needs the plugin too. See [`docs/PLUGIN_SYSTEM.md`](docs/PLUGIN_SYSTEM.md) for
the dispatch/discovery/env contract.

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

Every command supports `--format json|jsonl` with a stable
`{ v, ok, data, _meta }` envelope and documented exit codes — see
`clove agent-doc` for the agent-facing surface.

## Accelerators: index & daemon

Both are optional. Reads work directly from the file store without them.

```sh
clove reindex                 # build/refresh the SQLite index (.clove/index.db, gitignored)
clove daemon start            # watch files, keep the index + graph hot, serve reads over IPC
clove stats --snapshot        # record an analytics history point
clove stats --history         # replay recorded snapshots (a running daemon also auto-records)
```

## Browse: terminal & web UI

```sh
clove tui                     # terminal browser + add/edit form (master-detail, tabs, filters)
clove serve                   # serve the web UI on http://127.0.0.1:7373 (loopback)
clove serve --open            # …and open it in the browser
```

`clove serve` runs an HTTP/WebSocket server with a Kanban board, a filterable
list, an item detail view (Markdown body, dependency tree, comments, inline
edits), and a timeline — with live updates via a file-watcher. The SPA is built
into the binary (no Node needed to run). When a daemon is running it serves the
web UI itself (port `7373` by default), and `clove serve` hands off to it instead
of starting a second server. The web API mirrors the CLI under `/api/v1` with the
same JSON envelope and exit-code semantics.

## Interop & GitHub sync

```sh
clove export json|jsonl                 # export all items to a file / stdout (built-in)
clove import json|jsonl <file>          # restore that export, preserving ids (built-in round-trip)
clove import tk|beads <src>             # import from a foreign tracker (clove-import-tk/beads plugin)
clove sync github <owner/repo>          # two-way GitHub sync (clove-sync-github plugin)
clove init --merge-driver               # install the 3-way git merge driver for item files
```

**Native round-trip.** `clove export json` (or `jsonl`) and `clove import json`
(or `jsonl`) are inverse **built-ins** — clove's own serialization is core, only
foreign trackers are plugins. `import json|jsonl <file> [--dry-run] [--overwrite]`
restores items **verbatim, preserving their ids**, so `export → import` into
another repo reproduces them exactly (a backup/restore + snapshot-transfer path;
existing ids are skipped unless `--overwrite`). The export is versioned for future
migrations — each item carries its `schema`, and `export json` stamps
`_meta.clove_export = { format, item_schema }`; importing an export from a *newer*
clove is rejected with an upgrade hint. (Comments aren't part of the export — they
travel as git-tracked sidecar files.)

**Two-way GitHub sync.** `clove sync github <owner/repo>` reconciles both
directions in a single pass: it pulls remote issue changes *and* pushes local
ones, and when the same issue changed on both sides since the last sync it
resolves the conflict by policy (`--prefer newer|local|remote|manual`; default
*newest edit wins*, every conflict reported). Issue comments sync bidirectionally
too (`--no-comments` to skip). `--dry-run` plans without touching either side. A
per-repo last-sync clock lives under `.clove/sync/` (git-ignored), and a running
daemon can run the sync on a timer (`[daemon] github_sync_interval_min` +
`github_sync_repo`). Auth via `GITHUB_TOKEN` or the `gh` CLI. Requires the
**`clove-sync-github` plugin** (`cargo install clove-sync-github`; see
[Install](#install)) — the pre-built release bundle includes it.

## AI agents: MCP server & Claude Code plugin

`clove mcp` runs an MCP server (stdio) that exposes clove's items to AI agents as
native tools — list/search/show, dependency tree and ready/blocked, stats, and
create/edit/transition plus dep/parent/comment writes. Reads compute from the
file store; writes prefer the auto-started `cloved` daemon (and fall back to
direct file access), so concurrent agents stay coherent.

For **any MCP client** (not just Claude Code), register `clove mcp` as a stdio
server with command `clove` and args `["mcp"]`, run from inside the repository.
The server starts even before `clove init`; until the repo exists its tools
return a "no clove repository" error rather than failing to launch.

For **Claude Code**, this repo is also a plugin marketplace — install it with:

```sh
/plugin marketplace add egeapak/clove
/plugin install clove@clove
```

The plugin wires up the `clove mcp` server automatically (tools surface as
`mcp__plugin_clove_tracker__*`); it needs `clove` (and, for coordinated writes,
`cloved`) on your `PATH` and an initialized repo (`clove init`). See
[`.claude-plugin/README.md`](.claude-plugin/README.md) for details.

## Build & test

```sh
cargo build --release                    # lean binaries: clove (CLI), cloved (daemon)
cargo build --release -p clove-sync-github   # the GitHub sync plugin (adds octocrab/TLS)
cargo test --workspace --all-features    # unit + integration + doctests (incl. github sync)
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Frontend checks (only if you touch the web UI):

```sh
cd crates/clove-web/web && npm run check && npm run test   # svelte-check + vitest
```

## Workspace layout

| Path | What |
|------|------|
| `crates/clove-types` | pure shared data types (model/id/error/validation + the create/edit request types) |
| `crates/clove-core` | file store, dependency-graph engine, high-level ops (pure; no SQLite) |
| `crates/clove-index` | optional SQLite index (FTS5, staleness, incremental derived state, stats history) |
| `crates/clove` | the `clove` CLI (crate `clove-cli`) |
| `crates/cloved` | the optional `cloved` daemon (file-watch, IPC, optional git sync, web serving) |
| `crates/clove-import` | built-in `json`/`jsonl` export, the 3-way merge driver, the `tk`/`beads` importer logic (reused by the import plugins), and the pure GitHub field-mapping + reconciliation (its `github` feature — the octocrab network layer — is enabled only by the `clove-sync-github` plugin) |
| `crates/clove-plugin` | support crate for cargo-style subcommand plugins (typed `PluginContext` from the `CLOVE_*` env contract + envelope/exit-code harness) |
| `crates/clove-sync-github` | the `clove-sync-github` plugin: two-way GitHub Issues sync, installed separately (`cargo install clove-sync-github`) so the core carries no octocrab weight — `clove sync github <owner/repo>` resolves it |
| `crates/clove-import-tk` / `crates/clove-import-beads` | the `clove-import-tk` / `clove-import-beads` plugins: import from a `tk` `.tickets/` dir or a Beads `issues.jsonl`, installed separately (`cargo install clove-import-tk`) — `clove import tk\|beads` resolves them |
| `crates/clove-ipc` | CLI ↔ daemon wire protocol (`tarpc`) |
| `crates/clove-mcp` | the MCP server surface (`clove mcp`) |
| `crates/clove-tui` | terminal browser + add/edit form (`clove tui`, ratatui) |
| `crates/clove-web` | web UI server + embedded SvelteKit SPA (`clove serve`); see `web/README.md` |

## Documentation

| Doc | What |
|-----|------|
| [`docs/DESIGN.md`](docs/DESIGN.md) | the authoritative, implementation-ready spec (read this first) |
| [`docs/RELEASE.md`](docs/RELEASE.md) | the release runbook (crates.io + pre-built binaries) |
| [`docs/PLUGIN_SYSTEM.md`](docs/PLUGIN_SYSTEM.md) | the cargo-style plugin system: dispatch, discovery, and the host↔plugin contract |
| [`docs/PLUGIN_REGISTRY.md`](docs/PLUGIN_REGISTRY.md) | plugin list/install/`--help` discovery and the registry manifest schema |
| [`docs/json-schema/`](docs/json-schema/) | JSON Schemas for the stable `--format json` output |
| [`CHANGELOG.md`](CHANGELOG.md) | release notes |

## Contributing

Contributions are welcome. Before opening a PR, make sure the full quality gate
is clean:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

When adding an editable field or a synced field, add it in **one** place (the
unified write path / the GitHub mapping) rather than per surface — see
[`CLAUDE.md`](CLAUDE.md) for the conventions the codebase relies on.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual-licensed as above, without any additional terms or conditions.
