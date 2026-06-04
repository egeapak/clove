# clove

A fast, git-native, **dependency-aware** work-item tracker for AI coding agents and humans.

Plain Markdown + YAML-frontmatter files under `.clove/issues/` are the **single source
of truth** — grep-able, diffable, and they travel with the repo. An optional SQLite
index and an optional background daemon add speed and features but are never required:
delete them and nothing is lost. Written in Rust as a single cross-platform binary.

## Status

M0–M3 complete and gated; M4 in progress (`clove stats` + analytics history, and an
exact-incremental index/daemon graph). See `HANDOFF.md` for the current state and
`docs/` for the full design.

## Build & test

```sh
cargo build --release          # binaries: clove (CLI), cloved (daemon)
cargo test --workspace         # unit + integration + doctests
cargo clippy --workspace --all-targets -D warnings
```

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

Every command supports `--format json|jsonl` with a stable `{ v, ok, data, _meta }`
envelope and documented exit codes — see `clove agent-doc` for the agent-facing surface.

### Optional accelerators

```sh
clove reindex                 # build/refresh the SQLite index (.clove/index.db, gitignored)
clove daemon start            # watch files, keep the index + graph hot, serve reads over IPC
clove stats --snapshot        # record an analytics history point
clove stats --history         # replay recorded snapshots (a running daemon also auto-records)
```

### Interop

```sh
clove import tk|beads|github <src>      # import from other trackers
clove export json|jsonl|github          # export
clove init --merge-driver               # install the 3-way git merge driver for item files
```

## Layout

| Path | What |
|------|------|
| `crates/clove-core` | model, file store, dependency-graph engine, IDs (pure; no SQLite) |
| `crates/clove-index` | optional SQLite index (FTS5, staleness, incremental derived state, stats history) |
| `crates/clove` | the `clove` CLI |
| `crates/cloved` | the optional `cloved` daemon |
| `crates/clove-import` | import/export + merge driver |
| `crates/clove-ipc` | CLI↔daemon wire protocol |
| `docs/DESIGN.md` | authoritative, implementation-ready spec |
| `docs/IMPLEMENTATION_PLAN.md` | phased M0–M4 task plan |
| `docs/*_ACCEPTANCE_GATES.md` | per-milestone acceptance gates |

## License

MIT OR Apache-2.0.
