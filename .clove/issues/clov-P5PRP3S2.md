---
schema: 1
id: clov-P5PRP3S2
title: 'Agent directives: name items as id plus title when talking to the user'
status: closed
type: docs
priority: 2
created: 2026-10-31T10:01:43.113Z
updated: 2026-10-31T10:04:50.640Z
closed: 2026-10-31T10:04:50.640Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
Agents often refer to clove items by id alone in replies and questions (for example 'I closed clov-H2D91NF2'). A bare id means nothing to the user without a lookup. The agent-facing text must require the form `<id>` (<title>) whenever an agent mentions an item to the user, for example: I closed `clov-H2D91NF2` (Fix the parser). The rule covers replies, questions, and summaries to the user, not item bodies or comments.
Where: CLOVE.md and CLOVE_MD_CONTENT (crates/clove/src/cmd/setup.rs), the MCP server instructions (crates/clove-mcp/src/server.rs), and the workflow section of clove agent-doc.
Done when: all three texts carry the rule, CLOVE.md and the const are byte-identical, and the setup, agent_doc, and clove-mcp tests pass.