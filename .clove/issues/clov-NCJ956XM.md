---
schema: 1
id: clov-NCJ956XM
title: Add an agent workflow section to `clove agent-doc`
status: closed
type: docs
priority: 2
created: 2026-10-31T06:46:18.340Z
updated: 2026-10-31T06:49:57.260Z
closed: 2026-10-31T06:49:57.260Z
labels: []
deps: [clov-YCZZ7GS6]
relates: []
duplicates: []
supersedes: []
---
`clove agent-doc` (crates/clove/src/cmd/agent_doc.rs) is a command reference with no guidance on when to file an item or how to split work into items. Add a short 'Workflow for agents' section with the same rules as CLOVE.md: one item per task, steps as comments, self-contained bodies, and the start/comment/close lifecycle.
Done when: the generated document has the section and the agent_doc tests pass.