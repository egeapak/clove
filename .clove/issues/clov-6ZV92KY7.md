---
schema: 1
id: clov-6ZV92KY7
title: Web list shows the error of a superseded request
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:54.586Z
updated: 2026-11-14T19:18:02.666Z
closed: 2026-11-14T19:18:02.666Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/store.svelte.ts refetch() awaits api.items() and api.meta() with no check of loadToken on failure. When a newer request is already in flight and the older one fails, the older failure sets loadError and the list shows an error for a query that is no longer on screen.
Expected: a failure of a superseded request is ignored.
Done when: src/lib/store.test.ts 'ignores the failure of a superseded request' passes.