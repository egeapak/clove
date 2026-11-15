---
schema: 1
id: clov-34M16WEX
title: DependencyCycle error lists the rejected edge, not the cycle
status: closed
type: bug
priority: 2
created: 2026-11-14T19:11:42.090Z
updated: 2026-11-14T19:11:42.188Z
closed: 2026-11-14T19:11:42.188Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/ops.rs dep_add builds DependencyCycle { cycle: [id, dep_id] }. With a -> b -> c, 'dep add c a' reports the cycle 'c -> a', which is the rejected edge. The cycle is c -> a -> b -> c.
Expected: cycle is the closed path [from, to, ..., from], the shape DESIGN.md gives for 'dep cycle'.
Done when: graph::tests::would_cycle_path_is_the_closed_chain passes and the ops dep_add test asserts the path.