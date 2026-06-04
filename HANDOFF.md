# clove — Session Handoff

**Updated:** 2026-06-03
**State:** **M0, M1, M2, and M3 are complete and gated.** Full CLI command surface;
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
is **per-project** (one per resolved `.clove/`, reachable from any subdir; worktrees
sharing one `.clove/` share one daemon) — a system-wide daemon was evaluated and
rejected for v1 (DESIGN §8.1). **Idle self-shutdown now defaults to 30 min** so idle
daemons don't linger (`0` = never; `CLOVED_IDLE_SHUTDOWN_MS` overrides for tests). All five JSON schemas
published + validated. Perf/parity/fuzz/golden gates pass (M0
`docs/IMPLEMENTATION_PLAN.md`, M1 `docs/M1_ACCEPTANCE_GATES.md`, M2
`docs/M2_ACCEPTANCE_GATES.md`, M3 `docs/M3_ACCEPTANCE_GATES.md`). Tests green
except one environment-only failure (`repo::tests::linked_worktree…`, a sandbox
git-signing artifact, not a code defect; the token-gated `github_roundtrip`
shows as `1 ignored`).
**Next step:** **M4 — Extras** (TUI/web UI, vendor bridges, richer history, and the
deferred `clove stats` analytics command — see `IMPLEMENTATION_PLAN.md` M4 backlog).
Still undesigned.

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

- **M4 is undesigned** (TUI, web UI, bidirectional vendor bridges, richer
  changelog) — plan it in its own session after M3.
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
