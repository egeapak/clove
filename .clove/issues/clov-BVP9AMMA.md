---
schema: 1
id: clov-BVP9AMMA
title: 'web timeline: ''today'' marker label is cut off at the plot''s right edge'
status: closed
type: bug
priority: 4
created: 2026-11-04T09:49:36.816Z
updated: 2026-11-04T16:52:19.085Z
closed: 2026-11-04T16:52:19.085Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-web/web/src/routes/timeline/+page.svelte renders the 'today' label (.today span) starting at pct(domain.now). When now is at or near the right end of the domain (for example an empty timeline, or a project whose latest activity is now and the scroll is at the right end), the label runs past the plot border and shows as 'toda'. Expected: the label stays inside the plot, aligned inward near the right edge like the edge axis ticks (.tick-end).
Done when: a vitest case in src/test/timeline-cursor.svelte.test.ts checks the inward alignment, and the label renders in full in the browser.