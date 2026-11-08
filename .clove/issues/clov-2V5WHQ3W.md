---
schema: 1
id: clov-2V5WHQ3W
title: clove dep tree says 'no item' for an item written a moment earlier while cloved serves the project
status: open
type: bug
priority: 2
created: 2026-11-08T12:36:31.795Z
updated: 2026-11-08T12:36:31.795Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
With cloved serving a project, `clove dep tree <id>` reads the daemon's cached graph (crates/clove/src/cmd/dep.rs dep_tree_via_daemon). That graph lags the writes the same daemon applies, while `ls`/`ready`/`blocked` through the daemon are already current.

Repro (0.2.0 release binaries and master behave the same):
    clove daemon start
    F=$(clove -f json new T | jq -r .data.id); clove dep tree $F
    -> error: no item with id `<F>`   (exit 1)
`clove dep tree --no-index $F` prints the item. After about 200 ms the daemon answer is correct. The same lag shows after `clove dep add A B`: the next `clove dep tree A` omits the new edge.

Expected: a dep tree read after a write through the daemon reflects that write (read-your-writes), like the list reads do.
Done when: a test in crates/clove/tests that runs new/dep add then dep tree against a running cloved, with no sleep, passes.