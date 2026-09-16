# Changelog

All notable changes to clove are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-09-16

The first public release: milestones M0–M4, plus the unified read path
(`clove-engine`) and the crates.io plugin registry. **Foundation (M0–M4)** at
the end lists the feature set the release is built on; everything above it is
what changed after that foundation was complete.

### Added

- Plugin discovery via crates.io — no curated manifest; publishing makes a plugin discoverable
- `clove plugin install` / `uninstall` / `update`, including `--git` sources
- `clove-engine`: one read tier (daemon → index → files) behind every read surface
- `clove search` answers the same query on every surface
- `--sort`/`--desc` on every list read, and `sort`/`desc` on the MCP and web surfaces
- Multi-valued `--status`, `--type`, and `--label` filters on every list read
- `--q TEXT` substring filter over id, title, and labels
- `offset` on every list read, alongside the existing `limit`
- `--fields` and `--compact` read-shaping on every list read, CLI and MCP alike
- `?fields=` and `?compact=` on the web API
- `clove_comments` MCP tool, reading an item's comment thread
- `_meta.filters` echoes the parsed filter set on every list read
- `_meta` and the MCP page shape both have published schemas, the latter advertised as an `outputSchema`
- The published crate ships the built web UI, so `cargo install` needs no Node

### Changed

- `clove_ipc::PROTOCOL_VERSION` 4 → 5
- One filter, sort, and limit contract shared by every read surface
- `clove search` is a file scan on every surface — one tier, by design
- `clove show` no longer scans the whole store for `ready`/`blocked_by`
- `clove blocked`'s ordering moved into the daemon
- The bundled web UI pages, and asks the server rather than the browser
- `_meta.source` on the web list endpoints names the tier that answered
- One canonical timestamp spelling everywhere clove writes one
- Daemon-reported errors use clove's standard error codes; `clove daemon` failures exit 7, not 5
- An unrecognized filter, `?sort=`, or `?dir=` on the web API is now a `VALIDATION_ERROR`
- `GET /api/v1/items/:id/comments?limit=` keeps the newest N
- Debug builds emit line tables only; `target/` 2.5 GB → 1.7 GB (source builds only — `[profile.release]` unchanged)

### Performance

- `clove search` resolves each item path once instead of twice

### Fixed

- Silent lost updates in `clove status`/`start`/`close`, `clove set`, and `clove sync github`
- `clove search` could answer from a stale index
- `clove_search` paged over an undefined order
- `clove search` and `clove_search` disagreed on what counts as a hit
- A schema bump left the index empty rather than rebuilt
- An item blocked only by a dangling dependency was reported ready
- `clove blocked` omitted `blocked_by`, the field the list exists for
- `GET /api/v1/board` silently dropped `limit`/`offset`
- `GET /api/v1/stats/history` parsed its window twice and ignored `?offset=`
- `clove stats --since/--limit/--offset` were silently ignored
- `clove stats --history --limit N` reported the truncated count as the total
- A malformed number in a web query string silently meant the default
- `--fields` was silently dropped on the CLI's index and daemon paths
- `--no-index`/`--deep` advertised themselves on commands that ignore them
- `depth: 0` on `clove_dep_tree` and `?depth=0` returned the root alone
- The MCP `clove_dep_tree` payload carries `repeat_ref`
- The web UI's sorter tied on the wrong thing
- An index-backed list with a very large `--offset` failed where the file scan succeeded
- Plugin discovery works behind a TLS-intercepting proxy, and has an overall deadline rather than a per-request one
- The registry cache is keyed by the registry it came from
- Registry errors name the registry actually contacted
- A name that already names its multiplexer is no longer expanded again
- `clove plugin list --all` shows `RUN AS`, flags available updates, prints its sections when empty, and no longer appends a non-item line under `--format jsonl`
- `clove plugin list` compat notes no longer describe enforcement that does not happen
- `clove plugin search` no longer states a negative it could not check
- `clove plugin update` no longer reports success for plugins it never checked
- `clove plugin install --git` no longer claims an install it did not make, and a rejected install no longer claims a rollback
- The install confirmation says what it is and where the binary lands
- The plugin install suite runs on Windows
- Comment authors record the git user rather than `unknown`

### Security

- Update rustls to 0.23.45 for [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285) — TLS 1.3 handshake messages were accepted at the wrong encryption level

### Foundation (M0–M4)

- **Core CLI** (`clove`) — git-native work-item tracker over Markdown + YAML frontmatter, with a dependency graph
- **SQLite index** (`clove-index`) — optional FTS5 search and fast staleness checks
- **Daemon** (`cloved`) — optional background file-watcher keeping the index warm
- **Terminal UI** — `clove tui`, a read-only ratatui browser
- **Web UI** (`clove-web`) — `clove serve` serves a SvelteKit SPA (Kanban / list / detail)
- **MCP server** — `clove mcp` exposes items to AI agents as native MCP tools
- **Claude Code plugin** — this repo is a plugin marketplace; `clove setup` wires it up in one command
- **GitHub sync** — `clove sync github <owner/repo>`, two-way in one pass
- **Interop** — import from tk/beads, export to json/jsonl, and a 3-way git merge driver
- **Analytics** — `clove stats`: counts, ready/blocked, epics, throughput
- **Quality gates** — workspace tests, clippy `-D warnings`, fuzz targets, perf gates, render snapshots, and `cargo deny`, all in CI

### Notes

- Dual-licensed under MIT OR Apache-2.0.
- Release binaries for Linux, macOS (arm64 + x86_64), and Windows are published
  via `.github/workflows/release.yml` on `v*` tags.
