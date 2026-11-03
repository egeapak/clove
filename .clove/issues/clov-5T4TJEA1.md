---
schema: 1
id: clov-5T4TJEA1
title: 'web: a detail page opened by direct URL/refresh misses live updates'
status: closed
type: bug
priority: 3
created: 2026-10-06T18:18:20.000Z
updated: 2026-11-03T17:38:52.956Z
closed: 2026-10-17T23:01:15.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-70
---
Opening `/p/<slug>/items/<id>` directly (deep link or reload) and then changing the item elsewhere (CLI) does not update the page — header priority stayed stale for 5s+, reproducible. Reaching the same page from the Board (store pre-populated by the board query) updates live within ~1s, and the List view updates live too. Likely the WS batch → refetch only refreshes list/board queries, so an item upserted alone by the detail page's `api.item()` is never refreshed. Not yet checked against standalone `clove serve` (may predate 0.1.1). Found dogfooding v0.1.1 (2026-10-06).