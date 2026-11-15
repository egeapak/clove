---
schema: 1
id: clov-SAMT6P37
title: Web detail page state leaks from one item to the next
status: closed
type: bug
priority: 2
created: 2026-11-14T19:13:01.332Z
updated: 2026-11-14T19:13:01.720Z
closed: 2026-11-14T19:13:01.720Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/items/[id]/+page.svelte is reused when the id changes (a relationship link), and the id effect does not reset confirmingDelete, newComment, newDep, addingLabel, comments or deptree. With the delete confirmation open on item A, following a link shows 'Permanently delete B?' already armed. A comment draft typed on A is posted to B. A's comments and dep tree show under B until B's load lands.
Done when: 'does not carry the delete confirmation, drafts or comments over' in src/test/item-detail-states.svelte.test.ts passes.