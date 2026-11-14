---
schema: 1
id: clov-D6P12ZHS
title: Web timeline renders every item row and every dependency arrow
status: closed
type: bug
priority: 2
created: 2026-11-14T19:19:37.959Z
updated: 2026-11-14T19:19:38.055Z
closed: 2026-11-14T19:19:38.055Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/timeline/+page.svelte renders one label row, one track row and one SVG path per dependency for every item in the store (limit 0). With a few thousand items the page builds tens of thousands of nodes, and each hover re-evaluates the highlight class on all of them.
Expected: only the rows near the viewport, and the arrows that cross them, are in the DOM; the full height stays reserved.
Done when: src/test/timeline-window.svelte.test.ts passes.