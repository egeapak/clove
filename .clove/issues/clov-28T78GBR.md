---
schema: 1
id: clov-28T78GBR
title: clove dep tree reports not found when the daemon graph lacks an existing item
status: closed
type: bug
priority: 2
created: 2026-11-14T19:34:31.130Z
updated: 2026-11-14T19:44:50.354Z
closed: 2026-11-14T19:44:50.354Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/dep.rs tree(): when a running daemon answers GraphRequest::Tree with no node, the command exits 2 (not found), although the item file exists (the command checks store.exists first). dep tree and dep cycle also ignore --deep and ask the daemon.
Expected: a daemon answer with no node falls through to the file scan; --deep skips the daemon, as in clove-engine.
Done when: the dep suites pass and cmd::dep::run takes the deep flag.