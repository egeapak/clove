---
schema: 1
id: clov-Y8VYEJJT
title: CI never asserts the index performance gates
status: closed
type: chore
priority: 2
created: 2026-11-14T19:12:33.094Z
updated: 2026-11-14T19:19:00.847Z
closed: 2026-11-14T19:19:00.847Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-index/tests/index_perf_gates.rs asserts its budgets (reindex, ls, ready, search, staleness) only when debug_assertions is off. The perf-gates job in .github/workflows/ci.yml runs only `cargo test --release -p clove-core --test perf_gates`, and the test job is a debug build, so no CI job can fail on an index budget.
Done when: the perf-gates job also runs `cargo test --release -p clove-index --test index_perf_gates`.