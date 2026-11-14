---
schema: 1
id: clov-CNCQ6GGQ
title: close warning, dep tree, and stats scan every item file when a fresh index exists
status: open
type: feature
priority: 2
created: 2026-11-14T19:23:21.367Z
updated: 2026-11-14T19:23:21.367Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/status.rs warn_open_dependents calls store.scan_frontmatter() after every close to find open items that depend on the closed id. CLI dep tree and stats also build the whole graph from a file scan. The index has the same data: edges(to_id, kind) with idx_edges_to answers the dependents query, and items/edges hold the graph.
Expected: clove-engine gains a dependents method (index tier when it can be refreshed, file scan otherwise) that warn_open_dependents calls; dep tree and stats get the same daemon, index, files cascade.
Done when: tier_parity.rs asserts the index answer equals the file answer for the dependents query, and close on a store with an index reads no item file except the closed one.