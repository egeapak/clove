---
schema: 1
id: clov-M8JTB85Z
title: Web list rows have no link, and the j/k cursor leaves the screen
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:46.521Z
updated: 2026-11-14T19:08:46.810Z
closed: 2026-11-14T19:08:46.810Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/list/+page.svelte renders each row as <tr onclick> with only spans inside. There is no focusable element or link per item, so Tab and screen reader users cannot open an item and Ctrl/middle-click cannot open a new tab. The j/k keyboard cursor moves to a row outside the virtual window without scrolling, so Enter opens an item that is not visible.
Expected: each row has an <a href> to its item, and a cursor move scrolls the row into view.
Done when: the 'list rows' tests in src/test/list-page.svelte.test.ts pass.