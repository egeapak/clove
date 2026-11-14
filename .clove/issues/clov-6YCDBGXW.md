---
schema: 1
id: clov-6YCDBGXW
title: dep_tree sorts the ready list of the whole graph to flag the nodes of one tree
status: closed
type: bug
priority: 2
created: 2026-11-14T19:26:14.047Z
updated: 2026-11-14T19:26:58.928Z
closed: 2026-11-14T19:26:58.928Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/graph.rs GraphStore::dep_tree calls ready_items() and collects it into a HashSet<CloveId> only to fill DepTreeNode.ready for the nodes it visits. ready_items() computes topological ranks for the whole graph, sorts every ready item, and clones every ready id, so a dep tree of three nodes on a 10,000-item graph pays for all of them.
Expected: dep_tree tests readiness per visited node with the same predicate ready_items() uses, and computes no ranks and no sort.
Done when: a clove-core test shows that every DepTreeNode.ready equals membership in ready_items() on a graph with a closed dep, an open dep, a dangling dep, and a cycle.