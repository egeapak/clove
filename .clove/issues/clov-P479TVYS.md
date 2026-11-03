---
schema: 1
id: clov-P479TVYS
title: 'web timeline: throughput chart treats hourly stats snapshots as days (duplicate keys, wrong range)'
status: closed
type: bug
priority: 2
created: 2026-10-06T18:18:23.000Z
updated: 2026-11-03T17:38:52.949Z
closed: 2026-10-17T23:01:19.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-75
---
`GET /api/v1/stats/history` returns one point per stats snapshot — hourly by default (`stats_snapshot_min` = 60), so several points share a `date`. `routes/timeline/+page.svelte` keys the throughput chart `{#each tput.pts as p (p.date)}` → Svelte `each_key_duplicate` error in the console on every Timeline load, and slices `history.slice(-days)` treating points as days, so '30d'/'90d' really show the last 30/90 snapshots (~hours). Fix: aggregate to one point per date (server or client) before charting; key by date then. Real store: 112 snapshots, many per day. Found dogfooding v0.1.1 (2026-10-06); likely present since the stats-history endpoint (0.1.0).