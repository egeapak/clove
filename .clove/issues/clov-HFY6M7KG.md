---
schema: 1
id: clov-HFY6M7KG
title: Web board keyboard move drops focus from the moved card
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:54.643Z
updated: 2026-11-14T19:18:02.700Z
closed: 2026-11-14T19:18:02.700Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/board/+page.svelte: the move-left and move-right buttons re-render the card in another column, and focus falls to the document body. A keyboard user must tab back to the card after every move.
Expected: focus stays on the same move button of the moved card (or the opposite one at the last column).
Done when: src/test/board-and-modal.svelte.test.ts 'keeps focus on the moved card' passes.