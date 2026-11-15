---
schema: 1
id: clov-VE64ZNRV
title: Tests for doctor --fix through a running daemon and dep tree fallback
status: closed
type: chore
priority: 2
created: 2026-11-15T07:01:15.755Z
updated: 2026-11-15T07:03:58.073Z
closed: 2026-11-15T07:03:58.073Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
doctor --fix rebuilds the index daemon-first (cmd/doctor.rs) and dep tree falls back to the file scan when the daemon graph lacks the item (cmd/dep.rs). Neither has a test with a daemon whose index and graph miss a write.
Done when: crates/clove/tests/daemon_cli.rs has doctor_fix_rebuilds_the_index_through_the_running_daemon and dep_tree_of_an_item_the_daemon_lacks_reads_the_files, both passing.