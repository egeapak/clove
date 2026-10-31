---
schema: 1
id: clov-SAFV3WWD
title: Align MCP server instructions with the agent directives
status: closed
type: docs
priority: 2
created: 2026-10-31T06:46:18.316Z
updated: 2026-10-31T06:49:57.201Z
closed: 2026-10-31T06:49:57.201Z
labels: []
deps: [clov-YCZZ7GS6]
relates: []
duplicates: []
supersedes: []
---
The MCP server instructions in crates/clove-mcp/src/server.rs (get_info, with_instructions) are what an agent sees when it uses clove through MCP without a CLOVE.md. They must carry the same item-granularity and self-contained-body rules as CLOVE.md, and the clove_new tool description must say that the body must stand alone.
Done when: the instructions state the rules, and the clove-mcp tests pass.