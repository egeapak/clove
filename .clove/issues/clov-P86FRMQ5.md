---
schema: 1
id: clov-P86FRMQ5
title: CLI has no way to change an existing item's parent
status: closed
type: feature
priority: 3
created: 2026-10-06T18:18:21.000Z
updated: 2026-11-03T17:38:52.953Z
closed: 2026-10-21T07:24:46.000Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-72
---
Parent can be set only at creation (`clove new --parent`). `clove edit --field parent=…` is rejected (graph edges are dedicated, cycle-validated ops), but unlike deps (`clove dep add/rm`) there is no CLI op for it — MCP has `clove_set_parent` and the web sidebar sets it. Add e.g. `clove parent <id> <parent-id|--clear>` over `ops::set_parent`. Found dogfooding v0.1.1 (2026-10-06).