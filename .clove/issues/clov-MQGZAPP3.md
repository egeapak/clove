---
schema: 1
id: clov-MQGZAPP3
title: Staleness check stores a directory mtime newer than its file scan, hiding an edit that races an index read
status: closed
type: bug
priority: 2
created: 2026-11-14T19:14:10.879Z
updated: 2026-11-14T19:15:45.367Z
closed: 2026-11-14T19:15:45.367Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-index/src/stale.rs check_staleness runs scan_dir (a stat of every item file) and then reads the directory mtime, and apply_staleness stores that mtime as the meta oracle. If another process renames a new version of item X into place after X was stat-ed and before the directory mtime is read, the report does not contain X but the stored oracle is already the post-edit mtime. Once the directory is older than 2 s, check_staleness_fast matches and returns clean, and the old row of X is served until the next create or rename in issues/. On a 20k-item store, a concurrent 'clove ls' and 'clove edit' left the old title in the index in 7 of 30 trials.
apply_staleness has the same order per file: it reads the file and then stats it, so a rewrite in between stores the new mtime with the old content. A file that vanishes between the read and the stat also fails the whole batch.
Expected: the directory mtime is read before the scan, and each file is stat-ed before it is read.
Done when: a unit test edits an item between the scan and the end of check_staleness and the report's observed directory mtime is older than the directory's mtime after the edit.