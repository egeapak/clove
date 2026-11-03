---
schema: 1
id: clov-J20CF592
title: 'tui: short ids drop leading zeros, so #BRK6DND doesn''t match clov-0BRK6DND'
status: closed
type: bug
priority: 4
created: 2026-10-06T18:18:19.000Z
updated: 2026-11-03T17:38:52.957Z
closed: 2026-10-17T23:01:11.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-69
---
`crates/clove-tui/src/ui/util.rs::short_id` trims leading zeros (a holdover from sequential ids: `proj-00000042` → `42`). With random Crockford ids, `clov-0BRK6DND` renders as `#BRK6DND` in the TUI list/detail while the web and CLI show `#0BRK6DND`, and the row's id column loses alignment. Fix: only strip the prefix. Found dogfooding v0.1.1 (2026-10-06).