---
schema: 1
id: clov-F306EHQZ
title: Web New item modal discards a typed draft on Escape or backdrop click
status: closed
type: bug
priority: 2
created: 2026-11-14T19:17:54.679Z
updated: 2026-11-14T19:18:02.726Z
closed: 2026-11-14T19:18:02.726Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/lib/components/NewItemModal.svelte closes on Escape and on a backdrop click with no check, so a typed title and body are lost.
Expected: when the form differs from its initial state, the modal asks to keep editing or discard; an untouched form closes at once.
Done when: src/test/board-and-modal.svelte.test.ts 'asks before Escape or the backdrop discards a draft' passes.