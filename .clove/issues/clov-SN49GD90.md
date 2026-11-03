---
schema: 1
id: clov-SN49GD90
title: 'web: List/Timeline row click 404s under the daemon''s /p/<slug>/ mount'
status: closed
type: bug
priority: 1
created: 2026-10-06T18:18:20.000Z
updated: 2026-11-03T17:38:52.954Z
closed: 2026-10-17T23:01:16.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-71
---
In the daemon-served web UI (`/p/<slug>/`), clicking a List row (or pressing Enter on it) or a Timeline bar navigates to `/p/items/<id>` → 404. `routes/list/+page.svelte` (L164, L334) and `routes/timeline/+page.svelte` (L193) call `goto('../items/<id>')`, which is relative to the current URL: from `/p/<slug>/list` it climbs out of the slug. Standalone `clove serve` mounts at `/`, where `..` clamps at the root, so it only breaks under the 0.1.1 hub. Board cards use `./items/<id>` links and work.

Fix: `goto(`${base}/items/${id}`)` like the layout/NewItemModal already do. Found dogfooding the v0.1.1 release (2026-10-06).