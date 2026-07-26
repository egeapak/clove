# Changelog

All notable changes to clove are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **MCP read tools take `fields` and `compact`.** `fields` projects each item to
  a named subset (`{"fields": ["id", "title"]}`); `compact` drops keys that are
  null or an empty list, plus `schema` (a per-file migration marker, not item
  data — the one dropped key that is neither null nor empty). Compaction is on by default — measured on a 12-item
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
  count. `limit` keeps the most recent; `skip_newest` pages back through older
  ones — named for its direction, since it anchors at the opposite end from the
  list tools' `offset`.
- **`clove ls`/`ready`/`blocked` accept `--compact`**, applying the same
  omission the MCP read tools use — through one shared `view::compact_read`, so
  the two cannot drift. Note the CLI's index and daemon paths select a lean
  five-column row, so `--compact` there yields fewer keys still; `--no-index`
  gives the same key set as the matching MCP tool.
- **`offset` on every list read.** `clove search --offset`, `clove stats
  --history --offset`, and `offset` on the `clove_ready`, `clove_blocked` and
  `clove_search` MCP tools. All of them advertised a `limit` while pinning the
  offset to zero, so anything past the first page was simply unreachable — an
  agent that hit the cap had no way to ask for the rest. `clove comments` gains
  `--skip-newest` and `GET /api/v1/items/:id/comments` gains `?skip_newest=`,
  matching the `clove_comments` argument of the same name.
- **Read-shaping parity.** `--fields` and `--compact` on `clove search`,
  `--compact` on `clove show` and `clove query`. Each already existed on the
  matching MCP tool or on a sibling CLI command, so the same query shaped
  differently depending on which surface asked.
- **Multi-valued filters on every list read.** `--status`, `--type` and
  `--priority` repeat as *any of* (`clove ls --status open --status
  in_progress`); `--label` repeats as *all of* (`--label area:core --label
  area:ios` needs both). The MCP `clove_ready`/`clove_blocked`/`clove_list`
  tools take the same fields as either a string or a list of strings, and
  `clove query`'s JSON filter does too. Only the web API could express any of
  this before, so "open or in progress, labelled `area:core` **and**
  `area:ios`" was a question you could ask in the browser and nowhere else.
- **`--q TEXT` / `"q"` — a substring filter over id, title, and labels.** Also
  web-only until now (`?q=`). It is a *filter*, not a search: it never reads the
  body, which is what distinguishes `clove ls --q login` from `clove search
  login`.
- **`_meta.filters` echoes the parsed filter set** on every list read (and the
  `filters` key on the MCP page object), for the same reason `_meta.sort` and
  `_meta.limit` are echoed. The values are canonicalized and multi-valued, so
  the echo is what tells a client that `--status started` was applied as
  `in_progress` and `--label AREA:Core` as `area:core`. `search` omits the key —
  it takes no field filters.

- **`--sort`/`--desc` on every list read**, and `sort`/`desc` on the
  `clove_ready`, `clove_blocked`, `clove_list`, and `clove_search` MCP tools.
  Sorting previously existed only on the web API (`?sort=`), so an agent asking
  "what changed most recently" had to pull the whole store and sort client-side
  — exactly the payload the shaping work was about cutting.
  `clove ls --sort updated --desc --limit 10` now answers it in one call.

  The fields are `rank` (the default: priority, then dependency order, then id),
  `priority`, `created`, `updated`, `id`, `status`, and `type`. `status` sorts
  open → in_progress → closed and `type` sorts bug → feature → chore → docs →
  epic — the declared order, not the alphabetical order a bare `ORDER BY` on the
  stored words would give. `clove query`'s JSON filter object accepts `sort`/
  `desc` too.

  `clove search` keeps *relevance* as its default (title hits, then labels, then
  body). An explicit `--sort` there replaces that key entirely rather than
  tie-breaking within it, and `--desc` with no `--sort` reverses the relevance
  ranking rather than being silently dropped.

### Changed

- **One filter contract, shared by every surface.** `clove_core::view::Filters`
  is now the single filter set behind the CLI flags, the MCP tool arguments, the
  web query string, and `cloved`'s query RPC. Its fields are sets
  (`status`/`item_type`/`priority`/`labels`) plus `assignee` and `q`, with an
  empty set meaning *unconstrained* — so every existing single-value caller is
  unchanged, and the web's predicate (the only multi-value one there was) is
  deleted in favour of the shared type rather than reimplemented twice more.

  Three properties come with it:

  - **The index push-down is exhaustive.** `clove_index::push_down(&Filters)`
    splits the set into a SQL `Filter` and an optional in-memory `PostFilter`
    residue, destructuring `Filters` with **no `..` rest pattern** — so adding a
    filter field is a compile error rather than a constraint that quietly stops
    applying whenever `.clove/index.db` happens to exist. That is the bug class
    `--include-warnings` and `?limit=0` already demonstrated. Filter values are
    always bound parameters; nothing user-supplied reaches the statement text.
  - **`q` is deliberately held back as residue.** SQLite's `LIKE`/`lower()`
    case-fold ASCII only while `str::to_lowercase` is full Unicode, so a
    pushed-down `q=Ünicode` would miss a stored `ünicode-tag` the file path
    finds. A residue also changes the query mechanics — the `LIMIT` may no
    longer be pushed into SQL, and `COUNT(*)` is no longer `_meta.total` —
    which `clove_index::query_filtered` owns in one place.
  - **The file, index, and daemon paths return identical results for every
    filter combination**, pinned by `crates/clove/tests/filter_parity.rs`
    against a fixture built so each specific way of getting a filter wrong (a
    dropped field, OR-ed labels, a list truncated to its first element, a `q`
    that reaches the body) changes the answer.

- **`clove blocked`'s ordering moved into the daemon.** `GraphRequest::Blocked`
  now carries the `Order`, and `cloved` applies it through the index's own
  `ORDER BY`. The CLI used to re-sort the returned ids locally — a second
  implementation of the comparator that could only approximate `rank`, since it
  had no topological ranks of its own and had to special-case that field. That
  local sort is gone.

- **An unrecognized filter value on the web API is now a `VALIDATION_ERROR`.**
  `?status=bogus` used to compare raw strings and return `[]`, which a client
  cannot distinguish from "there are no open bugs" — the same treatment
  `?sort=nope` already gets. Relatedly, `?q=` now matches id, title, and each
  label *separately* rather than against the three concatenated into one
  haystack, so a needle containing a space can no longer match across a field
  boundary. That was an artefact of the concatenation, not a feature.

- **`clove_ipc::PROTOCOL_VERSION` is 4 → 5.** `QueryRequest`'s five scalar
  filter fields collapse into one `filters: view::Filters`, and
  `GraphRequest::Blocked` gains `order` while dropping the dead
  `include_warnings` (no surface could set it; the only caller hard-coded
  `true`). The codec is JSON, so a v4 frame still *decodes* — which is precisely
  why this needs a version rather than a compatible field: a v4 client's
  `"status": "open"` lands nowhere and the daemon answers the **unfiltered**
  list, silently. The handshake rejects the mismatch instead; restarting
  `clove daemon` is cheap and the daemon is a cache, not a source of truth.

- **One sort contract, shared by every surface.** `clove_core::view::Order`
  (`SortField` + a direction) is the single comparator behind `--sort`/`--desc`,
  the MCP `sort`/`desc` arguments, the web's `?sort=`/`?dir=`, and the SQL
  `ORDER BY` the index and daemon paths run. `clove-web`'s private `sort_items`
  — the only *user-selectable field* sort in the project — is now a thin wrapper
  that parses the query string and delegates, and its accepted spellings are
  unchanged. (Other comparators remain by design: `GraphStore`'s graph ordering,
  `clove-tui`'s in-memory list sort, and the SPA's client-side `sortItems`.)

  Three properties come with it:

  - **Every order is total, ending in an id tiebreak**, and `--desc` reverses
    the whole key rather than only its head. Paging over a partial order
    silently repeats and skips rows, because ties resolve to whatever the input
    order happened to be — raw `read_dir` order on the file paths.
  - **The file, index, and daemon paths return identical id sequences for every
    field.** `crates/clove-index/src/query.rs` had two hardcoded `ORDER BY`
    clauses that were *not* identical (the search one carried an extra, dead
    `topological_rank IS NULL ASC` term); both are now generated from one
    `match` on the enum, so a new sort field is a compile error rather than a
    path that silently keeps the old order. No user string is ever interpolated
    into SQL. Pinned by `crates/clove/tests/sort_order.rs`, which compares all
    three paths against a fixture where every field orders the store
    differently.
  - **`_meta.sort` and `_meta.dir` echo the ordering applied**, for the same
    reason `_meta.limit` echoes the effective limit. `search` reports
    `"relevance"` when no field was named.

  `clove_ipc::QueryRequest` gains an `order` field. `PROTOCOL_VERSION` is
  deliberately *not* bumped: the codec is length-delimited JSON and the field is
  `#[serde(default)]`, so a mixed-version `clove`/`cloved` pair keeps working in
  both directions and no existing field changes meaning.

- **An unrecognized `?sort=`/`?dir=` on the web API is now a `VALIDATION_ERROR`**
  rather than a silent fall back to `rank`, matching what `clove ls --sort nope`
  has always done. (`?limit=abc` remains lenient; making the whole query string
  strict is a separate item.)

- **One limit contract, shared by every surface.** `clove_core::view::Page`
  decodes `offset`/`limit` once — *absent → that surface's default, `0` →
  unlimited, `n` → at most `n`* — and the CLI, MCP, web API and daemon all route
  through it. Each surface keeps its own default (CLI 100, MCP 50, web
  unlimited), but those numbers now live in one module rather than as literals
  scattered across the call sites, and every list response echoes the effective
  limit back as `_meta.limit` / `"limit"` so the default is discoverable rather
  than folklore. Two disagreements this removes:

  - `GET /api/v1/items?limit=0` returned *zero* rows; on the CLI and MCP the same
    value means *everything*. It now means everything on all three.
  - `cloved`'s query RPC took the wire `limit` as a raw pass-through, so a
    non-CLI client sending `limit: 0` got zero rows from the one surface that
    documents the opposite.
  - `clove comments --limit 0` returned no comments, for the same reason.

  A comment thread now pages on the same per-surface default as an item list, so
  **`clove comments` caps at 100 by default** where it previously returned the
  whole thread. That cap is never silent: JSON output carries `_meta.total` /
  `returned` / `limit` (the `data` array itself is unchanged), and human output
  prints a `showing N of M comments` line. `--limit 0` returns everything.
  `GET /api/v1/items/:id/comments` keeps its unlimited web default, so the
  bundled UI is unaffected.

- **`clove show` no longer scans the whole store.** `ready`/`blocked_by` are
  computed from the item's own dependency closure, falling back to the
  whole-store graph only when that closure is very large. Measured 57.3ms ->
  0.06ms at 10k items, and flat rather than linear in store size. `clove show`,
  the `clove_show` MCP tool, and the daemon's `show` RPC now share the
  `ops::graph_terms` helper instead of each deriving those fields themselves.
  (`clove_show` and the daemon RPC share all of `ops::show`; `clove show` still
  assembles its own object and has no daemon or index tier.)

  *User-visible:* `clove show` computes `ready`/`blocked_by` unconditionally.
  They were previously gated behind `--verbose` — emitted as `null` with a
  "pass --verbose for ready/blocked_by" warning otherwise — solely because
  deriving them meant scanning the store. `--verbose` remains accepted and is
  now a no-op for this purpose.
- **`clove daemon` failures now exit 7, not 5.** Communication failures were
  reported as `CloveError::Io` against a fabricated `"daemon"` path, classifying
  them as `IO_ERROR` — a filesystem problem. They now use `DAEMON_ERROR`/exit 7,
  which the exit table has published since M0 without anything producing it.
- **`GET /api/v1/items/:id/comments?limit=` keeps the newest N**, matching
  `clove comments --limit` and the `clove_comments` tool. It previously kept the
  *oldest* N — the same parameter name with the opposite meaning. The bundled web
  UI never sent the parameter, so it is unaffected.
- **Daemon-reported errors now use clove's standard error codes.** `cloved`
  emitted a private code set (`not_found`, `self_loop`, `cycle`,
  `already_exists`, `invalid_field`, `op_failed`) that neither matched the
  documented `error.code` spellings nor covered them — every unrecognized
  failure collapsed into `op_failed`, merging distinct classes (I/O, exit 5,
  with parse failures, exit 4), so no client could recover the right exit code.
  It now emits the same `code`/`exit` pair as the CLI and web API, so a failure
  reported by the daemon is indistinguishable from the same failure raised
  locally — including its *text*: local MCP errors are now rendered
  `CODE: message` too. Before, the same missing id read `ITEM_NOT_FOUND: …`
  through the daemon and bare `no item with id …` through the local fallback,
  so within one session `clove_status` and `clove_show` disagreed.

  *User-visible:* MCP tool errors that route through the daemon now read
  `ITEM_NOT_FOUND: no item with id …` rather than `not_found: …` (likewise
  `CYCLE_DETECTED`, `VALIDATION_ERROR`, `SELF_LOOP`, `ALREADY_EXISTS`). Scripts
  matching the old lowercase strings need updating; the new spellings are the
  documented ones (DESIGN §7.3). `clove daemon` failures now report
  `daemon transport error: …` rather than `daemon protocol error: …`.

  The IPC wire is otherwise unchanged: the error reply gains a self-describing
  `exit` field that defaults compatibly in both directions, so there is no
  protocol bump and a mixed-version `clove`/`cloved` pair keeps working. The
  numeric `exit` from a *remote* failure still has no consumer — MCP reports
  daemon errors as text — so over IPC this change is visible as the error
  *strings* above, with the classification in place for the write routing that
  will use it.

### Fixed

- **`--fields` was silently dropped on the CLI's index and daemon paths.** Those
  tiers select a lean five-column row (`id`/`status`/`type`/`priority`/`title`),
  so `clove ls --fields id,created` returned `[{"id": …}]` where `--no-index`
  returned both keys — the answer depended on whether `.clove/index.db` happened
  to exist. A request reaching outside the lean set now falls back to the file
  scan; one that fits still uses the index.
- **`depth: 0` on `clove_dep_tree` and `?depth=0` returned the root alone.** `0`
  is unlimited for every other bound, including `clove dep tree --depth 0`, and
  DESIGN documented it as such; the two non-CLI surfaces passed it straight
  through, so a client asking for the whole tree got one node and no error.
- **`GET /api/v1/stats/history` ignored `?offset=`** (it was parsed and dropped)
  and reported no `total`, having pushed the limit into SQL — the same defect
  the CLI's `stats --history` had. It now windows after the fetch and reports
  `total`/`returned`/`offset`/`limit`.

- **`clove search` and `clove_search` disagreed on what counts as a hit.** The
  MCP tool matched title, **labels**, and body and ranked in three classes; the
  CLI matched title and body and ranked in two — and its index path used an FTS
  table that indexed `title, body` only, so a label-only hit was returned by the
  MCP tool and by neither CLI path. Matching and ranking are now one function
  (`view::rank_search_hits`) shared by all three, and `items_fts` indexes labels.

  *Index schema v4 → v5.* Existing indexes are rebuilt from the files on next
  open — one extra scan, and one `note: index at … schema changed …; rebuilding`
  line on **stderr**, which a script parsing stderr will see once. Nothing is
  lost: the durable `snapshots` history is carried across the rebuild.

  *Not fixed:* the FTS matches whole tokens while the shared matcher matches
  substrings, so the index path remains a narrower prefilter — searching `core`
  finds it inside the body word `corepart` only with `--no-index`. Pre-existing,
  and now written up in `docs/READ_PATH_ROADMAP.md` §6.1 with the options.
- **A schema bump left the index empty rather than rebuilt.** `open_or_create`
  recovered the file and stopped, and "empty" is indistinguishable from "nothing
  matched" at every call site — `clove search` returned zero rows for every query
  after a bump until that was patched defensively at the call site. The staleness
  gate then reported every file as changed, over the inline-refresh limit, so the
  CLI silently fell back to scanning files for *every* query until someone ran
  `clove reindex` by hand. `Index::open_or_rebuild` repopulates from the files
  instead; the CLI read paths and the daemon use it.
- **`GET /api/v1/board` silently dropped `limit`/`offset`.** It shares its filter
  and sort handling with the item list, so it accepted both and ignored them.
  They now window each column independently — the one reading of a single limit
  over grouped columns that means anything. `count` remains the column's full
  size (so a header reading "Closed · 412" over 50 cards stays honest) and
  `returned` is what came back.
- **`clove_search` paged over an undefined order.** Results were ranked by match
  class (title / label / body) with a *stable* sort over `read_dir` order, so
  ties kept whatever order the filesystem happened to return and reshuffled when
  a file was added. Harmless while the tool had no `offset`; adding one turned it
  into a paging contract, where an agent walking `offset=0,50,100…` would re-read
  some items and never see others. The order is now the total
  `(match class, priority, id)`. `clove_list`/`clove_ready`/`clove_blocked` were
  never affected — they already sorted by `(priority, topological rank, id)`.
- **`clove stats --history --limit N` reported the truncated count as
  `_meta.total`**, so a capped series was indistinguishable from an exhausted
  one. The limit was pushed into the SQL query; the series is now windowed after
  the fetch, like every other list, and `_meta` carries `returned`/`offset`/
  `limit` alongside the real total.
- **An index-backed list with a very large `--offset` failed where the file scan
  succeeded.** `offset + limit` was handed to SQLite as the row count to fetch;
  above `i64::MAX` that is a datatype error, surfaced as `IO_ERROR`/exit 5 for
  what is a legal (empty) window — and `--no-index` answered the same query with
  `[]` and exit 0. The fetch count is now clamped to SQLite's range.
- **`clove search` could answer from a stale index.** It queried the index with
  no freshness check at all, so any item created since the last `clove reindex`
  was silently absent from results. After a schema change the effect was total:
  `Index::open_or_create` drops and recreates the file *empty*, which read as "no
  matches" for every query rather than "index unavailable". It now applies the
  same staleness gate the list commands use, falling back to a file scan when the
  index cannot be trusted. (Not identical to the list commands: `clove search`
  falls back to files when `[index] auto_refresh = false`, where `ls`/`ready`
  still query the unverified index — for a *search*, "zero rows" is
  indistinguishable from "no matches", so answering from an unchecked index is a
  silent total failure rather than a stale ordering. The list commands' behavior
  here is arguably the remaining bug.) `clove --deep search` now honors `--deep`,
  which it previously accepted and ignored.
- **An item whose only obstacle was a dangling (missing) dependency was
  invisible.** It is excluded from `ready` — correctly, it is not workable — and
  `blocked` also filtered it out unless `--include-warnings` was passed, so by
  default a broken reference appeared in neither list. `blocked` now includes
  such items by default, matching what `GraphStore` has always reported
  (DESIGN §5.3) and keeping the `ready ∪ blocked ∪ closed` partition intact for
  dangling references. (That partition has a separate, pre-existing hole: an item
  inside a hard-dependency cycle is `excluded`, so it appears in neither list.)

  `--include-warnings` is **removed** from `ready` and `blocked` (and from the
  `clove_ready`/`clove_blocked` MCP tools): it no longer selects anything. It was
  documented on `ready` but never implemented there; rather than implement it —
  which would have put one item in both `ready` and `blocked`, breaking a
  DESIGN-stated invariant — the visibility it was meant to provide now comes from
  `blocked`'s default.
- **`clove blocked` omitted `blocked_by`.** The field the list exists for was
  emitted by `ops::blocked` and the web API but not by the CLI. It now comes from
  the same `ops::graph_terms` helper `clove show` and the MCP tools use, so the
  ids cannot drift between surfaces.
- **Silent lost updates in `clove status`/`start`/`close`, `clove set`, and
  `clove edit --field`.** These read the item without the store write lock and
  only took it for the write, so a concurrent writer — the web UI, an MCP agent,
  or the daemon — could commit in between and have its change silently
  overwritten, with no error. They now perform the whole read-modify-write under
  one lock via `ItemStore::update_with`, matching `clove_core::ops`. Only
  concurrent writers were affected; a single user running one command at a time
  never was. Each command also samples the clock once now, so a close writes the
  same timestamp to `closed` and `updated`.
- **The same lost-update race in `clove sync github`.** `link_local`, which
  stamps `source_system`/`external_ref` onto an item after reconciling it, read
  and wrote without holding the store lock. The per-repo sync lock does not help:
  it excludes other syncs, not the CLI, web, MCP, or daemon. Worse than the CLI
  case, because the write persists a whole-frontmatter snapshot — a concurrent
  edit landing mid-window was lost entirely rather than partially.

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
