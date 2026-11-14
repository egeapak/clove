---
schema: 1
id: clov-Q4BYAXS7
title: 'cloved: after index.db is replaced by another process the daemon keeps using the removed file'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:42.978Z
updated: 2026-11-14T19:33:51.166Z
closed: 2026-11-14T19:33:51.166Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
A clove reindex that falls back to a local rebuild (crates/clove/src/cmd/reindex.rs) replaces .clove/index.db while the daemon's connection stays on the old inode. The daemon's later applies and its hourly stats snapshots (crates/cloved/src/snapshot.rs) go into a file that no longer has a name.
Done when: the daemon reopens index.db when the file identity changed, and crates/cloved/tests/daemon_watch.rs::the_daemon_follows_an_index_replaced_under_it passes.