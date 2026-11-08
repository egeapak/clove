---
schema: 1
id: clov-M24P2MP4
title: 'web timeline: cursor date chip slides under the label column at the left edge when the plot is scrolled'
status: open
type: bug
priority: 2
created: 2026-11-08T12:36:31.807Z
updated: 2026-11-08T12:36:31.807Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/timeline/+page.svelte: the chip (data-testid timeline-cursor-chip) is clamped to the .tl-axis element, which spans the whole scroll width. When .tl-scroll is scrolled sideways (the default on a 1400 px window, which opens scrolled to today), a pointer just right of the sticky label column puts the chip about 34 px under that column, so the first characters of the date are hidden (for example 'Sep 3' shows as 'p 3'). At scrollLeft 0 the chip clamps correctly. Both the dark (midnight-ide) and light (linear-light) themes show it.

Expected: the chip stays inside the visible plot area at both edges for any scroll position.
Done when: a vitest or Playwright check moves the pointer to the left edge of the visible plot with the timeline scrolled, and the chip's left edge is at or right of the label column's right edge.