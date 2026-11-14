---
schema: 1
id: clov-097GJD6E
title: Web detail page inline edits do nothing when the item is outside the store window
status: closed
type: bug
priority: 2
created: 2026-11-14T19:13:01.246Z
updated: 2026-11-14T19:13:01.668Z
closed: 2026-11-14T19:13:01.668Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/store.svelte.ts optimistic() returns a no-op handle when the id is not in the cache, and replaceAll() drops every item outside the current window on each live refetch. On the detail page of an item that the list query does not hold, a status, priority, assignee or label change then shows no optimistic patch, discards the server answer on settle, and leaves a rejected value in the select because there is nothing to roll back.
Done when: the 'inline edits on an item outside the store window' tests in src/test/item-detail-states.svelte.test.ts pass.