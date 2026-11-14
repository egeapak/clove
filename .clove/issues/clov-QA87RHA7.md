---
schema: 1
id: clov-QA87RHA7
title: Web list and board throw when the row count drops below the rendered window
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:59.664Z
updated: 2026-11-14T19:04:53.035Z
closed: 2026-11-14T19:04:53.035Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/list/+page.svelte and routes/board/+page.svelte render a virtual window and index the rows from the template (rows[row.index], col.items[row.index]). The virtualizer count is pushed from an $effect, which runs after render. When the rows shrink (a filter that matches fewer rows, the Ready or Blocked tab, a shorter last page, a card moved out of a column whose last card is rendered) the existing rows re-evaluate with a stale index, read undefined, and throw "Cannot read properties of undefined (reading 'status')". The list keeps the old rows and does not recover; the board stops moving cards until a reload.
Expected: the view renders the shorter result.
Done when: src/test/shrinking-window.svelte.test.ts passes.