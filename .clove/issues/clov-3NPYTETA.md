---
schema: 1
id: clov-3NPYTETA
title: 'cloved: list queries run one at a time behind the index mutex and scan the directory on each call'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:42.855Z
updated: 2026-11-14T19:33:51.022Z
closed: 2026-11-14T19:33:51.022Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/ipc.rs handle_query holds Mutex<Index> across the refresh, the SQL query and a COUNT(*) for the status count, and the refresh reads the whole issues directory (count_item_files). Concurrent ready/ls calls from several clients are fully serialized.
Done when: handle_query holds the index mutex for the refresh only and runs the SQL on a pooled read connection, a quiet refresh is one stat, and the cloved suite passes.