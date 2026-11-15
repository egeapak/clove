---
schema: 1
id: clov-JW5GX3SD
title: Web detail and edit pages show Loading forever for an item that does not exist
status: closed
type: bug
priority: 2
created: 2026-11-14T19:13:01.207Z
updated: 2026-11-14T19:13:01.652Z
closed: 2026-11-14T19:13:01.652Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/items/[id]/+page.svelte and routes/items/[id]/edit/+page.svelte render 'Loading #ID…' while there is no item, and a 404 from GET /items/:id only raises a toast. A stale bookmark, a mistyped id, or the link of a missing reference leaves a permanent Loading with no not-found state and no way back. When an open item is deleted elsewhere, the live reload fails silently and the stale item stays editable.
Expected: a not-found panel with a link to the list; a Retry for other load failures.
Done when: the 'an item that does not exist' tests in src/test/item-detail-states.svelte.test.ts pass.