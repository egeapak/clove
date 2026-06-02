# clove — Session Handoff

**Updated:** 2026-06-02
**State:** Planning complete and reviewed. **Not yet built. Not yet a git repo.**
**Next step:** Start the M0 build from `docs/IMPLEMENTATION_PLAN.md`.

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
