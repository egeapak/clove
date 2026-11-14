---
schema: 1
id: clov-V2XYXZQA
title: Web detail page keeps a failed comment in the thread
status: closed
type: bug
priority: 2
created: 2026-11-14T19:13:01.125Z
updated: 2026-11-14T19:13:01.620Z
closed: 2026-11-14T19:13:01.620Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/items/[id]/+page.svelte addComment() appends an optimistic comment and, when POST /comments fails, removes it with comments.filter((c) => c !== optimistic). comments is deep $state, so each element is a proxy and never equals the raw object: nothing is removed. The toast says 'Comment failed', the draft is restored, and the comment (author '…') stays listed; sending again shows it twice.
Done when: 'takes a failed comment back out of the thread and restores the draft' in src/test/item-detail-states.svelte.test.ts passes.