# Changelog

All notable changes to clove are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `clove edit --body` / `--body-file` and `clove new --body-file` (`-` reads
  stdin).
- `clove sync github` records its repository in `.clove/sync-github.json` and
  refuses another repository. `--adopt-links` overrides.
- `cargo xtask publish-order`, `guard-sdk-licenses`, `guard-no-web`,
  `guard-tls`, and `guard-glibc`.

### Changed

- **The web UI and its API need an access token.** The URL that `clove serve`
  prints ends in `?token=…`; a request without the token gets 401. Scripts
  send `Authorization: Bearer <token>`. The daemon's `web_url` carries the
  token.
- **The daemon acts on `[daemon] git_sync` and periodic GitHub sync only after
  `git config clove.automation true` in the clone.** `clove doctor` reports
  `DAEMON_AUTOMATION_NOT_ALLOWED` until then. Existing users of these keys
  must run that command once per clone.
- `--clove-dir` with a path that is not a `.clove` directory is refused (exit
  4).
- `ls`, `ready`, and `query` JSON rows carry the whole frontmatter from every
  tier; use `--fields` for the lean row.
- Usage errors under `--format json|jsonl` print a `USAGE_ERROR` envelope on
  stdout.
- MCP tools refuse an unknown argument (for example `labels`, or `offset` on
  `clove_comments`) with an error that lists the valid names. Tool schemas
  carry `additionalProperties: false`.
- Web API: an empty PATCH or an unknown PATCH field is 422; write responses
  report `_meta.source: "files"`; a comment without an author uses the shared
  author rule; `?force=` accepts the shared boolean spellings.
- `clove dep add` and `clove parent` read only the items on the path that they
  validate. An unparseable file elsewhere in the store no longer blocks them.
- `clove sync github` reports an item deleted locally under `local_missing`
  and does not pull it again while its issue is unchanged.
- Daemon list queries run in parallel on read connections. A daemon write and
  the next read no longer scan the whole store.
- When `/tmp/clove-<uid>` exists and is not the user's private directory, the
  daemon uses `<clove-home>/run`.
- Web: a bare id (`#7AF3Q2K9`) is autolinked only in upper case. The prefixed
  form (`#proj-7af3q2k9`) still accepts either case.
- Web: the timeline renders only the rows near the viewport, and the New item
  modal asks before Escape or a backdrop click discards a draft.
- The TUI renders an item's Markdown body once per item and width, not on
  every frame.
- Performance: the frontmatter scan no longer reads item bodies; `list_rows`
  skips the graph for sorts that read no rank; a large full-row page is read
  from the files; `ready`, `blocked`, and `ls` skip the `COUNT` query when the
  page is not full; `dep tree` no longer ranks the whole graph; `clove export`
  streams items.
- Build: the toolchain is pinned to Rust 1.100.0, and the minimum supported
  Rust version is 1.91.
- Release: Linux binaries are built on Ubuntu 22.04 and need glibc 2.35 or
  newer. A tag is built only when CI succeeded on the tagged commit.

### Fixed

- A title, assignee, or external ref with `[`/`{` before `*` or `&` (for
  example `Deref [*ptr] crashes`) no longer makes the item unreadable.
- An item write that exceeds the 64 KiB frontmatter or 4 MiB body limit is
  refused, where before it wrote an unreadable file. A comment over 4 MiB is
  refused.
- Titles with several trailing line breaks and labels with Unicode line
  separators round-trip. An item file with a UTF-8 BOM parses.
- Delete without force refuses a parent that has children, and refuses while
  any item file is unparseable.
- Item files are 0644 when new and keep their mode on update.
  `.clove/.gitignore` ignores `issues/.tmp*`.
- The dependency cycle error shows the whole cycle path.
- A non-UTF-8 comment no longer hides its thread.
- `clove reindex` rebuilds the index inside `index.db`. A read during a
  rebuild can no longer leave an empty index or lose the stats history.
- A read no longer fails when the index cannot be refreshed or queried. The
  files answer, with a warning. An index that is more than 20 changes behind
  is refreshed or rebuilt by the next read.
- `clove ready` gives the same dangling-dependency warning from the index and
  from the files.
- An edit that happens during a staleness check is no longer missed by the
  next check.
- The daemon indexes an item file that an editor saves in place, reloads the
  project when `.clove/issues` is removed and created again (for example by
  `git checkout`), and follows an `index.db` that another process replaced.
- A daemon read that cannot be brought up to date is refused with
  `INDEX_BEHIND`, and the client reads the index or the files itself.
- A daemon-side create uses the current `id_prefix` and `default_type` from
  `config.toml`.
- Daemon answers larger than 8 MiB are no longer dropped (the limit is 64
  MiB).
- A starting hub removes the `hub.pid` of a dead hub at once. A watcher batch
  that fails is retried, and a burst of file events is flushed after 10 times
  `watch_debounce_ms`.
- Human list commands print a note on stderr when the page is truncated.
- `clove export` names item files that it could not export in `_meta.warnings`
  and on stderr.
- `clove import json|jsonl` validates every item before it writes any.
- `clove edit` in an editor saves through the store (validation, `updated`,
  index) and rolls back an invalid edit.
- Conflicting flags are refused in `stats`, `assign`, and `agent-doc`; an
  empty comment is refused; `agent-doc --out` works in JSON format; a
  non-UTF-8 argument no longer panics.
- `merge-driver` treats a failed `git merge-file` as an error.
- `doctor --fix` rebuilds the index through a running daemon.
- `dep tree` no longer reports "not found" when the daemon graph is behind;
  `dep tree` and `dep cycle` honor `--deep`.
- `clove import tk` and `clove import beads` translate the source tracker's
  ids in parent, deps, and links to the imported items. Beads `depends_on_id`
  edges are read. A second import no longer duplicates an item whose upstream
  ref or unmapped fields changed.
- `clove sync github`: a deleted linked issue no longer fails the comment
  sync; a lost create response no longer leads to a duplicate issue and item;
  an assignee-only change on GitHub is pulled; a pull from an issue without
  `clove-meta` keeps the local priority; pulled deps are existence- and
  cycle-checked, and a failed dep is retried; a comment with CRLF line breaks
  is not duplicated on a first sync; the issue list is retried on transient
  errors.
- Web: the list and board no longer freeze when a filter, a status change, or
  a card move reduces the number of rows.
- Web: the list filter box keeps a trailing space; `?page=` survives Back;
  invalid URL values are dropped. List titles are real links, and the `j`/`k`
  cursor scrolls into view.
- Web: the item and edit pages show "not found" or a Retry panel, not
  "Loading…" forever. A failed comment is removed from the thread and the
  draft is restored.
- Web: inline edits work for items outside the loaded window; item page state
  no longer carries over to the next item; form controls on the item pages
  have accessible names; the board keeps keyboard focus on a moved card.
- Web: an id inside a markdown link label or after a URL slash is no longer
  autolinked.
- Web server: store work no longer blocks the async workers; unlimited list
  and board reads do one file scan; a missing `/_app/` file is 404; only
  `/_app/immutable/` is cached as immutable.
- `clove-web` rebuilds when `crates/clove-web/dist` changes.
- The TUI edit form no longer reverts fields changed elsewhere while the form
  was open, saving an unchanged form writes nothing, and a malformed or
  unknown parent or dep id is rejected before any write. A failed save always
  shows its error.
- The TUI detail pane renders nested lists, tables, and link URLs, and scrolls
  to the end of bodies with indented code. The filter menu keeps active values
  that no item has any more; Up/Down move between Body lines in the form; the
  help overlay scrolls on short terminals.

### Security

- Human CLI output no longer prints terminal control sequences from item text.
- `index.db` is opened with `trusted_schema=OFF` and defensive mode. An index
  that contains a view or a trigger is rebuilt.
- Item reads refuse symlinks and non-regular files. `clove setup` and `clove
  init --merge-driver` do not write through symlinks in a project.
- A published `clove-web` verifies its downloaded web UI against a SHA-256
  pinned in the crate, not against the `.sha256` in the Release.
- Workflow tokens are read-only by default, and third-party actions are pinned
  to commits. CI runs a daily advisory check.
- `rusqlite` 0.41, which bundles SQLite 3.54 (RUSTSEC-2026-0459).

### Removed

- The unmaintained `.woodpecker/` pipelines and the redundant
  `package.homepage` manifest key.

## [0.3.0] - 2026-11-09

### Added

- `clove start`, `clove close`, and `clove status` accept one or more ids
  (`clove start ID1 ID2`, `clove status ID1 ID2 closed`). Every id is
  attempted. With one id the output is unchanged. With several ids, JSON
  `data` is an array with one `{id, ok, item}` or `{id, ok, error}` entry per
  id, the envelope `ok` is `false` and a top-level `error` is set if any id
  failed, and the exit code is the exit code of the first failed id. JSONL
  prints one line per id.
- `clove_plugin::run_raw(info, |cx, args| -> ExitCode)`: the plugin harness
  without the rendering step. It answers `--clove-plugin-info`, renders the
  error envelope when the host env is missing, and strips the leading
  command/provider echo; the closure renders and returns the exit code.
  `clove-sync-github`, `clove-import-tk`, and `clove-import-beads` use it.
- On Windows, plugin discovery (`clove <x>`, `clove plugin list`) tries every
  `PATHEXT` extension clove can start (`.com`, `.exe`, `.bat`, `.cmd`,
  `.ps1`), not only `.exe`. An unset or empty `PATHEXT` means
  `.COM;.EXE;.BAT;.CMD`, and `.exe` is always kept.
- `clove sync github` checks the body GitHub stored after it creates or
  updates an issue or a comment. When a proxy or GitHub App changed the body
  on the way (for example, an appended footer), sync edits it back once and
  lists it in `data.bodies_corrected`. If the edit back fails, sync adds a
  warning and goes on with the run.
- The web API (`clove serve`) adds `missing_refs` to every item row: every id
  in `deps`, `parent`, `relates`, `duplicates`, and `supersedes` that has no
  backing item, for an item of any status.
- `GET /stats/history` accepts `?days=all`: every day from the oldest item's
  `created` date, at least 90 days.

### Changed

- `clove_plugin::emit_success` (and so `run`/`run_with_info`) emits
  `_meta: {"warnings": []}` instead of `_meta: {}`, the same shape the import
  plugins emit. This includes the `clove sync github --dry-run` JSON envelope.
- `clove setup` registers the MCP server in the project's `.mcp.json`, the
  file Claude Code reads project servers from, and enables it with
  `enabledMcpjsonServers` in `.claude/settings.json`. Before, it wrote the
  server to the `mcpServers` key of `settings.json`, which Claude Code does not
  read, so the server did not start. Setup removes that old entry when it is
  unchanged. `clove setup --global` does not edit `~/.claude.json`; it prints
  `claude mcp add --scope user clove -- clove mcp` instead.
- The DCO sign-off is optional for now. The `DCO sign-off` CI job runs only
  when the repository variable `REQUIRE_DCO` is `true` (`CONTRIBUTING.md`).
- The agent directives that `clove setup` writes (`CLOVE.md`), the MCP server
  instructions, and `clove agent-doc` now say when to file items and how to
  split work: one item per separate task, bug, or fix; the steps of one task
  are comments on its item; every item has a self-contained body without
  conversation context. `scripts/agent-eval/` measures how well agents follow
  the directives.
- The agent directives require naming an item to the user as its id and
  title (`` `clov-H2D91NF2` (Fix the parser) ``), never a bare id. To make that
  easy, `clove new` and `clove comment` now print the title, and their JSON
  `data` (and the `clove_new` / `clove_comment` MCP results) gain a `title`
  key. `clove new` keeps the id and path as its first two fields.
- The id-and-title rule now covers every mention: a repeated mention of the
  same item and an id in a list get the title too, the title is copied
  exactly, and the directives tell the agent to check every id in a reply
  before it sends the reply.
- The `CLOVE.md` directives tell an agent to run the check that shows the work
  is done before it writes the closing comment, and to quote the actual result
  of that check (which tests pass and which fail) in that comment.
- The `CLOVE.md` directives show a short "Do not write / Write instead" table
  of conversation context that items and comments must not contain, and say
  that an item filed and left open needs no comment.

- Index schema 6 → 7 (`items.file_size`). The index rebuilds itself once on
  upgrade.
- cargo-nextest is the supported test runner. `cargo xtask test-all` runs
  `cargo nextest run --workspace --all-features` and then the doctests. A
  plain `cargo test` fails the daemon and plugin suites with a message that
  names nextest.

### Fixed

- `CLOVE.md` named a `clove list` command that does not exist; it now names
  `clove ls`, and a test rejects any subcommand the CLI does not define.
- `.clove/write.lock`, the store write lock, is git-ignored. `clove init` writes
  the entry to `.clove/.gitignore`, and in an existing project the next write
  adds it (as clove already does for `daemon.token`). `clove doctor` reports a
  `.gitignore` without it as `GITIGNORE_DRIFT`, and `clove doctor --fix` adds it.
- `clove sync github` (and the daemon's timed sync) now honours
  `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` and `NO_PROXY` (each also
  lowercase). It reaches the API through an HTTP `CONNECT` tunnel, with
  `Proxy-Authorization` from the proxy URL's credentials. Before, every request
  went direct, so sync failed behind a proxy. A proxy URL that is not
  `http://[user:password@]host[:port]` is now a clear setup error. TLS trust is
  unchanged (the platform roots, or `SSL_CERT_FILE` / `SSL_CERT_DIR`), so a
  re-terminating proxy works once its CA is trusted.
- Starting the daemon (`clove daemon start`, and the MCP server's auto-start)
  no longer fails with `daemon connect timed out` when it reaches a hub that
  has bound its socket but is not serving it yet, as happens on a loaded
  machine. A handshake that times out is now retried, like a refused connect,
  until the start's 10-second limit (gh-167).
- `clove dep tree --format json` now includes `repeat_ref` on every node, and
  `--flat` includes it on every row, as the MCP `clove_dep_tree` tool and the
  web already did. `repeat_ref` is required again in
  `docs/json-schema/v1/dep-tree.json`.
- The web dependency tree marks a subtree that is shown elsewhere in the tree
  with `(repeat)`. Before, that node was drawn with no children and no marker,
  so it looked like an item with no dependencies. The MCP `clove_dep_tree`
  description now names the `repeat_ref` marker.
- Issue and comment lists are now paged by page number on the
  `/repos/{owner}/{repo}/…` route instead of following GitHub's `Link: next`
  URL (`/repositories/{id}/…`), which some proxies refuse.
- `--deep` always runs the per-file staleness pass. Before, an in-place edit
  older than 2 seconds was missed, a running `cloved` answered `--deep` list
  reads with its fast check, and `clove blocked` ignored `--deep`. The
  per-file pass now hashes a file when its mtime or its size changed, and a
  clean check stores the new mtimes, so later checks do not hash the same
  files again.
- `clove dep tree` through a running `cloved` shows an item or an edge that
  was written just before. Before, the daemon's cached graph lagged the write
  by about 200 ms and the command could fail with `no item with id`.
- A pull-created item from `clove sync github` keeps the clove id from the
  issue's `clove-meta` when the id is valid and unused locally, and takes the
  issue's `created_at` as its `created`. Before, sync minted a new id, so deps
  and id mentions that named the old id broke.
- The `clove sync github` summary line counts comment conflicts too
  (`1 conflicts (1 comment)`). JSON output is unchanged.
- A human `clove plugin list` with no plugin installed says so, and names
  `clove plugin search` and `clove plugin list --all`. Before, it printed
  nothing.
- The web UI shows a missing reference without requesting it, so a board
  with dangling deps no longer logs `404` errors. An item page no longer
  shows the data of an item that was loaded before it.
- Web timeline: the axis ticks and the today label stay inside the plot, the
  cursor date chip stays right of the label column when the plot is
  scrolled, and the throughput `All` range covers the whole history instead
  of the last 90 days.
- TUI form: a long value that scrolls sideways keeps the field labels
  visible. Before, the labels were cut (`itle:`, `atus:`).

## [0.2.0] - 2026-10-21

### Changed

- `clove` gains a `test-knobs` feature (never in `default` or `full`): the test-only `CLOVE_PLUGIN_PROBE_MS`, `CLOVE_PLUGIN_HELLO_MS` and `CLOVE_REINDEX_WAIT_MARKER` are honoured only by such a build; released and `cargo install`ed binaries ignore them and keep the 500 ms probe and 5 s hello deadlines
- **License:** clove is now licensed under **AGPL-3.0-only** (`LICENSE`), with
  an additional permission for plugins (`LICENSE-PLUGIN-EXCEPTION`). The plugin
  SDK crates (`clove-types`, `clove-plugin`, `clove-plugin-echo`) stay
  **MIT OR Apache-2.0**. Releases up to and including 0.1.5 keep their
  "MIT OR Apache-2.0" license.
- Release archives now include `LICENSE`, `LICENSE-PLUGIN-EXCEPTION`,
  `LICENSE-MIT`, `LICENSE-APACHE` and `THIRD_PARTY_LICENSES.txt` (notices of the
  linked Rust crates); the web UI serves the notices of its bundled npm packages
  at `/THIRD_PARTY_LICENSES.txt`. Every published crate now carries its
  license text.
- Contributions need a DCO sign-off (`CONTRIBUTING.md`).
- **Plugin process model:** `clove <plugin>` now spawns the plugin and waits for
  it on every platform (Unix used to `exec` it). Stdio is inherited; Ctrl-C
  reaches the plugin as before; signals another process sends to the host pid
  are forwarded (SIGINT/SIGQUIT/SIGTERM/SIGHUP on Linux, SIGTERM/SIGHUP
  elsewhere);
  the host exits with the plugin's exit code, or dies of the same signal. The
  plugin no longer has the pid the user started.

- `rustls-webpki` 0.103.16, `webpki-roots` / `webpki-root-certs` 1.0.10
  (RUSTSEC-2026-0381, RUSTSEC-2026-0382, RUSTSEC-2026-0383)

### Added

- **Web timeline hover guide:** moving the pointer over the timeline shows a
  vertical guide line and a date chip on the top axis with the date under the
  cursor (mouse, touch and pen; themed like the tooltip) (#84).
- **`clove sync github` propagates comment edits** (#81): a comment edited on
  GitHub rewrites its local comment file, and a locally edited comment file is
  `PATCH`ed onto its GitHub comment in place, instead of being pulled or pushed
  once and then ignored (or, locally, posted again as a duplicate). A comment
  edited on both sides follows `--prefer` (`newer` compares GitHub's `updated_at`
  with the file's mtime; with no timestamp, or `manual`, neither side changes and
  the conflict is reported). Comment pairs are tracked in the sync state; a state
  written by 0.1.x, or none at all (a fresh clone), adopts identical pairs without
  duplicating or re-pushing anything.
- **`clove sync github` propagates comment deletions** (#80): a comment deleted
  on GitHub is deleted locally, and a deleted local comment file is deleted on
  GitHub too when you pass **`--delete-remote-comments`** (opt-in, because a
  missing file is also what checking out a branch without the comment looks
  like; without the flag the deletion is only reported as
  `data.comments.pending_remote_delete` and the comment is not re-pulled). Either
  way only if the surviving comment was not edited since the last sync (an edited
  one is re-created instead, never deleted). Every deletion appears in the
  `--dry-run` plan and in the run's
  output (`data.comments.delete_local` / `delete_remote`; the summary line gains
  `edited` and `deleted` counts); `--no-comments` disables all of it. A comment
  write GitHub rejects is skipped and retried like a rejected item.
- Plugin host channel: `store.update_comment` and `store.delete_comment`
  (`clove_import::Store::update_comment` / `delete_comment`), and `store.comments`
  reports each comment's `key` and modification time. No daemon protocol change
  (comments are not indexed, so these writes run in the host process).
- **The shipped plugins use the host IPC channel:** `clove-import-tk`,
  `clove-import-beads` and `clove-sync-github` no longer open the store; every
  read and write goes through the `clove` that runs them, via the new `store.*`
  tier of the channel (whole items with their own timestamps, host-minted ids,
  comments with original author and time). Imported and pulled items are now
  validated like `create` before they are written. `clove-import` code takes a
  `clove_import::Store` (`HostStore` / `DirectStore`). **Breaking (library):**
  `TkImporter::new` / `BeadsImporter::new` no longer take an id prefix, and
  `sync_net::sync_github` no longer takes one (the host mints ids). Like every
  other write, `store.*` writes go through a running `cloved`, and through the
  host only when no daemon runs.
- Daemon IPC protocol 10: the daemon applies imported items, external links and
  timestamped comments (`create_item`, `link_external`, `add_comment_at`), and
  `clove daemon status` reports `writes_applied`. A 0.1.x daemon is not used
  until it is restarted on 0.2.0; writes fall back to the host. `blocked` moves
  from the graph RPC onto `query` (`QueryKind::Blocked`), so the daemon filters,
  counts and pages it in SQL like `ls`/`ready` instead of returning every
  blocked id for the client to hydrate.

- **Host IPC channel for plugins** (`PLUGIN_SYSTEM.md` §6.6): while `clove
  <plugin>` runs, the host serves the clove tool surface (the MCP tools'
  arguments and results) over a per-invocation local socket, authenticated with a
  token (`CLOVE_HOST_SOCKET`, `CLOVE_HOST_TOKEN`). The SDK client is
  `PluginContext::host()` → `clove_plugin::HostClient`. Writes go through the
  unified write path in the host; a plugin that uses only this channel and the
  SDK links no AGPL code.

- New crate **`clove-service`**: the transport-independent tool engine (moved
  out of `clove-mcp`), shared by the MCP server and the plugin host IPC. The
  tool argument types moved to `clove_types::tool` (serde, with `JsonSchema`
  behind `clove-types`' optional `schemars` feature). MCP tool schemas and
  error texts are unchanged (pinned by a `tools/list` snapshot).

- **Source offer (AGPL-3.0 section 13):** the web UI links to the source of the
  running build (`/api/v1/meta` → `source_code`), and `clove version` prints the
  license and source URL. A fork sets `CLOVE_SOURCE_URL` at build time.

- **Breaking (plugin SDK):** `clove-plugin` no longer depends on `clove-core`.
  `PluginContext::open_store` / `load_config` and the `ItemStore` / `CloveConfig`
  re-exports are removed; a plugin that opens the store depends on `clove-core`
  and calls `ItemStore::new(cx.root.clone())`. `OutputFormat` moved to
  `clove-types` (still re-exported by `clove-core` and `clove-plugin`).

- `clove parent <id> <parent-id>` / `clove parent <id> --clear` sets or clears
  an existing item's parent from the CLI, through the same cycle-checked op as
  the web UI and MCP `clove_set_parent` (#72).

### Fixed

- **`clove sync github`: one rejected item no longer aborts the run (#76).** An
  assignee GitHub refuses (not a user) is dropped from that push with a warning,
  and any other per-item 4xx rejection (e.g. a 422) skips that item: the rest of
  the run completes and its state is saved, the skipped item is left unrecorded
  so the next run retries it, and the run lists it under `data.skipped` /
  `_meta.warnings` and exits 5. Auth (401/403), rate-limit, 5xx and network
  errors still abort. `sync_net::sync_github`'s `SyncReport` gains `skipped` and
  `warnings`.

- `clove serve` and `cloved` keep running when nothing reads their stderr any
  more (`clove serve 2>&1 | head -1`): a status line that cannot be written is
  dropped instead of panicking, which the release build turned into an abort.

- `clove edit` runs an `EDITOR`/`VISUAL`/`CLOVE_EDITOR` value that carries
  arguments (`code --wait`, `emacsclient -t`) the way git does, and a failure to
  start the editor names the editor instead of reporting "No such file" on the
  item (#73).

## [0.1.5] - 2026-10-18

### Changed

- The daemon's test knobs (`CLOVED_IDLE_SHUTDOWN_MS`, `CLOVED_HUB_GRACE_MS`, `CLOVED_STATS_SNAPSHOT_MS`, `CLOVED_GITHUB_SYNC_MS`, `CLOVED_LOAD_DELAY_MS`, `CLOVED_WATCH_ARM_DELAY_MS`, `CLOVED_BIND_DELAY_MS`) and `CLOVE_GITHUB_RETRY_MS` are honoured only by a `test-knobs` build; released and `cargo install`ed binaries ignore them (`CLOVED_WEB_PORT`, `CLOVED_DISABLE_WEB` and `CLOVE_GITHUB_API_URL` are unchanged)
- Frontmatter YAML is parsed with `serde_norway` instead of `serde_yaml_neo`, which pulled the unmaintained `unsafe-libyaml` (RUSTSEC-2024-0449); parsing, errors and written output are unchanged
- `rustls` 0.23.46 (RUSTSEC-2026-0346 through RUSTSEC-2026-0349)

### Fixed

- The plugin-install tests no longer see clove plugins installed on the developer's `PATH`
- The read-tier parity fixture keeps its intended `updated` order instead of re-stamping items with the wall clock

## [0.1.4] - 2026-10-17

### Changed

- The web server is the `web` feature of `clove-cli` and `cloved`, **off by default**: `cargo install clove-cli` / `cloved` no longer link the HTTP server or embed the web UI, and `clove serve` explains how to enable it; the release binaries are built with it (`cargo install clove-cli --features web` to get it from source)
- A published `clove-web` downloads its built web UI from the matching GitHub Release at build time and verifies its checksum (`CLOVE_WEB_DIST_DIR` for offline builds, `CLOVE_WEB_DIST_BASE_URL` for a mirror), instead of shipping generated assets in the crate; the web UI is generated into `OUT_DIR`

### Added

- Release workflow packs the web UI as `clove-web-dist-v<version>.tar.gz` (+ `.sha256`) and attaches it to the release; `scripts/release/publish.sh` publishes the crates in order behind a guard (tagged `HEAD`, clean tree, assets downloadable)

## [0.1.3] - 2026-10-08

### Fixed

- `clove daemon status` reports a daemon this clove cannot talk to (an older `cloved` after an upgrade) as running, with the `daemon stop --all` hint, instead of "not running"
- The daemon hub serves a favicon (`/favicon.svg`, `/favicon.ico`) and the project picker links it, so the picker no longer logs a 404; the web app declares its icon
- Web UI: the client-side `synced` filter counts any non-empty external ref, like the server

## [0.1.2] - 2026-10-08

### Added

- Web UI: synced items show their external id (`GitHub #67`, linked); with a sync target, unsynced items are marked and the list can filter by sync state
- Web UI: relationships show the related item's title; timeline rows and bars open the item; board move buttons name their target column
- `--synced`/`--unsynced` and `--parent ID` list filters (CLI `ls`/`ready`/`blocked`, `query` JSON, MCP, web `?synced=`/`?parent=`), answered by every read tier
- Web UI: an item's children on its detail page; comments render as Markdown; GitHub ids carry the GitHub mark; the timeline scrolls horizontally and marks closed items

### Changed

- Timestamps are written with millisecond precision (`2026-10-06T18:08:37.123Z`); whole-second values still read as the same instant; upgrade the plugins (`clove-sync-github`) together with clove, or the next sync re-pushes each item once
- Daemon IPC protocol 9 (filters gained `synced`/`parent`): a 0.1.1 daemon is not used until restarted on 0.1.2; reads fall back to the index and files

### Fixed

- `clove sync github` pushes a local edit made in the same second as the last pull
- `clove sync github` errors state GitHub's reason (status, message, field) and the item and operation that failed
- Web UI: List and Timeline open items under the daemon's `/p/<slug>/` mount instead of 404ing
- Web UI: the Timeline throughput chart groups hourly snapshots by day
- Web UI: an item page refreshes on live changes however it was opened, including body and comments
- MCP comments resolve their author from git config and `$USER` like the CLI, instead of `unknown`
- TUI short ids keep leading zeros
- Web UI: an item's type shows once — its full name where there is room, otherwise its letter with a themed tooltip
- Web UI: the list title column fills the table; detail and edit pages use the full height/width; Markdown bodies render task lists, tables and long code properly
- Web UI: the board's columns scroll inside the window instead of the page; a timeline row highlights its label and bar together on hover

## [0.1.1] - 2026-10-06

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
- `clove serve` defaults to the configured `[web] port`, falls back to a free port, and hands off to a running daemon; an explicit `--port` or `--host` the daemon is not on is always honored
- `clove reindex` waits for a rebuild already in progress instead of failing
- A symlinked `.clove/issues/` (or directory below it) is refused for reads and writes
- Daemon IPC protocol 8: clove 0.1.0 clients and daemons fall back to direct file access

### Fixed

- `clove … | head` exits quietly instead of aborting on a closed pipe
- The daemon runs in repositories nested deeper than the platform's socket-path limit
- `clove serve` in a second project no longer fails on the shared port or blames `[web] enabled = false`
- `clove sync github` names comment directions: `comments 0 pulled / 6 pushed`
- `clove doctor --fix` no longer deletes a live but slow daemon's socket and pid files, and `clove daemon stop` no longer reports such a daemon as not running
- `clove serve --host` is validated even when a daemon is running
- `clove tui` without an interactive terminal exits with an error instead of panicking

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
