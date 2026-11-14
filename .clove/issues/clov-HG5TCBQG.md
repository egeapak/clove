---
schema: 1
id: clov-HG5TCBQG
title: 'cloved: a starting hub leaves the previous hub''s hub.pid in place until it writes its own'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:43.152Z
updated: 2026-11-14T19:33:51.330Z
closed: 2026-11-14T19:33:51.330Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/lifecycle.rs run removes a corpse socket after taking hub.lock but leaves a corpse hub.pid until write_pid. In that window clove daemon stop (crates/clove/src/cmd/daemon.rs stop_hub) sees the lock held, reads the old pid, and can signal an unrelated process that reused it.
Done when: the pid file of a dead hub is removed as soon as the new hub holds the lock, and lifecycle::tests::taking_the_lock_discards_a_dead_hubs_pid_file passes.