# clove — Session Handoff

**Updated:** 2026-06-04
**State:** **M0–M3 are complete and gated; the first M4 items have landed**
(`clove stats` + analytics history, the `clove tui` read-only browser, and an
exact-incremental index/daemon graph — see the "M4" sections below). Full CLI
command surface;
the SQLite index serves `ls`/`ready`/`query` (lean covering-index scan, default
`--limit 100`, fast staleness with `--deep`), `search`, `reindex`, and
`doctor` divergence. **M2 (Interop)** adds import (tk/beads/github), export
(json/jsonl/github), and a real 3-way `clove merge-driver`. **M3 (Daemon)** adds
the optional `cloved` (file-watch incremental index, IPC, opt-in git auto-sync),
`clove daemon start|stop|status`, transparent read routing through the daemon, and
a `doctor` daemon-health check — see `docs/M3_PLAN.md` and
`docs/M3_ACCEPTANCE_GATES.md` (gates M3-G01–M3-G10 all pass). New lean crate
`clove-ipc`; index schema **v3** (`file_mtimes.synced_at`). When a daemon is
running the CLI defers read/graph work to it — `ls`/`ready`/`query` (lean index),
`search` (FTS), `blocked`/`dep tree`/`dep cycle`/`dep add` cycle-check (cached
graph), and `reindex` (delegated so the daemon stays coherent) — all with a clean
local fallback; see the routing matrix in `docs/M3_ACCEPTANCE_GATES.md`. The daemon
is **per-project**: all git worktrees of a project share the **main worktree's**
`.clove/` (and thus one index + one daemon) — `find_repo_root` resolves linked
worktrees to main; a system-wide daemon was evaluated and rejected for v1
(DESIGN §8.1). **Idle self-shutdown defaults to 4 h** (every command is a heartbeat
that resets it; `0` = never; no auto-restart yet — a future MCP holds the
heartbeat). The prior environment-only `linked_worktree` test failure is fixed
(commit signing disabled in the test). All five JSON schemas
published + validated. Perf/parity/fuzz/golden gates pass (M0
`docs/IMPLEMENTATION_PLAN.md`, M1 `docs/M1_ACCEPTANCE_GATES.md`, M2
`docs/M2_ACCEPTANCE_GATES.md`, M3 `docs/M3_ACCEPTANCE_GATES.md`). Tests green
except one environment-only failure (`repo::tests::linked_worktree…`, a sandbox
git-signing artifact, not a code defect; the token-gated `github_roundtrip`
shows as `1 ignored`).
**M4 — Extras** has begun: two items have landed — **`clove stats`** (the
analytics command; see the "M4 — `clove stats`" section below) and **`clove
tui`** (T-U01), a read-only terminal browser. New crate `clove-tui` (ratatui,
depends only on `clove-core`; reads via
the file-store scan path so it is always correct and never touches the index or
daemon). Master-detail UI: **All / Ready / Blocked** tabs with live counts, an
item list sorted like `ls`, and a detail pane with **Overview / Dep tree /
Comments** sub-views (dep tree shows status glyphs + titles inline; overview is
triage-ordered, renders the **Markdown body** via `pulldown-cmark`); substring
search (`/`), refresh
(`r`), help overlay (`?`), and pane-focus keys. **Sort & filter**: `s`/`S` cycle
the sort field/direction (default `rank` = `(priority, topo, id)`); `f` opens a
facet filter menu (status/assignee single, type/priority multi-OR, labels
multi-AND, over values present in the repo), AND-composed with search and tabs,
`x` clears, with status-line chips, an `Items (N/M)` count, and an empty-result
escape hatch. Filters/sort persist across tab-switch and refresh; selection is
preserved by id. The wide Overview detail is a **fixed shrink-to-fit two-line header**
(line 1: short id + priority glyph + ALL-CAPS type tag, status flush-right;
line 2: bold title with assignee + deps *count* flush-right under the status;
then any blockers) → **edge-to-edge rule** → **scrolling Markdown body** →
edge-to-edge rule → **sticky footer** (labels left, `created Jan 20 · updated
Jan 24` right, day resolution); the narrow Overview is one scrolling paragraph
(meta line, then the title, labels/dates inline). The deps *list* is in the Dep
tree tab. The list shows a single-letter colour-coded type icon, a **short id**
(`#42`, prefix dropped), and a **priority glyph** (`!` p0, `↑` p1, `•` p2 **and**
p3, `↓` p4) on a graded colour ramp (red → orange → amber → dim icy blue →
gray); p2/p3 share the `•` and are told apart by hue (amber vs icy blue). Legend in the help
overlay. The meta line (id + priority + type) renders from a single shared
`head_spans` for both header widths. `clove-tui` has 27 tests (data-layer + a
`TestBackend` smoke test + insta render snapshots of 12 states × 3 terminal
shapes), plus an `#[ignore]`d `generate_screenshots` PNG tool (DejaVu Sans Mono;
output gitignored under `docs/screenshots/`). The layout is **adaptive**
(`ui::pick_layout`): side-by-side
(≥80 cols) / stacked (50–79 & tall) / single focused pane (narrow or short), with
width-aware list columns, a compact tab bar below 20 rows, full-screen overlays
on small terminals, and a too-small guard. Design directions came from a
frontend-design and a UX/IA review (larger items recorded in the M4 backlog).
Wired as the default-on `clove tui` subcommand (interactive-only, ignores
`--format`). Validated by **insta render snapshots** across 3 terminal shapes
(portrait/landscape/square) for adaptive layout. Full `cargo test --workspace`,
clippy `-D warnings`, and fmt are green. The `ratatui`/`crossterm`/
`pulldown-cmark` tree is all MIT/Apache/Zlib/Unicode (no new `cargo deny`
exposure).
**TUI internals (refactor):** `clove-tui` is now modular — `app/{mod,data,listing,detail,filter_menu}.rs` with state regrouped into `Data`/`Listing`/`DetailPane`/`FilterMenu` sub-structs (command methods stay on `App` as the coordinator), and `ui/{mod,util,style,tabs,list,status,help,filter_menu}.rs` + `ui/detail/{mod,overview,tree,comments}.rs` (per-component/per-page). The event loop is tick-driven (1fps idle / redraw-after-event / 10fps when `App::is_busy()`). The split is structural (no locks yet); the **next M4 step** is the concurrent TUI model — move the wholesale re-scan onto a background worker behind per-sub-struct locks and drive the 10fps `is_busy()` cadence with a spinner.

**Next step (rest of M4):** TUI write actions (status/priority/label edits, …),
web UI, bidirectional vendor bridges, and richer history/changelog — see
`IMPLEMENTATION_PLAN.md` M4 backlog. Still undesigned beyond the TUI and stats.

### Small backlog (optional M0/M1 nice-to-haves, non-blocking)
- Broaden JSON-schema validation to more commands (version/reindex/doctor/new)
  if desired — the 5 data-shape families are done.
- `ls` gate is 15 ms with ~4.5 ms headroom; tighten to ~8 ms only if you want CI
  to catch a covering-scan regression.

---

## M0 CLI + index wiring (this session)

The whole `crates/clove` command surface is implemented, dispatched from
`main.rs` via `cli.rs`, on shared helpers (`context.rs` discovery, `util.rs`
parsing, `item_json.rs` shaping, `cmd/listing.rs` filter/sort/paginate/emit):

- init, new, show (fast vs graph path), edit/set (KEY=VALUE + `labels+=/-=`),
  status/start/close, label/assign/priority, dep add/rm/tree/cycle,
  ready/blocked, ls/query (stdin JSON filter), comment/comments, version,
  agent-doc (idempotent, `--check`), doctor (`--fix`/`--strict`), reindex, search.
- `reindex` calls `clove_index::reindex`; `search` uses the FTS index when
  present (`_meta.source = "index"`) and falls back to a file scan otherwise.
- Exit codes per DESIGN §7.6 (dep self-loop=4, cycle=3, not-found=2, …);
  every command supports `--format json|jsonl` with the standard envelope.
- New `clove-core` support: `CloveError::{NoRepo, SelfDependency,
  DependencyCycle, DependencyExists}` and the `doctor` module (`diagnose`/`fix`,
  the §7.7 check suite). New `clove-index` `search()`.
- Tests: `crates/clove/tests/cli_commands.rs` (14 e2e tests via `assert_cmd`)
  plus the clove-core `doctor` and unit tests. `cargo test --workspace` is green
  except the pre-existing `repo::tests::linked_worktree_resolves_to_main_worktree`
  (an environment-only failure: it makes a real `git commit`, which this sandbox
  routes through a signing server that returns 400).

**Deferred (noted above):** T-S06, T-S08, T-CLI14, T-CLI16. The list commands
(`ls`/`ready`/`blocked`/`query`) currently always read the files (the source of
truth); index acceleration for them is the T-S06 follow-up.

---

## M1 progress — `clove-index` library (this session)

Built the self-contained index crate; depends only on the finished `clove-core`.

- **T-S01** `db.rs` — schema/DDL, `Index::open`/`open_or_create`, `IndexError`,
  `ItemRow`; `user_version` schema check with drop-and-rebuild on
  mismatch/corruption.
- **T-S02** `write.rs` — `upsert_item`, the single encapsulated write path
  (items + edges + labels + FTS5) in one `BEGIN IMMEDIATE` txn.
- **T-S03** `stale.rs` — `check_staleness` / `apply_staleness`
  (`StalenessReport`); dir-mtime + count fast path, content-hash gate with the
  2 s recent-file guard.
- **T-S04** `reindex.rs` — **library half only**: `reindex(issues_dir, db_path)`
  with tmp-build + atomic rename, `fd-lock` advisory lock, parallel parse, topo
  ranks via `clove-core` `GraphStore`. The `clove reindex` CLI command is
  deferred (needs the M0 command surface).
- **T-S07** `query.rs` — `query_items` with `Filter`/`QueryMode` (ready SQL +
  list filters), ordered `(priority, topo rank NULLs-last, id)` to match the
  file path.
- Benchmarks in `benches/index.rs` (criterion); unit tests cover every AC above.

**Deferred (blocked on the M0 CLI):** T-S04 CLI half, T-S05 `clove search`,
T-S06 `with_index` read-path wrapper, T-S08 `doctor` index-divergence check.

**Decisions / deviations made (don't relitigate without reason):**
- Added `clove_core::graph::GraphStore::topological_ranks()` — a small public
  accessor exposing the already-computed ranks so the index can persist
  `topological_rank` without rebuilding the graph.
- **rusqlite pinned to 0.37** (not DESIGN's 0.40): 0.40's `libsqlite3-sys` 0.38
  build script needs the unstable `cfg_select!` macro, which fails on the pinned
  stable toolchain. 0.37 (libsqlite3-sys 0.35) still bundles SQLite ≥3.43.
- **FTS5 deviates from the §6.1 DDL in two ways**, both forced by pairing a
  contentless FTS table with a `WITHOUT ROWID` `items` table: (1)
  `contentless_delete=1` so a shadow row can be deleted by rowid (the spec's
  `'delete'` command needs the old column values, which we lack on edit/delete);
  (2) an `fts_map(fts_rowid → item_id)` side table so a full-text match (which
  yields only rowids on a contentless table) can be resolved back to an item id.
- `upsert_item` (incremental write-through) stores best-effort `file_mtime=now`
  and `content_hash` over the body; the authoritative file mtime/hash come from
  `reindex`/`apply_staleness`. `ParentOf` is not stored in `edges` (parent lives
  in `items.parent_id`; the ready query only consults `DependsOn`).
- **Perf note:** at 2k items (release): reindex ~116 ms, ls ~3.1 ms, ready
  ~3.7 ms, staleness-clean ~2.1 ms. The 10k acceptance-gate tuning (esp. the
  staleness fast path doing a per-file `readdir`, and `ls` row construction)
  should be revisited when the CLI read path lands.

---

## M2 — Interop (this session)

Made `clove-import` a real crate (was a wired stub) and added the import/export/
merge CLI surface. All five M2 tasks land, gated per `docs/M2_ACCEPTANCE_GATES.md`.

- **T-M04** `clove export json|jsonl` — JSON envelope with a `data` array, or
  NDJSON one item per line (Beads-isomorphic); `--out FILE` atomic write;
  byte-deterministic. (`export.rs`)
- **T-M05** `clove merge-driver <O> <A> <B> <L>` — 3-way item-file merge: scalars
  take the changed side or conflict; lists do a sorted/deduped 3-way set union
  with same-element remove/add conflicts isolated to that field; body delegates to
  `git merge-file`. Writes to `%A`, exit 0 = clean / nonzero = conflict (git
  contract). Installed via `clove init --merge-driver`. (`merge.rs`)
- **T-M01** `clove import tk <.tickets dir>` — `task→chore`, `tags→labels`,
  `links→relates`, H1→title (filename fallback warns), `source_system="tk"`,
  `external_ref` = tk id. (`tk.rs`)
- **T-M02** `clove import beads <issues.jsonl>` — full field map; `deferred`→open
  +label; unmapped fields stashed as `external_ref="beads-meta:<json>"`;
  `comment_count>0` warns. (`beads.rs`)
- **T-M03** `clove import/export github` — feature-gated (`github`, default on,
  adds `octocrab`+`tokio`); `<!-- clove-meta: {…} -->` body codec; `gh-<number>`
  refs. (`github.rs`)

**New crate layout:** `clove-import` now has `tk.rs` / `beads.rs` / `github.rs` /
`merge.rs` / `export.rs` / `map.rs` (shared coercion + `external_ref` idempotency
index) / `plan.rs` (`ImportPlan`/`ImportReport`) / `error.rs`. CLI handlers in
`crates/clove/src/cmd/{import,export,merge_driver}.rs`.

**`github` feature + token resolution:** `octocrab`/`tokio` are isolated behind the
`github` cargo feature so `--no-default-features` (lean / cross) builds stay light
(verified clippy-clean). The token resolves from `GITHUB_TOKEN`, falling back to
`gh auth token`. Network round-trip test is `#[ignore]` + token-gated, so CI/
sandbox stay green (offline mapping/codec tests cover the logic).

**Decisions (don't relitigate):** idempotency key = `external_ref` (shared
pre-scan); `export jsonl` is Beads-isomorphic so "re-import own export" round-trips
through `import beads` (no separate `import json`); no schema bump, no new
frontmatter fields. The tolerated `linked_worktree` env failure remains.

New fuzz targets (`merge_driver`, `import_tk`, `import_beads`) + seed corpora are
wired into the CI 30 s-per-target fuzz job; new benches (`bench_export`,
`bench_import_tk`, `bench_import_beads`) compile under `cargo bench --no-run`.
`clove agent-doc` now documents the interop surface + the §9.4 post-merge
index-refresh note (idempotency/`--check` tests stay green).

---

## M4 — `clove stats` (this session)

The first M4 item: a read-only **work-item analytics** command.

- **`clove stats`** — aggregates the store into one report: `total`; counts by
  status / type / priority / assignee / label (assignee+label capped by `--top N`,
  default 10); `ready` / `blocked` / `excluded` / `dangling`; dependency-cycle
  count; per-epic completion rollups (`--no-epics` to skip); and created/closed
  **throughput** over 7d/30d/all windows. It also folds in the **daemon** §8.4
  `STATUS` telemetry and local **index** presence/freshness, so one command shows
  work-item *and* operational state.
- **Compute path:** a single `scan_frontmatter()` + `GraphStore::build()` — files
  are always truth, so the report is always correct; the index/daemon are reported,
  not relied on. (Not a hot path; index-SQL `GROUP BY` acceleration is a noted
  future optimization, not needed for v1.)
- **Persistence (one SQLite database):** snapshots live in a `snapshots` **table
  inside `.clove/index.db`** — a single database for the whole tool, not a second
  file. `--snapshot` records the report; `clove stats --history [--since RFC3339]
  [--limit N]` replays the series (headline scalars as columns for cheap trend SQL +
  a `detail_json` blob for the rich breakdowns). The index is a rebuildable cache,
  so the two destructive cache ops are taught to **carry the durable `snapshots`
  table across them**: a full `reindex` (tmp-build + atomic rename) copies the rows
  into the new DB before the rename, and schema-mismatch recovery reads them out
  before the drop-and-rebuild and reinserts after. The table is created on demand
  (`CREATE TABLE IF NOT EXISTS` on every `Index::open`), so existing indexes gain it
  **without a forced rebuild / version bump**. The *only* loss case is true file
  corruption (the file can't be read to copy rows out) — acceptable, since
  snapshots are non-mandatory analytics and the item files remain truth.
- **Layout:** `clove-core/src/stats.rs` (`StatsReport` + pure `compute`),
  `clove-index/src/stats_store.rs` (snapshots table + `Index::{record_snapshot,
  snapshot_history,snapshot_count}` + the `preserve_from`/`insert_raw` carry-over
  helpers used by reindex/recovery), `clove/src/cmd/stats.rs` (orchestration +
  human/JSON rendering). JSON schema `docs/json-schema/v1/stats.json` (validated in
  `tests/stats.rs`). `clove stats` is wired into `agent-doc` and DESIGN §7.2. No new
  files in `.clove/` and no new gitignore entries.
- **Tests:** 6 `clove-core` stats unit tests, 6 `clove-index` `stats_store` tests
  (incl. `full_reindex_preserves_snapshots`, the headline carry-over guarantee),
  5 `clove` e2e tests (schema, empty repo, snapshot→history, `--since`, `--top`).
  `cargo test --workspace`, `clippy -D warnings`, `fmt` all green.

**Decisions (don't relitigate):** stats history lives in the **one** `index.db`
(no separate `stats.db`); the index layer preserves the `snapshots` table across
reindex and schema-mismatch rebuilds, so the only loss case is raw file corruption.
This was a deliberate merge from an earlier two-file design (the rationale: one
database, simpler layout; perf is unaffected since `index.db` is opened for stats
only on `--snapshot`/`--history`). Snapshots are recorded manually via
`clove stats --snapshot` **and** automatically by a running daemon on a timer
(`[daemon] stats_snapshot_min`, default 60; `0` disables; `CLOVED_STATS_SNAPSHOT_MS`
overrides for tests) — the daemon computes the snapshot from the same file scan +
`compute_stats` path the CLI uses (`cloved/src/snapshot.rs`), so daemon and manual
snapshots are byte-identical. Analytics compute from files for correctness; no new
frontmatter fields, no index `user_version` bump (the `snapshots` table is
additive/idempotent).

## M4 — Incremental index & daemon graph (this session)

Made the index/daemon maintain the dependency graph's derived state incrementally
instead of "approximate-until-reindex" (evaluated first with a 3-agent team).

- **P0 — canonical toposort.** `clove_core::graph::topological_ranks_internal` now
  uses a deterministic Kahn sort (smallest-id-first tie-break) instead of
  petgraph's insertion-order-dependent `toposort`, making `topological_rank` a
  **pure function of `(hard edges, ids)`**. This is the prerequisite that lets the
  incremental path produce ranks byte-identical to a full reindex.
- **P1 — exact incremental derived state.** New `clove-index/src/derive.rs`
  reconstructs the graph from the index's own `items`/`edges` tables (no file
  re-scan), runs the same `GraphStore` the file path uses, and writes back exact
  `topological_rank` / `has_dangling_deps` / `excluded` — **delta-only** (only rows
  whose values changed, so the `idx_items_list` covering index isn't churned per
  batch). `apply_staleness` calls it in its transaction, so an incremental sweep
  now equals a full reindex. Fixes two latent bugs: reverse-dangling (dependents of
  a newly created/deleted item are refreshed) and the index `ready` not excluding
  hard-cycle / malformed-parent members. New **schema v4** column `items.excluded`;
  the SQL `ready` filters `excluded = FALSE`. Differential tests assert incremental
  == reindex for the derived columns (chain re-rank, dangling resolution, cycle).
- **P3 — daemon graph from the DB, not files.** `cloved`'s `graph_cache` rebuilds
  its hot `GraphStore` from `Index::graph_frontmatters` (two indexed table scans)
  instead of re-scanning + re-parsing every `.clove/issues/*.md` on each change.
  The watcher keeps the index exact+fresh before marking the cache dirty, so the
  DB-sourced graph is parity-identical to the file scan it replaces, far cheaper.
  `QUERY`/`SEARCH` inline refresh and `REINDEX` now also invalidate the hot graph.
- **Topology-change guard.** `apply_staleness` now runs the O(V+E)
  `recompute_derived` **only when the dependency structure changed** (an item
  added/deleted, or a changed item's edge/parent signature differs). A content-only
  edit — `status`/`title`/`assignee`/`priority`/`labels`, the common case —
  preserves its existing exact derived columns (snapshotted before the row
  overwrite) and skips the recompute entirely. `apply_staleness_tracked` returns
  whether the recompute ran; tests assert a status edit skips it (and stays
  byte-identical to reindex) while a dep edit triggers it.

**Decisions / scope:** the graph is **already persisted correctly** as the `edges`
adjacency table + the `topological_rank`/`has_dangling_deps`/`excluded` columns —
no transitive-closure table (write-storm for a rare query) and no graph engine;
SQLite stays the single store, cycles detected in-memory during the recompute.
Both the P1 recompute and the P3 daemon rebuild are O(V+E) **in-memory** passes
(fast: no file I/O / YAML parse), and the topology-change guard skips even that for
content-only edits. **Pearce–Kelly** online topological ordering (true sub-linear
O(affected-region) maintenance) was implemented and benchmarked in a standalone
harness (correctness verified vs. a reachability reference; ~0.2–1 µs per
invalidating edit vs. 0.5–68 ms for a full recompute at 10k–500k nodes), then
**rejected for clove**: PK's order is *history-dependent*, which structurally
breaks clove's canonical-order parity contract (the daemon, the index, and the
from-scratch file-scan path must all agree on `topological_rank`); PK also can't
represent cycles (clove must) and only wins at a scale clove rarely hits. The
guard is the correctness-preserving alternative. An always-correct hot graph now
unblocks future work (live ready-queue push / `SUBSCRIBE`, MCP "what's ready",
per-batch analytics).

## What clove is

A fast, git-native, **dependency-aware** work-item tracker for AI coding agents
and humans. **Plain Markdown + YAML-frontmatter files are the single source of
truth** (grep-able, diffable, travel with the repo). An **optional SQLite index**
and an **optional daemon** add speed/features but are never required. Written in
Rust as a single cross-platform binary.

Positioning: Beads-complete features + tk's plain-file fallback, **faster than
both**. It supersedes an earlier `git-bug` adoption exploration (see Provenance).

## Where everything is

| File | What |
|---|---|
| `docs/PRD.md` | Product vision / motivation (high level) |
| `docs/DESIGN.md` | **Authoritative, implementation-ready spec** (14 sections; read this first) |
| `docs/IMPLEMENTATION_PLAN.md` | Phased M0–M4, ~45 tasks (`T-*`) with files, deps, acceptance criteria |
| `docs/VERIFICATION_PLAN.md` | 5-layer test pyramid, benchmark gates, PRD-claim→test mapping (`V-*`) |

`DESIGN.md` supersedes the PRD's open-decision sketches. When the PRD and DESIGN
disagree, **DESIGN wins.**

## How to start building (do this next session)

1. `cd /Users/egeapak/Projects/personal/clove`, then `git init` (it is not a repo
   yet) and create the cargo workspace.
2. Read `docs/DESIGN.md` in full, then the **M0 section** of
   `docs/IMPLEMENTATION_PLAN.md`.
3. Build **M0 only first** — it is *strictly file-only* (no SQLite, no daemon).
   Work the tasks roughly in order: `T-I*` (workspace/CI/id/store scaffolding) →
   `T-C*` (model, FrontmatterWriter, parser, validation, store CRUD) →
   `T-G*` (graph engine) → `T-CLI*` (commands) → `T-CLI18` (`clove doctor`).
4. Use **TDD against each task's acceptance criteria** and the matching `V-*`
   entries in `VERIFICATION_PLAN.md`. Honor the **M0 Acceptance Gates** before
   moving to M1.
5. Mirror this repo's sibling conventions where sensible: a `cargo xtask` for
   build/bench tooling, `cargo clippy -D warnings`, fmt, and a CI matrix incl.
   **Windows** (clove must avoid bash/POSIX-only assumptions).

Consider driving the build with the `executing-plans` skill — the plan is
already written and task-structured.

## Key decisions already locked (don't relitigate without reason)

- **Files are truth; SQLite index is a rebuildable, gitignored cache; daemon is
  opt-in.** Correctness lives entirely in the file store.
- **Crate split:** `clove-core` (lib) / `clove-index` / `clove` (cli) / `cloved`
  (daemon) / `clove-import`.
- **IDs:** `<prefix>-<8 Crockford base32>` (e.g. `proj-7af3q2k9`); random suffix
  for merge-safety; validated by `CloveId` newtype.
- **Frontmatter:** read via `serde_yaml_neo`; write via a hand-rolled
  `FrontmatterWriter` (canonical field order, inline sorted lists, empty lists
  serialized as `[]` not omitted — needed for the 3-way merge driver).
- **Item file layout:** flat `.clove/issues/<id>.md`; comments are append-only
  files in a sibling `.clove/issues/<id>/comments/<ts>-<author>.md`.
- **`type` and `priority` are first-class validated fields** (type enum
  bug/feature/chore/docs/epic; priority 0–4, 0=highest, default 2). **`area` is a
  label convention** (`area:core`), not a field.
- **Labels are case-insensitive** — canonicalized via `normalize_label()`
  (lowercase + trim + collapse whitespace + reject empty) on every write and in
  filters; stored values are always canonical.
- **Dependency engine:** petgraph `StableDiGraph`, five edge kinds (DependsOn /
  ParentOf / Relates / Duplicates / Supersedes). `ready` = open with all hard
  deps closed and no dangling; `blocked` otherwise; sorted `(priority, topo
  rank)`. Per-item dep array capped at `MAX_DEP_ARRAY_LEN`; total graph size
  uncapped. `dep add` rejects self-loops (exit 4) and cycles (exit 3).
- **`clove doctor`** (T-CLI18, M0) — store health check: parse failures,
  id/filename mismatch, duplicate ids, invalid fields, dangling refs, cycles,
  bad parents, non-canonical labels, unsorted/dup lists, orphaned comment dirs,
  bad config. `--fix` does only safe repairs (labels/lists/orphans), never
  structural; `--strict` → exit 4. Index-divergence check added in M1 (T-S08).
- **JSON envelope** `{ v, ok, data, _meta }` on every response; **exit codes
  0–7** (see DESIGN §7.6).
- **UI:** terminal only for now (tables + `cargo tree`-style `dep tree` + JSON).
  TUI/web are **M4 and not yet designed.**

See `DESIGN.md §14` for how expert conflicts were resolved (ID length, parser,
comment layout, empty-array serialization, cycle exit codes).

## Open / deferred (decide when you get there)

- **M4 in progress:** `clove stats` (+ durable history, daemon auto-snapshot) and
  the exact-incremental index/daemon graph are **done** (see the M4 sections
  above). The **remaining** M4 items are undesigned (TUI, web UI, bidirectional
  vendor bridges, richer changelog) — plan each in its own session.
- **Vendor bridges** (GitHub/GitLab/Jira) are documented for import/export, not
  built in M0–M3.
- **Optional soft dep cap** (warn past N deps/item) — offered but not added; add
  only if wanted.
- **crates.io name** `clove` was free on 2026-06-02 — **re-verify** (crates.io +
  GitHub + Homebrew) before first publish.

## Conventions

- **Commits:** always `git commit --no-gpg-sign`. End commit bodies with the
  `Co-Authored-By: Claude ...` trailer used in the author's other repos.
- Implement features fully (no stubs); remove temp files; keep the three plan
  docs internally consistent if you change scope.

## Provenance (how this plan was produced)

This session (in the `hn-reader` repo) explored vendor-neutral trackers, rejected
git-bug (no dependencies; issues not plain files), tk (great model but young
bash), and Beads (powerful but heavy daemon), then chose to build `clove`. The
PRD was hand-written, then expanded by a **13-agent workflow** (10 domain experts
→ synthesis → adversarial critique → revision; 16 critique issues fixed), then
manually refined to add case-insensitive labels, dependency-cap clarifications,
and `clove doctor`, and finally consistency-reviewed (fixed a §7.7 section-number
collision).

Superseded artifact (for reference only): the git-bug adoption spec at
`hn-reader:docs/superpowers/specs/2026-06-02-work-item-tracker-git-bug-design.md`
on branch `claude/work-item-tracker`.
