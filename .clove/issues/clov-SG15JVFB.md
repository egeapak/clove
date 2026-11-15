---
schema: 1
id: clov-SG15JVFB
title: reindex unlinks the live index.db before renaming the rebuilt file in
status: closed
type: bug
priority: 2
created: 2026-11-14T19:16:35.904Z
updated: 2026-11-14T19:22:30.127Z
closed: 2026-11-14T19:22:30.127Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-index/src/reindex.rs builds index.db.tmp, removes index.db with its -wal and -shm files, and then renames the tmp file into place. Between the remove and the rename the path does not exist. A reader that opens it there (clove-engine lean_via_index checks exists() and then calls Index::open) creates an empty database and writes the schema into a new -wal file. After the rename that -wal sits next to the rebuilt file, and the next opener replays it over the rebuilt database: the result is a valid, empty index at the current schema version, and the stats history that reindex carries over is gone. A crash between the two steps also loses the history. Index::open_or_create has the same remove-then-open sequence for a schema mismatch. A process that keeps a connection open across the rebuild (cloved) stays on the unlinked file, and on Windows the remove fails while any process has the file open.
The rebuild also keeps every parsed item body in memory until the last row is written (202 MB peak on a 192 MB store), although the index stores no bodies. stats_store::preserve_from ignores every error, so a locked database during a rebuild silently drops the snapshot history.
Expected: reindex rebuilds the cache tables inside the live database in one transaction and never unlinks a usable index.db; the snapshots table is not copied at all; bodies are dropped after hashing. Only an unreadable (corrupt) file is replaced.
Done when: tests show that index.db keeps its inode across reindex, a connection opened before the rebuild reads the rebuilt rows, snapshot rows survive a reindex and a schema-version rebuild, and parsed items carry no body.