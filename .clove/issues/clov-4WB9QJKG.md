---
schema: 1
id: clov-4WB9QJKG
title: Web list rewrites ?page= against the total of the previous query
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:46.491Z
updated: 2026-11-14T19:08:46.809Z
closed: 2026-11-14T19:08:46.809Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/list/+page.svelte steps back to the last page when offset >= store.total. store.total and store.loaded belong to the query the store last loaded. On Back to /list?page=5 after a search with fewer matches, the effect runs before the new response and rewrites the URL (replaceState) to a smaller page, for example ?page=2 showing 101-200.
Expected: the clamp runs only against the response to the current query.
Done when: 'does not clamp the page against the total of the previous query' in src/test/list-page.svelte.test.ts passes.