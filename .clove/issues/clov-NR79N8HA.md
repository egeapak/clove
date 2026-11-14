---
schema: 1
id: clov-NR79N8HA
title: dep add and parent scan every item file to validate one edge
status: closed
type: bug
priority: 2
created: 2026-11-14T19:23:21.344Z
updated: 2026-11-14T19:24:40.923Z
closed: 2026-11-14T19:24:40.923Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/ops.rs dep_add and set_parent call scan_or_fail, which parses the frontmatter of every item file under the store lock, to answer a question about one item: is id reachable from dep_id over deps (dep_add), or is id an ancestor of the new parent (set_parent). On a 10,000-item store each call parses 10,000 files. An unparseable file anywhere in the store also blocks both commands, although only a file on the walked path can hide a cycle.
Expected: both ops read only the items they walk (the forward deps closure of dep_id; the parent chain of the new parent), fail with ScanFailed when a walked file does not parse, and fall back to the whole-store scan when the walk passes LOCAL_CLOSURE_BUDGET items.
Done when: clove-core tests show that dep_add and set_parent still reject cycles, succeed with an unrelated unparseable file in the store, and fail with ScanFailed when the unparseable file is on the walked path.