---
schema: 1
id: clov-04504BWS
title: list_rows builds the dependency graph for sorts that read no rank
status: closed
type: chore
priority: 2
created: 2026-11-14T19:16:05.134Z
updated: 2026-11-14T19:16:05.309Z
closed: 2026-11-14T19:16:05.309Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/ops.rs list_rows calls list_rows_with_graph and drops the graph. For --sort priority, created, updated, id, status, or type, the whole-store graph build and the topological ranks are not used.
Expected: list_rows scans, filters, and sorts without a graph when Order::needs_ranks() is false, and returns the same rows.
Done when: ops test list_rows_without_ranks_matches_the_graph_path passes.