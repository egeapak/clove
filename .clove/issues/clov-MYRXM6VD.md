---
schema: 1
id: clov-MYRXM6VD
title: Web handlers do blocking store work on async workers
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:18.528Z
updated: 2026-11-14T19:30:06.494Z
closed: 2026-11-14T19:30:06.494Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/src/read.rs values_of (per-row file reads) runs after the blocking await in list_items and get_board; get_meta, get_cycles and get_stats_history scan the whole store on an axum worker; every handler in crates/clove-web/src/write.rs does file I/O (dep_add scans the store) on an axum worker. cloved hosts this server on a two-worker runtime, so two such requests stall its RPC serving.
Expected: every store call in read.rs and write.rs runs on spawn_blocking.
Done when: the handlers do no store I/O outside blocking() and cargo nextest run -p clove-web --all-features passes.