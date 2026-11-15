---
schema: 1
id: clov-R2N9DG12
title: 'web: board keyboard move loses focus in a real browser'
status: closed
type: bug
priority: 2
created: 2026-11-15T07:10:12.492Z
updated: 2026-11-15T07:10:12.555Z
closed: 2026-11-15T07:10:12.555Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/board/+page.svelte move() re-focuses the moved card's move button in its new column. The .kbd-move buttons are display: none until the card has :focus-within or :hover, and a browser does not focus a hidden element, so focus fell to <body> after every keyboard move. The jsdom test passed because jsdom has no layout.
Expected: after Enter on a move button, focus is on the same card's button in the new column, so the user can press Enter again.
Done when: the test 'shows the moved card's buttons before it focuses one' in src/test/board-and-modal.svelte.test.ts passes and a Chromium check of three keyboard moves keeps focus on the card.