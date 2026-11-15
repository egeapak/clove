---
schema: 1
id: clov-CCGYPCQQ
title: Web list search debounce timer survives leaving the page
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:46.563Z
updated: 2026-11-14T19:08:46.811Z
closed: 2026-11-14T19:08:46.811Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/list/+page.svelte starts a 220 ms timer in applySearch() and never clears it on destroy. Leaving the list within 220 ms of a keystroke lets the timer call goto() and rewrite the query string of the next route.
Done when: 'drops a pending debounced URL write when the page is destroyed' in src/test/list-page.svelte.test.ts passes.