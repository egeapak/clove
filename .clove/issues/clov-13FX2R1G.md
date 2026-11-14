---
schema: 1
id: clov-13FX2R1G
title: MCP tools silently ignore unknown arguments
status: closed
type: bug
priority: 2
created: 2026-11-14T19:04:18.447Z
updated: 2026-11-14T19:29:50.542Z
closed: 2026-11-14T19:29:50.542Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-types/src/tool.rs argument structs use #[serde(flatten)], so serde cannot deny unknown fields. clove_list {"labels":["x"]} (the key is label) returns the whole store, clove_comments {"offset":1} returns the newest page again, clove_search {"status":"closed"} ignores the filter, and clove_edit {"labels":[...]} reports 'no fields to edit'. The clove_comments description in crates/clove-mcp/src/server.rs names offset; the argument is skip_newest.
Expected: an argument that is not in the tool's inputSchema is a tool error that lists the valid names; the clove_comments description names skip_newest.
Done when: crates/clove/tests/mcp.rs::unknown_tool_arguments_are_refused passes.