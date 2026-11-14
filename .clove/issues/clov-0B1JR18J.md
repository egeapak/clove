---
schema: 1
id: clov-0B1JR18J
title: 'cloved: every write makes daemon writes and the next reads cost a pass over the whole store'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:42.827Z
updated: 2026-11-14T19:33:50.976Z
closed: 2026-11-14T19:33:50.976Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
check_staleness_fast (crates/clove-index/src/stale.rs) short-circuits only when the directory mtime is at least 2 s old, and every item write renames a file into .clove/issues. crates/cloved/src/ipc.rs after_write, each watcher batch, and each query refresh therefore run the full check_staleness: a stat of every item file and a SELECT of every index row, under the index mutex. A daemon-side create costs O(store) and an import of M items costs O(M x store).
Done when: after_write, the watcher batch and the query refresh check only the files that changed (clove_index::check_staleness_of), and crates/cloved fresh::tests::a_sync_checks_only_the_reported_files passes.