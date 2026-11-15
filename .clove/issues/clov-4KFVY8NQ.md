---
schema: 1
id: clov-4KFVY8NQ
title: Web list filter box deletes a trailing space while typing
status: closed
type: bug
priority: 2
created: 2026-11-14T19:08:46.435Z
updated: 2026-11-14T19:08:46.804Z
closed: 2026-11-14T19:08:46.804Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/list/+page.svelte writes the trimmed filter text to the URL after a 220 ms debounce, and an $effect then copies the URL value back over the input. Typing 'Item 2 ', pausing, then typing 'alpha' gives 'Item 2alpha' in the box and ?q=Item+2alpha in the URL.
Expected: the box keeps what was typed; it follows the URL only when the URL says something different from the trimmed text.
Done when: 'keeps a trailing space when the URL catches up with the typed text' in src/test/list-page.svelte.test.ts passes.