---
schema: 1
id: clov-Q7XVCJQ5
title: 'cloved: an in-place save of an item file is not indexed until another file is created or renamed'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:42.702Z
updated: 2026-11-14T19:33:50.822Z
closed: 2026-11-14T19:33:50.822Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/reindexer.rs sync_once runs check_staleness_fast for each watcher batch. An editor that writes in place (VS Code, nano) changes neither the mtime of .clove/issues nor its file count, so the check returns clean and the batch's own paths (crates/cloved/src/watcher.rs) are used only for git sync. ls/ready/blocked from the daemon then return the old row while show returns the new file.
Done when: crates/cloved/tests/daemon_watch.rs::an_in_place_save_reaches_the_daemon passes.