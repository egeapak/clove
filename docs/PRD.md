# clove — Product Requirements Document

- **Status:** Draft for review (build planned for a later session)
- **Date:** 2026-06-02
- **Owner:** Ege Apak
- **Working name:** `clove` (a clove-hitch knot — items tied to each other; verified free on crates.io 2026-06-02)

## 1. One-liner

`clove` is a fast, git-native, dependency-aware work-item tracker for AI coding
agents and humans. **Plain Markdown + YAML-frontmatter files are the source of
truth** (grep-able, diffable, travel with the repo); an **optional SQLite index**
and an **optional daemon** add speed and richer features without ever becoming
required.

## 2. Motivation

Work tracking for agent-driven, multi-language repos has no good vendor-neutral
option:

- **git-bug** — git-native and portable, but **no dependencies**, and issues
  live in git refs (not plain files), so agents can't grep them.
- **tk (ticket)** — great model (markdown+frontmatter in `.tickets/`, real
  dependency graph, `ready`/`blocked`), plain files, but it's a young single-
  maintainer **bash** script and not built for speed at scale.
- **Beads (bd)** — the richest feature set (dependency types, `bd ready`, agent
  memory) and mature, but **heavy**: a background daemon, SQLite/Dolt, and
  growing complexity/slowness.

`clove` targets the gap: **Beads-class features, tk's plain-file fallback,
faster than both**, as a single cross-platform Rust binary. The durable asset is
the *file format*; the index and daemon are accelerators layered on top, so
there's no lock-in and no mandatory moving parts.

## 3. Goals / non-goals

**Goals**

- First-class **dependency graph**: `depends-on`/`blocks`, parent/child
  (epics/subtasks), and soft relations (`relates`, `duplicates`, `supersedes`),
  with `ready` (unblocked) / `blocked` queries and cycle detection.
- **Plain files as the single source of truth** — one Markdown+frontmatter file
  per item; everything else is derivable.
- **Agent-first ergonomics** — stable JSON output on every read, predictable
  schema, clean exit codes, machine-greppable files.
- **Speed** — common operations materially faster than tk and Beads (target
  in §10).
- **Cross-language / cross-project** — one binary, `clove init` in any repo
  regardless of language; no runtime deps required for the core.
- **No lock-in** — files are git-versioned and portable; importers/exporters and
  (later) vendor bridges make migration in/out trivial.
- **Optional acceleration** — SQLite index and a daemon are opt-in features that
  never change correctness, only speed/extras.

**Non-goals (v1)**

- Hosted multi-tenant server, auth, real-time collaboration.
- A required daemon or required database.
- Replacing full project-management suites (no Gantt, sprints, billing).
- Mobile apps.

## 4. Target users

- **AI coding agents** (primary) — Claude Code et al., which need persistent,
  queryable, dependency-aware memory of work across sessions.
- **Solo developers and small teams** working across Rust/Swift/Python/other
  repos who want one in-repo tracker.

## 5. Guiding principles

1. **Files are truth; the index is a cache.** The SQLite index is fully
   rebuildable from the files and is `.gitignore`d. Deleting it loses nothing.
2. **Nothing required but the binary and the files.** Index and daemon are
   optional; the CLI works correctly (just slower on big sets) with neither.
3. **Agent-first, human-friendly.** JSON for machines, readable Markdown for
   people; the same data.
4. **Merge-friendly by construction.** File-per-item, stable field ordering, and
   an append-friendly comment model minimize git conflicts.
5. **Fast by default.** Rust, zero-copy where sensible, lazy/incremental
   indexing; no per-command process sprawl.
6. **Unix philosophy with taste.** Composable commands and pipes, but with the
   quality-of-life features (graph queries, search) that make it pleasant.

## 6. Data model

### Item

| Field | Type | Notes |
|---|---|---|
| `id` | string | Short, stable, collision-resistant, merge-safe (see §6.3) |
| `title` | string | |
| `status` | enum | `open` \| `in_progress` \| `closed` |
| `type` | enum | `bug` \| `feature` \| `chore` \| `docs` \| `epic` (extensible) |
| `priority` | int | 0–4, 0 = highest |
| `labels` | string[] | free-form, `key:value` convention encouraged (`area:ios`) |
| `assignee` | string? | |
| `deps` | id[] | hard dependencies — this item **depends on** these (the DAG) |
| `parent` | id? | epic/subtask hierarchy |
| `relates` / `duplicates` / `supersedes` | id[] | soft relations |
| `created` / `updated` | RFC3339 | |
| `closed` | RFC3339? | |
| body | Markdown | the file body below the frontmatter |
| comments | see §6.2 | discussion/history |

`blocked` is **derived** (an item is blocked if any `deps` item is not
`closed`); `ready` = `open`/`in_progress` with all `deps` closed.

### 6.1 File layout

```
.clove/
  config.toml             # repo-level config (id prefix, defaults) — committed
  issues/
    <id>.md               # one item per file — committed (source of truth)
  index.db                # SQLite derived cache — .gitignore'd, optional
  .gitignore              # ignores index.db (+ daemon socket/pid)
```

Example `.clove/issues/clove-7af.md`:

```markdown
---
id: clove-7af
title: Article image download + compression
status: open
type: feature
priority: 1
labels: [area:core]
assignee: null
deps: [clove-3k2]
parent: null
relates: []
created: 2026-06-02T10:00:00Z
updated: 2026-06-02T10:00:00Z
---

Save compressed versions of images the readability crate kept. Design:
docs/plan-article-image-download.md
```

### 6.2 Comments / history

**Decision to finalize in planning:** prefer **append-only comment files**
(`.clove/issues/<id>/comments/<ts>-<author>.md` or a `comments/` sibling) over
inlining comments in the item file, so concurrent comments don't conflict in
git. Item edits (status/label/dep changes) are captured by the file's git
history; an optional structured changelog is a later enhancement, not core.

### 6.3 IDs

Requirements: short, human-quotable, stable, collision-resistant across parallel
branches/agents, and merge-safe. **Proposed:** `<prefix>-<base32 random>` (e.g.
`clove-7af`), prefix from `config.toml` (default derived from repo name). Random
suffix (not sequential) avoids the merge collisions sequential numbering causes
when two branches both add items. Exact length/alphabet decided in planning.

## 7. Architecture

```
            ┌──────────────────────────────────────────┐
            │  clove CLI  (also a Rust library crate)   │
            └───────────────┬──────────────────────────┘
                            │
          ┌─────────────────┴───────────────────┐
          │                                      │
   ┌──────▼───────┐                     ┌────────▼─────────┐
   │ File store   │  source of truth    │ SQLite index     │  optional, derived
   │ .clove/issues│ ───── rebuilds ────▶│ FTS5 + graph     │  (.gitignore'd)
   │ *.md         │                     │ fast queries     │
   └──────────────┘                     └────────▲─────────┘
                                                  │ keeps fresh
                                        ┌─────────┴─────────┐
                                        │ daemon (optional) │  file-watch,
                                        │ incremental index │  optional git sync
                                        └───────────────────┘
```

- **Core crate** (`clove-core`): file store read/write, the item model, the
  dependency graph (topological `ready`/`blocked`, cycle detection), querying.
  Pure, no DB required — operates by scanning/parsing files.
- **Index** (`clove-index`): an optional SQLite (FTS5) cache mirroring the files
  for fast `ls`/`query`/`search`/graph ops at scale. Built/refreshed lazily on
  command, or continuously by the daemon. Always rebuildable via `clove reindex`.
- **CLI** (`clove`): thin shell over the core + index; JSON everywhere.
- **Daemon** (`cloved`, optional): watches `.clove/issues/`, keeps the index
  incrementally fresh, and (opt-in) can auto-`git` sync. Never required; the CLI
  detects and uses it if running, else does a direct/indexed read itself.

Correctness lives entirely in the file store. The index/daemon only make it
faster.

## 8. CLI surface (initial)

```
clove init                         # create .clove/, config, gitignore
clove new "title" [-t type -p N -l label -d dep ...]
clove show <id> [--format json]
clove edit <id>                    # $EDITOR on the file
clove status <id> open|start|close ; clove start|close <id>
clove label <id> add|rm <label> ; clove assign <id> <who> ; clove priority <id> <N>

clove dep add <id> <dep-id> ; clove dep rm <id> <dep-id>
clove dep tree <id> [--full]       # dependency tree
clove dep cycle                    # detect cycles among open items

clove ready  [filters]             # unblocked open/in_progress items
clove blocked [filters]            # items with unresolved deps
clove ls|list [--status --type --label --assignee --priority]
clove query [--format json] [expr] # structured query; JSON for agents
clove search <text>                # full-text (uses FTS index if present)

clove reindex                      # rebuild the SQLite cache from files
clove import beads|tk|github <src> # migrate in
clove export json|jsonl|github     # migrate out
clove daemon start|stop|status     # optional
clove version
```

Every read command supports `--format json` (default human). JSON schema is
versioned and stable.

## 9. Agent integration

- **JSON-first:** `clove ready --format json`, `clove query --format json`,
  `clove show <id> --format json` — stable, documented schema; non-zero exit on
  error with a JSON error body.
- **Plain files:** agents may also read `.clove/issues/*.md` directly (grep,
  diff) without the binary — the fallback that git-bug lacked.
- **Onboarding:** `clove init` writes a short `CLAUDE.md`/`AGENTS.md` snippet (or
  `clove agent-doc` prints one) describing the workflow: pull (normal `git`),
  `clove ready` to pick work, update status/deps via `clove`, commit + push the
  changed files like any code change.
- **No special sync:** because items are committed files, they travel with
  normal `git push`/`pull`. No `git bug push` equivalent, no refspec wiring.

## 10. Performance targets

`clove` must be visibly faster than tk (bash) and Beads (daemon) for everyday
operations. Indicative targets (to be benchmarked, methodology defined in the
plan):

- Cold, no index, ~1,000 items: `ls`/`ready` complete in well under ~100 ms.
- With index, ~10,000 items: common queries and `search` under ~10 ms.
- `new`/`show`/`status` feel instant (single-file I/O; no daemon round-trip).
- No always-on process required to be fast for interactive use.

## 11. Sync & merge model

- Items are committed files → standard `git push`/`pull` syncs them.
- Conflict minimization: file-per-item, deterministic frontmatter field order,
  sorted list fields, and append-only comment files (§6.2).
- When a frontmatter merge conflict does occur, it's a normal small text
  conflict in one item file; `clove` may later offer a `clove resolve` helper
  (post-v1).

## 12. Phasing / milestones

- **M0 — MVP (file core, no DB):** schema + file store, `init/new/show/edit/
  status/label/assign/priority`, `dep add|rm|tree|cycle`, `ready/blocked/ls/
  query`, JSON output. Already surpasses git-bug (deps) and matches tk's model,
  in fast Rust. **This is the milestone that proves the concept.**
- **M1 — SQLite index:** optional FTS5 + graph cache, `reindex`, lazy refresh;
  `search`. Speed at scale.
- **M2 — Interop:** `import beads|tk|github`, `export json|jsonl|github`; agent
  onboarding doc generation.
- **M3 — Daemon (optional):** file-watch incremental indexing; opt-in git
  auto-sync.
- **M4 — Extras:** TUI and/or web UI; bidirectional vendor bridges
  (GitHub/GitLab/Jira); richer history/changelog.

## 13. Distribution

- Single binary; `cargo install clove`, GitHub release binaries, and a Homebrew
  tap (later). CI downloads a pinned binary.
- `clove-core` published as a library so other Rust tools/agents can embed it.
- Cross-platform (macOS/Linux first; Windows supported by avoiding bash/POSIX-
  only assumptions — a concrete advantage over tk).

## 14. Risks & open questions (resolve in planning)

- **ID scheme** — exact length/alphabet; collision probability vs. brevity.
- **Comment storage** — append-only files vs. structured log; finalize §6.2.
- **Index invalidation** — how the CLI detects a stale index (mtime/hash) and
  whether to auto-refresh vs. require `reindex`.
- **Merge ergonomics** — validate the conflict story on real parallel-branch
  edits.
- **Body vs. metadata** — keep the long description in the item file vs. linking
  out to `docs/` for big designs (lean: short body in-file, link big designs).
- **Scope creep guard** — keep M0 strictly file-only; resist pulling the index/
  daemon forward.
- **Name** — final crates.io/GitHub/Homebrew availability re-check before
  publishing (`clove` free on crates.io as of 2026-06-02).

## 15. Success criteria

- A real repo (e.g. `hn-reader`) adopts `clove`, migrates its open items, and
  agents drive work via `clove ready` / status updates with no vendor service.
- Dependency queries (`ready`/`blocked`/`dep tree`) are correct and instant.
- Benchmarks beat tk and Beads on the §10 operations.
- Everything works with only the binary + committed files; index/daemon add
  speed but are never required.

## 16. Relationship to the prior exploration

This supersedes the earlier `git-bug` adoption spec (in `hn-reader`'s
`docs/superpowers/specs/`). The decisive factors: git-bug has no dependency
graph and stores issues in git refs (not grep-able files); tk has the right
model but is bash and young; Beads is powerful but heavy. `clove` keeps tk's
file model and Beads' feature ambitions, in fast Rust, with optional
acceleration. The format is the portable asset, so adopting `clove` later in
any repo (including ones currently on tk's `.tickets/` format, via `import tk`)
is low-friction.
