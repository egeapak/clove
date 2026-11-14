---
schema: 1
id: clov-MBQFHES6
title: 'tui: a save rescans the store on the UI thread and tab badges recount each frame'
status: open
type: chore
priority: 4
created: 2026-11-14T19:14:59.746Z
updated: 2026-11-14T19:14:59.746Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-tui/src/app/mod.rs form_submit calls refresh(), a synchronous scan_frontmatter + graph build, so the UI does not redraw for the scan time after each save (about 0.2 s at 20k items, 0.9 s at 80k). start_refresh already runs the same scan on a background thread for the r key. crates/clove-tui/src/ui/tabs.rs calls visible_for for each tab on every frame, which walks all items.
Expected: a save refreshes through the background path (the form closes at once and the list updates when the scan lands), and the tab counts are computed once per refresh.
Done when: form_submit does not call the synchronous scan, the form tests still see the saved item after polling the refresh, and the tab counts are stored on refresh.