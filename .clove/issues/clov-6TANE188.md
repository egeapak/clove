---
schema: 1
id: clov-6TANE188
title: 'cloved: the file watch stops when .clove/issues is removed and created again, and the daemon keeps answering from the old index'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:42.793Z
updated: 2026-11-14T19:33:50.951Z
closed: 2026-11-14T19:33:50.951Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/watcher.rs puts the OS watch on the issues directory inode. After git checkout across a commit without the store, or git stash -u, the directory is a new one: no event arrives, the watcher task waits forever, the state stays Watching, and crates/cloved/src/ipc.rs skips the inline refresh above 20 changed items. Daemon reads stay stale until a restart.
Done when: crates/cloved/tests/daemon_watch.rs::a_recreated_issues_directory_is_not_answered_from_the_old_one passes.