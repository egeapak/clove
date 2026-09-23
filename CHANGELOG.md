# Changelog

All notable changes to clove are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-09-24

### Added

- One per-user daemon serves every project, over one socket and one web port (`/p/<slug>/`, with a project picker)
- A per-project `.clove/daemon.token` scopes automated daemon calls (CLI, MCP) to their own project; clove records the tokens it issued under `<clove home>/daemon-tokens/`
- `clove daemon stop --all`; `clove daemon status` lists each served project and its file-watcher state
- The daemon logs to `hub.log` in its runtime directory
- `clove doctor` reports a clove 0.1.0 daemon, an unresponsive daemon, a git-tracked daemon token, and a clove-home mismatch

### Changed

- The daemon socket lives in a per-user runtime directory (`$CLOVE_RUNTIME_DIR`, `$XDG_RUNTIME_DIR/clove`, or `$TMPDIR/clove-<uid>`), not in `.clove/`
- `cloved run` takes no `--clove-dir`: the daemon starts bare and loads projects on demand
- `clove daemon stop` stops serving this project; the daemon exits once it serves none
- `clove daemon` JSON: `start`'s `pid` is a number, `status`'s `running` means "serving this project" (plus `hub`, `log`, `warnings`), and `stop` adds `stopping`, `hub_stopped`, `legacy`, and `restarted_by_another_client`
- The daemon's web UI and API live under `/p/<slug>/`; bare `/api/v1/…` redirects only while one project is loaded
- Starting the daemon for a project (`daemon start`, MCP, `serve`) adds `daemon.token` to `.clove/.gitignore`
- The daemon runs from its runtime directory with a minimal environment; its GitHub sync takes the token from `gh auth token`
- `clove serve` defaults to the configured `[web] port`, falls back to a free port, and hands off to a running daemon; an explicit `--port` is always honored
- `clove reindex` waits for a rebuild already in progress instead of failing
- A symlinked `.clove/issues/` (or directory below it) is refused for reads and writes
- Daemon IPC protocol 8: clove 0.1.0 clients and daemons fall back to direct file access

### Fixed

- `clove … | head` exits quietly instead of aborting on a closed pipe
- The daemon runs in repositories nested deeper than the platform's socket-path limit
- `clove serve` in a second project no longer fails on the shared port or blames `[web] enabled = false`
- `clove sync github` names comment directions: `comments 0 pulled / 6 pushed`
- `clove doctor --fix` no longer deletes a live but slow daemon's socket and pid files, and `clove daemon stop` no longer reports such a daemon as not running

### Security

- Files under `.clove/` are never read or written through a planted symlink (daemon lock, index, sync state, items, `doctor --fix`, `init`)
- Daemon-side GitHub sync only targets one of the project's own git remotes
- Daemon clients and the daemon verify each other's user; the Windows pipe and shutdown event are owner-only
- The web UI's event socket requires a same-port origin; pages carry a hash-pinned CSP and every response `nosniff`

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
