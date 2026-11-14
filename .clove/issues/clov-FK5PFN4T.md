---
schema: 1
id: clov-FK5PFN4T
title: 'cloved: a query is answered from the index although its refresh was skipped for more than 20 changed items'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:42.893Z
updated: 2026-11-14T19:33:51.132Z
closed: 2026-11-14T19:33:51.132Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/cloved/src/ipc.rs refresh applies a staleness report only when change_count <= STALE_REFRESH_LIMIT, and handle_query/handle_graph then answer from the index anyway, as source daemon, with rows that are known to be behind the files.
Done when: such a read is refused with INDEX_BEHIND so the client reads the next tier, the watcher task applies the catch-up, and crates/cloved fresh::tests::a_change_over_the_limit_is_refused_and_kept_for_the_watcher passes.