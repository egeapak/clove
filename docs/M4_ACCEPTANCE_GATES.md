# M4 Acceptance Gates — status

M4 ("Extras") is not pre-specified with G-numbered gates the way M1–M3 were
(`IMPLEMENTATION_PLAN.md` records the M4 backlog rather than a fixed task list).
This file gates the M4 items that have **landed**: `clove stats` (analytics +
durable history + daemon auto-snapshot) and the exact-incremental index/daemon
graph. The remaining backlog (TUI/web UI, bidirectional vendor bridges, richer
changelog) is undesigned and out of scope here.

All gates pass: `cargo test --workspace` (39 test binaries), `clippy -D warnings`,
and `cargo fmt --check` are clean.

## Gate table

| Gate | Assertion | Test | Status |
|------|-----------|------|--------|
| M4-G01 | `clove stats` JSON validates against the v1 schema (counts, ready/blocked/excluded/dangling, cycles, epics, throughput, daemon + index telemetry) | `clove/tests/stats.rs::stats_json_validates_against_schema` | ✅ |
| M4-G02 | `--snapshot` persists into the index DB; `--history [--since][--limit]` replays it | `clove/tests/stats.rs::{snapshot_persists_and_history_reads_back,history_since_filters_by_timestamp}` | ✅ |
| M4-G03 | Snapshot history survives a full `reindex` and a schema-mismatch rebuild (only true corruption loses it) | `clove-index/src/stats_store.rs::{full_reindex_preserves_snapshots,reopen_preserves_history,schema_mismatch_rebuild_preserves_history}` | ✅ |
| M4-G04 | `--since` accepts both `Z` and `+00:00` RFC3339 forms (canonical-comparison fix) | `clove-index/src/stats_store.rs::history_orders_desc_and_filters_since` | ✅ |
| M4-G05 | A running daemon auto-records snapshots on `[daemon] stats_snapshot_min` | `cloved/tests/daemon_watch.rs::daemon_auto_snapshots_on_interval`; `cloved/src/snapshot.rs` units | ✅ |
| M4-G06 | Toposort rank is a pure function of `(edges, ids)` — independent of insertion order | `clove-core/src/graph.rs::canonical_ranks_independent_of_insertion_order` | ✅ |
| M4-G07 | Incremental `apply_staleness` produces derived columns (rank / dangling / excluded) byte-identical to a full reindex, across add / re-dep / delete / cycle / dangling-resolution | `clove-index/src/derive.rs::{incremental_apply_matches_full_reindex,incremental_cycle_matches_reindex}`; `clove-index/tests/incremental.rs::multi_edit_sequence_matches_reindex` | ✅ |
| M4-G08 | SQL `ready` excludes hard-cycle / malformed-parent members (incl. a closed cycle member) — parity with `GraphStore::ready_items` | `clove-index/tests/incremental.rs::index_ready_excludes_cycle_with_closed_member`; `clove-core/src/graph.rs::excluded_ids_covers_cycle_and_malformed_parent` | ✅ |
| M4-G09 | Topology-change guard: a content-only edit skips the recompute yet stays exact; a structural edit triggers it | `clove-index/src/stale.rs::{status_only_edit_skips_recompute_and_stays_exact,dep_edit_triggers_recompute_and_matches_reindex,new_item_triggers_recompute}` | ✅ |
| M4-G10 | Daemon's DB-sourced graph reproduces the file-scan graph (ready/blocked/cycles/ranks/excluded) | `clove-index/tests/incremental.rs::graph_frontmatters_reproduces_file_graph`; `clove/tests/daemon_routing.rs` (parity) | ✅ |
| M4-G11 | All M0+M1+M2+M3 gates still pass | full `cargo test --workspace` | ✅ |

## Design decisions (see `HANDOFF.md` for rationale)

- **One SQLite database.** Stats history lives in a `snapshots` table inside
  `index.db`, not a separate file; the index layer carries it across the cache's
  destructive operations so it is durable.
- **Exact, not approximate.** A canonical Kahn toposort + a DB-sourced
  `recompute_derived` make the incremental path match a full reindex, replacing
  the old "ranks unknown until reindex" approximation.
- **Pearce–Kelly rejected.** Online O(region) topological maintenance was
  implemented and benchmarked, then rejected: its order is history-dependent,
  which breaks clove's canonical-order parity contract (daemon, index, and the
  from-scratch file-scan path must agree), and it can't represent cycles. The
  topology-change guard is the parity-preserving optimization adopted instead.
