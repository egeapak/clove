---
schema: 1
id: clov-KM9HZ74A
title: 'tui: body markdown is re-rendered every frame and a save rescans on the UI thread'
status: closed
type: chore
priority: 3
created: 2026-11-14T19:03:04.220Z
updated: 2026-11-14T19:14:59.838Z
closed: 2026-11-14T19:14:59.838Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-tui/src/ui/detail/overview.rs renders the item body from Markdown on every draw (each key press and tick) with no cache. app/mod.rs refresh() after a form submit is a synchronous full store scan on the UI thread. ui/tabs.rs recounts ready/blocked over all items on each frame.
Expected: rendered body lines are cached per (item, body) and reused across frames.
Done when: a test shows two draws of the same item render the Markdown once.