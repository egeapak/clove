---
schema: 1
id: clov-EXXM73ZZ
title: 'cloved: a failed index apply in a watcher batch is dropped with no retry'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:43.023Z
updated: 2026-11-14T19:33:51.195Z
closed: 2026-11-14T19:33:51.195Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/reindexer.rs and crates/cloved/src/ipc.rs ignore the result of apply_staleness (let _ =). One SQLITE_BUSY or a file that vanished between check and apply rolls back the batch, and nothing retries until another file event arrives.
Done when: a failed sync keeps its pending changes and the watcher task retries it after a delay.