---
schema: 1
id: clov-CMXQP7VM
title: MCP server-push notifications (tools/list_changed + ready-queue subscribe)
status: closed
type: feature
priority: 2
created: 2026-07-18T00:14:19.000Z
updated: 2026-11-03T17:38:52.989Z
closed: 2026-07-18T11:59:19.000Z
labels: [mcp]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-21
---
Now that the daemon keeps an always-correct hot graph, push tools/list_changed / a ready-queue SUBSCRIBE to MCP clients when the graph changes.