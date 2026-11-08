---
schema: 1
id: clov-6ZK5R0W3
title: 'web timeline: throughput ''All'' range shows the same 90 days as ''90d'''
status: closed
type: bug
priority: 2
created: 2026-11-08T12:36:31.823Z
updated: 2026-11-08T22:07:11.217Z
closed: 2026-11-08T22:07:11.217Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
The timeline throughput chart (routes/timeline/+page.svelte) gets its series from GET /stats/history with no parameters. When the series is synthesized from item timestamps (crates/clove-web/src/read.rs, `?days=` defaults to 90, clamped to 1..=365), the response holds only the last 90 days. So the 'All' button shows 90 bars, exactly like '90d', although this repo has items created from 2026-07-13.

Expected: 'All' covers the whole history (at least from the oldest item's created date, within the server's clamp), or the button is renamed to what it shows.
Done when: on a store whose oldest item is older than 90 days, 'All' renders more daily bars than '90d'.