---
schema: 1
id: clov-EXMARRXC
title: 'Windows: reindex fails while a process has index.db open'
status: open
type: bug
priority: 2
created: 2026-11-14T19:17:42.924Z
updated: 2026-11-14T19:17:42.924Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
clove_index::reindex (crates/clove-index/src/reindex.rs) removes index.db and renames the rebuilt file over it. Bundled SQLite opens the database without FILE_SHARE_DELETE, so on Windows both steps fail with a sharing violation while any connection is open. crates/cloved/src/ipc.rs handle_reindex kept the daemon's own connection open across the rebuild; a CLI process or a second reader that holds the file has the same effect, and crates/clove/src/cmd/reindex.rs falls back to a local rebuild that fails the same way.
Done when: clove reindex succeeds on Windows with a daemon serving the project and with another clove process reading the index, covered by a test or a step in scripts/ci/daemon-smoke.sh that runs on the Windows runner.